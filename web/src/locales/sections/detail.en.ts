/** Agent detail lane (#73): tabs, overview, created panel, advanced view. */
export const detailEn: Record<string, string> = {
  "agent.tab.configure": "Configure",
  "agent.tab.activity": "Activity",
  "agent.tab.advanced": "Advanced",

  "agent.header.noPersona": "No persona",
  "agent.header.lastActivity": "last activity {when}",
  "agent.header.copyId": "agent id",

  "agent.action.personality": "Edit personality",
  "agent.action.addSkill": "Add a skill",
  "agent.action.viewLogs": "View logs",
  "agent.action.configure": "View configuration",

  "agent.overview.problemsTitle": "Current problems",
  "agent.overview.noProblems": "No problem detected.",
  "agent.overview.whatTitle": "What this agent does",
  "agent.overview.whatDescription":
    "The personality it follows, the template it started from, and the model and channel it answers on.",
  "agent.overview.noPersona": "No persona: the template's SOUL.md is used as is.",
  "agent.overview.template": "Template",
  "agent.overview.nextRunsTitle": "Next scheduled runs",
  "agent.overview.manageSchedules": "Manage schedules",
  "agent.overview.nextRunsEmpty":
    "No scheduled run. A schedule wakes the agent on a cron cadence with a prompt.",
  "agent.overview.messagesTitle": "Latest messages",
  "agent.overview.openMessages": "Open messages",
  "agent.overview.messagesEmpty":
    "No message yet. The agent appears here as soon as it exchanges with another agent.",
  "agent.overview.viewerHint":
    "Your account is read-only: configuration, files and messages are visible but cannot be changed.",

  "agent.created.title": "{name} is ready",
  "agent.created.description":
    "The agent is created and its data directory is in place. Here is the next step.",
  "agent.created.discord": "Test on Discord",
  "agent.created.allowedUsers": "Choose who can talk to it",
  "agent.created.dismiss": "Dismiss",

  "agent.activity.view": "Activity view",

  "agent.advancedBannerTitle": "Advanced section",
  "agent.advancedBanner":
    "This section belongs to the advanced view. Switch to Advanced mode to find it in the tab bar.",
  "agent.advancedSwitch": "Switch to Advanced mode",
  "agent.advanced.schedules": "Scheduled runs",
  "agent.advanced.schedulesDescription":
    "Read-only list. Create, edit and run schedules from the global Schedules page.",
  "agent.advanced.manageSchedules": "Open schedules",
  "agent.advanced.paused": "paused",
  "agent.advanced.backupsDescription":
    "Snapshots of the data directory: take one before a risky change and restore it if needed.",
  "agent.advanced.connectors": "MCP connectors",
  "agent.advanced.connectorsDescription":
    "Deployed MCP servers attached to this agent. Attaching one rewrites config.yaml and recreates the container.",
  "agent.advanced.accessDescription":
    "Who may read or operate this agent: ACL entries by user or token.",
  "agent.advanced.networkDescription":
    "Destination policy and LAN reachability for this agent's egress.",
  "agent.advanced.memoryDescription": "Entries the agent keeps in its own memory namespace.",
  "agent.advanced.terminalDescription":
    "One-shot command inside the agent container, with a timeout.",
  "agent.advanced.composeDescription": "The compose.yaml rendered by the controller.",
};
