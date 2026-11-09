//! Disk quotas, usage detail and cleanup (issue #47).
//!
//! * `GET  /api/instances/{id}/quota` — current quota, usage and level;
//! * `PUT  /api/instances/{id}/quota` — change the quota or the policy;
//! * `GET  /api/instances/{id}/usage/detail` — data breakdown, Docker log size,
//!   image size and the last journal entry;
//! * `POST /api/instances/{id}/quota/cleanup` — purge `tmp/`, `cache/` and old
//!   rotated logs, returning what was freed;
//! * `GET  /api/disk/events?instance=&limit=` — the journal, newest first.
//!
//! Reads are open to every authenticated role (like `GET /api/usage`); the
//! mutations require an admin. See `docs/QUOTA.md`.

use crate::api::audit::{record, require_admin};
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::models::{validate_disk_quota_mb, DiskPolicy, InstanceSpec, DISK_WARN_PERCENT};
use crate::quota::{
    self, assess, data_dir, dir_breakdown, DiskEvent, QuotaView, CLEANUP_LOG_AGE_DAYS,
    DEFAULT_EVENTS_LIMIT, MAX_EVENTS_LIMIT,
};
use axum::extract::{Extension, Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

/// `GET /api/instances/{id}/quota` and part of the detail view.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaResponse {
    pub instance: String,
    pub quota_mb: u32,
    pub policy: String,
    pub used_bytes: u64,
    pub quota_bytes: u64,
    pub percent: f64,
    pub state: String,
    pub warn_percent: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event: Option<DiskEvent>,
}

/// Body of `PUT /api/instances/{id}/quota`.
///
/// Both fields are optional: a missing one keeps its stored value, which is
/// what `fleett quota alice --set 500` relies on.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QuotaUpdate {
    #[serde(default)]
    pub quota_mb: Option<u32>,
    #[serde(default)]
    pub policy: Option<String>,
}

/// `GET /api/instances/{id}/usage/detail`.
#[derive(Debug, Clone, Serialize)]
pub struct UsageDetailResponse {
    pub instance: String,
    pub data_dir: String,
    /// Size split of the data directory (filesystem truth).
    pub data: quota::DirBreakdown,
    /// `null` when Docker does not report a json-file log for the container.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_log_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_log_bytes: Option<u64>,
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_bytes: Option<u64>,
    pub quota: QuotaView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event: Option<DiskEvent>,
}

/// Query of `GET /api/disk/events`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub instance: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

fn load(state: &AppState, id: &str) -> Result<InstanceSpec, ApiError> {
    state
        .db
        .get(id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("instance \"{id}\" not found")))
}

/// Data size of one instance: driver sample first, filesystem walk as fallback.
///
/// The fallback keeps the quota endpoints useful when the Docker daemon is
/// down: the data directory is still measurable, only `docker stats` is not.
async fn used_bytes(state: &AppState, spec: &InstanceSpec) -> u64 {
    let specs = std::slice::from_ref(spec);
    if let Ok(samples) = state.driver.usage(specs).await {
        if let Some(sample) = samples.first() {
            return sample.disk_bytes;
        }
    }
    dir_breakdown(&data_dir(&state.cfg, &spec.id)).total_bytes
}

fn to_response(state: &AppState, spec: &InstanceSpec, used: u64) -> QuotaResponse {
    let view = quota::view(spec, used);
    QuotaResponse {
        instance: view.instance,
        quota_mb: view.quota_mb,
        policy: view.policy,
        used_bytes: view.used_bytes,
        quota_bytes: view.quota_bytes,
        percent: view.percent,
        state: view.state,
        warn_percent: DISK_WARN_PERCENT,
        last_event: quota::last_event(&state.db, &spec.id).ok().flatten(),
    }
}

pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<QuotaResponse>, ApiError> {
    let spec = load(&state, &id)?;
    let used = used_bytes(&state, &spec).await;
    Ok(Json(to_response(&state, &spec, used)))
}

pub async fn put(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<QuotaUpdate>,
) -> Result<Json<QuotaResponse>, ApiError> {
    require_admin(&context)?;
    let spec = load(&state, &id)?;
    if request.quota_mb.is_none() && request.policy.is_none() {
        return Err(ApiError::bad_request(
            "invalid_quota",
            "set quota_mb, policy, or both",
        ));
    }
    let quota_mb = request.quota_mb.unwrap_or(spec.disk_quota_mb);
    validate_disk_quota_mb(quota_mb)
        .map_err(|message| ApiError::bad_request("invalid_quota", message))?;
    let policy = match request.policy.as_deref() {
        None => spec.disk_policy,
        Some(value) => DiskPolicy::parse(value).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_policy",
                "policy must be \"warn\", \"freeze\" or \"stop\"",
            )
        })?,
    };

    let updated = state
        .db
        .set_disk_quota(&spec.id, quota_mb, policy)
        .map_err(ApiError::from)?;
    if updated == 0 {
        return Err(ApiError::not_found(format!(
            "instance \"{id}\" disappeared"
        )));
    }
    // A changed quota changes the level: forget the previous crossing so the
    // next tick evaluates the new threshold instead of the old one.
    state.quota.forget(&spec.id);

    let mut spec = load(&state, &id)?;
    spec.disk_quota_mb = quota_mb;
    spec.disk_policy = policy;
    record(
        &state,
        &context,
        "instance.quota.update",
        &spec.id,
        Some(serde_json::json!({
            "quota_mb": quota_mb,
            "policy": policy.as_str(),
        })),
    );
    let used = used_bytes(&state, &spec).await;
    Ok(Json(to_response(&state, &spec, used)))
}

pub async fn usage_detail(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<UsageDetailResponse>, ApiError> {
    let spec = load(&state, &id)?;
    let data = dir_breakdown(&data_dir(&state.cfg, &spec.id));
    let used = match state.driver.usage(std::slice::from_ref(&spec)).await {
        Ok(samples) => samples
            .first()
            .map(|sample| sample.disk_bytes)
            .unwrap_or(data.total_bytes),
        Err(_) => data.total_bytes,
    };

    let (docker_log_path, docker_log_bytes) = match state.driver.log_path(&spec).await {
        Ok(Some(path)) => {
            let bytes = std::fs::metadata(&path).ok().map(|metadata| metadata.len());
            (Some(path.display().to_string()), bytes)
        }
        _ => (None, None),
    };
    let image_bytes = state.driver.image_size(&spec.image).await.ok().flatten();
    let view = quota::view(&spec, used);

    Ok(Json(UsageDetailResponse {
        instance: spec.id.clone(),
        data_dir: data_dir(&state.cfg, &spec.id).display().to_string(),
        data,
        docker_log_path,
        docker_log_bytes,
        image: spec.image.clone(),
        image_bytes,
        quota: view,
        last_event: quota::last_event(&state.db, &spec.id).ok().flatten(),
    }))
}

pub async fn cleanup(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<quota::CleanupReport>, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let spec = load(&state, &id)?;
    let dir = data_dir(&state.cfg, &id);
    let mut report =
        tokio::task::spawn_blocking(move || quota::cleanup_data_dir(&dir, CLEANUP_LOG_AGE_DAYS))
            .await
            .map_err(|err| ApiError::internal(format!("the cleanup task failed: {err}")))?
            .map_err(|err| {
                ApiError::internal(format!(
                    "cannot clean the data directory of \"{id}\": {err}"
                ))
            })?;
    report.instance = spec.id.clone();

    let event = quota::record_event(
        &state.db,
        &quota::NewDiskEvent {
            instance_id: spec.id.clone(),
            action: quota::ACTION_CLEAN.to_string(),
            before_bytes: report.before_bytes,
            after_bytes: report.after_bytes,
            detail: Some(format!(
                "removed {} file(s) from tmp/, cache/ and rotated logs older than {} day(s)",
                report.removed_files, CLEANUP_LOG_AGE_DAYS
            )),
        },
    )
    .map_err(ApiError::from)?;
    quota::notify(&event);
    // The new level is recorded so the next tick does not re-apply the policy
    // while the instance is still above the threshold after a partial cleanup.
    state
        .quota
        .observe(&spec.id, assess(report.after_bytes, spec.disk_quota_mb));
    state.metrics.invalidate_usage();

    record(
        &state,
        &context,
        "instance.quota.cleanup",
        &spec.id,
        Some(serde_json::json!({
            "before_bytes": report.before_bytes,
            "after_bytes": report.after_bytes,
            "freed_bytes": report.freed_bytes,
            "removed_files": report.removed_files,
        })),
    );
    Ok(Json(report))
}

pub async fn events(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<DiskEvent>>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_EVENTS_LIMIT as u32) as usize;
    if limit == 0 || limit > MAX_EVENTS_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_EVENTS_LIMIT}"),
        ));
    }
    let instance = query
        .instance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let events = quota::list_events(&state.db, instance, limit).map_err(ApiError::from)?;
    Ok(Json(events))
}

/// 409 `insufficient_disk` for a create that would eat the host reserve.
pub(crate) fn insufficient_disk(err: quota::InsufficientDisk) -> ApiError {
    let capacity = err.0;
    ApiError::conflict(
        "insufficient_disk",
        format!(
            "refusing to create the instance: the host has {} MB free but {} MB must stay free \
             (FLEETT_MIN_FREE_DISK_MB or twice the instance quota)",
            capacity.free_mb, capacity.required_mb
        ),
    )
    .with_details(serde_json::json!({
        "free_mb": capacity.free_mb,
        "required_mb": capacity.required_mb,
    }))
}

/// Refuse a create that would leave the host below its reserve.
///
/// `df` is read synchronously (a few milliseconds); a host where `df` cannot
/// be read counts as 0 bytes free, the conservative choice: the operator can
/// disable the guard with `FLEETT_MIN_FREE_DISK_MB=0`.
pub(crate) fn enforce_host_disk(state: &AppState, spec: &InstanceSpec) -> Result<(), ApiError> {
    if state.cfg.min_free_disk_mb == 0 && spec.disk_quota_mb == 0 {
        return Ok(());
    }
    let free_bytes = quota::host_free_bytes(&state.cfg.root).unwrap_or(0);
    quota::ensure_capacity(free_bytes, spec.disk_quota_mb, state.cfg.min_free_disk_mb)
        .map_err(insufficient_disk)
}
