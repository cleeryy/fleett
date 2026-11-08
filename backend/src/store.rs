//! Filesystem layout of the fleet.
//!
//! The compose file and the instance directory are the source of truth; the
//! SQLite database is an index rebuilt from them at startup.

use crate::config::{parse_env_file, Config};
use crate::models::{self, ChannelKind, InstanceSpec, ShareMode, ShareRef, Status, GATE_TOKEN_ENV};
use crate::render::{self, RenderedInstance};
use serde_yaml::Value;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// UID/GID used by every agent container.
pub const CONTAINER_UID: u32 = 10_000;
pub const CONTAINER_GID: u32 = 10_000;

/// Create the directory layout if it does not exist yet.
pub fn ensure_layout(cfg: &Config) -> io::Result<()> {
    for dir in [
        cfg.root.clone(),
        cfg.instances_dir(),
        cfg.shares_dir(),
        cfg.templates_dir(),
    ] {
        create_dir_private(&dir)?;
    }
    Ok(())
}

/// Create a directory tree with mode 0700, tightening an existing one.
fn create_dir_private(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Write every generated file for one instance.
pub fn write_instance(
    cfg: &Config,
    spec: &InstanceSpec,
    rendered: &RenderedInstance,
    secrets: &BTreeMap<String, String>,
    write_soul: bool,
) -> io::Result<()> {
    let dir = cfg.instance_dir(&spec.id);
    let data = cfg.data_dir(&spec.id);
    create_dir_private(&dir)?;
    create_dir_private(&data)?;
    chown_quiet(&data, CONTAINER_UID, CONTAINER_GID);

    write_file(&cfg.compose_path(&spec.id), &rendered.compose_yaml, 0o644)?;
    write_file(&data.join("config.yaml"), &rendered.config_yaml, 0o600)?;
    if write_soul {
        if let Some(soul) = &rendered.soul_md {
            write_file(&data.join("SOUL.md"), soul, 0o600)?;
        }
    }
    write_secrets(cfg, &spec.id, secrets)?;
    Ok(())
}

/// Write `secrets.env` with mode 0600.
pub fn write_secrets(cfg: &Config, id: &str, secrets: &BTreeMap<String, String>) -> io::Result<()> {
    let mut text = String::new();
    for (key, value) in secrets {
        text.push_str(key);
        text.push('=');
        text.push_str(&format_secret_value(value));
        text.push('\n');
    }
    write_file(&cfg.secrets_path(id), &text, 0o600)
}

fn format_secret_value(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '#' || c == '"' || c == '\'' || c == '\\');
    if !needs_quotes {
        return value.to_string();
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

/// Read `secrets.env` back. Missing files yield an empty map.
pub fn read_secrets(cfg: &Config, id: &str) -> BTreeMap<String, String> {
    match fs::read_to_string(cfg.secrets_path(id)) {
        Ok(text) => parse_env_file(&text),
        Err(_) => BTreeMap::new(),
    }
}

/// Sorted secret keys, for `InstanceDetail::env_keys`.
pub fn read_env_keys(cfg: &Config, id: &str) -> Vec<String> {
    read_secrets(cfg, id).into_keys().collect()
}

/// The on-disk compose file, or an empty string when it is missing.
pub fn read_compose(cfg: &Config, id: &str) -> String {
    fs::read_to_string(cfg.compose_path(id)).unwrap_or_default()
}

/// Write the compose file only (share changes do not touch the Hermes config).
pub fn write_compose(cfg: &Config, id: &str, compose_yaml: &str) -> io::Result<()> {
    write_file(&cfg.compose_path(id), compose_yaml, 0o644)
}

/// Remove the generated files of an instance.
///
/// The data directory survives unless `purge` is set, so a later create with
/// the same id finds its files again.
pub fn remove_instance(cfg: &Config, id: &str, purge: bool) -> io::Result<()> {
    let dir = cfg.instance_dir(id);
    remove_file_if_exists(&cfg.compose_path(id))?;
    remove_file_if_exists(&cfg.secrets_path(id))?;
    if purge {
        let data = cfg.data_dir(id);
        if data.exists() {
            fs::remove_dir_all(&data)?;
        }
        // Only removes the directory when nothing else is left in it.
        let _ = fs::remove_dir(&dir);
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Create a share directory (idempotent).
pub fn ensure_share_dir(cfg: &Config, name: &str) -> io::Result<()> {
    let dir = cfg.share_dir(name);
    fs::create_dir_all(&dir)?;
    chown_quiet(&dir, CONTAINER_UID, CONTAINER_GID);
    Ok(())
}

/// Delete a share directory and its contents.
pub fn remove_share_dir(cfg: &Config, name: &str) -> io::Result<()> {
    let dir = cfg.share_dir(name);
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

/// Names of the share directories present on disk.
pub fn scan_shares(cfg: &Config) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(cfg.shares_dir()) else {
        return names;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if models::validate_share_name(name).is_ok() {
            names.push(name.to_string());
        }
    }
    names.sort();
    names
}

/// Every instance directory that still has a `compose.yaml`, reconstructed
/// from the files themselves (no database involved).
pub fn scan_instances(cfg: &Config) -> Vec<InstanceSpec> {
    let mut specs = Vec::new();
    let Ok(entries) = fs::read_dir(cfg.instances_dir()) else {
        return specs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if models::validate_instance_id(id).is_err() {
            continue;
        }
        let Ok(compose_text) = fs::read_to_string(path.join("compose.yaml")) else {
            continue;
        };
        let Ok(doc) = serde_yaml::from_str::<Value>(&compose_text) else {
            tracing::warn!(id, "ignoring instance with unparsable compose.yaml");
            continue;
        };
        specs.push(reconstruct_spec(cfg, id, &doc));
    }
    specs.sort_by(|left, right| left.id.cmp(&right.id));
    specs
}

fn reconstruct_spec(cfg: &Config, id: &str, compose: &Value) -> InstanceSpec {
    let agent = compose
        .get("services")
        .and_then(|services| services.get("agent"));

    let mem_limit_mb = agent
        .and_then(|service| service.get("mem_limit"))
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(parse_memory_mb))
        })
        .map(|value| value as u32)
        .filter(|value| models::validate_mem_limit(*value).is_ok())
        .unwrap_or(models::DEFAULT_MEM_LIMIT_MB);

    let shares = agent
        .and_then(|service| service.get("volumes"))
        .and_then(Value::as_sequence)
        .map(|volumes| {
            volumes
                .iter()
                .filter_map(Value::as_str)
                .filter_map(parse_share_volume)
                .collect()
        })
        .unwrap_or_default();

    let config_doc = fs::read_to_string(cfg.data_dir(id).join("config.yaml"))
        .ok()
        .and_then(|text| serde_yaml::from_str::<Value>(&text).ok());
    let (model, base_url, api_key_env) = config_doc
        .as_ref()
        .map(render::model_from_config)
        .unwrap_or((None, None, None));
    let channel = config_doc
        .as_ref()
        .map(render::channel_from_config)
        .unwrap_or(ChannelKind::None);

    InstanceSpec {
        id: id.to_string(),
        name: id.to_string(),
        template: "custom".to_string(),
        model: model.unwrap_or_default(),
        base_url: base_url.unwrap_or_default(),
        api_key_env: api_key_env.unwrap_or_else(|| GATE_TOKEN_ENV.to_string()),
        channel,
        shares,
        mem_limit_mb,
        notes: String::new(),
        status: Status::Unknown,
        last_error: None,
        created_at: models::now_iso(),
        updated_at: models::now_iso(),
    }
}

fn parse_memory_mb(value: &str) -> Option<u64> {
    let digits = value.trim().trim_end_matches(['m', 'M']);
    digits.parse::<u64>().ok()
}

fn parse_share_volume(volume: &str) -> Option<ShareRef> {
    let rest = volume.strip_prefix("../../shares/")?;
    let (name, _target) = rest.split_once(':')?;
    if models::validate_share_name(name).is_err() {
        return None;
    }
    let mode = if volume.ends_with(":ro") {
        ShareMode::Ro
    } else {
        ShareMode::Rw
    };
    Some(ShareRef {
        name: name.to_string(),
        mode,
    })
}

fn write_file(path: &Path, contents: &str, mode: u32) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(mode);
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    // `mode` above only applies to newly created files.
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Best-effort ownership change; a non-root process simply keeps its files.
fn chown_quiet(path: &Path, uid: u32, gid: u32) {
    if let Err(err) = std::os::unix::fs::chown(path, Some(uid), Some(gid)) {
        if err.kind() != io::ErrorKind::PermissionDenied {
            tracing::debug!(path = %path.display(), %err, "could not change ownership");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ShareMode, ShareRef};

    #[test]
    fn share_volumes_round_trip() {
        let rw = parse_share_volume("../../shares/findings:/opt/data/shared/findings")
            .expect("rw volume");
        assert_eq!(
            rw,
            ShareRef {
                name: "findings".to_string(),
                mode: ShareMode::Rw
            }
        );
        let ro = parse_share_volume("../../shares/archive:/opt/data/shared/archive:ro")
            .expect("ro volume");
        assert_eq!(ro.mode, ShareMode::Ro);
        assert!(parse_share_volume("/var/run/docker.sock:/var/run/docker.sock").is_none());
        assert!(parse_share_volume("../../shares/../x:/opt/data").is_none());
    }

    #[test]
    fn layout_is_private() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        ensure_layout(&cfg).expect("layout");
        for path in [
            cfg.root.clone(),
            cfg.instances_dir(),
            cfg.shares_dir(),
            cfg.templates_dir(),
        ] {
            let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", path.display());
        }
    }

    #[test]
    fn memory_limits_are_parsed() {
        assert_eq!(parse_memory_mb("1536m"), Some(1536));
        assert_eq!(parse_memory_mb("256M"), Some(256));
        assert_eq!(parse_memory_mb("nope"), None);
    }

    #[test]
    fn secrets_round_trip_with_quotes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().to_path_buf());
        ensure_layout(&cfg).expect("layout");
        let mut secrets = BTreeMap::new();
        secrets.insert("TELEGRAM_BOT_TOKEN".to_string(), "123:ABC".to_string());
        secrets.insert(
            "WEIRD".to_string(),
            "value with spaces # and \"quotes\"".to_string(),
        );
        write_secrets(&cfg, "alice", &secrets).expect("write secrets");

        let metadata = fs::metadata(cfg.secrets_path("alice")).expect("metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

        let read = read_secrets(&cfg, "alice");
        assert_eq!(
            read.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
            Some("123:ABC")
        );
        assert_eq!(
            read.get("WEIRD").map(String::as_str),
            Some("value with spaces # and \"quotes\"")
        );
    }
}
