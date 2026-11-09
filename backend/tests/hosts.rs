//! Multi-host API tests (issue #40).
//!
//! Every test runs with `MockDriver`: no SSH, no Docker. The mock records the
//! host of each lifecycle call and answers `probe_host` with canned capacity,
//! which is exactly the contract the compose driver implements with `docker
//! info` over `DOCKER_HOST`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::hosts::HostCapacity;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::Path;
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
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.secrets_key = Some(fleett::secrets::encode_key(&[7u8; 32]));
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap");
        Self { dir, state, mock }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    async fn request(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (status, text) = self.send(method, uri, body).await;
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, value)
    }

    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, String) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {TOKEN}"));
        let body = match body {
            Some(value) => {
                request = request.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = api::router(self.state.clone())
            .oneshot(request.body(body).expect("request"))
            .await
            .expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn add_host(&self, id: &str, transport: &str, address: &str) -> Value {
        let now = fleett::models::now_iso();
        let spec = fleett::hosts::HostSpec {
            id: id.to_string(),
            name: id.to_string(),
            transport: transport.to_string(),
            address: address.to_string(),
            docker_context: None,
            status: fleett::hosts::STATUS_UNKNOWN.to_string(),
            last_check_at: None,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        };
        self.state.db.insert_host(&spec).expect("insert host");
        serde_json::to_value(spec).expect("host json")
    }

    fn instance_body(&self, id: &str, host: &str) -> Value {
        json!({
            "id": id,
            "model": MODEL,
            "base_url": BASE_URL,
            "gate_token": "gate-token",
            "host": host,
        })
    }

    fn write_data(&self, id: &str, marker: &str) {
        let data = self.root().join("instances").join(id).join("data");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::write(data.join("MEMORY.md"), marker).expect("marker");
    }
}

#[tokio::test]
async fn the_local_host_is_always_registered() {
    let harness = Harness::new();
    let (status, body) = harness.request("GET", "/api/hosts", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let hosts = body.as_array().expect("array");
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0]["id"], "local");
    assert_eq!(hosts[0]["status"], "ok");
    assert_eq!(hosts[0]["instances"], 0);
    assert_eq!(hosts[0]["capacity"]["ncpu"], 8);
    assert_eq!(
        hosts[0]["capacity"]["mem_total_bytes"],
        16u64 * 1024 * 1024 * 1024
    );
}

#[tokio::test]
async fn host_crud_validates_and_protects_the_local_entry() {
    let harness = Harness::new();

    let (status, body) = harness
        .request(
            "POST",
            "/api/hosts",
            Some(json!({
                "id": "pve3",
                "name": "PVE 3",
                "transport": "ssh",
                "address": "cle@10.1.20.5",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["transport"], "ssh");
    assert_eq!(body["status"], "ok");

    // Duplicate id, duplicate name and invalid values are refused.
    let (status, _) = harness
        .request(
            "POST",
            "/api/hosts",
            Some(json!({"id": "pve3", "name": "other"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) = harness
        .request(
            "POST",
            "/api/hosts",
            Some(json!({"id": "bad", "name": "x", "transport": "ssh", "address": "cle@h;reboot"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_address");
    let (status, body) = harness
        .request(
            "POST",
            "/api/hosts",
            Some(json!({"id": "bad2", "name": "x", "transport": "local", "address": "host"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_address");

    // PATCH an address and test the daemon.
    let (status, body) = harness
        .request(
            "PATCH",
            "/api/hosts/pve3",
            Some(json!({"address": "ssh://cle@10.1.20.6"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["address"], "ssh://cle@10.1.20.6");

    let (status, body) = harness.request("POST", "/api/hosts/pve3/test", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["status"], "ok");
    assert!(body["latency_ms"].is_number());

    // The local host can be neither renamed nor deleted.
    let (status, body) = harness
        .request(
            "PATCH",
            "/api/hosts/local",
            Some(json!({"transport": "ssh"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = harness.request("DELETE", "/api/hosts/local", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "host_protected");

    // An unused host can be removed.
    let (status, _) = harness.request("DELETE", "/api/hosts/pve3", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = harness.request("GET", "/api/hosts/pve3", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_unreachable_host_is_reported_and_never_fails_the_list() {
    let harness = Harness::new();
    harness
        .mock
        .fail_host_test("pve3", "ssh: connect to host 10.1.20.5 port 22: refused");
    harness.add_host("pve3", "ssh", "cle@10.1.20.5");

    let (status, body) = harness.request("GET", "/api/hosts", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let hosts = body.as_array().expect("array");
    let pve3 = hosts
        .iter()
        .find(|host| host["id"] == "pve3")
        .expect("pve3");
    assert_eq!(pve3["status"], "unreachable");
    assert!(pve3["last_error"]
        .as_str()
        .expect("error")
        .contains("refused"));

    let (status, body) = harness.request("POST", "/api/hosts/pve3/test", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], false);
    assert_eq!(body["status"], "unreachable");
    assert!(body["error"].as_str().expect("error").contains("refused"));
}

#[tokio::test]
async fn auto_placement_picks_the_host_with_most_free_memory() {
    let harness = Harness::new();
    harness.mock.set_host_capacity(
        "local",
        HostCapacity {
            ncpu: 2,
            mem_total_bytes: 4 * 1024 * 1024 * 1024,
            docker_version: Some("mock".to_string()),
        },
    );
    harness.mock.set_host_capacity(
        "big",
        HostCapacity {
            ncpu: 16,
            mem_total_bytes: 64 * 1024 * 1024 * 1024,
            docker_version: Some("mock".to_string()),
        },
    );
    harness.add_host("big", "ssh", "cle@10.1.20.6");

    let (status, body) = harness
        .request(
            "POST",
            "/api/instances",
            Some(harness.instance_body("alice", "auto")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["host"], "big");
    assert_eq!(harness.mock.host_of("up", "alice").as_deref(), Some("big"));

    // An explicit host wins over the capacity ranking.
    let (status, body) = harness
        .request(
            "POST",
            "/api/instances",
            Some(harness.instance_body("bob", "local")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["host"], "local");

    // An unknown host is a 400 before anything is written.
    let (status, body) = harness
        .request(
            "POST",
            "/api/instances",
            Some(harness.instance_body("carol", "ghost")),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "unknown_host");
    assert!(!harness.root().join("instances/carol").exists());
}

#[tokio::test]
async fn lifecycle_calls_carry_the_instance_host() {
    let harness = Harness::new();
    harness.add_host("pve3", "ssh", "cle@10.1.20.5");
    let (status, body) = harness
        .request(
            "POST",
            "/api/instances",
            Some(harness.instance_body("alice", "pve3")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = harness.request("GET", "/api/instances/alice", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["host"], "pve3");
    let (status, body) = harness.request("GET", "/api/instances", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["host"], "pve3");

    for action in ["start", "stop", "restart"] {
        let (status, _) = harness
            .request("POST", &format!("/api/instances/alice/{action}"), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{action}");
        assert_eq!(
            harness.mock.host_of(action, "alice").as_deref(),
            Some("pve3"),
            "{action}"
        );
    }

    let (status, _) = harness
        .request("GET", "/api/instances/alice/logs", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        harness.mock.host_of("logs", "alice").as_deref(),
        Some("pve3")
    );
}

#[tokio::test]
async fn usage_aggregates_hosts_and_warns_about_the_unreachable_one() {
    let harness = Harness::new();
    harness.add_host("pve3", "ssh", "cle@10.1.20.5");
    for (id, host) in [("alice", "local"), ("bob", "pve3")] {
        let (status, body) = harness
            .request(
                "POST",
                "/api/instances",
                Some(harness.instance_body(id, host)),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    harness
        .mock
        .fail_usage_host("pve3", "Cannot connect to the Docker daemon");

    let (status, body) = harness.request("GET", "/api/usage", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = body["instances"].as_array().expect("instances");
    assert_eq!(entries.len(), 2, "the reachable host is still reported");
    let warnings = body["warnings"].as_array().expect("warnings");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].as_str().expect("warning").contains("pve3"));
}

#[tokio::test]
async fn moving_an_instance_keeps_its_data_and_routes_to_the_new_host() {
    let harness = Harness::new();
    harness.add_host("pve3", "ssh", "cle@10.1.20.5");
    let (status, body) = harness
        .request(
            "POST",
            "/api/instances",
            Some(harness.instance_body("alice", "local")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    harness.write_data("alice", "remember me");

    // The host is protected while an instance still lives on it.
    let (status, body) = harness.request("DELETE", "/api/hosts/local", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, body) = harness
        .request(
            "POST",
            "/api/instances/alice/move",
            Some(json!({"to": "pve3"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["host"], "pve3");
    assert_eq!(harness.mock.host_of("up", "alice").as_deref(), Some("pve3"));
    assert_eq!(
        std::fs::read_to_string(harness.root().join("instances/alice/data/MEMORY.md"))
            .expect("data"),
        "remember me"
    );

    // Moving to the same host and to an unknown host are refused.
    let (status, body) = harness
        .request(
            "POST",
            "/api/instances/alice/move",
            Some(json!({"to": "pve3"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "already_on_host");
    let (status, body) = harness
        .request(
            "POST",
            "/api/instances/alice/move",
            Some(json!({"to": "ghost"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "unknown_host");

    // Moving back works too, and the host becomes deletable once empty.
    let (status, body) = harness
        .request(
            "POST",
            "/api/instances/alice/move",
            Some(json!({"to": "local"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["host"], "local");
    let (status, _) = harness.request("DELETE", "/api/hosts/pve3", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_failed_move_rolls_back_to_the_source_host() {
    let harness = Harness::new();
    harness.add_host("pve3", "ssh", "cle@10.1.20.5");
    let (status, body) = harness
        .request(
            "POST",
            "/api/instances",
            Some(harness.instance_body("alice", "local")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    harness.mock.fail_next("up", "no such image on pve3");
    let (status, body) = harness
        .request(
            "POST",
            "/api/instances/alice/move",
            Some(json!({"to": "pve3"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"]["code"], "move_failed");
    assert!(body["error"]["details"]["from"].as_str() == Some("local"));

    let (status, body) = harness.request("GET", "/api/instances/alice", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["host"], "local", "the label was rolled back");
}
