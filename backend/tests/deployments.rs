//! Integration tests for maintenance windows, canary upgrades, scheduled
//! deployments and the deployment journal (issue #39).
//!
//! Every test runs against `MockDriver`; no test ever calls Docker.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::models::Health;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const TARGET: &str = "hermes-agent:0.23.0-local";

struct Harness {
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

    fn image_file(&self, id: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.state.cfg.compose_path(id)).expect("compose");
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("compose YAML");
        doc["services"]["agent"]["image"]
            .as_str()
            .map(str::to_string)
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

    async fn patch(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (status, body) = self.send(Method::PATCH, uri, Some(body), Some(TOKEN)).await;
        (status, parse(&body))
    }

    async fn delete(&self, uri: &str) -> StatusCode {
        self.send(Method::DELETE, uri, None, Some(TOKEN)).await.0
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

    async fn wait_for_upgrade(&self) -> Value {
        for _ in 0..600 {
            let (status, job) = self.get("/api/upgrade").await;
            if status == StatusCode::OK && job["finished_at"].is_string() {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the upgrade job never finished");
    }

    async fn wait_for_result(&self, id: &str, status: &str) -> Value {
        for _ in 0..600 {
            let (code, job) = self.get("/api/upgrade").await;
            if code == StatusCode::OK {
                if let Some(result) = result_for(&job, id) {
                    if result["status"] == status {
                        return job;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("instance {id} never reached {status}");
    }

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

fn result_for<'a>(job: &'a Value, id: &str) -> Option<&'a Value> {
    job["results"]
        .as_array()?
        .iter()
        .find(|result| result["id"] == id)
}

/// A window that is closed right now (and for the next hour).
fn closed_window() -> Value {
    let now = chrono::Utc::now();
    let minute = chrono::Timelike::hour(&now) * 60 + chrono::Timelike::minute(&now);
    let start = (minute + 60) % 1440;
    let end = (start + 60) % 1440;
    let (start, end) = if end > start { (start, end) } else { (0, 60) };
    json!({
        "name": "nightly",
        "days": ["mon", "tue", "wed", "thu", "fri", "sat", "sun"],
        "start_minute": start,
        "end_minute": end,
    })
}

#[tokio::test]
async fn windows_are_created_listed_updated_and_deleted() {
    let harness = Harness::new();
    let (status, window) = harness
        .post(
            "/api/maintenance-windows",
            json!({"name": "weekend", "days": ["sat", "sun"], "start_minute": 120, "end_minute": 240}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{window}");
    let id = window["id"].as_str().expect("id").to_string();
    assert_eq!(window["days"], json!(["sat", "sun"]));
    assert_eq!(window["enabled"], true);

    let (status, windows) = harness.get("/api/maintenance-windows").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(windows.as_array().expect("array").len(), 1);

    let (status, updated) = harness
        .patch(
            &format!("/api/maintenance-windows/{id}"),
            json!({"enabled": false, "name": "weekend", "days": "sun", "start_minute": 60}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["enabled"], false);
    assert_eq!(updated["days"], json!(["sun"]));
    assert_eq!(updated["start_minute"], 60);
    assert_eq!(updated["end_minute"], 240);

    assert_eq!(
        harness
            .delete(&format!("/api/maintenance-windows/{id}"))
            .await,
        StatusCode::NO_CONTENT
    );
    let (_, windows) = harness.get("/api/maintenance-windows").await;
    assert!(windows.as_array().expect("array").is_empty());
    assert_eq!(
        harness
            .delete(&format!("/api/maintenance-windows/{id}"))
            .await,
        StatusCode::NOT_FOUND
    );

    // Invalid inputs are refused, not coerced.
    for body in [
        json!({"name": "x", "days": [], "start_minute": 0, "end_minute": 10}),
        json!({"name": "x", "days": ["funday"], "start_minute": 0, "end_minute": 10}),
        json!({"name": "x", "days": ["mon"], "start_minute": 2000, "end_minute": 10}),
        json!({"name": "  ", "days": ["mon"], "start_minute": 0, "end_minute": 10}),
    ] {
        let (status, response) = harness.post("/api/maintenance-windows", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert_eq!(response["error"]["code"], "invalid_window", "{response}");
    }
}

#[tokio::test]
async fn the_status_reports_the_gate_and_the_next_opening() {
    let harness = Harness::new();

    // No window: nothing is enforced.
    let (status, report) = harness.get("/api/maintenance-windows/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["open"], true);
    assert_eq!(report["enforce"], true);
    assert!(report["next_open"].is_null());

    // A full-day window: open now.
    let (status, window) = harness
        .post(
            "/api/maintenance-windows",
            json!({"name": "all", "days": ["mon","tue","wed","thu","fri","sat","sun"], "start_minute": 0, "end_minute": 0}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{window}");
    let (_, report) = harness.get("/api/maintenance-windows/status").await;
    assert_eq!(report["open"], true);

    // Move it to a slot that is closed now.
    let closed = closed_window();
    let (status, _) = harness
        .patch(
            &format!(
                "/api/maintenance-windows/{}",
                window["id"].as_str().expect("id")
            ),
            json!({
                "days": closed["days"],
                "start_minute": closed["start_minute"],
                "end_minute": closed["end_minute"],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, report) = harness.get("/api/maintenance-windows/status").await;
    assert_eq!(report["open"], false, "{report}");
    let next = report["next_open"].as_str().expect("next_open");
    let next = chrono::DateTime::parse_from_rfc3339(next).expect("RFC 3339");
    assert!(next.with_timezone(&chrono::Utc) > chrono::Utc::now());
}

#[tokio::test]
async fn a_closed_window_refuses_an_upgrade_before_pulling_the_image() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.mock.clear_calls();
    let before = harness.image_file("alice");
    let (status, window) = harness
        .post("/api/maintenance-windows", closed_window())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{window}");

    let (status, body) = harness.post("/api/upgrade", json!({"image": TARGET})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "maintenance_window");
    let next = body["error"]["details"]["next_open"]
        .as_str()
        .expect("details.next_open");
    assert!(chrono::DateTime::parse_from_rfc3339(next).is_ok(), "{next}");
    assert!(body["error"]["message"]
        .as_str()
        .expect("message")
        .contains(next));

    // Nothing happened: no pull, no compose write, no job.
    assert!(
        harness.mock.image_calls().is_empty(),
        "{:?}",
        harness.mock.image_calls()
    );
    assert!(harness.mock.calls().is_empty());
    assert_eq!(harness.image_file("alice"), before);
    let (status, _) = harness.get("/api/upgrade").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Deleting an instance is destructive too, and refused the same way.
    let (status, body) = harness
        .send(Method::DELETE, "/api/instances/alice", None, Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(harness.state.db.get("alice").expect("get").is_some());
}

#[tokio::test]
async fn disabling_enforcement_lets_the_upgrade_through() {
    let harness = Harness::with_config(|cfg| cfg.maintenance_enforce = false);
    harness.create("alice").await;
    let (status, window) = harness
        .post("/api/maintenance-windows", closed_window())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{window}");

    let (_, report) = harness.get("/api/maintenance-windows/status").await;
    assert_eq!(report["open"], true);
    assert_eq!(report["enforce"], false);

    let job = harness
        .upgrade(json!({"image": TARGET, "timeout_s": 10}))
        .await;
    assert_eq!(
        result_for(&job, "alice").expect("result")["status"],
        "healthy"
    );
}

#[tokio::test]
async fn a_canary_moves_first_and_the_journal_records_the_deployment() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    harness.create("carol").await;
    harness.mock.clear_calls();

    let started = std::time::Instant::now();
    let job = harness
        .upgrade(json!({"image": TARGET, "canary": 1, "soak_s": 1, "timeout_s": 10}))
        .await;
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "the soak must actually wait: {:?}",
        started.elapsed()
    );
    assert_eq!(job["canary"], 1);
    assert_eq!(job["soak_s"], 1);
    for id in ["alice", "bob", "carol"] {
        assert_eq!(
            result_for(&job, id).expect("result")["status"],
            "healthy",
            "{job}"
        );
        assert_eq!(harness.image_file(id).as_deref(), Some(TARGET));
    }

    // The canary moved first, then the rest in order.
    let ups: Vec<String> = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| call.operation == "up")
        .map(|call| call.instance)
        .collect();
    assert_eq!(ups, ["alice", "bob", "carol"]);

    let deployment_id = job["deployment_id"]
        .as_str()
        .expect("deployment_id")
        .to_string();
    assert_eq!(deployment_id, job["id"].as_str().expect("job id"));

    let (status, deployment) = harness
        .get(&format!("/api/deployments/{deployment_id}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{deployment}");
    assert_eq!(deployment["status"], "ok");
    assert_eq!(deployment["canary"], 1);
    assert_eq!(deployment["soak_s"], 1);
    assert_eq!(deployment["instances"], json!(["alice", "bob", "carol"]));
    assert_eq!(deployment["rollback"], false);
    assert!(deployment["finished_at"].is_string());
    assert_eq!(
        deployment["result"].as_array().expect("results").len(),
        3,
        "{deployment}"
    );

    let (status, journal) = harness.get("/api/deployments?status=ok&limit=10").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(journal.as_array().expect("journal").len(), 1);
    let (_, journal) = harness.get("/api/deployments?status=failed").await;
    assert!(journal.as_array().expect("journal").is_empty());

    assert_eq!(
        harness.get("/api/deployments?limit=0").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        harness.get("/api/deployments?status=bogus").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        harness.get("/api/deployments/ghost").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_failing_canary_stops_the_deployment_and_leaves_the_rest_untouched() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    let original = harness.state.cfg.image.clone();
    // The target image is unhealthy: the canary upgrade itself rolls back.
    harness.mock.set_image_health(TARGET, Health::Unhealthy);

    let job = harness
        .upgrade(json!({"image": TARGET, "canary": 1, "soak_s": 0, "timeout_s": 5}))
        .await;
    assert_eq!(
        result_for(&job, "alice").expect("alice")["status"],
        "rolled_back",
        "{job}"
    );
    let id = "bob";
    let result = result_for(&job, id).expect("result");
    assert_eq!(result["status"], "pending", "{result}");
    assert!(
        result["error"]
            .as_str()
            .expect("error")
            .contains("not attempted"),
        "{result}"
    );
    // Untouched: same compose tag, same container image.
    assert_eq!(harness.image_file(id).as_deref(), Some(original.as_str()));
    assert_eq!(
        harness.mock.image_of(id).as_deref(),
        Some(original.as_str())
    );

    let deployment_id = job["deployment_id"].as_str().expect("deployment_id");
    let (_, deployment) = harness
        .get(&format!("/api/deployments/{deployment_id}"))
        .await;
    assert_eq!(deployment["status"], "rolled_back", "{deployment}");
    assert_eq!(deployment["rollback"], true);
}

#[tokio::test]
async fn a_canary_that_becomes_unhealthy_during_the_soak_is_rolled_back_alone() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    harness.create("carol").await;
    let original = harness.state.cfg.image.clone();

    let (status, started) = harness
        .post(
            "/api/upgrade",
            json!({"image": TARGET, "canary": 1, "soak_s": 2, "timeout_s": 5}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");

    // Wait until alice runs the target image, then break it: this is what a
    // crash after a successful healthcheck looks like to the soak.
    harness.wait_for_result("alice", "healthy").await;
    harness.mock.set_instance_health("alice", Health::Unhealthy);

    let job = harness.wait_for_upgrade().await;
    let alice = result_for(&job, "alice").expect("alice");
    assert_eq!(alice["status"], "rolled_back", "{job}");
    assert!(
        alice["error"]
            .as_str()
            .expect("error")
            .contains("became unhealthy"),
        "{alice}"
    );
    assert_eq!(
        harness.image_file("alice").as_deref(),
        Some(original.as_str())
    );
    assert_eq!(
        harness.mock.image_of("alice").as_deref(),
        Some(original.as_str())
    );

    for id in ["bob", "carol"] {
        let result = result_for(&job, id).expect("result");
        assert_eq!(result["status"], "pending", "{result}");
        assert!(
            result["error"]
                .as_str()
                .expect("error")
                .contains("not attempted"),
            "{result}"
        );
        assert_eq!(harness.image_file(id).as_deref(), Some(original.as_str()));
        assert_eq!(
            harness.mock.image_of(id).as_deref(),
            Some(original.as_str())
        );
    }

    let deployment_id = job["deployment_id"].as_str().expect("deployment_id");
    let (_, deployment) = harness
        .get(&format!("/api/deployments/{deployment_id}"))
        .await;
    assert_eq!(deployment["status"], "rolled_back");
    assert_eq!(deployment["rollback"], true);
}

#[tokio::test]
async fn a_scheduled_upgrade_is_journaled_then_fires_when_due() {
    let harness = Harness::new();
    harness.create("alice").await;
    let at = (chrono::Utc::now() + chrono::Duration::seconds(2))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let (status, scheduled) = harness
        .post(
            "/api/upgrade",
            json!({"image": TARGET, "canary": 1, "soak_s": 0, "at": at}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{scheduled}");
    assert_eq!(scheduled["status"], "scheduled");
    assert_eq!(scheduled["image"], TARGET);
    assert_eq!(scheduled["canary"], 1);
    let id = scheduled["id"].as_str().expect("id").to_string();

    // Nothing runs now and the journal keeps the request.
    let (status, _) = harness.get("/api/upgrade").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, rows) = harness.get("/api/deployments?status=scheduled").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows.as_array().expect("rows").len(), 1);
    let (_, row) = harness.get(&format!("/api/deployments/{id}")).await;
    assert_eq!(row["status"], "scheduled");
    assert_eq!(row["result"]["request"]["image"], TARGET);

    // Wait for the due time, then tick the scheduler once.
    tokio::time::sleep(Duration::from_millis(2200)).await;
    fleett::maintenance::tick_scheduled(&harness.state).await;
    let job = harness.wait_for_upgrade().await;
    assert_eq!(
        result_for(&job, "alice").expect("result")["status"],
        "healthy"
    );
    assert_eq!(job["id"], id, "the job reuses the journal id");

    let (_, row) = harness.get(&format!("/api/deployments/{id}")).await;
    assert_eq!(row["status"], "ok", "{row}");
    assert!(row["finished_at"].is_string());
    assert_eq!(row["instances"], json!(["alice"]));
}

#[tokio::test]
async fn a_scheduled_upgrade_is_cancelled_when_the_window_is_closed_at_firing() {
    let harness = Harness::new();
    harness.create("alice").await;
    let (status, window) = harness
        .post("/api/maintenance-windows", closed_window())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{window}");

    // Booking outside the window is allowed: the gate applies at firing time.
    let at = (chrono::Utc::now() + chrono::Duration::seconds(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (status, scheduled) = harness
        .post("/api/upgrade", json!({"image": TARGET, "at": at}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{scheduled}");
    let id = scheduled["id"].as_str().expect("id").to_string();

    tokio::time::sleep(Duration::from_millis(1500)).await;
    fleett::maintenance::tick_scheduled(&harness.state).await;

    let (_, row) = harness.get(&format!("/api/deployments/{id}")).await;
    assert_eq!(row["status"], "cancelled", "{row}");
    assert!(
        row["error"]
            .as_str()
            .expect("error")
            .contains("maintenance_window"),
        "{row}"
    );
    let (status, _) = harness.get("/api/upgrade").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nothing was started");
    assert!(harness.mock.image_calls().is_empty());
}

#[tokio::test]
async fn invalid_upgrade_bodies_are_rejected() {
    let harness = Harness::new();
    harness.create("alice").await;

    let past = (chrono::Utc::now() - chrono::Duration::seconds(60))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    for (body, code) in [
        (json!({"image": TARGET, "canary": 9}), "invalid_canary"),
        (json!({"image": TARGET, "soak_s": 3601}), "invalid_soak"),
        (json!({"image": TARGET, "at": past}), "invalid_at"),
        (
            json!({"image": TARGET, "at": "someday 04:00"}),
            "invalid_at",
        ),
        (
            json!({"image": TARGET, "window": "ghost"}),
            "invalid_window",
        ),
    ] {
        let (status, response) = harness.post("/api/upgrade", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} → {response}");
        assert_eq!(response["error"]["code"], code, "{body} → {response}");
    }
}

#[tokio::test]
async fn a_viewer_cannot_manage_the_windows() {
    let harness = Harness::new();
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
            "/api/maintenance-windows",
            Some(closed_window()),
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Reading stays allowed.
    let (status, _) = harness
        .send(
            Method::GET,
            "/api/maintenance-windows/status",
            None,
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}
