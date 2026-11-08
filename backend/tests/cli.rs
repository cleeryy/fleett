//! Integration tests for the client mode of the `fleett` binary (issue #14).
//!
//! Every test spawns the real binary against a mock axum server; no test ever
//! calls Docker.

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fleett::cli::{TOKEN_ENV, URL_ENV};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path as FsPath;
use std::process::Output;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::task::JoinHandle;

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

impl Recorded {
    fn query(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone)]
struct MockState {
    requests: Arc<Mutex<Vec<Recorded>>>,
    docker_ok: bool,
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
    async fn start(docker_ok: bool) -> Self {
        let state = MockState {
            requests: Arc::new(Mutex::new(Vec::new())),
            docker_ok,
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
        .route("/api/instances", get(list_instances).post(create_instance))
        .route(
            "/api/instances/{id}",
            get(get_instance).delete(delete_instance),
        )
        .route("/api/instances/{id}/start", post(start_instance))
        .route("/api/instances/{id}/stop", post(stop_instance))
        .route("/api/instances/{id}/restart", post(restart_instance))
        .route("/api/instances/{id}/logs", get(logs))
        .route("/api/usage", get(usage))
        .route("/api/health", get(health))
        .route("/api/auth/me", get(me))
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

fn status_of(id: &str) -> &'static str {
    match id {
        "bob" => "stopped",
        _ => "running",
    }
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

fn detail(id: &str, status: &str) -> Value {
    let container = if status == "running" {
        json!({
            "id": "3f2a9c1d4b5e6f708192a3",
            "image": "hermes-agent:0.22.2-local",
            "uptime_s": 93784
        })
    } else {
        Value::Null
    };
    json!({
        "id": id,
        "name": format!("Agent {id}"),
        "template": "guest",
        "model": "deepseek-v4.1-flash",
        "base_url": "https://opencode.ai/inference/go/openai/v1",
        "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
        "channel": "telegram",
        "status": status,
        "health": if status == "running" { "healthy" } else { "none" },
        "shares": [],
        "mem_limit_mb": 1536,
        "cpu_limit": 1.0,
        "allow_lan": false,
        "warnings": [],
        "notes": "",
        "created_at": "2026-11-08T01:00:00Z",
        "updated_at": "2026-11-08T01:00:00Z",
        "last_error": null,
        "data_dir": format!("/opt/fleett/instances/{id}/data"),
        "env_keys": [],
        "container": container,
        "compose_yaml": format!("name: fleett-{id}\n")
    })
}

fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": message}})),
    )
        .into_response()
}

async fn list_instances() -> Json<Value> {
    Json(json!([
        summary("alice", "running"),
        summary("bob", "stopped")
    ]))
}

async fn get_instance(Path(id): Path<String>) -> Response {
    if id == "ghost" {
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "instance \"ghost\" not found",
        );
    }
    Json(detail(&id, status_of(&id))).into_response()
}

async fn create_instance(Json(body): Json<Value>) -> Response {
    let id = body["id"].as_str().unwrap_or("test");
    (StatusCode::CREATED, Json(detail(id, "running"))).into_response()
}

async fn start_instance(Path(_id): Path<String>) -> Json<Value> {
    Json(json!({"status": "running"}))
}

async fn stop_instance(Path(_id): Path<String>) -> Json<Value> {
    Json(json!({"status": "stopped"}))
}

async fn restart_instance(Path(_id): Path<String>) -> Json<Value> {
    Json(json!({"status": "running"}))
}

async fn delete_instance(Path(_id): Path<String>) -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn logs(
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
) -> Response {
    if id == "ghost" {
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "instance \"ghost\" not found",
        );
    }
    if query.get("format").map(String::as_str) == Some("json") {
        return Json(json!([
            {"time": "2026-11-08T01:02:03Z", "message": "boom"},
            {"line": "plain line"}
        ]))
        .into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "line one\nline two\n".to_string(),
    )
        .into_response()
}

async fn usage() -> Json<Value> {
    Json(json!({
        "instances": [
            {"id": "alice", "status": "running", "cpu_pct": 12.5,
             "mem_used_bytes": 536870912, "mem_limit_bytes": 1610612736,
             "disk_bytes": 12582912},
            {"id": "bob", "status": "stopped", "cpu_pct": 0.0,
             "mem_used_bytes": 0, "mem_limit_bytes": 1610612736,
             "disk_bytes": 0}
        ],
        "totals": {"cpu_pct": 12.5, "mem_used_bytes": 536870912,
                   "mem_limit_bytes": 3221225472u64, "disk_bytes": 12582912},
        "caps": {"max_cpu": 8.0, "max_mem_mb": 16384}
    }))
}

async fn health(State(state): State<MockState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": "0.3.0",
        "driver": "compose",
        "docker": {"ok": state.docker_ok},
        "uptime_s": 42,
        "secrets": {"key": true, "key_error": null, "encrypted": 2, "plaintext": 0},
        "egress": {"mode": "restrict", "marker": true}
    }))
}

async fn me() -> Json<Value> {
    Json(json!({
        "authenticated": true,
        "actor": "admin",
        "role": "admin",
        "method": "bearer"
    }))
}

// ---------------------------------------------------------------------------
// Binary runner
// ---------------------------------------------------------------------------

async fn run(home: &FsPath, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_fleett"));
    command
        .args(args)
        .env("HOME", home)
        .env_remove(URL_ENV)
        .env_remove(TOKEN_ENV)
        .env_remove("FLEETT_ROOT")
        .env_remove("RUST_LOG")
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().await.expect("run the fleett binary")
}

async fn run_against(server: &MockServer, home: &FsPath, args: &[&str]) -> Output {
    run(
        home,
        args,
        &[(URL_ENV, server.base.as_str()), (TOKEN_ENV, "test-token")],
    )
    .await
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write_config(home: &FsPath, url: &str, token: &str) {
    let dir = home.join(".config/fleett");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!("[server]\nurl = \"{url}\"\ntoken = \"{token}\"\n"),
    )
    .expect("config file");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ls_prints_an_aligned_table() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");
    let output = run_against(&server, home.path(), &["ls"]).await;
    assert!(output.status.success(), "{}", stderr(&output));

    let text = stdout(&output);
    let mut lines = text.lines();
    assert!(lines.next().expect("header").contains("STATUS"), "{text}");
    assert!(text.contains("alice"), "{text}");
    assert!(text.contains("bob"), "{text}");
    assert!(text.contains("running"), "{text}");
    assert!(text.contains("stopped"), "{text}");
    assert!(text.contains("deepseek-v4.1-flash"), "{text}");

    let request = server.request("GET", "/api/instances");
    assert_eq!(request.authorization.as_deref(), Some("Bearer test-token"));
}

#[tokio::test]
async fn ls_json_passes_the_server_payload_through() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");
    let output = run_against(&server, home.path(), &["--json", "ls"]).await;
    assert!(output.status.success(), "{}", stderr(&output));

    let value: Value = serde_json::from_str(&stdout(&output)).expect("JSON output");
    let instances = value.as_array().expect("array");
    assert_eq!(instances.len(), 2);
    assert_eq!(instances[0]["id"], "alice");
    assert_eq!(
        instances[0]["created_at"], "2026-11-08T01:00:00Z",
        "unknown fields are preserved"
    );
}

#[tokio::test]
async fn ps_shows_running_containers_and_all_with_a_flag() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");

    let output = run_against(&server, home.path(), &["ps"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("alice"), "{text}");
    assert!(text.contains("3f2a9c1d4b5e"), "{text}");
    assert!(text.contains("1d2h"), "{text}");
    assert!(
        !text.contains("bob"),
        "stopped instances are hidden: {text}"
    );
    let detail = server.request("GET", "/api/instances/alice");
    assert_eq!(detail.authorization.as_deref(), Some("Bearer test-token"));

    let output = run_against(&server, home.path(), &["ps", "--all"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("bob"), "{}", stdout(&output));
}

#[tokio::test]
async fn ps_json_prints_the_details() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");
    let output = run_against(&server, home.path(), &["ps", "--json", "--all"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("JSON output");
    let details = value.as_array().expect("array");
    assert_eq!(details.len(), 2);
    assert_eq!(details[0]["container"]["id"], "3f2a9c1d4b5e6f708192a3");
}

#[tokio::test]
async fn logs_passes_every_filter_and_prints_the_text() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");
    let output = run_against(
        &server,
        home.path(),
        &[
            "logs", "alice", "--tail", "20", "--since", "10m", "--grep", "gateway", "--level",
            "warn",
        ],
    )
    .await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "line one\nline two\n");

    let request = server.request("GET", "/api/instances/alice/logs");
    assert_eq!(request.query("tail"), Some("20"));
    assert_eq!(request.query("since"), Some("10m"));
    assert_eq!(request.query("grep"), Some("gateway"));
    assert_eq!(request.query("level"), Some("warn"));
    assert_eq!(request.query("format"), Some("text"));
}

#[tokio::test]
async fn logs_json_requests_and_prints_json() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");
    let output = run_against(&server, home.path(), &["--json", "logs", "alice"]).await;
    assert!(output.status.success(), "{}", stderr(&output));

    let request = server.request("GET", "/api/instances/alice/logs");
    assert_eq!(request.query("format"), Some("json"));
    assert_eq!(request.query("tail"), Some("200"));

    let value: Value = serde_json::from_str(&stdout(&output)).expect("JSON output");
    assert_eq!(value.as_array().expect("array").len(), 2);
}

#[tokio::test]
async fn create_sends_every_flag() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");
    let output = run_against(
        &server,
        home.path(),
        &[
            "create",
            "--id",
            "test",
            "--name",
            "Test",
            "--template",
            "solo",
            "--model",
            "muse-spark-1.3-contributor",
            "--base-url",
            "https://opencode.ai/inference/go/openai/v1",
            "--api-key-env",
            "HERMES_GATE_PROFILE_TOKEN",
            "--gate-token",
            "gate-secret",
            "--channel",
            "telegram",
            "--channel-token",
            "123:ABC",
            "--allow-lan",
            "--cpu",
            "1.5",
            "--mem",
            "2048",
            "--note",
            "watch the gateway",
        ],
    )
    .await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("created instance test (running)"),
        "{}",
        stdout(&output)
    );

    let request = server.request("POST", "/api/instances");
    assert_eq!(
        request.body,
        Some(json!({
            "id": "test",
            "name": "Test",
            "template": "solo",
            "model": "muse-spark-1.3-contributor",
            "base_url": "https://opencode.ai/inference/go/openai/v1",
            "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
            "gate_token": "gate-secret",
            "channel": {"type": "telegram", "token": "123:ABC"},
            "allow_lan": true,
            "cpu_limit": 1.5,
            "mem_limit_mb": 2048,
            "notes": "watch the gateway"
        }))
    );
}

#[tokio::test]
async fn create_json_prints_the_created_instance() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");
    let output = run_against(&server, home.path(), &["create", "--id", "test", "--json"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("JSON output");
    assert_eq!(value["id"], "test");
    assert_eq!(value["status"], "running");
}

#[tokio::test]
async fn lifecycle_commands_hit_their_routes() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");

    for (command, path) in [
        ("start", "/api/instances/alice/start"),
        ("stop", "/api/instances/alice/stop"),
        ("restart", "/api/instances/alice/restart"),
    ] {
        let output = run_against(&server, home.path(), &[command, "alice"]).await;
        assert!(output.status.success(), "{command}: {}", stderr(&output));
        assert!(
            stdout(&output).starts_with("alice: "),
            "{command}: {}",
            stdout(&output)
        );
        server.request("POST", path);
    }
}

#[tokio::test]
async fn delete_purges_only_when_asked() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");

    let output = run_against(&server, home.path(), &["delete", "alice"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("deleted instance alice"),
        "{}",
        stdout(&output)
    );
    let request = server.request("DELETE", "/api/instances/alice");
    assert_eq!(request.query("purge"), None);

    let output = run_against(&server, home.path(), &["delete", "alice", "--purge"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let requests = server.requests();
    let last = requests.last().expect("request");
    assert_eq!(last.method, "DELETE");
    assert_eq!(last.query("purge"), Some("true"));
}

#[tokio::test]
async fn usage_prints_the_totals_and_the_caps() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");

    let output = run_against(&server, home.path(), &["usage"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("alice"), "{text}");
    assert!(text.contains("12.50%"), "{text}");
    assert!(text.contains("TOTAL"), "{text}");
    assert!(text.contains("caps: cpu 8.0, memory 16384 MB"), "{text}");

    let output = run_against(&server, home.path(), &["--json", "usage"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("JSON output");
    assert_eq!(value["totals"]["cpu_pct"], 12.5);
    assert_eq!(value["caps"]["max_mem_mb"], 16384);
}

#[tokio::test]
async fn doctor_prints_a_checklist() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");
    let output = run_against(&server, home.path(), &["doctor"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    for label in ["api", "auth", "docker", "secrets", "egress"] {
        assert!(text.contains(label), "missing {label}: {text}");
    }
    assert!(text.contains("ok"), "{text}");

    let output = run_against(&server, home.path(), &["--json", "doctor"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("JSON output");
    assert_eq!(value["ok"], true);
    assert_eq!(value["checks"].as_array().expect("checks").len(), 5);
}

#[tokio::test]
async fn doctor_fails_when_docker_is_down() {
    let server = MockServer::start(false).await;
    let home = tempfile::tempdir().expect("home");
    let output = run_against(&server, home.path(), &["doctor"]).await;
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(text.contains("fail"), "{text}");
    assert!(text.contains("docker"), "{text}");
}

#[tokio::test]
async fn api_errors_exit_non_zero_with_the_code_and_message() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");

    let output = run_against(&server, home.path(), &["logs", "ghost"]).await;
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let text = stderr(&output);
    assert!(text.contains("not_found"), "{text}");
    assert!(text.contains("instance \"ghost\" not found"), "{text}");

    // `--json` prints a machine-readable error on stderr.
    let output = run_against(&server, home.path(), &["--json", "logs", "ghost"]).await;
    assert_eq!(output.status.code(), Some(1));
    let value: Value = serde_json::from_str(stderr(&output).trim()).expect("JSON error");
    assert_eq!(value["error"]["code"], "not_found");
    assert_eq!(value["error"]["message"], "instance \"ghost\" not found");
}

#[tokio::test]
async fn the_configuration_file_is_used_and_the_environment_overrides_it() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");
    write_config(home.path(), &server.base, "file-token");

    // File only.
    let output = run(home.path(), &["ls"], &[]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let request = server.request("GET", "/api/instances");
    assert_eq!(request.authorization.as_deref(), Some("Bearer file-token"));

    // Environment wins over the file, field by field.
    write_config(home.path(), "http://127.0.0.1:1", "file-token");
    let output = run(
        home.path(),
        &["ls"],
        &[(URL_ENV, server.base.as_str()), (TOKEN_ENV, "env-token")],
    )
    .await;
    assert!(output.status.success(), "{}", stderr(&output));
    let requests = server.requests();
    let last = requests.last().expect("request");
    assert_eq!(last.authorization.as_deref(), Some("Bearer env-token"));
}

#[tokio::test]
async fn a_missing_configuration_is_explained() {
    let home = tempfile::tempdir().expect("home");
    let output = run(home.path(), &["ls"], &[]).await;
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("no server URL"), "{text}");
    assert!(text.contains("config.toml"), "{text}");
}

#[tokio::test]
async fn create_defaults_the_base_url_like_the_console() {
    let server = MockServer::start(true).await;
    let home = tempfile::tempdir().expect("home");
    let output = run_against(
        &server,
        home.path(),
        &["create", "--template", "solo", "--id", "test"],
    )
    .await;
    assert!(output.status.success(), "{}", stderr(&output));

    let body = server
        .request("POST", "/api/instances")
        .body
        .expect("JSON body");
    assert_eq!(
        body["base_url"],
        "https://opencode.ai/inference/go/openai/v1"
    );
    assert_eq!(body["id"], "test");
    assert_eq!(body["template"], "solo");
    assert!(body.get("channel").is_none());
    assert!(body.get("cpu_limit").is_none());
}

#[tokio::test]
async fn channel_token_requires_a_channel() {
    let home = tempfile::tempdir().expect("home");
    let output = run(
        home.path(),
        &["create", "--id", "test", "--channel-token", "123:ABC"],
        &[(URL_ENV, "http://127.0.0.1:1"), (TOKEN_ENV, "token")],
    )
    .await;
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("--channel-token"), "{text}");
    assert!(text.contains("--channel"), "{text}");
}

#[tokio::test]
async fn an_unreachable_server_is_a_connection_error() {
    let home = tempfile::tempdir().expect("home");
    write_config(home.path(), "http://127.0.0.1:1", "token");
    let output = run(home.path(), &["ls"], &[]).await;
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("cannot reach"), "{text}");
    assert!(text.contains("127.0.0.1:1"), "{text}");
}
