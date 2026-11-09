//! Outbound webhooks (issue #46).
//!
//! A webhook is one URL, a set of subscribed events and an HMAC-SHA256 secret.
//! Every delivery is signed over the exact JSON body that is sent:
//!
//! ```text
//! X-Fleett-Event: instance.started
//! X-Fleett-Delivery: 42
//! X-Fleett-Signature: sha256=<hex HMAC-SHA256(secret, raw_body)>
//! ```
//!
//! Delivery never blocks the API mutation that triggered it: [`emit`] spawns a
//! task, retries a failed endpoint (1 s, 10 s then 60 s) and records every
//! attempt in `webhook_deliveries`. The secret is encrypted with the same
//! AES-256-GCM key as providers and is never returned by any endpoint.
//!
//! Events without a mutation point yet (`incident.*`, `pipeline.run.*`) are
//! accepted by the API and can be published with [`emit`] from the scheduler /
//! watchdog merge; see `docs/WEBHOOKS.md`.

use crate::api::AppState;
use crate::config::Config;
use crate::db::{Db, DbError};
use crate::models::{now_iso, validate_secret_value};
use crate::secrets::{self, SecretsError, SecretsKey};
use hmac::{Hmac, Mac};
use reqwest::Client;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use url::Url;

/// Header carrying the event name.
pub const EVENT_HEADER: &str = "X-Fleett-Event";
/// Header carrying the delivery row id.
pub const DELIVERY_HEADER: &str = "X-Fleett-Delivery";
/// Header carrying the signature (`sha256=<hex>`).
pub const SIGNATURE_HEADER: &str = "X-Fleett-Signature";
/// Prefix of every signature value.
pub const SIGNATURE_PREFIX: &str = "sha256=";
/// Backoff before each retry of a failed delivery (1 s, 10 s, 60 s).
pub const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(10),
    Duration::from_secs(60),
];
/// Attempts of one delivery: the first try plus one per [`RETRY_DELAYS`] entry.
pub const MAX_ATTEMPTS: usize = RETRY_DELAYS.len() + 1;
/// Hard timeout of one HTTP attempt.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest webhook URL accepted.
pub const MAX_URL_LEN: usize = 2048;
/// Longest HMAC secret accepted.
pub const MAX_SECRET_BYTES: usize = 4 * 1024;
/// Largest subscription accepted for one webhook.
pub const MAX_EVENTS: usize = 32;
/// Deliveries returned when `?limit=` is omitted.
pub const DEFAULT_DELIVERIES_LIMIT: usize = 50;
/// Upper bound of one deliveries page.
pub const MAX_DELIVERIES_LIMIT: usize = 200;
/// Event used by `POST /api/webhooks/{id}/test`; not part of [`Event`].
pub const TEST_EVENT: &str = "webhook.test";
/// Field name the secret is encrypted under (binds a blob to this table).
pub const SECRET_FIELD: &str = "webhook-secret";
/// Delivery status: waiting for the first attempt.
pub const STATUS_PENDING: &str = "pending";
/// Delivery status: the endpoint accepted the payload (`2xx`).
pub const STATUS_SENT: &str = "sent";
/// Delivery status: every attempt failed.
pub const STATUS_FAILED: &str = "failed";

/// Every event the control plane can publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    InstanceCreated,
    InstanceStarted,
    InstanceStopped,
    InstanceDeleted,
    EnvChanged,
    UpgradeStarted,
    UpgradeOk,
    UpgradeFailed,
    UpgradeRolledBack,
    IncidentOpen,
    IncidentClosed,
    SecretRotated,
    PipelineRunStarted,
    PipelineRunOk,
    PipelineRunFailed,
}

impl Event {
    /// Every known event, in documentation order.
    pub const ALL: [Event; 15] = [
        Event::InstanceCreated,
        Event::InstanceStarted,
        Event::InstanceStopped,
        Event::InstanceDeleted,
        Event::EnvChanged,
        Event::UpgradeStarted,
        Event::UpgradeOk,
        Event::UpgradeFailed,
        Event::UpgradeRolledBack,
        Event::IncidentOpen,
        Event::IncidentClosed,
        Event::SecretRotated,
        Event::PipelineRunStarted,
        Event::PipelineRunOk,
        Event::PipelineRunFailed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Event::InstanceCreated => "instance.created",
            Event::InstanceStarted => "instance.started",
            Event::InstanceStopped => "instance.stopped",
            Event::InstanceDeleted => "instance.deleted",
            Event::EnvChanged => "env.changed",
            Event::UpgradeStarted => "upgrade.started",
            Event::UpgradeOk => "upgrade.ok",
            Event::UpgradeFailed => "upgrade.failed",
            Event::UpgradeRolledBack => "upgrade.rolled_back",
            Event::IncidentOpen => "incident.open",
            Event::IncidentClosed => "incident.closed",
            Event::SecretRotated => "secret.rotated",
            Event::PipelineRunStarted => "pipeline.run.started",
            Event::PipelineRunOk => "pipeline.run.ok",
            Event::PipelineRunFailed => "pipeline.run.failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Event::ALL
            .into_iter()
            .find(|event| event.as_str() == value.trim())
    }
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Secrets(#[from] SecretsError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("cannot serialize the webhook payload: {0}")]
    Payload(#[from] serde_json::Error),
}

/// One stored webhook, including its encrypted secret columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRecord {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Subscription filters, in insertion order.
    pub events: Vec<String>,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl WebhookRecord {
    /// `true` when the subscription covers one event.
    pub fn subscribes_to(&self, event: &str) -> bool {
        self.events
            .iter()
            .any(|filter| filter_matches(filter, event))
    }

    /// API view: metadata only, never the secret.
    pub fn summary(&self) -> WebhookSummary {
        WebhookSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            url: self.url.clone(),
            events: self.events.clone(),
            enabled: self.enabled,
            // The secret is mandatory, but never echo its bytes.
            has_secret: !self.ciphertext.is_empty(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

/// Public view of a webhook. There is deliberately no secret field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebhookSummary {
    pub id: String,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub enabled: bool,
    pub has_secret: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// One delivery row, including the exact body that was (or will be) sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRecord {
    pub id: i64,
    pub webhook_id: String,
    pub event: String,
    pub payload: String,
    pub status: String,
    pub attempts: u32,
    pub response_status: Option<i64>,
    pub error: Option<String>,
    pub created_at: String,
    pub delivered_at: Option<String>,
}

impl DeliveryRecord {
    pub fn view(&self) -> DeliveryView {
        DeliveryView {
            id: self.id,
            webhook_id: self.webhook_id.clone(),
            event: self.event.clone(),
            payload: serde_json::from_str(&self.payload)
                .unwrap_or_else(|_| Value::String(self.payload.clone())),
            status: self.status.clone(),
            attempts: self.attempts,
            response_status: self.response_status,
            error: self.error.clone(),
            created_at: self.created_at.clone(),
            delivered_at: self.delivered_at.clone(),
        }
    }
}

/// Serialized delivery (payload parsed back to JSON for the API).
#[derive(Debug, Clone, Serialize)]
pub struct DeliveryView {
    pub id: i64,
    pub webhook_id: String,
    pub event: String,
    pub payload: Value,
    pub status: String,
    pub attempts: u32,
    pub response_status: Option<i64>,
    pub error: Option<String>,
    pub created_at: String,
    pub delivered_at: Option<String>,
}

/// Body of `POST /api/webhooks`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateWebhookRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Body of `PATCH /api/webhooks/{id}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PatchWebhookRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Answer of `POST /api/webhooks/{id}/test`.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookTestResponse {
    pub ok: bool,
    pub status: Option<u16>,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a display name (1 to 64 printable characters, no control bytes).
pub fn validate_name(raw: Option<&str>) -> Result<String, WebhookError> {
    let name = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| WebhookError::Invalid("name is required".to_string()))?;
    if name.len() > 64 {
        return Err(WebhookError::Invalid(
            "name must be at most 64 characters".to_string(),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(WebhookError::Invalid(
            "name must not contain control characters".to_string(),
        ));
    }
    Ok(name.to_string())
}

/// Validate an endpoint URL: `http` or `https`, with a host.
pub fn validate_url(raw: Option<&str>) -> Result<String, WebhookError> {
    let url = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| WebhookError::Invalid("url is required".to_string()))?;
    if url.len() > MAX_URL_LEN {
        return Err(WebhookError::Invalid(format!(
            "url must be at most {MAX_URL_LEN} characters"
        )));
    }
    let parsed =
        Url::parse(url).map_err(|err| WebhookError::Invalid(format!("url is invalid: {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(WebhookError::Invalid(
            "url must use http or https".to_string(),
        ));
    }
    if parsed.host_str().is_none() {
        return Err(WebhookError::Invalid("url must name a host".to_string()));
    }
    Ok(url.to_string())
}

/// Validate one subscription filter.
///
/// Accepted forms: `*`, an exact event (`instance.started`) or a prefix
/// wildcard ending in `.*` (`instance.*`, `pipeline.run.*`).
pub fn is_valid_filter(filter: &str) -> bool {
    let filter = filter.trim();
    if filter == "*" {
        return true;
    }
    if let Some(prefix) = filter.strip_suffix(".*") {
        if prefix.is_empty() {
            return false;
        }
        return Event::ALL
            .iter()
            .any(|event| event.as_str().starts_with(&format!("{prefix}.")));
    }
    Event::parse(filter).is_some()
}

/// `true` when a subscription filter covers one event name.
pub fn filter_matches(filter: &str, event: &str) -> bool {
    let filter = filter.trim();
    if filter.is_empty() {
        return false;
    }
    if filter == "*" {
        return true;
    }
    if let Some(prefix) = filter.strip_suffix('*') {
        return event.starts_with(prefix);
    }
    filter == event
}

/// Validate and normalize an event list (trimmed, deduplicated, bounded).
pub fn validate_events(raw: &[String]) -> Result<Vec<String>, WebhookError> {
    let mut events: Vec<String> = Vec::new();
    for entry in raw {
        let filter = entry.trim();
        if filter.is_empty() {
            continue;
        }
        if !is_valid_filter(filter) {
            return Err(WebhookError::Invalid(format!(
                "unknown event or filter \"{filter}\"; use one of the documented events, a \
                 prefix wildcard like \"instance.*\", or \"*\""
            )));
        }
        if !events.iter().any(|existing| existing == filter) {
            events.push(filter.to_string());
        }
    }
    if events.is_empty() {
        return Err(WebhookError::Invalid(
            "events is required: send at least one event or \"*\"".to_string(),
        ));
    }
    if events.len() > MAX_EVENTS {
        return Err(WebhookError::Invalid(format!(
            "at most {MAX_EVENTS} event filters are accepted"
        )));
    }
    Ok(events)
}

/// Validate the HMAC secret before it is encrypted.
pub fn validate_secret(secret: &str) -> Result<(), WebhookError> {
    if secret.trim().is_empty() {
        return Err(WebhookError::Invalid(
            "secret must not be empty".to_string(),
        ));
    }
    if secret.len() > MAX_SECRET_BYTES {
        return Err(WebhookError::Invalid(format!(
            "secret must be at most {MAX_SECRET_BYTES} bytes"
        )));
    }
    validate_secret_value(secret).map_err(WebhookError::Invalid)
}

// ---------------------------------------------------------------------------
// Secret handling (same AES-256-GCM key as providers and instance env)
// ---------------------------------------------------------------------------

/// Encrypt one webhook secret (fresh nonce per call).
pub fn seal(cfg: &Config, secret: &str) -> Result<secrets::Sealed, WebhookError> {
    let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
    let mut map = BTreeMap::new();
    map.insert(SECRET_FIELD.to_string(), secret.to_string());
    Ok(secrets::encrypt_map(&key, &map)?)
}

/// Decrypt the secret of one webhook. Only delivery and the test endpoint call
/// this; the value never leaves the control plane otherwise.
pub fn secret_of(cfg: &Config, record: &WebhookRecord) -> Result<String, WebhookError> {
    let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
    let mut map = secrets::decrypt_map(&key, &record.nonce, &record.ciphertext)?;
    map.remove(SECRET_FIELD).ok_or_else(|| {
        SecretsError::Decrypt(format!(
            "the encrypted webhook value does not hold the field \"{SECRET_FIELD}\""
        ))
        .into()
    })
}

// ---------------------------------------------------------------------------
// Signature
// ---------------------------------------------------------------------------

/// `sha256=<hex HMAC-SHA256(secret, body)>`.
pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("{SIGNATURE_PREFIX}{hex}")
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

const COLUMNS: &str = "id, name, url, events, nonce, ciphertext, enabled, created_at, updated_at";

fn parse_events(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn row_to_webhook(row: &Row<'_>) -> rusqlite::Result<WebhookRecord> {
    let events: String = row.get("events")?;
    Ok(WebhookRecord {
        id: row.get("id")?,
        name: row.get("name")?,
        url: row.get("url")?,
        events: parse_events(&events),
        nonce: row.get("nonce")?,
        ciphertext: row.get("ciphertext")?,
        enabled: row.get::<_, i64>("enabled")? != 0,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn row_to_delivery(row: &Row<'_>) -> rusqlite::Result<DeliveryRecord> {
    Ok(DeliveryRecord {
        id: row.get("id")?,
        webhook_id: row.get("webhook_id")?,
        event: row.get("event")?,
        payload: row.get("payload")?,
        status: row.get("status")?,
        attempts: row.get::<_, i64>("attempts")?.max(0) as u32,
        response_status: row.get("response_status")?,
        error: row.get("error")?,
        created_at: row.get("created_at")?,
        delivered_at: row.get("delivered_at")?,
    })
}

/// Every webhook, sorted by name then id.
pub fn list(db: &Db) -> Result<Vec<WebhookRecord>, DbError> {
    db.with_conn(|conn| {
        let mut statement =
            conn.prepare(&format!("SELECT {COLUMNS} FROM webhooks ORDER BY name, id"))?;
        let rows = statement
            .query_map([], row_to_webhook)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

/// Enabled webhooks only (the ones [`emit`] may deliver to).
pub fn list_enabled(db: &Db) -> Result<Vec<WebhookRecord>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM webhooks WHERE enabled = 1 ORDER BY name, id"
        ))?;
        let rows = statement
            .query_map([], row_to_webhook)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

/// One webhook, or `None`.
pub fn find(db: &Db, id: &str) -> Result<Option<WebhookRecord>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM webhooks WHERE id = ?1"),
            params![id],
            row_to_webhook,
        )
        .optional()
    })
}

/// Insert one webhook and its encrypted secret.
pub fn insert(db: &Db, record: &WebhookRecord) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO webhooks (id, name, url, events, nonce, ciphertext, enabled, created_at, \
             updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.id,
                record.name,
                record.url,
                record.events.join(","),
                record.nonce,
                record.ciphertext,
                i64::from(record.enabled),
                record.created_at,
                record.updated_at,
            ],
        )?;
        Ok(())
    })
}

/// Update one webhook, optionally replacing its secret.
pub fn update(
    db: &Db,
    record: &WebhookRecord,
    secret: Option<(&[u8], &[u8])>,
) -> Result<bool, DbError> {
    db.with_conn(|conn| {
        let updated = match secret {
            Some((nonce, ciphertext)) => conn.execute(
                "UPDATE webhooks SET name = ?2, url = ?3, events = ?4, nonce = ?5, \
                 ciphertext = ?6, enabled = ?7, updated_at = ?8 WHERE id = ?1",
                params![
                    record.id,
                    record.name,
                    record.url,
                    record.events.join(","),
                    nonce,
                    ciphertext,
                    i64::from(record.enabled),
                    record.updated_at,
                ],
            )?,
            None => conn.execute(
                "UPDATE webhooks SET name = ?2, url = ?3, events = ?4, enabled = ?5, \
                 updated_at = ?6 WHERE id = ?1",
                params![
                    record.id,
                    record.name,
                    record.url,
                    record.events.join(","),
                    i64::from(record.enabled),
                    record.updated_at,
                ],
            )?,
        };
        Ok(updated > 0)
    })
}

/// Delete one webhook and its deliveries (the foreign key cascades).
pub fn delete(db: &Db, id: &str) -> Result<bool, DbError> {
    db.with_conn(|conn| {
        let deleted = conn.execute("DELETE FROM webhooks WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    })
}

/// Record a pending delivery, returning its row id.
pub fn create_delivery(
    db: &Db,
    webhook_id: &str,
    event: &str,
    payload: &str,
) -> Result<i64, DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO webhook_deliveries (webhook_id, event, payload, status, attempts, \
             created_at) VALUES (?1, ?2, ?3, ?4, 0, ?5)",
            params![webhook_id, event, payload, STATUS_PENDING, now_iso()],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

/// Replace the body of a delivery once its row id is known.
pub fn set_delivery_payload(db: &Db, id: i64, payload: &str) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE webhook_deliveries SET payload = ?2 WHERE id = ?1",
            params![id, payload],
        )?;
        Ok(())
    })
}

/// One delivery, or `None`.
pub fn delivery(db: &Db, id: i64) -> Result<Option<DeliveryRecord>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, webhook_id, event, payload, status, attempts, response_status, error, \
             created_at, delivered_at FROM webhook_deliveries WHERE id = ?1",
            params![id],
            row_to_delivery,
        )
        .optional()
    })
}

/// Record the outcome of one attempt (still pending: another try follows).
pub fn record_attempt(
    db: &Db,
    id: i64,
    attempts: u32,
    response_status: Option<u16>,
    error: Option<&str>,
) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE webhook_deliveries SET attempts = ?2, response_status = ?3, error = ?4 \
             WHERE id = ?1",
            params![id, attempts, response_status.map(i64::from), error],
        )?;
        Ok(())
    })
}

/// Final state of a delivery (`sent` or `failed`).
pub fn finish_delivery(
    db: &Db,
    id: i64,
    status: &str,
    attempts: u32,
    response_status: Option<u16>,
    error: Option<&str>,
    delivered_at: Option<&str>,
) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE webhook_deliveries SET status = ?2, attempts = ?3, response_status = ?4, \
             error = ?5, delivered_at = ?6 WHERE id = ?1",
            params![
                id,
                status,
                attempts,
                response_status.map(i64::from),
                error,
                delivered_at
            ],
        )?;
        Ok(())
    })
}

/// Deliveries of one webhook, newest first.
pub fn deliveries(db: &Db, webhook_id: &str, limit: usize) -> Result<Vec<DeliveryRecord>, DbError> {
    let limit = limit.clamp(1, MAX_DELIVERIES_LIMIT) as i64;
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, webhook_id, event, payload, status, attempts, response_status, error, \
             created_at, delivered_at FROM webhook_deliveries WHERE webhook_id = ?1 \
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![webhook_id, limit], row_to_delivery)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

fn http_client() -> Result<Client, String> {
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|err| format!("cannot build the webhook HTTP client: {err}"))
}

/// POST one payload once. `Ok(status)` is an HTTP answer (any status);
/// `Err(reason)` is a transport error.
async fn post_once(
    client: &Client,
    url: &str,
    secret: &str,
    event: &str,
    delivery_id: i64,
    body: &str,
) -> Result<u16, String> {
    let signature = sign(secret, body.as_bytes());
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(EVENT_HEADER, event)
        .header(DELIVERY_HEADER, delivery_id.to_string())
        .header(SIGNATURE_HEADER, signature)
        .body(body.to_string())
        .send()
        .await
        .map_err(|err| format!("cannot reach {url}: {err}"))?;
    Ok(response.status().as_u16())
}

fn success_status(status: u16) -> bool {
    (200..=299).contains(&status)
}

/// Attempt one delivery, retrying with the given backoff until it succeeds or
/// [`MAX_ATTEMPTS`] is reached. State is recorded in `webhook_deliveries`.
///
/// Production calls it with [`RETRY_DELAYS`]; tests pass an empty slice to
/// exercise the retry loop without sleeping.
pub async fn deliver(state: &AppState, delivery_id: i64, delays: &[Duration]) {
    let Some(row) = delivery(&state.db, delivery_id).ok().flatten() else {
        tracing::warn!(delivery = delivery_id, "webhook delivery disappeared");
        return;
    };
    let Some(hook) = find(&state.db, &row.webhook_id).ok().flatten() else {
        let message = "the webhook was deleted before the delivery";
        let _ = finish_delivery(
            &state.db,
            delivery_id,
            STATUS_FAILED,
            row.attempts,
            None,
            Some(message),
            None,
        );
        return;
    };
    let secret = match secret_of(&state.cfg, &hook) {
        Ok(secret) => secret,
        Err(err) => {
            let message = format!("cannot decrypt the webhook secret: {err}");
            let _ = finish_delivery(
                &state.db,
                delivery_id,
                STATUS_FAILED,
                row.attempts,
                None,
                Some(&message),
                None,
            );
            tracing::warn!(webhook = %hook.id, %err, "webhook delivery failed");
            return;
        }
    };
    let client = match http_client() {
        Ok(client) => client,
        Err(message) => {
            let _ = finish_delivery(
                &state.db,
                delivery_id,
                STATUS_FAILED,
                row.attempts,
                None,
                Some(&message),
                None,
            );
            return;
        }
    };

    let mut last_status: Option<u16> = None;
    let mut last_error: Option<String> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match post_once(
            &client,
            &hook.url,
            &secret,
            &row.event,
            delivery_id,
            &row.payload,
        )
        .await
        {
            Ok(status) if success_status(status) => {
                let _ = finish_delivery(
                    &state.db,
                    delivery_id,
                    STATUS_SENT,
                    attempt as u32,
                    Some(status),
                    None,
                    Some(&now_iso()),
                );
                return;
            }
            Ok(status) => {
                last_status = Some(status);
                last_error = Some(format!("the endpoint answered HTTP {status}"));
                let _ = record_attempt(
                    &state.db,
                    delivery_id,
                    attempt as u32,
                    Some(status),
                    last_error.as_deref(),
                );
            }
            Err(reason) => {
                last_status = None;
                last_error = Some(reason);
                let _ = record_attempt(
                    &state.db,
                    delivery_id,
                    attempt as u32,
                    None,
                    last_error.as_deref(),
                );
            }
        }
        if attempt < MAX_ATTEMPTS {
            // An empty slice means "no backoff" (tests); production passes
            // RETRY_DELAYS.
            let delay = delays.get(attempt - 1).copied().unwrap_or(Duration::ZERO);
            tokio::time::sleep(delay).await;
        }
    }

    let _ = finish_delivery(
        &state.db,
        delivery_id,
        STATUS_FAILED,
        MAX_ATTEMPTS as u32,
        last_status,
        last_error.as_deref(),
        None,
    );
    tracing::warn!(
        webhook = %hook.id,
        event = %row.event,
        delivery = delivery_id,
        error = last_error.unwrap_or_else(|| "unknown error".to_string()),
        "webhook delivery failed after every attempt"
    );
}

/// Deliver one event to every enabled webhook that subscribes to it, then
/// return the delivery ids. The mutation path uses [`emit`] instead, which
/// spawns this function; tests call it directly with an empty backoff.
pub async fn dispatch(
    state: &AppState,
    event: Event,
    payload: Value,
    delays: &[Duration],
) -> Vec<i64> {
    let hooks = match list_enabled(&state.db) {
        Ok(hooks) => hooks,
        Err(err) => {
            tracing::warn!(%err, "cannot read the webhook subscriptions");
            return Vec::new();
        }
    };
    let mut ids = Vec::new();
    for hook in hooks {
        if !hook.subscribes_to(event.as_str()) {
            continue;
        }
        let id = match create_delivery(&state.db, &hook.id, event.as_str(), "") {
            Ok(id) => id,
            Err(err) => {
                tracing::warn!(webhook = %hook.id, %err, "cannot record the webhook delivery");
                continue;
            }
        };
        let envelope = json!({
            "id": id,
            "event": event.as_str(),
            "at": now_iso(),
            "data": payload,
        });
        match serde_json::to_string(&envelope) {
            Ok(body) => {
                if let Err(err) = set_delivery_payload(&state.db, id, &body) {
                    tracing::warn!(delivery = id, %err, "cannot store the webhook body");
                }
            }
            Err(err) => {
                tracing::warn!(delivery = id, %err, "cannot serialize the webhook body");
            }
        }
        ids.push(id);
        deliver(state, id, delays).await;
    }
    ids
}

/// Publish one event without ever blocking the caller.
///
/// The database read, the HTTP posts and the retries all happen in a spawned
/// task; a slow or dead endpoint never delays the API mutation that emitted
/// the event.
pub fn emit(state: &AppState, event: Event, payload: Value) {
    let state = state.clone();
    tokio::spawn(async move {
        let _ = dispatch(&state, event, payload, &RETRY_DELAYS).await;
    });
}

/// Send one signed `webhook.test` payload and report the result. Not retried:
/// the caller is an operator waiting on the answer.
pub async fn probe(
    state: &AppState,
    record: &WebhookRecord,
) -> Result<WebhookTestResponse, WebhookError> {
    let secret = secret_of(&state.cfg, record)?;
    let started = Instant::now();
    let id = create_delivery(&state.db, &record.id, TEST_EVENT, "")?;
    let envelope = json!({
        "id": id,
        "event": TEST_EVENT,
        "at": now_iso(),
        "data": { "webhook": record.id, "message": "fleett webhook test" },
    });
    let body = serde_json::to_string(&envelope)?;
    set_delivery_payload(&state.db, id, &body)?;

    let client = http_client().map_err(WebhookError::Invalid)?;
    let outcome = post_once(&client, &record.url, &secret, TEST_EVENT, id, &body).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let response = match outcome {
        Ok(status) if success_status(status) => {
            let _ = finish_delivery(
                &state.db,
                id,
                STATUS_SENT,
                1,
                Some(status),
                None,
                Some(&now_iso()),
            );
            WebhookTestResponse {
                ok: true,
                status: Some(status),
                latency_ms,
                error: None,
            }
        }
        Ok(status) => {
            let message = format!("the endpoint answered HTTP {status}");
            let _ = finish_delivery(
                &state.db,
                id,
                STATUS_FAILED,
                1,
                Some(status),
                Some(&message),
                None,
            );
            WebhookTestResponse {
                ok: false,
                status: Some(status),
                latency_ms,
                error: Some(message),
            }
        }
        Err(reason) => {
            let _ = finish_delivery(&state.db, id, STATUS_FAILED, 1, None, Some(&reason), None);
            WebhookTestResponse {
                ok: false,
                status: None,
                latency_ms,
                error: Some(reason),
            }
        }
    };
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_matches_the_documented_vector() {
        // HMAC-SHA256("hello", key "secret"), checked against
        // `printf hello | openssl dgst -sha256 -hmac secret`.
        assert_eq!(
            sign("secret", b"hello"),
            "sha256=88aab3ede8d3adf94d26ab90d3bafd4a2083070c3bcce9c014ee04a443847c0b"
        );
    }

    #[test]
    fn a_signature_changes_with_the_body_and_the_secret() {
        let body = br#"{"event":"instance.started"}"#;
        assert_ne!(sign("one", body), sign("two", body));
        assert_ne!(sign("one", body), sign("one", b"{}"));
    }

    #[test]
    fn every_event_round_trips_and_is_a_valid_filter() {
        for event in Event::ALL {
            assert_eq!(Event::parse(event.as_str()), Some(event));
            assert!(is_valid_filter(event.as_str()), "{}", event.as_str());
        }
        assert_eq!(Event::ALL.len(), 15);
        assert!(Event::parse("nope").is_none());
    }

    #[test]
    fn filters_match_only_their_scope() {
        assert!(filter_matches("*", "instance.created"));
        assert!(filter_matches("instance.*", "instance.created"));
        assert!(filter_matches("instance.*", "instance.deleted"));
        assert!(!filter_matches("instance.*", "env.changed"));
        assert!(filter_matches("pipeline.run.*", "pipeline.run.ok"));
        assert!(!filter_matches("pipeline.run.*", "pipeline.run"));
        assert!(filter_matches("upgrade.ok", "upgrade.ok"));
        assert!(!filter_matches("upgrade.ok", "upgrade.failed"));
        assert!(!filter_matches("", "upgrade.ok"));
    }

    #[test]
    fn invalid_filters_are_refused() {
        assert!(!is_valid_filter("instance"));
        assert!(!is_valid_filter("instance."));
        assert!(!is_valid_filter("*.created"));
        assert!(!is_valid_filter("unknown.event"));

        let error = validate_events(&["instance.created".to_string(), "nope".to_string()])
            .expect_err("unknown event");
        assert!(error.to_string().contains("nope"));
        assert!(validate_events(&[]).is_err());
        assert_eq!(
            validate_events(&[" instance.* ".to_string(), "instance.*".to_string()])
                .expect("normalized"),
            vec!["instance.*".to_string()]
        );
    }

    #[test]
    fn subscriptions_cover_exact_and_wildcard_filters() {
        let record = WebhookRecord {
            id: "wh_1".to_string(),
            name: "n".to_string(),
            url: "https://example.test/hook".to_string(),
            events: vec!["instance.*".to_string(), "upgrade.ok".to_string()],
            nonce: vec![1; 12],
            ciphertext: vec![2; 32],
            enabled: true,
            created_at: now_iso(),
            updated_at: now_iso(),
        };
        assert!(record.subscribes_to("instance.created"));
        assert!(record.subscribes_to("upgrade.ok"));
        assert!(!record.subscribes_to("upgrade.failed"));
        assert!(!record.subscribes_to("env.changed"));
    }

    #[test]
    fn the_public_view_never_contains_the_secret() {
        let record = WebhookRecord {
            id: "wh_1".to_string(),
            name: "n".to_string(),
            url: "https://example.test/hook".to_string(),
            events: vec!["*".to_string()],
            nonce: vec![1; 12],
            ciphertext: b"super-secret-bytes".to_vec(),
            enabled: true,
            created_at: now_iso(),
            updated_at: now_iso(),
        };
        let summary = record.summary();
        let json = serde_json::to_value(&summary).expect("serialize");
        assert_eq!(json["has_secret"], json!(true));
        assert!(json.get("secret").is_none());
        assert!(json.get("ciphertext").is_none());
        let text = json.to_string();
        assert!(!text.contains("super-secret-bytes"));
    }

    #[test]
    fn urls_must_use_http_or_https() {
        assert!(validate_url(Some("https://example.test/hook")).is_ok());
        assert!(validate_url(Some("http://127.0.0.1:9000/hook")).is_ok());
        assert!(validate_url(Some("ftp://example.test/hook")).is_err());
        assert!(validate_url(Some("example.test/hook")).is_err());
        assert!(validate_url(Some("")).is_err());
        assert!(validate_url(None).is_err());
    }

    #[test]
    fn secrets_are_bounded_and_single_line() {
        assert!(validate_secret("0123456789abcdef").is_ok());
        assert!(validate_secret("   ").is_err());
        assert!(validate_secret("line\nbreak").is_err());
        assert!(validate_secret(&"x".repeat(MAX_SECRET_BYTES + 1)).is_err());
    }
}
