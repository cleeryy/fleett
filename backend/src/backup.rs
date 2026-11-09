//! Rotating, consistent backups of the whole fleet.
//!
//! `instances/` and `shares/` are the source of truth, but nothing protected
//! them: `restart: unless-stopped` survives a crash, not a lost disk. This
//! module snapshots the data root into a self-describing `.tgz`:
//!
//! ```text
//! manifest.json   format, version, timestamps, instances and image tags
//! fleet.db        consistent copy of the index (SQLite `VACUUM INTO`)
//! instances/      compose files and Hermes homes (no secrets.env, no *.key)
//! shares/         share contents
//! ```
//!
//! The secrets *key* lives outside the data root by design (`/etc/fleett`), so
//! a backup never contains it and never contains a token: restore the key
//! separately or the encrypted rows stay opaque.
//!
//! ```text
//! fleett backup [--dest DIR] [--keep 7]
//! fleett backups list [--dest DIR]
//! fleett backups restore <file> [--root DIR] [--force]
//! ```

use crate::archive;
use crate::config::{Config, VERSION};
use crate::db::Db;
use crate::models::now_iso;
use crate::store;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `format` field of a backup manifest.
pub const BACKUP_FORMAT: &str = "fleett.backup";
/// Manifest version understood by this build.
pub const FORMAT_VERSION: u32 = 1;
/// File name prefix, followed by `YYYYMMDD-HHMMSS.tgz`.
pub const PREFIX: &str = "fleett-backup-";
/// Archive suffix.
pub const SUFFIX: &str = ".tgz";
/// Backups kept by default.
pub const DEFAULT_KEEP: usize = 7;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("filesystem error: {0}")]
    Io(#[from] io::Error),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("index error: {0}")]
    Index(#[from] crate::db::DbError),
    #[error("cannot encode the manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid backup archive {path}: {detail}")]
    Invalid { path: PathBuf, detail: String },
    #[error("refusing to restore into {0}: the directory is not empty (re-run with --force)")]
    RootNotEmpty(PathBuf),
    #[error(
        "unsupported backup {format:?} v{version}; this build restores {expected:?} v{supported}"
    )]
    Unsupported {
        format: String,
        version: u32,
        expected: &'static str,
        supported: u32,
    },
}

/// One instance, as recorded in the backup manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupInstance {
    pub id: String,
    pub name: String,
    pub template: String,
    pub model: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format: String,
    pub format_version: u32,
    pub fleett_version: String,
    pub created_at: String,
    pub schema_version: i64,
    pub instances: Vec<BackupInstance>,
    pub image_tags: Vec<String>,
    pub shares: Vec<String>,
}

/// Result of one `fleett backup` run.
#[derive(Debug, Clone)]
pub struct BackupReport {
    pub path: PathBuf,
    pub bytes: u64,
    /// Instances recorded in the manifest.
    pub instances: usize,
    /// Share directories recorded in the manifest.
    pub shares: usize,
    /// Files and symlinks written to the archive.
    pub files: usize,
    pub removed: Vec<PathBuf>,
}

/// One archive found in a backup directory.
#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    pub created_at: String,
}

/// Result of one `fleett backups restore` run.
#[derive(Debug, Clone)]
pub struct RestoreReport {
    pub root: PathBuf,
    pub files: usize,
    pub manifest: BackupManifest,
}

/// Default backup directory (`<root>/backups`).
pub fn default_dest(cfg: &Config) -> PathBuf {
    cfg.root.join("backups")
}

/// Create one backup, then rotate.
pub fn backup(cfg: &Config, dest: &Path, keep: usize) -> Result<BackupReport, BackupError> {
    fs::create_dir_all(dest)?;
    fs::set_permissions(dest, fs::Permissions::from_mode(0o700))?;

    let token = uuid::Uuid::new_v4().to_string();
    let name = unique_name(dest, &timestamp());
    let tmp_path = dest.join(format!(".{name}.tmp"));
    let snapshot_path = dest.join(format!(".fleet-{token}.db"));

    let result = build_archive(cfg, dest, &tmp_path, &snapshot_path, &name, keep);
    // The snapshot lives beside the archive only while the archive is written.
    let _ = fs::remove_file(&snapshot_path);
    match result {
        Ok(report) => Ok(report),
        Err(err) => {
            let _ = fs::remove_file(&tmp_path);
            Err(err)
        }
    }
}

fn build_archive(
    cfg: &Config,
    dest: &Path,
    tmp_path: &Path,
    snapshot_path: &Path,
    name: &str,
    keep: usize,
) -> Result<BackupReport, BackupError> {
    let db_path = cfg.db_path();
    let manifest = build_manifest(cfg, &db_path)?;

    let file = fs::File::create(tmp_path)?;
    fs::set_permissions(tmp_path, fs::Permissions::from_mode(0o600))?;
    let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    let mut files = 2; // manifest.json and fleet.db

    archive::append_bytes(
        &mut builder,
        Path::new("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
        0o600,
    )?;

    if db_path.is_file() {
        snapshot_db(&db_path, snapshot_path)?;
        let bytes = fs::read(snapshot_path)?;
        archive::append_bytes(&mut builder, Path::new("fleet.db"), &bytes, 0o600)?;
    } else {
        files = 1;
    }

    if cfg.instances_dir().is_dir() {
        archive::append_dir(&mut builder, Path::new("instances"), 0o700)?;
        files += archive::append_tree(&mut builder, &cfg.instances_dir(), Path::new("instances"))?;
    }
    if cfg.shares_dir().is_dir() {
        files += archive::append_tree(&mut builder, &cfg.shares_dir(), Path::new("shares"))?;
    }

    let file = builder.into_inner()?.finish()?;
    file.sync_all()?;
    drop(file);

    let final_path = dest.join(name);
    fs::rename(tmp_path, &final_path)?;
    fs::set_permissions(&final_path, fs::Permissions::from_mode(0o600))?;

    let removed = rotate(dest, keep, name)?;
    Ok(BackupReport {
        path: final_path.clone(),
        bytes: fs::metadata(&final_path)?.len(),
        instances: manifest.instances.len(),
        shares: manifest.shares.len(),
        files,
        removed,
    })
}

/// Timestamp used in archive names (`YYYYMMDD-HHMMSS`, UTC).
pub fn timestamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Suffix `_2`, `_3`, ... when two backups land in the same second.
///
/// `_` sorts after `.`, so the extra archives of a second stay *after* the
/// first one in the name ordering the rotation relies on.
fn unique_name(dest: &Path, stamp: &str) -> String {
    let base = format!("{PREFIX}{stamp}");
    let mut candidate = format!("{base}{SUFFIX}");
    let mut counter = 2;
    while dest.join(&candidate).exists() {
        candidate = format!("{base}_{counter}{SUFFIX}");
        counter += 1;
    }
    candidate
}

fn build_manifest(cfg: &Config, db_path: &Path) -> Result<BackupManifest, BackupError> {
    let mut schema_version = 0i64;
    let mut rows: Vec<BackupInstance> = Vec::new();
    if db_path.is_file() {
        let db = Db::open(db_path)?;
        schema_version = db.schema_version().unwrap_or_default();
        for spec in db.list()? {
            rows.push(BackupInstance {
                id: spec.id,
                name: spec.name,
                template: spec.template,
                model: spec.model,
                status: spec.status.as_str().to_string(),
            });
        }
    }
    if rows.is_empty() {
        // A root without a database is still worth backing up: describe it
        // from the compose files themselves.
        for spec in store::scan_instances(cfg) {
            rows.push(BackupInstance {
                id: spec.id,
                name: spec.name,
                template: spec.template,
                model: spec.model,
                status: spec.status.as_str().to_string(),
            });
        }
    }

    Ok(BackupManifest {
        format: BACKUP_FORMAT.to_string(),
        format_version: FORMAT_VERSION,
        fleett_version: VERSION.to_string(),
        created_at: now_iso(),
        schema_version,
        instances: rows,
        image_tags: image_tags(cfg),
        shares: store::scan_shares(cfg),
    })
}

/// Image tags referenced by the compose files of the fleet.
fn image_tags(cfg: &Config) -> Vec<String> {
    let mut tags = BTreeSet::new();
    for spec in store::scan_instances(cfg) {
        let Ok(text) = fs::read_to_string(cfg.compose_path(&spec.id)) else {
            continue;
        };
        let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
            continue;
        };
        let Some(services) = doc.get("services").and_then(|value| value.as_mapping()) else {
            continue;
        };
        for service in services.values() {
            if let Some(image) = service.get("image").and_then(|value| value.as_str()) {
                tags.insert(image.to_string());
            }
        }
    }
    if tags.is_empty() {
        tags.insert(cfg.image.clone());
    }
    tags.into_iter().collect()
}

/// Consistent copy of the index even while the control plane is running.
fn snapshot_db(source: &Path, destination: &Path) -> Result<(), BackupError> {
    let conn = Connection::open(source)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    let target = destination
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non UTF-8 snapshot path"))?;
    conn.execute("VACUUM INTO ?1", rusqlite::params![target])?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Delete the oldest archives beyond `keep`, never the one just written.
pub fn rotate(dest: &Path, keep: usize, keep_name: &str) -> io::Result<Vec<PathBuf>> {
    let keep = keep.max(1);
    let archives = archive_paths(dest)?;
    let mut remaining = archives.len();
    let mut removed = Vec::new();
    for path in archives {
        if remaining <= keep {
            break;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some(keep_name) {
            continue;
        }
        fs::remove_file(&path)?;
        removed.push(path);
        remaining -= 1;
    }
    Ok(removed)
}

/// Archives present in `dest`, oldest first (names are timestamp-sortable).
pub fn archive_paths(dest: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let entries = match fs::read_dir(dest) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(paths),
        Err(err) => return Err(err),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(PREFIX) || !name.ends_with(SUFFIX) {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        paths.push(path);
    }
    paths.sort();
    Ok(paths)
}

/// `fleett backups list`, newest first.
pub fn list(dest: &Path) -> Result<Vec<BackupEntry>, BackupError> {
    let mut entries = archive_paths(dest)?
        .into_iter()
        .map(|path| {
            let metadata = fs::metadata(&path)?;
            let created_at = metadata
                .modified()
                .ok()
                .map(|time| {
                    chrono::DateTime::<chrono::Utc>::from(time)
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                })
                .unwrap_or_else(now_iso);
            Ok(BackupEntry {
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default(),
                bytes: metadata.len(),
                created_at,
                path,
            })
        })
        .collect::<Result<Vec<_>, BackupError>>()?;
    entries.reverse();
    Ok(entries)
}

/// Restore one archive into `root`.
///
/// The root must be empty unless `force` is set, and nothing is started: the
/// control plane has to be stopped while its database is replaced.
pub fn restore(
    archive_path: &Path,
    root: &Path,
    force: bool,
) -> Result<RestoreReport, BackupError> {
    let invalid = |detail: String| BackupError::Invalid {
        path: archive_path.to_path_buf(),
        detail,
    };

    // Pass 1: validate every entry and read the manifest before touching the
    // destination.
    let manifest = read_manifest(archive_path).map_err(|err| invalid(err.to_string()))?;

    if root.exists() {
        let mut entries = fs::read_dir(root)?;
        let non_empty = entries.next().is_some();
        if non_empty && !force {
            return Err(BackupError::RootNotEmpty(root.to_path_buf()));
        }
    }
    fs::create_dir_all(root)?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;

    // A stale WAL belonging to the replaced database would corrupt the
    // restored one, so drop the three files together.
    for name in ["fleet.db", "fleet.db-wal", "fleet.db-shm"] {
        let path = root.join(name);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }

    // Pass 2: extract.
    let file = fs::File::open(archive_path)?;
    let mut tar = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
    let mut files = 0usize;
    let mut skipped = 0usize;
    for entry in tar.entries()? {
        let entry = entry?;
        let path = entry.path()?.into_owned();
        archive::safe_path(&path).map_err(invalid)?;
        let Some(name) = path.file_name().map(|name| name.to_owned()) else {
            continue;
        };
        if archive::skip_extracted_entry(&name) {
            skipped += 1;
            continue;
        }
        if path.as_path() == Path::new("manifest.json") {
            continue;
        }
        archive::write_entry(entry, &root.join(&path)).map_err(|err| invalid(err.to_string()))?;
        files += 1;
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            "backup: ignored secret files found in the archive (backups never store them)"
        );
    }
    if !root.join("fleet.db").is_file() {
        tracing::warn!(
            root = %root.display(),
            "backup: the archive holds no fleet.db; agents are restored but the index is empty"
        );
    }
    store::adopt_restored_root(root);

    Ok(RestoreReport {
        root: root.to_path_buf(),
        files,
        manifest,
    })
}

fn read_manifest(archive_path: &Path) -> Result<BackupManifest, BackupError> {
    let file = fs::File::open(archive_path)?;
    let mut tar = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
    let mut seen_any = false;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        archive::safe_path(&path).map_err(|detail| BackupError::Invalid {
            path: archive_path.to_path_buf(),
            detail,
        })?;
        seen_any = true;
        if path.as_path() != Path::new("manifest.json") {
            continue;
        }
        let bytes = archive::read_metadata(&mut entry)?;
        let manifest: BackupManifest =
            serde_json::from_slice(&bytes).map_err(|err| BackupError::Invalid {
                path: archive_path.to_path_buf(),
                detail: format!("invalid manifest: {err}"),
            })?;
        if manifest.format != BACKUP_FORMAT || manifest.format_version != FORMAT_VERSION {
            return Err(BackupError::Unsupported {
                format: manifest.format,
                version: manifest.format_version,
                expected: BACKUP_FORMAT,
                supported: FORMAT_VERSION,
            });
        }
        return Ok(manifest);
    }
    let detail = if seen_any {
        "the archive has no manifest.json".to_string()
    } else {
        "the archive is empty".to_string()
    };
    Err(BackupError::Invalid {
        path: archive_path.to_path_buf(),
        detail,
    })
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// `fleett backup [--dest DIR] [--keep N]`.
pub fn run_backup(cfg: &Config, dest: Option<PathBuf>, keep: usize) -> anyhow::Result<()> {
    let dest = dest.unwrap_or_else(|| default_dest(cfg));
    let report = backup(cfg, &dest, keep)?;
    println!(
        "backup: {} ({}, {} instance(s), {} share(s), {} file(s))",
        report.path.display(),
        human_bytes(report.bytes),
        report.instances,
        report.shares,
        report.files
    );
    if report.removed.is_empty() {
        println!("rotation: kept at most {keep} archive(s), nothing to delete");
    } else {
        println!("rotation: deleted {} old archive(s)", report.removed.len());
        for path in &report.removed {
            println!("  removed {}", path.display());
        }
    }
    println!("the archive holds no secret: restore /etc/fleett/secrets.key separately.");
    Ok(())
}

/// `fleett backups list [--dest DIR]`.
pub fn run_list(cfg: &Config, dest: Option<PathBuf>) -> anyhow::Result<()> {
    let dest = dest.unwrap_or_else(|| default_dest(cfg));
    let entries = list(&dest)?;
    if entries.is_empty() {
        println!("no backup in {}", dest.display());
        return Ok(());
    }
    println!("{} backup(s) in {}:", entries.len(), dest.display());
    for entry in entries {
        println!(
            "  {}  {:>10}  {}",
            entry.name,
            human_bytes(entry.bytes),
            entry.created_at
        );
    }
    Ok(())
}

/// `fleett backups restore <file> [--root DIR] [--force]`.
pub fn run_restore(
    cfg: &Config,
    archive: PathBuf,
    root: Option<PathBuf>,
    force: bool,
) -> anyhow::Result<()> {
    let root = root.unwrap_or_else(|| cfg.root.clone());
    println!(
        "restoring {} into {} (the control plane must be stopped)",
        archive.display(),
        root.display()
    );
    let report = restore(&archive, &root, force)?;
    println!(
        "restored {} file(s); fleett {} backup from {}",
        report.files, report.manifest.fleett_version, report.manifest.created_at
    );
    println!(
        "  {} instance(s) indexed in the archive",
        report.manifest.instances.len()
    );
    if !report.manifest.image_tags.is_empty() {
        println!("  image(s): {}", report.manifest.image_tags.join(", "));
    }
    println!("nothing was started; run `systemctl start fleett` when ready.");
    Ok(())
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{InstanceSpec, Status};

    fn temp_config() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        store::ensure_layout(&cfg).expect("layout");
        (dir, cfg)
    }

    fn seed(cfg: &Config) -> InstanceSpec {
        let now = now_iso();
        let spec = InstanceSpec {
            id: "alice".to_string(),
            name: "Alice".to_string(),
            template: "guest".to_string(),
            model: "deepseek-v4.1-flash".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: "HERMES_GATE_PROFILE_TOKEN".to_string(),
            provider: None,
            channel: crate::models::ChannelKind::None,
            shares: Vec::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            mem_limit_mb: 1024,
            cpu_limit: 1.0,
            image: "hermes-agent:0.22.2-local".to_string(),
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
            status: Status::Stopped,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        };
        Db::open(&cfg.db_path())
            .expect("db")
            .insert_instance(&spec)
            .expect("insert");
        store::write_instance(
            cfg,
            &spec,
            &crate::render::RenderedInstance {
                compose_yaml: "name: fleett-alice\nservices:\n  agent:\n    image: agent:9.9\n"
                    .to_string(),
                config_yaml: "model: {}\n".to_string(),
                soul_md: None,
            },
            false,
        )
        .expect("write instance");
        std::fs::write(cfg.data_dir("alice").join("MEMORY.md"), "remember\n").expect("memory");
        std::fs::write(cfg.secrets_path("alice"), "TOKEN=leak\n").expect("legacy secret");
        std::fs::create_dir_all(cfg.share_dir("findings")).expect("share");
        std::fs::write(cfg.share_dir("findings").join("note.md"), "shared\n").expect("share file");
        spec
    }

    #[test]
    fn a_backup_holds_the_database_and_data_but_no_secret() {
        let (_dir, cfg) = temp_config();
        seed(&cfg);
        let dest = cfg.root.join("backups");

        let report = backup(&cfg, &dest, DEFAULT_KEEP).expect("backup");
        assert!(report.path.is_file());
        assert!(report
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(PREFIX));
        assert!(report.path.extension().unwrap() == "tgz");
        assert_eq!(report.instances, 1);
        assert_eq!(report.shares, 1);
        assert_eq!(report.files, 7);

        let mut archive =
            tar::Archive::new(GzDecoder::new(fs::File::open(&report.path).expect("open")));
        let mut names = Vec::new();
        for entry in archive.entries().expect("entries") {
            let entry = entry.expect("entry");
            names.push(entry.path().expect("path").display().to_string());
        }
        assert!(names.contains(&"fleet.db".to_string()), "{names:?}");
        assert!(
            names.contains(&"instances/alice/data/MEMORY.md".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"shares/findings/note.md".to_string()));
        assert!(!names.iter().any(|name| name.ends_with("secrets.env")));
        let bytes = fs::read(&report.path).expect("read archive");
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("leak"), "no plaintext token in the archive");

        // The snapshot copy is cleaned up.
        assert!(!dest.join(format!(".fleet-{}.db", "")).exists());
        assert_eq!(fs::read_dir(&dest).expect("dir").count(), 1);
    }

    #[test]
    fn rotation_keeps_exactly_the_newest_archives() {
        let dir = tempfile::tempdir().expect("temp dir");
        for stamp in 1..=5 {
            fs::write(
                dir.path()
                    .join(format!("{PREFIX}2026010{stamp}-000000{SUFFIX}")),
                b"x",
            )
            .expect("archive");
        }
        fs::write(dir.path().join("not-a-backup.tgz"), b"x").expect("decoy");

        let removed = rotate(dir.path(), 2, "fleett-backup-20260105-000000.tgz").expect("rotate");
        assert_eq!(removed.len(), 3, "{removed:?}");
        let kept: Vec<String> = archive_paths(dir.path())
            .expect("list")
            .into_iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            kept,
            vec![
                "fleett-backup-20260104-000000.tgz".to_string(),
                "fleett-backup-20260105-000000.tgz".to_string(),
            ]
        );
        assert!(dir.path().join("not-a-backup.tgz").is_file());
    }

    #[test]
    fn the_newest_archive_is_never_deleted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let newest = format!("{PREFIX}20260101-000000{SUFFIX}");
        fs::write(dir.path().join(&newest), b"x").expect("archive");
        let removed = rotate(dir.path(), 0, &newest).expect("rotate");
        assert!(removed.is_empty());
        assert!(dir.path().join(&newest).is_file());
    }

    #[test]
    fn a_restore_round_trips_into_an_empty_root() {
        let (_dir, cfg) = temp_config();
        seed(&cfg);
        let dest = cfg.root.join("backups");
        let report = backup(&cfg, &dest, DEFAULT_KEEP).expect("backup");

        let target = temp_config().0;
        let root = target.path().join("restored");
        let restored = restore(&report.path, &root, false).expect("restore");
        assert!(restored.files >= 7, "{restored:?}");
        assert_eq!(restored.manifest.instances.len(), 1);
        assert!(root.join("fleet.db").is_file());
        assert_eq!(
            fs::read_to_string(root.join("instances/alice/data/MEMORY.md")).expect("memory"),
            "remember\n"
        );
        assert!(root.join("instances/alice/compose.yaml").is_file());
        assert!(root.join("shares/findings/note.md").is_file());
        assert!(!root.join("instances/alice/secrets.env").exists());

        // The restored index lists the instance.
        let db = Db::open(&root.join("fleet.db")).expect("restored db");
        assert_eq!(db.list().expect("list").len(), 1);
    }

    #[test]
    fn a_non_empty_root_is_refused_without_force() {
        let (_dir, cfg) = temp_config();
        seed(&cfg);
        let report = backup(&cfg, &cfg.root.join("backups"), DEFAULT_KEEP).expect("backup");

        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("restored");
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("keep.txt"), "mine\n").expect("file");

        let err = restore(&report.path, &root, false).expect_err("refused");
        assert!(matches!(err, BackupError::RootNotEmpty(_)), "{err}");
        assert!(root.join("keep.txt").is_file());
        assert!(!root.join("fleet.db").exists());

        let restored = restore(&report.path, &root, true).expect("forced restore");
        assert!(restored.files > 0);
        assert!(
            root.join("keep.txt").is_file(),
            "force merges, it does not wipe"
        );
        assert!(root.join("fleet.db").is_file());
    }

    #[test]
    fn an_archive_without_a_manifest_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("plain.tgz");
        let file = fs::File::create(&path).expect("create");
        let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::default()));
        archive::append_bytes(&mut builder, Path::new("fleet.db"), b"nope", 0o600).expect("entry");
        builder.into_inner().expect("tar").finish().expect("gzip");

        let err = restore(&path, &dir.path().join("root"), true).expect_err("no manifest");
        assert!(matches!(err, BackupError::Invalid { .. }), "{err}");
    }

    #[test]
    fn list_is_newest_first() {
        let dir = tempfile::tempdir().expect("temp dir");
        for stamp in 1..=3 {
            fs::write(
                dir.path()
                    .join(format!("{PREFIX}2026010{stamp}-000000{SUFFIX}")),
                b"x",
            )
            .expect("archive");
        }
        let entries = list(dir.path()).expect("list");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "fleett-backup-20260103-000000.tgz");
        assert_eq!(entries[2].name, "fleett-backup-20260101-000000.tgz");
        assert_eq!(entries[0].bytes, 1);
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_bytes(12), "12 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
    }
}
