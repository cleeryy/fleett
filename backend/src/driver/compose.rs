//! Docker Compose driver.
//!
//! Every instance is a separate compose project (`hermes-<id>`) driven through
//! `docker compose -f <root>/instances/<id>/compose.yaml -p hermes-<id>`.

use super::{Driver, DriverError, DriverFuture, PsResult};
use crate::config::Config;
use crate::models::{ContainerInfo, InstanceSpec, Status};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const UP_TIMEOUT: Duration = Duration::from_secs(180);
const DOWN_TIMEOUT: Duration = Duration::from_secs(120);
const CMD_TIMEOUT: Duration = Duration::from_secs(60);
const PS_TIMEOUT: Duration = Duration::from_secs(20);
const LOGS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ComposeDriver {
    root: PathBuf,
    docker: PathBuf,
    network: String,
}

impl ComposeDriver {
    pub fn new(cfg: &Config) -> Self {
        Self {
            root: cfg.root.clone(),
            docker: PathBuf::from("docker"),
            network: cfg.network.clone(),
        }
    }

    /// Override the docker binary (useful for unusual installs).
    pub fn with_docker_binary(mut self, docker: impl Into<PathBuf>) -> Self {
        self.docker = docker.into();
        self
    }

    fn compose_path(&self, id: &str) -> PathBuf {
        self.root.join("instances").join(id).join("compose.yaml")
    }

    fn project(&self, id: &str) -> String {
        format!("hermes-{id}")
    }

    fn command(&self, id: &str) -> Command {
        let mut command = Command::new(&self.docker);
        command
            .arg("compose")
            .arg("-f")
            .arg(self.compose_path(id))
            .arg("-p")
            .arg(self.project(id));
        command.stdin(Stdio::null());
        command.kill_on_drop(true);
        command
    }

    async fn compose(
        &self,
        id: &str,
        args: &[&str],
        operation: &str,
        timeout: Duration,
    ) -> Result<String, DriverError> {
        let mut command = self.command(id);
        command.args(args);
        run_command(command, operation, timeout).await
    }

    /// Make sure the external `fleet` network exists.
    ///
    /// Returns `true` when the network had to be created.
    pub async fn ensure_network(&self) -> Result<bool, DriverError> {
        let mut inspect = Command::new(&self.docker);
        inspect
            .args(["network", "inspect", &self.network])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let exists = matches!(
            tokio::time::timeout(PS_TIMEOUT, inspect.output()).await,
            Ok(Ok(output)) if output.status.success()
        );
        if exists {
            return Ok(false);
        }

        let mut create = Command::new(&self.docker);
        create
            .args(["network", "create", &self.network])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        run_command(create, "network create", CMD_TIMEOUT).await?;
        Ok(true)
    }
}

async fn run_command(
    mut command: Command,
    operation: &str,
    timeout: Duration,
) -> Result<String, DriverError> {
    match tokio::time::timeout(timeout, command.output()).await {
        Err(_) => Err(DriverError::new(
            operation,
            format!("timed out after {}s", timeout.as_secs()),
        )),
        Ok(Err(err)) => Err(DriverError::new(
            operation,
            format!("could not run docker: {err}"),
        )),
        Ok(Ok(output)) => {
            if output.status.success() {
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if stderr.is_empty() { stdout } else { stderr };
            Err(DriverError::new(
                operation,
                if detail.is_empty() {
                    format!("exit status {}", output.status)
                } else {
                    detail
                },
            ))
        }
    }
}

impl Driver for ComposeDriver {
    fn name(&self) -> &'static str {
        "compose"
    }

    fn ping(&self) -> DriverFuture<'_, bool> {
        Box::pin(async move {
            let mut command = Command::new(&self.docker);
            command
                .args(["version", "--format", "{{.Server.Version}}"])
                .stdin(Stdio::null())
                .kill_on_drop(true);
            matches!(
                tokio::time::timeout(PS_TIMEOUT, command.output()).await,
                Ok(Ok(output)) if output.status.success()
            )
        })
    }

    fn up<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.compose(
                &spec.id,
                &["up", "-d", "--remove-orphans"],
                "up",
                UP_TIMEOUT,
            )
            .await
            .map(|_| ())
        })
    }

    fn stop<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.compose(&spec.id, &["stop"], "stop", CMD_TIMEOUT)
                .await
                .map(|_| ())
        })
    }

    fn start<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.compose(&spec.id, &["start"], "start", CMD_TIMEOUT)
                .await
                .map(|_| ())
        })
    }

    fn restart<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.compose(&spec.id, &["restart"], "restart", CMD_TIMEOUT)
                .await
                .map(|_| ())
        })
    }

    fn down<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.compose(
                &spec.id,
                &["down", "--remove-orphans"],
                "down",
                DOWN_TIMEOUT,
            )
            .await
            .map(|_| ())
        })
    }

    fn ps<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<PsResult, DriverError>> {
        Box::pin(async move {
            let stdout = self
                .compose(
                    &spec.id,
                    &["ps", "--all", "--format", "json"],
                    "ps",
                    PS_TIMEOUT,
                )
                .await?;
            Ok(parse_ps(&stdout))
        })
    }

    fn logs<'a>(
        &'a self,
        spec: &'a InstanceSpec,
        tail: u32,
    ) -> DriverFuture<'a, Result<String, DriverError>> {
        Box::pin(async move {
            let tail = tail.to_string();
            self.compose(
                &spec.id,
                &["logs", "--no-color", "--tail", &tail],
                "logs",
                LOGS_TIMEOUT,
            )
            .await
        })
    }
}

/// Parse `docker compose ps --format json`.
///
/// Compose has shipped both a JSON array and newline-delimited objects over
/// time, and field casing is not stable across versions, so both shapes and a
/// case-insensitive lookup are accepted.
fn parse_ps(stdout: &str) -> PsResult {
    let Some(object) = parse_ps_object(stdout) else {
        return PsResult {
            status: Status::Unknown,
            container: None,
        };
    };

    let state = field(&object, &["state"]).or_else(|| field(&object, &["status"]));
    let status = state
        .as_deref()
        .map(|value| {
            let lower = value.to_ascii_lowercase();
            if lower.starts_with("up") {
                Status::Running
            } else if lower.starts_with("exited") {
                Status::Stopped
            } else {
                Status::from_compose_state(value)
            }
        })
        .unwrap_or(Status::Unknown);

    PsResult {
        status,
        container: Some(ContainerInfo {
            id: field(&object, &["id", "containerid"]),
            image: field(&object, &["image"]),
            uptime_s: None,
        }),
    }
}

fn parse_ps_object(stdout: &str) -> Option<serde_json::Map<String, Value>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value_object(value);
    }
    trimmed
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find_map(value_object)
}

fn value_object(value: Value) -> Option<serde_json::Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map),
        Value::Array(items) => items.into_iter().find_map(|item| match item {
            Value::Object(map) => Some(map),
            _ => None,
        }),
        _ => None,
    }
}

fn field(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    for (key, value) in object {
        let key = key.to_ascii_lowercase();
        if !names.contains(&key.as_str()) {
            continue;
        }
        if let Some(text) = value.as_str() {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
        if let Some(number) = value.as_u64() {
            return Some(number.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_array_output() {
        let stdout = r#"[{"ID":"abc123","Image":"hermes-agent:0.22.2-local","State":"running","Status":"Up 2 minutes"}]"#;
        let ps = parse_ps(stdout);
        assert_eq!(ps.status, Status::Running);
        let container = ps.container.expect("container");
        assert_eq!(container.id.as_deref(), Some("abc123"));
        assert_eq!(
            container.image.as_deref(),
            Some("hermes-agent:0.22.2-local")
        );
    }

    #[test]
    fn parses_ndjson_output() {
        let stdout = "{\"Id\":\"deadbeef\",\"State\":\"exited\",\"Image\":\"img\"}\n";
        let ps = parse_ps(stdout);
        assert_eq!(ps.status, Status::Stopped);
        assert_eq!(
            ps.container.and_then(|container| container.id).as_deref(),
            Some("deadbeef")
        );
    }

    #[test]
    fn empty_output_is_unknown() {
        let ps = parse_ps("");
        assert_eq!(ps.status, Status::Unknown);
        assert!(ps.container.is_none());
    }
}
