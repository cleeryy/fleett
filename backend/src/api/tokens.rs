//! Named API tokens (admin only).
//!
//! A token is returned exactly once, at creation. Listing shows metadata only;
//! revoking keeps the row (and its name) for the audit trail.

use crate::api::audit::require_admin;
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::tokens;
use crate::auth::{AuthContext, Role};
use crate::db::NewApiToken;
use crate::models::now_iso;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Longest accepted token name.
pub const MAX_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTokenRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedToken {
    pub id: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    /// The only time the secret is ever returned.
    pub token: String,
}

/// `GET /api/tokens` — metadata only, never the secret.
pub async fn list(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<Vec<TokenInfo>>, ApiError> {
    require_admin(&context)?;
    let rows = state.db.list_api_tokens().map_err(ApiError::from)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| TokenInfo {
                id: row.id,
                name: row.name,
                role: row.role,
                created_at: row.created_at,
                revoked_at: row.revoked_at,
            })
            .collect(),
    ))
}

/// `POST /api/tokens` — create a named, role-scoped token.
pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreatedToken>), ApiError> {
    require_admin(&context)?;

    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("invalid_name", "name is required"))?
        .to_string();
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::bad_request(
            "invalid_name",
            format!("name must be at most {MAX_NAME_CHARS} characters"),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "invalid_name",
            "name must not contain control characters",
        ));
    }

    let role = match request.role.as_deref() {
        None => Role::Viewer,
        Some(value) => Role::parse(value).ok_or_else(|| {
            ApiError::bad_request("invalid_role", "role must be \"admin\" or \"viewer\"")
        })?,
    };

    let secret = tokens::generate();
    let record = NewApiToken {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.clone(),
        role: role.as_str().to_string(),
        hash: tokens::hash(&secret),
        created_at: now_iso(),
    };
    let id = record.id.clone();
    let created_at = record.created_at.clone();
    state.db.insert_api_token(&record).map_err(ApiError::from)?;

    crate::api::audit::record(
        &state,
        &context,
        "token.create",
        &id,
        Some(json!({ "name": name, "role": role.as_str() })),
    );

    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            id,
            name,
            role: role.as_str().to_string(),
            created_at,
            token: secret,
        }),
    ))
}

/// `DELETE /api/tokens/{id}` — revoke. Idempotent for an already revoked
/// token; `404` for an unknown id.
pub async fn revoke(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;
    if !state.db.api_token_exists(&id).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!("token \"{id}\" not found")));
    }
    state
        .db
        .revoke_api_token(&id, &now_iso())
        .map_err(ApiError::from)?;
    crate::api::audit::record(&state, &context, "token.revoke", &id, None);
    Ok(StatusCode::NO_CONTENT)
}
