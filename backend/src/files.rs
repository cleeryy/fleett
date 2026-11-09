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

/// Largest single file a multipart upload may carry (10 MiB).
pub const MAX_UPLOAD_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Largest total payload of one upload request (32 MiB).
pub const MAX_UPLOAD_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

/// Largest file `GET .../files/download` serves (50 MiB).
pub const MAX_DOWNLOAD_BYTES: u64 = 50 * 1024 * 1024;

/// Largest uncompressed tree one archive may carry (200 MiB).
pub const MAX_ARCHIVE_BYTES: u64 = 200 * 1024 * 1024;

/// Files above this size are never read by the full-text search (1 MiB).
pub const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;

/// Hits a single search returns before it stops.
pub const MAX_SEARCH_RESULTS: usize = 200;

/// Files one search visits before it stops.
pub const MAX_SEARCH_FILES: usize = 50_000;

/// Longest preview kept for one hit (characters, `…` included).
pub const MAX_PREVIEW_CHARS: usize = 240;

/// Extensions the console may edit; everything else is read-only.
pub const EDITABLE_EXTENSIONS: &[&str] = &["md", "yaml", "yml", "json", "txt", "toml"];

/// Extensions refused by the upload endpoint: the data directory of an agent
/// is executed inside the container, so an operator must not plant a binary
/// there by accident (or through a compromised console session).
pub const BLOCKED_EXTENSIONS: &[&str] = &[
    "exe", "com", "bat", "cmd", "msi", "dll", "so", "dylib", "bin", "app", "scr", "ps1", "sh",
    "bash", "zsh",
];

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

/// One file written by [`upload`].
#[derive(Debug, Clone, Serialize)]
pub struct UploadedFile {
    /// Virtual path of the new bytes (`/notes/todo.md`).
    pub path: String,
    pub size: u64,
    /// Virtual path of the `.bak-YYYYMMDD-HHMMSS` copy when the upload
    /// replaced an existing file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
}

/// Result of one multipart upload. The request is validated as a whole before
/// the first write, so a refused request never leaves half its files behind.
#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    /// Virtual path of the target directory.
    pub directory: String,
    pub files: Vec<UploadedFile>,
    /// Sum of the bytes written.
    pub total: u64,
}

/// One file served by [`download`].
#[derive(Debug, Clone)]
pub struct Download {
    pub path: String,
    /// Final component, used as the `Content-Disposition` filename.
    pub name: String,
    pub bytes: Vec<u8>,
    /// Media type inferred from the extension.
    pub content_type: &'static str,
}

/// One zip archive built by [`archive`].
#[derive(Debug, Clone)]
pub struct Archive {
    /// `None` for the data directory itself, `notes` for `/notes`.
    pub name: Option<String>,
    /// Entries written (files, symlinks and directories).
    pub entries: usize,
    pub bytes: Vec<u8>,
}

/// One full-text search hit.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    /// Virtual path of the file.
    pub path: String,
    /// 1-based line number.
    pub line: u32,
    /// Trimmed line, truncated to [`MAX_PREVIEW_CHARS`].
    pub preview: String,
}

/// Result of one full-text search.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub results: Vec<SearchHit>,
    /// `true` when the scan stopped at the result cap or the file cap.
    pub truncated: bool,
    /// Files opened (or skipped for their size) by the scan.
    pub scanned: usize,
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
    /// The target already exists (mkdir, or an upload without `?overwrite`).
    AlreadyExists(String),
    /// The target already exists and the upload may not replace it.
    Exists(String),
    /// Executable by extension or by ELF/Mach-O/PE magic bytes.
    Executable(String),
    /// Malformed upload (no file part, duplicate file name, empty name).
    InvalidUpload(String),
    /// Empty or unusable search query.
    InvalidQuery(String),
    /// `regex=true` and the pattern does not compile.
    InvalidRegex(String),
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
            | Self::Exists(message)
            | Self::Executable(message)
            | Self::InvalidUpload(message)
            | Self::InvalidQuery(message)
            | Self::InvalidRegex(message)
            | Self::DirectoryNotEmpty(message) => f.write_str(message),
            Self::TooLarge { size, limit } => {
                write!(f, "the file is {size} bytes, above the {limit}-byte limit")
            }
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
fn ensure_not_rendered(root: &Path, display: &str, real: &Path) -> Result<(), FileError> {
    if let Ok(relative) = real.strip_prefix(root) {
        let relative = relative.to_string_lossy();
        if RENDERED_FILES.contains(&relative.as_ref()) {
            return Err(FileError::Rendered(display.to_string()));
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
pub(crate) fn backup_path(path: &Path) -> PathBuf {
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

    ensure_not_rendered(&target.root, &target.display, &write_path)?;

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
            ensure_not_rendered(&target.root, &target.display, &resolved)?;
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
        ensure_not_rendered(&target.root, &target.display, &resolved)?;
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

/// Whether the upload endpoint refuses this extension (case-insensitive).
pub fn has_executable_extension(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| BLOCKED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Whether the first bytes identify a native executable (ELF, Mach-O, PE).
pub fn has_executable_magic(bytes: &[u8]) -> bool {
    const MACHO: [[u8; 4]; 5] = [
        // 32/64-bit, both endiannesses, plus the universal ("fat") header.
        [0xfe, 0xed, 0xfa, 0xce],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xca, 0xfe, 0xba, 0xbe],
    ];
    if bytes.starts_with(b"\x7fELF") {
        return true;
    }
    if bytes.len() >= 4 && MACHO.iter().any(|magic| bytes.starts_with(magic)) {
        return true;
    }
    // PE: the DOS "MZ" stub, then "PE\0\0" at the offset stored at 0x3c.
    if bytes.starts_with(b"MZ") && bytes.len() >= 0x40 {
        let offset =
            u32::from_le_bytes([bytes[0x3c], bytes[0x3d], bytes[0x3e], bytes[0x3f]]) as usize;
        return bytes
            .get(offset..offset.saturating_add(4))
            .is_some_and(|signature| signature == b"PE\0\0");
    }
    false
}

/// Media type of a downloaded file, inferred from its extension.
pub fn content_type_for(path: &str) -> &'static str {
    let extension = path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase());
    match extension.as_deref() {
        Some("md" | "markdown") => "text/markdown; charset=utf-8",
        Some("txt" | "log" | "conf" | "ini") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        Some("yaml" | "yml") => "application/yaml",
        Some("toml") => "application/toml",
        Some("csv") => "text/csv; charset=utf-8",
        Some("htm" | "html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("xml") => "application/xml",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("gz" | "tgz") => "application/gzip",
        Some("tar") => "application/x-tar",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

/// One uploaded file name: a single component, never a path.
fn validate_upload_name(name: &str) -> Result<(), FileError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(FileError::InvalidUpload(
            "every file part must carry a name".to_string(),
        ));
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(FileError::InvalidPath(format!(
            "\"{name}\" is not a file name: the upload targets one directory and carries names, \
             never paths"
        )));
    }
    Ok(())
}

/// Write one multipart upload into `raw_dir` through the regular confinement.
///
/// The whole request is validated before the first byte is written (name,
/// size, executable blocklist and magic bytes, existing targets and rendered
/// files), so a refused upload leaves the data directory untouched. An
/// existing target is only replaced with `overwrite`, and the previous bytes
/// are copied to a `.bak-YYYYMMDD-HHMMSS` sibling first, exactly like
/// [`write`].
pub fn upload(
    cfg: &Config,
    id: &str,
    raw_dir: &str,
    uploads: &[(String, Vec<u8>)],
    overwrite: bool,
) -> Result<UploadResult, FileError> {
    if uploads.is_empty() {
        return Err(FileError::InvalidUpload(
            "the request carries no \"file\" part".to_string(),
        ));
    }
    let mut total = 0u64;
    let mut names: Vec<&str> = Vec::with_capacity(uploads.len());
    for (name, bytes) in uploads {
        validate_upload_name(name)?;
        if names.contains(&name.as_str()) {
            return Err(FileError::InvalidUpload(format!(
                "the request carries \"{name}\" twice"
            )));
        }
        names.push(name);
        let size = bytes.len() as u64;
        if size > MAX_UPLOAD_FILE_BYTES {
            return Err(FileError::TooLarge {
                size,
                limit: MAX_UPLOAD_FILE_BYTES,
            });
        }
        if has_executable_extension(name) {
            return Err(FileError::Executable(format!(
                "\"{name}\" has an executable extension and cannot be uploaded"
            )));
        }
        if has_executable_magic(bytes) {
            return Err(FileError::Executable(format!(
                "\"{name}\" is a native executable (ELF, Mach-O or PE) and cannot be uploaded"
            )));
        }
        total = total.saturating_add(size);
        if total > MAX_UPLOAD_TOTAL_BYTES {
            return Err(FileError::TooLarge {
                size: total,
                limit: MAX_UPLOAD_TOTAL_BYTES,
            });
        }
    }

    let target = locate(cfg, id, raw_dir)?;
    let (directory, meta) = follow(&target)?;
    if !meta.is_dir() {
        return Err(FileError::NotADirectory(format!(
            "\"{}\" is not a directory",
            target.display
        )));
    }

    // Second pass, still before the first write: resolve every target and
    // refuse the whole request on one conflict, so an upload never leaves half
    // of its files behind because the last one already existed.
    let mut plans = Vec::with_capacity(uploads.len());
    for (name, _) in uploads {
        let display = if target.display == "/" {
            format!("/{name}")
        } else {
            format!("{}/{}", target.display, name)
        };
        if is_rendered(&display) {
            return Err(FileError::Rendered(display));
        }
        // The backup stays next to the name the client sent, even when the
        // final component is a symlink replaced in place.
        let link_path = directory.join(name);
        let existing = match fs::symlink_metadata(&link_path) {
            Ok(meta) => Some(meta),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(stat_error(&display, err)),
        };
        let (write_path, previous, mode) = match existing {
            Some(link) => {
                if !overwrite {
                    return Err(FileError::Exists(display));
                }
                let path = if link.file_type().is_symlink() {
                    let resolved =
                        fs::canonicalize(&link_path).map_err(|err| match err.kind() {
                            io::ErrorKind::NotFound => {
                                FileError::NotFound(format!("\"{display}\" is a dangling symlink"))
                            }
                            _ => FileError::Io(err),
                        })?;
                    ensure_inside(&target.root, &resolved)?;
                    resolved
                } else {
                    link_path.clone()
                };
                let meta = fs::metadata(&path)?;
                match kind_of(&meta) {
                    Some("file") => {}
                    Some(_) => {
                        return Err(FileError::NotAFile(format!("\"{display}\" is a directory")))
                    }
                    None => {
                        return Err(FileError::Unsupported(format!(
                            "\"{display}\" is not a regular file"
                        )))
                    }
                }
                let mode = meta.permissions().mode() & 0o7777;
                (path, Some(meta), Some(mode))
            }
            None => (link_path.clone(), None, None),
        };
        ensure_not_rendered(&target.root, &display, &write_path)?;
        plans.push((display, link_path, write_path, previous, mode));
    }

    let mut written = Vec::with_capacity(uploads.len());
    for ((display, link_path, write_path, previous, mode), (_, bytes)) in
        plans.into_iter().zip(uploads)
    {
        let backup = match &previous {
            Some(_) => {
                // Next to the name the operator sees, as a symlink is replaced
                // in place rather than through its target.
                let backup = backup_path(&link_path);
                fs::copy(&write_path, &backup)?;
                let name = backup
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                Some(sibling_display(&display, &name))
            }
            None => None,
        };

        let identity = previous.as_ref().map(|meta| (meta.dev(), meta.ino()));
        let mut options = fs::OpenOptions::new();
        options.write(true).truncate(true).mode(0o600);
        if previous.is_some() {
            options.create(true);
        } else {
            options.create_new(true);
        }
        let mut file = match options.open(&write_path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                return Err(FileError::Exists(display))
            }
            Err(err) => return Err(FileError::Io(err)),
        };
        if let Some((device, inode)) = identity {
            let opened = file.metadata()?;
            if (opened.dev(), opened.ino()) != (device, inode) {
                return Err(FileError::Exists(display));
            }
        }
        file.write_all(bytes)?;
        file.flush()?;
        drop(file);
        if let Some(mode) = mode {
            let _ = fs::set_permissions(&write_path, fs::Permissions::from_mode(mode));
        }
        chown_container(&write_path);
        written.push(UploadedFile {
            path: display,
            size: bytes.len() as u64,
            backup,
        });
    }

    Ok(UploadResult {
        directory: target.display,
        files: written,
        total,
    })
}

/// Read one file for the download endpoint. Binary content is allowed here:
/// the download is the explicit "give me the exact bytes" gesture, unlike the
/// text editor.
pub fn download(cfg: &Config, id: &str, raw: &str) -> Result<Download, FileError> {
    let target = locate(cfg, id, raw)?;
    let Some(name) = target.name.clone() else {
        return Err(FileError::NotAFile(format!(
            "\"{}\" is the data directory, not a file",
            target.display
        )));
    };
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
    if meta.len() > MAX_DOWNLOAD_BYTES {
        return Err(FileError::TooLarge {
            size: meta.len(),
            limit: MAX_DOWNLOAD_BYTES,
        });
    }
    let bytes = fs::read(&path)?;
    Ok(Download {
        content_type: content_type_for(&target.display),
        path: target.display,
        name,
        bytes,
    })
}

/// Build a zip of one directory of the tree, used to download a whole folder.
///
/// Sockets, FIFOs and devices are skipped, as are symlinks that escape the
/// data directory or point outside the archive; a relative link that stays
/// inside is stored as a link entry. The uncompressed total is capped at
/// [`MAX_ARCHIVE_BYTES`]; the archive itself is built in memory.
pub fn archive(cfg: &Config, id: &str, raw: &str) -> Result<Archive, FileError> {
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    let target = locate(cfg, id, raw)?;
    let (directory, meta) = follow(&target)?;
    if !meta.is_dir() {
        return Err(FileError::NotADirectory(format!(
            "\"{}\" is not a directory",
            target.display
        )));
    }

    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut writer = ZipWriter::new(io::Cursor::new(Vec::new()));
    let mut budget = MAX_ARCHIVE_BYTES;
    let mut entries = 0usize;
    archive_children(
        &mut writer,
        &directory,
        &target.root,
        Path::new(""),
        options,
        &mut budget,
        &mut entries,
    )?;
    let cursor = writer.finish().map_err(zip_error)?;
    Ok(Archive {
        name: target.name,
        entries,
        bytes: cursor.into_inner(),
    })
}

fn archive_children(
    writer: &mut zip::ZipWriter<io::Cursor<Vec<u8>>>,
    dir: &Path,
    root: &Path,
    prefix: &Path,
    options: zip::write::SimpleFileOptions,
    budget: &mut u64,
    entries: &mut usize,
) -> Result<(), FileError> {
    let mut children = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        let name = entry.file_name();
        let path = entry.path();
        let relative = prefix.join(&name);
        let relative = relative.to_string_lossy().replace('\\', "/");
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let file_type = meta.file_type();
        if file_type.is_dir() {
            writer
                .add_directory(&relative, options.unix_permissions(0o755))
                .map_err(zip_error)?;
            *entries += 1;
            archive_children(
                writer,
                &path,
                root,
                Path::new(&relative),
                options,
                budget,
                entries,
            )?;
            continue;
        }
        if file_type.is_symlink() {
            let Ok(resolved) = fs::canonicalize(&path) else {
                continue;
            };
            if !resolved.starts_with(root) {
                continue;
            }
            let Ok(link) = fs::read_link(&path) else {
                continue;
            };
            if !crate::archive::safe_link_target(&link) {
                continue;
            }
            writer
                .add_symlink(
                    &relative,
                    link.to_string_lossy().replace('\\', "/"),
                    options,
                )
                .map_err(zip_error)?;
            *entries += 1;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if meta.len() > *budget {
            let total = MAX_ARCHIVE_BYTES - *budget + meta.len();
            return Err(FileError::TooLarge {
                size: total,
                limit: MAX_ARCHIVE_BYTES,
            });
        }
        *budget -= meta.len();
        let mut file = fs::File::open(&path)?;
        writer.start_file(&relative, options).map_err(zip_error)?;
        io::copy(&mut file, writer)?;
        *entries += 1;
    }
    Ok(())
}

fn zip_error(err: zip::result::ZipError) -> FileError {
    FileError::Io(io::Error::other(err))
}

/// One search pattern: a literal needle or a compiled regular expression.
enum Matcher {
    Literal {
        needle: String,
        case_sensitive: bool,
    },
    Regex(regex::Regex),
}

impl Matcher {
    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Literal {
                needle,
                case_sensitive,
            } => {
                if *case_sensitive {
                    line.contains(needle)
                } else {
                    line.to_lowercase().contains(needle)
                }
            }
            Self::Regex(regex) => regex.is_match(line),
        }
    }
}

/// Whether a file is binary: a NUL byte in the first 8 KiB, or invalid UTF-8.
fn is_binary(bytes: &[u8]) -> bool {
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return true;
    }
    std::str::from_utf8(bytes).is_err()
}

/// Trim and clamp one line for the `preview` field of a hit.
fn preview_line(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() <= MAX_PREVIEW_CHARS {
        return line.to_string();
    }
    let mut preview: String = line.chars().take(MAX_PREVIEW_CHARS).collect();
    preview.push('…');
    preview
}

/// Full-text search over one directory of the tree.
///
/// Files above [`MAX_SEARCH_FILE_BYTES`], binary files and special files are
/// skipped; symlinks are never followed. The scan stops after `limit` hits or
/// [`MAX_SEARCH_FILES`] files, and `truncated` tells the client it did.
pub fn search(
    cfg: &Config,
    id: &str,
    raw_dir: &str,
    query: &str,
    regex: bool,
    case_sensitive: bool,
    limit: usize,
) -> Result<SearchResult, FileError> {
    if query.is_empty() {
        return Err(FileError::InvalidQuery(
            "the q parameter must not be empty".to_string(),
        ));
    }
    let matcher = if regex {
        // The group keeps `(?i)` scoped to the whole pattern, so an
        // alternation (`a|b`) is case-insensitive on both branches.
        let source = if case_sensitive {
            query.to_string()
        } else {
            format!("(?i)(?:{query})")
        };
        Matcher::Regex(regex::Regex::new(&source).map_err(|err| {
            FileError::InvalidRegex(format!("q is not a valid regular expression: {err}"))
        })?)
    } else {
        Matcher::Literal {
            needle: if case_sensitive {
                query.to_string()
            } else {
                query.to_lowercase()
            },
            case_sensitive,
        }
    };

    let target = locate(cfg, id, raw_dir)?;
    let (directory, meta) = follow(&target)?;
    if !meta.is_dir() {
        return Err(FileError::NotADirectory(format!(
            "\"{}\" is not a directory",
            target.display
        )));
    }

    let limit = limit.max(1);
    let mut result = SearchResult {
        results: Vec::new(),
        truncated: false,
        scanned: 0,
    };
    search_children(
        &directory,
        Path::new(""),
        &target.display,
        &matcher,
        limit,
        &mut result,
    )?;
    Ok(result)
}

fn search_children(
    dir: &Path,
    prefix: &Path,
    display: &str,
    matcher: &Matcher,
    limit: usize,
    result: &mut SearchResult,
) -> Result<bool, FileError> {
    let mut children = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        let path = entry.path();
        let relative = prefix.join(entry.file_name());
        let Ok(link) = fs::symlink_metadata(&path) else {
            continue;
        };
        if link.file_type().is_dir() {
            if search_children(&path, &relative, display, matcher, limit, result)? {
                return Ok(true);
            }
            continue;
        }
        // A symlink is never followed: a search must not walk a target twice.
        if !link.file_type().is_file() {
            continue;
        }
        if result.scanned >= MAX_SEARCH_FILES {
            result.truncated = true;
            return Ok(true);
        }
        result.scanned += 1;
        if link.len() > MAX_SEARCH_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        if is_binary(&bytes) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        let virtual_path = if display == "/" {
            format!("/{relative}")
        } else {
            format!("{display}/{relative}")
        };
        for (index, line) in text.split('\n').enumerate() {
            if !matcher.is_match(line) {
                continue;
            }
            result.results.push(SearchHit {
                path: virtual_path.clone(),
                line: index as u32 + 1,
                preview: preview_line(line),
            });
            if result.results.len() >= limit {
                result.truncated = true;
                return Ok(true);
            }
        }
    }
    Ok(false)
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
    fn executables_are_recognised_by_extension_and_magic() {
        for name in ["setup.exe", "RUN.BAT", "libevil.so", "tool.ps1", "boot.sh"] {
            assert!(has_executable_extension(name), "{name}");
        }
        for name in ["notes.md", "config.yaml", "report.exe.txt", "archive.tar"] {
            assert!(!has_executable_extension(name), "{name}");
        }

        assert!(has_executable_magic(b"\x7fELF\x02\x01\x01"));
        assert!(has_executable_magic(&[0xfe, 0xed, 0xfa, 0xcf, 0x00]));
        assert!(has_executable_magic(&[0xca, 0xfe, 0xba, 0xbe, 0x00]));
        let mut pe = vec![0u8; 0x90];
        pe[0..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        assert!(has_executable_magic(&pe));
        // A plain "MZ" prefix without the PE signature is just text.
        assert!(!has_executable_magic(b"MZ is not a binary"));
        assert!(!has_executable_magic(b"hello\n"));
    }

    #[test]
    fn download_content_types_follow_the_extension() {
        assert_eq!(content_type_for("/SOUL.md"), "text/markdown; charset=utf-8");
        assert_eq!(content_type_for("/notes.JSON"), "application/json");
        assert_eq!(content_type_for("/capture.png"), "image/png");
        assert_eq!(content_type_for("/blob"), "application/octet-stream");
    }

    #[test]
    fn previews_are_trimmed_and_clamped() {
        assert_eq!(preview_line("  hello\t"), "hello");
        let long = "x".repeat(MAX_PREVIEW_CHARS + 20);
        let preview = preview_line(&long);
        assert_eq!(preview.chars().count(), MAX_PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
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
