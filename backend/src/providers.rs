//! Reusable provider credentials (issue #25).
//!
//! A provider is a first-class object — name, kind, base URL, variable name
//! and one encrypted secret — that several instances can share. An instance
//! references it with `provider: "<id>"`; the compose file still declares
//! `${api_key_env}` and the compose driver injects the decrypted secret into
//! its child environment, exactly as it does for per-instance secrets. The
//! container contract does not change.
//!
//! Instances created before this feature have no provider and keep using their
//! own `gate_token`/`api_key_env`: the resolution below only overrides the key
//! when the instance row names a provider.
//!
//! Security:
//!
//! * the secret lives in `providers.nonce`/`providers.ciphertext`, encrypted
//!   with the same AES-256-GCM key as the instance environment (fresh nonce
//!   per write, the value is bound to the `secret` field name);
//! * the optional upstream headers (issue #64) live in
//!   `providers.headers_nonce`/`headers_ciphertext`, encrypted the same way and
//!   bound to the `header:` field prefix; only their names are returned by the
//!   API;
//! * no endpoint ever returns the secret or a header value — [`ProviderSummary`]
//!   has no secret field and the probe/test response only reports statuses,
//!   latency and a redacted error string;
//! * rotating it is a `PATCH /api/providers/{id}` that restarts every attached
//!   instance with the new value.

use crate::config::Config;
use crate::db::{Db, DbError, ProviderRecord};
use crate::envs;
use crate::models::{
    ProviderInstanceRef, ProviderSpec, ProviderSummary, ProviderTestResponse,
    PROVIDER_SECRET_MAX_BYTES,
};
use crate::secrets::{self, SecretsError, SecretsKey};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Field name the secret is encrypted under. A blob moved from another row (or
/// another field) fails to decrypt instead of silently changing a credential.
pub const SECRET_FIELD: &str = "secret";
/// Prefix every upstream header is encrypted under, so a secret blob moved to
/// the headers column never decrypts into a usable header.
pub const HEADER_FIELD_PREFIX: &str = "header:";
/// Path probed by the `GET` half of `POST /api/providers/{id}/test`.
pub const TEST_PATH: &str = "/models";
/// Path called by the completion half of the test, and by the assistant.
pub const COMPLETION_PATH: &str = "/chat/completions";
/// Most upstream headers one provider may carry.
pub const MAX_HEADERS: usize = 32;
/// Longest accepted header name (RFC 7230 token).
pub const MAX_HEADER_NAME_BYTES: usize = 128;
/// Longest accepted header value.
pub const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;
/// Longest accepted provider default model.
pub const MAX_MODEL_NAME_CHARS: usize = 200;
/// Header the opencode.ai gateway needs to route a completion (issue #64).
pub const SESSION_HEADER: &str = "x-opencode-session";
/// Hosts whose gateway requires [`SESSION_HEADER`].
pub const OPENCODE_HOST: &str = "opencode.ai";
/// Longest session id echoed in [`SESSION_HEADER`].
pub const MAX_SESSION_HEADER_CHARS: usize = 128;
/// Part of an upstream error body kept in an error detail (issue #64).
pub const MAX_UPSTREAM_BODY_BYTES: usize = 1024;
/// Part of an upstream error body kept in a provider test `error` string.
pub const MAX_UPSTREAM_ERROR_BYTES: usize = 512;
/// Hard timeout of one connectivity test (each of its two calls).
pub const TEST_TIMEOUT: Duration = Duration::from_secs(5);
/// `max_tokens` of the completion half of the test: one token is enough to
/// prove the route answers.
pub const TEST_MAX_TOKENS: u64 = 1;
/// Instance came back healthy after a rotation.
pub const RESTART_HEALTHY: &str = "healthy";
/// Instance did not come back after a rotation.
pub const RESTART_FAILED: &str = "failed";

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Secrets(#[from] SecretsError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Env(#[from] envs::EnvError),
    #[error("provider \"{0}\" not found")]
    NotFound(String),
}

/// Validate a secret before it is encrypted.
pub fn validate_secret(secret: &str) -> Result<(), ProviderError> {
    if secret.trim().is_empty() {
        return Err(ProviderError::Invalid(
            "secret must not be empty".to_string(),
        ));
    }
    if secret.len() > PROVIDER_SECRET_MAX_BYTES {
        return Err(ProviderError::Invalid(format!(
            "secret must be at most {} KiB",
            PROVIDER_SECRET_MAX_BYTES / 1024
        )));
    }
    crate::models::validate_secret_value(secret).map_err(ProviderError::Invalid)
}

/// Validate one HTTP header name (RFC 7230 token), so a header map can never
/// inject a second header or a body separator.
pub fn validate_header_name(name: &str) -> Result<(), ProviderError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_HEADER_NAME_BYTES
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        });
    if !valid {
        return Err(ProviderError::Invalid(format!(
            "invalid HTTP header name \"{name}\""
        )));
    }
    Ok(())
}

/// Validate a complete upstream header map before it is encrypted.
pub fn validate_headers(headers: &BTreeMap<String, String>) -> Result<(), ProviderError> {
    if headers.len() > MAX_HEADERS {
        return Err(ProviderError::Invalid(format!(
            "at most {MAX_HEADERS} headers are accepted"
        )));
    }
    let mut seen: Vec<String> = Vec::with_capacity(headers.len());
    let mut total = 0usize;
    for (name, value) in headers {
        validate_header_name(name)?;
        let lowered = name.to_ascii_lowercase();
        if seen.contains(&lowered) {
            return Err(ProviderError::Invalid(format!(
                "the header \"{name}\" is declared twice (names are case-insensitive)"
            )));
        }
        seen.push(lowered);
        if value.chars().any(|c| c.is_control()) {
            return Err(ProviderError::Invalid(format!(
                "the value of header \"{name}\" contains a control character"
            )));
        }
        if value.len() > MAX_HEADER_VALUE_BYTES {
            return Err(ProviderError::Invalid(format!(
                "the value of header \"{name}\" must be at most {} KiB",
                MAX_HEADER_VALUE_BYTES / 1024
            )));
        }
        total += name.len() + value.len();
    }
    if total > MAX_HEADERS * (MAX_HEADER_NAME_BYTES + MAX_HEADER_VALUE_BYTES) {
        return Err(ProviderError::Invalid(
            "the header map is too large".to_string(),
        ));
    }
    Ok(())
}

/// Validate and normalise one provider default model.
pub fn validate_model(model: &str) -> Result<String, ProviderError> {
    let model = model.trim();
    if model.is_empty() {
        return Err(ProviderError::Invalid(
            "model must not be empty (omit it to clear the default)".to_string(),
        ));
    }
    if model.chars().count() > MAX_MODEL_NAME_CHARS {
        return Err(ProviderError::Invalid(format!(
            "model must be at most {MAX_MODEL_NAME_CHARS} characters"
        )));
    }
    if model.chars().any(|c| c.is_control()) {
        return Err(ProviderError::Invalid(
            "model must not contain control characters".to_string(),
        ));
    }
    Ok(model.to_string())
}

/// Encrypt one secret with the fleet key.
pub fn seal(cfg: &Config, secret: &str) -> Result<secrets::Sealed, ProviderError> {
    let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
    let mut map = BTreeMap::new();
    map.insert(SECRET_FIELD.to_string(), secret.to_string());
    Ok(secrets::encrypt_map(&key, &map)?)
}

/// Encrypt one validated upstream header map. An empty map stores nothing.
pub fn seal_headers(
    cfg: &Config,
    headers: &BTreeMap<String, String>,
) -> Result<Option<secrets::Sealed>, ProviderError> {
    if headers.is_empty() {
        return Ok(None);
    }
    validate_headers(headers)?;
    let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
    let bound: BTreeMap<String, String> = headers
        .iter()
        .map(|(name, value)| (format!("{HEADER_FIELD_PREFIX}{name}"), value.to_string()))
        .collect();
    Ok(Some(secrets::encrypt_map(&key, &bound)?))
}

/// Decrypt the secret of one provider. Only the driver and the test endpoint
/// call this; the value never leaves the control plane otherwise.
pub fn secret_of(cfg: &Config, record: &ProviderRecord) -> Result<String, ProviderError> {
    let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
    let mut map = secrets::decrypt_map(&key, &record.nonce, &record.ciphertext)?;
    map.remove(SECRET_FIELD).ok_or_else(|| {
        SecretsError::Decrypt(format!(
            "the encrypted provider value does not hold the field \"{SECRET_FIELD}\""
        ))
        .into()
    })
}

/// Decrypt the upstream headers of one provider. Never returned by the API.
pub fn headers_of(
    cfg: &Config,
    db: &Db,
    id: &str,
) -> Result<BTreeMap<String, String>, ProviderError> {
    let Some(sealed) = db.provider_headers(id)? else {
        return Ok(BTreeMap::new());
    };
    let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
    let stored = secrets::decrypt_map(&key, &sealed.nonce, &sealed.ciphertext)?;
    let mut headers = BTreeMap::new();
    for (name, value) in stored {
        let Some(name) = name.strip_prefix(HEADER_FIELD_PREFIX) else {
            return Err(SecretsError::Decrypt(
                "the encrypted provider headers are not bound to the header field".to_string(),
            )
            .into());
        };
        headers.insert(name.to_string(), value);
    }
    Ok(headers)
}

/// Host of a base URL (`https://opencode.ai/x` → `opencode.ai`), lowercased.
pub fn host_of(base_url: &str) -> Option<String> {
    let rest = base_url.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    let host = host.split(':').next().unwrap_or("");
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// `true` when the gateway behind this base URL requires [`SESSION_HEADER`].
///
/// The rule is the documented one: the host contains `opencode.ai` (the
/// published gateway is `opencode.ai`, and a regional or proxied host such as
/// `go.opencode.ai` keeps the same requirement).
pub fn requires_session_header(base_url: &str) -> bool {
    host_of(base_url).is_some_and(|host| host.contains(OPENCODE_HOST))
}

/// Sanitise a conversation id into a header-safe session value.
///
/// The value is stable for one conversation (the gateway routes by it) and
/// contains only `[A-Za-z0-9._:-]`; anything else becomes `-`, the result is
/// capped at [`MAX_SESSION_HEADER_CHARS`] and an empty or unusable id falls
/// back to `fleett-assistant`.
pub fn session_value(raw: &str) -> String {
    let mut value: String = raw
        .chars()
        .take(MAX_SESSION_HEADER_CHARS * 2)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    value.truncate(MAX_SESSION_HEADER_CHARS);
    if value.trim_matches('-').is_empty() {
        return "fleett-assistant".to_string();
    }
    value
}

/// The session header to add to one call, when the provider needs it and the
/// operator did not already configure it explicitly.
pub fn session_header(
    base_url: &str,
    headers: &BTreeMap<String, String>,
    session_id: &str,
) -> Option<(String, String)> {
    if !requires_session_header(base_url) {
        return None;
    }
    let explicit = headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case(SESSION_HEADER));
    (!explicit).then(|| (SESSION_HEADER.to_string(), session_value(session_id)))
}

/// API view of one provider.
pub fn summary(spec: &ProviderSpec, instances_count: usize) -> ProviderSummary {
    ProviderSummary {
        id: spec.id.clone(),
        name: spec.name.clone(),
        kind: spec.kind.clone(),
        base_url: spec.base_url.clone(),
        api_key_env: spec.api_key_env.clone(),
        model: spec.model.clone(),
        extra_headers: spec.header_names.clone(),
        instances_count,
        created_at: spec.created_at.clone(),
        updated_at: spec.updated_at.clone(),
    }
}

/// Every provider with its instance count, sorted by name (database order).
pub fn summaries(db: &Db) -> Result<Vec<ProviderSummary>, DbError> {
    let counts = db.provider_instance_counts()?;
    Ok(db
        .list_providers()?
        .iter()
        .map(|spec| {
            let count = counts.get(&spec.id).copied().unwrap_or(0);
            summary(spec, count)
        })
        .collect())
}

/// Instances attached to one provider, for `GET /api/providers/{id}`.
pub fn instances(db: &Db, provider_id: &str) -> Result<Vec<ProviderInstanceRef>, DbError> {
    Ok(db
        .provider_instances(provider_id)?
        .into_iter()
        .map(|(id, name)| ProviderInstanceRef { id, name })
        .collect())
}

/// Environment injected into `docker compose` for one instance.
///
/// Every stored variable, then the provider secret under the instance
/// `api_key_env` when a provider is attached (the provider always wins over a
/// per-instance value of that key). This is the single resolution point used
/// by the compose driver, so a rotation takes effect on the next `up -d`.
pub fn effective_env(
    cfg: &Config,
    db: &Db,
    spec: &crate::models::InstanceSpec,
) -> Result<BTreeMap<String, String>, ProviderError> {
    let mut values = envs::values(cfg, db, &spec.id)?;
    if let Some(provider_id) = spec.provider.as_deref() {
        let record = db
            .provider(provider_id)?
            .ok_or_else(|| ProviderError::NotFound(provider_id.to_string()))?;
        values.insert(spec.api_key_env.clone(), secret_of(cfg, &record)?);
    }
    Ok(values)
}

/// Same as [`effective_env`] for an instance id: a missing row (for example a
/// `docker compose down` during a migration) simply has no provider.
pub fn resolved_env(
    cfg: &Config,
    db: &Db,
    id: &str,
) -> Result<BTreeMap<String, String>, ProviderError> {
    match db.get(id)? {
        Some(spec) => effective_env(cfg, db, &spec),
        None => Ok(envs::values(cfg, db, id)?),
    }
}

/// One call of `POST /api/providers/{id}/test`.
#[derive(Debug, Clone)]
pub struct TestRequest<'a> {
    pub base_url: &'a str,
    pub secret: &'a str,
    /// Decrypted upstream headers, already validated.
    pub headers: &'a BTreeMap<String, String>,
    /// Model of the completion half.
    pub model: &'a str,
}

/// `POST /api/providers/{id}/test`: authenticate on `${base_url}/models` **and**
/// prove the route the assistant actually uses with a real 1-token
/// `POST /chat/completions` (issue #64).
///
/// A provider that answers `GET /models` but rejects the completion (a gateway
/// that demands a routing header, a model that does not exist) used to be
/// reported as healthy: the two results are now separate fields, so the console
/// can turn red on `completion_ok` alone. Neither call ever carries the secret
/// or a header value in the answer.
pub async fn test_provider(request: TestRequest<'_>) -> ProviderTestResponse {
    let started = Instant::now();
    let mut redaction: Vec<String> = vec![request.secret.to_string()];
    redaction.extend(request.headers.values().cloned());
    let redactor = crate::redact::Redactor::new(redaction);

    let client = match reqwest::Client::builder().timeout(TEST_TIMEOUT).build() {
        Ok(client) => client,
        Err(err) => {
            return ProviderTestResponse {
                ok: false,
                models_ok: false,
                completion_ok: false,
                latency_ms: 0,
                models_status: None,
                completion_status: None,
                error: Some(format!("cannot build the HTTP client: {err}")),
            }
        }
    };

    let base = request.base_url.trim_end_matches('/');
    let mut headers = auth_headers(request.secret);
    headers.insert("content-type".to_string(), "application/json".to_string());
    // The operator map wins over the defaults, exactly like the assistant.
    headers.extend(request.headers.clone());
    // The test is not a conversation: a fresh UUID is the right session id.
    if let Some((name, value)) = session_header(
        request.base_url,
        request.headers,
        &uuid::Uuid::new_v4().to_string(),
    ) {
        headers.insert(name, value);
    }

    let models = get_models(&client, &format!("{base}{TEST_PATH}"), &headers).await;
    let completion = post_completion(
        &client,
        &format!("{base}{COMPLETION_PATH}"),
        &headers,
        request.model,
    )
    .await;

    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let mut error: Option<String> = None;
    for (label, outcome) in [
        ("GET /models", &models),
        ("POST /chat/completions", &completion),
    ] {
        if let Some(detail) = &outcome.error {
            if error.is_none() {
                error = Some(format!(
                    "{label} failed: {}",
                    redactor.redact_capped(detail, MAX_UPSTREAM_ERROR_BYTES)
                ));
            }
        }
    }
    ProviderTestResponse {
        ok: models.ok && completion.ok,
        models_ok: models.ok,
        completion_ok: completion.ok,
        latency_ms,
        models_status: models.status,
        completion_status: completion.status,
        error,
    }
}

/// Outcome of one half of the test.
struct Probe {
    ok: bool,
    status: Option<u16>,
    /// Raw reason on failure (status line or transport error), pre-redaction.
    error: Option<String>,
}

/// Headers every provider call carries, whatever the kind.
///
/// Both `Authorization: Bearer` and `x-api-key` (plus the Anthropic version
/// header) are sent so one implementation covers the OpenAI-style and
/// Anthropic-style deployments without knowing the kind.
fn auth_headers(secret: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("authorization".to_string(), format!("Bearer {secret}")),
        ("x-api-key".to_string(), secret.to_string()),
        ("anthropic-version".to_string(), "2023-06-01".to_string()),
    ])
}

async fn get_models(
    client: &reqwest::Client,
    url: &str,
    headers: &BTreeMap<String, String>,
) -> Probe {
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            let detail = if status.is_success() {
                None
            } else {
                let body = read_body_capped(response, MAX_UPSTREAM_ERROR_BYTES).await;
                Some(format_body(status.as_u16(), &body))
            };
            Probe {
                ok: status.is_success(),
                status: Some(status.as_u16()),
                error: detail,
            }
        }
        Err(err) => Probe {
            ok: false,
            status: None,
            error: Some(transport_error(&err)),
        },
    }
}

async fn post_completion(
    client: &reqwest::Client,
    url: &str,
    headers: &BTreeMap<String, String>,
    model: &str,
) -> Probe {
    let mut request = client.post(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "temperature": 0.0,
        "max_tokens": TEST_MAX_TOKENS,
        "stream": false,
    });
    // `.json()` would add its own content-type on top of an override in
    // `extra_headers`; the header map already carries exactly one.
    let payload = match serde_json::to_vec(&body) {
        Ok(payload) => payload,
        Err(err) => {
            return Probe {
                ok: false,
                status: None,
                error: Some(format!("cannot serialise the test request: {err}")),
            }
        }
    };
    match request.body(payload).send().await {
        Ok(response) => {
            let status = response.status();
            let detail = if status.is_success() {
                None
            } else {
                let body = read_body_capped(response, MAX_UPSTREAM_ERROR_BYTES).await;
                Some(format_body(status.as_u16(), &body))
            };
            Probe {
                ok: status.is_success(),
                status: Some(status.as_u16()),
                error: detail,
            }
        }
        Err(err) => Probe {
            ok: false,
            status: None,
            error: Some(transport_error(&err)),
        },
    }
}

/// Read at most `max` bytes of a response body, as UTF-8.
///
/// Shared by the provider test and the assistant: a gateway error is worth
/// quoting (it names the missing header or the unknown model), a whole HTML
/// error page is not.
pub async fn read_body_capped(mut response: reqwest::Response, max: usize) -> String {
    let mut body = Vec::new();
    loop {
        let room = max.saturating_sub(body.len());
        if room == 0 {
            break;
        }
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk[..chunk.len().min(room)]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&body).trim().to_string()
}

/// `HTTP 400: <body>` — the shape both the test and the assistant report.
pub fn format_body(status: u16, body: &str) -> String {
    if body.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {body}")
    }
}

fn transport_error(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        format!(
            "the provider did not answer within {}s",
            TEST_TIMEOUT.as_secs()
        )
    } else {
        format!("cannot reach the provider: {err}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::models::{now_iso, ChannelKind, InstanceSpec, Status};
    use crate::store;

    const KEY_BYTES: [u8; 32] = [9u8; 32];

    fn setup() -> (tempfile::TempDir, Config, Db) {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().join("root"));
        store::ensure_layout(&cfg).expect("layout");
        cfg.secrets_key = Some(crate::secrets::encode_key(&KEY_BYTES));
        let db = Db::open(&cfg.db_path()).expect("db");
        (dir, cfg, db)
    }

    fn provider(id: &str) -> ProviderSpec {
        let now = now_iso();
        ProviderSpec {
            id: id.to_string(),
            name: id.to_string(),
            kind: "openai-compatible".to_string(),
            base_url: "https://provider.test/v1".to_string(),
            api_key_env: "PROVIDER_KEY".to_string(),
            model: None,
            header_names: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    fn instance(id: &str, provider: Option<&str>) -> InstanceSpec {
        let now = now_iso();
        InstanceSpec {
            id: id.to_string(),
            name: id.to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://provider.test/v1".to_string(),
            api_key_env: "PROVIDER_KEY".to_string(),
            provider: provider.map(str::to_string),
            channel: ChannelKind::None,
            shares: Vec::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            mem_limit_mb: 512,
            cpu_limit: 1.0,
            image: "agent:1".to_string(),
            previous_image: None,
            allow_lan: false,
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            disk_quota_mb: 0,
            disk_policy: crate::models::DiskPolicy::Warn,
            notes: String::new(),
            host: crate::hosts::LOCAL_HOST.to_string(),
            status: Status::Created,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    #[test]
    fn the_secret_round_trips_and_needs_the_key() {
        let (_dir, cfg, db) = setup();
        let sealed = seal(&cfg, "sk-test").expect("seal");
        db.insert_provider(&provider("shared"), &sealed.nonce, &sealed.ciphertext)
            .expect("insert");

        let record = db.provider("shared").expect("read").expect("present");
        assert_eq!(secret_of(&cfg, &record).expect("decrypt"), "sk-test");

        // A wrong key is refused (GCM tag), never returned as plaintext.
        let mut other = cfg.clone();
        other.secrets_key = Some(crate::secrets::encode_key(&[3u8; 32]));
        assert!(secret_of(&other, &record).is_err());

        // Without a key the secret cannot be stored at all.
        let mut keyless = cfg.clone();
        keyless.secrets_key = None;
        keyless.secrets_key_file = std::path::PathBuf::from("/nonexistent/fleett.key");
        assert!(matches!(
            seal(&keyless, "sk-test"),
            Err(ProviderError::Secrets(SecretsError::MissingKey))
        ));
    }

    #[test]
    fn a_rotated_secret_gets_a_new_nonce_and_wins_in_the_effective_env() {
        let (_dir, cfg, db) = setup();
        let first = seal(&cfg, "one").expect("seal");
        db.insert_provider(&provider("shared"), &first.nonce, &first.ciphertext)
            .expect("insert");
        db.insert_instance(&instance("alice", Some("shared")))
            .expect("instance");

        let env = resolved_env(&cfg, &db, "alice").expect("env");
        assert_eq!(env.get("PROVIDER_KEY").map(String::as_str), Some("one"));

        let second = seal(&cfg, "two").expect("seal");
        let mut spec = provider("shared");
        spec.updated_at = now_iso();
        db.update_provider(&spec, Some((&second.nonce, &second.ciphertext)))
            .expect("rotate");

        let record = db.provider("shared").expect("read").expect("present");
        assert_ne!(record.nonce, first.nonce);
        let env = resolved_env(&cfg, &db, "alice").expect("env");
        assert_eq!(env.get("PROVIDER_KEY").map(String::as_str), Some("two"));
    }

    #[test]
    fn a_legacy_instance_keeps_its_own_values() {
        let (_dir, cfg, db) = setup();
        db.insert_instance(&instance("legacy", None))
            .expect("instance");
        envs::replace(
            &cfg,
            &db,
            "legacy",
            &[envs::EnvWrite {
                key: "PROVIDER_KEY".to_string(),
                value: "per-instance".to_string(),
                secret: true,
            }],
        )
        .expect("env");

        let env = resolved_env(&cfg, &db, "legacy").expect("env");
        assert_eq!(
            env.get("PROVIDER_KEY").map(String::as_str),
            Some("per-instance")
        );
    }

    #[test]
    fn summaries_count_the_attached_instances() {
        let (_dir, cfg, db) = setup();
        let sealed = seal(&cfg, "sk-test").expect("seal");
        db.insert_provider(&provider("shared"), &sealed.nonce, &sealed.ciphertext)
            .expect("insert");
        db.insert_instance(&instance("alice", Some("shared")))
            .expect("alice");
        db.insert_instance(&instance("bob", Some("shared")))
            .expect("bob");
        db.insert_instance(&instance("legacy", None))
            .expect("legacy");

        let list = summaries(&db).expect("summaries");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].instances_count, 2);
        let attached = instances(&db, "shared").expect("instances");
        assert_eq!(
            attached
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["alice", "bob"]
        );
    }
    #[test]
    fn header_maps_are_validated() {
        let headers = BTreeMap::from([
            ("x-opencode-session".to_string(), "abc".to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ]);
        validate_headers(&headers).expect("valid");

        // RFC token names, no control character in the value.
        for bad in [
            BTreeMap::from([("bad name".to_string(), "x".to_string())]),
            BTreeMap::from([("x".to_string(), "a\r\nb".to_string())]),
            BTreeMap::from([("".to_string(), "x".to_string())]),
            BTreeMap::from([("x".to_string(), "x".repeat(MAX_HEADER_VALUE_BYTES + 1))]),
        ] {
            assert!(validate_headers(&bad).is_err(), "{bad:?}");
        }

        // Names are unique case-insensitively.
        let duplicate = BTreeMap::from([
            ("x-api-version".to_string(), "1".to_string()),
            ("X-Api-Version".to_string(), "2".to_string()),
        ]);
        assert!(validate_headers(&duplicate).is_err());

        // At most 32 headers.
        let many: BTreeMap<String, String> = (0..=MAX_HEADERS)
            .map(|index| (format!("x-h{index}"), "1".to_string()))
            .collect();
        assert!(validate_headers(&many).is_err());

        // A model is trimmed and bounded.
        assert_eq!(validate_model("  gpt-4  ").expect("model"), "gpt-4");
        assert!(validate_model("   ").is_err());
        assert!(validate_model(&"m".repeat(MAX_MODEL_NAME_CHARS + 1)).is_err());
    }

    #[test]
    fn headers_round_trip_encrypted_and_only_names_are_exposed() {
        let (_dir, cfg, db) = setup();
        let mut spec = provider("shared");
        let headers = BTreeMap::from([
            ("x-opencode-session".to_string(), "session-1".to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ]);
        spec.header_names = headers.keys().cloned().collect();
        spec.model = Some("model-a".to_string());
        let sealed = seal(&cfg, "sk-test").expect("seal");
        db.insert_provider(&spec, &sealed.nonce, &sealed.ciphertext)
            .expect("insert");
        let sealed_headers = seal_headers(&cfg, &headers)
            .expect("seal")
            .expect("non-empty");
        db.set_provider_headers(&spec.id, Some(&sealed_headers))
            .expect("store headers");

        assert_eq!(headers_of(&cfg, &db, "shared").expect("read"), headers);
        let stored = db
            .provider_headers("shared")
            .expect("blob")
            .expect("present");
        let ciphertext = String::from_utf8_lossy(&stored.ciphertext).to_string();
        assert!(!ciphertext.contains("2023-06-01"), "{ciphertext}");

        // The summary exposes names only, never a value.
        let view = serde_json::to_value(summary(&spec, 0)).expect("json");
        assert_eq!(
            view["extra_headers"],
            serde_json::json!(["anthropic-version", "x-opencode-session"])
        );
        assert_eq!(view["model"], "model-a");
        assert!(!view.to_string().contains("2023-06-01"));

        // A secret blob moved into the headers column fails the binding check.
        db.set_provider_headers(&spec.id, Some(&sealed))
            .expect("swap");
        assert!(headers_of(&cfg, &db, "shared").is_err());

        // An empty map clears the blob.
        assert!(seal_headers(&cfg, &BTreeMap::new())
            .expect("empty")
            .is_none());
        db.set_provider_headers(&spec.id, None).expect("clear");
        assert!(headers_of(&cfg, &db, "shared").expect("read").is_empty());
    }

    #[test]
    fn the_opencode_session_header_is_added_only_where_it_is_needed() {
        assert_eq!(
            host_of("https://opencode.ai/inference/go/openai/v1").as_deref(),
            Some("opencode.ai")
        );
        assert_eq!(
            host_of("http://user:pw@opencode.ai:8443/v1").as_deref(),
            Some("opencode.ai")
        );
        assert_eq!(host_of("not a url"), None);
        assert!(requires_session_header(
            "https://opencode.ai/inference/go/openai/v1"
        ));
        assert!(requires_session_header("https://go.opencode.ai/v1"));
        assert!(!requires_session_header("https://api.example.test/v1"));
        // The rule is "the host contains opencode.ai".
        assert!(requires_session_header(
            "https://opencode.ai.example.test/v1"
        ));
        // The path never makes a host: only the authority is read.
        assert!(!requires_session_header(
            "https://gateway.example.test/opencode.ai/v1"
        ));

        // The session id is stable per conversation and header-safe.
        assert_eq!(session_value("panel 7f/3c"), "panel-7f-3c");
        assert_eq!(session_value(""), "fleett-assistant");
        assert_eq!(session_value("///"), "fleett-assistant");
        assert_eq!(
            session_value(&"a".repeat(MAX_SESSION_HEADER_CHARS + 10)).len(),
            MAX_SESSION_HEADER_CHARS
        );

        let headers = BTreeMap::new();
        let (name, value) = session_header(
            "https://opencode.ai/inference/go/openai/v1",
            &headers,
            "conv-1",
        )
        .expect("session header");
        assert_eq!(name, SESSION_HEADER);
        assert_eq!(value, "conv-1");

        // An explicit header disables the automatic one.
        let explicit = BTreeMap::from([(SESSION_HEADER.to_string(), "fixed".to_string())]);
        assert!(session_header(
            "https://opencode.ai/inference/go/openai/v1",
            &explicit,
            "conv-1"
        )
        .is_none());
        // Another provider never gets one.
        assert!(session_header("https://api.example.test/v1", &headers, "conv-1").is_none());
    }
}
