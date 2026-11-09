//! SQLite index of the fleet.
//!
//! The database caches what the filesystem already says. It is reconciled from
//! `instances/*/compose.yaml` and `shares/*` at every start, so deleting
//! `fleet.db` only loses notes and timestamps, never an agent.
//!
//! One exception: `instance_env` (and the legacy `instance_secrets` table it
//! replaces) holds encrypted values (see [`crate::envs`] and [`crate::secrets`]).
//! Those rows are *not* rebuildable from disk; losing the database loses the
//! stored tokens, and they cannot be decrypted without the key that lives
//! outside `FLEETT_ROOT`.

use crate::config::Config;
use crate::models::{
    now_iso, ChannelKind, InstanceSpec, ProviderSpec, ShareMode, ShareRef, Status,
};
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
    // v6 — per-instance container image (copy of `services.agent.image`).
    // Rolling upgrades move one instance at a time between tags.
    "ALTER TABLE instances ADD COLUMN image TEXT NOT NULL DEFAULT '';",
    // v7 — editable per-instance environment variables (issue #22). A secret
    // row holds only its AES-256-GCM blob; a non-secret row holds the
    // plaintext value and no ciphertext. Supersedes `instance_secrets`, whose
    // maps are imported into this table at startup (they cannot be migrated in
    // SQL: they are encrypted) and whose table stays readable for one release.
    r#"
CREATE TABLE IF NOT EXISTS instance_env (
    instance_id  TEXT NOT NULL,
    key          TEXT NOT NULL,
    secret       INTEGER NOT NULL DEFAULT 0,
    value        TEXT,
    nonce        BLOB,
    ciphertext   BLOB,
    updated_at   TEXT NOT NULL,
    PRIMARY KEY (instance_id, key)
);
"#,
    // v8 — reusable provider credentials (issue #25). One encrypted secret per
    // provider; an instance may reference it and draw its API key from it
    // instead of storing its own token. `provider` stays NULL for every
    // instance created before this release (no forced migration).
    r#"
CREATE TABLE IF NOT EXISTS providers (
    id           TEXT PRIMARY KEY NOT NULL,
    name         TEXT NOT NULL UNIQUE,
    kind         TEXT NOT NULL,
    base_url     TEXT NOT NULL,
    api_key_env  TEXT NOT NULL,
    nonce        BLOB NOT NULL,
    ciphertext   BLOB NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

ALTER TABLE instances ADD COLUMN provider TEXT;
CREATE INDEX IF NOT EXISTS idx_instances_provider ON instances (provider);
"#,
    // v9 — agent-to-agent message bus (issue #28). Messages are persisted so a
    // message addressed to a stopped instance waits for its next start. `hops`
    // bounds request/reply loops; `expires_at` bounds the queue lifetime.
    r#"
CREATE TABLE IF NOT EXISTS messages (
    id             TEXT PRIMARY KEY NOT NULL,
    from_instance  TEXT NOT NULL,
    to_instance    TEXT NOT NULL,
    kind           TEXT NOT NULL,
    body           TEXT NOT NULL,
    correlation_id TEXT,
    hops           INTEGER NOT NULL DEFAULT 0,
    status         TEXT NOT NULL DEFAULT 'queued',
    created_at     TEXT NOT NULL,
    expires_at     TEXT NOT NULL,
    delivered_at   TEXT,
    read_at        TEXT
);

CREATE INDEX IF NOT EXISTS idx_messages_inbox
    ON messages (to_instance, status, created_at);
CREATE INDEX IF NOT EXISTS idx_messages_outbox
    ON messages (from_instance, created_at);
CREATE INDEX IF NOT EXISTS idx_messages_correlation
    ON messages (correlation_id);
"#,
    // v10 — shared fleet memory (issue #29): key/value entries with FTS5
    // search, TTL and a full version history. Values are operator knowledge,
    // never credentials; secrets stay in `instance_env` / `providers`.
    r#"
CREATE TABLE IF NOT EXISTS memory_entries (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    namespace   TEXT NOT NULL,
    key         TEXT NOT NULL,
    value       TEXT NOT NULL,
    tags        TEXT NOT NULL DEFAULT '',
    owner       TEXT NOT NULL,
    version     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    expires_at  TEXT,
    deleted     INTEGER NOT NULL DEFAULT 0,
    UNIQUE (namespace, key)
);

CREATE INDEX IF NOT EXISTS idx_memory_namespace ON memory_entries (namespace);

CREATE TABLE IF NOT EXISTS memory_versions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id   INTEGER NOT NULL,
    namespace  TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    tags       TEXT NOT NULL,
    owner      TEXT NOT NULL,
    version    INTEGER NOT NULL,
    action     TEXT NOT NULL,
    at         TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_versions_entry ON memory_versions (entry_id, version);

CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts
    USING fts5(namespace, key, value, tags, tokenize = 'unicode61');
"#,
    // v11 — per-agent cron schedules and their run history (issue #30).
    // `schedules.next_run_at` is NULL while the schedule is disabled; the
    // background loop only reads enabled rows, so a disabled schedule never
    // affects another instance. `schedule_runs.status` is one of `running`,
    // `ok`, `error`, `timeout` or `skipped`; a `running` row left behind by a
    // crash is turned into `error` at startup.
    r#"
CREATE TABLE IF NOT EXISTS schedules (
    id           TEXT PRIMARY KEY NOT NULL,
    instance_id  TEXT NOT NULL,
    name         TEXT NOT NULL,
    cron         TEXT NOT NULL,
    prompt       TEXT NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 1,
    timezone     TEXT NOT NULL DEFAULT 'UTC',
    overlap      TEXT NOT NULL DEFAULT 'skip',
    timeout_s    INTEGER NOT NULL DEFAULT 300,
    catchup      INTEGER NOT NULL DEFAULT 0,
    last_run_at  TEXT,
    next_run_at  TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    UNIQUE (instance_id, name)
);

CREATE INDEX IF NOT EXISTS idx_schedules_instance ON schedules (instance_id, name);
CREATE INDEX IF NOT EXISTS idx_schedules_due ON schedules (enabled, next_run_at);

CREATE TABLE IF NOT EXISTS schedule_runs (
    id           TEXT PRIMARY KEY NOT NULL,
    schedule_id  TEXT NOT NULL,
    instance_id  TEXT NOT NULL,
    started_at   TEXT NOT NULL,
    finished_at  TEXT,
    status       TEXT NOT NULL,
    exit_code    INTEGER,
    output       TEXT,
    duration_ms  INTEGER,
    created_at   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_schedule_runs_schedule
    ON schedule_runs (schedule_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_schedule_runs_instance
    ON schedule_runs (instance_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_schedule_runs_status
    ON schedule_runs (status, started_at);
"#,
    // v12 — DAG pipelines (issue #31). The definition is stored as canonical
    // JSON (validated before the write); every run is a new UUID and keeps its
    // own step rows, so history is never overwritten.
    r#"
CREATE TABLE IF NOT EXISTS pipelines (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL UNIQUE,
    definition  TEXT NOT NULL,
    trigger     TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_pipelines_trigger ON pipelines (trigger, enabled);

CREATE TABLE IF NOT EXISTS pipeline_runs (
    id          TEXT PRIMARY KEY NOT NULL,
    pipeline_id TEXT NOT NULL,
    status      TEXT NOT NULL,
    trigger     TEXT NOT NULL,
    context     TEXT,
    started_at  TEXT,
    finished_at TEXT,
    error       TEXT
);

CREATE INDEX IF NOT EXISTS idx_pipeline_runs_pipeline
    ON pipeline_runs (pipeline_id, started_at);
CREATE INDEX IF NOT EXISTS idx_pipeline_runs_status ON pipeline_runs (status);

CREATE TABLE IF NOT EXISTS pipeline_steps (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id      TEXT NOT NULL,
    step_id     TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    status      TEXT NOT NULL,
    attempt     INTEGER NOT NULL DEFAULT 0,
    output      TEXT,
    error       TEXT,
    started_at  TEXT,
    finished_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_pipeline_steps_run
    ON pipeline_steps (run_id, step_id);
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

/// One row of `instance_env`.
///
/// Exactly one of the two storage shapes is filled: a secret row has `nonce`
/// and `ciphertext` set and no `value`; a non-secret row has `value` set and
/// neither blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceEnvRecord {
    pub key: String,
    pub secret: bool,
    pub value: Option<String>,
    pub nonce: Option<Vec<u8>>,
    pub ciphertext: Option<Vec<u8>>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRecord {
    pub spec: ProviderSpec,
    /// AES-GCM nonce of the stored secret.
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
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

    /// Run a read-only closure against the shared connection.
    ///
    /// Feature modules that own their tables keep their SQL next to their
    /// models instead of growing this file; the lock is held for the duration
    /// of the closure, so it must not await.
    pub fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, DbError> {
        let conn = self.lock();
        f(&conn).map_err(DbError::from)
    }

    /// Run a write closure inside one transaction, committing on success.
    pub fn with_transaction<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, DbError> {
        let mut conn = self.lock();
        let transaction = conn.transaction()?;
        let value = f(&transaction)?;
        transaction.commit()?;
        Ok(value)
    }

    pub fn get(&self, id: &str) -> Result<Option<InstanceSpec>, DbError> {
        let conn = self.lock();
        let stored = conn
            .query_row(
                "SELECT id, name, template, model, base_url, api_key_env, provider, channel, \
                 status, last_error, mem_limit_mb, cpu_limit, image, allow_lan, notes, created_at, \
                 updated_at \
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
            "SELECT id, name, template, model, base_url, api_key_env, provider, channel, status, \
             last_error, mem_limit_mb, cpu_limit, image, allow_lan, notes, created_at, \
             updated_at \
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
            "INSERT INTO instances (id, name, template, model, base_url, api_key_env, provider, \
             channel, status, last_error, mem_limit_mb, cpu_limit, image, allow_lan, notes, \
             created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                spec.id,
                spec.name,
                spec.template,
                spec.model,
                spec.base_url,
                spec.api_key_env,
                spec.provider,
                spec.channel.as_str(),
                spec.status.as_str(),
                spec.last_error,
                spec.mem_limit_mb,
                spec.cpu_limit,
                spec.image,
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
             api_key_env = ?6, provider = ?7, channel = ?8, status = ?9, last_error = ?10, \
             mem_limit_mb = ?11, cpu_limit = ?12, image = ?13, allow_lan = ?14, notes = ?15, \
             updated_at = ?16 \
             WHERE id = ?1",
            params![
                spec.id,
                spec.name,
                spec.template,
                spec.model,
                spec.base_url,
                spec.api_key_env,
                spec.provider,
                spec.channel.as_str(),
                spec.status.as_str(),
                spec.last_error,
                spec.mem_limit_mb,
                spec.cpu_limit,
                spec.image,
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

    /// Record the container image of one instance.
    ///
    /// Only the image column (and `updated_at`) move: a rolling upgrade must
    /// never rewrite the notes, the limits or the share links of the instance.
    pub fn set_image(&self, id: &str, image: &str) -> Result<(), DbError> {
        self.lock().execute(
            "UPDATE instances SET image = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, image, now_iso()],
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
        transaction.execute(
            "DELETE FROM instance_env WHERE instance_id = ?1",
            params![id],
        )?;
        // Schedules have no foreign key (a deleted instance must not keep an
        // orphan that fails on every tick), so their history is removed with
        // the instance.
        transaction.execute(
            "DELETE FROM schedule_runs WHERE instance_id = ?1",
            params![id],
        )?;
        transaction.execute("DELETE FROM schedules WHERE instance_id = ?1", params![id])?;
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

    /// Environment rows of one instance, sorted by key.
    pub fn list_instance_env(&self, id: &str) -> Result<Vec<InstanceEnvRecord>, DbError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT key, secret, value, nonce, ciphertext, updated_at FROM instance_env \
             WHERE instance_id = ?1 ORDER BY key",
        )?;
        let rows = statement
            .query_map(params![id], |row| {
                Ok(InstanceEnvRecord {
                    key: row.get(0)?,
                    secret: row.get::<_, i64>(1)? != 0,
                    value: row.get(2)?,
                    nonce: row.get(3)?,
                    ciphertext: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Replace the whole environment of one instance in a single transaction.
    ///
    /// A row that is written through this call supersedes the encrypted
    /// `instance_secrets` blob of the instance, which is dropped at the same
    /// time: two stores for the same values would let a stale token win.
    pub fn replace_instance_env(
        &self,
        id: &str,
        records: &[InstanceEnvRecord],
    ) -> Result<(), DbError> {
        let mut conn = self.lock();
        let transaction = conn.transaction()?;
        transaction.execute(
            "DELETE FROM instance_env WHERE instance_id = ?1",
            params![id],
        )?;
        for record in records {
            transaction.execute(
                "INSERT INTO instance_env (instance_id, key, secret, value, nonce, ciphertext, \
                 updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    record.key,
                    i64::from(record.secret),
                    record.value,
                    record.nonce,
                    record.ciphertext,
                    record.updated_at,
                ],
            )?;
        }
        transaction.execute(
            "DELETE FROM instance_secrets WHERE instance_id = ?1",
            params![id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Drop every environment row of one instance.
    pub fn delete_instance_env(&self, id: &str) -> Result<(), DbError> {
        self.lock().execute(
            "DELETE FROM instance_env WHERE instance_id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Number of instances that hold at least one encrypted environment value.
    pub fn secret_instance_count(&self) -> Result<usize, DbError> {
        let conn = self.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT instance_id) FROM instance_env WHERE secret = 1",
            [],
            |row| row.get(0),
        )?;
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

    /// Insert one provider and its encrypted secret.
    ///
    /// A duplicate name is a [`DbError::Conflict`], exactly like a duplicate id.
    pub fn insert_provider(
        &self,
        spec: &ProviderSpec,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<(), DbError> {
        let conn = self.lock();
        let taken = conn
            .query_row(
                "SELECT 1 FROM providers WHERE id = ?1 OR name = ?2",
                params![spec.id, spec.name],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if taken {
            return Err(DbError::Conflict(spec.name.clone()));
        }
        conn.execute(
            "INSERT INTO providers (id, name, kind, base_url, api_key_env, nonce, ciphertext, \
             created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                spec.id,
                spec.name,
                spec.kind,
                spec.base_url,
                spec.api_key_env,
                nonce,
                ciphertext,
                spec.created_at,
                spec.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Update the metadata of one provider, optionally replacing its secret.
    ///
    /// An empty `secret` tuple keeps the stored blob; a rotation passes a fresh
    /// nonce and ciphertext (never overwrite in place without a new nonce).
    pub fn update_provider(
        &self,
        spec: &ProviderSpec,
        secret: Option<(&[u8], &[u8])>,
    ) -> Result<(), DbError> {
        let conn = self.lock();
        let clash = conn
            .query_row(
                "SELECT 1 FROM providers WHERE name = ?1 AND id != ?2",
                params![spec.name, spec.id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if clash {
            return Err(DbError::Conflict(spec.name.clone()));
        }
        let updated = match secret {
            Some((nonce, ciphertext)) => conn.execute(
                "UPDATE providers SET name = ?2, kind = ?3, base_url = ?4, api_key_env = ?5, \
                 nonce = ?6, ciphertext = ?7, updated_at = ?8 WHERE id = ?1",
                params![
                    spec.id,
                    spec.name,
                    spec.kind,
                    spec.base_url,
                    spec.api_key_env,
                    nonce,
                    ciphertext,
                    spec.updated_at,
                ],
            )?,
            None => conn.execute(
                "UPDATE providers SET name = ?2, kind = ?3, base_url = ?4, api_key_env = ?5, \
                 updated_at = ?6 WHERE id = ?1",
                params![
                    spec.id,
                    spec.name,
                    spec.kind,
                    spec.base_url,
                    spec.api_key_env,
                    spec.updated_at,
                ],
            )?,
        };
        if updated == 0 {
            return Err(DbError::Conflict(spec.id.clone()));
        }
        Ok(())
    }

    /// One provider with its encrypted secret columns.
    pub fn provider(&self, id: &str) -> Result<Option<ProviderRecord>, DbError> {
        let conn = self.lock();
        let record = conn
            .query_row(
                "SELECT id, name, kind, base_url, api_key_env, nonce, ciphertext, created_at, \
                 updated_at FROM providers WHERE id = ?1",
                params![id],
                provider_record,
            )
            .optional()?;
        Ok(record)
    }

    /// Every provider, sorted by name. The secret blobs are not read.
    pub fn list_providers(&self) -> Result<Vec<ProviderSpec>, DbError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT id, name, kind, base_url, api_key_env, created_at, updated_at \
             FROM providers ORDER BY name",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(ProviderSpec {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    base_url: row.get(3)?,
                    api_key_env: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Delete one provider row. Returns `false` when the id is unknown.
    pub fn delete_provider(&self, id: &str) -> Result<bool, DbError> {
        let deleted = self
            .lock()
            .execute("DELETE FROM providers WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    }

    /// Instances that reference one provider, `(id, name)`, sorted by id.
    pub fn provider_instances(&self, provider_id: &str) -> Result<Vec<(String, String)>, DbError> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT id, name FROM instances WHERE provider = ?1 ORDER BY id")?;
        let rows = statement
            .query_map(params![provider_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Number of instances attached to each provider.
    pub fn provider_instance_counts(&self) -> Result<HashMap<String, usize>, DbError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT provider, COUNT(*) FROM instances WHERE provider IS NOT NULL \
             GROUP BY provider",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .map(|(id, count)| (id, count.max(0) as usize))
            .collect())
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
    provider: Option<String>,
    channel: String,
    status: String,
    last_error: Option<String>,
    mem_limit_mb: i64,
    cpu_limit: f64,
    image: String,
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
            provider: self.provider,
            channel: ChannelKind::parse(&self.channel).unwrap_or(ChannelKind::None),
            shares,
            mem_limit_mb: self.mem_limit_mb.clamp(0, u32::MAX as i64) as u32,
            cpu_limit: if crate::models::validate_cpu_limit(self.cpu_limit).is_ok() {
                self.cpu_limit
            } else {
                crate::models::DEFAULT_CPU_LIMIT
            },
            image: self.image,
            // Only the compose file carries the previous tag; the reconciler
            // reads it back when the instance is rendered.
            previous_image: None,
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
        provider: row.get("provider")?,
        channel: row.get("channel")?,
        status: row.get("status")?,
        last_error: row.get("last_error")?,
        mem_limit_mb: row.get("mem_limit_mb")?,
        cpu_limit: row.get("cpu_limit")?,
        image: row.get("image")?,
        allow_lan: row.get("allow_lan")?,
        notes: row.get("notes")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn provider_record(row: &Row<'_>) -> rusqlite::Result<ProviderRecord> {
    Ok(ProviderRecord {
        spec: ProviderSpec {
            id: row.get(0)?,
            name: row.get(1)?,
            kind: row.get(2)?,
            base_url: row.get(3)?,
            api_key_env: row.get(4)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        },
        nonce: row.get(5)?,
        ciphertext: row.get(6)?,
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
///
/// The image column follows the compose file: an instance whose
/// `services.agent.image` changed on disk (an upgrade, a hand edit, a restore)
/// is resynced instead of keeping a stale tag.
pub fn reconcile(cfg: &Config, db: &Db) -> Result<(), DbError> {
    let disk = store::scan_instances(cfg);
    let disk_ids: HashSet<&str> = disk.iter().map(|spec| spec.id.as_str()).collect();

    for spec in &disk {
        match db.get(&spec.id)? {
            None => db.insert_instance(spec)?,
            Some(existing) => {
                if !spec.image.is_empty() && existing.image != spec.image {
                    db.set_image(&spec.id, &spec.image)?;
                }
            }
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
            provider: None,
            channel: ChannelKind::Telegram,
            shares: vec![ShareRef {
                name: "findings".to_string(),
                mode: ShareMode::Rw,
            }],
            mem_limit_mb: DEFAULT_MEM_LIMIT_MB,
            cpu_limit: crate::models::DEFAULT_CPU_LIMIT,
            image: "hermes-agent:0.22.2-local".to_string(),
            previous_image: None,
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
        assert_eq!(stored.image, "hermes-agent:0.22.2-local");

        let mut updated = stored.clone();
        updated.name = "Alice".to_string();
        updated.image = "hermes-agent:0.23.0-local".to_string();
        updated.shares = vec![ShareRef {
            name: "archive".to_string(),
            mode: ShareMode::Ro,
        }];
        db.update_instance(&updated).expect("update");
        let stored = db.get("alice").expect("get").expect("present");
        assert_eq!(stored.name, "Alice");
        assert_eq!(stored.image, "hermes-agent:0.23.0-local");
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
            "instance_env",
            "audit_log",
            "api_tokens",
            "providers",
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
        assert!(columns.iter().any(|column| column == "image"));
        assert!(columns.iter().any(|column| column == "provider"));
    }

    #[test]
    fn providers_round_trip_and_link_instances() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");
        let now = now_iso();
        let provider = ProviderSpec {
            id: "provider-1".to_string(),
            name: "shared".to_string(),
            kind: "openai-compatible".to_string(),
            base_url: "https://provider.test/v1".to_string(),
            api_key_env: "PROVIDER_KEY".to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        db.insert_provider(&provider, &[1u8; 12], &[2u8, 3u8])
            .expect("insert provider");
        // The name is unique.
        let mut clash = provider.clone();
        clash.id = "provider-2".to_string();
        assert!(matches!(
            db.insert_provider(&clash, &[4u8; 12], &[5u8]),
            Err(DbError::Conflict(_))
        ));

        let record = db.provider("provider-1").expect("read").expect("present");
        assert_eq!(record.spec.name, "shared");
        assert_eq!(record.nonce, vec![1u8; 12]);
        assert_eq!(record.ciphertext, vec![2u8, 3u8]);
        assert!(db.provider("ghost").expect("read").is_none());
        assert_eq!(db.list_providers().expect("list").len(), 1);

        let mut alice = spec("alice");
        alice.provider = Some("provider-1".to_string());
        db.insert_instance(&alice).expect("insert alice");
        db.insert_instance(&spec("legacy")).expect("insert legacy");
        assert_eq!(
            db.provider_instance_counts()
                .expect("counts")
                .get("provider-1")
                .copied(),
            Some(1)
        );
        let linked = db.provider_instances("provider-1").expect("linked");
        assert_eq!(linked, vec![("alice".to_string(), "alice".to_string())]);
        assert_eq!(
            db.get("alice").expect("get").expect("present").provider,
            Some("provider-1".to_string())
        );

        // Detaching the instance releases the provider.
        let mut detached = db.get("alice").expect("get").expect("present");
        detached.provider = None;
        db.update_instance(&detached).expect("update");
        assert!(db
            .provider_instances("provider-1")
            .expect("linked")
            .is_empty());

        let mut renamed = provider.clone();
        renamed.name = "renamed".to_string();
        db.update_provider(&renamed, Some((&[6u8; 12], &[7u8])))
            .expect("update provider");
        let record = db.provider("provider-1").expect("read").expect("present");
        assert_eq!(record.spec.name, "renamed");
        assert_eq!(record.ciphertext, vec![7u8]);

        assert!(db.delete_provider("provider-1").expect("delete"));
        assert!(!db.delete_provider("provider-1").expect("delete again"));
        assert!(db.list_providers().expect("list").is_empty());
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
        assert_eq!(alice.image, "", "a v0 row starts without an image");
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
    fn instance_env_rows_replace_atomically_and_follow_the_instance() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");
        db.insert_instance(&spec("alice")).expect("insert");

        let record = |key: &str, secret: bool| InstanceEnvRecord {
            key: key.to_string(),
            secret,
            value: (!secret).then(|| "plain".to_string()),
            nonce: secret.then(|| vec![0u8; 12]),
            ciphertext: secret.then(|| vec![1u8, 2, 3]),
            updated_at: "2026-11-09T00:00:00Z".to_string(),
        };
        db.replace_instance_env("alice", &[record("B_KEY", true), record("A_KEY", false)])
            .expect("replace");
        let rows = db.list_instance_env("alice").expect("list");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key, "A_KEY");
        assert!(!rows[0].secret);
        assert_eq!(rows[0].value.as_deref(), Some("plain"));
        assert!(rows[0].nonce.is_none());
        assert!(rows[1].secret);
        assert!(rows[1].value.is_none());

        // A second replace drops the keys the caller left out.
        db.replace_instance_env("alice", &[record("A_KEY", false)])
            .expect("replace");
        assert_eq!(db.list_instance_env("alice").expect("list").len(), 1);
        assert_eq!(db.secret_instance_count().expect("count"), 0);

        db.replace_instance_env("alice", &[record("B_KEY", true)])
            .expect("replace");
        assert_eq!(db.secret_instance_count().expect("count"), 1);

        db.delete_instance("alice").expect("delete");
        assert!(db.list_instance_env("alice").expect("list").is_empty());
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
