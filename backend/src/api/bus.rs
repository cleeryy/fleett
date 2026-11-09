//! Agent-to-agent message bus (issue #28).

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::{AuthContext, Role};
use crate::bus::{
    self, BoxSide, BusError, Message, MessageKind, MessageStatus, NewMessage, DEFAULT_LIMIT,
    DEFAULT_MAX_HOPS, MAX_LIMIT,
};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    /// Recipient instance id. Required.
    pub to: String,
    /// Sender override. Normally omitted: the path id is the sender. Admin
    /// callers may name another instance (an orchestrator acting for one).
    #[serde(default)]
    pub from: Option<String>,
    /// `ask` (default), `tell` or `result`.
    #[serde(default)]
    pub kind: Option<String>,
    pub body: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub ttl_s: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct InboxQuery {
    /// `inbox` (default), `outbox` or `all`.
    #[serde(default)]
    pub view: Option<String>,
    /// `queued`, `delivered` or `read`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct GlobalQuery {
    #[serde(default)]
    pub instance: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Serialize)]
pub struct AckResponse {
    message: Message,
}

impl From<BusError> for ApiError {
    fn from(err: BusError) -> Self {
        match err {
            BusError::UnknownInstance(id) => ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_instance",
                format!("unknown instance \"{id}\""),
            ),
            BusError::NotFound(id) => ApiError::not_found(format!("unknown message \"{id}\"")),
            BusError::LoopDetected(hops, max) => ApiError::conflict(
                "message_loop",
                format!("message stopped after {hops} hops (max {max})"),
            )
            .with_details(json!({ "hops": hops, "max_hops": max })),
            BusError::SelfMessage => {
                ApiError::bad_request("self_message", "an instance cannot message itself")
            }
            BusError::EmptyBody => {
                ApiError::bad_request("empty_body", "message body must not be empty")
            }
            BusError::BodyTooLarge => ApiError::bad_request(
                "body_too_large",
                format!("message body exceeds {} bytes", bus::MAX_BODY_BYTES),
            ),
            BusError::InvalidTtl => ApiError::bad_request(
                "invalid_ttl",
                format!(
                    "ttl_s must be between {} and {} seconds",
                    bus::MIN_TTL_S,
                    bus::MAX_TTL_S
                ),
            ),
            BusError::Db(err) => ApiError::from(err),
        }
    }
}

/// `POST /api/instances/{id}/messages` — queue a message from `id`.
pub async fn send(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<Message>), ApiError> {
    if !state.db.exists(&id).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!("unknown instance \"{id}\"")));
    }
    let from = request
        .from
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&id)
        .to_string();
    if from != id && context.role != Role::Admin {
        return Err(ApiError::forbidden(
            "forbidden",
            "only an admin may send a message on behalf of another instance",
        ));
    }
    let kind = match request.kind.as_deref() {
        Some(value) => MessageKind::parse(value).ok_or_else(|| {
            ApiError::bad_request("invalid_kind", "kind must be ask, tell or result")
        })?,
        None => MessageKind::Tell,
    };
    let message = bus::send(
        &state.db,
        NewMessage {
            from: from.clone(),
            to: request.to.trim().to_string(),
            kind,
            body: request.body,
            correlation_id: request.correlation_id,
            ttl_s: request.ttl_s,
            max_hops: DEFAULT_MAX_HOPS,
        },
    )?;
    let _ = bus::spool(&state.cfg.root, &message);
    audit::record(
        &state,
        &context,
        "message.send",
        &message.id,
        Some(json!({
            "from": message.from,
            "to": message.to,
            "kind": message.kind.as_str(),
            "hops": message.hops,
            "correlation_id": message.correlation_id,
        })),
    );
    Ok((StatusCode::CREATED, Json(message)))
}

/// `GET /api/instances/{id}/messages` — inbox, outbox or both.
pub async fn inbox(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<InboxQuery>,
) -> Result<Json<Vec<Message>>, ApiError> {
    if !state.db.exists(&id).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!("unknown instance \"{id}\"")));
    }
    let side = match query.view.as_deref() {
        Some(value) => BoxSide::parse(value).ok_or_else(|| {
            ApiError::bad_request("invalid_view", "view must be inbox, outbox or all")
        })?,
        None => BoxSide::Inbox,
    };
    let status = match query.status.as_deref() {
        Some(value) => Some(MessageStatus::parse(value).ok_or_else(|| {
            ApiError::bad_request("invalid_status", "status must be queued, delivered or read")
        })?),
        None => None,
    };
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let messages = bus::list_for_instance(&state.db, &id, side, status, limit)?;
    Ok(Json(messages))
}

/// `POST /api/instances/{id}/messages/{message_id}/ack` — mark a message read.
pub async fn ack(
    Path((id, message_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<AckResponse>, ApiError> {
    let message = bus::ack(&state.db, &id, &message_id)?;
    audit::record(
        &state,
        &context,
        "message.ack",
        &message.id,
        Some(json!({ "to": message.to })),
    );
    Ok(Json(AckResponse { message }))
}

/// `GET /api/messages` — admin view of the whole bus.
pub async fn list(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<GlobalQuery>,
) -> Result<Json<Vec<Message>>, ApiError> {
    if context.role != Role::Admin {
        return Err(ApiError::forbidden(
            "forbidden",
            "this endpoint requires the admin role",
        ));
    }
    let kind = match query.kind.as_deref() {
        Some(value) => Some(MessageKind::parse(value).ok_or_else(|| {
            ApiError::bad_request("invalid_kind", "kind must be ask, tell or result")
        })?),
        None => None,
    };
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let messages = bus::list_all(
        &state.db,
        query.instance.as_deref(),
        query.from.as_deref(),
        query.to.as_deref(),
        kind,
        limit,
    )?;
    Ok(Json(messages))
}

/// `DELETE /api/messages/expired` — purge the expired queue (admin).
pub async fn purge(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if context.role != Role::Admin {
        return Err(ApiError::forbidden(
            "forbidden",
            "this endpoint requires the admin role",
        ));
    }
    let removed = bus::purge_expired(&state.db)?;
    audit::record(
        &state,
        &context,
        "message.purge",
        "messages",
        Some(json!({ "removed": removed })),
    );
    Ok(Json(json!({ "removed": removed })))
}
