//! Integration tests for reusable personas (issue #55).
//!
//! Everything runs against `MockDriver`; the personas themselves never touch
//! Docker, so a stopped fleet is enough to exercise the whole surface.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];

struct Harness {
    dir: tempfile::TempDir,
    state: AppState,
    mock: Arc<MockDriver>,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        Self { dir, state, mock }
    }

    fn personas_dir(&self) -> PathBuf {
        self.dir.path().join("personas")
    }

    fn soul(&self, id: &str) -> String {
        std::fs::read_to_string(
            self.dir
                .path()
                .join("instances")
                .join(id)
                .join("data/SOUL.md"),
        )
        .expect("rendered SOUL.md")
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
        let (status, _headers, body) = self.send(method, uri, body, token).await;
        (status, body)
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (StatusCode, HeaderMap, String) {
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
        let response = api::router(self.state.clone())
            .oneshot(builder.body(body).expect("valid request"))
            .await
            .expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        (status, headers, String::from_utf8_lossy(&bytes).to_string())
    }

    async fn create_instance(&self, id: &str) {
        self.create_instance_with(id, None).await;
    }

    async fn create_instance_with(&self, id: &str, persona: Option<Value>) {
        let mut body = json!({
            "id": id,
            "name": format!("Agent {id}"),
            "template": "guest",
            "model": MODEL,
            "base_url": BASE_URL,
            "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
            "shares": [],
            "mem_limit_mb": 1024,
        });
        if let Some(persona) = persona {
            body["persona"] = persona;
        }
        let (status, response) = self
            .request(Method::POST, "/api/instances", Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "create instance: {response}");
    }

    async fn create_persona(&self) -> Value {
        let (status, body) = self
            .request(Method::POST, "/api/personas", Some(house_persona()))
            .await;
        assert_eq!(status, StatusCode::CREATED, "create persona: {body}");
        parse(&body)
    }

    async fn persona(&self, id: &str) -> Value {
        let (status, body) = self
            .request(Method::GET, &format!("/api/personas/{id}"), None)
            .await;
        assert_eq!(status, StatusCode::OK, "persona detail: {body}");
        parse(&body)
    }

    async fn instance(&self, id: &str) -> Value {
        let (status, body) = self
            .request(Method::GET, &format!("/api/instances/{id}"), None)
            .await;
        assert_eq!(status, StatusCode::OK, "instance detail: {body}");
        parse(&body)
    }

    async fn audit(&self, action: &str) -> Vec<Value> {
        let (status, body) = self
            .request(
                Method::GET,
                &format!("/api/audit?limit=100&action={action}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "audit: {body}");
        parse(&body).as_array().expect("audit array").to_vec()
    }

    async fn viewer_token(&self) -> String {
        let (status, body) = self
            .request(
                Method::POST,
                "/api/tokens",
                Some(json!({"name": "read-only", "role": "viewer"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        parse(&body)["token"].as_str().expect("token").to_string()
    }
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

fn house_persona() -> Value {
    json!({
        "id": "house",
        "name": "House",
        "description": "The shared voice of the fleet",
        "tags": ["ops", "french"],
        "variables": [
            {"key": "tone", "description": "How direct the agent is", "default": "direct"},
            {"key": "language", "description": "Answer language", "default": "français"}
        ],
        "soul_md": "# Identité\n\nTu es {{agent_name}}.\n\
                    Ton ton est {{ tone }}.\nLangue : {{language}}.\n",
    })
}

const RENDERED_ALICE: &str =
    "# Identité\n\nTu es Agent alice.\nTon ton est direct.\nLangue : français.\n";

#[tokio::test]
async fn the_same_persona_renders_the_same_soul_on_two_instances() {
    let harness = Harness::new();
    harness.create_persona().await;
    // One instance gets the persona at create time, the other through a PATCH:
    // both paths must end with the same rendered file.
    harness
        .create_instance_with("alice", Some(json!("house")))
        .await;
    harness.create_instance("bob").await;

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/bob",
            Some(json!({"persona": "house"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(harness.soul("alice"), RENDERED_ALICE);
    assert_eq!(
        harness.soul("bob"),
        RENDERED_ALICE.replace("Agent alice", "Agent bob")
    );

    let detail = harness.persona("house").await;
    let instances = detail["instances"].as_array().expect("instances");
    assert_eq!(instances.len(), 2, "{detail}");
    assert_eq!(detail["instances_count"], 2);

    let alice = harness.instance("alice").await;
    assert_eq!(alice["persona"], "house");
    assert_eq!(alice["persona_state"], "applied");
    assert_eq!(alice["persona_vars"], json!({}));

    // The gallery carries the count and the excerpt.
    let (status, body) = harness.request(Method::GET, "/api/personas", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let gallery = parse(&body);
    let house = &gallery.as_array().expect("gallery")[0];
    assert_eq!(house["id"], "house");
    assert_eq!(house["instances_count"], 2);
    assert!(house["updated_at"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(house["excerpt"]
        .as_str()
        .expect("excerpt")
        .starts_with("Tu es"));
}

#[tokio::test]
async fn changing_a_variable_only_rewrites_that_value() {
    let harness = Harness::new();
    harness.create_persona().await;
    harness
        .create_instance_with("alice", Some(json!("house")))
        .await;
    harness
        .create_instance_with("bob", Some(json!("house")))
        .await;

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"persona_vars": {"tone": "warm"}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let alice = harness.soul("alice");
    assert_eq!(alice, RENDERED_ALICE.replace("direct", "warm"));
    // Only `tone` moved: every other byte of the template is intact.
    assert_eq!(alice.replace("warm", "direct"), RENDERED_ALICE);
    // The other instance keeps its own values.
    assert_eq!(harness.soul("bob"), RENDERED_ALICE.replace("alice", "bob"));

    let detail = harness.instance("alice").await;
    assert_eq!(detail["persona_vars"], json!({"tone": "warm"}));
    assert_eq!(detail["persona_state"], "applied");
}

#[tokio::test]
async fn a_manual_edit_diverges_and_reapplying_restores_the_persona() {
    let harness = Harness::new();
    harness.create_persona().await;
    harness
        .create_instance_with("alice", Some(json!("house")))
        .await;

    let soul = harness.dir.path().join("instances/alice/data/SOUL.md");
    std::fs::write(&soul, "# Hand written\n\nI do what I want.\n").expect("manual edit");

    let detail = harness.instance("alice").await;
    assert_eq!(detail["persona_state"], "diverged");

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/personas/house/apply",
            Some(json!({"instances": ["alice"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["results"][0]["status"], "applied");

    assert_eq!(harness.soul("alice"), RENDERED_ALICE);
    let detail = harness.instance("alice").await;
    assert_eq!(detail["persona_state"], "applied");
}

#[tokio::test]
async fn applying_to_two_instances_records_two_audit_rows() {
    let harness = Harness::new();
    harness.create_persona().await;
    harness.create_instance("alice").await;
    harness.create_instance("bob").await;

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/personas/house/apply",
            Some(json!({"instances": ["alice", "bob", "alice"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let results = parse(&body)["results"]
        .as_array()
        .cloned()
        .expect("results");
    assert_eq!(results.len(), 2, "duplicates are collapsed");
    assert!(results.iter().all(|result| result["status"] == "applied"));

    let rows = harness.audit("instance.persona.apply").await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let targets: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["target"].as_str())
        .collect();
    assert!(targets.contains(&"alice"));
    assert!(targets.contains(&"bob"));

    // The same values, rendered for two different agent names.
    assert_eq!(harness.soul("alice"), RENDERED_ALICE);
    assert_eq!(
        harness.soul("bob"),
        RENDERED_ALICE.replace("Agent alice", "Agent bob")
    );
    // Applying restarts the instance.
    assert!(harness
        .mock
        .calls()
        .iter()
        .any(|call| call.operation == "restart" && call.instance == "bob"));
}

#[tokio::test]
async fn a_persona_in_use_cannot_be_deleted() {
    let harness = Harness::new();
    harness.create_persona().await;
    harness
        .create_instance_with("alice", Some(json!("house")))
        .await;

    let (status, body) = harness
        .request(Method::DELETE, "/api/personas/house", None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = &parse(&body)["error"];
    assert_eq!(error["code"], "persona_in_use");
    assert_eq!(error["details"]["instances"], json!(["alice"]));

    // Detaching leaves SOUL.md untouched and frees the persona.
    let before = harness.soul("alice");
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"persona": null})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        harness.soul("alice"),
        before,
        "detach never rewrites SOUL.md"
    );
    let detail = harness.instance("alice").await;
    assert_eq!(detail["persona"], Value::Null);
    assert_eq!(detail["persona_state"], "none");

    let (status, body) = harness
        .request(Method::DELETE, "/api/personas/house", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, _) = harness
        .request(Method::GET, "/api/personas/house", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn preview_uses_defaults_instance_values_and_reports_missing_keys() {
    let harness = Harness::new();
    harness.create_persona().await;
    harness
        .create_instance_with("alice", Some(json!("house")))
        .await;

    // No instance: defaults + a missing `agent_name`.
    let (status, body) = harness
        .request(Method::POST, "/api/personas/house/preview", Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let preview = parse(&body);
    assert_eq!(preview["missing"], json!(["agent_name"]));
    assert!(preview["rendered"]
        .as_str()
        .expect("rendered")
        .contains("Tu es {{agent_name}}."));
    assert!(preview["rendered"]
        .as_str()
        .expect("rendered")
        .contains("Ton ton est direct."));

    // Instance: `agent_name` is filled from its name, values from its binding.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/personas/house/preview",
            Some(json!({"instance": "alice"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let preview = parse(&body);
    assert_eq!(preview["missing"], json!([]));
    assert_eq!(preview["rendered"], RENDERED_ALICE);

    // Explicit overrides win over the instance and the defaults.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/personas/house/preview",
            Some(json!({"instance": "alice", "variables": {"tone": "dry", "language": "english"}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        parse(&body)["rendered"],
        "# Identité\n\nTu es Agent alice.\nTon ton est dry.\nLangue : english.\n"
    );

    // A declared variable without a default and no value is reported.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/personas",
            Some(json!({
                "id": "strict",
                "name": "Strict",
                "variables": [{"key": "owner", "description": "Who owns the agent"}],
                "soul_md": "Owner: {{owner}}\n",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/personas/strict/preview",
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["missing"], json!(["owner"]));

    // Applying a template with a missing value is refused, per instance.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/personas/strict/apply",
            Some(json!({"instances": ["alice"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["results"][0]["status"], "failed");
}

#[tokio::test]
async fn persona_history_is_git_backed() {
    let harness = Harness::new();
    harness.create_persona().await;
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/personas/house",
            Some(json!({"name": "House v2"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/personas/house",
            Some(json!({"soul_md": "# Identité v3\n\nTu es {{agent_name}}.\n"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = harness
        .request(Method::GET, "/api/personas/house/git/log?limit=10", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let commits = parse(&body)["commits"]
        .as_array()
        .cloned()
        .expect("commits");
    assert_eq!(commits.len(), 3, "{commits:?}");
    assert!(commits
        .iter()
        .all(|commit| commit["message"] == "ui: persona house"));
    assert!(commits.iter().all(
        |commit| commit["paths"].as_array().is_some_and(|paths| paths
            .iter()
            .all(|path| path == "house/persona.yaml" || path == "house/SOUL.md"))
    ));
    let oldest = commits.last().expect("oldest")["id"].as_str().expect("id");
    let newest = commits.first().expect("newest")["id"].as_str().expect("id");

    let (status, body) = harness
        .request(
            Method::GET,
            &format!("/api/personas/house/git/diff?from={oldest}&to={newest}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let diff = parse(&body);
    assert_eq!(diff["from"], oldest);
    assert_eq!(diff["to"], newest);
    let text = diff["diff"].as_str().expect("diff");
    assert!(text.contains("--- a/house/SOUL.md"), "{text}");
    assert!(text.contains("+++ b/house/SOUL.md"), "{text}");
    assert!(text.contains("+name: House v2"), "{text}");

    // The repository never holds anything but the two whitelisted files.
    let root = harness.personas_dir();
    let repo = git2::Repository::open(&root).expect("personas repository");
    let head = repo.head().expect("HEAD").peel_to_commit().expect("commit");
    let tree = head.tree().expect("tree");
    let mut paths = Vec::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            paths.push(format!("{root}{}", entry.name().unwrap_or_default()));
        }
        git2::TreeWalkResult::Ok
    })
    .expect("walk");
    paths.sort();
    assert_eq!(
        paths,
        vec![
            "house/SOUL.md".to_string(),
            "house/persona.yaml".to_string()
        ]
    );
}

#[tokio::test]
async fn the_persona_repository_never_commits_stray_files_or_secrets() {
    let harness = Harness::new();
    harness.create_persona().await;
    let dir = harness.personas_dir().join("house");
    std::fs::write(dir.join("secrets.env"), "TELEGRAM_TOKEN=super-secret\n").expect("secret");
    std::fs::write(dir.join("notes.md"), "not part of a persona\n").expect("notes");
    std::fs::create_dir_all(harness.personas_dir().join("staging")).expect("stray dir");
    std::fs::write(harness.personas_dir().join("staging/blob"), "x").expect("stray file");

    // A save triggers a commit; only the whitelisted files may appear in it.
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/personas/house",
            Some(json!({"description": "still the same voice"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = harness
        .request(Method::GET, "/api/personas/house/git/log?limit=1", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let commits = parse(&body)["commits"]
        .as_array()
        .cloned()
        .expect("commits");
    let paths: Vec<String> = commits[0]["paths"]
        .as_array()
        .expect("paths")
        .iter()
        .filter_map(|path| path.as_str().map(str::to_string))
        .collect();
    assert_eq!(paths, vec!["house/persona.yaml".to_string()]);

    // Nothing else is reachable from HEAD: a secret dropped in the directory
    // and a stray top-level folder never reach the object database.
    let repo = git2::Repository::open(harness.personas_dir()).expect("repository");
    let head = repo.head().expect("HEAD").peel_to_commit().expect("commit");
    let tree = head.tree().expect("tree");
    let mut blobs = Vec::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            blobs.push(format!("{root}{}", entry.name().unwrap_or_default()));
        }
        git2::TreeWalkResult::Ok
    })
    .expect("walk");
    assert!(
        !blobs.iter().any(|path| path.contains("secrets.env")),
        "{blobs:?}"
    );
    assert!(
        !blobs.iter().any(|path| path.starts_with("staging/")),
        "{blobs:?}"
    );
    assert!(
        !blobs.iter().any(|path| path.contains("notes.md")),
        "{blobs:?}"
    );
}

#[tokio::test]
async fn viewers_cannot_mutate_personas() {
    let harness = Harness::new();
    harness.create_persona().await;
    let viewer = harness.viewer_token().await;

    // Reads stay available.
    let (status, body) = harness
        .request_as(Method::GET, "/api/personas", None, Some(&viewer))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for (method, uri, body) in [
        (
            Method::POST,
            "/api/personas".to_string(),
            Some(house_persona()),
        ),
        (
            Method::PATCH,
            "/api/personas/house".to_string(),
            Some(json!({"name": "Nope"})),
        ),
        (Method::DELETE, "/api/personas/house".to_string(), None),
        (
            Method::POST,
            "/api/personas/house/apply".to_string(),
            Some(json!({"instances": ["alice"]})),
        ),
    ] {
        let (status, body) = harness
            .request_as(method.clone(), &uri, body, Some(&viewer))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
        assert_eq!(parse(&body)["error"]["code"], "forbidden");
    }

    // The persona was not touched.
    let detail = harness.persona("house").await;
    assert_eq!(detail["name"], "House");
}

#[tokio::test]
async fn persona_validation_refuses_bad_ids_keys_and_oversized_templates() {
    let harness = Harness::new();
    for body in [
        json!({"id": "House", "name": "x", "soul_md": "x"}),
        json!({"id": "-house", "name": "x", "soul_md": "x"}),
        json!({
            "id": "house",
            "name": "x",
            "variables": [{"key": "Tone", "description": ""}],
            "soul_md": "x"
        }),
        json!({
            "id": "house",
            "name": "x",
            "variables": [
                {"key": "tone"},
                {"key": "tone"}
            ],
            "soul_md": "x"
        }),
        json!({"id": "house", "name": "x", "soul_md": "é".repeat(40_000)}),
    ] {
        let (status, response) = harness
            .request(Method::POST, "/api/personas", Some(body.clone()))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
        assert_eq!(parse(&response)["error"]["code"], "invalid_persona");
    }

    // A persona created, then the same id again: 409.
    harness.create_persona().await;
    let (status, body) = harness
        .request(Method::POST, "/api/personas", Some(house_persona()))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "persona_exists");
}

#[tokio::test]
async fn assigning_an_unknown_persona_refuses_the_instance() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "alice",
                "name": "Agent alice",
                "template": "guest",
                "model": MODEL,
                "base_url": BASE_URL,
                "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
                "persona": "ghost",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "not_found");

    harness.create_instance("alice").await;
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({"persona": "ghost"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_template_can_carry_the_default_persona() {
    let harness = Harness::new();
    harness.create_persona().await;
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/templates",
            Some(json!({
                "id": "house-template",
                "name": "House template",
                "parent": "guest",
                "persona": "house",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(parse(&body)["persona"], "house");

    // No `persona` in the create request: the template default applies.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "alice",
                "name": "Agent alice",
                "template": "house-template",
                "model": MODEL,
                "base_url": BASE_URL,
                "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = harness.instance("alice").await;
    assert_eq!(detail["persona"], "house");
    assert_eq!(detail["persona_state"], "applied");

    // An explicit `null` opts out.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "bob",
                "name": "Agent bob",
                "template": "house-template",
                "model": MODEL,
                "base_url": BASE_URL,
                "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
                "persona": null,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = harness.instance("bob").await;
    assert_eq!(detail["persona"], Value::Null);
    assert_eq!(detail["persona_state"], "none");
}
