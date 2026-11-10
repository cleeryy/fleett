//! Integration tests for outbound webhooks (issue #46).
//!
//! Every test runs against `MockDriver`; deliveries go to a local axum server
//! bound on `127.0.0.1:0`, never to the network.

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::webhooks::{self, Event};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const MODEL: &str = "deepseek-v4.1-flash";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const WEBHOOK_SECRET: &str = "integration-secret";

struct Harness {
    /// Held so the temporary data root outlives the state.
    _dir: tempfile::TempDir,
    state: AppState,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        let state = AppState::bootstrap(cfg, Arc::new(MockDriver::new())).expect("bootstrap state");
        Self { _dir: dir, state }
    }

    /// Same harness without a secrets key: encrypted secrets cannot be stored.
    fn keyless() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().to_path_buf());
        let state = AppState::bootstrap(cfg, Arc::new(MockDriver::new())).expect("bootstrap state");
        Self { _dir: dir, state }
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
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

// ---------------------------------------------------------------------------
// Local receiver
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Captured {
    headers: HeaderMap,
    body: Vec<u8>,
}

type SinkLog = Arc<Mutex<Vec<Captured>>>;

#[derive(Clone)]
struct SinkState {
    log: SinkLog,
    status: StatusCode,
}

async fn sink_handler(
    State(sink): State<SinkState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    sink.log.lock().unwrap().push(Captured {
        headers,
        body: body.to_vec(),
    });
    sink.status
}

/// Start a local endpoint and return its URL plus the captured requests.
async fn start_sink(status: StatusCode) -> (String, SinkLog) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind sink");
    let addr = listener.local_addr().expect("local addr");
    let log: SinkLog = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().fallback(sink_handler).with_state(SinkState {
        log: log.clone(),
        status,
    });
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/hook"), log)
}

fn header_value(request: &Captured, name: &str) -> String {
    request
        .headers
        .get(name)
        .unwrap_or_else(|| panic!("missing header {name}: {:?}", request.headers))
        .to_str()
        .expect("header text")
        .to_string()
}

async fn wait_for_requests(log: &SinkLog, count: usize) {
    for _ in 0..100 {
        if log.lock().unwrap().len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "expected {count} request(s), received {}",
        log.lock().unwrap().len()
    );
}

/// Wait for one request carrying a given `X-Fleett-Event` value.
async fn wait_for_event(log: &SinkLog, event: &str) -> Captured {
    for _ in 0..100 {
        let found = log
            .lock()
            .unwrap()
            .iter()
            .find(|request| {
                request
                    .headers
                    .get("x-fleett-event")
                    .and_then(|value| value.to_str().ok())
                    == Some(event)
            })
            .cloned();
        if let Some(request) = found {
            return request;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no delivery with event {event}");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn create_webhook(harness: &Harness, url: &str, events: &[&str]) -> Value {
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/webhooks",
            Some(json!({
                "name": "integration",
                "url": url,
                "events": events,
                "secret": WEBHOOK_SECRET,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "create webhook failed: {body}");
    parse(&body)
}

async fn create_instance(harness: &Harness, id: &str) {
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": id,
                "name": format!("Agent {id}"),
                "template": "guest",
                "model": MODEL,
                "base_url": BASE_URL,
                "shares": [],
                "mem_limit_mb": 1024,
            })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create instance failed: {body}"
    );
}

// ---------------------------------------------------------------------------
// API CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhooks_are_created_read_updated_and_deleted_without_their_secret() {
    let harness = Harness::new();

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/webhooks",
            Some(json!({
                "name": "deploy",
                "url": "https://example.test/hook",
                "events": ["instance.created", "upgrade.*"],
                "secret": "top-secret-value",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created = parse(&body);
    let id = created["id"].as_str().expect("id").to_string();
    assert_eq!(created["has_secret"], true);
    assert_eq!(created["enabled"], true);
    assert_eq!(created["events"], json!(["instance.created", "upgrade.*"]));
    assert!(created.get("secret").is_none());
    assert!(
        !body.contains("top-secret-value"),
        "the secret leaked in the create response: {body}"
    );

    let (status, body) = harness.request(Method::GET, "/api/webhooks", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("top-secret-value"));
    let list = parse(&body);
    assert_eq!(list.as_array().expect("array").len(), 1);

    let (status, body) = harness
        .request(Method::GET, &format!("/api/webhooks/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("top-secret-value"));
    assert_eq!(parse(&body)["name"], "deploy");

    let (status, body) = harness
        .request(
            Method::PATCH,
            &format!("/api/webhooks/{id}"),
            Some(json!({ "events": ["env.changed"], "enabled": false, "secret": "rotated" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let patched = parse(&body);
    assert_eq!(patched["events"], json!(["env.changed"]));
    assert_eq!(patched["enabled"], false);
    assert!(!body.contains("rotated"));

    let (status, _) = harness
        .request(Method::DELETE, &format!("/api/webhooks/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = harness
        .request(Method::GET, &format!("/api/webhooks/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn creating_a_webhook_needs_a_secrets_key() {
    let harness = Harness::keyless();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/webhooks",
            Some(json!({
                "name": "deploy",
                "url": "https://example.test/hook",
                "events": ["*"],
                "secret": "top-secret-value",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        parse(&body)["error"]["code"],
        fleett::secrets::MISSING_KEY_CODE
    );
}

#[tokio::test]
async fn invalid_urls_and_events_are_refused() {
    let harness = Harness::new();
    for payload in [
        json!({"name":"x","url":"ftp://example.test","events":["*"],"secret":"s"}),
        json!({"name":"x","url":"https://example.test","events":["nope"],"secret":"s"}),
        json!({"name":"x","url":"https://example.test","events":[],"secret":"s"}),
        json!({"name":"x","url":"https://example.test","events":["*"],"secret":""}),
    ] {
        let (status, body) = harness
            .request(Method::POST, "/api/webhooks", Some(payload.clone()))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload} -> {body}");
        assert_eq!(parse(&body)["error"]["code"], "invalid_webhook");
    }
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_test_endpoint_sends_a_signed_delivery() {
    let harness = Harness::new();
    let (url, log) = start_sink(StatusCode::NO_CONTENT).await;
    let hook = create_webhook(&harness, &url, &["*"]).await;
    let id = hook["id"].as_str().expect("id");

    let (status, body) = harness
        .request(Method::POST, &format!("/api/webhooks/{id}/test"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let result = parse(&body);
    assert_eq!(result["ok"], true);
    assert_eq!(result["status"], json!(204));

    wait_for_requests(&log, 1).await;
    let captured = log.lock().unwrap()[0].clone();
    assert_eq!(header_value(&captured, "x-fleett-event"), "webhook.test");
    assert_eq!(header_value(&captured, "x-fleett-delivery"), "1");
    let expected = webhooks::sign(WEBHOOK_SECRET, &captured.body);
    assert_eq!(header_value(&captured, "x-fleett-signature"), expected);
    let envelope: Value = serde_json::from_slice(&captured.body).expect("JSON envelope");
    assert_eq!(envelope["event"], "webhook.test");
    assert_eq!(envelope["data"]["webhook"], id);

    let (status, body) = harness
        .request(Method::GET, &format!("/api/webhooks/{id}/deliveries"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let rows = parse(&body);
    assert_eq!(rows[0]["status"], "sent");
    assert_eq!(rows[0]["attempts"], json!(1));
    assert_eq!(rows[0]["response_status"], json!(204));
}

#[tokio::test]
async fn a_failing_endpoint_is_retried_three_times_and_recorded() {
    let harness = Harness::new();
    let (url, log) = start_sink(StatusCode::INTERNAL_SERVER_ERROR).await;
    let hook = create_webhook(&harness, &url, &["instance.*"]).await;
    let id = hook["id"].as_str().expect("id").to_string();

    // Empty backoff: the production delays are exercised by RETRY_DELAYS, the
    // retry loop and its bookkeeping by this call.
    let ids = webhooks::dispatch(
        &harness.state,
        Event::InstanceStarted,
        json!({"id": "alice"}),
        &[],
    )
    .await;
    assert_eq!(ids.len(), 1);
    assert_eq!(log.lock().unwrap().len(), webhooks::MAX_ATTEMPTS);

    let delivery = webhooks::delivery(&harness.state.db, ids[0])
        .expect("read delivery")
        .expect("delivery exists");
    assert_eq!(delivery.status, "failed");
    assert_eq!(delivery.attempts, webhooks::MAX_ATTEMPTS as u32);
    assert_eq!(delivery.response_status, Some(500));
    assert!(delivery
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("500"));

    let (status, body) = harness
        .request(Method::GET, &format!("/api/webhooks/{id}/deliveries"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let rows = parse(&body);
    assert_eq!(rows[0]["status"], "failed");
    assert_eq!(rows[0]["attempts"], json!(webhooks::MAX_ATTEMPTS));
}

#[tokio::test]
async fn restarting_an_instance_posts_a_signed_event() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    let (url, log) = start_sink(StatusCode::NO_CONTENT).await;
    let hook = create_webhook(&harness, &url, &["instance.*"]).await;
    let id = hook["id"].as_str().expect("id").to_string();

    let (status, body) = harness
        .request(Method::POST, "/api/instances/alice/restart", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // `instance.created` may also reach this subscription (the create event is
    // emitted in a spawned task); pick the restart delivery specifically.
    let captured = wait_for_event(&log, "instance.started").await;
    assert_eq!(
        header_value(&captured, "x-fleett-event"),
        "instance.started"
    );
    assert_eq!(
        header_value(&captured, "x-fleett-signature"),
        webhooks::sign(WEBHOOK_SECRET, &captured.body)
    );
    let envelope: Value = serde_json::from_slice(&captured.body).expect("JSON envelope");
    assert_eq!(envelope["event"], "instance.started");
    assert_eq!(envelope["data"]["id"], "alice");
    assert_eq!(envelope["data"]["action"], "restart");

    // The delivery row reaches `sent` in the spawned task.
    for _ in 0..100 {
        let rows = webhooks::deliveries(&harness.state.db, &id, 20).expect("deliveries");
        if rows
            .iter()
            .any(|row| row.event == "instance.started" && row.status == "sent")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the restart delivery never reached the sent state");
}

#[tokio::test]
async fn an_event_matching_no_subscription_creates_no_delivery() {
    let harness = Harness::new();
    let (url, log) = start_sink(StatusCode::NO_CONTENT).await;
    let hook = create_webhook(&harness, &url, &["upgrade.*"]).await;
    let id = hook["id"].as_str().expect("id").to_string();

    let ids = webhooks::dispatch(&harness.state, Event::EnvChanged, json!({}), &[]).await;
    assert!(ids.is_empty());
    assert!(log.lock().unwrap().is_empty());
    assert!(webhooks::deliveries(&harness.state.db, &id, 10)
        .expect("deliveries")
        .is_empty());
}

// ---------------------------------------------------------------------------
// OpenAPI
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_openapi_document_describes_every_api_route() {
    let harness = Harness::new();
    let (status, body) = harness
        .request_as(Method::GET, "/api/openapi.json", None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "the document must be public");
    let doc = parse(&body);
    assert!(doc["openapi"]
        .as_str()
        .unwrap_or_default()
        .starts_with("3.0"));
    assert!(doc["info"]["title"].is_string());
    let paths = doc["paths"].as_object().expect("paths object");
    assert!(paths.len() > 40, "only {} paths", paths.len());
    assert!(paths.contains_key("/api/webhooks"));
    assert!(paths.contains_key("/api/webhooks/{id}/deliveries"));
    for (path, operations) in paths {
        assert!(path.starts_with("/api"), "unexpected path {path}");
        for (method, operation) in operations.as_object().expect("operation object") {
            let responses = operation["responses"]
                .as_object()
                .unwrap_or_else(|| panic!("{method} {path} has no responses"));
            assert!(
                !responses.is_empty(),
                "{method} {path} has an empty responses object"
            );
        }
    }
    // The served bytes are the embedded document with `info.version` replaced
    // by the running binary version (see `api::openapi::document_json`).
    let served = parse(&body);
    let embedded: serde_json::Value =
        serde_json::from_str(api::openapi::DOCUMENT).expect("embedded document");
    assert_eq!(served["paths"], embedded["paths"]);
    assert_eq!(served["components"], embedded["components"]);
    assert_eq!(
        served["info"]["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}
