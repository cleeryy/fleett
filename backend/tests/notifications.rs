//! Integration tests for outbound notification channels (issue #33).
//!
//! Every test runs against `MockDriver`; the delivery tests talk to a local
//! `std::net` HTTP server, never to Discord or Telegram.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const MODEL: &str = "deepseek-v4.1-flash";

struct Harness {
    #[allow(dead_code)]
    dir: tempfile::TempDir,
    state: AppState,
    #[allow(dead_code)]
    mock: Arc<MockDriver>,
}

impl Harness {
    fn new() -> Self {
        Self::build(true)
    }

    /// Same harness without a secrets key: a secret-bearing channel is
    /// refused with the existing `secrets_key_missing` error.
    fn keyless() -> Self {
        Self::build(false)
    }

    fn build(with_key: bool) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        if with_key {
            cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        }
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        Self { dir, state, mock }
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        send_to(api::router(self.state.clone()), method, uri, body).await
    }

    async fn create_channel(&self, body: Value) -> Value {
        let (status, text) = self
            .request(Method::POST, "/api/notifications", Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "create channel failed: {text}");
        parse(&text)
    }

    async fn create_instance(&self, id: &str) {
        let (status, body) = self
            .request(
                Method::POST,
                "/api/instances",
                Some(json!({
                    "id": id,
                    "name": format!("Agent {id}"),
                    "template": "guest",
                    "model": MODEL,
                    "base_url": "https://opencode.ai/inference/go/openai/v1",
                    "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
                    "shares": [],
                    "mem_limit_mb": 512,
                })),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "create instance failed: {body}"
        );
    }

    async fn wait_for_delivery(&self, channel: &str, event: &str) -> Value {
        for _ in 0..100 {
            let (status, body) = self
                .request(
                    Method::GET,
                    &format!("/api/notifications/deliveries?channel={channel}"),
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "deliveries failed: {body}");
            let rows: Vec<Value> = serde_json::from_str(&body).expect("delivery list");
            if let Some(row) = rows.into_iter().find(|row| {
                row["event"] == event && (row["status"] == "sent" || row["status"] == "failed")
            }) {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no {event} delivery for channel {channel} appeared in 2.5s");
    }
}

async fn send_to(
    router: Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let request = builder.body(body).expect("valid request");
    let response = router.oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

/// Local webhook server answering 200 and counting its requests.
fn spawn_webhook() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock webhook");
    let address = listener.local_addr().expect("local address");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut buffer = [0u8; 8192];
                let _ = stream.read(&mut buffer);
                let response =
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\
                     connection: close\r\n\r\n{}";
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            });
        }
    });
    (format!("http://{address}/hook"), hits)
}

#[tokio::test]
async fn channels_crud_and_the_secret_never_leaves_the_server() {
    let harness = Harness::new();
    let created = harness
        .create_channel(json!({
            "name": "ops telegram",
            "kind": "telegram",
            "target": "424242",
            "secret": "123456:very-secret-token",
            "events": ["instance.down", "incident.*"],
        }))
        .await;
    assert_eq!(created["kind"], "telegram");
    assert_eq!(created["has_secret"], true);
    assert_eq!(created["events"], json!(["instance.down", "incident.*"]));
    assert!(created.get("secret").is_none());
    assert!(created.get("ciphertext").is_none());
    assert!(created.get("nonce").is_none());
    let id = created["id"].as_str().expect("channel id").to_string();

    let (status, body) = harness
        .request(Method::GET, "/api/notifications", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("very-secret-token"), "{body}");
    assert!(!body.contains("ciphertext"), "{body}");
    assert!(body.contains(&id), "{body}");

    let (status, body) = harness
        .request(Method::GET, &format!("/api/notifications/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("very-secret-token"), "{body}");

    let (status, body) = harness
        .request(
            Method::PATCH,
            &format!("/api/notifications/{id}"),
            Some(json!({
                "name": "ops telegram renamed",
                "events": ["*"],
                "enabled": false,
                "secret": "999:rotated-token",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let patched = parse(&body);
    assert_eq!(patched["name"], "ops telegram renamed");
    assert_eq!(patched["events"], json!(["*"]));
    assert_eq!(patched["enabled"], false);
    assert_eq!(patched["has_secret"], true);
    assert!(!body.contains("rotated-token"), "{body}");

    let (status, body) = harness
        .request(
            Method::PATCH,
            &format!("/api/notifications/{id}"),
            Some(json!({ "secret": "424242:another" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("another"), "{body}");

    let (status, body) = harness
        .request(Method::GET, "/api/audit?limit=50", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("notification.create"), "{body}");
    assert!(body.contains("notification.update"), "{body}");
    assert!(!body.contains("very-secret-token"), "{body}");
    assert!(!body.contains("rotated-token"), "{body}");

    let (status, _) = harness
        .request(Method::DELETE, &format!("/api/notifications/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = harness
        .request(Method::GET, &format!("/api/notifications/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_channel_without_a_secret_needs_no_key_but_a_secret_does() {
    let harness = Harness::keyless();
    let created = harness
        .create_channel(json!({
            "name": "plain webhook",
            "kind": "webhook",
            "target": "https://example.test/hook",
            "events": ["instance.down"],
        }))
        .await;
    assert_eq!(created["has_secret"], false);

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/notifications",
            Some(json!({
                "name": "telegram",
                "kind": "telegram",
                "target": "42",
                "secret": "123:token",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "secrets_key_missing");

    // A secret on a plain webhook is encrypted too: same refusal without key.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/notifications",
            Some(json!({
                "name": "signed webhook",
                "kind": "webhook",
                "target": "https://example.test/hook",
                "secret": "hmac-secret",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Telegram without a token is refused even when a key exists.
    let keyed = Harness::new();
    let (status, body) = keyed
        .request(
            Method::POST,
            "/api/notifications",
            Some(json!({
                "name": "telegram",
                "kind": "telegram",
                "target": "42",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_notification");
}

#[tokio::test]
async fn invalid_kind_target_and_events_are_refused() {
    let harness = Harness::new();
    for (body, code) in [
        (
            json!({"name": "x", "kind": "email", "target": "a@b"}),
            "invalid_kind",
        ),
        (
            json!({"name": "x", "kind": "webhook", "target": "not a url"}),
            "invalid_target",
        ),
        (
            json!({"name": "x", "kind": "smtp", "target": "127.0.0.1"}),
            "invalid_target",
        ),
        (
            json!({
                "name": "x",
                "kind": "webhook",
                "target": "https://example.test/hook",
                "events": ["nope"],
            }),
            "invalid_events",
        ),
        (
            json!({"name": "", "kind": "webhook", "target": "https://example.test/h"}),
            "invalid_name",
        ),
    ] {
        let (status, text) = harness
            .request(Method::POST, "/api/notifications", Some(body))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert_eq!(parse(&text)["error"]["code"], code, "{text}");
    }
}

#[tokio::test]
async fn the_test_endpoint_sends_and_records_a_delivery() {
    let harness = Harness::new();
    let (target, hits) = spawn_webhook();
    let channel = harness
        .create_channel(json!({
            "name": "hook",
            "kind": "webhook",
            "target": target,
            "events": ["instance.down"],
        }))
        .await;
    let id = channel["id"].as_str().expect("channel id");

    let (status, body) = harness
        .request(Method::POST, &format!("/api/notifications/{id}/test"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let delivery = parse(&body);
    assert_eq!(delivery["event"], "notification.test");
    assert_eq!(delivery["status"], "sent");
    assert_eq!(delivery["attempts"], 1);
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);

    let (status, body) = harness
        .request(
            Method::GET,
            &format!("/api/notifications/deliveries?channel={id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows: Vec<Value> = serde_json::from_str(&body).expect("deliveries");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "sent");
}

#[tokio::test]
async fn stopping_an_instance_emits_instance_down() {
    let harness = Harness::new();
    let (target, hits) = spawn_webhook();
    let channel = harness
        .create_channel(json!({
            "name": "hook",
            "kind": "webhook",
            "target": target,
            "events": ["instance.down"],
        }))
        .await;
    let channel_id = channel["id"].as_str().expect("channel id").to_string();

    harness.create_instance("alice").await;
    let (status, body) = harness
        .request(Method::POST, "/api/instances/alice/stop", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let delivery = harness
        .wait_for_delivery(&channel_id, "instance.down")
        .await;
    assert_eq!(delivery["status"], "sent", "{delivery}");
    assert_eq!(delivery["attempts"], 1);
    assert!(delivery["subject"]
        .as_str()
        .unwrap_or_default()
        .contains("alice"));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn deliveries_reject_an_out_of_range_limit() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::GET,
            "/api/notifications/deliveries?limit=1001",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_limit");
}
