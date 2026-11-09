//! Watchdog: self-healing policies and the incident registry (issue #32).
//!
//! `restart: unless-stopped` restarts a container that dies, but nothing
//! notices an agent that is *alive and mute*: a gateway that stopped answering
//! its health check, a persistent crash loop, an image that vanished. The
//! watchdog samples every instance, records what it sees in the `incidents`
//! table and, when the instance opted in (`heal: restart|recreate`), applies a
//! bounded remediation:
//!
//! - `off` — observe only, incidents are still recorded;
//! - `restart` — `docker compose restart` after `unhealthy_after` consecutive
//!   unhealthy samples;
//! - `recreate` — `docker compose down` then `up -d`.
//!
//! Healing is bounded twice: `max_restarts` attempts per incident (then the
//! instance is marked `error` and the incident stays open for an operator) and
//! an exponential delay between two attempts (`heal_backoff_s`, doubled per
//! attempt, capped at [`crate::models::HEAL_BACKOFF_CAP_S`]).
//!
//! A cleanly stopped instance is never touched and never opens an incident:
//! `docker inspect` reports a clean exit (`ExitCode` 0, no error, no OOM), and
//! the sampler treats `stopped`/`created` with no failure evidence as a
//! deliberate stop.

use crate::api::AppState;
use crate::db::{Db, DbError};
use crate::models::{HealPolicy, Health, InstanceSpec, Status, HEAL_BACKOFF_CAP_S};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Incidents returned when `?limit=` is omitted.
pub const DEFAULT_LIMIT: usize = 100;
/// Upper bound of one page.
pub const MAX_LIMIT: usize = 500;
/// Upper bound of the watchdog sampling interval (`FLEETT_WATCHDOG_INTERVAL_S`).
pub const MAX_INTERVAL_S: u64 = 86_400;
/// Prefix of the `last_error` written when healing gives up.
pub const EXHAUSTED_ERROR_PREFIX: &str = "watchdog stopped healing";

/// Why an incident was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentCause {
    /// The container was killed by the OOM killer.
    Oom,
    /// The container keeps exiting and restarting.
    Crashloop,
    /// The health check stayed unhealthy for `unhealthy_after` samples.
    Unhealthy,
    /// The image the instance needs is not present on the host.
    ImageMissing,
    /// A heal action (`restart`/`recreate`) failed.
    HealFailed,
}

impl IncidentCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Oom => "oom",
            Self::Crashloop => "crashloop",
            Self::Unhealthy => "unhealthy",
            Self::ImageMissing => "image_missing",
            Self::HealFailed => "heal_failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "oom" => Some(Self::Oom),
            "crashloop" => Some(Self::Crashloop),
            "unhealthy" => Some(Self::Unhealthy),
            "image_missing" => Some(Self::ImageMissing),
            "heal_failed" => Some(Self::HealFailed),
            _ => None,
        }
    }
}

impl std::fmt::Display for IncidentCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lifecycle state of one incident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IncidentStatus {
    Open,
    Closed,
}

impl IncidentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

impl std::fmt::Display for IncidentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One action the watchdog attempted for an incident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentAction {
    /// RFC 3339 timestamp of the attempt.
    pub at: String,
    /// `restart`, `recreate`, `heal_exhausted` or `observe`.
    pub action: String,
    /// `ok`, `error` or `skipped`.
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl IncidentAction {
    fn new(at: impl Into<String>, action: &str, result: &str, detail: Option<String>) -> Self {
        Self {
            at: at.into(),
            action: action.to_string(),
            result: result.to_string(),
            detail,
        }
    }
}

/// One incident, as stored in the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incident {
    pub id: String,
    pub instance_id: String,
    pub cause: IncidentCause,
    pub status: IncidentStatus,
    pub opened_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    pub detail: String,
    pub actions: Vec<IncidentAction>,
    pub restart_count: u32,
    pub last_seen_at: String,
}

const COLUMNS: &str = "id, instance_id, cause, status, opened_at, closed_at, detail, actions, \
                       restart_count, last_seen_at";

fn row_to_incident(row: &rusqlite::Row<'_>) -> rusqlite::Result<Incident> {
    let cause: String = row.get(2)?;
    let status: String = row.get(3)?;
    let actions: String = row.get(7)?;
    Ok(Incident {
        id: row.get(0)?,
        instance_id: row.get(1)?,
        cause: IncidentCause::parse(&cause).unwrap_or(IncidentCause::Unhealthy),
        status: IncidentStatus::parse(&status).unwrap_or(IncidentStatus::Open),
        opened_at: row.get(4)?,
        closed_at: row.get(5)?,
        detail: row.get(6)?,
        actions: serde_json::from_str(&actions).unwrap_or_default(),
        restart_count: row.get::<_, i64>(8)?.clamp(0, u32::MAX as i64) as u32,
        last_seen_at: row.get(9)?,
    })
}

/// One page of incidents, newest first.
pub fn list(
    db: &Db,
    status: Option<IncidentStatus>,
    instance: Option<&str>,
    limit: usize,
) -> Result<Vec<Incident>, DbError> {
    let limit = limit.clamp(1, MAX_LIMIT) as i64;
    db.with_conn(|conn| {
        let where_clause = match (status, instance) {
            (Some(_), Some(_)) => "WHERE status = ?1 AND instance_id = ?2",
            (Some(_), None) => "WHERE status = ?1",
            (None, Some(_)) => "WHERE instance_id = ?1",
            (None, None) => "",
        };
        let sql = format!(
            "SELECT {COLUMNS} FROM incidents {where_clause} \
             ORDER BY opened_at DESC, id DESC LIMIT ?"
        );
        let mut statement = conn.prepare(&sql)?;
        let rows = match (status, instance) {
            (Some(status), Some(instance)) => statement
                .query_map(params![status.as_str(), instance, limit], row_to_incident)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            (Some(status), None) => statement
                .query_map(params![status.as_str(), limit], row_to_incident)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            (None, Some(instance)) => statement
                .query_map(params![instance, limit], row_to_incident)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            (None, None) => statement
                .query_map(params![limit], row_to_incident)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    })
}

/// One incident by id.
pub fn get(db: &Db, id: &str) -> Result<Option<Incident>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM incidents WHERE id = ?1"),
            params![id],
            row_to_incident,
        )
        .optional()
    })
}

/// Open incidents of one instance, newest first.
pub fn open_for_instance(db: &Db, instance_id: &str) -> Result<Vec<Incident>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM incidents WHERE instance_id = ?1 AND status = 'open' \
             ORDER BY opened_at, id"
        ))?;
        let rows = statement
            .query_map(params![instance_id], row_to_incident)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

fn find_open(
    db: &Db,
    instance_id: &str,
    cause: IncidentCause,
) -> Result<Option<Incident>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            &format!(
                "SELECT {COLUMNS} FROM incidents WHERE instance_id = ?1 AND cause = ?2 \
                 AND status = 'open' ORDER BY opened_at DESC LIMIT 1"
            ),
            params![instance_id, cause.as_str()],
            row_to_incident,
        )
        .optional()
    })
}

/// Open one incident, or refresh the one already open for this cause.
///
/// Returns the incident and whether it was created by this call.
pub fn ensure_open(
    db: &Db,
    instance_id: &str,
    cause: IncidentCause,
    detail: &str,
    restart_count: u32,
    at: &str,
) -> Result<(Incident, bool), DbError> {
    if let Some(mut incident) = find_open(db, instance_id, cause)? {
        incident.detail = detail.to_string();
        incident.restart_count = restart_count;
        incident.last_seen_at = at.to_string();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE incidents SET detail = ?2, restart_count = ?3, last_seen_at = ?4 \
                 WHERE id = ?1",
                params![
                    incident.id,
                    incident.detail,
                    restart_count,
                    incident.last_seen_at
                ],
            )?;
            Ok(())
        })?;
        return Ok((incident, false));
    }

    let incident = Incident {
        id: uuid::Uuid::new_v4().to_string(),
        instance_id: instance_id.to_string(),
        cause,
        status: IncidentStatus::Open,
        opened_at: at.to_string(),
        closed_at: None,
        detail: detail.to_string(),
        actions: Vec::new(),
        restart_count,
        last_seen_at: at.to_string(),
    };
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO incidents (id, instance_id, cause, status, opened_at, closed_at, detail, \
             actions, restart_count, last_seen_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9)",
            params![
                incident.id,
                incident.instance_id,
                incident.cause.as_str(),
                incident.status.as_str(),
                incident.opened_at,
                incident.detail,
                "[]",
                incident.restart_count,
                incident.last_seen_at,
            ],
        )?;
        Ok(())
    })?;
    Ok((incident, true))
}

/// Append one attempted action to an incident.
pub fn append_action(
    db: &Db,
    incident: &mut Incident,
    action: IncidentAction,
    at: &str,
) -> Result<(), DbError> {
    incident.actions.push(action);
    incident.last_seen_at = at.to_string();
    let actions = serde_json::to_string(&incident.actions).unwrap_or_else(|_| "[]".to_string());
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE incidents SET actions = ?2, last_seen_at = ?3 WHERE id = ?1",
            params![incident.id, actions, incident.last_seen_at],
        )?;
        Ok(())
    })
}

/// Close one incident. Returns `false` when it was already closed.
pub fn close(db: &Db, incident: &mut Incident, at: &str) -> Result<bool, DbError> {
    if incident.status == IncidentStatus::Closed {
        return Ok(false);
    }
    incident.status = IncidentStatus::Closed;
    incident.closed_at = Some(at.to_string());
    incident.last_seen_at = at.to_string();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE incidents SET status = 'closed', closed_at = ?2, last_seen_at = ?3 \
             WHERE id = ?1 AND status = 'open'",
            params![incident.id, incident.closed_at, incident.last_seen_at],
        )?;
        Ok(())
    })?;
    Ok(true)
}

/// Close every open incident of an instance (the instance is healthy again).
pub fn close_open_for_instance(db: &Db, instance_id: &str, at: &str) -> Result<usize, DbError> {
    let open = open_for_instance(db, instance_id)?;
    let mut closed = 0;
    for mut incident in open {
        if close(db, &mut incident, at)? {
            closed += 1;
        }
    }
    Ok(closed)
}

// ---------------------------------------------------------------------------
// Watchdog state
// ---------------------------------------------------------------------------

/// Per-instance sampling state. Deliberately in memory: it describes the
/// current streak and backoff of this process. Incidents are persisted, so a
/// restart of the control plane never loses what was observed, and the Docker
/// restart count (read on every tick) keeps bounding a crash loop.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct InstanceWatch {
    unhealthy_samples: u32,
    attempts: u32,
    last_attempt_at: Option<u64>,
    exhausted: bool,
}

/// Mutable state shared by the sampling loop and the API (a policy change
/// resets the streak and the attempt budget of its instance).
#[derive(Debug, Default)]
pub struct WatchdogState {
    inner: Mutex<HashMap<String, InstanceWatch>>,
}

impl WatchdogState {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, InstanceWatch>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Forget the streak and the attempt budget of one instance.
    pub fn reset(&self, id: &str) {
        self.lock().remove(id);
    }

    fn get(&self, id: &str) -> InstanceWatch {
        self.lock().get(id).cloned().unwrap_or_default()
    }

    fn put(&self, id: &str, watch: InstanceWatch) {
        let mut map = self.lock();
        if watch == InstanceWatch::default() {
            map.remove(id);
        } else {
            map.insert(id.to_string(), watch);
        }
    }
}

/// Summary of one sampling pass (used by the logs and the tests).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub sampled: usize,
    pub opened: usize,
    pub closed: usize,
    pub healed: usize,
    pub errors: usize,
}

/// What one sample says about the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sample {
    /// Running and healthy (or running without a health check).
    Healthy,
    /// Running but the health probe has not settled yet.
    Starting,
    /// Running but the health probe reports `unhealthy`.
    Unhealthy { restart_count: u32 },
    /// Not running, with evidence of a failure.
    Crashed {
        oom: bool,
        restart_count: u32,
        detail: String,
    },
    /// Deliberately stopped (clean exit, no error): never healed, no incident.
    Stopped,
    /// Driver could not answer: no conclusion this tick.
    Unavailable,
    /// The image the instance needs is absent from the host.
    ImageMissing { detail: String },
}

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn iso_at(epoch: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch as i64, 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Delay before the next heal attempt, doubled per attempt and capped.
pub fn backoff_s(base_s: u64, attempts: u32) -> u64 {
    let base = base_s.max(1);
    let exponent = attempts.saturating_sub(1).min(16);
    base.saturating_mul(1u64 << exponent)
        .min(HEAL_BACKOFF_CAP_S)
}

/// One sampling pass, for the background loop and the tests.
pub async fn tick(state: &AppState) -> TickReport {
    tick_at(state, epoch_now()).await
}

/// Sampling pass at an explicit Unix time (tests drive the backoff this way).
pub async fn tick_at(state: &AppState, now: u64) -> TickReport {
    let mut report = TickReport::default();
    let specs = match state.db.list() {
        Ok(specs) => specs,
        Err(err) => {
            tracing::warn!(%err, "watchdog: cannot list instances");
            return report;
        }
    };

    // `docker images` is fetched at most once per pass, and only when an
    // instance is broken enough that a missing image could be the reason.
    let mut images: std::collections::HashMap<String, Option<Vec<String>>> =
        std::collections::HashMap::new();

    for cached in specs {
        report.sampled += 1;
        // Serialise with lifecycle operations: a restart must never race an
        // operator's stop, and re-reading the spec picks up a policy change.
        let lock = state.locks.for_id(&cached.id);
        let _guard = lock.lock().await;
        let spec = match state.db.get(&cached.id) {
            Ok(Some(spec)) => spec,
            _ => continue,
        };

        let ps = state.driver.ps(&spec).await;
        let inspect = state.driver.inspect(&spec).await;
        let ps_ok = ps.as_ref().ok().cloned();
        let inspect_info = inspect.as_ref().ok().cloned().unwrap_or_default();

        let sample = classify(&spec, ps_ok.as_ref(), &inspect_info);

        // A dead container whose image is gone is an image problem, not a
        // crash loop: retrying a restart can never help. A cleanly stopped
        // instance is never probed (and never healed).
        let sample = if matches!(sample, Sample::Crashed { .. }) {
            match image_missing(state, &spec, &mut images).await {
                Some(detail) => Sample::ImageMissing { detail },
                None => sample,
            }
        } else {
            sample
        };

        apply_sample(state, &spec, sample, now, &mut report).await;
    }

    if report.opened > 0 || report.closed > 0 || report.healed > 0 || report.errors > 0 {
        tracing::info!(
            opened = report.opened,
            closed = report.closed,
            healed = report.healed,
            errors = report.errors,
            "watchdog pass"
        );
    }
    report
}

async fn image_missing(
    state: &AppState,
    spec: &InstanceSpec,
    cache: &mut std::collections::HashMap<String, Option<Vec<String>>>,
) -> Option<String> {
    let image = spec.image.trim();
    if image.is_empty() {
        return None;
    }
    let entry = cache.entry(spec.host.clone()).or_insert_with(|| None);
    if entry.is_none() {
        *entry = state
            .driver
            .list_images(&spec.host)
            .await
            .ok()
            .map(|records| records.into_iter().map(|record| record.tag).collect());
    }
    let images = entry.as_ref()?;
    if images.iter().any(|tag| tag == image) {
        return None;
    }
    Some(format!(
        "container image \"{image}\" is not present on the host; the instance cannot start \
         (pull it with `fleett upgrade --image {image}` or restore it locally)"
    ))
}

fn classify(
    _spec: &InstanceSpec,
    ps: Option<&crate::driver::PsResult>,
    inspect: &crate::driver::InspectInfo,
) -> Sample {
    let Some(ps) = ps else {
        // `ps` failed: Docker may be unreachable. Never heal on a blind read.
        return Sample::Unavailable;
    };
    if ps.status == Status::Running {
        return match ps.health {
            Health::Healthy => Sample::Healthy,
            Health::Starting => Sample::Starting,
            Health::Unknown => Sample::Healthy,
            Health::Unhealthy => Sample::Unhealthy {
                restart_count: inspect.restart_count,
            },
        };
    }

    let failure = inspect.error_message();
    let oom = inspect.oom_killed;
    let crashed = oom
        || failure.is_some()
        || ps.status == Status::Error
        || matches!(inspect.state.as_str(), "dead" | "restarting");
    if !crashed {
        // `stopped`/`created` and a clean inspect: a deliberate stop.
        return Sample::Stopped;
    }
    Sample::Crashed {
        oom,
        restart_count: inspect.restart_count,
        detail: failure.unwrap_or_else(|| {
            format!(
                "container is not running (docker state \"{}\", {} restart(s))",
                inspect.state, inspect.restart_count
            )
        }),
    }
}

fn unhealthy_detail(spec: &InstanceSpec, samples: u32) -> String {
    format!(
        "container health check failed for {samples} consecutive sample(s) \
         (threshold {})",
        spec.unhealthy_after
    )
}

async fn apply_sample(
    state: &AppState,
    spec: &InstanceSpec,
    sample: Sample,
    now: u64,
    report: &mut TickReport,
) {
    let at = iso_at(now);
    let mut watch = state.watchdog.get(&spec.id);

    match sample {
        Sample::Healthy => {
            state.watchdog.put(&spec.id, InstanceWatch::default());
            // Leave `error` behind: the sample says the container runs and the
            // health probe is happy. An unrelated `last_error` is kept.
            if spec.status != Status::Running
                || spec
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.starts_with(EXHAUSTED_ERROR_PREFIX))
            {
                let error = spec
                    .last_error
                    .as_deref()
                    .filter(|error| !error.starts_with(EXHAUSTED_ERROR_PREFIX));
                let _ = state.db.set_status(&spec.id, Status::Running, error);
            }
            match close_open_for_instance(&state.db, &spec.id, &at) {
                Ok(0) => {}
                Ok(closed) => {
                    report.closed += closed;
                    tracing::info!(instance = %spec.id, closed, "watchdog: instance healthy again");
                }
                Err(err) => {
                    report.errors += 1;
                    tracing::warn!(instance = %spec.id, %err, "watchdog: cannot close incidents");
                }
            }
        }
        Sample::Starting => {
            watch.unhealthy_samples = 0;
            state.watchdog.put(&spec.id, watch);
        }
        Sample::Stopped => {
            // A clean stop is not an incident and must never be healed. An
            // incident opened before the stop stays open (an operator still
            // has to see why the instance was unhealthy).
            watch.unhealthy_samples = 0;
            state.watchdog.put(&spec.id, watch);
        }
        Sample::Unavailable => {}
        Sample::Unhealthy { restart_count } => {
            watch.unhealthy_samples = watch.unhealthy_samples.saturating_add(1);
            if watch.unhealthy_samples >= spec.unhealthy_after {
                let detail = unhealthy_detail(spec, watch.unhealthy_samples);
                handle_incident(
                    state,
                    spec,
                    IncidentCause::Unhealthy,
                    &detail,
                    restart_count,
                    now,
                    &at,
                    &mut watch,
                    report,
                )
                .await;
            }
            state.watchdog.put(&spec.id, watch);
        }
        Sample::Crashed {
            oom,
            restart_count,
            detail,
        } => {
            watch.unhealthy_samples = 0;
            let cause = if oom {
                IncidentCause::Oom
            } else {
                IncidentCause::Crashloop
            };
            let detail = if oom {
                format!("{detail}; the container was killed by the OOM killer")
            } else {
                format!("{detail}; restart count {restart_count}")
            };
            if restart_count >= spec.max_restarts {
                watch.exhausted = true;
            }
            handle_incident(
                state,
                spec,
                cause,
                &detail,
                restart_count,
                now,
                &at,
                &mut watch,
                report,
            )
            .await;
            state.watchdog.put(&spec.id, watch);
        }
        Sample::ImageMissing { detail } => {
            watch.unhealthy_samples = 0;
            // Retrying a restart cannot pull an image: the incident is opened
            // but healing is exhausted immediately.
            watch.exhausted = true;
            handle_incident(
                state,
                spec,
                IncidentCause::ImageMissing,
                &detail,
                0,
                now,
                &at,
                &mut watch,
                report,
            )
            .await;
            state.watchdog.put(&spec.id, watch);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_incident(
    state: &AppState,
    spec: &InstanceSpec,
    cause: IncidentCause,
    detail: &str,
    restart_count: u32,
    now: u64,
    at: &str,
    watch: &mut InstanceWatch,
    report: &mut TickReport,
) {
    let (mut incident, created) =
        match ensure_open(&state.db, &spec.id, cause, detail, restart_count, at) {
            Ok(result) => result,
            Err(err) => {
                report.errors += 1;
                tracing::warn!(instance = %spec.id, %err, "watchdog: cannot record the incident");
                return;
            }
        };
    if created {
        report.opened += 1;
        tracing::warn!(
            instance = %spec.id,
            cause = cause.as_str(),
            detail,
            "watchdog: incident opened"
        );
    }

    if !spec.heal.heals() {
        return;
    }
    let was_exhausted = watch.exhausted;
    if was_exhausted || spec.max_restarts == 0 || watch.attempts >= spec.max_restarts {
        watch.exhausted = true;
        // The instance stays in error with the incident open: an operator must
        // look at it instead of watching an endless restart loop.
        let message = format!(
            "watchdog stopped healing {cause} after {} attempt(s); the incident stays open",
            watch.attempts
        );
        let _ = state.db.set_status(&spec.id, Status::Error, Some(&message));
        if !was_exhausted {
            let action = IncidentAction::new(at, "heal_exhausted", "skipped", Some(message));
            if let Err(err) = append_action(&state.db, &mut incident, action, at) {
                tracing::warn!(instance = %spec.id, %err, "watchdog: cannot record the action");
            }
        }
        return;
    }

    if let Some(last) = watch.last_attempt_at {
        let wait = backoff_s(spec.heal_backoff_s, watch.attempts);
        if now < last.saturating_add(wait) {
            return;
        }
    }

    let action = match spec.heal {
        HealPolicy::Restart => "restart",
        HealPolicy::Recreate => "recreate",
        HealPolicy::Off => return,
    };

    let outcome = heal(state, spec, spec.heal).await;
    watch.attempts = watch.attempts.saturating_add(1);
    watch.last_attempt_at = Some(now);
    watch.unhealthy_samples = 0;

    let (result, detail) = match outcome {
        Ok(()) => {
            report.healed += 1;
            state
                .metrics
                .record_compose_action(action, crate::metrics::RESULT_OK);
            tracing::info!(
                instance = %spec.id,
                action,
                cause = cause.as_str(),
                "watchdog: heal attempt succeeded"
            );
            ("ok", None)
        }
        Err(err) => {
            report.errors += 1;
            state
                .metrics
                .record_compose_action(action, crate::metrics::RESULT_ERROR);
            let message = err.detail.clone();
            tracing::warn!(
                instance = %spec.id,
                action,
                cause = cause.as_str(),
                %message,
                "watchdog: heal attempt failed"
            );
            let failed = ensure_open(
                &state.db,
                &spec.id,
                IncidentCause::HealFailed,
                &format!("{action} failed: {message}"),
                restart_count,
                at,
            );
            match failed {
                Ok((_, true)) => report.opened += 1,
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(instance = %spec.id, %err, "watchdog: cannot record heal failure");
                }
            }
            ("error", Some(message))
        }
    };

    let action = IncidentAction::new(at, action, result, detail);
    if let Err(err) = append_action(&state.db, &mut incident, action, at) {
        tracing::warn!(instance = %spec.id, %err, "watchdog: cannot record the action");
    }

    if watch.attempts >= spec.max_restarts {
        watch.exhausted = true;
        let message = format!(
            "watchdog stopped healing {cause} after {} attempt(s); the incident stays open",
            watch.attempts
        );
        let _ = state.db.set_status(&spec.id, Status::Error, Some(&message));
    }
}

async fn heal(
    state: &AppState,
    spec: &InstanceSpec,
    policy: HealPolicy,
) -> Result<(), crate::driver::DriverError> {
    match policy {
        HealPolicy::Off => Ok(()),
        HealPolicy::Restart => state.driver.restart(spec).await,
        HealPolicy::Recreate => {
            state.driver.down(spec).await?;
            state.driver.up(spec).await
        }
    }
}

/// Background loop started by `fleett serve`. A zero interval disables it.
pub async fn run(state: AppState) {
    let interval_s = state.cfg.watchdog_interval_s;
    if interval_s == 0 {
        tracing::info!("watchdog disabled (FLEETT_WATCHDOG_INTERVAL_S=0)");
        return;
    }
    let interval = std::time::Duration::from_secs(interval_s.min(MAX_INTERVAL_S));
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tracing::info!(interval_s, "watchdog started");
    loop {
        ticker.tick().await;
        tick(&state).await;
    }
}

/// Response of `PATCH /api/instances/{id}/heal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealPolicyView {
    pub id: String,
    pub heal: HealPolicy,
    pub unhealthy_after: u32,
    pub max_restarts: u32,
    pub heal_backoff_s: u64,
}

impl From<&InstanceSpec> for HealPolicyView {
    fn from(spec: &InstanceSpec) -> Self {
        Self {
            id: spec.id.clone(),
            heal: spec.heal,
            unhealthy_after: spec.unhealthy_after,
            max_restarts: spec.max_restarts,
            heal_backoff_s: spec.heal_backoff_s,
        }
    }
}

/// Details carried by the audit entry of a policy change.
pub fn policy_details(before: &InstanceSpec, after: &InstanceSpec) -> serde_json::Value {
    json!({
        "before": {
            "heal": before.heal.as_str(),
            "unhealthy_after": before.unhealthy_after,
            "max_restarts": before.max_restarts,
            "heal_backoff_s": before.heal_backoff_s,
        },
        "after": {
            "heal": after.heal.as_str(),
            "unhealthy_after": after.unhealthy_after,
            "max_restarts": after.max_restarts,
            "heal_backoff_s": after.heal_backoff_s,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::driver::mock::MockDriver;
    use crate::driver::InspectInfo;
    use crate::models::{
        ChannelKind, DEFAULT_API_KEY_ENV, DEFAULT_HEAL_BACKOFF_S, DEFAULT_MEM_LIMIT_MB,
        DEFAULT_UNHEALTHY_AFTER,
    };
    use std::sync::Arc;

    fn config(dir: &std::path::Path) -> Config {
        Config::for_tests(dir.to_path_buf())
    }

    fn spec(id: &str) -> InstanceSpec {
        InstanceSpec {
            id: id.to_string(),
            name: id.to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            provider: None,
            channel: ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: DEFAULT_MEM_LIMIT_MB,
            cpu_limit: 1.0,
            image: crate::config::DEFAULT_IMAGE.to_string(),
            previous_image: None,
            allow_lan: false,
            host: crate::hosts::LOCAL_HOST.to_string(),
            heal: HealPolicy::Off,
            unhealthy_after: DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: DEFAULT_HEAL_BACKOFF_S,
            notes: String::new(),
            status: Status::Running,
            last_error: None,
            created_at: crate::models::now_iso(),
            updated_at: crate::models::now_iso(),
        }
    }

    struct Harness {
        _dir: tempfile::TempDir,
        state: AppState,
        mock: Arc<MockDriver>,
    }

    impl Harness {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let cfg = config(dir.path());
            let mock = Arc::new(MockDriver::new());
            let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap");
            Self {
                _dir: dir,
                state,
                mock,
            }
        }

        fn add(&self, instance: InstanceSpec) -> InstanceSpec {
            self.state.db.insert_instance(&instance).expect("insert");
            self.mock.force_status(&instance.id, instance.status);
            instance
        }

        async fn pass(&self, now: u64) -> TickReport {
            tick_at(&self.state, now).await
        }

        fn incidents(&self, id: &str) -> Vec<Incident> {
            list(&self.state.db, None, Some(id), MAX_LIMIT).expect("incidents")
        }
    }

    #[tokio::test]
    async fn unhealthy_samples_open_one_incident_after_the_threshold() {
        let harness = Harness::new();
        let mut instance = spec("alice");
        instance.unhealthy_after = 3;
        let instance = harness.add(instance);
        harness.mock.force_health("alice", Health::Unhealthy);

        assert!(harness.pass(100).await.opened == 0);
        assert!(harness.pass(130).await.opened == 0);
        assert_eq!(harness.incidents("alice").len(), 0);

        let report = harness.pass(160).await;
        assert_eq!(report.opened, 1);
        let incidents = harness.incidents("alice");
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].cause, IncidentCause::Unhealthy);
        assert_eq!(incidents[0].status, IncidentStatus::Open);
        assert!(incidents[0].detail.contains("3 consecutive"));

        // A fourth unhealthy sample refreshes the same incident, never opens
        // a second one for the same cause.
        let report = harness.pass(190).await;
        assert_eq!(report.opened, 0);
        assert_eq!(harness.incidents("alice").len(), 1);
        assert!(harness
            .mock
            .calls()
            .iter()
            .all(|call| call.operation != "restart"));
        assert_eq!(instance.heal, HealPolicy::Off);
    }

    #[tokio::test]
    async fn a_healthy_instance_closes_its_incident() {
        let harness = Harness::new();
        let instance = harness.add(spec("alice"));
        harness.mock.force_health("alice", Health::Unhealthy);
        for step in 0..3 {
            harness.pass(100 + step * 30).await;
        }
        assert_eq!(harness.incidents(&instance.id).len(), 1);

        harness.mock.force_health("alice", Health::Healthy);
        let report = harness.pass(300).await;
        assert_eq!(report.closed, 1);
        let incidents = harness.incidents(&instance.id);
        assert_eq!(incidents[0].status, IncidentStatus::Closed);
        assert!(incidents[0].closed_at.is_some());
    }

    #[tokio::test]
    async fn a_cleanly_stopped_instance_opens_no_incident() {
        let harness = Harness::new();
        let mut instance = spec("alice");
        instance.heal = HealPolicy::Restart;
        let instance = harness.add(instance);
        harness.mock.force_status("alice", Status::Stopped);
        harness.mock.force_health("alice", Health::Unknown);

        for step in 0..5 {
            harness.pass(100 + step * 30).await;
        }
        assert!(harness.incidents(&instance.id).is_empty());
        assert!(harness
            .mock
            .calls()
            .iter()
            .all(|call| call.operation != "restart"));
    }

    #[tokio::test]
    async fn restart_policy_heals_after_the_threshold_and_respects_backoff() {
        let harness = Harness::new();
        let mut instance = spec("alice");
        instance.heal = HealPolicy::Restart;
        instance.unhealthy_after = 1;
        instance.heal_backoff_s = 30;
        let instance = harness.add(instance);
        harness.mock.force_health("alice", Health::Unhealthy);

        // Sample 1: threshold reached, one restart.
        let report = harness.pass(100).await;
        assert_eq!(report.healed, 1);
        let restarts = harness
            .mock
            .calls()
            .iter()
            .filter(|call| call.operation == "restart")
            .count();
        assert_eq!(restarts, 1);

        // The restart made the mock healthy; force the failure again to avoid
        // closing the incident on the next sample.
        harness.mock.force_health("alice", Health::Unhealthy);
        // Backoff (30 s) not elapsed: no second attempt.
        harness.pass(110).await;
        assert_eq!(
            harness
                .mock
                .calls()
                .iter()
                .filter(|call| call.operation == "restart")
                .count(),
            1
        );

        // Backoff elapsed: second attempt.
        harness.mock.force_health("alice", Health::Unhealthy);
        harness.pass(130).await;
        assert_eq!(
            harness
                .mock
                .calls()
                .iter()
                .filter(|call| call.operation == "restart")
                .count(),
            2
        );
        let incidents = harness.incidents(&instance.id);
        let unhealthy = incidents
            .iter()
            .find(|incident| incident.cause == IncidentCause::Unhealthy)
            .expect("unhealthy incident");
        assert_eq!(unhealthy.actions.len(), 2);
        assert!(unhealthy.actions.iter().all(|action| action.result == "ok"));
    }

    #[tokio::test]
    async fn recreate_policy_takes_the_container_down_and_up() {
        let harness = Harness::new();
        let mut instance = spec("alice");
        instance.heal = HealPolicy::Recreate;
        instance.unhealthy_after = 1;
        let instance = harness.add(instance);
        harness.mock.force_health("alice", Health::Unhealthy);

        harness.pass(100).await;
        let operations: Vec<&str> = harness
            .mock
            .calls()
            .iter()
            .map(|call| call.operation)
            .filter(|operation| matches!(*operation, "down" | "up"))
            .collect();
        assert_eq!(operations, ["down", "up"]);
        assert!(harness.mock.status_of(&instance.id).is_some());
    }

    #[tokio::test]
    async fn max_restarts_marks_the_instance_error_and_stops_healing() {
        let harness = Harness::new();
        let mut instance = spec("alice");
        instance.heal = HealPolicy::Restart;
        instance.unhealthy_after = 1;
        instance.max_restarts = 2;
        instance.heal_backoff_s = 1;
        let instance = harness.add(instance);
        harness.mock.force_health("alice", Health::Unhealthy);

        let mut now = 100;
        for _ in 0..8 {
            harness.mock.force_health("alice", Health::Unhealthy);
            harness.pass(now).await;
            now += 30;
        }

        let restarts = harness
            .mock
            .calls()
            .iter()
            .filter(|call| call.operation == "restart")
            .count();
        assert_eq!(restarts, 2);
        assert_eq!(
            harness
                .state
                .db
                .get("alice")
                .expect("get")
                .expect("present")
                .status,
            Status::Error
        );
        let incidents = harness.incidents(&instance.id);
        assert!(incidents
            .iter()
            .all(|incident| incident.status == IncidentStatus::Open));
    }

    #[tokio::test]
    async fn a_failed_heal_opens_a_heal_failed_incident() {
        let harness = Harness::new();
        let mut instance = spec("alice");
        instance.heal = HealPolicy::Restart;
        instance.unhealthy_after = 1;
        let instance = harness.add(instance);
        harness.mock.force_health("alice", Health::Unhealthy);
        harness
            .mock
            .fail_next("restart", "cannot restart the container");

        let report = harness.pass(100).await;
        assert_eq!(report.errors, 1);
        let incidents = harness.incidents(&instance.id);
        assert!(incidents
            .iter()
            .any(|incident| incident.cause == IncidentCause::HealFailed));
        let unhealthy = incidents
            .iter()
            .find(|incident| incident.cause == IncidentCause::Unhealthy)
            .expect("unhealthy incident");
        assert_eq!(unhealthy.actions[0].result, "error");
    }

    #[tokio::test]
    async fn crashloop_stops_at_max_restarts_and_keeps_the_incident_open() {
        let harness = Harness::new();
        let mut instance = spec("alice");
        instance.heal = HealPolicy::Restart;
        instance.max_restarts = 5;
        let instance = harness.add(instance);
        harness.mock.force_status("alice", Status::Error);
        harness.mock.force_health("alice", Health::Unknown);
        harness.mock.force_inspect(
            "alice",
            InspectInfo {
                state: "exited".to_string(),
                exit_code: Some(1),
                oom_killed: false,
                restart_count: 5,
                error: None,
                started_at: None,
            },
        );

        harness.pass(100).await;
        assert!(harness
            .mock
            .calls()
            .iter()
            .all(|call| call.operation != "restart"));
        let incidents = harness.incidents(&instance.id);
        let crashloop = incidents
            .iter()
            .find(|incident| incident.cause == IncidentCause::Crashloop)
            .expect("crashloop incident");
        assert_eq!(crashloop.restart_count, 5);
        assert_eq!(
            harness
                .state
                .db
                .get("alice")
                .expect("get")
                .expect("present")
                .status,
            Status::Error
        );
    }

    #[tokio::test]
    async fn an_oom_kill_opens_an_oom_incident() {
        let harness = Harness::new();
        let instance = harness.add(spec("alice"));
        harness.mock.force_status("alice", Status::Error);
        harness.mock.force_inspect(
            "alice",
            InspectInfo {
                state: "exited".to_string(),
                exit_code: Some(137),
                oom_killed: true,
                restart_count: 1,
                error: None,
                started_at: None,
            },
        );

        harness.pass(100).await;
        let incidents = harness.incidents(&instance.id);
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].cause, IncidentCause::Oom);
    }

    #[tokio::test]
    async fn a_missing_image_opens_an_incident_and_never_heals() {
        let harness = Harness::new();
        let mut instance = spec("alice");
        instance.heal = HealPolicy::Restart;
        instance.image = "gone:1".to_string();
        let instance = harness.add(instance);
        harness.mock.force_status("alice", Status::Error);
        harness.mock.force_inspect(
            "alice",
            InspectInfo {
                state: "exited".to_string(),
                exit_code: Some(1),
                oom_killed: false,
                restart_count: 0,
                error: None,
                started_at: None,
            },
        );

        harness.pass(100).await;
        let incidents = harness.incidents(&instance.id);
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].cause, IncidentCause::ImageMissing);
        assert!(harness
            .mock
            .calls()
            .iter()
            .all(|call| call.operation != "restart"));
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        assert_eq!(backoff_s(30, 1), 30);
        assert_eq!(backoff_s(30, 2), 60);
        assert_eq!(backoff_s(30, 3), 120);
        assert_eq!(backoff_s(30, 0), 30);
        assert_eq!(backoff_s(0, 1), 1);
        assert_eq!(backoff_s(60, 20), HEAL_BACKOFF_CAP_S);
    }

    #[test]
    fn causes_and_statuses_round_trip() {
        for cause in [
            IncidentCause::Oom,
            IncidentCause::Crashloop,
            IncidentCause::Unhealthy,
            IncidentCause::ImageMissing,
            IncidentCause::HealFailed,
        ] {
            assert_eq!(IncidentCause::parse(cause.as_str()), Some(cause));
        }
        assert_eq!(IncidentStatus::parse(" OPEN "), Some(IncidentStatus::Open));
        assert_eq!(
            IncidentStatus::parse("closed"),
            Some(IncidentStatus::Closed)
        );
        assert_eq!(IncidentStatus::parse("weird"), None);
    }
}
