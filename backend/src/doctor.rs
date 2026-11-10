//! Readiness checklist behind `GET /api/doctor` (issue #52).
//!
//! The console renders the same checklist as `fleett doctor`; this module owns
//! the checks so they stay testable without HTTP. Every check is self-contained:
//! a failing dependency (a database error, an unreachable engine) becomes one
//! `error` line carrying its detail, never a 500.
//!
//! A check is `ok`, `warn` (it works, but the fleet is degraded or a protection
//! is off) or `error` (the operator must fix it). The report is `ok` only when
//! no check is `error`: warnings never fail it.
//!
//! Everything here is local except the `egress-policy` check: when a policy
//! renders a proxy, that check makes at most one bounded TCP probe per distinct
//! proxy URL (750 ms each, through the shared probe cache). The DNS check only
//! reads `/etc/resolv.conf`, so the rest of the checklist still answers on an
//! isolated host.

use crate::assistant::{self, ProviderResolution};
use crate::config::{Config, DriverKind, EgressMode, VERSION};
use crate::db::Db;
use crate::driver::Driver;
use crate::egress::{self, MarkerState};
use crate::egress_policy::{self, ProxyProbeCache};
use crate::hosts::LOCAL_HOST;
use crate::secrets;
use serde::Serialize;
use std::path::Path;
use std::process::Command;

/// Ids of every check, in the order the report lists them.
pub const CHECK_IDS: [&str; 12] = [
    "api",
    "docker",
    "image",
    "secrets",
    "egress",
    "egress-policy",
    "providers",
    "assistant",
    "instances",
    "permissions",
    "disk",
    "dns",
];

/// Free-space percentage below which the disk check warns.
pub const DISK_WARN_PERCENT: f64 = 10.0;
/// Free-space percentage below which the disk check fails.
pub const DISK_ERROR_PERCENT: f64 = 5.0;
/// Socket the Docker CLI uses when `DOCKER_HOST` is unset.
pub const DEFAULT_DOCKER_SOCKET: &str = "/var/run/docker.sock";
/// File the DNS check reads. Only nameservers are inspected; no lookup is made.
pub const RESOLV_CONF: &str = "/etc/resolv.conf";
/// Repair offered when the agent image is absent from the local store.
pub const IMAGE_BUILD_HINT: &str = "/path/to/hermes-official";
/// Repair offered when the egress firewall is missing or stale.
pub const EGRESS_INSTALL_HINT: &str = "sudo fleett egress install";
/// Repair offered when no encryption key is configured.
pub const SECRETS_INIT_HINT: &str = "sudo fleett secrets init";
/// Repair offered when plaintext `secrets.env` files remain.
pub const SECRETS_MIGRATE_HINT: &str = "fleett secrets migrate --apply";
/// Console page where the assistant provider is selected.
pub const ASSISTANT_SELECTION_LINK: &str = "/providers?assistant=1";
/// Console form that creates the first provider.
pub const PROVIDER_CREATE_LINK: &str = "/providers?new=1";

/// Severity of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Error,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// One line of the checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// Stable id (`api`, `docker`, `image`, …).
    pub id: &'static str,
    /// Short title ready to display.
    pub label: &'static str,
    pub status: CheckStatus,
    /// Human-readable explanation of the observed state.
    pub detail: String,
    /// Exact command (or setting) that fixes an `error`/`warn`, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Internal console route that repairs the check, when one exists
    /// (`/providers?assistant=1`). `fix` stays the command form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Machine-readable cause of the line, for the console to branch on
    /// (`no_provider`, `not_selected`); absent when the detail says it all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Check {
    fn new(
        id: &'static str,
        label: &'static str,
        status: CheckStatus,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id,
            label,
            status,
            detail: detail.into(),
            fix: None,
            link: None,
            reason: None,
        }
    }

    fn ok(id: &'static str, label: &'static str, detail: impl Into<String>) -> Self {
        Self::new(id, label, CheckStatus::Ok, detail)
    }

    fn warn(id: &'static str, label: &'static str, detail: impl Into<String>) -> Self {
        Self::new(id, label, CheckStatus::Warn, detail)
    }

    fn error(id: &'static str, label: &'static str, detail: impl Into<String>) -> Self {
        Self::new(id, label, CheckStatus::Error, detail)
    }

    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }

    fn with_link(mut self, link: impl Into<String>) -> Self {
        self.link = Some(link.into());
        self
    }

    fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Body of `GET /api/doctor`.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// `true` when no check is [`CheckStatus::Error`].
    pub ok: bool,
    pub checks: Vec<Check>,
}

/// Run every check. Never fails: an error inside one check is reported as that
/// check's `error` line.
pub async fn report(
    cfg: &Config,
    db: &Db,
    driver: &dyn Driver,
    uptime_s: i64,
    probe: &ProxyProbeCache,
) -> Report {
    // `DOCKER_HOST` only feeds the permissions check; read it once so every
    // call of the report sees the same endpoint.
    let docker_host = std::env::var("DOCKER_HOST").ok();
    let marker = egress::read_marker(&cfg.egress_marker);
    let checks = vec![
        check_api(uptime_s),
        guarded("docker", "Docker engine", check_docker(cfg.driver, driver)).await,
        guarded("image", "Agent image", check_image(cfg, driver)).await,
        guarded_now("secrets", "Secrets", Ok(check_secrets(cfg, db))),
        check_egress(cfg, &marker),
        guarded(
            "egress-policy",
            "Egress policy",
            check_egress_policy(cfg, db, probe),
        )
        .await,
        guarded_now("providers", "Providers", check_providers(db)),
        guarded_now("assistant", "Assistant", check_assistant(cfg, db)),
        guarded_now("instances", "Instances", check_instances(db)),
        check_permissions(cfg, docker_host.as_deref()),
        check_disk(&cfg.root),
        check_dns(),
    ];
    let ok = report_ok(&checks);
    Report { ok, checks }
}

/// The report is `ok` only when no check is an error; warnings never fail it.
pub fn report_ok(checks: &[Check]) -> bool {
    checks
        .iter()
        .all(|check| check.status != CheckStatus::Error)
}

/// Run one fallible check; its failure becomes the `error` line of that check.
async fn guarded<F>(id: &'static str, label: &'static str, check: F) -> Check
where
    F: std::future::Future<Output = Result<Check, String>>,
{
    match check.await {
        Ok(check) => check,
        Err(detail) => Check::error(id, label, detail),
    }
}

/// Synchronous [`guarded`].
fn guarded_now(id: &'static str, label: &'static str, check: Result<Check, String>) -> Check {
    check.unwrap_or_else(|detail| Check::error(id, label, detail))
}

fn check_api(uptime_s: i64) -> Check {
    Check::ok(
        "api",
        "API",
        format!("fleett {VERSION}, up {}", format_uptime(uptime_s)),
    )
}

async fn check_docker(driver_kind: DriverKind, driver: &dyn Driver) -> Result<Check, String> {
    if driver_kind == DriverKind::Mock {
        return Ok(Check::ok(
            "docker",
            "Docker engine",
            "simulated: FLEETT_DRIVER=mock never contacts Docker",
        ));
    }
    if driver.ping().await {
        Ok(Check::ok(
            "docker",
            "Docker engine",
            format!("the Docker engine answers (driver {})", driver.name()),
        ))
    } else {
        Ok(Check::error(
            "docker",
            "Docker engine",
            "the Docker engine does not answer; containers cannot be managed",
        )
        .with_fix("sudo systemctl start docker && docker info"))
    }
}

async fn check_image(cfg: &Config, driver: &dyn Driver) -> Result<Check, String> {
    if image_present(cfg, driver).await? {
        return Ok(Check::ok(
            "image",
            "Agent image",
            format!("image \"{}\" is available locally", cfg.image),
        ));
    }
    Ok(Check::error(
        "image",
        "Agent image",
        format!(
            "image \"{}\" (FLEETT_IMAGE) is not in the local Docker image store",
            cfg.image
        ),
    )
    .with_fix(build_hint(&cfg.image)))
}

/// Whether the configured agent image is present in the local image store.
///
/// The driver error (an unreachable daemon, a broken CLI) is returned verbatim
/// so the caller can report it instead of guessing.
pub async fn image_present(cfg: &Config, driver: &dyn Driver) -> Result<bool, String> {
    let images = driver
        .list_images(LOCAL_HOST)
        .await
        .map_err(|err| err.to_string())?;
    Ok(images.iter().any(|image| image.tag == cfg.image))
}

/// Command that builds the agent image locally.
pub fn build_hint(image: &str) -> String {
    format!("docker build -t {image} {IMAGE_BUILD_HINT}")
}

fn check_secrets(cfg: &Config, db: &Db) -> Check {
    let status = secrets::status(cfg, db);
    let plaintext = status.legacy_files.len();
    let Some(source) = status.source else {
        let detail = status.key_error.unwrap_or_else(|| {
            "no encryption key configured; instance secrets cannot be stored encrypted".to_string()
        });
        return Check::error("secrets", "Secrets", detail).with_fix(SECRETS_INIT_HINT);
    };
    let detail = if plaintext == 0 {
        format!(
            "{source}; {} encrypted",
            count(status.encrypted_instances, "instance")
        )
    } else {
        format!(
            "{source}; {} encrypted, {} still plaintext",
            count(status.encrypted_instances, "instance"),
            count(plaintext, "file")
        )
    };
    if plaintext > 0 {
        Check::warn("secrets", "Secrets", detail).with_fix(SECRETS_MIGRATE_HINT)
    } else {
        Check::ok("secrets", "Secrets", detail)
    }
}

fn check_egress(cfg: &Config, marker: &MarkerState) -> Check {
    let marker_path = cfg.egress_marker.display();
    match cfg.egress {
        EgressMode::Off => Check::warn(
            "egress",
            "Egress firewall",
            "FLEETT_EGRESS=off: agents are not isolated from the LAN",
        )
        .with_fix("set FLEETT_EGRESS=restrict, then run `sudo fleett egress install`"),
        EgressMode::Restrict => match marker {
            MarkerState::Missing => Check::warn(
                "egress",
                "Egress firewall",
                format!("restrict, but the firewall is not installed (no marker at {marker_path})"),
            )
            .with_fix(EGRESS_INSTALL_HINT),
            MarkerState::Invalid(detail) => Check::warn(
                "egress",
                "Egress firewall",
                format!("restrict, but the marker {marker_path} is unreadable: {detail}"),
            )
            .with_fix(EGRESS_INSTALL_HINT),
            MarkerState::Present(marker) if marker.version != VERSION => Check::warn(
                "egress",
                "Egress firewall",
                format!(
                    "restrict, but the marker was installed by fleett {} (running {VERSION}); \
                     the rules may be stale",
                    marker.version
                ),
            )
            .with_fix(EGRESS_INSTALL_HINT),
            MarkerState::Present(marker) => Check::ok(
                "egress",
                "Egress firewall",
                format!(
                    "restrict, marker present (subnet {}, installed {})",
                    marker.subnet, marker.installed_at
                ),
            ),
        },
    }
}

/// Destination policies under `FLEETT_EGRESS`: distinguishes "no policy" from
/// "policy whose proxy is down".
///
/// An explicit policy that needs no proxy is fine (the host firewall covers
/// it); one that renders a proxy is `error` when that proxy does not answer,
/// because every outbound connection of the instance then fails. At most one
/// probe runs per distinct proxy URL, through the shared cache.
async fn check_egress_policy(
    cfg: &Config,
    db: &Db,
    probe: &ProxyProbeCache,
) -> Result<Check, String> {
    let policies = egress_policy::list_all(db).map_err(|err| err.to_string())?;
    if policies.is_empty() {
        return Ok(Check::ok(
            "egress-policy",
            "Egress policy",
            "no explicit destination policy on any instance; only the host LAN filter applies \
             (legacy source)",
        ));
    }

    let mut proxied: Vec<(String, String)> = Vec::new();
    let mut unconfigured: Vec<String> = Vec::new();
    for policy in &policies {
        if !egress_policy::needs_proxy(cfg, policy) {
            continue;
        }
        match egress_policy::proxy_url(cfg, policy) {
            Some(proxy) => proxied.push((policy.instance_id.clone(), proxy)),
            None => unconfigured.push(policy.instance_id.clone()),
        }
    }

    if proxied.is_empty() && unconfigured.is_empty() {
        return Ok(Check::ok(
            "egress-policy",
            "Egress policy",
            format!(
                "{} stored; none needs the proxy, the host firewall covers {}",
                count(policies.len(), "policy"),
                if policies.len() == 1 { "it" } else { "them" }
            ),
        ));
    }

    let mut failures = Vec::new();
    let mut probed: Vec<&str> = Vec::new();
    for (_, proxy) in &proxied {
        if probed.contains(&proxy.as_str()) {
            continue;
        }
        probed.push(proxy);
        if let Err(detail) = probe.reachable(proxy).await {
            let instances: Vec<&str> = proxied
                .iter()
                .filter(|(_, url)| url == proxy)
                .map(|(instance, _)| instance.as_str())
                .collect();
            failures.push(format!("{} → {proxy} ({detail})", instances.join(", ")));
        }
    }

    if !failures.is_empty() {
        return Ok(Check::error(
            "egress-policy",
            "Egress policy",
            format!(
                "explicit policy renders an unreachable proxy: {}; every outbound connection of \
                 the instance(s) fails until it answers",
                failures.join("; ")
            ),
        )
        .with_fix(egress_policy::PROXY_DEPLOY_HINT));
    }

    if !unconfigured.is_empty() {
        return Ok(Check::warn(
            "egress-policy",
            "Egress policy",
            format!(
                "{} needs a proxy but no address is configured (FLEETT_EGRESS_PROXY is off and \
                 the policy has no proxy override), so it is stored but unenforced",
                unconfigured.join(", ")
            ),
        )
        .with_fix("set FLEETT_EGRESS_PROXY, add a proxy override, or remove the policy"));
    }

    Ok(Check::ok(
        "egress-policy",
        "Egress policy",
        format!(
            "{} stored; rendered proxy reachable ({})",
            count(policies.len(), "policy"),
            probed.join(", ")
        ),
    ))
}

fn check_providers(db: &Db) -> Result<Check, String> {
    let providers = db.list_providers().map_err(|err| err.to_string())?;
    if providers.is_empty() {
        return Ok(Check::warn(
            "providers",
            "Providers",
            "no provider configured; instances cannot reuse a credential",
        ));
    }
    Ok(Check::ok(
        "providers",
        "Providers",
        format!("{} configured", count(providers.len(), "provider")),
    ))
}

/// Assistant provider resolution (issue #63).
///
/// The assistant borrows the secret of a provider; the panel is unusable
/// without one. Three outcomes the console must tell apart:
///
/// * resolved from `env`, the stored selection or `auto` → `ok` (a disabled
///   assistant is a deliberate choice, also `ok`);
/// * no provider at all, or several and none selected → `warn` with `reason`
///   and a `link` to the page that fixes it;
/// * a reference to a deleted provider → `error`, never a silent `null`.
fn check_assistant(cfg: &Config, db: &Db) -> Result<Check, String> {
    if !cfg.assistant_enabled {
        return Ok(Check::ok(
            "assistant",
            "Assistant",
            "FLEETT_ASSISTANT=off: the panel and its routes are disabled",
        ));
    }
    match assistant::resolve(cfg, db).map_err(|err| err.to_string())? {
        ProviderResolution::Provider { spec, source } => Ok(Check::ok(
            "assistant",
            "Assistant",
            format!(
                "provider \"{}\" selected ({}), model {}",
                spec.name,
                source.as_str(),
                cfg.assistant_model
            ),
        )),
        ProviderResolution::Missing { reason } => Ok(Check::warn(
            "assistant",
            "Assistant",
            match reason {
                assistant::Reason::NoProvider => {
                    "no provider configured; the assistant has nothing to borrow".to_string()
                }
                assistant::Reason::NotSelected => {
                    "providers exist but none is selected for the assistant".to_string()
                }
            },
        )
        .with_reason(reason.as_str())
        .with_link(if reason == assistant::Reason::NotSelected {
            ASSISTANT_SELECTION_LINK
        } else {
            PROVIDER_CREATE_LINK
        })),
        ProviderResolution::Broken { id, source, valid } => {
            let ids = if valid.is_empty() {
                "no provider exists".to_string()
            } else {
                format!("valid ids: {}", valid.join(", "))
            };
            let mut check = Check::error(
                "assistant",
                "Assistant",
                format!(
                    "provider \"{id}\" ({}) does not exist; {ids}",
                    source.as_str()
                ),
            );
            check = if source == assistant::ProviderSource::Env {
                check.with_fix(
                    "set FLEETT_ASSISTANT_PROVIDER to an existing provider id, or unset it",
                )
            } else {
                check.with_link(ASSISTANT_SELECTION_LINK)
            };
            Ok(check)
        }
    }
}

fn check_instances(db: &Db) -> Result<Check, String> {
    let instances = db.list().map_err(|err| err.to_string())?;
    if instances.is_empty() {
        return Ok(Check::ok(
            "instances",
            "Instances",
            "no instance yet; create one from a template",
        ));
    }
    Ok(Check::ok(
        "instances",
        "Instances",
        format!("{} configured", count(instances.len(), "instance")),
    ))
}

fn check_permissions(cfg: &Config, docker_host: Option<&str>) -> Check {
    if let Err(detail) = probe_writable(&cfg.root) {
        return Check::error("permissions", "Permissions", detail).with_fix(format!(
            "sudo chown -R fleett:fleett {}",
            cfg.root.display()
        ));
    }
    let writable = format!("FLEETT_ROOT {} is writable", cfg.root.display());
    if cfg.driver != DriverKind::Compose {
        return Check::ok(
            "permissions",
            "Permissions",
            format!("{writable}; the mock driver has no Docker socket"),
        );
    }
    match docker_host.map(str::trim).filter(|value| !value.is_empty()) {
        Some(endpoint)
            if endpoint.starts_with("tcp://")
                || endpoint.starts_with("http://")
                || endpoint.starts_with("ssh://") =>
        {
            Check::ok(
                "permissions",
                "Permissions",
                format!("{writable}; remote engine at {endpoint}"),
            )
        }
        Some(endpoint) => {
            let path = endpoint.strip_prefix("unix://").unwrap_or(endpoint);
            socket_probe(Path::new(path), &writable)
        }
        None => socket_probe(Path::new(DEFAULT_DOCKER_SOCKET), &writable),
    }
}

fn socket_probe(path: &Path, writable: &str) -> Check {
    if !path.exists() {
        return Check::error(
            "permissions",
            "Permissions",
            format!(
                "{writable}; the Docker socket {} does not exist",
                path.display()
            ),
        )
        .with_fix("mount /var/run/docker.sock into the control plane, or set DOCKER_HOST");
    }
    match unix_socket_reachable(path) {
        Ok(()) => Check::ok(
            "permissions",
            "Permissions",
            format!(
                "{writable}; the Docker socket {} is reachable",
                path.display()
            ),
        ),
        Err(detail) => Check::error(
            "permissions",
            "Permissions",
            format!(
                "{writable}; the Docker socket {} is not reachable: {detail}",
                path.display()
            ),
        )
        .with_fix("sudo usermod -aG docker fleett && sudo systemctl restart fleett"),
    }
}

/// Create and delete a probe file: a read-only data root breaks every write.
fn probe_writable(root: &Path) -> Result<(), String> {
    if !root.is_dir() {
        return Err(format!("FLEETT_ROOT {} is not a directory", root.display()));
    }
    let probe = root.join(".doctor-probe");
    std::fs::write(&probe, b"fleett doctor\n")
        .map_err(|err| format!("FLEETT_ROOT {} is not writable: {err}", root.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

#[cfg(unix)]
fn unix_socket_reachable(path: &Path) -> Result<(), String> {
    std::os::unix::net::UnixStream::connect(path)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(not(unix))]
fn unix_socket_reachable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Free and total bytes of the filesystem holding one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpace {
    pub free_bytes: u64,
    pub total_bytes: u64,
}

impl DiskSpace {
    pub fn free_percent(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.free_bytes as f64 * 100.0 / self.total_bytes as f64
        }
    }
}

/// Level of the disk check for one sample.
pub fn disk_level(space: &DiskSpace) -> CheckStatus {
    let free = space.free_percent();
    if free < DISK_ERROR_PERCENT {
        CheckStatus::Error
    } else if free < DISK_WARN_PERCENT {
        CheckStatus::Warn
    } else {
        CheckStatus::Ok
    }
}

fn check_disk(root: &Path) -> Check {
    let Some(space) = disk_space(root) else {
        return Check::error(
            "disk",
            "Disk space",
            format!("cannot measure the free space on {}", root.display()),
        )
        .with_fix("check that `df` is available and FLEETT_ROOT exists");
    };
    let check = Check::new(
        "disk",
        "Disk space",
        disk_level(&space),
        format!(
            "{} free of {} ({:.0}%) on {}",
            format_bytes(space.free_bytes),
            format_bytes(space.total_bytes),
            space.free_percent(),
            root.display()
        ),
    );
    if check.status == CheckStatus::Error {
        check.with_fix("free space or move FLEETT_ROOT to a larger filesystem")
    } else {
        check
    }
}

/// Free and total bytes on the filesystem holding `path`, or `None` when `df`
/// fails.
///
/// `df -B1 --output=avail,size` is preferred (GNU coreutils); the classic
/// `df -B1` parse is the fallback for busybox hosts, exactly like the quota
/// guard. No new dependency is pulled in for two numbers.
pub fn disk_space(path: &Path) -> Option<DiskSpace> {
    let gnu = Command::new("df")
        .args(["-B1", "--output=avail,size"])
        .arg(path)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            let line = text.lines().nth(1)?;
            let mut fields = line.split_whitespace();
            Some(DiskSpace {
                free_bytes: fields.next()?.parse().ok()?,
                total_bytes: fields.next()?.parse().ok()?,
            })
        });
    if gnu.is_some() {
        return gnu;
    }

    let output = Command::new("df").arg("-B1").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    // Filesystem 1B-blocks Used Available Use% Mounted-on
    let line = text.lines().nth(1)?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    Some(DiskSpace {
        total_bytes: fields.get(1)?.parse().ok()?,
        free_bytes: fields.get(3)?.parse().ok()?,
    })
}

fn check_dns() -> Check {
    match std::fs::read_to_string(RESOLV_CONF) {
        Ok(text) => dns_check(&text),
        Err(err) => Check::error("dns", "DNS", format!("cannot read {RESOLV_CONF}: {err}"))
            .with_fix("write `nameserver 9.9.9.9` into /etc/resolv.conf"),
    }
}

/// Status of the resolver configuration. No lookup is made: on an isolated host
/// a network call would turn a warning into a timeout.
pub fn dns_check(text: &str) -> Check {
    let servers = nameservers(text);
    let Some(first) = servers.first() else {
        return Check::error(
            "dns",
            "DNS",
            format!("no nameserver in {RESOLV_CONF}; names outside /etc/hosts cannot resolve"),
        )
        .with_fix("write `nameserver 9.9.9.9` into /etc/resolv.conf");
    };
    Check::ok(
        "dns",
        "DNS",
        format!(
            "{} configured ({first}), no lookup was made",
            count(servers.len(), "nameserver")
        ),
    )
}

/// Nameserver addresses of a `resolv.conf`, comments included.
pub fn nameservers(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some("nameserver"), Some(address)) => Some(address.to_string()),
                _ => None,
            }
        })
        .collect()
}

/// `1 instance` / `3 instances`.
pub fn count(total: usize, singular: &str) -> String {
    if total == 1 {
        format!("1 {singular}")
    } else {
        format!("{total} {singular}s")
    }
}

/// Compact byte size (`0 B`, `1.5 KiB`, `12.3 GiB`) for the disk detail.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Compact uptime (`42s`, `3m 4s`, `2h 5m`, `1d 2h`).
pub fn format_uptime(seconds: i64) -> String {
    let seconds = seconds.max(0) as u64;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{}m {}s", minutes, seconds % 60);
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{}h {}m", hours, minutes % 60);
    }
    format!("{}d {}h", hours / 24, hours % 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(free: u64, total: u64) -> DiskSpace {
        DiskSpace {
            free_bytes: free,
            total_bytes: total,
        }
    }

    #[test]
    fn warnings_never_fail_the_report() {
        let ok = Check::ok("api", "API", "up");
        let warn = Check::warn("egress", "Egress firewall", "off");
        assert!(report_ok(&[ok.clone(), warn.clone()]));
        assert!(!report_ok(&[
            ok,
            warn,
            Check::error("disk", "Disk space", "full")
        ]));
    }

    #[test]
    fn disk_thresholds_warn_below_ten_percent_and_fail_below_five() {
        assert_eq!(disk_level(&space(50, 100)), CheckStatus::Ok);
        assert_eq!(disk_level(&space(10, 100)), CheckStatus::Ok);
        assert_eq!(disk_level(&space(9, 100)), CheckStatus::Warn);
        assert_eq!(disk_level(&space(5, 100)), CheckStatus::Warn);
        assert_eq!(disk_level(&space(4, 100)), CheckStatus::Error);
        assert_eq!(disk_level(&space(0, 100)), CheckStatus::Error);
        assert_eq!(disk_level(&space(1, 0)), CheckStatus::Error);
    }

    #[test]
    fn disk_measures_a_temp_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sample = disk_space(dir.path()).expect("df reports the temp filesystem");
        assert!(sample.total_bytes > 0);
        assert!(sample.free_bytes <= sample.total_bytes);
        // The level depends on the host, the measurement never does.
        assert!((0.0..=100.0).contains(&sample.free_percent()), "{sample:?}");
    }

    #[test]
    fn the_image_build_hint_names_the_image() {
        assert_eq!(
            build_hint("hermes-agent:1.2.3"),
            "docker build -t hermes-agent:1.2.3 /path/to/hermes-official"
        );
    }

    #[test]
    fn a_resolv_conf_without_nameserver_fails() {
        assert_eq!(
            dns_check("search example.test\n").status,
            CheckStatus::Error
        );
        assert_eq!(dns_check("").status, CheckStatus::Error);
    }

    #[test]
    fn nameservers_ignore_comments_and_other_directives() {
        let text = "# generated\nsearch lan\nnameserver 9.9.9.9 # quad9\nnameserver\t10.0.0.1\noptions edns0\n";
        assert_eq!(nameservers(text), ["9.9.9.9", "10.0.0.1"]);
        let check = dns_check(text);
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.detail.contains("2 nameservers"));
        assert!(check.detail.contains("9.9.9.9"));
    }

    #[test]
    fn uptime_is_compact() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(42), "42s");
        assert_eq!(format_uptime(184), "3m 4s");
        assert_eq!(format_uptime(7500), "2h 5m");
        assert_eq!(format_uptime(93_600), "1d 2h");
        assert_eq!(format_uptime(-1), "0s");
    }

    #[test]
    fn bytes_use_binary_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn link_and_reason_are_optional_fields() {
        let check = Check::warn("assistant", "Assistant", "none selected")
            .with_reason("not_selected")
            .with_link(ASSISTANT_SELECTION_LINK);
        let value = serde_json::to_value(check).expect("check");
        assert_eq!(value["reason"], "not_selected");
        assert_eq!(value["link"], "/providers?assistant=1");
        assert!(value.get("fix").is_none(), "{value}");

        let plain = serde_json::to_value(Check::ok("api", "API", "up")).expect("check");
        assert!(plain.get("fix").is_none(), "{plain}");
        assert!(plain.get("link").is_none(), "{plain}");
        assert!(plain.get("reason").is_none(), "{plain}");
    }

    #[test]
    fn counts_are_pluralised() {
        assert_eq!(count(0, "instance"), "0 instances");
        assert_eq!(count(1, "instance"), "1 instance");
        assert_eq!(count(2, "provider"), "2 providers");
    }
}
