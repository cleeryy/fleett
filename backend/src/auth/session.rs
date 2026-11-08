//! Signed cookies: the browser session and the short-lived OIDC state.
//!
//! Both are HMAC-SHA256 sealed values (`v1.<payload>.<mac>`, base64url), keyed
//! by `FLEETT_SESSION_SECRET`. Only the control plane can mint one, so a
//! tampered cookie is rejected before its JSON is even parsed.

use crate::auth::{Role, SessionUser};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "fleett_session";
/// Name of the CSRF cookie (readable by the frontend on purpose).
pub const CSRF_COOKIE: &str = "fleett_csrf";
/// Name of the short-lived cookie holding state, nonce and PKCE verifier.
pub const OIDC_STATE_COOKIE: &str = "fleett_oidc";
/// Header that must echo the CSRF cookie on cookie-authenticated mutations.
pub const CSRF_HEADER: &str = "x-fleett-csrf";

const VERSION: &str = "v1";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("malformed signed value")]
    Malformed,
    #[error("bad signature")]
    BadSignature,
    #[error("session expired")]
    Expired,
    #[error("invalid payload: {0}")]
    Invalid(String),
}

/// Seals and opens opaque payloads with HMAC-SHA256.
#[derive(Debug, Clone)]
pub struct Signer {
    key: Vec<u8>,
}

impl Signer {
    pub fn new(key: Vec<u8>) -> Self {
        Self { key }
    }

    /// `v1.<base64url(payload)>.<base64url(hmac)>`.
    pub fn seal(&self, payload: &[u8]) -> String {
        let encoded = URL_SAFE_NO_PAD.encode(payload);
        let signed = format!("{VERSION}.{encoded}");
        let mac = self.mac(signed.as_bytes());
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(mac))
    }

    /// Verify the MAC, then return the payload bytes.
    pub fn open(&self, value: &str) -> Result<Vec<u8>, SessionError> {
        let mut parts = value.split('.');
        let (Some(version), Some(encoded), Some(mac)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(SessionError::Malformed);
        };
        if version != VERSION || parts.next().is_some() {
            return Err(SessionError::Malformed);
        }
        let provided = URL_SAFE_NO_PAD
            .decode(mac)
            .map_err(|_| SessionError::Malformed)?;
        let expected = self.mac(format!("{version}.{encoded}").as_bytes());
        if provided.len() != expected.len() || !bool::from(provided.ct_eq(&expected)) {
            return Err(SessionError::BadSignature);
        }
        URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| SessionError::Malformed)
    }

    fn mac(&self, message: &[u8]) -> Vec<u8> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts any key size");
        mac.update(message);
        mac.finalize().into_bytes().to_vec()
    }
}

/// Payload of the session cookie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClaims {
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    pub role: Role,
    pub iat: i64,
    pub exp: i64,
}

impl SessionClaims {
    pub fn is_expired(&self, now: i64) -> bool {
        self.exp <= now
    }

    pub fn user(&self) -> SessionUser {
        SessionUser {
            sub: self.sub.clone(),
            email: self.email.clone(),
            name: self.name.clone(),
        }
    }
}

/// Session cookie codec.
#[derive(Debug, Clone)]
pub struct SessionSigner {
    signer: Signer,
}

impl SessionSigner {
    pub fn new(secret: Vec<u8>) -> Self {
        Self {
            signer: Signer::new(secret),
        }
    }

    pub fn sign(&self, claims: &SessionClaims) -> String {
        let payload = serde_json::to_vec(claims).expect("session claims always serialize");
        self.signer.seal(&payload)
    }

    pub fn verify(&self, value: &str, now: i64) -> Result<SessionClaims, SessionError> {
        let payload = self.signer.open(value)?;
        let claims: SessionClaims = serde_json::from_slice(&payload)
            .map_err(|err| SessionError::Invalid(err.to_string()))?;
        if claims.is_expired(now) {
            return Err(SessionError::Expired);
        }
        Ok(claims)
    }
}

/// Payload of the short-lived OIDC state cookie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcState {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub exp: i64,
}

/// State cookie codec (same key as the session).
#[derive(Debug, Clone)]
pub struct StateSigner {
    signer: Signer,
}

impl StateSigner {
    pub fn new(secret: Vec<u8>) -> Self {
        Self {
            signer: Signer::new(secret),
        }
    }

    pub fn sign(&self, state: &OidcState) -> String {
        let payload = serde_json::to_vec(state).expect("state always serializes");
        self.signer.seal(&payload)
    }

    pub fn verify(&self, value: &str, now: i64) -> Result<OidcState, SessionError> {
        let payload = self.signer.open(value)?;
        let state: OidcState = serde_json::from_slice(&payload)
            .map_err(|err| SessionError::Invalid(err.to_string()))?;
        if state.exp <= now {
            return Err(SessionError::Expired);
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(exp: i64) -> SessionClaims {
        SessionClaims {
            sub: "d80c090d".into(),
            email: Some("cleeryy@example.test".into()),
            name: Some("Cleeryy".into()),
            role: Role::Admin,
            iat: 1_760_000_000,
            exp,
        }
    }

    #[test]
    fn signs_and_verifies_a_session() {
        let signer = SessionSigner::new(vec![7u8; 32]);
        let cookie = signer.sign(&claims(1_760_028_800));
        assert!(cookie.starts_with("v1."));
        let decoded = signer.verify(&cookie, 1_760_000_100).expect("valid");
        assert_eq!(decoded, claims(1_760_028_800));
        assert_eq!(decoded.user().actor(), "cleeryy@example.test");
        assert_eq!(decoded.role, Role::Admin);
    }

    #[test]
    fn rejects_a_tampered_payload() {
        let signer = SessionSigner::new(vec![7u8; 32]);
        let cookie = signer.sign(&claims(1_760_028_800));
        let mut parts: Vec<&str> = cookie.split('.').collect();
        // Swap the payload for another valid-looking one.
        let forged = SessionSigner::new(vec![7u8; 32]).sign(&claims(9_999_999_999));
        let forged_payload = forged.split('.').nth(1).expect("payload");
        parts[1] = forged_payload;
        let tampered = parts.join(".");
        assert_eq!(signer.verify(&tampered, 0), Err(SessionError::BadSignature));
    }

    #[test]
    fn rejects_a_tampered_mac() {
        let signer = SessionSigner::new(vec![7u8; 32]);
        let cookie = signer.sign(&claims(1_760_028_800));
        let mut bytes = cookie.into_bytes();
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).expect("utf8");
        assert_eq!(signer.verify(&tampered, 0), Err(SessionError::BadSignature));
    }

    #[test]
    fn rejects_an_expired_session() {
        let signer = SessionSigner::new(vec![7u8; 32]);
        let cookie = signer.sign(&claims(1_760_000_000));
        assert_eq!(
            signer.verify(&cookie, 1_760_000_000),
            Err(SessionError::Expired)
        );
        assert!(signer.verify(&cookie, 1_759_999_999).is_ok());
    }

    #[test]
    fn a_different_key_cannot_open_the_cookie() {
        let cookie = SessionSigner::new(vec![7u8; 32]).sign(&claims(1_760_028_800));
        let other = SessionSigner::new(vec![8u8; 32]);
        assert_eq!(other.verify(&cookie, 0), Err(SessionError::BadSignature));
    }

    #[test]
    fn malformed_cookies_are_rejected() {
        let signer = SessionSigner::new(vec![7u8; 32]);
        for value in [
            "",
            "v1",
            "v1.payload",
            "v2.abc.def",
            "v1.a.b.c",
            "not a cookie",
        ] {
            assert_eq!(
                signer.verify(value, 0),
                Err(SessionError::Malformed),
                "{value}"
            );
        }
    }

    #[test]
    fn state_cookie_round_trips_and_expires() {
        let signer = StateSigner::new(vec![1u8; 32]);
        let state = OidcState {
            state: "s".into(),
            nonce: "n".into(),
            verifier: "v".into(),
            exp: 1_000,
        };
        let cookie = signer.sign(&state);
        assert_eq!(signer.verify(&cookie, 999).expect("valid"), state);
        assert_eq!(signer.verify(&cookie, 1_000), Err(SessionError::Expired));
    }
}
