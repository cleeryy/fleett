//! OpenID Connect authorization-code flow with PKCE.
//!
//! Three pieces are deliberately separable so tests never need an identity
//! provider:
//!
//! * [`EndpointCache`] resolves `.well-known/openid-configuration` once and
//!   caches it;
//! * [`TokenVerifier`] checks an ID token (signature through the provider's
//!   JWKS, then issuer, audience, expiry and nonce);
//! * [`CodeExchange`] turns an authorization code into a token response.
//!
//! [`OidcClient`] wires them together. Tests inject fake implementations of
//! the last two and seed the endpoint cache.

use crate::auth::{random_base64_url, Role};
use crate::config::OidcSettings;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;

const DISCOVERY_PATH: &str = ".well-known/openid-configuration";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("OIDC discovery failed: {0}")]
    Discovery(String),
    #[error("token exchange failed: {0}")]
    Exchange(String),
    #[error("invalid ID token: {0}")]
    InvalidToken(String),
    #[error("the ID token is missing the nonce claim")]
    MissingNonce,
    #[error("the ID token nonce does not match the login request")]
    NonceMismatch,
}

/// Endpoints advertised by the provider.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OidcEndpoints {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

impl OidcEndpoints {
    /// An endpoint set for tests: every URL points at the local test server.
    pub fn for_tests(base_url: &str) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            issuer: base.to_string(),
            authorization_endpoint: format!("{base}/authorize"),
            token_endpoint: format!("{base}/token"),
            jwks_uri: format!("{base}/jwks"),
        }
    }
}

/// Lazily discovered provider endpoints.
#[derive(Debug)]
pub struct EndpointCache {
    issuer: String,
    http: reqwest::Client,
    cached: Mutex<Option<OidcEndpoints>>,
}

impl EndpointCache {
    pub fn new(issuer: impl Into<String>) -> Self {
        Self {
            issuer: normalize_issuer(&issuer.into()),
            http: http_client(),
            cached: Mutex::new(None),
        }
    }

    /// Cache a known endpoint set (tests, or a proxy that pre-fetched them).
    pub fn seeded(endpoints: OidcEndpoints) -> Self {
        let issuer = normalize_issuer(&endpoints.issuer);
        Self {
            issuer,
            http: http_client(),
            cached: Mutex::new(Some(endpoints)),
        }
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn get(&self) -> Option<OidcEndpoints> {
        self.cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Return the cached endpoints, discovering them on first use.
    pub async fn resolve(&self) -> Result<OidcEndpoints, OidcError> {
        if let Some(endpoints) = self.get() {
            return Ok(endpoints);
        }
        let url = format!("{}/{DISCOVERY_PATH}", self.issuer);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|err| OidcError::Discovery(format!("{url}: {err}")))?;
        if !response.status().is_success() {
            return Err(OidcError::Discovery(format!(
                "{url}: HTTP {}",
                response.status()
            )));
        }
        let endpoints: OidcEndpoints = response
            .json()
            .await
            .map_err(|err| OidcError::Discovery(format!("{url}: {err}")))?;
        if normalize_issuer(&endpoints.issuer) != self.issuer {
            return Err(OidcError::Discovery(format!(
                "discovery issuer {:?} does not match the configured issuer {:?}",
                endpoints.issuer, self.issuer
            )));
        }
        *self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(endpoints.clone());
        Ok(endpoints)
    }
}

/// Values the ID token must match.
#[derive(Debug, Clone)]
pub struct TokenExpectations {
    pub issuer: String,
    pub client_id: String,
    pub nonce: String,
}

/// ID token verification, abstracted so tests can inject a fake.
pub trait TokenVerifier: Send + Sync {
    fn verify<'a>(
        &'a self,
        token: &'a str,
        expected: &'a TokenExpectations,
    ) -> BoxFuture<'a, Result<IdTokenClaims, OidcError>>;
}

/// Authorization-code exchange, abstracted so tests can stub the IdP.
pub trait CodeExchange: Send + Sync {
    fn exchange<'a>(
        &'a self,
        code: &'a str,
        code_verifier: &'a str,
    ) -> BoxFuture<'a, Result<TokenResponse, OidcError>>;
}

/// Response of the token endpoint (only `id_token` is used).
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub id_token: String,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
}

/// Claims of the ID token that fleett maps to an identity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IdTokenClaims {
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default, deserialize_with = "deserialize_groups")]
    pub groups: Vec<String>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub iss: String,
}

impl IdTokenClaims {
    /// `name`, falling back to the Authentik `preferred_username` claim.
    pub fn display_name(&self) -> Option<&str> {
        self.name.as_deref().or(self.preferred_username.as_deref())
    }

    /// Check the nonce, in constant time.
    pub fn check_nonce(&self, expected: &str) -> Result<(), OidcError> {
        let Some(nonce) = self.nonce.as_deref() else {
            return Err(OidcError::MissingNonce);
        };
        use subtle::ConstantTimeEq;
        let matches = nonce.len() == expected.len()
            && bool::from(nonce.as_bytes().ct_eq(expected.as_bytes()));
        if matches {
            Ok(())
        } else {
            Err(OidcError::NonceMismatch)
        }
    }
}

/// `groups` may be a JSON array or a space-separated string.
fn deserialize_groups<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Groups {
        List(Vec<String>),
        Text(String),
    }

    Ok(match Option::<Groups>::deserialize(deserializer)? {
        None => Vec::new(),
        Some(Groups::List(list)) => list,
        Some(Groups::Text(text)) => text.split_whitespace().map(str::to_string).collect(),
    })
}

/// Real verifier: JWKS signature + registered claims + nonce.
pub struct HttpTokenVerifier {
    endpoints: Arc<EndpointCache>,
    http: reqwest::Client,
    jwks: tokio::sync::RwLock<Option<JwkSet>>,
}

impl HttpTokenVerifier {
    pub fn new(endpoints: Arc<EndpointCache>, http: reqwest::Client) -> Self {
        Self {
            endpoints,
            http,
            jwks: tokio::sync::RwLock::new(None),
        }
    }

    async fn jwks(&self, refresh: bool) -> Result<JwkSet, OidcError> {
        if !refresh {
            if let Some(keys) = self.jwks.read().await.clone() {
                return Ok(keys);
            }
        }
        let endpoints = self.endpoints.resolve().await?;
        let response = self
            .http
            .get(&endpoints.jwks_uri)
            .send()
            .await
            .map_err(|err| OidcError::InvalidToken(format!("JWKS: {err}")))?;
        if !response.status().is_success() {
            return Err(OidcError::InvalidToken(format!(
                "JWKS: HTTP {}",
                response.status()
            )));
        }
        let keys: JwkSet = response
            .json()
            .await
            .map_err(|err| OidcError::InvalidToken(format!("JWKS: {err}")))?;
        *self.jwks.write().await = Some(keys.clone());
        Ok(keys)
    }

    async fn verify_inner(
        &self,
        token: &str,
        expected: &TokenExpectations,
    ) -> Result<IdTokenClaims, OidcError> {
        let header = jsonwebtoken::decode_header(token)
            .map_err(|err| OidcError::InvalidToken(err.to_string()))?;
        if header.alg != jsonwebtoken::Algorithm::RS256 {
            return Err(OidcError::InvalidToken(format!(
                "unexpected algorithm {:?}",
                header.alg
            )));
        }

        let mut keys = self.jwks(false).await?;
        let mut jwk = keys.find(header.kid.as_deref());
        if jwk.is_none() {
            // The provider may have rotated its keys.
            keys = self.jwks(true).await?;
            jwk = keys.find(header.kid.as_deref());
        }
        let jwk = jwk.ok_or_else(|| {
            OidcError::InvalidToken(format!(
                "no RSA signing key for kid {:?}",
                header.kid.as_deref().unwrap_or("<none>")
            ))
        })?;
        let (Some(n), Some(e)) = (jwk.n.as_deref(), jwk.e.as_deref()) else {
            return Err(OidcError::InvalidToken(
                "the signing key is missing its RSA components".to_string(),
            ));
        };
        let key = jsonwebtoken::DecodingKey::from_rsa_components(n, e)
            .map_err(|err| OidcError::InvalidToken(err.to_string()))?;

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.set_audience(&[expected.client_id.as_str()]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.leeway = 30;
        let data = jsonwebtoken::decode::<IdTokenClaims>(token, &key, &validation)
            .map_err(|err| OidcError::InvalidToken(err.to_string()))?;

        // The issuer is compared after normalising a trailing slash so an IdP
        // that advertises `.../` but signs `...` (or the reverse) still works.
        if normalize_issuer(&data.claims.iss) != normalize_issuer(&expected.issuer) {
            return Err(OidcError::InvalidToken(format!(
                "issuer {:?} does not match {:?}",
                data.claims.iss, expected.issuer
            )));
        }
        data.claims.check_nonce(&expected.nonce)?;
        Ok(data.claims)
    }
}

impl TokenVerifier for HttpTokenVerifier {
    fn verify<'a>(
        &'a self,
        token: &'a str,
        expected: &'a TokenExpectations,
    ) -> BoxFuture<'a, Result<IdTokenClaims, OidcError>> {
        Box::pin(self.verify_inner(token, expected))
    }
}

/// Real exchange: `POST token_endpoint` with `client_secret_basic` + PKCE.
pub struct HttpCodeExchange {
    endpoints: Arc<EndpointCache>,
    settings: OidcSettings,
    http: reqwest::Client,
}

impl HttpCodeExchange {
    pub fn new(endpoints: Arc<EndpointCache>, settings: OidcSettings) -> Self {
        Self {
            endpoints,
            settings,
            http: http_client(),
        }
    }

    async fn exchange_inner(
        &self,
        code: &str,
        code_verifier: &str,
    ) -> Result<TokenResponse, OidcError> {
        let endpoints = self.endpoints.resolve().await?;
        let response = self
            .http
            .post(&endpoints.token_endpoint)
            .basic_auth(&self.settings.client_id, Some(&self.settings.client_secret))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", self.settings.redirect_url.as_str()),
                ("client_id", self.settings.client_id.as_str()),
                ("code_verifier", code_verifier),
            ])
            .send()
            .await
            .map_err(|err| OidcError::Exchange(err.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| OidcError::Exchange(err.to_string()))?;
        if !status.is_success() {
            return Err(OidcError::Exchange(format!(
                "HTTP {status}: {}",
                truncate(&body, 512)
            )));
        }
        let token: TokenResponse = serde_json::from_str(&body)
            .map_err(|err| OidcError::Exchange(format!("unexpected token response: {err}")))?;
        if token.id_token.trim().is_empty() {
            return Err(OidcError::Exchange(
                "the token response has no id_token".to_string(),
            ));
        }
        Ok(token)
    }
}

impl CodeExchange for HttpCodeExchange {
    fn exchange<'a>(
        &'a self,
        code: &'a str,
        code_verifier: &'a str,
    ) -> BoxFuture<'a, Result<TokenResponse, OidcError>> {
        Box::pin(self.exchange_inner(code, code_verifier))
    }
}

/// The whole flow, as used by the HTTP handlers.
pub struct OidcClient {
    pub settings: OidcSettings,
    pub endpoints: Arc<EndpointCache>,
    pub verifier: Arc<dyn TokenVerifier>,
    pub exchange: Arc<dyn CodeExchange>,
}

impl OidcClient {
    /// Production wiring (real HTTP, real JWKS validation).
    pub fn production(settings: OidcSettings) -> Self {
        let endpoints = Arc::new(EndpointCache::new(settings.issuer.clone()));
        let http = http_client();
        Self {
            verifier: Arc::new(HttpTokenVerifier::new(endpoints.clone(), http)),
            exchange: Arc::new(HttpCodeExchange::new(endpoints.clone(), settings.clone())),
            settings,
            endpoints,
        }
    }

    /// Wiring with injected verifier/exchange (tests).
    pub fn with_parts(
        settings: OidcSettings,
        endpoints: Arc<EndpointCache>,
        verifier: Arc<dyn TokenVerifier>,
        exchange: Arc<dyn CodeExchange>,
    ) -> Self {
        Self {
            settings,
            endpoints,
            verifier,
            exchange,
        }
    }

    /// Authorization URL with state, nonce and PKCE.
    pub async fn authorization_url(
        &self,
        state: &str,
        nonce: &str,
        code_challenge: &str,
    ) -> Result<String, OidcError> {
        let endpoints = self.endpoints.resolve().await?;
        let mut url = Url::parse(&endpoints.authorization_endpoint)
            .map_err(|err| OidcError::Discovery(err.to_string()))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.settings.client_id)
            .append_pair("redirect_uri", &self.settings.redirect_url)
            .append_pair("scope", &self.settings.scopes)
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", code_challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url.to_string())
    }

    /// Exchange the code, then verify the ID token against `nonce`.
    pub async fn complete(
        &self,
        code: &str,
        code_verifier: &str,
        nonce: &str,
    ) -> Result<IdTokenClaims, OidcError> {
        let token = self.exchange.exchange(code, code_verifier).await?;
        let expected = TokenExpectations {
            issuer: self.settings.issuer.clone(),
            client_id: self.settings.client_id.clone(),
            nonce: nonce.to_string(),
        };
        self.verifier.verify(&token.id_token, &expected).await
    }

    /// Map the `groups` claim to a role.
    pub fn role(&self, groups: &[String]) -> Option<Role> {
        Role::from_groups(
            groups,
            &self.settings.admin_group,
            &self.settings.viewer_group,
        )
    }
}

/// PKCE verifier: 43 characters from 32 random bytes.
pub fn pkce_verifier() -> String {
    random_base64_url(32)
}

/// `BASE64URL(SHA256(verifier))`, the S256 challenge.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn normalize_issuer(issuer: &str) -> String {
    issuer.trim().trim_end_matches('/').to_string()
}

pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .expect("the HTTP client configuration is valid")
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push('…');
    out
}

#[derive(Debug, Clone, Default, Deserialize)]
struct JwkSet {
    #[serde(default)]
    keys: Vec<Jwk>,
}

impl JwkSet {
    fn find(&self, kid: Option<&str>) -> Option<&Jwk> {
        self.keys
            .iter()
            .filter(|key| key.kty.eq_ignore_ascii_case("RSA"))
            .filter(|key| key.usage.as_deref() != Some("enc"))
            .find(|key| match (kid, key.kid.as_deref()) {
                (Some(wanted), Some(found)) => wanted == found,
                (Some(_), None) => false,
                (None, _) => true,
            })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    #[serde(default)]
    kid: Option<String>,
    kty: String,
    #[serde(rename = "use", default)]
    usage: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let generated = pkce_verifier();
        assert_eq!(generated.len(), 43);
        assert!(generated
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn issuers_are_compared_without_the_trailing_slash() {
        assert_eq!(
            normalize_issuer(" https://sso.example.test/application/o/fleett/ "),
            "https://sso.example.test/application/o/fleett"
        );
    }

    #[test]
    fn groups_accept_arrays_and_space_separated_strings() {
        let array: IdTokenClaims =
            serde_json::from_str(r#"{"sub":"u","groups":["a","b"]}"#).expect("array");
        assert_eq!(array.groups, vec!["a", "b"]);

        let text: IdTokenClaims =
            serde_json::from_str(r#"{"sub":"u","groups":"a b"}"#).expect("string");
        assert_eq!(text.groups, vec!["a", "b"]);

        let absent: IdTokenClaims = serde_json::from_str(r#"{"sub":"u"}"#).expect("absent");
        assert!(absent.groups.is_empty());
    }

    #[test]
    fn nonce_check_is_exact() {
        let claims = IdTokenClaims {
            sub: "u".into(),
            email: None,
            name: None,
            preferred_username: None,
            groups: Vec::new(),
            nonce: Some("abc".into()),
            iss: String::new(),
        };
        assert!(claims.check_nonce("abc").is_ok());
        assert!(matches!(
            claims.check_nonce("abd"),
            Err(OidcError::NonceMismatch)
        ));
        let mut missing = claims.clone();
        missing.nonce = None;
        assert!(matches!(
            missing.check_nonce("abc"),
            Err(OidcError::MissingNonce)
        ));
    }

    #[test]
    fn seeded_endpoints_are_returned_without_network() {
        let cache = EndpointCache::seeded(OidcEndpoints::for_tests("https://idp.test/"));
        assert_eq!(
            cache.get().expect("cached").authorization_endpoint,
            "https://idp.test/authorize"
        );
        assert_eq!(cache.issuer(), "https://idp.test");
    }

    #[tokio::test]
    async fn authorization_url_carries_state_nonce_and_pkce() {
        let settings = OidcSettings {
            issuer: "https://idp.test".into(),
            client_id: "fleett".into(),
            client_secret: "secret".into(),
            redirect_url: "https://fleett.test/auth/callback".into(),
            scopes: "openid profile email".into(),
            admin_group: "admins".into(),
            viewer_group: "viewers".into(),
        };
        let cache = Arc::new(EndpointCache::seeded(OidcEndpoints::for_tests(
            "https://idp.test",
        )));
        let client = OidcClient::with_parts(
            settings,
            cache,
            Arc::new(FakeVerifier),
            Arc::new(FakeExchange),
        );
        let verifier = pkce_verifier();
        let url = client
            .authorization_url("state-1", "nonce-1", &pkce_challenge(&verifier))
            .await
            .expect("url");
        let parsed = Url::parse(&url).expect("valid url");
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(
            params.get("response_type").map(String::as_str),
            Some("code")
        );
        assert_eq!(params.get("state").map(String::as_str), Some("state-1"));
        assert_eq!(params.get("nonce").map(String::as_str), Some("nonce-1"));
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("https://fleett.test/auth/callback")
        );
        assert_eq!(
            params.get("scope").map(String::as_str),
            Some("openid profile email")
        );
    }

    #[derive(Default)]
    struct FakeVerifier;

    impl TokenVerifier for FakeVerifier {
        fn verify<'a>(
            &'a self,
            _token: &'a str,
            _expected: &'a TokenExpectations,
        ) -> BoxFuture<'a, Result<IdTokenClaims, OidcError>> {
            Box::pin(async { Err(OidcError::InvalidToken("fake".into())) })
        }
    }

    #[derive(Default)]
    struct FakeExchange;

    impl CodeExchange for FakeExchange {
        fn exchange<'a>(
            &'a self,
            _code: &'a str,
            _verifier: &'a str,
        ) -> BoxFuture<'a, Result<TokenResponse, OidcError>> {
            Box::pin(async {
                Ok(TokenResponse {
                    id_token: "fake".into(),
                    access_token: None,
                    token_type: None,
                })
            })
        }
    }
}
