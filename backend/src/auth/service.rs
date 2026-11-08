//! Resolved authentication configuration, shared by every handler.

use crate::auth::oidc::{
    http_client, CodeExchange, EndpointCache, HttpCodeExchange, HttpTokenVerifier, OidcClient,
    OidcEndpoints, TokenVerifier,
};
use crate::auth::session::{SessionClaims, SessionError, SessionSigner, StateSigner};
use crate::config::{AuthMode, Config};
use std::sync::Arc;

/// Optional overrides used by tests to replace the network pieces.
#[derive(Default)]
pub struct OidcParts {
    pub endpoints: Option<OidcEndpoints>,
    pub verifier: Option<Arc<dyn TokenVerifier>>,
    pub exchange: Option<Arc<dyn CodeExchange>>,
}

/// Authentication surface of the control plane.
pub struct AuthService {
    pub mode: AuthMode,
    pub allow_no_auth: bool,
    pub session: Option<SessionSigner>,
    pub state_signer: Option<StateSigner>,
    pub oidc: Option<Arc<OidcClient>>,
    pub cookie_secure: bool,
    pub session_ttl_secs: u64,
    /// `FLEETT_TOKEN`, the bootstrap admin bearer.
    pub bootstrap_token: Option<String>,
}

impl AuthService {
    pub fn from_config(cfg: &Config) -> Self {
        Self::from_config_with(cfg, OidcParts::default())
    }

    pub fn from_config_with(cfg: &Config, parts: OidcParts) -> Self {
        let Some(oidc) = cfg.oidc.as_ref() else {
            return Self {
                mode: cfg.auth,
                allow_no_auth: cfg.allow_no_auth,
                session: None,
                state_signer: None,
                oidc: None,
                cookie_secure: false,
                session_ttl_secs: 0,
                bootstrap_token: cfg.token.clone(),
            };
        };

        let settings = oidc.settings.clone();
        let endpoints = Arc::new(match parts.endpoints {
            Some(endpoints) => EndpointCache::seeded(endpoints),
            None => EndpointCache::new(settings.issuer.clone()),
        });
        let verifier = parts
            .verifier
            .unwrap_or_else(|| Arc::new(HttpTokenVerifier::new(endpoints.clone(), http_client())));
        let exchange = parts.exchange.unwrap_or_else(|| {
            Arc::new(HttpCodeExchange::new(endpoints.clone(), settings.clone()))
        });
        let client = OidcClient::with_parts(settings, endpoints, verifier, exchange);

        Self {
            mode: cfg.auth,
            allow_no_auth: cfg.allow_no_auth,
            session: Some(SessionSigner::new(oidc.session_secret.clone())),
            state_signer: Some(StateSigner::new(oidc.session_secret.clone())),
            oidc: Some(Arc::new(client)),
            cookie_secure: oidc.cookie_secure,
            session_ttl_secs: oidc.session_ttl_secs,
            bootstrap_token: cfg.token.clone(),
        }
    }

    pub fn bearer_enabled(&self) -> bool {
        self.mode.bearer_enabled()
    }

    pub fn oidc_enabled(&self) -> bool {
        self.mode.oidc_enabled() && self.oidc.is_some()
    }

    /// Verify a `fleett_session` cookie against the wall clock.
    pub fn verify_session(&self, value: &str, now: i64) -> Result<SessionClaims, SessionError> {
        match &self.session {
            Some(signer) => signer.verify(value, now),
            None => Err(SessionError::Malformed),
        }
    }
}
