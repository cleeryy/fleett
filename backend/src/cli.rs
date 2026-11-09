//! Client mode: the `fleett` binary talking to a running control plane.
//!
//! Server commands (`serve`, `migrate`, `secrets`, `backup`, and the host
//! firewall half of `egress`) run on the host; the commands below are a thin
//! HTTP client so the fleet is scriptable from cron, CI or a runbook. See
//! `docs/CLI.md`.

use crate::logstore::LogEntry;
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
/// Client configuration, relative to `$HOME`.
pub const CONFIG_RELATIVE_PATH: &str = ".config/fleett/config.toml";
/// Overrides the `[server] url` of the configuration file.
pub const URL_ENV: &str = "FLEETT_URL";
/// Overrides the `[server] token` of the configuration file.
pub const TOKEN_ENV: &str = "FLEETT_TOKEN";
/// Default `--tail` of `fleett logs`, like the HTTP API.
pub const DEFAULT_LOG_TAIL: u32 = 200;
/// Default provider base URL, the same one the web console pre-fills.
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
/// Interval between two `GET /api/upgrade` polls of `fleett upgrade`.
pub const UPGRADE_POLL_INTERVAL: Duration = Duration::from_secs(2);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// `[server]` section of `~/.config/fleett/config.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
struct ServerSection {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    server: ServerSection,
}

/// Resolved client settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// Base URL, without a trailing slash.
    pub url: String,
    /// Bearer token; `None` lets the server answer (e.g. `FLEETT_ALLOW_NO_AUTH`).
    pub token: Option<String>,
}

impl ClientConfig {
    /// Load the file from `$HOME` and let `FLEETT_URL` / `FLEETT_TOKEN` win.
    pub fn load() -> Result<Self, CliError> {
        let path = default_config_path()?;
        Self::resolve(&path, |key| std::env::var(key).ok())
    }

    /// Resolve the settings from an explicit file and an injectable environment.
    ///
    /// Environment variables win over the file, field by field. A missing file
    /// is not an error as long as both values come from the environment.
    pub fn resolve(
        config_path: &Path,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, CliError> {
        let file = read_config_file(config_path)?;
        let url = env(URL_ENV)
            .and_then(non_empty)
            .or_else(|| file.server.url.and_then(non_empty))
            .ok_or_else(|| {
                CliError::Config(format!(
                    "no server URL configured: set [server] url in {} or {URL_ENV}",
                    config_path.display()
                ))
            })?;
        let token = env(TOKEN_ENV)
            .and_then(non_empty)
            .or_else(|| file.server.token.and_then(non_empty));
        Ok(Self {
            url: normalize_url(&url)?,
            token,
        })
    }
}

/// Path of the configuration file, `~/.config/fleett/config.toml`.
pub fn default_config_path() -> Result<PathBuf, CliError> {
    match std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        Some(home) => Ok(PathBuf::from(home).join(CONFIG_RELATIVE_PATH)),
        None => Err(CliError::Config(format!(
            "cannot locate {CONFIG_RELATIVE_PATH}: HOME is not set; set {URL_ENV} and {TOKEN_ENV} instead"
        ))),
    }
}

fn read_config_file(path: &Path) -> Result<ConfigFile, CliError> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|err| CliError::Config(format!("cannot parse {}: {err}", path.display()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(ConfigFile::default()),
        Err(err) => Err(CliError::Config(format!(
            "cannot read {}: {err}",
            path.display()
        ))),
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Validate and normalise the base URL (http/https, no trailing slash).
fn normalize_url(value: &str) -> Result<String, CliError> {
    let trimmed = value.trim().trim_end_matches('/');
    let parsed = url::Url::parse(trimmed)
        .map_err(|err| CliError::Config(format!("invalid server URL \"{value}\": {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(CliError::Config(format!(
            "invalid server URL \"{value}\": expected http:// or https://"
        )));
    }
    Ok(trimmed.to_string())
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Config(String),
    #[error("cannot reach {url}: {detail}")]
    Transport { url: String, detail: String },
    #[error("{message} (code: {code})")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
    #[error("unexpected response: {0}")]
    Decode(String),
    /// `fleett doctor` already printed its checklist; only the exit code
    /// carries the failure.
    #[error("one or more checks failed")]
    ChecksFailed,
    /// `fleett upgrade` already printed the per-instance results.
    #[error("one or more instances were not upgraded")]
    UpgradeIncomplete,
}

impl CliError {
    /// Stable machine-readable code, printed with `--json`.
    pub fn code(&self) -> &str {
        match self {
            Self::Config(_) => "config_error",
            Self::Transport { .. } => "connection_error",
            Self::Api { code, .. } => code,
            Self::Decode(_) => "decode_error",
            Self::ChecksFailed => "checks_failed",
            Self::UpgradeIncomplete => "upgrade_incomplete",
        }
    }

    /// Human message without the machine-readable suffix.
    pub fn message(&self) -> String {
        match self {
            Self::Api { message, .. } => message.clone(),
            other => other.to_string(),
        }
    }
}

fn parse_api_error(status: u16, text: &str) -> CliError {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(error) = value.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("http_error")
                .to_string();
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the server returned an error")
                .to_string();
            return CliError::Api {
                status,
                code,
                message,
            };
        }
    }
    let message = text.trim();
    CliError::Api {
        status,
        code: "http_error".to_string(),
        message: if message.is_empty() {
            format!("the server answered HTTP {status}")
        } else {
            message.to_string()
        },
    }
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct Client {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Client {
    pub(crate) fn new(cfg: &ClientConfig) -> Result<Self, CliError> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|err| CliError::Config(format!("cannot build the HTTP client: {err}")))?;
        Ok(Self {
            base: cfg.url.clone(),
            token: cfg.token.clone(),
            http,
        })
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<T, CliError> {
        let value = self.value(method, path, query, body).await?;
        serde_json::from_value(value).map_err(|err| CliError::Decode(err.to_string()))
    }

    pub(crate) async fn value(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value, CliError> {
        let text = self.text(method, path, query, body).await?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(trimmed).map_err(|err| CliError::Decode(format!("{err}: {trimmed}")))
    }

    pub(crate) async fn text(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<String, CliError> {
        let response = self.send(method, path, query, body).await?;
        response.text().await.map_err(|err| CliError::Transport {
            url: format!("{}{}", self.base, path),
            detail: err.to_string(),
        })
    }

    async fn bytes(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Vec<u8>, CliError> {
        let response = self.send(method, path, query, None).await?;
        let bytes = response.bytes().await.map_err(|err| CliError::Transport {
            url: format!("{}{}", self.base, path),
            detail: err.to_string(),
        })?;
        Ok(bytes.to_vec())
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<reqwest::Response, CliError> {
        let url = format!("{}{}", self.base, path);
        let mut request = self.http.request(method, &url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|err| CliError::Transport {
            url: url.clone(),
            detail: err.to_string(),
        })?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(response);
        }
        let text = response.text().await.unwrap_or_default();
        Err(parse_api_error(status, &text))
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// One client-mode invocation.
#[derive(Debug, Clone)]
pub enum Command {
    Ls,
    Ps {
        all: bool,
    },
    Logs(LogsArgs),
    LogsSearch(LogSearchArgs),
    LogsTail(LogTailArgs),
    LogsExport(LogExportArgs),
    Create(CreateArgs),
    Start(String),
    Stop(String),
    Restart(String),
    Delete {
        id: String,
        purge: bool,
    },
    Usage {
        detail: bool,
    },
    Cost(CostArgs),
    Pricing(PricingAction),
    Budget(BudgetArgs),
    Images,
    Quota(QuotaArgs),
    Disk(DiskCommand),
    Upgrade(UpgradeArgs),
    Hosts,
    Host(HostAction),
    Instances(InstancesAction),
    Doctor,
    Incidents(IncidentsArgs),
    Heal(HealArgs),
    IncidentClose(String),
    Say(SayArgs),
    Messages(MessagesArgs),
    Ack {
        id: String,
        message_id: String,
    },
    Templates,
    Template(TemplateCommand),
    Schedules {
        id: String,
    },
    ScheduleAdd(ScheduleAddArgs),
    ScheduleRm {
        schedule_id: String,
    },
    ScheduleRun {
        schedule_id: String,
    },
    Runs {
        id: String,
        limit: usize,
    },
    Memory(MemoryCommand),
    Marketplace(MarketplaceCommand),
    AclGet(String),
    AclSet {
        id: String,
        subject: String,
        role: String,
    },
    AclRm {
        id: String,
        subject: String,
    },
    Egress(EgressCommand),
    McpServer(McpServerCommand),
}

/// Sub-commands of `fleett mcp-server` (MCP marketplace, issue #62).
///
/// This is the *client* side of a deployed MCP server. `fleett mcp` (without
/// `-server`) stays the MCP **server** mode that exposes the fleet.
#[derive(Debug, Clone)]
pub enum McpServerCommand {
    /// `fleett mcp-server ls`.
    List,
    /// `fleett mcp-server get <id>`.
    Get(String),
    /// `fleett mcp-server catalog [--q TEXT]`.
    Catalog {
        q: Option<String>,
    },
    /// `fleett mcp-server import --file manifest.json`.
    Import {
        file: PathBuf,
    },
    /// `fleett mcp-server deploy …`.
    Deploy(DeployMcpArgs),
    Start(String),
    Stop(String),
    Restart(String),
    /// `fleett mcp-server rm <id> [--purge]`.
    Remove {
        id: String,
        purge: bool,
    },
    Logs {
        id: String,
        tail: u32,
    },
    /// `fleett mcp-server test <id>`.
    Test(String),
    /// `fleett mcp-server tools <id>`.
    Tools(String),
    Attach {
        instance: String,
        server: String,
        mode: Option<String>,
    },
    Detach {
        instance: String,
        server: String,
    },
}

/// Arguments of `fleett mcp-server deploy`.
#[derive(Debug, Clone, Default)]
pub struct DeployMcpArgs {
    /// Server id (or catalogue id when `--catalog` is omitted).
    pub id: String,
    pub catalog: Option<String>,
    pub image: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub port: Option<u16>,
    pub persist: bool,
    pub cpu: Option<f64>,
    pub mem: Option<u32>,
    /// `KEY=VALUE` pairs stored as non-secret variables.
    pub env: Vec<String>,
    /// `KEY=VALUE` pairs stored encrypted.
    pub secret: Vec<String>,
    /// `NAME=VALUE` HTTP headers, stored encrypted and never returned.
    pub header: Vec<String>,
}

/// Arguments of `fleett egress policy|set|unset|blocks` (client mode).
#[derive(Debug, Clone)]
pub enum EgressCommand {
    /// `fleett egress policy <id>`.
    Policy { id: String },
    /// `fleett egress set <id> …`.
    Set(EgressSetArgs),
    /// `fleett egress unset <id>`.
    Unset { id: String },
    /// `fleett egress blocks <id> [--limit N] [--clear]`.
    Blocks {
        id: String,
        limit: usize,
        clear: bool,
    },
}

/// Arguments of `fleett egress set <id>`.
///
/// `set` replaces the whole policy, like the PUT it calls: an omitted list
/// becomes empty and an omitted `--proxy` clears the override. Only `unset`
/// (DELETE) removes a policy — saving an empty one does not (issue #58).
#[derive(Debug, Clone, Default)]
pub struct EgressSetArgs {
    pub id: String,
    /// `allow` (blocklist) or `deny` (allowlist); the server defaults to deny.
    pub mode: Option<String>,
    pub fqdns: Vec<String>,
    pub cidrs: Vec<String>,
    pub ports: Vec<u16>,
    /// Per-instance proxy override; `off` clears it.
    pub proxy: Option<String>,
}

/// Arguments of `fleett template <action>`.
#[derive(Debug, Clone)]
pub enum TemplateCommand {
    List,
    Get(String),
    Create(TemplateCreateArgs),
    FromInstance {
        instance: String,
        id: String,
        name: Option<String>,
    },
    Remove(String),
    Versions(String),
    Restore {
        id: String,
        version: i64,
    },
}

/// Arguments of `fleett template create`.
#[derive(Debug, Clone, Default)]
pub struct TemplateCreateArgs {
    /// Complete YAML definition (the full `POST /api/templates` body).
    pub file: Option<PathBuf>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub parent: Option<String>,
    pub default_model: Option<String>,
    pub channel: Option<String>,
    /// File copied into `templates/<id>/config.yaml`.
    pub config: Option<PathBuf>,
    /// File copied into `templates/<id>/soul.md`.
    pub soul: Option<PathBuf>,
    /// File copied into `templates/<id>/defaults.yaml`.
    pub defaults: Option<PathBuf>,
    /// Environment keys of the schema (`--env KEY`, repeatable).
    pub env: Vec<String>,
}

/// Arguments of `fleett schedule add`.
#[derive(Debug, Clone)]
pub struct ScheduleAddArgs {
    pub id: String,
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub enabled: Option<bool>,
    pub overlap: Option<String>,
    pub timeout_s: Option<u64>,
    pub timezone: Option<String>,
    pub catchup: bool,
}

/// Sub-commands of `fleett memory`.
#[derive(Debug, Clone)]
pub enum MemoryCommand {
    Set {
        namespace: String,
        key: String,
        value: String,
        tags: Vec<String>,
        ttl_s: Option<u64>,
    },
    Get {
        namespace: String,
        key: String,
    },
    List {
        namespace: Option<String>,
        limit: usize,
    },
    Search {
        query: String,
        namespace: Option<String>,
        limit: usize,
    },
    History {
        id: i64,
    },
    Restore {
        id: i64,
        version: u32,
    },
    Delete {
        id: i64,
    },
    Promote {
        id: i64,
        namespace: String,
    },
}

/// Sub-commands of `fleett marketplace`.
#[derive(Debug, Clone)]
pub enum MarketplaceCommand {
    Search {
        query: String,
        source: Option<String>,
        limit: usize,
    },
    Preview {
        source: String,
        reference: Option<String>,
    },
    Install {
        source: String,
        reference: Option<String>,
        confirm_scripts: bool,
        name: Option<String>,
    },
    Update {
        name: String,
        promote: bool,
        confirm_scripts: bool,
    },
    Updates,
    Sync,
}

/// Arguments of `fleett incidents`.
#[derive(Debug, Clone)]
pub struct IncidentsArgs {
    pub status: Option<String>,
    pub instance: Option<String>,
    pub limit: usize,
}

/// Arguments of `fleett heal`.
#[derive(Debug, Clone)]
pub struct HealArgs {
    pub id: String,
    pub policy: String,
    pub unhealthy_after: Option<u32>,
    pub max_restarts: Option<u32>,
    pub backoff_s: Option<u64>,
}

/// `fleett host …`.
#[derive(Debug, Clone)]
pub enum HostAction {
    Add(HostAddArgs),
    Test(String),
    Rm(String),
}

/// `fleett instances …`.
#[derive(Debug, Clone)]
pub enum InstancesAction {
    Move(MoveArgs),
}

/// Arguments of `fleett host add`.
#[derive(Debug, Clone, Default)]
pub struct HostAddArgs {
    pub name: String,
    pub id: Option<String>,
    pub transport: Option<String>,
    pub address: Option<String>,
    pub docker_context: Option<String>,
}

/// Arguments of `fleett instances move`.
#[derive(Debug, Clone)]
pub struct MoveArgs {
    pub id: String,
    pub to: String,
    /// Do not copy the data: the target already sees the same bind-mount path.
    pub skip_transfer: bool,
    /// Leave the instance stopped on the target.
    pub no_start: bool,
    /// Override the instance directory on the target host.
    pub remote_instance_dir: Option<String>,
    /// Keep the temporary archive copied to the target.
    pub keep_archive: bool,
}

/// Arguments of `fleett quota`.
#[derive(Debug, Clone, Default)]
pub struct QuotaArgs {
    pub id: String,
    /// Set the quota, in MB (`0` = unlimited).
    pub set_mb: Option<u32>,
    /// `warn`, `freeze` or `stop`.
    pub policy: Option<String>,
    /// `fleett quota cleanup <id>`: purge tmp/, cache/ and old logs.
    pub cleanup: bool,
}

/// Sub-commands of `fleett disk`.
#[derive(Debug, Clone)]
pub enum DiskCommand {
    Events {
        instance: Option<String>,
        limit: usize,
    },
}

/// Arguments of `fleett say`.
#[derive(Debug, Clone)]
pub struct SayArgs {
    pub to: String,
    pub body: String,
    pub kind: String,
    pub from: Option<String>,
    pub correlation: Option<String>,
    pub ttl_s: Option<u64>,
}

/// Arguments of `fleett messages`.
#[derive(Debug, Clone)]
pub struct MessagesArgs {
    pub id: String,
    pub view: String,
    pub status: Option<String>,
    pub limit: usize,
}

/// Arguments of `fleett cost`.
#[derive(Debug, Clone)]
pub struct CostArgs {
    pub from: Option<String>,
    pub to: Option<String>,
    pub group_by: String,
}

/// Arguments of `fleett pricing set`.
#[derive(Debug, Clone)]
pub struct PricingArgs {
    pub model: String,
    pub input: f64,
    pub output: f64,
    pub currency: String,
    pub effective_from: Option<String>,
}

/// `fleett pricing` subcommand.
#[derive(Debug, Clone)]
pub enum PricingAction {
    List,
    Set(PricingArgs),
}

/// Arguments of `fleett budget`.
#[derive(Debug, Clone)]
pub struct BudgetArgs {
    pub id: String,
    pub set: Option<f64>,
}

/// Arguments of `fleett upgrade`.
#[derive(Debug, Clone)]
pub struct UpgradeArgs {
    pub image: String,
    pub parallel: usize,
    pub dry_run: bool,
    pub timeout_s: u64,
}

impl Default for UpgradeArgs {
    fn default() -> Self {
        Self {
            image: String::new(),
            parallel: 1,
            dry_run: false,
            timeout_s: 180,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogsArgs {
    pub id: String,
    pub tail: u32,
    pub since: Option<String>,
    pub grep: Option<String>,
    pub level: Option<String>,
}

/// Filters shared by `fleett logs search` and `fleett logs export`.
#[derive(Debug, Clone)]
pub struct LogSearchArgs {
    pub q: Option<String>,
    /// One id, or a comma-separated list.
    pub instance: Option<String>,
    pub level: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: u32,
}

impl Default for LogSearchArgs {
    fn default() -> Self {
        Self {
            q: None,
            instance: None,
            level: None,
            since: None,
            until: None,
            limit: 100,
        }
    }
}

/// Arguments of `fleett logs tail`.
#[derive(Debug, Clone, Default)]
pub struct LogTailArgs {
    pub instance: Option<String>,
    pub q: Option<String>,
    pub level: Option<String>,
    pub since: Option<String>,
}

/// Arguments of `fleett logs export`.
#[derive(Debug, Clone)]
pub struct LogExportArgs {
    pub format: String,
    pub out: PathBuf,
    pub filters: LogSearchArgs,
}

#[derive(Debug, Clone, Default)]
pub struct CreateArgs {
    pub id: String,
    pub name: Option<String>,
    pub template: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub gate_token: Option<String>,
    pub channel: Option<String>,
    pub channel_token: Option<String>,
    pub allow_lan: bool,
    pub cpu: Option<f64>,
    pub mem: Option<u32>,
    pub note: Option<String>,
    /// Docker host id, or `auto` for placement.
    pub host: Option<String>,
}

/// Run one client command, returning the process exit code.
pub async fn run(command: Command, json: bool) -> i32 {
    match execute(command, json).await {
        Ok(()) => 0,
        Err(err) => {
            print_error(&err, json);
            1
        }
    }
}

async fn execute(command: Command, json: bool) -> Result<(), CliError> {
    let config = ClientConfig::load()?;
    let client = Client::new(&config)?;
    match command {
        Command::Ls => ls(&client, json).await,
        Command::Ps { all } => ps(&client, json, all).await,
        Command::Logs(args) => logs(&client, json, &args).await,
        Command::LogsSearch(args) => logs_search(&client, json, &args).await,
        Command::LogsTail(args) => logs_tail(&client, json, &args).await,
        Command::LogsExport(args) => logs_export(&client, json, &args).await,
        Command::Create(args) => create(&client, json, &args).await,
        Command::Start(id) => lifecycle(&client, json, &id, "start").await,
        Command::Stop(id) => lifecycle(&client, json, &id, "stop").await,
        Command::Restart(id) => lifecycle(&client, json, &id, "restart").await,
        Command::Delete { id, purge } => delete(&client, json, &id, purge).await,
        Command::Usage { detail } => usage(&client, json, detail).await,
        Command::Cost(args) => cost(&client, json, &args).await,
        Command::Pricing(action) => pricing(&client, json, &action).await,
        Command::Budget(args) => budget(&client, json, &args).await,
        Command::Images => images(&client, json).await,
        Command::Quota(args) => quota(&client, json, &args).await,
        Command::Disk(command) => disk(&client, json, &command).await,
        Command::Upgrade(args) => upgrade(&client, json, &args).await,
        Command::Hosts => hosts(&client, json).await,
        Command::Host(action) => host(&client, json, &action).await,
        Command::Instances(action) => instances(&client, json, &action).await,
        Command::Doctor => doctor(&client, json, &config.url).await,
        Command::Incidents(args) => incidents(&client, json, &args).await,
        Command::Heal(args) => heal(&client, json, &args).await,
        Command::IncidentClose(id) => incident_close(&client, json, &id).await,
        Command::Say(args) => say(&client, json, &args).await,
        Command::Messages(args) => messages(&client, json, &args).await,
        Command::Ack { id, message_id } => ack(&client, json, &id, &message_id).await,
        Command::Templates => templates_command(&client, json).await,
        Command::Template(action) => template_command(&client, json, &action).await,
        Command::Memory(command) => memory(&client, json, &command).await,
        Command::Marketplace(command) => marketplace(&client, json, &command).await,
        Command::AclGet(id) => acl_get(&client, json, &id).await,
        Command::AclSet { id, subject, role } => acl_set(&client, json, &id, &subject, &role).await,
        Command::AclRm { id, subject } => acl_rm(&client, json, &id, &subject).await,
        Command::Egress(command) => egress(&client, json, &command).await,
        Command::McpServer(command) => mcp_server(&client, json, &command).await,
        Command::Schedules { id } => schedules(&client, json, &id).await,
        Command::ScheduleAdd(args) => schedule_add(&client, json, &args).await,
        Command::ScheduleRm { schedule_id } => schedule_rm(&client, json, &schedule_id).await,
        Command::ScheduleRun { schedule_id } => schedule_run(&client, json, &schedule_id).await,
        Command::Runs { id, limit } => runs(&client, json, &id, limit).await,
    }
}

fn print_error(err: &CliError, json: bool) {
    // The doctor checklist and the upgrade table already carry the failure
    // (and their exit code); repeating it would only add noise.
    if matches!(err, CliError::ChecksFailed | CliError::UpgradeIncomplete) {
        return;
    }
    if json {
        let body = json!({
            "error": { "code": err.code(), "message": err.message() }
        });
        eprintln!("{body}");
    } else {
        eprintln!("fleett: {err}");
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    let text =
        serde_json::to_string_pretty(value).map_err(|err| CliError::Decode(err.to_string()))?;
    println!("{text}");
    Ok(())
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, CliError> {
    serde_json::from_value(value).map_err(|err| CliError::Decode(err.to_string()))
}

// ---------------------------------------------------------------------------
// API payloads (a subset of the server types)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct InstanceSummary {
    id: String,
    name: String,
    template: String,
    model: String,
    channel: String,
    status: String,
    health: String,
    cpu_limit: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct ContainerInfo {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    uptime_s: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct InstanceDetail {
    id: String,
    status: String,
    health: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    data_dir: String,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    container: Option<ContainerInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct StatusResponse {
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UsageEntry {
    id: String,
    status: String,
    cpu_pct: f64,
    mem_used_bytes: u64,
    mem_limit_bytes: u64,
    disk_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct UsageTotals {
    cpu_pct: f64,
    mem_used_bytes: u64,
    mem_limit_bytes: u64,
    disk_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct UsageCaps {
    max_cpu: f64,
    max_mem_mb: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct UsageReport {
    instances: Vec<UsageEntry>,
    totals: UsageTotals,
    caps: UsageCaps,
    #[serde(default)]
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HostCapacityView {
    #[serde(default)]
    ncpu: u32,
    #[serde(default)]
    mem_total_bytes: u64,
    #[serde(default)]
    docker_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HostView {
    id: String,
    name: String,
    transport: String,
    #[serde(default)]
    address: String,
    status: String,
    #[serde(default)]
    instances: usize,
    #[serde(default)]
    capacity: Option<HostCapacityView>,
    #[serde(default)]
    free_mem_bytes: Option<u64>,
    #[serde(default)]
    last_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HostTestView {
    ok: bool,
    status: String,
    latency_ms: u64,
    #[serde(default)]
    capacity: Option<HostCapacityView>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CostTotalsView {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    cost_usd: f64,
    events: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct CostGroupView {
    key: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    cost_usd: f64,
    events: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentCostView {
    instance_id: String,
    cost_usd: f64,
    total_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectionView {
    month: String,
    elapsed_fraction: f64,
    month_to_date_usd: f64,
    projected_usd: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct BudgetStatusView {
    instance_id: String,
    monthly_usd: f64,
    used_usd: f64,
    percent: f64,
    projected_month_usd: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct CostReportView {
    from: String,
    to: String,
    group_by: String,
    currency: String,
    totals: CostTotalsView,
    #[serde(default)]
    groups: Vec<CostGroupView>,
    #[serde(default)]
    top_agents: Vec<AgentCostView>,
    #[serde(default)]
    budgets: Vec<BudgetStatusView>,
    projection: ProjectionView,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PricingView {
    model: String,
    input_per_mtok: f64,
    output_per_mtok: f64,
    currency: String,
    effective_from: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BudgetView {
    instance_id: String,
    month: String,
    #[serde(default)]
    monthly_usd: Option<f64>,
    enabled: bool,
    configured: bool,
    used_usd: f64,
    percent: f64,
    #[serde(default)]
    exceeded: bool,
    projected_month_usd: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct UsageDetailView {
    instance: String,
    data: DirBreakdownView,
    #[serde(default)]
    docker_log_bytes: Option<u64>,
    #[serde(default)]
    image_bytes: Option<u64>,
    quota: QuotaView,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DirBreakdownView {
    #[serde(default)]
    total_bytes: u64,
    #[serde(default)]
    tmp_bytes: u64,
    #[serde(default)]
    cache_bytes: u64,
    #[serde(default)]
    logs_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct QuotaView {
    instance: String,
    quota_mb: u32,
    policy: String,
    used_bytes: u64,
    percent: f64,
    state: String,
    #[serde(default)]
    last_event: Option<DiskEventView>,
}

#[derive(Debug, Clone, Deserialize)]
struct DiskEventView {
    instance: String,
    at: String,
    action: String,
    #[serde(default)]
    before_bytes: u64,
    #[serde(default)]
    after_bytes: u64,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CleanupView {
    instance: String,
    before_bytes: u64,
    after_bytes: u64,
    freed_bytes: u64,
    #[serde(default)]
    removed_files: usize,
    #[serde(default)]
    removed_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ImageView {
    tag: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    size: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    instances: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct UpgradeResultView {
    id: String,
    #[serde(default)]
    from: Option<String>,
    to: String,
    status: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct UpgradeJobView {
    id: String,
    image: String,
    started_at: String,
    #[serde(default)]
    finished_at: Option<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    results: Vec<UpgradeResultView>,
}

impl UpgradeJobView {
    fn finished(&self) -> bool {
        self.finished_at.is_some()
    }

    /// `true` when at least one instance does not run the target image.
    fn needs_attention(&self) -> bool {
        self.results
            .iter()
            .any(|result| matches!(result.status.as_str(), "rolled_back" | "failed"))
    }
}

#[derive(Debug, Deserialize)]
struct Health {
    version: String,
    driver: String,
    docker: HealthDocker,
    #[serde(default)]
    secrets: Option<HealthSecrets>,
    #[serde(default)]
    egress: Option<HealthEgress>,
}

#[derive(Debug, Deserialize)]
struct HealthDocker {
    ok: bool,
}

#[derive(Debug, Deserialize)]
struct HealthSecrets {
    key: bool,
    #[serde(default)]
    key_error: Option<String>,
    #[serde(default)]
    encrypted: usize,
    #[serde(default)]
    plaintext: usize,
}

#[derive(Debug, Deserialize)]
struct HealthEgress {
    mode: String,
    marker: bool,
}

#[derive(Debug, Deserialize)]
struct Me {
    #[serde(default)]
    actor: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    method: String,
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

async fn ls(client: &Client, json: bool) -> Result<(), CliError> {
    let value = client
        .value(Method::GET, "/api/instances", &[], None)
        .await?;
    if json {
        return print_json(&value);
    }
    let instances: Vec<InstanceSummary> = decode(value)?;
    let rows: Vec<Vec<String>> = instances
        .iter()
        .map(|instance| {
            vec![
                instance.id.clone(),
                instance.name.clone(),
                instance.status.clone(),
                instance.health.clone(),
                instance.template.clone(),
                instance.model.clone(),
                instance.channel.clone(),
                format!("{:.1}", instance.cpu_limit),
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(
            &["ID", "NAME", "STATUS", "HEALTH", "TEMPLATE", "MODEL", "CHANNEL", "CPU"],
            &rows
        )
    );
    Ok(())
}

async fn ps(client: &Client, json: bool, all: bool) -> Result<(), CliError> {
    let value = client
        .value(Method::GET, "/api/instances", &[], None)
        .await?;
    let instances: Vec<InstanceSummary> = decode(value)?;
    let mut details = Vec::new();
    for instance in instances {
        if !all && instance.status != "running" {
            continue;
        }
        let detail = client
            .value(
                Method::GET,
                &format!("/api/instances/{}", encode_path_segment(&instance.id)),
                &[],
                None,
            )
            .await?;
        details.push(detail);
    }
    if json {
        return print_json(&details);
    }
    let details: Vec<InstanceDetail> = details
        .into_iter()
        .map(decode)
        .collect::<Result<_, CliError>>()?;
    let rows: Vec<Vec<String>> = details
        .iter()
        .map(|detail| {
            let container = detail.container.as_ref();
            let short_id = container
                .and_then(|container| container.id.as_deref())
                .map(|id| id.chars().take(12).collect::<String>())
                .unwrap_or_else(|| "-".to_string());
            let uptime = container
                .and_then(|container| container.uptime_s)
                .map(human_uptime)
                .unwrap_or_else(|| "-".to_string());
            vec![
                detail.id.clone(),
                detail.status.clone(),
                detail.health.clone(),
                short_id,
                uptime,
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(&["ID", "STATUS", "HEALTH", "CONTAINER", "UPTIME"], &rows)
    );
    Ok(())
}

async fn logs(client: &Client, json: bool, args: &LogsArgs) -> Result<(), CliError> {
    let mut query: Vec<(&str, String)> = vec![("tail", args.tail.to_string())];
    if let Some(since) = args.since.as_deref().filter(|value| !value.is_empty()) {
        query.push(("since", since.to_string()));
    }
    if let Some(grep) = args.grep.as_deref().filter(|value| !value.is_empty()) {
        query.push(("grep", grep.to_string()));
    }
    if let Some(level) = args.level.as_deref().filter(|value| !value.is_empty()) {
        query.push(("level", level.to_string()));
    }
    query.push(("format", if json { "json" } else { "text" }.to_string()));

    let path = format!("/api/instances/{}/logs", encode_path_segment(&args.id));
    let text = client.text(Method::GET, &path, &query, None).await?;
    if json {
        let value: Value = serde_json::from_str(text.trim())
            .map_err(|err| CliError::Decode(format!("{err}: {text}")))?;
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .map_err(|err| CliError::Decode(err.to_string()))?
        );
    } else {
        print!("{text}");
        if !text.is_empty() && !text.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LogSearchResponse {
    entries: Vec<LogEntry>,
    #[serde(default)]
    count: usize,
    #[serde(default)]
    limit: usize,
}

fn log_filter_pairs(args: &LogSearchArgs) -> Vec<(&'static str, String)> {
    let mut query: Vec<(&'static str, String)> = vec![("limit", args.limit.to_string())];
    if let Some(q) = args
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query.push(("q", q.to_string()));
    }
    if let Some(instance) = args
        .instance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query.push(("instances", instance.to_string()));
    }
    if let Some(level) = args
        .level
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query.push(("level", level.to_string()));
    }
    if let Some(since) = args
        .since
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query.push(("since", since.to_string()));
    }
    if let Some(until) = args
        .until
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query.push(("until", until.to_string()));
    }
    query
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

async fn logs_search(client: &Client, json: bool, args: &LogSearchArgs) -> Result<(), CliError> {
    let value = client
        .value(Method::GET, "/api/logs", &log_filter_pairs(args), None)
        .await?;
    if json {
        return print_json(&value);
    }
    let response: LogSearchResponse = decode(value)?;
    if response.entries.is_empty() {
        println!("no matching log lines");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = response
        .entries
        .iter()
        .map(|entry| {
            vec![
                entry.at.clone(),
                entry.instance.clone(),
                entry.level.as_str().to_string(),
                truncate_chars(&entry.message, 160),
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(&["AT", "INSTANCE", "LEVEL", "MESSAGE"], &rows)
    );
    eprintln!(
        "{} line(s) shown (limit {})",
        response.count, response.limit
    );
    Ok(())
}

async fn logs_tail(client: &Client, json: bool, args: &LogTailArgs) -> Result<(), CliError> {
    let mut search = LogSearchArgs {
        q: args.q.clone(),
        instance: args.instance.clone(),
        level: args.level.clone(),
        since: args.since.clone(),
        until: None,
        limit: 200,
    };
    let mut last_id = 0i64;
    let mut first = true;
    loop {
        if !first {
            // Poll the window since the newest line we printed. The id cursor
            // drops lines that share the same timestamp.
            search.since = Some("30s".to_string());
        }
        let value = client
            .value(Method::GET, "/api/logs", &log_filter_pairs(&search), None)
            .await?;
        let response: LogSearchResponse = decode(value)?;
        let mut entries = response.entries;
        entries.sort_by_key(|entry| entry.id);
        for entry in entries {
            if entry.id <= last_id {
                continue;
            }
            last_id = entry.id;
            if json {
                let line = serde_json::to_string(&entry)
                    .map_err(|err| CliError::Decode(err.to_string()))?;
                println!("{line}");
            } else {
                println!(
                    "{} {} {:<5} {}",
                    entry.at,
                    entry.instance,
                    entry.level.as_str(),
                    entry.message
                );
            }
        }
        first = false;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn logs_export(client: &Client, json: bool, args: &LogExportArgs) -> Result<(), CliError> {
    let format = args.format.trim().to_ascii_lowercase();
    if !matches!(format.as_str(), "json" | "csv") {
        return Err(CliError::Config(
            "format must be \"json\" or \"csv\"".to_string(),
        ));
    }
    let mut query = log_filter_pairs(&args.filters);
    query.push(("format", format.clone()));
    let text = client
        .text(Method::GET, "/api/logs/export", &query, None)
        .await?;
    std::fs::write(&args.out, &text)
        .map_err(|err| CliError::Config(format!("cannot write {}: {err}", args.out.display())))?;
    if json {
        print_json(&json!({
            "path": args.out.display().to_string(),
            "format": format,
            "bytes": text.len(),
        }))?;
    } else {
        println!(
            "wrote {} ({} bytes, {format})",
            args.out.display(),
            text.len()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MessageView {
    id: String,
    from: String,
    to: String,
    kind: String,
    body: String,
    #[serde(default)]
    correlation_id: Option<String>,
    hops: u32,
    status: String,
    created_at: String,
    expires_at: String,
    #[serde(default)]
    delivered_at: Option<String>,
    #[serde(default)]
    read_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ScheduleView {
    id: String,
    instance_id: String,
    name: String,
    cron: String,
    #[serde(default)]
    prompt: String,
    enabled: bool,
    #[serde(default)]
    timezone: String,
    overlap: String,
    #[serde(default)]
    timeout: u64,
    #[serde(default)]
    catchup: bool,
    #[serde(default)]
    last_run_at: Option<String>,
    #[serde(default)]
    next_run_at: Option<String>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ScheduleRunView {
    id: String,
    schedule_id: String,
    instance_id: String,
    started_at: String,
    #[serde(default)]
    finished_at: Option<String>,
    status: String,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    duration_ms: Option<i64>,
}

async fn say(client: &Client, json: bool, args: &SayArgs) -> Result<(), CliError> {
    let from = args
        .from
        .clone()
        .or_else(|| std::env::var("FLEETT_INSTANCE").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            CliError::Config(
                "the sending instance is unknown: pass --from <id> or set FLEETT_INSTANCE"
                    .to_string(),
            )
        })?;
    let mut body = json!({
        "to": args.to,
        "from": from,
        "kind": args.kind,
        "body": args.body,
    });
    if let Some(correlation) = &args.correlation {
        body["correlation_id"] = json!(correlation);
    }
    if let Some(ttl) = args.ttl_s {
        body["ttl_s"] = json!(ttl);
    }
    let path = format!("/api/instances/{}/messages", encode_path_segment(&from));
    let value = client.value(Method::POST, &path, &[], Some(body)).await?;
    if json {
        return print_json(&value);
    }
    let message: MessageView = decode(value)?;
    println!(
        "sent {} to {} (id {}, hops {})",
        message.kind, message.to, message.id, message.hops
    );
    Ok(())
}

async fn messages(client: &Client, json: bool, args: &MessagesArgs) -> Result<(), CliError> {
    let mut query: Vec<(&str, String)> = vec![
        ("view", args.view.clone()),
        ("limit", args.limit.to_string()),
    ];
    if let Some(status) = &args.status {
        query.push(("status", status.clone()));
    }
    let path = format!("/api/instances/{}/messages", encode_path_segment(&args.id));
    let value = client.value(Method::GET, &path, &query, None).await?;
    if json {
        return print_json(&value);
    }
    let messages: Vec<MessageView> = decode(value)?;
    if messages.is_empty() {
        println!("no messages");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = messages
        .iter()
        .map(|message| {
            vec![
                message.created_at.clone(),
                message.kind.clone(),
                if args.view == "outbox" {
                    message.to.clone()
                } else {
                    message.from.clone()
                },
                message.status.clone(),
                message.hops.to_string(),
                message.body.replace('\n', " "),
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(
            &[
                "AT",
                "KIND",
                if args.view == "outbox" { "TO" } else { "FROM" },
                "STATUS",
                "HOPS",
                "BODY"
            ],
            &rows
        )
    );
    Ok(())
}

async fn ack(client: &Client, json: bool, id: &str, message_id: &str) -> Result<(), CliError> {
    let path = format!(
        "/api/instances/{}/messages/{}/ack",
        encode_path_segment(id),
        encode_path_segment(message_id)
    );
    let value = client.value(Method::POST, &path, &[], None).await?;
    if json {
        return print_json(&value);
    }
    println!("message {message_id} marked as read");
    Ok(())
}

async fn schedules(client: &Client, json: bool, id: &str) -> Result<(), CliError> {
    let path = format!("/api/instances/{}/schedules", encode_path_segment(id));
    let value = client.value(Method::GET, &path, &[], None).await?;
    if json {
        return print_json(&value);
    }
    let schedules: Vec<ScheduleView> = decode(value)?;
    if schedules.is_empty() {
        println!("no schedules");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = schedules
        .iter()
        .map(|schedule| {
            vec![
                schedule.name.clone(),
                schedule.cron.clone(),
                if schedule.enabled { "yes" } else { "no" }.to_string(),
                schedule
                    .next_run_at
                    .clone()
                    .unwrap_or_else(|| "-".to_string()),
                schedule
                    .last_run_at
                    .clone()
                    .unwrap_or_else(|| "-".to_string()),
                schedule.overlap.clone(),
                format!("{}s", schedule.timeout),
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(
            &["NAME", "CRON", "ENABLED", "NEXT", "LAST", "OVERLAP", "TIMEOUT"],
            &rows
        )
    );
    Ok(())
}

async fn schedule_add(client: &Client, json: bool, args: &ScheduleAddArgs) -> Result<(), CliError> {
    let mut body = json!({
        "name": args.name,
        "cron": args.cron,
        "prompt": args.prompt,
    });
    let object = body.as_object_mut().expect("object");
    if let Some(enabled) = args.enabled {
        object.insert("enabled".to_string(), json!(enabled));
    }
    if let Some(overlap) = &args.overlap {
        object.insert("overlap".to_string(), json!(overlap));
    }
    if let Some(timeout) = args.timeout_s {
        object.insert("timeout".to_string(), json!(timeout));
    }
    if let Some(timezone) = &args.timezone {
        object.insert("timezone".to_string(), json!(timezone));
    }
    if args.catchup {
        object.insert("catchup".to_string(), json!(true));
    }
    let path = format!("/api/instances/{}/schedules", encode_path_segment(&args.id));
    let value = client.value(Method::POST, &path, &[], Some(body)).await?;
    if json {
        return print_json(&value);
    }
    let schedule: ScheduleView = decode(value)?;
    println!(
        "created schedule {} ({} on {}, next {})",
        schedule.id,
        schedule.name,
        schedule.cron,
        schedule.next_run_at.as_deref().unwrap_or("-")
    );
    Ok(())
}

async fn schedule_rm(client: &Client, json: bool, schedule_id: &str) -> Result<(), CliError> {
    let path = format!("/api/schedules/{}", encode_path_segment(schedule_id));
    client.text(Method::DELETE, &path, &[], None).await?;
    if json {
        return print_json(&json!({ "id": schedule_id, "deleted": true }));
    }
    println!("deleted schedule {schedule_id}");
    Ok(())
}

async fn schedule_run(client: &Client, json: bool, schedule_id: &str) -> Result<(), CliError> {
    let path = format!("/api/schedules/{}/run", encode_path_segment(schedule_id));
    let value = client.value(Method::POST, &path, &[], None).await?;
    if json {
        return print_json(&value);
    }
    let run: ScheduleRunView = decode(value)?;
    let mut line = format!(
        "run {}: {}{}",
        run.id,
        run.status,
        run.duration_ms
            .map(|duration| format!(" ({duration} ms)"))
            .unwrap_or_default()
    );
    if let Some(code) = run.exit_code {
        line.push_str(&format!(", exit {code}"));
    }
    println!("{line}");
    if run.status != "ok" {
        if let Some(output) = run.output.as_deref().filter(|text| !text.trim().is_empty()) {
            for line in output.lines() {
                println!("  {line}");
            }
        }
    }
    Ok(())
}

async fn runs(client: &Client, json: bool, id: &str, limit: usize) -> Result<(), CliError> {
    let path = format!("/api/instances/{}/runs", encode_path_segment(id));
    let query = vec![("limit", limit.to_string())];
    let value = client.value(Method::GET, &path, &query, None).await?;
    if json {
        return print_json(&value);
    }
    let runs: Vec<ScheduleRunView> = decode(value)?;
    if runs.is_empty() {
        println!("no runs");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = runs
        .iter()
        .map(|run| {
            vec![
                run.started_at.clone(),
                run.status.clone(),
                run.exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                run.duration_ms
                    .map(|duration| format!("{duration}ms"))
                    .unwrap_or_else(|| "-".to_string()),
                run.output
                    .as_deref()
                    .map(|output| single_line(output, 80))
                    .unwrap_or_default(),
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(&["STARTED", "STATUS", "EXIT", "DURATION", "OUTPUT"], &rows)
    );
    Ok(())
}

/// First `max` characters of `text` on one line.
fn single_line(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(max).collect();
    out.push('…');
    out
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MemoryEntryView {
    id: i64,
    namespace: String,
    key: String,
    value: String,
    tags: Vec<String>,
    owner: String,
    version: u32,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MemoryVersionView {
    version: u32,
    value: String,
    tags: Vec<String>,
    owner: String,
    action: String,
    at: String,
}

async fn memory(client: &Client, json: bool, command: &MemoryCommand) -> Result<(), CliError> {
    match command {
        MemoryCommand::Set {
            namespace,
            key,
            value,
            tags,
            ttl_s,
        } => {
            let mut body = json!({
                "namespace": namespace,
                "key": key,
                "value": value,
                "tags": tags,
            });
            if let Some(ttl) = ttl_s {
                body["ttl_s"] = json!(ttl);
            }
            let value = client
                .value(Method::POST, "/api/memory", &[], Some(body))
                .await?;
            if json {
                return print_json(&value);
            }
            let entry: MemoryEntryView = decode(value["entry"].clone())?;
            println!(
                "{} {}/{} (version {}, id {})",
                if value["created"].as_bool().unwrap_or(false) {
                    "created"
                } else {
                    "updated"
                },
                entry.namespace,
                entry.key,
                entry.version,
                entry.id
            );
        }
        MemoryCommand::Get { namespace, key } => {
            let query = [("namespace", namespace.clone()), ("key", key.clone())];
            let value = client
                .value(Method::GET, "/api/memory/by-key", &query, None)
                .await?;
            if json {
                return print_json(&value);
            }
            let entry: MemoryEntryView = decode(value)?;
            println!(
                "{} / {} (version {})",
                entry.namespace, entry.key, entry.version
            );
            println!("{}", entry.value);
            if !entry.tags.is_empty() {
                println!("tags: {}", entry.tags.join(", "));
            }
        }
        MemoryCommand::List { namespace, limit } => {
            let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
            if let Some(namespace) = namespace {
                query.push(("namespace", namespace.clone()));
            }
            let value = client
                .value(Method::GET, "/api/memory", &query, None)
                .await?;
            if json {
                return print_json(&value);
            }
            let entries: Vec<MemoryEntryView> = decode(value)?;
            if entries.is_empty() {
                println!("memory is empty");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = entries
                .iter()
                .map(|entry| {
                    vec![
                        entry.id.to_string(),
                        entry.namespace.clone(),
                        entry.key.clone(),
                        entry.value.replace('\n', " "),
                        entry.tags.join(","),
                        entry.version.to_string(),
                    ]
                })
                .collect();
            print!(
                "{}",
                render_table(&["ID", "NAMESPACE", "KEY", "VALUE", "TAGS", "VER"], &rows)
            );
        }
        MemoryCommand::Search {
            query,
            namespace,
            limit,
        } => {
            let mut params: Vec<(&str, String)> =
                vec![("q", query.clone()), ("limit", limit.to_string())];
            if let Some(namespace) = namespace {
                params.push(("namespace", namespace.clone()));
            }
            let value = client
                .value(Method::GET, "/api/memory/search", &params, None)
                .await?;
            if json {
                return print_json(&value);
            }
            let entries: Vec<MemoryEntryView> = decode(value)?;
            if entries.is_empty() {
                println!("no match");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = entries
                .iter()
                .map(|entry| {
                    vec![
                        entry.id.to_string(),
                        entry.namespace.clone(),
                        entry.key.clone(),
                        entry.value.replace('\n', " "),
                    ]
                })
                .collect();
            print!(
                "{}",
                render_table(&["ID", "NAMESPACE", "KEY", "VALUE"], &rows)
            );
        }
        MemoryCommand::History { id } => {
            let value = client
                .value(Method::GET, &format!("/api/memory/{id}/history"), &[], None)
                .await?;
            if json {
                return print_json(&value);
            }
            let versions: Vec<MemoryVersionView> = decode(value)?;
            let rows: Vec<Vec<String>> = versions
                .iter()
                .map(|version| {
                    vec![
                        version.version.to_string(),
                        version.action.clone(),
                        version.owner.clone(),
                        version.at.clone(),
                        version.value.replace('\n', " "),
                    ]
                })
                .collect();
            print!(
                "{}",
                render_table(&["VER", "ACTION", "ACTOR", "AT", "VALUE"], &rows)
            );
        }
        MemoryCommand::Restore { id, version } => {
            let body = json!({ "version": version });
            let value = client
                .value(
                    Method::POST,
                    &format!("/api/memory/{id}/rollback"),
                    &[],
                    Some(body),
                )
                .await?;
            if json {
                return print_json(&value);
            }
            let entry: MemoryEntryView = decode(value)?;
            println!(
                "restored {}/{} as version {}",
                entry.namespace, entry.key, entry.version
            );
        }
        MemoryCommand::Delete { id } => {
            client
                .text(Method::DELETE, &format!("/api/memory/{id}"), &[], None)
                .await?;
            if json {
                println!("{}", json!({"deleted": id}));
            } else {
                println!("deleted memory entry {id}");
            }
        }
        MemoryCommand::Promote { id, namespace } => {
            let body = json!({ "namespace": namespace });
            let value = client
                .value(
                    Method::POST,
                    &format!("/api/memory/{id}/promote"),
                    &[],
                    Some(body),
                )
                .await?;
            if json {
                return print_json(&value);
            }
            let entry: MemoryEntryView = decode(value)?;
            println!("promoted {} to {}", entry.key, entry.namespace);
        }
    }
    Ok(())
}

/// One `instance_acl` row as returned by the API.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct AclItemView {
    subject: String,
    role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AclView {
    instance: String,
    #[serde(default)]
    acl: Vec<AclItemView>,
}

/// The policy view of `GET|PUT|DELETE /api/instances/{id}/egress`.
///
/// Only the fields the table prints: `--json` passes the raw payload through,
/// so `proxy_acl` and anything the server adds later stay visible.
#[derive(Debug, Clone, Deserialize)]
struct EgressPolicyView {
    instance: String,
    source: String,
    mode: String,
    #[serde(default)]
    fqdns: Vec<String>,
    #[serde(default)]
    cidrs: Vec<String>,
    #[serde(default)]
    ports: Vec<u16>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    proxy: Option<String>,
    #[serde(default)]
    legacy_equivalent: bool,
    #[serde(default)]
    warnings: Vec<String>,
}

/// One refused destination of `GET /api/instances/{id}/egress/blocks`.
#[derive(Debug, Clone, Deserialize)]
struct EgressBlockView {
    at: String,
    host: String,
    port: u16,
    #[serde(default)]
    reason: Option<String>,
}

fn acl_path(id: &str) -> String {
    format!("/api/instances/{}/acl", encode_path_segment(id))
}

fn print_acl(view: &AclView, json: bool) -> Result<(), CliError> {
    if json {
        return print_json(view);
    }
    if view.acl.is_empty() {
        println!("{}: no ACL entries", view.instance);
        return Ok(());
    }
    println!("{}:", view.instance);
    let width = view
        .acl
        .iter()
        .map(|item| item.subject.chars().count())
        .max()
        .unwrap_or(0);
    for item in &view.acl {
        println!("  {:width$}  {}", item.subject, item.role);
    }
    Ok(())
}

/// `fleett acl get <id>`.
async fn acl_get(client: &Client, json: bool, id: &str) -> Result<(), CliError> {
    let view: AclView = client.json(Method::GET, &acl_path(id), &[], None).await?;
    print_acl(&view, json)
}

/// `fleett acl set <id> <subject> <role>` — upsert one grant, keep the rest.
async fn acl_set(
    client: &Client,
    json: bool,
    id: &str,
    subject: &str,
    role: &str,
) -> Result<(), CliError> {
    let role = role.trim().to_ascii_lowercase();
    if !matches!(role.as_str(), "owner" | "operator" | "viewer") {
        return Err(CliError::Config(
            "role must be \"owner\", \"operator\" or \"viewer\"".to_string(),
        ));
    }
    let path = acl_path(id);
    let mut view: AclView = client.json(Method::GET, &path, &[], None).await?;
    let subject = subject.trim().to_string();
    match view.acl.iter_mut().find(|item| item.subject == subject) {
        Some(item) => item.role = role,
        None => view.acl.push(AclItemView {
            subject,
            role,
            created_at: None,
        }),
    }
    let body = json!({ "acl": view.acl });
    let updated: AclView = client.json(Method::PUT, &path, &[], Some(body)).await?;
    print_acl(&updated, json)
}

/// `fleett acl rm <id> <subject>`.
async fn acl_rm(client: &Client, json: bool, id: &str, subject: &str) -> Result<(), CliError> {
    let path = format!("{}/{}", acl_path(id), encode_path_segment(subject.trim()));
    let value = client.value(Method::DELETE, &path, &[], None).await?;
    if json {
        return print_json(&json!({
            "instance": id,
            "subject": subject.trim(),
            "removed": true,
            "response": value,
        }));
    }
    println!("removed \"{}\" from the ACL of {}", subject.trim(), id);
    Ok(())
}

/// Path of the destination policy of one instance.
fn egress_path(id: &str) -> String {
    format!("/api/instances/{}/egress", encode_path_segment(id))
}

/// `fleett egress policy <id>` — print the policy in force.
async fn egress_policy(client: &Client, json: bool, id: &str) -> Result<(), CliError> {
    let value = client
        .value(Method::GET, &egress_path(id), &[], None)
        .await?;
    print_egress(value, json)
}

/// `fleett egress set <id> …` — replace the whole policy.
async fn egress_set(client: &Client, json: bool, args: &EgressSetArgs) -> Result<(), CliError> {
    let mode = args
        .mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    if let Some(mode) = &mode {
        if !matches!(
            mode.as_str(),
            "deny" | "allow" | "allowlist" | "blocklist" | "permit"
        ) {
            return Err(CliError::Config(format!(
                "--mode must be \"allow\" or \"deny\" (got \"{mode}\")"
            )));
        }
    }
    for port in &args.ports {
        if *port == 0 {
            return Err(CliError::Config(
                "--port must be between 1 and 65535".to_string(),
            ));
        }
    }
    // The whole policy is replaced; `null` values mean "not set" to the server,
    // which is exactly the semantics of `set`.
    let body = json!({
        "mode": mode,
        "fqdns": args.fqdns,
        "cidrs": args.cidrs,
        "ports": args.ports,
        "proxy": args.proxy,
    });
    let value = client
        .value(Method::PUT, &egress_path(&args.id), &[], Some(body))
        .await?;
    print_egress(value, json)
}

/// `fleett egress unset <id>` — DELETE, back to the legacy shape.
async fn egress_unset(client: &Client, json: bool, id: &str) -> Result<(), CliError> {
    let value = client
        .value(Method::DELETE, &egress_path(id), &[], None)
        .await?;
    print_egress(value, json)
}

/// `fleett egress blocks <id> [--limit N] [--clear]`.
async fn egress_blocks(
    client: &Client,
    json: bool,
    id: &str,
    limit: usize,
    clear: bool,
) -> Result<(), CliError> {
    let path = format!("{}/blocks", egress_path(id));
    if clear {
        let value = client.value(Method::DELETE, &path, &[], None).await?;
        if json {
            return print_json(&value);
        }
        let removed = value.get("removed").and_then(Value::as_u64).unwrap_or(0);
        println!("cleared {removed} blocked destination(s) of {id}");
        return Ok(());
    }
    let limit = limit.clamp(1, 1000);
    let query = vec![("limit", limit.to_string())];
    let value = client.value(Method::GET, &path, &query, None).await?;
    if json {
        return print_json(&value);
    }
    let blocks: Vec<EgressBlockView> = decode(value)?;
    if blocks.is_empty() {
        println!("no blocked destination recorded for {id}");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = blocks
        .iter()
        .map(|block| {
            vec![
                block.at.clone(),
                block.host.clone(),
                block.port.to_string(),
                block.reason.as_deref().unwrap_or("-").to_string(),
            ]
        })
        .collect();
    print!("{}", render_table(&["AT", "HOST", "PORT", "REASON"], &rows));
    Ok(())
}

/// Print a policy view, either verbatim (`--json`) or as a short report.
fn print_egress(value: Value, json: bool) -> Result<(), CliError> {
    if json {
        return print_json(&value);
    }
    let view: EgressPolicyView = decode(value)?;
    println!("{}: {} policy ({})", view.instance, view.source, view.mode);
    let list = |values: &[String]| {
        if values.is_empty() {
            "-".to_string()
        } else {
            values.join(", ")
        }
    };
    println!("  fqdns            {}", list(&view.fqdns));
    println!("  cidrs            {}", list(&view.cidrs));
    let ports: Vec<String> = view.ports.iter().map(u16::to_string).collect();
    println!("  ports            {}", list(&ports));
    match view.proxy.as_deref() {
        Some(proxy) => println!("  proxy            {proxy}"),
        None if view.source != "explicit" => {
            println!("  proxy            none (no explicit policy)")
        }
        None if view.legacy_equivalent => println!(
            "  proxy            none (the policy repeats the host firewall; no proxy is rendered)"
        ),
        None => println!("  proxy            none (no proxy configured)"),
    }
    if let Some(updated) = &view.updated_at {
        println!("  updated          {updated}");
    }
    if view.warnings.is_empty() {
        println!("  warnings         none");
    } else {
        for warning in &view.warnings {
            println!("  warning          {warning}");
        }
    }
    Ok(())
}

/// `fleett egress …` — one client-mode destination-policy command.
async fn egress(client: &Client, json: bool, command: &EgressCommand) -> Result<(), CliError> {
    match command {
        EgressCommand::Policy { id } => egress_policy(client, json, id).await,
        EgressCommand::Set(args) => egress_set(client, json, args).await,
        EgressCommand::Unset { id } => egress_unset(client, json, id).await,
        EgressCommand::Blocks { id, limit, clear } => {
            egress_blocks(client, json, id, *limit, *clear).await
        }
    }
}

async fn create(client: &Client, json: bool, args: &CreateArgs) -> Result<(), CliError> {
    if args.channel_token.is_some() && args.channel.is_none() {
        return Err(CliError::Config(
            "--channel-token requires --channel (telegram, discord or none)".to_string(),
        ));
    }
    let mut body = json!({ "id": args.id.clone() });
    let object = body.as_object_mut().expect("object");
    for (key, value) in [
        ("name", &args.name),
        ("template", &args.template),
        ("model", &args.model),
        ("base_url", &args.base_url),
        ("api_key_env", &args.api_key_env),
        ("gate_token", &args.gate_token),
        ("notes", &args.note),
        ("host", &args.host),
    ] {
        if let Some(value) = value {
            object.insert(key.to_string(), json!(value));
        }
    }
    if let Some(channel) = &args.channel {
        let mut value = json!({ "type": channel });
        if let Some(token) = &args.channel_token {
            value["token"] = json!(token);
        }
        object.insert("channel".to_string(), value);
    }
    // The web console pre-fills the same provider URL; a CLI create should not
    // need a flag for the fleet's usual endpoint.
    object
        .entry("base_url".to_string())
        .or_insert_with(|| json!(DEFAULT_BASE_URL));
    if args.allow_lan {
        object.insert("allow_lan".to_string(), json!(true));
    }
    if let Some(cpu) = args.cpu {
        object.insert("cpu_limit".to_string(), json!(cpu));
    }
    if let Some(mem) = args.mem {
        object.insert("mem_limit_mb".to_string(), json!(mem));
    }

    let value = client
        .value(Method::POST, "/api/instances", &[], Some(body))
        .await?;
    if json {
        return print_json(&value);
    }
    let detail: InstanceDetail = decode(value)?;
    println!("created instance {} ({})", detail.id, detail.status);
    for warning in &detail.warnings {
        println!("warning: {warning}");
    }
    Ok(())
}

async fn lifecycle(client: &Client, json: bool, id: &str, action: &str) -> Result<(), CliError> {
    let path = format!("/api/instances/{}/{action}", encode_path_segment(id));
    let response: StatusResponse = client.json(Method::POST, &path, &[], None).await?;
    if json {
        return print_json(&json!({ "id": id, "status": response.status }));
    }
    println!("{id}: {}", response.status);
    Ok(())
}

async fn delete(client: &Client, json: bool, id: &str, purge: bool) -> Result<(), CliError> {
    let path = format!("/api/instances/{}", encode_path_segment(id));
    let query = if purge {
        vec![("purge", "true".to_string())]
    } else {
        Vec::new()
    };
    client.text(Method::DELETE, &path, &query, None).await?;
    if json {
        return print_json(&json!({ "id": id, "deleted": true, "purged": purge }));
    }
    println!(
        "deleted instance {id}{}",
        if purge { " (data purged)" } else { "" }
    );
    Ok(())
}

async fn usage(client: &Client, json: bool, detail: bool) -> Result<(), CliError> {
    let value = client.value(Method::GET, "/api/usage", &[], None).await?;
    if detail {
        return usage_detail(client, json, &value).await;
    }
    if json {
        return print_json(&value);
    }
    let report: UsageReport = decode(value)?;
    let mut rows: Vec<Vec<String>> = report
        .instances
        .iter()
        .map(|entry| {
            vec![
                entry.id.clone(),
                entry.status.clone(),
                format!("{:.2}%", entry.cpu_pct),
                format!(
                    "{} / {}",
                    human_bytes(entry.mem_used_bytes),
                    human_bytes(entry.mem_limit_bytes)
                ),
                human_bytes(entry.disk_bytes),
            ]
        })
        .collect();
    rows.push(vec![
        "TOTAL".to_string(),
        String::new(),
        format!("{:.2}%", report.totals.cpu_pct),
        format!(
            "{} / {}",
            human_bytes(report.totals.mem_used_bytes),
            human_bytes(report.totals.mem_limit_bytes)
        ),
        human_bytes(report.totals.disk_bytes),
    ]);
    print!(
        "{}",
        render_table(&["ID", "STATUS", "CPU", "MEMORY", "DISK"], &rows)
    );
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
    println!("{}", caps_line(&report.caps));
    Ok(())
}

async fn cost(client: &Client, json: bool, args: &CostArgs) -> Result<(), CliError> {
    let mut query: Vec<(&str, String)> = vec![("group_by", args.group_by.clone())];
    if let Some(from) = args.from.as_deref().filter(|value| !value.is_empty()) {
        query.push(("from", from.to_string()));
    }
    if let Some(to) = args.to.as_deref().filter(|value| !value.is_empty()) {
        query.push(("to", to.to_string()));
    }
    let value = client.value(Method::GET, "/api/cost", &query, None).await?;
    if json {
        return print_json(&value);
    }
    let report: CostReportView = decode(value)?;
    let mut rows: Vec<Vec<String>> = report
        .groups
        .iter()
        .map(|group| {
            vec![
                group.key.clone(),
                group.prompt_tokens.to_string(),
                group.completion_tokens.to_string(),
                group.total_tokens.to_string(),
                format_cost(group.cost_usd),
                group.events.to_string(),
            ]
        })
        .collect();
    rows.push(vec![
        "TOTAL".to_string(),
        report.totals.prompt_tokens.to_string(),
        report.totals.completion_tokens.to_string(),
        report.totals.total_tokens.to_string(),
        format_cost(report.totals.cost_usd),
        report.totals.events.to_string(),
    ]);
    println!(
        "cost {} -> {} by {} ({} tokens, {})",
        report.from, report.to, report.group_by, report.totals.total_tokens, report.currency
    );
    print!(
        "{}",
        render_table(
            &["KEY", "PROMPT", "COMPLETION", "TOTAL", "COST", "EVENTS"],
            &rows
        )
    );
    for agent in &report.top_agents {
        println!(
            "top {:<16} {} {} ({} tokens)",
            agent.instance_id,
            format_cost(agent.cost_usd),
            report.currency,
            agent.total_tokens
        );
    }
    for budget in &report.budgets {
        println!(
            "budget {:<16} {} / {} {} ({:.1}%) projected {}",
            budget.instance_id,
            format_cost(budget.used_usd),
            format_cost(budget.monthly_usd),
            report.currency,
            budget.percent,
            format_cost(budget.projected_month_usd)
        );
    }
    println!(
        "month {}: {} {} so far, {} projected ({:.0}% elapsed)",
        report.projection.month,
        format_cost(report.projection.month_to_date_usd),
        report.currency,
        format_cost(report.projection.projected_usd),
        report.projection.elapsed_fraction * 100.0
    );
    Ok(())
}

async fn pricing(client: &Client, json: bool, action: &PricingAction) -> Result<(), CliError> {
    match action {
        PricingAction::List => {
            let value = client
                .value(Method::GET, "/api/models/pricing", &[], None)
                .await?;
            if json {
                return print_json(&value);
            }
            let prices: Vec<PricingView> = decode(value)?;
            if prices.is_empty() {
                println!("no price configured");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = prices
                .iter()
                .map(|price| {
                    vec![
                        price.model.clone(),
                        format!("{}", price.input_per_mtok),
                        format!("{}", price.output_per_mtok),
                        price.currency.clone(),
                        price.effective_from.clone(),
                    ]
                })
                .collect();
            print!(
                "{}",
                render_table(
                    &[
                        "MODEL",
                        "INPUT/MTOK",
                        "OUTPUT/MTOK",
                        "CURRENCY",
                        "EFFECTIVE"
                    ],
                    &rows
                )
            );
            Ok(())
        }
        PricingAction::Set(args) => {
            let mut body = json!({
                "model": args.model,
                "input_per_mtok": args.input,
                "output_per_mtok": args.output,
                "currency": args.currency,
            });
            if let Some(effective_from) = &args.effective_from {
                body["effective_from"] = json!(effective_from);
            }
            let value = client
                .value(Method::POST, "/api/models/pricing", &[], Some(body))
                .await?;
            if json {
                return print_json(&value);
            }
            let price: PricingView = decode(value)?;
            println!(
                "price {}: input {} / output {} {} per Mtok, effective {}",
                price.model,
                price.input_per_mtok,
                price.output_per_mtok,
                price.currency,
                price.effective_from
            );
            Ok(())
        }
    }
}

/// `fleett usage --detail`: per-instance data, Docker log and image sizes.
async fn usage_detail(client: &Client, json: bool, report: &Value) -> Result<(), CliError> {
    let report: UsageReport = decode(report.clone())?;
    let mut details = Vec::with_capacity(report.instances.len());
    for entry in &report.instances {
        let path = format!(
            "/api/instances/{}/usage/detail",
            encode_path_segment(&entry.id)
        );
        details.push(client.value(Method::GET, &path, &[], None).await?);
    }
    if json {
        return print_json(&Value::Array(details));
    }
    let details: Vec<UsageDetailView> = details
        .into_iter()
        .map(decode)
        .collect::<Result<_, CliError>>()?;
    let rows: Vec<Vec<String>> = details
        .iter()
        .map(|detail| {
            vec![
                detail.instance.clone(),
                human_bytes(detail.data.total_bytes),
                human_bytes(detail.data.tmp_bytes),
                human_bytes(detail.data.cache_bytes),
                human_bytes(detail.data.logs_bytes),
                detail
                    .docker_log_bytes
                    .map(human_bytes)
                    .unwrap_or_else(|| "-".to_string()),
                detail
                    .image_bytes
                    .map(human_bytes)
                    .unwrap_or_else(|| "-".to_string()),
                if detail.quota.quota_mb == 0 {
                    "unlimited".to_string()
                } else {
                    format!(
                        "{} / {} MB ({:.1}%, {})",
                        human_bytes(detail.quota.used_bytes),
                        detail.quota.quota_mb,
                        detail.quota.percent,
                        detail.quota.state
                    )
                },
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(
            &[
                "ID",
                "DATA",
                "TMP",
                "CACHE",
                "LOGS",
                "DOCKER LOG",
                "IMAGE",
                "QUOTA"
            ],
            &rows
        )
    );
    Ok(())
}

/// `fleett quota <id> [--set MB] [--policy ...]` and `... cleanup <id>`.
async fn quota(client: &Client, json: bool, args: &QuotaArgs) -> Result<(), CliError> {
    let encoded = encode_path_segment(&args.id);
    if args.cleanup {
        let value = client
            .value(
                Method::POST,
                &format!("/api/instances/{encoded}/quota/cleanup"),
                &[],
                Some(json!({})),
            )
            .await?;
        if json {
            return print_json(&value);
        }
        let report: CleanupView = decode(value)?;
        println!(
            "{}: freed {} ({} -> {}, {} file(s) removed)",
            report.instance,
            human_bytes(report.freed_bytes),
            human_bytes(report.before_bytes),
            human_bytes(report.after_bytes),
            report.removed_files
        );
        if !report.removed_paths.is_empty() {
            println!("removed: {}", report.removed_paths.join(", "));
        }
        return Ok(());
    }

    if args.set_mb.is_none() && args.policy.is_none() {
        let value = client
            .value(
                Method::GET,
                &format!("/api/instances/{encoded}/quota"),
                &[],
                None,
            )
            .await?;
        if json {
            return print_json(&value);
        }
        let view: QuotaView = decode(value)?;
        print_quota(&view);
        return Ok(());
    }

    // PUT replaces both fields: read the current row so a lone `--set` never
    // resets the policy (and vice versa).
    let current: QuotaView = client
        .json(
            Method::GET,
            &format!("/api/instances/{encoded}/quota"),
            &[],
            None,
        )
        .await?;
    let body = json!({
        "quota_mb": args.set_mb.unwrap_or(current.quota_mb),
        "policy": args.policy.clone().unwrap_or(current.policy),
    });
    let value = client
        .value(
            Method::PUT,
            &format!("/api/instances/{encoded}/quota"),
            &[],
            Some(body),
        )
        .await?;
    if json {
        return print_json(&value);
    }
    let view: QuotaView = decode(value)?;
    print_quota(&view);
    Ok(())
}

fn print_quota(view: &QuotaView) {
    if view.quota_mb == 0 {
        println!("{}: unlimited (policy {})", view.instance, view.policy);
    } else {
        println!(
            "{}: {} / {} MB ({:.2}%, {}, policy {})",
            view.instance,
            human_bytes(view.used_bytes),
            view.quota_mb,
            view.percent,
            view.state,
            view.policy
        );
    }
    if let Some(event) = &view.last_event {
        println!(
            "last event: {} at {} ({})",
            event.action,
            event.at,
            event.detail.as_deref().unwrap_or("-")
        );
    }
}

/// `fleett disk events [--instance ID] [--limit N]`.
async fn disk(client: &Client, json: bool, command: &DiskCommand) -> Result<(), CliError> {
    match command {
        DiskCommand::Events { instance, limit } => {
            let mut query = vec![("limit", limit.to_string())];
            if let Some(id) = instance.as_deref().filter(|value| !value.is_empty()) {
                query.push(("instance", id.to_string()));
            }
            let value = client
                .value(Method::GET, "/api/disk/events", &query, None)
                .await?;
            if json {
                return print_json(&value);
            }
            let events: Vec<DiskEventView> = decode(value)?;
            if events.is_empty() {
                println!("no disk events");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = events
                .iter()
                .map(|event| {
                    vec![
                        event.at.clone(),
                        event.instance.clone(),
                        event.action.clone(),
                        human_bytes(event.before_bytes),
                        human_bytes(event.after_bytes),
                        event.detail.clone().unwrap_or_default(),
                    ]
                })
                .collect();
            print!(
                "{}",
                render_table(
                    &["AT", "INSTANCE", "ACTION", "BEFORE", "AFTER", "DETAIL"],
                    &rows
                )
            );
            Ok(())
        }
    }
}

async fn budget(client: &Client, json: bool, args: &BudgetArgs) -> Result<(), CliError> {
    let path = format!("/api/instances/{}/budget", encode_path_segment(&args.id));
    let value = match args.set {
        Some(monthly_usd) => {
            client
                .value(
                    Method::PUT,
                    &path,
                    &[],
                    Some(json!({"monthly_usd": monthly_usd, "enabled": true})),
                )
                .await?
        }
        None => client.value(Method::GET, &path, &[], None).await?,
    };
    if json {
        return print_json(&value);
    }
    let view: BudgetView = decode(value)?;
    match view.monthly_usd {
        Some(monthly_usd) => println!(
            "{}: {} / {} USD ({:.1}%) in {}{}, projected {} USD",
            view.instance_id,
            format_cost(view.used_usd),
            format_cost(monthly_usd),
            view.percent,
            view.month,
            if view.exceeded { ", EXCEEDED" } else { "" },
            format_cost(view.projected_month_usd)
        ),
        None => println!(
            "{}: no budget configured ({} USD used in {})",
            view.instance_id,
            format_cost(view.used_usd),
            view.month
        ),
    }
    Ok(())
}

fn format_cost(value: f64) -> String {
    if value != 0.0 && value.abs() < 1.0 {
        format!("{value:.6}")
    } else {
        format!("{value:.2}")
    }
}

async fn images(client: &Client, json: bool) -> Result<(), CliError> {
    let value = client.value(Method::GET, "/api/images", &[], None).await?;
    if json {
        return print_json(&value);
    }
    let images: Vec<ImageView> = decode(value)?;
    let rows: Vec<Vec<String>> = images
        .iter()
        .map(|image| {
            vec![
                image.tag.clone(),
                short_id(&image.id),
                image.size.clone(),
                image.created.clone(),
                if image.instances.is_empty() {
                    "-".to_string()
                } else {
                    image.instances.join(",")
                },
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(&["TAG", "ID", "SIZE", "CREATED", "INSTANCES"], &rows)
    );
    Ok(())
}

async fn hosts(client: &Client, json: bool) -> Result<(), CliError> {
    let value = client.value(Method::GET, "/api/hosts", &[], None).await?;
    if json {
        return print_json(&value);
    }
    let hosts: Vec<HostView> = decode(value)?;
    let rows: Vec<Vec<String>> = hosts
        .iter()
        .map(|host| {
            vec![
                host.id.clone(),
                host.name.clone(),
                host.transport.clone(),
                if host.address.is_empty() {
                    "-".to_string()
                } else {
                    host.address.clone()
                },
                host.status.clone(),
                host.instances.to_string(),
                host.capacity
                    .as_ref()
                    .map(|capacity| {
                        format!(
                            "{} CPU / {}",
                            capacity.ncpu,
                            human_bytes(capacity.mem_total_bytes)
                        )
                    })
                    .unwrap_or_else(|| "-".to_string()),
                host.free_mem_bytes
                    .map(human_bytes)
                    .unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(
            &[
                "ID",
                "NAME",
                "TRANSPORT",
                "ADDRESS",
                "STATUS",
                "INSTANCES",
                "CAPACITY",
                "FREE MEM"
            ],
            &rows
        )
    );
    for host in &hosts {
        if let Some(error) = &host.last_error {
            println!("  {}: {error}", host.id);
        }
    }
    Ok(())
}

async fn host(client: &Client, json: bool, action: &HostAction) -> Result<(), CliError> {
    match action {
        HostAction::Add(args) => {
            let mut body = json!({ "name": args.name });
            if let Some(id) = &args.id {
                body["id"] = json!(id);
            }
            if let Some(transport) = &args.transport {
                body["transport"] = json!(transport);
            }
            if let Some(address) = &args.address {
                body["address"] = json!(address);
            }
            if let Some(context) = &args.docker_context {
                body["docker_context"] = json!(context);
            }
            let value = client
                .value(Method::POST, "/api/hosts", &[], Some(body))
                .await?;
            if json {
                return print_json(&value);
            }
            let host: HostView = decode(value)?;
            println!(
                "added host {} ({} {}) status={}",
                host.id,
                host.transport,
                if host.address.is_empty() {
                    "-"
                } else {
                    &host.address
                },
                host.status
            );
            if let Some(error) = &host.last_error {
                println!("warning: {error}");
            }
            Ok(())
        }
        HostAction::Test(id) => {
            let path = format!("/api/hosts/{}/test", encode_path_segment(id));
            let value = client.value(Method::POST, &path, &[], None).await?;
            if json {
                return print_json(&value);
            }
            let test: HostTestView = decode(value)?;
            print!("host {id}: {} ({} ms)", test.status, test.latency_ms);
            if let Some(capacity) = &test.capacity {
                print!(
                    ", {} CPU / {}",
                    capacity.ncpu,
                    human_bytes(capacity.mem_total_bytes)
                );
                if let Some(version) = &capacity.docker_version {
                    print!(", docker {version}");
                }
            }
            println!();
            if let Some(error) = &test.error {
                println!("error: {error}");
            }
            if !test.ok {
                return Err(CliError::Config(format!("host \"{id}\" is not reachable")));
            }
            Ok(())
        }
        HostAction::Rm(id) => {
            let path = format!("/api/hosts/{}", encode_path_segment(id));
            client.text(Method::DELETE, &path, &[], None).await?;
            if json {
                return print_json(&json!({ "id": id, "deleted": true }));
            }
            println!("removed host {id}");
            Ok(())
        }
    }
}

async fn instances(client: &Client, json: bool, action: &InstancesAction) -> Result<(), CliError> {
    match action {
        InstancesAction::Move(args) => move_instance(client, json, args).await,
    }
}

async fn move_instance(client: &Client, json: bool, args: &MoveArgs) -> Result<(), CliError> {
    let detail_path = format!("/api/instances/{}", encode_path_segment(&args.id));
    let detail: InstanceDetail = client.json(Method::GET, &detail_path, &[], None).await?;
    let hosts: Vec<HostView> = client.json(Method::GET, "/api/hosts", &[], None).await?;
    let target = hosts
        .iter()
        .find(|host| host.id == args.to || host.name == args.to)
        .ok_or_else(|| {
            CliError::Config(format!(
                "host \"{}\" is not registered; run `fleett hosts`",
                args.to
            ))
        })?;
    if target.id == detail.host {
        return Err(CliError::Config(format!(
            "instance \"{}\" already runs on host \"{}\"",
            args.id, target.id
        )));
    }

    let transport = target.transport.as_str();
    let transfer = transport != "local" && !args.skip_transfer;
    let mut archive_path: Option<PathBuf> = None;
    let remote_archive = format!("/tmp/fleett-move-{}-{}.tgz", args.id, std::process::id());

    if transfer {
        let (remote, port) = split_ssh_target(&target.address);
        if remote.is_empty() {
            return Err(CliError::Config(format!(
                "host \"{}\" has no SSH address",
                target.id
            )));
        }
        let instance_dir = match &args.remote_instance_dir {
            Some(path) => path.clone(),
            None => instance_directory(&detail.data_dir)?,
        };
        validate_remote_instance_dir(&instance_dir)?;
        if !tool_available("scp") || !tool_available("ssh") {
            print_manual_move(&args.id, &target.id, &remote, &instance_dir);
            return Err(CliError::Config(
                "scp and ssh are required to move an instance to a remote host".to_string(),
            ));
        }

        // Stop before exporting so the archive is a consistent snapshot.
        stop_instance(client, &args.id).await?;
        let archive = std::env::temp_dir().join(format!(
            "fleett-move-{}-{}.tgz",
            args.id,
            std::process::id()
        ));
        let bytes = match client
            .bytes(Method::GET, &format!("{detail_path}/export"), &[])
            .await
        {
            Ok(bytes) => bytes,
            Err(err) => {
                let _ = start_instance(client, &args.id).await;
                return Err(err);
            }
        };
        if let Err(err) = std::fs::write(&archive, &bytes) {
            let _ = start_instance(client, &args.id).await;
            return Err(CliError::Config(format!(
                "cannot write the export archive: {err}"
            )));
        }
        archive_path = Some(archive.clone());

        if let Err(detail) = copy_to_host(
            &remote,
            port.as_deref(),
            &archive,
            &remote_archive,
            &instance_dir,
            args.keep_archive,
        ) {
            let _ = start_instance(client, &args.id).await;
            let _ = std::fs::remove_file(&archive);
            print_manual_move(&args.id, &target.id, &remote, &instance_dir);
            return Err(CliError::Config(format!(
                "cannot copy the instance data to {remote}: {detail}"
            )));
        }
    }

    let path = format!("{detail_path}/move");
    let body = json!({ "to": target.id, "start": !args.no_start });
    let moved = client.value(Method::POST, &path, &[], Some(body)).await;
    if let Some(archive) = &archive_path {
        if !args.keep_archive {
            let _ = std::fs::remove_file(archive);
        }
    }
    let value = match moved {
        Ok(value) => value,
        Err(err) => {
            // The server rolls the authorisation back on its own; the copied
            // data stays on the target and can be reused after a retry.
            return Err(err);
        }
    };
    if json {
        return print_json(&value);
    }
    println!(
        "moved instance {} from {} to {}{}",
        args.id,
        detail.host,
        target.id,
        if args.no_start { " (left stopped)" } else { "" }
    );
    if args.keep_archive && archive_path.is_some() {
        println!("archive kept at {remote_archive} (also in the local temp folder)");
    }
    Ok(())
}

async fn stop_instance(client: &Client, id: &str) -> Result<(), CliError> {
    let path = format!("/api/instances/{}/stop", encode_path_segment(id));
    client
        .json::<StatusResponse>(Method::POST, &path, &[], None)
        .await
        .map(|_| ())
}

async fn start_instance(client: &Client, id: &str) -> Result<(), CliError> {
    let path = format!("/api/instances/{}/start", encode_path_segment(id));
    client
        .json::<StatusResponse>(Method::POST, &path, &[], None)
        .await
        .map(|_| ())
}

/// Copy the export archive to the target and extract its `data/` tree at the
/// exact instance directory the compose bind mounts reference.
fn copy_to_host(
    remote: &str,
    port: Option<&str>,
    archive: &Path,
    remote_archive: &str,
    instance_dir: &str,
    keep: bool,
) -> Result<(), String> {
    let mut scp = std::process::Command::new("scp");
    scp.arg("-q");
    if let Some(port) = port {
        scp.arg("-P").arg(port);
    }
    let scp = scp
        .arg(archive)
        .arg(format!("{remote}:{remote_archive}"))
        .status()
        .map_err(|err| format!("cannot run scp: {err}"))?;
    if !scp.success() {
        return Err(format!("scp exited with {scp}"));
    }

    let cleanup = if keep {
        String::new()
    } else {
        format!(" && rm -f {}", shell_quote(remote_archive))
    };
    let command = format!(
        "mkdir -p {dir} && rm -rf {dir}/data && tar -xzf {archive} -C {dir} data && \
         (chown -R 10000:10000 {dir}/data 2>/dev/null || true){cleanup}",
        dir = shell_quote(instance_dir),
        archive = shell_quote(remote_archive),
    );
    let mut ssh = std::process::Command::new("ssh");
    if let Some(port) = port {
        ssh.arg("-p").arg(port);
    }
    let ssh = ssh
        .arg(remote)
        .arg(command)
        .status()
        .map_err(|err| format!("cannot run ssh: {err}"))?;
    if !ssh.success() {
        return Err(format!("ssh exited with {ssh}"));
    }
    Ok(())
}

/// Split an SSH address into its `user@host` part and an optional port.
fn split_ssh_target(address: &str) -> (String, Option<String>) {
    let address = address
        .trim()
        .strip_prefix("ssh://")
        .unwrap_or(address.trim());
    if let Some((host, port)) = address.rsplit_once(':') {
        if !host.is_empty() && !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()) {
            return (host.to_string(), Some(port.to_string()));
        }
    }
    (address.to_string(), None)
}

/// Parent directory of `<root>/instances/<id>/data`.
fn instance_directory(data_dir: &str) -> Result<String, CliError> {
    let path = Path::new(data_dir);
    let parent = path.parent().ok_or_else(|| {
        CliError::Config(format!(
            "cannot derive the instance directory from \"{data_dir}\"; pass \
             --remote-instance-dir"
        ))
    })?;
    Ok(parent.display().to_string())
}

/// Refuse a target directory that the remote `rm -rf` must never touch.
fn validate_remote_instance_dir(path: &str) -> Result<(), CliError> {
    let path = path.trim();
    if !path.starts_with('/') || path == "/" || path.split('/').any(|part| part == "..") {
        return Err(CliError::Config(format!(
            "refusing the remote instance directory \"{path}\": expected an absolute path \
             below /"
        )));
    }
    Ok(())
}

fn tool_available(name: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// POSIX single-quote a value embedded in a remote shell command.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn print_manual_move(id: &str, host: &str, remote: &str, instance_dir: &str) {
    println!(
        "The target host \"{host}\" is remote: its Docker daemon resolves bind mounts on its \
         own filesystem."
    );
    println!("Transfer the data yourself, then run this move again with --skip-transfer:");
    println!("  1. fleett stop {id}");
    println!(
        "  2. curl -fsS -H \"Authorization: Bearer $FLEETT_TOKEN\" \
         \"$FLEETT_URL/api/instances/{id}/export\" -o /tmp/{id}.tgz"
    );
    println!("  3. scp /tmp/{id}.tgz {remote}:/tmp/{id}.tgz");
    println!(
        "  4. ssh {remote} 'mkdir -p {dir} && tar -xzf /tmp/{id}.tgz -C {dir} data && \
         chown -R 10000:10000 {dir}/data'",
        dir = shell_quote(instance_dir)
    );
    println!("  5. fleett instances move {id} --to {host} --skip-transfer");
}

async fn upgrade(client: &Client, json: bool, args: &UpgradeArgs) -> Result<(), CliError> {
    let body = json!({
        "image": args.image,
        "parallel": args.parallel,
        "dry_run": args.dry_run,
        "timeout_s": args.timeout_s,
    });
    let value = client
        .value(Method::POST, "/api/upgrade", &[], Some(body))
        .await?;
    let mut job: UpgradeJobView = decode(value)?;

    if json {
        // The machine-readable output is always the final job.
        while !job.finished() {
            tokio::time::sleep(UPGRADE_POLL_INTERVAL).await;
            job = fetch_upgrade(client).await?;
        }
        print_json(&job)?;
        return upgrade_status(&job);
    }

    println!(
        "upgrade {}: {} instance(s) to {}{}",
        job.id,
        job.results.len(),
        job.image,
        if job.dry_run { " (dry run)" } else { "" }
    );
    if job.dry_run {
        for result in &job.results {
            println!(
                "  {:<16} {} -> {}",
                result.id,
                result.from.as_deref().unwrap_or("-"),
                result.to
            );
        }
        return Ok(());
    }

    // Print a line every time an instance changes state, then the final table.
    let mut seen: Vec<(String, String)> = job
        .results
        .iter()
        .map(|result| (result.id.clone(), result.status.clone()))
        .collect();
    while !job.finished() {
        tokio::time::sleep(UPGRADE_POLL_INTERVAL).await;
        job = fetch_upgrade(client).await?;
        for result in &job.results {
            if let Some(previous) = seen
                .iter_mut()
                .find(|(id, _)| id == &result.id)
                .map(|(_, status)| status)
            {
                if *previous != result.status {
                    println!("  {:<16} {}", result.id, result.status);
                    *previous = result.status.clone();
                }
            }
        }
    }
    println!();
    print_upgrade_table(&job);
    upgrade_status(&job)
}

async fn fetch_upgrade(client: &Client) -> Result<UpgradeJobView, CliError> {
    let value = client.value(Method::GET, "/api/upgrade", &[], None).await?;
    decode(value)
}

fn print_upgrade_table(job: &UpgradeJobView) {
    let rows: Vec<Vec<String>> = job
        .results
        .iter()
        .map(|result| {
            vec![
                result.id.clone(),
                result.from.clone().unwrap_or_else(|| "-".to_string()),
                result.to.clone(),
                result.status.clone(),
            ]
        })
        .collect();
    print!("{}", render_table(&["ID", "FROM", "TO", "STATUS"], &rows));
    for result in &job.results {
        if let Some(error) = &result.error {
            println!("  {}: {error}", result.id);
        }
    }
}

fn upgrade_status(job: &UpgradeJobView) -> Result<(), CliError> {
    if job.needs_attention() {
        return Err(CliError::UpgradeIncomplete);
    }
    Ok(())
}

fn short_id(id: &str) -> String {
    if id.is_empty() {
        return "-".to_string();
    }
    // `sha256:` plus the first 12 hex characters, like `docker ps`.
    let digest = id.strip_prefix("sha256:").unwrap_or(id);
    digest.chars().take(12).collect()
}

// ---------------------------------------------------------------------------
// Watchdog: incidents and heal policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
struct IncidentView {
    id: String,
    instance_id: String,
    cause: String,
    status: String,
    opened_at: String,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    restart_count: u32,
    #[serde(default)]
    actions: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct HealView {
    id: String,
    heal: String,
    unhealthy_after: u32,
    max_restarts: u32,
    heal_backoff_s: u64,
}

async fn incidents(client: &Client, json: bool, args: &IncidentsArgs) -> Result<(), CliError> {
    let mut query: Vec<(&str, String)> = vec![("limit", args.limit.to_string())];
    if let Some(status) = args.status.as_deref().filter(|value| !value.is_empty()) {
        query.push(("status", status.to_string()));
    }
    if let Some(instance) = args.instance.as_deref().filter(|value| !value.is_empty()) {
        query.push(("instance", instance.to_string()));
    }
    let value = client
        .value(Method::GET, "/api/incidents", &query, None)
        .await?;
    if json {
        return print_json(&value);
    }
    let incidents: Vec<IncidentView> = decode(value)?;
    if incidents.is_empty() {
        println!("no incidents");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = incidents
        .iter()
        .map(|incident| {
            vec![
                incident.opened_at.clone(),
                incident.instance_id.clone(),
                incident.cause.clone(),
                incident.status.clone(),
                incident.restart_count.to_string(),
                incident.detail.replace('\n', " "),
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(
            &["OPENED", "INSTANCE", "CAUSE", "STATUS", "RESTARTS", "DETAIL"],
            &rows
        )
    );
    Ok(())
}

async fn heal(client: &Client, json: bool, args: &HealArgs) -> Result<(), CliError> {
    let mut body = json!({ "heal": args.policy });
    let object = body.as_object_mut().expect("object");
    if let Some(unhealthy_after) = args.unhealthy_after {
        object.insert("unhealthy_after".to_string(), json!(unhealthy_after));
    }
    if let Some(max_restarts) = args.max_restarts {
        object.insert("max_restarts".to_string(), json!(max_restarts));
    }
    if let Some(backoff_s) = args.backoff_s {
        object.insert("heal_backoff_s".to_string(), json!(backoff_s));
    }
    let path = format!("/api/instances/{}/heal", encode_path_segment(&args.id));
    let value = client.value(Method::PATCH, &path, &[], Some(body)).await?;
    if json {
        return print_json(&value);
    }
    let view: HealView = decode(value)?;
    println!(
        "{}: heal={} unhealthy_after={} max_restarts={} backoff={}s",
        view.id, view.heal, view.unhealthy_after, view.max_restarts, view.heal_backoff_s
    );
    Ok(())
}

async fn incident_close(client: &Client, json: bool, id: &str) -> Result<(), CliError> {
    let path = format!("/api/incidents/{}/close", encode_path_segment(id));
    let value = client.value(Method::POST, &path, &[], None).await?;
    if json {
        return print_json(&value);
    }
    let incident: IncidentView = decode(value)?;
    println!(
        "closed incident {} ({} on {})",
        incident.id, incident.cause, incident.instance_id
    );
    Ok(())
}

async fn doctor(client: &Client, json: bool, url: &str) -> Result<(), CliError> {
    let checks = run_doctor(client).await;
    let failed = checks.iter().any(|check| check.state == CheckState::Fail);
    if json {
        print_json(&json!({
            "url": url,
            "ok": !failed,
            "checks": checks,
        }))?;
        return if failed {
            Err(CliError::ChecksFailed)
        } else {
            Ok(())
        };
    }

    println!("fleett doctor {url}");
    println!();
    for check in &checks {
        println!(
            "  {:<5} {:<8} {}",
            check.state.label(),
            check.name,
            check.detail
        );
    }
    if failed {
        println!();
        println!("one or more checks failed");
        return Err(CliError::ChecksFailed);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CheckState {
    Ok,
    Warn,
    Fail,
}

impl CheckState {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct Check {
    name: &'static str,
    state: CheckState,
    detail: String,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            state: CheckState::Ok,
            detail: detail.into(),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            state: CheckState::Warn,
            detail: detail.into(),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            state: CheckState::Fail,
            detail: detail.into(),
        }
    }
}

async fn run_doctor(client: &Client) -> Vec<Check> {
    let mut checks = Vec::new();

    // The API and the Docker engine are reported by the health endpoint; an
    // unreachable control plane is the only check that can run.
    let health: Health = match client
        .json::<Health>(Method::GET, "/api/health", &[], None)
        .await
    {
        Ok(health) => {
            checks.push(Check::ok(
                "api",
                format!("fleett {} ({})", health.version, health.driver),
            ));
            health
        }
        Err(err) => {
            checks.push(Check::fail("api", err.to_string()));
            return checks;
        }
    };

    match client
        .json::<Me>(Method::GET, "/api/auth/me", &[], None)
        .await
    {
        Ok(me) => {
            let actor = if me.actor.is_empty() {
                "authenticated".to_string()
            } else {
                me.actor
            };
            let role = if me.role.is_empty() {
                "unknown role".to_string()
            } else {
                format!("role {}", me.role)
            };
            let method = if me.method.is_empty() {
                String::new()
            } else {
                format!(" via {}", me.method)
            };
            checks.push(Check::ok("auth", format!("{actor}, {role}{method}")));
        }
        Err(err) => checks.push(Check::fail("auth", err.to_string())),
    }

    if health.docker.ok {
        checks.push(Check::ok("docker", "the docker engine answers"));
    } else {
        checks.push(Check::fail(
            "docker",
            "the docker engine is unreachable; containers cannot be managed",
        ));
    }

    match &health.secrets {
        Some(secrets) if secrets.key => {
            let mut detail = format!(
                "key present, {} encrypted, {} plaintext",
                secrets.encrypted, secrets.plaintext
            );
            if secrets.plaintext > 0 {
                detail.push_str("; run `fleett secrets migrate --apply`");
            }
            checks.push(Check::ok("secrets", detail));
        }
        Some(secrets) => {
            let detail = secrets.key_error.as_deref().unwrap_or(
                "no encryption key configured; run `sudo fleett secrets init` on the host",
            );
            checks.push(Check::fail("secrets", detail));
        }
        None => checks.push(Check::warn(
            "secrets",
            "this server does not report the secrets state",
        )),
    }

    match &health.egress {
        Some(egress) if egress.mode == "off" => checks.push(Check::warn(
            "egress",
            "FLEETT_EGRESS=off: agents are not isolated from the LAN",
        )),
        Some(egress) if egress.marker => {
            checks.push(Check::ok("egress", "restrict, firewall marker present"));
        }
        Some(_) => checks.push(Check::fail(
            "egress",
            "the firewall marker is missing; run `sudo fleett egress install` on the host",
        )),
        None => checks.push(Check::warn(
            "egress",
            "this server does not report the egress state",
        )),
    }

    // Instance warnings (missing allowlists, plaintext secrets, LAN egress):
    // one `warn` line per warning, so the operator sees the same message as
    // the console without opening the instance. A clean fleet adds no line.
    match client
        .json::<Vec<InstanceSummary>>(Method::GET, "/api/instances", &[], None)
        .await
    {
        Ok(instances) => {
            for instance in instances {
                let path = format!("/api/instances/{}", instance.id);
                let Ok(detail) = client
                    .json::<InstanceDetail>(Method::GET, &path, &[], None)
                    .await
                else {
                    continue;
                };
                for warning in detail.warnings {
                    checks.push(Check::warn(
                        "warnings",
                        format!("{}: {warning}", instance.id),
                    ));
                }
            }
        }
        Err(err) => checks.push(Check::warn(
            "warnings",
            format!("the instance list is unreadable: {err}"),
        )),
    }

    checks
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct TemplateEnvView {
    key: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    secret: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct TemplateInfoView {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    default_model: String,
    #[serde(default)]
    channel: String,
    #[serde(default)]
    env: Vec<TemplateEnvView>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    instances_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct TemplateDetailView {
    #[serde(flatten)]
    info: TemplateInfoView,
    #[serde(default)]
    config_yaml: String,
    #[serde(default)]
    soul_md: Option<String>,
    #[serde(default)]
    versions_count: usize,
    #[serde(default)]
    defaults: Value,
    #[serde(default)]
    source_instance: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TemplateVersionView {
    version: i64,
    action: String,
    actor: String,
    at: String,
}

async fn templates_command(client: &Client, json: bool) -> Result<(), CliError> {
    let value = client
        .value(Method::GET, "/api/templates", &[], None)
        .await?;
    if json {
        return print_json(&value);
    }
    let templates: Vec<TemplateInfoView> = decode(value)?;
    let rows: Vec<Vec<String>> = templates
        .iter()
        .map(|template| {
            vec![
                template.id.clone(),
                template.name.clone(),
                template.parent.clone().unwrap_or_else(|| "-".to_string()),
                if template.read_only {
                    "yes".to_string()
                } else {
                    "no".to_string()
                },
                template.channel.clone(),
                template.default_model.clone(),
                template.env.len().to_string(),
                template.instances_count.to_string(),
            ]
        })
        .collect();
    print!(
        "{}",
        render_table(
            &[
                "ID",
                "NAME",
                "PARENT",
                "READ-ONLY",
                "CHANNEL",
                "MODEL",
                "ENV",
                "INSTANCES"
            ],
            &rows
        )
    );
    Ok(())
}

async fn template_command(
    client: &Client,
    json: bool,
    action: &TemplateCommand,
) -> Result<(), CliError> {
    match action {
        TemplateCommand::List => templates_command(client, json).await,
        TemplateCommand::Get(id) => {
            let value = client
                .value(
                    Method::GET,
                    &format!("/api/templates/{}", encode_path_segment(id)),
                    &[],
                    None,
                )
                .await?;
            print_template_detail(&value, json)
        }
        TemplateCommand::Create(args) => create_template(client, json, args).await,
        TemplateCommand::FromInstance { instance, id, name } => {
            let mut body = json!({"instance_id": instance, "id": id});
            if let Some(name) = name {
                body["name"] = json!(name);
            }
            let value = client
                .value(
                    Method::POST,
                    "/api/templates/from-instance",
                    &[],
                    Some(body),
                )
                .await?;
            print_template_detail(&value, json)
        }
        TemplateCommand::Remove(id) => {
            client
                .text(
                    Method::DELETE,
                    &format!("/api/templates/{}", encode_path_segment(id)),
                    &[],
                    None,
                )
                .await?;
            if json {
                print_json(&json!({"id": id, "deleted": true}))
            } else {
                println!("template {id} deleted");
                Ok(())
            }
        }
        TemplateCommand::Versions(id) => {
            let value = client
                .value(
                    Method::GET,
                    &format!("/api/templates/{}/versions", encode_path_segment(id)),
                    &[],
                    None,
                )
                .await?;
            if json {
                return print_json(&value);
            }
            let versions: Vec<TemplateVersionView> = decode(value)?;
            let rows: Vec<Vec<String>> = versions
                .iter()
                .map(|version| {
                    vec![
                        version.version.to_string(),
                        version.action.clone(),
                        version.actor.clone(),
                        version.at.clone(),
                    ]
                })
                .collect();
            print!(
                "{}",
                render_table(&["VERSION", "ACTION", "ACTOR", "AT"], &rows)
            );
            Ok(())
        }
        TemplateCommand::Restore { id, version } => {
            let value = client
                .value(
                    Method::POST,
                    &format!("/api/templates/{}/restore", encode_path_segment(id)),
                    &[],
                    Some(json!({ "version": version })),
                )
                .await?;
            print_template_detail(&value, json)
        }
    }
}

async fn create_template(
    client: &Client,
    json: bool,
    args: &TemplateCreateArgs,
) -> Result<(), CliError> {
    let body = match &args.file {
        Some(path) => read_yaml_json(path)?,
        None => {
            let id = args.id.clone().ok_or_else(|| {
                CliError::Config("--id is required without -f <file.yaml>".to_string())
            })?;
            let mut body = json!({ "id": id });
            if let Some(name) = &args.name {
                body["name"] = json!(name);
            }
            if let Some(description) = &args.description {
                body["description"] = json!(description);
            }
            if let Some(parent) = &args.parent {
                body["parent"] = json!(parent);
            }
            if let Some(model) = &args.default_model {
                body["default_model"] = json!(model);
            }
            if let Some(channel) = &args.channel {
                body["channel"] = json!(channel);
            }
            if let Some(path) = &args.config {
                body["config_yaml"] = json!(read_text(path)?);
            }
            if let Some(path) = &args.soul {
                body["soul_md"] = json!(read_text(path)?);
            }
            if let Some(path) = &args.defaults {
                body["defaults"] = read_yaml_json(path)?;
            }
            if !args.env.is_empty() {
                let env: Vec<Value> = args
                    .env
                    .iter()
                    .map(|key| {
                        json!({
                            "key": key,
                            "description": "",
                            "required": false,
                            "secret": false,
                        })
                    })
                    .collect();
                body["env"] = json!(env);
            }
            body
        }
    };
    let value = client
        .value(Method::POST, "/api/templates", &[], Some(body))
        .await?;
    print_template_detail(&value, json)
}

fn read_text(path: &Path) -> Result<String, CliError> {
    std::fs::read_to_string(path)
        .map_err(|err| CliError::Config(format!("cannot read {}: {err}", path.display())))
}

fn read_yaml_json(path: &Path) -> Result<Value, CliError> {
    let text = read_text(path)?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(&text)
        .map_err(|err| CliError::Config(format!("invalid YAML in {}: {err}", path.display())))?;
    serde_json::to_value(&yaml).map_err(|err| {
        CliError::Config(format!("cannot convert {} to JSON: {err}", path.display()))
    })
}

fn print_template_detail(value: &Value, json: bool) -> Result<(), CliError> {
    if json {
        return print_json(value);
    }
    let detail: TemplateDetailView = decode(value.clone())?;
    println!("{} ({})", detail.info.name, detail.info.id);
    if let Some(parent) = &detail.info.parent {
        println!("  parent: {parent}");
    }
    if detail.info.read_only {
        println!("  read-only: yes (built-in)");
    }
    if !detail.info.description.is_empty() {
        println!("  description: {}", detail.info.description);
    }
    println!("  channel: {}", detail.info.channel);
    println!("  model: {}", detail.info.default_model);
    println!("  instances: {}", detail.info.instances_count);
    println!("  versions: {}", detail.versions_count);
    println!("  config.yaml: {} bytes", detail.config_yaml.len());
    if let Some(soul) = &detail.soul_md {
        println!("  soul.md: {} bytes", soul.len());
    }
    if let Some(source) = &detail.source_instance {
        println!("  source instance: {source}");
    }
    if !detail.info.env.is_empty() {
        let rows: Vec<Vec<String>> = detail
            .info
            .env
            .iter()
            .map(|entry| {
                vec![
                    entry.key.clone(),
                    if entry.required {
                        "yes".to_string()
                    } else {
                        "-".to_string()
                    },
                    if entry.secret {
                        "yes".to_string()
                    } else {
                        "no".to_string()
                    },
                    entry.description.clone(),
                ]
            })
            .collect();
        println!();
        print!(
            "{}",
            render_table(&["ENV", "REQUIRED", "SECRET", "DESCRIPTION"], &rows)
        );
    }
    if detail
        .defaults
        .as_object()
        .is_some_and(|object| !object.is_empty())
    {
        println!();
        println!("defaults:");
        let text = serde_yaml::to_string(&detail.defaults)
            .map_err(|err| CliError::Decode(err.to_string()))?;
        print!("{text}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Marketplace
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceEntryView {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    commit: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceSourceView {
    source: String,
    status: String,
    #[serde(default)]
    count: usize,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceSearchView {
    #[serde(default)]
    entries: Vec<MarketplaceEntryView>,
    #[serde(default)]
    sources: Vec<MarketplaceSourceView>,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplacePreviewFileView {
    path: String,
    #[serde(default)]
    size: usize,
    #[serde(default)]
    script: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplacePreviewView {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    files: Vec<MarketplacePreviewFileView>,
    #[serde(default)]
    total_bytes: usize,
    #[serde(default)]
    skill_md: String,
    #[serde(default)]
    scripts: Vec<String>,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    requires_confirmation: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceFileChangeView {
    path: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplacePromoteResultView {
    id: String,
    status: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplacePromoteView {
    latest: String,
    #[serde(default)]
    results: Vec<MarketplacePromoteResultView>,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceUpdateView {
    name: String,
    from: String,
    to: String,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    files: Vec<MarketplaceFileChangeView>,
    #[serde(default)]
    promoted: Option<MarketplacePromoteView>,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceUpdateInfoView {
    name: String,
    installed_version: String,
    #[serde(default)]
    upstream_version: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    source: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceUpdatesView {
    #[serde(default)]
    updates: Vec<MarketplaceUpdateInfoView>,
    #[serde(default)]
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceSyncView {
    skills: usize,
    source: String,
    #[serde(default)]
    commit: Option<String>,
}

/// Shorten a cell to keep the search table readable.
fn ellipsis(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>()
        + "…"
}

async fn marketplace(
    client: &Client,
    json: bool,
    command: &MarketplaceCommand,
) -> Result<(), CliError> {
    match command {
        MarketplaceCommand::Search {
            query,
            source,
            limit,
        } => marketplace_search(client, json, query, source.as_deref(), *limit).await,
        MarketplaceCommand::Preview { source, reference } => {
            marketplace_preview(client, json, source, reference.as_deref()).await
        }
        MarketplaceCommand::Install {
            source,
            reference,
            confirm_scripts,
            name,
        } => {
            marketplace_install(
                client,
                json,
                source,
                reference.as_deref(),
                *confirm_scripts,
                name.as_deref(),
            )
            .await
        }
        MarketplaceCommand::Update {
            name,
            promote,
            confirm_scripts,
        } => marketplace_update(client, json, name, *promote, *confirm_scripts).await,
        MarketplaceCommand::Updates => marketplace_updates(client, json).await,
        MarketplaceCommand::Sync => marketplace_sync(client, json).await,
    }
}

async fn marketplace_search(
    client: &Client,
    json: bool,
    query: &str,
    source: Option<&str>,
    limit: usize,
) -> Result<(), CliError> {
    let mut params = vec![("q", query.to_string()), ("limit", limit.to_string())];
    if let Some(source) = source {
        params.push(("source", source.to_string()));
    }
    let value = client
        .value(Method::GET, "/api/marketplace/skills", &params, None)
        .await?;
    if json {
        return print_json(&value);
    }
    let search: MarketplaceSearchView = decode(value)?;
    if search.entries.is_empty() {
        println!("no skill found");
    } else {
        let rows: Vec<Vec<String>> = search
            .entries
            .iter()
            .map(|entry| {
                vec![
                    entry.name.clone(),
                    entry.provider.clone(),
                    ellipsis(&entry.description, 48),
                    entry.source.clone().unwrap_or_else(|| "-".to_string()),
                    entry.version.clone().unwrap_or_else(|| "-".to_string()),
                    entry
                        .commit
                        .as_deref()
                        .map(|commit| commit.chars().take(7).collect())
                        .unwrap_or_else(|| "-".to_string()),
                ]
            })
            .collect();
        print!(
            "{}",
            render_table(
                &[
                    "NAME",
                    "PROVIDER",
                    "DESCRIPTION",
                    "SOURCE",
                    "VERSION",
                    "COMMIT"
                ],
                &rows
            )
        );
    }
    if !search.sources.is_empty() {
        println!();
        for source in &search.sources {
            match &source.message {
                Some(message) => println!(
                    "{}: {} ({} entries) — {}",
                    source.source, source.status, source.count, message
                ),
                None => println!(
                    "{}: {} ({} entries)",
                    source.source, source.status, source.count
                ),
            }
        }
    }
    Ok(())
}

async fn marketplace_preview(
    client: &Client,
    json: bool,
    source: &str,
    reference: Option<&str>,
) -> Result<(), CliError> {
    let value = client
        .value(
            Method::POST,
            "/api/marketplace/skills/preview",
            &[],
            Some(json!({ "source": source, "ref": reference })),
        )
        .await?;
    if json {
        return print_json(&value);
    }
    let preview: MarketplacePreviewView = decode(value)?;
    println!("name: {}", preview.name);
    if !preview.description.is_empty() {
        println!("description: {}", preview.description);
    }
    if let Some(version) = &preview.version {
        println!("upstream version: {version}");
    }
    if let Some(author) = &preview.author {
        println!("author: {author}");
    }
    if let Some(commit) = &preview.commit {
        println!("commit: {commit}");
    }
    if let Some(source) = &preview.source {
        println!("source: {source}");
    }
    println!(
        "files: {} ({} total)",
        preview.files.len(),
        human_bytes(preview.total_bytes as u64)
    );
    let rows: Vec<Vec<String>> = preview
        .files
        .iter()
        .map(|file| {
            vec![
                file.path.clone(),
                human_bytes(file.size as u64),
                if file.script {
                    "script".to_string()
                } else {
                    "-".to_string()
                },
            ]
        })
        .collect();
    print!("{}", render_table(&["PATH", "SIZE", "KIND"], &rows));
    if preview.requires_confirmation {
        println!();
        println!(
            "this skill contains {} script(s); re-run with --confirm-scripts to install it:",
            preview.scripts.len()
        );
        for script in &preview.scripts {
            println!("  {script}");
        }
    }
    if !preview.skill_md.is_empty() {
        println!();
        println!("--- SKILL.md ---");
        print!("{}", preview.skill_md);
        if !preview.skill_md.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

async fn marketplace_install(
    client: &Client,
    json: bool,
    source: &str,
    reference: Option<&str>,
    confirm_scripts: bool,
    name: Option<&str>,
) -> Result<(), CliError> {
    let mut body = json!({
        "source": source,
        "ref": reference,
        "confirm_scripts": confirm_scripts,
    });
    if let Some(name) = name {
        body["name"] = json!(name);
    }
    let value = client
        .value(
            Method::POST,
            "/api/marketplace/skills/install",
            &[],
            Some(body),
        )
        .await?;
    if json {
        return print_json(&value);
    }
    print_skill_summary(&value);
    Ok(())
}

fn print_skill_summary(value: &Value) {
    let name = value["name"].as_str().unwrap_or_default();
    let latest = value["latest"].as_str().unwrap_or("-");
    let files = value["files"].as_array().map(Vec::len).unwrap_or(0);
    println!("skill {name} installed as {latest} ({files} files)");
    if let Some(source) = value["source"].as_str() {
        println!("  source: {source}");
    }
    if let Some(commit) = value["commit"].as_str() {
        println!("  commit: {commit}");
    }
}

async fn marketplace_update(
    client: &Client,
    json: bool,
    name: &str,
    promote: bool,
    confirm_scripts: bool,
) -> Result<(), CliError> {
    let value = client
        .value(
            Method::POST,
            &format!(
                "/api/marketplace/skills/{}/update",
                encode_path_segment(name)
            ),
            &[],
            Some(json!({
                "promote": promote,
                "confirm_scripts": confirm_scripts,
            })),
        )
        .await?;
    if json {
        return print_json(&value);
    }
    let update: MarketplaceUpdateView = decode(value)?;
    println!(
        "{}: {} → {}{}",
        update.name,
        update.from,
        update.to,
        update
            .commit
            .as_ref()
            .map(|commit| format!(" (commit {commit})"))
            .unwrap_or_default()
    );
    if update.files.is_empty() {
        println!("no file change");
    } else {
        for file in &update.files {
            println!("  {:<9} {}", file.status, file.path);
        }
    }
    if let Some(promoted) = &update.promoted {
        println!("latest moved to {}", promoted.latest);
        for result in &promoted.results {
            match &result.error {
                Some(error) => println!("  {}: {} ({error})", result.id, result.status),
                None => println!("  {}: {}", result.id, result.status),
            }
        }
    }
    Ok(())
}

async fn marketplace_updates(client: &Client, json: bool) -> Result<(), CliError> {
    let value = client
        .value(Method::GET, "/api/marketplace/updates", &[], None)
        .await?;
    if json {
        return print_json(&value);
    }
    let updates: MarketplaceUpdatesView = decode(value)?;
    if updates.updates.is_empty() {
        println!("every installed skill is up to date");
    } else {
        let rows: Vec<Vec<String>> = updates
            .updates
            .iter()
            .map(|update| {
                vec![
                    update.name.clone(),
                    update.installed_version.clone(),
                    update
                        .upstream_version
                        .clone()
                        .unwrap_or_else(|| "-".to_string()),
                    update.commit.clone().unwrap_or_else(|| "-".to_string()),
                    update.source.clone(),
                ]
            })
            .collect();
        print!(
            "{}",
            render_table(
                &["NAME", "INSTALLED", "UPSTREAM", "COMMIT", "SOURCE"],
                &rows
            )
        );
    }
    for warning in &updates.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

async fn marketplace_sync(client: &Client, json: bool) -> Result<(), CliError> {
    let value = client
        .value(Method::POST, "/api/marketplace/sync", &[], Some(json!({})))
        .await?;
    if json {
        return print_json(&value);
    }
    let sync: MarketplaceSyncView = decode(value)?;
    println!(
        "indexed {} skills from {}{}",
        sync.skills,
        sync.source,
        sync.commit
            .as_ref()
            .map(|commit| format!(" (commit {commit})"))
            .unwrap_or_default()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Plain aligned text table (no TUI dependency).
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| header.chars().count())
        .collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if index < widths.len() {
                widths[index] = widths[index].max(cell.chars().count());
            }
        }
    }
    let mut out = String::new();
    let header: Vec<String> = headers.iter().map(|header| header.to_string()).collect();
    out.push_str(&table_line(&widths, &header));
    for row in rows {
        out.push_str(&table_line(&widths, row));
    }
    out
}

fn table_line(widths: &[usize], cells: &[String]) -> String {
    let mut line = String::new();
    for (index, width) in widths.iter().enumerate() {
        let cell = cells.get(index).map(String::as_str).unwrap_or("");
        line.push_str(cell);
        if index + 1 < widths.len() {
            line.push_str(&" ".repeat(width.saturating_sub(cell.chars().count()) + 2));
        }
    }
    line.push('\n');
    line
}

/// `1536` → `1.5 KiB` (binary units, like `docker stats`).
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `93784` → `1d2h`.
pub fn human_uptime(seconds: i64) -> String {
    if seconds < 0 {
        return "-".to_string();
    }
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

fn caps_line(caps: &UsageCaps) -> String {
    let cpu = if caps.max_cpu <= 0.0 {
        "unlimited".to_string()
    } else {
        format!("{:.1}", caps.max_cpu)
    };
    let memory = if caps.max_mem_mb == 0 {
        "unlimited".to_string()
    } else {
        format!("{} MB", caps.max_mem_mb)
    };
    format!("caps: cpu {cpu}, memory {memory}")
}

/// Percent-encode one path segment (instance ids are already safe).
pub(crate) fn encode_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// MCP marketplace (issue #62)
// ---------------------------------------------------------------------------

/// `fleett mcp-server <action>`: catalogue, deployment, discovery and attach.
async fn mcp_server(
    client: &Client,
    json: bool,
    command: &McpServerCommand,
) -> Result<(), CliError> {
    match command {
        McpServerCommand::List => {
            let value = client
                .value(Method::GET, "/api/mcp/servers", &[], None)
                .await?;
            if json {
                return print_json(&value);
            }
            let servers: Vec<McpServerSummary> = decode(value)?;
            let rows: Vec<Vec<String>> = servers
                .iter()
                .map(|server| {
                    vec![
                        server.id.clone(),
                        server.kind.clone(),
                        server.status.clone(),
                        server.health.clone(),
                        server.tools_count.to_string(),
                        server.instances.join(","),
                        server.endpoint.clone().unwrap_or_else(|| "-".to_string()),
                    ]
                })
                .collect();
            print!(
                "{}",
                render_table(
                    &[
                        "ID",
                        "KIND",
                        "STATUS",
                        "HEALTH",
                        "TOOLS",
                        "INSTANCES",
                        "ENDPOINT"
                    ],
                    &rows
                )
            );
            Ok(())
        }
        McpServerCommand::Get(id) => {
            let path = format!("/api/mcp/servers/{}", encode_path_segment(id));
            let value = client.value(Method::GET, &path, &[], None).await?;
            if json {
                return print_json(&value);
            }
            let server: McpServerDetail = decode(value)?;
            println!("{}  {}  {}", server.id, server.kind, server.status);
            if !server.description.is_empty() {
                println!("{}", server.description);
            }
            println!(
                "health: {}  tools: {}  host: {}",
                server.health, server.tools_count, server.host
            );
            if let Some(endpoint) = &server.endpoint {
                println!("endpoint: {endpoint}");
            }
            if !server.env.is_empty() {
                println!("env:");
                for variable in &server.env {
                    println!(
                        "  {} ({}){}",
                        variable.key,
                        if variable.secret { "secret" } else { "plain" },
                        if variable.set { " set" } else { " missing" }
                    );
                }
            }
            if !server.instances.is_empty() {
                println!("instances: {}", server.instances.join(", "));
            }
            Ok(())
        }
        McpServerCommand::Catalog { q } => {
            let query: Vec<(&str, String)> = q
                .as_deref()
                .map(|value| vec![("q", value.to_string())])
                .unwrap_or_default();
            let value = client
                .value(Method::GET, "/api/mcp/catalog", &query, None)
                .await?;
            if json {
                return print_json(&value);
            }
            let entries: Vec<CatalogSummary> = decode(value)?;
            let rows: Vec<Vec<String>> = entries
                .iter()
                .map(|entry| {
                    vec![
                        entry.id.clone(),
                        entry.kind.clone(),
                        entry.image.clone(),
                        entry.tags.join(","),
                    ]
                })
                .collect();
            print!("{}", render_table(&["ID", "KIND", "IMAGE", "TAGS"], &rows));
            Ok(())
        }
        McpServerCommand::Import { file } => {
            let text = std::fs::read_to_string(file).map_err(|err| {
                CliError::Config(format!("cannot read {}: {err}", file.display()))
            })?;
            let body: Value = serde_json::from_str(&text).map_err(|err| {
                CliError::Config(format!("{} is not valid JSON: {err}", file.display()))
            })?;
            let value = client
                .value(Method::POST, "/api/mcp/catalog/import", &[], Some(body))
                .await?;
            if json {
                return print_json(&value);
            }
            println!(
                "imported catalogue entry {}",
                value["id"].as_str().unwrap_or_default()
            );
            Ok(())
        }
        McpServerCommand::Deploy(args) => deploy_mcp(client, json, args).await,
        McpServerCommand::Start(id) => mcp_lifecycle(client, json, id, "start").await,
        McpServerCommand::Stop(id) => mcp_lifecycle(client, json, id, "stop").await,
        McpServerCommand::Restart(id) => mcp_lifecycle(client, json, id, "restart").await,
        McpServerCommand::Remove { id, purge } => {
            let path = format!("/api/mcp/servers/{}", encode_path_segment(id));
            let query = if *purge {
                vec![("purge", "true".to_string())]
            } else {
                Vec::new()
            };
            let _ = client.value(Method::DELETE, &path, &query, None).await?;
            if json {
                return print_json(&json!({ "id": id, "deleted": true }));
            }
            println!("deleted MCP server {id}");
            Ok(())
        }
        McpServerCommand::Logs { id, tail } => {
            let path = format!("/api/mcp/servers/{}/logs", encode_path_segment(id));
            let query = vec![("tail", tail.to_string())];
            let text = client.text(Method::GET, &path, &query, None).await?;
            print!("{text}");
            Ok(())
        }
        McpServerCommand::Test(id) => {
            let path = format!("/api/mcp/servers/{}/test", encode_path_segment(id));
            let value = client.value(Method::POST, &path, &[], None).await?;
            if json {
                return print_json(&value);
            }
            println!(
                "{id}: {} tools in {} ms (protocol {})",
                value["tools_count"].as_u64().unwrap_or_default(),
                value["latency_ms"].as_u64().unwrap_or_default(),
                value["protocol_version"].as_str().unwrap_or("?"),
            );
            if let Some(tools) = value["tools"].as_array() {
                for tool in tools {
                    println!(
                        "  {}{}",
                        tool["name"].as_str().unwrap_or("?"),
                        tool["description"]
                            .as_str()
                            .filter(|description| !description.is_empty())
                            .map(|description| format!(" — {description}"))
                            .unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
        McpServerCommand::Tools(id) => {
            let path = format!("/api/mcp/servers/{}/tools", encode_path_segment(id));
            let value = client.value(Method::GET, &path, &[], None).await?;
            if json {
                return print_json(&value);
            }
            for tool in value["tools"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{}{}",
                    tool["name"].as_str().unwrap_or("?"),
                    tool["description"]
                        .as_str()
                        .filter(|description| !description.is_empty())
                        .map(|description| format!(" — {description}"))
                        .unwrap_or_default()
                );
            }
            Ok(())
        }
        McpServerCommand::Attach {
            instance,
            server,
            mode,
        } => {
            let path = format!(
                "/api/instances/{}/mcp-servers/{}",
                encode_path_segment(instance),
                encode_path_segment(server)
            );
            let body = mode.as_ref().map(|mode| json!({ "mode": mode }));
            let value = client.value(Method::POST, &path, &[], body).await?;
            if json {
                return print_json(&value);
            }
            println!(
                "{instance}: {} ({})",
                value["status"].as_str().unwrap_or("?"),
                server
            );
            Ok(())
        }
        McpServerCommand::Detach { instance, server } => {
            let path = format!(
                "/api/instances/{}/mcp-servers/{}",
                encode_path_segment(instance),
                encode_path_segment(server)
            );
            let value = client.value(Method::DELETE, &path, &[], None).await?;
            if json {
                return print_json(&value);
            }
            println!(
                "{instance}: {} (detached {server})",
                value["status"].as_str().unwrap_or("?")
            );
            Ok(())
        }
    }
}

async fn deploy_mcp(client: &Client, json: bool, args: &DeployMcpArgs) -> Result<(), CliError> {
    // A bare id that exists in the catalogue deploys that entry; otherwise the
    // explicit `--image` path builds a one-off server. This makes
    // `fleett mcp-server deploy fetch` do the obvious thing.
    let catalog = match args.catalog.as_deref() {
        Some(id) => Some(id.to_string()),
        None => {
            let value = client
                .value(Method::GET, "/api/mcp/catalog", &[], None)
                .await?;
            let known = value
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .any(|entry| entry["id"].as_str() == Some(args.id.as_str()))
                })
                .unwrap_or(false);
            known.then(|| args.id.clone())
        }
    };
    let mut env: Vec<Value> = Vec::new();
    for entry in &args.env {
        let (key, value) = split_assignment(entry)?;
        env.push(json!({ "key": key, "value": value, "secret": false }));
    }
    for entry in &args.secret {
        let (key, value) = split_assignment(entry)?;
        env.push(json!({ "key": key, "value": value, "secret": true }));
    }
    let mut headers = serde_json::Map::new();
    for entry in &args.header {
        let (key, value) = split_assignment(entry)?;
        headers.insert(key, json!(value));
    }
    let mut body = json!({ "id": args.id });
    if let Some(catalog) = catalog {
        body["catalog"] = json!(catalog);
    }
    if let Some(image) = &args.image {
        body["image"] = json!(image);
        body["kind"] = json!("streamable-http");
    }
    if let Some(name) = &args.name {
        body["name"] = json!(name);
    }
    if let Some(description) = &args.description {
        body["description"] = json!(description);
    }
    if let Some(port) = args.port {
        body["port"] = json!(port);
    }
    if args.persist {
        body["persist"] = json!(true);
    }
    if let Some(cpu) = args.cpu {
        body["cpu_limit"] = json!(cpu);
    }
    if let Some(mem) = args.mem {
        body["mem_limit_mb"] = json!(mem);
    }
    if !env.is_empty() {
        body["env"] = json!(env);
    }
    if !headers.is_empty() {
        body["headers"] = Value::Object(headers);
    }
    let value = client
        .value(Method::POST, "/api/mcp/servers", &[], Some(body))
        .await?;
    if json {
        return print_json(&value);
    }
    println!(
        "deployed {} ({}, {} tools)",
        value["id"].as_str().unwrap_or("?"),
        value["status"].as_str().unwrap_or("?"),
        value["tools_count"].as_u64().unwrap_or_default()
    );
    if let Some(error) = value["last_error"]
        .as_str()
        .filter(|error| !error.is_empty())
    {
        println!("last error: {error}");
    }
    Ok(())
}

async fn mcp_lifecycle(
    client: &Client,
    json: bool,
    id: &str,
    action: &str,
) -> Result<(), CliError> {
    let path = format!("/api/mcp/servers/{}/{action}", encode_path_segment(id));
    let value = client.value(Method::POST, &path, &[], None).await?;
    if json {
        return print_json(&value);
    }
    println!(
        "{id}: {} ({})",
        value["status"].as_str().unwrap_or("?"),
        action
    );
    Ok(())
}

fn split_assignment(value: &str) -> Result<(String, String), CliError> {
    value
        .split_once('=')
        .map(|(key, value)| (key.trim().to_string(), value.to_string()))
        .ok_or_else(|| CliError::Config(format!("expected KEY=VALUE, got {value:?}")))
}

#[derive(Debug, Deserialize)]
struct McpServerSummary {
    id: String,
    kind: String,
    status: String,
    health: String,
    #[serde(default)]
    tools_count: u32,
    #[serde(default)]
    instances: Vec<String>,
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpServerDetail {
    id: String,
    kind: String,
    status: String,
    health: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tools_count: u32,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    instances: Vec<String>,
    #[serde(default)]
    env: Vec<McpEnvSummary>,
}

#[derive(Debug, Deserialize)]
struct McpEnvSummary {
    key: String,
    #[serde(default)]
    secret: bool,
    #[serde(default)]
    set: bool,
}

#[derive(Debug, Deserialize)]
struct CatalogSummary {
    id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    image: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("config.toml");
        std::fs::write(&path, text).expect("write config");
        path
    }

    #[test]
    fn the_configuration_file_provides_the_url_and_token() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write_config(
            dir.path(),
            "[server]\nurl = \"http://127.0.0.1:8090/\"\ntoken = \"file-token\"\n",
        );
        let cfg = ClientConfig::resolve(&path, |_| None).expect("config");
        assert_eq!(cfg.url, "http://127.0.0.1:8090");
        assert_eq!(cfg.token.as_deref(), Some("file-token"));
    }

    #[test]
    fn environment_variables_win_field_by_field() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write_config(
            dir.path(),
            "[server]\nurl = \"http://file.invalid:1\"\ntoken = \"file-token\"\n",
        );
        let cfg = ClientConfig::resolve(&path, |key| match key {
            URL_ENV => Some("https://fleett.example.test".to_string()),
            TOKEN_ENV => Some("env-token".to_string()),
            _ => None,
        })
        .expect("config");
        assert_eq!(cfg.url, "https://fleett.example.test");
        assert_eq!(cfg.token.as_deref(), Some("env-token"));

        // Only the token is overridden: the file URL still applies.
        let cfg = ClientConfig::resolve(&path, |key| {
            (key == TOKEN_ENV).then(|| "env-token".to_string())
        })
        .expect("config");
        assert_eq!(cfg.url, "http://file.invalid:1");
        assert_eq!(cfg.token.as_deref(), Some("env-token"));
    }

    #[test]
    fn a_missing_url_is_explained_and_an_empty_file_is_fine() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("missing.toml");
        let err = ClientConfig::resolve(&path, |_| None).expect_err("no url");
        let message = err.to_string();
        assert!(message.contains("no server URL"), "{message}");
        assert!(message.contains(URL_ENV), "{message}");
        assert!(message.contains("missing.toml"), "{message}");

        // A token alone does not make a configuration.
        let path = write_config(dir.path(), "[server]\ntoken = \"t\"\n");
        assert!(ClientConfig::resolve(&path, |_| None).is_err());

        // No token is allowed: the server may run without authentication.
        let path = write_config(dir.path(), "[server]\nurl = \"http://127.0.0.1:8090\"\n");
        let cfg = ClientConfig::resolve(&path, |_| None).expect("url only");
        assert!(cfg.token.is_none());
    }

    #[test]
    fn broken_or_unsafe_configuration_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write_config(dir.path(), "this is not toml [");
        let err = ClientConfig::resolve(&path, |_| None).expect_err("parse error");
        assert!(err.to_string().contains("cannot parse"), "{err}");

        let path = write_config(dir.path(), "[server]\nurl = \"ftp://example.test\"\n");
        let err = ClientConfig::resolve(&path, |_| None).expect_err("scheme");
        assert!(err.to_string().contains("http://"), "{err}");

        let path = write_config(dir.path(), "[server]\nurl = \"not a url\"\n");
        assert!(ClientConfig::resolve(&path, |_| None).is_err());
    }

    #[test]
    fn api_errors_keep_the_code_and_message() {
        let err = parse_api_error(
            404,
            r#"{"error":{"code":"not_found","message":"instance \"ghost\" not found"}}"#,
        );
        assert_eq!(err.code(), "not_found");
        let message = err.to_string();
        assert!(message.contains("ghost"), "{message}");
        assert!(message.contains("not_found"), "{message}");

        let err = parse_api_error(502, "upstream boom");
        assert_eq!(err.code(), "http_error");
        assert!(err.to_string().contains("upstream boom"));

        let err = parse_api_error(500, "");
        assert!(err.to_string().contains("HTTP 500"), "{err}");
    }

    #[test]
    fn tables_are_aligned() {
        let text = render_table(
            &["ID", "STATUS"],
            &[
                vec!["alice".to_string(), "running".to_string()],
                vec!["b".to_string(), "stopped".to_string()],
            ],
        );
        assert_eq!(text, "ID     STATUS\nalice  running\nb      stopped\n");
    }

    #[test]
    fn human_sizes_and_uptimes_read_well() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536 * 1024 * 1024), "1.5 GiB");
        assert_eq!(human_uptime(0), "0s");
        assert_eq!(human_uptime(90), "1m");
        assert_eq!(human_uptime(3_600), "1h0m");
        assert_eq!(human_uptime(93_784), "1d2h");
        assert_eq!(human_uptime(-1), "-");
    }

    #[test]
    fn caps_are_reported_as_unlimited_when_zero() {
        let caps = UsageCaps {
            max_cpu: 0.0,
            max_mem_mb: 0,
        };
        assert_eq!(caps_line(&caps), "caps: cpu unlimited, memory unlimited");
        let caps = UsageCaps {
            max_cpu: 4.0,
            max_mem_mb: 8192,
        };
        assert_eq!(caps_line(&caps), "caps: cpu 4.0, memory 8192 MB");
    }

    #[test]
    fn path_segments_are_escaped() {
        assert_eq!(encode_path_segment("alice-2"), "alice-2");
        assert_eq!(encode_path_segment("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn ssh_targets_are_split_without_losing_the_port() {
        assert_eq!(
            split_ssh_target("cle@10.1.20.5"),
            ("cle@10.1.20.5".to_string(), None)
        );
        assert_eq!(
            split_ssh_target("ssh://cle@10.1.20.5:2222"),
            ("cle@10.1.20.5".to_string(), Some("2222".to_string()))
        );
        assert_eq!(
            split_ssh_target(" ssh://cle@host "),
            ("cle@host".to_string(), None)
        );
    }

    #[test]
    fn remote_shell_paths_are_single_quoted() {
        assert_eq!(shell_quote("/opt/fleett"), "'/opt/fleett'");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    #[test]
    fn a_remote_instance_directory_can_never_be_root() {
        assert!(validate_remote_instance_dir("/opt/fleett/instances/alice").is_ok());
        for bad in ["", "/", "relative/path", "/opt/../etc"] {
            assert!(validate_remote_instance_dir(bad).is_err(), "{bad}");
        }
    }
}
