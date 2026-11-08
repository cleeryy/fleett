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
pub const DEFAULT_TEMPLATE: &str = "guest";
pub const DEFAULT_API_KEY_ENV: &str = "HERMES_GATE_PROFILE_TOKEN";
pub const GATE_TOKEN_ENV: &str = "HERMES_GATE_PROFILE_TOKEN";

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
    pub channel: ChannelKind,
    pub shares: Vec<ShareRef>,
    pub mem_limit_mb: u32,
    pub cpu_limit: f64,
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
    pub channel: String,
    pub status: String,
    pub health: String,
    pub cpu_limit: f64,
    pub allow_lan: bool,
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
    pub base_url: String,
    pub api_key_env: String,
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
    pub env_keys: Vec<String>,
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

#[derive(Debug, Clone, Serialize)]
pub struct TemplateInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub default_model: String,
    pub channel: String,
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
    /// Extra environment variables written to `secrets.env`.
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
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
}
