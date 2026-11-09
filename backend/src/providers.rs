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
//! * no endpoint ever returns it — [`ProviderSummary`] has no secret field and
//!   the probe/test response only reports status, latency and an error string;
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
/// Path probed by `POST /api/providers/{id}/test`.
pub const TEST_PATH: &str = "/models";
/// Hard timeout of one connectivity test.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(5);
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

/// Encrypt one secret with the fleet key.
pub fn seal(cfg: &Config, secret: &str) -> Result<secrets::Sealed, ProviderError> {
    let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
    let mut map = BTreeMap::new();
    map.insert(SECRET_FIELD.to_string(), secret.to_string());
    Ok(secrets::encrypt_map(&key, &map)?)
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

/// API view of one provider.
pub fn summary(spec: &ProviderSpec, instances_count: usize) -> ProviderSummary {
    ProviderSummary {
        id: spec.id.clone(),
        name: spec.name.clone(),
        kind: spec.kind.clone(),
        base_url: spec.base_url.clone(),
        api_key_env: spec.api_key_env.clone(),
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

/// `POST /api/providers/{id}/test`: call `${base_url}/models` with the
/// decrypted key and report the outcome. The key never appears in the answer.
///
/// The request carries both `Authorization: Bearer` and `x-api-key` (plus the
/// Anthropic version header) so one implementation covers the OpenAI-style and
/// Anthropic-style deployments without knowing the kind.
pub async fn probe(base_url: &str, secret: &str) -> ProviderTestResponse {
    let url = format!("{}{TEST_PATH}", base_url.trim_end_matches('/'));
    let started = Instant::now();
    let outcome = async {
        let client = reqwest::Client::builder()
            .timeout(TEST_TIMEOUT)
            .build()
            .map_err(|err| format!("cannot build the HTTP client: {err}"))?;
        client
            .get(&url)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {secret}"))
            .header("x-api-key", secret)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    format!(
                        "the provider did not answer within {}s",
                        TEST_TIMEOUT.as_secs()
                    )
                } else {
                    format!("cannot reach the provider: {err}")
                }
            })
    }
    .await;

    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    match outcome {
        Ok(response) => {
            let status = response.status().as_u16();
            ProviderTestResponse {
                ok: response.status().is_success(),
                status,
                latency_ms,
                error: (!response.status().is_success())
                    .then(|| format!("the provider answered with HTTP {status}")),
            }
        }
        Err(error) => ProviderTestResponse {
            ok: false,
            status: 0,
            latency_ms,
            error: Some(error),
        },
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
}
