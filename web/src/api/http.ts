import { ApiError } from "./errors";
import { readCookie } from "@/lib/cookies";
import { SseParser } from "@/lib/logs";
import { tokenStore } from "@/lib/token";
import type {
  AckMessageResult,
  AclItemInput,
  AssistantChatInput,
  AssistantConfig,
  AssistantReply,
  AuditEntry,
  AuthConfig,
  BudgetInput,
  BudgetStatus,
  BudgetView,
  CostReport,
  CostReportParams,
  CreateInstanceInput,
  CreateMemoryInput,
  CreatePersonaInput,
  CreatePipelineInput,
  CreateProviderInput,
  CreateScheduleInput,
  CreateSnapshotInput,
  CreateTokenInput,
  CreatedToken,
  Deployment,
  DeploymentStatus,
  DoctorReport,
  EgressPolicyInput,
  EgressPolicyView,
  EgressSummary,
  EgressTestResult,
  EnvInput,
  ExecInput,
  ExecResult,
  FileContent,
  FileEntry,
  FileSearchParams,
  FileSearchResult,
  FleetApi,
  GitDiff,
  GitDiffOptions,
  GitLog,
  GitLogOptions,
  GitStatus,
  HealPolicyInput,
  HealPolicyView,
  Health,
  Identity,
  Incident,
  InstanceAcl,
  InstanceDetail,
  InstanceLogQuery,
  InstanceMcpView,
  InstanceMessagesParams,
  InstanceSummary,
  LifecycleResult,
  ListIncidentsParams,
  ListMemoryParams,
  ListMessagesParams,
  LogEntry,
  LogExportFormat,
  LogQueryParams,
  LogSearchResponse,
  LogStreamHandlers,
  MaintenanceStatus,
  MaintenanceWindow,
  MaintenanceWindowInput,
  MarketplacePreview,
  MarketplaceSearch,
  MarketplaceSyncResult,
  MarketplaceUpdateResult,
  MarketplaceUpdates,
  McpCatalogEntry,
  McpIncident,
  McpServerView,
  McpTestResult,
  McpToolsSnapshot,
  MemoryEntry,
  MemoryVersion,
  Message,
  MkdirResult,
  MutationOptions,
  OnboardingReport,
  PatchInstanceInput,
  PatchPipelineInput,
  PatchProviderInput,
  PatchScheduleInput,
  PersonaApplyInput,
  PersonaApplyResult,
  PersonaDetail,
  PersonaGitDiff,
  PersonaGitDiffOptions,
  PersonaGitLog,
  PersonaGitLogOptions,
  PersonaPreview,
  PersonaPreviewInput,
  PersonaSummary,
  PatchPersonaInput,
  Pipeline,
  PipelineRun,
  PipelineRunDetail,
  Pricing,
  PricingInput,
  ProviderDetail,
  ProviderRotateResult,
  ProviderSummary,
  ProviderTestResult,
  PurgeResult,
  PublishSkillInput,
  RestoreReport,
  RevertInput,
  RevertResult,
  Schedule,
  ScheduleRun,
  ScheduleView,
  SearchMemoryParams,
  SecurityStatus,
  SendMessageInput,
  Share,
  ShareMode,
  SkillDetail,
  SkillGitDiff,
  SkillGitDiffOptions,
  SkillGitLog,
  SkillGitLogOptions,
  SkillPromoteResult,
  SkillSummary,
  Snapshot,
  SnapshotDiff,
  Template,
  TokenInfo,
  UpdateAssistantConfigInput,
  UpdateMemoryInput,
  UploadResult,
  WriteFileInput,
  WriteFileResult,
  WriteMemoryResult,
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

/** Drop empty values so `?instance=&kind=` never reaches the controller. */
function queryString(params: Record<string, string | number | undefined>): string {
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === "") continue;
    query.set(key, String(value));
  }
  const text = query.toString();
  return text ? `?${text}` : "";
}

/** Keep `/auth/login` same-origin, or prefix it when VITE_API_BASE points elsewhere. */
function authUrl(loginUrl: string): string {
  if (isAbsoluteUrl(loginUrl)) return loginUrl;
  return `${BASE}${loginUrl.startsWith("/") ? loginUrl : `/${loginUrl}`}`;
}

/**
 * Writes initiated by the assistant carry a marker so the controller records
 * "via assistant" in the audit log and keeps the usual backup and git trail.
 */
function assistantHeaders(options?: MutationOptions): Record<string, string> {
  return options?.viaAssistant ? { "X-Fleett-Assistant": "1" } : {};
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
  if (init.body !== undefined && !headers.has("Content-Type") && !(init.body instanceof FormData)) {
    // A FormData body carries its own multipart boundary: let fetch set it.
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

/** Fetch a binary attachment with the same credential rules as `request`. */
async function download(path: string): Promise<{ blob: Blob; filename: string }> {
  const token = tokenStore.get();
  const headers = new Headers();
  if (token) headers.set("Authorization", `Bearer ${token}`);
  const response = await fetch(`${BASE}${path}`, { headers, credentials: "same-origin" });
  if (!response.ok) {
    let code = "http_error";
    let message = response.statusText || `HTTP ${response.status}`;
    const contentType = response.headers.get("content-type") ?? "";
    if (contentType.includes("application/json")) {
      const body = (await response.json().catch(() => null)) as {
        error?: { code?: string; message?: string };
      } | null;
      if (body?.error?.code) code = body.error.code;
      if (body?.error?.message) message = body.error.message;
    }
    if (response.status === 401) {
      window.dispatchEvent(new CustomEvent("fleett:unauthorized"));
    }
    throw new ApiError(response.status, code, message);
  }
  const disposition = response.headers.get("content-disposition") ?? "";
  const match = /filename="?([^";]+)"?/.exec(disposition);
  return { blob: await response.blob(), filename: match?.[1] ?? "fleett-export" };
}

/**
 * Read one server-sent event stream. `EventSource` has no way to send the
 * bearer header, so the fetch body is decoded here; the caller owns the
 * `AbortSignal` and the reconnect policy.
 */
async function openLogStream(
  id: string,
  handlers: LogStreamHandlers,
  signal: AbortSignal,
): Promise<void> {
  const token = tokenStore.get();
  const headers = new Headers({ Accept: "text/event-stream" });
  if (token) headers.set("Authorization", `Bearer ${token}`);

  let response: Response;
  try {
    response = await fetch(
      `${BASE}/api/logs/stream?instances=${encodeURIComponent(id)}`,
      { headers, credentials: "same-origin", signal },
    );
  } catch (cause) {
    if (signal.aborted) throw cause;
    throw new ApiError(
      0,
      "network_error",
      "Controller unreachable. Check that the backend is running.",
    );
  }

  if (!response.ok) {
    let code = "http_error";
    let message = response.statusText || `HTTP ${response.status}`;
    const contentType = response.headers.get("content-type") ?? "";
    if (contentType.includes("application/json")) {
      const body = (await response.json().catch(() => null)) as {
        error?: { code?: string; message?: string };
      } | null;
      if (body?.error?.code) code = body.error.code;
      if (body?.error?.message) message = body.error.message;
    }
    if (response.status === 401) {
      window.dispatchEvent(new CustomEvent("fleett:unauthorized"));
    }
    throw new ApiError(response.status, code, message);
  }
  if (!response.body) {
    throw new ApiError(0, "stream_unavailable", "The controller returned no stream body.");
  }

  handlers.onOpen?.();
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  const parser = new SseParser();

  const dispatch = (frames: ReturnType<SseParser["push"]>) => {
    for (const frame of frames) {
      if (frame.event !== "log" || !frame.data) continue;
      try {
        const entry = JSON.parse(frame.data) as LogEntry;
        if (
          entry &&
          typeof entry.id === "number" &&
          typeof entry.instance === "string" &&
          typeof entry.message === "string"
        ) {
          handlers.onEntry(entry);
        }
      } catch {
        // One malformed frame must not kill the stream; the next one is whole.
      }
    }
  };

  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      handlers.onActivity?.();
      dispatch(parser.push(decoder.decode(value, { stream: true })));
    }
    dispatch(parser.flush());
  } finally {
    reader.releaseLock();
  }
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

  listPersonas: () => request<PersonaSummary[]>("/api/personas"),

  createPersona: (input: CreatePersonaInput) =>
    request<PersonaDetail>("/api/personas", { method: "POST", body: JSON.stringify(input) }),

  getPersona: (id) => request<PersonaDetail>(`/api/personas/${encodeURIComponent(id)}`),

  patchPersona: (id, input: PatchPersonaInput) =>
    request<PersonaDetail>(`/api/personas/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  deletePersona: (id) =>
    request<void>(`/api/personas/${encodeURIComponent(id)}`, { method: "DELETE" }),

  previewPersona: (id, input: PersonaPreviewInput = {}) =>
    request<PersonaPreview>(`/api/personas/${encodeURIComponent(id)}/preview`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  applyPersona: (id, input: PersonaApplyInput) =>
    request<PersonaApplyResult>(`/api/personas/${encodeURIComponent(id)}/apply`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  personaGitLog: (id, options: PersonaGitLogOptions = {}) =>
    request<PersonaGitLog>(
      `/api/personas/${encodeURIComponent(id)}/git/log${queryString({ limit: options.limit })}`,
    ),

  personaGitDiff: (id, options: PersonaGitDiffOptions = {}) =>
    request<PersonaGitDiff>(
      `/api/personas/${encodeURIComponent(id)}/git/diff${queryString({
        from: options.from,
        to: options.to,
      })}`,
    ),

  listSkills: () => request<SkillSummary[]>("/api/skills"),

  getSkill: (name) => request<SkillDetail>(`/api/skills/${encodeURIComponent(name)}`),

  publishSkill: (input: PublishSkillInput) =>
    request<SkillDetail>("/api/skills", { method: "POST", body: JSON.stringify(input) }),

  importSkill: (file: File) => {
    const body = new FormData();
    // The controller reads the first file part and ignores the rest.
    body.append("file", file, file.name);
    return request<SkillDetail>("/api/skills/import", { method: "POST", body });
  },

  promoteSkill: (name, version) =>
    request<SkillPromoteResult>(`/api/skills/${encodeURIComponent(name)}/promote`, {
      method: "POST",
      body: JSON.stringify({ version }),
    }),

  deleteSkill: (name) =>
    request<void>(`/api/skills/${encodeURIComponent(name)}`, { method: "DELETE" }),

  skillGitLog: (name, options: SkillGitLogOptions = {}) =>
    request<SkillGitLog>(
      `/api/skills/${encodeURIComponent(name)}/git/log${queryString({ limit: options.limit })}`,
    ),

  skillGitDiff: (name, options: SkillGitDiffOptions = {}) =>
    request<SkillGitDiff>(
      `/api/skills/${encodeURIComponent(name)}/git/diff${queryString({
        from: options.from,
        to: options.to,
      })}`,
    ),

  /* Skills marketplace */

  marketplaceSearch: (params = {}) =>
    request<MarketplaceSearch>(
      `/api/marketplace/skills${queryString({
        q: params.q,
        source: params.source,
        limit: params.limit,
      })}`,
    ),

  marketplacePreview: (input) =>
    request<MarketplacePreview>("/api/marketplace/skills/preview", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  marketplaceInstall: (input) =>
    request<SkillDetail>("/api/marketplace/skills/install", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  marketplaceUpdate: (name, input = {}) =>
    request<MarketplaceUpdateResult>(
      `/api/marketplace/skills/${encodeURIComponent(name)}/update`,
      { method: "POST", body: JSON.stringify(input) },
    ),

  marketplaceUpdates: () => request<MarketplaceUpdates>("/api/marketplace/updates"),

  marketplaceSync: () =>
    request<MarketplaceSyncResult>("/api/marketplace/sync", { method: "POST" }),

  /* MCP servers */

  listMcpCatalog: (q) => request<McpCatalogEntry[]>(`/api/mcp/catalog${queryString({ q })}`),

  importMcpCatalog: (entry) =>
    request<McpCatalogEntry>("/api/mcp/catalog/import", {
      method: "POST",
      body: JSON.stringify(entry),
    }),

  listMcpServers: () => request<McpServerView[]>("/api/mcp/servers"),

  getMcpServer: (id) => request<McpServerView>(`/api/mcp/servers/${encodeURIComponent(id)}`),

  createMcpServer: (input) =>
    request<McpServerView>("/api/mcp/servers", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  patchMcpServer: (id, input) =>
    request<McpServerView>(`/api/mcp/servers/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  deleteMcpServer: (id, purge = false) =>
    request<void>(
      `/api/mcp/servers/${encodeURIComponent(id)}${purge ? "?purge=true" : ""}`,
      { method: "DELETE" },
    ),

  mcpServerAction: (id, action) =>
    request<McpServerView>(`/api/mcp/servers/${encodeURIComponent(id)}/${action}`, {
      method: "POST",
    }),

  testMcpServer: (id) =>
    request<McpTestResult>(`/api/mcp/servers/${encodeURIComponent(id)}/test`, {
      method: "POST",
    }),

  mcpServerLogs: (id, params = {}) =>
    request<string>(
      `/api/mcp/servers/${encodeURIComponent(id)}/logs${queryString({
        tail: params.tail,
        since: params.since,
      })}`,
    ),

  mcpServerTools: (id) =>
    request<McpToolsSnapshot>(`/api/mcp/servers/${encodeURIComponent(id)}/tools`),

  putMcpServerEnv: (id, env) =>
    request<McpServerView>(`/api/mcp/servers/${encodeURIComponent(id)}/env`, {
      method: "PUT",
      body: JSON.stringify({ env }),
    }),

  deleteMcpServerEnv: (id, key) =>
    request<McpServerView>(
      `/api/mcp/servers/${encodeURIComponent(id)}/env/${encodeURIComponent(key)}`,
      { method: "DELETE" },
    ),

  mcpServerIncidents: (id, params = {}) =>
    request<McpIncident[]>(
      `/api/mcp/servers/${encodeURIComponent(id)}/incidents${queryString({
        status: params.status,
        limit: params.limit,
      })}`,
    ),

  closeMcpIncident: (id) =>
    request<McpIncident>(`/api/mcp-incidents/${encodeURIComponent(String(id))}/close`, {
      method: "POST",
    }),

  instanceMcpServers: (id) =>
    request<InstanceMcpView>(`/api/instances/${encodeURIComponent(id)}/mcp-servers`),

  putInstanceMcpServers: (id, servers) =>
    request<InstanceMcpView>(`/api/instances/${encodeURIComponent(id)}/mcp-servers`, {
      method: "PUT",
      body: JSON.stringify({ servers }),
    }),

  attachInstanceMcpServer: (id, server, mode) =>
    request<InstanceMcpView>(
      `/api/instances/${encodeURIComponent(id)}/mcp-servers/${encodeURIComponent(server)}`,
      {
        method: "POST",
        body: JSON.stringify(mode ? { mode } : {}),
      },
    ),

  detachInstanceMcpServer: (id, server) =>
    request<InstanceMcpView>(
      `/api/instances/${encodeURIComponent(id)}/mcp-servers/${encodeURIComponent(server)}`,
      { method: "DELETE" },
    ),

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

  putInstanceEnv: (id: string, env: EnvInput[], options?: MutationOptions) =>
    request<InstanceDetail>(`/api/instances/${encodeURIComponent(id)}/env`, {
      method: "PUT",
      headers: assistantHeaders(options),
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

  patchInstance: (id, input: PatchInstanceInput, options?: MutationOptions) =>
    request<InstanceDetail>(`/api/instances/${encodeURIComponent(id)}`, {
      method: "PATCH",
      headers: assistantHeaders(options),
      body: JSON.stringify(input),
    }),

  action: (id, action) =>
    request<LifecycleResult>(`/api/instances/${encodeURIComponent(id)}/${action}`, { method: "POST" }),

  deleteInstance: (id, purge) =>
    request<void>(`/api/instances/${encodeURIComponent(id)}${purge ? "?purge=true" : ""}`, {
      method: "DELETE",
    }),

  logs: (id, tail) => request<string>(`/api/instances/${encodeURIComponent(id)}/logs?tail=${tail}`),

  instanceLogs: (id, params: InstanceLogQuery = {}) =>
    request<LogSearchResponse>(
      `/api/logs/instances/${encodeURIComponent(id)}${queryString({
        q: params.q,
        level: params.level,
        since: params.since,
        until: params.until,
        after_id: params.after_id,
        limit: params.limit,
      })}`,
    ),

  streamLogs: (id, handlers, signal) => openLogStream(id, handlers, signal),

  listFiles: (id, path) =>
    request<FileEntry[]>(`/api/instances/${encodeURIComponent(id)}/files?path=${encodeURIComponent(path)}`),

  readFile: (id, path) =>
    request<FileContent>(
      `/api/instances/${encodeURIComponent(id)}/files/content?path=${encodeURIComponent(path)}`,
    ),

  writeFile: (id, input: WriteFileInput, options?: MutationOptions) => {
    const query = input.restart ? "?restart=true" : "";
    const headers: Record<string, string> = assistantHeaders(options);
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

  uploadFile: (id, directory, file, options) => {
    const body = new FormData();
    // The controller reads a `path` field (the target folder) and `file` parts;
    // this console sends one file per request so a refusal names one file.
    body.append("path", directory);
    body.append("file", file, file.name);
    const query = options?.overwrite ? "?overwrite=true" : "";
    return request<UploadResult>(
      `/api/instances/${encodeURIComponent(id)}/files/upload${query}`,
      { method: "POST", body },
    );
  },

  downloadFile: (id, path) =>
    download(
      `/api/instances/${encodeURIComponent(id)}/files/download?path=${encodeURIComponent(path)}`,
    ),

  downloadArchive: (id, path) =>
    download(
      `/api/instances/${encodeURIComponent(id)}/files/archive?path=${encodeURIComponent(path)}`,
    ),

  searchFiles: (id, params: FileSearchParams) => {
    const query = new URLSearchParams();
    query.set("q", params.q);
    query.set("path", params.path ?? "/");
    if (params.regex) query.set("regex", "true");
    if (params.limit) query.set("limit", String(params.limit));
    return request<FileSearchResult>(
      `/api/instances/${encodeURIComponent(id)}/files/search?${query.toString()}`,
    );
  },

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

  listMessages: (params: ListMessagesParams = {}) =>
    request<Message[]>(
      `/api/messages${queryString({
        instance: params.instance,
        from: params.from,
        to: params.to,
        kind: params.kind,
        limit: params.limit,
      })}`,
    ),

  instanceMessages: (id, params: InstanceMessagesParams = {}) =>
    request<Message[]>(
      `/api/instances/${encodeURIComponent(id)}/messages${queryString({
        view: params.view,
        status: params.status,
        limit: params.limit,
      })}`,
    ),

  sendMessage: (id, input: SendMessageInput) =>
    request<Message>(`/api/instances/${encodeURIComponent(id)}/messages`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  ackMessage: (id, messageId) =>
    request<AckMessageResult>(
      `/api/instances/${encodeURIComponent(id)}/messages/${encodeURIComponent(messageId)}/ack`,
      { method: "POST" },
    ),

  listMemory: (params: ListMemoryParams = {}) =>
    request<MemoryEntry[]>(
      `/api/memory${queryString({
        namespace: params.namespace,
        tag: params.tag,
        limit: params.limit,
      })}`,
    ),

  searchMemory: (params: SearchMemoryParams) =>
    request<MemoryEntry[]>(
      `/api/memory/search${queryString({
        q: params.q,
        namespace: params.namespace,
        tags: params.tags?.length ? params.tags.join(",") : undefined,
        limit: params.limit,
      })}`,
    ),

  getMemoryByKey: (namespace, key) =>
    request<MemoryEntry>(
      `/api/memory/by-key${queryString({ namespace, key })}`,
    ),

  createOrUpdateMemory: (input: CreateMemoryInput) =>
    request<WriteMemoryResult>("/api/memory", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  updateMemory: (id, input: UpdateMemoryInput) =>
    request<MemoryEntry>(`/api/memory/${id}`, {
      method: "PUT",
      body: JSON.stringify(input),
    }),

  deleteMemory: (id) => request<void>(`/api/memory/${id}`, { method: "DELETE" }),

  memoryHistory: (id) => request<MemoryVersion[]>(`/api/memory/${id}/history`),

  rollbackMemory: (id, version) =>
    request<MemoryEntry>(`/api/memory/${id}/rollback`, {
      method: "POST",
      body: JSON.stringify({ version }),
    }),

  promoteMemory: (id, namespace) =>
    request<MemoryEntry>(`/api/memory/${id}/promote`, {
      method: "POST",
      body: JSON.stringify({ namespace }),
    }),

  purgeExpiredMemory: () => request<PurgeResult>("/api/memory/expired", { method: "DELETE" }),

  /* Schedules */

  listSchedules: () => request<ScheduleView[]>("/api/schedules"),

  listInstanceSchedules: (id) =>
    request<Schedule[]>(`/api/instances/${encodeURIComponent(id)}/schedules`),

  createSchedule: (id, input: CreateScheduleInput) =>
    request<Schedule>(`/api/instances/${encodeURIComponent(id)}/schedules`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  patchSchedule: (scheduleId, input: PatchScheduleInput) =>
    request<Schedule>(`/api/schedules/${encodeURIComponent(scheduleId)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  deleteSchedule: (scheduleId) =>
    request<void>(`/api/schedules/${encodeURIComponent(scheduleId)}`, { method: "DELETE" }),

  runSchedule: (scheduleId) =>
    request<ScheduleRun>(`/api/schedules/${encodeURIComponent(scheduleId)}/run`, {
      method: "POST",
    }),

  listRuns: (id, limit = 50) =>
    request<ScheduleRun[]>(
      `/api/instances/${encodeURIComponent(id)}/runs?limit=${Math.min(Math.max(limit, 1), 500)}`,
    ),

  /* Pipelines */

  listPipelines: () => request<Pipeline[]>("/api/pipelines"),

  getPipeline: (id) => request<Pipeline>(`/api/pipelines/${encodeURIComponent(id)}`),

  createPipeline: (input: CreatePipelineInput) =>
    request<Pipeline>("/api/pipelines", { method: "POST", body: JSON.stringify(input) }),

  patchPipeline: (id, input: PatchPipelineInput) =>
    request<Pipeline>(`/api/pipelines/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  deletePipeline: (id) =>
    request<void>(`/api/pipelines/${encodeURIComponent(id)}`, { method: "DELETE" }),

  runPipeline: (id, context) =>
    request<PipelineRunDetail>(`/api/pipelines/${encodeURIComponent(id)}/run`, {
      method: "POST",
      body: JSON.stringify(context === undefined ? {} : { context }),
    }),

  listPipelineRuns: (id, limit = 50) =>
    request<PipelineRun[]>(
      `/api/pipelines/${encodeURIComponent(id)}/runs?limit=${Math.min(Math.max(limit, 1), 500)}`,
    ),

  getPipelineRun: (id, runId) =>
    request<PipelineRunDetail>(
      `/api/pipelines/${encodeURIComponent(id)}/runs/${encodeURIComponent(runId)}`,
    ),

  cancelPipelineRun: (id, runId) =>
    request<PipelineRunDetail>(
      `/api/pipelines/${encodeURIComponent(id)}/runs/${encodeURIComponent(runId)}/cancel`,
      { method: "POST" },
    ),

  /* Incidents */

  listIncidents: (params: ListIncidentsParams = {}) =>
    request<Incident[]>(
      `/api/incidents${queryString({
        status: params.status,
        instance: params.instance,
        limit: params.limit,
      })}`,
    ),

  instanceIncidents: (id, limit) =>
    request<Incident[]>(
      `/api/instances/${encodeURIComponent(id)}/incidents${queryString({ limit })}`,
    ),

  closeIncident: (id) =>
    request<Incident>(`/api/incidents/${encodeURIComponent(id)}/close`, { method: "POST" }),

  patchHeal: (id, input: HealPolicyInput) =>
    request<HealPolicyView>(`/api/instances/${encodeURIComponent(id)}/heal`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  /* Cost */

  costReport: (params: CostReportParams = {}) =>
    request<CostReport>(
      `/api/cost${queryString({
        from: params.from,
        to: params.to,
        group_by: params.group_by,
      })}`,
    ),

  listBudgets: () => request<BudgetStatus[]>("/api/cost/budgets"),

  getBudget: (id) => request<BudgetView>(`/api/instances/${encodeURIComponent(id)}/budget`),

  setBudget: (id, input: BudgetInput) =>
    request<BudgetView>(`/api/instances/${encodeURIComponent(id)}/budget`, {
      method: "PUT",
      body: JSON.stringify(input),
    }),

  listPricing: () => request<Pricing[]>("/api/models/pricing"),

  setPricing: (input: PricingInput) =>
    request<Pricing>("/api/models/pricing", { method: "POST", body: JSON.stringify(input) }),

  deletePricing: (model) =>
    request<void>(`/api/models/pricing/${encodeURIComponent(model)}`, { method: "DELETE" }),

  /* Log center */

  searchLogs: (params: LogQueryParams = {}) =>
    request<LogSearchResponse>(
      `/api/logs${queryString({
        instances: params.instances?.length ? params.instances.join(",") : undefined,
        q: params.q,
        level: params.level,
        since: params.since,
        until: params.until,
        after_id: params.after_id,
        limit: params.limit,
      })}`,
    ),

  exportLogs: (params: LogQueryParams | undefined, format: LogExportFormat) =>
    download(
      `/api/logs/export${queryString({
        format,
        instances: params?.instances?.length ? params.instances.join(",") : undefined,
        q: params?.q,
        level: params?.level,
        since: params?.since,
        until: params?.until,
        after_id: params?.after_id,
        limit: params?.limit,
      })}`,
    ),

  /* Security */

  securityStatus: () => request<SecurityStatus>("/api/security/status"),

  /* Deployments and maintenance */

  listDeployments: (params: { limit?: number; status?: DeploymentStatus } = {}) =>
    request<Deployment[]>(
      `/api/deployments${queryString({ limit: params.limit, status: params.status })}`,
    ),

  getDeployment: (id) => request<Deployment>(`/api/deployments/${encodeURIComponent(id)}`),

  listMaintenanceWindows: () => request<MaintenanceWindow[]>("/api/maintenance-windows"),

  createMaintenanceWindow: (input: MaintenanceWindowInput) =>
    request<MaintenanceWindow>("/api/maintenance-windows", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  patchMaintenanceWindow: (id, input) =>
    request<MaintenanceWindow>(`/api/maintenance-windows/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  deleteMaintenanceWindow: (id) =>
    request<void>(`/api/maintenance-windows/${encodeURIComponent(id)}`, { method: "DELETE" }),

  maintenanceStatus: () => request<MaintenanceStatus>("/api/maintenance-windows/status"),

  /* Snapshots */

  listSnapshots: (id) => request<Snapshot[]>(`/api/instances/${encodeURIComponent(id)}/snapshots`),

  createSnapshot: (id, input: CreateSnapshotInput = {}) =>
    request<Snapshot>(`/api/instances/${encodeURIComponent(id)}/snapshots`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  compareSnapshots: (id, a, b) =>
    request<SnapshotDiff>(`/api/instances/${encodeURIComponent(id)}/snapshots/compare`, {
      method: "POST",
      body: JSON.stringify({ a, b }),
    }),

  restoreSnapshot: (snapshotId) =>
    request<RestoreReport>(`/api/snapshots/${encodeURIComponent(snapshotId)}/restore`, {
      method: "POST",
    }),

  deleteSnapshot: (snapshotId) =>
    request<void>(`/api/snapshots/${encodeURIComponent(snapshotId)}`, { method: "DELETE" }),

  /* Instance ACL, egress and exec */

  getInstanceAcl: (id) => request<InstanceAcl>(`/api/instances/${encodeURIComponent(id)}/acl`),

  putInstanceAcl: (id, acl: AclItemInput[]) =>
    request<InstanceAcl>(`/api/instances/${encodeURIComponent(id)}/acl`, {
      method: "PUT",
      body: JSON.stringify({ acl }),
    }),

  getEgress: (id) => request<EgressPolicyView>(`/api/instances/${encodeURIComponent(id)}/egress`),

  putEgress: (id, input: EgressPolicyInput) =>
    request<EgressPolicyView>(`/api/instances/${encodeURIComponent(id)}/egress`, {
      method: "PUT",
      body: JSON.stringify(input),
    }),

  deleteEgress: (id) =>
    request<EgressPolicyView>(`/api/instances/${encodeURIComponent(id)}/egress`, {
      method: "DELETE",
    }),

  testEgress: (id, host, port) =>
    request<EgressTestResult>(`/api/instances/${encodeURIComponent(id)}/egress/test`, {
      method: "POST",
      body: JSON.stringify({ host, port }),
    }),

  egressSummary: () => request<EgressSummary>("/api/egress/summary"),

  exec: (id, input: ExecInput) =>
    request<ExecResult>(`/api/instances/${encodeURIComponent(id)}/exec`, {
      method: "POST",
      body: JSON.stringify(input),
    }),

  /* Onboarding and doctor */

  doctor: () => request<DoctorReport>("/api/doctor"),

  onboarding: () => request<OnboardingReport>("/api/onboarding"),

  /* Assistant */

  assistantConfig: () =>
    request<AssistantConfig>("/api/assistant/config", {}, { signalUnauthorized: false }),

  updateAssistantConfig: (input: UpdateAssistantConfigInput) =>
    request<AssistantConfig>("/api/assistant/config", {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  assistantChat: (input: AssistantChatInput) =>
    request<AssistantReply>("/api/assistant/chat", {
      method: "POST",
      body: JSON.stringify(input),
    }),
};
