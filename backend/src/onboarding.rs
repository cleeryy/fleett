//! First-run steps behind `GET /api/onboarding` (issue #52).
//!
//! A fresh host has no Docker access, no agent image, no provider and no
//! instance; the console used to show an empty fleet without saying why. The
//! step list makes the four prerequisites explicit, in the order they have to
//! be satisfied, and carries the hint the console needs to act (`action`).
//!
//! The list is derived from the live state, never stored: creating the first
//! instance marks the tour complete, and deleting the last one brings it back.

use crate::assistant::{self, ProviderReason, ProviderResolution, ProviderSource};
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

/// Evaluate the five steps against the live state.
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
    let assistant_step = assistant_step(cfg, db);

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
        assistant_step,
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

/// The `assistant` step (issues #56, #63).
///
/// The panel is useless until a provider answers it, so the tour names the
/// selection explicitly instead of letting the operator finish with a broken
/// assistant. `complete` does not depend on it: an instance is the finish line.
fn assistant_step(cfg: &Config, db: &Db) -> Step {
    if !cfg.assistant_enabled {
        return Step {
            id: "assistant",
            title: "Select the assistant provider",
            description: "The assistant is disabled (FLEETT_ASSISTANT=off); no provider is \
                          needed."
                .to_string(),
            done: true,
            action: None,
        };
    }
    let link = || Action::Link {
        to: "/providers?assistant=1".to_string(),
    };
    match assistant::resolve_provider(cfg, db) {
        Ok(ProviderResolution::Resolved { provider, source }) => Step {
            id: "assistant",
            title: "Select the assistant provider",
            description: format!(
                "The console assistant drafts with provider \"{}\" ({}).",
                provider.name,
                match source {
                    ProviderSource::Env => "FLEETT_ASSISTANT_PROVIDER",
                    ProviderSource::Setting => "selected in the console",
                    ProviderSource::Auto => "the only provider of the fleet",
                    ProviderSource::None => "no source",
                }
            ),
            done: true,
            action: None,
        },
        Ok(ProviderResolution::Unresolved { reason }) => Step {
            id: "assistant",
            title: "Select the assistant provider",
            description: match reason {
                ProviderReason::NoProvider => {
                    "The assistant needs a provider to answer; create one, then select it for the \
                     assistant."
                        .to_string()
                }
                ProviderReason::NotSelected => {
                    "Several providers exist and none is selected for the assistant: choose the \
                     one it should use."
                        .to_string()
                }
            },
            done: false,
            action: Some(match reason {
                ProviderReason::NoProvider => Action::Link {
                    to: "/providers?new=1".to_string(),
                },
                ProviderReason::NotSelected => link(),
            }),
        },
        Err(assistant::AssistantError::ProviderUnknown { id, .. }) => Step {
            id: "assistant",
            title: "Select the assistant provider",
            description: format!(
                "The assistant points at provider \"{id}\", which no longer exists: select an \
                 existing one."
            ),
            done: false,
            action: Some(link()),
        },
        Err(err) => {
            tracing::warn!(%err, "onboarding cannot resolve the assistant provider");
            Step {
                id: "assistant",
                title: "Select the assistant provider",
                description: "fleett cannot read the assistant configuration; open the providers \
                              page to check the selection."
                    .to_string(),
                done: false,
                action: Some(link()),
            }
        }
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
