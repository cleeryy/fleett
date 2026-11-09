//! Per-instance git history of the editable files.
//!
//! Every instance owns a repository at `FLEETT_ROOT/instances/<id>/.git`; its
//! work tree is the instance directory, so the paths stored in the repository
//! are the real ones (`data/SOUL.md`, `data/config.yaml`, ...). The `git2`
//! crate (libgit2) is used directly: the `.deb` and the container image need no
//! `git` binary on the host.
//!
//! # Whitelist
//!
//! Only user-editable text is versioned. [`is_tracked`] is the single source of
//! truth and every write goes through it; a `.gitignore` would not be enough,
//! because a path outside the whitelist must never reach the index in the first
//! place:
//!
//! * `data/SOUL.md` and `data/config.yaml` (the rendered config, committed by
//!   the reconcile hook);
//! * `data/skills/**` and every other text file with an editor extension under
//!   `data/` (`.md .yaml .yml .json .txt .toml`);
//! * never `healthcheck.py`, `secrets.env`, `*.db`, `*.key`, sockets, the
//!   timestamped `.bak-*` copies, or the runtime `logs/` and `sessions/`.
//!
//! Secrets never reach a file: they live encrypted in `fleet.db` and the
//! rendered `config.yaml` only carries `${KEY}` placeholders, so the
//! repository cannot leak a token.
//!
//! # Commits
//!
//! * A save in the file browser: author = the authenticated actor
//!   (`name <email>` for an OIDC session, `<actor> <actor@fleett.local>` for a
//!   bearer token), message `ui: <path>`.
//! * A change detected outside the API (startup, after a compose apply):
//!   author `fleett <fleett@localhost>`, message `sync: <path>`.
//!
//! A commit is only created when the resulting tree differs from `HEAD`, so the
//! hooks are cheap enough to run on every compose apply.

use crate::auth::{AuthContext, AuthMethod};
use crate::config::Config;
use crate::files::{self, MAX_CONTENT_BYTES};
use crate::store::{CONTAINER_GID, CONTAINER_UID};
use git2::{Commit, DiffFormat, DiffOptions, ObjectType, Repository, Signature, Sort, Tree};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Directory of the repository inside the instance directory.
pub const GIT_DIR: &str = ".git";
/// Prefix of every versioned path, relative to the instance directory.
pub const DATA_PREFIX: &str = "data/";
/// The rendered system prompt, always versioned.
pub const SOUL_PATH: &str = "data/SOUL.md";
/// The rendered Hermes configuration, always versioned (`${KEY}` only).
pub const CONFIG_PATH: &str = "data/config.yaml";
/// Everything under it is versioned when it is text.
pub const SKILLS_PREFIX: &str = "data/skills/";

/// Directories under `data/` that only hold runtime state.
const EXCLUDED_DIRS: &[&str] = &["logs", "sessions", "node_modules", "__pycache__"];
/// Exact file names that must never be versioned.
const EXCLUDED_NAMES: &[&str] = &["healthcheck.py", "secrets.env"];
/// Suffixes that must never be versioned (databases, sockets, private keys).
const EXCLUDED_SUFFIXES: &[&str] = &[".db", ".db-wal", ".db-shm", ".sock", ".key"];

/// Error of one history operation. The HTTP layer maps every variant.
#[derive(Debug)]
pub enum HistoryError {
    /// Filesystem failure.
    Io(io::Error),
    /// libgit2 failure (corrupted repository, concurrent writer, ...).
    Git(git2::Error),
    /// `from`/`to`/`commit` names nothing in the repository.
    UnknownCommit(String),
    /// A path outside the whitelist was requested.
    UntrackedPath(String),
    /// A path would be written through a symlink or outside the instance.
    Escape(String),
    /// A revert was requested on an instance that has no repository yet.
    NoHistory,
    /// The target exists and is not a regular file.
    NotAFile(String),
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "filesystem error: {err}"),
            Self::Git(err) => write!(f, "git error: {err}"),
            Self::UnknownCommit(rev) => write!(f, "unknown commit \"{rev}\""),
            Self::UntrackedPath(path) => {
                write!(f, "\"{path}\" is outside the versioned whitelist")
            }
            Self::Escape(message) => f.write_str(message),
            Self::NoHistory => f.write_str("this instance has no history yet"),
            Self::NotAFile(path) => write!(f, "\"{path}\" is not a regular file"),
        }
    }
}

impl std::error::Error for HistoryError {}

impl From<io::Error> for HistoryError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<git2::Error> for HistoryError {
    fn from(err: git2::Error) -> Self {
        Self::Git(err)
    }
}

/// Who a commit is attributed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
}

impl Author {
    /// Author of a change fleett detected on its own (`sync:` commits).
    pub fn fleett() -> Self {
        Self {
            name: "fleett".to_string(),
            email: "fleett@localhost".to_string(),
        }
    }

    pub fn new(name: &str, email: &str) -> Self {
        Self {
            name: sanitize_name(name),
            email: sanitize_email(email),
        }
    }
}

/// Normalised commit subject prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitKind {
    /// A save through the console (file API, revert).
    Ui,
    /// A change detected outside the API.
    Sync,
}

impl CommitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Sync => "sync",
        }
    }
}

/// One commit, as returned by `GET .../git/log`.
#[derive(Debug, Clone, Serialize)]
pub struct CommitInfo {
    /// Full hexadecimal object id.
    pub id: String,
    /// First seven characters of [`CommitInfo::id`].
    pub short: String,
    pub author: String,
    pub email: String,
    /// RFC 3339 UTC, second precision.
    pub at: String,
    pub message: String,
    /// Paths changed by the commit against its first parent.
    pub paths: Vec<String>,
}

/// `git status` restricted to the whitelist.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RepoStatus {
    /// Versioned paths whose content differs from `HEAD` (deletions included).
    pub modified: Vec<String>,
    /// Versioned paths present on disk but absent from `HEAD`.
    pub untracked: Vec<String>,
}

/// Unified diff of two commits.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DiffResponse {
    /// Resolved `from` commit, empty when the range starts at the empty tree.
    pub from: String,
    /// Resolved `to` commit, empty when the instance has no history.
    pub to: String,
    pub diff: String,
}

/// Result of one revert.
#[derive(Debug, Clone)]
pub struct Reverted {
    /// Commit the files were restored from.
    pub source: String,
    /// Repository-relative paths actually restored or removed.
    pub paths: Vec<String>,
    /// Commit recording the restore, when it changed the work tree.
    pub commit: Option<CommitInfo>,
}

// ---------------------------------------------------------------------------
// Whitelist
// ---------------------------------------------------------------------------

/// Whether `relative` (a path relative to the instance directory) is versioned.
///
/// This is a syntax-only check: [`tracked_files`] also requires the path to be
/// a regular UTF-8 file no larger than the editor cap.
pub fn is_tracked(relative: &str) -> bool {
    let relative = relative.trim();
    let Some(rest) = relative.strip_prefix(DATA_PREFIX) else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    let segments: Vec<&str> = rest.split('/').collect();
    if segments
        .iter()
        .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        return false;
    }
    // Only the directory part can exclude a whole subtree.
    if segments[..segments.len() - 1]
        .iter()
        .any(|segment| EXCLUDED_DIRS.contains(segment))
    {
        return false;
    }
    let name = segments[segments.len() - 1];
    if EXCLUDED_NAMES.contains(&name) || name.contains(".bak-") {
        return false;
    }
    if EXCLUDED_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return false;
    }
    if rest == "SOUL.md" || rest == "config.yaml" {
        return true;
    }
    if rest.starts_with("skills/") {
        return true;
    }
    has_editor_extension(name)
}

fn has_editor_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        files::EDITABLE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    })
}

/// Virtual file-API path (`/SOUL.md`) or plain path to a repository path.
pub fn to_repo_path(path: &str) -> String {
    let trimmed = path.trim();
    if let Some(rest) = trimmed.strip_prefix('/') {
        return format!("{DATA_PREFIX}{rest}");
    }
    if trimmed.starts_with(DATA_PREFIX) {
        return trimmed.to_string();
    }
    format!("{DATA_PREFIX}{trimmed}")
}

/// Validate a repository-relative path handed to the API.
fn checked_path(raw: &str) -> Result<String, HistoryError> {
    let relative = to_repo_path(raw);
    if relative.contains('\0') || !is_tracked(&relative) {
        return Err(HistoryError::UntrackedPath(raw.trim().to_string()));
    }
    Ok(relative)
}

// ---------------------------------------------------------------------------
// Authors
// ---------------------------------------------------------------------------

/// Author of a request: the OIDC identity for a session, the token name for a
/// bearer, in the `<name> <email>` form git expects.
pub fn author_for(context: &AuthContext) -> Author {
    if context.method == AuthMethod::Session {
        if let Some(user) = &context.user {
            let name = user
                .name
                .clone()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| user.email.clone())
                .or_else(|| (!context.actor.trim().is_empty()).then(|| context.actor.clone()))
                .unwrap_or_else(|| Author::fleett().name);
            let email = user
                .email
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| context.actor.clone());
            return Author::new(&name, &email);
        }
    }
    Author::new(&context.actor, &format!("{}@fleett.local", context.actor))
}

/// Git refuses `<`, `>` and control characters in a signature name.
fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| {
            if ch == '<' || ch == '>' || ch.is_control() {
                ' '
            } else {
                ch
            }
        })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        "fleett".to_string()
    } else {
        cleaned
    }
}

/// Keep the local part and the domain of an email signature-safe.
fn sanitize_email(email: &str) -> String {
    let email = email.trim();
    let (local, domain) = match email.rsplit_once('@') {
        Some((local, domain)) if !local.is_empty() && !domain.is_empty() => {
            (clean_email_part(local), clean_email_part(domain))
        }
        _ => (clean_email_part(email), "fleett.local".to_string()),
    };
    let local = if local.is_empty() {
        "fleett".to_string()
    } else {
        local
    };
    let domain = if domain.is_empty() {
        "fleett.local".to_string()
    } else {
        domain
    };
    format!("{local}@{domain}")
}

fn clean_email_part(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '+' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    cleaned.trim_matches('-').to_string()
}

// ---------------------------------------------------------------------------
// Repository lifecycle
// ---------------------------------------------------------------------------

/// Existing repository of an instance, without creating one.
fn open_existing(cfg: &Config, id: &str) -> Result<Option<Repository>, HistoryError> {
    let workdir = cfg.instance_dir(id);
    if !workdir.join(GIT_DIR).is_dir() {
        return Ok(None);
    }
    Ok(Some(Repository::open(&workdir)?))
}

/// Existing repository, initialised on first use.
fn open_or_init(cfg: &Config, id: &str) -> Result<Repository, HistoryError> {
    if let Some(repo) = open_existing(cfg, id)? {
        return Ok(repo);
    }
    let workdir = cfg.instance_dir(id);
    fs::create_dir_all(&workdir)?;
    let repo = Repository::init(&workdir)?;
    if repo.head().is_err() {
        // Deterministic first branch, whatever the host global config says.
        let _ = repo.set_head("refs/heads/main");
    }
    Ok(repo)
}

fn head_commit(repo: &Repository) -> Result<Option<Commit<'_>>, HistoryError> {
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

/// Resolve `HEAD`, a full/short object id or a reference name.
fn resolve_commit<'repo>(
    repo: &'repo Repository,
    revision: &str,
) -> Result<Commit<'repo>, HistoryError> {
    let revision = revision.trim();
    if revision.is_empty() {
        return Err(HistoryError::UnknownCommit(revision.to_string()));
    }
    repo.revparse_single(revision)
        .and_then(|object| object.peel_to_commit())
        .map_err(|_| HistoryError::UnknownCommit(revision.to_string()))
}

// ---------------------------------------------------------------------------
// Working tree
// ---------------------------------------------------------------------------

/// Every whitelisted text file currently on disk, sorted.
pub fn tracked_files(cfg: &Config, id: &str) -> Vec<String> {
    let mut paths = Vec::new();
    walk_data(
        &cfg.data_dir(id),
        DATA_PREFIX.trim_end_matches('/'),
        &mut paths,
    );
    paths.sort();
    paths
}

fn walk_data(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let relative = format!("{prefix}/{name}");
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            // A symlink is not a text file, and following it could leave the
            // data directory entirely.
            continue;
        }
        if meta.is_dir() {
            if !EXCLUDED_DIRS.contains(&name.as_str()) {
                walk_data(&path, &relative, out);
            }
            continue;
        }
        if !meta.is_file() || !is_tracked(&relative) || !is_text(&path, meta.len()) {
            continue;
        }
        out.push(relative);
    }
}

fn is_text(path: &Path, size: u64) -> bool {
    if size > MAX_CONTENT_BYTES {
        return false;
    }
    match fs::read(path) {
        Ok(bytes) => !bytes.contains(&0) && String::from_utf8(bytes).is_ok(),
        Err(_) => false,
    }
}

/// Whitelisted paths with their current bytes, for the status comparison.
fn tracked_contents(cfg: &Config, id: &str) -> Vec<(String, Vec<u8>)> {
    tracked_files(cfg, id)
        .into_iter()
        .filter_map(|relative| {
            let path = cfg.instance_dir(id).join(&relative);
            fs::read(path).ok().map(|bytes| (relative, bytes))
        })
        .collect()
}

/// Contents of every blob reachable from `HEAD`.
fn head_contents(repo: &Repository) -> Result<BTreeMap<String, Vec<u8>>, HistoryError> {
    let mut out = BTreeMap::new();
    let Some(commit) = head_commit(repo)? else {
        return Ok(out);
    };
    let tree = commit.tree()?;
    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            let path = format!("{root}{}", entry.name().unwrap_or_default());
            if let Ok(blob) = repo.find_blob(entry.id()) {
                out.insert(path, blob.content().to_vec());
            }
        }
        git2::TreeWalkResult::Ok
    })?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Commits
// ---------------------------------------------------------------------------

/// Commit status: what changed against `HEAD`, limited to the whitelist.
pub fn status(cfg: &Config, id: &str) -> Result<RepoStatus, HistoryError> {
    let head = match open_existing(cfg, id)? {
        Some(repo) => head_contents(&repo)?,
        None => BTreeMap::new(),
    };
    let disk = tracked_contents(cfg, id);
    let disk_paths: BTreeSet<&str> = disk.iter().map(|(path, _)| path.as_str()).collect();

    let mut modified = Vec::new();
    for (path, bytes) in &disk {
        match head.get(path) {
            Some(committed) if committed == bytes => {}
            Some(_) => modified.push(path.clone()),
            None => {}
        }
    }
    for path in head.keys() {
        if !disk_paths.contains(path.as_str()) {
            modified.push(path.clone());
        }
    }
    let mut untracked: Vec<String> = disk
        .iter()
        .filter(|(path, _)| !head.contains_key(path))
        .map(|(path, _)| path.clone())
        .collect();
    modified.sort();
    untracked.sort();
    Ok(RepoStatus {
        modified,
        untracked,
    })
}

/// `git log`, newest first, optionally restricted to one path.
pub fn log(
    cfg: &Config,
    id: &str,
    path: Option<&str>,
    limit: usize,
) -> Result<Vec<CommitInfo>, HistoryError> {
    let Some(repo) = open_existing(cfg, id)? else {
        return Ok(Vec::new());
    };
    let Some(head) = head_commit(&repo)? else {
        return Ok(Vec::new());
    };
    let filter = match path.map(str::trim).filter(|value| !value.is_empty()) {
        Some(path) => Some(checked_path(path)?),
        None => None,
    };

    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    walk.push(head.id())?;

    let mut commits = Vec::new();
    for oid in walk {
        let commit = repo.find_commit(oid?)?;
        let paths = commit_paths(&repo, &commit)?;
        if let Some(filter) = &filter {
            if !paths.iter().any(|path| path == filter) {
                continue;
            }
        }
        commits.push(commit_info(&commit, paths));
        if commits.len() >= limit {
            break;
        }
    }
    Ok(commits)
}

/// Unified diff between two commits.
///
/// `to` defaults to `HEAD`; `from` defaults to the first parent of `to` (the
/// empty tree for a root commit), which makes the call without arguments report
/// what the last commit changed.
pub fn diff(
    cfg: &Config,
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<DiffResponse, HistoryError> {
    let Some(repo) = open_existing(cfg, id)? else {
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
    let text = render_diff(&repo, from_tree.as_ref(), &to_tree)?;
    Ok(DiffResponse {
        from: from_commit
            .map(|commit| commit.id().to_string())
            .unwrap_or_default(),
        to: to_commit.id().to_string(),
        diff: text,
    })
}

/// Stage every whitelisted file and commit when the tree changed.
///
/// The index is rebuilt from the whitelist on every call, so the repository
/// can never accumulate a path outside it, and a file deleted outside the API
/// is recorded as a deletion.
pub fn commit_changes(
    cfg: &Config,
    id: &str,
    author: &Author,
    kind: CommitKind,
) -> Result<Option<CommitInfo>, HistoryError> {
    let repo = open_or_init(cfg, id)?;
    let files = tracked_files(cfg, id);

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
    let message = commit_message(kind, &changed);

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
    let at = commit_time(&repo.find_commit(oid)?);
    Ok(Some(CommitInfo {
        short: short_id(&id),
        id,
        author: author.name.clone(),
        email: author.email.clone(),
        at,
        message,
        paths: changed,
    }))
}

/// `ui: data/SOUL.md`, or `ui: data/SOUL.md (+2 more)` for a multi-file change.
fn commit_message(kind: CommitKind, changed: &[String]) -> String {
    let prefix = kind.as_str();
    match changed {
        [] => format!("{prefix}: no change"),
        [only] => format!("{prefix}: {only}"),
        [first, rest @ ..] => format!("{prefix}: {first} (+{} more)", rest.len()),
    }
}

/// Record one file-API write, best effort.
///
/// The write already happened when this runs: a history failure is logged and
/// never turned into an API error (the file is on disk either way).
pub fn record_ui_write(cfg: &Config, id: &str, context: &AuthContext, virtual_path: &str) {
    let relative = to_repo_path(virtual_path);
    if !is_tracked(&relative) {
        return;
    }
    let author = author_for(context);
    if let Err(err) = commit_changes(cfg, id, &author, CommitKind::Ui) {
        tracing::warn!(instance = %id, %err, "could not record the write in the instance history");
    }
}

/// Reconcile one instance after a change made outside the API.
pub fn reconcile(cfg: &Config, id: &str) -> Result<Option<CommitInfo>, HistoryError> {
    if !cfg.data_dir(id).is_dir() {
        return Ok(None);
    }
    commit_changes(cfg, id, &Author::fleett(), CommitKind::Sync)
}

/// Reconcile every instance directory found on disk (startup hook).
///
/// Errors are logged per instance: an unreadable repository must not prevent
/// the control plane from starting.
pub fn reconcile_all(cfg: &Config) {
    let Ok(entries) = fs::read_dir(cfg.instances_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if crate::models::validate_instance_id(&id).is_err() {
            continue;
        }
        match reconcile(cfg, &id) {
            Ok(Some(commit)) => {
                tracing::debug!(instance = %id, commit = %commit.short, "recorded changes made outside the API")
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(instance = %id, %err, "could not reconcile the instance history")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Revert
// ---------------------------------------------------------------------------

/// Restore the files of one commit into the work tree, then record the restore.
///
/// A path present in the commit is written back (the previous bytes are copied
/// to a `.bak-YYYYMMDD-HHMMSS` sibling); a path deleted since the commit is
/// removed. With `paths` omitted, every path the commit itself changed is
/// restored.
pub fn revert(
    cfg: &Config,
    id: &str,
    revision: &str,
    paths: Option<&[String]>,
    author: &Author,
) -> Result<Reverted, HistoryError> {
    let Some(repo) = open_existing(cfg, id)? else {
        return Err(HistoryError::NoHistory);
    };
    let target = resolve_commit(&repo, revision)?;
    let tree = target.tree()?;
    let source = target.id().to_string();

    let requested = match paths {
        Some(list) if !list.is_empty() => {
            let mut seen = BTreeSet::new();
            for path in list {
                seen.insert(checked_path(path)?);
            }
            seen.into_iter().collect::<Vec<_>>()
        }
        _ => commit_paths(&repo, &target)?
            .into_iter()
            .filter(|path| is_tracked(path))
            .collect(),
    };

    let mut restored = Vec::new();
    for relative in requested {
        let blob = tree
            .get_path(Path::new(&relative))
            .ok()
            .map(|entry| entry.to_object(&repo))
            .transpose()?
            .and_then(|object| object.into_blob().ok())
            .map(|blob| blob.content().to_vec());
        if restore_file(cfg, id, &relative, blob.as_deref())? {
            restored.push(relative);
        }
    }
    restored.sort();

    let commit = commit_changes(cfg, id, author, CommitKind::Ui)?;
    Ok(Reverted {
        source,
        paths: restored,
        commit,
    })
}

/// Write (or delete) one file, refusing symlinks on the way.
fn restore_file(
    cfg: &Config,
    id: &str,
    relative: &str,
    blob: Option<&[u8]>,
) -> Result<bool, HistoryError> {
    let target = safe_target(cfg, id, relative)?;
    let existing = match fs::symlink_metadata(&target) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(HistoryError::Escape(format!(
                    "\"{relative}\" is a symlink; the history never restores through one"
                )));
            }
            if meta.is_dir() {
                return Err(HistoryError::NotAFile(relative.to_string()));
            }
            Some(meta)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };

    match blob {
        Some(bytes) => {
            let mode = existing
                .as_ref()
                .map(|meta| meta.permissions().mode() & 0o7777)
                .unwrap_or(0o600);
            if existing.is_some() {
                write_backup(&target)?;
            } else if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&target)?;
            file.write_all(bytes)?;
            file.flush()?;
            drop(file);
            let _ = fs::set_permissions(&target, fs::Permissions::from_mode(mode));
            chown_container(&target);
            Ok(true)
        }
        None => {
            if existing.is_none() {
                return Ok(false);
            }
            write_backup(&target)?;
            fs::remove_file(&target)?;
            Ok(true)
        }
    }
}

/// Timestamped `.bak-*` copy of the bytes about to be replaced.
fn write_backup(path: &Path) -> Result<(), HistoryError> {
    let backup = files::backup_path(path);
    fs::copy(path, backup)?;
    Ok(())
}

/// Repository path resolved inside the instance directory, component by
/// component, refusing any symlink on the way.
fn safe_target(cfg: &Config, id: &str, relative: &str) -> Result<PathBuf, HistoryError> {
    let mut path = cfg.instance_dir(id);
    for segment in relative.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(HistoryError::UntrackedPath(relative.to_string()));
        }
        path.push(segment);
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                return Err(HistoryError::Escape(format!(
                    "\"{relative}\" goes through a symlink"
                )));
            }
        }
    }
    Ok(path)
}

fn chown_container(path: &Path) {
    if let Err(err) = std::os::unix::fs::chown(path, Some(CONTAINER_UID), Some(CONTAINER_GID)) {
        if err.kind() != io::ErrorKind::PermissionDenied {
            tracing::debug!(path = %path.display(), %err, "could not change file ownership");
        }
    }
}

// ---------------------------------------------------------------------------
// libgit2 helpers
// ---------------------------------------------------------------------------

/// Paths changed by `commit` against its first parent.
fn commit_paths(repo: &Repository, commit: &Commit<'_>) -> Result<Vec<String>, HistoryError> {
    let tree = commit.tree()?;
    let parent = commit
        .parent(0)
        .ok()
        .map(|parent| parent.tree())
        .transpose()?;
    diff_paths(repo, parent.as_ref(), &tree)
}

/// Paths of the deltas between two trees (the empty tree for `None`).
fn diff_paths(
    repo: &Repository,
    from: Option<&Tree<'_>>,
    to: &Tree<'_>,
) -> Result<Vec<String>, HistoryError> {
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

/// Standard unified patch text.
fn render_diff(
    repo: &Repository,
    from: Option<&Tree<'_>>,
    to: &Tree<'_>,
) -> Result<String, HistoryError> {
    let mut options = DiffOptions::new();
    options.include_typechange(true);
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
    use crate::auth::{AuthContext, AuthMethod, Role, SessionUser};
    use crate::models::now_iso;

    fn context(method: AuthMethod) -> AuthContext {
        AuthContext {
            actor: "cleeryy@example.test".to_string(),
            role: Role::Admin,
            method,
            user: None,
            ip: None,
        }
    }

    fn session() -> AuthContext {
        AuthContext {
            actor: "cleeryy@example.test".to_string(),
            role: Role::Admin,
            method: AuthMethod::Session,
            user: Some(SessionUser {
                sub: "d80c090d".to_string(),
                email: Some("cleeryy@example.test".to_string()),
                name: Some("Cleeryy".to_string()),
            }),
            ip: None,
        }
    }

    fn temp_config() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        (dir, cfg)
    }

    fn spec(id: &str) -> crate::models::InstanceSpec {
        let now = now_iso();
        crate::models::InstanceSpec {
            id: id.to_string(),
            name: id.to_string(),
            template: "guest".to_string(),
            model: "deepseek-v4.1-flash".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: "HERMES_GATE_PROFILE_TOKEN".to_string(),
            provider: None,
            channel: crate::models::ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: 1536,
            cpu_limit: 1.0,
            image: "agent:1".to_string(),
            previous_image: None,
            allow_lan: false,
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            notes: String::new(),
            status: crate::models::Status::Running,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    fn seed(cfg: &Config, id: &str) {
        crate::store::write_instance(
            cfg,
            &spec(id),
            &crate::render::RenderedInstance {
                compose_yaml: format!("name: fleett-{id}\nservices: {{}}\n"),
                config_yaml: "model: {}\n".to_string(),
                soul_md: Some("You are a test agent.\n".to_string()),
            },
            true,
        )
        .expect("write instance");
    }

    #[test]
    fn whitelist_covers_user_files_only() {
        assert!(is_tracked("data/SOUL.md"));
        assert!(is_tracked("data/config.yaml"));
        assert!(is_tracked("data/skills/research/SKILL.md"));
        assert!(is_tracked("data/skills/notes"));
        assert!(is_tracked("data/profiles/main.yaml"));
        assert!(is_tracked("data/notes/todo.toml"));

        assert!(!is_tracked("data/healthcheck.py"));
        assert!(!is_tracked("data/secrets.env"));
        assert!(!is_tracked("data/fleet.db"));
        assert!(!is_tracked("data/fleet.db-wal"));
        assert!(!is_tracked("data/gateway.sock"));
        assert!(!is_tracked("data/id_ed25519.key"));
        assert!(!is_tracked("data/SOUL.md.bak-20261109-120000"));
        assert!(!is_tracked("data/logs/run.log"));
        assert!(!is_tracked("data/logs/2026/run.md"));
        assert!(!is_tracked("data/sessions/one.json"));
        assert!(!is_tracked("compose.yaml"));
        assert!(!is_tracked("secrets.env"));
        assert!(!is_tracked("data/../fleet.db"));
        assert!(!is_tracked("data//SOUL.md"));
    }

    #[test]
    fn virtual_paths_are_mapped_to_repository_paths() {
        assert_eq!(to_repo_path("/SOUL.md"), "data/SOUL.md");
        assert_eq!(to_repo_path("/skills/a.md"), "data/skills/a.md");
        assert_eq!(to_repo_path("data/SOUL.md"), "data/SOUL.md");
        assert_eq!(to_repo_path("SOUL.md"), "data/SOUL.md");
        assert!(matches!(
            checked_path("/../fleet.db"),
            Err(HistoryError::UntrackedPath(_))
        ));
    }

    #[test]
    fn sessions_are_attributed_to_the_sso_identity() {
        let author = author_for(&session());
        assert_eq!(author.name, "Cleeryy");
        assert_eq!(author.email, "cleeryy@example.test");
    }

    #[test]
    fn bearer_tokens_are_attributed_to_the_token_name() {
        let mut ctx = context(AuthMethod::Bearer);
        ctx.actor = "ci runner".to_string();
        let author = author_for(&ctx);
        assert_eq!(author.name, "ci runner");
        assert_eq!(author.email, "ci-runner@fleett.local");

        let fleett = Author::fleett();
        assert_eq!(fleett.name, "fleett");
        assert_eq!(fleett.email, "fleett@localhost");
    }

    #[test]
    fn commit_messages_say_what_changed() {
        assert_eq!(
            commit_message(CommitKind::Ui, &["data/SOUL.md".to_string()]),
            "ui: data/SOUL.md"
        );
        assert_eq!(
            commit_message(CommitKind::Sync, &["data/SOUL.md".to_string()]),
            "sync: data/SOUL.md"
        );
        assert_eq!(
            commit_message(
                CommitKind::Sync,
                &["data/SOUL.md".to_string(), "data/config.yaml".to_string()]
            ),
            "sync: data/SOUL.md (+1 more)"
        );
        assert_eq!(commit_message(CommitKind::Ui, &[]), "ui: no change");
    }

    #[test]
    fn one_commit_per_change_and_only_whitelisted_paths() {
        let (_dir, cfg) = temp_config();
        seed(&cfg, "alice");
        let first = commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync)
            .expect("first commit")
            .expect("changes");
        assert_eq!(first.paths, vec!["data/SOUL.md", "data/config.yaml"]);
        assert_eq!(
            first.message, "sync: data/SOUL.md (+1 more)",
            "sorted paths, first one named"
        );

        // Nothing changed: no commit.
        assert!(
            commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync)
                .expect("no-op")
                .is_none()
        );

        // A second edit is a second commit.
        fs::write(cfg.data_dir("alice").join("SOUL.md"), "second\n").expect("write");
        let second = commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync)
            .expect("second commit")
            .expect("changes");
        assert_eq!(second.paths, vec!["data/SOUL.md"]);

        // A file removed outside the API is a deletion commit.
        fs::remove_file(cfg.data_dir("alice").join("SOUL.md")).expect("remove");
        let third = commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync)
            .expect("third commit")
            .expect("changes");
        assert_eq!(third.paths, vec!["data/SOUL.md"]);

        // No tracked path ever reaches the index.
        let repo = open_existing(&cfg, "alice").expect("repo").expect("repo");
        let tree = repo.head().expect("head").peel_to_tree().expect("tree");
        let mut paths = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
            if entry.kind() == Some(ObjectType::Blob) {
                paths.push(format!("{root}{}", entry.name().unwrap_or_default()));
            }
            git2::TreeWalkResult::Ok
        })
        .expect("walk");
        for path in &paths {
            assert!(is_tracked(path), "{path} must not be tracked");
        }
    }

    #[test]
    fn log_and_status_follow_the_work_tree() {
        let (_dir, cfg) = temp_config();
        seed(&cfg, "alice");
        commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync).expect("commit");
        assert!(status(&cfg, "alice").expect("status").modified.is_empty());

        fs::write(cfg.data_dir("alice").join("SOUL.md"), "edited\n").expect("write");
        fs::write(cfg.data_dir("alice").join("notes.md"), "notes\n").expect("write");
        let status = status(&cfg, "alice").expect("status");
        assert_eq!(status.modified, vec!["data/SOUL.md"]);
        assert_eq!(status.untracked, vec!["data/notes.md"]);

        commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync).expect("commit");
        let commits = log(&cfg, "alice", Some("/SOUL.md"), 10).expect("log");
        assert_eq!(commits.len(), 2, "the first snapshot and the edit");
        assert_eq!(commits[0].paths, vec!["data/SOUL.md", "data/notes.md"]);
        assert_eq!(commits[0].message, "sync: data/SOUL.md (+1 more)");
        assert_eq!(commits[0].author, "fleett");
        assert_eq!(commits[0].email, "fleett@localhost");
        assert!(commits[0].at.ends_with('Z'));
        assert_eq!(commits[0].short.len(), 7);
        assert_eq!(
            log(&cfg, "alice", Some("data/notes.md"), 10)
                .expect("log")
                .len(),
            1
        );
    }

    #[test]
    fn diff_reports_the_last_commit() {
        let (_dir, cfg) = temp_config();
        seed(&cfg, "alice");
        commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync).expect("commit");
        fs::write(cfg.data_dir("alice").join("SOUL.md"), "new soul\n").expect("write");
        commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync).expect("commit");

        let last = diff(&cfg, "alice", None, None).expect("diff");
        assert!(last.to.len() == 40, "{}", last.to);
        assert_eq!(last.from.len(), 40);
        assert!(
            last.diff.contains("-You are a test agent."),
            "{}",
            last.diff
        );
        assert!(last.diff.contains("+new soul"), "{}", last.diff);
        assert!(
            last.diff.starts_with("diff --git "),
            "a patch starts with the file header: {}",
            last.diff
        );
        assert!(last.diff.contains("@@ "), "{}", last.diff);

        let full = diff(&cfg, "alice", Some(&last.from), Some(&last.to)).expect("range");
        assert_eq!(full.diff, last.diff);
        assert!(matches!(
            diff(&cfg, "alice", Some("deadbeef"), None),
            Err(HistoryError::UnknownCommit(_))
        ));
    }

    #[test]
    fn revert_restores_the_content_and_deletes_a_later_file() {
        let (_dir, cfg) = temp_config();
        seed(&cfg, "alice");
        let first = commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync)
            .expect("commit")
            .expect("changes");
        fs::write(cfg.data_dir("alice").join("SOUL.md"), "broken soul\n").expect("write");
        fs::write(cfg.data_dir("alice").join("later.md"), "later\n").expect("write");
        commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync).expect("commit");

        let author = Author::new("Cleeryy", "cleeryy@example.test");
        let reverted = revert(
            &cfg,
            "alice",
            &first.id,
            Some(&["/SOUL.md".to_string(), "/later.md".to_string()]),
            &author,
        )
        .expect("revert");
        assert_eq!(reverted.paths, vec!["data/SOUL.md", "data/later.md"]);
        assert_eq!(
            fs::read_to_string(cfg.data_dir("alice").join("SOUL.md")).expect("soul"),
            "You are a test agent.\n"
        );
        assert!(!cfg.data_dir("alice").join("later.md").exists());
        // The replaced copy is kept next to the file.
        let backups: Vec<String> = fs::read_dir(cfg.data_dir("alice"))
            .expect("data")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with("SOUL.md.bak-"))
            .collect();
        assert_eq!(backups.len(), 1, "{backups:?}");

        let commit = reverted.commit.expect("the restore is committed");
        assert_eq!(commit.author, "Cleeryy");
        assert_eq!(commit.email, "cleeryy@example.test");
        assert!(commit.message.starts_with("ui: "), "{}", commit.message);
    }

    #[test]
    fn history_survives_a_missing_work_tree_file() {
        let (_dir, cfg) = temp_config();
        seed(&cfg, "alice");
        let first = commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync)
            .expect("commit")
            .expect("changes");
        fs::remove_file(cfg.data_dir("alice").join("SOUL.md")).expect("remove");

        // Reading the history never touches the container or the data tree.
        let commits = log(&cfg, "alice", None, 10).expect("log");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].id, first.id);

        let reverted = revert(&cfg, "alice", &first.id, None, &Author::fleett()).expect("revert");
        assert!(reverted.paths.contains(&"data/SOUL.md".to_string()));
        assert!(cfg.data_dir("alice").join("SOUL.md").is_file());
    }

    #[test]
    fn status_of_an_instance_without_a_repository_lists_untracked_files() {
        let (_dir, cfg) = temp_config();
        seed(&cfg, "alice");
        let status = status(&cfg, "alice").expect("status");
        assert!(status.modified.is_empty());
        assert_eq!(status.untracked, vec!["data/SOUL.md", "data/config.yaml"]);
        assert!(log(&cfg, "alice", None, 10).expect("log").is_empty());
        assert!(diff(&cfg, "alice", None, None)
            .expect("diff")
            .diff
            .is_empty());
        assert!(matches!(
            revert(&cfg, "alice", "HEAD", None, &Author::fleett()),
            Err(HistoryError::NoHistory)
        ));
    }

    #[test]
    fn the_repository_never_contains_a_secret() {
        let (_dir, cfg) = temp_config();
        const TOKEN: &str = "123456:SUPER-SECRET-BOT-TOKEN";
        seed(&cfg, "alice");
        crate::store::write_legacy_secrets(
            &cfg,
            "alice",
            &[("TELEGRAM_BOT_TOKEN".to_string(), TOKEN.to_string())]
                .into_iter()
                .collect(),
        )
        .expect("legacy secret");
        fs::write(
            cfg.data_dir("alice").join("healthcheck.py"),
            format!("# {TOKEN}\n"),
        )
        .expect("healthcheck");
        fs::write(
            cfg.data_dir("alice").join("SOUL.md.bak-20261109-000000"),
            TOKEN,
        )
        .expect("backup");
        fs::write(cfg.data_dir("alice").join("state.db"), TOKEN).expect("db");

        commit_changes(&cfg, "alice", &Author::fleett(), CommitKind::Sync).expect("commit");

        let status = status(&cfg, "alice").expect("status");
        assert!(status.untracked.is_empty(), "{status:?}");
        assert!(status.modified.is_empty(), "{status:?}");

        // Every reachable blob is text and carries no token.
        let repo = open_existing(&cfg, "alice").expect("repo").expect("repo");
        let head = head_contents(&repo).expect("head");
        assert!(!head.is_empty());
        for (path, bytes) in &head {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains(TOKEN), "{path} leaks the token");
        }
    }
}
