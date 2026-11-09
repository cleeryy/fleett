//! Authentication endpoints and the middleware guarding every API route.
//!
//! The session cookie is checked first, then the bearer token. In `both` mode
//! an invalid expired session does not block a valid bearer header, which is
//! what automation wants when a browser cookie happens to be in the jar.

use crate::api::error::{truncate_chars, ApiError};
use crate::api::AppState;
use crate::auth::oidc::pkce_challenge;
use crate::auth::session::{
    OidcState, SessionClaims, SessionError, StateSigner, CSRF_COOKIE, CSRF_HEADER,
    OIDC_STATE_COOKIE, SESSION_COOKIE,
};
use crate::auth::tokens;
use crate::auth::{
    random_hex, AuthContext, AuthMethod, Role, SessionUser, TokenScope, MAX_USER_AGENT_CHARS,
};
use axum::extract::{ConnectInfo, Extension, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use subtle::ConstantTimeEq;
use tracing::warn;

/// Stable callback failure reasons, reported as `error.details.reason`.
/// The state cookie was never sent: the link was opened in another browser.
const REASON_COOKIE_MISSING: &str = "cookie_missing";
/// The state cookie is valid but does not describe this login attempt.
const REASON_STATE_MISMATCH: &str = "state_mismatch";
/// The state cookie outlived `FLEETT_OIDC_STATE_TTL_SECS`.
const REASON_EXPIRED: &str = "expired";

/// Longest `Host`/`User-Agent` fragment copied into the failure log line.
const MAX_LOG_HEADER_CHARS: usize = 200;

/// Self-contained failure page: no script, no external asset, one action.
const CALLBACK_ERROR_PAGE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sign-in failed · fleett</title>
<style>
:root { color-scheme: light dark; }
body {
  margin: 0;
  min-height: 100vh;
  display: grid;
  place-items: center;
  font: 16px/1.6 system-ui, -apple-system, "Segoe UI", sans-serif;
  background: #f6f7f8;
  color: #1c2126;
}
main { max-width: 34rem; padding: 2rem; }
h1 { margin: 0 0 0.75rem; font-size: 1.5rem; }
p.cause { margin: 0 0 1.5rem; }
a {
  display: inline-block;
  padding: 0.6rem 1rem;
  border-radius: 6px;
  background: #1c2126;
  color: #f6f7f8;
  text-decoration: none;
}
@media (prefers-color-scheme: dark) {
  body { background: #14181b; color: #e8ecef; }
  a { background: #e8ecef; color: #14181b; }
}
</style>
</head>
<body>
<main>
<h1>Sign-in failed</h1>
<p class="cause">{cause}</p>
<a href="/auth/login">Sign in again</a>
</main>
</body>
</html>
"#;

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
    /// Identity matched against `instance_acl.subject`.
    pub subject: String,
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
        subject: ctx.subject,
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
    // `FLEETT_OIDC_STATE_TTL_SECS` drives both the cookie `Max-Age` and the
    // signed `exp`, so the two can never disagree.
    let state_ttl_secs = state.auth.state_ttl_secs as i64;
    let cookie = signer.sign(&OidcState {
        state: state_value,
        nonce,
        verifier,
        exp: now + state_ttl_secs,
    });

    let mut response = Redirect::to(&url).into_response();
    append_cookie(
        response.headers_mut(),
        &build_cookie(
            OIDC_STATE_COOKIE,
            &cookie,
            state_ttl_secs,
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
        Err(failure) => failure.respond(&state, &headers),
    }
}

/// A refused callback request.
///
/// State failures carry one of the stable reasons `cookie_missing`,
/// `state_mismatch` or `expired` (returned as `error.details.reason`); every
/// other failure reuses its API error code as the reason.
#[derive(Debug)]
struct CallbackFailure {
    error: ApiError,
    reason: &'static str,
    /// Only a missing state cookie is the legitimate "link opened in another
    /// browser" case: a browser is sent back to the login page.
    redirect_to_login: bool,
}

impl CallbackFailure {
    fn state(reason: &'static str, message: &'static str) -> Self {
        Self {
            error: ApiError::bad_request("invalid_state", message)
                .with_details(serde_json::json!({ "reason": reason })),
            reason,
            redirect_to_login: reason == REASON_COOKIE_MISSING,
        }
    }

    /// Log one WARN line (reason, requested host, User-Agent — never the code,
    /// state, nonce or cookie), append a failed `auth.login` row and pick the
    /// representation the client asked for: HTML page or redirect for browsers,
    /// the unchanged JSON for APIs.
    fn respond(self, state: &AppState, headers: &HeaderMap) -> Response {
        warn!(
            reason = self.reason,
            host = %log_header(headers, header::HOST),
            user_agent = %log_header(headers, header::USER_AGENT),
            "OIDC callback rejected"
        );

        crate::api::audit::record_entry(
            state,
            crate::db::NewAuditRecord {
                at: crate::models::now_iso(),
                actor: "anonymous".to_string(),
                role: "none".to_string(),
                method: "session".to_string(),
                action: "auth.login".to_string(),
                target: "oidc".to_string(),
                ip: client_ip(headers, None),
                user_agent: bounded_user_agent(headers),
                result: "failed".to_string(),
                request_id: None,
                details: Some(
                    serde_json::json!({
                        "reason": self.reason,
                        "code": self.error.code,
                    })
                    .to_string(),
                ),
            },
        );

        if !wants_html(headers) {
            return self.error.into_response();
        }
        if self.redirect_to_login {
            return redirect_to_login();
        }
        html_failure_page(self.reason)
    }
}

impl From<ApiError> for CallbackFailure {
    fn from(error: ApiError) -> Self {
        Self {
            reason: error.code,
            error,
            redirect_to_login: false,
        }
    }
}

async fn callback_inner(
    state: &AppState,
    query: CallbackQuery,
    headers: &HeaderMap,
) -> Result<Response, CallbackFailure> {
    let client = oidc_client(state)?;
    if let Some(error) = query.error.as_deref() {
        let detail = query
            .error_description
            .as_deref()
            .unwrap_or("the provider refused the login");
        return Err(ApiError::unauthorized_with(
            "oidc_denied",
            format!("OIDC login refused ({error}): {detail}"),
        )
        .into());
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
    let stored = verify_state_cookie(signer, headers, returned_state)?;

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

    // The login itself is a security event: who signed in, from where.
    let actor = claims.email.clone().unwrap_or_else(|| claims.sub.clone());
    crate::api::audit::record_entry(
        state,
        crate::db::NewAuditRecord {
            at: crate::models::now_iso(),
            actor,
            role: role.as_str().to_string(),
            method: "session".to_string(),
            action: "auth.login".to_string(),
            target: "oidc".to_string(),
            ip: client_ip(headers, None),
            user_agent: bounded_user_agent(headers),
            result: "ok".to_string(),
            request_id: None,
            details: Some(serde_json::json!({ "provider": client.settings.issuer }).to_string()),
        },
    );

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

/// Check the signed state cookie against the `state` returned by the provider.
fn verify_state_cookie(
    signer: &StateSigner,
    headers: &HeaderMap,
    returned_state: &str,
) -> Result<OidcState, CallbackFailure> {
    let raw_cookie = cookie_value(headers, OIDC_STATE_COOKIE).ok_or_else(|| {
        CallbackFailure::state(REASON_COOKIE_MISSING, "the login state cookie is missing")
    })?;
    let now = Utc::now().timestamp();
    let stored = signer.verify(&raw_cookie, now).map_err(|err| match err {
        SessionError::Expired => {
            CallbackFailure::state(REASON_EXPIRED, "the login state cookie has expired")
        }
        // Malformed or tampered: present but not a state this server issued.
        _ => CallbackFailure::state(REASON_STATE_MISMATCH, "the login state cookie is invalid"),
    })?;
    if !constant_time_eq(stored.state.as_bytes(), returned_state.as_bytes()) {
        return Err(CallbackFailure::state(
            REASON_STATE_MISMATCH,
            "the state does not match the login request",
        ));
    }
    Ok(stored)
}

/// Whether the client prefers an HTML error page over the JSON error body.
fn wants_html(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.to_ascii_lowercase().contains("text/html"))
}

/// `302 /auth/login` for a browser whose state cookie is gone.
fn redirect_to_login() -> Response {
    let mut response = StatusCode::FOUND.into_response();
    let headers = response.headers_mut();
    headers.insert(header::LOCATION, HeaderValue::from_static("/auth/login"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Small self-contained page: the cause and one way out.
fn html_failure_page(reason: &'static str) -> Response {
    let cause = match reason {
        REASON_EXPIRED => {
            "This login link has expired. Login links are short-lived, so start \
             again to get a fresh one."
        }
        REASON_STATE_MISMATCH => {
            "This login link was opened in another browser or device, or it has \
             already been used."
        }
        _ => "This login link was opened in another browser or device.",
    };
    let body = CALLBACK_ERROR_PAGE.replace("{cause}", cause);
    let mut response = (StatusCode::BAD_REQUEST, body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Bounded header value for the failure log line.
fn log_header(headers: &HeaderMap, name: HeaderName) -> String {
    let value = headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    truncate_chars(value, MAX_LOG_HEADER_CHARS)
}

/// `POST /auth/logout` — clear every cookie. Idempotent and unauthenticated:
/// logging out someone who is not logged in is harmless. A valid session is
/// identified in the audit trail before its cookie is cleared.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers_mut = response.headers_mut();
    for name in [SESSION_COOKIE, CSRF_COOKIE, OIDC_STATE_COOKIE] {
        append_cookie(
            headers_mut,
            &build_cookie(name, "", 0, true, state.auth.cookie_secure),
        );
    }

    let (actor, role) = cookie_value(&headers, SESSION_COOKIE)
        .and_then(|cookie| {
            state
                .auth
                .verify_session(&cookie, Utc::now().timestamp())
                .ok()
        })
        .map(|claims| (claims.user().actor(), claims.role.as_str()))
        .unwrap_or_else(|| ("anonymous".to_string(), "none"));

    crate::api::audit::record_entry(
        &state,
        crate::db::NewAuditRecord {
            at: crate::models::now_iso(),
            actor: actor.clone(),
            role: role.to_string(),
            method: "session".to_string(),
            action: "auth.logout".to_string(),
            target: actor,
            ip: client_ip(&headers, None),
            user_agent: bounded_user_agent(&headers),
            result: "ok".to_string(),
            request_id: None,
            details: None,
        },
    );
    response
}

/// Middleware guarding every `/api/*` route except health and auth config.
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
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
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| truncate_chars(value, MAX_USER_AGENT_CHARS));

    let mut context = None;

    if state.auth.oidc_enabled() {
        if let Some(value) = session_cookie.as_deref() {
            match state.auth.verify_session(value, Utc::now().timestamp()) {
                Ok(claims) => {
                    let user = claims.user();
                    let subject = user.actor();
                    context = Some(AuthContext {
                        actor: subject.clone(),
                        subject,
                        role: claims.role,
                        method: AuthMethod::Session,
                        user: Some(user),
                        scope: None,
                        ip: None,
                        user_agent: None,
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
            if let Some((subject, role, scope)) = bearer_identity(&state, token)? {
                context = Some(AuthContext {
                    actor: subject.clone(),
                    subject,
                    role,
                    method: AuthMethod::Bearer,
                    user: None,
                    scope,
                    ip: None,
                    user_agent: None,
                });
            }
        }
    }

    if context.is_none() && state.auth.allow_no_auth {
        context = Some(AuthContext {
            actor: "anonymous".to_string(),
            subject: "anonymous".to_string(),
            role: Role::Admin,
            method: AuthMethod::Bearer,
            user: None,
            scope: None,
            ip: None,
            user_agent: None,
        });
    }

    let Some(mut context) = context else {
        return Err(ApiError::unauthorized());
    };
    context.ip = ip;
    context.user_agent = user_agent;

    // The marketplace preview is a read-only POST: it resolves a source and
    // reports what would be installed, without writing anything (issue #61).
    // The path may or may not carry the `/api` prefix depending on the nesting
    // layer that runs this middleware.
    let unprefixed = path.strip_prefix("/api").unwrap_or(&path);
    let read_only_post = method == Method::POST && unprefixed == "/marketplace/skills/preview";

    if context.role == Role::Viewer && !is_read_method(&method) && !read_only_post {
        // A per-instance ACL may grant write (operator) or exec (owner) on an
        // instance route; `authz::require_instance_access` decides there.
        // Every other mutation stays admin-only.
        if crate::authz::route_target(request.uri().path(), &method).is_none() {
            crate::api::audit::record_result(
                &state,
                &context,
                "auth.mutation_refused",
                &path,
                Some(serde_json::json!({
                    "method": method.as_str(),
                    "reason": "viewer_read_only",
                })),
                "refused",
            );
            return Err(ApiError::forbidden(
                "forbidden",
                "the viewer role is read-only",
            ));
        }
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
            crate::api::audit::record_result(
                &state,
                &context,
                "auth.mutation_refused",
                &path,
                Some(serde_json::json!({
                    "method": method.as_str(),
                    "reason": "csrf_required",
                })),
                "refused",
            );
            return Err(ApiError::forbidden(
                "csrf_required",
                format!("cookie-authenticated mutations require the {CSRF_HEADER} header"),
            ));
        }
    }

    request.extensions_mut().insert(context);
    Ok(next.run(request).await)
}

/// Resolve a bearer token to `(actor/subject, role, scope)`.
fn bearer_identity(
    state: &AppState,
    token: &str,
) -> Result<Option<(String, Role, Option<TokenScope>)>, ApiError> {
    if let Some(expected) = state.auth.bootstrap_token.as_deref() {
        if constant_time_eq(expected.as_bytes(), token.as_bytes()) {
            return Ok(Some(("bootstrap".to_string(), Role::Admin, None)));
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
        let scope = TokenScope::parse_stored(record.scope.as_deref());
        (record.name, role, scope)
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
pub(crate) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
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

/// `User-Agent` as stored in the audit log, bounded to a sane length.
pub(crate) fn bounded_user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| truncate_chars(value, MAX_USER_AGENT_CHARS))
}

/// Best-effort client address, for the audit log and the rate limiter.
pub(crate) fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<String> {
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

    #[test]
    fn html_is_detected_anywhere_in_the_accept_header() {
        let mut headers = HeaderMap::new();
        assert!(
            !wants_html(&headers),
            "no Accept header means an API client"
        );

        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        assert!(!wants_html(&headers));

        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("TEXT/HTML, application/xhtml+xml;q=0.9"),
        );
        assert!(wants_html(&headers));

        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/html,application/xml;q=0.9,*/*;q=0.8"),
        );
        assert!(wants_html(&headers));
    }

    #[test]
    fn the_state_failures_carry_stable_reasons() {
        for (reason, message) in [
            (REASON_COOKIE_MISSING, "the login state cookie is missing"),
            (
                REASON_STATE_MISMATCH,
                "the state does not match the login request",
            ),
            (REASON_EXPIRED, "the login state cookie has expired"),
        ] {
            let failure = CallbackFailure::state(reason, message);
            assert_eq!(failure.error.code, "invalid_state");
            assert_eq!(failure.error.status, StatusCode::BAD_REQUEST);
            assert_eq!(
                failure.error.details.expect("details"),
                serde_json::json!({ "reason": reason })
            );
            assert_eq!(
                failure.redirect_to_login,
                reason == REASON_COOKIE_MISSING,
                "{reason}"
            );
        }
    }

    #[test]
    fn the_failure_page_is_html_and_uncached() {
        for reason in [REASON_EXPIRED, REASON_STATE_MISMATCH] {
            let response = html_failure_page(reason);
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("text/html; charset=utf-8")
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("no-store")
            );
        }
    }

    #[test]
    fn a_missing_cookie_redirects_the_browser_to_the_login() {
        let response = redirect_to_login();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/auth/login")
        );
    }

    #[test]
    fn log_headers_are_bounded() {
        let mut headers = HeaderMap::new();
        assert_eq!(log_header(&headers, header::USER_AGENT), "-");
        let long = "a".repeat(MAX_LOG_HEADER_CHARS + 50);
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_str(&long).expect("ascii"),
        );
        let logged = log_header(&headers, header::USER_AGENT);
        assert_eq!(logged.chars().count(), MAX_LOG_HEADER_CHARS + 1);
        assert!(logged.ends_with('…'));
    }
}
