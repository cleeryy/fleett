//! Outbound alert channels (issue #33).
//!
//! The control plane used to be pull-only: an operator found out that an agent
//! had stopped by looking at the console. This module pushes the same signal to
//! a webhook, Discord, Telegram or a plain SMTP relay, and keeps a delivery
//! journal of every attempt.
//!
//! Design:
//!
//! * a channel row carries a name, a kind, a target and a comma-separated list
//!   of subscriptions (`instance.down`, `incident.*`, `*`, ...). Its optional
//!   secret (the Telegram bot token) is encrypted with the same AES-256-GCM key
//!   as the provider credentials and is never returned by the API;
//! * [`notify`] selects the enabled channels subscribed to the event, coalesces
//!   per channel and event with a five-minute window (the duplicate is recorded
//!   `suppressed`), then sends with three retries (`1s`, `5s`, `30s`);
//! * every attempt updates one `notification_deliveries` row, so a failing
//!   webhook is visible instead of silently lost;
//! * the watchdog, the rolling upgrade and the quota checks call [`emit`] (or
//!   [`spawn_emit`] when the caller must not wait). `POST
//!   /api/instances/{id}/stop` already emits `instance.down`, `start` emits
//!   `instance.up`, a fleet cap refusal emits `quota.exceeded`, and a rolling
//!   upgrade that leaves an instance behind emits `upgrade.failed`.
//!   `egress.drift`, `secret.expiring`, `instance.unhealthy` and `incident.*`
//!   are available for the same call sites as they land.
//!
//! SMTP v1 is deliberately minimal: a plain TCP conversation, no STARTTLS and
//! no authentication. A server that requires TLS fails the delivery with an
//! explicit error; point `target` at a local relay instead.
//!
//! Secrets and token-bearing targets (a Discord webhook URL is a credential)
//! never reach a log line: only the channel id and kind are logged, and
//! [`safe_target`] masks the target for the audit trail.

use crate::config::Config;
use crate::db::{
    Db, DbError, NewNotificationDelivery, NotificationChannelRecord, NotificationDeliveryRecord,
};
use crate::models::{now_iso, InstanceSpec, PROVIDER_SECRET_MAX_BYTES};
use crate::secrets::{self, SecretsError, SecretsKey};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

/// Field name the channel secret is encrypted under. A blob moved from a
/// provider row (or another field) fails to decrypt instead of silently
/// changing a credential.
pub const SECRET_FIELD: &str = "secret";

/// Anti-spam window per channel and event: one delivery per five minutes.
pub const RATE_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Delays between the four attempts (initial send plus three retries).
pub const BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
];

/// Initial attempt plus one per entry of [`BACKOFF`].
pub const MAX_ATTEMPTS: usize = BACKOFF.len() + 1;

/// Hard timeout of one SMTP conversation.
pub const SMTP_TIMEOUT: Duration = Duration::from_secs(15);

/// `MAIL FROM` of an SMTP delivery.
pub const SMTP_FROM_ENV: &str = "FLEETT_SMTP_FROM";
/// `RCPT TO` of an SMTP delivery (defaults to the `MAIL FROM` address).
pub const SMTP_TO_ENV: &str = "FLEETT_SMTP_TO";
/// Address of the console, used to build the link of a message.
pub const PUBLIC_URL_ENV: &str = "FLEETT_PUBLIC_URL";
/// `MAIL FROM` used when [`SMTP_FROM_ENV`] is not set.
pub const DEFAULT_SMTP_FROM: &str = "fleett@localhost";

/// Subscription matching every event.
pub const WILDCARD: &str = "*";
/// Subscription matching every incident event.
pub const INCIDENT_WILDCARD: &str = "incident.*";

pub const KIND_WEBHOOK: &str = "webhook";
pub const KIND_DISCORD: &str = "discord";
pub const KIND_TELEGRAM: &str = "telegram";
pub const KIND_SMTP: &str = "smtp";
/// Channel kinds accepted by the API, in display order.
pub const KINDS: [&str; 4] = [KIND_WEBHOOK, KIND_DISCORD, KIND_TELEGRAM, KIND_SMTP];

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_SENT: &str = "sent";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_SUPPRESSED: &str = "suppressed";

/// Longest channel name accepted by the API.
pub const NAME_MAX: usize = 100;
/// Longest target accepted by the API.
pub const TARGET_MAX: usize = 2048;

#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Secrets(#[from] SecretsError),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// One event the fleet can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    InstanceDown,
    InstanceUnhealthy,
    InstanceUp,
    UpgradeFailed,
    QuotaExceeded,
    EgressDrift,
    SecretExpiring,
    IncidentOpen,
    IncidentClose,
    /// Synthetic event used by `POST /api/notifications/{id}/test`.
    Test,
}

impl Event {
    /// Every concrete event (wildcards excluded), in display order.
    pub const ALL: [Event; 10] = [
        Self::InstanceDown,
        Self::InstanceUnhealthy,
        Self::InstanceUp,
        Self::UpgradeFailed,
        Self::QuotaExceeded,
        Self::EgressDrift,
        Self::SecretExpiring,
        Self::IncidentOpen,
        Self::IncidentClose,
        Self::Test,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::InstanceDown => "instance.down",
            Self::InstanceUnhealthy => "instance.unhealthy",
            Self::InstanceUp => "instance.up",
            Self::UpgradeFailed => "upgrade.failed",
            Self::QuotaExceeded => "quota.exceeded",
            Self::EgressDrift => "egress.drift",
            Self::SecretExpiring => "secret.expiring",
            Self::IncidentOpen => "incident.open",
            Self::IncidentClose => "incident.close",
            Self::Test => "notification.test",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL.into_iter().find(|event| event.as_str() == value)
    }

    pub fn is_incident(self) -> bool {
        matches!(self, Self::IncidentOpen | Self::IncidentClose)
    }
}

/// True when a stored subscription matches one event.
///
/// `*` matches everything, `incident.*` matches every incident event, and any
/// other value is compared with [`Event::as_str`].
pub fn subscription_matches(subscription: &str, event: Event) -> bool {
    let subscription = subscription.trim();
    if subscription == WILDCARD {
        return true;
    }
    if subscription == INCIDENT_WILDCARD {
        return event.is_incident();
    }
    subscription == event.as_str()
}

/// Normalise and validate the subscriptions of a channel.
///
/// Each value may itself be a comma-separated list, so both a JSON array and
/// `--events a,b` are accepted. Unknown events and empty lists are refused;
/// duplicates are dropped while keeping the caller order.
pub fn normalize_events<I, S>(values: I) -> Result<Vec<String>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut events: Vec<String> = Vec::new();
    for value in values {
        for raw in value.as_ref().split(',') {
            let event = raw.trim();
            if event.is_empty() {
                continue;
            }
            if event != WILDCARD && event != INCIDENT_WILDCARD && Event::parse(event).is_none() {
                return Err(format!(
                    "unknown event \"{event}\"; expected one of {}, {WILDCARD} or {INCIDENT_WILDCARD}",
                    Event::ALL
                        .iter()
                        .map(|event| event.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !events.iter().any(|known| known == event) {
                events.push(event.to_string());
            }
        }
    }
    if events.is_empty() {
        return Err("at least one event is required".to_string());
    }
    Ok(events)
}

/// The stored subscriptions of one channel, as a comma-separated string.
pub fn join_events(events: &[String]) -> String {
    events.join(",")
}

/// True when a channel subscribed to `event`.
pub fn subscribed(events: &str, event: Event) -> bool {
    events
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .any(|subscription| subscription_matches(subscription, event))
}

pub fn validate_kind(kind: &str) -> Result<(), String> {
    if KINDS.contains(&kind) {
        return Ok(());
    }
    Err(format!("kind must be one of {}", KINDS.join(", ")))
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > NAME_MAX {
        return Err(format!(
            "name must be between 1 and {NAME_MAX} characters long"
        ));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err("name must not contain control characters".to_string());
    }
    Ok(())
}

/// Validate a target before it is stored. The exact grammar depends on the
/// kind: a URL for `webhook` and `discord`, a chat id for `telegram`, and
/// `host:port` for `smtp`.
pub fn validate_target(kind: &str, target: &str) -> Result<(), String> {
    if target.trim().is_empty() {
        return Err("target is required".to_string());
    }
    if target.chars().count() > TARGET_MAX {
        return Err(format!("target must be at most {TARGET_MAX} characters"));
    }
    if target.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("target must not contain whitespace or control characters".to_string());
    }
    match kind {
        KIND_WEBHOOK | KIND_DISCORD => {
            let parsed = url::Url::parse(target)
                .map_err(|err| format!("target must be a valid URL: {err}"))?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err("target must be an http:// or https:// URL".to_string());
            }
            Ok(())
        }
        KIND_TELEGRAM => {
            if target.trim_start_matches('@').is_empty() {
                return Err("target must be a Telegram chat id or @channel".to_string());
            }
            Ok(())
        }
        KIND_SMTP => {
            let (host, port) = target.rsplit_once(':').ok_or_else(|| {
                "target must be a host:port SMTP relay (for example 127.0.0.1:25)".to_string()
            })?;
            if host.is_empty() {
                return Err("target must declare an SMTP host before the port".to_string());
            }
            port.parse::<u16>()
                .map_err(|_| "target must end with a valid TCP port".to_string())?;
            Ok(())
        }
        other => Err(format!("unknown channel kind \"{other}\"")),
    }
}

/// True when a channel cannot work without a secret.
pub fn secret_required(kind: &str) -> bool {
    kind == KIND_TELEGRAM
}

/// Validate an optional channel secret. Telegram requires a bot token; the
/// other kinds accept no secret or an opaque token (kept for future auth).
pub fn validate_secret(kind: &str, secret: Option<&str>) -> Result<(), String> {
    match secret {
        Some(value) => {
            if value.trim().is_empty() {
                return Err("secret must not be empty".to_string());
            }
            if value.len() > PROVIDER_SECRET_MAX_BYTES {
                return Err(format!(
                    "secret must be at most {} KiB",
                    PROVIDER_SECRET_MAX_BYTES / 1024
                ));
            }
            crate::models::validate_secret_value(value)
        }
        None if secret_required(kind) => {
            Err("a Telegram channel requires the bot token as its secret".to_string())
        }
        None => Ok(()),
    }
}

/// Mask a target for a log or the audit trail. A webhook or Discord URL often
/// carries a token in its path, so only the scheme and host are kept.
pub fn safe_target(kind: &str, target: &str) -> String {
    match kind {
        KIND_WEBHOOK | KIND_DISCORD => match url::Url::parse(target) {
            Ok(parsed) => format!(
                "{}://{}/…",
                parsed.scheme(),
                parsed.host_str().unwrap_or("unknown")
            ),
            Err(_) => "…".to_string(),
        },
        _ => target.to_string(),
    }
}

/// Base URL of the console, without a trailing slash.
///
/// `FLEETT_PUBLIC_URL` wins, then the public URL configured for OIDC. Without
/// either, messages carry a relative `/instances/<id>` link.
pub fn public_base(cfg: &Config) -> Option<String> {
    std::env::var(PUBLIC_URL_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            cfg.oidc
                .as_ref()
                .map(|oidc| oidc.public_url.trim_end_matches('/').to_string())
        })
}

/// Encrypt one channel secret with the fleet key.
pub fn seal_secret(cfg: &Config, secret: &str) -> Result<secrets::Sealed, NotifyError> {
    let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
    let mut map = BTreeMap::new();
    map.insert(SECRET_FIELD.to_string(), secret.to_string());
    Ok(secrets::encrypt_map(&key, &map)?)
}

/// Decrypt the optional secret of one channel.
pub fn channel_secret(
    cfg: &Config,
    record: &NotificationChannelRecord,
) -> Result<Option<String>, NotifyError> {
    let (Some(nonce), Some(ciphertext)) = (&record.nonce, &record.ciphertext) else {
        return Ok(None);
    };
    let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
    let mut map = secrets::decrypt_map(&key, nonce, ciphertext)?;
    let secret = map.remove(SECRET_FIELD).ok_or_else(|| {
        NotifyError::Secrets(SecretsError::Decrypt(format!(
            "the encrypted channel value does not hold the field \"{SECRET_FIELD}\""
        )))
    })?;
    Ok(Some(secret))
}

/// Data carried by one event, rendered by [`message`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Payload {
    pub instance_id: Option<String>,
    pub instance_name: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub details: Option<String>,
}

impl Payload {
    /// Payload describing one instance at its current recorded status.
    pub fn instance(spec: &InstanceSpec) -> Self {
        Self {
            instance_id: Some(spec.id.clone()),
            instance_name: Some(spec.name.clone()),
            state: Some(spec.status.as_str().to_string()),
            error: spec.last_error.clone(),
            details: None,
        }
    }

    pub fn with_state(mut self, state: impl Into<String>) -> Self {
        self.state = Some(state.into());
        self
    }

    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }
}

/// Subject and body of one alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub subject: String,
    pub body: String,
}

/// Render the short message of one event.
///
/// The body always carries the instance id, its name when it differs, the
/// state, the last error when present, and a console link (absolute with
/// `FLEETT_PUBLIC_URL`, relative otherwise).
pub fn message(event: Event, payload: &Payload, public_base: Option<&str>) -> Message {
    let subject = match payload.instance_id.as_deref() {
        Some(id) => format!("fleett: {} - {id}", event.as_str()),
        None => format!("fleett: {}", event.as_str()),
    };
    let mut lines: Vec<String> = Vec::new();
    if let Some(id) = payload.instance_id.as_deref() {
        lines.push(format!("instance: {id}"));
    }
    if let Some(name) = payload.instance_name.as_deref() {
        if Some(name) != payload.instance_id.as_deref() {
            lines.push(format!("name: {name}"));
        }
    }
    if let Some(state) = payload.state.as_deref() {
        lines.push(format!("state: {state}"));
    }
    if let Some(error) = payload
        .error
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        lines.push(format!("last error: {error}"));
    }
    if let Some(details) = payload
        .details
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        lines.push(format!("details: {details}"));
    }
    if let Some(id) = payload.instance_id.as_deref() {
        match public_base {
            Some(base) => lines.push(format!(
                "console: {}/instances/{id}",
                base.trim_end_matches('/')
            )),
            None => lines.push(format!("console: /instances/{id}")),
        }
    }
    Message {
        subject,
        body: lines.join("\n"),
    }
}

/// Start of the coalescing window, in the same RFC 3339 form as [`now_iso`].
pub fn coalescing_cutoff() -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(RATE_WINDOW.as_secs() as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Process one event: select the channels, coalesce and deliver.
///
/// Returns one row per channel touched (sent, failed or suppressed). A send
/// failure is recorded in the journal, never returned as an error; only a
/// database or decryption failure of the control plane itself bubbles up.
pub async fn notify(
    cfg: &Config,
    db: &Db,
    http: &reqwest::Client,
    event: Event,
    payload: &Payload,
) -> Result<Vec<NotificationDeliveryRecord>, NotifyError> {
    notify_with_backoff(cfg, db, http, event, payload, &BACKOFF).await
}

/// Same as [`notify`] with an explicit backoff (tests use no delay).
pub async fn notify_with_backoff(
    cfg: &Config,
    db: &Db,
    http: &reqwest::Client,
    event: Event,
    payload: &Payload,
    backoff: &[Duration],
) -> Result<Vec<NotificationDeliveryRecord>, NotifyError> {
    let channels = db.list_notification_channels()?;
    let base = public_base(cfg);
    let message = message(event, payload, base.as_deref());
    let cutoff = coalescing_cutoff();
    let mut rows = Vec::new();
    for channel in channels {
        if !channel.enabled || !subscribed(&channel.events, event) {
            continue;
        }
        if let Some(previous) =
            db.recent_notification_delivery(&channel.id, event.as_str(), &cutoff)?
        {
            rows.push(suppressed_row(db, &channel, event, &message, previous)?);
            continue;
        }
        rows.push(deliver(cfg, db, http, &channel, event, payload, &message, backoff).await?);
    }
    Ok(rows)
}

/// Send one synthetic event to one channel, bypassing its subscriptions and
/// the coalescing window. Used by `POST /api/notifications/{id}/test`.
pub async fn test_channel(
    cfg: &Config,
    db: &Db,
    http: &reqwest::Client,
    channel: &NotificationChannelRecord,
) -> Result<NotificationDeliveryRecord, NotifyError> {
    let payload = Payload {
        details: Some("synthetic event sent by the console test button".to_string()),
        ..Payload::default()
    };
    let message = message(Event::Test, &payload, public_base(cfg).as_deref());
    deliver(
        cfg,
        db,
        http,
        channel,
        Event::Test,
        &payload,
        &message,
        &BACKOFF,
    )
    .await
}

/// Fire-and-forget variant of [`emit`] for API handlers: the response never
/// waits for a slow webhook.
pub fn spawn_emit(state: crate::api::AppState, event: Event, payload: Payload) {
    tokio::spawn(async move {
        emit(&state, event, payload).await;
    });
}

/// Emit one event from a module that owns an [`crate::api::AppState`].
///
/// Errors are logged (without the target) and swallowed: a broken notifier must
/// never fail the operation that triggered it.
pub async fn emit(state: &crate::api::AppState, event: Event, payload: Payload) {
    match notify(&state.cfg, &state.db, &state.http, event, &payload).await {
        Ok(rows) => {
            tracing::debug!(
                event = event.as_str(),
                deliveries = rows.len(),
                "notification event processed"
            );
        }
        Err(err) => {
            tracing::warn!(event = event.as_str(), %err, "could not process the notification event");
        }
    }
}

fn suppressed_row(
    db: &Db,
    channel: &NotificationChannelRecord,
    event: Event,
    message: &Message,
    previous: i64,
) -> Result<NotificationDeliveryRecord, NotifyError> {
    let created_at = now_iso();
    let error = format!(
        "suppressed: delivery #{previous} was already sent within the last {} minutes",
        RATE_WINDOW.as_secs() / 60
    );
    let id = db.insert_notification_delivery(&NewNotificationDelivery {
        channel_id: channel.id.clone(),
        event: event.as_str().to_string(),
        subject: message.subject.clone(),
        status: STATUS_SUPPRESSED.to_string(),
        attempts: 0,
        error: Some(error.clone()),
        created_at: created_at.clone(),
        delivered_at: None,
    })?;
    Ok(NotificationDeliveryRecord {
        id,
        channel_id: channel.id.clone(),
        event: event.as_str().to_string(),
        subject: message.subject.clone(),
        status: STATUS_SUPPRESSED.to_string(),
        attempts: 0,
        error: Some(error),
        created_at,
        delivered_at: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn deliver(
    cfg: &Config,
    db: &Db,
    http: &reqwest::Client,
    channel: &NotificationChannelRecord,
    event: Event,
    payload: &Payload,
    message: &Message,
    backoff: &[Duration],
) -> Result<NotificationDeliveryRecord, NotifyError> {
    let created_at = now_iso();
    let id = db.insert_notification_delivery(&NewNotificationDelivery {
        channel_id: channel.id.clone(),
        event: event.as_str().to_string(),
        subject: message.subject.clone(),
        status: STATUS_PENDING.to_string(),
        attempts: 0,
        error: None,
        created_at: created_at.clone(),
        delivered_at: None,
    })?;

    let secret = match channel_secret(cfg, channel) {
        Ok(secret) => secret,
        Err(err) => {
            let error = format!("cannot read the channel secret: {err}");
            db.update_notification_delivery(id, STATUS_FAILED, 0, Some(&error), None)?;
            return Ok(NotificationDeliveryRecord {
                id,
                channel_id: channel.id.clone(),
                event: event.as_str().to_string(),
                subject: message.subject.clone(),
                status: STATUS_FAILED.to_string(),
                attempts: 0,
                error: Some(error),
                created_at,
                delivered_at: None,
            });
        }
    };

    let mut attempts = 0usize;
    let mut last_error: Option<String> = None;
    for attempt in 0..=backoff.len() {
        attempts += 1;
        match send_once(
            http,
            channel,
            secret.as_deref(),
            event,
            payload,
            message,
            &created_at,
        )
        .await
        {
            Ok(()) => {
                let delivered_at = now_iso();
                db.update_notification_delivery(
                    id,
                    STATUS_SENT,
                    attempts,
                    None,
                    Some(&delivered_at),
                )?;
                tracing::debug!(
                    channel = %channel.id,
                    kind = %channel.kind,
                    event = event.as_str(),
                    attempts,
                    "notification delivered"
                );
                return Ok(NotificationDeliveryRecord {
                    id,
                    channel_id: channel.id.clone(),
                    event: event.as_str().to_string(),
                    subject: message.subject.clone(),
                    status: STATUS_SENT.to_string(),
                    attempts,
                    error: None,
                    created_at,
                    delivered_at: Some(delivered_at),
                });
            }
            Err(error) => {
                last_error = Some(error);
                db.update_notification_delivery(
                    id,
                    STATUS_PENDING,
                    attempts,
                    last_error.as_deref(),
                    None,
                )?;
                if let Some(delay) = backoff.get(attempt) {
                    if !delay.is_zero() {
                        tokio::time::sleep(*delay).await;
                    }
                }
            }
        }
    }

    let error = last_error.unwrap_or_else(|| "the notification could not be delivered".to_string());
    db.update_notification_delivery(id, STATUS_FAILED, attempts, Some(&error), None)?;
    tracing::warn!(
        channel = %channel.id,
        kind = %channel.kind,
        event = event.as_str(),
        attempts,
        %error,
        "notification delivery failed"
    );
    Ok(NotificationDeliveryRecord {
        id,
        channel_id: channel.id.clone(),
        event: event.as_str().to_string(),
        subject: message.subject.clone(),
        status: STATUS_FAILED.to_string(),
        attempts,
        error: Some(error),
        created_at,
        delivered_at: None,
    })
}

async fn send_once(
    http: &reqwest::Client,
    channel: &NotificationChannelRecord,
    secret: Option<&str>,
    event: Event,
    payload: &Payload,
    message: &Message,
    at: &str,
) -> Result<(), String> {
    match channel.kind.as_str() {
        KIND_WEBHOOK => send_webhook(http, channel, payload, message, event, at).await,
        KIND_DISCORD => send_discord(http, channel, message).await,
        KIND_TELEGRAM => send_telegram(http, channel, secret, message).await,
        KIND_SMTP => send_smtp(channel, message).await,
        other => Err(format!("unknown channel kind \"{other}\"")),
    }
}

async fn send_webhook(
    http: &reqwest::Client,
    channel: &NotificationChannelRecord,
    payload: &Payload,
    message: &Message,
    event: Event,
    at: &str,
) -> Result<(), String> {
    let instance = match payload.instance_id.as_deref() {
        Some(id) => json!({
            "id": id,
            "name": payload.instance_name,
            "state": payload.state,
        }),
        None => Value::Null,
    };
    let body = json!({
        "event": event.as_str(),
        "at": at,
        "instance": instance,
        "message": message.body,
        "details": payload.details,
    });
    let response = http
        .post(&channel.target)
        .json(&body)
        .send()
        .await
        .map_err(|err| transport_error("the webhook", &err))?;
    expect_http_success("the webhook", response.status()).await
}

async fn send_discord(
    http: &reqwest::Client,
    channel: &NotificationChannelRecord,
    message: &Message,
) -> Result<(), String> {
    // Discord refuses a `content` longer than 2000 characters.
    let content = truncate(&message.body, 1900);
    let response = http
        .post(&channel.target)
        .json(&json!({ "content": content }))
        .send()
        .await
        .map_err(|err| transport_error("Discord", &err))?;
    expect_http_success("Discord", response.status()).await
}

async fn send_telegram(
    http: &reqwest::Client,
    channel: &NotificationChannelRecord,
    secret: Option<&str>,
    message: &Message,
) -> Result<(), String> {
    let token = secret
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "the Telegram channel has no bot token".to_string())?;
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let text = truncate(&message.body, 4000);
    let response = http
        .post(&url)
        .json(&json!({
            "chat_id": channel.target,
            "text": text,
            "disable_web_page_preview": true,
        }))
        .send()
        .await
        .map_err(|err| transport_error("Telegram", &err))?;
    expect_http_success("Telegram", response.status()).await
}

/// A reqwest error formats its URL, which may contain a webhook or bot token.
/// Only the failure category is kept: the delivery journal is API-visible and
/// the message is logged.
fn transport_error(service: &str, err: &reqwest::Error) -> String {
    if err.is_timeout() {
        format!("{service} timed out")
    } else if err.is_connect() {
        format!("cannot reach {service}")
    } else {
        format!("{service} request failed")
    }
}

async fn expect_http_success(service: &str, status: reqwest::StatusCode) -> Result<(), String> {
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("{service} answered with HTTP {status}"))
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Minimal SMTP (plain TCP, no STARTTLS, no authentication)
// ---------------------------------------------------------------------------

async fn send_smtp(channel: &NotificationChannelRecord, message: &Message) -> Result<(), String> {
    let from = std::env::var(SMTP_FROM_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_SMTP_FROM.to_string());
    let to = std::env::var(SMTP_TO_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| from.clone());
    for address in [&from, &to] {
        if address
            .chars()
            .any(|c| matches!(c, '\r' | '\n' | '<' | '>'))
        {
            return Err(format!(
                "invalid SMTP address \"{address}\": set {SMTP_FROM_ENV} and {SMTP_TO_ENV}"
            ));
        }
    }
    let target = channel.target.clone();
    tokio::time::timeout(
        SMTP_TIMEOUT,
        smtp_conversation(&target, &from, &to, message),
    )
    .await
    .map_err(|_| {
        format!(
            "the SMTP server did not answer within {}s",
            SMTP_TIMEOUT.as_secs()
        )
    })?
}

async fn smtp_conversation(
    target: &str,
    from: &str,
    to: &str,
    message: &Message,
) -> Result<(), String> {
    let stream = TcpStream::connect(target)
        .await
        .map_err(|err| format!("cannot reach the SMTP relay: {err}"))?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let (code, text) = smtp_read_reply(&mut reader).await?;
    if code != 220 {
        return Err(smtp_reply_error(code, &text));
    }

    smtp_write(&mut write_half, "EHLO fleett").await?;
    let (code, _text) = smtp_read_reply(&mut reader).await?;
    if code != 250 {
        // An old relay may not speak ESMTP at all.
        smtp_write(&mut write_half, "HELO fleett").await?;
        let (code, text) = smtp_read_reply(&mut reader).await?;
        if code != 250 {
            return Err(smtp_reply_error(code, &text));
        }
    }

    for command in [format!("MAIL FROM:<{from}>"), format!("RCPT TO:<{to}>")] {
        smtp_write(&mut write_half, &command).await?;
        let (code, text) = smtp_read_reply(&mut reader).await?;
        if !matches!(code, 250 | 251) {
            return Err(smtp_reply_error(code, &text));
        }
    }

    smtp_write(&mut write_half, "DATA").await?;
    let (code, text) = smtp_read_reply(&mut reader).await?;
    if code != 354 {
        return Err(smtp_reply_error(code, &text));
    }

    let data = smtp_data(from, to, message);
    write_half
        .write_all(data.as_bytes())
        .await
        .map_err(|err| format!("cannot write the SMTP message: {err}"))?;
    write_half
        .write_all(b".\r\n")
        .await
        .map_err(|err| format!("cannot write the SMTP end of data: {err}"))?;
    write_half
        .flush()
        .await
        .map_err(|err| format!("cannot flush the SMTP message: {err}"))?;
    let (code, text) = smtp_read_reply(&mut reader).await?;
    if code != 250 {
        return Err(smtp_reply_error(code, &text));
    }

    smtp_write(&mut write_half, "QUIT").await?;
    let _ = smtp_read_reply(&mut reader).await;
    Ok(())
}

async fn smtp_write(writer: &mut OwnedWriteHalf, line: &str) -> Result<(), String> {
    writer
        .write_all(format!("{line}\r\n").as_bytes())
        .await
        .map_err(|err| format!("cannot write the SMTP command: {err}"))?;
    writer
        .flush()
        .await
        .map_err(|err| format!("cannot flush the SMTP command: {err}"))
}

async fn smtp_read_reply(reader: &mut BufReader<OwnedReadHalf>) -> Result<(u16, String), String> {
    let mut code = 0u16;
    let mut text = String::new();
    for _ in 0..20 {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .await
            .map_err(|err| format!("cannot read the SMTP reply: {err}"))?;
        if read == 0 {
            return Err("the SMTP server closed the connection".to_string());
        }
        let clean = line.trim_end_matches(['\r', '\n']);
        if let Some(prefix) = clean.get(..3) {
            if let Ok(parsed) = prefix.parse::<u16>() {
                code = parsed;
            }
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(clean.get(4..).unwrap_or("").trim());
        // `250-...` announces a continuation; `250 ...` ends the reply.
        if clean.as_bytes().get(3) != Some(&b'-') {
            return Ok((code, text));
        }
    }
    Err("the SMTP server sent too many continuation lines".to_string())
}

fn smtp_reply_error(code: u16, text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if code == 530 || lower.contains("starttls") || lower.contains("tls required") {
        format!(
            "the SMTP server requires TLS; fleett speaks plain SMTP only in v1 \
             (point the channel at a local relay): {code} {text}"
        )
    } else {
        format!("the SMTP server answered {code}: {text}")
    }
}

fn smtp_data(from: &str, to: &str, message: &Message) -> String {
    let subject = message.subject.replace(['\r', '\n'], " ");
    let mut data = String::new();
    data.push_str(&format!("From: {from}\r\n"));
    data.push_str(&format!("To: {to}\r\n"));
    data.push_str(&format!("Subject: {subject}\r\n"));
    data.push_str(&format!("Date: {}\r\n", now_iso()));
    data.push_str("MIME-Version: 1.0\r\n");
    data.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    data.push_str("\r\n");
    for line in message.body.lines() {
        if line.starts_with('.') {
            data.push('.');
        }
        data.push_str(line);
        data.push_str("\r\n");
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::models::{ChannelKind, Status};
    use crate::store;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const KEY_BYTES: [u8; 32] = [5u8; 32];

    fn setup() -> (tempfile::TempDir, Config, Db) {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().join("root"));
        store::ensure_layout(&cfg).expect("layout");
        cfg.secrets_key = Some(secrets::encode_key(&KEY_BYTES));
        let db = Db::open(&cfg.db_path()).expect("db");
        (dir, cfg, db)
    }

    fn channel(id: &str, kind: &str, target: &str, events: &str) -> NotificationChannelRecord {
        let now = now_iso();
        NotificationChannelRecord {
            id: id.to_string(),
            name: id.to_string(),
            kind: kind.to_string(),
            target: target.to_string(),
            nonce: None,
            ciphertext: None,
            events: events.to_string(),
            enabled: true,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    fn instance(id: &str) -> InstanceSpec {
        let now = now_iso();
        InstanceSpec {
            id: id.to_string(),
            name: format!("Agent {id}"),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://provider.test/v1".to_string(),
            api_key_env: "K".to_string(),
            provider: None,
            channel: ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: 512,
            cpu_limit: 1.0,
            image: "agent:1".to_string(),
            previous_image: None,
            allow_lan: false,
            notes: String::new(),
            status: Status::Running,
            last_error: Some("boom".to_string()),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    #[test]
    fn events_parse_and_match_subscriptions() {
        for event in Event::ALL {
            assert_eq!(Event::parse(event.as_str()), Some(event));
        }
        assert_eq!(Event::parse("nope"), None);
        assert_eq!(Event::parse("incident.*"), None);

        assert!(subscription_matches("*", Event::QuotaExceeded));
        assert!(subscription_matches("incident.*", Event::IncidentOpen));
        assert!(subscription_matches("incident.*", Event::IncidentClose));
        assert!(!subscription_matches("incident.*", Event::InstanceDown));
        assert!(subscription_matches("instance.down", Event::InstanceDown));
        assert!(!subscription_matches("instance.down", Event::InstanceUp));
        assert!(subscribed("instance.up,instance.down", Event::InstanceDown));
        assert!(!subscribed("incident.*", Event::Test));
        assert!(subscribed(" * ", Event::Test));
    }

    #[test]
    fn subscriptions_are_normalised_and_refused_when_unknown() {
        let events =
            normalize_events(["instance.down, instance.up", "incident.*", "instance.down"])
                .expect("valid");
        assert_eq!(events, vec!["instance.down", "instance.up", "incident.*"]);
        assert_eq!(join_events(&events), "instance.down,instance.up,incident.*");

        let err = normalize_events(["instance.down", "nope"]).expect_err("unknown");
        assert!(err.contains("nope"), "{err}");
        assert!(normalize_events([""]).is_err());
    }

    #[test]
    fn targets_and_secrets_follow_the_kind() {
        assert!(validate_kind(KIND_DISCORD).is_ok());
        assert!(validate_kind("email").is_err());

        assert!(validate_target(KIND_WEBHOOK, "https://example.test/hook").is_ok());
        assert!(validate_target(KIND_WEBHOOK, "ftp://example.test/hook").is_err());
        assert!(validate_target(KIND_DISCORD, "not a url").is_err());
        assert!(validate_target(KIND_TELEGRAM, "12345").is_ok());
        assert!(validate_target(KIND_SMTP, "127.0.0.1:25").is_ok());
        assert!(validate_target(KIND_SMTP, "127.0.0.1").is_err());

        assert!(validate_secret(KIND_TELEGRAM, None).is_err());
        assert!(validate_secret(KIND_TELEGRAM, Some("123:abc")).is_ok());
        assert!(validate_secret(KIND_WEBHOOK, None).is_ok());
        assert!(validate_secret(KIND_WEBHOOK, Some(" ")).is_err());
    }

    #[test]
    fn the_message_carries_the_instance_error_and_console_link() {
        let spec = instance("alice");
        let payload = Payload::instance(&spec);
        assert_eq!(
            message(Event::InstanceDown, &payload, Some("https://fleett.test")),
            Message {
                subject: "fleett: instance.down - alice".to_string(),
                body: "instance: alice\nname: Agent alice\nstate: running\nlast error: boom\n\
                       console: https://fleett.test/instances/alice"
                    .to_string(),
            }
        );
        assert!(message(Event::InstanceDown, &payload, None)
            .body
            .ends_with("console: /instances/alice"));
    }

    #[test]
    fn a_secret_round_trips_and_is_bound_to_its_channel() {
        let (_dir, cfg, db) = setup();
        let sealed = seal_secret(&cfg, "123:token").expect("seal");
        let mut record = channel("tg", KIND_TELEGRAM, "42", "*");
        db.insert_notification_channel(&record, Some((&sealed.nonce, &sealed.ciphertext)))
            .expect("insert");
        record = db
            .notification_channel("tg")
            .expect("read")
            .expect("present");
        assert_eq!(
            channel_secret(&cfg, &record).expect("decrypt").as_deref(),
            Some("123:token")
        );

        let mut other = cfg.clone();
        other.secrets_key = Some(secrets::encode_key(&[8u8; 32]));
        assert!(channel_secret(&other, &record).is_err());
    }

    #[test]
    fn a_target_with_a_token_is_masked_in_logs() {
        assert_eq!(
            safe_target(
                KIND_DISCORD,
                "https://discord.com/api/webhooks/1/secret-token"
            ),
            "https://discord.com/…"
        );
        assert_eq!(safe_target(KIND_TELEGRAM, "12345"), "12345");
        assert_eq!(safe_target(KIND_SMTP, "127.0.0.1:25"), "127.0.0.1:25");
    }

    /// Local axum webhook answering `status` and counting its requests.
    fn spawn_webhook(status: u16) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            let counter = counter.clone();
            let app = axum::Router::new().route(
                "/hook",
                axum::routing::post(move || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        axum::http::StatusCode::from_u16(status)
                            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    }
                }),
            );
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}/hook"), hits)
    }

    fn webhook_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client")
    }

    #[tokio::test]
    async fn a_second_event_in_the_window_is_suppressed() {
        let (_dir, cfg, db) = setup();
        let (target, hits) = spawn_webhook(200);
        db.insert_notification_channel(&channel("hook", KIND_WEBHOOK, &target, "*"), None)
            .expect("channel");
        let payload = Payload::instance(&instance("alice"));
        let client = webhook_client();

        let first = notify(&cfg, &db, &client, Event::InstanceDown, &payload)
            .await
            .expect("first");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].status, STATUS_SENT);
        assert_eq!(first[0].attempts, 1);

        let second = notify(&cfg, &db, &client, Event::InstanceDown, &payload)
            .await
            .expect("second");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].status, STATUS_SUPPRESSED);
        assert_eq!(second[0].attempts, 0);
        assert!(second[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("suppressed"));

        // A different event is not coalesced with `instance.down`.
        let third = notify(&cfg, &db, &client, Event::InstanceUp, &payload)
            .await
            .expect("third");
        assert_eq!(third[0].status, STATUS_SENT);
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        let journal = db
            .list_notification_deliveries(Some("hook"), 10)
            .expect("deliveries");
        assert_eq!(journal.len(), 3);
        assert_eq!(
            journal
                .iter()
                .filter(|row| row.status == STATUS_SUPPRESSED)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_failing_webhook_is_retried_and_journalled() {
        let (_dir, cfg, db) = setup();
        let (target, hits) = spawn_webhook(500);
        db.insert_notification_channel(
            &channel("broken", KIND_WEBHOOK, &target, "instance.down"),
            None,
        )
        .expect("channel");
        let payload = Payload::instance(&instance("alice"));

        let rows = notify_with_backoff(
            &cfg,
            &db,
            &webhook_client(),
            Event::InstanceDown,
            &payload,
            &[Duration::ZERO, Duration::ZERO, Duration::ZERO],
        )
        .await
        .expect("notify");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, STATUS_FAILED);
        assert_eq!(rows[0].attempts, MAX_ATTEMPTS);
        assert!(rows[0].error.as_deref().unwrap_or_default().contains("500"));
        assert_eq!(hits.load(Ordering::SeqCst), MAX_ATTEMPTS);

        let journal = db
            .list_notification_deliveries(None, 10)
            .expect("deliveries");
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0].status, STATUS_FAILED);
        assert_eq!(journal[0].attempts, MAX_ATTEMPTS);
    }

    #[tokio::test]
    async fn disabled_and_unsubscribed_channels_are_skipped() {
        let (_dir, cfg, db) = setup();
        let (target, hits) = spawn_webhook(200);
        let mut disabled = channel("disabled", KIND_WEBHOOK, &target, "*");
        disabled.enabled = false;
        db.insert_notification_channel(&disabled, None)
            .expect("disabled");
        db.insert_notification_channel(
            &channel("other", KIND_WEBHOOK, &target, "upgrade.failed"),
            None,
        )
        .expect("other");

        let rows = notify(
            &cfg,
            &db,
            &webhook_client(),
            Event::InstanceDown,
            &Payload::instance(&instance("alice")),
        )
        .await
        .expect("notify");
        assert!(rows.is_empty());
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_test_endpoint_bypasses_subscriptions_and_the_window() {
        let (_dir, cfg, db) = setup();
        let (target, hits) = spawn_webhook(200);
        let mut record = channel("hook", KIND_WEBHOOK, &target, "instance.down");
        record.enabled = false;
        // Disabled channels can still be tested explicitly.
        db.insert_notification_channel(&record, None)
            .expect("channel");
        let record = db.notification_channel("hook").expect("read").expect("row");

        let first = test_channel(&cfg, &db, &webhook_client(), &record)
            .await
            .expect("test");
        let second = test_channel(&cfg, &db, &webhook_client(), &record)
            .await
            .expect("test");
        assert_eq!(first.status, STATUS_SENT);
        assert_eq!(second.status, STATUS_SENT);
        assert_eq!(first.event, "notification.test");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn smtp_messages_are_dot_stuffed_and_header_safe() {
        let message = Message {
            subject: "fleett: instance.down".to_string(),
            body: ".leading dot\nsecond line".to_string(),
        };
        let data = smtp_data("fleett@localhost", "ops@example.test", &message);
        assert!(data.contains("To: ops@example.test\r\n"));
        assert!(data.contains("\r\n..leading dot\r\n"));
        assert!(data.contains("\r\nsecond line\r\n"));
    }

    #[tokio::test]
    async fn the_smtp_conversation_uses_plain_commands() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address: SocketAddr = listener.local_addr().expect("address");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                std::thread::spawn(move || {
                    use std::io::{BufRead, BufReader, Write};
                    let _ = stream.write_all(b"220 relay ready\r\n");
                    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                    let mut in_data = false;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        if in_data {
                            if line.trim_end() == "." {
                                in_data = false;
                                let _ = stream.write_all(b"250 queued\r\n");
                            }
                            continue;
                        }
                        let command = line.trim().to_ascii_uppercase();
                        let reply: &[u8] = if command.starts_with("EHLO") {
                            b"250-relay\r\n250 SIZE 1024\r\n"
                        } else if command.starts_with("MAIL") || command.starts_with("RCPT") {
                            b"250 ok\r\n"
                        } else if command == "DATA" {
                            in_data = true;
                            b"354 go ahead\r\n"
                        } else if command.starts_with("QUIT") {
                            let _ = stream.write_all(b"221 bye\r\n");
                            return;
                        } else {
                            b"250 ok\r\n"
                        };
                        if stream.write_all(reply).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        let channel = channel("mail", KIND_SMTP, &address.to_string(), "*");
        let message = Message {
            subject: "fleett: notification.test".to_string(),
            body: "instance: alice\nstate: stopped".to_string(),
        };
        send_smtp(&channel, &message).await.expect("smtp");
    }
}
