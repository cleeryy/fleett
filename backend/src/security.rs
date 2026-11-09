//! API hardening: in-memory rate limiting, progressive authentication lockout
//! and HTTP security headers (issue #49).
//!
//! The limiter is a sliding window keyed by `(client IP, credential, route
//! class)`: exhausting one token never affects another token, and exhausting
//! one IP never affects another address. The credential is a SHA-256 digest of
//! the bearer token (or of the session cookie), never the secret itself, so the
//! limiter state cannot leak a usable credential.
//!
//! Failed authentications additionally feed a progressive lockout keyed by
//! `(client IP, credential shape)`. The shape (`bearer`, `session`, `anonymous`)
//! instead of the token digest is deliberate: an attacker rotating random
//! tokens must still hit the lockout, while a client that keeps presenting one
//! bad token is also slowed down by the ordinary rate limit.
//!
//! Everything lives in process memory and dies with the process, like the
//! Prometheus counters: a restart is a legitimate reset. No dependency is used.

use crate::api::auth::{bearer_token, client_ip, cookie_value};
use crate::api::AppState;
use crate::auth::session::SESSION_COOKIE;
use crate::auth::tokens;
use crate::config::SecurityConfig;
use crate::db::NewAuditRecord;
use crate::models::now_iso;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Content-Security-Policy of the embedded console.
///
/// Inline styles stay allowed for the single-file error pages; no inline
/// script, no external origin, no framing.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; \
     style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; \
     font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// `Permissions-Policy` sent with every response.
pub const PERMISSIONS_POLICY: &str = "camera=(), microphone=(), geolocation=()";

/// `Referrer-Policy` sent with every response.
pub const REFERRER_POLICY: &str = "no-referrer";

/// `X-Frame-Options` sent with every response.
pub const X_FRAME_OPTIONS: &str = "DENY";

/// `Strict-Transport-Security` sent when the public URL is https.
pub const STRICT_TRANSPORT_SECURITY: &str = "max-age=31536000; includeSubDomains";

/// `X-Content-Type-Options` sent with every response.
pub const X_CONTENT_TYPE_OPTIONS: &str = "nosniff";

/// Length of one rate-limit window: every limit is a number per minute.
pub const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

/// How often stale buckets are collected.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

/// Hard bound on the number of buckets: a flood of unique keys must not grow
/// the process memory without limit. Past this point the whole map is dropped,
/// which only forgets recent counters — availability wins over exactness.
const MAX_BUCKETS: usize = 50_000;

/// Bucket class: which configured limit applies to a request path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteClass {
    /// Not rate-limited (`/api/health`, `/api/auth/config`, the static console).
    Exempt,
    /// Ordinary `/api/*` traffic.
    Api,
    /// Browser authentication pages (`/auth/login`, `/auth/callback`).
    Auth,
    /// Expensive instance routes (`/exec`, `/logs`, `/files`, `/export`).
    Heavy,
}

impl RouteClass {
    /// Label used by `fleett_rate_limited_total{class=...}`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exempt => "exempt",
            Self::Api => "api",
            Self::Auth => "auth",
            Self::Heavy => "heavy",
        }
    }
}

/// Classes always exposed by the metrics endpoint, so a dashboard never has to
/// guess whether a class was never refused or its counter is zero.
pub const RATE_LIMIT_CLASSES: [&str; 4] = ["api", "auth", "heavy", "lockout"];

/// Classify a request path for the limiter.
pub fn classify(path: &str) -> RouteClass {
    let path = path.trim_end_matches('/');
    if path == "/api/health" || path == "/api/auth/config" {
        return RouteClass::Exempt;
    }
    if path == "/auth/login" || path == "/auth/callback" || path.starts_with("/auth/") {
        return RouteClass::Auth;
    }
    if path.starts_with("/api/") {
        // Any segment, not just the last one: `/instances/{id}/files/content`
        // and `/instances/{id}/export` are as expensive as `/files` itself.
        if path
            .split('/')
            .any(|segment| matches!(segment, "exec" | "logs" | "files" | "export"))
        {
            return RouteClass::Heavy;
        }
        return RouteClass::Api;
    }
    RouteClass::Exempt
}

/// Result of one limiter check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// The sliding window of the class is exhausted.
    Limited {
        limit: u32,
        retry_after_s: u64,
        class: &'static str,
    },
    /// The progressive authentication lockout is engaged.
    Locked {
        retry_after_s: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BucketKey {
    ip: String,
    credential: String,
    class: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LockoutKey {
    ip: String,
    shape: &'static str,
}

#[derive(Debug, Default)]
struct LockoutEntry {
    /// Timestamps of the recent failures, oldest first.
    failures: VecDeque<Instant>,
    /// Set while the lockout refuses new attempts.
    locked_until: Option<Instant>,
}

#[derive(Debug, Default)]
struct LimiterState {
    buckets: HashMap<BucketKey, VecDeque<Instant>>,
    lockouts: HashMap<LockoutKey, LockoutEntry>,
    last_prune: Option<Instant>,
}

/// In-memory rate limiter and lockout, shared by every handler.
#[derive(Debug)]
pub struct Security {
    cfg: SecurityConfig,
    state: Mutex<LimiterState>,
}

impl Security {
    pub fn new(cfg: SecurityConfig) -> Self {
        Self {
            cfg,
            state: Mutex::new(LimiterState::default()),
        }
    }

    /// Effective configuration (`GET /api/security/status`).
    pub fn config(&self) -> &SecurityConfig {
        &self.cfg
    }

    /// Check one request against the lockout, then its sliding window.
    ///
    /// An allowed request is recorded immediately, so concurrent requests
    /// cannot overshoot a limit.
    pub fn check(
        &self,
        ip: &str,
        credential: &str,
        shape: &'static str,
        class: RouteClass,
        now: Instant,
    ) -> Decision {
        let mut state = self.lock();
        self.prune(&mut state, now);

        if self.cfg.lockout_enabled() {
            let key = LockoutKey {
                ip: ip.to_string(),
                shape,
            };
            if let Some(entry) = state.lockouts.get(&key) {
                if let Some(until) = entry.locked_until {
                    if until > now {
                        return Decision::Locked {
                            retry_after_s: seconds_between(now, until),
                        };
                    }
                }
            }
        }

        let Some(limit) = self.limit_for(class) else {
            return Decision::Allow;
        };
        let key = BucketKey {
            ip: ip.to_string(),
            credential: credential.to_string(),
            class: class.as_str(),
        };
        let bucket = state.buckets.entry(key).or_default();
        while bucket
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= RATE_LIMIT_WINDOW)
        {
            bucket.pop_front();
        }
        if bucket.len() >= limit as usize {
            let oldest = bucket.front().copied().unwrap_or(now);
            let reset_at = oldest + RATE_LIMIT_WINDOW;
            return Decision::Limited {
                limit,
                retry_after_s: seconds_between(now, reset_at),
                class: class.as_str(),
            };
        }
        bucket.push_back(now);
        Decision::Allow
    }

    /// Record one failed authentication.
    ///
    /// Returns `true` exactly when this failure engaged the lockout, so the
    /// caller can log and audit the transition once.
    pub fn record_failure(&self, ip: &str, shape: &'static str, now: Instant) -> bool {
        if !self.cfg.lockout_enabled() {
            return false;
        }
        let mut state = self.lock();
        self.prune(&mut state, now);
        let entry = state
            .lockouts
            .entry(LockoutKey {
                ip: ip.to_string(),
                shape,
            })
            .or_default();
        // An expired lock must not prevent the next one from engaging.
        if entry.locked_until.is_some_and(|until| until <= now) {
            entry.locked_until = None;
        }
        while entry.failures.front().is_some_and(|at| {
            now.saturating_duration_since(*at) >= Duration::from_secs(self.cfg.lockout_window_s)
        }) {
            entry.failures.pop_front();
        }
        entry.failures.push_back(now);
        if entry.failures.len() >= self.cfg.lockout_failures as usize
            && entry.locked_until.is_none()
        {
            entry.locked_until = Some(now + Duration::from_secs(self.cfg.lockout_s));
            return true;
        }
        false
    }

    /// A successful authenticated request clears the failure counter.
    pub fn record_success(&self, ip: &str, shape: &'static str) {
        if !self.cfg.lockout_enabled() {
            return;
        }
        let mut state = self.lock();
        state.lockouts.remove(&LockoutKey {
            ip: ip.to_string(),
            shape,
        });
    }

    /// Number of distinct client addresses currently locked out.
    pub fn locked_ips(&self) -> usize {
        let now = Instant::now();
        let state = self.lock();
        state
            .lockouts
            .iter()
            .filter(|(_, entry)| entry.locked_until.is_some_and(|until| until > now))
            .map(|(key, _)| key.ip.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    /// Number of live rate-limit buckets (tests and diagnostics).
    #[cfg(test)]
    fn bucket_count(&self) -> usize {
        self.lock().buckets.len()
    }

    fn limit_for(&self, class: RouteClass) -> Option<u32> {
        let limit = match class {
            RouteClass::Exempt => return None,
            RouteClass::Api => self.cfg.rate_limit_per_min,
            RouteClass::Auth => self.cfg.rate_limit_auth_per_min,
            RouteClass::Heavy => self.cfg.rate_limit_heavy_per_min,
        };
        (limit > 0).then_some(limit)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LimiterState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Drop stale buckets and expired lockouts, at most once per minute.
    ///
    /// Buckets keep only the timestamps of the current window; a bucket without
    /// one is forgotten. Lockouts survive while either some failure is recent or
    /// the lock itself has not expired.
    fn prune(&self, state: &mut LimiterState, now: Instant) {
        let due = state
            .last_prune
            .is_none_or(|last| now.saturating_duration_since(last) >= PRUNE_INTERVAL);
        if !due {
            return;
        }
        state.last_prune = Some(now);
        state.buckets.retain(|_, bucket| {
            while bucket
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) >= RATE_LIMIT_WINDOW)
            {
                bucket.pop_front();
            }
            !bucket.is_empty()
        });
        let window = Duration::from_secs(self.cfg.lockout_window_s);
        state.lockouts.retain(|_, entry| {
            while entry
                .failures
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) >= window)
            {
                entry.failures.pop_front();
            }
            let locked = entry.locked_until.is_some_and(|until| until > now);
            if !locked && entry.locked_until.is_some() {
                entry.locked_until = None;
            }
            locked || !entry.failures.is_empty()
        });
        if state.buckets.len() > MAX_BUCKETS {
            state.buckets.clear();
        }
    }
}

/// Whole seconds between `now` and `until`, at least one (a `Retry-After: 0`
/// would invite an immediate retry that is still refused).
fn seconds_between(now: Instant, until: Instant) -> u64 {
    let duration = until.saturating_duration_since(now);
    let secs = duration.as_secs() + u64::from(duration.subsec_nanos() > 0);
    secs.max(1)
}

/// Credential identity of the request, as a digest.
fn credential_key(headers: &HeaderMap) -> String {
    if let Some(token) = bearer_token(headers) {
        return format!("bearer:{}", tokens::hash(&token));
    }
    if let Some(session) = cookie_value(headers, SESSION_COOKIE) {
        return format!("session:{}", tokens::hash(&session));
    }
    "anonymous".to_string()
}

/// Coarse credential shape used by the lockout (never the secret itself).
fn credential_shape(headers: &HeaderMap) -> &'static str {
    if bearer_token(headers).is_some() {
        "bearer"
    } else if cookie_value(headers, SESSION_COOKIE).is_some() {
        "session"
    } else {
        "anonymous"
    }
}

/// Middleware enforcing the lockout and the rate limits on `/api/*` and the
/// authentication pages, and recording authentication failures.
pub async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let class = classify(request.uri().path());
    if class == RouteClass::Exempt {
        return next.run(request).await;
    }

    let now = Instant::now();
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let ip = client_ip(request.headers(), peer).unwrap_or_else(|| "unknown".to_string());
    let credential = credential_key(request.headers());
    let shape = credential_shape(request.headers());

    match state.security.check(&ip, &credential, shape, class, now) {
        Decision::Allow => {}
        Decision::Limited {
            limit,
            retry_after_s,
            class,
        } => {
            state.metrics.record_rate_limited(class);
            tracing::debug!(
                %ip,
                class,
                limit,
                retry_after_s,
                "request refused by the rate limiter"
            );
            return rate_limited(
                retry_after_s,
                json!({ "retry_after_s": retry_after_s, "class": class }),
            );
        }
        Decision::Locked { retry_after_s } => {
            state.metrics.record_rate_limited("lockout");
            tracing::warn!(
                %ip,
                shape,
                retry_after_s,
                "request refused: authentication lockout"
            );
            return rate_limited(
                retry_after_s,
                json!({ "retry_after_s": retry_after_s, "reason": "lockout" }),
            );
        }
    }

    let response = next.run(request).await;
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED {
        if state.security.record_failure(&ip, shape, now) {
            record_lockout(&state, &ip, shape);
        }
    } else if status.is_success() || status.is_redirection() {
        state.security.record_success(&ip, shape);
    }
    response
}

/// 429 with `Retry-After`, in the documented error envelope.
fn rate_limited(retry_after_s: u64, details: serde_json::Value) -> Response {
    let body = json!({
        "error": {
            "code": "rate_limited",
            "message": format!(
                "too many requests: retry in {retry_after_s} second(s)"
            ),
            "details": details,
        }
    });
    let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after_s.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// One `auth.lockout` audit row per engagement (never per refused attempt).
fn record_lockout(state: &AppState, ip: &str, shape: &'static str) {
    let cfg = state.security.config();
    tracing::warn!(
        %ip,
        shape,
        failures = cfg.lockout_failures,
        window_s = cfg.lockout_window_s,
        lockout_s = cfg.lockout_s,
        "authentication lockout engaged"
    );
    let entry = NewAuditRecord {
        at: now_iso(),
        actor: "anonymous".to_string(),
        role: "none".to_string(),
        method: shape.to_string(),
        action: "auth.lockout".to_string(),
        target: ip.to_string(),
        ip: Some(ip.to_string()),
        details: Some(
            json!({
                "failures": cfg.lockout_failures,
                "window_s": cfg.lockout_window_s,
                "lockout_s": cfg.lockout_s,
                "credential": shape,
            })
            .to_string(),
        ),
    };
    if let Err(err) = state.db.record_audit(&entry) {
        tracing::warn!(%err, "could not append the auth.lockout audit entry");
    }
}

/// Middleware adding the security headers to every response, API or UI.
///
/// HSTS is conditional: it is only sent when `FLEETT_PUBLIC_URL` is https, since
/// announcing it on a plain-HTTP deployment would pin browsers to a scheme the
/// server cannot serve.
pub async fn headers(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        HeaderValue::from_static(X_FRAME_OPTIONS),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static(REFERRER_POLICY),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static(PERMISSIONS_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static(X_CONTENT_TYPE_OPTIONS),
    );
    if state.cfg.hsts_enabled() {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static(STRICT_TRANSPORT_SECURITY),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecurityConfig;

    fn security(configure: impl FnOnce(&mut SecurityConfig)) -> Security {
        let mut cfg = SecurityConfig {
            rate_limit_per_min: 0,
            rate_limit_auth_per_min: 0,
            rate_limit_heavy_per_min: 0,
            lockout_failures: 0,
            ..SecurityConfig::default()
        };
        configure(&mut cfg);
        Security::new(cfg)
    }

    #[test]
    fn paths_choose_the_documented_bucket() {
        assert_eq!(classify("/api/health"), RouteClass::Exempt);
        assert_eq!(classify("/api/health/"), RouteClass::Exempt);
        assert_eq!(classify("/api/auth/config"), RouteClass::Exempt);
        assert_eq!(classify("/"), RouteClass::Exempt);
        assert_eq!(classify("/assets/app.js"), RouteClass::Exempt);
        assert_eq!(classify("/auth/login"), RouteClass::Auth);
        assert_eq!(classify("/auth/callback"), RouteClass::Auth);
        assert_eq!(classify("/api/instances"), RouteClass::Api);
        assert_eq!(classify("/api/instances/alice"), RouteClass::Api);
        assert_eq!(classify("/api/instances/alice/logs"), RouteClass::Heavy);
        assert_eq!(classify("/api/instances/alice/files"), RouteClass::Heavy);
        assert_eq!(
            classify("/api/instances/alice/files/content"),
            RouteClass::Heavy
        );
        assert_eq!(classify("/api/instances/alice/export"), RouteClass::Heavy);
        assert_eq!(classify("/api/instances/alice/exec"), RouteClass::Heavy);
    }

    #[test]
    fn the_sliding_window_refuses_and_reopens_after_a_minute() {
        let limiter = security(|cfg| cfg.rate_limit_per_min = 2);
        let now = Instant::now();
        assert_eq!(
            limiter.check("1.2.3.4", "t", "bearer", RouteClass::Api, now),
            Decision::Allow
        );
        assert_eq!(
            limiter.check("1.2.3.4", "t", "bearer", RouteClass::Api, now),
            Decision::Allow
        );
        let decision = limiter.check("1.2.3.4", "t", "bearer", RouteClass::Api, now);
        assert!(
            matches!(
                decision,
                Decision::Limited {
                    limit: 2,
                    retry_after_s: 60,
                    class: "api"
                }
            ),
            "{decision:?}"
        );
        // Another token, another IP: untouched.
        assert_eq!(
            limiter.check("1.2.3.4", "other", "bearer", RouteClass::Api, now),
            Decision::Allow
        );
        assert_eq!(
            limiter.check("5.6.7.8", "t", "bearer", RouteClass::Api, now),
            Decision::Allow
        );
        // After the window the bucket is open again.
        let later = now + RATE_LIMIT_WINDOW + Duration::from_millis(1);
        assert_eq!(
            limiter.check("1.2.3.4", "t", "bearer", RouteClass::Api, later),
            Decision::Allow
        );
    }

    #[test]
    fn a_zero_limit_disables_the_bucket() {
        let limiter = security(|_| {});
        let now = Instant::now();
        for _ in 0..100 {
            assert_eq!(
                limiter.check("1.2.3.4", "t", "bearer", RouteClass::Api, now),
                Decision::Allow
            );
        }
    }

    #[test]
    fn the_lockout_engages_after_the_configured_failures_and_expires() {
        let limiter = security(|cfg| {
            cfg.lockout_failures = 2;
            cfg.lockout_window_s = 600;
            cfg.lockout_s = 5;
        });
        let now = Instant::now();
        assert!(!limiter.record_failure("1.2.3.4", "bearer", now));
        assert!(limiter.record_failure("1.2.3.4", "bearer", now));
        assert_eq!(limiter.locked_ips(), 1);
        assert!(matches!(
            limiter.check("1.2.3.4", "t", "bearer", RouteClass::Api, now),
            Decision::Locked { retry_after_s: 5 }
        ));
        // Another IP or another credential shape is not locked.
        assert_eq!(
            limiter.check("5.6.7.8", "t", "bearer", RouteClass::Api, now),
            Decision::Allow
        );
        assert_eq!(
            limiter.check("1.2.3.4", "t", "session", RouteClass::Api, now),
            Decision::Allow
        );
        let later = now + Duration::from_secs(6);
        assert_eq!(
            limiter.check("1.2.3.4", "t", "bearer", RouteClass::Api, later),
            Decision::Allow
        );
    }

    #[test]
    fn a_success_resets_the_failure_counter() {
        let limiter = security(|cfg| cfg.lockout_failures = 2);
        let now = Instant::now();
        assert!(!limiter.record_failure("1.2.3.4", "bearer", now));
        limiter.record_success("1.2.3.4", "bearer");
        assert!(!limiter.record_failure("1.2.3.4", "bearer", now));
        assert_eq!(limiter.locked_ips(), 0);
    }

    #[test]
    fn failures_outside_the_window_do_not_count() {
        let limiter = security(|cfg| {
            cfg.lockout_failures = 2;
            cfg.lockout_window_s = 60;
        });
        let now = Instant::now();
        assert!(!limiter.record_failure("1.2.3.4", "bearer", now));
        let later = now + Duration::from_secs(61);
        assert!(!limiter.record_failure("1.2.3.4", "bearer", later));
        assert_eq!(limiter.locked_ips(), 0);
    }

    #[test]
    fn an_expired_lock_can_be_engaged_again() {
        let limiter = security(|cfg| {
            cfg.lockout_failures = 1;
            cfg.lockout_window_s = 60;
            cfg.lockout_s = 2;
        });
        let now = Instant::now();
        assert!(limiter.record_failure("1.2.3.4", "bearer", now));
        // While the lock is live, no new failure is recorded (the guard returns
        // 429 before the handler, so this is just the limiter contract).
        let later = now + Duration::from_secs(3);
        assert!(limiter.record_failure("1.2.3.4", "bearer", later));
        assert!(matches!(
            limiter.check("1.2.3.4", "t", "bearer", RouteClass::Api, later),
            Decision::Locked { retry_after_s: 2 }
        ));
    }

    #[test]
    fn stale_buckets_are_pruned() {
        let limiter = security(|cfg| cfg.rate_limit_per_min = 5);
        let now = Instant::now();
        limiter.check("1.2.3.4", "a", "bearer", RouteClass::Api, now);
        limiter.check("1.2.3.4", "b", "bearer", RouteClass::Api, now);
        assert_eq!(limiter.bucket_count(), 2);

        let later = now + Duration::from_secs(61);
        limiter.check("5.6.7.8", "c", "bearer", RouteClass::Api, later);
        assert_eq!(
            limiter.bucket_count(),
            1,
            "the two stale buckets must be collected"
        );
    }

    #[test]
    fn the_retry_after_is_never_zero() {
        assert_eq!(seconds_between(Instant::now(), Instant::now()), 1);
    }
}
