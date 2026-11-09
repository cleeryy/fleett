//! Integration tests for the Prometheus endpoint (issue #15).
//!
//! Every test runs against `MockDriver` and drives the two routers directly:
//! `api::router` for the control plane, `metrics::router` for `GET /metrics`.
//! No test binds a port or calls Docker.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::{Config, MetricsConfig};
use fleett::driver::mock::MockDriver;
use fleett::metrics;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const METRICS_TOKEN: &str = "scrape-me";

struct Harness {
    /// Kept alive for the duration of the test (the data root lives here).
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

    /// A harness whose metrics listener is configured like production.
    fn with_metrics(bind: Option<&str>, token: Option<&str>) -> Self {
        let bind = bind.map(|bind| bind.parse().expect("valid metrics bind"));
        let token = token.map(str::to_string);
        Self::with_config(move |cfg| {
            cfg.metrics = MetricsConfig { bind, token };
        })
    }

    /// Send one request to the control plane.
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
        collect(
            api::router(self.state.clone()),
            builder.body(body).expect("request"),
        )
        .await
    }

    /// Send one request to the metrics listener.
    async fn scrape(&self, bearer: Option<&str>) -> (StatusCode, HeaderMap, String) {
        let mut builder = Request::builder().method(Method::GET).uri("/metrics");
        if let Some(token) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = builder.body(Body::empty()).expect("request");
        let response = metrics::router(self.state.clone())
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, headers, String::from_utf8_lossy(&bytes).to_string())
    }

    /// Scrape the listener and return the exposition text.
    async fn metrics_text(&self) -> String {
        let (status, _, body) = self.scrape(None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
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

    async fn stop(&self, id: &str) {
        let (status, body) = self
            .send(
                Method::POST,
                &format!("/api/instances/{id}/stop"),
                None,
                Some(TOKEN),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// Calls the driver received for one operation.
    fn driver_calls(&self, operation: &str) -> usize {
        self.mock
            .calls()
            .iter()
            .filter(|call| call.operation == operation)
            .count()
    }
}

async fn collect(router: axum::Router, request: Request<Body>) -> (StatusCode, String) {
    let response = router.oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// Value of one series, matched on the exact `name{labels}` text.
fn value_of(text: &str, series: &str) -> Option<f64> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.rsplit_once(' '))
        .find(|(head, _)| *head == series)
        .and_then(|(_, value)| value.parse::<f64>().ok())
}

fn expect_value(text: &str, series: &str) -> f64 {
    value_of(text, series).unwrap_or_else(|| panic!("{series} missing from:\n{text}"))
}

#[tokio::test]
async fn the_scrape_exposes_the_fleet_in_the_prometheus_text_format() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;

    let (status, headers, text) = harness.scrape(None).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some(metrics::CONTENT_TYPE)
    );
    assert!(
        metrics::CONTENT_TYPE.contains("version=0.0.4"),
        "the text format version is part of the content type"
    );

    // HELP/TYPE preamble, then the values.
    for name in [
        "fleett_build_info",
        "fleett_up",
        "fleett_instances",
        "fleett_instance_cpu_percent",
        "fleett_instance_mem_used_bytes",
        "fleett_instance_mem_limit_bytes",
        "fleett_instance_disk_bytes",
        "fleett_api_requests_total",
        "fleett_compose_actions_total",
        "fleett_upgrade_jobs_total",
        "fleett_rate_limited_total",
        "fleett_usage_ok",
    ] {
        assert!(
            text.contains(&format!("# HELP {name} ")),
            "{name} has no HELP line:\n{text}"
        );
        assert!(
            text.contains(&format!("# TYPE {name} ")),
            "{name} has no TYPE line:\n{text}"
        );
    }

    assert_eq!(
        expect_value(
            &text,
            &format!(
                "fleett_build_info{{version=\"{}\"}}",
                env!("CARGO_PKG_VERSION")
            )
        ),
        1.0
    );
    assert_eq!(expect_value(&text, "fleett_up"), 1.0);
    assert_eq!(
        expect_value(
            &text,
            "fleett_instances{status=\"running\",health=\"healthy\"}"
        ),
        2.0,
        "{text}"
    );
    assert_eq!(expect_value(&text, "fleett_usage_ok"), 1.0);

    for id in ["alice", "bob"] {
        let cpu = expect_value(
            &text,
            &format!("fleett_instance_cpu_percent{{id=\"{id}\"}}"),
        );
        assert!(cpu > 0.0, "{id} reports a CPU usage:\n{text}");
        assert!(
            expect_value(
                &text,
                &format!("fleett_instance_mem_used_bytes{{id=\"{id}\"}}")
            ) > 0.0
        );
        assert_eq!(
            expect_value(
                &text,
                &format!("fleett_instance_mem_limit_bytes{{id=\"{id}\"}}")
            ),
            (1536 * 1024 * 1024) as f64
        );
        assert!(expect_value(&text, &format!("fleett_instance_disk_bytes{{id=\"{id}\"}}")) > 0.0);
    }

    // Every sample is a finite number Prometheus can parse.
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let (head, value) = line.rsplit_once(' ').unwrap_or_else(|| panic!("{line:?}"));
        assert!(!head.is_empty(), "no metric name on {line:?}");
        let value: f64 = value
            .parse()
            .unwrap_or_else(|err| panic!("{line:?} is not a number: {err}"));
        assert!(value.is_finite(), "{line:?} is not finite");
    }
}

#[tokio::test]
async fn stopping_an_instance_moves_the_counters() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    harness.stop("bob").await;

    let text = harness.metrics_text().await;
    assert_eq!(
        expect_value(
            &text,
            "fleett_compose_actions_total{action=\"stop\",result=\"ok\"}"
        ),
        1.0,
        "{text}"
    );
    assert_eq!(
        expect_value(
            &text,
            "fleett_compose_actions_total{action=\"up\",result=\"ok\"}"
        ),
        2.0,
        "creating the two instances ran two `compose up`:\n{text}"
    );
    assert_eq!(
        expect_value(
            &text,
            "fleett_instances{status=\"running\",health=\"healthy\"}"
        ),
        1.0,
        "{text}"
    );
    assert_eq!(
        expect_value(
            &text,
            "fleett_instances{status=\"stopped\",health=\"none\"}"
        ),
        1.0,
        "{text}"
    );
    assert_eq!(
        expect_value(&text, "fleett_instance_cpu_percent{id=\"bob\"}"),
        0.0,
        "the stopped container reports no CPU:\n{text}"
    );
}

#[tokio::test]
async fn api_requests_are_counted_by_method_and_status() {
    let harness = Harness::new();
    harness.create("alice").await;
    let (status, _) = harness.send(Method::GET, "/api/health", None, None).await;
    assert_eq!(status, StatusCode::OK);

    let text = harness.metrics_text().await;
    assert_eq!(
        expect_value(
            &text,
            "fleett_api_requests_total{method=\"POST\",status=\"201\"}"
        ),
        1.0,
        "{text}"
    );
    assert!(
        expect_value(
            &text,
            "fleett_api_requests_total{method=\"GET\",status=\"200\"}"
        ) >= 1.0,
        "GET /api/health is counted:\n{text}"
    );

    // A refused request is counted under its own status.
    let (status, _) = harness.send(Method::GET, "/api/usage", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let text = harness.metrics_text().await;
    assert_eq!(
        expect_value(
            &text,
            "fleett_api_requests_total{method=\"GET\",status=\"401\"}"
        ),
        1.0,
        "{text}"
    );
}

#[tokio::test]
async fn a_scrape_inside_the_cache_window_does_not_sample_docker_again() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.create("bob").await;
    harness.mock.clear_calls();

    harness.metrics_text().await;
    assert_eq!(
        harness.driver_calls("usage"),
        2,
        "one utilisation sample covers the whole fleet"
    );

    harness.metrics_text().await;
    assert_eq!(
        harness.driver_calls("usage"),
        2,
        "the second scrape inside the 10s window is served from the cache"
    );

    // A lifecycle action drops the cache, so the next scrape samples again.
    harness.stop("bob").await;
    harness.metrics_text().await;
    assert_eq!(
        harness.driver_calls("usage"),
        4,
        "stopping an instance invalidates the cached sample"
    );
}

#[tokio::test]
async fn the_metrics_token_is_required_when_one_is_configured() {
    let harness = Harness::with_metrics(Some("127.0.0.1:9100"), Some(METRICS_TOKEN));
    harness.create("alice").await;

    let (status, _, body) = harness.scrape(None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, _, body) = harness.scrape(Some("wrong-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, _, body) = harness.scrape(Some(TOKEN)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the API token is not the metrics token:\n{body}"
    );

    let (status, _, body) = harness.scrape(Some(METRICS_TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("fleett_up 1"), "{body}");
}

#[tokio::test]
async fn a_loopback_bind_without_a_token_answers_anonymously() {
    let harness = Harness::with_metrics(Some("127.0.0.1:9100"), None);
    let (status, _, body) = harness.scrape(None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("fleett_instances{status=\"running\",health=\"healthy\"} 0"));
}

#[tokio::test]
async fn an_upgrade_job_is_counted_once_it_finishes() {
    let harness = Harness::new();
    harness.create("alice").await;
    harness.mock.register_image("hermes-agent:0.23.0");

    let (status, body) = harness
        .send(
            Method::POST,
            "/api/upgrade",
            Some(json!({"image": "hermes-agent:0.23.0", "timeout_s": 5})),
            Some(TOKEN),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    // Wait for the job to finish (the mock is instant, but the job runs on a
    // background task).
    for _ in 0..200 {
        let (status, body) = harness
            .send(Method::GET, "/api/upgrade", None, Some(TOKEN))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let job: Value = serde_json::from_str(&body).expect("job JSON");
        if job["finished_at"].is_string() {
            assert_eq!(job["results"][0]["status"], "healthy", "{job}");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let text = harness.metrics_text().await;
    assert_eq!(
        expect_value(&text, "fleett_upgrade_jobs_total{result=\"success\"}"),
        1.0,
        "{text}"
    );
    // The upgrade itself is a compose action.
    assert_eq!(
        expect_value(
            &text,
            "fleett_compose_actions_total{action=\"upgrade\",result=\"ok\"}"
        ),
        1.0,
        "{text}"
    );
}
