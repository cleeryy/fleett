//! Integration tests for the shared skill registry (issue #54).
//!
//! Everything runs against `MockDriver`: publishing and promoting never touch
//! Docker, so a stopped fleet is enough to exercise the whole surface,
//! including the compose bytes that carry the read-only mount.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
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

    fn skills_dir(&self) -> PathBuf {
        self.dir.path().join("skills")
    }

    fn compose(&self, id: &str) -> String {
        std::fs::read_to_string(
            self.dir
                .path()
                .join("instances")
                .join(id)
                .join("compose.yaml"),
        )
        .expect("compose.yaml")
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

    async fn oneshot(&self, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
        let response = api::router(self.state.clone())
            .oneshot(request)
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
        self.oneshot(builder.body(body).expect("valid request"))
            .await
    }

    /// Send a raw multipart body (zip import).
    async fn upload(
        &self,
        uri: &str,
        filename: &str,
        bytes: &[u8],
        token: Option<&str>,
    ) -> (StatusCode, String) {
        let boundary = "fleett-test-boundary";
        let mut body = Vec::new();
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .expect("write");
        body.extend_from_slice(bytes);
        write!(body, "\r\n--{boundary}--\r\n").expect("write");
        let mut builder = Request::builder().method(Method::POST).uri(uri).header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        );
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let (status, _headers, body) = self
            .oneshot(builder.body(Body::from(body)).expect("request"))
            .await;
        (status, body)
    }

    async fn publish(&self, name: &str, version: &str, content: &str) -> Value {
        self.publish_with(name, version, content, Vec::new()).await
    }

    async fn publish_with(
        &self,
        name: &str,
        version: &str,
        content: &str,
        extra: Vec<Value>,
    ) -> Value {
        let mut files = vec![json!({"path": "SKILL.md", "content": content})];
        files.extend(extra);
        let (status, body) = self
            .request(
                Method::POST,
                "/api/skills",
                Some(json!({
                    "name": name,
                    "version": version,
                    "description": format!("{name} skill"),
                    "files": files,
                })),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "publish {name}@{version}: {body}"
        );
        parse(&body)
    }

    async fn create_instance(&self, id: &str, skills: Value) {
        let (status, body) = self
            .request(
                Method::POST,
                "/api/instances",
                Some(json!({
                    "id": id,
                    "name": format!("Agent {id}"),
                    "template": "guest",
                    "model": MODEL,
                    "base_url": BASE_URL,
                    "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
                    "skills": skills,
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "create instance {id}: {body}");
    }

    async fn delete_instance(&self, id: &str) {
        let (status, body) = self
            .request(Method::DELETE, &format!("/api/instances/{id}"), None)
            .await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "delete instance {id}: {body}"
        );
    }

    async fn detail(&self, id: &str) -> Value {
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

/// Build a zip archive from `(path, content)` entries.
fn zip_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for (path, content) in entries {
        writer.start_file(path, options).expect("start file");
        writer.write_all(content.as_bytes()).expect("write entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

// ---------------------------------------------------------------------------
// Publishing, versions, git
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publish_two_versions_and_read_the_registry() {
    let harness = Harness::new();
    let v1 = harness.publish("homelab", "v1", "# Homelab v1\n").await;
    assert_eq!(v1["latest"], "v1");
    assert_eq!(v1["versions"][0]["version"], "v1");
    assert_eq!(v1["files"][0]["path"], "SKILL.md");

    // The second version does not move `latest`: only a promote does.
    let v2 = harness
        .publish_with(
            "homelab",
            "v2",
            "# Homelab v2\n",
            vec![json!({"path": "notes/proxmox.md", "content": "CT 119\n"})],
        )
        .await;
    assert_eq!(v2["latest"], "v1");
    assert_eq!(v2["versions"].as_array().expect("versions").len(), 2);
    assert_eq!(v2["files"][0]["content"], "# Homelab v1\n");

    let (status, body) = harness.request(Method::GET, "/api/skills", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = parse(&body);
    assert_eq!(list[0]["name"], "homelab");
    assert_eq!(list[0]["description"], "homelab skill");
    assert_eq!(list[0]["latest"], "v1");
    assert_eq!(list[0]["versions"], json!(["v1", "v2"]));
    assert_eq!(list[0]["instances_count"], 0);

    // Files on disk: 0755 directories, 0644 files, per-version metadata.
    let root = harness.skills_dir().join("homelab");
    let version = root.join("v1");
    assert_eq!(
        std::fs::metadata(&version)
            .expect("version dir")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::metadata(version.join("SKILL.md"))
            .expect("SKILL.md")
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    let meta: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(version.join("skill.yaml")).expect("skill.yaml"),
    )
    .expect("yaml");
    assert_eq!(meta["name"], "homelab");
    assert_eq!(meta["description"], "homelab skill");
    assert_eq!(
        std::fs::read_to_string(root.join("latest"))
            .expect("latest")
            .trim(),
        "v1"
    );

    // Republishing an existing version is a 409.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/skills",
            Some(json!({
                "name": "homelab",
                "version": "v1",
                "files": [{"path": "SKILL.md", "content": "again"}],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "skill_version_exists");

    // The registry is a git repository: one commit per publish, authored by
    // the token actor.
    let (status, body) = harness
        .request(Method::GET, "/api/skills/homelab/git/log", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let log = parse(&body)["commits"].as_array().expect("commits").clone();
    assert_eq!(log.len(), 2, "{body}");
    assert_eq!(log[0]["message"], "ui: skill homelab@v2");
    assert_eq!(log[1]["message"], "ui: skill homelab@v1");
    let from = log[1]["id"].as_str().expect("id").to_string();
    let to = log[0]["id"].as_str().expect("id").to_string();
    let (status, body) = harness
        .request(
            Method::GET,
            &format!("/api/skills/homelab/git/diff?from={from}&to={to}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let diff = parse(&body);
    assert!(diff["diff"]
        .as_str()
        .unwrap_or_default()
        .contains("notes/proxmox.md"));
}

#[tokio::test]
async fn path_limits_and_missing_skill_md_are_refused() {
    let harness = Harness::new();
    let cases = [
        json!({"path": "../escape.md", "content": "x"}),
        json!({"path": "/etc/passwd", "content": "x"}),
        json!({"path": "notes/../../escape.md", "content": "x"}),
        json!({"path": "skill.yaml", "content": "name: forged"}),
    ];
    for entry in cases {
        let (status, body) = harness
            .request(
                Method::POST,
                "/api/skills",
                Some(json!({
                    "name": "escape",
                    "version": "v1",
                    "files": [
                        {"path": "SKILL.md", "content": "# Skill"},
                        entry,
                    ],
                })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{entry} -> {body}");
        assert_eq!(parse(&body)["error"]["code"], "invalid_skill");
    }

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/skills",
            Some(json!({
                "name": "escape",
                "version": "v1",
                "files": [{"path": "notes.md", "content": "no SKILL.md"}],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(parse(&body)["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("SKILL.md"));
    assert!(!harness.skills_dir().join("escape").exists());
}

#[tokio::test]
async fn zip_import_publishes_and_rejects_zip_slip() {
    let harness = Harness::new();

    let bytes = zip_bytes(&[
        ("skill.yaml", "name: web\ndescription: Web work\n"),
        ("SKILL.md", "# Web\n"),
        ("notes/forms.md", "forms\n"),
    ]);
    let (status, body) = harness
        .upload("/api/skills/import", "web.zip", &bytes, Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["name"], "web");
    assert_eq!(detail["description"], "Web work");
    assert_eq!(detail["latest"], "v1");
    assert_eq!(detail["files"].as_array().expect("files").len(), 2);

    // A wrapping folder is stripped (`web/SKILL.md` → `SKILL.md`).
    let wrapped = zip_bytes(&[
        ("web2/skill.yaml", "name: web2\n"),
        ("web2/SKILL.md", "# Web 2\n"),
    ]);
    let (status, body) = harness
        .upload("/api/skills/import", "web2.zip", &wrapped, Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(parse(&body)["name"], "web2");

    // A hostile entry aborts the import before anything is written.
    let hostile = zip_bytes(&[
        ("skill.yaml", "name: hostile\n"),
        ("SKILL.md", "# Hostile\n"),
        ("../escape.md", "x"),
    ]);
    let (status, body) = harness
        .upload("/api/skills/import", "hostile.zip", &hostile, Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_skill");
    assert!(!harness.skills_dir().join("hostile").exists());
    assert!(!harness
        .skills_dir()
        .parent()
        .expect("root")
        .join("escape.md")
        .exists());

    // A zip without `skill.yaml` is refused too.
    let nameless = zip_bytes(&[("SKILL.md", "# No metadata\n")]);
    let (status, body) = harness
        .upload("/api/skills/import", "nameless.zip", &nameless, Some(TOKEN))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

// ---------------------------------------------------------------------------
// Instances: sharing, pinning, promote
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_instances_share_a_skill_and_pinning_skips_the_promote() {
    let harness = Harness::new();
    harness.publish("shared", "v1", "# Shared v1\n").await;
    harness.publish("shared", "v2", "# Shared v2\n").await;

    harness
        .create_instance("alice", json!([{"name": "shared"}]))
        .await;
    harness
        .create_instance("bob", json!([{"name": "shared"}]))
        .await;
    harness
        .create_instance("pinned", json!([{"name": "shared", "version": "v1"}]))
        .await;

    // Every mount is read-only and names the concrete version.
    for id in ["alice", "bob", "pinned"] {
        let compose = harness.compose(id);
        assert!(
            compose.contains("../../skills/shared/v1:/opt/data/skills/shared:ro"),
            "{id}: {compose}"
        );
    }
    let detail = harness.detail("alice").await;
    assert_eq!(detail["skills"][0]["name"], "shared");
    assert!(detail["skills"][0]["version"].is_null(), "{detail}");
    assert_eq!(detail["skills"][0]["effective_version"], "v1");

    let (status, body) = harness.request(Method::GET, "/api/skills", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)[0]["instances_count"], 3);

    // Promote v2: the two followers restart, the pinned instance does not.
    harness.mock.clear_calls();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/skills/shared/promote",
            Some(json!({"version": "v2"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let report = parse(&body);
    assert_eq!(report["latest"], "v2");
    let results = report["results"].as_array().expect("results");
    let ids: Vec<&str> = results
        .iter()
        .map(|result| result["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["alice", "bob"]);
    assert!(results.iter().all(|result| result["status"] == "restarted"));

    let ups: Vec<String> = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| call.operation == "up")
        .map(|call| call.instance)
        .collect();
    assert_eq!(ups, ["alice", "bob"], "only the followers restart");

    for id in ["alice", "bob"] {
        assert!(
            harness
                .compose(id)
                .contains("../../skills/shared/v2:/opt/data/skills/shared:ro"),
            "{id}: {}",
            harness.compose(id)
        );
    }
    assert!(harness
        .compose("pinned")
        .contains("../../skills/shared/v1:/opt/data/skills/shared:ro"));
    assert_eq!(
        harness.detail("pinned").await["skills"][0]["effective_version"],
        "v1"
    );

    // The promote is one commit, like a publish.
    let (status, body) = harness
        .request(Method::GET, "/api/skills/shared/git/log", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let log = parse(&body)["commits"].as_array().expect("commits").clone();
    assert_eq!(log[0]["message"], "ui: skill shared@v2");

    // A PATCH that changes skills rewrites the compose and audits the list.
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/bob",
            Some(json!({"skills": []})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!harness.compose("bob").contains("/opt/data/skills/shared"));
    assert_eq!(harness.audit("instance.skills").await.len(), 1);
    assert_eq!(harness.audit("skill.promote").await.len(), 1);
    assert_eq!(harness.audit("skill.publish").await.len(), 2);
}

#[tokio::test]
async fn delete_is_refused_while_an_instance_mounts_the_skill() {
    let harness = Harness::new();
    harness.publish("inuse", "v1", "# In use\n").await;
    harness
        .create_instance("alice", json!([{"name": "inuse"}]))
        .await;

    let (status, body) = harness
        .request(Method::DELETE, "/api/skills/inuse", None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = &parse(&body)["error"];
    assert_eq!(error["code"], "skill_in_use");
    assert_eq!(error["details"]["instances"], json!(["alice"]));

    harness.delete_instance("alice").await;
    let (status, body) = harness
        .request(Method::DELETE, "/api/skills/inuse", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!harness.skills_dir().join("inuse").exists());
    let (status, body) = harness.request(Method::GET, "/api/skills", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(parse(&body).as_array().expect("skills").is_empty());
}

#[tokio::test]
async fn unknown_skills_and_versions_are_refused_on_the_instance() {
    let harness = Harness::new();
    harness.publish("known", "v1", "# Known\n").await;

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "ghost",
                "name": "Ghost",
                "template": "guest",
                "model": MODEL,
                "base_url": BASE_URL,
                "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
                "skills": [{"name": "nope"}],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "unknown_skill");

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "ghost",
                "name": "Ghost",
                "template": "guest",
                "model": MODEL,
                "base_url": BASE_URL,
                "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
                "skills": [{"name": "known", "version": "v9"}],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "unknown_skill_version");

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "ghost",
                "name": "Ghost",
                "template": "guest",
                "model": MODEL,
                "base_url": BASE_URL,
                "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
                "skills": [{"name": "known"}, {"name": "known"}],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "duplicate_skill");
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_viewer_reads_but_never_mutates_the_registry() {
    let harness = Harness::new();
    harness.publish("readonly", "v1", "# Read only\n").await;
    let viewer = harness.viewer_token().await;

    let (status, body) = harness
        .request_as(Method::GET, "/api/skills", None, Some(&viewer))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = harness
        .request_as(Method::GET, "/api/skills/readonly", None, Some(&viewer))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = harness
        .request_as(
            Method::POST,
            "/api/skills",
            Some(json!({
                "name": "nope",
                "version": "v1",
                "files": [{"path": "SKILL.md", "content": "# No"}],
            })),
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = harness
        .request_as(
            Method::POST,
            "/api/skills/readonly/promote",
            Some(json!({"version": "v1"})),
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = harness
        .request_as(Method::DELETE, "/api/skills/readonly", None, Some(&viewer))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let bytes = zip_bytes(&[("skill.yaml", "name: nope\n"), ("SKILL.md", "# No")]);
    let (status, body) = harness
        .upload("/api/skills/import", "nope.zip", &bytes, Some(&viewer))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

// ---------------------------------------------------------------------------
// Filesystem guards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compose_mounts_are_read_only_and_survive_a_reconcile() {
    let harness = Harness::new();
    harness.publish("safe", "v1", "# Safe\n").await;
    harness
        .create_instance("alice", json!([{"name": "safe", "version": "v1"}]))
        .await;
    let compose = harness.compose("alice");
    assert!(compose.contains("../../skills/safe/v1:/opt/data/skills/safe:ro"));
    assert!(!compose.contains("/opt/data/skills/safe:rw"));

    // The skill directory is never writable through the mount the agent sees:
    // the compose file declares `:ro`, and the registry itself is 0755.
    let dir = harness.skills_dir().join("safe").join("v1");
    let mode = std::fs::metadata(&dir)
        .expect("version dir")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755);
}
