//! First-run steps behind `GET /api/onboarding` (issue #52).
//!
//! A fresh host has no Docker access, no agent image, no provider and no
//! instance; the console used to show an empty fleet without saying why. The
//! step list makes the four prerequisites explicit, in the order they have to
//! be satisfied, and carries the hint the console needs to act (`action`).
//!
//! The list is derived from the live state, never stored: creating the first
//! instance marks the tour complete, and deleting the last one brings it back.

use crate::config::Config;
use crate::db::Db;
use crate::doctor;
use crate::driver::Driver;
use serde::Serialize;

/// Body of `GET /api/onboarding`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Onboarding {
    /// `true` as soon as one instance exists: the console stops showing the tour.
    pub complete: bool,
    pub steps: Vec<Step>,
}

/// One prerequisite of the first-run tour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    /// Stable id (`docker`, `image`, `provider`, `instance`).
    pub id: &'static str,
    pub title: &'static str,
    /// Why the step matters, ready to display.
    pub description: String,
    pub done: bool,
    /// What the console offers when the step is not done.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
}

/// Console hint attached to an unfinished step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Action {
    /// Navigate to an internal page (`{"kind":"link","to":"/providers?new=1"}`).
    Link { to: String },
    /// Run this command on the host (`{"kind":"command","command":"…"}`).
    Command { command: String },
}

/// Evaluate the four steps against the live state.
///
/// Driver and database failures mark the step undone instead of failing the
/// request: the tour is a hint, and an unreachable engine must not blank the
/// console.
pub async fn report(cfg: &Config, db: &Db, driver: &dyn Driver) -> Onboarding {
    let docker_done = driver.ping().await;
    let image_done = doctor::image_present(cfg, driver)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "onboarding cannot list the images");
            false
        });
    let providers = db
        .list_providers()
        .map(|rows| rows.len())
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "onboarding cannot count the providers");
            0
        });
    let instances = db.list().map(|rows| rows.len()).unwrap_or_else(|err| {
        tracing::warn!(%err, "onboarding cannot count the instances");
        0
    });

    let steps = vec![
        Step {
            id: "docker",
            title: "Check Docker",
            description: "fleett drives Docker Compose; the engine must answer on this host."
                .to_string(),
            done: docker_done,
            action: (!docker_done).then(|| Action::Command {
                command: "sudo systemctl start docker && docker info".to_string(),
            }),
        },
        Step {
            id: "image",
            title: "Build the agent image",
            description: format!(
                "The agent image \"{}\" (FLEETT_IMAGE) must exist in the local image store.",
                cfg.image
            ),
            done: image_done,
            action: (!image_done).then(|| Action::Command {
                command: doctor::build_hint(&cfg.image),
            }),
        },
        Step {
            id: "provider",
            title: "Create a provider",
            description: "A provider stores one API credential the instances reuse; its test \
                          button checks the key before the first instance starts."
                .to_string(),
            done: providers > 0,
            action: (providers == 0).then(|| Action::Link {
                to: "/providers?new=1".to_string(),
            }),
        },
        Step {
            id: "instance",
            title: "Create your first instance",
            description: "Pick a template, attach the provider and start the agent; the instance \
                          page shows its health, logs and allowlist variables."
                .to_string(),
            done: instances > 0,
            action: (instances == 0).then(|| Action::Link {
                to: "/instances?new=1".to_string(),
            }),
        },
    ];

    Onboarding {
        complete: instances > 0,
        steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_serialise_to_the_console_contract() {
        let link = serde_json::to_value(Action::Link {
            to: "/providers?new=1".to_string(),
        })
        .expect("action");
        assert_eq!(
            link,
            serde_json::json!({"kind": "link", "to": "/providers?new=1"})
        );

        let command = serde_json::to_value(Action::Command {
            command: "docker build -t x .".to_string(),
        })
        .expect("action");
        assert_eq!(
            command,
            serde_json::json!({"kind": "command", "command": "docker build -t x ."})
        );
    }

    #[test]
    fn a_done_step_has_no_action() {
        let step = Step {
            id: "docker",
            title: "Check Docker",
            description: "…".to_string(),
            done: true,
            action: None,
        };
        let value = serde_json::to_value(step).expect("step");
        assert!(value.get("action").is_none());
    }
}
