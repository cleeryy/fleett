//! `GET /api/health` — public.

use crate::api::AppState;
use crate::config::VERSION;
use axum::extract::State;
use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub version: &'static str,
    pub driver: &'static str,
    pub docker: DockerHealth,
    /// Control-plane uptime, in seconds.
    pub uptime_s: i64,
}

#[derive(Debug, Serialize)]
pub struct DockerHealth {
    pub ok: bool,
}

pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let docker_ok = state.driver.ping().await;
    Json(HealthResponse {
        ok: true,
        version: VERSION,
        driver: state.cfg.driver.as_str(),
        docker: DockerHealth { ok: docker_ok },
        uptime_s: state.started_at.elapsed().as_secs() as i64,
    })
}
