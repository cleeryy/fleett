//! Custom instance templates (issue #43).
//!
//! The built-in templates stay compiled constants ([`crate::templates_builtin`])
//! and are read-only. A custom template lives on disk under
//! `$FLEETT_ROOT/templates/<id>/`:
//!
//! * `template.yaml` — metadata: `name`, `description`, `parent`,
//!   `default_model`, `channel`, the `env` schema and the `source_instance`;
//! * `config.yaml` — the Hermes config used by the renderer (optional when the
//!   template extends a parent: the parent config is then inherited);
//! * `soul.md` — the starting `SOUL.md` (optional, inherited from the parent
//!   when absent; the historical `SOUL.md` spelling is still read);
//! * `defaults.yaml` — instance-creation defaults (`mem_limit_mb`, `cpu_limit`,
//!   `allow_lan`, `model`, `base_url`, `notes`).
//!
//! The filesystem is the source of truth. The `templates` and
//! `template_versions` tables are an index rebuilt at startup: the first one
//! lists custom and built-in templates, the second keeps a git-like history of
//! every create/update/delete (metadata plus file contents and SHA-256
//! digests). Nothing here shells out to git.
//!
//! Inheritance is live: a child that does not write `config.yaml` or `soul.md`
//! keeps reading its parent's file, field by field. Environment entries are
//! merged by key and defaults field by field, the child winning every
//! conflict. Cycles are refused before anything is written.

use crate::config::Config;
use crate::db::Db;
use crate::models::{
    now_iso, validate_base_url, validate_cpu_limit, validate_env_key, validate_instance_id,
    validate_mem_limit, validate_model, validate_template_id, ChannelKind, EnvSuggestion,
    TemplateDefaults, TemplateDetail, TemplateInfo, TemplateMeta,
};
use crate::templates_builtin;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Longest accepted inheritance chain.
pub const MAX_INHERITANCE_DEPTH: usize = 32;
/// Longest template description.
pub const MAX_DESCRIPTION_CHARS: usize = 4096;
/// Longest environment description or example.
pub const MAX_ENV_TEXT_CHARS: usize = 1024;
/// Longest free-form `format` hint of an environment entry.
pub const MAX_ENV_FORMAT_CHARS: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("unknown template \"{0}\"")]
    NotFound(String),
    #[error("unknown instance \"{0}\"")]
    InstanceNotFound(String),
    #[error("template \"{0}\" is built in and read-only")]
    ReadOnly(String),
    #[error("template \"{0}\" already exists")]
    Exists(String),
    #[error("invalid template: {0}")]
    Invalid(String),
    #[error("template \"{id}\" is still in use")]
    InUse {
        id: String,
        instances: Vec<String>,
        templates: Vec<String>,
    },
    #[error("template cycle detected: {0}")]
    Cycle(String),
    #[error("template \"{id}\" has no version {version}")]
    VersionNotFound { id: String, version: i64 },
    #[error("template filesystem error: {0}")]
    Io(String),
    #[error("template database error: {0}")]
    Db(String),
}

impl From<crate::db::DbError> for TemplateError {
    fn from(err: crate::db::DbError) -> Self {
        Self::Db(err.to_string())
    }
}

/// A template resolved through its inheritance chain.
#[derive(Debug, Clone)]
pub struct ResolvedTemplate {
    pub id: String,
    /// Parent declared by this template (the chain itself is not exposed).
    pub parent: Option<String>,
    pub read_only: bool,
    /// `required` keys declared by custom layers of the chain. Built-in
    /// templates are read-only: their allowlists stay warnings (#27) and never
    /// make an instance impossible to create.
    pub enforced_required: Vec<String>,
    pub meta: TemplateMeta,
    pub defaults: TemplateDefaults,
    pub config_yaml: String,
    pub soul_md: Option<String>,
}

impl ResolvedTemplate {
    /// Environment schema resolved through the chain.
    pub fn env_schema(&self) -> Vec<EnvSuggestion> {
        self.meta.env.clone().unwrap_or_default()
    }
}

/// One layer read from disk or from the built-ins.
#[derive(Debug, Clone)]
struct Layer {
    id: String,
    meta: TemplateMeta,
    config_yaml: Option<String>,
    soul_md: Option<String>,
    defaults: Option<TemplateDefaults>,
    read_only: bool,
}

/// Metadata indexed in the `templates` table.
#[derive(Debug, Clone, Default)]
struct IndexRow {
    read_only: bool,
    parent: Option<String>,
    metadata: TemplateMeta,
    source_instance: Option<String>,
    created_at: String,
    updated_at: String,
}

/// History entry returned by `GET /api/templates/{id}/versions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateVersion {
    pub version: i64,
    pub action: String,
    pub actor: String,
    pub at: String,
    /// SHA-256 of every file the snapshot holds (`config.yaml`, `soul.md`,
    /// `defaults.yaml`).
    pub sha256: BTreeMap<String, String>,
}

/// Stored snapshot of one version (JSON in `template_versions.snapshot`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateSnapshot {
    pub metadata: TemplateMeta,
    #[serde(default)]
    pub config_yaml: Option<String>,
    #[serde(default)]
    pub soul_md: Option<String>,
    #[serde(default)]
    pub defaults: TemplateDefaults,
}

#[derive(Debug, Clone)]
struct VersionRow {
    version: i64,
    action: String,
    actor: String,
    at: String,
    snapshot: String,
}

/// `POST /api/templates`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateTemplateRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub env: Option<Vec<EnvSuggestion>>,
    #[serde(default)]
    pub config_yaml: Option<String>,
    #[serde(default)]
    pub soul_md: Option<String>,
    #[serde(default)]
    pub defaults: Option<TemplateDefaults>,
}

/// `PUT /api/templates/{id}`.
///
/// A missing field is left untouched. `null` clears it (`parent`, `channel`,
/// `default_model`, `config_yaml`, `soul_md`, `defaults`); an explicit object
/// or string replaces it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateTemplateRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub parent: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub default_model: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub channel: Option<Option<String>>,
    #[serde(default)]
    pub env: Option<Vec<EnvSuggestion>>,
    #[serde(default, deserialize_with = "double_option")]
    pub config_yaml: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub soul_md: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub defaults: Option<Option<TemplateDefaults>>,
}

/// `POST /api/templates/from-instance`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FromInstanceRequest {
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// `POST /api/templates/{id}/restore`.
#[derive(Debug, Clone, Deserialize)]
pub struct RestoreTemplateRequest {
    pub version: i64,
}

fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ---------------------------------------------------------------------------
// Resolution (disk + built-ins, no database)
// ---------------------------------------------------------------------------

/// Resolve one template, following its `parent` chain.
pub fn resolve(cfg: &Config, id: &str) -> Result<ResolvedTemplate, TemplateError> {
    let id = id.trim();
    validate_template_id(id).map_err(|_| TemplateError::NotFound(id.to_string()))?;
    let mut chain = Vec::new();
    resolve_layer(cfg, id, &mut chain)
}

fn resolve_layer(
    cfg: &Config,
    id: &str,
    chain: &mut Vec<String>,
) -> Result<ResolvedTemplate, TemplateError> {
    if chain.iter().any(|entry| entry == id) {
        let mut path = chain.clone();
        path.push(id.to_string());
        return Err(TemplateError::Cycle(path.join(" -> ")));
    }
    if chain.len() >= MAX_INHERITANCE_DEPTH {
        let mut path = chain.clone();
        path.push(id.to_string());
        return Err(TemplateError::Cycle(format!(
            "inheritance deeper than {MAX_INHERITANCE_DEPTH}: {}",
            path.join(" -> ")
        )));
    }

    let layer = load_layer(cfg, id)?;
    let parent_id = clean(layer.meta.parent.as_deref());
    chain.push(id.to_string());
    let parent = match &parent_id {
        Some(parent_id) => Some(resolve_layer(cfg, parent_id, chain)?),
        None => None,
    };
    chain.pop();

    let parent_meta = parent
        .as_ref()
        .map(|resolved| resolved.meta.clone())
        .unwrap_or_default();
    let parent_defaults = parent
        .as_ref()
        .map(|resolved| resolved.defaults.clone())
        .unwrap_or_default();
    let parent_config = parent
        .as_ref()
        .map(|resolved| resolved.config_yaml.clone())
        .unwrap_or_default();
    let parent_soul = parent
        .as_ref()
        .and_then(|resolved| resolved.soul_md.clone());

    let config_yaml = match layer
        .config_yaml
        .clone()
        .filter(|text| !text.trim().is_empty())
    {
        Some(text) => text,
        None => parent_config,
    };
    if config_yaml.trim().is_empty() {
        return Err(TemplateError::Invalid(format!(
            "template \"{id}\" has no config.yaml and no parent to inherit one from"
        )));
    }

    let own_env = layer.meta.env.clone().unwrap_or_default();
    // A child that writes its own config may change the platform without
    // repeating `channel` in `template.yaml`: detect it from that config.
    let own_channel = layer.meta.channel.clone().or_else(|| {
        layer
            .config_yaml
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .and_then(channel_of)
    });
    let meta = TemplateMeta {
        name: layer.meta.name.clone().or(parent_meta.name),
        description: layer.meta.description.clone().or(parent_meta.description),
        // The declared parent is the one of this layer, never the parent's.
        parent: layer.meta.parent.clone(),
        default_model: layer
            .meta
            .default_model
            .clone()
            .or(parent_meta.default_model),
        channel: own_channel.or(parent_meta.channel),
        env: Some(merge_env(
            parent_meta.env.as_deref().unwrap_or_default(),
            &own_env,
        )),
        source_instance: layer
            .meta
            .source_instance
            .clone()
            .or(parent_meta.source_instance),
    };
    let defaults = layer
        .defaults
        .clone()
        .unwrap_or_default()
        .over(&parent_defaults);

    let mut enforced_required = parent
        .as_ref()
        .map(|resolved| resolved.enforced_required.clone())
        .unwrap_or_default();
    if !layer.read_only {
        for entry in &own_env {
            if entry.required && !enforced_required.contains(&entry.key) {
                enforced_required.push(entry.key.clone());
            }
        }
    }
    let mut resolved = ResolvedTemplate {
        id: layer.id.clone(),
        parent: clean(meta.parent.as_deref()),
        read_only: layer.read_only,
        enforced_required,
        meta,
        defaults,
        config_yaml,
        soul_md: layer.soul_md.clone().or(parent_soul),
    };

    if resolved.meta.default_model.is_none() {
        resolved.meta.default_model =
            model_of(&resolved.config_yaml).or_else(|| resolved.defaults.model.clone());
    }
    if resolved.meta.channel.is_none() {
        resolved.meta.channel = channel_of(&resolved.config_yaml);
    }
    if resolved.meta.name.is_none() {
        resolved.meta.name = Some(resolved.id.clone());
    }
    Ok(resolved)
}

fn merge_env(parent: &[EnvSuggestion], child: &[EnvSuggestion]) -> Vec<EnvSuggestion> {
    let mut merged = parent.to_vec();
    for entry in child {
        match merged.iter_mut().find(|existing| existing.key == entry.key) {
            Some(slot) => *slot = entry.clone(),
            None => merged.push(entry.clone()),
        }
    }
    merged
}

fn channel_of(config_yaml: &str) -> Option<String> {
    let doc: Value = serde_yaml::from_str(config_yaml).ok()?;
    Some(
        crate::render::channel_from_config(&doc)
            .as_str()
            .to_string(),
    )
}

fn model_of(config_yaml: &str) -> Option<String> {
    let doc: Value = serde_yaml::from_str(config_yaml).ok()?;
    let model = doc.get("model")?.get("default")?.as_str()?.trim();
    (!model.is_empty()).then(|| model.to_string())
}

/// Map a compact built-in declaration onto the API environment suggestion.
fn builtin_suggestion(entry: &templates_builtin::BuiltinEnv) -> EnvSuggestion {
    EnvSuggestion {
        key: entry.key.to_string(),
        description: entry.description.to_string(),
        example: entry.example.map(str::to_string),
        required: entry.required,
        format: entry.format.map(str::to_string),
        secret: false,
    }
}

fn load_layer(cfg: &Config, id: &str) -> Result<Layer, TemplateError> {
    // Built-ins are compiled constants: a hand-written directory with the same
    // name never shadows them.
    if let Some(builtin) = templates_builtin::find(id) {
        return Ok(Layer {
            id: builtin.id.to_string(),
            meta: TemplateMeta {
                name: Some(builtin.name.to_string()),
                description: Some(builtin.description.to_string()),
                parent: None,
                default_model: Some(builtin.default_model.to_string()),
                channel: Some(builtin.channel.to_string()),
                env: Some(builtin.env.iter().map(builtin_suggestion).collect()),
                source_instance: None,
            },
            config_yaml: Some(builtin.config_yaml.to_string()),
            soul_md: Some(builtin.soul_md.to_string()),
            defaults: None,
            read_only: true,
        });
    }

    let dir = cfg.template_dir(id);
    if dir.is_dir() {
        let meta_path = dir.join("template.yaml");
        let config_path = dir.join("config.yaml");
        if meta_path.is_file() || config_path.is_file() {
            let meta = match read_optional(&meta_path)? {
                Some(text) => serde_yaml::from_str::<TemplateMeta>(&text).map_err(|err| {
                    TemplateError::Invalid(format!("{}: {err}", meta_path.display()))
                })?,
                None => TemplateMeta::default(),
            };
            let config_yaml = read_optional(&config_path)?;
            let soul_md = match read_optional(&dir.join("soul.md"))? {
                Some(text) => Some(text),
                None => read_optional(&dir.join("SOUL.md"))?,
            };
            let defaults = match read_optional(&dir.join("defaults.yaml"))? {
                Some(text) if !text.trim().is_empty() => Some(
                    serde_yaml::from_str::<TemplateDefaults>(&text).map_err(|err| {
                        TemplateError::Invalid(format!(
                            "{}: {err}",
                            dir.join("defaults.yaml").display()
                        ))
                    })?,
                ),
                _ => None,
            };
            return Ok(Layer {
                id: id.to_string(),
                meta,
                config_yaml,
                soul_md,
                defaults,
                read_only: false,
            });
        }
    }

    Err(TemplateError::NotFound(id.to_string()))
}

fn read_optional(path: &Path) -> Result<Option<String>, TemplateError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(TemplateError::Io(format!("{}: {err}", path.display()))),
    }
}

// ---------------------------------------------------------------------------
// Listing and details
// ---------------------------------------------------------------------------

/// Built-in templates plus every custom template directory that resolves.
pub fn list(cfg: &Config, db: &Db) -> Result<Vec<TemplateInfo>, TemplateError> {
    let mut ids: BTreeSet<String> = templates_builtin::BUILTINS
        .iter()
        .map(|builtin| builtin.id.to_string())
        .collect();
    if let Ok(entries) = fs::read_dir(cfg.templates_dir()) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if validate_template_id(&name).is_ok() {
                ids.insert(name);
            }
        }
    }

    let rows = index_rows(db)?;
    let counts = instance_counts(db)?;
    let mut templates = Vec::with_capacity(ids.len());
    for id in ids {
        let resolved = match resolve(cfg, &id) {
            Ok(resolved) => resolved,
            Err(err) => {
                // A half-written directory must not hide the rest of the list.
                tracing::warn!(template = %id, %err, "skipping an unreadable template");
                continue;
            }
        };
        let row = rows.get(&id).cloned().unwrap_or_default();
        let count = counts.get(&id).copied().unwrap_or(0);
        templates.push(info_of(&resolved, &row, count));
    }
    Ok(templates)
}

/// One template with its files and history count.
pub fn detail(cfg: &Config, db: &Db, id: &str) -> Result<TemplateDetail, TemplateError> {
    let resolved = resolve(cfg, id)?;
    let row = index_row(db, id)?.unwrap_or_default();
    let count = instance_counts(db)?.get(id).copied().unwrap_or(0);
    let versions_count = versions_count(db, id)? as usize;
    Ok(TemplateDetail {
        info: info_of(&resolved, &row, count),
        config_yaml: resolved.config_yaml,
        soul_md: resolved.soul_md,
        versions_count,
    })
}

fn info_of(resolved: &ResolvedTemplate, row: &IndexRow, instances_count: usize) -> TemplateInfo {
    TemplateInfo {
        id: resolved.id.clone(),
        name: resolved
            .meta
            .name
            .clone()
            .unwrap_or_else(|| resolved.id.clone()),
        description: resolved.meta.description.clone().unwrap_or_default(),
        default_model: resolved.meta.default_model.clone().unwrap_or_default(),
        channel: resolved
            .meta
            .channel
            .clone()
            .unwrap_or_else(|| ChannelKind::None.as_str().to_string()),
        env: resolved.env_schema(),
        parent: resolved.parent.clone().or_else(|| row.parent.clone()),
        read_only: resolved.read_only,
        defaults: resolved.defaults.clone(),
        source_instance: resolved
            .meta
            .source_instance
            .clone()
            .or_else(|| row.metadata.source_instance.clone())
            .or_else(|| row.source_instance.clone()),
        instances_count,
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

/// Create a custom template (`POST /api/templates`).
pub fn create(
    cfg: &Config,
    db: &Db,
    request: &CreateTemplateRequest,
    actor: &str,
) -> Result<TemplateDetail, TemplateError> {
    create_inner(cfg, db, request, actor, None)
}

fn create_inner(
    cfg: &Config,
    db: &Db,
    request: &CreateTemplateRequest,
    actor: &str,
    source_instance: Option<&str>,
) -> Result<TemplateDetail, TemplateError> {
    let id = clean(request.id.as_deref())
        .ok_or_else(|| TemplateError::Invalid("id is required".to_string()))?;
    validate_template_id(&id).map_err(TemplateError::Invalid)?;
    if templates_builtin::find(&id).is_some() {
        return Err(TemplateError::Exists(id));
    }
    if cfg.template_dir(&id).exists() || index_row(db, &id)?.is_some() {
        return Err(TemplateError::Exists(id));
    }

    let parent = clean(request.parent.as_deref());
    if let Some(parent) = &parent {
        check_parent(cfg, &id, parent)?;
    }

    let env = sanitize_env(request.env.clone().unwrap_or_default())?;
    let meta = TemplateMeta {
        name: Some(clean(request.name.as_deref()).unwrap_or_else(|| id.clone())),
        description: clean(request.description.as_deref()),
        parent: parent.clone(),
        default_model: clean(request.default_model.as_deref()),
        channel: normalize_channel(request.channel.as_deref())?,
        env: Some(env),
        source_instance: source_instance.map(str::to_string),
    };
    validate_meta(&meta)?;

    let defaults = request.defaults.clone().unwrap_or_default();
    validate_defaults(&defaults)?;

    let mut config_yaml = request
        .config_yaml
        .clone()
        .filter(|text| !text.trim().is_empty());
    if let Some(text) = &config_yaml {
        validate_config_yaml(text)?;
    }
    let mut soul_md = request.soul_md.clone().filter(|text| !text.is_empty());
    if parent.is_none() && config_yaml.is_none() {
        // A root template with no config could never render an instance: seed
        // it from the `solo` built-in so the result is immediately usable.
        let solo = templates_builtin::find("solo").expect("the solo built-in exists");
        config_yaml = Some(solo.config_yaml.to_string());
        if soul_md.is_none() {
            soul_md = Some(solo.soul_md.to_string());
        }
    }

    write_template_files(
        cfg,
        &id,
        &meta,
        config_yaml.as_deref(),
        soul_md.as_deref(),
        &defaults,
    )?;
    let now = now_iso();
    upsert_index(db, &id, &meta, false, &now)?;
    let layer = load_layer(cfg, &id)?;
    append_version(db, &id, "create", actor, &snapshot_of(&layer))?;
    detail(cfg, db, &id)
}

/// Update a custom template (`PUT /api/templates/{id}`).
pub fn update(
    cfg: &Config,
    db: &Db,
    id: &str,
    request: &UpdateTemplateRequest,
    actor: &str,
) -> Result<TemplateDetail, TemplateError> {
    let layer = ensure_custom(cfg, db, id)?;
    let mut meta = layer.meta.clone();

    if let Some(name) = request.name.as_deref() {
        meta.name = Some(required_text(name, "name", 128)?);
    }
    if let Some(description) = request.description.as_deref() {
        meta.description = Some(required_text(
            description,
            "description",
            MAX_DESCRIPTION_CHARS,
        )?);
    }
    if let Some(parent) = &request.parent {
        meta.parent = parent
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    if let Some(default_model) = &request.default_model {
        meta.default_model = default_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    if let Some(channel) = &request.channel {
        meta.channel = normalize_channel(channel.as_deref())?;
    }
    if let Some(env) = &request.env {
        meta.env = Some(sanitize_env(env.clone())?);
    }
    if let Some(parent) = clean(meta.parent.as_deref()) {
        check_parent(cfg, id, &parent)?;
    }
    validate_meta(&meta)?;

    let config_yaml = match &request.config_yaml {
        None => layer.config_yaml.clone(),
        Some(None) => None,
        Some(Some(text)) if text.trim().is_empty() => None,
        Some(Some(text)) => {
            validate_config_yaml(text)?;
            Some(text.clone())
        }
    };
    if meta.parent.is_none() && config_yaml.is_none() {
        return Err(TemplateError::Invalid(
            "config_yaml is required for a template without a parent".to_string(),
        ));
    }

    let soul_md = match &request.soul_md {
        None => layer.soul_md.clone(),
        Some(None) => None,
        Some(Some(text)) if text.is_empty() => None,
        Some(Some(text)) => Some(text.clone()),
    };
    let defaults = match &request.defaults {
        None => layer.defaults.clone().unwrap_or_default(),
        Some(None) => TemplateDefaults::default(),
        Some(Some(defaults)) => {
            validate_defaults(defaults)?;
            defaults.clone()
        }
    };

    write_template_files(
        cfg,
        id,
        &meta,
        config_yaml.as_deref(),
        soul_md.as_deref(),
        &defaults,
    )?;
    let now = now_iso();
    upsert_index(db, id, &meta, false, &now)?;
    let updated = load_layer(cfg, id)?;
    append_version(db, id, "update", actor, &snapshot_of(&updated))?;
    detail(cfg, db, id)
}

/// Delete a custom template (`DELETE /api/templates/{id}`).
pub fn delete(cfg: &Config, db: &Db, id: &str, actor: &str) -> Result<(), TemplateError> {
    let layer = ensure_custom(cfg, db, id)?;
    let instances = db.with_conn(|conn| {
        let mut statement =
            conn.prepare("SELECT id FROM instances WHERE template = ?1 ORDER BY id")?;
        let rows = statement
            .query_map(rusqlite::params![id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;
    let children = db.with_conn(|conn| {
        let mut statement = conn
            .prepare("SELECT id FROM templates WHERE parent = ?1 AND read_only = 0 ORDER BY id")?;
        let rows = statement
            .query_map(rusqlite::params![id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;
    if !instances.is_empty() || !children.is_empty() {
        return Err(TemplateError::InUse {
            id: id.to_string(),
            instances,
            templates: children,
        });
    }

    append_version(db, id, "delete", actor, &snapshot_of(&layer))?;
    let dir = cfg.template_dir(id);
    if dir.exists() {
        fs::remove_dir_all(&dir)
            .map_err(|err| TemplateError::Io(format!("{}: {err}", dir.display())))?;
    }
    delete_index(db, id)?;
    Ok(())
}

/// Save an existing instance as a custom template
/// (`POST /api/templates/from-instance`).
pub fn from_instance(
    cfg: &Config,
    db: &Db,
    request: &FromInstanceRequest,
    actor: &str,
) -> Result<TemplateDetail, TemplateError> {
    let instance_id = clean(request.instance_id.as_deref())
        .ok_or_else(|| TemplateError::Invalid("instance_id is required".to_string()))?;
    validate_instance_id(&instance_id)
        .map_err(|_| TemplateError::InstanceNotFound(instance_id.clone()))?;
    let spec = db
        .get(&instance_id)?
        .ok_or_else(|| TemplateError::InstanceNotFound(instance_id.clone()))?;

    let config_path = cfg.data_dir(&instance_id).join("config.yaml");
    let config_yaml = fs::read_to_string(&config_path).map_err(|err| {
        TemplateError::Invalid(format!(
            "cannot read the rendered config of \"{instance_id}\" at {}: {err}",
            config_path.display()
        ))
    })?;
    validate_config_yaml(&config_yaml)?;
    let soul_md = read_optional(&cfg.data_dir(&instance_id).join("SOUL.md"))?;

    let env = match crate::envs::load(cfg, db, &instance_id) {
        Ok(entries) => entries
            .into_iter()
            .map(|entry| EnvSuggestion {
                key: entry.key,
                description: format!("Copied from instance \"{instance_id}\"."),
                example: None,
                required: false,
                format: None,
                secret: entry.secret,
            })
            .collect(),
        Err(err) => {
            tracing::warn!(instance = %instance_id, %err, "could not read the env schema of the source instance");
            Vec::new()
        }
    };

    let defaults = TemplateDefaults {
        model: Some(spec.model.clone()),
        base_url: Some(spec.base_url.clone()),
        mem_limit_mb: Some(spec.mem_limit_mb),
        cpu_limit: Some(spec.cpu_limit),
        allow_lan: Some(spec.allow_lan),
        notes: (!spec.notes.trim().is_empty()).then(|| spec.notes.clone()),
    };
    let request = CreateTemplateRequest {
        id: request.id.clone(),
        name: Some(
            clean(request.name.as_deref()).unwrap_or_else(|| format!("{} template", spec.name)),
        ),
        description: Some(format!("Saved from instance \"{instance_id}\".")),
        parent: None,
        default_model: Some(spec.model.clone()),
        channel: Some(spec.channel.as_str().to_string()),
        env: Some(env),
        config_yaml: Some(config_yaml),
        soul_md,
        defaults: Some(defaults),
    };
    create_inner(cfg, db, &request, actor, Some(&instance_id))
}

/// Restore one version (`POST /api/templates/{id}/restore`).
pub fn restore(
    cfg: &Config,
    db: &Db,
    id: &str,
    version: i64,
    actor: &str,
) -> Result<TemplateDetail, TemplateError> {
    ensure_custom(cfg, db, id)?;
    let row = version_row(db, id, version)?.ok_or_else(|| TemplateError::VersionNotFound {
        id: id.to_string(),
        version,
    })?;
    let snapshot: TemplateSnapshot = serde_json::from_str(&row.snapshot)
        .map_err(|err| TemplateError::Invalid(format!("version {version}: {err}")))?;
    validate_meta(&snapshot.metadata)?;
    validate_defaults(&snapshot.defaults)?;
    if let Some(text) = &snapshot.config_yaml {
        validate_config_yaml(text)?;
    }
    if snapshot.metadata.parent.is_none()
        && snapshot
            .config_yaml
            .as_deref()
            .map(|text| text.trim().is_empty())
            .unwrap_or(true)
    {
        return Err(TemplateError::Invalid(
            "the snapshot has no config.yaml and no parent to inherit one from".to_string(),
        ));
    }
    if let Some(parent) = clean(snapshot.metadata.parent.as_deref()) {
        check_parent(cfg, id, &parent)?;
    }

    write_template_files(
        cfg,
        id,
        &snapshot.metadata,
        snapshot.config_yaml.as_deref(),
        snapshot.soul_md.as_deref(),
        &snapshot.defaults,
    )?;
    let now = now_iso();
    upsert_index(db, id, &snapshot.metadata, false, &now)?;
    append_version(db, id, "update", actor, &snapshot)?;
    detail(cfg, db, id)
}

/// Every recorded version of one template, oldest first.
pub fn versions(cfg: &Config, db: &Db, id: &str) -> Result<Vec<TemplateVersion>, TemplateError> {
    let id = id.trim();
    validate_template_id(id).map_err(|_| TemplateError::NotFound(id.to_string()))?;
    if templates_builtin::find(id).is_some() {
        return Ok(Vec::new());
    }
    if !cfg.template_dir(id).is_dir() && index_row(db, id)?.is_none() {
        return Err(TemplateError::NotFound(id.to_string()));
    }
    let mut versions = Vec::new();
    for row in version_rows(db, id)? {
        let mut sha256 = BTreeMap::new();
        if let Ok(snapshot) = serde_json::from_str::<TemplateSnapshot>(&row.snapshot) {
            if let Some(text) = &snapshot.config_yaml {
                sha256.insert("config.yaml".to_string(), sha256_hex(text));
            }
            if let Some(text) = &snapshot.soul_md {
                sha256.insert("soul.md".to_string(), sha256_hex(text));
            }
            if !snapshot.defaults.is_empty() {
                if let Ok(text) = serde_yaml::to_string(&snapshot.defaults) {
                    sha256.insert("defaults.yaml".to_string(), sha256_hex(&text));
                }
            }
        }
        versions.push(TemplateVersion {
            version: row.version,
            action: row.action,
            actor: row.actor,
            at: row.at,
            sha256,
        });
    }
    Ok(versions)
}

// ---------------------------------------------------------------------------
// Index (SQLite)
// ---------------------------------------------------------------------------

/// Rebuild the template index from the filesystem and the built-ins.
///
/// Called at startup. A broken custom directory is logged and skipped: it must
/// never prevent the control plane from serving the rest of the fleet.
pub fn reindex(cfg: &Config, db: &Db) -> Result<(), TemplateError> {
    fs::create_dir_all(cfg.templates_dir())
        .map_err(|err| TemplateError::Io(format!("{}: {err}", cfg.templates_dir().display())))?;
    let now = now_iso();

    for builtin in templates_builtin::BUILTINS {
        let meta = TemplateMeta {
            name: Some(builtin.name.to_string()),
            description: Some(builtin.description.to_string()),
            parent: None,
            default_model: Some(builtin.default_model.to_string()),
            channel: Some(builtin.channel.to_string()),
            env: Some(builtin.env.iter().map(builtin_suggestion).collect()),
            source_instance: None,
        };
        upsert_index(db, builtin.id, &meta, true, &now)?;
    }

    let mut seen = BTreeSet::new();
    let entries = match fs::read_dir(cfg.templates_dir()) {
        Ok(entries) => entries,
        Err(err) => {
            return Err(TemplateError::Io(format!(
                "{}: {err}",
                cfg.templates_dir().display()
            )))
        }
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if validate_template_id(&id).is_err() {
            continue;
        }
        if templates_builtin::find(&id).is_some() {
            continue;
        }
        match resolve(cfg, &id) {
            Ok(resolved) => {
                seen.insert(id.clone());
                upsert_index(db, &id, &resolved.meta, false, &now)?;
            }
            Err(err) => {
                tracing::warn!(template = %id, %err, "could not index a custom template");
            }
        }
    }

    for (id, row) in index_rows(db)? {
        if !row.read_only && !seen.contains(&id) {
            delete_index(db, &id)?;
        }
    }
    Ok(())
}

fn upsert_index(
    db: &Db,
    id: &str,
    meta: &TemplateMeta,
    read_only: bool,
    now: &str,
) -> Result<(), TemplateError> {
    let stored = meta.clone();
    let metadata =
        serde_json::to_string(&stored).map_err(|err| TemplateError::Db(err.to_string()))?;
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO templates (id, parent, read_only, metadata, source_instance, \
             created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
             ON CONFLICT(id) DO UPDATE SET parent = excluded.parent, \
             read_only = excluded.read_only, metadata = excluded.metadata, \
             source_instance = excluded.source_instance, updated_at = excluded.updated_at",
            rusqlite::params![
                id,
                clean(meta.parent.as_deref()),
                read_only as i64,
                metadata,
                meta.source_instance.as_deref(),
                now,
            ],
        )?;
        Ok(())
    })?;
    Ok(())
}

fn index_row(db: &Db, id: &str) -> Result<Option<IndexRow>, TemplateError> {
    Ok(db.with_conn(|conn| {
        conn.query_row(
            "SELECT parent, read_only, metadata, source_instance, created_at, updated_at \
             FROM templates WHERE id = ?1",
            rusqlite::params![id],
            |row| {
                Ok(IndexRow {
                    parent: row.get(0)?,
                    read_only: row.get::<_, i64>(1)? != 0,
                    metadata: parse_meta(row.get::<_, String>(2)?),
                    source_instance: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )
        .optional()
    })?)
}

fn index_rows(db: &Db) -> Result<BTreeMap<String, IndexRow>, TemplateError> {
    let rows = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, parent, read_only, metadata, source_instance, created_at, updated_at \
             FROM templates ORDER BY id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    IndexRow {
                        parent: row.get(1)?,
                        read_only: row.get::<_, i64>(2)? != 0,
                        metadata: parse_meta(row.get::<_, String>(3)?),
                        source_instance: row.get(4)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().collect())
    })?;
    Ok(rows)
}

fn delete_index(db: &Db, id: &str) -> Result<(), TemplateError> {
    db.with_conn(|conn| {
        conn.execute("DELETE FROM templates WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    })?;
    Ok(())
}

fn parse_meta(text: String) -> TemplateMeta {
    serde_json::from_str(&text).unwrap_or_default()
}

fn instance_counts(db: &Db) -> Result<BTreeMap<String, usize>, TemplateError> {
    Ok(db.with_conn(|conn| {
        let mut statement =
            conn.prepare("SELECT template, COUNT(*) FROM instances GROUP BY template")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().collect())
    })?)
}

fn append_version(
    db: &Db,
    id: &str,
    action: &str,
    actor: &str,
    snapshot: &TemplateSnapshot,
) -> Result<i64, TemplateError> {
    let snapshot =
        serde_json::to_string(snapshot).map_err(|err| TemplateError::Db(err.to_string()))?;
    let at = now_iso();
    let version = db.with_transaction(|conn| {
        let next: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM template_versions WHERE template_id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO template_versions (template_id, version, snapshot, actor, at, action) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, next, snapshot, actor, at, action],
        )?;
        Ok(next)
    })?;
    Ok(version)
}

fn version_rows(db: &Db, id: &str) -> Result<Vec<VersionRow>, TemplateError> {
    Ok(db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT version, action, actor, at, snapshot FROM template_versions \
             WHERE template_id = ?1 ORDER BY version",
        )?;
        let rows = statement
            .query_map(rusqlite::params![id], |row| {
                Ok(VersionRow {
                    version: row.get(0)?,
                    action: row.get(1)?,
                    actor: row.get(2)?,
                    at: row.get(3)?,
                    snapshot: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?)
}

fn version_row(db: &Db, id: &str, version: i64) -> Result<Option<VersionRow>, TemplateError> {
    Ok(db.with_conn(|conn| {
        conn.query_row(
            "SELECT version, action, actor, at, snapshot FROM template_versions \
             WHERE template_id = ?1 AND version = ?2",
            rusqlite::params![id, version],
            |row| {
                Ok(VersionRow {
                    version: row.get(0)?,
                    action: row.get(1)?,
                    actor: row.get(2)?,
                    at: row.get(3)?,
                    snapshot: row.get(4)?,
                })
            },
        )
        .optional()
    })?)
}

fn versions_count(db: &Db, id: &str) -> Result<i64, TemplateError> {
    Ok(db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM template_versions WHERE template_id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
    })?)
}

// ---------------------------------------------------------------------------
// Validation and file helpers
// ---------------------------------------------------------------------------

fn ensure_custom(cfg: &Config, db: &Db, id: &str) -> Result<Layer, TemplateError> {
    validate_template_id(id).map_err(|_| TemplateError::NotFound(id.to_string()))?;
    if templates_builtin::find(id).is_some() {
        return Err(TemplateError::ReadOnly(id.to_string()));
    }
    match load_layer(cfg, id)? {
        layer if layer.read_only => Err(TemplateError::ReadOnly(id.to_string())),
        layer => {
            if index_row(db, id)?.is_none() {
                // On-disk template that was never indexed (control plane
                // started before it was written): adopt it.
                let now = now_iso();
                upsert_index(db, id, &layer.meta, false, &now)?;
            }
            Ok(layer)
        }
    }
}

fn check_parent(cfg: &Config, id: &str, parent: &str) -> Result<(), TemplateError> {
    let mut visited = BTreeSet::new();
    let mut cursor = Some(parent.to_string());
    while let Some(current) = cursor {
        if current == id {
            return Err(TemplateError::Cycle(format!("{id} -> {current}")));
        }
        if !visited.insert(current.clone()) {
            let path: Vec<&str> = visited.iter().map(String::as_str).collect();
            return Err(TemplateError::Cycle(format!(
                "{} -> {current}",
                path.join(" -> ")
            )));
        }
        if visited.len() > MAX_INHERITANCE_DEPTH {
            return Err(TemplateError::Cycle(format!(
                "inheritance deeper than {MAX_INHERITANCE_DEPTH}"
            )));
        }
        let layer = load_layer(cfg, &current)?;
        cursor = clean(layer.meta.parent.as_deref());
    }
    Ok(())
}

fn validate_meta(meta: &TemplateMeta) -> Result<(), TemplateError> {
    if let Some(name) = &meta.name {
        required_text(name, "name", 128)?;
    }
    if let Some(description) = &meta.description {
        if description.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(TemplateError::Invalid(format!(
                "description must be at most {MAX_DESCRIPTION_CHARS} characters"
            )));
        }
    }
    if let Some(model) = &meta.default_model {
        validate_model(model).map_err(TemplateError::Invalid)?;
    }
    if let Some(channel) = &meta.channel {
        if ChannelKind::parse(channel).is_none() {
            return Err(TemplateError::Invalid(format!(
                "channel must be telegram, discord or none (got \"{channel}\")"
            )));
        }
    }
    if let Some(env) = &meta.env {
        sanitize_env(env.clone())?;
    }
    Ok(())
}

fn validate_defaults(defaults: &TemplateDefaults) -> Result<(), TemplateError> {
    if let Some(model) = &defaults.model {
        validate_model(model).map_err(TemplateError::Invalid)?;
    }
    if let Some(base_url) = &defaults.base_url {
        validate_base_url(base_url).map_err(TemplateError::Invalid)?;
    }
    if let Some(mem) = defaults.mem_limit_mb {
        validate_mem_limit(mem).map_err(TemplateError::Invalid)?;
    }
    if let Some(cpu) = defaults.cpu_limit {
        validate_cpu_limit(cpu).map_err(TemplateError::Invalid)?;
    }
    if let Some(notes) = &defaults.notes {
        if notes.chars().count() > 4096 {
            return Err(TemplateError::Invalid(
                "defaults.notes must be at most 4096 characters".to_string(),
            ));
        }
    }
    Ok(())
}

fn sanitize_env(env: Vec<EnvSuggestion>) -> Result<Vec<EnvSuggestion>, TemplateError> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(env.len());
    for mut entry in env {
        entry.key = entry.key.trim().to_string();
        validate_env_key(&entry.key).map_err(|message| {
            TemplateError::Invalid(format!("env \"{}\": {message}", entry.key))
        })?;
        if !seen.insert(entry.key.clone()) {
            return Err(TemplateError::Invalid(format!(
                "environment variable \"{}\" is listed twice",
                entry.key
            )));
        }
        entry.description = entry.description.trim().to_string();
        if entry.description.chars().count() > MAX_ENV_TEXT_CHARS {
            return Err(TemplateError::Invalid(format!(
                "env \"{}\": description must be at most {MAX_ENV_TEXT_CHARS} characters",
                entry.key
            )));
        }
        entry.example = entry
            .example
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if entry
            .example
            .as_deref()
            .is_some_and(|value| value.chars().count() > MAX_ENV_TEXT_CHARS)
        {
            return Err(TemplateError::Invalid(format!(
                "env \"{}\": example must be at most {MAX_ENV_TEXT_CHARS} characters",
                entry.key
            )));
        }
        entry.format = entry
            .format
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if entry
            .format
            .as_deref()
            .is_some_and(|value| value.chars().count() > MAX_ENV_FORMAT_CHARS)
        {
            return Err(TemplateError::Invalid(format!(
                "env \"{}\": format must be at most {MAX_ENV_FORMAT_CHARS} characters",
                entry.key
            )));
        }
        out.push(entry);
    }
    Ok(out)
}

fn validate_config_yaml(text: &str) -> Result<(), TemplateError> {
    let doc: Value = serde_yaml::from_str(text)
        .map_err(|err| TemplateError::Invalid(format!("config_yaml is not valid YAML: {err}")))?;
    if !doc.is_mapping() {
        return Err(TemplateError::Invalid(
            "config_yaml must be a YAML mapping".to_string(),
        ));
    }
    Ok(())
}

fn normalize_channel(value: Option<&str>) -> Result<Option<String>, TemplateError> {
    let Some(value) = clean(value) else {
        return Ok(None);
    };
    let kind = ChannelKind::parse(&value).ok_or_else(|| {
        TemplateError::Invalid(format!(
            "channel must be telegram, discord or none (got \"{value}\")"
        ))
    })?;
    Ok(Some(kind.as_str().to_string()))
}

fn required_text(value: &str, field: &str, max: usize) -> Result<String, TemplateError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(TemplateError::Invalid(format!("{field} must not be empty")));
    }
    if trimmed.chars().count() > max {
        return Err(TemplateError::Invalid(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn write_template_files(
    cfg: &Config,
    id: &str,
    meta: &TemplateMeta,
    config_yaml: Option<&str>,
    soul_md: Option<&str>,
    defaults: &TemplateDefaults,
) -> Result<(), TemplateError> {
    let dir = cfg.template_dir(id);
    fs::create_dir_all(&dir)
        .map_err(|err| TemplateError::Io(format!("{}: {err}", dir.display())))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|err| TemplateError::Io(format!("{}: {err}", dir.display())))?;

    let meta_yaml = serde_yaml::to_string(meta)
        .map_err(|err| TemplateError::Invalid(format!("cannot serialise template.yaml: {err}")))?;
    write_file(&dir.join("template.yaml"), &meta_yaml)?;

    match config_yaml {
        Some(text) => write_file(&dir.join("config.yaml"), text)?,
        None => remove_if_exists(&dir.join("config.yaml"))?,
    }
    match soul_md {
        Some(text) if !text.is_empty() => {
            write_file(&dir.join("soul.md"), text)?;
            remove_if_exists(&dir.join("SOUL.md"))?;
        }
        _ => {
            remove_if_exists(&dir.join("soul.md"))?;
            remove_if_exists(&dir.join("SOUL.md"))?;
        }
    }
    if defaults.is_empty() {
        remove_if_exists(&dir.join("defaults.yaml"))?;
    } else {
        let defaults_yaml = serde_yaml::to_string(defaults).map_err(|err| {
            TemplateError::Invalid(format!("cannot serialise defaults.yaml: {err}"))
        })?;
        write_file(&dir.join("defaults.yaml"), &defaults_yaml)?;
    }
    Ok(())
}

fn write_file(path: &Path, contents: &str) -> Result<(), TemplateError> {
    fs::write(path, contents)
        .map_err(|err| TemplateError::Io(format!("{}: {err}", path.display())))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|err| TemplateError::Io(format!("{}: {err}", path.display())))?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<(), TemplateError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(TemplateError::Io(format!("{}: {err}", path.display()))),
    }
}

fn snapshot_of(layer: &Layer) -> TemplateSnapshot {
    TemplateSnapshot {
        metadata: layer.meta.clone(),
        config_yaml: layer.config_yaml.clone(),
        soul_md: layer.soul_md.clone(),
        defaults: layer.defaults.clone().unwrap_or_default(),
    }
}

fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        (dir, cfg)
    }

    fn db(cfg: &Config) -> Db {
        Db::open(&cfg.db_path()).expect("database")
    }

    fn request(id: &str, parent: Option<&str>) -> CreateTemplateRequest {
        CreateTemplateRequest {
            id: Some(id.to_string()),
            name: Some(format!("Template {id}")),
            parent: parent.map(str::to_string),
            channel: Some("none".to_string()),
            ..CreateTemplateRequest::default()
        }
    }

    #[test]
    fn builtins_resolve_read_only() {
        let (_dir, cfg) = cfg();
        let guest = resolve(&cfg, "guest").expect("guest resolves");
        assert!(guest.read_only);
        assert_eq!(guest.meta.channel.as_deref(), Some("telegram"));
        assert_eq!(
            guest.config_yaml,
            templates_builtin::find("guest").unwrap().config_yaml
        );
        assert!(resolve(&cfg, "nope").is_err());
    }

    #[test]
    fn inheritance_merges_env_and_defaults_child_wins() {
        let (_dir, cfg) = cfg();
        let db = db(&cfg);
        let mut base = request("base", None);
        base.env = Some(vec![
            EnvSuggestion::new("SHARED", "base description"),
            EnvSuggestion::new("BASE_ONLY", "base only"),
        ]);
        base.defaults = Some(TemplateDefaults {
            mem_limit_mb: Some(1024),
            cpu_limit: Some(1.0),
            ..TemplateDefaults::default()
        });
        create(&cfg, &db, &base, "tester").expect("base created");

        let mut child = request("child", Some("base"));
        child.env = Some(vec![EnvSuggestion {
            key: "SHARED".to_string(),
            description: "child description".to_string(),
            example: None,
            required: true,
            format: None,
            secret: false,
        }]);
        child.defaults = Some(TemplateDefaults {
            mem_limit_mb: Some(2048),
            ..TemplateDefaults::default()
        });
        let detail = create(&cfg, &db, &child, "tester").expect("child created");

        assert_eq!(detail.info.parent.as_deref(), Some("base"));
        let env: Vec<&str> = detail
            .info
            .env
            .iter()
            .map(|entry| entry.key.as_str())
            .collect();
        assert_eq!(env, ["SHARED", "BASE_ONLY"]);
        let shared = &detail.info.env[0];
        assert_eq!(shared.description, "child description");
        assert!(shared.required);
        assert_eq!(detail.info.defaults.mem_limit_mb, Some(2048));
        assert_eq!(detail.info.defaults.cpu_limit, Some(1.0));
        // The child wrote no config: the base one is inherited live.
        assert_eq!(
            detail.config_yaml,
            templates_builtin::find("solo").unwrap().config_yaml
        );
        assert!(!cfg.template_config_path("child").exists());
    }

    #[test]
    fn cycles_are_refused_before_writing() {
        let (_dir, cfg) = cfg();
        let db = db(&cfg);
        create(&cfg, &db, &request("one", None), "tester").expect("one");
        create(&cfg, &db, &request("two", Some("one")), "tester").expect("two");

        let mut looped = UpdateTemplateRequest {
            parent: Some(Some("two".to_string())),
            ..UpdateTemplateRequest::default()
        };
        let err = update(&cfg, &db, "one", &looped, "tester").expect_err("cycle");
        assert!(matches!(err, TemplateError::Cycle(_)), "{err}");

        looped.parent = Some(Some("one".to_string()));
        let err = update(&cfg, &db, "one", &looped, "tester").expect_err("self parent");
        assert!(matches!(err, TemplateError::Cycle(_)), "{err}");
    }

    #[test]
    fn delete_refuses_children_and_is_restore_aware() {
        let (_dir, cfg) = cfg();
        let db = db(&cfg);
        create(&cfg, &db, &request("base", None), "tester").expect("base");
        create(&cfg, &db, &request("child", Some("base")), "tester").expect("child");

        let err = delete(&cfg, &db, "base", "tester").expect_err("in use");
        match err {
            TemplateError::InUse { templates, .. } => assert_eq!(templates, ["child"]),
            other => panic!("unexpected error: {other}"),
        }

        delete(&cfg, &db, "child", "tester").expect("child deleted");
        delete(&cfg, &db, "base", "tester").expect("base deleted");
        assert!(!cfg.template_dir("base").exists());
        assert!(resolve(&cfg, "base").is_err());
    }

    #[test]
    fn versions_capture_files_and_restore_rewrites_them() {
        let (_dir, cfg) = cfg();
        let db = db(&cfg);
        let mut create_request = request("house", None);
        create_request.soul_md = Some("# House\n".to_string());
        create(&cfg, &db, &create_request, "tester").expect("created");

        let update_request = UpdateTemplateRequest {
            name: Some("Renamed".to_string()),
            soul_md: Some(Some("# Renamed\n".to_string())),
            ..UpdateTemplateRequest::default()
        };
        update(&cfg, &db, "house", &update_request, "tester").expect("updated");
        assert_eq!(
            fs::read_to_string(cfg.template_dir("house").join("soul.md")).unwrap(),
            "# Renamed\n"
        );

        let history = versions(&cfg, &db, "house").expect("versions");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].action, "create");
        assert_eq!(history[1].action, "update");
        assert!(history[0].sha256.contains_key("soul.md"));

        restore(&cfg, &db, "house", 1, "tester").expect("restored");
        assert_eq!(
            fs::read_to_string(cfg.template_dir("house").join("soul.md")).unwrap(),
            "# House\n"
        );
        let detail = detail(&cfg, &db, "house").expect("detail");
        assert_eq!(detail.info.name, "Template house");
        assert_eq!(versions(&cfg, &db, "house").unwrap().len(), 3);

        let err = restore(&cfg, &db, "house", 99, "tester").expect_err("missing version");
        assert!(matches!(err, TemplateError::VersionNotFound { .. }));
    }

    #[test]
    fn builtins_cannot_be_deleted_or_updated() {
        let (_dir, cfg) = cfg();
        let db = db(&cfg);
        assert!(matches!(
            delete(&cfg, &db, "guest", "tester"),
            Err(TemplateError::ReadOnly(_))
        ));
        assert!(matches!(
            update(
                &cfg,
                &db,
                "guest",
                &UpdateTemplateRequest::default(),
                "tester"
            ),
            Err(TemplateError::ReadOnly(_))
        ));
    }
}
