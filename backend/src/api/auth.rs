//! Authentication endpoints and the middleware guarding every API route.
//!
//! The session cookie is checked first, then the bearer token. In `both` mode
//! an invalid expired session does not block a valid bearer header, which is
//! what automation wants when a browser cookie happens to be in the jar.

use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::oidc::pkce_challenge;
use crate::auth::session::{
    OidcState, SessionClaims, CSRF_COOKIE, CSRF_HEADER, OIDC_STATE_COOKIE, SESSION_COOKIE,
};
use crate::auth::tokens;
use crate::auth::{random_hex, AuthContext, AuthMethod, Role, SessionUser};
use axum::extract::{ConnectInfo, Extension, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use subtle::ConstantTimeEq;

/// Lifetime of the state/nonce cookie, in seconds.
pub const OIDC_STATE_TTL_SECS: i64 = 600;

/// `GET /api/auth/check` — reaching this handler means the request is allowed.
pub async fn check() -> StatusCode {
    StatusCode::NO_CONTENT
}

#[derive(Debug, Serialize)]
pub struct AuthConfigResponse {
    pub mode: &'static str,
    pub oidc: OidcConfigPublic,
    pub bearer: BearerConfigPublic,
}

#[derive(Debug, Serialize)]
pub struct OidcConfigPublic {
    pub enabled: bool,
    pub login_url: &'static str,
}

#[derive(Debug, Serialize)]
pub struct BearerConfigPublic {
    pub enabled: bool,
}

/// `GET /api/auth/config` — public, so the console can pick a login method.
pub async fn config(State(state): State<AppState>) -> Json<AuthConfigResponse> {
    Json(AuthConfigResponse {
        mode: state.auth.mode.as_str(),
        oidc: OidcConfigPublic {
            enabled: state.auth.oidc_enabled(),
            login_url: "/auth/login",
        },
        bearer: BearerConfigPublic {
            enabled: state.auth.bearer_enabled(),
        },
    })
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub authenticated: bool,
    pub actor: String,
    pub role: &'static str,
    pub method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<SessionUser>,
}

/// `GET /api/auth/me` — identity behind the current request.
pub async fn me(Extension(ctx): Extension<AuthContext>) -> Json<MeResponse> {
    Json(MeResponse {
        authenticated: true,
        actor: ctx.actor,
        role: ctx.role.as_str(),
        method: ctx.method.as_str(),
        user: ctx.user,
    })
}

/// `GET /auth/login` — start the authorization-code flow.
pub async fn login(State(state): State<AppState>) -> Response {
    match login_inner(&state).await {
        Ok(response) => response,
        Err(err) => err.into_response(),
    }
}

async fn login_inner(state: &AppState) -> Result<Response, ApiError> {
    let client = oidc_client(state)?;
    let now = Utc::now().timestamp();

    let state_value = random_hex(16);
    let nonce = random_hex(16);
    let verifier = crate::auth::oidc::pkce_verifier();
    let challenge = pkce_challenge(&verifier);

    let url = client
        .authorization_url(&state_value, &nonce, &challenge)
        .await
        .map_err(|err| oidc_unavailable(&err))?;

    let signer = state
        .auth
        .state_signer
        .as_ref()
        .ok_or_else(|| ApiError::not_found("OIDC is not enabled"))?;
    let cookie = signer.sign(&OidcState {
        state: state_value,
        nonce,
        verifier,
        exp: now + OIDC_STATE_TTL_SECS,
    });

    let mut response = Redirect::to(&url).into_response();
    append_cookie(
        response.headers_mut(),
        &build_cookie(
            OIDC_STATE_COOKIE,
            &cookie,
            OIDC_STATE_TTL_SECS,
            true,
            state.auth.cookie_secure,
        ),
    );
    // The console polls /api/auth/me after the redirect; never let a proxy
    // cache a login response.
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// `GET /auth/callback` — exchange the code and mint the session.
pub async fn callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
    headers: HeaderMap,
) -> Response {
    match callback_inner(&state, query, &headers).await {
        Ok(response) => response,
        Err(err) => err.into_response(),
    }
}

async fn callback_inner(
    state: &AppState,
    query: CallbackQuery,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let client = oidc_client(state)?;
    if let Some(error) = query.error.as_deref() {
        let detail = query
            .error_description
            .as_deref()
            .unwrap_or("the provider refused the login");
        return Err(ApiError::unauthorized_with(
            "oidc_denied",
            format!("OIDC login refused ({error}): {detail}"),
        ));
    }
    let code = query
        .code
        .as_deref()
        .filter(|code| !code.is_empty())
        .ok_or_else(|| ApiError::bad_request("invalid_request", "missing code"))?;
    let returned_state = query
        .state
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("invalid_request", "missing state"))?;

    let signer = state
        .auth
        .state_signer
        .as_ref()
        .ok_or_else(|| ApiError::not_found("OIDC is not enabled"))?;
    let raw_cookie = cookie_value(headers, OIDC_STATE_COOKIE).ok_or_else(|| {
        ApiError::bad_request("invalid_state", "the login state cookie is missing")
    })?;
    let now = Utc::now().timestamp();
    let stored = signer
        .verify(&raw_cookie, now)
        .map_err(|_| ApiError::bad_request("invalid_state", "the login state cookie is invalid"))?;
    if !constant_time_eq(stored.state.as_bytes(), returned_state.as_bytes()) {
        return Err(ApiError::bad_request(
            "invalid_state",
            "the state does not match the login request",
        ));
    }

    let claims = client
        .complete(code, &stored.verifier, &stored.nonce)
        .await
        .map_err(|err| oidc_failed(&err))?;

    let role = client.role(&claims.groups).ok_or_else(|| {
        ApiError::forbidden(
            "forbidden",
            format!(
                "the account is in neither the \"{}\" nor the \"{}\" group",
                client.settings.admin_group, client.settings.viewer_group
            ),
        )
    })?;

    let session_signer = state
        .auth
        .session
        .as_ref()
        .ok_or_else(|| ApiError::not_found("OIDC is not enabled"))?;
    let issued_at = Utc::now().timestamp();
    let session = session_signer.sign(&SessionClaims {
        sub: claims.sub.clone(),
        email: claims.email.clone(),
        name: claims.display_name().map(str::to_string),
        role,
        iat: issued_at,
        exp: issued_at + state.auth.session_ttl_secs as i64,
    });
    let csrf = random_hex(16);

    let mut response = Redirect::to("/").into_response();
    let headers_mut = response.headers_mut();
    append_cookie(
        headers_mut,
        &build_cookie(
            SESSION_COOKIE,
            &session,
            state.auth.session_ttl_secs as i64,
            true,
            state.auth.cookie_secure,
        ),
    );
    append_cookie(
        headers_mut,
        &build_cookie(
            CSRF_COOKIE,
            &csrf,
            state.auth.session_ttl_secs as i64,
            false,
            state.auth.cookie_secure,
        ),
    );
    // The state cookie is single-use.
    append_cookie(
        headers_mut,
        &build_cookie(OIDC_STATE_COOKIE, "", 0, true, state.auth.cookie_secure),
    );
    headers_mut.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// `POST /auth/logout` — clear every cookie. Idempotent and unauthenticated:
/// logging out someone who is not logged in is harmless.
pub async fn logout(State(state): State<AppState>) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    for name in [SESSION_COOKIE, CSRF_COOKIE, OIDC_STATE_COOKIE] {
        append_cookie(
            headers,
            &build_cookie(name, "", 0, true, state.auth.cookie_secure),
        );
    }
    response
}

/// Middleware guarding every `/api/*` route except health and auth config.
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let method = request.method().clone();
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let headers = request.headers();

    let session_cookie = cookie_value(headers, SESSION_COOKIE);
    let bearer = bearer_token(headers);
    let csrf_cookie = cookie_value(headers, CSRF_COOKIE);
    let csrf_header = headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let ip = client_ip(headers, peer);

    let mut context = None;

    if state.auth.oidc_enabled() {
        if let Some(value) = session_cookie.as_deref() {
            match state.auth.verify_session(value, Utc::now().timestamp()) {
                Ok(claims) => {
                    let user = claims.user();
                    context = Some(AuthContext {
                        actor: user.actor(),
                        role: claims.role,
                        method: AuthMethod::Session,
                        user: Some(user),
                        ip: None,
                    });
                }
                Err(err) => {
                    // Expired sessions are routine (a tab left open overnight).
                    tracing::debug!(%err, "rejected session cookie");
                }
            }
        }
    }

    if context.is_none() && state.auth.bearer_enabled() {
        if let Some(token) = bearer.as_deref() {
            if let Some((actor, role)) = bearer_identity(&state, token)? {
                context = Some(AuthContext {
                    actor,
                    role,
                    method: AuthMethod::Bearer,
                    user: None,
                    ip: None,
                });
            }
        }
    }

    if context.is_none() && state.auth.allow_no_auth {
        context = Some(AuthContext {
            actor: "anonymous".to_string(),
            role: Role::Admin,
            method: AuthMethod::Bearer,
            user: None,
            ip: None,
        });
    }

    let Some(mut context) = context else {
        return Err(ApiError::unauthorized());
    };
    context.ip = ip;

    if context.role == Role::Viewer && !is_read_method(&method) {
        return Err(ApiError::forbidden(
            "forbidden",
            "the viewer role is read-only",
        ));
    }

    // Bearer clients are not vulnerable to CSRF; cookies are, unless the
    // double-submit header echoes the cookie.
    if context.method == AuthMethod::Session && !is_read_method(&method) {
        let valid = match (csrf_cookie.as_deref(), csrf_header.as_deref()) {
            (Some(cookie), Some(header)) if !cookie.is_empty() => {
                constant_time_eq(cookie.as_bytes(), header.as_bytes())
            }
            _ => false,
        };
        if !valid {
            return Err(ApiError::forbidden(
                "csrf_required",
                format!("cookie-authenticated mutations require the {CSRF_HEADER} header"),
            ));
        }
    }

    request.extensions_mut().insert(context);
    Ok(next.run(request).await)
}

/// Resolve a bearer token to `(actor, role)`.
fn bearer_identity(state: &AppState, token: &str) -> Result<Option<(String, Role)>, ApiError> {
    if let Some(expected) = state.auth.bootstrap_token.as_deref() {
        if constant_time_eq(expected.as_bytes(), token.as_bytes()) {
            return Ok(Some(("bootstrap".to_string(), Role::Admin)));
        }
    }
    if !tokens::looks_like_token(token) {
        return Ok(None);
    }
    let digest = tokens::hash(token);
    let record = state
        .db
        .active_token_by_hash(&digest)
        .map_err(ApiError::from)?;
    Ok(record.map(|record| {
        let role = Role::parse(&record.role).unwrap_or(Role::Viewer);
        (record.name, role)
    }))
}

pub(crate) fn oidc_client(state: &AppState) -> Result<&crate::auth::oidc::OidcClient, ApiError> {
    if !state.auth.oidc_enabled() {
        return Err(ApiError::not_found("OIDC is not enabled"));
    }
    state
        .auth
        .oidc
        .as_deref()
        .ok_or_else(|| ApiError::not_found("OIDC is not enabled"))
}

fn oidc_unavailable(err: &crate::auth::oidc::OidcError) -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "oidc_unavailable",
        format!("the identity provider is unreachable: {err}"),
    )
}

fn oidc_failed(err: &crate::auth::oidc::OidcError) -> ApiError {
    ApiError::unauthorized_with("oidc_failed", format!("OIDC login failed: {err}"))
}

fn is_read_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.ct_eq(right).into()
}

/// First non-empty `Cookie:` header value with that name.
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(raw) = value.to_str() else {
            continue;
        };
        for part in raw.split(';') {
            let part = part.trim();
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            if key.trim() == name {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

/// Best-effort client address, for the audit log.
fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<String> {
    let forwarded_for = HeaderName::from_static("x-forwarded-for");
    if let Some(value) = headers.get(&forwarded_for).and_then(|v| v.to_str().ok()) {
        if let Some(first) = value.split(',').map(str::trim).find(|s| !s.is_empty()) {
            return Some(first.to_string());
        }
    }
    let real_ip = HeaderName::from_static("x-real-ip");
    if let Some(value) = headers.get(&real_ip).and_then(|v| v.to_str().ok()) {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    peer.map(|addr| addr.ip().to_string())
}

/// Serialize a `Set-Cookie` value. Values are hex/base64url, never escaped.
fn build_cookie(name: &str, value: &str, max_age: i64, http_only: bool, secure: bool) -> String {
    let mut cookie = format!("{name}={value}; Path=/; Max-Age={max_age}; SameSite=Lax");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn append_cookie(headers: &mut HeaderMap, cookie: &str) {
    if let Ok(value) = HeaderValue::from_str(cookie) {
        headers.append(header::SET_COOKIE, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bearer_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc"),
        );
        assert_eq!(bearer_token(&headers), Some("abc".into()));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bearer  abc "),
        );
        assert_eq!(bearer_token(&headers), Some("abc".into()));
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer_token(&headers), None);
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer "));
        assert_eq!(bearer_token(&headers), None);
    }

    #[test]
    fn constant_time_comparison_is_exact() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret "));
    }

    #[test]
    fn cookie_values_are_read_from_any_cookie_header() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("a=1; fleett_csrf=dead"),
        );
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("fleett_session=abc.def"),
        );
        assert_eq!(cookie_value(&headers, "fleett_csrf"), Some("dead".into()));
        assert_eq!(
            cookie_value(&headers, "fleett_session"),
            Some("abc.def".into())
        );
        assert_eq!(cookie_value(&headers, "absent"), None);
    }

    #[test]
    fn cookies_are_lax_httponly_and_secure_on_demand() {
        let cookie = build_cookie(SESSION_COOKIE, "value", 60, true, true);
        assert!(cookie.starts_with("fleett_session=value;"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("Max-Age=60"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));

        let csrf = build_cookie(CSRF_COOKIE, "abc", 60, false, false);
        assert!(!csrf.contains("HttpOnly"));
        assert!(!csrf.contains("Secure"));
    }

    #[test]
    fn client_ip_prefers_the_forwarded_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("203.0.113.7, 10.0.0.1"),
        );
        assert_eq!(client_ip(&headers, None).as_deref(), Some("203.0.113.7"));

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-real-ip"),
            HeaderValue::from_static("203.0.113.8"),
        );
        assert_eq!(client_ip(&headers, None).as_deref(), Some("203.0.113.8"));

        let peer: SocketAddr = "127.0.0.1:9000".parse().expect("addr");
        assert_eq!(
            client_ip(&HeaderMap::new(), Some(peer)).as_deref(),
            Some("127.0.0.1")
        );
    }
}
