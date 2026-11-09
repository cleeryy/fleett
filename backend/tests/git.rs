//! Integration tests for the per-instance git history.
//!
//! Everything runs against `MockDriver`: the history itself never talks to
//! Docker, which is exactly what the stopped-instance test pins down.

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
/// Secret handed to the instance at create time; must never reach the repo.
const CHANNEL_TOKEN: &str = "123456:SUPER-SECRET-BOT-TOKEN";

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

    fn instance_dir(&self, id: &str) -> PathBuf {
        self.dir.path().join("instances").join(id)
    }

    fn data_dir(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("data")
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        self.request_as(method, uri, body, TOKEN).await
    }

    async fn request_as(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: &str,
    ) -> (StatusCode, String) {
        let (status, _headers, body) = self.send(method, uri, body, Some(token)).await;
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
        let request = builder.body(body).expect("valid request");
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
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

fn instance_body(id: &str) -> Value {
    json!({
        "id": id,
        "name": format!("Agent {id}"),
        "template": "guest",
        "model": MODEL,
        "base_url": BASE_URL,
        "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
        "channel": {"type": "telegram", "token": CHANNEL_TOKEN},
        "shares": [],
        "mem_limit_mb": 1024,
        "notes": "git history test"
    })
}

async fn create_instance(harness: &Harness, id: &str) {
    let (status, body) = harness
        .request(Method::POST, "/api/instances", Some(instance_body(id)))
        .await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {body}");
}

async fn fetch_file(harness: &Harness, id: &str, path: &str) -> Value {
    let uri = format!("/api/instances/{id}/files/content?path={path}");
    let (status, body) = harness.request(Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    parse(&body)
}

/// Current etag of a file, or the empty etag of a file that does not exist yet.
async fn etag_of(harness: &Harness, id: &str, path: &str) -> String {
    let uri = format!("/api/instances/{id}/files/content?path={path}");
    match harness.request(Method::GET, &uri, None).await {
        (StatusCode::OK, body) => parse(&body)["etag"].as_str().expect("etag").to_string(),
        _ => String::new(),
    }
}

async fn put_file(harness: &Harness, id: &str, path: &str, content: &str) -> Value {
    let etag = etag_of(harness, id, path).await;
    let (status, body) = harness
        .request(
            Method::PUT,
            &format!("/api/instances/{id}/files/content"),
            Some(json!({"path": path, "content": content, "etag": etag})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    parse(&body)
}

async fn log(harness: &Harness, id: &str, query: &str) -> Value {
    let (status, body) = harness
        .request(
            Method::GET,
            &format!("/api/instances/{id}/git/log{query}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    parse(&body)
}

/// Number of `.bak-*` copies in the instance data directory.
fn count_backups(harness: &Harness, id: &str) -> usize {
    std::fs::read_dir(harness.data_dir(id))
        .expect("data")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".bak-"))
        .count()
}

#[tokio::test]
async fn two_saves_are_two_commits_attributed_to_the_actor() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    put_file(&harness, "alice", "/SOUL.md", "first version\n").await;
    put_file(&harness, "alice", "/SOUL.md", "second version\n").await;

    let body = log(&harness, "alice", "?path=/SOUL.md").await;
    let commits = body["commits"].as_array().expect("commits");
    // Newest first: the two saves, then the snapshot written at create time.
    assert_eq!(commits.len(), 3, "{body}");
    for commit in &commits[..2] {
        assert_eq!(commit["message"], "ui: data/SOUL.md");
        // The bootstrap bearer token owns the request.
        assert_eq!(commit["author"], "bootstrap");
        assert_eq!(commit["email"], "bootstrap@fleett.local");
        assert_eq!(commit["short"].as_str().expect("short").len(), 7);
        assert!(commit["at"].as_str().expect("at").ends_with('Z'));
        assert_eq!(commit["paths"], json!(["data/SOUL.md"]));
    }
    assert_ne!(commits[0]["id"], commits[1]["id"]);
    assert_eq!(commits[2]["message"], "sync: data/SOUL.md (+1 more)");
    assert_eq!(commits[2]["author"], "fleett");
}

#[tokio::test]
async fn a_change_outside_the_api_becomes_a_sync_commit() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    std::fs::write(
        harness.data_dir("alice").join("SOUL.md"),
        "edited over ssh\n",
    )
    .expect("external edit");

    // The reconcile hook used at startup and after a compose apply.
    fleett::history::reconcile(&harness.state.cfg, "alice").expect("reconcile");

    let body = log(&harness, "alice", "?limit=1").await;
    let commit = &body["commits"][0];
    assert_eq!(commit["message"], "sync: data/SOUL.md");
    assert_eq!(commit["author"], "fleett");
    assert_eq!(commit["email"], "fleett@localhost");

    // The same hook runs when the control plane starts again.
    std::fs::write(
        harness.data_dir("alice").join("SOUL.md"),
        "edited while fleett was down\n",
    )
    .expect("external edit");
    let cfg = (*harness.state.cfg).clone();
    let _restarted = AppState::bootstrap(cfg, Arc::new(MockDriver::new())).expect("bootstrap");

    let body = log(&harness, "alice", "?limit=1").await;
    let commit = &body["commits"][0];
    assert_eq!(commit["message"], "sync: data/SOUL.md");
    assert_eq!(commit["author"], "fleett");
}

#[tokio::test]
async fn log_diff_and_status_describe_the_work_tree() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    // Nothing changed since the snapshot written at create time.
    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/git/status", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let status = parse(&body);
    assert_eq!(status["modified"], json!([]));
    assert_eq!(status["untracked"], json!([]));

    put_file(&harness, "alice", "/SOUL.md", "a new soul\n").await;

    // The last commit is the save; its diff carries the changed line.
    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/git/diff", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let diff = parse(&body);
    assert_eq!(diff["to"].as_str().expect("to").len(), 40);
    assert_eq!(diff["from"].as_str().expect("from").len(), 40);
    let patch = diff["diff"].as_str().expect("diff");
    assert!(patch.starts_with("diff --git "), "{patch}");
    assert!(patch.contains("+a new soul"), "{patch}");
    assert!(patch.contains("--- a/data/SOUL.md"), "{patch}");

    let (status, body) = harness
        .request(
            Method::GET,
            &format!(
                "/api/instances/alice/git/diff?from={}&to={}",
                diff["from"].as_str().expect("from"),
                diff["to"].as_str().expect("to")
            ),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["diff"], diff["diff"]);

    // Drift is reported against the whitelist only.
    std::fs::write(harness.data_dir("alice").join("SOUL.md"), "drifted\n").expect("drift");
    std::fs::write(
        harness
            .data_dir("alice")
            .join("SOUL.md.bak-20261109-120000"),
        "backup\n",
    )
    .expect("backup");
    std::fs::write(harness.data_dir("alice").join("notes.md"), "notes\n").expect("notes");
    std::fs::write(
        harness.data_dir("alice").join("healthcheck.py"),
        "# probe\n",
    )
    .expect("healthcheck");

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/git/status", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let status = parse(&body);
    assert_eq!(status["modified"], json!(["data/SOUL.md"]));
    assert_eq!(status["untracked"], json!(["data/notes.md"]));

    let (status, body) = harness
        .request(
            Method::GET,
            "/api/instances/alice/git/log?path=/notes.md",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["commits"], json!([]), "not committed yet");

    let (status, body) = harness
        .request(
            Method::GET,
            "/api/instances/alice/git/log?path=/secrets.env",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "untracked_path");

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/git/log?limit=0", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn revert_restores_the_content_and_creates_an_audit_row() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    put_file(&harness, "alice", "/SOUL.md", "good soul\n").await;
    put_file(&harness, "alice", "/SOUL.md", "bad soul\n").await;

    let history = log(&harness, "alice", "?path=/SOUL.md").await;
    let good = history["commits"][1]["id"]
        .as_str()
        .expect("id")
        .to_string();
    let bad = history["commits"][0]["id"]
        .as_str()
        .expect("id")
        .to_string();
    assert_eq!(history["commits"][0]["message"], "ui: data/SOUL.md");
    let backups_before = count_backups(&harness, "alice");

    harness.mock.clear_calls();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/git/revert",
            Some(json!({"commit": good, "paths": ["/SOUL.md"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response = parse(&body);
    assert_eq!(response["paths"], json!(["data/SOUL.md"]));
    assert_eq!(response["restarted"], false);
    assert_eq!(response["status"], "running");
    assert_eq!(response["source"], good);
    assert_eq!(response["commit"]["author"], "bootstrap");
    assert!(response["commit"]["message"]
        .as_str()
        .expect("message")
        .starts_with("ui: "));
    assert!(
        harness.mock.calls().is_empty(),
        "a revert without restart never touches Docker"
    );

    let current = fetch_file(&harness, "alice", "/SOUL.md").await;
    assert_eq!(current["content"], "good soul\n");
    assert_eq!(
        count_backups(&harness, "alice"),
        backups_before + 1,
        "the replaced bytes are kept"
    );

    // The audit trail names the actor, the source commit and the paths.
    let (status, body) = harness
        .request(Method::GET, "/api/audit?limit=50", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = parse(&body);
    let entry = entries
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["action"] == "instance.git.revert")
        .expect("instance.git.revert audit row");
    assert_eq!(entry["target"], "alice");
    assert_eq!(entry["actor"], "bootstrap");
    assert_eq!(entry["details"]["source"], good);
    assert_eq!(entry["details"]["paths"], json!(["data/SOUL.md"]));

    // A revert that asks for a restart restarts the instance.
    harness.mock.clear_calls();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/git/revert",
            Some(json!({"commit": bad, "restart": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response = parse(&body);
    assert_eq!(response["restarted"], true);
    assert_eq!(response["status"], "running");
    // Without `paths`, the whole commit (its changed files) is restored.
    assert_eq!(response["paths"], json!(["data/SOUL.md"]));
    let calls = harness.mock.calls();
    let restarts: Vec<&str> = calls
        .iter()
        .filter(|call| call.operation == "restart")
        .map(|call| call.instance.as_str())
        .collect();
    assert_eq!(restarts, ["alice"]);
    let current = fetch_file(&harness, "alice", "/SOUL.md").await;
    assert_eq!(current["content"], "bad soul\n");
}

#[tokio::test]
async fn revert_can_be_limited_to_a_path_deleted_since_the_commit() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/files/mkdir",
            Some(json!({"path": "/notes"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    put_file(&harness, "alice", "/notes/plan.md", "plan v1\n").await;
    let first = log(&harness, "alice", "?path=/notes/plan.md").await["commits"][0]["id"]
        .as_str()
        .expect("id")
        .to_string();

    let (status, _) = harness
        .request(
            Method::DELETE,
            "/api/instances/alice/files/content?path=/notes/plan.md",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!harness.data_dir("alice").join("notes/plan.md").exists());

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/git/revert",
            Some(json!({"commit": first, "paths": ["/notes/plan.md"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        std::fs::read_to_string(harness.data_dir("alice").join("notes/plan.md"))
            .expect("restored file"),
        "plan v1\n"
    );

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/git/revert",
            Some(json!({"commit": "does-not-exist"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "not_found");
}

#[tokio::test]
async fn history_is_readable_while_the_instance_is_stopped() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    put_file(&harness, "alice", "/SOUL.md", "still saved\n").await;

    let (status, body) = harness
        .request(Method::POST, "/api/instances/alice/stop", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["status"], "stopped");

    let body = log(&harness, "alice", "?path=/SOUL.md").await;
    assert_eq!(body["commits"][0]["message"], "ui: data/SOUL.md");

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/git/diff", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(parse(&body)["diff"]
        .as_str()
        .expect("diff")
        .contains("+still saved"));

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/git/status", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["modified"], json!([]));

    // The revert works on a stopped instance too: no Docker involved.
    harness.mock.clear_calls();
    let first = log(&harness, "alice", "?limit=1").await["commits"][0]["id"]
        .as_str()
        .expect("id")
        .to_string();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/alice/git/revert",
            Some(json!({"commit": first})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["status"], "stopped");
    assert!(harness.mock.calls().is_empty());
}

#[tokio::test]
async fn the_repository_carries_no_secret_and_no_runtime_file() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;

    // Runtime files and the legacy plaintext secrets file, all outside the
    // whitelist.
    std::fs::write(
        harness.instance_dir("alice").join("secrets.env"),
        format!("TELEGRAM_BOT_TOKEN={CHANNEL_TOKEN}\n"),
    )
    .expect("legacy secret");
    std::fs::write(
        harness
            .data_dir("alice")
            .join("SOUL.md.bak-20261109-000000"),
        format!("old {CHANNEL_TOKEN}\n"),
    )
    .expect("backup");
    std::fs::write(
        harness.data_dir("alice").join("state.db"),
        format!("{CHANNEL_TOKEN}\n"),
    )
    .expect("database");

    fleett::history::reconcile(&harness.state.cfg, "alice").expect("reconcile");

    let (status, body) = harness
        .request(Method::GET, "/api/instances/alice/git/status", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let status = parse(&body);
    assert_eq!(status["modified"], json!([]));
    assert_eq!(status["untracked"], json!([]), "only the whitelist is seen");

    let body = log(&harness, "alice", "?limit=100").await;
    for commit in body["commits"].as_array().expect("commits") {
        for path in commit["paths"].as_array().expect("paths") {
            let path = path.as_str().expect("path");
            assert!(path.starts_with("data/"), "{path}");
            assert!(!path.contains(".bak-"), "{path}");
            assert!(!path.ends_with(".db"), "{path}");
        }
    }

    // No object of the repository — reachable or not — contains the token.
    let repo = git2::Repository::open(harness.instance_dir("alice")).expect("repository");
    let odb = repo.odb().expect("object database");
    let mut oids = Vec::new();
    odb.foreach(|oid| {
        oids.push(*oid);
        true
    })
    .expect("walk the object database");
    assert!(!oids.is_empty(), "the history has objects");
    for oid in oids {
        let object = odb.read(oid).expect("object");
        let text = String::from_utf8_lossy(object.data());
        assert!(
            !text.contains(CHANNEL_TOKEN),
            "object {oid} leaks the channel token"
        );
    }
}

#[tokio::test]
async fn export_does_not_include_the_repository() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    put_file(&harness, "alice", "/SOUL.md", "exported soul\n").await;
    assert!(harness.instance_dir("alice").join(".git").is_dir());

    // The archive builder walks `data/` and the compose file only: the
    // repository lives next to `data/`, so it can never be exported.
    let spec = harness.state.db.get("alice").expect("db").expect("spec");
    let bytes = fleett::instance_archive::export(&harness.state.cfg, &spec).expect("export");

    let archive = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut tar = tar::Archive::new(archive);
    let mut paths = Vec::new();
    for entry in tar.entries().expect("entries") {
        let entry = entry.expect("entry");
        paths.push(entry.path().expect("path").to_string_lossy().to_string());
    }
    assert!(paths.iter().any(|path| path == "data/SOUL.md"), "{paths:?}");
    assert!(
        paths.iter().any(|path| path == "manifest.json"),
        "{paths:?}"
    );
    for path in &paths {
        assert!(!path.contains(".git"), "export carries {path}");
        assert!(!path.contains("secrets.env"), "export carries {path}");
    }
}

#[tokio::test]
async fn a_viewer_reads_the_history_but_never_reverts() {
    let harness = Harness::new();
    create_instance(&harness, "alice").await;
    put_file(&harness, "alice", "/SOUL.md", "first\n").await;
    put_file(&harness, "alice", "/SOUL.md", "second\n").await;

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/tokens",
            Some(json!({"name": "auditor", "role": "viewer"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let viewer = parse(&body)["token"]
        .as_str()
        .expect("token secret")
        .to_string();

    let (status, body) = harness
        .request_as(Method::GET, "/api/instances/alice/git/log", None, &viewer)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(parse(&body)["commits"].as_array().expect("commits").len() >= 2);

    let (status, _) = harness
        .request_as(
            Method::GET,
            "/api/instances/alice/git/status",
            None,
            &viewer,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = harness
        .request_as(
            Method::POST,
            "/api/instances/alice/git/revert",
            Some(json!({"commit": "HEAD"})),
            &viewer,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}
