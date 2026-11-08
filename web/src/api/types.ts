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
  checkAuth(): Promise<void>;
  listTemplates(): Promise<Template[]>;
  listInstances(): Promise<InstanceSummary[]>;
  getInstance(id: string): Promise<InstanceDetail>;
  createInstance(input: CreateInstanceInput): Promise<InstanceDetail>;
  patchInstance(id: string, input: PatchInstanceInput): Promise<InstanceDetail>;
  action(id: string, action: "start" | "stop" | "restart"): Promise<LifecycleResult>;
  deleteInstance(id: string, purge: boolean): Promise<void>;
  logs(id: string, tail: number): Promise<string>;
  listShares(): Promise<Share[]>;
  createShare(name: string): Promise<void>;
  deleteShare(name: string): Promise<void>;
  attachShare(name: string, instance: string, mode: ShareMode): Promise<void>;
  detachShare(name: string, instance: string): Promise<void>;
}
