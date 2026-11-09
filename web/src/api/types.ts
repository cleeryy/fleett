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
}
