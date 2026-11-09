//! Docker Compose driver.
//!
//! Every instance is a separate compose project driven through
//! `docker compose -f <root>/instances/<id>/compose.yaml -p <project>`. The
//! project name is read from the `name:` field of the instance compose file, so
//! projects created by older releases (`hermes-<id>`) remain manageable without
//! being recreated.

use super::{
    Driver, DriverError, DriverFuture, ExecResult, ImageRecord, InspectInfo, LogsOptions, PsResult,
    UsageSample,
};
use crate::config::Config;
use crate::db::Db;
use crate::hosts::{self, HostCapacity, HostEndpoint, HostSpec};
use crate::models::{ContainerInfo, Health, InstanceSpec, Status};
use crate::render;
use crate::store::compose_project_name;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const UP_TIMEOUT: Duration = Duration::from_secs(180);
const DOWN_TIMEOUT: Duration = Duration::from_secs(120);
const CMD_TIMEOUT: Duration = Duration::from_secs(60);
const PS_TIMEOUT: Duration = Duration::from_secs(20);
const LOGS_TIMEOUT: Duration = Duration::from_secs(30);
const INSPECT_TIMEOUT: Duration = Duration::from_secs(20);
const STATS_TIMEOUT: Duration = Duration::from_secs(30);
const PULL_TIMEOUT: Duration = Duration::from_secs(1800);
const IMAGES_TIMEOUT: Duration = Duration::from_secs(30);

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

    /// Environment of the compose process: every instance variable, decrypted
    /// for the secret rows and plaintext for the others, with the provider
    /// secret injected under the instance [`InstanceSpec::api_key_env`] when a
    /// reusable provider is attached (it always wins over a per-instance value
    /// of that key).
    ///
    /// The compose file declares `${KEY}` for every variable, so the values
    /// only exist in this child process (`docker inspect` sees them per
    /// container).
    fn environment(
        &self,
        id: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, DriverError> {
        let values = match &self.db {
            Some(db) => crate::providers::resolved_env(&self.cfg, db, id),
            None => Ok(crate::store::read_legacy_secrets(&self.cfg, id)),
        };
        values.map_err(|err| DriverError::new("secrets", err.to_string()))
    }

    /// Endpoint of one instance host id.
    ///
    /// Resolved through the registry at every call, so editing a host address
    /// takes effect on the next lifecycle operation without a restart.
    fn endpoint(&self, host: &str) -> Result<HostEndpoint, DriverError> {
        hosts::endpoint_for(self.db.as_ref(), host)
            .map_err(|detail| DriverError::new("host", detail))
    }

    /// Endpoint of one host row (used by the connectivity probe).
    fn host_endpoint(host: &HostSpec) -> HostEndpoint {
        hosts::endpoint(host)
    }

    fn apply_endpoint(&self, command: &mut Command, host: &str) -> Result<(), DriverError> {
        self.endpoint(host)?.apply(command);
        Ok(())
    }

    fn command_for(&self, id: &str, project: &str, host: &str) -> Result<Command, DriverError> {
        let mut environment = self.environment(id)?;
        // A per-instance variable must never redirect the daemon selector: the
        // host registry owns DOCKER_HOST / DOCKER_CONTEXT.
        environment.remove("DOCKER_HOST");
        environment.remove("DOCKER_CONTEXT");
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
        self.apply_endpoint(&mut command, host)?;
        command.stdin(Stdio::null());
        command.kill_on_drop(true);
        Ok(command)
    }

    /// Compose command against the local daemon (tests only).
    #[cfg(test)]
    fn command(&self, id: &str) -> Result<Command, DriverError> {
        self.command_for(id, &self.project(id), hosts::LOCAL_HOST)
    }

    async fn compose(
        &self,
        id: &str,
        host: &str,
        args: &[&str],
        operation: &str,
        timeout: Duration,
    ) -> Result<String, DriverError> {
        let mut command = self.command_for(id, &self.project(id), host)?;
        command.args(args);
        run_command(command, operation, timeout).await
    }

    /// Run a compose operation against an explicit project name.
    ///
    /// The migration command uses it to talk to the legacy project before the
    /// compose file is rewritten. Legacy projects only ever lived on the local
    /// host, so this stays local.
    pub async fn project_operation(
        &self,
        id: &str,
        project: &str,
        args: &[&str],
        operation: &str,
        timeout: Duration,
    ) -> Result<String, DriverError> {
        let mut command = self.command_for(id, project, hosts::LOCAL_HOST)?;
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

    /// Make sure the external `fleett` network exists on one host.
    ///
    /// Returns `true` when the network had to be created.
    pub async fn ensure_network_named(&self, host: &str, name: &str) -> Result<bool, DriverError> {
        let mut inspect = Command::new(&self.docker);
        inspect
            .args(["network", "inspect", name])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        self.apply_endpoint(&mut inspect, host)?;
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
        self.apply_endpoint(&mut create, host)?;
        run_command(create, "network create", CMD_TIMEOUT).await?;
        Ok(true)
    }

    /// Raw `docker network inspect <name>` output.
    async fn inspect_network(&self, host: &str, name: &str) -> Result<String, DriverError> {
        let mut command = Command::new(&self.docker);
        command
            .args(["network", "inspect", name])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        self.apply_endpoint(&mut command, host)?;
        run_command(command, "network inspect", PS_TIMEOUT).await
    }

    /// `docker stats --no-stream --format json` for every running container of
    /// one host.
    ///
    /// The whole host is sampled in one call and the result is keyed by
    /// container name, so instances whose container is not running simply have
    /// no entry (and therefore report zero usage).
    async fn docker_stats(&self, host: &str) -> Result<HashMap<String, StatsSample>, DriverError> {
        let mut command = Command::new(&self.docker);
        command
            .args(["stats", "--no-stream", "--format", "json"])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        self.apply_endpoint(&mut command, host)?;
        let stdout = run_command(command, "stats", STATS_TIMEOUT).await?;
        Ok(parse_stats(&stdout))
    }

    /// `docker info` of one host row, parsed for CPU, memory and version.
    async fn probe(&self, host: &HostSpec) -> Result<HostCapacity, DriverError> {
        let mut command = Command::new(&self.docker);
        command
            .args(["info", "--format", "{{json .}}"])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        Self::host_endpoint(host).apply(&mut command);
        let stdout = run_command(command, "info", PS_TIMEOUT).await?;
        parse_info(&stdout).ok_or_else(|| DriverError::new("info", "unparsable docker info output"))
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

    fn ensure_network<'a>(
        &'a self,
        host: &'a str,
        name: &'a str,
    ) -> DriverFuture<'a, Result<bool, DriverError>> {
        Box::pin(async move { self.ensure_network_named(host, name).await })
    }

    fn network_subnet<'a>(
        &'a self,
        host: &'a str,
        name: &'a str,
    ) -> DriverFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(async move {
            let json = self.inspect_network(host, name).await?;
            crate::egress::first_ipv4_subnet(&json)
                .map(Some)
                .map_err(|detail| DriverError::new("network inspect", detail))
        })
    }

    fn probe_host<'a>(
        &'a self,
        host: &'a HostSpec,
    ) -> DriverFuture<'a, Result<HostCapacity, DriverError>> {
        Box::pin(async move { self.probe(host).await })
    }

    fn up<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.compose(
                &spec.id,
                &spec.host,
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
            self.compose(&spec.id, &spec.host, &["stop"], "stop", CMD_TIMEOUT)
                .await
                .map(|_| ())
        })
    }

    fn start<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.compose(&spec.id, &spec.host, &["start"], "start", CMD_TIMEOUT)
                .await
                .map(|_| ())
        })
    }

    fn restart<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.compose(&spec.id, &spec.host, &["restart"], "restart", CMD_TIMEOUT)
                .await
                .map(|_| ())
        })
    }

    fn down<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            self.compose(
                &spec.id,
                &spec.host,
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
                    &spec.host,
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
        options: &'a LogsOptions,
    ) -> DriverFuture<'a, Result<String, DriverError>> {
        Box::pin(async move {
            let args = logs_arguments(options.tail, options.since.as_deref());
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            self.compose(&spec.id, &spec.host, &args, "logs", LOGS_TIMEOUT)
                .await
        })
    }

    fn inspect<'a>(
        &'a self,
        spec: &'a InstanceSpec,
    ) -> DriverFuture<'a, Result<InspectInfo, DriverError>> {
        Box::pin(async move {
            // `container_name` is always the compose project name (see
            // `render::render_compose`), and reading the declared project keeps
            // containers created by older releases (`hermes-<id>`) addressable.
            let container = self.project(&spec.id);
            let mut command = Command::new(&self.docker);
            command
                .args(["inspect", "--type", "container", &container])
                .stdin(Stdio::null())
                .kill_on_drop(true);
            self.apply_endpoint(&mut command, &spec.host)?;
            let stdout = run_command(command, "inspect", INSPECT_TIMEOUT).await?;
            parse_inspect(&stdout)
                .ok_or_else(|| DriverError::new("inspect", "unparsable docker inspect output"))
        })
    }

    fn log_path<'a>(
        &'a self,
        spec: &'a InstanceSpec,
    ) -> DriverFuture<'a, Result<Option<PathBuf>, DriverError>> {
        Box::pin(async move {
            let container = self.project(&spec.id);
            let mut command = Command::new(&self.docker);
            command
                .args([
                    "inspect",
                    "--type",
                    "container",
                    "--format",
                    "{{.LogPath}}",
                    &container,
                ])
                .stdin(Stdio::null())
                .kill_on_drop(true);
            self.apply_endpoint(&mut command, &spec.host)?;
            let stdout = run_command(command, "inspect", INSPECT_TIMEOUT).await?;
            let path = stdout.trim();
            Ok((!path.is_empty() && path != "<no value>").then(|| PathBuf::from(path)))
        })
    }

    fn image_size<'a>(
        &'a self,
        image: &'a str,
    ) -> DriverFuture<'a, Result<Option<u64>, DriverError>> {
        Box::pin(async move {
            let mut command = Command::new(&self.docker);
            command
                .args(["image", "inspect", "--format", "{{.Size}}", image])
                .stdin(Stdio::null())
                .kill_on_drop(true);
            // A missing tag is not an error for a read-only detail view.
            match run_command(command, "image inspect", INSPECT_TIMEOUT).await {
                Ok(stdout) => Ok(stdout.trim().parse::<u64>().ok()),
                Err(_) => Ok(None),
            }
        })
    }

    fn usage<'a>(
        &'a self,
        specs: &'a [InstanceSpec],
    ) -> DriverFuture<'a, Result<Vec<UsageSample>, DriverError>> {
        Box::pin(async move {
            // An empty fleet never talks to Docker: `fleett usage` on a fresh
            // host must succeed even when the daemon is down.
            if specs.is_empty() {
                return Ok(Vec::new());
            }

            // Group by host so each daemon is sampled exactly once. The caller
            // (API, metrics) decides how to degrade when one host is down; this
            // call stays all-or-nothing for the hosts it was given.
            let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
            for (index, spec) in specs.iter().enumerate() {
                groups.entry(spec.host.as_str()).or_default().push(index);
            }
            let mut samples = vec![UsageSample::default(); specs.len()];
            for (host, indices) in groups {
                let group: Vec<&InstanceSpec> =
                    indices.iter().map(|index| &specs[*index]).collect();
                let stats = self.docker_stats(host).await?;
                let cfg = self.cfg.clone();
                let ids: Vec<String> = group.iter().map(|spec| spec.id.clone()).collect();
                let disks = tokio::task::spawn_blocking(move || {
                    let mut sizes = Vec::with_capacity(ids.len());
                    for id in ids {
                        sizes.push(du_bytes(&cfg.data_dir(&id))?);
                    }
                    Ok::<Vec<u64>, String>(sizes)
                })
                .await
                .map_err(|err| {
                    DriverError::new("du", format!("the disk usage task failed: {err}"))
                })?
                .map_err(|detail| DriverError::new("du", detail))?;

                for ((index, spec), disk_bytes) in indices.into_iter().zip(group).zip(disks) {
                    let configured_limit = u64::from(spec.mem_limit_mb) * 1024 * 1024;
                    samples[index] = match stats.get(&self.project(&spec.id)) {
                        Some(sample) => UsageSample {
                            running: true,
                            cpu_pct: sample.cpu_pct,
                            mem_used_bytes: sample.mem_used_bytes,
                            mem_limit_bytes: if sample.mem_limit_bytes > 0 {
                                sample.mem_limit_bytes
                            } else {
                                configured_limit
                            },
                            disk_bytes,
                        },
                        None => UsageSample {
                            running: false,
                            cpu_pct: 0.0,
                            mem_used_bytes: 0,
                            mem_limit_bytes: configured_limit,
                            disk_bytes,
                        },
                    };
                }
            }
            Ok(samples)
        })
    }

    fn exec<'a>(
        &'a self,
        spec: &'a InstanceSpec,
        argv: &'a [String],
        timeout_s: u64,
    ) -> DriverFuture<'a, Result<ExecResult, DriverError>> {
        Box::pin(async move {
            // `-T` disables the TTY, so the output is plain bytes and the call
            // never hangs waiting for a terminal. The service name is always
            // `agent` (see `render::render_compose`).
            let mut command = self.command_for(&spec.id, &self.project(&spec.id), &spec.host)?;
            command.args(["exec", "-T", "agent"]);
            command.args(argv);
            command.stdin(Stdio::null()).kill_on_drop(true);
            let timeout = Duration::from_secs(timeout_s.max(1));
            match tokio::time::timeout(timeout, command.output()).await {
                Err(_) => Ok(ExecResult {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("timed out after {}s", timeout.as_secs()),
                    timed_out: true,
                    duration_ms: 0,
                    truncated: false,
                }),
                Ok(Err(err)) => Err(DriverError::new(
                    "exec",
                    format!("could not run docker: {err}"),
                )),
                Ok(Ok(output)) => Ok(ExecResult {
                    exit_code: output.status.code(),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    timed_out: false,
                    duration_ms: 0,
                    truncated: false,
                }),
            }
        })
    }

    fn ensure_image<'a>(
        &'a self,
        host: &'a str,
        image: &'a str,
    ) -> DriverFuture<'a, Result<(), DriverError>> {
        Box::pin(async move {
            // `docker image inspect` exits non-zero when the tag is absent, so
            // its output is not parsed: only the exit status matters.
            let mut inspect = Command::new(&self.docker);
            inspect
                .args(["image", "inspect", image])
                .stdin(Stdio::null())
                .kill_on_drop(true);
            self.apply_endpoint(&mut inspect, host)?;
            let present = matches!(
                tokio::time::timeout(PS_TIMEOUT, inspect.output()).await,
                Ok(Ok(output)) if output.status.success()
            );
            if present {
                return Ok(());
            }

            let mut pull = Command::new(&self.docker);
            pull.args(["pull", image])
                .stdin(Stdio::null())
                .kill_on_drop(true);
            self.apply_endpoint(&mut pull, host)?;
            run_command(pull, "image pull", PULL_TIMEOUT)
                .await
                .map(|_| ())
        })
    }

    fn list_images<'a>(
        &'a self,
        host: &'a str,
    ) -> DriverFuture<'a, Result<Vec<ImageRecord>, DriverError>> {
        Box::pin(async move {
            let mut command = Command::new(&self.docker);
            command
                .args(["images", "--format", "json"])
                .stdin(Stdio::null())
                .kill_on_drop(true);
            self.apply_endpoint(&mut command, host)?;
            let stdout = run_command(command, "images", IMAGES_TIMEOUT).await?;
            Ok(parse_images(&stdout))
        })
    }
}

/// One line of `docker stats --no-stream --format json`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct StatsSample {
    cpu_pct: f64,
    mem_used_bytes: u64,
    mem_limit_bytes: u64,
}

/// Parse `docker stats --no-stream --format json` (one object per line).
///
/// An array is accepted too, and unnamed or malformed entries are ignored:
/// another container on the host must never break `GET /api/usage`.
fn parse_stats(stdout: &str) -> HashMap<String, StatsSample> {
    let mut samples = HashMap::new();
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return samples;
    }
    let values: Vec<Value> = match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Array(items)) => items,
        Ok(value @ Value::Object(_)) => vec![value],
        _ => trimmed
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
            .collect(),
    };
    for value in values {
        let Value::Object(object) = value else {
            continue;
        };
        let Some(name) = field(&object, &["name"]) else {
            continue;
        };
        let name = name.trim_start_matches('/').to_string();
        if name.is_empty() {
            continue;
        }
        let cpu_pct = field(&object, &["cpuperc"])
            .and_then(|value| parse_percent(&value))
            .unwrap_or(0.0);
        let (mem_used_bytes, mem_limit_bytes) = field(&object, &["memusage"])
            .and_then(|value| parse_mem_usage(&value))
            .unwrap_or((0, 0));
        samples.insert(
            name,
            StatsSample {
                cpu_pct,
                mem_used_bytes,
                mem_limit_bytes,
            },
        );
    }
    samples
}

/// `"1.50%"` → `1.5`.
fn parse_percent(value: &str) -> Option<f64> {
    value
        .trim()
        .trim_end_matches('%')
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// `"128.5MiB / 1.5GiB"` → `(used, limit)`.
fn parse_mem_usage(value: &str) -> Option<(u64, u64)> {
    let (used, limit) = value.split_once('/')?;
    Some((parse_size(used)?, parse_size(limit)?))
}

/// Parse a Docker size string (`0B`, `512KiB`, `1.5GiB`, `12MB`).
fn parse_size(value: &str) -> Option<u64> {
    let text = value.trim();
    let split = text
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .unwrap_or(text.len());
    if split == 0 {
        return None;
    }
    let number = text[..split].parse::<f64>().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    let multiplier = match text[split..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((number * multiplier).round() as u64)
}

/// Size of `path` in bytes, from `du -sb`.
///
/// A missing directory is an empty one: an instance whose data was never
/// written must not fail the whole usage report.
fn du_bytes(path: &Path) -> Result<u64, String> {
    if !path.exists() {
        return Ok(0);
    }
    let output = std::process::Command::new("du")
        .arg("-sb")
        .arg(path)
        .output()
        .map_err(|err| format!("cannot run du: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("du -sb {} failed", path.display())
        } else {
            stderr
        });
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("unparsable du output for {}", path.display()))
}

/// Parse `docker info --format '{{json .}}'` into the capacity of a host.
///
/// Docker versions disagree on casing (`NCPU` vs `Ncpu`), so the lookup is
/// case-insensitive. Missing fields keep zero: capacity is best-effort and a
/// host without numbers can still be used explicitly.
fn parse_info(stdout: &str) -> Option<HostCapacity> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    let Value::Object(object) = value else {
        return None;
    };
    let number = |names: &[&str]| -> Option<u64> {
        object.iter().find_map(|(key, value)| {
            names
                .contains(&key.to_ascii_lowercase().as_str())
                .then(|| value.as_u64())
                .flatten()
        })
    };
    Some(HostCapacity {
        ncpu: number(&["ncpu"]).unwrap_or(0).min(u64::from(u32::MAX)) as u32,
        mem_total_bytes: number(&["memtotal"]).unwrap_or(0),
        docker_version: field(&object, &["serverversion"]),
    })
}

/// Parse `docker images --format json` (one object per line, an array is
/// accepted too).
///
/// Dangling images (`<none>:<none>`) are skipped: no instance may reference
/// them, and `fleett images` is meant to answer "what can I upgrade to".
fn parse_images(stdout: &str) -> Vec<ImageRecord> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let values: Vec<Value> = match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Array(items)) => items,
        Ok(value @ Value::Object(_)) => vec![value],
        _ => trimmed
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
            .collect(),
    };
    let mut images: Vec<ImageRecord> = Vec::new();
    for value in values {
        let Value::Object(object) = value else {
            continue;
        };
        let repository = field(&object, &["repository"]).unwrap_or_default();
        let tag = field(&object, &["tag"]).unwrap_or_default();
        if repository.is_empty() || tag.is_empty() || repository == "<none>" || tag == "<none>" {
            continue;
        }
        images.push(ImageRecord {
            tag: format!("{repository}:{tag}"),
            id: field(&object, &["id", "imageid"]).unwrap_or_default(),
            size: field(&object, &["size"]).unwrap_or_default(),
            created: field(&object, &["createdat", "createdsince", "created"]).unwrap_or_default(),
        });
    }
    images.sort_by(|left, right| left.tag.cmp(&right.tag));
    images
}

/// Arguments of `docker compose logs` for the given options.
///
/// Kept separate from the process call so the `--tail`/`--since` wiring is
/// testable without Docker.
fn logs_arguments(tail: u32, since: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "logs".to_string(),
        "--no-color".to_string(),
        "--tail".to_string(),
        tail.to_string(),
    ];
    if let Some(since) = since {
        args.push("--since".to_string());
        args.push(since.to_string());
    }
    args
}

/// Parse `docker inspect --type container <name>` output.
///
/// Docker returns an array with one object; the `State` block carries
/// `Status`, `ExitCode`, `OOMKilled`, `Error` and `StartedAt`.
fn parse_inspect(stdout: &str) -> Option<InspectInfo> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    let object = value_object(value)?;
    let state = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("state"))
        .and_then(|(_, value)| value.as_object())?;

    let text = |names: &[&str]| -> Option<String> {
        for (key, value) in state {
            let key = key.to_ascii_lowercase();
            if names.contains(&key.as_str()) {
                if let Some(text) = value.as_str() {
                    return Some(text.to_string());
                }
            }
        }
        None
    };
    let started_at = text(&["startedat"]).filter(|started| !is_zero_timestamp(started));

    Some(InspectInfo {
        state: text(&["status"]).unwrap_or_default(),
        exit_code: state
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("exitcode"))
            .and_then(|(_, value)| value.as_i64())
            .and_then(|code| i32::try_from(code).ok()),
        oom_killed: state
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("oomkilled"))
            .and_then(|(_, value)| value.as_bool())
            .unwrap_or(false),
        // `RestartCount` is a sibling of `State` in `docker inspect`, not a
        // member of it.
        restart_count: object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("restartcount"))
            .and_then(|(_, value)| value.as_u64())
            .map(|count| u32::try_from(count).unwrap_or(u32::MAX))
            .unwrap_or(0),
        error: text(&["error"]).filter(|error| !error.trim().is_empty()),
        started_at,
    })
}

/// `docker inspect` reports this placeholder before the first start.
fn is_zero_timestamp(value: &str) -> bool {
    value.starts_with("0001-01-01")
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
    use crate::secrets;

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

    #[test]
    fn a_remote_host_sets_docker_host_on_every_command() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        std::fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        std::fs::write(
            cfg.compose_path("alice"),
            "name: fleett-alice\nservices: {}\n",
        )
        .expect("compose file");

        let db = Db::open(&cfg.db_path()).expect("db");
        let now = crate::models::now_iso();
        db.insert_host(&HostSpec {
            id: "pve3".to_string(),
            name: "PVE 3".to_string(),
            transport: crate::hosts::TRANSPORT_SSH.to_string(),
            address: "cle@10.1.20.5".to_string(),
            docker_context: None,
            status: crate::hosts::STATUS_UNKNOWN.to_string(),
            last_check_at: None,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        })
        .expect("host");

        let driver = ComposeDriver::new(&cfg).with_secrets(db);
        let command = driver
            .command_for("alice", "fleett-alice", "pve3")
            .expect("command");
        let environment = env_of(&command);
        assert_eq!(
            environment.get("DOCKER_HOST").map(String::as_str),
            Some("ssh://cle@10.1.20.5")
        );

        // The local host adds no selector, so the process environment is
        // inherited untouched.
        let local = driver
            .command_for("alice", "fleett-alice", crate::hosts::LOCAL_HOST)
            .expect("local command");
        assert!(!env_of(&local).contains_key("DOCKER_HOST"));
    }

    #[test]
    fn docker_info_capacity_is_parsed() {
        let info = parse_info(
            r#"{"NCPU":8,"MemTotal":17179869184,"ServerVersion":"27.3.1","Containers":3}"#,
        )
        .expect("capacity");
        assert_eq!(info.ncpu, 8);
        assert_eq!(info.mem_total_bytes, 17_179_869_184);
        assert_eq!(info.docker_version.as_deref(), Some("27.3.1"));

        // Casing is not stable across Docker releases, and missing fields are
        // zero rather than a parse failure.
        let info = parse_info(r#"{"ncpu":2,"memtotal":1024}"#).expect("capacity");
        assert_eq!(info.ncpu, 2);
        assert_eq!(info.mem_total_bytes, 1024);
        assert_eq!(info.docker_version, None);
        assert!(parse_info("not json").is_none());
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
    fn compose_commands_receive_the_decrypted_secrets_and_the_plain_values() {
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
        let entries = vec![
            crate::envs::EnvWrite {
                key: "TELEGRAM_BOT_TOKEN".to_string(),
                value: "123:ABC".to_string(),
                secret: true,
            },
            crate::envs::EnvWrite {
                key: "HERMES_GATE_PROFILE_TOKEN".to_string(),
                value: "gate".to_string(),
                secret: true,
            },
            crate::envs::EnvWrite {
                key: "DISCORD_ALLOWED_USERS".to_string(),
                value: "42,43".to_string(),
                secret: false,
            },
            crate::envs::EnvWrite {
                key: "DOCKER_HOST".to_string(),
                value: "tcp://evil.test:2375".to_string(),
                secret: false,
            },
        ];
        crate::envs::replace(&cfg, &db, "alice", &entries).expect("store env");

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
        assert_eq!(
            environment.get("DISCORD_ALLOWED_USERS").map(String::as_str),
            Some("42,43"),
            "a non-secret value must reach the compose process too"
        );
        assert!(
            !environment.contains_key("DOCKER_HOST"),
            "an instance variable must never redirect the Docker daemon"
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
        let entries = vec![crate::envs::EnvWrite {
            key: "TELEGRAM_BOT_TOKEN".to_string(),
            value: "123:ABC".to_string(),
            secret: true,
        }];
        crate::envs::replace(&cfg, &db, "alice", &entries).expect("store");

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

    #[test]
    fn logs_arguments_carry_the_tail_and_since() {
        assert_eq!(
            logs_arguments(200, None),
            ["logs", "--no-color", "--tail", "200"]
                .map(str::to_string)
                .to_vec()
        );
        assert_eq!(
            logs_arguments(25, Some("10m")),
            ["logs", "--no-color", "--tail", "25", "--since", "10m"]
                .map(str::to_string)
                .to_vec()
        );
        assert_eq!(
            logs_arguments(2000, Some("2026-11-08T01:00:00Z")),
            [
                "logs",
                "--no-color",
                "--tail",
                "2000",
                "--since",
                "2026-11-08T01:00:00Z"
            ]
            .map(str::to_string)
            .to_vec()
        );
    }

    #[test]
    fn parses_a_running_container() {
        let stdout = r#"[{
            "Id": "abc",
            "Name": "/fleett-alice",
            "RestartCount": 4,
            "State": {
                "Status": "running",
                "Running": true,
                "ExitCode": 0,
                "OOMKilled": false,
                "Error": "",
                "StartedAt": "2026-11-08T01:02:03.456789Z",
                "FinishedAt": "0001-01-01T00:00:00Z"
            }
        }]"#;
        let info = parse_inspect(stdout).expect("parsed");
        assert_eq!(info.state, "running");
        assert_eq!(info.exit_code, Some(0));
        assert!(!info.oom_killed);
        assert_eq!(info.restart_count, 4);
        assert_eq!(info.error, None);
        assert_eq!(
            info.started_at.as_deref(),
            Some("2026-11-08T01:02:03.456789Z")
        );
        assert!(info.error_message().is_none());
    }

    #[test]
    fn parses_a_crashed_container() {
        let stdout = r#"[{"State":{"status":"exited","exitcode":137,"oomkilled":true,
            "error":"task: non-zero exit (137)","startedAt":"0001-01-01T00:00:00Z"}}]"#;
        let info = parse_inspect(stdout).expect("parsed");
        assert_eq!(info.state, "exited");
        assert_eq!(info.exit_code, Some(137));
        assert!(info.oom_killed);
        assert_eq!(info.error.as_deref(), Some("task: non-zero exit (137)"));
        assert_eq!(
            info.started_at, None,
            "the zero timestamp means never started"
        );
        let message = info.error_message().expect("failure");
        assert!(message.contains("137"), "{message}");
        assert!(message.contains("OOM"), "{message}");
    }

    #[test]
    fn inspect_output_must_be_json() {
        assert!(parse_inspect("").is_none());
        assert!(parse_inspect("Error: No such object: fleett-alice").is_none());
        assert!(
            parse_inspect(r#"[{"Id":"abc"}]"#).is_none(),
            "no State block"
        );
    }

    #[test]
    fn stats_lines_are_keyed_by_container_name() {
        let stdout = "\
{\"Name\":\"fleett-alice\",\"CPUPerc\":\"1.50%\",\"MemUsage\":\"128.5MiB / 1.5GiB\"}
{\"Name\":\"fleett-bob\",\"CPUPerc\":\"0.00%\",\"MemUsage\":\"0B / 0B\"}
";
        let stats = parse_stats(stdout);
        assert_eq!(stats.len(), 2);
        let alice = stats.get("fleett-alice").expect("alice");
        assert_eq!(alice.cpu_pct, 1.5);
        assert_eq!(alice.mem_used_bytes, 134_742_016);
        assert_eq!(alice.mem_limit_bytes, 1_610_612_736);
        let bob = stats.get("fleett-bob").expect("bob");
        assert_eq!(bob.cpu_pct, 0.0);
        assert_eq!(bob.mem_used_bytes, 0);
        assert_eq!(bob.mem_limit_bytes, 0);
    }

    #[test]
    fn stats_accept_an_array_and_ignore_junk() {
        let stdout =
            r#"[{"Name":"/fleett-alice","CPUPerc":"2%","MemUsage":"1MiB / 2MiB"},{"Foo":1}]"#;
        let stats = parse_stats(stdout);
        assert_eq!(stats.len(), 1, "the unnamed entry is ignored");
        assert_eq!(stats["fleett-alice"].cpu_pct, 2.0);

        assert!(parse_stats("").is_empty());
        assert!(parse_stats("not json").is_empty());
        assert!(parse_stats("{\"Name\":\"x\",\"CPUPerc\":\"oops\"}")
            .get("x")
            .is_some_and(|sample| sample.cpu_pct == 0.0));
    }

    #[test]
    fn sizes_are_parsed_like_docker_prints_them() {
        for (text, expected) in [
            ("0B", 0),
            ("512B", 512),
            ("1KiB", 1024),
            ("1.5MiB", 1_572_864),
            ("2GiB", 2_147_483_648),
            ("12MB", 12_000_000),
            ("1.5GB", 1_500_000_000),
            ("3", 3),
        ] {
            assert_eq!(parse_size(text), Some(expected), "{text}");
        }
        for text in ["", "MiB", "-1MiB", "1XiB", "abc"] {
            assert_eq!(parse_size(text), None, "{text}");
        }
    }

    #[test]
    fn disk_usage_reads_the_data_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::write(data.join("MEMORY.md"), "hello").expect("file");

        let size = du_bytes(&data).expect("du");
        assert!(size >= 5, "{size}");

        // A missing directory is empty, not an error.
        assert_eq!(du_bytes(&dir.path().join("missing")).expect("du"), 0);
    }

    fn usage_spec(id: &str) -> InstanceSpec {
        InstanceSpec {
            id: id.to_string(),
            name: id.to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: crate::models::DEFAULT_API_KEY_ENV.to_string(),
            provider: None,
            channel: crate::models::ChannelKind::None,
            shares: Vec::new(),
            mem_limit_mb: 512,
            cpu_limit: 1.0,
            image: "hermes-agent:0.22.2-local".to_string(),
            previous_image: None,
            allow_lan: false,
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            disk_quota_mb: 0,
            disk_policy: crate::models::DiskPolicy::Warn,
            notes: String::new(),
            host: crate::hosts::LOCAL_HOST.to_string(),
            status: Status::Created,
            last_error: None,
            created_at: crate::models::now_iso(),
            updated_at: crate::models::now_iso(),
        }
    }

    #[tokio::test]
    async fn usage_maps_docker_stats_to_instances_without_docker() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        for (id, bytes) in [("alice", 5u64), ("bob", 0)] {
            std::fs::create_dir_all(cfg.instance_dir(id)).expect("instance dir");
            std::fs::write(
                cfg.compose_path(id),
                format!("name: fleett-{id}\nservices: {{}}\n"),
            )
            .expect("compose file");
            if bytes > 0 {
                std::fs::create_dir_all(cfg.data_dir(id)).expect("data dir");
                std::fs::write(cfg.data_dir(id).join("MEMORY.md"), "hello").expect("data file");
            }
        }

        // A stand-in for the Docker CLI: only `docker stats` is answered.
        let docker = dir.path().join("docker");
        std::fs::write(
            &docker,
            "#!/bin/sh\nprintf '%s\\n' '{\"Name\":\"fleett-alice\",\"CPUPerc\":\"2.50%\",\"MemUsage\":\"100MiB / 512MiB\"}'\n",
        )
        .expect("fake docker");
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let driver = ComposeDriver::new(&cfg).with_docker_binary(&docker);
        let specs = vec![usage_spec("alice"), usage_spec("bob")];
        let samples = driver.usage(&specs).await.expect("usage");

        assert_eq!(samples.len(), 2);
        let alice = &samples[0];
        assert!(alice.running);
        assert_eq!(alice.cpu_pct, 2.5);
        assert_eq!(alice.mem_used_bytes, 100 * 1024 * 1024);
        assert_eq!(alice.mem_limit_bytes, 512 * 1024 * 1024);
        assert!(alice.disk_bytes >= 5, "{}", alice.disk_bytes);

        let bob = &samples[1];
        assert!(!bob.running, "no stats line means stopped");
        assert_eq!(bob.cpu_pct, 0.0);
        assert_eq!(bob.mem_used_bytes, 0);
        assert_eq!(
            bob.mem_limit_bytes,
            512 * 1024 * 1024,
            "the configured limit is reported for a stopped instance"
        );
    }

    #[tokio::test]
    async fn usage_without_instances_never_runs_docker() {
        let dir = tempfile::tempdir().expect("temp dir");
        let driver = driver_in(dir.path()).with_docker_binary(dir.path().join("missing-docker"));
        let samples = driver.usage(&[]).await.expect("usage");
        assert!(samples.is_empty());
    }

    #[test]
    fn images_are_parsed_from_ndjson() {
        let stdout = "\
{\"Containers\":\"0\",\"CreatedAt\":\"2026-11-08 01:02:03 +0000 UTC\",\"ID\":\"sha256:aaa\",\"Repository\":\"hermes-agent\",\"Size\":\"1.2GB\",\"Tag\":\"0.23.0\"}
{\"ID\":\"sha256:bbb\",\"Repository\":\"hermes-agent\",\"Size\":\"1.1GB\",\"Tag\":\"0.22.2-local\"}
{\"ID\":\"sha256:ccc\",\"Repository\":\"<none>\",\"Tag\":\"<none>\"}
";
        let images = parse_images(stdout);
        assert_eq!(images.len(), 2, "dangling images are skipped");
        assert_eq!(images[0].tag, "hermes-agent:0.22.2-local");
        assert_eq!(images[1].tag, "hermes-agent:0.23.0");
        assert_eq!(images[1].id, "sha256:aaa");
        assert_eq!(images[1].size, "1.2GB");
        assert_eq!(images[1].created, "2026-11-08 01:02:03 +0000 UTC");

        assert!(parse_images("").is_empty());
        assert!(parse_images("not json").is_empty());
        let array = r#"[{"Repository":"agent","Tag":"1"}]"#;
        assert_eq!(parse_images(array)[0].tag, "agent:1");
    }

    fn fake_docker(dir: &std::path::Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("docker");
        std::fs::write(&path, script).expect("fake docker");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[tokio::test]
    async fn ensure_image_inspects_then_pulls_only_when_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let calls = dir.path().join("calls");
        // A local tag: `docker image inspect` succeeds and nothing is pulled.
        let docker = fake_docker(
            dir.path(),
            &format!(
                "#!/bin/sh\necho \"$@\" >> {calls}\ncase \"$1 $2\" in\n  \"image inspect\") exit 0;;\n  *) exit 1;;\nesac\n",
                calls = calls.display()
            ),
        );
        let driver = driver_in(dir.path()).with_docker_binary(&docker);
        driver
            .ensure_image(crate::hosts::LOCAL_HOST, "agent:1")
            .await
            .expect("local image");
        assert_eq!(
            std::fs::read_to_string(&calls).expect("calls"),
            "image inspect agent:1\n"
        );

        // A missing tag: the inspect fails and the pull runs.
        let docker = fake_docker(
            dir.path(),
            &format!(
                "#!/bin/sh\necho \"$@\" >> {calls}\ncase \"$1 $2\" in\n  \"image inspect\") exit 1;;\n  \"pull agent:2\") exit 0;;\nesac\nexit 3\n",
                calls = calls.display()
            ),
        );
        let driver = driver_in(dir.path()).with_docker_binary(&docker);
        driver
            .ensure_image(crate::hosts::LOCAL_HOST, "agent:2")
            .await
            .expect("pulled image");
        let recorded = std::fs::read_to_string(&calls).expect("calls");
        assert!(recorded.contains("image inspect agent:2"), "{recorded}");
        assert!(recorded.contains("pull agent:2"), "{recorded}");
    }

    #[tokio::test]
    async fn a_failed_pull_is_reported_verbatim() {
        let dir = tempfile::tempdir().expect("temp dir");
        let docker = fake_docker(
            dir.path(),
            "#!/bin/sh\nif [ \"$1\" = image ]; then exit 1; fi\necho 'manifest unknown' >&2\nexit 1\n",
        );
        let driver = driver_in(dir.path()).with_docker_binary(&docker);
        let err = driver
            .ensure_image(crate::hosts::LOCAL_HOST, "agent:9")
            .await
            .expect_err("pull fails");
        assert_eq!(err.operation, "image pull");
        assert!(err.detail.contains("manifest unknown"), "{err}");
    }

    #[tokio::test]
    async fn images_can_be_listed_without_docker() {
        let dir = tempfile::tempdir().expect("temp dir");
        let docker = fake_docker(
            dir.path(),
            "#!/bin/sh\necho '{\"ID\":\"sha256:aaa\",\"Repository\":\"agent\",\"Tag\":\"2\",\"Size\":\"9MB\",\"CreatedAt\":\"now\"}'\n",
        );
        let driver = driver_in(dir.path()).with_docker_binary(&docker);
        let images = driver
            .list_images(crate::hosts::LOCAL_HOST)
            .await
            .expect("images");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].tag, "agent:2");
        assert_eq!(images[0].size, "9MB");
    }
}
