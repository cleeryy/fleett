//! Fleet knowledge base endpoints (issue #29).

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::memory::{self, MemoryEntry, MemoryError, MemoryVersion, DEFAULT_LIMIT, MAX_LIMIT};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
pub struct WriteMemoryRequest {
    pub namespace: String,
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub ttl_s: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMemoryRequest {
    pub value: String,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub ttl_s: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub include_expired: Option<bool>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub namespace: Option<String>,
    /// Comma-separated tags, all required.
    #[serde(default)]
    pub tags: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct RollbackRequest {
    pub version: u32,
}

#[derive(Debug, Deserialize)]
pub struct PromoteRequest {
    #[serde(default = "default_global")]
    pub namespace: String,
}

fn default_global() -> String {
    "global".to_string()
}

#[derive(Debug, serde::Serialize)]
pub struct WriteResponse {
    pub entry: MemoryEntry,
    pub created: bool,
}

impl From<MemoryError> for ApiError {
    fn from(err: MemoryError) -> Self {
        match err {
            MemoryError::InvalidNamespace(message) => ApiError::bad_request(
                "invalid_namespace",
                format!(
                    "namespace must be global, shared, instance:<id> or project:<name> ({message})"
                ),
            ),
            MemoryError::InvalidKey(message) => {
                ApiError::bad_request("invalid_key", format!("invalid key: {message}"))
            }
            MemoryError::ValueTooLarge => ApiError::bad_request(
                "value_too_large",
                format!("value exceeds {} bytes", memory::VALUE_MAX_BYTES),
            ),
            MemoryError::InvalidTag(tag) => {
                ApiError::bad_request("invalid_tag", format!("invalid tag: {tag}"))
            }
            MemoryError::InvalidTtl => ApiError::bad_request(
                "invalid_ttl",
                format!(
                    "ttl_s must be between {} and {} seconds",
                    memory::TTL_MIN_S,
                    memory::TTL_MAX_S
                ),
            ),
            MemoryError::EmptyQuery => {
                ApiError::bad_request("empty_query", "q must contain at least one word")
            }
            MemoryError::NotFound(id) => ApiError::not_found(format!("unknown memory entry {id}")),
            MemoryError::UnknownVersion(version) => {
                ApiError::not_found(format!("version {version} does not exist for this entry"))
            }
            MemoryError::Db(err) => ApiError::from(err),
        }
    }
}

/// `POST /api/memory` — create or update `(namespace, key)`.
pub async fn write(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<WriteMemoryRequest>,
) -> Result<(StatusCode, Json<WriteResponse>), ApiError> {
    let (entry, created) = memory::upsert(
        &state.db,
        &request.namespace,
        &request.key,
        &request.value,
        &request.tags,
        &context.actor,
        request.ttl_s,
    )?;
    audit::record(
        &state,
        &context,
        if created {
            "memory.create"
        } else {
            "memory.update"
        },
        &format!("{}/{}", entry.namespace, entry.key),
        Some(json!({ "id": entry.id, "version": entry.version })),
    );
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(WriteResponse { entry, created }),
    ))
}

/// `PUT /api/memory/{id}` — update by id.
pub async fn update(
    Path(id): Path<i64>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<UpdateMemoryRequest>,
) -> Result<Json<MemoryEntry>, ApiError> {
    let existing = memory::get(&state.db, id)?
        .ok_or_else(|| ApiError::not_found(format!("unknown memory entry {id}")))?;
    let tags = request.tags.unwrap_or_else(|| existing.tags.clone());
    let (entry, _) = memory::upsert(
        &state.db,
        &existing.namespace,
        &existing.key,
        &request.value,
        &tags,
        &context.actor,
        request.ttl_s,
    )?;
    audit::record(
        &state,
        &context,
        "memory.update",
        &format!("{}/{}", entry.namespace, entry.key),
        Some(json!({ "id": entry.id, "version": entry.version })),
    );
    Ok(Json(entry))
}

/// `GET /api/memory` — browse one namespace.
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<MemoryEntry>>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let entries = memory::list(
        &state.db,
        query.namespace.as_deref(),
        query.include_expired.unwrap_or(false),
        limit,
    )?;
    Ok(Json(entries))
}

/// `GET /api/memory/search?q=…` — full text search.
pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<MemoryEntry>>, ApiError> {
    let tags: Vec<String> = query
        .tags
        .as_deref()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let entries = memory::search(
        &state.db,
        &query.q,
        query.namespace.as_deref(),
        &tags,
        limit,
    )?;
    Ok(Json(entries))
}

/// `GET /api/memory/{id}`.
pub async fn get(
    Path(id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Json<MemoryEntry>, ApiError> {
    memory::get(&state.db, id)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("unknown memory entry {id}")))
}

#[derive(Debug, Deserialize)]
pub struct KeyQuery {
    pub namespace: String,
    pub key: String,
}

/// `GET /api/memory/by-key?namespace=&key=`.
pub async fn get_by_key(
    State(state): State<AppState>,
    Query(query): Query<KeyQuery>,
) -> Result<Json<MemoryEntry>, ApiError> {
    memory::get_by_key(&state.db, &query.namespace, &query.key)?
        .map(Json)
        .ok_or_else(|| {
            ApiError::not_found(format!("no memory entry {}/{}", query.namespace, query.key))
        })
}

/// `GET /api/memory/{id}/history`.
pub async fn history(
    Path(id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Json<Vec<MemoryVersion>>, ApiError> {
    Ok(Json(memory::history(&state.db, id)?))
}

/// `POST /api/memory/{id}/rollback` — restore a version.
pub async fn rollback(
    Path(id): Path<i64>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<RollbackRequest>,
) -> Result<Json<MemoryEntry>, ApiError> {
    let entry = memory::rollback(&state.db, id, request.version, &context.actor)?;
    audit::record(
        &state,
        &context,
        "memory.rollback",
        &format!("{}/{}", entry.namespace, entry.key),
        Some(json!({ "id": entry.id, "version": request.version, "new_version": entry.version })),
    );
    Ok(Json(entry))
}

/// `POST /api/memory/{id}/promote` — copy into `global` (or another namespace).
pub async fn promote(
    Path(id): Path<i64>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PromoteRequest>,
) -> Result<Json<MemoryEntry>, ApiError> {
    let entry = memory::promote(&state.db, id, &request.namespace, &context.actor)?;
    audit::record(
        &state,
        &context,
        "memory.promote",
        &format!("{}/{}", entry.namespace, entry.key),
        Some(json!({ "from_id": id, "to": entry.namespace })),
    );
    Ok(Json(entry))
}

/// `DELETE /api/memory/{id}`.
pub async fn delete(
    Path(id): Path<i64>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<StatusCode, ApiError> {
    if memory::delete(&state.db, id, &context.actor)? {
        audit::record(&state, &context, "memory.delete", &id.to_string(), None);
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("unknown memory entry {id}")))
    }
}

/// `DELETE /api/memory/expired` — purge expired rows.
pub async fn purge(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let removed = memory::purge_expired(&state.db)?;
    audit::record(
        &state,
        &context,
        "memory.purge",
        "memory",
        Some(json!({ "removed": removed })),
    );
    Ok(Json(json!({ "removed": removed })))
}
