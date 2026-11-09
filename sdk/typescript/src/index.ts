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

export interface AssistantUsage {
  prompt: number;
  completion: number;
  total: number;
}

export interface AssistantRedaction {
  sent: string[];
  omitted: string[];
  redacted: number;
}

export interface AssistantChatResponse {
  message: string;
  proposal?: AssistantProposal;
  usage: AssistantUsage;
  redaction: AssistantRedaction;
}

export interface AssistantConfig {
  enabled: boolean;
  provider?: ProviderSummary | null;
  model: string;
  budget_tokens: number;
  used_tokens: number;
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

  constructor(options: FleettClientOptions) {
    this.base = options.url.replace(/\/+$/, "");
    this.token = options.token;
    this.fetchImpl = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.timeoutMs = options.timeoutMs ?? 30_000;
    this.instances = new InstancesApi(this);
    this.providers = new ProvidersApi(this);
    this.webhooks = new WebhooksApi(this);
    this.assistant = new AssistantApi(this);
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
}
