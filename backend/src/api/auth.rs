//! Bearer-token authentication.

use crate::api::error::ApiError;
use crate::api::AppState;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use subtle::ConstantTimeEq;

/// `GET /api/auth/check` — reaching this handler means the token is valid.
pub async fn check() -> StatusCode {
    StatusCode::NO_CONTENT
}

/// Middleware guarding every route except `GET /api/health`.
pub async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_token);

    let authorized = match state.cfg.token.as_deref() {
        Some(expected) => provided
            .map(|token| constant_time_eq(expected.as_bytes(), token.as_bytes()))
            .unwrap_or(false),
        // Only reachable with FLEETT_ALLOW_NO_AUTH=1; the startup banner warns.
        None => state.cfg.allow_no_auth,
    };

    if !authorized {
        return Err(ApiError::unauthorized());
    }
    Ok(next.run(request).await)
}

fn bearer_token(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.ct_eq(right).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bearer_headers() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer  abc "), Some("abc"));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("abc"), None);
    }

    #[test]
    fn constant_time_comparison_is_exact() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret "));
    }
}
