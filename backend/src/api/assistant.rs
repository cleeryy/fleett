//! HTTP routes of the integrated assistant (issue #56).
//!
//! Two routes only, and both disappear when `FLEETT_ASSISTANT=off`: the router
//! does not register them, so the API answers its generic `404 route not
//! found` and the console hides the panel. A disabled feature must not
//! advertise itself, which is why this is a `404` and not a `403`.
//!
//! The assistant drafts; it never writes. Every proposal is applied by the
//! console through the regular routes, so the mutation keeps its ETag check,
//! backup, git commit and audit row.

use crate::api::audit::{record, record_result};
use crate::api::error::ApiError;
use crate::api::json::ApiJson;
use crate::api::AppState;
use crate::assistant::{
    self, AssistantConfig, AssistantError, ChatRequest, ChatResponse, Usage, MAX_MESSAGES,
    MAX_MESSAGE_BYTES, MAX_SESSION_ID_CHARS,
};
use crate::auth::AuthContext;
use crate::providers;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

/// Query of `GET /api/assistant/config`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConfigQuery {
    /// Session whose counter is reported; omit for the sum of every session.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// `409 assistant_not_configured`, with the provider setup hint.
fn not_configured() -> ApiError {
    ApiError::conflict(
        "assistant_not_configured",
        "the assistant has no provider: create one and point FLEETT_ASSISTANT_PROVIDER at its id",
    )
    .with_details(json!({
        "env": "FLEETT_ASSISTANT_PROVIDER",
        "hint": "create a provider with POST /api/providers, then restart the control plane with \
                 FLEETT_ASSISTANT_PROVIDER=<provider id>",
        "providers": "/api/providers",
    }))
}

fn missing_provider(id: &str) -> ApiError {
    ApiError::conflict(
        "assistant_not_configured",
        format!("FLEETT_ASSISTANT_PROVIDER names provider \"{id}\", which does not exist"),
    )
    .with_details(json!({
        "env": "FLEETT_ASSISTANT_PROVIDER",
        "provider": id,
        "hint": "create the provider with POST /api/providers or point the variable at an \
                 existing id, then restart the control plane",
        "providers": "/api/providers",
    }))
}

/// Map the core errors onto the shared envelope. A provider failure is a
/// `502`: the control plane is fine, the upstream is not.
fn map_error(err: AssistantError) -> ApiError {
    match err {
        AssistantError::NotConfigured | AssistantError::ProviderNotFound(_) => not_configured(),
        AssistantError::ProviderStatus(status) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "assistant_provider_error",
            format!("the assistant provider answered with HTTP {status}"),
        )
        .with_details(json!({ "provider_status": status })),
        AssistantError::Provider(message) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "assistant_provider_error",
            format!("the assistant provider request failed: {message}"),
        ),
        AssistantError::BadPayload => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "assistant_provider_error",
            "the assistant provider returned an unexpected payload",
        ),
        AssistantError::Secrets(crate::secrets::SecretsError::MissingKey) => ApiError::conflict(
            crate::secrets::MISSING_KEY_CODE,
            crate::secrets::SecretsError::MissingKey.to_string(),
        ),
        AssistantError::Secrets(other) => ApiError::internal(other.to_string()),
        AssistantError::Env(err) => ApiError::from(err),
        AssistantError::ProviderError(providers::ProviderError::Secrets(
            crate::secrets::SecretsError::MissingKey,
        )) => ApiError::conflict(
            crate::secrets::MISSING_KEY_CODE,
            crate::secrets::SecretsError::MissingKey.to_string(),
        ),
        AssistantError::ProviderError(err) => ApiError::internal(err.to_string()),
        AssistantError::Db(err) => ApiError::from(err),
    }
}

/// Refuse an instance the caller may not read.
///
/// `POST /api/assistant/chat` is not under the `/api/instances/{id}` ACL
/// middleware, so the read check is repeated here: without it, a scoped token
/// could send an instance's file content to the model.
fn ensure_instance_read(
    state: &AppState,
    context: &AuthContext,
    instance: &str,
) -> Result<(), ApiError> {
    if !state.db.exists(instance).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!(
            "instance \"{instance}\" not found"
        )));
    }
    let acl = crate::authz::entries(
        state
            .db
            .acl_for_subject(&context.subject)
            .map_err(ApiError::from)?,
    );
    if crate::authz::can(context, &acl, instance, crate::authz::Action::Read) {
        return Ok(());
    }
    Err(ApiError::forbidden(
        "forbidden",
        format!("read access to instance \"{instance}\" is not granted"),
    )
    .with_details(json!({ "instance": instance, "action": "read" })))
}

fn validate_request(request: &ChatRequest) -> Result<(), ApiError> {
    let session = request.session_id.trim();
    if session.is_empty()
        || session.chars().count() > MAX_SESSION_ID_CHARS
        || session.chars().any(char::is_control)
    {
        return Err(ApiError::unprocessable(
            "invalid_session_id",
            format!("session_id must be 1 to {MAX_SESSION_ID_CHARS} printable characters"),
        ));
    }
    if request.messages.len() > MAX_MESSAGES {
        return Err(ApiError::unprocessable(
            "invalid_messages",
            format!("at most {MAX_MESSAGES} messages are accepted"),
        ));
    }
    for message in &request.messages {
        if !matches!(message.role.as_str(), "user" | "assistant") {
            return Err(ApiError::unprocessable(
                "invalid_messages",
                "every message must have role \"user\" or \"assistant\"",
            ));
        }
        if message.content.len() > MAX_MESSAGE_BYTES {
            return Err(ApiError::unprocessable(
                "invalid_messages",
                format!("a message must be at most {MAX_MESSAGE_BYTES} bytes"),
            ));
        }
    }
    Ok(())
}

/// Estimated tokens of the prompt, used to refuse a turn before paying for it.
fn prompt_estimate(system: &str, request: &ChatRequest) -> u64 {
    assistant::estimate_tokens(system)
        + request
            .messages
            .iter()
            .map(|message| assistant::estimate_tokens(&message.content))
            .fold(0u64, u64::saturating_add)
}

/// `GET /api/assistant/config` — any authenticated role.
pub async fn config(
    State(state): State<AppState>,
    Extension(_context): Extension<AuthContext>,
    Query(query): Query<ConfigQuery>,
) -> Result<Json<AssistantConfig>, ApiError> {
    let payload = assistant::config_payload(
        &state.cfg,
        &state.db,
        &state.assistant,
        query.session_id.as_deref(),
    )
    .map_err(map_error)?;
    Ok(Json(payload))
}

/// `POST /api/assistant/chat`.
pub async fn chat(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    ApiJson(request): ApiJson<ChatRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
    validate_request(&request)?;
    if let Some(instance) = request.context.instance.as_deref() {
        ensure_instance_read(&state, &context, instance)?;
    }
    let target = request
        .context
        .instance
        .clone()
        .unwrap_or_else(|| "assistant".to_string());
    let task = request.task.as_str();
    let model = state.cfg.assistant_model.clone();

    let provider = match assistant::configured_provider(&state.cfg, &state.db) {
        Ok(Some(provider)) => provider,
        Ok(None) => return Err(not_configured()),
        Err(AssistantError::ProviderNotFound(id)) => return Err(missing_provider(&id)),
        Err(err) => return Err(map_error(err)),
    };
    let secret = assistant::provider_secret(&state.cfg, &state.db, &provider).map_err(map_error)?;

    let (context_text, redaction) = assistant::prepare_context(
        &state.cfg,
        &state.db,
        &request.context,
        std::slice::from_ref(&secret),
    )
    .map_err(map_error)?;
    let system = assistant::system_with_context(request.task, &context_text);
    let estimated = prompt_estimate(&system, &request);
    let used = state.assistant.used(request.session_id.trim());
    if !assistant::budget_allows(state.cfg.assistant_budget_tokens, used, estimated) {
        record_result(
            &state,
            &context,
            "assistant.chat",
            &target,
            Some(json!({
                "task": task,
                "model": model,
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0,
            })),
            "refused",
        );
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "assistant_budget_exceeded",
            format!(
                "the assistant budget of this session is exhausted ({} of {} tokens used)",
                used, state.cfg.assistant_budget_tokens
            ),
        )
        .with_details(json!({
            "used_tokens": used,
            "budget_tokens": state.cfg.assistant_budget_tokens,
        })));
    }

    let call = assistant::prepare_chat(&provider, &secret, &model, &system, &request.messages);
    let completion = match assistant::complete(&call).await {
        Ok(completion) => completion,
        Err(err) => {
            record_result(
                &state,
                &context,
                "assistant.chat",
                &target,
                Some(json!({
                    "task": task,
                    "model": model,
                    "prompt_tokens": 0,
                    "completion_tokens": 0,
                    "total_tokens": 0,
                })),
                "failed",
            );
            return Err(map_error(err));
        }
    };

    let prompt_tokens = if completion.prompt_tokens > 0 {
        completion.prompt_tokens
    } else {
        estimated
    };
    let completion_tokens = if completion.completion_tokens > 0 {
        completion.completion_tokens
    } else {
        assistant::estimate_tokens(&completion.text)
    };
    let total = prompt_tokens.saturating_add(completion_tokens);
    state.assistant.add(request.session_id.trim(), total);
    let (message, proposal) = assistant::parse_reply(&completion.text);

    record(
        &state,
        &context,
        "assistant.chat",
        &target,
        Some(json!({
            "task": task,
            "model": model,
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total,
        })),
    );

    Ok(Json(ChatResponse {
        message,
        proposal,
        usage: Usage {
            prompt: prompt_tokens,
            completion: completion_tokens,
            total,
        },
        redaction,
    }))
}
