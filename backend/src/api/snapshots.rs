//! Restore-point endpoints (issue #38).
//!
//! Routes (all admin for mutations, any role for reads):
//!
//! ```text
//! POST   /api/instances/{id}/snapshots           {label?, ttl_s?}  -> 201 Snapshot
//! GET    /api/instances/{id}/snapshots                             -> Snapshot[]
//! POST   /api/instances/{id}/snapshots/compare   {a, b}            -> SnapshotDiff
//! GET    /api/snapshots/{sid}                                      -> Snapshot
//! DELETE /api/snapshots/{sid}                                      -> 204
//! POST   /api/snapshots/{sid}/restore                              -> 200 RestoreReport
//! DELETE /api/snapshots/expired                                    -> {"removed","failed"}
//! ```
//!
//! Creation and restore serialise with every other lifecycle operation of the
//! instance through [`crate::api::InstanceLocks`]; the heavy lifting lives in
//! [`crate::snapshots`]. A failed restore has already put the previous state
//! back and answers `409 restore_failed` with the reason.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::instances;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::snapshots::{self, Snapshot, SnapshotDiff, SnapshotError};
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Body of `POST /api/instances/{id}/snapshots`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateSnapshotRequest {
    #[serde(default)]
    pub label: Option<String>,
    /// Retention in seconds (default 7 days, maximum 90).
    #[serde(default)]
    pub ttl_s: Option<u64>,
}

/// Body of `POST /api/instances/{id}/snapshots/compare`.
#[derive(Debug, Clone, Deserialize)]
pub struct CompareRequest {
    /// Id of the first snapshot.
    pub a: String,
    /// Id of the second snapshot.
    pub b: String,
}

/// Response of `DELETE /api/snapshots/expired`.
#[derive(Debug, Clone, Serialize)]
pub struct PurgeResponse {
    pub removed: usize,
    pub failed: usize,
}

/// `POST /api/instances/{id}/snapshots`.
pub async fn create(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateSnapshotRequest>,
) -> Result<(StatusCode, Json<Snapshot>), ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let spec = instances::load(&state, &id)?;
    let cfg = state.cfg.clone();
    let db = state.db.clone();
    let target = spec.clone();
    let label = request.label.clone();
    let ttl_s = request.ttl_s;
    let snapshot = tokio::task::spawn_blocking(move || {
        snapshots::create(&cfg, &db, &target, label.as_deref(), ttl_s)
    })
    .await
    .map_err(|err| ApiError::internal(format!("the snapshot task failed: {err}")))?
    .map_err(map_error)?;

    audit::record(
        &state,
        &context,
        "instance.snapshot.create",
        &spec.id,
        Some(json!({
            "snapshot": snapshot.id,
            "label": snapshot.label,
            "ttl_s": ttl_s,
            "size_bytes": snapshot.size_bytes,
        })),
    );
    Ok((StatusCode::CREATED, Json(snapshot)))
}

/// `GET /api/instances/{id}/snapshots`, newest first.
pub async fn list(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<Snapshot>>, ApiError> {
    instances::load(&state, &id)?;
    let snapshots = snapshots::list(&state.db, &id).map_err(map_error)?;
    Ok(Json(snapshots))
}

/// `POST /api/instances/{id}/snapshots/compare`.
pub async fn compare(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<CompareRequest>,
) -> Result<Json<SnapshotDiff>, ApiError> {
    instances::load(&state, &id)?;
    let a = request.a.trim();
    let b = request.b.trim();
    if a.is_empty() || b.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_snapshot",
            "a and b are required: send {\"a\":\"<snapshot id>\",\"b\":\"<snapshot id>\"}",
        ));
    }
    let diff = snapshots::compare(&state.db, &id, a, b).map_err(map_error)?;
    Ok(Json(diff))
}

/// `GET /api/snapshots/{sid}`.
pub async fn get(
    Path(sid): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Snapshot>, ApiError> {
    let snapshot = snapshots::get(&state.db, &sid).map_err(map_error)?;
    Ok(Json(snapshot))
}

/// `DELETE /api/snapshots/{sid}`.
pub async fn delete(
    Path(sid): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    let snapshot = snapshots::get(&state.db, &sid).map_err(map_error)?;
    let lock = state.locks.for_id(&snapshot.instance_id);
    let _guard = lock.lock().await;

    let deleted = snapshots::delete(&state.cfg, &state.db, &sid).map_err(map_error)?;
    audit::record(
        &state,
        &context,
        "instance.snapshot.delete",
        &deleted.instance_id,
        Some(json!({ "snapshot": deleted.id, "label": deleted.label })),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/snapshots/{sid}/restore`.
///
/// Runs synchronously: the answer carries the result (or the reason the
/// previous state was put back). The instance lock is held for the whole
/// operation, so nothing else touches the instance while it is replaced.
pub async fn restore(
    Path(sid): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<snapshots::RestoreReport>, ApiError> {
    let snapshot = snapshots::get(&state.db, &sid).map_err(map_error)?;
    let lock = state.locks.for_id(&snapshot.instance_id);
    let _guard = lock.lock().await;

    match snapshots::restore(&state, &sid).await {
        Ok(report) => {
            audit::record(
                &state,
                &context,
                "instance.snapshot.restore",
                &report.instance_id,
                Some(json!({
                    "snapshot": report.snapshot.id,
                    "status": "ok",
                    "files": report.files,
                    "previous_data_dir": report.previous_data_dir,
                })),
            );
            Ok(Json(report))
        }
        Err(err) => {
            audit::record(
                &state,
                &context,
                "instance.snapshot.restore",
                &snapshot.instance_id,
                Some(json!({
                    "snapshot": snapshot.id,
                    "status": "failed",
                    "error": err.to_string(),
                })),
            );
            Err(map_error(err))
        }
    }
}

/// `DELETE /api/snapshots/expired` — DELETE files and rows whose TTL elapsed.
pub async fn purge(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<PurgeResponse>, ApiError> {
    let report = snapshots::purge_expired(&state.cfg, &state.db).map_err(map_error)?;
    audit::record(
        &state,
        &context,
        "instance.snapshot.purge",
        "expired",
        Some(json!({ "removed": report.removed, "failed": report.failed })),
    );
    Ok(Json(PurgeResponse {
        removed: report.removed,
        failed: report.failed,
    }))
}

/// Map the snapshot errors onto the shared API envelope.
fn map_error(err: SnapshotError) -> ApiError {
    match err {
        SnapshotError::NotFound(id) => {
            ApiError::not_found(format!("snapshot \"{id}\" was not found"))
        }
        SnapshotError::InstanceNotFound(id) => {
            ApiError::not_found(format!("instance \"{id}\" not found"))
        }
        SnapshotError::Invalid(detail) => ApiError::bad_request("invalid_snapshot", detail),
        SnapshotError::RestoreFailed { snapshot, message } => ApiError::conflict(
            "restore_failed",
            format!("snapshot \"{snapshot}\" could not be restored: {message}"),
        )
        .with_details(json!({
            "snapshot": snapshot,
            "rolled_back": true,
        })),
        other => ApiError::internal(other.to_string()),
    }
}
