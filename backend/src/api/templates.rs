//! Versioned custom templates (issue #43).
//!
//! Built-in templates stay compiled constants and are read-only. A custom
//! template lives under `$FLEETT_ROOT/templates/<id>/` (metadata, config,
//! `SOUL.md` and defaults) and keeps a git-like history in
//! `template_versions`. See docs/TEMPLATES.md.

use crate::api::audit::{record, require_admin};
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::models::{TemplateDetail, TemplateInfo};
use crate::templates::{
    self, CreateTemplateRequest, FromInstanceRequest, RestoreTemplateRequest, TemplateError,
    TemplateVersion, UpdateTemplateRequest,
};
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::json;

impl From<TemplateError> for ApiError {
    fn from(err: TemplateError) -> Self {
        match err {
            TemplateError::NotFound(id) => {
                ApiError::not_found(format!("template \"{id}\" not found"))
            }
            TemplateError::InstanceNotFound(id) => {
                ApiError::not_found(format!("instance \"{id}\" not found"))
            }
            TemplateError::ReadOnly(id) => ApiError::conflict(
                "read_only",
                format!("template \"{id}\" is built in and read-only"),
            ),
            TemplateError::Exists(id) => ApiError::conflict(
                "template_exists",
                format!("template \"{id}\" already exists"),
            ),
            TemplateError::Invalid(message) => ApiError::bad_request("invalid_template", message),
            TemplateError::Cycle(message) => ApiError::bad_request("template_cycle", message),
            TemplateError::InUse {
                id,
                instances: used_by,
                templates: children,
            } => {
                let mut details = serde_json::Map::new();
                if !used_by.is_empty() {
                    details.insert("instances".to_string(), json!(used_by));
                }
                if !children.is_empty() {
                    details.insert("templates".to_string(), json!(children));
                }
                ApiError::conflict(
                    "template_in_use",
                    format!("template \"{id}\" is still in use"),
                )
                .with_details(serde_json::Value::Object(details))
            }
            TemplateError::VersionNotFound { id, version } => {
                ApiError::not_found(format!("template \"{id}\" has no version {version}"))
            }
            TemplateError::Io(message) | TemplateError::Db(message) => ApiError::internal(message),
        }
    }
}

/// `GET /api/templates`: built-ins plus custom templates.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<TemplateInfo>>, ApiError> {
    let templates = templates::list(&state.cfg, &state.db).map_err(ApiError::from)?;
    Ok(Json(templates))
}

/// `GET /api/templates/{id}`: metadata, files and history count.
pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<TemplateDetail>, ApiError> {
    let detail = templates::detail(&state.cfg, &state.db, &id).map_err(ApiError::from)?;
    Ok(Json(detail))
}

/// `POST /api/templates`.
pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateTemplateRequest>,
) -> Result<(StatusCode, Json<TemplateDetail>), ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id("template:create");
    let _guard = lock.lock().await;

    let detail = templates::create(&state.cfg, &state.db, &request, &context.actor)
        .map_err(ApiError::from)?;
    record(
        &state,
        &context,
        "template.create",
        &detail.info.id,
        Some(json!({
            "parent": detail.info.parent,
            "env": detail.info.env.len(),
            "defaults": detail.info.defaults,
        })),
    );
    Ok((StatusCode::CREATED, Json(detail)))
}

/// `PUT /api/templates/{id}`.
pub async fn update(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<UpdateTemplateRequest>,
) -> Result<Json<TemplateDetail>, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&format!("template:{id}"));
    let _guard = lock.lock().await;

    let detail = templates::update(&state.cfg, &state.db, &id, &request, &context.actor)
        .map_err(ApiError::from)?;
    record(
        &state,
        &context,
        "template.update",
        &id,
        Some(json!({
            "parent": detail.info.parent,
            "env": detail.info.env.len(),
        })),
    );
    Ok(Json(detail))
}

/// `DELETE /api/templates/{id}`.
pub async fn delete(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&format!("template:{id}"));
    let _guard = lock.lock().await;

    templates::delete(&state.cfg, &state.db, &id, &context.actor).map_err(ApiError::from)?;
    record(&state, &context, "template.delete", &id, None);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/templates/from-instance`: save an instance as a template.
pub async fn from_instance(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<FromInstanceRequest>,
) -> Result<(StatusCode, Json<TemplateDetail>), ApiError> {
    require_admin(&context)?;
    let detail = templates::from_instance(&state.cfg, &state.db, &request, &context.actor)
        .map_err(ApiError::from)?;
    record(
        &state,
        &context,
        "template.create",
        &detail.info.id,
        Some(json!({
            "source_instance": detail.info.source_instance,
            "env": detail.info.env.len(),
        })),
    );
    Ok((StatusCode::CREATED, Json(detail)))
}

/// `GET /api/templates/{id}/versions`.
pub async fn versions(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<TemplateVersion>>, ApiError> {
    let versions = templates::versions(&state.cfg, &state.db, &id).map_err(ApiError::from)?;
    Ok(Json(versions))
}

/// `POST /api/templates/{id}/restore`.
pub async fn restore(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<RestoreTemplateRequest>,
) -> Result<Json<TemplateDetail>, ApiError> {
    require_admin(&context)?;
    if request.version <= 0 {
        return Err(ApiError::bad_request(
            "invalid_version",
            "version must be a positive integer",
        ));
    }
    let lock = state.locks.for_id(&format!("template:{id}"));
    let _guard = lock.lock().await;

    let detail = templates::restore(&state.cfg, &state.db, &id, request.version, &context.actor)
        .map_err(ApiError::from)?;
    record(
        &state,
        &context,
        "template.restore",
        &id,
        Some(json!({ "version": request.version })),
    );
    Ok(Json(detail))
}
