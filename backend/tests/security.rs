//! Integration tests for the API hardening (issue #49): rate limiting,
//! progressive authentication lockout, security headers and the status route.
//!
//! Every test runs against `MockDriver`; no test ever calls Docker. The limiter
//! state lives in `AppState`, so each request of a test goes through a fresh
//! `api::router(...)` built on the same state.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::{Config, SecurityConfig};
use fleett::driver::mock::MockDriver;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";

struct Harness {
    dir: tempfile::TempDir,
    state: AppState,
}

impl Harness {
    fn new() -> Self {
        Self::with_config(|_| {})
    }

    fn with_config(configure: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        configure(&mut cfg);
        let driver = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, driver).expect("bootstrap state");
        Self { dir, state }
    }

    /// Rate limiting enabled with the documented defaults.
    fn with_hardening(configure: impl FnOnce(&mut Config)) -> Self {
        Self::with_config(|cfg| {
            cfg.security = SecurityConfig::default();
            configure(cfg);
        })
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        ip: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, String) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        // The limiter reads the first `X-Forwarded-For` hop, like the audit log.
        if let Some(ip) = ip {
            builder = builder.header("x-forwarded-for", ip);
        }
        let body = match body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let request = builder.body(body).expect("valid request");
        let response = api::router(self.state.clone())
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        (status, headers, String::from_utf8_lossy(&bytes).to_string())
    }

    async fn get(&self, uri: &str, token: Option<&str>) -> (StatusCode, HeaderMap, String) {
        self.send(Method::GET, uri, token, None, None).await
    }

    /// Create a named token with the bootstrap admin credential.
    async fn create_token(&self, role: &str) -> (String, String) {
        let (status, _, body) = self
            .send(
                Method::POST,
                "/api/tokens",
                Some(TOKEN),
                None,
                Some(json!({ "name": "security-test", "role": role })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "token create failed: {body}");
        let value = parse(&body);
        (
            value["id"].as_str().expect("token id").to_string(),
            value["token"].as_str().expect("token secret").to_string(),
        )
    }
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

fn header_value(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn retry_after(headers: &HeaderMap) -> u64 {
    header_value(headers, header::RETRY_AFTER)
        .unwrap_or_else(|| panic!("no Retry-After header: {headers:?}"))
        .parse()
        .expect("Retry-After is an integer number of seconds")
}

async fn wrong_attempt(harness: &Harness) -> StatusCode {
    harness.get("/api/instances", Some("wrong-token")).await.0
}

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' \
                   'unsafe-inline'; img-src 'self' data:; connect-src 'self'; \
                   font-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
                   form-action 'self'";

// ---------------------------------------------------------------------------
// Rate limiting and lockout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn twenty_invalid_attempts_end_in_a_retryable_429() {
    // The defaults: 120 attempts/min but a lockout after 10 failures.
    let harness = Harness::with_hardening(|_| {});

    let mut unauthorized = 0;
    let mut limited = 0;
    for _ in 0..20 {
        let (status, headers, body) = harness
            .send(
                Method::GET,
                "/api/instances",
                Some("not-a-valid-token"),
                None,
                None,
            )
            .await;
        match status {
            StatusCode::UNAUTHORIZED => {
                assert_eq!(parse(&body)["error"]["code"], "unauthorized");
                unauthorized += 1;
            }
            StatusCode::TOO_MANY_REQUESTS => {
                assert!(retry_after(&headers) >= 1);
                let value = parse(&body);
                assert_eq!(value["error"]["code"], "rate_limited");
                assert_eq!(value["error"]["details"]["reason"], "lockout");
                assert!(
                    value["error"]["details"]["retry_after_s"]
                        .as_u64()
                        .is_some(),
                    "{value}"
                );
                limited += 1;
            }
            other => panic!("unexpected status {other}: {body}"),
        }
    }
    assert_eq!(unauthorized, 10, "the first ten failures are 401");
    assert_eq!(limited, 10, "the following attempts are locked out");

    // The lockout is recorded once, in the audit trail. Read it from another
    // address: the test's own IP+shape is locked out by now.
    let (status, _, body) = harness
        .send(
            Method::GET,
            "/api/audit?limit=100",
            Some(TOKEN),
            Some("198.51.100.9"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    let lockout = entries
        .as_array()
        .expect("audit array")
        .iter()
        .find(|entry| entry["action"] == "auth.lockout")
        .expect("auth.lockout audit entry");
    assert_eq!(lockout["role"], "none");
    assert_eq!(lockout["details"]["failures"], 10);
    assert_eq!(lockout["details"]["credential"], "bearer");
    // The refused credential never reaches the audit trail.
    assert!(!body.contains("not-a-valid-token"), "{body}");
}

#[tokio::test]
async fn exhausting_one_token_leaves_the_others_alone() {
    let harness = Harness::with_config(|cfg| {
        cfg.security.rate_limit_per_min = 2;
        cfg.security.lockout_failures = 0;
    });
    let (_id, other_token) = harness.create_token("viewer").await;

    // Bootstrap token: the creation above used one slot, one more is allowed.
    let (status, _, body) = harness.get("/api/instances", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, headers, body) = harness.get("/api/instances", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(retry_after(&headers) >= 1);
    let value = parse(&body);
    assert_eq!(value["error"]["code"], "rate_limited");
    assert_eq!(value["error"]["details"]["class"], "api");

    // Another token is not affected by the first one's bucket.
    let (status, _, body) = harness.get("/api/instances", Some(&other_token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Neither is the same token from another address.
    let (status, _, body) = harness
        .send(
            Method::GET,
            "/api/instances",
            Some(TOKEN),
            Some("203.0.113.7"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_successful_authentication_resets_the_lockout() {
    let harness = Harness::with_config(|cfg| {
        cfg.security = SecurityConfig {
            rate_limit_per_min: 0,
            rate_limit_auth_per_min: 0,
            rate_limit_heavy_per_min: 0,
            lockout_failures: 3,
            lockout_window_s: 600,
            lockout_s: 900,
        };
    });

    // Two failures, then one success clears the counter.
    assert_eq!(wrong_attempt(&harness).await, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong_attempt(&harness).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        harness.get("/api/instances", Some(TOKEN)).await.0,
        StatusCode::OK
    );

    // Two more failures still answer 401: the counter started over.
    assert_eq!(wrong_attempt(&harness).await, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong_attempt(&harness).await, StatusCode::UNAUTHORIZED);

    // The third failure engages the lockout; the next attempt is refused.
    assert_eq!(wrong_attempt(&harness).await, StatusCode::UNAUTHORIZED);
    let (status, headers, body) = harness.get("/api/instances", Some("wrong-token")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(retry_after(&headers) > 800);
}

#[tokio::test]
async fn a_zero_lockout_never_locks_out() {
    let harness = Harness::with_config(|cfg| {
        cfg.security.rate_limit_per_min = 0;
        cfg.security.lockout_failures = 0;
    });
    for _ in 0..40 {
        let (status, _, _) = harness.get("/api/instances", Some("wrong-token")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

// ---------------------------------------------------------------------------
// Buckets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_and_auth_config_are_exempt() {
    let harness = Harness::with_config(|cfg| {
        cfg.security.rate_limit_per_min = 1;
        cfg.security.rate_limit_auth_per_min = 1;
        cfg.security.rate_limit_heavy_per_min = 1;
        cfg.security.lockout_failures = 0;
    });

    for _ in 0..5 {
        let (status, _, body) = harness.get("/api/health", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    for _ in 0..5 {
        let (status, _, body) = harness.get("/api/auth/config", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    // The ordinary bucket is exhausted after its single slot.
    let (status, _, body) = harness.get("/api/instances", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = harness.get("/api/instances", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
}

#[tokio::test]
async fn the_auth_pages_use_their_own_bucket() {
    let harness = Harness::with_config(|cfg| {
        cfg.security.rate_limit_auth_per_min = 1;
        cfg.security.lockout_failures = 0;
    });

    // OIDC is disabled, so the handler answers 404 — but it still consumes the
    // auth bucket, which is the point of the protection.
    let (status, _, _) = harness.get("/auth/login", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, headers, body) = harness.get("/auth/login", None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(retry_after(&headers) >= 1);

    // The API bucket was never touched.
    let (status, _, body) = harness.get("/api/instances", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn the_expensive_routes_have_their_own_bucket() {
    let harness = Harness::with_config(|cfg| {
        cfg.security.rate_limit_per_min = 100;
        cfg.security.rate_limit_heavy_per_min = 1;
        cfg.security.lockout_failures = 0;
    });

    let (status, _, body) = harness.get("/api/instances", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Unknown instance: 404, but the heavy slot is consumed.
    let (status, _, _) = harness.get("/api/instances/ghost/logs", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, headers, body) = harness.get("/api/instances/ghost/files", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(retry_after(&headers) >= 1);
    assert_eq!(parse(&body)["error"]["details"]["class"], "heavy");
}

// ---------------------------------------------------------------------------
// Security headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_security_headers_are_on_the_ui_and_the_api() {
    let harness = Harness::new();
    for uri in ["/", "/api/health"] {
        let (status, headers, _) = harness.get(uri, None).await;
        assert!(status.is_success(), "{uri}: {status}");
        assert_eq!(
            header_value(&headers, header::CONTENT_SECURITY_POLICY),
            Some(CSP)
        );
        assert_eq!(
            header_value(&headers, header::X_FRAME_OPTIONS),
            Some("DENY")
        );
        assert_eq!(
            header_value(&headers, header::REFERRER_POLICY),
            Some("no-referrer")
        );
        assert_eq!(
            header_value(&headers, header::X_CONTENT_TYPE_OPTIONS),
            Some("nosniff")
        );
        let permissions = headers
            .get("permissions-policy")
            .and_then(|value| value.to_str().ok());
        assert_eq!(
            permissions,
            Some("camera=(), microphone=(), geolocation=()")
        );
        // The test configuration is plain HTTP: no HSTS.
        assert!(
            headers.get(header::STRICT_TRANSPORT_SECURITY).is_none(),
            "{uri} must not advertise HSTS over http"
        );
    }
}

#[tokio::test]
async fn the_headers_are_also_on_error_responses() {
    let harness = Harness::new();
    let (status, headers, _) = harness.get("/api/unknown", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(header_value(&headers, header::CONTENT_SECURITY_POLICY).is_some());
    assert_eq!(
        header_value(&headers, header::X_FRAME_OPTIONS),
        Some("DENY")
    );
}

#[tokio::test]
async fn hsts_is_sent_only_behind_https() {
    let harness = Harness::with_config(|cfg| {
        cfg.public_url = "https://fleett.example.test".to_string();
    });
    let (status, headers, _) = harness.get("/api/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_value(&headers, header::STRICT_TRANSPORT_SECURITY),
        Some("max-age=31536000; includeSubDomains")
    );
}

// ---------------------------------------------------------------------------
// GET /api/security/status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn security_status_reports_the_documented_shape() {
    let harness = Harness::with_hardening(|cfg| {
        cfg.public_url = "https://fleett.example.test".to_string();
    });

    let (status, _, body) = harness.get("/api/security/status", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value = parse(&body);

    assert_eq!(value["rate_limit"]["enabled"], true);
    assert_eq!(
        value["rate_limit"]["limits"],
        json!({ "per_min": 120, "auth_per_min": 10, "heavy_per_min": 30 })
    );
    assert_eq!(
        value["lockout"],
        json!({ "failures": 10, "window_s": 600, "lockout_s": 900, "locked_ips": 0 })
    );
    let headers = &value["headers"];
    assert_eq!(
        headers["csp"].as_str().expect("csp"),
        CSP,
        "the policy is reported verbatim"
    );
    assert_eq!(headers["x_frame_options"], "DENY");
    assert_eq!(headers["referrer_policy"], "no-referrer");
    assert_eq!(
        headers["permissions_policy"],
        "camera=(), microphone=(), geolocation=()"
    );
    assert_eq!(headers["hsts"], true);
    assert_eq!(
        value["tokens"],
        json!({ "expired_or_revoked": 0, "active": 0 })
    );
    assert_eq!(value["auth_mode"], "bearer");
    // No secret ever appears in the payload.
    assert!(!body.contains(TOKEN), "{body}");
}

#[tokio::test]
async fn security_status_is_admin_only_and_counts_tokens() {
    let harness = Harness::with_hardening(|_| {});
    let (id, viewer_token) = harness.create_token("viewer").await;

    let (status, _, body) = harness
        .get("/api/security/status", Some(&viewer_token))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "forbidden");

    let (status, _, body) = harness.get("/api/security/status", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        parse(&body)["tokens"],
        json!({ "expired_or_revoked": 0, "active": 1 })
    );

    let (status, _, _) = harness
        .send(
            Method::DELETE,
            &format!("/api/tokens/{id}"),
            Some(TOKEN),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, body) = harness.get("/api/security/status", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        parse(&body)["tokens"],
        json!({ "expired_or_revoked": 1, "active": 0 })
    );
}

#[tokio::test]
async fn security_status_reports_a_fully_disabled_posture() {
    let harness = Harness::new();
    let (status, _, body) = harness.get("/api/security/status", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value = parse(&body);
    assert_eq!(value["rate_limit"]["enabled"], false);
    assert_eq!(value["lockout"]["failures"], 0);
    assert_eq!(value["headers"]["hsts"], false);
}

// Keep the harness alive for the whole test; it owns the temporary root.
impl Harness {
    #[allow(dead_code)]
    fn root(&self) -> &std::path::Path {
        self.dir.path()
    }
}
