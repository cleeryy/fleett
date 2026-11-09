//! Deployment journal (issue #39).
//!
//! Every upgrade — immediate, canary or scheduled — gets one row before it
//! touches Docker and the row is updated at each phase. The row survives the
//! control plane: the in-memory job of `GET /api/upgrade` is lost on a restart,
//! the journal is not, so an operator can still answer "what was deployed
//! yesterday, and did it roll back?".
//!
//! `result` holds the per-instance outcome (the `results` array of the job).
//! While a deployment is `scheduled` it holds the deferred request instead,
//! under `{"request": {...}}`: the table keeps one row and no extra column for
//! the `parallel`/`timeout_s`/`window` parameters (see [`Deployment::request`]).

use crate::db::{Db, DbError};
use crate::models::now_iso;
use rusqlite::{params, OptionalExtension, Row};
use serde_json::{json, Value};

/// Accepted values of the `status` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStatus {
    /// Accepted, not started yet (image pull in progress or job queued).
    Pending,
    /// The main (post-canary) phase is moving the remaining instances.
    Running,
    /// The canary instances are being upgraded or watched during the soak.
    Canary,
    /// Every instance runs the target image.
    Ok,
    /// At least one instance failed without a usable rollback.
    Failed,
    /// At least one instance was restored to its previous image.
    RolledBack,
    /// Never started: maintenance window closed, invalid request, ...
    Cancelled,
    /// Stored and waiting for its `at` time.
    Scheduled,
}

impl DeploymentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Canary => "canary",
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::RolledBack => "rolled_back",
            Self::Cancelled => "cancelled",
            Self::Scheduled => "scheduled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "canary" => Self::Canary,
            "ok" => Self::Ok,
            "failed" => Self::Failed,
            "rolled_back" => Self::RolledBack,
            "cancelled" => Self::Cancelled,
            "scheduled" => Self::Scheduled,
            _ => return None,
        })
    }

    /// Final states (nothing will move the row again).
    pub fn is_final(self) -> bool {
        matches!(
            self,
            Self::Ok | Self::Failed | Self::RolledBack | Self::Cancelled
        )
    }
}

impl std::fmt::Display for DeploymentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One row of `deployments`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Deployment {
    pub id: String,
    pub image: String,
    pub status: DeploymentStatus,
    /// Instance ids captured when the deployment was accepted.
    pub instances: Vec<String>,
    pub canary: usize,
    pub soak_s: u64,
    /// Planned time while `scheduled`, actual start otherwise.
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// Per-instance results once the deployment runs, the deferred request
    /// while it is `scheduled`.
    pub result: Option<Value>,
    pub error: Option<String>,
    pub rollback: bool,
    pub created_by: String,
}

impl Deployment {
    /// Deferred `POST /api/upgrade` body of a scheduled deployment.
    pub fn request(&self) -> Option<Value> {
        self.result.as_ref()?.get("request").cloned()
    }
}

/// A journal row to insert.
#[derive(Debug, Clone)]
pub struct NewDeployment {
    pub id: String,
    pub image: String,
    pub status: DeploymentStatus,
    pub instances: Vec<String>,
    pub canary: usize,
    pub soak_s: u64,
    /// Planned time (`scheduled`) or start time (immediate).
    pub started_at: Option<String>,
    pub created_by: String,
    /// Deferred request, only for `scheduled` rows.
    pub request: Option<Value>,
}

const COLUMNS: &str = "id, image, status, instances, canary, soak_s, started_at, finished_at, \
                       result, error, rollback, created_by";

pub fn insert(db: &Db, new: &NewDeployment) -> Result<(), DbError> {
    let instances = serde_json::to_string(&new.instances).unwrap_or_else(|_| "[]".to_string());
    let result = new
        .request
        .as_ref()
        .map(|request| json!({ "request": request }).to_string());
    db.with_transaction(|conn| {
        conn.execute(
            "INSERT INTO deployments (id, image, status, instances, canary, soak_s, started_at, \
             finished_at, result, error, rollback, created_by) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, NULL, 0, ?9)",
            params![
                new.id,
                new.image,
                new.status.as_str(),
                instances,
                new.canary as i64,
                new.soak_s as i64,
                new.started_at,
                result,
                new.created_by,
            ],
        )?;
        Ok(())
    })
}

pub fn get(db: &Db, id: &str) -> Result<Option<Deployment>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM deployments WHERE id = ?1"),
            params![id],
            row_to_deployment,
        )
        .optional()
    })
}

/// Most recent first, optionally filtered by status.
pub fn list(
    db: &Db,
    limit: usize,
    status: Option<DeploymentStatus>,
) -> Result<Vec<Deployment>, DbError> {
    db.with_conn(|conn| {
        let (sql, filter) = match status {
            Some(status) => (
                format!(
                    "SELECT {COLUMNS} FROM deployments WHERE status = ?1 \
                     ORDER BY COALESCE(started_at, '') DESC, rowid DESC LIMIT ?2"
                ),
                Some(status.as_str().to_string()),
            ),
            None => (
                format!(
                    "SELECT {COLUMNS} FROM deployments \
                     ORDER BY COALESCE(started_at, '') DESC, rowid DESC LIMIT ?1"
                ),
                None,
            ),
        };
        let mut statement = conn.prepare(&sql)?;
        let rows = match filter {
            Some(status) => statement
                .query_map(params![status, limit as i64], row_to_deployment)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => statement
                .query_map(params![limit as i64], row_to_deployment)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    })
}

/// Move a running deployment to another phase.
pub fn set_phase(db: &Db, id: &str, status: DeploymentStatus) -> Result<(), DbError> {
    db.with_transaction(|conn| {
        conn.execute(
            "UPDATE deployments SET status = ?2 WHERE id = ?1",
            params![id, status.as_str()],
        )?;
        Ok(())
    })
}

/// Replace the per-instance results of a running deployment.
pub fn set_result(db: &Db, id: &str, result: &Value) -> Result<(), DbError> {
    db.with_transaction(|conn| {
        conn.execute(
            "UPDATE deployments SET result = ?2 WHERE id = ?1",
            params![id, result.to_string()],
        )?;
        Ok(())
    })
}

/// Final state of a deployment that ran.
pub fn finish(
    db: &Db,
    id: &str,
    status: DeploymentStatus,
    result: &Value,
    rollback: bool,
) -> Result<(), DbError> {
    db.with_transaction(|conn| {
        conn.execute(
            "UPDATE deployments SET status = ?2, result = ?3, rollback = ?4, finished_at = ?5, \
             error = NULL WHERE id = ?1",
            params![
                id,
                status.as_str(),
                result.to_string(),
                i64::from(rollback),
                now_iso(),
            ],
        )?;
        Ok(())
    })
}

/// Terminal failure (bad image, invalid request at firing time, ...).
pub fn fail(db: &Db, id: &str, error: &str) -> Result<(), DbError> {
    db.with_transaction(|conn| {
        conn.execute(
            "UPDATE deployments SET status = 'failed', error = ?2, finished_at = ?3, \
             rollback = 0 WHERE id = ?1",
            params![id, error, now_iso()],
        )?;
        Ok(())
    })
}

/// Terminal refusal (maintenance window closed when the time came).
pub fn cancel(db: &Db, id: &str, error: &str) -> Result<(), DbError> {
    db.with_transaction(|conn| {
        conn.execute(
            "UPDATE deployments SET status = 'cancelled', error = ?2, finished_at = ?3 \
             WHERE id = ?1",
            params![id, error, now_iso()],
        )?;
        Ok(())
    })
}

/// Claim a scheduled deployment for execution.
///
/// The status guard makes the claim idempotent: two ticks racing on the same
/// row see exactly one `true`. Returns `false` when the row was no longer
/// `scheduled` (or does not exist).
pub fn mark_started(db: &Db, id: &str, started_at: &str) -> Result<bool, DbError> {
    db.with_transaction(|conn| {
        let updated = conn.execute(
            "UPDATE deployments SET status = 'pending', started_at = ?2, result = NULL, \
             error = NULL WHERE id = ?1 AND status = 'scheduled'",
            params![id, started_at],
        )?;
        Ok(updated > 0)
    })
}

/// Scheduled deployments whose due time has passed, oldest first.
pub fn due(db: &Db, now: &str) -> Result<Vec<Deployment>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM deployments \
             WHERE status = 'scheduled' AND started_at IS NOT NULL AND started_at <= ?1 \
             ORDER BY started_at LIMIT 8"
        ))?;
        let rows = statement
            .query_map(params![now], row_to_deployment)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

fn row_to_deployment(row: &Row<'_>) -> rusqlite::Result<Deployment> {
    let status: String = row.get(2)?;
    let instances: String = row.get(3)?;
    let result: Option<String> = row.get(8)?;
    Ok(Deployment {
        id: row.get(0)?,
        image: row.get(1)?,
        status: DeploymentStatus::parse(&status).unwrap_or(DeploymentStatus::Failed),
        instances: serde_json::from_str(&instances).unwrap_or_default(),
        canary: row.get::<_, i64>(4)?.max(0) as usize,
        soak_s: row.get::<_, i64>(5)?.max(0) as u64,
        started_at: row.get(6)?,
        finished_at: row.get(7)?,
        result: result.and_then(|text| serde_json::from_str(&text).ok()),
        error: row.get(9)?,
        rollback: row.get::<_, i64>(10)? != 0,
        created_by: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open");
        (dir, db)
    }

    fn new(id: &str, status: DeploymentStatus) -> NewDeployment {
        NewDeployment {
            id: id.to_string(),
            image: "agent:2".to_string(),
            status,
            instances: vec!["alice".to_string(), "bob".to_string()],
            canary: 1,
            soak_s: 600,
            started_at: Some(now_iso()),
            created_by: "tester".to_string(),
            request: None,
        }
    }

    #[test]
    fn a_row_round_trips_and_finishes() {
        let (_dir, db) = db();
        insert(&db, &new("dep-1", DeploymentStatus::Pending)).expect("insert");
        let record = get(&db, "dep-1").expect("get").expect("present");
        assert_eq!(record.status, DeploymentStatus::Pending);
        assert_eq!(record.instances, ["alice", "bob"]);
        assert_eq!(record.canary, 1);
        assert!(record.finished_at.is_none());
        assert!(!record.rollback);

        set_phase(&db, "dep-1", DeploymentStatus::Canary).expect("phase");
        assert_eq!(
            get(&db, "dep-1").expect("get").expect("present").status,
            DeploymentStatus::Canary
        );

        finish(
            &db,
            "dep-1",
            DeploymentStatus::RolledBack,
            &json!([{"id": "alice", "status": "rolled_back"}]),
            true,
        )
        .expect("finish");
        let record = get(&db, "dep-1").expect("get").expect("present");
        assert_eq!(record.status, DeploymentStatus::RolledBack);
        assert!(record.rollback);
        assert!(record.finished_at.is_some());
        assert_eq!(record.result.expect("result")[0]["id"], "alice");

        assert_eq!(list(&db, 10, None).expect("list").len(), 1);
        assert_eq!(
            list(&db, 10, Some(DeploymentStatus::Ok))
                .expect("list")
                .len(),
            0
        );
    }

    #[test]
    fn a_scheduled_row_keeps_its_request_until_claimed() {
        let (_dir, db) = db();
        let mut row = new("dep-2", DeploymentStatus::Scheduled);
        row.request = Some(json!({"image": "agent:2", "canary": 1}));
        insert(&db, &row).expect("insert");
        let record = get(&db, "dep-2").expect("get").expect("present");
        assert_eq!(
            record.request().expect("request")["image"],
            Value::String("agent:2".to_string())
        );
        assert_eq!(due(&db, "9999-01-01T00:00:00Z").expect("due").len(), 1);
        assert!(due(&db, "1970-01-01T00:00:00Z").expect("due").is_empty());

        assert!(mark_started(&db, "dep-2", &now_iso()).expect("claim"));
        assert!(!mark_started(&db, "dep-2", &now_iso()).expect("second claim"));
        let record = get(&db, "dep-2").expect("get").expect("present");
        assert_eq!(record.status, DeploymentStatus::Pending);
        assert!(record.request().is_none(), "the request is cleared");
    }

    #[test]
    fn statuses_parse_back() {
        for status in [
            DeploymentStatus::Pending,
            DeploymentStatus::Running,
            DeploymentStatus::Canary,
            DeploymentStatus::Ok,
            DeploymentStatus::Failed,
            DeploymentStatus::RolledBack,
            DeploymentStatus::Cancelled,
            DeploymentStatus::Scheduled,
        ] {
            assert_eq!(DeploymentStatus::parse(status.as_str()), Some(status));
            assert_eq!(status.to_string(), status.as_str());
        }
        assert!(DeploymentStatus::parse("bogus").is_none());
        assert!(DeploymentStatus::Ok.is_final());
        assert!(!DeploymentStatus::Running.is_final());
    }
}
