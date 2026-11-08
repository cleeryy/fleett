//! Audit trail of every successful mutation.

use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::{AuthContext, Role};
use crate::db::NewAuditRecord;
use crate::models::now_iso;
use axum::extract::{Extension, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Entries returned when `?limit=` is omitted.
pub const DEFAULT_LIMIT: usize = 200;
/// Upper bound of one page.
pub const MAX_LIMIT: usize = 1000;

#[derive(Debug, Clone, Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub at: String,
    pub actor: String,
    pub role: String,
    pub method: String,
    pub action: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Append one entry. A database failure is logged, never returned: the
/// mutation already happened, failing the response would be a lie.
pub fn record(
    state: &AppState,
    context: &AuthContext,
    action: &str,
    target: &str,
    details: Option<Value>,
) {
    let entry = NewAuditRecord {
        at: now_iso(),
        actor: context.actor.clone(),
        role: context.role.as_str().to_string(),
        method: context.method.as_str().to_string(),
        action: action.to_string(),
        target: target.to_string(),
        ip: context.ip.clone(),
        details: details.map(|value| value.to_string()),
    };
    if let Err(err) = state.db.record_audit(&entry) {
        tracing::warn!(%err, action, target, "could not append to the audit log");
    }
}

/// `GET /api/audit?limit=200` — admin only, newest first.
pub async fn list(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Vec<AuditEntry>>, ApiError> {
    require_admin(&context)?;
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT as u32) as usize;
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_LIMIT}"),
        ));
    }
    let entries = state.db.list_audit(limit).map_err(ApiError::from)?;
    Ok(Json(
        entries
            .into_iter()
            .map(|entry| AuditEntry {
                id: entry.id,
                at: entry.at,
                actor: entry.actor,
                role: entry.role,
                method: entry.method,
                action: entry.action,
                target: entry.target,
                ip: entry.ip,
                details: entry.details.and_then(|text| {
                    serde_json::from_str(&text)
                        .ok()
                        .or(Some(Value::String(text)))
                }),
            })
            .collect(),
    ))
}

pub fn require_admin(context: &AuthContext) -> Result<(), ApiError> {
    if context.role == Role::Admin {
        return Ok(());
    }
    Err(ApiError::forbidden(
        "forbidden",
        "this endpoint requires the admin role",
    ))
}
