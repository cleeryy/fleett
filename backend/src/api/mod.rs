//! HTTP layer.

pub mod audit;
pub mod auth;
pub mod error;
pub mod health;
pub mod instances;
pub mod shares;
pub mod templates;
pub mod tokens;

pub use error::ApiError;

use crate::auth::AuthService;
use crate::config::Config;
#[cfg(not(feature = "embed-web"))]
use crate::config::VERSION;
use crate::db::{self, Db};
use crate::driver::Driver;
use crate::models::{Health, InstanceSpec, Status, HEALTH_UNHEALTHY_MESSAGE};
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
    pub auth: Arc<AuthService>,
    pub locks: Arc<InstanceLocks>,
    pub started_at: Instant,
}

impl AppState {
    /// Open the database, rebuild the index from the filesystem and return the
    /// ready state.
    pub fn bootstrap(cfg: Config, driver: Arc<dyn Driver>) -> anyhow::Result<Self> {
        let db = Db::open(&cfg.db_path())?;
        Self::bootstrap_with_db(cfg, driver, db)
    }

    /// Same as [`AppState::bootstrap`] with an already open database, so the
    /// compose driver and the state share one connection pool.
    pub fn bootstrap_with_db(cfg: Config, driver: Arc<dyn Driver>, db: Db) -> anyhow::Result<Self> {
        let auth = AuthService::from_config(&cfg);
        Self::bootstrap_with_db_and_auth(cfg, driver, db, auth)
    }

    /// Bootstrap with an explicit [`AuthService`] (tests inject fake OIDC
    /// verifiers this way).
    pub fn bootstrap_with_db_and_auth(
        cfg: Config,
        driver: Arc<dyn Driver>,
        db: Db,
        auth: AuthService,
    ) -> anyhow::Result<Self> {
        store::ensure_layout(&cfg)?;
        db::reconcile(&cfg, &db)?;
        Ok(Self {
            cfg: Arc::new(cfg),
            db,
            driver,
            auth: Arc::new(auth),
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
    finalize(api, auth_pages(), state)
}

/// Browser-facing authentication routes (outside `/api` so a reverse proxy
/// can forward them to the control plane without touching XHR calls).
fn auth_pages() -> Router<AppState> {
    Router::new()
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
}

fn api_routes(state: AppState) -> Router<AppState> {
    let public: Router<AppState> = Router::new()
        .route("/health", get(health::health))
        .route("/auth/config", get(auth::config));

    let protected: Router<AppState> = Router::new()
        .route("/auth/check", get(auth::check))
        .route("/auth/me", get(auth::me))
        .route("/audit", get(audit::list))
        .route("/tokens", get(tokens::list).post(tokens::create))
        .route("/tokens/{id}", delete(tokens::revoke))
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
            auth::require_auth,
        ));

    Router::new()
        .merge(public)
        .merge(protected)
        .fallback(api_not_found)
}

async fn api_not_found() -> ApiError {
    ApiError::not_found("route not found")
}

#[cfg(not(feature = "embed-web"))]
fn finalize(api: Router<AppState>, pages: Router<AppState>, state: AppState) -> Router {
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
                .merge(pages)
                .fallback_service(ServeDir::new(dist).not_found_service(ServeFile::new(index)))
        }
        None => Router::new()
            .nest("/api", api)
            .merge(pages)
            .fallback(frontend_hint),
    };
    app.with_state(state).layer(TraceLayer::new_for_http())
}

#[cfg(not(feature = "embed-web"))]
async fn frontend_hint() -> Json<serde_json::Value> {
    Json(json!({
        "name": "fleett",
        "version": VERSION,
        "hint": "The web console is not built. Run `pnpm build` in web/ and restart, \
                 or point FLEETT_WEB_DIST at an existing build.",
        "api": "/api/health",
    }))
}

#[cfg(feature = "embed-web")]
fn finalize(api: Router<AppState>, pages: Router<AppState>, state: AppState) -> Router {
    Router::new()
        .nest("/api", api)
        .merge(pages)
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
    let project = store::declared_project(&state.cfg, &spec.id);
    let keys = crate::secrets::env_keys(&state.cfg, &state.db, &spec.id).map_err(ApiError::from)?;
    let compose =
        crate::render::render_compose_with_project(&state.cfg, spec, project.as_deref(), &keys)
            .map_err(ApiError::from)?;
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
            let observed = state.driver.ps(spec).await.ok();
            let status = observed
                .as_ref()
                .map(|result| result.status)
                .unwrap_or(Status::Running);
            let health = observed
                .as_ref()
                .map(|result| result.health)
                .unwrap_or(Health::Unknown);
            let last_error = match health {
                Health::Unhealthy => Some(HEALTH_UNHEALTHY_MESSAGE),
                _ => None,
            };
            spec.status = status;
            spec.last_error = last_error.map(str::to_string);
            state
                .db
                .set_status(&spec.id, status, last_error)
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
