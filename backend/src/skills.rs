//! Shared skills registry (issue #54).
//!
//! A skill is a small tree of text files an agent reads at
//! `/opt/data/skills/<name>` (`SKILL.md` plus anything it references). Instead
//! of copying the same improvement into N instances, the fleet keeps one
//! registry and mounts it read-only.
//!
//! # Storage
//!
//! `$FLEETT_ROOT/skills/<name>/<version>/` holds the files of one version
//! (`SKILL.md` is mandatory), and `$FLEETT_ROOT/skills/<name>/latest` names the
//! version an instance follows when it pins none. Every version carries its
//! own `skill.yaml` metadata (`name`, `description`), so a version is
//! self-describing when the registry is copied elsewhere.
//!
//! The whole `skills/` directory is one git repository (`.git` at its root),
//! like [`crate::personas`]: every publish and every promote is a commit
//! authored by the operator, so the history answers "who changed this skill,
//! and when". Only whitelisted paths ever reach the index — a valid skill
//! name, a valid version directory, a `latest` file and regular files with
//! validated relative paths. A stray secret dropped into a skill directory
//! can never be committed.
//!
//! # Mounts
//!
//! The compose renderer mounts `../../skills/<name>/<version>` at
//! `/opt/data/skills/<name>` with `:ro`: the agent reads the skill, it cannot
//! modify the registry. An instance that pins no version follows `latest`,
//! which is why a promote has to restart it (the mount path contains the
//! concrete version).
//!
//! # Limits
//!
//! One version is at most [`MAX_TOTAL_BYTES`] of UTF-8 across at most
//! [`MAX_FILES`] files, each bounded by [`MAX_FILE_BYTES`]. Paths are relative
//! and may not climb (`..`), be absolute, or contain a backslash. A zip import
//! is read in memory and bounded by [`MAX_ZIP_BYTES`].

use crate::config::Config;
use crate::db::Db;
use crate::history::{Author, CommitInfo, DiffResponse, HistoryError};
use crate::models::{validate_skill_name, validate_skill_version, SkillRef};
use git2::{Commit, DiffFormat, DiffOptions, Repository, Signature, Sort, Tree};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{self, Cursor, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Directory name of the repository inside the skills root.
pub const GIT_DIR: &str = ".git";
/// Metadata file of one version.
pub const META_FILE: &str = "skill.yaml";
/// Mandatory entry file of one version.
pub const SKILL_FILE: &str = "SKILL.md";
/// Name of the file holding the version `latest` points at.
pub const LATEST_FILE: &str = "latest";
/// Largest accepted version, all files included.
pub const MAX_TOTAL_BYTES: usize = 1024 * 1024;
/// Largest accepted single file.
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// Most files one version may contain.
pub const MAX_FILES: usize = 256;
/// Longest accepted relative path.
pub const MAX_PATH_CHARS: usize = 200;
/// Longest accepted skill description.
pub const MAX_DESCRIPTION_CHARS: usize = 4096;
/// Largest accepted zip upload (compressed bytes).
pub const MAX_ZIP_BYTES: usize = 8 * 1024 * 1024;
/// Most skills one instance may mount.
pub const MAX_SKILLS_PER_INSTANCE: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("unknown skill \"{0}\"")]
    NotFound(String),
    #[error("skill \"{0}\" already exists")]
    Exists(String),
    #[error("version \"{version}\" of skill \"{name}\" already exists")]
    VersionExists { name: String, version: String },
    #[error("unknown version \"{version}\" of skill \"{name}\"")]
    UnknownVersion { name: String, version: String },
    #[error("unknown commit \"{0}\"")]
    UnknownCommit(String),
    #[error("invalid skill: {0}")]
    Invalid(String),
    #[error("skill \"{id}\" is still in use")]
    InUse { id: String, instances: Vec<String> },
    #[error("skill filesystem error: {0}")]
    Io(String),
    #[error("skill git error: {0}")]
    Git(String),
    #[error("skill database error: {0}")]
    Db(String),
}

impl SkillError {
    fn io(path: &Path, err: io::Error) -> Self {
        Self::Io(format!("{}: {err}", path.display()))
    }
}

impl From<git2::Error> for SkillError {
    fn from(err: git2::Error) -> Self {
        Self::Git(err.message().to_string())
    }
}

impl From<crate::db::DbError> for SkillError {
    fn from(err: crate::db::DbError) -> Self {
        Self::Db(err.to_string())
    }
}

impl From<HistoryError> for SkillError {
    fn from(err: HistoryError) -> Self {
        match err {
            HistoryError::Io(err) => Self::Io(err.to_string()),
            HistoryError::Git(err) => Self::Git(err.message().to_string()),
            other => Self::Git(other.to_string()),
        }
    }
}

/// One text file of a skill version, path relative to the version directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillFile {
    pub path: String,
    pub content: String,
}

/// `skill.yaml` as stored. Every field is optional on disk so a hand-written
/// file only has to declare what it needs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SkillMetaFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// `GET /api/skills` entry.
#[derive(Debug, Clone, Serialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    /// Version instances follow when they pin none (`null` when a hand-copied
    /// directory has no `latest` file).
    pub latest: Option<String>,
    /// Every published version, oldest first.
    pub versions: Vec<String>,
    /// Instances mounting the skill, pinned or not.
    pub instances_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

/// One version in `GET /api/skills/{name}`.
#[derive(Debug, Clone, Serialize)]
pub struct SkillVersionInfo {
    pub version: String,
    pub description: String,
    pub files_count: usize,
    pub size_bytes: usize,
    pub published_at: String,
}

/// `GET /api/skills/{name}`.
#[derive(Debug, Clone, Serialize)]
pub struct SkillDetail {
    pub name: String,
    pub description: String,
    pub latest: Option<String>,
    pub versions: Vec<SkillVersionInfo>,
    /// Files of the `latest` version (`skill.yaml` excluded).
    pub files: Vec<SkillFile>,
    /// Instances mounting the skill, sorted.
    pub instances: Vec<String>,
    pub instances_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of `POST /api/skills`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PublishSkillRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub files: Option<Vec<SkillFile>>,
}

/// Body of `POST /api/skills/{name}/promote`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PromoteRequest {
    #[serde(default)]
    pub version: Option<String>,
}

/// Outcome of one instance in a promote report.
#[derive(Debug, Clone, Serialize)]
pub struct PromoteResult {
    pub id: String,
    /// `restarted` or `failed`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response of `POST /api/skills/{name}/promote`.
#[derive(Debug, Clone, Serialize)]
pub struct PromoteResponse {
    /// Version `latest` was moved to.
    pub latest: String,
    pub results: Vec<PromoteResult>,
}

// ---------------------------------------------------------------------------
// Paths and validation
// ---------------------------------------------------------------------------

/// Skill directory, after validating the name.
fn skill_dir(cfg: &Config, name: &str) -> Result<PathBuf, SkillError> {
    validate_skill_name(name).map_err(SkillError::Invalid)?;
    Ok(cfg.skill_dir(name))
}

/// Version directory, after validating both parts.
fn version_dir(cfg: &Config, name: &str, version: &str) -> Result<PathBuf, SkillError> {
    validate_skill_name(name).map_err(SkillError::Invalid)?;
    validate_skill_version(version).map_err(SkillError::Invalid)?;
    Ok(cfg.skill_version_dir(name, version))
}

/// Create `skills/` (0755: the tree is mounted read-only, it holds no secret).
pub fn ensure_registry(cfg: &Config) -> io::Result<()> {
    let dir = cfg.skills_dir();
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))
}

/// Normalise and validate one relative file path.
///
/// Returns the path unchanged when valid; refuses absolute paths, `.`/`..`
/// components, empty components, backslashes, NUL bytes and over-long paths.
fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("a file path must not be empty".to_string());
    }
    if path.chars().count() > MAX_PATH_CHARS {
        return Err(format!(
            "file path \"{path}\" must be at most {MAX_PATH_CHARS} characters"
        ));
    }
    if path.contains('\0') {
        return Err(format!("file path \"{path}\" must not contain a NUL byte"));
    }
    if path.contains('\\') {
        return Err(format!(
            "file path \"{path}\" must use '/' as separator (no backslash)"
        ));
    }
    if path.starts_with('/') {
        return Err(format!("file path \"{path}\" must be relative"));
    }
    for component in path.split('/') {
        if component.is_empty() {
            return Err(format!(
                "file path \"{path}\" must not contain an empty component"
            ));
        }
        if component == "." || component == ".." {
            return Err(format!(
                "file path \"{path}\" must not contain a \".\" or \"..\" component"
            ));
        }
        if component.chars().count() > 64 {
            return Err(format!(
                "each component of \"{path}\" must be at most 64 characters"
            ));
        }
    }
    Ok(())
}

/// `true` when the path names generated registry metadata rather than content.
fn reserved_path(path: &str) -> bool {
    matches!(path, META_FILE | LATEST_FILE)
}

fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0)
}

fn read_text(path: &Path) -> Result<String, SkillError> {
    let bytes = fs::read(path).map_err(|err| SkillError::io(path, err))?;
    String::from_utf8(bytes)
        .map_err(|_| SkillError::Invalid(format!("{} is not valid UTF-8", path.display())))
}

/// Validate the content of one version before anything is written.
fn validate_files(files: &[SkillFile]) -> Result<(), SkillError> {
    if files.is_empty() {
        return Err(SkillError::Invalid("files must not be empty".to_string()));
    }
    if files.len() > MAX_FILES {
        return Err(SkillError::Invalid(format!(
            "a version may contain at most {MAX_FILES} files"
        )));
    }
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    for file in files {
        validate_relative_path(&file.path).map_err(SkillError::Invalid)?;
        if reserved_path(&file.path) {
            return Err(SkillError::Invalid(format!(
                "\"{}\" is generated by the registry and must not be uploaded",
                file.path
            )));
        }
        if !seen.insert(file.path.clone()) {
            return Err(SkillError::Invalid(format!(
                "file \"{}\" is listed twice",
                file.path
            )));
        }
        if file.content.len() > MAX_FILE_BYTES {
            return Err(SkillError::Invalid(format!(
                "file \"{}\" must be at most {MAX_FILE_BYTES} bytes",
                file.path
            )));
        }
        if !is_text(file.content.as_bytes()) {
            return Err(SkillError::Invalid(format!(
                "file \"{}\" must be text (no NUL byte)",
                file.path
            )));
        }
        total = total.saturating_add(file.content.len());
    }
    if total > MAX_TOTAL_BYTES {
        return Err(SkillError::Invalid(format!(
            "a version must be at most {MAX_TOTAL_BYTES} bytes in total"
        )));
    }
    if !seen.contains(SKILL_FILE) {
        return Err(SkillError::Invalid(format!(
            "{SKILL_FILE} is required at the root of the version"
        )));
    }
    let skill = files
        .iter()
        .find(|file| file.path == SKILL_FILE)
        .expect("SKILL.md was just checked");
    if skill.content.trim().is_empty() {
        return Err(SkillError::Invalid(format!(
            "{SKILL_FILE} must not be empty"
        )));
    }
    Ok(())
}

fn validate_description(description: &str) -> Result<(), SkillError> {
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(SkillError::Invalid(format!(
            "description must be at most {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    if description.contains('\0') {
        return Err(SkillError::Invalid(
            "description must not contain a NUL byte".to_string(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reading the registry
// ---------------------------------------------------------------------------

fn read_optional(path: &Path) -> Result<Option<String>, SkillError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(SkillError::io(path, err)),
    }
}

/// `(name, description)` of one version, tolerating a hand-written file.
fn read_meta(cfg: &Config, name: &str, version: &str) -> Result<(String, String), SkillError> {
    let path = version_dir(cfg, name, version)?.join(META_FILE);
    let Some(text) = read_optional(&path)? else {
        return Ok((name.to_string(), String::new()));
    };
    let file: SkillMetaFile = serde_yaml::from_str(&text).map_err(|err| {
        SkillError::Invalid(format!("{} is not valid YAML: {err}", path.display()))
    })?;
    let stored_name = file
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(name)
        .to_string();
    let description = file
        .description
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    Ok((stored_name, description))
}

/// Versions present on disk for one skill, sorted by number.
fn versions_on_disk(cfg: &Config, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(cfg.skill_dir(name)) else {
        return out;
    };
    for entry in entries.flatten() {
        let version = entry.file_name().to_string_lossy().to_string();
        if validate_skill_version(&version).is_err() {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() && !meta.file_type().is_symlink() {
            out.push(version);
        }
    }
    out.sort_by_key(|version| version_number(version).unwrap_or(u64::MAX));
    out
}

fn version_number(version: &str) -> Option<u64> {
    version.strip_prefix('v')?.parse::<u64>().ok()
}

/// Version `latest` points at, when the pointer is present and valid.
pub fn latest(cfg: &Config, name: &str) -> Option<String> {
    if validate_skill_name(name).is_err() {
        return None;
    }
    let text = fs::read_to_string(cfg.skill_dir(name).join(LATEST_FILE)).ok()?;
    let version = text.trim().to_string();
    if validate_skill_version(&version).is_err() || !cfg.skill_version_dir(name, &version).is_dir()
    {
        return None;
    }
    Some(version)
}

/// Version an instance actually mounts for one skill reference.
///
/// `None` means the reference cannot be satisfied (unknown skill, removed
/// pinned version): the compose renderer then leaves the mount out instead of
/// letting Docker create an empty root-owned directory.
pub fn effective_version(cfg: &Config, skill: &SkillRef) -> Option<String> {
    if validate_skill_name(&skill.name).is_err() {
        return None;
    }
    match skill
        .version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(version)
            if validate_skill_version(version).is_ok()
                && cfg.skill_version_dir(&skill.name, version).is_dir() =>
        {
            Some(version.to_string())
        }
        Some(_) => None,
        None => latest(cfg, &skill.name),
    }
}

/// Every skill name present on disk.
fn names_on_disk(cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(cfg.skills_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == GIT_DIR || validate_skill_name(&name).is_err() {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() && !meta.file_type().is_symlink() {
            out.push(name);
        }
    }
    out.sort();
    out
}

/// Files of one version (`skill.yaml` excluded), `path` relative.
fn read_files(cfg: &Config, name: &str, version: &str) -> Result<Vec<SkillFile>, SkillError> {
    let root = version_dir(cfg, name, version)?;
    let mut paths = Vec::new();
    collect_files(&root, &root, 0, &mut paths)?;
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for relative in paths {
        let content = read_text(&root.join(&relative))?;
        out.push(SkillFile {
            path: relative,
            content,
        });
    }
    Ok(out)
}

fn collect_files(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<(), SkillError> {
    if depth > 8 {
        return Err(SkillError::Invalid(
            "a skill version may not nest directories more than 8 levels deep".to_string(),
        ));
    }
    let entries = fs::read_dir(dir).map_err(|err| SkillError::io(dir, err))?;
    for entry in entries {
        let entry = entry.map_err(|err| SkillError::io(dir, err))?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|err| SkillError::io(&path, err))?;
        if meta.file_type().is_symlink() {
            return Err(SkillError::Invalid(format!(
                "{} is a symlink; the registry only holds regular files",
                path.display()
            )));
        }
        if meta.is_dir() {
            collect_files(root, &path, depth + 1, out)?;
            continue;
        }
        if !meta.is_file() {
            return Err(SkillError::Invalid(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| SkillError::Invalid(format!("{} is outside the version", path.display())))?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == META_FILE {
            continue;
        }
        validate_relative_path(&relative).map_err(SkillError::Invalid)?;
        out.push(relative);
    }
    Ok(())
}

/// `GET /api/skills`.
pub fn list(cfg: &Config, db: &Db) -> Result<Vec<SkillSummary>, SkillError> {
    let counts = db.skill_instance_counts()?;
    let mut out = Vec::new();
    for name in names_on_disk(cfg) {
        match summary(cfg, &name, &counts) {
            Ok(entry) => out.push(entry),
            Err(err) => tracing::warn!(skill = %name, %err, "skipping an unreadable skill"),
        }
    }
    Ok(out)
}

fn summary(
    cfg: &Config,
    name: &str,
    counts: &HashMap<String, usize>,
) -> Result<SkillSummary, SkillError> {
    let versions = versions_on_disk(cfg, name);
    let latest = latest(cfg, name);
    // The description is the one of the version instances follow, then of the
    // newest version: a skill keeps a description even without a `latest`.
    let description = latest
        .as_deref()
        .or_else(|| versions.last().map(String::as_str))
        .map(|version| read_meta(cfg, name, version).map(|(_, description)| description))
        .transpose()?
        .unwrap_or_default();
    let (created_at, updated_at) = timestamps(cfg, name);
    Ok(SkillSummary {
        name: name.to_string(),
        description,
        latest,
        versions,
        instances_count: counts.get(name).copied().unwrap_or(0),
        created_at,
        updated_at,
    })
}

/// `GET /api/skills/{name}`.
pub fn detail(cfg: &Config, db: &Db, name: &str) -> Result<SkillDetail, SkillError> {
    if validate_skill_name(name).is_err() || !cfg.skill_dir(name).is_dir() {
        return Err(SkillError::NotFound(name.to_string()));
    }
    let versions = versions_on_disk(cfg, name);
    let latest = latest(cfg, name);
    let mut infos = Vec::with_capacity(versions.len());
    for version in &versions {
        let (_, description) = read_meta(cfg, name, version)?;
        let files = read_files(cfg, name, version)?;
        infos.push(SkillVersionInfo {
            version: version.clone(),
            description,
            files_count: files.len(),
            size_bytes: files.iter().map(|file| file.content.len()).sum(),
            published_at: version_time(cfg, name, version),
        });
    }
    let description = latest
        .as_deref()
        .or_else(|| versions.last().map(String::as_str))
        .map(|version| read_meta(cfg, name, version).map(|(_, description)| description))
        .transpose()?
        .unwrap_or_default();
    let files = match latest.as_deref() {
        Some(version) => read_files(cfg, name, version)?,
        None => Vec::new(),
    };
    let instances = db.instances_using_skill(name)?;
    let (created_at, updated_at) = timestamps(cfg, name);
    Ok(SkillDetail {
        name: name.to_string(),
        description,
        latest,
        versions: infos,
        files,
        instances_count: instances.len(),
        instances,
        created_at,
        updated_at,
    })
}

// ---------------------------------------------------------------------------
// Writing the registry
// ---------------------------------------------------------------------------

/// Publish one version and commit it.
pub fn publish(
    cfg: &Config,
    db: &Db,
    request: &PublishSkillRequest,
    author: &Author,
) -> Result<SkillDetail, SkillError> {
    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SkillError::Invalid("name is required".to_string()))?;
    let version = request
        .version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SkillError::Invalid("version is required".to_string()))?;
    publish_parts(
        cfg,
        db,
        name,
        version,
        request.description.as_deref().unwrap_or_default(),
        request.files.as_deref().unwrap_or_default(),
        author,
    )
}

/// Publish after the name, version, description and files are known.
fn publish_parts(
    cfg: &Config,
    db: &Db,
    name: &str,
    version: &str,
    description: &str,
    files: &[SkillFile],
    author: &Author,
) -> Result<SkillDetail, SkillError> {
    validate_skill_name(name).map_err(SkillError::Invalid)?;
    validate_skill_version(version).map_err(SkillError::Invalid)?;
    let description = description.trim();
    validate_description(description)?;
    validate_files(files)?;
    let dir = version_dir(cfg, name, version)?;
    if dir.exists() {
        return Err(SkillError::VersionExists {
            name: name.to_string(),
            version: version.to_string(),
        });
    }
    ensure_registry(cfg).map_err(|err| SkillError::io(&cfg.skills_dir(), err))?;
    write_version(cfg, name, version, description, files)?;
    // The first version of a skill becomes its `latest`: an instance that
    // follows latest has something to mount immediately.
    if latest(cfg, name).is_none() {
        write_latest(cfg, name, version)?;
    }
    commit(cfg, name, version, author)?;
    detail(cfg, db, name)
}

/// Write one version into a temporary sibling directory, then rename it, so a
/// failed write never leaves a half-published version behind.
fn write_version(
    cfg: &Config,
    name: &str,
    version: &str,
    description: &str,
    files: &[SkillFile],
) -> Result<(), SkillError> {
    let root = skill_dir(cfg, name)?;
    fs::create_dir_all(&root).map_err(|err| SkillError::io(&root, err))?;
    set_mode(&root, 0o755)?;
    let temporary = root.join(format!(".{version}.tmp"));
    if temporary.exists() {
        fs::remove_dir_all(&temporary).map_err(|err| SkillError::io(&temporary, err))?;
    }
    fs::create_dir_all(&temporary).map_err(|err| SkillError::io(&temporary, err))?;
    let result = (|| -> Result<(), SkillError> {
        set_mode(&temporary, 0o755)?;
        for file in files {
            let target = temporary.join(&file.path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| SkillError::io(parent, err))?;
                set_mode(parent, 0o755)?;
            }
            fs::write(&target, &file.content).map_err(|err| SkillError::io(&target, err))?;
            set_mode(&target, 0o644)?;
        }
        let meta = SkillMetaFile {
            name: Some(name.to_string()),
            description: Some(description.to_string()),
        };
        let text = serde_yaml::to_string(&meta)
            .map_err(|err| SkillError::Invalid(format!("cannot serialise {META_FILE}: {err}")))?;
        let meta_path = temporary.join(META_FILE);
        fs::write(&meta_path, text).map_err(|err| SkillError::io(&meta_path, err))?;
        set_mode(&meta_path, 0o644)?;
        Ok(())
    })();
    if let Err(err) = result {
        let _ = fs::remove_dir_all(&temporary);
        return Err(err);
    }
    let target = root.join(version);
    fs::rename(&temporary, &target).map_err(|err| SkillError::io(&target, err))?;
    set_mode(&target, 0o755)?;
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), SkillError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|err| SkillError::io(path, err))
}

fn write_latest(cfg: &Config, name: &str, version: &str) -> Result<(), SkillError> {
    let root = skill_dir(cfg, name)?;
    fs::create_dir_all(&root).map_err(|err| SkillError::io(&root, err))?;
    let path = root.join(LATEST_FILE);
    fs::write(&path, format!("{version}\n")).map_err(|err| SkillError::io(&path, err))?;
    set_mode(&path, 0o644)
}

/// `POST /api/skills/{name}/promote` — move `latest`, commit, and report the
/// version instances should now mount.
pub fn promote(
    cfg: &Config,
    name: &str,
    version: &str,
    author: &Author,
) -> Result<String, SkillError> {
    validate_skill_name(name).map_err(SkillError::Invalid)?;
    validate_skill_version(version).map_err(SkillError::Invalid)?;
    if !cfg.skill_dir(name).is_dir() {
        return Err(SkillError::NotFound(name.to_string()));
    }
    if !cfg.skill_version_dir(name, version).is_dir() {
        return Err(SkillError::UnknownVersion {
            name: name.to_string(),
            version: version.to_string(),
        });
    }
    write_latest(cfg, name, version)?;
    commit(cfg, name, version, author)?;
    Ok(version.to_string())
}

/// Instances that follow the `latest` pointer of `name`, so a promote has to
/// restart them.
pub fn followers(db: &Db, name: &str) -> Result<Vec<String>, SkillError> {
    Ok(db.instances_following_skill(name)?)
}

/// `DELETE /api/skills/{name}` — refused while an instance mounts it.
pub fn delete(cfg: &Config, db: &Db, name: &str, author: &Author) -> Result<(), SkillError> {
    if validate_skill_name(name).is_err() || !cfg.skill_dir(name).is_dir() {
        return Err(SkillError::NotFound(name.to_string()));
    }
    let instances = db.instances_using_skill(name)?;
    if !instances.is_empty() {
        return Err(SkillError::InUse {
            id: name.to_string(),
            instances,
        });
    }
    let dir = cfg.skill_dir(name);
    fs::remove_dir_all(&dir).map_err(|err| SkillError::io(&dir, err))?;
    if let Err(err) = commit(cfg, name, "deleted", author) {
        // The directory is already gone; a git failure must not turn the
        // delete into a 500 that hides that.
        tracing::warn!(skill = %name, %err, "could not record the skill deletion");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Zip import
// ---------------------------------------------------------------------------

/// One entry read out of a zip, before the common root folder is stripped.
struct ZipEntry {
    path: String,
    content: String,
}

/// Publish a skill from a zip archive held in memory.
///
/// The archive must contain a `skill.yaml` at its root (or inside a single
/// wrapping folder, which is stripped). Every entry is validated like a
/// published file: relative path, no `..`, text only, size bounded.
pub fn publish_zip(
    cfg: &Config,
    db: &Db,
    bytes: &[u8],
    author: &Author,
) -> Result<SkillDetail, SkillError> {
    if bytes.len() > MAX_ZIP_BYTES {
        return Err(SkillError::Invalid(format!(
            "the archive must be at most {MAX_ZIP_BYTES} bytes"
        )));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|err| SkillError::Invalid(format!("not a readable zip archive: {err}")))?;
    if archive.len() > MAX_FILES * 2 + 8 {
        return Err(SkillError::Invalid(format!(
            "the archive contains more than {} entries",
            MAX_FILES * 2 + 8
        )));
    }
    let mut entries = Vec::new();
    let mut total = 0usize;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|err| SkillError::Invalid(format!("cannot read zip entry {index}: {err}")))?;
        let raw = file.name().to_string();
        if raw.ends_with('/') {
            continue;
        }
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(SkillError::Invalid(format!(
                "zip entry \"{raw}\" is a symlink"
            )));
        }
        validate_relative_path(&raw).map_err(SkillError::Invalid)?;
        if file.size() > MAX_FILE_BYTES as u64 {
            return Err(SkillError::Invalid(format!(
                "zip entry \"{raw}\" must be at most {MAX_FILE_BYTES} bytes"
            )));
        }
        let mut buffer = Vec::new();
        file.by_ref()
            .take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut buffer)
            .map_err(|err| SkillError::Invalid(format!("cannot read \"{raw}\": {err}")))?;
        if buffer.len() > MAX_FILE_BYTES {
            return Err(SkillError::Invalid(format!(
                "zip entry \"{raw}\" must be at most {MAX_FILE_BYTES} bytes"
            )));
        }
        total = total.saturating_add(buffer.len());
        if total > MAX_TOTAL_BYTES {
            return Err(SkillError::Invalid(format!(
                "the archive expands to more than {MAX_TOTAL_BYTES} bytes"
            )));
        }
        if !is_text(&buffer) {
            return Err(SkillError::Invalid(format!(
                "zip entry \"{raw}\" must be text (no NUL byte)"
            )));
        }
        let content = String::from_utf8(buffer)
            .map_err(|_| SkillError::Invalid(format!("zip entry \"{raw}\" is not valid UTF-8")))?;
        entries.push(ZipEntry { path: raw, content });
    }
    if entries.is_empty() {
        return Err(SkillError::Invalid(
            "the archive contains no file".to_string(),
        ));
    }
    strip_common_root(&mut entries);
    let meta_index = entries
        .iter()
        .position(|entry| entry.path == META_FILE)
        .ok_or_else(|| {
            SkillError::Invalid(format!(
                "the archive must contain {META_FILE} at its root (name and description)"
            ))
        })?;
    let meta: SkillMetaFile = serde_yaml::from_str(&entries[meta_index].content)
        .map_err(|err| SkillError::Invalid(format!("{META_FILE} is not valid YAML: {err}")))?;
    let name = meta
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SkillError::Invalid(format!("{META_FILE} must declare a name")))?
        .to_string();
    let description = meta
        .description
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let version = default_version(cfg, &name);
    let files: Vec<SkillFile> = entries
        .into_iter()
        .filter(|entry| entry.path != META_FILE)
        .map(|entry| SkillFile {
            path: entry.path,
            content: entry.content,
        })
        .collect();
    publish_parts(cfg, db, &name, &version, &description, &files, author)
}

/// Remove a single wrapping folder (`my-skill/SKILL.md` → `SKILL.md`).
fn strip_common_root(entries: &mut [ZipEntry]) {
    let Some(first) = entries.first() else {
        return;
    };
    let Some(root) = first.path.split('/').next() else {
        return;
    };
    if root.is_empty() {
        return;
    }
    let prefix = format!("{root}/");
    if !entries
        .iter()
        .all(|entry| entry.path.starts_with(&prefix) && !entry.path[prefix.len()..].is_empty())
    {
        return;
    }
    for entry in entries.iter_mut() {
        entry.path = entry.path[prefix.len()..].to_string();
    }
}

/// Version a zip import publishes: the next free one, so importing the same
/// archive twice yields `v1` then `v2` instead of a conflict.
fn default_version(cfg: &Config, name: &str) -> String {
    if validate_skill_name(name).is_err() {
        return "v1".to_string();
    }
    let next = versions_on_disk(cfg, name)
        .iter()
        .filter_map(|version| version_number(version))
        .max()
        .unwrap_or(0)
        + 1;
    format!("v{next}")
}

// ---------------------------------------------------------------------------
// Git repository
// ---------------------------------------------------------------------------

fn open_existing(cfg: &Config) -> Result<Option<Repository>, SkillError> {
    if !cfg.skills_dir().join(GIT_DIR).is_dir() {
        return Ok(None);
    }
    Repository::open(cfg.skills_dir())
        .map(Some)
        .map_err(SkillError::from)
}

fn open_or_init(cfg: &Config) -> Result<Repository, SkillError> {
    if let Some(repo) = open_existing(cfg)? {
        return Ok(repo);
    }
    ensure_registry(cfg).map_err(|err| SkillError::io(&cfg.skills_dir(), err))?;
    let repo = Repository::init(cfg.skills_dir())?;
    if repo.head().is_err() {
        // Deterministic first branch, whatever the host global config says.
        let _ = repo.set_head("refs/heads/main");
    }
    Ok(repo)
}

fn head_commit(repo: &Repository) -> Result<Option<Commit<'_>>, SkillError> {
    match repo.head() {
        Ok(head) => Ok(Some(head.peel_to_commit()?)),
        Err(err)
            if matches!(
                err.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) =>
        {
            Ok(None)
        }
        Err(err) => Err(err.into()),
    }
}

/// Versioned paths of the repository: `<name>/latest` and every regular file
/// under `<name>/<version>/` for valid names and versions.
///
/// This whitelist is the reason a secret dropped in a skill directory can
/// never be committed.
fn tracked_files(cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for name in names_on_disk(cfg) {
        let latest_path = cfg.skill_dir(&name).join(LATEST_FILE);
        if fs::symlink_metadata(&latest_path)
            .map(|meta| meta.is_file() && !meta.file_type().is_symlink())
            .unwrap_or(false)
        {
            out.push(format!("{name}/{LATEST_FILE}"));
        }
        for version in versions_on_disk(cfg, &name) {
            let mut paths = Vec::new();
            let root = cfg.skill_version_dir(&name, &version);
            if collect_files(&root, &root, 0, &mut paths).is_err() {
                continue;
            }
            for relative in paths {
                out.push(format!("{name}/{version}/{relative}"));
            }
        }
    }
    out.sort();
    out
}

/// Stage the whitelist and commit it when it differs from `HEAD`.
fn commit(
    cfg: &Config,
    name: &str,
    version: &str,
    author: &Author,
) -> Result<Option<CommitInfo>, SkillError> {
    let repo = open_or_init(cfg)?;
    let files = tracked_files(cfg);
    let mut index = repo.index()?;
    index.clear()?;
    for relative in &files {
        index.add_path(Path::new(relative))?;
    }
    index.write()?;
    let tree_id = index.write_tree()?;
    let head = head_commit(&repo)?;
    if head
        .as_ref()
        .is_some_and(|commit| commit.tree_id() == tree_id)
    {
        return Ok(None);
    }
    if head.is_none() && files.is_empty() {
        return Ok(None);
    }
    let tree = repo.find_tree(tree_id)?;
    let head_tree = head.as_ref().map(|commit| commit.tree()).transpose()?;
    let changed = diff_paths(&repo, head_tree.as_ref(), &tree)?;
    let message = format!("ui: skill {name}@{version}");
    let signature = Signature::now(&author.name, &author.email)?;
    let parents: Vec<Commit<'_>> = head.into_iter().collect();
    let parent_refs: Vec<&Commit<'_>> = parents.iter().collect();
    let oid = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &message,
        &tree,
        &parent_refs,
    )?;
    let id = oid.to_string();
    let commit = repo.find_commit(oid)?;
    Ok(Some(CommitInfo {
        short: short_id(&id),
        id,
        author: author.name.clone(),
        email: author.email.clone(),
        at: commit_time(&commit),
        message,
        paths: changed,
    }))
}

/// `(created_at, updated_at)` of one skill, from its commits.
fn timestamps(cfg: &Config, name: &str) -> (String, String) {
    if let Some((created, updated)) = git_timestamps(cfg, name) {
        return (created, updated);
    }
    fallback_timestamp(&cfg.skill_dir(name))
}

fn fallback_timestamp(path: &Path) -> (String, String) {
    let at = fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|time| {
            chrono::DateTime::<chrono::Utc>::from(time)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
        .unwrap_or_default();
    (at.clone(), at)
}

fn git_timestamps(cfg: &Config, name: &str) -> Option<(String, String)> {
    let repo = open_existing(cfg).ok().flatten()?;
    let head = head_commit(&repo).ok().flatten()?;
    let prefix = format!("{name}/");
    let mut walk = repo.revwalk().ok()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).ok()?;
    walk.push(head.id()).ok()?;
    let mut created = String::new();
    let mut updated = String::new();
    for oid in walk.flatten().take(5000) {
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let Ok(paths) = commit_paths(&repo, &commit) else {
            continue;
        };
        if !paths.iter().any(|path| path.starts_with(&prefix)) {
            continue;
        }
        let at = commit_time(&commit);
        if updated.is_empty() {
            updated = at.clone();
        }
        created = at;
    }
    if updated.is_empty() {
        return None;
    }
    Some((created, updated))
}

/// Publication time of one version: the commit that introduced its files,
/// with the directory mtime as a fallback.
fn version_time(cfg: &Config, name: &str, version: &str) -> String {
    if let Some((_created, updated)) = git_timestamps(cfg, &format!("{name}/{version}")) {
        return updated;
    }
    fallback_timestamp(&cfg.skill_version_dir(name, version)).1
}

/// `GET /api/skills/{name}/git/log` — commits that touched this skill, newest
/// first.
pub fn log(cfg: &Config, name: &str, limit: usize) -> Result<Vec<CommitInfo>, SkillError> {
    validate_skill_name(name).map_err(SkillError::Invalid)?;
    if !cfg.skill_dir(name).is_dir() {
        return Err(SkillError::NotFound(name.to_string()));
    }
    let Some(repo) = open_existing(cfg)? else {
        return Ok(Vec::new());
    };
    let Some(head) = head_commit(&repo)? else {
        return Ok(Vec::new());
    };
    let prefix = format!("{name}/");
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    walk.push(head.id())?;
    let mut commits = Vec::new();
    for oid in walk {
        let commit = repo.find_commit(oid?)?;
        let paths = commit_paths(&repo, &commit)?;
        if !paths.iter().any(|path| path.starts_with(&prefix)) {
            continue;
        }
        commits.push(commit_info(&commit, paths));
        if commits.len() >= limit {
            break;
        }
    }
    Ok(commits)
}

/// `GET /api/skills/{name}/git/diff` — unified patch of this skill only.
pub fn diff(
    cfg: &Config,
    name: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<DiffResponse, SkillError> {
    validate_skill_name(name).map_err(SkillError::Invalid)?;
    if !cfg.skill_dir(name).is_dir() {
        return Err(SkillError::NotFound(name.to_string()));
    }
    let Some(repo) = open_existing(cfg)? else {
        return Ok(DiffResponse::default());
    };
    let Some(head) = head_commit(&repo)? else {
        return Ok(DiffResponse::default());
    };
    let to_commit = match to.map(str::trim).filter(|value| !value.is_empty()) {
        Some(revision) => resolve_commit(&repo, revision)?,
        None => head,
    };
    let from_commit = match from.map(str::trim).filter(|value| !value.is_empty()) {
        Some(revision) => Some(resolve_commit(&repo, revision)?),
        None => to_commit.parent(0).ok(),
    };
    let to_tree = to_commit.tree()?;
    let from_tree = from_commit
        .as_ref()
        .map(|commit| commit.tree())
        .transpose()?;
    let text = render_diff(&repo, &format!("{name}/"), from_tree.as_ref(), &to_tree)?;
    Ok(DiffResponse {
        from: from_commit
            .map(|commit| commit.id().to_string())
            .unwrap_or_default(),
        to: to_commit.id().to_string(),
        diff: text,
    })
}

fn resolve_commit<'repo>(
    repo: &'repo Repository,
    revision: &str,
) -> Result<Commit<'repo>, SkillError> {
    repo.revparse_single(revision)
        .and_then(|object| object.peel_to_commit())
        .map_err(|_| SkillError::UnknownCommit(revision.to_string()))
}

fn commit_paths(repo: &Repository, commit: &Commit<'_>) -> Result<Vec<String>, SkillError> {
    let tree = commit.tree()?;
    let parent = commit
        .parent(0)
        .ok()
        .map(|parent| parent.tree())
        .transpose()?;
    diff_paths(repo, parent.as_ref(), &tree)
}

fn diff_paths(
    repo: &Repository,
    from: Option<&Tree<'_>>,
    to: &Tree<'_>,
) -> Result<Vec<String>, SkillError> {
    let mut options = DiffOptions::new();
    options.include_typechange(true);
    let diff = repo.diff_tree_to_tree(from, Some(to), Some(&mut options))?;
    let mut paths = BTreeSet::new();
    for delta in diff.deltas() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|path| path.to_string_lossy().to_string());
        if let Some(path) = path {
            paths.insert(path);
        }
    }
    Ok(paths.into_iter().collect())
}

fn render_diff(
    repo: &Repository,
    prefix: &str,
    from: Option<&Tree<'_>>,
    to: &Tree<'_>,
) -> Result<String, SkillError> {
    let mut options = DiffOptions::new();
    options.include_typechange(true);
    options.pathspec(prefix);
    let diff = repo.diff_tree_to_tree(from, Some(to), Some(&mut options))?;
    let mut text = String::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        match line.origin() {
            // File and hunk headers already carry their prefix.
            'F' | 'H' | 'B' => text.push_str(&String::from_utf8_lossy(line.content())),
            origin => {
                text.push(origin);
                text.push_str(&String::from_utf8_lossy(line.content()));
            }
        }
        true
    })?;
    Ok(text)
}

fn commit_info(commit: &Commit<'_>, paths: Vec<String>) -> CommitInfo {
    let id = commit.id().to_string();
    let author = commit.author();
    CommitInfo {
        short: short_id(&id),
        id,
        author: author.name().unwrap_or_default().to_string(),
        email: author.email().unwrap_or_default().to_string(),
        at: commit_time(commit),
        message: commit.message().unwrap_or_default().trim_end().to_string(),
        paths,
    }
}

fn commit_time(commit: &Commit<'_>) -> String {
    let time = commit.time();
    chrono::DateTime::<chrono::Utc>::from_timestamp(time.seconds(), 0)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn short_id(id: &str) -> String {
    id.chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_reject_traversal_and_absolute_paths() {
        for valid in [
            "SKILL.md",
            "notes/one.md",
            "a/b/c.txt",
            "deep/file with space.md",
        ] {
            assert!(validate_relative_path(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            "/etc/passwd",
            "../secret",
            "notes/../../secret",
            "notes/./x",
            "notes//x",
            "notes\\x",
            "a\0b",
        ] {
            assert!(validate_relative_path(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn reserved_paths_are_refused() {
        let files = vec![
            SkillFile {
                path: "SKILL.md".to_string(),
                content: "# Skill".to_string(),
            },
            SkillFile {
                path: META_FILE.to_string(),
                content: "name: x".to_string(),
            },
        ];
        let err = validate_files(&files).unwrap_err().to_string();
        assert!(err.contains(META_FILE), "{err}");
    }

    #[test]
    fn skill_md_is_mandatory_and_not_empty() {
        let missing = vec![SkillFile {
            path: "notes.md".to_string(),
            content: "x".to_string(),
        }];
        assert!(validate_files(&missing).is_err());
        let empty = vec![SkillFile {
            path: SKILL_FILE.to_string(),
            content: "   \n".to_string(),
        }];
        assert!(validate_files(&empty).is_err());
    }

    #[test]
    fn total_size_is_bounded() {
        let files = vec![
            SkillFile {
                path: SKILL_FILE.to_string(),
                content: "# Skill".to_string(),
            },
            SkillFile {
                path: "big.txt".to_string(),
                content: "a".repeat(MAX_TOTAL_BYTES + 1),
            },
        ];
        let err = validate_files(&files).unwrap_err().to_string();
        assert!(err.contains("at most"), "{err}");
    }

    #[test]
    fn versions_sort_numerically() {
        let mut versions = vec!["v10".to_string(), "v2".to_string(), "v1".to_string()];
        versions.sort_by_key(|version| version_number(version).unwrap_or(u64::MAX));
        assert_eq!(versions, ["v1", "v2", "v10"]);
    }

    #[test]
    fn common_root_folder_is_stripped() {
        let mut entries = vec![
            ZipEntry {
                path: "my-skill/SKILL.md".to_string(),
                content: "# Skill".to_string(),
            },
            ZipEntry {
                path: "my-skill/notes/a.md".to_string(),
                content: "a".to_string(),
            },
        ];
        strip_common_root(&mut entries);
        assert_eq!(entries[0].path, "SKILL.md");
        assert_eq!(entries[1].path, "notes/a.md");
    }

    #[test]
    fn a_shared_prefix_that_is_a_file_is_kept() {
        let mut entries = vec![
            ZipEntry {
                path: "SKILL.md".to_string(),
                content: "# Skill".to_string(),
            },
            ZipEntry {
                path: "notes/a.md".to_string(),
                content: "a".to_string(),
            },
        ];
        strip_common_root(&mut entries);
        assert_eq!(entries[0].path, "SKILL.md");
    }
}
