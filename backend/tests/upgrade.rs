//! Integration tests for rolling image upgrades (issue #11).
//!
//! Every test runs against `MockDriver`; no test ever calls Docker. The mock
//! tracks the image of each simulated container and can declare a tag
//! unhealthy, which is exactly what a broken image looks like to the job.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::models::Health;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
/// Image the tests upgrade to when they just need a valid tag.
const TARGET: &str = "hermes-agent:0.23.0-local";

struct Harness {
    /// Kept alive for the whole test: it owns the data root.
    _dir: tempfile::TempDir,
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
        Self {
            _dir: dir,
            state,
            mock,
        }
    }

    fn compose(&self, id: &str) -> String {
        std::fs::read_to_string(self.state.cfg.compose_path(id)).expect("compose file")
    }

    fn compose_doc(&self, id: &str) -> Value {
        let doc: serde_yaml::Value =
            serde_yaml::from_str(&self.compose(id)).expect("compose YAML parses");
        serde_json::to_value(doc).expect("compose as JSON")
    }

    fn image_file(&self, id: &str) -> Option<String> {
        self.compose_doc(id)["services"]["agent"]["image"]
            .as_str()
            .map(str::to_string)
    }

    fn previous_label(&self, id: &str) -> Option<String> {
        self.compose_doc(id)["services"]["agent"]["labels"]["fleett.previous_image"]
            .as_str()
            .map(str::to_string)
    }

    fn instance_dir(&self, id: &str) -> PathBuf {
        self.state.cfg.instance_dir(id)
    }

    async fn send(
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
            .expect("body")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let (status, body) = self.send(Method::GET, uri, None, Some(TOKEN)).await;
        (status, parse(&body))
    }

    async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (status, body) = self.send(Method::POST, uri, Some(body), Some(TOKEN)).await;
        (status, parse(&body))
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

    async fn detail(&self, id: &str) -> Value {
        let (status, detail) = self.get(&format!("/api/instances/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        detail
    }

    /// Poll `GET /api/upgrade` until the job is finished.
    async fn wait_for_upgrade(&self) -> Value {
        for _ in 0..300 {
            let (status, job) = self.get("/api/upgrade").await;
            if status == StatusCode::OK && job["finished_at"].is_string() {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the upgrade job never finished");
    }

    /// Start an upgrade and wait for it to finish.
    async fn upgrade(&self, body: Value) -> Value {
        let (status, job) = self.post("/api/upgrade", body).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{job}");
        if job["finished_at"].is_string() {
            return job;
        }
        self.wait_for_upgrade().await
    }
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

fn result_for<'a>(job: &'a Value, id: &str) -> &'a Value {
    job["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|result| result["id"] == id)
        .unwrap_or_else(|| panic!("no result for {id}: {job}"))
}

#[tokio::test]
async fn a_new_instance_uses_the_configured_image() {
    let harness = Harness::with_config(|cfg| cfg.image = "agent:0.1".to_string());
    harness.create("alice").await;

    let detail = harness.detail("alice").await;
    assert_eq!(detail["image"], "agent:0.1");
    assert_eq!(harness.image_file("alice").as_deref(), Some("agent:0.1"));
    assert!(harness.previous_label("alice").is_none());
}

#[tokio::test]
async fn a_healthy_upgrade_moves_the_image_and_records_the_previous_tag() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    let original = harness.state.cfg.image.clone();
    harness.mock.clear_calls();

    let job = harness
        .upgrade(json!({"image": TARGET, "timeout_s": 10}))
        .await;
    assert_eq!(job["image"], TARGET);
    assert!(!job["results"].as_array().expect("results").is_empty());

    for id in ["alice", "bob"] {
        let result = result_for(&job, id);
        assert_eq!(result["status"], "healthy", "{result}");
        assert_eq!(result["from"], original);
        assert_eq!(result["to"], TARGET);

        // The compose file is the source of truth: new tag plus the label.
        assert_eq!(harness.image_file(id).as_deref(), Some(TARGET));
        assert_eq!(
            harness.previous_label(id).as_deref(),
            Some(original.as_str())
        );
        assert_eq!(harness.detail(id).await["image"], TARGET);
        assert_eq!(harness.mock.image_of(id).as_deref(), Some(TARGET));
    }

    // Both containers were really restarted with `up -d`.
    let ups: Vec<String> = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| call.operation == "up")
        .map(|call| call.instance)
        .collect();
    assert_eq!(ups, ["alice", "bob"]);
}

#[tokio::test]
async fn a_second_successful_upgrade_moves_the_label_forward() {
    let harness = Harness::new();
    harness.create("alice").await;
    let first = harness.state.cfg.image.clone();

    harness.upgrade(json!({"image": "agent:0.2"})).await;
    let job = harness.upgrade(json!({"image": "agent:0.3"})).await;
    assert_eq!(result_for(&job, "alice")["status"], "healthy");

    assert_eq!(harness.image_file("alice").as_deref(), Some("agent:0.3"));
    assert_eq!(
        harness.previous_label("alice").as_deref(),
        Some("agent:0.2")
    );
    assert_ne!(
        harness.previous_label("alice").as_deref(),
        Some(first.as_str())
    );

    // A failed third upgrade restores 0.3 and keeps the 0.2 label of the last
    // successful move (the compose file is not the only history, it is the
    // history).
    harness.mock.set_image_health("broken:1", Health::Unhealthy);
    let job = harness
        .upgrade(json!({"image": "broken:1", "timeout_s": 5}))
        .await;
    assert_eq!(result_for(&job, "alice")["status"], "rolled_back");
    assert_eq!(harness.image_file("alice").as_deref(), Some("agent:0.3"));
    assert_eq!(
        harness.previous_label("alice").as_deref(),
        Some("agent:0.2")
    );
}

#[tokio::test]
async fn an_unhealthy_image_is_rolled_back() {
    let harness = Harness::new();
    harness.create("alice").await;
    let original = harness.state.cfg.image.clone();
    harness.mock.set_image_health("broken:1", Health::Unhealthy);

    let job = harness
        .upgrade(json!({"image": "broken:1", "timeout_s": 5}))
        .await;
    let result = result_for(&job, "alice");
    assert_eq!(result["status"], "rolled_back", "{job}");
    let error = result["error"].as_str().expect("error");
    assert!(error.contains("upgrade failed"), "{error}");
    assert!(
        error.contains(&format!("rolled back to {original}")),
        "{error}"
    );

    // The instance runs the old tag again, on disk and in the database.
    assert_eq!(
        harness.image_file("alice").as_deref(),
        Some(original.as_str())
    );
    assert!(harness.previous_label("alice").is_none());
    assert_eq!(
        harness.mock.image_of("alice").as_deref(),
        Some(original.as_str())
    );

    let detail = harness.detail("alice").await;
    assert_eq!(detail["image"], original);
    let last_error = detail["last_error"].as_str().expect("last_error");
    assert!(last_error.contains("upgrade failed"), "{last_error}");
    assert!(last_error.contains("unhealthy"), "{last_error}");
    assert!(
        last_error.contains(&format!("rolled back to {original}")),
        "{last_error}"
    );
    assert_eq!(detail["status"], "running");
}

#[tokio::test]
async fn a_failed_pull_is_a_502_and_writes_nothing() {
    let harness = Harness::new();
    harness.create("alice").await;
    let before = harness.compose("alice");
    harness
        .mock
        .fail_image_pull("ghost:1", "manifest unknown: manifest unknown");

    let (status, body) = harness
        .post("/api/upgrade", json!({"image": "ghost:1"}))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"]["code"], "image_unavailable");
    let message = body["error"]["message"].as_str().expect("message");
    assert!(message.contains("ghost:1"), "{message}");
    assert!(message.contains("manifest unknown"), "{message}");

    // No job was recorded and no instance was touched.
    let (status, _) = harness.get("/api/upgrade").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(harness.compose("alice"), before);
}

#[tokio::test]
async fn a_dry_run_lists_the_plan_and_writes_nothing() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    let original = harness.state.cfg.image.clone();
    harness.mock.clear_calls();
    let before = (harness.compose("alice"), harness.compose("bob"));

    let (status, job) = harness
        .post(
            "/api/upgrade",
            json!({"image": TARGET, "dry_run": true, "parallel": 2}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    assert_eq!(job["dry_run"], true);
    assert!(
        job["finished_at"].is_string(),
        "a dry run is finished immediately: {job}"
    );

    let results = job["results"].as_array().expect("results");
    assert_eq!(results.len(), 2);
    for result in results {
        assert_eq!(result["status"], "pending");
        assert_eq!(result["from"], original);
        assert_eq!(result["to"], TARGET);
    }

    // No write, no restart, no pull.
    assert_eq!((harness.compose("alice"), harness.compose("bob")), before);
    assert_eq!(
        harness.image_file("alice").as_deref(),
        Some(original.as_str())
    );
    assert!(
        harness.mock.calls().is_empty(),
        "{:?}",
        harness.mock.calls()
    );
    assert!(harness.mock.image_calls().is_empty());
}

#[tokio::test]
async fn the_parallel_limit_is_respected() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    harness.mock.set_up_delay(Duration::from_millis(120));

    let started = std::time::Instant::now();
    let job = harness
        .upgrade(json!({"image": "agent:0.2", "parallel": 2}))
        .await;
    assert_eq!(result_for(&job, "alice")["status"], "healthy");
    assert_eq!(
        harness.mock.max_concurrent_ups(),
        2,
        "two instances must be upgraded at once with --parallel 2"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the two upgrades ran sequentially"
    );

    // Sequential by default: one `up` at a time, whatever the fleet size.
    let sequential = Harness::new();
    sequential.create("alice").await;
    sequential.create("bob").await;
    sequential.mock.set_up_delay(Duration::from_millis(40));
    let job = sequential.upgrade(json!({"image": "agent:0.2"})).await;
    assert_eq!(result_for(&job, "bob")["status"], "healthy");
    assert_eq!(sequential.mock.max_concurrent_ups(), 1);
}

#[tokio::test]
async fn a_running_job_refuses_a_second_one() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    // Each `up` takes long enough that the job is still running when the
    // second request arrives.
    harness.mock.set_up_delay(Duration::from_millis(250));

    let (status, job) = harness
        .post("/api/upgrade", json!({"image": "agent:0.2"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    assert!(
        job["finished_at"].is_null(),
        "the job must still be running: {job}"
    );

    let (status, body) = harness
        .post("/api/upgrade", json!({"image": "agent:0.3"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "upgrade_in_progress");

    let finished = harness.wait_for_upgrade().await;
    assert_eq!(finished["image"], "agent:0.2");
    for result in finished["results"].as_array().expect("results") {
        assert_eq!(result["status"], "healthy", "{result}");
    }

    // Once finished, a new job is accepted.
    let (status, _) = harness
        .post(
            "/api/upgrade",
            json!({"image": "agent:0.4", "dry_run": true}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn the_image_listing_reports_the_instances_of_each_tag() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    let original = harness.state.cfg.image.clone();

    let (status, images) = harness.get("/api/images").await;
    assert_eq!(status, StatusCode::OK, "{images}");
    let entry = images
        .as_array()
        .expect("images")
        .iter()
        .find(|image| image["tag"] == original.as_str())
        .unwrap_or_else(|| panic!("no entry for {original}: {images}"));
    assert_eq!(entry["instances"], json!(["alice", "bob"]));
    assert!(entry["id"].as_str().expect("id").starts_with("sha256:"));
    assert!(entry["size"].as_str().is_some());

    harness.upgrade(json!({"image": TARGET})).await;
    let (status, images) = harness.get("/api/images").await;
    assert_eq!(status, StatusCode::OK);
    let images = images.as_array().expect("images");
    let old = images
        .iter()
        .find(|image| image["tag"] == original.as_str())
        .expect("the old tag is still local");
    assert_eq!(old["instances"], json!([]), "nobody uses it any more");
    let new = images
        .iter()
        .find(|image| image["tag"] == TARGET)
        .expect("the pulled tag is listed");
    assert_eq!(new["instances"], json!(["alice", "bob"]));
}

#[tokio::test]
async fn the_audit_trail_records_upgrades_and_rollbacks() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    let original = harness.state.cfg.image.clone();

    harness.upgrade(json!({"image": "agent:0.2"})).await;
    harness.mock.set_image_health("broken:1", Health::Unhealthy);
    let job = harness
        .upgrade(json!({"image": "broken:1", "timeout_s": 5}))
        .await;
    for id in ["alice", "bob"] {
        assert_eq!(result_for(&job, id)["status"], "rolled_back");
    }

    let (status, entries) = harness.get("/api/audit?limit=100").await;
    assert_eq!(status, StatusCode::OK, "{entries}");
    let entries = entries.as_array().expect("array");
    let upgrades: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["action"] == "instance.upgrade")
        .collect();
    assert_eq!(upgrades.len(), 2, "{entries:?}");
    assert!(upgrades
        .iter()
        .all(|entry| entry["details"]["to"] == "agent:0.2"));
    assert!(upgrades
        .iter()
        .all(|entry| entry["details"]["status"] == "healthy"));
    assert_eq!(upgrades[0]["details"]["from"], original);

    let rollbacks: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["action"] == "instance.upgrade.rollback")
        .collect();
    assert_eq!(rollbacks.len(), 2, "{entries:?}");
    assert!(rollbacks
        .iter()
        .all(|entry| entry["details"]["status"] == "rolled_back"));
    assert!(rollbacks
        .iter()
        .all(|entry| entry["details"]["to"] == "broken:1"));
}

#[tokio::test]
async fn a_viewer_cannot_start_an_upgrade() {
    let harness = Harness::new();
    harness.create("alice").await;
    let (status, body) = harness
        .send(
            Method::POST,
            "/api/tokens",
            Some(json!({"name": "ci", "role": "viewer"})),
            Some(TOKEN),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let viewer = parse(&body)["token"].as_str().expect("token").to_string();

    let (status, body) = harness
        .send(
            Method::POST,
            "/api/upgrade",
            Some(json!({"image": TARGET})),
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Read access is still fine, and no job was started.
    let (status, _) = harness
        .send(Method::GET, "/api/upgrade", None, Some(&viewer))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        harness.image_file("alice").as_deref(),
        Some(harness.state.cfg.image.as_str())
    );
}

#[tokio::test]
async fn invalid_requests_are_rejected() {
    let harness = Harness::new();
    harness.create("alice").await;

    for (body, code) in [
        (json!({}), "invalid_image"),
        (json!({"image": "  "}), "invalid_image"),
        (json!({"image": "with space"}), "invalid_image"),
        (json!({"image": TARGET, "parallel": 0}), "invalid_parallel"),
        (json!({"image": TARGET, "parallel": 99}), "invalid_parallel"),
        (json!({"image": TARGET, "timeout_s": 0}), "invalid_timeout"),
        (
            json!({"image": TARGET, "timeout_s": 99999}),
            "invalid_timeout",
        ),
    ] {
        let (status, response) = harness.post("/api/upgrade", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} → {response}");
        assert_eq!(response["error"]["code"], code, "{body} → {response}");
    }
}

#[tokio::test]
async fn a_timeout_rolls_back() {
    let harness = Harness::new();
    harness.create("alice").await;
    let original = harness.state.cfg.image.clone();
    // "starting" never becomes healthy: the job must give up on the timeout.
    harness.mock.set_image_health("slow:1", Health::Starting);

    let started = std::time::Instant::now();
    let job = harness
        .upgrade(json!({"image": "slow:1", "timeout_s": 1}))
        .await;
    assert!(started.elapsed() >= Duration::from_millis(900));
    let result = result_for(&job, "alice");
    assert_eq!(result["status"], "rolled_back", "{job}");
    let error = result["error"].as_str().expect("error");
    assert!(error.contains("timed out"), "{error}");
    assert_eq!(
        harness.image_file("alice").as_deref(),
        Some(original.as_str())
    );
}

#[tokio::test]
async fn the_reconciler_follows_the_compose_file() {
    let harness = Harness::new();
    harness.create("alice").await;
    let db = harness.state.db.clone();

    // Simulate an instance written by another build: the compose file moved to
    // another tag without going through the API.
    let compose = harness.compose("alice").replace(
        &format!("image: {}", harness.state.cfg.image),
        "image: hand:1",
    );
    std::fs::write(harness.state.cfg.compose_path("alice"), compose).expect("compose");

    fleett::db::reconcile(&harness.state.cfg, &db).expect("reconcile");
    let spec = db.get("alice").expect("get").expect("alice");
    assert_eq!(spec.image, "hand:1");
    assert!(harness.instance_dir("alice").is_dir());
}
