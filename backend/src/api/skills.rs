//! Shared skill registry routes (issue #54).
//!
//! The registry itself — files, versions, zip import, git — lives in
//! [`crate::skills`]. This module owns authentication, the locks, the audit
//! trail and the propagation to instances. Reads need any authenticated role;
//! every mutation needs `admin`, exactly like personas and templates.

use crate::api::audit::{record, record_result, require_admin};
use crate::api::error::ApiError;
use crate::api::{instances, AppState};
use crate::auth::AuthContext;
use crate::history::{self, CommitInfo, DiffResponse};
use crate::skills::{
    self, PromoteRequest, PromoteResponse, PromoteResult, PublishSkillRequest, SkillDetail,
    SkillError, SkillSummary,
};
use axum::extract::{Extension, Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

/// Commits returned when `?limit=` is omitted.
pub const DEFAULT_LOG_LIMIT: usize = 50;
/// Upper bound of `?limit=`.
pub const MAX_LOG_LIMIT: usize = 500;
/// Time allowed to one instance to become healthy after a promote restart.
pub const PROMOTE_HEALTH_TIMEOUT: Duration = Duration::from_secs(60);
/// Largest multipart body accepted by `POST /api/skills/import` (the zip plus
/// its multipart framing).
pub const MAX_IMPORT_BODY: usize = skills::MAX_ZIP_BYTES + 64 * 1024;

impl From<SkillError> for ApiError {
    fn from(err: SkillError) -> Self {
        match err {
            SkillError::NotFound(name) => {
                ApiError::not_found(format!("skill \"{name}\" not found"))
            }
            SkillError::Exists(name) => {
                ApiError::conflict("skill_exists", format!("skill \"{name}\" already exists"))
            }
            SkillError::VersionExists { name, version } => ApiError::conflict(
                "skill_version_exists",
                format!("version \"{version}\" of skill \"{name}\" already exists"),
            ),
            SkillError::UnknownVersion { name, version } => ApiError::not_found(format!(
                "version \"{version}\" of skill \"{name}\" does not exist"
            )),
            SkillError::UnknownCommit(revision) => ApiError::not_found(format!(
                "commit \"{revision}\" was not found in the skill history"
            )),
            SkillError::Invalid(message) => ApiError::bad_request("invalid_skill", message),
            SkillError::InUse { id, instances } => ApiError::conflict(
                "skill_in_use",
                format!("skill \"{id}\" is still mounted by an instance"),
            )
            .with_details(json!({ "instances": instances })),
            SkillError::Io(message) | SkillError::Git(message) | SkillError::Db(message) => {
                ApiError::internal(message)
            }
        }
    }
}

/// Query of `GET /api/skills/{name}/git/log`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Query of `GET /api/skills/{name}/git/diff`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DiffQuery {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

/// Response of `GET /api/skills/{name}/git/log`.
#[derive(Debug, Clone, Serialize)]
pub struct LogResponse {
    pub commits: Vec<CommitInfo>,
}

/// `GET /api/skills`: the registry.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<SkillSummary>>, ApiError> {
    let skills = skills::list(&state.cfg, &state.db).map_err(ApiError::from)?;
    Ok(Json(skills))
}

/// `GET /api/skills/{name}`: versions, files and the instances that mount it.
pub async fn detail(
    Path(name): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<SkillDetail>, ApiError> {
    let detail = skills::detail(&state.cfg, &state.db, &name).map_err(ApiError::from)?;
    Ok(Json(detail))
}

/// `POST /api/skills` — publish one version.
pub async fn publish(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PublishSkillRequest>,
) -> Result<(StatusCode, Json<SkillDetail>), ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id("skill:publish");
    let _guard = lock.lock().await;

    let author = history::author_for(&context);
    let detail =
        skills::publish(&state.cfg, &state.db, &request, &author).map_err(ApiError::from)?;
    let version = request
        .version
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    record(
        &state,
        &context,
        "skill.publish",
        &detail.name,
        Some(json!({
            "version": version,
            "files": detail.files.len(),
            "versions": detail.versions.len(),
        })),
    );
    Ok((StatusCode::CREATED, Json(detail)))
}

/// `POST /api/skills/import` — publish a version from a zip archive.
///
/// The first file part of the multipart body is read (the console sends it as
/// `file`); every other part is ignored. Validation is the same as a JSON
/// publish, plus zip-slip and entry-size checks.
pub async fn import(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<SkillDetail>), ApiError> {
    require_admin(&context)?;
    let mut archive: Option<Vec<u8>> = None;
    loop {
        let field = multipart.next_field().await.map_err(|err| {
            ApiError::bad_request("invalid_import", format!("cannot read the upload: {err}"))
        })?;
        let Some(field) = field else {
            break;
        };
        if archive.is_some() {
            continue;
        }
        let filename = field.file_name().map(str::to_string);
        let name = field.name().unwrap_or_default().to_string();
        if filename.is_none() && name != "file" && name != "archive" {
            continue;
        }
        let bytes = field.bytes().await.map_err(|err| {
            ApiError::bad_request("invalid_import", format!("cannot read the upload: {err}"))
        })?;
        archive = Some(bytes.to_vec());
    }
    let Some(bytes) = archive else {
        return Err(ApiError::bad_request(
            "invalid_import",
            "the request must contain a zip file (multipart field \"file\")",
        ));
    };

    let lock = state.locks.for_id("skill:publish");
    let _guard = lock.lock().await;
    let author = history::author_for(&context);
    let detail =
        skills::publish_zip(&state.cfg, &state.db, &bytes, &author).map_err(ApiError::from)?;
    let version = detail
        .versions
        .last()
        .map(|entry| entry.version.clone())
        .unwrap_or_default();
    record(
        &state,
        &context,
        "skill.import",
        &detail.name,
        Some(json!({
            "version": version,
            "bytes": bytes.len(),
        })),
    );
    Ok((StatusCode::CREATED, Json(detail)))
}

/// `POST /api/skills/{name}/promote` — move `latest` and restart the instances
/// that follow it.
///
/// Each follower is handled sequentially, under its instance lock: rewrite the
/// compose file (the resolved mount path changed), `up -d`, then wait for a
/// healthy container. A failure is reported in the array and never aborts the
/// remaining instances.
pub async fn promote(
    Path(name): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PromoteRequest>,
) -> Result<Json<PromoteResponse>, ApiError> {
    require_admin(&context)?;
    let version = request
        .version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request("invalid_version", "version is required (for example v2)")
        })?;

    let lock = state.locks.for_id(&format!("skill:{name}"));
    let _guard = lock.lock().await;
    let author = history::author_for(&context);
    let latest = skills::promote(&state.cfg, &name, version, &author).map_err(ApiError::from)?;
    let followers = state
        .db
        .instances_following_skill(&name)
        .map_err(ApiError::from)?;

    let mut results = Vec::with_capacity(followers.len());
    for instance_id in followers {
        let lock = state.locks.for_id(&instance_id);
        let _guard = lock.lock().await;
        match promote_one(&state, &instance_id).await {
            Ok(()) => results.push(PromoteResult {
                id: instance_id,
                status: "restarted",
                error: None,
            }),
            Err(message) => {
                record_result(
                    &state,
                    &context,
                    "instance.skills",
                    &instance_id,
                    Some(json!({ "skill": name, "version": latest, "code": "promote_failed" })),
                    "failed",
                );
                results.push(PromoteResult {
                    id: instance_id,
                    status: "failed",
                    error: Some(message),
                });
            }
        }
    }
    let restarted = results
        .iter()
        .filter(|result| result.status == "restarted")
        .count();
    record(
        &state,
        &context,
        "skill.promote",
        &name,
        Some(json!({
            "version": latest,
            "instances": results.iter().map(|result| result.id.clone()).collect::<Vec<_>>(),
            "restarted": restarted,
            "failed": results.len() - restarted,
        })),
    );
    Ok(Json(PromoteResponse { latest, results }))
}

/// `DELETE /api/skills/{name}` — refused while an instance mounts it.
pub async fn delete(
    Path(name): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&format!("skill:{name}"));
    let _guard = lock.lock().await;

    let author = history::author_for(&context);
    skills::delete(&state.cfg, &state.db, &name, &author).map_err(ApiError::from)?;
    record(&state, &context, "skill.delete", &name, None);
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/skills/{name}/git/log?limit=50` — newest first.
pub async fn log(
    Path(name): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<LogQuery>,
) -> Result<Json<LogResponse>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LOG_LIMIT);
    if limit == 0 || limit > MAX_LOG_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_LOG_LIMIT}"),
        ));
    }
    let commits = skills::log(&state.cfg, &name, limit).map_err(ApiError::from)?;
    Ok(Json(LogResponse { commits }))
}

/// `GET /api/skills/{name}/git/diff?from=&to=` — unified patch text.
pub async fn diff(
    Path(name): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<DiffResponse>, ApiError> {
    let response = skills::diff(
        &state.cfg,
        &name,
        query.from.as_deref(),
        query.to.as_deref(),
    )
    .map_err(ApiError::from)?;
    Ok(Json(response))
}

/// Rewrite one instance with the new mount and restart it.
async fn promote_one(state: &AppState, instance_id: &str) -> Result<(), String> {
    let mut spec = instances::load(state, instance_id).map_err(|err| err.message.clone())?;
    crate::api::hydrate_previous_image(state, &mut spec);
    crate::api::write_compose_file(state, &spec).map_err(|err| err.message.clone())?;
    crate::api::up_and_record(state, &mut spec)
        .await
        .map_err(|err| err.message.clone())?;
    crate::api::upgrade::wait_until_healthy(state, &spec, PROMOTE_HEALTH_TIMEOUT)
        .await
        .map(|_| ())
}
