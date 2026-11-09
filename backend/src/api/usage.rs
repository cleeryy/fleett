//! `GET /api/usage` — CPU, memory and disk usage of the fleet (issue #10).
//!
//! Instances may live on several Docker hosts (issue #40). Each host is
//! sampled separately: an unreachable daemon yields a warning and zeroed
//! numbers for its instances instead of failing the whole report.

use crate::api::error::ApiError;
use crate::api::AppState;
use crate::driver;
use crate::models::{UsageCaps, UsageEntry, UsageResponse, UsageTotals};
use axum::extract::State;
use axum::Json;

/// Usage of every instance plus the fleet totals and the configured caps.
///
/// Read-only, so every authenticated role may call it. An empty fleet answers
/// with zeros and never talks to Docker.
pub async fn usage(State(state): State<AppState>) -> Result<Json<UsageResponse>, ApiError> {
    let specs = state.db.list().map_err(ApiError::from)?;
    let usage = driver::usage_by_host(&*state.driver, &specs).await;

    let mut instances = Vec::with_capacity(specs.len());
    let mut totals = UsageTotals {
        cpu_pct: 0.0,
        mem_used_bytes: 0,
        mem_limit_bytes: 0,
        disk_bytes: 0,
    };
    for (index, spec) in specs.iter().enumerate() {
        let sample = usage.samples.get(index).cloned().unwrap_or_default();
        let status = if sample.running {
            "running".to_string()
        } else {
            spec.status.as_str().to_string()
        };
        totals.cpu_pct += sample.cpu_pct;
        totals.mem_used_bytes += sample.mem_used_bytes;
        totals.mem_limit_bytes += sample.mem_limit_bytes;
        totals.disk_bytes += sample.disk_bytes;
        instances.push(UsageEntry {
            id: spec.id.clone(),
            status,
            cpu_pct: sample.cpu_pct,
            mem_used_bytes: sample.mem_used_bytes,
            mem_limit_bytes: sample.mem_limit_bytes,
            disk_bytes: sample.disk_bytes,
        });
    }

    Ok(Json(UsageResponse {
        instances,
        totals,
        caps: UsageCaps {
            max_cpu: state.cfg.max_total_cpu,
            max_mem_mb: state.cfg.max_total_mem_mb,
        },
        warnings: usage.warnings,
    }))
}
