//! Prometheus metrics exposition (issue #15).
//!
//! `GET /metrics` is served by a dedicated listener (`FLEETT_METRICS_BIND`, on
//! `127.0.0.1:9100` by default) so a scrape never shares the API port: the
//! endpoint answers without an API token, and the API port stays unreachable
//! from the scraper. A `FLEETT_METRICS_TOKEN` is required as soon as the bind
//! is not a loopback address.
//!
//! The exposition text (format version 0.0.4) is rendered by hand: the metric
//! set is small and fixed, so a client library and its dependency tree would
//! not pay for themselves.
//!
//! Utilisation costs one `docker stats` plus one `du` per data directory, so a
//! sample is cached for [`USAGE_CACHE_TTL`]: a 15 s scrape interval reads
//! Docker at most every 10 s. Every lifecycle action drops the cache, so a
//! start/stop is visible at the next scrape instead of ten seconds later.

use crate::api::AppState;
use crate::config::VERSION;
use crate::db::Db;
use crate::driver::UsageSample;
use crate::models::{Health, InstanceSpec, Status, HEALTH_UNHEALTHY_MESSAGE};
use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// `Content-Type` of the Prometheus text exposition format, version 0.0.4.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
/// Path served by the dedicated listener.
pub const METRICS_PATH: &str = "/metrics";
/// How long one utilisation sample is reused across scrapes.
pub const USAGE_CACHE_TTL: Duration = Duration::from_secs(10);

/// Result label of a compose action or an upgrade job that worked.
pub const RESULT_OK: &str = "ok";
/// Result label of a compose action that failed.
pub const RESULT_ERROR: &str = "error";

/// Counters of the control plane, keyed by their label values.
///
/// Kept in memory on purpose: they describe the life of one process, and a
/// restart is a legitimate reset as far as Prometheus is concerned.
#[derive(Debug, Default)]
struct Counters {
    api_requests: BTreeMap<(String, String), u64>,
    compose_actions: BTreeMap<(String, String), u64>,
    upgrade_jobs: BTreeMap<String, u64>,
}

/// Last utilisation sample, with the instant it was taken.
#[derive(Debug)]
struct CachedUsage {
    taken_at: Instant,
    samples: BTreeMap<String, UsageSample>,
}

/// In-memory counters plus the utilisation cache shared by every handler.
#[derive(Debug, Default)]
pub struct Metrics {
    counters: Mutex<Counters>,
    usage: Mutex<Option<CachedUsage>>,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count one API request (`fleett_api_requests_total`).
    pub fn record_request(&self, method: &str, status: u16) {
        let mut counters = self.counters();
        *counters
            .api_requests
            .entry((method.to_string(), status.to_string()))
            .or_default() += 1;
    }

    /// Count one compose action (`fleett_compose_actions_total`).
    ///
    /// Every action may change what `docker stats` reports, so the cached
    /// utilisation sample is dropped at the same time.
    pub fn record_compose_action(&self, action: &str, result: &str) {
        {
            let mut counters = self.counters();
            *counters
                .compose_actions
                .entry((action.to_string(), result.to_string()))
                .or_default() += 1;
        }
        self.invalidate_usage();
    }

    /// Count one finished upgrade job (`fleett_upgrade_jobs_total`).
    pub fn record_upgrade_job(&self, result: &str) {
        let mut counters = self.counters();
        *counters.upgrade_jobs.entry(result.to_string()).or_default() += 1;
    }

    /// Drop the cached utilisation sample.
    pub fn invalidate_usage(&self) {
        *self.usage() = None;
    }

    /// Render the exposition for the current state of the fleet.
    pub async fn scrape(&self, state: &AppState) -> String {
        let specs = match state.db.list() {
            Ok(specs) => specs,
            Err(err) => {
                tracing::warn!(%err, "cannot list the instances for the metrics scrape");
                Vec::new()
            }
        };
        let samples = self.usage_samples(state, &specs).await;
        let live = LiveState::read(&state.db);
        render(&self.snapshot(&specs, samples.as_ref(), &live))
    }

    /// Utilisation of the whole fleet, from the cache when it is fresh.
    async fn usage_samples(
        &self,
        state: &AppState,
        specs: &[InstanceSpec],
    ) -> Option<BTreeMap<String, UsageSample>> {
        {
            let cache = self.usage();
            if let Some(cached) = cache.as_ref() {
                if cached.taken_at.elapsed() < USAGE_CACHE_TTL {
                    return Some(cached.samples.clone());
                }
            }
        }

        let samples = match state.driver.usage(specs).await {
            Ok(samples) => samples,
            Err(err) => {
                tracing::debug!(%err, "the metrics scrape cannot read the container usage");
                return None;
            }
        };
        let by_id = specs
            .iter()
            .map(|spec| spec.id.clone())
            .zip(samples)
            .collect::<BTreeMap<_, _>>();
        *self.usage() = Some(CachedUsage {
            taken_at: Instant::now(),
            samples: by_id.clone(),
        });
        Some(by_id)
    }

    /// Freeze the current state into the value the renderer takes.
    fn snapshot(
        &self,
        specs: &[InstanceSpec],
        samples: Option<&BTreeMap<String, UsageSample>>,
        live: &LiveState,
    ) -> Snapshot {
        let rows = specs
            .iter()
            .map(|spec| {
                let sample = samples
                    .and_then(|samples| samples.get(&spec.id))
                    .cloned()
                    .unwrap_or_default();
                // Same rule as `GET /api/usage`: a container Docker reports as
                // running wins over a lifecycle status the control plane wrote
                // before the scrape.
                let status = if sample.running {
                    Status::Running
                } else {
                    spec.status
                };
                InstanceRow {
                    id: spec.id.clone(),
                    status: status.as_str().to_string(),
                    health: instance_health(spec, status).as_str().to_string(),
                    cpu_pct: sample.cpu_pct,
                    mem_used_bytes: sample.mem_used_bytes,
                    mem_limit_bytes: sample.mem_limit_bytes,
                    disk_bytes: sample.disk_bytes,
                }
            })
            .collect();

        let counters = self.counters();
        Snapshot {
            version: VERSION.to_string(),
            rows,
            usage_ok: samples.is_some(),
            api_requests: counters.api_requests.clone(),
            compose_actions: counters.compose_actions.clone(),
            upgrade_jobs: counters.upgrade_jobs.clone(),
            restarts: live.restarts.clone(),
            incidents_open: live.incidents_open,
            secret_expiry_days: live.secret_expiry_days.clone(),
            disk_quotas: live.disk_quotas.clone(),
        }
    }

    fn counters(&self) -> MutexGuard<'_, Counters> {
        self.counters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn usage(&self) -> MutexGuard<'_, Option<CachedUsage>> {
        self.usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Fleet state the control plane reads from the database for a scrape.
///
/// Every feature is detected at scrape time: a table or column that a newer
/// build adds (incidents, disk quotas, secret expiry) never makes the endpoint
/// fail on an older database. A missing feature yields an empty map, and the
/// renderer exposes the stable fallback value instead.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveState {
    /// Control-plane restarts per instance, from `audit_log`.
    pub restarts: BTreeMap<String, u64>,
    /// Incidents the registry still reports as open (`0` when the registry is
    /// absent).
    pub incidents_open: u64,
    /// Days until the earliest secret expires, per instance. `-1` means the
    /// expiry is unknown; an instance without secrets has no entry.
    pub secret_expiry_days: BTreeMap<String, i64>,
    /// Per-instance disk quota in bytes, when the quota feature is present.
    pub disk_quotas: BTreeMap<String, u64>,
}

impl LiveState {
    /// Read every optional feature from `db`, never failing on a missing one.
    pub fn read(db: &Db) -> Self {
        Self {
            restarts: restart_counts(db),
            incidents_open: open_incidents(db),
            secret_expiry_days: secret_expiry_days(db),
            disk_quotas: disk_quotas(db),
        }
    }
}

/// `PRAGMA table_info(<table>)` columns, empty when the table does not exist.
fn columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect()
}

/// Restarts requested through the control plane, per instance.
///
/// The audit trail has recorded `instance.restart` since schema v5; a database
/// created before that has no `audit_log` and simply reports no restarts.
/// Docker-driven restarts (a crashloop) are not visible here.
fn restart_counts(db: &Db) -> BTreeMap<String, u64> {
    match db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT target, COUNT(*) FROM audit_log \
             WHERE action = 'instance.restart' GROUP BY target",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.map(|row| {
            row.map(|(target, count)| (target, u64::try_from(count).unwrap_or_default()))
        })
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()
    }) {
        Ok(counts) => counts,
        Err(err) => {
            tracing::debug!(%err, "no restart history available for the metrics scrape");
            BTreeMap::new()
        }
    }
}

/// Open incidents, when the watchdog's registry exists.
fn open_incidents(db: &Db) -> u64 {
    match db.with_conn(|conn| {
        let columns = columns(conn, "incidents")?;
        if columns.is_empty() {
            return Ok(0);
        }
        // The registry lands with issue #32; accept the shapes it may take
        // instead of pinning the scrape to one unreleased schema.
        let query = if columns.iter().any(|column| column == "resolved_at") {
            "SELECT COUNT(*) FROM incidents WHERE resolved_at IS NULL"
        } else if columns.iter().any(|column| column == "closed_at") {
            "SELECT COUNT(*) FROM incidents WHERE closed_at IS NULL"
        } else if columns.iter().any(|column| column == "status") {
            "SELECT COUNT(*) FROM incidents WHERE lower(status) \
             IN ('open', 'acknowledged', 'investigating')"
        } else if columns.iter().any(|column| column == "open") {
            "SELECT COUNT(*) FROM incidents WHERE open = 1"
        } else {
            return Ok(0);
        };
        conn.query_row(query, [], |row| row.get::<_, i64>(0))
            .map(|count| u64::try_from(count).unwrap_or_default())
    }) {
        Ok(count) => count,
        Err(err) => {
            tracing::debug!(%err, "cannot count the open incidents for the metrics scrape");
            0
        }
    }
}

/// Days until the earliest secret of each instance expires.
///
/// The current schema has no expiry column, so every instance holding at least
/// one encrypted secret reports `-1` ("unknown"). When a later schema adds
/// `instance_env.expires_at`, the nearest deadline wins. Instances without a
/// secret are omitted: there is nothing to expire.
fn secret_expiry_days(db: &Db) -> BTreeMap<String, i64> {
    match db.with_conn(|conn| {
        let columns = columns(conn, "instance_env")?;
        if columns.is_empty() || !columns.iter().any(|column| column == "secret") {
            return Ok(BTreeMap::new());
        }
        let mut ids =
            conn.prepare("SELECT DISTINCT instance_id FROM instance_env WHERE secret = 1")?;
        let ids = ids
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut expiries: BTreeMap<String, i64> = ids.into_iter().map(|id| (id, -1)).collect();
        if columns.iter().any(|column| column == "expires_at") {
            let mut statement = conn.prepare(
                "SELECT instance_id, \
                        MIN(CAST(julianday(expires_at) - julianday('now') AS INTEGER)) \
                 FROM instance_env \
                 WHERE secret = 1 AND julianday(expires_at) IS NOT NULL \
                 GROUP BY instance_id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
            })?;
            for row in rows {
                let (id, days) = row?;
                if let Some(days) = days {
                    expiries.insert(id, days);
                }
            }
        }
        Ok(expiries)
    }) {
        Ok(expiries) => expiries,
        Err(err) => {
            tracing::debug!(%err, "cannot read the secret expiries for the metrics scrape");
            BTreeMap::new()
        }
    }
}

/// Per-instance disk quota in bytes, when the quota feature exists.
///
/// Issue #47 introduces `instances.disk_quota_mb`; accepting
/// `disk_quota_bytes` too keeps the scrape working whichever unit lands.
fn disk_quotas(db: &Db) -> BTreeMap<String, u64> {
    match db.with_conn(|conn| {
        let columns = columns(conn, "instances")?;
        let (column, scale) = if columns.iter().any(|name| name == "disk_quota_mb") {
            ("disk_quota_mb", 1024 * 1024)
        } else if columns.iter().any(|name| name == "disk_quota_bytes") {
            ("disk_quota_bytes", 1)
        } else {
            return Ok(BTreeMap::new());
        };
        let mut statement = conn.prepare(&format!(
            "SELECT id, {column} FROM instances \
             WHERE {column} IS NOT NULL AND {column} > 0"
        ))?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.map(|row| {
            row.map(|(id, value)| {
                (
                    id,
                    u64::try_from(value)
                        .unwrap_or_default()
                        .saturating_mul(scale),
                )
            })
        })
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()
    }) {
        Ok(quotas) => quotas,
        Err(err) => {
            tracing::debug!(%err, "cannot read the disk quotas for the metrics scrape");
            BTreeMap::new()
        }
    }
}

/// Container health, as far as the control plane has recorded it.
///
/// The database keeps the health *failure* (`last_error`), cleared as soon as a
/// probe answers again, so a running instance without that error is reported as
/// healthy and a non-running one as `none`.
fn instance_health(spec: &InstanceSpec, status: Status) -> Health {
    if spec.last_error.as_deref() == Some(HEALTH_UNHEALTHY_MESSAGE) {
        Health::Unhealthy
    } else if status == Status::Running {
        Health::Healthy
    } else {
        Health::Unknown
    }
}

/// One instance, flattened for the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceRow {
    pub id: String,
    pub status: String,
    pub health: String,
    pub cpu_pct: f64,
    pub mem_used_bytes: u64,
    pub mem_limit_bytes: u64,
    pub disk_bytes: u64,
}

/// Everything the exposition needs, gathered in one value so the renderer is a
/// pure function.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub version: String,
    pub rows: Vec<InstanceRow>,
    /// Whether the last utilisation sample could be read from the driver.
    pub usage_ok: bool,
    pub api_requests: BTreeMap<(String, String), u64>,
    pub compose_actions: BTreeMap<(String, String), u64>,
    pub upgrade_jobs: BTreeMap<String, u64>,
    /// Control-plane restarts per instance (`audit_log`).
    pub restarts: BTreeMap<String, u64>,
    /// Incidents still open; `0` when the registry is unavailable.
    pub incidents_open: u64,
    /// Days until the earliest secret expires, per instance (`-1` unknown).
    pub secret_expiry_days: BTreeMap<String, i64>,
    /// Disk quota in bytes, per instance (empty when the feature is absent).
    pub disk_quotas: BTreeMap<String, u64>,
}

/// Series that always exist, so a dashboard query never has to guess whether a
/// status was never seen or its count is zero.
const SEEDED_SERIES: [(&str, &str); 6] = [
    ("running", "healthy"),
    ("running", "unhealthy"),
    ("created", "none"),
    ("stopped", "none"),
    ("error", "none"),
    ("unknown", "none"),
];

/// Upgrade outcomes that always keep a series, even before the first job.
const UPGRADE_STATUSES: [&str; 3] = ["success", "failed", "rolled_back"];

/// Render the Prometheus text exposition (format version 0.0.4).
pub fn render(snapshot: &Snapshot) -> String {
    let mut out = String::with_capacity(2048);

    block(
        &mut out,
        "fleett_build_info",
        "Build information of the fleett control plane.",
        "gauge",
    );
    let _ = writeln!(
        out,
        "fleett_build_info{{version=\"{}\"}} 1",
        escape(&snapshot.version)
    );

    block(
        &mut out,
        "fleett_up",
        "1 when the control plane answers a scrape.",
        "gauge",
    );
    let _ = writeln!(out, "fleett_up 1");

    block(
        &mut out,
        "fleett_instances",
        "Instances per lifecycle status and recorded container health.",
        "gauge",
    );
    for ((status, health), count) in instance_counts(&snapshot.rows) {
        let _ = writeln!(
            out,
            "fleett_instances{{status=\"{}\",health=\"{}\"}} {count}",
            escape(&status),
            escape(&health)
        );
    }

    block(
        &mut out,
        "fleett_instance_cpu_percent",
        "CPU usage of one instance, in percent of one core.",
        "gauge",
    );
    for row in &snapshot.rows {
        let _ = writeln!(
            out,
            "fleett_instance_cpu_percent{{id=\"{}\"}} {}",
            escape(&row.id),
            number(row.cpu_pct)
        );
    }

    block(
        &mut out,
        "fleett_instance_mem_used_bytes",
        "Memory used by one instance container, in bytes.",
        "gauge",
    );
    for row in &snapshot.rows {
        let _ = writeln!(
            out,
            "fleett_instance_mem_used_bytes{{id=\"{}\"}} {}",
            escape(&row.id),
            row.mem_used_bytes
        );
    }

    block(
        &mut out,
        "fleett_instance_mem_limit_bytes",
        "Memory limit of one instance container, in bytes.",
        "gauge",
    );
    for row in &snapshot.rows {
        let _ = writeln!(
            out,
            "fleett_instance_mem_limit_bytes{{id=\"{}\"}} {}",
            escape(&row.id),
            row.mem_limit_bytes
        );
    }

    block(
        &mut out,
        "fleett_instance_disk_bytes",
        "Size of the data directory of one instance, in bytes.",
        "gauge",
    );
    for row in &snapshot.rows {
        let _ = writeln!(
            out,
            "fleett_instance_disk_bytes{{id=\"{}\"}} {}",
            escape(&row.id),
            row.disk_bytes
        );
    }

    block(
        &mut out,
        "fleett_instance_health",
        "1 when the recorded container health of the instance is healthy, 0 otherwise.",
        "gauge",
    );
    for row in &snapshot.rows {
        let _ = writeln!(
            out,
            "fleett_instance_health{{instance=\"{}\"}} {}",
            escape(&row.id),
            u8::from(row.health == "healthy")
        );
    }

    block(
        &mut out,
        "fleett_instance_restarts_total",
        "Restarts requested through the control plane, per instance (audit log).",
        "counter",
    );
    for row in &snapshot.rows {
        let _ = writeln!(
            out,
            "fleett_instance_restarts_total{{instance=\"{}\"}} {}",
            escape(&row.id),
            snapshot.restarts.get(&row.id).copied().unwrap_or_default()
        );
    }

    block(
        &mut out,
        "fleett_disk_quota_bytes",
        "Disk quota of one instance, in bytes; 0 when no quota is configured.",
        "gauge",
    );
    for row in &snapshot.rows {
        let _ = writeln!(
            out,
            "fleett_disk_quota_bytes{{instance=\"{}\"}} {}",
            escape(&row.id),
            snapshot
                .disk_quotas
                .get(&row.id)
                .copied()
                .unwrap_or_default()
        );
    }

    block(
        &mut out,
        "fleett_disk_used_bytes",
        "Disk used against the quota of one instance, in bytes; 0 when no quota is configured.",
        "gauge",
    );
    for row in &snapshot.rows {
        let used = if snapshot.disk_quotas.contains_key(&row.id) {
            row.disk_bytes
        } else {
            0
        };
        let _ = writeln!(
            out,
            "fleett_disk_used_bytes{{instance=\"{}\"}} {}",
            escape(&row.id),
            used
        );
    }

    block(
        &mut out,
        "fleett_secrets_expiring_days",
        "Days until the earliest secret of one instance expires; -1 when the expiry is unknown.",
        "gauge",
    );
    for row in &snapshot.rows {
        if let Some(days) = snapshot.secret_expiry_days.get(&row.id) {
            let _ = writeln!(
                out,
                "fleett_secrets_expiring_days{{instance=\"{}\"}} {days}",
                escape(&row.id)
            );
        }
    }

    block(
        &mut out,
        "fleett_incidents_open",
        "Incidents currently open; 0 when the incident registry is unavailable.",
        "gauge",
    );
    let _ = writeln!(out, "fleett_incidents_open {}", snapshot.incidents_open);

    block(
        &mut out,
        "fleett_api_requests_total",
        "HTTP requests served by the API, per method and status.",
        "counter",
    );
    for ((method, status), count) in &snapshot.api_requests {
        let _ = writeln!(
            out,
            "fleett_api_requests_total{{method=\"{}\",status=\"{}\"}} {count}",
            escape(method),
            escape(status)
        );
    }

    block(
        &mut out,
        "fleett_compose_actions_total",
        "Compose actions run by the control plane, per action and result.",
        "counter",
    );
    for ((action, result), count) in &snapshot.compose_actions {
        let _ = writeln!(
            out,
            "fleett_compose_actions_total{{action=\"{}\",result=\"{}\"}} {count}",
            escape(action),
            escape(result)
        );
    }

    block(
        &mut out,
        "fleett_upgrade_jobs_total",
        "Rolling upgrade jobs, per final result.",
        "counter",
    );
    for (result, count) in &snapshot.upgrade_jobs {
        let _ = writeln!(
            out,
            "fleett_upgrade_jobs_total{{result=\"{}\"}} {count}",
            escape(result)
        );
    }

    block(
        &mut out,
        "fleett_upgrades_total",
        "Rolling upgrade jobs, per final status (same counter as fleett_upgrade_jobs_total).",
        "counter",
    );
    for status in UPGRADE_STATUSES {
        let _ = writeln!(
            out,
            "fleett_upgrades_total{{status=\"{status}\"}} {}",
            snapshot
                .upgrade_jobs
                .get(status)
                .copied()
                .unwrap_or_default()
        );
    }

    block(
        &mut out,
        "fleett_rollbacks_total",
        "Rolling upgrade jobs that rolled at least one instance back.",
        "counter",
    );
    let _ = writeln!(
        out,
        "fleett_rollbacks_total {}",
        snapshot
            .upgrade_jobs
            .get("rolled_back")
            .copied()
            .unwrap_or_default()
    );

    block(
        &mut out,
        "fleett_usage_ok",
        "1 when the last utilisation sample could be read from Docker.",
        "gauge",
    );
    let _ = writeln!(out, "fleett_usage_ok {}", u8::from(snapshot.usage_ok));

    out
}

/// Count the rows, always emitting the [`SEEDED_SERIES`] first.
fn instance_counts(rows: &[InstanceRow]) -> BTreeMap<(String, String), u64> {
    let mut counts: BTreeMap<(String, String), u64> = SEEDED_SERIES
        .iter()
        .map(|(status, health)| ((status.to_string(), health.to_string()), 0))
        .collect();
    for row in rows {
        *counts
            .entry((row.status.clone(), row.health.clone()))
            .or_default() += 1;
    }
    counts
}

fn block(out: &mut String, name: &str, help: &str, kind: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

/// Prometheus label values escape backslashes, double quotes and newlines.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(character),
        }
    }
    out
}

/// A gauge value Prometheus can always parse.
fn number(value: f64) -> String {
    if value.is_finite() {
        value.to_string()
    } else {
        "0".to_string()
    }
}

/// Router of the dedicated metrics listener.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route(METRICS_PATH, get(scrape))
        .layer(middleware::from_fn_with_state(state.clone(), require_token))
        .with_state(state)
}

async fn scrape(State(state): State<AppState>) -> Response {
    let body = state.metrics.scrape(&state).await;
    ([(header::CONTENT_TYPE, CONTENT_TYPE)], body).into_response()
}

/// Require `Authorization: Bearer <FLEETT_METRICS_TOKEN>` when a token is set.
///
/// A missing token only happens on a loopback bind, which `Config` refuses to
/// combine with a public address.
async fn require_token(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(expected) = state.cfg.metrics.token.as_deref() else {
        return next.run(request).await;
    };
    let provided = crate::api::auth::bearer_token(request.headers());
    let authorized = provided.is_some_and(|token| {
        crate::api::auth::constant_time_eq(expected.as_bytes(), token.as_bytes())
    });
    if !authorized {
        return crate::api::ApiError::unauthorized_with(
            "unauthorized",
            "the metrics endpoint requires the bearer token configured in FLEETT_METRICS_TOKEN",
        )
        .into_response();
    }
    next.run(request).await
}

/// Middleware counting every API request by method and status.
pub async fn track_request(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().as_str().to_string();
    let response = next.run(request).await;
    state
        .metrics
        .record_request(&method, response.status().as_u16());
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, status: &str, health: &str) -> InstanceRow {
        InstanceRow {
            id: id.to_string(),
            status: status.to_string(),
            health: health.to_string(),
            cpu_pct: 12.5,
            mem_used_bytes: 2 * 1024 * 1024,
            mem_limit_bytes: 1536 * 1024 * 1024,
            disk_bytes: 4096,
        }
    }

    fn snapshot() -> Snapshot {
        let metrics = Metrics::new();
        metrics.record_request("GET", 200);
        metrics.record_request("GET", 200);
        metrics.record_request("POST", 201);
        metrics.record_compose_action("stop", RESULT_OK);
        metrics.record_upgrade_job("success");
        let mut snapshot = metrics.snapshot(
            &[],
            Some(&BTreeMap::from([(
                "alice".to_string(),
                UsageSample {
                    running: true,
                    cpu_pct: 12.5,
                    mem_used_bytes: 2 * 1024 * 1024,
                    mem_limit_bytes: 1536 * 1024 * 1024,
                    disk_bytes: 4096,
                },
            )])),
            &LiveState::default(),
        );
        snapshot.rows = vec![row("alice", "running", "healthy")];
        snapshot
    }

    /// Every sample line is `name{labels} value`, and the value is finite.
    fn sample_lines(text: &str) -> Vec<(&str, f64)> {
        text.lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .map(|line| {
                let (head, value) = line
                    .rsplit_once(' ')
                    .unwrap_or_else(|| panic!("no value on {line:?}"));
                let name = head.split('{').next().expect("metric name");
                let parsed = value
                    .parse::<f64>()
                    .unwrap_or_else(|err| panic!("{line:?} is not a number: {err}"));
                assert!(parsed.is_finite(), "{line:?} is not finite");
                (name, parsed)
            })
            .collect()
    }

    #[test]
    fn every_metric_documents_its_help_and_type() {
        let text = render(&snapshot());
        for (name, kind) in [
            ("fleett_build_info", "gauge"),
            ("fleett_up", "gauge"),
            ("fleett_instances", "gauge"),
            ("fleett_instance_cpu_percent", "gauge"),
            ("fleett_instance_mem_used_bytes", "gauge"),
            ("fleett_instance_mem_limit_bytes", "gauge"),
            ("fleett_instance_disk_bytes", "gauge"),
            ("fleett_instance_health", "gauge"),
            ("fleett_instance_restarts_total", "counter"),
            ("fleett_disk_quota_bytes", "gauge"),
            ("fleett_disk_used_bytes", "gauge"),
            ("fleett_secrets_expiring_days", "gauge"),
            ("fleett_incidents_open", "gauge"),
            ("fleett_api_requests_total", "counter"),
            ("fleett_compose_actions_total", "counter"),
            ("fleett_upgrade_jobs_total", "counter"),
            ("fleett_upgrades_total", "counter"),
            ("fleett_rollbacks_total", "counter"),
            ("fleett_usage_ok", "gauge"),
        ] {
            assert!(
                text.contains(&format!("# HELP {name} ")),
                "{name} has no HELP line:\n{text}"
            );
            assert!(
                text.contains(&format!("# TYPE {name} {kind}\n")),
                "{name} has no TYPE {kind} line:\n{text}"
            );
        }
    }

    #[test]
    fn every_sample_carries_a_finite_number() {
        let text = render(&snapshot());
        let lines = sample_lines(&text);
        assert!(!lines.is_empty());
        assert_eq!(
            lines.len(),
            text.lines()
                .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
                .count()
        );
        // 6 instance series + 4 legacy per-instance series + 4 new
        // per-instance series + 2 API + 1 action + 1 upgrade job +
        // 3 seeded upgrade statuses + 1 rollback total + 1 incidents +
        // build_info + up + usage_ok.
        assert_eq!(lines.len(), 6 + 4 + 4 + 2 + 1 + 1 + 3 + 1 + 1 + 3);
    }

    #[test]
    fn the_fleet_is_counted_per_status_and_health() {
        let mut snapshot = snapshot();
        snapshot.rows = vec![
            row("alice", "running", "healthy"),
            row("bob", "running", "healthy"),
            row("carol", "stopped", "none"),
        ];
        let text = render(&snapshot);
        assert!(
            text.contains("fleett_instances{status=\"running\",health=\"healthy\"} 2"),
            "{text}"
        );
        assert!(
            text.contains("fleett_instances{status=\"stopped\",health=\"none\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("fleett_instances{status=\"error\",health=\"none\"} 0"),
            "unseen statuses keep their series:\n{text}"
        );
    }

    #[test]
    fn an_empty_fleet_still_exposes_the_instance_series() {
        let snapshot = Snapshot {
            version: "0.3.0".to_string(),
            ..Snapshot::default()
        };
        let text = render(&snapshot);
        for (status, health) in SEEDED_SERIES {
            assert!(
                text.contains(&format!(
                    "fleett_instances{{status=\"{status}\",health=\"{health}\"}} 0"
                )),
                "{status}/{health} missing:\n{text}"
            );
        }
        assert!(text.contains("fleett_up 1"));
        assert!(text.contains("fleett_usage_ok 0"));
    }

    #[test]
    fn counters_start_empty_and_increment() {
        let metrics = Metrics::new();
        let empty = metrics.snapshot(&[], None, &LiveState::default());
        assert!(empty.api_requests.is_empty());
        assert!(empty.compose_actions.is_empty());
        assert!(empty.upgrade_jobs.is_empty());
        assert!(!empty.usage_ok);

        metrics.record_request("GET", 200);
        metrics.record_request("GET", 200);
        metrics.record_request("GET", 500);
        metrics.record_compose_action("start", RESULT_OK);
        metrics.record_compose_action("start", RESULT_ERROR);
        metrics.record_upgrade_job("rolled_back");

        let counters = metrics.snapshot(&[], None, &LiveState::default());
        assert_eq!(
            counters.api_requests.get(&("GET".into(), "200".into())),
            Some(&2)
        );
        assert_eq!(
            counters.api_requests.get(&("GET".into(), "500".into())),
            Some(&1)
        );
        assert_eq!(
            counters
                .compose_actions
                .get(&("start".into(), RESULT_OK.into())),
            Some(&1)
        );
        assert_eq!(
            counters
                .compose_actions
                .get(&("start".into(), RESULT_ERROR.into())),
            Some(&1)
        );
        assert_eq!(counters.upgrade_jobs.get("rolled_back"), Some(&1));

        let text = render(&counters);
        assert!(
            text.contains("fleett_api_requests_total{method=\"GET\",status=\"200\"} 2"),
            "{text}"
        );
        assert!(
            text.contains("fleett_compose_actions_total{action=\"start\",result=\"error\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("fleett_upgrade_jobs_total{result=\"rolled_back\"} 1"),
            "{text}"
        );
    }

    #[test]
    fn the_rows_report_the_recorded_usage() {
        let mut snapshot = snapshot();
        snapshot.rows = vec![row("alice", "running", "healthy")];
        let text = render(&snapshot);
        assert!(
            text.contains("fleett_instance_cpu_percent{id=\"alice\"} 12.5"),
            "{text}"
        );
        assert!(
            text.contains("fleett_instance_mem_used_bytes{id=\"alice\"} 2097152"),
            "{text}"
        );
        assert!(
            text.contains("fleett_instance_mem_limit_bytes{id=\"alice\"} 1610612736"),
            "{text}"
        );
        assert!(
            text.contains("fleett_instance_disk_bytes{id=\"alice\"} 4096"),
            "{text}"
        );
        assert!(text.contains("fleett_usage_ok 1"));
    }

    #[test]
    fn health_restarts_quotas_and_secrets_are_rendered_per_instance() {
        let mut snapshot = snapshot();
        snapshot.rows = vec![
            row("alice", "running", "healthy"),
            row("bob", "running", "unhealthy"),
            row("carol", "stopped", "none"),
        ];
        snapshot.restarts = BTreeMap::from([("alice".to_string(), 2)]);
        snapshot.disk_quotas = BTreeMap::from([("alice".to_string(), 10 * 1024 * 1024)]);
        snapshot.secret_expiry_days =
            BTreeMap::from([("alice".to_string(), 12), ("bob".to_string(), -1)]);

        let text = render(&snapshot);
        assert!(
            text.contains("fleett_instance_health{instance=\"alice\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("fleett_instance_health{instance=\"bob\"} 0"),
            "{text}"
        );
        assert!(
            text.contains("fleett_instance_health{instance=\"carol\"} 0"),
            "{text}"
        );
        assert!(
            text.contains("fleett_instance_restarts_total{instance=\"alice\"} 2"),
            "{text}"
        );
        assert!(
            text.contains("fleett_instance_restarts_total{instance=\"bob\"} 0"),
            "{text}"
        );
        assert!(
            text.contains("fleett_disk_quota_bytes{instance=\"alice\"} 10485760"),
            "{text}"
        );
        assert!(
            text.contains("fleett_disk_quota_bytes{instance=\"bob\"} 0"),
            "{text}"
        );
        assert!(
            text.contains("fleett_disk_used_bytes{instance=\"alice\"} 4096"),
            "the used series mirrors the measured data directory:\n{text}"
        );
        assert!(
            text.contains("fleett_disk_used_bytes{instance=\"bob\"} 0"),
            "without a quota the used series falls back to 0:\n{text}"
        );
        assert!(
            text.contains("fleett_secrets_expiring_days{instance=\"alice\"} 12"),
            "{text}"
        );
        assert!(
            text.contains("fleett_secrets_expiring_days{instance=\"bob\"} -1"),
            "{text}"
        );
        assert!(
            !text.contains("fleett_secrets_expiring_days{instance=\"carol\"}"),
            "an instance without secrets is omitted:\n{text}"
        );
    }

    #[test]
    fn upgrades_rollbacks_and_incidents_are_counted() {
        let metrics = Metrics::new();
        metrics.record_upgrade_job("success");
        metrics.record_upgrade_job("success");
        metrics.record_upgrade_job("rolled_back");
        let snapshot = metrics.snapshot(
            &[],
            None,
            &LiveState {
                incidents_open: 4,
                ..LiveState::default()
            },
        );
        let text = render(&snapshot);
        assert!(
            text.contains("fleett_upgrades_total{status=\"success\"} 2"),
            "{text}"
        );
        assert!(
            text.contains("fleett_upgrades_total{status=\"failed\"} 0"),
            "unseen statuses keep their series:\n{text}"
        );
        assert!(
            text.contains("fleett_upgrades_total{status=\"rolled_back\"} 1"),
            "{text}"
        );
        assert!(text.contains("fleett_rollbacks_total 1"), "{text}");
        assert!(text.contains("fleett_incidents_open 4"), "{text}");
    }

    #[test]
    fn optional_features_do_not_break_a_scrape_on_a_current_schema() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");
        assert_eq!(LiveState::read(&db), LiveState::default());
    }

    #[test]
    fn future_feature_tables_and_columns_are_detected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");

        // No expiry column yet: a secret reports an unknown (-1) deadline.
        db.with_transaction(|conn| {
            conn.execute(
                "INSERT INTO instance_env (instance_id, key, secret, value, updated_at) \
                 VALUES ('alice', 'TOKEN', 1, NULL, 'now')",
                [],
            )?;
            Ok(())
        })
        .expect("insert a secret");
        assert_eq!(
            secret_expiry_days(&db).get("alice"),
            Some(&-1),
            "without an expiry column the deadline is unknown"
        );

        db.with_transaction(|conn| {
            conn.execute_batch(
                "ALTER TABLE instance_env ADD COLUMN expires_at TEXT;
                 ALTER TABLE instances ADD COLUMN disk_quota_mb INTEGER;
                 CREATE TABLE incidents (id INTEGER PRIMARY KEY, status TEXT NOT NULL);
                 UPDATE instance_env SET expires_at = datetime('now', '+30 days');
                 INSERT INTO instances (id, name, template, model, base_url, api_key_env, \
                     channel, status, mem_limit_mb, created_at, updated_at, disk_quota_mb) \
                 VALUES ('alice', 'Alice', 'guest', 'm', 'https://example.test/v1', \
                     'OPENAI_API_KEY', 'none', 'running', 512, 'now', 'now', 100);
                 INSERT INTO incidents (status) VALUES ('open'), ('acknowledged'), ('resolved');",
            )
        })
        .expect("create the optional feature columns");

        let days = secret_expiry_days(&db);
        let days = days.get("alice").copied().expect("alice has a secret");
        assert!((28..=30).contains(&days), "unexpected deadline: {days}");
        assert_eq!(disk_quotas(&db).get("alice"), Some(&(100 * 1024 * 1024)));
        assert_eq!(open_incidents(&db), 2);
    }

    #[test]
    fn a_non_finite_measurement_never_reaches_the_output() {
        let mut snapshot = snapshot();
        let mut broken = row("alice", "running", "healthy");
        broken.cpu_pct = f64::NAN;
        snapshot.rows = vec![broken];
        let text = render(&snapshot);
        assert!(
            text.contains("fleett_instance_cpu_percent{id=\"alice\"} 0"),
            "{text}"
        );
        sample_lines(&text);
    }

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("a\"b"), "a\\\"b");
        assert_eq!(escape("a\\b"), "a\\\\b");
        assert_eq!(escape("a\nb"), "a\\nb");
    }

    #[test]
    fn the_health_follows_the_recorded_failure() {
        let mut spec = InstanceSpec {
            id: "alice".to_string(),
            name: "Alice".to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: crate::models::DEFAULT_API_KEY_ENV.to_string(),
            provider: None,
            channel: crate::models::ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: 512,
            cpu_limit: 1.0,
            image: "agent:1".to_string(),
            previous_image: None,
            allow_lan: false,
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            notes: String::new(),
            status: Status::Running,
            last_error: None,
            created_at: crate::models::now_iso(),
            updated_at: crate::models::now_iso(),
        };
        assert_eq!(instance_health(&spec, Status::Running), Health::Healthy);
        assert_eq!(instance_health(&spec, Status::Stopped), Health::Unknown);

        spec.last_error = Some(HEALTH_UNHEALTHY_MESSAGE.to_string());
        assert_eq!(instance_health(&spec, Status::Running), Health::Unhealthy);
        assert_eq!(instance_health(&spec, Status::Stopped), Health::Unhealthy);
    }
}
