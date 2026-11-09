//! Per-instance disk quotas (issue #47).
//!
//! The control plane samples the size of `instances/<id>/data` (through the
//! same `du` the metrics already pay for) and compares it with the instance
//! quota:
//!
//! * below 80 %: nothing happens;
//! * at 80 %: one `warn` event per crossing (a crossing is entering the band,
//!   not every tick);
//! * at 100 %: the instance policy is applied once — `warn` alerts only,
//!   `freeze` stops the instance and records
//!   [`DISK_QUOTA_EXCEEDED_MESSAGE`], `stop` stops it silently.
//!
//! The in-memory [`QuotaMonitor`] remembers the last level of every instance,
//! so a container that stays above its quota is never restarted or re-stopped
//! on every tick. It is deliberately not persisted: a control-plane restart
//! re-evaluates the fleet once, which is what an operator expects after an
//! upgrade.
//!
//! Events are appended to `disk_events` (migration v11). There is no
//! notification backend in this build yet: [`notify`] logs every event, and a
//! future notifier can consume the same table without touching this module.

use crate::api::AppState;
use crate::config::Config;
use crate::db::{Db, DbError};
use crate::models::{
    now_iso, DiskPolicy, InstanceSpec, Status, DISK_QUOTA_EXCEEDED_MESSAGE, DISK_WARN_PERCENT,
};
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// Journal actions written to `disk_events`.
pub const ACTION_WARN: &str = "warn";
pub const ACTION_FREEZE: &str = "freeze";
pub const ACTION_STOP: &str = "stop";
pub const ACTION_CLEAN: &str = "clean";
pub const ACTIONS: [&str; 4] = [ACTION_WARN, ACTION_FREEZE, ACTION_STOP, ACTION_CLEAN];

/// Files older than this are removed by a cleanup (rotated logs, mostly).
pub const CLEANUP_LOG_AGE_DAYS: u64 = 7;
/// Default page size of `GET /api/disk/events`.
pub const DEFAULT_EVENTS_LIMIT: usize = 50;
/// Largest page accepted by `GET /api/disk/events`.
pub const MAX_EVENTS_LIMIT: usize = 500;

const MIB: u64 = 1024 * 1024;

/// Severity of one instance, derived from its data size and its quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiskLevel {
    Ok,
    Warn,
    Exceeded,
}

impl DiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Exceeded => "exceeded",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ok" => Some(Self::Ok),
            "warn" | "warning" => Some(Self::Warn),
            "exceeded" | "full" => Some(Self::Exceeded),
            _ => None,
        }
    }
}

impl std::fmt::Display for DiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Quota of an instance and what it currently uses.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaView {
    pub instance: String,
    /// `0` means unlimited.
    pub quota_mb: u32,
    pub policy: String,
    pub used_bytes: u64,
    pub quota_bytes: u64,
    /// `used_bytes / quota_bytes`, in percent (`0.0` when unlimited).
    pub percent: f64,
    pub state: String,
}

/// One row of `disk_events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiskEvent {
    pub id: i64,
    pub instance: String,
    pub at: String,
    pub action: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A row to append to `disk_events`.
#[derive(Debug, Clone)]
pub struct NewDiskEvent {
    pub instance_id: String,
    pub action: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub detail: Option<String>,
}

/// What a cleanup freed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CleanupReport {
    pub instance: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub freed_bytes: u64,
    pub removed_files: usize,
    /// Paths relative to the data directory, sorted.
    pub removed_paths: Vec<String>,
}

/// Size breakdown of one data directory, for `GET .../usage/detail`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DirBreakdown {
    pub total_bytes: u64,
    pub tmp_bytes: u64,
    pub cache_bytes: u64,
    pub logs_bytes: u64,
    pub other_bytes: u64,
    pub file_count: u64,
}

/// Last level seen for every instance, so a warning fires once per crossing.
#[derive(Debug, Default)]
pub struct QuotaMonitor {
    levels: Mutex<HashMap<String, DiskLevel>>,
}

impl QuotaMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Last level recorded for `id`.
    pub fn level(&self, id: &str) -> Option<DiskLevel> {
        self.levels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .copied()
    }

    /// Record `level`, returning `true` when it is a new crossing.
    pub fn observe(&self, id: &str, level: DiskLevel) -> bool {
        let mut levels = self
            .levels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = levels.insert(id.to_string(), level);
        previous != Some(level)
    }

    /// Forget one instance (its quota was removed or the instance is gone).
    pub fn forget(&self, id: &str) {
        self.levels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
    }
}

/// Quota in bytes, saturating instead of overflowing.
pub fn quota_bytes(quota_mb: u32) -> u64 {
    u64::from(quota_mb).saturating_mul(MIB)
}

/// Classify `used_bytes` against `quota_mb` (`0` is always `ok`).
pub fn assess(used_bytes: u64, quota_mb: u32) -> DiskLevel {
    if quota_mb == 0 {
        return DiskLevel::Ok;
    }
    let limit = quota_bytes(quota_mb) as u128;
    let used = used_bytes as u128;
    if used >= limit {
        DiskLevel::Exceeded
    } else if used * 100 >= limit * u128::from(DISK_WARN_PERCENT) {
        DiskLevel::Warn
    } else {
        DiskLevel::Ok
    }
}

/// Percentage of `quota_mb` used, rounded to two decimals by the API.
pub fn percent(used_bytes: u64, quota_mb: u32) -> f64 {
    if quota_mb == 0 {
        return 0.0;
    }
    (used_bytes as f64) * 100.0 / (quota_bytes(quota_mb) as f64)
}

/// Build the public view of one instance.
pub fn view(spec: &InstanceSpec, used_bytes: u64) -> QuotaView {
    QuotaView {
        instance: spec.id.clone(),
        quota_mb: spec.disk_quota_mb,
        policy: spec.disk_policy.as_str().to_string(),
        used_bytes,
        quota_bytes: quota_bytes(spec.disk_quota_mb),
        percent: (percent(used_bytes, spec.disk_quota_mb) * 100.0).round() / 100.0,
        state: assess(used_bytes, spec.disk_quota_mb).as_str().to_string(),
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

fn row_to_event(row: &Row<'_>) -> rusqlite::Result<DiskEvent> {
    Ok(DiskEvent {
        id: row.get("id")?,
        instance: row.get("instance_id")?,
        at: row.get("at")?,
        action: row.get("action")?,
        before_bytes: row.get::<_, i64>("before_bytes")?.max(0) as u64,
        after_bytes: row.get::<_, i64>("after_bytes")?.max(0) as u64,
        detail: row.get("detail")?,
    })
}

/// Append one event and return it with its id.
pub fn record_event(db: &Db, event: &NewDiskEvent) -> Result<DiskEvent, DbError> {
    let at = now_iso();
    db.with_transaction(|conn| {
        conn.execute(
            "INSERT INTO disk_events \
             (instance_id, at, action, before_bytes, after_bytes, detail) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.instance_id,
                at,
                event.action,
                event.before_bytes as i64,
                event.after_bytes as i64,
                event.detail,
            ],
        )?;
        let id = conn.last_insert_rowid();
        conn.query_row(
            "SELECT id, instance_id, at, action, before_bytes, after_bytes, detail \
             FROM disk_events WHERE id = ?1",
            params![id],
            row_to_event,
        )
    })
}

/// Events of one instance (or of the whole fleet), newest first.
pub fn list_events(
    db: &Db,
    instance: Option<&str>,
    limit: usize,
) -> Result<Vec<DiskEvent>, DbError> {
    db.with_conn(|conn| match instance {
        Some(id) => {
            let mut statement = conn.prepare(
                "SELECT id, instance_id, at, action, before_bytes, after_bytes, detail \
                     FROM disk_events WHERE instance_id = ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let rows = statement
                .query_map(params![id, limit as i64], row_to_event)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }
        None => {
            let mut statement = conn.prepare(
                "SELECT id, instance_id, at, action, before_bytes, after_bytes, detail \
                     FROM disk_events ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = statement
                .query_map(params![limit as i64], row_to_event)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }
    })
}

/// Last event of one instance (used to expose the journal in the API).
pub fn last_event(db: &Db, instance: &str) -> Result<Option<DiskEvent>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, instance_id, at, action, before_bytes, after_bytes, detail \
             FROM disk_events WHERE instance_id = ?1 ORDER BY id DESC LIMIT 1",
            params![instance],
            row_to_event,
        )
        .optional()
    })
}

// ---------------------------------------------------------------------------
// Host capacity guard
// ---------------------------------------------------------------------------

/// Free space a host must keep before a new instance is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub free_mb: u64,
    pub required_mb: u64,
}

/// Returned by [`ensure_capacity`] when a create would eat the reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsufficientDisk(pub Capacity);

/// Reserve a create must leave free: the configured floor, or twice the quota.
pub fn required_free_mb(min_free_mb: u64, quota_mb: u32) -> u64 {
    min_free_mb.max(u64::from(quota_mb).saturating_mul(2))
}

/// Refuse a create that would leave less than [`required_free_mb`] on the host.
pub fn ensure_capacity(
    free_bytes: u64,
    quota_mb: u32,
    min_free_mb: u64,
) -> Result<(), InsufficientDisk> {
    let required_mb = required_free_mb(min_free_mb, quota_mb);
    if required_mb == 0 {
        return Ok(());
    }
    let free_mb = free_bytes / MIB;
    if free_mb < required_mb {
        return Err(InsufficientDisk(Capacity {
            free_mb,
            required_mb,
        }));
    }
    Ok(())
}

/// Free bytes on the filesystem holding `path`, or `None` when `df` fails.
///
/// `df -B1 --output=avail` is preferred (GNU coreutils); the classic
/// `df -B1` output is parsed as a fallback for busybox hosts. No new
/// dependency is pulled in for one number.
pub fn host_free_bytes(path: &Path) -> Option<u64> {
    let gnu = Command::new("df")
        .args(["-B1", "--output=avail"])
        .arg(path)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let text = String::from_utf8_lossy(&output.stdout);
            text.lines().nth(1).and_then(parse_u64)
        });
    if let Some(free) = gnu {
        return Some(free);
    }

    let output = Command::new("df").arg("-B1").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Filesystem 1B-blocks Used Available Use% Mounted-on
    text.lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(parse_u64)
}

fn parse_u64(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

// ---------------------------------------------------------------------------
// Quota loop
// ---------------------------------------------------------------------------

/// Sample every quota-bound instance once and apply its policy.
///
/// Returns the events emitted by this tick, which is what the tests assert on.
/// Failures are logged, never propagated: one unreachable Docker daemon must
/// not kill the loop.
pub async fn tick(state: &AppState) -> Vec<DiskEvent> {
    let specs = match state.db.list() {
        Ok(specs) => specs,
        Err(err) => {
            tracing::warn!(%err, "the disk quota tick cannot list the instances");
            return Vec::new();
        }
    };
    let quota_specs: Vec<InstanceSpec> = specs
        .into_iter()
        .filter(|spec| spec.disk_quota_mb > 0)
        .collect();
    if quota_specs.is_empty() {
        return Vec::new();
    }

    let samples = match state.driver.usage(&quota_specs).await {
        Ok(samples) => samples,
        Err(err) => {
            tracing::warn!(%err, "the disk quota tick cannot read the data directory sizes");
            return Vec::new();
        }
    };

    let mut events = Vec::new();
    for (index, spec) in quota_specs.iter().enumerate() {
        let sample = samples.get(index).cloned().unwrap_or_default();
        let level = assess(sample.disk_bytes, spec.disk_quota_mb);
        if !state.quota.observe(&spec.id, level) {
            continue;
        }
        match level {
            DiskLevel::Ok => {
                tracing::info!(
                    instance = %spec.id,
                    used_bytes = sample.disk_bytes,
                    quota_mb = spec.disk_quota_mb,
                    "disk usage is back below the quota"
                );
            }
            DiskLevel::Warn => {
                let detail = format!(
                    "data directory at {:.1}% of the {}-MB quota (warning threshold {}%)",
                    percent(sample.disk_bytes, spec.disk_quota_mb),
                    spec.disk_quota_mb,
                    DISK_WARN_PERCENT
                );
                let new = NewDiskEvent {
                    instance_id: spec.id.clone(),
                    action: ACTION_WARN.to_string(),
                    before_bytes: sample.disk_bytes,
                    after_bytes: sample.disk_bytes,
                    detail: Some(detail),
                };
                match record_event(&state.db, &new) {
                    Ok(event) => {
                        notify(&event);
                        events.push(event);
                    }
                    Err(err) => {
                        tracing::warn!(%err, instance = %spec.id, "cannot record a disk event")
                    }
                }
            }
            DiskLevel::Exceeded => {
                if let Some(event) = enforce(state, spec, &sample).await {
                    events.push(event);
                }
            }
        }
    }
    events
}

/// Apply the policy of an instance that reached 100 % of its quota.
async fn enforce(
    state: &AppState,
    spec: &InstanceSpec,
    sample: &crate::driver::UsageSample,
) -> Option<DiskEvent> {
    let used = sample.disk_bytes;
    let quota = spec.disk_quota_mb;
    let over = format!(
        "data directory at {:.1}% of the {}-MB quota",
        percent(used, quota),
        quota
    );
    let (action, detail) = match spec.disk_policy {
        DiskPolicy::Warn => (
            ACTION_WARN,
            format!("{over}; policy is \"warn\" (no action)"),
        ),
        DiskPolicy::Freeze => {
            let (detail, status, message) = match stop_instance(state, spec, sample).await {
                Ok(true) => (
                    format!("{over}; the instance was frozen (stopped)"),
                    Status::Stopped,
                    DISK_QUOTA_EXCEEDED_MESSAGE.to_string(),
                ),
                Ok(false) => (
                    format!("{over}; the instance was already stopped"),
                    Status::Stopped,
                    DISK_QUOTA_EXCEEDED_MESSAGE.to_string(),
                ),
                Err(err) => (
                    format!("{over}; freeze failed: {err}"),
                    Status::Error,
                    format!("{DISK_QUOTA_EXCEEDED_MESSAGE} (the stop failed: {err})"),
                ),
            };
            let _ = state.db.set_status(&spec.id, status, Some(&message));
            (ACTION_FREEZE, detail)
        }
        DiskPolicy::Stop => {
            let detail = match stop_instance(state, spec, sample).await {
                Ok(true) => format!("{over}; the instance was stopped"),
                Ok(false) => format!("{over}; the instance was already stopped"),
                Err(err) => format!("{over}; stop failed: {err}"),
            };
            let _ = state
                .db
                .set_status(&spec.id, Status::Stopped, spec.last_error.as_deref());
            (ACTION_STOP, detail)
        }
    };

    let new = NewDiskEvent {
        instance_id: spec.id.clone(),
        action: action.to_string(),
        before_bytes: used,
        after_bytes: used,
        detail: Some(detail.clone()),
    };
    match record_event(&state.db, &new) {
        Ok(event) => {
            notify(&event);
            state.metrics.invalidate_usage();
            Some(event)
        }
        Err(err) => {
            tracing::warn!(%err, instance = %spec.id, "cannot record a disk event");
            None
        }
    }
}

/// Stop one instance through the driver; `Ok(false)` when nothing was running.
async fn stop_instance(
    state: &AppState,
    spec: &InstanceSpec,
    sample: &crate::driver::UsageSample,
) -> Result<bool, String> {
    if !sample.running {
        return Ok(false);
    }
    state
        .driver
        .stop(spec)
        .await
        .map(|_| true)
        .map_err(|err| err.detail.trim().to_string())
}

/// Notification hook: every event is logged, and the journal stays queryable.
///
/// This build has no webhook/Telegram notifier of its own; a future notifier
/// (or `fleett disk events` in a cron) is expected to read `disk_events`.
pub(crate) fn notify(event: &DiskEvent) {
    match event.action.as_str() {
        ACTION_WARN => tracing::warn!(
            instance = %event.instance,
            used_bytes = event.before_bytes,
            "disk quota warning"
        ),
        ACTION_FREEZE => tracing::error!(
            instance = %event.instance,
            used_bytes = event.before_bytes,
            "disk quota exceeded: the instance was frozen"
        ),
        ACTION_STOP => tracing::error!(
            instance = %event.instance,
            used_bytes = event.before_bytes,
            "disk quota exceeded: the instance was stopped"
        ),
        ACTION_CLEAN => tracing::info!(
            instance = %event.instance,
            freed_bytes = event.before_bytes.saturating_sub(event.after_bytes),
            "disk cleanup"
        ),
        other => tracing::info!(instance = %event.instance, action = other, "disk event"),
    }
}

// ---------------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------------

/// Remove `tmp/`, `cache/` and rotated logs older than `log_age_days`.
///
/// Docker's own `json-file` logs are already capped by the compose file
/// (`max-size: 10m`, `max-file: 3`), so this only touches what the instance
/// wrote under its data directory. Symlinks are never followed, and the
/// *current* log file is never unlinked: removing a file Docker still has open
/// frees no space on Linux. Only clearly rotated names (`.log.1`, `.log.gz`,
/// `.log.2026-11-01`) qualify, which is what logrotate produces.
pub fn cleanup_data_dir(data_dir: &Path, log_age_days: u64) -> io::Result<CleanupReport> {
    let before_bytes = dir_breakdown(data_dir).total_bytes;
    let mut removed_files = 0usize;
    let mut removed_paths = Vec::new();
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(log_age_days.saturating_mul(24 * 3600)))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    for dir in ["tmp", "cache"] {
        let path = data_dir.join(dir);
        if path.exists() {
            removed_files += count_files(&path);
            remove_dir_all_if_present(&path)?;
            removed_paths.push(dir.to_string());
        }
    }

    collect_old_logs(
        data_dir,
        data_dir,
        cutoff,
        &mut removed_files,
        &mut removed_paths,
    )?;

    removed_paths.sort();
    removed_paths.dedup();
    let after_bytes = dir_breakdown(data_dir).total_bytes;
    Ok(CleanupReport {
        instance: String::new(),
        before_bytes,
        after_bytes,
        freed_bytes: before_bytes.saturating_sub(after_bytes),
        removed_files,
        removed_paths,
    })
}

fn remove_dir_all_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn count_files(path: &Path) -> usize {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.file_type().is_symlink() {
        return 1;
    }
    if metadata.is_file() {
        return 1;
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| count_files(&entry.path()))
                .sum()
        })
        .unwrap_or(0)
}

fn collect_old_logs(
    root: &Path,
    dir: &Path,
    cutoff: SystemTime,
    removed_files: &mut usize,
    removed_paths: &mut Vec<String>,
) -> io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_old_logs(root, &path, cutoff, removed_files, removed_paths)?;
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_rotated_log(&name) {
            continue;
        }
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if modified > cutoff {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            *removed_files += 1;
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            removed_paths.push(relative);
        }
    }
    Ok(())
}

/// `app.log.1`, `app.log.2026-11-01`, `app.log.gz`: rotated, safe to unlink.
///
/// A sole `app.log` is intentionally excluded: it may be the file the agent
/// still holds open.
fn is_rotated_log(name: &str) -> bool {
    name.contains(".log.") || name.ends_with(".log.gz") || name.ends_with(".log.zst")
}

/// Size of every file under `dir`, split by the directories cleanup targets.
pub fn dir_breakdown(dir: &Path) -> DirBreakdown {
    let mut breakdown = DirBreakdown::default();
    walk_breakdown(dir, dir, &mut breakdown);
    breakdown.total_bytes = breakdown
        .tmp_bytes
        .saturating_add(breakdown.cache_bytes)
        .saturating_add(breakdown.logs_bytes)
        .saturating_add(breakdown.other_bytes);
    breakdown
}

fn walk_breakdown(root: &Path, dir: &Path, breakdown: &mut DirBreakdown) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            walk_breakdown(root, &path, breakdown);
            continue;
        }
        let size = metadata.len();
        breakdown.file_count += 1;
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let first = relative
            .components()
            .next()
            .map(|component| component.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if first == "tmp" {
            breakdown.tmp_bytes = breakdown.tmp_bytes.saturating_add(size);
        } else if first == "cache" {
            breakdown.cache_bytes = breakdown.cache_bytes.saturating_add(size);
        } else if is_log_name(&name) {
            breakdown.logs_bytes = breakdown.logs_bytes.saturating_add(size);
        } else {
            breakdown.other_bytes = breakdown.other_bytes.saturating_add(size);
        }
    }
}

fn is_log_name(name: &str) -> bool {
    name.contains(".log")
}

/// Data directory of one instance (`<root>/instances/<id>/data`).
pub fn data_dir(cfg: &Config, id: &str) -> PathBuf {
    cfg.data_dir(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, vec![b'x'; bytes]).expect("write");
    }

    fn age(path: &Path, days: u64) {
        let file = fs::OpenOptions::new().write(true).open(path).expect("open");
        let when = SystemTime::now() - Duration::from_secs(days * 24 * 3600);
        file.set_modified(when).expect("set mtime");
    }

    #[test]
    fn an_unlimited_quota_is_never_classified() {
        assert_eq!(assess(u64::MAX, 0), DiskLevel::Ok);
        assert_eq!(percent(5, 0), 0.0);
    }

    #[test]
    fn thresholds_are_80_and_100_percent() {
        let mib = MIB;
        assert_eq!(assess(79 * mib, 100), DiskLevel::Ok);
        assert_eq!(assess(80 * mib, 100), DiskLevel::Warn);
        assert_eq!(assess(99 * mib, 100), DiskLevel::Warn);
        assert_eq!(assess(100 * mib, 100), DiskLevel::Exceeded);
        assert_eq!(assess(40 * mib, 50), DiskLevel::Warn, "80 % of 50 MB");
    }

    #[test]
    fn a_monitor_fires_once_per_crossing() {
        let monitor = QuotaMonitor::new();
        assert!(monitor.observe("alice", DiskLevel::Warn));
        assert!(!monitor.observe("alice", DiskLevel::Warn), "same level");
        assert!(monitor.observe("alice", DiskLevel::Exceeded));
        assert!(!monitor.observe("alice", DiskLevel::Exceeded));
        assert!(monitor.observe("alice", DiskLevel::Ok), "back to ok");
        assert!(monitor.observe("alice", DiskLevel::Warn), "new crossing");
        assert_eq!(monitor.level("alice"), Some(DiskLevel::Warn));
        monitor.forget("alice");
        assert_eq!(monitor.level("alice"), None);
    }

    #[test]
    fn the_capacity_guard_uses_the_largest_reserve() {
        assert_eq!(required_free_mb(2048, 0), 2048);
        assert_eq!(required_free_mb(2048, 4096), 8192, "twice the quota wins");
        assert_eq!(required_free_mb(0, 0), 0);

        // 3 GiB free: enough for the default floor, not for a 2-GiB quota.
        let free = 3 * 1024 * MIB;
        assert!(ensure_capacity(free, 0, 2048).is_ok());
        let err = ensure_capacity(free, 2048, 2048).expect_err("twice the quota");
        assert_eq!(
            err,
            InsufficientDisk(Capacity {
                free_mb: 3072,
                required_mb: 4096
            })
        );
        assert!(ensure_capacity(4 * 1024 * MIB, 2048, 2048).is_ok());
        assert!(ensure_capacity(0, 0, 0).is_ok(), "guard disabled");
    }

    #[test]
    fn only_rotated_logs_are_eligible() {
        assert!(is_rotated_log("app.log.1"));
        assert!(is_rotated_log("gateway.log.2026-11-01"));
        assert!(is_rotated_log("app.log.gz"));
        assert!(!is_rotated_log("app.log"), "an open log is never unlinked");
        assert!(!is_rotated_log("app.txt"));
    }

    #[test]
    fn breakdown_splits_tmp_cache_and_logs() {
        let dir = tempfile::tempdir().expect("dir");
        write(&dir.path().join("MEMORY.md"), 10);
        write(&dir.path().join("tmp/scratch.bin"), 20);
        write(&dir.path().join("cache/http.bin"), 30);
        write(&dir.path().join("logs/agent.log"), 40);
        let breakdown = dir_breakdown(dir.path());
        assert_eq!(breakdown.total_bytes, 100);
        assert_eq!(breakdown.tmp_bytes, 20);
        assert_eq!(breakdown.cache_bytes, 30);
        assert_eq!(breakdown.logs_bytes, 40);
        assert_eq!(breakdown.other_bytes, 10);
        assert_eq!(breakdown.file_count, 4);
    }

    #[test]
    fn cleanup_removes_tmp_cache_and_old_rotated_logs_only() {
        let dir = tempfile::tempdir().expect("dir");
        write(&dir.path().join("MEMORY.md"), 1024);
        write(&dir.path().join("tmp/scratch.bin"), 40 * 1024);
        write(&dir.path().join("cache/http.bin"), 60 * 1024);
        write(&dir.path().join("logs/agent.log"), 10 * 1024);
        write(&dir.path().join("logs/agent.log.1"), 30 * 1024);
        age(&dir.path().join("logs/agent.log.1"), 30);
        write(&dir.path().join("logs/agent.log.2"), 10 * 1024);
        let report = cleanup_data_dir(dir.path(), CLEANUP_LOG_AGE_DAYS).expect("cleanup");

        assert!(report.before_bytes >= 100 * 1024);
        assert!(report.after_bytes < 30 * 1024, "{}", report.after_bytes);
        assert_eq!(report.freed_bytes, report.before_bytes - report.after_bytes);
        assert!(report.removed_paths.contains(&"tmp".to_string()));
        assert!(report.removed_paths.contains(&"cache".to_string()));
        assert!(
            report
                .removed_paths
                .contains(&"logs/agent.log.1".to_string()),
            "{:?}",
            report.removed_paths
        );
        assert!(
            dir.path().join("logs/agent.log").is_file(),
            "current log kept"
        );
        assert!(
            dir.path().join("logs/agent.log.2").is_file(),
            "recent log kept"
        );
        assert!(dir.path().join("MEMORY.md").is_file());
    }

    #[test]
    fn cleanup_of_a_missing_directory_is_a_noop() {
        let dir = tempfile::tempdir().expect("dir");
        let report = cleanup_data_dir(&dir.path().join("absent"), CLEANUP_LOG_AGE_DAYS)
            .expect("missing dir");
        assert_eq!(report, CleanupReport::default());
    }
}
