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
use std::path::{Path, PathBuf};

/// UID/GID used by every agent container.
pub const CONTAINER_UID: u32 = 10_000;
pub const CONTAINER_GID: u32 = 10_000;

/// Compose label recording the image an instance ran before its last
/// successful upgrade (kept for reproducibility).
pub const PREVIOUS_IMAGE_LABEL: &str = "fleett.previous_image";

/// Create the directory layout if it does not exist yet.
pub fn ensure_layout(cfg: &Config) -> io::Result<()> {
    for dir in [
        cfg.root.clone(),
        cfg.instances_dir(),
        cfg.shares_dir(),
        cfg.templates_dir(),
        cfg.root.join("bus"),
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
///
/// Secrets are not written here any more: they go to the encrypted store (see
/// [`crate::secrets`]), or to a legacy `secrets.env` for instances that still
/// use one.
pub fn write_instance(
    cfg: &Config,
    spec: &InstanceSpec,
    rendered: &RenderedInstance,
    write_soul: bool,
) -> io::Result<()> {
    let dir = cfg.instance_dir(&spec.id);
    let data = cfg.data_dir(&spec.id);
    create_dir_private(&dir)?;
    create_dir_private(&data)?;
    chown_quiet(&data, CONTAINER_UID, CONTAINER_GID);

    write_file(&cfg.compose_path(&spec.id), &rendered.compose_yaml, 0o644)?;
    write_file(&data.join("config.yaml"), &rendered.config_yaml, 0o600)?;
    write_file(
        &data.join("healthcheck.py"),
        render::HEALTHCHECK_SCRIPT,
        0o644,
    )?;
    if write_soul {
        if let Some(soul) = &rendered.soul_md {
            write_file(&data.join("SOUL.md"), soul, 0o600)?;
        }
    }
    Ok(())
}

/// Write the legacy plaintext `secrets.env` with mode 0600.
///
/// Only used by instances that were created before the encrypted store and
/// have not been migrated yet.
pub fn write_legacy_secrets(
    cfg: &Config,
    id: &str,
    secrets: &BTreeMap<String, String>,
) -> io::Result<()> {
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

/// Read a legacy `secrets.env` back. Missing files yield an empty map.
pub fn read_legacy_secrets(cfg: &Config, id: &str) -> BTreeMap<String, String> {
    match fs::read_to_string(cfg.secrets_path(id)) {
        Ok(text) => parse_env_file(&text),
        Err(_) => BTreeMap::new(),
    }
}

/// The on-disk compose file, or an empty string when it is missing.
pub fn read_compose(cfg: &Config, id: &str) -> String {
    fs::read_to_string(cfg.compose_path(id)).unwrap_or_default()
}

/// Compose project declared by the instance file, when one is declared.
///
/// Updates reuse it so a project created by an older release keeps its name
/// (and its containers) instead of being duplicated under the new prefix.
pub fn declared_project(cfg: &Config, id: &str) -> Option<String> {
    let text = fs::read_to_string(cfg.compose_path(id)).ok()?;
    compose_project_name(&text)
}

/// Read the `name:` field of a compose document.
///
/// Missing, blank or unparsable documents yield `None`; callers then fall back
/// to the name used by this build.
pub(crate) fn compose_project_name(text: &str) -> Option<String> {
    let doc: Value = serde_yaml::from_str(text).ok()?;
    let name = doc.get("name")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Container image declared by a compose document (`services.agent.image`).
pub(crate) fn compose_image(text: &str) -> Option<String> {
    let doc: Value = serde_yaml::from_str(text).ok()?;
    let image = doc
        .get("services")?
        .get("agent")?
        .get("image")?
        .as_str()?
        .trim();
    (!image.is_empty()).then(|| image.to_string())
}

/// `fleett.previous_image` label of an instance, read back from its compose
/// file.
///
/// The label is the reproducibility record of the last successful upgrade; the
/// compose file is the only place that keeps it.
pub fn previous_image(cfg: &Config, id: &str) -> Option<String> {
    let text = fs::read_to_string(cfg.compose_path(id)).ok()?;
    previous_image_from_compose(&text)
}

/// Read `fleett.previous_image` from a compose document.
pub(crate) fn previous_image_from_compose(text: &str) -> Option<String> {
    let doc: Value = serde_yaml::from_str(text).ok()?;
    let labels = doc.get("services")?.get("agent")?.get("labels")?;
    let value = labels.get(PREVIOUS_IMAGE_LABEL)?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Write the compose file only (share changes do not touch the Hermes config).
pub fn write_compose(cfg: &Config, id: &str, compose_yaml: &str) -> io::Result<()> {
    write_file(&cfg.compose_path(id), compose_yaml, 0o644)
}

/// Recursively copy the `data/` directory of `from` into the one of `to`.
///
/// Used by `POST /api/instances/{id}/clone`: the clone receives its own copy
/// (independent data directory), and the generated files written afterwards by
/// [`write_instance`] stay authoritative. Returns the number of bytes copied.
///
/// The copy preserves modes and symlinks, and normalises ownership to the
/// container UID/GID so the agent can write its own memory files. A missing
/// source directory simply copies nothing.
pub fn copy_data(cfg: &Config, from: &str, to: &str) -> io::Result<u64> {
    if from == to {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot copy an instance data directory onto itself",
        ));
    }
    let source = cfg.data_dir(from);
    let destination = cfg.data_dir(to);
    if !source.is_dir() {
        return Ok(0);
    }
    let copied = copy_tree(&source, &destination)?;
    chown_tree(&destination, CONTAINER_UID, CONTAINER_GID);
    Ok(copied)
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<u64> {
    fs::create_dir_all(destination)?;
    let mut copied = 0u64;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)?;
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            copied += copy_tree(&from, &to)?;
            continue;
        }
        if file_type.is_symlink() {
            let target = fs::read_link(&from)?;
            let _ = fs::remove_file(&to);
            std::os::unix::fs::symlink(target, &to)?;
            continue;
        }
        if !file_type.is_file() {
            // Sockets, FIFOs and device nodes are runtime artifacts, not data.
            continue;
        }
        copied += fs::copy(&from, &to)?;
        fs::set_permissions(
            &to,
            fs::Permissions::from_mode(metadata.permissions().mode()),
        )?;
    }
    Ok(copied)
}

/// Best-effort recursive ownership change (a non-root process keeps its files).
fn chown_tree(path: &Path, uid: u32, gid: u32) {
    chown_quiet(path, uid, gid);
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if fs::symlink_metadata(&child)
            .map(|metadata| metadata.file_type().is_dir())
            .unwrap_or(false)
        {
            chown_tree(&child, uid, gid);
        } else {
            chown_quiet(&child, uid, gid);
        }
    }
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
        // The history dies with the data: a purged instance is forgotten, and
        // the directory must be empty for `remove_dir` below to succeed.
        let git = dir.join(crate::history::GIT_DIR);
        if git.exists() {
            fs::remove_dir_all(&git)?;
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

/// Create `instances/<id>` (mode 0700) for an import.
///
/// Returns an error when the directory already exists: an import never adopts
/// the files of another instance.
pub fn create_instance_dir(cfg: &Config, id: &str) -> io::Result<PathBuf> {
    let dir = cfg.instance_dir(id);
    if dir.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", dir.display()),
        ));
    }
    create_dir_private(&dir)?;
    Ok(dir)
}

/// Normalise an imported `data/` tree: tight directory modes, container
/// ownership, and the health probe when the archive did not carry one.
pub fn finalize_imported_data(cfg: &Config, id: &str) -> io::Result<()> {
    let data = cfg.data_dir(id);
    create_dir_private(&data)?;
    let healthcheck = data.join("healthcheck.py");
    if !healthcheck.is_file() {
        write_file(&healthcheck, render::HEALTHCHECK_SCRIPT, 0o644)?;
    }
    chown_tree(&data, CONTAINER_UID, CONTAINER_GID);
    Ok(())
}

/// Best-effort ownership repair after `fleett backups restore`.
///
/// The archive format carries no ownership (and must not: it would let a
/// crafted archive take files over). Every agent directory gets the container
/// UID/GID back, so the restored agents can write their own memory files.
pub fn adopt_restored_root(root: &Path) {
    if let Ok(entries) = fs::read_dir(root.join("instances")) {
        for entry in entries.flatten() {
            let data = entry.path().join("data");
            if data.is_dir() {
                let _ = fs::set_permissions(&data, fs::Permissions::from_mode(0o700));
                chown_quiet(&data, CONTAINER_UID, CONTAINER_GID);
                chown_tree(&data, CONTAINER_UID, CONTAINER_GID);
            }
        }
    }
    if let Ok(entries) = fs::read_dir(root.join("shares")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
            chown_quiet(&path, CONTAINER_UID, CONTAINER_GID);
            chown_tree(&path, CONTAINER_UID, CONTAINER_GID);
        }
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

    let cpu_limit = agent
        .and_then(|service| service.get("cpus"))
        .and_then(|value| {
            value.as_f64().or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.trim().parse::<f64>().ok())
            })
        })
        .filter(|value| models::validate_cpu_limit(*value).is_ok())
        .unwrap_or(models::DEFAULT_CPU_LIMIT);

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

    let allow_lan = agent
        .and_then(|service| service.get("networks"))
        .and_then(Value::as_sequence)
        .map(|networks| {
            networks
                .iter()
                .filter_map(Value::as_str)
                .any(|network| network == "lan")
        })
        .unwrap_or(false);

    let image = agent
        .and_then(|service| service.get("image"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|image| !image.is_empty())
        .unwrap_or_default()
        .to_string();
    let previous_image = agent
        .and_then(|service| service.get("labels"))
        .and_then(|labels| labels.get(PREVIOUS_IMAGE_LABEL))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|image| !image.is_empty())
        .map(str::to_string);

    InstanceSpec {
        id: id.to_string(),
        name: id.to_string(),
        template: "custom".to_string(),
        model: model.unwrap_or_default(),
        base_url: base_url.unwrap_or_default(),
        api_key_env: api_key_env.unwrap_or_else(|| GATE_TOKEN_ENV.to_string()),
        provider: None,
        channel,
        shares,
        mem_limit_mb,
        cpu_limit,
        image,
        previous_image,
        allow_lan,
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
    fn lan_access_is_read_back_from_the_compose_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        ensure_layout(&cfg).expect("layout");
        std::fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        std::fs::write(
            cfg.compose_path("alice"),
            "name: fleett-alice\nservices:\n  agent:\n    networks:\n      - fleet\n      - lan\n",
        )
        .expect("compose file");

        let specs = scan_instances(&cfg);
        assert_eq!(specs.len(), 1);
        assert!(specs[0].allow_lan, "the lan network must be read back");

        std::fs::write(
            cfg.compose_path("alice"),
            "name: fleett-alice\nservices:\n  agent:\n    networks:\n      - fleet\n",
        )
        .expect("compose file");
        assert!(!scan_instances(&cfg)[0].allow_lan);
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
    fn compose_project_names_are_read_from_the_document() {
        assert_eq!(
            compose_project_name("name: fleett-alice\n"),
            Some("fleett-alice".to_string())
        );
        assert_eq!(compose_project_name("services: {}\n"), None);
        assert_eq!(compose_project_name("name: \" \"\n"), None);
        assert_eq!(compose_project_name("name: [unclosed\n"), None);
    }

    #[test]
    fn the_image_and_the_previous_tag_are_read_back_from_the_compose_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        ensure_layout(&cfg).expect("layout");
        std::fs::create_dir_all(cfg.instance_dir("alice")).expect("instance dir");
        std::fs::write(
            cfg.compose_path("alice"),
            "name: fleett-alice\nservices:\n  agent:\n    image: agent:2\n    labels:\n      \
             fleett.previous_image: agent:1\n",
        )
        .expect("compose file");

        let specs = scan_instances(&cfg);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].image, "agent:2");
        assert_eq!(specs[0].previous_image.as_deref(), Some("agent:1"));
        assert_eq!(previous_image(&cfg, "alice").as_deref(), Some("agent:1"));
        assert_eq!(
            compose_image("services:\n  agent:\n    image: agent:3\n").as_deref(),
            Some("agent:3")
        );

        // A compose file without an image or a label reports none.
        std::fs::write(cfg.compose_path("alice"), "name: fleett-alice\n").expect("compose file");
        let specs = scan_instances(&cfg);
        assert_eq!(specs[0].image, "");
        assert!(specs[0].previous_image.is_none());
        assert!(previous_image(&cfg, "alice").is_none());
        assert!(previous_image(&cfg, "ghost").is_none());
    }

    #[test]
    fn memory_limits_are_parsed() {
        assert_eq!(parse_memory_mb("1536m"), Some(1536));
        assert_eq!(parse_memory_mb("256M"), Some(256));
        assert_eq!(parse_memory_mb("nope"), None);
    }

    #[test]
    fn data_directories_are_copied_recursively() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        ensure_layout(&cfg).expect("layout");

        let source = cfg.data_dir("alice");
        std::fs::create_dir_all(source.join("sessions")).expect("source tree");
        std::fs::write(source.join("config.yaml"), "model: {}\n").expect("config");
        std::fs::write(source.join("sessions/one.json"), "{}").expect("session");
        std::fs::write(source.join("MEMORY.md"), "remember\n").expect("memory");
        std::os::unix::fs::symlink("MEMORY.md", source.join("latest.md")).expect("symlink");

        let copied = copy_data(&cfg, "alice", "bob").expect("copy");
        assert!(copied > 0);

        let destination = cfg.data_dir("bob");
        assert_eq!(
            std::fs::read_to_string(destination.join("MEMORY.md")).expect("memory"),
            "remember\n"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("sessions/one.json")).expect("session"),
            "{}"
        );
        assert!(destination.join("sessions").is_dir());
        let link = std::fs::read_link(destination.join("latest.md")).expect("symlink kept");
        assert_eq!(link, std::path::PathBuf::from("MEMORY.md"));

        // The source is untouched and the copy is a real copy.
        std::fs::write(source.join("MEMORY.md"), "changed\n").expect("source write");
        assert_eq!(
            std::fs::read_to_string(destination.join("MEMORY.md")).expect("memory"),
            "remember\n"
        );

        assert_eq!(
            copy_data(&cfg, "ghost", "carol").expect("missing source"),
            0
        );
        assert!(copy_data(&cfg, "alice", "alice").is_err());
    }

    #[test]
    fn legacy_secrets_round_trip_with_quotes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().to_path_buf());
        ensure_layout(&cfg).expect("layout");
        let mut secrets = BTreeMap::new();
        secrets.insert("TELEGRAM_BOT_TOKEN".to_string(), "123:ABC".to_string());
        secrets.insert(
            "WEIRD".to_string(),
            "value with spaces # and \"quotes\"".to_string(),
        );
        write_legacy_secrets(&cfg, "alice", &secrets).expect("write secrets");

        let metadata = fs::metadata(cfg.secrets_path("alice")).expect("metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

        let read = read_legacy_secrets(&cfg, "alice");
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
