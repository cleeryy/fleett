//! Incident registry and self-healing policy endpoints (issue #32).
//!
//! The watchdog ([`crate::watchdog`]) fills the registry; these routes read it
//! and let an operator change the policy or close an incident by hand. Never
//! does a read trigger a sample: `fleett incidents` must not wake Docker.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::instances::load;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::models::{
    validate_heal_backoff, validate_max_restarts, validate_unhealthy_after, HealPolicy,
};
use crate::watchdog::{self, HealPolicyView, Incident, IncidentStatus, DEFAULT_LIMIT, MAX_LIMIT};
use axum::extract::{Extension, Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct IncidentsQuery {
    /// `open` or `closed`.
    #[serde(default)]
    pub status: Option<String>,
    /// Restrict the page to one instance.
    #[serde(default)]
    pub instance: Option<String>,
    /// Page size (1..=500, default 100).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PageQuery {
    /// Page size (1..=500, default 100).
    #[serde(default)]
    pub limit: Option<usize>,
}

fn page_limit(value: Option<usize>) -> Result<usize, ApiError> {
    let limit = value.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_LIMIT}"),
        ));
    }
    Ok(limit)
}

/// `GET /api/incidents?status=&instance=&limit=` — newest first.
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<IncidentsQuery>,
) -> Result<Json<Vec<Incident>>, ApiError> {
    let status = match query.status.as_deref() {
        None => None,
        Some(value) => Some(IncidentStatus::parse(value).ok_or_else(|| {
            ApiError::bad_request("invalid_status", "status must be open or closed")
        })?),
    };
    let limit = page_limit(query.limit)?;
    if let Some(instance) = query.instance.as_deref() {
        if !state.db.exists(instance).map_err(ApiError::from)? {
            return Err(ApiError::not_found(format!(
                "instance \"{instance}\" not found"
            )));
        }
    }
    let incidents = watchdog::list(&state.db, status, query.instance.as_deref(), limit)
        .map_err(ApiError::from)?;
    Ok(Json(incidents))
}

/// `GET /api/instances/{id}/incidents` — every incident of one instance.
pub async fn instance_incidents(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> Result<Json<Vec<Incident>>, ApiError> {
    load(&state, &id)?;
    let limit = page_limit(query.limit)?;
    let incidents = watchdog::list(&state.db, None, Some(&id), limit).map_err(ApiError::from)?;
    Ok(Json(incidents))
}

/// `POST /api/incidents/{id}/close` — operator override.
///
/// Closing an incident resets the heal budget of its instance: the next
/// unhealthy sample opens a fresh incident and may heal again.
pub async fn close(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<Incident>, ApiError> {
    audit::require_admin(&context)?;
    let mut incident = watchdog::get(&state.db, &id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("incident \"{id}\" not found")))?;
    if !watchdog::close(&state.db, &mut incident, &crate::models::now_iso())
        .map_err(ApiError::from)?
    {
        return Err(ApiError::conflict(
            "incident_closed",
            format!("incident \"{id}\" is already closed"),
        ));
    }
    state.watchdog.reset(&incident.instance_id);
    audit::record(
        &state,
        &context,
        "incident.close",
        &incident.id,
        Some(json!({
            "instance": incident.instance_id,
            "cause": incident.cause.as_str(),
            "opened_at": incident.opened_at,
        })),
    );
    Ok(Json(incident))
}

/// Body of `PATCH /api/instances/{id}/heal`. A missing field is left as is.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HealRequest {
    /// `off`, `restart` or `recreate`.
    #[serde(default)]
    pub heal: Option<String>,
    #[serde(default)]
    pub unhealthy_after: Option<u32>,
    #[serde(default)]
    pub max_restarts: Option<u32>,
    #[serde(default)]
    pub heal_backoff_s: Option<u64>,
}

/// `PATCH /api/instances/{id}/heal` — change the self-healing policy.
pub async fn patch_heal(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<HealRequest>,
) -> Result<Json<HealPolicyView>, ApiError> {
    audit::require_admin(&context)?;
    let mut spec = load(&state, &id)?;
    let before = spec.clone();

    if let Some(value) = request.heal.as_deref() {
        spec.heal = HealPolicy::parse(value).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_heal_policy",
                "heal must be off, restart or recreate",
            )
        })?;
    }
    if let Some(value) = request.unhealthy_after {
        validate_unhealthy_after(value)
            .map_err(|message| ApiError::bad_request("invalid_unhealthy_after", message))?;
        spec.unhealthy_after = value;
    }
    if let Some(value) = request.max_restarts {
        validate_max_restarts(value)
            .map_err(|message| ApiError::bad_request("invalid_max_restarts", message))?;
        spec.max_restarts = value;
    }
    if let Some(value) = request.heal_backoff_s {
        validate_heal_backoff(value)
            .map_err(|message| ApiError::bad_request("invalid_heal_backoff_s", message))?;
        spec.heal_backoff_s = value;
    }

    state
        .db
        .set_heal_policy(
            &spec.id,
            spec.heal,
            spec.unhealthy_after,
            spec.max_restarts,
            spec.heal_backoff_s,
        )
        .map_err(ApiError::from)?;
    // A policy change starts a fresh budget: the operator asked for a new
    // behaviour, the previous attempts must not lock it out.
    state.watchdog.reset(&spec.id);

    audit::record(
        &state,
        &context,
        "instance.heal",
        &spec.id,
        Some(watchdog::policy_details(&before, &spec)),
    );
    Ok(Json(HealPolicyView::from(&spec)))
}
