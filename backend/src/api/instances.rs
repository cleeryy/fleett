//! Instance endpoints.

use crate::api::error::ApiError;
use crate::api::{up_and_record, AppState};
use crate::models::{
    now_iso, validate_api_key_env, validate_base_url, validate_instance_id, validate_mem_limit,
    validate_model, validate_notes, validate_secret_value, validate_share_name, ChannelKind,
    ChannelRequest, ContainerInfo, CreateInstanceRequest, InstanceDetail, InstanceSpec,
    InstanceSummary, PatchInstanceRequest, ShareRef, Status, DEFAULT_API_KEY_ENV,
    DEFAULT_MEM_LIMIT_MB, DEFAULT_TEMPLATE,
};
use crate::render::{self, RenderError, TemplateSource};
use crate::store;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Deserialize)]
pub struct LogsQuery {
    #[serde(default)]
    pub tail: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    pub purge: Option<String>,
}

impl DeleteQuery {
    fn purge(&self) -> bool {
        matches!(
            self.purge.as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "on")
        )
    }
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub status: String,
}

struct Prepared {
    spec: InstanceSpec,
    template: TemplateSource,
    gate_token: Option<String>,
    channel_token: Option<String>,
    extras: BTreeMap<String, String>,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<InstanceSummary>>, ApiError> {
    let mut specs = state.db.list().map_err(ApiError::from)?;
    let mut summaries = Vec::with_capacity(specs.len());
    for spec in &mut specs {
        if let Ok(result) = state.driver.ps(spec).await {
            let status = effective_status(spec.status, result.status);
            if status != spec.status {
                spec.status = status;
                let _ = state
                    .db
                    .set_status(&spec.id, status, spec.last_error.as_deref());
            }
        }
        summaries.push(to_summary(spec));
    }
    Ok(Json(summaries))
}

pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<InstanceDetail>, ApiError> {
    let spec = load(&state, &id)?;
    let (status, container) = observe(&state, &spec).await;
    Ok(Json(to_detail(&state, &spec, status, container)))
}

pub async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateInstanceRequest>,
) -> Result<(StatusCode, Json<InstanceDetail>), ApiError> {
    let prepared = prepare_create(&state, &request)?;
    let mut spec = prepared.spec;

    let lock = state.locks.for_id(&spec.id);
    let _guard = lock.lock().await;

    if state.db.exists(&spec.id).map_err(ApiError::from)?
        || state.cfg.compose_path(&spec.id).exists()
    {
        return Err(ApiError::conflict(
            "instance_exists",
            format!("instance \"{}\" already exists", spec.id),
        ));
    }

    let rendered =
        render::render_instance(&state.cfg, &spec, &prepared.template).map_err(ApiError::from)?;
    let secrets = render::build_secrets(
        &spec,
        prepared.gate_token.as_deref(),
        prepared.channel_token.as_deref(),
        &prepared.extras,
    );

    state.db.insert_instance(&spec).map_err(ApiError::from)?;
    if let Err(err) = store::write_instance(&state.cfg, &spec, &rendered, &secrets, true) {
        let _ = state.db.delete_instance(&spec.id);
        return Err(ApiError::internal(format!(
            "failed to write instance files: {err}"
        )));
    }

    up_and_record(&state, &mut spec).await?;
    let (status, container) = match state.driver.ps(&spec).await {
        Ok(result) => (result.status, result.container),
        Err(_) => (Status::Running, None),
    };
    let stored = load(&state, &spec.id)?;
    Ok((
        StatusCode::CREATED,
        Json(to_detail(&state, &stored, status, container)),
    ))
}

pub async fn patch(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PatchInstanceRequest>,
) -> Result<Json<InstanceDetail>, ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let mut spec = load(&state, &id)?;
    let channel_token = apply_patch(&state, &mut spec, &request)?;

    let rendered = render_for_update(&state, &spec)?;
    let existing = store::read_secrets(&state.cfg, &id);
    let secrets = render::merge_secrets(
        existing,
        &spec,
        request.gate_token.as_deref(),
        channel_token.as_deref(),
    );

    store::write_instance(&state.cfg, &spec, &rendered, &secrets, false)
        .map_err(|err| ApiError::internal(format!("failed to write instance files: {err}")))?;
    spec.updated_at = now_iso();
    spec.status = Status::Created;
    spec.last_error = None;
    state.db.update_instance(&spec).map_err(ApiError::from)?;

    up_and_record(&state, &mut spec).await?;
    let (status, container) = match state.driver.ps(&spec).await {
        Ok(result) => (result.status, result.container),
        Err(_) => (Status::Running, None),
    };
    Ok(Json(to_detail(&state, &spec, status, container)))
}

pub async fn start(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<StatusResponse>, ApiError> {
    lifecycle(state, id, Lifecycle::Start).await
}

pub async fn stop(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<StatusResponse>, ApiError> {
    lifecycle(state, id, Lifecycle::Stop).await
}

pub async fn restart(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<StatusResponse>, ApiError> {
    lifecycle(state, id, Lifecycle::Restart).await
}

pub async fn logs(
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let tail = query.tail.unwrap_or(200);
    if !(1..=2000).contains(&tail) {
        return Err(ApiError::bad_request(
            "invalid_tail",
            "tail must be between 1 and 2000",
        ));
    }
    let spec = load(&state, &id)?;
    let logs = state
        .driver
        .logs(&spec, tail)
        .await
        .map_err(ApiError::driver)?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], logs).into_response())
}

pub async fn delete(
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let spec = load(&state, &id)?;
    let purge = query.purge();
    if let Err(err) = state.driver.down(&spec).await {
        let detail = err.detail.clone();
        let _ = state.db.set_status(&id, Status::Error, Some(&detail));
        return Err(ApiError::driver(err));
    }
    state.db.delete_instance(&id).map_err(ApiError::from)?;
    store::remove_instance(&state.cfg, &id, purge)
        .map_err(|err| ApiError::internal(format!("failed to remove instance files: {err}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Start,
    Stop,
    Restart,
}

async fn lifecycle(
    state: AppState,
    id: String,
    operation: Lifecycle,
) -> Result<Json<StatusResponse>, ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let spec = load(&state, &id)?;
    let result = match operation {
        Lifecycle::Start => state.driver.start(&spec).await,
        Lifecycle::Stop => state.driver.stop(&spec).await,
        Lifecycle::Restart => state.driver.restart(&spec).await,
    };
    if let Err(err) = result {
        let detail = err.detail.clone();
        let _ = state.db.set_status(&id, Status::Error, Some(&detail));
        return Err(ApiError::driver(err));
    }

    let fallback = if operation == Lifecycle::Stop {
        Status::Stopped
    } else {
        Status::Running
    };
    let status = state
        .driver
        .ps(&spec)
        .await
        .map(|result| result.status)
        .unwrap_or(fallback);
    state
        .db
        .set_status(&id, status, None)
        .map_err(ApiError::from)?;
    Ok(Json(StatusResponse {
        status: status.as_str().to_string(),
    }))
}

/// Render the files for a PATCH.
///
/// Instances adopted from disk have the template `custom`: their config was
/// written by hand (or by an older control plane), so the on-disk file is used
/// as the template instead of a built-in.
fn render_for_update(
    state: &AppState,
    spec: &InstanceSpec,
) -> Result<render::RenderedInstance, ApiError> {
    match render::load_template(&state.cfg, &spec.template) {
        Ok(template) => {
            render::render_instance(&state.cfg, spec, &template).map_err(ApiError::from)
        }
        Err(RenderError::UnknownTemplate(_)) if spec.template == "custom" => {
            let config_path = state.cfg.data_dir(&spec.id).join("config.yaml");
            let existing = std::fs::read_to_string(&config_path).map_err(|err| {
                ApiError::internal(format!(
                    "failed to read the existing config at {}: {err}",
                    config_path.display()
                ))
            })?;
            Ok(render::RenderedInstance {
                compose_yaml: render::render_compose(&state.cfg, spec).map_err(ApiError::from)?,
                config_yaml: render::render_config(spec, &existing).map_err(ApiError::from)?,
                soul_md: None,
            })
        }
        Err(err) => Err(ApiError::from(err)),
    }
}

fn load(state: &AppState, id: &str) -> Result<InstanceSpec, ApiError> {
    state
        .db
        .get(id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("instance \"{id}\" not found")))
}

async fn observe(state: &AppState, spec: &InstanceSpec) -> (Status, Option<ContainerInfo>) {
    match state.driver.ps(spec).await {
        Ok(result) => {
            let status = effective_status(spec.status, result.status);
            if status != spec.status {
                let _ = state
                    .db
                    .set_status(&spec.id, status, spec.last_error.as_deref());
            }
            (status, result.container)
        }
        Err(_) => (spec.status, None),
    }
}

/// `docker compose ps` reports `unknown` when no container exists; keep a
/// recorded failure visible instead of hiding it behind an empty runtime.
fn effective_status(cached: Status, observed: Status) -> Status {
    if observed == Status::Unknown && cached == Status::Error {
        Status::Error
    } else {
        observed
    }
}

fn to_summary(spec: &InstanceSpec) -> InstanceSummary {
    InstanceSummary {
        id: spec.id.clone(),
        name: spec.name.clone(),
        template: spec.template.clone(),
        model: spec.model.clone(),
        channel: spec.channel.as_str().to_string(),
        status: spec.status.as_str().to_string(),
        shares: spec.shares.iter().map(|share| share.name.clone()).collect(),
        created_at: spec.created_at.clone(),
    }
}

fn to_detail(
    state: &AppState,
    spec: &InstanceSpec,
    status: Status,
    container: Option<ContainerInfo>,
) -> InstanceDetail {
    InstanceDetail {
        id: spec.id.clone(),
        name: spec.name.clone(),
        template: spec.template.clone(),
        model: spec.model.clone(),
        base_url: spec.base_url.clone(),
        api_key_env: spec.api_key_env.clone(),
        channel: spec.channel.as_str().to_string(),
        status: status.as_str().to_string(),
        shares: spec.shares.clone(),
        mem_limit_mb: spec.mem_limit_mb,
        notes: spec.notes.clone(),
        created_at: spec.created_at.clone(),
        updated_at: spec.updated_at.clone(),
        last_error: spec.last_error.clone(),
        data_dir: state.cfg.data_dir(&spec.id).display().to_string(),
        env_keys: store::read_env_keys(&state.cfg, &spec.id),
        container,
        compose_yaml: store::read_compose(&state.cfg, &spec.id),
    }
}

fn prepare_create(state: &AppState, request: &CreateInstanceRequest) -> Result<Prepared, ApiError> {
    let id = request.id.as_deref().unwrap_or_default().trim().to_string();
    validate_instance_id(&id).map_err(|message| ApiError::bad_request("invalid_id", message))?;

    let template_id = request
        .template
        .as_deref()
        .unwrap_or(DEFAULT_TEMPLATE)
        .trim()
        .to_string();
    validate_share_name(&template_id)
        .map_err(|message| ApiError::bad_request("invalid_template", message))?;
    let template = render::load_template(&state.cfg, &template_id).map_err(ApiError::from)?;

    let model = match request.model.as_deref() {
        Some(value) => {
            let value = value.trim();
            validate_model(value)
                .map_err(|message| ApiError::bad_request("invalid_model", message))?;
            value.to_string()
        }
        None => template.meta.default_model.clone().ok_or_else(|| {
            ApiError::bad_request("invalid_model", "model is required for this template")
        })?,
    };

    let base_url = request
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError::bad_request("invalid_base_url", "base_url is required"))?;
    validate_base_url(&base_url)
        .map_err(|message| ApiError::bad_request("invalid_base_url", message))?;

    let api_key_env = request
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_API_KEY_ENV)
        .to_string();
    validate_api_key_env(&api_key_env)
        .map_err(|message| ApiError::bad_request("invalid_api_key_env", message))?;

    let (channel, channel_token) = normalize_channel(request.channel.as_ref())?;

    let shares = match &request.shares {
        Some(shares) => validate_share_refs(state, shares)?,
        None => Vec::new(),
    };

    let mem_limit_mb = request.mem_limit_mb.unwrap_or(DEFAULT_MEM_LIMIT_MB);
    validate_mem_limit(mem_limit_mb)
        .map_err(|message| ApiError::bad_request("invalid_mem_limit", message))?;

    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&id)
        .to_string();
    if name.chars().count() > 128 {
        return Err(ApiError::bad_request(
            "invalid_name",
            "name must be at most 128 characters",
        ));
    }

    let notes = request.notes.clone().unwrap_or_default();
    validate_notes(&notes).map_err(|message| ApiError::bad_request("invalid_notes", message))?;

    let extras = validate_extras(request.env.as_ref())?;
    let gate_token = request
        .gate_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(token) = &gate_token {
        validate_secret_value(token)
            .map_err(|message| ApiError::bad_request("invalid_gate_token", message))?;
    }

    let now = now_iso();
    Ok(Prepared {
        spec: InstanceSpec {
            id,
            name,
            template: template.id.clone(),
            model,
            base_url,
            api_key_env,
            channel,
            shares,
            mem_limit_mb,
            notes,
            status: Status::Created,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        },
        template,
        gate_token,
        channel_token,
        extras,
    })
}

fn apply_patch(
    state: &AppState,
    spec: &mut InstanceSpec,
    request: &PatchInstanceRequest,
) -> Result<Option<String>, ApiError> {
    if let Some(name) = request.name.as_deref() {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::bad_request(
                "invalid_name",
                "name must not be empty",
            ));
        }
        if name.chars().count() > 128 {
            return Err(ApiError::bad_request(
                "invalid_name",
                "name must be at most 128 characters",
            ));
        }
        spec.name = name.to_string();
    }
    if let Some(model) = request.model.as_deref() {
        let model = model.trim();
        validate_model(model).map_err(|message| ApiError::bad_request("invalid_model", message))?;
        spec.model = model.to_string();
    }
    if let Some(base_url) = request.base_url.as_deref() {
        let base_url = base_url.trim();
        validate_base_url(base_url)
            .map_err(|message| ApiError::bad_request("invalid_base_url", message))?;
        spec.base_url = base_url.to_string();
    }
    if let Some(api_key_env) = request.api_key_env.as_deref() {
        let api_key_env = api_key_env.trim();
        validate_api_key_env(api_key_env)
            .map_err(|message| ApiError::bad_request("invalid_api_key_env", message))?;
        spec.api_key_env = api_key_env.to_string();
    }
    if let Some(mem_limit_mb) = request.mem_limit_mb {
        validate_mem_limit(mem_limit_mb)
            .map_err(|message| ApiError::bad_request("invalid_mem_limit", message))?;
        spec.mem_limit_mb = mem_limit_mb;
    }
    if let Some(notes) = request.notes.as_deref() {
        validate_notes(notes).map_err(|message| ApiError::bad_request("invalid_notes", message))?;
        spec.notes = notes.to_string();
    }
    if let Some(shares) = &request.shares {
        spec.shares = validate_share_refs(state, shares)?;
    }

    let mut channel_token = None;
    if let Some(channel) = &request.channel {
        let (kind, token) = normalize_channel(Some(channel))?;
        spec.channel = kind;
        channel_token = token;
    }
    Ok(channel_token)
}

fn normalize_channel(
    request: Option<&ChannelRequest>,
) -> Result<(ChannelKind, Option<String>), ApiError> {
    let Some(channel) = request else {
        return Ok((ChannelKind::None, None));
    };
    let kind = channel
        .kind
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("invalid_channel", "channel.type is required"))?;
    let kind = ChannelKind::parse(kind).ok_or_else(|| {
        ApiError::bad_request(
            "invalid_channel",
            format!("unsupported channel type \"{kind}\" (expected telegram, discord or none)"),
        )
    })?;
    let token = channel
        .token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(token) = &token {
        validate_secret_value(token)
            .map_err(|message| ApiError::bad_request("invalid_channel_token", message))?;
    }
    if kind == ChannelKind::None {
        return Ok((kind, None));
    }
    Ok((kind, token))
}

fn validate_share_refs(state: &AppState, shares: &[ShareRef]) -> Result<Vec<ShareRef>, ApiError> {
    let mut seen = BTreeSet::new();
    let mut validated = Vec::with_capacity(shares.len());
    for share in shares {
        validate_share_name(&share.name)
            .map_err(|message| ApiError::bad_request("invalid_share", message))?;
        if !seen.insert(share.name.clone()) {
            return Err(ApiError::bad_request(
                "duplicate_share",
                format!("share \"{}\" is listed twice", share.name),
            ));
        }
        if !state.db.share_exists(&share.name).map_err(ApiError::from)? {
            return Err(ApiError::bad_request(
                "unknown_share",
                format!("share \"{}\" does not exist", share.name),
            ));
        }
        validated.push(share.clone());
    }
    validated.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(validated)
}

fn validate_extras(
    extras: Option<&BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, ApiError> {
    let mut out = BTreeMap::new();
    let Some(extras) = extras else {
        return Ok(out);
    };
    for (key, value) in extras {
        if crate::models::validate_secret_name(key).is_err() {
            return Err(ApiError::bad_request(
                "invalid_env",
                format!("\"{key}\" is not a valid environment variable name"),
            ));
        }
        validate_secret_value(value)
            .map_err(|message| ApiError::bad_request("invalid_env", message))?;
        out.insert(key.clone(), value.clone());
    }
    Ok(out)
}
