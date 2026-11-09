//! Portable instance archives: `GET /api/instances/{id}/export` and
//! `POST /api/instances/import`.
//!
//! An archive is a gzipped tar with a fixed layout:
//!
//! ```text
//! manifest.json   format version, timestamps and the instance metadata
//! compose.yaml    the compose file shipped with the instance
//! data/           the full Hermes home (config, SOUL, memory, sessions)
//! ```
//!
//! Share *contents* are never included: shares are fleet-wide and restored
//! separately. Plaintext secret material (`secrets.env`, `*.key`) is excluded
//! at export time and refused at import time, so an archive can be copied off
//! the host without leaking a token; the operator re-supplies the tokens with
//! a PATCH after the import.

use crate::archive;
use crate::config::{Config, VERSION};
use crate::models::{
    now_iso, validate_api_key_env, validate_base_url, validate_cpu_limit, validate_instance_id,
    validate_mem_limit, validate_model, validate_notes, validate_secret_name, validate_share_name,
    ChannelKind, InstanceSpec, ShareRef, Status, DEFAULT_API_KEY_ENV, DEFAULT_CPU_LIMIT,
    DEFAULT_MEM_LIMIT_MB,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// `format` field of an instance manifest.
pub const ARCHIVE_FORMAT: &str = "fleett.instance";
/// Manifest version understood by this build.
pub const FORMAT_VERSION: u32 = 1;
/// Compressed size cap of an uploaded archive.
pub const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub const MANIFEST_NAME: &str = "manifest.json";
pub const COMPOSE_NAME: &str = "compose.yaml";
pub const DATA_PREFIX: &str = "data";

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("filesystem error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid archive: {0}")]
    Invalid(String),
    #[error("the archive has no {MANIFEST_NAME}")]
    MissingManifest,
    #[error(
        "unsupported archive {format:?} v{version}; this build reads {expected:?} v{supported}"
    )]
    Unsupported {
        format: String,
        version: u32,
        expected: &'static str,
        supported: u32,
    },
    #[error("invalid manifest: {0}")]
    Manifest(String),
    #[error("cannot encode the manifest: {0}")]
    Json(#[from] serde_json::Error),
}

/// Instance metadata carried by `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveInstance {
    pub id: String,
    pub name: String,
    pub template: String,
    pub model: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub channel: String,
    /// Provider the instance drew its key from, when it had one. The archive
    /// carries the reference only, never the provider secret.
    #[serde(default)]
    pub provider: Option<String>,
    pub mem_limit_mb: u32,
    pub cpu_limit: f64,
    pub allow_lan: bool,
    #[serde(default)]
    pub shares: Vec<ShareRef>,
    #[serde(default)]
    pub notes: String,
    /// Host the instance ran on when the archive was written. Informational
    /// only: an import starts on the local host of the receiving control plane.
    #[serde(default = "default_host")]
    pub host: String,
}

fn default_host() -> String {
    crate::hosts::LOCAL_HOST.to_string()
}

impl ArchiveInstance {
    fn from_spec(spec: &InstanceSpec) -> Self {
        Self {
            id: spec.id.clone(),
            name: spec.name.clone(),
            template: spec.template.clone(),
            model: spec.model.clone(),
            base_url: spec.base_url.clone(),
            api_key_env: spec.api_key_env.clone(),
            channel: spec.channel.as_str().to_string(),
            provider: spec.provider.clone(),
            mem_limit_mb: spec.mem_limit_mb,
            cpu_limit: spec.cpu_limit,
            allow_lan: spec.allow_lan,
            shares: spec.shares.clone(),
            notes: spec.notes.clone(),
            host: spec.host.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveManifest {
    pub format: String,
    pub format_version: u32,
    pub created_at: String,
    pub fleett_version: String,
    pub instance: ArchiveInstance,
}

/// What the importer read out of an archive.
#[derive(Debug)]
pub struct Extraction {
    pub manifest: ArchiveManifest,
    /// Compose document as shipped, kept for its `${KEY}` declarations only.
    pub compose_yaml: Option<String>,
    /// Files restored under `data/`.
    pub data_entries: usize,
    pub warnings: Vec<String>,
}

/// Build the `.tgz` of one instance.
pub fn export(cfg: &Config, spec: &InstanceSpec) -> Result<Vec<u8>, ArchiveError> {
    let manifest = ArchiveManifest {
        format: ARCHIVE_FORMAT.to_string(),
        format_version: FORMAT_VERSION,
        created_at: now_iso(),
        fleett_version: VERSION.to_string(),
        instance: ArchiveInstance::from_spec(spec),
    };
    let manifest_json = serde_json::to_vec_pretty(&manifest)?;

    let mut builder = tar::Builder::new(archive::gzip_writer());
    archive::append_bytes(
        &mut builder,
        Path::new(MANIFEST_NAME),
        &manifest_json,
        0o600,
    )?;

    let compose_path = cfg.compose_path(&spec.id);
    if let Ok(compose) = fs::read(&compose_path) {
        archive::append_bytes(&mut builder, Path::new(COMPOSE_NAME), &compose, 0o644)?;
    }

    let data = cfg.data_dir(&spec.id);
    if data.is_dir() {
        archive::append_dir(&mut builder, Path::new(DATA_PREFIX), 0o700)?;
        archive::append_tree(&mut builder, &data, Path::new(DATA_PREFIX))?;
    }

    let encoder = builder.into_inner()?;
    Ok(encoder.finish()?)
}

/// Read an archive into `data_dir` and return its manifest.
///
/// Only `manifest.json`, `compose.yaml` and `data/**` are extracted; every
/// other entry is ignored. Unsafe paths abort the whole import before anything
/// is written to the destination.
pub fn extract(reader: impl Read, data_dir: &Path) -> Result<Extraction, ArchiveError> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(reader));
    let mut manifest_json: Option<Vec<u8>> = None;
    let mut compose_yaml: Option<String> = None;
    let mut data_entries = 0usize;
    let mut extracted_bytes = 0u64;
    let mut skipped_secrets = 0usize;

    let entries = tar
        .entries()
        .map_err(|err| ArchiveError::Invalid(format!("cannot read the gzip stream: {err}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|err| ArchiveError::Invalid(format!("cannot read an entry: {err}")))?;
        let path = entry.path()?.into_owned();
        archive::safe_path(&path).map_err(ArchiveError::Invalid)?;

        let Some(name) = path.file_name().map(|name| name.to_owned()) else {
            return Err(ArchiveError::Invalid(format!(
                "entry {} has no file name",
                path.display()
            )));
        };
        if archive::skip_extracted_entry(&name) {
            skipped_secrets += 1;
            continue;
        }

        extracted_bytes = extracted_bytes.saturating_add(entry.size());
        if extracted_bytes > archive::MAX_EXTRACTED_BYTES {
            return Err(ArchiveError::Invalid(format!(
                "the archive expands beyond {} MiB",
                archive::MAX_EXTRACTED_BYTES / (1024 * 1024)
            )));
        }

        if path.as_path() == Path::new(MANIFEST_NAME) {
            manifest_json = Some(archive::read_metadata(&mut entry)?);
            continue;
        }
        if path.as_path() == Path::new(COMPOSE_NAME) {
            compose_yaml =
                Some(String::from_utf8_lossy(&archive::read_metadata(&mut entry)?).into());
            continue;
        }
        let Some(relative) = path
            .strip_prefix(DATA_PREFIX)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
        else {
            // Anything else at the top level belongs to a newer format: ignore.
            continue;
        };
        let is_dir = entry.header().entry_type().is_dir();
        let destination = data_dir.join(relative);
        archive::write_entry(entry, &destination).map_err(|err| {
            ArchiveError::Invalid(format!("cannot restore {}: {err}", path.display()))
        })?;
        if !is_dir {
            data_entries += 1;
        }
    }

    let manifest_json = manifest_json.ok_or(ArchiveError::MissingManifest)?;
    let manifest: ArchiveManifest = serde_json::from_slice(&manifest_json)
        .map_err(|err| ArchiveError::Manifest(err.to_string()))?;
    if manifest.format != ARCHIVE_FORMAT {
        return Err(ArchiveError::Unsupported {
            format: manifest.format,
            version: manifest.format_version,
            expected: ARCHIVE_FORMAT,
            supported: FORMAT_VERSION,
        });
    }
    if manifest.format_version != FORMAT_VERSION {
        return Err(ArchiveError::Unsupported {
            format: manifest.format,
            version: manifest.format_version,
            expected: ARCHIVE_FORMAT,
            supported: FORMAT_VERSION,
        });
    }

    let mut warnings = Vec::new();
    if skipped_secrets > 0 {
        warnings.push(format!(
            "{skipped_secrets} secret file(s) in the archive were ignored; import never restores \
             secrets.env or *.key files"
        ));
    }

    Ok(Extraction {
        manifest,
        compose_yaml,
        data_entries,
        warnings,
    })
}

/// Build the [`InstanceSpec`] of an import from its manifest.
///
/// The id always comes from the request: a manifest id is metadata of the
/// source instance, never the identity of the copy.
pub fn spec_from_manifest(
    manifest: &ArchiveInstance,
    id: &str,
) -> Result<InstanceSpec, ArchiveError> {
    let invalid = |detail: String| ArchiveError::Manifest(detail);
    validate_instance_id(id).map_err(invalid)?;

    let name = manifest.name.trim();
    let name = if name.is_empty() { id } else { name };
    if name.chars().count() > 128 {
        return Err(invalid("name must be at most 128 characters".to_string()));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(invalid("name contains control characters".to_string()));
    }

    validate_model(&manifest.model).map_err(invalid)?;
    validate_base_url(&manifest.base_url).map_err(invalid)?;
    let api_key_env = if manifest.api_key_env.trim().is_empty() {
        DEFAULT_API_KEY_ENV.to_string()
    } else {
        manifest.api_key_env.trim().to_string()
    };
    validate_api_key_env(&api_key_env).map_err(invalid)?;

    let channel = match manifest.channel.trim() {
        "" => ChannelKind::None,
        value => ChannelKind::parse(value)
            .ok_or_else(|| invalid(format!("unsupported channel {value:?}")))?,
    };

    let mem_limit_mb = if manifest.mem_limit_mb == 0 {
        DEFAULT_MEM_LIMIT_MB
    } else {
        manifest.mem_limit_mb
    };
    validate_mem_limit(mem_limit_mb).map_err(invalid)?;
    let cpu_limit = if manifest.cpu_limit <= 0.0 {
        DEFAULT_CPU_LIMIT
    } else {
        manifest.cpu_limit
    };
    validate_cpu_limit(cpu_limit).map_err(invalid)?;

    validate_notes(&manifest.notes).map_err(invalid)?;

    let mut shares = Vec::with_capacity(manifest.shares.len());
    for share in &manifest.shares {
        validate_share_name(&share.name).map_err(invalid)?;
        if shares
            .iter()
            .any(|existing: &ShareRef| existing.name == share.name)
        {
            return Err(invalid(format!("share \"{}\" is listed twice", share.name)));
        }
        shares.push(share.clone());
    }
    shares.sort_by(|left, right| left.name.cmp(&right.name));

    let now = now_iso();
    Ok(InstanceSpec {
        id: id.to_string(),
        name: name.to_string(),
        template: manifest.template.trim().to_string(),
        model: manifest.model.trim().to_string(),
        base_url: manifest.base_url.trim().to_string(),
        api_key_env,
        provider: manifest.provider.clone(),
        channel,
        shares,
        mem_limit_mb,
        cpu_limit,
        image: String::new(),
        previous_image: None,
        allow_lan: manifest.allow_lan,
        heal: crate::models::HealPolicy::Off,
        unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
        max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
        heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
        notes: manifest.notes.clone(),
        host: crate::hosts::LOCAL_HOST.to_string(),
        status: Status::Created,
        last_error: None,
        created_at: now.clone(),
        updated_at: now,
    })
}

/// Environment variable names an archived compose file declares as `${KEY}`.
///
/// The import re-renders the compose file with this build's shape (no
/// `env_file`, no literal value) but keeps the interpolation names, so a PATCH
/// with the tokens restores the exact same environment.
pub fn compose_env_keys(compose: &str) -> Vec<String> {
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(compose) else {
        return Vec::new();
    };
    let Some(services) = doc.get("services").and_then(|value| value.as_mapping()) else {
        return Vec::new();
    };

    let mut keys = BTreeSet::new();
    for service in services.values() {
        let Some(environment) = service.get("environment") else {
            continue;
        };
        let values: Vec<&str> = match environment {
            serde_yaml::Value::Mapping(map) => map
                .values()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>(),
            serde_yaml::Value::Sequence(items) => items
                .iter()
                .filter_map(|item| item.as_str())
                .filter_map(|item| item.split_once('=').map(|(_, value)| value))
                .collect(),
            _ => Vec::new(),
        };
        for value in values {
            let Some(inner) = value
                .trim()
                .strip_prefix("${")
                .and_then(|rest| rest.strip_suffix('}'))
            else {
                continue;
            };
            if validate_secret_name(inner).is_ok() {
                keys.insert(inner.to_string());
            }
        }
    }
    keys.into_iter().collect()
}

/// Path handed to a template renderer: the built-in when it still exists, the
/// on-disk config (`custom`) otherwise.
pub fn settle_template(cfg: &Config, template: &str) -> String {
    let candidate = if template.is_empty() {
        "custom"
    } else {
        template
    };
    if crate::render::load_template(cfg, candidate).is_ok() {
        candidate.to_string()
    } else {
        "custom".to_string()
    }
}

/// Destination of the data directory of an import (staging path).
pub fn staging_data_dir(root: &Path, token: &str) -> PathBuf {
    root.join("tmp")
        .join(format!("import-{token}"))
        .join(DATA_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ShareMode, GATE_TOKEN_ENV};
    use std::io::Cursor;

    fn temp_config() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().join("root"));
        crate::store::ensure_layout(&cfg).expect("layout");
        (dir, cfg)
    }

    fn spec() -> InstanceSpec {
        let now = now_iso();
        InstanceSpec {
            id: "alice".to_string(),
            name: "Alice".to_string(),
            template: "guest".to_string(),
            model: "deepseek-v4.1-flash".to_string(),
            base_url: "https://example.test/v1".to_string(),
            api_key_env: GATE_TOKEN_ENV.to_string(),
            provider: None,
            channel: ChannelKind::Telegram,
            shares: vec![ShareRef {
                name: "findings".to_string(),
                mode: ShareMode::Ro,
            }],
            mem_limit_mb: 2048,
            cpu_limit: 1.5,
            image: "hermes-agent:0.22.2-local".to_string(),
            previous_image: Some("hermes-agent:0.21.0".to_string()),
            allow_lan: true,
            heal: crate::models::HealPolicy::Off,
            unhealthy_after: crate::models::DEFAULT_UNHEALTHY_AFTER,
            max_restarts: crate::models::DEFAULT_MAX_RESTARTS,
            heal_backoff_s: crate::models::DEFAULT_HEAL_BACKOFF_S,
            notes: "ported from staging".to_string(),
            host: crate::hosts::LOCAL_HOST.to_string(),
            status: Status::Running,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    #[test]
    fn export_then_extract_round_trips_the_data() {
        let (_dir, cfg) = temp_config();
        let spec = spec();
        crate::store::write_instance(
            &cfg,
            &spec,
            &crate::render::RenderedInstance {
                compose_yaml: "name: fleett-alice\nservices:\n  agent:\n    image: agent:1\n"
                    .to_string(),
                config_yaml: "model: {}\n".to_string(),
                soul_md: Some("soul\n".to_string()),
            },
            true,
        )
        .expect("write instance");
        std::fs::create_dir_all(cfg.data_dir("alice").join("sessions")).expect("tree");
        std::fs::write(cfg.data_dir("alice").join("sessions/one.json"), "{}").expect("session");
        std::fs::write(cfg.secrets_path("alice"), "TOKEN=leak\n").expect("legacy secret");

        let bytes = export(&cfg, &spec).expect("export");
        assert!(
            !String::from_utf8_lossy(&bytes).contains("leak"),
            "the archive must not carry plaintext secrets"
        );

        let target = cfg.root.join("extract");
        let extraction =
            extract(Cursor::new(bytes), &target).expect("extract the exported archive");
        assert_eq!(extraction.manifest.instance.id, "alice");
        assert_eq!(extraction.manifest.instance.mem_limit_mb, 2048);
        assert_eq!(extraction.manifest.instance.cpu_limit, 1.5);
        assert!(extraction.manifest.instance.allow_lan);
        assert_eq!(extraction.manifest.instance.name, "Alice");
        assert_eq!(
            extraction.data_entries, 4,
            "config, SOUL, healthcheck, session"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("sessions/one.json")).expect("session"),
            "{}"
        );
        assert!(target.join("SOUL.md").is_file());
        assert!(!target.join("secrets.env").exists());
        assert!(extraction
            .compose_yaml
            .as_deref()
            .is_some_and(|text| text.contains("fleett-alice")));
    }

    #[test]
    fn a_manifest_with_a_foreign_format_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut builder = tar::Builder::new(archive::gzip_writer());
        let manifest = serde_json::json!({
            "format": "something.else",
            "format_version": 1,
            "created_at": now_iso(),
            "fleett_version": VERSION,
            "instance": serde_json::to_value(ArchiveInstance::from_spec(&spec())).expect("spec"),
        });
        archive::append_bytes(
            &mut builder,
            Path::new(MANIFEST_NAME),
            manifest.to_string().as_bytes(),
            0o600,
        )
        .expect("manifest");
        let bytes = builder.into_inner().expect("tar").finish().expect("gzip");

        let err = extract(Cursor::new(bytes), &dir.path().join("data")).expect_err("refused");
        assert!(matches!(err, ArchiveError::Unsupported { .. }), "{err}");
    }

    #[test]
    fn an_archive_without_a_manifest_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut builder = tar::Builder::new(archive::gzip_writer());
        archive::append_bytes(&mut builder, Path::new("data/config.yaml"), b"{}", 0o600)
            .expect("entry");
        let bytes = builder.into_inner().expect("tar").finish().expect("gzip");
        let err = extract(Cursor::new(bytes), &dir.path().join("data")).expect_err("no manifest");
        assert!(matches!(err, ArchiveError::MissingManifest), "{err}");
    }

    #[test]
    fn zip_slip_entries_are_rejected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut builder = tar::Builder::new(archive::gzip_writer());
        archive::append_bytes(
            &mut builder,
            Path::new(MANIFEST_NAME),
            serde_json::to_vec(&serde_json::json!({
                "format": ARCHIVE_FORMAT,
                "format_version": FORMAT_VERSION,
                "created_at": now_iso(),
                "fleett_version": VERSION,
                "instance": serde_json::to_value(ArchiveInstance::from_spec(&spec())).expect("spec"),
            }))
            .expect("json")
            .as_slice(),
            0o600,
        )
        .expect("manifest");
        // The tar writer refuses `..` itself, so write the raw name field: the
        // point is precisely to hand a hostile archive to the extractor.
        let mut header = tar::Header::new_gnu();
        header.set_size(2);
        header.set_mode(0o600);
        header.set_entry_type(tar::EntryType::Regular);
        {
            let name = b"../../evil";
            header.as_mut_bytes()[..name.len()].copy_from_slice(name);
        }
        header.set_cksum();
        builder.append(&header, &b"x\n"[..]).expect("unsafe entry");
        let bytes = builder.into_inner().expect("tar").finish().expect("gzip");

        let target = dir.path().join("data");
        let err = extract(Cursor::new(bytes), &target).expect_err("unsafe path");
        assert!(matches!(err, ArchiveError::Invalid(_)), "{err}");
        assert!(!dir.path().join("evil").exists());
    }

    #[test]
    fn specs_are_built_from_the_manifest_metadata() {
        let (_dir, cfg) = temp_config();
        let manifest = ArchiveInstance::from_spec(&spec());
        let imported = spec_from_manifest(&manifest, "bob").expect("spec");
        assert_eq!(imported.id, "bob");
        assert_eq!(imported.name, "Alice");
        assert_eq!(imported.mem_limit_mb, 2048);
        assert_eq!(imported.cpu_limit, 1.5);
        assert!(imported.allow_lan);
        assert_eq!(imported.shares[0].mode, ShareMode::Ro);
        assert_eq!(imported.status, Status::Created);
        assert_eq!(settle_template(&cfg, "guest"), "guest");
        assert_eq!(settle_template(&cfg, "gone"), "custom");
    }

    #[test]
    fn invalid_manifest_values_are_refused() {
        let mut manifest = ArchiveInstance::from_spec(&spec());
        manifest.mem_limit_mb = 1;
        assert!(spec_from_manifest(&manifest, "bob").is_err());

        let mut manifest = ArchiveInstance::from_spec(&spec());
        manifest.cpu_limit = 99.0;
        assert!(spec_from_manifest(&manifest, "bob").is_err());

        let mut manifest = ArchiveInstance::from_spec(&spec());
        manifest.channel = "carrier-pigeon".to_string();
        assert!(spec_from_manifest(&manifest, "bob").is_err());

        let mut manifest = ArchiveInstance::from_spec(&spec());
        manifest.shares.push(manifest.shares[0].clone());
        assert!(spec_from_manifest(&manifest, "bob").is_err());
    }

    #[test]
    fn interpolation_names_are_read_back_from_a_compose_file() {
        let composed = "name: fleett-alice\n\
                        services:\n\
                        \x20 agent:\n\
                        \x20   environment:\n\
                        \x20     HERMES_UID: '10000'\n\
                        \x20     TELEGRAM_BOT_TOKEN: ${TELEGRAM_BOT_TOKEN}\n\
                        \x20     HERMES_GATE_PROFILE_TOKEN: 'literal'\n\
                        \x20 other:\n\
                        \x20   environment:\n\
                        \x20     - EXTRA=${EXTRA}\n\
                        \x20     - NOT_A_SECRET=plain\n";
        assert_eq!(
            compose_env_keys(composed),
            vec!["EXTRA".to_string(), "TELEGRAM_BOT_TOKEN".to_string()]
        );
        assert!(compose_env_keys("not: [valid").is_empty());
    }
}
