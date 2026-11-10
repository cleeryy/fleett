import { useEffect, useRef, useState, type ReactNode } from "react";
import { Boxes, Loader2, LockKeyhole } from "lucide-react";
import { toast } from "sonner";
import { api, FALLBACK_AUTH_CONFIG } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { AuthConfig, Health, Identity } from "@/api/types";
import { AppShell, type Crumb } from "@/components/app-shell";
import { AssistantPanel } from "@/components/assistant";
import { CommandPalette } from "@/components/command-palette";
import { Button } from "@/components/ui/button";
import { Toaster } from "@/components/ui/sonner";
import { EmptyState } from "@/components/empty-state";
import { PageHeader } from "@/components/page-header";
import { useI18n } from "@/lib/i18n";
import { navigate, useRoute } from "@/lib/router";
import { SessionContext } from "@/lib/session";
import { tokenStore } from "@/lib/token";
import { AuditPage } from "@/pages/audit";
import { CostPage } from "@/pages/cost";
import { DeploymentsPage } from "@/pages/deployments";
import { FleetPage } from "@/pages/fleet";
import { IncidentsPage } from "@/pages/incidents";
import { InstanceDetailPage } from "@/pages/instance-detail";
import { NewInstancePage } from "@/pages/instance-new";
import { LoginPage } from "@/pages/login";
import { LogsPage } from "@/pages/logs";
import { MarketplacePage } from "@/pages/marketplace";
import { McpServerDetailPage } from "@/pages/mcp-server-detail";
import { McpServersPage } from "@/pages/mcp-servers";
import { MemoryPage } from "@/pages/memory";
import { MessagesPage } from "@/pages/messages";
import { PersonaDetailPage } from "@/pages/persona-detail";
import { PersonasPage } from "@/pages/personas";
import { PipelinesPage } from "@/pages/pipelines";
import { ProviderDetailPage } from "@/pages/provider-detail";
import { ProvidersPage } from "@/pages/providers";
import { SchedulesPage } from "@/pages/schedules";
import { SecurityPage } from "@/pages/security";
import { SetupPage } from "@/pages/setup";
import { SharesPage } from "@/pages/shares";
import { SkillDetailPage } from "@/pages/skill-detail";
import { SkillsPage } from "@/pages/skills";
import { TemplatesPage } from "@/pages/templates";
import { TokensPage } from "@/pages/tokens";

type Theme = "dark" | "light";

const THEME_KEY = "fleett.theme";
// Key written before the rename; still read so an open tab keeps its theme.
const LEGACY_THEME_KEY = "fleet.theme";

type Translate = ReturnType<typeof useI18n>["t"];

function titleFor(path: string, t: Translate): string {
  if (path === "/") return t("nav.instances");
  if (path === "/instances") return t("nav.instances");
  if (path === "/instances/new") return t("nav.newInstance");
  if (path.startsWith("/instances/")) return decodeURIComponent(path.slice("/instances/".length));
  if (path === "/providers") return t("nav.providers");
  if (path.startsWith("/providers/")) return t("nav.providerDetail");
  if (path === "/shares") return t("nav.shares");
  if (path === "/templates") return t("nav.templates");
  if (path === "/personas") return t("nav.personas");
  if (path.startsWith("/personas/")) return t("nav.personaDetail");
  if (path === "/skills") return t("nav.skills");
  if (path.startsWith("/skills/")) return t("nav.skillDetail");
  if (path === "/marketplace") return t("nav.marketplace");
  if (path === "/mcp-servers") return t("nav.mcpServers");
  if (path.startsWith("/mcp-servers/")) return t("nav.mcpServerDetail");
  if (path === "/tokens") return t("nav.tokens");
  if (path === "/audit") return t("nav.audit");
  if (path === "/schedules") return t("nav.schedules");
  if (path === "/pipelines") return t("nav.pipelines");
  if (path === "/incidents") return t("nav.incidents");
  if (path === "/cost") return t("nav.cost");
  if (path === "/logs") return t("nav.logs");
  if (path === "/security") return t("nav.security");
  if (path === "/deployments") return t("nav.deployments");
  if (path === "/messages") return t("nav.messages");
  if (path === "/memory") return t("nav.memory");
  if (path === "/setup") return t("nav.setup");
  if (path === "/login") return t("nav.login");
  return t("nav.notFound");
}

function readTheme(): Theme {
  try {
    const stored = localStorage.getItem(THEME_KEY) ?? localStorage.getItem(LEGACY_THEME_KEY);
    return stored === "light" ? "light" : "dark";
  } catch {
    return "dark";
  }
}

function BootScreen() {
  const { t } = useI18n();
  return (
    <div className="flex min-h-dvh items-center justify-center bg-background px-4">
      <div className="flex items-center gap-2 text-sm text-muted-foreground" aria-live="polite">
        <Boxes aria-hidden className="size-4" />
        <Loader2 aria-hidden className="size-4 animate-spin" />
        {t("shell.bootConnecting")}
      </div>
    </div>
  );
}

function RestrictedScreen({ title, message }: { title: string; message: string }) {
  const { t } = useI18n();
  return (
    <>
      <PageHeader title={title} description={message} />
      <div className="rounded-md border">
        <EmptyState
          icon={LockKeyhole}
          title={t("shell.notAvailable")}
          action={
            <Button asChild variant="outline" size="sm">
              <a href="#/instances">{t("shell.backToInstances")}</a>
            </Button>
          }
        />
      </div>
    </>
  );
}

export default function App() {
  const { t, lang } = useI18n();
  const route = useRoute();
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [authConfig, setAuthConfig] = useState<AuthConfig>(FALLBACK_AUTH_CONFIG);
  const [booting, setBooting] = useState(true);
  const [bootError, setBootError] = useState<ApiError | null>(null);
  const [theme, setTheme] = useState<Theme>(readTheme);
  const [health, setHealth] = useState<Health | null>(null);
  const [healthFailed, setHealthFailed] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const identityRef = useRef<Identity | null>(null);

  useEffect(() => {
    identityRef.current = identity;
  }, [identity]);

  useEffect(() => {
    document.documentElement.classList.toggle("dark", theme === "dark");
    const themeColor = document.querySelector('meta[name="theme-color"]');
    if (themeColor) {
      themeColor.setAttribute("content", theme === "dark" ? "#0a0a0a" : "#ffffff");
    }
    try {
      localStorage.setItem(THEME_KEY, theme);
      localStorage.removeItem(LEGACY_THEME_KEY);
    } catch {
      /* ignore storage failures */
    }
  }, [theme]);

  // Boot: advertise the credential types, then ask who we are. A 401 here is
  // the normal signed-out answer, never a reason to retry in a loop.
  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const config = await api.authConfig();
        if (alive) setAuthConfig(config);
      } catch {
        if (alive) setAuthConfig(FALLBACK_AUTH_CONFIG);
      }

      try {
        const me = await api.me();
        if (!alive) return;
        setIdentity(me);
        setBootError(null);
      } catch (cause) {
        const error = toApiError(cause);
        if (alive && error.status !== 401) setBootError(error);
      } finally {
        if (alive) setBooting(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    if (!identity) {
      setHealth(null);
      setHealthFailed(false);
      return;
    }
    let alive = true;
    api
      .health()
      .then((value) => {
        if (!alive) return;
        setHealth(value);
        setHealthFailed(false);
      })
      .catch(() => {
        if (!alive) return;
        setHealth(null);
        setHealthFailed(true);
      });
    return () => {
      alive = false;
    };
  }, [identity]);

  useEffect(() => {
    function onUnauthorized() {
      // A burst of parallel calls can 401 together; only the first one clears.
      if (!identityRef.current) return;
      identityRef.current = null;
      tokenStore.clear();
      setIdentity(null);
      setBootError(null);
      setPaletteOpen(false);
      toast.error(t("shell.sessionExpired"));
    }
    window.addEventListener("fleett:unauthorized", onUnauthorized);
    return () => window.removeEventListener("fleett:unauthorized", onUnauthorized);
    // The listener is registered once; `t` is read from the latest render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!identity) return;
    if (route.path === "/" || route.path === "/login") {
      navigate("/instances", { replace: true });
    }
  }, [identity, route.path]);

  useEffect(() => {
    window.scrollTo(0, 0);
    document.title = `${identity ? titleFor(route.path, t) : t("nav.login")} · fleett`;
  }, [identity, route.path, t, lang]);

  function handleSignedIn(next: Identity) {
    identityRef.current = next;
    setIdentity(next);
    setBootError(null);
  }

  function toggleTheme() {
    setTheme((current) => (current === "dark" ? "light" : "dark"));
  }

  async function handleSignOut() {
    setPaletteOpen(false);
    const method = identity?.method;
    try {
      // Sessions live in cookies the server must clear. With OIDC enabled the
      // call is harmless even on a bearer login and removes a stale cookie.
      if (method === "session" || authConfig.oidc.enabled) {
        await api.logout().catch(() => undefined);
      }
    } finally {
      tokenStore.clear();
      // Reload so the boot check runs with no credential left behind.
      window.location.reload();
    }
  }

  if (booting) {
    return (
      <>
        <BootScreen />
        <Toaster theme={theme} position="bottom-right" />
      </>
    );
  }

  if (!identity) {
    return (
      <>
        <LoginPage config={authConfig} bootError={bootError} onSignedIn={handleSignedIn} />
        <Toaster theme={theme} position="bottom-right" />
      </>
    );
  }

  const path = route.path;
  if (path === "/" || path === "/login") return null;

  const admin = identity.role === "admin";
  const canEdit = identity.role !== "viewer";

  let crumbs: Crumb[] = [];
  let screen: ReactNode = null;

  const detailMatch = /^\/instances\/([^/]+)$/.exec(path);
  const providerMatch = /^\/providers\/([^/]+)$/.exec(path);
  const personaMatch = /^\/personas\/([^/]+)$/.exec(path);
  const skillMatch = /^\/skills\/([^/]+)$/.exec(path);
  const mcpMatch = /^\/mcp-servers\/([^/]+)$/.exec(path);

  if (path === "/instances") {
    crumbs = [{ label: t("nav.instances") }];
    screen = <FleetPage />;
  } else if (path === "/instances/new") {
    crumbs = [{ label: t("nav.instances"), href: "#/instances" }, { label: t("nav.newInstance") }];
    screen = canEdit ? (
      <NewInstancePage />
    ) : (
      <RestrictedScreen
        title={t("nav.newInstance")}
        message={t("shell.restricted.newInstance")}
      />
    );
  } else if (detailMatch) {
    const id = decodeURIComponent(detailMatch[1]);
    crumbs = [{ label: t("nav.instances"), href: "#/instances" }, { label: id, mono: true }];
    screen = <InstanceDetailPage key={id} id={id} />;
  } else if (path === "/providers") {
    crumbs = [{ label: t("nav.providers") }];
    screen = <ProvidersPage />;
  } else if (providerMatch) {
    const id = decodeURIComponent(providerMatch[1]);
    crumbs = [{ label: t("nav.providers"), href: "#/providers" }, { label: t("nav.providerDetail") }];
    screen = <ProviderDetailPage key={id} id={id} />;
  } else if (path === "/shares") {
    crumbs = [{ label: t("nav.shares") }];
    screen = <SharesPage />;
  } else if (path === "/templates") {
    crumbs = [{ label: t("nav.templates") }];
    screen = <TemplatesPage />;
  } else if (path === "/personas") {
    crumbs = [{ label: t("nav.personas") }];
    screen = <PersonasPage />;
  } else if (personaMatch) {
    const id = decodeURIComponent(personaMatch[1]);
    crumbs = [{ label: t("nav.personas"), href: "#/personas" }, { label: id, mono: true }];
    screen = <PersonaDetailPage key={id} id={id} />;
  } else if (path === "/skills") {
    crumbs = [{ label: t("nav.skills") }];
    screen = <SkillsPage />;
  } else if (skillMatch) {
    const name = decodeURIComponent(skillMatch[1]);
    crumbs = [{ label: t("nav.skills"), href: "#/skills" }, { label: name, mono: true }];
    screen = <SkillDetailPage key={name} name={name} />;
  } else if (path === "/marketplace") {
    crumbs = [{ label: t("nav.marketplace") }];
    screen = <MarketplacePage />;
  } else if (path === "/mcp-servers") {
    crumbs = [{ label: t("nav.mcpServers") }];
    screen = <McpServersPage />;
  } else if (mcpMatch) {
    const id = decodeURIComponent(mcpMatch[1]);
    crumbs = [
      { label: t("nav.mcpServers"), href: "#/mcp-servers" },
      { label: id, mono: true },
    ];
    screen = <McpServerDetailPage key={id} id={id} />;
  } else if (path === "/schedules") {
    crumbs = [{ label: t("nav.schedules") }];
    screen = admin ? (
      <SchedulesPage />
    ) : (
      <RestrictedScreen
        title={t("nav.schedules")}
        message={t("shell.restricted.schedules")}
      />
    );
  } else if (path === "/pipelines") {
    crumbs = [{ label: t("nav.pipelines") }];
    screen = <PipelinesPage />;
  } else if (path === "/setup") {
    crumbs = [{ label: t("nav.setup") }];
    screen = <SetupPage />;
  } else if (path === "/incidents") {
    crumbs = [{ label: t("nav.incidents") }];
    screen = <IncidentsPage />;
  } else if (path === "/cost") {
    crumbs = [{ label: t("nav.cost") }];
    screen = <CostPage />;
  } else if (path === "/logs") {
    crumbs = [{ label: t("nav.logs") }];
    screen = <LogsPage />;
  } else if (path === "/deployments") {
    crumbs = [{ label: t("nav.deployments") }];
    screen = <DeploymentsPage />;
  } else if (path === "/security") {
    crumbs = [{ label: t("nav.security") }];
    screen = admin ? (
      <SecurityPage />
    ) : (
      <RestrictedScreen
        title={t("nav.security")}
        message={t("shell.restricted.security")}
      />
    );
  } else if (path === "/tokens") {
    crumbs = [{ label: t("nav.tokens") }];
    screen = admin ? (
      <TokensPage />
    ) : (
      <RestrictedScreen
        title={t("nav.tokens")}
        message={t("shell.restricted.tokens")}
      />
    );
  } else if (path === "/audit") {
    crumbs = [{ label: t("nav.audit") }];
    screen = admin ? (
      <AuditPage />
    ) : (
      <RestrictedScreen
        title={t("nav.audit")}
        message={t("shell.restricted.audit")}
      />
    );
  } else if (path === "/messages") {
    crumbs = [{ label: t("nav.messages") }];
    screen = admin ? (
      <MessagesPage />
    ) : (
      <RestrictedScreen
        title={t("nav.messages")}
        message={t("shell.restricted.messages")}
      />
    );
  } else if (path === "/memory") {
    crumbs = [{ label: t("nav.memory") }];
    screen = <MemoryPage />;
  } else {
    crumbs = [{ label: t("nav.notFound") }];
    screen = (
      <div className="space-y-4">
        <PageHeader
          title={t("nav.notFound")}
          description={
            <span>
              {t("nav.notFoundPrefix")} <span className="font-mono">{path}</span>.
            </span>
          }
        />
        <Button asChild variant="outline">
          <a href="#/instances">{t("shell.backToInstances")}</a>
        </Button>
      </div>
    );
  }

  return (
    <>
      <SessionContext.Provider value={identity}>
        <AppShell
          path={path}
          crumbs={crumbs}
          identity={identity}
          health={health}
          healthFailed={healthFailed}
          theme={theme}
          onToggleTheme={toggleTheme}
          onSignOut={() => void handleSignOut()}
          onOpenCommand={() => setPaletteOpen(true)}
        >
          {screen}
        </AppShell>
        <AssistantPanel />
        <CommandPalette
          open={paletteOpen}
          onOpenChange={setPaletteOpen}
          theme={theme}
          onToggleTheme={toggleTheme}
        />
      </SessionContext.Provider>
      {/* Lifted so toasts clear the floating assistant button. */}
      <Toaster theme={theme} position="bottom-right" offset={{ bottom: 88 }} />
    </>
  );
}
