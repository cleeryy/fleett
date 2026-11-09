import { useState, type ReactNode } from "react";
import {
  Boxes,
  Brain,
  CalendarClock,
  CircleDollarSign,
  FileClock,
  Fingerprint,
  FolderTree,
  KeyRound,
  LayoutTemplate,
  ListChecks,
  LogOut,
  Menu,
  MessagesSquare,
  Moon,
  Plug,
  Rocket,
  ScrollText,
  Search,
  ShieldCheck,
  Sparkles,
  Sun,
  TriangleAlert,
  Workflow,
  type LucideIcon,
} from "lucide-react";
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
import { useI18n } from "@/lib/i18n";
import { identityLabel } from "@/lib/session";
import { COMMAND_KEY_ARIA, COMMAND_KEY_LABEL } from "@/lib/shortcut";
import { cn } from "@/lib/utils";

export interface Crumb {
  label: string;
  href?: string;
  mono?: boolean;
}

interface NavItem {
  key: string;
  /** Dictionary key under `nav.*`; the label is translated at render time. */
  labelKey: string;
  href: string;
  icon: LucideIcon;
  /** Hidden from `viewer` accounts, and the route refuses them. */
  adminOnly?: boolean;
}

const NAV: NavItem[] = [
  { key: "instances", labelKey: "nav.instances", href: "#/instances", icon: Boxes },
  { key: "schedules", labelKey: "nav.schedules", href: "#/schedules", icon: CalendarClock, adminOnly: true },
  { key: "pipelines", labelKey: "nav.pipelines", href: "#/pipelines", icon: Workflow },
  { key: "incidents", labelKey: "nav.incidents", href: "#/incidents", icon: TriangleAlert },
  { key: "cost", labelKey: "nav.cost", href: "#/cost", icon: CircleDollarSign },
  { key: "logs", labelKey: "nav.logs", href: "#/logs", icon: ScrollText },
  { key: "deployments", labelKey: "nav.deployments", href: "#/deployments", icon: Rocket },
  { key: "security", labelKey: "nav.security", href: "#/security", icon: ShieldCheck, adminOnly: true },
  { key: "messages", labelKey: "nav.messages", href: "#/messages", icon: MessagesSquare, adminOnly: true },
  { key: "memory", labelKey: "nav.memory", href: "#/memory", icon: Brain },
  { key: "providers", labelKey: "nav.providers", href: "#/providers", icon: Plug },
  { key: "shares", labelKey: "nav.shares", href: "#/shares", icon: FolderTree },
  { key: "templates", labelKey: "nav.templates", href: "#/templates", icon: LayoutTemplate },
  { key: "personas", labelKey: "nav.personas", href: "#/personas", icon: Fingerprint },
  { key: "skills", labelKey: "nav.skills", href: "#/skills", icon: Sparkles },
  { key: "setup", labelKey: "nav.setup", href: "#/setup", icon: ListChecks },
  { key: "tokens", labelKey: "nav.tokens", href: "#/tokens", icon: KeyRound, adminOnly: true },
  { key: "audit", labelKey: "nav.audit", href: "#/audit", icon: FileClock, adminOnly: true },
];

function activeKey(path: string): string {
  if (path.startsWith("/schedules")) return "schedules";
  if (path.startsWith("/pipelines")) return "pipelines";
  if (path.startsWith("/incidents")) return "incidents";
  if (path.startsWith("/cost")) return "cost";
  if (path.startsWith("/logs")) return "logs";
  if (path.startsWith("/deployments")) return "deployments";
  if (path.startsWith("/security")) return "security";
  if (path.startsWith("/providers")) return "providers";
  if (path.startsWith("/shares")) return "shares";
  if (path.startsWith("/templates")) return "templates";
  if (path.startsWith("/personas")) return "personas";
  if (path.startsWith("/skills")) return "skills";
  if (path.startsWith("/setup")) return "setup";
  if (path.startsWith("/tokens")) return "tokens";
  if (path.startsWith("/audit")) return "audit";
  if (path.startsWith("/messages")) return "messages";
  if (path.startsWith("/memory")) return "memory";
  return "instances";
}

function SideNav({
  path,
  identity,
  health,
  healthFailed,
  theme,
  onToggleTheme,
  onSignOut,
  onNavigate,
}: {
  path: string;
  identity: Identity;
  health: Health | null;
  healthFailed: boolean;
  theme: "dark" | "light";
  onToggleTheme: () => void;
  onSignOut: () => void;
  onNavigate?: () => void;
}) {
  const { t } = useI18n();
  const active = activeKey(path);
  const dockerOk = health?.docker.ok ?? true;
  const admin = identity.role === "admin";
  const viewer = !admin;
  const items = NAV.filter((item) => !item.adminOnly || admin);

  return (
    <div className="flex h-full flex-col bg-background">
      <div className="flex h-14 items-center gap-2 border-b px-4 pr-12">
        <div className="flex size-7 shrink-0 items-center justify-center rounded-md bg-primary text-primary-foreground">
          <Boxes aria-hidden className="size-4" />
        </div>
        <span className="text-sm font-semibold tracking-tight">fleett</span>
      </div>

      <nav aria-label={t("nav.consoleSections")} className="flex-1 space-y-1 overflow-y-auto p-2">
        {items.map((item) => {
          const Icon = item.icon;
          const isActive = item.key === active;
          return (
            <a
              key={item.key}
              href={item.href}
              onClick={onNavigate}
              aria-current={isActive ? "page" : undefined}
              className={cn(
                "flex min-h-11 items-center gap-2.5 rounded-md px-3 text-sm outline-none transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 md:min-h-9",
                isActive
                  ? "bg-accent font-medium text-accent-foreground"
                  : "text-muted-foreground hover:bg-accent/60 hover:text-foreground",
              )}
            >
              <Icon aria-hidden className="size-4 shrink-0" />
              {t(item.labelKey)}
            </a>
          );
        })}
      </nav>

      <div className="space-y-3 border-t p-3 pb-[calc(0.75rem+env(safe-area-inset-bottom))]">
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
            {viewer ? (
              <Badge variant="outline" className="font-mono text-[11px] font-normal">
                {t("shell.readOnly")}
              </Badge>
            ) : null}
          </div>
          {viewer ? (
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

        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="w-full justify-start"
          onClick={onToggleTheme}
          aria-label={theme === "dark" ? t("shell.switchToLight") : t("shell.switchToDark")}
        >
          {theme === "dark" ? <Sun aria-hidden /> : <Moon aria-hidden />}
          {theme === "dark" ? t("shell.lightTheme") : t("shell.darkTheme")}
        </Button>
        <div className="flex items-center justify-between gap-2 px-0.5">
          <span className="text-xs text-muted-foreground">{t("shell.language")}</span>
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
  const { t } = useI18n();

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

      <aside className="hidden w-56 shrink-0 border-r md:block">
        <div className="sticky top-0 h-dvh">
          <SideNav
            path={path}
            identity={identity}
            health={health}
            healthFailed={healthFailed}
            theme={theme}
            onToggleTheme={onToggleTheme}
            onSignOut={onSignOut}
          />
        </div>
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="sticky top-0 z-20 flex min-h-14 items-center gap-3 border-b bg-background px-4 pt-[env(safe-area-inset-top)] md:px-6">
          <Sheet open={navOpen} onOpenChange={setNavOpen}>
            <SheetTrigger asChild>
              <Button variant="ghost" size="icon-sm" className="md:hidden" aria-label={t("nav.openNavigation")}>
                <Menu aria-hidden />
              </Button>
            </SheetTrigger>
            <SheetContent side="left" className="w-64 p-0 pt-[env(safe-area-inset-top)]">
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
                onToggleTheme={onToggleTheme}
                onSignOut={onSignOut}
                onNavigate={() => setNavOpen(false)}
              />
            </SheetContent>
          </Sheet>

          <nav aria-label={t("nav.breadcrumb")} className="min-w-0 flex-1">
            <ol className="flex min-w-0 items-center gap-2 text-sm">
              {crumbs.map((crumb, index) => (
                <li key={`${crumb.label}-${index}`} className="flex min-w-0 items-center gap-2">
                  {index > 0 ? (
                    <span aria-hidden className="text-muted-foreground/60">
                      /
                    </span>
                  ) : null}
                  {crumb.href ? (
                    <a href={crumb.href} className="truncate text-muted-foreground hover:text-foreground">
                      {crumb.label}
                    </a>
                  ) : (
                    <span
                      aria-current="page"
                      className={cn("truncate font-medium", crumb.mono && "font-mono text-xs")}
                    >
                      {crumb.label}
                    </span>
                  )}
                </li>
              ))}
            </ol>
          </nav>

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
