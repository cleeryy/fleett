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

export interface Template {
  id: string;
  name: string;
  description: string;
  default_model: string;
  channel: string;
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
  env_keys: string[];
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
  /** Write-only. Encrypted at rest as HERMES_GATE_PROFILE_TOKEN. */
  gate_token?: string;
  channel: ChannelInput;
  shares: ShareRef[];
  mem_limit_mb: number;
  /** false keeps the agent on the restricted `fleett` network only. */
  allow_lan: boolean;
  notes: string;
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
  /** Write-only. Rotates the provider key written as HERMES_GATE_PROFILE_TOKEN. */
  gate_token?: string;
  channel?: ChannelInput;
  shares?: ShareRef[];
  mem_limit_mb?: number;
  /** Turning LAN access off fails with 409 `egress_not_enforced` until the host firewall is installed. */
  allow_lan?: boolean;
  notes?: string;
}

export interface LifecycleResult {
  status: InstanceStatus;
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
  listInstances(): Promise<InstanceSummary[]>;
  getInstance(id: string): Promise<InstanceDetail>;
  createInstance(input: CreateInstanceInput): Promise<InstanceDetail>;
  cloneInstance(id: string, input: CloneInstanceInput): Promise<InstanceDetail>;
  patchInstance(id: string, input: PatchInstanceInput): Promise<InstanceDetail>;
  action(id: string, action: "start" | "stop" | "restart"): Promise<LifecycleResult>;
  deleteInstance(id: string, purge: boolean): Promise<void>;
  logs(id: string, tail: number): Promise<string>;
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
