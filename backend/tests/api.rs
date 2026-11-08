//! Integration tests for the HTTP API.
//!
//! Every test runs against `MockDriver`; no test ever calls Docker.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use fleett::api::{self, AppState};
use fleett::config::{Config, EgressMode};
use fleett::driver::mock::MockDriver;
use fleett::models::Health;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";

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
        configure(&mut cfg);
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        Self { dir, state, mock }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn instance_dir(&self, id: &str) -> PathBuf {
        self.root().join("instances").join(id)
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
    ) -> (StatusCode, HeaderMap, String) {
        send_to(api::router(self.state.clone()), method, uri, body, token).await
    }

    /// Write the marker `fleett egress install` would leave behind.
    fn write_egress_marker(&self, subnet: &str) {
        let path = &self.state.cfg.egress_marker;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("marker dir");
        }
        std::fs::write(
            path,
            json!({
                "subnet": subnet,
                "installed_at": "2026-11-08T00:00:00Z",
                "version": "0.2.0",
            })
            .to_string(),
        )
        .expect("marker file");
    }
}

async fn send_to(
    router: Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
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
    let request = builder.body(body).expect("valid request");
    let response = router.oneshot(request).await.expect("response");
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

fn instance_body(id: &str) -> Value {
    json!({
        "id": id,
        "name": format!("Agent {id}"),
        "template": "guest",
        "model": MODEL,
        "base_url": BASE_URL,
        "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
        "channel": {"type": "telegram", "token": "123:ABC"},
        "shares": [],
        "mem_limit_mb": 1024,
        "notes": "integration test"
    })
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

fn read_yaml(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
    serde_yaml::from_str(&text)
        .unwrap_or_else(|err| panic!("invalid YAML in {}: {err}", path.display()))
}

async fn create_instance(harness: &Harness, id: &str) -> Value {
    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(instance_body(id)))
        .await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {body}");
    parse(&body)
}

async fn create_share(harness: &Harness, name: &str) {
    let (status, body) = harness
        .request(Method::POST, "/api/shares", Some(json!({ "name": name })))
        .await;
    assert_eq!(status, StatusCode::CREATED, "share create failed: {body}");
}

// ---------------------------------------------------------------------------
// Health and auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_is_public() {
    let harness = Harness::new();
    let (status, body) = harness
        .request_as(Method::GET, "/api/health", None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let health = parse(&body);
    assert_eq!(health["ok"], true);
    assert_eq!(health["version"], "0.2.0");
    assert_eq!(health["driver"], "mock");
    assert_eq!(health["docker"]["ok"], true);
}

#[tokio::test]
async fn api_requires_a_bearer_token() {
    let harness = Harness::new();

    let (status, body) = harness
        .request_as(Method::GET, "/api/instances", None, None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(parse(&body)["error"]["code"], "unauthorized");

    let (status, _) = harness
        .request_as(Method::GET, "/api/instances", None, Some("wrong-token"))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = harness
        .request_as(Method::GET, "/api/instances", None, Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = harness
        .request_as(Method::GET, "/api/auth/check", None, Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn no_auth_mode_allows_anonymous_requests() {
    let harness = Harness::with_config(|cfg| {
        cfg.token = None;
        cfg.allow_no_auth = true;
    });
    let (status, body) = harness
        .request_as(Method::GET, "/api/instances", None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lists_builtin_templates() {
    let harness = Harness::new();
    let (status, body) = harness.request(Method::GET, "/api/templates", None).await;
    assert_eq!(status, StatusCode::OK);
    let templates = parse(&body);
    let templates = templates.as_array().expect("array");
    assert!(templates.len() >= 3);

    let guest = templates
        .iter()
        .find(|template| template["id"] == "guest")
        .expect("guest template");
    assert_eq!(guest["channel"], "telegram");
    assert_eq!(guest["default_model"], MODEL);
    assert!(guest["description"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));

    let ids: Vec<&str> = templates
        .iter()
        .filter_map(|template| template["id"].as_str())
        .collect();
    assert!(ids.contains(&"guest-discord"));
    assert!(ids.contains(&"solo"));
}

// ---------------------------------------------------------------------------
// Create / read / list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn creates_instance_and_writes_files() {
    let harness = Harness::new();
    let mut body = instance_body("alice");
    body["gate_token"] = json!("gate-secret");
    body["env"] = json!({"EXTRA_FLAG": "1"});

    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");

    let detail = parse(&response);
    assert_eq!(detail["id"], "alice");
    assert_eq!(detail["status"], "running");
    assert_eq!(detail["channel"], "telegram");
    assert_eq!(detail["model"], MODEL);
    assert_eq!(detail["mem_limit_mb"], 1024);
    assert_eq!(detail["allow_lan"], false);
    // Secrets never come back through the API.
    assert!(!response.contains("123:ABC"));
    assert!(!response.contains("gate-secret"));

    // compose.yaml parses and names the project.
    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    assert_eq!(compose["name"], "fleett-alice");

    // secrets.env is 0600 and contains the expected keys.
    let secrets_path = harness.instance_dir("alice").join("secrets.env");
    let metadata = std::fs::metadata(&secrets_path).expect("secrets metadata");
    assert_eq!(metadata.mode() & 0o777, 0o600);
    let secrets = std::fs::read_to_string(&secrets_path).expect("secrets");
    assert!(secrets.contains("TELEGRAM_BOT_TOKEN=123:ABC"), "{secrets}");
    assert!(
        secrets.contains("HERMES_GATE_PROFILE_TOKEN=gate-secret"),
        "{secrets}"
    );
    assert!(secrets.contains("EXTRA_FLAG=1"), "{secrets}");

    // config.yaml parses and has the mandated model block.
    let config = read_yaml(&harness.instance_dir("alice").join("data/config.yaml"));
    assert_eq!(config["model"]["default"], MODEL);
    assert_eq!(config["model"]["provider"], "custom");
    assert_eq!(config["model"]["base_url"], BASE_URL);
    assert_eq!(config["model"]["api_key"], "${HERMES_GATE_PROFILE_TOKEN}");
    assert_eq!(config["model"]["api_mode"], "chat_completions");
    assert_eq!(config["platforms"]["telegram"]["enabled"], true);
    assert_eq!(config["platforms"]["discord"]["enabled"], false);
    assert!(harness.instance_dir("alice").join("data/SOUL.md").is_file());

    assert_eq!(
        detail["env_keys"],
        json!([
            "EXTRA_FLAG",
            "HERMES_GATE_PROFILE_TOKEN",
            "TELEGRAM_BOT_TOKEN"
        ])
    );
    assert_eq!(
        detail["data_dir"],
        harness
            .instance_dir("alice")
            .join("data")
            .display()
            .to_string()
    );
    assert!(detail["container"]["id"].is_string());
    assert!(detail["compose_yaml"]
        .as_str()
        .is_some_and(|text| text.contains("fleett-alice")));

    let ups = harness
        .mock
        .calls()
        .iter()
        .filter(|call| call.operation == "up")
        .count();
    assert_eq!(ups, 1);
}

#[tokio::test]
async fn duplicate_ids_conflict() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(instance_body("alice")))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(parse(&body)["error"]["code"], "instance_exists");
}

#[tokio::test]
async fn rejects_invalid_instance_inputs() {
    let harness = Harness::new();

    for id in [
        "Alice",
        "a",
        "alice_1",
        "alice/../x",
        "-alice",
        &"a".repeat(33),
    ] {
        let (status, body) = harness
            .request(Method::POST, "/api/instances", Some(instance_body(id)))
            .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "id {id:?} accepted: {body}"
        );
        assert_eq!(parse(&body)["error"]["code"], "invalid_id");
    }

    let mut empty_model = instance_body("bob");
    empty_model["model"] = json!("   ");
    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(empty_model))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "invalid_model");

    let mut empty_base_url = instance_body("bob");
    empty_base_url["base_url"] = json!("");
    let (status, _) = harness
        .request(Method::POST, "/api/instances", Some(empty_base_url))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    for limit in [0, 100, 255, 8193, 100_000] {
        let mut too_small = instance_body("bob");
        too_small["mem_limit_mb"] = json!(limit);
        let (status, body) = harness
            .request(Method::POST, "/api/instances", Some(too_small))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "limit {limit}");
        assert_eq!(parse(&body)["error"]["code"], "invalid_mem_limit");
    }

    let mut unknown_template = instance_body("bob");
    unknown_template["template"] = json!("nope");
    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(unknown_template))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "unknown_template");

    let mut bad_channel = instance_body("bob");
    bad_channel["channel"] = json!({"type": "signal"});
    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(bad_channel))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "invalid_channel");

    let mut unknown_share = instance_body("bob");
    unknown_share["shares"] = json!([{"name": "ghost", "mode": "rw"}]);
    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(unknown_share))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "unknown_share");
}

#[tokio::test]
async fn gets_and_lists_instances() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    create_instance(&harness, "bob").await;

    let (status, body) = harness.request(Method::GET, "/api/instances", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = parse(&body);
    let list = list.as_array().expect("array");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["id"], "alice");
    assert_eq!(list[1]["id"], "bob");
    assert_eq!(list[0]["status"], "running");
    assert_eq!(list[0]["shares"], json!([]));
    assert!(list[0]["created_at"]
        .as_str()
        .is_some_and(|value| value.ends_with('Z')));

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let detail = parse(&body);
    assert_eq!(detail["api_key_env"], "HERMES_GATE_PROFILE_TOKEN");
    assert_eq!(detail["base_url"], BASE_URL);
    assert_eq!(detail["status"], "running");
    assert_eq!(
        parse(&body)["container"]["image"],
        "hermes-agent:0.22.2-local"
    );
}

#[tokio::test]
async fn unknown_instances_are_404() {
    let harness = Harness::new();
    for (method, uri) in [
        (Method::GET, "/api/instances/ghost"),
        (Method::DELETE, "/api/instances/ghost"),
        (Method::POST, "/api/instances/ghost/start"),
        (Method::GET, "/api/instances/ghost/logs"),
    ] {
        let (status, body) = harness.request(method.clone(), uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        assert_eq!(parse(&body)["error"]["code"], "not_found", "{method} {uri}");
    }

    let (status, _) = harness
        .request(
            Method::PATCH,
            "/api/instances/ghost",
            Some(json!({"name": "Ghost"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Patch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_rewrites_compose_and_keeps_data() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    let marker = harness.instance_dir("alice").join("data/notes.txt");
    std::fs::write(&marker, "keep me").expect("marker");

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({
                "name": "Alice Prime",
                "model": "other-model",
                "mem_limit_mb": 2048,
                "notes": "updated"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let detail = parse(&body);
    assert_eq!(detail["name"], "Alice Prime");
    assert_eq!(detail["model"], "other-model");
    assert_eq!(detail["mem_limit_mb"], 2048);
    assert_eq!(detail["status"], "running");

    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    assert_eq!(compose["services"]["agent"]["mem_limit"], "2048m");

    let config = read_yaml(&harness.instance_dir("alice").join("data/config.yaml"));
    assert_eq!(config["model"]["default"], "other-model");

    assert_eq!(
        std::fs::read_to_string(&marker).expect("marker readable"),
        "keep me"
    );
    // Channel token survives a patch that does not mention it.
    let secrets = std::fs::read_to_string(harness.instance_dir("alice").join("secrets.env"))
        .expect("secrets");
    assert!(secrets.contains("TELEGRAM_BOT_TOKEN=123:ABC"), "{secrets}");
}

#[tokio::test]
async fn patch_keeps_a_legacy_compose_project_name() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    // Simulate a project created by the previous release: the compose file
    // declares `hermes-<id>` and must keep it until an explicit migration.
    let path = harness.instance_dir("alice").join("compose.yaml");
    let legacy = std::fs::read_to_string(&path)
        .expect("compose")
        .replace("fleett-alice", "hermes-alice");
    std::fs::write(&path, legacy).expect("legacy compose");

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"notes": "legacy project"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let compose = read_yaml(&path);
    assert_eq!(compose["name"], "hermes-alice");
    assert_eq!(
        compose["services"]["agent"]["container_name"],
        "hermes-alice"
    );

    // The driver is asked to operate on that declared project.
    let started = harness
        .mock
        .calls()
        .iter()
        .any(|call| call.operation == "up" && call.instance == "alice");
    assert!(started);
}

#[tokio::test]
async fn patch_can_rotate_the_channel_token() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"channel": {"type": "discord", "token": "discord-secret"}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["channel"], "discord");

    let secrets = std::fs::read_to_string(harness.instance_dir("alice").join("secrets.env"))
        .expect("secrets");
    assert!(
        secrets.contains("DISCORD_BOT_TOKEN=discord-secret"),
        "{secrets}"
    );
    assert!(!secrets.contains("TELEGRAM_BOT_TOKEN"), "{secrets}");

    let config = read_yaml(&harness.instance_dir("alice").join("data/config.yaml"));
    assert_eq!(config["platforms"]["discord"]["enabled"], true);
    assert_eq!(config["platforms"]["telegram"]["enabled"], false);
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn start_stop_restart_transitions() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    let (status, body) = harness
        .request(Method::POST, "/api/instances/alice/stop", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["status"], "stopped");

    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(parse(&body)["status"], "stopped");

    let (status, body) = harness
        .request(Method::POST, "/api/instances/alice/start", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["status"], "running");

    let (status, body) = harness
        .request(Method::POST, "/api/instances/alice/restart", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["status"], "running");

    let operations: Vec<&str> = harness
        .mock
        .calls()
        .iter()
        .map(|call| call.operation)
        .collect();
    assert!(operations.contains(&"stop"));
    assert!(operations.contains(&"start"));
    assert!(operations.contains(&"restart"));
}

#[tokio::test]
async fn logs_are_plain_text_and_bounded() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    let (status, headers, body) = harness
        .send(
            Method::GET,
            "/api/instances/alice/logs?tail=2",
            None,
            Some(TOKEN),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(body.lines().count(), 2);
    assert!(body.contains("[mock]"));

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/logs", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.lines().count(), 4);

    for tail in ["0", "2001"] {
        let (status, body) = harness
            .request(
                Method::GET,
                &format!("/api/instances/alice/logs?tail={tail}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "tail {tail}");
        assert_eq!(parse(&body)["error"]["code"], "invalid_tail");
    }
}

// ---------------------------------------------------------------------------
// Shares
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shares_can_be_created_attached_and_detached() {
    let harness = Harness::new();
    create_share(&harness, "findings").await;
    assert!(harness.root().join("shares/findings").is_dir());
    create_instance(&harness, "alice").await;

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/shares/findings/attach",
            Some(json!({"instance": "alice", "mode": "rw"})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    let volumes = compose["services"]["agent"]["volumes"]
        .as_array()
        .expect("volumes");
    assert!(volumes
        .iter()
        .any(|volume| volume == "../../shares/findings:/opt/data/shared/findings"));

    let (status, _) = harness
        .request(
            Method::POST,
            "/api/shares/findings/attach",
            Some(json!({"instance": "alice", "mode": "ro"})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    let volumes = compose["services"]["agent"]["volumes"]
        .as_array()
        .expect("volumes");
    assert!(volumes
        .iter()
        .any(|volume| volume == "../../shares/findings:/opt/data/shared/findings:ro"));

    let (status, body) = harness.request(Method::GET, "/api/shares", None).await;
    assert_eq!(status, StatusCode::OK);
    let shares = parse(&body);
    assert_eq!(shares[0]["name"], "findings");
    assert_eq!(shares[0]["instances"][0]["id"], "alice");
    assert_eq!(shares[0]["instances"][0]["mode"], "ro");
    assert!(shares[0]["path"]
        .as_str()
        .is_some_and(|path| path.ends_with("shares/findings")));

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        parse(&body)["shares"],
        json!([{"name": "findings", "mode": "ro"}])
    );

    let (status, _) = harness
        .request(
            Method::POST,
            "/api/shares/findings/detach",
            Some(json!({"instance": "alice"})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    let volumes = compose["services"]["agent"]["volumes"]
        .as_array()
        .expect("volumes");
    assert!(!volumes
        .iter()
        .any(|volume| volume.as_str().is_some_and(|text| text.contains("shares/"))));
}

#[tokio::test]
async fn shares_cannot_be_deleted_while_attached() {
    let harness = Harness::new();
    create_share(&harness, "findings").await;
    create_instance(&harness, "alice").await;
    let (status, _) = harness
        .request(
            Method::POST,
            "/api/shares/findings/attach",
            Some(json!({"instance": "alice", "mode": "rw"})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = harness
        .request(Method::DELETE, "/api/shares/findings", None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "share_attached");

    let (status, _) = harness
        .request(
            Method::POST,
            "/api/shares/findings/detach",
            Some(json!({"instance": "alice"})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = harness
        .request(Method::DELETE, "/api/shares/findings", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!harness.root().join("shares/findings").exists());

    let (status, _) = harness.request(Method::GET, "/api/shares", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn invalid_share_requests_are_rejected() {
    let harness = Harness::new();

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "Bad Name"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "invalid_share");

    let (status, _) = harness
        .request(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "findings"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "findings"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(parse(&body)["error"]["code"], "share_exists");

    let (status, _) = harness
        .request(
            Method::POST,
            "/api/shares/findings/attach",
            Some(json!({"instance": "ghost", "mode": "rw"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    create_instance(&harness, "alice").await;
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/shares/findings/attach",
            Some(json!({"instance": "alice", "mode": "write"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "invalid_mode");
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_keeps_data_by_default() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let marker = harness.instance_dir("alice").join("data/keep.txt");
    std::fs::write(&marker, "precious").expect("marker");

    let (status, body) = harness
        .request(Method::DELETE, "/api/instances/alice", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, _) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!harness.instance_dir("alice").join("compose.yaml").exists());
    assert!(!harness.instance_dir("alice").join("secrets.env").exists());
    assert!(marker.is_file(), "data directory must be preserved");
    assert_eq!(
        std::fs::read_to_string(&marker).expect("marker"),
        "precious"
    );
}

#[tokio::test]
async fn delete_with_purge_removes_data() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let marker = harness.instance_dir("alice").join("data/keep.txt");
    std::fs::write(&marker, "precious").expect("marker");

    let (status, body) = harness
        .request(Method::DELETE, "/api/instances/alice?purge=true", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!harness.instance_dir("alice").exists());
}

// ---------------------------------------------------------------------------
// Compose shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compose_file_is_restrictive() {
    let harness = Harness::new();
    create_share(&harness, "findings").await;
    create_share(&harness, "archive").await;

    let mut body = instance_body("alice");
    body["shares"] = json!([
        {"name": "findings", "mode": "rw"},
        {"name": "archive", "mode": "ro"}
    ]);
    body["mem_limit_mb"] = json!(1536);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");

    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    let agent = &compose["services"]["agent"];

    assert_eq!(compose["name"], "fleett-alice");
    assert_eq!(agent["image"], "hermes-agent:0.22.2-local");
    assert_eq!(agent["container_name"], "fleett-alice");
    assert_eq!(agent["restart"], "unless-stopped");
    assert_eq!(agent["command"], json!(["gateway", "run"]));
    assert_eq!(agent["env_file"], json!(["secrets.env"]));
    assert_eq!(agent["environment"]["HERMES_UID"], "10000");
    assert_eq!(agent["environment"]["HERMES_GID"], "10000");
    assert_eq!(agent["networks"], json!(["fleet"]));
    assert_eq!(agent["security_opt"], json!(["no-new-privileges:true"]));
    assert_eq!(agent["cap_drop"], json!(["NET_RAW"]));
    assert_eq!(agent["tmpfs"], json!(["/tmp"]));
    assert_eq!(agent["pids_limit"], 256);
    assert_eq!(agent["mem_limit"], "1536m");
    assert_eq!(agent["memswap_limit"], "1536m");
    assert_eq!(agent["cpus"], "1");
    assert_eq!(
        agent["healthcheck"]["test"],
        json!(["CMD", "python3", "/opt/data/healthcheck.py"])
    );
    assert_eq!(agent["healthcheck"]["interval"], "30s");
    assert_eq!(agent["healthcheck"]["timeout"], "5s");
    assert_eq!(agent["healthcheck"]["retries"], 3);
    assert_eq!(agent["healthcheck"]["start_period"], "60s");
    assert_eq!(agent["logging"]["driver"], "json-file");
    assert_eq!(agent["logging"]["options"]["max-size"], "10m");
    assert_eq!(agent["logging"]["options"]["max-file"], "3");

    // No host networking, no Docker socket, no privilege escalation.
    assert!(compose.get("network_mode").is_none());
    assert!(agent.get("network_mode").is_none());
    assert!(agent.get("privileged").is_none());
    let volumes = agent["volumes"].as_array().expect("volumes");
    assert!(volumes
        .iter()
        .all(|volume| { !volume.as_str().unwrap_or_default().contains("docker.sock") }));
    assert!(volumes.iter().any(|volume| volume == "./data:/opt/data"));
    assert!(volumes
        .iter()
        .any(|volume| { volume == "../../shares/findings:/opt/data/shared/findings" }));
    assert!(volumes
        .iter()
        .any(|volume| { volume == "../../shares/archive:/opt/data/shared/archive:ro" }));

    assert_eq!(compose["networks"]["fleet"]["external"], true);
    assert_eq!(compose["networks"]["fleet"]["name"], "fleett");
}

// ---------------------------------------------------------------------------
// Egress / LAN access
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allow_lan_defaults_to_false_on_the_restricted_network() {
    let harness = Harness::new();
    let detail = create_instance(&harness, "alice").await;
    assert_eq!(detail["allow_lan"], false);

    let (_, body) = harness.request(Method::GET, "/api/instances", None).await;
    assert_eq!(parse(&body)[0]["allow_lan"], false);

    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    assert_eq!(compose["services"]["agent"]["networks"], json!(["fleet"]));
    assert!(compose["networks"].get("lan").is_none());
}

#[tokio::test]
async fn allow_lan_attaches_the_lan_network_and_warns() {
    let harness = Harness::new();
    let mut body = instance_body("alice");
    body["allow_lan"] = json!(true);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");

    let detail = parse(&response);
    assert_eq!(detail["allow_lan"], true);
    let warnings = detail["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .filter_map(Value::as_str)
            .any(|warning| warning.contains("fleett-lan")),
        "{warnings:?}"
    );

    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    assert_eq!(
        compose["services"]["agent"]["networks"],
        json!(["fleet", "lan"])
    );
    assert_eq!(compose["networks"]["lan"]["external"], true);
    assert_eq!(compose["networks"]["lan"]["name"], "fleett-lan");

    let (_, body) = harness.request(Method::GET, "/api/instances", None).await;
    assert_eq!(parse(&body)[0]["allow_lan"], true);

    // Turning it back off detaches the network.
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"allow_lan": false})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["allow_lan"], false);
    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    assert_eq!(compose["services"]["agent"]["networks"], json!(["fleet"]));
    assert!(compose["networks"].get("lan").is_none());
}

#[tokio::test]
async fn restrict_mode_refuses_a_restricted_instance_without_a_marker() {
    let harness = Harness::with_config(|cfg| cfg.egress = EgressMode::Restrict);

    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(instance_body("alice")))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = parse(&body)["error"].clone();
    assert_eq!(error["code"], "egress_not_enforced");
    let message = error["message"].as_str().expect("message");
    assert!(message.contains("fleett egress install"), "{message}");
    assert_eq!(error["details"]["egress"], "restrict");
    assert!(error["details"]["marker"]
        .as_str()
        .is_some_and(|path| path.ends_with("egress.json")));
    // Nothing was written for a refused create.
    assert!(!harness.instance_dir("alice").exists());
}

#[tokio::test]
async fn restrict_mode_accepts_a_matching_marker() {
    let harness = Harness::with_config(|cfg| cfg.egress = EgressMode::Restrict);
    harness.mock.force_network_subnet(Some("172.18.0.0/16"));
    harness.write_egress_marker("172.18.0.0/16");

    let detail = create_instance(&harness, "alice").await;
    assert_eq!(detail["allow_lan"], false);
    let warnings = detail["warnings"].as_array().expect("warnings");
    assert!(
        !warnings
            .iter()
            .filter_map(Value::as_str)
            .any(|warning| warning.contains("not installed")),
        "{warnings:?}"
    );
    assert_eq!(
        read_yaml(&harness.instance_dir("alice").join("compose.yaml"))["services"]["agent"]
            ["networks"],
        json!(["fleet"])
    );
}

#[tokio::test]
async fn restrict_mode_refuses_a_stale_marker() {
    let harness = Harness::with_config(|cfg| cfg.egress = EgressMode::Restrict);
    // The network was recreated with another subnet since the install.
    harness.mock.force_network_subnet(Some("172.19.0.0/16"));
    harness.write_egress_marker("172.18.0.0/16");

    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(instance_body("alice")))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = parse(&body)["error"].clone();
    assert_eq!(error["code"], "egress_not_enforced");
    let message = error["message"].as_str().expect("message");
    assert!(message.contains("172.18.0.0/16"), "{message}");
    assert!(message.contains("172.19.0.0/16"), "{message}");
    assert!(!harness.instance_dir("alice").exists());
}

#[tokio::test]
async fn restrict_mode_allows_lan_access_with_a_warning() {
    let harness = Harness::with_config(|cfg| cfg.egress = EgressMode::Restrict);
    let mut body = instance_body("alice");
    body["allow_lan"] = json!(true);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
    let detail = parse(&response);
    let warnings: Vec<&str> = detail["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(warnings
        .iter()
        .any(|warning| warning.contains("fleett-lan")));
    assert!(!warnings
        .iter()
        .any(|warning| warning.contains("not installed")));
}

#[tokio::test]
async fn restrict_mode_blocks_patching_back_to_the_restricted_network() {
    let harness = Harness::with_config(|cfg| cfg.egress = EgressMode::Restrict);
    harness.mock.force_network_subnet(Some("172.18.0.0/16"));

    let mut body = instance_body("alice");
    body["allow_lan"] = json!(true);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"allow_lan": false})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "egress_not_enforced");
    // The refused patch left the instance on the LAN.
    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(parse(&body)["allow_lan"], true);

    harness.write_egress_marker("172.18.0.0/16");
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"allow_lan": false})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["allow_lan"], false);
}

#[tokio::test]
async fn egress_off_allows_creation_with_a_warning() {
    let harness = Harness::new();
    assert_eq!(harness.state.cfg.egress, EgressMode::Off);
    let detail = create_instance(&harness, "alice").await;
    let warnings: Vec<&str> = detail["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("FLEETT_EGRESS=off")),
        "{warnings:?}"
    );
}

#[tokio::test]
async fn lifecycle_is_never_blocked_by_the_egress_policy() {
    let harness = Harness::with_config(|cfg| cfg.egress = EgressMode::Restrict);
    harness.write_egress_marker("172.18.0.0/16");
    create_instance(&harness, "alice").await;

    // The firewall disappears (marker removed, rules flushed)…
    std::fs::remove_file(&harness.state.cfg.egress_marker).expect("remove marker");

    // …existing instances still stop and start.
    for action in ["stop", "start"] {
        let (status, body) = harness
            .request(
                Method::POST,
                &format!("/api/instances/alice/{action}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{action}: {body}");
    }
}

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_writes_the_healthcheck_and_exposes_health() {
    let harness = Harness::new();
    let detail = create_instance(&harness, "alice").await;
    assert_eq!(detail["health"], "healthy");
    assert_eq!(detail["cpu_limit"], 1.0);

    let script_path = harness.instance_dir("alice").join("data/healthcheck.py");
    let metadata = std::fs::metadata(&script_path).expect("healthcheck metadata");
    assert_eq!(metadata.mode() & 0o777, 0o644);
    let script = std::fs::read_to_string(&script_path).expect("healthcheck");
    assert_eq!(script, fleett::render::HEALTHCHECK_SCRIPT);
    assert!(script.contains("/opt/data/gateway.sock"));
    assert!(script.contains("\"verb\": \"identify\""));
    assert!(script.contains("TIMEOUT_S = 3.0"));

    // A later PATCH regenerates the probe as well.
    std::fs::write(&script_path, "tampered").expect("tamper");
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"notes": "rewrite"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        std::fs::read_to_string(&script_path).expect("healthcheck"),
        fleett::render::HEALTHCHECK_SCRIPT
    );
}

#[tokio::test]
async fn unhealthy_containers_are_reported_with_last_error() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    harness.mock.force_health("alice", Health::Unhealthy);
    let (status, body) = harness.request(Method::GET, "/api/instances", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = parse(&body);
    assert_eq!(list[0]["health"], "unhealthy");
    let last_error = list[0]["last_error"]
        .as_str()
        .expect("last_error is set for an unhealthy instance");
    assert!(last_error.contains("health check"), "{last_error}");

    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    let detail = parse(&body);
    assert_eq!(detail["health"], "unhealthy");
    // The process is up; only the probe fails.
    assert_eq!(detail["status"], "running");

    harness.mock.force_health("alice", Health::Healthy);
    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    let detail = parse(&body);
    assert_eq!(detail["health"], "healthy");
    assert!(detail["last_error"].is_null(), "{detail}");
}

// ---------------------------------------------------------------------------
// Resource limits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cpu_limit_is_validated_accepted_and_rendered() {
    let harness = Harness::new();
    let mut body = instance_body("alice");
    body["cpu_limit"] = json!(0.5);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
    assert_eq!(parse(&response)["cpu_limit"], 0.5);

    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    assert_eq!(compose["services"]["agent"]["cpus"], "0.5");
    assert_eq!(
        compose["services"]["agent"]["memswap_limit"],
        json!(format!("{}m", 1024))
    );

    let (_, body) = harness.request(Method::GET, "/api/instances", None).await;
    assert_eq!(parse(&body)[0]["cpu_limit"], 0.5);

    for bad in [json!(0.0), json!(0.05), json!(-1), json!(8.5), json!(100)] {
        let mut body = instance_body("bob");
        body["cpu_limit"] = bad.clone();
        let (status, response) = harness
            .request(Method::POST, "/api/instances", Some(body))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "cpu_limit {bad}");
        assert_eq!(parse(&response)["error"]["code"], "invalid_cpu_limit");
    }

    let (status, response) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"cpu_limit": 9.0})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&response)["error"]["code"], "invalid_cpu_limit");
}

#[tokio::test]
async fn total_cpu_cap_returns_409_with_details() {
    let harness = Harness::with_config(|cfg| cfg.max_total_cpu = 1.0);

    for (id, cpu) in [("alice", 0.5), ("bob", 0.5)] {
        let mut body = instance_body(id);
        body["cpu_limit"] = json!(cpu);
        let (status, response) = harness
            .request(Method::POST, "/api/instances", Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{response}");
    }

    let mut carol = instance_body("carol");
    carol["cpu_limit"] = json!(0.1);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(carol))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    let error = parse(&response)["error"].clone();
    assert_eq!(error["code"], "cpu_cap_exceeded");
    assert_eq!(error["details"]["used"], 1.0);
    assert_eq!(error["details"]["requested"], 0.1);
    assert_eq!(error["details"]["cap"], 1.0);
    // Nothing is written for a rejected create.
    assert!(!harness.instance_dir("carol").exists());
}

#[tokio::test]
async fn total_memory_cap_returns_409_with_details() {
    let harness = Harness::with_config(|cfg| cfg.max_total_mem_mb = 2048);
    create_instance(&harness, "alice").await;
    create_instance(&harness, "bob").await;

    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(instance_body("carol")))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    let error = parse(&response)["error"].clone();
    assert_eq!(error["code"], "mem_cap_exceeded");
    assert_eq!(error["details"]["used"], 2048);
    assert_eq!(error["details"]["requested"], 1024);
    assert_eq!(error["details"]["cap"], 2048);
}

#[tokio::test]
async fn patch_accounting_does_not_count_the_instance_twice() {
    let harness = Harness::with_config(|cfg| cfg.max_total_cpu = 1.0);
    for (id, cpu) in [("alice", 0.5), ("bob", 0.5)] {
        let mut body = instance_body(id);
        body["cpu_limit"] = json!(cpu);
        let (status, response) = harness
            .request(Method::POST, "/api/instances", Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{response}");
    }

    // Unchanged: alice must not be counted on top of her own row.
    let (status, response) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"cpu_limit": 0.5})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");

    // Shrinking always fits.
    let (status, response) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"cpu_limit": 0.1})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    assert_eq!(compose["services"]["agent"]["cpus"], "0.1");

    // Growing past the cap (0.5 for bob + 0.9 for alice > 1.0) is refused.
    let (status, response) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"cpu_limit": 0.9})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    let error = parse(&response)["error"].clone();
    assert_eq!(error["code"], "cpu_cap_exceeded");
    assert_eq!(error["details"]["used"], 0.5);
    assert_eq!(error["details"]["requested"], 0.9);
    assert_eq!(error["details"]["cap"], 1.0);

    // The refused patch changed nothing.
    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(parse(&body)["cpu_limit"], 0.1);
}

// ---------------------------------------------------------------------------
// Driver failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn driver_failures_return_502_with_stderr() {
    let harness = Harness::new();
    harness.mock.fail_next(
        "up",
        "Cannot connect to the Docker daemon at unix:///var/run/docker.sock",
    );
    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(instance_body("alice")))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let error = parse(&body);
    assert_eq!(error["error"]["code"], "driver_error");
    let message = error["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("Cannot connect to the Docker daemon"),
        "{message}"
    );
    assert!(message.contains("up"), "{message}");

    // The instance is kept in the error state so the failure is visible.
    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let detail = parse(&body);
    assert_eq!(detail["status"], "error");
    assert!(detail["last_error"]
        .as_str()
        .is_some_and(|value| value.contains("Cannot connect")));
}

#[tokio::test]
async fn driver_errors_are_truncated_to_two_kilobytes() {
    let harness = Harness::new();
    harness.mock.fail_next("up", "x".repeat(5000));
    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(instance_body("alice")))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let error = parse(&body);
    let message = error["error"]["message"].as_str().expect("message");
    assert!(
        message.chars().count() <= 2048 + 64,
        "message kept {} chars",
        message.chars().count()
    );
    assert!(message.ends_with('…'));
}

// ---------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn startup_reconciles_from_compose_files() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let (status, _) = harness
        .request(Method::DELETE, "/api/instances/alice?purge=true", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    create_instance(&harness, "bob").await;

    // Simulate a restart with a brand-new database and mock driver.
    let cfg = (*harness.state.cfg).clone();
    std::fs::remove_file(cfg.db_path()).expect("remove db");
    let second = AppState::bootstrap(cfg, Arc::new(MockDriver::new())).expect("second boot");
    let (status, _headers, body) = send_to(
        api::router(second),
        Method::GET,
        "/api/instances",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = parse(&body);
    let list = list.as_array().expect("array");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], "bob");
    assert_eq!(list[0]["model"], MODEL);
    assert_eq!(list[0]["template"], "custom");
}

#[tokio::test]
async fn patch_works_on_reconciled_instances() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    // Reboot with a fresh database: the instance is adopted as "custom".
    let cfg = (*harness.state.cfg).clone();
    std::fs::remove_file(cfg.db_path()).expect("remove db");
    let second = AppState::bootstrap(cfg, Arc::new(MockDriver::new())).expect("second boot");

    let (status, _headers, body) = send_to(
        api::router(second.clone()),
        Method::GET,
        "/api/instances/alice",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["template"], "custom");

    let (status, _headers, body) = send_to(
        api::router(second),
        Method::PATCH,
        "/api/instances/alice",
        Some(json!({"model": "reconciled-model", "mem_limit_mb": 768})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["model"], "reconciled-model");

    let config = read_yaml(&harness.instance_dir("alice").join("data/config.yaml"));
    assert_eq!(config["model"]["default"], "reconciled-model");
    assert!(harness.instance_dir("alice").join("data/SOUL.md").is_file());
    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    assert_eq!(compose["services"]["agent"]["mem_limit"], "768m");
}

#[tokio::test]
async fn startup_drops_index_rows_without_compose_file() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    // Delete the compose file behind the API's back, then reboot.
    std::fs::remove_file(harness.instance_dir("alice").join("compose.yaml")).expect("remove");

    let cfg = (*harness.state.cfg).clone();
    let second = AppState::bootstrap(cfg, Arc::new(MockDriver::new())).expect("second boot");
    let (status, _headers, body) = send_to(
        api::router(second),
        Method::GET,
        "/api/instances",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse(&body), json!([]));
}
