//! Rolling image upgrades with automatic rollback (issue #11), canary phases,
//! maintenance windows and a deployment journal (issue #39).
//!
//! One job moves every instance to a new image, sequentially by default
//! (`parallel` upgrades at once otherwise). An instance whose container never
//! reports `healthy` is put back on its previous tag: the compose file is
//! rewritten, `up -d` runs again and `last_error` records what happened.
//!
//! With `canary: N`, the first N instances are moved first; they must stay
//! healthy for `soak_s` seconds before the rest of the fleet moves. A canary
//! that fails stops the deployment: the canary instances are rolled back (or a
//! failed move is rolled back by the normal path) and the rest is left
//! untouched.
//!
//! The compose file of an instance remains the source of truth: the job only
//! writes `services.agent.image` (and the `fleett.previous_image` label of a
//! successful upgrade), and the database follows it.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::{write_compose_file, AppState};
use crate::auth::{AuthContext, AuthMethod, Role};
use crate::deployments::{self, Deployment, DeploymentStatus, NewDeployment};
use crate::maintenance;
use crate::models::{
    now_iso, validate_image, Health, ImageInfo, InstanceSpec, Status, UpgradeJob, UpgradeRequest,
    UpgradeResult, UpgradeStatus, UPGRADE_PARALLEL_MAX, UPGRADE_SOAK_DEFAULT_S, UPGRADE_SOAK_MAX_S,
    UPGRADE_TIMEOUT_DEFAULT_S, UPGRADE_TIMEOUT_MAX_S,
};
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Longest interval between two health probes of an upgrading instance.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Shortest interval, so a short `timeout_s` still polls a few times.
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Health probe interval of the canary soak (capped by `soak_s`).
const SOAK_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Last (or running) upgrade job.
///
/// In-memory on purpose: a job drives Docker calls that cannot be replayed
/// after a restart, so keeping a stale "running" job in SQLite would be a lie.
/// The durable trace lives in `deployments` (see [`crate::deployments`]).
#[derive(Debug, Default)]
pub struct UpgradeManager {
    job: Mutex<Option<UpgradeJob>>,
}

impl UpgradeManager {
    /// Last or running job, when one exists.
    pub fn current(&self) -> Option<UpgradeJob> {
        self.lock().clone()
    }

    /// `true` while a job has not finished yet.
    pub fn is_running(&self) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|job| job.finished_at.is_none())
    }

    /// Record a new job, refusing to shadow a running one.
    pub fn begin(&self, job: UpgradeJob) -> Result<UpgradeJob, ApiError> {
        let mut current = self.lock();
        if current
            .as_ref()
            .is_some_and(|running| running.finished_at.is_none())
        {
            return Err(upgrade_in_progress());
        }
        *current = Some(job.clone());
        Ok(job)
    }

    /// Apply `update` to the result of one instance.
    fn update_result(&self, job_id: &str, instance: &str, update: impl FnOnce(&mut UpgradeResult)) {
        let mut current = self.lock();
        let Some(job) = current.as_mut().filter(|job| job.id == job_id) else {
            return;
        };
        if let Some(result) = job.results.iter_mut().find(|result| result.id == instance) {
            update(result);
        }
    }

    /// Copy of the result of one instance, when it is still part of the job.
    fn result(&self, job_id: &str, instance: &str) -> Option<UpgradeResult> {
        let current = self.lock();
        let job = current.as_ref().filter(|job| job.id == job_id)?;
        job.results
            .iter()
            .find(|result| result.id == instance)
            .cloned()
    }

    /// Flag every not-started instance as intentionally skipped.
    ///
    /// A failed canary stops the deployment; the untouched instances stay
    /// `pending` with the reason instead of being reported as failures.
    fn mark_skipped(&self, job_id: &str, message: &str) {
        let mut current = self.lock();
        let Some(job) = current.as_mut().filter(|job| job.id == job_id) else {
            return;
        };
        for result in job
            .results
            .iter_mut()
            .filter(|result| result.status == UpgradeStatus::Pending)
        {
            result.error = Some(message.to_string());
        }
    }

    /// Stamp `finished_at`, so `POST /api/upgrade` becomes possible again.
    fn finish(&self, job_id: &str) {
        let mut current = self.lock();
        if let Some(job) = current.as_mut().filter(|job| job.id == job_id) {
            job.finished_at.get_or_insert_with(now_iso);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<UpgradeJob>> {
        self.job
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn upgrade_in_progress() -> ApiError {
    ApiError::conflict(
        "upgrade_in_progress",
        "an upgrade is already running; wait for it to finish or poll GET /api/upgrade",
    )
}

/// Validated body of `POST /api/upgrade`.
#[derive(Debug, Clone)]
struct Prepared {
    image: String,
    parallel: usize,
    timeout_s: u64,
    dry_run: bool,
    canary: usize,
    soak_s: u64,
    /// Id of the window the run must fall in, when one was named.
    window: Option<String>,
    /// Planned start, when the request asked for one.
    at: Option<chrono::DateTime<Utc>>,
}

fn prepare(state: &AppState, request: &UpgradeRequest) -> Result<Prepared, ApiError> {
    let image = request
        .image
        .as_deref()
        .map(str::trim)
        .filter(|image| !image.is_empty())
        .ok_or_else(|| ApiError::bad_request("invalid_image", "image is required"))?
        .to_string();
    validate_image(&image).map_err(|message| ApiError::bad_request("invalid_image", message))?;

    let parallel = request.parallel.unwrap_or(1);
    if !(1..=UPGRADE_PARALLEL_MAX).contains(&parallel) {
        return Err(ApiError::bad_request(
            "invalid_parallel",
            format!("parallel must be between 1 and {UPGRADE_PARALLEL_MAX}"),
        ));
    }
    let timeout_s = request.timeout_s.unwrap_or(UPGRADE_TIMEOUT_DEFAULT_S);
    if !(1..=UPGRADE_TIMEOUT_MAX_S).contains(&timeout_s) {
        return Err(ApiError::bad_request(
            "invalid_timeout",
            format!("timeout_s must be between 1 and {UPGRADE_TIMEOUT_MAX_S}"),
        ));
    }
    let soak_s = request.soak_s.unwrap_or(UPGRADE_SOAK_DEFAULT_S);
    if soak_s > UPGRADE_SOAK_MAX_S {
        return Err(ApiError::bad_request(
            "invalid_soak",
            format!("soak_s must be between 0 and {UPGRADE_SOAK_MAX_S}"),
        ));
    }
    let canary = request.canary.unwrap_or(0);

    let window = match request
        .window
        .as_deref()
        .map(str::trim)
        .filter(|selector| !selector.is_empty())
    {
        None => None,
        Some(selector) => {
            let found = maintenance::find(&state.db, selector).map_err(ApiError::from)?;
            match found {
                Some(window) if window.enabled => Some(window.id),
                Some(window) => {
                    return Err(ApiError::bad_request(
                        "invalid_window",
                        format!("maintenance window \"{}\" is disabled", window.name),
                    ))
                }
                None => {
                    return Err(ApiError::bad_request(
                        "invalid_window",
                        format!("unknown maintenance window \"{selector}\""),
                    ))
                }
            }
        }
    };

    let at = match request
        .at
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(text) => Some(
            maintenance::parse_at(text, Utc::now())
                .map_err(|message| ApiError::bad_request("invalid_at", message))?,
        ),
    };
    if at.is_some() && request.dry_run == Some(true) {
        return Err(ApiError::bad_request(
            "invalid_at",
            "a dry run cannot be scheduled: drop at or drop dry_run",
        ));
    }

    Ok(Prepared {
        image,
        parallel,
        timeout_s,
        dry_run: request.dry_run.unwrap_or(false),
        canary,
        soak_s,
        window,
        at,
    })
}

/// `POST /api/upgrade` — start (or schedule) a rolling image upgrade.
pub async fn start(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<UpgradeRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let prepared = prepare(&state, &request)?;

    let specs = state.db.list().map_err(ApiError::from)?;
    if prepared.canary > specs.len() {
        return Err(ApiError::bad_request(
            "invalid_canary",
            format!(
                "canary must be at most the number of instances ({})",
                specs.len()
            ),
        ));
    }

    // A future `at` never touches Docker now: the row is journaled as
    // `scheduled` and the server fires it when due.
    if let Some(at) = prepared.at {
        let record = schedule(&state, &context, &request, &prepared, at, &specs)?;
        let record = serde_json::to_value(record)
            .map_err(|err| ApiError::internal(format!("cannot serialise the deployment: {err}")))?;
        return Ok((StatusCode::ACCEPTED, Json(record)));
    }

    // A running job is answered before the window check only when the window
    // is open: a refused request must never pull an image.
    maintenance::enforce_window_at(&state, prepared.window.as_deref(), Utc::now())?;
    if state.upgrade.is_running() {
        return Err(upgrade_in_progress());
    }

    let job = launch(state, context, prepared, specs, None).await?;
    let job = serde_json::to_value(job)
        .map_err(|err| ApiError::internal(format!("cannot serialise the job: {err}")))?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

/// Persist a scheduled deployment and return its journal row.
fn schedule(
    state: &AppState,
    context: &AuthContext,
    request: &UpgradeRequest,
    prepared: &Prepared,
    at: chrono::DateTime<Utc>,
    specs: &[InstanceSpec],
) -> Result<Deployment, ApiError> {
    let id = uuid::Uuid::new_v4().to_string();
    let started_at = at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    // The stored request is the *fired* request: `at` is dropped, otherwise a
    // re-validation at firing time would reject the now-past timestamp. The
    // window is stored by id so a rename between scheduling and firing keeps
    // working.
    let mut deferred = request.clone();
    deferred.at = None;
    deferred.window = prepared.window.clone();
    let new = NewDeployment {
        id: id.clone(),
        image: prepared.image.clone(),
        status: DeploymentStatus::Scheduled,
        instances: specs.iter().map(|spec| spec.id.clone()).collect(),
        canary: prepared.canary,
        soak_s: prepared.soak_s,
        started_at: Some(started_at.clone()),
        created_by: context.actor.clone(),
        request: Some(serde_json::to_value(&deferred).map_err(|err| {
            ApiError::internal(format!("cannot serialise the scheduled request: {err}"))
        })?),
    };
    deployments::insert(&state.db, &new).map_err(ApiError::from)?;
    audit::record(
        state,
        context,
        "deployment.schedule",
        &id,
        Some(json!({
            "image": prepared.image,
            "at": started_at,
            "canary": prepared.canary,
            "soak_s": prepared.soak_s,
        })),
    );
    deployments::get(&state.db, &id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::internal("the scheduled deployment disappeared"))
}

/// Start the job of a due scheduled deployment (called by the ticker).
///
/// The maintenance window is evaluated **at firing time**, not when the
/// request was accepted: a window that closed in between cancels the run.
pub(crate) async fn run_scheduled(state: AppState, request: UpgradeRequest, deployment_id: String) {
    if state.upgrade.is_running() {
        return;
    }
    let context = AuthContext {
        actor: "scheduler".to_string(),
        role: Role::Admin,
        method: AuthMethod::Bearer,
        user: None,
        ip: None,
    };
    let prepared = match prepare(&state, &request) {
        Ok(prepared) => prepared,
        Err(err) => {
            let message = format!("{}: {}", err.code, err.message);
            let _ = deployments::fail(&state.db, &deployment_id, &message);
            audit::record(
                &state,
                &context,
                "deployment.fail",
                &deployment_id,
                Some(json!({ "error": message })),
            );
            return;
        }
    };
    if let Err(err) = maintenance::enforce_window_at(&state, prepared.window.as_deref(), Utc::now())
    {
        let message = format!("{}: {}", err.code, err.message);
        let _ = deployments::cancel(&state.db, &deployment_id, &message);
        audit::record(
            &state,
            &context,
            "deployment.cancel",
            &deployment_id,
            Some(json!({ "error": message })),
        );
        return;
    }
    let specs = match state.db.list() {
        Ok(specs) => specs,
        Err(err) => {
            let _ = deployments::fail(&state.db, &deployment_id, &err.to_string());
            return;
        }
    };
    match deployments::mark_started(&state.db, &deployment_id, &now_iso()) {
        Ok(true) => {}
        Ok(false) => return,
        Err(err) => {
            tracing::warn!(deployment = %deployment_id, %err, "cannot claim the scheduled deployment");
            return;
        }
    }
    audit::record(
        &state,
        &context,
        "deployment.start",
        &deployment_id,
        Some(json!({
            "image": prepared.image,
            "canary": prepared.canary.min(specs.len()),
            "soak_s": prepared.soak_s,
        })),
    );
    if let Err(err) = launch(
        state.clone(),
        context,
        prepared,
        specs,
        Some(deployment_id.clone()),
    )
    .await
    {
        let _ = deployments::fail(
            &state.db,
            &deployment_id,
            &format!("{}: {}", err.code, err.message),
        );
    }
}

/// Build the job of a prepared upgrade, journal it, pull the image and spawn it.
async fn launch(
    state: AppState,
    context: AuthContext,
    prepared: Prepared,
    specs: Vec<InstanceSpec>,
    scheduled_row: Option<String>,
) -> Result<UpgradeJob, ApiError> {
    let canary = prepared.canary.min(specs.len());
    let results: Vec<UpgradeResult> = specs
        .iter()
        .map(|spec| UpgradeResult {
            id: spec.id.clone(),
            from: (!spec.image.is_empty()).then(|| spec.image.clone()),
            to: prepared.image.clone(),
            status: UpgradeStatus::Pending,
            error: None,
        })
        .collect();
    let instances: Vec<String> = specs.iter().map(|spec| spec.id.clone()).collect();
    let started_at = now_iso();
    let job_id = scheduled_row
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // The journal row exists before the pull: a registry failure is as much a
    // deployment as a healthy one. A dry run is a plan, not a deployment.
    let deployment_id = if prepared.dry_run {
        None
    } else {
        match &scheduled_row {
            Some(id) => Some(id.clone()),
            None => {
                let id = job_id.clone();
                deployments::insert(
                    &state.db,
                    &NewDeployment {
                        id: id.clone(),
                        image: prepared.image.clone(),
                        status: DeploymentStatus::Pending,
                        instances: instances.clone(),
                        canary,
                        soak_s: prepared.soak_s,
                        started_at: Some(started_at.clone()),
                        created_by: context.actor.clone(),
                        request: None,
                    },
                )
                .map_err(ApiError::from)?;
                Some(id)
            }
        }
    };

    let mut job = UpgradeJob {
        id: job_id.clone(),
        image: prepared.image.clone(),
        started_at,
        finished_at: None,
        dry_run: prepared.dry_run,
        parallel: prepared.parallel,
        timeout_s: prepared.timeout_s,
        canary,
        soak_s: prepared.soak_s,
        deployment_id: deployment_id.clone(),
        results,
    };

    // A dry run writes nothing, and an empty fleet has nothing to do: both are
    // finished before the response is sent, so the caller never polls.
    if prepared.dry_run || job.results.is_empty() {
        job.finished_at = Some(job.started_at.clone());
        let job = state.upgrade.begin(job)?;
        if job.deployment_id.is_some() {
            journal_finish(&state, job.deployment_id.as_deref(), &job);
        }
        return Ok(job);
    }

    // The image is pulled before the job starts: a bad tag is a `502` with the
    // Docker detail instead of a job in which every instance fails.
    if let Err(err) = state.driver.ensure_image(&prepared.image).await {
        let message = format!(
            "cannot pull image {}: {}",
            prepared.image,
            err.detail.trim()
        );
        if let Some(id) = &deployment_id {
            let _ = deployments::fail(&state.db, id, &message);
        }
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "image_unavailable",
            message,
        ));
    }

    let job = state.upgrade.begin(job)?;
    let task_state = state.clone();
    tokio::spawn(async move {
        run_job(task_state, context, job_id).await;
    });
    Ok(job)
}

/// `GET /api/upgrade` — current or last job.
pub async fn status(State(state): State<AppState>) -> Result<Json<UpgradeJob>, ApiError> {
    state
        .upgrade
        .current()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("no upgrade job has run yet"))
}

/// `GET /api/images` — local images and the instances that use each tag.
pub async fn images(State(state): State<AppState>) -> Result<Json<Vec<ImageInfo>>, ApiError> {
    let local = state.driver.list_images().await.map_err(|err| {
        let detail = err.detail.trim();
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "images_unavailable",
            if detail.is_empty() {
                format!("cannot list the local docker images: {}", err.operation)
            } else {
                format!("cannot list the local docker images: {detail}")
            },
        )
    })?;
    let specs = state.db.list().map_err(ApiError::from)?;

    let images = local
        .into_iter()
        .map(|record| {
            let instances = specs
                .iter()
                .filter(|spec| spec.image == record.tag)
                .map(|spec| spec.id.clone())
                .collect();
            ImageInfo {
                tag: record.tag,
                id: record.id,
                size: record.size,
                created: record.created,
                instances,
            }
        })
        .collect();
    Ok(Json(images))
}

/// Run one job: canary phase, soak, then the rest.
async fn run_job(state: AppState, context: AuthContext, job_id: String) {
    let Some(job) = state.upgrade.current().filter(|job| job.id == job_id) else {
        return;
    };
    let image = job.image.clone();
    let timeout = Duration::from_secs(job.timeout_s);
    let canary = job.canary.min(job.results.len());
    let soak = Duration::from_secs(job.soak_s);
    let deployment = job.deployment_id.clone();
    let results = job.results.clone();

    let canary_ids: Vec<String> = results.iter().take(canary).map(|r| r.id.clone()).collect();
    if !canary_ids.is_empty() {
        journal_phase(&state, deployment.as_deref(), DeploymentStatus::Canary);
        let mut previous_labels: HashMap<String, Option<String>> = HashMap::new();
        let mut failure: Option<String> = None;
        for id in &canary_ids {
            previous_labels.insert(id.clone(), crate::store::previous_image(&state.cfg, id));
            upgrade_one(
                state.clone(),
                context.clone(),
                job_id.clone(),
                id.clone(),
                image.clone(),
                timeout,
            )
            .await;
            let result = state.upgrade.result(&job_id, id);
            if result.as_ref().map(|result| result.status) != Some(UpgradeStatus::Healthy) {
                let detail = result
                    .and_then(|result| result.error)
                    .unwrap_or_else(|| "the instance did not become healthy".to_string());
                failure = Some(format!("canary instance \"{id}\" failed: {detail}"));
                break;
            }
        }
        journal_result(&state, deployment.as_deref(), &job_id);

        if failure.is_none() && !soak.is_zero() {
            if let Err(reason) = soak_canaries(&state, &job_id, &canary_ids, soak).await {
                rollback_canaries(
                    &state,
                    &context,
                    &job_id,
                    &canary_ids,
                    &previous_labels,
                    &image,
                    &reason,
                )
                .await;
                failure = Some(reason);
            }
        }

        if let Some(reason) = failure {
            state.upgrade.mark_skipped(
                &job_id,
                &format!("not attempted: the canary phase failed ({reason})"),
            );
            journal_result(&state, deployment.as_deref(), &job_id);
            finish_job(&state, &job_id, deployment.as_deref());
            return;
        }
    }
    journal_phase(&state, deployment.as_deref(), DeploymentStatus::Running);

    let permits = Arc::new(Semaphore::new(job.parallel.max(1)));
    let mut tasks = JoinSet::new();
    let remainder: Vec<UpgradeResult> = results
        .into_iter()
        .filter(|result| !canary_ids.contains(&result.id))
        .collect();
    for result in remainder {
        let state = state.clone();
        let context = context.clone();
        let job_id = job_id.clone();
        let image = image.clone();
        let permits = permits.clone();
        tasks.spawn(async move {
            let Ok(_permit) = permits.acquire_owned().await else {
                return;
            };
            upgrade_one(state, context, job_id, result.id, image, timeout).await;
        });
    }
    while tasks.join_next().await.is_some() {}

    finish_job(&state, &job_id, deployment.as_deref());
}

/// Poll the healthy canaries for `soak`; the first one that stops serving
/// fails the phase.
async fn soak_canaries(
    state: &AppState,
    job_id: &str,
    ids: &[String],
    soak: Duration,
) -> Result<(), String> {
    let interval = SOAK_POLL_INTERVAL.min(soak).max(Duration::from_millis(100));
    let deadline = tokio::time::Instant::now() + soak;
    loop {
        for id in ids {
            // A canary already rolled back by its own upgrade is not probed.
            if state.upgrade.result(job_id, id).map(|result| result.status)
                != Some(UpgradeStatus::Healthy)
            {
                continue;
            }
            let Ok(Some(spec)) = state.db.get(id) else {
                return Err(format!(
                    "canary instance \"{id}\" disappeared during the soak"
                ));
            };
            match state.driver.ps(&spec).await {
                Ok(observed) => match observed.health {
                    Health::Unhealthy => {
                        return Err(format!(
                            "canary instance \"{id}\" became unhealthy during the {}s soak",
                            soak.as_secs()
                        ))
                    }
                    Health::Healthy | Health::Starting | Health::Unknown => {
                        if matches!(observed.status, Status::Stopped | Status::Error) {
                            return Err(format!(
                                "canary instance \"{id}\" stopped during the {}s soak",
                                soak.as_secs()
                            ));
                        }
                    }
                },
                Err(err) => {
                    tracing::debug!(instance = %id, %err, "health probe failed during the soak");
                }
            }
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(());
        }
        tokio::time::sleep(interval.min(deadline - now)).await;
    }
}

/// Restore every still-healthy canary after an unhealthy soak.
#[allow(clippy::too_many_arguments)]
async fn rollback_canaries(
    state: &AppState,
    context: &AuthContext,
    job_id: &str,
    ids: &[String],
    previous_labels: &HashMap<String, Option<String>>,
    attempted: &str,
    reason: &str,
) {
    for id in ids {
        let Some(result) = state.upgrade.result(job_id, id) else {
            continue;
        };
        if result.status != UpgradeStatus::Healthy {
            continue;
        }
        let Ok(Some(mut spec)) = state.db.get(id) else {
            continue;
        };
        let Some(old_image) = result.from.filter(|image| !image.is_empty()) else {
            let message = format!(
                "canary rollback failed: {reason}; the previous image of \"{id}\" is unknown"
            );
            let _ = state.db.set_status(id, Status::Error, Some(&message));
            state.upgrade.update_result(job_id, id, |result| {
                result.status = UpgradeStatus::Failed;
                result.error = Some(message.clone());
            });
            continue;
        };
        rollback(
            state,
            context,
            job_id,
            &mut spec,
            &old_image,
            previous_labels.get(id).cloned().flatten(),
            attempted,
            reason,
        )
        .await;
    }
}

/// Stamp the final state of a job and its journal row.
fn finish_job(state: &AppState, job_id: &str, deployment: Option<&str>) {
    state.upgrade.finish(job_id);
    let Some(job) = state.upgrade.current().filter(|job| job.id == job_id) else {
        return;
    };
    state.metrics.record_upgrade_job(job_result(&job));
    journal_finish(state, deployment, &job);
}

fn journal_phase(state: &AppState, deployment: Option<&str>, phase: DeploymentStatus) {
    let Some(id) = deployment else {
        return;
    };
    if let Err(err) = deployments::set_phase(&state.db, id, phase) {
        tracing::warn!(deployment = %id, %err, "cannot journal the deployment phase");
    }
}

fn journal_result(state: &AppState, deployment: Option<&str>, job_id: &str) {
    let (Some(id), Some(job)) = (
        deployment,
        state.upgrade.current().filter(|job| job.id == job_id),
    ) else {
        return;
    };
    let result = serde_json::to_value(&job.results).unwrap_or(Value::Null);
    if let Err(err) = deployments::set_result(&state.db, id, &result) {
        tracing::warn!(deployment = %id, %err, "cannot journal the deployment results");
    }
}

fn journal_finish(state: &AppState, deployment: Option<&str>, job: &UpgradeJob) {
    let Some(id) = deployment else {
        return;
    };
    let status = deployment_status(job);
    let rollback = job
        .results
        .iter()
        .any(|result| result.status == UpgradeStatus::RolledBack);
    let result = serde_json::to_value(&job.results).unwrap_or(Value::Null);
    if let Err(err) = deployments::finish(&state.db, id, status, &result, rollback) {
        tracing::warn!(deployment = %id, %err, "cannot finish the deployment journal row");
    }
}

/// Journal status of a finished job.
fn deployment_status(job: &UpgradeJob) -> DeploymentStatus {
    if job
        .results
        .iter()
        .any(|result| result.status == UpgradeStatus::Failed)
    {
        DeploymentStatus::Failed
    } else if job
        .results
        .iter()
        .any(|result| result.status == UpgradeStatus::RolledBack)
    {
        DeploymentStatus::RolledBack
    } else {
        DeploymentStatus::Ok
    }
}

/// Final result label of one job (`fleett_upgrade_jobs_total`).
///
/// A job that lost at least one instance is a failure even when the others were
/// moved; a clean rollback is reported apart because it needs operator action
/// but left the fleet running.
fn job_result(job: &UpgradeJob) -> &'static str {
    if job
        .results
        .iter()
        .any(|result| result.status == UpgradeStatus::Failed)
    {
        "failed"
    } else if job
        .results
        .iter()
        .any(|result| result.status == UpgradeStatus::RolledBack)
    {
        "rolled_back"
    } else {
        "success"
    }
}

/// Upgrade one instance, rolling back when the new image never gets healthy.
async fn upgrade_one(
    state: AppState,
    context: AuthContext,
    job_id: String,
    id: String,
    image: String,
    timeout: Duration,
) {
    let Ok(Some(spec)) = state.db.get(&id) else {
        state.upgrade.update_result(&job_id, &id, |result| {
            result.status = UpgradeStatus::Failed;
            result.error = Some(format!("instance \"{id}\" disappeared during the upgrade"));
        });
        return;
    };
    let from = spec.image.clone();

    state.upgrade.update_result(&job_id, &id, |result| {
        result.status = UpgradeStatus::Upgrading;
        result.from = (!from.is_empty()).then(|| from.clone());
    });

    // The image is made available before anything is written: a failed pull
    // leaves the instance untouched and is reported as a failure, not a
    // rollback (there is nothing to roll back).
    if let Err(err) = state.driver.ensure_image(&image).await {
        let message = format!("cannot use image {image}: {}", err.detail.trim());
        let _ = state.db.set_last_error(&id, Some(&message));
        state.upgrade.update_result(&job_id, &id, |result| {
            result.status = UpgradeStatus::Failed;
            result.error = Some(message.clone());
        });
        audit::record(
            &state,
            &context,
            "instance.upgrade",
            &id,
            Some(json!({
                "from": from,
                "to": image,
                "status": UpgradeStatus::Failed.as_str(),
                "error": message,
            })),
        );
        return;
    }

    // Serialise against start/stop/restart/patch of the same instance.
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;
    let Ok(Some(mut spec)) = state.db.get(&id) else {
        state.upgrade.update_result(&job_id, &id, |result| {
            result.status = UpgradeStatus::Failed;
            result.error = Some(format!("instance \"{id}\" disappeared during the upgrade"));
        });
        return;
    };
    // The label lives in the compose file only; read it back so a failed
    // upgrade of an already-upgraded instance keeps its history.
    crate::api::hydrate_previous_image(&state, &mut spec);

    let old_image = spec.image.clone();
    let previous_before = spec.previous_image.clone();
    spec.image = image.clone();
    if !old_image.is_empty() {
        spec.previous_image = Some(old_image.clone());
    }

    let started = if let Err(err) = write_compose_file(&state, &spec) {
        Err(format!("cannot write the compose file: {}", err.message))
    } else {
        match state.driver.up(&spec).await {
            Ok(()) => {
                state
                    .metrics
                    .record_compose_action("upgrade", crate::metrics::RESULT_OK);
                Ok(())
            }
            Err(err) => {
                state
                    .metrics
                    .record_compose_action("upgrade", crate::metrics::RESULT_ERROR);
                Err(format!("docker compose up failed: {}", err.detail.trim()))
            }
        }
    };

    if let Err(reason) = started {
        rollback(
            &state,
            &context,
            &job_id,
            &mut spec,
            &old_image,
            previous_before,
            &image,
            &reason,
        )
        .await;
        return;
    }

    match wait_until_healthy(&state, &spec, timeout).await {
        Ok(observed) => {
            let _ = state.db.set_image(&id, &image);
            let _ = state.db.set_status(&id, observed, None);
            state.upgrade.update_result(&job_id, &id, |result| {
                result.status = UpgradeStatus::Healthy;
                result.error = None;
            });
            audit::record(
                &state,
                &context,
                "instance.upgrade",
                &id,
                Some(json!({
                    "from": old_image,
                    "to": image,
                    "status": UpgradeStatus::Healthy.as_str(),
                })),
            );
        }
        Err(reason) => {
            rollback(
                &state,
                &context,
                &job_id,
                &mut spec,
                &old_image,
                previous_before,
                &image,
                &reason,
            )
            .await;
        }
    }
}

/// Poll `docker compose ps` until the container is healthy, or give up.
///
/// `Ok(status)` carries the observed lifecycle status of the container;
/// `Err(reason)` explains why the image is not usable.
///
/// Shared with the provider rotation (`PATCH /api/providers/{id}`), which
/// recreates attached instances with the rotated credential.
pub(crate) async fn wait_until_healthy(
    state: &AppState,
    spec: &InstanceSpec,
    timeout: Duration,
) -> Result<Status, String> {
    let interval = poll_interval(timeout);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match state.driver.ps(spec).await {
            Ok(observed) => match observed.health {
                Health::Healthy => return Ok(observed.status),
                Health::Unhealthy => {
                    return Err("the container reported unhealthy".to_string());
                }
                Health::Starting | Health::Unknown => {}
            },
            Err(err) => {
                tracing::debug!(instance = %spec.id, %err, "health probe failed during the upgrade");
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out after {}s waiting for a healthy container",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(interval).await;
    }
}

/// Poll often enough that a short timeout still probes a few times.
fn poll_interval(timeout: Duration) -> Duration {
    (timeout / 20)
        .clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)
        .max(Duration::from_millis(1))
}

/// Restore the previous image after a failed upgrade.
///
/// The compose file is rewritten with the old tag and `up -d` runs again; the
/// instance is left `rolled_back` when that works, `failed` when even the
/// rollback does not start.
#[allow(clippy::too_many_arguments)]
async fn rollback(
    state: &AppState,
    context: &AuthContext,
    job_id: &str,
    spec: &mut InstanceSpec,
    old_image: &str,
    previous_before: Option<String>,
    attempted: &str,
    reason: &str,
) {
    let id = spec.id.clone();
    if old_image.is_empty() {
        let message = format!(
            "upgrade failed: {reason}; the previous image is unknown, nothing to roll back to"
        );
        let _ = state.db.set_status(&id, Status::Error, Some(&message));
        state.upgrade.update_result(job_id, &id, |result| {
            result.status = UpgradeStatus::Failed;
            result.error = Some(message.clone());
        });
        audit::record(
            state,
            context,
            "instance.upgrade.rollback",
            &id,
            Some(json!({
                "from": old_image,
                "to": attempted,
                "status": UpgradeStatus::Failed.as_str(),
                "error": reason,
            })),
        );
        return;
    }

    spec.image = old_image.to_string();
    spec.previous_image = previous_before;
    let restored = match write_compose_file(state, spec) {
        Ok(()) => match state.driver.up(spec).await {
            Ok(()) => {
                state
                    .metrics
                    .record_compose_action("rollback", crate::metrics::RESULT_OK);
                Ok(())
            }
            Err(err) => {
                state
                    .metrics
                    .record_compose_action("rollback", crate::metrics::RESULT_ERROR);
                Err(err.detail.trim().to_string())
            }
        },
        Err(err) => Err(err.message),
    };

    let message = format!("upgrade failed: {reason}; rolled back to {old_image}");
    let failure = match restored {
        Ok(()) => None,
        Err(detail) => Some(format!("{message}; the rollback failed: {detail}")),
    };

    match &failure {
        None => {
            let status = state
                .driver
                .ps(spec)
                .await
                .map(|observed| observed.status)
                .unwrap_or(Status::Running);
            let _ = state.db.set_status(&id, status, Some(&message));
            state.upgrade.update_result(job_id, &id, |result| {
                result.status = UpgradeStatus::RolledBack;
                result.error = Some(message.clone());
            });
        }
        Some(detail) => {
            let _ = state
                .db
                .set_status(&id, Status::Error, Some(detail.as_str()));
            state.upgrade.update_result(job_id, &id, |result| {
                result.status = UpgradeStatus::Failed;
                result.error = Some(detail.clone());
            });
        }
    }

    audit::record(
        state,
        context,
        "instance.upgrade.rollback",
        &id,
        Some(json!({
            "from": old_image,
            "to": attempted,
            "status": if failure.is_some() {
                UpgradeStatus::Failed.as_str()
            } else {
                UpgradeStatus::RolledBack.as_str()
            },
            "error": reason,
        })),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str) -> UpgradeJob {
        UpgradeJob {
            id: id.to_string(),
            image: "agent:2".to_string(),
            started_at: now_iso(),
            finished_at: None,
            dry_run: false,
            parallel: 1,
            timeout_s: 180,
            canary: 0,
            soak_s: 0,
            deployment_id: None,
            results: Vec::new(),
        }
    }

    #[test]
    fn a_running_job_blocks_a_second_one() {
        let manager = UpgradeManager::default();
        assert!(manager.current().is_none());

        manager.begin(job("job-1")).expect("first job");
        let err = manager
            .begin(job("job-2"))
            .expect_err("a running job blocks the next one");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.code, "upgrade_in_progress");

        manager.finish("job-1");
        let finished = manager.current().expect("job");
        assert!(finished.finished_at.is_some(), "{finished:?}");
        assert!(manager.begin(job("job-2")).is_ok());
    }

    fn result(id: &str, status: UpgradeStatus) -> UpgradeResult {
        UpgradeResult {
            id: id.to_string(),
            from: Some("agent:1".to_string()),
            to: "agent:2".to_string(),
            status,
            error: None,
        }
    }

    #[test]
    fn results_are_updated_by_instance() {
        let manager = UpgradeManager::default();
        let mut first = job("job-1");
        first.results = vec![
            result("alice", UpgradeStatus::Pending),
            result("bob", UpgradeStatus::Pending),
        ];
        manager.begin(first).expect("job");
        manager.update_result("job-1", "bob", |result| {
            result.status = UpgradeStatus::Healthy;
        });
        let current = manager.current().expect("job");
        assert_eq!(current.results[0].status, UpgradeStatus::Pending);
        assert_eq!(current.results[1].status, UpgradeStatus::Healthy);

        // A stale job id never mutates the current one.
        manager.update_result("job-0", "alice", |result| {
            result.status = UpgradeStatus::Failed;
        });
        assert_eq!(
            manager.current().expect("job").results[0].status,
            UpgradeStatus::Pending
        );

        assert_eq!(
            manager.result("job-1", "bob").map(|result| result.status),
            Some(UpgradeStatus::Healthy)
        );
        assert!(manager.result("job-1", "ghost").is_none());
    }

    #[test]
    fn skipped_results_keep_their_pending_status() {
        let manager = UpgradeManager::default();
        let mut first = job("job-1");
        first.results = vec![
            result("alice", UpgradeStatus::RolledBack),
            result("bob", UpgradeStatus::Pending),
            result("carol", UpgradeStatus::Healthy),
        ];
        manager.begin(first).expect("job");
        manager.mark_skipped("job-1", "not attempted");
        let results = manager.current().expect("job").results;
        assert_eq!(results[1].status, UpgradeStatus::Pending);
        assert_eq!(results[1].error.as_deref(), Some("not attempted"));
        assert!(results[2].error.is_none());
    }

    #[test]
    fn the_poll_interval_is_bounded() {
        assert_eq!(poll_interval(Duration::from_secs(1)), MIN_POLL_INTERVAL);
        assert_eq!(poll_interval(Duration::from_secs(180)), MAX_POLL_INTERVAL);
        assert_eq!(
            poll_interval(Duration::from_secs(10)),
            Duration::from_millis(500)
        );
    }

    fn job_with(statuses: &[UpgradeStatus]) -> UpgradeJob {
        let mut job = job("job-1");
        job.results = statuses
            .iter()
            .enumerate()
            .map(|(index, status)| result(&format!("instance-{index}"), *status))
            .collect();
        job
    }

    #[test]
    fn the_job_result_reports_the_worst_outcome() {
        assert_eq!(job_result(&job_with(&[UpgradeStatus::Healthy])), "success");
        assert_eq!(
            job_result(&job_with(&[
                UpgradeStatus::Healthy,
                UpgradeStatus::RolledBack
            ])),
            "rolled_back",
            "one rollback makes the whole job a rollback"
        );
        assert_eq!(
            job_result(&job_with(&[
                UpgradeStatus::RolledBack,
                UpgradeStatus::Failed
            ])),
            "failed",
            "a failure dominates a rollback"
        );
    }

    #[test]
    fn the_deployment_status_maps_the_job_outcomes() {
        assert_eq!(
            deployment_status(&job_with(&[UpgradeStatus::Healthy])),
            DeploymentStatus::Ok
        );
        assert_eq!(
            deployment_status(&job_with(&[UpgradeStatus::Healthy, UpgradeStatus::Pending])),
            DeploymentStatus::Ok
        );
        assert_eq!(
            deployment_status(&job_with(&[UpgradeStatus::RolledBack])),
            DeploymentStatus::RolledBack
        );
        assert_eq!(
            deployment_status(&job_with(&[UpgradeStatus::Failed])),
            DeploymentStatus::Failed
        );
    }
}
