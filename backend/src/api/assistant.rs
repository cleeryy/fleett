//! HTTP routes of the integrated assistant (issue #56).
//!
//! `GET`/`PATCH /api/assistant/config` plus `POST /api/assistant/chat`; every
//! route disappears when `FLEETT_ASSISTANT=off`: the router does not register
//! them, so the API answers its generic `404 route not found` and the console
//! hides the panel. A disabled feature must not advertise itself, which is why
//! this is a `404` and not a `403`.
//!
//! The assistant drafts; it never writes. Every proposal is applied by the
//! console through the regular routes, so the mutation keeps its ETag check,
//! backup, git commit and audit row.

use crate::api::audit::{record, record_result, require_admin};
use crate::api::error::ApiError;
use crate::api::json::ApiJson;
use crate::api::AppState;
use crate::assistant::{
    self, AssistantConfig, AssistantError, ChatRequest, ChatResponse, ProviderSource, Reason,
    Usage, MAX_MESSAGES, MAX_MESSAGE_BYTES, MAX_SESSION_ID_CHARS,
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

/// Which provider the operator picked.
///
/// `Option<String>` cannot express this: serde collapses a missing field and an
/// explicit `null` to `None`, and a body typo (`{}`) would then silently clear
/// a working selection. This enum keeps "absent" a deserialization error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderChoice {
    /// `{"provider": null}` — clear the stored selection.
    Clear,
    /// `{"provider": "<id>"}` — select this provider.
    Select(String),
}

impl<'de> Deserialize<'de> for ProviderChoice {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ChoiceVisitor;

        impl<'de> serde::de::Visitor<'de> for ChoiceVisitor {
            type Value = ProviderChoice;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a provider id or null")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(ProviderChoice::Select(value.to_string()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(ProviderChoice::Select(value))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(ProviderChoice::Clear)
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(ProviderChoice::Clear)
            }
        }

        // `deserialize_any`, never `deserialize_option`: serde only lets an
        // `Option` field default silently, so an absent key stays a schema
        // error instead of clearing the selection.
        deserializer.deserialize_any(ChoiceVisitor)
    }
}

/// Body of `PATCH /api/assistant/config`.
///
/// `provider` is required: a string selects a provider, `null` clears the
/// stored selection (and lets `auto` take over again).
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigPatch {
    pub provider: ProviderChoice,
}

impl ProviderChoice {
    /// The id to store, `None` when the selection is cleared. A blank string
    /// counts as a clear: it is what an emptied form field sends.
    pub fn selected(&self) -> Option<String> {
        match self {
            Self::Clear => None,
            Self::Select(id) => {
                let id = id.trim();
                (!id.is_empty()).then(|| id.to_string())
            }
        }
    }
}

/// `409 assistant_not_configured`, with the reason and the setup hint.
fn not_configured(reason: Reason) -> ApiError {
    let (message, hint) = match reason {
        Reason::NoProvider => (
            "the assistant has no provider: create one, then select it for the assistant",
            "create a provider with POST /api/providers; a single provider is used automatically",
        ),
        Reason::NotSelected => (
            "the assistant has no provider selected: several providers exist",
            "pick one with PATCH /api/assistant/config {\"provider\": \"<id>\"}",
        ),
    };
    ApiError::conflict("assistant_not_configured", message).with_details(json!({
        "reason": reason.as_str(),
        "env": "FLEETT_ASSISTANT_PROVIDER",
        "providers": "/api/providers",
        "config": "/api/assistant/config",
        "hint": hint,
    }))
}

/// `422 assistant_provider_unknown`: a reference (env or stored setting) names
/// an id that does not exist. `details.valid` is what the console offers.
fn provider_unknown(id: &str, source: &str, valid: Vec<String>) -> ApiError {
    ApiError::unprocessable(
        "assistant_provider_unknown",
        format!("the assistant provider \"{id}\" ({source}) does not exist"),
    )
    .with_details(json!({
        "provider": id,
        "source": source,
        "valid": valid,
        "env": "FLEETT_ASSISTANT_PROVIDER",
        "providers": "/api/providers",
        "config": "/api/assistant/config",
        "hint": if source == "env" {
            "set FLEETT_ASSISTANT_PROVIDER to an existing provider id, or unset it and select \
             one with PATCH /api/assistant/config"
        } else {
            "select an existing provider with PATCH /api/assistant/config"
        },
    }))
}

/// Map the core errors onto the shared envelope. A provider failure is a
/// `502`: the control plane is fine, the upstream is not.
fn map_error(err: AssistantError) -> ApiError {
    match err {
        AssistantError::NotConfigured => not_configured(Reason::NoProvider),
        // The row vanished between the resolution and the decryption: the same
        // 422, with the provider named and no valid id to offer.
        AssistantError::ProviderNotFound(id) => provider_unknown(&id, "deleted", Vec::new()),
        AssistantError::ProviderUnknown { id, origin, valid } => {
            provider_unknown(&id, origin, valid)
        }
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

/// `PATCH /api/assistant/config` — admin only.
///
/// Selects the provider the assistant borrows (issue #63), or clears the
/// selection with `null`. The answer is the resolved configuration, so the
/// console needs no second call. `FLEETT_ASSISTANT_PROVIDER` still wins: the
/// body is stored either way and `provider_source` says who won, which is the
/// only way to keep the panel honest about it.
pub async fn patch_config(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    ApiJson(request): ApiJson<ConfigPatch>,
) -> Result<Json<AssistantConfig>, ApiError> {
    require_admin(&context)?;

    let selected = request.provider.selected();
    let stored = match selected.as_deref() {
        Some(id) => {
            if state.db.provider(id).map_err(ApiError::from)?.is_none() {
                let valid = assistant::provider_ids(&state.db).map_err(map_error)?;
                return Err(provider_unknown(id, "setting", valid));
            }
            state
                .db
                .set_setting(assistant::SETTING_ASSISTANT_PROVIDER, id)
                .map_err(ApiError::from)?;
            Some(id.to_string())
        }
        None => {
            state
                .db
                .clear_setting(assistant::SETTING_ASSISTANT_PROVIDER)
                .map_err(ApiError::from)?;
            None
        }
    };

    let payload = assistant::config_payload(&state.cfg, &state.db, &state.assistant, None)
        .map_err(map_error)?;
    // The panel must be able to say "selected, but the environment overrides
    // it": the resolved source is the truth, the body is what we stored.
    let env_override = payload.provider_source == ProviderSource::Env;
    record(
        &state,
        &context,
        "assistant.config",
        "assistant",
        Some(json!({
            "provider": stored,
            "source": payload.provider_source.as_str(),
            "env_override": env_override,
        })),
    );
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

    let provider = match assistant::resolve(&state.cfg, &state.db) {
        Ok(assistant::ProviderResolution::Provider { spec, .. }) => spec,
        Ok(assistant::ProviderResolution::Missing { reason }) => {
            return Err(not_configured(reason))
        }
        Ok(assistant::ProviderResolution::Broken { id, source, valid }) => {
            return Err(provider_unknown(&id, source.as_str(), valid))
        }
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
