//! Integration tests for `GET /api/instances/{id}/export` and
//! `POST /api/instances/import` (issue #7).
//!
//! Every test runs against `MockDriver`; no test ever calls Docker.

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use flate2::read::GzDecoder;
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::instance_archive::{ArchiveInstance, ArchiveManifest, ARCHIVE_FORMAT, FORMAT_VERSION};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const BASE_URL: &str = "https://opencode.ai/inference/go/openai/v1";
const MODEL: &str = "deepseek-v4.1-flash";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const GATE_TOKEN: &str = "gate-secret";
const CHANNEL_TOKEN: &str = "123:ABC";

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

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn instance_dir(&self, id: &str) -> PathBuf {
        self.root().join("instances").join(id)
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Body,
        content_type: Option<&str>,
    ) -> (StatusCode, HeaderMap, Bytes) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        if let Some(value) = content_type {
            builder = builder.header(header::CONTENT_TYPE, value);
        }
        let request = builder.body(body).expect("valid request");
        let response = router(self).oneshot(request).await.expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();
        (status, headers, bytes)
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        let body = match body {
            Some(value) => Body::from(value.to_string()),
            None => Body::empty(),
        };
        let (status, _headers, bytes) =
            self.send(method, uri, body, Some("application/json")).await;
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    async fn create(&self, id: &str, extra: Value) -> Value {
        let mut body = json!({
            "id": id,
            "name": format!("Agent {id}"),
            "template": "guest",
            "model": MODEL,
            "base_url": BASE_URL,
            "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
            "channel": {"type": "telegram", "token": CHANNEL_TOKEN},
            "shares": [],
            "mem_limit_mb": 1024,
            "notes": "integration test",
            "gate_token": GATE_TOKEN,
        });
        if let Value::Object(extra) = extra {
            for (key, value) in extra {
                body[key] = value;
            }
        }
        let (status, response) = self
            .request(Method::POST, "/api/instances", Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "create failed: {response}");
        parse(&response)
    }

    async fn export(&self, id: &str) -> (StatusCode, HeaderMap, Bytes) {
        self.send(
            Method::GET,
            &format!("/api/instances/{id}/export"),
            Body::empty(),
            None,
        )
        .await
    }

    async fn import_bytes(&self, id: &str, start: bool, bytes: Bytes) -> (StatusCode, String) {
        let (status, _headers, body) = self
            .send(
                Method::POST,
                &format!("/api/instances/import?id={id}&start={start}"),
                Body::from(bytes),
                Some("application/gzip"),
            )
            .await;
        (status, String::from_utf8_lossy(&body).to_string())
    }
}

fn router(harness: &Harness) -> Router {
    api::router(harness.state.clone())
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

/// Every entry of a `.tgz`, as `(name, contents)`; directories have no content.
fn archive_entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut archive = tar::Archive::new(GzDecoder::new(bytes));
    let mut entries = Vec::new();
    for entry in archive.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let name = entry.path().expect("path").display().to_string();
        let mut contents = Vec::new();
        if entry.header().entry_type().is_file() {
            std::io::Read::read_to_end(&mut entry, &mut contents).expect("read entry");
        }
        entries.push((name, contents));
    }
    entries
}

fn manifest_of(instance: &Value) -> ArchiveInstance {
    ArchiveInstance {
        id: instance["id"].as_str().expect("id").to_string(),
        name: instance["name"].as_str().expect("name").to_string(),
        template: instance["template"].as_str().expect("template").to_string(),
        model: instance["model"].as_str().expect("model").to_string(),
        base_url: instance["base_url"].as_str().expect("base_url").to_string(),
        api_key_env: instance["api_key_env"]
            .as_str()
            .expect("api_key_env")
            .to_string(),
        channel: instance["channel"].as_str().expect("channel").to_string(),
        provider: None,
        mem_limit_mb: instance["mem_limit_mb"].as_u64().expect("mem") as u32,
        cpu_limit: instance["cpu_limit"].as_f64().expect("cpu"),
        allow_lan: instance["allow_lan"].as_bool().expect("allow_lan"),
        // Older archives omit both fields; the defaults are applied on import.
        disk_quota_mb: instance["disk_quota_mb"].as_u64().unwrap_or(0) as u32,
        disk_policy: instance["disk_policy"]
            .as_str()
            .unwrap_or("warn")
            .to_string(),
        shares: Vec::new(),
        skills: Vec::new(),
        notes: String::new(),
        host: fleett::hosts::LOCAL_HOST.to_string(),
    }
}

/// Build a valid archive whose only unusual property is one hostile entry.
fn hostile_archive(instance: &Value, entry_name: &str) -> Bytes {
    let manifest = ArchiveManifest {
        format: ARCHIVE_FORMAT.to_string(),
        format_version: FORMAT_VERSION,
        created_at: "2026-11-08T00:00:00Z".to_string(),
        fleett_version: "0.2.0".to_string(),
        instance: manifest_of(instance),
    };
    let mut builder = tar::Builder::new(fleett::archive::gzip_writer());
    fleett::archive::append_bytes(
        &mut builder,
        Path::new("manifest.json"),
        &serde_json::to_vec(&manifest).expect("manifest"),
        0o600,
    )
    .expect("manifest entry");
    // The tar writer refuses `..` itself; write the raw name field instead.
    let mut header = tar::Header::new_gnu();
    header.set_size(2);
    header.set_mode(0o600);
    header.set_entry_type(tar::EntryType::Regular);
    {
        let name = entry_name.as_bytes();
        header.as_mut_bytes()[..name.len()].copy_from_slice(name);
    }
    header.set_cksum();
    builder.append(&header, &b"x\n"[..]).expect("hostile entry");
    let bytes = builder.into_inner().expect("tar").finish().expect("gzip");
    Bytes::from(bytes)
}

fn secret_values(harness: &Harness, id: &str) -> BTreeMap<String, String> {
    fleett::envs::values(&harness.state.cfg, &harness.state.db, id).expect("secrets readable")
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_is_a_gzip_tar_without_any_secret() {
    let harness = Harness::new();
    harness.create("alice", json!({})).await;
    std::fs::write(
        harness.instance_dir("alice").join("data/MEMORY.md"),
        "remember\n",
    )
    .expect("memory");

    let (status, headers, body) = harness.export("alice").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/gzip")
    );
    assert_eq!(
        headers
            .get(header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok()),
        Some("attachment; filename=\"alice.tgz\"")
    );

    let entries = archive_entries(&body);
    let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    assert!(names.contains(&"manifest.json"), "{names:?}");
    assert!(names.contains(&"compose.yaml"), "{names:?}");
    assert!(names.contains(&"data/config.yaml"), "{names:?}");
    assert!(names.contains(&"data/MEMORY.md"), "{names:?}");
    assert!(
        !names.iter().any(|name| name.ends_with("secrets.env")),
        "{names:?}"
    );

    let everything: String = entries
        .iter()
        .map(|(_, contents)| String::from_utf8_lossy(contents).to_string())
        .collect();
    assert!(!everything.contains(CHANNEL_TOKEN));
    assert!(!everything.contains(GATE_TOKEN));

    let manifest: Value = serde_json::from_slice(
        &entries
            .iter()
            .find(|(name, _)| name == "manifest.json")
            .expect("manifest")
            .1,
    )
    .expect("manifest JSON");
    assert_eq!(manifest["format"], ARCHIVE_FORMAT);
    assert_eq!(manifest["format_version"], FORMAT_VERSION);
    assert_eq!(manifest["instance"]["id"], "alice");
    assert_eq!(manifest["instance"]["mem_limit_mb"], 1024);
}

#[tokio::test]
async fn export_of_an_unknown_instance_is_a_404() {
    let harness = Harness::new();
    let (status, _, body) = harness.export("ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        parse(&String::from_utf8_lossy(&body))["error"]["code"],
        "not_found"
    );
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_delete_import_round_trips_the_data() {
    let harness = Harness::new();
    harness.create("alice", json!({})).await;
    std::fs::write(
        harness.instance_dir("alice").join("data/MEMORY.md"),
        "remember this\n",
    )
    .expect("memory");
    std::fs::create_dir_all(harness.instance_dir("alice").join("data/sessions")).expect("sessions");
    std::fs::write(
        harness.instance_dir("alice").join("data/sessions/one.json"),
        "{\"turn\":1}",
    )
    .expect("session");

    let (status, _, archive) = harness.export("alice").await;
    assert_eq!(status, StatusCode::OK);

    // Delete with purge so the import really starts from nothing.
    let (status, body) = harness
        .request(Method::DELETE, "/api/instances/alice?purge=true", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!harness.instance_dir("alice").exists());

    harness.mock.clear_calls();
    let (status, body) = harness.import_bytes("alice", false, archive).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["id"], "alice");
    assert_eq!(detail["status"], "created", "start=false must not start");
    assert_eq!(detail["health"], "none");
    assert_eq!(
        std::fs::read_to_string(harness.instance_dir("alice").join("data/MEMORY.md"))
            .expect("memory restored"),
        "remember this\n"
    );
    assert_eq!(
        std::fs::read_to_string(harness.instance_dir("alice").join("data/sessions/one.json"))
            .expect("session restored"),
        "{\"turn\":1}"
    );
    assert!(harness
        .instance_dir("alice")
        .join("data/config.yaml")
        .is_file());

    // The imported instance is indexed and holds no secret at all.
    let stored = fleett::envs::values(&harness.state.cfg, &harness.state.db, "alice")
        .expect("secrets readable");
    assert!(stored.is_empty(), "{stored:?}");
    assert_eq!(detail["env"], json!([]));

    // The warnings tell the operator which tokens to PATCH.
    let warnings: Vec<&str> = detail["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let joined = warnings.join("\n");
    assert!(joined.contains("no secrets"), "{joined}");
    assert!(joined.contains("HERMES_GATE_PROFILE_TOKEN"), "{joined}");
    assert!(joined.contains("TELEGRAM_BOT_TOKEN"), "{joined}");

    // The compose file is rewritten for the new project and still interpolates
    // the token names, so a PATCH restores the environment.
    let compose = std::fs::read_to_string(harness.instance_dir("alice").join("compose.yaml"))
        .expect("compose");
    let doc: serde_yaml::Value = serde_yaml::from_str(&compose).expect("compose YAML");
    assert_eq!(doc["name"], "fleett-alice");
    assert_eq!(doc["services"]["agent"]["container_name"], "fleett-alice");
    assert!(
        doc["services"]["agent"].get("env_file").is_none(),
        "{compose}"
    );
    assert_eq!(
        doc["services"]["agent"]["environment"]["TELEGRAM_BOT_TOKEN"],
        "${TELEGRAM_BOT_TOKEN}"
    );
    assert!(
        !compose.contains(CHANNEL_TOKEN),
        "no literal token in the compose file"
    );

    // Nothing was started: no `up` call reached the driver.
    assert!(
        !harness
            .mock
            .calls()
            .iter()
            .any(|call| call.operation == "up"),
        "{:?}",
        harness.mock.calls()
    );
}

#[tokio::test]
async fn import_into_an_existing_id_is_a_409_and_changes_nothing() {
    let harness = Harness::new();
    harness.create("alice", json!({})).await;
    let (status, _, archive) = harness.export("alice").await;
    assert_eq!(status, StatusCode::OK);
    std::fs::write(
        harness.instance_dir("alice").join("data/MEMORY.md"),
        "original\n",
    )
    .expect("memory");

    let (status, body) = harness.import_bytes("alice", true, archive).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "instance_exists");
    assert_eq!(
        std::fs::read_to_string(harness.instance_dir("alice").join("data/MEMORY.md"))
            .expect("memory kept"),
        "original\n"
    );
    // The existing instance keeps its tokens: the refused import wrote nothing.
    assert_eq!(
        secret_values(&harness, "alice")
            .get("TELEGRAM_BOT_TOKEN")
            .map(String::as_str),
        Some(CHANNEL_TOKEN)
    );
}

#[tokio::test]
async fn import_preserves_the_manifest_metadata() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "findings"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    harness
        .create(
            "alice",
            json!({
                "name": "Alice the analyst",
                "mem_limit_mb": 2048,
                "cpu_limit": 1.5,
                "allow_lan": true,
                "notes": "kept across the archive",
                "shares": [{"name": "findings", "mode": "ro"}],
            }),
        )
        .await;
    let (status, _, archive) = harness.export("alice").await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = harness.import_bytes("bob", false, archive).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["id"], "bob");
    assert_eq!(detail["name"], "Alice the analyst");
    assert_eq!(detail["mem_limit_mb"], 2048);
    assert_eq!(detail["cpu_limit"], 1.5);
    assert_eq!(detail["allow_lan"], true);
    assert_eq!(detail["notes"], "kept across the archive");
    assert_eq!(detail["shares"][0]["name"], "findings");
    assert_eq!(detail["shares"][0]["mode"], "ro");

    // The copy is its own compose project and container.
    let compose =
        std::fs::read_to_string(harness.instance_dir("bob").join("compose.yaml")).expect("compose");
    let doc: serde_yaml::Value = serde_yaml::from_str(&compose).expect("compose YAML");
    assert_eq!(doc["name"], "fleett-bob");
    assert_eq!(doc["services"]["agent"]["container_name"], "fleett-bob");
}

#[tokio::test]
async fn import_rejects_zip_slip_entries() {
    let harness = Harness::new();
    harness.create("alice", json!({})).await;
    let (_, _, archive) = harness.export("alice").await;
    let entries = archive_entries(&archive);
    let manifest: Value = serde_json::from_slice(
        &entries
            .iter()
            .find(|(name, _)| name == "manifest.json")
            .expect("manifest")
            .1,
    )
    .expect("manifest JSON");
    let hostile = hostile_archive(&manifest["instance"], "../../evil");

    let (status, body) = harness.import_bytes("mallory", false, hostile).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_archive");
    assert!(!harness.root().join("evil").exists(), "the entry escaped");
    assert!(
        !harness.instance_dir("mallory").exists(),
        "a refused import must not create an instance directory"
    );
    assert!(!harness.state.db.exists("mallory").expect("index readable"));
}

#[tokio::test]
async fn import_rejects_an_absolute_entry_path() {
    let harness = Harness::new();
    harness.create("alice", json!({})).await;
    let (_, _, archive) = harness.export("alice").await;
    let entries = archive_entries(&archive);
    let manifest: Value = serde_json::from_slice(
        &entries
            .iter()
            .find(|(name, _)| name == "manifest.json")
            .expect("manifest")
            .1,
    )
    .expect("manifest JSON");
    let hostile = hostile_archive(&manifest["instance"], "/tmp/fleett-escape");

    let (status, body) = harness.import_bytes("mallory", false, hostile).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!Path::new("/tmp/fleett-escape").exists());
}

#[tokio::test]
async fn import_requires_a_valid_and_free_id() {
    let harness = Harness::new();
    harness.create("alice", json!({})).await;
    let (_, _, archive) = harness.export("alice").await;

    let (status, _headers, body) = harness
        .send(
            Method::POST,
            "/api/instances/import",
            Body::from(archive.clone()),
            Some("application/gzip"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        parse(&String::from_utf8_lossy(&body))["error"]["code"],
        "invalid_id"
    );

    let (status, body) = harness.import_bytes("Alice", false, archive).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn import_starts_the_instance_only_when_asked() {
    let harness = Harness::new();
    harness.create("alice", json!({})).await;
    let (_, _, archive) = harness.export("alice").await;

    harness.mock.clear_calls();
    let (status, body) = harness.import_bytes("bob", true, archive).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["status"], "running");
    assert!(
        harness
            .mock
            .calls()
            .iter()
            .any(|call| call.operation == "up" && call.instance == "bob"),
        "{:?}",
        harness.mock.calls()
    );
}

#[tokio::test]
async fn import_rejects_a_body_that_is_not_a_gzip_tar() {
    let harness = Harness::new();
    let (status, body) = harness
        .import_bytes("alice", false, Bytes::from_static(b"this is not a tgz"))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_archive");

    let (status, body) = harness.import_bytes("alice", false, Bytes::new()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "empty_archive");
}
