//! Instance file browser endpoints.
//!
//! Admin (or, for reads, viewer) access to the files inside
//! `FLEETT_ROOT/instances/<id>/data`. The filesystem policy itself — path
//! confinement, size cap, extension allowlist — lives in [`crate::files`];
//! this module owns authentication, the instance lock, the audit trail and the
//! optional restart.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::instances;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::files::{self, Content, Entry, FileError, WriteResult};
use crate::models::Status;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Query of the list and content endpoints.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PathQuery {
    /// Virtual path, `/` when omitted (the data directory itself).
    #[serde(default)]
    pub path: Option<String>,
}

impl PathQuery {
    fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("/")
    }
}

/// Query of `PUT .../files/content`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WriteQuery {
    /// Restart the instance once the write succeeded (default `false`).
    #[serde(default)]
    pub restart: Option<bool>,
}

/// Body of `PUT .../files/content`.
#[derive(Debug, Clone, Deserialize)]
pub struct WriteRequest {
    pub path: String,
    pub content: String,
    /// Etag of the version the client edited. Required when the file exists.
    #[serde(default)]
    pub etag: Option<String>,
}

/// Body of `POST .../files/mkdir`.
#[derive(Debug, Clone, Deserialize)]
pub struct MkdirRequest {
    pub path: String,
}

/// Response of a successful write.
#[derive(Debug, Clone, Serialize)]
pub struct WriteResponse {
    pub path: String,
    pub etag: String,
    pub size: u64,
    pub mtime: String,
    pub editable: bool,
    pub rendered: bool,
    /// Virtual path of the `.bak-YYYYMMDD-HHMMSS` copy, when one was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    /// `true` when `?restart=true` triggered a restart after the write.
    pub restarted: bool,
    /// Status of the instance after the restart (`null` when not restarted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Response of `POST .../files/mkdir`.
#[derive(Debug, Clone, Serialize)]
pub struct MkdirResponse {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
}

/// `GET /api/instances/{id}/files?path=/` — one directory level.
pub async fn list(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<PathQuery>,
) -> Result<Json<Vec<Entry>>, ApiError> {
    instances::load(&state, &id)?;
    let entries = files::list(&state.cfg, &id, query.path()).map_err(map_error)?;
    Ok(Json(entries))
}

/// `GET /api/instances/{id}/files/content?path=/SOUL.md` — text + etag.
pub async fn read(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<PathQuery>,
) -> Result<Json<Content>, ApiError> {
    instances::load(&state, &id)?;
    let content = files::read(&state.cfg, &id, query.path()).map_err(map_error)?;
    Ok(Json(content))
}

/// `PUT /api/instances/{id}/files/content` — compare-and-swap write.
///
/// The expected etag comes from the `If-Match` header (preferred, standard
/// HTTP) or from the body. `If-Match: *` means "the file must exist"; any value
/// must name the bytes currently on disk. A missing etag never overwrites.
pub async fn write(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<WriteQuery>,
    headers: HeaderMap,
    Json(request): Json<WriteRequest>,
) -> Result<Json<WriteResponse>, ApiError> {
    let restart = query.restart.unwrap_or(false);

    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let mut spec = instances::load(&state, &id)?;
    let expected = expected_etag(&headers, request.etag.as_deref());
    let outcome = files::write(
        &state.cfg,
        &id,
        &request.path,
        expected.as_deref(),
        &request.content,
    )
    .map_err(map_error)?;

    // The write already happened: audit it before a restart can fail, so the
    // trail never loses a mutation.
    crate::history::record_ui_write(&state.cfg, &id, &context, &outcome.path);
    audit::record(
        &state,
        &context,
        "instance.file.write",
        &spec.id,
        Some(json!({
            "path": outcome.path,
            "size": outcome.size,
            "backup": outcome.backup,
            "restart": restart,
        })),
    );

    let mut restarted = false;
    let mut status = None;
    if restart {
        if let Err(err) = state.driver.restart(&spec).await {
            let detail = err.detail.clone();
            let _ = state.db.set_status(&id, Status::Error, Some(&detail));
            state
                .metrics
                .record_compose_action("restart", crate::metrics::RESULT_ERROR);
            return Err(ApiError::driver(err));
        }
        state
            .metrics
            .record_compose_action("restart", crate::metrics::RESULT_OK);
        let observed = state
            .driver
            .ps(&spec)
            .await
            .map(|result| result.status)
            .unwrap_or(Status::Running);
        state
            .db
            .set_status(&id, observed, None)
            .map_err(ApiError::from)?;
        spec.status = observed;
        restarted = true;
        status = Some(observed.as_str().to_string());
    }

    Ok(Json(to_response(outcome, restarted, status)))
}

/// `DELETE /api/instances/{id}/files/content?path=/notes/old.md`.
///
/// Files and empty directories only; a rendered file and a non-empty directory
/// are refused (`409`).
pub async fn delete_file(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<PathQuery>,
) -> Result<StatusCode, ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let spec = instances::load(&state, &id)?;
    let path = files::delete(&state.cfg, &id, query.path()).map_err(map_error)?;
    crate::history::record_ui_write(&state.cfg, &id, &context, &path);
    audit::record(
        &state,
        &context,
        "instance.file.delete",
        &spec.id,
        Some(json!({ "path": path })),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/instances/{id}/files/mkdir` — create one directory.
pub async fn mkdir(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<MkdirRequest>,
) -> Result<(StatusCode, Json<MkdirResponse>), ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let spec = instances::load(&state, &id)?;
    let path = files::mkdir(&state.cfg, &id, &request.path).map_err(map_error)?;
    audit::record(
        &state,
        &context,
        "instance.file.mkdir",
        &spec.id,
        Some(json!({ "path": path })),
    );
    Ok((
        StatusCode::CREATED,
        Json(MkdirResponse { path, kind: "dir" }),
    ))
}

/// Normalize `If-Match` (`W/"abc"`, `"abc"`) and the body etag into one value.
///
/// The header wins: it is the standard way to express a conditional write, and
/// a reverse proxy may have stripped the body field.
fn expected_etag(headers: &HeaderMap, body: Option<&str>) -> Option<String> {
    let header = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let value = value.strip_prefix("W/").unwrap_or(value).trim();
            let value = value.trim_matches('"');
            value.to_string()
        });
    header.or_else(|| body.map(str::trim).map(str::to_string))
}

fn to_response(outcome: WriteResult, restarted: bool, status: Option<String>) -> WriteResponse {
    WriteResponse {
        path: outcome.path,
        etag: outcome.etag,
        size: outcome.size,
        mtime: outcome.mtime,
        editable: outcome.editable,
        rendered: outcome.rendered,
        backup: outcome.backup,
        restarted,
        status,
    }
}

/// Map the filesystem policy errors onto the shared API envelope.
fn map_error(err: FileError) -> ApiError {
    match err {
        FileError::InvalidPath(message) => ApiError::bad_request("invalid_path", message),
        FileError::Escape(message) => ApiError::bad_request("path_escape", message),
        FileError::NotFound(message) => ApiError::not_found(message),
        FileError::NotAFile(message) => ApiError::bad_request("not_a_file", message),
        FileError::NotADirectory(message) => ApiError::bad_request("not_a_directory", message),
        FileError::Unsupported(message) => ApiError::bad_request("unsupported_file_type", message),
        FileError::TooLarge { size, limit } => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "file_too_large",
            format!("the file is {size} bytes, above the {limit}-byte limit of the editor"),
        )
        .with_details(json!({ "size": size, "limit": limit })),
        FileError::Binary(message) => {
            ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "binary_file", message)
        }
        FileError::Rendered(path) => ApiError::conflict(
            "file_regenerated",
            format!(
                "\"{path}\" is rendered by fleett and cannot be edited here: {}",
                files::RENDERED_NOTE
            ),
        )
        .with_details(json!({
            "file": path,
            "rendered": true,
            "hint": files::RENDERED_NOTE,
        })),
        FileError::NotEditable(path) => ApiError::conflict(
            "file_not_editable",
            format!(
                "\"{path}\" is read-only: only {} files are editable",
                files::EDITABLE_EXTENSIONS
                    .iter()
                    .map(|ext| format!(".{ext}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .with_details(json!({
            "file": path,
            "extensions": files::EDITABLE_EXTENSIONS,
        })),
        FileError::AlreadyExists(message) => ApiError::conflict("already_exists", message),
        FileError::DirectoryNotEmpty(message) => ApiError::conflict("directory_not_empty", message),
        FileError::EtagMismatch(current) => ApiError::conflict(
            "etag_mismatch",
            "the file changed since it was loaded: reload it and retry",
        )
        .with_details(json!({ "current": current })),
        FileError::Io(err) => ApiError::internal(format!("file operation failed: {err}")),
    }
}
