//! Per-instance access control lists (issue #37).
//!
//! `GET/PUT /api/instances/{id}/acl` and `DELETE …/acl/{subject}` manage the
//! `instance_acl` rows. Only an admin or an `owner` of the instance may read or
//! change them. Every change is audited with the caller's real identity and the
//! affected subject; grants are read from the database on every request, so a
//! change applies to the next call without a new login.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::{AuthContext, Role};
use crate::authz::{self, InstanceRole};
use crate::models::now_iso;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Upper bound of one instance ACL.
pub const MAX_ACL_ENTRIES: usize = 100;
/// Upper bound of one `subject` (an email or a token name).
pub const MAX_SUBJECT_CHARS: usize = 320;

#[derive(Debug, Clone, Serialize)]
pub struct AclItem {
    pub subject: String,
    pub role: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AclResponse {
    pub instance: String,
    pub acl: Vec<AclItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AclItemRequest {
    pub subject: String,
    pub role: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PutAclRequest {
    #[serde(default)]
    pub acl: Vec<AclItemRequest>,
}

/// Validate one subject string.
fn validate_subject(subject: &str) -> Result<String, ApiError> {
    let subject = subject.trim().to_string();
    if subject.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_acl",
            "subject must not be empty",
        ));
    }
    if subject.chars().count() > MAX_SUBJECT_CHARS {
        return Err(ApiError::bad_request(
            "invalid_acl",
            format!("subject must be at most {MAX_SUBJECT_CHARS} characters"),
        ));
    }
    if subject.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "invalid_acl",
            "subject must not contain control characters",
        ));
    }
    Ok(subject)
}

/// 404 for an unknown instance, 403 unless the caller owns it (or is an admin).
fn authorize(state: &AppState, context: &AuthContext, id: &str) -> Result<(), ApiError> {
    if !state.db.exists(id).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!("instance \"{id}\" not found")));
    }
    if context.role == Role::Admin {
        return Ok(());
    }
    let acl = authz::entries(
        state
            .db
            .acl_for_subject(&context.subject)
            .map_err(ApiError::from)?,
    );
    if authz::instance_role(context, &acl, id) == Some(InstanceRole::Owner) {
        return Ok(());
    }
    Err(ApiError::forbidden(
        "forbidden",
        "only an admin or an owner of the instance may manage its ACL",
    ))
}

/// Current rows as the API representation.
fn response(state: &AppState, id: &str) -> Result<Json<AclResponse>, ApiError> {
    let rows = state.db.list_instance_acl(id).map_err(ApiError::from)?;
    Ok(Json(AclResponse {
        instance: id.to_string(),
        acl: rows
            .into_iter()
            .map(|row| AclItem {
                subject: row.subject,
                role: row.role,
                created_at: row.created_at,
            })
            .collect(),
    }))
}

/// `GET /api/instances/{id}/acl` — admin or instance owner.
pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<AclResponse>, ApiError> {
    authorize(&state, &context, &id)?;
    response(&state, &id)
}

/// `PUT /api/instances/{id}/acl` — replace the whole ACL.
///
/// The body is `{"acl":[{"subject":"…","role":"viewer"}]}`. It replaces every
/// row, so a subject left out loses its access at the next request.
pub async fn put(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PutAclRequest>,
) -> Result<Json<AclResponse>, ApiError> {
    authorize(&state, &context, &id)?;
    if request.acl.len() > MAX_ACL_ENTRIES {
        return Err(ApiError::bad_request(
            "invalid_acl",
            format!("an ACL holds at most {MAX_ACL_ENTRIES} entries"),
        ));
    }

    let mut entries: Vec<(String, String)> = Vec::with_capacity(request.acl.len());
    for item in request.acl {
        let subject = validate_subject(&item.subject)?;
        let role = InstanceRole::parse(&item.role).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_acl",
                "role must be \"owner\", \"operator\" or \"viewer\"",
            )
        })?;
        if entries.iter().any(|(existing, _)| existing == &subject) {
            return Err(ApiError::bad_request(
                "invalid_acl",
                format!("subject \"{subject}\" appears more than once"),
            ));
        }
        entries.push((subject, role.as_str().to_string()));
    }

    state
        .db
        .replace_instance_acl(&id, &entries, &now_iso())
        .map_err(ApiError::from)?;
    audit::record(
        &state,
        &context,
        "instance.acl.update",
        &id,
        Some(json!({
            "subjects": entries.iter().map(|(subject, _)| subject.clone()).collect::<Vec<_>>(),
            "acl": entries
                .iter()
                .map(|(subject, role)| json!({ "subject": subject, "role": role }))
                .collect::<Vec<_>>(),
        })),
    );
    response(&state, &id)
}

/// `DELETE /api/instances/{id}/acl/{subject}` — remove one grant.
pub async fn delete(
    Path((id, subject)): Path<(String, String)>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &context, &id)?;
    if !state
        .db
        .delete_instance_acl(&id, &subject)
        .map_err(ApiError::from)?
    {
        return Err(ApiError::not_found(format!(
            "instance \"{id}\" has no ACL entry for \"{subject}\""
        )));
    }
    audit::record(
        &state,
        &context,
        "instance.acl.delete",
        &id,
        Some(json!({ "subject": subject })),
    );
    Ok(StatusCode::NO_CONTENT)
}
