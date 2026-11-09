//! Integration tests for the HTTP API.
//!
//! Every test runs against `MockDriver`; no test ever calls Docker.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use fleett::api::{self, AppState};
use fleett::config::{Config, EgressMode};
use fleett::driver::mock::MockDriver;
use fleett::driver::InspectInfo;
use fleett::models::{Health, Status};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";
/// AES-256 key used by the harness unless a test opts out.
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];

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
        // Most tests exercise the encrypted path; the legacy/no-key paths opt
        // out explicitly in their own tests.
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
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

    /// Decrypted secrets of an instance, read from the control plane store.
    fn instance_secrets(&self, id: &str) -> BTreeMap<String, String> {
        fleett::envs::values(&self.state.cfg, &self.state.db, id).expect("secrets readable")
    }

    /// Every file under the instance directory, concatenated.
    fn instance_files(&self, id: &str) -> String {
        fn read_tree(dir: &Path, out: &mut String) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    read_tree(&path, out);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push_str(&text);
                    out.push('\n');
                }
            }
        }
        let mut out = String::new();
        read_tree(&self.instance_dir(id), &mut out);
        out
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
    assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
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

    // Templates suggest the allowlist variables so the console can propose
    // them instead of making the operator guess.
    let discord = templates
        .iter()
        .find(|template| template["id"] == "guest-discord")
        .expect("guest-discord template");
    let discord_env = discord["env"].as_array().expect("env suggestions");
    let keys: Vec<&str> = discord_env
        .iter()
        .filter_map(|entry| entry["key"].as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "DISCORD_ALLOWED_USERS",
            "DISCORD_ALLOWED_ROLES",
            "DISCORD_ALLOWED_CHANNELS",
            "DISCORD_ALLOW_ALL_USERS"
        ]
    );
    assert!(discord_env.iter().all(|entry| entry["secret"] == false));
    assert!(discord_env.iter().all(|entry| entry["description"]
        .as_str()
        .is_some_and(|text| !text.is_empty())));
    let guest_env: Vec<&str> = guest["env"]
        .as_array()
        .expect("guest env suggestions")
        .iter()
        .filter_map(|entry| entry["key"].as_str())
        .collect();
    assert_eq!(
        guest_env,
        ["TELEGRAM_ALLOWED_USERS", "GATEWAY_ALLOW_ALL_USERS"]
    );
    assert_eq!(
        templates
            .iter()
            .find(|template| template["id"] == "solo")
            .expect("solo template")["env"],
        json!([])
    );
}

// ---------------------------------------------------------------------------
// Create / read / list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn creates_instance_and_writes_files() {
    let harness = Harness::new();
    let mut body = instance_body("alice");
    body["gate_token"] = json!("gate-secret");
    body["env"] = json!([
        {"key": "EXTRA_FLAG", "value": "1", "secret": false},
        {"key": "PROVIDER_TOKEN", "value": "provider-secret", "secret": true}
    ]);

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
    assert!(!response.contains("provider-secret"));

    // compose.yaml parses and names the project.
    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    assert_eq!(compose["name"], "fleett-alice");
    assert!(compose["services"]["agent"].get("env_file").is_none());
    assert_eq!(
        compose["services"]["agent"]["environment"]["TELEGRAM_BOT_TOKEN"],
        "${TELEGRAM_BOT_TOKEN}"
    );
    assert_eq!(
        compose["services"]["agent"]["environment"]["PROVIDER_TOKEN"],
        "${PROVIDER_TOKEN}"
    );
    assert_eq!(
        compose["services"]["agent"]["environment"]["EXTRA_FLAG"], "${EXTRA_FLAG}",
        "a non-secret value is interpolated too, never written in the file"
    );

    // No plaintext secrets file: the values are encrypted in the index.
    let secrets_path = harness.instance_dir("alice").join("secrets.env");
    assert!(!secrets_path.exists());
    let secrets = harness.instance_secrets("alice");
    assert_eq!(
        secrets.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
        Some("123:ABC")
    );
    assert_eq!(
        secrets.get("HERMES_GATE_PROFILE_TOKEN").map(String::as_str),
        Some("gate-secret")
    );
    assert_eq!(secrets.get("EXTRA_FLAG").map(String::as_str), Some("1"));
    assert_eq!(
        secrets.get("PROVIDER_TOKEN").map(String::as_str),
        Some("provider-secret")
    );
    let on_disk = harness.instance_files("alice");
    assert!(!on_disk.contains("123:ABC"), "{on_disk}");
    assert!(!on_disk.contains("gate-secret"), "{on_disk}");
    assert!(!on_disk.contains("provider-secret"), "{on_disk}");

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

    let env = detail["env"].as_array().expect("env list");
    let keys: Vec<&str> = env
        .iter()
        .filter_map(|entry| entry["key"].as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "EXTRA_FLAG",
            "HERMES_GATE_PROFILE_TOKEN",
            "PROVIDER_TOKEN",
            "TELEGRAM_BOT_TOKEN"
        ]
    );
    let flag = env
        .iter()
        .find(|entry| entry["key"] == "EXTRA_FLAG")
        .expect("plain entry");
    assert_eq!(flag["secret"], false);
    assert_eq!(flag["value"], "1");
    let provider = env
        .iter()
        .find(|entry| entry["key"] == "PROVIDER_TOKEN")
        .expect("secret entry");
    assert_eq!(provider["secret"], true);
    assert!(provider.get("value").is_none(), "{provider}");
    assert!(provider["updated_at"].is_string());
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
    let secrets = harness.instance_secrets("alice");
    assert_eq!(
        secrets.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
        Some("123:ABC")
    );
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

    let secrets = harness.instance_secrets("alice");
    assert_eq!(
        secrets.get("DISCORD_BOT_TOKEN").map(String::as_str),
        Some("discord-secret")
    );
    assert!(!secrets.contains_key("TELEGRAM_BOT_TOKEN"));
    assert!(!harness.instance_dir("alice").join("secrets.env").exists());

    // The compose file follows the new channel: the old key is gone, the new
    // one is interpolated from the driver environment.
    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    let environment = compose["services"]["agent"]["environment"]
        .as_object()
        .expect("environment");
    assert_eq!(environment["DISCORD_BOT_TOKEN"], "${DISCORD_BOT_TOKEN}");
    assert!(!environment.contains_key("TELEGRAM_BOT_TOKEN"));

    let config = read_yaml(&harness.instance_dir("alice").join("data/config.yaml"));
    assert_eq!(config["platforms"]["discord"]["enabled"], true);
    assert_eq!(config["platforms"]["telegram"]["enabled"], false);
}

// ---------------------------------------------------------------------------
// Editable environment (issue #22)
// ---------------------------------------------------------------------------

/// Create an instance with no messaging platform, so its environment is easy
/// to reason about.
async fn create_solo(harness: &Harness, id: &str) -> Value {
    let body = json!({
        "id": id,
        "name": format!("Agent {id}"),
        "template": "solo",
        "model": MODEL,
        "base_url": BASE_URL,
        "channel": {"type": "none"},
    });
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {response}");
    parse(&response)
}

fn env_keys(detail: &Value) -> Vec<&str> {
    detail["env"]
        .as_array()
        .expect("env list")
        .iter()
        .filter_map(|entry| entry["key"].as_str())
        .collect()
}

#[tokio::test]
async fn put_env_adds_plain_and_secret_variables_and_applies() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let ups_before = harness
        .mock
        .calls()
        .iter()
        .filter(|call| call.operation == "up" && call.instance == "alice")
        .count();

    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [
                {"key": "DISCORD_ALLOWED_USERS", "value": "123,456", "secret": false},
                {"key": "OPENAI_KEY", "value": "sk-secret", "secret": true},
                {"key": "TELEGRAM_BOT_TOKEN", "secret": true}
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("sk-secret"), "{body}");
    assert!(!body.contains("123:ABC"), "{body}");

    let detail = parse(&body);
    assert_eq!(
        env_keys(&detail),
        ["DISCORD_ALLOWED_USERS", "OPENAI_KEY", "TELEGRAM_BOT_TOKEN"]
    );
    let env = detail["env"].as_array().expect("env");
    let allowed = env
        .iter()
        .find(|entry| entry["key"] == "DISCORD_ALLOWED_USERS")
        .expect("plain entry");
    assert_eq!(allowed["value"], "123,456");
    assert_eq!(allowed["secret"], false);
    let key = env
        .iter()
        .find(|entry| entry["key"] == "OPENAI_KEY")
        .expect("secret entry");
    assert_eq!(key["secret"], true);
    assert!(
        key.get("value").is_none(),
        "a secret never returns its value"
    );

    // The stored values: the plain one in clear, the secret one encrypted, and
    // the platform token the request did not resend kept as it was.
    let rows = harness
        .state
        .db
        .list_instance_env("alice")
        .expect("env rows");
    let plain = rows
        .iter()
        .find(|row| row.key == "DISCORD_ALLOWED_USERS")
        .expect("plain row");
    assert_eq!(plain.value.as_deref(), Some("123,456"));
    assert!(plain.ciphertext.is_none());
    let secret = rows
        .iter()
        .find(|row| row.key == "OPENAI_KEY")
        .expect("secret row");
    assert!(secret.value.is_none());
    assert!(secret.ciphertext.is_some());
    assert_eq!(
        harness
            .instance_secrets("alice")
            .get("TELEGRAM_BOT_TOKEN")
            .map(String::as_str),
        Some("123:ABC"),
        "a secret resent without a value keeps the stored one"
    );

    // The container is recreated through a rewritten compose file that only
    // interpolates names.
    let compose = std::fs::read_to_string(harness.instance_dir("alice").join("compose.yaml"))
        .expect("compose");
    for name in ["DISCORD_ALLOWED_USERS", "OPENAI_KEY", "TELEGRAM_BOT_TOKEN"] {
        assert!(
            compose.contains(&format!("{name}: ${{{name}}}")),
            "{name} missing from {compose}"
        );
    }
    assert!(!compose.contains("123,456"));
    assert!(!compose.contains("sk-secret"));
    let ups_after = harness
        .mock
        .calls()
        .iter()
        .filter(|call| call.operation == "up" && call.instance == "alice")
        .count();
    assert_eq!(ups_after, ups_before + 1, "the change must be applied");

    let actions: Vec<String> = harness
        .state
        .db
        .list_audit(20)
        .expect("audit")
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert!(
        actions.contains(&"instance.env.update".to_string()),
        "{actions:?}"
    );
    let audit = harness.state.db.list_audit(5).expect("audit");
    let entry = audit
        .iter()
        .find(|entry| entry.action == "instance.env.update")
        .expect("env update entry");
    assert_eq!(entry.target, "alice");
    let details = entry.details.clone().expect("details");
    assert!(details.contains("OPENAI_KEY"), "{details}");
    assert!(
        !details.contains("sk-secret") && !details.contains("123:ABC"),
        "an audit detail must never carry a value: {details}"
    );
}

#[tokio::test]
async fn put_env_replaces_the_whole_set() {
    let harness = Harness::new();
    create_solo(&harness, "alice").await;
    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [
                {"key": "KEEP_ME", "value": "a", "secret": false},
                {"key": "DROP_ME", "value": "b", "secret": false}
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [{"key": "KEEP_ME", "value": "c", "secret": false}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = parse(&body);
    assert_eq!(env_keys(&detail), ["KEEP_ME"]);
    assert_eq!(detail["env"][0]["value"], "c");

    let compose = std::fs::read_to_string(harness.instance_dir("alice").join("compose.yaml"))
        .expect("compose");
    assert!(!compose.contains("DROP_ME"), "{compose}");
    assert!(compose.contains("KEEP_ME: ${KEEP_ME}"), "{compose}");

    // An empty list clears the environment (and the compose declarations).
    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": []})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(env_keys(&parse(&body)), Vec::<&str>::new());
}

#[tokio::test]
async fn delete_env_removes_one_variable() {
    let harness = Harness::new();
    create_solo(&harness, "alice").await;
    harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [
                {"key": "DELETE_ME", "value": "1", "secret": false},
                {"key": "KEEP_ME", "value": "2", "secret": false}
            ]})),
        )
        .await;
    let ups_before = harness
        .mock
        .calls()
        .iter()
        .filter(|call| call.operation == "up" && call.instance == "alice")
        .count();

    let (status, body) = harness
        .request(Method::DELETE, "/api/instances/alice/env/DELETE_ME", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let ups_after = harness
        .mock
        .calls()
        .iter()
        .filter(|call| call.operation == "up" && call.instance == "alice")
        .count();
    assert_eq!(ups_after, ups_before + 1, "the removal must be applied");

    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(env_keys(&parse(&body)), ["KEEP_ME"]);
    let compose = std::fs::read_to_string(harness.instance_dir("alice").join("compose.yaml"))
        .expect("compose");
    assert!(!compose.contains("DELETE_ME"), "{compose}");
    assert!(compose.contains("KEEP_ME: ${KEEP_ME}"), "{compose}");

    let actions: Vec<String> = harness
        .state
        .db
        .list_audit(20)
        .expect("audit")
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert!(
        actions.contains(&"instance.env.delete".to_string()),
        "{actions:?}"
    );

    // Deleting an unknown key is a 404, not a silent success.
    let (status, body) = harness
        .request(Method::DELETE, "/api/instances/alice/env/ABSENT", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn patch_env_merges_deletes_and_keeps_empty_values() {
    let harness = Harness::new();
    create_solo(&harness, "alice").await;
    harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [
                {"key": "EMPTIED", "value": "old", "secret": false},
                {"key": "DELETED", "value": "old", "secret": false},
                {"key": "KEPT", "value": "same", "secret": false}
            ]})),
        )
        .await;

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"env": [
                {"key": "EMPTIED", "value": ""},
                {"key": "DELETED", "value": null},
                {"key": "ADDED", "value": "new"}
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = parse(&body);
    assert_eq!(env_keys(&detail), ["ADDED", "EMPTIED", "KEPT"]);
    let env = detail["env"].as_array().expect("env");
    assert_eq!(env[1]["key"], "EMPTIED");
    assert_eq!(env[1]["value"], "", "an empty string is a kept value");
    let added = env
        .iter()
        .find(|entry| entry["key"] == "ADDED")
        .expect("added");
    assert_eq!(added["secret"], false, "a new key defaults to non-secret");

    let compose = std::fs::read_to_string(harness.instance_dir("alice").join("compose.yaml"))
        .expect("compose");
    assert!(!compose.contains("DELETED"), "{compose}");
    assert!(compose.contains("EMPTIED: ${EMPTIED}"), "{compose}");
    assert!(compose.contains("KEPT: ${KEPT}"), "{compose}");
}

#[tokio::test]
async fn env_validation_refuses_reserved_duplicate_and_oversized_entries() {
    let harness = Harness::new();
    create_solo(&harness, "alice").await;

    let cases = [
        (
            json!([{"key": "HERMES_UID", "value": "0", "secret": false}]),
            "reserved_env",
        ),
        (
            json!([{"key": "PATH", "value": "/bin", "secret": false}]),
            "reserved_env",
        ),
        (
            json!([{"key": "FLEETT_ROOT", "value": "/tmp", "secret": false}]),
            "reserved_env",
        ),
        (
            json!([
                {"key": "DUP", "value": "1", "secret": false},
                {"key": "DUP", "value": "2", "secret": false}
            ]),
            "duplicate_env",
        ),
        (
            json!([{"key": "lower_case", "value": "1", "secret": false}]),
            "invalid_env",
        ),
        (
            json!([{"key": "TOO_LONG", "value": "x".repeat(8 * 1024 + 1), "secret": false}]),
            "invalid_env",
        ),
        (
            json!([{"key": "NO_SECRET_FLAG", "value": "1"}]),
            "invalid_env",
        ),
    ];
    for (env, code) in cases {
        let (status, body) = harness
            .request(
                Method::PUT,
                "/api/instances/alice/env",
                Some(json!({ "env": env })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{code}: {body}");
        assert_eq!(parse(&body)["error"]["code"], code, "{body}");
    }

    // A reserved key is refused at creation too.
    let mut body = instance_body("bob");
    body["env"] = json!([{"key": "HOME", "value": "/root", "secret": false}]);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert_eq!(parse(&response)["error"]["code"], "reserved_env");

    // The `env` field itself is required on a full replace.
    let (status, body) = harness
        .request(Method::PUT, "/api/instances/alice/env", Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn put_env_refuses_a_new_secret_without_a_value() {
    let harness = Harness::new();
    create_solo(&harness, "alice").await;
    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [{"key": "NEW_SECRET", "secret": true}]})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_env");
}

#[tokio::test]
async fn put_env_records_a_failed_apply_in_last_error() {
    let harness = Harness::new();
    create_solo(&harness, "alice").await;
    harness.mock.fail_next("up", "compose exploded");

    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [{"key": "PLAIN", "value": "1", "secret": false}]})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(
        harness
            .state
            .db
            .get("alice")
            .expect("get")
            .expect("stored")
            .status,
        Status::Error,
        "a failed apply must be recorded"
    );

    // The values were stored, the failure is visible, and the operator can
    // retry with another write or a restart.
    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = parse(&body);
    assert!(
        detail["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("compose exploded")),
        "{detail}"
    );
    assert_eq!(env_keys(&detail), ["PLAIN"]);
}

#[tokio::test]
async fn env_writes_need_the_secrets_key() {
    let harness = Harness::with_config(|cfg| {
        cfg.secrets_key = None;
        cfg.secrets_key_file = cfg.root.with_extension("missing.key");
    });
    create_solo(&harness, "alice").await;

    // A plain variable needs no key, at creation or through the API.
    let mut body = json!({
        "id": "bob",
        "name": "Agent bob",
        "template": "solo",
        "model": MODEL,
        "base_url": BASE_URL,
        "channel": {"type": "none"},
        "env": [{"key": "PLAIN", "value": "1", "secret": false}],
    });
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
    assert_eq!(parse(&response)["env"][0]["value"], "1");
    body["id"] = json!("carol");
    body["env"] = json!([{"key": "TOKEN", "value": "secret", "secret": true}]);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(
        parse(&response)["error"]["code"],
        "secrets_key_missing",
        "{response}"
    );

    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [{"key": "PLAIN", "value": "1", "secret": false}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [{"key": "TOKEN", "value": "s", "secret": true}]})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "secrets_key_missing");
}

#[tokio::test]
async fn the_startup_migration_imports_instance_secrets() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let values = harness.instance_secrets("alice");
    assert!(!values.is_empty());

    // Roll back to the pre-0.5 store: one encrypted map instead of rows.
    harness
        .state
        .db
        .delete_instance_env("alice")
        .expect("drop rows");
    let key = fleett::secrets::SecretsKey::from_config(&harness.state.cfg)
        .expect("key readable")
        .expect("key configured");
    let sealed = fleett::secrets::encrypt_map(&key, &values).expect("encrypt");
    harness
        .state
        .db
        .put_sealed("alice", &sealed.nonce, &sealed.ciphertext)
        .expect("sealed row");

    // A fresh state (what a service restart does) imports it.
    let cfg = (*harness.state.cfg).clone();
    let state = AppState::bootstrap_with_db(cfg, harness.mock.clone(), harness.state.db.clone())
        .expect("bootstrap");
    assert!(state.db.sealed("alice").expect("sealed").is_none());
    let rows = state.db.list_instance_env("alice").expect("rows");
    assert_eq!(rows.len(), values.len());
    assert!(rows.iter().all(|row| row.secret));
    assert_eq!(
        fleett::envs::values(&state.cfg, &state.db, "alice").expect("values"),
        values
    );
}

// ---------------------------------------------------------------------------
// Secrets at rest
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_without_a_key_is_refused_with_a_hint() {
    let harness = Harness::with_config(|cfg| {
        cfg.secrets_key = None;
        cfg.secrets_key_file = cfg.root.with_extension("missing.key");
    });

    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(instance_body("alice")))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    let error = parse(&response)["error"].clone();
    assert_eq!(error["code"], "secrets_key_missing");
    let message = error["message"].as_str().expect("message");
    assert!(message.contains("fleett secrets init"), "{message}");

    // Nothing was written: no directory, no row.
    assert!(!harness.instance_dir("alice").exists());
    assert!(harness.state.db.sealed("alice").expect("sealed").is_none());
}

#[tokio::test]
async fn a_legacy_secrets_env_is_read_but_writes_need_a_key() {
    let harness = Harness::with_config(|cfg| {
        cfg.secrets_key = None;
        cfg.secrets_key_file = cfg.root.with_extension("missing.key");
    });

    // An instance without any secret needs no key at all.
    let body = json!({
        "id": "alice",
        "name": "Agent alice",
        "template": "solo",
        "model": MODEL,
        "base_url": BASE_URL,
        "channel": {"type": "none"},
    });
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
    assert!(!harness.instance_dir("alice").join("secrets.env").exists());

    // Simulate an instance adopted from an older release: plaintext file on
    // disk, no encrypted row.
    std::fs::write(
        harness.instance_dir("alice").join("secrets.env"),
        "TELEGRAM_BOT_TOKEN=111:OLD\n",
    )
    .expect("legacy secrets");

    // The legacy token is still read (and masked in the detail) without a key.
    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = parse(&body);
    let env = detail["env"].as_array().expect("env");
    let token = env
        .iter()
        .find(|entry| entry["key"] == "TELEGRAM_BOT_TOKEN")
        .expect("legacy token listed");
    assert_eq!(token["secret"], true);
    assert!(token.get("value").is_none());
    assert_eq!(
        harness
            .instance_secrets("alice")
            .get("TELEGRAM_BOT_TOKEN")
            .map(String::as_str),
        Some("111:OLD")
    );

    // Rotating a token through the API now needs the key: the control plane
    // never writes a plaintext token again.
    let (status, response) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"channel": {"type": "telegram", "token": "222:NEW"}})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(
        parse(&response)["error"]["code"],
        "secrets_key_missing",
        "{response}"
    );
    let text = std::fs::read_to_string(harness.instance_dir("alice").join("secrets.env"))
        .expect("legacy secrets kept");
    assert!(text.contains("TELEGRAM_BOT_TOKEN=111:OLD"), "{text}");

    // The detail still warns about the plaintext file.
    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    let warnings = parse(&body)["warnings"].clone();
    assert!(
        warnings
            .as_array()
            .expect("warnings")
            .iter()
            .filter_map(Value::as_str)
            .any(|warning| warning.contains("plain text")),
        "{warnings:?}"
    );
}

#[tokio::test]
async fn secrets_migration_dry_run_then_apply() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    // Roll the instance back to what an older release left on disk: the stored
    // environment removed, a plaintext file, a compose file with env_file.
    harness
        .state
        .db
        .delete_instance_env("alice")
        .expect("drop env rows");
    harness.state.db.delete_sealed("alice").expect("drop row");
    std::fs::write(
        harness.instance_dir("alice").join("secrets.env"),
        "TELEGRAM_BOT_TOKEN=123:ABC\nHERMES_GATE_PROFILE_TOKEN=gate-secret\n",
    )
    .expect("legacy secrets");
    let legacy_compose = "name: fleett-alice\n\
                          services:\n\
                          \x20 agent:\n\
                          \x20   image: hermes-agent:0.22.2-local\n\
                          \x20   env_file:\n\
                          \x20     - secrets.env\n\
                          \x20   environment:\n\
                          \x20     HERMES_UID: '10000'\n";
    std::fs::write(
        harness.instance_dir("alice").join("compose.yaml"),
        legacy_compose,
    )
    .expect("legacy compose");

    // Dry run: the plan is reported, nothing changes.
    let dry = fleett::secrets::migrate(&harness.state.cfg, false).expect("dry run");
    assert!(!dry.applied);
    assert_eq!(dry.entries.len(), 1);
    assert_eq!(dry.entries[0].instance_id, "alice");
    assert!(dry.entries[0].compose_references_env_file);
    assert!(harness.instance_dir("alice").join("secrets.env").is_file());
    assert_eq!(
        std::fs::read_to_string(harness.instance_dir("alice").join("compose.yaml"))
            .expect("compose"),
        legacy_compose
    );

    // Apply: encrypted, compose rewritten, plaintext deleted.
    let applied = fleett::secrets::migrate(&harness.state.cfg, true).expect("apply");
    assert_eq!(applied.migrated, vec!["alice".to_string()]);
    assert!(applied.failures.is_empty(), "{:?}", applied.failures);
    assert!(!harness.instance_dir("alice").join("secrets.env").exists());

    let compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    let agent = &compose["services"]["agent"];
    assert!(agent.get("env_file").is_none());
    assert_eq!(
        agent["environment"]["TELEGRAM_BOT_TOKEN"],
        "${TELEGRAM_BOT_TOKEN}"
    );
    assert_eq!(agent["environment"]["HERMES_UID"], "10000");

    let secrets = harness.instance_secrets("alice");
    assert_eq!(
        secrets.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
        Some("123:ABC")
    );
    assert!(!harness.instance_files("alice").contains("123:ABC"));
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
    assert!(
        harness.state.db.sealed("alice").expect("sealed").is_none(),
        "the legacy secrets row must be deleted with the instance"
    );
    assert!(
        harness
            .state
            .db
            .list_instance_env("alice")
            .expect("env rows")
            .is_empty(),
        "the environment rows must be deleted with the instance"
    );
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
    assert!(agent.get("env_file").is_none());
    assert_eq!(
        agent["environment"]["TELEGRAM_BOT_TOKEN"],
        "${TELEGRAM_BOT_TOKEN}"
    );
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

// ---------------------------------------------------------------------------
// Logs: filtering, structured output and error summaries (#12)
// ---------------------------------------------------------------------------

/// Mixed log stream: timestamps, a warning, an error, a clean line.
const MIXED_LOGS: &str = "\
2026-11-08T01:00:00Z starting gateway
fleett-alice  | 2026-11-08T01:01:00Z WARNING: slow upstream
2026-11-08T01:02:00Z ERROR: gateway.sock refused
no timestamp on this line
";

#[tokio::test]
async fn logs_can_be_grepped_and_filtered_by_level() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    harness.mock.set_logs("alice", MIXED_LOGS);

    let (status, headers, body) = harness
        .send(
            Method::GET,
            "/api/instances/alice/logs?grep=gateway[.]sock",
            None,
            Some(TOKEN),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(body.lines().count(), 1);
    assert!(body.contains("gateway.sock refused"), "{body}");

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/logs?level=error", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.lines().count(), 1);
    assert!(body.contains("ERROR"), "{body}");

    // `warn` means warnings and above.
    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/logs?level=warn", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.lines().count(), 2, "{body}");

    // grep and level combine.
    let (status, body) = harness
        .request(
            Method::GET,
            "/api/instances/alice/logs?level=warn&grep=upstream",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.lines().count(), 1, "{body}");
    assert!(body.contains("slow upstream"), "{body}");

    for (query, code) in [
        ("level=debug", "invalid_level"),
        ("grep=%28unclosed", "invalid_grep"),
        ("format=xml", "invalid_format"),
        ("since=soon", "invalid_since"),
        ("since=", "invalid_since"),
    ] {
        let (status, body) = harness
            .request(
                Method::GET,
                &format!("/api/instances/alice/logs?{query}"),
                None,
            )
            .await;
        if query == "since=" {
            // An empty value is treated as absent, not as an error.
            assert_eq!(status, StatusCode::OK, "{query}: {body}");
            continue;
        }
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        assert_eq!(parse(&body)["error"]["code"], code, "{query}: {body}");
    }
}

#[tokio::test]
async fn logs_json_output_is_structured() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    harness.mock.set_logs("alice", MIXED_LOGS);

    let (status, headers, body) = harness
        .send(
            Method::GET,
            "/api/instances/alice/logs?format=json",
            None,
            Some(TOKEN),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let entries = parse(&body);
    let entries = entries.as_array().expect("array");
    assert_eq!(entries.len(), 4, "{body}");
    assert_eq!(
        entries[0],
        json!({"time": "2026-11-08T01:00:00Z", "message": "starting gateway"})
    );
    assert_eq!(
        entries[1],
        json!({"time": "2026-11-08T01:01:00Z", "message": "WARNING: slow upstream"}),
        "the docker compose prefix is not part of the timestamp"
    );
    assert_eq!(entries[3], json!({"line": "no timestamp on this line"}));

    // A filtered JSON response keeps the shape.
    let (status, body) = harness
        .request(
            Method::GET,
            "/api/instances/alice/logs?format=json&level=error",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let entries = parse(&body);
    assert_eq!(entries.as_array().expect("array").len(), 1);
    assert_eq!(entries[0]["time"], "2026-11-08T01:02:00Z");
    assert_eq!(entries[0]["message"], "ERROR: gateway.sock refused");
}

#[tokio::test]
async fn logs_forward_the_tail_and_since_to_the_driver() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    let (status, body) = harness
        .request(
            Method::GET,
            "/api/instances/alice/logs?tail=7&since=10m",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (instance, options) = harness.mock.logs_calls().pop().expect("logs call");
    assert_eq!(instance, "alice");
    assert_eq!(options.tail, 7);
    assert_eq!(options.since.as_deref(), Some("10m"));

    let (status, _) = harness
        .request(
            Method::GET,
            "/api/instances/alice/logs?since=2026-11-08T01%3A00%3A00Z",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, options) = harness.mock.logs_calls().pop().expect("logs call");
    assert_eq!(options.since.as_deref(), Some("2026-11-08T01:00:00Z"));
    assert_eq!(options.tail, 200, "the default tail is forwarded");

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/logs?tail=0", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "invalid_tail");
}

#[tokio::test]
async fn errors_endpoint_summarises_the_recent_failures() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    harness.mock.set_logs("alice", MIXED_LOGS);

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/errors", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let summary = parse(&body);
    assert_eq!(summary["count"], 2, "{body}");
    assert_eq!(
        summary["last"],
        "2026-11-08T01:02:00Z ERROR: gateway.sock refused"
    );
    assert_eq!(summary["last_at"], "2026-11-08T01:02:00Z");

    let (_, options) = harness.mock.logs_calls().pop().expect("logs call");
    assert_eq!(options.since.as_deref(), Some("24h"), "default window");
    assert_eq!(options.tail, 2000, "the scan is bounded");

    let (status, _) = harness
        .request(Method::GET, "/api/instances/alice/errors?since=2h", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, options) = harness.mock.logs_calls().pop().expect("logs call");
    assert_eq!(options.since.as_deref(), Some("2h"));

    harness.mock.set_logs("alice", "all good\n");
    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/errors", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let summary = parse(&body);
    assert_eq!(summary["count"], 0);
    assert!(summary["last"].is_null(), "{summary}");
    assert!(summary["last_at"].is_null(), "{summary}");

    let (status, body) = harness
        .request(
            Method::GET,
            "/api/instances/alice/errors?since=someday",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "invalid_since");

    let (status, body) = harness
        .request(Method::GET, "/api/instances/ghost/errors", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(parse(&body)["error"]["code"], "not_found");
}

#[tokio::test]
async fn the_detail_reports_the_docker_inspect_failure() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse(&body)["last_error"].is_null(), "{body}");

    harness.mock.force_status("alice", Status::Error);
    harness.mock.force_health("alice", Health::Unknown);
    harness.mock.force_inspect(
        "alice",
        InspectInfo {
            state: "exited".to_string(),
            exit_code: Some(137),
            oom_killed: true,
            error: Some("task: non-zero exit (137)".to_string()),
            started_at: None,
        },
    );

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = parse(&body);
    let last_error = detail["last_error"].as_str().expect("last error");
    assert!(last_error.contains("OOM"), "{last_error}");
    assert!(last_error.contains("137"), "{last_error}");

    // Persisted: the list shows the same cause without another inspect call.
    let (status, body) = harness.request(Method::GET, "/api/instances", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = parse(&body);
    assert_eq!(list[0]["last_error"], detail["last_error"]);
    assert!(harness
        .mock
        .calls()
        .iter()
        .any(|call| call.operation == "inspect" && call.instance == "alice"));
}

// ---------------------------------------------------------------------------
// Clone (#9)
// ---------------------------------------------------------------------------

async fn clone_instance(harness: &Harness, source: &str, body: Value) -> (StatusCode, Value) {
    let (status, response) = harness
        .request(
            Method::POST,
            &format!("/api/instances/{source}/clone"),
            Some(body),
        )
        .await;
    (status, parse(&response))
}

#[tokio::test]
async fn clone_creates_an_independent_instance() {
    let harness = Harness::new();
    create_share(&harness, "findings").await;
    let mut body = instance_body("alice");
    body["gate_token"] = json!("gate-alice");
    body["shares"] = json!([{"name": "findings", "mode": "rw"}]);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
    std::fs::write(
        harness.instance_dir("alice").join("data/MEMORY.md"),
        "remember alice",
    )
    .expect("memory file");

    let (status, detail) = clone_instance(
        &harness,
        "alice",
        json!({
            "new_id": "bob",
            "name": "Bob",
            "channel_token": "999:BOB",
            "gate_token": "gate-bob",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{detail}");
    assert_eq!(detail["id"], "bob");
    assert_eq!(detail["name"], "Bob");
    assert_eq!(detail["status"], "running");
    assert_eq!(
        detail["shares"],
        json!([{"name": "findings", "mode": "rw"}])
    );
    assert_eq!(detail["model"], MODEL);

    // Own compose project, own container, own directory.
    let clone_compose = read_yaml(&harness.instance_dir("bob").join("compose.yaml"));
    assert_eq!(clone_compose["name"], "fleett-bob");
    assert_eq!(
        clone_compose["services"]["agent"]["container_name"],
        "fleett-bob"
    );
    let source_compose = read_yaml(&harness.instance_dir("alice").join("compose.yaml"));
    assert_eq!(source_compose["name"], "fleett-alice");
    assert_ne!(
        harness.instance_dir("alice").join("data"),
        harness.instance_dir("bob").join("data")
    );

    // The data copy keeps the memory files; the generated files are the
    // clone's own (same model here, but a different instance directory).
    assert_eq!(
        std::fs::read_to_string(harness.instance_dir("bob").join("data/MEMORY.md"))
            .expect("memory copied"),
        "remember alice"
    );
    let clone_config = read_yaml(&harness.instance_dir("bob").join("data/config.yaml"));
    assert_eq!(clone_config["model"]["default"], MODEL);
    assert!(harness.instance_dir("bob").join("data/SOUL.md").is_file());
    assert!(harness
        .instance_dir("bob")
        .join("data/healthcheck.py")
        .is_file());

    // Tokens are the replacements, never the source ones.
    let source_secrets = harness.instance_secrets("alice");
    assert_eq!(
        source_secrets.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
        Some("123:ABC")
    );
    assert_eq!(
        source_secrets
            .get("HERMES_GATE_PROFILE_TOKEN")
            .map(String::as_str),
        Some("gate-alice")
    );
    let clone_secrets = harness.instance_secrets("bob");
    assert_eq!(
        clone_secrets.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
        Some("999:BOB")
    );
    assert_eq!(
        clone_secrets
            .get("HERMES_GATE_PROFILE_TOKEN")
            .map(String::as_str),
        Some("gate-bob")
    );
    assert!(!harness.instance_files("bob").contains("gate-alice"));

    // Two rows, two containers.
    let (status, body) = harness.request(Method::GET, "/api/instances", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = parse(&body);
    assert_eq!(list.as_array().expect("array").len(), 2);
    let calls = harness.mock.calls();
    let ups: Vec<&str> = calls
        .iter()
        .filter(|call| call.operation == "up" && call.instance == "bob")
        .map(|call| call.instance.as_str())
        .collect();
    assert_eq!(ups, ["bob"]);

    let actions: Vec<String> = harness
        .state
        .db
        .list_audit(10)
        .expect("audit")
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert!(
        actions.contains(&"instance.clone".to_string()),
        "{actions:?}"
    );
}

#[tokio::test]
async fn clone_without_replacement_tokens_is_refused_before_writing() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await; // holds TELEGRAM_BOT_TOKEN only

    let (status, error) = clone_instance(&harness, "alice", json!({ "new_id": "bob" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], "clone_token_required");
    assert_eq!(
        error["error"]["details"]["missing"],
        json!(["TELEGRAM_BOT_TOKEN"])
    );
    assert!(!harness.instance_dir("bob").exists(), "nothing written");
    assert!(harness.state.db.get("bob").expect("get").is_none());
    assert!(harness.state.db.sealed("bob").expect("sealed").is_none());
    assert!(
        harness.instance_dir("alice").join("compose.yaml").is_file(),
        "the source is untouched"
    );

    // A gate token does not replace the channel token.
    let (status, error) = clone_instance(
        &harness,
        "alice",
        json!({"new_id": "carol", "gate_token": "gate"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error["error"]["details"]["missing"],
        json!(["TELEGRAM_BOT_TOKEN"])
    );

    // A missing source and a duplicate id are rejected too.
    let (status, error) = clone_instance(
        &harness,
        "ghost",
        json!({"new_id": "bob", "channel_token": "1:2"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "not_found");

    let (status, error) = clone_instance(
        &harness,
        "alice",
        json!({"new_id": "alice", "channel_token": "1:2"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "invalid_id");
}

#[tokio::test]
async fn clone_can_start_empty_and_without_shares() {
    let harness = Harness::new();
    create_share(&harness, "findings").await;
    let mut body = instance_body("alice");
    body["shares"] = json!([{"name": "findings", "mode": "rw"}]);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
    std::fs::write(
        harness.instance_dir("alice").join("data/MEMORY.md"),
        "remember alice",
    )
    .expect("memory file");

    let (status, detail) = clone_instance(
        &harness,
        "alice",
        json!({
            "new_id": "bob",
            "channel_token": "999:BOB",
            "model": "clone-model",
            "copy_data": false,
            "shares_inherit": false,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{detail}");
    assert_eq!(detail["shares"], json!([]));
    assert_eq!(detail["model"], "clone-model");
    assert_eq!(detail["name"], "Agent alice copy", "default display name");
    assert!(
        !harness.instance_dir("bob").join("data/MEMORY.md").exists(),
        "copy_data=false must start empty"
    );
    // The generated files are always written.
    assert!(harness
        .instance_dir("bob")
        .join("data/config.yaml")
        .is_file());
    assert!(harness.instance_dir("bob").join("data/SOUL.md").is_file());
    assert!(harness
        .instance_dir("bob")
        .join("data/healthcheck.py")
        .is_file());
    let config = read_yaml(&harness.instance_dir("bob").join("data/config.yaml"));
    assert_eq!(config["model"]["default"], "clone-model");
}

#[tokio::test]
async fn clone_inherits_only_the_shares_that_still_exist() {
    let harness = Harness::new();
    create_share(&harness, "findings").await;
    let mut body = instance_body("alice");
    body["shares"] = json!([{"name": "findings", "mode": "ro"}]);
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");

    // The share directory disappears behind the control plane's back.
    harness
        .state
        .db
        .delete_share("findings")
        .expect("drop share");
    std::fs::remove_dir_all(harness.root().join("shares/findings")).expect("remove share dir");

    let (status, detail) = clone_instance(
        &harness,
        "alice",
        json!({"new_id": "bob", "channel_token": "999:BOB"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{detail}");
    assert_eq!(
        detail["shares"],
        json!([]),
        "a deleted share is not inherited"
    );
    let compose = read_yaml(&harness.instance_dir("bob").join("compose.yaml"));
    assert!(!compose["services"]["agent"]["volumes"]
        .as_array()
        .expect("volumes")
        .iter()
        .any(|volume| volume.as_str().is_some_and(|text| text.contains("shares/"))));
}

#[tokio::test]
async fn stopping_and_deleting_the_clone_leaves_the_source_alone() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    std::fs::write(
        harness.instance_dir("alice").join("data/MEMORY.md"),
        "remember alice",
    )
    .expect("memory file");

    let (status, detail) = clone_instance(
        &harness,
        "alice",
        json!({"new_id": "bob", "channel_token": "999:BOB"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{detail}");

    let (status, body) = harness
        .request(Method::POST, "/api/instances/bob/stop", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        parse(&body)["status"],
        "running",
        "the source keeps running"
    );
    assert!(harness.instance_dir("alice").join("compose.yaml").is_file());

    let (status, body) = harness
        .request(Method::DELETE, "/api/instances/bob?purge=true", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!harness.instance_dir("bob").exists());
    assert!(harness.instance_dir("alice").join("compose.yaml").is_file());
    assert!(harness
        .instance_dir("alice")
        .join("data/healthcheck.py")
        .is_file());
    assert_eq!(
        std::fs::read_to_string(harness.instance_dir("alice").join("data/MEMORY.md"))
            .expect("memory file kept"),
        "remember alice"
    );
    assert!(harness.state.db.get("alice").expect("get").is_some());
    assert_eq!(
        harness
            .instance_secrets("alice")
            .get("TELEGRAM_BOT_TOKEN")
            .map(String::as_str),
        Some("123:ABC"),
        "the source token is untouched"
    );

    let calls = harness.mock.calls();
    let downs: Vec<&str> = calls
        .iter()
        .filter(|call| call.operation == "down")
        .map(|call| call.instance.as_str())
        .collect();
    assert_eq!(downs, ["bob"], "only the clone was brought down");
}

// ---------------------------------------------------------------------------
// Instance files
// ---------------------------------------------------------------------------

fn data_dir(harness: &Harness, id: &str) -> PathBuf {
    harness.instance_dir(id).join("data")
}

async fn fetch_file(harness: &Harness, id: &str, path: &str) -> (StatusCode, Value) {
    let uri = format!("/api/instances/{id}/files/content?path={path}");
    let (status, body) = harness.request(Method::GET, &uri, None).await;
    (status, parse(&body))
}

async fn put_file(
    harness: &Harness,
    id: &str,
    path: &str,
    content: &str,
    etag: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = json!({ "path": path, "content": content });
    if let Some(etag) = etag {
        body["etag"] = json!(etag);
    }
    let (status, response) = harness
        .request(
            Method::PUT,
            &format!("/api/instances/{id}/files/content"),
            Some(body),
        )
        .await;
    (status, parse(&response))
}

async fn put_file_if_match(
    harness: &Harness,
    id: &str,
    path: &str,
    content: &str,
    if_match: &str,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(Method::PUT)
        .uri(format!("/api/instances/{id}/files/content"))
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::IF_MATCH, format!("\"{if_match}\""))
        .body(Body::from(
            json!({ "path": path, "content": content }).to_string(),
        ))
        .expect("valid request");
    let response = api::router(harness.state.clone())
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
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn files_listing_shows_one_level_of_the_data_directory() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/files?path=/", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    let entries = entries.as_array().expect("array of entries");
    let names: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    for expected in ["SOUL.md", "config.yaml", "healthcheck.py"] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }

    let soul = entries
        .iter()
        .find(|entry| entry["name"] == "SOUL.md")
        .expect("SOUL.md entry");
    assert_eq!(soul["path"], "/SOUL.md");
    assert_eq!(soul["type"], "file");
    assert_eq!(soul["editable"], true);
    assert_eq!(soul["rendered"], false);
    assert!(soul["size"].as_u64().expect("size") > 0);
    assert!(soul["mtime"].as_str().expect("mtime").ends_with('Z'));

    let config = entries
        .iter()
        .find(|entry| entry["name"] == "config.yaml")
        .expect("config.yaml entry");
    assert_eq!(config["editable"], false);
    assert_eq!(config["rendered"], true);

    // `?path` defaults to the data root.
    let (_, default_body) = harness
        .request(Method::GET, "/api/instances/alice/files", None)
        .await;
    assert_eq!(parse(&default_body), parse(&body));

    // One level only, and a directory gets its own listing.
    let (status, response) = harness
        .request(
            Method::POST,
            "/api/instances/alice/files/mkdir",
            Some(json!({"path": "/skills"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
    std::fs::write(data_dir(&harness, "alice").join("skills/NOTES.md"), "notes")
        .expect("notes file");

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/files?path=/skills", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    assert_eq!(entries.as_array().expect("array").len(), 1);
    assert_eq!(entries[0]["path"], "/skills/NOTES.md");
    assert_eq!(entries[0]["editable"], true);

    // The root listing never recurses into the subdirectory.
    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice/files?path=/", None)
        .await;
    assert!(!parse(&body)
        .as_array()
        .expect("array")
        .iter()
        .any(|entry| entry["name"] == "NOTES.md"));

    let (status, body) = harness
        .request(
            Method::GET,
            "/api/instances/alice/files?path=/SOUL.md",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "not_a_directory");

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/files?path=/ghost", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(parse(&body)["error"]["code"], "not_found");

    // A file is not a directory in the middle of a path either.
    let (status, body) = harness
        .request(
            Method::GET,
            "/api/instances/alice/files?path=/SOUL.md/skills",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "not_a_directory");

    let (status, body) = harness
        .request(Method::GET, "/api/instances/ghost/files", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(parse(&body)["error"]["code"], "not_found");
}

#[tokio::test]
async fn file_content_carries_the_etag_of_the_bytes_on_disk() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let on_disk =
        std::fs::read_to_string(data_dir(&harness, "alice").join("SOUL.md")).expect("soul");

    let (status, body) = fetch_file(&harness, "alice", "/SOUL.md").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path"], "/SOUL.md");
    assert_eq!(body["content"], on_disk);
    assert_eq!(body["size"], on_disk.len());
    assert_eq!(body["editable"], true);
    assert_eq!(body["rendered"], false);
    assert!(body.get("note").is_none());
    let etag = body["etag"].as_str().expect("etag").to_string();
    assert_eq!(etag.len(), 64);
    assert!(etag.chars().all(|c| c.is_ascii_hexdigit()));

    // Reading twice yields the same etag; editing the file changes it.
    let (_, again) = fetch_file(&harness, "alice", "/SOUL.md").await;
    assert_eq!(again["etag"], etag);
    std::fs::write(
        data_dir(&harness, "alice").join("SOUL.md"),
        format!("{on_disk}\nextra"),
    )
    .expect("edit soul");
    let (_, changed) = fetch_file(&harness, "alice", "/SOUL.md").await;
    assert_ne!(changed["etag"], etag);

    let (status, body) = fetch_file(&harness, "alice", "/config.yaml").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["editable"], false);
    assert_eq!(body["rendered"], true);
    assert!(body["note"]
        .as_str()
        .expect("rendered note")
        .contains("rendered by fleett"));

    let (status, body) = fetch_file(&harness, "alice", "/").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "not_a_file");
}

#[tokio::test]
async fn file_write_is_a_compare_and_swap() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let path = data_dir(&harness, "alice").join("SOUL.md");
    let original = std::fs::read_to_string(&path).expect("soul");
    let (_, body) = fetch_file(&harness, "alice", "/SOUL.md").await;
    let etag = body["etag"].as_str().expect("etag").to_string();

    // A stale etag is refused and nothing changes on disk.
    let (status, body) =
        put_file(&harness, "alice", "/SOUL.md", "hijacked", Some("deadbeef")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "etag_mismatch");
    assert_eq!(body["error"]["details"]["current"], etag);
    assert_eq!(std::fs::read_to_string(&path).expect("soul"), original);

    // An edit without any etag never overwrites an existing file.
    let (status, body) = put_file(&harness, "alice", "/SOUL.md", "hijacked", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "etag_mismatch");
    assert_eq!(std::fs::read_to_string(&path).expect("soul"), original);

    // The version the client loaded is accepted.
    let (status, body) = put_file(&harness, "alice", "/SOUL.md", "new soul", Some(&etag)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path"], "/SOUL.md");
    assert_eq!(body["editable"], true);
    assert_eq!(std::fs::read_to_string(&path).expect("soul"), "new soul");
    let fresh = body["etag"].as_str().expect("new etag").to_string();
    assert_ne!(fresh, etag);
    let (_, reread) = fetch_file(&harness, "alice", "/SOUL.md").await;
    assert_eq!(reread["etag"], fresh);

    // The old etag is now stale.
    let (status, body) = put_file(&harness, "alice", "/SOUL.md", "again", Some(&etag)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["details"]["current"], fresh);

    // If-Match is honoured (quoted, the way a browser sends it back).
    let (status, body) =
        put_file_if_match(&harness, "alice", "/SOUL.md", "via header", &fresh).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(std::fs::read_to_string(&path).expect("soul"), "via header");

    // A file that does not exist yet is created with an empty etag.
    let (status, body) = put_file(&harness, "alice", "/NEW.md", "created", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        std::fs::read_to_string(data_dir(&harness, "alice").join("NEW.md")).expect("new file"),
        "created"
    );
}

#[tokio::test]
async fn file_write_keeps_a_timestamped_backup() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let path = data_dir(&harness, "alice").join("SOUL.md");
    let original = std::fs::read_to_string(&path).expect("soul");
    let (_, body) = fetch_file(&harness, "alice", "/SOUL.md").await;
    let etag = body["etag"].as_str().expect("etag").to_string();

    let (status, body) = put_file(&harness, "alice", "/SOUL.md", "rewritten", Some(&etag)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let backup = body["backup"].as_str().expect("backup path");
    assert!(
        backup.starts_with("/SOUL.md.bak-"),
        "unexpected backup path {backup}"
    );
    let stamp = backup.trim_start_matches("/SOUL.md.bak-");
    assert_eq!(stamp.len(), 15, "YYYYMMDD-HHMMSS expected, got {stamp}");
    assert_eq!(
        std::fs::read_to_string(data_dir(&harness, "alice").join(&backup[1..]))
            .expect("backup readable"),
        original
    );

    // A creation has nothing to back up.
    let (status, body) = put_file(&harness, "alice", "/FRESH.md", "new", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("backup").is_none());
}

#[tokio::test]
async fn file_traversal_is_refused_and_writes_nothing() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let database = harness.root().join("fleet.db");
    let before = std::fs::read(&database).expect("database readable");

    for path in [
        "../../fleet.db",
        "/../fleet.db",
        "/skills/../../fleet.db",
        "/..",
    ] {
        let (status, body) = fetch_file(&harness, "alice", path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert_eq!(body["error"]["code"], "invalid_path", "{path}");

        let (status, body) = put_file(&harness, "alice", path, "owned", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert_eq!(body["error"]["code"], "invalid_path", "{path}");
    }

    // The database is untouched and no backup landed next to it.
    assert_eq!(std::fs::read(&database).expect("database readable"), before);
    let stray: Vec<String> = std::fs::read_dir(harness.root())
        .expect("root")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".bak-"))
        .collect();
    assert!(stray.is_empty(), "stray backups: {stray:?}");
}

#[tokio::test]
async fn file_symlinks_cannot_escape_the_data_directory() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let data = data_dir(&harness, "alice");
    let outside = tempfile::tempdir().expect("outside dir");
    std::fs::write(outside.path().join("secret.txt"), "top secret").expect("outside file");

    std::os::unix::fs::symlink(outside.path().join("secret.txt"), data.join("leak.txt"))
        .expect("file symlink");
    let (status, body) = fetch_file(&harness, "alice", "/leak.txt").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "path_escape");
    assert!(body["error"]["message"]
        .as_str()
        .expect("message")
        .contains("outside"));

    // A symlinked directory cannot receive a write either.
    std::os::unix::fs::symlink(outside.path(), data.join("drop")).expect("dir symlink");
    let (status, body) = put_file(&harness, "alice", "/drop/pwned.md", "owned", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "path_escape");
    assert!(!outside.path().join("pwned.md").exists());

    // Listing never advertises an escaping entry.
    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice/files?path=/", None)
        .await;
    assert!(!parse(&body)
        .as_array()
        .expect("array")
        .iter()
        .any(|entry| entry["name"] == "leak.txt" || entry["name"] == "drop"));

    // A link that stays inside the data directory is followed.
    std::os::unix::fs::symlink(data.join("SOUL.md"), data.join("soul-link.md"))
        .expect("inside symlink");
    let (status, body) = fetch_file(&harness, "alice", "/soul-link.md").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body["content"].as_str().expect("content").is_empty());
}

#[tokio::test]
async fn file_special_files_are_never_served() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let data = data_dir(&harness, "alice");

    // A socket is not a file: it cannot be read, deleted or advertised.
    let _listener =
        std::os::unix::net::UnixListener::bind(data.join("gateway.sock")).expect("socket");
    let (status, body) = fetch_file(&harness, "alice", "/gateway.sock").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "unsupported_file_type");

    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice/files?path=/", None)
        .await;
    assert!(!parse(&body)
        .as_array()
        .expect("array")
        .iter()
        .any(|entry| entry["name"] == "gateway.sock"));

    let (status, body) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/gateway.sock",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "unsupported_file_type");
    assert!(data.join("gateway.sock").exists());
}

#[tokio::test]
async fn file_content_is_limited_to_text_under_the_size_cap() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let data = data_dir(&harness, "alice");

    // Exactly 512 KiB is accepted…
    let at_cap = "a".repeat(512 * 1024);
    let (status, body) = put_file(&harness, "alice", "/at-cap.txt", &at_cap, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["size"], 512 * 1024);

    // …one byte more is refused, and nothing is written.
    let above = "a".repeat(512 * 1024 + 1);
    let (status, body) = put_file(&harness, "alice", "/above.txt", &above, None).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["error"]["code"], "file_too_large");
    assert_eq!(body["error"]["details"]["limit"], 512 * 1024);
    assert!(!data.join("above.txt").exists());

    // An oversized file already on disk is listed as read-only and refused.
    std::fs::write(data.join("huge.txt"), "a".repeat(512 * 1024 + 1)).expect("huge file");
    let (_, body) = harness
        .request(Method::GET, "/api/instances/alice/files?path=/", None)
        .await;
    let huge = parse(&body)
        .as_array()
        .expect("array")
        .iter()
        .find(|entry| entry["name"] == "huge.txt")
        .expect("huge entry")
        .clone();
    assert_eq!(huge["editable"], false);
    let (status, body) = fetch_file(&harness, "alice", "/huge.txt").await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["error"]["code"], "file_too_large");

    // Binary files are refused, never returned as mojibake.
    std::fs::write(data.join("blob.bin"), [0u8, 159, 146, 150]).expect("binary file");
    let (status, body) = fetch_file(&harness, "alice", "/blob.bin").await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    assert_eq!(body["error"]["code"], "binary_file");

    std::fs::write(data.join("latin.txt"), [0xE9, 0x74, 0xE9]).expect("invalid utf8");
    let (status, body) = fetch_file(&harness, "alice", "/latin.txt").await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(body["error"]["code"], "binary_file");

    std::fs::write(data.join("nul.txt"), b"hello\0world").expect("nul file");
    let (status, body) = fetch_file(&harness, "alice", "/nul.txt").await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(body["error"]["code"], "binary_file");
}

#[tokio::test]
async fn file_editing_is_limited_to_the_extension_allowlist() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let data = data_dir(&harness, "alice");

    std::fs::write(data.join("notes.png"), "plain text in disguise").expect("png");
    let (status, body) = fetch_file(&harness, "alice", "/notes.png").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["editable"], false);
    assert_eq!(body["content"], "plain text in disguise");

    let (status, body) = put_file(&harness, "alice", "/notes.png", "changed", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "file_not_editable");
    assert_eq!(
        std::fs::read_to_string(data.join("notes.png")).expect("png"),
        "plain text in disguise"
    );

    // A new file outside the allowlist is refused too.
    let (status, body) = put_file(&harness, "alice", "/settings.ini", "x=1", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "file_not_editable");
    assert!(!data.join("settings.ini").exists());

    // Every allowlisted extension is accepted.
    for (name, ext) in [
        ("a.md", "md"),
        ("a.yaml", "yaml"),
        ("a.yml", "yml"),
        ("a.json", "json"),
        ("a.txt", "txt"),
        ("a.toml", "toml"),
    ] {
        let (status, body) = put_file(&harness, "alice", &format!("/{name}"), "x", None).await;
        assert_eq!(status, StatusCode::OK, "{ext}: {body}");
        assert_eq!(body["editable"], true);
    }
}

#[tokio::test]
async fn rendered_files_are_read_only_and_explained() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    let (status, body) = fetch_file(&harness, "alice", "/config.yaml").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["editable"], false);
    assert_eq!(body["rendered"], true);
    let etag = body["etag"].as_str().expect("etag").to_string();

    for path in ["/config.yaml", "/healthcheck.py"] {
        let (status, body) = put_file(&harness, "alice", path, "manual edit", Some(&etag)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}: {body}");
        assert_eq!(body["error"]["code"], "file_regenerated", "{path}");
        assert_eq!(body["error"]["details"]["rendered"], true);
        assert!(body["error"]["message"]
            .as_str()
            .expect("message")
            .contains("rendered by fleett"));
    }

    let (status, body) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/config.yaml",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(parse(&body)["error"]["code"], "file_regenerated");

    // A symlinked directory cannot be used as a side door.
    std::os::unix::fs::symlink(
        data_dir(&harness, "alice"),
        data_dir(&harness, "alice").join("alias"),
    )
    .expect("alias symlink");
    let (status, body) = put_file(
        &harness,
        "alice",
        "/alias/config.yaml",
        "sneaky",
        Some(&etag),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "file_regenerated");
    let (status, body) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/alias/config.yaml",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(parse(&body)["error"]["code"], "file_regenerated");
    assert!(data_dir(&harness, "alice").join("config.yaml").is_file());

    // The next PATCH still regenerates the file untouched.
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"notes": "patched"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = fetch_file(&harness, "alice", "/config.yaml").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn mkdir_and_delete_cover_files_and_empty_directories() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let data = data_dir(&harness, "alice");

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/files/mkdir",
            Some(json!({"path": "/profiles"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(parse(&body)["path"], "/profiles");
    assert_eq!(parse(&body)["type"], "dir");
    assert!(data.join("profiles").is_dir());

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/files/mkdir",
            Some(json!({"path": "/profiles"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(parse(&body)["error"]["code"], "already_exists");

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/files/mkdir",
            Some(json!({"path": "/missing/deep"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(parse(&body)["error"]["code"], "not_found");

    // An empty directory may be deleted.
    let (status, _) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/profiles",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!data.join("profiles").exists());

    let (status, body) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/profiles",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(parse(&body)["error"]["code"], "not_found");

    // A non-empty directory is refused.
    let (status, _) = harness
        .request(
            Method::POST,
            "/api/instances/alice/files/mkdir",
            Some(json!({"path": "/work"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = put_file(&harness, "alice", "/work/brief.md", "brief", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/work",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(parse(&body)["error"]["code"], "directory_not_empty");

    let (status, _) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/work/brief.md",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!data.join("work/brief.md").exists());
    let (status, _) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/work",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The data directory itself is never deletable.
    let (status, body) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(parse(&body)["error"]["code"], "not_a_file");
}

#[tokio::test]
async fn file_write_can_restart_the_instance() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let (_, body) = fetch_file(&harness, "alice", "/SOUL.md").await;
    let etag = body["etag"].as_str().expect("etag").to_string();
    harness.mock.clear_calls();

    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/files/content?restart=true",
            Some(json!({"path": "/SOUL.md", "content": "boot me", "etag": etag})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response = parse(&body);
    assert_eq!(response["restarted"], true);
    assert_eq!(response["status"], "running");
    let calls = harness.mock.calls();
    let restarts: Vec<&str> = calls
        .iter()
        .filter(|call| call.operation == "restart")
        .map(|call| call.instance.as_str())
        .collect();
    assert_eq!(restarts, ["alice"]);

    // Without the flag, saving never touches the container.
    let etag = response["etag"].as_str().expect("etag").to_string();
    harness.mock.clear_calls();
    let (status, body) = put_file(&harness, "alice", "/SOUL.md", "no restart", Some(&etag)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restarted"], false);
    assert!(body.get("status").is_none());
    assert!(harness.mock.calls().is_empty(), "no driver call expected");
}

#[tokio::test]
async fn viewers_cannot_write_files() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/tokens",
            Some(json!({"name": "read-only", "role": "viewer"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = parse(&body)["token"].as_str().expect("token").to_string();

    let (status, _, body) = harness
        .send(
            Method::GET,
            "/api/instances/alice/files?path=/",
            None,
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = harness
        .send(
            Method::GET,
            "/api/instances/alice/files/content?path=/SOUL.md",
            None,
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _, body) = harness
        .send(
            Method::PUT,
            "/api/instances/alice/files/content",
            Some(json!({"path": "/SOUL.md", "content": "nope"})),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(parse(&body)["error"]["code"], "forbidden");

    let (status, _, _) = harness
        .send(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/SOUL.md",
            None,
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = harness
        .send(
            Method::POST,
            "/api/instances/alice/files/mkdir",
            Some(json!({"path": "/nope"})),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!data_dir(&harness, "alice").join("nope").exists());
}

#[tokio::test]
async fn file_mutations_are_audited() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let (_, body) = fetch_file(&harness, "alice", "/SOUL.md").await;
    let etag = body["etag"].as_str().expect("etag").to_string();

    let (status, body) = put_file(&harness, "alice", "/SOUL.md", "audited soul", Some(&etag)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = harness
        .request(
            Method::POST,
            "/api/instances/alice/files/mkdir",
            Some(json!({"path": "/notes"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/notes",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = harness
        .request(Method::GET, "/api/audit?limit=20", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    let entries = entries.as_array().expect("audit array");
    let find = |action: &str| {
        entries
            .iter()
            .find(|entry| entry["action"] == action)
            .unwrap_or_else(|| panic!("no {action} audit entry: {entries:?}"))
    };

    let write = find("instance.file.write");
    assert_eq!(write["target"], "alice");
    assert_eq!(write["details"]["path"], "/SOUL.md");
    assert_eq!(write["details"]["size"], "audited soul".len());
    assert_eq!(write["details"]["restart"], false);
    assert_eq!(write["actor"], "bootstrap");

    let mkdir = find("instance.file.mkdir");
    assert_eq!(mkdir["details"]["path"], "/notes");
    let delete = find("instance.file.delete");
    assert_eq!(delete["details"]["path"], "/notes");
}

// ---------------------------------------------------------------------------
// Agent-to-agent bus (#28)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn messages_are_queued_delivered_and_acknowledged() {
    let harness = Harness::new();
    create_instance(&harness, "probe").await;
    create_instance(&harness, "tchouniz").await;

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/probe/messages",
            Some(json!({
                "to": "tchouniz",
                "kind": "ask",
                "body": "what is the weather?",
                "correlation_id": "conv-1",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let sent = parse(&body);
    assert_eq!(sent["from"], "probe");
    assert_eq!(sent["to"], "tchouniz");
    assert_eq!(sent["status"], "queued");
    assert_eq!(sent["hops"], 0);
    let message_id = sent["id"].as_str().expect("id").to_string();

    let (status, body) = harness
        .request(Method::GET, "/api/instances/tchouniz/messages", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let inbox = parse(&body);
    assert_eq!(inbox.as_array().expect("array").len(), 1);
    assert_eq!(inbox[0]["status"], "delivered");
    assert!(inbox[0]["delivered_at"].is_string());

    let (status, body) = harness
        .request(
            Method::POST,
            &format!("/api/instances/tchouniz/messages/{message_id}/ack"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["message"]["status"], "read");

    let (status, body) = harness
        .request(Method::GET, "/api/messages?kind=ask", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse(&body).as_array().expect("array").len(), 1);
}

#[tokio::test]
async fn a_message_loop_is_stopped_by_hops() {
    let harness = Harness::new();
    create_instance(&harness, "aa").await;
    create_instance(&harness, "bb").await;
    for _ in 0..fleett::bus::DEFAULT_MAX_HOPS {
        let (status, body) = harness
            .request(
                Method::POST,
                "/api/instances/aa/messages",
                Some(json!({"to": "bb", "body": "ping", "correlation_id": "loop"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/aa/messages",
            Some(json!({"to": "bb", "body": "ping", "correlation_id": "loop"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "message_loop");
}

// ---------------------------------------------------------------------------
// Custom templates (issue #43)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn custom_templates_live_on_disk_and_inherit_their_parent() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/templates",
            Some(json!({
                "id": "house",
                "name": "House",
                "description": "Our house template",
                "parent": "guest",
                "env": [{
                    "key": "HOUSE_FLAG",
                    "description": "House switch",
                    "example": "1",
                    "format": "boolean",
                    "required": false,
                    "secret": false
                }],
                "defaults": {
                    "mem_limit_mb": 2048,
                    "cpu_limit": 2.0,
                    "notes": "house policy",
                    "allow_lan": false
                }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["id"], "house");
    assert_eq!(detail["parent"], "guest");
    assert_eq!(detail["read_only"], false);
    assert_eq!(detail["defaults"]["mem_limit_mb"], 2048);
    assert_eq!(detail["defaults"]["cpu_limit"], 2.0);
    assert_eq!(detail["versions_count"], 1);
    let env = detail["env"].as_array().expect("env");
    assert!(env
        .iter()
        .any(|entry| entry["key"] == "HOUSE_FLAG" && entry["required"] == false));
    // The inherited guest entries are part of the resolved schema.
    assert!(env
        .iter()
        .any(|entry| entry["key"] == "TELEGRAM_ALLOWED_USERS"));

    let dir = harness.root().join("templates/house");
    let meta = read_yaml(&dir.join("template.yaml"));
    assert_eq!(meta["parent"], "guest");
    assert_eq!(meta["name"], "House");
    assert!(
        !dir.join("config.yaml").exists(),
        "a child keeps inheriting the parent config instead of copying it"
    );
    assert!(!dir.join("soul.md").exists());
    let defaults = read_yaml(&dir.join("defaults.yaml"));
    assert_eq!(defaults["mem_limit_mb"], 2048);

    let (status, body) = harness.request(Method::GET, "/api/templates", None).await;
    assert_eq!(status, StatusCode::OK);
    let templates = parse(&body);
    let house = templates
        .as_array()
        .expect("array")
        .iter()
        .find(|template| template["id"] == "house")
        .expect("house template");
    assert_eq!(house["parent"], "guest");
    assert_eq!(house["instances_count"], 0);

    let (status, body) = harness
        .request(Method::GET, "/api/templates/house", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = parse(&body);
    assert!(detail["config_yaml"]
        .as_str()
        .expect("config")
        .contains("fleett built-in template: guest"));
    assert!(detail["soul_md"]
        .as_str()
        .expect("soul")
        .contains("{{AGENT_NAME}}"));
}

#[tokio::test]
async fn template_defaults_and_required_env_drive_instance_creation() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/templates",
            Some(json!({
                "id": "house",
                "parent": "guest",
                "env": [{
                    "key": "HOUSE_FLAG",
                    "description": "Required switch",
                    "required": true,
                    "secret": true
                }],
                "defaults": {
                    "mem_limit_mb": 2048,
                    "cpu_limit": 2.0,
                    "notes": "house policy"
                }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // A missing required variable is a 400 carrying the key and the field.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "villa",
                "template": "house",
                "model": MODEL,
                "base_url": BASE_URL,
                "gate_token": "gate-secret"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let error = parse(&body);
    assert_eq!(error["error"]["code"], "missing_required_env");
    let details = error["error"]["details"].as_array().expect("details");
    assert_eq!(details[0]["key"], "HOUSE_FLAG");
    assert_eq!(details[0]["field"], "env");

    // Supplied without a `secret` flag: the template schema decides (secret).
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "villa",
                "template": "house",
                "model": MODEL,
                "base_url": BASE_URL,
                "gate_token": "gate-secret",
                "env": [{"key": "HOUSE_FLAG", "value": "on"}]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["mem_limit_mb"], 2048);
    assert_eq!(detail["cpu_limit"], 2.0);
    assert_eq!(detail["notes"], "house policy");
    // The template metadata supplied the platform; no channel was requested.
    assert_eq!(detail["channel"], "telegram");
    let flag = detail["env"]
        .as_array()
        .expect("env")
        .iter()
        .find(|entry| entry["key"] == "HOUSE_FLAG")
        .expect("flag");
    assert_eq!(flag["secret"], true);
    assert!(flag.get("value").is_none(), "secret values never come back");
    assert_eq!(
        harness
            .instance_secrets("villa")
            .get("HOUSE_FLAG")
            .map(String::as_str),
        Some("on")
    );

    let config = read_yaml(&harness.instance_dir("villa").join("data/config.yaml"));
    assert_eq!(
        config["platforms"]["telegram"]["enabled"].as_bool(),
        Some(true)
    );
}

#[tokio::test]
async fn template_inheritance_is_live_and_cycles_are_refused() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/templates",
            Some(json!({
                "id": "base",
                "name": "Base",
                "channel": "none",
                "env": [
                    {"key": "SHARED", "description": "base", "required": false, "secret": false},
                    {"key": "BASE_ONLY", "description": "base only", "required": false, "secret": false}
                ]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/templates",
            Some(json!({
                "id": "child",
                "name": "Child",
                "parent": "base",
                "env": [{"key": "SHARED", "description": "child", "required": true, "secret": false}]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = harness
        .request(Method::GET, "/api/templates/child", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = parse(&body);
    let env = detail["env"].as_array().expect("env");
    let keys: Vec<&str> = env
        .iter()
        .filter_map(|entry| entry["key"].as_str())
        .collect();
    assert_eq!(keys, ["SHARED", "BASE_ONLY"]);
    assert_eq!(env[0]["description"], "child");
    assert_eq!(env[0]["required"], true);

    // base -> child -> base would be a cycle.
    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/templates/base",
            Some(json!({"parent": "child"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "template_cycle");

    // A self-parent is refused too.
    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/templates/base",
            Some(json!({"parent": "base"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "template_cycle");
}

#[tokio::test]
async fn deleting_a_template_in_use_is_a_409_and_builtins_are_read_only() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/templates",
            Some(json!({"id": "house", "parent": "solo", "defaults": {"mem_limit_mb": 512}})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "villa",
                "template": "house",
                "model": MODEL,
                "base_url": BASE_URL,
                "gate_token": "gate-secret"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(parse(&body)["mem_limit_mb"], 512);

    let (status, body) = harness
        .request(Method::DELETE, "/api/templates/house", None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = parse(&body);
    assert_eq!(error["error"]["code"], "template_in_use");
    assert_eq!(error["error"]["details"]["instances"], json!(["villa"]));

    let (status, body) = harness
        .request(Method::DELETE, "/api/templates/guest", None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "read_only");

    let (status, body) = harness
        .request(Method::DELETE, "/api/instances/villa", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body) = harness
        .request(Method::DELETE, "/api/templates/house", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!harness.root().join("templates/house").exists());
    let (status, _) = harness
        .request(Method::GET, "/api/templates/house", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn from_instance_copies_the_config_the_soul_and_the_limits() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "alice",
                "name": "Alice",
                "template": "solo",
                "model": MODEL,
                "base_url": BASE_URL,
                "channel": {"type": "none"},
                "mem_limit_mb": 1024,
                "cpu_limit": 1.5,
                "notes": "source instance",
                "gate_token": "gate-secret",
                "env": [{"key": "EXTRA_FLAG", "value": "1", "secret": false}]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    std::fs::write(
        harness.instance_dir("alice").join("data/SOUL.md"),
        "# Alice\n\nYou are the source agent.\n",
    )
    .expect("write soul");

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/templates/from-instance",
            Some(json!({"instance_id": "alice", "id": "alice-copy", "name": "Alice copy"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["id"], "alice-copy");
    assert_eq!(detail["name"], "Alice copy");
    assert_eq!(detail["source_instance"], "alice");
    assert_eq!(detail["defaults"]["mem_limit_mb"], 1024);
    assert_eq!(detail["defaults"]["cpu_limit"], 1.5);
    assert_eq!(detail["defaults"]["notes"], "source instance");
    assert_eq!(detail["parent"], Value::Null);
    let env = detail["env"].as_array().expect("env");
    assert!(env
        .iter()
        .any(|entry| entry["key"] == "EXTRA_FLAG" && entry["secret"] == false));
    assert!(env
        .iter()
        .any(|entry| entry["key"] == "HERMES_GATE_PROFILE_TOKEN" && entry["secret"] == true));

    let template_dir = harness.root().join("templates/alice-copy");
    assert_eq!(
        std::fs::read_to_string(template_dir.join("config.yaml")).expect("config"),
        std::fs::read_to_string(harness.instance_dir("alice").join("data/config.yaml"))
            .expect("source config")
    );
    assert_eq!(
        std::fs::read_to_string(template_dir.join("soul.md")).expect("soul"),
        "# Alice\n\nYou are the source agent.\n"
    );

    // The saved template recreates the same limits and starting SOUL.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "bob",
                "template": "alice-copy",
                "gate_token": "gate-secret"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["mem_limit_mb"], 1024);
    assert_eq!(detail["cpu_limit"], 1.5);
    assert_eq!(
        std::fs::read_to_string(harness.instance_dir("bob").join("data/SOUL.md"))
            .expect("bob soul"),
        "# Alice\n\nYou are the source agent.\n"
    );
}

#[tokio::test]
async fn template_versions_are_listed_and_restorable() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/templates",
            Some(json!({
                "id": "house",
                "name": "House",
                "channel": "none",
                "soul_md": "# House one\n"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/templates/house",
            Some(json!({"name": "Renamed", "soul_md": "# House two\n"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["name"], "Renamed");

    let (status, body) = harness
        .request(Method::GET, "/api/templates/house/versions", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let versions = parse(&body);
    let versions = versions.as_array().expect("versions");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["action"], "create");
    assert_eq!(versions[1]["action"], "update");
    assert_eq!(
        versions[0]["sha256"]["soul.md"].as_str().map(str::len),
        Some(64)
    );

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/templates/house/restore",
            Some(json!({"version": 1})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["name"], "House");
    assert_eq!(detail["soul_md"], "# House one\n");
    assert_eq!(detail["versions_count"], 3);
    assert_eq!(
        std::fs::read_to_string(harness.root().join("templates/house/soul.md")).expect("soul"),
        "# House one\n"
    );

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/templates/house/restore",
            Some(json!({"version": 99})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
