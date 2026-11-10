import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Boxes,
  ExternalLink,
  FileClock,
  Fingerprint,
  FolderTree,
  KeyRound,
  LayoutTemplate,
  ListChecks,
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
  type LucideIcon,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError } from "@/api/errors";
import type { InstanceSummary } from "@/api/types";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import { notifyInstancesChanged } from "@/lib/events";
import { useI18n } from "@/lib/i18n";
import { navigate } from "@/lib/router";
import { useCanEdit, useIdentity } from "@/lib/session";
import { cn } from "@/lib/utils";

type GroupKey = "instances" | "navigation" | "actions";

const GROUP_ORDER: GroupKey[] = ["instances", "navigation", "actions"];

const GROUP_LABEL: Record<GroupKey, string> = {
  instances: "palette.groupInstances",
  navigation: "palette.groupNavigation",
  actions: "palette.groupActions",
};

interface Command {
  id: string;
  group: GroupKey;
  label: string;
  /** Muted text on the right: a status, an id, a model. */
  detail?: string;
  keywords: string[];
  icon: LucideIcon;
  perform: () => void;
}

interface NavigationItem {
  label: string;
  href: string;
  icon: LucideIcon;
}

/**
 * Skills (#54) owns its own screen later. Probe for the page instead of
 * listing a route that would land on "Not found".
 */
const SKILLS_SCREEN = Object.keys(import.meta.glob("/src/pages/skills.tsx")).length > 0;

function navigationItems(t: (key: string) => string): NavigationItem[] {
  const items: NavigationItem[] = [
    { label: t("nav.instances"), href: "/instances", icon: Boxes },
    { label: t("nav.providers"), href: "/providers", icon: Plug },
    { label: t("nav.personas"), href: "/personas", icon: Fingerprint },
    { label: t("nav.shares"), href: "/shares", icon: FolderTree },
    { label: t("nav.templates"), href: "/templates", icon: LayoutTemplate },
    { label: t("nav.setup"), href: "/setup", icon: ListChecks },
    { label: t("nav.tokens"), href: "/tokens", icon: KeyRound },
    { label: t("nav.audit"), href: "/audit", icon: FileClock },
  ];
  if (SKILLS_SCREEN) {
    items.splice(4, 0, { label: t("nav.skills"), href: "/skills", icon: Sparkles });
  }
  items.splice(5, 0, { label: t("nav.marketplace"), href: "/marketplace", icon: Store });
  items.splice(6, 0, { label: t("nav.mcpServers"), href: "/mcp-servers", icon: Server });
  return items;
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

  const inputRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [instances, setInstances] = useState<InstanceSummary[] | null>(null);
  const [instancesError, setInstancesError] = useState<string | null>(null);
  const [loadingInstances, setLoadingInstances] = useState(false);

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

  // A fresh search every time the palette opens; instances change under us.
  useEffect(() => {
    if (!open) return;
    setQuery("");
    setActive(0);
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
    return () => {
      alive = false;
    };
  }, [open]);

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

  const commands = useMemo<Command[]>(() => {
    const list: Command[] = [];
    const sorted = [...(instances ?? [])].sort((a, b) => a.name.localeCompare(b.name));

    for (const instance of sorted) {
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
        id: `open:${instance.id}`,
        group: "instances",
        label: t("palette.open", { name: instance.name }),
        detail,
        keywords: [...base, "open", "details", "overview"],
        icon: ExternalLink,
        perform: () => navigate(path),
      });

      if (canEdit) {
        if (instance.status !== "running") {
          list.push({
            id: `start:${instance.id}`,
            group: "instances",
            label: t("palette.start", { name: instance.name }),
            detail: instance.id,
            keywords: [...base, "start", "run", "up"],
            icon: Play,
            perform: () => void runInstanceAction(instance, "start"),
          });
        }
        if (instance.status === "running") {
          list.push({
            id: `stop:${instance.id}`,
            group: "instances",
            label: t("palette.stop", { name: instance.name }),
            detail: instance.id,
            keywords: [...base, "stop", "halt", "down"],
            icon: Square,
            perform: () => void runInstanceAction(instance, "stop"),
          });
        }
        list.push({
          id: `restart:${instance.id}`,
          group: "instances",
          label: t("palette.restart", { name: instance.name }),
          detail: instance.id,
          keywords: [...base, "restart", "reload", "bounce"],
          icon: RotateCw,
          perform: () => void runInstanceAction(instance, "restart"),
        });
      }

      list.push({
        id: `logs:${instance.id}`,
        group: "instances",
        label: t("palette.logs", { name: instance.name }),
        detail: instance.id,
        keywords: [...base, "logs", "log", "output", "tail"],
        icon: ScrollText,
        perform: () => navigate(`${path}?tab=logs`),
      });
      list.push({
        id: `files:${instance.id}`,
        group: "instances",
        label: t("palette.files", { name: instance.name }),
        detail: instance.id,
        keywords: [...base, "files", "file", "data", "soul"],
        icon: FolderTree,
        perform: () => navigate(`${path}?tab=files`),
      });
      list.push({
        id: `env:${instance.id}`,
        group: "instances",
        label: t("palette.env", { name: instance.name }),
        detail: instance.id,
        keywords: [...base, "env", "environment", "variables", "secrets"],
        icon: SlidersHorizontal,
        perform: () => navigate(`${path}?tab=configuration`),
      });
      list.push({
        id: `mcp:${instance.id}`,
        group: "instances",
        label: t("palette.mcp", { name: instance.name }),
        detail: instance.id,
        keywords: [...base, "mcp", "servers", "tools", "attach"],
        icon: Server,
        perform: () => navigate(`${path}?tab=mcp`),
      });
    }

    for (const item of navigationItems(t)) {
      if ((item.href === "/tokens" || item.href === "/audit") && !admin) continue;
      list.push({
        id: `go:${item.href}`,
        group: "navigation",
        label: item.label,
        keywords: [item.label, "go", "open", "page", "navigate"],
        icon: item.icon,
        perform: () => navigate(item.href),
      });
    }

    if (canEdit) {
      list.push({
        id: "new:instance",
        group: "actions",
        label: t("nav.newInstance"),
        detail: t("palette.composeProject"),
        keywords: ["new", "create", "instance", "agent"],
        icon: Plus,
        perform: () => navigate("/instances/new"),
      });
      list.push({
        id: "new:provider",
        group: "actions",
        label: t("providers.form.newTitle"),
        detail: t("palette.sharedCredentials"),
        keywords: ["new", "create", "provider", "model", "key"],
        icon: Plug,
        perform: () => navigate("/providers?new=1"),
      });
    }

    list.push({
      id: "theme",
      group: "actions",
      label: theme === "dark" ? t("shell.switchToLight") : t("shell.switchToDark"),
      detail: t("palette.appearance"),
      keywords: ["theme", "appearance", "dark", "light", "toggle"],
      icon: theme === "dark" ? Sun : Moon,
      perform: onToggleTheme,
    });

    return list;
  }, [instances, canEdit, admin, theme, onToggleTheme, runInstanceAction, t]);

  const groups = useMemo(() => {
    const phrase = query.trim().toLowerCase().replace(/\s+/g, " ");
    const tokens = phrase.split(" ").filter(Boolean);

    return GROUP_ORDER.map((key) => {
      let items = commands.filter((command) => command.group === key);
      if (tokens.length === 0) {
        // A fresh palette is a shortlist, not seven rows per instance.
        if (key === "instances") items = items.filter((command) => command.id.startsWith("open:"));
      } else {
        items = items
          .map((command) => ({ command, score: scoreOf(command, tokens, phrase) }))
          .filter((entry): entry is { command: Command; score: number } => entry.score !== null)
          .sort((a, b) => b.score - a.score)
          .map((entry) => entry.command);
      }
      return { key, label: t(GROUP_LABEL[key]), items: items.slice(0, 12) };
    }).filter((group) => group.items.length > 0);
  }, [commands, query, t]);

  const rows = useMemo(
    () => groups.flatMap((group) => group.items.map((item) => ({ item, group: group.key }))),
    [groups],
  );
  const indexById = useMemo(
    () => new Map(rows.map((row, index) => [row.item.id, index])),
    [rows],
  );
  const activeIndex = rows.length === 0 ? -1 : Math.min(active, rows.length - 1);

  useEffect(() => {
    if (!open || activeIndex < 0) return;
    const node = document.getElementById(rows[activeIndex]?.item.id ?? "");
    node?.scrollIntoView({ block: "nearest" });
  }, [open, activeIndex, rows]);

  function invoke(command: Command) {
    onOpenChange(false);
    command.perform();
  }

  function onInputKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
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
      if (row) invoke(row.item);
    }
  }

  const total = rows.length;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="top-[8%] max-h-[min(32rem,82dvh)] translate-y-0 gap-0 overflow-hidden p-0 sm:top-[12%] sm:max-w-xl"
        showCloseButton={false}
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          inputRef.current?.focus();
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
            aria-activedescendant={activeIndex >= 0 ? rows[activeIndex]?.item.id : undefined}
            aria-label={t("palette.inputAria")}
            placeholder={t("palette.inputPlaceholder")}
            autoComplete="off"
            spellCheck={false}
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setActive(0);
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
          aria-busy={loadingInstances}
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
                const index = indexById.get(item.id) ?? -1;
                const selected = index === activeIndex;
                const Icon = item.icon;
                return (
                  <div
                    key={item.id}
                    id={item.id}
                    role="option"
                    aria-selected={selected}
                    data-index={index}
                    onClick={() => invoke(item)}
                    onMouseMove={() => {
                      if (!selected) setActive(index);
                    }}
                    className={cn(
                      "flex min-h-11 cursor-pointer items-center gap-2.5 rounded-md px-2 py-1.5 text-sm select-none md:min-h-9",
                      selected ? "bg-accent text-accent-foreground" : "text-foreground",
                    )}
                  >
                    <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                    <span className="min-w-0 flex-1 truncate">{item.label}</span>
                    {item.detail ? (
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

          {!loadingInstances && total === 0 ? (
            <p role="status" className="px-3 py-10 text-center text-sm text-muted-foreground">
              {t("palette.noMatch", { query: query.trim() })}
            </p>
          ) : null}
        </div>

        <div className="flex items-center justify-between gap-3 border-t px-3 py-2 text-xs text-muted-foreground">
          <span className="flex items-center gap-3">
            <span className="flex items-center gap-1">
              <Kbd aria-hidden>↑</Kbd>
              <Kbd aria-hidden>↓</Kbd>
              <span className="ml-0.5">{t("palette.navigate")}</span>
            </span>
            <span className="flex items-center gap-1">
              <Kbd aria-hidden>↵</Kbd>
              <span className="ml-0.5">{t("palette.run")}</span>
            </span>
          </span>
          <span className="truncate">
            {t(total === 1 ? "palette.resultOne" : "palette.resultMany", { count: total })}
          </span>
        </div>
      </DialogContent>
    </Dialog>
  );
}
