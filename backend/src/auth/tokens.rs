//! Named API tokens for automation.
//!
//! A token is shown exactly once, when it is created: only its SHA-256 digest
//! is stored, so a database dump never yields a usable credential. Revoking a
//! token sets `revoked_at`; the row is kept for the audit trail.

use crate::auth::random_hex;
use sha2::{Digest, Sha256};

/// Prefix of every named token.
pub const TOKEN_PREFIX: &str = "flt_";
/// Number of hex characters after the prefix.
pub const TOKEN_HEX_CHARS: usize = 32;

/// A fresh `flt_<32 hex>` token.
pub fn generate() -> String {
    format!("{TOKEN_PREFIX}{}", random_hex(TOKEN_HEX_CHARS / 2))
}

/// Hex-encoded SHA-256 digest of a token, as stored in `api_tokens.hash`.
pub fn hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Cheap shape check so a malformed header never reaches the database.
pub fn looks_like_token(token: &str) -> bool {
    match token.strip_prefix(TOKEN_PREFIX) {
        Some(rest) => rest.len() == TOKEN_HEX_CHARS && rest.chars().all(|c| c.is_ascii_hexdigit()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_have_the_expected_shape() {
        let token = generate();
        assert!(token.starts_with("flt_"));
        assert_eq!(token.len(), TOKEN_PREFIX.len() + TOKEN_HEX_CHARS);
        assert!(looks_like_token(&token));
        assert_ne!(generate(), token, "tokens must be random");
        assert!(!looks_like_token("flt_short"));
        assert!(!looks_like_token("nope"));
    }

    #[test]
    fn hashing_is_stable_and_distinct() {
        let first = generate();
        let second = generate();
        assert_eq!(hash(&first), hash(&first));
        assert_ne!(hash(&first), hash(&second));
        assert_eq!(hash(&first).len(), 64);
        assert!(hash(&first).chars().all(|c| c.is_ascii_hexdigit()));
    }
}
