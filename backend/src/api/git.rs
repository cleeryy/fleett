//! Per-instance git history endpoints.
//!
//! The repository itself — whitelist, authors, commit messages — lives in
//! [`crate::history`]; this module owns authentication, the instance lock, the
//! audit trail and the optional restart. Reads need any role, the revert needs
//! `admin`. None of these routes talks to Docker: the history is read from disk
//! and works on a stopped instance.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::instances;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::history::{self, CommitInfo, HistoryError};
use crate::models::Status;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Commits returned when `?limit=` is omitted.
pub const DEFAULT_LOG_LIMIT: usize = 50;
/// Upper bound of `?limit=`.
pub const MAX_LOG_LIMIT: usize = 500;

/// Query of `GET .../git/log`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LogQuery {
    /// Repository path (`data/SOUL.md`) or virtual path (`/SOUL.md`).
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Query of `GET .../git/diff`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DiffQuery {
    /// Start of the range (defaults to the first parent of `to`).
    #[serde(default)]
    pub from: Option<String>,
    /// End of the range (defaults to `HEAD`).
    #[serde(default)]
    pub to: Option<String>,
}

/// Response of `GET .../git/log`.
#[derive(Debug, Clone, Serialize)]
pub struct LogResponse {
    pub commits: Vec<CommitInfo>,
}

/// Body of `POST .../git/revert`.
#[derive(Debug, Clone, Deserialize)]
pub struct RevertRequest {
    /// Full or short commit id (or any revision `HEAD`, `HEAD~2`).
    pub commit: String,
    /// Files to restore (repository or virtual paths). Omitted: every path the
    /// commit changed.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Restart the instance once the files are back (default `false`).
    #[serde(default)]
    pub restart: Option<bool>,
}

/// Response of `POST .../git/revert`.
#[derive(Debug, Clone, Serialize)]
pub struct RevertResponse {
    /// Commit the files were restored from.
    pub source: String,
    /// Repository-relative paths actually restored or removed.
    pub paths: Vec<String>,
    /// Commit recording the restore (`null` when nothing changed).
    pub commit: Option<CommitInfo>,
    pub restarted: bool,
    /// Instance status after the optional restart.
    pub status: String,
}

/// `GET /api/instances/{id}/git/log?path=&limit=50` — newest first.
pub async fn log(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<LogQuery>,
) -> Result<Json<LogResponse>, ApiError> {
    instances::load(&state, &id)?;
    let limit = query.limit.unwrap_or(DEFAULT_LOG_LIMIT);
    if limit == 0 || limit > MAX_LOG_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_LOG_LIMIT}"),
        ));
    }
    let commits = history::log(&state.cfg, &id, query.path.as_deref(), limit).map_err(map_error)?;
    Ok(Json(LogResponse { commits }))
}

/// `GET /api/instances/{id}/git/diff?from=&to=` — unified patch text.
pub async fn diff(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<history::DiffResponse>, ApiError> {
    instances::load(&state, &id)?;
    let response = history::diff(&state.cfg, &id, query.from.as_deref(), query.to.as_deref())
        .map_err(map_error)?;
    Ok(Json(response))
}

/// `GET /api/instances/{id}/git/status` — work-tree drift, whitelist only.
pub async fn status(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<history::RepoStatus>, ApiError> {
    instances::load(&state, &id)?;
    let status = history::status(&state.cfg, &id).map_err(map_error)?;
    Ok(Json(status))
}

/// `POST /api/instances/{id}/git/revert` — restore files from one commit.
///
/// Every restored file is backed up (`.bak-YYYYMMDD-HHMMSS`) before it is
/// overwritten, the restore is recorded as a new `ui:` commit attributed to the
/// caller, and the audit trail gets an `instance.git.revert` row.
pub async fn revert(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<RevertRequest>,
) -> Result<Json<RevertResponse>, ApiError> {
    audit::require_admin(&context)?;
    let commit = request.commit.trim().to_string();
    if commit.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_commit",
            "commit is required: send {\"commit\": \"<id>\"}",
        ));
    }
    let restart = request.restart.unwrap_or(false);

    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let mut spec = instances::load(&state, &id)?;
    let author = history::author_for(&context);
    let outcome = history::revert(&state.cfg, &id, &commit, request.paths.as_deref(), &author)
        .map_err(map_error)?;

    // The files are back on disk: audit before a restart can fail, exactly like
    // a file write, so the trail never loses the mutation.
    audit::record(
        &state,
        &context,
        "instance.git.revert",
        &spec.id,
        Some(json!({
            "source": outcome.source,
            "paths": outcome.paths,
            "commit": outcome.commit.as_ref().map(|commit| commit.id.clone()),
            "restart": restart,
        })),
    );

    let mut restarted = false;
    let mut status = spec.status;
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
        status = observed;
    }

    Ok(Json(RevertResponse {
        source: outcome.source,
        paths: outcome.paths,
        commit: outcome.commit,
        restarted,
        status: status.as_str().to_string(),
    }))
}

/// Map the history errors onto the shared API envelope.
fn map_error(err: HistoryError) -> ApiError {
    match err {
        HistoryError::UnknownCommit(revision) => ApiError::not_found(format!(
            "commit \"{revision}\" was not found in the instance history"
        )),
        HistoryError::UntrackedPath(path) => ApiError::bad_request(
            "untracked_path",
            format!("\"{path}\" is outside the versioned whitelist"),
        ),
        HistoryError::Escape(message) => ApiError::bad_request("path_escape", message),
        HistoryError::NotAFile(path) => {
            ApiError::conflict("not_a_file", format!("\"{path}\" is not a regular file"))
        }
        HistoryError::NoHistory => ApiError::new(
            StatusCode::NOT_FOUND,
            "no_history",
            "this instance has no history yet: edit a file first",
        ),
        HistoryError::Io(err) => ApiError::internal(format!("history operation failed: {err}")),
        HistoryError::Git(err) => ApiError::internal(format!("git operation failed: {err}")),
    }
}
