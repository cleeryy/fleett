//! Client mode: the `fleett` binary talking to a running control plane.
//!
//! Server commands (`serve`, `migrate`, `egress`, `secrets`, `backup`) run on
//! the host; the commands below are a thin HTTP client so the fleet is
//! scriptable from cron, CI or a runbook. See `docs/CLI.md`.

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

struct Client {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Client {
    fn new(cfg: &ClientConfig) -> Result<Self, CliError> {
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

    async fn value(
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

    async fn text(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<String, CliError> {
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
        let text = response.text().await.map_err(|err| CliError::Transport {
            url: url.clone(),
            detail: err.to_string(),
        })?;
        if (200..300).contains(&status) {
            Ok(text)
        } else {
            Err(parse_api_error(status, &text))
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// One client-mode invocation.
#[derive(Debug, Clone)]
pub enum Command {
    Ls,
    Ps { all: bool },
    Logs(LogsArgs),
    Create(CreateArgs),
    Start(String),
    Stop(String),
    Restart(String),
    Delete { id: String, purge: bool },
    Usage,
    Images,
    Upgrade(UpgradeArgs),
    Doctor,
    Say(SayArgs),
    Messages(MessagesArgs),
    Ack { id: String, message_id: String },
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
        Command::Create(args) => create(&client, json, &args).await,
        Command::Start(id) => lifecycle(&client, json, &id, "start").await,
        Command::Stop(id) => lifecycle(&client, json, &id, "stop").await,
        Command::Restart(id) => lifecycle(&client, json, &id, "restart").await,
        Command::Delete { id, purge } => delete(&client, json, &id, purge).await,
        Command::Usage => usage(&client, json).await,
        Command::Images => images(&client, json).await,
        Command::Upgrade(args) => upgrade(&client, json, &args).await,
        Command::Doctor => doctor(&client, json, &config.url).await,
        Command::Say(args) => say(&client, json, &args).await,
        Command::Messages(args) => messages(&client, json, &args).await,
        Command::Ack { id, message_id } => ack(&client, json, &id, &message_id).await,
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

async fn usage(client: &Client, json: bool) -> Result<(), CliError> {
    let value = client.value(Method::GET, "/api/usage", &[], None).await?;
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
    println!("{}", caps_line(&report.caps));
    Ok(())
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
fn encode_path_segment(value: &str) -> String {
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
}
