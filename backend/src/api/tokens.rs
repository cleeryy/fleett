//! Named API tokens (admin only).
//!
//! A token is returned exactly once, at creation. Listing shows metadata only;
//! revoking keeps the row (and its name) for the audit trail.

use crate::api::audit::require_admin;
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::tokens;
use crate::auth::{Action, AuthContext, Role, TokenScope};
use crate::db::NewApiToken;
use crate::models::now_iso;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Longest accepted token name.
pub const MAX_NAME_CHARS: usize = 64;

/// Body of the `scope` field: strings are validated by the handler so the
/// error is a `400 invalid_scope` rather than a deserialization failure.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScopeRequest {
    #[serde(default)]
    pub instances: Vec<String>,
    #[serde(default)]
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTokenRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    /// Optional restriction (`{"instances":[…],"actions":["read","write"]}`).
    #[serde(default)]
    pub scope: Option<ScopeRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    /// `null`/absent for a legacy global token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<TokenScope>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedToken {
    pub id: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<TokenScope>,
    /// The only time the secret is ever returned.
    pub token: String,
}

/// Validate a requested scope and turn it into its stored JSON form.
///
/// Every listed instance must exist and every action must be one of `read`,
/// `write`, `exec`; duplicates are dropped. An empty `instances` or `actions`
/// list is refused: a token that can reach nothing is always a mistake.
fn validate_scope(
    state: &AppState,
    request: Option<ScopeRequest>,
) -> Result<Option<TokenScope>, ApiError> {
    let Some(request) = request else {
        return Ok(None);
    };
    let mut instances: Vec<String> = Vec::new();
    for id in request.instances {
        let id = id.trim().to_string();
        if id.is_empty() {
            return Err(ApiError::bad_request(
                "invalid_scope",
                "scope.instances must not contain an empty id",
            ));
        }
        if !instances.contains(&id) {
            instances.push(id);
        }
    }
    if instances.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_scope",
            "scope.instances must list at least one instance",
        ));
    }
    for id in &instances {
        if !state.db.exists(id).map_err(ApiError::from)? {
            return Err(ApiError::bad_request(
                "invalid_scope",
                format!("unknown instance \"{id}\""),
            ));
        }
    }

    let mut actions: Vec<Action> = Vec::new();
    for value in request.actions {
        let action = Action::parse(&value).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_scope",
                format!("unknown action \"{value}\": expected read, write or exec"),
            )
        })?;
        if !actions.contains(&action) {
            actions.push(action);
        }
    }
    if actions.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_scope",
            "scope.actions must list at least one of read, write, exec",
        ));
    }

    Ok(Some(TokenScope { instances, actions }))
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
                scope: TokenScope::parse_stored(row.scope.as_deref()),
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
    let scope = validate_scope(&state, request.scope)?;

    let secret = tokens::generate();
    let record = NewApiToken {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.clone(),
        role: role.as_str().to_string(),
        hash: tokens::hash(&secret),
        created_at: now_iso(),
        scope: scope.as_ref().map(TokenScope::to_json),
    };
    let id = record.id.clone();
    let created_at = record.created_at.clone();
    state.db.insert_api_token(&record).map_err(ApiError::from)?;

    crate::api::audit::record(
        &state,
        &context,
        "token.create",
        &id,
        Some(json!({ "name": name, "role": role.as_str(), "scope": scope })),
    );

    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            id,
            name,
            role: role.as_str().to_string(),
            created_at,
            scope,
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
