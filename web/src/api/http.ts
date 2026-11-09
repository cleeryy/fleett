import { ApiError } from "./errors";
import { readCookie } from "@/lib/cookies";
import { tokenStore } from "@/lib/token";
import type {
  AuditEntry,
  AuthConfig,
  CreateInstanceInput,
  CreateProviderInput,
  CreateTokenInput,
  CreatedToken,
  EnvInput,
  FileContent,
  FileEntry,
  FleetApi,
  GitDiff,
  GitDiffOptions,
  GitLog,
  GitLogOptions,
  GitStatus,
  Health,
  Identity,
  InstanceDetail,
  InstanceSummary,
  LifecycleResult,
  MkdirResult,
  PatchInstanceInput,
  PatchProviderInput,
  ProviderDetail,
  ProviderRotateResult,
  ProviderSummary,
  ProviderTestResult,
  RevertInput,
  RevertResult,
  Share,
  ShareMode,
  Template,
  TokenInfo,
  WriteFileInput,
  WriteFileResult,
} from "./types";

/** Empty base = same origin. Vite proxies /api and /auth to the controller in development. */
const BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? "";

const MUTATIONS = new Set(["POST", "PATCH", "PUT", "DELETE"]);

interface RequestOptions {
  /** Boot and credential checks must not fire the global "session expired" event. */
  signalUnauthorized?: boolean;
}

function isAbsoluteUrl(value: string): boolean {
  return /^https?:\/\//i.test(value);
}

/** Keep `/auth/login` same-origin, or prefix it when VITE_API_BASE points elsewhere. */
function authUrl(loginUrl: string): string {
  if (isAbsoluteUrl(loginUrl)) return loginUrl;
  return `${BASE}${loginUrl.startsWith("/") ? loginUrl : `/${loginUrl}`}`;
}

async function request<T>(path: string, init: RequestInit = {}, options: RequestOptions = {}): Promise<T> {
  const token = tokenStore.get();
  const headers = new Headers(init.headers);
  if (token) headers.set("Authorization", `Bearer ${token}`);

  const method = (init.method ?? "GET").toUpperCase();
  if (MUTATIONS.has(method)) {
    // Cookie sessions need a double-submit proof; bearer clients are exempt and
    // the cookie is simply absent. Harmless in every mode.
    const csrf = readCookie("fleett_csrf");
    if (csrf) headers.set("X-Fleett-CSRF", csrf);
  }
  if (init.body !== undefined && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }

  let response: Response;
  try {
    response = await fetch(`${BASE}${path}`, {
      ...init,
      headers,
      credentials: "same-origin",
    });
  } catch {
    throw new ApiError(0, "network_error", "Controller unreachable. Check that the backend is running.");
  }

  if (!response.ok) {
    let code = "http_error";
    let message = response.statusText || `HTTP ${response.status}`;
    let details: unknown;
    const contentType = response.headers.get("content-type") ?? "";
    if (contentType.includes("application/json")) {
      const body = (await response.json().catch(() => null)) as {
        error?: { code?: string; message?: string; details?: unknown };
      } | null;
      if (body?.error?.code) code = body.error.code;
      if (body?.error?.message) message = body.error.message;
      details = body?.error?.details;
    }

    // Turn the two role/CSRF refusals into something a person can act on. The
    // server wording stays visible for every other error.
    if (code === "csrf_required") {
      message =
        "The browser session is missing its security token (CSRF). Reload the page and sign in again.";
    } else if (code === "forbidden") {
      message = `${message.replace(/\.$/, "")}. This account cannot make that change.`;
    }

    if (response.status === 401 && options.signalUnauthorized !== false) {
      window.dispatchEvent(new CustomEvent("fleett:unauthorized"));
    }
    throw new ApiError(response.status, code, message, details);
  }

  if (response.status === 204) return undefined as T;
  const contentType = response.headers.get("content-type") ?? "";
  if (contentType.includes("application/json")) return (await response.json()) as T;
  return (await response.text()) as T;
}

export const httpApi: FleetApi = {
  health: () => request<Health>("/api/health"),

  authConfig: () => request<AuthConfig>("/api/auth/config", {}, { signalUnauthorized: false }),

  me: () => request<Identity>("/api/auth/me", {}, { signalUnauthorized: false }),

  checkAuth: () => request<void>("/api/auth/check", {}, { signalUnauthorized: false }),

  async signInWithOidc(loginUrl: string): Promise<void> {
    window.location.assign(authUrl(loginUrl));
    // The browser is leaving this document; never resolve so the caller cannot
    // race the navigation with an /api/auth/me call.
    await new Promise<void>(() => undefined);
  },

  logout: () => request<void>("/auth/logout", { method: "POST" }, { signalUnauthorized: false }),

  listTemplates: () => request<Template[]>("/api/templates"),

  listProviders: () => request<ProviderSummary[]>("/api/providers"),

  createProvider: (input: CreateProviderInput) =>
    request<ProviderSummary>("/api/providers", { method: "POST", body: JSON.stringify(input) }),

  getProvider: (id) => request<ProviderDetail>(`/api/providers/${encodeURIComponent(id)}`),

  patchProvider: (id, input: PatchProviderInput) =>
    request<ProviderRotateResult>(`/api/providers/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  deleteProvider: (id) =>
    request<void>(`/api/providers/${encodeURIComponent(id)}`, { method: "DELETE" }),

  testProvider: (id) =>
    request<ProviderTestResult>(`/api/providers/${encodeURIComponent(id)}/test`, { method: "POST" }),

  listInstances: () => request<InstanceSummary[]>("/api/instances"),

  getInstance: (id) => request<InstanceDetail>(`/api/instances/${encodeURIComponent(id)}`),

  putInstanceEnv: (id: string, env: EnvInput[]) =>
    request<InstanceDetail>(`/api/instances/${encodeURIComponent(id)}/env`, {
      method: "PUT",
      body: JSON.stringify({ env }),
    }),

  deleteInstanceEnv: (id: string, key: string) =>
    request<void>(`/api/instances/${encodeURIComponent(id)}/env/${encodeURIComponent(key)}`, {
      method: "DELETE",
    }),

  createInstance: (input: CreateInstanceInput) =>
    request<InstanceDetail>("/api/instances", { method: "POST", body: JSON.stringify(input) }),

  cloneInstance: (id, input) =>
    request<InstanceDetail>(`/api/instances/${encodeURIComponent(id)}/clone`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  patchInstance: (id, input: PatchInstanceInput) =>
    request<InstanceDetail>(`/api/instances/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  action: (id, action) =>
    request<LifecycleResult>(`/api/instances/${encodeURIComponent(id)}/${action}`, { method: "POST" }),

  deleteInstance: (id, purge) =>
    request<void>(`/api/instances/${encodeURIComponent(id)}${purge ? "?purge=true" : ""}`, {
      method: "DELETE",
    }),

  logs: (id, tail) => request<string>(`/api/instances/${encodeURIComponent(id)}/logs?tail=${tail}`),

  listFiles: (id, path) =>
    request<FileEntry[]>(`/api/instances/${encodeURIComponent(id)}/files?path=${encodeURIComponent(path)}`),

  readFile: (id, path) =>
    request<FileContent>(
      `/api/instances/${encodeURIComponent(id)}/files/content?path=${encodeURIComponent(path)}`,
    ),

  writeFile: (id, input: WriteFileInput) => {
    const query = input.restart ? "?restart=true" : "";
    const headers: Record<string, string> = {};
    if (input.force) {
      // `*` means "the file must exist, any version": the documented override.
      headers["If-Match"] = "*";
    }
    return request<WriteFileResult>(`/api/instances/${encodeURIComponent(id)}/files/content${query}`, {
      method: "PUT",
      headers,
      body: JSON.stringify({
        path: input.path,
        content: input.content,
        etag: input.force ? undefined : (input.etag ?? undefined),
      }),
    });
  },

  deleteFile: (id, path) =>
    request<void>(
      `/api/instances/${encodeURIComponent(id)}/files/content?path=${encodeURIComponent(path)}`,
      { method: "DELETE" },
    ),

  mkdir: (id, path) =>
    request<MkdirResult>(`/api/instances/${encodeURIComponent(id)}/files/mkdir`, {
      method: "POST",
      body: JSON.stringify({ path }),
    }),

  gitLog: (id, options: GitLogOptions = {}) => {
    const query = new URLSearchParams();
    if (options.path) query.set("path", options.path);
    if (options.limit) query.set("limit", String(options.limit));
    const params = query.toString();
    const suffix = params ? `?${params}` : "";
    return request<GitLog>(`/api/instances/${encodeURIComponent(id)}/git/log${suffix}`);
  },

  gitDiff: (id, options: GitDiffOptions = {}) => {
    const query = new URLSearchParams();
    if (options.from) query.set("from", options.from);
    if (options.to) query.set("to", options.to);
    const params = query.toString();
    const suffix = params ? `?${params}` : "";
    return request<GitDiff>(`/api/instances/${encodeURIComponent(id)}/git/diff${suffix}`);
  },

  gitStatus: (id) => request<GitStatus>(`/api/instances/${encodeURIComponent(id)}/git/status`),

  gitRevert: (id, input: RevertInput) =>
    request<RevertResult>(`/api/instances/${encodeURIComponent(id)}/git/revert`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  listShares: () => request<Share[]>("/api/shares"),

  createShare: (name) =>
    request<void>("/api/shares", { method: "POST", body: JSON.stringify({ name }) }),

  deleteShare: (name) =>
    request<void>(`/api/shares/${encodeURIComponent(name)}`, { method: "DELETE" }),

  attachShare: (name, instance, mode: ShareMode) =>
    request<void>(`/api/shares/${encodeURIComponent(name)}/attach`, {
      method: "POST",
      body: JSON.stringify({ instance, mode }),
    }),

  detachShare: (name, instance) =>
    request<void>(`/api/shares/${encodeURIComponent(name)}/detach`, {
      method: "POST",
      body: JSON.stringify({ instance }),
    }),

  listTokens: () => request<TokenInfo[]>("/api/tokens"),

  createToken: (input: CreateTokenInput) =>
    request<CreatedToken>("/api/tokens", { method: "POST", body: JSON.stringify(input) }),

  revokeToken: (id) =>
    request<void>(`/api/tokens/${encodeURIComponent(id)}`, { method: "DELETE" }),

  listAudit: (limit) => request<AuditEntry[]>(`/api/audit?limit=${limit}`),
};
