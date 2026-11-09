//! Declarative fleet state (`fleet.yaml`, issue #34).
//!
//! The document describes the providers and instances the fleet should have.
//! [`plan`] compares it with the live index and returns field-level changes;
//! [`apply`] executes them through the same store, render and driver machinery
//! as the HTTP API, then records the document in `desired_state`. Recomputed
//! against the live index, that stored document is the drift report.
//!
//! ## Secrets
//!
//! A value never appears in the file. Every secret is a *reference*:
//!
//! * `env:NAME` reads the environment of the control plane process (or of the
//!   environment file it loaded);
//! * `file:/path/to/secret` reads a single-line file, trailing whitespace
//!   trimmed;
//! * `infisical:…` is refused: a vault integration is not shipped yet, and an
//!   operator can always inject the value as an environment variable.
//!
//! References are resolved at apply time only. [`plan`] never reads a secret,
//! so a plan can be pasted into a pull request without redaction.
//!
//! ## Deletion
//!
//! The document is the whole fleet: an instance or provider that it does not
//! declare is a deletion candidate. Deletions are only executed when
//! `allow_delete` is set (`fleett apply --allow-delete`, or
//! `?allow_delete=true` on the API); otherwise they are reported in
//! [`Plan::refused`] and the rest of the document is still applied.

use crate::api::error::ApiError;
use crate::api::AppState;
use crate::config::Config;
use crate::db::{Db, DbError};
use crate::driver::DriverError;
use crate::envs::{self, EnvError, EnvWrite};
use crate::models::{
    now_iso, validate_api_key_env, validate_base_url, validate_cpu_limit, validate_env_key,
    validate_env_raw_value, validate_instance_id, validate_mem_limit, validate_model,
    validate_notes, validate_provider_kind, validate_provider_name, validate_secret_name,
    validate_secret_value, validate_share_name, InstanceSpec, ProviderSpec, ShareMode, ShareRef,
    Status, DEFAULT_API_KEY_ENV, DEFAULT_CPU_LIMIT, DEFAULT_MEM_LIMIT_MB, DEFAULT_TEMPLATE,
};
use crate::providers;
use crate::render::{self, RenderError};
use crate::store;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Default URL of an instance declared without a provider.
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
/// Longest accepted `source` of an apply.
pub const SOURCE_MAX: usize = 64;

// ---------------------------------------------------------------------------
// Document schema
// ---------------------------------------------------------------------------

/// One `fleet.yaml`, parsed and validated.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FleetDocument {
    #[serde(default)]
    pub providers: Vec<ProviderDocument>,
    #[serde(default)]
    pub instances: Vec<InstanceDocument>,
    /// Accepted for the upcoming scheduling subsystem. Parsed and validated,
    /// but not reconciled by this version.
    #[serde(default)]
    pub schedules: Vec<ScheduleDocument>,
}

/// One reusable provider credential.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderDocument {
    pub name: String,
    pub kind: String,
    pub base_url: String,
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    /// Reference resolved at apply time. Required to create a provider; when
    /// omitted on an existing provider its stored secret is left untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
}

/// One agent instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InstanceDocument {
    pub id: String,
    #[serde(default = "default_template")]
    pub template: String,
    /// Display name (defaults to the id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Model; defaults to the template default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider name declared in `providers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_limit_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_limit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_lan: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The complete environment of the instance: a key that is not declared is
    /// removed by an apply.
    #[serde(default)]
    pub env: Vec<EnvDocument>,
    #[serde(default)]
    pub shares: Vec<ShareDocument>,
}

/// One environment variable. Exactly one of `value` and `secret_ref` is set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnvDocument {
    pub key: String,
    /// Literal value of a non-secret variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Reference to a secret value, resolved at apply time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
}

/// One share mount.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShareDocument {
    pub name: String,
    #[serde(default = "default_share_mode")]
    pub mode: ShareMode,
}

/// Reserved schedule entry (accepted, not reconciled by this version).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScheduleDocument {
    pub instance: String,
    pub cron: String,
    pub command: String,
}

fn default_template() -> String {
    DEFAULT_TEMPLATE.to_string()
}

fn default_api_key_env() -> String {
    DEFAULT_API_KEY_ENV.to_string()
}

fn default_share_mode() -> ShareMode {
    ShareMode::Rw
}

impl FleetDocument {
    /// Parse a YAML (or JSON) document and validate every field.
    pub fn parse(text: &str) -> Result<Self, GitopsError> {
        let document: Self = serde_yaml::from_str(text)
            .map_err(|err| GitopsError::Invalid(format!("invalid YAML: {err}")))?;
        document.validate()?;
        Ok(document)
    }

    /// Validate the document without touching the live fleet.
    pub fn validate(&self) -> Result<(), GitopsError> {
        let mut provider_names = BTreeSet::new();
        for provider in &self.providers {
            validate_provider_name(&provider.name)
                .map_err(|message| GitopsError::Invalid(format!("provider: {message}")))?;
            if !provider_names.insert(provider.name.as_str()) {
                return Err(GitopsError::Invalid(format!(
                    "provider \"{}\" is declared twice",
                    provider.name
                )));
            }
            validate_provider_kind(&provider.kind).map_err(|message| {
                GitopsError::Invalid(format!("provider \"{}\": {message}", provider.name))
            })?;
            validate_base_url(&provider.base_url).map_err(|message| {
                GitopsError::Invalid(format!("provider \"{}\": {message}", provider.name))
            })?;
            validate_api_key_env(&provider.api_key_env).map_err(|message| {
                GitopsError::Invalid(format!("provider \"{}\": {message}", provider.name))
            })?;
            if let Some(reference) = &provider.secret_ref {
                validate_secret_ref(reference).map_err(|message| {
                    GitopsError::Invalid(format!("provider \"{}\": {message}", provider.name))
                })?;
            }
        }

        let mut ids = BTreeSet::new();
        for instance in &self.instances {
            validate_instance_id(&instance.id)
                .map_err(|message| GitopsError::Invalid(format!("instance: {message}")))?;
            if !ids.insert(instance.id.as_str()) {
                return Err(GitopsError::Invalid(format!(
                    "instance \"{}\" is declared twice",
                    instance.id
                )));
            }
            validate_share_name(&instance.template).map_err(|message| {
                GitopsError::Invalid(format!("instance \"{}\": {message}", instance.id))
            })?;
            if let Some(name) = &instance.name {
                let name = name.trim();
                if name.is_empty() {
                    return Err(GitopsError::Invalid(format!(
                        "instance \"{}\": name must not be empty",
                        instance.id
                    )));
                }
                if name.chars().count() > 128 {
                    return Err(GitopsError::Invalid(format!(
                        "instance \"{}\": name must be at most 128 characters",
                        instance.id
                    )));
                }
            }
            if let Some(model) = &instance.model {
                validate_model(model).map_err(|message| {
                    GitopsError::Invalid(format!("instance \"{}\": {message}", instance.id))
                })?;
            }
            if let Some(provider) = &instance.provider {
                validate_provider_name(provider).map_err(|message| {
                    GitopsError::Invalid(format!("instance \"{}\": {message}", instance.id))
                })?;
            }
            if let Some(mem) = instance.mem_limit_mb {
                validate_mem_limit(mem).map_err(|message| {
                    GitopsError::Invalid(format!("instance \"{}\": {message}", instance.id))
                })?;
            }
            if let Some(cpu) = instance.cpu_limit {
                validate_cpu_limit(cpu).map_err(|message| {
                    GitopsError::Invalid(format!("instance \"{}\": {message}", instance.id))
                })?;
            }
            if let Some(notes) = &instance.notes {
                validate_notes(notes).map_err(|message| {
                    GitopsError::Invalid(format!("instance \"{}\": {message}", instance.id))
                })?;
            }

            let mut keys = BTreeSet::new();
            for env in &instance.env {
                validate_env_key(&env.key).map_err(|message| {
                    GitopsError::Invalid(format!("instance \"{}\": {message}", instance.id))
                })?;
                if !keys.insert(env.key.as_str()) {
                    return Err(GitopsError::Invalid(format!(
                        "instance \"{}\": environment variable \"{}\" is listed twice",
                        instance.id, env.key
                    )));
                }
                match (&env.value, &env.secret_ref) {
                    (Some(value), None) => validate_env_raw_value(value).map_err(|message| {
                        GitopsError::Invalid(format!("instance \"{}\": {message}", instance.id))
                    })?,
                    (None, Some(reference)) => {
                        validate_secret_ref(reference).map_err(|message| {
                            GitopsError::Invalid(format!("instance \"{}\": {message}", instance.id))
                        })?;
                    }
                    (Some(_), Some(_)) => {
                        return Err(GitopsError::Invalid(format!(
                            "instance \"{}\": environment variable \"{}\" sets both value and \
                             secret_ref; a secret value must use secret_ref only",
                            instance.id, env.key
                        )))
                    }
                    (None, None) => {
                        return Err(GitopsError::Invalid(format!(
                            "instance \"{}\": environment variable \"{}\" needs a value or a \
                             secret_ref",
                            instance.id, env.key
                        )))
                    }
                }
            }

            let mut shares = BTreeSet::new();
            for share in &instance.shares {
                validate_share_name(&share.name).map_err(|message| {
                    GitopsError::Invalid(format!("instance \"{}\": {message}", instance.id))
                })?;
                if !shares.insert(share.name.as_str()) {
                    return Err(GitopsError::Invalid(format!(
                        "instance \"{}\": share \"{}\" is listed twice",
                        instance.id, share.name
                    )));
                }
            }
        }

        for schedule in &self.schedules {
            validate_instance_id(&schedule.instance)
                .map_err(|message| GitopsError::Invalid(format!("schedule: {message}")))?;
            if schedule.cron.trim().is_empty() {
                return Err(GitopsError::Invalid(
                    "schedule: cron must not be empty".to_string(),
                ));
            }
            if schedule.command.trim().is_empty() {
                return Err(GitopsError::Invalid(
                    "schedule: command must not be empty".to_string(),
                ));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Secret references
// ---------------------------------------------------------------------------

/// Validate the shape of a `secret_ref` without resolving it.
pub fn validate_secret_ref(reference: &str) -> Result<(), String> {
    let reference = reference.trim();
    if let Some(name) = reference.strip_prefix("env:") {
        validate_secret_name(name.trim())?;
        return Ok(());
    }
    if let Some(path) = reference.strip_prefix("file:") {
        if path.trim().is_empty() {
            return Err("file: secret_ref needs a path".to_string());
        }
        if !path.starts_with('/') {
            return Err("file: secret_ref needs an absolute path".to_string());
        }
        if path.contains(['\n', '\r', '\0']) {
            return Err("file: secret_ref must be a single line".to_string());
        }
        return Ok(());
    }
    if reference.starts_with("infisical:") {
        return Err(
            "infisical: secret references are not implemented; inject the value with env: instead"
                .to_string(),
        );
    }
    Err(format!(
        "unsupported secret_ref \"{reference}\": use env:NAME or file:/path"
    ))
}

/// Resolve a `secret_ref` to its value. Never logs or stores the result.
pub fn resolve_secret_ref(reference: &str) -> Result<String, GitopsError> {
    let reference = reference.trim();
    validate_secret_ref(reference).map_err(GitopsError::Secret)?;
    let value = if let Some(name) = reference.strip_prefix("env:") {
        std::env::var(name.trim()).map_err(|_| {
            GitopsError::Secret(format!(
                "secret_ref \"env:{}\" is not set in the control plane environment",
                name.trim()
            ))
        })?
    } else {
        let path = reference
            .strip_prefix("file:")
            .expect("validate_secret_ref accepted the reference");
        std::fs::read_to_string(path).map_err(|err| {
            GitopsError::Secret(format!("cannot read secret_ref file \"{path}\": {err}"))
        })?
    };
    let value = value.trim().to_string();
    validate_secret_value(&value).map_err(GitopsError::Secret)?;
    if value.is_empty() {
        return Err(GitopsError::Secret(format!(
            "secret_ref \"{reference}\" resolved to an empty value"
        )));
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum GitopsError {
    /// The document itself is wrong.
    #[error("{0}")]
    Invalid(String),
    /// A secret reference cannot be resolved.
    #[error("{0}")]
    Secret(String),
    /// The request conflicts with the live fleet (a provider still in use, …).
    #[error("{0}")]
    Conflict(String),
    /// A store, render or driver operation failed.
    #[error("{0}")]
    Operation(String),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Env(#[from] EnvError),
    #[error(transparent)]
    Render(#[from] RenderError),
}

impl From<ApiError> for GitopsError {
    fn from(err: ApiError) -> Self {
        Self::Operation(err.message)
    }
}

impl From<DriverError> for GitopsError {
    fn from(err: DriverError) -> Self {
        Self::Operation(format!(
            "docker compose {} failed: {}",
            err.operation,
            err.detail.trim()
        ))
    }
}

// ---------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------

/// One planned change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanEntry {
    /// `create`, `update` or `delete`.
    pub action: String,
    /// `instance` or `provider`.
    pub resource: String,
    /// Instance id, or provider name.
    pub id: String,
    pub name: String,
    /// Field-level changes of an update (never a secret value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldDiff>,
}

/// One changed field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldDiff {
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// What an apply would change.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    #[serde(default)]
    pub create: Vec<PlanEntry>,
    #[serde(default)]
    pub update: Vec<PlanEntry>,
    /// Deletions that will be executed (`allow_delete`).
    #[serde(default)]
    pub delete: Vec<PlanEntry>,
    /// Deletions refused because `allow_delete` was not set.
    #[serde(default)]
    pub refused: Vec<PlanEntry>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.create.is_empty()
            && self.update.is_empty()
            && self.delete.is_empty()
            && self.refused.is_empty()
    }

    /// `(create, update, delete, refused)` counts.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        (
            self.create.len(),
            self.update.len(),
            self.delete.len(),
            self.refused.len(),
        )
    }

    /// Human-readable plan.
    pub fn render_text(&self) -> String {
        let (create, update, delete, refused) = self.counts();
        let mut out =
            format!("plan: {create} create, {update} update, {delete} delete, {refused} refused\n");
        for entry in self
            .create
            .iter()
            .chain(self.update.iter())
            .chain(self.delete.iter())
            .chain(self.refused.iter())
        {
            let marker = match entry.action.as_str() {
                "create" => "+",
                "delete" => "-",
                _ => "~",
            };
            let refused = self
                .refused
                .iter()
                .any(|refused| refused.resource == entry.resource && refused.id == entry.id);
            out.push_str(&format!(
                "  {marker} {} {}{}\n",
                entry.resource,
                entry.id,
                if refused {
                    " (refused: pass --allow-delete)"
                } else {
                    ""
                }
            ));
            for field in &entry.fields {
                out.push_str(&format!(
                    "      {}: {} -> {}\n",
                    field.field,
                    field.from.as_deref().unwrap_or("(unset)"),
                    field.to.as_deref().unwrap_or("(removed)")
                ));
            }
        }
        out
    }
}

/// Outcome of an apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyReport {
    pub plan: Plan,
    /// Entries actually executed, in order.
    pub applied: Vec<PlanEntry>,
    pub applied_at: String,
    pub source: String,
}

/// Stored desired state plus the drift measured against it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredStateView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Raw document text, as applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_text: Option<String>,
    /// The same document converted to JSON for the web console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<Value>,
    pub in_sync: bool,
    pub drift: Plan,
}

/// `GET /api/desired-state/drift`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftReport {
    pub found: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub in_sync: bool,
    pub drift: Plan,
}

// ---------------------------------------------------------------------------
// Desired resolution
// ---------------------------------------------------------------------------

/// Provider identity resolution: a document names providers, the index keys
/// them by uuid. A provider missing from the index receives the id it will be
/// created with, so a single apply can create a provider and its instances.
struct ProviderResolution {
    ids: BTreeMap<String, String>,
    documents: BTreeMap<String, ProviderDocument>,
    existing: BTreeMap<String, ProviderSpec>,
}

impl ProviderResolution {
    fn resolve(db: &Db, document: &FleetDocument) -> Result<Self, GitopsError> {
        let existing: BTreeMap<String, ProviderSpec> = db
            .list_providers()?
            .into_iter()
            .map(|spec| (spec.name.clone(), spec))
            .collect();
        let documents: BTreeMap<String, ProviderDocument> = document
            .providers
            .iter()
            .map(|provider| (provider.name.clone(), provider.clone()))
            .collect();
        let mut ids = BTreeMap::new();
        for (name, spec) in &existing {
            ids.insert(name.clone(), spec.id.clone());
        }
        for name in documents.keys() {
            ids.entry(name.clone())
                .or_insert_with(|| uuid::Uuid::new_v4().to_string());
        }
        // A provider reference must point at a declared or an existing
        // provider: a typo must never silently detach the credential.
        for instance in &document.instances {
            if let Some(provider) = &instance.provider {
                if !ids.contains_key(provider) {
                    return Err(GitopsError::Invalid(format!(
                        "instance \"{}\" references unknown provider \"{provider}\"",
                        instance.id
                    )));
                }
            }
        }
        Ok(Self {
            ids,
            documents,
            existing,
        })
    }

    fn provider_document(&self, name: &str) -> Option<&ProviderDocument> {
        self.documents.get(name)
    }
}

/// One instance with every default resolved, ready to be compared or written.
#[derive(Debug, Clone)]
struct DesiredInstance {
    id: String,
    name: String,
    template: String,
    model: String,
    provider_name: Option<String>,
    provider_id: Option<String>,
    base_url: String,
    api_key_env: String,
    mem_limit_mb: u32,
    cpu_limit: f64,
    allow_lan: bool,
    notes: String,
    env: Vec<EnvDocument>,
    shares: Vec<ShareRef>,
}

fn desired_instance(
    cfg: &Config,
    db: &Db,
    document: &InstanceDocument,
    resolution: &ProviderResolution,
) -> Result<DesiredInstance, GitopsError> {
    let existing = db.get(&document.id)?;
    let template = match render::load_template(cfg, &document.template) {
        Ok(template) => Some(template),
        // `custom` is the on-disk config of an adopted instance: it has no
        // template source, but an update can still render from `data/`.
        Err(RenderError::UnknownTemplate(_))
            if document.template == "custom" && existing.is_some() =>
        {
            None
        }
        Err(err) => return Err(err.into()),
    };
    let model = document
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            template
                .as_ref()
                .and_then(|template| template.meta.default_model.clone())
        })
        .or_else(|| existing.as_ref().map(|spec| spec.model.clone()))
        .ok_or_else(|| {
            GitopsError::Invalid(format!(
                "instance \"{}\": model is required (the template declares no default)",
                document.id
            ))
        })?;
    validate_model(&model).map_err(|message| {
        GitopsError::Invalid(format!("instance \"{}\": {message}", document.id))
    })?;

    let (provider_name, provider_id, base_url, api_key_env) = match &document.provider {
        Some(name) => {
            let id = resolution.ids.get(name).cloned().ok_or_else(|| {
                GitopsError::Invalid(format!(
                    "instance \"{}\" references unknown provider \"{name}\"",
                    document.id
                ))
            })?;
            match resolution.provider_document(name) {
                Some(provider) => (
                    Some(name.clone()),
                    Some(id),
                    provider.base_url.trim().to_string(),
                    provider.api_key_env.clone(),
                ),
                None => {
                    let provider = resolution
                        .existing
                        .get(name)
                        .expect("a resolved provider is declared or exists");
                    (
                        Some(name.clone()),
                        Some(id),
                        provider.base_url.clone(),
                        provider.api_key_env.clone(),
                    )
                }
            }
        }
        None => match &existing {
            Some(spec) => (None, None, spec.base_url.clone(), spec.api_key_env.clone()),
            None => (
                None,
                None,
                DEFAULT_BASE_URL.to_string(),
                DEFAULT_API_KEY_ENV.to_string(),
            ),
        },
    };

    let name = document
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&document.id)
        .to_string();

    let mut shares: Vec<ShareRef> = document
        .shares
        .iter()
        .map(|share| ShareRef {
            name: share.name.clone(),
            mode: share.mode,
        })
        .collect();
    shares.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(DesiredInstance {
        id: document.id.clone(),
        name,
        template: document.template.clone(),
        model,
        provider_name,
        provider_id,
        base_url,
        api_key_env,
        mem_limit_mb: document.mem_limit_mb.unwrap_or(DEFAULT_MEM_LIMIT_MB),
        cpu_limit: document.cpu_limit.unwrap_or(DEFAULT_CPU_LIMIT),
        allow_lan: document.allow_lan.unwrap_or(false),
        notes: document.notes.clone().unwrap_or_default(),
        env: document.env.clone(),
        shares,
    })
}

impl DesiredInstance {
    fn into_spec(self, existing: Option<&InstanceSpec>, image: &str) -> InstanceSpec {
        let now = now_iso();
        InstanceSpec {
            id: self.id,
            name: self.name,
            template: self.template,
            model: self.model,
            base_url: self.base_url,
            api_key_env: self.api_key_env,
            provider: self.provider_id,
            channel: existing
                .map(|spec| spec.channel)
                .unwrap_or(crate::models::ChannelKind::None),
            shares: self.shares,
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            mem_limit_mb: self.mem_limit_mb,
            cpu_limit: self.cpu_limit,
            image: existing
                .map(|spec| spec.image.clone())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| image.to_string()),
            previous_image: None,
            allow_lan: self.allow_lan,
            host: existing
                .map(|spec| spec.host.clone())
                .unwrap_or_else(|| crate::hosts::LOCAL_HOST.to_string()),
            heal: existing
                .map(|spec| spec.heal)
                .unwrap_or(crate::models::HealPolicy::Off),
            unhealthy_after: existing
                .map(|spec| spec.unhealthy_after)
                .unwrap_or(crate::models::DEFAULT_UNHEALTHY_AFTER),
            max_restarts: existing
                .map(|spec| spec.max_restarts)
                .unwrap_or(crate::models::DEFAULT_MAX_RESTARTS),
            heal_backoff_s: existing
                .map(|spec| spec.heal_backoff_s)
                .unwrap_or(crate::models::DEFAULT_HEAL_BACKOFF_S),
            disk_quota_mb: existing.map(|spec| spec.disk_quota_mb).unwrap_or(0),
            disk_policy: existing
                .map(|spec| spec.disk_policy)
                .unwrap_or(crate::models::DiskPolicy::Warn),
            notes: self.notes,
            status: existing.map(|spec| spec.status).unwrap_or(Status::Created),
            last_error: existing.and_then(|spec| spec.last_error.clone()),
            created_at: existing
                .map(|spec| spec.created_at.clone())
                .unwrap_or_else(|| now.clone()),
            updated_at: now,
        }
    }
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// Compare a document with the live index.
///
/// No secret is read: provider secrets only ever appear as `secret_ref`
/// strings, and secret instance variables are compared by presence and flag.
pub fn plan(
    cfg: &Config,
    db: &Db,
    document: &FleetDocument,
    allow_delete: bool,
) -> Result<Plan, GitopsError> {
    document.validate()?;
    let resolution = ProviderResolution::resolve(db, document)?;
    let mut plan = Plan::default();

    let declared_providers: BTreeSet<&str> = document
        .providers
        .iter()
        .map(|provider| provider.name.as_str())
        .collect();
    for provider in &document.providers {
        match resolution.existing.get(&provider.name) {
            None => plan.create.push(PlanEntry {
                action: "create".to_string(),
                resource: "provider".to_string(),
                id: provider.name.clone(),
                name: provider.name.clone(),
                fields: Vec::new(),
            }),
            Some(existing) => {
                let mut fields = Vec::new();
                push_diff(&mut fields, "kind", &existing.kind, &provider.kind);
                push_diff(
                    &mut fields,
                    "base_url",
                    &existing.base_url,
                    provider.base_url.trim(),
                );
                push_diff(
                    &mut fields,
                    "api_key_env",
                    &existing.api_key_env,
                    &provider.api_key_env,
                );
                if !fields.is_empty() {
                    plan.update.push(PlanEntry {
                        action: "update".to_string(),
                        resource: "provider".to_string(),
                        id: provider.name.clone(),
                        name: provider.name.clone(),
                        fields,
                    });
                }
            }
        }
    }
    for spec in resolution.existing.values() {
        if !declared_providers.contains(spec.name.as_str()) {
            push_delete(&mut plan, allow_delete, "provider", &spec.name, &spec.name);
        }
    }

    let current = db.list()?;
    let declared_instances: BTreeSet<&str> = document
        .instances
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    for entry in &document.instances {
        let desired = desired_instance(cfg, db, entry, &resolution)?;
        match current.iter().find(|spec| spec.id == entry.id) {
            None => plan.create.push(PlanEntry {
                action: "create".to_string(),
                resource: "instance".to_string(),
                id: entry.id.clone(),
                name: desired.name.clone(),
                fields: Vec::new(),
            }),
            Some(existing) => {
                let fields = diff_instance(cfg, db, existing, &desired, &resolution)?;
                if !fields.is_empty() {
                    plan.update.push(PlanEntry {
                        action: "update".to_string(),
                        resource: "instance".to_string(),
                        id: entry.id.clone(),
                        name: desired.name.clone(),
                        fields,
                    });
                }
            }
        }
    }
    for spec in &current {
        if !declared_instances.contains(spec.id.as_str()) {
            push_delete(&mut plan, allow_delete, "instance", &spec.id, &spec.name);
        }
    }
    Ok(plan)
}

fn push_delete(plan: &mut Plan, allow_delete: bool, resource: &str, id: &str, name: &str) {
    let entry = PlanEntry {
        action: "delete".to_string(),
        resource: resource.to_string(),
        id: id.to_string(),
        name: name.to_string(),
        fields: Vec::new(),
    };
    if allow_delete {
        plan.delete.push(entry);
    } else {
        plan.refused.push(entry);
    }
}

fn push_diff(fields: &mut Vec<FieldDiff>, field: &str, from: &str, to: &str) {
    if from != to {
        fields.push(FieldDiff {
            field: field.to_string(),
            from: Some(from.to_string()),
            to: Some(to.to_string()),
        });
    }
}

/// One stored environment variable, as compared by the planner.
struct CurrentEnv {
    secret: bool,
    value: Option<String>,
}

/// Environment of an instance without decrypting a secret it does not need to.
fn current_env(
    cfg: &Config,
    db: &Db,
    id: &str,
) -> Result<BTreeMap<String, CurrentEnv>, GitopsError> {
    let records = db.list_instance_env(id)?;
    if !records.is_empty() {
        return Ok(records
            .into_iter()
            .map(|record| {
                (
                    record.key,
                    CurrentEnv {
                        secret: record.secret,
                        value: if record.secret { None } else { record.value },
                    },
                )
            })
            .collect());
    }
    // Legacy stores (sealed blob, plaintext secrets.env): every entry is a
    // secret, so only its presence is compared.
    match envs::load(cfg, db, id) {
        Ok(entries) => Ok(entries
            .into_iter()
            .map(|entry| {
                (
                    entry.key,
                    CurrentEnv {
                        secret: entry.secret,
                        value: (!entry.secret).then_some(entry.value),
                    },
                )
            })
            .collect()),
        Err(_) => Ok(BTreeMap::new()),
    }
}

fn diff_instance(
    cfg: &Config,
    db: &Db,
    existing: &InstanceSpec,
    desired: &DesiredInstance,
    resolution: &ProviderResolution,
) -> Result<Vec<FieldDiff>, GitopsError> {
    let mut fields = Vec::new();
    push_diff(&mut fields, "name", &existing.name, &desired.name);
    push_diff(
        &mut fields,
        "template",
        &existing.template,
        &desired.template,
    );
    push_diff(&mut fields, "model", &existing.model, &desired.model);
    push_diff(
        &mut fields,
        "base_url",
        &existing.base_url,
        &desired.base_url,
    );
    push_diff(
        &mut fields,
        "api_key_env",
        &existing.api_key_env,
        &desired.api_key_env,
    );

    let current_provider = existing
        .provider
        .as_deref()
        .map(|id| provider_name_of(id, resolution))
        .or_else(|| existing.provider.clone());
    if current_provider != desired.provider_name {
        fields.push(FieldDiff {
            field: "provider".to_string(),
            from: current_provider,
            to: desired.provider_name.clone(),
        });
    }

    if existing.mem_limit_mb != desired.mem_limit_mb {
        fields.push(FieldDiff {
            field: "mem_limit_mb".to_string(),
            from: Some(existing.mem_limit_mb.to_string()),
            to: Some(desired.mem_limit_mb.to_string()),
        });
    }
    if (existing.cpu_limit - desired.cpu_limit).abs() > f64::EPSILON {
        fields.push(FieldDiff {
            field: "cpu_limit".to_string(),
            from: Some(existing.cpu_limit.to_string()),
            to: Some(desired.cpu_limit.to_string()),
        });
    }
    if existing.allow_lan != desired.allow_lan {
        fields.push(FieldDiff {
            field: "allow_lan".to_string(),
            from: Some(existing.allow_lan.to_string()),
            to: Some(desired.allow_lan.to_string()),
        });
    }
    push_diff(&mut fields, "notes", &existing.notes, &desired.notes);
    diff_env(
        &mut fields,
        &current_env(cfg, db, &existing.id)?,
        &desired.env,
    );
    diff_shares(&mut fields, &existing.shares, &desired.shares);
    Ok(fields)
}

fn provider_name_of(provider_id: &str, resolution: &ProviderResolution) -> String {
    resolution
        .existing
        .values()
        .find(|spec| spec.id == provider_id)
        .map(|spec| spec.name.clone())
        .unwrap_or_else(|| provider_id.to_string())
}

fn diff_env(
    fields: &mut Vec<FieldDiff>,
    current: &BTreeMap<String, CurrentEnv>,
    desired: &[EnvDocument],
) {
    let desired: BTreeMap<&str, &EnvDocument> =
        desired.iter().map(|env| (env.key.as_str(), env)).collect();
    let keys: BTreeSet<&str> = current
        .keys()
        .map(String::as_str)
        .chain(desired.keys().copied())
        .collect();
    for key in keys {
        let field = format!("env.{key}");
        match (current.get(key), desired.get(key)) {
            (None, Some(env)) => fields.push(FieldDiff {
                field,
                from: None,
                to: Some(display_desired_env(env)),
            }),
            (Some(env), None) => fields.push(FieldDiff {
                field,
                from: Some(display_current_env(env)),
                to: None,
            }),
            (Some(current), Some(desired_env)) => {
                let desired_secret = desired_env.secret_ref.is_some();
                let changed = current.secret != desired_secret
                    || (!current.secret
                        && current.value.as_deref() != desired_env.value.as_deref());
                if changed {
                    fields.push(FieldDiff {
                        field,
                        from: Some(display_current_env(current)),
                        to: Some(display_desired_env(desired_env)),
                    });
                }
                // A secret value cannot be compared without resolving it, and
                // a plan never resolves secrets: presence and flag are enough.
            }
            (None, None) => {}
        }
    }
}

fn display_current_env(env: &CurrentEnv) -> String {
    if env.secret {
        "<secret>".to_string()
    } else {
        env.value.clone().unwrap_or_default()
    }
}

fn display_desired_env(env: &EnvDocument) -> String {
    match (&env.value, &env.secret_ref) {
        (Some(value), _) => value.clone(),
        (None, Some(reference)) => format!("<secret: {reference}>"),
        (None, None) => String::new(),
    }
}

fn diff_shares(fields: &mut Vec<FieldDiff>, current: &[ShareRef], desired: &[ShareRef]) {
    let render = |shares: &[ShareRef]| {
        shares
            .iter()
            .map(|share| format!("{}:{}", share.name, share.mode.as_str()))
            .collect::<Vec<_>>()
            .join(",")
    };
    let from = render(current);
    let to = render(desired);
    if from != to {
        fields.push(FieldDiff {
            field: "shares".to_string(),
            from: Some(from),
            to: Some(to),
        });
    }
}

// ---------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------

/// Apply a document: creates, updates, then (with `allow_delete`) deletes.
///
/// The document is stored in `desired_state` only after every mutation
/// succeeded, so a failed apply leaves the previous drift baseline in place.
pub async fn apply(
    state: &AppState,
    document: &FleetDocument,
    raw: &str,
    source: &str,
    allow_delete: bool,
) -> Result<ApplyReport, GitopsError> {
    let plan = plan(&state.cfg, &state.db, document, allow_delete)?;
    let resolution = ProviderResolution::resolve(&state.db, document)?;
    check_provider_deletions(state, &plan, &resolution)?;

    // Resolve every secret, and check every share, before the first mutation:
    // a document that cannot be applied in full must not leave a half-created
    // fleet behind.
    let mut provider_secrets: BTreeMap<String, String> = BTreeMap::new();
    for provider in &document.providers {
        match (
            &provider.secret_ref,
            resolution.existing.get(&provider.name),
        ) {
            (Some(reference), _) => {
                provider_secrets.insert(provider.name.clone(), resolve_secret_ref(reference)?);
            }
            (None, None) => {
                return Err(GitopsError::Invalid(format!(
                    "provider \"{}\" must declare a secret_ref",
                    provider.name
                )))
            }
            (None, Some(_)) => {}
        }
    }
    let mut instance_env: BTreeMap<String, Vec<EnvWrite>> = BTreeMap::new();
    let needs_instance = |id: &str| {
        plan.create
            .iter()
            .chain(plan.update.iter())
            .any(|entry| entry.resource == "instance" && entry.id == id)
    };
    for entry in &document.instances {
        if !needs_instance(&entry.id) {
            continue;
        }
        let desired = desired_instance(&state.cfg, &state.db, entry, &resolution)?;
        check_shares(state, &desired)?;
        let entries = resolved_env(&desired)?;
        envs::check_storable(&state.cfg, &entries)?;
        instance_env.insert(entry.id.clone(), entries);
    }

    let mut applied = Vec::new();

    // Providers first: an instance created below may reference a provider
    // created by the same apply.
    for provider in &document.providers {
        let secret = provider_secrets.get(&provider.name).map(String::as_str);
        match resolution.existing.get(&provider.name) {
            None => {
                let id = resolution.ids.get(&provider.name).cloned().ok_or_else(|| {
                    GitopsError::Operation(format!(
                        "provider \"{}\" has no resolved id",
                        provider.name
                    ))
                })?;
                let secret = secret.expect("a created provider resolved its secret");
                create_provider(state, provider, &id, secret)?;
                applied.push(PlanEntry {
                    action: "create".to_string(),
                    resource: "provider".to_string(),
                    id: provider.name.clone(),
                    name: provider.name.clone(),
                    fields: Vec::new(),
                });
            }
            Some(existing) => {
                if update_provider(state, existing, provider, secret).await? {
                    applied.push(PlanEntry {
                        action: "update".to_string(),
                        resource: "provider".to_string(),
                        id: provider.name.clone(),
                        name: provider.name.clone(),
                        fields: Vec::new(),
                    });
                }
            }
        }
    }

    for entry in &document.instances {
        let desired = desired_instance(&state.cfg, &state.db, entry, &resolution)?;
        match state.db.get(&entry.id)? {
            None => {
                let name = desired.name.clone();
                let env = instance_env
                    .remove(&entry.id)
                    .expect("a created instance was preflighted");
                create_instance(state, desired, env).await?;
                applied.push(PlanEntry {
                    action: "create".to_string(),
                    resource: "instance".to_string(),
                    id: entry.id.clone(),
                    name,
                    fields: Vec::new(),
                });
            }
            Some(existing) => {
                let fields =
                    diff_instance(&state.cfg, &state.db, &existing, &desired, &resolution)?;
                if !fields.is_empty() {
                    let env = instance_env
                        .remove(&entry.id)
                        .expect("an updated instance was preflighted");
                    update_instance(state, existing, desired, env).await?;
                    applied.push(PlanEntry {
                        action: "update".to_string(),
                        resource: "instance".to_string(),
                        id: entry.id.clone(),
                        name: entry.id.clone(),
                        fields,
                    });
                }
            }
        }
    }

    // Deletions run last: a document can remove an instance and the provider
    // it used in the same apply.
    for entry in &plan.delete {
        if entry.resource == "instance" {
            if let Some(spec) = state.db.get(&entry.id)? {
                delete_instance(state, &spec).await?;
                applied.push(entry.clone());
            }
        }
    }
    for entry in &plan.delete {
        if entry.resource == "provider" {
            if let Some(spec) = resolution.existing.get(&entry.id) {
                state.db.delete_provider(&spec.id)?;
                applied.push(entry.clone());
            }
        }
    }

    let applied_at = now_iso();
    let source = normalize_source(source);
    state.db.put_desired_state(raw, &applied_at, &source)?;
    Ok(ApplyReport {
        plan,
        applied,
        applied_at,
        source,
    })
}

fn normalize_source(source: &str) -> String {
    let source = source.trim();
    if source.is_empty() {
        "gitops".to_string()
    } else {
        source.chars().take(SOURCE_MAX).collect()
    }
}

/// Refuse a provider deletion that would orphan an instance kept by the apply.
fn check_provider_deletions(
    state: &AppState,
    plan: &Plan,
    resolution: &ProviderResolution,
) -> Result<(), GitopsError> {
    let deleted_instances: BTreeSet<&str> = plan
        .delete
        .iter()
        .filter(|entry| entry.resource == "instance")
        .map(|entry| entry.id.as_str())
        .collect();
    for entry in plan
        .delete
        .iter()
        .filter(|entry| entry.resource == "provider")
    {
        let Some(spec) = resolution.existing.get(&entry.id) else {
            continue;
        };
        let attached = state.db.provider_instances(&spec.id)?;
        let kept: Vec<String> = attached
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| !deleted_instances.contains(id.as_str()))
            .collect();
        if !kept.is_empty() {
            return Err(GitopsError::Conflict(format!(
                "provider \"{}\" is still used by instance(s) {}; declare the provider or \
                 delete the instances in the same document",
                entry.id,
                kept.join(", ")
            )));
        }
    }
    Ok(())
}

fn create_provider(
    state: &AppState,
    provider: &ProviderDocument,
    id: &str,
    secret: &str,
) -> Result<(), GitopsError> {
    providers::validate_secret(secret).map_err(|err| GitopsError::Operation(err.to_string()))?;
    let sealed = providers::seal(&state.cfg, secret)
        .map_err(|err| GitopsError::Operation(err.to_string()))?;

    let now = now_iso();
    let spec = ProviderSpec {
        id: id.to_string(),
        name: provider.name.clone(),
        kind: provider.kind.clone(),
        base_url: provider.base_url.trim().to_string(),
        api_key_env: provider.api_key_env.clone(),
        created_at: now.clone(),
        updated_at: now,
    };
    state
        .db
        .insert_provider(&spec, &sealed.nonce, &sealed.ciphertext)
        .map_err(|err| GitopsError::Operation(err.to_string()))?;
    Ok(())
}

/// Returns `true` when a metadata or secret change was written.
async fn update_provider(
    state: &AppState,
    existing: &ProviderSpec,
    provider: &ProviderDocument,
    secret: Option<&str>,
) -> Result<bool, GitopsError> {
    let mut spec = existing.clone();
    let mut changed = false;
    if spec.kind != provider.kind {
        spec.kind = provider.kind.clone();
        changed = true;
    }
    if spec.base_url != provider.base_url.trim() {
        spec.base_url = provider.base_url.trim().to_string();
        changed = true;
    }
    if spec.api_key_env != provider.api_key_env {
        spec.api_key_env = provider.api_key_env.clone();
        changed = true;
    }

    let mut rotated = false;
    if let Some(secret) = secret {
        providers::validate_secret(secret)
            .map_err(|err| GitopsError::Operation(err.to_string()))?;
        let record = state.db.provider(&spec.id)?.ok_or_else(|| {
            GitopsError::Operation(format!("provider \"{}\" disappeared", spec.name))
        })?;
        let current = providers::secret_of(&state.cfg, &record)
            .map_err(|err| GitopsError::Operation(err.to_string()))?;
        if current != secret {
            let sealed = providers::seal(&state.cfg, secret)
                .map_err(|err| GitopsError::Operation(err.to_string()))?;
            spec.updated_at = now_iso();
            state.db.update_provider(
                &spec,
                Some((sealed.nonce.as_slice(), sealed.ciphertext.as_slice())),
            )?;
            rotated = true;
            changed = true;
        }
    }
    if changed && !rotated {
        spec.updated_at = now_iso();
        state.db.update_provider(&spec, None)?;
    }
    if rotated {
        restart_attached(state, &spec.id).await?;
    }
    Ok(changed)
}

/// Recreate every instance attached to a rotated provider.
async fn restart_attached(state: &AppState, provider_id: &str) -> Result<(), GitopsError> {
    for (id, _) in state.db.provider_instances(provider_id)? {
        if let Some(mut spec) = state.db.get(&id)? {
            crate::api::up_and_record(state, &mut spec).await?;
        }
    }
    Ok(())
}

fn resolved_env(desired: &DesiredInstance) -> Result<Vec<EnvWrite>, GitopsError> {
    let mut entries = Vec::with_capacity(desired.env.len());
    for env in &desired.env {
        match (&env.value, &env.secret_ref) {
            (Some(value), None) => entries.push(EnvWrite {
                key: env.key.clone(),
                value: value.clone(),
                secret: false,
            }),
            (None, Some(reference)) => entries.push(EnvWrite {
                key: env.key.clone(),
                value: resolve_secret_ref(reference)?,
                secret: true,
            }),
            _ => {
                return Err(GitopsError::Invalid(format!(
                    "environment variable \"{}\" needs exactly one of value and secret_ref",
                    env.key
                )))
            }
        }
    }
    Ok(entries)
}

fn check_shares(state: &AppState, desired: &DesiredInstance) -> Result<(), GitopsError> {
    for share in &desired.shares {
        if !state.db.share_exists(&share.name)? {
            return Err(GitopsError::Invalid(format!(
                "instance \"{}\" references unknown share \"{}\": create it before applying",
                desired.id, share.name
            )));
        }
    }
    Ok(())
}

async fn create_instance(
    state: &AppState,
    desired: DesiredInstance,
    entries: Vec<EnvWrite>,
) -> Result<(), GitopsError> {
    let spec = desired.into_spec(None, &state.cfg.image);
    crate::api::instances::enforce_caps(state, None, &spec)?;
    crate::api::instances::check_egress_policy(state, &spec).await?;
    crate::api::instances::ensure_lan_network(state, &spec).await?;

    let template = render::load_template(&state.cfg, &spec.template)?;
    let mut env_keys: Vec<String> = entries.iter().map(|entry| entry.key.clone()).collect();
    crate::api::push_provider_key(&mut env_keys, &spec);
    let rendered = render::render_instance(&state.cfg, &spec, &template, &env_keys)?;

    state.db.insert_instance(&spec)?;
    if let Err(err) = envs::replace(&state.cfg, &state.db, &spec.id, &entries) {
        let _ = state.db.delete_instance(&spec.id);
        return Err(err.into());
    }
    if let Err(err) = store::write_instance(&state.cfg, &spec, &rendered, true) {
        let _ = state.db.delete_instance(&spec.id);
        let _ = std::fs::remove_dir_all(state.cfg.instance_dir(&spec.id));
        return Err(GitopsError::Operation(format!(
            "failed to write instance files: {err}"
        )));
    }
    let mut spec = state.db.get(&spec.id)?.unwrap_or(spec);
    crate::api::up_and_record(state, &mut spec).await?;
    Ok(())
}

async fn update_instance(
    state: &AppState,
    existing: InstanceSpec,
    desired: DesiredInstance,
    entries: Vec<EnvWrite>,
) -> Result<(), GitopsError> {
    let mut spec = desired.into_spec(Some(&existing), &state.cfg.image);
    spec.status = Status::Created;
    spec.last_error = None;
    crate::api::instances::enforce_caps(state, Some(&spec.id), &spec)?;
    crate::api::instances::check_egress_policy(state, &spec).await?;
    crate::api::instances::ensure_lan_network(state, &spec).await?;

    let mut env_keys: Vec<String> = entries.iter().map(|entry| entry.key.clone()).collect();
    crate::api::push_provider_key(&mut env_keys, &spec);
    let rendered = crate::api::instances::render_for_update(state, &spec, &env_keys)?;
    envs::replace(&state.cfg, &state.db, &spec.id, &entries)?;
    store::write_instance(&state.cfg, &spec, &rendered, false)
        .map_err(|err| GitopsError::Operation(format!("failed to write instance files: {err}")))?;
    state.db.update_instance(&spec)?;
    crate::api::up_and_record(state, &mut spec).await?;
    Ok(())
}

async fn delete_instance(state: &AppState, spec: &InstanceSpec) -> Result<(), GitopsError> {
    if let Err(err) = state.driver.down(spec).await {
        let detail = err.detail.clone();
        let _ = state.db.set_status(&spec.id, Status::Error, Some(&detail));
        state
            .metrics
            .record_compose_action("down", crate::metrics::RESULT_ERROR);
        return Err(err.into());
    }
    state
        .metrics
        .record_compose_action("down", crate::metrics::RESULT_OK);
    state.db.delete_instance(&spec.id)?;
    store::remove_instance(&state.cfg, &spec.id, false)
        .map_err(|err| GitopsError::Operation(format!("failed to remove instance files: {err}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Drift
// ---------------------------------------------------------------------------

/// Recompute the plan from the stored document, with deletions refused.
pub fn drift(cfg: &Config, db: &Db) -> Result<DriftReport, GitopsError> {
    let Some(stored) = db.desired_state()? else {
        return Ok(DriftReport {
            found: false,
            applied_at: None,
            source: None,
            in_sync: true,
            drift: Plan::default(),
        });
    };
    let document = FleetDocument::parse(&stored.document)?;
    let plan = plan(cfg, db, &document, false)?;
    Ok(DriftReport {
        found: true,
        applied_at: Some(stored.applied_at),
        source: Some(stored.source),
        in_sync: plan.is_empty(),
        drift: plan,
    })
}

/// Read the stored document, its JSON view and the drift in one call.
pub fn desired_state(cfg: &Config, db: &Db) -> Result<DesiredStateView, GitopsError> {
    let Some(stored) = db.desired_state()? else {
        return Ok(DesiredStateView {
            applied_at: None,
            source: None,
            document_text: None,
            document: None,
            in_sync: true,
            drift: Plan::default(),
        });
    };
    let report = drift(cfg, db)?;
    Ok(DesiredStateView {
        applied_at: Some(stored.applied_at),
        source: Some(stored.source),
        document: Some(document_value(&stored.document)?),
        document_text: Some(stored.document),
        in_sync: report.in_sync,
        drift: report.drift,
    })
}

/// Convert a stored document into JSON for the web console.
pub fn document_value(text: &str) -> Result<Value, GitopsError> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Ok(value);
    }
    let yaml: serde_yaml::Value = serde_yaml::from_str(text)
        .map_err(|err| GitopsError::Invalid(format!("invalid YAML: {err}")))?;
    serde_json::to_value(yaml)
        .map_err(|err| GitopsError::Invalid(format!("cannot convert the document: {err}")))
}
