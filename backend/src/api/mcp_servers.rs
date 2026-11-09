//! MCP marketplace routes (issue #62).
//!
//! Reads need any authenticated role; every mutation of the catalogue, of a
//! server or of an attachment needs `admin` (an attachment changes the config
//! of an agent, which is a write on that instance and is additionally covered
//! by [`crate::authz`] for the `/api/instances/{id}/...` routes).
//!
//! The API never returns an environment value, a header value or an image
//! registry credential: a server exposes only `{key, secret, set}` views.

use crate::api::audit::{record, require_admin};
use crate::api::error::ApiError;
use crate::api::instances::MAX_LOGS_TAIL;
use crate::api::{instances, AppState};
use crate::auth::AuthContext;
use crate::driver::LogsOptions;
use crate::envs::EnvWrite;
use crate::mcp_servers::{self, CatalogEntry, Lifecycle, McpServerError, RefreshReport};
use crate::models::{Health, McpKind, McpRef, McpServerRecord, McpServerSpec, Status};
use crate::render::McpConfigEntry;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

/// Default number of trailing log lines.
pub const DEFAULT_LOG_TAIL: u32 = crate::api::instances::DEFAULT_LOGS_TAIL;
/// Upper bound of one log page (mirrors the instance log route).
pub const MAX_LOG_TAIL_MCP: u32 = MAX_LOGS_TAIL;

impl From<McpServerError> for ApiError {
    fn from(err: McpServerError) -> Self {
        match err {
            McpServerError::Invalid(message) => {
                ApiError::bad_request("invalid_mcp_server", message)
            }
            McpServerError::NotFound(id) => {
                ApiError::not_found(format!("MCP server \"{id}\" not found"))
            }
            McpServerError::Duplicate(id) => ApiError::conflict(
                "mcp_duplicate",
                format!("MCP server or catalogue entry \"{id}\" already exists"),
            ),
            McpServerError::InUse { id, instances } => ApiError::conflict(
                "mcp_server_in_use",
                format!("MCP server \"{id}\" is attached to one or more agents"),
            )
            .with_details(json!({ "instances": instances })),
            McpServerError::Builtin(id) => ApiError::bad_request(
                "invalid_mcp_server",
                format!("built-in catalogue entry \"{id}\" cannot be deleted"),
            ),
            McpServerError::Unreachable(detail) => ApiError::unprocessable(
                "mcp_unreachable",
                format!("the MCP server is unreachable: {detail}"),
            )
            .with_details(json!({ "detail": detail })),
            McpServerError::Upstream(detail) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "mcp_upstream",
                format!("the MCP upstream failed: {detail}"),
            )
            .with_details(json!({ "detail": detail })),
            McpServerError::Secrets(err) => ApiError::from(err),
            McpServerError::Db(err) => ApiError::from(err),
            McpServerError::Render(err) => ApiError::from(err),
            McpServerError::Io(detail) => ApiError::internal(detail),
        }
    }
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// One environment variable, without its value.
#[derive(Debug, Clone, Serialize)]
pub struct EnvView {
    pub key: String,
    pub secret: bool,
    pub set: bool,
}

/// One MCP server as returned by the API.
#[derive(Debug, Clone, Serialize)]
pub struct ServerView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub image: String,
    pub command: Vec<String>,
    pub port: u16,
    pub path: String,
    pub persist: bool,
    pub cpu_limit: f64,
    pub mem_limit_mb: u32,
    pub host: String,
    pub status: String,
    pub health: String,
    pub last_error: Option<String>,
    pub last_probe_at: Option<String>,
    /// Docker-DNS endpoint rendered into an agent config (null for stdio).
    pub endpoint: Option<String>,
    pub tools: Vec<serde_json::Value>,
    pub tools_count: u32,
    pub tools_hash: String,
    pub env: Vec<EnvView>,
    /// Header names only; values are never returned.
    pub headers: Vec<String>,
    /// Instances that attach this server (share one container).
    pub instances: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn server_view(state: &AppState, stored: &McpServerRecord) -> Result<ServerView, ApiError> {
    let spec = &stored.spec;
    Ok(ServerView {
        id: spec.id.clone(),
        name: spec.name.clone(),
        description: spec.description.clone(),
        kind: spec.kind.as_str().to_string(),
        image: spec.image.clone(),
        command: spec.command.clone(),
        port: spec.port,
        path: spec.path.clone(),
        persist: spec.persist,
        cpu_limit: spec.cpu_limit,
        mem_limit_mb: spec.mem_limit_mb,
        host: spec.host.clone(),
        status: spec.status.as_str().to_string(),
        health: spec.health.as_str().to_string(),
        last_error: spec.last_error.clone(),
        last_probe_at: spec.last_probe_at.clone(),
        endpoint: spec
            .kind
            .is_deployed()
            .then(|| mcp_servers::endpoint(&spec.id, spec.port, &spec.path)),
        tools: stored.tools.clone(),
        tools_count: stored.tools_count,
        tools_hash: stored.tools_hash.clone(),
        env: mcp_servers::env_views(&state.cfg, &state.db, &spec.id)
            .map_err(ApiError::from)?
            .into_iter()
            .map(|view| EnvView {
                key: view.key,
                secret: view.secret,
                set: view.set,
            })
            .collect(),
        headers: mcp_servers::header_names(&state.cfg, &state.db, &spec.id)
            .map_err(ApiError::from)?,
        instances: state
            .db
            .mcp_server_instances(&spec.id)
            .map_err(ApiError::from)?,
        created_at: stored.created_at.clone(),
        updated_at: stored.updated_at.clone(),
    })
}

async fn load_record(state: &AppState, id: &str) -> Result<McpServerRecord, ApiError> {
    state
        .db
        .get_mcp_server(id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("MCP server \"{id}\" not found")))
}

// ---------------------------------------------------------------------------
// Catalogue
// ---------------------------------------------------------------------------

/// Query of `GET /api/mcp/catalog`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatalogQuery {
    #[serde(default)]
    pub q: Option<String>,
}

/// `GET /api/mcp/catalog?q=` — built-in entries merged with the local index.
pub async fn catalog(
    State(state): State<AppState>,
    Query(query): Query<CatalogQuery>,
) -> Json<Vec<CatalogEntry>> {
    Json(mcp_servers::catalog(&state.cfg, query.q.as_deref()))
}

/// `POST /api/mcp/catalog/import` — append one manifest to the local index.
pub async fn import(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(entry): Json<CatalogEntry>,
) -> Result<(StatusCode, Json<CatalogEntry>), ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id("mcp:catalog");
    let _guard = lock.lock().await;
    let entry = mcp_servers::import_entry(&state.cfg, entry).map_err(ApiError::from)?;
    record(
        &state,
        &context,
        "mcp.catalog.import",
        &entry.id,
        Some(json!({ "kind": entry.kind, "image": entry.image })),
    );
    Ok((StatusCode::CREATED, Json(entry)))
}

/// `DELETE /api/mcp/catalog/{id}` — only imported entries can be removed.
pub async fn delete_catalog(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id("mcp:catalog");
    let _guard = lock.lock().await;
    mcp_servers::delete_entry(&state.cfg, &id).map_err(ApiError::from)?;
    record(&state, &context, "mcp.catalog.delete", &id, None);
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Servers
// ---------------------------------------------------------------------------

/// Body of `POST /api/mcp/servers`.
///
/// Either a complete spec or a `catalog` reference: `{"catalog": "fetch"}`
/// deploys the catalogue entry with the given id. Every field is optional so
/// the CLI can send only `{"catalog": "..."}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateServerRequest {
    #[serde(default)]
    pub catalog: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub persist: Option<bool>,
    #[serde(default)]
    pub cpu_limit: Option<f64>,
    #[serde(default)]
    pub mem_limit_mb: Option<u32>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub env: Vec<EnvInput>,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
}

/// One environment variable write.
#[derive(Debug, Clone, Deserialize)]
pub struct EnvInput {
    pub key: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub secret: bool,
}

fn spec_from_request(
    state: &AppState,
    request: &CreateServerRequest,
) -> Result<McpServerSpec, ApiError> {
    if let Some(catalog_id) = request
        .catalog
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        let entry = mcp_servers::catalog(&state.cfg, None)
            .into_iter()
            .find(|entry| entry.id == catalog_id)
            .ok_or_else(|| {
                ApiError::not_found(format!(
                    "catalogue entry \"{catalog_id}\" not found; run `fleett mcp-server catalog`"
                ))
            })?;
        let mut spec = mcp_servers::spec_from_catalog(&state.cfg, &entry);
        if let Some(id) = request.id.clone() {
            spec.id = id;
        }
        if let Some(name) = request.name.clone() {
            spec.name = name;
        }
        if let Some(description) = request.description.clone() {
            spec.description = description;
        }
        if let Some(cpu) = request.cpu_limit {
            spec.cpu_limit = cpu;
        }
        if let Some(mem) = request.mem_limit_mb {
            spec.mem_limit_mb = mem;
        }
        if let Some(host) = request.host.clone() {
            spec.host = host;
        }
        if let Some(port) = request.port {
            spec.port = port;
        }
        if let Some(path) = request.path.clone() {
            spec.path = path;
        }
        if let Some(persist) = request.persist {
            spec.persist = persist;
        }
        return mcp_servers::validate(spec).map_err(ApiError::from);
    }

    let id = request
        .id
        .clone()
        .ok_or_else(|| ApiError::bad_request("invalid_mcp_server", "id is required"))?;
    let kind = request
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            McpKind::parse(value).ok_or_else(|| {
                ApiError::bad_request(
                    "invalid_mcp_server",
                    format!("unknown kind \"{value}\" (expected stdio, streamable-http or sse)"),
                )
            })
        })
        .transpose()?
        .unwrap_or(McpKind::StreamableHttp);
    let spec = McpServerSpec {
        id,
        name: request
            .name
            .clone()
            .unwrap_or_else(|| "MCP server".to_string()),
        description: request.description.clone().unwrap_or_default(),
        kind,
        image: request.image.clone().unwrap_or_default(),
        command: request.command.clone().unwrap_or_default(),
        port: request.port.unwrap_or(mcp_servers::DEFAULT_PORT),
        path: request
            .path
            .clone()
            .unwrap_or_else(|| mcp_servers::DEFAULT_PATH.to_string()),
        persist: request.persist.unwrap_or(false),
        cpu_limit: request.cpu_limit.unwrap_or(mcp_servers::DEFAULT_CPU_LIMIT),
        mem_limit_mb: request
            .mem_limit_mb
            .unwrap_or(mcp_servers::DEFAULT_MEM_LIMIT_MB),
        host: request
            .host
            .clone()
            .unwrap_or_else(|| crate::hosts::LOCAL_HOST.to_string()),
        status: Status::Created,
        health: Health::Unknown,
        last_error: None,
        last_probe_at: None,
    };
    mcp_servers::validate(spec).map_err(ApiError::from)
}

fn env_writes(entries: &[EnvInput]) -> Result<Vec<EnvWrite>, ApiError> {
    entries
        .iter()
        .map(|entry| {
            crate::models::validate_secret_name(&entry.key)
                .map_err(|message| ApiError::bad_request("invalid_mcp_server", message))?;
            Ok(EnvWrite {
                key: entry.key.clone(),
                value: entry.value.clone().unwrap_or_default(),
                secret: entry.secret,
            })
        })
        .collect()
}

/// `GET /api/mcp/servers`.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<ServerView>>, ApiError> {
    let records = state.db.list_mcp_servers().map_err(ApiError::from)?;
    let mut views = Vec::with_capacity(records.len());
    for record in records {
        views.push(server_view(&state, &record)?);
    }
    Ok(Json(views))
}

/// `GET /api/mcp/servers/{id}`.
pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ServerView>, ApiError> {
    let stored = load_record(&state, &id).await?;
    Ok(Json(server_view(&state, &stored)?))
}

/// `POST /api/mcp/servers` — create, deploy and discover one server.
pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateServerRequest>,
) -> Result<(StatusCode, Json<ServerView>), ApiError> {
    require_admin(&context)?;
    let spec = spec_from_request(&state, &request)?;
    let lock = state.locks.for_id(&format!("mcp:{}", spec.id));
    let _guard = lock.lock().await;

    if state
        .db
        .get_mcp_server(&spec.id)
        .map_err(ApiError::from)?
        .is_some()
    {
        return Err(ApiError::conflict(
            "mcp_duplicate",
            format!("MCP server \"{}\" already exists", spec.id),
        ));
    }
    let env = env_writes(&request.env)?;
    crate::envs::check_storable(&state.cfg, &env).map_err(ApiError::from)?;

    state.db.insert_mcp_server(&spec).map_err(ApiError::from)?;
    if let Err(err) = mcp_servers::replace_env(&state.cfg, &state.db, &spec.id, &env) {
        let _ = state.db.delete_mcp_server(&spec.id);
        return Err(ApiError::from(err));
    }
    if let Some(headers) = request.headers.as_ref() {
        if let Err(err) =
            mcp_servers::replace_headers(&state.cfg, &state.db, &spec.id, Some(headers))
        {
            let _ = state.db.delete_mcp_server(&spec.id);
            return Err(ApiError::from(err));
        }
    }

    if spec.kind.is_deployed() {
        if let Err(err) = mcp_servers::deploy(&state, &spec).await {
            let _ = state.db.set_mcp_server_status(
                &spec.id,
                Status::Error,
                Health::Unhealthy,
                Some(&err.to_string()),
                Some(&crate::models::now_iso()),
            );
            record(
                &state,
                &context,
                "mcp_server.create",
                &spec.id,
                Some(json!({"result": "failed", "error": err.to_string()})),
            );
            return Err(ApiError::from(err));
        }
        let _ = mcp_servers::refresh(&state, &spec).await;
        if let Ok(stored) = load_record(&state, &spec.id).await {
            let _ = mcp_servers::discover(&state, &stored).await;
        }
    }

    let stored = load_record(&state, &spec.id).await?;
    record(
        &state,
        &context,
        "mcp_server.create",
        &spec.id,
        Some(json!({
            "kind": spec.kind.as_str(),
            "image": spec.image,
            "port": spec.port,
            "env": env.iter().map(|entry| entry.key.clone()).collect::<Vec<_>>(),
        })),
    );
    Ok((StatusCode::CREATED, Json(server_view(&state, &stored)?)))
}

/// Body of `PATCH /api/mcp/servers/{id}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateServerRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub persist: Option<bool>,
    #[serde(default)]
    pub cpu_limit: Option<f64>,
    #[serde(default)]
    pub mem_limit_mb: Option<u32>,
    #[serde(default)]
    pub host: Option<String>,
    /// `null` clears every header; an object replaces the map.
    #[serde(default, deserialize_with = "double_option")]
    pub headers: Option<Option<BTreeMap<String, String>>>,
}

/// Distinguish an absent field (`None`) from an explicit `null` (`Some(None)`).
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// `PATCH /api/mcp/servers/{id}` — spec changes do not redeploy automatically.
pub async fn patch(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<UpdateServerRequest>,
) -> Result<Json<ServerView>, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&format!("mcp:{id}"));
    let _guard = lock.lock().await;
    let mut stored = load_record(&state, &id).await?;
    let mut spec = stored.spec.clone();
    if let Some(name) = request.name.clone() {
        spec.name = name;
    }
    if let Some(description) = request.description.clone() {
        spec.description = description;
    }
    if let Some(image) = request.image.clone() {
        spec.image = image;
    }
    if let Some(command) = request.command.clone() {
        spec.command = command;
    }
    if let Some(port) = request.port {
        spec.port = port;
    }
    if let Some(path) = request.path.clone() {
        spec.path = path;
    }
    if let Some(persist) = request.persist {
        spec.persist = persist;
    }
    if let Some(cpu) = request.cpu_limit {
        spec.cpu_limit = cpu;
    }
    if let Some(mem) = request.mem_limit_mb {
        spec.mem_limit_mb = mem;
    }
    if let Some(host) = request.host.clone() {
        spec.host = host;
    }
    spec = mcp_servers::validate(spec).map_err(ApiError::from)?;
    if let Some(headers) = request.headers.as_ref() {
        mcp_servers::replace_headers(&state.cfg, &state.db, &id, headers.as_ref())
            .map_err(ApiError::from)?;
    }
    state.db.update_mcp_server(&spec).map_err(ApiError::from)?;
    stored.spec = spec.clone();
    record(
        &state,
        &context,
        "mcp_server.update",
        &id,
        Some(json!({
            "headers": request.headers.is_some(),
            "redeploy_required": spec.kind.is_deployed(),
        })),
    );
    Ok(Json(server_view(&state, &stored)?))
}

/// Query of `DELETE /api/mcp/servers/{id}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    pub purge: Option<bool>,
}

/// `DELETE /api/mcp/servers/{id}` — refused while attached.
pub async fn delete(
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&format!("mcp:{id}"));
    let _guard = lock.lock().await;
    let stored = load_record(&state, &id).await?;
    let attached = state.db.mcp_server_instances(&id).map_err(ApiError::from)?;
    if !attached.is_empty() {
        return Err(ApiError::from(McpServerError::InUse {
            id: id.clone(),
            instances: attached,
        }));
    }
    let purge = query.purge.unwrap_or(false);
    mcp_servers::remove(&state, &stored.spec, purge)
        .await
        .map_err(ApiError::from)?;
    record(
        &state,
        &context,
        "mcp_server.delete",
        &id,
        Some(json!({ "purge": purge })),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Lifecycle action of `POST …/{id}/start|stop|restart`.
async fn lifecycle(
    state: &AppState,
    context: &AuthContext,
    id: &str,
    action: Lifecycle,
) -> Result<Json<ServerView>, ApiError> {
    let lock = state.locks.for_id(&format!("mcp:{id}"));
    let _guard = lock.lock().await;
    let stored = load_record(state, id).await?;
    mcp_servers::lifecycle(state, &stored.spec, action)
        .await
        .map_err(ApiError::from)?;
    let _ = mcp_servers::refresh(state, &stored.spec).await;
    if action != Lifecycle::Stop {
        if let Ok(stored) = load_record(state, id).await {
            let _ = mcp_servers::discover(state, &stored).await;
        }
    }
    let stored = load_record(state, id).await?;
    record(
        state,
        context,
        &format!("mcp_server.{}", action.as_str()),
        id,
        None,
    );
    Ok(Json(server_view(state, &stored)?))
}

pub async fn start(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<ServerView>, ApiError> {
    require_admin(&context)?;
    lifecycle(&state, &context, &id, Lifecycle::Start).await
}

pub async fn stop(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<ServerView>, ApiError> {
    require_admin(&context)?;
    lifecycle(&state, &context, &id, Lifecycle::Stop).await
}

pub async fn restart(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<ServerView>, ApiError> {
    require_admin(&context)?;
    lifecycle(&state, &context, &id, Lifecycle::Restart).await
}

/// Response of `POST /api/mcp/servers/{id}/test`.
#[derive(Debug, Clone, Serialize)]
pub struct TestResponse {
    pub server: String,
    pub reachable: bool,
    pub latency_ms: u64,
    pub tools: Vec<serde_json::Value>,
    pub tools_count: usize,
    pub tools_hash: String,
    pub protocol_version: String,
    pub server_info: serde_json::Value,
}

/// `POST /api/mcp/servers/{id}/test` — run `initialize` + `tools/list` now.
pub async fn test(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<TestResponse>, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&format!("mcp:{id}"));
    let _guard = lock.lock().await;
    let stored = load_record(&state, &id).await?;
    let discovery = mcp_servers::discover(&state, &stored)
        .await
        .map_err(ApiError::from)?;
    let tools_json = serde_json::to_string(&discovery.tools).unwrap_or_else(|_| "[]".to_string());
    let tools_count = discovery.tools.len();
    record(
        &state,
        &context,
        "mcp_server.test",
        &id,
        Some(json!({
            "latency_ms": discovery.latency_ms,
            "tools": tools_count,
        })),
    );
    Ok(Json(TestResponse {
        server: id,
        reachable: true,
        latency_ms: discovery.latency_ms,
        tools: discovery
            .tools
            .into_iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            })
            .collect(),
        tools_count,
        tools_hash: mcp_servers::tools_hash(&tools_json),
        protocol_version: discovery.protocol_version,
        server_info: discovery.server_info,
    }))
}

/// Query of `GET /api/mcp/servers/{id}/logs`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogsQuery {
    #[serde(default)]
    pub tail: Option<u32>,
    #[serde(default)]
    pub since: Option<String>,
}

/// `GET /api/mcp/servers/{id}/logs` — plain text, like the instance logs.
pub async fn logs(
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let tail = query.tail.unwrap_or(DEFAULT_LOG_TAIL);
    if tail == 0 || tail > MAX_LOG_TAIL_MCP {
        return Err(ApiError::bad_request(
            "invalid_tail",
            format!("tail must be between 1 and {MAX_LOG_TAIL_MCP}"),
        ));
    }
    let stored = load_record(&state, &id).await?;
    let options = LogsOptions {
        tail,
        since: query.since.clone(),
    };
    let text = mcp_servers::logs(&state, &stored.spec, &options)
        .await
        .map_err(ApiError::from)?;
    Ok(text.into_response())
}

/// `GET /api/mcp/servers/{id}/tools` — the cached discovery result.
pub async fn tools(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let stored = load_record(&state, &id).await?;
    Ok(Json(json!({
        "server": stored.spec.id,
        "tools": stored.tools,
        "count": stored.tools_count,
        "hash": stored.tools_hash,
        "probed_at": stored.spec.last_probe_at,
    })))
}

/// Body of `PUT /api/mcp/servers/{id}/env`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PutEnvRequest {
    #[serde(default)]
    pub env: Option<Vec<EnvInput>>,
}

/// `PUT /api/mcp/servers/{id}/env` — replace the whole environment.
pub async fn put_env(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PutEnvRequest>,
) -> Result<Json<ServerView>, ApiError> {
    require_admin(&context)?;
    let entries = env_writes(&request.env.ok_or_else(|| {
        ApiError::bad_request(
            "invalid_mcp_server",
            "env is required: send {\"env\": [{key, value, secret}]}",
        )
    })?)?;
    let lock = state.locks.for_id(&format!("mcp:{id}"));
    let _guard = lock.lock().await;
    let stored = load_record(&state, &id).await?;
    crate::envs::check_storable(&state.cfg, &entries).map_err(ApiError::from)?;
    mcp_servers::replace_env(&state.cfg, &state.db, &id, &entries).map_err(ApiError::from)?;
    if stored.spec.kind.is_deployed() {
        mcp_servers::deploy(&state, &stored.spec)
            .await
            .map_err(ApiError::from)?;
        let _ = mcp_servers::refresh(&state, &stored.spec).await;
    }
    record(
        &state,
        &context,
        "mcp_server.env.update",
        &id,
        Some(json!({
            "keys": entries.iter().map(|entry| entry.key.clone()).collect::<Vec<_>>(),
            "secrets": entries.iter().filter(|entry| entry.secret).count(),
        })),
    );
    let stored = load_record(&state, &id).await?;
    Ok(Json(server_view(&state, &stored)?))
}

/// `DELETE /api/mcp/servers/{id}/env/{key}`.
pub async fn delete_env(
    Path((id, key)): Path<(String, String)>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<ServerView>, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&format!("mcp:{id}"));
    let _guard = lock.lock().await;
    let stored = load_record(&state, &id).await?;
    let values = mcp_servers::env_values(&state.cfg, &state.db, &id).map_err(ApiError::from)?;
    if !values.contains_key(&key) {
        return Err(ApiError::not_found(format!(
            "MCP server \"{id}\" has no environment variable \"{key}\""
        )));
    }
    let entries: Vec<EnvWrite> = state
        .db
        .list_mcp_server_env(&id)
        .map_err(ApiError::from)?
        .into_iter()
        .filter(|row| row.key != key)
        .map(|row| {
            let value = values.get(&row.key).cloned().unwrap_or_default();
            EnvWrite {
                key: row.key,
                value,
                secret: row.secret,
            }
        })
        .collect();
    mcp_servers::replace_env(&state.cfg, &state.db, &id, &entries).map_err(ApiError::from)?;
    if stored.spec.kind.is_deployed() {
        mcp_servers::deploy(&state, &stored.spec)
            .await
            .map_err(ApiError::from)?;
    }
    record(
        &state,
        &context,
        "mcp_server.env.delete",
        &id,
        Some(json!({ "key": key })),
    );
    let stored = load_record(&state, &id).await?;
    Ok(Json(server_view(&state, &stored)?))
}

/// Query of `GET /api/mcp/servers/{id}/incidents`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IncidentQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /api/mcp/servers/{id}/incidents`.
pub async fn incidents(
    Path(id): Path<String>,
    Query(query): Query<IncidentQuery>,
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::models::McpIncident>>, ApiError> {
    load_record(&state, &id).await?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let incidents = state
        .db
        .list_mcp_incidents(Some(&id), query.status.as_deref(), limit)
        .map_err(ApiError::from)?;
    Ok(Json(incidents))
}

/// `POST /api/mcp-incidents/{id}/close`.
pub async fn close_incident(
    Path(id): Path<i64>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<crate::models::McpIncident>, ApiError> {
    require_admin(&context)?;
    let incident = state.db.close_mcp_incident(id).map_err(ApiError::from)?;
    let Some(incident) = incident else {
        return Err(ApiError::not_found(format!(
            "MCP incident {id} not found or already closed"
        )));
    };
    record(
        &state,
        &context,
        "mcp_incident.close",
        &incident.server_id,
        Some(json!({ "incident": id })),
    );
    Ok(Json(incident))
}

// ---------------------------------------------------------------------------
// Attachment to agents
// ---------------------------------------------------------------------------

/// One attached server, with the resolved metadata the console shows.
#[derive(Debug, Clone, Serialize)]
pub struct AttachedServer {
    pub server: String,
    pub mode: String,
    pub kind: String,
    pub status: String,
    pub health: String,
    pub tools_count: u32,
    pub endpoint: Option<String>,
}

/// Response of the instance MCP routes.
#[derive(Debug, Clone, Serialize)]
pub struct InstanceMcpResponse {
    pub id: String,
    pub status: String,
    pub servers: Vec<AttachedServer>,
}

fn attached(state: &AppState, refs: &[McpRef]) -> Result<Vec<AttachedServer>, ApiError> {
    let mut out = Vec::with_capacity(refs.len());
    for reference in refs {
        let records = state.db.list_mcp_servers().map_err(ApiError::from)?;
        let Some(record) = records
            .into_iter()
            .find(|candidate| candidate.spec.id == reference.server)
        else {
            continue;
        };
        let spec = &record.spec;
        out.push(AttachedServer {
            server: spec.id.clone(),
            mode: reference.mode.as_str().to_string(),
            kind: spec.kind.as_str().to_string(),
            status: spec.status.as_str().to_string(),
            health: spec.health.as_str().to_string(),
            tools_count: record.tools_count,
            endpoint: spec
                .kind
                .is_deployed()
                .then(|| mcp_servers::endpoint(&spec.id, spec.port, &spec.path)),
        });
    }
    Ok(out)
}

/// Render `config.yaml` with the current attachments, then recreate the agent.
///
/// `config.yaml` is bind-mounted, but the compose service is recreated through
/// the shared `apply_compose` path so a change of the managed block is applied
/// exactly like any other instance change (and reported with its status).
async fn apply_attachment(
    state: &AppState,
    spec: &mut crate::models::InstanceSpec,
) -> Result<Status, ApiError> {
    let keys = crate::api::compose_env_keys(state, spec)?;
    let rendered = instances::render_for_update(state, spec, &keys)?;
    crate::store::write_instance(&state.cfg, spec, &rendered, false)
        .map_err(|err| ApiError::internal(format!("failed to write instance files: {err}")))?;
    crate::api::apply_compose(state, spec).await?;
    let status = state
        .driver
        .ps(spec)
        .await
        .map(|result| result.status)
        .unwrap_or(Status::Running);
    Ok(status)
}

/// `GET /api/instances/{id}/mcp-servers`.
pub async fn instance_list(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<InstanceMcpResponse>, ApiError> {
    let spec = instances::load(&state, &id)?;
    Ok(Json(InstanceMcpResponse {
        id: spec.id.clone(),
        status: spec.status.as_str().to_string(),
        servers: attached(&state, &spec.mcp_servers)?,
    }))
}

/// Body of `PUT /api/instances/{id}/mcp-servers`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PutInstanceMcpRequest {
    #[serde(default)]
    pub servers: Vec<mcp_servers::McpRefInput>,
}

/// `PUT /api/instances/{id}/mcp-servers` — replace the whole attachment list.
pub async fn instance_put(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PutInstanceMcpRequest>,
) -> Result<Json<InstanceMcpResponse>, ApiError> {
    require_admin(&context)?;
    let refs = mcp_servers::normalize_refs(&request.servers).map_err(ApiError::from)?;
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;
    let mut spec = instances::load(&state, &id)?;
    ensure_servers_exist(&state, &refs)?;
    state
        .db
        .set_instance_mcp_servers(&id, &refs)
        .map_err(ApiError::from)?;
    spec.mcp_servers = refs;
    let status = apply_attachment(&state, &mut spec).await?;
    record(
        &state,
        &context,
        "instance.mcp.attach",
        &id,
        Some(json!({
            "servers": spec.mcp_servers.iter().map(|server| json!({
                "server": server.server,
                "mode": server.mode.as_str(),
            })).collect::<Vec<_>>(),
            "replace": true,
        })),
    );
    Ok(Json(InstanceMcpResponse {
        id: spec.id.clone(),
        status: status.as_str().to_string(),
        servers: attached(&state, &spec.mcp_servers)?,
    }))
}

/// Body of `POST /api/instances/{id}/mcp-servers/{server}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AttachRequest {
    #[serde(default)]
    pub mode: Option<String>,
}

/// `POST /api/instances/{id}/mcp-servers/{server}`.
pub async fn instance_attach(
    Path((id, server)): Path<(String, String)>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    body: Option<Json<AttachRequest>>,
) -> Result<Json<InstanceMcpResponse>, ApiError> {
    require_admin(&context)?;
    let mode = body
        .and_then(|Json(body)| body.mode)
        .as_deref()
        .map(|value| {
            crate::models::McpMode::parse(value).ok_or_else(|| {
                ApiError::bad_request(
                    "invalid_mcp_server",
                    format!("unknown mode \"{value}\" (expected readonly or rw)"),
                )
            })
        })
        .transpose()?
        .unwrap_or(crate::models::McpMode::Rw);
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;
    let mut spec = instances::load(&state, &id)?;
    let refs = vec![McpRef { server, mode }];
    ensure_servers_exist(&state, &refs)?;
    let mut current = state.db.instance_mcp_servers(&id).map_err(ApiError::from)?;
    match current
        .iter_mut()
        .find(|reference| reference.server == refs[0].server)
    {
        Some(existing) => existing.mode = mode,
        None => current.push(refs[0].clone()),
    }
    current.sort_by(|left, right| left.server.cmp(&right.server));
    state
        .db
        .set_instance_mcp_servers(&id, &current)
        .map_err(ApiError::from)?;
    spec.mcp_servers = current;
    let status = apply_attachment(&state, &mut spec).await?;
    record(
        &state,
        &context,
        "instance.mcp.attach",
        &id,
        Some(json!({ "server": refs[0].server, "mode": mode.as_str() })),
    );
    Ok(Json(InstanceMcpResponse {
        id: spec.id.clone(),
        status: status.as_str().to_string(),
        servers: attached(&state, &spec.mcp_servers)?,
    }))
}

/// `DELETE /api/instances/{id}/mcp-servers/{server}`.
pub async fn instance_detach(
    Path((id, server)): Path<(String, String)>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<InstanceMcpResponse>, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;
    let mut spec = instances::load(&state, &id)?;
    let mut current = state.db.instance_mcp_servers(&id).map_err(ApiError::from)?;
    let before = current.len();
    current.retain(|reference| reference.server != server);
    if current.len() == before {
        return Err(ApiError::not_found(format!(
            "instance \"{id}\" is not attached to MCP server \"{server}\""
        )));
    }
    state
        .db
        .set_instance_mcp_servers(&id, &current)
        .map_err(ApiError::from)?;
    spec.mcp_servers = current;
    let status = apply_attachment(&state, &mut spec).await?;
    record(
        &state,
        &context,
        "instance.mcp.detach",
        &id,
        Some(json!({ "server": server })),
    );
    Ok(Json(InstanceMcpResponse {
        id: spec.id.clone(),
        status: status.as_str().to_string(),
        servers: attached(&state, &spec.mcp_servers)?,
    }))
}

fn ensure_servers_exist(state: &AppState, refs: &[McpRef]) -> Result<(), ApiError> {
    for reference in refs {
        if state
            .db
            .get_mcp_server(&reference.server)
            .map_err(ApiError::from)?
            .is_none()
        {
            return Err(ApiError::not_found(format!(
                "MCP server \"{}\" not found",
                reference.server
            )));
        }
    }
    Ok(())
}

/// Resolved attachment entries of an instance, for the render call sites.
pub fn config_entries_for(
    state: &AppState,
    instance_id: &str,
) -> Result<Vec<McpConfigEntry>, ApiError> {
    mcp_servers::config_entries(&state.cfg, &state.db, instance_id).map_err(ApiError::from)
}

/// Snapshot of a server plus its last refresh, used by the CLI `test` command.
pub struct ProbedServer {
    pub spec: McpServerSpec,
    pub report: RefreshReport,
}
