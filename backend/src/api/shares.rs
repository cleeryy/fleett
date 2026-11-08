//! Share endpoints: named directories mounted into one or more instances.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::{apply_compose, AppState};
use crate::auth::AuthContext;
use crate::models::{
    now_iso, validate_share_name, AttachShareRequest, CreateShareRequest, DetachShareRequest,
    ShareInfo, ShareInstance, ShareMode, ShareRef,
};
use crate::store;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use std::collections::HashMap;

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<ShareInfo>>, ApiError> {
    let shares = state.db.list_shares().map_err(ApiError::from)?;
    let links = state.db.all_links().map_err(ApiError::from)?;
    let mut grouped: HashMap<String, Vec<ShareInstance>> = HashMap::new();
    for (name, instance_id, mode) in links {
        grouped.entry(name).or_default().push(ShareInstance {
            id: instance_id,
            mode: mode.as_str().to_string(),
        });
    }
    let out = shares
        .into_iter()
        .map(|share| ShareInfo {
            path: state.cfg.share_dir(&share.name).display().to_string(),
            instances: grouped.remove(&share.name).unwrap_or_default(),
            name: share.name,
        })
        .collect();
    Ok(Json(out))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateShareRequest>,
) -> Result<(StatusCode, Json<ShareInfo>), ApiError> {
    let name = request
        .name
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    validate_share_name(&name)
        .map_err(|message| ApiError::bad_request("invalid_share", message))?;
    if state.db.share_exists(&name).map_err(ApiError::from)? {
        return Err(ApiError::conflict(
            "share_exists",
            format!("share \"{name}\" already exists"),
        ));
    }
    store::ensure_share_dir(&state.cfg, &name)
        .map_err(|err| ApiError::internal(format!("failed to create share directory: {err}")))?;
    state.db.insert_share(&name).map_err(ApiError::from)?;
    audit::record(&state, &context, "share.create", &name, None);
    Ok((
        StatusCode::CREATED,
        Json(ShareInfo {
            path: state.cfg.share_dir(&name).display().to_string(),
            name,
            instances: Vec::new(),
        }),
    ))
}

pub async fn delete(
    Path(name): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    if !state.db.share_exists(&name).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!("share \"{name}\" not found")));
    }
    let links = state.db.links_for_share(&name).map_err(ApiError::from)?;
    if !links.is_empty() {
        let attached: Vec<String> = links
            .iter()
            .map(|(id, mode)| format!("{id} ({})", mode.as_str()))
            .collect();
        return Err(ApiError::conflict(
            "share_attached",
            format!(
                "share \"{name}\" is still attached to {}",
                attached.join(", ")
            ),
        ));
    }
    store::remove_share_dir(&state.cfg, &name)
        .map_err(|err| ApiError::internal(format!("failed to delete share directory: {err}")))?;
    state.db.delete_share(&name).map_err(ApiError::from)?;
    audit::record(&state, &context, "share.delete", &name, None);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn attach(
    Path(name): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<AttachShareRequest>,
) -> Result<StatusCode, ApiError> {
    let mode = match request.mode.as_deref() {
        None => ShareMode::Rw,
        Some(value) => ShareMode::parse(value).ok_or_else(|| {
            ApiError::bad_request("invalid_mode", "mode must be \"rw\" or \"ro\"")
        })?,
    };
    if !state.db.share_exists(&name).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!("share \"{name}\" not found")));
    }
    if !state.cfg.share_dir(&name).is_dir() {
        store::ensure_share_dir(&state.cfg, &name).map_err(|err| {
            ApiError::internal(format!("failed to create share directory: {err}"))
        })?;
    }

    let instance = request
        .instance
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    if instance.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_instance",
            "instance is required",
        ));
    }

    let lock = state.locks.for_id(&instance);
    let _guard = lock.lock().await;
    let mut spec = state
        .db
        .get(&instance)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("instance \"{instance}\" not found")))?;

    match spec.shares.iter_mut().find(|share| share.name == name) {
        Some(existing) => existing.mode = mode,
        None => spec.shares.push(ShareRef {
            name: name.clone(),
            mode,
        }),
    }
    spec.shares
        .sort_by(|left, right| left.name.cmp(&right.name));
    spec.updated_at = now_iso();

    state.db.update_instance(&spec).map_err(ApiError::from)?;
    apply_compose(&state, &mut spec).await?;
    audit::record(
        &state,
        &context,
        "share.attach",
        &name,
        Some(serde_json::json!({ "instance": instance, "mode": mode.as_str() })),
    );
    Ok(StatusCode::NO_CONTENT)
}

pub async fn detach(
    Path(name): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<DetachShareRequest>,
) -> Result<StatusCode, ApiError> {
    if !state.db.share_exists(&name).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!("share \"{name}\" not found")));
    }

    let instance = request
        .instance
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    if instance.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_instance",
            "instance is required",
        ));
    }

    let lock = state.locks.for_id(&instance);
    let _guard = lock.lock().await;
    let mut spec = state
        .db
        .get(&instance)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("instance \"{instance}\" not found")))?;

    let before = spec.shares.len();
    spec.shares.retain(|share| share.name != name);
    if spec.shares.len() == before {
        return Ok(StatusCode::NO_CONTENT);
    }
    spec.updated_at = now_iso();

    state.db.update_instance(&spec).map_err(ApiError::from)?;
    apply_compose(&state, &mut spec).await?;
    audit::record(
        &state,
        &context,
        "share.detach",
        &name,
        Some(serde_json::json!({ "instance": instance })),
    );
    Ok(StatusCode::NO_CONTENT)
}
