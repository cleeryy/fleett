//! Integration tests for the first-run assistant and the doctor checklist
//! (issue #52).
//!
//! Every test runs against `MockDriver`; nothing calls Docker and no check
//! reaches the network.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use fleett::api::{self, AppState};
use fleett::config::{Config, EgressMode};
use fleett::doctor;
use fleett::driver::mock::MockDriver;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const MISSING_IMAGE: &str = "fleett-test:missing";
const MODEL: &str = "deepseek-v4.1-flash";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const PROVIDER_BASE_URL: &str = "https://provider.test/v1";

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
        send_to(api::router(self.state.clone()), method, uri, body, token).await
    }

    async fn get_json(&self, path: &str) -> Value {
        let (status, body) = self.request(Method::GET, path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        parse(&body)
    }

    async fn onboarding(&self) -> Value {
        self.get_json("/api/onboarding").await
    }
}

async fn send_to(
    router: Router,
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
    let response = router.oneshot(request).await.expect("response");
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

/// One check of a doctor report.
fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("no checks array: {report}"))
        .iter()
        .find(|check| check["id"] == id)
        .unwrap_or_else(|| panic!("no \"{id}\" check: {report}"))
}

/// One step of an onboarding report.
fn step<'a>(onboarding: &'a Value, id: &str) -> &'a Value {
    onboarding["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("no steps array: {onboarding}"))
        .iter()
        .find(|step| step["id"] == id)
        .unwrap_or_else(|| panic!("no \"{id}\" step: {onboarding}"))
}

fn done_count(onboarding: &Value) -> usize {
    onboarding["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .filter(|step| step["done"] == true)
        .count()
}

/// Write the marker `fleett egress install` leaves behind, for one version.
fn write_marker(harness: &Harness, version: &str) {
    let path = &harness.state.cfg.egress_marker;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("marker dir");
    }
    let marker = json!({
        "subnet": "172.30.0.0/16",
        "installed_at": "2026-11-09T10:00:00Z",
        "version": version,
    });
    std::fs::write(path, marker.to_string()).expect("marker file");
}

async fn create_provider(harness: &Harness) -> Value {
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/providers",
            Some(json!({
                "name": "openai",
                "kind": "openai-compatible",
                "base_url": PROVIDER_BASE_URL,
                "api_key_env": "PROVIDER_API_KEY",
                "secret": "sk-test",
            })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create provider failed: {body}"
    );
    parse(&body)
}

async fn create_instance(harness: &Harness, id: &str, provider: Option<&str>) -> Value {
    let mut body = json!({
        "id": id,
        "name": format!("Agent {id}"),
        "template": "guest",
        "model": MODEL,
        "base_url": BASE_URL,
        "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
        "channel": {"type": "telegram", "token": "123:ABC"},
        "shares": [],
        "mem_limit_mb": 1024,
    });
    if let Some(provider) = provider {
        body["provider"] = json!(provider);
    }
    let (status, response) = harness
        .request(Method::POST, "/api/instances", Some(body))
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create instance failed: {response}"
    );
    parse(&response)
}

/// `ok` is exactly "no check is an error": environment-dependent warnings (a
/// full host, a provider not created yet) never break this invariant.
fn assert_ok_matches(report: &Value) {
    let expected = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .all(|check| check["status"] != "error");
    assert_eq!(report["ok"], expected, "{report}");
}

// ---------------------------------------------------------------------------
// GET /api/doctor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn doctor_reports_every_check_with_a_status() {
    let harness = Harness::new();
    let report = harness.get_json("/api/doctor").await;

    let checks = report["checks"].as_array().expect("checks array");
    let ids: Vec<&str> = checks
        .iter()
        .map(|check| check["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, doctor::CHECK_IDS, "{report}");

    for check in checks {
        assert!(
            !check["label"].as_str().unwrap_or_default().is_empty(),
            "{check}"
        );
        assert!(
            !check["detail"].as_str().unwrap_or_default().is_empty(),
            "{check}"
        );
        assert!(
            matches!(check["status"].as_str(), Some("ok" | "warn" | "error")),
            "{check}"
        );
    }

    let api = check(&report, "api");
    assert_eq!(api["status"], "ok");
    assert!(api["detail"]
        .as_str()
        .expect("detail")
        .contains(env!("CARGO_PKG_VERSION")));

    // The mock driver is announced as simulated, never as a real engine.
    let docker = check(&report, "docker");
    assert_eq!(docker["status"], "ok");
    assert!(docker["detail"].as_str().expect("detail").contains("mock"));

    // A fresh fleet: the default image is present, no provider yet.
    assert_eq!(check(&report, "image")["status"], "ok");
    assert_eq!(check(&report, "secrets")["status"], "ok");
    assert_eq!(check(&report, "providers")["status"], "warn");
    assert_eq!(check(&report, "instances")["status"], "ok");
    assert_eq!(check(&report, "permissions")["status"], "ok");
    assert!(check(&report, "disk")["detail"]
        .as_str()
        .expect("detail")
        .contains("free of"));
    assert!(check(&report, "dns")["detail"]
        .as_str()
        .expect("detail")
        .contains("nameserver"));

    // Warnings do not fail the report.
    assert_ok_matches(&report);
}

#[tokio::test]
async fn doctor_explains_a_missing_image_with_the_exact_command() {
    let harness = Harness::with_config(|cfg| cfg.image = MISSING_IMAGE.to_string());
    let report = harness.get_json("/api/doctor").await;

    let image = check(&report, "image");
    assert_eq!(image["status"], "error", "{report}");
    assert!(image["detail"]
        .as_str()
        .expect("detail")
        .contains(MISSING_IMAGE));
    assert_eq!(
        image["fix"],
        format!("docker build -t {MISSING_IMAGE} /path/to/hermes-official")
    );
    assert_eq!(report["ok"], false);
    assert_ok_matches(&report);

    // Building it (simulated by registering the tag) clears the check.
    harness.mock.register_image(MISSING_IMAGE);
    let report = harness.get_json("/api/doctor").await;
    assert_eq!(check(&report, "image")["status"], "ok");
    assert_ok_matches(&report);
}

#[tokio::test]
async fn doctor_flags_a_missing_secrets_key() {
    let harness = Harness::with_config(|cfg| cfg.secrets_key = None);
    let report = harness.get_json("/api/doctor").await;

    let secrets = check(&report, "secrets");
    assert_eq!(secrets["status"], "error", "{report}");
    assert!(secrets["detail"].as_str().expect("detail").contains("key"));
    assert_eq!(secrets["fix"], "sudo fleett secrets init");
    assert_eq!(report["ok"], false);
}

#[tokio::test]
async fn doctor_counts_legacy_plaintext_secrets_as_a_warning() {
    let harness = Harness::new();
    let dir = harness.state.cfg.instance_dir("legacy");
    std::fs::create_dir_all(&dir).expect("instance dir");
    std::fs::write(
        harness.state.cfg.secrets_path("legacy"),
        "MODEL_API_KEY=sk-legacy\n",
    )
    .expect("legacy secrets file");

    let report = harness.get_json("/api/doctor").await;
    let secrets = check(&report, "secrets");
    assert_eq!(secrets["status"], "warn", "{report}");
    assert!(secrets["detail"]
        .as_str()
        .expect("detail")
        .contains("1 file still plaintext"));
    assert_eq!(secrets["fix"], "fleett secrets migrate --apply");
    // A warning never fails the report.
    assert_ok_matches(&report);
}

#[tokio::test]
async fn doctor_reports_the_egress_modes_and_the_marker() {
    // `off` is a warning: the fleet works, the isolation does not.
    let off = Harness::new();
    let report = off.get_json("/api/doctor").await;
    let egress = check(&report, "egress");
    assert_eq!(egress["status"], "warn");
    assert!(
        egress["detail"].as_str().expect("detail").contains("off"),
        "{egress}"
    );
    assert!(egress["fix"]
        .as_str()
        .expect("fix")
        .contains("FLEETT_EGRESS=restrict"));

    // `restrict` without a marker: the install command is the fix.
    let harness = Harness::with_config(|cfg| cfg.egress = EgressMode::Restrict);
    let report = harness.get_json("/api/doctor").await;
    let egress = check(&report, "egress");
    assert_eq!(egress["status"], "warn", "{report}");
    assert_eq!(egress["fix"], "sudo fleett egress install");
    assert!(egress["detail"]
        .as_str()
        .expect("detail")
        .contains("not installed"));

    // A marker written by this build: installed.
    write_marker(&harness, env!("CARGO_PKG_VERSION"));
    let report = harness.get_json("/api/doctor").await;
    let egress = check(&report, "egress");
    assert_eq!(egress["status"], "ok", "{report}");
    assert!(egress["detail"]
        .as_str()
        .expect("detail")
        .contains("172.30.0.0/16"));

    // A marker left by another build is stale.
    write_marker(&harness, "0.0.1");
    let report = harness.get_json("/api/doctor").await;
    let egress = check(&report, "egress");
    assert_eq!(egress["status"], "warn", "{report}");
    assert!(egress["detail"].as_str().expect("detail").contains("stale"));
    assert_eq!(egress["fix"], "sudo fleett egress install");
}

#[tokio::test]
async fn doctor_measures_the_free_space_of_the_data_root() {
    let harness = Harness::new();
    let report = harness.get_json("/api/doctor").await;
    let disk = check(&report, "disk");
    let detail = disk["detail"].as_str().expect("detail");
    assert!(
        detail.contains(harness.root().to_str().expect("utf-8 root")),
        "{detail}"
    );
    assert!(detail.contains("free of"), "{detail}");
    assert!(matches!(
        disk["status"].as_str(),
        Some("ok" | "warn" | "error")
    ));
}

#[tokio::test]
async fn a_failing_check_is_reported_inline_not_as_a_500() {
    let harness = Harness::new();
    harness
        .mock
        .fail_next("images", "the Docker daemon is unreachable");

    let (status, body) = harness.request(Method::GET, "/api/doctor", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let report = parse(&body);
    let image = check(&report, "image");
    assert_eq!(image["status"], "error", "{report}");
    assert!(image["detail"]
        .as_str()
        .expect("detail")
        .contains("the Docker daemon is unreachable"));
    assert_eq!(report["ok"], false);
    // The rest of the checklist is still reported.
    assert_eq!(
        report["checks"].as_array().expect("checks").len(),
        doctor::CHECK_IDS.len()
    );
    assert_eq!(check(&report, "api")["status"], "ok");
}

// ---------------------------------------------------------------------------
// GET /api/onboarding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn onboarding_walks_an_empty_host_to_a_complete_fleet() {
    // A fresh host: no engine, no image, no provider, no instance.
    let harness = Harness::with_config(|cfg| cfg.image = MISSING_IMAGE.to_string());
    harness.mock.set_engine_down(true);

    let onboarding = harness.onboarding().await;
    assert_eq!(onboarding["complete"], false, "{onboarding}");
    let ids: Vec<&str> = onboarding["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|step| step["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["docker", "image", "provider", "instance"]);
    assert!(
        onboarding["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .all(|step| step["done"] == false),
        "{onboarding}"
    );
    assert_eq!(
        step(&onboarding, "docker")["action"],
        json!({"kind": "command", "command": "sudo systemctl start docker && docker info"})
    );
    assert_eq!(
        step(&onboarding, "image")["action"],
        json!({
            "kind": "command",
            "command": format!("docker build -t {MISSING_IMAGE} /path/to/hermes-official"),
        })
    );
    assert_eq!(
        step(&onboarding, "provider")["action"],
        json!({"kind": "link", "to": "/providers?new=1"})
    );
    assert_eq!(
        step(&onboarding, "instance")["action"],
        json!({"kind": "link", "to": "/instances?new=1"})
    );

    // The engine answers and the image is built: two of four.
    harness.mock.set_engine_down(false);
    harness.mock.register_image(MISSING_IMAGE);
    let onboarding = harness.onboarding().await;
    assert_eq!(done_count(&onboarding), 2, "{onboarding}");

    // A provider: three of four, with the remaining action on the instance.
    let provider = create_provider(&harness).await;
    let provider_id = provider["id"].as_str().expect("provider id").to_string();
    let onboarding = harness.onboarding().await;
    assert_eq!(done_count(&onboarding), 3, "{onboarding}");
    assert_eq!(step(&onboarding, "provider")["done"], true);
    assert!(step(&onboarding, "provider").get("action").is_none());
    assert_eq!(onboarding["complete"], false);

    // The first instance completes the tour.
    create_instance(&harness, "probe", Some(&provider_id)).await;
    let onboarding = harness.onboarding().await;
    assert_eq!(onboarding["complete"], true, "{onboarding}");
    assert_eq!(done_count(&onboarding), 4, "{onboarding}");
    assert!(
        onboarding["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .all(|step| step.get("action").is_none()),
        "{onboarding}"
    );
}

#[tokio::test]
async fn onboarding_is_complete_once_an_instance_exists() {
    let harness = Harness::new();
    let onboarding = harness.onboarding().await;
    // The mock fleet starts with its image registered: Docker and the image
    // are already satisfied, the provider and instance are not.
    assert_eq!(done_count(&onboarding), 2, "{onboarding}");
    assert_eq!(onboarding["complete"], false);

    // An instance can be created without a provider: it is the fourth step
    // that decides, not the provider whoever skipped it.
    create_instance(&harness, "probe", None).await;
    let onboarding = harness.onboarding().await;
    assert_eq!(onboarding["complete"], true, "{onboarding}");
    assert_eq!(step(&onboarding, "instance")["done"], true);
    assert_eq!(step(&onboarding, "provider")["done"], false);
}

#[tokio::test]
async fn a_viewer_reads_the_doctor_and_the_onboarding() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/tokens",
            Some(json!({"name": "ci", "role": "viewer"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = parse(&body)["token"]
        .as_str()
        .expect("token once")
        .to_string();

    for path in ["/api/doctor", "/api/onboarding"] {
        let (status, body) = harness
            .request_as(Method::GET, path, None, Some(&token))
            .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }

    // Anonymous callers stay rejected, like every other protected route.
    for path in ["/api/doctor", "/api/onboarding"] {
        let (status, _) = harness.request_as(Method::GET, path, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
}
