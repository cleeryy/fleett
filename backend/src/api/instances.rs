//! Instance endpoints.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::json::ApiJson;
use crate::api::{up_and_record, AppState};
use crate::auth::AuthContext;
use crate::config::EgressMode;
use crate::driver::LogsOptions;
use crate::egress;
use crate::envs::{self, EnvWrite};
use crate::instance_archive::{ArchiveError, MAX_ARCHIVE_BYTES};
use crate::logs;
use crate::models::{
    env_key_is_reserved, now_iso, validate_api_key_env, validate_base_url, validate_cpu_limit,
    validate_env_key, validate_env_value, validate_instance_id, validate_mem_limit, validate_model,
    validate_notes, validate_secret_value, validate_share_name, ChannelKind, ChannelRequest,
    CloneInstanceRequest, ContainerInfo, CreateInstanceRequest, EnvInput, EnvVarInput, HealPolicy,
    Health, InstanceDetail, InstanceSpec, InstanceSummary, PatchInstanceRequest, ShareRef, Status,
    TemplateMeta, DEFAULT_API_KEY_ENV, DEFAULT_CPU_LIMIT, DEFAULT_HEAL_BACKOFF_S,
    DEFAULT_MAX_RESTARTS, DEFAULT_MEM_LIMIT_MB, DEFAULT_TEMPLATE, DEFAULT_UNHEALTHY_AFTER,
    GATE_TOKEN_ENV, HEALTH_UNHEALTHY_MESSAGE,
};
use crate::render::{self, RenderError, TemplateSource};
use crate::store;
use axum::body::Body;
use axum::extract::{Extension, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tokio::io::AsyncWriteExt;

/// Lines returned when `?tail=` is omitted.
pub const DEFAULT_LOGS_TAIL: u32 = 200;
/// Upper bound of `?tail=`.
pub const MAX_LOGS_TAIL: u32 = 2000;
/// Window used by `GET /api/instances/{id}/errors` when `?since=` is omitted.
pub const DEFAULT_ERRORS_SINCE: &str = "24h";
/// Lines scanned by the error counter.
pub const ERRORS_TAIL: u32 = 2000;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogsQuery {
    #[serde(default)]
    pub tail: Option<u32>,
    /// Docker duration (`10m`, `2h`) or an RFC 3339 timestamp.
    #[serde(default)]
    pub since: Option<String>,
    /// Regex the line must match.
    #[serde(default)]
    pub grep: Option<String>,
    /// `error` or `warn`.
    #[serde(default)]
    pub level: Option<String>,
    /// `text` (default) or `json`.
    #[serde(default)]
    pub format: Option<String>,
}

/// Response format of the logs endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogFormat {
    Text,
    Json,
}

impl LogFormat {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "text" | "plain" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ErrorsQuery {
    /// Docker duration or RFC 3339 timestamp (default `24h`).
    #[serde(default)]
    pub since: Option<String>,
}

/// Recent error/warn lines of one instance, without the full log.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorsResponse {
    pub count: usize,
    pub last: Option<String>,
    pub last_at: Option<String>,
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
    env: Vec<EnvWrite>,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<InstanceSummary>>, ApiError> {
    let mut specs = state.db.list().map_err(ApiError::from)?;
    let mut summaries = Vec::with_capacity(specs.len());
    for spec in &mut specs {
        let mut health = Health::Unknown;
        if let Ok(result) = state.driver.ps(spec).await {
            health = result.health;
            let status = effective_status(spec.status, result.status);
            if status != spec.status {
                spec.status = status;
                let _ = state
                    .db
                    .set_status(&spec.id, status, spec.last_error.as_deref());
            }
            record_health(&state, spec, health);
        }
        summaries.push(to_summary(spec, health));
    }
    Ok(Json(summaries))
}

pub async fn get(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<InstanceDetail>, ApiError> {
    let mut spec = load(&state, &id)?;
    let (status, health, container) = observe(&state, &mut spec).await;
    record_inspect_error(&state, &mut spec, health).await;
    let warnings = egress::warnings(&state.cfg, spec.allow_lan);
    Ok(Json(to_detail(
        &state, &spec, status, health, container, warnings,
    )))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    ApiJson(request): ApiJson<CreateInstanceRequest>,
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

    enforce_caps(&state, None, &spec)?;
    check_egress_policy(&state, &spec).await?;
    ensure_lan_network(&state, &spec).await?;

    let values = render::build_secrets(
        &spec,
        prepared.gate_token.as_deref(),
        prepared.channel_token.as_deref(),
        &env_values(&prepared.env),
    );
    let forced = token_keys(
        &spec,
        prepared.gate_token.is_some(),
        prepared.channel_token.is_some(),
    );
    let entries = env_entries(values, &env_flags(&prepared.env), &forced);
    // Refuse a create that could not store its secrets *before* writing any
    // file, so a refused create leaves nothing behind.
    envs::check_storable(&state.cfg, &entries).map_err(ApiError::from)?;
    let mut env_keys: Vec<String> = entries.iter().map(|entry| entry.key.clone()).collect();
    crate::api::push_provider_key(&mut env_keys, &spec);

    let rendered = render::render_instance(&state.cfg, &spec, &prepared.template, &env_keys)
        .map_err(ApiError::from)?;

    state.db.insert_instance(&spec).map_err(ApiError::from)?;
    if let Err(err) = envs::replace(&state.cfg, &state.db, &spec.id, &entries) {
        let _ = state.db.delete_instance(&spec.id);
        return Err(ApiError::from(err));
    }
    if let Err(err) = store::write_instance(&state.cfg, &spec, &rendered, true) {
        let _ = state.db.delete_instance(&spec.id);
        return Err(ApiError::internal(format!(
            "failed to write instance files: {err}"
        )));
    }

    up_and_record(&state, &mut spec).await?;
    let (status, health, container) = match state.driver.ps(&spec).await {
        Ok(result) => (result.status, result.health, result.container),
        Err(_) => (Status::Running, Health::Unknown, None),
    };
    let mut stored = load(&state, &spec.id)?;
    record_health(&state, &mut stored, health);
    audit::record(
        &state,
        &context,
        "instance.create",
        &stored.id,
        Some(serde_json::json!({
            "name": stored.name,
            "template": stored.template,
            "allow_lan": stored.allow_lan,
        })),
    );
    let warnings = egress::warnings(&state.cfg, stored.allow_lan);
    Ok((
        StatusCode::CREATED,
        Json(to_detail(
            &state, &stored, status, health, container, warnings,
        )),
    ))
}

/// Duplicate an instance under a new id.
///
/// The clone is a completely independent instance: its own compose project,
/// container, data directory and database row. Secrets are never copied — the
/// request must replace every token the source holds, and a missing token
/// refuses the clone *before anything is written*.
pub async fn clone(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CloneInstanceRequest>,
) -> Result<(StatusCode, Json<InstanceDetail>), ApiError> {
    let new_id = request
        .new_id
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    validate_instance_id(&new_id)
        .map_err(|message| ApiError::bad_request("invalid_id", message))?;
    if new_id == id {
        return Err(ApiError::bad_request(
            "invalid_id",
            "new_id must differ from the source instance",
        ));
    }
    let shares_inherit = request.shares_inherit.unwrap_or(true);
    let copy_data = request.copy_data.unwrap_or(true);

    // Two instance locks, always taken in the same order so two concurrent
    // clones of the same pair cannot deadlock on each other.
    let (first, second) = if id < new_id {
        (id.clone(), new_id.clone())
    } else {
        (new_id.clone(), id.clone())
    };
    let first_lock = state.locks.for_id(&first);
    let _first = first_lock.lock_owned().await;
    let second_lock = state.locks.for_id(&second);
    let _second = second_lock.lock_owned().await;

    let source = load(&state, &id)?;
    if state.db.exists(&new_id).map_err(ApiError::from)? || state.cfg.compose_path(&new_id).exists()
    {
        return Err(ApiError::conflict(
            "instance_exists",
            format!("instance \"{new_id}\" already exists"),
        ));
    }

    let gate_token = normalize_token(request.gate_token.as_deref(), "invalid_gate_token")?;
    let channel_token = normalize_token(request.channel_token.as_deref(), "invalid_channel_token")?;

    // The clone inherits the provider of its source: a provider is a reference
    // to a fleet-wide credential, not a per-instance secret. An explicit
    // `provider` overrides it, and an empty string detaches the clone.
    let clone_provider = match request.provider.as_deref() {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => Some(load_provider(&state, value.trim())?),
        None => match source.provider.as_deref() {
            Some(provider_id) => state
                .db
                .provider(provider_id)
                .map_err(ApiError::from)?
                .map(|record| record.spec),
            None => None,
        },
    };

    // Tokens are never inherited: every token the source holds has to be
    // replaced by the request. Checked before the first write so a refused
    // clone leaves nothing behind. Non-secret variables, on the other hand,
    // are configuration and are copied. A provider supplies the gate key at
    // runtime, so a clone that keeps one needs no gate token.
    let source_env = envs::load(&state.cfg, &state.db, &id).map_err(ApiError::from)?;
    let source_secrets: BTreeMap<String, String> = source_env
        .iter()
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect();
    let mut missing: Vec<String> = Vec::new();
    if gate_token.is_none() && clone_provider.is_none() {
        missing.extend(
            gate_env_keys(&source)
                .into_iter()
                .filter(|key| source_secrets.contains_key(*key))
                .map(str::to_string),
        );
    }
    if channel_token.is_none() {
        if let Some(env) = source.channel.token_env() {
            if source_secrets.contains_key(env) {
                missing.push(env.to_string());
            }
        }
    }
    if !missing.is_empty() {
        return Err(ApiError::bad_request(
            "clone_token_required",
            format!(
                "the clone never inherits secrets: supply the replacement token(s) for {}",
                missing.join(", ")
            ),
        )
        .with_details(serde_json::json!({
            "source": id,
            "missing": missing,
        })));
    }

    let mut clone = source.clone();
    clone.id = new_id.clone();
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
        clone.name = name.to_string();
    } else {
        clone.name = clone_default_name(&source.name);
    }
    if let Some(model) = request.model.as_deref() {
        let model = model.trim();
        validate_model(model).map_err(|message| ApiError::bad_request("invalid_model", message))?;
        clone.model = model.to_string();
    }
    if request.provider.is_some() {
        // An explicit provider owns the endpoint and the variable name the
        // clone draws its key from; the source values would point elsewhere.
        match &clone_provider {
            Some(provider) => {
                clone.base_url = provider.base_url.clone();
                clone.api_key_env = provider.api_key_env.clone();
                clone.provider = Some(provider.id.clone());
            }
            None => clone.provider = None,
        }
    }
    clone.shares = if shares_inherit {
        source
            .shares
            .iter()
            .filter(|share| state.db.share_exists(&share.name).unwrap_or(false))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    clone.status = Status::Created;
    clone.last_error = None;
    // The target image is inherited, but the clone has no upgrade history of
    // its own: the previous tag belongs to the source instance.
    clone.previous_image = None;
    let now = now_iso();
    clone.created_at = now.clone();
    clone.updated_at = now;

    enforce_caps(&state, None, &clone)?;
    check_egress_policy(&state, &clone).await?;
    ensure_lan_network(&state, &clone).await?;

    let template = clone_template(&state, &source)?;
    let inherited: BTreeMap<String, String> = source_env
        .iter()
        .filter(|entry| !entry.secret)
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect();
    let values = render::build_secrets(
        &clone,
        gate_token.as_deref(),
        channel_token.as_deref(),
        &inherited,
    );
    let flags: BTreeMap<String, bool> = inherited.keys().map(|key| (key.clone(), false)).collect();
    let forced = token_keys(&clone, gate_token.is_some(), channel_token.is_some());
    let entries = env_entries(values, &flags, &forced);
    envs::check_storable(&state.cfg, &entries).map_err(ApiError::from)?;
    let mut env_keys: Vec<String> = entries.iter().map(|entry| entry.key.clone()).collect();
    crate::api::push_provider_key(&mut env_keys, &clone);
    let rendered = render::render_instance(&state.cfg, &clone, &template, &env_keys)
        .map_err(ApiError::from)?;

    // From here on the clone exists: any failure rolls it back completely.
    state.db.insert_instance(&clone).map_err(ApiError::from)?;
    if let Err(err) = write_clone(&state, &source.id, &clone, &entries, &rendered, copy_data).await
    {
        rollback_clone(&state, &new_id);
        return Err(err);
    }

    up_and_record(&state, &mut clone).await?;
    let (status, health, container) = match state.driver.ps(&clone).await {
        Ok(result) => (result.status, result.health, result.container),
        Err(_) => (Status::Running, Health::Unknown, None),
    };
    let mut stored = load(&state, &new_id)?;
    record_health(&state, &mut stored, health);
    audit::record(
        &state,
        &context,
        "instance.clone",
        &stored.id,
        Some(serde_json::json!({
            "source": source.id,
            "name": stored.name,
            "model": stored.model,
            "copy_data": copy_data,
            "shares_inherit": shares_inherit,
        })),
    );
    let warnings = egress::warnings(&state.cfg, stored.allow_lan);
    Ok((
        StatusCode::CREATED,
        Json(to_detail(
            &state, &stored, status, health, container, warnings,
        )),
    ))
}

/// `GET /api/instances/{id}/export`: portable `.tgz` of one instance.
///
/// The archive carries the manifest, the compose file and the full Hermes
/// home. Shares and secrets are excluded: a share is fleet-wide, and a token
/// must never leave the host in a file an operator copies around.
pub async fn export(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let spec = load(&state, &id)?;
    // The stored id, never the raw path segment, names the download.
    let filename = spec.id.clone();
    let cfg = state.cfg.clone();
    let bytes = tokio::task::spawn_blocking(move || crate::instance_archive::export(&cfg, &spec))
        .await
        .map_err(|err| ApiError::internal(format!("the export task failed: {err}")))?
        .map_err(|err| match err {
            ArchiveError::Io(err) => ApiError::internal(format!("cannot build the archive: {err}")),
            other => ApiError::internal(other.to_string()),
        })?;

    Ok((
        [
            (header::CONTENT_TYPE, "application/gzip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}.tgz\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Query of `POST /api/instances/import`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ImportQuery {
    /// Id of the new instance (required, validated by the handler for a 400).
    #[serde(default)]
    pub id: Option<String>,
    /// Start the instance once it is restored (default `false`).
    #[serde(default)]
    pub start: Option<bool>,
}

/// `POST /api/instances/import?id=<new_id>&start=false`.
///
/// The body is the raw gzip archive returned by [`export`]. Nothing is started
/// unless `start=true`, and the imported instance has no secret at all: the
/// response carries the warnings that tell the operator which tokens to PATCH.
pub async fn import(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<ImportQuery>,
    request: Request,
) -> Result<(StatusCode, Json<InstanceDetail>), ApiError> {
    let id = query.id.as_deref().unwrap_or_default().trim().to_string();
    validate_instance_id(&id).map_err(|message| ApiError::bad_request("invalid_id", message))?;
    let start = query.start.unwrap_or(false);

    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    if state.db.exists(&id).map_err(ApiError::from)? || state.cfg.instance_dir(&id).exists() {
        return Err(ApiError::conflict(
            "instance_exists",
            format!("instance \"{id}\" already exists"),
        ));
    }

    // The upload is streamed to disk: a Hermes home can be far larger than the
    // JSON body limit, and the compressed size cap is enforced frame by frame.
    let tmp_dir = state.cfg.root.join("tmp");
    tokio::fs::create_dir_all(&tmp_dir)
        .await
        .map_err(|err| ApiError::internal(format!("cannot prepare the upload: {err}")))?;
    let token = uuid::Uuid::new_v4().to_string();
    let archive_path = tmp_dir.join(format!("import-{token}.tgz"));
    let staging = tmp_dir.join(format!("import-{token}"));

    if let Err(err) = write_body_to_file(request.into_body(), &archive_path).await {
        let _ = tokio::fs::remove_file(&archive_path).await;
        return Err(err);
    }
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|err| ApiError::internal(format!("cannot prepare the import: {err}")))?;

    let outcome = import_archive(&state, &id, start, &archive_path, &staging).await;

    let _ = tokio::fs::remove_dir_all(&staging).await;
    let _ = tokio::fs::remove_file(&archive_path).await;

    let (status, detail) = outcome?;
    audit::record(
        &state,
        &context,
        "instance.import",
        &id,
        Some(serde_json::json!({
            "name": detail.name,
            "template": detail.template,
            "started": start,
        })),
    );
    Ok((status, Json(detail)))
}

/// Stream one request body to `path`, enforcing the compressed size cap.
async fn write_body_to_file(body: Body, path: &std::path::Path) -> Result<u64, ApiError> {
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|err| ApiError::internal(format!("cannot stage the upload: {err}")))?;
    let mut body = body;
    let mut written = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|err| {
            ApiError::bad_request(
                "invalid_archive",
                format!("cannot read the request body: {err}"),
            )
        })?;
        let Some(chunk) = frame.data_ref() else {
            continue;
        };
        written = written.saturating_add(chunk.len() as u64);
        if written > MAX_ARCHIVE_BYTES {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "archive_too_large",
                format!(
                    "the archive must be at most {} MiB compressed",
                    MAX_ARCHIVE_BYTES / (1024 * 1024)
                ),
            ));
        }
        file.write_all(chunk)
            .await
            .map_err(|err| ApiError::internal(format!("cannot stage the upload: {err}")))?;
    }
    file.flush()
        .await
        .map_err(|err| ApiError::internal(format!("cannot stage the upload: {err}")))?;
    if written == 0 {
        return Err(ApiError::bad_request(
            "empty_archive",
            "the request body is empty; send the .tgz produced by GET /api/instances/{id}/export",
        ));
    }
    Ok(written)
}

/// Extract, validate, write and (optionally) start one imported instance.
///
/// Every failure after the first write rolls the import back completely: a
/// refused import never leaves an instance directory or a database row behind.
async fn import_archive(
    state: &AppState,
    id: &str,
    start: bool,
    archive_path: &std::path::Path,
    staging: &std::path::Path,
) -> Result<(StatusCode, InstanceDetail), ApiError> {
    let source = archive_path.to_path_buf();
    let data_target = staging.join("data");
    let extraction = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&data_target)?;
        let file = std::fs::File::open(&source)?;
        crate::instance_archive::extract(std::io::BufReader::new(file), &data_target)
    })
    .await
    .map_err(|err| ApiError::internal(format!("the import task failed: {err}")))?
    .map_err(import_error)?;

    let mut spec = crate::instance_archive::spec_from_manifest(&extraction.manifest.instance, id)
        .map_err(import_error)?;
    spec.template = crate::instance_archive::settle_template(&state.cfg, &spec.template);
    // Keep the image the archived instance ran; an archive without one falls
    // back to this build's `FLEETT_IMAGE`.
    spec.image = extraction
        .compose_yaml
        .as_deref()
        .and_then(store::compose_image)
        .unwrap_or_else(|| state.cfg.image.clone());
    let mut env_keys = extraction
        .compose_yaml
        .as_deref()
        .map(crate::instance_archive::compose_env_keys)
        .unwrap_or_default();
    crate::api::push_provider_key(&mut env_keys, &spec);

    let mut warnings = extraction.warnings;
    // A provider is a local reference: an archive never carries the secret, so
    // an unknown one is dropped with a warning instead of failing the import.
    if let Some(provider_id) = spec.provider.clone() {
        if state
            .db
            .provider(&provider_id)
            .map_err(ApiError::from)?
            .is_none()
        {
            warnings.push(format!(
                "the archive references provider \"{provider_id}\", which does not exist on this \
                 host; the instance keeps its own token instead"
            ));
            spec.provider = None;
        }
    }
    let mut missing_shares = Vec::new();
    spec.shares
        .retain(|share| match state.db.share_exists(&share.name) {
            Ok(true) => true,
            _ => {
                missing_shares.push(share.name.clone());
                false
            }
        });
    if !missing_shares.is_empty() {
        warnings.push(format!(
            "share(s) {} are not present on this host; create them and attach them again",
            missing_shares.join(", ")
        ));
    }
    let mut expected = vec![spec.api_key_env.clone()];
    if let Some(env) = spec.channel.token_env() {
        expected.push(env.to_string());
    }
    expected.sort();
    expected.dedup();
    warnings.push(format!(
        "the archive carries no secrets: PATCH the instance or PUT its env to set {}",
        expected.join(", ")
    ));

    enforce_caps(state, None, &spec)?;
    check_egress_policy(state, &spec).await?;
    ensure_lan_network(state, &spec).await?;

    let compose = render::render_compose(&state.cfg, &spec, &env_keys).map_err(ApiError::from)?;

    // From here on the instance exists on disk: any failure rolls it back.
    let instance_dir = match store::create_instance_dir(&state.cfg, id) {
        Ok(dir) => dir,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(ApiError::conflict(
                "instance_exists",
                format!("instance \"{id}\" already exists"),
            ))
        }
        Err(err) => {
            return Err(ApiError::internal(format!(
                "cannot create the instance directory: {err}"
            )))
        }
    };
    if let Err(err) = std::fs::rename(staging.join("data"), state.cfg.data_dir(id)) {
        let _ = std::fs::remove_dir_all(&instance_dir);
        return Err(ApiError::internal(format!(
            "cannot move the imported data into place: {err}"
        )));
    }
    if let Err(err) = store::finalize_imported_data(&state.cfg, id) {
        let _ = std::fs::remove_dir_all(&instance_dir);
        return Err(ApiError::internal(format!(
            "cannot finalise the imported data: {err}"
        )));
    }
    if let Err(err) = state.db.insert_instance(&spec) {
        let _ = std::fs::remove_dir_all(&instance_dir);
        return Err(ApiError::from(err));
    }
    if let Err(err) = store::write_compose(&state.cfg, id, &compose) {
        let _ = state.db.delete_instance(id);
        let _ = std::fs::remove_dir_all(&instance_dir);
        return Err(ApiError::internal(format!(
            "cannot write the compose file: {err}"
        )));
    }

    if start {
        up_and_record(state, &mut spec).await?;
    }

    let (status, health, container) = if start {
        match state.driver.ps(&spec).await {
            Ok(result) => (result.status, result.health, result.container),
            Err(_) => (Status::Running, Health::Unknown, None),
        }
    } else {
        (Status::Created, Health::Unknown, None)
    };
    let mut stored = load(state, id)?;
    record_health(state, &mut stored, health);
    warnings.extend(egress::warnings(&state.cfg, stored.allow_lan));
    Ok((
        StatusCode::CREATED,
        to_detail(state, &stored, status, health, container, warnings),
    ))
}

fn import_error(err: ArchiveError) -> ApiError {
    match err {
        ArchiveError::MissingManifest => ApiError::bad_request("invalid_archive", err.to_string()),
        ArchiveError::Invalid(detail) => ApiError::bad_request("invalid_archive", detail),
        ArchiveError::Unsupported { .. } => {
            ApiError::bad_request("unsupported_archive", err.to_string())
        }
        ArchiveError::Manifest(detail) => ApiError::bad_request("invalid_manifest", detail),
        other => ApiError::internal(other.to_string()),
    }
}

pub async fn patch(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    ApiJson(request): ApiJson<PatchInstanceRequest>,
) -> Result<Json<InstanceDetail>, ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let mut spec = load(&state, &id)?;
    let channel_token = apply_patch(&state, &mut spec, &request)?;
    let gate_token = normalize_token(request.gate_token.as_deref(), "invalid_gate_token")?;
    enforce_caps(&state, Some(&id), &spec)?;
    check_egress_policy(&state, &spec).await?;
    ensure_lan_network(&state, &spec).await?;

    let existing = envs::load(&state.cfg, &state.db, &id).map_err(ApiError::from)?;
    let mut values = env_values_of(&existing);
    let mut flags = env_flags_of(&existing);
    let env_vars = request.env.clone().map(EnvInput::into_vars);
    apply_env_merge(&mut values, &mut flags, env_vars.as_deref())?;
    let values = render::merge_secrets(
        values,
        &spec,
        gate_token.as_deref(),
        channel_token.as_deref(),
    );
    let forced = token_keys(&spec, gate_token.is_some(), channel_token.is_some());
    let entries = env_entries(values, &flags, &forced);
    envs::check_storable(&state.cfg, &entries).map_err(ApiError::from)?;
    let mut env_keys: Vec<String> = entries.iter().map(|entry| entry.key.clone()).collect();
    crate::api::push_provider_key(&mut env_keys, &spec);
    let rendered = render_for_update(&state, &spec, &env_keys)?;
    // The new rows are durable before the compose file starts referencing the
    // new key names; the driver reads them on its next `docker compose` call.
    envs::replace(&state.cfg, &state.db, &id, &entries).map_err(ApiError::from)?;

    store::write_instance(&state.cfg, &spec, &rendered, false)
        .map_err(|err| ApiError::internal(format!("failed to write instance files: {err}")))?;
    spec.updated_at = now_iso();
    spec.status = Status::Created;
    spec.last_error = None;
    state.db.update_instance(&spec).map_err(ApiError::from)?;

    up_and_record(&state, &mut spec).await?;
    let (status, health, container) = match state.driver.ps(&spec).await {
        Ok(result) => (result.status, result.health, result.container),
        Err(_) => (Status::Running, Health::Unknown, None),
    };
    record_health(&state, &mut spec, health);
    audit::record(&state, &context, "instance.patch", &spec.id, None);
    let warnings = egress::warnings(&state.cfg, spec.allow_lan);
    Ok(Json(to_detail(
        &state, &spec, status, health, container, warnings,
    )))
}

/// Body of `PUT /api/instances/{id}/env`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PutEnvRequest {
    /// Canonical list (`[{key,value,secret}]`) or deprecated v0.4.0 map
    /// (`{"KEY": "value"}`, every entry stored as a secret).
    #[serde(default)]
    pub env: Option<EnvInput>,
}

/// Replace the whole environment of an instance.
///
/// A secret entry may be sent back without its value (`secret: true`,
/// `value: null`): the stored value is kept, so a console that never received
/// the secret can still round-trip the list. Every key the request leaves out
/// is deleted.
pub async fn put_env(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    ApiJson(request): ApiJson<PutEnvRequest>,
) -> Result<Json<InstanceDetail>, ApiError> {
    let vars = request.env.map(EnvInput::into_vars).ok_or_else(|| {
        ApiError::bad_request("invalid_env", "env is required: send {\"env\": [{...}]}")
    })?;

    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let mut spec = load(&state, &id)?;
    let existing = envs::load(&state.cfg, &state.db, &id).map_err(ApiError::from)?;
    let entries = resolve_full_env(&existing, &vars)?;

    envs::check_storable(&state.cfg, &entries).map_err(ApiError::from)?;
    envs::replace(&state.cfg, &state.db, &id, &entries).map_err(ApiError::from)?;
    crate::api::write_compose_file(&state, &spec)?;
    apply_env_state(&state, &mut spec)?;

    audit::record(
        &state,
        &context,
        "instance.env.update",
        &spec.id,
        Some(serde_json::json!({
            "keys": entries.iter().map(|entry| entry.key.clone()).collect::<Vec<_>>(),
            "secrets": entries.iter().filter(|entry| entry.secret).count(),
        })),
    );
    detail_after_apply(&state, &spec).await
}

/// `DELETE /api/instances/{id}/env/{key}`: remove one variable and apply.
pub async fn delete_env(
    Path((id, key)): Path<(String, String)>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let mut spec = load(&state, &id)?;
    let existing = envs::load(&state.cfg, &state.db, &id).map_err(ApiError::from)?;
    if !existing.iter().any(|entry| entry.key == key) {
        return Err(ApiError::not_found(format!(
            "instance \"{id}\" has no environment variable \"{key}\""
        )));
    }
    let entries: Vec<EnvWrite> = existing
        .into_iter()
        .filter(|entry| entry.key != key)
        .map(|entry| EnvWrite {
            key: entry.key,
            value: entry.value,
            secret: entry.secret,
        })
        .collect();

    envs::replace(&state.cfg, &state.db, &id, &entries).map_err(ApiError::from)?;
    crate::api::write_compose_file(&state, &spec)?;
    apply_env_state(&state, &mut spec)?;

    audit::record(
        &state,
        &context,
        "instance.env.delete",
        &spec.id,
        Some(serde_json::json!({ "key": key })),
    );
    up_and_record(&state, &mut spec).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Record the touch of a successful environment write.
fn apply_env_state(state: &AppState, spec: &mut InstanceSpec) -> Result<(), ApiError> {
    spec.updated_at = now_iso();
    spec.status = Status::Created;
    spec.last_error = None;
    state.db.update_instance(spec).map_err(ApiError::from)
}

/// `docker compose ps`, then the full detail of an instance that was just
/// restarted by an environment change.
async fn detail_after_apply(
    state: &AppState,
    spec: &InstanceSpec,
) -> Result<Json<InstanceDetail>, ApiError> {
    let mut spec = spec.clone();
    up_and_record(state, &mut spec).await?;
    let (status, health, container) = match state.driver.ps(&spec).await {
        Ok(result) => (result.status, result.health, result.container),
        Err(_) => (Status::Running, Health::Unknown, None),
    };
    record_health(state, &mut spec, health);
    let warnings = egress::warnings(&state.cfg, spec.allow_lan);
    Ok(Json(to_detail(
        state, &spec, status, health, container, warnings,
    )))
}

pub async fn start(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<StatusResponse>, ApiError> {
    lifecycle(state, id, Lifecycle::Start, context).await
}

pub async fn stop(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<StatusResponse>, ApiError> {
    lifecycle(state, id, Lifecycle::Stop, context).await
}

pub async fn restart(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<StatusResponse>, ApiError> {
    lifecycle(state, id, Lifecycle::Restart, context).await
}

pub async fn logs(
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let tail = query.tail.unwrap_or(DEFAULT_LOGS_TAIL);
    if !(1..=MAX_LOGS_TAIL).contains(&tail) {
        return Err(invalid_tail());
    }
    let since = match query
        .since
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(value) => Some(
            logs::normalize_since(value)
                .map_err(|message| ApiError::bad_request("invalid_since", message))?,
        ),
        None => None,
    };
    let format = match query
        .format
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => LogFormat::parse(value).ok_or_else(|| {
            ApiError::bad_request("invalid_format", "format must be \"text\" or \"json\"")
        })?,
        None => LogFormat::Text,
    };
    let filters =
        logs::Filters::new(query.grep.as_deref(), query.level.as_deref()).map_err(filter_error)?;

    let spec = load(&state, &id)?;
    let raw = state
        .driver
        .logs(&spec, &LogsOptions { tail, since })
        .await
        .map_err(ApiError::driver)?;
    let raw = logs::truncate(&raw, logs::MAX_SCAN_BYTES);
    let lines = filters.apply(raw);

    match format {
        LogFormat::Text => Ok((
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            logs::render_text(&lines, logs::MAX_RESPONSE_BYTES),
        )
            .into_response()),
        LogFormat::Json => Ok((
            [(header::CONTENT_TYPE, "application/json")],
            Json(logs::entries(&lines, logs::MAX_RESPONSE_BYTES)),
        )
            .into_response()),
    }
}

/// Recent error/warn lines of one instance.
///
/// Scans a bounded window (`since`, at most [`ERRORS_TAIL`] lines) so the web
/// console can show "3 errors, last at 12:04" without downloading the log.
pub async fn errors(
    Path(id): Path<String>,
    Query(query): Query<ErrorsQuery>,
    State(state): State<AppState>,
) -> Result<Json<ErrorsResponse>, ApiError> {
    let since = query
        .since
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_ERRORS_SINCE);
    let since = logs::normalize_since(since)
        .map_err(|message| ApiError::bad_request("invalid_since", message))?;

    let spec = load(&state, &id)?;
    let raw = state
        .driver
        .logs(
            &spec,
            &LogsOptions {
                tail: ERRORS_TAIL,
                since: Some(since),
            },
        )
        .await
        .map_err(ApiError::driver)?;
    let summary = logs::error_summary(logs::truncate(&raw, logs::MAX_SCAN_BYTES));
    Ok(Json(ErrorsResponse {
        count: summary.count,
        last: summary.last,
        last_at: summary.last_at,
    }))
}

pub async fn delete(
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let spec = load(&state, &id)?;
    let purge = query.purge();
    if let Err(err) = state.driver.down(&spec).await {
        let detail = err.detail.clone();
        let _ = state.db.set_status(&id, Status::Error, Some(&detail));
        state
            .metrics
            .record_compose_action("down", crate::metrics::RESULT_ERROR);
        return Err(ApiError::driver(err));
    }
    state
        .metrics
        .record_compose_action("down", crate::metrics::RESULT_OK);
    state.db.delete_instance(&id).map_err(ApiError::from)?;
    store::remove_instance(&state.cfg, &id, purge)
        .map_err(|err| ApiError::internal(format!("failed to remove instance files: {err}")))?;
    audit::record(
        &state,
        &context,
        "instance.delete",
        &id,
        Some(serde_json::json!({ "purge": purge })),
    );
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
    context: AuthContext,
) -> Result<Json<StatusResponse>, ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let spec = load(&state, &id)?;
    let action = match operation {
        Lifecycle::Start => "start",
        Lifecycle::Stop => "stop",
        Lifecycle::Restart => "restart",
    };
    let result = match operation {
        Lifecycle::Start => state.driver.start(&spec).await,
        Lifecycle::Stop => state.driver.stop(&spec).await,
        Lifecycle::Restart => state.driver.restart(&spec).await,
    };
    if let Err(err) = result {
        let detail = err.detail.clone();
        let _ = state.db.set_status(&id, Status::Error, Some(&detail));
        state
            .metrics
            .record_compose_action(action, crate::metrics::RESULT_ERROR);
        return Err(ApiError::driver(err));
    }
    state
        .metrics
        .record_compose_action(action, crate::metrics::RESULT_OK);

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
    let audit_action = match operation {
        Lifecycle::Start => "instance.start",
        Lifecycle::Stop => "instance.stop",
        Lifecycle::Restart => "instance.restart",
    };
    audit::record(&state, &context, audit_action, &id, None);
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
    env_keys: &[String],
) -> Result<render::RenderedInstance, ApiError> {
    // Keep the project already declared on disk: an instance created by an
    // older release stays on `hermes-<id>` until it is migrated explicitly.
    let project = store::declared_project(&state.cfg, &spec.id);
    let mut spec = spec.clone();
    crate::api::hydrate_previous_image(state, &mut spec);
    let spec = &spec;
    match render::load_template(&state.cfg, &spec.template) {
        Ok(template) => render::render_instance_with_project(
            &state.cfg,
            spec,
            &template,
            project.as_deref(),
            env_keys,
        )
        .map_err(ApiError::from),
        Err(RenderError::UnknownTemplate(_)) if spec.template == "custom" => {
            let config_path = state.cfg.data_dir(&spec.id).join("config.yaml");
            let existing = std::fs::read_to_string(&config_path).map_err(|err| {
                ApiError::internal(format!(
                    "failed to read the existing config at {}: {err}",
                    config_path.display()
                ))
            })?;
            Ok(render::RenderedInstance {
                compose_yaml: render::render_compose_with_project(
                    &state.cfg,
                    spec,
                    project.as_deref(),
                    env_keys,
                )
                .map_err(ApiError::from)?,
                config_yaml: render::render_config(spec, &existing).map_err(ApiError::from)?,
                soul_md: None,
            })
        }
        Err(err) => Err(ApiError::from(err)),
    }
}

/// Stored spec of one instance, or `404`.
pub(crate) fn load(state: &AppState, id: &str) -> Result<InstanceSpec, ApiError> {
    state
        .db
        .get(id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("instance \"{id}\" not found")))
}

async fn observe(
    state: &AppState,
    spec: &mut InstanceSpec,
) -> (Status, Health, Option<ContainerInfo>) {
    match state.driver.ps(spec).await {
        Ok(result) => {
            let status = effective_status(spec.status, result.status);
            if status != spec.status {
                let _ = state
                    .db
                    .set_status(&spec.id, status, spec.last_error.as_deref());
            }
            spec.status = status;
            record_health(state, spec, result.health);
            (status, result.health, result.container)
        }
        Err(_) => (spec.status, Health::Unknown, None),
    }
}

/// Keep `last_error` in sync with the container health.
///
/// An unhealthy container is still `running`, so the lifecycle status is left
/// alone; only `last_error` is set (and cleared once the probe answers again).
pub(crate) fn record_health(state: &AppState, spec: &mut InstanceSpec, health: Health) {
    let is_health_error = spec.last_error.as_deref() == Some(HEALTH_UNHEALTHY_MESSAGE);
    match health {
        Health::Unhealthy if !is_health_error => {
            let _ = state
                .db
                .set_last_error(&spec.id, Some(HEALTH_UNHEALTHY_MESSAGE));
            spec.last_error = Some(HEALTH_UNHEALTHY_MESSAGE.to_string());
        }
        Health::Healthy | Health::Starting if is_health_error => {
            let _ = state.db.set_last_error(&spec.id, None);
            spec.last_error = None;
        }
        _ => {}
    }
}

/// Enforce the host egress firewall policy on create and patch.
///
/// `allow_lan=true` always passes (with a warning in the detail): the operator
/// opted in explicitly. A restricted instance needs proof that the firewall is
/// installed for the current `fleett` subnet, i.e. a marker written by
/// `fleett egress install` whose subnet still matches. Without it (or with
/// `FLEETT_EGRESS=off`) the promise "no LAN access" cannot be kept, so the
/// request is refused with `409 egress_not_enforced`.
///
/// Lifecycle calls (start/stop/restart) never go through this check.
async fn check_egress_policy(state: &AppState, spec: &InstanceSpec) -> Result<(), ApiError> {
    if spec.allow_lan || state.cfg.egress == EgressMode::Off {
        return Ok(());
    }

    let marker_path = &state.cfg.egress_marker;
    let marker = egress::read_marker(marker_path);
    let current_subnet = match state.driver.network_subnet(&state.cfg.network).await {
        Ok(subnet) => subnet,
        Err(err) => {
            return Err(egress_conflict(
                format!(
                    "the egress firewall cannot be verified: the subnet of the \"{}\" docker \
                     network is unreadable ({})",
                    state.cfg.network, err.detail
                ),
                marker_path,
            ));
        }
    };

    egress::check_marker(&marker, current_subnet.as_deref())
        .map_err(|detail| egress_conflict(detail, marker_path))
}

fn egress_conflict(detail: String, marker_path: &std::path::Path) -> ApiError {
    ApiError::conflict(
        "egress_not_enforced",
        format!("egress is not enforced: {detail}"),
    )
    .with_details(serde_json::json!({
        "egress": EgressMode::Restrict.as_str(),
        "marker": marker_path.display().to_string(),
    }))
}

/// Create the unrestricted LAN network before an `allow_lan` instance joins it.
async fn ensure_lan_network(state: &AppState, spec: &InstanceSpec) -> Result<(), ApiError> {
    if !spec.allow_lan {
        return Ok(());
    }
    let name = state.cfg.lan_network();
    match state.driver.ensure_network(&name).await {
        Ok(true) => tracing::info!(network = %name, "created the docker network"),
        Ok(false) => {}
        Err(err) => return Err(ApiError::driver(err)),
    }
    Ok(())
}

/// Refuse a create/update that would push the fleet over a configured cap.
///
/// `replacing` is the instance being patched: its stored row is excluded from
/// the sum so an unchanged or shrinking value never trips the cap.
fn enforce_caps(
    state: &AppState,
    replacing: Option<&str>,
    spec: &InstanceSpec,
) -> Result<(), ApiError> {
    let cfg = &state.cfg;
    if cfg.max_total_cpu <= 0.0 && cfg.max_total_mem_mb == 0 {
        return Ok(());
    }

    let mut used_cpu = 0.0f64;
    let mut used_mem = 0u64;
    for existing in state.db.list().map_err(ApiError::from)? {
        if Some(existing.id.as_str()) == replacing {
            continue;
        }
        used_cpu += existing.cpu_limit;
        used_mem += u64::from(existing.mem_limit_mb);
    }

    // Float sums of decimal quotas are not exact; 1e-9 absorbs the noise.
    const EPSILON: f64 = 1e-9;
    if cfg.max_total_cpu > 0.0 && used_cpu + spec.cpu_limit > cfg.max_total_cpu + EPSILON {
        return Err(ApiError::conflict(
            "cpu_cap_exceeded",
            format!(
                "requested cpu_limit {} would bring the fleet to {} CPUs, above the configured cap of {}",
                spec.cpu_limit,
                used_cpu + spec.cpu_limit,
                cfg.max_total_cpu
            ),
        )
        .with_details(serde_json::json!({
            "used": used_cpu,
            "requested": spec.cpu_limit,
            "cap": cfg.max_total_cpu,
        })));
    }
    if cfg.max_total_mem_mb > 0 && used_mem + u64::from(spec.mem_limit_mb) > cfg.max_total_mem_mb {
        return Err(ApiError::conflict(
            "mem_cap_exceeded",
            format!(
                "requested mem_limit_mb {} would bring the fleet to {} MB, above the configured cap of {} MB",
                spec.mem_limit_mb,
                used_mem + u64::from(spec.mem_limit_mb),
                cfg.max_total_mem_mb
            ),
        )
        .with_details(serde_json::json!({
            "used": used_mem,
            "requested": spec.mem_limit_mb,
            "cap": cfg.max_total_mem_mb,
        })));
    }
    Ok(())
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

fn to_summary(spec: &InstanceSpec, health: Health) -> InstanceSummary {
    InstanceSummary {
        id: spec.id.clone(),
        name: spec.name.clone(),
        template: spec.template.clone(),
        model: spec.model.clone(),
        image: spec.image.clone(),
        channel: spec.channel.as_str().to_string(),
        status: spec.status.as_str().to_string(),
        health: health.as_str().to_string(),
        cpu_limit: spec.cpu_limit,
        allow_lan: spec.allow_lan,
        provider: spec.provider.clone(),
        shares: spec.shares.iter().map(|share| share.name.clone()).collect(),
        last_error: spec.last_error.clone(),
        created_at: spec.created_at.clone(),
    }
}

fn to_detail(
    state: &AppState,
    spec: &InstanceSpec,
    status: Status,
    health: Health,
    container: Option<ContainerInfo>,
    mut warnings: Vec<String>,
) -> InstanceDetail {
    let env = match envs::views(&state.cfg, &state.db, &spec.id) {
        Ok(env) => env,
        Err(err) => {
            warnings.push(format!("environment variables are unreadable: {err}"));
            Vec::new()
        }
    };
    if crate::secrets::has_plaintext_file(&state.cfg, &spec.id) {
        warnings.push(crate::secrets::WARNING_LEGACY.to_string());
    }
    InstanceDetail {
        id: spec.id.clone(),
        name: spec.name.clone(),
        template: spec.template.clone(),
        model: spec.model.clone(),
        image: spec.image.clone(),
        base_url: spec.base_url.clone(),
        api_key_env: spec.api_key_env.clone(),
        provider: spec.provider.clone(),
        channel: spec.channel.as_str().to_string(),
        status: status.as_str().to_string(),
        health: health.as_str().to_string(),
        shares: spec.shares.clone(),
        mem_limit_mb: spec.mem_limit_mb,
        cpu_limit: spec.cpu_limit,
        allow_lan: spec.allow_lan,
        warnings,
        notes: spec.notes.clone(),
        created_at: spec.created_at.clone(),
        updated_at: spec.updated_at.clone(),
        last_error: spec.last_error.clone(),
        data_dir: state.cfg.data_dir(&spec.id).display().to_string(),
        env,
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

    // A reusable provider supplies the default endpoint and variable name; an
    // explicit request value always wins.
    let provider = resolve_provider(state, request.provider.as_deref())?;
    let base_url = request
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| provider.as_ref().map(|entry| entry.base_url.clone()))
        .ok_or_else(|| ApiError::bad_request("invalid_base_url", "base_url is required"))?;
    validate_base_url(&base_url)
        .map_err(|message| ApiError::bad_request("invalid_base_url", message))?;

    let api_key_env = request
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| provider.as_ref().map(|entry| entry.api_key_env.clone()))
        .unwrap_or_else(|| DEFAULT_API_KEY_ENV.to_string());
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

    let cpu_limit = request.cpu_limit.unwrap_or(DEFAULT_CPU_LIMIT);
    validate_cpu_limit(cpu_limit)
        .map_err(|message| ApiError::bad_request("invalid_cpu_limit", message))?;

    let allow_lan = request.allow_lan.unwrap_or(false);

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

    let env_vars = request
        .env
        .clone()
        .map(EnvInput::into_vars)
        .unwrap_or_default();
    let env: Vec<EnvWrite> = validate_env_inputs(&env_vars, true, true)?
        .into_iter()
        .map(|var| EnvWrite {
            key: var.key,
            value: var.value.unwrap_or_default(),
            secret: var.secret.unwrap_or(false),
        })
        .collect();
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
            provider: provider.map(|entry| entry.id),
            channel,
            shares,
            mem_limit_mb,
            cpu_limit,
            image: state.cfg.image.clone(),
            previous_image: None,
            allow_lan,
            heal: HealPolicy::Off,
            unhealthy_after: DEFAULT_UNHEALTHY_AFTER,
            max_restarts: DEFAULT_MAX_RESTARTS,
            heal_backoff_s: DEFAULT_HEAL_BACKOFF_S,
            notes,
            status: Status::Created,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        },
        template,
        gate_token,
        channel_token,
        env,
    })
}

fn apply_patch(
    state: &AppState,
    spec: &mut InstanceSpec,
    request: &PatchInstanceRequest,
) -> Result<Option<String>, ApiError> {
    // Provider attachment comes first so the provider defaults are applied
    // before explicit `base_url`/`api_key_env` values of the same request.
    if let Some(requested) = &request.provider {
        match requested
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(provider_id) => {
                let provider = load_provider(state, provider_id)?;
                if request.base_url.is_none() {
                    spec.base_url = provider.base_url.clone();
                }
                if request.api_key_env.is_none() {
                    spec.api_key_env = provider.api_key_env.clone();
                }
                spec.provider = Some(provider.id);
            }
            None => spec.provider = None,
        }
    }

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
    if let Some(cpu_limit) = request.cpu_limit {
        validate_cpu_limit(cpu_limit)
            .map_err(|message| ApiError::bad_request("invalid_cpu_limit", message))?;
        spec.cpu_limit = cpu_limit;
    }
    if let Some(allow_lan) = request.allow_lan {
        spec.allow_lan = allow_lan;
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

/// Provider referenced by a request, validated against the store.
///
/// A blank or missing id means "no provider" (`Ok(None)`); an unknown id is a
/// `400 unknown_provider` so a console typo never silently falls back to a
/// per-instance token.
fn resolve_provider(
    state: &AppState,
    id: Option<&str>,
) -> Result<Option<crate::models::ProviderSpec>, ApiError> {
    match id.map(str::trim).filter(|value| !value.is_empty()) {
        Some(id) => load_provider(state, id).map(Some),
        None => Ok(None),
    }
}

fn load_provider(state: &AppState, id: &str) -> Result<crate::models::ProviderSpec, ApiError> {
    state
        .db
        .provider(id)
        .map_err(ApiError::from)?
        .map(|record| record.spec)
        .ok_or_else(|| {
            ApiError::bad_request(
                "unknown_provider",
                format!("provider \"{id}\" does not exist"),
            )
        })
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

/// One environment variable parsed from a request, before it is resolved
/// against the stored values.
struct ParsedEnv {
    key: String,
    value: Option<String>,
    secret: Option<bool>,
}

fn env_error_code(key: &str) -> &'static str {
    if env_key_is_reserved(key) {
        "reserved_env"
    } else {
        "invalid_env"
    }
}

/// Validate the `env` list of a request.
///
/// `require_value` refuses a missing value (create), `require_secret` refuses
/// a missing secret flag (create and full replace). PATCH accepts both and
/// merges instead.
fn validate_env_inputs(
    vars: &[EnvVarInput],
    require_value: bool,
    require_secret: bool,
) -> Result<Vec<ParsedEnv>, ApiError> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(vars.len());
    for var in vars {
        validate_env_key(&var.key)
            .map_err(|message| ApiError::bad_request(env_error_code(&var.key), message))?;
        if !seen.insert(var.key.as_str()) {
            return Err(ApiError::bad_request(
                "duplicate_env",
                format!("environment variable \"{}\" is listed twice", var.key),
            ));
        }
        match &var.value {
            Some(value) => validate_env_value(value)
                .map_err(|message| ApiError::bad_request("invalid_env", message))?,
            None if require_value => {
                return Err(ApiError::bad_request(
                    "invalid_env",
                    format!("environment variable \"{}\" requires a value", var.key),
                ))
            }
            None => {}
        }
        if require_secret && var.secret.is_none() {
            return Err(ApiError::bad_request(
                "invalid_env",
                format!(
                    "environment variable \"{}\" must say whether it is a secret",
                    var.key
                ),
            ));
        }
        out.push(ParsedEnv {
            key: var.key.clone(),
            value: var.value.clone(),
            secret: var.secret,
        });
    }
    Ok(out)
}

/// Resolve a full replacement against the stored environment.
///
/// A secret entry without a value keeps the stored value (the API never
/// returned it, so a console cannot send it back); any other entry without a
/// value is a request error.
fn resolve_full_env(
    existing: &[envs::EnvValue],
    vars: &[EnvVarInput],
) -> Result<Vec<EnvWrite>, ApiError> {
    let parsed = validate_env_inputs(vars, false, true)?;
    let mut entries = Vec::with_capacity(parsed.len());
    for var in parsed {
        let secret = var.secret.unwrap_or(false);
        match var.value {
            Some(value) => entries.push(EnvWrite {
                key: var.key,
                value,
                secret,
            }),
            None => {
                let stored = existing.iter().find(|entry| entry.key == var.key);
                match stored {
                    Some(entry) if entry.secret && secret => entries.push(EnvWrite {
                        key: var.key,
                        value: entry.value.clone(),
                        secret: true,
                    }),
                    Some(_) => {
                        return Err(ApiError::bad_request(
                            "invalid_env",
                            format!(
                                "environment variable \"{}\" is not a secret: a value is required",
                                var.key
                            ),
                        ))
                    }
                    None => {
                        return Err(ApiError::bad_request(
                            "invalid_env",
                            format!(
                                "environment variable \"{}\" is new: a value is required",
                                var.key
                            ),
                        ))
                    }
                }
            }
        }
    }
    Ok(entries)
}

/// Merge a `PATCH` env list into the stored values: `null` deletes, `""` keeps
/// an empty value, a missing secret flag keeps the flag of an existing key.
fn apply_env_merge(
    values: &mut BTreeMap<String, String>,
    flags: &mut BTreeMap<String, bool>,
    vars: Option<&[EnvVarInput]>,
) -> Result<(), ApiError> {
    let Some(vars) = vars else {
        return Ok(());
    };
    let parsed = validate_env_inputs(vars, false, false)?;
    for var in parsed {
        match var.value {
            None => {
                values.remove(&var.key);
                flags.remove(&var.key);
            }
            Some(value) => {
                let secret = var
                    .secret
                    .unwrap_or_else(|| flags.get(&var.key).copied().unwrap_or(false));
                values.insert(var.key.clone(), value);
                flags.insert(var.key, secret);
            }
        }
    }
    Ok(())
}

fn env_values(entries: &[EnvWrite]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect()
}

fn env_flags(entries: &[EnvWrite]) -> BTreeMap<String, bool> {
    entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.secret))
        .collect()
}

fn env_values_of(entries: &[envs::EnvValue]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect()
}

fn env_flags_of(entries: &[envs::EnvValue]) -> BTreeMap<String, bool> {
    entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.secret))
        .collect()
}

fn env_entries(
    values: BTreeMap<String, String>,
    flags: &BTreeMap<String, bool>,
    forced_secret: &[&str],
) -> Vec<EnvWrite> {
    values
        .into_iter()
        .map(|(key, value)| EnvWrite {
            secret: forced_secret.contains(&key.as_str())
                || flags.get(&key).copied().unwrap_or(false),
            key,
            value,
        })
        .collect()
}

/// Keys supplied by the gate/channel tokens of a request: always secret, even
/// when the request marks them as plain.
fn token_keys(spec: &InstanceSpec, gate: bool, channel: bool) -> Vec<&str> {
    let mut keys = Vec::new();
    if gate {
        keys.push(GATE_TOKEN_ENV);
        if spec.api_key_env != GATE_TOKEN_ENV && spec.api_key_env != DEFAULT_API_KEY_ENV {
            keys.push(spec.api_key_env.as_str());
        }
    }
    if channel {
        if let Some(env) = spec.channel.token_env() {
            keys.push(env);
        }
    }
    keys
}

fn invalid_tail() -> ApiError {
    ApiError::bad_request(
        "invalid_tail",
        format!("tail must be between 1 and {MAX_LOGS_TAIL}"),
    )
}

fn filter_error(err: logs::FilterError) -> ApiError {
    let message = err.to_string();
    match err {
        logs::FilterError::InvalidLevel => ApiError::bad_request("invalid_level", message),
        logs::FilterError::GrepTooLong | logs::FilterError::InvalidGrep(_) => {
            ApiError::bad_request("invalid_grep", message)
        }
    }
}

/// Trim and validate a replacement token (empty means "not supplied").
fn normalize_token(value: Option<&str>, code: &'static str) -> Result<Option<String>, ApiError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    validate_secret_value(value).map_err(|message| ApiError::bad_request(code, message))?;
    Ok(Some(value.to_string()))
}

/// Secret keys replaced by `gate_token`.
fn gate_env_keys(spec: &InstanceSpec) -> Vec<&str> {
    let mut keys = vec![GATE_TOKEN_ENV];
    if spec.api_key_env != GATE_TOKEN_ENV {
        keys.push(spec.api_key_env.as_str());
    }
    keys
}

/// Display name of a clone when the request does not name it.
fn clone_default_name(source: &str) -> String {
    let suffix = " copy";
    let budget = 128usize.saturating_sub(suffix.chars().count());
    let base: String = source.trim().chars().take(budget).collect();
    if base.is_empty() {
        return "copy".to_string();
    }
    format!("{base}{suffix}")
}

/// Template of a clone: the source template, or its on-disk config when the
/// source was adopted from disk (`custom`).
fn clone_template(state: &AppState, source: &InstanceSpec) -> Result<TemplateSource, ApiError> {
    match render::load_template(&state.cfg, &source.template) {
        Ok(template) => Ok(template),
        Err(RenderError::UnknownTemplate(_)) if source.template == "custom" => {
            let config_path = state.cfg.data_dir(&source.id).join("config.yaml");
            let config_yaml = std::fs::read_to_string(&config_path).map_err(|err| {
                ApiError::internal(format!(
                    "failed to read the source config at {}: {err}",
                    config_path.display()
                ))
            })?;
            let soul_md =
                std::fs::read_to_string(state.cfg.data_dir(&source.id).join("SOUL.md")).ok();
            Ok(TemplateSource {
                id: source.template.clone(),
                config_yaml,
                soul_md,
                meta: TemplateMeta::default(),
            })
        }
        Err(err) => Err(ApiError::from(err)),
    }
}

/// Persist the clone: secrets, optional data copy, then the generated files.
///
/// The generated files are written last so the clone's own `config.yaml`,
/// `SOUL.md` and health probe always win over the copied ones.
async fn write_clone(
    state: &AppState,
    source_id: &str,
    spec: &InstanceSpec,
    entries: &[EnvWrite],
    rendered: &render::RenderedInstance,
    copy_data: bool,
) -> Result<(), ApiError> {
    envs::replace(&state.cfg, &state.db, &spec.id, entries).map_err(ApiError::from)?;

    if copy_data {
        let cfg = state.cfg.clone();
        let from = source_id.to_string();
        let to = spec.id.clone();
        match tokio::task::spawn_blocking(move || store::copy_data(&cfg, &from, &to)).await {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                return Err(ApiError::internal(format!(
                    "failed to copy the instance data: {err}"
                )))
            }
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "failed to copy the instance data: {err}"
                )))
            }
        }
    }

    store::write_instance(&state.cfg, spec, rendered, true)
        .map_err(|err| ApiError::internal(format!("failed to write instance files: {err}")))
}

/// Remove every trace of a clone that failed halfway through.
fn rollback_clone(state: &AppState, id: &str) {
    let _ = state.db.delete_instance(id);
    let _ = std::fs::remove_dir_all(state.cfg.instance_dir(id));
}

/// Record the `docker inspect` failure of a non-healthy container.
///
/// Docker knows *why* a container died (`State.Error`, `OOMKilled`, exit
/// code); the health probe only knows that it did. When the probe is unhappy,
/// the inspect answer becomes `last_error` so the UI can show the cause
/// without the full log.
async fn record_inspect_error(state: &AppState, spec: &mut InstanceSpec, health: Health) {
    if health == Health::Healthy {
        return;
    }
    let Ok(info) = state.driver.inspect(spec).await else {
        return;
    };
    let Some(message) = info.error_message() else {
        return;
    };
    if spec.last_error.as_deref() == Some(message.as_str()) {
        return;
    }
    let _ = state.db.set_last_error(&spec.id, Some(&message));
    spec.last_error = Some(message);
}
