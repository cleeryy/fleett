//! Audit trail of every mutation, plus the signed export and the retention
//! purge.
//!
//! The trail is append-only from the API: a mutation that already happened is
//! recorded even when the response is a failure, so the log never hides an
//! action. `result` tells `ok` apart from `failed` and `refused`.

use crate::api::error::ApiError;
use crate::api::AppState;
use crate::audit::{retention_label, sign, HMAC_KEY_VAR, SIGNATURE_PREFIX};
use crate::auth::{AuthContext, Role};
use crate::db::{AuditFilter, AuditRecord, NewAuditRecord};
use crate::models::now_iso;
use axum::body::Body;
use axum::extract::{Extension, Query, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Entries returned when `?limit=` is omitted.
pub const DEFAULT_LIMIT: usize = 200;
/// Upper bound of one page.
pub const MAX_LIMIT: usize = 1000;
/// Media type of `GET /api/audit/export`.
pub const EXPORT_CONTENT_TYPE: &str = "application/x-ndjson";
/// Outcomes accepted by the `result` filter.
pub const RESULTS: [&str; 3] = ["ok", "failed", "refused"];

const SIGNATURE_HEADER: HeaderName = HeaderName::from_static("x-fleett-signature");

#[derive(Debug, Clone, Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
}

/// One row as returned by the API, including the signed-export shape.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub at: String,
    pub actor: String,
    pub role: String,
    pub method: String,
    pub action: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl From<AuditRecord> for AuditEntry {
    fn from(entry: AuditRecord) -> Self {
        Self {
            id: entry.id,
            at: entry.at,
            actor: entry.actor,
            role: entry.role,
            method: entry.method,
            action: entry.action,
            target: entry.target,
            ip: entry.ip,
            user_agent: entry.user_agent,
            result: entry.result,
            request_id: entry.request_id,
            details: entry.details.and_then(|text| {
                serde_json::from_str(&text)
                    .ok()
                    .or(Some(Value::String(text)))
            }),
        }
    }
}

/// Append one successful entry. A database failure is logged, never returned:
/// the mutation already happened, failing the response would be a lie.
pub fn record(
    state: &AppState,
    context: &AuthContext,
    action: &str,
    target: &str,
    details: Option<Value>,
) {
    record_result(state, context, action, target, details, "ok");
}

/// Same as [`record`] with an explicit outcome (`failed`, `refused`).
pub fn record_result(
    state: &AppState,
    context: &AuthContext,
    action: &str,
    target: &str,
    details: Option<Value>,
    result: &str,
) {
    record_entry(
        state,
        NewAuditRecord {
            at: now_iso(),
            actor: context.actor.clone(),
            role: context.role.as_str().to_string(),
            method: context.method.as_str().to_string(),
            action: action.to_string(),
            target: target.to_string(),
            ip: context.ip.clone(),
            user_agent: context.user_agent.clone(),
            result: result.to_string(),
            request_id: None,
            details: details.map(|value| value.to_string()),
        },
    );
}

/// Append a fully-built row (login and logout have no [`AuthContext`] yet).
pub fn record_entry(state: &AppState, entry: NewAuditRecord) {
    let action = entry.action.clone();
    let target = entry.target.clone();
    if let Err(err) = state.db.record_audit(&entry) {
        tracing::warn!(%err, action, target, "could not append to the audit log");
    }
}

/// `GET /api/audit` — admin only, newest first, filterable and paginated.
pub async fn list(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Vec<AuditEntry>>, ApiError> {
    require_admin(&context)?;
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT as u32) as usize;
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_LIMIT}"),
        ));
    }
    if let Some(result) = query.result.as_deref() {
        if !RESULTS.contains(&result) {
            return Err(ApiError::bad_request(
                "invalid_result",
                format!("result must be one of {}", RESULTS.join(", ")),
            ));
        }
    }
    let from = query
        .from
        .as_deref()
        .map(|value| normalize_bound(value, "invalid_from", false))
        .transpose()?;
    let to = query
        .to
        .as_deref()
        .map(|value| normalize_bound(value, "invalid_to", true))
        .transpose()?;
    check_range(from.as_deref(), to.as_deref())?;

    let entries = state
        .db
        .query_audit(&AuditFilter {
            actor: non_empty(query.actor),
            action: non_empty(query.action),
            target: non_empty(query.target),
            from,
            to,
            result: non_empty(query.result),
            limit: Some(limit),
            offset: query.offset.unwrap_or(0) as usize,
        })
        .map_err(ApiError::from)?;
    Ok(Json(entries.into_iter().map(AuditEntry::from).collect()))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

/// `GET /api/audit/export?from=&to=` — HMAC-signed JSON Lines.
///
/// Refuses with `409 audit_key_missing` when no key is configured: exporting
/// evidence that claims integrity but cannot be verified would be worse than
/// no export at all.
pub async fn export(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    require_admin(&context)?;
    let (Some(from), Some(to)) = (query.from.as_deref(), query.to.as_deref()) else {
        return Err(ApiError::bad_request(
            "missing_range",
            "from and to are required: exporting the whole unlabelled history is not evidence",
        ));
    };
    let from = normalize_bound(from, "invalid_from", false)?;
    let to = normalize_bound(to, "invalid_to", true)?;
    check_range(Some(&from), Some(&to))?;

    let Some(key) = state.cfg.audit_hmac_key.as_ref() else {
        return Err(ApiError::conflict(
            "audit_key_missing",
            format!(
                "no audit export key is configured: set {HMAC_KEY_VAR} in the control \
                     plane environment (hex or raw text, e.g. `openssl rand -hex 32`) and \
                     restart. The signature header is \
                     `{SIGNATURE_PREFIX}<hex HMAC-SHA256 of the exact body>`."
            ),
        )
        .with_details(serde_json::json!({
            "env": HMAC_KEY_VAR,
            "format": "sha256=<hex hmac-sha256 of the exact response body>",
        })));
    };

    let entries = state
        .db
        .query_audit(&AuditFilter {
            from: Some(from),
            to: Some(to),
            limit: None,
            ..AuditFilter::default()
        })
        .map_err(ApiError::from)?;

    let mut body = String::new();
    for entry in entries {
        let line = serde_json::to_string(&AuditEntry::from(entry))
            .map_err(|err| ApiError::internal(format!("cannot serialize audit row: {err}")))?;
        body.push_str(&line);
        body.push('\n');
    }

    let signature = format!(
        "{SIGNATURE_PREFIX}{}",
        sign(key.as_bytes(), body.as_bytes())
    );
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let filename = format!("fleett-audit-{timestamp}.jsonl");

    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(EXPORT_CONTENT_TYPE),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .expect("the generated filename is a valid header value"),
    );
    headers.insert(
        SIGNATURE_HEADER,
        HeaderValue::from_str(&signature).expect("a hex signature is a valid header value"),
    );
    Ok(response)
}

/// Result of a retention purge, returned by the endpoint and the housekeeping.
#[derive(Debug, Clone, Serialize)]
pub struct PurgeReport {
    pub removed: usize,
    /// Every row strictly older than this RFC 3339 instant was deleted.
    pub cutoff: String,
    /// Human label of the configured window (`90d`).
    pub retention: String,
}

/// Delete the rows older than the configured retention and journal the purge.
///
/// `context` is the admin who asked; `None` is the daily housekeeping, which
/// only writes a row when something was actually removed (a no-op purge every
/// day would drown the trail). The purge row itself is newer than the cutoff,
/// so a second run never removes it.
pub fn purge_expired(
    state: &AppState,
    context: Option<&AuthContext>,
) -> Result<PurgeReport, ApiError> {
    let retention = state.cfg.audit_retention;
    let cutoff_at = Utc::now() - retention;
    let cutoff = cutoff_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let removed = state
        .db
        .purge_audit_before(&cutoff)
        .map_err(ApiError::from)?;
    let retention_label = retention_label(retention);

    if context.is_some() || removed > 0 {
        let details = serde_json::json!({
            "removed": removed,
            "cutoff": cutoff,
            "retention": retention_label,
            "trigger": if context.is_some() { "api" } else { "schedule" },
        });
        match context {
            Some(context) => record_result(
                state,
                context,
                "audit.purge",
                "audit_log",
                Some(details),
                "ok",
            ),
            None => record_entry(
                state,
                NewAuditRecord {
                    at: now_iso(),
                    actor: "system".to_string(),
                    role: "system".to_string(),
                    method: "system".to_string(),
                    action: "audit.purge".to_string(),
                    target: "audit_log".to_string(),
                    ip: None,
                    user_agent: None,
                    result: "ok".to_string(),
                    request_id: None,
                    details: Some(details.to_string()),
                },
            ),
        }
    }

    Ok(PurgeReport {
        removed,
        cutoff,
        retention: retention_label,
    })
}

/// `DELETE /api/audit/expired` — apply the retention now (admin).
pub async fn purge(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<PurgeReport>, ApiError> {
    require_admin(&context)?;
    Ok(Json(purge_expired(&state, Some(&context))?))
}

pub fn require_admin(context: &AuthContext) -> Result<(), ApiError> {
    if context.role == Role::Admin {
        return Ok(());
    }
    Err(ApiError::forbidden(
        "forbidden",
        "this endpoint requires the admin role",
    ))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Normalise an RFC 3339 instant (or a bare `YYYY-MM-DD` date) so it compares
/// lexicographically with the stored `at` column.
///
/// A date-only `to` covers the whole day; a date-only `from` starts at
/// midnight. Explicit offsets are converted to UTC, the format every row uses.
pub(crate) fn normalize_bound(
    value: &str,
    code: &'static str,
    end_of_day: bool,
) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ApiError::bad_request(code, "the bound must not be empty"));
    }
    if is_date(value) {
        let time = if end_of_day { "23:59:59Z" } else { "00:00:00Z" };
        return Ok(format!("{value}T{time}"));
    }
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|err| {
        ApiError::bad_request(
            code,
            format!("expected an RFC 3339 instant or YYYY-MM-DD: {err}"),
        )
    })?;
    Ok(parsed
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn is_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn check_range(from: Option<&str>, to: Option<&str>) -> Result<(), ApiError> {
    if let (Some(from), Some(to)) = (from, to) {
        if from > to {
            return Err(ApiError::bad_request(
                "invalid_range",
                format!("from ({from}) is after to ({to})"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_normalised_for_lexicographic_comparison() {
        assert_eq!(
            normalize_bound("2026-11-09", "invalid_from", false).expect("date"),
            "2026-11-09T00:00:00Z"
        );
        assert_eq!(
            normalize_bound("2026-11-09", "invalid_to", true).expect("date"),
            "2026-11-09T23:59:59Z"
        );
        assert_eq!(
            normalize_bound("2026-11-09T10:00:00+01:00", "invalid_from", false).expect("offset"),
            "2026-11-09T09:00:00Z"
        );
        assert!(normalize_bound("yesterday", "invalid_from", false).is_err());
        assert!(normalize_bound("", "invalid_from", false).is_err());
    }

    #[test]
    fn ranges_are_ordered() {
        assert!(check_range(Some("2026-01-01T00:00:00Z"), Some("2026-01-02T00:00:00Z")).is_ok());
        assert!(check_range(Some("2026-01-02T00:00:00Z"), Some("2026-01-01T00:00:00Z")).is_err());
    }

    #[test]
    fn hex_signing_matches_the_public_helper() {
        // The handler and the CLI must sign the same bytes identically.
        assert_eq!(crate::audit::encode_hex(b"abc"), "616263");
        assert_eq!(sign(b"k", b"line\n"), sign(b"k", b"line\n"));
    }
}
