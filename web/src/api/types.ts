/** Wire types. Field names and values mirror API.md exactly. */

export type InstanceStatus = "created" | "running" | "stopped" | "error" | "unknown";
export type ChannelType = "telegram" | "discord" | "none";
export type ShareMode = "rw" | "ro";
export type Driver = "mock" | "compose";

export interface Health {
  ok: boolean;
  version: string;
  driver: Driver;
  docker: { ok: boolean };
}

/** Credential types offered by the controller (`FLEETT_AUTH`). */
export type AuthMode = "bearer" | "oidc" | "both";
export type Role = "admin" | "viewer";
export type AuthMethod = "session" | "bearer";

export interface AuthConfig {
  mode: AuthMode;
  oidc: { enabled: boolean; login_url: string };
  bearer: { enabled: boolean };
}

export interface SessionUser {
  sub: string;
  email: string;
  name: string;
}

/** `GET /api/auth/me`: the identity behind the current request. */
export interface Identity {
  authenticated: boolean;
  actor: string;
  role: Role;
  method: AuthMethod;
  /** Present for cookie sessions only. */
  user?: SessionUser;
}

export interface TokenInfo {
  id: string;
  name: string;
  role: Role;
  created_at: string;
  /** Omitted while the token is active. */
  revoked_at?: string | null;
}

/** `POST /api/tokens`: the secret is returned exactly once. */
export interface CreatedToken {
  id: string;
  name: string;
  role: Role;
  created_at: string;
  token: string;
}

export interface CreateTokenInput {
  name: string;
  role: Role;
}

export interface AuditEntry {
  id: number;
  at: string;
  actor: string;
  role: Role;
  method: AuthMethod;
  action: string;
  target: string;
  ip?: string | null;
  details?: unknown;
}

/** One variable a template suggests, mostly an allowlist. Never a credential. */
export interface EnvSuggestion {
  key: string;
  description: string;
  /** `true` when the value has to be stored encrypted. The controller defaults to false. */
  secret?: boolean;
  /** Value the console pre-fills when the variable is added. */
  example?: string;
  /** A missing value makes the gateway refuse every message. */
  required?: boolean;
  /** `csv-of-ids`, `bool`, `url` or `text`; enforced by the write endpoints. */
  format?: string;
}

export interface Template {
  id: string;
  name: string;
  description: string;
  default_model: string;
  channel: string;
  env: EnvSuggestion[];
}

/* --------------------------------------------------------------------------
 * Providers (docs/PROVIDERS.md)
 * -------------------------------------------------------------------------- */

/** Metadata of one reusable API credential. The secret is never returned. */
export interface ProviderSummary {
  id: string;
  /** Lowercase slug, unique across the fleet. */
  name: string;
  /** `openai-compatible`, `anthropic` or `google`; metadata for the console. */
  kind: string;
  base_url: string;
  /** Container variable the secret is injected under. */
  api_key_env: string;
  /** Instances whose `provider` is this id. */
  instances_count: number;
  created_at: string;
  updated_at: string;
}

/** One instance attached to a provider. */
export interface ProviderInstanceRef {
  id: string;
  name: string;
}

/** `GET /api/providers/{id}`: the summary plus the instances that use it. */
export interface ProviderDetail extends ProviderSummary {
  instances: ProviderInstanceRef[];
}

/** Outcome of one instance in a rotation report. */
export interface ProviderRestart {
  id: string;
  /** `healthy` when the instance came back, `failed` otherwise. */
  status: string;
  error?: string;
}

/** `PATCH /api/providers/{id}`: a rotated secret restarts every attached instance. */
export interface ProviderRotateResult {
  provider: ProviderSummary;
  restarted: ProviderRestart[];
}

/** `POST /api/providers/{id}/test`: never contains the key. */
export interface ProviderTestResult {
  ok: boolean;
  /** HTTP status of `${base_url}/models`, `0` when no answer arrived. */
  status: number;
  latency_ms: number;
  error?: string;
}

/** `POST /api/providers`. */
export interface CreateProviderInput {
  name: string;
  kind: string;
  base_url: string;
  api_key_env: string;
  /** Write-only. Encrypted at rest. */
  secret: string;
}

/** `PATCH /api/providers/{id}`. A missing field is left untouched. */
export interface PatchProviderInput {
  name?: string;
  kind?: string;
  base_url?: string;
  api_key_env?: string;
  /** Write-only. Rotates the credential and restarts attached instances. */
  secret?: string;
}

/** `409 provider_in_use`: the instances that still reference the provider. */
export interface ProviderInUseDetails {
  provider?: string;
  instances?: ProviderInstanceRef[];
}

/** One variable of an instance, as returned by the API. */
export interface EnvEntry {
  key: string;
  /** Encrypted at rest: the value never leaves the controller. */
  secret: boolean;
  updated_at: string;
  /** Only present when `secret` is false. */
  value?: string;
}

/**
 * One variable in a write request. On a full replace, omitting `value` (or
 * sending `null`) keeps the stored value of a secret entry, which is the only
 * way a console can round-trip a secret it never received.
 */
export interface EnvInput {
  key: string;
  value?: string | null;
  /** Required on create and full replace; optional on PATCH, which keeps the flag. */
  secret?: boolean;
}

export interface InstanceSummary {
  id: string;
  name: string;
  template: string;
  model: string;
  channel: string;
  status: InstanceStatus;
  shares: string[];
  /** true = also attached to the `fleett-lan` network, so the LAN is reachable. */
  allow_lan: boolean;
  /** Provider the instance draws its API key from; absent for a legacy instance. */
  provider?: string;
  created_at: string;
}

export interface ShareRef {
  name: string;
  mode: ShareMode;
}

export interface ContainerInfo {
  id: string;
  image: string;
  uptime_s: number;
}

export interface InstanceDetail {
  id: string;
  name: string;
  template: string;
  model: string;
  base_url: string;
  api_key_env: string;
  /** Provider the instance draws its API key from; absent for a legacy instance. */
  provider?: string;
  /** API.md returns the channel type as a string on read. */
  channel: string;
  status: InstanceStatus;
  shares: ShareRef[];
  mem_limit_mb: number;
  /** true = also attached to the `fleett-lan` network, so the LAN is reachable. */
  allow_lan: boolean;
  notes: string;
  created_at: string;
  updated_at: string;
  last_error: string | null;
  data_dir: string;
  /** Sorted by key. Values are only present on non-secret entries. */
  env: EnvEntry[];
  container: ContainerInfo | null;
  compose_yaml: string;
  /**
   * Operator-facing warnings: egress enforcement, LAN access, plaintext
   * secrets and template allowlists. Empty when there is nothing to report.
   */
  warnings?: string[];
}

export interface ShareInstance {
  id: string;
  mode: ShareMode;
}

export interface Share {
  name: string;
  path: string;
  instances: ShareInstance[];
}

export interface ChannelInput {
  type: ChannelType;
  /** Omit on PATCH to keep the stored token. */
  token?: string;
}

export interface CreateInstanceInput {
  id: string;
  name: string;
  template: string;
  model: string;
  base_url: string;
  api_key_env: string;
  /** Reusable provider credential. `base_url`/`api_key_env` default to its values. */
  provider?: string;
  /** Write-only. Encrypted at rest as HERMES_GATE_PROFILE_TOKEN. */
  gate_token?: string;
  channel: ChannelInput;
  shares: ShareRef[];
  mem_limit_mb: number;
  /** false keeps the agent on the restricted `fleett` network only. */
  allow_lan: boolean;
  notes: string;
  /** Extra variables. Gate and platform tokens always win over an entry with the same key. */
  env?: EnvInput[];
}

/**
 * `POST /api/instances/{id}/clone`. The clone is independent: its own compose
 * project, container, data directory and row. Secrets are never inherited, so
 * every token the source holds must be replaced here.
 */
export interface CloneInstanceInput {
  new_id: string;
  /** Defaults to the source name plus " copy". */
  name?: string;
  /** Defaults to the source model. */
  model?: string;
  /**
   * Provider of the clone. Omitted inherits the source provider; an empty
   * string detaches it (then a gate token is required).
   */
  provider?: string;
  /** Replacement for `HERMES_GATE_PROFILE_TOKEN` (and `api_key_env`). */
  gate_token?: string;
  /** Replacement for the platform bot token of the source channel. */
  channel_token?: string;
  /** Copy the source shares that still exist. Defaults to true. */
  shares_inherit?: boolean;
  /** Copy the source data directory. Defaults to true. */
  copy_data?: boolean;
}

/** `400 clone_token_required`: the env keys the clone needs a fresh value for. */
export interface CloneTokenDetails {
  source: string;
  missing: string[];
}

export interface PatchInstanceInput {
  name?: string;
  model?: string;
  base_url?: string;
  api_key_env?: string;
  /** `null` detaches the provider; a missing field leaves it in place. */
  provider?: string | null;
  /** Write-only. Rotates the provider key written as HERMES_GATE_PROFILE_TOKEN. */
  gate_token?: string;
  channel?: ChannelInput;
  shares?: ShareRef[];
  mem_limit_mb?: number;
  /** Turning LAN access off fails with 409 `egress_not_enforced` until the host firewall is installed. */
  allow_lan?: boolean;
  notes?: string;
  /** Merge semantics: `value:null` deletes, `""` keeps an empty value, a missing `secret` keeps the flag. */
  env?: EnvInput[];
}

export interface LifecycleResult {
  status: InstanceStatus;
}

/* --------------------------------------------------------------------------
 * Instance files (docs/FILES.md)
 * -------------------------------------------------------------------------- */

export type FileKind = "file" | "dir";

/** One directory level of the agent home, paths rooted at the data directory. */
export interface FileEntry {
  name: string;
  path: string;
  type: FileKind;
  /** Bytes on disk; a directory reports its own size and is never editable. */
  size: number;
  /** RFC 3339, second precision. */
  mtime: string;
  editable: boolean;
  /** fleett rewrites this file on the next PATCH; a write is refused. */
  rendered: boolean;
}

export interface FileContent {
  path: string;
  content: string;
  /** Lowercase SHA-256 of the bytes on disk, 64 characters. */
  etag: string;
  size: number;
  mtime: string;
  editable: boolean;
  rendered: boolean;
  /** Present for rendered files: why a manual write would be lost. */
  note?: string;
}

export interface WriteFileInput {
  path: string;
  content: string;
  /**
   * Version the editor loaded. Omit (or send an empty string) to create a new
   * file; an existing file without an etag is a conflict.
   */
  etag?: string | null;
  /** Write, then restart the instance. Defaults to false. */
  restart?: boolean;
  /**
   * Admin override, sent as `If-Match: *`: overwrite whatever is on disk even
   * though another version was loaded. The previous bytes go to a `.bak`.
   */
  force?: boolean;
}

export interface WriteFileResult {
  path: string;
  etag: string;
  size: number;
  mtime: string;
  editable: boolean;
  rendered: boolean;
  /** Virtual path of the timestamped `.bak` copy, when one was written. */
  backup?: string;
  restarted: boolean;
  status?: string;
}

export interface MkdirResult {
  path: string;
  type: "dir";
}

/* --------------------------------------------------------------------------
 * Instance history (docs/GIT.md)
 * -------------------------------------------------------------------------- */

export interface CommitInfo {
  id: string;
  /** First seven characters of `id`. */
  short: string;
  author: string;
  email: string;
  /** RFC 3339 UTC, second precision. */
  at: string;
  message: string;
  /** Paths changed against the first parent, repository relative. */
  paths: string[];
}

export interface GitLog {
  commits: CommitInfo[];
}

export interface GitDiff {
  /** Empty when the range starts at the empty tree (root commit). */
  from: string;
  /** Empty when the instance has no history. */
  to: string;
  diff: string;
}

/** Work tree against `HEAD`, whitelist only. */
export interface GitStatus {
  modified: string[];
  untracked: string[];
}

export interface GitLogOptions {
  /** Repository (`data/SOUL.md`) or virtual (`/SOUL.md`) path. */
  path?: string;
  /** 1..500, defaults to 50. */
  limit?: number;
}

export interface GitDiffOptions {
  /** Defaults to the first parent of `to`. */
  from?: string;
  /** Defaults to `HEAD`. */
  to?: string;
}

export interface RevertInput {
  /** Full id, short id or any revision `HEAD~2` resolves. */
  commit: string;
  /** Paths to restore; omitted restores every path the commit changed. */
  paths?: string[];
  restart?: boolean;
}

export interface RevertResult {
  source: string;
  /** Repository relative paths actually restored or removed. */
  paths: string[];
  /** Commit recording the restore; `null` when nothing changed. */
  commit: CommitInfo | null;
  restarted: boolean;
  status: string;
}

/* --------------------------------------------------------------------------
 * Agent-to-agent messages (docs/BUS.md)
 * -------------------------------------------------------------------------- */

/** `ask` expects a `result`; `tell` is one-way. */
export type MessageKind = "ask" | "tell" | "result";
/** `delivered` is stamped when the recipient fetches its inbox; `read` when it acks. */
export type MessageStatus = "queued" | "delivered" | "read";
/** Which side of an instance mailbox to read. */
export type MessageBox = "inbox" | "outbox" | "all";

/** One row of the persistent bus. */
export interface Message {
  id: string;
  from: string;
  to: string;
  kind: MessageKind;
  body: string;
  /** Ties a question to its answer and powers loop detection. */
  correlation_id?: string | null;
  hops: number;
  status: MessageStatus;
  created_at: string;
  expires_at: string;
  delivered_at?: string | null;
  read_at?: string | null;
}

export interface ListMessagesParams {
  /** Either side of the conversation. */
  instance?: string;
  from?: string;
  to?: string;
  kind?: MessageKind;
  limit?: number;
}

export interface InstanceMessagesParams {
  view?: MessageBox;
  status?: MessageStatus;
  limit?: number;
}

export interface SendMessageInput {
  to: string;
  /** Admin only: send on behalf of another instance. Defaults to the path id. */
  from?: string;
  /** Defaults to `tell`. */
  kind?: MessageKind;
  body: string;
  correlation_id?: string;
  /** 30 s … 30 days. Defaults to 24 h. */
  ttl_s?: number;
}

/** `POST /api/instances/{id}/messages/{message_id}/ack`. */
export interface AckMessageResult {
  message: Message;
}

/* --------------------------------------------------------------------------
 * Shared fleet memory (docs/MEMORY.md)
 * -------------------------------------------------------------------------- */

/** `global`, `shared`, `instance:<id>` or `project:<name>`. */
export type MemoryNamespace = string;
export type MemoryAction = "create" | "update" | "delete" | "rollback";

export interface MemoryEntry {
  id: number;
  namespace: MemoryNamespace;
  key: string;
  /** Plaintext by design: knowledge, never a credential. */
  value: string;
  tags: string[];
  /** Instance id, actor or admin that wrote the current version. */
  owner: string;
  version: number;
  created_at: string;
  updated_at: string;
  /** Absent when the entry never expires. */
  expires_at?: string | null;
}

/** One append-only row of an entry history. */
export interface MemoryVersion {
  version: number;
  value: string;
  tags: string[];
  owner: string;
  action: MemoryAction | string;
  at: string;
}

/** `POST /api/memory`: created is false when `(namespace, key)` already existed. */
export interface WriteMemoryResult {
  entry: MemoryEntry;
  created: boolean;
}

export interface ListMemoryParams {
  namespace?: string;
  tag?: string;
  limit?: number;
}

export interface SearchMemoryParams {
  q: string;
  namespace?: string;
  /** Every tag is required (AND). */
  tags?: string[];
  limit?: number;
}

export interface CreateMemoryInput {
  namespace: string;
  key: string;
  value: string;
  tags?: string[];
  /** 60 s … 1 year; omitted means no expiry. */
  ttl_s?: number;
}

export interface UpdateMemoryInput {
  value: string;
  tags?: string[];
  /** Omitted keeps the stored expiry. */
  ttl_s?: number;
}

/** `DELETE /api/memory/expired` and `/api/messages/expired`. */
export interface PurgeResult {
  removed: number;
}

/* --------------------------------------------------------------------------
 * Cron schedules (docs/SCHEDULES.md)
 * -------------------------------------------------------------------------- */

export type ScheduleOverlap = "skip" | "queue";
export type ScheduleRunStatus = "running" | "ok" | "error" | "timeout" | "skipped";

export interface Schedule {
  id: string;
  instance_id: string;
  name: string;
  cron: string;
  prompt: string;
  enabled: boolean;
  timezone: string;
  overlap: ScheduleOverlap;
  /** Execution budget in seconds. */
  timeout: number;
  catchup: boolean;
  last_run_at?: string;
  next_run_at?: string;
  created_at: string;
  updated_at: string;
}

export interface ScheduleRun {
  id: string;
  schedule_id: string;
  instance_id: string;
  started_at: string;
  finished_at?: string;
  status: ScheduleRunStatus;
  exit_code?: number | null;
  output?: string;
  duration_ms?: number | null;
  created_at: string;
}

/** A schedule flattened with its latest run and all-time failure statistics. */
export interface ScheduleView extends Schedule {
  last_run?: ScheduleRun | null;
  runs_total: number;
  runs_failed: number;
  /** `runs_failed / runs_total`, `0` when the schedule never ran. */
  failure_rate: number;
}

export interface CreateScheduleInput {
  name: string;
  cron: string;
  prompt: string;
  enabled?: boolean;
  timezone?: string;
  overlap?: ScheduleOverlap;
  timeout?: number;
  catchup?: boolean;
}

export type PatchScheduleInput = Partial<CreateScheduleInput>;

/* --------------------------------------------------------------------------
 * Pipelines (docs/PIPELINES.md)
 * -------------------------------------------------------------------------- */

export type PipelineTrigger = "manual" | "cron" | "webhook";
export type PipelineStepStatus = "pending" | "running" | "ok" | "failed" | "skipped";
export type PipelineRunStatus = "pending" | "running" | "ok" | "failed" | "cancelled";
export type PipelineOnFailure = "stop" | "continue";

export interface PipelineStep {
  id: string;
  instance: string;
  prompt: string;
  depends_on?: string[];
  retries?: number;
  timeout_s?: number;
  on_failure?: PipelineOnFailure;
}

export interface Pipeline {
  id: string;
  name: string;
  trigger: PipelineTrigger;
  cron?: string;
  enabled: boolean;
  steps: PipelineStep[];
  created_at: string;
  updated_at: string;
}

export interface PipelineRun {
  id: string;
  pipeline_id: string;
  status: PipelineRunStatus;
  trigger: PipelineTrigger;
  context?: string;
  started_at?: string;
  finished_at?: string;
  error?: string;
}

export interface PipelineStepRun {
  id: number;
  run_id: string;
  step_id: string;
  instance_id: string;
  status: PipelineStepStatus;
  attempt: number;
  output?: string;
  error?: string;
  started_at?: string;
  finished_at?: string;
}

export interface PipelineRunDetail extends PipelineRun {
  steps: PipelineStepRun[];
}

export interface CreatePipelineInput {
  name: string;
  trigger?: PipelineTrigger;
  cron?: string;
  enabled?: boolean;
  steps: PipelineStep[];
}

export type PatchPipelineInput = Partial<CreatePipelineInput>;

/* --------------------------------------------------------------------------
 * Watchdog and incidents (docs/WATCHDOG.md)
 * -------------------------------------------------------------------------- */

export type IncidentCause = "oom" | "crashloop" | "unhealthy" | "image_missing" | "heal_failed";
export type IncidentStatus = "open" | "closed";
export type HealPolicy = "off" | "restart" | "recreate";

export interface IncidentAction {
  at: string;
  /** `restart`, `recreate`, `heal_exhausted` or `observe`. */
  action: string;
  /** `ok`, `error` or `skipped`. */
  result: string;
  detail?: string;
}

export interface Incident {
  id: string;
  instance_id: string;
  cause: IncidentCause;
  status: IncidentStatus;
  opened_at: string;
  closed_at?: string;
  detail: string;
  actions: IncidentAction[];
  restart_count: number;
  last_seen_at: string;
}

export interface HealPolicyView {
  id: string;
  heal: HealPolicy;
  unhealthy_after: number;
  max_restarts: number;
  heal_backoff_s: number;
}

/** Every field is optional: a missing field keeps the stored value. */
export interface HealPolicyInput {
  heal?: HealPolicy;
  unhealthy_after?: number;
  max_restarts?: number;
  heal_backoff_s?: number;
}

export interface ListIncidentsParams {
  status?: IncidentStatus;
  instance?: string;
  limit?: number;
}

/* --------------------------------------------------------------------------
 * Cost, budgets and pricing (docs/COST.md)
 * -------------------------------------------------------------------------- */

export type CostGroupBy = "instance" | "model" | "day";

export interface CostTotals {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cost_usd: number;
  events: number;
}

export interface CostGroup extends CostTotals {
  /** Instance id, model name or `YYYY-MM-DD` day. */
  key: string;
}

export interface AgentCost {
  instance_id: string;
  cost_usd: number;
  total_tokens: number;
}

export interface BudgetStatus {
  instance_id: string;
  month: string;
  monthly_usd: number;
  enabled: boolean;
  used_usd: number;
  /** `used / limit * 100`. */
  percent: number;
  exceeded: boolean;
  projected_month_usd: number;
  updated_at: string;
}

export interface CostProjection {
  month: string;
  elapsed_fraction: number;
  month_to_date_usd: number;
  projected_usd: number;
}

export interface CostReport {
  from: string;
  to: string;
  group_by: string;
  currency: string;
  totals: CostTotals;
  groups: CostGroup[];
  top_agents: AgentCost[];
  budgets: BudgetStatus[];
  projection: CostProjection;
}

export interface CostReportParams {
  from?: string;
  to?: string;
  group_by?: CostGroupBy;
}

export interface Pricing {
  model: string;
  input_per_mtok: number;
  output_per_mtok: number;
  currency: string;
  /** First day (`YYYY-MM-DD`) the price applies to. */
  effective_from: string;
  updated_at: string;
}

export interface PricingInput {
  model: string;
  input_per_mtok: number;
  output_per_mtok: number;
  currency: string;
  effective_from: string;
}

export interface BudgetView {
  instance_id: string;
  month: string;
  /** `null` when no budget is configured. */
  monthly_usd: number | null;
  enabled: boolean;
  configured: boolean;
  used_usd: number;
  percent: number;
  exceeded: boolean;
  projected_month_usd: number;
  updated_at?: string;
}

export interface BudgetInput {
  monthly_usd: number;
  enabled: boolean;
}

/* --------------------------------------------------------------------------
 * Log center (docs/LOGS.md)
 * -------------------------------------------------------------------------- */

export type LogLevel = "info" | "warn" | "error";

export interface LogEntry {
  id: number;
  instance: string;
  at: string;
  level: LogLevel;
  message: string;
}

export interface LogSearchResponse {
  entries: LogEntry[];
  count: number;
  limit: number;
}

export interface LogQueryParams {
  instances?: string[];
  q?: string;
  level?: LogLevel;
  /** RFC 3339 timestamp or a relative window (`30m`, `24h`, `7d`). */
  since?: string;
  until?: string;
  limit?: number;
}

export type LogExportFormat = "json" | "csv";

/* --------------------------------------------------------------------------
 * Security posture (docs/HARDENING.md)
 * -------------------------------------------------------------------------- */

export interface SecurityStatus {
  rate_limit: {
    enabled: boolean;
    limits: { per_min: number; auth_per_min: number; heavy_per_min: number };
  };
  lockout: { failures: number; window_s: number; lockout_s: number; locked_ips: number };
  headers: {
    csp: string;
    x_frame_options: string;
    referrer_policy: string;
    permissions_policy: string;
    x_content_type_options: string;
    hsts: boolean;
  };
  tokens: { expired_or_revoked: number; active: number };
  auth_mode: string;
}

/* --------------------------------------------------------------------------
 * Deployments and maintenance windows (docs/DEPLOYMENTS.md)
 * -------------------------------------------------------------------------- */

export type DeploymentStatus =
  | "pending"
  | "canary"
  | "running"
  | "scheduled"
  | "ok"
  | "failed"
  | "rolled_back"
  | "cancelled";

export interface DeploymentInstanceResult {
  id: string;
  from?: string | null;
  to: string;
  status: string;
  error?: string;
}

export interface Deployment {
  id: string;
  image: string;
  status: DeploymentStatus;
  instances: string[];
  canary: number;
  soak_s: number;
  started_at?: string;
  finished_at?: string;
  /** Per-instance results once it runs, the deferred request while scheduled. */
  result?: { request?: Record<string, unknown> } | DeploymentInstanceResult[] | null;
  error?: string | null;
  rollback: boolean;
  created_by: string;
}

export interface MaintenanceWindow {
  id: string;
  name: string;
  days: string[];
  /** UTC minute since midnight, inclusive. */
  start_minute: number;
  /** UTC minute since midnight, exclusive. */
  end_minute: number;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface MaintenanceWindowInput {
  name: string;
  days: string[];
  start_minute: number;
  end_minute: number;
  enabled: boolean;
}

export interface MaintenanceStatus {
  open: boolean;
  next_open?: string | null;
  enforce: boolean;
}

/* --------------------------------------------------------------------------
 * Snapshots (docs/SNAPSHOTS.md)
 * -------------------------------------------------------------------------- */

export interface Snapshot {
  id: string;
  instance_id: string;
  path: string;
  label: string;
  created_at: string;
  expires_at: string;
  size_bytes: number;
  /** `ok`, `restoring` or `failed`. */
  status: string;
  error?: string | null;
}

export interface CreateSnapshotInput {
  label?: string;
  /** 1 … 7 776 000 seconds; defaults to 7 days. */
  ttl_s?: number;
}

export interface SnapshotDiffFile {
  path: string;
  size: number;
  sha256: string;
}

export interface SnapshotChangedFile {
  path: string;
  size_a: number;
  size_b: number;
  sha256_a: string;
  sha256_b: string;
}

export interface SnapshotDiff {
  instance_id: string;
  a: string;
  b: string;
  added: SnapshotDiffFile[];
  removed: SnapshotDiffFile[];
  changed: SnapshotChangedFile[];
  unchanged: number;
}

export interface RestoreReport {
  snapshot: Snapshot;
  instance_id: string;
  files: number;
  status: string;
  env_restored: boolean;
  compose_restored: boolean;
  previous_data_dir?: string | null;
}

/* --------------------------------------------------------------------------
 * Instance ACL, egress policy and exec (docs/RBAC.md, docs/EGRESS-POLICY.md)
 * -------------------------------------------------------------------------- */

export type AclRole = "owner" | "operator" | "viewer";

export interface AclItem {
  subject: string;
  role: AclRole;
  created_at: string;
}

export interface InstanceAcl {
  instance: string;
  acl: AclItem[];
}

export interface AclItemInput {
  subject: string;
  role: AclRole;
}

export type EgressMode = "deny" | "allow";
export type EgressPolicySource = "explicit" | "allow_lan" | "legacy" | "disabled";

export interface EgressPolicyView {
  instance: string;
  source: EgressPolicySource;
  mode: EgressMode;
  fqdns: string[];
  cidrs: string[];
  ports: number[];
  updated_at?: string;
  proxy?: string;
  proxy_acl?: string;
  warnings: string[];
}

export interface EgressPolicyInput {
  mode: EgressMode;
  fqdns: string[];
  cidrs: string[];
  ports: number[];
}

export interface EgressTestResult {
  instance: string;
  host: string;
  port: number;
  /** Decision of the policy, not an observation of real traffic. */
  allowed: boolean;
  reason: string;
  source: EgressPolicySource;
  mode: EgressMode;
  /** True only when a component that could enforce the decision is reachable. */
  enforced: boolean;
  enforcement?: string;
  proxy?: string;
}

export interface ExecInput {
  /** Preferred: command and arguments already separated. */
  argv?: string[];
  /** Split on whitespace; refused characters include `; | & $` and quotes. */
  cmd?: string;
  timeout_s?: number;
}

export interface ExecResult {
  /** `null` when the timeout or a signal killed the command. */
  exit_code: number | null;
  stdout: string;
  stderr: string;
  timed_out: boolean;
  duration_ms: number;
  truncated: boolean;
}

export interface FleetApi {
  health(): Promise<Health>;
  /** Public: which login methods the controller advertises. */
  authConfig(): Promise<AuthConfig>;
  /** Identity behind the current credential; 401 when there is none. */
  me(): Promise<Identity>;
  checkAuth(): Promise<void>;
  /** Session login; a no-op navigation for the real API, a mock session in the adapter. */
  signInWithOidc(loginUrl: string): Promise<void>;
  /** Clears the session cookies. Harmless when there is no session. */
  logout(): Promise<void>;
  listTemplates(): Promise<Template[]>;
  listProviders(): Promise<ProviderSummary[]>;
  createProvider(input: CreateProviderInput): Promise<ProviderSummary>;
  getProvider(id: string): Promise<ProviderDetail>;
  patchProvider(id: string, input: PatchProviderInput): Promise<ProviderRotateResult>;
  deleteProvider(id: string): Promise<void>;
  testProvider(id: string): Promise<ProviderTestResult>;
  listInstances(): Promise<InstanceSummary[]>;
  getInstance(id: string): Promise<InstanceDetail>;
  /** Replaces the whole environment and applies it: rewrites compose and runs `up -d`. */
  putInstanceEnv(id: string, env: EnvInput[]): Promise<InstanceDetail>;
  /** Removes one variable and applies it. */
  deleteInstanceEnv(id: string, key: string): Promise<void>;
  createInstance(input: CreateInstanceInput): Promise<InstanceDetail>;
  cloneInstance(id: string, input: CloneInstanceInput): Promise<InstanceDetail>;
  patchInstance(id: string, input: PatchInstanceInput): Promise<InstanceDetail>;
  action(id: string, action: "start" | "stop" | "restart"): Promise<LifecycleResult>;
  deleteInstance(id: string, purge: boolean): Promise<void>;
  logs(id: string, tail: number): Promise<string>;
  listFiles(id: string, path: string): Promise<FileEntry[]>;
  readFile(id: string, path: string): Promise<FileContent>;
  /** Compare-and-swap write; `restart` restarts, `force` sends `If-Match: *`. */
  writeFile(id: string, input: WriteFileInput): Promise<WriteFileResult>;
  deleteFile(id: string, path: string): Promise<void>;
  mkdir(id: string, path: string): Promise<MkdirResult>;
  gitLog(id: string, options?: GitLogOptions): Promise<GitLog>;
  gitDiff(id: string, options?: GitDiffOptions): Promise<GitDiff>;
  gitStatus(id: string): Promise<GitStatus>;
  gitRevert(id: string, input: RevertInput): Promise<RevertResult>;
  listShares(): Promise<Share[]>;
  createShare(name: string): Promise<void>;
  deleteShare(name: string): Promise<void>;
  attachShare(name: string, instance: string, mode: ShareMode): Promise<void>;
  detachShare(name: string, instance: string): Promise<void>;
  listTokens(): Promise<TokenInfo[]>;
  createToken(input: CreateTokenInput): Promise<CreatedToken>;
  revokeToken(id: string): Promise<void>;
  listAudit(limit: number): Promise<AuditEntry[]>;
  /** Admin view of the whole bus, both directions. */
  listMessages(params?: ListMessagesParams): Promise<Message[]>;
  /** One instance mailbox; reading the inbox stamps `delivered_at`. */
  instanceMessages(id: string, params?: InstanceMessagesParams): Promise<Message[]>;
  /** The path id is the sender unless an admin overrides `from`. */
  sendMessage(id: string, input: SendMessageInput): Promise<Message>;
  /** Only the recipient can ack; stamps `read_at`. */
  ackMessage(id: string, messageId: string): Promise<AckMessageResult>;
  listMemory(params?: ListMemoryParams): Promise<MemoryEntry[]>;
  /** Full-text search; tags are ANDed. */
  searchMemory(params: SearchMemoryParams): Promise<MemoryEntry[]>;
  getMemoryByKey(namespace: string, key: string): Promise<MemoryEntry>;
  /** Upsert keyed by `(namespace, key)`. */
  createOrUpdateMemory(input: CreateMemoryInput): Promise<WriteMemoryResult>;
  updateMemory(id: number, input: UpdateMemoryInput): Promise<MemoryEntry>;
  /** Soft delete: the entry disappears from reads, its history stays restorable. */
  deleteMemory(id: number): Promise<void>;
  memoryHistory(id: number): Promise<MemoryVersion[]>;
  /** Restores an old value as a new version. */
  rollbackMemory(id: number, version: number): Promise<MemoryEntry>;
  /** Copies the entry into another namespace; the source stays readable. */
  promoteMemory(id: number, namespace: string): Promise<MemoryEntry>;
  purgeExpiredMemory(): Promise<PurgeResult>;

  /* Schedules */
  listSchedules(): Promise<ScheduleView[]>;
  listInstanceSchedules(id: string): Promise<Schedule[]>;
  createSchedule(id: string, input: CreateScheduleInput): Promise<Schedule>;
  patchSchedule(scheduleId: string, input: PatchScheduleInput): Promise<Schedule>;
  deleteSchedule(scheduleId: string): Promise<void>;
  runSchedule(scheduleId: string): Promise<ScheduleRun>;
  listRuns(id: string, limit?: number): Promise<ScheduleRun[]>;

  /* Pipelines */
  listPipelines(): Promise<Pipeline[]>;
  getPipeline(id: string): Promise<Pipeline>;
  createPipeline(input: CreatePipelineInput): Promise<Pipeline>;
  patchPipeline(id: string, input: PatchPipelineInput): Promise<Pipeline>;
  deletePipeline(id: string): Promise<void>;
  runPipeline(id: string, context?: unknown): Promise<PipelineRunDetail>;
  listPipelineRuns(id: string, limit?: number): Promise<PipelineRun[]>;
  getPipelineRun(id: string, runId: string): Promise<PipelineRunDetail>;
  cancelPipelineRun(id: string, runId: string): Promise<PipelineRunDetail>;

  /* Incidents and healing */
  listIncidents(params?: ListIncidentsParams): Promise<Incident[]>;
  instanceIncidents(id: string, limit?: number): Promise<Incident[]>;
  closeIncident(id: string): Promise<Incident>;
  patchHeal(id: string, input: HealPolicyInput): Promise<HealPolicyView>;

  /* Cost */
  costReport(params?: CostReportParams): Promise<CostReport>;
  listBudgets(): Promise<BudgetStatus[]>;
  getBudget(id: string): Promise<BudgetView>;
  setBudget(id: string, input: BudgetInput): Promise<BudgetView>;
  listPricing(): Promise<Pricing[]>;
  setPricing(input: PricingInput): Promise<Pricing>;
  deletePricing(model: string): Promise<void>;

  /* Log center */
  searchLogs(params?: LogQueryParams): Promise<LogSearchResponse>;
  exportLogs(
    params: LogQueryParams | undefined,
    format: LogExportFormat,
  ): Promise<{ blob: Blob; filename: string }>;

  /* Security */
  securityStatus(): Promise<SecurityStatus>;

  /* Deployments and maintenance */
  listDeployments(params?: { limit?: number; status?: DeploymentStatus }): Promise<Deployment[]>;
  getDeployment(id: string): Promise<Deployment>;
  listMaintenanceWindows(): Promise<MaintenanceWindow[]>;
  createMaintenanceWindow(input: MaintenanceWindowInput): Promise<MaintenanceWindow>;
  patchMaintenanceWindow(id: string, input: Partial<MaintenanceWindowInput>): Promise<MaintenanceWindow>;
  deleteMaintenanceWindow(id: string): Promise<void>;
  maintenanceStatus(): Promise<MaintenanceStatus>;

  /* Snapshots */
  listSnapshots(id: string): Promise<Snapshot[]>;
  createSnapshot(id: string, input?: CreateSnapshotInput): Promise<Snapshot>;
  compareSnapshots(id: string, a: string, b: string): Promise<SnapshotDiff>;
  restoreSnapshot(snapshotId: string): Promise<RestoreReport>;
  deleteSnapshot(snapshotId: string): Promise<void>;

  /* Instance ACL, egress and exec */
  getInstanceAcl(id: string): Promise<InstanceAcl>;
  putInstanceAcl(id: string, acl: AclItemInput[]): Promise<InstanceAcl>;
  getEgress(id: string): Promise<EgressPolicyView>;
  putEgress(id: string, input: EgressPolicyInput): Promise<EgressPolicyView>;
  testEgress(id: string, host: string, port: number): Promise<EgressTestResult>;
  exec(id: string, input: ExecInput): Promise<ExecResult>;
}
