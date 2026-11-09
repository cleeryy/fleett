//! Confined file access for the instance file browser.
//!
//! The console edits the files of one instance (`SOUL.md`, notes, profiles)
//! without an SSH session. Every path the API accepts is virtual: it starts
//! with `/`, which is the *data directory* of the instance
//! (`FLEETT_ROOT/instances/<id>/data`), never the host root. The syntactic
//! checks below reject `..` and NUL before the filesystem is touched, the
//! parent chain is canonicalized so a symlinked directory cannot redirect a
//! write, and a symlink is followed only when its resolved target stays inside
//! the data directory.
//!
//! This module never authenticates and never audits: it is the filesystem
//! layer, the HTTP handlers in [`crate::api::files`] own the policy.

use crate::config::Config;
use crate::store::{CONTAINER_GID, CONTAINER_UID};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Largest file the browser reads or writes (512 KiB).
pub const MAX_CONTENT_BYTES: u64 = 512 * 1024;

/// Extensions the console may edit; everything else is read-only.
pub const EDITABLE_EXTENSIONS: &[&str] = &["md", "yaml", "yml", "json", "txt", "toml"];

/// Files rendered by fleett on every PATCH: a manual edit is overwritten.
pub const RENDERED_FILES: &[&str] = &["config.yaml", "healthcheck.py"];

/// Explanation carried by the response (and the `409`) of a rendered file.
pub const RENDERED_NOTE: &str = "this file is rendered by fleett from the instance configuration; a manual edit is overwritten on the next PATCH, so change the instance instead";

/// One directory entry.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    /// Virtual path, always rooted at the data directory (`/SOUL.md`).
    pub path: String,
    /// `file` or `dir`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub size: u64,
    /// RFC 3339, second precision.
    pub mtime: String,
    /// Whether the editor may save this entry.
    pub editable: bool,
    /// Whether fleett overwrites this file on the next PATCH.
    pub rendered: bool,
}

/// Body of `GET /api/instances/{id}/files/content`.
#[derive(Debug, Clone, Serialize)]
pub struct Content {
    pub path: String,
    pub content: String,
    /// SHA-256 of the bytes on disk, lowercase hex.
    pub etag: String,
    pub size: u64,
    pub mtime: String,
    pub editable: bool,
    pub rendered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Result of a successful write.
#[derive(Debug, Clone, Serialize)]
pub struct WriteResult {
    pub path: String,
    pub etag: String,
    pub size: u64,
    pub mtime: String,
    pub editable: bool,
    pub rendered: bool,
    /// Virtual path of the `.bak-YYYYMMDD-HHMMSS` copy, when one was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
}

/// Why a file operation was refused. The HTTP layer maps every variant.
#[derive(Debug)]
pub enum FileError {
    /// Syntactically invalid virtual path (absolute-host path, `..`, NUL).
    InvalidPath(String),
    /// Nothing exists at that path.
    NotFound(String),
    /// A symlink (or the parent chain) resolves outside the data directory.
    Escape(String),
    /// The path names a directory where a file is required.
    NotAFile(String),
    /// The path names a file where a directory is required.
    NotADirectory(String),
    /// Socket, FIFO, device: never browsed, edited or deleted.
    Unsupported(String),
    /// Above [`MAX_CONTENT_BYTES`].
    TooLarge {
        size: u64,
        limit: u64,
    },
    /// Not valid UTF-8, or carries a NUL byte.
    Binary(String),
    /// Rendered by fleett; the next PATCH would erase a manual edit.
    Rendered(String),
    /// Extension outside [`EDITABLE_EXTENSIONS`].
    NotEditable(String),
    /// The target already exists (mkdir).
    AlreadyExists(String),
    /// `DELETE` on a directory that still holds entries.
    DirectoryNotEmpty(String),
    /// The file changed since the client read it (`409 etag_mismatch`).
    EtagMismatch(Option<String>),
    Io(io::Error),
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(message)
            | Self::NotFound(message)
            | Self::Escape(message)
            | Self::NotAFile(message)
            | Self::NotADirectory(message)
            | Self::Unsupported(message)
            | Self::Binary(message)
            | Self::Rendered(message)
            | Self::NotEditable(message)
            | Self::AlreadyExists(message)
            | Self::DirectoryNotEmpty(message) => f.write_str(message),
            Self::TooLarge { size, limit } => write!(
                f,
                "the file is {size} bytes, above the {limit}-byte limit of the editor"
            ),
            Self::EtagMismatch(_) => {
                f.write_str("the file changed since it was loaded: reload it and retry")
            }
            Self::Io(err) => write!(f, "filesystem error: {err}"),
        }
    }
}

impl std::error::Error for FileError {}

impl From<io::Error> for FileError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// SHA-256 of `bytes`, lowercase hex. The `etag` of every response.
pub fn etag(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut out = String::with_capacity(64);
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Whether the editor may save this virtual path.
pub fn is_editable(display: &str, size: u64) -> bool {
    size <= MAX_CONTENT_BYTES && !is_rendered(display) && is_editable_extension(display)
}

/// Whether fleett regenerates this exact file on the next PATCH.
pub fn is_rendered(display: &str) -> bool {
    RENDERED_FILES.contains(&display.trim_start_matches('/'))
}

fn is_editable_extension(display: &str) -> bool {
    display
        .rsplit_once('.')
        .is_some_and(|(_, ext)| EDITABLE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// One resolved target: the canonical parent, the final name and the virtual
/// path shown to the client.
struct Target {
    root: PathBuf,
    path: PathBuf,
    name: Option<String>,
    display: String,
}

/// Split a virtual path into its segments, rejecting everything unsafe.
fn normalize(raw: &str) -> Result<(String, Vec<String>), FileError> {
    if raw.contains('\0') {
        return Err(FileError::InvalidPath(
            "the path must not contain a NUL byte".to_string(),
        ));
    }
    let trimmed = raw.trim();
    let raw = if trimmed.is_empty() { "/" } else { trimmed };
    if !raw.starts_with('/') {
        return Err(FileError::InvalidPath(format!(
            "\"{raw}\" is not an absolute path: every path starts with \"/\", the root of the \
             instance data directory (e.g. /SOUL.md)"
        )));
    }
    let mut segments = Vec::new();
    for segment in raw.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                return Err(FileError::InvalidPath(
                    "the path must not contain \"..\" segments".to_string(),
                ))
            }
            other => segments.push(other.to_string()),
        }
    }
    let display = format!("/{}", segments.join("/"));
    Ok((display, segments))
}

/// Canonical data directory of the instance, or `404`.
fn data_root(cfg: &Config, id: &str) -> Result<PathBuf, FileError> {
    let root = cfg.data_dir(id);
    fs::canonicalize(&root).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            FileError::NotFound(format!("instance \"{id}\" has no data directory"))
        }
        _ => FileError::Io(err),
    })
}

/// Resolve one virtual path to a canonical parent plus its final component.
///
/// The parent is canonicalized and checked against the root, so a symlinked
/// directory pointing outside can never receive a write. The final component is
/// *not* followed here: callers decide whether to follow a symlink (read) or
/// operate on the link itself (delete).
fn locate(cfg: &Config, id: &str, raw: &str) -> Result<Target, FileError> {
    let (display, segments) = normalize(raw)?;
    let root = data_root(cfg, id)?;
    let Some(name) = segments.last().cloned() else {
        return Ok(Target {
            path: root.clone(),
            root,
            name: None,
            display,
        });
    };

    let mut raw_path = root.clone();
    for segment in &segments {
        raw_path.push(segment);
    }
    let raw_parent = raw_path.parent().unwrap_or(&root);
    let parent = fs::canonicalize(raw_parent).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            FileError::NotFound(format!("directory not found: {}", parent_display(&display)))
        }
        io::ErrorKind::NotADirectory => {
            FileError::NotADirectory(format!("{} is not a directory", parent_display(&display)))
        }
        _ => FileError::Io(err),
    })?;
    ensure_inside(&root, &parent)?;
    let path = parent.join(&name);
    Ok(Target {
        root,
        path,
        name: Some(name),
        display,
    })
}

/// Refuse a path that leaves the data directory (`Path::starts_with` compares
/// whole components, so `/data2` never matches `/data`).
fn ensure_inside(root: &Path, path: &Path) -> Result<(), FileError> {
    if path.starts_with(root) {
        return Ok(());
    }
    Err(FileError::Escape(format!(
        "the path resolves outside the instance data directory: {}",
        path.display()
    )))
}

/// Map a failed `lstat`/`stat` on a virtual path.
///
/// `ENOTDIR` is the "a file sits where a directory is required" case
/// (`/SOUL.md/skills`), which must be a `400`, not a `500`.
fn stat_error(display: &str, err: io::Error) -> FileError {
    match err.kind() {
        io::ErrorKind::NotFound => FileError::NotFound(format!("\"{display}\" does not exist")),
        io::ErrorKind::NotADirectory => FileError::NotADirectory(format!(
            "\"{display}\" has a file where a directory is required"
        )),
        _ => FileError::Io(err),
    }
}

/// Refuse an operation whose *real* target is one of the rendered files, even
/// when it is reached through a symlinked directory or a link.
fn ensure_not_rendered(target: &Target, real: &Path) -> Result<(), FileError> {
    if let Ok(relative) = real.strip_prefix(&target.root) {
        let relative = relative.to_string_lossy();
        if RENDERED_FILES.contains(&relative.as_ref()) {
            return Err(FileError::Rendered(target.display.clone()));
        }
    }
    Ok(())
}

/// Follow the target (including a final symlink) and check it stays inside the
/// root. `404` for a missing file or a dangling link.
fn follow(target: &Target) -> Result<(PathBuf, fs::Metadata), FileError> {
    let link =
        fs::symlink_metadata(&target.path).map_err(|err| stat_error(&target.display, err))?;
    let path = if link.file_type().is_symlink() {
        fs::canonicalize(&target.path).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                FileError::NotFound(format!("\"{}\" is a dangling symlink", target.display))
            }
            _ => FileError::Io(err),
        })?
    } else {
        target.path.clone()
    };
    ensure_inside(&target.root, &path)?;
    let meta = fs::metadata(&path)?;
    Ok((path, meta))
}

/// `file` / `dir`, or `None` for a socket, FIFO or device.
fn kind_of(meta: &fs::Metadata) -> Option<&'static str> {
    let file_type = meta.file_type();
    if file_type.is_dir() {
        Some("dir")
    } else if file_type.is_file() {
        Some("file")
    } else {
        None
    }
}

fn mtime_str(meta: &fs::Metadata) -> String {
    match meta.modified() {
        Ok(time) => chrono::DateTime::<chrono::Utc>::from(time)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        Err(_) => String::new(),
    }
}

/// `/a/b` -> `/a` (and `/b` -> `/`).
fn parent_display(display: &str) -> String {
    match display.rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
    }
}

/// Virtual path of `name` in the directory of `display`.
fn sibling_display(display: &str, name: &str) -> String {
    match display.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{name}"),
        None => format!("/{name}"),
    }
}

/// Best-effort hand-over to the container UID so the agent keeps writing its
/// own home. A non-root control plane simply keeps its own files.
fn chown_container(path: &Path) {
    if let Err(err) = std::os::unix::fs::chown(path, Some(CONTAINER_UID), Some(CONTAINER_GID)) {
        if err.kind() != io::ErrorKind::PermissionDenied {
            tracing::debug!(path = %path.display(), %err, "could not change file ownership");
        }
    }
}

/// One level of `ls`. Directories come first, then names, case-insensitively.
///
/// Entries that are not regular files or directories (sockets, FIFOs,
/// devices) and symlinks escaping the data directory are skipped: the console
/// must never advertise a path it would refuse to open.
pub fn list(cfg: &Config, id: &str, raw: &str) -> Result<Vec<Entry>, FileError> {
    let target = locate(cfg, id, raw)?;
    let (path, meta) = follow(&target)?;
    if !meta.is_dir() {
        return Err(FileError::NotADirectory(format!(
            "\"{}\" is not a directory",
            target.display
        )));
    }

    let mut entries = Vec::new();
    for item in fs::read_dir(&path)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().to_string();
        let item_path = item.path();
        let Ok(link) = fs::symlink_metadata(&item_path) else {
            continue;
        };
        let item_meta = if link.file_type().is_symlink() {
            let Ok(resolved) = fs::canonicalize(&item_path) else {
                continue;
            };
            if !resolved.starts_with(&target.root) {
                continue;
            }
            let Ok(meta) = fs::metadata(&resolved) else {
                continue;
            };
            meta
        } else {
            link
        };
        let Some(kind) = kind_of(&item_meta) else {
            continue;
        };
        let display = if target.display == "/" {
            format!("/{name}")
        } else {
            format!("{}/{}", target.display, name)
        };
        let size = item_meta.len();
        entries.push(Entry {
            name,
            editable: kind == "file" && is_editable(&display, size),
            rendered: is_rendered(&display),
            path: display,
            kind,
            size,
            mtime: mtime_str(&item_meta),
        });
    }
    entries.sort_by(|left, right| {
        (left.kind != "dir")
            .cmp(&(right.kind != "dir"))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(entries)
}

/// Read one text file. Binary, oversized and special files are refused.
pub fn read(cfg: &Config, id: &str, raw: &str) -> Result<Content, FileError> {
    let target = locate(cfg, id, raw)?;
    if target.name.is_none() {
        return Err(FileError::NotAFile(format!(
            "\"{}\" is the data directory, not a file",
            target.display
        )));
    }
    let (path, meta) = follow(&target)?;
    match kind_of(&meta) {
        Some("file") => {}
        Some(_) => {
            return Err(FileError::NotAFile(format!(
                "\"{}\" is a directory",
                target.display
            )))
        }
        None => {
            return Err(FileError::Unsupported(format!(
                "\"{}\" is not a regular file",
                target.display
            )))
        }
    }
    if meta.len() > MAX_CONTENT_BYTES {
        return Err(FileError::TooLarge {
            size: meta.len(),
            limit: MAX_CONTENT_BYTES,
        });
    }
    let bytes = fs::read(&path)?;
    let Ok(text) = String::from_utf8(bytes) else {
        return Err(FileError::Binary(format!(
            "\"{}\" is not UTF-8 text",
            target.display
        )));
    };
    if text.contains('\0') {
        return Err(FileError::Binary(format!(
            "\"{}\" contains a NUL byte",
            target.display
        )));
    }
    let size = text.len() as u64;
    let editable = is_editable(&target.display, size);
    let rendered = is_rendered(&target.display);
    Ok(Content {
        note: rendered.then(|| RENDERED_NOTE.to_string()),
        path: target.display,
        etag: etag(text.as_bytes()),
        content: text,
        size,
        mtime: mtime_str(&meta),
        editable,
        rendered,
    })
}

/// Path of the backup written before an overwrite, never colliding.
fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let mut candidate = path.with_file_name(format!("{name}.bak-{stamp}"));
    let mut attempt = 1;
    while candidate.exists() {
        candidate = path.with_file_name(format!("{name}.bak-{stamp}-{attempt}"));
        attempt += 1;
    }
    candidate
}

/// Create or overwrite one editable text file.
///
/// `expected` is the etag the client loaded (`*` means "any version, the file
/// must exist"). A missing etag on an existing file is a conflict, never an
/// unconditional overwrite. The previous bytes are copied to a timestamped
/// `.bak-YYYYMMDD-HHMMSS` sibling before the write.
pub fn write(
    cfg: &Config,
    id: &str,
    raw: &str,
    expected: Option<&str>,
    content: &str,
) -> Result<WriteResult, FileError> {
    let target = locate(cfg, id, raw)?;
    if target.name.is_none() {
        return Err(FileError::NotAFile(
            "the data directory itself cannot be edited".to_string(),
        ));
    }
    if is_rendered(&target.display) {
        return Err(FileError::Rendered(target.display.clone()));
    }
    if !is_editable_extension(&target.display) {
        return Err(FileError::NotEditable(target.display.clone()));
    }
    let size = content.len() as u64;
    if size > MAX_CONTENT_BYTES {
        return Err(FileError::TooLarge {
            size,
            limit: MAX_CONTENT_BYTES,
        });
    }
    if content.contains('\0') {
        return Err(FileError::Binary(
            "the content must not contain a NUL byte".to_string(),
        ));
    }

    let link = match fs::symlink_metadata(&target.path) {
        Ok(meta) => Some(meta),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(stat_error(&target.display, err)),
    };
    let (write_path, previous, mode) = match link {
        Some(link) => {
            let path = if link.file_type().is_symlink() {
                let resolved = fs::canonicalize(&target.path).map_err(|err| match err.kind() {
                    io::ErrorKind::NotFound => {
                        FileError::NotFound(format!("\"{}\" is a dangling symlink", target.display))
                    }
                    _ => FileError::Io(err),
                })?;
                ensure_inside(&target.root, &resolved)?;
                resolved
            } else {
                target.path.clone()
            };
            let meta = fs::metadata(&path)?;
            match kind_of(&meta) {
                Some("file") => {}
                Some(_) => {
                    return Err(FileError::NotAFile(format!(
                        "\"{}\" is a directory",
                        target.display
                    )))
                }
                None => {
                    return Err(FileError::Unsupported(format!(
                        "\"{}\" is not a regular file",
                        target.display
                    )))
                }
            }
            if meta.len() > MAX_CONTENT_BYTES {
                return Err(FileError::TooLarge {
                    size: meta.len(),
                    limit: MAX_CONTENT_BYTES,
                });
            }
            let mode = meta.permissions().mode() & 0o7777;
            let bytes = fs::read(&path)?;
            (path, Some((meta, bytes)), Some(mode))
        }
        None => (target.path.clone(), None, None),
    };

    ensure_not_rendered(&target, &write_path)?;

    let current = previous.as_ref().map(|(_, bytes)| etag(bytes));
    let matches = match (expected, current.as_deref()) {
        (Some("*"), Some(_)) => true,
        (Some("*"), None) => false,
        // An existing file always needs the version the client loaded.
        (None, Some(_)) => false,
        (Some(value), Some(current)) => value == current,
        (Some(value), None) => value.is_empty(),
        (None, None) => true,
    };
    if !matches {
        return Err(FileError::EtagMismatch(current));
    }

    let backup = match &previous {
        Some(_) => {
            // Next to the file the operator sees: for a symlink that is the
            // link, not its target.
            let path = backup_path(&target.path);
            fs::copy(&write_path, &path)?;
            Some(sibling_display(
                &target.display,
                &path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default(),
            ))
        }
        None => None,
    };

    let identity = previous.as_ref().map(|(meta, _)| (meta.dev(), meta.ino()));
    let mut options = fs::OpenOptions::new();
    options.write(true).truncate(true).mode(0o600);
    if previous.is_some() {
        options.create(true);
    } else {
        // O_CREAT|O_EXCL never follows a symlink planted since the lstat.
        options.create_new(true);
    }
    let mut file = match options.open(&write_path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            return Err(FileError::EtagMismatch(None))
        }
        Err(err) => return Err(FileError::Io(err)),
    };
    // The file was replaced between the etag read and the open: the etag the
    // client sent no longer describes what a rename would overwrite.
    if let Some((device, inode)) = identity {
        let opened = file.metadata()?;
        if (opened.dev(), opened.ino()) != (device, inode) {
            return Err(FileError::EtagMismatch(None));
        }
    }
    file.write_all(content.as_bytes())?;
    file.flush()?;
    drop(file);

    if let Some(mode) = mode {
        let _ = fs::set_permissions(&write_path, fs::Permissions::from_mode(mode));
    }
    chown_container(&write_path);

    let meta = fs::metadata(&write_path)?;
    Ok(WriteResult {
        path: target.display,
        etag: etag(content.as_bytes()),
        size: meta.len(),
        mtime: mtime_str(&meta),
        editable: true,
        rendered: false,
        backup,
    })
}

/// Delete one file, or one *empty* directory.
pub fn delete(cfg: &Config, id: &str, raw: &str) -> Result<String, FileError> {
    let target = locate(cfg, id, raw)?;
    if target.name.is_none() {
        return Err(FileError::NotAFile(
            "the data directory itself cannot be deleted".to_string(),
        ));
    }
    if is_rendered(&target.display) {
        return Err(FileError::Rendered(target.display.clone()));
    }
    let link =
        fs::symlink_metadata(&target.path).map_err(|err| stat_error(&target.display, err))?;
    if link.file_type().is_symlink() {
        // Refuse to unlink a link whose target escapes: the console must not be
        // a way to clean up an escape someone else planted.
        if let Ok(resolved) = fs::canonicalize(&target.path) {
            ensure_inside(&target.root, &resolved)?;
            ensure_not_rendered(&target, &resolved)?;
        }
        fs::remove_file(&target.path)?;
    } else if link.is_dir() {
        let mut entries = fs::read_dir(&target.path)?;
        if entries.next().is_some() {
            return Err(FileError::DirectoryNotEmpty(format!(
                "\"{}\" is not empty",
                target.display
            )));
        }
        fs::remove_dir(&target.path)?;
    } else if link.is_file() {
        let resolved = fs::canonicalize(&target.path)?;
        ensure_inside(&target.root, &resolved)?;
        ensure_not_rendered(&target, &resolved)?;
        fs::remove_file(&target.path)?;
    } else {
        return Err(FileError::Unsupported(format!(
            "\"{}\" is not a regular file or directory",
            target.display
        )));
    }
    Ok(target.display)
}

/// Create one directory (the parent must exist), owned by the container UID.
pub fn mkdir(cfg: &Config, id: &str, raw: &str) -> Result<String, FileError> {
    let target = locate(cfg, id, raw)?;
    if target.name.is_none() {
        return Err(FileError::AlreadyExists(
            "the data directory already exists".to_string(),
        ));
    }
    match fs::symlink_metadata(&target.path) {
        Ok(_) => return Err(FileError::AlreadyExists(target.display.clone())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(stat_error(&target.display, err)),
    }
    fs::create_dir(&target.path).map_err(|err| match err.kind() {
        io::ErrorKind::AlreadyExists => FileError::AlreadyExists(target.display.clone()),
        _ => FileError::Io(err),
    })?;
    let _ = fs::set_permissions(&target.path, fs::Permissions::from_mode(0o755));
    chown_container(&target.path);
    Ok(target.display)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_virtual_paths() {
        assert_eq!(normalize("/").expect("root"), ("/".to_string(), vec![]));
        assert_eq!(normalize("").expect("empty"), ("/".to_string(), vec![]));
        assert_eq!(
            normalize("//a//./b/").expect("segments"),
            ("/a/b".to_string(), vec!["a".to_string(), "b".to_string()])
        );
        assert!(matches!(
            normalize("../../fleet.db"),
            Err(FileError::InvalidPath(_))
        ));
        assert!(matches!(
            normalize("/a/../fleet.db"),
            Err(FileError::InvalidPath(_))
        ));
        assert!(matches!(normalize("/a\0b"), Err(FileError::InvalidPath(_))));
    }

    #[test]
    fn editability_follows_the_allowlist() {
        assert!(is_editable("/SOUL.md", 10));
        assert!(is_editable("/notes/PLAN.TOML", 10));
        assert!(!is_editable("/avatar.png", 10));
        assert!(!is_editable("/config.yaml", 10));
        assert!(!is_editable("/healthcheck.py", 10));
        assert!(!is_editable("/big.md", MAX_CONTENT_BYTES + 1));
        assert!(is_rendered("/config.yaml"));
        assert!(!is_rendered("/skills/config.yaml"));
    }

    #[test]
    fn etag_is_stable_sha256_hex() {
        assert_eq!(
            etag(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(etag(b"hello").len(), 64);
        assert_ne!(etag(b"hello"), etag(b"hello "));
    }
}
