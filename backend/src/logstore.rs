//! Persistent, searchable log center (issue #42).
//!
//! The control plane tails every instance container on a fixed cadence and
//! stores the lines in SQLite (`log_entries`) with an FTS5 index (`log_fts`).
//! Stored lines are redacted first: a token that reaches a container log never
//! reaches the database or a search response.
//!
//! Repeated tails are idempotent: `(instance_id, line_hash)` is unique, so the
//! same raw line of the same instance is stored once even when every tick reads
//! the same 500-line tail again. The ingestion path also feeds a broadcast
//! channel, which is what `GET /api/logs/stream` forwards as SSE.
//!
//! Retention is bounded twice: by age (`FLEETT_LOG_RETENTION`, default `7d`) and
//! by row count (`FLEETT_LOG_MAX_ROWS`, default 500000, `0` disables the cap).

use crate::api::AppState;
use crate::config::{parse_log_retention, Config};
use crate::db::{Db, DbError};
use crate::driver::LogsOptions;
use crate::logs;
use chrono::{DateTime, SecondsFormat, Utc};
use regex::Regex;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// Lines read from each instance on one background tick.
pub const TAIL_LINES: u32 = 500;
/// Largest `?limit=` accepted by the search API.
pub const MAX_SEARCH_LIMIT: usize = 500;
/// Largest accepted full-text query, in characters.
pub const MAX_QUERY_LEN: usize = 512;
/// Capacity of the SSE broadcast channel.
pub const BROADCAST_CAPACITY: usize = 1024;
/// Background cadence of the tail loop.
pub const TAIL_INTERVAL: Duration = Duration::from_secs(10);
/// How often the background loop enforces retention.
pub const PURGE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Severity of one stored line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "info" | "information" => Some(Self::Info),
            "warn" | "warning" | "warnings" => Some(Self::Warn),
            "error" | "errors" | "fatal" | "critical" | "panic" => Some(Self::Error),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Warn => 1,
            Self::Error => 2,
        }
    }
}

/// One line returned by a search, an SSE event or an export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: i64,
    pub instance: String,
    pub at: String,
    pub level: LogLevel,
    pub message: String,
}

/// Filters accepted by [`search`].
#[derive(Debug, Clone, Default)]
pub struct LogQuery {
    /// Empty means every instance.
    pub instances: Vec<String>,
    /// FTS5 query; a substring LIKE search is used when FTS finds nothing or
    /// rejects the syntax.
    pub q: Option<String>,
    /// Minimum severity: `warn` keeps warnings and errors.
    pub level: Option<LogLevel>,
    /// RFC 3339 lower bound (inclusive).
    pub since: Option<String>,
    /// RFC 3339 upper bound (inclusive).
    pub until: Option<String>,
    /// Page size, `1..=MAX_SEARCH_LIMIT`.
    pub limit: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum LogStoreError {
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("the query exceeds {MAX_QUERY_LEN} characters")]
    QueryTooLong,
    #[error("limit must be between 1 and {MAX_SEARCH_LIMIT}")]
    InvalidLimit,
    #[error("invalid time filter: {0}")]
    InvalidTime(String),
}

/// Counters of one ingestion batch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IngestReport {
    pub scanned: usize,
    pub inserted: usize,
    pub duplicates: usize,
}

/// Result of one retention pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PurgeReport {
    /// Rows older than the retention window.
    pub removed_old: usize,
    /// Rows removed to stay under `FLEETT_LOG_MAX_ROWS`.
    pub removed_overflow: usize,
    pub remaining: usize,
    /// Timestamp of the pass (RFC 3339, UTC).
    pub at: String,
}

/// Result of one background tick.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TickReport {
    pub instances: usize,
    pub ingested: usize,
    pub inserted: usize,
    pub duplicates: usize,
    pub errors: usize,
    pub purged: Option<PurgeReport>,
}

/// One parsed, redacted log line, ready to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLine {
    pub at: String,
    pub level: LogLevel,
    pub message: String,
    pub raw: String,
    pub line_hash: String,
}

/// Patterns whose value must never reach the database.
///
/// The list follows issue #42: control-plane tokens (`flt_`), OpenAI-style
/// keys (`sk-`), Discord bot tokens (`Bot …`), bearer credentials and generic
/// `token=…` query strings. Every match is replaced by a fixed marker so a
/// search never reveals the shape of the secret either.
static SECRET_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(r"flt_[0-9a-f]{32}").expect("valid flt_ pattern"),
            "flt_[REDACTED]",
        ),
        (
            Regex::new(r"sk-[A-Za-z0-9_-]{20,}").expect("valid sk- pattern"),
            "sk-[REDACTED]",
        ),
        (
            Regex::new(r"(?i)\bBot\s+[A-Za-z0-9._-]{20,}").expect("valid Bot pattern"),
            "Bot [REDACTED]",
        ),
        (
            Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]+").expect("valid Bearer pattern"),
            "Bearer [REDACTED]",
        ),
        (
            Regex::new(r#"(?i)\btoken=[^\s&"',;]+"#).expect("valid token= pattern"),
            "token=[REDACTED]",
        ),
    ]
});

/// Level keywords used when the line is not structured JSON.
static LEVEL_KEYWORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(error|fatal|critical|panic|warn(?:ing)?|info)\b")
        .expect("valid level pattern")
});

/// Replace every known secret pattern with its redaction marker.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for (pattern, replacement) in SECRET_PATTERNS.iter() {
        out = pattern.replace_all(&out, *replacement).into_owned();
    }
    out
}

/// SHA-256 of a raw log line, hex encoded.
pub fn line_hash(line: &str) -> String {
    let digest = Sha256::digest(line.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// RFC 3339 timestamp normalised to UTC milliseconds, when parseable.
fn normalize_timestamp(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value).ok().map(|time| {
        time.with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    })
}

fn now_millis() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Best-effort level of one line.
///
/// A JSON line is read first (`level`, `severity`, `lvl`); otherwise every
/// keyword of the line is scanned and the most severe one wins.
fn parse_level(body: &str) -> LogLevel {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        for key in ["level", "severity", "lvl"] {
            if let Some(level) = value.get(key).and_then(|value| value.as_str()) {
                if let Some(level) = LogLevel::parse(level) {
                    return level;
                }
            }
        }
    }
    let mut level = LogLevel::Info;
    for found in LEVEL_KEYWORDS.find_iter(body) {
        let candidate = LogLevel::parse(found.as_str()).unwrap_or(LogLevel::Info);
        if candidate.rank() > level.rank() {
            level = candidate;
        }
    }
    level
}

/// Parse and redact one raw line.
///
/// The timestamp is taken from the line when it starts with RFC 3339; otherwise
/// the ingestion time is used so every row has a sortable `at`. The stored
/// message drops the `docker compose logs` container prefix and a leading
/// timestamp; `raw` keeps the whole line, redacted.
pub fn parse_line(line: &str, ingested_at: &str) -> Option<ParsedLine> {
    let line = line.trim_end_matches('\r');
    if line.trim().is_empty() {
        return None;
    }
    let (at, body) = match logs::split_timestamp(line) {
        Some((time, message)) => (
            normalize_timestamp(&time).unwrap_or_else(|| ingested_at.to_string()),
            message,
        ),
        None => (
            ingested_at.to_string(),
            logs::strip_compose_prefix(line).to_string(),
        ),
    };
    let level = parse_level(&body);
    Some(ParsedLine {
        at,
        level,
        message: redact(&body),
        raw: redact(line),
        line_hash: line_hash(line),
    })
}

/// Parse a `since`/`until` value: RFC 3339 or a relative window (`30m`, `24h`,
/// `7d`). Relative values mean "before now", so they only make sense for
/// `since`; an `until` in the future is clamped by the caller.
pub fn parse_time_value(value: &str, now: DateTime<Utc>) -> Result<String, LogStoreError> {
    let value = value.trim();
    if let Some(at) = normalize_timestamp(value) {
        return Ok(at);
    }
    if let Some(window) = parse_log_retention(value) {
        let cutoff = now - chrono::Duration::seconds(window.as_secs() as i64);
        return Ok(cutoff.to_rfc3339_opts(SecondsFormat::Millis, true));
    }
    Err(LogStoreError::InvalidTime(format!(
        "\"{value}\" is neither an RFC 3339 timestamp nor a duration (30m, 24h, 7d)"
    )))
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<LogEntry> {
    let level: String = row.get(3)?;
    Ok(LogEntry {
        id: row.get(0)?,
        instance: row.get(1)?,
        at: row.get(2)?,
        level: LogLevel::parse(&level).unwrap_or(LogLevel::Info),
        message: row.get(4)?,
    })
}

fn query_entries(
    conn: &rusqlite::Connection,
    sql: &str,
    values: Vec<SqlValue>,
) -> rusqlite::Result<Vec<LogEntry>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), row_to_entry)?;
    rows.collect()
}

fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Search the stored lines, newest first.
///
/// The text filter is an FTS5 `MATCH` first. If FTS rejects the syntax, or the
/// query finds nothing (a partial word such as `allow` in `allowlist` is not an
/// FTS token), the same filter is retried as a substring `LIKE`. Every other
/// filter (instances, level, window, limit) is identical in both passes.
pub fn search(db: &Db, query: &LogQuery) -> Result<Vec<LogEntry>, LogStoreError> {
    if query.limit == 0 || query.limit > MAX_SEARCH_LIMIT {
        return Err(LogStoreError::InvalidLimit);
    }
    let text = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(text) = text {
        if text.chars().count() > MAX_QUERY_LEN {
            return Err(LogStoreError::QueryTooLong);
        }
    }

    let mut clauses: Vec<String> = Vec::new();
    let mut values: Vec<SqlValue> = Vec::new();

    if !query.instances.is_empty() {
        clauses.push(format!(
            "e.instance_id IN ({})",
            placeholders(query.instances.len())
        ));
        values.extend(query.instances.iter().map(|id| SqlValue::Text(id.clone())));
    }
    match query.level {
        Some(LogLevel::Warn) => clauses.push("e.level IN ('warn', 'error')".to_string()),
        Some(LogLevel::Error) => clauses.push("e.level = 'error'".to_string()),
        _ => {}
    }
    if let Some(since) = &query.since {
        clauses.push("e.at >= ?".to_string());
        values.push(SqlValue::Text(since.clone()));
    }
    if let Some(until) = &query.until {
        clauses.push("e.at <= ?".to_string());
        values.push(SqlValue::Text(until.clone()));
    }

    let has_clauses = !clauses.is_empty();
    let base = if has_clauses {
        format!(
            "SELECT e.id, e.instance_id, e.at, e.level, e.message FROM log_entries e WHERE {}",
            clauses.join(" AND ")
        )
    } else {
        "SELECT e.id, e.instance_id, e.at, e.level, e.message FROM log_entries e".to_string()
    };
    let connector = if has_clauses { " AND " } else { " WHERE " };
    let order = " ORDER BY e.at DESC, e.id DESC LIMIT ?".to_string();

    let Some(text) = text else {
        let sql = format!("{base}{order}");
        let mut values = values;
        values.push(SqlValue::Integer(query.limit as i64));
        return db
            .with_conn(|conn| query_entries(conn, &sql, values))
            .map_err(LogStoreError::from);
    };

    let fts_sql = format!(
        "{base}{connector}e.id IN (SELECT rowid FROM log_fts WHERE log_fts MATCH ?){order}"
    );
    let mut fts_values = values.clone();
    fts_values.push(SqlValue::Text(text.to_string()));
    fts_values.push(SqlValue::Integer(query.limit as i64));

    let fts_result = db.with_conn(|conn| query_entries(conn, &fts_sql, fts_values));
    match fts_result {
        Ok(entries) if !entries.is_empty() => return Ok(entries),
        Ok(_) => {}
        Err(err) => {
            tracing::debug!(%err, query = text, "FTS query rejected; falling back to LIKE");
        }
    }

    let like_sql = format!("{base}{connector}e.message LIKE ? ESCAPE '\\'{order}");
    let mut like_values = values;
    like_values.push(SqlValue::Text(format!("%{}%", escape_like(text))));
    like_values.push(SqlValue::Integer(query.limit as i64));
    db.with_conn(|conn| query_entries(conn, &like_sql, like_values))
        .map_err(LogStoreError::from)
}

/// Delete rows older than `retention`, then trim the oldest rows over `max_rows`.
///
/// `max_rows == 0` disables the row cap. The FTS documents are deleted in the
/// same transaction as their `log_entries` row, so a search never sees a
/// message whose row is gone.
pub fn purge_retention(
    db: &Db,
    retention: Duration,
    max_rows: u64,
    now: DateTime<Utc>,
) -> Result<PurgeReport, LogStoreError> {
    let cutoff = now
        - chrono::Duration::from_std(retention)
            .map_err(|err| LogStoreError::InvalidTime(err.to_string()))?;
    let cutoff = cutoff.to_rfc3339_opts(SecondsFormat::Millis, true);
    let at = now.to_rfc3339_opts(SecondsFormat::Millis, true);

    let report = db.with_transaction(|tx| {
        tx.execute(
            "DELETE FROM log_fts WHERE rowid IN (SELECT id FROM log_entries WHERE at < ?1)",
            params![cutoff],
        )?;
        let removed_old = tx.execute("DELETE FROM log_entries WHERE at < ?1", params![cutoff])?;

        let mut removed_overflow = 0usize;
        if max_rows > 0 {
            let total: i64 =
                tx.query_row("SELECT COUNT(*) FROM log_entries", [], |row| row.get(0))?;
            let overflow = total.saturating_sub(max_rows as i64);
            if overflow > 0 {
                tx.execute(
                    "DELETE FROM log_fts WHERE rowid IN (SELECT id FROM log_entries \
                     ORDER BY at ASC, id ASC LIMIT ?1)",
                    params![overflow],
                )?;
                removed_overflow = tx.execute(
                    "DELETE FROM log_entries WHERE id IN (SELECT id FROM log_entries \
                     ORDER BY at ASC, id ASC LIMIT ?1)",
                    params![overflow],
                )?;
            }
        }

        let remaining: i64 =
            tx.query_row("SELECT COUNT(*) FROM log_entries", [], |row| row.get(0))?;
        Ok::<_, rusqlite::Error>((removed_old, removed_overflow, remaining as usize))
    })?;

    let report = PurgeReport {
        removed_old: report.0,
        removed_overflow: report.1,
        remaining: report.2,
        at,
    };
    if report.removed_old > 0 || report.removed_overflow > 0 {
        tracing::info!(
            removed_old = report.removed_old,
            removed_overflow = report.removed_overflow,
            remaining = report.remaining,
            "log retention purge"
        );
    }
    Ok(report)
}

/// Process-wide log store: broadcast fan-out plus retention policy.
#[derive(Debug)]
pub struct LogStore {
    sender: broadcast::Sender<LogEntry>,
    retention: Duration,
    max_rows: u64,
    last_purge: Mutex<Option<Instant>>,
}

impl LogStore {
    pub fn new(retention: Duration, max_rows: u64) -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            sender,
            retention,
            max_rows,
            last_purge: Mutex::new(None),
        }
    }

    pub fn from_config(cfg: &Config) -> Self {
        Self::new(cfg.log_retention, cfg.log_max_rows)
    }

    pub fn retention(&self) -> Duration {
        self.retention
    }

    pub fn max_rows(&self) -> u64 {
        self.max_rows
    }

    /// Subscribe to the new lines fed by ingestion.
    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.sender.subscribe()
    }

    /// Store `text` for one instance and broadcast the newly inserted lines.
    pub fn ingest(
        &self,
        db: &Db,
        instance: &str,
        text: &str,
    ) -> Result<IngestReport, LogStoreError> {
        let ingested_at = now_millis();
        let parsed: Vec<ParsedLine> = text
            .lines()
            .filter_map(|line| parse_line(line, &ingested_at))
            .collect();
        let mut events: Vec<LogEntry> = Vec::with_capacity(parsed.len());

        db.with_transaction(|tx| {
            for line in &parsed {
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO log_entries \
                     (instance_id, at, level, message, raw, line_hash) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        instance,
                        line.at,
                        line.level.as_str(),
                        line.message,
                        line.raw,
                        line.line_hash
                    ],
                )?;
                if inserted == 0 {
                    continue;
                }
                let id = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO log_fts (rowid, message, instance_id) VALUES (?1, ?2, ?3)",
                    params![id, line.message, instance],
                )?;
                events.push(LogEntry {
                    id,
                    instance: instance.to_string(),
                    at: line.at.clone(),
                    level: line.level,
                    message: line.message.clone(),
                });
            }
            Ok(())
        })?;

        for event in &events {
            // No receiver is not an error: ingestion must work before the first
            // SSE console connects.
            let _ = self.sender.send(event.clone());
        }
        Ok(IngestReport {
            scanned: parsed.len(),
            inserted: events.len(),
            duplicates: parsed.len() - events.len(),
        })
    }

    /// Run the retention pass when the hourly window elapsed.
    pub fn purge_due(
        &self,
        db: &Db,
        now: DateTime<Utc>,
    ) -> Result<Option<PurgeReport>, LogStoreError> {
        {
            let last = self
                .last_purge
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(last) = *last {
                if last.elapsed() < PURGE_INTERVAL {
                    return Ok(None);
                }
            }
        }
        let report = purge_retention(db, self.retention, self.max_rows, now)?;
        let mut last = self
            .last_purge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *last = Some(Instant::now());
        Ok(Some(report))
    }
}

/// Tail every instance once and ingest what Docker returned.
///
/// A failing container never fails the tick: the instance is counted in
/// `errors` and the remaining instances are still processed.
pub async fn tail_all(state: &AppState) -> TickReport {
    let specs = match state.db.list() {
        Ok(specs) => specs,
        Err(err) => {
            tracing::warn!(%err, "could not list instances for the log tail");
            return TickReport {
                errors: 1,
                ..TickReport::default()
            };
        }
    };
    let mut report = TickReport {
        instances: specs.len(),
        ..TickReport::default()
    };
    for spec in &specs {
        let raw = match state
            .driver
            .logs(
                spec,
                &LogsOptions {
                    tail: TAIL_LINES,
                    since: None,
                },
            )
            .await
        {
            Ok(raw) => raw,
            Err(err) => {
                report.errors += 1;
                tracing::debug!(instance = %spec.id, %err, "could not tail the instance logs");
                continue;
            }
        };
        let raw = logs::truncate(&raw, logs::MAX_SCAN_BYTES);
        match state.logstore.ingest(&state.db, &spec.id, raw) {
            Ok(ingested) => {
                report.ingested += ingested.scanned;
                report.inserted += ingested.inserted;
                report.duplicates += ingested.duplicates;
            }
            Err(err) => {
                report.errors += 1;
                tracing::warn!(instance = %spec.id, %err, "could not store the instance logs");
            }
        }
    }
    report
}

/// One background pass: tail everything, then enforce retention when due.
pub async fn tick(state: &AppState) -> TickReport {
    let mut report = tail_all(state).await;
    match state.logstore.purge_due(&state.db, Utc::now()) {
        Ok(purged) => report.purged = purged,
        Err(err) => {
            report.errors += 1;
            tracing::warn!(%err, "log retention purge failed");
        }
    }
    report
}

/// Background loop started by `serve` only (tests call [`tick`] directly).
pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(TAIL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let report = tick(&state).await;
        if report.errors > 0 {
            tracing::debug!(
                instances = report.instances,
                inserted = report.inserted,
                errors = report.errors,
                "log tail tick finished with errors"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open database");
        (dir, db)
    }

    fn store() -> LogStore {
        LogStore::new(Duration::from_secs(7 * 24 * 3600), 0)
    }

    #[test]
    fn known_secrets_are_redacted() {
        let flt = format!("flt_{}", "a".repeat(32));
        let sk = format!("sk-{}", "B".repeat(24));
        let discord = format!("Bot {}", "C".repeat(30));
        let text = format!(
            "token leaked: {flt} and {sk} and {discord} with Bearer abc.def-ghi \
             and url?token=super-secret&x=1"
        );
        let redacted = redact(&text);
        for secret in [&flt, &sk, "CCCCCCCCC", "abc.def-ghi", "super-secret"] {
            assert!(!redacted.contains(secret), "{secret} leaked: {redacted}");
        }
        assert!(redacted.contains("flt_[REDACTED]"), "{redacted}");
        assert!(redacted.contains("sk-[REDACTED]"), "{redacted}");
        assert!(redacted.contains("Bot [REDACTED]"), "{redacted}");
        assert!(redacted.contains("Bearer [REDACTED]"), "{redacted}");
        assert!(redacted.contains("token=[REDACTED]"), "{redacted}");
    }

    #[test]
    fn lines_are_parsed_with_timestamps_and_levels() {
        let line = "alice  | 2026-11-09T10:00:00+01:00 ERROR gateway died";
        let parsed = parse_line(line, "2026-11-09T09:00:00.000Z").expect("parsed");
        assert_eq!(parsed.at, "2026-11-09T09:00:00.000Z");
        assert_eq!(parsed.level, LogLevel::Error);
        assert_eq!(parsed.message, "ERROR gateway died");

        let json = r#"{"level":"warning","msg":"slow"}"#;
        let parsed = parse_line(json, "2026-11-09T09:00:00.000Z").expect("parsed");
        assert_eq!(parsed.level, LogLevel::Warn);
        assert_eq!(parsed.at, "2026-11-09T09:00:00.000Z");

        assert_eq!(parse_level("INFO ready"), LogLevel::Info);
        assert_eq!(parse_level("ERROR while WARNING"), LogLevel::Error);
        assert_eq!(parse_level("plain line"), LogLevel::Info);
        assert!(parse_line("   ", "2026-11-09T09:00:00.000Z").is_none());
    }

    #[test]
    fn ingestion_redacts_and_repeated_tails_are_idempotent() {
        let (_dir, db) = temp_db();
        let store = store();
        let flt = format!("flt_{}", "d".repeat(32));
        let text = format!(
            "2026-11-09T10:00:00Z ERROR boom token={flt}\n\
             2026-11-09T10:00:01Z INFO ready\n"
        );
        let first = store.ingest(&db, "alice", &text).expect("first ingest");
        assert_eq!(first.inserted, 2);
        assert_eq!(first.duplicates, 0);

        let second = store.ingest(&db, "alice", &text).expect("second ingest");
        assert_eq!(second.inserted, 0);
        assert_eq!(second.duplicates, 2, "the same tail is swallowed");

        let rows: Vec<(String, String)> = db
            .with_conn(|conn| {
                let mut stmt = conn.prepare("SELECT message, raw FROM log_entries ORDER BY id")?;
                let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect()
            })
            .expect("rows");
        assert_eq!(rows.len(), 2);
        for (message, raw) in &rows {
            assert!(!message.contains(&flt), "{message}");
            assert!(!raw.contains(&flt), "{raw}");
        }
    }

    #[test]
    fn search_is_multi_instance_and_sorted_by_time() {
        let (_dir, db) = temp_db();
        let store = store();
        store
            .ingest(
                &db,
                "alice",
                "2026-11-09T10:00:00Z allowlist rejected\n2026-11-09T10:02:00Z other",
            )
            .expect("alice");
        store
            .ingest(&db, "bob", "2026-11-09T10:01:00Z allowlist accepted")
            .expect("bob");

        let entries = search(
            &db,
            &LogQuery {
                q: Some("allowlist".to_string()),
                limit: 10,
                ..LogQuery::default()
            },
        )
        .expect("search");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].instance, "bob");
        assert_eq!(entries[1].instance, "alice");
        assert!(entries[0].at > entries[1].at);

        let only_alice = search(
            &db,
            &LogQuery {
                instances: vec!["alice".to_string()],
                limit: 10,
                ..LogQuery::default()
            },
        )
        .expect("search");
        assert_eq!(only_alice.len(), 2);
        assert!(only_alice.iter().all(|entry| entry.instance == "alice"));
    }

    #[test]
    fn partial_words_fall_back_to_like_and_bad_fts_syntax_is_safe() {
        let (_dir, db) = temp_db();
        let store = store();
        store
            .ingest(&db, "alice", "2026-11-09T10:00:00Z the allowlist is strict")
            .expect("ingest");

        let partial = search(
            &db,
            &LogQuery {
                q: Some("allow".to_string()),
                limit: 10,
                ..LogQuery::default()
            },
        )
        .expect("partial search");
        assert_eq!(partial.len(), 1, "LIKE covers partial words");

        let broken = search(
            &db,
            &LogQuery {
                q: Some("AND (".to_string()),
                limit: 10,
                ..LogQuery::default()
            },
        )
        .expect("broken FTS falls back");
        assert!(broken.is_empty());
    }

    #[test]
    fn level_filter_keeps_warnings_and_above() {
        let (_dir, db) = temp_db();
        let store = store();
        store
            .ingest(
                &db,
                "alice",
                "2026-11-09T10:00:00Z INFO one\n\
                 2026-11-09T10:01:00Z WARN two\n\
                 2026-11-09T10:02:00Z ERROR three",
            )
            .expect("ingest");
        let warnings = search(
            &db,
            &LogQuery {
                level: Some(LogLevel::Warn),
                limit: 10,
                ..LogQuery::default()
            },
        )
        .expect("search");
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(|entry| entry.level != LogLevel::Info));

        let infos = search(
            &db,
            &LogQuery {
                level: Some(LogLevel::Info),
                limit: 10,
                ..LogQuery::default()
            },
        )
        .expect("search");
        assert_eq!(infos.len(), 3);
    }

    #[test]
    fn retention_purges_old_rows_and_enforces_the_row_cap() {
        let (_dir, db) = temp_db();
        let store = store();
        store
            .ingest(
                &db,
                "alice",
                "2020-01-01T00:00:00Z ancient\n\
                 2026-11-09T09:00:00Z recent one\n\
                 2026-11-09T10:00:00Z recent two",
            )
            .expect("ingest");

        let now = DateTime::parse_from_rfc3339("2026-11-09T12:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let report =
            purge_retention(&db, Duration::from_secs(7 * 24 * 3600), 0, now).expect("purge");
        assert_eq!(report.removed_old, 1);
        assert_eq!(report.removed_overflow, 0);
        assert_eq!(report.remaining, 2);

        let report =
            purge_retention(&db, Duration::from_secs(7 * 24 * 3600), 1, now).expect("cap purge");
        assert_eq!(report.removed_overflow, 1);
        assert_eq!(report.remaining, 1);

        let remaining = search(
            &db,
            &LogQuery {
                limit: 10,
                ..LogQuery::default()
            },
        )
        .expect("search after purge");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].message, "recent two");
    }

    #[tokio::test]
    async fn ingestion_feeds_the_sse_broadcast() {
        let (_dir, db) = temp_db();
        let store = store();
        let mut receiver = store.subscribe();
        store
            .ingest(&db, "alice", "2026-11-09T10:00:00Z ERROR boom")
            .expect("ingest");
        let event = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("event before timeout")
            .expect("event");
        assert_eq!(event.instance, "alice");
        assert_eq!(event.level, LogLevel::Error);
        assert!(event.message.contains("boom"));
    }

    #[test]
    fn limits_and_time_values_are_validated() {
        assert!(matches!(
            search(
                &temp_db().1,
                &LogQuery {
                    limit: 0,
                    ..LogQuery::default()
                }
            ),
            Err(LogStoreError::InvalidLimit)
        ));
        let now = Utc::now();
        assert!(parse_time_value("1h", now).is_ok());
        assert!(parse_time_value("2026-11-09T10:00:00Z", now).is_ok());
        assert!(matches!(
            parse_time_value("yesterday", now),
            Err(LogStoreError::InvalidTime(_))
        ));
    }
}
