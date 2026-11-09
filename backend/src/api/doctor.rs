//! `GET /api/doctor` and `GET /api/onboarding` (issue #52).
//!
//! Both routes are read-only and open to every authenticated role: a viewer
//! needs the checklist as much as an admin to know why a page is empty. The
//! doctor never fails on a check: a broken dependency becomes one `error` line.

use crate::api::AppState;
use crate::doctor::Report;
use crate::onboarding::Onboarding;
use axum::extract::State;
use axum::Json;

/// `GET /api/doctor` — the checklist of `fleett doctor`, for the console.
pub async fn doctor(State(state): State<AppState>) -> Json<Report> {
    let uptime_s = state.started_at.elapsed().as_secs() as i64;
    Json(crate::doctor::report(&state.cfg, &state.db, state.driver.as_ref(), uptime_s).await)
}

/// `GET /api/onboarding` — the four first-run steps and their live state.
pub async fn onboarding(State(state): State<AppState>) -> Json<Onboarding> {
    Json(crate::onboarding::report(&state.cfg, &state.db, state.driver.as_ref()).await)
}
