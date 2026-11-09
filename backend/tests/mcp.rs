//! Integration tests for the MCP server (issue #45).
//!
//! Every test drives the real protocol code against a local axum mock of the
//! control plane; the stdio framing test spawns the real `fleett mcp` binary.
//! No test ever calls Docker or a real network.

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use fleett::cli::ClientConfig;
use fleett::mcp::{self, Session};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::task::JoinHandle;
use tower::ServiceExt;

const TOKEN: &str = "flt_0123456789abcdef0123456789abcdef";
const INSTANCE: &str = "alice";

// ---------------------------------------------------------------------------
// Mock control plane
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    authorization: Option<String>,
    body: Option<Value>,
}

#[derive(Clone)]
struct MockState {
    requests: Arc<Mutex<Vec<Recorded>>>,
    /// Answer every mutation with the viewer `403` envelope.
    forbidden_mutations: bool,
    /// Return a file body larger than the 64 KiB cap.
    huge_file: bool,
}

impl MockState {
    fn requests(&self) -> MutexGuard<'_, Vec<Recorded>> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct MockServer {
    base: String,
    state: MockState,
    handle: JoinHandle<()>,
}

impl MockServer {
    async fn start(forbidden_mutations: bool) -> Self {
        Self::start_with(forbidden_mutations, false).await
    }

    async fn start_with(forbidden_mutations: bool, huge_file: bool) -> Self {
        let state = MockState {
            requests: Arc::new(Mutex::new(Vec::new())),
            forbidden_mutations,
            huge_file,
        };
        let app = app(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the mock server");
        let address = listener.local_addr().expect("local address");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base: format!("http://{address}"),
            state,
            handle,
        }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.state.requests().clone()
    }

    fn request(&self, method: &str, path: &str) -> Recorded {
        self.requests()
            .into_iter()
            .find(|record| record.method == method && record.path == path)
            .unwrap_or_else(|| panic!("no {method} {path} recorded"))
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn app(state: MockState) -> Router {
    Router::new()
        .route("/api/instances", get(list_instances))
        .route("/api/instances/{id}", patch(patch_instance))
        .route("/api/instances/{id}/start", post(start_instance))
        .route("/api/instances/{id}/files/content", get(file_content))
        .route("/api/providers", get(list_providers))
        .fallback(fallback)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, record))
}

async fn record(State(state): State<MockState>, request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, 1024 * 1024)
        .await
        .unwrap_or_default();
    let query = url::form_urlencoded::parse(parts.uri.query().unwrap_or("").as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    state.requests().push(Recorded {
        method: parts.method.to_string(),
        path: parts.uri.path().to_string(),
        query,
        authorization: parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: if bytes.is_empty() {
            None
        } else {
            serde_json::from_slice(&bytes).ok()
        },
    });
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": message}})),
    )
        .into_response()
}

fn summary(id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "name": format!("Agent {id}"),
        "template": "guest",
        "model": "deepseek-v4.1-flash",
        "channel": "telegram",
        "status": status,
        "health": if status == "running" { "healthy" } else { "none" },
        "cpu_limit": 1.0,
        "allow_lan": false,
        "shares": [],
        "last_error": null,
        "created_at": "2026-11-08T01:00:00Z"
    })
}

async fn list_instances() -> Json<Value> {
    Json(json!([
        summary(INSTANCE, "running"),
        summary("bob", "stopped")
    ]))
}

async fn start_instance(State(state): State<MockState>, Path(id): Path<String>) -> Response {
    if state.forbidden_mutations {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "the viewer role is read-only",
        );
    }
    Json(json!({ "id": id, "status": "running" })).into_response()
}

async fn patch_instance(Path(id): Path<String>, Json(_body): Json<Value>) -> Json<Value> {
    Json(json!({ "id": id, "status": "running", "env": [] }))
}

async fn file_content(
    State(state): State<MockState>,
    Path(_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let path = query
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let content = if state.huge_file {
        "x".repeat(mcp::MAX_BODY_BYTES + 1024)
    } else {
        "# soul\n".to_string()
    };
    Json(json!({
        "path": path,
        "content": content,
        "etag": "a".repeat(64),
        "size": content.len(),
        "mtime": "2026-11-08T01:00:00Z",
        "editable": true,
        "rendered": false
    }))
    .into_response()
}

async fn list_providers() -> Json<Value> {
    Json(json!([]))
}

async fn fallback() -> Response {
    api_error(StatusCode::NOT_FOUND, "not_found", "route not found")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn config_for(mock: &MockServer) -> ClientConfig {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!("[server]\nurl = \"{}\"\ntoken = \"{TOKEN}\"\n", mock.base),
    )
    .expect("write config");
    // Resolving from the file proves the MCP server uses the CLI configuration.
    ClientConfig::resolve(&path, |_| None).expect("client config")
}

async fn session_for(mock: &MockServer) -> Session {
    Session::new(&config_for(mock)).expect("session")
}

fn call(name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_handshake_and_notifications() {
    let mock = MockServer::start(false).await;
    let session = session_for(&mock).await;

    let response = session
        .handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": mcp::PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            },
        }))
        .await
        .expect("initialize has a response");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["protocolVersion"], mcp::PROTOCOL_VERSION);
    assert_eq!(response["result"]["serverInfo"]["name"], "fleett");
    assert_eq!(
        response["result"]["capabilities"]["tools"]["listChanged"],
        false
    );

    let notification = session
        .handle(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    assert!(
        notification.is_none(),
        "a notification never gets a response"
    );

    let ping = session
        .handle(json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}))
        .await
        .expect("ping has a response");
    assert_eq!(ping["result"], json!({}));
}

#[tokio::test]
async fn tools_list_contains_every_required_name() {
    let mock = MockServer::start(false).await;
    let session = session_for(&mock).await;

    let response = session
        .handle(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}))
        .await
        .expect("tools/list has a response");
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools array")
        .clone();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    for required in mcp::TOOL_NAMES {
        assert!(names.contains(&required), "missing tool {required}");
    }
    assert_eq!(names.len(), mcp::TOOL_NAMES.len());
    for tool in &tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }
}

#[tokio::test]
async fn unknown_tool_and_unknown_method_are_protocol_errors() {
    let mock = MockServer::start(false).await;
    let session = session_for(&mock).await;

    let response = session
        .handle(call("run_shell", json!({"cmd": "rm -rf /"})))
        .await
        .expect("response");
    assert_eq!(response["error"]["code"], -32602);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown tool"));

    let response = session
        .handle(json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}))
        .await
        .expect("response");
    assert_eq!(response["error"]["code"], -32601);
}

// ---------------------------------------------------------------------------
// Tools against the mock API
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_instances_uses_the_api_with_the_configured_token() {
    let mock = MockServer::start(false).await;
    let session = session_for(&mock).await;

    let response = session
        .handle(call("list_instances", json!({})))
        .await
        .expect("response");
    assert_eq!(response["result"]["isError"], Value::Null);
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured.as_array().expect("array").len(), 2);
    assert_eq!(structured[0]["id"], INSTANCE);
    assert!(response["result"]["content"][0]["text"]
        .as_str()
        .expect("text")
        .contains(INSTANCE));

    let recorded = mock.request("GET", "/api/instances");
    assert_eq!(
        recorded.authorization.as_deref(),
        Some("Bearer flt_0123456789abcdef0123456789abcdef")
    );
}

#[tokio::test]
async fn read_file_returns_content_and_metadata() {
    let mock = MockServer::start(false).await;
    let session = session_for(&mock).await;

    let response = session
        .handle(call(
            "read_file",
            json!({"id": INSTANCE, "path": "/SOUL.md"}),
        ))
        .await
        .expect("response");
    assert_eq!(response["result"]["content"][0]["text"], "# soul\n");
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["path"], "/SOUL.md");
    assert_eq!(structured["editable"], true);
    assert_eq!(structured["content"], "# soul\n");
    assert_eq!(structured["truncated"], false);

    let recorded = mock.request("GET", &format!("/api/instances/{INSTANCE}/files/content"));
    assert_eq!(
        recorded.query,
        vec![("path".to_string(), "/SOUL.md".to_string())]
    );
}

#[tokio::test]
async fn env_set_sends_one_validated_merge_patch() {
    let mock = MockServer::start(false).await;
    let session = session_for(&mock).await;

    let response = session
        .handle(call(
            "env_set",
            json!({"id": INSTANCE, "key": "PROVIDER_KEY", "value": "s3cret", "secret": true}),
        ))
        .await
        .expect("response");
    assert!(response["result"]["isError"].is_null());
    assert_eq!(
        response["result"]["structuredContent"]["key"],
        "PROVIDER_KEY"
    );
    assert_eq!(response["result"]["structuredContent"]["secret"], true);
    assert!(!response["result"]["content"][0]["text"]
        .as_str()
        .expect("text")
        .contains("s3cret"));

    let recorded = mock.request("PATCH", &format!("/api/instances/{INSTANCE}"));
    assert_eq!(
        recorded.body,
        Some(json!({"env": [{"key": "PROVIDER_KEY", "value": "s3cret", "secret": true}]}))
    );
}

#[tokio::test]
async fn huge_file_bodies_are_truncated_with_a_marker() {
    let mock = MockServer::start_with(false, true).await;
    let session = session_for(&mock).await;

    let response = session
        .handle(call(
            "read_file",
            json!({"id": INSTANCE, "path": "/big.txt"}),
        ))
        .await
        .expect("response");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.ends_with(']'));
    assert!(text.contains("[truncated:"));
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["truncated"], true);
    assert!(structured.get("content").is_none());
}

#[tokio::test]
async fn a_viewer_403_becomes_a_tool_error_with_the_api_message() {
    let mock = MockServer::start(true).await;
    let session = session_for(&mock).await;

    let response = session
        .handle(call("start_instance", json!({"id": INSTANCE})))
        .await
        .expect("response");
    let result = &response["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["ok"], false);
    assert_eq!(result["structuredContent"]["error"]["code"], "forbidden");
    assert_eq!(result["structuredContent"]["error"]["status"], 403);
    assert_eq!(
        result["structuredContent"]["error"]["message"],
        "the viewer role is read-only"
    );
    assert!(result["content"][0]["text"]
        .as_str()
        .expect("text")
        .contains("the viewer role is read-only"));

    // The call really reached the API (which is where the ACL is enforced).
    mock.request("POST", &format!("/api/instances/{INSTANCE}/start"));
}

#[tokio::test]
async fn run_pipeline_reports_not_available_without_faking_a_call() {
    let mock = MockServer::start(false).await;
    let session = session_for(&mock).await;

    let response = session
        .handle(call("run_pipeline", json!({"pipeline_id": "nightly"})))
        .await
        .expect("response");
    let result = &response["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["structuredContent"]["error"]["code"],
        "not_available"
    );
    assert!(result["structuredContent"]["error"]["message"]
        .as_str()
        .expect("message")
        .contains("#31"));
    assert!(
        mock.requests().is_empty(),
        "run_pipeline must not call a made-up endpoint"
    );
}

#[tokio::test]
async fn unknown_arguments_are_rejected_before_any_call() {
    let mock = MockServer::start(false).await;
    let session = session_for(&mock).await;

    let response = session
        .handle(call(
            "logs",
            json!({"id": INSTANCE, "cmd": "tail -f /etc/passwd"}),
        ))
        .await
        .expect("response");
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"],
        "invalid_arguments"
    );
    assert!(mock.requests().is_empty());
}

// ---------------------------------------------------------------------------
// Transports
// ---------------------------------------------------------------------------

#[tokio::test]
async fn http_mode_answers_jsonrpc_and_rejects_get() {
    let mock = MockServer::start(false).await;
    let session = session_for(&mock).await;
    let app = mcp::http_router(session);

    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": mcp::PROTOCOL_VERSION },
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(init.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(value["result"]["serverInfo"]["name"], "fleett");

    // The same tools/call path as stdio: one POST, one JSON result.
    let tool_call = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(Body::from(call("list_instances", json!({})).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(tool_call.status(), StatusCode::OK);
    let bytes = tool_call
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(value["result"]["structuredContent"][0]["id"], INSTANCE);

    let notification = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(Body::from(
                    json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(notification.status(), StatusCode::ACCEPTED);

    let get = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/mcp")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn stdio_framing_is_line_delimited_jsonrpc() {
    let mock = MockServer::start(false).await;
    let session = session_for(&mock).await;

    // Two in-memory pipes: requests flow client -> server, responses server ->
    // client, exactly like stdin/stdout.
    let (mut requests, server_reader) = tokio::io::duplex(64 * 1024);
    let (server_writer, responses) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(mcp::serve_stream(session, server_reader, server_writer));
    let mut lines = BufReader::new(responses).lines();

    let requests_to_send = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": mcp::PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            },
        }),
        // A notification must not produce a response line.
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ];
    for request in &requests_to_send {
        requests
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("write request");
    }
    // A malformed line must answer a parse error on the same framing.
    requests
        .write_all(b"not json\n")
        .await
        .expect("write malformed line");
    requests.flush().await.expect("flush");

    let mut received = Vec::new();
    for _ in 0..3 {
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("response within 10s")
            .expect("io")
            .expect("one response line");
        received.push(serde_json::from_str::<Value>(&line).expect("response json"));
    }
    let init = &received[0];
    let tools = &received[1];
    let parse_error = &received[2];

    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "fleett");
    assert_eq!(tools["id"], 2);
    assert_eq!(
        tools["result"]["tools"].as_array().expect("tools").len(),
        mcp::TOOL_NAMES.len()
    );
    assert_eq!(parse_error["id"], Value::Null);
    assert_eq!(parse_error["error"]["code"], -32700);

    // EOF on the request side ends the loop.
    drop(requests);
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("serve_stream exits at EOF")
        .expect("server task")
        .expect("io");
}
