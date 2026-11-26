import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Activity,
  Bot,
  Eraser,
  ExternalLink,
  Fingerprint,
  FolderTree,
  History,
  LayoutTemplate,
  Loader2,
  Moon,
  Play,
  Plug,
  Plus,
  RotateCw,
  ScrollText,
  Search,
  Server,
  SlidersHorizontal,
  Sparkles,
  Square,
  Store,
  Sun,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError } from "@/api/errors";
import type {
  InstanceSummary,
  MarketplaceEntry,
  McpServerView,
  PersonaSummary,
  ProviderSummary,
  SkillDetail,
  SkillSummary,
  Template,
} from "@/api/types";
import { DeleteInstanceDialog, type DeletableInstance } from "@/components/delete-instance-dialog";
import { SkillDeleteDialog } from "@/components/skill-delete-dialog";
import { Badge } from "@/components/ui/badge";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import { notifyInstancesChanged } from "@/lib/events";
import { useI18n } from "@/lib/i18n";
import { useUiMode } from "@/lib/mode";
import { NAV_GROUPS, NAV_SPACES, type NavTarget } from "@/lib/navigation";
import {
  PALETTE_HISTORY_LIMIT,
  clearPaletteHistory,
  paletteHistoryKey,
  paletteHistoryPath,
  readPaletteHistory,
  rememberPaletteHit,
  type PaletteHistoryEntry,
  type PaletteHistoryHit,
} from "@/lib/palette-history";
import { navigate } from "@/lib/router";
import { useCanEdit, useIdentity } from "@/lib/session";
import { cn } from "@/lib/utils";

/**
 * Global search and command palette (#76).
 *
 * Five groups in a fixed order: Récents (local, empty query), Agents, Pages,
 * Bibliothèque, Actions. Pages come from the shared navigation model so the
 * palette and the sidebar can never disagree; advanced destinations stay
 * hidden in Simple until the query matches them, and then carry an Avancé
 * badge. Library lists load once per palette instance, marketplace results
 * only on a two-character query and after a 200 ms debounce.
 *
 * Destructive rows never run on a single keystroke: the first Enter turns the
 * row into an inline confirmation, the second opens the existing delete dialog,
 * and Escape cancels. History only records destinations, after a successful
 * invoke; a cancelled confirmation writes nothing.
 */

type PaletteGroup = "recents" | "agents" | "pages" | "library" | "actions";

const GROUP_ORDER: readonly PaletteGroup[] = ["recents", "agents", "pages", "library", "actions"];

const GROUP_LABEL: Record<PaletteGroup, string> = {
  recents: "palette.group.recents",
  agents: "palette.group.agents",
  pages: "palette.group.pages",
  library: "palette.group.library",
  actions: "palette.group.actions",
};

const GROUP_LIMIT: Record<PaletteGroup, number> = {
  recents: PALETTE_HISTORY_LIMIT + 1,
  agents: 12,
  // Four spaces plus every advanced entry: Advanced mode must list them all.
  pages: 20,
  library: 30,
  actions: 16,
};

/** Cap per library type, as the issue asks: five rows each. */
const LIBRARY_PER_TYPE = 5;

const MARKETPLACE_MIN_QUERY = 2;
const MARKETPLACE_DEBOUNCE_MS = 200;

type LibraryType = "template" | "persona" | "skill" | "mcp" | "marketplace";

interface Command {
  /** Stable identity; recents reuse `kind:id` so a row is never listed twice. */
  key: string;
  group: PaletteGroup;
  label: string;
  /** Muted text on the right: a status, an id, a model. */
  detail?: string;
  keywords: string[];
  icon: LucideIcon;
  /** First Enter confirms inline instead of running it. */
  destructive?: boolean;
  /** Page or library row only listed once the operator types something. */
  queryOnly?: boolean;
  /** Technical destination surfaced with an Avancé badge in Simple mode. */
  advanced?: boolean;
  /** Empty palette lists "open" rows only, not every lifecycle action. */
  agentOpen?: boolean;
  /** Counts against the five-per-type budget. */
  libraryType?: LibraryType;
  /** Destination written to local history after a successful invoke. */
  hit?: PaletteHistoryHit;
  perform: () => void;
}

interface LibraryData {
  templates: Template[];
  personas: PersonaSummary[];
  skills: SkillSummary[];
  mcpServers: McpServerView[];
}

const NAV_TARGETS: readonly NavTarget[] = [
  ...NAV_SPACES,
  ...NAV_GROUPS.flatMap((group) => group.entries),
];

const PAGE_ICONS: Readonly<Record<string, LucideIcon>> = {
  "/templates": LayoutTemplate,
  "/personas": Fingerprint,
  "/skills": Sparkles,
  "/mcp-servers": Server,
  "/marketplace": Store,
};

/**
 * Technical destinations carry product vocabulary in Simple mode (the
 * breadcrumb does the same). Searching "modèles" must still find the model
 * providers page.
 */
const SIMPLE_LABEL_KEYS: Readonly<Record<string, string>> = {
  "/providers": "shell.settings.providers",
  "/security": "shell.settings.security",
  "/tokens": "shell.settings.tokens",
  "/audit": "shell.settings.audit",
  "/setup": "shell.settings.setup",
};

/** Library sub-pages live in the Bibliothèque group, only on a matching query. */
const LIBRARY_SECTIONS: readonly {
  path: string;
  labelKey: string;
  icon: LucideIcon;
  keywords: string[];
}[] = [
  {
    path: "/templates",
    labelKey: "shell.library.templates",
    icon: LayoutTemplate,
    keywords: ["templates", "template", "modèles", "modeles", "agent", "page"],
  },
  {
    path: "/personas",
    labelKey: "shell.library.personas",
    icon: Fingerprint,
    keywords: ["personas", "persona", "personnalités", "personnalites", "personality", "page"],
  },
  {
    path: "/skills",
    labelKey: "shell.library.skills",
    icon: Sparkles,
    keywords: ["skills", "skill", "compétences", "competences", "page"],
  },
  {
    path: "/mcp-servers",
    labelKey: "shell.library.connectors",
    icon: Server,
    keywords: ["mcp", "connectors", "connecteurs", "servers", "serveurs", "page"],
  },
  {
    path: "/marketplace",
    labelKey: "shell.library.marketplace",
    icon: Store,
    keywords: ["marketplace", "market", "skills", "page"],
  },
];

function pageIcon(path: string): LucideIcon {
  return NAV_TARGETS.find((target) => target.path === path)?.icon ?? PAGE_ICONS[path] ?? History;
}

function historyIcon(hit: PaletteHistoryHit): LucideIcon {
  switch (hit.kind) {
    case "agent":
    case "agent-logs":
    case "agent-files":
    case "agent-env":
    case "agent-connectors":
      return Bot;
    case "template":
      return LayoutTemplate;
    case "persona":
      return Fingerprint;
    case "skill":
      return Sparkles;
    case "mcp":
      return Server;
    case "marketplace":
      return Store;
    case "page":
      return pageIcon(hit.id);
    default:
      return History;
  }
}

/** Rank a command against the typed query; `null` means it does not match. */
function scoreOf(command: Command, tokens: string[], phrase: string): number | null {
  const label = command.label.toLowerCase();
  const haystack = `${label} ${command.keywords.join(" ")}`.toLowerCase();
  if (!tokens.every((token) => haystack.includes(token))) return null;

  let score = 0;
  if (label === phrase) score += 100;
  else if (label.startsWith(phrase)) score += 80;
  else if (label.includes(phrase)) score += 60;
  if (command.keywords.some((keyword) => keyword.toLowerCase().startsWith(phrase))) score += 40;
  if (tokens.length > 0 && label.startsWith(tokens[0])) score += 10;
  return score;
}

function sortedByName<T extends { name: string }>(items: readonly T[]): T[] {
  return [...items].sort((left, right) => left.name.localeCompare(right.name));
}

export function CommandPalette({
  open,
  onOpenChange,
  theme,
  onToggleTheme,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  theme: "dark" | "light";
  onToggleTheme: () => void;
}) {
  const { t } = useI18n();
  const identity = useIdentity();
  const canEdit = useCanEdit();
  const admin = identity?.role === "admin";
  const { mode, simple, toggleMode } = useUiMode();

  const inputRef = useRef<HTMLInputElement>(null);
  const marketplaceCache = useRef(new Map<string, MarketplaceEntry[]>());

  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [history, setHistory] = useState<PaletteHistoryEntry[]>([]);

  const [instances, setInstances] = useState<InstanceSummary[] | null>(null);
  const [instancesError, setInstancesError] = useState<string | null>(null);
  const [loadingInstances, setLoadingInstances] = useState(false);
  const [providers, setProviders] = useState<ProviderSummary[] | null>(null);

  const [library, setLibrary] = useState<LibraryData | null>(null);
  const [loadingLibrary, setLoadingLibrary] = useState(false);
  const [marketplaceEntries, setMarketplaceEntries] = useState<MarketplaceEntry[] | null>(null);
  const [loadingMarketplace, setLoadingMarketplace] = useState(false);

  const [deleteTarget, setDeleteTarget] = useState<DeletableInstance | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [skillTarget, setSkillTarget] = useState<SkillDetail | null>(null);
  const [skillDeleteOpen, setSkillDeleteOpen] = useState(false);

  // Ctrl/Cmd+K works anywhere in the console, and closes the dialog again.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return;
      if (event.key.toLowerCase() !== "k") return;
      // Do not stack the palette on top of another modal (delete sheets,
      // forms). The assistant side panel is a non-modal dialog and may stay.
      if (!open && document.querySelector('[role="dialog"][data-slot="dialog-content"]')) return;
      event.preventDefault();
      onOpenChange(!open);
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onOpenChange]);

  // A fresh search every time the palette opens; instances and providers
  // change under us. Library content loads once and is kept for the next open.
  useEffect(() => {
    if (!open) return;
    setQuery("");
    setActive(0);
    setConfirming(null);
    setHistory(readPaletteHistory());
    marketplaceCache.current.clear();
    setMarketplaceEntries(null);
    setLoadingMarketplace(false);

    let alive = true;
    setLoadingInstances(true);
    setInstancesError(null);
    api.listInstances().then(
      (list) => {
        if (!alive) return;
        setInstances(list);
        setLoadingInstances(false);
      },
      (cause: unknown) => {
        if (!alive) return;
        setInstancesError(apiErrorMessage(toApiError(cause), t));
        setLoadingInstances(false);
      },
    );

    if (canEdit) {
      api.listProviders().then(
        (list) => {
          if (alive) setProviders(list);
        },
        () => {
          if (alive) setProviders([]);
        },
      );
    } else {
      // A viewer never tests a provider: no point loading the credentials list.
      setProviders([]);
    }

    return () => {
      alive = false;
    };
  }, [open, canEdit, t]);

  // Library lists: one round trip per palette instance, never per keystroke.
  useEffect(() => {
    if (!open || library !== null) return;
    let alive = true;
    setLoadingLibrary(true);
    Promise.allSettled([
      api.listTemplates(),
      api.listPersonas(),
      api.listSkills(),
      api.listMcpServers(),
    ]).then(([templates, personas, skills, mcpServers]) => {
      if (!alive) return;
      setLibrary({
        templates: templates.status === "fulfilled" ? templates.value : [],
        personas: personas.status === "fulfilled" ? personas.value : [],
        skills: skills.status === "fulfilled" ? skills.value : [],
        mcpServers: mcpServers.status === "fulfilled" ? mcpServers.value : [],
      });
      setLoadingLibrary(false);
    });
    return () => {
      alive = false;
    };
  }, [open, library]);

  // Marketplace: two characters minimum, 200 ms debounce, cached per open so
  // re-typing a query never hits the index twice.
  useEffect(() => {
    if (!open) return;
    const trimmed = query.trim();
    if (trimmed.length < MARKETPLACE_MIN_QUERY) {
      setMarketplaceEntries(null);
      setLoadingMarketplace(false);
      return;
    }
    const cached = marketplaceCache.current.get(trimmed);
    if (cached) {
      setMarketplaceEntries(cached);
      setLoadingMarketplace(false);
      return;
    }

    let alive = true;
    setLoadingMarketplace(true);
    const timer = window.setTimeout(() => {
      api.marketplaceSearch({ q: trimmed, limit: 8 }).then(
        (result) => {
          marketplaceCache.current.set(trimmed, result.entries);
          if (!alive) return;
          setMarketplaceEntries(result.entries);
          setLoadingMarketplace(false);
        },
        () => {
          marketplaceCache.current.set(trimmed, []);
          if (!alive) return;
          setMarketplaceEntries([]);
          setLoadingMarketplace(false);
        },
      );
    }, MARKETPLACE_DEBOUNCE_MS);

    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [open, query]);

  const runInstanceAction = useCallback(
    async (instance: InstanceSummary, action: "start" | "stop" | "restart") => {
      try {
        await api.action(instance.id, action);
        const past = t(
          action === "stop"
            ? "status.stopped"
            : action === "start"
              ? "status.started"
              : "status.restarted",
        );
        toast.success(t("palette.actionDone", { id: instance.id, state: past }));
        notifyInstancesChanged();
      } catch (cause) {
        toast.error(apiErrorMessage(toApiError(cause), t));
      }
    },
    [t],
  );

  const testProvider = useCallback(
    async (provider: ProviderSummary) => {
      const toastId = toast.loading(t("palette.providerTesting", { name: provider.name }));
      try {
        const result = await api.testProvider(provider.id);
        // The probe result is already redacted upstream; the toast never
        // repeats a key or a request body either way.
        if (result.ok) {
          toast.success(t("palette.providerOk", { name: provider.name }), { id: toastId });
        } else {
          toast.error(t("palette.providerFailed", { name: provider.name }), { id: toastId });
        }
      } catch {
        toast.error(t("palette.providerFailed", { name: provider.name }), { id: toastId });
      }
    },
    [t],
  );

  const toggleModeWithToast = useCallback(() => {
    const next = mode === "simple" ? "advanced" : "simple";
    toggleMode();
    toast.success(next === "advanced" ? t("shell.modeAdvancedOn") : t("shell.modeSimpleOn"));
  }, [mode, t, toggleMode]);

  const openSkillDelete = useCallback(
    async (name: string) => {
      const toastId = toast.loading(t("palette.loadingSkill", { name }));
      try {
        const detail = await api.getSkill(name);
        toast.dismiss(toastId);
        setSkillTarget(detail);
        setSkillDeleteOpen(true);
      } catch (cause) {
        toast.error(apiErrorMessage(toApiError(cause), t), { id: toastId });
      }
    },
    [t],
  );

  const commands = useMemo<Command[]>(() => {
    const list: Command[] = [];

    for (const instance of sortedByName(instances ?? [])) {
      const path = `/instances/${encodeURIComponent(instance.id)}`;
      const base = [
        instance.id,
        instance.name,
        instance.model,
        instance.template,
        instance.channel,
        instance.status,
      ];
      const detail = `${t(`status.${instance.status}`)} · ${instance.model}`;

      list.push({
        key: `agent:${instance.id}`,
        group: "agents",
        label: t("palette.open", { name: instance.name }),
        detail,
        keywords: [...base, "open", "details", "overview", "ouvrir"],
        icon: ExternalLink,
        agentOpen: true,
        hit: { kind: "agent", id: instance.id, label: instance.name },
        perform: () => navigate(path),
      });

      if (canEdit) {
        if (instance.status !== "running") {
          list.push({
            key: `agent-start:${instance.id}`,
            group: "agents",
            label: t("palette.start", { name: instance.name }),
            detail: instance.id,
            keywords: [...base, "start", "run", "up", "démarrer", "demarrer"],
            icon: Play,
            perform: () => void runInstanceAction(instance, "start"),
          });
        }
        if (instance.status === "running") {
          list.push({
            key: `agent-stop:${instance.id}`,
            group: "agents",
            label: t("palette.stop", { name: instance.name }),
            detail: instance.id,
            keywords: [...base, "stop", "halt", "down", "arrêter", "arreter"],
            icon: Square,
            perform: () => void runInstanceAction(instance, "stop"),
          });
        }
        list.push({
          key: `agent-restart:${instance.id}`,
          group: "agents",
          label: t("palette.restart", { name: instance.name }),
          detail: instance.id,
          keywords: [...base, "restart", "reload", "bounce", "redémarrer", "redemarrer"],
          icon: RotateCw,
          perform: () => void runInstanceAction(instance, "restart"),
        });
      }

      list.push({
        key: `agent-logs:${instance.id}`,
        group: "agents",
        label: t("palette.logs", { name: instance.name }),
        detail: instance.id,
        keywords: [...base, "logs", "log", "output", "tail", "journaux"],
        icon: ScrollText,
        hit: {
          kind: "agent-logs",
          id: instance.id,
          label: t("palette.logs", { name: instance.name }),
        },
        perform: () => navigate(`${path}?tab=activity&view=logs`),
      });
      list.push({
        key: `agent-files:${instance.id}`,
        group: "agents",
        label: t("palette.files", { name: instance.name }),
        detail: instance.id,
        keywords: [...base, "files", "file", "data", "soul", "fichiers"],
        icon: FolderTree,
        hit: {
          kind: "agent-files",
          id: instance.id,
          label: t("palette.files", { name: instance.name }),
        },
        perform: () => navigate(`${path}?tab=files`),
      });
      list.push({
        key: `agent-env:${instance.id}`,
        group: "agents",
        label: t("palette.env", { name: instance.name }),
        detail: instance.id,
        keywords: [...base, "env", "environment", "variables", "secrets", "environnement"],
        icon: SlidersHorizontal,
        hit: {
          kind: "agent-env",
          id: instance.id,
          label: t("palette.env", { name: instance.name }),
        },
        perform: () => navigate(`${path}?tab=configure&focus=env`),
      });
      list.push({
        key: `agent-connectors:${instance.id}`,
        group: "agents",
        label: t("palette.connectors", { name: instance.name }),
        detail: instance.id,
        keywords: [...base, "mcp", "servers", "tools", "attach", "connecteurs"],
        icon: Server,
        hit: {
          kind: "agent-connectors",
          id: instance.id,
          label: t("palette.connectors", { name: instance.name }),
        },
        perform: () => navigate(`${path}?tab=advanced&section=connectors`),
      });

      if (canEdit) {
        list.push({
          key: `agent-delete:${instance.id}`,
          group: "agents",
          label: t("palette.deleteAgent", { name: instance.name }),
          detail: instance.id,
          keywords: [...base, "delete", "remove", "destroy", "supprimer"],
          icon: TriangleAlert,
          destructive: true,
          perform: () => {
            setDeleteTarget({ id: instance.id, name: instance.name });
            setDeleteOpen(true);
          },
        });
      }
    }

    for (const space of NAV_SPACES) {
      list.push({
        key: `page:${space.path}`,
        group: "pages",
        label: t(space.labelKey),
        keywords: [t(space.labelKey), space.path, "page", "go", "open", "navigate"],
        icon: space.icon,
        hit: { kind: "page", id: space.path, label: t(space.labelKey) },
        perform: () => navigate(space.path),
      });
    }

    for (const group of NAV_GROUPS) {
      for (const entry of group.entries) {
        if (entry.adminOnly && !admin) continue;
        const simpleLabel = SIMPLE_LABEL_KEYS[entry.path];
        list.push({
          key: `page:${entry.path}`,
          group: "pages",
          label: t(entry.labelKey),
          detail: t(group.labelKey),
          keywords: [
            t(entry.labelKey),
            simpleLabel ? t(simpleLabel) : "",
            t(group.labelKey),
            t(group.hintKey),
            entry.path,
            "page",
            "advanced",
            "avancé",
          ],
          icon: entry.icon,
          advanced: true,
          hit: { kind: "page", id: entry.path, label: t(entry.labelKey) },
          perform: () => navigate(entry.path),
        });
      }
    }

    for (const section of LIBRARY_SECTIONS) {
      list.push({
        key: `page:${section.path}`,
        group: "library",
        label: t(section.labelKey),
        keywords: [t(section.labelKey), ...section.keywords],
        icon: section.icon,
        queryOnly: true,
        hit: { kind: "page", id: section.path, label: t(section.labelKey) },
        perform: () => navigate(section.path),
      });
    }

    if (library) {
      for (const template of sortedByName(library.templates)) {
        list.push({
          key: `template:${template.id}`,
          group: "library",
          label: template.name,
          detail: t("shell.library.templates"),
          keywords: [
            template.id,
            template.name,
            template.description,
            template.default_model,
            template.channel,
            "template",
            "modèle",
            "modele",
            "agent",
          ],
          icon: LayoutTemplate,
          libraryType: "template",
          hit: { kind: "template", id: template.id, label: template.name },
          perform: () => navigate("/library?type=templates"),
        });
      }

      for (const persona of sortedByName(library.personas)) {
        list.push({
          key: `persona:${persona.id}`,
          group: "library",
          label: persona.name,
          detail: t("shell.library.personas"),
          keywords: [
            persona.id,
            persona.name,
            persona.description,
            ...persona.tags,
            "persona",
            "personnalité",
            "personnalite",
            "personality",
          ],
          icon: Fingerprint,
          libraryType: "persona",
          hit: { kind: "persona", id: persona.id, label: persona.name },
          perform: () => navigate(`/personas/${encodeURIComponent(persona.id)}`),
        });
      }

      for (const skill of sortedByName(library.skills)) {
        list.push({
          key: `skill:${skill.name}`,
          group: "library",
          label: skill.name,
          detail: t("shell.library.skills"),
          keywords: [
            skill.name,
            skill.description,
            "skill",
            "compétence",
            "competence",
          ],
          icon: Sparkles,
          libraryType: "skill",
          hit: { kind: "skill", id: skill.name, label: skill.name },
          perform: () => navigate(`/skills/${encodeURIComponent(skill.name)}`),
        });
      }

      if (canEdit) {
        for (const skill of sortedByName(library.skills)) {
          list.push({
            key: `skill-delete:${skill.name}`,
            group: "library",
            label: t("palette.deleteSkill", { name: skill.name }),
            detail: skill.name,
            keywords: [skill.name, "delete", "remove", "supprimer", "skill", "compétence"],
            icon: TriangleAlert,
            destructive: true,
            queryOnly: true,
            perform: () => void openSkillDelete(skill.name),
          });
        }
      }

      for (const server of sortedByName(library.mcpServers)) {
        list.push({
          key: `mcp:${server.id}`,
          group: "library",
          label: server.name,
          detail: t("shell.library.connectors"),
          keywords: [
            server.id,
            server.name,
            server.description,
            server.kind,
            "mcp",
            "server",
            "serveur",
            "connecteur",
            "connector",
          ],
          icon: Server,
          libraryType: "mcp",
          hit: { kind: "mcp", id: server.id, label: server.name },
          perform: () => navigate(`/mcp-servers/${encodeURIComponent(server.id)}`),
        });
      }
    }

    for (const entry of marketplaceEntries ?? []) {
      list.push({
        key: `marketplace:${entry.name}`,
        group: "library",
        label: entry.name,
        detail: entry.author?.trim() || entry.source || t("shell.library.marketplace"),
        keywords: [
          entry.name,
          entry.description,
          entry.provider,
          entry.source ?? "",
          entry.author ?? "",
          ...(entry.tags ?? []),
          "marketplace",
          "skill",
        ],
        icon: Store,
        libraryType: "marketplace",
        hit: { kind: "marketplace", id: entry.name, label: entry.name },
        perform: () => navigate("/library?type=marketplace"),
      });
    }

    if (canEdit) {
      list.push({
        key: "action:create-agent",
        group: "actions",
        label: t("palette.createAgent"),
        keywords: ["new", "create", "agent", "instance", "nouveau", "créer", "creer"],
        icon: Plus,
        perform: () => navigate("/instances/new"),
      });

      for (const provider of sortedByName(providers ?? [])) {
        list.push({
          key: `action:provider:${provider.id}`,
          group: "actions",
          label: t("palette.testProvider", { name: provider.name }),
          detail: provider.kind,
          keywords: [
            provider.id,
            provider.name,
            provider.kind,
            provider.base_url,
            "test",
            "connection",
            "provider",
            "fournisseur",
            "tester",
          ],
          icon: Plug,
          perform: () => void testProvider(provider),
        });
      }
    }

    list.push({
      key: "action:incidents",
      group: "actions",
      label: t("palette.viewIncidents"),
      keywords: ["incidents", "open", "view", "voir", "activity"],
      icon: Activity,
      perform: () => navigate("/incidents"),
    });

    list.push({
      key: "action:mode",
      group: "actions",
      label: mode === "simple" ? t("palette.switchToAdvanced") : t("palette.switchToSimple"),
      detail: t("shell.mode"),
      keywords: ["mode", "simple", "advanced", "avancé", "console", "switch"],
      icon: SlidersHorizontal,
      perform: toggleModeWithToast,
    });

    list.push({
      key: "action:theme",
      group: "actions",
      label: theme === "dark" ? t("shell.switchToLight") : t("shell.switchToDark"),
      detail: t("palette.appearance"),
      keywords: ["theme", "appearance", "dark", "light", "toggle", "thème", "clair", "sombre"],
      icon: theme === "dark" ? Sun : Moon,
      perform: onToggleTheme,
    });

    return list;
  }, [
    instances,
    canEdit,
    admin,
    providers,
    library,
    marketplaceEntries,
    mode,
    theme,
    onToggleTheme,
    runInstanceAction,
    testProvider,
    toggleModeWithToast,
    openSkillDelete,
    t,
  ]);

  const recents = useMemo<Command[]>(() => {
    const list: Command[] = [];
    for (const entry of history) {
      const path = paletteHistoryPath(entry);
      if (!path) continue;
      list.push({
        key: paletteHistoryKey(entry),
        group: "recents",
        label: entry.label,
        detail: entry.id,
        keywords: [entry.label, entry.id, entry.kind],
        icon: historyIcon(entry),
        hit: { kind: entry.kind, id: entry.id, label: entry.label },
        perform: () => navigate(path),
      });
      if (list.length === PALETTE_HISTORY_LIMIT) break;
    }
    if (history.length > 0) {
      list.push({
        key: "action:history-clear",
        group: "recents",
        label: t("palette.historyClear"),
        keywords: ["clear", "history", "effacer", "historique"],
        icon: Eraser,
        perform: () => {
          clearPaletteHistory();
          setHistory([]);
          toast.success(t("palette.historyCleared"));
        },
      });
    }
    return list;
  }, [history, t]);

  const groups = useMemo(() => {
    const phrase = query.trim().toLowerCase().replace(/\s+/g, " ");
    const tokens = phrase.split(" ").filter(Boolean);
    const hasQuery = tokens.length > 0;
    const seen = new Set<string>();
    const result: { key: PaletteGroup; label: string; items: Command[] }[] = [];

    for (const key of GROUP_ORDER) {
      let candidates: Command[];
      if (key === "recents") {
        candidates = hasQuery ? [] : recents;
      } else {
        candidates = commands.filter((command) => command.group === key);
        if (!hasQuery) {
          candidates = candidates.filter((command) => {
            if (command.queryOnly) return false;
            if (key === "agents") return command.agentOpen === true;
            if (key === "pages" && simple) return command.advanced !== true;
            return true;
          });
        } else {
          candidates = candidates
            .map((command) => ({ command, score: scoreOf(command, tokens, phrase) }))
            .filter(
              (entry): entry is { command: Command; score: number } => entry.score !== null,
            )
            .sort((left, right) => right.score - left.score)
            .map((entry) => entry.command);
        }
      }

      const items: Command[] = [];
      const perType = new Map<LibraryType, number>();
      for (const command of candidates) {
        if (seen.has(command.key)) continue;
        if (key === "library" && command.libraryType) {
          const count = perType.get(command.libraryType) ?? 0;
          if (count >= LIBRARY_PER_TYPE) continue;
          perType.set(command.libraryType, count + 1);
        }
        seen.add(command.key);
        items.push(command);
        if (items.length >= GROUP_LIMIT[key]) break;
      }
      if (items.length > 0) {
        result.push({ key, label: t(GROUP_LABEL[key]), items });
      }
    }
    return result;
  }, [commands, recents, query, simple, t]);

  const rows = useMemo(
    () => groups.flatMap((group) => group.items.map((command) => ({ command, group: group.key }))),
    [groups],
  );
  const indexByKey = useMemo(
    () => new Map(rows.map((row, index) => [row.command.key, index])),
    [rows],
  );
  const activeIndex = rows.length === 0 ? -1 : Math.min(active, rows.length - 1);
  const total = rows.length;
  const busy = loadingInstances || loadingLibrary || loadingMarketplace;
  const confirmingCommand =
    confirming !== null
      ? (rows.find((row) => row.command.key === confirming)?.command ?? null)
      : null;

  useEffect(() => {
    if (!open || activeIndex < 0) return;
    document.getElementById(`palette-option-${activeIndex}`)?.scrollIntoView({ block: "nearest" });
  }, [open, activeIndex, rows]);

  // Moving the highlight cancels an armed confirmation: the second Enter must
  // land on the same row the operator confirmed.
  useEffect(() => {
    if (!confirming) return;
    if (rows[activeIndex]?.command.key !== confirming) setConfirming(null);
  }, [activeIndex, confirming, rows]);

  function invoke(command: Command) {
    if (command.destructive) {
      if (confirming !== command.key) {
        setConfirming(command.key);
        return;
      }
      setConfirming(null);
      onOpenChange(false);
      command.perform();
      return;
    }
    setConfirming(null);
    onOpenChange(false);
    command.perform();
    if (command.hit) setHistory(rememberPaletteHit(command.hit));
  }

  function onInputKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Escape" && confirming) {
      event.preventDefault();
      event.stopPropagation();
      setConfirming(null);
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setConfirming(null);
      if (rows.length === 0) return;
      const delta = event.key === "ArrowDown" ? 1 : -1;
      setActive((current) => {
        if (current < 0) return delta === 1 ? 0 : rows.length - 1;
        return (current + delta + rows.length) % rows.length;
      });
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const row = rows[activeIndex];
      if (row) invoke(row.command);
    }
  }

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent
          className="top-[8%] max-h-[min(32rem,82dvh)] translate-y-0 gap-0 overflow-hidden p-0 sm:top-[12%] sm:max-w-xl"
          showCloseButton={false}
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            inputRef.current?.focus();
          }}
          onEscapeKeyDown={(event) => {
            if (!confirming) return;
            event.preventDefault();
            setConfirming(null);
          }}
        >
          <DialogTitle className="sr-only">{t("palette.title")}</DialogTitle>
          <DialogDescription className="sr-only">{t("palette.description")}</DialogDescription>

          <div className="flex items-center gap-2 border-b px-3">
            <Search aria-hidden className="size-4 shrink-0 text-muted-foreground" />
            <input
              ref={inputRef}
              type="text"
              role="combobox"
              aria-expanded="true"
              aria-controls="command-palette-list"
              aria-autocomplete="list"
              aria-activedescendant={
                activeIndex >= 0 ? `palette-option-${activeIndex}` : undefined
              }
              aria-label={t("palette.inputAria")}
              placeholder={t("palette.inputPlaceholder")}
              autoComplete="off"
              spellCheck={false}
              value={query}
              onChange={(event) => {
                setQuery(event.target.value);
                setActive(0);
                setConfirming(null);
              }}
              onKeyDown={onInputKeyDown}
              className="h-12 w-full min-w-0 bg-transparent text-base outline-none placeholder:text-muted-foreground md:text-sm"
            />
            <Kbd className="shrink-0">Esc</Kbd>
          </div>

          <div
            id="command-palette-list"
            role="listbox"
            aria-label={t("palette.commands")}
            aria-busy={busy}
            className="max-h-[min(24rem,62dvh)] overflow-y-auto overscroll-contain p-1.5"
          >
            {loadingInstances && instances === null ? (
              <p
                role="status"
                className="flex items-center gap-2 px-2 py-3 text-sm text-muted-foreground"
              >
                <Loader2 aria-hidden className="size-4 animate-spin" />
                {t("palette.loadingInstances")}
              </p>
            ) : null}

            {instancesError ? (
              <p role="status" className="px-2 py-3 text-sm text-destructive">
                {t("palette.instancesUnavailable", { message: instancesError })}
              </p>
            ) : null}

            {groups.map((group) => (
              <div key={group.key} role="group" aria-label={group.label}>
                <div className="px-2 pt-2 pb-1 text-xs font-medium text-muted-foreground">
                  {group.label}
                </div>
                {group.items.map((item) => {
                  const index = indexByKey.get(item.key) ?? -1;
                  const selected = index === activeIndex;
                  const arming = confirming === item.key;
                  const Icon = arming ? TriangleAlert : item.icon;
                  return (
                    <div
                      key={item.key}
                      id={`palette-option-${index}`}
                      role="option"
                      aria-selected={selected}
                      aria-label={arming ? t("palette.confirmDelete") : item.label}
                      onClick={() => {
                        setActive(index);
                        invoke(item);
                      }}
                      onMouseMove={() => {
                        if (!selected) setActive(index);
                      }}
                      className={cn(
                        "flex min-h-11 cursor-pointer items-center gap-2.5 rounded-md px-2 py-1.5 text-sm select-none md:min-h-9",
                        selected ? "bg-accent text-accent-foreground" : "text-foreground",
                        arming && "bg-destructive/10 text-destructive",
                      )}
                    >
                      <Icon
                        aria-hidden
                        className={cn(
                          "size-4 shrink-0",
                          item.destructive ? "text-destructive" : "text-muted-foreground",
                        )}
                      />
                      <span className="min-w-0 flex-1 truncate">
                        {arming ? t("palette.confirmDelete") : item.label}
                      </span>
                      {item.advanced && simple ? (
                        <Badge variant="outline" className="shrink-0">
                          {t("palette.badgeAdvanced")}
                        </Badge>
                      ) : null}
                      {item.detail && !arming ? (
                        <span className="hidden max-w-40 truncate text-xs text-muted-foreground sm:block">
                          {item.detail}
                        </span>
                      ) : null}
                      <Kbd aria-hidden className="shrink-0">
                        ↵
                      </Kbd>
                    </div>
                  );
                })}
              </div>
            ))}

            {!busy && total === 0 ? (
              <p role="status" className="px-3 py-10 text-center text-sm text-muted-foreground">
                {query.trim()
                  ? t("palette.noMatch", { query: query.trim() })
                  : t("palette.empty")}
              </p>
            ) : null}

            {loadingLibrary && library === null && total === 0 ? (
              <p
                role="status"
                className="flex items-center gap-2 px-2 py-3 text-sm text-muted-foreground"
              >
                <Loader2 aria-hidden className="size-4 animate-spin" />
                {t("palette.loadingLibrary")}
              </p>
            ) : null}
          </div>

          <div aria-live="assertive" className="sr-only">
            {confirmingCommand
              ? `${confirmingCommand.label} — ${t("palette.confirmDelete")}`
              : ""}
          </div>

          <div className="flex items-center justify-between gap-3 border-t px-3 py-2 text-xs text-muted-foreground">
            <span className="flex min-w-0 items-center gap-3">
              {confirming ? (
                <>
                  <span className="flex items-center gap-1">
                    <Kbd aria-hidden>↵</Kbd>
                    <span className="ml-0.5">{t("palette.hintConfirm")}</span>
                  </span>
                  <span className="flex items-center gap-1">
                    <Kbd aria-hidden>Esc</Kbd>
                    <span className="ml-0.5">{t("palette.hintCancel")}</span>
                  </span>
                </>
              ) : (
                <>
                  <span className="flex items-center gap-1">
                    <Kbd aria-hidden>↑</Kbd>
                    <Kbd aria-hidden>↓</Kbd>
                    <span className="ml-0.5">{t("palette.navigate")}</span>
                  </span>
                  <span className="flex items-center gap-1">
                    <Kbd aria-hidden>↵</Kbd>
                    <span className="ml-0.5">{t("palette.run")}</span>
                  </span>
                  <span className="hidden items-center gap-1 sm:flex">
                    <Kbd aria-hidden>Esc</Kbd>
                    <span className="ml-0.5">{t("palette.hintClose")}</span>
                  </span>
                </>
              )}
            </span>
            <span aria-live="polite" className="truncate">
              {t(total === 1 ? "palette.resultOne" : "palette.resultMany", { count: total })}
            </span>
          </div>
        </DialogContent>
      </Dialog>

      <DeleteInstanceDialog
        instance={deleteTarget}
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        onDeleted={() => {
          const id = deleteTarget?.id;
          setDeleteOpen(false);
          setDeleteTarget(null);
          notifyInstancesChanged();
          if (id) {
            setInstances((current) =>
              current ? current.filter((instance) => instance.id !== id) : current,
            );
            toast.success(t("fleet.deletedId", { id }));
          }
        }}
      />

      {skillTarget ? (
        <SkillDeleteDialog
          skill={skillTarget}
          instances={instances ?? []}
          open={skillDeleteOpen}
          onOpenChange={setSkillDeleteOpen}
          onDeleted={() => {
            const name = skillTarget.name;
            setSkillDeleteOpen(false);
            setSkillTarget(null);
            setLibrary((current) =>
              current
                ? { ...current, skills: current.skills.filter((skill) => skill.name !== name) }
                : current,
            );
            toast.success(t("skills.detail.deleted", { name }));
          }}
        />
      ) : null}
    </>
  );
}
