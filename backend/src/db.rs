//! SQLite index of the fleet.
//!
//! The database caches what the filesystem already says. It is reconciled from
//! `instances/*/compose.yaml` and `shares/*` at every start, so deleting
//! `fleet.db` only loses notes and timestamps, never an agent.
//!
//! One exception: `instance_secrets` holds the AES-256-GCM blobs of the
//! instance secrets (see [`crate::secrets`]). That table is *not* rebuildable
//! from disk; losing the database loses the stored tokens, and they cannot be
//! decrypted without the key that lives outside `FLEETT_ROOT`.

use crate::config::Config;
use crate::models::{now_iso, ChannelKind, InstanceSpec, ShareMode, ShareRef, Status};
use crate::store;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

/// Ordered schema migrations, applied at startup through `PRAGMA user_version`.
///
/// Each entry is one transactional step. Append only: never edit, reorder or
/// remove a released entry, or databases created by an older build will take a
/// different upgrade path.
const MIGRATIONS: &[&str] = &[
    // v1 — baseline index schema (create-only, safe to replay on a v0 file).
    r#"
CREATE TABLE IF NOT EXISTS instances (
    id            TEXT PRIMARY KEY NOT NULL,
    name          TEXT NOT NULL,
    template      TEXT NOT NULL,
    model         TEXT NOT NULL,
    base_url      TEXT NOT NULL,
    api_key_env   TEXT NOT NULL,
    channel       TEXT NOT NULL,
    status        TEXT NOT NULL,
    last_error    TEXT,
    mem_limit_mb  INTEGER NOT NULL,
    notes         TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS shares (
    name        TEXT PRIMARY KEY NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS share_links (
    name         TEXT NOT NULL,
    instance_id  TEXT NOT NULL,
    mode         TEXT NOT NULL,
    PRIMARY KEY (name, instance_id)
);

CREATE INDEX IF NOT EXISTS idx_share_links_instance ON share_links (instance_id);
"#,
    // v2 — per-instance CPU quota (databases from v0/v1 get it through ALTER).
    "ALTER TABLE instances ADD COLUMN cpu_limit REAL NOT NULL DEFAULT 1.0;",
    // v3 — explicit LAN access flag (restricted `fleett` network by default).
    "ALTER TABLE instances ADD COLUMN allow_lan INTEGER NOT NULL DEFAULT 0;",
    // v4 — instance secrets encrypted at rest (one AES-256-GCM blob per
    // instance). The plaintext never reaches the database.
    r#"
CREATE TABLE IF NOT EXISTS instance_secrets (
    instance_id  TEXT PRIMARY KEY NOT NULL,
    nonce        BLOB NOT NULL,
    ciphertext   BLOB NOT NULL,
    updated_at   TEXT NOT NULL
);
"#,
    // v5 — authentication: the audit trail and the named API tokens (only a
    // SHA-256 digest is stored, never the token itself).
    r#"
CREATE TABLE IF NOT EXISTS audit_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    at         TEXT NOT NULL,
    actor      TEXT NOT NULL,
    role       TEXT NOT NULL,
    method     TEXT NOT NULL,
    action     TEXT NOT NULL,
    target     TEXT NOT NULL,
    ip         TEXT,
    details    TEXT
);

CREATE INDEX IF NOT EXISTS idx_audit_log_at ON audit_log (at);

CREATE TABLE IF NOT EXISTS api_tokens (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    role        TEXT NOT NULL,
    hash        TEXT NOT NULL UNIQUE,
    created_at  TEXT NOT NULL,
    revoked_at  TEXT
);
"#,
];

/// Schema version this build expects, i.e. the number of applied migrations.
pub const SCHEMA_VERSION: usize = MIGRATIONS.len();

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema v{found} is newer than this build supports (v{supported})")]
    SchemaNewer { found: i64, supported: usize },
    #[error("{0} already exists")]
    Conflict(String),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct ShareRecord {
    pub name: String,
    pub created_at: String,
}

/// One encrypted secrets blob, as stored in `instance_secrets`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedRecord {
    pub instance_id: String,
    /// AES-GCM nonce (96 bits).
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub updated_at: String,
}

/// One audit log entry, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    pub id: i64,
    pub at: String,
    pub actor: String,
    pub role: String,
    pub method: String,
    pub action: String,
    pub target: String,
    pub ip: Option<String>,
    /// JSON text, when the action carries structured details.
    pub details: Option<String>,
}

/// A row to append to `audit_log`.
#[derive(Debug, Clone)]
pub struct NewAuditRecord {
    pub at: String,
    pub actor: String,
    pub role: String,
    pub method: String,
    pub action: String,
    pub target: String,
    pub ip: Option<String>,
    pub details: Option<String>,
}

/// A named API token. The secret is never stored, only its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiTokenRecord {
    pub id: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

impl ApiTokenRecord {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// A token row to insert (already hashed).
#[derive(Debug, Clone)]
pub struct NewApiToken {
    pub id: String,
    pub name: String,
    pub role: String,
    pub hash: String,
    pub created_at: String,
}

/// Handle to the index database.
#[derive(Debug, Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // `journal_mode` returns a row, so it cannot go through execute_batch.
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn migrate(&self) -> Result<(), DbError> {
        let mut conn = self.lock();
        apply_migrations(&mut conn, MIGRATIONS)
    }

    /// Current `PRAGMA user_version`.
    pub fn schema_version(&self) -> Result<i64, DbError> {
        let conn = self.lock();
        Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    pub fn get(&self, id: &str) -> Result<Option<InstanceSpec>, DbError> {
        let conn = self.lock();
        let stored = conn
            .query_row(
                "SELECT id, name, template, model, base_url, api_key_env, channel, status, \
                 last_error, mem_limit_mb, cpu_limit, allow_lan, notes, created_at, updated_at \
                 FROM instances WHERE id = ?1",
                params![id],
                row_to_stored,
            )
            .optional()?;
        let Some(stored) = stored else {
            return Ok(None);
        };
        let shares = load_links(&conn, id)?;
        Ok(Some(stored.into_spec(shares)))
    }

    pub fn list(&self) -> Result<Vec<InstanceSpec>, DbError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT id, name, template, model, base_url, api_key_env, channel, status, \
             last_error, mem_limit_mb, cpu_limit, allow_lan, notes, created_at, updated_at \
             FROM instances ORDER BY id",
        )?;
        let stored = statement
            .query_map([], row_to_stored)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);

        let mut links = load_all_links(&conn)?;
        Ok(stored
            .into_iter()
            .map(|row| {
                let shares = links.remove(&row.id).unwrap_or_default();
                row.into_spec(shares)
            })
            .collect())
    }

    pub fn exists(&self, id: &str) -> Result<bool, DbError> {
        let conn = self.lock();
        let found = conn
            .query_row("SELECT 1 FROM instances WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        Ok(found)
    }

    pub fn insert_instance(&self, spec: &InstanceSpec) -> Result<(), DbError> {
        let mut conn = self.lock();
        let transaction = conn.transaction()?;
        let exists = transaction
            .query_row(
                "SELECT 1 FROM instances WHERE id = ?1",
                params![spec.id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            return Err(DbError::Conflict(spec.id.clone()));
        }
        transaction.execute(
            "INSERT INTO instances (id, name, template, model, base_url, api_key_env, channel, \
             status, last_error, mem_limit_mb, cpu_limit, allow_lan, notes, created_at, \
             updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                spec.id,
                spec.name,
                spec.template,
                spec.model,
                spec.base_url,
                spec.api_key_env,
                spec.channel.as_str(),
                spec.status.as_str(),
                spec.last_error,
                spec.mem_limit_mb,
                spec.cpu_limit,
                spec.allow_lan,
                spec.notes,
                spec.created_at,
                spec.updated_at,
            ],
        )?;
        for share in &spec.shares {
            insert_link(&transaction, share, &spec.id)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn update_instance(&self, spec: &InstanceSpec) -> Result<(), DbError> {
        let mut conn = self.lock();
        let transaction = conn.transaction()?;
        let updated = transaction.execute(
            "UPDATE instances SET name = ?2, template = ?3, model = ?4, base_url = ?5, \
             api_key_env = ?6, channel = ?7, status = ?8, last_error = ?9, mem_limit_mb = ?10, \
             cpu_limit = ?11, allow_lan = ?12, notes = ?13, updated_at = ?14 WHERE id = ?1",
            params![
                spec.id,
                spec.name,
                spec.template,
                spec.model,
                spec.base_url,
                spec.api_key_env,
                spec.channel.as_str(),
                spec.status.as_str(),
                spec.last_error,
                spec.mem_limit_mb,
                spec.cpu_limit,
                spec.allow_lan,
                spec.notes,
                spec.updated_at,
            ],
        )?;
        if updated == 0 {
            return Err(DbError::Conflict(spec.id.clone()));
        }
        transaction.execute(
            "DELETE FROM share_links WHERE instance_id = ?1",
            params![spec.id],
        )?;
        for share in &spec.shares {
            insert_link(&transaction, share, &spec.id)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn set_status(
        &self,
        id: &str,
        status: Status,
        last_error: Option<&str>,
    ) -> Result<(), DbError> {
        self.lock().execute(
            "UPDATE instances SET status = ?2, last_error = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, status.as_str(), last_error, now_iso()],
        )?;
        Ok(())
    }

    /// Update `last_error` alone (health probe failures do not change status).
    pub fn set_last_error(&self, id: &str, last_error: Option<&str>) -> Result<(), DbError> {
        self.lock().execute(
            "UPDATE instances SET last_error = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, last_error, now_iso()],
        )?;
        Ok(())
    }

    pub fn delete_instance(&self, id: &str) -> Result<(), DbError> {
        let mut conn = self.lock();
        let transaction = conn.transaction()?;
        transaction.execute(
            "DELETE FROM share_links WHERE instance_id = ?1",
            params![id],
        )?;
        transaction.execute(
            "DELETE FROM instance_secrets WHERE instance_id = ?1",
            params![id],
        )?;
        transaction.execute("DELETE FROM instances WHERE id = ?1", params![id])?;
        transaction.commit()?;
        Ok(())
    }

    /// Encrypted secrets blob of one instance, when it has one.
    pub fn sealed(&self, id: &str) -> Result<Option<SealedRecord>, DbError> {
        let conn = self.lock();
        let record = conn
            .query_row(
                "SELECT instance_id, nonce, ciphertext, updated_at FROM instance_secrets \
                 WHERE instance_id = ?1",
                params![id],
                |row| {
                    Ok(SealedRecord {
                        instance_id: row.get(0)?,
                        nonce: row.get(1)?,
                        ciphertext: row.get(2)?,
                        updated_at: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(record)
    }

    /// Insert or replace the encrypted secrets blob of one instance.
    ///
    /// A fresh nonce must be generated for every value: the caller encrypts
    /// first, this only records the bytes.
    pub fn put_sealed(&self, id: &str, nonce: &[u8], ciphertext: &[u8]) -> Result<(), DbError> {
        self.lock().execute(
            "INSERT INTO instance_secrets (instance_id, nonce, ciphertext, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (instance_id) DO UPDATE SET \
                 nonce = excluded.nonce, \
                 ciphertext = excluded.ciphertext, \
                 updated_at = excluded.updated_at",
            params![id, nonce, ciphertext, now_iso()],
        )?;
        Ok(())
    }

    pub fn delete_sealed(&self, id: &str) -> Result<(), DbError> {
        self.lock().execute(
            "DELETE FROM instance_secrets WHERE instance_id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Ids of every instance that has an encrypted secrets blob.
    pub fn sealed_ids(&self) -> Result<Vec<String>, DbError> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT instance_id FROM instance_secrets ORDER BY instance_id")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn sealed_count(&self) -> Result<usize, DbError> {
        let conn = self.lock();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM instance_secrets", [], |row| {
            row.get(0)
        })?;
        Ok(count.max(0) as usize)
    }

    pub fn share_exists(&self, name: &str) -> Result<bool, DbError> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT 1 FROM shares WHERE name = ?1",
                params![name],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn insert_share(&self, name: &str) -> Result<(), DbError> {
        let conn = self.lock();
        if conn
            .query_row(
                "SELECT 1 FROM shares WHERE name = ?1",
                params![name],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(DbError::Conflict(name.to_string()));
        }
        conn.execute(
            "INSERT INTO shares (name, created_at) VALUES (?1, ?2)",
            params![name, now_iso()],
        )?;
        Ok(())
    }

    pub fn delete_share(&self, name: &str) -> Result<(), DbError> {
        self.lock()
            .execute("DELETE FROM shares WHERE name = ?1", params![name])?;
        Ok(())
    }

    pub fn list_shares(&self) -> Result<Vec<ShareRecord>, DbError> {
        let conn = self.lock();
        let mut statement = conn.prepare("SELECT name, created_at FROM shares ORDER BY name")?;
        let rows = statement
            .query_map([], |row| {
                Ok(ShareRecord {
                    name: row.get(0)?,
                    created_at: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn links_for_share(&self, name: &str) -> Result<Vec<(String, ShareMode)>, DbError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT instance_id, mode FROM share_links WHERE name = ?1 ORDER BY instance_id",
        )?;
        let rows = statement
            .query_map(params![name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .map(|(id, mode)| (id, ShareMode::parse(&mode).unwrap_or(ShareMode::Rw)))
            .collect())
    }

    pub fn all_links(&self) -> Result<Vec<(String, String, ShareMode)>, DbError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT name, instance_id, mode FROM share_links ORDER BY name, instance_id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .map(|(name, id, mode)| (name, id, ShareMode::parse(&mode).unwrap_or(ShareMode::Rw)))
            .collect())
    }

    pub fn is_share_attached(&self, name: &str) -> Result<bool, DbError> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT 1 FROM share_links WHERE name = ?1 LIMIT 1",
                params![name],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn attach_share(
        &self,
        name: &str,
        instance_id: &str,
        mode: ShareMode,
    ) -> Result<(), DbError> {
        self.lock().execute(
            "INSERT INTO share_links (name, instance_id, mode) VALUES (?1, ?2, ?3) \
             ON CONFLICT (name, instance_id) DO UPDATE SET mode = excluded.mode",
            params![name, instance_id, mode.as_str()],
        )?;
        Ok(())
    }

    pub fn detach_share(&self, name: &str, instance_id: &str) -> Result<(), DbError> {
        self.lock().execute(
            "DELETE FROM share_links WHERE name = ?1 AND instance_id = ?2",
            params![name, instance_id],
        )?;
        Ok(())
    }

    /// Append one mutation to the audit trail, returning its row id.
    pub fn record_audit(&self, entry: &NewAuditRecord) -> Result<i64, DbError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO audit_log (at, actor, role, method, action, target, ip, details) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.at,
                entry.actor,
                entry.role,
                entry.method,
                entry.action,
                entry.target,
                entry.ip,
                entry.details,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Most recent entries first.
    pub fn list_audit(&self, limit: usize) -> Result<Vec<AuditRecord>, DbError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT id, at, actor, role, method, action, target, ip, details \
             FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement
            .query_map(params![limit as i64], |row| {
                Ok(AuditRecord {
                    id: row.get(0)?,
                    at: row.get(1)?,
                    actor: row.get(2)?,
                    role: row.get(3)?,
                    method: row.get(4)?,
                    action: row.get(5)?,
                    target: row.get(6)?,
                    ip: row.get(7)?,
                    details: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn insert_api_token(&self, token: &NewApiToken) -> Result<(), DbError> {
        self.lock().execute(
            "INSERT INTO api_tokens (id, name, role, hash, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                token.id,
                token.name,
                token.role,
                token.hash,
                token.created_at
            ],
        )?;
        Ok(())
    }

    pub fn list_api_tokens(&self) -> Result<Vec<ApiTokenRecord>, DbError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT id, name, role, created_at, revoked_at FROM api_tokens ORDER BY created_at, id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(ApiTokenRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    role: row.get(2)?,
                    created_at: row.get(3)?,
                    revoked_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Set `revoked_at` on an active token. Returns `false` when the id is
    /// unknown (an already revoked token keeps its first timestamp).
    pub fn revoke_api_token(&self, id: &str, at: &str) -> Result<bool, DbError> {
        let updated = self.lock().execute(
            "UPDATE api_tokens SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![id, at],
        )?;
        Ok(updated > 0)
    }

    pub fn api_token_exists(&self, id: &str) -> Result<bool, DbError> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT 1 FROM api_tokens WHERE id = ?1",
                params![id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Active (non-revoked) token matching a digest.
    pub fn active_token_by_hash(&self, hash: &str) -> Result<Option<ApiTokenRecord>, DbError> {
        let conn = self.lock();
        let record = conn
            .query_row(
                "SELECT id, name, role, created_at, revoked_at FROM api_tokens \
                 WHERE hash = ?1 AND revoked_at IS NULL",
                params![hash],
                |row| {
                    Ok(ApiTokenRecord {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        role: row.get(2)?,
                        created_at: row.get(3)?,
                        revoked_at: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(record)
    }
}

/// Apply every migration above the recorded `PRAGMA user_version`.
///
/// One transaction per migration: a failing statement rolls back its own step
/// (including the version bump) and leaves the database at the last version
/// that applied cleanly.
fn apply_migrations(conn: &mut Connection, migrations: &[&str]) -> Result<(), DbError> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let current = current.max(0) as usize;
    if current > migrations.len() {
        return Err(DbError::SchemaNewer {
            found: current as i64,
            supported: migrations.len(),
        });
    }
    for (index, sql) in migrations.iter().enumerate().skip(current) {
        let version = (index + 1) as i64;
        let transaction = conn.transaction()?;
        transaction.execute_batch(sql)?;
        transaction.pragma_update(None, "user_version", version)?;
        transaction.commit()?;
    }
    Ok(())
}

fn insert_link(conn: &Connection, share: &ShareRef, instance_id: &str) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO share_links (name, instance_id, mode) VALUES (?1, ?2, ?3) \
         ON CONFLICT (name, instance_id) DO UPDATE SET mode = excluded.mode",
        params![share.name, instance_id, share.mode.as_str()],
    )?;
    Ok(())
}

struct StoredRow {
    id: String,
    name: String,
    template: String,
    model: String,
    base_url: String,
    api_key_env: String,
    channel: String,
    status: String,
    last_error: Option<String>,
    mem_limit_mb: i64,
    cpu_limit: f64,
    allow_lan: bool,
    notes: String,
    created_at: String,
    updated_at: String,
}

impl StoredRow {
    fn into_spec(self, shares: Vec<ShareRef>) -> InstanceSpec {
        InstanceSpec {
            id: self.id,
            name: self.name,
            template: self.template,
            model: self.model,
            base_url: self.base_url,
            api_key_env: self.api_key_env,
            channel: ChannelKind::parse(&self.channel).unwrap_or(ChannelKind::None),
            shares,
            mem_limit_mb: self.mem_limit_mb.clamp(0, u32::MAX as i64) as u32,
            cpu_limit: if crate::models::validate_cpu_limit(self.cpu_limit).is_ok() {
                self.cpu_limit
            } else {
                crate::models::DEFAULT_CPU_LIMIT
            },
            allow_lan: self.allow_lan,
            notes: self.notes,
            status: Status::parse(&self.status).unwrap_or(Status::Unknown),
            last_error: self.last_error,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

fn row_to_stored(row: &Row<'_>) -> rusqlite::Result<StoredRow> {
    Ok(StoredRow {
        id: row.get("id")?,
        name: row.get("name")?,
        template: row.get("template")?,
        model: row.get("model")?,
        base_url: row.get("base_url")?,
        api_key_env: row.get("api_key_env")?,
        channel: row.get("channel")?,
        status: row.get("status")?,
        last_error: row.get("last_error")?,
        mem_limit_mb: row.get("mem_limit_mb")?,
        cpu_limit: row.get("cpu_limit")?,
        allow_lan: row.get("allow_lan")?,
        notes: row.get("notes")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn load_links(conn: &Connection, instance_id: &str) -> Result<Vec<ShareRef>, DbError> {
    let mut statement =
        conn.prepare("SELECT name, mode FROM share_links WHERE instance_id = ?1 ORDER BY name")?;
    let rows = statement
        .query_map(params![instance_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(name, mode)| ShareRef {
            name,
            mode: ShareMode::parse(&mode).unwrap_or(ShareMode::Rw),
        })
        .collect())
}

fn load_all_links(conn: &Connection) -> Result<HashMap<String, Vec<ShareRef>>, DbError> {
    let mut statement =
        conn.prepare("SELECT instance_id, name, mode FROM share_links ORDER BY instance_id, name")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut links: HashMap<String, Vec<ShareRef>> = HashMap::new();
    for (instance_id, name, mode) in rows {
        links.entry(instance_id).or_default().push(ShareRef {
            name,
            mode: ShareMode::parse(&mode).unwrap_or(ShareMode::Rw),
        });
    }
    Ok(links)
}

/// Rebuild the index from the filesystem. Instances whose compose file
/// disappeared are dropped from the index; new directories are adopted.
pub fn reconcile(cfg: &Config, db: &Db) -> Result<(), DbError> {
    let disk = store::scan_instances(cfg);
    let disk_ids: HashSet<&str> = disk.iter().map(|spec| spec.id.as_str()).collect();

    for spec in &disk {
        if db.get(&spec.id)?.is_none() {
            db.insert_instance(spec)?;
        }
    }
    for existing in db.list()? {
        if !disk_ids.contains(existing.id.as_str()) {
            db.delete_instance(&existing.id)?;
        }
    }

    for name in store::scan_shares(cfg) {
        if !db.share_exists(&name)? {
            if let Err(err) = db.insert_share(&name) {
                tracing::warn!(share = %name, %err, "could not index share directory");
            }
        }
    }
    for share in db.list_shares()? {
        if !cfg.share_dir(&share.name).is_dir() && !db.is_share_attached(&share.name)? {
            db.delete_share(&share.name)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DEFAULT_API_KEY_ENV, DEFAULT_MEM_LIMIT_MB};

    fn spec(id: &str) -> InstanceSpec {
        InstanceSpec {
            id: id.to_string(),
            name: id.to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            channel: ChannelKind::Telegram,
            shares: vec![ShareRef {
                name: "findings".to_string(),
                mode: ShareMode::Rw,
            }],
            mem_limit_mb: DEFAULT_MEM_LIMIT_MB,
            cpu_limit: crate::models::DEFAULT_CPU_LIMIT,
            allow_lan: false,
            notes: String::new(),
            status: Status::Created,
            last_error: None,
            created_at: now_iso(),
            updated_at: now_iso(),
        }
    }

    #[test]
    fn insert_get_update_delete() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");

        db.insert_instance(&spec("alice")).expect("insert");
        assert!(matches!(
            db.insert_instance(&spec("alice")),
            Err(DbError::Conflict(_))
        ));

        let stored = db.get("alice").expect("get").expect("present");
        assert_eq!(stored.shares.len(), 1);
        assert_eq!(stored.shares[0].name, "findings");

        let mut updated = stored.clone();
        updated.name = "Alice".to_string();
        updated.shares = vec![ShareRef {
            name: "archive".to_string(),
            mode: ShareMode::Ro,
        }];
        db.update_instance(&updated).expect("update");
        let stored = db.get("alice").expect("get").expect("present");
        assert_eq!(stored.name, "Alice");
        assert_eq!(stored.shares[0].name, "archive");

        db.delete_instance("alice").expect("delete");
        assert!(db.get("alice").expect("get").is_none());
    }

    #[test]
    fn shares_can_be_attached_and_detached() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");
        db.insert_share("findings").expect("share");
        assert!(matches!(
            db.insert_share("findings"),
            Err(DbError::Conflict(_))
        ));
        assert!(!db.is_share_attached("findings").expect("check"));

        db.attach_share("findings", "alice", ShareMode::Ro)
            .expect("attach");
        assert!(db.is_share_attached("findings").expect("check"));
        assert_eq!(
            db.links_for_share("findings").expect("links")[0].1,
            ShareMode::Ro
        );

        db.detach_share("findings", "alice").expect("detach");
        assert!(!db.is_share_attached("findings").expect("check"));
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |_| Ok(()),
        )
        .optional()
        .expect("query sqlite_master")
        .is_some()
    }

    #[test]
    fn fresh_database_is_created_at_the_current_schema_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");
        assert_eq!(db.schema_version().expect("version"), SCHEMA_VERSION as i64);
        let conn = db.lock();
        for table in [
            "instances",
            "shares",
            "share_links",
            "instance_secrets",
            "audit_log",
            "api_tokens",
        ] {
            assert!(table_exists(&conn, table), "missing table {table}");
        }
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(instances)")
            .expect("table_info")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("columns")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect columns");
        assert!(columns.iter().any(|column| column == "cpu_limit"));
        assert!(columns.iter().any(|column| column == "allow_lan"));
    }

    #[test]
    fn a_v0_database_is_migrated_in_place() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("fleet.db");
        {
            // What a build from before CPU limits and schema versioning left:
            // the v1 tables and `user_version = 0`.
            let conn = Connection::open(&path).expect("create v0 file");
            conn.execute_batch(MIGRATIONS[0]).expect("baseline schema");
            conn.execute(
                "INSERT INTO instances (id, name, template, model, base_url, api_key_env, \
                 channel, status, last_error, mem_limit_mb, notes, created_at, updated_at) \
                 VALUES ('alice', 'Alice', 'guest', 'm', 'https://example.test/v1', \
                 'HERMES_GATE_PROFILE_TOKEN', 'telegram', 'running', NULL, 1024, '', \
                 '2026-11-08T01:00:00Z', '2026-11-08T01:00:00Z')",
                [],
            )
            .expect("seed v0 row");
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .expect("version");
            assert_eq!(version, 0);
        }

        let db = Db::open(&path).expect("migrate the v0 database");
        assert_eq!(db.schema_version().expect("version"), SCHEMA_VERSION as i64);
        let alice = db.get("alice").expect("get").expect("kept row");
        assert_eq!(alice.name, "Alice");
        assert_eq!(alice.mem_limit_mb, 1024);
        assert_eq!(alice.cpu_limit, crate::models::DEFAULT_CPU_LIMIT);
        assert!(!alice.allow_lan);
        assert!(table_exists(&db.lock(), "share_links"));

        // Re-opening an up-to-date database applies nothing.
        drop(db);
        let reopened = Db::open(&path).expect("reopen");
        assert_eq!(
            reopened.schema_version().expect("version"),
            SCHEMA_VERSION as i64
        );
    }

    #[test]
    fn a_failing_migration_rolls_back_and_keeps_the_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("fleet.db");
        let mut conn = Connection::open(&path).expect("open");

        let err = apply_migrations(
            &mut conn,
            &["CREATE TABLE partial (id INTEGER);\nTHIS IS NOT SQL;"],
        )
        .expect_err("migration must fail");
        assert!(matches!(err, DbError::Sqlite(_)));
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 0, "a failed migration must not bump user_version");
        assert!(
            !table_exists(&conn, "partial"),
            "statements of a failed migration must be rolled back"
        );

        // A step that succeeds commits and bumps the version; a later failing
        // step leaves the database at that last good version.
        apply_migrations(&mut conn, &["CREATE TABLE alpha (id INTEGER);"]).expect("first step");
        let err = apply_migrations(
            &mut conn,
            &[
                "CREATE TABLE alpha (id INTEGER);",
                "CREATE TABLE beta (id INTEGER);",
                "STILL NOT SQL;",
            ],
        )
        .expect_err("third step fails");
        assert!(matches!(err, DbError::Sqlite(_)));
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 2);
        assert!(table_exists(&conn, "beta"));
    }

    #[test]
    fn audit_entries_are_appended_and_listed_newest_first() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");

        for (index, action) in ["instance.create", "instance.start"].iter().enumerate() {
            db.record_audit(&NewAuditRecord {
                at: format!("2026-11-08T00:00:0{index}Z"),
                actor: "cleeryy@example.test".to_string(),
                role: "admin".to_string(),
                method: "session".to_string(),
                action: action.to_string(),
                target: "alice".to_string(),
                ip: Some("10.1.20.50".to_string()),
                details: None,
            })
            .expect("record");
        }

        let entries = db.list_audit(10).expect("list");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action, "instance.start");
        assert_eq!(entries[1].action, "instance.create");
        assert_eq!(entries[0].actor, "cleeryy@example.test");
        assert_eq!(entries[0].ip.as_deref(), Some("10.1.20.50"));
        assert_eq!(db.list_audit(1).expect("limit").len(), 1);
    }

    #[test]
    fn api_tokens_are_stored_hashed_and_revoked_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");
        let hash = "a".repeat(64);
        db.insert_api_token(&NewApiToken {
            id: "tok-1".to_string(),
            name: "ci".to_string(),
            role: "viewer".to_string(),
            hash: hash.clone(),
            created_at: "2026-11-08T00:00:00Z".to_string(),
        })
        .expect("insert");

        let found = db
            .active_token_by_hash(&hash)
            .expect("lookup")
            .expect("active");
        assert_eq!(found.name, "ci");
        assert_eq!(found.role, "viewer");
        assert!(found.is_active());
        assert!(db
            .active_token_by_hash(&"b".repeat(64))
            .expect("lookup")
            .is_none());

        assert!(db
            .revoke_api_token("tok-1", "2026-11-08T01:00:00Z")
            .expect("revoke"));
        // A second revoke does not overwrite the timestamp.
        assert!(!db
            .revoke_api_token("tok-1", "2026-11-08T02:00:00Z")
            .expect("revoke again"));
        assert!(db.active_token_by_hash(&hash).expect("lookup").is_none());
        let listed = db.list_api_tokens().expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].revoked_at.as_deref(),
            Some("2026-11-08T01:00:00Z")
        );
        assert!(db.api_token_exists("tok-1").expect("exists"));
        assert!(!db.api_token_exists("tok-2").expect("exists"));
    }

    #[test]
    fn refuses_a_database_from_a_newer_build() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("fleet.db");
        {
            let conn = Connection::open(&path).expect("open");
            conn.pragma_update(None, "user_version", 99)
                .expect("bump version");
        }
        let err = Db::open(&path).expect_err("newer schema is refused");
        assert!(matches!(err, DbError::SchemaNewer { found: 99, .. }));
    }
}
