//! Shared fleet knowledge base (issue #29).
//!
//! A key/value store with FTS5 search, TTL, tags and a full version history.
//! Namespaces are labels, not security boundaries for now: `global`, `shared`,
//! `instance:<id>` and `project:<slug>`. Writes from an agent are expected to
//! target `instance:<its id>` or `shared`; per-token write scoping arrives with
//! the RBAC work (issue #37).
//!
//! Values are knowledge (`what did we learn`, `where is the runbook`), never
//! credentials. Secrets belong to `instance_env` and `providers`, where they
//! are encrypted.

use crate::db::{Db, DbError};
use crate::models::now_iso;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Largest accepted key.
pub const KEY_MAX: usize = 128;
/// Largest accepted value (bytes).
pub const VALUE_MAX_BYTES: usize = 64 * 1024;
/// Largest number of tags per entry.
pub const TAGS_MAX: usize = 16;
/// Largest tag length.
pub const TAG_MAX: usize = 32;
/// Smallest TTL.
pub const TTL_MIN_S: u64 = 60;
/// Largest TTL (one year).
pub const TTL_MAX_S: u64 = 365 * 24 * 60 * 60;
/// Default page size.
pub const DEFAULT_LIMIT: usize = 50;
/// Largest page size.
pub const MAX_LIMIT: usize = 500;

/// One current entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: i64,
    pub namespace: String,
    pub key: String,
    pub value: String,
    pub tags: Vec<String>,
    pub owner: String,
    pub version: u32,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// One historical version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryVersion {
    pub version: u32,
    pub value: String,
    pub tags: Vec<String>,
    pub owner: String,
    /// `create`, `update`, `delete` or `rollback`.
    pub action: String,
    pub at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("invalid namespace: {0}")]
    InvalidNamespace(String),
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("value exceeds {VALUE_MAX_BYTES} bytes")]
    ValueTooLarge,
    #[error("invalid tag: {0}")]
    InvalidTag(String),
    #[error("ttl_s must be between {TTL_MIN_S} and {TTL_MAX_S}")]
    InvalidTtl,
    #[error("search query is empty")]
    EmptyQuery,
    #[error("unknown memory entry {0}")]
    NotFound(i64),
    #[error("unknown version {0}")]
    UnknownVersion(u32),
    #[error("database error: {0}")]
    Db(#[from] DbError),
}

pub fn validate_namespace(namespace: &str) -> Result<(), MemoryError> {
    let value = namespace.trim();
    let ok = value == "global"
        || value == "shared"
        || value
            .strip_prefix("instance:")
            .is_some_and(|id| crate::models::validate_instance_id(id).is_ok())
        || value.strip_prefix("project:").is_some_and(|slug| {
            !slug.is_empty() && slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if ok {
        Ok(())
    } else {
        Err(MemoryError::InvalidNamespace(namespace.to_string()))
    }
}

fn validate_key(key: &str) -> Result<String, MemoryError> {
    let key = key.trim();
    if key.is_empty() || key.len() > KEY_MAX {
        return Err(MemoryError::InvalidKey(
            "a key is 1 to 128 characters long".to_string(),
        ));
    }
    if key.chars().any(|c| c.is_control()) {
        return Err(MemoryError::InvalidKey(
            "a key must not contain control characters".to_string(),
        ));
    }
    Ok(key.to_string())
}

fn normalize_tags(tags: &[String]) -> Result<Vec<String>, MemoryError> {
    if tags.len() > TAGS_MAX {
        return Err(MemoryError::InvalidTag(format!(
            "at most {TAGS_MAX} tags are accepted"
        )));
    }
    let mut out = Vec::new();
    for tag in tags {
        let tag = tag.trim().to_ascii_lowercase();
        if tag.is_empty() {
            continue;
        }
        if tag.len() > TAG_MAX
            || !tag
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        {
            return Err(MemoryError::InvalidTag(tag));
        }
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    Ok(out)
}

fn tags_to_string(tags: &[String]) -> String {
    tags.join(",")
}

fn tags_from_string(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

fn expires_at(ttl_s: Option<u64>) -> Result<Option<String>, MemoryError> {
    let Some(ttl) = ttl_s else {
        return Ok(None);
    };
    if !(TTL_MIN_S..=TTL_MAX_S).contains(&ttl) {
        return Err(MemoryError::InvalidTtl);
    }
    let at = chrono::Utc::now() + chrono::Duration::seconds(ttl as i64);
    Ok(Some(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
}

const COLUMNS: &str = "id, namespace, key, value, tags, owner, version, created_at, updated_at, \
                       expires_at";

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryEntry> {
    let tags: String = row.get(4)?;
    Ok(MemoryEntry {
        id: row.get(0)?,
        namespace: row.get(1)?,
        key: row.get(2)?,
        value: row.get(3)?,
        tags: tags_from_string(&tags),
        owner: row.get(5)?,
        version: row.get::<_, i64>(6)?.max(0) as u32,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        expires_at: row.get(9)?,
    })
}

fn index(conn: &rusqlite::Connection, entry: &MemoryEntry) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM memory_fts WHERE rowid = ?1", params![entry.id])?;
    conn.execute(
        "INSERT INTO memory_fts (rowid, namespace, key, value, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            entry.id,
            entry.namespace,
            entry.key,
            entry.value,
            tags_to_string(&entry.tags),
        ],
    )?;
    Ok(())
}

fn unindex(conn: &rusqlite::Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM memory_fts WHERE rowid = ?1", params![id])?;
    Ok(())
}

fn record_version(
    conn: &rusqlite::Connection,
    entry: &MemoryEntry,
    action: &str,
    now: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO memory_versions (entry_id, namespace, key, value, tags, owner, version, \
         action, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            entry.id,
            entry.namespace,
            entry.key,
            entry.value,
            tags_to_string(&entry.tags),
            entry.owner,
            entry.version,
            action,
            now,
        ],
    )?;
    Ok(())
}

/// Insert or update an entry, returning it with `created = true` on a new key.
pub fn upsert(
    db: &Db,
    namespace: &str,
    key: &str,
    value: &str,
    tags: &[String],
    owner: &str,
    ttl_s: Option<u64>,
) -> Result<(MemoryEntry, bool), MemoryError> {
    validate_namespace(namespace)?;
    let namespace = namespace.trim().to_string();
    let key = validate_key(key)?;
    if value.len() > VALUE_MAX_BYTES {
        return Err(MemoryError::ValueTooLarge);
    }
    let tags = normalize_tags(tags)?;
    let now = now_iso();

    let existing = get_by_key(db, &namespace, &key)?;
    let (entry, created) = match existing {
        Some(mut entry) => {
            entry.value = value.to_string();
            entry.tags = tags;
            entry.owner = owner.to_string();
            entry.version = entry.version.saturating_add(1).max(1);
            entry.updated_at = now.clone();
            if ttl_s.is_some() {
                entry.expires_at = expires_at(ttl_s)?;
            }
            db.with_conn(|conn| {
                let changed = conn.execute(
                    "UPDATE memory_entries SET value = ?1, tags = ?2, owner = ?3, version = ?4, \
                     updated_at = ?5, expires_at = ?6, deleted = 0 WHERE id = ?7",
                    params![
                        entry.value,
                        tags_to_string(&entry.tags),
                        entry.owner,
                        entry.version as i64,
                        entry.updated_at,
                        entry.expires_at,
                        entry.id,
                    ],
                )?;
                if changed == 0 {
                    return Err(rusqlite::Error::QueryReturnedNoRows);
                }
                index(conn, &entry)?;
                record_version(conn, &entry, "update", &now)?;
                Ok(())
            })?;
            (entry, false)
        }
        None => {
            let expires = expires_at(ttl_s)?;
            let count = db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO memory_entries (namespace, key, value, tags, owner, version, \
                     created_at, updated_at, expires_at, deleted) \
                     VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6, ?7, 0)",
                    params![
                        namespace,
                        key,
                        value,
                        tags_to_string(&tags),
                        owner,
                        now,
                        expires
                    ],
                )?;
                Ok(conn.last_insert_rowid())
            })?;
            let entry = MemoryEntry {
                id: count,
                namespace,
                key,
                value: value.to_string(),
                tags,
                owner: owner.to_string(),
                version: 1,
                created_at: now.clone(),
                updated_at: now.clone(),
                expires_at: expires,
            };
            db.with_conn(|conn| {
                index(conn, &entry)?;
                record_version(conn, &entry, "create", &now)
            })?;
            (entry, true)
        }
    };
    Ok((entry, created))
}

pub fn get(db: &Db, id: i64) -> Result<Option<MemoryEntry>, MemoryError> {
    let now = now_iso();
    let entry = db.with_conn(|conn| {
        conn.query_row(
            &format!(
                "SELECT {COLUMNS} FROM memory_entries \
                 WHERE id = ?1 AND deleted = 0 AND (expires_at IS NULL OR expires_at > ?2)"
            ),
            params![id, now],
            row_to_entry,
        )
        .optional()
    })?;
    Ok(entry)
}

pub fn get_by_key(db: &Db, namespace: &str, key: &str) -> Result<Option<MemoryEntry>, MemoryError> {
    let now = now_iso();
    let entry = db.with_conn(|conn| {
        conn.query_row(
            &format!(
                "SELECT {COLUMNS} FROM memory_entries \
                 WHERE namespace = ?1 AND key = ?2 AND deleted = 0 \
                   AND (expires_at IS NULL OR expires_at > ?3)"
            ),
            params![namespace, key, now],
            row_to_entry,
        )
        .optional()
    })?;
    Ok(entry)
}

/// Browse one namespace (or all when `None`), newest first.
pub fn list(
    db: &Db,
    namespace: Option<&str>,
    include_expired: bool,
    limit: usize,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let now = now_iso();
    let entries = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM memory_entries \
             WHERE deleted = 0 \
               AND (?1 IS NULL OR namespace = ?1) \
               AND (?2 = 1 OR expires_at IS NULL OR expires_at > ?3) \
             ORDER BY updated_at DESC LIMIT ?4"
        ))?;
        let rows = statement.query_map(
            params![namespace, include_expired as i64, now, limit as i64],
            row_to_entry,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    Ok(entries)
}

/// Turn free text into a safe FTS5 query: every word is quoted, so a user
/// cannot inject `OR`, `NEAR` or a column filter.
pub fn fts_query(text: &str) -> String {
    let terms: Vec<String> = text
        .split_whitespace()
        .map(|term| {
            term.chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
                .collect::<String>()
        })
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\"*"))
        .collect();
    terms.join(" AND ")
}

/// Full-text search, optionally restricted to one namespace and tags.
pub fn search(
    db: &Db,
    query: &str,
    namespace: Option<&str>,
    tags: &[String],
    limit: usize,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let fts = fts_query(query);
    if fts.is_empty() {
        return Err(MemoryError::EmptyQuery);
    }
    let tags = normalize_tags(tags)?;
    let limit = limit.clamp(1, MAX_LIMIT);
    let now = now_iso();
    let entries = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT m.id, m.namespace, m.key, m.value, m.tags, m.owner, m.version, \
                    m.created_at, m.updated_at, m.expires_at \
             FROM memory_fts f JOIN memory_entries m ON m.id = f.rowid \
             WHERE memory_fts MATCH ?1 \
               AND m.deleted = 0 \
               AND (?2 IS NULL OR m.namespace = ?2) \
               AND (m.expires_at IS NULL OR m.expires_at > ?3) \
             ORDER BY bm25(memory_fts), m.updated_at DESC LIMIT ?4",
        )?;
        let rows = statement.query_map(params![fts, namespace, now, limit as i64], row_to_entry)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    // Tag filtering happens in Rust: an AND over comma-separated tags would
    // need one LIKE per tag, and a page is capped at 500 rows.
    let filtered = entries
        .into_iter()
        .filter(|entry| tags.iter().all(|tag| entry.tags.contains(tag)))
        .collect();
    Ok(filtered)
}

/// Soft-delete an entry; its history stays readable and restorable.
pub fn delete(db: &Db, id: i64, actor: &str) -> Result<bool, MemoryError> {
    let Some(mut entry) = get(db, id)? else {
        return Ok(false);
    };
    let now = now_iso();
    entry.owner = actor.to_string();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE memory_entries SET deleted = 1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        unindex(conn, id)?;
        record_version(conn, &entry, "delete", &now)
    })?;
    Ok(true)
}

/// Full history, newest version first.
pub fn history(db: &Db, id: i64) -> Result<Vec<MemoryVersion>, MemoryError> {
    let versions = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT version, value, tags, owner, action, at FROM memory_versions \
             WHERE entry_id = ?1 ORDER BY version DESC, id DESC",
        )?;
        let rows = statement.query_map(params![id], |row| {
            let tags: String = row.get(2)?;
            Ok(MemoryVersion {
                version: row.get::<_, i64>(0)?.max(0) as u32,
                value: row.get(1)?,
                tags: tags_from_string(&tags),
                owner: row.get(3)?,
                action: row.get(4)?,
                at: row.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    if versions.is_empty() {
        // Distinguish "entry exists with no history" (impossible) from a bad id.
        return Err(MemoryError::NotFound(id));
    }
    Ok(versions)
}

/// Restore one historical version as a new current version.
pub fn rollback(db: &Db, id: i64, version: u32, actor: &str) -> Result<MemoryEntry, MemoryError> {
    let target = db.with_conn(|conn| {
        conn.query_row(
            "SELECT value, tags FROM memory_versions WHERE entry_id = ?1 AND version = ?2 \
             ORDER BY id DESC LIMIT 1",
            params![id, version],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
    })?;
    let Some((value, tags)) = target else {
        return Err(MemoryError::UnknownVersion(version));
    };
    let Some(mut entry) = db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM memory_entries WHERE id = ?1"),
            params![id],
            row_to_entry,
        )
        .optional()
    })?
    else {
        return Err(MemoryError::NotFound(id));
    };
    let now = now_iso();
    entry.value = value;
    entry.tags = tags_from_string(&tags);
    entry.owner = actor.to_string();
    entry.version = entry.version.saturating_add(1).max(1);
    entry.updated_at = now.clone();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE memory_entries SET value = ?1, tags = ?2, owner = ?3, version = ?4, \
             updated_at = ?5, expires_at = NULL, deleted = 0 WHERE id = ?6",
            params![
                entry.value,
                tags_to_string(&entry.tags),
                entry.owner,
                entry.version as i64,
                entry.updated_at,
                id,
            ],
        )?;
        index(conn, &entry)?;
        record_version(conn, &entry, "rollback", &now)
    })?;
    entry.expires_at = None;
    Ok(entry)
}

/// Copy one entry into another namespace (the `promote to global` action).
pub fn promote(db: &Db, id: i64, namespace: &str, actor: &str) -> Result<MemoryEntry, MemoryError> {
    let entry = get(db, id)?.ok_or(MemoryError::NotFound(id))?;
    let (promoted, _) = upsert(
        db,
        namespace,
        &entry.key,
        &entry.value,
        &entry.tags,
        actor,
        None,
    )?;
    Ok(promoted)
}

/// Delete expired rows and their FTS documents, returning the count.
pub fn purge_expired(db: &Db) -> Result<usize, MemoryError> {
    let now = now_iso();
    let expired = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id FROM memory_entries \
             WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        )?;
        let rows = statement.query_map(params![now], |row| row.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    for id in &expired {
        db.with_conn(|conn| {
            unindex(conn, *id)?;
            conn.execute("DELETE FROM memory_entries WHERE id = ?1", params![id])
        })?;
    }
    Ok(expired.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        let dir = tempfile::tempdir().expect("temp dir");
        Db::open(&dir.keep().join("fleet.db")).expect("open")
    }

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn entries_are_versioned_and_restorable() {
        let db = db();
        let (first, created) = upsert(
            &db,
            "instance:probe",
            "runbook",
            "restart the gateway",
            &tags(&["ops"]),
            "probe",
            None,
        )
        .expect("create");
        assert!(created);
        assert_eq!(first.version, 1);

        let (second, created) = upsert(
            &db,
            "instance:probe",
            "runbook",
            "restart the gateway then flush",
            &tags(&["ops", "urgent"]),
            "probe",
            None,
        )
        .expect("update");
        assert!(!created);
        assert_eq!(second.version, 2);
        assert_eq!(second.id, first.id);

        let versions = history(&db, first.id).expect("history");
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, 2);
        assert_eq!(versions[1].value, "restart the gateway");

        let restored = rollback(&db, first.id, 1, "tchouniz").expect("rollback");
        assert_eq!(restored.version, 3);
        assert_eq!(restored.value, "restart the gateway");
        assert_eq!(restored.owner, "tchouniz");
        assert_eq!(history(&db, first.id).expect("history").len(), 3);
    }

    #[test]
    fn search_finds_words_in_two_namespaces_and_ignores_expired_rows() {
        let db = db();
        upsert(
            &db,
            "instance:probe",
            "a",
            "the allowlist accepts discord ids",
            &tags(&["discord"]),
            "probe",
            None,
        )
        .expect("create a");
        upsert(
            &db,
            "shared",
            "b",
            "guest allowlist troubleshooting",
            &[],
            "probe",
            None,
        )
        .expect("create b");
        upsert(
            &db,
            "global",
            "c",
            "allowlist expired note",
            &[],
            "probe",
            Some(60),
        )
        .expect("create c");
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE memory_entries SET expires_at = '2000-01-01T00:00:00Z' WHERE key = 'c'",
                [],
            )
        })
        .expect("expire");

        let found = search(&db, "allowlist", None, &[], 10).expect("search");
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|entry| entry.key != "c"));

        let scoped = search(&db, "allowlist", Some("shared"), &[], 10).expect("scoped");
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].key, "b");
        assert_eq!(purge_expired(&db).expect("purge"), 1);
        assert!(get_by_key(&db, "global", "c").expect("gone").is_none());
    }

    #[test]
    fn deleted_entries_disappear_but_can_be_restored() {
        let db = db();
        let (entry, _) = upsert(&db, "shared", "gone", "v1", &[], "probe", None).expect("create");
        assert!(delete(&db, entry.id, "probe").expect("delete"));
        assert!(get(&db, entry.id).expect("get").is_none());
        assert!(list(&db, None, None, false, 10).expect("list").is_empty());
        let versions = history(&db, entry.id).expect("history");
        assert_eq!(versions[0].action, "delete");
        rollback(&db, entry.id, 1, "probe").expect("restore");
        assert_eq!(
            get(&db, entry.id).expect("get").expect("restored").value,
            "v1"
        );
        // Re-creating a deleted key updates the same row.
        let (again, created) =
            upsert(&db, "shared", "gone", "v2", &[], "probe", None).expect("recreate");
        assert!(!created);
        assert_eq!(again.version, 3);
    }

    #[test]
    fn promote_copies_an_entry_into_global() {
        let db = db();
        let (entry, _) = upsert(
            &db,
            "instance:probe",
            "lesson",
            "pin the image tag",
            &tags(&["deploy"]),
            "probe",
            None,
        )
        .expect("create");
        let promoted = promote(&db, entry.id, "global", "tchouniz").expect("promote");
        assert_eq!(promoted.namespace, "global");
        assert_eq!(promoted.value, "pin the image tag");
        assert_eq!(promoted.owner, "tchouniz");
        assert_eq!(promoted.version, 1);
        assert!(get_by_key(&db, "instance:probe", "lesson")
            .expect("source")
            .is_some());
    }

    #[test]
    fn validation_rejects_bad_input_and_sanitises_fts() {
        let db = db();
        assert!(matches!(
            upsert(&db, "somewhere", "k", "v", &[], "x", None),
            Err(MemoryError::InvalidNamespace(_))
        ));
        assert!(matches!(
            upsert(&db, "shared", "", "v", &[], "x", None),
            Err(MemoryError::InvalidKey(_))
        ));
        assert!(matches!(
            upsert(&db, "shared", "k", "v", &tags(&["not a tag"]), "x", None),
            Err(MemoryError::InvalidTag(_))
        ));
        assert!(matches!(
            upsert(&db, "shared", "k", "v", &[], "x", Some(1)),
            Err(MemoryError::InvalidTtl)
        ));
        assert_eq!(
            fts_query("hello OR world"),
            "\"hello\"* AND \"OR\"* AND \"world\"*"
        );
        assert!(matches!(
            search(&db, "   ", None, &[], 10),
            Err(MemoryError::EmptyQuery)
        ));
    }
}
