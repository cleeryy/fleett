/** Agent creation wizard lane (#72). */
export const wizardEn: Record<string, string> = {
  // ── Page and progress ─────────────────────────────────────────────────
  "wizard.title": "Create an agent",
  "wizard.description":
    "Four steps to get an agent running. Your answers are saved in this browser until the agent exists.",
  "wizard.advancedConfig": "Advanced configuration",
  "wizard.guidedConfig": "Guided setup",
  "wizard.progressAria": "Creation progress",
  "wizard.saved": "Draft saved",
  "wizard.submit": "Create and start",
  "wizard.created": "Agent {id} created.",
  "wizard.mcpAttachFailed":
    "The agent was created, but MCP server {server} could not be attached: {message}",

  "wizard.resume.aria": "Resume creation",
  "wizard.resume.title": "Resume creating {name}?",
  "wizard.resume.resume": "Resume",
  "wizard.resume.restart": "Start over",
  "wizard.resume.unnamed": "this agent",
  "wizard.resume.secrets": "Tokens are never saved: retype them on the connection step if needed.",

  // ── Steps ─────────────────────────────────────────────────────────────
  "wizard.step.purpose.short": "Purpose",
  "wizard.step.purpose.title": "What is it for?",
  "wizard.step.purpose.description":
    "Pick the closest use. It chooses an agent template; everything stays changeable later.",

  "wizard.step.identity.short": "Identity",
  "wizard.step.identity.title": "Its identity",
  "wizard.step.identity.description":
    "A name people recognise, an id for the host, and who the agent is.",

  "wizard.step.connection.short": "Connection",
  "wizard.step.connection.title": "Its connection",
  "wizard.step.connection.description":
    "The model credential it calls, and how you talk to it.",

  "wizard.step.review.short": "Review",
  "wizard.step.review.title": "Review",
  "wizard.step.review.description":
    "Check the essentials, then create. Advanced options are one click away.",

  // ── Purpose cards ─────────────────────────────────────────────────────
  "wizard.usage.aria": "What is this agent for?",
  "wizard.usage.discord.title": "Discord assistant",
  "wizard.usage.discord.hint":
    "Answers your team in a Discord server, with its own bot and token.",
  "wizard.usage.watch.title": "Watch / summary",
  "wizard.usage.watch.hint":
    "Reads sources on a schedule and keeps a short, durable summary.",
  "wizard.usage.dev.title": "Development",
  "wizard.usage.dev.hint":
    "Runs code, inspects repositories and helps with engineering work.",
  "wizard.usage.other.title": "Something else",
  "wizard.usage.other.hint": "Start from any agent template and tune it yourself.",
  "wizard.usage.otherTemplate": "Agent template",
  "wizard.usage.otherTemplatePlaceholder": "Select a template",
  "wizard.usage.otherTemplateHint":
    "The template provides the configuration and the starting `SOUL.md` and model.",
  "wizard.usage.selected": "Template {name} · default model {model}.",
  "wizard.usage.noTemplatesLead": "No agent template is available yet. Add one from",
  "wizard.usage.noTemplatesAction": "Agent templates",
  "wizard.usage.noTemplatesTail": "on the host, then reload this page.",

  // ── Field errors ──────────────────────────────────────────────────────
  "wizard.errors.templateRequired": "Pick a use or an agent template.",
  "wizard.errors.nameRequired": "Enter a name.",
  "wizard.errors.nameTooLong": "Use 48 characters or fewer.",
  "wizard.errors.idRequired": "The name must produce an id.",
  "wizard.errors.idFormat": "Use 2 to 32 characters: lowercase letters, digits, hyphens.",
  "wizard.errors.idReserved": "This id is reserved.",
  "wizard.errors.modelRequired": "Enter a model.",
  "wizard.errors.tokenRequired": "Enter the bot token.",
  "wizard.errors.memoryRange": "Use 128 to 8192 MB.",
  "wizard.errors.tooManySkills": "An agent mounts at most {max} skills.",

  // ── Identity ──────────────────────────────────────────────────────────
  "wizard.identity.name": "Agent name",
  "wizard.identity.namePlaceholder": "Alice",
  "wizard.identity.slug": "Id",
  "wizard.identity.slugHint":
    "Derived from the name: names the container directory `instances/{id}` and the compose project.",
  "wizard.identity.language": "Answer language",
  "wizard.identity.languageHint":
    "Written into the persona when it exposes a language variable.",
  "wizard.language.fr": "Français",
  "wizard.language.en": "English",
  "wizard.language.valueFr": "français",
  "wizard.language.valueEn": "English",
  "wizard.identity.persona": "Personality",
  "wizard.identity.personaHint":
    "Renders a reusable `SOUL.md` at creation. Values stay on this agent.",
  "wizard.identity.managePersonas": "Manage personalities",
  "wizard.identity.personaVarsOne": "{count} variable to fill in",
  "wizard.identity.personaVarsMany": "{count} variables to fill in",
  "wizard.identity.values": "Values for this agent",
  "wizard.identity.preview": "Preview the personality",
  "wizard.identity.previewHint":
    "Rendered from the saved personality; stored on the agent at creation.",
  "wizard.identity.noPersona":
    "No personality: the agent keeps the SOUL.md its template provides. You can assign one later in Configuration.",
  "wizard.identity.notes": "Notes",
  "wizard.identity.notesPlaceholder": "Who is this agent for, what it may touch.",

  // ── Connection ────────────────────────────────────────────────────────
  "wizard.connection.provider": "Model provider",
  "wizard.connection.providerHint":
    "Share one credential and inherit its base URL and variable name. The model stays per agent.",
  "wizard.connection.providersLoadFailed": "Could not load providers: {message}",
  "wizard.connection.usesProvider": "Uses your provider {name}",
  "wizard.connection.test": "Test",
  "wizard.connection.testing": "Testing…",
  "wizard.connection.testOk": "Test OK · {ms} ms",
  "wizard.connection.testFailed": "Test failed",
  "wizard.connection.noProviderTitle": "No model provider yet",
  "wizard.connection.noProviderHint":
    "An agent needs a credential to call its model. Create one shared provider once; every agent can reuse it.",
  "wizard.connection.createProvider": "Create a provider",
  "wizard.connection.noProviderContinue":
    "You can carry on: the agent will be created, but cannot answer until a provider is attached.",
  "wizard.connection.noProviderSelected":
    "No provider selected: the agent stores its own credential in the advanced options.",
  "wizard.connection.model": "Model",
  "wizard.connection.modelHint": "The template default; type any model your provider serves.",
  "wizard.connection.channel": "Connector",
  "wizard.channel.discord": "Discord",
  "wizard.channel.discordHint": "Bot in a Discord server",
  "wizard.channel.telegram": "Telegram",
  "wizard.channel.telegramHint": "Bot in Telegram",
  "wizard.channel.none": "No connector",
  "wizard.channel.noneHint": "Console and API only",
  "wizard.channelSummary.discord": "Discord assistant",
  "wizard.channelSummary.telegram": "Telegram assistant",
  "wizard.channelSummary.none": "console agent",
  "wizard.connection.botToken": "Bot token",
  "wizard.connection.botTokenHint":
    "Encrypted at rest. The API never returns it. A new agent cannot reuse a stored token.",

  // ── Review ────────────────────────────────────────────────────────────
  "wizard.review.summaryAria": "Summary",
  "wizard.review.personality": "Personality",
  "wizard.review.overrides": "{count} override(s)",
  "wizard.review.nonePersona": "None — keeps the template SOUL.md",
  "wizard.review.channel": "Connector",
  "wizard.review.model": "Model",
  "wizard.review.viaProvider": "via {name}",
  "wizard.review.template": "Agent template",
  "wizard.review.noneGeneric": "None",
  "wizard.review.skills": "Skills",
  "wizard.review.shares": "Shared folders",
  "wizard.review.latest": "latest",
  "wizard.review.mcp": "MCP server",
  "wizard.review.mcpHint":
    "This MCP server is attached right after the agent is created.",
  "wizard.review.advanced": "Advanced options",
  "wizard.review.advancedHint":
    "Memory, network, environment and mounts. The expert form keeps every field.",
  "wizard.review.memory": "Memory limit (MB)",
  "wizard.review.memoryHint": "128 to 8192 MB. 1536 MB suits a chat assistant.",
  "wizard.review.network": "Network access",
  "wizard.review.gateToken": "Provider credential",
  "wizard.review.gateTokenHint":
    "Only when no provider is attached. Encrypted at rest; the host needs its secrets key (`sudo fleett secrets init`).",
  "wizard.units.gb": "GB",
};
