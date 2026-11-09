//! Reusable provider credentials (issue #25).
//!
//! Admin role required for every mutation; viewers may read the list and the
//! detail. No endpoint ever returns the secret: `GET` bodies carry metadata
//! only, and the test endpoint reports status, latency and an error string.

use crate::api::audit::{record, require_admin};
use crate::api::error::ApiError;
use crate::api::upgrade::wait_until_healthy;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::db::ProviderRecord;
use crate::models::{
    now_iso, validate_api_key_env, validate_base_url, validate_provider_kind,
    validate_provider_name, CreateProviderRequest, PatchProviderRequest, ProviderDetail,
    ProviderRestart, ProviderRotateResponse, ProviderSpec, ProviderSummary, ProviderTestResponse,
    Status, DEFAULT_API_KEY_ENV,
};
use crate::providers::{self, ProviderError, RESTART_FAILED, RESTART_HEALTHY};
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::json;
use std::time::Duration;

/// Per-instance health wait of a rotation, shared by every attached instance.
///
/// The rolling-upgrade helper is reused as-is; a rotation is sequential so one
/// slow instance never blocks the report of the others.
pub const ROTATE_TIMEOUT: Duration = Duration::from_secs(120);

impl From<ProviderError> for ApiError {
    fn from(err: ProviderError) -> Self {
        match err {
            ProviderError::Invalid(message) => ApiError::bad_request("invalid_provider", message),
            ProviderError::NotFound(id) => {
                ApiError::not_found(format!("provider \"{id}\" not found"))
            }
            ProviderError::Secrets(crate::secrets::SecretsError::MissingKey) => ApiError::conflict(
                crate::secrets::MISSING_KEY_CODE,
                crate::secrets::SecretsError::MissingKey.to_string(),
            ),
            ProviderError::Secrets(other) => ApiError::internal(other.to_string()),
            ProviderError::Db(other) => ApiError::from(other),
            ProviderError::Env(other) => ApiError::from(other),
        }
    }
}

/// `GET /api/providers`.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<ProviderSummary>>, ApiError> {
    let summaries = providers::summaries(&state.db).map_err(ApiError::from)?;
    Ok(Json(summaries))
}

/// `GET /api/providers/{id}`: metadata plus the instances that use it.
pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ProviderDetail>, ApiError> {
    let stored = load(&state, &id)?;
    let attached = providers::instances(&state.db, &id).map_err(ApiError::from)?;
    Ok(Json(detail(&stored.spec, attached)))
}

/// `POST /api/providers`.
pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<ProviderSummary>), ApiError> {
    require_admin(&context)?;

    let name = required_name(request.name.as_deref())?;
    let kind = request
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("invalid_kind", "kind is required"))?
        .to_string();
    validate_provider_kind(&kind)
        .map_err(|message| ApiError::bad_request("invalid_kind", message))?;
    let base_url = required_base_url(request.base_url.as_deref())?;
    let api_key_env = key_env_or_default(request.api_key_env.as_deref())?;
    let secret = request
        .secret
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("invalid_secret", "secret is required"))?;
    providers::validate_secret(secret).map_err(ApiError::from)?;

    let sealed = providers::seal(&state.cfg, secret).map_err(ApiError::from)?;
    let now = now_iso();
    let spec = ProviderSpec {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        kind,
        base_url,
        api_key_env,
        created_at: now.clone(),
        updated_at: now,
    };
    state
        .db
        .insert_provider(&spec, &sealed.nonce, &sealed.ciphertext)
        .map_err(provider_db_error)?;

    record(
        &state,
        &context,
        "provider.create",
        &spec.id,
        Some(json!({
            "name": spec.name,
            "kind": spec.kind,
            "base_url": spec.base_url,
            "api_key_env": spec.api_key_env,
        })),
    );
    Ok((StatusCode::CREATED, Json(providers::summary(&spec, 0))))
}

/// `PATCH /api/providers/{id}`.
///
/// Rotating the secret (`secret` present) restarts every attached instance
/// sequentially and reports one outcome per instance. Instances that do not
/// come back are reported `failed` with their last error; the others are not
/// blocked.
pub async fn patch(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PatchProviderRequest>,
) -> Result<Json<ProviderRotateResponse>, ApiError> {
    require_admin(&context)?;

    // Serialise concurrent edits of the same provider (and its instances).
    let lock = state.locks.for_id(&format!("provider:{id}"));
    let _guard = lock.lock().await;

    let mut stored = load(&state, &id)?;
    let mut spec = stored.spec.clone();
    if let Some(name) = request.name.as_deref() {
        spec.name = required_name(Some(name))?;
    }
    if let Some(kind) = request.kind.as_deref() {
        let kind = kind.trim().to_string();
        validate_provider_kind(&kind)
            .map_err(|message| ApiError::bad_request("invalid_kind", message))?;
        spec.kind = kind;
    }
    if let Some(base_url) = request.base_url.as_deref() {
        spec.base_url = required_base_url(Some(base_url))?;
    }
    if let Some(api_key_env) = request.api_key_env.as_deref() {
        let api_key_env = api_key_env.trim();
        validate_api_key_env(api_key_env)
            .map_err(|message| ApiError::bad_request("invalid_api_key_env", message))?;
        spec.api_key_env = api_key_env.to_string();
    }

    let rotated = match request.secret.as_deref() {
        Some(secret) => {
            providers::validate_secret(secret).map_err(ApiError::from)?;
            let sealed = providers::seal(&state.cfg, secret).map_err(ApiError::from)?;
            stored.nonce = sealed.nonce;
            stored.ciphertext = sealed.ciphertext;
            true
        }
        None => false,
    };
    spec.updated_at = now_iso();
    state
        .db
        .update_provider(
            &spec,
            rotated.then_some((stored.nonce.as_slice(), stored.ciphertext.as_slice())),
        )
        .map_err(provider_db_error)?;
    stored.spec = spec.clone();

    record(
        &state,
        &context,
        "provider.update",
        &spec.id,
        Some(json!({
            "name": spec.name,
            "kind": spec.kind,
            "base_url": spec.base_url,
            "api_key_env": spec.api_key_env,
            "rotated": rotated,
        })),
    );

    let restarted = if rotated {
        let report = restart_attached(&state, &spec.id).await?;
        record(
            &state,
            &context,
            "provider.rotate",
            &spec.id,
            Some(json!({
                "name": spec.name,
                "api_key_env": spec.api_key_env,
                "restarted": report.iter().map(|entry| json!({
                    "id": entry.id,
                    "status": entry.status,
                })).collect::<Vec<_>>(),
            })),
        );
        crate::webhooks::emit(
            &state,
            crate::webhooks::Event::SecretRotated,
            json!({
                "provider": spec.id,
                "name": spec.name,
                "api_key_env": spec.api_key_env,
                "restarted": report.len(),
            }),
        );
        report
    } else {
        Vec::new()
    };

    let count = state
        .db
        .provider_instances(&spec.id)
        .map_err(ApiError::from)?
        .len();
    Ok(Json(ProviderRotateResponse {
        provider: providers::summary(&spec, count),
        restarted,
    }))
}

/// `DELETE /api/providers/{id}`: refused while an instance still uses it.
pub async fn delete(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;

    let stored = load(&state, &id)?;
    let attached = providers::instances(&state.db, &id).map_err(ApiError::from)?;
    if !attached.is_empty() {
        return Err(ApiError::conflict(
            "provider_in_use",
            format!(
                "provider \"{}\" is still used by {} instance(s)",
                stored.spec.name,
                attached.len()
            ),
        )
        .with_details(json!({
            "provider": id,
            "instances": attached,
        })));
    }
    state.db.delete_provider(&id).map_err(ApiError::from)?;
    record_audit_delete(&state, &context, &stored.spec);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/providers/{id}/test`: decrypt the key and probe `/models`.
///
/// The answer never contains the key; see [`providers::probe`].
pub async fn test(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<ProviderTestResponse>, ApiError> {
    require_admin(&context)?;

    let stored = load(&state, &id)?;
    let secret = providers::secret_of(&state.cfg, &stored).map_err(ApiError::from)?;
    let outcome = providers::probe(&stored.spec.base_url, &secret).await;
    record(
        &state,
        &context,
        "provider.test",
        &stored.spec.id,
        Some(json!({
            "ok": outcome.ok,
            "status": outcome.status,
            "latency_ms": outcome.latency_ms,
        })),
    );
    Ok(Json(outcome))
}

fn record_audit_delete(state: &AppState, context: &AuthContext, spec: &ProviderSpec) {
    record(
        state,
        context,
        "provider.delete",
        &spec.id,
        Some(json!({
            "name": spec.name,
            "kind": spec.kind,
            "base_url": spec.base_url,
            "api_key_env": spec.api_key_env,
        })),
    );
}

/// One provider record, or `404`.
fn load(state: &AppState, id: &str) -> Result<ProviderRecord, ApiError> {
    state
        .db
        .provider(id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("provider \"{id}\" not found")))
}

fn detail(
    spec: &ProviderSpec,
    instances: Vec<crate::models::ProviderInstanceRef>,
) -> ProviderDetail {
    ProviderDetail {
        id: spec.id.clone(),
        name: spec.name.clone(),
        kind: spec.kind.clone(),
        base_url: spec.base_url.clone(),
        api_key_env: spec.api_key_env.clone(),
        instances_count: instances.len(),
        created_at: spec.created_at.clone(),
        updated_at: spec.updated_at.clone(),
        instances,
    }
}

fn required_name(raw: Option<&str>) -> Result<String, ApiError> {
    let name = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("invalid_name", "name is required"))?;
    validate_provider_name(name)
        .map_err(|message| ApiError::bad_request("invalid_name", message))?;
    Ok(name.to_string())
}

fn required_base_url(raw: Option<&str>) -> Result<String, ApiError> {
    let base_url = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("invalid_base_url", "base_url is required"))?;
    validate_base_url(base_url)
        .map_err(|message| ApiError::bad_request("invalid_base_url", message))?;
    Ok(base_url.to_string())
}

fn key_env_or_default(raw: Option<&str>) -> Result<String, ApiError> {
    let value = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_API_KEY_ENV);
    validate_api_key_env(value)
        .map_err(|message| ApiError::bad_request("invalid_api_key_env", message))?;
    Ok(value.to_string())
}

fn provider_db_error(err: crate::db::DbError) -> ApiError {
    match err {
        crate::db::DbError::Conflict(name) => ApiError::conflict(
            "provider_exists",
            format!("a provider named \"{name}\" already exists"),
        ),
        other => ApiError::from(other),
    }
}

/// Restart every instance attached to one provider, sequentially.
///
/// Each instance is locked on its own, so a rotation never holds several
/// instance locks at once (a concurrent PATCH of one instance only delays that
/// instance). A failure is reported, never propagated: the remaining instances
/// still receive the rotated credential.
async fn restart_attached(
    state: &AppState,
    provider_id: &str,
) -> Result<Vec<ProviderRestart>, ApiError> {
    let linked = state
        .db
        .provider_instances(provider_id)
        .map_err(ApiError::from)?;
    let mut report = Vec::with_capacity(linked.len());
    for (id, _name) in linked {
        report.push(restart_one(state, &id).await);
    }
    Ok(report)
}

/// Recreate one instance with the new provider secret and wait for health.
async fn restart_one(state: &AppState, id: &str) -> ProviderRestart {
    let lock = state.locks.for_id(id);
    let _guard = lock.lock().await;

    let Some(spec) = state.db.get(id).ok().flatten() else {
        return ProviderRestart {
            id: id.to_string(),
            status: RESTART_FAILED.to_string(),
            error: Some("the instance disappeared during the rotation".to_string()),
        };
    };

    if let Err(err) = state.driver.up(&spec).await {
        let detail = format!("docker compose up failed: {}", err.detail.trim());
        let _ = state.db.set_status(id, Status::Error, Some(&detail));
        state
            .metrics
            .record_compose_action("up", crate::metrics::RESULT_ERROR);
        return ProviderRestart {
            id: id.to_string(),
            status: RESTART_FAILED.to_string(),
            error: Some(detail),
        };
    }
    state
        .metrics
        .record_compose_action("up", crate::metrics::RESULT_OK);

    match wait_until_healthy(state, &spec, ROTATE_TIMEOUT).await {
        Ok(status) => {
            let _ = state.db.set_status(id, status, None);
            ProviderRestart {
                id: id.to_string(),
                status: RESTART_HEALTHY.to_string(),
                error: None,
            }
        }
        Err(reason) => {
            let message = format!("the instance did not become healthy: {reason}");
            let _ = state.db.set_status(id, Status::Error, Some(&message));
            ProviderRestart {
                id: id.to_string(),
                status: RESTART_FAILED.to_string(),
                error: Some(message),
            }
        }
    }
}
