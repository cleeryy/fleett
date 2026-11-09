//! Runtime configuration.
//!
//! Configuration comes from the process environment, completed by an optional
//! environment file located in the data root. The file is a plain `KEY=VALUE`
//! list: no shell expansion, no command substitution, and process environment
//! variables always win over file entries.
//!
//! Variables are read with the canonical `FLEETT_*` prefix first, then with the
//! legacy `FLEET_*` prefix. The installer also accepts `HERMES_FLEET_*`.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_ROOT: &str = "/opt/fleett";
pub const DEFAULT_BIND: &str = "127.0.0.1:8090";
pub const DEFAULT_IMAGE: &str = "hermes-agent:0.22.2-local";
pub const DEFAULT_NETWORK: &str = "fleett";
/// Default sampling interval of the watchdog (`FLEETT_WATCHDOG_INTERVAL_S`).
pub const DEFAULT_WATCHDOG_INTERVAL_S: u64 = 30;
/// Marker written by `fleett egress install` and read by the control plane.
pub const DEFAULT_EGRESS_MARKER: &str = "/etc/fleett/egress.json";
/// Proxy every instance with a destination policy is pointed at (issue #41).
pub const DEFAULT_EGRESS_PROXY: &str = "http://fleett-egress-proxy:3128";
/// Key file used to encrypt instance secrets at rest.
///
/// Outside `FLEETT_ROOT` on purpose: a backup of the data root must never
/// contain the key that decrypts the secrets stored in it.
pub const DEFAULT_SECRETS_KEY_FILE: &str = "/etc/fleett/secrets.key";

/// Name of the environment file inside `FLEETT_ROOT`.
pub const ENV_FILE_NAME: &str = "fleett.env";
/// Legacy name of the same file; read when `fleett.env` is absent.
pub const LEGACY_ENV_FILE_NAME: &str = "fleet.env";
/// Directory under `$HOME` used by the installer.
pub const HOME_ROOT_NAME: &str = ".fleett";
/// Legacy data root, adopted when `~/.fleett` does not exist yet.
pub const LEGACY_HOME_ROOT_NAME: &str = ".hermes-fleet";

/// Canonical configuration prefix.
pub const PREFIX: &str = "FLEETT_";
/// Legacy configuration prefix, still accepted.
pub const LEGACY_PREFIX: &str = "FLEET_";

/// Docker driver selected by `FLEETT_DRIVER` (or `FLEET_DRIVER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind {
    Mock,
    Compose,
}

impl DriverKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mock => "mock",
            Self::Compose => "compose",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Some(Self::Mock),
            "compose" | "docker" => Some(Self::Compose),
            _ => None,
        }
    }
}

impl fmt::Display for DriverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How strictly the control plane enforces the host egress firewall.
///
/// `restrict` (default): creating or patching an instance that stays on the
/// restricted network requires a fresh `fleett egress install` marker.
/// `off`: no check, with a startup warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressMode {
    Restrict,
    Off,
}

impl EgressMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Restrict => "restrict",
            Self::Off => "off",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "restrict" => Some(Self::Restrict),
            "off" | "disabled" => Some(Self::Off),
            _ => None,
        }
    }
}

impl fmt::Display for EgressMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How strictly the agent containers are hardened (`FLEETT_HARDENING`).
///
/// `standard` is the historical posture: `no-new-privileges`, `NET_RAW` dropped
/// and a writable `/tmp` tmpfs. `strict` makes the root filesystem read-only and
/// drops every capability; it needs an image that can start without writing
/// outside its data volume (see docs/HARDENING.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Hardening {
    #[default]
    Standard,
    Strict,
}

impl Hardening {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Strict => "strict",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "standard" | "default" => Some(Self::Standard),
            "strict" | "hardened" => Some(Self::Strict),
            _ => None,
        }
    }
}

impl fmt::Display for Hardening {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Settings of the dedicated Prometheus listener (`FLEETT_METRICS_*`).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct MetricsConfig {
    /// `None` disables the listener (`FLEETT_METRICS_BIND` empty).
    pub bind: Option<SocketAddr>,
    /// Bearer token required by `GET /metrics`, when one is configured.
    pub token: Option<String>,
}

impl MetricsConfig {
    /// Whether a scrape has to carry a bearer token.
    pub fn requires_token(&self) -> bool {
        self.token.is_some()
    }
}

/// `Debug` is implemented by hand: the metrics token must never reach a log
/// line, exactly like the OIDC secrets.
impl fmt::Debug for MetricsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetricsConfig")
            .field("bind", &self.bind)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Authentication mode selected by `FLEETT_AUTH`.
///
/// `bearer` is the historical behaviour (one static token plus named API
/// tokens), `oidc` only trusts browser sessions, `both` accepts either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Bearer,
    Oidc,
    Both,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::Oidc => "oidc",
            Self::Both => "both",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bearer" | "token" => Some(Self::Bearer),
            "oidc" | "sso" => Some(Self::Oidc),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    /// Whether browser sessions (the OIDC flow) are accepted.
    pub fn oidc_enabled(self) -> bool {
        matches!(self, Self::Oidc | Self::Both)
    }

    /// Whether `Authorization: Bearer` is accepted.
    pub fn bearer_enabled(self) -> bool {
        matches!(self, Self::Bearer | Self::Both)
    }
}

impl fmt::Display for AuthMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `FLEETT_COOKIE_SECURE`: force the `Secure` cookie attribute or derive it
/// from `FLEETT_PUBLIC_URL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieSecure {
    Auto,
    Always,
    Never,
}

impl CookieSecure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "true",
            Self::Never => "false",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "true" | "1" | "yes" | "on" => Some(Self::Always),
            "false" | "0" | "no" | "off" => Some(Self::Never),
            _ => None,
        }
    }

    /// Resolve the attribute against the public URL.
    pub fn resolve(self, public_url: &str) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => public_url
                .trim()
                .to_ascii_lowercase()
                .starts_with("https://"),
        }
    }
}

/// Settings of the OpenID Connect provider (`FLEETT_OIDC_*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcSettings {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    /// Space-separated scopes, `openid` first.
    pub scopes: String,
    pub admin_group: String,
    pub viewer_group: String,
}

/// Resolved OIDC configuration: the provider settings plus everything the
/// session cookie needs.
///
/// `Debug` is implemented by hand: the client secret and the session key must
/// never reach a log line.
#[derive(Clone, PartialEq, Eq)]
pub struct OidcConfig {
    pub public_url: String,
    pub settings: OidcSettings,
    /// HMAC key of the session and state cookies (at least 32 bytes).
    pub session_secret: Vec<u8>,
    pub session_ttl_secs: u64,
    /// Lifetime of the `fleett_oidc` state cookie, in seconds.
    pub state_ttl_secs: u64,
    /// `Secure` attribute of every cookie we set.
    pub cookie_secure: bool,
}

impl fmt::Debug for OidcConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcConfig")
            .field("public_url", &self.public_url)
            .field("issuer", &self.settings.issuer)
            .field("client_id", &self.settings.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_url", &self.settings.redirect_url)
            .field("scopes", &self.settings.scopes)
            .field("admin_group", &self.settings.admin_group)
            .field("viewer_group", &self.settings.viewer_group)
            .field("session_secret", &"<redacted>")
            .field("session_ttl_secs", &self.session_ttl_secs)
            .field("state_ttl_secs", &self.state_ttl_secs)
            .field("cookie_secure", &self.cookie_secure)
            .finish()
    }
}

/// Default bind of the dedicated Prometheus listener (`GET /metrics`).
pub const DEFAULT_METRICS_BIND: &str = "127.0.0.1:9100";

/// Requests per minute allowed on `/api/*`, per client and credential
/// (`FLEETT_RATE_LIMIT_PER_MIN`); `0` disables the limiter.
pub const DEFAULT_RATE_LIMIT_PER_MIN: u32 = 120;
/// Requests per minute allowed on the browser authentication pages
/// (`FLEETT_RATE_LIMIT_AUTH_PER_MIN`); `0` disables the bucket.
pub const DEFAULT_RATE_LIMIT_AUTH_PER_MIN: u32 = 10;
/// Requests per minute allowed on the heavy instance routes — `/exec`, `/logs`,
/// `/files`, `/export` (`FLEETT_RATE_LIMIT_HEAVY_PER_MIN`); `0` disables the
/// bucket.
pub const DEFAULT_RATE_LIMIT_HEAVY_PER_MIN: u32 = 30;
/// Failed authentications tolerated from one IP and credential shape before the
/// progressive lockout engages (`FLEETT_AUTH_LOCKOUT_FAILURES`); `0` disables
/// the lockout.
pub const DEFAULT_AUTH_LOCKOUT_FAILURES: u32 = 10;
/// Sliding window of the failure counter (`FLEETT_AUTH_LOCKOUT_WINDOW_S`).
pub const DEFAULT_AUTH_LOCKOUT_WINDOW_S: u64 = 600;
/// How long the lockout refuses new attempts (`FLEETT_AUTH_LOCKOUT_S`).
pub const DEFAULT_AUTH_LOCKOUT_S: u64 = 900;

/// API hardening: rate limits, progressive lockout and header posture.
///
/// A rate limit of `0` disables that bucket; `FLEETT_AUTH_LOCKOUT_FAILURES=0`
/// disables the lockout entirely, so the window and duration are only used
/// when at least one failure is tolerated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityConfig {
    /// `FLEETT_RATE_LIMIT_PER_MIN`, `0` = disabled.
    pub rate_limit_per_min: u32,
    /// `FLEETT_RATE_LIMIT_AUTH_PER_MIN`, `0` = disabled.
    pub rate_limit_auth_per_min: u32,
    /// `FLEETT_RATE_LIMIT_HEAVY_PER_MIN`, `0` = disabled.
    pub rate_limit_heavy_per_min: u32,
    /// `FLEETT_AUTH_LOCKOUT_FAILURES`, `0` = disabled.
    pub lockout_failures: u32,
    /// `FLEETT_AUTH_LOCKOUT_WINDOW_S`.
    pub lockout_window_s: u64,
    /// `FLEETT_AUTH_LOCKOUT_S`.
    pub lockout_s: u64,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            rate_limit_per_min: DEFAULT_RATE_LIMIT_PER_MIN,
            rate_limit_auth_per_min: DEFAULT_RATE_LIMIT_AUTH_PER_MIN,
            rate_limit_heavy_per_min: DEFAULT_RATE_LIMIT_HEAVY_PER_MIN,
            lockout_failures: DEFAULT_AUTH_LOCKOUT_FAILURES,
            lockout_window_s: DEFAULT_AUTH_LOCKOUT_WINDOW_S,
            lockout_s: DEFAULT_AUTH_LOCKOUT_S,
        }
    }
}

impl SecurityConfig {
    /// Whether at least one rate-limit bucket is active.
    pub fn limiting_enabled(&self) -> bool {
        self.rate_limit_per_min > 0
            || self.rate_limit_auth_per_min > 0
            || self.rate_limit_heavy_per_min > 0
    }

    /// Whether the progressive authentication lockout is active.
    pub fn lockout_enabled(&self) -> bool {
        self.lockout_failures > 0
    }
}

pub const DEFAULT_PUBLIC_URL: &str = "http://127.0.0.1:8090";
pub const DEFAULT_OIDC_SCOPES: &str = "openid profile email";
pub const DEFAULT_OIDC_ADMIN_GROUP: &str = "fleett-admins";
pub const DEFAULT_OIDC_VIEWER_GROUP: &str = "fleett-viewers";
pub const DEFAULT_SESSION_TTL_SECS: u64 = 28800;
/// Default lifetime of the OIDC state cookie (`FLEETT_OIDC_STATE_TTL_SECS`).
pub const DEFAULT_OIDC_STATE_TTL_SECS: u64 = 600;
/// Shortest accepted `FLEETT_OIDC_STATE_TTL_SECS`: a login that completes
/// faster than a minute is not a realistic flow.
pub const MIN_OIDC_STATE_TTL_SECS: u64 = 60;
/// Longest accepted `FLEETT_OIDC_STATE_TTL_SECS`: a state cookie is a
/// short-lived secret, it must not outlive the login by hours.
pub const MAX_OIDC_STATE_TTL_SECS: u64 = 3600;
/// Minimum length of the decoded `FLEETT_SESSION_SECRET`.
pub const MIN_SESSION_SECRET_BYTES: usize = 32;
/// Default retention of the persistent log center (`FLEETT_LOG_RETENTION`).
pub const DEFAULT_LOG_RETENTION: &str = "7d";
/// Default row cap of the persistent log center (`FLEETT_LOG_MAX_ROWS`).
pub const DEFAULT_LOG_MAX_ROWS: u64 = 500_000;

/// Fully resolved configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub bind: SocketAddr,
    /// Public base URL (`FLEETT_PUBLIC_URL`); used by HSTS and notification links.
    pub public_url: String,
    /// Proxy rendered into instances that have a destination policy
    /// (`FLEETT_EGRESS_PROXY`). Empty or `off` renders no proxy variables.
    pub egress_proxy: String,
    /// `None` only when `FLEETT_ALLOW_NO_AUTH=1` or OIDC is the only mode.
    pub token: Option<String>,
    pub allow_no_auth: bool,
    /// `FLEETT_AUTH` (`bearer` by default).
    pub auth: AuthMode,
    /// `FLEETT_OIDC_*`, present when `FLEETT_AUTH` includes `oidc`.
    pub oidc: Option<OidcConfig>,
    pub driver: DriverKind,
    pub image: String,
    pub network: String,
    /// Optional fleet-wide caps. `0` (the default) disables the cap.
    pub max_total_cpu: f64,
    pub max_total_mem_mb: u64,
    /// Enforcement of the host egress firewall (`FLEETT_EGRESS`).
    pub egress: EgressMode,
    /// Marker written by `fleett egress install` (`FLEETT_EGRESS_MARKER`).
    pub egress_marker: PathBuf,
    /// Base64 AES-256 key (`FLEETT_SECRETS_KEY`), when set directly.
    pub secrets_key: Option<String>,
    /// Key file (`FLEETT_SECRETS_KEY_FILE`), read when no inline key is set.
    pub secrets_key_file: PathBuf,
    /// Delete imported `secrets.env` files at startup (`FLEETT_SECRETS_AUTOIMPORT`).
    pub secrets_autoimport: bool,
    /// Directory containing an on-disk frontend build, when one is present.
    pub web_dist: Option<PathBuf>,
    /// Security posture of the rendered agent containers (`FLEETT_HARDENING`).
    pub hardening: Hardening,
    /// Enforce `maintenance_windows` (`FLEETT_MAINTENANCE_ENFORCE`, default on).
    ///
    /// Enforcement only kicks in when at least one enabled window exists, so a
    /// fresh install is never locked out.
    pub maintenance_enforce: bool,
    /// Dedicated Prometheus listener (`FLEETT_METRICS_*`).
    pub metrics: MetricsConfig,
    /// Sampling interval of the watchdog, in seconds (`FLEETT_WATCHDOG_INTERVAL_S`,
    /// default 30). `0` disables the background loop; `fleett incidents` still
    /// reads the registry.
    pub watchdog_interval_s: u64,
    /// Seconds between two automatic usage-log scans (`FLEETT_COST_SCAN_INTERVAL_S`).
    /// `0` disables the background scanner; ingestion stays available through
    /// `POST /api/usage/ingest`.
    pub cost_scan_interval_s: u64,
    /// Raw `FLEETT_EXEC_ALLOWLIST` (`None` keeps the built-in allowlist).
    pub exec_allowlist: Option<String>,
    /// Retention window of the persistent log center (`FLEETT_LOG_RETENTION`).
    pub log_retention: std::time::Duration,
    /// Row cap of the log center; `0` disables the cap (`FLEETT_LOG_MAX_ROWS`).
    pub log_max_rows: u64,
    /// API hardening: rate limits and auth lockout (`FLEETT_RATE_LIMIT_*`,
    /// `FLEETT_AUTH_LOCKOUT_*`). See docs/HARDENING.md.
    pub security: SecurityConfig,
    /// Key signing `GET /api/audit/export` (`FLEETT_AUDIT_HMAC_KEY`), decoded.
    pub audit_hmac_key: Option<crate::audit::AuditKey>,
    /// Rows older than this are pruned (`FLEETT_AUDIT_RETENTION`, default 90d).
    pub audit_retention: chrono::Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid value for {var}: {value:?} ({detail})")]
    Invalid {
        var: &'static str,
        value: String,
        detail: String,
    },
    #[error(
        "FLEETT_TOKEN is required (set it in the environment or in $FLEETT_ROOT/fleett.env, \
         or set FLEETT_ALLOW_NO_AUTH=1 to explicitly disable authentication)"
    )]
    MissingToken,
    #[error("{var} is required when FLEETT_AUTH={mode}")]
    MissingOidcVar {
        var: &'static str,
        mode: &'static str,
    },
    #[error(
        "FLEETT_METRICS_TOKEN is required when FLEETT_METRICS_BIND ({bind}) is not a loopback \
         address: without it anyone who can reach the port reads the fleet layout"
    )]
    MetricsTokenRequired { bind: SocketAddr },
}

impl Config {
    /// Load the configuration from the real process environment plus the
    /// environment file found in the data root.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_process_env(|key| std::env::var(key).ok())
    }

    /// Same as [`Config::from_env`] without requiring a bearer token.
    ///
    /// Maintenance commands (`fleett migrate`) never serve HTTP, so they must
    /// stay usable when the token lives in a file the caller did not load.
    pub fn from_env_for_maintenance() -> Result<Self, ConfigError> {
        Self::from_process_env_inner(|key| std::env::var(key).ok(), false)
    }

    /// Same as [`Config::from_env`] with an injectable environment lookup.
    ///
    /// `FLEETT_ROOT` is resolved from the process environment first (or the
    /// home root); the environment file is then read from that root and fills
    /// in any variable the process environment does not define.
    pub fn from_process_env(process: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        Self::from_process_env_inner(process, true)
    }

    fn from_process_env_inner(
        process: impl Fn(&str) -> Option<String>,
        require_token: bool,
    ) -> Result<Self, ConfigError> {
        let explicit_root = resolve_var(&process, "ROOT").map(PathBuf::from);

        let env_file = match explicit_root {
            Some(ref root) => env_file_in(root),
            None => discover_env_file(&process),
        };
        let file_vars = load_env_file(&env_file);
        let file_lookup = |name: &str| file_vars.get(name).cloned();

        // Precedence is by name first, then by source: a canonical `FLEETT_`
        // in the file must beat a legacy `FLEET_` in the process environment.
        //   1. process FLEETT_   2. file FLEETT_
        //   3. process FLEET_    4. file FLEET_
        let lookup = |suffix: &str| {
            let canonical = format!("{PREFIX}{suffix}");
            let legacy = format!("{LEGACY_PREFIX}{suffix}");
            process(&canonical)
                .or_else(|| file_lookup(&canonical))
                .or_else(|| process(&legacy))
                .or_else(|| file_lookup(&legacy))
        };

        let default_root = explicit_root
            .or_else(|| home_root(&process))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_ROOT));

        Self::assemble(&lookup, default_root, require_token)
    }

    /// Resolve a configuration from a raw lookup function.
    ///
    /// The lookup receives full variable names, so a caller can inject
    /// `FLEETT_TOKEN` or the legacy `FLEET_TOKEN` freely.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        Self::assemble(
            &|suffix| resolve_var_raw(&lookup, suffix),
            PathBuf::from(DEFAULT_ROOT),
            true,
        )
    }

    fn assemble(
        get: &dyn Fn(&str) -> Option<String>,
        default_root: PathBuf,
        require_token: bool,
    ) -> Result<Self, ConfigError> {
        // `get` drops blank values (a variable set to nothing is "unset"); the
        // two variables where the empty string is meaningful read `get_raw`.
        let get_raw = |suffix: &str| get(suffix).map(|value| value.trim().to_string());
        let get = |suffix: &str| get_raw(suffix).filter(|value| !value.is_empty());

        let root = get("ROOT").map(PathBuf::from).unwrap_or(default_root);

        let bind_raw = get("BIND").unwrap_or_else(|| DEFAULT_BIND.to_string());
        let bind = bind_raw
            .parse::<SocketAddr>()
            .map_err(|err| ConfigError::Invalid {
                var: "FLEETT_BIND",
                value: bind_raw.clone(),
                detail: err.to_string(),
            })?;

        let auth = match get("AUTH") {
            None => AuthMode::Bearer,
            Some(value) => AuthMode::parse(&value).ok_or_else(|| ConfigError::Invalid {
                var: "FLEETT_AUTH",
                value: value.clone(),
                detail: "expected \"bearer\", \"oidc\" or \"both\"".to_string(),
            })?,
        };

        let allow_no_auth = matches!(
            get("ALLOW_NO_AUTH").as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "on")
        );
        // With OIDC the browser session is the credential; FLEETT_TOKEN is only
        // the optional bootstrap token and named tokens live in the database.
        let token = get("TOKEN");
        if auth == AuthMode::Bearer && token.is_none() && !allow_no_auth && require_token {
            return Err(ConfigError::MissingToken);
        }

        let oidc = if auth.oidc_enabled() {
            Some(resolve_oidc(&get, auth)?)
        } else {
            None
        };

        let driver = match get("DRIVER") {
            None => DriverKind::Mock,
            Some(value) => DriverKind::parse(&value).ok_or_else(|| ConfigError::Invalid {
                var: "FLEETT_DRIVER",
                value: value.clone(),
                detail: "expected \"mock\" or \"compose\"".to_string(),
            })?,
        };

        let image = get("IMAGE").unwrap_or_else(|| DEFAULT_IMAGE.to_string());
        let network = get("NETWORK").unwrap_or_else(|| DEFAULT_NETWORK.to_string());

        let max_total_cpu = match get("MAX_TOTAL_CPU") {
            None => 0.0,
            Some(value) => {
                let parsed = value.parse::<f64>().map_err(|err| ConfigError::Invalid {
                    var: "FLEETT_MAX_TOTAL_CPU",
                    value: value.clone(),
                    detail: err.to_string(),
                })?;
                if !parsed.is_finite() || parsed < 0.0 {
                    return Err(ConfigError::Invalid {
                        var: "FLEETT_MAX_TOTAL_CPU",
                        value,
                        detail: "expected a non-negative number of CPUs (0 disables the cap)"
                            .to_string(),
                    });
                }
                parsed
            }
        };
        let max_total_mem_mb = match get("MAX_TOTAL_MEM_MB") {
            None => 0,
            Some(value) => value.parse::<u64>().map_err(|err| ConfigError::Invalid {
                var: "FLEETT_MAX_TOTAL_MEM_MB",
                value: value.clone(),
                detail: err.to_string(),
            })?,
        };

        let web_dist = get("WEB_DIST").map(PathBuf::from).or_else(|| {
            ["web/dist", "../web/dist"]
                .iter()
                .map(PathBuf::from)
                .find(|dir| dir.join("index.html").is_file())
        });

        let egress = match get("EGRESS") {
            None => EgressMode::Restrict,
            Some(value) => EgressMode::parse(&value).ok_or_else(|| ConfigError::Invalid {
                var: "FLEETT_EGRESS",
                value: value.clone(),
                detail: "expected \"restrict\" or \"off\"".to_string(),
            })?,
        };
        let egress_marker = get("EGRESS_MARKER")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_EGRESS_MARKER));
        let egress_proxy =
            get_raw("EGRESS_PROXY").unwrap_or_else(|| DEFAULT_EGRESS_PROXY.to_string());

        let secrets_key = get("SECRETS_KEY");
        let secrets_key_file = get("SECRETS_KEY_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SECRETS_KEY_FILE));
        let secrets_autoimport = matches!(
            get("SECRETS_AUTOIMPORT").as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "on")
        );

        let hardening = match get("HARDENING") {
            None => Hardening::Standard,
            Some(value) => Hardening::parse(&value).ok_or_else(|| ConfigError::Invalid {
                var: "FLEETT_HARDENING",
                value: value.clone(),
                detail: "expected \"standard\" or \"strict\"".to_string(),
            })?,
        };

        let metrics = resolve_metrics(get_raw("METRICS_BIND"), get("METRICS_TOKEN"))?;
        let security = resolve_security(&get)?;
        let audit_hmac_key = match get("AUDIT_HMAC_KEY") {
            None => None,
            Some(value) => Some(crate::audit::AuditKey::new(
                crate::audit::parse_key(&value).map_err(|detail| ConfigError::Invalid {
                    var: "FLEETT_AUDIT_HMAC_KEY",
                    value: "<redacted>".to_string(),
                    detail,
                })?,
            )),
        };
        let audit_retention = match get("AUDIT_RETENTION") {
            None => crate::audit::parse_retention(crate::audit::DEFAULT_RETENTION)
                .expect("the default retention is valid"),
            Some(value) => {
                crate::audit::parse_retention(&value).map_err(|detail| ConfigError::Invalid {
                    var: "FLEETT_AUDIT_RETENTION",
                    value: value.clone(),
                    detail,
                })?
            }
        };
        let public_url = get("PUBLIC_URL").unwrap_or_else(|| DEFAULT_PUBLIC_URL.to_string());
        let exec_allowlist = get("EXEC_ALLOWLIST");

        let cost_scan_interval_s = match get("COST_SCAN_INTERVAL_S") {
            None => 300,
            Some(value) => value.parse::<u64>().map_err(|err| ConfigError::Invalid {
                var: "FLEETT_COST_SCAN_INTERVAL_S",
                value: value.clone(),
                detail: err.to_string(),
            })?,
        };

        let watchdog_interval_s = match get("WATCHDOG_INTERVAL_S") {
            None => DEFAULT_WATCHDOG_INTERVAL_S,
            Some(value) => value.parse::<u64>().map_err(|err| ConfigError::Invalid {
                var: "FLEETT_WATCHDOG_INTERVAL_S",
                value: value.clone(),
                detail: format!("{err} (0 disables the watchdog loop)"),
            })?,
        };

        // `FLEETT_MAINTENANCE_ENFORCE=0` disables the upkeep windows entirely;
        // any other value (or no value) keeps the default, which only blocks
        // once an enabled window exists.
        let maintenance_enforce = match get("MAINTENANCE_ENFORCE") {
            None => true,
            Some(value) => !matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            ),
        };

        let log_retention = match get("LOG_RETENTION") {
            None => parse_log_retention(DEFAULT_LOG_RETENTION).expect("default retention is valid"),
            Some(value) => parse_log_retention(&value).ok_or_else(|| ConfigError::Invalid {
                var: "FLEETT_LOG_RETENTION",
                value: value.clone(),
                detail: "expected a positive duration such as 7d, 24h or 1h30m".to_string(),
            })?,
        };
        let log_max_rows = match get("LOG_MAX_ROWS") {
            None => DEFAULT_LOG_MAX_ROWS,
            Some(value) => value.parse::<u64>().map_err(|err| ConfigError::Invalid {
                var: "FLEETT_LOG_MAX_ROWS",
                value: value.clone(),
                detail: err.to_string(),
            })?,
        };

        Ok(Self {
            root,
            bind,
            public_url,
            egress_proxy,
            token,
            allow_no_auth,
            auth,
            oidc,
            driver,
            image,
            network,
            max_total_cpu,
            max_total_mem_mb,
            egress,
            egress_marker,
            secrets_key,
            secrets_key_file,
            secrets_autoimport,
            web_dist,
            hardening,
            maintenance_enforce,
            metrics,
            cost_scan_interval_s,
            watchdog_interval_s,
            exec_allowlist,
            log_retention,
            log_max_rows,
            security,
            audit_hmac_key,
            audit_retention,
        })
    }

    /// Configuration used by the integration tests: mock driver, temp root.
    ///
    /// Egress enforcement is off by default here: tests that exercise the
    /// policy opt in with `cfg.egress = EgressMode::Restrict`, and the marker
    /// lives in the temporary root instead of `/etc/fleett`.
    pub fn for_tests(root: PathBuf) -> Self {
        let egress_marker = root.join("egress.json");
        // The key file lives next to the root, never inside it, exactly like
        // the production default (`/etc/fleett/secrets.key` vs `/opt/fleett`).
        let secrets_key_file = root.with_extension("secrets.key");
        Self {
            root,
            bind: "127.0.0.1:0"
                .parse()
                .expect("literal socket address is valid"),
            public_url: DEFAULT_PUBLIC_URL.to_string(),
            token: Some("test-token".to_string()),
            allow_no_auth: false,
            auth: AuthMode::Bearer,
            oidc: None,
            driver: DriverKind::Mock,
            image: DEFAULT_IMAGE.to_string(),
            network: DEFAULT_NETWORK.to_string(),
            max_total_cpu: 0.0,
            max_total_mem_mb: 0,
            egress: EgressMode::Off,
            egress_marker,
            egress_proxy: DEFAULT_EGRESS_PROXY.to_string(),
            secrets_key: None,
            secrets_key_file,
            secrets_autoimport: false,
            web_dist: None,
            hardening: Hardening::Standard,
            maintenance_enforce: true,
            // The listener is started by `serve` only: tests never bind a port.
            metrics: MetricsConfig::default(),
            // Tests drive `watchdog::tick` explicitly; no background loop runs.
            watchdog_interval_s: 0,
            // Tests ingest usage explicitly; no background scanner.
            cost_scan_interval_s: 0,
            exec_allowlist: None,
            log_retention: parse_log_retention(DEFAULT_LOG_RETENTION).expect("valid default"),
            log_max_rows: DEFAULT_LOG_MAX_ROWS,
            // Tests exercise the limiter explicitly; by default they are never
            // rate-limited.
            security: SecurityConfig {
                rate_limit_per_min: 0,
                rate_limit_auth_per_min: 0,
                rate_limit_heavy_per_min: 0,
                lockout_failures: 0,
                ..SecurityConfig::default()
            },
            audit_hmac_key: None,
            audit_retention: crate::audit::parse_retention(crate::audit::DEFAULT_RETENTION)
                .expect("valid default"),
        }
    }

    /// Name of the unrestricted network attached to `allow_lan` instances.
    pub fn lan_network(&self) -> String {
        format!("{}-lan", self.network)
    }

    pub fn hsts_enabled(&self) -> bool {
        self.public_url
            .trim()
            .to_ascii_lowercase()
            .starts_with("https://")
    }

    /// Proxy URL rendered into an instance with a destination policy.
    ///
    /// `None` when `FLEETT_EGRESS_PROXY` is empty, `off` or `none`: the policy
    /// is still stored, but no `HTTP(S)_PROXY` variable is written (an honest
    /// gap the API reports).
    pub fn egress_proxy_url(&self) -> Option<&str> {
        let value = self.egress_proxy.trim();
        if value.is_empty()
            || value.eq_ignore_ascii_case("off")
            || value.eq_ignore_ascii_case("none")
            || value.eq_ignore_ascii_case("disabled")
        {
            None
        } else {
            Some(value)
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("fleet.db")
    }

    /// Environment file in use: `fleett.env` first, then the legacy `fleet.env`.
    pub fn env_file_path(&self) -> PathBuf {
        env_file_in(&self.root)
    }

    pub fn instances_dir(&self) -> PathBuf {
        self.root.join("instances")
    }

    pub fn instance_dir(&self, id: &str) -> PathBuf {
        self.instances_dir().join(id)
    }

    pub fn compose_path(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("compose.yaml")
    }

    pub fn data_dir(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("data")
    }

    pub fn secrets_path(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("secrets.env")
    }

    pub fn shares_dir(&self) -> PathBuf {
        self.root.join("shares")
    }

    pub fn share_dir(&self, name: &str) -> PathBuf {
        self.shares_dir().join(name)
    }

    pub fn templates_dir(&self) -> PathBuf {
        self.root.join("templates")
    }

    pub fn template_dir(&self, id: &str) -> PathBuf {
        self.templates_dir().join(id)
    }

    pub fn template_config_path(&self, id: &str) -> PathBuf {
        self.template_dir(id).join("config.yaml")
    }
}

/// Resolve and validate `FLEETT_OIDC_*` when OIDC is enabled.
fn resolve_oidc(
    get: &dyn Fn(&str) -> Option<String>,
    mode: AuthMode,
) -> Result<OidcConfig, ConfigError> {
    fn required(
        get: &dyn Fn(&str) -> Option<String>,
        var: &'static str,
        suffix: &str,
        mode: AuthMode,
    ) -> Result<String, ConfigError> {
        get(suffix).ok_or(ConfigError::MissingOidcVar {
            var,
            mode: mode.as_str(),
        })
    }

    let public_url = get("PUBLIC_URL").unwrap_or_else(|| DEFAULT_PUBLIC_URL.to_string());
    let issuer = required(get, "FLEETT_OIDC_ISSUER", "OIDC_ISSUER", mode)?;
    let client_id = required(get, "FLEETT_OIDC_CLIENT_ID", "OIDC_CLIENT_ID", mode)?;
    let client_secret = required(get, "FLEETT_OIDC_CLIENT_SECRET", "OIDC_CLIENT_SECRET", mode)?;
    let redirect_url = get("OIDC_REDIRECT_URL")
        .unwrap_or_else(|| format!("{}/auth/callback", public_url.trim_end_matches('/')));
    let mut scopes = get("OIDC_SCOPES").unwrap_or_else(|| DEFAULT_OIDC_SCOPES.to_string());
    if !scopes.split_whitespace().any(|scope| scope == "openid") {
        scopes = format!("openid {scopes}");
    }
    let admin_group =
        get("OIDC_ADMIN_GROUP").unwrap_or_else(|| DEFAULT_OIDC_ADMIN_GROUP.to_string());
    let viewer_group =
        get("OIDC_VIEWER_GROUP").unwrap_or_else(|| DEFAULT_OIDC_VIEWER_GROUP.to_string());

    let session_secret_raw = required(get, "FLEETT_SESSION_SECRET", "SESSION_SECRET", mode)?;
    let session_secret =
        decode_session_secret(&session_secret_raw).ok_or_else(|| ConfigError::Invalid {
            var: "FLEETT_SESSION_SECRET",
            value: "<redacted>".to_string(),
            detail: format!(
                "expected base64 of at least {MIN_SESSION_SECRET_BYTES} bytes; generate one with \
                 `openssl rand -base64 32`"
            ),
        })?;

    let session_ttl_secs = match get("SESSION_TTL_SECS") {
        None => DEFAULT_SESSION_TTL_SECS,
        Some(value) => {
            let parsed = value.parse::<u64>().map_err(|err| ConfigError::Invalid {
                var: "FLEETT_SESSION_TTL_SECS",
                value: value.clone(),
                detail: err.to_string(),
            })?;
            if parsed == 0 {
                return Err(ConfigError::Invalid {
                    var: "FLEETT_SESSION_TTL_SECS",
                    value,
                    detail: "expected a positive number of seconds".to_string(),
                });
            }
            parsed
        }
    };

    let state_ttl_secs = match get("OIDC_STATE_TTL_SECS") {
        None => DEFAULT_OIDC_STATE_TTL_SECS,
        Some(value) => {
            let parsed = value.parse::<u64>().map_err(|err| ConfigError::Invalid {
                var: "FLEETT_OIDC_STATE_TTL_SECS",
                value: value.clone(),
                detail: err.to_string(),
            })?;
            if !(MIN_OIDC_STATE_TTL_SECS..=MAX_OIDC_STATE_TTL_SECS).contains(&parsed) {
                return Err(ConfigError::Invalid {
                    var: "FLEETT_OIDC_STATE_TTL_SECS",
                    value,
                    detail: format!(
                        "expected a number of seconds between {MIN_OIDC_STATE_TTL_SECS} \
                         and {MAX_OIDC_STATE_TTL_SECS}"
                    ),
                });
            }
            parsed
        }
    };

    let cookie_secure = match get("COOKIE_SECURE") {
        None => CookieSecure::Auto,
        Some(value) => CookieSecure::parse(&value).ok_or_else(|| ConfigError::Invalid {
            var: "FLEETT_COOKIE_SECURE",
            value: value.clone(),
            detail: "expected \"auto\", \"true\" or \"false\"".to_string(),
        })?,
    };

    Ok(OidcConfig {
        cookie_secure: cookie_secure.resolve(&public_url),
        public_url,
        settings: OidcSettings {
            issuer,
            client_id,
            client_secret,
            redirect_url,
            scopes,
            admin_group,
            viewer_group,
        },
        session_secret,
        session_ttl_secs,
        state_ttl_secs,
    })
}

/// Decode `FLEETT_SESSION_SECRET`, accepting standard or URL-safe base64.
fn decode_session_secret(value: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    use base64::Engine;

    let bytes = STANDARD
        .decode(value)
        .or_else(|_| STANDARD_NO_PAD.decode(value))
        .or_else(|_| URL_SAFE.decode(value))
        .or_else(|_| URL_SAFE_NO_PAD.decode(value))
        .ok()?;
    (bytes.len() >= MIN_SESSION_SECRET_BYTES).then_some(bytes)
}

/// Resolve `FLEETT_METRICS_*` and enforce the listen-address policy.
///
/// The listener is on by default (`127.0.0.1:9100`, no token needed). An empty
/// value — or `off` — disables it; any other value is parsed as a socket
/// address, and a non-loopback bind then requires a token.
fn resolve_metrics(
    bind: Option<String>,
    token: Option<String>,
) -> Result<MetricsConfig, ConfigError> {
    let bind = match bind.as_deref().map(str::trim) {
        None => Some(
            DEFAULT_METRICS_BIND
                .parse::<SocketAddr>()
                .expect("the default metrics bind is a valid socket address"),
        ),
        Some(value)
            if value.is_empty()
                || matches!(
                    value.to_ascii_lowercase().as_str(),
                    "off" | "none" | "disabled" | "false"
                ) =>
        {
            None
        }
        Some(value) => Some(
            value
                .parse::<SocketAddr>()
                .map_err(|err| ConfigError::Invalid {
                    var: "FLEETT_METRICS_BIND",
                    value: value.to_string(),
                    detail: err.to_string(),
                })?,
        ),
    };

    if let Some(addr) = bind {
        if !addr.ip().is_loopback() && token.is_none() {
            return Err(ConfigError::MetricsTokenRequired { bind: addr });
        }
    }

    Ok(MetricsConfig { bind, token })
}

/// Parse a log retention window.
///
/// Accepts one or more `<number><unit>` groups (`7d`, `24h`, `1h30m`) or a bare
/// number of seconds. Units are `s`, `m`, `h`, `d` and `w`; the result must be
/// strictly positive.
pub fn parse_log_retention(value: &str) -> Option<std::time::Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return (seconds > 0).then(|| std::time::Duration::from_secs(seconds));
    }
    let mut rest = value;
    let mut total = 0u64;
    while !rest.is_empty() {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let (number, tail) = rest.split_at(digits);
        let number: u64 = number.parse().ok()?;
        let unit = tail.chars().next()?;
        let unit_seconds = match unit {
            's' => 1,
            'm' => 60,
            'h' => 60 * 60,
            'd' => 24 * 60 * 60,
            'w' => 7 * 24 * 60 * 60,
            _ => return None,
        };
        total = total.checked_add(number.checked_mul(unit_seconds)?)?;
        rest = &tail[unit.len_utf8()..];
    }
    (total > 0).then(|| std::time::Duration::from_secs(total))
}

/// Resolve one configuration variable: `FLEETT_<suffix>` then `FLEET_<suffix>`.
fn resolve_var(lookup: &dyn Fn(&str) -> Option<String>, suffix: &str) -> Option<String> {
    resolve_var_raw(lookup, suffix).filter(|value| !value.is_empty())
}

/// Same as [`resolve_var`] keeping a blank value.
///
/// `FLEETT_METRICS_BIND=` (an empty value) is the documented way to disable the
/// metrics listener, so the resolver must be able to see it.
fn resolve_var_raw(lookup: &dyn Fn(&str) -> Option<String>, suffix: &str) -> Option<String> {
    [
        format!("{PREFIX}{suffix}"),
        format!("{LEGACY_PREFIX}{suffix}"),
    ]
    .iter()
    .find_map(|name| lookup(name))
    .map(|value| value.trim().to_string())
}

/// Existing environment file in `root`: `fleett.env` wins over `fleet.env`.
fn env_file_in(root: &Path) -> PathBuf {
    let canonical = root.join(ENV_FILE_NAME);
    if canonical.is_file() {
        return canonical;
    }
    let legacy = root.join(LEGACY_ENV_FILE_NAME);
    if legacy.is_file() {
        return legacy;
    }
    canonical
}

/// Environment file to read when no root was given explicitly.
///
/// Mirrors [`home_root`]: the home root wins, the legacy home root is adopted
/// only when `~/.fleett` is absent, and `/opt/fleett` is used without `HOME`.
fn discover_env_file(process: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(home) = non_empty(process("HOME")) {
        let home = PathBuf::from(home);
        let canonical = home.join(HOME_ROOT_NAME);
        if canonical.is_dir() {
            return env_file_in(&canonical);
        }
        let legacy = home.join(LEGACY_HOME_ROOT_NAME);
        if legacy.is_dir() {
            return env_file_in(&legacy);
        }
        return canonical.join(ENV_FILE_NAME);
    }
    env_file_in(Path::new(DEFAULT_ROOT))
}

/// Data root used when neither the environment nor the file defines one.
fn home_root(process: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let home = PathBuf::from(non_empty(process("HOME"))?);
    let canonical = home.join(HOME_ROOT_NAME);
    if canonical.is_dir() {
        return Some(canonical);
    }
    let legacy = home.join(LEGACY_HOME_ROOT_NAME);
    if legacy.is_dir() {
        tracing::warn!(
            root = %legacy.display(),
            "using the legacy data root; rename it to ~/.fleett when convenient"
        );
        return Some(legacy);
    }
    Some(canonical)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Parse a `KEY=VALUE` environment file.
///
/// Blank lines and lines starting with `#` are ignored, keys must look like
/// environment variable names, and a matching pair of surrounding quotes is
/// removed. Nothing is expanded or executed.
pub fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !is_env_key(key) {
            continue;
        }
        vars.insert(key.to_string(), unquote(value.trim()));
    }
    vars
}

/// Read `path` as an environment file. Missing or unreadable files yield an
/// empty map: the control plane must still start on a fresh host.
pub fn load_env_file(path: &Path) -> BTreeMap<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_env_file(&text),
        Err(_) => BTreeMap::new(),
    }
}

fn is_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() < 2 {
        return value.to_string();
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if !((first == b'"' && last == b'"') || (first == b'\'' && last == b'\'')) {
        return value.to_string();
    }
    let inner = &value[1..value.len() - 1];
    if first == b'\'' {
        return inner.to_string();
    }
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn log_retention_windows_are_parsed() {
        assert_eq!(
            parse_log_retention("7d"),
            Some(std::time::Duration::from_secs(7 * 24 * 3600))
        );
        assert_eq!(
            parse_log_retention("24h"),
            Some(std::time::Duration::from_secs(24 * 3600))
        );
        assert_eq!(
            parse_log_retention("1h30m"),
            Some(std::time::Duration::from_secs(90 * 60))
        );
        assert_eq!(
            parse_log_retention("3600"),
            Some(std::time::Duration::from_secs(3600))
        );
        for invalid in ["", "0", "0d", "d", "1x", "-1h", "1.5h"] {
            assert_eq!(parse_log_retention(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn parses_env_file_and_ignores_comments() {
        let text = "\
# configuration
FLEETT_BIND=127.0.0.1:9999

FLEETT_IMAGE = \"example/agent:1.0\"\x20
not a variable
9BAD=value
FLEETT_NETWORK='fleett-net'
";
        let vars = parse_env_file(text);
        assert_eq!(
            vars.get("FLEETT_BIND").map(String::as_str),
            Some("127.0.0.1:9999")
        );
        assert_eq!(
            vars.get("FLEETT_IMAGE").map(String::as_str),
            Some("example/agent:1.0")
        );
        assert_eq!(
            vars.get("FLEETT_NETWORK").map(String::as_str),
            Some("fleett-net")
        );
        assert!(!vars.contains_key("9BAD"));
        assert_eq!(vars.len(), 3);
    }

    #[test]
    fn fleett_env_file_fills_missing_variables() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join(ENV_FILE_NAME),
            "# generated by the installer\n\
             FLEETT_BIND=127.0.0.1:9999\n\
             FLEETT_IMAGE=from-file:1\n\
             FLEETT_TOKEN=file-token\n",
        )
        .expect("write fleett.env");

        let root = dir.path().to_string_lossy().to_string();
        let cfg = Config::from_process_env(|key| match key {
            "FLEETT_ROOT" => Some(root.clone()),
            "FLEETT_TOKEN" => Some("process-token".to_string()),
            _ => None,
        })
        .expect("config resolves");

        // Process environment wins over the file.
        assert_eq!(cfg.token.as_deref(), Some("process-token"));
        // Missing variables are filled from the file.
        assert_eq!(cfg.image, "from-file:1");
        assert_eq!(cfg.bind.port(), 9999);
        assert_eq!(cfg.network, DEFAULT_NETWORK);
        assert_eq!(cfg.driver, DriverKind::Mock);
    }

    #[test]
    fn legacy_fleet_prefix_is_still_accepted() {
        let cfg = Config::from_process_env(|key| match key {
            "FLEET_ROOT" => Some("/tmp/legacy-root".to_string()),
            "FLEET_TOKEN" => Some("legacy-token".to_string()),
            "FLEET_DRIVER" => Some("compose".to_string()),
            _ => None,
        })
        .expect("legacy variables resolve");
        assert_eq!(cfg.token.as_deref(), Some("legacy-token"));
        assert_eq!(cfg.driver, DriverKind::Compose);
        assert_eq!(cfg.root, PathBuf::from("/tmp/legacy-root"));
    }

    #[test]
    fn fleett_prefix_wins_over_the_legacy_prefix() {
        let cfg = Config::from_process_env(|key| match key {
            "FLEETT_TOKEN" => Some("canonical".to_string()),
            "FLEET_TOKEN" => Some("legacy".to_string()),
            "FLEETT_BIND" => Some("127.0.0.1:1234".to_string()),
            "FLEET_BIND" => Some("127.0.0.1:4321".to_string()),
            _ => None,
        })
        .expect("config resolves");
        assert_eq!(cfg.token.as_deref(), Some("canonical"));
        assert_eq!(cfg.bind.port(), 1234);
    }

    #[test]
    fn fleett_env_wins_over_the_legacy_env_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join(LEGACY_ENV_FILE_NAME),
            "FLEETT_TOKEN=legacy-file\nFLEETT_IMAGE=legacy:1\n",
        )
        .expect("write fleet.env");
        std::fs::write(
            dir.path().join(ENV_FILE_NAME),
            "FLEETT_TOKEN=canonical-file\n",
        )
        .expect("write fleett.env");

        let root = dir.path().to_string_lossy().to_string();
        let cfg = Config::from_process_env(|key| match key {
            "FLEETT_ROOT" => Some(root.clone()),
            _ => None,
        })
        .expect("config resolves");
        assert_eq!(cfg.token.as_deref(), Some("canonical-file"));
        // `fleet.env` is not read at all when `fleett.env` exists.
        assert_eq!(cfg.image, DEFAULT_IMAGE);
    }

    #[test]
    fn home_fleett_env_is_used_without_fleett_root() {
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().join(HOME_ROOT_NAME);
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(
            root.join(ENV_FILE_NAME),
            format!(
                "FLEETT_ROOT={}\nFLEETT_TOKEN=home-token\n",
                root.to_string_lossy()
            ),
        )
        .expect("write home fleett.env");

        let home_str = home.path().to_string_lossy().to_string();
        let cfg = Config::from_process_env(|key| match key {
            "HOME" => Some(home_str.clone()),
            _ => None,
        })
        .expect("config resolves from the home file");

        assert_eq!(cfg.token.as_deref(), Some("home-token"));
        assert_eq!(cfg.root, root);
    }

    #[test]
    fn the_legacy_home_root_is_adopted_when_fleett_is_absent() {
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().join(LEGACY_HOME_ROOT_NAME);
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(
            root.join(LEGACY_ENV_FILE_NAME),
            "FLEET_TOKEN=legacy-home-token\n",
        )
        .expect("write legacy fleet.env");

        let home_str = home.path().to_string_lossy().to_string();
        let cfg = Config::from_process_env(|key| match key {
            "HOME" => Some(home_str.clone()),
            _ => None,
        })
        .expect("config resolves from the legacy home file");

        assert_eq!(cfg.token.as_deref(), Some("legacy-home-token"));
        assert_eq!(cfg.root, root);
        assert_eq!(cfg.env_file_path(), root.join(LEGACY_ENV_FILE_NAME));
    }

    #[test]
    fn an_existing_fleett_root_wins_over_the_legacy_one() {
        let home = tempfile::tempdir().expect("temp home");
        let canonical = home.path().join(HOME_ROOT_NAME);
        let legacy = home.path().join(LEGACY_HOME_ROOT_NAME);
        std::fs::create_dir_all(&canonical).expect("create root");
        std::fs::create_dir_all(&legacy).expect("create legacy root");

        let home_str = home.path().to_string_lossy().to_string();
        let cfg = Config::from_process_env(|key| match key {
            "HOME" => Some(home_str.clone()),
            "FLEETT_ALLOW_NO_AUTH" => Some("1".to_string()),
            _ => None,
        })
        .expect("config resolves without a token");
        assert_eq!(cfg.root, canonical);
    }

    #[test]
    fn an_existing_fleett_root_hides_the_legacy_env_file() {
        let home = tempfile::tempdir().expect("temp home");
        let canonical = home.path().join(HOME_ROOT_NAME);
        let legacy = home.path().join(LEGACY_HOME_ROOT_NAME);
        std::fs::create_dir_all(&canonical).expect("create root");
        std::fs::create_dir_all(&legacy).expect("create legacy root");
        std::fs::write(legacy.join(LEGACY_ENV_FILE_NAME), "FLEETT_TOKEN=legacy\n")
            .expect("write legacy env");

        let home_str = home.path().to_string_lossy().to_string();
        let env_file = discover_env_file(&|key: &str| (key == "HOME").then(|| home_str.clone()));
        // The canonical root exists, so the legacy environment file is not read.
        assert_eq!(env_file, canonical.join(ENV_FILE_NAME));
    }

    #[test]
    fn canonical_name_in_file_beats_legacy_name_in_process() {
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().join(".fleett");
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(
            root.join(ENV_FILE_NAME),
            "FLEETT_BIND=127.0.0.1:18095\nFLEETT_TOKEN=new-token\n",
        )
        .expect("write fleett.env");

        let home_str = home.path().to_string_lossy().to_string();
        let cfg = Config::from_process_env(|key| match key {
            "HOME" => Some(home_str.clone()),
            "FLEET_BIND" => Some("127.0.0.1:18093".to_string()),
            "FLEET_TOKEN" => Some("old-token".to_string()),
            _ => None,
        })
        .expect("config resolves");

        assert_eq!(cfg.bind.port(), 18095);
        assert_eq!(cfg.token.as_deref(), Some("new-token"));
    }

    #[test]
    fn legacy_process_values_are_used_when_canonical_is_absent() {
        let home = tempfile::tempdir().expect("temp home");
        let home_str = home.path().to_string_lossy().to_string();
        let cfg = Config::from_process_env(|key| match key {
            "HOME" => Some(home_str.clone()),
            "FLEET_BIND" => Some("127.0.0.1:18093".to_string()),
            "FLEET_TOKEN" => Some("old-token".to_string()),
            _ => None,
        })
        .expect("config resolves");

        assert_eq!(cfg.bind.port(), 18093);
        assert_eq!(cfg.token.as_deref(), Some("old-token"));
    }

    #[test]
    fn missing_token_is_rejected_unless_auth_is_disabled() {
        let err = Config::from_lookup(|_| None).expect_err("token required");
        assert!(matches!(err, ConfigError::MissingToken));
        assert!(err.to_string().contains("FLEETT_TOKEN"));

        let cfg = Config::from_lookup(|key| match key {
            "FLEETT_ALLOW_NO_AUTH" => Some("1".to_string()),
            _ => None,
        })
        .expect("no-auth mode resolves");
        assert!(cfg.token.is_none());
        assert!(cfg.allow_no_auth);
    }

    #[test]
    fn the_maintenance_configuration_does_not_require_a_token() {
        let lookup = |name: &str| (name == "FLEETT_ROOT").then(|| "/tmp/maintenance".to_string());
        let cfg = Config::assemble(
            &|suffix| resolve_var(&lookup, suffix),
            PathBuf::from(DEFAULT_ROOT),
            false,
        )
        .expect("maintenance config resolves without a token");
        assert!(cfg.token.is_none());
        assert!(!cfg.allow_no_auth);
        assert_eq!(cfg.root, PathBuf::from("/tmp/maintenance"));
    }

    #[test]
    fn total_caps_are_optional_and_validated() {
        let cfg = Config::from_lookup(|key| match key {
            "FLEETT_TOKEN" => Some("t".to_string()),
            "FLEETT_MAX_TOTAL_CPU" => Some("4.5".to_string()),
            "FLEETT_MAX_TOTAL_MEM_MB" => Some("6144".to_string()),
            _ => None,
        })
        .expect("caps resolve");
        assert_eq!(cfg.max_total_cpu, 4.5);
        assert_eq!(cfg.max_total_mem_mb, 6144);

        let default = Config::from_lookup(|key| (key == "FLEETT_TOKEN").then(|| "t".to_string()))
            .expect("defaults resolve");
        assert_eq!(default.max_total_cpu, 0.0);
        assert_eq!(default.max_total_mem_mb, 0);

        for (var, value) in [
            ("FLEETT_MAX_TOTAL_CPU", "-1"),
            ("FLEETT_MAX_TOTAL_CPU", "lots"),
            ("FLEETT_MAX_TOTAL_MEM_MB", "-5"),
            ("FLEETT_MAX_TOTAL_MEM_MB", "1.5"),
        ] {
            let err = Config::from_lookup(|key| match key {
                "FLEETT_TOKEN" => Some("t".to_string()),
                other if other == var => Some(value.to_string()),
                _ => None,
            })
            .expect_err("invalid cap");
            assert!(
                matches!(err, ConfigError::Invalid { var: reported, .. } if reported == var),
                "unexpected error for {var}={value}: {err}"
            );
        }
    }

    #[test]
    fn egress_mode_defaults_to_restrict_and_is_validated() {
        let cfg = Config::from_lookup(|key| (key == "FLEETT_TOKEN").then(|| "t".to_string()))
            .expect("defaults resolve");
        assert_eq!(cfg.egress, EgressMode::Restrict);
        assert_eq!(cfg.egress_marker, PathBuf::from(DEFAULT_EGRESS_MARKER));
        assert_eq!(cfg.egress_proxy_url(), Some(DEFAULT_EGRESS_PROXY));
        assert_eq!(cfg.lan_network(), "fleett-lan");

        for value in ["off", "OFF", "disabled"] {
            let cfg = Config::from_lookup(|key| match key {
                "FLEETT_TOKEN" => Some("t".to_string()),
                "FLEETT_EGRESS" => Some(value.to_string()),
                _ => None,
            })
            .expect("off resolves");
            assert_eq!(cfg.egress, EgressMode::Off, "{value}");
        }

        let cfg = Config::from_lookup(|key| match key {
            "FLEETT_TOKEN" => Some("t".to_string()),
            "FLEETT_EGRESS" => Some("restrict".to_string()),
            "FLEETT_EGRESS_MARKER" => Some("/tmp/marker.json".to_string()),
            _ => None,
        })
        .expect("restrict resolves");
        assert_eq!(cfg.egress, EgressMode::Restrict);
        assert_eq!(cfg.egress_marker, PathBuf::from("/tmp/marker.json"));

        let err = Config::from_lookup(|key| match key {
            "FLEETT_TOKEN" => Some("t".to_string()),
            "FLEETT_EGRESS" => Some("sometimes".to_string()),
            _ => None,
        })
        .expect_err("invalid egress mode");
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "FLEETT_EGRESS",
                ..
            }
        ));
    }

    #[test]
    fn bearer_is_the_default_auth_mode() {
        let cfg = Config::from_lookup(|key| (key == "FLEETT_TOKEN").then(|| "t".to_string()))
            .expect("defaults resolve");
        assert_eq!(cfg.auth, AuthMode::Bearer);
        assert!(cfg.oidc.is_none());
        assert!(cfg.auth.bearer_enabled());
        assert!(!cfg.auth.oidc_enabled());
    }

    #[test]
    fn oidc_mode_resolves_its_defaults() {
        let secret = base64::engine::general_purpose::STANDARD.encode([9u8; 32]);
        let cfg = Config::from_lookup(|key| match key {
            "FLEETT_AUTH" => Some("oidc".to_string()),
            "FLEETT_OIDC_ISSUER" => Some("https://sso.example.test/application/o/fleett/".into()),
            "FLEETT_OIDC_CLIENT_ID" => Some("client".into()),
            "FLEETT_OIDC_CLIENT_SECRET" => Some("secret".into()),
            "FLEETT_SESSION_SECRET" => Some(secret.clone()),
            "FLEETT_PUBLIC_URL" => Some("https://fleett.example.test".into()),
            _ => None,
        })
        .expect("oidc config resolves");

        assert_eq!(cfg.auth, AuthMode::Oidc);
        // No bearer token is required in oidc mode.
        assert!(cfg.token.is_none());
        let oidc = cfg.oidc.expect("oidc enabled");
        assert_eq!(oidc.public_url, "https://fleett.example.test");
        assert_eq!(
            oidc.settings.redirect_url,
            "https://fleett.example.test/auth/callback"
        );
        assert_eq!(oidc.settings.scopes, DEFAULT_OIDC_SCOPES);
        assert_eq!(oidc.settings.admin_group, DEFAULT_OIDC_ADMIN_GROUP);
        assert_eq!(oidc.settings.viewer_group, DEFAULT_OIDC_VIEWER_GROUP);
        assert_eq!(oidc.session_secret, vec![9u8; 32]);
        assert_eq!(oidc.session_ttl_secs, DEFAULT_SESSION_TTL_SECS);
        assert_eq!(oidc.state_ttl_secs, DEFAULT_OIDC_STATE_TTL_SECS);
        assert!(oidc.cookie_secure, "auto follows the https public url");
    }

    #[test]
    fn the_oidc_state_ttl_is_configurable_and_bounded() {
        let secret = base64::engine::general_purpose::STANDARD.encode([6u8; 32]);
        let with_ttl = |ttl: &str| {
            let secret = secret.clone();
            let ttl = ttl.to_string();
            Config::from_lookup(move |key| match key {
                "FLEETT_AUTH" => Some("oidc".to_string()),
                "FLEETT_OIDC_ISSUER" => Some("https://sso.example.test".into()),
                "FLEETT_OIDC_CLIENT_ID" => Some("client".into()),
                "FLEETT_OIDC_CLIENT_SECRET" => Some("secret".into()),
                "FLEETT_SESSION_SECRET" => Some(secret.clone()),
                "FLEETT_OIDC_STATE_TTL_SECS" => Some(ttl.clone()),
                _ => None,
            })
        };

        for value in [MIN_OIDC_STATE_TTL_SECS, 300, 900, MAX_OIDC_STATE_TTL_SECS] {
            let cfg = with_ttl(&value.to_string()).expect("valid state ttl");
            assert_eq!(cfg.oidc.expect("oidc").state_ttl_secs, value);
        }

        for value in ["0", "59", "3601", "abc", "-1"] {
            let err = with_ttl(value).expect_err("invalid state ttl");
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "FLEETT_OIDC_STATE_TTL_SECS",
                        ..
                    }
                ),
                "unexpected error for {value}: {err}"
            );
        }
    }

    #[test]
    fn oidc_cookies_are_insecure_on_plain_http_unless_forced() {
        let secret = base64::engine::general_purpose::STANDARD.encode([1u8; 32]);
        let base = |extra: (&str, &str)| {
            let secret = secret.clone();
            let (extra_var, extra_value) = (extra.0.to_string(), extra.1.to_string());
            move |key: &str| match key {
                "FLEETT_AUTH" => Some("oidc".to_string()),
                "FLEETT_OIDC_ISSUER" => Some("https://sso.example.test".into()),
                "FLEETT_OIDC_CLIENT_ID" => Some("client".into()),
                "FLEETT_OIDC_CLIENT_SECRET" => Some("secret".into()),
                "FLEETT_SESSION_SECRET" => Some(secret.clone()),
                other if other == extra_var => Some(extra_value.clone()),
                _ => None,
            }
        };

        let cfg = Config::from_lookup(base(("FLEETT_PUBLIC_URL", "http://127.0.0.1:8090")))
            .expect("http config");
        assert!(!cfg.oidc.expect("oidc").cookie_secure);

        let cfg = Config::from_lookup(base(("FLEETT_COOKIE_SECURE", "true"))).expect("forced");
        assert!(cfg.oidc.expect("oidc").cookie_secure);

        let cfg = Config::from_lookup(base(("FLEETT_PUBLIC_URL", "https://x.test"))).expect("auto");
        assert!(cfg.oidc.expect("oidc").cookie_secure);
    }

    #[test]
    fn the_oidc_debug_representation_redacts_secrets() {
        let secret = base64::engine::general_purpose::STANDARD.encode([5u8; 32]);
        let cfg = Config::from_lookup(|key| match key {
            "FLEETT_AUTH" => Some("oidc".to_string()),
            "FLEETT_OIDC_ISSUER" => Some("https://sso.example.test".into()),
            "FLEETT_OIDC_CLIENT_ID" => Some("client".into()),
            "FLEETT_OIDC_CLIENT_SECRET" => Some("super-client-secret".into()),
            "FLEETT_SESSION_SECRET" => Some(secret.clone()),
            _ => None,
        })
        .expect("oidc config resolves");
        let debug = format!("{:?}", cfg);
        assert!(!debug.contains("super-client-secret"), "{debug}");
        assert!(!debug.contains(&secret), "{debug}");
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn oidc_requires_its_variables() {
        for var in [
            "FLEETT_OIDC_ISSUER",
            "FLEETT_OIDC_CLIENT_ID",
            "FLEETT_OIDC_CLIENT_SECRET",
            "FLEETT_SESSION_SECRET",
        ] {
            let secret = base64::engine::general_purpose::STANDARD.encode([3u8; 32]);
            let err = Config::from_lookup(|key| {
                let provided = match key {
                    "FLEETT_AUTH" => Some("both".to_string()),
                    "FLEETT_OIDC_ISSUER" => Some("https://sso.example.test".to_string()),
                    "FLEETT_OIDC_CLIENT_ID" => Some("client".to_string()),
                    "FLEETT_OIDC_CLIENT_SECRET" => Some("secret".to_string()),
                    "FLEETT_SESSION_SECRET" => Some(secret.clone()),
                    _ => None,
                };
                provided.filter(|_| key != var)
            })
            .expect_err("missing variable must fail");
            assert!(
                matches!(
                    err,
                    ConfigError::MissingOidcVar { var: reported, .. } if reported == var
                ),
                "unexpected error for {var}: {err}"
            );
        }
    }

    #[test]
    fn a_short_session_secret_is_rejected() {
        let err = Config::from_lookup(|key| match key {
            "FLEETT_AUTH" => Some("oidc".to_string()),
            "FLEETT_OIDC_ISSUER" => Some("https://sso.example.test".into()),
            "FLEETT_OIDC_CLIENT_ID" => Some("client".into()),
            "FLEETT_OIDC_CLIENT_SECRET" => Some("secret".into()),
            "FLEETT_SESSION_SECRET" => Some("c2hvcnQ=".into()),
            _ => None,
        })
        .expect_err("short secret");
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "FLEETT_SESSION_SECRET",
                ..
            }
        ));
    }

    #[test]
    fn invalid_auth_mode_and_ttl_are_rejected() {
        let err = Config::from_lookup(|key| match key {
            "FLEETT_TOKEN" => Some("t".to_string()),
            "FLEETT_AUTH" => Some("saml".to_string()),
            _ => None,
        })
        .expect_err("bad auth mode");
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "FLEETT_AUTH",
                ..
            }
        ));

        let secret = base64::engine::general_purpose::STANDARD.encode([4u8; 32]);
        let err = Config::from_lookup(|key| match key {
            "FLEETT_AUTH" => Some("oidc".to_string()),
            "FLEETT_OIDC_ISSUER" => Some("https://sso.example.test".into()),
            "FLEETT_OIDC_CLIENT_ID" => Some("client".into()),
            "FLEETT_OIDC_CLIENT_SECRET" => Some("secret".into()),
            "FLEETT_SESSION_SECRET" => Some(secret.clone()),
            "FLEETT_SESSION_TTL_SECS" => Some("0".into()),
            _ => None,
        })
        .expect_err("zero ttl");
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "FLEETT_SESSION_TTL_SECS",
                ..
            }
        ));
    }

    #[test]
    fn rejects_invalid_driver_and_bind() {
        let err = Config::from_lookup(|key| match key {
            "FLEETT_TOKEN" => Some("t".to_string()),
            "FLEETT_DRIVER" => Some("swarm".to_string()),
            _ => None,
        })
        .expect_err("bad driver");
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "FLEETT_DRIVER",
                ..
            }
        ));

        let err = Config::from_lookup(|key| match key {
            "FLEETT_TOKEN" => Some("t".to_string()),
            "FLEETT_BIND" => Some("nope".to_string()),
            _ => None,
        })
        .expect_err("bad bind");
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "FLEETT_BIND",
                ..
            }
        ));
    }

    #[test]
    fn the_hardening_posture_defaults_to_standard_and_is_validated() {
        let cfg = Config::from_lookup(|key| (key == "FLEETT_TOKEN").then(|| "t".to_string()))
            .expect("defaults resolve");
        assert_eq!(cfg.hardening, Hardening::Standard);

        for value in ["standard", "DEFAULT", " strict ", "hardened"] {
            let cfg = Config::from_lookup(|key| match key {
                "FLEETT_TOKEN" => Some("t".to_string()),
                "FLEETT_HARDENING" => Some(value.to_string()),
                _ => None,
            })
            .expect("hardening resolves");
            let expected = if value.trim().eq_ignore_ascii_case("strict")
                || value.trim().eq_ignore_ascii_case("hardened")
            {
                Hardening::Strict
            } else {
                Hardening::Standard
            };
            assert_eq!(cfg.hardening, expected, "{value:?}");
        }

        let err = Config::from_lookup(|key| match key {
            "FLEETT_TOKEN" => Some("t".to_string()),
            "FLEETT_HARDENING" => Some("paranoid".to_string()),
            _ => None,
        })
        .expect_err("invalid hardening");
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "FLEETT_HARDENING",
                ..
            }
        ));
    }

    #[test]
    fn the_metrics_listener_defaults_to_loopback_without_a_token() {
        let cfg = Config::from_lookup(|key| (key == "FLEETT_TOKEN").then(|| "t".to_string()))
            .expect("defaults resolve");
        assert_eq!(
            cfg.metrics.bind,
            Some("127.0.0.1:9100".parse().expect("valid address"))
        );
        assert!(cfg.metrics.token.is_none());
        assert!(!cfg.metrics.requires_token());
    }

    #[test]
    fn an_empty_metrics_bind_disables_the_listener() {
        for value in ["", "  ", "off", "none", "disabled", "false"] {
            let cfg = Config::from_lookup(|key| match key {
                "FLEETT_TOKEN" => Some("t".to_string()),
                "FLEETT_METRICS_BIND" => Some(value.to_string()),
                _ => None,
            })
            .expect("disabled resolves");
            assert_eq!(
                cfg.metrics.bind, None,
                "{value:?} must disable the listener"
            );
        }
    }

    #[test]
    fn a_public_metrics_bind_requires_a_token() {
        let public = |token: Option<&'static str>| {
            Config::from_lookup(move |key| match key {
                "FLEETT_TOKEN" => Some("t".to_string()),
                "FLEETT_METRICS_BIND" => Some("0.0.0.0:9100".to_string()),
                "FLEETT_METRICS_TOKEN" => token.map(str::to_string),
                _ => None,
            })
        };

        let err = public(None).expect_err("a public bind needs a token");
        assert!(
            matches!(err, ConfigError::MetricsTokenRequired { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("FLEETT_METRICS_TOKEN"), "{err}");

        let cfg = public(Some("scrape-me")).expect("public bind with a token");
        assert_eq!(
            cfg.metrics.bind,
            Some("0.0.0.0:9100".parse().expect("valid address"))
        );
        assert!(cfg.metrics.requires_token());

        // A loopback bind never needs a token, and the token is accepted anyway.
        let cfg = Config::from_lookup(|key| match key {
            "FLEETT_TOKEN" => Some("t".to_string()),
            "FLEETT_METRICS_BIND" => Some("127.0.0.1:9999".to_string()),
            _ => None,
        })
        .expect("loopback resolves");
        assert_eq!(
            cfg.metrics.bind,
            Some("127.0.0.1:9999".parse().expect("addr"))
        );
        assert!(cfg.metrics.token.is_none());

        let err = Config::from_lookup(|key| match key {
            "FLEETT_TOKEN" => Some("t".to_string()),
            "FLEETT_METRICS_BIND" => Some("not-an-address".to_string()),
            _ => None,
        })
        .expect_err("bad metrics bind");
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "FLEETT_METRICS_BIND",
                ..
            }
        ));
    }

    #[test]
    fn the_metrics_token_never_reaches_the_debug_output() {
        let cfg = Config::from_lookup(|key| match key {
            "FLEETT_TOKEN" => Some("t".to_string()),
            "FLEETT_METRICS_TOKEN" => Some("scrape-me".to_string()),
            _ => None,
        })
        .expect("resolves");
        let debug = format!("{:?}", cfg.metrics);
        assert!(!debug.contains("scrape-me"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }
}

fn resolve_security(get: &dyn Fn(&str) -> Option<String>) -> Result<SecurityConfig, ConfigError> {
    fn env_u32(
        get: &dyn Fn(&str) -> Option<String>,
        suffix: &str,
        var: &'static str,
        default: u32,
    ) -> Result<u32, ConfigError> {
        match get(suffix) {
            None => Ok(default),
            Some(value) => value.parse::<u32>().map_err(|err| ConfigError::Invalid {
                var,
                value: value.clone(),
                detail: err.to_string(),
            }),
        }
    }

    fn env_u64(
        get: &dyn Fn(&str) -> Option<String>,
        suffix: &str,
        var: &'static str,
        default: u64,
    ) -> Result<u64, ConfigError> {
        match get(suffix) {
            None => Ok(default),
            Some(value) => value.parse::<u64>().map_err(|err| ConfigError::Invalid {
                var,
                value: value.clone(),
                detail: err.to_string(),
            }),
        }
    }

    let rate_limit_per_min = env_u32(
        get,
        "RATE_LIMIT_PER_MIN",
        "FLEETT_RATE_LIMIT_PER_MIN",
        DEFAULT_RATE_LIMIT_PER_MIN,
    )?;
    let rate_limit_auth_per_min = env_u32(
        get,
        "RATE_LIMIT_AUTH_PER_MIN",
        "FLEETT_RATE_LIMIT_AUTH_PER_MIN",
        DEFAULT_RATE_LIMIT_AUTH_PER_MIN,
    )?;
    let rate_limit_heavy_per_min = env_u32(
        get,
        "RATE_LIMIT_HEAVY_PER_MIN",
        "FLEETT_RATE_LIMIT_HEAVY_PER_MIN",
        DEFAULT_RATE_LIMIT_HEAVY_PER_MIN,
    )?;
    let lockout_failures = env_u32(
        get,
        "AUTH_LOCKOUT_FAILURES",
        "FLEETT_AUTH_LOCKOUT_FAILURES",
        DEFAULT_AUTH_LOCKOUT_FAILURES,
    )?;
    let lockout_window_s = env_u64(
        get,
        "AUTH_LOCKOUT_WINDOW_S",
        "FLEETT_AUTH_LOCKOUT_WINDOW_S",
        DEFAULT_AUTH_LOCKOUT_WINDOW_S,
    )?;
    let lockout_s = env_u64(
        get,
        "AUTH_LOCKOUT_S",
        "FLEETT_AUTH_LOCKOUT_S",
        DEFAULT_AUTH_LOCKOUT_S,
    )?;

    for (var, value) in [
        ("FLEETT_AUTH_LOCKOUT_WINDOW_S", lockout_window_s),
        ("FLEETT_AUTH_LOCKOUT_S", lockout_s),
    ] {
        if value == 0 {
            return Err(ConfigError::Invalid {
                var,
                value: "0".to_string(),
                detail: "expected a positive number of seconds".to_string(),
            });
        }
    }

    Ok(SecurityConfig {
        rate_limit_per_min,
        rate_limit_auth_per_min,
        rate_limit_heavy_per_min,
        lockout_failures,
        lockout_window_s,
        lockout_s,
    })
}
