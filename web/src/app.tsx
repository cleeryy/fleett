import { useEffect, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { api } from "@/api";
import type { Health } from "@/api/types";
import { AppShell, type Crumb } from "@/components/app-shell";
import { Button } from "@/components/ui/button";
import { Toaster } from "@/components/ui/sonner";
import { PageHeader } from "@/components/page-header";
import { navigate, useRoute } from "@/lib/router";
import { tokenStore } from "@/lib/token";
import { FleetPage } from "@/pages/fleet";
import { InstanceDetailPage } from "@/pages/instance-detail";
import { NewInstancePage } from "@/pages/instance-new";
import { LoginPage } from "@/pages/login";
import { SharesPage } from "@/pages/shares";
import { TemplatesPage } from "@/pages/templates";

type Theme = "dark" | "light";

const THEME_KEY = "fleet.theme";

function titleFor(path: string): string {
  if (path === "/") return "Instances";
  if (path === "/instances") return "Instances";
  if (path === "/instances/new") return "New instance";
  if (path.startsWith("/instances/")) return decodeURIComponent(path.slice("/instances/".length));
  if (path === "/shares") return "Shares";
  if (path === "/templates") return "Templates";
  if (path === "/login") return "Sign in";
  return "Not found";
}

function readTheme(): Theme {
  try {
    return localStorage.getItem(THEME_KEY) === "light" ? "light" : "dark";
  } catch {
    return "dark";
  }
}

export default function App() {
  const route = useRoute();
  const [token, setToken] = useState<string | null>(() => tokenStore.get());
  const [theme, setTheme] = useState<Theme>(readTheme);
  const [health, setHealth] = useState<Health | null>(null);
  const [healthFailed, setHealthFailed] = useState(false);
  const tokenRef = useRef(token);

  useEffect(() => {
    tokenRef.current = token;
  }, [token]);

  useEffect(() => {
    document.documentElement.classList.toggle("dark", theme === "dark");
    const themeColor = document.querySelector('meta[name="theme-color"]');
    if (themeColor) {
      themeColor.setAttribute("content", theme === "dark" ? "#0a0a0a" : "#ffffff");
    }
    try {
      localStorage.setItem(THEME_KEY, theme);
    } catch {
      /* ignore storage failures */
    }
  }, [theme]);

  useEffect(() => {
    if (!token) {
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
  }, [token]);

  useEffect(() => {
    function onUnauthorized() {
      // Several requests can 401 at once; only the first one clears and notifies.
      if (!tokenRef.current || !tokenStore.get()) return;
      tokenStore.clear();
      setToken(null);
      toast.error("Session expired. Sign in again.");
    }
    window.addEventListener("fleet:unauthorized", onUnauthorized);
    return () => window.removeEventListener("fleet:unauthorized", onUnauthorized);
  }, []);

  useEffect(() => {
    if (!token) return;
    if (route.path === "/" || route.path === "/login") {
      navigate("/instances", { replace: true });
    }
  }, [token, route.path]);

  useEffect(() => {
    window.scrollTo(0, 0);
    document.title = `${titleFor(route.path)} · hermes-fleet`;
  }, [route.path]);

  if (!token) {
    return (
      <>
        <LoginPage onSignedIn={() => setToken(tokenStore.get())} />
        <Toaster theme={theme} position="bottom-right" />
      </>
    );
  }

  const path = route.path;
  if (path === "/" || path === "/login") return null;

  let crumbs: Crumb[] = [];
  let screen: ReactNode = null;

  const detailMatch = /^\/instances\/([^/]+)$/.exec(path);

  if (path === "/instances") {
    crumbs = [{ label: "Instances" }];
    screen = <FleetPage />;
  } else if (path === "/instances/new") {
    crumbs = [{ label: "Instances", href: "#/instances" }, { label: "New instance" }];
    screen = <NewInstancePage />;
  } else if (detailMatch) {
    const id = decodeURIComponent(detailMatch[1]);
    crumbs = [{ label: "Instances", href: "#/instances" }, { label: id, mono: true }];
    screen = <InstanceDetailPage key={id} id={id} />;
  } else if (path === "/shares") {
    crumbs = [{ label: "Shares" }];
    screen = <SharesPage />;
  } else if (path === "/templates") {
    crumbs = [{ label: "Templates" }];
    screen = <TemplatesPage />;
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
      <AppShell
        path={path}
        crumbs={crumbs}
        health={health}
        healthFailed={healthFailed}
        theme={theme}
        onToggleTheme={() => setTheme((current) => (current === "dark" ? "light" : "dark"))}
        onSignOut={() => {
          tokenStore.clear();
          setToken(null);
          toast.info("Signed out.");
        }}
      >
        {screen}
      </AppShell>
      <Toaster theme={theme} position="bottom-right" />
    </>
  );
}
