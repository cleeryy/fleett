//! Rolling image upgrades with automatic rollback (issue #11).
//!
//! One job moves every instance to a new image, sequentially by default
//! (`parallel` upgrades at once otherwise). An instance whose container never
//! reports `healthy` is put back on its previous tag: the compose file is
//! rewritten, `up -d` runs again and `last_error` records what happened.
//!
//! The compose file of an instance remains the source of truth: the job only
//! writes `services.agent.image` (and the `fleett.previous_image` label of a
//! successful upgrade), and the database follows it.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::{write_compose_file, AppState};
use crate::auth::AuthContext;
use crate::models::{
    now_iso, validate_image, Health, ImageInfo, InstanceSpec, Status, UpgradeJob, UpgradeRequest,
    UpgradeResult, UpgradeStatus, UPGRADE_PARALLEL_MAX, UPGRADE_TIMEOUT_DEFAULT_S,
    UPGRADE_TIMEOUT_MAX_S,
};
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::json;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Longest interval between two health probes of an upgrading instance.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Shortest interval, so a short `timeout_s` still polls a few times.
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Last (or running) upgrade job.
///
/// In-memory on purpose: a job drives Docker calls that cannot be replayed
/// after a restart, so keeping a stale "running" job in SQLite would be a lie.
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

/// `POST /api/upgrade` — start a rolling image upgrade.
pub async fn start(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<UpgradeRequest>,
) -> Result<(StatusCode, Json<UpgradeJob>), ApiError> {
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
    let dry_run = request.dry_run.unwrap_or(false);

    // A running job is answered before anything else, so a refused request
    // never pulls an image.
    if state.upgrade.is_running() {
        return Err(upgrade_in_progress());
    }

    let specs = state.db.list().map_err(ApiError::from)?;
    let results: Vec<UpgradeResult> = specs
        .iter()
        .map(|spec| UpgradeResult {
            id: spec.id.clone(),
            from: (!spec.image.is_empty()).then(|| spec.image.clone()),
            to: image.clone(),
            status: UpgradeStatus::Pending,
            error: None,
        })
        .collect();

    let mut job = UpgradeJob {
        id: uuid::Uuid::new_v4().to_string(),
        image: image.clone(),
        started_at: now_iso(),
        finished_at: None,
        dry_run,
        parallel,
        timeout_s,
        results,
    };

    // A dry run writes nothing, and an empty fleet has nothing to do: both are
    // finished before the response is sent, so the caller never polls.
    if dry_run || job.results.is_empty() {
        job.finished_at = Some(job.started_at.clone());
        let job = state.upgrade.begin(job)?;
        return Ok((StatusCode::ACCEPTED, Json(job)));
    }

    // The image is pulled before the job starts: a bad tag is a `502` with the
    // Docker detail instead of a job in which every instance fails.
    state.driver.ensure_image(&image).await.map_err(|err| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "image_unavailable",
            format!("cannot pull image {image}: {}", err.detail.trim()),
        )
    })?;

    let job = state.upgrade.begin(job)?;
    let job_id = job.id.clone();
    let task_state = state.clone();
    let task_context = context.clone();
    tokio::spawn(async move {
        run_job(task_state, task_context, job_id).await;
    });
    Ok((StatusCode::ACCEPTED, Json(job)))
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

/// Run every instance of one job, honouring the parallel limit, then finish it.
async fn run_job(state: AppState, context: AuthContext, job_id: String) {
    let Some(job) = state.upgrade.current().filter(|job| job.id == job_id) else {
        return;
    };
    let image = job.image.clone();
    let timeout = Duration::from_secs(job.timeout_s);

    // Restore point before any image moves. Taken as one phase so the rolling
    // operation itself is not perturbed by disk I/O: every snapshot is written
    // before the first instance changes image.
    let mut snapshots = JoinSet::new();
    for result in &job.results {
        let state = state.clone();
        let id = result.id.clone();
        snapshots.spawn(async move {
            crate::snapshots::auto_before(&state, &id, "upgrade").await;
        });
    }
    while snapshots.join_next().await.is_some() {}

    let permits = std::sync::Arc::new(Semaphore::new(job.parallel.max(1)));

    let mut tasks = JoinSet::new();
    for result in job.results {
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

    state.upgrade.finish(&job_id);
    if let Some(job) = state.upgrade.current().filter(|job| job.id == job_id) {
        state.metrics.record_upgrade_job(job_result(&job));
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
            let _ = state.db.set_status(&id, Status::Error, Some(detail));
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

    #[test]
    fn a_running_job_blocks_a_second_one() {
        let manager = UpgradeManager::default();
        assert!(manager.current().is_none());

        let job = UpgradeJob {
            id: "job-1".to_string(),
            image: "agent:2".to_string(),
            started_at: now_iso(),
            finished_at: None,
            dry_run: false,
            parallel: 1,
            timeout_s: 180,
            results: Vec::new(),
        };
        manager.begin(job).expect("first job");
        let err = manager
            .begin(UpgradeJob {
                id: "job-2".to_string(),
                image: "agent:3".to_string(),
                started_at: now_iso(),
                finished_at: None,
                dry_run: false,
                parallel: 1,
                timeout_s: 180,
                results: Vec::new(),
            })
            .expect_err("a running job blocks the next one");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.code, "upgrade_in_progress");

        manager.finish("job-1");
        let finished = manager.current().expect("job");
        assert!(finished.finished_at.is_some(), "{finished:?}");
        assert!(manager
            .begin(UpgradeJob {
                id: "job-2".to_string(),
                image: "agent:3".to_string(),
                started_at: now_iso(),
                finished_at: None,
                dry_run: false,
                parallel: 1,
                timeout_s: 180,
                results: Vec::new(),
            })
            .is_ok());
    }

    #[test]
    fn results_are_updated_by_instance() {
        let manager = UpgradeManager::default();
        let job = UpgradeJob {
            id: "job-1".to_string(),
            image: "agent:2".to_string(),
            started_at: now_iso(),
            finished_at: None,
            dry_run: false,
            parallel: 1,
            timeout_s: 180,
            results: vec![
                UpgradeResult {
                    id: "alice".to_string(),
                    from: Some("agent:1".to_string()),
                    to: "agent:2".to_string(),
                    status: UpgradeStatus::Pending,
                    error: None,
                },
                UpgradeResult {
                    id: "bob".to_string(),
                    from: Some("agent:1".to_string()),
                    to: "agent:2".to_string(),
                    status: UpgradeStatus::Pending,
                    error: None,
                },
            ],
        };
        manager.begin(job).expect("job");
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
        UpgradeJob {
            id: "job-1".to_string(),
            image: "agent:2".to_string(),
            started_at: now_iso(),
            finished_at: None,
            dry_run: false,
            parallel: 1,
            timeout_s: 180,
            results: statuses
                .iter()
                .enumerate()
                .map(|(index, status)| UpgradeResult {
                    id: format!("instance-{index}"),
                    from: Some("agent:1".to_string()),
                    to: "agent:2".to_string(),
                    status: *status,
                    error: None,
                })
                .collect(),
        }
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
}
