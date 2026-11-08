//! Integration tests for `GET /api/usage` (issue #10) and the health fields
//! the CLI `doctor` command reads.
//!
//! Every test runs against `MockDriver`; no test ever calls Docker.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::{Config, EgressMode};
use fleett::driver::mock::MockDriver;
use fleett::driver::UsageSample;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const MIB: u64 = 1024 * 1024;

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
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        configure(&mut cfg);
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        Self { dir, state, mock }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = match body {
            Some(value) => Body::from(value.to_string()),
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
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

#[tokio::test]
async fn usage_reports_each_instance_its_totals_and_the_caps() {
    let harness = Harness::with_config(|cfg| {
        cfg.max_total_cpu = 4.0;
        cfg.max_total_mem_mb = 6144;
    });
    harness.create("alice").await;
    harness.create("bob").await;

    let (status, report) = harness.get("/api/usage").await;
    assert_eq!(status, StatusCode::OK, "{report}");

    let instances = report["instances"].as_array().expect("instances");
    assert_eq!(instances.len(), 2);
    assert_eq!(instances[0]["id"], "alice");
    assert_eq!(instances[1]["id"], "bob");

    let configured = 1536 * MIB;
    for entry in instances {
        assert_eq!(entry["status"], "running");
        assert!(entry["cpu_pct"].as_f64().expect("cpu") > 0.0, "{entry}");
        assert!(entry["mem_used_bytes"].as_u64().expect("mem") > 0);
        assert_eq!(entry["mem_limit_bytes"].as_u64(), Some(configured));
        assert!(entry["disk_bytes"].as_u64().expect("disk") > 0);
    }

    // Totals are the sum of the instances.
    let cpu: f64 = instances
        .iter()
        .map(|entry| entry["cpu_pct"].as_f64().expect("cpu"))
        .sum();
    let mem: u64 = instances
        .iter()
        .map(|entry| entry["mem_used_bytes"].as_u64().expect("mem"))
        .sum();
    let disk: u64 = instances
        .iter()
        .map(|entry| entry["disk_bytes"].as_u64().expect("disk"))
        .sum();
    let totals = &report["totals"];
    assert!((totals["cpu_pct"].as_f64().expect("cpu") - cpu).abs() < 1e-9);
    assert_eq!(totals["mem_used_bytes"].as_u64(), Some(mem));
    assert_eq!(totals["mem_limit_bytes"].as_u64(), Some(2 * configured));
    assert_eq!(totals["disk_bytes"].as_u64(), Some(disk));

    assert_eq!(report["caps"]["max_cpu"].as_f64(), Some(4.0));
    assert_eq!(report["caps"]["max_mem_mb"].as_u64(), Some(6144));
}

#[tokio::test]
async fn a_stopped_instance_reports_zero_usage() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;

    let (status, body) = harness
        .send(Method::POST, "/api/instances/bob/stop", None, Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, report) = harness.get("/api/usage").await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let bob = report["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .find(|entry| entry["id"] == "bob")
        .expect("bob");
    assert_eq!(bob["status"], "stopped");
    assert_eq!(bob["cpu_pct"].as_f64(), Some(0.0));
    assert_eq!(bob["mem_used_bytes"].as_u64(), Some(0));
    assert_eq!(bob["disk_bytes"].as_u64(), Some(0));

    let alice = report["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .find(|entry| entry["id"] == "alice")
        .expect("alice");
    assert!(alice["cpu_pct"].as_f64().expect("cpu") > 0.0);
}

#[tokio::test]
async fn forced_samples_are_reported_verbatim() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.mock.force_usage(
        "alice",
        UsageSample {
            running: true,
            cpu_pct: 12.5,
            mem_used_bytes: 100 * MIB,
            mem_limit_bytes: 256 * MIB,
            disk_bytes: 3 * MIB,
        },
    );

    let (status, report) = harness.get("/api/usage").await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let alice = &report["instances"][0];
    assert_eq!(alice["cpu_pct"].as_f64(), Some(12.5));
    assert_eq!(alice["mem_used_bytes"].as_u64(), Some(100 * MIB));
    assert_eq!(alice["mem_limit_bytes"].as_u64(), Some(256 * MIB));
    assert_eq!(alice["disk_bytes"].as_u64(), Some(3 * MIB));
    assert_eq!(report["totals"]["cpu_pct"].as_f64(), Some(12.5));
    assert_eq!(report["totals"]["disk_bytes"].as_u64(), Some(3 * MIB));
}

#[tokio::test]
async fn usage_failures_are_502_with_a_clear_message() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.mock.fail_next(
        "usage",
        "Cannot connect to the Docker daemon at unix:///var/run/docker.sock",
    );

    let (status, report) = harness.get("/api/usage").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{report}");
    assert_eq!(report["error"]["code"], "usage_unavailable");
    let message = report["error"]["message"].as_str().expect("message");
    assert!(message.contains("Docker daemon"), "{message}");
    assert!(message.contains("cannot read"), "{message}");
}

#[tokio::test]
async fn an_empty_fleet_reports_zeros() {
    let harness = Harness::new();
    let (status, report) = harness.get("/api/usage").await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["instances"], json!([]));
    assert_eq!(
        report["totals"],
        json!({
            "cpu_pct": 0.0,
            "mem_used_bytes": 0,
            "mem_limit_bytes": 0,
            "disk_bytes": 0
        })
    );
    assert_eq!(report["caps"], json!({"max_cpu": 0.0, "max_mem_mb": 0}));
}

#[tokio::test]
async fn a_viewer_may_read_the_usage() {
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
        .send(Method::GET, "/api/usage", None, Some(&viewer))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A mutation is still refused.
    let (status, _) = harness
        .send(
            Method::POST,
            "/api/instances/alice/stop",
            None,
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn health_reports_the_secrets_and_egress_state() {
    let harness = Harness::new();
    let (status, body) = harness.send(Method::GET, "/api/health", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let health = parse(&body);
    assert_eq!(health["secrets"]["key"], true);
    assert_eq!(health["secrets"]["encrypted"], 0);
    assert_eq!(health["secrets"]["plaintext"], 0);
    assert_eq!(health["egress"]["mode"], "off");
    assert_eq!(health["egress"]["marker"], false);
}

#[tokio::test]
async fn health_sees_the_egress_marker_and_plaintext_secrets() {
    let harness = Harness::with_config(|cfg| {
        cfg.egress = EgressMode::Restrict;
    });
    std::fs::write(
        &harness.state.cfg.egress_marker,
        r#"{"subnet":"172.18.0.0/16","installed_at":"2026-11-08T01:00:00Z","version":"0.3.0"}"#,
    )
    .expect("write marker file");
    harness.create("alice").await;
    std::fs::write(
        harness.root().join("instances/alice/secrets.env"),
        "TELEGRAM_BOT_TOKEN=legacy\n",
    )
    .expect("legacy secrets");

    let (status, body) = harness.send(Method::GET, "/api/health", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let health = parse(&body);
    assert_eq!(health["egress"]["mode"], "restrict");
    assert_eq!(health["egress"]["marker"], true);
    assert_eq!(health["secrets"]["key"], true);
    assert_eq!(health["secrets"]["plaintext"], 1);
}
