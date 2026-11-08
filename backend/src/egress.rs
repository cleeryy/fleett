//! Host firewall for agent egress.
//!
//! Agents join the restricted `fleett` bridge network. That network still
//! routes to every LAN network the host can reach (Proxmox, OPNsense, the
//! bastion, …), so the host firewall is the only place where egress can be
//! filtered: Docker publishes `DOCKER-USER` for exactly this purpose.
//!
//! `fleett egress install` writes five rules into `DOCKER-USER`, in this order:
//!
//! 1. accept established/related flows (replies to allowed connections),
//! 2. accept traffic inside the `fleett` subnet (agents ↔ agents, the gate),
//! 3. drop 10.0.0.0/8, 172.16.0.0/12 and 192.168.0.0/16 from that subnet.
//!
//! Every rule carries `--comment fleett-egress`, which is what `status` and
//! `remove` look for. The command also writes a small marker
//! (`/etc/fleett/egress.json`) recording the subnet the rules were installed
//! for; the control plane refuses to create restricted instances when that
//! marker is missing or stale (`FLEETT_EGRESS=restrict`, the default).
//!
//! The module is deliberately synchronous: it is a CLI operation, not part of
//! the HTTP server's hot path.

use crate::config::{Config, EgressMode, DEFAULT_EGRESS_MARKER};
use crate::models::now_iso;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Comment tagging every rule owned by fleett.
pub const COMMENT: &str = "fleett-egress";
/// Docker's user-defined chain, evaluated before its own forwarding rules.
pub const CHAIN: &str = "DOCKER-USER";
/// Network ranges an agent must never reach.
pub const BLOCKED_V4: [&str; 3] = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"];
/// Capability that allows manipulating the firewall without being root.
pub const CAP_NET_ADMIN: u64 = 1 << 12;

/// Message returned (and serialised into `409 egress_not_enforced`) when the
/// operator has to run the installer.
pub const INSTALL_HINT: &str = "run `sudo fleett egress install` on the host";

/// Warning attached to any `allow_lan=true` instance.
pub const WARNING_LAN: &str = "allow_lan is enabled: this agent joins the unrestricted \
     fleett-lan network and can reach the host LAN (10.0.0.0/8, 172.16.0.0/12, \
     192.168.0.0/16) and every service it exposes";

/// Warning added while `FLEETT_EGRESS=off`.
pub const WARNING_DISABLED: &str = "FLEETT_EGRESS=off: the host egress firewall is not \
     enforced, agents on the fleett network are not isolated from the LAN";

/// Warning added when the marker cannot prove the firewall is installed.
pub const WARNING_NOT_INSTALLED: &str = "the egress firewall is not installed for the current \
     fleett subnet; agents may reach the host LAN - run `sudo fleett egress install`";

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// One firewall rule, as the arguments that follow `iptables <op> DOCKER-USER`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub args: Vec<String>,
}

impl Rule {
    fn new(args: &[&str]) -> Self {
        Self {
            args: args.iter().map(|arg| arg.to_string()).collect(),
        }
    }

    /// `iptables -C DOCKER-USER …` (exit 0 when the rule already exists).
    pub fn check_args(&self) -> Vec<String> {
        let mut args = vec!["-C".to_string(), CHAIN.to_string()];
        args.extend(self.args.iter().cloned());
        args
    }

    /// `iptables -A DOCKER-USER …`.
    pub fn append_args(&self) -> Vec<String> {
        let mut args = vec!["-A".to_string(), CHAIN.to_string()];
        args.extend(self.args.iter().cloned());
        args
    }

    /// `iptables -D DOCKER-USER …`.
    pub fn delete_args(&self) -> Vec<String> {
        let mut args = vec!["-D".to_string(), CHAIN.to_string()];
        args.extend(self.args.iter().cloned());
        args
    }

    /// The command `install` runs, for `print` and for the report.
    pub fn command_line(&self) -> String {
        format!("iptables {}", self.append_args().join(" "))
    }
}

/// The five rules, in the order they must be evaluated.
pub fn rules(subnet: &str) -> Vec<Rule> {
    let mut rules = vec![Rule::new(&[
        "-m",
        "comment",
        "--comment",
        COMMENT,
        "-m",
        "conntrack",
        "--ctstate",
        "ESTABLISHED,RELATED",
        "-j",
        "ACCEPT",
    ])];
    rules.push(Rule::new(&[
        "-m",
        "comment",
        "--comment",
        COMMENT,
        "-s",
        subnet,
        "-d",
        subnet,
        "-j",
        "ACCEPT",
    ]));
    for blocked in BLOCKED_V4 {
        rules.push(Rule::new(&[
            "-m",
            "comment",
            "--comment",
            COMMENT,
            "-s",
            subnet,
            "-d",
            blocked,
            "-j",
            "DROP",
        ]));
    }
    rules
}

/// The exact commands `install` would run, one per line.
pub fn print_plan(subnet: &str) -> String {
    let mut out = String::new();
    for rule in rules(subnet) {
        out.push_str(&rule.command_line());
        out.push('\n');
    }
    out
}

/// Indices of the rules that are missing, given one "already present" flag per
/// rule (the exit status of `iptables -C`).
pub fn missing_indices(present: &[bool]) -> Vec<usize> {
    present
        .iter()
        .enumerate()
        .filter(|(_, present)| !**present)
        .map(|(index, _)| index)
        .collect()
}

/// Parse one `iptables -S DOCKER-USER` line into `-D` arguments.
///
/// Only lines that append a rule to `DOCKER-USER` and carry our comment are
/// accepted, so `remove` can never delete a rule it does not own.
pub fn parse_delete(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    let mut tokens = trimmed.split_whitespace();
    if tokens.next()? != "-A" {
        return None;
    }
    if tokens.next()? != CHAIN {
        return None;
    }
    let owns_rule = trimmed
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair[0] == "--comment" && pair[1].trim_matches('"') == COMMENT);
    if !owns_rule {
        return None;
    }
    let mut args = vec!["-D".to_string(), CHAIN.to_string()];
    args.extend(
        trimmed
            .split_whitespace()
            .skip(2)
            .map(|token| token.to_string()),
    );
    Some(args)
}

// ---------------------------------------------------------------------------
// Marker
// ---------------------------------------------------------------------------

/// Contents of `/etc/fleett/egress.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// Subnet the rules were installed for.
    pub subnet: String,
    /// RFC 3339 UTC timestamp of the last successful install.
    pub installed_at: String,
    /// fleett version that installed the rules.
    pub version: String,
}

/// Outcome of reading the marker file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerState {
    Missing,
    Invalid(String),
    Present(Marker),
}

impl MarkerState {
    pub fn is_present(&self) -> bool {
        matches!(self, Self::Present(_))
    }
}

/// Read the marker without ever failing: a missing or unreadable marker is a
/// state, not an error.
pub fn read_marker(path: &Path) -> MarkerState {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Marker>(&text) {
            Ok(marker) if !marker.subnet.trim().is_empty() => MarkerState::Present(marker),
            Ok(_) => MarkerState::Invalid("the marker has no subnet".to_string()),
            Err(err) => MarkerState::Invalid(err.to_string()),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => MarkerState::Missing,
        Err(err) => MarkerState::Invalid(err.to_string()),
    }
}

/// Write the marker (0644) after a successful install.
pub fn write_marker(path: &Path, subnet: &str) -> Result<Marker, String> {
    let marker = Marker {
        subnet: subnet.to_string(),
        installed_at: now_iso(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(&marker)
        .map_err(|err| format!("cannot serialise the marker: {err}"))?;
    std::fs::write(path, format!("{body}\n"))
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    set_mode(path, 0o644)?;
    Ok(marker)
}

/// Delete the marker. Returns whether a file was removed.
pub fn remove_marker(path: &Path) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("cannot remove {}: {err}", path.display())),
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|err| format!("cannot set the permissions of {}: {err}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

/// Whether the marker proves the rules are installed for `current_subnet`.
///
/// `current_subnet` is the IPv4 subnet of the `fleett` network as reported by
/// Docker. `None` means "unknown" (mock driver, unreadable inspect output): the
/// marker is then taken as-is, which is what the tests rely on.
pub fn check_marker(marker: &MarkerState, current_subnet: Option<&str>) -> Result<(), String> {
    match marker {
        MarkerState::Missing => Err(format!(
            "the egress firewall is not installed (no marker at {DEFAULT_EGRESS_MARKER}); \
             {INSTALL_HINT}"
        )),
        MarkerState::Invalid(detail) => Err(format!(
            "the egress firewall marker is unreadable ({detail}); {INSTALL_HINT}"
        )),
        MarkerState::Present(marker) => match current_subnet {
            Some(current) if current != marker.subnet => Err(format!(
                "the egress firewall is installed for {} but the fleett network is now {}; \
                 {INSTALL_HINT}",
                marker.subnet, current
            )),
            _ => Ok(()),
        },
    }
}

/// Warnings the API attaches to an instance detail.
pub fn warnings(cfg: &Config, allow_lan: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    match cfg.egress {
        EgressMode::Off => warnings.push(WARNING_DISABLED.to_string()),
        EgressMode::Restrict => {
            if !allow_lan && !read_marker(&cfg.egress_marker).is_present() {
                warnings.push(WARNING_NOT_INSTALLED.to_string());
            }
        }
    }
    if allow_lan {
        warnings.push(WARNING_LAN.to_string());
    }
    warnings
}

// ---------------------------------------------------------------------------
// Privileges
// ---------------------------------------------------------------------------

/// Effective UID, read from `/proc/self/status` (Linux, no libc dependency).
pub fn effective_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

/// Effective capabilities bitmask, read from `/proc/self/status`.
pub fn effective_caps() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("CapEff:"))?;
    u64::from_str_radix(line.split_whitespace().nth(1)?, 16).ok()
}

/// `None` when the process may change the firewall, `Some(message)` otherwise.
pub fn privilege_error(euid: Option<u32>, caps: Option<u64>) -> Option<String> {
    if euid == Some(0) {
        return None;
    }
    if caps.is_some_and(|mask| mask & CAP_NET_ADMIN != 0) {
        return None;
    }
    Some(
        "changing the egress firewall requires root or CAP_NET_ADMIN \
         (`sudo fleett egress install`)"
            .to_string(),
    )
}

// ---------------------------------------------------------------------------
// Docker network inspection
// ---------------------------------------------------------------------------

/// First IPv4 subnet declared by `docker network inspect <name>`.
///
/// The output is a JSON array; only the first network is read. IPv6 subnets
/// (`fd00::/64`, …) are skipped: the rules installed by this module are v4.
pub fn first_ipv4_subnet(inspect_json: &str) -> Result<String, String> {
    let networks: serde_json::Value = serde_json::from_str(inspect_json)
        .map_err(|err| format!("cannot parse `docker network inspect` output: {err}"))?;
    let network = networks
        .as_array()
        .and_then(|items| items.first())
        .ok_or_else(|| "`docker network inspect` returned no network".to_string())?;
    let configs = network
        .get("IPAM")
        .and_then(|ipam| ipam.get("Config"))
        .and_then(serde_json::Value::as_array);
    let subnets: Vec<&str> = configs
        .map(|configs| {
            configs
                .iter()
                .filter_map(|config| config.get("Subnet").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    subnets
        .iter()
        .find(|subnet| !subnet.contains(':'))
        .map(|subnet| subnet.to_string())
        .ok_or_else(|| {
            "the docker network has no IPv4 subnet; create it with `docker network create \
             --subnet <cidr> <name>`"
                .to_string()
        })
}

fn docker_network_inspect(docker: &str, network: &str) -> Result<String, String> {
    let output = Command::new(docker)
        .args(["network", "inspect", network])
        .output()
        .map_err(|err| format!("cannot run `{docker} network inspect {network}`: {err}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(format!(
        "`{docker} network inspect {network}` failed: {}",
        if stderr.is_empty() {
            format!("exit status {}", output.status)
        } else {
            stderr
        }
    ))
}

/// Subnet of an existing Docker network.
pub fn network_subnet(docker: &str, network: &str) -> Result<String, String> {
    first_ipv4_subnet(&docker_network_inspect(docker, network)?)
}

// ---------------------------------------------------------------------------
// iptables
// ---------------------------------------------------------------------------

fn iptables(args: &[String]) -> Result<std::process::Output, String> {
    Command::new("iptables")
        .args(args)
        .output()
        .map_err(|err| format!("cannot run `iptables`: {err}"))
}

fn exit_code(output: &std::process::Output) -> Option<i32> {
    output.status.code()
}

fn iptables_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        format!("exit status {}", output.status)
    } else {
        stderr
    }
}

/// Exit 0 when the rule is in the chain, 1 when it is not, anything else is an
/// error (missing privileges, missing chain, …).
fn rule_present(rule: &Rule) -> Result<bool, String> {
    let output = iptables(&rule.check_args())?;
    match exit_code(&output) {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "`iptables -C {CHAIN}` failed: {}",
            iptables_detail(&output)
        )),
    }
}

fn ensure_privileges() -> Result<(), String> {
    match privilege_error(effective_uid(), effective_caps()) {
        None => Ok(()),
        Some(message) => Err(message),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Install outcome, printed by `fleett egress install`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub subnet: String,
    pub added: usize,
    pub already_present: usize,
    pub marker_path: PathBuf,
}

/// Install (idempotently) the five rules for the current `fleett` subnet, then
/// write the marker.
pub fn install(docker: &str, network: &str, marker_path: &Path) -> Result<InstallReport, String> {
    ensure_privileges()?;
    let subnet = network_subnet(docker, network)?;
    let rules = rules(&subnet);

    let mut present = Vec::with_capacity(rules.len());
    for rule in &rules {
        present.push(rule_present(rule)?);
    }

    let mut added = 0;
    for index in missing_indices(&present) {
        let output = iptables(&rules[index].append_args())?;
        if !output.status.success() {
            return Err(format!(
                "`{}` failed: {}",
                rules[index].command_line(),
                iptables_detail(&output)
            ));
        }
        added += 1;
    }

    write_marker(marker_path, &subnet)?;
    Ok(InstallReport {
        subnet,
        added,
        already_present: rules.len() - added,
        marker_path: marker_path.to_path_buf(),
    })
}

/// State reported by `fleett egress status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    pub subnet: String,
    pub present: Vec<bool>,
    pub marker: MarkerState,
    pub marker_path: PathBuf,
}

impl StatusReport {
    /// `status` exits 0 only when every rule is present for the current subnet.
    pub fn enforced(&self) -> bool {
        !self.present.is_empty() && self.present.iter().all(|present| *present)
    }

    pub fn render(&self, network: &str) -> String {
        let mut out = format!("network: {network} ({})\n", self.subnet);
        match &self.marker {
            MarkerState::Present(marker) => out.push_str(&format!(
                "marker:  {} (installed {} by fleett {})\n",
                self.marker_path.display(),
                marker.installed_at,
                marker.version
            )),
            MarkerState::Missing => out.push_str(&format!(
                "marker:  {} (missing)\n",
                self.marker_path.display()
            )),
            MarkerState::Invalid(detail) => out.push_str(&format!(
                "marker:  {} (unreadable: {detail})\n",
                self.marker_path.display()
            )),
        }
        for (rule, present) in rules(&self.subnet).iter().zip(&self.present) {
            out.push_str(if *present { "  ok      " } else { "  MISSING " });
            out.push_str(&rule.command_line());
            out.push('\n');
        }
        let missing = self.present.iter().filter(|present| !**present).count();
        if missing == 0 {
            out.push_str("egress firewall: enforced\n");
        } else {
            out.push_str(&format!(
                "egress firewall: NOT enforced ({missing} of {} rules missing); {INSTALL_HINT}\n",
                self.present.len()
            ));
        }
        out
    }
}

/// Read-only status of the installed rules.
pub fn status(docker: &str, network: &str, marker_path: &Path) -> Result<StatusReport, String> {
    let subnet = network_subnet(docker, network)?;
    let mut present = Vec::with_capacity(BLOCKED_V4.len() + 2);
    for rule in rules(&subnet) {
        present.push(rule_present(&rule)?);
    }
    Ok(StatusReport {
        subnet,
        present,
        marker: read_marker(marker_path),
        marker_path: marker_path.to_path_buf(),
    })
}

/// Remove every rule carrying the `fleett-egress` comment, then the marker.
///
/// The rules are discovered from the live chain, so a stale install (rules for
/// a subnet that no longer exists) is cleaned up as well.
pub fn remove(marker_path: &Path) -> Result<usize, String> {
    ensure_privileges()?;
    let output = iptables(&["-S".to_string(), CHAIN.to_string()])?;
    if !output.status.success() {
        return Err(format!(
            "`iptables -S {CHAIN}` failed: {}",
            iptables_detail(&output)
        ));
    }
    let listing = String::from_utf8_lossy(&output.stdout).into_owned();

    let mut removed = 0;
    for line in listing.lines() {
        let Some(args) = parse_delete(line) else {
            continue;
        };
        let output = iptables(&args)?;
        if !output.status.success() {
            return Err(format!(
                "`iptables {}` failed: {}",
                args.join(" "),
                iptables_detail(&output)
            ));
        }
        removed += 1;
    }
    remove_marker(marker_path)?;
    Ok(removed)
}

// ---------------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Remove,
    Status,
    Print,
}

impl Action {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "install" => Some(Self::Install),
            "remove" => Some(Self::Remove),
            "status" => Some(Self::Status),
            "print" => Some(Self::Print),
            _ => None,
        }
    }
}

/// Run one `fleett egress` action. The returned integer is the process exit
/// code: `status` uses 1 when the rules are not all installed.
pub fn run(action: Action, cfg: &Config) -> anyhow::Result<i32> {
    let marker = cfg.egress_marker.as_path();
    let docker = "docker";
    let fail = |detail: String| anyhow::anyhow!("{detail}");
    match action {
        Action::Print => {
            let subnet = network_subnet(docker, &cfg.network).map_err(fail)?;
            print!("{}", print_plan(&subnet));
            Ok(0)
        }
        Action::Install => {
            let report = install(docker, &cfg.network, marker).map_err(fail)?;
            println!(
                "egress firewall: installed {} rule(s) for {} ({} already present)",
                report.added, report.subnet, report.already_present
            );
            println!("marker: {}", report.marker_path.display());
            Ok(0)
        }
        Action::Status => {
            let report = status(docker, &cfg.network, marker).map_err(fail)?;
            print!("{}", report.render(&cfg.network));
            Ok(if report.enforced() { 0 } else { 1 })
        }
        Action::Remove => {
            let removed = remove(marker).map_err(fail)?;
            println!("egress firewall: removed {removed} rule(s) tagged {COMMENT}; marker deleted");
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_marker() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("fleett/egress.json");
        (dir, path)
    }

    #[test]
    fn rules_are_ordered_and_fully_tagged() {
        let subnet = "172.18.0.0/16";
        let rules = rules(subnet);
        assert_eq!(rules.len(), 5);

        let commands: Vec<String> = rules.iter().map(Rule::command_line).collect();
        assert_eq!(
            commands[0],
            "iptables -A DOCKER-USER -m comment --comment fleett-egress -m conntrack \
             --ctstate ESTABLISHED,RELATED -j ACCEPT"
        );
        assert_eq!(
            commands[1],
            "iptables -A DOCKER-USER -m comment --comment fleett-egress -s 172.18.0.0/16 \
             -d 172.18.0.0/16 -j ACCEPT"
        );
        assert_eq!(
            commands[2],
            "iptables -A DOCKER-USER -m comment --comment fleett-egress -s 172.18.0.0/16 \
             -d 10.0.0.0/8 -j DROP"
        );
        assert_eq!(
            commands[3],
            "iptables -A DOCKER-USER -m comment --comment fleett-egress -s 172.18.0.0/16 \
             -d 172.16.0.0/12 -j DROP"
        );
        assert_eq!(
            commands[4],
            "iptables -A DOCKER-USER -m comment --comment fleett-egress -s 172.18.0.0/16 \
             -d 192.168.0.0/16 -j DROP"
        );

        for rule in &rules {
            let args = rule.args.join(" ");
            assert!(args.contains("--comment fleett-egress"), "{args}");
            assert_eq!(rule.append_args()[0], "-A");
            assert_eq!(rule.check_args()[0], "-C");
            assert_eq!(rule.delete_args()[0], "-D");
            assert_eq!(rule.append_args()[1], CHAIN);
        }
        // The self-subnet accept comes before the drops: an agent reaches the
        // gate, never a LAN range.
        assert!(rules[1].args.contains(&subnet.to_string()));
        assert_eq!(print_plan(subnet).lines().count(), 5);
    }

    #[test]
    fn missing_rules_are_reported_by_index() {
        assert!(missing_indices(&[true, true, true, true, true]).is_empty());
        assert_eq!(missing_indices(&[false, true, true, true, true]), vec![0]);
        assert_eq!(
            missing_indices(&[true, true, false, true, false]),
            vec![2, 4]
        );
        assert_eq!(
            missing_indices(&[false, false, false, false, false]),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn delete_parsing_only_takes_our_rules() {
        let ours = "-A DOCKER-USER -m comment --comment fleett-egress -s 172.18.0.0/16 \
                    -d 10.0.0.0/8 -j DROP";
        let args = parse_delete(ours).expect("owned rule");
        assert_eq!(args[0], "-D");
        assert_eq!(args[1], CHAIN);
        assert!(args.contains(&"172.18.0.0/16".to_string()));
        assert!(args.contains(&"10.0.0.0/8".to_string()));

        assert!(parse_delete("-P DOCKER-USER ACCEPT").is_none());
        assert!(parse_delete("-A DOCKER-USER -s 172.18.0.0/16 -j DROP").is_none());
        assert!(parse_delete("-A FORWARD -m comment --comment fleett-egress -j DROP").is_none());
        assert!(parse_delete("# a comment").is_none());
        assert!(parse_delete("").is_none());
    }

    #[test]
    fn markers_round_trip_and_delete() {
        let (_dir, path) = temp_marker();
        assert_eq!(read_marker(&path), MarkerState::Missing);
        assert!(!remove_marker(&path).expect("remove missing"));

        let marker = write_marker(&path, "172.18.0.0/16").expect("write marker");
        assert_eq!(marker.subnet, "172.18.0.0/16");
        assert_eq!(marker.version, env!("CARGO_PKG_VERSION"));
        assert!(marker.installed_at.ends_with('Z'));

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("metadata");
        assert_eq!(mode.permissions().mode() & 0o777, 0o644);

        assert_eq!(read_marker(&path), MarkerState::Present(marker));
        assert!(remove_marker(&path).expect("remove"));
        assert_eq!(read_marker(&path), MarkerState::Missing);
    }

    #[test]
    fn unreadable_markers_are_invalid() {
        let (_dir, path) = temp_marker();
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, "{not json").expect("write");
        assert!(matches!(read_marker(&path), MarkerState::Invalid(_)));

        std::fs::write(&path, r#"{"subnet":"  ","installed_at":"x","version":"y"}"#)
            .expect("write");
        assert!(matches!(read_marker(&path), MarkerState::Invalid(_)));
    }

    #[test]
    fn markers_are_checked_against_the_current_subnet() {
        let present = MarkerState::Present(Marker {
            subnet: "172.18.0.0/16".to_string(),
            installed_at: "2026-11-08T00:00:00Z".to_string(),
            version: "0.2.0".to_string(),
        });

        // Unknown subnet (mock driver): the marker is taken as-is.
        assert!(check_marker(&present, None).is_ok());
        assert!(check_marker(&present, Some("172.18.0.0/16")).is_ok());

        let stale = check_marker(&present, Some("172.19.0.0/16")).expect_err("stale marker");
        assert!(stale.contains("172.18.0.0/16"), "{stale}");
        assert!(stale.contains("172.19.0.0/16"), "{stale}");
        assert!(stale.contains("fleett egress install"), "{stale}");

        let missing = check_marker(&MarkerState::Missing, None).expect_err("missing marker");
        assert!(missing.contains("not installed"), "{missing}");
        assert!(missing.contains("fleett egress install"), "{missing}");

        let invalid =
            check_marker(&MarkerState::Invalid("boom".to_string()), None).expect_err("invalid");
        assert!(invalid.contains("unreadable"), "{invalid}");
    }

    #[test]
    fn privilege_check_accepts_root_and_cap_net_admin() {
        assert!(privilege_error(Some(0), None).is_none());
        assert!(privilege_error(Some(0), Some(0)).is_none());
        assert!(privilege_error(Some(1000), Some(CAP_NET_ADMIN)).is_none());
        assert!(privilege_error(Some(1000), Some(CAP_NET_ADMIN | 1)).is_none());

        let err = privilege_error(Some(1000), Some(0)).expect("unprivileged");
        assert!(err.contains("root"), "{err}");
        assert!(err.contains("CAP_NET_ADMIN"), "{err}");
        assert!(privilege_error(None, None).is_some());

        // Warnings helper: the marker path is a state, never a panic.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().join("root"));
        cfg.egress = EgressMode::Restrict;
        assert!(warnings(&cfg, false)
            .iter()
            .any(|warning| warning.contains("not installed")));
        assert!(warnings(&cfg, true)
            .iter()
            .any(|warning| warning.contains("fleett-lan")));
        write_marker(&cfg.egress_marker, "172.18.0.0/16").expect("marker");
        assert!(warnings(&cfg, false).is_empty());
        cfg.egress = EgressMode::Off;
        assert!(warnings(&cfg, false)
            .iter()
            .any(|warning| warning.contains("FLEETT_EGRESS=off")));
    }

    #[test]
    fn docker_inspect_subnets_are_parsed() {
        let ipv4 = r#"[{"Name":"fleett","IPAM":{"Config":[{"Subnet":"172.18.0.0/16"}]}}]"#;
        assert_eq!(first_ipv4_subnet(ipv4).expect("subnet"), "172.18.0.0/16");

        let both = r#"[{"Name":"fleett","IPAM":{"Config":[
            {"Subnet":"fd00:dead::/64"},
            {"Subnet":"172.19.0.0/16"}
        ]}}]"#;
        assert_eq!(first_ipv4_subnet(both).expect("subnet"), "172.19.0.0/16");

        let v6 = r#"[{"IPAM":{"Config":[{"Subnet":"fd00:dead::/64"}]}}]"#;
        let err = first_ipv4_subnet(v6).expect_err("no ipv4 subnet");
        assert!(err.contains("no IPv4 subnet"), "{err}");

        let empty = r#"[{"Name":"fleett"}]"#;
        assert!(first_ipv4_subnet(empty).is_err());
        assert!(first_ipv4_subnet("not json").is_err());
        assert!(first_ipv4_subnet("[]").is_err());
    }

    #[test]
    fn actions_parse_from_the_cli_names() {
        for (text, expected) in [
            ("install", Action::Install),
            ("REMOVE", Action::Remove),
            (" status ", Action::Status),
            ("print", Action::Print),
        ] {
            assert_eq!(Action::parse(text), Some(expected), "{text}");
        }
        assert_eq!(Action::parse("reload"), None);
    }
}
