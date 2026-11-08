//! `GET /api/templates`

use crate::api::AppState;
use crate::models::TemplateInfo;
use crate::render;
use axum::extract::State;
use axum::Json;

pub async fn list(State(state): State<AppState>) -> Json<Vec<TemplateInfo>> {
    Json(render::list_templates(&state.cfg))
}
