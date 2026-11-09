import { useEffect, useRef, useState, type ReactNode } from "react";
import { Boxes, Loader2, LockKeyhole } from "lucide-react";
import { toast } from "sonner";
import { api, FALLBACK_AUTH_CONFIG } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { AuthConfig, Health, Identity } from "@/api/types";
import { AppShell, type Crumb } from "@/components/app-shell";
import { Button } from "@/components/ui/button";
import { Toaster } from "@/components/ui/sonner";
import { EmptyState } from "@/components/empty-state";
import { PageHeader } from "@/components/page-header";
import { navigate, useRoute } from "@/lib/router";
import { SessionContext } from "@/lib/session";
import { tokenStore } from "@/lib/token";
import { AuditPage } from "@/pages/audit";
import { FleetPage } from "@/pages/fleet";
import { InstanceDetailPage } from "@/pages/instance-detail";
import { NewInstancePage } from "@/pages/instance-new";
import { LoginPage } from "@/pages/login";
import { MemoryPage } from "@/pages/memory";
import { MessagesPage } from "@/pages/messages";
import { ProviderDetailPage } from "@/pages/provider-detail";
import { ProvidersPage } from "@/pages/providers";
import { SharesPage } from "@/pages/shares";
import { TemplatesPage } from "@/pages/templates";
import { TokensPage } from "@/pages/tokens";

type Theme = "dark" | "light";

const THEME_KEY = "fleett.theme";
// Key written before the rename; still read so an open tab keeps its theme.
const LEGACY_THEME_KEY = "fleet.theme";

function titleFor(path: string): string {
  if (path === "/") return "Instances";
  if (path === "/instances") return "Instances";
  if (path === "/instances/new") return "New instance";
  if (path.startsWith("/instances/")) return decodeURIComponent(path.slice("/instances/".length));
  if (path === "/providers") return "Providers";
  if (path.startsWith("/providers/")) return "Provider details";
  if (path === "/shares") return "Shares";
  if (path === "/templates") return "Templates";
  if (path === "/tokens") return "Tokens";
  if (path === "/audit") return "Audit log";
  if (path === "/messages") return "Messages";
  if (path === "/memory") return "Memory";
  if (path === "/login") return "Sign in";
  return "Not found";
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
  return (
    <div className="flex min-h-dvh items-center justify-center bg-background px-4">
      <div className="flex items-center gap-2 text-sm text-muted-foreground" aria-live="polite">
        <Boxes aria-hidden className="size-4" />
        <Loader2 aria-hidden className="size-4 animate-spin" />
        Connecting…
      </div>
    </div>
  );
}

function RestrictedScreen({ title, message }: { title: string; message: string }) {
  return (
    <>
      <PageHeader title={title} description={message} />
      <div className="rounded-md border">
        <EmptyState
          icon={LockKeyhole}
          title="Not available for this account"
          action={
            <Button asChild variant="outline" size="sm">
              <a href="#/instances">Back to instances</a>
            </Button>
          }
        />
      </div>
    </>
  );
}

export default function App() {
  const route = useRoute();
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [authConfig, setAuthConfig] = useState<AuthConfig>(FALLBACK_AUTH_CONFIG);
  const [booting, setBooting] = useState(true);
  const [bootError, setBootError] = useState<ApiError | null>(null);
  const [theme, setTheme] = useState<Theme>(readTheme);
  const [health, setHealth] = useState<Health | null>(null);
  const [healthFailed, setHealthFailed] = useState(false);
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
      toast.error("Session expired. Sign in again.");
    }
    window.addEventListener("fleett:unauthorized", onUnauthorized);
    return () => window.removeEventListener("fleett:unauthorized", onUnauthorized);
  }, []);

  useEffect(() => {
    if (!identity) return;
    if (route.path === "/" || route.path === "/login") {
      navigate("/instances", { replace: true });
    }
  }, [identity, route.path]);

  useEffect(() => {
    window.scrollTo(0, 0);
    document.title = `${identity ? titleFor(route.path) : "Sign in"} · fleett`;
  }, [identity, route.path]);

  function handleSignedIn(next: Identity) {
    identityRef.current = next;
    setIdentity(next);
    setBootError(null);
  }

  async function handleSignOut() {
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

  if (path === "/instances") {
    crumbs = [{ label: "Instances" }];
    screen = <FleetPage />;
  } else if (path === "/instances/new") {
    crumbs = [{ label: "Instances", href: "#/instances" }, { label: "New instance" }];
    screen = canEdit ? (
      <NewInstancePage />
    ) : (
      <RestrictedScreen
        title="New instance"
        message="Your account has the viewer role: creating an instance is disabled. Ask an admin to make the change."
      />
    );
  } else if (detailMatch) {
    const id = decodeURIComponent(detailMatch[1]);
    crumbs = [{ label: "Instances", href: "#/instances" }, { label: id, mono: true }];
    screen = <InstanceDetailPage key={id} id={id} />;
  } else if (path === "/providers") {
    crumbs = [{ label: "Providers" }];
    screen = <ProvidersPage />;
  } else if (providerMatch) {
    const id = decodeURIComponent(providerMatch[1]);
    crumbs = [{ label: "Providers", href: "#/providers" }, { label: "Provider details" }];
    screen = <ProviderDetailPage key={id} id={id} />;
  } else if (path === "/shares") {
    crumbs = [{ label: "Shares" }];
    screen = <SharesPage />;
  } else if (path === "/templates") {
    crumbs = [{ label: "Templates" }];
    screen = <TemplatesPage />;
  } else if (path === "/tokens") {
    crumbs = [{ label: "Tokens" }];
    screen = admin ? (
      <TokensPage />
    ) : (
      <RestrictedScreen
        title="Tokens"
        message="Access tokens are managed by admins. Your account can browse the fleet but not issue credentials."
      />
    );
  } else if (path === "/audit") {
    crumbs = [{ label: "Audit log" }];
    screen = admin ? (
      <AuditPage />
    ) : (
      <RestrictedScreen
        title="Audit log"
        message="The audit trail is readable by admins only."
      />
    );
  } else if (path === "/messages") {
    crumbs = [{ label: "Messages" }];
    screen = admin ? (
      <MessagesPage />
    ) : (
      <RestrictedScreen
        title="Messages"
        message="The global bus view is readable by admins. Open an instance to read its own inbox and outbox."
      />
    );
  } else if (path === "/memory") {
    crumbs = [{ label: "Memory" }];
    screen = <MemoryPage />;
  } else {
    crumbs = [{ label: "Not found" }];
    screen = (
      <div className="space-y-4">
        <PageHeader
          title="Not found"
          description={
            <span>
              No screen at <span className="font-mono">{path}</span>.
            </span>
          }
        />
        <Button asChild variant="outline">
          <a href="#/instances">Back to instances</a>
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
          onToggleTheme={() => setTheme((current) => (current === "dark" ? "light" : "dark"))}
          onSignOut={() => void handleSignOut()}
        >
          {screen}
        </AppShell>
      </SessionContext.Provider>
      <Toaster theme={theme} position="bottom-right" />
    </>
  );
}
