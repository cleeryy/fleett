//! DAG pipelines (issue #31).
//!
//! Mutations require the admin role; every authenticated role may read the
//! pipeline list, a definition and the run history. Starting a run answers
//! `202` immediately: the engine runs in a background task and persists each
//! state transition, so `GET …/runs/{run}` is the report.

use crate::api::audit::{record, require_admin};
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::pipelines::{
    self, PipelineDefinition, PipelineError, PipelineRunDetail, PipelineStep, PipelineView,
    Trigger, DEFAULT_RUN_LIMIT, MAX_RUN_LIMIT,
};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

impl From<PipelineError> for ApiError {
    fn from(err: PipelineError) -> Self {
        match err {
            PipelineError::NotFound(id) => {
                ApiError::not_found(format!("pipeline \"{id}\" not found"))
            }
            PipelineError::RunNotFound(id) => {
                ApiError::not_found(format!("pipeline run \"{id}\" not found"))
            }
            PipelineError::RunFinished(id) => ApiError::conflict(
                "run_finished",
                format!("pipeline run \"{id}\" already finished"),
            ),
            PipelineError::NameTaken(name) => ApiError::conflict(
                "pipeline_exists",
                format!("pipeline \"{name}\" already exists"),
            ),
            PipelineError::Corrupt(detail) => {
                ApiError::internal(format!("stored pipeline definition is corrupted: {detail}"))
            }
            PipelineError::Db(err) => ApiError::from(err),
            other => ApiError::bad_request(other.code(), other.to_string()),
        }
    }
}

/// `POST /api/pipelines` body.
#[derive(Debug, Deserialize)]
pub struct CreatePipelineRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub steps: Option<Vec<PipelineStep>>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `PATCH /api/pipelines/{id}` body: merge over the stored definition.
#[derive(Debug, Deserialize)]
pub struct UpdatePipelineRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub steps: Option<Vec<PipelineStep>>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `POST /api/pipelines/{id}/run` body (optional).
#[derive(Debug, Default, Deserialize)]
pub struct RunPipelineRequest {
    /// Free-form run context, available as `{{context}}` in every prompt.
    #[serde(default)]
    pub context: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct RunsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

fn parse_trigger(value: Option<&str>) -> Result<Trigger, ApiError> {
    match value {
        None => Ok(Trigger::Manual),
        Some(value) => Trigger::parse(value).ok_or_else(|| {
            ApiError::bad_request("invalid_trigger", "trigger must be manual, cron or webhook")
        }),
    }
}

fn create_definition(request: CreatePipelineRequest) -> Result<PipelineDefinition, ApiError> {
    let steps = request
        .steps
        .ok_or_else(|| ApiError::bad_request("no_steps", "a pipeline needs at least one step"))?;
    Ok(PipelineDefinition {
        name: request.name.unwrap_or_default().trim().to_string(),
        trigger: parse_trigger(request.trigger.as_deref())?,
        cron: request
            .cron
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        steps,
    })
}

fn patched_definition(
    existing: &PipelineDefinition,
    request: UpdatePipelineRequest,
) -> Result<(PipelineDefinition, Option<bool>), ApiError> {
    let mut definition = existing.clone();
    if let Some(name) = request.name {
        definition.name = name.trim().to_string();
    }
    if let Some(trigger) = request.trigger {
        definition.trigger = parse_trigger(Some(&trigger))?;
    }
    if let Some(cron) = request.cron {
        let trimmed = cron.trim();
        definition.cron = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Some(steps) = request.steps {
        definition.steps = steps;
    }
    Ok((definition, request.enabled))
}

/// `GET /api/pipelines`.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<PipelineView>>, ApiError> {
    let records = pipelines::list(&state.db)?;
    Ok(Json(records.iter().map(|record| record.view()).collect()))
}

/// `POST /api/pipelines`.
pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreatePipelineRequest>,
) -> Result<(StatusCode, Json<PipelineView>), ApiError> {
    require_admin(&context)?;
    let enabled = request.enabled.unwrap_or(true);
    let definition = create_definition(request)?;
    let record = pipelines::create(&state.db, definition, enabled)?;
    record_pipeline_audit(&state, &context, "pipeline.create", &record);
    Ok((StatusCode::CREATED, Json(record.view())))
}

/// `GET /api/pipelines/{id}`.
pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<PipelineView>, ApiError> {
    let record = pipelines::get(&state.db, &id)?
        .ok_or_else(|| ApiError::not_found(format!("pipeline \"{id}\" not found")))?;
    Ok(Json(record.view()))
}

/// `PATCH /api/pipelines/{id}`.
pub async fn patch(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<UpdatePipelineRequest>,
) -> Result<Json<PipelineView>, ApiError> {
    require_admin(&context)?;
    let existing = pipelines::get(&state.db, &id)?
        .ok_or_else(|| ApiError::not_found(format!("pipeline \"{id}\" not found")))?;
    let (definition, enabled) = patched_definition(&existing.definition, request)?;
    let record = pipelines::update(&state.db, &id, definition, enabled)?;
    record_pipeline_audit(&state, &context, "pipeline.update", &record);
    Ok(Json(record.view()))
}

/// `DELETE /api/pipelines/{id}`.
pub async fn delete(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;
    if !pipelines::delete(&state.db, &id)? {
        return Err(ApiError::not_found(format!("pipeline \"{id}\" not found")));
    }
    record(&state, &context, "pipeline.delete", &id, None);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/pipelines/{id}/run` — record a run and start the engine.
pub async fn run(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    body: Option<Json<RunPipelineRequest>>,
) -> Result<(StatusCode, Json<PipelineRunDetail>), ApiError> {
    require_admin(&context)?;
    let pipeline = pipelines::get(&state.db, &id)?
        .ok_or_else(|| ApiError::not_found(format!("pipeline \"{id}\" not found")))?;
    if !pipeline.enabled {
        return Err(ApiError::conflict(
            "pipeline_disabled",
            format!("pipeline \"{id}\" is disabled"),
        ));
    }
    pipelines::validate_instances(&state.db, &pipeline.definition)?;
    let run_context = body
        .as_ref()
        .and_then(|Json(request)| request.context.as_ref())
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        });
    let detail = pipelines::start_run(
        &state.db,
        &pipeline,
        Trigger::Manual,
        run_context.as_deref(),
    )?;
    let task_state = state.clone();
    let task_record = pipeline.clone();
    let run_id = detail.id.clone();
    tokio::spawn(async move {
        pipelines::execute(
            task_state.db.clone(),
            task_state.driver.clone(),
            task_record,
            run_id,
        )
        .await;
    });
    record(
        &state,
        &context,
        "pipeline.run",
        &detail.id,
        Some(json!({ "pipeline": pipeline.id, "name": pipeline.definition.name })),
    );
    Ok((StatusCode::ACCEPTED, Json(detail)))
}

/// `GET /api/pipelines/{id}/runs`.
pub async fn runs(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<Vec<crate::pipelines::PipelineRun>>, ApiError> {
    if pipelines::get(&state.db, &id)?.is_none() {
        return Err(ApiError::not_found(format!("pipeline \"{id}\" not found")));
    }
    let limit = query.limit.unwrap_or(DEFAULT_RUN_LIMIT);
    if limit == 0 || limit > MAX_RUN_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_RUN_LIMIT}"),
        ));
    }
    Ok(Json(pipelines::list_runs(&state.db, &id, limit)?))
}

/// `GET /api/pipelines/{id}/runs/{run}`.
pub async fn run_detail(
    Path((id, run_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<PipelineRunDetail>, ApiError> {
    if pipelines::get(&state.db, &id)?.is_none() {
        return Err(ApiError::not_found(format!("pipeline \"{id}\" not found")));
    }
    Ok(Json(pipelines::get_run(&state.db, &id, &run_id)?))
}

/// `POST /api/pipelines/{id}/runs/{run}/cancel`.
pub async fn cancel(
    Path((id, run_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<PipelineRunDetail>, ApiError> {
    require_admin(&context)?;
    let detail = pipelines::cancel_run(&state.db, &id, &run_id)?;
    record(
        &state,
        &context,
        "pipeline.cancel",
        &run_id,
        Some(json!({ "pipeline": id })),
    );
    Ok(Json(detail))
}

fn record_pipeline_audit(
    state: &AppState,
    context: &AuthContext,
    action: &str,
    pipeline: &crate::pipelines::PipelineRecord,
) {
    record(
        state,
        context,
        action,
        &pipeline.id,
        Some(json!({
            "name": pipeline.definition.name,
            "trigger": pipeline.definition.trigger.as_str(),
            "cron": pipeline.definition.cron.clone(),
            "steps": pipeline.definition.steps.len(),
            "enabled": pipeline.enabled,
        })),
    );
}
