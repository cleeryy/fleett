//! Built-in instance templates.
//!
//! Every template is a normal Hermes `config.yaml` (derived from
//! `templates-reference/cli-config.yaml.example`) plus a `SOUL.md`. Rendering
//! substitutes `{{MODEL}}`, `{{BASE_URL}}`, `{{AGENT_NAME}}` and
//! `{{API_KEY_ENV}}`; the model block is normalised by the renderer.

/// One environment variable a built-in template expects.
///
/// The console pre-fills `example` and validates the value against `format`;
/// `required` keys are the allowlists that make the gateway refuse every
/// message when they are absent (issue #27).
pub struct BuiltinEnv {
    pub key: &'static str,
    pub description: &'static str,
    pub example: Option<&'static str>,
    pub required: bool,
    pub format: Option<&'static str>,
}

/// One built-in template and its API metadata.
pub struct BuiltinTemplate {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub default_model: &'static str,
    pub channel: &'static str,
    /// Environment variables the template expects.
    pub env: &'static [BuiltinEnv],
    pub config_yaml: &'static str,
    pub soul_md: &'static str,
}

const GUEST_CONFIG: &str = r#"# fleett built-in template: guest (Telegram)
# Placeholders: {{MODEL}} {{BASE_URL}} {{AGENT_NAME}} {{API_KEY_ENV}}
_config_version: 50

database:
  journal_mode: "wal"

model:
  default: "{{MODEL}}"
  provider: custom
  base_url: "{{BASE_URL}}"
  api_key: "${{{API_KEY_ENV}}}"
  api_mode: chat_completions

agent:
  max_turns: 60

memory:
  memory_enabled: true
  user_profile_enabled: true
  nudge_interval: 10

compression:
  enabled: true
  threshold: 0.50

skills:
  creation_nudge_interval: 15

platform_toolsets:
  cli: [hermes-cli]
  telegram: [web, file, vision, skills, todo, memory]
  discord: [web, file, vision, skills, todo, memory]

platforms:
  telegram:
    enabled: true
  discord:
    enabled: false
"#;

const GUEST_SOUL: &str = r#"# {{AGENT_NAME}}

You are {{AGENT_NAME}}, a guest agent hosted on fleett.

- You run inside your own container. The host, the Docker socket and the
  management network are out of reach; do not claim otherwise.
- Use the folders under /opt/data/shared for anything you want to hand over to
  another agent.
- Prefer short, verifiable answers. Say that you do not know instead of
  guessing.
- Ask before doing anything destructive or expensive.
"#;

const DISCORD_CONFIG: &str = r#"# fleett built-in template: guest-discord
# Placeholders: {{MODEL}} {{BASE_URL}} {{AGENT_NAME}} {{API_KEY_ENV}}
_config_version: 50

database:
  journal_mode: "wal"

model:
  default: "{{MODEL}}"
  provider: custom
  base_url: "{{BASE_URL}}"
  api_key: "${{{API_KEY_ENV}}}"
  api_mode: chat_completions

agent:
  max_turns: 60

memory:
  memory_enabled: true
  user_profile_enabled: true
  nudge_interval: 10

compression:
  enabled: true
  threshold: 0.50

skills:
  creation_nudge_interval: 15

platform_toolsets:
  cli: [hermes-cli]
  telegram: [web, file, vision, skills, todo, memory]
  discord: [web, file, vision, skills, todo, memory]

platforms:
  telegram:
    enabled: false
  discord:
    enabled: true
"#;

const DISCORD_SOUL: &str = r#"# {{AGENT_NAME}}

You are {{AGENT_NAME}}, a guest agent hosted on fleett and reachable on
Discord.

- You run inside your own container. The host, the Docker socket and the
  management network are out of reach; do not claim otherwise.
- Answer in the thread where you were mentioned, keep replies compact, and
  avoid long unformatted dumps.
- Use the folders under /opt/data/shared for shared work with other agents.
- Ask before doing anything destructive or expensive.
"#;

const SOLO_CONFIG: &str = r#"# fleett built-in template: solo (no messaging platform)
# Placeholders: {{MODEL}} {{BASE_URL}} {{AGENT_NAME}} {{API_KEY_ENV}}
_config_version: 50

database:
  journal_mode: "wal"

model:
  default: "{{MODEL}}"
  provider: custom
  base_url: "{{BASE_URL}}"
  api_key: "${{{API_KEY_ENV}}}"
  api_mode: chat_completions

agent:
  max_turns: 120

memory:
  memory_enabled: true
  user_profile_enabled: true
  nudge_interval: 10

compression:
  enabled: true
  threshold: 0.50

skills:
  creation_nudge_interval: 15

platform_toolsets:
  cli: [hermes-cli]

platforms:
  telegram:
    enabled: false
  discord:
    enabled: false
"#;

const SOLO_SOUL: &str = r#"# {{AGENT_NAME}}

You are {{AGENT_NAME}}, a Hermes agent hosted on fleett without a
messaging platform. You are driven from the command line or from the Hermes
API server.

- You run inside your own container. The host, the Docker socket and the
  management network are out of reach; do not claim otherwise.
- Work in /opt/data, and use /opt/data/shared for shared folders.
- Ask before doing anything destructive or expensive.
"#;

const fn env(
    key: &'static str,
    description: &'static str,
    example: Option<&'static str>,
    required: bool,
    format: Option<&'static str>,
) -> BuiltinEnv {
    BuiltinEnv {
        key,
        description,
        example,
        required,
        format,
    }
}

const GUEST_ENV: &[BuiltinEnv] = &[
    env(
        "TELEGRAM_ALLOWED_USERS",
        "Comma-separated Telegram user ids allowed to talk to the agent; without it every \
         message is denied.",
        Some("123456789"),
        true,
        Some("csv-of-ids"),
    ),
    env(
        "GATEWAY_ALLOW_ALL_USERS",
        "Set to true to accept every user on the platform, with no allowlist. Use with care.",
        Some("false"),
        false,
        Some("bool"),
    ),
];

const GUEST_DISCORD_ENV: &[BuiltinEnv] = &[
    env(
        "DISCORD_ALLOWED_USERS",
        "Comma-separated Discord user ids allowed to talk to the agent; without it every \
         message is denied. Right-click a user in Discord (Developer Mode) and copy its id.",
        Some("274575374111866880"),
        true,
        Some("csv-of-ids"),
    ),
    env(
        "DISCORD_ALLOWED_ROLES",
        "Comma-separated Discord role ids allowed to talk to the agent.",
        Some("987654321098765432"),
        false,
        Some("csv-of-ids"),
    ),
    env(
        "DISCORD_ALLOWED_CHANNELS",
        "Comma-separated Discord channel ids where the agent answers.",
        Some("123456789012345678"),
        false,
        Some("csv-of-ids"),
    ),
    env(
        "DISCORD_ALLOW_ALL_USERS",
        "Set to true to accept every Discord user, with no allowlist. Use with care.",
        Some("false"),
        false,
        Some("bool"),
    ),
];

const GUEST: BuiltinTemplate = BuiltinTemplate {
    id: "guest",
    name: "Guest",
    description: "Web, files and memory only. No infrastructure access.",
    default_model: "deepseek-v4.1-flash",
    channel: "telegram",
    env: GUEST_ENV,
    config_yaml: GUEST_CONFIG,
    soul_md: GUEST_SOUL,
};

const GUEST_DISCORD: BuiltinTemplate = BuiltinTemplate {
    id: "guest-discord",
    name: "Guest (Discord)",
    description: "Same restrictions as guest, connected to Discord.",
    default_model: "deepseek-v4.1-flash",
    channel: "discord",
    env: GUEST_DISCORD_ENV,
    config_yaml: DISCORD_CONFIG,
    soul_md: DISCORD_SOUL,
};

const SOLO: BuiltinTemplate = BuiltinTemplate {
    id: "solo",
    name: "Solo",
    description: "No messaging platform. Driven from the CLI or the Hermes API.",
    default_model: "deepseek-v4.1-flash",
    channel: "none",
    env: &[],
    config_yaml: SOLO_CONFIG,
    soul_md: SOLO_SOUL,
};

pub const BUILTINS: &[BuiltinTemplate] = &[GUEST, GUEST_DISCORD, SOLO];

pub fn find(id: &str) -> Option<&'static BuiltinTemplate> {
    BUILTINS.iter().find(|template| template.id == id)
}
