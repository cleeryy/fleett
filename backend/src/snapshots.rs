//! Per-instance restore points (issue #38).
//!
//! `fleett upgrade` can roll back an *image*; nothing protected the *data* of
//! an instance. A broken `SOUL.md`, a bad `env` write or a failed migration had
//! no safety net, and `fleett backup` snapshots the whole fleet: too heavy for
//! one risky change.
//!
//! A snapshot is a gzipped tar of one instance:
//!
//! ```text
//! snapshot.json   format/version, ids, timestamps, label, retention
//! compose.yaml    the compose file as it was
//! config.yaml     the rendered Hermes config (also inside data/)
//! soul.md         the agent persona, when it had one (also inside data/)
//! env.json        the instance_env rows, *secrets still encrypted*
//! data/           the Hermes home (no secrets.env, no *.key, no runtime caches)
//! ```
//!
//! ## Secret handling
//!
//! Secret values are never decrypted to write a snapshot: `env.json` carries
//! the AES-256-GCM nonce and ciphertext exactly as they sit in `instance_env`,
//! base64-encoded. Restoring writes those bytes back, so a snapshot is useless
//! without the host key (`/etc/fleett/secrets.key`) and never leaks a token.
//! Non-secret rows keep their plaintext value, exactly like the database.
//! `secrets.env` and `*.key` files are excluded by the shared archive walker.
//!
//! ## Restore
//!
//! `restore` stops the instance, moves its `data/` aside to a
//! `data.pre-restore-<timestamp>` sibling (**never deletes it**), extracts the
//! snapshot, restores the compose file and env rows, starts the instance and
//! waits up to 180 s for a healthy container. Any failure puts the previous
//! data directory, compose file and env rows back and restarts the instance;
//! the row records `failed` and the reason. A successful restore leaves the
//! previous directory next to the new one for the operator to inspect.
//!
//! ## Retention
//!
//! Every snapshot carries an expiry (`ttl_s`, default 7 days, maximum 90).
//! `purge_expired` deletes expired files and rows; it runs at startup and on
//! `DELETE /api/snapshots/expired`.

use crate::api::upgrade::wait_until_healthy;
use crate::api::AppState;
use crate::archive;
use crate::config::{Config, VERSION};
use crate::db::{Db, DbError, InstanceEnvRecord};
use crate::models::{now_iso, InstanceSpec, Status};
use crate::store;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::SecondsFormat;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `format` field of a snapshot manifest.
pub const SNAPSHOT_FORMAT: &str = "fleett.snapshot";
/// Manifest version understood by this build.
pub const FORMAT_VERSION: u32 = 1;
/// Directory under the data root holding every snapshot.
pub const SNAPSHOTS_DIR: &str = "snapshots";
/// Name of the manifest entry.
pub const MANIFEST_NAME: &str = "snapshot.json";
/// Name of the encrypted environment entry.
pub const ENV_NAME: &str = "env.json";
pub const COMPOSE_NAME: &str = "compose.yaml";
pub const CONFIG_NAME: &str = "config.yaml";
pub const SOUL_NAME: &str = "soul.md";
pub const DATA_PREFIX: &str = "data";

/// Default retention of a snapshot (7 days).
pub const DEFAULT_TTL_S: u64 = 7 * 24 * 60 * 60;
/// Maximum retention of a snapshot (90 days).
pub const MAX_TTL_S: u64 = 90 * 24 * 60 * 60;
/// Longest label accepted by the API.
pub const MAX_LABEL_CHARS: usize = 128;
/// How long a restore waits for a healthy container.
pub const RESTORE_TIMEOUT: Duration = Duration::from_secs(180);

/// Snapshot completed successfully and can be restored.
pub const STATUS_OK: &str = "ok";
/// A restore is currently running.
pub const STATUS_RESTORING: &str = "restoring";
/// The last restore failed (the previous state was put back).
pub const STATUS_FAILED: &str = "failed";

/// Directories regenerated at runtime: never archived in a snapshot.
pub const RUNTIME_CACHES: &[&str] = &["__pycache__", ".cache", "cache", "node_modules"];

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("filesystem error: {0}")]
    Io(#[from] io::Error),
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("invalid snapshot: {0}")]
    Invalid(String),
    #[error("snapshot \"{0}\" not found")]
    NotFound(String),
    #[error("instance \"{0}\" not found")]
    InstanceNotFound(String),
    #[error("invalid snapshot JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("snapshot \"{snapshot}\" could not be restored: {message}")]
    RestoreFailed { snapshot: String, message: String },
}

/// One restore point, as stored in the `snapshots` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub instance_id: String,
    /// Absolute path of the `.tgz` on this host.
    pub path: String,
    pub label: String,
    pub created_at: String,
    pub expires_at: String,
    pub size_bytes: u64,
    /// `ok`, `restoring` or `failed`.
    pub status: String,
    pub error: Option<String>,
}

/// Instance metadata carried by `snapshot.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub format: String,
    pub format_version: u32,
    pub id: String,
    pub instance_id: String,
    pub instance_name: String,
    pub image: String,
    #[serde(default)]
    pub label: String,
    pub created_at: String,
    pub expires_at: String,
    pub fleett_version: String,
}

/// One `instance_env` row inside `env.json`.
///
/// A secret row carries the nonce and ciphertext *as stored* (base64), never a
/// decrypted value; a non-secret row carries its plaintext value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvRow {
    pub key: String,
    pub secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ciphertext: Option<String>,
    #[serde(default)]
    pub updated_at: String,
}

/// Body of `env.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvSnapshot {
    /// Provider the instance drew its key from (a reference, never a secret).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Variable name the provider key is injected into.
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub rows: Vec<EnvRow>,
}

impl EnvSnapshot {
    fn from_records(
        records: Vec<InstanceEnvRecord>,
        provider: Option<String>,
        api_key_env: &str,
    ) -> Self {
        Self {
            provider,
            api_key_env: api_key_env.to_string(),
            rows: records
                .into_iter()
                .map(|record| EnvRow {
                    key: record.key,
                    secret: record.secret,
                    value: record.value,
                    nonce: record.nonce.map(|bytes| BASE64.encode(bytes)),
                    ciphertext: record.ciphertext.map(|bytes| BASE64.encode(bytes)),
                    updated_at: record.updated_at,
                })
                .collect(),
        }
    }

    fn into_records(self) -> Result<Vec<InstanceEnvRecord>, SnapshotError> {
        self.rows
            .into_iter()
            .map(|row| {
                let key = row.key.clone();
                let decode =
                    |value: Option<String>, name: &str| -> Result<Option<Vec<u8>>, SnapshotError> {
                        match value {
                            None => Ok(None),
                            Some(text) => BASE64.decode(text.trim()).map(Some).map_err(|err| {
                                SnapshotError::Invalid(format!(
                                    "env.json: {name} of \"{key}\" is not base64: {err}"
                                ))
                            }),
                        }
                    };
                Ok(InstanceEnvRecord {
                    key: row.key,
                    secret: row.secret,
                    value: row.value,
                    nonce: decode(row.nonce, "nonce")?,
                    ciphertext: decode(row.ciphertext, "ciphertext")?,
                    updated_at: row.updated_at,
                })
            })
            .collect()
    }
}

/// Result of one purge run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PurgeReport {
    pub removed: usize,
    /// Snapshot rows kept because their files could not be deleted.
    pub failed: usize,
}

/// Result of one restore.
#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    pub snapshot: Snapshot,
    pub instance_id: String,
    /// Files restored under `data/`.
    pub files: usize,
    /// Observed instance status once the restore succeeded.
    pub status: String,
    pub env_restored: bool,
    pub compose_restored: bool,
    /// The directory the previous data was moved to (kept, not deleted).
    pub previous_data_dir: Option<String>,
}

/// One file of a snapshot comparison.
#[derive(Debug, Clone, Serialize)]
pub struct DiffFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// One file present in both snapshots with different bytes.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedFile {
    pub path: String,
    pub size_a: u64,
    pub size_b: u64,
    pub sha256_a: String,
    pub sha256_b: String,
}

/// File-level difference between two snapshots (no extraction).
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotDiff {
    pub instance_id: String,
    pub a: String,
    pub b: String,
    /// Present in `b`, absent from `a`.
    pub added: Vec<DiffFile>,
    /// Present in `a`, absent from `b`.
    pub removed: Vec<DiffFile>,
    /// Present in both, different bytes.
    pub changed: Vec<ChangedFile>,
    pub unchanged: usize,
}

/// Directory of one snapshot (`<root>/snapshots/<id>`).
fn snapshot_dir(cfg: &Config, id: &str) -> PathBuf {
    cfg.root.join(SNAPSHOTS_DIR).join(id)
}

/// Create one snapshot of `spec`.
///
/// The label is free-form (at most 128 characters, no control character); the
/// TTL must be between 1 second and [`MAX_TTL_S`] (default [`DEFAULT_TTL_S`]).
pub fn create(
    cfg: &Config,
    db: &Db,
    spec: &InstanceSpec,
    label: Option<&str>,
    ttl_s: Option<u64>,
) -> Result<Snapshot, SnapshotError> {
    let label = normalize_label(label)?;
    let ttl_s = normalize_ttl(ttl_s)?;
    let created_at = now_iso();
    let expires_at = expiry(&created_at, ttl_s);
    let id = uuid::Uuid::new_v4().to_string();

    let dir = snapshot_dir(cfg, &id);
    let parent = cfg.root.join(SNAPSHOTS_DIR);
    fs::create_dir_all(&parent)?;
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let file_name = format!("{}.tgz", crate::backup::timestamp());
    let final_path = dir.join(&file_name);
    let tmp_path = dir.join(format!(".{file_name}.tmp"));

    let manifest = SnapshotManifest {
        format: SNAPSHOT_FORMAT.to_string(),
        format_version: FORMAT_VERSION,
        id: id.clone(),
        instance_id: spec.id.clone(),
        instance_name: spec.name.clone(),
        image: spec.image.clone(),
        label: label.clone(),
        created_at: created_at.clone(),
        expires_at: expires_at.clone(),
        fleett_version: VERSION.to_string(),
    };

    let built = build_archive(cfg, db, spec, &manifest, &tmp_path);
    if let Err(err) = built {
        let _ = fs::remove_dir_all(&dir);
        return Err(err);
    }
    fs::rename(&tmp_path, &final_path)?;
    fs::set_permissions(&final_path, fs::Permissions::from_mode(0o600))?;
    let size_bytes = fs::metadata(&final_path)?.len();

    let snapshot = Snapshot {
        id,
        instance_id: spec.id.clone(),
        path: final_path.display().to_string(),
        label,
        created_at,
        expires_at,
        size_bytes,
        status: STATUS_OK.to_string(),
        error: None,
    };
    if let Err(err) = insert(db, &snapshot) {
        let _ = fs::remove_dir_all(&dir);
        return Err(err);
    }
    Ok(snapshot)
}

fn build_archive(
    cfg: &Config,
    db: &Db,
    spec: &InstanceSpec,
    manifest: &SnapshotManifest,
    destination: &Path,
) -> Result<(), SnapshotError> {
    let file = File::create(destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
    let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::default()));

    archive::append_bytes(
        &mut builder,
        Path::new(MANIFEST_NAME),
        &serde_json::to_vec_pretty(manifest)?,
        0o600,
    )?;

    let compose_path = cfg.compose_path(&spec.id);
    if let Ok(compose) = fs::read(&compose_path) {
        archive::append_bytes(&mut builder, Path::new(COMPOSE_NAME), &compose, 0o644)?;
    }
    let data = cfg.data_dir(&spec.id);
    if let Ok(config) = fs::read(data.join("config.yaml")) {
        archive::append_bytes(&mut builder, Path::new(CONFIG_NAME), &config, 0o600)?;
    }
    if let Ok(soul) = fs::read(data.join("SOUL.md")) {
        archive::append_bytes(&mut builder, Path::new(SOUL_NAME), &soul, 0o600)?;
    }
    let env = EnvSnapshot::from_records(
        db.list_instance_env(&spec.id)?,
        spec.provider.clone(),
        &spec.api_key_env,
    );
    archive::append_bytes(
        &mut builder,
        Path::new(ENV_NAME),
        &serde_json::to_vec_pretty(&env)?,
        0o600,
    )?;

    if data.is_dir() {
        archive::append_dir(&mut builder, Path::new(DATA_PREFIX), 0o700)?;
        archive::append_tree_excluding(&mut builder, &data, Path::new(DATA_PREFIX), |name| {
            is_runtime_cache(name)
        })?;
    }

    let file = builder.into_inner()?.finish()?;
    file.sync_all()?;
    Ok(())
}

fn is_runtime_cache(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| RUNTIME_CACHES.contains(&name))
}

/// List the snapshots of one instance, newest first.
pub fn list(db: &Db, instance_id: &str) -> Result<Vec<Snapshot>, SnapshotError> {
    let rows = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, instance_id, path, label, created_at, expires_at, size_bytes, status, \
             error FROM snapshots WHERE instance_id = ?1 ORDER BY created_at DESC, id DESC",
        )?;
        let rows = statement
            .query_map(params![instance_id], row_to_snapshot)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;
    Ok(rows)
}

/// One snapshot row, or `404`.
pub fn get(db: &Db, id: &str) -> Result<Snapshot, SnapshotError> {
    let row = db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, instance_id, path, label, created_at, expires_at, size_bytes, status, \
             error FROM snapshots WHERE id = ?1",
            params![id],
            row_to_snapshot,
        )
        .optional()
    })?;
    row.ok_or_else(|| SnapshotError::NotFound(id.to_string()))
}

/// Delete one snapshot (file and row).
pub fn delete(cfg: &Config, db: &Db, id: &str) -> Result<Snapshot, SnapshotError> {
    let snapshot = get(db, id)?;
    remove_snapshot_dir(cfg, id)?;
    let deleted =
        db.with_conn(|conn| conn.execute("DELETE FROM snapshots WHERE id = ?1", params![id]))?;
    if deleted == 0 {
        return Err(SnapshotError::NotFound(id.to_string()));
    }
    Ok(snapshot)
}

/// Restore one snapshot into its instance.
///
/// The caller must hold the instance lock (`AppState::locks`). The flow is
/// documented at module level; every failure after the first change puts the
/// previous data directory, compose file and env rows back before returning.
pub async fn restore(state: &AppState, snapshot_id: &str) -> Result<RestoreReport, SnapshotError> {
    let mut snapshot = get(&state.db, snapshot_id)?;
    let mut spec = state
        .db
        .get(&snapshot.instance_id)?
        .ok_or_else(|| SnapshotError::InstanceNotFound(snapshot.instance_id.clone()))?;

    let archive_path = PathBuf::from(&snapshot.path);
    let metadata = read_metadata(&archive_path)?;
    if metadata.manifest.instance_id != spec.id || metadata.manifest.id != snapshot.id {
        return Err(SnapshotError::Invalid(format!(
            "the archive describes instance \"{}\" / snapshot \"{}\", not \"{}\" / \"{}\"",
            metadata.manifest.instance_id, metadata.manifest.id, spec.id, snapshot.id
        )));
    }

    let compose_path = state.cfg.compose_path(&spec.id);
    let previous_compose = match fs::read(&compose_path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(SnapshotError::Io(err)),
    };
    let previous_env = state.db.list_instance_env(&spec.id)?;
    let old_image = spec.image.clone();
    let data_existed = state.cfg.data_dir(&spec.id).exists();
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let previous_data = unique_sibling(
        &state.cfg.instance_dir(&spec.id),
        &format!("data.pre-restore-{stamp}"),
    );

    set_status(&state.db, &snapshot.id, STATUS_RESTORING, None)?;

    match apply_restore(state, &mut spec, &archive_path, &metadata, &previous_data).await {
        Ok(files) => {
            let observed = state
                .driver
                .ps(&spec)
                .await
                .map(|result| result.status)
                .unwrap_or(Status::Running);
            let _ = state.db.set_status(&spec.id, observed, None);
            let _ = state.db.set_image(&spec.id, &spec.image);
            if let Err(err) = crate::history::reconcile(&state.cfg, &spec.id) {
                tracing::warn!(
                    instance = %spec.id,
                    %err,
                    "snapshot restore: could not reconcile the instance history"
                );
            }
            set_status(&state.db, &snapshot.id, STATUS_OK, None)?;
            snapshot.status = STATUS_OK.to_string();
            snapshot.error = None;
            Ok(RestoreReport {
                snapshot,
                instance_id: spec.id.clone(),
                files,
                status: observed.as_str().to_string(),
                env_restored: metadata.env.is_some(),
                compose_restored: metadata.compose.is_some(),
                previous_data_dir: previous_data
                    .is_dir()
                    .then(|| previous_data.display().to_string()),
            })
        }
        Err(message) => {
            rollback_restore(
                state,
                &mut spec,
                &previous_data,
                data_existed,
                previous_compose.as_deref(),
                metadata.compose.is_some(),
                metadata.env.is_some(),
                &previous_env,
                &old_image,
                &message,
            )
            .await;
            let _ = state.db.set_status(&spec.id, Status::Error, Some(&message));
            set_status(&state.db, &snapshot.id, STATUS_FAILED, Some(&message))?;
            Err(SnapshotError::RestoreFailed {
                snapshot: snapshot.id,
                message,
            })
        }
    }
}

async fn apply_restore(
    state: &AppState,
    spec: &mut InstanceSpec,
    archive_path: &Path,
    metadata: &SnapshotMetadata,
    previous_data: &Path,
) -> Result<usize, String> {
    if let Err(err) = state.driver.stop(spec).await {
        return Err(format!("cannot stop the instance: {}", err.detail.trim()));
    }

    let data = state.cfg.data_dir(&spec.id);
    if data.exists() {
        if let Err(err) = fs::rename(&data, previous_data) {
            return Err(format!(
                "cannot move the current data directory to {}: {err}",
                previous_data.display()
            ));
        }
    }
    if let Err(err) = fs::create_dir_all(&data) {
        return Err(format!("cannot create the data directory: {err}"));
    }
    let _ = fs::set_permissions(&data, fs::Permissions::from_mode(0o700));

    let files = match extract_data(archive_path, &data) {
        Ok(files) => files,
        Err(err) => return Err(format!("cannot extract the snapshot: {err}")),
    };

    // The root copies are authoritative: they survive a truncated data tree.
    if let Some(config) = &metadata.config {
        if let Err(err) = write_private(&data.join("config.yaml"), config) {
            return Err(format!("cannot restore config.yaml: {err}"));
        }
    }
    if let Some(soul) = &metadata.soul {
        if let Err(err) = write_private(&data.join("SOUL.md"), soul) {
            return Err(format!("cannot restore SOUL.md: {err}"));
        }
    }
    if let Err(err) = store::finalize_imported_data(&state.cfg, &spec.id) {
        return Err(format!("cannot finalise the restored data: {err}"));
    }

    if let Some(compose) = &metadata.compose {
        let text = String::from_utf8_lossy(compose);
        store::write_compose(&state.cfg, &spec.id, &text)
            .map_err(|err| format!("cannot restore the compose file: {err}"))?;
        if let Some(image) = store::compose_image(&text) {
            spec.image = image;
        }
    }

    if let Some(env) = &metadata.env {
        let records = env
            .clone()
            .into_records()
            .map_err(|err| format!("cannot read env.json: {err}"))?;
        state
            .db
            .replace_instance_env(&spec.id, &records)
            .map_err(|err| format!("cannot restore the environment: {err}"))?;
    }

    if let Err(err) = state.driver.up(spec).await {
        return Err(format!("docker compose up failed: {}", err.detail.trim()));
    }
    wait_until_healthy(state, spec, RESTORE_TIMEOUT)
        .await
        .map_err(|reason| format!("the restored instance is not healthy: {reason}"))?;
    Ok(files)
}

#[allow(clippy::too_many_arguments)]
async fn rollback_restore(
    state: &AppState,
    spec: &mut InstanceSpec,
    previous_data: &Path,
    data_existed: bool,
    previous_compose: Option<&[u8]>,
    compose_applied: bool,
    env_applied: bool,
    previous_env: &[InstanceEnvRecord],
    old_image: &str,
    reason: &str,
) {
    let _ = state.driver.stop(spec).await;

    let data = state.cfg.data_dir(&spec.id);
    if previous_data.exists() {
        // The live directory was moved aside: drop the partial extraction and
        // put the original back.
        let _ = fs::remove_dir_all(&data);
        if let Err(err) = fs::rename(previous_data, &data) {
            tracing::error!(
                instance = %spec.id,
                from = %previous_data.display(),
                %err,
                "snapshot restore: cannot put the previous data directory back"
            );
        }
    } else if !data_existed {
        // There was no data directory before the attempt: remove what the
        // failed extraction created.
        let _ = fs::remove_dir_all(&data);
    }
    // `!previous_data.exists() && data_existed` means the failure happened
    // before the live directory was moved (stop or rename failed): the data on
    // disk is still the original one and must not be touched.

    if compose_applied {
        let compose_path = state.cfg.compose_path(&spec.id);
        match previous_compose {
            Some(bytes) => {
                if let Err(err) = fs::write(&compose_path, bytes) {
                    tracing::error!(instance = %spec.id, %err, "snapshot restore: cannot restore the compose file");
                }
            }
            None => {
                let _ = fs::remove_file(&compose_path);
            }
        }
    }

    if env_applied {
        // `replace_instance_env` also drops the legacy `instance_secrets` row;
        // skip it entirely when the rows were not modified (the failure
        // happened before the environment step).
        let current = state.db.list_instance_env(&spec.id);
        let unchanged = matches!(&current, Ok(rows) if rows.as_slice() == previous_env);
        if !unchanged {
            if let Err(err) = state.db.replace_instance_env(&spec.id, previous_env) {
                tracing::error!(instance = %spec.id, %err, "snapshot restore: cannot restore the environment");
            }
        }
    }

    spec.image = old_image.to_string();
    if let Err(err) = state.driver.up(spec).await {
        tracing::warn!(
            instance = %spec.id,
            %err,
            "snapshot restore: the previous instance could not be restarted"
        );
    }
    tracing::warn!(
        instance = %spec.id,
        %reason,
        "snapshot restore failed; the previous data and compose file were put back"
    );
}

/// What `read_metadata` found in one archive.
#[derive(Debug)]
struct SnapshotMetadata {
    manifest: SnapshotManifest,
    env: Option<EnvSnapshot>,
    compose: Option<Vec<u8>>,
    config: Option<Vec<u8>>,
    soul: Option<Vec<u8>>,
}

/// Read the metadata entries of an archive without extracting `data/`.
fn read_metadata(path: &Path) -> Result<SnapshotMetadata, SnapshotError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(SnapshotError::Invalid(format!(
                "the archive file {} is missing",
                path.display()
            )))
        }
        Err(err) => return Err(SnapshotError::Io(err)),
    };
    let mut tar = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
    let mut manifest: Option<SnapshotManifest> = None;
    let mut env: Option<EnvSnapshot> = None;
    let mut compose: Option<Vec<u8>> = None;
    let mut config: Option<Vec<u8>> = None;
    let mut soul: Option<Vec<u8>> = None;
    let mut seen_bytes = 0u64;

    let entries = tar
        .entries()
        .map_err(|err| SnapshotError::Invalid(format!("cannot read the archive: {err}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|err| SnapshotError::Invalid(format!("cannot read an entry: {err}")))?;
        let entry_path = entry
            .path()
            .map_err(|err| SnapshotError::Invalid(format!("cannot read an entry path: {err}")))?
            .into_owned();
        archive::safe_path(&entry_path).map_err(SnapshotError::Invalid)?;
        seen_bytes = seen_bytes.saturating_add(entry.size());
        if seen_bytes > archive::MAX_EXTRACTED_BYTES {
            return Err(SnapshotError::Invalid(format!(
                "the archive expands beyond {} MiB",
                archive::MAX_EXTRACTED_BYTES / (1024 * 1024)
            )));
        }
        match entry_path.as_path() {
            p if p == Path::new(MANIFEST_NAME) => {
                manifest = Some(serde_json::from_slice(&read_entry(&mut entry)?).map_err(
                    |err| SnapshotError::Invalid(format!("invalid {MANIFEST_NAME}: {err}")),
                )?);
            }
            p if p == Path::new(ENV_NAME) => {
                env = Some(
                    serde_json::from_slice(&read_entry(&mut entry)?).map_err(|err| {
                        SnapshotError::Invalid(format!("invalid {ENV_NAME}: {err}"))
                    })?,
                );
            }
            p if p == Path::new(COMPOSE_NAME) => compose = Some(read_entry(&mut entry)?),
            p if p == Path::new(CONFIG_NAME) => config = Some(read_entry(&mut entry)?),
            p if p == Path::new(SOUL_NAME) => soul = Some(read_entry(&mut entry)?),
            _ => {}
        }
    }

    let manifest = manifest
        .ok_or_else(|| SnapshotError::Invalid(format!("the archive has no {MANIFEST_NAME}")))?;
    if manifest.format != SNAPSHOT_FORMAT {
        return Err(SnapshotError::Invalid(format!(
            "unsupported snapshot format {:?}; this build reads {SNAPSHOT_FORMAT:?} v{FORMAT_VERSION}",
            manifest.format
        )));
    }
    if manifest.format_version != FORMAT_VERSION {
        return Err(SnapshotError::Invalid(format!(
            "unsupported snapshot version {}; this build reads v{FORMAT_VERSION}",
            manifest.format_version
        )));
    }
    Ok(SnapshotMetadata {
        manifest,
        env,
        compose,
        config,
        soul,
    })
}

/// Read one small metadata entry with the shared size cap.
fn read_entry<R: Read>(entry: &mut tar::Entry<'_, R>) -> Result<Vec<u8>, SnapshotError> {
    archive::read_metadata(entry)
        .map_err(|err| SnapshotError::Invalid(format!("cannot read metadata: {err}")))
}

/// Extract every `data/**` entry of one archive into `data_dir`.
fn extract_data(archive_path: &Path, data_dir: &Path) -> Result<usize, SnapshotError> {
    let file = File::open(archive_path)?;
    let mut tar = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
    let mut extracted_bytes = 0u64;
    let mut files = 0usize;

    let entries = tar
        .entries()
        .map_err(|err| SnapshotError::Invalid(format!("cannot read the archive: {err}")))?;
    for entry in entries {
        let entry =
            entry.map_err(|err| SnapshotError::Invalid(format!("cannot read an entry: {err}")))?;
        let entry_path = entry
            .path()
            .map_err(|err| SnapshotError::Invalid(format!("cannot read an entry path: {err}")))?
            .into_owned();
        archive::safe_path(&entry_path).map_err(SnapshotError::Invalid)?;
        let Some(name) = entry_path.file_name().map(|name| name.to_owned()) else {
            continue;
        };
        // Same names the writer refuses to store (secrets.env, *.key).
        if archive::skip_extracted_entry(&name) {
            continue;
        }
        let Some(relative) = entry_path
            .strip_prefix(DATA_PREFIX)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
        else {
            continue;
        };
        extracted_bytes = extracted_bytes.saturating_add(entry.size());
        if extracted_bytes > archive::MAX_EXTRACTED_BYTES {
            return Err(SnapshotError::Invalid(format!(
                "the archive expands beyond {} MiB",
                archive::MAX_EXTRACTED_BYTES / (1024 * 1024)
            )));
        }
        let is_dir = entry.header().entry_type().is_dir();
        archive::write_entry(entry, &data_dir.join(relative)).map_err(|err| {
            SnapshotError::Invalid(format!("cannot restore {}: {err}", entry_path.display()))
        })?;
        if !is_dir {
            files += 1;
        }
    }
    Ok(files)
}

/// Hash every file of one archive (path -> size, SHA-256 hex).
fn hash_archive(path: &Path) -> Result<BTreeMap<String, (u64, String)>, SnapshotError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(SnapshotError::Invalid(format!(
                "the archive file {} is missing",
                path.display()
            )))
        }
        Err(err) => return Err(SnapshotError::Io(err)),
    };
    let mut tar = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
    let mut files = BTreeMap::new();
    let entries = tar
        .entries()
        .map_err(|err| SnapshotError::Invalid(format!("cannot read the archive: {err}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|err| SnapshotError::Invalid(format!("cannot read an entry: {err}")))?;
        let entry_path = entry
            .path()
            .map_err(|err| SnapshotError::Invalid(format!("cannot read an entry path: {err}")))?
            .into_owned();
        archive::safe_path(&entry_path).map_err(SnapshotError::Invalid)?;
        let display = entry_path.to_string_lossy().to_string();
        if display == MANIFEST_NAME {
            continue;
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            continue;
        }
        if entry_type.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|err| SnapshotError::Invalid(format!("cannot read a link: {err}")))?
                .map(|target| target.to_string_lossy().to_string())
                .unwrap_or_default();
            files.insert(display, (0, sha256_hex(target.as_bytes())));
            continue;
        }
        if !entry_type.is_file() {
            continue;
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut size = 0u64;
        loop {
            let read = entry
                .read(&mut buffer)
                .map_err(|err| SnapshotError::Invalid(format!("cannot read {display}: {err}")))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            size = size.saturating_add(read as u64);
        }
        files.insert(display, (size, hex(&hasher.finalize())));
    }
    Ok(files)
}

/// File-level difference between two snapshots of one instance.
pub fn compare(
    db: &Db,
    instance_id: &str,
    a: &str,
    b: &str,
) -> Result<SnapshotDiff, SnapshotError> {
    let left = get(db, a)?;
    let right = get(db, b)?;
    for snapshot in [&left, &right] {
        if snapshot.instance_id != instance_id {
            return Err(SnapshotError::Invalid(format!(
                "snapshot \"{}\" belongs to instance \"{}\"",
                snapshot.id, snapshot.instance_id
            )));
        }
    }
    let left_files = hash_archive(Path::new(&left.path))?;
    let right_files = hash_archive(Path::new(&right.path))?;

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut unchanged = 0usize;

    for (path, (size, hash)) in &left_files {
        match right_files.get(path) {
            None => removed.push(DiffFile {
                path: path.clone(),
                size: *size,
                sha256: hash.clone(),
            }),
            Some((right_size, right_hash)) if right_hash == hash => unchanged += 1,
            Some((right_size, right_hash)) => changed.push(ChangedFile {
                path: path.clone(),
                size_a: *size,
                size_b: *right_size,
                sha256_a: hash.clone(),
                sha256_b: right_hash.clone(),
            }),
        }
    }
    for (path, (size, hash)) in &right_files {
        if !left_files.contains_key(path) {
            added.push(DiffFile {
                path: path.clone(),
                size: *size,
                sha256: hash.clone(),
            });
        }
    }

    Ok(SnapshotDiff {
        instance_id: instance_id.to_string(),
        a: left.id,
        b: right.id,
        added,
        removed,
        changed,
        unchanged,
    })
}

/// Delete every snapshot whose retention elapsed.
pub fn purge_expired(cfg: &Config, db: &Db) -> Result<PurgeReport, SnapshotError> {
    let now = now_iso();
    let expired = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, instance_id, path, label, created_at, expires_at, size_bytes, status, \
             error FROM snapshots WHERE status != ?1 AND expires_at <= ?2 \
             ORDER BY expires_at",
        )?;
        let rows = statement
            .query_map(params![STATUS_RESTORING, now], row_to_snapshot)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;

    let mut report = PurgeReport::default();
    for snapshot in expired {
        match remove_snapshot_dir(cfg, &snapshot.id) {
            Ok(()) => {
                db.with_conn(|conn| {
                    conn.execute("DELETE FROM snapshots WHERE id = ?1", params![snapshot.id])
                })?;
                report.removed += 1;
            }
            Err(err) => {
                tracing::warn!(
                    snapshot = %snapshot.id,
                    %err,
                    "snapshots: cannot delete an expired snapshot"
                );
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

/// Startup housekeeping: purge expired snapshots, best effort.
pub fn startup(cfg: &Config, db: &Db) {
    match purge_expired(cfg, db) {
        Ok(report) if report.removed > 0 || report.failed > 0 => tracing::info!(
            removed = report.removed,
            failed = report.failed,
            "snapshots: retention applied"
        ),
        Ok(_) => {}
        Err(err) => tracing::warn!(%err, "snapshots: cannot apply the retention policy"),
    }
}

/// Take a snapshot before a risky operation, best effort.
///
/// A failure only logs a warning: the operation continues, exactly like a
/// backup that cannot be written must not block an upgrade. Returns the id of
/// the snapshot when one was created.
pub async fn auto_before(state: &AppState, instance_id: &str, reason: &str) -> Option<String> {
    let cfg = state.cfg.clone();
    let db = state.db.clone();
    let id = instance_id.to_string();
    let label = format!("auto: {reason}");
    let result = tokio::task::spawn_blocking(move || {
        let spec = db
            .get(&id)
            .ok()
            .flatten()
            .ok_or_else(|| format!("instance \"{id}\" is not indexed"))?;
        create(&cfg, &db, &spec, Some(&label), None)
            .map(|snapshot| snapshot.id)
            .map_err(|err| err.to_string())
    })
    .await;
    match result {
        Ok(Ok(snapshot_id)) => {
            tracing::info!(
                instance = %instance_id,
                reason,
                snapshot = %snapshot_id,
                "automatic snapshot taken before a risky operation"
            );
            Some(snapshot_id)
        }
        Ok(Err(err)) => {
            tracing::warn!(
                instance = %instance_id,
                reason,
                %err,
                "automatic snapshot failed; the operation continues without a restore point"
            );
            None
        }
        Err(err) => {
            tracing::warn!(
                instance = %instance_id,
                reason,
                %err,
                "automatic snapshot task failed; the operation continues"
            );
            None
        }
    }
}

fn normalize_label(label: Option<&str>) -> Result<String, SnapshotError> {
    let value = label.unwrap_or_default().trim();
    if value.chars().count() > MAX_LABEL_CHARS {
        return Err(SnapshotError::Invalid(format!(
            "label must be at most {MAX_LABEL_CHARS} characters"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(SnapshotError::Invalid(
            "label must not contain control characters".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn normalize_ttl(ttl_s: Option<u64>) -> Result<u64, SnapshotError> {
    match ttl_s {
        None => Ok(DEFAULT_TTL_S),
        Some(0) => Err(SnapshotError::Invalid(
            "ttl_s must be at least 1 second".to_string(),
        )),
        Some(value) if value > MAX_TTL_S => Err(SnapshotError::Invalid(format!(
            "ttl_s must be at most {MAX_TTL_S} seconds ({} days)",
            MAX_TTL_S / 86_400
        ))),
        Some(value) => Ok(value),
    }
}

/// RFC 3339 expiry, second precision (lexicographically sortable).
fn expiry(created_at: &str, ttl_s: u64) -> String {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|created| {
            (created + chrono::Duration::seconds(ttl_s as i64))
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        })
        .unwrap_or_else(|_| {
            (chrono::Utc::now() + chrono::Duration::seconds(ttl_s as i64))
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        })
}

fn unique_sibling(parent: &Path, name: &str) -> PathBuf {
    let mut candidate = parent.join(name);
    let mut attempt = 1;
    while candidate.exists() {
        candidate = parent.join(format!("{name}-{attempt}"));
        attempt += 1;
    }
    candidate
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::write(path, bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

fn remove_snapshot_dir(cfg: &Config, id: &str) -> io::Result<()> {
    match fs::remove_dir_all(snapshot_dir(cfg, id)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn insert(db: &Db, snapshot: &Snapshot) -> Result<(), SnapshotError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO snapshots (id, instance_id, path, label, created_at, expires_at, \
             size_bytes, status, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                snapshot.id,
                snapshot.instance_id,
                snapshot.path,
                snapshot.label,
                snapshot.created_at,
                snapshot.expires_at,
                snapshot.size_bytes as i64,
                snapshot.status,
                snapshot.error,
            ],
        )
    })?;
    Ok(())
}

fn set_status(db: &Db, id: &str, status: &str, error: Option<&str>) -> Result<(), SnapshotError> {
    let updated = db.with_conn(|conn| {
        conn.execute(
            "UPDATE snapshots SET status = ?2, error = ?3 WHERE id = ?1",
            params![id, status, error],
        )
    })?;
    if updated == 0 {
        return Err(SnapshotError::NotFound(id.to_string()));
    }
    Ok(())
}

fn row_to_snapshot(row: &Row<'_>) -> rusqlite::Result<Snapshot> {
    Ok(Snapshot {
        id: row.get(0)?,
        instance_id: row.get(1)?,
        path: row.get(2)?,
        label: row.get(3)?,
        created_at: row.get(4)?,
        expires_at: row.get(5)?,
        size_bytes: row.get::<_, i64>(6)?.max(0) as u64,
        status: row.get(7)?,
        error: row.get(8)?,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::mock::MockDriver;
    use crate::models::{ChannelKind, Health, DEFAULT_API_KEY_ENV};
    use std::sync::Arc;

    const KEY_BYTES: [u8; crate::secrets::KEY_LEN] = [7u8; crate::secrets::KEY_LEN];
    const SECRET_VALUE: &str = "s3cr3t-token-value-never-on-disk";

    fn temp_config() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().join("root"));
        cfg.secrets_key = Some(crate::secrets::encode_key(&KEY_BYTES));
        store::ensure_layout(&cfg).expect("layout");
        (dir, cfg)
    }

    fn spec(image: &str) -> InstanceSpec {
        let now = now_iso();
        InstanceSpec {
            id: "alice".to_string(),
            name: "Alice".to_string(),
            template: "guest".to_string(),
            model: "deepseek-v4.1-flash".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            provider: None,
            channel: ChannelKind::None,
            shares: Vec::new(),
            skills: Vec::new(),
            mem_limit_mb: 1024,
            cpu_limit: 1.0,
            image: image.to_string(),
            previous_image: None,
            allow_lan: false,
            host: crate::hosts::LOCAL_HOST.to_string(),
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            disk_quota_mb: 0,
            disk_policy: crate::models::DiskPolicy::Warn,
            notes: String::new(),
            status: Status::Stopped,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    fn seeded() -> (tempfile::TempDir, Config, Db, InstanceSpec) {
        let (dir, cfg) = temp_config();
        let spec = spec("hermes-agent:0.22.2-local");
        let db = Db::open(&cfg.db_path()).expect("db");
        db.insert_instance(&spec).expect("insert");
        store::write_instance(
            &cfg,
            &spec,
            &crate::render::RenderedInstance {
                compose_yaml: format!(
                    "name: fleett-alice\nservices:\n  agent:\n    image: {}\n",
                    spec.image
                ),
                config_yaml: "model: {}\n".to_string(),
                soul_md: Some("SOUL original\n".to_string()),
            },
            true,
        )
        .expect("write instance");
        let data = cfg.data_dir("alice");
        fs::write(data.join("MEMORY.md"), "remember\n").expect("memory");
        fs::create_dir_all(data.join("cache")).expect("cache dir");
        fs::write(data.join("cache/blob.bin"), "cache\n").expect("cache file");
        fs::create_dir_all(data.join("sessions")).expect("sessions");
        fs::write(data.join("sessions/one.json"), "{}").expect("session");
        fs::write(cfg.secrets_path("alice"), "TOKEN=legacy-leak\n").expect("legacy secret");
        crate::envs::replace(
            &cfg,
            &db,
            "alice",
            &[
                crate::envs::EnvWrite {
                    key: "DISCORD_ALLOWED_USERS".to_string(),
                    value: "123".to_string(),
                    secret: false,
                },
                crate::envs::EnvWrite {
                    key: "TELEGRAM_BOT_TOKEN".to_string(),
                    value: SECRET_VALUE.to_string(),
                    secret: true,
                },
            ],
        )
        .expect("env rows");
        (dir, cfg, db, spec)
    }

    fn read_entries(path: &Path) -> Vec<(String, Vec<u8>)> {
        let file = File::open(path).expect("open archive");
        let mut tar = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
        let mut entries = Vec::new();
        for entry in tar.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let name = entry.path().expect("path").display().to_string();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read entry");
            entries.push((name, bytes));
        }
        entries
    }

    fn state(cfg: &Config, db: Db, mock: Arc<MockDriver>) -> AppState {
        AppState::bootstrap_with_db(cfg.clone(), mock, db).expect("state")
    }

    #[test]
    fn a_snapshot_holds_data_and_its_metadata_without_a_plaintext_secret() {
        let (_dir, cfg, db, spec) = seeded();
        let snapshot =
            create(&cfg, &db, &spec, Some("before upgrade"), Some(3600)).expect("create snapshot");

        assert_eq!(snapshot.status, STATUS_OK);
        assert_eq!(snapshot.label, "before upgrade");
        assert!(snapshot.size_bytes > 0);
        assert!(Path::new(&snapshot.path).is_file());
        assert!(snapshot.path.contains(&snapshot.id));
        assert!(snapshot.expires_at > snapshot.created_at);

        let entries = read_entries(Path::new(&snapshot.path));
        let raw = fs::read(&snapshot.path).expect("raw archive");
        assert!(
            !String::from_utf8_lossy(&raw).contains(SECRET_VALUE),
            "the compressed archive must not contain the token either"
        );
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        for expected in [
            "snapshot.json",
            "compose.yaml",
            "config.yaml",
            "soul.md",
            "env.json",
            "data",
            "data/SOUL.md",
            "data/MEMORY.md",
            "data/sessions/one.json",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        assert!(
            !names.iter().any(|name| name.contains("cache")),
            "runtime caches are not archived: {names:?}"
        );
        assert!(!names.iter().any(|name| name.ends_with("secrets.env")));

        // No entry carries a plaintext token, and the legacy file never left
        // the host.
        for (name, bytes) in &entries {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains(SECRET_VALUE), "plaintext secret in {name}");
            assert!(!text.contains("legacy-leak"), "legacy secret in {name}");
        }
        let env_json = entries
            .iter()
            .find(|(name, _)| name == "env.json")
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).to_string())
            .expect("env.json");
        assert!(env_json.contains("ciphertext"), "{env_json}");
        assert!(env_json.contains("\"123\""), "plain values stay readable");
        let manifest = entries
            .iter()
            .find(|(name, _)| name == "snapshot.json")
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).to_string())
            .expect("manifest");
        assert!(
            manifest.contains("\"instance_id\": \"alice\""),
            "{manifest}"
        );
    }

    #[tokio::test]
    async fn restoring_brings_back_soul_and_env_rows() {
        let (_dir, cfg, db, spec) = seeded();
        let snapshot = create(&cfg, &db, &spec, None, None).expect("snapshot");

        // Change the instance after the restore point.
        fs::write(cfg.data_dir("alice").join("SOUL.md"), "SOUL broken\n").expect("soul");
        crate::envs::replace(
            &cfg,
            &db,
            "alice",
            &[crate::envs::EnvWrite {
                key: "TELEGRAM_BOT_TOKEN".to_string(),
                value: "rotated".to_string(),
                secret: true,
            }],
        )
        .expect("rotate");

        let mock = Arc::new(MockDriver::new());
        let state = state(&cfg, db, mock);
        let report = restore(&state, &snapshot.id).await.expect("restore");

        assert_eq!(
            fs::read_to_string(state.cfg.data_dir("alice").join("SOUL.md")).expect("soul"),
            "SOUL original\n"
        );
        assert_eq!(
            fs::read_to_string(state.cfg.data_dir("alice").join("MEMORY.md")).expect("memory"),
            "remember\n"
        );
        let values = crate::envs::values(&state.cfg, &state.db, "alice").expect("values");
        assert_eq!(
            values.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
            Some(SECRET_VALUE)
        );
        assert_eq!(
            values.get("DISCORD_ALLOWED_USERS").map(String::as_str),
            Some("123")
        );
        assert!(report.files >= 2, "{report:?}");
        assert!(report.env_restored);
        assert!(report.compose_restored);
        assert_eq!(report.status, "running");
        assert!(report
            .previous_data_dir
            .as_deref()
            .is_some_and(|path| Path::new(path).is_dir()));
        assert_eq!(get(&state.db, &snapshot.id).expect("row").status, STATUS_OK);
        assert_eq!(
            state
                .db
                .get("alice")
                .expect("spec")
                .expect("present")
                .status,
            Status::Running
        );
    }

    #[tokio::test]
    async fn a_failed_restore_puts_the_previous_data_compose_and_env_back() {
        let (_dir, cfg, db, spec) = seeded();
        let snapshot = create(&cfg, &db, &spec, None, None).expect("snapshot");

        fs::write(cfg.data_dir("alice").join("SOUL.md"), "SOUL broken\n").expect("soul");
        crate::envs::replace(
            &cfg,
            &db,
            "alice",
            &[crate::envs::EnvWrite {
                key: "TELEGRAM_BOT_TOKEN".to_string(),
                value: "rotated".to_string(),
                secret: true,
            }],
        )
        .expect("rotate");
        let compose_before = fs::read_to_string(cfg.compose_path("alice")).expect("compose");

        let mock = Arc::new(MockDriver::new());
        mock.set_image_health(&spec.image, Health::Unhealthy);
        let state = state(&cfg, db, mock);
        let err = restore(&state, &snapshot.id)
            .await
            .expect_err("unhealthy restore");
        assert!(matches!(err, SnapshotError::RestoreFailed { .. }), "{err}");

        assert_eq!(
            fs::read_to_string(state.cfg.data_dir("alice").join("SOUL.md")).expect("soul"),
            "SOUL broken\n",
            "the previous data must be back"
        );
        let values = crate::envs::values(&state.cfg, &state.db, "alice").expect("values");
        assert_eq!(
            values.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
            Some("rotated"),
            "the previous env rows must be back"
        );
        assert_eq!(
            fs::read_to_string(state.cfg.compose_path("alice")).expect("compose"),
            compose_before
        );
        let stored = get(&state.db, &snapshot.id).expect("row");
        assert_eq!(stored.status, STATUS_FAILED);
        assert!(stored.error.is_some());
        assert_eq!(
            state
                .db
                .get("alice")
                .expect("spec")
                .expect("present")
                .status,
            Status::Error
        );
    }

    #[tokio::test]
    async fn a_restore_that_cannot_stop_the_instance_leaves_the_data_untouched() {
        let (_dir, cfg, db, spec) = seeded();
        let snapshot = create(&cfg, &db, &spec, None, None).expect("snapshot");
        fs::write(cfg.data_dir("alice").join("SOUL.md"), "live data\n").expect("soul");

        let mock = Arc::new(MockDriver::new());
        mock.fail_next("stop", "docker daemon unreachable");
        let state = state(&cfg, db, mock);
        let err = restore(&state, &snapshot.id)
            .await
            .expect_err("the stop must fail the restore");
        assert!(matches!(err, SnapshotError::RestoreFailed { .. }), "{err}");
        assert_eq!(
            fs::read_to_string(state.cfg.data_dir("alice").join("SOUL.md")).expect("soul"),
            "live data\n",
            "a restore that never moved the data must not delete it"
        );
        let leftovers: Vec<String> = fs::read_dir(state.cfg.instance_dir("alice"))
            .expect("instance dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with("data.pre-restore"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        assert_eq!(
            get(&state.db, &snapshot.id).expect("row").status,
            STATUS_FAILED
        );
    }

    #[test]
    fn expired_snapshots_are_purged_with_their_files() {
        let (_dir, cfg, db, spec) = seeded();
        let snapshot = create(&cfg, &db, &spec, None, Some(1)).expect("snapshot");
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE snapshots SET expires_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
                params![snapshot.id],
            )
        })
        .expect("expire");

        let report = purge_expired(&cfg, &db).expect("purge");
        assert_eq!(report.removed, 1);
        assert_eq!(report.failed, 0);
        assert!(matches!(
            get(&db, &snapshot.id),
            Err(SnapshotError::NotFound(_))
        ));
        assert!(!Path::new(&snapshot.path).exists());
    }

    #[test]
    fn comparing_two_snapshots_reports_file_differences() {
        let (_dir, cfg, db, spec) = seeded();
        let first = create(&cfg, &db, &spec, Some("a"), None).expect("first");
        fs::write(cfg.data_dir("alice").join("SOUL.md"), "SOUL changed\n").expect("soul");
        fs::write(cfg.data_dir("alice").join("NEW.md"), "new\n").expect("new file");
        let second = create(&cfg, &db, &spec, Some("b"), None).expect("second");

        let diff = compare(&db, "alice", &first.id, &second.id).expect("compare");
        let changed: Vec<&str> = diff.changed.iter().map(|file| file.path.as_str()).collect();
        assert!(changed.contains(&"data/SOUL.md"), "{diff:?}");
        let added: Vec<&str> = diff.added.iter().map(|file| file.path.as_str()).collect();
        assert!(added.contains(&"data/NEW.md"), "{diff:?}");
        assert!(diff.removed.is_empty(), "{diff:?}");
        assert!(diff.unchanged > 0);

        // A snapshot compared with itself has no difference at all.
        let same = compare(&db, "alice", &first.id, &first.id).expect("self compare");
        assert!(same.added.is_empty() && same.removed.is_empty() && same.changed.is_empty());

        // A snapshot of another instance is refused.
        assert!(matches!(
            compare(&db, "bob", &first.id, &second.id),
            Err(SnapshotError::Invalid(_))
        ));
    }

    #[test]
    fn invalid_ttl_and_labels_are_refused() {
        let (_dir, cfg, db, spec) = seeded();
        assert!(matches!(
            create(&cfg, &db, &spec, None, Some(0)),
            Err(SnapshotError::Invalid(_))
        ));
        assert!(matches!(
            create(&cfg, &db, &spec, None, Some(MAX_TTL_S + 1)),
            Err(SnapshotError::Invalid(_))
        ));
        assert!(matches!(
            create(&cfg, &db, &spec, Some("line\nbreak"), None),
            Err(SnapshotError::Invalid(_))
        ));
        assert!(matches!(
            create(
                &cfg,
                &db,
                &spec,
                Some(&"x".repeat(MAX_LABEL_CHARS + 1)),
                None
            ),
            Err(SnapshotError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn an_auto_snapshot_is_best_effort() {
        let (_dir, cfg, db, _spec) = seeded();
        let mock = Arc::new(MockDriver::new());
        let state = state(&cfg, db, mock);
        let id = auto_before(&state, "alice", "upgrade")
            .await
            .expect("snapshot");
        let stored = get(&state.db, &id).expect("row");
        assert_eq!(stored.label, "auto: upgrade");
        assert!(auto_before(&state, "ghost", "upgrade").await.is_none());
    }
}
