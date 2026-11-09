//! Integration tests of the watchdog incident API (issue #32).
//!
//! Every test drives `watchdog::tick_at` explicitly: no background loop runs
//! and no test ever calls Docker.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::driver::InspectInfo;
use fleett::models::{
    ChannelKind, HealPolicy, Health, InstanceSpec, Status, DEFAULT_API_KEY_ENV,
    DEFAULT_HEAL_BACKOFF_S, DEFAULT_MEM_LIMIT_MB, DEFAULT_UNHEALTHY_AFTER,
};
use fleett::watchdog;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";

struct Harness {
    _dir: tempfile::TempDir,
    state: AppState,
    mock: Arc<MockDriver>,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().to_path_buf());
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap");
        Self {
            _dir: dir,
            state,
            mock,
        }
    }

    fn instance(&self, id: &str) -> InstanceSpec {
        InstanceSpec {
            id: id.to_string(),
            name: id.to_string(),
            template: "guest".to_string(),
            model: "deepseek-v4.1-flash".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            provider: None,
            channel: ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: DEFAULT_MEM_LIMIT_MB,
            cpu_limit: 1.0,
            image: "hermes-agent:0.22.2-local".to_string(),
            previous_image: None,
            allow_lan: false,
            heal: HealPolicy::Off,
            unhealthy_after: DEFAULT_UNHEALTHY_AFTER,
            max_restarts: 5,
            heal_backoff_s: DEFAULT_HEAL_BACKOFF_S,
            notes: String::new(),
            status: Status::Running,
            last_error: None,
            created_at: fleett::models::now_iso(),
            updated_at: fleett::models::now_iso(),
        }
    }

    fn add(&self, id: &str) -> InstanceSpec {
        let spec = self.instance(id);
        self.state.db.insert_instance(&spec).expect("insert");
        self.mock.force_status(id, Status::Running);
        spec
    }

    async fn tick(&self, now: u64) {
        watchdog::tick_at(&self.state, now).await;
    }

    async fn request(
        &self,
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
        let request = builder.body(body).expect("request");
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
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

#[tokio::test]
async fn incidents_are_listed_closed_and_audited() {
    let harness = Harness::new();
    let instance = harness.add("alice");
    harness.mock.force_health("alice", Health::Unhealthy);

    for step in 0..3 {
        harness.tick(100 + step * 30).await;
    }

    let (status, body) = harness
        .request(Method::GET, "/api/incidents?status=open", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let incidents = parse(&body);
    let incidents = incidents.as_array().expect("array");
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0]["instance_id"], "alice");
    assert_eq!(incidents[0]["cause"], "unhealthy");
    assert_eq!(incidents[0]["status"], "open");
    let incident_id = incidents[0]["id"].as_str().expect("id").to_string();

    let (status, body) = harness
        .request(
            Method::GET,
            &format!("/api/instances/{}/incidents", instance.id),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body).as_array().expect("array").len(), 1);

    let (status, body) = harness
        .request(
            Method::POST,
            &format!("/api/incidents/{incident_id}/close"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["status"], "closed");

    // Closing twice is a conflict, not a second audit entry.
    let (status, _) = harness
        .request(
            Method::POST,
            &format!("/api/incidents/{incident_id}/close"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, body) = harness
        .request(Method::GET, "/api/incidents?status=open", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse(&body).as_array().expect("array").is_empty());

    let (status, body) = harness
        .request(Method::GET, "/api/audit?limit=100", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let actions: Vec<String> = parse(&body)
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|entry| entry["action"].as_str().map(str::to_string))
        .collect();
    assert!(
        actions.contains(&"incident.close".to_string()),
        "{actions:?}"
    );
}

#[tokio::test]
async fn heal_policy_is_updated_validated_and_audited() {
    let harness = Harness::new();
    harness.add("alice");

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice/heal",
            Some(json!({
                "heal": "restart",
                "unhealthy_after": 2,
                "max_restarts": 4,
                "heal_backoff_s": 10,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let policy = parse(&body);
    assert_eq!(policy["heal"], "restart");
    assert_eq!(policy["unhealthy_after"], 2);
    assert_eq!(policy["max_restarts"], 4);
    assert_eq!(policy["heal_backoff_s"], 10);

    let stored = harness
        .state
        .db
        .get("alice")
        .expect("get")
        .expect("present");
    assert_eq!(stored.heal, HealPolicy::Restart);
    assert_eq!(stored.unhealthy_after, 2);
    assert_eq!(stored.max_restarts, 4);
    assert_eq!(stored.heal_backoff_s, 10);

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice/heal",
            Some(json!({ "heal": "yolo" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_heal_policy");

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice/heal",
            Some(json!({ "unhealthy_after": 0 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_unhealthy_after");

    let (status, body) = harness
        .request(Method::GET, "/api/audit?limit=100", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entry = parse(&body)
        .as_array()
        .expect("array")
        .iter()
        .find(|entry| entry["action"] == "instance.heal")
        .cloned()
        .expect("heal audit entry");
    assert_eq!(entry["target"], "alice");
    assert_eq!(entry["details"]["after"]["heal"], "restart");
}

#[tokio::test]
async fn a_stopped_instance_records_no_incident() {
    let harness = Harness::new();
    let mut instance = harness.add("alice");
    instance.heal = HealPolicy::Restart;
    harness
        .state
        .db
        .set_heal_policy("alice", instance.heal, 3, 5, 30)
        .expect("policy");
    harness.mock.force_status("alice", Status::Stopped);
    harness.mock.force_health("alice", Health::Unknown);

    for step in 0..5 {
        harness.tick(100 + step * 30).await;
    }

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/incidents", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(parse(&body).as_array().expect("array").is_empty());
    assert!(harness
        .mock
        .calls()
        .iter()
        .all(|call| call.operation != "restart"));
}

#[tokio::test]
async fn a_crashloop_at_max_restarts_keeps_the_incident_open() {
    let harness = Harness::new();
    harness.add("alice");
    harness
        .state
        .db
        .set_heal_policy("alice", HealPolicy::Restart, 3, 5, 30)
        .expect("policy");
    harness.mock.force_status("alice", Status::Error);
    harness.mock.force_health("alice", Health::Unknown);
    harness.mock.force_inspect(
        "alice",
        InspectInfo {
            state: "exited".to_string(),
            exit_code: Some(1),
            oom_killed: false,
            restart_count: 5,
            error: None,
            started_at: None,
        },
    );

    harness.tick(100).await;
    // A second pass must not heal or open a second incident.
    harness.tick(130).await;

    let (status, body) = harness
        .request(Method::GET, "/api/incidents?instance=alice", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let incidents = parse(&body);
    let incidents = incidents.as_array().expect("array");
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0]["cause"], "crashloop");
    assert_eq!(incidents[0]["status"], "open");
    assert_eq!(incidents[0]["restart_count"], 5);

    let stored = harness
        .state
        .db
        .get("alice")
        .expect("get")
        .expect("present");
    assert_eq!(stored.status, Status::Error);
    assert!(harness
        .mock
        .calls()
        .iter()
        .all(|call| call.operation != "restart"));
}

#[tokio::test]
async fn listing_rejects_an_unknown_instance_and_a_bad_status() {
    let harness = Harness::new();

    let (status, body) = harness
        .request(Method::GET, "/api/incidents?instance=ghost", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body) = harness
        .request(Method::GET, "/api/incidents?status=weird", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_status");

    let (status, _) = harness
        .request(Method::GET, "/api/incidents?limit=0", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn incident_routes_require_authentication() {
    let harness = Harness::new();
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/incidents")
        .body(Body::empty())
        .expect("request");
    let response = api::router(harness.state.clone())
        .oneshot(request)
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_unknown_incident_close_is_a_404() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(Method::POST, "/api/incidents/ghost/close", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
