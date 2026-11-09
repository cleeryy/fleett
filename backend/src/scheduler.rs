//! Per-agent cron schedules (issue #30).
//!
//! A schedule belongs to one instance and holds a five-field cron expression, a
//! prompt and an execution policy. A background tick (`main.rs`, every 15 s)
//! finds the enabled schedules whose `next_run_at` has passed and runs the
//! prompt inside the instance container through [`crate::driver::Driver::exec`].
//! Every attempt, successful or not, leaves a row in `schedule_runs`.
//!
//! Cron semantics are the classic five fields (`minute hour day-of-month month
//! day-of-week`) in **UTC only**. When both day-of-month and day-of-week are
//! restricted, a day matching either one fires (Vixie cron semantics). The
//! `timezone` column is accepted and stored today, but every evaluation is UTC
//! until a timezone database is added.
//!
//! Length of run history is unbounded for now; the API only pages it.

use crate::api::AppState;
use crate::db::{Db, DbError};
use chrono::{DateTime, Datelike, Timelike, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Timeout used when the request does not set one.
pub const DEFAULT_TIMEOUT_S: u64 = 300;
/// Smallest accepted timeout.
pub const MIN_TIMEOUT_S: u64 = 1;
/// Largest accepted timeout.
pub const MAX_TIMEOUT_S: u64 = 3600;
/// Largest accepted prompt.
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
/// Longest accepted schedule name, in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// Longest accepted timezone identifier (stored, not yet interpreted).
pub const MAX_TIMEZONE_CHARS: usize = 64;
/// Output is truncated to this many bytes in the run history.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Runs returned when `?limit=` is omitted.
pub const DEFAULT_RUN_LIMIT: usize = 50;
/// Largest run page.
pub const MAX_RUN_LIMIT: usize = 500;
/// Period of the background loop, in seconds.
pub const TICK_INTERVAL_S: u64 = 15;
/// Environment variable holding the argv prefix of a scheduled command.
pub const RUN_COMMAND_ENV: &str = "FLEETT_RUN_COMMAND";
/// Default command run inside the container; the prompt is the last argument.
pub const DEFAULT_RUN_COMMAND: &str = "gateway ask";

/// How a run is started when the previous one is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Overlap {
    /// Skip the occurrence and record a `skipped` run.
    Skip,
    /// Leave the occurrence due and run it once the previous run finishes.
    Queue,
}

impl Overlap {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Queue => "queue",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "skip" => Some(Self::Skip),
            "queue" => Some(Self::Queue),
            _ => None,
        }
    }
}

/// State of one scheduled run.
///
/// `running` is internal: it is written before the command starts so the
/// overlap policy can see it, and a row left in that state by a crash is
/// turned into `error` by [`recover`] at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Ok,
    Error,
    Timeout,
    Skipped,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Timeout => "timeout",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "running" => Some(Self::Running),
            "ok" => Some(Self::Ok),
            "error" => Some(Self::Error),
            "timeout" => Some(Self::Timeout),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One stored schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Schedule {
    pub id: String,
    pub instance_id: String,
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub enabled: bool,
    pub timezone: String,
    pub overlap: Overlap,
    /// Execution budget, in seconds (`timeout` in the API payload).
    #[serde(rename = "timeout")]
    pub timeout_s: u64,
    pub catchup: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A schedule about to be inserted.
#[derive(Debug, Clone)]
pub struct NewSchedule {
    pub instance_id: String,
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub enabled: bool,
    pub timezone: String,
    pub overlap: Overlap,
    pub timeout_s: u64,
    /// Replay one missed occurrence at startup when `true` (default `false`).
    pub catchup: bool,
}

impl Default for NewSchedule {
    fn default() -> Self {
        Self {
            instance_id: String::new(),
            name: String::new(),
            cron: String::new(),
            prompt: String::new(),
            enabled: true,
            timezone: "UTC".to_string(),
            overlap: Overlap::Skip,
            timeout_s: DEFAULT_TIMEOUT_S,
            catchup: false,
        }
    }
}

/// Partial update of a schedule; `None` keeps the stored value.
#[derive(Debug, Clone, Default)]
pub struct SchedulePatch {
    pub name: Option<String>,
    pub cron: Option<String>,
    pub prompt: Option<String>,
    pub enabled: Option<bool>,
    pub timezone: Option<String>,
    pub overlap: Option<Overlap>,
    pub timeout_s: Option<u64>,
    pub catchup: Option<bool>,
}

/// One recorded run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScheduleRun {
    pub id: String,
    pub schedule_id: String,
    pub instance_id: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub status: RunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    pub created_at: String,
}

/// One schedule with its latest run and all-time failure statistics.
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleView {
    #[serde(flatten)]
    pub schedule: Schedule,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<ScheduleRun>,
    pub runs_total: u64,
    pub runs_failed: u64,
    /// `runs_failed / runs_total`, `0.0` when the schedule never ran.
    pub failure_rate: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("unknown instance \"{0}\"")]
    UnknownInstance(String),
    #[error("unknown schedule \"{0}\"")]
    NotFound(String),
    #[error("a schedule named \"{0}\" already exists for this instance")]
    NameExists(String),
    #[error("invalid cron expression: {0}")]
    InvalidCron(String),
    #[error("invalid schedule name: {0}")]
    InvalidName(String),
    #[error("invalid prompt: {0}")]
    InvalidPrompt(String),
    #[error("invalid timezone: {0}")]
    InvalidTimezone(String),
    #[error("invalid overlap policy \"{0}\": expected skip or queue")]
    InvalidOverlap(String),
    #[error("timeout must be between {MIN_TIMEOUT_S} and {MAX_TIMEOUT_S} seconds")]
    InvalidTimeout,
    #[error("a run is already in progress for this schedule")]
    RunInProgress,
    #[error("database error: {0}")]
    Db(#[from] DbError),
}

// ---------------------------------------------------------------------------
// Cron parser
// ---------------------------------------------------------------------------

/// Minutes searched by [`CronSchedule::next_after`] before giving up
/// (a little over five years, enough to find any leap-day expression).
const MAX_SEARCH_MINUTES: u64 = 5 * 366 * 24 * 60;

/// A parsed five-field cron expression, UTC only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSchedule {
    minutes: u64,
    hours: u32,
    days_of_month: u32,
    months: u32,
    days_of_week: u8,
    day_of_month_any: bool,
    day_of_week_any: bool,
}

impl CronSchedule {
    /// Parse `minute hour day-of-month month day-of-week`.
    ///
    /// Supported syntax: `*`, `*/n`, lists (`a,b`), ranges (`a-b`) and stepped
    /// ranges (`a-b/n`). Day-of-week accepts `0` and `7` for Sunday. Names
    /// (`MON`, `JAN`) and `@macros` are refused.
    pub fn parse(expression: &str) -> Result<Self, String> {
        let fields: Vec<&str> = expression.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "expected 5 fields (minute hour day-of-month month day-of-week), found {}",
                fields.len()
            ));
        }
        let minutes = parse_field(fields[0], "minute", 0, 59, false)?;
        let hours = parse_field(fields[1], "hour", 0, 23, false)? as u32;
        let days_of_month = parse_field(fields[2], "day-of-month", 1, 31, false)? as u32;
        let months = parse_field(fields[3], "month", 1, 12, false)? as u32;
        let days_of_week = parse_field(fields[4], "day-of-week", 0, 7, true)? as u8;
        let minute_any = minutes == bit_range(0, 59);
        let hour_any = hours == bit_range(0, 23) as u32;
        let dom_any = days_of_month == bit_range(1, 31) as u32;
        let month_any = months == bit_range(1, 12) as u32;
        let dow_any = days_of_week == bit_range(0, 6) as u8;
        // `* * * * *` has no restriction at all; a fully-expanded equivalent
        // (`0-59 0-23 ...`) behaves the same, which is why the masks are
        // compared instead of remembering whether the field was written `*`.
        let unrestricted = minute_any && hour_any && dom_any && month_any && dow_any;
        Ok(Self {
            minutes,
            hours,
            days_of_month,
            months,
            days_of_week,
            day_of_month_any: dom_any || unrestricted,
            day_of_week_any: dow_any || unrestricted,
        })
    }

    /// Whether `at` matches, to the minute.
    pub fn matches(&self, at: DateTime<Utc>) -> bool {
        if self.minutes & (1u64 << at.minute()) == 0 {
            return false;
        }
        if self.hours & (1u32 << at.hour()) == 0 {
            return false;
        }
        if self.months & (1u32 << at.month()) == 0 {
            return false;
        }
        let dom = self.days_of_month & (1u32 << at.day()) != 0;
        let dow = self.days_of_week & (1u8 << at.weekday().num_days_from_sunday()) != 0;
        if self.day_of_month_any && self.day_of_week_any {
            return true;
        }
        if self.day_of_month_any {
            return dow;
        }
        if self.day_of_week_any {
            return dom;
        }
        // Classic cron: two restricted day fields are ORed.
        dom || dow
    }

    /// First matching minute strictly after `after`, or `None` when the
    /// expression can never fire (for example `0 0 31 2 *`).
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let mut timestamp = (after.timestamp().div_euclid(60) + 1) * 60;
        for _ in 0..MAX_SEARCH_MINUTES {
            let candidate = DateTime::<Utc>::from_timestamp(timestamp, 0)?;
            if self.matches(candidate) {
                return Some(candidate);
            }
            timestamp += 60;
        }
        None
    }
}

/// Bitmask with every bit of `min..=max` set (values are below 64).
fn bit_range(min: u32, max: u32) -> u64 {
    let mut mask = 0u64;
    let mut value = min;
    while value <= max {
        mask |= 1u64 << value;
        value += 1;
    }
    mask
}

fn parse_field(
    field: &str,
    what: &str,
    min: u32,
    max: u32,
    sunday_is_seven: bool,
) -> Result<u64, String> {
    let mut mask = 0u64;
    for item in field.split(',') {
        if item.is_empty() {
            return Err(format!("{what}: empty list item"));
        }
        let (base, step) = match item.split_once('/') {
            Some((base, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("{what}: invalid step \"{step}\""))?;
                if step == 0 {
                    return Err(format!("{what}: step must be at least 1"));
                }
                (base, step)
            }
            None => (item, 1),
        };
        let (start, end) = if base == "*" {
            (min, max)
        } else if let Some((left, right)) = base.split_once('-') {
            (
                parse_value(left, what, min, max)?,
                parse_value(right, what, min, max)?,
            )
        } else {
            let value = parse_value(base, what, min, max)?;
            // `5/10` means "from 5 to the maximum, every 10".
            if item.contains('/') {
                (value, max)
            } else {
                (value, value)
            }
        };
        if start > end {
            return Err(format!(
                "{what}: range start {start} is after its end {end}"
            ));
        }
        let mut value = start;
        loop {
            let normalized = if sunday_is_seven && value == 7 {
                0
            } else {
                value
            };
            mask |= 1u64 << normalized;
            value = match value.checked_add(step) {
                Some(next) if next <= end => next,
                _ => break,
            };
        }
    }
    Ok(mask)
}

fn parse_value(value: &str, what: &str, min: u32, max: u32) -> Result<u32, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{what}: invalid value \"{value}\""));
    }
    let parsed: u32 = value
        .parse()
        .map_err(|_| format!("{what}: invalid value \"{value}\""))?;
    if !(min..=max).contains(&parsed) {
        return Err(format!("{what}: value {parsed} is outside {min}..={max}"));
    }
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// Validation and helpers
// ---------------------------------------------------------------------------

pub fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

pub fn validate_name(value: &str) -> Result<String, SchedulerError> {
    let name = value.trim();
    if name.is_empty() {
        return Err(SchedulerError::InvalidName("must not be empty".to_string()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(SchedulerError::InvalidName(format!(
            "must be at most {MAX_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(SchedulerError::InvalidName(
            "must not contain control characters".to_string(),
        ));
    }
    Ok(name.to_string())
}

pub fn validate_prompt(value: &str) -> Result<String, SchedulerError> {
    let prompt = value.trim();
    if prompt.is_empty() {
        return Err(SchedulerError::InvalidPrompt(
            "must not be empty".to_string(),
        ));
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(SchedulerError::InvalidPrompt(format!(
            "must be at most {} KiB",
            MAX_PROMPT_BYTES / 1024
        )));
    }
    Ok(prompt.to_string())
}

pub fn validate_timezone(value: &str) -> Result<String, SchedulerError> {
    let timezone = value.trim();
    if timezone.is_empty() {
        return Err(SchedulerError::InvalidTimezone(
            "must not be empty".to_string(),
        ));
    }
    if timezone.chars().count() > MAX_TIMEZONE_CHARS || timezone.chars().any(char::is_control) {
        return Err(SchedulerError::InvalidTimezone(format!(
            "must be at most {MAX_TIMEZONE_CHARS} characters without control characters"
        )));
    }
    Ok(timezone.to_string())
}

pub fn validate_timeout(timeout_s: u64) -> Result<(), SchedulerError> {
    if !(MIN_TIMEOUT_S..=MAX_TIMEOUT_S).contains(&timeout_s) {
        return Err(SchedulerError::InvalidTimeout);
    }
    Ok(())
}

/// Argv of one scheduled run: the `FLEETT_RUN_COMMAND` prefix (whitespace
/// separated, `gateway ask` by default) plus the prompt as the final argument.
pub fn run_argv(prompt: &str) -> Vec<String> {
    let prefix = std::env::var(RUN_COMMAND_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RUN_COMMAND.to_string());
    let mut argv: Vec<String> = prefix.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        argv = DEFAULT_RUN_COMMAND
            .split_whitespace()
            .map(str::to_string)
            .collect();
    }
    argv.push(prompt.to_string());
    argv
}

fn combined_output(stdout: &str, stderr: &str) -> String {
    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str("[stderr]\n");
        output.push_str(stderr);
    }
    truncate_output(&output)
}

/// Truncate on a character boundary, marking the cut.
fn truncate_output(text: &str) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text.to_string();
    }
    let mut cut = MAX_OUTPUT_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut output = text[..cut].to_string();
    output.push_str("…(truncated)");
    output
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

const SCHEDULE_COLUMNS: &str = "id, instance_id, name, cron, prompt, enabled, timezone, overlap, \
                                timeout_s, catchup, last_run_at, next_run_at, created_at, updated_at";

const RUN_COLUMNS: &str = "id, schedule_id, instance_id, started_at, finished_at, status, \
                           exit_code, output, duration_ms, created_at";

fn row_to_schedule(row: &rusqlite::Row<'_>) -> rusqlite::Result<Schedule> {
    let overlap: String = row.get(7)?;
    Ok(Schedule {
        id: row.get(0)?,
        instance_id: row.get(1)?,
        name: row.get(2)?,
        cron: row.get(3)?,
        prompt: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        timezone: row.get(6)?,
        overlap: Overlap::parse(&overlap).unwrap_or(Overlap::Skip),
        timeout_s: row.get::<_, i64>(8)?.max(0) as u64,
        catchup: row.get::<_, i64>(9)? != 0,
        last_run_at: row.get(10)?,
        next_run_at: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleRun> {
    let status: String = row.get(5)?;
    Ok(ScheduleRun {
        id: row.get(0)?,
        schedule_id: row.get(1)?,
        instance_id: row.get(2)?,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        status: RunStatus::parse(&status).unwrap_or(RunStatus::Error),
        exit_code: row.get(6)?,
        output: row.get(7)?,
        duration_ms: row.get(8)?,
        created_at: row.get(9)?,
    })
}

/// Persist one schedule, refusing a name already used by the same instance.
pub fn create(db: &Db, new: NewSchedule) -> Result<Schedule, SchedulerError> {
    if !db.exists(&new.instance_id)? {
        return Err(SchedulerError::UnknownInstance(new.instance_id));
    }
    let name = validate_name(&new.name)?;
    let prompt = validate_prompt(&new.prompt)?;
    let timezone = validate_timezone(&new.timezone)?;
    validate_timeout(new.timeout_s)?;
    let cron = new.cron.trim().to_string();
    let parsed = CronSchedule::parse(&cron).map_err(SchedulerError::InvalidCron)?;
    let now = Utc::now();
    let next_run_at = parsed
        .next_after(now)
        .ok_or_else(|| SchedulerError::InvalidCron("the expression never fires".to_string()))?;
    let duplicate = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT 1 FROM schedules WHERE instance_id = ?1 AND name = ?2",
                params![new.instance_id, name],
                |_| Ok(()),
            )
            .optional()
        })?
        .is_some();
    if duplicate {
        return Err(SchedulerError::NameExists(name));
    }
    let schedule = Schedule {
        id: Uuid::new_v4().to_string(),
        instance_id: new.instance_id.clone(),
        name: name.clone(),
        cron,
        prompt,
        enabled: new.enabled,
        timezone,
        overlap: new.overlap,
        timeout_s: new.timeout_s,
        catchup: new.catchup,
        last_run_at: None,
        next_run_at: new.enabled.then(|| iso(next_run_at)),
        created_at: iso(now),
        updated_at: iso(now),
    };
    db.with_conn(|conn| {
        conn.execute(
            &format!("INSERT INTO schedules ({SCHEDULE_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"),
            params![
                schedule.id,
                schedule.instance_id,
                schedule.name,
                schedule.cron,
                schedule.prompt,
                i64::from(schedule.enabled),
                schedule.timezone,
                schedule.overlap.as_str(),
                schedule.timeout_s as i64,
                i64::from(schedule.catchup),
                schedule.last_run_at,
                schedule.next_run_at,
                schedule.created_at,
                schedule.updated_at,
            ],
        )
    })?;
    Ok(schedule)
}

pub fn get(db: &Db, id: &str) -> Result<Option<Schedule>, SchedulerError> {
    let schedule = db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {SCHEDULE_COLUMNS} FROM schedules WHERE id = ?1"),
            params![id],
            row_to_schedule,
        )
        .optional()
    })?;
    Ok(schedule)
}

/// Schedules of one instance, ordered by name.
pub fn list_for_instance(db: &Db, instance_id: &str) -> Result<Vec<Schedule>, SchedulerError> {
    let schedules = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules WHERE instance_id = ?1 ORDER BY name, id"
        ))?;
        let rows = statement.query_map(params![instance_id], row_to_schedule)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    Ok(schedules)
}

/// Partial update. The next run is recomputed when the cron expression or the
/// enabled flag changes, so a disabled schedule stores `next_run_at = NULL`.
pub fn update(db: &Db, id: &str, patch: SchedulePatch) -> Result<Schedule, SchedulerError> {
    let mut schedule = get(db, id)?.ok_or_else(|| SchedulerError::NotFound(id.to_string()))?;
    let was_enabled = schedule.enabled;
    let cron_changed = patch.cron.is_some();
    if let Some(name) = patch.name {
        schedule.name = validate_name(&name)?;
    }
    if let Some(cron) = patch.cron {
        schedule.cron = cron.trim().to_string();
    }
    if let Some(prompt) = patch.prompt {
        schedule.prompt = validate_prompt(&prompt)?;
    }
    if let Some(enabled) = patch.enabled {
        schedule.enabled = enabled;
    }
    if let Some(timezone) = patch.timezone {
        schedule.timezone = validate_timezone(&timezone)?;
    }
    if let Some(overlap) = patch.overlap {
        schedule.overlap = overlap;
    }
    if let Some(timeout_s) = patch.timeout_s {
        validate_timeout(timeout_s)?;
        schedule.timeout_s = timeout_s;
    }
    if let Some(catchup) = patch.catchup {
        schedule.catchup = catchup;
    }

    let parsed = CronSchedule::parse(&schedule.cron).map_err(SchedulerError::InvalidCron)?;
    let now = Utc::now();
    let next = parsed
        .next_after(now)
        .ok_or_else(|| SchedulerError::InvalidCron("the expression never fires".to_string()))?;
    if !schedule.enabled {
        schedule.next_run_at = None;
    } else if cron_changed || !was_enabled || schedule.next_run_at.is_none() {
        schedule.next_run_at = Some(iso(next));
    }

    let duplicate = db.with_conn(|conn| {
        conn.query_row(
            "SELECT 1 FROM schedules WHERE instance_id = ?1 AND name = ?2 AND id != ?3",
            params![schedule.instance_id, schedule.name, schedule.id],
            |_| Ok(()),
        )
        .optional()
    })?;
    if duplicate.is_some() {
        return Err(SchedulerError::NameExists(schedule.name));
    }

    let now_text = iso(now);
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE schedules SET name = ?2, cron = ?3, prompt = ?4, enabled = ?5, \
             timezone = ?6, overlap = ?7, timeout_s = ?8, catchup = ?9, next_run_at = ?10, \
             updated_at = ?11 WHERE id = ?1",
            params![
                schedule.id,
                schedule.name,
                schedule.cron,
                schedule.prompt,
                i64::from(schedule.enabled),
                schedule.timezone,
                schedule.overlap.as_str(),
                schedule.timeout_s as i64,
                i64::from(schedule.catchup),
                schedule.next_run_at,
                now_text,
            ],
        )
    })?;
    schedule.updated_at = now_text;
    Ok(schedule)
}

/// Delete a schedule and its history; `false` when the id is unknown.
pub fn delete(db: &Db, id: &str) -> Result<bool, SchedulerError> {
    let deleted = db.with_transaction(|conn| {
        conn.execute(
            "DELETE FROM schedule_runs WHERE schedule_id = ?1",
            params![id],
        )?;
        conn.execute("DELETE FROM schedules WHERE id = ?1", params![id])
    })?;
    Ok(deleted > 0)
}

/// Most recent runs of one instance, newest first.
pub fn list_runs(
    db: &Db,
    instance_id: &str,
    limit: usize,
) -> Result<Vec<ScheduleRun>, SchedulerError> {
    let limit = limit.clamp(1, MAX_RUN_LIMIT);
    let runs = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM schedule_runs WHERE instance_id = ?1 \
             ORDER BY started_at DESC, rowid DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![instance_id, limit as i64], row_to_run)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    Ok(runs)
}

/// Most recent runs of one schedule, newest first.
pub fn list_runs_for_schedule(
    db: &Db,
    schedule_id: &str,
    limit: usize,
) -> Result<Vec<ScheduleRun>, SchedulerError> {
    let limit = limit.clamp(1, MAX_RUN_LIMIT);
    let runs = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM schedule_runs WHERE schedule_id = ?1 \
             ORDER BY started_at DESC, rowid DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![schedule_id, limit as i64], row_to_run)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    Ok(runs)
}

/// Every schedule with its latest run and its all-time failure rate.
pub fn list_all(db: &Db) -> Result<Vec<ScheduleView>, SchedulerError> {
    let schedules = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules ORDER BY instance_id, name, id"
        ))?;
        let rows = statement.query_map([], row_to_schedule)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;

    let mut last_runs: HashMap<String, ScheduleRun> = HashMap::new();
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM schedule_runs ORDER BY started_at DESC, rowid DESC"
        ))?;
        let rows = statement.query_map([], row_to_run)?;
        for run in rows {
            let run = run?;
            last_runs.entry(run.schedule_id.clone()).or_insert(run);
        }
        Ok(())
    })?;

    let mut stats: HashMap<String, (u64, u64)> = HashMap::new();
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT schedule_id, COUNT(*), \
                    SUM(CASE WHEN status IN ('error', 'timeout') THEN 1 ELSE 0 END) \
             FROM schedule_runs WHERE status != 'running' GROUP BY schedule_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as u64,
                row.get::<_, i64>(2)?.max(0) as u64,
            ))
        })?;
        for row in rows {
            let (schedule_id, total, failed) = row?;
            stats.insert(schedule_id, (total, failed));
        }
        Ok(())
    })?;

    Ok(schedules
        .into_iter()
        .map(|schedule| {
            let (runs_total, runs_failed) = stats.get(&schedule.id).copied().unwrap_or((0, 0));
            let failure_rate = if runs_total == 0 {
                0.0
            } else {
                (runs_failed as f64 / runs_total as f64 * 10_000.0).round() / 10_000.0
            };
            ScheduleView {
                last_run: last_runs.remove(&schedule.id),
                schedule,
                runs_total,
                runs_failed,
                failure_rate,
            }
        })
        .collect())
}

/// Enabled schedules whose next run is due.
pub fn due_schedules(db: &Db, now: DateTime<Utc>) -> Result<Vec<Schedule>, SchedulerError> {
    let due = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules \
             WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1 \
             ORDER BY next_run_at, id"
        ))?;
        let rows = statement.query_map(params![iso(now)], row_to_schedule)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    Ok(due)
}

/// The running run of one schedule, when there is one.
pub fn running_run(db: &Db, schedule_id: &str) -> Result<Option<ScheduleRun>, SchedulerError> {
    let run = db.with_conn(|conn| {
        conn.query_row(
            &format!(
                "SELECT {RUN_COLUMNS} FROM schedule_runs \
                 WHERE schedule_id = ?1 AND status = 'running' \
                 ORDER BY started_at DESC, rowid DESC LIMIT 1"
            ),
            params![schedule_id],
            row_to_run,
        )
        .optional()
    })?;
    Ok(run)
}

fn insert_running_run(
    db: &Db,
    schedule: &Schedule,
    run_id: &str,
    started_at: &str,
    next_run_at: Option<&str>,
) -> Result<(), SchedulerError> {
    db.with_transaction(|conn| {
        conn.execute(
            "INSERT INTO schedule_runs (id, schedule_id, instance_id, started_at, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, 'running', ?4)",
            params![run_id, schedule.id, schedule.instance_id, started_at],
        )?;
        conn.execute(
            "UPDATE schedules SET last_run_at = ?2, next_run_at = ?3, updated_at = ?4 \
             WHERE id = ?1",
            params![schedule.id, started_at, next_run_at, started_at],
        )?;
        Ok(())
    })?;
    Ok(())
}

fn finish_run(
    db: &Db,
    run_id: &str,
    status: RunStatus,
    exit_code: Option<i32>,
    output: &str,
    duration_ms: i64,
    finished_at: &str,
) -> Result<(), SchedulerError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE schedule_runs SET finished_at = ?2, status = ?3, exit_code = ?4, \
             output = ?5, duration_ms = ?6 WHERE id = ?1",
            params![
                run_id,
                finished_at,
                status.as_str(),
                exit_code,
                output,
                duration_ms,
            ],
        )
    })?;
    Ok(())
}

/// Record a `skipped` run and move the schedule past this occurrence.
fn record_skipped(
    db: &Db,
    schedule: &Schedule,
    at: DateTime<Utc>,
) -> Result<ScheduleRun, SchedulerError> {
    let started_at = iso(at);
    let next = if schedule.enabled {
        CronSchedule::parse(&schedule.cron)
            .ok()
            .and_then(|parsed| parsed.next_after(at))
            .map(iso)
    } else {
        None
    };
    let run = ScheduleRun {
        id: Uuid::new_v4().to_string(),
        schedule_id: schedule.id.clone(),
        instance_id: schedule.instance_id.clone(),
        started_at: started_at.clone(),
        finished_at: Some(started_at.clone()),
        status: RunStatus::Skipped,
        exit_code: None,
        output: Some("skipped: the previous run is still in progress".to_string()),
        duration_ms: Some(0),
        created_at: started_at.clone(),
    };
    db.with_transaction(|conn| {
        conn.execute(
            "INSERT INTO schedule_runs \
             (id, schedule_id, instance_id, started_at, finished_at, status, output, duration_ms, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'skipped', ?6, 0, ?5)",
            params![
                run.id,
                run.schedule_id,
                run.instance_id,
                run.started_at,
                run.finished_at,
                run.output,
            ],
        )?;
        conn.execute(
            "UPDATE schedules SET last_run_at = ?2, next_run_at = ?3, updated_at = ?2 \
             WHERE id = ?1",
            params![schedule.id, started_at, next],
        )?;
        Ok(())
    })?;
    Ok(run)
}

/// Execute one schedule now, recording the run and its output.
///
/// `last_run_at` and `next_run_at` are stamped **before** the command starts,
/// so a concurrent tick sees the overlap and a long run never causes a burst of
/// catch-up executions.
pub async fn execute(state: &AppState, schedule: &Schedule) -> Result<ScheduleRun, SchedulerError> {
    let started = Utc::now();
    let started_at = iso(started);
    let next_run_at = if schedule.enabled {
        CronSchedule::parse(&schedule.cron)
            .map_err(SchedulerError::InvalidCron)?
            .next_after(started)
            .map(iso)
    } else {
        None
    };
    let run_id = Uuid::new_v4().to_string();
    insert_running_run(
        &state.db,
        schedule,
        &run_id,
        &started_at,
        next_run_at.as_deref(),
    )?;

    let argv = run_argv(&schedule.prompt);
    let result = match state.db.get(&schedule.instance_id) {
        Ok(Some(spec)) => state.driver.exec(&spec, &argv, schedule.timeout_s).await,
        Ok(None) => Err(crate::driver::DriverError::new(
            "exec",
            format!("unknown instance \"{}\"", schedule.instance_id),
        )),
        Err(err) => Err(crate::driver::DriverError::new(
            "exec",
            format!("database error: {err}"),
        )),
    };

    let (status, exit_code, output) = match result {
        Ok(executed) => {
            let status = if executed.timed_out {
                RunStatus::Timeout
            } else if executed.exit_code == Some(0) {
                RunStatus::Ok
            } else {
                RunStatus::Error
            };
            (
                status,
                executed.exit_code,
                combined_output(&executed.stdout, &executed.stderr),
            )
        }
        Err(err) => (RunStatus::Error, None, err.detail),
    };

    let finished = Utc::now();
    let duration_ms = (finished - started).num_milliseconds().max(0);
    finish_run(
        &state.db,
        &run_id,
        status,
        exit_code,
        &output,
        duration_ms,
        &iso(finished),
    )?;

    Ok(ScheduleRun {
        id: run_id,
        schedule_id: schedule.id.clone(),
        instance_id: schedule.instance_id.clone(),
        started_at,
        finished_at: Some(iso(finished)),
        status,
        exit_code,
        output: Some(output),
        duration_ms: Some(duration_ms),
        created_at: iso(started),
    })
}

/// Manual trigger: refuse when the schedule is already running, then execute
/// it without touching `next_run_at` beyond the usual advance.
pub async fn run_now(state: &AppState, schedule_id: &str) -> Result<ScheduleRun, SchedulerError> {
    let schedule = get(&state.db, schedule_id)?
        .ok_or_else(|| SchedulerError::NotFound(schedule_id.to_string()))?;
    if running_run(&state.db, &schedule.id)?.is_some() {
        return Err(SchedulerError::RunInProgress);
    }
    execute(state, &schedule).await
}

/// One background iteration: run every due schedule that is not blocked.
///
/// Overlap `skip` records a `skipped` run and advances; `queue` simply leaves
/// the occurrence due, so the next tick starts it once the running one ends.
pub async fn tick(state: &AppState) {
    let now = Utc::now();
    let due = match due_schedules(&state.db, now) {
        Ok(due) => due,
        Err(err) => {
            tracing::warn!(%err, "scheduler: cannot read due schedules");
            return;
        }
    };
    for schedule in due {
        if !schedule.enabled {
            continue;
        }
        if running_run(&state.db, &schedule.id)
            .ok()
            .flatten()
            .is_some()
        {
            match schedule.overlap {
                Overlap::Skip => {
                    if let Err(err) = record_skipped(&state.db, &schedule, Utc::now()) {
                        tracing::warn!(schedule = %schedule.id, %err, "scheduler: cannot record a skipped run");
                    }
                }
                Overlap::Queue => continue,
            }
            continue;
        }
        if let Err(err) = execute(state, &schedule).await {
            tracing::warn!(schedule = %schedule.id, %err, "scheduler: run failed");
        }
    }
}

/// Startup recovery.
///
/// 1. A `running` row belongs to a process that died: it becomes `error`.
/// 2. With `catchup: false` (the default), a schedule whose `next_run_at` is
///    in the past is moved to its next future occurrence instead of replaying
///    the missed slot. `catchup: true` leaves it due so exactly one catch-up
///    run fires immediately.
///
/// Returns the number of stale runs recovered.
pub fn recover(db: &Db) -> Result<usize, SchedulerError> {
    let now = Utc::now();
    let now_text = iso(now);
    let stale = db.with_conn(|conn| {
        conn.execute(
            "UPDATE schedule_runs SET status = 'error', finished_at = ?1, \
             output = COALESCE(output, '') || CASE WHEN COALESCE(output, '') = '' THEN '' ELSE char(10) END \
             || 'interrupted by a control-plane restart' \
             WHERE status = 'running'",
            params![now_text],
        )
    })?;

    for schedule in list_all(db)?.into_iter().map(|view| view.schedule) {
        if !schedule.enabled || schedule.catchup {
            continue;
        }
        let due = match schedule.next_run_at.as_deref() {
            Some(value) => parse_timestamp(value).map(|at| at <= now).unwrap_or(true),
            None => true,
        };
        if !due {
            continue;
        }
        let Some(next) = CronSchedule::parse(&schedule.cron)
            .ok()
            .and_then(|parsed| parsed.next_after(now))
        else {
            continue;
        };
        if let Err(err) = db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules SET next_run_at = ?2, updated_at = ?3 WHERE id = ?1",
                params![schedule.id, iso(next), now_text],
            )
        }) {
            tracing::warn!(schedule = %schedule.id, %err, "scheduler: cannot advance a missed schedule");
        }
    }
    Ok(stale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::driver::mock::MockDriver;
    use crate::driver::ExecResult;
    use crate::models::{
        ChannelKind, InstanceSpec, Status, DEFAULT_CPU_LIMIT, DEFAULT_MEM_LIMIT_MB,
    };
    use std::sync::Arc;

    fn at(value: &str) -> DateTime<Utc> {
        parse_timestamp(value).expect("valid timestamp")
    }

    fn cron(expression: &str) -> CronSchedule {
        CronSchedule::parse(expression).expect("valid cron")
    }

    fn spec(id: &str) -> InstanceSpec {
        InstanceSpec {
            id: id.to_string(),
            name: id.to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: "HERMES_GATE_PROFILE_TOKEN".to_string(),
            provider: None,
            channel: ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: DEFAULT_MEM_LIMIT_MB,
            cpu_limit: DEFAULT_CPU_LIMIT,
            image: "img:1".to_string(),
            previous_image: None,
            allow_lan: false,
            host: crate::hosts::LOCAL_HOST.to_string(),
            notes: String::new(),
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            status: Status::Running,
            last_error: None,
            created_at: crate::models::now_iso(),
            updated_at: crate::models::now_iso(),
        }
    }

    fn harness() -> (AppState, Arc<MockDriver>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.keep());
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap");
        state
            .db
            .insert_instance(&spec("alice"))
            .expect("insert alice");
        state.db.insert_instance(&spec("bob")).expect("insert bob");
        (state, mock)
    }

    fn new_schedule(instance: &str, name: &str, expression: &str) -> NewSchedule {
        NewSchedule {
            instance_id: instance.to_string(),
            name: name.to_string(),
            cron: expression.to_string(),
            prompt: "summarize the incidents".to_string(),
            ..NewSchedule::default()
        }
    }

    fn force_due(state: &AppState, schedule_id: &str) {
        state
            .db
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE schedules SET next_run_at = ?2 WHERE id = ?1",
                    params![schedule_id, "2000-01-01T00:00:00Z"],
                )
            })
            .expect("force due");
    }

    #[test]
    fn every_five_minutes_matches_the_expected_minutes() {
        let parsed = cron("*/5 * * * *");
        let start = at("2026-11-09T10:02:30Z");
        let next = parsed.next_after(start).expect("next");
        assert_eq!(iso(next), "2026-11-09T10:05:00Z");
        assert!(parsed.matches(at("2026-11-09T10:05:00Z")));
        assert!(!parsed.matches(at("2026-11-09T10:06:00Z")));
    }

    #[test]
    fn lists_ranges_and_stepped_ranges_are_supported() {
        let parsed = cron("0,30 9-17/2 * * 1-5");
        assert!(parsed.matches(at("2026-11-09T09:00:00Z"))); // Monday 09:00
        assert!(parsed.matches(at("2026-11-09T17:30:00Z")));
        assert!(!parsed.matches(at("2026-11-09T10:00:00Z")));
        assert!(!parsed.matches(at("2026-11-08T09:00:00Z"))); // Sunday
        let next = parsed.next_after(at("2026-11-09T17:30:00Z")).expect("next");
        assert_eq!(iso(next), "2026-11-10T09:00:00Z");
    }

    #[test]
    fn sunday_can_be_written_seven() {
        let parsed = cron("0 0 * * 7");
        let next = parsed.next_after(at("2026-11-09T00:00:00Z")).expect("next");
        assert_eq!(iso(next), "2026-11-15T00:00:00Z");
        assert!(parsed.matches(at("2026-11-15T00:00:00Z")));
    }

    #[test]
    fn day_of_month_and_day_of_week_are_ored() {
        // The 1st of the month OR any Monday.
        let parsed = cron("0 0 1 * 1");
        assert!(parsed.matches(at("2026-12-01T00:00:00Z"))); // Tuesday the 1st
        assert!(parsed.matches(at("2026-11-09T00:00:00Z"))); // Monday
        assert!(!parsed.matches(at("2026-11-10T00:00:00Z"))); // Tuesday the 10th
    }

    #[test]
    fn invalid_expressions_are_refused() {
        for expression in [
            "",
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * 32 * *",
            "* * * 13 *",
            "* * * * 8",
            "a * * * *",
            "*/0 * * * *",
            "5-1 * * * *",
            "1,,2 * * * *",
            "1-2-3 * * * *",
            "-1 * * * *",
        ] {
            assert!(
                CronSchedule::parse(expression).is_err(),
                "{expression:?} should be refused"
            );
        }
    }

    #[test]
    fn an_expression_that_never_fires_has_no_next_run() {
        let parsed = cron("0 0 31 2 *");
        assert_eq!(parsed.next_after(at("2026-01-01T00:00:00Z")), None);
    }

    #[test]
    fn the_run_command_is_the_prefix_plus_the_prompt() {
        // The default is read at call time; this test never mutates the
        // process environment, so it observes the default.
        let argv = run_argv("hello world");
        assert_eq!(argv.last().map(String::as_str), Some("hello world"));
        assert_eq!(argv.len(), 3);
        assert_eq!(&argv[..2], &["gateway".to_string(), "ask".to_string()]);
    }

    #[test]
    fn output_is_truncated_on_a_character_boundary() {
        let text = "é".repeat(MAX_OUTPUT_BYTES);
        let truncated = truncate_output(&text);
        assert!(truncated.len() <= MAX_OUTPUT_BYTES + 16);
        assert!(truncated.ends_with("…(truncated)"));
    }

    #[test]
    fn schedules_are_created_listed_and_validated() {
        let (state, _) = harness();
        let schedule =
            create(&state.db, new_schedule("alice", "daily", "0 9 * * *")).expect("create");
        assert!(schedule.enabled);
        assert!(schedule.next_run_at.is_some());
        assert_eq!(schedule.timeout_s, DEFAULT_TIMEOUT_S);
        assert_eq!(schedule.overlap, Overlap::Skip);

        let listed = list_for_instance(&state.db, "alice").expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "daily");

        let duplicate =
            create(&state.db, new_schedule("alice", "daily", "* * * * *")).expect_err("duplicate");
        assert!(matches!(duplicate, SchedulerError::NameExists(_)));

        let ghost = create(&state.db, new_schedule("ghost", "x", "* * * * *"))
            .expect_err("unknown instance");
        assert!(matches!(ghost, SchedulerError::UnknownInstance(_)));

        let bad_cron =
            create(&state.db, new_schedule("alice", "bad", "nope")).expect_err("invalid cron");
        assert!(matches!(bad_cron, SchedulerError::InvalidCron(_)));

        // The same name is fine on another instance.
        create(&state.db, new_schedule("bob", "daily", "0 9 * * *")).expect("bob create");
        assert_eq!(
            list_for_instance(&state.db, "bob").expect("bob list").len(),
            1
        );
    }

    #[test]
    fn disabling_a_schedule_clears_its_next_run_and_reenabling_computes_one() {
        let (state, _) = harness();
        let schedule =
            create(&state.db, new_schedule("alice", "daily", "0 9 * * *")).expect("create");
        let disabled = update(
            &state.db,
            &schedule.id,
            SchedulePatch {
                enabled: Some(false),
                ..SchedulePatch::default()
            },
        )
        .expect("disable");
        assert!(!disabled.enabled);
        assert!(disabled.next_run_at.is_none());

        let reenabled = update(
            &state.db,
            &schedule.id,
            SchedulePatch {
                enabled: Some(true),
                ..SchedulePatch::default()
            },
        )
        .expect("enable");
        assert!(reenabled.enabled);
        assert!(reenabled.next_run_at.is_some());

        let deleted = delete(&state.db, &schedule.id).expect("delete");
        assert!(deleted);
        assert!(!delete(&state.db, &schedule.id).expect("again"));
    }

    #[tokio::test]
    async fn a_due_schedule_runs_and_records_ok() {
        let (state, mock) = harness();
        let schedule =
            create(&state.db, new_schedule("alice", "five", "*/5 * * * *")).expect("create");
        force_due(&state, &schedule.id);

        tick(&state).await;

        let runs = list_runs(&state.db, "alice", 10).expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Ok);
        assert_eq!(runs[0].exit_code, Some(0));
        assert_eq!(runs[0].schedule_id, schedule.id);
        assert!(runs[0]
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("summarize the incidents"));

        let calls = mock.exec_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].instance, "alice");
        assert_eq!(
            calls[0].argv.last().map(String::as_str),
            Some("summarize the incidents")
        );
        assert_eq!(calls[0].timeout_s, DEFAULT_TIMEOUT_S);

        let stored = get(&state.db, &schedule.id).expect("get").expect("exists");
        assert!(stored.last_run_at.is_some());
        assert!(
            parse_timestamp(stored.next_run_at.as_deref().expect("next")).expect("timestamp")
                > Utc::now()
        );
    }

    #[tokio::test]
    async fn a_failing_command_records_error_and_exit_code() {
        let (state, mock) = harness();
        mock.force_exec(
            "alice",
            ExecResult {
                exit_code: Some(2),
                stdout: String::new(),
                stderr: "gateway: model overloaded".to_string(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            },
        );
        let schedule =
            create(&state.db, new_schedule("alice", "five", "*/5 * * * *")).expect("create");
        force_due(&state, &schedule.id);

        tick(&state).await;

        let runs = list_runs(&state.db, "alice", 10).expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Error);
        assert_eq!(runs[0].exit_code, Some(2));
        assert!(runs[0]
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("model overloaded"));
    }

    #[tokio::test]
    async fn a_timed_out_command_records_timeout() {
        let (state, mock) = harness();
        mock.force_exec(
            "alice",
            ExecResult {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
                duration_ms: 0,
                truncated: false,
            },
        );
        let mut new = new_schedule("alice", "five", "*/5 * * * *");
        new.timeout_s = 30;
        let schedule = create(&state.db, new).expect("create");
        force_due(&state, &schedule.id);

        tick(&state).await;

        let runs = list_runs(&state.db, "alice", 10).expect("runs");
        assert_eq!(runs[0].status, RunStatus::Timeout);
        assert_eq!(runs[0].exit_code, None);
    }

    #[tokio::test]
    async fn overlap_skip_records_a_skipped_run_and_does_not_execute() {
        let (state, mock) = harness();
        let schedule =
            create(&state.db, new_schedule("alice", "five", "*/5 * * * *")).expect("create");
        let started = iso(Utc::now());
        state
            .db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO schedule_runs (id, schedule_id, instance_id, started_at, status, created_at) \
                     VALUES ('running-run', ?1, 'alice', ?2, 'running', ?2)",
                    params![schedule.id, started],
                )
            })
            .expect("insert running");
        force_due(&state, &schedule.id);

        tick(&state).await;

        assert!(mock.exec_calls().is_empty());
        let runs = list_runs(&state.db, "alice", 10).expect("runs");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].status, RunStatus::Skipped);
        assert!(runs[1].status == RunStatus::Running);
        assert!(get(&state.db, &schedule.id)
            .expect("get")
            .expect("exists")
            .next_run_at
            .is_some());
    }

    #[tokio::test]
    async fn overlap_queue_waits_for_the_running_run() {
        let (state, mock) = harness();
        let schedule =
            create(&state.db, new_schedule("alice", "five", "*/5 * * * *")).expect("create");
        update(
            &state.db,
            &schedule.id,
            SchedulePatch {
                overlap: Some(Overlap::Queue),
                ..SchedulePatch::default()
            },
        )
        .expect("queue");
        let started = iso(Utc::now());
        state
            .db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO schedule_runs (id, schedule_id, instance_id, started_at, status, created_at) \
                     VALUES ('running-run', ?1, 'alice', ?2, 'running', ?2)",
                    params![schedule.id, started],
                )
            })
            .expect("insert running");
        force_due(&state, &schedule.id);

        tick(&state).await;
        assert!(mock.exec_calls().is_empty());

        // The running run ends: the next tick starts the queued occurrence.
        state
            .db
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE schedule_runs SET status = 'ok', finished_at = ?1 WHERE id = 'running-run'",
                    params![iso(Utc::now())],
                )
            })
            .expect("finish");
        tick(&state).await;
        assert_eq!(mock.exec_calls().len(), 1);
    }

    #[tokio::test]
    async fn disabling_one_schedule_never_touches_another_instance() {
        let (state, mock) = harness();
        let alice = create(&state.db, new_schedule("alice", "five", "*/5 * * * *")).expect("alice");
        let bob = create(&state.db, new_schedule("bob", "five", "*/5 * * * *")).expect("bob");
        update(
            &state.db,
            &bob.id,
            SchedulePatch {
                enabled: Some(false),
                ..SchedulePatch::default()
            },
        )
        .expect("disable bob");
        force_due(&state, &alice.id);
        force_due(&state, &bob.id);

        tick(&state).await;

        let calls = mock.exec_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].instance, "alice");
        assert!(list_runs(&state.db, "bob", 10)
            .expect("bob runs")
            .is_empty());
    }

    #[tokio::test]
    async fn a_manual_run_executes_immediately() {
        let (state, mock) = harness();
        let schedule =
            create(&state.db, new_schedule("alice", "five", "*/5 * * * *")).expect("create");
        let run = run_now(&state, &schedule.id).await.expect("run");
        assert_eq!(run.status, RunStatus::Ok);
        assert_eq!(mock.exec_calls().len(), 1);

        // A second manual run while the first is recorded as running answers 409.
        let started = iso(Utc::now());
        state
            .db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO schedule_runs (id, schedule_id, instance_id, started_at, status, created_at) \
                     VALUES ('running-run', ?1, 'alice', ?2, 'running', ?2)",
                    params![schedule.id, started],
                )
            })
            .expect("insert running");
        let err = run_now(&state, &schedule.id).await.expect_err("conflict");
        assert!(matches!(err, SchedulerError::RunInProgress));
    }

    #[test]
    fn recovery_marks_stale_runs_and_skips_missed_occurrences() {
        let (state, _) = harness();
        let schedule =
            create(&state.db, new_schedule("alice", "five", "*/5 * * * *")).expect("create");
        force_due(&state, &schedule.id);
        state
            .db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO schedule_runs (id, schedule_id, instance_id, started_at, status, created_at) \
                     VALUES ('stale', ?1, 'alice', '2026-01-01T00:00:00Z', 'running', '2026-01-01T00:00:00Z')",
                    params![schedule.id],
                )
            })
            .expect("insert stale");

        let recovered = recover(&state.db).expect("recover");
        assert_eq!(recovered, 1);
        let runs = list_runs(&state.db, "alice", 10).expect("runs");
        assert_eq!(runs[0].status, RunStatus::Error);
        assert!(runs[0]
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("restart"));

        // catchup: false → the missed occurrence is replaced by a future one.
        let stored = get(&state.db, &schedule.id).expect("get").expect("exists");
        let next = parse_timestamp(stored.next_run_at.as_deref().expect("next")).expect("ts");
        assert!(next > Utc::now());
    }

    #[test]
    fn recovery_keeps_a_missed_schedule_due_with_catchup() {
        let (state, _) = harness();
        let mut new = new_schedule("alice", "five", "*/5 * * * *");
        new.catchup = true;
        let schedule = create(&state.db, new).expect("create");
        force_due(&state, &schedule.id);

        recover(&state.db).expect("recover");
        let stored = get(&state.db, &schedule.id).expect("get").expect("exists");
        assert_eq!(stored.next_run_at.as_deref(), Some("2000-01-01T00:00:00Z"));
    }

    #[tokio::test]
    async fn the_global_view_reports_last_run_and_failure_rate() {
        let (state, mock) = harness();
        let schedule =
            create(&state.db, new_schedule("alice", "five", "*/5 * * * *")).expect("create");
        mock.force_exec(
            "alice",
            ExecResult {
                exit_code: Some(1),
                stdout: String::new(),
                stderr: "boom".to_string(),
                timed_out: false,
                duration_ms: 0,
                truncated: false,
            },
        );
        execute(&state, &schedule).await.expect("run one");
        execute(&state, &schedule).await.expect("run two");

        let views = list_all(&state.db).expect("global");
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].runs_total, 2);
        assert_eq!(views[0].runs_failed, 2);
        assert_eq!(views[0].failure_rate, 1.0);
        assert_eq!(
            views[0].last_run.as_ref().map(|run| run.status),
            Some(RunStatus::Error)
        );
    }

    #[test]
    fn deleting_an_instance_removes_its_schedules_and_runs() {
        let (state, _) = harness();
        let schedule =
            create(&state.db, new_schedule("alice", "five", "*/5 * * * *")).expect("create");
        state.db.delete_instance("alice").expect("delete");
        assert!(get(&state.db, &schedule.id).expect("get").is_none());
        assert!(list_runs(&state.db, "alice", 10).expect("runs").is_empty());
    }
}
