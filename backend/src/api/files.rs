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
use crate::files::{self, Content, Entry, FileError, SearchResult, WriteResult};
use crate::models::Status;
use axum::body::Body;
use axum::extract::{Extension, Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Largest multipart body accepted by `POST .../files/upload`: the 32 MiB
/// payload cap plus the multipart framing of a few dozen parts.
pub const MAX_UPLOAD_BODY: usize = files::MAX_UPLOAD_TOTAL_BYTES as usize + 1024 * 1024;

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

/// Query of `POST .../files/upload`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UploadQuery {
    /// Replace an existing file (default `false`): the old bytes are backed up
    /// as `.bak-YYYYMMDD-HHMMSS` first.
    #[serde(default)]
    pub overwrite: Option<bool>,
}

/// Query of `GET .../files/search`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchQuery {
    /// Needle, or regular expression when `regex=true`.
    #[serde(default)]
    pub q: Option<String>,
    /// Virtual directory to walk, `/` when omitted.
    #[serde(default)]
    pub path: Option<String>,
    /// Treat `q` as a regular expression (default `false`, literal search).
    #[serde(default)]
    pub regex: Option<bool>,
    /// Maximum number of hits (default and cap: 200).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Match case (default `false`, case-insensitive).
    #[serde(default)]
    pub case_sensitive: Option<bool>,
}

/// Response of `POST .../files/upload`.
#[derive(Debug, Clone, Serialize)]
pub struct UploadResponse {
    pub directory: String,
    pub files: Vec<files::UploadedFile>,
    /// Sum of the bytes written.
    pub total: u64,
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
    // trail never loses a mutation. A proposal applied from the assistant
    // panel is tagged `via: "assistant"` instead of getting another action.
    crate::history::record_ui_write(&state.cfg, &id, &context, &outcome.path);
    audit::record(
        &state,
        &context,
        "instance.file.write",
        &spec.id,
        audit::with_assistant_tag(
            Some(json!({
                "path": outcome.path,
                "size": outcome.size,
                "backup": outcome.backup,
                "restart": restart,
            })),
            &headers,
        ),
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
    headers: HeaderMap,
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
        audit::with_assistant_tag(Some(json!({ "path": path })), &headers),
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

/// `POST /api/instances/{id}/files/upload` — multipart upload.
///
/// Fields: `path` (the target directory, virtual and `/`-rooted) and one or
/// more `file` parts. The whole request is checked before the first write:
/// per-file 10 MiB, 32 MiB total, no executable extension or magic bytes, and
/// an existing target needs `?overwrite=true` (which keeps a `.bak` copy).
pub async fn upload(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ApiError> {
    instances::load(&state, &id)?;
    let overwrite = query.overwrite.unwrap_or(false);

    let mut directory: Option<String> = None;
    let mut uploads: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total = 0usize;
    loop {
        let field = multipart.next_field().await.map_err(multipart_error)?;
        let Some(field) = field else {
            break;
        };
        let name = field.name().unwrap_or_default().to_string();
        if name == "path" {
            directory = Some(
                field
                    .text()
                    .await
                    .map_err(multipart_error)?
                    .trim()
                    .to_string(),
            );
            continue;
        }
        let Some(filename) = field.file_name().map(str::to_string) else {
            continue;
        };
        let bytes = field.bytes().await.map_err(multipart_error)?;
        let size = bytes.len();
        if size as u64 > files::MAX_UPLOAD_FILE_BYTES {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "file_too_large",
                format!(
                    "each uploaded file must be at most {} bytes",
                    files::MAX_UPLOAD_FILE_BYTES
                ),
            )
            .with_details(json!({ "size": size, "limit": files::MAX_UPLOAD_FILE_BYTES })));
        }
        total += size;
        if total as u64 > files::MAX_UPLOAD_TOTAL_BYTES {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "file_too_large",
                format!(
                    "one upload carries at most {} bytes",
                    files::MAX_UPLOAD_TOTAL_BYTES
                ),
            )
            .with_details(json!({ "size": total, "limit": files::MAX_UPLOAD_TOTAL_BYTES })));
        }
        uploads.push((filename, bytes.to_vec()));
    }

    let directory = directory.ok_or_else(|| {
        ApiError::bad_request(
            "invalid_upload",
            "the request must carry a \"path\" field naming the target directory (e.g. \"/notes\")",
        )
    })?;

    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;
    let spec = instances::load(&state, &id)?;
    let outcome =
        files::upload(&state.cfg, &id, &directory, &uploads, overwrite).map_err(map_error)?;

    // The first tracked path commits the whole batch (the diff already holds
    // every file); the other calls are no-ops for the git tree. Then one audit
    // row for the request.
    for file in &outcome.files {
        crate::history::record_ui_write(&state.cfg, &id, &context, &file.path);
    }
    audit::record(
        &state,
        &context,
        "instance.file.upload",
        &spec.id,
        Some(json!({
            "directory": outcome.directory,
            "overwrite": overwrite,
            "total": outcome.total,
            "files": outcome
                .files
                .iter()
                .map(|file| json!({
                    "path": file.path,
                    "size": file.size,
                    "backup": file.backup,
                }))
                .collect::<Vec<_>>(),
        })),
    );

    Ok((
        StatusCode::CREATED,
        Json(UploadResponse {
            directory: outcome.directory,
            files: outcome.files,
            total: outcome.total,
        }),
    ))
}

/// `GET /api/instances/{id}/files/download?path=/notes/capture.png` — raw
/// bytes, binary allowed, 50 MiB cap.
pub async fn download(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<PathQuery>,
) -> Result<Response, ApiError> {
    instances::load(&state, &id)?;
    let cfg = state.cfg.clone();
    let raw = query.path().to_string();
    let file = tokio::task::spawn_blocking(move || files::download(&cfg, &id, &raw))
        .await
        .map_err(|err| ApiError::internal(format!("the download task failed: {err}")))?
        .map_err(map_error)?;

    let filename = header_filename(&file.name, "download");
    let mut response = Response::new(Body::from(file.bytes));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(file.content_type),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .expect("the sanitized filename is a valid header value"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// `GET /api/instances/{id}/files/archive?path=/` — zip of one directory.
pub async fn archive(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<PathQuery>,
) -> Result<Response, ApiError> {
    instances::load(&state, &id)?;
    let cfg = state.cfg.clone();
    let raw = query.path().to_string();
    let stored_id = id.clone();
    let outcome = tokio::task::spawn_blocking(move || files::archive(&cfg, &stored_id, &raw))
        .await
        .map_err(|err| ApiError::internal(format!("the archive task failed: {err}")))?
        .map_err(map_error)?;

    let suffix = outcome.name.as_deref().unwrap_or("data");
    let filename = header_filename(&format!("{id}-{suffix}.zip"), "instance-data.zip");
    let mut response = Response::new(Body::from(outcome.bytes));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .expect("the sanitized filename is a valid header value"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// `GET /api/instances/{id}/files/search?q=…&path=/&regex=false&limit=200` —
/// full-text search over one directory of the tree (viewer).
pub async fn search(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResult>, ApiError> {
    instances::load(&state, &id)?;
    let needle = query
        .q
        .as_deref()
        .map(str::to_string)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_query",
                "the q parameter is required (a literal string, or a regular expression with regex=true)",
            )
        })?;
    let limit = match query.limit {
        Some(0) => {
            return Err(ApiError::bad_request(
                "invalid_limit",
                "limit must be between 1 and 200",
            ))
        }
        Some(limit) => limit.min(files::MAX_SEARCH_RESULTS),
        None => files::MAX_SEARCH_RESULTS,
    };
    let directory = query.path.clone().unwrap_or_else(|| "/".to_string());
    let regex = query.regex.unwrap_or(false);
    let case_sensitive = query.case_sensitive.unwrap_or(false);

    let cfg = state.cfg.clone();
    let result = tokio::task::spawn_blocking(move || {
        files::search(&cfg, &id, &directory, &needle, regex, case_sensitive, limit)
    })
    .await
    .map_err(|err| ApiError::internal(format!("the search task failed: {err}")))?
    .map_err(map_error)?;
    Ok(Json(result))
}

/// `Content-Disposition` filename: printable ASCII only, quotes and control
/// characters replaced, never empty.
fn header_filename(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | ' ') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

/// Turn a multipart read error into the shared envelope; a body above the
/// route limit is a `413`, not an opaque axum rejection.
fn multipart_error(err: axum::extract::multipart::MultipartError) -> ApiError {
    if err.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "file_too_large",
            format!("the upload is larger than the {MAX_UPLOAD_BODY}-byte multipart limit"),
        )
        .with_details(json!({ "limit": files::MAX_UPLOAD_TOTAL_BYTES }));
    }
    ApiError::bad_request(
        "invalid_upload",
        format!("cannot read the multipart body: {err}"),
    )
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
            format!("the file is {size} bytes, above the {limit}-byte limit"),
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
        FileError::Exists(path) => ApiError::conflict(
            "file_exists",
            format!("\"{path}\" already exists: send ?overwrite=true to replace it"),
        )
        .with_details(json!({ "file": path, "overwrite": false })),
        FileError::Executable(message) => ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "executable_file",
            message,
        )
        .with_details(json!({ "extensions": files::BLOCKED_EXTENSIONS })),
        FileError::InvalidUpload(message) => ApiError::bad_request("invalid_upload", message),
        FileError::InvalidQuery(message) => ApiError::bad_request("invalid_query", message),
        FileError::InvalidRegex(message) => ApiError::bad_request("invalid_regex", message),
        FileError::DirectoryNotEmpty(message) => ApiError::conflict("directory_not_empty", message),
        FileError::EtagMismatch(current) => ApiError::conflict(
            "etag_mismatch",
            "the file changed since it was loaded: reload it and retry",
        )
        .with_details(json!({ "current": current })),
        FileError::Io(err) => ApiError::internal(format!("file operation failed: {err}")),
    }
}
