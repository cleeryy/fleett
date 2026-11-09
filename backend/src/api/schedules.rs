//! Cron schedule endpoints (issue #30).

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::scheduler::{
    self, NewSchedule, Overlap, Schedule, SchedulePatch, ScheduleRun, ScheduleView, SchedulerError,
    DEFAULT_RUN_LIMIT, DEFAULT_TIMEOUT_S, MAX_RUN_LIMIT, MAX_TIMEOUT_S, MIN_TIMEOUT_S,
};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
pub struct CreateScheduleRequest {
    pub name: String,
    /// Five-field cron expression, evaluated in UTC.
    pub cron: String,
    pub prompt: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub timezone: Option<String>,
    /// `skip` (default) or `queue`.
    #[serde(default)]
    pub overlap: Option<String>,
    /// Execution budget in seconds (`timeout_s` is accepted too).
    #[serde(default, alias = "timeout_s")]
    pub timeout: Option<u64>,
    /// Replay one missed occurrence at startup (default `false`).
    #[serde(default)]
    pub catchup: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PatchScheduleRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub overlap: Option<String>,
    #[serde(default, alias = "timeout_s")]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub catchup: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RunsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

impl From<SchedulerError> for ApiError {
    fn from(err: SchedulerError) -> Self {
        match err {
            SchedulerError::UnknownInstance(id) => ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_instance",
                format!("unknown instance \"{id}\""),
            ),
            SchedulerError::NotFound(id) => {
                ApiError::not_found(format!("unknown schedule \"{id}\""))
            }
            SchedulerError::NameExists(name) => ApiError::conflict(
                "schedule_exists",
                format!("a schedule named \"{name}\" already exists for this instance"),
            ),
            SchedulerError::InvalidCron(detail) => {
                ApiError::bad_request("invalid_cron", format!("invalid cron expression: {detail}"))
            }
            SchedulerError::InvalidName(detail) => {
                ApiError::bad_request("invalid_name", format!("invalid schedule name: {detail}"))
            }
            SchedulerError::InvalidPrompt(detail) => {
                ApiError::bad_request("invalid_prompt", format!("invalid prompt: {detail}"))
            }
            SchedulerError::InvalidTimezone(detail) => {
                ApiError::bad_request("invalid_timezone", format!("invalid timezone: {detail}"))
            }
            SchedulerError::InvalidOverlap(value) => ApiError::bad_request(
                "invalid_overlap",
                format!("invalid overlap policy \"{value}\": expected skip or queue"),
            ),
            SchedulerError::InvalidTimeout => ApiError::bad_request(
                "invalid_timeout",
                format!("timeout must be between {MIN_TIMEOUT_S} and {MAX_TIMEOUT_S} seconds"),
            ),
            SchedulerError::RunInProgress => ApiError::conflict(
                "run_in_progress",
                "a run is already in progress for this schedule",
            ),
            SchedulerError::Db(err) => ApiError::from(err),
        }
    }
}

fn overlap_from(value: Option<String>) -> Result<Option<Overlap>, ApiError> {
    value
        .map(|raw| {
            Overlap::parse(&raw).ok_or_else(|| {
                ApiError::bad_request("invalid_overlap", "overlap must be skip or queue")
            })
        })
        .transpose()
}

fn load_instance(state: &AppState, id: &str) -> Result<(), ApiError> {
    if state.db.exists(id).map_err(ApiError::from)? {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_instance",
            format!("unknown instance \"{id}\""),
        ))
    }
}

/// `POST /api/instances/{id}/schedules` — create a schedule.
pub async fn create(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateScheduleRequest>,
) -> Result<(StatusCode, Json<Schedule>), ApiError> {
    load_instance(&state, &id)?;
    let overlap = overlap_from(request.overlap)?.unwrap_or(Overlap::Skip);
    let schedule = scheduler::create(
        &state.db,
        NewSchedule {
            instance_id: id.clone(),
            name: request.name,
            cron: request.cron,
            prompt: request.prompt,
            enabled: request.enabled.unwrap_or(true),
            timezone: request.timezone.unwrap_or_else(|| "UTC".to_string()),
            overlap,
            timeout_s: request.timeout.unwrap_or(DEFAULT_TIMEOUT_S),
            catchup: request.catchup.unwrap_or(false),
        },
    )?;
    audit::record(
        &state,
        &context,
        "schedule.create",
        &schedule.id,
        Some(json!({
            "instance": schedule.instance_id,
            "name": schedule.name,
            "cron": schedule.cron,
            "enabled": schedule.enabled,
        })),
    );
    Ok((StatusCode::CREATED, Json(schedule)))
}

/// `GET /api/instances/{id}/schedules` — schedules of one instance.
pub async fn list_for_instance(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<Schedule>>, ApiError> {
    load_instance(&state, &id)?;
    let schedules = scheduler::list_for_instance(&state.db, &id)?;
    Ok(Json(schedules))
}

/// `PATCH /api/schedules/{sid}` — partial update.
pub async fn patch(
    Path(sid): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PatchScheduleRequest>,
) -> Result<Json<Schedule>, ApiError> {
    let overlap = overlap_from(request.overlap)?;
    let schedule = scheduler::update(
        &state.db,
        &sid,
        SchedulePatch {
            name: request.name,
            cron: request.cron,
            prompt: request.prompt,
            enabled: request.enabled,
            timezone: request.timezone,
            overlap,
            timeout_s: request.timeout,
            catchup: request.catchup,
        },
    )?;
    audit::record(
        &state,
        &context,
        "schedule.update",
        &schedule.id,
        Some(json!({
            "instance": schedule.instance_id,
            "name": schedule.name,
            "cron": schedule.cron,
            "enabled": schedule.enabled,
        })),
    );
    Ok(Json(schedule))
}

/// `DELETE /api/schedules/{sid}` — delete a schedule and its history.
pub async fn delete(
    Path(sid): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    if !scheduler::delete(&state.db, &sid)? {
        return Err(ApiError::not_found(format!("unknown schedule \"{sid}\"")));
    }
    audit::record(&state, &context, "schedule.delete", &sid, None);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/schedules/{sid}/run` — manual trigger, executed inline.
pub async fn run(
    Path(sid): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<(StatusCode, Json<ScheduleRun>), ApiError> {
    let run = scheduler::run_now(&state, &sid).await?;
    audit::record(
        &state,
        &context,
        "schedule.run",
        &sid,
        Some(json!({
            "run": run.id,
            "status": run.status.as_str(),
            "duration_ms": run.duration_ms,
        })),
    );
    Ok((StatusCode::CREATED, Json(run)))
}

/// `GET /api/instances/{id}/runs?limit=50` — run history of one instance.
pub async fn list_runs(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<Vec<ScheduleRun>>, ApiError> {
    load_instance(&state, &id)?;
    let limit = query.limit.unwrap_or(DEFAULT_RUN_LIMIT);
    if limit == 0 || limit > MAX_RUN_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_RUN_LIMIT}"),
        ));
    }
    let runs = scheduler::list_runs(&state.db, &id, limit)?;
    Ok(Json(runs))
}

/// `GET /api/schedules` — every schedule with its next/last run and failure
/// rate (admin only, like the global message view).
pub async fn list_all(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<Vec<ScheduleView>>, ApiError> {
    audit::require_admin(&context)?;
    let schedules = scheduler::list_all(&state.db)?;
    Ok(Json(schedules))
}
