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
    authorization: Option<String>,
    body: Value,
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
                let authorization = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("authorization")
                        .then(|| value.trim().to_string())
                });
                recorded.lock().expect("requests").push(Captured {
                    method,
                    path,
                    authorization,
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

    /// Harness whose `FLEETT_ASSISTANT_PROVIDER` names an id that does not
    /// exist (a deleted provider).
    fn with_env_provider(id: &str) -> Self {
        Self::build_full(
            Some(id.to_string()),
            Some(MODEL.to_string()),
            50_000,
            true,
            false,
        )
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
        Self::build_full(
            attach_provider.then(|| PROVIDER_ID.to_string()),
            model,
            budget,
            enabled,
            attach_provider,
        )
    }

    fn build_full(
        env_provider: Option<String>,
        model: Option<String>,
        budget: u64,
        enabled: bool,
        with_provider: bool,
    ) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        cfg.secrets_key = Some(fleett::secrets::encode_key(&SECRETS_KEY_BYTES));
        cfg.assistant_enabled = enabled;
        cfg.assistant_provider = env_provider;
        cfg.assistant_model = model.unwrap_or_else(|| MODEL.to_string());
        cfg.assistant_budget_tokens = budget;
        let mock = Arc::new(MockDriver::new());
        let state = AppState::bootstrap(cfg, mock.clone()).expect("bootstrap state");
        let provider = MockProvider::spawn();
        if with_provider {
            attach(&state, &provider.base_url);
        }
        Self {
            _dir: dir,
            state,
            provider,
        }
    }

    /// Insert one more provider row, pointing at the same mock upstream.
    fn add_provider(&self, id: &str, name: &str) {
        insert_provider(&self.state, id, name, &self.provider.base_url);
    }

    /// A `viewer` token, for the RBAC assertions.
    async fn viewer_token(&self) -> String {
        let (status, body) = self
            .request(
                Method::POST,
                "/api/tokens",
                Some(json!({"name": "assistant-viewer", "role": "viewer"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "token: {body}");
        parse(&body)["token"]
            .as_str()
            .expect("token once")
            .to_string()
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        self.request_as(method, uri, body, Some(TOKEN)).await
    }

    async fn request_with(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, String) {
        self.request_full(method, uri, body, Some(TOKEN), headers)
            .await
    }

    /// Send one request with an explicit bearer token (`None` sends none).
    async fn request_as(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (StatusCode, String) {
        self.request_full(method, uri, body, token, &[]).await
    }

    async fn request_full(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, String) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
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

/// Insert the provider row the assistant reads at request time.
fn attach(state: &AppState, base_url: &str) {
    insert_provider(state, PROVIDER_ID, "assistant-provider", base_url);
}

/// Insert one provider row with a placeholder id and name.
fn insert_provider(state: &AppState, id: &str, name: &str, base_url: &str) {
    let sealed = fleett::providers::seal(&state.cfg, PROVIDER_SECRET).expect("seal");
    let now = now_iso();
    let spec = ProviderSpec {
        id: id.to_string(),
        name: name.to_string(),
        kind: "openai-compatible".to_string(),
        base_url: base_url.to_string(),
        api_key_env: "PROVIDER_API_KEY".to_string(),
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
    // `null` is never bare: the reason tells "empty fleet" from "not selected".
    assert_eq!(config["provider_source"], "none");
    assert_eq!(config["reason"], "no_provider");
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
    let (status, body) = disabled
        .request(
            Method::PATCH,
            "/api/assistant/config",
            Some(json!({"provider": null})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_single_provider_is_used_automatically() {
    let harness = Harness::unconfigured();
    harness.add_provider("solo-provider", "opencode-go");

    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = parse(&body);
    assert_eq!(config["provider"]["id"], "solo-provider");
    assert_eq!(config["provider"]["name"], "opencode-go");
    assert_eq!(config["provider_source"], "auto");
    assert!(config["reason"].is_null(), "{config}");

    // And the chat works with no manual configuration at all: the panel must
    // stop claiming there is no provider when one exists.
    harness
        .provider
        .answer_with("{\"message\":\"draft\"}", 10, 5);
    let (status, body) = harness
        .chat(chat_body("auto", "skill", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "draft");
    assert_eq!(harness.provider.last().path, "/v1/chat/completions");
}

#[tokio::test]
async fn several_providers_are_never_selected_implicitly() {
    let harness = Harness::unconfigured();
    harness.add_provider("p-one", "one");
    harness.add_provider("p-two", "two");

    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = parse(&body);
    assert!(config["provider"].is_null(), "{config}");
    assert_eq!(config["provider_source"], "none");
    assert_eq!(config["reason"], "not_selected");

    // No credential is guessed: the chat refuses and names the reason.
    let (status, body) = harness
        .chat(chat_body("pick", "skill", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let error = body["error"].clone();
    assert_eq!(error["code"], "assistant_not_configured");
    assert_eq!(error["details"]["reason"], "not_selected");
    assert_eq!(error["details"]["providers"], "/api/providers");
    assert!(harness.provider.requests().is_empty(), "no paid call");
}

#[tokio::test]
async fn patch_selects_a_provider_and_persists_it() {
    let harness = Harness::unconfigured();
    harness.add_provider("p-one", "one");
    harness.add_provider("p-two", "two");

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/assistant/config",
            Some(json!({"provider": "p-two"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = parse(&body);
    assert_eq!(config["provider"]["id"], "p-two");
    assert_eq!(config["provider_source"], "setting");
    assert!(config["reason"].is_null(), "{config}");

    // The selection survives the request: it is stored, not echoed.
    assert_eq!(
        harness
            .state
            .db
            .setting(fleett::assistant::SETTING_ASSISTANT_PROVIDER)
            .expect("setting")
            .as_deref(),
        Some("p-two")
    );
    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["provider_source"], "setting");

    // The chat now answers through that provider.
    harness
        .provider
        .answer_with("{\"message\":\"selected\"}", 10, 5);
    let (status, body) = harness
        .chat(chat_body("selected", "skill", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "selected");

    // `null` clears the selection; two providers remain, so nothing is guessed.
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
    assert_eq!(config["reason"], "not_selected");
    assert_eq!(
        harness
            .state
            .db
            .setting(fleett::assistant::SETTING_ASSISTANT_PROVIDER)
            .expect("setting"),
        None
    );

    let rows = harness.audit("assistant.config").await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1]["details"]["provider"], "p-two");
    assert_eq!(rows[1]["details"]["source"], "setting");
    assert_eq!(rows[1]["details"]["env_override"], false);
}

#[tokio::test]
async fn the_environment_still_wins_over_the_stored_selection() {
    let harness = Harness::new();
    harness.add_provider("p-two", "two");

    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/assistant/config",
            Some(json!({"provider": "p-two"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = parse(&body);
    // The body is stored, but `FLEETT_ASSISTANT_PROVIDER` still answers, and
    // the panel is told exactly that instead of pretending otherwise.
    assert_eq!(config["provider"]["id"], PROVIDER_ID, "{config}");
    assert_eq!(config["provider_source"], "env");
    assert_eq!(
        harness
            .state
            .db
            .setting(fleett::assistant::SETTING_ASSISTANT_PROVIDER)
            .expect("setting")
            .as_deref(),
        Some("p-two")
    );

    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["provider_source"], "env");

    let rows = harness.audit("assistant.config").await;
    assert_eq!(rows[0]["details"]["env_override"], true, "{}", rows[0]);
}

#[tokio::test]
async fn a_broken_reference_answers_422_with_the_valid_ids() {
    // A deleted id in `FLEETT_ASSISTANT_PROVIDER`.
    let harness = Harness::with_env_provider("deleted-provider");
    harness.add_provider("p-one", "one");
    let valid = json!(["p-one"]);

    let (status, body) = harness
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let error = parse(&body)["error"].clone();
    assert_eq!(error["code"], "assistant_provider_unknown");
    assert_eq!(error["details"]["provider"], "deleted-provider");
    assert_eq!(error["details"]["source"], "env");
    assert_eq!(error["details"]["valid"], valid);

    // The chat reports the same thing instead of a 500 or a silent null.
    let (status, body) = harness
        .chat(chat_body("broken", "skill", json!({"page": "files"})))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "assistant_provider_unknown");

    // The same for a stored setting pointing at a provider that was deleted.
    let stored = Harness::unconfigured();
    stored.add_provider("p-one", "one");
    stored
        .state
        .db
        .set_setting(fleett::assistant::SETTING_ASSISTANT_PROVIDER, "gone")
        .expect("setting");
    let (status, body) = stored
        .request(Method::GET, "/api/assistant/config", None)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let error = parse(&body)["error"].clone();
    assert_eq!(error["code"], "assistant_provider_unknown");
    assert_eq!(error["details"]["source"], "setting");
    assert_eq!(error["details"]["valid"], valid);
}

#[tokio::test]
async fn patch_validates_the_provider_and_requires_admin() {
    let harness = Harness::unconfigured();
    harness.add_provider("p-one", "one");

    // A viewer may read the config but never select a provider.
    let viewer = harness.viewer_token().await;
    let (status, body) = harness
        .request_as(
            Method::PATCH,
            "/api/assistant/config",
            Some(json!({"provider": "p-one"})),
            Some(&viewer),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        harness
            .state
            .db
            .setting(fleett::assistant::SETTING_ASSISTANT_PROVIDER)
            .expect("setting"),
        None
    );

    // An unknown id is refused with the ids that would work.
    let (status, body) = harness
        .request(
            Method::PATCH,
            "/api/assistant/config",
            Some(json!({"provider": "nope"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let error = parse(&body)["error"].clone();
    assert_eq!(error["code"], "assistant_provider_unknown");
    assert_eq!(error["details"]["source"], "setting");
    assert_eq!(error["details"]["valid"], json!(["p-one"]));

    // The field is required: a typo must never clear a working selection.
    let (status, body) = harness
        .request(Method::PATCH, "/api/assistant/config", Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(parse(&body)["error"]["code"], "invalid_body");
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
        request.authorization.as_deref(),
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
