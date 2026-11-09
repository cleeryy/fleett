//! Minimal Model Context Protocol client (issue #62).
//!
//! fleett acts as an MCP *server* in [`crate::mcp`]; this module is the other
//! side: it probes MCP servers fleett deployed or imported, so the catalogue
//! can show what a server actually exposes. Only the Streamable-HTTP transport
//! is implemented (`POST` JSON-RPC 2.0, one response per request, JSON or
//! SSE-framed body).
//!
//! A probe runs `initialize` → `notifications/initialized` → `tools/list`.
//! The `Mcp-Session-Id` header is carried across the three calls when the
//! upstream returns one, and `MCP-Protocol-Version` is sent on every request
//! after initialization, as the 2025-06-18 revision requires.
//!
//! Every request is bounded twice: a per-call timeout ([`DEFAULT_TIMEOUT`],
//! `FLEETT_MCP_CALL_TIMEOUT_S`) and a hard cap on the response body. Neither
//! the headers nor the environment values of a server are ever logged.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Protocol revision announced by [`McpClient`] during initialization.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Default per-call timeout (`FLEETT_MCP_CALL_TIMEOUT_S`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// Hard cap of one response body.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Hard cap of the tools accepted from one `tools/list` page.
pub const MAX_TOOLS: usize = 2_000;
/// `clientInfo.name` announced by the control plane.
pub const CLIENT_NAME: &str = "fleett";

/// One tool as reported by `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema of the arguments (`inputSchema`).
    #[serde(default, alias = "inputSchema")]
    pub input_schema: Value,
}

/// Result of one discovery probe.
#[derive(Debug, Clone, PartialEq)]
pub struct Discovery {
    /// Revision the server negotiated (echoed or announced by the server).
    pub protocol_version: String,
    /// `serverInfo` of the upstream, when it sent one.
    pub server_info: Value,
    pub tools: Vec<McpTool>,
    /// Wall-clock duration of the whole probe (initialize + list).
    pub latency_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    #[error("cannot reach the MCP server at {url}: {detail}")]
    Transport { url: String, detail: String },
    #[error("the MCP server answered HTTP {status}: {detail}")]
    Status { status: u16, detail: String },
    #[error("invalid MCP response: {0}")]
    Protocol(String),
    #[error("the MCP response exceeds the {MAX_BODY_BYTES} byte limit")]
    TooLarge,
}

/// State of one logical MCP session (session id + negotiated revision).
#[derive(Debug, Clone, Default)]
struct Session {
    id: Option<String>,
    protocol_version: Option<String>,
}

/// HTTP client for MCP servers.
#[derive(Debug, Clone)]
pub struct McpClient {
    http: reqwest::Client,
    timeout: Duration,
}

impl McpClient {
    /// Client with an explicit per-call timeout.
    pub fn new(timeout: Duration) -> Self {
        let timeout = timeout.max(Duration::from_millis(100));
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(format!("fleett/{}", crate::config::VERSION))
            .build()
            .unwrap_or_default();
        Self { http, timeout }
    }

    /// Client configured from `FLEETT_MCP_CALL_TIMEOUT_S`.
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        Self::new(Duration::from_secs(cfg.mcp_call_timeout_s.max(1)))
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Probe a server: `initialize`, `notifications/initialized`, `tools/list`.
    ///
    /// `headers` are the decrypted HTTP headers of the server (authorization,
    /// API keys). They are sent verbatim and never logged.
    pub async fn discover(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<Discovery, McpClientError> {
        let started = Instant::now();
        let mut session = Session::default();
        let init = self
            .request(
                url,
                headers,
                &mut session,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {},
                        "clientInfo": {
                            "name": CLIENT_NAME,
                            "version": crate::config::VERSION,
                        },
                    },
                }),
            )
            .await?
            .ok_or_else(|| {
                McpClientError::Protocol("the initialize request returned no response".into())
            })?;
        let protocol_version = init
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(PROTOCOL_VERSION)
            .to_string();
        session.protocol_version = Some(protocol_version.clone());
        let server_info = init.get("serverInfo").cloned().unwrap_or(Value::Null);

        // A notification: accepted without a body (`202 Accepted`).
        self.notify(
            url,
            headers,
            &mut session,
            "notifications/initialized",
            json!({}),
        )
        .await?;

        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for page in 0..10 {
            let mut params = serde_json::Map::new();
            if let Some(cursor) = cursor.as_deref() {
                params.insert("cursor".to_string(), Value::from(cursor));
            }
            let result = self
                .request(
                    url,
                    headers,
                    &mut session,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 2 + page,
                        "method": "tools/list",
                        "params": Value::Object(params),
                    }),
                )
                .await?
                .ok_or_else(|| {
                    McpClientError::Protocol("the tools/list request returned no response".into())
                })?;
            let Some(list) = result.get("tools").and_then(Value::as_array) else {
                return Err(McpClientError::Protocol(
                    "the tools/list result has no \"tools\" array".into(),
                ));
            };
            for value in list {
                if tools.len() >= MAX_TOOLS {
                    break;
                }
                let Some(name) = value.get("name").and_then(Value::as_str) else {
                    continue;
                };
                tools.push(McpTool {
                    name: name.to_string(),
                    description: value
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    input_schema: value
                        .get("inputSchema")
                        .or_else(|| value.get("input_schema"))
                        .cloned()
                        .unwrap_or(Value::Null),
                });
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string);
            if cursor.is_none() || tools.len() >= MAX_TOOLS {
                break;
            }
        }

        Ok(Discovery {
            protocol_version,
            server_info,
            tools,
            latency_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        })
    }

    /// Send a JSON-RPC `ping` (the cheapest liveness probe of a live session).
    pub async fn ping(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<(), McpClientError> {
        let mut session = Session {
            id: None,
            protocol_version: Some(PROTOCOL_VERSION.to_string()),
        };
        self.request(
            url,
            headers,
            &mut session,
            json!({"jsonrpc": "2.0", "id": 1, "method": "ping", "params": {}}),
        )
        .await?
        .ok_or_else(|| McpClientError::Protocol("the ping request returned no response".into()))?;
        Ok(())
    }

    async fn notify(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        session: &mut Session,
        method: &str,
        params: Value,
    ) -> Result<(), McpClientError> {
        self.request(
            url,
            headers,
            session,
            json!({"jsonrpc": "2.0", "method": method, "params": params}),
        )
        .await?;
        Ok(())
    }

    /// One JSON-RPC call. `Ok(None)` for a notification (no response body).
    async fn request(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        session: &mut Session,
        payload: Value,
    ) -> Result<Option<Value>, McpClientError> {
        let is_initialize = payload.get("method").and_then(Value::as_str) == Some("initialize");
        let mut request = self
            .http
            .post(url)
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&payload);
        if let Some(id) = session.id.as_deref() {
            request = request.header("Mcp-Session-Id", id);
        }
        if !is_initialize {
            if let Some(version) = session.protocol_version.as_deref() {
                request = request.header("MCP-Protocol-Version", version);
            }
        }
        for (key, value) in headers {
            request = request.header(key.as_str(), value.as_str());
        }
        let response = request
            .send()
            .await
            .map_err(|err| McpClientError::Transport {
                url: url.to_string(),
                // `reqwest` errors never include a header value; URL + reason only.
                detail: err.to_string(),
            })?;

        let status = response.status();
        if let Some(id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            session.id = Some(id.to_string());
        }
        if !status.is_success() {
            let detail = read_bounded_text(response).await.unwrap_or_default();
            return Err(McpClientError::Status {
                status: status.as_u16(),
                detail: truncate(&detail, 500),
            });
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let body = read_bounded(response).await?;
        if body.is_empty() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&body);

        let value = if content_type.contains("text/event-stream") {
            parse_sse(&text).ok_or_else(|| {
                McpClientError::Protocol("no JSON-RPC response in the SSE stream".into())
            })?
        } else {
            serde_json::from_str(text.trim()).map_err(|err| {
                McpClientError::Protocol(format!("the body is not valid JSON: {err}"))
            })?
        };

        let envelope = match value {
            Value::Object(map) => map,
            _ => {
                return Err(McpClientError::Protocol(
                    "the JSON-RPC response is not an object".into(),
                ))
            }
        };
        if let Some(error) = envelope.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            return Err(McpClientError::Protocol(format!(
                "the server answered JSON-RPC error {code}: {message}"
            )));
        }
        Ok(Some(envelope.get("result").cloned().ok_or_else(|| {
            McpClientError::Protocol("the JSON-RPC response has no \"result\"".into())
        })?))
    }
}

/// Read a response body, refusing anything above [`MAX_BODY_BYTES`].
async fn read_bounded(mut response: reqwest::Response) -> Result<Vec<u8>, McpClientError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| McpClientError::Transport {
            url: String::new(),
            detail: err.to_string(),
        })?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(McpClientError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn read_bounded_text(response: reqwest::Response) -> Result<String, McpClientError> {
    read_bounded(response)
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Find the first JSON-RPC response in an SSE body.
///
/// Frames end on a blank line; `data:` lines are concatenated with `\n`.
/// Comments (`:`), `event:` and `id:` lines are ignored, and a frame that is
/// not valid JSON (a keep-alive or a notification) is skipped rather than
/// failing the whole probe.
fn parse_sse(text: &str) -> Option<Value> {
    let mut data: Vec<String> = Vec::new();
    let mut fallback: Option<Value> = None;
    for line in text.lines() {
        if line.is_empty() {
            if let Some(value) = flush_sse_frame(&mut data) {
                if value.get("id").is_some() {
                    return Some(value);
                }
                fallback.get_or_insert(value);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
    }
    if let Some(value) = flush_sse_frame(&mut data) {
        if value.get("id").is_some() {
            return Some(value);
        }
        fallback.get_or_insert(value);
    }
    fallback
}

fn flush_sse_frame(data: &mut Vec<String>) -> Option<Value> {
    if data.is_empty() {
        return None;
    }
    let payload = data.join("\n");
    data.clear();
    serde_json::from_str(&payload).ok()
}

fn truncate(value: &str, max: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Fixture {
        /// Upstream requires this session header on every non-initialize call.
        require_session: bool,
        /// Answer `tools/list` with an SSE frame instead of JSON.
        sse: bool,
        /// Answer every request with this status.
        fail_status: Option<u16>,
        /// Send a malformed SSE frame before the real response.
        malformed_frame: bool,
        seen: Arc<Mutex<Vec<SeenRequest>>>,
    }

    #[derive(Debug, Clone)]
    struct SeenRequest {
        method: String,
        accept: String,
        protocol_version: Option<String>,
        session: Option<String>,
    }

    async fn rpc(headers: HeaderMap, Json(body): Json<Value>) -> Response {
        let state = headers
            .get("x-fixture")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let _ = state;
        handle(headers, body).await
    }

    async fn handle(headers: HeaderMap, body: Value) -> Response {
        // The fixture state travels in a header set by `start`: decoding it
        // here keeps every test independent without a router state type.
        let config = FixtureConfig::from_headers(&headers);
        let method = body
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let fixture = FIXTURES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|(key, _)| key == &config.0)
            .map(|(_, fixture)| fixture.clone())
            .unwrap_or_default();
        fixture
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(SeenRequest {
                method: method.clone(),
                accept: headers
                    .get(header::ACCEPT)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string(),
                protocol_version: headers
                    .get("MCP-Protocol-Version")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
                session: headers
                    .get("Mcp-Session-Id")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
            });

        if let Some(status) = fixture.fail_status {
            return (
                StatusCode::from_u16(status).unwrap_or(StatusCode::UNAUTHORIZED),
                "upstream says no",
            )
                .into_response();
        }
        if fixture.require_session && method != "initialize" {
            let ok = headers
                .get("Mcp-Session-Id")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value == "session-1");
            if !ok {
                return (StatusCode::BAD_REQUEST, "missing session").into_response();
            }
        }

        let id = body.get("id").cloned();
        let Some(id) = id else {
            return StatusCode::ACCEPTED.into_response();
        };
        let result = match method.as_str() {
            "initialize" => json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fixture", "version": "1.0"},
            }),
            "tools/list" => json!({
                "tools": [
                    {"name": "fetch", "description": "Fetch a URL",
                     "inputSchema": {"type": "object"}},
                    {"name": "search", "inputSchema": {"type": "object"}},
                ]
            }),
            "ping" => json!({}),
            other => {
                return (
                    StatusCode::OK,
                    Json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": format!("unknown method {other}")},
                    })),
                )
                    .into_response()
            }
        };
        let response = json!({"jsonrpc": "2.0", "id": id, "result": result});
        if fixture.sse && method == "tools/list" {
            let mut frames = String::new();
            if fixture.malformed_frame {
                frames.push_str("data: {not json}\n\n");
            }
            frames.push_str(&format!("event: message\ndata: {response}\n\n"));
            let mut response =
                ([(header::CONTENT_TYPE, "text/event-stream")], frames).into_response();
            response.headers_mut().insert(
                "Mcp-Session-Id",
                header::HeaderValue::from_static("session-1"),
            );
            return response;
        }
        let mut response = Json(response).into_response();
        if method == "initialize" || fixture.require_session {
            response.headers_mut().insert(
                "Mcp-Session-Id",
                header::HeaderValue::from_static("session-1"),
            );
        }
        response
    }

    // One static registry keyed by a per-test id: axum handlers cannot capture
    // per-server state without a state type, and the fixture is a test aid.
    static FIXTURES: Mutex<Vec<(String, Fixture)>> = Mutex::new(Vec::new());

    struct FixtureConfig(String);

    impl FixtureConfig {
        fn from_headers(headers: &HeaderMap) -> Self {
            Self(
                headers
                    .get("x-fixture")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string(),
            )
        }
    }

    struct Server {
        url: String,
        fixture: Fixture,
        handle: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    async fn start(fixture: Fixture) -> Server {
        let id = uuid::Uuid::new_v4().to_string();
        FIXTURES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((id.clone(), fixture.clone()));
        let app = Router::new()
            .route("/mcp", post(rpc))
            .layer(axum::middleware::from_fn(
                move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                    let id = id.clone();
                    async move {
                        request.headers_mut().insert(
                            "x-fixture",
                            header::HeaderValue::from_str(&id).expect("fixture id"),
                        );
                        next.run(request).await
                    }
                },
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Server {
            url: format!("http://{address}/mcp"),
            fixture,
            handle,
        }
    }

    #[tokio::test]
    async fn discovers_tools_over_plain_json() {
        let server = start(Fixture::default()).await;
        let client = McpClient::new(Duration::from_secs(5));
        let discovery = client
            .discover(&server.url, &BTreeMap::new())
            .await
            .expect("discovery");
        assert_eq!(discovery.protocol_version, PROTOCOL_VERSION);
        assert_eq!(discovery.tools.len(), 2);
        assert_eq!(discovery.tools[0].name, "fetch");
        assert_eq!(discovery.tools[0].description, "Fetch a URL");
        assert_eq!(discovery.tools[0].input_schema["type"], "object");
        assert_eq!(discovery.server_info["name"], "fixture");
        assert!(discovery.latency_ms < 10_000);

        let seen = server
            .fixture
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(seen[0].method, "initialize");
        assert_eq!(seen[1].method, "notifications/initialized");
        assert_eq!(seen[2].method, "tools/list");
        assert!(seen[0].accept.contains("text/event-stream"));
        assert!(seen[0].protocol_version.is_none());
        assert_eq!(seen[2].protocol_version.as_deref(), Some(PROTOCOL_VERSION));
    }

    #[tokio::test]
    async fn decodes_an_sse_body_and_skips_a_malformed_frame() {
        let server = start(Fixture {
            sse: true,
            malformed_frame: true,
            ..Fixture::default()
        })
        .await;
        let client = McpClient::new(Duration::from_secs(5));
        let discovery = client
            .discover(&server.url, &BTreeMap::new())
            .await
            .expect("discovery");
        assert_eq!(discovery.tools.len(), 2);
    }

    #[tokio::test]
    async fn carries_the_session_header_and_protocol_version() {
        let server = start(Fixture {
            require_session: true,
            ..Fixture::default()
        })
        .await;
        let client = McpClient::new(Duration::from_secs(5));
        client
            .discover(&server.url, &BTreeMap::new())
            .await
            .expect("discovery");
        let seen = server
            .fixture
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert!(seen[0].session.is_none());
        assert_eq!(seen[1].session.as_deref(), Some("session-1"));
        assert_eq!(seen[2].session.as_deref(), Some("session-1"));
    }

    #[tokio::test]
    async fn an_upstream_401_is_reported_without_leaking_headers() {
        let server = start(Fixture {
            fail_status: Some(401),
            ..Fixture::default()
        })
        .await;
        let client = McpClient::new(Duration::from_secs(5));
        let mut headers = BTreeMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer super-secret".to_string(),
        );
        let err = client
            .discover(&server.url, &headers)
            .await
            .expect_err("401");
        let message = err.to_string();
        assert!(message.contains("401"), "{message}");
        assert!(
            !message.contains("super-secret"),
            "secret leaked: {message}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_transport_error() {
        let client = McpClient::new(Duration::from_millis(200));
        let err = client
            .discover("http://127.0.0.1:1/mcp", &BTreeMap::new())
            .await
            .expect_err("unreachable");
        assert!(matches!(err, McpClientError::Transport { .. }), "{err:?}");
    }

    #[test]
    fn sse_parser_ignores_comments_and_notifications() {
        let text = "\
: keep-alive
event: message
data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}

data: {\"jsonrpc\":\"2.0\",
data: \"id\":7,\"result\":{\"ok\":true}}
";
        let value = parse_sse(text).expect("response");
        assert_eq!(value["id"], 7);
    }
}
