//! Declarative desired-state endpoints (issue #34).
//!
//! `POST /api/desired-state/plan` compares a `fleet.yaml` document with the
//! live fleet without writing anything. `POST /api/desired-state/apply` runs
//! the same plan, recording the document in `desired_state` once every
//! mutation succeeded. `GET /api/desired-state` and `GET
//! /api/desired-state/drift` expose the stored document and the plan
//! recomputed from it, i.e. the drift introduced outside the file.
//!
//! Every apply is audited as `gitops.apply`. See `docs/GITOPS.md`.

use crate::api::audit::{record, require_admin};
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::gitops::{
    self, ApplyReport, DesiredStateView, DriftReport, FleetDocument, GitopsError, Plan,
};
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use serde_json::Value;

/// Query of the plan/apply routes.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DesiredStateQuery {
    /// Execute the deletions the document implies (`true`/`false`).
    #[serde(default)]
    pub allow_delete: Option<bool>,
    /// Origin recorded in `desired_state.source` (`api`, `cli`, `cron`, …).
    #[serde(default)]
    pub source: Option<String>,
}

/// One parsed request body.
struct Input {
    document: FleetDocument,
    /// Text stored in `desired_state` (never contains a secret value).
    raw: String,
    allow_delete: bool,
    source: String,
}

/// `POST /api/desired-state/plan`.
///
/// The body is either the document itself or `{"document": …, "allow_delete":
/// false}`; `document` may be a JSON object or the raw YAML text. Deletions are
/// reported in `refused` unless `allow_delete` is set.
pub async fn plan(
    State(state): State<AppState>,
    Query(query): Query<DesiredStateQuery>,
    Json(body): Json<Value>,
) -> Result<Json<Plan>, ApiError> {
    let input = parse_input(body, &query)?;
    let plan = gitops::plan(&state.cfg, &state.db, &input.document, input.allow_delete)
        .map_err(gitops_error)?;
    Ok(Json(plan))
}

/// `POST /api/desired-state/apply`: plan, execute, store the document.
pub async fn apply(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<DesiredStateQuery>,
    Json(body): Json<Value>,
) -> Result<Json<ApplyReport>, ApiError> {
    require_admin(&context)?;
    let input = parse_input(body, &query)?;
    let report = gitops::apply(
        &state,
        &input.document,
        &input.raw,
        &input.source,
        input.allow_delete,
    )
    .await
    .map_err(gitops_error)?;

    record(
        &state,
        &context,
        "gitops.apply",
        "desired-state",
        Some(json!({
            "source": report.source,
            "applied_at": report.applied_at,
            "created": report.plan.create.len(),
            "updated": report.plan.update.len(),
            "deleted": report.plan.delete.len(),
            "refused": report.plan.refused.len(),
            "allow_delete": input.allow_delete,
        })),
    );
    Ok(Json(report))
}

/// `GET /api/desired-state`: stored document, JSON view and drift.
pub async fn get(State(state): State<AppState>) -> Result<Json<DesiredStateView>, ApiError> {
    let view = gitops::desired_state(&state.cfg, &state.db).map_err(gitops_error)?;
    Ok(Json(view))
}

/// `GET /api/desired-state/drift`: only the recomputed plan.
pub async fn drift(State(state): State<AppState>) -> Result<Json<DriftReport>, ApiError> {
    let report = gitops::drift(&state.cfg, &state.db).map_err(gitops_error)?;
    Ok(Json(report))
}

fn parse_input(body: Value, query: &DesiredStateQuery) -> Result<Input, ApiError> {
    let wrapped = body.get("document").is_some();
    let allow_delete = if wrapped {
        body.get("allow_delete")
            .and_then(Value::as_bool)
            .or(query.allow_delete)
            .unwrap_or(false)
    } else {
        query.allow_delete.unwrap_or(false)
    };
    let source = if wrapped {
        body.get("source")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| query.source.clone())
            .unwrap_or_else(|| "api".to_string())
    } else {
        query.source.clone().unwrap_or_else(|| "api".to_string())
    };
    let document_value = if wrapped {
        body.get("document").cloned().unwrap_or(Value::Null)
    } else {
        body
    };

    let (document, raw) = match document_value {
        Value::String(text) => {
            let document = FleetDocument::parse(&text).map_err(gitops_error)?;
            (document, text)
        }
        Value::Null => {
            return Err(ApiError::bad_request(
                "invalid_fleet_document",
                "the request body must carry a fleet document",
            ))
        }
        other => {
            let document: FleetDocument = serde_json::from_value(other.clone()).map_err(|err| {
                ApiError::bad_request(
                    "invalid_fleet_document",
                    format!("invalid fleet document: {err}"),
                )
            })?;
            document.validate().map_err(gitops_error)?;
            let raw = serde_json::to_string_pretty(&other).map_err(|err| {
                ApiError::internal(format!("cannot serialise the document: {err}"))
            })?;
            (document, raw)
        }
    };
    Ok(Input {
        document,
        raw,
        allow_delete,
        source,
    })
}

fn gitops_error(err: GitopsError) -> ApiError {
    match err {
        GitopsError::Invalid(message) => ApiError::bad_request("invalid_fleet_document", message),
        GitopsError::Secret(message) => ApiError::bad_request("invalid_secret_ref", message),
        GitopsError::Conflict(message) => ApiError::conflict("gitops_conflict", message),
        GitopsError::Operation(message) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "gitops_failed", message)
        }
        GitopsError::Db(err) => ApiError::from(err),
        GitopsError::Env(err) => ApiError::from(err),
        GitopsError::Render(err) => ApiError::from(err),
    }
}
