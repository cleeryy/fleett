//! Migration from the legacy `hermes-<id>` compose projects.
//!
//! Older releases named every compose project (and every container) after the
//! product: `hermes-<id>`. This build uses `fleett-<id>`. The compose driver
//! reads the project name back from the instance file, so legacy projects keep
//! working untouched; this module offers an explicit, opt-in rewrite:
//!
//! ```text
//! fleett migrate --from hermes-fleet           # print the plan
//! fleett migrate --from hermes-fleet --apply   # down, rewrite, up
//! ```
//!
//! The rewrite itself is a `serde_yaml` round-trip of the compose document, not
//! a string replacement. Instances that were running are restarted; stopped
//! instances only get their files rewritten.

use crate::config::Config;
use crate::db::{self, Db};
use crate::driver::compose::ComposeDriver;
use crate::driver::DriverError;
use crate::models::{self, Status};
use crate::render;
use crate::store::compose_project_name;
use serde_yaml::Value;
use std::io;
use std::path::PathBuf;

/// Source product accepted by `--from`.
pub const SOURCE: &str = "hermes-fleet";

/// Prefix of the compose projects created by previous releases.
pub const LEGACY_PREFIX: &str = "hermes-";

/// External network created by previous releases.
pub const LEGACY_NETWORK: &str = "hermes-fleet";

/// One instance to migrate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationEntry {
    pub id: String,
    pub old_project: String,
    pub new_project: String,
    /// External network the rewritten compose file points at.
    pub new_network: String,
    pub compose_path: PathBuf,
}

/// Project name used by previous releases.
pub fn legacy_project_name(id: &str) -> String {
    format!("{LEGACY_PREFIX}{id}")
}

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("cannot write {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("cannot rewrite {path}: {source}")]
    Rewrite { path: PathBuf, source: RewriteError },
    #[error("{0}")]
    Driver(#[from] DriverError),
}

#[derive(Debug, thiserror::Error)]
pub enum RewriteError {
    #[error("invalid compose YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("the compose document must be a YAML mapping")]
    NotMapping,
}

/// Instances whose compose file still declares `hermes-<id>`.
///
/// Read-only and Docker-free: it only looks at `instances/*/compose.yaml`.
pub fn plan(cfg: &Config) -> Vec<MigrationEntry> {
    let mut entries = Vec::new();
    let Ok(dir) = std::fs::read_dir(cfg.instances_dir()) else {
        return entries;
    };
    for entry in dir.flatten() {
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
        let compose_path = path.join("compose.yaml");
        let Ok(text) = std::fs::read_to_string(&compose_path) else {
            continue;
        };
        let Some(project) = compose_project_name(&text) else {
            continue;
        };
        if project != legacy_project_name(id) {
            continue;
        }
        entries.push(MigrationEntry {
            id: id.to_string(),
            old_project: project,
            new_project: render::project_name(id),
            new_network: cfg.network.clone(),
            compose_path,
        });
    }
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    entries
}

/// Rewrite one compose document: project name, container name, network.
///
/// Only `name`, the `container_name` of services that used the legacy prefix,
/// and an external network still called `hermes-fleet` are touched; everything
/// else is preserved field by field through `serde_yaml`.
pub fn rewrite_compose(text: &str, id: &str, new_network: &str) -> Result<String, RewriteError> {
    let mut doc: Value = serde_yaml::from_str(text)?;
    if !doc.is_mapping() {
        return Err(RewriteError::NotMapping);
    }

    let old_project = legacy_project_name(id);
    let new_project = render::project_name(id);

    if let Value::Mapping(map) = &mut doc {
        map.insert(Value::from("name"), Value::from(new_project.as_str()));
    }

    if let Some(Value::Mapping(services)) = doc.get_mut("services") {
        for service in services.values_mut() {
            let Value::Mapping(service) = service else {
                continue;
            };
            let rename = service
                .get("container_name")
                .and_then(Value::as_str)
                .is_some_and(|name| name == old_project || name.starts_with(LEGACY_PREFIX));
            if rename {
                service.insert(
                    Value::from("container_name"),
                    Value::from(new_project.as_str()),
                );
            }
        }
    }

    if let Some(Value::Mapping(networks)) = doc.get_mut("networks") {
        for network in networks.values_mut() {
            let Value::Mapping(network) = network else {
                continue;
            };
            let legacy = network
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name == LEGACY_NETWORK);
            if legacy {
                network.insert(Value::from("name"), Value::from(new_network));
            }
        }
    }

    serde_yaml::to_string(&doc).map_err(RewriteError::from)
}

/// Result of one applied migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The project was running before the migration.
    pub was_running: bool,
    /// The project was brought back up after the rewrite.
    pub restarted: bool,
}

/// Rename one project: down, rewrite the file, up when it was running.
pub async fn apply(
    driver: &ComposeDriver,
    entry: &MigrationEntry,
) -> Result<Applied, MigrationError> {
    let text =
        std::fs::read_to_string(&entry.compose_path).map_err(|source| MigrationError::Read {
            path: entry.compose_path.clone(),
            source,
        })?;
    let rewritten = rewrite_compose(&text, &entry.id, &entry.new_network).map_err(|source| {
        MigrationError::Rewrite {
            path: entry.compose_path.clone(),
            source,
        }
    })?;

    let was_running = driver
        .project_status(&entry.id, &entry.old_project)
        .await?
        .status
        == Status::Running;

    driver.project_down(&entry.id, &entry.old_project).await?;

    std::fs::write(&entry.compose_path, rewritten).map_err(|source| MigrationError::Write {
        path: entry.compose_path.clone(),
        source,
    })?;

    if was_running {
        driver.project_up(&entry.id, &entry.new_project).await?;
    }

    Ok(Applied {
        was_running,
        restarted: was_running,
    })
}

/// CLI entry point: print the plan, then apply it when requested.
pub async fn run(cfg: &Config, source: &str, apply: bool) -> anyhow::Result<()> {
    if source != SOURCE {
        anyhow::bail!("unsupported --from value {source:?} (supported: {SOURCE})");
    }

    let entries = plan(cfg);
    if entries.is_empty() {
        println!("No legacy {LEGACY_PREFIX}<id> compose project found.");
        return Ok(());
    }

    println!(
        "{} legacy compose project(s) declared as {LEGACY_PREFIX}<id>:",
        entries.len()
    );
    for entry in &entries {
        println!(
            "  {}: {} -> {}  ({})",
            entry.id,
            entry.old_project,
            entry.new_project,
            entry.compose_path.display()
        );
    }

    if !apply {
        println!("Dry run: nothing changed. Re-run with --apply to rename and restart.");
        return Ok(());
    }

    let driver = ComposeDriver::new(cfg);
    driver
        .ensure_network()
        .await
        .map_err(|err| anyhow::anyhow!("could not ensure the {} network: {err}", cfg.network))?;

    let db = Db::open(&cfg.db_path())?;
    db::reconcile(cfg, &db)?;

    let mut failures = 0usize;
    for entry in &entries {
        match self::apply(&driver, entry).await {
            Ok(applied) => {
                let status = if applied.was_running {
                    Status::Running
                } else {
                    Status::Stopped
                };
                db.set_status(&entry.id, status, None)?;
                println!(
                    "  migrated {}: {}{}",
                    entry.id,
                    entry.new_project,
                    if applied.restarted {
                        " (was running, restarted)"
                    } else {
                        " (was stopped, left stopped)"
                    }
                );
            }
            Err(err) => {
                failures += 1;
                eprintln!("  error for {}: {err}", entry.id);
            }
        }
    }
    db::reconcile(cfg, &db)?;

    if failures > 0 {
        anyhow::bail!("{failures} instance(s) could not be migrated");
    }
    println!("Migrated {} instance(s).", entries.len() - failures);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn legacy_compose(id: &str) -> String {
        format!(
            "name: hermes-{id}\n\
             services:\n\
             \x20 agent:\n\
             \x20   image: hermes-agent:0.22.2-local\n\
             \x20   container_name: hermes-{id}\n\
             \x20   restart: unless-stopped\n\
             \x20   environment:\n\
             \x20     HERMES_UID: '10000'\n\
             networks:\n\
             \x20 fleet:\n\
             \x20   external: true\n\
             \x20   name: hermes-fleet\n"
        )
    }

    fn write_instance(cfg: &Config, id: &str, compose: &str) {
        let dir = cfg.instance_dir(id);
        std::fs::create_dir_all(&dir).expect("instance dir");
        std::fs::write(dir.join("compose.yaml"), compose).expect("compose file");
    }

    #[test]
    fn the_planner_only_selects_legacy_projects() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().to_path_buf());

        write_instance(&cfg, "alice", &legacy_compose("alice"));
        write_instance(
            &cfg,
            "bob",
            "name: fleett-bob\nservices:\n  agent:\n    container_name: fleett-bob\n",
        );
        write_instance(&cfg, "carol", "services:\n  agent:\n    image: x\n");
        // A directory whose id is invalid is not an instance.
        write_instance(&cfg, "Bad_Id", &legacy_compose("Bad_Id"));

        let plan = plan(&cfg);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].id, "alice");
        assert_eq!(plan[0].old_project, "hermes-alice");
        assert_eq!(plan[0].new_project, "fleett-alice");
        assert_eq!(plan[0].new_network, "fleett");
        assert_eq!(plan[0].compose_path, cfg.compose_path("alice"));
    }

    #[test]
    fn the_planner_is_empty_without_an_instances_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cfg = Config::for_tests(dir.path().to_path_buf());
        assert!(plan(&cfg).is_empty());
    }

    #[test]
    fn rewrite_renames_the_project_container_and_network() {
        let rewritten =
            rewrite_compose(&legacy_compose("alice"), "alice", "fleett").expect("rewrite");
        let doc: Value = serde_yaml::from_str(&rewritten).expect("parses");

        assert_eq!(doc["name"].as_str(), Some("fleett-alice"));
        assert_eq!(
            doc["services"]["agent"]["container_name"].as_str(),
            Some("fleett-alice")
        );
        assert_eq!(doc["networks"]["fleet"]["name"].as_str(), Some("fleett"));
        // Everything else is preserved.
        assert_eq!(
            doc["services"]["agent"]["image"].as_str(),
            Some("hermes-agent:0.22.2-local")
        );
        assert_eq!(
            doc["services"]["agent"]["environment"]["HERMES_UID"].as_str(),
            Some("10000")
        );
        assert_eq!(
            doc["services"]["agent"]["restart"].as_str(),
            Some("unless-stopped")
        );
        assert!(!rewritten.contains("hermes-alice"));
    }

    #[test]
    fn rewrite_leaves_unrelated_names_alone() {
        let text = "name: hermes-alice\n\
                    services:\n\
                    \x20 agent:\n\
                    \x20   container_name: custom-sidecar\n\
                    \x20 other:\n\
                    \x20   image: busybox\n\
                    networks:\n\
                    \x20 fleet:\n\
                    \x20   external: true\n\
                    \x20   name: my-network\n";
        let rewritten = rewrite_compose(text, "alice", "fleett").expect("rewrite");
        let doc: Value = serde_yaml::from_str(&rewritten).expect("parses");
        assert_eq!(
            doc["services"]["agent"]["container_name"].as_str(),
            Some("custom-sidecar")
        );
        assert!(doc["services"].get("other").is_some());
        assert_eq!(
            doc["networks"]["fleet"]["name"].as_str(),
            Some("my-network")
        );
    }

    #[test]
    fn rewrite_rejects_non_mapping_and_invalid_yaml() {
        assert!(matches!(
            rewrite_compose("- a\n- b\n", "alice", "fleett"),
            Err(RewriteError::NotMapping)
        ));
        assert!(matches!(
            rewrite_compose("name: [unclosed", "alice", "fleett"),
            Err(RewriteError::Yaml(_))
        ));
    }

    #[test]
    fn rewrite_is_idempotent() {
        let once = rewrite_compose(&legacy_compose("alice"), "alice", "fleett").expect("first");
        let twice = rewrite_compose(&once, "alice", "fleett").expect("second");
        assert_eq!(once, twice);
    }
}
