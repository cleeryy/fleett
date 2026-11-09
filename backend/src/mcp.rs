//! MCP server: the fleet as tools for an orchestrating agent.
//!
//! `fleett mcp` speaks the Model Context Protocol over stdin/stdout (one
//! JSON-RPC 2.0 message per line) or, with `--http <addr>`, over a minimal
//! Streamable-HTTP endpoint (`POST /mcp`, one JSON response; SSE streaming is
//! not implemented).
//!
//! The server is a *client* of the fleett HTTP API: every tool maps to one API
//! call made with the token of `~/.config/fleett/config.toml` (or `FLEETT_URL`
//! / `FLEETT_TOKEN`). It adds no privilege of its own: the API enforces the
//! token role, audits every mutation and answers `403` for a read-only viewer
//! that tries a mutation; that error is returned as a tool error carrying the
//! API message. See `docs/MCP.md`.

use crate::cli::{encode_path_segment, CliError, Client, ClientConfig, DEFAULT_LOG_TAIL};
use crate::config::VERSION;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use reqwest::Method;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Protocol revision announced by the server when the client asks for an
/// unknown one.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Revisions the server accepts from `initialize`.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// Bodies (log pages, file contents) above this size are truncated in the
/// textual result with an explicit marker.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// `serverInfo.name` of the MCP server.
pub const SERVER_NAME: &str = "fleett";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Every tool exposed by `tools/list`, in schema order.
pub const TOOL_NAMES: [&str; 19] = [
    "list_instances",
    "get_instance",
    "create_instance",
    "start_instance",
    "stop_instance",
    "restart_instance",
    "delete_instance",
    "logs",
    "env_set",
    "env_unset",
    "read_file",
    "write_file",
    "git_log",
    "git_status",
    "git_revert",
    "list_providers",
    "test_provider",
    "usage",
    "run_pipeline",
];

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// One MCP session: a JSON-RPC dispatcher bound to one API client.
///
/// Cheap to clone (the HTTP client is an `Arc` internally), which is what the
/// HTTP mode does for every request: the endpoint is stateless.
#[derive(Clone)]
pub struct Session {
    client: Client,
}

impl Session {
    /// Build a session from the usual client configuration.
    pub fn new(cfg: &ClientConfig) -> Result<Self, CliError> {
        Ok(Self {
            client: Client::new(cfg)?,
        })
    }

    /// Handle one decoded JSON-RPC message.
    ///
    /// Returns `None` for a notification (`notifications/initialized`, …),
    /// which never receives a response.
    pub async fn handle(&self, request: Value) -> Option<Value> {
        let Value::Object(object) = request else {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "invalid request: expected a JSON-RPC object",
                None,
            ));
        };
        let is_notification = !object.contains_key("id");
        let id = object.get("id").cloned().unwrap_or(Value::Null);
        if is_notification {
            // Every notification is accepted silently: `notifications/initialized`
            // is the only one the protocol requires, and no capability of this
            // server sends notifications of its own.
            let method = object
                .get("method")
                .and_then(|value| value.as_str())
                .unwrap_or("-");
            tracing::debug!(method, "MCP notification");
            return None;
        }
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_response(
                id,
                INVALID_REQUEST,
                "invalid request: \"jsonrpc\" must be \"2.0\"",
                None,
            ));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Some(error_response(
                id,
                INVALID_REQUEST,
                "invalid request: missing \"method\"",
                None,
            ));
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => result_response(id, initialize_result(&params)),
            "ping" => result_response(id, json!({})),
            "tools/list" => result_response(id, json!({ "tools": tools() })),
            "tools/call" => self.tools_call(id, &params).await,
            other => error_response(
                id,
                METHOD_NOT_FOUND,
                format!("method not found: {other}"),
                None,
            ),
        })
    }

    async fn tools_call(&self, id: Value, params: &Value) -> Value {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if name.is_empty() {
            return error_response(id, INVALID_PARAMS, "tools/call requires params.name", None);
        }
        if !TOOL_NAMES.contains(&name) {
            return error_response(
                id,
                INVALID_PARAMS,
                format!("unknown tool \"{name}\""),
                Some(json!({ "code": "unknown_tool", "tools": TOOL_NAMES })),
            );
        }
        let args = match Args::parse(params.get("arguments")) {
            Ok(args) => args,
            Err(err) => return result_response(id, err.into_result()),
        };
        match self.dispatch(name, args).await {
            Ok(outcome) => result_response(id, tool_success(outcome)),
            Err(err) => result_response(id, err.into_result()),
        }
    }

    async fn dispatch(&self, name: &str, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        match name {
            "list_instances" => self.list_instances(args).await,
            "get_instance" => self.get_instance(args).await,
            "create_instance" => self.create_instance(args).await,
            "start_instance" => self.lifecycle(args, "start").await,
            "stop_instance" => self.lifecycle(args, "stop").await,
            "restart_instance" => self.lifecycle(args, "restart").await,
            "delete_instance" => self.delete_instance(args).await,
            "logs" => self.logs(args).await,
            "env_set" => self.env_set(args).await,
            "env_unset" => self.env_unset(args).await,
            "read_file" => self.read_file(args).await,
            "write_file" => self.write_file(args).await,
            "git_log" => self.git_log(args).await,
            "git_status" => self.git_status(args).await,
            "git_revert" => self.git_revert(args).await,
            "list_providers" => self.list_providers(args).await,
            "test_provider" => self.test_provider(args).await,
            "usage" => self.usage(args).await,
            "run_pipeline" => self.run_pipeline(args).await,
            // `tools_call` rejects unknown names first.
            _ => Err(ToolError::invalid(format!("unknown tool \"{name}\""))),
        }
    }

    // -- instances ----------------------------------------------------------

    async fn list_instances(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&[])?;
        let value = self.get("/api/instances", &[]).await?;
        let mut lines = Vec::new();
        match value.as_array() {
            Some(items) => {
                lines.push(format!("{} instance(s)", items.len()));
                for item in items {
                    lines.push(format!(
                        "{}  status={} health={} template={} model={}",
                        text_field(item, "id"),
                        text_field(item, "status"),
                        text_field(item, "health"),
                        text_field(item, "template"),
                        text_field(item, "model"),
                    ));
                }
            }
            None => lines.push(compact_json(&value)),
        }
        Ok(ToolOutcome::new(lines.join("\n"), value))
    }

    async fn get_instance(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id"])?;
        let id = args.required_str("id")?;
        let value = self.get(&instance_api(id, ""), &[]).await?;
        let mut text = format!(
            "{}  status={} health={} image={} provider={} model={}",
            text_field(&value, "id"),
            text_field(&value, "status"),
            text_field(&value, "health"),
            text_field(&value, "image"),
            text_field(&value, "provider"),
            text_field(&value, "model"),
        );
        if let Some(error) = value.get("last_error").and_then(Value::as_str) {
            text.push_str(&format!("\nlast_error: {error}"));
        }
        for warning in string_list(&value, "warnings") {
            text.push_str(&format!("\nwarning: {warning}"));
        }
        Ok(ToolOutcome::new(text, value))
    }

    async fn create_instance(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&[
            "id",
            "name",
            "template",
            "model",
            "base_url",
            "api_key_env",
            "provider",
            "gate_token",
            "channel",
            "channel_token",
            "allow_lan",
            "cpu_limit",
            "mem_limit_mb",
            "notes",
        ])?;
        let id = args.required_str("id")?;
        let mut body = json!({ "id": id });
        let object = body.as_object_mut().expect("object");
        for key in [
            "name",
            "template",
            "model",
            "base_url",
            "api_key_env",
            "provider",
            "gate_token",
            "notes",
        ] {
            if let Some(value) = args.opt_str(key)? {
                object.insert(key.to_string(), json!(value));
            }
        }
        let channel = args.opt_str("channel")?;
        let channel_token = args.opt_str("channel_token")?;
        match (channel, channel_token) {
            (Some(channel), token) => {
                if !matches!(channel, "telegram" | "discord" | "none") {
                    return Err(ToolError::invalid(
                        "channel must be \"telegram\", \"discord\" or \"none\"",
                    ));
                }
                if channel == "none" && token.is_some() {
                    return Err(ToolError::invalid(
                        "channel_token is meaningless when channel is \"none\"",
                    ));
                }
                let mut channel_value = json!({ "type": channel });
                if let Some(token) = token {
                    channel_value["token"] = json!(token);
                }
                object.insert("channel".to_string(), channel_value);
            }
            (None, Some(_)) => {
                return Err(ToolError::invalid(
                    "channel_token requires channel (telegram, discord or none)",
                ))
            }
            (None, None) => {}
        }
        if let Some(value) = args.opt_bool("allow_lan")? {
            object.insert("allow_lan".to_string(), json!(value));
        }
        if let Some(value) = args.opt_f64("cpu_limit")? {
            if !(0.1..=8.0).contains(&value) {
                return Err(ToolError::invalid("cpu_limit must be between 0.1 and 8.0"));
            }
            object.insert("cpu_limit".to_string(), json!(value));
        }
        if let Some(value) = args.opt_u32("mem_limit_mb")? {
            if !(256..=8192).contains(&value) {
                return Err(ToolError::invalid(
                    "mem_limit_mb must be between 256 and 8192",
                ));
            }
            object.insert("mem_limit_mb".to_string(), json!(value));
        }
        let value = self.post("/api/instances", Some(body)).await?;
        let mut text = format!(
            "created instance {} ({})",
            text_field(&value, "id"),
            text_field(&value, "status")
        );
        for warning in string_list(&value, "warnings") {
            text.push_str(&format!("\nwarning: {warning}"));
        }
        Ok(ToolOutcome::new(text, value))
    }

    async fn lifecycle(&self, args: Args<'_>, action: &str) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id"])?;
        let id = args.required_str("id")?;
        let value = self
            .post(&instance_api(id, &format!("/{action}")), None)
            .await?;
        let status = text_field(&value, "status");
        Ok(ToolOutcome::new(
            format!("{id}: {status}"),
            json!({ "id": id, "status": status, "action": action }),
        ))
    }

    async fn delete_instance(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id", "purge"])?;
        let id = args.required_str("id")?;
        let purge = args.opt_bool("purge")?.unwrap_or(false);
        let query = if purge {
            vec![("purge", "true".to_string())]
        } else {
            Vec::new()
        };
        self.client
            .text(Method::DELETE, &instance_api(id, ""), &query, None)
            .await
            .map_err(ToolError::Api)?;
        Ok(ToolOutcome::new(
            format!(
                "deleted instance {id}{}",
                if purge { " (data purged)" } else { "" }
            ),
            json!({ "id": id, "deleted": true, "purged": purge }),
        ))
    }

    // -- logs and environment ----------------------------------------------

    async fn logs(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id", "tail", "since", "grep", "level"])?;
        let id = args.required_str("id")?;
        let tail = args.opt_u32("tail")?.unwrap_or(DEFAULT_LOG_TAIL);
        if !(1..=2000).contains(&tail) {
            return Err(ToolError::invalid("tail must be between 1 and 2000"));
        }
        let level = args.opt_str("level")?;
        if let Some(level) = level {
            if !matches!(level, "error" | "warn") {
                return Err(ToolError::invalid("level must be \"error\" or \"warn\""));
            }
        }
        let mut query = vec![("tail", tail.to_string()), ("format", "json".to_string())];
        if let Some(since) = args.opt_str("since")? {
            query.push(("since", since.to_string()));
        }
        if let Some(grep) = args.opt_str("grep")? {
            query.push(("grep", grep.to_string()));
        }
        if let Some(level) = level {
            query.push(("level", level.to_string()));
        }
        let value = self.get(&instance_api(id, "/logs"), &query).await?;
        let entries = value.as_array().cloned().unwrap_or_default();
        let lines: Vec<String> = entries.iter().map(log_line).collect();
        let text = if lines.is_empty() {
            "(no log lines)".to_string()
        } else {
            lines.join("\n")
        };
        let (text, truncated, total) = truncate_body(&text);
        let mut structured = json!({
            "id": id,
            "count": entries.len(),
            "truncated": truncated,
            "total_bytes": total,
        });
        if !truncated {
            structured["entries"] = Value::Array(entries);
        }
        Ok(ToolOutcome::new(text, structured))
    }

    async fn env_set(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id", "key", "value", "secret"])?;
        let id = args.required_str("id")?;
        let key = args.required_str("key")?;
        let value = args.required_text("value")?;
        let secret = args.opt_bool("secret")?.unwrap_or(false);
        let body = json!({
            "env": [{ "key": key, "value": value, "secret": secret }],
        });
        let detail = self.patch(&instance_api(id, ""), body).await?;
        let status = text_field(&detail, "status");
        Ok(ToolOutcome::new(
            format!("set {key} on {id} (secret: {secret})"),
            json!({ "id": id, "key": key, "secret": secret, "status": status }),
        ))
    }

    async fn env_unset(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id", "key"])?;
        let id = args.required_str("id")?;
        let key = args.required_str("key")?;
        let body = json!({ "env": [{ "key": key, "value": null }] });
        let detail = self.patch(&instance_api(id, ""), body).await?;
        let status = text_field(&detail, "status");
        Ok(ToolOutcome::new(
            format!("unset {key} on {id}"),
            json!({ "id": id, "key": key, "removed": true, "status": status }),
        ))
    }

    // -- files --------------------------------------------------------------

    async fn read_file(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id", "path"])?;
        let id = args.required_str("id")?;
        let path = args.required_str("path")?;
        let value = self
            .get(
                &instance_api(id, "/files/content"),
                &[("path", path.to_string())],
            )
            .await?;
        let content = value.get("content").and_then(Value::as_str).unwrap_or("");
        let (text, truncated, total) = truncate_body(content);
        let mut structured = json!({
            "path": value.get("path").cloned().unwrap_or_else(|| json!(path)),
            "etag": value.get("etag").cloned().unwrap_or(Value::Null),
            "size": value.get("size").cloned().unwrap_or(Value::Null),
            "mtime": value.get("mtime").cloned().unwrap_or(Value::Null),
            "editable": value.get("editable").cloned().unwrap_or(Value::Null),
            "rendered": value.get("rendered").cloned().unwrap_or(Value::Null),
            "truncated": truncated,
            "total_bytes": total,
        });
        if let Some(note) = value.get("note").and_then(Value::as_str) {
            structured["note"] = json!(note);
        }
        if !truncated {
            structured["content"] = json!(content);
        }
        Ok(ToolOutcome::new(text, structured))
    }

    async fn write_file(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id", "path", "content", "create_parents"])?;
        let id = args.required_str("id")?;
        let path = args.required_str("path")?;
        let content = args.required_text("content")?;
        let create_parents = args.opt_bool("create_parents")?.unwrap_or(false);
        if create_parents {
            self.ensure_parents(id, path).await?;
        }
        // The API is compare-and-swap: fetch the current etag so a write never
        // silently overwrites a concurrent edit. A missing file needs none.
        let etag = match self
            .get(
                &instance_api(id, "/files/content"),
                &[("path", path.to_string())],
            )
            .await
        {
            Ok(value) => value
                .get("etag")
                .and_then(Value::as_str)
                .map(str::to_string),
            Err(ToolError::Api(CliError::Api { status: 404, .. })) => None,
            Err(err) => return Err(err),
        };
        let mut body = json!({ "path": path, "content": content });
        if let Some(etag) = etag {
            body["etag"] = json!(etag);
        }
        let value = self
            .client
            .value(
                Method::PUT,
                &instance_api(id, "/files/content"),
                &[],
                Some(body),
            )
            .await
            .map_err(ToolError::Api)?;
        let mut text = format!(
            "wrote {} ({} bytes)",
            text_field(&value, "path"),
            value.get("size").and_then(Value::as_u64).unwrap_or(0)
        );
        if let Some(backup) = value.get("backup").and_then(Value::as_str) {
            text.push_str(&format!(" (backup {backup})"));
        }
        Ok(ToolOutcome::new(text, value))
    }

    /// Create the parent chain of a virtual path, one level at a time.
    ///
    /// `POST .../files/mkdir` requires the parent to exist; an `already_exists`
    /// conflict is the expected answer for a level that is already there.
    async fn ensure_parents(&self, id: &str, path: &str) -> Result<(), ToolError> {
        let trimmed = path.trim_start_matches('/');
        let mut segments: Vec<&str> = trimmed.split('/').collect();
        segments.pop();
        let mut prefix = String::new();
        for segment in segments {
            if segment.is_empty() || segment == "." {
                continue;
            }
            prefix.push('/');
            prefix.push_str(segment);
            let body = json!({ "path": prefix });
            match self
                .client
                .value(
                    Method::POST,
                    &instance_api(id, "/files/mkdir"),
                    &[],
                    Some(body),
                )
                .await
            {
                Ok(_) => {}
                Err(CliError::Api { code, .. }) if code == "already_exists" => {}
                Err(err) => return Err(ToolError::Api(err)),
            }
        }
        Ok(())
    }

    // -- history ------------------------------------------------------------

    async fn git_log(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id", "path", "limit"])?;
        let id = args.required_str("id")?;
        let mut query = Vec::new();
        if let Some(path) = args.opt_str("path")? {
            query.push(("path", path.to_string()));
        }
        if let Some(limit) = args.opt_u32("limit")? {
            if !(1..=500).contains(&limit) {
                return Err(ToolError::invalid("limit must be between 1 and 500"));
            }
            query.push(("limit", limit.to_string()));
        }
        let value = self.get(&instance_api(id, "/git/log"), &query).await?;
        let commits = value
            .get("commits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let lines: Vec<String> = commits
            .iter()
            .map(|commit| {
                format!(
                    "{} {} {}: {}",
                    text_field(commit, "short"),
                    text_field(commit, "at"),
                    text_field(commit, "author"),
                    text_field(commit, "message").replace('\n', " "),
                )
            })
            .collect();
        let text = if lines.is_empty() {
            "no commit".to_string()
        } else {
            lines.join("\n")
        };
        Ok(ToolOutcome::new(text, value))
    }

    async fn git_status(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id"])?;
        let id = args.required_str("id")?;
        let value = self.get(&instance_api(id, "/git/status"), &[]).await?;
        let modified = string_list(&value, "modified");
        let untracked = string_list(&value, "untracked");
        let text = if modified.is_empty() && untracked.is_empty() {
            "clean".to_string()
        } else {
            format!(
                "modified: {}\nuntracked: {}",
                if modified.is_empty() {
                    "-".to_string()
                } else {
                    modified.join(", ")
                },
                if untracked.is_empty() {
                    "-".to_string()
                } else {
                    untracked.join(", ")
                },
            )
        };
        Ok(ToolOutcome::new(text, value))
    }

    async fn git_revert(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id", "commit", "paths", "restart"])?;
        let id = args.required_str("id")?;
        let commit = args.required_str("commit")?;
        let mut body = json!({ "commit": commit });
        if let Some(paths) = args.opt_str_list("paths")? {
            body["paths"] = json!(paths);
        }
        if let Some(restart) = args.opt_bool("restart")? {
            body["restart"] = json!(restart);
        }
        let value = self
            .post(&instance_api(id, "/git/revert"), Some(body))
            .await?;
        let paths = string_list(&value, "paths");
        let text = format!(
            "reverted {} path(s) from {}",
            paths.len(),
            text_field(&value, "source")
        );
        Ok(ToolOutcome::new(text, value))
    }

    // -- providers ----------------------------------------------------------

    async fn list_providers(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&[])?;
        let value = self.get("/api/providers", &[]).await?;
        let mut lines = Vec::new();
        match value.as_array() {
            Some(items) => {
                lines.push(format!("{} provider(s)", items.len()));
                for item in items {
                    lines.push(format!(
                        "{}  name={} kind={} base_url={} instances={}",
                        text_field(item, "id"),
                        text_field(item, "name"),
                        text_field(item, "kind"),
                        text_field(item, "base_url"),
                        item.get("instances_count")
                            .map(Value::to_string)
                            .unwrap_or_else(|| "-".to_string()),
                    ));
                }
            }
            None => lines.push(compact_json(&value)),
        }
        Ok(ToolOutcome::new(lines.join("\n"), value))
    }

    async fn test_provider(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["id"])?;
        let id = args.required_str("id")?;
        let value = self.post(&instance_api_provider(id, "/test"), None).await?;
        let text = if value.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            format!(
                "provider {id}: ok (HTTP {}, {} ms)",
                value.get("status").and_then(Value::as_u64).unwrap_or(0),
                value.get("latency_ms").and_then(Value::as_u64).unwrap_or(0)
            )
        } else {
            format!(
                "provider {id}: failed: {}",
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("the probe failed")
            )
        };
        Ok(ToolOutcome::new(text, value))
    }

    // -- usage and pipelines ------------------------------------------------

    async fn usage(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&[])?;
        let value = self.get("/api/usage", &[]).await?;
        let mut lines = Vec::new();
        if let Some(instances) = value.get("instances").and_then(Value::as_array) {
            lines.push(format!("{} instance(s)", instances.len()));
            for entry in instances {
                lines.push(format!(
                    "{}  status={} cpu={:.2}% mem={} / {} disk={}",
                    text_field(entry, "id"),
                    text_field(entry, "status"),
                    entry.get("cpu_pct").and_then(Value::as_f64).unwrap_or(0.0),
                    crate::cli::human_bytes(
                        entry
                            .get("mem_used_bytes")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    ),
                    crate::cli::human_bytes(
                        entry
                            .get("mem_limit_bytes")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    ),
                    crate::cli::human_bytes(
                        entry.get("disk_bytes").and_then(Value::as_u64).unwrap_or(0)
                    ),
                ));
            }
        }
        if let Some(totals) = value.get("totals") {
            lines.push(format!(
                "total cpu={:.2}% mem={} / {} disk={}",
                totals.get("cpu_pct").and_then(Value::as_f64).unwrap_or(0.0),
                crate::cli::human_bytes(
                    totals
                        .get("mem_used_bytes")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                ),
                crate::cli::human_bytes(
                    totals
                        .get("mem_limit_bytes")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                ),
                crate::cli::human_bytes(
                    totals
                        .get("disk_bytes")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                ),
            ));
        }
        if let Some(caps) = value.get("caps") {
            lines.push(format!(
                "caps: cpu {} memory {} MB",
                caps.get("max_cpu").and_then(Value::as_f64).unwrap_or(0.0),
                caps.get("max_mem_mb").and_then(Value::as_u64).unwrap_or(0)
            ));
        }
        Ok(ToolOutcome::new(lines.join("\n"), value))
    }

    async fn run_pipeline(&self, args: Args<'_>) -> Result<ToolOutcome, ToolError> {
        args.reject_unknown(&["pipeline_id", "pipeline", "wait"])?;
        let pipeline_id = args.opt_str("pipeline_id")?;
        let pipeline = args.opt_str("pipeline")?;
        let _ = args.opt_bool("wait")?;
        match (pipeline_id, pipeline) {
            (None, None) => Err(ToolError::invalid(
                "run_pipeline requires \"pipeline_id\" or \"pipeline\"",
            )),
            (Some(_), Some(_)) => Err(ToolError::invalid(
                "pass either \"pipeline_id\" or \"pipeline\", not both",
            )),
            _ => Err(ToolError::tool(
                "not_available",
                "run_pipeline is not available: this build has no pipeline API \
                 (issue #31 is not merged). Start and stop instances with the \
                 individual tools instead.",
            )),
        }
    }

    // -- HTTP helpers -------------------------------------------------------

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, ToolError> {
        self.client
            .value(Method::GET, path, query, None)
            .await
            .map_err(ToolError::Api)
    }

    async fn post(&self, path: &str, body: Option<Value>) -> Result<Value, ToolError> {
        self.client
            .value(Method::POST, path, &[], body)
            .await
            .map_err(ToolError::Api)
    }

    async fn patch(&self, path: &str, body: Value) -> Result<Value, ToolError> {
        self.client
            .value(Method::PATCH, path, &[], Some(body))
            .await
            .map_err(ToolError::Api)
    }
}

fn instance_api(id: &str, suffix: &str) -> String {
    format!("/api/instances/{}{suffix}", encode_path_segment(id))
}

fn instance_api_provider(id: &str, suffix: &str) -> String {
    format!("/api/providers/{}{suffix}", encode_path_segment(id))
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = requested
        .filter(|value| SUPPORTED_PROTOCOL_VERSIONS.contains(value))
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": {
            "tools": { "listChanged": false },
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": VERSION,
        },
        "instructions": "fleett control plane. Every tool is one HTTP API call made with the \
                         configured token; the API enforces the token role, audits the call and \
                         answers \"forbidden\" for a viewer token that tries a mutation. \
                         Logs and file bodies are truncated to 64 KiB in the text result.",
    })
}

fn result_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message.into() });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

fn tool_success(outcome: ToolOutcome) -> Value {
    json!({
        "content": [{ "type": "text", "text": outcome.text }],
        "structuredContent": outcome.structured,
    })
}

fn tool_error(code: &str, message: &str, status: Option<u16>) -> Value {
    let mut error = json!({ "code": code, "message": message });
    if let Some(status) = status {
        error["status"] = json!(status);
    }
    json!({
        "content": [{
            "type": "text",
            "text": format!("fleett: {message} (code: {code})"),
        }],
        "structuredContent": { "ok": false, "error": error },
        "isError": true,
    })
}

// ---------------------------------------------------------------------------
// Tool plumbing
// ---------------------------------------------------------------------------

struct ToolOutcome {
    text: String,
    structured: Value,
}

impl ToolOutcome {
    fn new(text: impl Into<String>, structured: Value) -> Self {
        Self {
            text: text.into(),
            structured,
        }
    }
}

enum ToolError {
    /// The arguments do not match the published schema.
    Invalid(String),
    /// A deliberate tool-level refusal (`not_available`).
    Tool { code: &'static str, message: String },
    /// The HTTP API answered with an error.
    Api(CliError),
}

impl ToolError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    fn tool(code: &'static str, message: impl Into<String>) -> Self {
        Self::Tool {
            code,
            message: message.into(),
        }
    }

    fn into_result(self) -> Value {
        match self {
            Self::Invalid(message) => tool_error("invalid_arguments", &message, None),
            Self::Tool { code, message } => tool_error(code, &message, None),
            Self::Api(err) => {
                let status = match &err {
                    CliError::Api { status, .. } => Some(*status),
                    _ => None,
                };
                tool_error(err.code(), &err.message(), status)
            }
        }
    }
}

/// Typed view of one `tools/call` argument object.
struct Args<'a> {
    map: Option<&'a Map<String, Value>>,
}

impl<'a> Args<'a> {
    fn parse(arguments: Option<&'a Value>) -> Result<Self, ToolError> {
        match arguments {
            None | Some(Value::Null) => Ok(Self { map: None }),
            Some(Value::Object(map)) => Ok(Self { map: Some(map) }),
            Some(_) => Err(ToolError::invalid("arguments must be a JSON object")),
        }
    }

    fn get(&self, key: &str) -> Option<&'a Value> {
        self.map.and_then(|map| map.get(key))
    }

    fn reject_unknown(&self, allowed: &[&str]) -> Result<(), ToolError> {
        if let Some(map) = self.map {
            for key in map.keys() {
                if !allowed.contains(&key.as_str()) {
                    return Err(ToolError::invalid(format!(
                        "unknown argument \"{key}\"; allowed: {}",
                        if allowed.is_empty() {
                            "none".to_string()
                        } else {
                            allowed.join(", ")
                        }
                    )));
                }
            }
        }
        Ok(())
    }

    fn required_str(&self, key: &str) -> Result<&'a str, ToolError> {
        match self.get(key) {
            Some(Value::String(value)) if !value.is_empty() => Ok(value),
            Some(Value::String(_)) => {
                Err(ToolError::invalid(format!("\"{key}\" must not be empty")))
            }
            Some(_) => Err(ToolError::invalid(format!("\"{key}\" must be a string"))),
            None => Err(ToolError::invalid(format!(
                "missing required argument \"{key}\""
            ))),
        }
    }

    /// Like [`Args::required_str`], but an empty string is a valid value
    /// (an empty file, an empty environment variable).
    fn required_text(&self, key: &str) -> Result<&'a str, ToolError> {
        match self.get(key) {
            Some(Value::String(value)) => Ok(value),
            Some(_) => Err(ToolError::invalid(format!("\"{key}\" must be a string"))),
            None => Err(ToolError::invalid(format!(
                "missing required argument \"{key}\""
            ))),
        }
    }

    fn opt_str(&self, key: &str) -> Result<Option<&'a str>, ToolError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value)),
            Some(_) => Err(ToolError::invalid(format!("\"{key}\" must be a string"))),
        }
    }

    fn opt_str_list(&self, key: &str) -> Result<Option<Vec<String>>, ToolError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let Some(value) = item.as_str() else {
                        return Err(ToolError::invalid(format!(
                            "\"{key}\" must be an array of strings"
                        )));
                    };
                    out.push(value.to_string());
                }
                Ok(Some(out))
            }
            Some(_) => Err(ToolError::invalid(format!(
                "\"{key}\" must be an array of strings"
            ))),
        }
    }

    fn opt_bool(&self, key: &str) -> Result<Option<bool>, ToolError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(ToolError::invalid(format!("\"{key}\" must be a boolean"))),
        }
    }

    fn opt_u32(&self, key: &str) -> Result<Option<u32>, ToolError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => {
                let number = value
                    .as_u64()
                    .filter(|number| *number <= u64::from(u32::MAX))
                    .ok_or_else(|| {
                        ToolError::invalid(format!("\"{key}\" must be a positive integer"))
                    })?;
                Ok(Some(number as u32))
            }
        }
    }

    fn opt_f64(&self, key: &str) -> Result<Option<f64>, ToolError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_f64()
                .map(Some)
                .ok_or_else(|| ToolError::invalid(format!("\"{key}\" must be a number"))),
        }
    }
}

fn text_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("-")
}

fn string_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn log_line(entry: &Value) -> String {
    if let Some(message) = entry.get("message").and_then(Value::as_str) {
        match entry.get("time").and_then(Value::as_str) {
            Some(time) => format!("{time} {message}"),
            None => message.to_string(),
        }
    } else {
        entry
            .get("line")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

/// Cut a textual body to [`MAX_BODY_BYTES`] on a character boundary.
///
/// Returns the (possibly truncated) text, whether it was cut, and the original
/// length in bytes.
fn truncate_body(text: &str) -> (String, bool, usize) {
    let total = text.len();
    if total <= MAX_BODY_BYTES {
        return (text.to_string(), false, total);
    }
    let mut end = MAX_BODY_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let marker = format!("\n… [truncated: {end} of {total} bytes shown]");
    (format!("{}{marker}", &text[..end]), true, total)
}

// ---------------------------------------------------------------------------
// Tool catalog
// ---------------------------------------------------------------------------

/// The `tools/list` result payload.
pub fn tools() -> Vec<Value> {
    vec![
        tool(
            "list_instances",
            "List every instance of the fleet with its status, health, template and model.",
            json!({}),
            &[],
        ),
        tool(
            "get_instance",
            "Read one instance: status, health, image, model, provider, warnings and last error.",
            json!({ "id": instance_id_property() }),
            &["id"],
        ),
        tool(
            "create_instance",
            "Create and start an instance. Secrets (gate_token, channel_token) are stored \
             encrypted and never returned.",
            json!({
                "id": instance_id_property(),
                "name": { "type": "string", "description": "Display name (defaults to the id)." },
                "template": { "type": "string", "description": "Template id (default \"guest\")." },
                "model": { "type": "string", "description": "Model served by the gateway." },
                "base_url": { "type": "string", "description": "OpenAI-compatible provider base URL." },
                "api_key_env": {
                    "type": "string",
                    "pattern": "^[A-Z_][A-Z0-9_]{0,63}$",
                    "description": "Environment variable holding the provider key.",
                },
                "provider": { "type": "string", "description": "Attach a reusable provider id." },
                "gate_token": { "type": "string", "description": "Value for HERMES_GATE_PROFILE_TOKEN." },
                "channel": { "type": "string", "enum": ["telegram", "discord", "none"] },
                "channel_token": { "type": "string", "description": "Platform bot token; requires channel." },
                "allow_lan": { "type": "boolean", "default": false },
                "cpu_limit": { "type": "number", "minimum": 0.1, "maximum": 8.0 },
                "mem_limit_mb": { "type": "integer", "minimum": 256, "maximum": 8192 },
                "notes": { "type": "string", "description": "Free-form operator note." },
            }),
            &["id"],
        ),
        tool(
            "start_instance",
            "Start one instance.",
            json!({ "id": instance_id_property() }),
            &["id"],
        ),
        tool(
            "stop_instance",
            "Stop one instance (its data directory is preserved).",
            json!({ "id": instance_id_property() }),
            &["id"],
        ),
        tool(
            "restart_instance",
            "Restart one instance.",
            json!({ "id": instance_id_property() }),
            &["id"],
        ),
        tool(
            "delete_instance",
            "Delete one instance. purge=true also erases its data directory.",
            json!({
                "id": instance_id_property(),
                "purge": { "type": "boolean", "default": false },
            }),
            &["id"],
        ),
        tool(
            "logs",
            "Read the logs of an instance (newest tail, optional Docker duration or RFC 3339 \
             timestamp, regex grep and level filter).",
            json!({
                "id": instance_id_property(),
                "tail": { "type": "integer", "minimum": 1, "maximum": 2000, "default": 200 },
                "since": { "type": "string", "description": "Docker duration (30s, 10m, 2h) or RFC 3339 timestamp." },
                "grep": { "type": "string", "description": "Only keep lines matching this regex." },
                "level": { "type": "string", "enum": ["error", "warn"] },
            }),
            &["id"],
        ),
        tool(
            "env_set",
            "Create or update one environment variable of an instance and apply the change.",
            json!({
                "id": instance_id_property(),
                "key": {
                    "type": "string",
                    "pattern": "^[A-Z_][A-Z0-9_]{0,63}$",
                    "description": "Variable name.",
                },
                "value": { "type": "string", "description": "Variable value (never returned once stored as a secret)." },
                "secret": { "type": "boolean", "default": false, "description": "Encrypt the value at rest." },
            }),
            &["id", "key", "value"],
        ),
        tool(
            "env_unset",
            "Remove one environment variable of an instance and apply the change.",
            json!({
                "id": instance_id_property(),
                "key": { "type": "string", "pattern": "^[A-Z_][A-Z0-9_]{0,63}$" },
            }),
            &["id", "key"],
        ),
        tool(
            "read_file",
            "Read a text file inside the instance data directory (virtual absolute path).",
            json!({
                "id": instance_id_property(),
                "path": instance_path_property(),
            }),
            &["id", "path"],
        ),
        tool(
            "write_file",
            "Create or overwrite a text file inside the instance data directory. The write is \
             compare-and-swap against the version read just before.",
            json!({
                "id": instance_id_property(),
                "path": instance_path_property(),
                "content": { "type": "string", "description": "Full new content of the file." },
                "create_parents": { "type": "boolean", "default": false, "description": "Create the missing parent directories first." },
            }),
            &["id", "path", "content"],
        ),
        tool(
            "git_log",
            "List the commits of the instance history (newest first).",
            json!({
                "id": instance_id_property(),
                "path": { "type": "string", "description": "Limit to one file (data/SOUL.md or /SOUL.md)." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 },
            }),
            &["id"],
        ),
        tool(
            "git_status",
            "Report the files of the instance history that changed outside the API.",
            json!({ "id": instance_id_property() }),
            &["id"],
        ),
        tool(
            "git_revert",
            "Restore the files of one commit (a .bak is written first, then the restore is committed).",
            json!({
                "id": instance_id_property(),
                "commit": { "type": "string", "description": "Full or short commit id, or a revision such as HEAD~2." },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Files to restore; defaults to every path the commit changed.",
                },
                "restart": { "type": "boolean", "default": false },
            }),
            &["id", "commit"],
        ),
        tool(
            "list_providers",
            "List the reusable providers (metadata only, never a secret).",
            json!({}),
            &[],
        ),
        tool(
            "test_provider",
            "Probe one provider with a real request and report status and latency.",
            json!({ "id": { "type": "string", "description": "Provider id." } }),
            &["id"],
        ),
        tool(
            "usage",
            "Report CPU, memory and disk usage of the fleet with totals and configured caps.",
            json!({}),
            &[],
        ),
        tool(
            "run_pipeline",
            "Run a pipeline. Not available in this build: returns the code \"not_available\" \
             until the pipeline API (issue #31) exists.",
            json!({
                "pipeline_id": { "type": "string", "description": "Id of a stored pipeline." },
                "pipeline": { "type": "string", "description": "Pipeline YAML, for an ad-hoc run." },
                "wait": { "type": "boolean", "default": true, "description": "Wait for the pipeline to finish." },
            }),
            &[],
        ),
    ]
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

fn instance_id_property() -> Value {
    json!({
        "type": "string",
        "pattern": "^[a-z0-9-]{2,32}$",
        "description": "Instance id.",
    })
}

fn instance_path_property() -> Value {
    json!({
        "type": "string",
        "pattern": "^/",
        "description": "Virtual path inside the instance data directory, e.g. /SOUL.md.",
    })
}

// ---------------------------------------------------------------------------
// stdio transport
// ---------------------------------------------------------------------------

/// Run the server on stdin/stdout until EOF.
pub async fn run_stdio() -> anyhow::Result<()> {
    let cfg = load_config()?;
    let session = Session::new(&cfg).map_err(|err| anyhow::anyhow!("{err}"))?;
    tracing::info!(url = %cfg.url, "MCP server listening on stdio");
    serve_stream(session, tokio::io::stdin(), tokio::io::stdout()).await?;
    Ok(())
}

fn load_config() -> anyhow::Result<ClientConfig> {
    ClientConfig::load().map_err(|err| anyhow::anyhow!("{err}"))
}

/// Read line-delimited JSON-RPC from `reader`, write one response line per
/// request to `writer`. Notifications produce no output.
pub async fn serve_stream<R, W>(session: Session, reader: R, mut writer: W) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(request) => session.handle(request).await,
            Err(err) => Some(error_response(
                Value::Null,
                PARSE_ERROR,
                format!("parse error: {err}"),
                None,
            )),
        };
        if let Some(response) = response {
            let mut out = serde_json::to_vec(&response).map_err(std::io::Error::other)?;
            out.push(b'\n');
            writer.write_all(&out).await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Streamable HTTP transport (minimal: POST /mcp, JSON response, no SSE)
// ---------------------------------------------------------------------------

/// Run the server on `addr`, serving `POST /mcp`.
pub async fn run_http(addr: &str) -> anyhow::Result<()> {
    let cfg = load_config()?;
    let session = Session::new(&cfg).map_err(|err| anyhow::anyhow!("{err}"))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| anyhow::anyhow!("cannot bind {addr}: {err}"))?;
    let bound = listener.local_addr()?;
    tracing::info!(
        bind = %bound,
        path = "/mcp",
        "MCP Streamable HTTP listening (SSE streaming is not implemented)"
    );
    axum::serve(listener, http_router(session)).await?;
    Ok(())
}

/// Router of the HTTP mode. Stateless: every POST carries one JSON-RPC
/// message and receives one JSON response (or `202` for a notification).
pub fn http_router(session: Session) -> Router {
    Router::new()
        .route(
            "/mcp",
            post(http_mcp)
                .get(http_unsupported)
                .delete(http_unsupported),
        )
        .with_state(Arc::new(session))
}

async fn http_mcp(State(session): State<Arc<Session>>, body: Bytes) -> Response {
    let value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::OK,
                Json(error_response(
                    Value::Null,
                    PARSE_ERROR,
                    format!("parse error: {err}"),
                    None,
                )),
            )
                .into_response()
        }
    };
    match session.handle(value).await {
        Some(response) => (StatusCode::OK, Json(response)).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn http_unsupported() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "error": {
                "code": "sse_not_implemented",
                "message": "the fleett MCP HTTP endpoint is POST /mcp with one JSON-RPC \
                            response; SSE streaming is not implemented",
            }
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_strict_schema() {
        let tools = tools();
        assert_eq!(tools.len(), TOOL_NAMES.len());
        for (tool, expected) in tools.iter().zip(TOOL_NAMES) {
            assert_eq!(tool["name"], expected);
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert!(
                tool["inputSchema"]["properties"].is_object(),
                "{expected} has no properties object"
            );
            assert!(
                tool["description"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
                "{expected} has no description"
            );
        }
    }

    #[test]
    fn strict_schemas_never_expose_a_free_form_command() {
        for tool in tools() {
            let properties = tool["inputSchema"]["properties"].as_object().unwrap();
            assert!(
                !properties.contains_key("cmd") && !properties.contains_key("command"),
                "{} exposes a free-form command",
                tool["name"]
            );
        }
    }

    #[test]
    fn short_bodies_are_returned_untouched() {
        let (text, truncated, total) = truncate_body("hello");
        assert_eq!(text, "hello");
        assert!(!truncated);
        assert_eq!(total, 5);
    }

    #[test]
    fn long_bodies_are_truncated_on_a_character_boundary() {
        let body = "é".repeat(MAX_BODY_BYTES);
        let (text, truncated, total) = truncate_body(&body);
        assert!(truncated);
        assert_eq!(total, body.len());
        assert!(text.contains("[truncated:"));
        let body_part = text.split("\n… [truncated:").next().unwrap();
        assert!(body_part.len() <= MAX_BODY_BYTES);
        assert!(body_part.chars().all(|ch| ch == 'é'));
    }

    #[test]
    fn unknown_protocol_versions_fall_back_to_the_current_one() {
        let result = initialize_result(&json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        let result = initialize_result(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(result["protocolVersion"], "2024-11-05");
    }
}
