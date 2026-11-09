//! Skill marketplace catalogue (issue #61).
//!
//! The registry ([`crate::skills`]) is deliberately empty by default: it only
//! knows the skills an operator published or imported from a zip. This module
//! adds the discovery half — a catalogue made of independent *providers* that
//! answer "which skills exist upstream, and where do I fetch them from?".
//!
//! # Providers
//!
//! | id | source of truth |
//! |---|---|
//! | `local` | `$FLEETT_ROOT/marketplace/index.json`, written by [`sync`] |
//! | `git` | `index.json` of `FLEETT_MARKETPLACE_GIT_INDEX=owner/repo`, fetched over HTTPS and cached |
//! | `github` | repository search (`/search/repositories?q=… skill`) |
//! | `skills.sh` | light scrape of `https://skills.sh/search`, behind `FLEETT_MARKETPLACE_SKILLS_SH=auto|off` |
//!
//! A provider that fails only reports `status:"error"` with a message: one
//! broken upstream never fails the whole search.
//!
//! # Sources
//!
//! - `local:<name>` or a bare name → an entry of the local index;
//! - `git:<owner>/<repo>[:<path>]` → a directory of a repository, resolved
//!   through an HTTPS archive (never a network `git clone`);
//! - `github:<owner>/<repo>[:<path>][@<ref>]` → same, with an explicit ref;
//! - `url:<https://…zip|…md>` → a direct archive or a single `SKILL.md`;
//! - `skills.sh:<owner>/<name>` → the upstream repository linked by the page.
//!
//! # Safety
//!
//! A skill is untrusted content. Importing never executes anything, never
//! follows a symlink, never writes outside the version directory, and caps
//! every download (index 1 MiB, archive 8 MiB streamed, 256 files of 256 KiB,
//! 1 MiB total). Scripts are detected and reported before installation.
//! Downloads are HTTPS-only, except loopback or when
//! `FLEETT_MARKETPLACE_INSECURE=1` is set (tests).

use crate::config::Config;
use crate::db::Db;
use crate::history::Author;
use crate::models::{now_iso, validate_skill_name};
use crate::skills::{self, Provenance, SkillDetail, SkillError, SkillFile};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Largest accepted index document.
pub const MAX_INDEX_BYTES: usize = 1024 * 1024;
/// Largest accepted skill archive (streamed and capped while downloading).
pub const MAX_ARCHIVE_BYTES: usize = 8 * 1024 * 1024;
/// Timeout of one marketplace HTTP request.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Redirects followed before the fetch is refused.
pub const MAX_REDIRECTS: usize = 3;
/// Lifetime of the in-memory git index cache.
pub const INDEX_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
/// Lifetime of one scraped skills.sh search.
pub const SKILLS_SH_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// Minimum delay between two requests to skills.sh.
pub const SKILLS_SH_MIN_INTERVAL: Duration = Duration::from_secs(2);
/// Lifetime of the `GET /api/marketplace/updates` answer.
pub const UPDATES_CACHE_TTL: Duration = Duration::from_secs(60);
/// Preview calls allowed per minute and per process.
pub const PREVIEW_PER_MINUTE: usize = 30;
/// Default and maximum `?limit=` of a search.
pub const DEFAULT_SEARCH_LIMIT: usize = 50;
pub const MAX_SEARCH_LIMIT: usize = 200;
/// Most entries accepted from one index document.
pub const MAX_INDEX_ENTRIES: usize = 1000;

/// Extensions treated as executable content by the pre-install warning.
pub const SCRIPT_EXTENSIONS: &[&str] = &[
    "sh", "bash", "zsh", "py", "js", "mjs", "ts", "rb", "pl", "php", "ps1", "bat", "cmd", "exe",
    "jar",
];

#[derive(Debug, thiserror::Error)]
pub enum MarketplaceError {
    #[error("invalid marketplace source: {0}")]
    InvalidSource(String),
    #[error("the marketplace index is not configured (set FLEETT_MARKETPLACE_GIT_INDEX)")]
    NoIndexConfigured,
    #[error("unknown marketplace skill \"{0}\"")]
    NotFound(String),
    #[error("skill \"{0}\" has no upstream provenance")]
    NoUpstream(String),
    #[error("the upstream content of \"{0}\" has not changed")]
    Unchanged(String),
    #[error("skill scripts require confirmation")]
    ScriptsRequireConfirmation(Vec<String>),
    #[error("marketplace fetch failed: {0}")]
    Fetch(String),
    #[error("invalid marketplace response: {0}")]
    Invalid(String),
    #[error("too many marketplace requests")]
    RateLimited,
    #[error("marketplace I/O error: {0}")]
    Io(String),
    #[error(transparent)]
    Skill(#[from] SkillError),
}

impl MarketplaceError {
    /// Stable machine-readable code returned by the API.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidSource(_) => "invalid_source",
            Self::NoIndexConfigured => "no_index_configured",
            Self::NotFound(_) => "not_found",
            Self::NoUpstream(_) => "no_upstream",
            Self::Unchanged(_) => "upstream_unchanged",
            Self::ScriptsRequireConfirmation(_) => "scripts_require_confirmation",
            Self::Fetch(_) => "marketplace_unavailable",
            Self::Invalid(_) => "invalid_marketplace_response",
            Self::RateLimited => "rate_limited",
            Self::Io(_) => "marketplace_io",
            Self::Skill(_) => "invalid_skill",
        }
    }
}

// ---------------------------------------------------------------------------
// Source grammar
// ---------------------------------------------------------------------------

/// One parsed marketplace source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// An entry of the local index (`local:<name>` or a bare name).
    Local { name: String },
    /// A directory of an `owner/repo` repository, resolved over HTTPS.
    Git { repo: String, path: Option<String> },
    /// Same as [`SourceKind::Git`] with an explicit ref.
    Github {
        repo: String,
        path: Option<String>,
        reference: Option<String>,
    },
    /// A direct archive (`.zip`) or a single `SKILL.md` (`.md`).
    Url { url: String },
    /// A page of skills.sh naming the upstream repository.
    SkillsSh { owner: String, name: String },
}

/// Parse the documented source grammar.
pub fn parse_source(input: &str) -> Result<SourceKind, MarketplaceError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(MarketplaceError::InvalidSource(
            "the source must not be empty".to_string(),
        ));
    }
    if let Some(rest) = input.strip_prefix("local:") {
        let name = rest.trim();
        validate_skill_name(name)
            .map_err(|err| MarketplaceError::InvalidSource(format!("local:{name}: {err}")))?;
        return Ok(SourceKind::Local {
            name: name.to_string(),
        });
    }
    if let Some(rest) = input.strip_prefix("github:") {
        let (repo, path, reference) = parse_repo_ref(rest)?;
        return Ok(SourceKind::Github {
            repo,
            path,
            reference,
        });
    }
    if let Some(rest) = input.strip_prefix("git:") {
        let (repo, path, _) = parse_repo_ref(rest)?;
        return Ok(SourceKind::Git { repo, path });
    }
    if let Some(rest) = input.strip_prefix("url:") {
        let url = rest.trim();
        validate_url(url)?;
        return Ok(SourceKind::Url {
            url: url.to_string(),
        });
    }
    if let Some(rest) = input.strip_prefix("skills.sh:") {
        let mut parts = rest.splitn(2, '/');
        let owner = parts.next().unwrap_or_default().trim();
        let name = parts.next().unwrap_or_default().trim();
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return Err(MarketplaceError::InvalidSource(format!(
                "skills.sh:{rest} must be skills.sh:<owner>/<name>"
            )));
        }
        return Ok(SourceKind::SkillsSh {
            owner: owner.to_string(),
            name: name.to_string(),
        });
    }
    if input.contains(':') || input.contains('/') {
        return Err(MarketplaceError::InvalidSource(format!(
            "\"{input}\" is not a known source; expected local:<name>, \
             git:<owner>/<repo>[:<path>], github:<owner>/<repo>[:<path>][@<ref>], \
             url:<https://…> or skills.sh:<owner>/<name>"
        )));
    }
    // A bare name is a local index lookup.
    validate_skill_name(input)
        .map_err(|err| MarketplaceError::InvalidSource(format!("local:{input}: {err}")))?;
    Ok(SourceKind::Local {
        name: input.to_string(),
    })
}

/// `owner/repo`, an optional `:path` and an optional `@ref`.
fn parse_repo_ref(
    rest: &str,
) -> Result<(String, Option<String>, Option<String>), MarketplaceError> {
    let rest = rest.trim();
    let (rest, reference) = match rest.rsplit_once('@') {
        Some((head, reference)) if !reference.trim().is_empty() => {
            (head, Some(reference.trim().to_string()))
        }
        _ => (rest, None),
    };
    let (repo, path) = match rest.split_once(':') {
        Some((repo, path)) => {
            let path = path.trim().trim_matches('/');
            let path = (!path.is_empty()).then(|| path.to_string());
            if let Some(path) = path.as_deref() {
                skills::validate_relative_path(path).map_err(MarketplaceError::InvalidSource)?;
            }
            (repo.trim(), path)
        }
        None => (rest, None),
    };
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default().trim();
    let name = parts.next().unwrap_or_default().trim();
    if owner.is_empty()
        || name.is_empty()
        || parts.next().is_some()
        || !valid_repo_part(owner)
        || !valid_repo_part(name)
    {
        return Err(MarketplaceError::InvalidSource(format!(
            "\"{repo}\" must be owner/repo"
        )));
    }
    Ok((format!("{owner}/{name}"), path, reference))
}

fn valid_repo_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Accept only HTTPS, plus HTTP for loopback or when insecurity was requested.
fn validate_url(url: &str) -> Result<(), MarketplaceError> {
    let parsed = url::Url::parse(url)
        .map_err(|err| MarketplaceError::InvalidSource(format!("\"{url}\": {err}")))?;
    if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
        return Err(MarketplaceError::InvalidSource(format!(
            "\"{url}\" must be an https URL (http is accepted for loopback only)"
        )));
    }
    Ok(())
}

fn is_loopback(host: &str) -> bool {
    matches!(
        host.trim_matches(['[', ']']),
        "127.0.0.1" | "localhost" | "::1" | "0.0.0.0"
    )
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One entry of `index.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct IndexFile {
    #[serde(default)]
    skills: Vec<IndexEntry>,
}

/// One catalogue search hit.
#[derive(Debug, Clone, Serialize)]
pub struct SearchEntry {
    pub name: String,
    pub description: String,
    /// Provider id that produced the entry (`local`, `git`, `github`, `skills.sh`).
    pub provider: String,
    /// Canonical install source (`github:owner/repo`, `url:…`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// Health of one provider for the current search.
#[derive(Debug, Clone, Serialize)]
pub struct SourceStatus {
    pub source: String,
    pub status: String,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `GET /api/marketplace/skills`.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub entries: Vec<SearchEntry>,
    pub sources: Vec<SourceStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewFile {
    pub path: String,
    pub size: usize,
    pub script: bool,
}

/// `POST /api/marketplace/skills/preview`.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewResponse {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub files: Vec<PreviewFile>,
    pub total_bytes: usize,
    pub skill_md: String,
    pub scripts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub requires_confirmation: bool,
}

/// One file change reported by an update.
#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    pub path: String,
    /// `added`, `modified` or `removed`.
    pub status: &'static str,
}

/// Result of [`Marketplace::update`].
#[derive(Debug, Clone)]
pub struct UpdateOutcome {
    pub detail: SkillDetail,
    pub from: String,
    pub to: String,
    pub commit: Option<String>,
    pub files: Vec<FileChange>,
}

/// One `GET /api/marketplace/updates` entry.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub name: String,
    pub installed_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub source: String,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// `GET /api/marketplace/updates`: an empty list plus warnings on an outage.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdatesResponse {
    pub updates: Vec<UpdateInfo>,
    pub warnings: Vec<String>,
}

/// Result of [`Marketplace::sync`].
#[derive(Debug, Clone, Serialize)]
pub struct SyncOutcome {
    pub skills: usize,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// A resolved upstream skill, ready to publish.
#[derive(Debug, Clone)]
pub struct ResolvedSkill {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub tags: Vec<String>,
    /// Canonical source stored in the provenance.
    pub source: String,
    pub reference: Option<String>,
    pub commit: Option<String>,
    pub files: Vec<SkillFile>,
}

impl ResolvedSkill {
    fn into_preview(self) -> PreviewResponse {
        let scripts = scripts_of(&self.files);
        let files = self
            .files
            .iter()
            .map(|file| PreviewFile {
                path: file.path.clone(),
                size: file.content.len(),
                script: is_script(&file.path, &file.content),
            })
            .collect();
        let skill_md = self
            .files
            .iter()
            .find(|file| file.path == skills::SKILL_FILE)
            .map(|file| file.content.clone())
            .unwrap_or_default();
        PreviewResponse {
            name: self.name,
            description: self.description,
            version: self.version,
            author: self.author,
            files,
            total_bytes: self.files.iter().map(|file| file.content.len()).sum(),
            skill_md,
            requires_confirmation: !scripts.is_empty(),
            scripts,
            commit: self.commit,
            source: Some(self.source),
        }
    }
}

// ---------------------------------------------------------------------------
// Detection helpers
// ---------------------------------------------------------------------------

/// `true` when the file looks executable: a known extension or a shebang.
pub fn is_script(path: &str, content: &str) -> bool {
    let extension = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    if extension
        .as_deref()
        .is_some_and(|extension| SCRIPT_EXTENSIONS.contains(&extension))
    {
        return true;
    }
    content.trim_start_matches('\u{feff}').starts_with("#!")
}

/// Scripts of a file set, sorted by path.
pub fn scripts_of(files: &[SkillFile]) -> Vec<String> {
    let mut scripts: Vec<String> = files
        .iter()
        .filter(|file| is_script(&file.path, &file.content))
        .map(|file| file.path.clone())
        .collect();
    scripts.sort();
    scripts
}

/// Frontmatter fields understood by the catalogue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub tags: Vec<String>,
}

/// Parse the optional `---` fenced YAML block of a `SKILL.md`.
///
/// Tolerant by design: a malformed block returns empty metadata, it never
/// fails the import.
pub fn parse_frontmatter(markdown: &str) -> Frontmatter {
    let text = markdown.trim_start_matches('\u{feff}');
    let Some(rest) = text.strip_prefix("---") else {
        return Frontmatter::default();
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);
    let Some(end) = rest.find("\n---") else {
        return Frontmatter::default();
    };
    let block = &rest[..end];
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(block) else {
        return Frontmatter::default();
    };
    let string = |key: &str| {
        value
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let tags = match value.get("tags") {
        Some(serde_yaml::Value::Sequence(sequence)) => sequence
            .iter()
            .filter_map(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .take(16)
            .collect(),
        Some(serde_yaml::Value::String(text)) => text
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .take(16)
            .collect(),
        _ => Vec::new(),
    };
    Frontmatter {
        name: string("name"),
        description: string("description"),
        version: string("version"),
        author: string("author"),
        tags,
    }
}

/// Turn an upstream title into a valid registry skill name.
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let mut slug = out.trim_matches('-').to_string();
    if slug.len() > 32 {
        slug.truncate(32);
        slug = slug.trim_matches('-').to_string();
    }
    if slug.len() < 2 {
        slug = format!("{slug}-skill")
            .trim_matches('-')
            .chars()
            .take(32)
            .collect();
    }
    slug
}

/// Every file that differs between two versions, sorted by path.
pub fn diff_files(installed: &[SkillFile], upstream: &[SkillFile]) -> Vec<FileChange> {
    let before: HashMap<&str, &str> = installed
        .iter()
        .map(|file| (file.path.as_str(), file.content.as_str()))
        .collect();
    let after: HashMap<&str, &str> = upstream
        .iter()
        .map(|file| (file.path.as_str(), file.content.as_str()))
        .collect();
    let paths: BTreeSet<&str> = before.keys().chain(after.keys()).copied().collect();
    let mut changes = Vec::new();
    for path in paths {
        let change = match (before.get(path), after.get(path)) {
            (None, Some(_)) => Some("added"),
            (Some(_), None) => Some("removed"),
            (Some(old), Some(new)) if old != new => Some("modified"),
            _ => None,
        };
        if let Some(status) = change {
            changes.push(FileChange {
                path: path.to_string(),
                status,
            });
        }
    }
    changes
}

// ---------------------------------------------------------------------------
// Index files
// ---------------------------------------------------------------------------

fn parse_index(bytes: &[u8]) -> Result<Vec<IndexEntry>, MarketplaceError> {
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(MarketplaceError::Invalid(format!(
            "the index must be at most {MAX_INDEX_BYTES} bytes"
        )));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| MarketplaceError::Invalid("the index is not valid UTF-8".to_string()))?;
    let entries = match serde_json::from_str::<IndexFile>(text) {
        Ok(file) => file.skills,
        Err(object_error) => serde_json::from_str::<Vec<IndexEntry>>(text).map_err(|_| {
            MarketplaceError::Invalid(format!(
                "the index must be {{\"skills\":[…]}} or an array: {object_error}"
            ))
        })?,
    };
    if entries.len() > MAX_INDEX_ENTRIES {
        return Err(MarketplaceError::Invalid(format!(
            "the index may list at most {MAX_INDEX_ENTRIES} skills"
        )));
    }
    for entry in &entries {
        if entry.name.trim().is_empty() || entry.name.chars().count() > 128 {
            return Err(MarketplaceError::Invalid(
                "every index entry needs a bounded name".to_string(),
            ));
        }
    }
    Ok(entries)
}

fn read_local_index(cfg: &Config) -> Result<Option<Vec<IndexEntry>>, MarketplaceError> {
    let path = cfg.marketplace_index_path();
    match fs::read(&path) {
        Ok(bytes) => parse_index(&bytes).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(MarketplaceError::Io(format!("{}: {err}", path.display()))),
    }
}

fn write_local_index(cfg: &Config, entries: &[IndexEntry]) -> Result<(), MarketplaceError> {
    let dir = cfg.marketplace_dir();
    fs::create_dir_all(&dir)
        .map_err(|err| MarketplaceError::Io(format!("{}: {err}", dir.display())))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|err| MarketplaceError::Io(format!("{}: {err}", dir.display())))?;
    let document = serde_json::to_vec_pretty(&serde_json::json!({ "skills": entries }))
        .map_err(|err| MarketplaceError::Invalid(format!("cannot serialise the index: {err}")))?;
    atomic_write(&cfg.marketplace_index_path(), &document)
}

/// Write `bytes` next to `path` then rename it, so a reader never sees a
/// half-written index.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), MarketplaceError> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)
        .map_err(|err| MarketplaceError::Io(format!("{}: {err}", temporary.display())))?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
        .map_err(|err| MarketplaceError::Io(format!("{}: {err}", temporary.display())))?;
    fs::rename(&temporary, path)
        .map_err(|err| MarketplaceError::Io(format!("{}: {err}", path.display())))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// HTML scanner
// ---------------------------------------------------------------------------

/// One skill link found on a skills.sh page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapedSkill {
    pub owner: String,
    pub name: String,
    pub description: String,
}

/// Tolerant scanner for the skills.sh search page.
///
/// It only looks for `href="/owner/name"` anchors and their visible text, so a
/// redesign degrades to "no entry" instead of an error. No regex, no new
/// dependency.
pub fn scan_skills_sh(html: &str) -> Vec<ScrapedSkill> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut rest = html;
    while let Some(position) = rest.find("href=") {
        let after = &rest[position + 5..];
        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            rest = after;
            continue;
        };
        let after_quote = &after[1..];
        let Some(end) = after_quote.find(quote) else {
            break;
        };
        let href = &after_quote[..end];
        let tail = &after_quote[end + 1..];
        rest = tail;
        let Some((owner, name)) = parse_skills_sh_href(href) else {
            continue;
        };
        if !seen.insert((owner.clone(), name.clone())) {
            continue;
        }
        let description = anchor_text(tail).unwrap_or_default();
        out.push(ScrapedSkill {
            owner,
            name,
            description,
        });
    }
    out
}

fn parse_skills_sh_href(href: &str) -> Option<(String, String)> {
    let path = href.split(['?', '#']).next().unwrap_or_default();
    let path = path.trim_end_matches('/');
    let path = path.strip_prefix('/').unwrap_or(path);
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() || owner.is_empty() || name.is_empty() {
        return None;
    }
    if !valid_repo_part(owner) || !valid_repo_part(name) {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

/// Visible text of the anchor whose attributes were just parsed.
fn anchor_text(tail: &str) -> Option<String> {
    let start = tail.find('>')? + 1;
    let end = tail[start..].find("</a>")? + start;
    let text = strip_tags(&tail[start..end]);
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for character in html.chars() {
        match character {
            '<' => inside = true,
            '>' => inside = false,
            other if !inside => out.push(other),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// HTTP fetching
// ---------------------------------------------------------------------------

struct ProviderResult {
    source: &'static str,
    status: &'static str,
    entries: Vec<SearchEntry>,
    message: Option<String>,
}

impl ProviderResult {
    fn ok(source: &'static str, entries: Vec<SearchEntry>, message: Option<String>) -> Self {
        Self {
            source,
            status: "ok",
            entries,
            message,
        }
    }

    fn disabled(source: &'static str, message: impl Into<String>) -> Self {
        Self {
            source,
            status: "disabled",
            entries: Vec::new(),
            message: Some(message.into()),
        }
    }

    fn error(source: &'static str, message: impl Into<String>) -> Self {
        Self {
            source,
            status: "error",
            entries: Vec::new(),
            message: Some(message.into()),
        }
    }

    fn status(&self) -> SourceStatus {
        SourceStatus {
            source: self.source.to_string(),
            status: self.status.to_string(),
            count: self.entries.len(),
            message: self.message.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct CachedIndex {
    at: Instant,
    entries: Vec<IndexEntry>,
}

#[derive(Debug, Clone)]
struct CachedScrape {
    at: Instant,
    entries: Vec<SearchEntry>,
    message: Option<String>,
}

#[derive(Default)]
struct Cache {
    git: Option<CachedIndex>,
    skills_sh: HashMap<String, CachedScrape>,
}

#[derive(Debug, Clone)]
struct CachedUpdates {
    at: Instant,
    response: UpdatesResponse,
}

/// The catalogue: HTTP client, caches and rate limits shared by every handler.
pub struct Marketplace {
    http: reqwest::Client,
    cache: Mutex<Cache>,
    skills_sh_gate: Mutex<Option<Instant>>,
    preview_gate: Mutex<VecDeque<Instant>>,
    updates: Mutex<Option<CachedUpdates>>,
}

impl Default for Marketplace {
    fn default() -> Self {
        Self::build(false)
    }
}

impl Marketplace {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the shared HTTP client, enforcing the download origins the
    /// operator allowed at redirect time too (not only on the first URL).
    pub fn from_config(cfg: &Config) -> Self {
        Self::build(cfg.marketplace_insecure)
    }

    fn build(insecure: bool) -> Self {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many redirects");
                }
                let url = attempt.url();
                let host = url.host_str().unwrap_or_default();
                let allowed = url.scheme() == "https"
                    || (url.scheme() == "http" && (insecure || is_loopback(host)));
                if allowed {
                    attempt.follow()
                } else {
                    attempt.error("refusing a redirect to a non-HTTPS origin")
                }
            }))
            .user_agent("fleett-marketplace")
            .build()
            .unwrap_or_default();
        Self {
            http,
            cache: Mutex::new(Cache::default()),
            skills_sh_gate: Mutex::new(None),
            preview_gate: Mutex::new(VecDeque::new()),
            updates: Mutex::new(None),
        }
    }

    /// Drop the cached `updates` answer after a mutation.
    pub fn invalidate(&self) {
        let mut updates = self
            .updates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *updates = None;
    }

    /// Preview budget (an outbound fetch per call): `false` means `429`.
    pub fn allow_preview(&self) -> bool {
        let now = Instant::now();
        let mut events = self
            .preview_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while events
            .front()
            .is_some_and(|first| now.duration_since(*first) > Duration::from_secs(60))
        {
            events.pop_front();
        }
        if events.len() >= PREVIEW_PER_MINUTE {
            return false;
        }
        events.push_back(now);
        true
    }

    /// One GET with a streamed size cap. `api` adds the GitHub headers (and
    /// the token, only to the configured API origin).
    async fn http_get(
        &self,
        cfg: &Config,
        url: &str,
        cap: usize,
        accept: Option<&str>,
        api: bool,
    ) -> Result<Vec<u8>, MarketplaceError> {
        validate_url(url)?;
        let parsed = url::Url::parse(url)
            .map_err(|err| MarketplaceError::InvalidSource(format!("\"{url}\": {err}")))?;
        if parsed.scheme() == "http"
            && !cfg.marketplace_insecure
            && !parsed.host_str().map(is_loopback).unwrap_or(false)
        {
            return Err(MarketplaceError::Fetch(format!(
                "refusing to download over plain HTTP from {url}; \
                 set FLEETT_MARKETPLACE_INSECURE=1 to allow it"
            )));
        }
        let mut request = self.http.get(url);
        if let Some(accept) = accept {
            request = request.header(reqwest::header::ACCEPT, accept);
        }
        if api {
            request = request
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header(reqwest::header::USER_AGENT, "fleett-marketplace");
            if let Some(token) = cfg.github_token.as_deref() {
                // Only ever sent to the configured API origin.
                if url.starts_with(&cfg.marketplace_api_base) {
                    request = request.bearer_auth(token);
                }
            }
        }
        let mut response = request
            .send()
            .await
            .map_err(|err| MarketplaceError::Fetch(format!("{url}: {err}")))?;
        if !response.status().is_success() {
            return Err(MarketplaceError::Fetch(format!(
                "{url}: HTTP {}",
                response.status()
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| MarketplaceError::Fetch(format!("{url}: {err}")))?
        {
            if body.len().saturating_add(chunk.len()) > cap {
                return Err(MarketplaceError::Fetch(format!(
                    "{url}: the response is larger than {cap} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn api_get(
        &self,
        cfg: &Config,
        path: &str,
        cap: usize,
        raw: bool,
    ) -> Result<Vec<u8>, MarketplaceError> {
        let url = format!("{}{}", cfg.marketplace_api_base.trim_end_matches('/'), path);
        let accept = if raw {
            Some("application/vnd.github.raw")
        } else {
            Some("application/vnd.github+json")
        };
        self.http_get(cfg, &url, cap, accept, true).await
    }

    // -----------------------------------------------------------------------
    // Providers
    // -----------------------------------------------------------------------

    fn local_provider(&self, cfg: &Config, query: &str) -> ProviderResult {
        match read_local_index(cfg) {
            Ok(None) => ProviderResult::ok(
                "local",
                Vec::new(),
                Some("no local index yet (run `fleett marketplace sync`)".to_string()),
            ),
            Ok(Some(entries)) => ProviderResult::ok(
                "local",
                entries
                    .into_iter()
                    .filter(|entry| index_entry_matches(entry, query))
                    .map(|entry| index_search_entry(entry, "local"))
                    .collect(),
                None,
            ),
            Err(err) => ProviderResult::error("local", err.to_string()),
        }
    }

    async fn git_provider(&self, cfg: &Config, query: &str) -> ProviderResult {
        let Some(configured) = cfg.marketplace_git_index.as_deref() else {
            return ProviderResult::disabled("git", "FLEETT_MARKETPLACE_GIT_INDEX is not set");
        };
        let repo = match parse_repo_ref(configured) {
            Ok((repo, _, _)) => repo,
            Err(err) => return ProviderResult::error("git", err.to_string()),
        };
        if let Some(cached) = self.cached_git_index() {
            return ProviderResult::ok(
                "git",
                cached
                    .entries
                    .iter()
                    .filter(|entry| index_entry_matches(entry, query))
                    .cloned()
                    .map(|entry| index_search_entry(entry, "git"))
                    .collect(),
                None,
            );
        }
        match self.fetch_git_index(cfg, &repo).await {
            Ok((entries, _commit)) => {
                self.store_git_index(entries.clone());
                ProviderResult::ok(
                    "git",
                    entries
                        .iter()
                        .filter(|entry| index_entry_matches(entry, query))
                        .cloned()
                        .map(|entry| index_search_entry(entry, "git"))
                        .collect(),
                    None,
                )
            }
            Err(err) => ProviderResult::error("git", err.to_string()),
        }
    }

    async fn github_provider(&self, cfg: &Config, query: &str) -> ProviderResult {
        if query.trim().is_empty() {
            return ProviderResult::ok(
                "github",
                Vec::new(),
                Some("type a query to search GitHub repositories".to_string()),
            );
        }
        let search = format!("{query} skill");
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", &search)
            .append_pair("per_page", "30")
            .finish();
        let path = format!("/search/repositories?{encoded}");
        let bytes = match self.api_get(cfg, &path, MAX_INDEX_BYTES, false).await {
            Ok(bytes) => bytes,
            Err(err) => return ProviderResult::error("github", err.to_string()),
        };
        let document: GithubSearch = match serde_json::from_slice(&bytes) {
            Ok(document) => document,
            Err(err) => return ProviderResult::error("github", format!("invalid search: {err}")),
        };
        let entries = document
            .items
            .into_iter()
            .take(MAX_SEARCH_LIMIT)
            .map(|item| {
                let owner = item
                    .owner
                    .as_ref()
                    .map(|owner| owner.login.clone())
                    .unwrap_or_default();
                let repo = format!("{owner}/{}", item.name);
                let slug = slugify(item.name.strip_suffix("-skill").unwrap_or(&item.name));
                SearchEntry {
                    name: slug,
                    description: item.description.unwrap_or_default(),
                    provider: "github".to_string(),
                    source: (!owner.is_empty()).then(|| format!("github:{repo}")),
                    reference: None,
                    url: Some(item.html_url),
                    author: (!owner.is_empty()).then_some(owner),
                    tags: item.topics.into_iter().take(16).collect(),
                    version: None,
                    commit: None,
                }
            })
            .collect();
        ProviderResult::ok("github", entries, None)
    }

    async fn skills_sh_provider(&self, cfg: &Config, query: &str) -> ProviderResult {
        if !cfg.marketplace_skills_sh.enabled() {
            return ProviderResult::disabled("skills.sh", "FLEETT_MARKETPLACE_SKILLS_SH=off");
        }
        if let Some(cached) = self.cached_scrape(query) {
            return ProviderResult::ok("skills.sh", cached.entries, cached.message);
        }
        if !self.allow_skills_sh() {
            return ProviderResult::error(
                "skills.sh",
                "rate limited (at most one request per 2 s)".to_string(),
            );
        }
        let base = cfg.marketplace_skills_sh_base.trim_end_matches('/');
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", query)
            .finish();
        let url = format!("{base}/search?{encoded}");
        let bytes = match self
            .http_get(cfg, &url, MAX_INDEX_BYTES, Some("text/html"), false)
            .await
        {
            Ok(bytes) => bytes,
            Err(err) => return ProviderResult::error("skills.sh", err.to_string()),
        };
        let html = String::from_utf8_lossy(&bytes).to_string();
        let scraped = scan_skills_sh(&html);
        let entries: Vec<SearchEntry> = scraped
            .into_iter()
            .take(MAX_SEARCH_LIMIT)
            .map(|skill| SearchEntry {
                name: slugify(&skill.name),
                description: skill.description,
                provider: "skills.sh".to_string(),
                source: Some(format!("skills.sh:{}/{}", skill.owner, skill.name)),
                reference: None,
                url: Some(format!("{base}/{}/{}", skill.owner, skill.name)),
                author: Some(skill.owner),
                tags: Vec::new(),
                version: None,
                commit: None,
            })
            .collect();
        let message = entries
            .is_empty()
            .then(|| "no parseable skill on the page (the layout may have changed)".to_string());
        self.store_scrape(query, entries.clone(), message.clone());
        ProviderResult::ok("skills.sh", entries, message)
    }

    fn cached_git_index(&self) -> Option<CachedIndex> {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache
            .git
            .as_ref()
            .filter(|cached| cached.at.elapsed() < INDEX_CACHE_TTL)
            .cloned()
    }

    fn store_git_index(&self, entries: Vec<IndexEntry>) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.git = Some(CachedIndex {
            at: Instant::now(),
            entries,
        });
    }

    fn cached_scrape(&self, query: &str) -> Option<CachedScrape> {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache
            .skills_sh
            .get(query)
            .filter(|cached| cached.at.elapsed() < SKILLS_SH_CACHE_TTL)
            .cloned()
    }

    fn store_scrape(&self, query: &str, entries: Vec<SearchEntry>, message: Option<String>) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Bound the cache: 32 distinct queries are plenty for one console.
        if cache.skills_sh.len() > 32 {
            cache
                .skills_sh
                .retain(|_, cached| cached.at.elapsed() < SKILLS_SH_CACHE_TTL);
        }
        cache.skills_sh.insert(
            query.to_string(),
            CachedScrape {
                at: Instant::now(),
                entries,
                message,
            },
        );
    }

    fn allow_skills_sh(&self) -> bool {
        let now = Instant::now();
        let mut gate = self
            .skills_sh_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if gate.is_some_and(|last| now.duration_since(last) < SKILLS_SH_MIN_INTERVAL) {
            return false;
        }
        *gate = Some(now);
        true
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// Query every configured provider. One failure never fails the others.
    pub async fn search(
        &self,
        cfg: &Config,
        query: &str,
        provider: Option<&str>,
        limit: usize,
    ) -> Result<SearchResponse, MarketplaceError> {
        if let Some(provider) = provider {
            if !matches!(provider, "local" | "git" | "github" | "skills.sh") {
                return Err(MarketplaceError::InvalidSource(format!(
                    "unknown provider \"{provider}\"; expected local, git, github or skills.sh"
                )));
            }
        }
        let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
        let query = query.trim();

        let local = (provider.is_none() || provider == Some("local"))
            .then(|| self.local_provider(cfg, query));
        let (git, github, skills_sh) = tokio::join!(
            async {
                if provider.is_none() || provider == Some("git") {
                    Some(self.git_provider(cfg, query).await)
                } else {
                    None
                }
            },
            async {
                if provider.is_none() || provider == Some("github") {
                    Some(self.github_provider(cfg, query).await)
                } else {
                    None
                }
            },
            async {
                if provider.is_none() || provider == Some("skills.sh") {
                    Some(self.skills_sh_provider(cfg, query).await)
                } else {
                    None
                }
            }
        );

        let mut results: Vec<ProviderResult> = Vec::new();
        if let Some(result) = local {
            results.push(result);
        }
        if let Some(result) = git {
            results.push(result);
        }
        if let Some(result) = github {
            results.push(result);
        }
        if let Some(result) = skills_sh {
            results.push(result);
        }

        let mut entries = Vec::new();
        let mut sources = Vec::new();
        for result in results {
            entries.extend(result.entries.iter().cloned());
            sources.push(result.status());
        }
        entries.truncate(limit);
        Ok(SearchResponse { entries, sources })
    }

    // -----------------------------------------------------------------------
    // Resolution
    // -----------------------------------------------------------------------

    /// Resolve a source to the files it would install, without writing.
    pub async fn resolve(
        &self,
        cfg: &Config,
        source: &str,
        reference: Option<&str>,
    ) -> Result<ResolvedSkill, MarketplaceError> {
        let reference = reference.map(str::trim).filter(|value| !value.is_empty());
        match parse_source(source)? {
            SourceKind::Local { name } => self.resolve_local(cfg, &name, reference).await,
            SourceKind::Git { repo, path } => {
                self.resolve_repo(cfg, &repo, path.as_deref(), reference, "git")
                    .await
            }
            SourceKind::Github {
                repo,
                path,
                reference: inline,
            } => {
                let reference = inline.as_deref().or(reference);
                self.resolve_repo(cfg, &repo, path.as_deref(), reference, "github")
                    .await
            }
            SourceKind::Url { url } => self.resolve_url(cfg, &url, reference).await,
            SourceKind::SkillsSh { owner, name } => {
                self.resolve_skills_sh(cfg, &owner, &name).await
            }
        }
    }

    async fn resolve_local(
        &self,
        cfg: &Config,
        name: &str,
        reference: Option<&str>,
    ) -> Result<ResolvedSkill, MarketplaceError> {
        let entries =
            read_local_index(cfg)?.ok_or_else(|| MarketplaceError::NotFound(name.to_string()))?;
        let entry = entries
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| MarketplaceError::NotFound(name.to_string()))?;
        let reference = reference.or(entry.reference.as_deref());
        if let Some(url) = entry.url.as_deref() {
            let mut resolved = self.resolve_url(cfg, url, reference).await?;
            // The index may carry the upstream name, description and version.
            if !entry.name.trim().is_empty() {
                resolved.name = slugify(&entry.name);
            }
            if !entry.description.trim().is_empty() {
                resolved.description = entry.description.clone();
            }
            if entry.version.is_some() {
                resolved.version = entry.version.clone();
            }
            if entry.commit.is_some() {
                resolved.commit = entry.commit.clone();
            }
            if entry.author.is_some() {
                resolved.author = entry.author.clone();
            }
            if !entry.tags.is_empty() {
                resolved.tags = entry.tags.clone();
            }
            resolved.source = entry
                .source
                .clone()
                .unwrap_or_else(|| format!("local:{name}"));
            resolved.reference = reference.map(str::to_string);
            return Ok(resolved);
        }
        if let Some(upstream) = entry.source.as_deref() {
            // An index entry must point upstream; following another local
            // name would let a malformed index recurse forever.
            if matches!(parse_source(upstream)?, SourceKind::Local { .. }) {
                return Err(MarketplaceError::Invalid(format!(
                    "the local index entry \"{name}\" must point to an upstream source"
                )));
            }
            let mut resolved = Box::pin(self.resolve(cfg, upstream, reference)).await?;
            if entry.commit.is_some() {
                resolved.commit = entry.commit.clone();
            }
            if entry.version.is_some() {
                resolved.version = entry.version.clone();
            }
            return Ok(resolved);
        }
        Err(MarketplaceError::Invalid(format!(
            "the local index entry \"{name}\" has neither a url nor a source"
        )))
    }

    async fn resolve_repo(
        &self,
        cfg: &Config,
        repo: &str,
        path: Option<&str>,
        reference: Option<&str>,
        kind: &str,
    ) -> Result<ResolvedSkill, MarketplaceError> {
        let info = self.repo_info(cfg, repo).await.unwrap_or_default();
        let reference = reference
            .map(str::to_string)
            .or_else(|| info.default_branch.clone());
        let ref_used = reference.clone().unwrap_or_else(|| "HEAD".to_string());
        let commit = self.repo_commit(cfg, repo, &ref_used).await.ok().flatten();
        let archive_path = format!(
            "/repos/{repo}/zipball/{}",
            url::form_urlencoded::byte_serialize(ref_used.as_bytes()).collect::<String>()
        );
        let bytes = self
            .api_get(cfg, &archive_path, MAX_ARCHIVE_BYTES, false)
            .await?;
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(MarketplaceError::Invalid(format!(
                "the archive must be at most {MAX_ARCHIVE_BYTES} bytes"
            )));
        }
        let mut files = skills::read_zip(&bytes)?;
        if let Some(prefix) = path {
            let prefix = format!("{}/", prefix.trim_matches('/'));
            files = files
                .into_iter()
                .filter_map(|mut file| {
                    file.path
                        .strip_prefix(&prefix)
                        .map(|rest| rest.to_string())
                        .map(|rest| {
                            file.path = rest;
                            file
                        })
                })
                .collect();
            if files.is_empty() {
                return Err(MarketplaceError::NotFound(format!(
                    "{repo}:{prefix} contains no SKILL.md"
                )));
            }
        }
        let mut resolved = self.build_resolved(files, info.description.clone())?;
        // Prefer the frontmatter, then the path/repository name.
        if resolved.name.is_empty() {
            resolved.name = slugify(
                path.and_then(|path| path.rsplit('/').next())
                    .unwrap_or_else(|| repo.rsplit('/').next().unwrap_or(repo)),
            );
        }
        resolved.source = match path {
            Some(path) => format!("{kind}:{repo}:{path}"),
            None => format!("{kind}:{repo}"),
        };
        resolved.reference = reference;
        resolved.commit = commit;
        resolved.author = resolved
            .author
            .or_else(|| info.owner.clone())
            .or_else(|| repo.split('/').next().map(str::to_string));
        if resolved.tags.is_empty() {
            resolved.tags = info.topics;
        }
        Ok(resolved)
    }

    async fn resolve_url(
        &self,
        cfg: &Config,
        url: &str,
        reference: Option<&str>,
    ) -> Result<ResolvedSkill, MarketplaceError> {
        validate_url(url)?;
        let bytes = self
            .http_get(cfg, url, MAX_ARCHIVE_BYTES, None, false)
            .await?;
        let files = if bytes.starts_with(b"PK") {
            skills::read_zip(&bytes)?
        } else {
            let content = String::from_utf8(bytes).map_err(|_| {
                MarketplaceError::Invalid("the file is not valid UTF-8".to_string())
            })?;
            vec![SkillFile {
                path: skills::SKILL_FILE.to_string(),
                content,
            }]
        };
        let mut resolved = self.build_resolved(files, None)?;
        if resolved.name.is_empty() {
            resolved.name = slugify(
                url.rsplit('/')
                    .next()
                    .unwrap_or("skill")
                    .split('.')
                    .next()
                    .unwrap_or("skill"),
            );
        }
        resolved.source = format!("url:{url}");
        resolved.reference = reference.map(str::to_string);
        Ok(resolved)
    }

    async fn resolve_skills_sh(
        &self,
        cfg: &Config,
        owner: &str,
        name: &str,
    ) -> Result<ResolvedSkill, MarketplaceError> {
        let base = cfg.marketplace_skills_sh_base.trim_end_matches('/');
        let url = format!("{base}/{owner}/{name}");
        let bytes = self
            .http_get(cfg, &url, MAX_INDEX_BYTES, Some("text/html"), false)
            .await?;
        let html = String::from_utf8_lossy(&bytes).to_string();
        let target = find_upstream_link(&html).ok_or_else(|| {
            MarketplaceError::NotFound(format!(
                "skills.sh:{owner}/{name} does not link to an upstream repository"
            ))
        })?;
        let mut resolved = Box::pin(self.resolve(cfg, &target, None)).await?;
        resolved.source = format!("skills.sh:{owner}/{name}");
        Ok(resolved)
    }

    /// Turn a validated file set into the fields of a skill version.
    fn build_resolved(
        &self,
        files: Vec<SkillFile>,
        fallback_description: Option<String>,
    ) -> Result<ResolvedSkill, MarketplaceError> {
        if files.is_empty() {
            return Err(MarketplaceError::Invalid(
                "the archive contains no file".to_string(),
            ));
        }
        let skill_md = files
            .iter()
            .find(|file| file.path == skills::SKILL_FILE)
            .map(|file| file.content.clone())
            .ok_or_else(|| {
                MarketplaceError::Invalid(format!(
                    "{} is required at the root of the skill",
                    skills::SKILL_FILE
                ))
            })?;
        skills::validate_files(&files).map_err(MarketplaceError::Skill)?;
        let front = parse_frontmatter(&skill_md);
        let description = front
            .description
            .clone()
            .or_else(|| fallback_description.filter(|value| !value.trim().is_empty()))
            .unwrap_or_default();
        let description: String = description
            .chars()
            .take(skills::MAX_DESCRIPTION_CHARS)
            .collect();
        Ok(ResolvedSkill {
            name: front.name.as_deref().map(slugify).unwrap_or_default(),
            description,
            version: front.version,
            author: front.author,
            tags: front.tags,
            source: String::new(),
            reference: None,
            commit: None,
            files,
        })
    }

    // -----------------------------------------------------------------------
    // GitHub API
    // -----------------------------------------------------------------------

    async fn repo_info(&self, cfg: &Config, repo: &str) -> Result<RepoInfo, MarketplaceError> {
        let bytes = self
            .api_get(cfg, &format!("/repos/{repo}"), MAX_INDEX_BYTES, false)
            .await?;
        serde_json::from_slice::<RepoInfo>(&bytes)
            .map_err(|err| MarketplaceError::Invalid(format!("repository {repo}: {err}")))
    }

    async fn repo_commit(
        &self,
        cfg: &Config,
        repo: &str,
        reference: &str,
    ) -> Result<Option<String>, MarketplaceError> {
        let path = format!(
            "/repos/{repo}/commits/{}",
            url::form_urlencoded::byte_serialize(reference.as_bytes()).collect::<String>()
        );
        let bytes = self.api_get(cfg, &path, MAX_INDEX_BYTES, false).await?;
        #[derive(Deserialize)]
        struct Commit {
            sha: Option<String>,
        }
        let commit: Commit = serde_json::from_slice(&bytes)
            .map_err(|err| MarketplaceError::Invalid(format!("commit {reference}: {err}")))?;
        Ok(commit.sha)
    }

    async fn fetch_git_index(
        &self,
        cfg: &Config,
        repo: &str,
    ) -> Result<(Vec<IndexEntry>, Option<String>), MarketplaceError> {
        let bytes = self
            .api_get(
                cfg,
                &format!("/repos/{repo}/contents/index.json"),
                MAX_INDEX_BYTES,
                true,
            )
            .await?;
        let entries = parse_index(&bytes)?;
        Ok((entries, None))
    }

    // -----------------------------------------------------------------------
    // Install / update / sync
    // -----------------------------------------------------------------------

    /// `POST /api/marketplace/skills/install`.
    #[allow(clippy::too_many_arguments)]
    pub async fn install(
        &self,
        cfg: &Config,
        db: &Db,
        source: &str,
        reference: Option<&str>,
        name_override: Option<&str>,
        confirm_scripts: bool,
        author: &Author,
    ) -> Result<SkillDetail, MarketplaceError> {
        let resolved = self.resolve(cfg, source, reference).await?;
        let scripts = scripts_of(&resolved.files);
        if !scripts.is_empty() && !confirm_scripts {
            return Err(MarketplaceError::ScriptsRequireConfirmation(scripts));
        }
        let name = match name_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(name) if validate_skill_name(name).is_ok() => name.to_string(),
            Some(name) => {
                return Err(MarketplaceError::InvalidSource(format!(
                    "\"{name}\" is not a valid skill name"
                )))
            }
            None => resolved.name.clone(),
        };
        let version = skills::next_version(cfg, &name);
        let provenance = Provenance {
            source: Some(resolved.source.clone()),
            reference: resolved.reference.clone(),
            commit: resolved.commit.clone(),
            upstream_version: resolved.version.clone(),
            author: resolved.author.clone(),
            tags: resolved.tags.clone(),
            installed_at: Some(now_iso()),
        };
        let detail = skills::publish_with_provenance(
            cfg,
            db,
            &name,
            &version,
            &resolved.description,
            &resolved.files,
            &provenance,
            author,
        )?;
        self.invalidate();
        Ok(detail)
    }

    /// `POST /api/marketplace/skills/{name}/update`: re-resolve the recorded
    /// provenance, diff, and publish the next version.
    pub async fn update(
        &self,
        cfg: &Config,
        db: &Db,
        name: &str,
        confirm_scripts: bool,
        author: &Author,
    ) -> Result<UpdateOutcome, MarketplaceError> {
        if validate_skill_name(name).is_err() || !cfg.skill_dir(name).is_dir() {
            return Err(MarketplaceError::NotFound(name.to_string()));
        }
        let from = skills::latest(cfg, name)
            .ok_or_else(|| MarketplaceError::NoUpstream(name.to_string()))?;
        let provenance = skills::version_provenance(cfg, name, &from)?;
        let source = provenance
            .source
            .clone()
            .ok_or_else(|| MarketplaceError::NoUpstream(name.to_string()))?;
        let resolved = self
            .resolve(cfg, &source, provenance.reference.as_deref())
            .await?;
        let scripts = scripts_of(&resolved.files);
        if !scripts.is_empty() && !confirm_scripts {
            return Err(MarketplaceError::ScriptsRequireConfirmation(scripts));
        }
        let installed = skills::version_files(cfg, name, &from)?;
        let changes = diff_files(&installed, &resolved.files);
        let same_commit = match (provenance.commit.as_deref(), resolved.commit.as_deref()) {
            (Some(before), Some(after)) => before == after,
            _ => false,
        };
        if changes.is_empty() && same_commit {
            return Err(MarketplaceError::Unchanged(name.to_string()));
        }
        let to = skills::next_version(cfg, name);
        let next = Provenance {
            source: Some(resolved.source.clone()),
            reference: resolved.reference.clone(),
            commit: resolved.commit.clone(),
            upstream_version: resolved.version.clone(),
            author: resolved.author.clone(),
            tags: resolved.tags.clone(),
            installed_at: Some(now_iso()),
        };
        let detail = skills::publish_with_provenance(
            cfg,
            db,
            name,
            &to,
            &resolved.description,
            &resolved.files,
            &next,
            author,
        )?;
        self.invalidate();
        Ok(UpdateOutcome {
            detail,
            from,
            to,
            commit: resolved.commit,
            files: changes,
        })
    }

    /// `GET /api/marketplace/updates`, cached for one minute.
    pub async fn updates(&self, cfg: &Config, db: &Db) -> UpdatesResponse {
        {
            let cached = self
                .updates
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cached
                .as_ref()
                .filter(|cached| cached.at.elapsed() < UPDATES_CACHE_TTL)
            {
                return cached.response.clone();
            }
        }
        let response = self.compute_updates(cfg, db).await;
        let mut cached = self
            .updates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *cached = Some(CachedUpdates {
            at: Instant::now(),
            response: response.clone(),
        });
        response
    }

    async fn compute_updates(&self, cfg: &Config, db: &Db) -> UpdatesResponse {
        let summaries = match skills::list(cfg, db) {
            Ok(summaries) => summaries,
            Err(err) => {
                return UpdatesResponse {
                    updates: Vec::new(),
                    warnings: vec![format!("the skill registry is unreadable: {err}")],
                }
            }
        };
        let mut updates = Vec::new();
        let mut warnings = Vec::new();
        for summary in summaries {
            let Some(installed) = summary.latest.clone() else {
                continue;
            };
            let provenance = match skills::version_provenance(cfg, &summary.name, &installed) {
                Ok(provenance) => provenance,
                Err(err) => {
                    warnings.push(format!("{}: {err}", summary.name));
                    continue;
                }
            };
            let Some(source) = provenance.source.clone() else {
                continue;
            };
            let resolved = match self
                .resolve(cfg, &source, provenance.reference.as_deref())
                .await
            {
                Ok(resolved) => resolved,
                Err(err) => {
                    // A provider outage must never turn the endpoint into a
                    // 500: the entry is omitted and a warning explains why.
                    warnings.push(format!("{}: {err}", summary.name));
                    continue;
                }
            };
            let installed_files = match skills::version_files(cfg, &summary.name, &installed) {
                Ok(files) => files,
                Err(err) => {
                    warnings.push(format!("{}: {err}", summary.name));
                    continue;
                }
            };
            let changed = match (provenance.commit.as_deref(), resolved.commit.as_deref()) {
                (Some(before), Some(after)) => before != after,
                _ => !diff_files(&installed_files, &resolved.files).is_empty(),
            };
            if changed {
                updates.push(UpdateInfo {
                    name: summary.name,
                    installed_version: installed,
                    upstream_version: resolved.version,
                    commit: resolved.commit,
                    source,
                    reference: resolved.reference,
                });
            }
        }
        updates.sort_by(|left, right| left.name.cmp(&right.name));
        UpdatesResponse { updates, warnings }
    }

    /// `POST /api/marketplace/sync`: refresh the local index from the
    /// configured git repository.
    pub async fn sync(&self, cfg: &Config) -> Result<SyncOutcome, MarketplaceError> {
        let configured = cfg
            .marketplace_git_index
            .clone()
            .ok_or(MarketplaceError::NoIndexConfigured)?;
        let (repo, _, _) = parse_repo_ref(&configured)?;
        let (entries, commit) = self.fetch_git_index(cfg, &repo).await?;
        write_local_index(cfg, &entries)?;
        self.store_git_index(entries.clone());
        self.invalidate();
        Ok(SyncOutcome {
            skills: entries.len(),
            source: repo,
            commit,
        })
    }

    /// `POST /api/marketplace/skills/preview`.
    pub async fn preview(
        &self,
        cfg: &Config,
        source: &str,
        reference: Option<&str>,
    ) -> Result<PreviewResponse, MarketplaceError> {
        let resolved = self.resolve(cfg, source, reference).await?;
        Ok(resolved.into_preview())
    }
}

// ---------------------------------------------------------------------------
// Small response shapes
// ---------------------------------------------------------------------------

fn index_entry_matches(entry: &IndexEntry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let needle = query.to_ascii_lowercase();
    let haystack = format!(
        "{} {} {} {}",
        entry.name,
        entry.description,
        entry.tags.join(" "),
        entry.author.clone().unwrap_or_default()
    )
    .to_ascii_lowercase();
    haystack.contains(&needle)
}

fn index_search_entry(entry: IndexEntry, provider: &str) -> SearchEntry {
    let source = entry
        .source
        .clone()
        .or_else(|| entry.url.clone().map(|url| format!("url:{url}")))
        .or_else(|| Some(format!("local:{}", entry.name)));
    SearchEntry {
        name: entry.name,
        description: entry.description,
        provider: provider.to_string(),
        source,
        reference: entry.reference,
        url: entry.url,
        author: entry.author,
        tags: entry.tags,
        version: entry.version,
        commit: entry.commit,
    }
}

/// Find the first upstream link of a skills.sh detail page.
fn find_upstream_link(html: &str) -> Option<String> {
    let mut rest = html;
    while let Some(position) = rest.find("https://github.com/") {
        let candidate = &rest[position..];
        let end = candidate
            .find(['"', '\'', '<', ' ', '\n', '\r', ')'])
            .unwrap_or(candidate.len());
        let url = candidate[..end].trim_end_matches(['.', ',']);
        let path = url.trim_start_matches("https://github.com/");
        if path.split('/').filter(|part| !part.is_empty()).count() >= 2 {
            return Some(format!("github:{path}"));
        }
        rest = &rest[position + 1..];
    }
    let mut rest = html;
    while let Some(position) = rest.find("href=") {
        let after = &rest[position + 5..];
        let quote = after.chars().next()?;
        if quote != '"' && quote != '\'' {
            rest = after;
            continue;
        }
        let after_quote = &after[1..];
        let end = after_quote.find(quote)?;
        let href = &after_quote[..end];
        rest = &after_quote[end + 1..];
        if href.starts_with("https://") && (href.ends_with(".zip") || href.ends_with(".md")) {
            return Some(format!("url:{href}"));
        }
    }
    None
}

#[derive(Debug, Clone, Default)]
struct RepoInfo {
    default_branch: Option<String>,
    description: Option<String>,
    owner: Option<String>,
    topics: Vec<String>,
}

impl<'de> Deserialize<'de> for RepoInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawOwner {
            login: Option<String>,
        }
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            default_branch: Option<String>,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            owner: Option<RawOwner>,
            #[serde(default)]
            topics: Vec<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(RepoInfo {
            default_branch: raw.default_branch,
            description: raw.description,
            owner: raw.owner.and_then(|owner| owner.login),
            topics: raw.topics,
        })
    }
}

#[derive(Debug, Deserialize)]
struct GithubSearch {
    #[serde(default)]
    items: Vec<GithubRepo>,
}

#[derive(Debug, Deserialize)]
struct GithubRepo {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    owner: Option<GithubOwner>,
    #[serde(default)]
    topics: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GithubOwner {
    #[serde(default)]
    login: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_grammar_parses_every_form() {
        assert_eq!(
            parse_source("local:pdf-tools").unwrap(),
            SourceKind::Local {
                name: "pdf-tools".to_string()
            }
        );
        assert_eq!(
            parse_source("pdf-tools").unwrap(),
            SourceKind::Local {
                name: "pdf-tools".to_string()
            }
        );
        assert_eq!(
            parse_source("github:owner/repo").unwrap(),
            SourceKind::Github {
                repo: "owner/repo".to_string(),
                path: None,
                reference: None
            }
        );
        assert_eq!(
            parse_source("github:owner/repo:skills/pdf@main").unwrap(),
            SourceKind::Github {
                repo: "owner/repo".to_string(),
                path: Some("skills/pdf".to_string()),
                reference: Some("main".to_string())
            }
        );
        assert_eq!(
            parse_source("git:owner/repo:nested/skill").unwrap(),
            SourceKind::Git {
                repo: "owner/repo".to_string(),
                path: Some("nested/skill".to_string())
            }
        );
        assert_eq!(
            parse_source("url:https://example.com/skill.zip").unwrap(),
            SourceKind::Url {
                url: "https://example.com/skill.zip".to_string()
            }
        );
        assert_eq!(
            parse_source("skills.sh:owner/name").unwrap(),
            SourceKind::SkillsSh {
                owner: "owner".to_string(),
                name: "name".to_string()
            }
        );
    }

    #[test]
    fn source_grammar_rejects_hostile_forms() {
        for bad in [
            "",
            "github:owner",
            "github:owner/repo/extra",
            "github:owner/repo:../escape",
            "git:owner/repo:..",
            "url:ftp://example.com/x.zip",
            "skills.sh:owner",
            "local:../escape",
            "unknown:thing",
        ] {
            assert!(parse_source(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn scripts_are_detected_by_extension_or_shebang() {
        assert!(is_script("scripts/install.sh", "echo hi"));
        assert!(is_script("run.PY", "print(1)"));
        assert!(is_script("bin/tool", "#!/usr/bin/env bash\necho hi"));
        assert!(!is_script("SKILL.md", "# Skill\n```sh\n```"));
        assert!(!is_script("notes.txt", "not a shebang"));
        let files = vec![
            SkillFile {
                path: "SKILL.md".to_string(),
                content: "# x".to_string(),
            },
            SkillFile {
                path: "scripts/install.sh".to_string(),
                content: "#!/bin/sh".to_string(),
            },
        ];
        assert_eq!(scripts_of(&files), vec!["scripts/install.sh"]);
    }

    #[test]
    fn html_scanner_reads_anchors_tolerantly() {
        let html = r#"
            <html><body>
              <a class="card" href="/vercel-labs/agent-skills">
                <h3>Agent Skills</h3>
              </a>
              <a href='/acme/pdf-tools?tab=readme'>PDF tools</a>
              <a href="/not-a-skill">Nope</a>
              <a href="/acme/pdf-tools">Duplicate</a>
            </body></html>
        "#;
        let found = scan_skills_sh(html);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].owner, "vercel-labs");
        assert_eq!(found[0].name, "agent-skills");
        assert_eq!(found[0].description, "Agent Skills");
        assert_eq!(found[1].description, "PDF tools");
    }

    #[test]
    fn html_scanner_degrades_to_empty_on_a_redesign() {
        assert!(scan_skills_sh("<html><body><div>Nothing here</div></body></html>").is_empty());
        assert!(scan_skills_sh("").is_empty());
    }

    #[test]
    fn frontmatter_is_optional_and_tolerant() {
        let parsed = parse_frontmatter(
            "---\nname: PDF Tools\ndescription: Work with PDFs\ntags: [pdf, docs]\n---\n# Body\n",
        );
        assert_eq!(parsed.name.as_deref(), Some("PDF Tools"));
        assert_eq!(parsed.description.as_deref(), Some("Work with PDFs"));
        assert_eq!(parsed.tags, vec!["pdf", "docs"]);
        assert_eq!(
            parse_frontmatter("# No frontmatter\n"),
            Frontmatter::default()
        );
        assert_eq!(
            parse_frontmatter("---\nnot: [valid\n---\n"),
            Frontmatter::default()
        );
    }

    #[test]
    fn slugify_produces_a_registry_name() {
        assert_eq!(slugify("PDF Tools!"), "pdf-tools");
        assert_eq!(slugify("  Hello   World  "), "hello-world");
        assert_eq!(slugify("x"), "x-skill");
        assert!(slugify(&"a".repeat(80)).len() <= 32);
        assert!(validate_skill_name(&slugify("Vercel Labs agent-skills")).is_ok());
    }

    #[test]
    fn diff_files_reports_added_modified_removed() {
        let before = vec![
            SkillFile {
                path: "SKILL.md".to_string(),
                content: "one".to_string(),
            },
            SkillFile {
                path: "old.md".to_string(),
                content: "old".to_string(),
            },
        ];
        let after = vec![
            SkillFile {
                path: "SKILL.md".to_string(),
                content: "two".to_string(),
            },
            SkillFile {
                path: "new.md".to_string(),
                content: "new".to_string(),
            },
        ];
        let changes = diff_files(&before, &after);
        let statuses: Vec<(&str, &str)> = changes
            .iter()
            .map(|change| (change.path.as_str(), change.status))
            .collect();
        assert_eq!(
            statuses,
            [
                ("SKILL.md", "modified"),
                ("new.md", "added"),
                ("old.md", "removed")
            ]
        );
    }

    #[test]
    fn the_index_parser_accepts_object_and_array() {
        let object = br#"{"skills":[{"name":"pdf","description":"x"}]}"#;
        assert_eq!(parse_index(object).unwrap().len(), 1);
        let array = br#"[{"name":"pdf"}]"#;
        assert_eq!(parse_index(array).unwrap().len(), 1);
        assert!(parse_index(br#"{"skills":"nope"}"#).is_err());
    }

    #[test]
    fn the_zip_reader_is_shared_and_strips_one_wrapper() {
        use std::io::{Cursor, Write as _};
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (path, content) in [
            ("pdf-tools-main/SKILL.md", "# PDF\n"),
            ("pdf-tools-main/notes/a.md", "a\n"),
        ] {
            writer.start_file(path, options).expect("start file");
            writer.write_all(content.as_bytes()).expect("write");
        }
        let bytes = writer.finish().expect("zip").into_inner();
        let files = skills::read_zip(&bytes).expect("read zip");
        assert_eq!(
            files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["SKILL.md", "notes/a.md"]
        );

        // A symlink entry is refused before anything reaches the disk.
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer.start_file("SKILL.md", options).expect("start");
        writer.write_all(b"# PDF\n").expect("write");
        writer
            .add_symlink("passwd", "/etc/passwd", options)
            .expect("symlink");
        let bytes = writer.finish().expect("zip").into_inner();
        assert!(skills::read_zip(&bytes).is_err());
    }

    #[test]
    fn the_preview_budget_is_bounded() {
        let marketplace = Marketplace::new();
        for _ in 0..PREVIEW_PER_MINUTE {
            assert!(marketplace.allow_preview());
        }
        assert!(!marketplace.allow_preview());
    }
}
