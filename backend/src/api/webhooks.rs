//! Outbound webhook endpoints (issue #46).
//!
//! Admin role required for every mutation; viewers may read the list, the
//! detail and the delivery log. No response ever carries the HMAC secret:
//! [`WebhookSummary`] only reports `has_secret`.

use crate::api::audit::{record, require_admin};
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::models::now_iso;
use crate::webhooks::{
    self, CreateWebhookRequest, DeliveryView, PatchWebhookRequest, WebhookRecord,
    WebhookTestResponse, DEFAULT_DELIVERIES_LIMIT, MAX_DELIVERIES_LIMIT,
};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

impl From<webhooks::WebhookError> for ApiError {
    fn from(err: webhooks::WebhookError) -> Self {
        match err {
            webhooks::WebhookError::Invalid(message) => {
                ApiError::bad_request("invalid_webhook", message)
            }
            webhooks::WebhookError::Secrets(crate::secrets::SecretsError::MissingKey) => {
                ApiError::conflict(
                    crate::secrets::MISSING_KEY_CODE,
                    crate::secrets::SecretsError::MissingKey.to_string(),
                )
            }
            webhooks::WebhookError::Secrets(other) => ApiError::internal(other.to_string()),
            webhooks::WebhookError::Db(other) => ApiError::from(other),
            webhooks::WebhookError::Payload(other) => ApiError::internal(other.to_string()),
        }
    }
}

/// `GET /api/webhooks` — every webhook, secret never included.
pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<Vec<webhooks::WebhookSummary>>, ApiError> {
    let records = webhooks::list(&state.db).map_err(ApiError::from)?;
    Ok(Json(records.iter().map(WebhookRecord::summary).collect()))
}

/// `GET /api/webhooks/{id}`.
pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<webhooks::WebhookSummary>, ApiError> {
    Ok(Json(load(&state, &id)?.summary()))
}

/// `POST /api/webhooks` — requires a secrets key (the HMAC secret is stored
/// encrypted, exactly like a provider credential).
pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateWebhookRequest>,
) -> Result<(StatusCode, Json<webhooks::WebhookSummary>), ApiError> {
    require_admin(&context)?;

    let name = webhooks::validate_name(request.name.as_deref())?;
    let url = webhooks::validate_url(request.url.as_deref())?;
    let events = webhooks::validate_events(request.events.as_deref().unwrap_or(&[]))?;
    let secret = request
        .secret
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("invalid_secret", "secret is required"))?;
    webhooks::validate_secret(secret)?;
    let sealed = webhooks::seal(&state.cfg, secret)?;

    let now = now_iso();
    let record = WebhookRecord {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        url,
        events,
        nonce: sealed.nonce,
        ciphertext: sealed.ciphertext,
        enabled: request.enabled.unwrap_or(true),
        created_at: now.clone(),
        updated_at: now,
    };
    webhooks::insert(&state.db, &record).map_err(ApiError::from)?;

    record_audit(
        &state,
        &context,
        "webhook.create",
        &record,
        json!({
            "url": record.url,
            "events": record.events,
            "enabled": record.enabled,
        }),
    );
    Ok((StatusCode::CREATED, Json(record.summary())))
}

/// `PATCH /api/webhooks/{id}` — `secret` rotates the signing key.
pub async fn patch(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PatchWebhookRequest>,
) -> Result<Json<webhooks::WebhookSummary>, ApiError> {
    require_admin(&context)?;

    let mut record = load(&state, &id)?;
    if let Some(name) = request.name.as_deref() {
        record.name = webhooks::validate_name(Some(name))?;
    }
    if let Some(url) = request.url.as_deref() {
        record.url = webhooks::validate_url(Some(url))?;
    }
    if let Some(events) = request.events.as_deref() {
        record.events = webhooks::validate_events(events)?;
    }
    if let Some(enabled) = request.enabled {
        record.enabled = enabled;
    }

    let rotated = match request.secret.as_deref() {
        Some(secret) => {
            webhooks::validate_secret(secret)?;
            let sealed = webhooks::seal(&state.cfg, secret)?;
            record.nonce = sealed.nonce;
            record.ciphertext = sealed.ciphertext;
            true
        }
        None => false,
    };
    record.updated_at = now_iso();
    let updated = webhooks::update(
        &state.db,
        &record,
        rotated.then_some((record.nonce.as_slice(), record.ciphertext.as_slice())),
    )
    .map_err(ApiError::from)?;
    if !updated {
        return Err(ApiError::not_found(format!("webhook \"{id}\" not found")));
    }

    record_audit(
        &state,
        &context,
        "webhook.update",
        &record,
        json!({
            "url": record.url,
            "events": record.events,
            "enabled": record.enabled,
            "rotated": rotated,
        }),
    );
    Ok(Json(record.summary()))
}

/// `DELETE /api/webhooks/{id}` — the delivery log is removed with it.
pub async fn delete(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;

    let record = load(&state, &id)?;
    webhooks::delete(&state.db, &id).map_err(ApiError::from)?;
    record_audit(
        &state,
        &context,
        "webhook.delete",
        &record,
        json!({ "url": record.url }),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/webhooks/{id}/test` — one signed `webhook.test` delivery.
///
/// Refused without a secrets key: the signature cannot be computed otherwise.
pub async fn test(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<WebhookTestResponse>, ApiError> {
    require_admin(&context)?;

    let record = load(&state, &id)?;
    let response = webhooks::probe(&state, &record).await?;
    record_audit(
        &state,
        &context,
        "webhook.test",
        &record,
        json!({
            "url": record.url,
            "ok": response.ok,
            "status": response.status,
            "latency_ms": response.latency_ms,
        }),
    );
    Ok(Json(response))
}

/// Query string of `GET /api/webhooks/{id}/deliveries`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeliveriesQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /api/webhooks/{id}/deliveries?limit=50` — newest first.
pub async fn deliveries(
    Path(id): Path<String>,
    Query(query): Query<DeliveriesQuery>,
    State(state): State<AppState>,
) -> Result<Json<Vec<DeliveryView>>, ApiError> {
    let record = load(&state, &id)?;
    let limit = query.limit.unwrap_or(DEFAULT_DELIVERIES_LIMIT);
    if limit == 0 || limit > MAX_DELIVERIES_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_DELIVERIES_LIMIT}"),
        ));
    }
    let rows = webhooks::deliveries(&state.db, &record.id, limit).map_err(ApiError::from)?;
    Ok(Json(
        rows.iter().map(webhooks::DeliveryRecord::view).collect(),
    ))
}

fn load(state: &AppState, id: &str) -> Result<WebhookRecord, ApiError> {
    webhooks::find(&state.db, id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("webhook \"{id}\" not found")))
}

fn record_audit(
    state: &AppState,
    context: &AuthContext,
    action: &str,
    webhook: &WebhookRecord,
    details: serde_json::Value,
) {
    record(state, context, action, &webhook.id, Some(details));
}
