//! Container drivers.
//!
//! `FLEETT_DRIVER=compose` talks to Docker through `docker compose -f …`; the
//! mock driver keeps every instance in memory and never touches Docker, which
//! is what the test suite uses.

pub mod compose;
pub mod mock;

use crate::models::{ContainerInfo, Health, InstanceSpec, Status};
use std::future::Future;
use std::pin::Pin;

/// Result of `docker compose ps` for one instance.
#[derive(Debug, Clone)]
pub struct PsResult {
    pub status: Status,
    pub health: Health,
    pub container: Option<ContainerInfo>,
}

/// Options of one `logs` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsOptions {
    /// Number of trailing lines. The API bounds it to `1..=2000`.
    pub tail: u32,
    /// Docker `--since`: a duration (`30s`, `10m`, `2h`, `1h30m`) or an
    /// RFC 3339 timestamp.
    pub since: Option<String>,
}

impl Default for LogsOptions {
    fn default() -> Self {
        Self {
            tail: 200,
            since: None,
        }
    }
}

/// Runtime usage of one instance, read by the driver.
///
/// The compose driver fills it from one `docker stats` call plus `du -sb` on
/// the instance data directory; the mock driver simulates stable numbers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageSample {
    /// The container was seen running when the sample was taken.
    pub running: bool,
    /// CPU usage in percent of one core (`docker stats` `CPUPerc`).
    pub cpu_pct: f64,
    pub mem_used_bytes: u64,
    /// Container limit, or the configured limit when the container is absent.
    pub mem_limit_bytes: u64,
    /// Size of the instance data directory (`du -sb`).
    pub disk_bytes: u64,
}

/// Runtime state of one instance container, read with `docker inspect`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InspectInfo {
    /// `State.Status`: `running`, `exited`, `dead`, `created`, …
    pub state: String,
    pub exit_code: Option<i32>,
    pub oom_killed: bool,
    /// `State.Error`, when Docker recorded one.
    pub error: Option<String>,
    /// `State.StartedAt` as RFC 3339, when the container has ever started.
    pub started_at: Option<String>,
}

impl InspectInfo {
    /// Operator-facing failure message, when Docker reports one.
    ///
    /// A clean stop (exit code `0`, no `Error`, not OOM-killed) yields `None`,
    /// so a deliberately stopped instance never looks broken.
    pub fn error_message(&self) -> Option<String> {
        let mut reasons = Vec::new();
        if let Some(error) = self
            .error
            .as_deref()
            .map(str::trim)
            .filter(|error| !error.is_empty())
        {
            reasons.push(error.to_string());
        }
        if self.oom_killed {
            reasons.push("the container was killed by the OOM killer".to_string());
        }
        if let Some(code) = self.exit_code.filter(|code| *code != 0) {
            reasons.push(format!("the container exited with code {code}"));
        }
        if reasons.is_empty() {
            return None;
        }
        let state = self.state.trim();
        let detail = reasons.join("; ");
        Some(if state.is_empty() {
            detail
        } else {
            format!("container state \"{state}\": {detail}")
        })
    }
}

/// A failed driver call, carrying the compose stderr.
#[derive(Debug, Clone, thiserror::Error)]
#[error("docker compose {operation} failed: {detail}")]
pub struct DriverError {
    pub operation: String,
    pub detail: String,
}

impl DriverError {
    pub fn new(operation: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            detail: detail.into(),
        }
    }
}

/// One image present in the local Docker image store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageRecord {
    /// `repository:tag`, exactly as an instance would reference it.
    pub tag: String,
    /// Short image id (`sha256:…`), when Docker reports one.
    pub id: String,
    /// Human size (`1.2GB`), verbatim from Docker.
    pub size: String,
    /// Creation date, verbatim from Docker.
    pub created: String,
}

pub type DriverFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Lifecycle operations the control plane needs.
pub trait Driver: Send + Sync {
    fn name(&self) -> &'static str;

    /// Cheap liveness probe for `GET /api/health`.
    fn ping(&self) -> DriverFuture<'_, bool>;

    /// Make sure a Docker network exists. `true` when it had to be created.
    fn ensure_network<'a>(&'a self, name: &'a str) -> DriverFuture<'a, Result<bool, DriverError>>;

    /// IPv4 subnet of a Docker network, when it can be read.
    ///
    /// `Ok(None)` means "unknown" — the egress policy then trusts the marker as
    /// it is instead of comparing subnets.
    fn network_subnet<'a>(
        &'a self,
        name: &'a str,
    ) -> DriverFuture<'a, Result<Option<String>, DriverError>>;

    fn up<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>>;
    fn stop<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>>;
    fn start<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>>;
    fn restart<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>>;
    fn down<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<(), DriverError>>;
    fn ps<'a>(&'a self, spec: &'a InstanceSpec) -> DriverFuture<'a, Result<PsResult, DriverError>>;

    /// Tail of the container logs, filtered by the driver-level options only
    /// (`tail`, `since`); `grep`/`level` filtering happens in the API.
    fn logs<'a>(
        &'a self,
        spec: &'a InstanceSpec,
        options: &'a LogsOptions,
    ) -> DriverFuture<'a, Result<String, DriverError>>;

    /// `docker inspect` state of the instance container.
    fn inspect<'a>(
        &'a self,
        spec: &'a InstanceSpec,
    ) -> DriverFuture<'a, Result<InspectInfo, DriverError>>;

    /// Runtime usage of the given instances, in the same order.
    ///
    /// One call covers the whole fleet: the compose driver runs a single
    /// `docker stats` for every running container and measures each data
    /// directory. Empty input never touches Docker.
    fn usage<'a>(
        &'a self,
        specs: &'a [InstanceSpec],
    ) -> DriverFuture<'a, Result<Vec<UsageSample>, DriverError>>;

    /// Make sure `image` is present locally, pulling it when it is not.
    ///
    /// `docker image inspect` first so a local tag is never re-pulled; the
    /// pull error is returned verbatim so the API can answer `502` with the
    /// Docker detail.
    fn ensure_image<'a>(&'a self, image: &'a str) -> DriverFuture<'a, Result<(), DriverError>>;

    /// Images present in the local Docker image store.
    fn list_images(&self) -> DriverFuture<'_, Result<Vec<ImageRecord>, DriverError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(state: &str, exit_code: Option<i32>) -> InspectInfo {
        InspectInfo {
            state: state.to_string(),
            exit_code,
            ..InspectInfo::default()
        }
    }

    #[test]
    fn a_running_container_has_no_error() {
        assert_eq!(info("running", Some(0)).error_message(), None);
        assert_eq!(info("created", Some(0)).error_message(), None);
        assert_eq!(info("exited", Some(0)).error_message(), None);
    }

    #[test]
    fn a_clean_container_without_state_is_fine() {
        assert_eq!(InspectInfo::default().error_message(), None);
    }

    #[test]
    fn a_non_zero_exit_code_is_reported() {
        let message = info("exited", Some(137)).error_message().expect("error");
        assert!(message.contains("137"), "{message}");
        assert!(message.contains("exited"), "{message}");
    }

    #[test]
    fn an_oom_kill_is_reported() {
        let info = InspectInfo {
            state: "exited".to_string(),
            exit_code: Some(137),
            oom_killed: true,
            ..InspectInfo::default()
        };
        let message = info.error_message().expect("error");
        assert!(message.contains("OOM"), "{message}");
        assert!(message.contains("137"), "{message}");
    }

    #[test]
    fn a_docker_error_is_reported_verbatim() {
        let info = InspectInfo {
            state: "dead".to_string(),
            error: Some("oci runtime error: exec failed".to_string()),
            ..InspectInfo::default()
        };
        let message = info.error_message().expect("error");
        assert!(message.contains("oci runtime error"), "{message}");
    }

    #[test]
    fn a_blank_docker_error_is_ignored() {
        let info = InspectInfo {
            state: "exited".to_string(),
            error: Some("   ".to_string()),
            exit_code: Some(0),
            ..InspectInfo::default()
        };
        assert_eq!(info.error_message(), None);
    }

    #[test]
    fn logs_options_default_to_a_bounded_tail() {
        let options = LogsOptions::default();
        assert_eq!(options.tail, 200);
        assert!(options.since.is_none());
    }
}
