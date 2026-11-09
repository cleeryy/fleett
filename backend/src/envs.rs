//! Editable per-instance environment variables.
//!
//! Instances used to receive their whole environment once, at creation, and
//! every value went into the encrypted `instance_secrets` blob. An allowlist
//! (say `DISCORD_ALLOWED_USERS`) could not be added to a running agent without
//! recreating it.
//!
//! Each variable now lives in its own `instance_env` row, with an explicit
//! secret flag:
//!
//! * `secret = 1` stores only an AES-256-GCM blob (the same key as
//!   [`crate::secrets`], one nonce per value) and never a plaintext value;
//! * `secret = 0` stores the plaintext value and no ciphertext.
//!
//! The value is encrypted together with its variable name (a single-entry JSON
//! map), so a ciphertext moved to another row fails to decrypt instead of
//! silently changing another variable.
//!
//! `GET /api/instances/{id}` never returns the value of a secret entry. The
//! compose file only ever declares `${KEY}`; the compose driver injects the
//! decrypted secrets and the plaintext values into the `docker compose` child
//! environment.
//!
//! ## Legacy stores
//!
//! Two older stores are read as a fallback and superseded by the first write:
//!
//! * `instance_secrets` (one encrypted JSON map per instance) is imported at
//!   startup by [`migrate_sealed`]; the table is kept for one release so a
//!   downgrade can still read it, and its rows are dropped as they are
//!   imported;
//! * `instances/<id>/secrets.env` (plaintext) is still injected by the driver
//!   and imported by `fleett secrets migrate --apply`, as documented in
//!   `docs/SECRETS.md`. Writing through the API requires a configured key and
//!   removes the file, so a stale token can never come back.

use crate::config::Config;
use crate::db::{Db, DbError, InstanceEnvRecord};
use crate::models::{now_iso, validate_env_key, validate_env_raw_value, EnvEntry};
use crate::secrets::{self, SecretsError, SecretsKey};
use crate::store;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Secrets(#[from] SecretsError),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// One environment variable to store, with its final value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvWrite {
    pub key: String,
    pub value: String,
    pub secret: bool,
}

/// One environment variable of an instance, cleartext (control-plane side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvValue {
    pub key: String,
    pub value: String,
    pub secret: bool,
    pub updated_at: String,
}

/// Environment entries of one instance, secrets decrypted, sorted by key.
///
/// Rows win over the legacy stores. When the instance has no row at all, the
/// encrypted `instance_secrets` blob is decrypted (it needs the key), and
/// otherwise the plaintext `secrets.env` file is read.
pub fn load(cfg: &Config, db: &Db, id: &str) -> Result<Vec<EnvValue>, EnvError> {
    let records = db.list_instance_env(id)?;
    if !records.is_empty() {
        return decrypt_records(cfg, records).map_err(EnvError::from);
    }

    if let Some(row) = db.sealed(id)? {
        let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
        let map = secrets::decrypt_map(&key, &row.nonce, &row.ciphertext)?;
        return Ok(map
            .into_iter()
            .map(|(key, value)| EnvValue {
                key,
                value,
                secret: true,
                updated_at: row.updated_at.clone(),
            })
            .collect());
    }

    let updated_at = legacy_file_timestamp(cfg, id);
    Ok(store::read_legacy_secrets(cfg, id)
        .into_iter()
        .map(|(key, value)| EnvValue {
            key,
            value,
            secret: true,
            updated_at: updated_at.clone(),
        })
        .collect())
}

/// Values of every variable, secrets decrypted: what the compose driver
/// injects into the `docker compose` child environment.
pub fn values(cfg: &Config, db: &Db, id: &str) -> Result<BTreeMap<String, String>, EnvError> {
    Ok(load(cfg, db, id)?
        .into_iter()
        .map(|entry| (entry.key, entry.value))
        .collect())
}

/// Sorted names of every variable of one instance.
pub fn keys(cfg: &Config, db: &Db, id: &str) -> Result<Vec<String>, EnvError> {
    Ok(load(cfg, db, id)?
        .into_iter()
        .map(|entry| entry.key)
        .collect())
}

/// API view: a secret value is never included.
pub fn views(cfg: &Config, db: &Db, id: &str) -> Result<Vec<EnvEntry>, EnvError> {
    Ok(load(cfg, db, id)?
        .into_iter()
        .map(|entry| EnvEntry {
            key: entry.key,
            secret: entry.secret,
            updated_at: entry.updated_at,
            value: (!entry.secret).then_some(entry.value),
        })
        .collect())
}

/// Fail early when a write could not store its secrets.
pub fn check_storable(cfg: &Config, entries: &[EnvWrite]) -> Result<(), EnvError> {
    if !entries.iter().any(|entry| entry.secret) {
        return Ok(());
    }
    if SecretsKey::from_config(cfg)?.is_none() {
        return Err(SecretsError::MissingKey.into());
    }
    Ok(())
}

/// Validate and store the complete environment of one instance.
///
/// The old encrypted blob and the plaintext `secrets.env` file of the instance
/// are removed: the row set becomes the only source of values.
pub fn replace(cfg: &Config, db: &Db, id: &str, entries: &[EnvWrite]) -> Result<(), EnvError> {
    validate_entries(entries)?;
    store_rows(cfg, db, id, entries)?;
    remove_legacy_file(cfg, id);
    Ok(())
}

/// Store the row set without touching the legacy plaintext file.
///
/// Used by `fleett secrets migrate`: the file is deleted by the migration
/// itself (and only with `--apply`).
pub fn store_rows(cfg: &Config, db: &Db, id: &str, entries: &[EnvWrite]) -> Result<(), EnvError> {
    let key = if entries.iter().any(|entry| entry.secret) {
        Some(SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?)
    } else {
        None
    };
    let now = now_iso();
    let mut records = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.secret {
            let key = key.as_ref().expect("a secret entry resolved the key above");
            let sealed = encrypt_value(key, &entry.key, &entry.value)?;
            records.push(InstanceEnvRecord {
                key: entry.key.clone(),
                secret: true,
                value: None,
                nonce: Some(sealed.nonce),
                ciphertext: Some(sealed.ciphertext),
                updated_at: now.clone(),
            });
        } else {
            records.push(InstanceEnvRecord {
                key: entry.key.clone(),
                secret: false,
                value: Some(entry.value.clone()),
                nonce: None,
                ciphertext: None,
                updated_at: now.clone(),
            });
        }
    }
    db.replace_instance_env(id, &records)?;
    Ok(())
}

/// Import every `instance_secrets` map into `instance_env` (`secret = 1`).
///
/// Idempotent: an instance that already has rows keeps them and only loses the
/// superseded blob. Returns the number of instances imported. Without a key
/// the blobs stay in place (and remain readable through [`load`]) with a
/// warning.
pub fn migrate_sealed(cfg: &Config, db: &Db) -> usize {
    let ids = match db.sealed_ids() {
        Ok(ids) => ids,
        Err(err) => {
            tracing::warn!(%err, "env: cannot list the legacy encrypted secrets");
            return 0;
        }
    };
    if ids.is_empty() {
        return 0;
    }
    let key = match SecretsKey::from_config(cfg) {
        Ok(Some(key)) => key,
        Ok(None) => {
            tracing::warn!(
                instances = ids.len(),
                "env: encrypted secrets still live in instance_secrets and no key is \
                 configured; run `sudo fleett secrets init` (or restore the key) to import them"
            );
            return 0;
        }
        Err(err) => {
            tracing::warn!(%err, "env: the configured secrets key is unusable");
            return 0;
        }
    };

    let mut imported = 0;
    for id in ids {
        match db.list_instance_env(&id) {
            Ok(rows) if !rows.is_empty() => {
                // Already migrated: the rows are authoritative.
                if let Err(err) = db.delete_sealed(&id) {
                    tracing::warn!(instance = %id, %err, "env: cannot drop the legacy secrets row");
                }
                continue;
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(instance = %id, %err, "env: cannot read the instance environment");
                continue;
            }
        }
        let Some(row) = db.sealed(&id).ok().flatten() else {
            continue;
        };
        let entries = match secrets::decrypt_map(&key, &row.nonce, &row.ciphertext) {
            Ok(map) => map
                .into_iter()
                .map(|(key, value)| EnvWrite {
                    key,
                    value,
                    secret: true,
                })
                .collect::<Vec<_>>(),
            Err(err) => {
                tracing::warn!(
                    instance = %id,
                    %err,
                    "env: cannot decrypt the legacy secrets row; it is left in place"
                );
                continue;
            }
        };
        match store_rows(cfg, db, &id, &entries) {
            Ok(()) => {
                imported += 1;
                tracing::info!(
                    instance = %id,
                    keys = entries.len(),
                    "env: imported the legacy instance_secrets map"
                );
            }
            Err(err) => tracing::warn!(
                instance = %id,
                %err,
                "env: cannot import the legacy secrets row; it is left in place"
            ),
        }
    }
    imported
}

/// Reject a request that could never be stored or rendered.
pub fn validate_entries(entries: &[EnvWrite]) -> Result<(), EnvError> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        validate_env_key(&entry.key).map_err(EnvError::Invalid)?;
        validate_env_raw_value(&entry.value).map_err(EnvError::Invalid)?;
        if !seen.insert(entry.key.as_str()) {
            return Err(EnvError::Invalid(format!(
                "environment variable \"{}\" is listed twice",
                entry.key
            )));
        }
    }
    Ok(())
}

fn decrypt_records(
    cfg: &Config,
    records: Vec<InstanceEnvRecord>,
) -> Result<Vec<EnvValue>, SecretsError> {
    let mut cache: Option<SecretsKey> = None;
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let updated_at = record.updated_at;
        let value = if record.secret {
            if cache.is_none() {
                cache = Some(SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?);
            }
            let key = cache.as_ref().expect("the key was resolved just above");
            let nonce = record
                .nonce
                .ok_or_else(|| SecretsError::Decrypt("missing nonce".to_string()))?;
            let ciphertext = record
                .ciphertext
                .ok_or_else(|| SecretsError::Decrypt("missing ciphertext".to_string()))?;
            decrypt_value(key, &record.key, &nonce, &ciphertext)?
        } else {
            record.value.ok_or_else(|| {
                SecretsError::Decrypt(format!(
                    "the non-secret variable \"{}\" has no value",
                    record.key
                ))
            })?
        };
        out.push(EnvValue {
            key: record.key,
            value,
            secret: record.secret,
            updated_at,
        });
    }
    Ok(out)
}

fn encrypt_value(
    key: &SecretsKey,
    name: &str,
    value: &str,
) -> Result<secrets::Sealed, SecretsError> {
    let mut map = BTreeMap::new();
    map.insert(name.to_string(), value.to_string());
    secrets::encrypt_map(key, &map)
}

fn decrypt_value(
    key: &SecretsKey,
    name: &str,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<String, SecretsError> {
    let mut map = secrets::decrypt_map(key, nonce, ciphertext)?;
    map.remove(name).ok_or_else(|| {
        SecretsError::Decrypt(format!(
            "the encrypted value does not hold the variable \"{name}\""
        ))
    })
}

/// Drop the plaintext `secrets.env` of an instance once its rows supersede it.
fn remove_legacy_file(cfg: &Config, id: &str) {
    let path = cfg.secrets_path(id);
    match std::fs::remove_file(&path) {
        Ok(()) => tracing::info!(
            path = %path.display(),
            "env: removed the plaintext secrets file superseded by the instance env"
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(
            path = %path.display(),
            %err,
            "env: cannot remove the superseded plaintext secrets file"
        ),
    }
}

fn legacy_file_timestamp(cfg: &Config, id: &str) -> String {
    std::fs::metadata(cfg.secrets_path(id))
        .and_then(|metadata| metadata.modified())
        .map(|time| {
            chrono::DateTime::<chrono::Utc>::from(time)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
        .unwrap_or_else(|_| now_iso())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_BYTES: [u8; secrets::KEY_LEN] = [7u8; secrets::KEY_LEN];

    fn setup() -> (tempfile::TempDir, Config, Db) {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        cfg.secrets_key = Some(secrets::encode_key(&KEY_BYTES));
        let db = Db::open(&cfg.db_path()).expect("db");
        (dir, cfg, db)
    }

    fn write(key: &str, value: &str, secret: bool) -> EnvWrite {
        EnvWrite {
            key: key.to_string(),
            value: value.to_string(),
            secret,
        }
    }

    #[test]
    fn values_round_trip_and_only_secrets_are_encrypted() {
        let (_dir, cfg, db) = setup();
        let entries = vec![
            write("DISCORD_ALLOWED_USERS", "123", false),
            write("DISCORD_BOT_TOKEN", "bot-secret", true),
        ];
        replace(&cfg, &db, "alice", &entries).expect("replace");

        let values = values(&cfg, &db, "alice").expect("values");
        assert_eq!(
            values.get("DISCORD_ALLOWED_USERS").map(String::as_str),
            Some("123")
        );
        assert_eq!(
            values.get("DISCORD_BOT_TOKEN").map(String::as_str),
            Some("bot-secret")
        );

        // The non-secret row is readable in the database; the secret row is
        // not, and carries a ciphertext instead.
        let rows = db.list_instance_env("alice").expect("rows");
        let allowed = rows
            .iter()
            .find(|row| row.key == "DISCORD_ALLOWED_USERS")
            .expect("plain row");
        assert_eq!(allowed.value.as_deref(), Some("123"));
        assert!(allowed.ciphertext.is_none());
        let token = rows
            .iter()
            .find(|row| row.key == "DISCORD_BOT_TOKEN")
            .expect("secret row");
        assert!(token.value.is_none());
        assert!(token.ciphertext.is_some());
        assert!(
            !String::from_utf8_lossy(token.ciphertext.as_ref().expect("blob"))
                .contains("bot-secret")
        );

        // The API view hides the secret value and returns the plaintext one.
        let views = views(&cfg, &db, "alice").expect("views");
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].key, "DISCORD_ALLOWED_USERS");
        assert_eq!(views[0].value.as_deref(), Some("123"));
        assert_eq!(views[1].key, "DISCORD_BOT_TOKEN");
        assert!(views[1].secret);
        assert!(views[1].value.is_none());
    }

    #[test]
    fn a_secret_needs_the_key_but_a_plain_value_does_not() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        let db = Db::open(&cfg.db_path()).expect("db");

        replace(
            &cfg,
            &db,
            "alice",
            &[write("DISCORD_ALLOWED_USERS", "123", false)],
        )
        .expect("plain value without a key");
        assert!(matches!(
            replace(
                &cfg,
                &db,
                "alice",
                &[write("DISCORD_BOT_TOKEN", "secret", true)]
            ),
            Err(EnvError::Secrets(SecretsError::MissingKey))
        ));
        assert!(matches!(
            check_storable(&cfg, &[write("TOKEN", "x", true)]),
            Err(EnvError::Secrets(SecretsError::MissingKey))
        ));
        assert!(check_storable(&cfg, &[write("DISCORD_ALLOWED_USERS", "123", false)]).is_ok());
    }

    #[test]
    fn a_new_nonce_is_used_for_every_rotation() {
        let (_dir, cfg, db) = setup();
        replace(&cfg, &db, "alice", &[write("TOKEN", "one", true)]).expect("replace");
        let first = db.list_instance_env("alice").expect("rows").remove(0);
        replace(&cfg, &db, "alice", &[write("TOKEN", "two", true)]).expect("replace");
        let second = db.list_instance_env("alice").expect("rows").remove(0);
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.ciphertext, second.ciphertext);
        assert_eq!(
            values(&cfg, &db, "alice").expect("values").get("TOKEN"),
            Some(&"two".to_string())
        );
    }

    #[test]
    fn a_ciphertext_moved_to_another_row_is_rejected() {
        let (_dir, cfg, db) = setup();
        replace(
            &cfg,
            &db,
            "alice",
            &[write("TOKEN_A", "a", true), write("TOKEN_B", "b", true)],
        )
        .expect("replace");
        let mut rows = db.list_instance_env("alice").expect("rows");
        let a = rows.remove(0);
        let b = rows.remove(0);
        let swapped = InstanceEnvRecord {
            key: a.key.clone(),
            secret: true,
            value: None,
            nonce: b.nonce,
            ciphertext: b.ciphertext,
            updated_at: a.updated_at,
        };
        db.replace_instance_env("alice", &[swapped]).expect("write");
        let err = values(&cfg, &db, "alice").expect_err("swapped blob");
        assert!(err.to_string().contains("TOKEN_A"), "{err}");
    }

    #[test]
    fn invalid_and_reserved_names_are_refused() {
        let (_dir, cfg, db) = setup();
        for entry in [
            write("lower_case", "x", false),
            write("HERMES_UID", "1", false),
            write("FLEETT_ROOT", "/tmp", false),
            write("PATH", "/bin", false),
        ] {
            assert!(matches!(
                replace(&cfg, &db, "alice", std::slice::from_ref(&entry)),
                Err(EnvError::Invalid(_))
            ));
        }
        assert!(matches!(
            replace(
                &cfg,
                &db,
                "alice",
                &[write("A", "x", false), write("A", "y", false)]
            ),
            Err(EnvError::Invalid(_))
        ));
        assert!(matches!(
            replace(
                &cfg,
                &db,
                "alice",
                &[write("A", &"x".repeat(8 * 1024 + 1), false)]
            ),
            Err(EnvError::Invalid(_))
        ));
    }

    #[test]
    fn the_legacy_encrypted_map_is_imported_once() {
        let (_dir, cfg, db) = setup();
        let mut map = BTreeMap::new();
        map.insert("TELEGRAM_BOT_TOKEN".to_string(), "123:ABC".to_string());
        map.insert("HERMES_GATE_PROFILE_TOKEN".to_string(), "gate".to_string());
        let sealed = secrets::encrypt_map(
            &SecretsKey::new(KEY_BYTES, secrets::KeySource::Environment),
            &map,
        )
        .expect("encrypt");
        db.put_sealed("alice", &sealed.nonce, &sealed.ciphertext)
            .expect("sealed row");

        // Readable through the fallback before the migration runs.
        assert_eq!(values(&cfg, &db, "alice").expect("values").len(), 2);
        let views = views(&cfg, &db, "alice").expect("views");
        assert!(views
            .iter()
            .all(|entry| entry.secret && entry.value.is_none()));

        assert_eq!(migrate_sealed(&cfg, &db), 1);
        assert!(db.sealed("alice").expect("sealed").is_none());
        let rows = db.list_instance_env("alice").expect("rows");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.secret));
        assert_eq!(
            values(&cfg, &db, "alice")
                .expect("values")
                .get("TELEGRAM_BOT_TOKEN")
                .map(String::as_str),
            Some("123:ABC")
        );

        // Idempotent, and a stale blob never overwrites the rows.
        assert_eq!(migrate_sealed(&cfg, &db), 0);
    }

    #[test]
    fn an_api_write_supersedes_the_legacy_sources() {
        let (_dir, cfg, db) = setup();
        store::write_legacy_secrets(
            &cfg,
            "alice",
            &BTreeMap::from([("TELEGRAM_BOT_TOKEN".to_string(), "123:ABC".to_string())]),
        )
        .expect("legacy file");
        assert_eq!(values(&cfg, &db, "alice").expect("values").len(), 1);

        replace(
            &cfg,
            &db,
            "alice",
            &[write("DISCORD_ALLOWED_USERS", "1", false)],
        )
        .expect("replace");
        assert!(!cfg.secrets_path("alice").exists());
        let current = values(&cfg, &db, "alice").expect("values");
        assert!(!current.contains_key("TELEGRAM_BOT_TOKEN"));
        assert_eq!(
            current.get("DISCORD_ALLOWED_USERS").map(String::as_str),
            Some("1")
        );

        // Clearing every row stays empty: the legacy file is gone for good.
        replace(&cfg, &db, "alice", &[]).expect("clear");
        assert!(values(&cfg, &db, "alice").expect("values").is_empty());
    }

    #[test]
    fn a_legacy_plaintext_file_is_read_without_a_key() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        let db = Db::open(&cfg.db_path()).expect("db");
        store::write_legacy_secrets(
            &cfg,
            "alice",
            &BTreeMap::from([("TELEGRAM_BOT_TOKEN".to_string(), "111:OLD".to_string())]),
        )
        .expect("legacy file");

        let views = views(&cfg, &db, "alice").expect("views");
        assert_eq!(views.len(), 1);
        assert!(views[0].secret);
        assert!(views[0].value.is_none());
        assert_eq!(
            values(&cfg, &db, "alice")
                .expect("values")
                .get("TELEGRAM_BOT_TOKEN"),
            Some(&"111:OLD".to_string())
        );
    }
}
