//! HTTP layer.

pub mod auth;
pub mod error;
pub mod health;
pub mod instances;
pub mod shares;
pub mod templates;

pub use error::ApiError;

use crate::config::Config;
#[cfg(not(feature = "embed-web"))]
use crate::config::VERSION;
use crate::db::{self, Db};
use crate::driver::Driver;
use crate::models::{InstanceSpec, Status};
use crate::store;
use axum::middleware;
use axum::routing::{delete, get, post};
#[cfg(not(feature = "embed-web"))]
use axum::Json;
use axum::Router;
#[cfg(not(feature = "embed-web"))]
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::Mutex as AsyncMutex;
use tower_http::trace::TraceLayer;

/// Shared state of every handler.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub driver: Arc<dyn Driver>,
    pub locks: Arc<InstanceLocks>,
    pub started_at: Instant,
}

impl AppState {
    /// Open the database, rebuild the index from the filesystem and return the
    /// ready state.
    pub fn bootstrap(cfg: Config, driver: Arc<dyn Driver>) -> anyhow::Result<Self> {
        store::ensure_layout(&cfg)?;
        let db = Db::open(&cfg.db_path())?;
        db::reconcile(&cfg, &db)?;
        Ok(Self {
            cfg: Arc::new(cfg),
            db,
            driver,
            locks: Arc::new(InstanceLocks::default()),
            started_at: Instant::now(),
        })
    }
}

/// One async mutex per instance id, so lifecycle operations on the same
/// instance never interleave.
#[derive(Default)]
pub struct InstanceLocks {
    inner: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl InstanceLocks {
    pub fn for_id(&self, id: &str) -> Arc<AsyncMutex<()>> {
        let mut map = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.entry(id.to_string()).or_default().clone()
    }
}

/// Build the full application router.
pub fn router(state: AppState) -> Router {
    let api = api_routes(state.clone());
    finalize(api, state)
}

fn api_routes(state: AppState) -> Router<AppState> {
    let protected: Router<AppState> = Router::new()
        .route("/auth/check", get(auth::check))
        .route("/templates", get(templates::list))
        .route("/instances", get(instances::list).post(instances::create))
        .route(
            "/instances/{id}",
            get(instances::get)
                .patch(instances::patch)
                .delete(instances::delete),
        )
        .route("/instances/{id}/start", post(instances::start))
        .route("/instances/{id}/stop", post(instances::stop))
        .route("/instances/{id}/restart", post(instances::restart))
        .route("/instances/{id}/logs", get(instances::logs))
        .route("/shares", get(shares::list).post(shares::create))
        .route("/shares/{name}", delete(shares::delete))
        .route("/shares/{name}/attach", post(shares::attach))
        .route("/shares/{name}/detach", post(shares::detach))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ));

    Router::new()
        .route("/health", get(health::health))
        .merge(protected)
        .fallback(api_not_found)
}

async fn api_not_found() -> ApiError {
    ApiError::not_found("route not found")
}

#[cfg(not(feature = "embed-web"))]
fn finalize(api: Router<AppState>, state: AppState) -> Router {
    use tower_http::services::{ServeDir, ServeFile};

    let app: Router<AppState> = match state
        .cfg
        .web_dist
        .clone()
        .filter(|dist| dist.join("index.html").is_file())
    {
        Some(dist) => {
            let index = dist.join("index.html");
            Router::new()
                .nest("/api", api)
                .fallback_service(ServeDir::new(dist).not_found_service(ServeFile::new(index)))
        }
        None => Router::new().nest("/api", api).fallback(frontend_hint),
    };
    app.with_state(state).layer(TraceLayer::new_for_http())
}

#[cfg(not(feature = "embed-web"))]
async fn frontend_hint() -> Json<serde_json::Value> {
    Json(json!({
        "name": "hermes-fleet",
        "version": VERSION,
        "hint": "The web console is not built. Run `pnpm build` in web/ and restart, \
                 or point FLEET_WEB_DIST at an existing build.",
        "api": "/api/health",
    }))
}

#[cfg(feature = "embed-web")]
fn finalize(api: Router<AppState>, state: AppState) -> Router {
    Router::new()
        .nest("/api", api)
        .fallback(embedded_asset)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

#[cfg(feature = "embed-web")]
#[derive(rust_embed::Embed)]
#[folder = "../web/dist/"]
struct WebAssets;

#[cfg(feature = "embed-web")]
async fn embedded_asset(uri: axum::http::Uri) -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;

    let requested = uri.path().trim_start_matches('/');
    let requested = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    let Some(file) = WebAssets::get(requested).or_else(|| WebAssets::get("index.html")) else {
        return ApiError::not_found("the embedded frontend is empty").into_response();
    };
    let cache = if requested.contains('.') {
        "public, max-age=3600"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, content_type(requested)),
            (header::CACHE_CONTROL, cache),
        ],
        file.data.into_owned(),
    )
        .into_response()
}

#[cfg(feature = "embed-web")]
fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// Write the compose file from the current spec and run `up -d`.
pub(crate) async fn apply_compose(
    state: &AppState,
    spec: &mut InstanceSpec,
) -> Result<(), ApiError> {
    let compose = crate::render::render_compose(&state.cfg, spec).map_err(ApiError::from)?;
    store::write_compose(&state.cfg, &spec.id, &compose)
        .map_err(|err| ApiError::internal(format!("failed to write compose file: {err}")))?;
    up_and_record(state, spec).await
}

/// `docker compose up -d`, then record the resulting status.
pub(crate) async fn up_and_record(
    state: &AppState,
    spec: &mut InstanceSpec,
) -> Result<(), ApiError> {
    match state.driver.up(spec).await {
        Ok(()) => {
            let status = state
                .driver
                .ps(spec)
                .await
                .map(|result| result.status)
                .unwrap_or(Status::Running);
            spec.status = status;
            state
                .db
                .set_status(&spec.id, status, None)
                .map_err(ApiError::from)?;
            Ok(())
        }
        Err(err) => {
            let detail = err.detail.clone();
            spec.status = Status::Error;
            let _ = state.db.set_status(&spec.id, Status::Error, Some(&detail));
            Err(ApiError::driver(err))
        }
    }
}
