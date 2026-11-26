import { useEffect, useState, type ReactNode } from "react";
import { Boxes, LogOut, Menu, Moon, Search, Sun } from "lucide-react";
import type { Health, Identity } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { LanguageToggle } from "@/components/language-toggle";
import { ModeToggle } from "@/components/mode-toggle";
import { NavGroup, NavLink } from "@/components/nav-group";
import { useI18n } from "@/lib/i18n";
import { useUiMode } from "@/lib/mode";
import {
  activeLeaf,
  groupOf,
  NAV_GROUPS,
  NAV_SPACES,
  navSelection,
  type Crumb,
  type GroupKey,
} from "@/lib/navigation";
import { identityLabel } from "@/lib/session";
import { COMMAND_KEY_ARIA, COMMAND_KEY_LABEL } from "@/lib/shortcut";
import { cn } from "@/lib/utils";

export type { Crumb };

const GROUPS_KEY = "fleett.nav.groups";

type GroupOpen = Record<GroupKey, boolean>;

const GROUPS_CLOSED: GroupOpen = { automate: false, data: false, system: false };

/** Opens are a quiet preference: a blocked storage keeps the default. */
function readGroupOpen(): GroupOpen {
  try {
    const raw = localStorage.getItem(GROUPS_KEY);
    if (!raw) return GROUPS_CLOSED;
    const parsed = JSON.parse(raw) as Partial<Record<GroupKey, unknown>>;
    return {
      automate: parsed.automate === true,
      data: parsed.data === true,
      system: parsed.system === true,
    };
  } catch {
    return GROUPS_CLOSED;
  }
}

function writeGroupOpen(value: GroupOpen): void {
  try {
    localStorage.setItem(GROUPS_KEY, JSON.stringify(value));
  } catch {
    /* ignore storage failures */
  }
}

function SideNav({
  path,
  identity,
  health,
  healthFailed,
  theme,
  groupOpen,
  onToggleGroup,
  onToggleTheme,
  onSignOut,
  onNavigate,
  showModeToggle = false,
}: {
  path: string;
  identity: Identity;
  health: Health | null;
  healthFailed: boolean;
  theme: "dark" | "light";
  groupOpen: GroupOpen;
  onToggleGroup: (key: GroupKey) => void;
  onToggleTheme: () => void;
  onSignOut: () => void;
  onNavigate?: () => void;
  showModeToggle?: boolean;
}) {
  const { t } = useI18n();
  const { mode } = useUiMode();
  const active = navSelection(path, mode);
  const dockerOk = health?.docker.ok ?? true;
  const admin = identity.role === "admin";

  return (
    <div className="flex h-full flex-col bg-background">
      <div className="flex h-14 items-center gap-2 border-b px-4 pr-12">
        <div className="flex size-7 shrink-0 items-center justify-center rounded-md bg-primary text-primary-foreground">
          <Boxes aria-hidden className="size-4" />
        </div>
        <span className="text-sm font-semibold tracking-tight">fleett</span>
      </div>

      {showModeToggle ? (
        <div className="border-b px-3 py-2.5">
          <ModeToggle size="md" fullWidth />
        </div>
      ) : null}

      <nav aria-label={t("nav.consoleSections")} className="flex-1 overflow-y-auto px-2 py-2">
        <div className="space-y-0.5">
          {NAV_SPACES.map((space) => (
            <NavLink
              key={space.key}
              entry={space}
              active={space.key === active}
              onNavigate={onNavigate}
            />
          ))}
        </div>

        {mode === "advanced" ? (
          <div className="mt-4 space-y-1 border-t pt-3">
            <p className="px-2.5 pb-0.5 text-[11px] font-medium tracking-wider text-muted-foreground/70 uppercase">
              {t("nav.advancedTools")}
            </p>
            {NAV_GROUPS.map((group) => (
              <NavGroup
                key={group.key}
                group={group}
                activeKey={active}
                open={groupOpen[group.key]}
                admin={admin}
                onToggle={() => onToggleGroup(group.key)}
                onNavigate={onNavigate}
              />
            ))}
          </div>
        ) : null}
      </nav>

      <div className="space-y-3 border-t bg-background p-3 pb-[calc(0.75rem+env(safe-area-inset-bottom))]">
        <div className="space-y-1.5 rounded-md border bg-muted/30 p-2.5">
          <p
            className="truncate text-sm font-medium"
            title={`${identity.actor} · ${identity.method}`}
          >
            {identityLabel(identity)}
          </p>
          <div className="flex flex-wrap items-center gap-1.5">
            <Badge
              variant={admin ? "default" : "secondary"}
              className="font-mono text-[11px] font-normal"
            >
              {t(`role.${identity.role}`)}
            </Badge>
            {!admin ? (
              <Badge variant="outline" className="font-mono text-[11px] font-normal">
                {t("shell.readOnly")}
              </Badge>
            ) : null}
          </div>
          {!admin ? (
            <p className="text-xs text-muted-foreground">{t("shell.readOnlyHint")}</p>
          ) : null}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="mt-1 w-full justify-start"
            onClick={onSignOut}
          >
            <LogOut aria-hidden />
            {t("shell.signOut")}
          </Button>
        </div>

        <div
          className="space-y-1 px-1"
          title={
            health
              ? t("shell.driverTitle", {
                  driver: health.driver,
                  docker: dockerOk ? t("shell.dockerOk") : t("shell.unreachable"),
                })
              : undefined
          }
        >
          <p className="flex items-center gap-2 font-mono text-xs text-muted-foreground">
            <span
              aria-hidden
              className={cn(
                "size-1.5 shrink-0 rounded-full",
                healthFailed ? "bg-destructive" : dockerOk ? "bg-emerald-500" : "bg-destructive",
              )}
            />
            <span className={cn("truncate", healthFailed && "text-destructive")}>
              {health
                ? `${health.driver} · v${health.version}`
                : healthFailed
                  ? t("shell.unreachable")
                  : t("shell.connecting")}
            </span>
          </p>
          {health && !dockerOk ? (
            <p className="pl-3.5 text-xs text-destructive">{t("shell.dockerUnreachable")}</p>
          ) : null}
        </div>

        <div className="flex items-center justify-between gap-2">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="justify-start px-2"
            onClick={onToggleTheme}
            aria-label={theme === "dark" ? t("shell.switchToLight") : t("shell.switchToDark")}
          >
            {theme === "dark" ? <Sun aria-hidden /> : <Moon aria-hidden />}
            {theme === "dark" ? t("shell.lightTheme") : t("shell.darkTheme")}
          </Button>
          <LanguageToggle />
        </div>
      </div>
    </div>
  );
}

export function AppShell({
  path,
  crumbs,
  identity,
  health,
  healthFailed,
  theme,
  onToggleTheme,
  onSignOut,
  onOpenCommand,
  children,
}: {
  path: string;
  crumbs: Crumb[];
  identity: Identity;
  health: Health | null;
  healthFailed: boolean;
  theme: "dark" | "light";
  onToggleTheme: () => void;
  onSignOut: () => void;
  onOpenCommand: () => void;
  children: ReactNode;
}) {
  const [navOpen, setNavOpen] = useState(false);
  const [groupOpen, setGroupOpen] = useState<GroupOpen>(readGroupOpen);
  const { t } = useI18n();
  const { mode } = useUiMode();

  // Crossing into the desktop layout must not leave the mobile sheet on top
  // of the permanent sidebar.
  useEffect(() => {
    const query = window.matchMedia("(min-width: 768px)");
    function closeAtDesktop(event: MediaQueryListEvent) {
      if (event.matches) setNavOpen(false);
    }
    query.addEventListener("change", closeAtDesktop);
    return () => query.removeEventListener("change", closeAtDesktop);
  }, []);

  // A direct link can land inside a collapsed group. Open that one group
  // once, without overriding a collapse the operator does afterwards.
  useEffect(() => {
    if (mode !== "advanced") return;
    const group = groupOf(activeLeaf(path));
    if (!group) return;
    setGroupOpen((current) => {
      if (current[group.key]) return current;
      const next = { ...current, [group.key]: true };
      writeGroupOpen(next);
      return next;
    });
  }, [mode, path]);

  function toggleGroup(key: GroupKey) {
    setGroupOpen((current) => {
      const next = { ...current, [key]: !current[key] };
      writeGroupOpen(next);
      return next;
    });
  }

  return (
    <div className="flex min-h-dvh bg-background">
      <a
        href="#main"
        onClick={(event) => {
          event.preventDefault();
          document.getElementById("main")?.focus();
        }}
        className="fixed top-3 left-3 z-50 -translate-y-20 rounded-md border bg-background px-3 py-2 text-sm font-medium shadow-sm focus:translate-y-0"
      >
        {t("nav.skipToContent")}
      </a>

      <aside className="hidden w-60 shrink-0 border-r md:block">
        <div className="sticky top-0 h-dvh">
          <SideNav
            path={path}
            identity={identity}
            health={health}
            healthFailed={healthFailed}
            theme={theme}
            groupOpen={groupOpen}
            onToggleGroup={toggleGroup}
            onToggleTheme={onToggleTheme}
            onSignOut={onSignOut}
          />
        </div>
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="sticky top-0 z-20 flex min-h-14 items-center gap-3 border-b bg-background/95 px-4 pt-[env(safe-area-inset-top)] backdrop-blur supports-[backdrop-filter]:bg-background/80 md:px-6">
          <Sheet open={navOpen} onOpenChange={setNavOpen}>
            <SheetTrigger asChild>
              <Button variant="ghost" size="icon-sm" className="md:hidden" aria-label={t("nav.openNavigation")}>
                <Menu aria-hidden />
              </Button>
            </SheetTrigger>
            <SheetContent side="left" className="w-72 p-0 pt-[env(safe-area-inset-top)]">
              <SheetHeader className="sr-only">
                <SheetTitle>{t("nav.navigation")}</SheetTitle>
                <SheetDescription>{t("nav.consoleSections")}</SheetDescription>
              </SheetHeader>
              <SideNav
                path={path}
                identity={identity}
                health={health}
                healthFailed={healthFailed}
                theme={theme}
                groupOpen={groupOpen}
                onToggleGroup={toggleGroup}
                onToggleTheme={onToggleTheme}
                onSignOut={onSignOut}
                onNavigate={() => setNavOpen(false)}
                showModeToggle
              />
            </SheetContent>
          </Sheet>

          <nav aria-label={t("nav.breadcrumb")} className="min-w-0 flex-1">
            <ol className="flex min-w-0 items-center gap-1.5 text-sm sm:gap-2">
              {crumbs.map((crumb, index) => {
                const last = index === crumbs.length - 1;
                return (
                  <li
                    key={`${crumb.label}-${index}`}
                    className={cn(
                      "flex min-w-0 items-center gap-1.5 sm:gap-2",
                      index < crumbs.length - 2 && "hidden sm:flex",
                    )}
                  >
                    {index > 0 ? (
                      <span aria-hidden className="text-muted-foreground/50">
                        /
                      </span>
                    ) : null}
                    {crumb.href && !last ? (
                      <a
                        href={crumb.href}
                        className="truncate text-muted-foreground transition-colors outline-none hover:text-foreground focus-visible:rounded-sm"
                      >
                        {crumb.label}
                      </a>
                    ) : (
                      <span
                        className={cn(
                          "truncate",
                          last ? "font-medium text-foreground" : "text-muted-foreground",
                          crumb.mono && "font-mono text-xs",
                        )}
                      >
                        {crumb.label}
                      </span>
                    )}
                  </li>
                );
              })}
            </ol>
          </nav>

          <ModeToggle className="hidden shrink-0 md:flex" />

          {/* One palette, two entry points: a labelled button on desktop and
              the same dialog behind an icon on a phone. */}
          <Button
            variant="outline"
            size="sm"
            onClick={onOpenCommand}
            aria-keyshortcuts={COMMAND_KEY_ARIA}
            className="hidden w-56 shrink-0 justify-start gap-2 font-normal text-muted-foreground md:inline-flex"
          >
            <Search aria-hidden />
            <span>{t("nav.search")}</span>
            <Kbd className="ml-auto">{COMMAND_KEY_LABEL}</Kbd>
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={onOpenCommand}
            aria-label={t("nav.searchCommands")}
            aria-keyshortcuts={COMMAND_KEY_ARIA}
            title={t("nav.searchCommandsTitle", { shortcut: COMMAND_KEY_LABEL })}
            className="shrink-0 md:hidden"
          >
            <Search aria-hidden />
          </Button>
        </header>

        <main
          id="main"
          tabIndex={-1}
          className="mx-auto w-full max-w-[1400px] flex-1 space-y-6 px-4 py-6 pb-[calc(1.5rem+env(safe-area-inset-bottom))] outline-none md:px-6 md:py-8"
        >
          {children}
        </main>
      </div>
    </div>
  );
}
