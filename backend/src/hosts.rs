//! Host registry: where the Docker daemons of the fleet live (issue #40).
//!
//! A host is either the local daemon (`local`), a named Docker context, or a
//! remote daemon reached through `DOCKER_HOST=ssh://user@host`. The driver never
//! shares volumes between hosts: the compose file and the instance data
//! directory stay on the controller, and a remote host must expose the same
//! absolute paths for the bind mounts to resolve. See `docs/HOSTS.md`.

use crate::db::Db;
use crate::models::InstanceSpec;
use serde::{Deserialize, Serialize};

/// Id of the implicit local host (the daemon of the control plane machine).
pub const LOCAL_HOST: &str = "local";
/// Transport of the daemon running next to the control plane.
pub const TRANSPORT_LOCAL: &str = "local";
/// Transport through `DOCKER_HOST=ssh://…`.
pub const TRANSPORT_SSH: &str = "ssh";

/// Connectivity state of a host, as last observed.
pub const STATUS_UNKNOWN: &str = "unknown";
pub const STATUS_OK: &str = "ok";
pub const STATUS_UNREACHABLE: &str = "unreachable";

/// One registered Docker host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSpec {
    pub id: String,
    pub name: String,
    /// `local` or `ssh`.
    pub transport: String,
    /// `user@host` or `ssh://user@host` for a remote host, empty for local.
    pub address: String,
    /// Docker context name, used when no `DOCKER_HOST` is set.
    pub docker_context: Option<String>,
    /// Last observed connectivity: `unknown`, `ok` or `unreachable`.
    pub status: String,
    pub last_check_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl HostSpec {
    /// Whether this row describes the local daemon.
    pub fn is_local(&self) -> bool {
        self.id == LOCAL_HOST
    }

    /// `DOCKER_HOST` value, when the host is reached through SSH.
    pub fn docker_host(&self) -> Option<String> {
        (self.transport == TRANSPORT_SSH).then(|| normalize_ssh_address(&self.address))
    }
}

/// Best-effort capacity of one host, read with `docker info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCapacity {
    /// Logical CPUs reported by the daemon (`NCPU`), `0` when unknown.
    pub ncpu: u32,
    /// Total memory reported by the daemon (`MemTotal`), `0` when unknown.
    pub mem_total_bytes: u64,
    /// Docker server version, when the daemon reported one.
    pub docker_version: Option<String>,
}

/// Environment and CLI selectors of one host endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostEndpoint {
    /// `DOCKER_HOST`, for a host reached through SSH.
    pub docker_host: Option<String>,
    /// `DOCKER_CONTEXT`, for a host declared by context name.
    pub docker_context: Option<String>,
}

impl HostEndpoint {
    /// Apply the endpoint to a child process.
    pub fn apply(&self, command: &mut tokio::process::Command) {
        if let Some(host) = &self.docker_host {
            command.env("DOCKER_HOST", host);
        }
        if let Some(context) = &self.docker_context {
            command.env("DOCKER_CONTEXT", context);
        }
    }

    /// Apply the endpoint to a blocking child process.
    pub fn apply_std(&self, command: &mut std::process::Command) {
        if let Some(host) = &self.docker_host {
            command.env("DOCKER_HOST", host);
        }
        if let Some(context) = &self.docker_context {
            command.env("DOCKER_CONTEXT", context);
        }
    }

    /// Whether the endpoint is the local daemon (no selector at all).
    pub fn is_local(&self) -> bool {
        self.docker_host.is_none() && self.docker_context.is_none()
    }
}

/// Endpoint of one host row.
pub fn endpoint(host: &HostSpec) -> HostEndpoint {
    HostEndpoint {
        docker_host: host.docker_host(),
        docker_context: host
            .docker_context
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    }
}

/// Resolve the endpoint of an instance host id through the index database.
///
/// An unknown host is an error: silently falling back to the local daemon
/// would run the container on the wrong machine.
pub fn endpoint_for(db: Option<&Db>, host_id: &str) -> Result<HostEndpoint, String> {
    if host_id.trim().is_empty() || host_id == LOCAL_HOST {
        return Ok(HostEndpoint::default());
    }
    let Some(db) = db else {
        return Err(format!(
            "host \"{host_id}\" is not resolvable: the driver has no host registry"
        ));
    };
    let host = db
        .host(host_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("unknown host \"{host_id}\""))?;
    Ok(endpoint(&host))
}

/// Normalise a remote address to a `DOCKER_HOST` value.
///
/// `user@host` becomes `ssh://user@host`; an explicit supported scheme is kept.
pub fn normalize_ssh_address(address: &str) -> String {
    let address = address.trim();
    if address.starts_with("ssh://") {
        return address.to_string();
    }
    format!("ssh://{address}")
}

/// Validation error of a host definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    InvalidId(String),
    InvalidName(String),
    InvalidTransport(String),
    InvalidAddress(String),
    InvalidContext(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidId(detail)
            | Self::InvalidName(detail)
            | Self::InvalidTransport(detail)
            | Self::InvalidAddress(detail)
            | Self::InvalidContext(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for HostError {}

/// Validate a host id (`[a-z0-9][a-z0-9-]{0,31}`).
pub fn validate_host_id(id: &str) -> Result<(), HostError> {
    if id.is_empty() || id.len() > 32 {
        return Err(HostError::InvalidId(
            "host id must be between 1 and 32 characters".to_string(),
        ));
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap_or_default();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(HostError::InvalidId(
            "host id must start with a lowercase letter or a digit".to_string(),
        ));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(HostError::InvalidId(
            "host id may only contain lowercase letters, digits and '-'".to_string(),
        ));
    }
    Ok(())
}

/// Validate a display name (1..=64 printable characters).
pub fn validate_host_name(name: &str) -> Result<(), HostError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(HostError::InvalidName(
            "host name must be between 1 and 64 characters".to_string(),
        ));
    }
    if name.chars().any(|ch| ch.is_control()) {
        return Err(HostError::InvalidName(
            "host name must not contain control characters".to_string(),
        ));
    }
    Ok(())
}

/// Derive the default id of a host from its name.
pub fn slug(name: &str) -> String {
    let mut id = String::new();
    let mut last_dash = false;
    for ch in name.trim().chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            id.push(ch);
            last_dash = false;
        } else if !last_dash && !id.is_empty() {
            id.push('-');
            last_dash = true;
        }
    }
    while id.ends_with('-') {
        id.pop();
    }
    id.truncate(32);
    id
}

/// Validate a transport (`local` or `ssh`).
pub fn validate_transport(value: &str) -> Result<&'static str, HostError> {
    match value.trim().to_ascii_lowercase().as_str() {
        TRANSPORT_LOCAL | "" => Ok(TRANSPORT_LOCAL),
        TRANSPORT_SSH => Ok(TRANSPORT_SSH),
        other => Err(HostError::InvalidTransport(format!(
            "unsupported transport \"{other}\" (expected local or ssh)"
        ))),
    }
}

/// Validate a remote address.
///
/// Accepted: `user@host`, `host`, `ssh://user@host`, `ssh://user@host:port`.
/// Whitespace, shell metacharacters and other schemes are refused because the
/// value ends up in `DOCKER_HOST` and, for `fleett instances move`, in an
/// `ssh`/`scp` command line.
pub fn validate_address(address: &str, transport: &str) -> Result<String, HostError> {
    let address = address.trim();
    if transport == TRANSPORT_LOCAL {
        if !address.is_empty() {
            return Err(HostError::InvalidAddress(
                "a local host has no address; use transport \"ssh\" for a remote daemon"
                    .to_string(),
            ));
        }
        return Ok(String::new());
    }
    if address.is_empty() {
        return Err(HostError::InvalidAddress(
            "an ssh host requires an address (user@host or ssh://user@host)".to_string(),
        ));
    }
    let stripped = address.strip_prefix("ssh://").unwrap_or(address);
    if stripped.is_empty() {
        return Err(HostError::InvalidAddress(
            "the ssh address is empty".to_string(),
        ));
    }
    if let Some((scheme, _)) = address.split_once("://") {
        if scheme != "ssh" {
            return Err(HostError::InvalidAddress(format!(
                "unsupported address scheme \"{scheme}\" (only ssh:// is accepted)"
            )));
        }
    }
    if stripped.chars().any(|ch| {
        ch.is_whitespace()
            || matches!(
                ch,
                ';' | '|' | '&' | '$' | '`' | '\\' | '"' | '\'' | '<' | '>' | '(' | ')'
            )
    }) {
        return Err(HostError::InvalidAddress(
            "the address must not contain whitespace or shell metacharacters".to_string(),
        ));
    }
    if stripped.starts_with('-') || stripped.contains("..") {
        return Err(HostError::InvalidAddress(
            "the address is not a valid user@host".to_string(),
        ));
    }
    Ok(address.to_string())
}

/// Validate a Docker context name.
pub fn validate_context(value: Option<&str>) -> Result<Option<String>, HostError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > 128
        || value.starts_with('-')
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(HostError::InvalidContext(
            "the docker context name must not contain whitespace and must not start with '-'"
                .to_string(),
        ));
    }
    Ok(Some(value.to_string()))
}

/// Free memory of one host, given the instances already placed on it.
///
/// `used_mem_mb` and `used_cpu` are the declared limits, not live usage:
/// placement must be deterministic and must not depend on a running sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostScore {
    pub free_mem_bytes: u64,
    pub free_cpu: f64,
    pub instances: usize,
}

/// Score one host for automatic placement.
///
/// A host whose daemon reported no memory is treated as having an unknown
/// capacity: it scores zero free memory and is only chosen when nothing else
/// is reachable.
pub fn score_host(capacity: &HostCapacity, instances: &[&InstanceSpec]) -> HostScore {
    let used_mem: u64 = instances
        .iter()
        .map(|spec| u64::from(spec.mem_limit_mb) * 1024 * 1024)
        .sum();
    let used_cpu: f64 = instances.iter().map(|spec| spec.cpu_limit).sum();
    HostScore {
        free_mem_bytes: capacity.mem_total_bytes.saturating_sub(used_mem),
        free_cpu: (f64::from(capacity.ncpu) - used_cpu).max(0.0),
        instances: instances.len(),
    }
}

/// Compare two placement scores; `Greater` means `left` is the better host.
///
/// The order is: most free memory, then most free CPU, then fewest instances,
/// then the smallest id (the caller breaks ties).
pub fn compare_scores(left: HostScore, right: HostScore) -> std::cmp::Ordering {
    left.free_mem_bytes
        .cmp(&right.free_mem_bytes)
        .then_with(|| {
            left.free_cpu
                .partial_cmp(&right.free_cpu)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| right.instances.cmp(&left.instances))
}

/// `true` when the host row is the protected local entry.
pub fn is_protected(host: &HostSpec) -> bool {
    host.is_local()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_strict_slugs() {
        for ok in ["local", "pve3", "node-1", "a"] {
            assert!(validate_host_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Pve3", "-node", "no de", "n_o", "x".repeat(33).as_str()] {
            assert!(validate_host_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_slug_to_a_stable_id() {
        assert_eq!(slug("PVE 3"), "pve-3");
        assert_eq!(slug("  host.example  "), "host-example");
        assert_eq!(slug("A--B"), "a-b");
        assert_eq!(slug("!!!"), "");
    }

    #[test]
    fn addresses_are_normalised_for_docker_host() {
        assert_eq!(
            normalize_ssh_address("cle@10.1.20.5"),
            "ssh://cle@10.1.20.5"
        );
        assert_eq!(
            normalize_ssh_address("ssh://cle@10.1.20.5:22"),
            "ssh://cle@10.1.20.5:22"
        );
        let host = HostSpec {
            id: "pve3".to_string(),
            name: "pve3".to_string(),
            transport: TRANSPORT_SSH.to_string(),
            address: "cle@10.1.20.5".to_string(),
            docker_context: None,
            status: STATUS_UNKNOWN.to_string(),
            last_check_at: None,
            last_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert_eq!(host.docker_host().as_deref(), Some("ssh://cle@10.1.20.5"));
    }

    #[test]
    fn addresses_refuse_shell_injection() {
        for bad in [
            "cle@host;rm -rf /",
            "cle@host && id",
            "file:///tmp/socket",
            "-oProxyCommand=evil",
            "cle@ho st",
        ] {
            assert!(
                validate_address(bad, TRANSPORT_SSH).is_err(),
                "accepted {bad}"
            );
        }
        assert_eq!(
            validate_address(" ssh://cle@host ", TRANSPORT_SSH)
                .ok()
                .as_deref(),
            Some("ssh://cle@host")
        );
        assert_eq!(
            validate_address("", TRANSPORT_LOCAL).ok().as_deref(),
            Some("")
        );
        assert!(validate_address("host", TRANSPORT_LOCAL).is_err());
        assert!(validate_address("", TRANSPORT_SSH).is_err());
    }

    #[test]
    fn capacity_scores_prefer_free_memory() {
        let capacity = HostCapacity {
            ncpu: 4,
            mem_total_bytes: 8 * 1024 * 1024 * 1024,
            docker_version: None,
        };
        let mut small = spec("small");
        small.mem_limit_mb = 1024;
        small.cpu_limit = 1.0;
        let mut large = spec("large");
        large.mem_limit_mb = 6144;
        large.cpu_limit = 3.0;

        let empty = score_host(&capacity, &[]);
        let one = score_host(&capacity, &[&small]);
        let heavy = score_host(&capacity, &[&small, &large]);
        assert!(empty.free_mem_bytes > one.free_mem_bytes);
        assert!(one.free_mem_bytes > heavy.free_mem_bytes);
        assert!(compare_scores(empty, one).is_gt());
        assert_eq!(heavy.instances, 2);
        assert_eq!(heavy.free_cpu, 0.0);
    }

    #[test]
    fn a_local_host_carries_no_selector() {
        let host = HostSpec {
            id: LOCAL_HOST.to_string(),
            name: "local".to_string(),
            transport: TRANSPORT_LOCAL.to_string(),
            address: String::new(),
            docker_context: None,
            status: STATUS_UNKNOWN.to_string(),
            last_check_at: None,
            last_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(endpoint(&host).is_local());
        assert!(is_protected(&host));
    }

    fn spec(id: &str) -> InstanceSpec {
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
            skills: Vec::new(),
            mem_limit_mb: 512,
            cpu_limit: 1.0,
            image: "agent:1".to_string(),
            previous_image: None,
            allow_lan: false,
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            disk_quota_mb: 0,
            disk_policy: crate::models::DiskPolicy::Warn,
            notes: String::new(),
            status: crate::models::Status::Created,
            last_error: None,
            host: LOCAL_HOST.to_string(),
            created_at: crate::models::now_iso(),
            updated_at: crate::models::now_iso(),
        }
    }
}
