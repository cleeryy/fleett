import { httpApi } from "./http";
import { mockApi } from "./mock";
import type { AuthConfig, FleetApi } from "./types";

/**
 * VITE_MOCK wins when set. Otherwise the mock adapter is on in development and
 * off in a production build (the Rust server serves dist and the real API).
 */
function resolveMock(): boolean {
  const raw = (import.meta.env.VITE_MOCK as string | undefined)?.trim().toLowerCase();
  if (!raw) return import.meta.env.DEV;
  return raw !== "0" && raw !== "false" && raw !== "no";
}

export const isMock = resolveMock();

export const api: FleetApi = isMock ? mockApi : httpApi;

/** Used when `/api/auth/config` is unreachable: the console still offers a token. */
export const FALLBACK_AUTH_CONFIG: AuthConfig = {
  mode: "bearer",
  oidc: { enabled: false, login_url: "/auth/login" },
  bearer: { enabled: true },
};

export const DEMO_TOKEN = "demo";
/** Mock only: signs in as a `viewer` to check the read-only console. */
export const DEMO_VIEWER_TOKEN = "demo-viewer";
