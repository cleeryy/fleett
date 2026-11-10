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

/// Gateway simulator of `POST /api/providers/{id}/test`: records every request
/// and answers `GET /models` and `POST /chat/completions` independently.
///
/// Returns `(base_url, requests)`; each request is `(method, path, headers)`.
#[allow(clippy::type_complexity)]
fn spawn_gateway(
    models: (u16, &'static str),
    completion: (u16, &'static str),
) -> (
    String,
    Arc<std::sync::Mutex<Vec<(String, String, Vec<(String, String)>)>>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock gateway");
    let address = listener.local_addr().expect("local address");
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 8192];
            // Read the headers (and a possible body) of one request.
            let header_end = loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break 0;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            if header_end == 0 {
                continue;
            }
            let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let mut lines = headers.lines();
            let request_line = lines.next().unwrap_or_default();
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or_default().to_string();
            let path = parts.next().unwrap_or_default().to_string();
            let captured: Vec<(String, String)> = lines
                .filter_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    let value = value.trim();
                    if value.is_empty() {
                        return None;
                    }
                    Some((name.trim().to_ascii_lowercase(), value.to_string()))
                })
                .collect();
            recorded
                .lock()
                .expect("requests")
                .push((method.clone(), path.clone(), captured));
            let (status, body) = if method == "POST" { completion } else { models };
            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                404 => "Not Found",
                _ => "Error",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{address}"), requests)
}

fn request_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
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
async fn the_test_endpoint_probes_models_and_a_real_completion() {
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
    assert_eq!(outcome["models_ok"], true);
    assert_eq!(outcome["completion_ok"], true, "{outcome}");
    assert_eq!(outcome["models_status"], 200);
    assert_eq!(outcome["completion_status"], 200);
    assert!(outcome["latency_ms"].as_u64().is_some());
    assert!(outcome.get("error").is_none(), "{outcome}");
    assert!(!body.contains("sk-good"), "the key must never be echoed");

    // Same endpoint, wrong key: both halves answer 401, reported as such.
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
    assert_eq!(outcome["models_ok"], false);
    assert_eq!(outcome["completion_ok"], false);
    assert_eq!(outcome["models_status"], 401);
    assert_eq!(outcome["completion_status"], 401);
    assert!(outcome["error"]
        .as_str()
        .is_some_and(|text| text.contains("401")));
    assert!(!body.contains("sk-wrong"));
}

#[tokio::test]
async fn the_test_endpoint_turns_red_when_only_the_completion_fails() {
    // The case the issue describes: /models answers 200, the completion route
    // rejects the request (a gateway that requires a session header).
    let harness = Harness::new();
    let (base_url, requests) = spawn_gateway(
        (200, r#"{"data":[]}"#),
        (
            400,
            r#"{"type":"error","error":{"type":"MissingSessionID","message":"Request is missing x-opencode-session"}}"#,
        ),
    );
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/providers",
            Some(json!({
                "name": "gateway",
                "kind": "openai-compatible",
                "base_url": base_url,
                "api_key_env": PROVIDER_KEY_ENV,
                "secret": "sk-gateway",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = parse(&body)["id"].as_str().expect("id").to_string();

    let (status, body) = harness
        .request(Method::POST, &format!("/api/providers/{id}/test"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcome = parse(&body);
    assert_eq!(outcome["ok"], false, "{outcome}");
    assert_eq!(outcome["models_ok"], true, "{outcome}");
    assert_eq!(outcome["completion_ok"], false, "{outcome}");
    assert_eq!(outcome["models_status"], 200);
    assert_eq!(outcome["completion_status"], 400);
    assert!(
        outcome["error"]
            .as_str()
            .is_some_and(|text| text.contains("MissingSessionID")),
        "the upstream reason must reach the UI: {outcome}"
    );
    assert!(!body.contains("sk-gateway"));

    // Two calls were made: the metadata one and the completion one.
    let recorded = requests.lock().expect("requests").clone();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert_eq!(recorded[0].0, "GET");
    assert_eq!(recorded[0].1, "/models");
    assert_eq!(recorded[1].0, "POST");
    assert_eq!(recorded[1].1, "/chat/completions");
    assert_eq!(
        recorded[1]
            .2
            .iter()
            .find(|(name, _)| name == "authorization")
            .map(|(_, value)| value.as_str()),
        Some("Bearer sk-gateway")
    );
}

#[tokio::test]
async fn the_test_endpoint_sends_the_extra_headers_and_the_opencode_session() {
    let harness = Harness::new();
    let (base_url, requests) = spawn_gateway((200, r#"{"data":[]}"#), (200, r#"{"id":"x"}"#));
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/providers",
            Some(json!({
                "name": "gateway",
                "kind": "openai-compatible",
                "base_url": base_url,
                "api_key_env": PROVIDER_KEY_ENV,
                "secret": "sk-gateway",
                "model": "provider-model",
                "extra_headers": {"x-api-version": "2026-01-01"},
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = parse(&body)["id"].as_str().expect("id").to_string();

    let (status, body) = harness
        .request(Method::POST, &format!("/api/providers/{id}/test"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["completion_ok"], true, "{body}");

    let recorded = requests.lock().expect("requests").clone();
    assert_eq!(recorded.len(), 2);
    for (method, path, headers) in &recorded {
        assert_eq!(
            request_header(headers, "x-api-version"),
            Some("2026-01-01"),
            "{method} {path} did not carry the configured header: {headers:?}"
        );
        assert!(request_header(headers, "authorization").is_some());
    }
    // The completion half uses the provider default model and one token.
    let (_, _, completion_headers) = &recorded[1];
    assert!(request_header(completion_headers, "content-type").is_some());
    // A local host is not the opencode gateway: no session header.
    assert_eq!(
        request_header(completion_headers, "x-opencode-session"),
        None
    );

    // `?model=` overrides the provider default for the test only.
    let (status, body) = harness
        .request(
            Method::POST,
            &format!("/api/providers/{id}/test?model=other-model"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["completion_ok"], true);
}

#[tokio::test]
async fn provider_headers_are_validated_encrypted_and_never_returned() {
    let harness = Harness::new();
    let (status, body) = harness
        .request(
            Method::POST,
            "/api/providers",
            Some(json!({
                "name": "gateway",
                "kind": "openai-compatible",
                "base_url": PROVIDER_BASE_URL,
                "api_key_env": PROVIDER_KEY_ENV,
                "secret": "sk-gateway",
                "model": "provider-model",
                "extra_headers": {
                    "x-opencode-session": "session-42",
                    "anthropic-version": "2023-06-01",
                },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let summary = parse(&body);
    let id = summary["id"].as_str().expect("id").to_string();
    assert_eq!(summary["model"], "provider-model");
    assert_eq!(
        summary["extra_headers"],
        json!(["anthropic-version", "x-opencode-session"]),
        "the summary exposes names only"
    );
    assert!(!body.contains("session-42"), "{body}");
    assert!(!body.contains("2023-06-01"), "{body}");

    // Nothing returns a value: list, detail and the later patch.
    for path in ["/api/providers".to_string(), format!("/api/providers/{id}")] {
        let (status, body) = harness.request(Method::GET, &path, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains("session-42"), "{path}: {body}");
        assert!(!body.contains("2023-06-01"), "{path}: {body}");
    }

    // The values are encrypted at rest, with the fleet key.
    let sealed = harness
        .state
        .db
        .provider_headers(&id)
        .expect("blob")
        .expect("present");
    let blob = String::from_utf8_lossy(&sealed.ciphertext).to_string();
    assert!(!blob.contains("session-42"), "{blob}");
    assert!(!blob.contains("2023-06-01"), "{blob}");

    // PATCH replaces the whole map and never restarts an instance.
    let (status, body) = harness
        .request(
            Method::PATCH,
            &format!("/api/providers/{id}"),
            Some(json!({"extra_headers": {"x-api-version": "2026-01-01"}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        parse(&body)["provider"]["extra_headers"],
        json!(["x-api-version"])
    );
    assert!(parse(&body)["restarted"]
        .as_array()
        .expect("restarted")
        .is_empty());

    // An empty object removes every header; a blank model clears the default.
    let (status, body) = harness
        .request(
            Method::PATCH,
            &format!("/api/providers/{id}"),
            Some(json!({"extra_headers": {}, "model": ""})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let provider = parse(&body)["provider"].clone();
    assert_eq!(provider["extra_headers"], json!([]), "{provider}");
    assert_eq!(provider["model"], Value::Null, "{provider}");

    // Validation: RFC token names, at most 32 headers, no control character.
    for extra_headers in [
        json!({"bad name": "x"}),
        json!({"x": "a\r\nb"}),
        json!((0..=32)
            .map(|index| (format!("x-h{index}"), "1".to_string()))
            .collect::<std::collections::BTreeMap<_, _>>()),
    ] {
        let (status, body) = harness
            .request(
                Method::POST,
                "/api/providers",
                Some(json!({
                    "name": format!("invalid-{}", rand_suffix()),
                    "kind": "openai-compatible",
                    "base_url": PROVIDER_BASE_URL,
                    "api_key_env": PROVIDER_KEY_ENV,
                    "secret": "sk-invalid",
                    "extra_headers": extra_headers,
                })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(parse(&body)["error"]["code"], "invalid_provider", "{body}");
    }

    // A duplicate name would fail the insert first: the errors above are about
    // the headers, so the provider list still holds exactly one provider.
    let (status, body) = harness.request(Method::GET, "/api/providers", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body).as_array().expect("list").len(), 1, "{body}");
}

/// A short unique suffix so a refusal test never trips on a duplicate name.
fn rand_suffix() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos().to_string())
        .unwrap_or_else(|_| "0".to_string())
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
