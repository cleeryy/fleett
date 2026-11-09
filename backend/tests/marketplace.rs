//! Integration tests for the skill marketplace (issue #61).
//!
//! Every provider talks to a local axum fixture instead of the internet: the
//! GitHub API, the git index, skills.sh and the archives all live on
//! `127.0.0.1`. Nothing here needs a network, a token or Docker.

use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::{header, HeaderMap, Method, Request, Response, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::Router;
use fleett::api::{self, AppState};
use fleett::config::{Config, MarketplaceSkillsSh};
use fleett::driver::mock::MockDriver;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Cursor, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];

// ---------------------------------------------------------------------------
// Fixture server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct FixtureData {
    commit: String,
    zip: Vec<u8>,
    github_items: Vec<Value>,
    index: Value,
    skills_sh_search: String,
    skills_sh_detail: String,
    archives: HashMap<String, Vec<u8>>,
}

#[derive(Clone)]
struct FixtureState {
    inner: Arc<Mutex<FixtureData>>,
}

struct Fixture {
    base: String,
    state: FixtureState,
}

impl Fixture {
    async fn start() -> Self {
        let state = FixtureState {
            inner: Arc::new(Mutex::new(FixtureData {
                commit: "c1".to_string(),
                zip: zip_bytes(&[
                    ("pdf-tools-main/SKILL.md", "# PDF Tools\n"),
                    ("pdf-tools-main/notes/pages.md", "one page\n"),
                ]),
                github_items: vec![json!({
                    "name": "pdf-tools",
                    "description": "PDF tools for agents",
                    "html_url": "https://github.com/acme/pdf-tools",
                    "owner": {"login": "acme"},
                    "topics": ["pdf", "documents"],
                })],
                index: json!({"skills": [{
                    "name": "git-pdf",
                    "description": "PDF skill from the git index",
                    "source": "github:acme/pdf-tools",
                    "version": "1.2.0",
                    "commit": "abc1234",
                }]}),
                skills_sh_search: "<a href=\"/acme/pdf-tools\">PDF tools for agents</a>"
                    .to_string(),
                skills_sh_detail: "<a href=\"https://github.com/acme/pdf-tools\">upstream</a>"
                    .to_string(),
                archives: HashMap::new(),
            })),
        };
        let app = Router::new()
            .fallback(fixture_handler)
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base: format!("http://{address}"),
            state,
        }
    }

    fn update(&self, edit: impl FnOnce(&mut FixtureData)) {
        let mut data = self.state.inner.lock().expect("fixture lock");
        edit(&mut data);
    }

    fn commit(&self, commit: &str) {
        self.update(|data| data.commit = commit.to_string());
    }

    fn zip(&self, bytes: Vec<u8>) {
        self.update(|data| data.zip = bytes);
    }

    fn archive(&self, name: &str, bytes: Vec<u8>) {
        self.update(|data| {
            data.archives.insert(name.to_string(), bytes);
        });
    }
}

async fn fixture_handler(AxumState(state): AxumState<FixtureState>, uri: Uri) -> Response<Body> {
    let path = uri.path().to_string();
    let data = state.inner.lock().expect("fixture lock").clone();
    if path == "/repos/acme/pdf-tools" {
        return json_response(json!({
            "default_branch": "main",
            "description": "PDF tools for agents",
            "owner": {"login": "acme"},
            "topics": ["pdf", "documents"],
        }));
    }
    if path.starts_with("/repos/acme/pdf-tools/commits/") {
        return json_response(json!({ "sha": data.commit }));
    }
    if path == "/repos/acme/pdf-tools/zipball/main" {
        return bytes_response("application/zip", data.zip);
    }
    if path == "/repos/acme/index/contents/index.json" {
        return bytes_response(
            "application/json",
            serde_json::to_vec(&data.index).expect("index"),
        );
    }
    if path == "/search/repositories" {
        return json_response(json!({ "items": data.github_items }));
    }
    if path == "/sh/search" {
        return bytes_response("text/html", data.skills_sh_search.into_bytes());
    }
    if path == "/sh/acme/pdf-tools" {
        return bytes_response("text/html", data.skills_sh_detail.into_bytes());
    }
    if let Some(name) = path.strip_prefix("/archives/") {
        if let Some(bytes) = data.archives.get(name) {
            return bytes_response("application/octet-stream", bytes.clone());
        }
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn json_response(value: Value) -> Response<Body> {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response()
}

fn bytes_response(content_type: &str, bytes: Vec<u8>) -> Response<Body> {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type.to_string())],
        bytes,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    dir: tempfile::TempDir,
    state: AppState,
    fixture: Fixture,
    mock: Arc<MockDriver>,
}

impl Harness {
    async fn new() -> Self {
        Self::new_with(|_| {}).await
    }

    async fn new_with(configure: impl FnOnce(&mut Config)) -> Self {
        let fixture = Fixture::start().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        cfg.marketplace_api_base = fixture.base.clone();
        cfg.marketplace_skills_sh_base = format!("{}/sh", fixture.base);
        cfg.marketplace_git_index = Some("acme/index".to_string());
        cfg.marketplace_insecure = true;
        configure(&mut cfg);
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        Self {
            dir,
            state,
            fixture,
            mock,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn skills_dir(&self) -> PathBuf {
        self.root().join("skills")
    }

    fn compose(&self, id: &str) -> String {
        std::fs::read_to_string(self.root().join("instances").join(id).join("compose.yaml"))
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

    async fn install(&self, source: &str, reference: Option<&str>) -> (StatusCode, Value) {
        let (status, body) = self
            .request(
                Method::POST,
                "/api/marketplace/skills/install",
                Some(json!({
                    "source": source,
                    "ref": reference,
                    "confirm_scripts": true,
                })),
            )
            .await;
        let value = serde_json::from_str(&body).unwrap_or(Value::Null);
        (status, value)
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
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

/// Build a zip archive from `(path, content)` entries.
fn zip_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for (path, content) in entries {
        writer.start_file(path, options).expect("start file");
        writer.write_all(content.as_bytes()).expect("write entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

fn symlink_zip() -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    writer.start_file("SKILL.md", options).expect("start file");
    writer.write_all(b"# Skill\n").expect("write");
    writer
        .add_symlink("link.md", "/etc/passwd", options)
        .expect("symlink");
    writer.finish().expect("finish zip").into_inner()
}

/// The standard fixture archive with a `pdf-tools` frontmatter.
fn pdf_zip(version: &str) -> Vec<u8> {
    zip_bytes(&[
        (
            "pdf-tools-main/SKILL.md",
            &format!(
                "---\nname: PDF Tools\ndescription: Work with PDF documents ({version})\n---\n# PDF Tools\n"
            ),
        ),
        ("pdf-tools-main/notes/pages.md", "one page\n"),
    ])
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_covers_every_provider() {
    let harness = Harness::new().await;
    std::fs::write(
        harness.root().join("marketplace").join("index.json"),
        serde_json::to_vec(&json!({"skills": [{
            "name": "local-pdf",
            "description": "PDF tools from the local index",
            "url": "https://example.com/local-pdf.zip",
            "commit": "local1",
        }]}))
        .expect("index"),
    )
    .expect("write index");

    let (status, body) = harness
        .request(Method::GET, "/api/marketplace/skills?q=pdf&limit=20", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response = parse(&body);
    let providers: Vec<&str> = response["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter_map(|entry| entry["provider"].as_str())
        .collect();
    for provider in ["local", "git", "github", "skills.sh"] {
        assert!(
            providers.contains(&provider),
            "missing {provider} in {providers:?}"
        );
    }
    let sources = response["sources"].as_array().expect("sources");
    assert_eq!(sources.len(), 4);
    for source in sources {
        assert_eq!(source["status"], "ok", "{source}");
        assert!(
            source["count"].as_u64().unwrap_or_default() >= 1,
            "{source}"
        );
    }
}

#[tokio::test]
async fn skills_sh_off_disables_only_that_provider() {
    let harness =
        Harness::new_with(|cfg| cfg.marketplace_skills_sh = MarketplaceSkillsSh::Off).await;
    let (status, body) = harness
        .request(Method::GET, "/api/marketplace/skills?q=pdf", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response = parse(&body);
    let sources = response["sources"].as_array().expect("sources");
    let skills_sh = sources
        .iter()
        .find(|source| source["source"] == "skills.sh")
        .expect("skills.sh status");
    assert_eq!(skills_sh["status"], "disabled", "{skills_sh}");
    assert!(!response["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .any(|entry| entry["provider"] == "skills.sh"));
    assert!(response["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .any(|entry| entry["provider"] == "github"));
}

#[tokio::test]
async fn an_unknown_provider_filter_is_a_400() {
    let harness = Harness::new().await;
    let (status, body) = harness
        .request(
            Method::GET,
            "/api/marketplace/skills?q=pdf&source=nope",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_source");
}

// ---------------------------------------------------------------------------
// Preview and install
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preview_lists_files_sizes_and_scripts() {
    let harness = Harness::new().await;
    harness.fixture.zip(zip_bytes(&[
        (
            "pdf-tools-main/SKILL.md",
            "---\nname: PDF Tools\ndescription: Work with PDFs\n---\n# PDF Tools\n",
        ),
        ("pdf-tools-main/scripts/install.sh", "#!/bin/sh\necho hi\n"),
    ]));

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/marketplace/skills/preview",
            Some(json!({"source": "github:acme/pdf-tools", "ref": "main"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let preview = parse(&body);
    assert_eq!(preview["name"], "pdf-tools");
    assert_eq!(preview["description"], "Work with PDFs");
    assert_eq!(preview["commit"], "c1");
    assert_eq!(preview["source"], "github:acme/pdf-tools");
    assert_eq!(preview["requires_confirmation"], true);
    assert_eq!(preview["scripts"], json!(["scripts/install.sh"]));
    let files = preview["files"].as_array().expect("files");
    assert_eq!(files.len(), 2);
    let script = files
        .iter()
        .find(|file| file["path"] == "scripts/install.sh")
        .expect("script file");
    assert_eq!(script["script"], true);
    assert!(preview["total_bytes"].as_u64().unwrap_or_default() > 0);
    assert!(preview["skill_md"]
        .as_str()
        .unwrap_or_default()
        .contains("# PDF Tools"));

    // Installing without confirmation is a 409 listing the scripts.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/marketplace/skills/install",
            Some(json!({"source": "github:acme/pdf-tools", "ref": "main"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = &parse(&body)["error"];
    assert_eq!(error["code"], "scripts_require_confirmation");
    assert_eq!(error["details"]["scripts"], json!(["scripts/install.sh"]));
    assert!(!harness.skills_dir().join("pdf-tools").exists());
}

#[tokio::test]
async fn install_publishes_provenance_and_mounts_read_only() {
    let harness = Harness::new().await;
    harness.fixture.zip(pdf_zip("v1"));

    let (status, detail) = harness.install("github:acme/pdf-tools", Some("main")).await;
    assert_eq!(status, StatusCode::CREATED, "{detail}");
    assert_eq!(detail["name"], "pdf-tools");
    assert_eq!(detail["latest"], "v1");
    assert_eq!(detail["source"], "github:acme/pdf-tools");
    assert_eq!(detail["commit"], "c1");
    assert_eq!(detail["versions"][0]["source"], "github:acme/pdf-tools");
    assert_eq!(detail["versions"][0]["commit"], "c1");

    // The registry endpoint exposes the same provenance.
    let (status, body) = harness
        .request(Method::GET, "/api/skills/pdf-tools", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = parse(&body);
    assert_eq!(stored["source"], "github:acme/pdf-tools");
    assert_eq!(stored["commit"], "c1");

    // ... and so does `skill.yaml` on disk.
    let meta: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(
            harness
                .skills_dir()
                .join("pdf-tools")
                .join("v1")
                .join("skill.yaml"),
        )
        .expect("skill.yaml"),
    )
    .expect("yaml");
    assert_eq!(meta["name"], "pdf-tools");
    assert_eq!(meta["source"], "github:acme/pdf-tools");
    assert_eq!(meta["ref"], "main");
    assert_eq!(meta["commit"], "c1");
    assert!(meta["installed_at"]
        .as_str()
        .is_some_and(|at| !at.is_empty()));

    // An instance following latest mounts the resolved version read-only.
    harness
        .create_instance("alice", json!([{"name": "pdf-tools"}]))
        .await;
    assert!(harness
        .compose("alice")
        .contains("../../skills/pdf-tools/v1:/opt/data/skills/pdf-tools:ro"));

    assert_eq!(harness.audit("marketplace.install").await.len(), 1);
}

#[tokio::test]
async fn install_refuses_zip_slip_symlinks_and_oversize_files() {
    let harness = Harness::new().await;
    harness.fixture.archive(
        "slip.zip",
        zip_bytes(&[("SKILL.md", "# Slip\n"), ("../escape.md", "nope")]),
    );
    harness.fixture.archive("symlink.zip", symlink_zip());
    let huge = "a".repeat(300 * 1024);
    harness
        .fixture
        .archive("huge.zip", zip_bytes(&[("SKILL.md", huge.as_str())]));

    for archive in ["slip.zip", "symlink.zip", "huge.zip"] {
        let source = format!("url:{}/archives/{archive}", harness.fixture.base);
        let (status, body) = harness
            .request(
                Method::POST,
                "/api/marketplace/skills/preview",
                Some(json!({"source": source})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{archive}: {body}");
        assert_eq!(parse(&body)["error"]["code"], "invalid_skill", "{archive}");
    }
    assert!(!harness.skills_dir().join("slip").exists());
    assert!(!harness.root().join("escape.md").exists());
}

// ---------------------------------------------------------------------------
// Updates and promote
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_publishes_v2_and_promotes_followers() {
    let harness = Harness::new().await;
    harness.fixture.zip(pdf_zip("v1"));
    let (status, detail) = harness.install("github:acme/pdf-tools", Some("main")).await;
    assert_eq!(status, StatusCode::CREATED, "{detail}");
    harness
        .create_instance("alice", json!([{"name": "pdf-tools"}]))
        .await;

    // No upstream change yet.
    let (status, body) = harness
        .request(Method::GET, "/api/marketplace/updates", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["updates"], json!([]));

    // New upstream commit and content.
    harness.fixture.commit("c2");
    harness.fixture.zip(zip_bytes(&[
        (
            "pdf-tools-main/SKILL.md",
            "---\nname: PDF Tools\ndescription: Work with PDFs v2\n---\n# PDF Tools v2\n",
        ),
        ("pdf-tools-main/notes/pages.md", "two pages\n"),
        ("pdf-tools-main/notes/forms.md", "forms\n"),
    ]));
    // The empty answer was cached by the previous call: a real deployment
    // waits for the 60 s TTL, the test clears it explicitly.
    harness.state.marketplace.invalidate();
    let (status, body) = harness
        .request(Method::GET, "/api/marketplace/updates", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let updates = parse(&body);
    assert_eq!(updates["updates"].as_array().expect("updates").len(), 1);
    assert_eq!(updates["updates"][0]["name"], "pdf-tools");
    assert_eq!(updates["updates"][0]["installed_version"], "v1");
    assert_eq!(updates["updates"][0]["commit"], "c2");
    assert_eq!(updates["updates"][0]["source"], "github:acme/pdf-tools");

    harness.mock.clear_calls();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/marketplace/skills/pdf-tools/update",
            Some(json!({"promote": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let update = parse(&body);
    assert_eq!(update["name"], "pdf-tools");
    assert_eq!(update["from"], "v1");
    assert_eq!(update["to"], "v2");
    assert_eq!(update["commit"], "c2");
    let changes: Vec<(&str, &str)> = update["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|file| {
            (
                file["path"].as_str().expect("path"),
                file["status"].as_str().expect("status"),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [
            ("SKILL.md", "modified"),
            ("notes/forms.md", "added"),
            ("notes/pages.md", "modified"),
        ]
    );
    assert_eq!(update["promoted"]["latest"], "v2");
    let results = update["promoted"]["results"].as_array().expect("results");
    assert_eq!(results[0]["id"], "alice");
    assert_eq!(results[0]["status"], "restarted");
    assert!(harness
        .compose("alice")
        .contains("../../skills/pdf-tools/v2:/opt/data/skills/pdf-tools:ro"));
    let ups: Vec<String> = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| call.operation == "up")
        .map(|call| call.instance)
        .collect();
    assert_eq!(ups, ["alice"]);

    // The second update is a no-op: `409 upstream_unchanged`.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/marketplace/skills/pdf-tools/update",
            Some(json!({"promote": false})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "upstream_unchanged");

    // The cached `updates` answer was invalidated by the update.
    let (status, body) = harness
        .request(Method::GET, "/api/marketplace/updates", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["updates"], json!([]));

    assert_eq!(harness.audit("marketplace.update").await.len(), 1);
}

#[tokio::test]
async fn a_local_publish_has_no_upstream() {
    let harness = Harness::new().await;
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/skills",
            Some(json!({
                "name": "local-only",
                "version": "v1",
                "description": "local",
                "files": [{"path": "SKILL.md", "content": "# Local\n"}],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/marketplace/skills/local-only/update",
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "no_upstream");

    let (status, body) = harness
        .request(Method::GET, "/api/marketplace/updates", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["updates"], json!([]));
}

// ---------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_writes_the_local_index_atomically() {
    let harness = Harness::new().await;
    let (status, body) = harness
        .request(Method::POST, "/api/marketplace/sync", Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sync = parse(&body);
    assert_eq!(sync["skills"], 1);
    assert_eq!(sync["source"], "acme/index");

    let path = harness.root().join("marketplace").join("index.json");
    let index: Value = serde_json::from_slice(&std::fs::read(&path).expect("index")).expect("json");
    assert_eq!(index["skills"][0]["name"], "git-pdf");
    assert_eq!(
        std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!path.with_extension("json.tmp").exists());

    let (status, body) = harness
        .request(
            Method::GET,
            "/api/marketplace/skills?q=git-pdf&source=local",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["entries"][0]["name"], "git-pdf");
    assert_eq!(harness.audit("marketplace.sync").await.len(), 1);
}

#[tokio::test]
async fn sync_without_a_configured_index_is_a_409() {
    let harness = Harness::new_with(|cfg| cfg.marketplace_git_index = None).await;
    let (status, body) = harness
        .request(Method::POST, "/api/marketplace/sync", Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "no_index_configured");
}

// ---------------------------------------------------------------------------
// Safety and authorization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uninstall_is_refused_while_an_instance_mounts_the_skill() {
    let harness = Harness::new().await;
    harness.fixture.zip(pdf_zip("v1"));
    let (status, detail) = harness.install("github:acme/pdf-tools", Some("main")).await;
    assert_eq!(status, StatusCode::CREATED, "{detail}");
    harness
        .create_instance("alice", json!([{"name": "pdf-tools"}]))
        .await;

    let (status, body) = harness
        .request(Method::DELETE, "/api/skills/pdf-tools", None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = &parse(&body)["error"];
    assert_eq!(error["code"], "skill_in_use");
    assert_eq!(error["details"]["instances"], json!(["alice"]));
}

#[tokio::test]
async fn a_viewer_reads_the_catalogue_but_never_mutates_it() {
    let harness = Harness::new().await;
    harness.fixture.zip(pdf_zip("v1"));
    let viewer = harness.viewer_token().await;

    let (status, body) = harness
        .request_as(
            Method::GET,
            "/api/marketplace/skills?q=pdf",
            None,
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = harness
        .request_as(
            Method::POST,
            "/api/marketplace/skills/preview",
            Some(json!({"source": "github:acme/pdf-tools"})),
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = harness
        .request_as(Method::GET, "/api/marketplace/updates", None, Some(&viewer))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let mutations = [
        (
            Method::POST,
            "/api/marketplace/skills/install",
            json!({"source": "github:acme/pdf-tools", "confirm_scripts": true}),
        ),
        (
            Method::POST,
            "/api/marketplace/skills/pdf-tools/update",
            json!({"promote": true}),
        ),
        (Method::POST, "/api/marketplace/sync", json!({})),
    ];
    for (method, uri, body) in mutations {
        let (status, response) = harness
            .request_as(method.clone(), uri, Some(body), Some(&viewer))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {response}");
    }
    assert!(!harness.skills_dir().join("pdf-tools").exists());
}

#[tokio::test]
async fn concurrent_installs_are_serialized() {
    let harness = Harness::new().await;
    harness.fixture.zip(pdf_zip("v1"));
    let body = json!({
        "source": "github:acme/pdf-tools",
        "ref": "main",
        "confirm_scripts": true,
    });
    let (first, second) = tokio::join!(
        harness.request(
            Method::POST,
            "/api/marketplace/skills/install",
            Some(body.clone())
        ),
        harness.request(Method::POST, "/api/marketplace/skills/install", Some(body))
    );
    assert_eq!(first.0, StatusCode::CREATED, "{}", first.1);
    assert_eq!(second.0, StatusCode::CREATED, "{}", second.1);
    // Two serialized installs produce two versions, never one overwritten
    // write: the second one computes `v2` only after the first is published.
    let (status, body) = harness
        .request(Method::GET, "/api/skills/pdf-tools", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["versions"].as_array().expect("versions").len(), 2);
    assert_eq!(detail["versions"][0]["version"], "v1");
    assert_eq!(detail["versions"][1]["version"], "v2");
    assert!(harness
        .skills_dir()
        .join("pdf-tools")
        .join("v1")
        .join("SKILL.md")
        .is_file());
}
