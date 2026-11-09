//! Reusable personas (issue #55).
//!
//! A persona is a personality an instance can adopt: a Markdown template for
//! `data/SOUL.md` plus the variables that template is rendered with. The same
//! persona assigned to several instances keeps them in sync until one of them
//! is edited by hand or the persona itself changes.
//!
//! # Storage
//!
//! `$FLEETT_ROOT/personas/<id>/` holds two files:
//!
//! * `persona.yaml` — `name`, `description`, `variables` (`key`, `description`,
//!   `default`) and `tags`;
//! * `SOUL.md` — the template, at most [`MAX_SOUL_BYTES`] of UTF-8, with
//!   `{{variable}}` placeholders.
//!
//! The whole `personas/` directory is one git repository (`.git` at its root),
//! so a persona is versioned from its first save. Only those two files, inside
//! directories whose name is a valid persona id, ever reach the index: a
//! stray `secrets.env` or a note dropped in the directory can never be
//! committed. `git2` is used directly, like [`crate::history`], so the `.deb`
//! and the container image need no `git` binary.
//!
//! # Rendering
//!
//! A placeholder is `{{key}}` (inner spaces allowed) where `key` matches
//! `^[a-z][a-z0-9_]{0,31}$`. Anything else — `{{MODEL}}`, `{{ not a key }}` —
//! is left untouched, so the instance placeholders keep working. A key is
//! resolved from, in order: `agent_name` (the instance name, when an instance
//! is known), the instance overrides, the persona default. A key with no value
//! is reported in `missing` and kept literally in the output; applying such a
//! render is refused rather than shipping a `{{tone}}` to an agent.

use crate::config::Config;
use crate::db::Db;
use crate::history::{Author, CommitInfo, DiffResponse, HistoryError};
use crate::models::{
    validate_persona_id, validate_persona_variable_key, InstanceSpec, PersonaBinding,
};
use git2::{Commit, DiffFormat, DiffOptions, Repository, Signature, Sort, Tree};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Directory name of the repository inside the personas root.
pub const GIT_DIR: &str = ".git";
/// Metadata file of one persona.
pub const META_FILE: &str = "persona.yaml";
/// Template file of one persona.
pub const SOUL_FILE: &str = "SOUL.md";
/// Name of the implicit variable filled with the instance name.
pub const AGENT_NAME_VAR: &str = "agent_name";
/// Longest accepted template. The rendered form follows the file-API cap.
pub const MAX_SOUL_BYTES: usize = 64 * 1024;
/// Longest accepted persona name.
pub const MAX_NAME_CHARS: usize = 128;
/// Longest accepted persona description.
pub const MAX_DESCRIPTION_CHARS: usize = 4096;
/// Longest accepted variable description.
pub const MAX_VARIABLE_DESCRIPTION_CHARS: usize = 1024;
/// Longest accepted variable default.
pub const MAX_VARIABLE_DEFAULT_CHARS: usize = 4096;
/// Variables a persona may declare.
pub const MAX_VARIABLES: usize = 64;
/// Tags a persona may carry.
pub const MAX_TAGS: usize = 32;
/// Longest accepted tag.
pub const MAX_TAG_CHARS: usize = 32;
/// Characters of the gallery excerpt.
pub const EXCERPT_CHARS: usize = 180;

#[derive(Debug, thiserror::Error)]
pub enum PersonaError {
    #[error("unknown persona \"{0}\"")]
    NotFound(String),
    #[error("persona \"{0}\" already exists")]
    Exists(String),
    #[error("unknown commit \"{0}\"")]
    UnknownCommit(String),
    #[error("invalid persona: {0}")]
    Invalid(String),
    #[error("persona \"{id}\" is still in use")]
    InUse { id: String, instances: Vec<String> },
    #[error("persona filesystem error: {0}")]
    Io(String),
    #[error("persona git error: {0}")]
    Git(String),
    #[error("persona database error: {0}")]
    Db(String),
}

impl PersonaError {
    fn io(path: &Path, err: io::Error) -> Self {
        Self::Io(format!("{}: {err}", path.display()))
    }
}

impl From<git2::Error> for PersonaError {
    fn from(err: git2::Error) -> Self {
        Self::Git(err.message().to_string())
    }
}

impl From<crate::db::DbError> for PersonaError {
    fn from(err: crate::db::DbError) -> Self {
        Self::Db(err.to_string())
    }
}

impl From<HistoryError> for PersonaError {
    fn from(err: HistoryError) -> Self {
        match err {
            HistoryError::Io(err) => Self::Io(err.to_string()),
            HistoryError::Git(err) => Self::Git(err.message().to_string()),
            other => Self::Git(other.to_string()),
        }
    }
}

/// One variable declared by a persona (`variables:` of `persona.yaml`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaVariable {
    /// Matches `^[a-z][a-z0-9_]{0,31}$`.
    pub key: String,
    #[serde(default)]
    pub description: String,
    /// Value used when the instance (or the request) supplies none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// `persona.yaml` as stored. Every field is optional on disk so a hand-written
/// file only has to declare what it needs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersonaFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    variables: Vec<PersonaVariable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

/// One persona loaded from disk.
#[derive(Debug, Clone)]
pub struct Persona {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub variables: Vec<PersonaVariable>,
    /// Template text, before rendering.
    pub soul_md: String,
}

impl Persona {
    /// Declared variable by key.
    pub fn variable(&self, key: &str) -> Option<&PersonaVariable> {
        self.variables.iter().find(|entry| entry.key == key)
    }

    /// First meaningful line of the template, for the gallery.
    pub fn excerpt(&self) -> String {
        excerpt_of(&self.soul_md)
    }
}

/// `GET /api/personas` entry.
#[derive(Debug, Clone, Serialize)]
pub struct PersonaSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub variables: Vec<PersonaVariable>,
    /// Instances currently applying the persona.
    pub instances_count: usize,
    /// First meaningful line of the template, bounded by [`EXCERPT_CHARS`].
    pub excerpt: String,
    pub created_at: String,
    pub updated_at: String,
}

/// `GET /api/personas/{id}`.
#[derive(Debug, Clone, Serialize)]
pub struct PersonaDetail {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub variables: Vec<PersonaVariable>,
    pub soul_md: String,
    /// Instances currently applying the persona, sorted.
    pub instances: Vec<String>,
    pub instances_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of `POST /api/personas`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreatePersonaRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub variables: Option<Vec<PersonaVariable>>,
    #[serde(default)]
    pub soul_md: Option<String>,
}

/// Body of `PATCH /api/personas/{id}`: a missing field is left untouched, an
/// empty `description` clears it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdatePersonaRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub variables: Option<Vec<PersonaVariable>>,
    #[serde(default)]
    pub soul_md: Option<String>,
}

/// Body of `POST /api/personas/{id}/preview`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PreviewRequest {
    /// Overrides, on top of the instance values (when one is named) and the
    /// persona defaults.
    #[serde(default)]
    pub variables: Option<BTreeMap<String, String>>,
    /// Instance whose name feeds `agent_name` and whose stored values seed the
    /// overrides.
    #[serde(default)]
    pub instance: Option<String>,
}

/// Response of `POST /api/personas/{id}/preview`.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewResponse {
    /// The rendered template, missing placeholders included verbatim.
    pub rendered: String,
    /// Keys used by the template with no value (sorted).
    pub missing: Vec<String>,
}

/// Body of `POST /api/personas/{id}/apply`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApplyRequest {
    /// Instances the persona is written to, in order.
    #[serde(default)]
    pub instances: Vec<String>,
    /// Overrides merged into the stored values of every instance.
    #[serde(default)]
    pub variables: Option<BTreeMap<String, String>>,
}

/// One line of the apply report.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub id: String,
    /// `applied` or `failed`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response of `POST /api/personas/{id}/apply`.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyResponse {
    pub results: Vec<ApplyResult>,
}

/// Result of rendering one persona for one instance.
#[derive(Debug, Clone)]
pub struct RenderedPersona {
    pub rendered: String,
    /// Keys with no value after defaults, overrides and `agent_name`.
    pub missing: Vec<String>,
}

/// Persona state surfaced on an instance (issue #55).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaState {
    /// `data/SOUL.md` is exactly the current render of the persona.
    Applied,
    /// The file was edited by hand, the persona changed, or it is unreadable.
    Diverged,
    /// No persona assigned.
    None,
}

impl PersonaState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Diverged => "diverged",
            Self::None => "none",
        }
    }
}

// ---------------------------------------------------------------------------
// Paths and files
// ---------------------------------------------------------------------------

/// Directory of one persona, after validating the id.
fn persona_dir(cfg: &Config, id: &str) -> Result<PathBuf, PersonaError> {
    validate_persona_id(id).map_err(PersonaError::Invalid)?;
    Ok(cfg.persona_dir(id))
}

fn read_optional(path: &Path) -> Result<Option<String>, PersonaError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(PersonaError::io(path, err)),
    }
}

fn write_file(path: &Path, contents: &str) -> Result<(), PersonaError> {
    fs::write(path, contents).map_err(|err| PersonaError::io(path, err))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|err| PersonaError::io(path, err))?;
    Ok(())
}

/// Load one persona from disk (`persona.yaml` + `SOUL.md`).
pub fn load(cfg: &Config, id: &str) -> Result<Persona, PersonaError> {
    let dir = persona_dir(cfg, id)?;
    let meta_path = dir.join(META_FILE);
    let soul_path = dir.join(SOUL_FILE);
    let Some(text) = read_optional(&meta_path)? else {
        return Err(PersonaError::NotFound(id.to_string()));
    };
    let file: PersonaFile = serde_yaml::from_str(&text).map_err(|err| {
        PersonaError::Invalid(format!("{} is not valid YAML: {err}", meta_path.display()))
    })?;
    let Some(soul_md) = read_optional(&soul_path)? else {
        return Err(PersonaError::Invalid(format!(
            "persona \"{id}\" has no {SOUL_FILE}"
        )));
    };
    let name = file
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(id)
        .to_string();
    let description = file
        .description
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let mut persona = Persona {
        id: id.to_string(),
        name,
        description,
        tags: normalize_tags(file.tags),
        variables: file.variables,
        soul_md,
    };
    validate_persona(&persona)?;
    // Normalise what a hand-written file may have drifted on.
    persona.variables = normalize_variables(&persona.variables)?;
    Ok(persona)
}

/// Every persona of the fleet, sorted by id. A broken directory is logged and
/// skipped so one bad file never hides the rest of the gallery.
pub fn list(cfg: &Config, db: &Db) -> Result<Vec<PersonaSummary>, PersonaError> {
    let counts = instance_counts(db)?;
    let mut out = Vec::new();
    for id in ids_on_disk(cfg) {
        match load(cfg, &id) {
            Ok(persona) => {
                let (created_at, updated_at) = timestamps(cfg, &id);
                out.push(PersonaSummary {
                    id: persona.id.clone(),
                    name: persona.name.clone(),
                    description: persona.description.clone(),
                    tags: persona.tags.clone(),
                    variables: persona.variables.clone(),
                    instances_count: counts.get(&persona.id).copied().unwrap_or(0),
                    excerpt: persona.excerpt(),
                    created_at,
                    updated_at,
                });
            }
            Err(err) => {
                tracing::warn!(persona = %id, %err, "skipping an unreadable persona")
            }
        }
    }
    out.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(out)
}

/// One persona with its instances and timestamps.
pub fn detail(cfg: &Config, db: &Db, id: &str) -> Result<PersonaDetail, PersonaError> {
    let persona = load(cfg, id)?;
    let instances = db.instances_using_persona(&persona.id)?;
    let (created_at, updated_at) = timestamps(cfg, &persona.id);
    Ok(PersonaDetail {
        id: persona.id.clone(),
        name: persona.name,
        description: persona.description,
        tags: persona.tags,
        variables: persona.variables,
        soul_md: persona.soul_md,
        instances_count: instances.len(),
        instances,
        created_at,
        updated_at,
    })
}

/// Create one persona and commit it.
pub fn create(
    cfg: &Config,
    db: &Db,
    request: &CreatePersonaRequest,
    author: &Author,
) -> Result<PersonaDetail, PersonaError> {
    let id = request
        .id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PersonaError::Invalid("id is required".to_string()))?;
    validate_persona_id(id).map_err(PersonaError::Invalid)?;
    let dir = cfg.persona_dir(id);
    if dir.exists() {
        return Err(PersonaError::Exists(id.to_string()));
    }
    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PersonaError::Invalid("name is required".to_string()))?;
    let soul_md = request
        .soul_md
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| PersonaError::Invalid("soul_md is required".to_string()))?;
    let persona = Persona {
        id: id.to_string(),
        name: name.to_string(),
        description: request
            .description
            .as_deref()
            .unwrap_or_default()
            .to_string(),
        tags: normalize_tags(request.tags.clone().unwrap_or_default()),
        variables: normalize_variables(&request.variables.clone().unwrap_or_default())?,
        soul_md: soul_md.to_string(),
    };
    validate_persona(&persona)?;
    write_persona(cfg, &persona)?;
    commit(cfg, &persona.id, author)?;
    detail(cfg, db, &persona.id)
}

/// Update one persona and commit the result.
pub fn update(
    cfg: &Config,
    db: &Db,
    id: &str,
    request: &UpdatePersonaRequest,
    author: &Author,
) -> Result<PersonaDetail, PersonaError> {
    let mut persona = load(cfg, id)?;
    if let Some(name) = request.name.as_deref() {
        let name = name.trim();
        if name.is_empty() {
            return Err(PersonaError::Invalid("name must not be empty".to_string()));
        }
        persona.name = name.to_string();
    }
    if let Some(description) = request.description.as_deref() {
        persona.description = description.trim().to_string();
    }
    if let Some(tags) = &request.tags {
        persona.tags = normalize_tags(tags.clone());
    }
    if let Some(variables) = &request.variables {
        persona.variables = normalize_variables(variables)?;
    }
    if let Some(soul_md) = request.soul_md.as_deref() {
        if soul_md.trim().is_empty() {
            return Err(PersonaError::Invalid(
                "soul_md must not be empty".to_string(),
            ));
        }
        persona.soul_md = soul_md.to_string();
    }
    validate_persona(&persona)?;
    write_persona(cfg, &persona)?;
    commit(cfg, &persona.id, author)?;
    detail(cfg, db, &persona.id)
}

/// Delete one persona; an assigned persona is refused.
///
/// The deletion is committed when the repository already exists, so the
/// history keeps the removal even though the files are gone. A git failure is
/// logged: the directory is already deleted, turning it into a `500` would
/// only hide that.
pub fn delete(cfg: &Config, db: &Db, id: &str, author: &Author) -> Result<(), PersonaError> {
    let persona = load(cfg, id)?;
    let instances = db.instances_using_persona(&persona.id)?;
    if !instances.is_empty() {
        return Err(PersonaError::InUse {
            id: persona.id,
            instances,
        });
    }
    let dir = persona_dir(cfg, &persona.id)?;
    fs::remove_dir_all(&dir).map_err(|err| PersonaError::io(&dir, err))?;
    if let Err(err) = commit(cfg, &persona.id, author) {
        tracing::warn!(persona = %persona.id, %err, "could not record the persona deletion");
    }
    Ok(())
}

fn write_persona(cfg: &Config, persona: &Persona) -> Result<(), PersonaError> {
    let dir = cfg.persona_dir(&persona.id);
    fs::create_dir_all(&dir).map_err(|err| PersonaError::io(&dir, err))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|err| PersonaError::io(&dir, err))?;
    let file = PersonaFile {
        name: Some(persona.name.clone()),
        description: Some(persona.description.clone()),
        variables: persona.variables.clone(),
        tags: persona.tags.clone(),
    };
    let yaml = serde_yaml::to_string(&file)
        .map_err(|err| PersonaError::Invalid(format!("cannot serialise {META_FILE}: {err}")))?;
    write_file(&dir.join(META_FILE), &yaml)?;
    write_file(&dir.join(SOUL_FILE), &persona.soul_md)?;
    Ok(())
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for tag in tags {
        let tag = tag.trim();
        if !tag.is_empty() {
            out.insert(tag.to_string());
        }
    }
    out.into_iter().collect()
}

fn normalize_variables(
    variables: &[PersonaVariable],
) -> Result<Vec<PersonaVariable>, PersonaError> {
    if variables.len() > MAX_VARIABLES {
        return Err(PersonaError::Invalid(format!(
            "at most {MAX_VARIABLES} variables are allowed"
        )));
    }
    let mut out = Vec::with_capacity(variables.len());
    let mut seen = BTreeSet::new();
    for variable in variables {
        let key = variable.key.trim();
        validate_persona_variable_key(key).map_err(PersonaError::Invalid)?;
        if !seen.insert(key.to_string()) {
            return Err(PersonaError::Invalid(format!(
                "variable \"{key}\" is declared twice"
            )));
        }
        let description = variable.description.trim().to_string();
        if description.chars().count() > MAX_VARIABLE_DESCRIPTION_CHARS {
            return Err(PersonaError::Invalid(format!(
                "description of \"{key}\" must be at most {MAX_VARIABLE_DESCRIPTION_CHARS} characters"
            )));
        }
        let default = variable
            .default
            .as_deref()
            .map(str::to_string)
            .filter(|value| !value.is_empty());
        if let Some(value) = &default {
            if value.chars().count() > MAX_VARIABLE_DEFAULT_CHARS {
                return Err(PersonaError::Invalid(format!(
                    "default of \"{key}\" must be at most {MAX_VARIABLE_DEFAULT_CHARS} characters"
                )));
            }
        }
        out.push(PersonaVariable {
            key: key.to_string(),
            description,
            default,
        });
    }
    Ok(out)
}

fn validate_persona(persona: &Persona) -> Result<(), PersonaError> {
    if persona.name.trim().is_empty() {
        return Err(PersonaError::Invalid("name must not be empty".to_string()));
    }
    if persona.name.chars().count() > MAX_NAME_CHARS {
        return Err(PersonaError::Invalid(format!(
            "name must be at most {MAX_NAME_CHARS} characters"
        )));
    }
    if persona.description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(PersonaError::Invalid(format!(
            "description must be at most {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    if persona.tags.len() > MAX_TAGS {
        return Err(PersonaError::Invalid(format!(
            "at most {MAX_TAGS} tags are allowed"
        )));
    }
    for tag in &persona.tags {
        if tag.chars().count() > MAX_TAG_CHARS {
            return Err(PersonaError::Invalid(format!(
                "tag \"{tag}\" must be at most {MAX_TAG_CHARS} characters"
            )));
        }
    }
    if persona.soul_md.trim().is_empty() {
        return Err(PersonaError::Invalid(
            "soul_md must not be empty".to_string(),
        ));
    }
    if persona.soul_md.len() > MAX_SOUL_BYTES {
        return Err(PersonaError::Invalid(format!(
            "soul_md must be at most {MAX_SOUL_BYTES} bytes"
        )));
    }
    if persona.soul_md.contains('\0') {
        return Err(PersonaError::Invalid(
            "soul_md must not contain a NUL byte".to_string(),
        ));
    }
    Ok(())
}

fn excerpt_of(soul_md: &str) -> String {
    for line in soul_md.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("---") {
            continue;
        }
        return line
            .chars()
            .take(EXCERPT_CHARS)
            .collect::<String>()
            .trim()
            .to_string();
    }
    soul_md.trim().chars().take(EXCERPT_CHARS).collect()
}

fn ids_on_disk(cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(cfg.personas_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == GIT_DIR || validate_persona_id(&name).is_err() {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() && !meta.file_type().is_symlink() {
            out.push(name);
        }
    }
    out.sort();
    out
}

fn instance_counts(db: &Db) -> Result<HashMap<String, usize>, PersonaError> {
    let mut counts = HashMap::new();
    for binding in db.instance_personas()?.values() {
        if let Some(persona) = &binding.persona {
            *counts.entry(persona.clone()).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

/// `(created_at, updated_at)` from the persona git history, with the file
/// modification time as a fallback for a directory copied in by hand.
fn timestamps(cfg: &Config, id: &str) -> (String, String) {
    if let Some((created, updated)) = git_timestamps(cfg, id) {
        return (created, updated);
    }
    fallback_timestamp(&cfg.persona_dir(id).join(SOUL_FILE))
}

fn fallback_timestamp(path: &Path) -> (String, String) {
    let at = fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|time| {
            chrono::DateTime::<chrono::Utc>::from(time)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
        .unwrap_or_default();
    (at.clone(), at)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn variable_key(key: &str) -> bool {
    validate_persona_variable_key(key).is_ok()
}

/// Render `persona.soul_md` with `overrides` and, when an instance is known,
/// `agent_name`.
///
/// Missing keys are returned (sorted) and left verbatim in the output.
pub fn render(
    persona: &Persona,
    overrides: &BTreeMap<String, String>,
    agent_name: Option<&str>,
) -> RenderedPersona {
    let mut out = String::with_capacity(persona.soul_md.len());
    let mut missing = BTreeSet::new();
    let mut rest = persona.soul_md.as_str();
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // An unterminated `{{` is plain text.
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let inner = after[..end].trim();
        let literal = &rest[start..start + 2 + end + 2];
        if !variable_key(inner) {
            out.push_str(literal);
        } else {
            match resolve_value(persona, inner, overrides, agent_name) {
                Some(value) => out.push_str(&value),
                None => {
                    missing.insert(inner.to_string());
                    out.push_str(literal);
                }
            }
        }
        rest = &rest[start + 2 + end + 2..];
    }
    out.push_str(rest);
    RenderedPersona {
        rendered: out,
        missing: missing.into_iter().collect(),
    }
}

fn resolve_value(
    persona: &Persona,
    key: &str,
    overrides: &BTreeMap<String, String>,
    agent_name: Option<&str>,
) -> Option<String> {
    if key == AGENT_NAME_VAR {
        if let Some(name) = agent_name.filter(|name| !name.trim().is_empty()) {
            return Some(name.to_string());
        }
    }
    if let Some(value) = overrides.get(key) {
        return Some(value.clone());
    }
    persona
        .variable(key)
        .and_then(|variable| variable.default.clone())
}

/// Render a persona without applying it (`POST .../preview`).
pub fn preview(
    cfg: &Config,
    id: &str,
    overrides: &BTreeMap<String, String>,
    agent_name: Option<&str>,
) -> Result<PreviewResponse, PersonaError> {
    let persona = load(cfg, id)?;
    let rendered = render(&persona, overrides, agent_name);
    Ok(PreviewResponse {
        rendered: rendered.rendered,
        missing: rendered.missing,
    })
}

/// Render a persona for one instance, ready to be written.
///
/// Returns the overrides that were used (the instance stores them, the defaults
/// stay in the persona) and the digest of the rendered `SOUL.md`.
pub fn render_for(
    cfg: &Config,
    id: &str,
    overrides: &BTreeMap<String, String>,
    agent_name: &str,
) -> Result<(RenderedPersona, String), PersonaError> {
    let persona = load(cfg, id)?;
    let rendered = render(&persona, overrides, Some(agent_name));
    let hash = digest(&rendered.rendered);
    Ok((rendered, hash))
}

/// SHA-256 of a rendered `SOUL.md`, hex encoded.
pub fn digest(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Current `SOUL.md` of one instance, when it is readable.
pub fn current_soul(cfg: &Config, instance_id: &str) -> Option<String> {
    fs::read_to_string(cfg.data_dir(instance_id).join(SOUL_FILE)).ok()
}

/// Divergence state of one instance against its persona.
///
/// `applied` means the file byte-for-byte equals a fresh render with the
/// currently stored variables; anything else (hand edit, persona edit, missing
/// file, deleted persona) is `diverged`.
pub fn state(cfg: &Config, spec: &InstanceSpec, binding: &PersonaBinding) -> PersonaState {
    let Some(id) = binding.persona.as_deref() else {
        return PersonaState::None;
    };
    let Ok((rendered, _hash)) = render_for(cfg, id, &binding.variables, &spec.name) else {
        return PersonaState::Diverged;
    };
    if rendered.missing.is_empty()
        && current_soul(cfg, &spec.id).as_deref() == Some(&rendered.rendered)
    {
        PersonaState::Applied
    } else {
        PersonaState::Diverged
    }
}

/// Write `data/SOUL.md` for one instance.
///
/// The previous bytes are copied next to it (`.bak-YYYYMMDD-HHMMSS`); the file
/// is written `0600` and, when the process is allowed to, owned by the agent
/// uid/gid so the container can read it.
pub fn write_soul(cfg: &Config, instance_id: &str, rendered: &str) -> Result<(), PersonaError> {
    let data = cfg.data_dir(instance_id);
    fs::create_dir_all(&data).map_err(|err| PersonaError::io(&data, err))?;
    let target = data.join(SOUL_FILE);
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(PersonaError::Invalid(format!(
                "{} is a symlink; refusing to write through it",
                target.display()
            )))
        }
        Ok(_) => {
            let backup = crate::files::backup_path(&target);
            fs::copy(&target, &backup).map_err(|err| PersonaError::io(&backup, err))?;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(PersonaError::io(&target, err)),
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&target)
        .map_err(|err| PersonaError::io(&target, err))?;
    file.write_all(rendered.as_bytes())
        .map_err(|err| PersonaError::io(&target, err))?;
    file.flush().map_err(|err| PersonaError::io(&target, err))?;
    drop(file);
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
        .map_err(|err| PersonaError::io(&target, err))?;
    let uid = crate::store::CONTAINER_UID;
    let gid = crate::store::CONTAINER_GID;
    if let Err(err) = std::os::unix::fs::chown(&target, Some(uid), Some(gid)) {
        if err.kind() != io::ErrorKind::PermissionDenied {
            tracing::debug!(path = %target.display(), %err, "could not change the SOUL.md owner");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Git repository
// ---------------------------------------------------------------------------

fn open_existing(cfg: &Config) -> Result<Option<Repository>, PersonaError> {
    if !cfg.personas_dir().join(GIT_DIR).is_dir() {
        return Ok(None);
    }
    Repository::open(cfg.personas_dir())
        .map(Some)
        .map_err(PersonaError::from)
}

fn open_or_init(cfg: &Config) -> Result<Repository, PersonaError> {
    if let Some(repo) = open_existing(cfg)? {
        return Ok(repo);
    }
    fs::create_dir_all(cfg.personas_dir())
        .map_err(|err| PersonaError::io(&cfg.personas_dir(), err))?;
    let repo = Repository::init(cfg.personas_dir())?;
    if repo.head().is_err() {
        // Deterministic first branch, whatever the host global config says.
        let _ = repo.set_head("refs/heads/main");
    }
    Ok(repo)
}

fn head_commit(repo: &Repository) -> Result<Option<Commit<'_>>, PersonaError> {
    match repo.head() {
        Ok(head) => Ok(Some(head.peel_to_commit()?)),
        Err(err)
            if matches!(
                err.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) =>
        {
            Ok(None)
        }
        Err(err) => Err(err.into()),
    }
}

/// Versioned paths of the repository: `<id>/persona.yaml` and `<id>/SOUL.md`
/// for every directory whose name is a valid persona id.
///
/// This whitelist is the reason a secret dropped in `personas/<id>/` can never
/// be committed.
fn tracked_files(cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(cfg.personas_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == GIT_DIR || validate_persona_id(&name).is_err() {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        for file in [META_FILE, SOUL_FILE] {
            let path = entry.path().join(file);
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_file() && !meta.file_type().is_symlink() {
                out.push(format!("{name}/{file}"));
            }
        }
    }
    out.sort();
    out
}

/// Stage the whitelist and commit when it differs from `HEAD`.
///
/// Returns the commit (`None` when nothing changed) and refuses to commit
/// anything when the repository holds no whitelisted path at all.
fn commit(
    cfg: &Config,
    persona_id: &str,
    author: &Author,
) -> Result<Option<CommitInfo>, PersonaError> {
    let repo = open_or_init(cfg)?;
    let files = tracked_files(cfg);
    let mut index = repo.index()?;
    index.clear()?;
    for relative in &files {
        index.add_path(Path::new(relative))?;
    }
    index.write()?;
    let tree_id = index.write_tree()?;
    let head = head_commit(&repo)?;
    if head
        .as_ref()
        .is_some_and(|commit| commit.tree_id() == tree_id)
    {
        return Ok(None);
    }
    if head.is_none() && files.is_empty() {
        return Ok(None);
    }
    let tree = repo.find_tree(tree_id)?;
    let head_tree = head.as_ref().map(|commit| commit.tree()).transpose()?;
    let changed = diff_paths(&repo, head_tree.as_ref(), &tree)?;
    let message = format!("ui: persona {persona_id}");
    let signature = Signature::now(&author.name, &author.email)?;
    let parents: Vec<Commit<'_>> = head.into_iter().collect();
    let parent_refs: Vec<&Commit<'_>> = parents.iter().collect();
    let oid = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &message,
        &tree,
        &parent_refs,
    )?;
    let id = oid.to_string();
    let commit = repo.find_commit(oid)?;
    Ok(Some(CommitInfo {
        short: short_id(&id),
        id,
        author: author.name.clone(),
        email: author.email.clone(),
        at: commit_time(&commit),
        message,
        paths: changed,
    }))
}

/// `(created_at, updated_at)` of one persona, from its commits.
fn git_timestamps(cfg: &Config, id: &str) -> Option<(String, String)> {
    let repo = open_existing(cfg).ok().flatten()?;
    let head = head_commit(&repo).ok().flatten()?;
    let prefix = format!("{id}/");
    let mut walk = repo.revwalk().ok()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).ok()?;
    walk.push(head.id()).ok()?;
    let mut created = String::new();
    let mut updated = String::new();
    for oid in walk.flatten().take(5000) {
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let Ok(paths) = commit_paths(&repo, &commit) else {
            continue;
        };
        if !paths.iter().any(|path| path.starts_with(&prefix)) {
            continue;
        }
        let at = commit_time(&commit);
        if updated.is_empty() {
            updated = at.clone();
        }
        created = at;
    }
    if updated.is_empty() {
        return None;
    }
    Some((created, updated))
}

/// `GET /api/personas/{id}/git/log` — commits that touched this persona,
/// newest first.
pub fn log(cfg: &Config, id: &str, limit: usize) -> Result<Vec<CommitInfo>, PersonaError> {
    validate_persona_id(id).map_err(PersonaError::Invalid)?;
    // A persona that no longer exists has no page to show history in.
    if !cfg.persona_dir(id).is_dir() {
        return Err(PersonaError::NotFound(id.to_string()));
    }
    let Some(repo) = open_existing(cfg)? else {
        return Ok(Vec::new());
    };
    let Some(head) = head_commit(&repo)? else {
        return Ok(Vec::new());
    };
    let prefix = format!("{id}/");
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    walk.push(head.id())?;
    let mut commits = Vec::new();
    for oid in walk {
        let commit = repo.find_commit(oid?)?;
        let paths = commit_paths(&repo, &commit)?;
        if !paths.iter().any(|path| path.starts_with(&prefix)) {
            continue;
        }
        commits.push(commit_info(&commit, paths));
        if commits.len() >= limit {
            break;
        }
    }
    Ok(commits)
}

/// `GET /api/personas/{id}/git/diff` — unified patch of this persona only.
pub fn diff(
    cfg: &Config,
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<DiffResponse, PersonaError> {
    validate_persona_id(id).map_err(PersonaError::Invalid)?;
    if !cfg.persona_dir(id).is_dir() {
        return Err(PersonaError::NotFound(id.to_string()));
    }
    let Some(repo) = open_existing(cfg)? else {
        return Ok(DiffResponse::default());
    };
    let Some(head) = head_commit(&repo)? else {
        return Ok(DiffResponse::default());
    };
    let to_commit = match to.map(str::trim).filter(|value| !value.is_empty()) {
        Some(revision) => resolve_commit(&repo, revision)?,
        None => head,
    };
    let from_commit = match from.map(str::trim).filter(|value| !value.is_empty()) {
        Some(revision) => Some(resolve_commit(&repo, revision)?),
        None => to_commit.parent(0).ok(),
    };
    let to_tree = to_commit.tree()?;
    let from_tree = from_commit
        .as_ref()
        .map(|commit| commit.tree())
        .transpose()?;
    let text = render_diff(&repo, &format!("{id}/"), from_tree.as_ref(), &to_tree)?;
    Ok(DiffResponse {
        from: from_commit
            .map(|commit| commit.id().to_string())
            .unwrap_or_default(),
        to: to_commit.id().to_string(),
        diff: text,
    })
}

fn resolve_commit<'repo>(
    repo: &'repo Repository,
    revision: &str,
) -> Result<Commit<'repo>, PersonaError> {
    repo.revparse_single(revision)
        .and_then(|object| object.peel_to_commit())
        .map_err(|_| PersonaError::UnknownCommit(revision.to_string()))
}

fn commit_paths(repo: &Repository, commit: &Commit<'_>) -> Result<Vec<String>, PersonaError> {
    let tree = commit.tree()?;
    let parent = commit
        .parent(0)
        .ok()
        .map(|parent| parent.tree())
        .transpose()?;
    diff_paths(repo, parent.as_ref(), &tree)
}

fn diff_paths(
    repo: &Repository,
    from: Option<&Tree<'_>>,
    to: &Tree<'_>,
) -> Result<Vec<String>, PersonaError> {
    let mut options = DiffOptions::new();
    options.include_typechange(true);
    let diff = repo.diff_tree_to_tree(from, Some(to), Some(&mut options))?;
    let mut paths = BTreeSet::new();
    for delta in diff.deltas() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|path| path.to_string_lossy().to_string());
        if let Some(path) = path {
            paths.insert(path);
        }
    }
    Ok(paths.into_iter().collect())
}

fn render_diff(
    repo: &Repository,
    prefix: &str,
    from: Option<&Tree<'_>>,
    to: &Tree<'_>,
) -> Result<String, PersonaError> {
    let mut options = DiffOptions::new();
    options.include_typechange(true);
    options.pathspec(prefix);
    let diff = repo.diff_tree_to_tree(from, Some(to), Some(&mut options))?;
    let mut text = String::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        match line.origin() {
            // File and hunk headers already carry their prefix.
            'F' | 'H' | 'B' => text.push_str(&String::from_utf8_lossy(line.content())),
            origin => {
                text.push(origin);
                text.push_str(&String::from_utf8_lossy(line.content()));
            }
        }
        true
    })?;
    Ok(text)
}

fn commit_info(commit: &Commit<'_>, paths: Vec<String>) -> CommitInfo {
    let id = commit.id().to_string();
    let author = commit.author();
    CommitInfo {
        short: short_id(&id),
        id,
        author: author.name().unwrap_or_default().to_string(),
        email: author.email().unwrap_or_default().to_string(),
        at: commit_time(commit),
        message: commit.message().unwrap_or_default().trim_end().to_string(),
        paths,
    }
}

fn commit_time(commit: &Commit<'_>) -> String {
    let time = commit.time();
    chrono::DateTime::<chrono::Utc>::from_timestamp(time.seconds(), 0)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn short_id(id: &str) -> String {
    id.chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona(soul_md: &str, variables: Vec<PersonaVariable>) -> Persona {
        Persona {
            id: "house".to_string(),
            name: "House".to_string(),
            description: String::new(),
            tags: Vec::new(),
            variables,
            soul_md: soul_md.to_string(),
        }
    }

    fn variable(key: &str, default: Option<&str>) -> PersonaVariable {
        PersonaVariable {
            key: key.to_string(),
            description: String::new(),
            default: default.map(str::to_string),
        }
    }

    #[test]
    fn renders_defaults_overrides_and_agent_name() {
        let persona = persona(
            "Tu es {{agent_name}}, ton {{ tone }} en {{language}}.",
            vec![variable("tone", Some("direct")), variable("language", None)],
        );
        let overrides = BTreeMap::from([("language".to_string(), "français".to_string())]);
        let rendered = render(&persona, &overrides, Some("Agent alice"));
        assert_eq!(
            rendered.rendered,
            "Tu es Agent alice, ton direct en français."
        );
        assert!(rendered.missing.is_empty());

        // No instance: `agent_name` is missing but defaults still apply.
        let rendered = render(&persona, &BTreeMap::new(), None);
        assert_eq!(
            rendered.missing,
            vec!["agent_name".to_string(), "language".to_string()]
        );
        assert!(rendered.rendered.contains("{{agent_name}}"));
    }

    #[test]
    fn leaves_foreign_placeholders_alone() {
        let persona = persona("{{MODEL}} {{ not a key }} {{}} {{ truncated", Vec::new());
        let rendered = render(&persona, &BTreeMap::new(), None);
        assert_eq!(rendered.rendered, persona.soul_md);
        assert!(rendered.missing.is_empty());
    }

    #[test]
    fn inner_whitespace_and_edges_are_rendered() {
        let persona = persona(
            "a{{ tone}}b{{tone }}c{{  tone  }}d{{tone}}",
            vec![variable("tone", Some("x"))],
        );
        assert_eq!(
            render(&persona, &BTreeMap::new(), None).rendered,
            "axbxcxdx"
        );
    }

    #[test]
    fn excerpt_skips_headings() {
        assert_eq!(
            excerpt_of("# Identité\n\nTu es {{agent_name}}.\n"),
            "Tu es {{agent_name}}."
        );
        assert_eq!(excerpt_of(""), "");
    }

    #[test]
    fn digest_is_stable() {
        assert_eq!(digest("soul"), digest("soul"));
        assert_ne!(digest("soul"), digest("soul\n"));
    }

    #[test]
    fn variable_keys_follow_the_documented_grammar() {
        for key in ["a", "tone", "agent_1", "a1234567890123456789012345678901"] {
            assert!(variable_key(key), "{key}");
        }
        for key in [
            "",
            "Tone",
            "1tone",
            "ton e",
            "a-b",
            "a12345678901234567890123456789012",
        ] {
            assert!(!variable_key(key), "{key}");
        }
    }
}
