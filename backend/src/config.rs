//! Runtime configuration.
//!
//! Configuration comes from the process environment, completed by an optional
//! `fleet.env` file located in the fleet root. The file is a plain
//! `KEY=VALUE` list: no shell expansion, no command substitution, and process
//! environment variables always win over file entries.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_ROOT: &str = "/opt/hermes-fleet";
pub const DEFAULT_BIND: &str = "127.0.0.1:8090";
pub const DEFAULT_IMAGE: &str = "hermes-agent:0.22.2-local";
pub const DEFAULT_NETWORK: &str = "hermes-fleet";

/// Name of the optional environment file inside `FLEET_ROOT`.
pub const ENV_FILE_NAME: &str = "fleet.env";

/// Docker driver selected by `FLEET_DRIVER`.
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
    /// `None` only when `FLEET_ALLOW_NO_AUTH=1`.
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
        "FLEET_TOKEN is required (set it in the environment or in $FLEET_ROOT/fleet.env, \
         or set FLEET_ALLOW_NO_AUTH=1 to explicitly disable authentication)"
    )]
    MissingToken,
}

impl Config {
    /// Load the configuration from the real process environment plus
    /// `$FLEET_ROOT/fleet.env`.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_process_env(|key| std::env::var(key).ok())
    }

    /// Same as [`Config::from_env`] with an injectable environment lookup.
    ///
    /// `FLEET_ROOT` is resolved from the process environment first (or the
    /// default root); `fleet.env` is then read from that root and fills in any
    /// variable the process environment does not define.
    pub fn from_process_env(process: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let explicit_root = process("FLEET_ROOT")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);

        let env_file = match explicit_root {
            Some(ref root) => root.join(ENV_FILE_NAME),
            None => {
                // The installer writes `$HOME/.hermes-fleet/fleet.env`, while the
                // system default root is `/opt/hermes-fleet`. Prefer the home file
                // when it exists so a bare `hermes-fleet` starts with no env at all.
                match process("HOME").map(|home| {
                    PathBuf::from(home)
                        .join(".hermes-fleet")
                        .join(ENV_FILE_NAME)
                }) {
                    Some(path) if path.is_file() => path,
                    _ => PathBuf::from(DEFAULT_ROOT).join(ENV_FILE_NAME),
                }
            }
        };
        let file_vars = load_env_file(&env_file);
        Self::from_lookup(|key| process(key).or_else(|| file_vars.get(key).cloned()))
    }

    /// Resolve a configuration from a raw lookup function.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |key: &str| {
            lookup(key)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };

        let root = get("FLEET_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_ROOT));

        let bind_raw = get("FLEET_BIND").unwrap_or_else(|| DEFAULT_BIND.to_string());
        let bind = bind_raw
            .parse::<SocketAddr>()
            .map_err(|err| ConfigError::Invalid {
                var: "FLEET_BIND",
                value: bind_raw.clone(),
                detail: err.to_string(),
            })?;

        let allow_no_auth = matches!(
            get("FLEET_ALLOW_NO_AUTH").as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "on")
        );
        let token = get("FLEET_TOKEN");
        if token.is_none() && !allow_no_auth {
            return Err(ConfigError::MissingToken);
        }

        let driver = match get("FLEET_DRIVER") {
            None => DriverKind::Mock,
            Some(value) => DriverKind::parse(&value).ok_or_else(|| ConfigError::Invalid {
                var: "FLEET_DRIVER",
                value: value.clone(),
                detail: "expected \"mock\" or \"compose\"".to_string(),
            })?,
        };

        let image = get("FLEET_IMAGE").unwrap_or_else(|| DEFAULT_IMAGE.to_string());
        let network = get("FLEET_NETWORK").unwrap_or_else(|| DEFAULT_NETWORK.to_string());

        let max_total_cpu = match get("FLEET_MAX_TOTAL_CPU") {
            None => 0.0,
            Some(value) => {
                let parsed = value.parse::<f64>().map_err(|err| ConfigError::Invalid {
                    var: "FLEET_MAX_TOTAL_CPU",
                    value: value.clone(),
                    detail: err.to_string(),
                })?;
                if !parsed.is_finite() || parsed < 0.0 {
                    return Err(ConfigError::Invalid {
                        var: "FLEET_MAX_TOTAL_CPU",
                        value,
                        detail: "expected a non-negative number of CPUs (0 disables the cap)"
                            .to_string(),
                    });
                }
                parsed
            }
        };
        let max_total_mem_mb = match get("FLEET_MAX_TOTAL_MEM_MB") {
            None => 0,
            Some(value) => value.parse::<u64>().map_err(|err| ConfigError::Invalid {
                var: "FLEET_MAX_TOTAL_MEM_MB",
                value: value.clone(),
                detail: err.to_string(),
            })?,
        };

        let web_dist = get("FLEET_WEB_DIST").map(PathBuf::from).or_else(|| {
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

    pub fn env_file_path(&self) -> PathBuf {
        self.root.join(ENV_FILE_NAME)
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
/// empty map: the fleet must still start on a fresh host.
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
# fleet configuration
FLEET_BIND=127.0.0.1:9999

FLEET_IMAGE = \"example/agent:1.0\" 
not a variable
9BAD=value
FLEET_NETWORK='fleet-net'
";
        let vars = parse_env_file(text);
        assert_eq!(
            vars.get("FLEET_BIND").map(String::as_str),
            Some("127.0.0.1:9999")
        );
        assert_eq!(
            vars.get("FLEET_IMAGE").map(String::as_str),
            Some("example/agent:1.0")
        );
        assert_eq!(
            vars.get("FLEET_NETWORK").map(String::as_str),
            Some("fleet-net")
        );
        assert!(!vars.contains_key("9BAD"));
        assert_eq!(vars.len(), 3);
    }

    #[test]
    fn fleet_env_file_fills_missing_variables() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join(ENV_FILE_NAME),
            "# generated by the installer\n\
             FLEET_BIND=127.0.0.1:9999\n\
             FLEET_IMAGE=from-file:1\n\
             FLEET_TOKEN=file-token\n",
        )
        .expect("write fleet.env");

        let root = dir.path().to_string_lossy().to_string();
        let cfg = Config::from_process_env(|key| match key {
            "FLEET_ROOT" => Some(root.clone()),
            "FLEET_TOKEN" => Some("process-token".to_string()),
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
    fn home_fleet_env_is_used_without_fleet_root() {
        let home = tempfile::tempdir().expect("temp home");
        let root = home.path().join(".hermes-fleet");
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(
            root.join(ENV_FILE_NAME),
            format!(
                "FLEET_ROOT={}\nFLEET_TOKEN=home-token\n",
                root.to_string_lossy()
            ),
        )
        .expect("write home fleet.env");

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
    fn missing_token_is_rejected_unless_auth_is_disabled() {
        let err = Config::from_lookup(|_| None).expect_err("token required");
        assert!(matches!(err, ConfigError::MissingToken));

        let cfg = Config::from_lookup(|key| match key {
            "FLEET_ALLOW_NO_AUTH" => Some("1".to_string()),
            _ => None,
        })
        .expect("no-auth mode resolves");
        assert!(cfg.token.is_none());
        assert!(cfg.allow_no_auth);
    }

    #[test]
    fn total_caps_are_optional_and_validated() {
        let cfg = Config::from_lookup(|key| match key {
            "FLEET_TOKEN" => Some("t".to_string()),
            "FLEET_MAX_TOTAL_CPU" => Some("4.5".to_string()),
            "FLEET_MAX_TOTAL_MEM_MB" => Some("6144".to_string()),
            _ => None,
        })
        .expect("caps resolve");
        assert_eq!(cfg.max_total_cpu, 4.5);
        assert_eq!(cfg.max_total_mem_mb, 6144);

        let default = Config::from_lookup(|key| (key == "FLEET_TOKEN").then(|| "t".to_string()))
            .expect("defaults resolve");
        assert_eq!(default.max_total_cpu, 0.0);
        assert_eq!(default.max_total_mem_mb, 0);

        for (var, value) in [
            ("FLEET_MAX_TOTAL_CPU", "-1"),
            ("FLEET_MAX_TOTAL_CPU", "lots"),
            ("FLEET_MAX_TOTAL_MEM_MB", "-5"),
            ("FLEET_MAX_TOTAL_MEM_MB", "1.5"),
        ] {
            let err = Config::from_lookup(|key| match key {
                "FLEET_TOKEN" => Some("t".to_string()),
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
            "FLEET_TOKEN" => Some("t".to_string()),
            "FLEET_DRIVER" => Some("swarm".to_string()),
            _ => None,
        })
        .expect_err("bad driver");
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "FLEET_DRIVER",
                ..
            }
        ));

        let err = Config::from_lookup(|key| match key {
            "FLEET_TOKEN" => Some("t".to_string()),
            "FLEET_BIND" => Some("nope".to_string()),
            _ => None,
        })
        .expect_err("bad bind");
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "FLEET_BIND",
                ..
            }
        ));
    }
}
