//! Log center endpoints (issue #42).
//!
//! `GET /api/logs` searches the persisted lines across instances;
//! `GET /api/logs/stream` forwards new lines as Server-Sent Events and
//! `GET /api/logs/export` downloads the same result set as JSON or CSV.
//! Every route is authenticated by the protected router and served from the
//! redacted rows written by [`crate::logstore`].

use crate::api::error::ApiError;
use crate::api::AppState;
use crate::logstore::{self, LogEntry, LogLevel, LogQuery, LogStoreError};
use crate::models::validate_instance_id;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use futures_util::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::broadcast;

/// Rows returned when `?limit=` is omitted.
pub const DEFAULT_LIMIT: usize = 100;
/// Upper bound of `?limit=`, identical to the log store.
pub const MAX_LIMIT: usize = logstore::MAX_SEARCH_LIMIT;
/// Largest number of instance ids accepted in `?instances=`.
pub const MAX_INSTANCES: usize = 100;
/// Keep-alive interval of the SSE stream.
pub const SSE_KEEPALIVE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogsQuery {
    /// Comma-separated instance ids; empty means every instance.
    #[serde(default)]
    pub instances: Option<String>,
    /// FTS5 query, with a substring fallback.
    #[serde(default)]
    pub q: Option<String>,
    /// Minimum severity: `info`, `warn` or `error`.
    #[serde(default)]
    pub level: Option<String>,
    /// RFC 3339 timestamp or a relative window (`30m`, `24h`, `7d`).
    #[serde(default)]
    pub since: Option<String>,
    /// RFC 3339 timestamp or a relative window.
    #[serde(default)]
    pub until: Option<String>,
    /// Only rows newer than this entry id (live-follow resume cursor).
    #[serde(default, deserialize_with = "deserialize_after_id")]
    pub after_id: Option<i64>,
    /// Page size, `1..=500`.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Export only: `json` (default) or `csv`.
    #[serde(default)]
    pub format: Option<String>,
}

/// Read `?after_id=`.
///
/// A malformed value is normalised to `-1` instead of failing the extractor,
/// so [`build_query`] can answer the documented `invalid_after_id` error rather
/// than an opaque axum query rejection.
fn deserialize_after_id<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    let Some(raw) = raw.map(|value| value.trim().to_string()) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    Ok(Some(raw.parse::<i64>().unwrap_or(-1)))
}

/// Search response envelope.
#[derive(Debug, Clone, Serialize)]
pub struct LogsResponse {
    pub entries: Vec<LogEntry>,
    pub count: usize,
    pub limit: usize,
}

/// Stream filter, parsed from `?instances=`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreamQuery {
    #[serde(default)]
    pub instances: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportFormat {
    Json,
    Csv,
}

impl ExportFormat {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "json" => Some(Self::Json),
            "csv" => Some(Self::Csv),
            _ => None,
        }
    }
}

fn parse_instances(value: Option<&str>) -> Result<Vec<String>, ApiError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut instances = Vec::new();
    for id in value.split(',').map(str::trim).filter(|id| !id.is_empty()) {
        validate_instance_id(id)
            .map_err(|message| ApiError::bad_request("invalid_instance", message))?;
        if !instances.iter().any(|seen| seen == id) {
            instances.push(id.to_string());
        }
    }
    if instances.len() > MAX_INSTANCES {
        return Err(ApiError::bad_request(
            "too_many_instances",
            format!("at most {MAX_INSTANCES} instances can be queried at once"),
        ));
    }
    Ok(instances)
}

fn build_query(query: &LogsQuery) -> Result<LogQuery, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT as u32) as usize;
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_LIMIT}"),
        ));
    }
    let level = match query
        .level
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => Some(LogLevel::parse(value).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_level",
                "level must be \"info\", \"warn\" or \"error\"",
            )
        })?),
        None => None,
    };
    let q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(q) = &q {
        if q.chars().count() > logstore::MAX_QUERY_LEN {
            return Err(ApiError::bad_request(
                "invalid_query",
                format!("q must be at most {} characters", logstore::MAX_QUERY_LEN),
            ));
        }
    }
    let now = Utc::now();
    let since = match query
        .since
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => Some(
            logstore::parse_time_value(value, now)
                .map_err(|err| ApiError::bad_request("invalid_since", err.to_string()))?,
        ),
        None => None,
    };
    let until = match query
        .until
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => Some(
            logstore::parse_time_value(value, now)
                .map_err(|err| ApiError::bad_request("invalid_until", err.to_string()))?,
        ),
        None => None,
    };
    if let (Some(since), Some(until)) = (&since, &until) {
        if since > until {
            return Err(ApiError::bad_request(
                "invalid_window",
                "since must be before until",
            ));
        }
    }
    if query.after_id.is_some_and(|value| value < 0) {
        return Err(ApiError::bad_request(
            "invalid_after_id",
            "after_id must be a non-negative integer",
        ));
    }
    Ok(LogQuery {
        instances: parse_instances(query.instances.as_deref())?,
        q,
        level,
        since,
        until,
        after_id: query.after_id,
        limit,
    })
}

fn log_error(err: LogStoreError) -> ApiError {
    match err {
        LogStoreError::QueryTooLong
        | LogStoreError::InvalidLimit
        | LogStoreError::InvalidTime(_) => {
            ApiError::bad_request("invalid_logs_query", err.to_string())
        }
        LogStoreError::Db(db) => ApiError::from(db),
    }
}

fn respond(entries: Vec<LogEntry>, limit: usize) -> LogsResponse {
    LogsResponse {
        count: entries.len(),
        entries,
        limit,
    }
}

/// `GET /api/logs?instances=&q=&level=&since=&until=&after_id=&limit=`
pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, ApiError> {
    let query = build_query(&query)?;
    let entries = logstore::search(&state.db, &query).map_err(log_error)?;
    Ok(Json(respond(entries, query.limit)))
}

/// `GET /api/logs/instances/{id}` — the same search restricted to one instance.
pub async fn instance_logs(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(mut query): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, ApiError> {
    query.instances = Some(id);
    let query = build_query(&query)?;
    let entries = logstore::search(&state.db, &query).map_err(log_error)?;
    Ok(Json(respond(entries, query.limit)))
}

/// `GET /api/logs/stream?instances=` — SSE feed of the new lines.
pub async fn stream(
    State(state): State<AppState>,
    Query(query): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let instances = parse_instances(query.instances.as_deref())?;
    let receiver = state.logstore.subscribe();
    let events = stream::unfold(
        (receiver, instances),
        |(mut receiver, instances)| async move {
            loop {
                match receiver.recv().await {
                    Ok(entry) => {
                        if instances.is_empty() || instances.iter().any(|id| id == &entry.instance)
                        {
                            let data = serde_json::to_string(&entry).unwrap_or_default();
                            return Some((
                                Ok(Event::default()
                                    .id(entry.id.to_string())
                                    .event("log")
                                    .data(data)),
                                (receiver, instances),
                            ));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "the log SSE client fell behind");
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE)))
}

/// `GET /api/logs/export?format=json|csv` with the same filters as the search.
pub async fn export(
    State(state): State<AppState>,
    Query(query): Query<LogsQuery>,
) -> Result<Response, ApiError> {
    let format = match query
        .format
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => ExportFormat::parse(value).ok_or_else(|| {
            ApiError::bad_request("invalid_format", "format must be \"json\" or \"csv\"")
        })?,
        None => ExportFormat::Json,
    };
    let query = build_query(&query)?;
    let entries = logstore::search(&state.db, &query).map_err(log_error)?;
    match format {
        ExportFormat::Json => Ok((
            [
                (header::CONTENT_TYPE, "application/json"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"fleett-logs.json\"",
                ),
            ],
            Json(entries),
        )
            .into_response()),
        ExportFormat::Csv => Ok((
            [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"fleett-logs.csv\"",
                ),
            ],
            render_csv(&entries),
        )
            .into_response()),
    }
}

/// CSV with a header row; fields are quoted when needed.
pub fn render_csv(entries: &[LogEntry]) -> String {
    let mut out = String::from("at,instance,level,message\n");
    for entry in entries {
        out.push_str(&csv_field(&entry.at));
        out.push(',');
        out.push_str(&csv_field(&entry.instance));
        out.push(',');
        out.push_str(entry.level.as_str());
        out.push(',');
        out.push_str(&csv_field(&entry.message));
        out.push('\n');
    }
    out
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes_only_when_needed() {
        let entries = vec![
            LogEntry {
                id: 1,
                instance: "alice".to_string(),
                at: "2026-11-09T10:00:00.000Z".to_string(),
                level: LogLevel::Error,
                message: "boom".to_string(),
            },
            LogEntry {
                id: 2,
                instance: "bob".to_string(),
                at: "2026-11-09T10:01:00.000Z".to_string(),
                level: LogLevel::Info,
                message: "a, b \"quoted\"".to_string(),
            },
        ];
        let csv = render_csv(&entries);
        assert_eq!(
            csv,
            "at,instance,level,message\n\
             2026-11-09T10:00:00.000Z,alice,error,boom\n\
             2026-11-09T10:01:00.000Z,bob,info,\"a, b \"\"quoted\"\"\"\n"
        );
    }

    #[test]
    fn instance_lists_are_deduplicated_and_validated() {
        let instances = parse_instances(Some("alice,bob,alice")).expect("valid");
        assert_eq!(instances, ["alice", "bob"]);
        assert!(parse_instances(Some("Not An Id")).is_err());
        assert!(parse_instances(None).expect("empty").is_empty());
    }

    #[test]
    fn after_id_must_be_non_negative() {
        let negative = LogsQuery {
            after_id: Some(-1),
            ..LogsQuery::default()
        };
        let err = build_query(&negative).expect_err("negative cursor");
        assert_eq!(err.code, "invalid_after_id");

        let cursor = build_query(&LogsQuery {
            after_id: Some(0),
            ..LogsQuery::default()
        })
        .expect("zero is a valid cursor");
        assert_eq!(cursor.after_id, Some(0));
    }
}
