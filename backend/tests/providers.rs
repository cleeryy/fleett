//! Integration tests for reusable provider credentials (issue #25).
//!
//! Every test runs against `MockDriver`; the connectivity test talks to a
//! local `std::net` HTTP server, never to a real provider.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const PROVIDER_KEY_ENV: &str = "PROVIDER_API_KEY";
const PROVIDER_BASE_URL: &str = "https://provider.test/v1";
const MODEL: &str = "deepseek-v4.1-flash";

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

    /// Same harness without a secrets key: every encrypted write is refused.
    fn keyless() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().to_path_buf());
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        Self { dir, state, mock }
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
        send_to(api::router(self.state.clone()), method, uri, body, token).await
    }

    /// Every generated file under the instance directory, concatenated.
    fn instance_files(&self, id: &str) -> String {
        fn read_tree(dir: &Path, out: &mut String) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    read_tree(&path, out);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push_str(&text);
                    out.push('\n');
                }
            }
        }
        let mut out = String::new();
        read_tree(&self.dir.path().join("instances").join(id), &mut out);
        out
    }

    /// Environment the compose driver would inject for one instance.
    fn effective_env(&self, id: &str) -> std::collections::BTreeMap<String, String> {
        fleett::providers::resolved_env(&self.state.cfg, &self.state.db, id).expect("env readable")
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

async fn create_provider(harness: &Harness, name: &str, secret: &str) -> Value {
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/providers",
            Some(json!({
                "name": name,
                "kind": "openai-compatible",
                "base_url": PROVIDER_BASE_URL,
                "api_key_env": PROVIDER_KEY_ENV,
                "secret": secret,
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

async fn create_instance_with_provider(harness: &Harness, id: &str, provider: &str) -> Value {
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": id,
                "name": format!("Agent {id}"),
                "template": "guest",
                "model": MODEL,
                "provider": provider,
                "shares": [],
                "mem_limit_mb": 1024,
            })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create instance failed: {body}"
    );
    parse(&body)
}

/// Local provider simulator: `200` when the request carries the expected
/// bearer key, `401` otherwise. Returns the base URL.
fn spawn_models_server(secret: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock provider");
    let address = listener.local_addr().expect("local address");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buffer = [0u8; 8192];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let expected = format!("authorization: bearer {secret}").to_ascii_lowercase();
            let authorized = request.to_ascii_lowercase().contains(&expected);
            let (status, body) = if authorized {
                ("HTTP/1.1 200 OK", r#"{"data":[]}"#)
            } else {
                ("HTTP/1.1 401 Unauthorized", r#"{"error":"invalid key"}"#)
            };
            let response = format!(
                "{status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{address}")
}

#[tokio::test]
async fn two_instances_share_a_provider_and_one_rotation_restarts_both() {
    let harness = Harness::new();
    let provider = create_provider(&harness, "shared", "sk-one").await;
    let provider_id = provider["id"].as_str().expect("provider id").to_string();

    // Neither instance sets base_url or api_key_env: both default to the
    // provider's values.
    for id in ["alice", "bob"] {
        let detail = create_instance_with_provider(&harness, id, &provider_id).await;
        assert_eq!(detail["provider"], provider_id);
        assert_eq!(detail["base_url"], PROVIDER_BASE_URL);
        assert_eq!(detail["api_key_env"], PROVIDER_KEY_ENV);
        assert_eq!(harness.effective_env(id)["PROVIDER_API_KEY"], "sk-one");
        assert!(
            harness
                .instance_files(id)
                .contains("PROVIDER_API_KEY: ${PROVIDER_API_KEY}"),
            "the compose file must interpolate the variable"
        );
    }

    let (status, body) = harness
        .request(Method::GET, &format!("/api/providers/{provider_id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["instances_count"], 2);
    let attached: Vec<&str> = detail["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id"))
        .collect();
    assert_eq!(attached, vec!["alice", "bob"]);

    // Rotate: one PATCH restarts both, sequentially, with the new secret.
    harness.mock.clear_calls();
    let (status, body) = harness
        .request(
            Method::PATCH,
            &format!("/api/providers/{provider_id}"),
            Some(json!({ "secret": "sk-two" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rotated = parse(&body);
    assert_eq!(rotated["provider"]["instances_count"], 2);
    let restarted = rotated["restarted"].as_array().expect("restarted");
    assert_eq!(restarted.len(), 2, "{restarted:?}");
    assert_eq!(restarted[0]["id"], "alice");
    assert_eq!(restarted[0]["status"], "healthy");
    assert_eq!(restarted[1]["id"], "bob");
    assert_eq!(restarted[1]["status"], "healthy");

    let ups: Vec<String> = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| call.operation == "up")
        .map(|call| call.instance)
        .collect();
    assert_eq!(ups, vec!["alice".to_string(), "bob".to_string()]);

    for id in ["alice", "bob"] {
        assert_eq!(
            harness.effective_env(id)["PROVIDER_API_KEY"],
            "sk-two",
            "the rotation must reach every attached instance"
        );
    }
}

#[tokio::test]
async fn deleting_a_provider_in_use_is_a_conflict_with_the_instance_list() {
    let harness = Harness::new();
    let provider = create_provider(&harness, "shared", "sk-secret").await;
    let provider_id = provider["id"].as_str().expect("provider id").to_string();
    create_instance_with_provider(&harness, "alice", &provider_id).await;
    create_instance_with_provider(&harness, "bob", &provider_id).await;

    let (status, body) = harness
        .request(
            Method::DELETE,
            &format!("/api/providers/{provider_id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = parse(&body)["error"].clone();
    assert_eq!(error["code"], "provider_in_use");
    let listed: Vec<&str> = error["details"]["instances"]
        .as_array()
        .expect("instances details")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id"))
        .collect();
    assert_eq!(listed, vec!["alice", "bob"]);

    // Detaching both instances makes the deletion possible.
    for id in ["alice", "bob"] {
        let (status, body) = harness
            .request(
                Method::PATCH,
                &format!("/api/instances/{id}"),
                Some(json!({ "provider": null })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(parse(&body)["provider"].is_null());
    }
    let (status, body) = harness
        .request(
            Method::DELETE,
            &format!("/api/providers/{provider_id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

#[tokio::test]
async fn no_endpoint_ever_returns_the_secret() {
    let harness = Harness::new();
    let secret = "sk-super-secret-value";
    let provider = create_provider(&harness, "shared", secret).await;
    let provider_id = provider["id"].as_str().expect("provider id").to_string();
    create_instance_with_provider(&harness, "alice", &provider_id).await;

    let mut bodies = Vec::new();
    for (method, uri) in [
        (Method::GET, "/api/providers".to_string()),
        (Method::GET, format!("/api/providers/{provider_id}")),
        (Method::PATCH, format!("/api/providers/{provider_id}")),
        (Method::GET, "/api/instances/alice".to_string()),
        (Method::GET, "/api/instances".to_string()),
        (Method::GET, "/api/audit".to_string()),
    ] {
        let body = if method == Method::PATCH {
            Some(json!({ "name": "shared-renamed" }))
        } else {
            None
        };
        let (status, body) = harness.request(method, &uri, body).await;
        assert!(status.is_success(), "{uri}: {status} {body}");
        bodies.push(body);
    }
    bodies.push(harness.instance_files("alice"));

    for body in bodies {
        assert!(
            !body.contains(secret),
            "the provider secret leaked in: {body}"
        );
    }
}

#[tokio::test]
async fn a_legacy_instance_without_a_provider_keeps_working() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "legacy",
                "name": "Legacy",
                "template": "guest",
                "model": MODEL,
                "base_url": "https://legacy.test/v1",
                "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
                "channel": {"type": "telegram", "token": "123:ABC"},
                "gate_token": "sk-legacy-own-token",
                "shares": [],
                "mem_limit_mb": 1024,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = parse(&body);
    assert!(
        detail["provider"].is_null(),
        "a legacy instance has no provider: {detail}"
    );
    assert_eq!(detail["api_key_env"], "HERMES_GATE_PROFILE_TOKEN");
    assert_eq!(
        harness.effective_env("legacy")["HERMES_GATE_PROFILE_TOKEN"],
        "sk-legacy-own-token"
    );
    assert!(harness
        .instance_files("legacy")
        .contains("HERMES_GATE_PROFILE_TOKEN: ${HERMES_GATE_PROFILE_TOKEN}"));

    // Lifecycle calls still work, with no provider involved.
    let (status, body) = harness
        .request(Method::POST, "/api/instances/legacy/restart", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_provider_base_url_wins_when_the_instance_omits_it_but_not_when_it_overrides_it() {
    let harness = Harness::new();
    let provider = create_provider(&harness, "shared", "sk-secret").await;
    let provider_id = provider["id"].as_str().expect("provider id").to_string();

    let inherited = create_instance_with_provider(&harness, "inherits", &provider_id).await;
    assert_eq!(inherited["base_url"], PROVIDER_BASE_URL);

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "overrides",
                "template": "guest",
                "model": MODEL,
                "provider": provider_id,
                "base_url": "https://override.test/v1",
                "api_key_env": "OTHER_KEY",
                "mem_limit_mb": 512,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let detail = parse(&body);
    assert_eq!(detail["base_url"], "https://override.test/v1");
    assert_eq!(detail["api_key_env"], "OTHER_KEY");
    assert_eq!(detail["provider"], provider_id.as_str());
    assert_eq!(harness.effective_env("overrides")["OTHER_KEY"], "sk-secret");
}

#[tokio::test]
async fn the_test_endpoint_probes_models_and_reports_ok_then_401() {
    let harness = Harness::new();
    let base_url = spawn_models_server("sk-good");

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/providers",
            Some(json!({
                "name": "good",
                "kind": "openai-compatible",
                "base_url": base_url,
                "api_key_env": PROVIDER_KEY_ENV,
                "secret": "sk-good",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let good = parse(&body)["id"].as_str().expect("id").to_string();

    let (status, body) = harness
        .request(Method::POST, &format!("/api/providers/{good}/test"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcome = parse(&body);
    assert_eq!(outcome["ok"], true, "{outcome}");
    assert_eq!(outcome["status"], 200);
    assert!(outcome["latency_ms"].as_u64().is_some());
    assert!(!body.contains("sk-good"), "the key must never be echoed");

    // Same endpoint, wrong key: the provider answers 401, reported as such.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/providers",
            Some(json!({
                "name": "bad",
                "kind": "openai-compatible",
                "base_url": base_url,
                "api_key_env": PROVIDER_KEY_ENV,
                "secret": "sk-wrong",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let bad = parse(&body)["id"].as_str().expect("id").to_string();

    let (status, body) = harness
        .request(Method::POST, &format!("/api/providers/{bad}/test"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcome = parse(&body);
    assert_eq!(outcome["ok"], false);
    assert_eq!(outcome["status"], 401);
    assert!(outcome["error"]
        .as_str()
        .is_some_and(|text| text.contains("401")));
    assert!(!body.contains("sk-wrong"));
}

#[tokio::test]
async fn viewers_read_providers_but_cannot_mutate_them() {
    let harness = Harness::new();
    let provider = create_provider(&harness, "shared", "sk-secret").await;
    let provider_id = provider["id"].as_str().expect("id").to_string();

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/tokens",
            Some(json!({"name": "ci", "role": "viewer"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let viewer = parse(&body)["token"].as_str().expect("token").to_string();

    let (status, body) = harness
        .request_as(Method::GET, "/api/providers", None, &viewer)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for (method, uri) in [
        (Method::POST, "/api/providers".to_string()),
        (Method::PATCH, format!("/api/providers/{provider_id}")),
        (Method::DELETE, format!("/api/providers/{provider_id}")),
        (Method::POST, format!("/api/providers/{provider_id}/test")),
    ] {
        let (status, body) = harness
            .request_as(method.clone(), &uri, Some(json!({"name": "x"})), &viewer)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
    }
}

#[tokio::test]
async fn provider_mutations_are_audited() {
    let harness = Harness::new();
    let provider = create_provider(&harness, "shared", "sk-one").await;
    let provider_id = provider["id"].as_str().expect("id").to_string();
    create_instance_with_provider(&harness, "alice", &provider_id).await;

    let (status, body) = harness
        .request(
            Method::PATCH,
            &format!("/api/providers/{provider_id}"),
            Some(json!({ "secret": "sk-two" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = harness
        .request(
            Method::DELETE,
            &format!("/api/providers/{provider_id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Detach, delete, then read the trail.
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/alice",
            Some(json!({ "provider": null })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = harness
        .request(
            Method::DELETE,
            &format!("/api/providers/{provider_id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body) = harness.request(Method::GET, "/api/audit", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let actions: Vec<String> = parse(&body)
        .as_array()
        .expect("audit array")
        .iter()
        .filter(|entry| entry["target"] == provider_id.as_str())
        .map(|entry| entry["action"].as_str().unwrap_or_default().to_string())
        .collect();
    for expected in [
        "provider.create",
        "provider.update",
        "provider.rotate",
        "provider.delete",
    ] {
        assert!(
            actions.iter().any(|action| action == expected),
            "missing audit action {expected}: {actions:?}"
        );
    }

    // No audit detail ever carries the secret.
    let (status, body) = harness.request(Method::GET, "/api/audit", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("sk-one") && !body.contains("sk-two"),
        "{body}"
    );
}

#[tokio::test]
async fn unknown_providers_are_refused_on_create_and_patch() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances",
            Some(json!({
                "id": "alice",
                "template": "guest",
                "model": MODEL,
                "provider": "ghost",
                "mem_limit_mb": 512,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "unknown_provider");

    let provider = create_provider(&harness, "shared", "sk-secret").await;
    let provider_id = provider["id"].as_str().expect("id").to_string();
    create_instance_with_provider(&harness, "bob", &provider_id).await;
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/instances/bob",
            Some(json!({ "provider": "ghost" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "unknown_provider");
}

#[tokio::test]
async fn provider_names_are_unique_and_validated() {
    let harness = Harness::new();
    create_provider(&harness, "shared", "sk-one").await;

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/providers",
            Some(json!({
                "name": "shared",
                "kind": "openai-compatible",
                "base_url": PROVIDER_BASE_URL,
                "api_key_env": PROVIDER_KEY_ENV,
                "secret": "sk-two",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "provider_exists");

    for (body, code) in [
        (
            json!({
                "name": "Bad Name",
                "kind": "openai-compatible",
                "base_url": PROVIDER_BASE_URL,
                "secret": "sk",
            }),
            "invalid_name",
        ),
        (
            json!({
                "name": "kindless",
                "kind": "weird",
                "base_url": PROVIDER_BASE_URL,
                "secret": "sk",
            }),
            "invalid_kind",
        ),
        (
            json!({
                "name": "secretless",
                "kind": "openai-compatible",
                "base_url": PROVIDER_BASE_URL,
            }),
            "invalid_secret",
        ),
    ] {
        let (status, body) = harness
            .request(Method::POST, "/api/providers", Some(body))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(parse(&body)["error"]["code"], code);
    }
}

#[tokio::test]
async fn a_clone_inherits_the_provider_without_asking_for_a_token() {
    let harness = Harness::new();
    let provider = create_provider(&harness, "shared", "sk-secret").await;
    let provider_id = provider["id"].as_str().expect("id").to_string();
    create_instance_with_provider(&harness, "source", &provider_id).await;

    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/source/clone",
            Some(json!({"new_id": "copy"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let clone = parse(&body);
    assert_eq!(clone["provider"], provider_id.as_str());
    assert_eq!(clone["api_key_env"], PROVIDER_KEY_ENV);
    assert_eq!(
        harness.effective_env("copy")["PROVIDER_API_KEY"],
        "sk-secret"
    );

    // An explicit empty provider detaches the clone from the credential.
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/instances/source/clone",
            Some(json!({"new_id": "detached", "provider": ""})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(parse(&body)["provider"].is_null());
}

#[tokio::test]
async fn patching_metadata_without_a_secret_never_restarts_anything() {
    let harness = Harness::new();
    let provider = create_provider(&harness, "shared", "sk-secret").await;
    let provider_id = provider["id"].as_str().expect("id").to_string();
    create_instance_with_provider(&harness, "alice", &provider_id).await;

    harness.mock.clear_calls();
    let (status, body) = harness
        .request(
            Method::PATCH,
            &format!("/api/providers/{provider_id}"),
            Some(json!({"name": "renamed", "base_url": "https://other.test/v1"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let updated = parse(&body);
    assert_eq!(updated["provider"]["name"], "renamed");
    assert_eq!(updated["restarted"].as_array().expect("restarted").len(), 0);
    assert!(
        harness
            .mock
            .calls()
            .iter()
            .all(|call| call.operation != "up"),
        "a metadata patch must not recreate a container"
    );
}

#[tokio::test]
async fn storing_a_provider_secret_needs_the_secrets_key() {
    let harness = Harness::keyless();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/providers",
            Some(json!({
                "name": "shared",
                "kind": "openai-compatible",
                "base_url": PROVIDER_BASE_URL,
                "api_key_env": PROVIDER_KEY_ENV,
                "secret": "sk-secret",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "secrets_key_missing");
}
