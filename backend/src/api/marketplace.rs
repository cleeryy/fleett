//! Marketplace routes (issue #61).
//!
//! The catalogue itself lives in [`crate::marketplace`]: providers, source
//! grammar, downloads and validation. This module owns authentication, the
//! install/update locks, the audit trail, and the rolling promote shared with
//! the skill registry. Reads need any authenticated role; every mutation needs
//! `admin`, exactly like skills, personas and templates.
//!
//! Nothing here ever executes imported content, and a response never contains
//! a token or a secret-looking environment variable.

use crate::api::audit::{record, require_admin};
use crate::api::error::ApiError;
use crate::api::skills::promote_and_restart;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::history;
use crate::marketplace::{
    MarketplaceError, SearchResponse, SyncOutcome, UpdateOutcome, UpdatesResponse,
    DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT,
};
use crate::skills::SkillDetail;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

impl From<MarketplaceError> for ApiError {
    fn from(err: MarketplaceError) -> Self {
        match err {
            MarketplaceError::InvalidSource(message) => {
                ApiError::bad_request("invalid_source", message)
            }
            MarketplaceError::NoIndexConfigured => ApiError::conflict(
                "no_index_configured",
                MarketplaceError::NoIndexConfigured.to_string(),
            ),
            MarketplaceError::NotFound(name) => {
                ApiError::not_found(format!("marketplace skill \"{name}\" not found"))
            }
            MarketplaceError::NoUpstream(name) => ApiError::conflict(
                "no_upstream",
                format!("skill \"{name}\" has no upstream provenance"),
            ),
            MarketplaceError::Unchanged(name) => ApiError::conflict(
                "upstream_unchanged",
                format!("the upstream content of \"{name}\" has not changed"),
            ),
            MarketplaceError::ScriptsRequireConfirmation(scripts) => ApiError::conflict(
                "scripts_require_confirmation",
                "this skill contains scripts; confirm them explicitly to install it",
            )
            .with_details(json!({ "scripts": scripts })),
            MarketplaceError::RateLimited => ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many marketplace previews: wait a minute and retry",
            ),
            MarketplaceError::Fetch(message) => {
                ApiError::new(StatusCode::BAD_GATEWAY, "marketplace_unavailable", message)
            }
            MarketplaceError::Invalid(message) => {
                ApiError::bad_request("invalid_marketplace_response", message)
            }
            MarketplaceError::Io(message) => ApiError::internal(message),
            MarketplaceError::Skill(err) => ApiError::from(err),
        }
    }
}

/// Query of `GET /api/marketplace/skills`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Body of `POST /api/marketplace/skills/preview`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PreviewRequest {
    pub source: String,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
}

/// Body of `POST /api/marketplace/skills/install`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InstallRequest {
    pub source: String,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub confirm_scripts: bool,
}

/// Body of `POST /api/marketplace/skills/{name}/update`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateRequest {
    #[serde(default)]
    pub promote: bool,
    #[serde(default)]
    pub confirm_scripts: bool,
}

/// Response of `POST /api/marketplace/skills/{name}/update`.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateResponse {
    pub name: String,
    pub from: String,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub files: Vec<crate::marketplace::FileChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted: Option<crate::skills::PromoteResponse>,
}

impl From<UpdateOutcome> for UpdateResponse {
    fn from(outcome: UpdateOutcome) -> Self {
        Self {
            name: outcome.detail.name.clone(),
            from: outcome.from,
            to: outcome.to,
            commit: outcome.commit,
            files: outcome.files,
            promoted: None,
        }
    }
}

/// `GET /api/marketplace/skills?q=&source=&limit=` — every provider at once.
pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
    if limit == 0 || limit > MAX_SEARCH_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_SEARCH_LIMIT}"),
        ));
    }
    let response = state
        .marketplace
        .search(
            &state.cfg,
            query.q.as_deref().unwrap_or_default(),
            query.source.as_deref(),
            limit,
        )
        .await
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

/// `POST /api/marketplace/skills/preview` — resolve and validate without
/// writing anything. Reads are open to every role, but the call performs an
/// outbound fetch, so it is rate-limited per process.
pub async fn preview(
    State(state): State<AppState>,
    Json(request): Json<PreviewRequest>,
) -> Result<Json<crate::marketplace::PreviewResponse>, ApiError> {
    if !state.marketplace.allow_preview() {
        return Err(ApiError::from(MarketplaceError::RateLimited));
    }
    let preview = state
        .marketplace
        .preview(&state.cfg, &request.source, request.reference.as_deref())
        .await
        .map_err(ApiError::from)?;
    Ok(Json(preview))
}

/// `POST /api/marketplace/skills/install` — publish the next free version with
/// its provenance. Refuses executable content unless explicitly confirmed.
pub async fn install(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<InstallRequest>,
) -> Result<(StatusCode, Json<SkillDetail>), ApiError> {
    require_admin(&context)?;
    // One marketplace install at a time: two concurrent calls would otherwise
    // both compute the same "next free version" and one would lose the race.
    let lock = state.locks.for_id("marketplace:install");
    let _guard = lock.lock().await;
    let author = history::author_for(&context);
    let detail = state
        .marketplace
        .install(
            &state.cfg,
            &state.db,
            &request.source,
            request.reference.as_deref(),
            request.name.as_deref(),
            request.confirm_scripts,
            &author,
        )
        .await
        .map_err(ApiError::from)?;
    state.marketplace.invalidate();
    record(
        &state,
        &context,
        "marketplace.install",
        &detail.name,
        Some(json!({
            "source": request.source,
            "ref": request.reference,
            "version": detail.latest,
            "commit": detail.commit,
            "files": detail.files.len(),
        })),
    );
    Ok((StatusCode::CREATED, Json(detail)))
}

/// `POST /api/marketplace/skills/{name}/update` — publish the diff as the next
/// version, then optionally move `latest` and roll the followers with the same
/// health-gated loop as `POST /api/skills/{name}/promote`.
pub async fn update(
    Path(name): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<UpdateResponse>, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id(&format!("marketplace:update:{name}"));
    let _guard = lock.lock().await;
    let author = history::author_for(&context);
    let outcome = state
        .marketplace
        .update(
            &state.cfg,
            &state.db,
            &name,
            request.confirm_scripts,
            &author,
        )
        .await
        .map_err(ApiError::from)?;
    let mut response = UpdateResponse::from(outcome);
    if request.promote {
        let report =
            promote_and_restart(&state, &context, &name, &response.to, "marketplace.update")
                .await?;
        response.promoted = Some(report);
    } else {
        record(
            &state,
            &context,
            "marketplace.update",
            &name,
            Some(json!({
                "from": response.from,
                "to": response.to,
                "commit": response.commit,
                "files": response.files.len(),
            })),
        );
    }
    state.marketplace.invalidate();
    Ok(Json(response))
}

/// `GET /api/marketplace/updates` — installed skills with a newer upstream.
/// A provider outage yields an empty list plus a warning, never a 500.
pub async fn updates(State(state): State<AppState>) -> Json<UpdatesResponse> {
    let response = state.marketplace.updates(&state.cfg, &state.db).await;
    Json(response)
}

/// `POST /api/marketplace/sync` — refresh the local index from the configured
/// git repository, atomically.
pub async fn sync(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<SyncOutcome>, ApiError> {
    require_admin(&context)?;
    let lock = state.locks.for_id("marketplace:sync");
    let _guard = lock.lock().await;
    let outcome = state
        .marketplace
        .sync(&state.cfg)
        .await
        .map_err(ApiError::from)?;
    record(
        &state,
        &context,
        "marketplace.sync",
        &outcome.source,
        Some(json!({
            "skills": outcome.skills,
            "commit": outcome.commit,
        })),
    );
    Ok(Json(outcome))
}
