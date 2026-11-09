//! MCP server marketplace: catalogue, deployment, discovery and supervision
//! (issue #62).
//!
//! An MCP server is a *service*, not a file. A `streamable-http` (or `sse`)
//! server is a container fleett deploys as the Compose project
//! `fleett-mcp-<id>` on the shared `fleett` network, with the same hardening
//! as an agent: no published port, `cap_drop: ALL`, a read-only root, bounded
//! memory/CPU and capped logs. A `stdio` server is a command the agent itself
//! launches: it has no container and is never supervised.
//!
//! Secrets live in `mcp_server_env` (one AES-256-GCM row per key, always
//! encrypted for `secret` rows) and in the encrypted `headers_nonce/ciphertext`
//! columns for HTTP authentication. Neither the API nor the logs ever return a
//! value. See `docs/MCP-SERVERS.md`.

use crate::api::AppState;
use crate::config::Config;
use crate::db::Db;
use crate::driver::{ComposeProject, LogsOptions, PsResult};
use crate::envs::EnvWrite;
use crate::models::{
    now_iso, validate_image, validate_instance_id, validate_mem_limit, Health, InstanceSpec,
    McpKind, McpMode, McpRef, McpServerEnvRecord, McpServerRecord, McpServerSpec, Status,
};
use crate::secrets::{self, SecretsKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;
use tokio::net::TcpStream;

/// Prefix of the Compose project (and container) of a deployed MCP server.
pub const PROJECT_PREFIX: &str = "fleett-mcp-";
/// Compose service name of a deployed MCP server.
pub const SERVICE: &str = "mcp";
/// Default port an MCP HTTP container listens on.
pub const DEFAULT_PORT: u16 = 8000;
/// Default HTTP path of the MCP endpoint.
pub const DEFAULT_PATH: &str = "/mcp";
/// Default CPU quota of an MCP container.
pub const DEFAULT_CPU_LIMIT: f64 = 0.5;
/// Default memory quota of an MCP container.
pub const DEFAULT_MEM_LIMIT_MB: u32 = 256;
/// Upper bound of the supervision interval (`FLEETT_MCP_SUPERVISE_INTERVAL_S`).
pub const MAX_INTERVAL_S: u64 = 86_400;
/// Upper bound of the TCP probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Transport of a server checked by discovery/supervision.
pub const CAUSE_UNREACHABLE: &str = "unreachable";
/// Cause recorded when the container runs but its port refuses connections.
pub const CAUSE_UNHEALTHY: &str = "unhealthy";

#[derive(Debug, thiserror::Error)]
pub enum McpServerError {
    #[error("{0}")]
    Invalid(String),
    #[error("MCP server \"{0}\" not found")]
    NotFound(String),
    #[error("MCP server \"{0}\" already exists")]
    Duplicate(String),
    #[error("MCP server \"{id}\" is attached to an instance")]
    InUse { id: String, instances: Vec<String> },
    #[error("built-in catalogue entry \"{0}\" cannot be deleted")]
    Builtin(String),
    #[error("the MCP server is unreachable: {0}")]
    Unreachable(String),
    #[error("MCP upstream error: {0}")]
    Upstream(String),
    #[error(transparent)]
    Secrets(#[from] secrets::SecretsError),
    #[error(transparent)]
    Db(#[from] crate::db::DbError),
    #[error(transparent)]
    Render(#[from] crate::render::RenderError),
    #[error("filesystem error: {0}")]
    Io(String),
}

impl From<std::io::Error> for McpServerError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<crate::envs::EnvError> for McpServerError {
    fn from(err: crate::envs::EnvError) -> Self {
        match err {
            crate::envs::EnvError::Invalid(message) => Self::Invalid(message),
            other => Self::Invalid(other.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Names, endpoints and the compose document
// ---------------------------------------------------------------------------

/// Compose project of one MCP server.
pub fn project_name(id: &str) -> String {
    format!("{PROJECT_PREFIX}{id}")
}

/// Docker-DNS endpoint of a deployed server, as written into `config.yaml`.
///
/// Two agents that attach the same server share this one container.
pub fn endpoint(id: &str, port: u16, path: &str) -> String {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    format!("http://{}{}:{}{}", PROJECT_PREFIX, id, port, path)
}

#[derive(Debug, Serialize)]
struct ComposeFile {
    name: String,
    services: BTreeMap<String, ComposeService>,
    networks: BTreeMap<String, ComposeNetwork>,
}

#[derive(Debug, Serialize)]
struct ComposeService {
    image: String,
    container_name: String,
    restart: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    environment: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    volumes: Vec<String>,
    networks: Vec<String>,
    security_opt: Vec<String>,
    cap_drop: Vec<String>,
    read_only: bool,
    tmpfs: Vec<String>,
    pids_limit: u32,
    mem_limit: String,
    memswap_limit: String,
    cpus: String,
    logging: ComposeLogging,
}

#[derive(Debug, Serialize)]
struct ComposeLogging {
    driver: String,
    options: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct ComposeNetwork {
    external: bool,
    name: String,
}

/// Render `mcp/<id>/compose.yaml`.
///
/// Hardening is expressed by construction: the struct has no `ports`,
/// `privileged`, `network_mode`, `pid`, `volumes` other than the optional data
/// directory and no arbitrary mount fields, so an API request can never ask for
/// one. Every environment value is declared `${KEY}`; the driver injects the
/// decrypted values into the `docker compose` child process only.
pub fn render_compose(
    cfg: &Config,
    spec: &McpServerSpec,
    env_keys: &[String],
) -> Result<String, crate::render::RenderError> {
    let mut environment = BTreeMap::new();
    for key in env_keys {
        if crate::models::validate_secret_name(key).is_ok() {
            environment.insert(key.clone(), format!("${{{key}}}"));
        }
    }

    let mut volumes = Vec::new();
    if spec.persist {
        volumes.push("./data:/data".to_string());
    }

    let mut log_options = BTreeMap::new();
    log_options.insert("max-size".to_string(), "10m".to_string());
    log_options.insert("max-file".to_string(), "3".to_string());

    let mut networks = BTreeMap::new();
    networks.insert(
        "fleet".to_string(),
        ComposeNetwork {
            external: true,
            name: cfg.network.clone(),
        },
    );

    let service = ComposeService {
        image: spec.image.clone(),
        container_name: project_name(&spec.id),
        restart: "unless-stopped".to_string(),
        command: (!spec.command.is_empty()).then(|| spec.command.clone()),
        environment,
        volumes,
        networks: vec!["fleet".to_string()],
        security_opt: vec!["no-new-privileges:true".to_string()],
        cap_drop: vec!["ALL".to_string()],
        read_only: true,
        tmpfs: vec!["/tmp".to_string()],
        pids_limit: 128,
        mem_limit: format!("{}m", spec.mem_limit_mb),
        memswap_limit: format!("{}m", spec.mem_limit_mb),
        cpus: crate::render::format_cpu(spec.cpu_limit),
        logging: ComposeLogging {
            driver: "json-file".to_string(),
            options: log_options,
        },
    };

    let mut services = BTreeMap::new();
    services.insert(SERVICE.to_string(), service);
    let compose = ComposeFile {
        name: project_name(&spec.id),
        services,
        networks,
    };
    serde_yaml::to_string(&compose)
        .map_err(|err| crate::render::RenderError::Compose(err.to_string()))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate one deployment spec, returning the normalised value.
pub fn validate(mut spec: McpServerSpec) -> Result<McpServerSpec, McpServerError> {
    validate_instance_id(&spec.id).map_err(McpServerError::Invalid)?;
    let name = spec.name.trim();
    if name.is_empty() {
        return Err(McpServerError::Invalid("name must not be empty".into()));
    }
    if name.chars().count() > 128 {
        return Err(McpServerError::Invalid(
            "name must be at most 128 characters".into(),
        ));
    }
    spec.name = name.to_string();
    spec.description = spec.description.trim().to_string();
    spec.path = spec.path.trim().to_string();
    if spec.path.is_empty() {
        spec.path = DEFAULT_PATH.to_string();
    }
    if !spec.path.starts_with('/') || spec.path.chars().any(char::is_whitespace) {
        return Err(McpServerError::Invalid(
            "path must start with '/' and contain no whitespace".into(),
        ));
    }
    if spec.kind.is_deployed() {
        validate_image(&spec.image).map_err(McpServerError::Invalid)?;
        if spec.port == 0 {
            return Err(McpServerError::Invalid(
                "port must be between 1 and 65535".into(),
            ));
        }
    } else {
        if spec.command.is_empty() {
            return Err(McpServerError::Invalid(
                "a stdio server requires a command".into(),
            ));
        }
        if spec.image.trim().is_empty() {
            spec.image = String::new();
        }
        if spec.persist {
            return Err(McpServerError::Invalid(
                "a stdio server has no container and cannot persist data".into(),
            ));
        }
    }
    for arg in &spec.command {
        if arg.chars().any(|c| c.is_control()) {
            return Err(McpServerError::Invalid(
                "command arguments must not contain control characters".into(),
            ));
        }
    }
    if spec.cpu_limit <= 0.0
        || !spec.cpu_limit.is_finite()
        || spec.cpu_limit > crate::models::CPU_LIMIT_MAX
    {
        return Err(McpServerError::Invalid(format!(
            "cpu_limit must be between 0.1 and {}",
            crate::models::CPU_LIMIT_MAX
        )));
    }
    validate_mem_limit(spec.mem_limit_mb).map_err(McpServerError::Invalid)?;
    if spec.host.trim().is_empty() {
        spec.host = crate::hosts::LOCAL_HOST.to_string();
    }
    Ok(spec)
}

/// Whether the id looks like a valid server id (used by the catalogue import).
pub fn validate_catalog_entry(entry: &CatalogEntry) -> Result<(), McpServerError> {
    validate_instance_id(&entry.id).map_err(McpServerError::Invalid)?;
    if entry.name.trim().is_empty() {
        return Err(McpServerError::Invalid("name must not be empty".into()));
    }
    let kind = McpKind::parse(&entry.kind)
        .ok_or_else(|| McpServerError::Invalid(format!("unknown kind \"{}\"", entry.kind)))?;
    if kind.is_deployed() {
        validate_image(&entry.image).map_err(McpServerError::Invalid)?;
        if entry.port == 0 {
            return Err(McpServerError::Invalid("port must not be zero".into()));
        }
    }
    let mut seen = BTreeSet::new();
    for variable in &entry.env {
        if crate::models::validate_secret_name(&variable.key).is_err() {
            return Err(McpServerError::Invalid(format!(
                "invalid environment variable name \"{}\"",
                variable.key
            )));
        }
        if !seen.insert(variable.key.as_str()) {
            return Err(McpServerError::Invalid(format!(
                "environment variable \"{}\" is listed twice",
                variable.key
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Environment and headers (encrypted store)
// ---------------------------------------------------------------------------

/// Decrypted environment of one server (control-plane side only).
pub fn env_values(
    cfg: &Config,
    db: &Db,
    id: &str,
) -> Result<BTreeMap<String, String>, McpServerError> {
    let rows = db.list_mcp_server_env(id)?;
    let mut cache: Option<SecretsKey> = None;
    let mut values = BTreeMap::new();
    for row in rows {
        if row.secret {
            if cache.is_none() {
                cache =
                    Some(SecretsKey::from_config(cfg)?.ok_or(secrets::SecretsError::MissingKey)?);
            }
            let key = cache.as_ref().expect("resolved just above");
            let nonce = row
                .nonce
                .ok_or_else(|| McpServerError::Invalid("missing nonce".into()))?;
            let ciphertext = row
                .ciphertext
                .ok_or_else(|| McpServerError::Invalid("missing ciphertext".into()))?;
            let mut map = secrets::decrypt_map(key, &nonce, &ciphertext)?;
            let value = map.remove(&row.key).ok_or_else(|| {
                McpServerError::Invalid(format!(
                    "the encrypted value does not hold \"{}\"",
                    row.key
                ))
            })?;
            values.insert(row.key, value);
        } else if let Some(value) = row.value {
            values.insert(row.key, value);
        }
    }
    Ok(values)
}

/// Sorted names of every variable of one server.
pub fn env_keys(cfg: &Config, db: &Db, id: &str) -> Result<Vec<String>, McpServerError> {
    Ok(env_values(cfg, db, id)?.into_keys().collect())
}

/// API view of one variable: no value, only whether it is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpEnvView {
    pub key: String,
    pub secret: bool,
    pub set: bool,
}

/// API views of the environment of one server.
pub fn env_views(cfg: &Config, db: &Db, id: &str) -> Result<Vec<McpEnvView>, McpServerError> {
    let rows = db.list_mcp_server_env(id)?;
    let secrets: BTreeMap<String, bool> =
        rows.into_iter().map(|row| (row.key, row.secret)).collect();
    Ok(env_values(cfg, db, id)?
        .into_keys()
        .map(|key| McpEnvView {
            secret: secrets.get(&key).copied().unwrap_or(false),
            key,
            set: true,
        })
        .collect())
}

/// Replace the whole environment of one server.
pub fn replace_env(
    cfg: &Config,
    db: &Db,
    id: &str,
    entries: &[EnvWrite],
) -> Result<(), McpServerError> {
    crate::envs::validate_entries(entries).map_err(McpServerError::from)?;
    let key = if entries.iter().any(|entry| entry.secret) {
        Some(SecretsKey::from_config(cfg)?.ok_or(secrets::SecretsError::MissingKey)?)
    } else {
        None
    };
    let now = now_iso();
    let mut records = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.secret {
            let key = key.as_ref().expect("a secret entry resolved the key above");
            let mut map = BTreeMap::new();
            map.insert(entry.key.clone(), entry.value.clone());
            let sealed = secrets::encrypt_map(key, &map)?;
            records.push(McpServerEnvRecord {
                key: entry.key.clone(),
                secret: true,
                value: None,
                nonce: Some(sealed.nonce),
                ciphertext: Some(sealed.ciphertext),
                updated_at: now.clone(),
            });
        } else {
            records.push(McpServerEnvRecord {
                key: entry.key.clone(),
                secret: false,
                value: Some(entry.value.clone()),
                nonce: None,
                ciphertext: None,
                updated_at: now.clone(),
            });
        }
    }
    db.replace_mcp_server_env(id, &records)?;
    Ok(())
}

/// Validate an HTTP header name (RFC 7230 token), so a header map can never
/// inject a second header or a body separator.
fn validate_header_name(name: &str) -> Result<(), McpServerError> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        });
    if !valid {
        return Err(McpServerError::Invalid(format!(
            "invalid HTTP header name \"{name}\""
        )));
    }
    Ok(())
}

/// Decrypted HTTP headers of one server (never returned by the API).
pub fn headers(
    cfg: &Config,
    db: &Db,
    id: &str,
) -> Result<BTreeMap<String, String>, McpServerError> {
    let Some(sealed) = db.mcp_server_headers(id)? else {
        return Ok(BTreeMap::new());
    };
    let Some(key) = SecretsKey::from_config(cfg)? else {
        return Err(secrets::SecretsError::MissingKey.into());
    };
    Ok(secrets::decrypt_map(
        &key,
        &sealed.nonce,
        &sealed.ciphertext,
    )?)
}

/// Replace (or clear) the encrypted HTTP headers of one server.
pub fn replace_headers(
    cfg: &Config,
    db: &Db,
    id: &str,
    map: Option<&BTreeMap<String, String>>,
) -> Result<(), McpServerError> {
    let Some(map) = map else {
        db.set_mcp_server_headers(id, None)?;
        return Ok(());
    };
    if map.is_empty() {
        db.set_mcp_server_headers(id, None)?;
        return Ok(());
    }
    for (name, value) in map {
        validate_header_name(name)?;
        if value
            .chars()
            .any(|c| c == '\r' || c == '\n' || c.is_control())
        {
            return Err(McpServerError::Invalid(format!(
                "the value of header \"{name}\" contains a control character"
            )));
        }
    }
    let key = SecretsKey::from_config(cfg)?.ok_or(secrets::SecretsError::MissingKey)?;
    let sealed = secrets::encrypt_map(&key, map)?;
    db.set_mcp_server_headers(id, Some((&sealed.nonce, &sealed.ciphertext)))?;
    Ok(())
}

/// Names only: the API never exposes a header value.
pub fn header_names(cfg: &Config, db: &Db, id: &str) -> Result<Vec<String>, McpServerError> {
    Ok(headers(cfg, db, id)?.into_keys().collect())
}

// ---------------------------------------------------------------------------
// Deployment
// ---------------------------------------------------------------------------

/// Compose project of one stored server, with its decrypted environment.
pub fn compose_project(
    cfg: &Config,
    db: &Db,
    spec: &McpServerSpec,
) -> Result<ComposeProject, McpServerError> {
    Ok(ComposeProject {
        host: spec.host.clone(),
        file: cfg.mcp_compose_path(&spec.id),
        project: project_name(&spec.id),
        service: SERVICE.to_string(),
        env: env_values(cfg, db, &spec.id)?,
    })
}

/// Write `mcp/<id>/compose.yaml`, creating the directory tree.
pub fn write_compose(
    cfg: &Config,
    spec: &McpServerSpec,
    env_keys: &[String],
) -> Result<(), McpServerError> {
    let dir = cfg.mcp_server_dir(&spec.id);
    std::fs::create_dir_all(&dir)?;
    if spec.persist {
        std::fs::create_dir_all(cfg.mcp_data_dir(&spec.id))?;
    }
    let compose = render_compose(cfg, spec, env_keys)?;
    std::fs::write(cfg.mcp_compose_path(&spec.id), compose)?;
    Ok(())
}

/// Deploy (or redeploy) one server: network, image, compose file, `up -d`.
pub async fn deploy(state: &AppState, spec: &McpServerSpec) -> Result<(), McpServerError> {
    if !spec.kind.is_deployed() {
        return Err(McpServerError::Invalid(
            "a stdio server is config-only: it has no container to deploy".into(),
        ));
    }
    state
        .driver
        .ensure_network(&spec.host, &state.cfg.network)
        .await
        .map_err(|err| McpServerError::Upstream(err.detail))?;
    state
        .driver
        .ensure_image(&spec.host, &spec.image)
        .await
        .map_err(|err| McpServerError::Upstream(err.detail))?;
    let keys = env_keys(&state.cfg, &state.db, &spec.id)?;
    write_compose(&state.cfg, spec, &keys)?;
    let project = compose_project(&state.cfg, &state.db, spec)?;
    state
        .driver
        .project_up(&project)
        .await
        .map_err(|err| McpServerError::Upstream(err.detail))?;
    state
        .metrics
        .record_compose_action("mcp-up", crate::metrics::RESULT_OK);
    Ok(())
}

/// Lifecycle action requested on a deployed server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Start,
    Stop,
    Restart,
}

impl Lifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

/// Start, stop or restart one deployed server.
pub async fn lifecycle(
    state: &AppState,
    spec: &McpServerSpec,
    action: Lifecycle,
) -> Result<(), McpServerError> {
    if !spec.kind.is_deployed() {
        return Err(McpServerError::Invalid(
            "a stdio server is launched by the agent: there is no container to manage".into(),
        ));
    }
    let project = compose_project(&state.cfg, &state.db, spec)?;
    let result = match action {
        Lifecycle::Start => state.driver.project_start(&project).await,
        Lifecycle::Stop => state.driver.project_stop(&project).await,
        Lifecycle::Restart => state.driver.project_restart(&project).await,
    };
    match result {
        Ok(()) => {
            state.metrics.record_compose_action(
                &format!("mcp-{}", action.as_str()),
                crate::metrics::RESULT_OK,
            );
            Ok(())
        }
        Err(err) => {
            state.metrics.record_compose_action(
                &format!("mcp-{}", action.as_str()),
                crate::metrics::RESULT_ERROR,
            );
            Err(McpServerError::Upstream(err.detail))
        }
    }
}

/// Remove one server: `down`, then optionally the compose file and data.
pub async fn remove(
    state: &AppState,
    spec: &McpServerSpec,
    purge: bool,
) -> Result<(), McpServerError> {
    if spec.kind.is_deployed() {
        let project = compose_project(&state.cfg, &state.db, spec)?;
        if project.file.exists() {
            state
                .driver
                .project_down(&project)
                .await
                .map_err(|err| McpServerError::Upstream(err.detail))?;
        }
    }
    if purge {
        let dir = state.cfg.mcp_server_dir(&spec.id);
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
    }
    state.db.delete_mcp_server(&spec.id)?;
    Ok(())
}

/// Tail of the container logs; a stdio server has no logs.
pub async fn logs(
    state: &AppState,
    spec: &McpServerSpec,
    options: &LogsOptions,
) -> Result<String, McpServerError> {
    if !spec.kind.is_deployed() {
        return Ok(String::new());
    }
    let project = compose_project(&state.cfg, &state.db, spec)?;
    state
        .driver
        .project_logs(&project, options)
        .await
        .map_err(|err| McpServerError::Upstream(err.detail))
}

// ---------------------------------------------------------------------------
// Discovery and supervision
// ---------------------------------------------------------------------------

/// Result of one status refresh.
impl Default for RefreshReport {
    fn default() -> Self {
        Self {
            status: Status::Unknown,
            health: Health::Unknown,
            last_error: None,
            restarts: 0,
            ip_address: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RefreshReport {
    pub status: Status,
    pub health: Health,
    pub last_error: Option<String>,
    pub restarts: u32,
    pub ip_address: Option<String>,
}

/// Read the container state of one server and probe its port.
///
/// The probe is a bounded TCP connect to the container IP on the configured
/// port: it proves the server accepts connections without requiring an HTTP
/// path that a given MCP implementation may not expose. The API still publishes
/// a `/health` hint in the docs; discovery (`tools/list`) is the real
/// end-to-end check.
pub async fn refresh(
    state: &AppState,
    spec: &McpServerSpec,
) -> Result<RefreshReport, McpServerError> {
    if !spec.kind.is_deployed() {
        return Ok(RefreshReport {
            status: spec.status,
            health: Health::Unknown,
            last_error: None,
            restarts: 0,
            ip_address: None,
        });
    }
    let project = compose_project(&state.cfg, &state.db, spec)?;
    let ps = state
        .driver
        .project_ps(&project)
        .await
        .map_err(|err| McpServerError::Upstream(err.detail))?;
    let inspect = state.driver.project_inspect(&project).await.ok();
    let ip_address = inspect.as_ref().and_then(|info| info.ip_address.clone());
    let restarts = inspect.as_ref().map(|info| info.restart_count).unwrap_or(0);

    let mut last_error = None;
    let mut health = ps.health;
    if ps.status == Status::Running {
        match ip_address.as_deref() {
            Some(ip) => match probe(ip, spec.port).await {
                Ok(()) => health = Health::Healthy,
                Err(detail) => {
                    health = Health::Unhealthy;
                    last_error = Some(detail);
                }
            },
            None => {
                health = Health::Unhealthy;
                last_error = Some("the container has no IP on the fleet network yet".to_string());
            }
        }
    } else if ps.status != Status::Created && ps.status != Status::Stopped {
        if let Some(detail) = inspect.as_ref().and_then(|info| info.error_message()) {
            last_error = Some(detail);
        }
    }

    state.db.set_mcp_server_status(
        &spec.id,
        ps.status,
        health,
        last_error.as_deref(),
        Some(&now_iso()),
    )?;
    Ok(RefreshReport {
        status: ps.status,
        health,
        last_error,
        restarts,
        ip_address,
    })
}

/// Bounded TCP probe of `<ip>:<port>`.
pub async fn probe(ip: &str, port: u16) -> Result<(), String> {
    match tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect((ip, port))).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(format!("{ip}:{port} refused the connection: {err}")),
        Err(_) => Err(format!(
            "{ip}:{port} did not answer within {}s",
            PROBE_TIMEOUT.as_secs()
        )),
    }
}

/// URL the control plane probes for one server (container IP, not Docker DNS:
/// the control plane does not run inside the `fleett` network).
fn probe_url(spec: &McpServerSpec, report: &RefreshReport) -> Option<String> {
    let ip = report.ip_address.as_deref()?;
    Some(format!(
        "http://{ip}:{}{}",
        spec.port,
        if spec.path.starts_with('/') {
            spec.path.clone()
        } else {
            format!("/{}", spec.path)
        }
    ))
}

/// Run `initialize` + `tools/list` against one deployed server, cache the tools
/// and return the discovery (including its latency).
pub async fn discover(
    state: &AppState,
    record: &McpServerRecord,
) -> Result<crate::mcp_client::Discovery, McpServerError> {
    let spec = &record.spec;
    if !spec.kind.is_deployed() {
        return Err(McpServerError::Invalid(
            "a stdio server is launched by the agent: fleett cannot probe it".into(),
        ));
    }
    let report = refresh(state, spec).await?;
    let Some(url) = probe_url(spec, &report) else {
        return Err(McpServerError::Unreachable(
            report
                .last_error
                .clone()
                .unwrap_or_else(|| "the container has no IP address".to_string()),
        ));
    };
    let http_headers = headers(&state.cfg, &state.db, &spec.id)?;
    let client = crate::mcp_client::McpClient::from_config(&state.cfg);
    match client.discover(&url, &http_headers).await {
        Ok(discovery) => {
            let tools_json =
                serde_json::to_string(&discovery.tools).unwrap_or_else(|_| "[]".to_string());
            let count = u32::try_from(discovery.tools.len()).unwrap_or(u32::MAX);
            state
                .db
                .set_mcp_tools(&spec.id, &tools_json, count, &tools_hash(&tools_json))?;
            state.metrics.record_mcp_probe(crate::metrics::RESULT_OK);
            Ok(discovery)
        }
        Err(err) => {
            state.metrics.record_mcp_probe(crate::metrics::RESULT_ERROR);
            let detail = err.to_string();
            state.db.set_mcp_server_status(
                &spec.id,
                Status::Error,
                Health::Unhealthy,
                Some(&detail),
                Some(&now_iso()),
            )?;
            Err(McpServerError::Unreachable(detail))
        }
    }
}

/// Hex SHA-256 of the tools JSON (a stable fingerprint for the console).
pub fn tools_hash(tools_json: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(tools_json.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Result of one supervision pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickReport {
    pub servers: usize,
    pub healthy: usize,
    pub unhealthy: usize,
    pub incidents_opened: usize,
    pub incidents_closed: usize,
}

/// Sample every deployed server, open/close incidents and refresh metrics.
pub async fn tick(state: &AppState) -> TickReport {
    let mut report = TickReport::default();
    let records = match state.db.list_mcp_servers() {
        Ok(records) => records,
        Err(err) => {
            tracing::warn!(%err, "mcp: cannot list the servers for supervision");
            return report;
        }
    };
    for record in records {
        if !record.spec.kind.is_deployed() {
            continue;
        }
        report.servers += 1;
        let observed = refresh(state, &record.spec).await;
        match observed {
            Ok(observed)
                if observed.status == Status::Running && observed.health == Health::Healthy =>
            {
                report.healthy += 1;
                state.metrics.record_mcp_probe(crate::metrics::RESULT_OK);
                match state.db.close_open_mcp_incidents(&record.spec.id) {
                    Ok(closed) => report.incidents_closed += closed,
                    Err(err) => {
                        tracing::warn!(server = %record.spec.id, %err, "mcp: cannot close incidents")
                    }
                }
            }
            Ok(observed) => {
                report.unhealthy += 1;
                state.metrics.record_mcp_probe(crate::metrics::RESULT_ERROR);
                let cause = if observed.status == Status::Running {
                    CAUSE_UNHEALTHY
                } else {
                    CAUSE_UNREACHABLE
                };
                open_incident(
                    state,
                    &record.spec.id,
                    cause,
                    observed.last_error.as_deref(),
                    &mut report,
                );
            }
            Err(err) => {
                report.unhealthy += 1;
                state.metrics.record_mcp_probe(crate::metrics::RESULT_ERROR);
                open_incident(
                    state,
                    &record.spec.id,
                    CAUSE_UNREACHABLE,
                    Some(&err.to_string()),
                    &mut report,
                );
            }
        }
    }
    report
}

fn open_incident(
    state: &AppState,
    server_id: &str,
    cause: &str,
    detail: Option<&str>,
    report: &mut TickReport,
) {
    match state.db.open_mcp_incident(server_id, cause, detail) {
        Ok(incident) => {
            report.incidents_opened += 1;
            if let Err(err) = state.db.touch_mcp_incident(incident.id, detail) {
                tracing::warn!(server = %server_id, %err, "mcp: cannot touch the incident");
            }
            if let Err(err) = state.db.append_mcp_incident_action(
                incident.id,
                &json!({
                    "at": now_iso(),
                    "action": "probe",
                    "result": "error",
                    "detail": detail,
                }),
            ) {
                tracing::warn!(server = %server_id, %err, "mcp: cannot append the incident action");
            }
        }
        Err(err) => tracing::warn!(server = %server_id, %err, "mcp: cannot open an incident"),
    }
}

/// Background loop started by `fleett serve`; a zero interval disables it.
pub async fn run(state: AppState) {
    let interval_s = state.cfg.mcp_supervise_interval_s;
    if interval_s == 0 {
        tracing::info!("MCP supervision disabled (FLEETT_MCP_SUPERVISE_INTERVAL_S=0)");
        return;
    }
    let interval = Duration::from_secs(interval_s.min(MAX_INTERVAL_S));
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tracing::info!(interval_s, "MCP supervision started");
    loop {
        ticker.tick().await;
        tick(&state).await;
    }
}

// ---------------------------------------------------------------------------
// Attachment to agents
// ---------------------------------------------------------------------------

/// Resolve the attachments of one instance into the entries rendered into its
/// `config.yaml`.
///
/// A reference to a server that no longer exists is skipped with a warning: a
/// deleted server must never make an unrelated PATCH fail.
pub fn config_entries(
    cfg: &Config,
    db: &Db,
    instance_id: &str,
) -> Result<Vec<crate::render::McpConfigEntry>, McpServerError> {
    let refs = db.instance_mcp_servers(instance_id)?;
    config_entries_for_refs(cfg, db, &refs)
}

/// Same as [`config_entries`] for an explicit attachment list (a clone is
/// rendered before its links exist in the database).
pub fn config_entries_for_refs(
    cfg: &Config,
    db: &Db,
    refs: &[McpRef],
) -> Result<Vec<crate::render::McpConfigEntry>, McpServerError> {
    let mut entries = Vec::with_capacity(refs.len());
    for reference in refs {
        let Some(record) = db.get_mcp_server(&reference.server)? else {
            tracing::warn!(
                server = %reference.server,
                "mcp: attachment references a server that does not exist; skipping it"
            );
            continue;
        };
        let spec = record.spec;
        let timeout_s = cfg.mcp_call_timeout_s.max(1);
        let entry = match spec.kind {
            McpKind::Stdio => {
                let mut command = spec.command.into_iter();
                crate::render::McpConfigEntry {
                    id: spec.id.clone(),
                    transport: "stdio".to_string(),
                    url: None,
                    command: command.next(),
                    args: command.collect(),
                    env: env_keys(cfg, db, &spec.id)?,
                    timeout_s,
                    connect_timeout_s: timeout_s.min(5),
                }
            }
            McpKind::StreamableHttp | McpKind::Sse => crate::render::McpConfigEntry {
                id: spec.id.clone(),
                transport: spec.kind.as_str().to_string(),
                url: Some(endpoint(&spec.id, spec.port, &spec.path)),
                command: None,
                args: Vec::new(),
                env: Vec::new(),
                timeout_s,
                connect_timeout_s: timeout_s.min(5),
            },
        };
        entries.push(entry);
    }
    Ok(entries)
}

/// Parsed attachment request entry.
#[derive(Debug, Clone, Deserialize)]
pub struct McpRefInput {
    pub server: String,
    #[serde(default)]
    pub mode: Option<String>,
}

/// Normalise an attachment list, validating ids and modes.
pub fn normalize_refs(refs: &[McpRefInput]) -> Result<Vec<McpRef>, McpServerError> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(refs.len());
    for reference in refs {
        let server = reference.server.trim();
        validate_instance_id(server).map_err(McpServerError::Invalid)?;
        if !seen.insert(server.to_string()) {
            return Err(McpServerError::Invalid(format!(
                "server \"{server}\" is listed twice"
            )));
        }
        let mode = match reference.mode.as_deref() {
            None => McpMode::Rw,
            Some(value) => McpMode::parse(value)
                .ok_or_else(|| McpServerError::Invalid(format!("unknown mode \"{value}\"")))?,
        };
        out.push(McpRef {
            server: server.to_string(),
            mode,
        });
    }
    Ok(out)
}

/// Drop the attachments an instance no longer declares, and seed the new ones.
///
/// The instance row keeps `mcp_servers` in sync with `instance_mcp_servers`;
/// the API calls this before every render so a PATCH never loses an attachment.
pub fn sync_spec(spec: &mut InstanceSpec, refs: Vec<McpRef>) {
    spec.mcp_servers = refs;
}

// ---------------------------------------------------------------------------
// Catalogue
// ---------------------------------------------------------------------------

/// One environment variable a catalogue entry requires.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEnv {
    pub key: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub required: bool,
}

/// One catalogue entry (built-in or imported).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `stdio`, `streamable-http` or `sse`.
    pub kind: String,
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub env: Vec<CatalogEnv>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// On-disk schema of the local catalogue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogIndex {
    #[serde(default = "default_index_version")]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<CatalogEntry>,
}

fn default_index_version() -> u32 {
    1
}

impl Default for CatalogIndex {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

/// Built-in catalogue, always available offline.
pub fn builtin_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry {
            id: "fetch".to_string(),
            name: "Fetch".to_string(),
            description:
                "Fetch a URL and convert it to markdown for the model (reference implementation)."
                    .to_string(),
            kind: "streamable-http".to_string(),
            image: "mcp/fetch:latest".to_string(),
            command: Vec::new(),
            port: DEFAULT_PORT,
            path: DEFAULT_PATH.to_string(),
            env: Vec::new(),
            tags: vec!["web".to_string(), "reference".to_string()],
        },
        CatalogEntry {
            id: "filesystem".to_string(),
            name: "Filesystem".to_string(),
            description: "Read and write files under a directory served by the container."
                .to_string(),
            kind: "streamable-http".to_string(),
            image: "mcp/filesystem:latest".to_string(),
            command: Vec::new(),
            port: DEFAULT_PORT,
            path: DEFAULT_PATH.to_string(),
            env: Vec::new(),
            tags: vec!["files".to_string(), "persistent".to_string()],
        },
        CatalogEntry {
            id: "memory".to_string(),
            name: "Memory".to_string(),
            description: "A persistent knowledge graph the agent can read and update.".to_string(),
            kind: "streamable-http".to_string(),
            image: "mcp/memory:latest".to_string(),
            command: Vec::new(),
            port: DEFAULT_PORT,
            path: DEFAULT_PATH.to_string(),
            env: Vec::new(),
            tags: vec!["memory".to_string(), "persistent".to_string()],
        },
        CatalogEntry {
            id: "github".to_string(),
            name: "GitHub".to_string(),
            description: "Issues, pull requests, code search and repository metadata.".to_string(),
            kind: "streamable-http".to_string(),
            image: "mcp/github:latest".to_string(),
            command: Vec::new(),
            port: DEFAULT_PORT,
            path: DEFAULT_PATH.to_string(),
            env: vec![CatalogEnv {
                key: "GITHUB_PERSONAL_ACCESS_TOKEN".to_string(),
                secret: true,
                required: true,
            }],
            tags: vec!["git".to_string(), "github".to_string()],
        },
        CatalogEntry {
            id: "time".to_string(),
            name: "Time".to_string(),
            description: "Current time and timezone conversions (stdio, launched by the agent)."
                .to_string(),
            kind: "stdio".to_string(),
            image: String::new(),
            command: vec!["uvx".to_string(), "mcp-server-time".to_string()],
            port: 0,
            path: String::new(),
            env: Vec::new(),
            tags: vec!["reference".to_string(), "stdio".to_string()],
        },
    ]
}

/// Read the local index; a missing or malformed file is an empty catalogue
/// (a broken import must never take the whole API down).
pub fn read_index(cfg: &Config) -> CatalogIndex {
    let path = cfg.mcp_index_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            tracing::warn!(path = %path.display(), %err, "mcp: the local catalogue is invalid");
            CatalogIndex::default()
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => CatalogIndex::default(),
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "mcp: cannot read the local catalogue");
            CatalogIndex::default()
        }
    }
}

/// Write the local index atomically.
pub fn write_index(cfg: &Config, index: &CatalogIndex) -> Result<(), McpServerError> {
    let path = cfg.mcp_index_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let body =
        serde_json::to_vec_pretty(index).map_err(|err| McpServerError::Io(err.to_string()))?;
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Catalogue merged with the local imports; imported entries win on a clash so
/// an operator can override a built-in image without editing the binary.
pub fn catalog(cfg: &Config, query: Option<&str>) -> Vec<CatalogEntry> {
    let local = read_index(cfg);
    let mut entries: BTreeMap<String, CatalogEntry> = builtin_catalog()
        .into_iter()
        .map(|entry| (entry.id.clone(), entry))
        .collect();
    for entry in local.entries {
        entries.insert(entry.id.clone(), entry);
    }
    let needle = query
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    entries
        .into_values()
        .filter(|entry| match needle.as_deref() {
            None => true,
            Some(needle) => {
                entry.id.to_ascii_lowercase().contains(needle)
                    || entry.name.to_ascii_lowercase().contains(needle)
                    || entry.description.to_ascii_lowercase().contains(needle)
                    || entry
                        .tags
                        .iter()
                        .any(|tag| tag.to_ascii_lowercase().contains(needle))
            }
        })
        .collect()
}

/// Whether an entry is built in (and therefore cannot be deleted).
pub fn is_builtin(id: &str) -> bool {
    builtin_catalog().iter().any(|entry| entry.id == id)
}

/// Import one manifest into the local index.
pub fn import_entry(cfg: &Config, entry: CatalogEntry) -> Result<CatalogEntry, McpServerError> {
    validate_catalog_entry(&entry)?;
    if is_builtin(&entry.id)
        || read_index(cfg)
            .entries
            .iter()
            .any(|item| item.id == entry.id)
    {
        return Err(McpServerError::Duplicate(entry.id));
    }
    let mut index = read_index(cfg);
    index.entries.push(entry.clone());
    index.entries.sort_by(|left, right| left.id.cmp(&right.id));
    write_index(cfg, &index)?;
    Ok(entry)
}

/// Remove one imported entry; a built-in one is refused.
pub fn delete_entry(cfg: &Config, id: &str) -> Result<(), McpServerError> {
    if is_builtin(id) {
        return Err(McpServerError::Builtin(id.to_string()));
    }
    let mut index = read_index(cfg);
    let before = index.entries.len();
    index.entries.retain(|entry| entry.id != id);
    if index.entries.len() == before {
        return Err(McpServerError::NotFound(id.to_string()));
    }
    write_index(cfg, &index)?;
    Ok(())
}

/// Dereference one catalogue entry into a deployment spec.
pub fn spec_from_catalog(_cfg: &Config, entry: &CatalogEntry) -> McpServerSpec {
    let kind = McpKind::parse(&entry.kind).unwrap_or(McpKind::StreamableHttp);
    McpServerSpec {
        id: entry.id.clone(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        kind,
        image: entry.image.clone(),
        command: entry.command.clone(),
        port: if entry.port == 0 {
            DEFAULT_PORT
        } else {
            entry.port
        },
        path: if entry.path.trim().is_empty() {
            DEFAULT_PATH.to_string()
        } else {
            entry.path.clone()
        },
        persist: entry.tags.iter().any(|tag| tag == "persistent"),
        cpu_limit: DEFAULT_CPU_LIMIT,
        mem_limit_mb: DEFAULT_MEM_LIMIT_MB,
        host: crate::hosts::LOCAL_HOST.to_string(),
        status: Status::Created,
        health: Health::Unknown,
        last_error: None,
        last_probe_at: None,
    }
}

/// Path of the compose file, exposed for docs and tests.
pub fn compose_path(cfg: &Config, id: &str) -> std::path::PathBuf {
    cfg.mcp_compose_path(id)
}

/// Whether a file exists (small helper used by the deploy path).
pub fn exists(path: &Path) -> bool {
    path.exists()
}

/// `docker compose ps` of one stored server, for the API detail view.
pub async fn ps(state: &AppState, spec: &McpServerSpec) -> Result<PsResult, McpServerError> {
    if !spec.kind.is_deployed() {
        return Ok(PsResult {
            status: spec.status,
            health: Health::Unknown,
            container: None,
        });
    }
    let project = compose_project(&state.cfg, &state.db, spec)?;
    state
        .driver
        .project_ps(&project)
        .await
        .map_err(|err| McpServerError::Upstream(err.detail))
}

/// Discovery tools of one server, as stored (`[]` when never probed).
pub fn cached_tools(record: &McpServerRecord) -> Vec<Value> {
    record.tools.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::McpKind;

    fn config() -> Config {
        Config::for_tests(std::path::PathBuf::from("/tmp/fleett-mcp-test"))
    }

    fn spec() -> McpServerSpec {
        McpServerSpec {
            id: "fetch".to_string(),
            name: "Fetch".to_string(),
            description: "Fetch a URL".to_string(),
            kind: McpKind::StreamableHttp,
            image: "mcp/fetch:latest".to_string(),
            command: Vec::new(),
            port: DEFAULT_PORT,
            path: DEFAULT_PATH.to_string(),
            persist: false,
            cpu_limit: DEFAULT_CPU_LIMIT,
            mem_limit_mb: DEFAULT_MEM_LIMIT_MB,
            host: crate::hosts::LOCAL_HOST.to_string(),
            status: Status::Created,
            health: Health::Unknown,
            last_error: None,
            last_probe_at: None,
        }
    }

    #[test]
    fn the_compose_document_is_hardened_by_construction() {
        let cfg = config();
        let text = render_compose(&cfg, &spec(), &["GITHUB_TOKEN".to_string()]).expect("render");
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("yaml");
        let service = &doc["services"]["mcp"];

        assert_eq!(doc["name"], "fleett-mcp-fetch");
        assert_eq!(service["container_name"], "fleett-mcp-fetch");
        assert_eq!(service["restart"], "unless-stopped");
        assert_eq!(service["environment"]["GITHUB_TOKEN"], "${GITHUB_TOKEN}");
        assert!(!text.contains("ghp_"), "no value may reach the file");
        assert!(service.get("ports").is_none());
        assert!(service.get("privileged").is_none());
        assert!(service.get("network_mode").is_none());
        assert!(service.get("pid").is_none());
        assert!(service.get("devices").is_none());
        assert_eq!(service["read_only"], true);
        assert_eq!(
            service["cap_drop"],
            serde_yaml::Value::Sequence(vec!["ALL".into()])
        );
        assert_eq!(service["security_opt"][0], "no-new-privileges:true");
        assert_eq!(service["networks"][0], "fleet");
        assert_eq!(doc["networks"]["fleet"]["external"], true);
        assert_eq!(doc["networks"]["fleet"]["name"], cfg.network);
        assert_eq!(service["mem_limit"], "256m");
        assert_eq!(service["logging"]["options"]["max-size"], "10m");
    }

    #[test]
    fn persist_is_the_only_mount() {
        let cfg = config();
        let mut server = spec();
        server.persist = true;
        let text = render_compose(&cfg, &server, &[]).expect("render");
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("yaml");
        assert_eq!(doc["services"]["mcp"]["volumes"][0], "./data:/data");
        assert_eq!(
            doc["services"]["mcp"]["volumes"]
                .as_sequence()
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn validation_refuses_unsafe_or_incomplete_specs() {
        let valid = validate(spec()).expect("valid");
        assert_eq!(valid.port, DEFAULT_PORT);

        let mut bad = spec();
        bad.id = "Bad_ID".to_string();
        assert!(validate(bad).is_err());

        let mut bad = spec();
        bad.image = "mcp/fetch:latest --privileged".to_string();
        assert!(validate(bad).is_err());

        let mut stdio = spec();
        stdio.kind = McpKind::Stdio;
        stdio.command = Vec::new();
        assert!(validate(stdio).is_err());

        let mut stdio = spec();
        stdio.kind = McpKind::Stdio;
        stdio.command = vec!["npx".to_string(), "server".to_string()];
        stdio.persist = true;
        assert!(validate(stdio).is_err());

        let mut bad = spec();
        bad.path = "mcp".to_string();
        assert!(validate(bad).is_err());
    }

    #[test]
    fn the_endpoint_is_the_docker_dns_name() {
        assert_eq!(
            endpoint("fetch", 8000, "/mcp"),
            "http://fleett-mcp-fetch:8000/mcp"
        );
        assert_eq!(
            endpoint("fetch", 8000, "sse"),
            "http://fleett-mcp-fetch:8000/sse"
        );
    }

    #[test]
    fn the_tools_hash_is_stable_and_sensitive() {
        let first = tools_hash(r#"[{"name":"fetch"}]"#);
        assert_eq!(first.len(), 64);
        assert_eq!(first, tools_hash(r#"[{"name":"fetch"}]"#));
        assert_ne!(first, tools_hash(r#"[{"name":"search"}]"#));
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_catalogue_import_round_trips_and_refuses_duplicates() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().to_path_buf());
        let entry = CatalogEntry {
            id: "custom".to_string(),
            name: "Custom".to_string(),
            kind: "streamable-http".to_string(),
            image: "example/custom:1".to_string(),
            port: 8000,
            path: "/mcp".to_string(),
            ..CatalogEntry::default()
        };
        import_entry(&cfg, entry.clone()).expect("import");
        assert!(matches!(
            import_entry(&cfg, entry),
            Err(McpServerError::Duplicate(_))
        ));
        assert!(catalog(&cfg, None).iter().any(|entry| entry.id == "custom"));
        delete_entry(&cfg, "custom").expect("delete");
        assert!(!catalog(&cfg, None).iter().any(|entry| entry.id == "custom"));
        assert!(matches!(
            delete_entry(&cfg, "fetch"),
            Err(McpServerError::Builtin(_))
        ));
    }
}
