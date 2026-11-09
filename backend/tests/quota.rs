//! Integration tests for per-instance disk quotas (issue #47).
//!
//! Every test runs against `MockDriver`; the quota loop is called directly, so
//! no background task and no Docker is involved.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::driver::UsageSample;
use fleett::quota::{self, assess, DiskLevel};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const MIB: u64 = 1024 * 1024;

struct Harness {
    dir: tempfile::TempDir,
    state: AppState,
    mock: Arc<MockDriver>,
}

impl Harness {
    fn new() -> Self {
        Self::with_config(|_| {})
    }

    fn with_config(configure: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        configure(&mut cfg);
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        Self { dir, state, mock }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn data_dir(&self, id: &str) -> PathBuf {
        self.root().join("instances").join(id).join("data")
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = builder
            .body(match body {
                Some(value) => Body::from(value.to_string()),
                None => Body::empty(),
            })
            .expect("valid request");
        let response = api::router(self.state.clone())
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    async fn request(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (status, body) = self.send(method, uri, body, Some(TOKEN)).await;
        (
            status,
            serde_json::from_str(&body).unwrap_or(Value::String(body)),
        )
    }

    async fn create(&self, id: &str) {
        let body = json!({
            "id": id,
            "name": format!("Agent {id}"),
            "template": "guest",
            "model": MODEL,
            "base_url": BASE_URL,
            "channel": {"type": "none"},
        });
        let (status, response) = self
            .send(Method::POST, "/api/instances", Some(body), Some(TOKEN))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{response}");
    }

    async fn set_quota(&self, id: &str, quota_mb: u32, policy: &str) {
        let (status, body) = self
            .request(
                Method::PUT,
                &format!("/api/instances/{id}/quota"),
                Some(json!({"quota_mb": quota_mb, "policy": policy})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    fn force_used(&self, id: &str, used_bytes: u64) {
        self.mock.force_usage(
            id,
            UsageSample {
                running: true,
                cpu_pct: 1.0,
                mem_used_bytes: 1024,
                mem_limit_bytes: 2048,
                disk_bytes: used_bytes,
            },
        );
    }

    fn events(&self, id: &str) -> Vec<fleett::quota::DiskEvent> {
        quota::list_events(&self.state.db, Some(id), 100).expect("events")
    }
}

fn write(path: &Path, bytes: usize) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent");
    }
    std::fs::write(path, vec![b'x'; bytes]).expect("write");
}

/// A rotated log old enough for the cleanup (30 days).
fn write_old_log(path: &Path, bytes: usize) {
    write(path, bytes);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open");
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 24 * 3600);
    file.set_modified(when).expect("set mtime");
}

#[tokio::test]
async fn quota_reads_default_to_unlimited_and_writes_persist() {
    let harness = Harness::new();
    harness.create("alice").await;

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/quota", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["quota_mb"], 0);
    assert_eq!(body["policy"], "warn");
    assert_eq!(body["state"], "ok");
    assert_eq!(body["warn_percent"], 80);

    harness.set_quota("alice", 100, "freeze").await;
    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/quota", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["quota_mb"], 100);
    assert_eq!(body["policy"], "freeze");
    assert_eq!(body["quota_bytes"], 100 * MIB);
    let stored = harness.state.db.get("alice").expect("get").expect("row");
    assert_eq!(stored.disk_quota_mb, 100);
    assert_eq!(stored.disk_policy, fleett::models::DiskPolicy::Freeze);

    let actions: Vec<String> = harness
        .state
        .db
        .list_audit(10)
        .expect("audit")
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert!(actions.contains(&"instance.quota.update".to_string()));
}

#[tokio::test]
async fn quota_writes_require_an_admin_and_valid_values() {
    let harness = Harness::new();
    harness.create("alice").await;
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/tokens",
            Some(json!({"name": "read-only", "role": "viewer"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let viewer = body["token"].as_str().expect("token").to_string();

    let (status, _) = harness
        .send(
            Method::PUT,
            "/api/instances/alice/quota",
            Some(json!({"quota_mb": 100})),
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = harness
        .send(
            Method::GET,
            "/api/instances/alice/quota",
            None,
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "reads are open to viewers");

    for body in [
        json!({}),
        json!({"quota_mb": 1_048_577}),
        json!({"policy": "explode"}),
    ] {
        let (status, body) = harness
            .request(Method::PUT, "/api/instances/alice/quota", Some(body))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    let (status, _) = harness
        .request(Method::GET, "/api/instances/absent/quota", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_warning_fires_once_per_crossing() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.set_quota("alice", 100, "freeze").await;
    harness.force_used("alice", 85 * MIB);

    let events = quota::tick(&harness.state).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].action, "warn");
    assert_eq!(events[0].before_bytes, 85 * MIB);
    assert!(!harness
        .mock
        .calls()
        .iter()
        .any(|call| call.operation == "stop"));

    let again = quota::tick(&harness.state).await;
    assert!(again.is_empty(), "the same crossing must not warn twice");
    assert_eq!(harness.events("alice").len(), 1);

    // Back below 80 % resets the crossing, so a second warning is emitted.
    harness.force_used("alice", 10 * MIB);
    assert!(quota::tick(&harness.state).await.is_empty());
    harness.force_used("alice", 85 * MIB);
    let events = quota::tick(&harness.state).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].action, "warn");
}

#[tokio::test]
async fn freeze_stops_the_instance_once_and_records_the_error() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.set_quota("alice", 100, "freeze").await;
    harness.force_used("alice", 150 * MIB);

    let events = quota::tick(&harness.state).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].action, "freeze");
    let stops = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| call.operation == "stop")
        .count();
    assert_eq!(stops, 1, "freeze must stop exactly once");

    let stored = harness.state.db.get("alice").expect("get").expect("row");
    assert_eq!(stored.status, fleett::models::Status::Stopped);
    assert!(
        stored
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("disk quota exceeded"),
        "{:?}",
        stored.last_error
    );

    // The next tick must never re-stop (no restart loop).
    let again = quota::tick(&harness.state).await;
    assert!(again.is_empty(), "{again:?}");
    let stops = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| call.operation == "stop")
        .count();
    assert_eq!(stops, 1);
}

#[tokio::test]
async fn the_warn_policy_at_100_percent_only_alerts() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.set_quota("alice", 100, "warn").await;
    harness.force_used("alice", 200 * MIB);

    let events = quota::tick(&harness.state).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].action, "warn");
    assert!(!harness
        .mock
        .calls()
        .iter()
        .any(|call| call.operation == "stop"));
    let stored = harness.state.db.get("alice").expect("get").expect("row");
    assert_ne!(stored.status, fleett::models::Status::Stopped);
}

#[tokio::test]
async fn the_stop_policy_stops_without_a_quota_error_message() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.set_quota("alice", 50, "stop").await;
    harness.force_used("alice", 60 * MIB);

    let events = quota::tick(&harness.state).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].action, "stop");
    assert_eq!(
        harness
            .mock
            .calls()
            .iter()
            .filter(|call| call.operation == "stop")
            .count(),
        1
    );
    let stored = harness.state.db.get("alice").expect("get").expect("row");
    assert_eq!(stored.status, fleett::models::Status::Stopped);
    assert!(stored.last_error.is_none());
}

#[tokio::test]
async fn usage_detail_reports_the_data_breakdown_and_image_size() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.set_quota("alice", 10, "warn").await;
    let data = harness.data_dir("alice");
    // `create` already wrote config.yaml, healthcheck.py and SOUL.md.
    let before = quota::dir_breakdown(&data);
    write(&data.join("MEMORY.md"), 10);
    write(&data.join("tmp/scratch.bin"), 20);
    write(&data.join("cache/http.bin"), 30);
    write(&data.join("logs/agent.log"), 40);

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/usage/detail", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["instance"], "alice");
    assert_eq!(
        body["data"]["total_bytes"],
        before.total_bytes + 100,
        "{body}"
    );
    assert_eq!(body["data"]["tmp_bytes"], 20);
    assert_eq!(body["data"]["cache_bytes"], 30);
    assert_eq!(body["data"]["logs_bytes"], 40);
    assert_eq!(
        body["data"]["other_bytes"],
        before.other_bytes + 10,
        "{body}"
    );
    assert_eq!(body["image"], fleett::config::DEFAULT_IMAGE);
    assert_eq!(body["image_bytes"], 128 * MIB);
    assert_eq!(body["quota"]["quota_mb"], 10);
    assert!(body.get("docker_log_path").is_none(), "{body}");
}

#[tokio::test]
async fn cleanup_frees_the_data_directory_and_journals_it() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.set_quota("alice", 1, "freeze").await;
    let data = harness.data_dir("alice");
    write(&data.join("MEMORY.md"), 1024);
    write(&data.join("tmp/scratch.bin"), 2 * MIB as usize);
    write(&data.join("cache/http.bin"), MIB as usize);
    write_old_log(&data.join("logs/agent.log.1"), MIB as usize);

    let breakdown = quota::dir_breakdown(&data);
    assert!(breakdown.total_bytes >= 4 * MIB, "{breakdown:?}");
    assert_eq!(assess(breakdown.total_bytes, 1), DiskLevel::Exceeded);

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/quota/cleanup",
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["instance"], "alice");
    assert!(body["before_bytes"].as_u64().expect("before") >= 4 * MIB);
    assert!(
        body["after_bytes"].as_u64().expect("after") < 100 * 1024,
        "{body}"
    );
    assert!(body["freed_bytes"].as_u64().expect("freed") > 3 * MIB);
    assert_eq!(body["removed_files"], 3);
    assert!(
        body["removed_paths"]
            .as_array()
            .expect("paths")
            .iter()
            .any(|path| path == "tmp"),
        "{body}"
    );

    let after = quota::dir_breakdown(&data).total_bytes;
    assert_eq!(
        assess(after, 1),
        DiskLevel::Ok,
        "cleanup restores the instance below the warning threshold"
    );
    assert!(!data.join("tmp").exists());
    assert!(data.join("MEMORY.md").is_file());
    assert_eq!(
        harness
            .events("alice")
            .first()
            .map(|event| event.action.as_str()),
        Some("clean"),
        "the cleanup is journalled"
    );
    let (status, body) = harness
        .request(Method::GET, "/api/disk/events?instance=alice", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["action"], "clean");
}

#[tokio::test]
async fn disk_events_are_paginated_and_validated() {
    let harness = Harness::new();
    harness.create("alice").await;
    for index in 0..3 {
        quota::record_event(
            &harness.state.db,
            &quota::NewDiskEvent {
                instance_id: "alice".to_string(),
                action: quota::ACTION_WARN.to_string(),
                before_bytes: index,
                after_bytes: index,
                detail: Some(format!("event {index}")),
            },
        )
        .expect("record");
    }
    let (status, body) = harness
        .request(Method::GET, "/api/disk/events?limit=2", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().expect("array").len(), 2);
    let (status, body) = harness
        .request(
            Method::GET,
            "/api/disk/events?instance=alice&limit=100",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().expect("array").len(), 3);
    let (status, _) = harness
        .request(Method::GET, "/api/disk/events?limit=0", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = harness
        .request(Method::GET, "/api/disk/events?limit=501", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_create_is_refused_when_the_host_reserve_is_not_met() {
    let harness = Harness::with_config(|cfg| {
        // Any host has less than ~8 EiB free, so the guard always trips.
        cfg.min_free_disk_mb = u64::MAX / 2;
    });
    let body = json!({
        "id": "alice",
        "name": "Alice",
        "template": "guest",
        "model": MODEL,
        "base_url": BASE_URL,
        "channel": {"type": "none"},
    });
    let (status, response) = harness
        .send(Method::POST, "/api/instances", Some(body), Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    let response: Value = serde_json::from_str(&response).expect("json");
    assert_eq!(response["error"]["code"], "insufficient_disk");
    assert!(
        response["error"]["details"]["free_mb"].is_u64(),
        "{response}"
    );
    assert_eq!(
        response["error"]["details"]["required_mb"],
        u64::MAX / 2,
        "{response}"
    );
    assert!(
        !harness
            .root()
            .join("instances")
            .join("alice")
            .join("compose.yaml")
            .exists(),
        "a refused create must leave nothing behind"
    );
}

#[tokio::test]
async fn a_create_with_a_quota_reserves_twice_that_quota() {
    let harness = Harness::with_config(|cfg| cfg.min_free_disk_mb = 0);
    let body = json!({
        "id": "alice",
        "name": "Alice",
        "template": "guest",
        "model": MODEL,
        "base_url": BASE_URL,
        "channel": {"type": "none"},
        // 1 TiB quota -> 2 TiB must stay free: no CI host has that.
        "disk_quota_mb": 1_048_576,
        "disk_policy": "freeze",
    });
    let (status, response) = harness
        .send(Method::POST, "/api/instances", Some(body), Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    let response: Value = serde_json::from_str(&response).expect("json");
    assert_eq!(response["error"]["code"], "insufficient_disk");
    assert_eq!(response["error"]["details"]["required_mb"], 2_097_152);
}

#[tokio::test]
async fn a_quota_is_kept_through_a_patch() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.set_quota("alice", 250, "stop").await;
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"notes": "quota must survive"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = harness.state.db.get("alice").expect("get").expect("row");
    assert_eq!(stored.disk_quota_mb, 250);
    assert_eq!(stored.disk_policy, fleett::models::DiskPolicy::Stop);
}
