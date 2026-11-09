//! Per-instance destination policy for agent egress (issue #41).
//!
//! The historical model is binary: an instance either joins the restricted
//! `fleett` bridge (the host firewall drops LAN destinations) or sets
//! `allow_lan: true` and joins the unrestricted `<network>-lan` bridge. The
//! real requirement is narrower: "this agent needs `api.github.com` and
//! nothing else".
//!
//! A policy is stored per instance in `egress_policies` and rendered as
//! `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` in the agent container. The proxy
//! itself is **not** deployed by fleett: the operator runs the separate
//! `fleett-proxy` compose service described in `docker/fleett-proxy.example.yaml`
//! and in `docs/EGRESS-POLICY.md`. That gap is deliberate and documented: this
//! module owns the model, the validation and the decision, never a pretence of
//! enforcement.
//!
//! Two modes:
//!
//! * `deny` — default-deny allowlist. An empty policy denies everything.
//! * `allow` — default-allow blocklist. An empty policy allows everything.
//!
//! `fqdns` and `cidrs` are the exception set; `ports` optionally narrows the
//! exception to a set of destination ports (empty = every port). A `*.` prefix
//! matches subdomains only, never the apex.
//!
//! Compatibility: an instance without a policy keeps exactly the legacy
//! behaviour. `allow_lan: true` is reported as an `allow` policy with no
//! entries; `allow_lan: false` under `FLEETT_EGRESS=restrict` is reported as an
//! `allow` policy blocking the three RFC 1918 LAN ranges, which is what the
//! host firewall does.

use crate::config::{Config, EgressMode};
use crate::db::{Db, DbError};
use crate::models::{now_iso, InstanceSpec};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

/// Default proxy address rendered into every instance that has a policy.
pub const DEFAULT_PROXY_URL: &str = "http://fleett-egress-proxy:3128";
/// Compose service the operator deploys (`docker/fleett-proxy.example.yaml`).
pub const PROXY_SERVICE: &str = "fleett-proxy";
/// Container name of the proxy on the `fleett` network.
pub const PROXY_CONTAINER: &str = "fleett-egress-proxy";
/// Proxy port used by the shipped example.
pub const PROXY_PORT: u16 = 3128;
/// LAN ranges the legacy host firewall drops for a restricted instance.
pub const LAN_CIDRS: [&str; 3] = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"];
/// Longest accepted FQDN (RFC 1035).
pub const MAX_FQDN_LEN: usize = 253;
/// Longest accepted label.
pub const MAX_LABEL_LEN: usize = 63;
/// Maximum entries per list (`fqdns`, `cidrs`, `ports`).
pub const MAX_ENTRIES: usize = 64;
/// Longest accepted host in an ingestion record.
pub const MAX_BLOCK_HOST_LEN: usize = 253;
/// Longest accepted reason in an ingestion record.
pub const MAX_BLOCK_REASON_LEN: usize = 512;
/// Default page size of `GET …/egress/blocks`.
pub const BLOCKS_DEFAULT_LIMIT: usize = 200;
/// Largest page size of `GET …/egress/blocks`.
pub const BLOCKS_MAX_LIMIT: usize = 1000;
/// Ingestion budget per instance and per minute.
pub const INGEST_PER_MINUTE: usize = 120;
/// Timeout of the proxy reachability probe (`POST …/egress/test`).
pub const PROXY_PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// Direction of a destination policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyMode {
    /// Default-deny: only the listed destinations are allowed.
    Deny,
    /// Default-allow: the listed destinations are blocked.
    Allow,
}

impl PolicyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Allow => "allow",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "deny" | "allowlist" => Some(Self::Deny),
            "allow" | "blocklist" | "permit" => Some(Self::Allow),
            _ => None,
        }
    }
}

impl std::fmt::Display for PolicyMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One explicit policy, as stored in `egress_policies`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressPolicy {
    pub instance_id: String,
    pub mode: PolicyMode,
    pub fqdns: Vec<String>,
    pub cidrs: Vec<String>,
    pub ports: Vec<u16>,
    pub updated_at: String,
}

/// Body of `PUT /api/instances/{id}/egress`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PolicyInput {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub fqdns: Option<Vec<String>>,
    #[serde(default)]
    pub cidrs: Option<Vec<String>>,
    #[serde(default)]
    pub ports: Option<Vec<u16>>,
}

/// Where an effective policy comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicySource {
    /// `egress_policies` row written through the API.
    Explicit,
    /// No policy, `allow_lan: true`: the legacy unrestricted LAN bridge.
    AllowLan,
    /// No policy, restricted instance: the host firewall drops the LAN.
    Legacy,
    /// No policy and `FLEETT_EGRESS=off`: nothing is filtered.
    Disabled,
}

impl PolicySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::AllowLan => "allow_lan",
            Self::Legacy => "legacy",
            Self::Disabled => "disabled",
        }
    }
}

/// The policy actually in force for one instance.
///
/// It flattens the explicit policy and the two legacy shortcuts into the same
/// shape, so the test endpoint and the UI have one model to reason about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePolicy {
    pub source: PolicySource,
    pub mode: PolicyMode,
    pub fqdns: Vec<String>,
    pub cidrs: Vec<String>,
    pub ports: Vec<u16>,
}

impl EffectivePolicy {
    /// Decision for one destination. This is the *policy* answer; whether the
    /// proxy or the host firewall actually enforces it is reported separately.
    pub fn evaluate(&self, host: &str, port: u16) -> Decision {
        evaluate(self.mode, &self.fqdns, &self.cidrs, &self.ports, host, port)
    }
}

/// Answer of [`EffectivePolicy::evaluate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub allowed: bool,
    pub reason: String,
}

/// Resolve the policy in force for `spec`.
///
/// An explicit policy always wins. Without one, the legacy flags are projected
/// onto the same model: `allow_lan` allows everything, a restricted instance
/// under `FLEETT_EGRESS=restrict` denies the RFC 1918 ranges, and
/// `FLEETT_EGRESS=off` allows everything.
pub fn effective(
    cfg: &Config,
    spec: &InstanceSpec,
    policy: Option<&EgressPolicy>,
) -> EffectivePolicy {
    if let Some(policy) = policy {
        return EffectivePolicy {
            source: PolicySource::Explicit,
            mode: policy.mode,
            fqdns: policy.fqdns.clone(),
            cidrs: policy.cidrs.clone(),
            ports: policy.ports.clone(),
        };
    }
    if spec.allow_lan {
        return EffectivePolicy {
            source: PolicySource::AllowLan,
            mode: PolicyMode::Allow,
            fqdns: Vec::new(),
            cidrs: Vec::new(),
            ports: Vec::new(),
        };
    }
    match cfg.egress {
        EgressMode::Restrict => EffectivePolicy {
            source: PolicySource::Legacy,
            mode: PolicyMode::Allow,
            fqdns: Vec::new(),
            cidrs: LAN_CIDRS.iter().map(|cidr| cidr.to_string()).collect(),
            ports: Vec::new(),
        },
        EgressMode::Off => EffectivePolicy {
            source: PolicySource::Disabled,
            mode: PolicyMode::Allow,
            fqdns: Vec::new(),
            cidrs: Vec::new(),
            ports: Vec::new(),
        },
    }
}

/// A validated policy, ready to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedPolicy {
    pub mode: PolicyMode,
    pub fqdns: Vec<String>,
    pub cidrs: Vec<String>,
    pub ports: Vec<u16>,
}

/// Normalise and validate the body of a policy write.
pub fn normalize_input(input: &PolicyInput) -> Result<NormalizedPolicy, String> {
    let mode = match input.mode.as_deref() {
        None => PolicyMode::Deny,
        Some(value) => PolicyMode::parse(value)
            .ok_or_else(|| format!("mode must be \"deny\" or \"allow\" (got \"{value}\")"))?,
    };

    let fqdns = normalize_list(
        input.fqdns.as_deref().unwrap_or_default(),
        validate_fqdn,
        "fqdn",
    )?;
    let cidrs = normalize_list(
        input.cidrs.as_deref().unwrap_or_default(),
        validate_cidr,
        "cidr",
    )?;
    let mut ports = input.ports.clone().unwrap_or_default();
    if ports.len() > MAX_ENTRIES {
        return Err(format!("ports accepts at most {MAX_ENTRIES} entries"));
    }
    for port in &ports {
        validate_port(*port)?;
    }
    ports.sort_unstable();
    ports.dedup();
    Ok(NormalizedPolicy {
        mode,
        fqdns,
        cidrs,
        ports,
    })
}

fn normalize_list(
    values: &[String],
    validate: fn(&str) -> Result<String, String>,
    label: &str,
) -> Result<Vec<String>, String> {
    if values.len() > MAX_ENTRIES {
        return Err(format!("{label} accepts at most {MAX_ENTRIES} entries"));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        seen.insert(validate(value)?);
    }
    Ok(seen.into_iter().collect())
}

/// Validate one FQDN (or a `*.` wildcard), returning its normalised form.
///
/// The value is lowercased and a trailing dot is removed. A scheme, a path, a
/// port, whitespace or an empty label is refused; `*` is only accepted as a
/// leading `*.` label.
pub fn validate_fqdn(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("a destination host must not be empty".to_string());
    }
    if trimmed.contains("://") {
        return Err(format!(
            "\"{trimmed}\" is a URL: give a bare host (no scheme), the proxy ACLs own the port"
        ));
    }
    if trimmed.contains('/') {
        return Err(format!("\"{trimmed}\" contains a path: give a bare host"));
    }
    if trimmed.contains(':') {
        return Err(format!(
            "\"{trimmed}\" contains a port: use the ports list instead"
        ));
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(format!("\"{trimmed}\" contains whitespace"));
    }

    let lowered = trimmed.trim_end_matches('.').to_ascii_lowercase();
    let host = match lowered.strip_prefix("*.") {
        Some(rest) => rest,
        None => lowered.as_str(),
    };
    if host.is_empty() {
        return Err("a wildcard must name a domain (\"*.example.com\")".to_string());
    }
    if lowered.len() > MAX_FQDN_LEN {
        return Err(format!(
            "\"{trimmed}\" is longer than {MAX_FQDN_LEN} characters"
        ));
    }
    if lowered.contains('*') && !lowered.starts_with("*.") {
        return Err(format!(
            "\"{trimmed}\": the only accepted wildcard is a leading \"*.\""
        ));
    }
    for label in host.split('.') {
        if label.is_empty() {
            return Err(format!("\"{trimmed}\" has an empty label"));
        }
        if label.len() > MAX_LABEL_LEN {
            return Err(format!(
                "\"{trimmed}\" has a label longer than {MAX_LABEL_LEN} characters"
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!(
                "\"{trimmed}\" has a label starting or ending with '-'"
            ));
        }
        if !label
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return Err(format!("\"{trimmed}\" has an invalid character in a label"));
        }
    }
    Ok(lowered)
}

/// Validate one CIDR, returning `ip/prefix` with a compressed address.
pub fn validate_cidr(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    let (address, prefix) = trimmed
        .split_once('/')
        .ok_or_else(|| format!("\"{trimmed}\" is not a CIDR: expected <ip>/<prefix>"))?;
    let address = IpAddr::from_str(address)
        .map_err(|_| format!("\"{trimmed}\": \"{address}\" is not an IP address"))?;
    let prefix: u8 = prefix
        .parse()
        .map_err(|_| format!("\"{trimmed}\": \"{prefix}\" is not a prefix length"))?;
    let max = match address {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    if prefix > max {
        return Err(format!(
            "\"{trimmed}\": the prefix of an IPv{} must be at most {max}",
            if max == 32 { 4 } else { 6 }
        ));
    }
    Ok(format!("{address}/{prefix}"))
}

/// Validate one destination port (`1..=65535`).
pub fn validate_port(port: u16) -> Result<(), String> {
    if port == 0 {
        return Err("a port must be between 1 and 65535".to_string());
    }
    Ok(())
}

/// Validate a host accepted by the test endpoint or a block record.
///
/// An IP literal is normalised; anything else must be a valid FQDN.
pub fn validate_host(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if let Ok(address) = IpAddr::from_str(trimmed) {
        return Ok(address.to_string());
    }
    validate_fqdn(trimmed)
}

/// Whether `host` is covered by one CIDR of the list.
pub fn host_in_cidrs(host: &str, cidrs: &[String]) -> Option<String> {
    let address = IpAddr::from_str(host.trim()).ok()?;
    cidrs.iter().find(|cidr| ip_in_cidr(address, cidr)).cloned()
}

fn ip_in_cidr(address: IpAddr, cidr: &str) -> bool {
    let Some((network, prefix)) = cidr.split_once('/') else {
        return false;
    };
    let Ok(network) = IpAddr::from_str(network) else {
        return false;
    };
    let Ok(prefix) = prefix.parse::<u32>() else {
        return false;
    };
    match (address, network) {
        (IpAddr::V4(address), IpAddr::V4(network)) => {
            let mask = if prefix == 0 {
                0
            } else if prefix >= 32 {
                u32::MAX
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(address) & mask) == (u32::from(network) & mask)
        }
        (IpAddr::V6(address), IpAddr::V6(network)) => {
            let mask = if prefix == 0 {
                0
            } else if prefix >= 128 {
                u128::MAX
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(address) & mask) == (u128::from(network) & mask)
        }
        _ => false,
    }
}

/// Whether one FQDN pattern covers `host`.
pub fn fqdn_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
        None => host == pattern,
    }
}

/// Pure decision function shared by the API and the tests.
pub fn evaluate(
    mode: PolicyMode,
    fqdns: &[String],
    cidrs: &[String],
    ports: &[u16],
    host: &str,
    port: u16,
) -> Decision {
    let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let fqdn = fqdns
        .iter()
        .find(|pattern| fqdn_matches(pattern, &normalized));
    let cidr = host_in_cidrs(&normalized, cidrs);
    let port_allowed = ports.is_empty() || ports.contains(&port);

    let destination = match (fqdn, cidr) {
        (Some(pattern), _) => Some(format!("fqdn {pattern}")),
        (None, Some(cidr)) => Some(format!("cidr {cidr}")),
        (None, None) => None,
    };

    match mode {
        PolicyMode::Deny => {
            if let Some(destination) = destination {
                if port_allowed {
                    Decision {
                        allowed: true,
                        reason: format!(
                            "{destination} matches the deny-mode allowlist on port {port}"
                        ),
                    }
                } else {
                    Decision {
                        allowed: false,
                        reason: format!(
                            "{destination} matches but port {port} is not in the allowed ports {:?}",
                            ports
                        ),
                    }
                }
            } else {
                Decision {
                    allowed: false,
                    reason: format!(
                        "{normalized}:{port} is not in the deny-mode allowlist ({})",
                        describe_lists(fqdns, cidrs)
                    ),
                }
            }
        }
        PolicyMode::Allow => {
            if let Some(destination) = destination {
                if port_allowed {
                    Decision {
                        allowed: false,
                        reason: format!(
                            "{destination} matches the allow-mode blocklist on port {port}"
                        ),
                    }
                } else {
                    Decision {
                        allowed: true,
                        reason: format!(
                            "{destination} matches but port {port} is not in the blocked ports {:?}",
                            ports
                        ),
                    }
                }
            } else {
                Decision {
                    allowed: true,
                    reason: format!("{normalized}:{port} is not in the allow-mode blocklist"),
                }
            }
        }
    }
}

fn describe_lists(fqdns: &[String], cidrs: &[String]) -> String {
    let mut parts = Vec::new();
    if !fqdns.is_empty() {
        parts.push(format!("fqdns: {}", fqdns.join(", ")));
    }
    if !cidrs.is_empty() {
        parts.push(format!("cidrs: {}", cidrs.join(", ")));
    }
    if parts.is_empty() {
        "empty policy".to_string()
    } else {
        parts.join("; ")
    }
}

/// `NO_PROXY` rendered alongside the proxy variables.
///
/// Loopback and the `fleett` bridge stay direct so agent-to-agent traffic and
/// the gate never depend on the proxy. Operators with a different gate name
/// extend this list in their proxy configuration.
pub fn no_proxy(network: &str) -> String {
    format!("localhost,127.0.0.1,::1,.local,{network}")
}

/// Squid-style ACL snippet generated from one policy.
///
/// The endpoint returns it in the policy view so an operator can copy it into
/// the shared proxy without re-deriving the lists. It is documentation, not a
/// deployment: fleett never writes the proxy configuration.
pub fn squid_acl(policy: &EgressPolicy) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# fleett egress policy for {} ({} mode)\n",
        policy.instance_id, policy.mode
    ));
    if !policy.fqdns.is_empty() {
        out.push_str(&format!(
            "acl fleett_{0}_domains dstdomain \"/etc/squid/fleett-{0}.domains\"\n",
            policy.instance_id
        ));
    }
    if !policy.cidrs.is_empty() {
        out.push_str(&format!(
            "acl fleett_{0}_nets dst \"/etc/squid/fleett-{0}.cidrs\"\n",
            policy.instance_id
        ));
    }
    if !policy.ports.is_empty() {
        let ports: Vec<String> = policy.ports.iter().map(u16::to_string).collect();
        out.push_str(&format!(
            "acl fleett_{}_ports port {}\n",
            policy.instance_id,
            ports.join(" ")
        ));
    }
    match policy.mode {
        PolicyMode::Deny => {
            if !policy.fqdns.is_empty() {
                out.push_str(&format!(
                    "http_access allow fleett_{0}_domains{1}\n",
                    policy.instance_id,
                    port_acl(policy)
                ));
            }
            if !policy.cidrs.is_empty() {
                out.push_str(&format!(
                    "http_access allow fleett_{0}_nets{1}\n",
                    policy.instance_id,
                    port_acl(policy)
                ));
            }
            out.push_str("http_access deny all\n");
        }
        PolicyMode::Allow => {
            if !policy.fqdns.is_empty() {
                out.push_str(&format!(
                    "http_access deny fleett_{0}_domains{1}\n",
                    policy.instance_id,
                    port_acl(policy)
                ));
            }
            if !policy.cidrs.is_empty() {
                out.push_str(&format!(
                    "http_access deny fleett_{0}_nets{1}\n",
                    policy.instance_id,
                    port_acl(policy)
                ));
            }
            out.push_str("http_access allow all\n");
        }
    }
    out
}

fn port_acl(policy: &EgressPolicy) -> String {
    if policy.ports.is_empty() {
        String::new()
    } else {
        format!(" fleett_{}_ports", policy.instance_id)
    }
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

const POLICY_COLUMNS: &str = "instance_id, mode, fqdns, cidrs, ports, updated_at";
const BLOCK_COLUMNS: &str = "id, instance_id, host, port, reason, at";

fn row_to_policy(row: &rusqlite::Row<'_>) -> rusqlite::Result<EgressPolicy> {
    let mode: String = row.get(1)?;
    let fqdns: String = row.get(2)?;
    let cidrs: String = row.get(3)?;
    let ports: String = row.get(4)?;
    Ok(EgressPolicy {
        instance_id: row.get(0)?,
        mode: PolicyMode::parse(&mode).unwrap_or(PolicyMode::Deny),
        fqdns: split_list(&fqdns),
        cidrs: split_list(&cidrs),
        ports: split_ports(&ports),
        updated_at: row.get(5)?,
    })
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

fn split_ports(value: &str) -> Vec<u16> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| entry.parse::<u16>().ok())
        .filter(|port| *port > 0)
        .collect()
}

fn join_list(values: &[String]) -> String {
    values.join(",")
}

fn join_ports(ports: &[u16]) -> String {
    ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Stored policy of one instance.
pub fn load(db: &Db, instance_id: &str) -> Result<Option<EgressPolicy>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {POLICY_COLUMNS} FROM egress_policies WHERE instance_id = ?1"),
            params![instance_id],
            row_to_policy,
        )
        .optional()
    })
}

/// Insert or replace the policy of one instance.
pub fn upsert(db: &Db, policy: &EgressPolicy) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO egress_policies (instance_id, mode, fqdns, cidrs, ports, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (instance_id) DO UPDATE SET \
                 mode = excluded.mode, \
                 fqdns = excluded.fqdns, \
                 cidrs = excluded.cidrs, \
                 ports = excluded.ports, \
                 updated_at = excluded.updated_at",
            params![
                policy.instance_id,
                policy.mode.as_str(),
                join_list(&policy.fqdns),
                join_list(&policy.cidrs),
                join_ports(&policy.ports),
                policy.updated_at,
            ],
        )
    })?;
    Ok(())
}

/// Delete the explicit policy of one instance. Returns whether a row existed.
pub fn delete(db: &Db, instance_id: &str) -> Result<bool, DbError> {
    let changed = db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM egress_policies WHERE instance_id = ?1",
            params![instance_id],
        )
    })?;
    Ok(changed > 0)
}

/// One refused destination reported by the proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressBlock {
    pub id: i64,
    pub instance_id: String,
    pub host: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at: String,
}

fn row_to_block(row: &rusqlite::Row<'_>) -> rusqlite::Result<EgressBlock> {
    Ok(EgressBlock {
        id: row.get(0)?,
        instance_id: row.get(1)?,
        host: row.get(2)?,
        port: row.get::<_, i64>(3)?.clamp(0, i64::from(u16::MAX)) as u16,
        reason: row.get(4)?,
        at: row.get(5)?,
    })
}

/// Record one refused destination.
pub fn insert_block(
    db: &Db,
    instance_id: &str,
    host: &str,
    port: u16,
    reason: Option<&str>,
) -> Result<EgressBlock, DbError> {
    let at = now_iso();
    let id = db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO egress_blocks (instance_id, host, port, reason, at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![instance_id, host, i64::from(port), reason, at],
        )?;
        Ok(conn.last_insert_rowid())
    })?;
    Ok(EgressBlock {
        id,
        instance_id: instance_id.to_string(),
        host: host.to_string(),
        port,
        reason: reason.map(str::to_string),
        at,
    })
}

/// Newest-first block records, optionally restricted to one instance.
pub fn list_blocks(
    db: &Db,
    instance_id: Option<&str>,
    limit: usize,
) -> Result<Vec<EgressBlock>, DbError> {
    let limit = limit.clamp(1, BLOCKS_MAX_LIMIT) as i64;
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {BLOCK_COLUMNS} FROM egress_blocks \
             WHERE (?1 IS NULL OR instance_id = ?1) \
             ORDER BY at DESC, id DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![instance_id, limit], row_to_block)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
}

/// Delete every block record of one instance, returning the number removed.
pub fn clear_blocks(db: &Db, instance_id: &str) -> Result<usize, DbError> {
    let changed = db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM egress_blocks WHERE instance_id = ?1",
            params![instance_id],
        )
    })?;
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Proxy probe
// ---------------------------------------------------------------------------

/// Whether a TCP connection to the proxy succeeds.
///
/// This deliberately checks the socket only: it proves the proxy process is
/// reachable, not that its ACLs match the policy. `POST …/egress/test` reports
/// the distinction instead of pretending a policy is enforced.
pub async fn probe_proxy(proxy_url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(proxy_url)
        .map_err(|err| format!("\"{proxy_url}\" is not a valid proxy URL: {err}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "\"{proxy_url}\": the proxy URL must use http:// or https://"
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("\"{proxy_url}\" has no host"))?
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(PROXY_PORT);
    let target = format!("{host}:{port}");
    match tokio::time::timeout(PROXY_PROBE_TIMEOUT, tokio::net::TcpStream::connect(&target)).await {
        Ok(Ok(_)) => Ok(target),
        Ok(Err(err)) => Err(format!("proxy {target} is unreachable: {err}")),
        Err(_) => Err(format!(
            "proxy {target} did not answer within {} ms",
            PROXY_PROBE_TIMEOUT.as_millis()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ChannelKind, Status, DEFAULT_API_KEY_ENV, DEFAULT_CPU_LIMIT};

    fn spec(allow_lan: bool) -> InstanceSpec {
        InstanceSpec {
            id: "alice".to_string(),
            name: "Alice".to_string(),
            template: "guest".to_string(),
            model: "m".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            provider: None,
            channel: ChannelKind::None,
            shares: Vec::new(),
            skills: Vec::new(),
            mem_limit_mb: 512,
            cpu_limit: DEFAULT_CPU_LIMIT,
            image: "hermes-agent:0.22.2-local".to_string(),
            previous_image: None,
            allow_lan,
            host: crate::hosts::LOCAL_HOST.to_string(),
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            disk_quota_mb: 0,
            disk_policy: crate::models::DiskPolicy::Warn,
            notes: String::new(),
            status: Status::Created,
            last_error: None,
            created_at: now_iso(),
            updated_at: now_iso(),
        }
    }

    fn policy(mode: PolicyMode, fqdns: &[&str], cidrs: &[&str], ports: &[u16]) -> EgressPolicy {
        EgressPolicy {
            instance_id: "alice".to_string(),
            mode,
            fqdns: fqdns.iter().map(|value| value.to_string()).collect(),
            cidrs: cidrs.iter().map(|value| value.to_string()).collect(),
            ports: ports.to_vec(),
            updated_at: "2026-11-09T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn fqdns_are_lowercased_and_bare() {
        assert_eq!(
            validate_fqdn("API.GitHub.com").expect("valid"),
            "api.github.com"
        );
        assert_eq!(
            validate_fqdn("*.Example.COM.").expect("wildcard"),
            "*.example.com"
        );
        assert!(validate_fqdn("https://api.github.com").is_err());
        assert!(validate_fqdn("api.github.com:443").is_err());
        assert!(validate_fqdn("api.github.com/path").is_err());
        assert!(validate_fqdn("api github.com").is_err());
        assert!(validate_fqdn("api..github.com").is_err());
        assert!(validate_fqdn("api.*.github.com").is_err());
        assert!(validate_fqdn("").is_err());
        assert!(validate_fqdn(&"a".repeat(MAX_FQDN_LEN + 1)).is_err());
    }

    #[test]
    fn cidrs_are_parsed_and_bounded() {
        assert_eq!(validate_cidr("10.0.0.0/8").expect("valid"), "10.0.0.0/8");
        assert_eq!(
            validate_cidr(" 192.168.1.0/24 ").expect("valid"),
            "192.168.1.0/24"
        );
        assert_eq!(validate_cidr("fd00::/8").expect("v6"), "fd00::/8");
        assert!(validate_cidr("10.0.0.0/33").is_err());
        assert!(validate_cidr("fd00::/129").is_err());
        assert!(validate_cidr("10.0.0.0").is_err());
        assert!(validate_cidr("not-a-cidr/8").is_err());
    }

    #[test]
    fn policy_input_is_normalised_and_capped() {
        let normalized = normalize_input(&PolicyInput {
            mode: Some("DENY".to_string()),
            fqdns: Some(vec![
                "API.GitHub.com".to_string(),
                "api.github.com".to_string(),
                "*.example.com".to_string(),
            ]),
            cidrs: Some(vec!["10.0.0.0/8".to_string()]),
            ports: Some(vec![443, 80, 443]),
        })
        .expect("valid policy");
        assert_eq!(normalized.mode, PolicyMode::Deny);
        assert_eq!(normalized.fqdns, ["*.example.com", "api.github.com"]);
        assert_eq!(normalized.cidrs, ["10.0.0.0/8"]);
        assert_eq!(normalized.ports, [80, 443]);

        let too_many: Vec<String> = (0..=MAX_ENTRIES)
            .map(|index| format!("h{index}.test"))
            .collect();
        assert!(normalize_input(&PolicyInput {
            fqdns: Some(too_many),
            ..PolicyInput::default()
        })
        .is_err());
        assert!(normalize_input(&PolicyInput {
            mode: Some("maybe".to_string()),
            ..PolicyInput::default()
        })
        .is_err());
        assert!(normalize_input(&PolicyInput {
            ports: Some(vec![0]),
            ..PolicyInput::default()
        })
        .is_err());
    }

    #[test]
    fn effective_policy_keeps_the_legacy_behaviour() {
        let mut cfg = Config::for_tests(std::env::temp_dir().join("fleett-egress-test"));
        cfg.egress = EgressMode::Restrict;

        let restricted = effective(&cfg, &spec(false), None);
        assert_eq!(restricted.source, PolicySource::Legacy);
        assert_eq!(restricted.mode, PolicyMode::Allow);
        assert_eq!(restricted.cidrs, LAN_CIDRS);
        assert!(!restricted.evaluate("10.1.20.1", 443).allowed);
        assert!(restricted.evaluate("api.github.com", 443).allowed);

        let lan = effective(&cfg, &spec(true), None);
        assert_eq!(lan.source, PolicySource::AllowLan);
        assert!(lan.evaluate("10.1.20.1", 443).allowed);

        cfg.egress = EgressMode::Off;
        let off = effective(&cfg, &spec(false), None);
        assert_eq!(off.source, PolicySource::Disabled);
        assert!(off.evaluate("10.1.20.1", 443).allowed);

        let explicit = policy(PolicyMode::Deny, &["api.github.com"], &[], &[443]);
        let effective_explicit = effective(&cfg, &spec(true), Some(&explicit));
        assert_eq!(effective_explicit.source, PolicySource::Explicit);
        assert_eq!(effective_explicit.mode, PolicyMode::Deny);
        assert!(effective_explicit.evaluate("api.github.com", 443).allowed);
        assert!(!effective_explicit.evaluate("10.1.20.1", 443).allowed);
    }

    #[test]
    fn deny_mode_is_an_allowlist() {
        let decision = evaluate(
            PolicyMode::Deny,
            &["api.github.com".to_string(), "*.example.com".to_string()],
            &["10.0.0.0/8".to_string()],
            &[443],
            "api.github.com",
            443,
        );
        assert!(decision.allowed, "{}", decision.reason);

        let decision = evaluate(
            PolicyMode::Deny,
            &["api.github.com".to_string()],
            &[],
            &[443],
            "api.github.com",
            80,
        );
        assert!(!decision.allowed);
        assert!(decision.reason.contains("port 80"), "{}", decision.reason);

        let decision = evaluate(
            PolicyMode::Deny,
            &[],
            &["10.1.0.0/16".to_string()],
            &[],
            "10.1.20.1",
            443,
        );
        assert!(decision.allowed, "{}", decision.reason);

        let decision = evaluate(PolicyMode::Deny, &[], &[], &[], "example.com", 443);
        assert!(!decision.allowed, "an empty deny policy denies everything");

        let decision = evaluate(
            PolicyMode::Deny,
            &["*.example.com".to_string()],
            &[],
            &[],
            "example.com",
            443,
        );
        assert!(!decision.allowed, "a wildcard does not match the apex");
    }

    #[test]
    fn allow_mode_is_a_blocklist() {
        let decision = evaluate(
            PolicyMode::Allow,
            &["api.github.com".to_string()],
            &["10.0.0.0/8".to_string()],
            &[],
            "api.github.com",
            443,
        );
        assert!(!decision.allowed);
        let decision = evaluate(
            PolicyMode::Allow,
            &["api.github.com".to_string()],
            &["10.0.0.0/8".to_string()],
            &[],
            "10.1.20.1",
            443,
        );
        assert!(!decision.allowed);
        let decision = evaluate(
            PolicyMode::Allow,
            &["api.github.com".to_string()],
            &[],
            &[],
            "example.com",
            443,
        );
        assert!(decision.allowed);

        // A blocked host on another port is allowed.
        let decision = evaluate(
            PolicyMode::Allow,
            &["api.github.com".to_string()],
            &[],
            &[443],
            "api.github.com",
            80,
        );
        assert!(decision.allowed, "{}", decision.reason);
    }

    #[test]
    fn cidr_matching_handles_v4_and_v6() {
        assert!(ip_in_cidr("10.1.2.3".parse().expect("ip"), "10.0.0.0/8"));
        assert!(!ip_in_cidr("11.0.0.1".parse().expect("ip"), "10.0.0.0/8"));
        assert!(ip_in_cidr("10.1.2.3".parse().expect("ip"), "10.1.0.0/16"));
        assert!(ip_in_cidr("fd00::1".parse().expect("ip"), "fd00::/8"));
        assert!(!ip_in_cidr("fd00::1".parse().expect("ip"), "10.0.0.0/8"));
        assert!(ip_in_cidr("192.168.1.1".parse().expect("ip"), "0.0.0.0/0"));
    }

    #[test]
    fn stored_policies_round_trip() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open db");
        assert!(load(&db, "alice").expect("load").is_none());

        let stored = policy(
            PolicyMode::Deny,
            &["api.github.com", "*.example.com"],
            &["10.0.0.0/8"],
            &[80, 443],
        );
        upsert(&db, &stored).expect("insert");
        let loaded = load(&db, "alice").expect("load").expect("row");
        assert_eq!(loaded.mode, PolicyMode::Deny);
        assert_eq!(loaded.fqdns, ["api.github.com", "*.example.com"]);
        assert_eq!(loaded.cidrs, ["10.0.0.0/8"]);
        assert_eq!(loaded.ports, [80, 443]);

        let replaced = policy(PolicyMode::Allow, &["blocked.test"], &[], &[]);
        upsert(&db, &replaced).expect("update");
        let loaded = load(&db, "alice").expect("load").expect("row");
        assert_eq!(loaded.mode, PolicyMode::Allow);
        assert_eq!(loaded.fqdns, ["blocked.test"]);
        assert!(loaded.cidrs.is_empty());

        assert!(delete(&db, "alice").expect("delete"));
        assert!(load(&db, "alice").expect("load").is_none());
        assert!(!delete(&db, "alice").expect("idempotent delete"));
    }

    #[test]
    fn blocks_round_trip_and_clear() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(&dir.path().join("fleet.db")).expect("open db");

        let block = insert_block(&db, "alice", "10.1.20.1", 443, Some("destination denied"))
            .expect("insert");
        assert!(block.id > 0);
        insert_block(&db, "bob", "example.com", 80, None).expect("insert");

        let alice = list_blocks(&db, Some("alice"), BLOCKS_DEFAULT_LIMIT).expect("list");
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].host, "10.1.20.1");
        assert_eq!(alice[0].port, 443);

        let all = list_blocks(&db, None, BLOCKS_DEFAULT_LIMIT).expect("list all");
        assert_eq!(all.len(), 2);

        assert_eq!(clear_blocks(&db, "alice").expect("clear"), 1);
        assert!(list_blocks(&db, Some("alice"), BLOCKS_DEFAULT_LIMIT)
            .expect("list")
            .is_empty());
        assert_eq!(
            list_blocks(&db, None, BLOCKS_DEFAULT_LIMIT)
                .expect("list")
                .len(),
            1
        );
    }

    #[test]
    fn proxy_url_and_no_proxy_are_derived() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        assert_eq!(cfg.egress_proxy_url(), Some(DEFAULT_PROXY_URL));
        cfg.egress_proxy = "off".to_string();
        assert!(cfg.egress_proxy_url().is_none());
        cfg.egress_proxy = "  ".to_string();
        assert!(cfg.egress_proxy_url().is_none());

        assert_eq!(no_proxy("fleett"), "localhost,127.0.0.1,::1,.local,fleett");
    }

    #[test]
    fn squid_acls_follow_the_mode() {
        let deny = policy(
            PolicyMode::Deny,
            &["api.github.com"],
            &["10.0.0.0/8"],
            &[443],
        );
        let acl = squid_acl(&deny);
        assert!(acl.contains("http_access allow fleett_alice_domains fleett_alice_ports"));
        assert!(acl.contains("http_access deny all"));

        let allow = policy(PolicyMode::Allow, &["blocked.test"], &[], &[]);
        let acl = squid_acl(&allow);
        assert!(acl.contains("http_access deny fleett_alice_domains"));
        assert!(acl.contains("http_access allow all"));
    }

    #[test]
    fn hosts_and_ports_are_validated() {
        assert_eq!(validate_host("10.1.20.1").expect("ip"), "10.1.20.1");
        assert_eq!(
            validate_host("API.GitHub.com").expect("fqdn"),
            "api.github.com"
        );
        assert!(validate_host("https://example.com").is_err());
        assert!(validate_port(0).is_err());
        assert!(validate_port(65535).is_ok());
    }
}
