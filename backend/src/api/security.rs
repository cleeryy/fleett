//! `GET /api/security/status` — effective protections of this process (issue
//! #49), admin only.
//!
//! The payload describes the running configuration, never a secret: no token,
//! no lockout key, no client address. It is what `fleett security` renders.

use crate::api::audit::require_admin;
use crate::api::error::ApiError;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::security;
use axum::extract::{Extension, State};
use axum::Json;
use serde::Serialize;

/// Effective limits of the three rate-limit buckets.
#[derive(Debug, Serialize)]
pub struct RateLimitLimits {
    /// `FLEETT_RATE_LIMIT_PER_MIN` (`0` = disabled).
    pub per_min: u32,
    /// `FLEETT_RATE_LIMIT_AUTH_PER_MIN` (`0` = disabled).
    pub auth_per_min: u32,
    /// `FLEETT_RATE_LIMIT_HEAVY_PER_MIN` (`0` = disabled).
    pub heavy_per_min: u32,
}

#[derive(Debug, Serialize)]
pub struct RateLimitStatus {
    /// At least one bucket is active.
    pub enabled: bool,
    pub limits: RateLimitLimits,
}

#[derive(Debug, Serialize)]
pub struct LockoutStatus {
    /// Failed authentications tolerated before the lockout engages
    /// (`FLEETT_AUTH_LOCKOUT_FAILURES`, `0` = disabled).
    pub failures: u32,
    /// Sliding window of the failure counter (`FLEETT_AUTH_LOCKOUT_WINDOW_S`).
    pub window_s: u64,
    /// How long one lockout lasts (`FLEETT_AUTH_LOCKOUT_S`).
    pub lockout_s: u64,
    /// Distinct client addresses currently locked out.
    pub locked_ips: usize,
}

#[derive(Debug, Serialize)]
pub struct HeaderStatus {
    pub csp: String,
    pub x_frame_options: String,
    pub referrer_policy: String,
    pub permissions_policy: String,
    pub x_content_type_options: String,
    /// `FLEETT_PUBLIC_URL` is https, so HSTS is sent.
    pub hsts: bool,
}

#[derive(Debug, Serialize)]
pub struct TokenStatus {
    /// Named tokens that were revoked (they never expire on their own).
    pub expired_or_revoked: usize,
    /// Named tokens still accepted. The bootstrap `FLEETT_TOKEN` is not stored
    /// in the database and is therefore not counted.
    pub active: usize,
}

#[derive(Debug, Serialize)]
pub struct SecurityStatus {
    pub rate_limit: RateLimitStatus,
    pub lockout: LockoutStatus,
    pub headers: HeaderStatus,
    pub tokens: TokenStatus,
    pub auth_mode: &'static str,
}

/// `GET /api/security/status` — admin only.
pub async fn status(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<SecurityStatus>, ApiError> {
    require_admin(&context)?;

    let cfg = state.security.config();
    let tokens = state.db.list_api_tokens().map_err(ApiError::from)?;
    let active = tokens.iter().filter(|token| token.is_active()).count();
    let expired_or_revoked = tokens.len() - active;

    Ok(Json(SecurityStatus {
        rate_limit: RateLimitStatus {
            enabled: cfg.limiting_enabled(),
            limits: RateLimitLimits {
                per_min: cfg.rate_limit_per_min,
                auth_per_min: cfg.rate_limit_auth_per_min,
                heavy_per_min: cfg.rate_limit_heavy_per_min,
            },
        },
        lockout: LockoutStatus {
            failures: cfg.lockout_failures,
            window_s: cfg.lockout_window_s,
            lockout_s: cfg.lockout_s,
            locked_ips: state.security.locked_ips(),
        },
        headers: HeaderStatus {
            csp: security::CONTENT_SECURITY_POLICY.to_string(),
            x_frame_options: security::X_FRAME_OPTIONS.to_string(),
            referrer_policy: security::REFERRER_POLICY.to_string(),
            permissions_policy: security::PERMISSIONS_POLICY.to_string(),
            x_content_type_options: security::X_CONTENT_TYPE_OPTIONS.to_string(),
            hsts: state.cfg.hsts_enabled(),
        },
        tokens: TokenStatus {
            expired_or_revoked,
            active,
        },
        auth_mode: state.auth.mode.as_str(),
    }))
}
