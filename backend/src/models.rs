//! Domain types, API payloads and input validation.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const INSTANCE_ID_MIN: usize = 2;
pub const INSTANCE_ID_MAX: usize = 32;
pub const SHARE_NAME_MIN: usize = 2;
pub const SHARE_NAME_MAX: usize = 32;
pub const MEM_LIMIT_MIN_MB: u32 = 256;
pub const MEM_LIMIT_MAX_MB: u32 = 8192;
pub const DEFAULT_MEM_LIMIT_MB: u32 = 1536;
pub const CPU_LIMIT_MIN: f64 = 0.1;
pub const CPU_LIMIT_MAX: f64 = 8.0;
pub const DEFAULT_CPU_LIMIT: f64 = 1.0;
/// Written to `last_error` while the container health check keeps failing.
pub const HEALTH_UNHEALTHY_MESSAGE: &str =
    "container health check failed: the Hermes gateway is not answering on /opt/data/gateway.sock";
/// Provider names follow the same slug grammar as instance and share names.
pub const PROVIDER_NAME_MIN: usize = 2;
pub const PROVIDER_NAME_MAX: usize = 63;
/// Provider kinds understood by this build (metadata only: every kind is
/// probed through its OpenAI-compatible `/models` endpoint).
pub const PROVIDER_KINDS: [&str; 3] = ["openai-compatible", "anthropic", "google"];
/// Longest provider secret accepted by the API (ASCII bytes, single line).
pub const PROVIDER_SECRET_MAX_BYTES: usize = 8 * 1024;
pub const DEFAULT_TEMPLATE: &str = "guest";
pub const DEFAULT_API_KEY_ENV: &str = "HERMES_GATE_PROFILE_TOKEN";
pub const GATE_TOKEN_ENV: &str = "HERMES_GATE_PROFILE_TOKEN";
/// Longest environment variable name accepted by the API (`^[A-Z_][A-Z0-9_]{0,63}$`).
pub const ENV_KEY_MAX: usize = 64;
/// Largest value accepted for one environment variable (8 KiB).
pub const ENV_VALUE_MAX_BYTES: usize = 8 * 1024;
/// Variable names the container contract owns; an instance env never sets them.
pub const RESERVED_ENV_KEYS: [&str; 4] = ["HERMES_UID", "HERMES_GID", "PATH", "HOME"];
/// Prefix reserved for the control plane itself.
pub const RESERVED_ENV_PREFIX: &str = "FLEETT_";

/// Messaging platform of an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Telegram,
    Discord,
    None,
}

impl ChannelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Discord => "discord",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "telegram" => Some(Self::Telegram),
            "discord" => Some(Self::Discord),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// Hermes `platforms.<name>` key, when the instance has a platform.
    pub fn platform(self) -> Option<&'static str> {
        match self {
            Self::Telegram => Some("telegram"),
            Self::Discord => Some("discord"),
            Self::None => None,
        }
    }

    /// Environment variable holding the bot token for this platform.
    pub fn token_env(self) -> Option<&'static str> {
        match self {
            Self::Telegram => Some("TELEGRAM_BOT_TOKEN"),
            Self::Discord => Some("DISCORD_BOT_TOKEN"),
            Self::None => None,
        }
    }

    pub fn all_platform_token_envs() -> [&'static str; 2] {
        ["TELEGRAM_BOT_TOKEN", "DISCORD_BOT_TOKEN"]
    }
}

impl std::fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Mount mode of a share inside an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareMode {
    Rw,
    Ro,
}

impl ShareMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rw => "rw",
            Self::Ro => "ro",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "rw" | "readwrite" | "read-write" => Some(Self::Rw),
            "ro" | "readonly" | "read-only" => Some(Self::Ro),
            _ => None,
        }
    }
}

impl std::fmt::Display for ShareMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A share name plus its mount mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareRef {
    pub name: String,
    pub mode: ShareMode,
}

/// Lifecycle status of an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Created,
    Running,
    Stopped,
    Error,
    Unknown,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Error => "error",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "created" => Some(Self::Created),
            "running" => Some(Self::Running),
            "stopped" => Some(Self::Stopped),
            "error" => Some(Self::Error),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Map a `docker compose ps` state string onto a fleet status.
    pub fn from_compose_state(state: &str) -> Self {
        match state.trim().to_ascii_lowercase().as_str() {
            "running" | "restarting" => Self::Running,
            "exited" | "stopped" | "created" | "paused" | "removing" => Self::Stopped,
            "dead" => Self::Error,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Container health, as reported by the Docker health check.
///
/// `Unknown` covers both "no container" and "no health check configured"; it is
/// exposed as `"none"` in the API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Unhealthy,
    Starting,
    #[default]
    Unknown,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::Starting => "starting",
            Self::Unknown => "none",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "healthy" => Self::Healthy,
            "unhealthy" => Self::Unhealthy,
            "starting" => Self::Starting,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for Health {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything the control plane knows about one agent.
///
/// This is the database row plus the share links; the compose file and the
/// instance directory remain the source of truth for the container itself.
#[derive(Debug, Clone)]
pub struct InstanceSpec {
    pub id: String,
    pub name: String,
    pub template: String,
    pub model: String,
    pub base_url: String,
    pub api_key_env: String,
    /// Reusable provider credential this instance draws its API key from
    /// (`None` keeps the historical per-instance token).
    pub provider: Option<String>,
    pub channel: ChannelKind,
    pub shares: Vec<ShareRef>,
    pub mem_limit_mb: u32,
    pub cpu_limit: f64,
    /// Container image the instance runs (`services.agent.image` of its
    /// compose file). Empty only for instances adopted from a compose file
    /// that declares no image, which the renderer then fills from
    /// `FLEETT_IMAGE`.
    pub image: String,
    /// Image the instance ran before its last successful upgrade. Only ever
    /// written as the `fleett.previous_image` compose label; the compose file
    /// remains the source of truth.
    pub previous_image: Option<String>,
    /// Join the unrestricted `<network>-lan` network as well (LAN access).
    pub allow_lan: bool,
    pub notes: String,
    pub status: Status,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl InstanceSpec {
    pub fn share(&self, name: &str) -> Option<&ShareRef> {
        self.shares.iter().find(|share| share.name == name)
    }
}

/// A reusable provider credential (issue #25).
///
/// The secret is never part of this struct: it lives encrypted in its own
/// columns and is only decrypted by the compose driver (injection) and by the
/// connectivity test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSpec {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub api_key_env: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One provider, as returned by `GET /api/providers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub api_key_env: String,
    /// Number of instances that reference this provider.
    pub instances_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

/// `GET /api/providers/{id}`: the summary plus the instances that use it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderDetail {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub api_key_env: String,
    pub instances_count: usize,
    pub created_at: String,
    pub updated_at: String,
    pub instances: Vec<ProviderInstanceRef>,
}

/// One instance attached to a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderInstanceRef {
    pub id: String,
    pub name: String,
}

/// Outcome of one instance in a rotation report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderRestart {
    pub id: String,
    /// `healthy` when the instance came back, `failed` otherwise.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response of `PATCH /api/providers/{id}` when the secret was rotated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderRotateResponse {
    pub provider: ProviderSummary,
    pub restarted: Vec<ProviderRestart>,
}

/// Response of `POST /api/providers/{id}/test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderTestResponse {
    pub ok: bool,
    /// HTTP status of `${base_url}/models`, `0` when no answer arrived.
    pub status: u16,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `POST /api/providers`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateProviderRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
}

/// `PATCH /api/providers/{id}`. A missing field is left untouched; `secret`
/// rotates the credential and restarts every attached instance.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PatchProviderRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
}

/// Container runtime details returned by the driver.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ContainerInfo {
    pub id: Option<String>,
    pub image: Option<String>,
    pub uptime_s: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstanceSummary {
    pub id: String,
    pub name: String,
    pub template: String,
    pub model: String,
    pub image: String,
    pub channel: String,
    pub status: String,
    pub health: String,
    pub cpu_limit: f64,
    pub allow_lan: bool,
    /// Provider credential in use, when the instance has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub shares: Vec<String>,
    pub last_error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstanceDetail {
    pub id: String,
    pub name: String,
    pub template: String,
    pub model: String,
    pub image: String,
    pub base_url: String,
    pub api_key_env: String,
    /// Provider credential in use (`null`/absent for a legacy instance).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub channel: String,
    pub status: String,
    pub health: String,
    pub shares: Vec<ShareRef>,
    pub mem_limit_mb: u32,
    pub cpu_limit: f64,
    pub allow_lan: bool,
    /// Operator-facing security warnings (egress firewall, LAN access).
    pub warnings: Vec<String>,
    pub notes: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_error: Option<String>,
    pub data_dir: String,
    /// Environment variables of the instance. A secret entry never carries a
    /// `value`; a non-secret one always does.
    pub env: Vec<EnvEntry>,
    pub container: Option<ContainerInfo>,
    pub compose_yaml: String,
}

/// One instance of `GET /api/usage`.
#[derive(Debug, Clone, Serialize)]
pub struct UsageEntry {
    pub id: String,
    pub status: String,
    /// Percent of one CPU core, as reported by `docker stats`.
    pub cpu_pct: f64,
    pub mem_used_bytes: u64,
    pub mem_limit_bytes: u64,
    pub disk_bytes: u64,
}

/// Fleet-wide sums of [`UsageEntry`].
#[derive(Debug, Clone, Serialize)]
pub struct UsageTotals {
    pub cpu_pct: f64,
    pub mem_used_bytes: u64,
    pub mem_limit_bytes: u64,
    pub disk_bytes: u64,
}

/// Configured fleet caps (`FLEETT_MAX_TOTAL_CPU`, `FLEETT_MAX_TOTAL_MEM_MB`).
#[derive(Debug, Clone, Serialize)]
pub struct UsageCaps {
    pub max_cpu: f64,
    pub max_mem_mb: u64,
}

/// Body of `GET /api/usage`.
#[derive(Debug, Clone, Serialize)]
pub struct UsageResponse {
    pub instances: Vec<UsageEntry>,
    pub totals: UsageTotals,
    pub caps: UsageCaps,
}

/// Parallel upgrades allowed by one job.
pub const UPGRADE_PARALLEL_MAX: usize = 16;
/// Default per-instance health timeout of an upgrade.
pub const UPGRADE_TIMEOUT_DEFAULT_S: u64 = 180;
/// Upper bound of the per-instance health timeout.
pub const UPGRADE_TIMEOUT_MAX_S: u64 = 3600;

/// Body of `POST /api/upgrade`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpgradeRequest {
    /// Image tag every instance is moved to (required).
    #[serde(default)]
    pub image: Option<String>,
    /// Instances upgraded at once (default `1`, i.e. sequential).
    #[serde(default)]
    pub parallel: Option<usize>,
    /// Only list the planned changes, never write anything.
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// Per-instance health timeout, in seconds (default `180`).
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// State of one instance inside an upgrade job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeStatus {
    /// Not started yet (or planned, for a dry run).
    Pending,
    Upgrading,
    /// Running the new image and reported healthy.
    Healthy,
    /// The new image did not become healthy; the old one was restored.
    RolledBack,
    /// The upgrade failed without a rollback (for example a failed pull).
    Failed,
}

impl UpgradeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Upgrading => "upgrading",
            Self::Healthy => "healthy",
            Self::RolledBack => "rolled_back",
            Self::Failed => "failed",
        }
    }

    /// `true` when the instance no longer runs the target image.
    pub fn needs_attention(self) -> bool {
        matches!(self, Self::RolledBack | Self::Failed)
    }
}

impl std::fmt::Display for UpgradeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-instance outcome of an upgrade job.
#[derive(Debug, Clone, Serialize)]
pub struct UpgradeResult {
    pub id: String,
    /// Image the instance ran when the job started.
    pub from: Option<String>,
    /// Image the job targets.
    pub to: String,
    pub status: UpgradeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `POST /api/upgrade` and `GET /api/upgrade` payload.
#[derive(Debug, Clone, Serialize)]
pub struct UpgradeJob {
    pub id: String,
    pub image: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub dry_run: bool,
    pub parallel: usize,
    pub timeout_s: u64,
    pub results: Vec<UpgradeResult>,
}

/// One local Docker image, as reported by `GET /api/images`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageInfo {
    /// `repository:tag`, as instances reference it.
    pub tag: String,
    /// Short image id (`sha256:…`).
    pub id: String,
    /// Human size (`1.2GB`), verbatim from Docker.
    pub size: String,
    /// Creation date, verbatim from Docker.
    pub created: String,
    /// Ids of the instances whose compose file uses this tag.
    pub instances: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TemplateInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub default_model: String,
    pub channel: String,
    /// Environment variables the template expects, so the console can propose
    /// them instead of making the operator guess (allowlists, mostly).
    pub env: Vec<EnvSuggestion>,
}

/// One environment variable a template suggests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvSuggestion {
    pub key: String,
    #[serde(default)]
    pub description: String,
    /// `true` when the value must be stored encrypted (default `false`).
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShareInstance {
    pub id: String,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShareInfo {
    pub name: String,
    pub path: String,
    pub instances: Vec<ShareInstance>,
}

/// Optional `template.yaml` next to a custom `config.yaml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TemplateMeta {
    pub name: Option<String>,
    pub description: Option<String>,
    pub default_model: Option<String>,
    pub channel: Option<String>,
    /// Environment variables the template suggests.
    #[serde(default)]
    pub env: Option<Vec<EnvSuggestion>>,
}

/// One environment variable of an instance, as returned by the API.
///
/// `value` is only set for a non-secret entry: a secret value never leaves the
/// control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvEntry {
    pub key: String,
    pub secret: bool,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// One environment variable supplied by a request.
///
/// `value: null` deletes the key on `PATCH` and keeps the stored secret value
/// on `PUT`; an empty string is a real, kept value.
#[derive(Debug, Clone, Deserialize)]
pub struct EnvVarInput {
    pub key: String,
    #[serde(default)]
    pub value: Option<String>,
    /// Required by `POST /api/instances` and `PUT .../env`; optional on
    /// `PATCH`, where an existing key keeps its previous flag.
    #[serde(default)]
    pub secret: Option<bool>,
}

/// The `env` field of a request, in either accepted shape.
///
/// The canonical shape is a list of [`EnvVarInput`]. Releases up to v0.4.0
/// accepted an object of `"KEY": "value"` pairs on create; it is still read
/// for compatibility and every entry of it is stored as a secret, as v0.4.0
/// did.
#[derive(Debug, Clone)]
pub enum EnvInput {
    /// `[{"key": "…", "value": "…", "secret": false}, …]` (canonical).
    List(Vec<EnvVarInput>),
    /// `{"KEY": "value"}` (deprecated v0.4.0 shape; every entry is a secret).
    Map(BTreeMap<String, String>),
}

/// Why a JSON value could not be read as an [`EnvInput`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvShapeError {
    /// JSON type that was received (`sequence`, `map`, `string`, …).
    pub received: &'static str,
    /// Human-readable reason (the offending entry, for example).
    pub detail: String,
}

impl std::fmt::Display for EnvShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for EnvShapeError {}

impl EnvInput {
    /// Parse either accepted shape from a raw JSON value.
    ///
    /// The error carries the JSON type that was received so the API can answer
    /// `400 invalid_env` with both accepted shapes instead of axum's generic
    /// deserialization text.
    pub fn from_json_value(value: &serde_json::Value) -> Result<Self, EnvShapeError> {
        match value {
            serde_json::Value::Array(_) => serde_json::from_value(value.clone())
                .map(Self::List)
                .map_err(|err| EnvShapeError {
                    received: "sequence",
                    detail: err.to_string(),
                }),
            serde_json::Value::Object(entries) => {
                let mut map = BTreeMap::new();
                for (key, value) in entries {
                    match value {
                        serde_json::Value::String(value) => {
                            map.insert(key.clone(), value.clone());
                        }
                        other => {
                            return Err(EnvShapeError {
                                received: "map",
                                detail: format!(
                                    "env[\"{key}\"] must be a string, got {}",
                                    json_type(other)
                                ),
                            })
                        }
                    }
                }
                Ok(Self::Map(map))
            }
            other => Err(EnvShapeError {
                received: json_type(other),
                detail: "env must be a list of {key,value,secret}".to_string(),
            }),
        }
    }

    /// Canonical list of this request, consuming it.
    ///
    /// The deprecated object form logs one warning per request. Every entry of
    /// it is a secret: that is what the v0.4.0 create path did.
    pub fn into_vars(self) -> Vec<EnvVarInput> {
        match self {
            Self::List(vars) => vars,
            Self::Map(entries) => {
                tracing::warn!(
                    deprecated = "env_map",
                    keys = entries.len(),
                    "env: the object form is deprecated; send a list of {{key,value,secret}}"
                );
                entries
                    .into_iter()
                    .map(|(key, value)| EnvVarInput {
                        key,
                        value: Some(value),
                        secret: Some(true),
                    })
                    .collect()
            }
        }
    }
}

impl<'de> Deserialize<'de> for EnvInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::from_json_value(&value).map_err(serde::de::Error::custom)
    }
}

/// JSON type name used by [`EnvShapeError`].
fn json_type(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "sequence",
        serde_json::Value::Object(_) => "map",
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelRequest {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateInstanceRequest {
    /// Validated by the handler so that a missing id is a 400, not a 422.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Attach a reusable provider. When set, `base_url` and `api_key_env`
    /// default to the provider's values.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub channel: Option<ChannelRequest>,
    #[serde(default)]
    pub shares: Option<Vec<ShareRef>>,
    #[serde(default)]
    pub mem_limit_mb: Option<u32>,
    #[serde(default)]
    pub cpu_limit: Option<f64>,
    /// LAN access (default `false`). See `docs/EGRESS.md`.
    #[serde(default)]
    pub allow_lan: Option<bool>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Convenience field: stored as `HERMES_GATE_PROFILE_TOKEN` (and as
    /// `api_key_env` when the two differ).
    #[serde(default)]
    pub gate_token: Option<String>,
    /// Additional environment variables. The canonical shape is a list of
    /// `{key, value, secret}`; the deprecated v0.4.0 object form
    /// (`{"KEY": "value"}`) is accepted too and stores every entry as a secret.
    /// The gate token and the platform token are always secret and win over an
    /// entry with the same key.
    #[serde(default)]
    pub env: Option<EnvInput>,
}

/// `POST /api/instances/{id}/clone`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CloneInstanceRequest {
    /// Id of the new instance; required (validated by the handler for a 400).
    #[serde(default)]
    pub new_id: Option<String>,
    /// Display name (defaults to the source name plus " copy").
    #[serde(default)]
    pub name: Option<String>,
    /// Model of the clone (defaults to the source model).
    #[serde(default)]
    pub model: Option<String>,
    /// Provider of the clone (defaults to the source provider). An empty
    /// string detaches the clone from any provider.
    #[serde(default)]
    pub provider: Option<String>,
    /// Replacement for `HERMES_GATE_PROFILE_TOKEN` (and `api_key_env`).
    #[serde(default)]
    pub gate_token: Option<String>,
    /// Replacement for the platform bot token of the source channel.
    #[serde(default)]
    pub channel_token: Option<String>,
    /// Copy the source shares that still exist (default `true`).
    #[serde(default)]
    pub shares_inherit: Option<bool>,
    /// Copy the source `data/` directory (default `true`).
    #[serde(default)]
    pub copy_data: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PatchInstanceRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// `null` detaches the provider, a string attaches one, a missing field
    /// leaves the current attachment alone.
    #[serde(default, deserialize_with = "double_option")]
    pub provider: Option<Option<String>>,
    #[serde(default)]
    pub channel: Option<ChannelRequest>,
    #[serde(default)]
    pub shares: Option<Vec<ShareRef>>,
    #[serde(default)]
    pub mem_limit_mb: Option<u32>,
    #[serde(default)]
    pub cpu_limit: Option<f64>,
    #[serde(default)]
    pub allow_lan: Option<bool>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub gate_token: Option<String>,
    /// Merge into the current environment: `null` deletes a key, `""` keeps an
    /// empty value, a missing `secret` keeps the flag of an existing key. The
    /// deprecated v0.4.0 object form is accepted and stores its entries as
    /// secrets.
    #[serde(default)]
    pub env: Option<EnvInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateShareRequest {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttachShareRequest {
    #[serde(default)]
    pub instance: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetachShareRequest {
    #[serde(default)]
    pub instance: Option<String>,
}

/// Deserialize a field in a way that tells "absent" (`None`) from "null"
/// (`Some(None)`): used by PATCH bodies that can detach a reference.
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// RFC 3339 UTC timestamp, second precision (`2026-11-08T01:00:00Z`).
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn validate_instance_id(id: &str) -> Result<(), String> {
    validate_slug(id, "instance id", INSTANCE_ID_MIN, INSTANCE_ID_MAX)
}

pub fn validate_share_name(name: &str) -> Result<(), String> {
    validate_slug(name, "share name", SHARE_NAME_MIN, SHARE_NAME_MAX)
}

/// Provider names share the slug grammar of ids (lowercase, digits, `-`).
pub fn validate_provider_name(name: &str) -> Result<(), String> {
    validate_slug(name, "provider name", PROVIDER_NAME_MIN, PROVIDER_NAME_MAX)
}

/// A provider kind is metadata; unknown kinds are refused so the console can
/// offer a fixed list instead of free text.
pub fn validate_provider_kind(kind: &str) -> Result<(), String> {
    if PROVIDER_KINDS.contains(&kind) {
        return Ok(());
    }
    Err(format!("kind must be one of {}", PROVIDER_KINDS.join(", ")))
}

fn validate_slug(value: &str, what: &str, min: usize, max: usize) -> Result<(), String> {
    if value.len() < min || value.len() > max {
        return Err(format!("{what} must be {min} to {max} characters long"));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!(
            "{what} may only contain lowercase ASCII letters, digits and '-'"
        ));
    }
    let first = value.as_bytes()[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        // Docker Compose project names must start with a letter or a digit.
        return Err(format!("{what} must start with a letter or a digit"));
    }
    Ok(())
}

pub fn validate_mem_limit(value: u32) -> Result<(), String> {
    if !(MEM_LIMIT_MIN_MB..=MEM_LIMIT_MAX_MB).contains(&value) {
        return Err(format!(
            "mem_limit_mb must be between {MEM_LIMIT_MIN_MB} and {MEM_LIMIT_MAX_MB}"
        ));
    }
    Ok(())
}

pub fn validate_cpu_limit(value: f64) -> Result<(), String> {
    if !value.is_finite() || !(CPU_LIMIT_MIN..=CPU_LIMIT_MAX).contains(&value) {
        return Err(format!(
            "cpu_limit must be between {CPU_LIMIT_MIN} and {CPU_LIMIT_MAX}"
        ));
    }
    Ok(())
}

pub fn validate_model(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("model must not be empty".to_string());
    }
    if value.chars().any(|c| c.is_control()) {
        return Err("model contains control characters".to_string());
    }
    Ok(())
}

/// Validate a Docker image reference (`repo/name:tag`, `name@sha256:…`).
///
/// This is not the full distribution-reference grammar: it only refuses what
/// would break the compose document or the command line (empty value, spaces,
/// control characters, a leading dash).
pub fn validate_image(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("image must not be empty".to_string());
    }
    if value.len() > 256 {
        return Err("image must be at most 256 characters".to_string());
    }
    if value.starts_with('-') {
        return Err("image must not start with '-'".to_string());
    }
    if value
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '\'' | '"' | '\\'))
    {
        return Err("image must not contain whitespace, quotes or control characters".to_string());
    }
    Ok(())
}

pub fn validate_base_url(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("base_url must not be empty".to_string());
    }
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("base_url must not contain whitespace or control characters".to_string());
    }
    Ok(())
}

pub fn validate_api_key_env(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 64 {
        return Err("api_key_env must be 1 to 64 characters long".to_string());
    }
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() || first == '_' => {}
        _ => return Err("api_key_env must start with an uppercase letter or '_'".to_string()),
    }
    if !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        return Err("api_key_env may only contain uppercase letters, digits and '_'".to_string());
    }
    Ok(())
}

pub fn validate_secret_name(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 128 {
        return Err("environment variable name must be 1 to 128 characters long".to_string());
    }
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return Err("environment variable name must start with a letter or '_'".to_string()),
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(
            "environment variable name may only contain letters, digits and '_'".to_string(),
        );
    }
    Ok(())
}

pub fn validate_secret_value(value: &str) -> Result<(), String> {
    if value.contains(['\n', '\r', '\0']) {
        return Err("secret values must not contain newlines or NUL bytes".to_string());
    }
    Ok(())
}

/// True when the control plane owns the name and an instance may not set it.
pub fn env_key_is_reserved(key: &str) -> bool {
    RESERVED_ENV_KEYS.contains(&key) || key.starts_with(RESERVED_ENV_PREFIX)
}

/// Validate an instance environment variable name.
///
/// `^[A-Z_][A-Z0-9_]{0,63}$`, minus the names the container contract owns
/// (`HERMES_UID`, `HERMES_GID`, `PATH`, `HOME` and the `FLEETT_` prefix).
pub fn validate_env_key(key: &str) -> Result<(), String> {
    if key.len() > ENV_KEY_MAX {
        return Err(format!(
            "environment variable names must be at most {ENV_KEY_MAX} characters"
        ));
    }
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() || first == '_' => {}
        _ => {
            return Err(
                "environment variable names must start with an uppercase letter or '_'".to_string(),
            )
        }
    }
    if !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        return Err(
            "environment variable names may only contain uppercase letters, digits and '_'"
                .to_string(),
        );
    }
    if env_key_is_reserved(key) {
        return Err(format!("environment variable \"{key}\" is reserved"));
    }
    Ok(())
}

/// Validate an instance environment variable value.
pub fn validate_env_value(value: &str) -> Result<(), String> {
    if value.len() > ENV_VALUE_MAX_BYTES {
        return Err(format!(
            "environment variable values must be at most {} KiB",
            ENV_VALUE_MAX_BYTES / 1024
        ));
    }
    if value.contains(['\n', '\r', '\0']) {
        return Err(
            "environment variable values must not contain newlines or NUL bytes".to_string(),
        );
    }
    Ok(())
}

pub fn validate_notes(value: &str) -> Result<(), String> {
    if value.len() > 4096 {
        return Err("notes must be at most 4096 characters".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_ids_are_strict() {
        assert!(validate_instance_id("alice").is_ok());
        assert!(validate_instance_id("al-2").is_ok());
        assert!(validate_instance_id("ab").is_ok());
        assert!(validate_instance_id("a").is_err());
        assert!(validate_instance_id("Alice").is_err());
        assert!(validate_instance_id("alice_1").is_err());
        assert!(validate_instance_id("alice/../x").is_err());
        assert!(validate_instance_id("-alice").is_err());
        assert!(validate_instance_id(&"a".repeat(33)).is_err());
    }

    #[test]
    fn mem_limit_bounds() {
        assert!(validate_mem_limit(256).is_ok());
        assert!(validate_mem_limit(8192).is_ok());
        assert!(validate_mem_limit(255).is_err());
        assert!(validate_mem_limit(8193).is_err());
    }

    #[test]
    fn cpu_limit_bounds() {
        assert!(validate_cpu_limit(CPU_LIMIT_MIN).is_ok());
        assert!(validate_cpu_limit(DEFAULT_CPU_LIMIT).is_ok());
        assert!(validate_cpu_limit(CPU_LIMIT_MAX).is_ok());
        assert!(validate_cpu_limit(0.0).is_err());
        assert!(validate_cpu_limit(0.05).is_err());
        assert!(validate_cpu_limit(8.1).is_err());
        assert!(validate_cpu_limit(f64::NAN).is_err());
        assert!(validate_cpu_limit(f64::INFINITY).is_err());
    }

    #[test]
    fn health_values_round_trip() {
        for (text, expected) in [
            ("healthy", Health::Healthy),
            ("Unhealthy", Health::Unhealthy),
            (" STARTING ", Health::Starting),
            ("", Health::Unknown),
            ("none", Health::Unknown),
            ("weird", Health::Unknown),
        ] {
            assert_eq!(Health::parse(text), expected, "{text:?}");
        }
        assert_eq!(Health::Healthy.as_str(), "healthy");
        assert_eq!(Health::Unhealthy.as_str(), "unhealthy");
        assert_eq!(Health::Starting.as_str(), "starting");
        assert_eq!(Health::Unknown.as_str(), "none");
    }

    #[test]
    fn secret_values_reject_newlines() {
        assert!(validate_secret_value("123:ABC").is_ok());
        assert!(validate_secret_value("a\nb").is_err());
        assert!(validate_secret_name("TELEGRAM_BOT_TOKEN").is_ok());
        assert!(validate_secret_name("lower_case").is_ok());
        assert!(validate_secret_name("1BAD").is_err());
        assert!(validate_secret_name("WITH-DASH").is_err());
    }

    #[test]
    fn env_names_are_uppercase_and_not_reserved() {
        for valid in ["DISCORD_ALLOWED_USERS", "_PRIVATE", "A", "A1_B2"] {
            assert!(validate_env_key(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "lower", "1ST", "WITH-DASH", "WITH SPACE", "A\nB"] {
            assert!(validate_env_key(invalid).is_err(), "{invalid:?}");
        }
        assert!(validate_env_key(&"A".repeat(64)).is_ok());
        assert!(validate_env_key(&"A".repeat(65)).is_err());

        for reserved in ["HERMES_UID", "HERMES_GID", "PATH", "HOME", "FLEETT_ROOT"] {
            assert!(env_key_is_reserved(reserved), "{reserved}");
            assert!(validate_env_key(reserved).is_err(), "{reserved}");
        }
        assert!(!env_key_is_reserved("FLEET_ROOT"));
        assert!(!env_key_is_reserved("TELEGRAM_ALLOWED_USERS"));
    }

    #[test]
    fn env_values_are_bounded_and_single_line() {
        assert!(validate_env_value("").is_ok());
        assert!(validate_env_value("123,456").is_ok());
        assert!(validate_env_value(&"a".repeat(ENV_VALUE_MAX_BYTES)).is_ok());
        assert!(validate_env_value(&"a".repeat(ENV_VALUE_MAX_BYTES + 1)).is_err());
        assert!(validate_env_value("a\nb").is_err());
        assert!(validate_env_value("a\rb").is_err());
        assert!(validate_env_value("a\0b").is_err());
    }

    #[test]
    fn image_references_are_validated() {
        for valid in [
            "hermes-agent:0.22.2-local",
            "ghcr.io/cleeryy/hermes-agent:0.23.0",
            "registry.example.test:5000/agent",
            "agent@sha256:0123abcd",
        ] {
            assert!(validate_image(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "  ", "with space:1", "-agent:1", "a\nb", "quote\"d"] {
            assert!(validate_image(invalid).is_err(), "{invalid:?}");
        }
        assert!(validate_image(&"a".repeat(257)).is_err());
    }

    #[test]
    fn env_accepts_the_list_and_the_legacy_map() {
        let list = serde_json::json!([
            {"key": "DISCORD_ALLOWED_USERS", "value": "123", "secret": false}
        ]);
        let parsed = EnvInput::from_json_value(&list).expect("list");
        let vars = parsed.into_vars();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].key, "DISCORD_ALLOWED_USERS");
        assert_eq!(vars[0].value.as_deref(), Some("123"));
        assert_eq!(vars[0].secret, Some(false));

        let map = serde_json::json!({"DISCORD_BOT_TOKEN": "secret"});
        let parsed = EnvInput::from_json_value(&map).expect("map");
        let vars = parsed.into_vars();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].key, "DISCORD_BOT_TOKEN");
        assert_eq!(vars[0].value.as_deref(), Some("secret"));
        assert_eq!(vars[0].secret, Some(true), "the map defaults to secret");
    }

    #[test]
    fn env_shape_errors_name_the_received_type() {
        let number = serde_json::json!(42);
        assert_eq!(
            EnvInput::from_json_value(&number)
                .expect_err("number")
                .received,
            "number"
        );
        let string = serde_json::json!("nope");
        assert_eq!(
            EnvInput::from_json_value(&string)
                .expect_err("string")
                .received,
            "string"
        );
        let bad_entry = serde_json::json!([{"key": 1}]);
        assert_eq!(
            EnvInput::from_json_value(&bad_entry)
                .expect_err("bad entry")
                .received,
            "sequence"
        );
        let bad_map_value = serde_json::json!({"KEY": 1});
        assert_eq!(
            EnvInput::from_json_value(&bad_map_value)
                .expect_err("bad map value")
                .received,
            "map"
        );
    }

    #[test]
    fn upgrade_statuses_report_attention() {
        assert!(!UpgradeStatus::Pending.needs_attention());
        assert!(!UpgradeStatus::Upgrading.needs_attention());
        assert!(!UpgradeStatus::Healthy.needs_attention());
        assert!(UpgradeStatus::RolledBack.needs_attention());
        assert!(UpgradeStatus::Failed.needs_attention());
        assert_eq!(UpgradeStatus::RolledBack.to_string(), "rolled_back");
    }
}
