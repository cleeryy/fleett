//! Outbound notification channels and their delivery journal (issue #33).
//!
//! Mutations require the admin role; every authenticated role may read the
//! channel list and the deliveries. No endpoint ever returns the channel
//! secret: the views carry `has_secret` instead, and the audit trail records a
//! masked target for the token-bearing kinds.

use crate::api::audit::{record, require_admin};
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::db::{NotificationChannelRecord, NotificationDeliveryRecord};
use crate::models::now_iso;
use crate::notify::{self, NotifyError};
use crate::secrets::{SecretsError, MISSING_KEY_CODE};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Deliveries returned when `?limit=` is omitted.
pub const DEFAULT_DELIVERY_LIMIT: usize = 100;
/// Upper bound of one page.
pub const MAX_DELIVERY_LIMIT: usize = 1000;

impl From<NotifyError> for ApiError {
    fn from(err: NotifyError) -> Self {
        match err {
            NotifyError::Invalid(message) => ApiError::bad_request("invalid_notification", message),
            NotifyError::Secrets(SecretsError::MissingKey) => {
                ApiError::conflict(MISSING_KEY_CODE, SecretsError::MissingKey.to_string())
            }
            NotifyError::Secrets(other) => ApiError::internal(other.to_string()),
            NotifyError::Db(other) => ApiError::from(other),
        }
    }
}

/// `POST /api/notifications`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateNotificationRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    /// Optional channel secret (the Telegram bot token). Encrypted at rest.
    #[serde(default)]
    pub secret: Option<String>,
    /// `["instance.down", "incident.*"]`; defaults to `["*"]`.
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `PATCH /api/notifications/{id}`.
///
/// `secret` omitted keeps the stored value, `""` clears it, anything else
/// rotates it (a fresh nonce is generated).
#[derive(Debug, Clone, Deserialize)]
pub struct PatchNotificationRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `GET /api/notifications/deliveries?channel=…&limit=…`.
#[derive(Debug, Clone, Deserialize)]
pub struct DeliveryQuery {
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// API view of one channel. It has no secret and no ciphertext field.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationChannelView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub events: Vec<String>,
    pub enabled: bool,
    /// True when an encrypted secret is stored.
    pub has_secret: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// API view of one delivery.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationDeliveryView {
    pub id: i64,
    pub channel_id: String,
    pub event: String,
    pub subject: String,
    pub status: String,
    pub attempts: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
}

/// `GET /api/notifications`.
pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<Vec<NotificationChannelView>>, ApiError> {
    let channels = state
        .db
        .list_notification_channels()
        .map_err(ApiError::from)?;
    Ok(Json(channels.iter().map(channel_view).collect()))
}

/// `GET /api/notifications/{id}`.
pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<NotificationChannelView>, ApiError> {
    let channel = load(&state, &id)?;
    Ok(Json(channel_view(&channel)))
}

/// `POST /api/notifications`.
pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateNotificationRequest>,
) -> Result<(StatusCode, Json<NotificationChannelView>), ApiError> {
    require_admin(&context)?;

    let name = required_name(request.name.as_deref())?;
    let kind = required_kind(request.kind.as_deref())?;
    let target = required_target(&kind, request.target.as_deref())?;
    let events = normalize_events(request.events)?;
    let secret = request.secret.as_deref().filter(|value| !value.is_empty());
    notify::validate_secret(&kind, secret)
        .map_err(|message| ApiError::bad_request("invalid_notification", message))?;
    let sealed = match secret {
        Some(secret) => Some(notify::seal_secret(&state.cfg, secret).map_err(ApiError::from)?),
        None => None,
    };

    let now = now_iso();
    let channel = NotificationChannelRecord {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        kind,
        target,
        nonce: None,
        ciphertext: None,
        events: notify::join_events(&events),
        enabled: request.enabled.unwrap_or(true),
        created_at: now.clone(),
        updated_at: now,
    };
    state
        .db
        .insert_notification_channel(
            &channel,
            sealed
                .as_ref()
                .map(|sealed| (sealed.nonce.as_slice(), sealed.ciphertext.as_slice())),
        )
        .map_err(ApiError::from)?;

    let stored = load(&state, &channel.id)?;
    audit_channel(&state, &context, "notification.create", &stored, None);
    Ok((StatusCode::CREATED, Json(channel_view(&stored))))
}

/// `PATCH /api/notifications/{id}`.
pub async fn patch(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PatchNotificationRequest>,
) -> Result<Json<NotificationChannelView>, ApiError> {
    require_admin(&context)?;

    let stored = load(&state, &id)?;
    let mut spec = stored.clone();
    if let Some(name) = request.name.as_deref() {
        spec.name = required_name(Some(name))?;
    }
    if let Some(kind) = request.kind.as_deref() {
        spec.kind = required_kind(Some(kind))?;
    }
    let target = request.target.as_deref().unwrap_or(&spec.target);
    spec.target = required_target(&spec.kind, Some(target))?;
    if let Some(values) = request.events {
        spec.events = notify::join_events(&normalize_events(Some(values))?);
    }
    if let Some(enabled) = request.enabled {
        spec.enabled = enabled;
    }

    // `None` keeps the secret, `Some(None)` clears it, `Some(Some(..))` rotates.
    let secret_update = match request.secret.as_deref() {
        None => None,
        Some("") => Some(None),
        Some(secret) => {
            notify::validate_secret(&spec.kind, Some(secret))
                .map_err(|message| ApiError::bad_request("invalid_notification", message))?;
            Some(Some(
                notify::seal_secret(&state.cfg, secret).map_err(ApiError::from)?,
            ))
        }
    };
    let rotated = matches!(&secret_update, Some(Some(_)));
    let cleared = matches!(&secret_update, Some(None));
    let has_secret = match &secret_update {
        Some(None) => false,
        Some(Some(_)) => true,
        None => stored.ciphertext.is_some(),
    };
    // A channel that needs a secret (Telegram) may not end up without one.
    if !has_secret {
        notify::validate_secret(&spec.kind, None)
            .map_err(|message| ApiError::bad_request("invalid_notification", message))?;
    }

    let update = match &secret_update {
        Some(Some(sealed)) => Some(Some((
            sealed.nonce.as_slice(),
            sealed.ciphertext.as_slice(),
        ))),
        Some(None) => Some(None),
        None => None,
    };
    spec.updated_at = now_iso();
    state
        .db
        .update_notification_channel(&spec, update)
        .map_err(ApiError::from)?;

    let stored = load(&state, &spec.id)?;
    audit_channel(
        &state,
        &context,
        "notification.update",
        &stored,
        Some(json!({ "rotated": rotated, "cleared": cleared })),
    );
    Ok(Json(channel_view(&stored)))
}

/// `DELETE /api/notifications/{id}`.
pub async fn delete(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;
    let channel = load(&state, &id)?;
    state
        .db
        .delete_notification_channel(&id)
        .map_err(ApiError::from)?;
    audit_channel(&state, &context, "notification.delete", &channel, None);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/notifications/{id}/test`: send a synthetic `notification.test`
/// event to this channel, bypassing its subscriptions and the rate limit.
///
/// The delivery row is returned even when the send failed: the HTTP call
/// succeeded, the alert did not.
pub async fn test(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<NotificationDeliveryView>, ApiError> {
    require_admin(&context)?;
    let channel = load(&state, &id)?;
    let delivery = notify::test_channel(&state.cfg, &state.db, &state.http, &channel)
        .await
        .map_err(ApiError::from)?;
    record(
        &state,
        &context,
        "notification.test",
        &channel.id,
        Some(json!({
            "name": channel.name,
            "kind": channel.kind,
            "target": notify::safe_target(&channel.kind, &channel.target),
            "status": delivery.status,
            "attempts": delivery.attempts,
        })),
    );
    Ok(Json(delivery_view(&delivery)))
}

/// `GET /api/notifications/deliveries?channel=…&limit=…`.
pub async fn deliveries(
    State(state): State<AppState>,
    Query(query): Query<DeliveryQuery>,
) -> Result<Json<Vec<NotificationDeliveryView>>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_DELIVERY_LIMIT as u32) as usize;
    if limit == 0 || limit > MAX_DELIVERY_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_DELIVERY_LIMIT}"),
        ));
    }
    let channel = query
        .channel
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let rows = state
        .db
        .list_notification_deliveries(channel, limit)
        .map_err(ApiError::from)?;
    Ok(Json(rows.iter().map(delivery_view).collect()))
}

fn load(state: &AppState, id: &str) -> Result<NotificationChannelRecord, ApiError> {
    state
        .db
        .notification_channel(id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("notification channel \"{id}\" not found")))
}

fn channel_view(channel: &NotificationChannelRecord) -> NotificationChannelView {
    NotificationChannelView {
        id: channel.id.clone(),
        name: channel.name.clone(),
        kind: channel.kind.clone(),
        target: channel.target.clone(),
        events: channel
            .events
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        enabled: channel.enabled,
        has_secret: channel.ciphertext.is_some(),
        created_at: channel.created_at.clone(),
        updated_at: channel.updated_at.clone(),
    }
}

fn delivery_view(delivery: &NotificationDeliveryRecord) -> NotificationDeliveryView {
    NotificationDeliveryView {
        id: delivery.id,
        channel_id: delivery.channel_id.clone(),
        event: delivery.event.clone(),
        subject: delivery.subject.clone(),
        status: delivery.status.clone(),
        attempts: delivery.attempts,
        error: delivery.error.clone(),
        created_at: delivery.created_at.clone(),
        delivered_at: delivery.delivered_at.clone(),
    }
}

/// Audited details never carry a secret or an unmasked token-bearing target.
fn audit_channel(
    state: &AppState,
    context: &AuthContext,
    action: &str,
    channel: &NotificationChannelRecord,
    extra: Option<Value>,
) {
    let mut details = json!({
        "name": channel.name,
        "kind": channel.kind,
        "target": notify::safe_target(&channel.kind, &channel.target),
        "events": channel.events,
        "enabled": channel.enabled,
        "has_secret": channel.ciphertext.is_some(),
    });
    if let (Some(Value::Object(extra)), Value::Object(map)) = (extra, &mut details) {
        for (key, value) in extra {
            map.insert(key, value);
        }
    }
    record(state, context, action, &channel.id, Some(details));
}

fn required_name(raw: Option<&str>) -> Result<String, ApiError> {
    let name = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("invalid_name", "name is required"))?;
    notify::validate_name(name)
        .map_err(|message| ApiError::bad_request("invalid_name", message))?;
    Ok(name.to_string())
}

fn required_kind(raw: Option<&str>) -> Result<String, ApiError> {
    let kind = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("invalid_kind", "kind is required"))?;
    notify::validate_kind(kind)
        .map_err(|message| ApiError::bad_request("invalid_kind", message))?;
    Ok(kind.to_string())
}

fn required_target(kind: &str, raw: Option<&str>) -> Result<String, ApiError> {
    let target = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("invalid_target", "target is required"))?;
    notify::validate_target(kind, target)
        .map_err(|message| ApiError::bad_request("invalid_target", message))?;
    Ok(target.to_string())
}

fn normalize_events(values: Option<Vec<String>>) -> Result<Vec<String>, ApiError> {
    match values {
        None => Ok(vec![notify::WILDCARD.to_string()]),
        Some(values) => notify::normalize_events(values)
            .map_err(|message| ApiError::bad_request("invalid_events", message)),
    }
}
