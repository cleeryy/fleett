//! Integration tests for token accounting, pricing and budgets (issue #35).
//!
//! Every test runs against `MockDriver`; no test ever calls Docker.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];

struct Harness {
    dir: tempfile::TempDir,
    state: AppState,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        let state = AppState::bootstrap(cfg, Arc::new(MockDriver::new())).expect("bootstrap state");
        Self { dir, state }
    }

    #[allow(dead_code)]
    fn root(&self) -> &std::path::Path {
        self.dir.path()
    }

    async fn send(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(match body {
                Some(value) => Body::from(value.to_string()),
                None => Body::empty(),
            })
            .expect("valid request");
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
        let text = String::from_utf8_lossy(&bytes).to_string();
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|err| panic!("invalid JSON ({err}): {text}"))
        };
        (status, value)
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
        let (status, response) = self.send(Method::POST, "/api/instances", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{response}");
    }

    async fn set_price(&self, input: f64, output: f64, effective_from: &str) {
        let (status, response) = self
            .send(
                Method::POST,
                "/api/models/pricing",
                Some(json!({
                    "model": MODEL,
                    "input_per_mtok": input,
                    "output_per_mtok": output,
                    "effective_from": effective_from,
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{response}");
    }
}

#[tokio::test]
async fn ingesting_a_log_line_reports_tokens_and_a_cost() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.set_price(2.0, 8.0, "2026-01-01").await;

    let line = "gateway completed prompt_tokens=1000 completion_tokens=500 total_tokens=1500";
    let (status, summary) = harness
        .send(
            Method::POST,
            "/api/usage/ingest",
            Some(json!({"instance": "alice", "lines": [line]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{summary}");
    assert_eq!(summary["parsed"].as_u64(), Some(1));
    assert_eq!(summary["inserted"].as_u64(), Some(1));

    let (status, report) = harness.send(Method::GET, "/api/cost", None).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["totals"]["total_tokens"].as_u64(), Some(1500));
    assert_eq!(report["totals"]["prompt_tokens"].as_u64(), Some(1000));
    assert_eq!(report["totals"]["completion_tokens"].as_u64(), Some(500));
    assert_eq!(report["totals"]["events"].as_u64(), Some(1));
    let cost = report["totals"]["cost_usd"].as_f64().expect("cost");
    assert!((cost - 0.006).abs() < 1e-12, "{cost}");
    assert_eq!(report["groups"][0]["key"], "alice");
    assert_eq!(report["groups"][0]["total_tokens"].as_u64(), Some(1500));
    assert_eq!(report["top_agents"][0]["instance_id"], "alice");
}

#[tokio::test]
async fn replaying_the_same_lines_never_double_counts() {
    let harness = Harness::new();
    harness.create("alice").await;
    let line = "prompt_tokens=10 completion_tokens=5";
    let body = json!({"instance": "alice", "lines": [line]});

    let (status, first) = harness
        .send(Method::POST, "/api/usage/ingest", Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["inserted"].as_u64(), Some(1));

    let (status, second) = harness
        .send(Method::POST, "/api/usage/ingest", Some(body))
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["parsed"].as_u64(), Some(1));
    assert_eq!(second["inserted"].as_u64(), Some(0));
    assert_eq!(second["duplicates"].as_u64(), Some(1));

    let (_, report) = harness.send(Method::GET, "/api/cost", None).await;
    assert_eq!(report["totals"]["total_tokens"].as_u64(), Some(15));
    assert_eq!(report["totals"]["events"].as_u64(), Some(1));
}

#[tokio::test]
async fn a_crossed_budget_is_reported_with_its_projection() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.set_price(2.0, 8.0, "2026-01-01").await;

    let (status, _) = harness
        .send(
            Method::POST,
            "/api/usage/ingest",
            Some(json!({
                "instance": "alice",
                "lines": ["prompt_tokens=1000 completion_tokens=500"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, budget) = harness
        .send(
            Method::PUT,
            "/api/instances/alice/budget",
            Some(json!({"monthly_usd": 0.001})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{budget}");
    assert_eq!(budget["configured"], true);
    assert_eq!(budget["exceeded"], true);
    assert!(budget["percent"].as_f64().expect("percent") > 100.0);
    assert!(
        budget["projected_month_usd"].as_f64().expect("projection")
            >= budget["used_usd"].as_f64().expect("used")
    );

    let (status, budgets) = harness.send(Method::GET, "/api/cost/budgets", None).await;
    assert_eq!(status, StatusCode::OK, "{budgets}");
    assert_eq!(budgets.as_array().expect("budgets").len(), 1);
    assert_eq!(budgets[0]["instance_id"], "alice");
    assert_eq!(budgets[0]["exceeded"], true);

    let (_, report) = harness.send(Method::GET, "/api/cost", None).await;
    assert_eq!(report["budgets"][0]["instance_id"], "alice");
    assert_eq!(report["budgets"][0]["exceeded"], true);
    let projection = report["projection"]["projected_usd"]
        .as_f64()
        .expect("projected");
    assert!(projection >= report["projection"]["month_to_date_usd"].as_f64().unwrap());
}

#[tokio::test]
async fn prices_are_versioned_and_the_history_keeps_its_own_price() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.set_price(1.0, 0.0, "2026-01-01").await;

    let (status, _) = harness
        .send(
            Method::POST,
            "/api/usage/ingest",
            Some(json!({
                "instance": "alice",
                "model": MODEL,
                "prompt_tokens": 1_000_000,
                "at": "2026-01-15T00:00:00Z",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // The new price only applies from February on.
    harness.set_price(2.0, 0.0, "2026-02-01").await;
    let (status, _) = harness
        .send(
            Method::POST,
            "/api/usage/ingest",
            Some(json!({
                "instance": "alice",
                "model": MODEL,
                "prompt_tokens": 1_000_000,
                "at": "2026-02-15T00:00:00Z",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, report) = harness
        .send(
            Method::GET,
            "/api/cost?from=2026-01-01&to=2026-03-01&group_by=day",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["groups"].as_array().expect("groups").len(), 2);
    let day = |key: &str| {
        report["groups"]
            .as_array()
            .expect("groups")
            .iter()
            .find(|group| group["key"] == key)
            .unwrap_or_else(|| panic!("no group {key}: {report}"))["cost_usd"]
            .as_f64()
            .expect("cost")
    };
    assert!((day("2026-01-15") - 1.0).abs() < 1e-12, "{report}");
    assert!((day("2026-02-15") - 2.0).abs() < 1e-12, "{report}");
    assert!((report["totals"]["cost_usd"].as_f64().unwrap() - 3.0).abs() < 1e-12);

    // The list only shows the latest version of each model.
    let (status, prices) = harness.send(Method::GET, "/api/models/pricing", None).await;
    assert_eq!(status, StatusCode::OK, "{prices}");
    assert_eq!(prices.as_array().expect("prices").len(), 1);
    assert_eq!(prices[0]["input_per_mtok"].as_f64(), Some(2.0));
    assert_eq!(prices[0]["effective_from"], "2026-02-01");
}

#[tokio::test]
async fn ingest_rejects_an_unknown_instance_or_an_empty_payload() {
    let harness = Harness::new();
    harness.create("alice").await;

    let (status, body) = harness
        .send(
            Method::POST,
            "/api/usage/ingest",
            Some(json!({"instance": "ghost", "lines": ["prompt_tokens=1"]})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body) = harness
        .send(
            Method::POST,
            "/api/usage/ingest",
            Some(json!({"instance": "alice"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "missing_usage");
}

#[tokio::test]
async fn deleting_a_model_price_removes_every_version() {
    let harness = Harness::new();
    harness.set_price(1.0, 2.0, "2026-01-01").await;
    harness.set_price(1.5, 2.5, "2026-02-01").await;

    let path = format!("/api/models/pricing/{}", MODEL);
    let (status, body) = harness.send(Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, prices) = harness.send(Method::GET, "/api/models/pricing", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(prices.as_array().expect("prices").len(), 0);
}
