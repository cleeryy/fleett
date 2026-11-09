//! Token accounting and cost endpoints (issue #35).
//!
//! Reads (`GET /api/cost`, `/api/cost/budgets`, `/api/models/pricing` and the
//! per-instance budget) are open to every authenticated role; writes require
//! admin, like every other mutation of the fleet.

use crate::api::audit::{record, require_admin};
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::cost::{
    self, BudgetStatus, CostReport, GroupBy, IngestSummary, NewPrice, NewUsageEvent, Source,
};
use crate::models::now_iso;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostQuery {
    /// Inclusive lower bound: `YYYY-MM-DD` or RFC 3339 (default: first day of
    /// the current month).
    #[serde(default)]
    pub from: Option<String>,
    /// Upper bound: a date covers the whole day, a timestamp is exclusive
    /// (default: now).
    #[serde(default)]
    pub to: Option<String>,
    /// `instance` (default), `model` or `day`.
    #[serde(default)]
    pub group_by: Option<String>,
}

/// `GET /api/cost` — aggregation, top agents, budgets and month projection.
pub async fn report(
    State(state): State<AppState>,
    Query(query): Query<CostQuery>,
) -> Result<Json<CostReport>, ApiError> {
    let group_by = match query.group_by.as_deref() {
        None => GroupBy::Instance,
        Some(value) => GroupBy::parse(value).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_group_by",
                "group_by must be \"instance\", \"model\" or \"day\"",
            )
        })?,
    };
    let report = cost::report(
        &state.db,
        query.from.as_deref(),
        query.to.as_deref(),
        group_by,
    )?;
    Ok(Json(report))
}

#[derive(Debug, Clone, Deserialize)]
pub struct IngestRequest {
    /// Reporting instance.
    pub instance: String,
    /// Log lines to parse (mutually exclusive with `text` and the explicit
    /// token counts).
    #[serde(default)]
    pub lines: Option<Vec<String>>,
    /// Raw log text to parse, split on newlines.
    #[serde(default)]
    pub text: Option<String>,
    /// Model of the events (default: the model of the instance).
    #[serde(default)]
    pub model: Option<String>,
    /// Timestamp of lines that carry none (default: now).
    #[serde(default)]
    pub at: Option<String>,
    /// `log`, `api` or `test` (default: `api`).
    #[serde(default)]
    pub source: Option<String>,
    /// Explicit counts, when the caller already parsed the upstream response.
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

/// `POST /api/usage/ingest` — agent- or test-driven ingestion.
///
/// Accepts either raw log lines (`lines` or `text`) or explicit token counts.
/// The response counts what was parsed and what was written; replaying the
/// same payload writes nothing (`inserted` stays `0`).
pub async fn ingest(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<IngestRequest>,
) -> Result<Json<IngestSummary>, ApiError> {
    require_admin(&context)?;
    let spec = state
        .db
        .get(&request.instance)
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::not_found(format!("instance \"{}\" not found", request.instance))
        })?;
    let source = match request.source.as_deref() {
        None => Source::Api,
        Some(value) => Source::parse(value).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_source",
                "source must be \"log\", \"api\" or \"test\"",
            )
        })?,
    };
    let model = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(spec.model.as_str())
        .to_string();

    let alert = |state: &AppState| {
        if let Err(err) = cost::emit_budget_alerts(&state.db) {
            tracing::warn!(%err, "could not evaluate the cost budgets");
        }
    };

    let explicit = request.prompt_tokens.is_some()
        || request.completion_tokens.is_some()
        || request.total_tokens.is_some();
    if explicit {
        let prompt = request.prompt_tokens.unwrap_or(0);
        let completion = request.completion_tokens.unwrap_or(0);
        let total = request.total_tokens.unwrap_or(prompt + completion);
        if prompt == 0 && completion == 0 && total == 0 {
            return Err(ApiError::bad_request(
                "empty_usage",
                "prompt_tokens, completion_tokens and total_tokens are all zero",
            ));
        }
        let at = match request.at.as_deref() {
            None => now_iso(),
            Some(value) => cost::normalize_timestamp(value).ok_or_else(|| {
                ApiError::bad_request(
                    "invalid_timestamp",
                    format!("at must be an RFC 3339 timestamp, got \"{value}\""),
                )
            })?,
        };
        let inserted = cost::ingest_event(
            &state.db,
            &NewUsageEvent {
                instance_id: spec.id.clone(),
                at,
                model: model.clone(),
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: total,
                source,
            },
        )
        .map_err(ApiError::from)?;
        record(
            &state,
            &context,
            "usage.ingest",
            &spec.id,
            Some(json!({"source": source.as_str(), "model": model, "inserted": inserted})),
        );
        if inserted {
            alert(&state);
        }
        return Ok(Json(IngestSummary {
            parsed: 1,
            inserted: usize::from(inserted),
            duplicates: usize::from(!inserted),
            ignored: 0,
        }));
    }

    let owned: Vec<String> = match (request.lines, request.text) {
        (Some(lines), _) => lines,
        (None, Some(text)) => {
            if text.len() > cost::MAX_INGEST_TEXT_BYTES {
                return Err(ApiError::bad_request(
                    "text_too_large",
                    format!("text must be at most {} bytes", cost::MAX_INGEST_TEXT_BYTES),
                ));
            }
            text.lines().map(str::to_string).collect()
        }
        (None, None) => {
            return Err(ApiError::bad_request(
                "missing_usage",
                "provide lines, text or the prompt_tokens/completion_tokens counts",
            ))
        }
    };
    if owned.len() > cost::MAX_INGEST_LINES {
        return Err(ApiError::bad_request(
            "too_many_lines",
            format!("at most {} lines are accepted", cost::MAX_INGEST_LINES),
        ));
    }
    let at = match request.at.as_deref() {
        None => None,
        Some(value) => Some(cost::normalize_timestamp(value).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_timestamp",
                format!("at must be an RFC 3339 timestamp, got \"{value}\""),
            )
        })?),
    };
    let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
    let summary = cost::ingest_lines(&state.db, &spec.id, &lines, &model, at.as_deref(), source)
        .map_err(ApiError::from)?;
    if summary.inserted > 0 {
        record(
            &state,
            &context,
            "usage.ingest",
            &spec.id,
            Some(json!({"source": source.as_str(), "model": model, "inserted": summary.inserted})),
        );
        alert(&state);
    }
    Ok(Json(summary))
}

#[derive(Debug, Clone, Deserialize)]
pub struct PricingRequest {
    pub model: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default)]
    pub currency: Option<String>,
    /// `YYYY-MM-DD` or RFC 3339 (default: today, UTC).
    #[serde(default)]
    pub effective_from: Option<String>,
}

/// `GET /api/models/pricing` — the latest price of every model.
pub async fn pricing_list(
    State(state): State<AppState>,
) -> Result<Json<Vec<cost::Pricing>>, ApiError> {
    Ok(Json(cost::list_prices(&state.db).map_err(ApiError::from)?))
}

/// `POST /api/models/pricing` — add or replace the price effective on a day.
///
/// A new effective date keeps the previous versions: the historical report is
/// recomputed with the price that was effective on each event day.
pub async fn pricing_set(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<PricingRequest>,
) -> Result<Json<cost::Pricing>, ApiError> {
    require_admin(&context)?;
    let model = request.model.trim().to_string();
    let effective_from = request
        .effective_from
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
    let price = cost::set_price(
        &state.db,
        NewPrice {
            model: request.model,
            input_per_mtok: request.input_per_mtok,
            output_per_mtok: request.output_per_mtok,
            currency: request.currency.unwrap_or_else(|| "USD".to_string()),
            effective_from,
        },
    )?;
    record(
        &state,
        &context,
        "pricing.set",
        &model,
        Some(json!({
            "input_per_mtok": price.input_per_mtok,
            "output_per_mtok": price.output_per_mtok,
            "currency": price.currency,
            "effective_from": price.effective_from,
        })),
    );
    Ok(Json(price))
}

/// `DELETE /api/models/pricing/{model}` — drop every version of one model.
pub async fn pricing_delete(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Path(model): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&context)?;
    if !cost::delete_prices(&state.db, &model).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!(
            "no price for model \"{model}\""
        )));
    }
    record(&state, &context, "pricing.delete", &model, None);
    Ok(StatusCode::NO_CONTENT)
}

/// Budget of one instance plus its month-to-date consumption.
#[derive(Debug, Clone, Serialize)]
pub struct BudgetView {
    pub instance_id: String,
    pub month: String,
    /// `null` when no budget is configured.
    pub monthly_usd: Option<f64>,
    pub enabled: bool,
    pub configured: bool,
    pub used_usd: f64,
    pub percent: f64,
    pub exceeded: bool,
    pub projected_month_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

fn budget_view(
    statuses: &[BudgetStatus],
    instance_id: &str,
    budget: Option<&cost::Budget>,
) -> BudgetView {
    let month = statuses
        .first()
        .map(|status| status.month.clone())
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m").to_string());
    match budget {
        Some(budget) => match statuses.iter().find(|s| s.instance_id == instance_id) {
            Some(status) => BudgetView {
                instance_id: instance_id.to_string(),
                month: status.month.clone(),
                monthly_usd: Some(budget.monthly_usd),
                enabled: budget.enabled,
                configured: true,
                used_usd: status.used_usd,
                percent: status.percent,
                exceeded: status.exceeded,
                projected_month_usd: status.projected_month_usd,
                updated_at: Some(budget.updated_at.clone()),
            },
            None => BudgetView {
                instance_id: instance_id.to_string(),
                month,
                monthly_usd: Some(budget.monthly_usd),
                enabled: budget.enabled,
                configured: true,
                used_usd: 0.0,
                percent: 0.0,
                exceeded: false,
                projected_month_usd: 0.0,
                updated_at: Some(budget.updated_at.clone()),
            },
        },
        None => BudgetView {
            instance_id: instance_id.to_string(),
            month,
            monthly_usd: None,
            enabled: false,
            configured: false,
            used_usd: 0.0,
            percent: 0.0,
            exceeded: false,
            projected_month_usd: 0.0,
            updated_at: None,
        },
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct BudgetRequest {
    pub monthly_usd: f64,
    /// Defaults to `true`: setting a budget also arms it.
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `GET /api/instances/{id}/budget` — configured budget and month-to-date use.
pub async fn budget_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<BudgetView>, ApiError> {
    if !state.db.exists(&id).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!("instance \"{id}\" not found")));
    }
    let budget = cost::get_budget(&state.db, &id).map_err(ApiError::from)?;
    let statuses = cost::budget_statuses(&state.db, chrono::Utc::now()).map_err(ApiError::from)?;
    Ok(Json(budget_view(&statuses, &id, budget.as_ref())))
}

/// `PUT /api/instances/{id}/budget` — set (or disable) the monthly ceiling.
pub async fn budget_put(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(request): Json<BudgetRequest>,
) -> Result<Json<BudgetView>, ApiError> {
    require_admin(&context)?;
    if !state.db.exists(&id).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!("instance \"{id}\" not found")));
    }
    let enabled = request.enabled.unwrap_or(true);
    cost::set_budget(&state.db, &id, request.monthly_usd, enabled)?;
    record(
        &state,
        &context,
        "budget.set",
        &id,
        Some(json!({"monthly_usd": request.monthly_usd, "enabled": enabled})),
    );
    let budget = cost::get_budget(&state.db, &id).map_err(ApiError::from)?;
    let statuses = cost::budget_statuses(&state.db, chrono::Utc::now()).map_err(ApiError::from)?;
    Ok(Json(budget_view(&statuses, &id, budget.as_ref())))
}

/// `GET /api/cost/budgets` — every configured budget with its status.
pub async fn budgets(State(state): State<AppState>) -> Result<Json<Vec<BudgetStatus>>, ApiError> {
    Ok(Json(
        cost::budget_statuses(&state.db, chrono::Utc::now()).map_err(ApiError::from)?,
    ))
}
