//! Integration tests for DAG pipelines (issue #31).
//!
//! Every test runs against `MockDriver`; no test ever calls Docker. The mock
//! driver is told exactly what each `exec` answers, which is how the chain
//! `{{steps.<id>.output}}` and the failure paths are observed.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::driver::ExecOutput;
use fleett::models::{
    now_iso, ChannelKind, InstanceSpec, Status, DEFAULT_CPU_LIMIT, DEFAULT_MEM_LIMIT_MB,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "test-token";

struct Harness {
    // Keeps the data root alive for the whole test.
    _dir: tempfile::TempDir,
    state: AppState,
    mock: Arc<MockDriver>,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().to_path_buf());
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        Self {
            _dir: dir,
            state,
            mock,
        }
    }

    /// Insert a running instance directly: the pipeline engine only reads the
    /// spec, it never renders a compose file.
    fn instance(&self, id: &str) {
        let now = now_iso();
        self.state
            .db
            .insert_instance(&InstanceSpec {
                id: id.to_string(),
                name: id.to_string(),
                template: "guest".to_string(),
                model: "m".to_string(),
                base_url: "https://example.test/v1".to_string(),
                api_key_env: "HERMES_GATE_PROFILE_TOKEN".to_string(),
                provider: None,
                channel: ChannelKind::None,
                shares: Vec::new(),
                skills: Vec::new(),
                mcp_servers: Vec::new(),
                mem_limit_mb: DEFAULT_MEM_LIMIT_MB,
                cpu_limit: DEFAULT_CPU_LIMIT,
                image: "agent:1".to_string(),
                previous_image: None,
                allow_lan: false,
                host: "local".to_string(),
                notes: String::new(),
                heal: fleett::models::HealPolicy::Off,
                unhealthy_after: fleett::models::DEFAULT_UNHEALTHY_AFTER,
                max_restarts: fleett::models::DEFAULT_MAX_RESTARTS,
                heal_backoff_s: fleett::models::DEFAULT_HEAL_BACKOFF_S,
                disk_quota_mb: 0,
                disk_policy: fleett::models::DiskPolicy::Warn,
                status: Status::Running,
                last_error: None,
                created_at: now.clone(),
                updated_at: now,
            })
            .expect("insert instance");
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        send_to(api::router(self.state.clone()), method, uri, body, TOKEN).await
    }

    async fn create_pipeline(&self, body: Value) -> Value {
        let (status, text) = self
            .request(Method::POST, "/api/pipelines", Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "create failed: {text}");
        parse(&text)
    }

    async fn start_run(&self, pipeline: &str) -> Value {
        let (status, text) = self
            .request(
                Method::POST,
                &format!("/api/pipelines/{pipeline}/run"),
                Some(json!({})),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "run failed: {text}");
        parse(&text)
    }

    async fn wait_run(&self, pipeline: &str, run: &str) -> Value {
        for _ in 0..250 {
            let (status, text) = self
                .request(
                    Method::GET,
                    &format!("/api/pipelines/{pipeline}/runs/{run}"),
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "run detail failed: {text}");
            let detail = parse(&text);
            let state = detail["status"].as_str().unwrap_or_default();
            if matches!(state, "ok" | "failed" | "cancelled") {
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("run {run} did not finish");
    }
}

async fn send_to(
    router: Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: &str,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
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

fn output(stdout: &str) -> ExecOutput {
    ExecOutput {
        exit_code: Some(0),
        stdout: stdout.to_string(),
        stderr: String::new(),
        timed_out: false,
        duration_ms: 0,
        truncated: false,
    }
}

fn failure(stderr: &str) -> ExecOutput {
    ExecOutput {
        exit_code: Some(2),
        stdout: String::new(),
        stderr: stderr.to_string(),
        timed_out: false,
        duration_ms: 0,
        truncated: false,
    }
}

fn step(id: &str, instance: &str, depends_on: &[&str], prompt: &str) -> Value {
    json!({
        "id": id,
        "instance": instance,
        "prompt": prompt,
        "depends_on": depends_on,
    })
}

fn step_status(detail: &Value, id: &str) -> String {
    detail["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .find(|step| step["step_id"] == id)
        .and_then(|step| step["status"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn step_output(detail: &Value, id: &str) -> String {
    detail["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .find(|step| step["step_id"] == id)
        .and_then(|step| step["output"].as_str())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_three_step_pipeline_pipes_outputs_and_keeps_each_run() {
    let harness = Harness::new();
    for id in ["alice", "bob", "carol"] {
        harness.instance(id);
    }
    let pipeline = harness
        .create_pipeline(json!({
            "name": "release",
            "trigger": "manual",
            "steps": [
                step("produce", "alice", &[], "draft the release notes"),
                step("verify", "bob", &["produce"], "review {{steps.produce.output}}"),
                step("publish", "carol", &["verify"], "publish {{steps.verify.output}}"),
            ],
        }))
        .await;
    let pipeline_id = pipeline["id"].as_str().expect("id").to_string();
    assert_eq!(pipeline["steps"].as_array().expect("steps").len(), 3);

    // First run.
    harness.mock.push_exec_result("alice", output("draft-1"));
    harness.mock.push_exec_result("bob", output("review-1"));
    harness.mock.push_exec_result("carol", output("publish-1"));
    let run = harness.start_run(&pipeline_id).await;
    let run_id = run["id"].as_str().expect("run id").to_string();
    assert_eq!(run["status"], "pending");
    let detail = harness.wait_run(&pipeline_id, &run_id).await;
    assert_eq!(detail["status"], "ok", "{detail}");
    assert_eq!(step_status(&detail, "produce"), "ok");
    assert_eq!(step_status(&detail, "verify"), "ok");
    assert_eq!(step_status(&detail, "publish"), "ok");
    assert_eq!(step_output(&detail, "produce"), "draft-1");
    assert_eq!(step_output(&detail, "publish"), "publish-1");

    let prompts: Vec<String> = harness
        .mock
        .exec_calls()
        .iter()
        .map(|call| call.argv.last().cloned().unwrap_or_default())
        .collect();
    assert!(
        prompts.contains(&"review draft-1".to_string()),
        "step 2 did not receive step 1 output: {prompts:?}"
    );
    assert!(
        prompts.contains(&"publish review-1".to_string()),
        "step 3 did not receive step 2 output: {prompts:?}"
    );

    // A second run is a new UUID; the first run is untouched.
    harness.mock.push_exec_result("alice", output("draft-2"));
    harness.mock.push_exec_result("bob", output("review-2"));
    harness.mock.push_exec_result("carol", output("publish-2"));
    let second = harness.start_run(&pipeline_id).await;
    let second_id = second["id"].as_str().expect("run id").to_string();
    assert_ne!(second_id, run_id);
    let second_detail = harness.wait_run(&pipeline_id, &second_id).await;
    assert_eq!(second_detail["status"], "ok");
    assert_eq!(step_output(&second_detail, "produce"), "draft-2");

    let (status, text) = harness
        .request(
            Method::GET,
            &format!("/api/pipelines/{pipeline_id}/runs/{run_id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let first = parse(&text);
    assert_eq!(first["status"], "ok");
    assert_eq!(step_output(&first, "produce"), "draft-1");
    assert_eq!(step_output(&first, "publish"), "publish-1");

    let (status, text) = harness
        .request(
            Method::GET,
            &format!("/api/pipelines/{pipeline_id}/runs?limit=10"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let runs = parse(&text);
    assert_eq!(runs.as_array().expect("runs").len(), 2);
    assert_eq!(runs[0]["id"], second_id, "newest run first");
}

#[tokio::test]
async fn a_failed_step_stops_the_run_and_skips_its_dependents() {
    let harness = Harness::new();
    for id in ["alice", "bob"] {
        harness.instance(id);
    }
    let pipeline = harness
        .create_pipeline(json!({
            "name": "deploy",
            "steps": [
                step("build", "alice", &[], "build it"),
                step("ship", "bob", &["build"], "ship {{steps.build.output}}"),
            ],
        }))
        .await;
    let pipeline_id = pipeline["id"].as_str().expect("id").to_string();
    harness
        .mock
        .push_exec_result("alice", failure("compiler exploded"));

    let run = harness.start_run(&pipeline_id).await;
    let detail = harness
        .wait_run(&pipeline_id, run["id"].as_str().expect("run id"))
        .await;
    assert_eq!(detail["status"], "failed", "{detail}");
    assert_eq!(step_status(&detail, "build"), "failed");
    assert_eq!(step_status(&detail, "ship"), "skipped");
    // Only the first step reached the driver.
    assert_eq!(harness.mock.exec_calls().len(), 1);

    let (status, text) = harness
        .request(
            Method::GET,
            &format!("/api/pipelines/{pipeline_id}/runs"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let runs = parse(&text);
    assert_eq!(runs[0]["status"], "failed");
    assert!(runs[0]["error"].as_str().is_some(), "{text}");
}

#[tokio::test]
async fn independent_steps_run_in_parallel_up_to_the_cap() {
    let harness = Harness::new();
    for id in ["a", "b", "c", "d"] {
        harness.instance(id);
    }
    let pipeline = harness
        .create_pipeline(json!({
            "name": "fanout",
            "steps": [
                step("one", "a", &[], "one"),
                step("two", "b", &[], "two"),
                step("three", "c", &[], "three"),
                step("four", "d", &[], "four"),
            ],
        }))
        .await;
    let pipeline_id = pipeline["id"].as_str().expect("id").to_string();
    harness.mock.set_exec_delay(Duration::from_millis(120));

    let run = harness.start_run(&pipeline_id).await;
    let detail = harness
        .wait_run(&pipeline_id, run["id"].as_str().expect("run id"))
        .await;
    assert_eq!(detail["status"], "ok", "{detail}");
    let concurrent = harness.mock.max_concurrent_execs();
    assert!(
        (2..=4).contains(&concurrent),
        "expected parallel scheduling (2..=4), saw {concurrent}"
    );
}

#[tokio::test]
async fn a_run_can_be_cancelled_between_steps() {
    let harness = Harness::new();
    for id in ["alice", "bob"] {
        harness.instance(id);
    }
    let pipeline = harness
        .create_pipeline(json!({
            "name": "slow",
            "steps": [
                step("first", "alice", &[], "first"),
                step("second", "bob", &["first"], "second"),
            ],
        }))
        .await;
    let pipeline_id = pipeline["id"].as_str().expect("id").to_string();
    harness.mock.set_exec_delay(Duration::from_millis(150));
    harness.mock.push_exec_result("alice", output("done-1"));
    harness.mock.push_exec_result("bob", output("done-2"));

    let run = harness.start_run(&pipeline_id).await;
    let run_id = run["id"].as_str().expect("run id").to_string();

    // Wait until the engine is actually running, then cancel.
    let mut started = false;
    for _ in 0..100 {
        let (_, text) = harness
            .request(
                Method::GET,
                &format!("/api/pipelines/{pipeline_id}/runs/{run_id}"),
                None,
            )
            .await;
        let detail = parse(&text);
        if detail["status"] == "running" {
            started = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(started, "the run never started");

    let (status, text) = harness
        .request(
            Method::POST,
            &format!("/api/pipelines/{pipeline_id}/runs/{run_id}/cancel"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "cancel failed: {text}");
    let cancelled = parse(&text);
    assert_eq!(cancelled["status"], "cancelled");

    let detail = harness.wait_run(&pipeline_id, &run_id).await;
    assert_eq!(detail["status"], "cancelled", "{detail}");
    assert_eq!(step_status(&detail, "second"), "skipped");
    // Cancelling an already finished run is a conflict.
    let (status, text) = harness
        .request(
            Method::POST,
            &format!("/api/pipelines/{pipeline_id}/runs/{run_id}/cancel"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(parse(&text)["error"]["code"], "run_finished");
}

#[tokio::test]
async fn invalid_definitions_are_refused_with_clear_codes() {
    let harness = Harness::new();
    let (status, text) = harness
        .request(
            Method::POST,
            "/api/pipelines",
            Some(json!({
                "name": "cycle",
                "steps": [
                    step("a", "alice", &["b"], "a"),
                    step("b", "alice", &["a"], "b"),
                ],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(parse(&text)["error"]["code"], "cycle_detected");

    let (status, text) = harness
        .request(
            Method::POST,
            "/api/pipelines",
            Some(json!({
                "name": "empty",
                "steps": [],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(parse(&text)["error"]["code"], "no_steps");

    let pipeline = harness
        .create_pipeline(json!({
            "name": "known",
            "steps": [step("one", "ghost", &[], "hello")],
        }))
        .await;
    let (status, text) = harness
        .request(
            Method::POST,
            &format!(
                "/api/pipelines/{}/run",
                pipeline["id"].as_str().expect("id")
            ),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(parse(&text)["error"]["code"], "unknown_instance");
}

#[tokio::test]
async fn create_update_delete_are_audited_and_validated() {
    let harness = Harness::new();
    harness.instance("alice");
    let pipeline = harness
        .create_pipeline(json!({
            "name": "audited",
            "trigger": "cron",
            "cron": "0 3 * * *",
            "steps": [step("one", "alice", &[], "hello")],
        }))
        .await;
    let id = pipeline["id"].as_str().expect("id");

    let (status, text) = harness
        .request(
            Method::PATCH,
            &format!("/api/pipelines/{id}"),
            Some(json!({ "enabled": false })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(parse(&text)["enabled"], false);

    let (status, text) = harness
        .request(
            Method::POST,
            &format!("/api/pipelines/{id}/run"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(parse(&text)["error"]["code"], "pipeline_disabled");

    let (status, text) = harness
        .request(Method::DELETE, &format!("/api/pipelines/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");

    let (status, text) = harness
        .request(Method::GET, "/api/audit?limit=10", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let actions: Vec<String> = parse(&text)
        .as_array()
        .expect("audit")
        .iter()
        .filter_map(|entry| entry["action"].as_str().map(str::to_string))
        .collect();
    for action in ["pipeline.delete", "pipeline.update", "pipeline.create"] {
        assert!(actions.contains(&action.to_string()), "{actions:?}");
    }
}
