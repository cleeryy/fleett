//! SQLite index of the fleet.
//!
//! The database caches what the filesystem already says. It is reconciled from
//! `instances/*/compose.yaml` and `shares/*` at every start, so deleting
//! `fleet.db` only loses notes and timestamps, never an agent.

use crate::config::Config;
use crate::models::{now_iso, ChannelKind, InstanceSpec, ShareMode, ShareRef, Status};
use crate::store;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

const SCHEMA: &str = r#"
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
"#;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
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

/// Handle to the index database.
#[derive(Clone)]
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
        self.lock().execute_batch(SCHEMA)?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<InstanceSpec>, DbError> {
        let conn = self.lock();
        let stored = conn
            .query_row(
                "SELECT id, name, template, model, base_url, api_key_env, channel, status, \
                 last_error, mem_limit_mb, notes, created_at, updated_at \
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
             last_error, mem_limit_mb, notes, created_at, updated_at \
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
             status, last_error, mem_limit_mb, notes, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
             notes = ?11, updated_at = ?12 WHERE id = ?1",
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

    pub fn delete_instance(&self, id: &str) -> Result<(), DbError> {
        let mut conn = self.lock();
        let transaction = conn.transaction()?;
        transaction.execute(
            "DELETE FROM share_links WHERE instance_id = ?1",
            params![id],
        )?;
        transaction.execute("DELETE FROM instances WHERE id = ?1", params![id])?;
        transaction.commit()?;
        Ok(())
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
}
