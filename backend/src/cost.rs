//! Token accounting, model pricing and monthly budgets per agent (issue #35).
//!
//! The gateway of an instance prints its upstream `usage` object in the
//! container logs, one line per request. [`scan_all`] tails those logs through
//! the driver, [`parse_line`] recognises the OpenAI, Anthropic and plain
//! `key=value` shapes, and [`ingest_lines`] stores one row per line in
//! `usage_events`.
//!
//! Ingestion is idempotent: the fingerprint of an event is the SHA-256 of the
//! instance id and the exact log line, so scanning the same tail twice (or
//! replaying a request) never double-counts a request.
//!
//! Costs are computed from `model_pricing`, which is versioned by
//! `effective_from`: the price applied to an event is the latest row of its
//! model whose effective date is on or before the event day. Updating a price
//! therefore never rewrites yesterday's cost — add a row with a new effective
//! date instead.
//!
//! Budgets (`cost_budgets`) are monthly ceilings in USD. [`check_budgets`]
//! reports a crossing once per process and month; [`events::emit`] is the hook
//! the notification/incident pipeline will implement. Until that module is
//! merged it writes a structured WARN line, documented in `docs/COST.md`.

use crate::api::AppState;
use crate::db::{Db, DbError};
use crate::driver::LogsOptions;
use crate::models::now_iso;
use chrono::{DateTime, Datelike, NaiveDate, SecondsFormat, Utc};
use regex::Regex;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// Lines of container log read by one automatic scan.
pub const LOG_SCAN_TAIL: u32 = 2000;
/// Default interval of the background scanner (`FLEETT_COST_SCAN_INTERVAL_S`).
pub const DEFAULT_SCAN_INTERVAL_S: u64 = 300;
/// Most lines accepted by one `POST /api/usage/ingest` call.
pub const MAX_INGEST_LINES: usize = 5000;
/// Longest log line parsed (longer lines are ignored, never truncated).
pub const MAX_LINE_BYTES: usize = 16 * 1024;
/// Largest `text` body accepted by `POST /api/usage/ingest` (4 MiB).
pub const MAX_INGEST_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// Largest accepted price, in currency per million tokens.
pub const MAX_PRICE_PER_MTOK: f64 = 1_000_000.0;
/// Largest accepted monthly budget, in USD.
pub const MAX_BUDGET_USD: f64 = 1_000_000.0;
/// Number of agents returned in `top_agents`.
pub const TOP_AGENTS: usize = 5;
/// Model key used by the report when a usage line names no model.
pub const UNKNOWN_MODEL: &str = "unknown";

static KV_TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b(prompt_tokens|completion_tokens|total_tokens|input_tokens|output_tokens)["']?\s*[:=]\s*["']?(\d+)"#,
    )
    .expect("the token regex is valid")
});

static KV_MODEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\bmodel["']?\s*[:=]\s*["']?([A-Za-z0-9][A-Za-z0-9._:/@+-]*)"#)
        .expect("the model regex is valid")
});

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Where a usage row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Log,
    Api,
    Test,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Api => "api",
            Self::Test => "test",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "log" => Some(Self::Log),
            "api" => Some(Self::Api),
            "test" => Some(Self::Test),
            _ => None,
        }
    }
}

/// Token counts parsed from one log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub model: Option<String>,
    /// RFC 3339 timestamp carried by the line, normalised to UTC.
    pub at: Option<String>,
}

/// One row of `usage_events`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageEvent {
    pub id: i64,
    pub instance_id: String,
    pub at: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub source: String,
}

/// Event inserted by an API caller that already knows the token counts.
#[derive(Debug, Clone)]
pub struct NewUsageEvent {
    pub instance_id: String,
    pub at: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub source: Source,
}

/// Result of one ingestion call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IngestSummary {
    /// Lines that carried a token usage payload.
    pub parsed: usize,
    /// Rows actually written; a duplicate line is parsed but not inserted.
    pub inserted: usize,
    /// Lines that repeated an already stored fingerprint.
    pub duplicates: usize,
    /// Lines with no usage payload (or longer than [`MAX_LINE_BYTES`]).
    pub ignored: usize,
}

/// Parse one log line into its token counts, when it carries any.
///
/// Recognised shapes:
/// - an OpenAI/Anthropic JSON object, anywhere in the line:
///   `{"model":"m","usage":{"prompt_tokens":12,"completion_tokens":3}}`,
///   `{"usage":{"input_tokens":12,"output_tokens":3}}` (a nested `usage`
///   object is found at any depth);
/// - a Python-style repr (`usage={'prompt_tokens': 12, ...}`);
/// - plain `key=value` or `key: value` pairs (`prompt_tokens=12`).
///
/// `total_tokens` defaults to `prompt_tokens + completion_tokens`.
pub fn parse_line(line: &str) -> Option<ParsedUsage> {
    let line = line.trim_end_matches(['\r', '\n']).trim();
    if line.is_empty() {
        return None;
    }
    let (at, body) = match crate::logs::split_timestamp(line) {
        Some((time, message)) => (normalize_timestamp(&time), message),
        None => (None, line.to_string()),
    };

    let mut parsed = parse_json_usage(&body);
    if parsed.is_none() {
        parsed = parse_key_value(&body);
    }
    let mut parsed = parsed?;
    parsed.at = at;
    Some(parsed)
}

fn parse_json_usage(line: &str) -> Option<ParsedUsage> {
    for candidate in json_objects(line) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(candidate) else {
            continue;
        };
        if let Some(usage) = usage_from_json(&value) {
            return Some(usage);
        }
    }
    None
}

fn usage_from_json(value: &serde_json::Value) -> Option<ParsedUsage> {
    let usage = find_key(value, "usage")
        .and_then(serde_json::Value::as_object)
        .or_else(|| value.as_object())?;
    let prompt = token_field(usage, &["prompt_tokens", "input_tokens"]).unwrap_or(0);
    let completion = token_field(usage, &["completion_tokens", "output_tokens"]).unwrap_or(0);
    let total = token_field(usage, &["total_tokens"]).unwrap_or(prompt + completion);
    if prompt == 0 && completion == 0 && total == 0 {
        return None;
    }
    let model = value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            find_key(value, "model")
                .and_then(serde_json::Value::as_str)
                .filter(|model| !model.trim().is_empty())
        })
        .map(str::to_string);
    Some(ParsedUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total.max(prompt + completion),
        model,
        at: None,
    })
}

fn parse_key_value(line: &str) -> Option<ParsedUsage> {
    let mut prompt = 0u64;
    let mut completion = 0u64;
    let mut total = None;
    let mut found = false;
    for capture in KV_TOKENS.captures_iter(line) {
        let Some(value) = capture
            .get(2)
            .and_then(|value| value.as_str().parse::<u64>().ok())
        else {
            continue;
        };
        found = true;
        match capture
            .get(1)
            .map(|name| name.as_str().to_ascii_lowercase())
            .unwrap_or_default()
            .as_str()
        {
            "prompt_tokens" | "input_tokens" => prompt = value,
            "completion_tokens" | "output_tokens" => completion = value,
            "total_tokens" => total = Some(value),
            _ => {}
        }
    }
    found
        .then(|| {
            let total = total.unwrap_or(prompt + completion);
            ParsedUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: total.max(prompt + completion),
                model: KV_MODEL
                    .captures(line)
                    .and_then(|capture| capture.get(1))
                    .map(|model| model.as_str().to_string()),
                at: None,
            }
        })
        .filter(|usage| {
            usage.total_tokens > 0 || usage.prompt_tokens > 0 || usage.completion_tokens > 0
        })
}

/// Every top-level `{...}` fragment of a line, in order (at most 8).
fn json_objects(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(begin) = start.take() {
                        out.push(&line[begin..=index]);
                        if out.len() >= 8 {
                            return out;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn find_key<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(found) = map.get(key) {
                return Some(found);
            }
            map.values().find_map(|child| find_key(child, key))
        }
        serde_json::Value::Array(items) => items.iter().find_map(|item| find_key(item, key)),
        _ => None,
    }
}

fn token_field(object: &serde_json::Map<String, serde_json::Value>, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| {
        object.get(*name).and_then(|value| {
            value
                .as_u64()
                .or_else(|| {
                    value
                        .as_f64()
                        .filter(|n| *n >= 0.0)
                        .map(|n| n.round() as u64)
                })
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
    })
}

/// Normalise an RFC 3339 timestamp to UTC (`2026-11-09T10:00:00Z`).
pub fn normalize_timestamp(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|value| {
            value
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        })
}

fn fingerprint(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0x1f]);
    }
    format!("{:x}", hasher.finalize())
}

fn token_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// Ingestion
// ---------------------------------------------------------------------------

/// Parse and store every usage line of `text` (idempotent).
pub fn ingest_logs(
    db: &Db,
    instance: &str,
    text: &str,
    default_model: &str,
) -> Result<IngestSummary, DbError> {
    let lines: Vec<&str> = text.lines().collect();
    ingest_lines(db, instance, &lines, default_model, None, Source::Log)
}

/// Parse and store a batch of log lines (idempotent).
///
/// `at` is the fallback timestamp of lines that carry none (normally the scan
/// time). The fingerprint covers the instance id and the exact line: replaying
/// the same tail twice inserts nothing the second time.
pub fn ingest_lines(
    db: &Db,
    instance: &str,
    lines: &[&str],
    default_model: &str,
    at: Option<&str>,
    source: Source,
) -> Result<IngestSummary, DbError> {
    let fallback_at = at.and_then(normalize_timestamp).unwrap_or_else(now_iso);
    let mut summary = IngestSummary::default();
    let mut rows: Vec<(String, String, String, u64, u64, u64)> = Vec::new();
    for line in lines {
        if line.len() > MAX_LINE_BYTES {
            summary.ignored += 1;
            continue;
        }
        let Some(usage) = parse_line(line) else {
            summary.ignored += 1;
            continue;
        };
        summary.parsed += 1;
        let model = usage
            .model
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| default_model.to_string());
        rows.push((
            fingerprint(&[instance, line.trim_end_matches(['\r', '\n'])]),
            usage.at.unwrap_or_else(|| fallback_at.clone()),
            model,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens,
        ));
    }
    if rows.is_empty() {
        return Ok(summary);
    }
    let inserted = db.with_transaction(|conn| {
        let mut inserted = 0usize;
        for (fingerprint, at, model, prompt, completion, total) in &rows {
            let changed = conn.execute(
                "INSERT INTO usage_events (instance_id, at, model, prompt_tokens, \
                 completion_tokens, total_tokens, source, fingerprint) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT (fingerprint) DO NOTHING",
                params![
                    instance,
                    at,
                    model,
                    token_i64(*prompt),
                    token_i64(*completion),
                    token_i64(*total),
                    source.as_str(),
                    fingerprint,
                ],
            )?;
            if changed > 0 {
                inserted += 1;
            }
        }
        Ok(inserted)
    })?;
    summary.inserted = inserted;
    summary.duplicates = summary.parsed.saturating_sub(inserted);
    Ok(summary)
}

/// Store one usage event whose counts are already known (API ingestion).
///
/// Returns `false` when an identical event was already recorded.
pub fn ingest_event(db: &Db, event: &NewUsageEvent) -> Result<bool, DbError> {
    let at = normalize_timestamp(&event.at).unwrap_or_else(now_iso);
    let prompt = token_i64(event.prompt_tokens);
    let completion = token_i64(event.completion_tokens);
    let total = token_i64(
        event
            .total_tokens
            .max(event.prompt_tokens + event.completion_tokens),
    );
    let fingerprint = fingerprint(&[
        &event.instance_id,
        &at,
        &event.model,
        &prompt.to_string(),
        &completion.to_string(),
        &total.to_string(),
    ]);
    let changed = db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO usage_events (instance_id, at, model, prompt_tokens, \
             completion_tokens, total_tokens, source, fingerprint) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT (fingerprint) DO NOTHING",
            params![
                event.instance_id,
                at,
                event.model,
                prompt,
                completion,
                total,
                event.source.as_str(),
                fingerprint,
            ],
        )
    })?;
    Ok(changed > 0)
}

/// Events whose `at` falls in `[from, to)`, oldest first.
pub fn events_between(db: &Db, from: &str, to: &str) -> Result<Vec<UsageEvent>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, instance_id, at, model, prompt_tokens, completion_tokens, \
             total_tokens, source FROM usage_events \
             WHERE at >= ?1 AND at < ?2 ORDER BY at, id",
        )?;
        let rows = statement
            .query_map(params![from, to], |row| {
                Ok(UsageEvent {
                    id: row.get(0)?,
                    instance_id: row.get(1)?,
                    at: row.get(2)?,
                    model: row.get(3)?,
                    prompt_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                    completion_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                    total_tokens: row.get::<_, i64>(6)?.max(0) as u64,
                    source: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

// ---------------------------------------------------------------------------
// Pricing
// ---------------------------------------------------------------------------

/// One versioned price row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pricing {
    pub model: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub currency: String,
    /// First day (`YYYY-MM-DD`) the price applies to.
    pub effective_from: String,
    pub updated_at: String,
}

/// Fields accepted by [`set_price`].
#[derive(Debug, Clone)]
pub struct NewPrice {
    pub model: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub currency: String,
    pub effective_from: String,
}

impl NewPrice {
    /// Validate and normalise the fields (`effective_from` becomes a date).
    pub fn validated(mut self) -> Result<Self, CostError> {
        self.model = self.model.trim().to_string();
        if self.model.is_empty() {
            return Err(CostError::InvalidPricing("model is required".to_string()));
        }
        if self.model.chars().count() > 128 {
            return Err(CostError::InvalidPricing(
                "model must be at most 128 characters".to_string(),
            ));
        }
        for (name, value) in [
            ("input_per_mtok", self.input_per_mtok),
            ("output_per_mtok", self.output_per_mtok),
        ] {
            if !value.is_finite() || !(0.0..=MAX_PRICE_PER_MTOK).contains(&value) {
                return Err(CostError::InvalidPricing(format!(
                    "{name} must be a number between 0 and {MAX_PRICE_PER_MTOK}"
                )));
            }
        }
        self.currency = if self.currency.trim().is_empty() {
            "USD".to_string()
        } else {
            self.currency.trim().to_ascii_uppercase()
        };
        if self.currency.len() != 3 || !self.currency.chars().all(|ch| ch.is_ascii_alphabetic()) {
            return Err(CostError::InvalidPricing(
                "currency must be a three-letter code (ISO 4217)".to_string(),
            ));
        }
        self.effective_from = normalize_date(&self.effective_from)?;
        Ok(self)
    }
}

/// Insert (or replace) the price of `model` effective on `effective_from`.
///
/// A new effective date adds a version: historical events keep the price that
/// was effective on their own day.
pub fn set_price(db: &Db, price: NewPrice) -> Result<Pricing, CostError> {
    let price = price.validated()?;
    let updated_at = now_iso();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO model_pricing (model, input_per_mtok, output_per_mtok, currency, \
             effective_from, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (model, effective_from) DO UPDATE SET \
                 input_per_mtok = excluded.input_per_mtok, \
                 output_per_mtok = excluded.output_per_mtok, \
                 currency = excluded.currency, \
                 updated_at = excluded.updated_at",
            params![
                price.model,
                price.input_per_mtok,
                price.output_per_mtok,
                price.currency,
                price.effective_from,
                updated_at,
            ],
        )
    })?;
    Ok(Pricing {
        model: price.model,
        input_per_mtok: price.input_per_mtok,
        output_per_mtok: price.output_per_mtok,
        currency: price.currency,
        effective_from: price.effective_from,
        updated_at,
    })
}

/// Every price row, ordered by model then effective date.
pub fn all_prices(db: &Db) -> Result<Vec<Pricing>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT model, input_per_mtok, output_per_mtok, currency, effective_from, \
             updated_at FROM model_pricing ORDER BY model, effective_from",
        )?;
        let rows = statement
            .query_map([], pricing_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

/// Latest price of every model (what the CLI and the API list).
pub fn list_prices(db: &Db) -> Result<Vec<Pricing>, DbError> {
    let mut latest: BTreeMap<String, Pricing> = BTreeMap::new();
    for price in all_prices(db)? {
        latest.insert(price.model.clone(), price);
    }
    Ok(latest.into_values().collect())
}

/// Delete every price version of one model. `false` when the model is unknown.
pub fn delete_prices(db: &Db, model: &str) -> Result<bool, DbError> {
    let deleted = db.with_conn(|conn| {
        conn.execute("DELETE FROM model_pricing WHERE model = ?1", params![model])
    })?;
    Ok(deleted > 0)
}

/// Price effective on `day` (`YYYY-MM-DD`) for `model`.
pub fn price_for<'a>(prices: &'a [Pricing], model: &str, day: &str) -> Option<&'a Pricing> {
    prices
        .iter()
        .filter(|price| price.model == model && price.effective_from.as_str() <= day)
        .max_by(|left, right| left.effective_from.cmp(&right.effective_from))
}

fn pricing_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Pricing> {
    Ok(Pricing {
        model: row.get(0)?,
        input_per_mtok: row.get(1)?,
        output_per_mtok: row.get(2)?,
        currency: row.get(3)?,
        effective_from: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn normalize_date(value: &str) -> Result<String, CostError> {
    let value = value.trim();
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Ok(date.format("%Y-%m-%d").to_string());
    }
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp
            .with_timezone(&Utc)
            .date_naive()
            .format("%Y-%m-%d")
            .to_string());
    }
    Err(CostError::InvalidPricing(format!(
        "effective_from must be YYYY-MM-DD or an RFC 3339 timestamp, got \"{value}\""
    )))
}

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

/// One configured monthly budget.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Budget {
    pub instance_id: String,
    pub monthly_usd: f64,
    pub enabled: bool,
    pub updated_at: String,
}

/// Budget plus its month-to-date consumption.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BudgetStatus {
    pub instance_id: String,
    /// Month the status is computed for (`YYYY-MM`).
    pub month: String,
    pub monthly_usd: f64,
    pub enabled: bool,
    pub used_usd: f64,
    /// `used / limit * 100`; `0` when the limit is `0`.
    pub percent: f64,
    /// `true` once the used amount reaches the limit.
    pub exceeded: bool,
    /// Linear projection at the current fraction of the month.
    pub projected_month_usd: f64,
    pub updated_at: String,
}

/// One budget crossing (returned once per process and month).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BudgetAlert {
    pub instance_id: String,
    pub month: String,
    pub monthly_usd: f64,
    pub used_usd: f64,
    pub percent: f64,
    pub projected_month_usd: f64,
}

fn validate_budget(monthly_usd: f64) -> Result<(), CostError> {
    if !monthly_usd.is_finite() || !(0.0..=MAX_BUDGET_USD).contains(&monthly_usd) {
        return Err(CostError::InvalidBudget(format!(
            "monthly_usd must be a number between 0 and {MAX_BUDGET_USD}"
        )));
    }
    Ok(())
}

/// Create or replace the budget of one instance.
pub fn set_budget(
    db: &Db,
    instance_id: &str,
    monthly_usd: f64,
    enabled: bool,
) -> Result<Budget, CostError> {
    validate_budget(monthly_usd)?;
    let updated_at = now_iso();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO cost_budgets (instance_id, monthly_usd, enabled, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (instance_id) DO UPDATE SET \
                 monthly_usd = excluded.monthly_usd, \
                 enabled = excluded.enabled, \
                 updated_at = excluded.updated_at",
            params![instance_id, monthly_usd, i64::from(enabled), updated_at],
        )
    })?;
    Ok(Budget {
        instance_id: instance_id.to_string(),
        monthly_usd,
        enabled,
        updated_at,
    })
}

/// Budget of one instance, when configured.
pub fn get_budget(db: &Db, instance_id: &str) -> Result<Option<Budget>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT instance_id, monthly_usd, enabled, updated_at FROM cost_budgets \
             WHERE instance_id = ?1",
            params![instance_id],
            budget_row,
        )
        .optional()
    })
}

/// Every configured budget, sorted by instance id.
pub fn list_budgets(db: &Db) -> Result<Vec<Budget>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT instance_id, monthly_usd, enabled, updated_at FROM cost_budgets \
             ORDER BY instance_id",
        )?;
        let rows = statement
            .query_map([], budget_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

fn budget_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Budget> {
    Ok(Budget {
        instance_id: row.get(0)?,
        monthly_usd: row.get(1)?,
        enabled: row.get::<_, i64>(2)? != 0,
        updated_at: row.get(3)?,
    })
}

/// Month-to-date status of every configured budget.
pub fn budget_statuses(db: &Db, now: DateTime<Utc>) -> Result<Vec<BudgetStatus>, DbError> {
    let budgets = list_budgets(db)?;
    if budgets.is_empty() {
        return Ok(Vec::new());
    }
    let (start, end, fraction) = month_window(now);
    let events = events_between(db, &start, &end)?;
    let prices = all_prices(db)?;
    let mut used: HashMap<String, f64> = HashMap::new();
    for event in &events {
        *used.entry(event.instance_id.clone()).or_default() += event_cost(event, &prices);
    }
    let month = month_label(now);
    Ok(budgets
        .into_iter()
        .map(|budget| {
            let used_usd = used.get(&budget.instance_id).copied().unwrap_or(0.0);
            let percent = if budget.monthly_usd > 0.0 {
                used_usd / budget.monthly_usd * 100.0
            } else {
                0.0
            };
            let exceeded =
                budget.enabled && budget.monthly_usd > 0.0 && used_usd >= budget.monthly_usd;
            BudgetStatus {
                instance_id: budget.instance_id,
                month: month.clone(),
                monthly_usd: budget.monthly_usd,
                enabled: budget.enabled,
                used_usd,
                percent,
                exceeded,
                projected_month_usd: project(used_usd, fraction),
                updated_at: budget.updated_at,
            }
        })
        .collect())
}

/// Budgets that just crossed their limit.
///
/// A crossing is reported once per process and month; when the used amount
/// falls back below the limit the alert is armed again for the next crossing.
pub fn check_budgets(db: &Db, now: DateTime<Utc>) -> Result<Vec<BudgetAlert>, DbError> {
    static ALERTED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
    let statuses = budget_statuses(db, now)?;
    let key = |status: &BudgetStatus| format!("{}\u{1f}{}", status.instance_id, status.month);
    let current: HashSet<String> = statuses
        .iter()
        .filter(|status| status.exceeded)
        .map(key)
        .collect();
    let mut alerted = ALERTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    alerted.retain(|entry| current.contains(entry));
    let mut alerts = Vec::new();
    for status in statuses.into_iter().filter(|status| status.exceeded) {
        if alerted.insert(key(&status)) {
            alerts.push(BudgetAlert {
                instance_id: status.instance_id,
                month: status.month,
                monthly_usd: status.monthly_usd,
                used_usd: status.used_usd,
                percent: status.percent,
                projected_month_usd: status.projected_month_usd,
            });
        }
    }
    Ok(alerts)
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Axis of the cost aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Instance,
    Model,
    Day,
}

impl GroupBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Instance => "instance",
            Self::Model => "model",
            Self::Day => "day",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "instance" | "agent" => Some(Self::Instance),
            "model" => Some(Self::Model),
            "day" | "date" => Some(Self::Day),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CostTotals {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub events: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostGroup {
    /// Instance id, model name or `YYYY-MM-DD` day.
    pub key: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub events: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentCost {
    pub instance_id: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Projection {
    pub month: String,
    /// Fraction of the month elapsed when the report was computed (`0..=1`).
    pub elapsed_fraction: f64,
    pub month_to_date_usd: f64,
    pub projected_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostReport {
    /// Inclusive lower bound, RFC 3339.
    pub from: String,
    /// Exclusive upper bound, RFC 3339.
    pub to: String,
    pub group_by: String,
    /// Currency of the prices; totals assume every price is expressed in it.
    pub currency: String,
    pub totals: CostTotals,
    pub groups: Vec<CostGroup>,
    pub top_agents: Vec<AgentCost>,
    pub budgets: Vec<BudgetStatus>,
    pub projection: Projection,
}

/// Cost report for the current month (default range) or an explicit range.
pub fn report(
    db: &Db,
    from: Option<&str>,
    to: Option<&str>,
    group_by: GroupBy,
) -> Result<CostReport, CostError> {
    report_at(db, from, to, group_by, Utc::now())
}

/// Same as [`report`] with an injectable clock (deterministic tests).
pub fn report_at(
    db: &Db,
    from: Option<&str>,
    to: Option<&str>,
    group_by: GroupBy,
    now: DateTime<Utc>,
) -> Result<CostReport, CostError> {
    let default_from = month_start(now).date_naive().format("%Y-%m-%d").to_string();
    // Include an event stamped in the current second: `to` is exclusive.
    let default_to = now_bound(now);
    let from = normalize_bound(from.unwrap_or(&default_from), false)?;
    let to = normalize_bound(to.unwrap_or(&default_to), true)?;
    if to <= from {
        return Err(CostError::InvalidRange(format!(
            "to ({to}) must be after from ({from})"
        )));
    }

    let events = events_between(db, &from, &to)?;
    let prices = all_prices(db)?;
    let budget_statuses = budget_statuses(db, now)?;

    let mut groups: BTreeMap<String, CostGroup> = BTreeMap::new();
    let mut per_agent: HashMap<String, AgentCost> = HashMap::new();
    let mut totals = CostTotals::default();
    for event in &events {
        let cost = event_cost(event, &prices);
        let key = match group_by {
            GroupBy::Instance => event.instance_id.clone(),
            GroupBy::Model => {
                if event.model.trim().is_empty() {
                    UNKNOWN_MODEL.to_string()
                } else {
                    event.model.clone()
                }
            }
            GroupBy::Day => event.at.get(..10).unwrap_or(event.at.as_str()).to_string(),
        };
        let group = groups.entry(key.clone()).or_insert_with(|| CostGroup {
            key,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cost_usd: 0.0,
            events: 0,
        });
        group.prompt_tokens += event.prompt_tokens;
        group.completion_tokens += event.completion_tokens;
        group.total_tokens += event.total_tokens;
        group.cost_usd += cost;
        group.events += 1;

        let agent = per_agent
            .entry(event.instance_id.clone())
            .or_insert_with(|| AgentCost {
                instance_id: event.instance_id.clone(),
                cost_usd: 0.0,
                total_tokens: 0,
            });
        agent.cost_usd += cost;
        agent.total_tokens += event.total_tokens;

        totals.prompt_tokens += event.prompt_tokens;
        totals.completion_tokens += event.completion_tokens;
        totals.total_tokens += event.total_tokens;
        totals.cost_usd += cost;
        totals.events += 1;
    }

    let mut groups: Vec<CostGroup> = groups.into_values().collect();
    groups.sort_by(|left, right| {
        right
            .cost_usd
            .total_cmp(&left.cost_usd)
            .then_with(|| right.total_tokens.cmp(&left.total_tokens))
            .then_with(|| left.key.cmp(&right.key))
    });
    let mut top_agents: Vec<AgentCost> = per_agent.into_values().collect();
    top_agents.sort_by(|left, right| {
        right
            .cost_usd
            .total_cmp(&left.cost_usd)
            .then_with(|| right.total_tokens.cmp(&left.total_tokens))
            .then_with(|| left.instance_id.cmp(&right.instance_id))
    });
    top_agents.truncate(TOP_AGENTS);

    let (month_from, month_to, fraction) = month_window(now);
    let month_events = events_between(db, &month_from, &month_to)?;
    let month_to_date_usd: f64 = month_events
        .iter()
        .map(|event| event_cost(event, &prices))
        .sum();
    let currency = prices
        .first()
        .map(|price| price.currency.clone())
        .unwrap_or_else(|| "USD".to_string());

    Ok(CostReport {
        from,
        to,
        group_by: group_by.as_str().to_string(),
        currency,
        totals,
        groups,
        top_agents,
        budgets: budget_statuses,
        projection: Projection {
            month: month_label(now),
            elapsed_fraction: fraction,
            month_to_date_usd,
            projected_usd: project(month_to_date_usd, fraction),
        },
    })
}

/// Cost of one event, using the price effective on its day.
pub fn event_cost(event: &UsageEvent, prices: &[Pricing]) -> f64 {
    let day = event.at.get(..10).unwrap_or(event.at.as_str());
    let Some(price) = price_for(prices, &event.model, day) else {
        return 0.0;
    };
    event.prompt_tokens as f64 / 1_000_000.0 * price.input_per_mtok
        + event.completion_tokens as f64 / 1_000_000.0 * price.output_per_mtok
}

/// Normalise a range bound to RFC 3339 UTC.
///
/// A bare `YYYY-MM-DD` covers the whole day; `end` makes it point to the next
/// midnight, so `to = 2026-11-09` includes every event of that day.
pub fn normalize_bound(value: &str, end: bool) -> Result<String, CostError> {
    let value = value.trim();
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Secs, true));
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| {
        CostError::InvalidRange(format!(
            "expected YYYY-MM-DD or an RFC 3339 timestamp, got \"{value}\""
        ))
    })?;
    let date = if end {
        date.succ_opt()
            .ok_or_else(|| CostError::InvalidRange(format!("date \"{value}\" is out of range")))?
    } else {
        date
    };
    let naive = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| CostError::InvalidRange(format!("date \"{value}\" is out of range")))?;
    Ok(naive.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    NaiveDate::from_ymd_opt(now.year(), now.month(), 1)
        .expect("the first day of a month exists")
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists")
        .and_utc()
}

fn month_label(now: DateTime<Utc>) -> String {
    now.format("%Y-%m").to_string()
}

/// `(month start, now, elapsed fraction)` as RFC 3339 bounds and a fraction.
fn month_window(now: DateTime<Utc>) -> (String, String, f64) {
    let start = month_start(now);
    let next = if now.month() == 12 {
        NaiveDate::from_ymd_opt(now.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(now.year(), now.month() + 1, 1)
    }
    .expect("the first day of the next month exists")
    .and_hms_opt(0, 0, 0)
    .expect("midnight exists")
    .and_utc();
    let total = (next - start).num_seconds().max(1) as f64;
    let elapsed = (now - start).num_seconds().clamp(0, total as i64) as f64;
    // A month that just started would project absurdly high numbers; cap the
    // multiplier at 1000 (roughly the first 45 minutes of a 31-day month).
    let fraction = (elapsed / total).clamp(0.001, 1.0);
    (
        start.to_rfc3339_opts(SecondsFormat::Secs, true),
        now_bound(now),
        fraction,
    )
}

fn now_bound(now: DateTime<Utc>) -> String {
    (now + chrono::Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn project(month_to_date_usd: f64, fraction: f64) -> f64 {
    if fraction <= 0.0 {
        return month_to_date_usd;
    }
    month_to_date_usd / fraction
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CostError {
    #[error("{0}")]
    InvalidRange(String),
    #[error("{0}")]
    InvalidPricing(String),
    #[error("{0}")]
    InvalidBudget(String),
    #[error("{0}")]
    InvalidIngest(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

// ---------------------------------------------------------------------------
// Outbound events
// ---------------------------------------------------------------------------

/// Hook for the notification / incident pipeline.
///
/// The notifications module landed after this feature; until it is wired in,
/// an alert is a structured WARN line an operator (or a log-based alert rule)
/// can key on. The payload is stable, so the future pipeline only has to swap
/// the sink, not the call sites.
pub mod events {
    /// Emit one cost event (`cost.budget` for a crossed budget).
    pub fn emit(kind: &str, payload: &serde_json::Value) {
        tracing::warn!(event = kind, payload = %payload, "fleett cost event");
    }
}

// ---------------------------------------------------------------------------
// Background scanner
// ---------------------------------------------------------------------------

/// Outcome of one automatic scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ScanReport {
    pub instances: usize,
    pub scanned: usize,
    pub parsed: usize,
    pub inserted: usize,
    pub ignored: usize,
    pub errors: usize,
}

/// Evaluate the budgets and emit one `cost.budget` event per new crossing.
///
/// Shared by the background scanner and the ingest endpoint, so a budget is
/// reported as soon as the usage that crosses it is recorded.
pub fn emit_budget_alerts(db: &Db) -> Result<Vec<BudgetAlert>, DbError> {
    let alerts = check_budgets(db, Utc::now())?;
    for alert in &alerts {
        events::emit("cost.budget", &serde_json::json!(alert));
    }
    Ok(alerts)
}

/// Tail the logs of every instance, ingest the usage lines and evaluate the
/// budgets. Driver errors are counted, never fatal: a stopped container must
/// not stop the scan of the others.
pub async fn scan_all(state: &AppState) -> ScanReport {
    let mut report = ScanReport::default();
    let specs = match state.db.list() {
        Ok(specs) => specs,
        Err(err) => {
            tracing::warn!(%err, "cost scan could not list the instances");
            report.errors += 1;
            return report;
        }
    };
    report.instances = specs.len();
    let options = LogsOptions {
        tail: LOG_SCAN_TAIL,
        since: None,
    };
    for spec in &specs {
        match state.driver.logs(spec, &options).await {
            Ok(text) => {
                report.scanned += 1;
                match ingest_logs(&state.db, &spec.id, &text, &spec.model) {
                    Ok(summary) => {
                        report.parsed += summary.parsed;
                        report.inserted += summary.inserted;
                        report.ignored += summary.ignored;
                    }
                    Err(err) => {
                        tracing::warn!(instance = %spec.id, %err, "cost scan could not ingest the logs");
                        report.errors += 1;
                    }
                }
            }
            Err(err) => {
                tracing::debug!(instance = %spec.id, %err, "cost scan skipped an instance");
                report.errors += 1;
            }
        }
    }
    match emit_budget_alerts(&state.db) {
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(%err, "cost scan could not evaluate the budgets");
            report.errors += 1;
        }
    }
    report
}

/// Start the background scanner: one scan immediately, then every `interval`.
pub fn spawn_scanner(state: AppState, interval: Duration) {
    if interval.is_zero() {
        return;
    }
    tokio::spawn(async move {
        loop {
            let report = scan_all(&state).await;
            if report.inserted > 0 || report.errors > 0 {
                tracing::info!(
                    instances = report.instances,
                    inserted = report.inserted,
                    errors = report.errors,
                    "cost scan finished"
                );
            }
            tokio::time::sleep(interval).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");
        (dir, db)
    }

    fn usage(prompt: u64, completion: u64) -> NewUsageEvent {
        NewUsageEvent {
            instance_id: "alice".to_string(),
            at: "2026-11-02T10:00:00Z".to_string(),
            model: "model-a".to_string(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            source: Source::Test,
        }
    }

    #[test]
    fn parses_openai_key_value_lines() {
        let parsed = parse_line(
            "2026-11-09T10:00:00Z gateway completed model=deepseek-v4.1-flash \
             prompt_tokens=1200 completion_tokens=300 total_tokens=1500",
        )
        .expect("usage");
        assert_eq!(parsed.prompt_tokens, 1200);
        assert_eq!(parsed.completion_tokens, 300);
        assert_eq!(parsed.total_tokens, 1500);
        assert_eq!(parsed.model.as_deref(), Some("deepseek-v4.1-flash"));
        assert_eq!(parsed.at.as_deref(), Some("2026-11-09T10:00:00Z"));
    }

    #[test]
    fn parses_openai_json_usage_objects() {
        let line = "INFO response: {\"id\":\"x\",\"model\":\"muse-spark\",\
                    \"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\
                    \"total_tokens\":15}}";
        let parsed = parse_line(line).expect("usage");
        assert_eq!(parsed.prompt_tokens, 10);
        assert_eq!(parsed.completion_tokens, 5);
        assert_eq!(parsed.total_tokens, 15);
        assert_eq!(parsed.model.as_deref(), Some("muse-spark"));
    }

    #[test]
    fn parses_anthropic_json_usage_objects_and_nested_payloads() {
        let line = "{\"response\":{\"model\":\"claude-sonnet-4\",\
                    \"usage\":{\"input_tokens\":100,\"output_tokens\":50}}}";
        let parsed = parse_line(line).expect("usage");
        assert_eq!(parsed.prompt_tokens, 100);
        assert_eq!(parsed.completion_tokens, 50);
        assert_eq!(parsed.total_tokens, 150, "total defaults to the sum");
        assert_eq!(parsed.model.as_deref(), Some("claude-sonnet-4"));
    }

    #[test]
    fn parses_python_repr_usage_maps() {
        let line = "turn done usage={'prompt_tokens': 7, 'completion_tokens': 3, \
                    'total_tokens': 10} model='gpt-4o'";
        let parsed = parse_line(line).expect("usage");
        assert_eq!(parsed.prompt_tokens, 7);
        assert_eq!(parsed.completion_tokens, 3);
        assert_eq!(parsed.total_tokens, 10);
        assert_eq!(parsed.model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn ignores_lines_without_usage() {
        for line in [
            "",
            "gateway started",
            "{\"status\":\"ok\"}",
            "prompt_tokens=0 completion_tokens=0",
            "[mock] fleett-alice: image=x status=running",
        ] {
            assert!(parse_line(line).is_none(), "{line:?} parsed");
        }
    }

    #[test]
    fn ingestion_is_idempotent() {
        let (_dir, db) = db();
        let line = "prompt_tokens=100 completion_tokens=25 total_tokens=125 model=model-a";
        let first = ingest_lines(&db, "alice", &[line, line], "default", None, Source::Log)
            .expect("ingest");
        assert_eq!(first.parsed, 2);
        assert_eq!(first.inserted, 1, "the same line is one event");
        assert_eq!(first.duplicates, 1);

        let second =
            ingest_lines(&db, "alice", &[line], "default", None, Source::Log).expect("re-ingest");
        assert_eq!(second.inserted, 0);
        assert_eq!(second.duplicates, 1);

        let events =
            events_between(&db, "0000-01-01T00:00:00Z", "9999-12-31T00:00:00Z").expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].prompt_tokens, 100);
        assert_eq!(events[0].total_tokens, 125);
        assert_eq!(events[0].source, "log");
    }

    #[test]
    fn scan_tail_replay_never_double_counts() {
        let (_dir, db) = db();
        let text = "a\nprompt_tokens=1 completion_tokens=2\nb\n";
        let first = ingest_logs(&db, "alice", text, "model-a").expect("first");
        assert_eq!((first.parsed, first.inserted), (1, 1));
        let second = ingest_logs(&db, "alice", text, "model-a").expect("second");
        assert_eq!((second.parsed, second.inserted), (1, 0));
    }

    fn price(model: &str, input: f64, output: f64, effective_from: &str) -> NewPrice {
        NewPrice {
            model: model.to_string(),
            input_per_mtok: input,
            output_per_mtok: output,
            currency: "USD".to_string(),
            effective_from: effective_from.to_string(),
        }
    }

    #[test]
    fn aggregation_groups_by_instance_model_and_day() {
        let (_dir, db) = db();
        ingest_event(&db, &usage(1_000_000, 500_000)).expect("alice day 2");
        let mut bob = usage(2_000_000, 0);
        bob.instance_id = "bob".to_string();
        bob.at = "2026-11-03T12:00:00Z".to_string();
        bob.model = "model-b".to_string();
        ingest_event(&db, &bob).expect("bob day 3");

        set_price(&db, price("model-a", 2.0, 8.0, "2026-01-01")).expect("price a");
        set_price(&db, price("model-b", 1.0, 1.0, "2026-01-01")).expect("price b");

        let now = DateTime::parse_from_rfc3339("2026-11-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let range = ("2026-11-01", "2026-12-01");

        let by_instance =
            report_at(&db, Some(range.0), Some(range.1), GroupBy::Instance, now).expect("report");
        assert_eq!(by_instance.totals.total_tokens, 3_500_000);
        assert_eq!(by_instance.totals.events, 2);
        assert!((by_instance.totals.cost_usd - 8.0).abs() < 1e-9);
        assert_eq!(by_instance.groups.len(), 2);
        assert_eq!(by_instance.groups[0].key, "alice");
        assert!((by_instance.groups[0].cost_usd - 6.0).abs() < 1e-9);
        assert_eq!(by_instance.top_agents[0].instance_id, "alice");

        let by_model =
            report_at(&db, Some(range.0), Some(range.1), GroupBy::Model, now).expect("report");
        let keys: Vec<&str> = by_model
            .groups
            .iter()
            .map(|group| group.key.as_str())
            .collect();
        assert_eq!(keys, ["model-a", "model-b"]);

        let by_day =
            report_at(&db, Some(range.0), Some(range.1), GroupBy::Day, now).expect("report");
        let keys: Vec<&str> = by_day
            .groups
            .iter()
            .map(|group| group.key.as_str())
            .collect();
        assert_eq!(keys, ["2026-11-02", "2026-11-03"], "cost desc");
    }

    #[test]
    fn prices_are_versioned_by_effective_date() {
        let (_dir, db) = db();
        set_price(&db, price("model-a", 1.0, 1.0, "2026-01-01")).expect("v1");
        set_price(&db, price("model-a", 10.0, 10.0, "2026-11-01")).expect("v2");

        let mut january = usage(1_000_000, 0);
        january.at = "2026-01-15T00:00:00Z".to_string();
        ingest_event(&db, &january).expect("january");
        let mut november = usage(1_000_000, 0);
        november.at = "2026-11-05T00:00:00Z".to_string();
        ingest_event(&db, &november).expect("november");

        let now = DateTime::parse_from_rfc3339("2026-11-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let report = report_at(
            &db,
            Some("2026-01-01"),
            Some("2026-12-01"),
            GroupBy::Day,
            now,
        )
        .expect("report");
        assert!((report.totals.cost_usd - 11.0).abs() < 1e-9);
        let day = |key: &str| {
            report
                .groups
                .iter()
                .find(|group| group.key == key)
                .expect(key)
                .cost_usd
        };
        assert!((day("2026-01-15") - 1.0).abs() < 1e-9, "old price");
        assert!((day("2026-11-05") - 10.0).abs() < 1e-9, "new price");
    }

    #[test]
    fn the_same_effective_day_is_replaced_not_duplicated() {
        let (_dir, db) = db();
        set_price(&db, price("model-a", 1.0, 1.0, "2026-11-01")).expect("v1");
        set_price(&db, price("model-a", 2.0, 2.0, "2026-11-01")).expect("v2");
        let prices = all_prices(&db).expect("prices");
        assert_eq!(prices.len(), 1);
        assert_eq!(prices[0].input_per_mtok, 2.0);
    }

    #[test]
    fn budgets_track_usage_and_project_the_month() {
        let (_dir, db) = db();
        set_budget(&db, "alice", 5.0, true).expect("budget");
        set_price(&db, price("model-a", 1.0, 0.0, "2026-01-01")).expect("price");

        // 2 USD spent by the middle of a 30-day month: 50 % and 4 USD projected.
        let mut event = usage(2_000_000, 0);
        event.at = "2026-11-02T00:00:00Z".to_string();
        ingest_event(&db, &event).expect("event");

        let now = DateTime::parse_from_rfc3339("2026-11-16T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let statuses = budget_statuses(&db, now).expect("statuses");
        assert_eq!(statuses.len(), 1);
        let status = &statuses[0];
        assert_eq!(status.monthly_usd, 5.0);
        assert!((status.used_usd - 2.0).abs() < 1e-9);
        assert!((status.percent - 40.0).abs() < 1e-9);
        assert!(!status.exceeded);
        assert!((status.projected_month_usd - 4.0).abs() < 1e-9);
        assert_eq!(status.month, "2026-11");
    }

    #[test]
    fn crossing_a_budget_is_reported_once() {
        let (_dir, db) = db();
        set_budget(&db, "alice", 1.0, true).expect("budget");
        set_price(&db, price("model-a", 1.0, 0.0, "2026-01-01")).expect("price");
        let mut event = usage(2_000_000, 0);
        event.at = "2026-11-02T00:00:00Z".to_string();
        ingest_event(&db, &event).expect("event");

        let now = DateTime::parse_from_rfc3339("2026-11-16T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let first = check_budgets(&db, now).expect("check");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].instance_id, "alice");
        assert!((first[0].percent - 200.0).abs() < 1e-9);
        let second = check_budgets(&db, now).expect("second check");
        assert!(second.is_empty(), "an alert fires once per month");
    }

    #[test]
    fn bounds_cover_whole_days_and_reject_garbage() {
        assert_eq!(
            normalize_bound("2026-11-09", false).expect("date"),
            "2026-11-09T00:00:00Z"
        );
        assert_eq!(
            normalize_bound("2026-11-09", true).expect("date"),
            "2026-11-10T00:00:00Z"
        );
        assert_eq!(
            normalize_bound("2026-11-09T10:00:00+01:00", false).expect("timestamp"),
            "2026-11-09T09:00:00Z"
        );
        assert!(normalize_bound("yesterday", false).is_err());
        assert!(normalize_bound("2026-13-01", false).is_err());
    }

    #[test]
    fn budgets_validate_their_limits() {
        let (_dir, db) = db();
        assert!(matches!(
            set_budget(&db, "alice", -1.0, true),
            Err(CostError::InvalidBudget(_))
        ));
        assert!(matches!(
            set_budget(&db, "alice", f64::INFINITY, true),
            Err(CostError::InvalidBudget(_))
        ));
        assert!(matches!(
            set_price(&db, price("m", -1.0, 0.0, "2026-01-01")),
            Err(CostError::InvalidPricing(_))
        ));
        assert!(matches!(
            set_price(&db, price("m", 1.0, 0.0, "someday")),
            Err(CostError::InvalidPricing(_))
        ));
        assert!(matches!(
            set_price(&db, price("", 1.0, 0.0, "2026-01-01")),
            Err(CostError::InvalidPricing(_))
        ));
    }

    #[test]
    fn deleting_a_price_removes_every_version() {
        let (_dir, db) = db();
        set_price(&db, price("model-a", 1.0, 1.0, "2026-01-01")).expect("v1");
        set_price(&db, price("model-a", 2.0, 2.0, "2026-11-01")).expect("v2");
        assert!(delete_prices(&db, "model-a").expect("delete"));
        assert!(!delete_prices(&db, "model-a").expect("delete again"));
        assert!(list_prices(&db).expect("list").is_empty());
    }

    #[test]
    fn report_rejects_an_inverted_range() {
        let (_dir, db) = db();
        assert!(matches!(
            report_at(
                &db,
                Some("2026-11-10"),
                Some("2026-11-09"),
                GroupBy::Instance,
                Utc::now()
            ),
            Err(CostError::InvalidRange(_))
        ));
    }
}
