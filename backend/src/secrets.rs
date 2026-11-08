//! Instance secrets encrypted at rest.
//!
//! Provider keys and bot tokens used to live in `instances/<id>/secrets.env`,
//! mode `0600` but plain text: a `tar` of `FLEETT_ROOT` carried every token.
//! They are now stored as one AES-256-GCM blob per instance in the
//! `instance_secrets` table, encrypted with a key that lives outside the data
//! root (`/etc/fleett/secrets.key` by default).
//!
//! The compose file references the values through Compose interpolation
//! (`TELEGRAM_BOT_TOKEN: ${TELEGRAM_BOT_TOKEN}`); the compose driver decrypts
//! the instance and injects the values into the `docker compose` child
//! environment, so no plaintext is ever written to disk.
//!
//! Existing hosts keep working: an instance that still has a `secrets.env`
//! file and no encrypted row is read from that file (with a warning) until the
//! operator runs `fleett secrets migrate --apply`.
//!
//! ```text
//! sudo fleett secrets init            # generate /etc/fleett/secrets.key (0640)
//! fleett secrets status               # key source, encrypted vs legacy counts
//! fleett secrets migrate              # dry run of the plaintext import
//! fleett secrets migrate --apply      # encrypt, rewrite compose, delete files
//! ```

use crate::config::Config;
use crate::db::{Db, DbError};
use crate::models::validate_secret_name;
use crate::store;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rand::RngCore;
use serde_yaml::Value;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

/// Inline key variable (base64, 32 bytes).
pub const KEY_ENV: &str = "FLEETT_SECRETS_KEY";
/// Key file variable.
pub const KEY_FILE_ENV: &str = "FLEETT_SECRETS_KEY_FILE";
/// Startup import switch.
pub const AUTOIMPORT_ENV: &str = "FLEETT_SECRETS_AUTOIMPORT";

/// Name of the legacy plaintext file inside an instance directory.
pub const LEGACY_FILE_NAME: &str = "secrets.env";

/// AES-256 key length.
pub const KEY_LEN: usize = 32;
/// AES-GCM nonce length (96 bits).
pub const NONCE_LEN: usize = 12;

/// Error code returned by the API when an encrypted store is required but no
/// key is configured.
pub const MISSING_KEY_CODE: &str = "secrets_key_missing";
/// Operator-facing hint for the same condition.
pub const MISSING_KEY_HINT: &str = "no secrets key is configured; run `sudo fleett secrets init` \
     (or set FLEETT_SECRETS_KEY) and retry";

/// Warning attached to instances whose secrets are still in a plaintext file.
pub const WARNING_LEGACY: &str = "secrets are stored in plain text in secrets.env; run \
     `fleett secrets migrate --apply` to encrypt them and delete the file";

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("{MISSING_KEY_HINT}")]
    MissingKey,
    #[error("invalid secrets key in {name}: {detail}")]
    InvalidKey { name: String, detail: String },
    #[error("cannot read the secrets key at {path}: {source}")]
    KeyFile { path: PathBuf, source: io::Error },
    #[error("cannot encrypt the secrets")]
    Encrypt,
    #[error("cannot decrypt the secrets: {0}")]
    Decrypt(String),
    #[error("cannot rewrite the compose file {path}: {detail}")]
    Compose { path: PathBuf, detail: String },
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("filesystem error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid secrets payload: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("the key file {0} already exists; refusing to overwrite it")]
    Exists(PathBuf),
    #[error(
        "the key file {0} is inside FLEETT_ROOT ({1}); keep the key outside the data root so a \
         backup of the root never contains it"
    )]
    InsideRoot(PathBuf, PathBuf),
    #[error("cannot write the key file {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
}

/// Where the key in use comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    Environment,
    File(PathBuf),
}

impl KeySource {
    pub fn describe(&self) -> String {
        match self {
            Self::Environment => format!("{KEY_ENV} (environment)"),
            Self::File(path) => path.display().to_string(),
        }
    }
}

/// A usable AES-256 key.
#[derive(Clone)]
pub struct SecretsKey {
    bytes: [u8; KEY_LEN],
    source: KeySource,
}

impl SecretsKey {
    pub fn new(bytes: [u8; KEY_LEN], source: KeySource) -> Self {
        Self { bytes, source }
    }

    pub fn source(&self) -> &KeySource {
        &self.source
    }

    /// Resolve the key from the configuration: `FLEETT_SECRETS_KEY` first, then
    /// the key file. `Ok(None)` means "no key configured at all".
    pub fn from_config(cfg: &Config) -> Result<Option<Self>, SecretsError> {
        if let Some(value) = cfg
            .secrets_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let bytes = decode_key(value, KEY_ENV)?;
            return Ok(Some(Self::new(bytes, KeySource::Environment)));
        }
        let path = &cfg.secrets_key_file;
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(SecretsError::KeyFile {
                    path: path.clone(),
                    source,
                })
            }
        };
        let bytes = decode_key(text.trim(), &path.display().to_string())?;
        Ok(Some(Self::new(bytes, KeySource::File(path.clone()))))
    }
}

impl std::fmt::Debug for SecretsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key material.
        f.debug_struct("SecretsKey")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// Encrypt a secrets map into one blob (fresh nonce per call).
pub fn encrypt_map(
    key: &SecretsKey,
    secrets: &BTreeMap<String, String>,
) -> Result<Sealed, SecretsError> {
    let plaintext = serde_json::to_vec(secrets)?;
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher(key)
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_slice())
        .map_err(|_| SecretsError::Encrypt)?;
    Ok(Sealed {
        nonce: nonce.to_vec(),
        ciphertext,
    })
}

/// Decrypt one blob. Any tampering or a wrong key fails the GCM tag.
pub fn decrypt_map(
    key: &SecretsKey,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<BTreeMap<String, String>, SecretsError> {
    if nonce.len() != NONCE_LEN {
        return Err(SecretsError::Decrypt(format!(
            "invalid nonce length {} (expected {NONCE_LEN})",
            nonce.len()
        )));
    }
    let plaintext = cipher(key)
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| {
            SecretsError::Decrypt("authentication failed (wrong key or tampered blob)".to_string())
        })?;
    Ok(serde_json::from_slice(&plaintext)?)
}

/// One encrypted secrets map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

fn cipher(key: &SecretsKey) -> Aes256Gcm {
    Aes256Gcm::new_from_slice(&key.bytes).expect("AES-256 keys are 32 bytes")
}

/// Base64-encode a key (the format written by `secrets init`).
pub fn encode_key(bytes: &[u8; KEY_LEN]) -> String {
    BASE64.encode(bytes)
}

/// Length of the generated key, as bytes.
pub fn generate_key() -> [u8; KEY_LEN] {
    let mut bytes = [0u8; KEY_LEN];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
}

fn decode_key(value: &str, name: &str) -> Result<[u8; KEY_LEN], SecretsError> {
    let decoded = BASE64
        .decode(value.trim())
        .map_err(|err| SecretsError::InvalidKey {
            name: name.to_string(),
            detail: format!("expected base64: {err}"),
        })?;
    let bytes: [u8; KEY_LEN] =
        decoded
            .as_slice()
            .try_into()
            .map_err(|_| SecretsError::InvalidKey {
                name: name.to_string(),
                detail: format!("expected {KEY_LEN} bytes, got {}", decoded.len()),
            })?;
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Storage decisions
// ---------------------------------------------------------------------------

/// Where the secrets of one instance ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// Encrypted row in `instance_secrets`.
    Encrypted,
    /// Plaintext `secrets.env` (legacy instance, no key yet).
    LegacyFile,
    /// The instance has no secret at all.
    Empty,
}

/// True when the key file would end up inside the data root.
///
/// Compared lexically: `FLEETT_SECRETS_KEY_FILE` is expected to be a clean
/// absolute path, like the default `/etc/fleett/secrets.key`.
pub fn key_file_inside_root(cfg: &Config) -> bool {
    cfg.secrets_key_file.starts_with(&cfg.root)
}

/// Fail early (before any file is written) when the secrets could not be
/// stored safely.
///
/// * an existing encrypted row needs the key to stay readable;
/// * an existing legacy file keeps the instance in legacy mode;
/// * a fresh instance needs a key, otherwise the API answers
///   `409 secrets_key_missing`.
pub fn check_storable(
    cfg: &Config,
    db: &Db,
    id: &str,
    secrets: &BTreeMap<String, String>,
) -> Result<(), SecretsError> {
    if secrets.is_empty() {
        return Ok(());
    }
    match db.sealed(id)? {
        Some(_) => match SecretsKey::from_config(cfg)? {
            Some(_) => Ok(()),
            None => Err(SecretsError::MissingKey),
        },
        None if cfg.secrets_path(id).is_file() => Ok(()),
        None => match SecretsKey::from_config(cfg)? {
            Some(_) => Ok(()),
            None => Err(SecretsError::MissingKey),
        },
    }
}

/// Persist the secrets of one instance, keeping the mode it already uses.
///
/// Encrypted instances stay encrypted (a rotation replaces the row with a new
/// nonce); legacy instances keep their `secrets.env` until
/// `fleett secrets migrate --apply`; a new instance is encrypted when a key is
/// available.
pub fn store(
    cfg: &Config,
    db: &Db,
    id: &str,
    secrets: &BTreeMap<String, String>,
) -> Result<Storage, SecretsError> {
    let key = SecretsKey::from_config(cfg)?;
    if db.sealed(id)?.is_some() {
        let key = key.ok_or(SecretsError::MissingKey)?;
        if secrets.is_empty() {
            db.delete_sealed(id)?;
            return Ok(Storage::Empty);
        }
        let sealed = encrypt_map(&key, secrets)?;
        db.put_sealed(id, &sealed.nonce, &sealed.ciphertext)?;
        return Ok(Storage::Encrypted);
    }
    if cfg.secrets_path(id).is_file() {
        store::write_legacy_secrets(cfg, id, secrets)?;
        return Ok(Storage::LegacyFile);
    }
    let Some(key) = key else {
        if secrets.is_empty() {
            return Ok(Storage::Empty);
        }
        return Err(SecretsError::MissingKey);
    };
    if secrets.is_empty() {
        return Ok(Storage::Empty);
    }
    let sealed = encrypt_map(&key, secrets)?;
    db.put_sealed(id, &sealed.nonce, &sealed.ciphertext)?;
    Ok(Storage::Encrypted)
}

/// Secrets of one instance: encrypted row first, then legacy file, else empty.
pub fn load(cfg: &Config, db: &Db, id: &str) -> Result<BTreeMap<String, String>, SecretsError> {
    if let Some(row) = db.sealed(id)? {
        let key = SecretsKey::from_config(cfg)?.ok_or(SecretsError::MissingKey)?;
        return decrypt_map(&key, &row.nonce, &row.ciphertext);
    }
    if cfg.secrets_path(id).is_file() {
        return Ok(store::read_legacy_secrets(cfg, id));
    }
    Ok(BTreeMap::new())
}

/// Sorted secret keys of one instance, for `InstanceDetail::env_keys`.
pub fn env_keys(cfg: &Config, db: &Db, id: &str) -> Result<Vec<String>, SecretsError> {
    Ok(load(cfg, db, id)?.into_keys().collect())
}

/// True when a plaintext `secrets.env` is still on disk for this instance
/// (whether or not an encrypted row already exists).
pub fn has_plaintext_file(cfg: &Config, id: &str) -> bool {
    cfg.secrets_path(id).is_file()
}

// ---------------------------------------------------------------------------
// Key file lifecycle
// ---------------------------------------------------------------------------

/// Generate the key file. Refuses to overwrite an existing one.
pub fn init(cfg: &Config) -> Result<PathBuf, InitError> {
    let path = cfg.secrets_key_file.clone();
    if path.exists() {
        return Err(InitError::Exists(path));
    }
    if key_file_inside_root(cfg) {
        return Err(InitError::InsideRoot(path, cfg.root.clone()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| InitError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o640);
    let mut file = options.open(&path).map_err(|source| InitError::Write {
        path: path.clone(),
        source,
    })?;
    let encoded = encode_key(&generate_key());
    file.write_all(encoded.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| InitError::Write {
            path: path.clone(),
            source,
        })?;
    // `mode` above only applies when the file is created; tighten explicitly so
    // a pre-existing umask cannot widen it.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).map_err(|source| {
        InitError::Write {
            path: path.clone(),
            source,
        }
    })?;
    Ok(path)
}

/// Report printed by `fleett secrets status`.
#[derive(Debug, Clone)]
pub struct Status {
    /// Human description of the key source, when a usable key was found.
    pub source: Option<String>,
    /// Why the configured key is unusable, when it is.
    pub key_error: Option<String>,
    pub key_file: PathBuf,
    pub key_file_exists: bool,
    pub key_file_mode: Option<u32>,
    pub key_file_inside_root: bool,
    pub encrypted_instances: usize,
    pub legacy_files: Vec<PathBuf>,
}

pub fn status(cfg: &Config, db: &Db) -> Status {
    let key = SecretsKey::from_config(cfg);
    let (source, key_error) = match &key {
        Ok(Some(key)) => (Some(key.source().describe()), None),
        Ok(None) => (None, None),
        Err(err) => (None, Some(err.to_string())),
    };
    let key_file_exists = cfg.secrets_key_file.is_file();
    let key_file_mode = fs::metadata(&cfg.secrets_key_file)
        .ok()
        .map(|meta| meta.permissions().mode() & 0o777);
    Status {
        source,
        key_error,
        key_file: cfg.secrets_key_file.clone(),
        key_file_exists,
        key_file_mode,
        key_file_inside_root: key_file_inside_root(cfg),
        encrypted_instances: db.sealed_count().unwrap_or(0),
        legacy_files: legacy_entries(cfg)
            .into_iter()
            .map(|entry| entry.path)
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Migration of the legacy plaintext files
// ---------------------------------------------------------------------------

/// One `secrets.env` file left by an older release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyEntry {
    pub instance_id: String,
    pub path: PathBuf,
    pub compose_path: PathBuf,
    /// Keys found in the file (sorted).
    pub keys: Vec<String>,
    /// The compose file still declares `env_file: secrets.env`.
    pub compose_references_env_file: bool,
}

/// Every `instances/*/secrets.env` still on disk.
pub fn legacy_entries(cfg: &Config) -> Vec<LegacyEntry> {
    let mut entries = Vec::new();
    let Ok(dir) = fs::read_dir(cfg.instances_dir()) else {
        return entries;
    };
    for entry in dir.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(id) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if crate::models::validate_instance_id(id).is_err() {
            continue;
        }
        let path = cfg.secrets_path(id);
        if !path.is_file() {
            continue;
        }
        let keys = store::read_legacy_secrets(cfg, id)
            .into_keys()
            .collect::<Vec<_>>();
        let compose_path = cfg.compose_path(id);
        let compose_references_env_file = fs::read_to_string(&compose_path)
            .ok()
            .is_some_and(|text| compose_uses_legacy_env_file(&text));
        entries.push(LegacyEntry {
            instance_id: id.to_string(),
            path,
            compose_path,
            keys,
            compose_references_env_file,
        });
    }
    entries.sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
    entries
}

/// Result of `fleett secrets migrate`.
#[derive(Debug, Clone, Default)]
pub struct MigrateReport {
    pub applied: bool,
    pub entries: Vec<LegacyEntry>,
    pub migrated: Vec<String>,
    pub failures: Vec<(String, String)>,
}

/// Import every legacy file into the encrypted store.
///
/// The plaintext file is deleted only when `apply` is set (dry run by default).
pub fn migrate(cfg: &Config, apply: bool) -> anyhow::Result<MigrateReport> {
    let entries = legacy_entries(cfg);
    let mut report = MigrateReport {
        applied: apply,
        entries,
        ..MigrateReport::default()
    };
    if report.entries.is_empty() {
        return Ok(report);
    }
    let key = SecretsKey::from_config(cfg)?.ok_or_else(|| anyhow::anyhow!("{MISSING_KEY_HINT}"))?;
    if !apply {
        return Ok(report);
    }

    let db = Db::open(&cfg.db_path())?;
    for entry in &report.entries {
        match import_entry(cfg, &db, &key, entry) {
            Ok(()) => {
                if let Err(err) = fs::remove_file(&entry.path) {
                    if err.kind() != io::ErrorKind::NotFound {
                        report.failures.push((
                            entry.instance_id.clone(),
                            format!("cannot delete {}: {err}", entry.path.display()),
                        ));
                        continue;
                    }
                }
                report.migrated.push(entry.instance_id.clone());
            }
            Err(err) => report
                .failures
                .push((entry.instance_id.clone(), err.to_string())),
        }
    }
    Ok(report)
}

/// Encrypt one legacy file into the database and rewrite its compose file.
///
/// When the instance already has an encrypted row, that row wins: a stale
/// `secrets.env` never reverts a token rotated through the API. The file is
/// still superseded (compose rewritten, deleted by the caller with `--apply`).
///
/// The compose rewrite drops `env_file: secrets.env` and declares every key
/// through Compose interpolation, so the driver environment is the only source
/// of values once the file is gone.
pub fn import_entry(
    cfg: &Config,
    db: &Db,
    key: &SecretsKey,
    entry: &LegacyEntry,
) -> Result<(), SecretsError> {
    let keys = match db.sealed(&entry.instance_id)? {
        Some(row) => {
            tracing::info!(
                instance = %entry.instance_id,
                "secrets: the plaintext secrets.env is superseded by the encrypted row"
            );
            decrypt_map(key, &row.nonce, &row.ciphertext)?
                .into_keys()
                .collect::<Vec<_>>()
        }
        None => {
            let secrets = store::read_legacy_secrets(cfg, &entry.instance_id);
            if !secrets.is_empty() {
                let sealed = encrypt_map(key, &secrets)?;
                db.put_sealed(&entry.instance_id, &sealed.nonce, &sealed.ciphertext)?;
            }
            secrets.into_keys().collect()
        }
    };
    if entry.compose_path.is_file() {
        let text = fs::read_to_string(&entry.compose_path)?;
        let rewritten =
            rewrite_compose_secrets(&text, &keys).map_err(|detail| SecretsError::Compose {
                path: entry.compose_path.clone(),
                detail,
            })?;
        if rewritten != text {
            store::write_compose(cfg, &entry.instance_id, &rewritten)?;
        }
    }
    Ok(())
}

/// Startup import, best effort.
///
/// Every legacy file is encrypted when a key is available; the file is deleted
/// only when `FLEETT_SECRETS_AUTOIMPORT=1`. Remaining files are reported as
/// warnings on every start.
pub fn startup(cfg: &Config, db: &Db) {
    if key_file_inside_root(cfg) {
        tracing::warn!(
            path = %cfg.secrets_key_file.display(),
            root = %cfg.root.display(),
            "secrets: the key file is inside FLEETT_ROOT; a backup of the root would contain the \
             key. Move it outside, then run `fleett secrets status`"
        );
    }

    let key = match SecretsKey::from_config(cfg) {
        Ok(key) => key,
        Err(err) => {
            tracing::warn!(%err, "secrets: the configured key is unusable");
            None
        }
    };

    let entries = legacy_entries(cfg);
    if let Some(key) = &key {
        for entry in &entries {
            match import_entry(cfg, db, key, entry) {
                Ok(()) => {
                    tracing::info!(
                        instance = %entry.instance_id,
                        "secrets: imported secrets.env into the encrypted store"
                    );
                    if cfg.secrets_autoimport {
                        match fs::remove_file(&entry.path) {
                            Ok(()) => tracing::info!(
                                instance = %entry.instance_id,
                                "secrets: deleted the plaintext secrets.env ({AUTOIMPORT_ENV}=1)"
                            ),
                            Err(err) => tracing::warn!(
                                path = %entry.path.display(),
                                %err,
                                "secrets: could not delete the plaintext secrets.env"
                            ),
                        }
                    }
                }
                Err(err) => tracing::warn!(
                    instance = %entry.instance_id,
                    %err,
                    "secrets: could not import the legacy secrets.env"
                ),
            }
        }
    }

    for entry in legacy_entries(cfg) {
        tracing::warn!(
            path = %entry.path.display(),
            "secrets: plaintext secrets still on disk; run `fleett secrets migrate --apply` to \
             encrypt them and delete the file"
        );
    }

    if key.is_none() && db.sealed_count().unwrap_or(0) > 0 {
        tracing::warn!(
            "secrets: encrypted secrets exist but no key is configured; run \
             `sudo fleett secrets init` or restore the key file"
        );
    }
}

// ---------------------------------------------------------------------------
// Compose rewrite
// ---------------------------------------------------------------------------

/// True when a compose document still mounts a `secrets.env` env file.
pub fn compose_uses_legacy_env_file(text: &str) -> bool {
    let Ok(doc) = serde_yaml::from_str::<Value>(text) else {
        return false;
    };
    service_env_files(doc.get("services"))
        .iter()
        .any(|name| name == LEGACY_FILE_NAME)
}

/// Rewrite the `agent` service of a compose document:
///
/// * drop `env_file: secrets.env` (other env files are kept);
/// * keep `HERMES_UID` / `HERMES_GID` as literals;
/// * declare every secret key as `${KEY}` in `environment`.
pub fn rewrite_compose_secrets(text: &str, keys: &[String]) -> Result<String, String> {
    let mut doc: Value =
        serde_yaml::from_str(text).map_err(|err| format!("invalid compose YAML: {err}"))?;
    if !doc.is_mapping() {
        return Err("the compose document must be a YAML mapping".to_string());
    }
    let Some(services) = doc.get_mut("services").and_then(Value::as_mapping_mut) else {
        return Ok(text.to_string());
    };

    for (name, service) in services.iter_mut() {
        // Only the agent service receives the instance secrets.
        let is_agent = name.as_str().is_some_and(|name| name == "agent");
        let Some(service) = service.as_mapping_mut() else {
            continue;
        };
        if is_agent {
            strip_legacy_env_file(service);
            if !keys.is_empty() {
                let environment = environment_mapping(service);
                for key in keys {
                    if validate_secret_name(key).is_ok() {
                        environment.insert(
                            Value::from(key.as_str()),
                            Value::from(format!("${{{key}}}")),
                        );
                    }
                }
            }
        }
    }

    serde_yaml::to_string(&doc).map_err(|err| format!("cannot render compose YAML: {err}"))
}

fn strip_legacy_env_file(service: &mut serde_yaml::Mapping) {
    let Some(value) = service.get_mut("env_file") else {
        return;
    };
    match value {
        Value::String(name) if name == LEGACY_FILE_NAME => {
            service.remove("env_file");
        }
        Value::Sequence(items) => {
            items.retain(|item| item.as_str() != Some(LEGACY_FILE_NAME));
            if items.is_empty() {
                service.remove("env_file");
            }
        }
        _ => {}
    }
}

fn environment_mapping(service: &mut serde_yaml::Mapping) -> &mut serde_yaml::Mapping {
    let replace = !matches!(service.get("environment"), Some(Value::Mapping(_)));
    if replace {
        service.insert(
            Value::from("environment"),
            Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    service
        .get_mut("environment")
        .and_then(Value::as_mapping_mut)
        .expect("environment mapping was created just above")
}

/// Read the `env_file` values of a compose document (diagnostics helper).
fn service_env_files(services: Option<&Value>) -> Vec<String> {
    let Some(services) = services.and_then(Value::as_mapping) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for service in services.values() {
        let Some(env_file) = service.get("env_file") else {
            continue;
        };
        match env_file {
            Value::String(name) => names.push(name.clone()),
            Value::Sequence(items) => {
                names.extend(items.iter().filter_map(Value::as_str).map(str::to_string));
            }
            Value::Mapping(single) => {
                if let Some(path) = single.get("path").and_then(Value::as_str) {
                    names.push(path.to_string());
                }
            }
            _ => {}
        }
    }
    names
}

/// Read the `env_file` values of a compose document (test helper, also useful
/// for diagnostics).
pub fn compose_env_files(text: &str) -> Vec<String> {
    let Ok(doc) = serde_yaml::from_str::<Value>(text) else {
        return Vec::new();
    };
    service_env_files(doc.get("services"))
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// `fleett secrets init`.
pub fn run_init(cfg: &Config) -> anyhow::Result<()> {
    let path = init(cfg).map_err(|err| anyhow::anyhow!("{err}"))?;
    println!("secrets key: created {}", path.display());
    if cfg.secrets_key.is_some() {
        println!(
            "  note: {KEY_ENV} is set and takes precedence over this file; remove it to use the file"
        );
    }
    println!("  mode 0640; keep it outside FLEETT_ROOT and back it up separately.");
    println!("  without this file the encrypted secrets cannot be decrypted.");
    Ok(())
}

/// `fleett secrets status`.
pub fn run_status(cfg: &Config) -> anyhow::Result<()> {
    let db =
        Db::open(&cfg.db_path()).map_err(|err| anyhow::anyhow!("cannot open the index: {err}"))?;
    let status = status(cfg, &db);

    match (&status.source, &status.key_error) {
        (Some(source), _) => println!("key: {source}"),
        (None, Some(err)) => println!("key: unusable ({err})"),
        (None, None) => println!("key: none"),
    }
    if status.key_error.is_none() && status.source.is_none() {
        println!("  {}", MISSING_KEY_HINT);
    }
    match status.key_file_mode {
        Some(mode) => println!("key file: {} (mode {mode:04o})", status.key_file.display()),
        None => println!("key file: {} (missing)", status.key_file.display()),
    }
    if status.key_file_inside_root {
        println!(
            "  warning: the key file is inside FLEETT_ROOT; a backup of the root would contain it"
        );
    }
    println!("encrypted instances: {}", status.encrypted_instances);
    println!("legacy secrets.env files: {}", status.legacy_files.len());
    for path in &status.legacy_files {
        println!("  {} (plaintext)", path.display());
    }
    if !status.legacy_files.is_empty() {
        println!("run `fleett secrets migrate --apply` to encrypt them and delete the files");
    }
    Ok(())
}

/// `fleett secrets migrate [--apply]`.
pub fn run_migrate(cfg: &Config, apply: bool) -> anyhow::Result<()> {
    let report = migrate(cfg, apply).map_err(|err| anyhow::anyhow!("{err}"))?;
    if report.entries.is_empty() {
        println!("No legacy secrets.env file found.");
        return Ok(());
    }

    println!("{} legacy secrets.env file(s):", report.entries.len());
    for entry in &report.entries {
        let mut line = format!(
            "  {}: {} key(s)  {}",
            entry.instance_id,
            entry.keys.len(),
            entry.path.display()
        );
        if entry.compose_references_env_file {
            line.push_str("  (compose still uses env_file)");
        }
        println!("{line}");
    }

    if !apply {
        println!("Dry run: nothing changed. Re-run with --apply to encrypt and delete.");
        return Ok(());
    }

    for id in &report.migrated {
        println!("  encrypted {id}; compose rewritten, plaintext file deleted");
    }
    for (id, err) in &report.failures {
        eprintln!("  error for {id}: {err}");
    }
    if !report.failures.is_empty() {
        anyhow::bail!(
            "{} instance(s) could not be migrated",
            report.failures.len()
        );
    }
    println!("Migrated {} instance(s).", report.migrated.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::Path;

    const KEY_BYTES: [u8; KEY_LEN] = [7u8; KEY_LEN];

    fn key() -> SecretsKey {
        SecretsKey::new(KEY_BYTES, KeySource::Environment)
    }

    fn temp_config() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        (dir, cfg)
    }

    fn sample() -> BTreeMap<String, String> {
        let mut secrets = BTreeMap::new();
        secrets.insert("TELEGRAM_BOT_TOKEN".to_string(), "123:ABC".to_string());
        secrets.insert("HERMES_GATE_PROFILE_TOKEN".to_string(), "gate".to_string());
        secrets
    }

    #[test]
    fn secrets_round_trip_through_the_cipher() {
        let key = key();
        let sealed = encrypt_map(&key, &sample()).expect("encrypt");
        assert_eq!(sealed.nonce.len(), NONCE_LEN);
        assert_ne!(sealed.ciphertext, b"{}");
        assert!(!sealed
            .ciphertext
            .windows(7)
            .any(|window| window == b"123:ABC"));

        let decrypted = decrypt_map(&key, &sealed.nonce, &sealed.ciphertext).expect("decrypt");
        assert_eq!(decrypted, sample());
    }

    #[test]
    fn a_fresh_nonce_is_used_for_every_value() {
        let key = key();
        let first = encrypt_map(&key, &sample()).expect("encrypt");
        let second = encrypt_map(&key, &sample()).expect("encrypt");
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.ciphertext, second.ciphertext);
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let key = key();
        let mut sealed = encrypt_map(&key, &sample()).expect("encrypt");
        let last = sealed.ciphertext.len() - 1;
        sealed.ciphertext[last] ^= 0x01;
        let err = decrypt_map(&key, &sealed.nonce, &sealed.ciphertext).expect_err("tampered");
        assert!(matches!(err, SecretsError::Decrypt(_)), "{err}");

        let mut nonce = sealed.nonce.clone();
        nonce[0] ^= 0xFF;
        let err = decrypt_map(&key, &nonce, &sealed.ciphertext).expect_err("wrong nonce");
        assert!(matches!(err, SecretsError::Decrypt(_)), "{err}");

        let err = decrypt_map(&key, &[1, 2, 3], &sealed.ciphertext).expect_err("short nonce");
        assert!(matches!(err, SecretsError::Decrypt(_)), "{err}");

        let other = SecretsKey::new([9u8; KEY_LEN], KeySource::Environment);
        let err = decrypt_map(&other, &sealed.nonce, &sealed.ciphertext).expect_err("wrong key");
        assert!(matches!(err, SecretsError::Decrypt(_)), "{err}");
    }

    #[test]
    fn keys_are_base64_encoded_and_validated() {
        let encoded = encode_key(&KEY_BYTES);
        assert_eq!(decode_key(&encoded, "test").expect("decode"), KEY_BYTES);
        assert!(matches!(
            decode_key("not base64 !!", "test"),
            Err(SecretsError::InvalidKey { .. })
        ));
        let short = BASE64.encode([1u8; 16]);
        assert!(matches!(
            decode_key(&short, "test"),
            Err(SecretsError::InvalidKey { .. })
        ));
    }

    #[test]
    fn the_key_is_resolved_from_the_environment_then_the_file() {
        let (_dir, mut cfg) = temp_config();
        assert!(SecretsKey::from_config(&cfg).expect("resolve").is_none());

        fs::write(
            &cfg.secrets_key_file,
            format!("{}\n", encode_key(&KEY_BYTES)),
        )
        .expect("write key file");
        let from_file = SecretsKey::from_config(&cfg)
            .expect("resolve")
            .expect("key");
        assert_eq!(
            from_file.source(),
            &KeySource::File(cfg.secrets_key_file.clone())
        );

        cfg.secrets_key = Some(encode_key(&[3u8; KEY_LEN]));
        let from_env = SecretsKey::from_config(&cfg)
            .expect("resolve")
            .expect("key");
        assert_eq!(from_env.source(), &KeySource::Environment);

        cfg.secrets_key = Some("not-a-key".to_string());
        assert!(matches!(
            SecretsKey::from_config(&cfg),
            Err(SecretsError::InvalidKey { .. })
        ));
    }

    #[test]
    fn init_creates_a_0640_key_file_once() {
        let (_dir, mut cfg) = temp_config();
        cfg.secrets_key_file = cfg.root.with_extension("secrets.key");
        assert!(!cfg.secrets_key_file.exists());

        let path = init(&cfg).expect("init");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        let text = fs::read_to_string(&path).expect("key readable");
        assert_eq!(text.trim().len(), BASE64.encode([0u8; KEY_LEN]).len());

        let err = init(&cfg).expect_err("refuse to overwrite");
        assert!(matches!(err, InitError::Exists(_)), "{err}");
    }

    #[test]
    fn init_refuses_a_key_inside_the_data_root() {
        let (_dir, mut cfg) = temp_config();
        cfg.secrets_key_file = cfg.root.join("secrets.key");
        let err = init(&cfg).expect_err("inside root");
        assert!(matches!(err, InitError::InsideRoot(_, _)), "{err}");
        assert!(key_file_inside_root(&cfg));
    }

    #[test]
    fn the_default_test_key_file_is_outside_the_root() {
        let (_dir, cfg) = temp_config();
        assert!(!key_file_inside_root(&cfg));
        assert!(cfg
            .secrets_key_file
            .starts_with(cfg.root.parent().expect("parent")));
    }

    #[test]
    fn a_fresh_instance_is_encrypted_when_a_key_exists() {
        let (_dir, mut cfg) = temp_config();
        cfg.secrets_key = Some(encode_key(&KEY_BYTES));
        let db = Db::open(&cfg.db_path()).expect("db");

        let storage = store(&cfg, &db, "alice", &sample()).expect("store");
        assert_eq!(storage, Storage::Encrypted);
        assert!(!cfg.secrets_path("alice").exists());
        let row = db.sealed("alice").expect("sealed").expect("row");
        assert_eq!(load(&cfg, &db, "alice").expect("load"), sample());
        assert_eq!(
            env_keys(&cfg, &db, "alice").expect("keys"),
            vec![
                "HERMES_GATE_PROFILE_TOKEN".to_string(),
                "TELEGRAM_BOT_TOKEN".to_string()
            ]
        );
        assert_eq!(
            decrypt_map(&key(), &row.nonce, &row.ciphertext).expect("decrypt"),
            sample()
        );
    }

    #[test]
    fn a_fresh_instance_without_a_key_is_refused() {
        let (_dir, cfg) = temp_config();
        let db = Db::open(&cfg.db_path()).expect("db");
        let err = store(&cfg, &db, "alice", &sample()).expect_err("no key");
        assert!(matches!(err, SecretsError::MissingKey));
        assert!(check_storable(&cfg, &db, "alice", &sample()).is_err());
        assert!(!cfg.secrets_path("alice").exists());
        assert!(db.sealed("alice").expect("sealed").is_none());

        // No secret at all is always fine.
        assert_eq!(
            store(&cfg, &db, "alice", &BTreeMap::new()).expect("empty"),
            Storage::Empty
        );
        assert!(check_storable(&cfg, &db, "alice", &BTreeMap::new()).is_ok());
    }

    #[test]
    fn a_legacy_file_keeps_working_without_a_key() {
        let (_dir, cfg) = temp_config();
        let db = Db::open(&cfg.db_path()).expect("db");
        store::write_legacy_secrets(&cfg, "alice", &sample()).expect("legacy file");

        assert!(has_plaintext_file(&cfg, "alice"));
        assert!(check_storable(&cfg, &db, "alice", &sample()).is_ok());
        let storage = store(&cfg, &db, "alice", &sample()).expect("store");
        assert_eq!(storage, Storage::LegacyFile);
        assert_eq!(load(&cfg, &db, "alice").expect("load"), sample());

        // Rotation rewrites the plaintext file: the legacy mode is untouched.
        let mut rotated = sample();
        rotated.insert("TELEGRAM_BOT_TOKEN".to_string(), "456:DEF".to_string());
        store(&cfg, &db, "alice", &rotated).expect("rotate");
        assert_eq!(
            store::read_legacy_secrets(&cfg, "alice")
                .get("TELEGRAM_BOT_TOKEN")
                .map(String::as_str),
            Some("456:DEF")
        );
        assert!(db.sealed("alice").expect("sealed").is_none());
    }

    #[test]
    fn rotation_replaces_the_encrypted_row() {
        let (_dir, mut cfg) = temp_config();
        cfg.secrets_key = Some(encode_key(&KEY_BYTES));
        let db = Db::open(&cfg.db_path()).expect("db");
        store(&cfg, &db, "alice", &sample()).expect("store");
        let first = db.sealed("alice").expect("sealed").expect("row");

        let mut rotated = sample();
        rotated.insert("TELEGRAM_BOT_TOKEN".to_string(), "456:DEF".to_string());
        assert_eq!(
            store(&cfg, &db, "alice", &rotated).expect("rotate"),
            Storage::Encrypted
        );
        let second = db.sealed("alice").expect("sealed").expect("row");
        assert_ne!(first.ciphertext, second.ciphertext);
        assert_ne!(first.nonce, second.nonce);
        assert_eq!(
            load(&cfg, &db, "alice").expect("load"),
            rotated,
            "the new token must be readable"
        );
    }

    #[test]
    fn removing_every_secret_drops_the_row() {
        let (_dir, mut cfg) = temp_config();
        cfg.secrets_key = Some(encode_key(&KEY_BYTES));
        let db = Db::open(&cfg.db_path()).expect("db");
        store(&cfg, &db, "alice", &sample()).expect("store");
        assert_eq!(
            store(&cfg, &db, "alice", &BTreeMap::new()).expect("clear"),
            Storage::Empty
        );
        assert!(db.sealed("alice").expect("sealed").is_none());
    }

    #[test]
    fn deleting_an_instance_drops_its_encrypted_row() {
        let (_dir, mut cfg) = temp_config();
        cfg.secrets_key = Some(encode_key(&KEY_BYTES));
        let db = Db::open(&cfg.db_path()).expect("db");
        let spec = crate::models::InstanceSpec {
            id: "alice".to_string(),
            name: "alice".to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: crate::models::DEFAULT_API_KEY_ENV.to_string(),
            channel: crate::models::ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: crate::models::DEFAULT_MEM_LIMIT_MB,
            cpu_limit: crate::models::DEFAULT_CPU_LIMIT,
            image: "hermes-agent:0.22.2-local".to_string(),
            previous_image: None,
            allow_lan: false,
            notes: String::new(),
            status: crate::models::Status::Created,
            last_error: None,
            created_at: crate::models::now_iso(),
            updated_at: crate::models::now_iso(),
        };
        db.insert_instance(&spec).expect("insert");
        store(&cfg, &db, "alice", &sample()).expect("store");
        db.delete_instance("alice").expect("delete");
        assert!(db.sealed("alice").expect("sealed").is_none());
    }

    #[test]
    fn migration_dry_run_lists_files_without_touching_them() {
        let (_dir, mut cfg) = temp_config();
        cfg.secrets_key = Some(encode_key(&KEY_BYTES));
        let legacy_compose = "name: fleett-alice\n\
                              services:\n\
                              \x20 agent:\n\
                              \x20   env_file:\n\
                              \x20     - secrets.env\n\
                              \x20   environment:\n\
                              \x20     HERMES_UID: '10000'\n";
        fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        fs::write(cfg.compose_path("alice"), legacy_compose).expect("compose");
        store::write_legacy_secrets(&cfg, "alice", &sample()).expect("legacy file");

        let entries = legacy_entries(&cfg);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].keys.len(), 2);
        assert!(entries[0].compose_references_env_file);

        let report = migrate(&cfg, false).expect("dry run");
        assert!(!report.applied);
        assert_eq!(report.entries.len(), 1);
        assert!(report.migrated.is_empty());
        assert!(cfg.secrets_path("alice").is_file());
        assert_eq!(
            fs::read_to_string(cfg.compose_path("alice")).expect("compose kept"),
            legacy_compose
        );
    }

    #[test]
    fn migration_apply_encrypts_rewrites_and_deletes() {
        let (_dir, mut cfg) = temp_config();
        cfg.secrets_key = Some(encode_key(&KEY_BYTES));
        let legacy_compose = "name: fleett-alice\n\
                              services:\n\
                              \x20 agent:\n\
                              \x20   env_file:\n\
                              \x20     - secrets.env\n\
                              \x20   environment:\n\
                              \x20     HERMES_UID: '10000'\n\
                              \x20     HERMES_GID: '10000'\n";
        fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        fs::write(cfg.compose_path("alice"), legacy_compose).expect("compose");
        store::write_legacy_secrets(&cfg, "alice", &sample()).expect("legacy file");

        let report = migrate(&cfg, true).expect("apply");
        assert_eq!(report.migrated, vec!["alice".to_string()]);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(!cfg.secrets_path("alice").is_file());

        let compose = fs::read_to_string(cfg.compose_path("alice")).expect("compose");
        assert!(!compose.contains("env_file"), "{compose}");
        assert!(
            compose.contains("TELEGRAM_BOT_TOKEN: ${TELEGRAM_BOT_TOKEN}"),
            "{compose}"
        );
        assert!(compose.contains("HERMES_UID: '10000'"), "{compose}");

        let db = Db::open(&cfg.db_path()).expect("db");
        assert_eq!(load(&cfg, &db, "alice").expect("load"), sample());

        // Nothing usable remains in the instance directory.
        let everything = read_tree(&cfg.instance_dir("alice"));
        assert!(!everything.contains("123:ABC"), "{everything}");
        assert!(
            !everything.contains("HERMES_GATE_PROFILE_TOKEN=gate"),
            "{everything}"
        );
    }

    #[test]
    fn migration_apply_keeps_the_encrypted_row_over_a_stale_file() {
        let (_dir, mut cfg) = temp_config();
        cfg.secrets_key = Some(encode_key(&KEY_BYTES));
        let db = Db::open(&cfg.db_path()).expect("db");
        let mut rotated = sample();
        rotated.insert("TELEGRAM_BOT_TOKEN".to_string(), "456:DEF".to_string());
        store(&cfg, &db, "alice", &rotated).expect("store");

        fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        fs::write(
            cfg.compose_path("alice"),
            "name: fleett-alice\nservices:\n  agent:\n    env_file: secrets.env\n",
        )
        .expect("compose");
        store::write_legacy_secrets(&cfg, "alice", &sample()).expect("stale file");

        let report = migrate(&cfg, true).expect("apply");
        assert_eq!(report.migrated, vec!["alice".to_string()]);
        assert!(!cfg.secrets_path("alice").is_file());
        assert_eq!(
            load(&cfg, &db, "alice")
                .expect("load")
                .get("TELEGRAM_BOT_TOKEN")
                .map(String::as_str),
            Some("456:DEF"),
            "the encrypted row must survive the stale file"
        );
        let compose = fs::read_to_string(cfg.compose_path("alice")).expect("compose");
        assert!(compose.contains("TELEGRAM_BOT_TOKEN: ${TELEGRAM_BOT_TOKEN}"));
        assert!(!compose.contains("env_file"));
    }

    #[test]
    fn migration_needs_a_key() {
        let (_dir, cfg) = temp_config();
        fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        store::write_legacy_secrets(&cfg, "alice", &sample()).expect("legacy file");
        let err = migrate(&cfg, false).expect_err("no key");
        assert!(err.to_string().contains("fleett secrets init"), "{err}");
    }

    #[test]
    fn startup_imports_a_legacy_file_but_keeps_it_unless_autoimport() {
        let (_dir, mut cfg) = temp_config();
        let legacy_compose = "name: fleett-alice\n\
                              services:\n\
                              \x20 agent:\n\
                              \x20   env_file: secrets.env\n";
        fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        fs::write(cfg.compose_path("alice"), legacy_compose).expect("compose");
        store::write_legacy_secrets(&cfg, "alice", &sample()).expect("legacy file");

        // No key: nothing is imported and the file is kept.
        let db = Db::open(&cfg.db_path()).expect("db");
        startup(&cfg, &db);
        assert!(db.sealed("alice").expect("sealed").is_none());
        assert!(cfg.secrets_path("alice").is_file());

        // Key available, autoimport off: the row is created, the file stays.
        cfg.secrets_key = Some(encode_key(&KEY_BYTES));
        startup(&cfg, &db);
        assert!(db.sealed("alice").expect("sealed").is_some());
        assert!(cfg.secrets_path("alice").is_file());

        // A rotation through the API must not be reverted by the stale file on
        // the next start.
        let mut rotated = sample();
        rotated.insert("TELEGRAM_BOT_TOKEN".to_string(), "456:DEF".to_string());
        store(&cfg, &db, "alice", &rotated).expect("rotate");
        startup(&cfg, &db);
        assert_eq!(
            load(&cfg, &db, "alice")
                .expect("load")
                .get("TELEGRAM_BOT_TOKEN")
                .map(String::as_str),
            Some("456:DEF"),
            "the encrypted row wins over a stale secrets.env"
        );

        // Autoimport on: the imported file is deleted.
        cfg.secrets_autoimport = true;
        startup(&cfg, &db);
        assert!(!cfg.secrets_path("alice").is_file());
        assert_eq!(
            load(&cfg, &db, "alice")
                .expect("load")
                .get("TELEGRAM_BOT_TOKEN")
                .map(String::as_str),
            Some("456:DEF")
        );
    }

    #[test]
    fn compose_rewrite_drops_the_env_file_and_inlines_interpolation() {
        let text = "name: fleett-alice\n\
                    services:\n\
                    \x20 agent:\n\
                    \x20   image: agent:1\n\
                    \x20   env_file: secrets.env\n\
                    \x20   environment:\n\
                    \x20     HERMES_UID: '10000'\n";
        let keys = vec!["TELEGRAM_BOT_TOKEN".to_string(), "EXTRA".to_string()];
        let rewritten = rewrite_compose_secrets(text, &keys).expect("rewrite");
        let doc: Value = serde_yaml::from_str(&rewritten).expect("parses");
        let agent = &doc["services"]["agent"];
        assert!(agent.get("env_file").is_none(), "{rewritten}");
        assert_eq!(agent["environment"]["HERMES_UID"].as_str(), Some("10000"));
        assert_eq!(
            agent["environment"]["TELEGRAM_BOT_TOKEN"].as_str(),
            Some("${TELEGRAM_BOT_TOKEN}")
        );
        assert_eq!(agent["environment"]["EXTRA"].as_str(), Some("${EXTRA}"));
        assert!(!rewritten.contains("123:ABC"));
        assert!(compose_uses_legacy_env_file(text));
        assert!(!compose_uses_legacy_env_file(&rewritten));
    }

    #[test]
    fn compose_rewrite_keeps_other_env_files_and_the_sequence_shape() {
        let text = "name: fleett-alice\n\
                    services:\n\
                    \x20 agent:\n\
                    \x20   env_file:\n\
                    \x20     - secrets.env\n\
                    \x20     - overrides.env\n";
        let rewritten = rewrite_compose_secrets(text, &["A".to_string()]).expect("rewrite");
        let doc: Value = serde_yaml::from_str(&rewritten).expect("parses");
        let env_file: Vec<&str> = doc["services"]["agent"]["env_file"]
            .as_sequence()
            .expect("sequence")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(env_file, ["overrides.env"]);
    }

    #[test]
    fn compose_rewrite_rejects_invalid_yaml() {
        assert!(rewrite_compose_secrets("name: [unclosed", &[]).is_err());
        assert!(rewrite_compose_secrets("- a\n", &[]).is_err());
    }

    #[test]
    fn status_reports_the_key_source_and_the_counts() {
        let (_dir, mut cfg) = temp_config();
        cfg.secrets_key = Some(encode_key(&KEY_BYTES));
        let db = Db::open(&cfg.db_path()).expect("db");
        store(&cfg, &db, "alice", &sample()).expect("store");
        fs::create_dir_all(cfg.instance_dir("bob")).expect("instance dir");
        store::write_legacy_secrets(&cfg, "bob", &sample()).expect("legacy");

        let status = status(&cfg, &db);
        assert_eq!(
            status.source.as_deref(),
            Some("FLEETT_SECRETS_KEY (environment)")
        );
        assert!(status.key_error.is_none());
        assert_eq!(status.encrypted_instances, 1);
        assert_eq!(status.legacy_files.len(), 1);
        assert!(status.legacy_files[0].ends_with("bob/secrets.env"));
    }

    fn read_tree(dir: &Path) -> String {
        let mut out = String::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.push_str(&read_tree(&path));
            } else if let Ok(text) = fs::read_to_string(&path) {
                out.push_str(&text);
            }
        }
        out
    }
}
