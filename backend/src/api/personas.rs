//! Reusable personas (issue #55).
//!
//! The library itself — files, rendering, git — lives in [`crate::personas`].
//! This module owns authentication, the locks, the audit trail and the
//! propagation to instances. Reads need any authenticated role; every mutation
//! needs `admin`, exactly like the templates.

use crate::api::audit::{record, record_result, require_admin};
use crate::api::error::ApiError;
use crate::api::instances;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::history::{self, CommitInfo, DiffResponse};
use crate::models::{validate_persona_variable_key, Status};
use crate::personas::{
    self, ApplyRequest, ApplyResponse, ApplyResult, CreatePersonaRequest, PersonaDetail,
    PersonaError, PersonaSummary, PreviewRequest, PreviewResponse, UpdatePersonaRequest,
};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

/// Commits returned when `?limit=` is omitted.
pub const DEFAULT_LOG_LIMIT: usize = 50;
/// Upper bound of `?limit=`.
pub const MAX_LOG_LIMIT: usize = 500;
/// Instances one `apply` call may target.
pub const MAX_APPLY_INSTANCES: usize = 200;

impl From<PersonaError> for ApiError {
    fn from(err: PersonaError) -> Self {
        match err {
            PersonaError::NotFound(id) => {
                ApiError::not_found(format!("persona \"{id}\" not found"))
            }
            PersonaError::UnknownCommit(revision) => ApiError::not_found(format!(
                "commit \"{revision}\" was not found in the persona history"
            )),
            PersonaError::Exists(id) => {
                ApiError::conflict("persona_exists", format!("persona \"{id}\" already exists"))
            }
            PersonaError::Invalid(message) => ApiError::bad_request("invalid_persona", message),
            PersonaError::InUse { id, instances } => ApiError::conflict(
                "persona_in_use",
                format!("persona \"{id}\" is still assigned to an instance"),
            )
            .with_details(json!({ "instances": instances })),
            PersonaError::Io(message) | PersonaError::Git(message) | PersonaError::Db(message) => {
                ApiError::internal(message)
            }
        }
    }
}

/// Query of `GET /api/personas/{id}/git/log`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Query of `GET /api/personas/{id}/git/diff`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DiffQuery {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

/// Response of `GET /api/personas/{id}/git/log`.
#[derive(Debug, Clone, Serialize)]
pub struct LogResponse {
    pub commits: Vec<CommitInfo>,
}

/// `GET /api/personas`: the gallery.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<PersonaSummary>>, ApiError> {
    let summaries = personas::list(&state.cfg, &state.db).map_err(ApiError::from)?;
    Ok(Json(summaries))
}

/// `GET /api/personas/{id}`: template, variables and the attached instances.
pub async fn detail(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<PersonaDetail>, ApiError> {
    let detail = personas::detail(&state.cfg, &state.db, &id).map_err(ApiError::from)?;
    Ok(Json(detail))
}

/// `POST /api/personas`.
pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreatePersonaRequest>,
) -> Result<(StatusCode, Json<PersonaDetail>), ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id("persona:create");
    let _guard = lock.lock().await;

    let author = history::author_for(&context);
    let detail =
        personas::create(&state.cfg, &state.db, &request, &author).map_err(ApiError::from)?;
    record(
        &state,
        &context,
        "persona.create",
        &detail.id,
        Some(json!({
            "name": detail.name,
            "variables": detail.variables.iter().map(|entry| entry.key.clone()).collect::<Vec<_>>(),
            "tags": detail.tags,
        })),
    );
    Ok((StatusCode::CREATED, Json(detail)))
}

/// `PATCH /api/personas/{id}`.
pub async fn update(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<UpdatePersonaRequest>,
) -> Result<Json<PersonaDetail>, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&format!("persona:{id}"));
    let _guard = lock.lock().await;

    let mut fields = Vec::new();
    if request.name.is_some() {
        fields.push("name");
    }
    if request.description.is_some() {
        fields.push("description");
    }
    if request.tags.is_some() {
        fields.push("tags");
    }
    if request.variables.is_some() {
        fields.push("variables");
    }
    if request.soul_md.is_some() {
        fields.push("soul_md");
    }
    let author = history::author_for(&context);
    let detail =
        personas::update(&state.cfg, &state.db, &id, &request, &author).map_err(ApiError::from)?;
    record(
        &state,
        &context,
        "persona.update",
        &id,
        Some(json!({ "fields": fields, "instances_count": detail.instances_count })),
    );
    Ok(Json(detail))
}

/// `DELETE /api/personas/{id}` — refused while an instance uses it.
pub async fn delete(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&format!("persona:{id}"));
    let _guard = lock.lock().await;

    let author = history::author_for(&context);
    personas::delete(&state.cfg, &state.db, &id, &author).map_err(ApiError::from)?;
    record(&state, &context, "persona.delete", &id, None);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/personas/{id}/preview`.
///
/// The body may name an instance: its name fills `agent_name` and its stored
/// values seed the overrides, then the request `variables` win. Missing keys
/// are reported instead of failing.
pub async fn preview(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PreviewRequest>,
) -> Result<Json<PreviewResponse>, ApiError> {
    let mut overrides = BTreeMap::new();
    let mut agent_name = None;
    if let Some(instance_id) = request
        .instance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let spec = instances::load(&state, instance_id)?;
        let binding = state
            .db
            .instance_persona(&spec.id)
            .map_err(ApiError::from)?
            .unwrap_or_default();
        overrides.extend(binding.variables);
        agent_name = Some(spec.name.clone());
    }
    if let Some(values) = request.variables {
        overrides.extend(values);
    }
    let rendered = personas::preview(&state.cfg, &id, &overrides, agent_name.as_deref())
        .map_err(ApiError::from)?;
    Ok(Json(rendered))
}

/// `POST /api/personas/{id}/apply` — propagate the persona, instance by
/// instance.
///
/// Each instance is rendered, snapshotted, written (`data/SOUL.md`, backup and
/// git commit), recorded and restarted in that order. A failure is reported in
/// the array and never aborts the remaining instances; every attempt (success
/// or failure) gets its own `instance.persona.apply` audit row.
pub async fn apply(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<ApplyRequest>,
) -> Result<Json<ApplyResponse>, ApiError> {
    require_admin(&context)?;
    // Resolve the persona first: an unknown id is a 404, not N failed rows.
    personas::load(&state.cfg, &id).map_err(ApiError::from)?;

    let targets = unique_targets(&request.instances)?;
    let overrides = request.variables.unwrap_or_default();
    let mut results = Vec::with_capacity(targets.len());
    for instance_id in targets {
        let lock = state.locks.for_id(&instance_id);
        let _guard = lock.lock().await;
        match apply_one(&state, &context, &id, &instance_id, &overrides).await {
            Ok(()) => results.push(ApplyResult {
                id: instance_id,
                status: "applied",
                error: None,
            }),
            Err(err) => {
                record_result(
                    &state,
                    &context,
                    "instance.persona.apply",
                    &instance_id,
                    Some(json!({ "persona": id, "code": err.code, "message": err.message })),
                    "failed",
                );
                results.push(ApplyResult {
                    id: instance_id,
                    status: "failed",
                    error: Some(err.message),
                });
            }
        }
    }
    let applied = results
        .iter()
        .filter(|result| result.status == "applied")
        .count();
    record(
        &state,
        &context,
        "persona.apply",
        &id,
        Some(json!({
            "instances": results.iter().map(|result| result.id.clone()).collect::<Vec<_>>(),
            "applied": applied,
            "failed": results.len() - applied,
        })),
    );
    Ok(Json(ApplyResponse { results }))
}

/// `GET /api/personas/{id}/git/log?limit=50` — newest first.
pub async fn log(
    Path(id): Path<String>,
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
    let commits = personas::log(&state.cfg, &id, limit).map_err(ApiError::from)?;
    Ok(Json(LogResponse { commits }))
}

/// `GET /api/personas/{id}/git/diff?from=&to=` — unified patch text.
pub async fn diff(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<DiffResponse>, ApiError> {
    let response = personas::diff(&state.cfg, &id, query.from.as_deref(), query.to.as_deref())
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

fn unique_targets(requested: &[String]) -> Result<Vec<String>, ApiError> {
    if requested.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_instances",
            "instances must name at least one instance",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for id in requested {
        let id = id.trim();
        if id.is_empty() {
            return Err(ApiError::bad_request(
                "invalid_instances",
                "an instance id must not be empty",
            ));
        }
        if seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    }
    if out.len() > MAX_APPLY_INSTANCES {
        return Err(ApiError::bad_request(
            "invalid_instances",
            format!("at most {MAX_APPLY_INSTANCES} instances may be named"),
        ));
    }
    Ok(out)
}

/// Render, snapshot, write, record and restart one instance.
async fn apply_one(
    state: &AppState,
    context: &AuthContext,
    persona_id: &str,
    instance_id: &str,
    overrides: &BTreeMap<String, String>,
) -> Result<(), ApiError> {
    let spec = instances::load(state, instance_id)?;
    let mut binding = state
        .db
        .instance_persona(&spec.id)
        .map_err(ApiError::from)?
        .unwrap_or_default();
    for key in overrides.keys() {
        validate_persona_variable_key(key)
            .map_err(|message| ApiError::bad_request("invalid_persona_vars", message))?;
    }
    binding.variables.extend(overrides.clone());
    let (rendered, hash) =
        personas::render_for(&state.cfg, persona_id, &binding.variables, &spec.name)
            .map_err(ApiError::from)?;
    if !rendered.missing.is_empty() {
        return Err(missing_variables(&rendered.missing));
    }

    crate::snapshots::auto_before(state, &spec.id, "persona").await;
    personas::write_soul(&state.cfg, &spec.id, &rendered.rendered).map_err(ApiError::from)?;
    // The instance keeps its own history: the write is recorded as `ui:` and
    // attributed to the caller, like an edit through the file browser.
    history::record_ui_write(&state.cfg, &spec.id, context, "/SOUL.md");

    binding.persona = Some(persona_id.to_string());
    binding.hash = hash;
    state
        .db
        .set_instance_persona(&spec.id, &binding)
        .map_err(ApiError::from)?;

    if let Err(err) = state.driver.restart(&spec).await {
        state
            .metrics
            .record_compose_action("restart", crate::metrics::RESULT_ERROR);
        return Err(ApiError::driver(err));
    }
    state
        .metrics
        .record_compose_action("restart", crate::metrics::RESULT_OK);
    let observed = state
        .driver
        .ps(&spec)
        .await
        .map(|result| result.status)
        .unwrap_or(Status::Running);
    let _ = state.db.set_status(&spec.id, observed, None);

    record(
        state,
        context,
        "instance.persona.apply",
        &spec.id,
        Some(json!({ "persona": persona_id, "variables": binding.variables })),
    );
    Ok(())
}

/// `400 missing_persona_variables` naming every unresolved key.
pub(crate) fn missing_variables(missing: &[String]) -> ApiError {
    ApiError::bad_request(
        "missing_persona_variables",
        format!(
            "the persona template needs value(s) for: {}",
            missing.join(", ")
        ),
    )
    .with_details(json!({ "missing": missing }))
}
