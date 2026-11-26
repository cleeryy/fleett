/**
 * Navigation model for the two console modes (#71).
 *
 * Simple shows four spaces — Agents, Bibliothèque, Activité, Réglages — with
 * product vocabulary. Advanced keeps those four and adds three collapsible
 * groups of technical destinations. This file is pure data plus path
 * derivations: no React state, no storage, so the table is unit-testable.
 */
import {
  Activity,
  Bot,
  Brain,
  CalendarClock,
  CircleDollarSign,
  Database,
  FileClock,
  FolderTree,
  KeyRound,
  Library,
  ListChecks,
  MessagesSquare,
  Plug,
  Rocket,
  ServerCog,
  Settings,
  ShieldCheck,
  Workflow,
  type LucideIcon,
} from "lucide-react";
import type { UiMode } from "@/lib/mode";

export type Translate = (key: string, params?: Record<string, string | number>) => string;

/** One breadcrumb segment. The last one is the current page and has no href. */
export interface Crumb {
  label: string;
  href?: string;
  mono?: boolean;
}

export type SpaceKey = "agents" | "library" | "activity" | "settings";

export type GroupKey = "automate" | "data" | "system";

export type LeafKey =
  | "schedules"
  | "pipelines"
  | "messages"
  | "memory"
  | "shares"
  | "cost"
  | "deployments"
  | "providers"
  | "security"
  | "tokens"
  | "audit"
  | "setup"
  | "templates"
  | "personas"
  | "skills"
  | "mcpServers"
  | "marketplace"
  | "incidents"
  | "logs";

export type NavKey = SpaceKey | LeafKey;

/** Anything the sidebar renders as a link. */
export interface NavTarget {
  key: NavKey;
  path: string;
  labelKey: string;
  icon: LucideIcon;
}

/** A simple-mode destination: the four spaces. */
export interface NavSpace extends NavTarget {
  key: SpaceKey;
}

/** A technical destination, rendered inside an advanced group. */
export interface NavEntry extends NavTarget {
  key: LeafKey;
  /** Hidden from `viewer` accounts; the route guard mirrors this. */
  adminOnly?: boolean;
}

export interface NavGroup {
  key: GroupKey;
  labelKey: string;
  hintKey: string;
  icon: LucideIcon;
  entries: NavEntry[];
}

export const NAV_SPACES: readonly NavSpace[] = [
  { key: "agents", path: "/instances", labelKey: "nav.agents", icon: Bot },
  { key: "library", path: "/library", labelKey: "nav.library", icon: Library },
  { key: "activity", path: "/activity", labelKey: "nav.activity", icon: Activity },
  { key: "settings", path: "/settings", labelKey: "nav.settings", icon: Settings },
];

export const NAV_GROUPS: readonly NavGroup[] = [
  {
    key: "automate",
    labelKey: "nav.groupAutomate",
    hintKey: "nav.groupAutomateHint",
    icon: CalendarClock,
    entries: [
      {
        key: "schedules",
        path: "/schedules",
        labelKey: "nav.schedules",
        icon: CalendarClock,
        adminOnly: true,
      },
      { key: "pipelines", path: "/pipelines", labelKey: "nav.pipelines", icon: Workflow },
    ],
  },
  {
    key: "data",
    labelKey: "nav.groupData",
    hintKey: "nav.groupDataHint",
    icon: Database,
    entries: [
      {
        key: "messages",
        path: "/messages",
        labelKey: "nav.messages",
        icon: MessagesSquare,
        adminOnly: true,
      },
      { key: "memory", path: "/memory", labelKey: "nav.memory", icon: Brain },
      { key: "shares", path: "/shares", labelKey: "nav.shares", icon: FolderTree },
      { key: "cost", path: "/cost", labelKey: "nav.cost", icon: CircleDollarSign },
    ],
  },
  {
    key: "system",
    labelKey: "nav.groupSystem",
    hintKey: "nav.groupSystemHint",
    icon: ServerCog,
    entries: [
      { key: "deployments", path: "/deployments", labelKey: "nav.deployments", icon: Rocket },
      { key: "providers", path: "/providers", labelKey: "nav.providers", icon: Plug },
      {
        key: "security",
        path: "/security",
        labelKey: "nav.security",
        icon: ShieldCheck,
        adminOnly: true,
      },
      { key: "tokens", path: "/tokens", labelKey: "nav.tokens", icon: KeyRound, adminOnly: true },
      { key: "audit", path: "/audit", labelKey: "nav.audit", icon: FileClock, adminOnly: true },
      { key: "setup", path: "/setup", labelKey: "nav.setup", icon: ListChecks },
    ],
  },
];

/**
 * Prefix table. Segment-aware matching (`/x` or `/x/…`), so `/setup` never
 * lights `/settings`. `/` and `/instances` are handled explicitly above it.
 */
const ACTIVE_PREFIXES: ReadonlyArray<readonly [string, NavKey]> = [
  ["/library", "library"],
  ["/templates", "templates"],
  ["/personas", "personas"],
  ["/skills", "skills"],
  ["/mcp-servers", "mcpServers"],
  ["/marketplace", "marketplace"],
  ["/activity", "activity"],
  ["/incidents", "incidents"],
  ["/logs", "logs"],
  ["/deployments", "deployments"],
  ["/settings", "settings"],
  ["/providers", "providers"],
  ["/security", "security"],
  ["/tokens", "tokens"],
  ["/audit", "audit"],
  ["/setup", "setup"],
  ["/schedules", "schedules"],
  ["/pipelines", "pipelines"],
  ["/messages", "messages"],
  ["/memory", "memory"],
  ["/shares", "shares"],
  ["/cost", "cost"],
];

/** Which destination a path belongs to; `null` for login and unknown paths. */
export function activeNavKey(path: string): NavKey | null {
  if (path === "/" || path === "/instances" || path.startsWith("/instances/")) return "agents";
  for (const [prefix, key] of ACTIVE_PREFIXES) {
    if (path === prefix || path.startsWith(`${prefix}/`)) return key;
  }
  return null;
}

const SPACE_OF: Readonly<Partial<Record<NavKey, SpaceKey>>> = {
  agents: "agents",
  library: "library",
  templates: "library",
  personas: "library",
  skills: "library",
  mcpServers: "library",
  marketplace: "library",
  activity: "activity",
  incidents: "activity",
  logs: "activity",
  deployments: "activity",
  settings: "settings",
  providers: "settings",
  security: "settings",
  tokens: "settings",
  audit: "settings",
  setup: "settings",
};

/** The simple-mode space a path belongs to, if any. */
export function spaceFor(path: string): SpaceKey | null {
  const key = activeNavKey(path);
  return key ? (SPACE_OF[key] ?? null) : null;
}

/** The advanced group holding a key, if any. */
export function groupOf(key: NavKey | null): NavGroup | null {
  if (!key) return null;
  return NAV_GROUPS.find((group) => group.entries.some((entry) => entry.key === key)) ?? null;
}

/** A key that is only reachable through an advanced group. */
export function activeLeaf(path: string): LeafKey | null {
  const key = activeNavKey(path);
  if (!key) return null;
  return groupOf(key) ? (key as LeafKey) : null;
}

/**
 * The one key the sidebar lights.
 *
 * Advanced prefers the technical entry (Providers, Deployments, …); when the
 * page has no advanced entry (Templates, Incidents, …) it lights its space so
 * the menu never goes dark. Simple only lights spaces.
 */
export function navSelection(path: string, mode: UiMode): NavKey | null {
  if (mode === "advanced") return activeLeaf(path) ?? spaceFor(path);
  return spaceFor(path);
}

function hash(path: string): string {
  return `#${path}`;
}

function decode(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    // A malformed escape is still a readable label — never crash the shell.
    return segment;
  }
}

function current(label: string, mono = false): Crumb {
  return mono ? { label, mono: true } : { label };
}

function linked(label: string, path: string): Crumb {
  return { label, href: hash(path) };
}

function spaceCrumb(key: SpaceKey, t: Translate, link: boolean): Crumb {
  const space = NAV_SPACES.find((entry) => entry.key === key);
  if (!space) return { label: t("nav.notFound") };
  return link ? { label: t(space.labelKey), href: hash(space.path) } : { label: t(space.labelKey) };
}

function groupCrumb(key: GroupKey, t: Translate): Crumb {
  const group = NAV_GROUPS.find((entry) => entry.key === key);
  return { label: group ? t(group.labelKey) : t("nav.notFound") };
}

const INSTANCE_DETAIL = /^\/instances\/([^/]+)$/;
const PROVIDER_DETAIL = /^\/providers\/([^/]+)$/;
const PERSONA_DETAIL = /^\/personas\/([^/]+)$/;
const SKILL_DETAIL = /^\/skills\/([^/]+)$/;
const MCP_DETAIL = /^\/mcp-servers\/([^/]+)$/;

/**
 * Breadcrumbs, three levels at most. Simple mode roots technical pages in
 * their product space (Réglages › Fournisseurs de modèles); Advanced roots
 * them in their group (Système › Fournisseurs). Detail pages keep the list
 * link in the middle, so the current id is always the last segment.
 */
export function crumbsFor(path: string, mode: UiMode, t: Translate): Crumb[] {
  const advanced = mode === "advanced";
  const agents = (link: boolean) => spaceCrumb("agents", t, link);
  const library = (link: boolean) => spaceCrumb("library", t, link);
  const activity = (link: boolean) => spaceCrumb("activity", t, link);
  const settings = (link: boolean) => spaceCrumb("settings", t, link);

  if (path === "/" || path === "/instances") return [current(t("nav.agents"))];

  const instance = INSTANCE_DETAIL.exec(path);
  if (path === "/instances/new") return [agents(true), current(t("nav.newAgent"))];
  if (instance) return [agents(true), current(decode(instance[1]), true)];

  if (path === "/library") return [current(t("nav.library"))];
  if (path === "/templates") return [library(true), current(t("shell.library.templates"))];
  if (path === "/personas") return [library(true), current(t("shell.library.personas"))];
  const persona = PERSONA_DETAIL.exec(path);
  if (persona) {
    return [
      library(true),
      linked(t("shell.library.personas"), "/personas"),
      current(decode(persona[1]), true),
    ];
  }
  if (path === "/skills") return [library(true), current(t("shell.library.skills"))];
  const skill = SKILL_DETAIL.exec(path);
  if (skill) {
    return [
      library(true),
      linked(t("shell.library.skills"), "/skills"),
      current(decode(skill[1]), true),
    ];
  }
  if (path === "/marketplace") return [library(true), current(t("shell.library.marketplace"))];
  if (path === "/mcp-servers") return [library(true), current(t("shell.library.connectors"))];
  const mcp = MCP_DETAIL.exec(path);
  if (mcp) {
    return [
      library(true),
      linked(t("shell.library.connectors"), "/mcp-servers"),
      current(decode(mcp[1]), true),
    ];
  }

  if (path === "/activity") return [current(t("nav.activity"))];
  if (path === "/incidents") return [activity(true), current(t("nav.incidents"))];
  if (path === "/logs") return [activity(true), current(t("nav.logs"))];
  if (path === "/deployments") {
    return advanced
      ? [groupCrumb("system", t), current(t("nav.deployments"))]
      : [activity(true), current(t("nav.deployments"))];
  }

  if (path === "/settings") return [current(t("nav.settings"))];
  const provider = PROVIDER_DETAIL.exec(path);
  if (path === "/providers") {
    return advanced
      ? [groupCrumb("system", t), current(t("nav.providers"))]
      : [settings(true), current(t("shell.settings.providers"))];
  }
  if (provider) {
    return advanced
      ? [
          groupCrumb("system", t),
          linked(t("nav.providers"), "/providers"),
          current(t("nav.providerDetail")),
        ]
      : [
          settings(true),
          linked(t("shell.settings.providers"), "/providers"),
          current(t("nav.providerDetail")),
        ];
  }

  const settingsPage: Readonly<Record<string, { simple: string; full: string }>> = {
    "/security": { simple: "shell.settings.security", full: "nav.security" },
    "/tokens": { simple: "shell.settings.tokens", full: "nav.tokens" },
    "/audit": { simple: "shell.settings.audit", full: "nav.audit" },
    "/setup": { simple: "shell.settings.setup", full: "nav.setup" },
  };
  const settingsEntry = settingsPage[path];
  if (settingsEntry) {
    return advanced
      ? [groupCrumb("system", t), current(t(settingsEntry.full))]
      : [settings(true), current(t(settingsEntry.simple))];
  }

  const advancedPages: Readonly<Record<string, { group: GroupKey; labelKey: string }>> = {
    "/schedules": { group: "automate", labelKey: "nav.schedules" },
    "/pipelines": { group: "automate", labelKey: "nav.pipelines" },
    "/messages": { group: "data", labelKey: "nav.messages" },
    "/memory": { group: "data", labelKey: "nav.memory" },
    "/shares": { group: "data", labelKey: "nav.shares" },
    "/cost": { group: "data", labelKey: "nav.cost" },
  };
  const advancedPage = advancedPages[path];
  if (advancedPage) {
    return advanced
      ? [groupCrumb(advancedPage.group, t), current(t(advancedPage.labelKey))]
      : [current(t(advancedPage.labelKey))];
  }

  return [current(t("nav.notFound"))];
}

/** Document title: the last breadcrumb, i.e. the canonical page label. */
export function titleFor(path: string, mode: UiMode, t: Translate): string {
  const crumbs = crumbsFor(path, mode, t);
  return crumbs[crumbs.length - 1]?.label ?? t("nav.notFound");
}
