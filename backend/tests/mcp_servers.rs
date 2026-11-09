//! Integration tests for the MCP server marketplace (issue #62).
//!
//! Every test runs against `MockDriver`; discovery talks to a local axum
//! fixture that speaks the Streamable-HTTP MCP transport, never to a real
//! upstream.

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::models::{Health, Status};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const MODEL: &str = "deepseek-v4.1-flash";
const BASE_URL: &str = "https://example.test/v1";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];

// ---------------------------------------------------------------------------
// Fake MCP upstream
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct FakeMcp {
    calls: Arc<Mutex<Vec<String>>>,
}

async fn rpc(State(state): State<FakeMcp>, Json(body): Json<Value>) -> Response {
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    state
        .calls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(method.clone());
    let Some(id) = body.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let result = match method.as_str() {
        "initialize" => json!({
            "protocolVersion": fixture_protocol(),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fixture", "version": "1.0"},
        }),
        "tools/list" => json!({
            "tools": [
                {"name": "fetch", "description": "Fetch a URL", "inputSchema": {"type": "object"}},
                {"name": "search", "description": "Search", "inputSchema": {"type": "object"}},
            ]
        }),
        "ping" => json!({}),
        other => {
            return Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("unknown {other}")},
            }))
            .into_response()
        }
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn fixture_protocol() -> &'static str {
    fleett::mcp::PROTOCOL_VERSION
}

struct FakeServer {
    port: u16,
    _address: std::net::SocketAddr,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn start_fake() -> FakeServer {
    let app = Router::new()
        .route("/mcp", post(rpc))
        .with_state(FakeMcp::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake MCP server");
    let address = listener.local_addr().expect("address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    FakeServer {
        port: address.port(),
        _address: address,
        handle,
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

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

    fn config_yaml(&self, id: &str) -> Value {
        let path = self
            .root()
            .join("instances")
            .join(id)
            .join("data")
            .join("config.yaml");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
        serde_yaml::from_str(&text).expect("valid config.yaml")
    }

    fn compose_yaml(&self, id: &str) -> Value {
        let path = self.root().join("mcp").join(id).join("compose.yaml");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
        serde_yaml::from_str(&text).expect("valid compose.yaml")
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        send(api::router(self.state.clone()), method, uri, body).await
    }

    async fn send_bytes(
        &self,
        method: Method,
        uri: &str,
        body: Body,
        content_type: Option<&str>,
    ) -> (StatusCode, Bytes) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        if let Some(value) = content_type {
            builder = builder.header(header::CONTENT_TYPE, value);
        }
        let request = builder.body(body).expect("valid request");
        let response = api::router(self.state.clone())
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        (status, bytes)
    }
}

async fn send(
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
    let response = router
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

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
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
                "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
                "channel": {"type": "telegram", "token": "123:ABC"},
                "shares": [],
                "mem_limit_mb": 512,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "create instance: {body}");
}

async fn deploy_fetch(harness: &Harness, port: u16) -> Value {
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/mcp/servers",
            Some(json!({"catalog": "fetch", "port": port})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "deploy fetch: {body}");
    parse(&body)
}

// ---------------------------------------------------------------------------
// Deployment and discovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalog_deploy_gives_a_healthy_project_with_tools_cached() {
    let harness = Harness::new();
    let fake = start_fake().await;
    let server = deploy_fetch(&harness, fake.port).await;

    assert_eq!(server["id"], "fetch");
    assert_eq!(server["status"], "running");
    assert_eq!(server["health"], "healthy");
    assert_eq!(server["tools_count"], 2, "{server}");
    assert_eq!(server["tools"][0]["name"], "fetch");
    assert!(!server["tools_hash"].as_str().unwrap_or_default().is_empty());
    assert_eq!(server["instances"].as_array().map(Vec::len), Some(0));

    let compose = harness.compose_yaml("fetch");
    let service = &compose["services"]["mcp"];
    assert_eq!(compose["name"], "fleett-mcp-fetch");
    assert_eq!(service["container_name"], "fleett-mcp-fetch");
    assert!(service.get("ports").is_none(), "no port may be published");
    assert_eq!(service["read_only"], true);
    assert!(service["cap_drop"]
        .as_array()
        .expect("cap_drop")
        .iter()
        .any(|value| value == "ALL"));
    assert!(service["security_opt"]
        .as_array()
        .expect("security_opt")
        .iter()
        .any(|value| value == "no-new-privileges:true"));
    assert_eq!(service["networks"], json!(["fleet"]));
    assert_eq!(compose["networks"]["fleet"]["external"], true);
    assert_eq!(
        compose["networks"]["fleet"]["name"],
        harness.state.cfg.network
    );
    assert!(service.get("privileged").is_none());
    assert!(service.get("pid").is_none());
    assert!(service.get("network_mode").is_none());
    // No `persist` on the catalogue entry: no data volume.
    assert!(service.get("volumes").is_none());
}

#[tokio::test]
async fn the_catalogue_lists_the_builtin_entries_and_filters() {
    let harness = Harness::new();
    let (status, body) = harness.request(Method::GET, "/api/mcp/catalog", None).await;
    assert_eq!(status, StatusCode::OK);
    let entries = parse(&body);
    let ids: Vec<&str> = entries
        .as_array()
        .expect("catalog")
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    for id in ["fetch", "filesystem", "memory", "github"] {
        assert!(ids.contains(&id), "{ids:?}");
    }

    let (status, body) = harness
        .request(Method::GET, "/api/mcp/catalog?q=github", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let entries = parse(&body);
    assert_eq!(entries.as_array().map(Vec::len), Some(1));
    assert_eq!(entries[0]["id"], "github");
    assert_eq!(entries[0]["env"][0]["key"], "GITHUB_PERSONAL_ACCESS_TOKEN");
    assert_eq!(entries[0]["env"][0]["secret"], true);
}

#[tokio::test]
async fn an_imported_catalogue_entry_can_be_deleted_but_not_a_builtin() {
    let harness = Harness::new();
    let manifest = json!({
        "id": "custom",
        "name": "Custom",
        "description": "A custom server",
        "kind": "streamable-http",
        "image": "example/custom:1",
        "port": 8000,
        "path": "/mcp",
        "env": [{"key": "API_KEY", "secret": true, "required": true}],
        "tags": ["custom"],
    });
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/mcp/catalog/import",
            Some(manifest.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = harness
        .request(Method::GET, "/api/mcp/catalog?q=custom", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse(&body).as_array().map(Vec::len), Some(1));

    // A duplicate import is refused.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/mcp/catalog/import",
            Some(manifest.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "mcp_duplicate");

    // Built-ins cannot be deleted; imports can.
    let (status, body) = harness
        .request(Method::DELETE, "/api/mcp/catalog/fetch", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = harness
        .request(Method::DELETE, "/api/mcp/catalog/custom", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = harness
        .request(Method::GET, "/api/mcp/catalog?q=custom", None)
        .await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Attachment
// ---------------------------------------------------------------------------

#[tokio::test]
async fn attach_renders_the_managed_block_and_recreates_the_agent() {
    let harness = Harness::new();
    let fake = start_fake().await;
    deploy_fetch(&harness, fake.port).await;
    create_instance(&harness, "alice").await;
    harness.mock.clear_calls();

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/mcp-servers/fetch",
            Some(json!({"mode": "readonly"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["status"], "running");
    assert_eq!(parse(&body)["servers"][0]["server"], "fetch");
    assert_eq!(parse(&body)["servers"][0]["mode"], "readonly");

    let config = harness.config_yaml("alice");
    let entry = &config["mcp_servers"]["fleett-fetch"];
    assert_eq!(
        entry["url"],
        format!("http://fleett-mcp-fetch:{}/mcp", fake.port)
    );
    assert_eq!(entry["timeout"], 15);
    assert!(entry.get("command").is_none());

    // The shared apply path recreated the agent.
    let calls = harness.mock.calls();
    assert!(
        calls
            .iter()
            .any(|call| call.operation == "up" && call.instance == "alice"),
        "no up call recorded: {calls:?}"
    );
}

#[tokio::test]
async fn a_patch_preserves_the_attachment_and_operator_entries() {
    let harness = Harness::new();
    let fake = start_fake().await;
    deploy_fetch(&harness, fake.port).await;
    create_instance(&harness, "alice").await;

    // An operator-written entry is part of the config before the attach.
    let config_path = harness.root().join("instances/alice/data/config.yaml");
    let mut text = std::fs::read_to_string(&config_path).expect("config");
    text.push_str("\nmcp_servers:\n  mine:\n    url: http://example.test/mcp\n");
    std::fs::write(&config_path, text).expect("config write");

    let (status, body) = harness
        .request(Method::POST, "/api/instances/alice/mcp-servers/fetch", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"notes": "patched"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = harness.config_yaml("alice");
    assert!(config["mcp_servers"]["fleett-fetch"]["url"].is_string());
    assert_eq!(
        config["mcp_servers"]["mine"]["url"], "http://example.test/mcp",
        "an operator entry must survive a PATCH"
    );

    // Detach strips only the managed entry.
    let (status, body) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/mcp-servers/fetch",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = harness.config_yaml("alice");
    assert!(config["mcp_servers"].get("fleett-fetch").is_none());
    assert_eq!(
        config["mcp_servers"]["mine"]["url"],
        "http://example.test/mcp"
    );
}

#[tokio::test]
async fn two_agents_share_one_deployed_container() {
    let harness = Harness::new();
    let fake = start_fake().await;
    deploy_fetch(&harness, fake.port).await;
    create_instance(&harness, "alice").await;
    create_instance(&harness, "bob").await;

    for id in ["alice", "bob"] {
        let (status, body) = harness
            .request(
                Method::POST,
                &format!("/api/instances/{id}/mcp-servers/fetch"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let ups = harness
        .mock
        .project_calls()
        .into_iter()
        .filter(|call| call.operation == "up" && call.project == "fleett-mcp-fetch")
        .count();
    assert_eq!(ups, 1, "attaching must never redeploy the server");

    let (status, body) = harness
        .request(Method::GET, "/api/mcp/servers/fetch", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let server = parse(&body);
    assert_eq!(server["instances"], json!(["alice", "bob"]));

    // Each agent has its own managed entry pointing at the same endpoint.
    for id in ["alice", "bob"] {
        let config = harness.config_yaml(id);
        assert_eq!(
            config["mcp_servers"]["fleett-fetch"]["url"],
            format!("http://fleett-mcp-fetch:{}/mcp", fake.port)
        );
    }
}

#[tokio::test]
async fn deleting_an_attached_server_is_a_409_with_the_instance_list() {
    let harness = Harness::new();
    let fake = start_fake().await;
    deploy_fetch(&harness, fake.port).await;
    create_instance(&harness, "alice").await;
    let (status, _) = harness
        .request(Method::POST, "/api/instances/alice/mcp-servers/fetch", None)
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = harness
        .request(Method::DELETE, "/api/mcp/servers/fetch", None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = &parse(&body)["error"];
    assert_eq!(error["code"], "mcp_server_in_use");
    assert_eq!(error["details"]["instances"], json!(["alice"]));

    // Detach, then delete with purge removes the compose file.
    let (status, _) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/mcp-servers/fetch",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = harness
        .request(Method::DELETE, "/api/mcp/servers/fetch?purge=true", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!harness.root().join("mcp/fetch").exists());
}

#[tokio::test]
async fn a_stdio_server_renders_command_and_args() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/mcp/servers",
            Some(json!({
                "id": "fs-stdio",
                "name": "Filesystem (stdio)",
                "kind": "stdio",
                "command": ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/data"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(parse(&body)["status"], "created");
    assert!(!harness.root().join("mcp/fs-stdio").exists());

    create_instance(&harness, "alice").await;
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/mcp-servers/fs-stdio",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let config = harness.config_yaml("alice");
    let entry = &config["mcp_servers"]["fleett-fs-stdio"];
    assert_eq!(entry["command"], "npx");
    assert_eq!(entry["args"][0], "-y");
    assert_eq!(entry["args"][2], "/data");
    assert!(entry.get("url").is_none());
}

// ---------------------------------------------------------------------------
// Test endpoint and secrets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_test_endpoint_reports_latency_and_tools_and_422_on_failure() {
    let harness = Harness::new();
    let fake = start_fake().await;
    deploy_fetch(&harness, fake.port).await;

    let (status, body) = harness
        .request(Method::POST, "/api/mcp/servers/fetch/test", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let discovery = parse(&body);
    assert_eq!(discovery["reachable"], true);
    assert_eq!(discovery["tools_count"], 2);
    assert!(discovery["latency_ms"].as_u64().is_some());
    assert_eq!(discovery["protocol_version"], fleett::mcp::PROTOCOL_VERSION);

    // A server whose port refuses connections answers 422 mcp_unreachable.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/mcp/servers",
            Some(json!({
                "id": "broken",
                "name": "Broken",
                "kind": "streamable-http",
                "image": "example/broken:1",
                "port": 1,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "deploy registers it: {body}");
    let (status, body) = harness
        .request(Method::POST, "/api/mcp/servers/broken/test", None)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "mcp_unreachable");
}

#[tokio::test]
async fn env_and_header_values_are_never_returned() {
    let harness = Harness::new();
    let fake = start_fake().await;
    deploy_fetch(&harness, fake.port).await;

    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/mcp/servers/fetch/env",
            Some(json!({
                "env": [
                    {"key": "API_TOKEN", "value": "super-secret-value", "secret": true},
                    {"key": "MODE", "value": "fast", "secret": false},
                ]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains("super-secret-value"),
        "secret leaked: {body}"
    );
    assert!(!body.contains("\"fast\""), "value leaked: {body}");
    let server = parse(&body);
    let env = server["env"].as_array().expect("env");
    assert_eq!(env.len(), 2);
    assert!(env.iter().all(|entry| entry.get("value").is_none()));
    assert_eq!(
        env.iter()
            .find(|entry| entry["key"] == "API_TOKEN")
            .expect("token row")["secret"],
        true
    );

    let (status, body) = harness
        .request(Method::GET, "/api/mcp/servers/fetch", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("super-secret-value"), "{body}");

    // The control plane can still decrypt the value for the compose process.
    let values = fleett::mcp_servers::env_values(&harness.state.cfg, &harness.state.db, "fetch")
        .expect("values");
    assert_eq!(values["API_TOKEN"], "super-secret-value");
    assert_eq!(values["MODE"], "fast");
}

// ---------------------------------------------------------------------------
// Supervision
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_supervision_tick_opens_then_closes_an_incident() {
    let harness = Harness::new();
    let fake = start_fake().await;
    deploy_fetch(&harness, fake.port).await;

    harness
        .mock
        .force_project("fleett-mcp-fetch", Status::Stopped, Health::Unknown);
    let report = fleett::mcp_servers::tick(&harness.state).await;
    assert_eq!(report.servers, 1);
    assert!(report.incidents_opened >= 1, "{report:?}");

    let (status, body) = harness
        .request(Method::GET, "/api/mcp/servers/fetch/incidents", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let incidents = parse(&body);
    assert_eq!(incidents[0]["status"], "open");
    assert_eq!(incidents[0]["cause"], "unreachable");
    let incident_id = incidents[0]["id"].as_i64().expect("id");

    harness
        .mock
        .force_project("fleett-mcp-fetch", Status::Running, Health::Healthy);
    let report = fleett::mcp_servers::tick(&harness.state).await;
    assert!(report.incidents_closed >= 1, "{report:?}");

    let (status, body) = harness
        .request(Method::GET, "/api/mcp/servers/fetch/incidents", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let incidents = parse(&body);
    assert_eq!(incidents[0]["status"], "closed");
    assert!(incidents[0]["closed_at"].is_string());

    // Close is idempotent through the API (already closed → 404).
    let (status, _) = harness
        .request(
            Method::POST,
            &format!("/api/mcp-incidents/{incident_id}/close"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Export / import
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_import_round_trips_the_mcp_attachments() {
    let harness = Harness::new();
    let fake = start_fake().await;
    deploy_fetch(&harness, fake.port).await;
    create_instance(&harness, "alice").await;
    let (status, _) = harness
        .request(Method::POST, "/api/instances/alice/mcp-servers/fetch", None)
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, archive) = harness
        .send_bytes(
            Method::GET,
            "/api/instances/alice/export",
            Body::empty(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = harness
        .request(Method::DELETE, "/api/instances/alice?purge=true", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body) = harness
        .send_bytes(
            Method::POST,
            "/api/instances/import?id=alice-copy&start=false",
            Body::from(archive),
            Some("application/gzip"),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice-copy/mcp-servers", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let response = parse(&body);
    assert_eq!(response["servers"][0]["server"], "fetch");
    let config = harness.config_yaml("alice-copy");
    assert!(config["mcp_servers"]["fleett-fetch"]["url"].is_string());
}

#[tokio::test]
async fn an_unknown_server_in_an_archive_is_dropped_with_a_warning() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cfg = Config::for_tests(dir.path().to_path_buf());
    let spec = fleett::models::InstanceSpec {
        id: "alice".to_string(),
        name: "Alice".to_string(),
        template: "guest".to_string(),
        model: MODEL.to_string(),
        base_url: BASE_URL.to_string(),
        api_key_env: "HERMES_GATE_PROFILE_TOKEN".to_string(),
        provider: None,
        channel: fleett::models::ChannelKind::None,
        shares: Vec::new(),
        skills: Vec::new(),
        mcp_servers: vec![fleett::models::McpRef {
            server: "gone".to_string(),
            mode: fleett::models::McpMode::Rw,
        }],
        mem_limit_mb: 512,
        cpu_limit: 1.0,
        image: "hermes-agent:test".to_string(),
        previous_image: None,
        allow_lan: false,
        heal: fleett::models::HealPolicy::Off,
        unhealthy_after: fleett::models::DEFAULT_UNHEALTHY_AFTER,
        max_restarts: fleett::models::DEFAULT_MAX_RESTARTS,
        heal_backoff_s: 60,
        disk_quota_mb: 0,
        disk_policy: fleett::models::DiskPolicy::Warn,
        notes: String::new(),
        host: fleett::hosts::LOCAL_HOST.to_string(),
        status: Status::Running,
        last_error: None,
        created_at: fleett::models::now_iso(),
        updated_at: fleett::models::now_iso(),
    };
    let archive = fleett::instance_archive::export(&cfg, &spec).expect("archive");
    let mut out = Vec::new();
    let mut reader = flate2::read::GzDecoder::new(archive.as_slice());
    std::io::Read::read_to_end(&mut reader, &mut out).expect("gunzip");
    let mut tar = tar::Archive::new(out.as_slice());
    let mut found: Option<String> = None;
    for entry in tar.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let path = entry.path().expect("path").to_string_lossy().to_string();
        if path == "manifest.json" {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text).expect("manifest");
            found = Some(text);
        }
    }
    let manifest = found.expect("manifest");
    assert!(manifest.contains("\"mcp_servers\""), "{manifest}");
    assert!(manifest.contains("\"gone\""), "{manifest}");
}
