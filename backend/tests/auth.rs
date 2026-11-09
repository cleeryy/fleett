//! Integration tests for authentication, roles, CSRF and audit.
//!
//! No identity provider is ever contacted: the OIDC verifier and the token
//! exchange are fakes, and the endpoint cache is seeded with local URLs.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::auth::oidc::{
    BoxFuture, CodeExchange, IdTokenClaims, OidcEndpoints, OidcError, TokenExpectations,
    TokenResponse, TokenVerifier,
};
use fleett::auth::session::{CSRF_COOKIE, CSRF_HEADER, OIDC_STATE_COOKIE, SESSION_COOKIE};
use fleett::auth::{AuthService, OidcParts};
use fleett::config::{AuthMode, Config, OidcConfig, OidcSettings, DEFAULT_OIDC_STATE_TTL_SECS};
use fleett::db::Db;
use fleett::driver::mock::MockDriver;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const ADMIN_GROUP: &str = "fleett-admins";
const VIEWER_GROUP: &str = "fleett-viewers";
const SECRET_KEY_BYTES: [u8; 32] = [7u8; 32];
const SESSION_SECRET: [u8; 32] = [42u8; 32];

/// ID token returned by the fake exchange.
const FAKE_ID_TOKEN: &str = "fake-id-token";

struct FakeVerifier {
    claims: IdTokenClaims,
}

impl TokenVerifier for FakeVerifier {
    fn verify<'a>(
        &'a self,
        token: &'a str,
        expected: &'a TokenExpectations,
    ) -> BoxFuture<'a, Result<IdTokenClaims, OidcError>> {
        Box::pin(async move {
            if token != FAKE_ID_TOKEN {
                return Err(OidcError::InvalidToken("unexpected token".into()));
            }
            assert_eq!(expected.client_id, "fleett");
            assert_eq!(expected.issuer, "https://idp.test");
            // The real verifier validates the signature, issuer and audience;
            // here we only mirror the nonce it would return.
            let mut claims = self.claims.clone();
            claims.nonce = Some(expected.nonce.clone());
            Ok(claims)
        })
    }
}

struct FakeExchange;

impl CodeExchange for FakeExchange {
    fn exchange<'a>(
        &'a self,
        code: &'a str,
        verifier: &'a str,
    ) -> BoxFuture<'a, Result<TokenResponse, OidcError>> {
        Box::pin(async move {
            if code != "the-code" || verifier.is_empty() {
                return Err(OidcError::Exchange("unexpected code".into()));
            }
            Ok(TokenResponse {
                id_token: FAKE_ID_TOKEN.to_string(),
                access_token: None,
                token_type: Some("Bearer".into()),
            })
        })
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    state: AppState,
}

impl Harness {
    /// Default bearer mode, exactly as before this feature.
    fn bearer() -> Self {
        Self::build(AuthMode::Bearer, None, &[])
    }

    /// OIDC-enabled harness (`both`) with a fake verifier for `groups`.
    fn oidc(groups: &[&str]) -> Self {
        Self::oidc_with_state_ttl(groups, DEFAULT_OIDC_STATE_TTL_SECS)
    }

    /// Same as [`Harness::oidc`], with an explicit state-cookie lifetime.
    fn oidc_with_state_ttl(groups: &[&str], state_ttl_secs: u64) -> Self {
        let claims = IdTokenClaims {
            sub: "d80c090d-0000-4000-8000-000000000001".to_string(),
            email: Some("cleeryy@example.test".to_string()),
            name: Some("Cleeryy".to_string()),
            preferred_username: Some("cleeryy".to_string()),
            groups: groups.iter().map(|group| group.to_string()).collect(),
            nonce: None,
            iss: "https://idp.test".to_string(),
        };
        let parts = OidcParts {
            endpoints: Some(OidcEndpoints::for_tests("https://idp.test")),
            verifier: Some(Arc::new(FakeVerifier { claims })),
            exchange: Some(Arc::new(FakeExchange)),
        };
        Self::build_with_state_ttl(AuthMode::Both, Some(parts), state_ttl_secs)
    }

    fn build(mode: AuthMode, parts: Option<OidcParts>, _groups: &[&str]) -> Self {
        Self::build_with_state_ttl(mode, parts, DEFAULT_OIDC_STATE_TTL_SECS)
    }

    fn build_with_state_ttl(mode: AuthMode, parts: Option<OidcParts>, state_ttl_secs: u64) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.auth = mode;
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRET_KEY_BYTES));
        cfg.oidc = mode.oidc_enabled().then(|| OidcConfig {
            public_url: "https://fleett.test".to_string(),
            settings: OidcSettings {
                issuer: "https://idp.test".to_string(),
                client_id: "fleett".to_string(),
                client_secret: "client-secret".to_string(),
                redirect_url: "https://fleett.test/auth/callback".to_string(),
                scopes: "openid profile email".to_string(),
                admin_group: ADMIN_GROUP.to_string(),
                viewer_group: VIEWER_GROUP.to_string(),
            },
            session_secret: SESSION_SECRET.to_vec(),
            session_ttl_secs: 3600,
            state_ttl_secs,
            cookie_secure: true,
        });

        let auth = AuthService::from_config_with(&cfg, parts.unwrap_or_default());
        let db = Db::open(&cfg.db_path()).expect("open db");
        let state =
            AppState::bootstrap_with_db_and_auth(cfg, Arc::new(MockDriver::new()), db, auth)
                .expect("bootstrap");
        Self { _dir: dir, state }
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        bearer: Option<&str>,
        cookies: &BTreeMap<String, String>,
        extra: &[(&str, String)],
    ) -> (StatusCode, HeaderMap, String) {
        let router = api::router(self.state.clone());
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let cookie_header = cookies
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        if !cookie_header.is_empty() {
            builder = builder.header(header::COOKIE, cookie_header);
        }
        for (name, value) in extra {
            builder = builder.header(*name, value.as_str());
        }
        let body = match body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = router
            .oneshot(builder.body(body).expect("valid request"))
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

    async fn get(&self, uri: &str, bearer: Option<&str>) -> (StatusCode, String) {
        let (status, _, body) = self
            .send(Method::GET, uri, None, bearer, &BTreeMap::new(), &[])
            .await;
        (status, body)
    }

    /// Run `/auth/login`, returning the state cookie and the `state` param.
    async fn start_login(&self) -> (String, String) {
        let (status, headers, _) = self
            .send(
                Method::GET,
                "/auth/login",
                None,
                None,
                &BTreeMap::new(),
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "login must redirect");
        let location = headers
            .get(header::LOCATION)
            .expect("Location")
            .to_str()
            .expect("ascii")
            .to_string();
        assert!(
            location.starts_with("https://idp.test/authorize?"),
            "{location}"
        );
        let parsed = url::Url::parse(&location).expect("valid authorization url");
        let params: BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert!(params.contains_key("nonce"));
        let returned_state = params.get("state").expect("state").clone();

        let cookies = set_cookies(&headers);
        let state_cookie = cookies
            .get(OIDC_STATE_COOKIE)
            .expect("state cookie")
            .clone();
        assert!(!state_cookie.is_empty());
        (state_cookie, returned_state)
    }

    /// Complete the login, returning the session and CSRF cookies.
    async fn sign_in(&self) -> BTreeMap<String, String> {
        let (state_cookie, returned_state) = self.start_login().await;
        let mut cookies = BTreeMap::new();
        cookies.insert(OIDC_STATE_COOKIE.to_string(), state_cookie);
        let (status, headers, body) = self
            .send(
                Method::GET,
                &format!("/auth/callback?code=the-code&state={returned_state}"),
                None,
                None,
                &cookies,
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "callback failed: {body}");
        let session = set_cookies(&headers);
        let session_cookie = session.get(SESSION_COOKIE).expect("session cookie").clone();
        let csrf_cookie = session.get(CSRF_COOKIE).expect("csrf cookie").clone();
        assert!(!session_cookie.is_empty());
        assert_eq!(csrf_cookie.len(), 32);
        let mut out = BTreeMap::new();
        out.insert(SESSION_COOKIE.to_string(), session_cookie);
        out.insert(CSRF_COOKIE.to_string(), csrf_cookie);
        out
    }
}

fn set_cookies(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for value in headers.get_all(header::SET_COOKIE) {
        let raw = value.to_str().expect("ascii cookie");
        let pair = raw.split(';').next().expect("pair");
        if let Some((name, value)) = pair.split_once('=') {
            out.insert(name.trim().to_string(), value.trim().to_string());
        }
    }
    out
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

/// The `Accept` a browser sends for a top-level navigation.
const BROWSER_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

fn accept(value: &str) -> Vec<(&'static str, String)> {
    vec![("accept", value.to_string())]
}

fn state_reason(body: &str) -> String {
    parse(body)["error"]["details"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn error_code(body: &str) -> String {
    parse(body)["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn instance_body(id: &str) -> Value {
    json!({
        "id": id,
        "template": "guest",
        "model": "deepseek-v4.1-flash",
        "base_url": "https://opencode.ai/inference/go/openai/v1",
        "channel": {"type": "none"},
        "mem_limit_mb": 512,
    })
}

fn csrf(cookies: &BTreeMap<String, String>) -> Vec<(&'static str, String)> {
    vec![(
        CSRF_HEADER,
        cookies.get(CSRF_COOKIE).cloned().unwrap_or_default(),
    )]
}

// ---------------------------------------------------------------------------
// /api/auth/config, /api/auth/me and the login flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_config_is_public_and_describes_the_mode() {
    let bearer = Harness::bearer();
    let (status, body) = bearer.get("/api/auth/config", None).await;
    assert_eq!(status, StatusCode::OK);
    let config = parse(&body);
    assert_eq!(config["mode"], "bearer");
    assert_eq!(config["oidc"]["enabled"], false);
    assert_eq!(config["oidc"]["login_url"], "/auth/login");
    assert_eq!(config["bearer"]["enabled"], true);

    let oidc = Harness::oidc(&[ADMIN_GROUP]);
    let (status, body) = oidc.get("/api/auth/config", None).await;
    assert_eq!(status, StatusCode::OK);
    let config = parse(&body);
    assert_eq!(config["mode"], "both");
    assert_eq!(config["oidc"]["enabled"], true);
    assert_eq!(config["bearer"]["enabled"], true);
}

#[tokio::test]
async fn unauthenticated_api_calls_return_401_json_and_never_redirect() {
    for harness in [Harness::bearer(), Harness::oidc(&[ADMIN_GROUP])] {
        let (status, headers, body) = harness
            .send(
                Method::GET,
                "/api/instances",
                None,
                None,
                &BTreeMap::new(),
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert!(
            headers.get(header::LOCATION).is_none(),
            "an XHR must never be redirected to the IdP"
        );
        assert_eq!(error_code(&body), "unauthorized");
    }
}

#[tokio::test]
async fn login_is_404_when_oidc_is_disabled() {
    let harness = Harness::bearer();
    let (status, body) = harness.get("/auth/login", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");
}

#[tokio::test]
async fn login_sets_a_signed_state_cookie_and_redirects_with_pkce() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let (state_cookie, _) = harness.start_login().await;
    // The state cookie is a signed value, not a bare token.
    assert!(state_cookie.starts_with("v1."), "{state_cookie}");
}

#[tokio::test]
async fn callback_rejects_a_state_that_does_not_match_the_cookie() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let (state_cookie, _) = harness.start_login().await;
    let mut cookies = BTreeMap::new();
    cookies.insert(OIDC_STATE_COOKIE.to_string(), state_cookie);
    let (status, headers, body) = harness
        .send(
            Method::GET,
            "/auth/callback?code=the-code&state=forged",
            None,
            None,
            &cookies,
            &accept("application/json"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_state");
    assert_eq!(state_reason(&body), "state_mismatch");
    assert!(
        headers.get(header::LOCATION).is_none(),
        "a JSON client is never redirected"
    );
}

#[tokio::test]
async fn callback_without_the_state_cookie_is_cookie_missing_for_json_clients() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let (status, headers, body) = harness
        .send(
            Method::GET,
            "/auth/callback?code=the-code&state=anything",
            None,
            None,
            &BTreeMap::new(),
            &accept("application/json"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_state");
    assert_eq!(state_reason(&body), "cookie_missing");
    assert!(parse(&body)["error"]["message"]
        .as_str()
        .expect("message")
        .contains("missing"));
    assert!(
        headers.get(header::LOCATION).is_none(),
        "a JSON client is never redirected"
    );

    // No `Accept` header at all is the API default: JSON, no redirect.
    let (status, headers, body) = harness
        .send(
            Method::GET,
            "/auth/callback?code=the-code&state=anything",
            None,
            None,
            &BTreeMap::new(),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(state_reason(&body), "cookie_missing");
    assert!(headers.get(header::LOCATION).is_none());
}

#[tokio::test]
async fn a_tampered_state_cookie_is_a_state_mismatch() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let mut cookies = BTreeMap::new();
    cookies.insert(OIDC_STATE_COOKIE.to_string(), "v1.abc.def".to_string());
    let (status, _, body) = harness
        .send(
            Method::GET,
            "/auth/callback?code=the-code&state=anything",
            None,
            None,
            &cookies,
            &accept("application/json"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_state");
    assert_eq!(state_reason(&body), "state_mismatch");
}

#[tokio::test]
async fn a_browser_without_the_state_cookie_is_redirected_to_the_login_page() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let (status, headers, _) = harness
        .send(
            Method::GET,
            "/auth/callback?code=the-code&state=anything",
            None,
            None,
            &BTreeMap::new(),
            &accept(BROWSER_ACCEPT),
        )
        .await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(
        headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/auth/login")
    );
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
}

#[tokio::test]
async fn a_browser_with_a_mismatched_state_gets_a_self_contained_html_page() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let (state_cookie, _) = harness.start_login().await;
    let mut cookies = BTreeMap::new();
    cookies.insert(OIDC_STATE_COOKIE.to_string(), state_cookie);
    let (status, headers, body) = harness
        .send(
            Method::GET,
            "/auth/callback?code=the-code&state=forged",
            None,
            None,
            &cookies,
            &accept(BROWSER_ACCEPT),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert!(body.contains("<title>Sign-in failed"), "{body}");
    assert!(body.contains("another browser"), "{body}");
    assert!(body.contains("href=\"/auth/login\""), "{body}");
    assert!(body.contains("Sign in again"), "{body}");
    assert!(!body.contains("state="), "the page never echoes the query");
}

#[tokio::test]
async fn an_expired_state_cookie_is_reported_as_expired() {
    // A one-second TTL keeps the test quick while exercising the real clock.
    let harness = Harness::oidc_with_state_ttl(&[ADMIN_GROUP], 1);
    let (state_cookie, returned_state) = harness.start_login().await;
    let mut cookies = BTreeMap::new();
    cookies.insert(OIDC_STATE_COOKIE.to_string(), state_cookie);
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let uri = format!("/auth/callback?code=the-code&state={returned_state}");
    let (status, _, body) = harness
        .send(
            Method::GET,
            &uri,
            None,
            None,
            &cookies,
            &accept("application/json"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_state");
    assert_eq!(state_reason(&body), "expired");
    let message = parse(&body)["error"]["message"]
        .as_str()
        .expect("message")
        .to_string();
    assert!(message.contains("expired"), "{message}");

    // The same failure in a browser is a page, still with a way back in.
    let (status, headers, body) = harness
        .send(
            Method::GET,
            &uri,
            None,
            None,
            &cookies,
            &accept(BROWSER_ACCEPT),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert!(body.contains("login link has expired"), "{body}");
    assert!(body.contains("href=\"/auth/login\""), "{body}");
}

#[tokio::test]
async fn login_honours_the_configured_state_ttl() {
    let harness = Harness::oidc_with_state_ttl(&[ADMIN_GROUP], 120);
    let (status, headers, _) = harness
        .send(
            Method::GET,
            "/auth/login",
            None,
            None,
            &BTreeMap::new(),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let state_cookie = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|cookie| cookie.starts_with(&format!("{OIDC_STATE_COOKIE}=")))
        .expect("state cookie")
        .to_string();
    assert!(state_cookie.contains("Max-Age=120"), "{state_cookie}");
    assert!(!state_cookie.contains("Max-Age=600"), "{state_cookie}");
}

#[tokio::test]
async fn the_full_flow_mints_a_session_and_me_returns_the_admin_role() {
    let harness = Harness::oidc(&["other", ADMIN_GROUP]);
    let cookies = harness.sign_in().await;

    let (status, _, body) = harness
        .send(Method::GET, "/api/auth/me", None, None, &cookies, &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    let me = parse(&body);
    assert_eq!(me["authenticated"], true);
    assert_eq!(me["role"], "admin");
    assert_eq!(me["method"], "session");
    assert_eq!(me["actor"], "cleeryy@example.test");
    assert_eq!(me["user"]["sub"], "d80c090d-0000-4000-8000-000000000001");
    assert_eq!(me["user"]["email"], "cleeryy@example.test");
    assert_eq!(me["user"]["name"], "Cleeryy");

    // The session is also accepted by /api/auth/check.
    let (status, _, _) = harness
        .send(Method::GET, "/api/auth/check", None, None, &cookies, &[])
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_group_outside_both_roles_is_forbidden() {
    let harness = Harness::oidc(&["some-other-group"]);
    let (state_cookie, returned_state) = harness.start_login().await;
    let mut cookies = BTreeMap::new();
    cookies.insert(OIDC_STATE_COOKIE.to_string(), state_cookie);
    let (status, _, body) = harness
        .send(
            Method::GET,
            &format!("/auth/callback?code=the-code&state={returned_state}"),
            None,
            None,
            &cookies,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "forbidden");
}

#[tokio::test]
async fn a_tampered_session_cookie_is_unauthorized() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let cookies = harness.sign_in().await;
    let session = cookies.get(SESSION_COOKIE).expect("session").clone();
    let mut tampered = session.clone();
    tampered.push('x');
    let mut bad = BTreeMap::new();
    bad.insert(SESSION_COOKIE.to_string(), tampered);
    let (status, _, _) = harness
        .send(Method::GET, "/api/instances", None, None, &bad, &[])
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_clears_every_cookie() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let (status, headers, _) = harness
        .send(
            Method::POST,
            "/auth/logout",
            None,
            None,
            &BTreeMap::new(),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let cookies = set_cookies(&headers);
    for name in [SESSION_COOKIE, CSRF_COOKIE, OIDC_STATE_COOKIE] {
        assert_eq!(cookies.get(name).map(String::as_str), Some(""), "{name}");
    }
    let raw: Vec<&str> = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    assert!(raw.iter().all(|cookie| cookie.contains("Max-Age=0")));
}

// ---------------------------------------------------------------------------
// Roles, CSRF and audit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_viewer_reads_but_cannot_mutate() {
    let harness = Harness::oidc(&[VIEWER_GROUP]);
    let cookies = harness.sign_in().await;

    let (status, _, body) = harness
        .send(Method::GET, "/api/auth/me", None, None, &cookies, &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse(&body)["role"], "viewer");

    let (status, _, _) = harness
        .send(Method::GET, "/api/instances", None, None, &cookies, &[])
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, body) = harness
        .send(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "findings"})),
            None,
            &cookies,
            &csrf(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "forbidden");

    // Reads of the audit log are admin-only, even for a GET.
    let (status, _, body) = harness
        .send(Method::GET, "/api/audit", None, None, &cookies, &[])
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "forbidden");
}

#[tokio::test]
async fn cookie_mutations_require_the_csrf_header() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let cookies = harness.sign_in().await;

    let (status, _, body) = harness
        .send(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "findings"})),
            None,
            &cookies,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "csrf_required");

    // A wrong value is just as invalid as a missing header.
    let (status, _, body) = harness
        .send(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "findings"})),
            None,
            &cookies,
            &[("x-fleett-csrf", "deadbeef".to_string())],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "csrf_required");

    let (status, _, body) = harness
        .send(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "findings"})),
            None,
            &cookies,
            &csrf(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn admin_mutations_succeed_and_are_audited() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let cookies = harness.sign_in().await;

    let (status, _, body) = harness
        .send(
            Method::POST,
            "/api/instances",
            Some(instance_body("alice")),
            None,
            &cookies,
            &csrf(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, _, body) = harness
        .send(
            Method::POST,
            "/api/instances/alice/start",
            None,
            None,
            &cookies,
            &csrf(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _, body) = harness
        .send(
            Method::GET,
            "/api/audit?limit=50",
            None,
            None,
            &cookies,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    let entries = entries.as_array().expect("array");
    assert!(!entries.is_empty());
    let actions: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry["action"].as_str())
        .collect();
    assert!(actions.contains(&"instance.create"), "{actions:?}");
    assert!(actions.contains(&"instance.start"), "{actions:?}");
    let create = entries
        .iter()
        .find(|entry| entry["action"] == "instance.create")
        .expect("create entry");
    assert_eq!(create["target"], "alice");
    assert_eq!(create["actor"], "cleeryy@example.test");
    assert_eq!(create["role"], "admin");
    assert_eq!(create["method"], "session");
    assert_eq!(create["details"]["name"], "alice");
}

#[tokio::test]
async fn bearer_mutations_need_no_csrf_and_are_audited() {
    let harness = Harness::bearer();
    let (status, _, body) = harness
        .send(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "artifacts"})),
            Some(TOKEN),
            &BTreeMap::new(),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = harness.get("/api/audit", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let entries = parse(&body);
    let entry = entries
        .as_array()
        .expect("array")
        .iter()
        .find(|entry| entry["action"] == "share.create")
        .expect("audited");
    assert_eq!(entry["actor"], "bootstrap");
    assert_eq!(entry["role"], "admin");
    assert_eq!(entry["method"], "bearer");
}

#[tokio::test]
async fn logins_logouts_and_refusals_are_audited() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let cookies = harness.sign_in().await;

    let (status, body) = harness
        .get("/api/audit?action=auth.login", Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    let entry = entries.as_array().expect("array")[0].clone();
    assert_eq!(entry["actor"], "cleeryy@example.test");
    assert_eq!(entry["role"], "admin");
    assert_eq!(entry["method"], "session");
    assert_eq!(entry["result"], "ok");
    assert_eq!(entry["details"]["provider"], "https://idp.test");

    // A callback with no state cookie is a failed login, recorded as such.
    let (status, _, body) = harness
        .send(
            Method::GET,
            "/auth/callback?code=the-code&state=forged",
            None,
            None,
            &BTreeMap::new(),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = harness
        .get("/api/audit?action=auth.login&result=failed", Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    let entry = entries.as_array().expect("array")[0].clone();
    assert_eq!(entry["actor"], "anonymous");
    assert_eq!(entry["result"], "failed");
    assert_eq!(entry["details"]["reason"], "cookie_missing");

    // Logout identifies the session before the cookie is cleared.
    let (status, _, body) = harness
        .send(Method::POST, "/auth/logout", None, None, &cookies, &[])
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body) = harness
        .get("/api/audit?action=auth.logout", Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    let entry = entries.as_array().expect("array")[0].clone();
    assert_eq!(entry["actor"], "cleeryy@example.test");
    assert_eq!(entry["result"], "ok");
}

#[tokio::test]
async fn refused_mutations_are_audited_as_refused() {
    // A viewer attempting a mutation.
    let viewer = Harness::oidc(&[VIEWER_GROUP]);
    let cookies = viewer.sign_in().await;
    let (status, _, _) = viewer
        .send(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "findings"})),
            None,
            &cookies,
            &csrf(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = viewer
        .get("/api/audit?action=auth.mutation_refused", Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    let entry = entries.as_array().expect("array")[0].clone();
    assert_eq!(entry["result"], "refused");
    assert_eq!(entry["details"]["reason"], "viewer_read_only");

    // An admin cookie missing the CSRF header.
    let admin = Harness::oidc(&[ADMIN_GROUP]);
    let cookies = admin.sign_in().await;
    let (status, _, _) = admin
        .send(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "findings"})),
            None,
            &cookies,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = admin
        .get("/api/audit?action=auth.mutation_refused", Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    let entry = entries.as_array().expect("array")[0].clone();
    assert_eq!(entry["result"], "refused");
    assert_eq!(entry["details"]["reason"], "csrf_required");
}

// ---------------------------------------------------------------------------
// Named API tokens
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tokens_are_returned_once_listed_without_the_secret_and_revocable() {
    let harness = Harness::oidc(&[ADMIN_GROUP]);
    let cookies = harness.sign_in().await;

    let (status, _, body) = harness
        .send(
            Method::POST,
            "/api/tokens",
            Some(json!({"name": "ci", "role": "viewer"})),
            None,
            &cookies,
            &csrf(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created = parse(&body);
    let secret = created["token"].as_str().expect("secret").to_string();
    assert!(secret.starts_with("flt_"));
    assert_eq!(secret.len(), 4 + 32);
    assert_eq!(created["role"], "viewer");
    let id = created["id"].as_str().expect("id").to_string();

    // Only the digest is stored; the secret itself is not a lookup key.
    assert!(harness
        .state
        .db
        .active_token_by_hash(&fleett::auth::tokens::hash(&secret))
        .expect("lookup")
        .is_some());
    assert!(harness
        .state
        .db
        .active_token_by_hash(&secret)
        .expect("lookup")
        .is_none());

    // The list never contains the secret.
    let (status, _, body) = harness
        .send(Method::GET, "/api/tokens", None, None, &cookies, &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    let listed = parse(&body);
    let rows = listed.as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "ci");
    assert!(rows[0].get("token").is_none());
    assert!(!body.contains(&secret));

    // A viewer token can read...
    let (status, _) = harness.get("/api/instances", Some(&secret)).await;
    assert_eq!(status, StatusCode::OK);

    // ...but not mutate, even without CSRF.
    let (status, _, body) = harness
        .send(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "nope"})),
            Some(&secret),
            &BTreeMap::new(),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Revoke it; the secret stops working immediately.
    let (status, _, body) = harness
        .send(
            Method::DELETE,
            &format!("/api/tokens/{id}"),
            None,
            None,
            &cookies,
            &csrf(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, _) = harness.get("/api/instances", Some(&secret)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Revoking twice is a no-op, an unknown id is a 404.
    let (status, _, _) = harness
        .send(
            Method::DELETE,
            &format!("/api/tokens/{id}"),
            None,
            None,
            &cookies,
            &csrf(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = harness
        .send(
            Method::DELETE,
            "/api/tokens/unknown",
            None,
            None,
            &cookies,
            &csrf(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn token_endpoints_are_admin_only() {
    let harness = Harness::oidc(&[VIEWER_GROUP]);
    let cookies = harness.sign_in().await;
    let (status, _, body) = harness
        .send(
            Method::POST,
            "/api/tokens",
            Some(json!({"name": "ci", "role": "viewer"})),
            None,
            &cookies,
            &csrf(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "forbidden");

    let (status, _, _) = harness
        .send(Method::GET, "/api/tokens", None, None, &cookies, &[])
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Bearer mode is unchanged
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bearer_mode_still_authenticates_like_before() {
    let harness = Harness::bearer();

    let (status, _) = harness.get("/api/templates", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = harness.get("/api/templates", Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = harness.get("/api/templates", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK);

    // Bearer identity is the bootstrap token.
    let (status, body) = harness.get("/api/auth/me", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let me = parse(&body);
    assert_eq!(me["authenticated"], true);
    assert_eq!(me["actor"], "bootstrap");
    assert_eq!(me["role"], "admin");
    assert_eq!(me["method"], "bearer");
    assert!(me.get("user").is_none());

    // Mutations work without any CSRF header in bearer mode.
    let (status, _, _) = harness
        .send(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "legacy"})),
            Some(TOKEN),
            &BTreeMap::new(),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn oidc_only_mode_disables_the_bearer_token() {
    let harness = Harness::build(AuthMode::Oidc, None, &[]);

    let (status, body) = harness.get("/api/auth/config", None).await;
    assert_eq!(status, StatusCode::OK);
    let config = parse(&body);
    assert_eq!(config["mode"], "oidc");
    assert_eq!(config["bearer"]["enabled"], false);
    assert_eq!(config["oidc"]["enabled"], true);

    // Even the bootstrap token is refused: `oidc` means sessions only.
    let (status, _) = harness.get("/api/instances", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_cookies_are_ignored_in_bearer_mode() {
    // A session signed with the OIDC key must not authenticate when the mode
    // is `bearer` (no session signer is configured at all there).
    let harness = Harness::bearer();
    let mut cookies = BTreeMap::new();
    cookies.insert(SESSION_COOKIE.to_string(), "v1.abc.def".to_string());
    let (status, _, _) = harness
        .send(Method::GET, "/api/instances", None, None, &cookies, &[])
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
