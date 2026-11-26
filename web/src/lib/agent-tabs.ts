/**
 * Agent detail tab model (#73).
 *
 * The console shows five agent-oriented tabs. The controller, the command
 * palette and the assistant still speak the old per-panel tab ids, so this
 * module is the single place that translates between the two: legacy values
 * map onto a tab plus an optional sub-view (`?view=`) or advanced anchor
 * (`?section=`), and the reverse map feeds the assistant schema.
 */

export const AGENT_TABS = ["overview", "configure", "files", "activity", "advanced"] as const;
export type AgentTab = (typeof AGENT_TABS)[number];

export const ACTIVITY_VIEWS = ["logs", "incidents", "messages", "history"] as const;
export type ActivityView = (typeof ACTIVITY_VIEWS)[number];

export const ADVANCED_SECTIONS = [
  "schedules",
  "backups",
  "connectors",
  "access",
  "network",
  "memory",
  "terminal",
  "compose",
] as const;
export type AdvancedSection = (typeof ADVANCED_SECTIONS)[number];

/** One resolved position in the detail page, independent of the raw query. */
export interface AgentTabState {
  tab: AgentTab;
  view?: ActivityView;
  section?: AdvancedSection;
}

/** Tab labels, all already in the dictionary. */
export const AGENT_TAB_LABEL_KEYS: Record<AgentTab, string> = {
  overview: "detail.tab.overview",
  configure: "agent.tab.configure",
  files: "files.title",
  activity: "agent.tab.activity",
  advanced: "agent.tab.advanced",
};

export const ACTIVITY_LABEL_KEYS: Record<ActivityView, string> = {
  logs: "nav.logs",
  incidents: "nav.incidents",
  messages: "nav.messages",
  history: "skills.detail.history",
};

/** Old `?tab=` values, with their new home. */
const LEGACY_TABS: Record<string, AgentTabState> = {
  overview: { tab: "overview" },
  configuration: { tab: "configure" },
  files: { tab: "files" },
  logs: { tab: "activity", view: "logs" },
  incidents: { tab: "activity", view: "incidents" },
  messages: { tab: "activity", view: "messages" },
  history: { tab: "activity", view: "history" },
  schedules: { tab: "advanced", section: "schedules" },
  snapshots: { tab: "advanced", section: "backups" },
  mcp: { tab: "advanced", section: "connectors" },
  access: { tab: "advanced", section: "access" },
  egress: { tab: "advanced", section: "network" },
  memory: { tab: "advanced", section: "memory" },
  terminal: { tab: "advanced", section: "terminal" },
  compose: { tab: "advanced", section: "compose" },
};

/** Old tabs only an editor may open; a viewer falls back to the overview. */
const LEGACY_EDIT_ONLY = new Set(["configuration", "terminal"]);

const SECTION_TO_LEGACY: Record<AdvancedSection, string> = {
  schedules: "schedules",
  backups: "snapshots",
  connectors: "mcp",
  access: "access",
  network: "egress",
  memory: "memory",
  terminal: "terminal",
  compose: "compose",
};

export function isAgentTab(value: string | null): value is AgentTab {
  return value !== null && (AGENT_TABS as readonly string[]).includes(value);
}

export function isActivityView(value: string | null): value is ActivityView {
  return value !== null && (ACTIVITY_VIEWS as readonly string[]).includes(value);
}

export function isAdvancedSection(value: string | null): value is AdvancedSection {
  return value !== null && (ADVANCED_SECTIONS as readonly string[]).includes(value);
}

/**
 * Resolve the URL query into a tab position. Accepts both the new ids and the
 * legacy ones (palette links, bookmarks, assistant proposals). Unknown or
 * forbidden values land on the overview, like the old `requestedTab`.
 */
export function resolveAgentTab(
  tabParam: string | null,
  viewParam: string | null,
  sectionParam: string | null,
  canEdit: boolean,
): AgentTabState {
  if (isAgentTab(tabParam)) {
    if (tabParam === "configure" && !canEdit) return { tab: "overview" };
    if (tabParam === "activity") {
      return { tab: "activity", view: isActivityView(viewParam) ? viewParam : "logs" };
    }
    if (tabParam === "advanced") {
      const section = isAdvancedSection(sectionParam) ? sectionParam : undefined;
      if (!canEdit && section === "terminal") return { tab: "overview" };
      return { tab: "advanced", section };
    }
    return { tab: tabParam };
  }

  const legacy = tabParam && Object.hasOwn(LEGACY_TABS, tabParam) ? LEGACY_TABS[tabParam] : null;
  if (!legacy) return { tab: "overview" };
  if (!canEdit && LEGACY_EDIT_ONLY.has(tabParam ?? "")) return { tab: "overview" };
  return legacy;
}

/**
 * Legacy id handed to `instanceTabSchema`, so the assistant receives the
 * schema of the panel actually on screen. An activity tab follows its
 * segment; an advanced tab follows its anchor.
 */
export function legacyTabOf(state: AgentTabState): string {
  switch (state.tab) {
    case "configure":
      return "configuration";
    case "activity":
      return state.view ?? "logs";
    case "advanced":
      return state.section ? SECTION_TO_LEGACY[state.section] : "schedules";
    default:
      return state.tab;
  }
}

/** Query for a tab position, with extra parameters kept for deep links. */
export function agentTabQuery(
  state: AgentTabState,
  extra: Record<string, string | null | undefined> = {},
): string {
  const query = new URLSearchParams({ tab: state.tab });
  if (state.tab === "activity") query.set("view", state.view ?? "logs");
  if (state.tab === "advanced" && state.section) query.set("section", state.section);
  for (const [key, value] of Object.entries(extra)) {
    if (value) query.set(key, value);
  }
  return `?${query.toString()}`;
}

/** `#`-ready href for another tab of the same agent. */
export function agentTabHref(
  path: string,
  state: AgentTabState,
  extra: Record<string, string | null | undefined> = {},
): string {
  return `${path}${agentTabQuery(state, extra)}`;
}
