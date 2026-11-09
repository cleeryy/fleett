//! Agent-to-agent message bus (issue #28).
//!
//! Messages are persisted in SQLite so a message addressed to a stopped
//! instance waits in its inbox until the instance polls it after its next
//! start. The bus is deliberately small: one row per message, a monotonic
//! `hops` counter that stops request/reply ping-pong, and a TTL that keeps an
//! abandoned queue from growing forever.
//!
//! Agents read and write the bus through the HTTP API (`fleett say`,
//! `fleett messages`, `fleett ack`). A mirror of every delivered message is
//! spooled to `$FLEETT_ROOT/bus/<recipient>/<message>.json`, mounted read-only
//! at `/opt/data/shared/bus` for agents that prefer files to HTTP.

use crate::db::{Db, DbError};
use crate::models::now_iso;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Largest accepted message body.
pub const MAX_BODY_BYTES: usize = 16 * 1024;
/// Default time a queued message stays deliverable.
pub const DEFAULT_TTL_S: u64 = 24 * 60 * 60;
/// Smallest accepted TTL.
pub const MIN_TTL_S: u64 = 30;
/// Largest accepted TTL (30 days).
pub const MAX_TTL_S: u64 = 30 * 24 * 60 * 60;
/// Default maximum number of hops of one conversation.
pub const DEFAULT_MAX_HOPS: u32 = 8;
/// Absolute maximum accepted by the API for `hops`.
pub const MAX_HOPS_CEILING: u32 = 32;
/// Default page size of an inbox.
pub const DEFAULT_LIMIT: usize = 100;
/// Largest page size.
pub const MAX_LIMIT: usize = 500;

/// Kind of message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    /// A question expecting a `result` reply.
    Ask,
    /// A one-way instruction or notification.
    Tell,
    /// The answer to an `ask`.
    Result,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Tell => "tell",
            Self::Result => "result",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ask" => Some(Self::Ask),
            "tell" => Some(Self::Tell),
            "result" => Some(Self::Result),
            _ => None,
        }
    }
}

impl std::fmt::Display for MessageKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Delivery state of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    Queued,
    Delivered,
    Read,
}

impl MessageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Delivered => "delivered",
            Self::Read => "read",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "queued" => Some(Self::Queued),
            "delivered" => Some(Self::Delivered),
            "read" => Some(Self::Read),
            _ => None,
        }
    }
}

/// One persisted message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: MessageKind,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub hops: u32,
    pub status: MessageStatus,
    pub created_at: String,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_at: Option<String>,
}

/// A message about to be queued.
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub from: String,
    pub to: String,
    pub kind: MessageKind,
    pub body: String,
    pub correlation_id: Option<String>,
    pub ttl_s: Option<u64>,
    /// Maximum hops accepted for this bus. The API reads it from the
    /// configuration; tests use a smaller value.
    pub max_hops: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("unknown instance \"{0}\"")]
    UnknownInstance(String),
    #[error("an instance cannot message itself")]
    SelfMessage,
    #[error("message body is empty")]
    EmptyBody,
    #[error("message body exceeds {MAX_BODY_BYTES} bytes")]
    BodyTooLarge,
    #[error("ttl_s must be between {MIN_TTL_S} and {MAX_TTL_S}")]
    InvalidTtl,
    #[error("message loop stopped: {0} hops reached (max {1})")]
    LoopDetected(u32, u32),
    #[error("unknown message \"{0}\"")]
    NotFound(String),
    #[error("database error: {0}")]
    Db(#[from] DbError),
}

const COLUMNS: &str = "id, from_instance, to_instance, kind, body, correlation_id, hops, status, \
                       created_at, expires_at, delivered_at, read_at";

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let kind: String = row.get(3)?;
    let status: String = row.get(7)?;
    Ok(Message {
        id: row.get(0)?,
        from: row.get(1)?,
        to: row.get(2)?,
        kind: MessageKind::parse(&kind).unwrap_or(MessageKind::Tell),
        body: row.get(4)?,
        correlation_id: row.get(5)?,
        hops: row.get::<_, i64>(6)?.max(0) as u32,
        status: MessageStatus::parse(&status).unwrap_or(MessageStatus::Queued),
        created_at: row.get(8)?,
        expires_at: row.get(9)?,
        delivered_at: row.get(10)?,
        read_at: row.get(11)?,
    })
}

/// Reject a message that would make the bus loop.
fn next_hops(db: &Db, correlation_id: Option<&str>) -> Result<u32, BusError> {
    let Some(correlation) = correlation_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(0);
    };
    let current = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COALESCE(MAX(hops), -1) FROM messages WHERE correlation_id = ?1",
            params![correlation],
            |row| row.get::<_, i64>(0),
        )
    })?;
    Ok((current + 1).max(0) as u32)
}

fn expires_at(ttl_s: Option<u64>) -> Result<String, BusError> {
    let ttl = ttl_s.unwrap_or(DEFAULT_TTL_S);
    if !(MIN_TTL_S..=MAX_TTL_S).contains(&ttl) {
        return Err(BusError::InvalidTtl);
    }
    let now = chrono::Utc::now();
    let at = now + chrono::Duration::seconds(ttl as i64);
    Ok(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Validate and persist one message.
pub fn send(db: &Db, message: NewMessage) -> Result<Message, BusError> {
    let body = message.body.trim().to_string();
    if body.is_empty() {
        return Err(BusError::EmptyBody);
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(BusError::BodyTooLarge);
    }
    if message.from == message.to {
        return Err(BusError::SelfMessage);
    }
    if !db.exists(&message.from)? {
        return Err(BusError::UnknownInstance(message.from));
    }
    if !db.exists(&message.to)? {
        return Err(BusError::UnknownInstance(message.to));
    }
    let hops = next_hops(db, message.correlation_id.as_deref())?;
    if hops >= message.max_hops {
        return Err(BusError::LoopDetected(hops, message.max_hops));
    }
    let created_at = now_iso();
    let expires_at = expires_at(message.ttl_s)?;
    let id = uuid::Uuid::new_v4().to_string();
    let correlation = message
        .correlation_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO messages (id, from_instance, to_instance, kind, body, correlation_id, \
             hops, status, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8, ?9)",
            params![
                id,
                message.from,
                message.to,
                message.kind.as_str(),
                body,
                correlation,
                hops,
                created_at,
                expires_at,
            ],
        )
    })?;
    Ok(Message {
        id,
        from: message.from,
        to: message.to,
        kind: message.kind,
        body,
        correlation_id: correlation,
        hops,
        status: MessageStatus::Queued,
        created_at,
        expires_at,
        delivered_at: None,
        read_at: None,
    })
}

/// Which side of the conversation to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxSide {
    Inbox,
    Outbox,
    All,
}

impl BoxSide {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "inbox" | "in" | "received" => Some(Self::Inbox),
            "outbox" | "out" | "sent" => Some(Self::Outbox),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// Messages of one instance, newest first, expired ones excluded.
///
/// Reading the inbox stamps `delivered_at` on the queued rows (the agent has
/// fetched them); `ack` later stamps `read_at`.
pub fn list_for_instance(
    db: &Db,
    id: &str,
    side: BoxSide,
    status: Option<MessageStatus>,
    limit: usize,
) -> Result<Vec<Message>, BusError> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let now = now_iso();
    let mut messages = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM messages \
             WHERE ((?1 = 'inbox' AND to_instance = ?2) OR (?1 = 'outbox' AND from_instance = ?2) \
                    OR (?1 = 'all' AND (to_instance = ?2 OR from_instance = ?2))) \
               AND (?3 IS NULL OR status = ?3) \
               AND expires_at > ?4 \
             ORDER BY created_at DESC, rowid DESC LIMIT ?5"
        ))?;
        let rows = statement.query_map(
            params![
                match side {
                    BoxSide::Inbox => "inbox",
                    BoxSide::Outbox => "outbox",
                    BoxSide::All => "all",
                },
                id,
                status.map(MessageStatus::as_str),
                now,
                limit as i64,
            ],
            row_to_message,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    if side != BoxSide::Outbox {
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE messages SET delivered_at = ?1 \
                 WHERE to_instance = ?2 AND status = 'queued' AND expires_at > ?1",
                params![now, id],
            )
        })?;
        for message in &mut messages {
            if message.to == id && message.status == MessageStatus::Queued {
                message.status = MessageStatus::Delivered;
                message.delivered_at = Some(now.clone());
            }
        }
    }
    Ok(messages)
}

/// Mark one inbox message as read.
pub fn ack(db: &Db, recipient: &str, message_id: &str) -> Result<Message, BusError> {
    let now = now_iso();
    let changed = db.with_conn(|conn| {
        conn.execute(
            "UPDATE messages SET status = 'read', read_at = ?1 \
             WHERE id = ?2 AND to_instance = ?3",
            params![now, message_id, recipient],
        )
    })?;
    if changed == 0 {
        return Err(BusError::NotFound(message_id.to_string()));
    }
    get(db, message_id)?.ok_or_else(|| BusError::NotFound(message_id.to_string()))
}

pub fn get(db: &Db, id: &str) -> Result<Option<Message>, BusError> {
    let message = db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM messages WHERE id = ?1"),
            params![id],
            row_to_message,
        )
        .optional()
    })?;
    Ok(message)
}

/// Administrative view: newest first, with optional filters.
pub fn list_all(
    db: &Db,
    instance: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    kind: Option<MessageKind>,
    limit: usize,
) -> Result<Vec<Message>, BusError> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let now = now_iso();
    let messages = db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM messages \
             WHERE (?1 IS NULL OR from_instance = ?1 OR to_instance = ?1) \
               AND (?2 IS NULL OR from_instance = ?2) \
               AND (?3 IS NULL OR to_instance = ?3) \
               AND (?4 IS NULL OR kind = ?4) \
               AND expires_at > ?5 \
             ORDER BY created_at DESC, rowid DESC LIMIT ?6"
        ))?;
        let rows = statement.query_map(
            params![
                instance,
                from,
                to,
                kind.map(MessageKind::as_str),
                now,
                limit as i64
            ],
            row_to_message,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    Ok(messages)
}

/// Delete expired messages, returning how many rows were removed.
pub fn purge_expired(db: &Db) -> Result<usize, BusError> {
    let now = now_iso();
    let removed = db.with_conn(|conn| {
        conn.execute("DELETE FROM messages WHERE expires_at <= ?1", params![now])
    })?;
    Ok(removed)
}

/// Mirror a message under `$FLEETT_ROOT/bus/<recipient>/`.
///
/// Best effort: a write failure never fails the API call, the message is
/// already in the database.
pub fn spool(root: &Path, message: &Message) -> std::io::Result<()> {
    let dir = root.join("bus").join(&message.to);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", message.id));
    let body = serde_json::to_vec_pretty(message).map_err(std::io::Error::other)?;
    std::fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        ChannelKind, InstanceSpec, Status, DEFAULT_CPU_LIMIT, DEFAULT_MEM_LIMIT_MB,
    };

    fn db() -> Db {
        let dir = tempfile::tempdir().expect("temp dir");
        // Leak the directory: the database handle outlives this function.
        let path = dir.keep().join("fleet.db");
        Db::open(&path).expect("open")
    }

    fn spec(id: &str) -> InstanceSpec {
        InstanceSpec {
            id: id.to_string(),
            name: id.to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: "HERMES_GATE_PROFILE_TOKEN".to_string(),
            provider: None,
            channel: ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: DEFAULT_MEM_LIMIT_MB,
            cpu_limit: DEFAULT_CPU_LIMIT,
            image: "img:1".to_string(),
            previous_image: None,
            allow_lan: false,
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            disk_quota_mb: 0,
            disk_policy: crate::models::DiskPolicy::Warn,
            notes: String::new(),
            host: crate::hosts::LOCAL_HOST.to_string(),
            status: Status::Running,
            last_error: None,
            created_at: now_iso(),
            updated_at: now_iso(),
        }
    }

    fn message(from: &str, to: &str, body: &str, correlation: Option<&str>) -> NewMessage {
        NewMessage {
            from: from.to_string(),
            to: to.to_string(),
            kind: MessageKind::Ask,
            body: body.to_string(),
            correlation_id: correlation.map(str::to_string),
            ttl_s: None,
            max_hops: 3,
        }
    }

    #[test]
    fn a_message_is_queued_and_delivered() {
        let db = db();
        db.insert_instance(&spec("probe")).expect("insert");
        db.insert_instance(&spec("tchouniz")).expect("insert");
        let sent = send(&db, message("probe", "tchouniz", "  hello  ", None)).expect("send");
        assert_eq!(sent.body, "hello");
        assert_eq!(sent.status, MessageStatus::Queued);
        assert_eq!(sent.hops, 0);

        let inbox = list_for_instance(&db, "tchouniz", BoxSide::Inbox, None, 10).expect("inbox");
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].status, MessageStatus::Delivered);
        assert!(inbox[0].delivered_at.is_some());

        let read = ack(&db, "tchouniz", &sent.id).expect("ack");
        assert_eq!(read.status, MessageStatus::Read);
        assert!(read.read_at.is_some());
        assert!(
            ack(&db, "probe", &sent.id).is_err(),
            "only the recipient acks"
        );
    }

    #[test]
    fn a_conversation_stops_at_max_hops() {
        let db = db();
        db.insert_instance(&spec("a")).expect("insert");
        db.insert_instance(&spec("b")).expect("insert");
        for hops in 0..3 {
            let mut next = message("a", "b", "ping", Some("conv"));
            next.max_hops = 3;
            send(&db, next).expect("send");
            assert_eq!(
                db.with_conn(|conn| conn.query_row(
                    "SELECT COUNT(*) FROM messages WHERE correlation_id = 'conv' AND hops = ?1",
                    params![hops],
                    |row| row.get::<_, i64>(0)
                ))
                .expect("count"),
                1
            );
        }
        let err = send(&db, message("a", "b", "ping", Some("conv"))).expect_err("loop");
        assert!(matches!(err, BusError::LoopDetected(3, 3)), "{err}");
    }

    #[test]
    fn invalid_messages_are_refused() {
        let db = db();
        db.insert_instance(&spec("a")).expect("insert");
        db.insert_instance(&spec("b")).expect("insert");
        assert!(matches!(
            send(&db, message("a", "a", "x", None)),
            Err(BusError::SelfMessage)
        ));
        assert!(matches!(
            send(&db, message("a", "ghost", "x", None)),
            Err(BusError::UnknownInstance(_))
        ));
        assert!(matches!(
            send(&db, message("a", "b", "   ", None)),
            Err(BusError::EmptyBody)
        ));
        let big = "x".repeat(MAX_BODY_BYTES + 1);
        assert!(matches!(
            send(&db, message("a", "b", &big, None)),
            Err(BusError::BodyTooLarge)
        ));
        let mut tiny = message("a", "b", "x", None);
        tiny.ttl_s = Some(1);
        assert!(matches!(send(&db, tiny), Err(BusError::InvalidTtl)));
    }

    #[test]
    fn expired_messages_disappear_and_are_purged() {
        let db = db();
        db.insert_instance(&spec("a")).expect("insert");
        db.insert_instance(&spec("b")).expect("insert");
        let sent = send(&db, message("a", "b", "x", None)).expect("send");
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE messages SET expires_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
                params![sent.id],
            )
        })
        .expect("expire");
        assert!(list_for_instance(&db, "b", BoxSide::Inbox, None, 10)
            .expect("inbox")
            .is_empty());
        assert_eq!(purge_expired(&db).expect("purge"), 1);
        assert!(get(&db, &sent.id).expect("get").is_none());
    }

    #[test]
    fn the_admin_view_filters_by_instance_and_kind() {
        let db = db();
        db.insert_instance(&spec("a")).expect("insert");
        db.insert_instance(&spec("b")).expect("insert");
        db.insert_instance(&spec("c")).expect("insert");
        let mut tell = message("a", "b", "one", None);
        tell.kind = MessageKind::Tell;
        send(&db, tell).expect("send");
        send(&db, message("b", "c", "two", None)).expect("send");

        let all = list_all(&db, None, None, None, None, 10).expect("all");
        assert_eq!(all.len(), 2);
        let only_b = list_all(&db, Some("b"), None, None, None, 10).expect("b");
        assert_eq!(only_b.len(), 2);
        let asks = list_all(&db, None, None, None, Some(MessageKind::Ask), 10).expect("asks");
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].body, "two");
    }
}
