//! `GET /api/health` — public.

use crate::api::AppState;
use crate::config::VERSION;
use crate::egress;
use crate::secrets;
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
    /// Secrets-at-rest state, checked by `fleett doctor`.
    pub secrets: SecretsHealth,
    /// Host egress firewall state, checked by `fleett doctor`.
    pub egress: EgressHealth,
}

#[derive(Debug, Serialize)]
pub struct DockerHealth {
    pub ok: bool,
}

#[derive(Debug, Serialize)]
pub struct SecretsHealth {
    /// A usable encryption key is configured.
    pub key: bool,
    /// Why the configured key is unusable, when it is.
    pub key_error: Option<String>,
    /// Instances whose secrets are stored encrypted.
    pub encrypted: usize,
    /// Instances whose secrets are still in a plaintext `secrets.env`.
    pub plaintext: usize,
}

#[derive(Debug, Serialize)]
pub struct EgressHealth {
    /// `restrict` or `off`.
    pub mode: &'static str,
    /// The marker written by `fleett egress install` is present.
    pub marker: bool,
}

pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let docker_ok = state.driver.ping().await;
    let secrets = secrets::status(&state.cfg, &state.db);
    let marker = egress::read_marker(&state.cfg.egress_marker);
    Json(HealthResponse {
        ok: true,
        version: VERSION,
        driver: state.cfg.driver.as_str(),
        docker: DockerHealth { ok: docker_ok },
        uptime_s: state.started_at.elapsed().as_secs() as i64,
        secrets: SecretsHealth {
            key: secrets.source.is_some(),
            key_error: secrets.key_error,
            encrypted: secrets.encrypted_instances,
            plaintext: secrets.legacy_files.len(),
        },
        egress: EgressHealth {
            mode: state.cfg.egress.as_str(),
            marker: marker.is_present(),
        },
    })
}
