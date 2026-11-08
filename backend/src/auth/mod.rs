//! Authentication primitives shared by the HTTP layer.
//!
//! Two credential types are accepted, depending on `FLEETT_AUTH`:
//!
//! * a browser session (`fleett_session`), an HMAC-signed JSON cookie minted by
//!   the OpenID Connect callback;
//! * a bearer token, either the bootstrap `FLEETT_TOKEN` or a named token
//!   stored hashed in `api_tokens`.
//!
//! Both resolve to an [`AuthContext`] carrying the actor, its [`Role`] and the
//! credential type, which the API middleware uses for authorization, CSRF and
//! the audit log.

pub mod oidc;
pub mod service;
pub mod session;
pub mod tokens;

pub use service::{AuthService, OidcParts};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// `bytes` random bytes as lowercase hex (2 chars per byte).
pub fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buffer);
    let mut out = String::with_capacity(bytes * 2);
    for byte in buffer {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// `bytes` random bytes as unpadded base64url (PKCE alphabet).
pub fn random_base64_url(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

/// Authorization level. `viewer` is read-only, `admin` may mutate the fleet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Viewer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Viewer => "viewer",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "admin" => Some(Self::Admin),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }

    /// Map the OIDC `groups` claim to a role.
    ///
    /// The admin group wins: an account listed in both groups is an admin.
    /// Returns `None` when the account belongs to neither, which the callback
    /// turns into a `403`.
    pub fn from_groups(groups: &[String], admin_group: &str, viewer_group: &str) -> Option<Self> {
        if groups.iter().any(|group| group == admin_group) {
            return Some(Self::Admin);
        }
        if groups.iter().any(|group| group == viewer_group) {
            return Some(Self::Viewer);
        }
        None
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Credential that authenticated the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Session,
    Bearer,
}

impl AuthMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Bearer => "bearer",
        }
    }
}

/// Identity of the caller of one request.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// Human-readable actor: the token name for bearers, the email (or `sub`)
    /// for sessions.
    pub actor: String,
    pub role: Role,
    pub method: AuthMethod,
    /// Profile carried by a session, absent for bearer tokens.
    pub user: Option<SessionUser>,
    /// Client address, best effort (`X-Forwarded-For`, then the peer).
    pub ip: Option<String>,
}

/// Profile of the signed-in user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUser {
    pub sub: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl SessionUser {
    /// Name recorded in the audit log.
    pub fn actor(&self) -> String {
        self.email.clone().unwrap_or_else(|| self.sub.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn roles_round_trip() {
        assert_eq!(Role::parse("ADMIN"), Some(Role::Admin));
        assert_eq!(Role::parse("viewer"), Some(Role::Viewer));
        assert_eq!(Role::parse("root"), None);
        assert_eq!(Role::Admin.as_str(), "admin");
        assert_eq!(Role::Viewer.to_string(), "viewer");
    }

    #[test]
    fn role_mapping_prefers_the_admin_group() {
        let admin = "fleett-admins";
        let viewer = "fleett-viewers";
        assert_eq!(
            Role::from_groups(&groups(&["other", admin]), admin, viewer),
            Some(Role::Admin)
        );
        assert_eq!(
            Role::from_groups(&groups(&[admin, viewer]), admin, viewer),
            Some(Role::Admin)
        );
        assert_eq!(
            Role::from_groups(&groups(&[viewer]), admin, viewer),
            Some(Role::Viewer)
        );
        assert_eq!(Role::from_groups(&groups(&["other"]), admin, viewer), None);
        assert_eq!(Role::from_groups(&[], admin, viewer), None);
    }
}
