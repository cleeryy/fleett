//! HTTP routes of the deployment journal (issue #39).

use crate::api::error::ApiError;
use crate::api::AppState;
use crate::deployments::{self, Deployment, DeploymentStatus};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

/// Rows returned when `?limit=` is omitted.
pub const DEFAULT_LIMIT: usize = 50;
/// Upper bound of one page.
pub const MAX_LIMIT: usize = 500;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeploymentsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub status: Option<String>,
}

/// `GET /api/deployments?limit=50&status=ok` — newest first.
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<DeploymentsQuery>,
) -> Result<Json<Vec<Deployment>>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_LIMIT}"),
        ));
    }
    let status = match query.status.as_deref() {
        None | Some("") => None,
        Some(value) => Some(DeploymentStatus::parse(value).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_status",
                "status must be one of pending, running, canary, ok, failed, rolled_back, \
                 cancelled, scheduled"
                    .to_string(),
            )
        })?),
    };
    deployments::list(&state.db, limit, status)
        .map(Json)
        .map_err(ApiError::from)
}

/// `GET /api/deployments/{id}` — one row with its per-instance result.
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Deployment>, ApiError> {
    deployments::get(&state.db, &id)
        .map_err(ApiError::from)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("deployment \"{id}\" not found")))
}
