//! Integration tests for the declarative GitOps layer (issue #34).
//!
//! Every test runs against `MockDriver` and a temporary data root; no Docker
//! call is ever made.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::gitops::{self, FleetDocument, Plan};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const TOKEN: &str = "test-token";

struct Harness {
    /// Keeps the temporary data root alive for the lifetime of the harness.
    _dir: tempfile::TempDir,
    state: AppState,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        let state = AppState::bootstrap(cfg, Arc::new(MockDriver::new())).expect("bootstrap state");
        Self { _dir: dir, state }
    }

    fn compose_path(&self, id: &str) -> PathBuf {
        self.state.cfg.compose_path(id)
    }

    async fn send(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, String) {
        let mut builder = Request::builder().method(method).uri(uri);
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        let body = match body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = api::router(self.state.clone())
            .oneshot(builder.body(body).expect("valid request"))
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
}

fn parse(text: &str) -> FleetDocument {
    FleetDocument::parse(text).unwrap_or_else(|err| panic!("document should parse: {err}"))
}

fn entry_ids(entries: &[fleett::gitops::PlanEntry]) -> Vec<String> {
    entries.iter().map(|entry| entry.id.clone()).collect()
}

fn fields(plan: &Plan) -> Vec<String> {
    plan.update
        .iter()
        .flat_map(|entry| entry.fields.iter().map(|field| field.field.clone()))
        .collect()
}

fn base_document() -> String {
    r#"
instances:
  - id: alice
    template: guest
    model: test-model
    mem_limit_mb: 1024
"#
    .to_string()
}

#[test]
fn parsing_rejects_unknown_fields_duplicates_and_ambiguous_env() {
    // Unknown keys are refused instead of silently ignored.
    assert!(FleetDocument::parse("instances:\n  - id: alice\n    wizard: true\n").is_err());
    assert!(FleetDocument::parse("notifications: []\n").is_err());

    // Duplicate ids.
    let err = FleetDocument::parse(
        "instances:\n  - {id: alice, template: guest, model: m}\n  - {id: alice, template: guest, model: m}\n",
    )
    .expect_err("duplicate id");
    assert!(err.to_string().contains("twice"), "{err}");

    // `value` and `secret_ref` are mutually exclusive, and one is required.
    assert!(FleetDocument::parse(
        "instances:\n  - id: alice\n    env:\n      - {key: TOKEN, value: x, secret_ref: 'env:TOKEN'}\n"
    )
    .is_err());
    assert!(
        FleetDocument::parse("instances:\n  - id: alice\n    env:\n      - {key: TOKEN}\n")
            .is_err()
    );

    // Vault references are documented as not implemented.
    let err = FleetDocument::parse(
        "instances:\n  - id: alice\n    env:\n      - {key: TOKEN, secret_ref: 'infisical:/infra/fleett/TOKEN'}\n",
    )
    .expect_err("infisical");
    assert!(err.to_string().contains("not implemented"), "{err}");

    // Invalid ids, providers and templates.
    assert!(FleetDocument::parse("instances:\n  - {id: ALICE}\n").is_err());
    assert!(FleetDocument::parse(
        "providers:\n  - {name: bad_name, kind: openai-compatible, base_url: 'https://x', secret_ref: 'env:X'}\n"
    )
    .is_err());
    assert!(FleetDocument::parse("instances:\n  - {id: alice, template: 'no spaces'}\n").is_err());

    // A valid document still parses.
    parse(&base_document());
}

#[tokio::test]
async fn apply_creates_the_instance_and_a_second_plan_is_empty() {
    let harness = Harness::new();
    let document = parse(&base_document());

    let plan = gitops::plan(&harness.state.cfg, &harness.state.db, &document, false).expect("plan");
    assert_eq!(entry_ids(&plan.create), vec!["alice".to_string()]);
    assert!(plan.update.is_empty() && plan.delete.is_empty() && plan.refused.is_empty());

    let report = gitops::apply(&harness.state, &document, &base_document(), "test", false)
        .await
        .expect("apply");
    assert_eq!(report.applied.len(), 1);
    assert_eq!(
        harness
            .state
            .db
            .get("alice")
            .expect("get")
            .expect("row")
            .model,
        "test-model"
    );
    assert!(harness.compose_path("alice").is_file());

    let plan = gitops::plan(&harness.state.cfg, &harness.state.db, &document, false).expect("plan");
    assert!(plan.is_empty(), "second plan should be empty: {plan:?}");

    let drift = gitops::drift(&harness.state.cfg, &harness.state.db).expect("drift");
    assert!(drift.found && drift.in_sync, "{drift:?}");
}

#[tokio::test]
async fn plan_reports_field_diffs_and_apply_updates_the_instance() {
    let harness = Harness::new();
    let first = parse(&base_document());
    gitops::apply(&harness.state, &first, &base_document(), "test", false)
        .await
        .expect("apply");

    let changed = base_document()
        .replace("model: test-model", "model: other-model")
        .replace(
            "mem_limit_mb: 1024",
            "mem_limit_mb: 2048\n    cpu_limit: 2.0",
        );
    let document = parse(&changed);
    let plan = gitops::plan(&harness.state.cfg, &harness.state.db, &document, false).expect("plan");
    let changed_fields = fields(&plan);
    assert_eq!(entry_ids(&plan.update), vec!["alice".to_string()]);
    assert!(
        changed_fields.contains(&"model".to_string()),
        "{changed_fields:?}"
    );
    assert!(
        changed_fields.contains(&"mem_limit_mb".to_string()),
        "{changed_fields:?}"
    );
    assert!(
        changed_fields.contains(&"cpu_limit".to_string()),
        "{changed_fields:?}"
    );

    gitops::apply(&harness.state, &document, &changed, "test", false)
        .await
        .expect("apply");
    let spec = harness.state.db.get("alice").expect("get").expect("row");
    assert_eq!(spec.model, "other-model");
    assert_eq!(spec.mem_limit_mb, 2048);
    assert!((spec.cpu_limit - 2.0).abs() < f64::EPSILON);

    let plan = gitops::plan(&harness.state.cfg, &harness.state.db, &document, false).expect("plan");
    assert!(plan.is_empty(), "{plan:?}");
}

#[tokio::test]
async fn deletion_is_refused_without_allow_delete() {
    let harness = Harness::new();
    let two = format!(
        "{}  - id: bob\n    template: guest\n    model: test-model\n",
        base_document()
    );
    let document = parse(&two);
    gitops::apply(&harness.state, &document, &two, "test", false)
        .await
        .expect("apply");

    let one = parse(&base_document());
    let plan = gitops::plan(&harness.state.cfg, &harness.state.db, &one, false).expect("plan");
    assert!(plan.delete.is_empty());
    assert_eq!(entry_ids(&plan.refused), vec!["bob".to_string()]);

    let report = gitops::apply(&harness.state, &one, &base_document(), "test", false)
        .await
        .expect("apply");
    assert!(report.applied.is_empty(), "{report:?}");
    assert!(
        harness.state.db.get("bob").expect("get").is_some(),
        "refused deletion must keep the instance"
    );

    // The drift baseline is the new document: bob is now visible as drift.
    let drift = gitops::drift(&harness.state.cfg, &harness.state.db).expect("drift");
    assert!(!drift.in_sync);
    assert_eq!(entry_ids(&drift.drift.refused), vec!["bob".to_string()]);
}

#[tokio::test]
async fn deletion_is_applied_with_allow_delete() {
    let harness = Harness::new();
    let two = format!(
        "{}  - id: bob\n    template: guest\n    model: test-model\n",
        base_document()
    );
    let document = parse(&two);
    gitops::apply(&harness.state, &document, &two, "test", false)
        .await
        .expect("apply");
    assert!(harness.compose_path("bob").is_file());

    let one = parse(&base_document());
    let report = gitops::apply(&harness.state, &one, &base_document(), "test", true)
        .await
        .expect("apply");
    assert_eq!(entry_ids(&report.applied), vec!["bob".to_string()]);
    assert!(harness.state.db.get("bob").expect("get").is_none());
    assert!(!harness.compose_path("bob").exists());
    assert!(harness.state.db.get("alice").expect("get").is_some());
}

#[tokio::test]
async fn a_manual_change_is_detected_as_drift() {
    let harness = Harness::new();
    let document = parse(&base_document());
    gitops::apply(&harness.state, &document, &base_document(), "test", false)
        .await
        .expect("apply");

    let mut spec = harness.state.db.get("alice").expect("get").expect("row");
    spec.model = "edited-by-hand".to_string();
    harness
        .state
        .db
        .update_instance(&spec)
        .expect("manual update");

    let drift = gitops::drift(&harness.state.cfg, &harness.state.db).expect("drift");
    assert!(drift.found && !drift.in_sync);
    assert_eq!(entry_ids(&drift.drift.update), vec!["alice".to_string()]);
    assert!(fields(&drift.drift).contains(&"model".to_string()));

    // Re-applying the document removes the drift.
    gitops::apply(&harness.state, &document, &base_document(), "test", false)
        .await
        .expect("apply");
    let drift = gitops::drift(&harness.state.cfg, &harness.state.db).expect("drift");
    assert!(drift.in_sync, "{drift:?}");
}

#[tokio::test]
async fn apply_stores_secrets_without_serializing_them_in_the_plan() {
    let harness = Harness::new();
    std::env::set_var("FLEETT_TEST_GITOPS_KEY_34", "provider-secret-value-34");
    std::env::set_var("FLEETT_TEST_GITOPS_BOT_34", "bot-secret-value-34");

    let text = r#"
providers:
  - name: shared
    kind: openai-compatible
    base_url: https://provider.test/v1
    api_key_env: PROVIDER_KEY
    secret_ref: env:FLEETT_TEST_GITOPS_KEY_34
instances:
  - id: alice
    template: guest
    model: test-model
    provider: shared
    env:
      - key: TELEGRAM_BOT_TOKEN
        secret_ref: env:FLEETT_TEST_GITOPS_BOT_34
"#;
    let document = parse(text);
    let plan = gitops::plan(&harness.state.cfg, &harness.state.db, &document, false).expect("plan");
    let serialized = serde_json::to_string(&plan).expect("plan json");
    assert!(
        !serialized.contains("provider-secret-value-34"),
        "the plan leaked a provider secret: {serialized}"
    );
    assert!(
        !serialized.contains("bot-secret-value-34"),
        "the plan leaked a bot secret: {serialized}"
    );
    let document_json = serde_json::to_string(&document).expect("document json");
    assert!(!document_json.contains("provider-secret-value-34"));
    assert!(!document_json.contains("bot-secret-value-34"));

    let report = gitops::apply(&harness.state, &document, text, "test", false)
        .await
        .expect("apply");
    assert_eq!(report.plan.create.len(), 2, "one provider and one instance");

    let provider = harness
        .state
        .db
        .list_providers()
        .expect("providers")
        .into_iter()
        .find(|provider| provider.name == "shared")
        .expect("provider created");
    assert_eq!(provider.base_url, "https://provider.test/v1");

    let spec = harness.state.db.get("alice").expect("get").expect("row");
    assert_eq!(spec.provider.as_deref(), Some(provider.id.as_str()));
    let compose = std::fs::read_to_string(harness.compose_path("alice")).expect("compose");
    assert!(
        compose.contains("PROVIDER_KEY"),
        "the provider key must be interpolated by the compose file: {compose}"
    );
    let env =
        fleett::envs::load(&harness.state.cfg, &harness.state.db, "alice").expect("env readable");
    let bot = env
        .iter()
        .find(|entry| entry.key == "TELEGRAM_BOT_TOKEN")
        .expect("bot token stored");
    assert!(bot.secret);
    assert_eq!(bot.value, "bot-secret-value-34");
}

#[tokio::test]
async fn api_routes_expose_plan_apply_and_drift() {
    let harness = Harness::new();
    let plan_body = json!({ "document": base_document() });

    let (status, body) = harness
        .send(
            Method::POST,
            "/api/desired-state/plan",
            Some(plan_body.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let plan = serde_json::from_str::<Plan>(&body).expect("plan json");
    assert_eq!(entry_ids(&plan.create), vec!["alice".to_string()]);

    let (status, body) = harness
        .send(Method::POST, "/api/desired-state/apply", Some(plan_body))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let report = serde_json::from_str::<fleett::gitops::ApplyReport>(&body).expect("report");
    assert_eq!(report.applied.len(), 1);
    assert_eq!(report.source, "api");

    let (status, body) = harness.send(Method::GET, "/api/desired-state", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let view = serde_json::from_str::<fleett::gitops::DesiredStateView>(&body).expect("view");
    assert!(view.in_sync);
    assert_eq!(view.source.as_deref(), Some("api"));
    assert!(view.document.is_some());

    // An unmanaged instance created outside the file is drift.
    harness
        .state
        .db
        .insert_instance(&fleett::models::InstanceSpec {
            id: "intruder".to_string(),
            name: "intruder".to_string(),
            template: "guest".to_string(),
            model: "test-model".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: "HERMES_GATE_PROFILE_TOKEN".to_string(),
            provider: None,
            channel: fleett::models::ChannelKind::None,
            shares: Vec::new(),
            skills: Vec::new(),
            mem_limit_mb: 512,
            cpu_limit: 1.0,
            image: "image:1".to_string(),
            previous_image: None,
            allow_lan: false,
            host: "local".to_string(),
            heal: fleett::models::HealPolicy::Off,
            unhealthy_after: fleett::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: fleett::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: fleett::models::DEFAULT_HEAL_BACKOFF_S,
            disk_quota_mb: 0,
            disk_policy: fleett::models::DiskPolicy::Warn,
            notes: String::new(),
            status: fleett::models::Status::Created,
            last_error: None,
            created_at: fleett::models::now_iso(),
            updated_at: fleett::models::now_iso(),
        })
        .expect("insert intruder");

    let (status, body) = harness
        .send(Method::GET, "/api/desired-state/drift", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let drift = serde_json::from_str::<fleett::gitops::DriftReport>(&body).expect("drift");
    assert!(!drift.in_sync);
    assert_eq!(
        entry_ids(&drift.drift.refused),
        vec!["intruder".to_string()]
    );
}

#[tokio::test]
async fn a_provider_still_in_use_blocks_its_deletion() {
    let harness = Harness::new();
    std::env::set_var("FLEETT_TEST_GITOPS_PROVIDER_34", "provider-secret-34");
    let text = r#"
providers:
  - name: shared
    kind: openai-compatible
    base_url: https://provider.test/v1
    secret_ref: env:FLEETT_TEST_GITOPS_PROVIDER_34
instances:
  - id: alice
    template: guest
    model: test-model
    provider: shared
"#;
    let document = parse(text);
    gitops::apply(&harness.state, &document, text, "test", false)
        .await
        .expect("apply");

    // Remove the provider but keep the instance: the apply must refuse rather
    // than leave a dangling reference.
    let orphan = "instances:\n  - id: alice\n    template: guest\n    model: test-model\n    provider: shared\n";
    let document = FleetDocument::parse(orphan).expect("parse");
    let err = gitops::apply(&harness.state, &document, orphan, "test", true)
        .await
        .expect_err("provider in use");
    assert!(err.to_string().contains("still used"), "{err}");
    assert_eq!(
        harness.state.db.list_providers().expect("providers").len(),
        1
    );

    // Removing both at once is accepted.
    let gone = "instances: []\n";
    let document = FleetDocument::parse(gone).expect("parse");
    gitops::apply(&harness.state, &document, gone, "test", true)
        .await
        .expect("apply");
    assert!(harness
        .state
        .db
        .list_providers()
        .expect("providers")
        .is_empty());
    assert!(harness.state.db.get("alice").expect("get").is_none());
}
