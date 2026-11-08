//! Docker Compose driver.
//!
//! Every instance is a separate compose project driven through
//! `docker compose -f <root>/instances/<id>/compose.yaml -p <project>`. The
//! project name is read from the `name:` field of the instance compose file, so
//! projects created by older releases (`hermes-<id>`) remain manageable without
//! being recreated.

use super::{Driver, DriverError, DriverFuture, PsResult};
use crate::config::Config;
use crate::db::Db;
use crate::models::{ContainerInfo, Health, InstanceSpec, Status};
use crate::render;
use crate::secrets;
use crate::store::compose_project_name;
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
    cfg: Config,
    docker: PathBuf,
    /// Encrypted secrets live in the index database; without it the driver
    /// falls back to a legacy `secrets.env` file.
    db: Option<Db>,
}

impl ComposeDriver {
    pub fn new(cfg: &Config) -> Self {
        Self {
            cfg: cfg.clone(),
            docker: PathBuf::from("docker"),
            db: None,
        }
    }

    /// Read the instance secrets from the encrypted store and inject them into
    /// every `docker compose` child process.
    pub fn with_secrets(mut self, db: Db) -> Self {
        self.db = Some(db);
        self
    }

    /// Override the docker binary (useful for unusual installs).
    pub fn with_docker_binary(mut self, docker: impl Into<PathBuf>) -> Self {
        self.docker = docker.into();
        self
    }

    fn compose_path(&self, id: &str) -> PathBuf {
        self.cfg
            .root
            .join("instances")
            .join(id)
            .join("compose.yaml")
    }

    /// Project name declared by the instance compose file.
    ///
    /// Reading it back (instead of computing a prefix) keeps projects created
    /// by older releases, declared as `hermes-<id>`, fully manageable after an
    /// upgrade: no container is ever recreated by a start, stop or logs call.
    fn project(&self, id: &str) -> String {
        std::fs::read_to_string(self.compose_path(id))
            .ok()
            .and_then(|text| compose_project_name(&text))
            .unwrap_or_else(|| render::project_name(id))
    }

    /// Environment of the compose process: the instance secrets, decrypted.
    ///
    /// The compose file declares `${KEY}` for every secret, so the values only
    /// exist in this child process (`docker inspect` sees them per container).
    fn environment(
        &self,
        id: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, DriverError> {
        let secrets = match &self.db {
            Some(db) => secrets::load(&self.cfg, db, id),
            None => Ok(crate::store::read_legacy_secrets(&self.cfg, id)),
        };
        secrets.map_err(|err| DriverError::new("secrets", err.to_string()))
    }

    fn command_for(&self, id: &str, project: &str) -> Result<Command, DriverError> {
        let environment = self.environment(id)?;
        let mut command = Command::new(&self.docker);
        command
            .arg("compose")
            .arg("-f")
            .arg(self.compose_path(id))
            .arg("-p")
            .arg(project);
        // An empty map keeps the parent environment untouched (a fresh driver
        // without secrets must still be able to run `docker compose down`).
        command.envs(environment);
        command.stdin(Stdio::null());
        command.kill_on_drop(true);
        Ok(command)
    }

    fn command(&self, id: &str) -> Result<Command, DriverError> {
        self.command_for(id, &self.project(id))
    }

    async fn compose(
        &self,
        id: &str,
        args: &[&str],
        operation: &str,
        timeout: Duration,
    ) -> Result<String, DriverError> {
        let mut command = self.command(id)?;
        command.args(args);
        run_command(command, operation, timeout).await
    }

    /// Run a compose operation against an explicit project name.
    ///
    /// The migration command uses it to talk to the legacy project before the
    /// compose file is rewritten.
    pub async fn project_operation(
        &self,
        id: &str,
        project: &str,
        args: &[&str],
        operation: &str,
        timeout: Duration,
    ) -> Result<String, DriverError> {
        let mut command = self.command_for(id, project)?;
        command.args(args);
        run_command(command, operation, timeout).await
    }

    /// `docker compose ps` for an explicit project name.
    pub async fn project_status(&self, id: &str, project: &str) -> Result<PsResult, DriverError> {
        let stdout = self
            .project_operation(
                id,
                project,
                &["ps", "--all", "--format", "json"],
                "ps",
                PS_TIMEOUT,
            )
            .await?;
        Ok(parse_ps(&stdout))
    }

    /// `docker compose down` for an explicit project name.
    pub async fn project_down(&self, id: &str, project: &str) -> Result<(), DriverError> {
        self.project_operation(
            id,
            project,
            &["down", "--remove-orphans"],
            "down",
            DOWN_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// `docker compose up -d` for an explicit project name.
    pub async fn project_up(&self, id: &str, project: &str) -> Result<(), DriverError> {
        self.project_operation(
            id,
            project,
            &["up", "-d", "--remove-orphans"],
            "up",
            UP_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Make sure the external `fleett` network exists.
    ///
    /// Returns `true` when the network had to be created.
    pub async fn ensure_network_named(&self, name: &str) -> Result<bool, DriverError> {
        let mut inspect = Command::new(&self.docker);
        inspect
            .args(["network", "inspect", name])
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
            .args(["network", "create", name])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        run_command(create, "network create", CMD_TIMEOUT).await?;
        Ok(true)
    }

    /// Raw `docker network inspect <name>` output.
    async fn inspect_network(&self, name: &str) -> Result<String, DriverError> {
        let mut command = Command::new(&self.docker);
        command
            .args(["network", "inspect", name])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        run_command(command, "network inspect", PS_TIMEOUT).await
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

    fn ensure_network<'a>(&'a self, name: &'a str) -> DriverFuture<'a, Result<bool, DriverError>> {
        Box::pin(async move { self.ensure_network_named(name).await })
    }

    fn network_subnet<'a>(
        &'a self,
        name: &'a str,
    ) -> DriverFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(async move {
            let json = self.inspect_network(name).await?;
            crate::egress::first_ipv4_subnet(&json)
                .map(Some)
                .map_err(|detail| DriverError::new("network inspect", detail))
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
            health: Health::Unknown,
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
        health: health_field(&object),
        container: Some(ContainerInfo {
            id: field(&object, &["id", "containerid"]),
            image: field(&object, &["image"]),
            uptime_s: None,
        }),
    }
}

/// Read the `Health` field of a `docker compose ps` object.
///
/// Compose exposes it as a string (`"healthy"`, `"unhealthy"`, `"starting"` or
/// empty); some versions nest it as `{"Status": "healthy"}`. Anything absent or
/// unrecognised maps to [`Health::Unknown`].
fn health_field(object: &serde_json::Map<String, Value>) -> Health {
    for (key, value) in object {
        if !key.eq_ignore_ascii_case("health") {
            continue;
        }
        if let Some(text) = value.as_str() {
            return Health::parse(text);
        }
        if let Some(nested) = value.as_object() {
            for (key, value) in nested {
                if key.eq_ignore_ascii_case("status") {
                    if let Some(text) = value.as_str() {
                        return Health::parse(text);
                    }
                }
            }
        }
    }
    Health::Unknown
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

    fn driver_in(dir: &std::path::Path) -> ComposeDriver {
        ComposeDriver::new(&Config::for_tests(dir.to_path_buf()))
    }

    #[test]
    fn reads_the_project_name_from_the_compose_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let driver = driver_in(dir.path());
        std::fs::create_dir_all(dir.path().join("instances/alice")).expect("instance dir");
        std::fs::write(
            dir.path().join("instances/alice/compose.yaml"),
            "name: hermes-alice\nservices: {}\n",
        )
        .expect("compose file");
        assert_eq!(driver.project("alice"), "hermes-alice");
    }

    #[test]
    fn project_name_falls_back_to_the_new_prefix() {
        let dir = tempfile::tempdir().expect("temp dir");
        let driver = driver_in(dir.path());
        std::fs::create_dir_all(dir.path().join("instances/bob")).expect("instance dir");
        for compose in ["services: {}\n", "name: \"  \"\n", "name: [unclosed\n"] {
            std::fs::write(dir.path().join("instances/bob/compose.yaml"), compose)
                .expect("compose file");
            assert_eq!(driver.project("bob"), "fleett-bob", "{compose:?}");
        }
        std::fs::remove_file(dir.path().join("instances/bob/compose.yaml")).expect("remove");
        assert_eq!(driver.project("bob"), "fleett-bob");
    }

    #[test]
    fn docker_commands_use_the_declared_project_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let driver = driver_in(dir.path());
        std::fs::create_dir_all(dir.path().join("instances/alice")).expect("instance dir");
        std::fs::write(
            dir.path().join("instances/alice/compose.yaml"),
            "name: hermes-alice\nservices: {}\n",
        )
        .expect("compose file");

        let command = driver.command("alice").expect("command");
        let args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let project = args
            .iter()
            .position(|arg| arg == "-p")
            .map(|index| args[index + 1].clone());
        assert_eq!(project.as_deref(), Some("hermes-alice"));
        assert!(args
            .iter()
            .any(|arg| arg.ends_with("instances/alice/compose.yaml")));
    }

    fn env_of(command: &Command) -> std::collections::BTreeMap<String, String> {
        command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value
                        .map(|value| value.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
            })
            .collect()
    }

    #[test]
    fn compose_commands_receive_the_decrypted_secrets() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        std::fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        std::fs::write(
            cfg.compose_path("alice"),
            "name: fleett-alice\nservices: {}\n",
        )
        .expect("compose file");
        cfg.secrets_key = Some(secrets::encode_key(&[7u8; 32]));

        let db = Db::open(&cfg.db_path()).expect("db");
        let mut stored = std::collections::BTreeMap::new();
        stored.insert("TELEGRAM_BOT_TOKEN".to_string(), "123:ABC".to_string());
        stored.insert("HERMES_GATE_PROFILE_TOKEN".to_string(), "gate".to_string());
        secrets::store(&cfg, &db, "alice", &stored).expect("store secrets");

        let driver = ComposeDriver::new(&cfg).with_secrets(db);
        let environment = env_of(&driver.command("alice").expect("command"));
        assert_eq!(
            environment.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
            Some("123:ABC")
        );
        assert_eq!(
            environment
                .get("HERMES_GATE_PROFILE_TOKEN")
                .map(String::as_str),
            Some("gate")
        );
    }

    #[test]
    fn compose_commands_fall_back_to_a_legacy_secrets_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        std::fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        std::fs::write(
            cfg.compose_path("alice"),
            "name: fleett-alice\nservices: {}\n",
        )
        .expect("compose file");
        let mut stored = std::collections::BTreeMap::new();
        stored.insert("DISCORD_BOT_TOKEN".to_string(), "legacy-token".to_string());
        crate::store::write_legacy_secrets(&cfg, "alice", &stored).expect("legacy file");

        let db = Db::open(&cfg.db_path()).expect("db");
        let driver = ComposeDriver::new(&cfg).with_secrets(db);
        let environment = env_of(&driver.command("alice").expect("command"));
        assert_eq!(
            environment.get("DISCORD_BOT_TOKEN").map(String::as_str),
            Some("legacy-token")
        );
    }

    #[test]
    fn an_encrypted_instance_without_a_key_fails_loudly() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        std::fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        std::fs::write(
            cfg.compose_path("alice"),
            "name: fleett-alice\nservices: {}\n",
        )
        .expect("compose file");
        cfg.secrets_key = Some(secrets::encode_key(&[7u8; 32]));
        let db = Db::open(&cfg.db_path()).expect("db");
        let mut stored = std::collections::BTreeMap::new();
        stored.insert("TELEGRAM_BOT_TOKEN".to_string(), "123:ABC".to_string());
        secrets::store(&cfg, &db, "alice", &stored).expect("store");

        // Same database, key gone: the compose command must not run with an
        // empty token.
        cfg.secrets_key = None;
        cfg.secrets_key_file = cfg.root.with_extension("missing.key");
        let driver = ComposeDriver::new(&cfg).with_secrets(db);
        let err = driver.command("alice").expect_err("no key");
        assert_eq!(err.operation, "secrets");
        assert!(err.detail.contains("fleett secrets init"), "{err}");
    }

    #[test]
    fn parses_array_output() {
        let stdout = r#"[{"ID":"abc123","Image":"hermes-agent:0.22.2-local","State":"running","Status":"Up 2 minutes","Health":"healthy"}]"#;
        let ps = parse_ps(stdout);
        assert_eq!(ps.status, Status::Running);
        assert_eq!(ps.health, Health::Healthy);
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
        assert_eq!(ps.health, Health::Unknown);
        assert_eq!(
            ps.container.and_then(|container| container.id).as_deref(),
            Some("deadbeef")
        );
    }

    #[test]
    fn parses_health_from_both_shapes() {
        let cases = [
            (
                r#"[{"State":"running","Health":"unhealthy"}]"#,
                Health::Unhealthy,
            ),
            (
                "{\"State\":\"running\",\"Health\":\"starting\"}\n",
                Health::Starting,
            ),
            (
                r#"[{"State":"running","Health":{"Status":"healthy"}}]"#,
                Health::Healthy,
            ),
            (r#"[{"State":"running","Health":""}]"#, Health::Unknown),
            (r#"[{"State":"running"}]"#, Health::Unknown),
        ];
        for (stdout, expected) in cases {
            assert_eq!(parse_ps(stdout).health, expected, "{stdout}");
        }
    }

    #[test]
    fn empty_output_is_unknown() {
        let ps = parse_ps("");
        assert_eq!(ps.status, Status::Unknown);
        assert_eq!(ps.health, Health::Unknown);
        assert!(ps.container.is_none());
    }
}
