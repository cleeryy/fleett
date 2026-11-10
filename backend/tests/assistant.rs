//! Integration tests for the integrated assistant (issue #56).
//!
//! Every test runs against `MockDriver`, and the provider is a local
//! `std::net` HTTP server: no real model is ever called, no real secret is
//! used.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::driver::mock::MockDriver;
use fleett::models::{now_iso, ProviderSpec};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const SECRETS_KEY_BYTES: [u8; 32] = [7u8; 32];
const PROVIDER_ID: &str = "11111111-2222-3333-4444-555555555555";
const PROVIDER_SECRET: &str = "sk-test-provider-secret-0123456789";
const MODEL: &str = "test-model";
const ASSISTANT_HEADER: &str = "x-fleett-assistant";

/// One captured provider call.
#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Value,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn header_count(&self, name: &str) -> usize {
        self.headers.iter().filter(|(key, _)| key == name).count()
    }
}

/// Local OpenAI-compatible provider: records every request and answers with
/// the payload configured by the test.
#[derive(Clone)]
struct MockProvider {
    base_url: String,
    requests: Arc<Mutex<Vec<Captured>>>,
    response: Arc<Mutex<(u16, String)>>,
}

impl MockProvider {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock provider");
        let address = listener.local_addr().expect("local address");
        let requests: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
        let response: Arc<Mutex<(u16, String)>> = Arc::new(Mutex::new((
            200,
            json!({
                "choices": [{"message": {"role": "assistant", "content": "{\"message\":\"ok\"}"}}],
                "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
            })
            .to_string(),
        )));
        let recorded = requests.clone();
        let answered = response.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let Some((headers, body)) = read_request(&mut stream) else {
                    continue;
                };
                let mut request_line = headers
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split_whitespace();
                let method = request_line.next().unwrap_or_default().to_string();
                let path = request_line.next().unwrap_or_default().to_string();
                let captured_headers: Vec<(String, String)> = headers
                    .lines()
                    .skip(1)
                    .filter_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        let value = value.trim();
                        if value.is_empty() {
                            return None;
                        }
                        Some((name.trim().to_ascii_lowercase(), value.to_string()))
                    })
                    .collect();
                recorded.lock().expect("requests").push(Captured {
                    method,
                    path,
                    headers: captured_headers,
                    body: serde_json::from_str(&body).unwrap_or(Value::Null),
                });
                let (status, body) = answered.lock().expect("response").clone();
                let reason = if status == 200 { "OK" } else { "Error" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        Self {
            base_url: format!("http://{address}/v1"),
            requests,
            response,
        }
    }

    fn answer_with(&self, content: &str, prompt_tokens: u64, completion_tokens: u64) {
        let body = json!({
            "choices": [{"message": {"role": "assistant", "content": content}}],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens + completion_tokens,
            },
        });
        *self.response.lock().expect("response") = (200, body.to_string());
    }

    /// Answer every request with a non-2xx status and an upstream body.
    fn fail_with(&self, status: u16, body: &str) {
        *self.response.lock().expect("response") = (status, body.to_string());
    }

    fn requests(&self) -> Vec<Captured> {
        self.requests.lock().expect("requests").clone()
    }

    fn last(&self) -> Captured {
        self.requests().pop().expect("one provider request")
    }
}

/// Read one HTTP/1.1 request completely (headers and body).
fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, String)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while buffer.len() < header_end + length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&buffer[header_end..]).to_string();
    Some((headers, body))
}

struct Harness {
    _dir: tempfile::TempDir,
    state: AppState,
    provider: MockProvider,
}

impl Harness {
    /// Harness with a running mock provider and an attached assistant provider.
    fn new() -> Self {
        Self::build(true, Some(MODEL.to_string()), 50_000)
    }

    /// Harness without `FLEETT_ASSISTANT_PROVIDER`.
    fn unconfigured() -> Self {
        Self::build(false, Some(MODEL.to_string()), 50_000)
    }

    /// Harness with the assistant disabled.
    fn disabled() -> Self {
        Self::build_with(false, Some(MODEL.to_string()), 50_000, false)
    }

    fn with_budget(budget: u64) -> Self {
        Self::build(true, Some(MODEL.to_string()), budget)
    }

    fn build(attach_provider: bool, model: Option<String>, budget: u64) -> Self {
        Self::build_with(attach_provider, model, budget, true)
    }

    fn build_with(
        attach_provider: bool,
        model: Option<String>,
        budget: u64,
        enabled: bool,
    ) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        cfg.assistant_enabled = enabled;
        cfg.assistant_provider = attach_provider.then(|| PROVIDER_ID.to_string());
        cfg.assistant_model = model.unwrap_or_else(|| MODEL.to_string());
        cfg.assistant_budget_tokens = budget;
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        let provider = MockProvider::spawn();
        if attach_provider {
            attach(&state, &provider.base_url);
        }
        Self {
            _dir: dir,
            state,
            provider,
        }
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        self.request_with(method, uri, body, &[]).await
    }

    async fn request_with(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, String) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
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
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    /// Same as [`Harness::request`] with another credential (a viewer token).
    async fn request_as(
        &self,
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
        let response = api::router(self.state.clone())
            .oneshot(builder.body(body).expect("valid request"))
            .await
            .expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    async fn chat(&self, body: Value) -> (StatusCode, Value) {
        let (status, body) = self
            .request(Method::POST, "/api/assistant/chat", Some(body))
            .await;
        (status, parse(&body))
    }

    async fn create_instance(&self, id: &str) {
        let (status, body) = self
            .request(
                Method::POST,
                "/api/instances",
                Some(json!({
                    "id": id,
                    "name": format!("Agent {id}"),
                    "template": "guest",
                    "model": MODEL,
                    "base_url": "https://provider.test/v1",
                    "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
                    "shares": [],
                    "mem_limit_mb": 1024,
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "create instance: {body}");
    }

    async fn audit(&self, action: &str) -> Vec<Value> {
        let (status, body) = self
            .request(
                Method::GET,
                &format!("/api/audit?limit=50&action={action}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "audit: {body}");
        parse(&body).as_array().expect("audit array").to_vec()
    }
}

/// Insert one more provider the assistant can resolve.
fn attach_as(state: &AppState, id: &str, name: &str, base_url: &str) {
    let sealed = fleett::providers::seal(&state.cfg, PROVIDER_SECRET).expect("seal");
    let now = now_iso();
    let spec = ProviderSpec {
        id: id.to_string(),
        name: name.to_string(),
        kind: "openai-compatible".to_string(),
        base_url: base_url.to_string(),
        api_key_env: "PROVIDER_API_KEY".to_string(),
        model: None,
        header_names: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    state
        .db
        .insert_provider(&spec, &sealed.nonce, &sealed.ciphertext)
        .expect("insert provider");
}

/// Insert the provider row the assistant reads at request time.
fn attach(state: &AppState, base_url: &str) {
    let sealed = fleett::providers::seal(&state.cfg, PROVIDER_SECRET).expect("seal");
    let now = now_iso();
    let spec = ProviderSpec {
        id: PROVIDER_ID.to_string(),
        name: "assistant-provider".to_string(),
        kind: "openai-compatible".to_string(),
        base_url: base_url.to_string(),
        api_key_env: "PROVIDER_API_KEY".to_string(),
        model: None,
        header_names: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    state
        .db
        .insert_provider(&spec, &sealed.nonce, &sealed.ciphertext)
        .expect("insert provider");
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

fn chat_body(session: &str, task: &str, context: Value) -> Value {
    json!({
        "session_id": session,
        "context": context,
        "messages": [{"role": "user", "content": "write me a SKILL.md"}],
        "task": task,
    })
}

#[tokio::test]
async fn config_reports_the_defaults_and_off_removes_every_route() {
    let harness = Harness::unconfigured();
    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = parse(&body);
    assert_eq!(config["enabled"], true);
    assert!(config["provider"].is_null(), "{config}");
    assert_eq!(config["provider_source"], "none", "{config}");
    assert_eq!(config["reason"], "no_provider", "{config}");
    assert_eq!(config["model"], MODEL);
    assert_eq!(config["budget_tokens"], 50_000);
    assert_eq!(config["used_tokens"], 0);

    let disabled = Harness::disabled();
    for (method, uri) in [
        (Method::GET, "/api/assistant/config"),
        (Method::POST, "/api/assistant/chat"),
    ] {
        let (status, body) = disabled.request(method, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
        assert_eq!(parse(&body)["error"]["code"], "not_found");
    }
}

#[tokio::test]
async fn chat_without_a_provider_answers_the_setup_hint() {
    let harness = Harness::unconfigured();
    let (status, body) = harness
        .chat(chat_body("s1", "skill", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = body["error"].clone();
    assert_eq!(error["code"], "assistant_not_configured");
    assert_eq!(error["details"]["env"], "FLEETT_ASSISTANT_PROVIDER");
    assert_eq!(error["details"]["providers"], "/api/providers");
    assert!(harness.provider.requests().is_empty());
}

#[tokio::test]
async fn the_console_selects_the_assistant_provider_and_the_env_still_wins() {
    let harness = Harness::unconfigured();

    // One provider, no selection: auto.
    attach(&harness.state, &harness.provider.base_url);
    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = parse(&body);
    assert_eq!(config["provider_source"], "auto", "{config}");
    assert_eq!(config["provider"]["id"], PROVIDER_ID);
    assert!(config["reason"].is_null(), "{config}");

    // A second provider makes the choice explicit instead of guessing.
    attach_as(
        &harness.state,
        "second-provider",
        "second",
        "https://second.test/v1",
    );
    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = parse(&body);
    assert!(config["provider"].is_null(), "{config}");
    assert_eq!(config["provider_source"], "none");
    assert_eq!(config["reason"], "not_selected");
    // The chat route says the same thing, with the actionable hint.
    let (status, body) = harness
        .chat(chat_body("select", "freeform", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "assistant_not_configured");
    assert_eq!(body["error"]["details"]["reason"], "not_selected");
    assert_eq!(
        body["error"]["details"]["configure"],
        "/api/assistant/config"
    );

    // A selection persists and applies immediately.
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/assistant/config",
            Some(json!({"provider": PROVIDER_ID})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = parse(&body);
    assert_eq!(config["provider_source"], "setting", "{config}");
    assert_eq!(config["provider"]["id"], PROVIDER_ID, "{config}");
    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["provider_source"], "setting");

    // The selection really answers: the assistant calls that provider.
    let (status, body) = harness
        .chat(chat_body("selected", "freeform", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(harness.provider.requests().len(), 1);

    // An unknown id is refused with the valid ids, never silently stored.
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/assistant/config",
            Some(json!({"provider": "ghost"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let refusal = parse(&body);
    assert_eq!(
        refusal["error"]["code"], "assistant_provider_unknown",
        "{body}"
    );
    let valid = refusal["error"]["details"]["valid"]
        .as_array()
        .expect("valid");
    assert!(valid.iter().any(|id| id == PROVIDER_ID), "{valid:?}");
    assert!(
        !valid.iter().any(|id| id == "ghost"),
        "the refused id must not be reported as valid: {valid:?}"
    );
    // An absent field is refused too.
    let (status, body) = harness
        .request(Method::PATCH, "/api/assistant/config", Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_provider");

    // `null` clears the selection and the fleet falls back to not_selected.
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/assistant/config",
            Some(json!({"provider": null})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = parse(&body);
    assert!(config["provider"].is_null(), "{config}");
    assert_eq!(config["reason"], "not_selected", "{config}");

    // The env variable keeps priority over the persisted setting.
    let env = Harness::new();
    attach_as(
        &env.state,
        "second-provider",
        "second",
        "https://second.test/v1",
    );
    let (status, body) = env
        .request(
            Method::PATCH,
            "/api/assistant/config",
            Some(json!({"provider": "second-provider"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = parse(&body);
    assert_eq!(config["provider_source"], "env", "{config}");
    assert_eq!(config["provider"]["id"], PROVIDER_ID, "{config}");
}

#[tokio::test]
async fn only_an_admin_selects_the_provider() {
    let harness = Harness::unconfigured();
    attach(&harness.state, &harness.provider.base_url);
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

    let (status, body) = harness
        .request_as(
            Method::PATCH,
            "/api/assistant/config",
            Some(json!({"provider": PROVIDER_ID})),
            &token,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "forbidden");
    // A viewer still reads the configuration.
    let (status, body) = harness
        .request_as(Method::GET, "/api/assistant/config", None, &token)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["provider_source"], "auto");
}

#[tokio::test]
async fn a_dangling_env_reference_is_a_422_with_the_valid_ids() {
    let harness = Harness::new();
    harness
        .state
        .db
        .delete_provider(PROVIDER_ID)
        .expect("delete provider");

    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let refusal = parse(&body);
    let error = refusal["error"].clone();
    assert_eq!(error["code"], "assistant_provider_unknown");
    assert_eq!(error["details"]["source"], "env");
    assert_eq!(error["details"]["provider"], PROVIDER_ID);
    assert_eq!(error["details"]["valid"], json!([]));

    // The chat route refuses instead of pretending the fleet is empty.
    let (status, body) = harness
        .chat(chat_body("dangling", "freeform", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "assistant_provider_unknown");
    assert!(harness.provider.requests().is_empty());
    // Never a 500 and never a silent null.
    assert_ne!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn chat_calls_the_provider_with_the_contract_shape() {
    let harness = Harness::new();
    harness.create_instance("alice").await;
    harness
        .provider
        .answer_with(
            r#"{"message":"Here is the draft.","proposal":{"kind":"file","path":"/SKILL.md","content":"---\nname: invoices\ndescription: Sort invoices in a folder\n---\n1. Read.\n"}}"#,
            120,
            80,
        );
    let (status, body) = harness
        .chat(chat_body(
            "session-a",
            "skill",
            json!({
                "page": "files",
                "instance": "alice",
                "path": "/SKILL.md",
                "selection": "old step",
                "file_content": "---\nname: x\n---\n",
                "schema": "skills: [{name, description}]",
            }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "Here is the draft.");
    assert_eq!(body["proposal"]["kind"], "file");
    assert_eq!(body["proposal"]["path"], "/SKILL.md");
    assert!(body["proposal"]["content"]
        .as_str()
        .expect("content")
        .contains("name: invoices"));
    assert_eq!(
        body["usage"],
        json!({"prompt": 120, "completion": 80, "total": 200})
    );
    let sent = body["redaction"]["sent"].as_array().expect("sent");
    assert!(sent.iter().any(|entry| entry == "page: files"), "{sent:?}");

    let request = harness.provider.last();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {PROVIDER_SECRET}").as_str())
    );
    assert_eq!(request.body["model"], MODEL);
    assert_eq!(request.body["temperature"], 0.2);
    assert_eq!(request.body["max_tokens"], 1200);
    let messages = request.body["messages"].as_array().expect("messages");
    assert_eq!(messages[0]["role"], "system");
    let system = messages[0]["content"].as_str().expect("system text");
    assert!(system.contains("Current task (skill)"), "{system}");
    assert!(system.contains("Page: files"), "{system}");
    assert!(system.contains("## file_content"), "{system}");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "write me a SKILL.md");
    assert!(
        !request.body.to_string().contains(PROVIDER_SECRET),
        "the provider secret must stay in the header"
    );
}

#[tokio::test]
async fn the_provider_headers_and_default_model_are_used() {
    let harness = Harness::new();

    // Configure the provider the way the console does: headers (encrypted at
    // rest, names only in the API) and a default model.
    let (status, body) = harness
        .request(
            Method::PATCH,
            &format!("/api/providers/{PROVIDER_ID}"),
            Some(json!({
                "model": "provider-model",
                "extra_headers": {
                    "x-opencode-session": "conversation-42",
                    "anthropic-version": "2023-06-01",
                },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let updated = parse(&body)["provider"].clone();
    assert_eq!(updated["model"], "provider-model", "{updated}");
    assert_eq!(
        updated["extra_headers"],
        json!(["anthropic-version", "x-opencode-session"]),
        "the API exposes header names only"
    );
    assert!(
        !body.contains("conversation-42"),
        "a header value must never be returned: {body}"
    );

    // The default model of the provider wins over FLEETT_ASSISTANT_MODEL, and
    // config reports it.
    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["model"], "provider-model");

    let (status, body) = harness
        .chat(chat_body("headers", "freeform", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let request = harness.provider.last();
    assert_eq!(request.body["model"], "provider-model");
    assert_eq!(
        request.header("x-opencode-session"),
        Some("conversation-42"),
        "the configured header must be sent: {request:?}"
    );
    assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(request.header_count("x-opencode-session"), 1);
    // The credential still wins over a per-call default.
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {PROVIDER_SECRET}").as_str())
    );
}

#[tokio::test]
async fn the_opencode_session_header_is_stable_for_one_conversation() {
    // A provider whose host is the gateway: the header is added automatically
    // and derived from the assistant session id. The URL is never called here
    // (prepare only), so no name resolution is involved.
    let provider = ProviderSpec {
        id: "opencode-provider".to_string(),
        name: "opencode-go".to_string(),
        kind: "openai-compatible".to_string(),
        base_url: "https://opencode.ai/inference/go/openai/v1".to_string(),
        api_key_env: "PROVIDER_API_KEY".to_string(),
        model: None,
        header_names: Vec::new(),
        created_at: now_iso(),
        updated_at: now_iso(),
    };
    let headers = std::collections::BTreeMap::new();
    let call = fleett::assistant::prepare_chat(
        &fleett::assistant::ChatTarget {
            provider: &provider,
            secret: "sk-secret",
            headers: &headers,
            model: "deepseek-v4.1-flash",
            session_id: "panel 7f/3c",
        },
        "system",
        &[],
    );
    let session = |call: &fleett::assistant::PreparedCall| {
        call.headers
            .iter()
            .find(|(name, _)| name == "x-opencode-session")
            .map(|(_, value)| value.clone())
    };
    assert_eq!(session(&call).as_deref(), Some("panel-7f-3c"));

    // The same conversation id always yields the same header; a different one
    // changes it.
    let same = fleett::assistant::prepare_chat(
        &fleett::assistant::ChatTarget {
            provider: &provider,
            secret: "sk-secret",
            headers: &headers,
            model: "deepseek-v4.1-flash",
            session_id: "panel 7f/3c",
        },
        "system",
        &[],
    );
    assert_eq!(session(&same), session(&call));
    let other = fleett::assistant::prepare_chat(
        &fleett::assistant::ChatTarget {
            provider: &provider,
            secret: "sk-secret",
            headers: &headers,
            model: "deepseek-v4.1-flash",
            session_id: "panel-other",
        },
        "system",
        &[],
    );
    assert_ne!(session(&other), session(&call));

    // An explicit header configured by the operator is never duplicated.
    let explicit = std::collections::BTreeMap::from([(
        "x-opencode-session".to_string(),
        "fixed-by-operator".to_string(),
    )]);
    let call = fleett::assistant::prepare_chat(
        &fleett::assistant::ChatTarget {
            provider: &provider,
            secret: "sk-secret",
            headers: &explicit,
            model: "deepseek-v4.1-flash",
            session_id: "panel 7f/3c",
        },
        "system",
        &[],
    );
    assert_eq!(session(&call).as_deref(), Some("fixed-by-operator"));
    assert_eq!(
        call.headers
            .iter()
            .filter(|(name, _)| name == "x-opencode-session")
            .count(),
        1
    );
}

#[tokio::test]
async fn an_upstream_error_is_reported_in_the_502_and_redacted() {
    let harness = Harness::new();
    harness.provider.fail_with(
        400,
        r#"{"type":"error","error":{"type":"MissingSessionID","message":"Request is missing x-opencode-session"},"key":"sk-leaked-upstream-value"}"#,
    );

    let (status, body) = harness
        .chat(chat_body("upstream", "freeform", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let error = body["error"].clone();
    assert_eq!(error["code"], "assistant_provider_error");
    assert_eq!(error["details"]["provider_status"], 400);
    assert_eq!(error["details"]["upstream"]["status"], 400);
    let upstream = error["details"]["upstream"]["body"]
        .as_str()
        .expect("upstream body");
    assert!(upstream.contains("MissingSessionID"), "{upstream}");
    // The gateway body is quoted, the credential never is.
    assert!(!body.to_string().contains(PROVIDER_SECRET), "{body}");
    assert!(!upstream.contains("sk-leaked-upstream-value"), "{upstream}");
    assert!(upstream.contains("[REDACTED]"), "{upstream}");
}

#[tokio::test]
async fn a_large_upstream_error_is_truncated() {
    let harness = Harness::new();
    let body = format!(
        "{{\"error\":\"boom\",\"padding\":\"{}\"}}",
        "x".repeat(8 * 1024)
    );
    harness.provider.fail_with(500, &body);

    let (status, body) = harness
        .chat(chat_body("truncate", "freeform", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let upstream = body["error"]["details"]["upstream"]["body"]
        .as_str()
        .expect("upstream body");
    assert!(upstream.contains("boom"));
    assert!(
        upstream.len() <= 1024 + 4,
        "the upstream body is not capped: {} bytes",
        upstream.len()
    );
    assert!(
        body.to_string().len() < 4 * 1024,
        "the error response is not capped"
    );
}

#[tokio::test]
async fn known_secrets_never_reach_the_provider() {
    let harness = Harness::new();
    harness.create_instance("alice").await;
    let discord_token = "MTIzNDU2Nzg5MDEyMzQ1Njc4OTAuYWJjZGVm.ghijklmnopqrstuvwxyz0123456789";
    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [
                {"key": "DISCORD_ALLOWED_USERS", "value": "123,456", "secret": false},
                {"key": "DISCORD_BOT_TOKEN", "value": discord_token, "secret": true},
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "env: {body}");

    let (status, body) = harness
        .chat(chat_body(
            "redaction",
            "env",
            json!({
                "page": "env",
                "instance": "alice",
                "path": "/.env",
                "file_content": format!("DISCORD_BOT_TOKEN={discord_token}\nDISCORD_ALLOWED_USERS=123"),
                "selection": "token sk-abcdefgh12345678 and Authorization: Bearer header.payload",
                "schema": "{\"key\":\"DISCORD_BOT_TOKEN\",\"secret\":true}",
            }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let request = harness.provider.last();
    let payload = request.body.to_string();
    assert!(
        !payload.contains(discord_token),
        "the secret env value left the process"
    );
    assert!(!payload.contains("sk-abcdefgh12345678"), "{payload}");
    assert!(!payload.contains("header.payload"), "{payload}");
    assert!(!payload.contains(PROVIDER_SECRET), "{payload}");
    assert!(payload.contains("[REDACTED]"), "{payload}");
    assert!(
        body["redaction"]["redacted"].as_u64().unwrap_or(0) >= 3,
        "{}",
        body["redaction"]
    );
    let omitted = body["redaction"]["omitted"].as_array().expect("omitted");
    assert!(
        omitted
            .iter()
            .any(|entry| entry.as_str().unwrap_or("").contains("DISCORD_BOT_TOKEN")),
        "{omitted:?}"
    );
}

#[tokio::test]
async fn oversized_and_binary_context_is_capped() {
    let harness = Harness::new();
    let mut file_content = "a".repeat(40 * 1024);
    file_content.push_str("TAIL_MARKER");
    let (status, body) = harness
        .chat(chat_body(
            "caps",
            "freeform",
            json!({
                "page": "files",
                "path": "/BIG.md",
                "file_content": file_content,
                "selection": "b".repeat(20 * 1024),
                "schema": "c".repeat(20 * 1024),
            }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let request = harness.provider.last();
    let payload = request.body.to_string();
    assert!(!payload.contains("TAIL_MARKER"), "the tail was sent");
    assert!(
        payload.len() < 70 * 1024,
        "the payload is not capped: {} bytes",
        payload.len()
    );
    let omitted = body["redaction"]["omitted"].as_array().expect("omitted");
    for field in [
        "file_content: truncated to 32768 bytes",
        "selection: truncated to 8192 bytes",
    ] {
        assert!(
            omitted
                .iter()
                .any(|entry| entry.as_str().unwrap_or("").starts_with(field)),
            "{field}: {omitted:?}"
        );
    }

    // Binary content is never sent, not even truncated.
    let (status, body) = harness
        .chat(chat_body(
            "binary",
            "freeform",
            json!({"page": "files", "file_content": "abc\u{0}\u{1}\u{2}def"}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let payload = harness.provider.last().body.to_string();
    assert!(!payload.contains("abc"), "{payload}");
    assert!(body["redaction"]["omitted"]
        .as_array()
        .expect("omitted")
        .iter()
        .any(|entry| entry
            .as_str()
            .unwrap_or("")
            .contains("binary content withheld")));
}

#[tokio::test]
async fn the_budget_is_enforced_per_session_and_zero_disables_it() {
    let harness = Harness::with_budget(50_000);
    harness
        .provider
        .answer_with("{\"message\":\"ok\"}", 48_000, 1_000);

    let (status, body) = harness
        .chat(chat_body("budget-a", "freeform", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["usage"]["total"], 49_000);

    let (status, body) = harness
        .chat(chat_body("budget-a", "freeform", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    let error = body["error"].clone();
    assert_eq!(error["code"], "assistant_budget_exceeded");
    assert_eq!(error["details"]["budget_tokens"], 50_000);
    assert_eq!(error["details"]["used_tokens"], 49_000);
    assert_eq!(harness.provider.requests().len(), 1, "no extra paid call");

    // The counter is per session: another id starts from zero.
    let (status, body) = harness
        .chat(chat_body("budget-b", "freeform", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = harness
        .request(
            Method::GET,
            "/api/assistant/config?session_id=budget-a",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["used_tokens"], 49_000);

    // `0` disables the budget entirely.
    let unlimited = Harness::with_budget(0);
    unlimited
        .provider
        .answer_with("{\"message\":\"ok\"}", 48_000, 1_000);
    for _ in 0..2 {
        let (status, body) = unlimited
            .chat(chat_body("anything", "freeform", json!({"page": "files"})))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(unlimited.provider.requests().len(), 2);
}

#[tokio::test]
async fn plain_text_answers_fall_back_to_the_message() {
    let harness = Harness::new();
    harness
        .provider
        .answer_with("Sorry, the log says the token expired.", 30, 10);
    let (status, body) = harness
        .chat(chat_body("plain", "explain", json!({"page": "instances"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "Sorry, the log says the token expired.");
    assert!(body["proposal"].is_null(), "{body}");
}

#[tokio::test]
async fn the_audit_row_carries_the_task_model_and_token_counts_only() {
    let harness = Harness::new();
    harness
        .provider
        .answer_with("{\"message\":\"draft ready\"}", 40, 20);
    let (status, body) = harness
        .chat(chat_body(
            "audit",
            "soul",
            json!({"page": "files", "instance": null}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let rows = harness.audit("assistant.chat").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let details = &rows[0]["details"];
    assert_eq!(details["task"], "soul");
    assert_eq!(details["model"], MODEL);
    assert_eq!(details["prompt_tokens"], 40);
    assert_eq!(details["completion_tokens"], 20);
    assert_eq!(details["total_tokens"], 60);
    let raw = rows[0].to_string();
    assert!(
        !raw.contains("write me a SKILL.md"),
        "the prompt must never be audited: {raw}"
    );
}

#[tokio::test]
async fn applying_a_proposal_marks_the_write_in_the_audit() {
    let harness = Harness::new();
    harness.create_instance("alice").await;

    let write = json!({"path": "/SKILL.md", "content": "---\nname: invoices\n---\n"});
    let (status, body) = harness
        .request_with(
            Method::PUT,
            "/api/instances/alice/files/content",
            Some(write.clone()),
            &[(ASSISTANT_HEADER, "1")],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "assistant write: {body}");

    let (status, body) = harness
        .request(
            Method::PUT,
            "/api/instances/alice/files/content",
            Some(json!({"path": "/MANUAL.md", "content": "hand written"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "manual write: {body}");

    let rows = harness.audit("instance.file.write").await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[0]["details"]["via"].is_null(),
        "a manual write has no assistant tag: {}",
        rows[0]
    );
    assert_eq!(rows[1]["details"]["via"], "assistant", "{}", rows[1]);

    // The env route carries the same tag.
    let (status, body) = harness
        .request_with(
            Method::PUT,
            "/api/instances/alice/env",
            Some(json!({"env": [{"key": "MODE", "value": "prod", "secret": false}]})),
            &[(ASSISTANT_HEADER, "1")],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "env write: {body}");
    let rows = harness.audit("instance.env.update").await;
    assert_eq!(rows[0]["details"]["via"], "assistant", "{}", rows[0]);
}
