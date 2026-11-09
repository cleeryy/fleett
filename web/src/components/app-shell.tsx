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
  LogOut,
  Menu,
  MessagesSquare,
  Moon,
  Plug,
  Rocket,
  ScrollText,
  ShieldCheck,
  Sun,
  TriangleAlert,
  Workflow,
  type LucideIcon,
} from "lucide-react";
import type { Health, Identity } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { identityLabel } from "@/lib/session";
import { cn } from "@/lib/utils";

export interface Crumb {
  label: string;
  href?: string;
  mono?: boolean;
}

interface NavItem {
  key: string;
  label: string;
  href: string;
  icon: LucideIcon;
  /** Hidden from `viewer` accounts, and the route refuses them. */
  adminOnly?: boolean;
}

const NAV: NavItem[] = [
  { key: "instances", label: "Instances", href: "#/instances", icon: Boxes },
  { key: "schedules", label: "Schedules", href: "#/schedules", icon: CalendarClock, adminOnly: true },
  { key: "pipelines", label: "Pipelines", href: "#/pipelines", icon: Workflow },
  { key: "incidents", label: "Incidents", href: "#/incidents", icon: TriangleAlert },
  { key: "cost", label: "Cost", href: "#/cost", icon: CircleDollarSign },
  { key: "logs", label: "Logs", href: "#/logs", icon: ScrollText },
  { key: "deployments", label: "Deployments", href: "#/deployments", icon: Rocket },
  { key: "security", label: "Security", href: "#/security", icon: ShieldCheck, adminOnly: true },
  { key: "messages", label: "Messages", href: "#/messages", icon: MessagesSquare, adminOnly: true },
  { key: "memory", label: "Memory", href: "#/memory", icon: Brain },
  { key: "providers", label: "Providers", href: "#/providers", icon: Plug },
  { key: "shares", label: "Shares", href: "#/shares", icon: FolderTree },
  { key: "templates", label: "Templates", href: "#/templates", icon: LayoutTemplate },
  { key: "personas", label: "Personas", href: "#/personas", icon: Fingerprint },
  { key: "tokens", label: "Tokens", href: "#/tokens", icon: KeyRound, adminOnly: true },
  { key: "audit", label: "Audit log", href: "#/audit", icon: FileClock, adminOnly: true },
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

      <nav aria-label="Console sections" className="flex-1 space-y-1 overflow-y-auto p-2">
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
              {item.label}
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
              {identity.role}
            </Badge>
            {viewer ? (
              <Badge variant="outline" className="font-mono text-[11px] font-normal">
                read-only
              </Badge>
            ) : null}
          </div>
          {viewer ? (
            <p className="text-xs text-muted-foreground">Changes are disabled for this role.</p>
          ) : null}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="mt-1 w-full justify-start"
            onClick={onSignOut}
          >
            <LogOut aria-hidden />
            Sign out
          </Button>
        </div>

        <div
          className="space-y-1 px-1"
          title={health ? `driver ${health.driver} · docker ${dockerOk ? "ok" : "unreachable"}` : undefined}
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
              {health ? `${health.driver} · v${health.version}` : healthFailed ? "unreachable" : "connecting…"}
            </span>
          </p>
          {health && !dockerOk ? (
            <p className="pl-3.5 text-xs text-destructive">Docker unreachable</p>
          ) : null}
        </div>

        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="w-full justify-start"
          onClick={onToggleTheme}
          aria-label={theme === "dark" ? "Switch to light theme" : "Switch to dark theme"}
        >
          {theme === "dark" ? <Sun aria-hidden /> : <Moon aria-hidden />}
          {theme === "dark" ? "Light theme" : "Dark theme"}
        </Button>
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
  children: ReactNode;
}) {
  const [navOpen, setNavOpen] = useState(false);

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
        Skip to content
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
              <Button variant="ghost" size="icon-sm" className="md:hidden" aria-label="Open navigation">
                <Menu aria-hidden />
              </Button>
            </SheetTrigger>
            <SheetContent side="left" className="w-64 p-0 pt-[env(safe-area-inset-top)]">
              <SheetHeader className="sr-only">
                <SheetTitle>Navigation</SheetTitle>
                <SheetDescription>Console sections</SheetDescription>
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

          <nav aria-label="Breadcrumb" className="min-w-0">
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
