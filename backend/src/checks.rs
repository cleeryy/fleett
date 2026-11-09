//! Template environment checks (issue #27).
//!
//! A template can attach a format to the variables it suggests, and mark the
//! allowlist keys that make the gateway refuse every message when absent. This
//! module owns the format validation used by the write endpoints and the
//! warnings returned by `GET /api/instances/{id}/check` and the instance
//! detail (`warnings`).

use crate::config::Config;
use crate::envs::EnvValue;
use crate::models::{
    validate_env_value, ChannelKind, EnvEntry, EnvSuggestion, Health, InstanceSpec, TemplateInfo,
};
use crate::render;
use serde::Serialize;
use std::collections::BTreeMap;

/// Code of the warning emitted when a required allowlist key is absent/empty.
pub const ALLOWLIST_MISSING: &str = "allowlist_missing";
/// Code of the warning emitted when the container is healthy but a required
/// allowlist key is missing (the gateway is up and refusing every message).
pub const GATEWAY_MAY_REFUSE: &str = "gateway_may_refuse";

/// One operator-facing warning about the template environment of an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvWarning {
    /// Stable machine-readable code (`allowlist_missing`, `gateway_may_refuse`).
    pub code: String,
    /// Environment variable the warning is about.
    pub key: String,
    /// Human-readable explanation, ready to display.
    pub message: String,
    /// Form field to focus in the console (`env.<KEY>`).
    pub field: String,
}

/// Body of `GET /api/instances/{id}/check`.
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub ok: bool,
    pub warnings: Vec<EnvWarning>,
    /// Id of the template that was checked (same value as `InstanceDetail.template`).
    pub template: String,
}

/// A value refused by the format a template attached to its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFormatError {
    pub key: String,
    pub expected: String,
    pub received: String,
    pub reason: String,
}

impl EnvFormatError {
    pub fn message(&self) -> String {
        format!(
            "environment variable \"{}\" must match format \"{}\": {}",
            self.key, self.expected, self.reason
        )
    }
}

/// API view of the template of an instance.
///
/// An adopted (`custom`) template has no metadata: the synthesized entry keeps
/// the id and exposes no suggestion, so `/check` always returns an object.
pub fn template_info(cfg: &Config, spec: &InstanceSpec) -> TemplateInfo {
    match render::load_template(cfg, &spec.template) {
        Ok(template) => {
            let meta = template.meta;
            TemplateInfo {
                id: spec.template.clone(),
                name: meta.name.unwrap_or_else(|| spec.template.clone()),
                description: meta.description.unwrap_or_default(),
                default_model: meta.default_model.unwrap_or_else(|| spec.model.clone()),
                channel: meta
                    .channel
                    .unwrap_or_else(|| spec.channel.as_str().to_string()),
                env: meta.env.unwrap_or_default(),
                parent: meta.parent,
                read_only: template.read_only,
                defaults: template.defaults,
                source_instance: meta.source_instance,
                instances_count: 0,
                created_at: String::new(),
                updated_at: String::new(),
            }
        }
        Err(_) => TemplateInfo {
            id: spec.template.clone(),
            name: spec.template.clone(),
            description: String::new(),
            default_model: spec.model.clone(),
            channel: spec.channel.as_str().to_string(),
            env: Vec::new(),
            parent: None,
            read_only: false,
            defaults: crate::models::TemplateDefaults::default(),
            source_instance: None,
            instances_count: 0,
            created_at: String::new(),
            updated_at: String::new(),
        },
    }
}

/// Validate the values a request supplies against the template formats.
///
/// Only keys a suggestion associates with a `format` are checked; unknown keys
/// and suggestions without a format are left alone. An unknown format is not
/// enforced by [`validate_env_value`], so a newer template never blocks an
/// older control plane.
pub fn validate_values<'a, I>(
    suggestions: &[EnvSuggestion],
    values: I,
) -> Result<(), EnvFormatError>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    for (key, value) in values {
        let Some(format) = suggestions
            .iter()
            .find(|suggestion| suggestion.key == key)
            .and_then(|suggestion| suggestion.format.as_deref())
        else {
            continue;
        };
        if let Err(reason) = validate_env_value(format, value) {
            return Err(EnvFormatError {
                key: key.to_string(),
                expected: format.to_string(),
                received: value.to_string(),
                reason,
            });
        }
    }
    Ok(())
}

/// Warnings of one instance, from its template and its current values.
///
/// `has_value` tells whether a key holds a non-empty value; a secret key is
/// considered present (the control plane cannot read it back here).
pub fn warnings<F>(
    template: &TemplateInfo,
    channel: ChannelKind,
    health: Health,
    has_value: F,
) -> Vec<EnvWarning>
where
    F: Fn(&str) -> bool,
{
    let mut out = Vec::new();
    for suggestion in &template.env {
        if !suggestion.required || !applies_to_channel(&suggestion.key, channel) {
            continue;
        }
        if has_value(&suggestion.key) {
            continue;
        }
        out.push(EnvWarning {
            code: ALLOWLIST_MISSING.to_string(),
            key: suggestion.key.clone(),
            message: format!(
                "{} is required by the {} template but is not set: the gateway will refuse every \
                 message until it is",
                suggestion.key, template.id
            ),
            field: field_id(&suggestion.key),
        });
        if health == Health::Healthy {
            out.push(EnvWarning {
                code: GATEWAY_MAY_REFUSE.to_string(),
                key: suggestion.key.clone(),
                message: format!(
                    "the container is healthy but {} is empty: the gateway is probably refusing \
                     every message",
                    suggestion.key
                ),
                field: field_id(&suggestion.key),
            });
        }
    }
    out
}

/// `GET /api/instances/{id}/check` body.
pub fn report(cfg: &Config, spec: &InstanceSpec, health: Health, env: &[EnvValue]) -> CheckReport {
    let template = template_info(cfg, spec);
    let warnings = warnings(&template, spec.channel, health, |key| {
        env.iter()
            .any(|entry| entry.key == key && !entry.value.trim().is_empty())
    });
    CheckReport {
        ok: warnings.is_empty(),
        warnings,
        template: template.id,
    }
}

/// Warning messages appended to `InstanceDetail.warnings`.
pub fn detail_warnings(
    template: &TemplateInfo,
    channel: ChannelKind,
    health: Health,
    env: &[EnvEntry],
) -> Vec<String> {
    warnings(template, channel, health, |key| {
        env.iter().any(|entry| {
            entry.key == key
                && (entry.secret
                    || entry
                        .value
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty()))
        })
    })
    .into_iter()
    .map(|warning| warning.message)
    .collect()
}

/// True when a stored environment map holds a non-empty value for `key`.
pub fn has_value(env: &BTreeMap<String, String>, key: &str) -> bool {
    env.get(key).is_some_and(|value| !value.trim().is_empty())
}

/// Console form field of one environment variable (`env.DISCORD_ALLOWED_USERS`).
fn field_id(key: &str) -> String {
    format!("env.{key}")
}

/// A platform-specific allowlist only matters for its own channel.
fn applies_to_channel(key: &str, channel: ChannelKind) -> bool {
    if key.starts_with("TELEGRAM_") {
        channel == ChannelKind::Telegram
    } else if key.starts_with("DISCORD_") {
        channel == ChannelKind::Discord
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(env: Vec<EnvSuggestion>) -> TemplateInfo {
        TemplateInfo {
            id: "guest-discord".to_string(),
            name: "Guest (Discord)".to_string(),
            description: String::new(),
            default_model: "deepseek-v4.1-flash".to_string(),
            channel: "discord".to_string(),
            env,
            parent: None,
            read_only: true,
            defaults: crate::models::TemplateDefaults::default(),
            source_instance: None,
            instances_count: 0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn suggestion(key: &str, required: bool, format: Option<&str>) -> EnvSuggestion {
        EnvSuggestion {
            key: key.to_string(),
            description: String::new(),
            secret: false,
            example: None,
            required,
            format: format.map(str::to_string),
        }
    }

    #[test]
    fn a_missing_required_allowlist_warns_even_when_healthy() {
        let template = template(vec![
            suggestion("DISCORD_ALLOWED_USERS", true, Some("csv-of-ids")),
            suggestion("DISCORD_ALLOWED_ROLES", false, Some("csv-of-ids")),
        ]);
        let found = warnings(&template, ChannelKind::Discord, Health::Healthy, |_| false);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].code, ALLOWLIST_MISSING);
        assert_eq!(found[0].key, "DISCORD_ALLOWED_USERS");
        assert_eq!(found[0].field, "env.DISCORD_ALLOWED_USERS");
        assert!(found[0].message.contains("DISCORD_ALLOWED_USERS"));
        assert_eq!(found[1].code, GATEWAY_MAY_REFUSE);
        assert_eq!(found[1].key, "DISCORD_ALLOWED_USERS");

        // A stopped container is not "healthy": the gateway cannot refuse yet.
        let found = warnings(&template, ChannelKind::Discord, Health::Unknown, |_| false);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].code, ALLOWLIST_MISSING);
    }

    #[test]
    fn an_empty_value_is_not_a_configured_allowlist() {
        let template = template(vec![suggestion(
            "DISCORD_ALLOWED_USERS",
            true,
            Some("csv-of-ids"),
        )]);
        let values = BTreeMap::from([("DISCORD_ALLOWED_USERS".to_string(), "  ".to_string())]);
        let found = warnings(&template, ChannelKind::Discord, Health::Healthy, |key| {
            has_value(&values, key)
        });
        assert_eq!(found.len(), 2);

        let values = BTreeMap::from([(
            "DISCORD_ALLOWED_USERS".to_string(),
            "274575374111866880".to_string(),
        )]);
        assert!(
            warnings(&template, ChannelKind::Discord, Health::Healthy, |key| {
                has_value(&values, key)
            })
            .is_empty()
        );
    }

    #[test]
    fn only_the_allowlist_of_the_instance_channel_is_checked() {
        let template = template(vec![
            suggestion("TELEGRAM_ALLOWED_USERS", true, Some("csv-of-ids")),
            suggestion("DISCORD_ALLOWED_USERS", true, Some("csv-of-ids")),
        ]);
        let found = warnings(&template, ChannelKind::Telegram, Health::Healthy, |_| false);
        let keys: Vec<&str> = found.iter().map(|warning| warning.key.as_str()).collect();
        assert_eq!(keys, ["TELEGRAM_ALLOWED_USERS", "TELEGRAM_ALLOWED_USERS"]);

        let found = warnings(&template, ChannelKind::None, Health::Healthy, |_| false);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn value_formats_are_checked_only_for_suggested_keys() {
        let suggestions = vec![
            suggestion("DISCORD_ALLOWED_USERS", true, Some("csv-of-ids")),
            suggestion("DISCORD_ALLOW_ALL_USERS", false, Some("bool")),
            suggestion("PLAIN", false, None),
        ];
        assert!(validate_values(
            &suggestions,
            [
                ("DISCORD_ALLOWED_USERS", "274575374111866880, 123"),
                ("DISCORD_ALLOW_ALL_USERS", "false"),
                ("PLAIN", ""),
                ("UNKNOWN_KEY", "anything"),
            ],
        )
        .is_ok());

        let err = validate_values(&suggestions, [("DISCORD_ALLOWED_USERS", "alice")])
            .expect_err("not a numeric id");
        assert_eq!(err.key, "DISCORD_ALLOWED_USERS");
        assert_eq!(err.expected, "csv-of-ids");
        assert_eq!(err.received, "alice");
        assert!(err.message().contains("csv-of-ids"));

        let err = validate_values(&suggestions, [("DISCORD_ALLOW_ALL_USERS", "maybe")])
            .expect_err("not a boolean");
        assert_eq!(err.expected, "bool");
    }
}
