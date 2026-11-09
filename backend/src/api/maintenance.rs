//! HTTP routes of the maintenance windows (issue #39).

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::maintenance::{self, MaintenanceWindow, NewWindow, WindowPatch, WindowsStatus};
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use serde_json::json;

/// `GET /api/maintenance-windows` — every window, sorted by name.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<MaintenanceWindow>>, ApiError> {
    maintenance::list(&state.db)
        .map(Json)
        .map_err(ApiError::from)
}

/// `POST /api/maintenance-windows` — create one (admin).
pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(body): Json<NewWindow>,
) -> Result<(StatusCode, Json<MaintenanceWindow>), ApiError> {
    let window = maintenance::insert(&state.db, &body)?;
    audit::record(
        &state,
        &context,
        "maintenance_window.create",
        &window.id,
        Some(json!({
            "name": window.name,
            "days": window.days,
            "start_minute": window.start_minute,
            "end_minute": window.end_minute,
            "enabled": window.enabled,
        })),
    );
    Ok((StatusCode::CREATED, Json(window)))
}

/// `PATCH /api/maintenance-windows/{id}` — change some fields (admin).
pub async fn patch(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(body): Json<WindowPatch>,
) -> Result<Json<MaintenanceWindow>, ApiError> {
    let window = maintenance::update(&state.db, &id, &body)?;
    audit::record(
        &state,
        &context,
        "maintenance_window.update",
        &window.id,
        Some(json!({
            "name": window.name,
            "days": window.days,
            "start_minute": window.start_minute,
            "end_minute": window.end_minute,
            "enabled": window.enabled,
        })),
    );
    Ok(Json(window))
}

/// `DELETE /api/maintenance-windows/{id}` (admin).
pub async fn delete(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !maintenance::delete(&state.db, &id).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!(
            "maintenance window \"{id}\" not found"
        )));
    }
    audit::record(&state, &context, "maintenance_window.delete", &id, None);
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/maintenance-windows/status` — gate state and next opening.
///
/// `open` is `true` when a destructive operation would be allowed right now
/// (no window enforced, no enabled window, or one is open). `next_open` is the
/// RFC 3339 UTC opening used by the `403 maintenance_window` error.
pub async fn status(State(state): State<AppState>) -> Result<Json<WindowsStatus>, ApiError> {
    maintenance::status(&state.db, state.cfg.maintenance_enforce, Utc::now())
        .map(Json)
        .map_err(ApiError::from)
}
