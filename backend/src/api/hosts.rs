//! Host registry endpoints (issue #40).
//!
//! `GET /api/hosts` derives the capacity of every host with a live `docker
//! info`; `POST /api/hosts` registers a daemon and tests it immediately. The
//! placement helper is used by `POST /api/instances` when `host` is `auto`.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::hosts::{
    self, HostCapacity, HostError, HostSpec, STATUS_OK, STATUS_UNKNOWN, STATUS_UNREACHABLE,
};
use crate::models::{now_iso, InstanceSpec};
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::task::JoinSet;

/// Time allowed to one connectivity probe (a dead SSH daemon must not block the
/// whole host list for minutes).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// `POST /api/hosts`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateHostRequest {
    /// Host id (`[a-z0-9][a-z0-9-]{0,31}`); derived from `name` when omitted.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// `local` (default) or `ssh`.
    #[serde(default)]
    pub transport: Option<String>,
    /// `user@host` or `ssh://user@host`; required for `ssh`.
    #[serde(default)]
    pub address: Option<String>,
    /// Docker context name, when the host is selected by context.
    #[serde(default)]
    pub docker_context: Option<String>,
}

/// `PATCH /api/hosts/{id}`. A missing field is left untouched.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PatchHostRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    /// `""` clears the context; a missing field leaves it alone.
    #[serde(default, deserialize_with = "double_option")]
    pub docker_context: Option<Option<String>>,
}

fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// One host, as returned by the API.
#[derive(Debug, Clone, Serialize)]
pub struct HostSummary {
    pub id: String,
    pub name: String,
    pub transport: String,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_context: Option<String>,
    pub status: String,
    pub last_check_at: Option<String>,
    pub last_error: Option<String>,
    /// Number of instances placed on this host.
    pub instances: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<HostCapacity>,
    /// `capacity.mem_total_bytes` minus the declared limits of the instances.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_mem_bytes: Option<u64>,
    /// `capacity.ncpu` minus the declared CPU limits of the instances.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_cpu: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
}

/// `POST /api/hosts/{id}/test`.
#[derive(Debug, Clone, Serialize)]
pub struct HostTestResponse {
    pub ok: bool,
    pub status: String,
    pub checked_at: String,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<HostCapacity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct ProbeOutcome {
    capacity: Option<HostCapacity>,
    error: Option<String>,
}

/// `GET /api/hosts` — every host with its freshly probed capacity.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<HostSummary>>, ApiError> {
    let hosts = state.db.list_hosts().map_err(ApiError::from)?;
    let specs = state.db.list().map_err(ApiError::from)?;

    // Probe every host concurrently: one dead SSH endpoint must not delay the
    // answer for the others.
    let mut tasks = JoinSet::new();
    for host in hosts.clone() {
        let driver = state.driver.clone();
        tasks.spawn(async move {
            let outcome = probe(&*driver, &host).await;
            (host.id, outcome)
        });
    }
    let mut outcomes = std::collections::HashMap::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((id, outcome)) => {
                outcomes.insert(id, outcome);
            }
            Err(err) => {
                tracing::warn!(%err, "a host probe task failed");
            }
        }
    }

    let mut summaries = Vec::with_capacity(hosts.len());
    for host in hosts {
        let outcome = outcomes.remove(&host.id).unwrap_or(ProbeOutcome {
            capacity: None,
            error: Some("the connectivity probe did not complete".to_string()),
        });
        let host = record_probe(&state, host, &outcome)?;
        let placed: Vec<&InstanceSpec> = specs.iter().filter(|spec| spec.host == host.id).collect();
        summaries.push(to_summary(&host, &placed, outcome.capacity));
    }
    Ok(Json(summaries))
}

/// `GET /api/hosts/{id}` — one host, probed now.
pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<HostSummary>, ApiError> {
    let host = load(&state, &id)?;
    let outcome = probe(&*state.driver, &host).await;
    let host = record_probe(&state, host, &outcome)?;
    let specs = state.db.list().map_err(ApiError::from)?;
    let placed: Vec<&InstanceSpec> = specs.iter().filter(|spec| spec.host == host.id).collect();
    Ok(Json(to_summary(&host, &placed, outcome.capacity)))
}

/// `POST /api/hosts` — register a daemon and test it immediately.
pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateHostRequest>,
) -> Result<(StatusCode, Json<HostSummary>), ApiError> {
    let name = request
        .name
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() && request.id.is_none() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "name is required (or an explicit id)",
        ));
    }
    let id = match request
        .id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(id) => id.to_string(),
        None => hosts::slug(&name),
    };
    hosts::validate_host_id(&id)
        .map_err(|err| ApiError::bad_request("invalid_id", err.to_string()))?;
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "name is required when an id is supplied",
        ));
    }
    hosts::validate_host_name(&name)
        .map_err(|err| ApiError::bad_request("invalid_name", err.to_string()))?;
    let transport = hosts::validate_transport(request.transport.as_deref().unwrap_or_default())
        .map_err(|err| ApiError::bad_request("invalid_transport", err.to_string()))?;
    let address =
        hosts::validate_address(request.address.as_deref().unwrap_or_default(), transport)
            .map_err(|err| ApiError::bad_request("invalid_address", err.to_string()))?;
    let docker_context = hosts::validate_context(request.docker_context.as_deref())
        .map_err(|err| ApiError::bad_request("invalid_docker_context", err.to_string()))?;
    let now = now_iso();
    let host = HostSpec {
        id: id.clone(),
        name,
        transport: transport.to_string(),
        address,
        docker_context,
        status: STATUS_UNKNOWN.to_string(),
        last_check_at: None,
        last_error: None,
        created_at: now.clone(),
        updated_at: now,
    };
    if id == hosts::LOCAL_HOST {
        return Err(ApiError::conflict(
            "host_exists",
            "the local host already exists",
        ));
    }
    state.db.insert_host(&host).map_err(|err| match err {
        crate::db::DbError::Conflict(_) => ApiError::conflict(
            "host_exists",
            format!("a host with id \"{id}\" or this name already exists"),
        ),
        other => ApiError::from(other),
    })?;

    let outcome = probe(&*state.driver, &host).await;
    let host = record_probe(&state, host, &outcome)?;
    audit::record(
        &state,
        &context,
        "host.create",
        &host.id,
        Some(serde_json::json!({
            "name": host.name,
            "transport": host.transport,
            "address": host.address,
            "status": host.status,
        })),
    );
    Ok((
        StatusCode::CREATED,
        Json(to_summary(&host, &[], outcome.capacity)),
    ))
}

/// `PATCH /api/hosts/{id}` — update a definition and re-test it.
pub async fn patch(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PatchHostRequest>,
) -> Result<Json<HostSummary>, ApiError> {
    let mut host = load(&state, &id)?;
    if host.is_local()
        && (request.name.is_some() || request.transport.is_some() || request.address.is_some())
    {
        return Err(ApiError::bad_request(
            "host_protected",
            "the local host cannot be renamed or moved; only its docker_context can change",
        ));
    }

    if let Some(name) = request.name.as_deref() {
        let name = name.trim().to_string();
        hosts::validate_host_name(&name)
            .map_err(|err| ApiError::bad_request("invalid_name", err.to_string()))?;
        host.name = name;
    }
    if let Some(transport) = request.transport.as_deref() {
        host.transport = hosts::validate_transport(transport)
            .map_err(|err| ApiError::bad_request("invalid_transport", err.to_string()))?
            .to_string();
    }
    if let Some(address) = request.address.as_deref() {
        host.address = hosts::validate_address(address, &host.transport)
            .map_err(|err| ApiError::bad_request("invalid_address", err.to_string()))?;
    } else if request.transport.is_some() {
        // A transport change re-validates the existing address against it.
        host.address = hosts::validate_address(&host.address, &host.transport)
            .map_err(|err| ApiError::bad_request("invalid_address", err.to_string()))?;
    }
    if let Some(context) = request.docker_context {
        host.docker_context = hosts::validate_context(context.as_deref())
            .map_err(|err| ApiError::bad_request("invalid_docker_context", err.to_string()))?;
    }
    host.updated_at = now_iso();
    state.db.update_host(&host).map_err(|err| match err {
        crate::db::DbError::Conflict(_) => ApiError::conflict(
            "host_exists",
            format!("another host already uses the name \"{}\"", host.name),
        ),
        other => ApiError::from(other),
    })?;

    let outcome = probe(&*state.driver, &host).await;
    let host = record_probe(&state, host, &outcome)?;
    let specs = state.db.list().map_err(ApiError::from)?;
    let placed: Vec<&InstanceSpec> = specs.iter().filter(|spec| spec.host == host.id).collect();
    audit::record(&state, &context, "host.patch", &host.id, None);
    Ok(Json(to_summary(&host, &placed, outcome.capacity)))
}

/// `DELETE /api/hosts/{id}` — refuse the local host and any host still in use.
pub async fn delete(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    let host = load(&state, &id)?;
    if host.is_local() {
        return Err(ApiError::conflict(
            "host_protected",
            "the local host cannot be deleted",
        ));
    }
    let specs = state.db.list().map_err(ApiError::from)?;
    let placed: Vec<String> = specs
        .iter()
        .filter(|spec| spec.host == host.id)
        .map(|spec| spec.id.clone())
        .collect();
    if !placed.is_empty() {
        return Err(ApiError::conflict(
            "host_in_use",
            format!(
                "host \"{}\" still runs {} instance(s): move them first",
                host.id,
                placed.len()
            ),
        )
        .with_details(serde_json::json!({ "instances": placed })));
    }
    let deleted = state.db.delete_host(&host.id).map_err(ApiError::from)?;
    if !deleted {
        return Err(ApiError::not_found(format!(
            "host \"{}\" not found",
            host.id
        )));
    }
    audit::record(&state, &context, "host.delete", &host.id, None);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/hosts/{id}/test` — test one daemon and record the result.
pub async fn test(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<HostTestResponse>, ApiError> {
    let host = load(&state, &id)?;
    let started = Instant::now();
    let outcome = probe(&*state.driver, &host).await;
    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let host = record_probe(&state, host, &outcome)?;
    audit::record(
        &state,
        &context,
        "host.test",
        &host.id,
        Some(serde_json::json!({ "status": host.status })),
    );
    Ok(Json(HostTestResponse {
        ok: outcome.error.is_none(),
        status: host.status,
        checked_at: host.last_check_at.unwrap_or_else(now_iso),
        latency_ms,
        capacity: outcome.capacity,
        error: host.last_error,
    }))
}

/// Resolve the `host` field of a create request, applying placement.
///
/// An explicit host id or name wins and must exist (`400 unknown_host`).
/// `auto`, an empty value or a missing field place the instance on the
/// reachable host with the most free memory, then CPU, then the fewest
/// instances.
pub async fn resolve_placement(
    state: &AppState,
    requested: Option<&str>,
    mem_limit_mb: u32,
    cpu_limit: f64,
) -> Result<String, ApiError> {
    let requested = requested.map(str::trim).unwrap_or_default();
    if !requested.is_empty() && !requested.eq_ignore_ascii_case("auto") {
        let host = state
            .db
            .host_by_ref(requested)
            .map_err(ApiError::from)?
            .ok_or_else(|| {
                ApiError::bad_request(
                    "unknown_host",
                    format!(
                        "host \"{requested}\" is not registered; add it with POST /api/hosts first"
                    ),
                )
            })?;
        return Ok(host.id);
    }

    let hosts = state.db.list_hosts().map_err(ApiError::from)?;
    let specs = state.db.list().map_err(ApiError::from)?;
    let mut best: Option<(String, hosts::HostScore)> = None;
    let mut last_error: Option<String> = None;
    for host in hosts {
        let outcome = probe(&*state.driver, &host).await;
        let Some(capacity) = outcome.capacity else {
            last_error = outcome.error.or(last_error);
            continue;
        };
        let placed: Vec<&InstanceSpec> = specs.iter().filter(|spec| spec.host == host.id).collect();
        // Overcommit is allowed for an explicit host, but automatic placement
        // must respect the declared limits of the requests it already served.
        let used_mem: u64 = placed.iter().map(|spec| u64::from(spec.mem_limit_mb)).sum();
        let requested_mem = u64::from(mem_limit_mb);
        let used_cpu: f64 = placed.iter().map(|spec| spec.cpu_limit).sum();
        if capacity.mem_total_bytes > 0
            && (used_mem + requested_mem) * 1024 * 1024 > capacity.mem_total_bytes
        {
            continue;
        }
        if capacity.ncpu > 0 && used_cpu + cpu_limit > f64::from(capacity.ncpu) {
            continue;
        }
        let score = hosts::score_host(&capacity, &placed);
        best = match best {
            None => Some((host.id, score)),
            Some((current_id, current)) => match hosts::compare_scores(score, current) {
                std::cmp::Ordering::Greater => Some((host.id, score)),
                std::cmp::Ordering::Equal if host.id < current_id => Some((host.id, score)),
                _ => Some((current_id, current)),
            },
        };
    }
    match best {
        Some((id, _)) => Ok(id),
        None => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_reachable_host",
            format!(
                "no Docker host has room for {mem_limit_mb} MB / {cpu_limit} CPU{}",
                last_error
                    .map(|detail| format!(": {detail}"))
                    .unwrap_or_default()
            ),
        )),
    }
}

/// Probe one host and persist the observed status.
fn record_probe(
    state: &AppState,
    mut host: HostSpec,
    outcome: &ProbeOutcome,
) -> Result<HostSpec, ApiError> {
    let checked_at = now_iso();
    let status = if outcome.error.is_none() {
        STATUS_OK
    } else {
        STATUS_UNREACHABLE
    };
    state
        .db
        .set_host_status(&host.id, status, outcome.error.as_deref(), &checked_at)
        .map_err(ApiError::from)?;
    host.status = status.to_string();
    host.last_error = outcome.error.clone();
    host.last_check_at = Some(checked_at);
    Ok(host)
}

async fn probe(driver: &dyn crate::driver::Driver, host: &HostSpec) -> ProbeOutcome {
    match tokio::time::timeout(PROBE_TIMEOUT, driver.probe_host(host)).await {
        Ok(Ok(capacity)) => ProbeOutcome {
            capacity: Some(capacity),
            error: None,
        },
        Ok(Err(err)) => ProbeOutcome {
            capacity: None,
            error: Some(if err.detail.trim().is_empty() {
                err.to_string()
            } else {
                err.detail
            }),
        },
        Err(_) => ProbeOutcome {
            capacity: None,
            error: Some(format!(
                "the connectivity test timed out after {}s",
                PROBE_TIMEOUT.as_secs()
            )),
        },
    }
}

fn to_summary(
    host: &HostSpec,
    placed: &[&InstanceSpec],
    capacity: Option<HostCapacity>,
) -> HostSummary {
    let (free_mem_bytes, free_cpu) = match &capacity {
        Some(capacity) => {
            let used_mem: u64 = placed
                .iter()
                .map(|spec| u64::from(spec.mem_limit_mb) * 1024 * 1024)
                .sum();
            let used_cpu: f64 = placed.iter().map(|spec| spec.cpu_limit).sum();
            (
                Some(capacity.mem_total_bytes.saturating_sub(used_mem)),
                Some((f64::from(capacity.ncpu) - used_cpu).max(0.0)),
            )
        }
        None => (None, None),
    };
    HostSummary {
        id: host.id.clone(),
        name: host.name.clone(),
        transport: host.transport.clone(),
        address: host.address.clone(),
        docker_context: host.docker_context.clone(),
        status: host.status.clone(),
        last_check_at: host.last_check_at.clone(),
        last_error: host.last_error.clone(),
        instances: placed.len(),
        capacity,
        free_mem_bytes,
        free_cpu,
        created_at: host.created_at.clone(),
        updated_at: host.updated_at.clone(),
    }
}

fn load(state: &AppState, id: &str) -> Result<HostSpec, ApiError> {
    state
        .db
        .host(id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("host \"{id}\" not found")))
}

/// Map a host validation error to a 400 with a stable code.
pub fn validation_error(err: HostError) -> ApiError {
    let code = match err {
        HostError::InvalidId(_) => "invalid_id",
        HostError::InvalidName(_) => "invalid_name",
        HostError::InvalidTransport(_) => "invalid_transport",
        HostError::InvalidAddress(_) => "invalid_address",
        HostError::InvalidContext(_) => "invalid_docker_context",
    };
    ApiError::bad_request(code, err.to_string())
}
