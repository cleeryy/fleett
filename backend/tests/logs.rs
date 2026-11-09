//! Integration tests of the persistent log center (issue #42).
//!
//! Every test uses `MockDriver`; ingestion is exercised both directly (through
//! `LogStore::ingest`) and through `logstore::tick`, which is the same function
//! the background task calls.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::logstore::{self, LogStore};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];

struct Harness {
    dir: tempfile::TempDir,
    state: AppState,
    mock: Arc<MockDriver>,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        Self { dir, state, mock }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        self.request_as(method, uri, body, Some(TOKEN)).await
    }

    async fn request_as(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (StatusCode, String) {
        let (status, _headers, body) = self.send(method, uri, body, token).await;
        (status, body)
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = match body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = api::router(self.state.clone())
            .oneshot(builder.body(body).expect("valid request"))
            .await
            .expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        (status, headers, String::from_utf8_lossy(&bytes).to_string())
    }

    fn ingest(&self, instance: &str, text: &str) {
        self.state
            .logstore
            .ingest(&self.state.db, instance, text)
            .expect("ingest");
    }

    async fn create_instance(&self, id: &str) {
        let body = json!({
            "id": id,
            "name": format!("Agent {id}"),
            "template": "guest",
            "model": "deepseek-v4.1-flash",
            "base_url": "https://opencode.ai/inference/go/openai/v1",
            "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
            "channel": {"type": "none"},
            "shares": [],
            "mem_limit_mb": 1024,
        });
        let (status, detail) = self
            .request(Method::POST, "/api/instances", Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{detail}");
    }
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

fn parse_entries(body: &str) -> Vec<Value> {
    parse(body)["entries"]
        .as_array()
        .expect("entries array")
        .clone()
}

#[tokio::test]
async fn search_spans_two_instances_and_sorts_by_time() {
    let harness = Harness::new();
    harness.create_instance("alice").await;
    harness.create_instance("bob").await;

    harness.ingest(
        "alice",
        "2026-11-09T10:00:00Z allowlist rejected for 10.1.20.9\n\
         2026-11-09T10:02:00Z INFO unrelated",
    );
    harness.ingest(
        "bob",
        "2026-11-09T10:01:00Z allowlist accepted for 10.1.20.10",
    );

    let (status, body) = harness
        .request(Method::GET, "/api/logs?q=allowlist", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse_entries(&body);
    assert_eq!(entries.len(), 2, "{body}");
    assert_eq!(entries[0]["instance"], "bob");
    assert_eq!(entries[1]["instance"], "alice");
    assert!(entries[0]["at"].as_str() > entries[1]["at"].as_str());
    assert_eq!(parse(&body)["count"], 2);

    let (status, body) = harness
        .request(Method::GET, "/api/logs?instances=alice", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let entries = parse_entries(&body);
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry["instance"] == "alice"));

    let (status, body) = harness
        .request(Method::GET, "/api/logs/instances/bob?q=allowlist", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_entries(&body).len(), 1);
}

#[tokio::test]
async fn secrets_are_redacted_before_storage_and_in_responses() {
    let harness = Harness::new();
    let flt = format!("flt_{}", "a".repeat(32));
    let sk = format!("sk-{}", "B".repeat(24));
    let discord = "C".repeat(30);
    harness.ingest(
        "alice",
        &format!(
            "2026-11-09T10:00:00Z ERROR token={flt} key {sk} Bearer abc.def-ghi \
             Bot {discord}\n"
        ),
    );

    let (status, body) = harness
        .request(Method::GET, "/api/logs?q=ERROR", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for secret in [&flt, &sk, "abc.def-ghi"] {
        assert!(!body.contains(secret), "{secret} leaked in {body}");
    }
    assert!(!body.contains(&discord), "{discord} leaked in {body}");
    assert!(body.contains("[REDACTED]"), "{body}");

    let stored: Vec<(String, String)> = harness
        .state
        .db
        .with_conn(|conn| {
            let mut statement = conn.prepare("SELECT message, raw FROM log_entries ORDER BY id")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect()
        })
        .expect("rows");
    assert_eq!(stored.len(), 1);
    for (message, raw) in stored {
        for secret in [&flt, &sk, "abc.def-ghi"] {
            assert!(!message.contains(secret), "{secret} in {message}");
            assert!(!raw.contains(secret), "{secret} in {raw}");
        }
        assert!(!message.contains(&discord), "{discord} in {message}");
        assert!(!raw.contains(&discord), "{discord} in {raw}");
    }
}

#[tokio::test]
async fn export_serves_json_and_csv() {
    let harness = Harness::new();
    harness.ingest(
        "alice",
        "2026-11-09T10:00:00Z ERROR export, with comma\n2026-11-09T10:01:00Z INFO ok",
    );

    let (status, headers, body) = harness
        .send(
            Method::GET,
            "/api/logs/export?format=csv",
            None,
            Some(TOKEN),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "text/csv; charset=utf-8");
    assert!(body.starts_with("at,instance,level,message\n"), "{body}");
    assert!(body.contains("\"ERROR export, with comma\"") || body.contains("export, with comma"));
    assert_eq!(body.lines().count(), 3);

    let (status, headers, body) = harness
        .send(
            Method::GET,
            "/api/logs/export?format=json&q=export",
            None,
            Some(TOKEN),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(parse(&body).as_array().expect("array").len(), 1);

    let (status, body) = harness
        .request(Method::GET, "/api/logs/export?format=xml", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "invalid_format");
}

#[tokio::test]
async fn filters_and_validation_are_enforced() {
    let harness = Harness::new();
    harness.ingest(
        "alice",
        "2026-11-09T10:00:00Z INFO one\n\
         2026-11-09T10:01:00Z WARN two\n\
         2026-11-09T10:02:00Z ERROR three",
    );

    let (status, body) = harness
        .request(Method::GET, "/api/logs?level=warn", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let entries = parse_entries(&body);
    assert_eq!(entries.len(), 2, "{body}");

    let (status, body) = harness
        .request(
            Method::GET,
            "/api/logs?since=2026-11-09T10:01:30Z&until=2026-11-09T10:03:00Z",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let entries = parse_entries(&body);
    assert_eq!(entries.len(), 1, "{body}");
    assert_eq!(entries[0]["level"], "error");

    for (uri, code) in [
        ("/api/logs?level=debug", "invalid_level"),
        ("/api/logs?limit=501", "invalid_limit"),
        ("/api/logs?since=yesterday", "invalid_since"),
        ("/api/logs?instances=Not%20An%20Id", "invalid_instance"),
    ] {
        let (status, body) = harness.request(Method::GET, uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
        assert_eq!(parse(&body)["error"]["code"], code, "{uri}: {body}");
    }

    let (status, body) = harness
        .request(Method::GET, "/api/logs?q=AND%20(", None)
        .await;
    // A syntactically invalid FTS query is not an API error: LIKE takes over.
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["count"], 0);
}

#[tokio::test]
async fn tick_tails_the_mock_driver_and_deduplicates() {
    let harness = Harness::new();
    harness.create_instance("alice").await;
    harness.mock.set_logs(
        "alice",
        "2026-11-09T10:00:00Z ERROR tick-one\n\
         2026-11-09T10:00:01Z WARN tick-two\n",
    );

    let first = logstore::tick(&harness.state).await;
    assert_eq!(first.instances, 1);
    assert_eq!(first.inserted, 2, "{first:?}");
    assert_eq!(first.errors, 0);

    let second = logstore::tick(&harness.state).await;
    assert_eq!(second.inserted, 0, "{second:?}");
    assert_eq!(second.duplicates, 2, "{second:?}");
    assert!(second.purged.is_none(), "the hourly purge is not due yet");

    let (status, body) = harness
        .request(Method::GET, "/api/logs?q=tick-one", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let entries = parse_entries(&body);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["instance"], "alice");
}

#[tokio::test]
async fn sse_stream_delivers_a_new_line_in_under_a_second() {
    let harness = Harness::new();
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/logs/stream?instances=alice")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .expect("request");

    let response = api::router(harness.state.clone())
        .oneshot(request)
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );

    let mut body = response.into_body();
    harness.ingest("alice", "2026-11-09T10:00:00Z ERROR allowlist live");

    let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
        .await
        .expect("an SSE frame before one second")
        .expect("a frame")
        .expect("valid frame");
    let data = frame.into_data().expect("data frame");
    let text = String::from_utf8_lossy(&data);
    assert!(text.contains("event: log"), "{text}");
    assert!(text.contains("allowlist live"), "{text}");
}

#[tokio::test]
async fn log_routes_require_authentication() {
    let harness = Harness::new();
    for uri in [
        "/api/logs",
        "/api/logs/export",
        "/api/logs/stream",
        "/api/logs/instances/alice",
    ] {
        let (status, body) = harness.request_as(Method::GET, uri, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}: {body}");
    }
    assert!(harness.root().join("instances").is_dir());
    let _: &Arc<LogStore> = &harness.state.logstore;
}
