//! Per-instance egress policy endpoints (issue #41).
//!
//! The handlers own validation, storage, the compose rewrite that points the
//! agent at the proxy, and the refused-destination log. They never claim the
//! proxy is enforcing an ACL they cannot see: `POST …/egress/test` reports the
//! policy decision and, separately, whether the proxy is reachable.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::instances;
use crate::api::{up_and_record, AppState};
use crate::auth::{AuthContext, Role};
use crate::egress as firewall;
use crate::egress_policy::{
    self, EgressBlock, EgressPolicy, PolicyInput, BLOCKS_DEFAULT_LIMIT, BLOCKS_MAX_LIMIT,
    INGEST_PER_MINUTE, MAX_BLOCK_HOST_LEN, MAX_BLOCK_REASON_LEN,
};
use crate::models::{now_iso, validate_instance_id, InstanceSpec};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Sliding window of the ingestion rate limit.
pub const INGEST_WINDOW: Duration = Duration::from_secs(60);

/// Per-instance budget of `POST /api/egress/blocks`.
///
/// A proxy that is being flooded by a scanning agent must not be able to fill
/// the database or the API thread pool: at most [`INGEST_PER_MINUTE`] records
/// per instance per minute. Records of different instances do not compete.
#[derive(Debug, Default)]
pub struct BlockRateLimiter {
    events: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl BlockRateLimiter {
    /// Record one attempt; `false` means the caller must answer `429`.
    pub fn allow(&self, instance: &str) -> bool {
        let now = Instant::now();
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Keep the map from growing without bound on a fleet with many ids.
        if events.len() > 4096 {
            events.retain(|_, queue| {
                queue
                    .back()
                    .is_some_and(|last| now.duration_since(*last) <= INGEST_WINDOW)
            });
        }
        let queue = events.entry(instance.to_string()).or_default();
        while queue
            .front()
            .is_some_and(|first| now.duration_since(*first) > INGEST_WINDOW)
        {
            queue.pop_front();
        }
        if queue.len() >= INGEST_PER_MINUTE {
            return false;
        }
        queue.push_back(now);
        true
    }

    #[cfg(test)]
    fn reset(&self) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

/// `GET /api/instances/{id}/egress` and `PUT …/egress`.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyView {
    pub instance: String,
    /// `explicit`, `allow_lan`, `legacy` or `disabled`.
    pub source: String,
    pub mode: String,
    pub fqdns: Vec<String>,
    pub cidrs: Vec<String>,
    pub ports: Vec<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Proxy the instance is pointed at, when one is rendered at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// Squid ACL snippet generated from an explicit policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_acl: Option<String>,
    /// Whether the policy decides nothing more than the host firewall does, in
    /// which case no proxy variable is rendered (issue #58).
    pub legacy_equivalent: bool,
    pub warnings: Vec<String>,
}

async fn policy_view(
    state: &AppState,
    spec: &InstanceSpec,
    policy: Option<&EgressPolicy>,
) -> PolicyView {
    let cfg = state.cfg.as_ref();
    let effective = egress_policy::effective(cfg, spec, policy);
    let mut warnings = Vec::new();
    let proxy = policy.and_then(|policy| egress_policy::proxy_url(cfg, policy));
    if let Some(policy) = policy {
        if proxy.is_none() && egress_policy::needs_proxy(cfg, policy) {
            warnings.push(
                "FLEETT_EGRESS_PROXY does not name a proxy and the policy has no proxy override: \
                 the policy is stored but no HTTP(S)_PROXY is rendered, so nothing routes the \
                 agent through the proxy"
                    .to_string(),
            );
        }
        if spec.allow_lan {
            warnings.push(if proxy.is_some() {
                "allow_lan is enabled: the instance joins the unrestricted LAN network and can \
                 bypass the proxy entirely; set allow_lan=false to enforce the policy"
                    .to_string()
            } else {
                "allow_lan is enabled: the instance joins the unrestricted LAN network, which the \
                 host egress firewall does not filter; set allow_lan=false to isolate it"
                    .to_string()
            });
        }
        if cfg.egress == crate::config::EgressMode::Off {
            warnings.push(firewall::WARNING_DISABLED.to_string());
        }
        // A rendered proxy that does not answer cuts *every* outbound
        // connection, which is exactly the silent outage of issue #58: say it
        // here instead of waiting for the operator to read aiohttp tracebacks.
        if let Some(proxy_url) = proxy.as_deref() {
            if let Err(detail) = state.egress_probe.reachable(proxy_url).await {
                warnings.push(format!(
                    "{detail}: the instance points at a proxy that does not answer, so every \
                     outbound connection fails; start the proxy or remove the policy with \
                     DELETE /api/instances/{}/egress",
                    spec.id
                ));
            }
        }
    } else {
        warnings.extend(firewall::warnings(cfg, spec.allow_lan));
    }
    PolicyView {
        instance: spec.id.clone(),
        source: effective.source.as_str().to_string(),
        mode: effective.mode.as_str().to_string(),
        fqdns: effective.fqdns.clone(),
        cidrs: effective.cidrs.clone(),
        ports: effective.ports.clone(),
        updated_at: policy.map(|policy| policy.updated_at.clone()),
        proxy,
        proxy_acl: policy.map(egress_policy::squid_acl),
        legacy_equivalent: policy.is_some_and(egress_policy::is_legacy_equivalent),
        warnings,
    }
}

/// `GET /api/instances/{id}/egress` — policy in force.
pub async fn get_policy(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<PolicyView>, ApiError> {
    let spec = instances::load(&state, &id)?;
    let policy = egress_policy::load(&state.db, &id).map_err(ApiError::from)?;
    Ok(Json(policy_view(&state, &spec, policy.as_ref()).await))
}

/// Rewrite the compose file and apply it, keeping the policy as the source of
/// truth for the proxy variables.
async fn apply(state: &AppState, spec: &mut InstanceSpec) -> Result<(), ApiError> {
    crate::api::write_compose_file(state, spec)?;
    up_and_record(state, spec).await
}

/// `PUT /api/instances/{id}/egress` — replace the explicit policy.
///
/// The compose file is rewritten and the container restarted so the proxy
/// variables reach the process. A failure after the database write restores the
/// previous row rather than leaving the store and the container disagreeing.
/// An unreachable proxy is reported in `warnings`, never as an error: the
/// operator decides whether to start the proxy or remove the policy (#58).
pub async fn put_policy(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(input): Json<PolicyInput>,
) -> Result<Json<PolicyView>, ApiError> {
    let normalized = egress_policy::normalize_input(&input)
        .map_err(|message| ApiError::bad_request("invalid_egress_policy", message))?;

    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let mut spec = instances::load(&state, &id)?;
    let previous = egress_policy::load(&state.db, &id).map_err(ApiError::from)?;
    let policy = EgressPolicy {
        instance_id: id.clone(),
        mode: normalized.mode,
        fqdns: normalized.fqdns,
        cidrs: normalized.cidrs,
        ports: normalized.ports,
        proxy: normalized.proxy,
        updated_at: now_iso(),
    };
    egress_policy::upsert(&state.db, &policy).map_err(ApiError::from)?;

    if let Err(err) = apply(&state, &mut spec).await {
        restore(&state, &id, previous.as_ref());
        return Err(err);
    }
    // The policy may have changed the proxy: probe again instead of reusing a
    // result cached for another configuration.
    state.egress_probe.clear();

    audit::record(
        &state,
        &context,
        "egress.policy.update",
        &id,
        Some(json!({
            "mode": policy.mode.as_str(),
            "fqdns": policy.fqdns,
            "cidrs": policy.cidrs,
            "ports": policy.ports,
            // The URL itself may carry credentials: log only that one is set.
            "proxy": policy.proxy.is_some(),
        })),
    );
    Ok(Json(policy_view(&state, &spec, Some(&policy)).await))
}

/// `DELETE /api/instances/{id}/egress` — drop the explicit policy and fall
/// back to the legacy `allow_lan` / `FLEETT_EGRESS` behaviour.
pub async fn delete_policy(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<PolicyView>, ApiError> {
    let lock = state.locks.for_id(&id);
    let _guard = lock.lock().await;

    let mut spec = instances::load(&state, &id)?;
    let previous = egress_policy::load(&state.db, &id).map_err(ApiError::from)?;
    if previous.is_none() {
        return Err(ApiError::not_found(format!(
            "instance \"{id}\" has no explicit egress policy"
        )));
    }
    egress_policy::delete(&state.db, &id).map_err(ApiError::from)?;

    if let Err(err) = apply(&state, &mut spec).await {
        restore(&state, &id, previous.as_ref());
        return Err(err);
    }
    // The proxy variables just disappeared: drop a cached failure for them.
    state.egress_probe.clear();

    audit::record(&state, &context, "egress.policy.delete", &id, None);
    Ok(Json(policy_view(&state, &spec, None).await))
}

/// Best-effort rollback of a policy row after a failed compose apply.
fn restore(state: &AppState, id: &str, previous: Option<&EgressPolicy>) {
    let outcome = match previous {
        Some(policy) => egress_policy::upsert(&state.db, policy),
        None => egress_policy::delete(&state.db, id).map(|_| ()),
    };
    if let Err(err) = outcome {
        tracing::warn!(instance = %id, %err, "could not restore the previous egress policy");
    }
    // Best effort: leave the compose file consistent with the restored row.
    if let Ok(spec) = instances::load(state, id) {
        if let Err(err) = crate::api::write_compose_file(state, &spec) {
            tracing::warn!(instance = %id, error = %err.message, "could not restore the compose file");
        }
    }
}

/// `POST /api/instances/{id}/egress/test`.
#[derive(Debug, Clone, Deserialize)]
pub struct TestRequest {
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
}

/// Result of one policy test.
#[derive(Debug, Clone, Serialize)]
pub struct TestResponse {
    pub instance: String,
    pub host: String,
    pub port: u16,
    /// Decision of the policy itself, not an observation of real traffic.
    pub allowed: bool,
    pub reason: String,
    pub source: String,
    pub mode: String,
    /// Whether a component that could enforce the decision is reachable.
    pub enforced: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

/// `POST /api/instances/{id}/egress/test` — evaluate one destination.
///
/// The endpoint is honest about the gap: `allowed` comes from the policy, and
/// `enforced` only becomes true when the proxy TCP endpoint answers (explicit
/// policy that needs one) or the host firewall marker matches (legacy, and any
/// policy equivalent to it).
pub async fn test(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<TestRequest>,
) -> Result<Json<TestResponse>, ApiError> {
    let host = egress_policy::validate_host(&request.host)
        .map_err(|message| ApiError::bad_request("invalid_host", message))?;
    let port = request
        .port
        .ok_or_else(|| ApiError::bad_request("invalid_port", "port is required (1..=65535)"))?;
    egress_policy::validate_port(port)
        .map_err(|message| ApiError::bad_request("invalid_port", message))?;

    let spec = instances::load(&state, &id)?;
    let policy = egress_policy::load(&state.db, &id).map_err(ApiError::from)?;
    let effective = egress_policy::effective(&state.cfg, &spec, policy.as_ref());
    let decision = effective.evaluate(&host, port);
    let (enforced, enforcement, proxy) = enforcement(&state, &spec, policy.as_ref()).await;

    Ok(Json(TestResponse {
        instance: id,
        host,
        port,
        allowed: decision.allowed,
        reason: decision.reason,
        source: effective.source.as_str().to_string(),
        mode: effective.mode.as_str().to_string(),
        enforced,
        enforcement,
        proxy,
    }))
}

async fn enforcement(
    state: &AppState,
    spec: &InstanceSpec,
    policy: Option<&EgressPolicy>,
) -> (bool, Option<String>, Option<String>) {
    if let Some(policy) = policy {
        if egress_policy::needs_proxy(&state.cfg, policy) {
            return proxy_enforcement(state, spec, policy).await;
        }
        // A policy equivalent to the legacy host firewall renders no proxy:
        // the component that enforces it is the host firewall, checked below.
    }
    if spec.allow_lan {
        return (
            false,
            Some(
                "allow_lan=true: the instance joins the unrestricted LAN network; no destination \
                 policy is enforced"
                    .to_string(),
            ),
            None,
        );
    }
    if state.cfg.egress == crate::config::EgressMode::Off {
        return (false, Some(firewall::WARNING_DISABLED.to_string()), None);
    }
    let marker = firewall::read_marker(&state.cfg.egress_marker);
    let subnet = state
        .driver
        .network_subnet(&spec.host, &state.cfg.network)
        .await
        .ok()
        .flatten();
    match firewall::check_marker(&marker, subnet.as_deref()) {
        Ok(()) => (
            true,
            Some(
                "the host egress firewall is installed for the current fleett subnet; Internet \
                 destinations stay reachable and LAN destinations are dropped"
                    .to_string(),
            ),
            None,
        ),
        Err(detail) => (false, Some(detail), None),
    }
}

async fn proxy_enforcement(
    state: &AppState,
    spec: &InstanceSpec,
    policy: &EgressPolicy,
) -> (bool, Option<String>, Option<String>) {
    let proxy = egress_policy::proxy_url(&state.cfg, policy);
    let Some(proxy_url) = proxy.as_deref() else {
        return (
            false,
            Some(
                "FLEETT_EGRESS_PROXY does not name a proxy and the policy has no proxy override: \
                 nothing routes the agent through a proxy, so the policy is recorded but not \
                 enforced"
                    .to_string(),
            ),
            None,
        );
    };
    if spec.allow_lan {
        return (
            false,
            Some(
                "allow_lan=true: the instance joins the unrestricted LAN network and can bypass \
                 the proxy"
                    .to_string(),
            ),
            Some(proxy_url.to_string()),
        );
    }
    match state.egress_probe.reachable(proxy_url).await {
        Ok(target) => (
            true,
            Some(format!(
                "proxy {target} is reachable; this probe checks the socket only, not that its \
                 ACLs match the stored policy"
            )),
            Some(proxy_url.to_string()),
        ),
        Err(detail) => (false, Some(detail), Some(proxy_url.to_string())),
    }
}

/// `GET /api/instances/{id}/egress/blocks` and `GET /api/egress/blocks`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BlocksQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    /// Admin view only: restrict to one instance.
    #[serde(default)]
    pub instance: Option<String>,
}

fn block_limit(query: &BlocksQuery) -> usize {
    query
        .limit
        .unwrap_or(BLOCKS_DEFAULT_LIMIT)
        .clamp(1, BLOCKS_MAX_LIMIT)
}

/// `GET /api/instances/{id}/egress/blocks` — newest first.
pub async fn list_blocks(
    Path(id): Path<String>,
    Query(query): Query<BlocksQuery>,
    State(state): State<AppState>,
) -> Result<Json<Vec<EgressBlock>>, ApiError> {
    let _spec = instances::load(&state, &id)?;
    let blocks = egress_policy::list_blocks(&state.db, Some(&id), block_limit(&query))
        .map_err(ApiError::from)?;
    Ok(Json(blocks))
}

/// `DELETE /api/instances/{id}/egress/blocks` — clear the log.
pub async fn clear_blocks(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let _spec = instances::load(&state, &id)?;
    let removed = egress_policy::clear_blocks(&state.db, &id).map_err(ApiError::from)?;
    audit::record(
        &state,
        &context,
        "egress.blocks.clear",
        &id,
        Some(json!({ "removed": removed })),
    );
    Ok(Json(json!({ "removed": removed })))
}

/// `GET /api/egress/blocks?instance=&limit=` — admin view of every instance.
pub async fn list_all_blocks(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<BlocksQuery>,
) -> Result<Json<Vec<EgressBlock>>, ApiError> {
    if context.role != Role::Admin {
        return Err(ApiError::forbidden(
            "forbidden",
            "this endpoint requires the admin role",
        ));
    }
    if let Some(instance) = query
        .instance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        validate_instance_id(instance)
            .map_err(|message| ApiError::bad_request("invalid_instance", message))?;
    }
    let instance = query
        .instance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let blocks = egress_policy::list_blocks(&state.db, instance, block_limit(&query))
        .map_err(ApiError::from)?;
    Ok(Json(blocks))
}

/// `POST /api/egress/blocks` — authenticated ingestion for the proxy.
#[derive(Debug, Clone, Deserialize)]
pub struct IngestBlockRequest {
    /// Instance the proxy refused the destination for.
    pub instance: String,
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /api/egress/blocks` — record one refused destination.
///
/// Bearer/session authenticated (a viewer token cannot POST), validated and
/// rate-limited per instance. The proxy is expected to call it after its ACL
/// refuses a request; fleett ships no log shipper, which the docs state.
pub async fn ingest_block(
    State(state): State<AppState>,
    Json(request): Json<IngestBlockRequest>,
) -> Result<(StatusCode, Json<EgressBlock>), ApiError> {
    let instance = request.instance.trim().to_string();
    validate_instance_id(&instance)
        .map_err(|message| ApiError::bad_request("invalid_instance", message))?;
    if !state.db.exists(&instance).map_err(ApiError::from)? {
        return Err(ApiError::not_found(format!(
            "instance \"{instance}\" not found"
        )));
    }

    let host = request.host.trim().to_string();
    if host.is_empty() || host.len() > MAX_BLOCK_HOST_LEN {
        return Err(ApiError::bad_request(
            "invalid_host",
            format!("host must be between 1 and {MAX_BLOCK_HOST_LEN} bytes"),
        ));
    }
    let host = egress_policy::validate_host(&host)
        .map_err(|message| ApiError::bad_request("invalid_host", message))?;
    let port = request
        .port
        .ok_or_else(|| ApiError::bad_request("invalid_port", "port is required (1..=65535)"))?;
    egress_policy::validate_port(port)
        .map_err(|message| ApiError::bad_request("invalid_port", message))?;
    let reason = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(reason) = &reason {
        if reason.len() > MAX_BLOCK_REASON_LEN {
            return Err(ApiError::bad_request(
                "invalid_reason",
                format!("reason must be at most {MAX_BLOCK_REASON_LEN} bytes"),
            ));
        }
    }

    if !state.block_ingest.allow(&instance) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            format!("at most {INGEST_PER_MINUTE} blocked destinations per instance per minute"),
        ));
    }

    let block = egress_policy::insert_block(&state.db, &instance, &host, port, reason.as_deref())
        .map_err(ApiError::from)?;
    Ok((StatusCode::CREATED, Json(block)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingestion_budget_is_per_instance() {
        let limiter = BlockRateLimiter::default();
        for _ in 0..INGEST_PER_MINUTE {
            assert!(limiter.allow("alice"));
        }
        assert!(!limiter.allow("alice"), "the budget is exhausted");
        assert!(
            limiter.allow("bob"),
            "another instance keeps its own budget"
        );
        limiter.reset();
        assert!(limiter.allow("alice"));
    }
}
