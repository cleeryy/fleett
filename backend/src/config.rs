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

/// Fully resolved configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub bind: SocketAddr,
    /// `None` only when `FLEETT_ALLOW_NO_AUTH=1`.
    pub token: Option<String>,
    pub allow_no_auth: bool,
    pub driver: DriverKind,
    pub image: String,
    pub network: String,
    /// Optional fleet-wide caps. `0` (the default) disables the cap.
    pub max_total_cpu: f64,
    pub max_total_mem_mb: u64,
    /// Directory containing an on-disk frontend build, when one is present.
    pub web_dist: Option<PathBuf>,
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
            &|suffix| resolve_var(&lookup, suffix),
            PathBuf::from(DEFAULT_ROOT),
            true,
        )
    }

    fn assemble(
        get: &dyn Fn(&str) -> Option<String>,
        default_root: PathBuf,
        require_token: bool,
    ) -> Result<Self, ConfigError> {
        let get = |suffix: &str| {
            get(suffix)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };

        let root = get("ROOT").map(PathBuf::from).unwrap_or(default_root);

        let bind_raw = get("BIND").unwrap_or_else(|| DEFAULT_BIND.to_string());
        let bind = bind_raw
            .parse::<SocketAddr>()
            .map_err(|err| ConfigError::Invalid {
                var: "FLEETT_BIND",
                value: bind_raw.clone(),
                detail: err.to_string(),
            })?;

        let allow_no_auth = matches!(
            get("ALLOW_NO_AUTH").as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "on")
        );
        let token = get("TOKEN");
        if token.is_none() && !allow_no_auth && require_token {
            return Err(ConfigError::MissingToken);
        }

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

        Ok(Self {
            root,
            bind,
            token,
            allow_no_auth,
            driver,
            image,
            network,
            max_total_cpu,
            max_total_mem_mb,
            web_dist,
        })
    }

    /// Configuration used by the integration tests: mock driver, temp root.
    pub fn for_tests(root: PathBuf) -> Self {
        Self {
            root,
            bind: "127.0.0.1:0"
                .parse()
                .expect("literal socket address is valid"),
            token: Some("test-token".to_string()),
            allow_no_auth: false,
            driver: DriverKind::Mock,
            image: DEFAULT_IMAGE.to_string(),
            network: DEFAULT_NETWORK.to_string(),
            max_total_cpu: 0.0,
            max_total_mem_mb: 0,
            web_dist: None,
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

/// Resolve one configuration variable: `FLEETT_<suffix>` then `FLEET_<suffix>`.
fn resolve_var(lookup: &dyn Fn(&str) -> Option<String>, suffix: &str) -> Option<String> {
    [
        format!("{PREFIX}{suffix}"),
        format!("{LEGACY_PREFIX}{suffix}"),
    ]
    .iter()
    .find_map(|name| lookup(name))
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
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
}
