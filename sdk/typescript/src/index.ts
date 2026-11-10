/**
 * Minimal typed client for the fleett HTTP API (issue #46).
 *
 * No runtime dependency, no build step: import `FleettClient` from this file
 * (TypeScript or a bundler that understands `.ts`), or compile it with
 * `tsc`. The types mirror `openapi.json`; see `docs/WEBHOOKS.md`.
 *
 * ```ts
 * const fleett = new FleettClient({ url: "http://127.0.0.1:8090", token: process.env.FLEETT_TOKEN });
 * const instances = await fleett.instances.list();
 * await fleett.instances.restart("alice");
 * ```
 */

export interface FleettClientOptions {
  /** Base URL of the control plane, with or without a trailing slash. */
  url: string;
  /** Bearer token (`FLEETT_TOKEN` or a named API token). */
  token?: string;
  /** Injectable fetch, for tests and non-standard runtimes. */
  fetch?: typeof fetch;
  /** Per-request timeout in milliseconds (default 30 s). */
  timeoutMs?: number;
}

export interface ErrorBody {
  error: {
    code: string;
    message: string;
    details?: unknown;
  };
}

/** Non-2xx answer from the API. */
export class FleettError extends Error {
  readonly status: number;
  readonly code: string;
  readonly details?: unknown;

  constructor(status: number, code: string, message: string, details?: unknown) {
    super(message);
    this.name = "FleettError";
    this.status = status;
    this.code = code;
    this.details = details;
  }
}

export interface Health {
  ok: boolean;
  version: string;
  driver: "mock" | "compose";
  docker: { ok: boolean };
  uptime_s: number;
  secrets?: { key: boolean; key_error: string | null; encrypted: number; plaintext: number };
  egress?: { mode: "restrict" | "off"; marker: boolean };
}

export interface AuthConfig {
  mode: "bearer" | "oidc" | "both";
  oidc: { enabled: boolean; login_url: string };
  bearer: { enabled: boolean };
}

export interface InstanceSummary {
  id: string;
  name: string;
  template: string;
  model: string;
  image: string;
  channel: string;
  status: string;
  health: string;
  cpu_limit: number;
  allow_lan: boolean;
  provider: string | null;
  shares: string[];
  last_error: string | null;
  created_at: string;
}

export interface EnvEntry {
  key: string;
  secret: boolean;
  value?: string | null;
  updated_at?: string;
}

export interface EnvVarInput {
  key: string;
  value?: string | null;
  secret?: boolean;
}

export interface ShareRef {
  name: string;
  mode?: "rw" | "ro";
}

export interface InstanceDetail {
  id: string;
  name: string;
  template: string;
  model: string;
  image?: string;
  previous_image?: string | null;
  base_url: string;
  api_key_env: string;
  provider: string | null;
  channel: string;
  status: string;
  health: string;
  shares: ShareRef[];
  mem_limit_mb: number;
  cpu_limit: number;
  allow_lan: boolean;
  notes: string;
  warnings: string[];
  created_at: string;
  updated_at: string;
  last_error: string | null;
  data_dir?: string;
  env: EnvEntry[];
  container?: Record<string, unknown> | null;
  compose_yaml?: string;
}

export interface CreateInstanceRequest {
  id: string;
  name?: string;
  template?: string;
  model?: string;
  base_url?: string;
  api_key_env?: string;
  gate_token?: string;
  channel?: { type: "telegram" | "discord" | "none"; token?: string };
  shares?: ShareRef[];
  mem_limit_mb?: number;
  cpu_limit?: number;
  allow_lan?: boolean;
  notes?: string;
  provider?: string;
  env?: EnvVarInput[];
}

export type PatchInstanceRequest = Partial<Omit<CreateInstanceRequest, "id" | "channel">> & {
  channel?: { type: "telegram" | "discord" | "none"; token?: string };
  provider?: string | null;
};

export interface StatusResponse {
  status: string;
}

export interface UsageResponse {
  instances: Array<{
    id: string;
    status: string;
    cpu_pct: number;
    mem_used_bytes: number;
    mem_limit_bytes: number;
    disk_bytes: number;
  }>;
  totals: {
    cpu_pct: number;
    mem_used_bytes: number;
    mem_limit_bytes: number;
    disk_bytes: number;
  };
  caps: { max_cpu: number; max_mem_mb: number };
}

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified: string | null;
}

export interface FileContent {
  path: string;
  content: string;
  truncated: boolean;
}

export interface GitCommit {
  id: string;
  at: string;
  actor: string;
  message: string;
}

export interface GitDiff {
  commit: string;
  diff: string;
}

export interface GitStatus {
  clean: boolean;
  changes: string[];
}

export interface ProviderSummary {
  id: string;
  name: string;
  kind: string;
  base_url: string;
  api_key_env: string;
  instances_count: number;
  created_at: string;
  updated_at: string;
}

export interface ProviderDetail extends ProviderSummary {
  instances: Array<{ id: string; name: string }>;
}

export interface ProviderRestart {
  id: string;
  status: "healthy" | "failed";
  error?: string | null;
}

export interface RotateReport {
  provider: ProviderSummary;
  restarted: ProviderRestart[];
}

export interface ProviderTestResult {
  ok: boolean;
  status: number;
  latency_ms: number;
  error?: string | null;
}

export interface CreateProviderRequest {
  name: string;
  kind: "openai-compatible" | "anthropic" | "google";
  base_url: string;
  api_key_env?: string;
  secret: string;
}

export interface PatchProviderRequest {
  name?: string;
  kind?: string;
  base_url?: string;
  api_key_env?: string;
  secret?: string;
}

/** Every event published by the control plane. */
export const WEBHOOK_EVENTS = [
  "instance.created",
  "instance.started",
  "instance.stopped",
  "instance.deleted",
  "env.changed",
  "upgrade.started",
  "upgrade.ok",
  "upgrade.failed",
  "upgrade.rolled_back",
  "incident.open",
  "incident.closed",
  "secret.rotated",
  "pipeline.run.started",
  "pipeline.run.ok",
  "pipeline.run.failed",
] as const;

export type WebhookEvent = (typeof WEBHOOK_EVENTS)[number];
/** Exact event, `*`, or a prefix wildcard such as `instance.*`. */
export type WebhookFilter = WebhookEvent | "*" | `${string}.*`;

export interface Webhook {
  id: string;
  name: string;
  url: string;
  events: string[];
  enabled: boolean;
  /** Always true: the secret is stored encrypted and never returned. */
  has_secret: boolean;
  created_at: string;
  updated_at: string;
}

export interface CreateWebhookRequest {
  name: string;
  url: string;
  events: WebhookFilter[];
  secret: string;
  enabled?: boolean;
}

export interface PatchWebhookRequest {
  name?: string;
  url?: string;
  events?: WebhookFilter[];
  secret?: string;
  enabled?: boolean;
}

export interface WebhookTestResult {
  ok: boolean;
  status: number | null;
  latency_ms: number;
  error?: string | null;
}

export interface WebhookDelivery {
  id: number;
  webhook_id: string;
  event: string;
  payload: {
    id: number;
    event: string;
    at: string;
    data: unknown;
  };
  status: "pending" | "sent" | "failed";
  attempts: number;
  response_status: number | null;
  error: string | null;
  created_at: string;
  delivered_at: string | null;
}

export interface LogEntry {
  time?: string;
  line?: string;
  message?: string;
}

export type AssistantTask =
  | "freeform"
  | "skill"
  | "soul"
  | "env"
  | "pipeline"
  | "fleet_yaml"
  | "explain"
  | "config";

export interface AssistantMessage {
  role: "user" | "assistant";
  content: string;
}

export interface AssistantContext {
  page: string;
  instance?: string;
  path?: string;
  selection?: string;
  file_content?: string;
  schema?: string;
}

export interface AssistantChatRequest {
  session_id: string;
  context: AssistantContext;
  messages: AssistantMessage[];
  task?: AssistantTask;
  /** Optional output ceiling for this turn, bounded by the server cap. */
  max_tokens?: number;
}

export interface AssistantEnvEntry {
  key: string;
  value?: string;
  secret: boolean;
}

export interface AssistantProposal {
  kind: "file" | "yaml" | "env";
  path?: string;
  content?: string;
  env?: AssistantEnvEntry[];
}

export type AssistantFinishReason = "stop" | "length" | "other";

export interface AssistantUsage {
  prompt: number;
  completion: number;
  total: number;
  /** Reasoning tokens reported by the provider, sharing the same ceiling. */
  reasoning?: number;
  /** Ceiling actually sent upstream; absent when the provider decided. */
  max_tokens?: number;
  finish_reason: AssistantFinishReason;
  truncated: boolean;
}

export interface AssistantRedaction {
  sent: string[];
  omitted: string[];
  redacted: number;
}

export interface AssistantChatResponse {
  message: string;
  proposal?: AssistantProposal;
  /** Generation stopped at the output ceiling; no proposal is then present. */
  truncated: boolean;
  finish_reason: AssistantFinishReason;
  usage: AssistantUsage;
  redaction: AssistantRedaction;
}

export interface AssistantConfig {
  enabled: boolean;
  /** Resolved provider; `null` when nothing is resolved (see `reason`). */
  provider: AssistantProvider | null;
  /** `env` (FLEETT_ASSISTANT_PROVIDER), the stored selection, `auto`, or none. */
  provider_source: "env" | "setting" | "auto" | "none";
  /** `null` when resolved, else `no_provider` or `not_selected`. */
  reason: "no_provider" | "not_selected" | null;
  model: string;
  budget_tokens: number;
  used_tokens: number;
  /** Output ceiling of one reply; `0` lets the provider decide. */
  max_output_tokens: number;
}

export interface AssistantProvider {
  id: string;
  name: string;
}

/** Body of `PATCH /api/assistant/config`: an id, or `null` to clear it. */
export interface AssistantConfigPatch {
  provider: string | null;
}

type Query = Record<string, string | number | boolean | undefined>;

function encode(value: string): string {
  return encodeURIComponent(value);
}

/** Typed facade over the REST API. */
export class FleettClient {
  private readonly base: string;
  private readonly token?: string;
  private readonly fetchImpl: typeof fetch;
  private readonly timeoutMs: number;

  readonly instances: InstancesApi;
  readonly providers: ProvidersApi;
  readonly webhooks: WebhooksApi;
  readonly assistant: AssistantApi;
  readonly skills: SkillsApi;
  readonly marketplace: MarketplaceApi;
  readonly mcp: McpApi;
  readonly egress: EgressApi;
  readonly doctor: DoctorApi;

  constructor(options: FleettClientOptions) {
    this.base = options.url.replace(/\/+$/, "");
    this.token = options.token;
    this.fetchImpl = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.timeoutMs = options.timeoutMs ?? 30_000;
    this.instances = new InstancesApi(this);
    this.providers = new ProvidersApi(this);
    this.webhooks = new WebhooksApi(this);
    this.assistant = new AssistantApi(this);
    this.skills = new SkillsApi(this);
    this.marketplace = new MarketplaceApi(this);
    this.mcp = new McpApi(this);
    this.egress = new EgressApi(this);
    this.doctor = new DoctorApi(this);
  }

  /** Low-level request; every typed method delegates to it. */
  async request<T>(
    method: string,
    path: string,
    options: { body?: unknown; query?: Query; signal?: AbortSignal } = {},
  ): Promise<T> {
    const url = new URL(this.base + path);
    for (const [key, value] of Object.entries(options.query ?? {})) {
      if (value !== undefined) url.searchParams.set(key, String(value));
    }

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    const signal = options.signal ?? controller.signal;
    const headers: Record<string, string> = { Accept: "application/json" };
    if (this.token) headers.Authorization = `Bearer ${this.token}`;
    if (options.body !== undefined) headers["Content-Type"] = "application/json";

    try {
      const response = await this.fetchImpl(url, {
        method,
        headers,
        signal,
        body: options.body === undefined ? undefined : JSON.stringify(options.body),
      });
      if (response.status === 204) return undefined as T;
      const text = await response.text();
      if (!response.ok) {
        let code = "http_error";
        let message = `the server answered HTTP ${response.status}`;
        let details: unknown;
        try {
          const parsed = JSON.parse(text) as ErrorBody;
          code = parsed.error?.code ?? code;
          message = parsed.error?.message ?? message;
          details = parsed.error?.details;
        } catch {
          if (text.trim()) message = text.trim();
        }
        throw new FleettError(response.status, code, message, details);
      }
      return (text.trim() ? JSON.parse(text) : undefined) as T;
    } finally {
      clearTimeout(timer);
    }
  }

  // --- top-level routes ---------------------------------------------------

  health(): Promise<Health> {
    return this.request("GET", "/api/health");
  }

  authConfig(): Promise<AuthConfig> {
    return this.request("GET", "/api/auth/config");
  }

  usage(): Promise<UsageResponse> {
    return this.request("GET", "/api/usage");
  }

  /** `GET /api/openapi.json` (public). */
  openapi(): Promise<Record<string, unknown>> {
    return this.request("GET", "/api/openapi.json");
  }
}

/** Instances, their environment, files and git history. */
export class InstancesApi {
  constructor(private readonly client: FleettClient) {}

  list(): Promise<InstanceSummary[]> {
    return this.client.request("GET", "/api/instances");
  }

  get(id: string): Promise<InstanceDetail> {
    return this.client.request("GET", `/api/instances/${encode(id)}`);
  }

  create(body: CreateInstanceRequest): Promise<InstanceDetail> {
    return this.client.request("POST", "/api/instances", { body });
  }

  patch(id: string, body: PatchInstanceRequest): Promise<InstanceDetail> {
    return this.client.request("PATCH", `/api/instances/${encode(id)}`, { body });
  }

  delete(id: string, purge = false): Promise<void> {
    return this.client.request("DELETE", `/api/instances/${encode(id)}`, {
      query: purge ? { purge: "true" } : {},
    });
  }

  start(id: string): Promise<StatusResponse> {
    return this.client.request("POST", `/api/instances/${encode(id)}/start`);
  }

  stop(id: string): Promise<StatusResponse> {
    return this.client.request("POST", `/api/instances/${encode(id)}/stop`);
  }

  restart(id: string): Promise<StatusResponse> {
    return this.client.request("POST", `/api/instances/${encode(id)}/restart`);
  }

  logs(id: string, options: { tail?: number; since?: string; grep?: string; level?: "warn" | "error" } = {}): Promise<string> {
    return this.client.request("GET", `/api/instances/${encode(id)}/logs`, {
      query: { ...options, format: "text" },
    });
  }

  /** Replace the whole environment; a `secret` entry without a value keeps the stored one. */
  putEnv(id: string, env: EnvVarInput[]): Promise<InstanceDetail> {
    return this.client.request("PUT", `/api/instances/${encode(id)}/env`, {
      body: { env },
    });
  }

  deleteEnv(id: string, key: string): Promise<void> {
    return this.client.request("DELETE", `/api/instances/${encode(id)}/env/${encode(key)}`);
  }

  listFiles(id: string, path = "."): Promise<FileEntry[]> {
    return this.client.request("GET", `/api/instances/${encode(id)}/files`, {
      query: { path },
    });
  }

  readFile(id: string, path: string): Promise<FileContent> {
    return this.client.request("GET", `/api/instances/${encode(id)}/files/content`, {
      query: { path },
    });
  }

  writeFile(id: string, path: string, content: string): Promise<FileEntry> {
    return this.client.request("PUT", `/api/instances/${encode(id)}/files/content`, {
      body: { path, content },
    });
  }

  deleteFile(id: string, path: string): Promise<void> {
    return this.client.request("DELETE", `/api/instances/${encode(id)}/files/content`, {
      query: { path },
    });
  }

  mkdir(id: string, path: string): Promise<void> {
    return this.client.request("POST", `/api/instances/${encode(id)}/files/mkdir`, {
      body: { path },
    });
  }

  gitLog(id: string, limit = 50): Promise<GitCommit[]> {
    return this.client.request("GET", `/api/instances/${encode(id)}/git/log`, {
      query: { limit },
    });
  }

  gitDiff(id: string, commit?: string): Promise<GitDiff> {
    return this.client.request("GET", `/api/instances/${encode(id)}/git/diff`, {
      query: { commit },
    });
  }

  gitStatus(id: string): Promise<GitStatus> {
    return this.client.request("GET", `/api/instances/${encode(id)}/git/status`);
  }

  gitRevert(id: string, commit: string): Promise<GitCommit> {
    return this.client.request("POST", `/api/instances/${encode(id)}/git/revert`, {
      body: { commit },
    });
  }
}

/** Reusable provider credentials; the secret never comes back. */
export class ProvidersApi {
  constructor(private readonly client: FleettClient) {}

  list(): Promise<ProviderSummary[]> {
    return this.client.request("GET", "/api/providers");
  }

  get(id: string): Promise<ProviderDetail> {
    return this.client.request("GET", `/api/providers/${encode(id)}`);
  }

  create(body: CreateProviderRequest): Promise<ProviderSummary> {
    return this.client.request("POST", "/api/providers", { body });
  }

  patch(id: string, body: PatchProviderRequest): Promise<RotateReport> {
    return this.client.request("PATCH", `/api/providers/${encode(id)}`, { body });
  }

  delete(id: string): Promise<void> {
    return this.client.request("DELETE", `/api/providers/${encode(id)}`);
  }

  test(id: string): Promise<ProviderTestResult> {
    return this.client.request("POST", `/api/providers/${encode(id)}/test`);
  }
}

/** Outbound webhooks: CRUD, test delivery and delivery log. */
export class WebhooksApi {
  constructor(private readonly client: FleettClient) {}

  list(): Promise<Webhook[]> {
    return this.client.request("GET", "/api/webhooks");
  }

  get(id: string): Promise<Webhook> {
    return this.client.request("GET", `/api/webhooks/${encode(id)}`);
  }

  create(body: CreateWebhookRequest): Promise<Webhook> {
    return this.client.request("POST", "/api/webhooks", { body });
  }

  patch(id: string, body: PatchWebhookRequest): Promise<Webhook> {
    return this.client.request("PATCH", `/api/webhooks/${encode(id)}`, { body });
  }

  delete(id: string): Promise<void> {
    return this.client.request("DELETE", `/api/webhooks/${encode(id)}`);
  }

  /** One signed `webhook.test` delivery, not retried. */
  test(id: string): Promise<WebhookTestResult> {
    return this.client.request("POST", `/api/webhooks/${encode(id)}/test`);
  }

  deliveries(id: string, limit = 50): Promise<WebhookDelivery[]> {
    return this.client.request("GET", `/api/webhooks/${encode(id)}/deliveries`, {
      query: { limit },
    });
  }
}

/**
 * Contextual assistant (issue #56): configuration and draft requests.
 *
 * The client never holds the provider credential; the control plane decrypts
 * it per request. The context sent from here is redacted server-side, and a
 * proposal is applied through `instances.putEnv` or `instances.writeFile` (the
 * console adds the `X-Fleett-Assistant: 1` marker so the audit row says
 * `via: "assistant"`).
 */
export class AssistantApi {
  constructor(private readonly client: FleettClient) {}

  config(sessionId?: string): Promise<AssistantConfig> {
    return this.client.request("GET", "/api/assistant/config", {
      query: { session_id: sessionId },
    });
  }

  chat(body: AssistantChatRequest): Promise<AssistantChatResponse> {
    return this.client.request("POST", "/api/assistant/chat", { body });
  }

  /**
   * Select the provider the assistant borrows (admin), or clear the selection
   * with `null`. `FLEETT_ASSISTANT_PROVIDER` still wins: read `provider_source`
   * from the answer instead of assuming the body applied.
   */
  selectProvider(provider: string | null): Promise<AssistantConfig> {
    const body: AssistantConfigPatch = { provider };
    return this.client.request("PATCH", "/api/assistant/config", { body });
  }
}

// --- skills, marketplace, MCP, egress and doctor (issues #59, #60, #61, #62) --

export interface SkillFile {
  path: string;
  content: string;
}

export interface Provenance {
  source?: string;
  ref?: string;
  commit?: string;
  upstream_version?: string;
  author?: string;
  tags?: string[];
  installed_at?: string;
}

export interface SkillSummary {
  name: string;
  description: string;
  latest: string | null;
  versions: string[];
  instances_count: number;
  source?: string;
  ref?: string;
  commit?: string;
  created_at: string;
  updated_at: string;
}

export interface SkillVersionInfo extends Provenance {
  version: string;
  description: string;
  files_count: number;
  size_bytes: number;
  published_at: string;
}

export interface SkillDetail {
  name: string;
  description: string;
  latest: string | null;
  versions: SkillVersionInfo[];
  files: SkillFile[];
  instances: string[];
  instances_count: number;
  source?: string;
  ref?: string;
  commit?: string;
  created_at: string;
  updated_at: string;
}

export interface PublishSkillRequest {
  name?: string;
  version?: string;
  description?: string;
  files?: SkillFile[];
}

export interface SkillPromoteRequest {
  version?: string;
}

export interface SkillPromoteResult {
  id: string;
  status: string;
  error?: string | null;
}

export interface SkillPromoteResponse {
  latest: string;
  results: SkillPromoteResult[];
}

export interface GitLog {
  commits: GitCommit[];
}

export interface MarketplaceEntry {
  name: string;
  description: string;
  provider: string;
  source?: string;
  ref?: string;
  url?: string;
  author?: string;
  tags?: string[];
  version?: string;
  commit?: string;
}

export interface MarketplaceSourceStatus {
  source: string;
  status: string;
  count: number;
  message?: string;
}

export interface MarketplaceSearchResponse {
  entries: MarketplaceEntry[];
  sources: MarketplaceSourceStatus[];
}

export interface MarketplaceFile {
  path: string;
  size: number;
  script: boolean;
}

export interface MarketplacePreview {
  name: string;
  description: string;
  version?: string;
  author?: string;
  files: MarketplaceFile[];
  total_bytes: number;
  skill_md: string;
  scripts: string[];
  commit?: string;
  source?: string;
  requires_confirmation: boolean;
}

export interface MarketplacePreviewRequest {
  source: string;
  ref?: string;
}

export interface MarketplaceInstallRequest extends MarketplacePreviewRequest {
  name?: string;
  confirm_scripts?: boolean;
}

export interface MarketplaceUpdateInfo {
  name: string;
  installed_version: string;
  upstream_version?: string;
  commit?: string;
  source: string;
  ref?: string;
}

export interface MarketplaceUpdatesResponse {
  updates: MarketplaceUpdateInfo[];
  warnings: string[];
}

export interface MarketplaceFileChange {
  path: string;
  status: string;
}

export interface MarketplaceUpdateResult {
  name: string;
  from: string;
  to: string;
  commit?: string;
  files: MarketplaceFileChange[];
  promoted?: SkillPromoteResponse;
}

export interface MarketplaceUpdateRequest {
  promote?: boolean;
  confirm_scripts?: boolean;
}

export interface MarketplaceSyncResult {
  skills: number;
  source: string;
  commit?: string;
}

export type McpKind = "stdio" | "streamable-http" | "sse";
export type McpMode = "ro" | "rw";

export interface McpCatalogEnv {
  key: string;
  secret?: boolean;
  required?: boolean;
}

export interface McpCatalogEntry {
  id: string;
  name: string;
  description?: string;
  kind: McpKind;
  image?: string;
  command?: string[];
  port?: number;
  path?: string;
  env?: McpCatalogEnv[];
  tags?: string[];
}

export interface McpServerEnv {
  key: string;
  secret: boolean;
  set: boolean;
}

export interface McpServer {
  id: string;
  name: string;
  description: string;
  kind: McpKind;
  image: string;
  command: string[];
  port: number;
  path: string;
  persist: boolean;
  cpu_limit: number;
  mem_limit_mb: number;
  host: string;
  status: string;
  health: string;
  last_error: string | null;
  last_probe_at: string | null;
  endpoint: string | null;
  tools: unknown[];
  tools_count: number;
  tools_hash: string;
  env: McpServerEnv[];
  headers: string[];
  instances: string[];
  created_at: string;
  updated_at: string;
}

export interface McpTool {
  name: string;
  description: string;
  input_schema: unknown;
}

export interface McpTestResult {
  server: string;
  reachable: boolean;
  latency_ms: number;
  tools: McpTool[];
  tools_count: number;
  tools_hash: string;
  protocol_version: string;
  server_info: unknown;
}

export interface McpIncident {
  id: number;
  server_id: string;
  cause: string;
  status: string;
  opened_at: string;
  closed_at: string | null;
  detail: string | null;
  actions: unknown[];
  restart_count: number;
  last_seen_at: string;
}

export interface McpEnvInput {
  key: string;
  value?: string;
  secret?: boolean;
}

export interface CreateMcpServerRequest {
  catalog?: string;
  id?: string;
  name?: string;
  description?: string;
  kind?: McpKind;
  image?: string;
  command?: string[];
  port?: number;
  path?: string;
  persist?: boolean;
  cpu_limit?: number;
  mem_limit_mb?: number;
  host?: string;
  env?: McpEnvInput[];
  headers?: Record<string, string>;
}

export interface UpdateMcpServerRequest {
  name?: string;
  description?: string;
  image?: string;
  command?: string[];
  port?: number;
  path?: string;
  persist?: boolean;
  cpu_limit?: number;
  mem_limit_mb?: number;
  host?: string;
  headers?: Record<string, string>;
}

export interface McpRefInput {
  server: string;
  mode?: McpMode;
}

export interface AttachedMcpServer {
  server: string;
  mode: string;
  kind: string;
  status: string;
  health: string;
  tools_count: number;
  endpoint: string | null;
}

export interface InstanceMcpResponse {
  id: string;
  status: string;
  servers: AttachedMcpServer[];
}

export interface EgressPolicy {
  instance: string;
  source: string;
  mode: string;
  fqdns: string[];
  cidrs: string[];
  ports: number[];
  updated_at?: string;
  proxy?: string;
  proxy_acl?: string;
  legacy_equivalent: boolean;
  proxy_reachable?: boolean;
  proxy_error?: string;
  warnings: string[];
}

export interface EgressSummaryEntry {
  instance: string;
  policy: string;
  mode: string;
  fqdns: string[];
  cidrs: string[];
  ports: number[];
  proxy: string | null;
  proxy_reachable: boolean | null;
  proxy_error?: string;
  source: string;
  warnings: string[];
}

export interface EgressSummary {
  instances: EgressSummaryEntry[];
}

export interface EgressBlock {
  id: number;
  instance_id: string;
  host: string;
  port: number;
  reason?: string;
  at: string;
}

export interface EgressTestRequest {
  host: string;
  port?: number;
}

export interface EgressTestResult {
  instance: string;
  host: string;
  port: number;
  allowed: boolean;
  reason: string;
  source: string;
  mode: string;
  enforced: boolean;
  enforcement?: string;
  proxy?: string;
}

export interface DoctorCheck {
  id: string;
  label: string;
  status: "ok" | "warn" | "fail";
  detail: string;
  /** Exact command (or setting) that repairs the check. */
  fix?: string;
  /** Internal console route that repairs the check (`/providers?assistant=1`). */
  link?: string;
}

export interface DoctorReport {
  ok: boolean;
  checks: DoctorCheck[];
}

export interface OnboardingAction {
  kind: "link" | "command";
  to?: string;
  command?: string;
}

export interface OnboardingStep {
  id: string;
  title: string;
  description: string;
  done: boolean;
  action?: OnboardingAction;
}

export interface Onboarding {
  complete: boolean;
  steps: OnboardingStep[];
}

/** Shared skill registry (issue #54): versions, files, promotion and git. */
export class SkillsApi {
  constructor(private readonly client: FleettClient) {}

  list(): Promise<SkillSummary[]> {
    return this.client.request("GET", "/api/skills");
  }

  get(name: string): Promise<SkillDetail> {
    return this.client.request("GET", `/api/skills/${encode(name)}`);
  }

  publish(body: PublishSkillRequest): Promise<SkillDetail> {
    return this.client.request("POST", "/api/skills", { body });
  }

  delete(name: string): Promise<void> {
    return this.client.request("DELETE", `/api/skills/${encode(name)}`);
  }

  /** Move `latest` and restart the instances that follow it. */
  promote(name: string, version?: string): Promise<SkillPromoteResponse> {
    return this.client.request("POST", `/api/skills/${encode(name)}/promote`, {
      body: { version },
    });
  }

  log(name: string, limit = 50): Promise<GitLog> {
    return this.client.request("GET", `/api/skills/${encode(name)}/git/log`, {
      query: { limit },
    });
  }

  diff(name: string, from?: string, to?: string): Promise<GitDiff> {
    return this.client.request("GET", `/api/skills/${encode(name)}/git/diff`, {
      query: { from, to },
    });
  }
}

/** Marketplace search, preview, install and update (issue #61). */
export class MarketplaceApi {
  constructor(private readonly client: FleettClient) {}

  search(options: { q?: string; source?: string; limit?: number } = {}): Promise<MarketplaceSearchResponse> {
    return this.client.request("GET", "/api/marketplace/skills", {
      query: { ...options },
    });
  }

  preview(body: MarketplacePreviewRequest): Promise<MarketplacePreview> {
    return this.client.request("POST", "/api/marketplace/skills/preview", { body });
  }

  install(body: MarketplaceInstallRequest): Promise<SkillDetail> {
    return this.client.request("POST", "/api/marketplace/skills/install", { body });
  }

  update(name: string, body: MarketplaceUpdateRequest = {}): Promise<MarketplaceUpdateResult> {
    return this.client.request("POST", `/api/marketplace/skills/${encode(name)}/update`, { body });
  }

  updates(): Promise<MarketplaceUpdatesResponse> {
    return this.client.request("GET", "/api/marketplace/updates");
  }

  sync(): Promise<MarketplaceSyncResult> {
    return this.client.request("POST", "/api/marketplace/sync");
  }
}

/** MCP server catalog, lifecycle and instance attachment (issue #62). */
export class McpApi {
  constructor(private readonly client: FleettClient) {}

  catalog(q?: string): Promise<McpCatalogEntry[]> {
    return this.client.request("GET", "/api/mcp/catalog", { query: { q } });
  }

  importCatalog(entry: McpCatalogEntry): Promise<McpCatalogEntry> {
    return this.client.request("POST", "/api/mcp/catalog/import", { body: entry });
  }

  deleteCatalog(id: string): Promise<void> {
    return this.client.request("DELETE", `/api/mcp/catalog/${encode(id)}`);
  }

  list(): Promise<McpServer[]> {
    return this.client.request("GET", "/api/mcp/servers");
  }

  get(id: string): Promise<McpServer> {
    return this.client.request("GET", `/api/mcp/servers/${encode(id)}`);
  }

  create(body: CreateMcpServerRequest): Promise<McpServer> {
    return this.client.request("POST", "/api/mcp/servers", { body });
  }

  patch(id: string, body: UpdateMcpServerRequest): Promise<McpServer> {
    return this.client.request("PATCH", `/api/mcp/servers/${encode(id)}`, { body });
  }

  delete(id: string, purge = false): Promise<void> {
    return this.client.request("DELETE", `/api/mcp/servers/${encode(id)}`, {
      query: purge ? { purge: true } : {},
    });
  }

  start(id: string): Promise<McpServer> {
    return this.client.request("POST", `/api/mcp/servers/${encode(id)}/start`);
  }

  stop(id: string): Promise<McpServer> {
    return this.client.request("POST", `/api/mcp/servers/${encode(id)}/stop`);
  }

  restart(id: string): Promise<McpServer> {
    return this.client.request("POST", `/api/mcp/servers/${encode(id)}/restart`);
  }

  test(id: string): Promise<McpTestResult> {
    return this.client.request("POST", `/api/mcp/servers/${encode(id)}/test`);
  }

  logs(id: string, options: { tail?: number; since?: string } = {}): Promise<string> {
    return this.client.request("GET", `/api/mcp/servers/${encode(id)}/logs`, {
      query: { ...options },
    });
  }

  tools(id: string): Promise<unknown> {
    return this.client.request("GET", `/api/mcp/servers/${encode(id)}/tools`);
  }

  putEnv(id: string, env: McpEnvInput[]): Promise<McpServer> {
    return this.client.request("PUT", `/api/mcp/servers/${encode(id)}/env`, { body: { env } });
  }

  deleteEnv(id: string, key: string): Promise<McpServer> {
    return this.client.request("DELETE", `/api/mcp/servers/${encode(id)}/env/${encode(key)}`);
  }

  incidents(id: string, options: { status?: "open" | "closed"; limit?: number } = {}): Promise<McpIncident[]> {
    return this.client.request("GET", `/api/mcp/servers/${encode(id)}/incidents`, {
      query: { ...options },
    });
  }

  closeIncident(id: number): Promise<McpIncident> {
    return this.client.request("POST", `/api/mcp-incidents/${encode(String(id))}/close`);
  }

  instanceList(id: string): Promise<InstanceMcpResponse> {
    return this.client.request("GET", `/api/instances/${encode(id)}/mcp-servers`);
  }

  instancePut(id: string, servers: McpRefInput[]): Promise<InstanceMcpResponse> {
    return this.client.request("PUT", `/api/instances/${encode(id)}/mcp-servers`, {
      body: { servers },
    });
  }

  attach(id: string, server: string, mode: McpMode = "ro"): Promise<InstanceMcpResponse> {
    return this.client.request(
      "POST",
      `/api/instances/${encode(id)}/mcp-servers/${encode(server)}`,
      { body: { mode } },
    );
  }

  detach(id: string, server: string): Promise<InstanceMcpResponse> {
    return this.client.request(
      "DELETE",
      `/api/instances/${encode(id)}/mcp-servers/${encode(server)}`,
    );
  }
}

/** Egress destination policy and proxy status (issue #60). */
export class EgressApi {
  constructor(private readonly client: FleettClient) {}

  getPolicy(instance: string): Promise<EgressPolicy> {
    return this.client.request("GET", `/api/instances/${encode(instance)}/egress`);
  }

  putPolicy(instance: string, body: Record<string, unknown>): Promise<EgressPolicy> {
    return this.client.request("PUT", `/api/instances/${encode(instance)}/egress`, { body });
  }

  deletePolicy(instance: string): Promise<EgressPolicy> {
    return this.client.request("DELETE", `/api/instances/${encode(instance)}/egress`);
  }

  test(instance: string, body: EgressTestRequest): Promise<EgressTestResult> {
    return this.client.request("POST", `/api/instances/${encode(instance)}/egress/test`, { body });
  }

  summary(): Promise<EgressSummary> {
    return this.client.request("GET", "/api/egress/summary");
  }

  blocks(instance: string, limit?: number): Promise<EgressBlock[]> {
    return this.client.request("GET", `/api/instances/${encode(instance)}/egress/blocks`, {
      query: { limit },
    });
  }

  clearBlocks(instance: string): Promise<{ removed: number }> {
    return this.client.request("DELETE", `/api/instances/${encode(instance)}/egress/blocks`);
  }

  allBlocks(options: { limit?: number; instance?: string } = {}): Promise<EgressBlock[]> {
    return this.client.request("GET", "/api/egress/blocks", { query: { ...options } });
  }

  ingestBlock(body: { instance: string; host: string; port?: number; reason?: string }): Promise<EgressBlock> {
    return this.client.request("POST", "/api/egress/blocks", { body });
  }
}

/** Installation checks and first-run onboarding. */
export class DoctorApi {
  constructor(private readonly client: FleettClient) {}

  doctor(): Promise<DoctorReport> {
    return this.client.request("GET", "/api/doctor");
  }

  onboarding(): Promise<Onboarding> {
    return this.client.request("GET", "/api/onboarding");
  }
}
