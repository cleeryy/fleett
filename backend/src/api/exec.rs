//! One-shot command execution inside an instance container (`exec`).
//!
//! This is deliberately **not** an interactive terminal: every call runs one
//! argv vector to completion (or until its timeout) and answers with the
//! captured streams. There is no stdin, no PTY and no session state. The web
//! console and `fleett exec` are thin clients of these two routes.
//!
//! The security model lives in [`crate::exec`]: allowlist, hard denylist,
//! basename-only names, bounded argv, bounded timeout and capped output. Every
//! invocation — accepted or refused — writes an `instance.exec` audit entry.

use crate::api::audit;
use crate::api::error::ApiError;
use crate::api::instances;
use crate::api::AppState;
use crate::auth::AuthContext;
use crate::exec::{
    ExecPolicy, MAX_ARGS, MAX_ARG_BYTES, MAX_OUTPUT_BYTES, MAX_TIMEOUT_S, MIN_TIMEOUT_S,
};
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Instant;

/// Audit action of every exec, accepted or refused.
pub const AUDIT_ACTION: &str = "instance.exec";
/// Characters a `cmd` string may not contain.
///
/// `cmd` is split on whitespace and **never** given to a shell; rejecting shell
/// syntax avoids the illusion of quoting or pipelines. Clients that need an
/// argument with a space or a quote send `argv` instead.
const CMD_FORBIDDEN: [char; 15] = [
    ';', '|', '&', '$', '`', '<', '>', '(', ')', '\\', '"', '\'', '\n', '\r', '\0',
];

/// Body of `POST /api/instances/{id}/exec`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExecRequest {
    /// Command line split on whitespace (no shell, no quoting).
    #[serde(default)]
    pub cmd: Option<String>,
    /// Command and arguments, already separated (preferred).
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    /// Seconds before the command is killed (`1..=300`, default `30`).
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// Response of `POST /api/instances/{id}/exec`.
#[derive(Debug, Clone, Serialize)]
pub struct ExecResponse {
    /// `None` when the command was killed by the timeout (or by a signal).
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// `true` when the timeout killed the command; the streams are then empty.
    pub timed_out: bool,
    pub duration_ms: u64,
    /// At least one stream was cut at 256 KiB and carries
    /// [`crate::exec::TRUNCATION_MARKER`].
    pub truncated: bool,
}

impl ExecResponse {
    fn from_output(output: crate::driver::ExecOutput) -> Self {
        Self {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            timed_out: output.timed_out,
            duration_ms: output.duration_ms,
            truncated: output.truncated,
        }
    }

    fn result(&self) -> &'static str {
        if self.timed_out {
            "timeout"
        } else {
            "ok"
        }
    }
}

/// Query of `GET /api/instances/{id}/exec/stream`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExecQuery {
    /// Command line split on whitespace (no shell, no quoting).
    #[serde(default)]
    pub cmd: Option<String>,
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// Split and validate the request into an argv vector plus its timeout.
fn prepare(request: &ExecRequest) -> Result<(Vec<String>, u64), ApiError> {
    let argv = match (request.cmd.as_deref(), request.argv.as_ref()) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "invalid_exec",
                "send either \"cmd\" or \"argv\", not both",
            ))
        }
        (None, None) => {
            return Err(ApiError::bad_request(
                "invalid_exec",
                "one of \"cmd\" or \"argv\" is required",
            ))
        }
        (Some(cmd), None) => split_cmd(cmd)?,
        (None, Some(argv)) => argv.clone(),
    };
    if argv.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_exec",
            "argv must contain at least a command name",
        ));
    }

    let timeout_s = request.timeout_s.unwrap_or(crate::exec::DEFAULT_TIMEOUT_S);
    if !(MIN_TIMEOUT_S..=MAX_TIMEOUT_S).contains(&timeout_s) {
        return Err(ApiError::bad_request(
            "invalid_timeout",
            format!("timeout_s must be between {MIN_TIMEOUT_S} and {MAX_TIMEOUT_S}"),
        ));
    }
    Ok((argv, timeout_s))
}

/// Split a `cmd` string on ASCII whitespace, refusing shell syntax.
fn split_cmd(cmd: &str) -> Result<Vec<String>, ApiError> {
    if let Some(found) = cmd.chars().find(|ch| CMD_FORBIDDEN.contains(ch)) {
        return Err(ApiError::bad_request(
            "invalid_exec",
            format!(
                "cmd contains {found:?}: it is split on whitespace without a shell, so quoting, \
                 pipes and redirections never work; send argv for literal arguments"
            ),
        ));
    }
    Ok(cmd.split_whitespace().map(str::to_string).collect())
}

/// Refuse one argv with the policy, in the shared 403 shape.
fn refuse(policy: &ExecPolicy, argv: &[String], refusal: crate::exec::ExecRefusal) -> ApiError {
    ApiError::forbidden(
        "exec_refused",
        format!(
            "command refused by the exec policy ({}): {}",
            refusal.code, refusal.reason
        ),
    )
    .with_details(json!({
        "reason": refusal.code,
        "detail": refusal.reason,
        "argv": argv,
        "allowlist": policy.allowlist(),
        "denylist": ExecPolicy::denylist(),
        "wildcard": policy.wildcard(),
    }))
}

/// Everything recorded for one exec attempt.
struct ExecAudit<'a> {
    argv: &'a [String],
    timeout_s: u64,
    result: &'static str,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    truncated: bool,
    reason: Option<&'a str>,
}

impl<'a> ExecAudit<'a> {
    fn refused(argv: &'a [String], timeout_s: u64, refusal: &'a crate::exec::ExecRefusal) -> Self {
        Self {
            argv,
            timeout_s,
            result: "refused",
            exit_code: None,
            duration_ms: None,
            truncated: false,
            reason: Some(refusal.code),
        }
    }

    fn failed(argv: &'a [String], timeout_s: u64, detail: &'a str) -> Self {
        Self {
            argv,
            timeout_s,
            result: "error",
            exit_code: None,
            duration_ms: None,
            truncated: false,
            reason: Some(detail),
        }
    }

    fn completed(argv: &'a [String], timeout_s: u64, response: &ExecResponse) -> Self {
        Self {
            argv,
            timeout_s,
            result: response.result(),
            exit_code: response.exit_code,
            duration_ms: Some(response.duration_ms),
            truncated: response.truncated,
            reason: None,
        }
    }
}

/// Audit one exec attempt, whatever its outcome.
fn audit_exec(state: &AppState, context: &AuthContext, id: &str, audit: ExecAudit<'_>) {
    let mut details = json!({
        "argv": audit.argv,
        "timeout_s": audit.timeout_s,
        "result": audit.result,
        "exit_code": audit.exit_code,
        "duration_ms": audit.duration_ms,
        "truncated": audit.truncated,
        "wildcard": state.exec_policy.wildcard(),
    });
    if let Some(reason) = audit.reason {
        details["reason"] = json!(reason);
    }
    audit::record(state, context, AUDIT_ACTION, id, Some(details));
}

/// `POST /api/instances/{id}/exec` — run one allowlisted command.
pub async fn run(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<ExecRequest>,
) -> Result<Json<ExecResponse>, ApiError> {
    // `run` is a POST (the viewer role is already blocked by the method
    // policy); the check is repeated here so the authorization of a command
    // never depends on the HTTP verb alone.
    audit::require_admin(&context)?;
    let (argv, timeout_s) = prepare(&request)?;
    let spec = instances::load(&state, &id)?;

    if let Err(refusal) = state.exec_policy.check(&argv) {
        audit_exec(
            &state,
            &context,
            &id,
            ExecAudit::refused(&argv, timeout_s, &refusal),
        );
        return Err(refuse(&state.exec_policy, &argv, refusal));
    }

    match state.driver.exec(&spec, &argv, timeout_s).await {
        Ok(output) => {
            let response = ExecResponse::from_output(output);
            audit_exec(
                &state,
                &context,
                &id,
                ExecAudit::completed(&argv, timeout_s, &response),
            );
            Ok(Json(response))
        }
        Err(err) => {
            audit_exec(
                &state,
                &context,
                &id,
                ExecAudit::failed(&argv, timeout_s, &err.detail),
            );
            Err(ApiError::driver(err))
        }
    }
}

/// `GET /api/instances/{id}/exec/stream?cmd=ls&timeout_s=30` — one-shot SSE.
///
/// The whole output is emitted once the command ends: `stdout` and `stderr`
/// events carry one JSON `{"line":"…"}` each, then a final `exit` event carries
/// the same fields as the JSON response. There is no stdin: this is not an
/// interactive terminal, and the docs say so.
pub async fn stream(
    Path(id): Path<String>,
    Query(query): Query<ExecQuery>,
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Response, ApiError> {
    // The SSE route is a GET: without this check the viewer role, which the
    // middleware lets through on read methods, could execute commands.
    audit::require_admin(&context)?;
    let request = ExecRequest {
        cmd: query.cmd,
        argv: None,
        timeout_s: query.timeout_s,
    };
    let (argv, timeout_s) = prepare(&request)?;
    let spec = instances::load(&state, &id)?;

    if let Err(refusal) = state.exec_policy.check(&argv) {
        audit_exec(
            &state,
            &context,
            &id,
            ExecAudit::refused(&argv, timeout_s, &refusal),
        );
        return Err(refuse(&state.exec_policy, &argv, refusal));
    }

    let output = match state.driver.exec(&spec, &argv, timeout_s).await {
        Ok(output) => output,
        Err(err) => {
            audit_exec(
                &state,
                &context,
                &id,
                ExecAudit::failed(&argv, timeout_s, &err.detail),
            );
            return Err(ApiError::driver(err));
        }
    };
    let response = ExecResponse::from_output(output);
    audit_exec(
        &state,
        &context,
        &id,
        ExecAudit::completed(&argv, timeout_s, &response),
    );

    let start = Instant::now();
    let body = sse_body(&response);
    tracing::debug!(
        instance = %id,
        render_ms = start.elapsed().as_millis(),
        "exec stream rendered"
    );
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONNECTION, "keep-alive"),
        ],
        body,
    )
        .into_response())
}

/// Render the one-shot SSE body. Each `data:` payload is JSON, so a line that
/// contains newlines or control characters stays on one SSE line.
fn sse_body(response: &ExecResponse) -> String {
    let mut body = String::new();
    for line in response.stdout.lines() {
        push_event(&mut body, "stdout", &json!({ "line": line }));
    }
    for line in response.stderr.lines() {
        push_event(&mut body, "stderr", &json!({ "line": line }));
    }
    push_event(
        &mut body,
        "exit",
        &json!({
            "exit_code": response.exit_code,
            "timed_out": response.timed_out,
            "duration_ms": response.duration_ms,
            "truncated": response.truncated,
        }),
    );
    body
}

fn push_event(body: &mut String, event: &str, data: &serde_json::Value) {
    body.push_str("event: ");
    body.push_str(event);
    body.push_str("\ndata: ");
    body.push_str(&data.to_string());
    body.push_str("\n\n");
}

/// `GET /api/exec/policy` — effective allowlist, denylist and bounds (admin).
pub async fn policy(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<serde_json::Value>, ApiError> {
    audit::require_admin(&context)?;
    let policy = &state.exec_policy;
    Ok(Json(json!({
        "wildcard": policy.wildcard(),
        "source": policy.source(),
        "allowlist": policy.allowlist(),
        "denylist": ExecPolicy::denylist(),
        "max_args": MAX_ARGS,
        "max_arg_bytes": MAX_ARG_BYTES,
        "max_output_bytes": MAX_OUTPUT_BYTES,
        "timeout_s": {
            "default": crate::exec::DEFAULT_TIMEOUT_S,
            "min": MIN_TIMEOUT_S,
            "max": MAX_TIMEOUT_S,
        },
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(cmd: Option<&str>, argv: Option<&[&str]>, timeout_s: Option<u64>) -> ExecRequest {
        ExecRequest {
            cmd: cmd.map(str::to_string),
            argv: argv.map(|values| values.iter().map(|value| value.to_string()).collect()),
            timeout_s,
        }
    }

    #[test]
    fn cmd_is_split_on_whitespace() {
        let (argv, timeout_s) =
            prepare(&request(Some("  ls   /opt/data "), None, None)).expect("ok");
        assert_eq!(argv, vec!["ls", "/opt/data"]);
        assert_eq!(timeout_s, crate::exec::DEFAULT_TIMEOUT_S);
    }

    #[test]
    fn shell_syntax_is_refused_instead_of_being_faked() {
        for cmd in [
            "ls; rm -rf /",
            "cat /x | grep y",
            "echo $HOME",
            "ls \"a b\"",
        ] {
            let err = prepare(&request(Some(cmd), None, None)).expect_err(cmd);
            assert_eq!(err.code, "invalid_exec", "{cmd}");
            assert_eq!(err.status, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn exactly_one_of_cmd_and_argv_is_required() {
        assert_eq!(
            prepare(&request(None, None, None)).expect_err("none").code,
            "invalid_exec"
        );
        assert_eq!(
            prepare(&request(Some("ls"), Some(&["ls"]), None))
                .expect_err("both")
                .code,
            "invalid_exec"
        );
        assert_eq!(
            prepare(&request(None, Some(&[]), None))
                .expect_err("empty")
                .code,
            "invalid_exec"
        );
    }

    #[test]
    fn the_timeout_is_bounded() {
        for value in [1, 30, 300] {
            assert_eq!(
                prepare(&request(None, Some(&["ls"]), Some(value)))
                    .expect("valid timeout")
                    .1,
                value
            );
        }
        for value in [0, 301, u64::MAX] {
            let err = prepare(&request(None, Some(&["ls"]), Some(value))).expect_err("timeout");
            assert_eq!(err.code, "invalid_timeout", "{value}");
        }
    }

    #[test]
    fn the_refusal_uses_the_documented_403_shape() {
        let policy = ExecPolicy::default_policy();
        let argv = vec!["rm".to_string(), "-rf".to_string(), "/".to_string()];
        let refusal = policy.check(&argv).expect_err("refused");
        let err = refuse(&policy, &argv, refusal);
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.code, "exec_refused");
        let details = err.details.expect("details");
        assert_eq!(details["reason"], "denied_command");
        assert!(details["allowlist"]
            .as_array()
            .expect("allowlist")
            .iter()
            .any(|value| value == "ls"));
    }

    #[test]
    fn sse_events_carry_json_lines_and_a_final_exit() {
        let response = ExecResponse {
            exit_code: Some(0),
            stdout: "one\ntwo".to_string(),
            stderr: "warn".to_string(),
            timed_out: false,
            duration_ms: 12,
            truncated: false,
        };
        let body = sse_body(&response);
        assert!(
            body.contains("event: stdout\ndata: {\"line\":\"one\"}"),
            "{body}"
        );
        assert!(
            body.contains("event: stdout\ndata: {\"line\":\"two\"}"),
            "{body}"
        );
        assert!(
            body.contains("event: stderr\ndata: {\"line\":\"warn\"}"),
            "{body}"
        );
        assert!(
            body.ends_with("event: exit\ndata: {\"duration_ms\":12,\"exit_code\":0,\"timed_out\":false,\"truncated\":false}\n\n"),
            "{body}"
        );
    }
}
