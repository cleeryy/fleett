import { ApiError } from "./errors";
import type {
  ChannelType,
  CreateInstanceInput,
  FleetApi,
  Health,
  InstanceDetail,
  InstanceStatus,
  InstanceSummary,
  LifecycleResult,
  PatchInstanceInput,
  Share,
  ShareMode,
  Template,
} from "./types";
import { tokenStore } from "@/lib/token";
import { isValidSlug } from "@/lib/validation";

/**
 * In-memory adapter used when VITE_MOCK is on (default in development).
 * It implements every endpoint in API.md, including the error statuses the UI
 * has to render: 401, 404, 409 and 502.
 */

const DEMO_TOKEN = "demo";
const IMAGE = "hermes-agent:0.22.2-local";
const FLEET_ROOT = "/opt/hermes-fleet";
const DOCKER_ERROR =
  "compose up failed (exit 1): Error response from daemon: pull access denied for hermes-agent:0.22.2-local, " +
  "repository does not exist or may require 'docker login': denied: requested access to the resource is denied";

interface Stored {
  id: string;
  name: string;
  template: string;
  model: string;
  base_url: string;
  api_key_env: string;
  /** Write-only secret; only its presence is exposed through env_keys. */
  gate_token: string | null;
  channel: ChannelType;
  status: InstanceStatus;
  shares: { name: string; mode: ShareMode }[];
  mem_limit_mb: number;
  notes: string;
  created_at: string;
  updated_at: string;
  last_error: string | null;
  container_id: string | null;
  started_at: number | null;
  /** The next lifecycle call fails once, to exercise the 502 path. */
  fail_next: boolean;
  logs: string[];
}

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const SEEDED_AT = Date.now();

let rows: Stored[] = [];
let shares: Share[] = [];
let templates: Template[] = [];

// ---------------------------------------------------------------------------
// helpers

function delay(ms = 140 + Math.random() * 180): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function fail(status: number, code: string, message: string): never {
  throw new ApiError(status, code, message);
}

function requireAuth(): void {
  if (tokenStore.get() !== DEMO_TOKEN) {
    fail(401, "unauthorized", "Missing or invalid bearer token.");
  }
}

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

function nowIso(): string {
  return new Date().toISOString();
}

function containerId(): string {
  const bytes = new Uint8Array(6);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function envKeys(row: Stored): string[] {
  const keys: string[] = [];
  if (row.gate_token) keys.push("HERMES_GATE_PROFILE_TOKEN");
  if (row.channel === "telegram") keys.push("TELEGRAM_BOT_TOKEN");
  if (row.channel === "discord") keys.push("DISCORD_BOT_TOKEN");
  return [...new Set(keys)];
}

function composeYaml(row: Stored): string {
  const mounts = [`      - ./data:/opt/data`];
  for (const share of row.shares) {
    mounts.push(`      - ../../shares/${share.name}:/opt/data/shared/${share.name}:${share.mode}`);
  }
  const channelLine =
    row.channel === "none"
      ? "      HERMES_CHANNEL: none"
      : `      HERMES_CHANNEL: ${row.channel}`;

  return [
    "# Managed by hermes-fleet. Edit through the API: hand edits are overwritten.",
    `name: hermes-${row.id}`,
    "",
    "services:",
    "  hermes:",
    `    image: ${IMAGE}`,
    "    restart: unless-stopped",
    '    user: "10000:10000"',
    "    env_file:",
    "      - secrets.env",
    "    environment:",
    `      HERMES_PROFILE: ${row.id}`,
    `      HERMES_MODEL: ${row.model}`,
    `      HERMES_BASE_URL: ${row.base_url}`,
    channelLine,
    "    volumes:",
    ...mounts,
    "    networks:",
    "      - hermes-fleet",
    "    security_opt:",
    "      - no-new-privileges:true",
    "    cap_drop:",
    "      - NET_RAW",
    "    tmpfs:",
    "      - /tmp",
    "    pids_limit: 256",
    `    mem_limit: ${row.mem_limit_mb}m`,
    "    logging:",
    "      driver: json-file",
    "      options:",
    "        max-size: 10m",
    '        max-file: "3"',
    "",
    "networks:",
    "  hermes-fleet:",
    "    external: true",
    "",
  ].join("\n");
}

function toDetail(row: Stored): InstanceDetail {
  const uptime =
    row.status === "running" && row.started_at
      ? Math.max(0, Math.floor((Date.now() - row.started_at) / 1000))
      : 0;

  return {
    id: row.id,
    name: row.name,
    template: row.template,
    model: row.model,
    base_url: row.base_url,
    api_key_env: row.api_key_env,
    channel: row.channel,
    status: row.status,
    shares: row.shares.map((share) => ({ ...share })),
    mem_limit_mb: row.mem_limit_mb,
    notes: row.notes,
    created_at: row.created_at,
    updated_at: row.updated_at,
    last_error: row.last_error,
    data_dir: `${FLEET_ROOT}/instances/${row.id}/data`,
    env_keys: envKeys(row),
    container: row.container_id
      ? { id: row.container_id, image: IMAGE, uptime_s: uptime }
      : null,
    compose_yaml: composeYaml(row),
  };
}

function toSummary(row: Stored): InstanceSummary {
  return {
    id: row.id,
    name: row.name,
    template: row.template,
    model: row.model,
    channel: row.channel,
    status: row.status,
    shares: row.shares.map((share) => share.name),
    created_at: row.created_at,
  };
}

function findRow(id: string): Stored {
  const row = rows.find((candidate) => candidate.id === id);
  if (!row) fail(404, "not_found", `Instance "${id}" does not exist.`);
  return row;
}

function findShare(name: string): Share {
  const share = shares.find((candidate) => candidate.name === name);
  if (!share) fail(404, "not_found", `Share "${name}" does not exist.`);
  return share;
}

function assertSharesExist(refs: { name: string }[]): void {
  for (const ref of refs) {
    if (!shares.some((share) => share.name === ref.name)) {
      fail(400, "invalid_input", `Unknown share "${ref.name}".`);
    }
  }
}

function attachToShare(shareName: string, instanceId: string, mode: ShareMode): void {
  const share = shares.find((candidate) => candidate.name === shareName);
  if (!share) return;
  if (share.instances.some((entry) => entry.id === instanceId)) return;
  share.instances.push({ id: instanceId, mode });
}

function detachFromShare(shareName: string, instanceId: string): void {
  const share = shares.find((candidate) => candidate.name === shareName);
  if (!share) return;
  share.instances = share.instances.filter((entry) => entry.id !== instanceId);
}

function syncShareLinks(rowId: string, next: { name: string; mode: ShareMode }[]): void {
  const previous = rows.find((row) => row.id === rowId)?.shares ?? [];
  for (const ref of previous) {
    if (!next.some((candidate) => candidate.name === ref.name)) {
      detachFromShare(ref.name, rowId);
    }
  }
  for (const ref of next) {
    attachToShare(ref.name, rowId, ref.mode);
  }
}

// ---------------------------------------------------------------------------
// seeded logs

function logLine(ms: number, level: "INFO" | "WARN" | "ERROR", message: string): string {
  return `${iso(ms)} ${level.padEnd(5)} ${message}`;
}

function makeLogs(row: Stored): string[] {
  if (row.status === "created") return [];

  const start = new Date(row.created_at).getTime() + 1_000;
  const lines: string[] = [
    logLine(start, "INFO", "hermes 0.22.2 starting (profile=default)"),
    logLine(start + 18, "INFO", "config loaded /opt/data/.hermes/config.yaml (v50)"),
    logLine(start + 44, "INFO", `model ${row.model} base_url=${row.base_url}`),
    logLine(start + 96, "INFO", "sqlite journal_mode=wal path=/opt/data/.hermes/state.db"),
    logLine(
      start + 180,
      "INFO",
      row.channel === "none" ? "channel disabled" : `channel ${row.channel} connected`,
    ),
    logLine(start + 240, "INFO", "gateway listening on 127.0.0.1:9119"),
    logLine(start + 320, "INFO", "scheduler started (cti every 15m)"),
  ];

  if (row.status === "error") {
    lines.push(
      logLine(start + 420, "INFO", `creating container hermes-${row.id}`),
      logLine(start + 610, "ERROR", DOCKER_ERROR),
      logLine(start + 640, "ERROR", "rollback: container not created"),
    );
    return lines;
  }

  const tools = ["web.search", "memory.write", "file.read", "gateway.ping"];
  for (let i = 0; i < 14; i += 1) {
    const at = start + 900_000 + i * 640_000;
    lines.push(logLine(at, "INFO", `session ses_${(i + 11).toString(16)}f${row.id.slice(0, 3)} turn started`));
    lines.push(logLine(at + 1_400, "INFO", `tool ${tools[i % tools.length]} ok ${(180 + ((i * 97) % 620)).toString()}ms`));
    if (i === 6) {
      lines.push(logLine(at + 2_300, "WARN", "upstream 429, retry 1/3 in 2s"));
      lines.push(logLine(at + 4_800, "INFO", "retry ok"));
    }
  }

  if (row.status === "stopped") {
    const at = start + 11 * 900_000;
    lines.push(
      logLine(at, "INFO", "received SIGTERM, shutting down"),
      logLine(at + 160, "INFO", "gateway stopped"),
    );
  }

  return lines;
}

// ---------------------------------------------------------------------------
// seed

function seedRow(input: {
  id: string;
  name: string;
  template: string;
  model: string;
  channel: ChannelType;
  status: InstanceStatus;
  shares: { name: string; mode: ShareMode }[];
  age_days: number;
  uptime_hours: number | null;
  mem_limit_mb?: number;
  notes?: string;
  last_error?: string | null;
  gate_token?: string;
}): Stored {
  const created = SEEDED_AT - input.age_days * DAY;
  const running = input.status === "running";
  const row: Stored = {
    id: input.id,
    name: input.name,
    template: input.template,
    model: input.model,
    base_url: "https://opencode.ai/inference/go/openai/v1",
    api_key_env: "HERMES_GATE_PROFILE_TOKEN",
    gate_token: input.gate_token ?? null,
    channel: input.channel,
    status: input.status,
    shares: input.shares,
    mem_limit_mb: input.mem_limit_mb ?? 1536,
    notes: input.notes ?? "",
    created_at: iso(created),
    updated_at: iso(created + 2 * HOUR),
    last_error: input.last_error ?? null,
    container_id: input.status === "created" ? null : containerId(),
    started_at: running && input.uptime_hours !== null ? SEEDED_AT - input.uptime_hours * HOUR : null,
    fail_next: input.id === "workshop",
    logs: [],
  };
  row.logs = makeLogs(row);
  return row;
}

export function resetMock(): void {
  templates = [
    {
      id: "guest",
      name: "Guest",
      description: "Web and memory only. No infrastructure access.",
      default_model: "deepseek-v4.1-flash",
      channel: "telegram",
    },
    {
      id: "family",
      name: "Family",
      description: "Everyday assistant with a shared photo archive.",
      default_model: "deepseek-v4.1-flash",
      channel: "discord",
    },
    {
      id: "ops",
      name: "Ops",
      description: "Homelab diagnostics. Read access to operations shares.",
      default_model: "muse-spark-1.3-contributor",
      channel: "none",
    },
    {
      id: "sandbox",
      name: "Sandbox",
      description: "Free execution, isolated network, disposable data.",
      default_model: "deepseek-v4.1-flash",
      channel: "none",
    },
  ];

  shares = [
    {
      name: "findings",
      path: `${FLEET_ROOT}/shares/findings`,
      instances: [
        { id: "alice", mode: "rw" },
        { id: "ops", mode: "ro" },
      ],
    },
    {
      name: "media",
      path: `${FLEET_ROOT}/shares/media`,
      instances: [{ id: "margaret", mode: "ro" }],
    },
    {
      name: "runbooks",
      path: `${FLEET_ROOT}/shares/runbooks`,
      instances: [{ id: "ops", mode: "rw" }],
    },
  ];

  rows = [
    seedRow({
      id: "alice",
      name: "Alice",
      template: "guest",
      model: "deepseek-v4.1-flash",
      channel: "telegram",
      status: "running",
      shares: [{ name: "findings", mode: "rw" }],
      age_days: 3,
      uptime_hours: 4.2,
      gate_token: "seed-alice",
      notes: "Friend account. Telegram bot for personal use.",
    }),
    seedRow({
      id: "margaret",
      name: "Margaret",
      template: "family",
      model: "deepseek-v4.1-flash",
      channel: "discord",
      status: "stopped",
      shares: [{ name: "media", mode: "ro" }],
      age_days: 12,
      uptime_hours: null,
      gate_token: "seed-margaret",
      notes: "Family account. Read-only access to the photo archive.",
    }),
    seedRow({
      id: "ops",
      name: "Ops",
      template: "ops",
      model: "muse-spark-1.3-contributor",
      channel: "none",
      status: "running",
      shares: [
        { name: "findings", mode: "ro" },
        { name: "runbooks", mode: "rw" },
      ],
      age_days: 6,
      uptime_hours: 51,
      gate_token: "seed-ops",
      mem_limit_mb: 3072,
      notes: "Homelab diagnostics. No Docker socket.",
    }),
    seedRow({
      id: "school",
      name: "School",
      template: "sandbox",
      model: "deepseek-v4.1-flash",
      channel: "none",
      status: "created",
      shares: [],
      age_days: 0.1,
      uptime_hours: null,
      notes: "Test agent. Delete when unused.",
    }),
    seedRow({
      id: "workshop",
      name: "Workshop",
      template: "ops",
      model: "muse-spark-1.3-contributor",
      channel: "none",
      status: "error",
      shares: [],
      age_days: 1,
      uptime_hours: null,
      gate_token: "seed-workshop",
      last_error: DOCKER_ERROR,
      notes: "Template check. Kept for the error state.",
    }),
  ];
}

resetMock();

// ---------------------------------------------------------------------------
// adapter

function validateChannel(channel: { type: ChannelType; token?: string }, previous: ChannelType): void {
  if (channel.type === "none") return;
  if (!channel.token && channel.type !== previous) {
    fail(400, "invalid_input", `A token is required when switching the channel to ${channel.type}.`);
  }
}

export const mockApi: FleetApi = {
  async health(): Promise<Health> {
    await delay(60);
    return { ok: true, version: "0.1.0", driver: "mock", docker: { ok: true } };
  },

  async checkAuth(): Promise<void> {
    await delay(220);
    requireAuth();
  },

  async listTemplates(): Promise<Template[]> {
    requireAuth();
    await delay();
    return templates.map((template) => ({ ...template }));
  },

  async listInstances(): Promise<InstanceSummary[]> {
    requireAuth();
    await delay();
    return rows.map(toSummary);
  },

  async getInstance(id: string): Promise<InstanceDetail> {
    requireAuth();
    await delay();
    return toDetail(findRow(id));
  },

  async createInstance(input: CreateInstanceInput): Promise<InstanceDetail> {
    requireAuth();
    await delay(320);

    const id = input.id.trim();
    if (!isValidSlug(id)) {
      fail(400, "invalid_input", "Id must be 2-32 characters: lowercase letters, digits, hyphens.");
    }
    if (rows.some((row) => row.id === id)) {
      fail(409, "conflict", `Instance "${id}" already exists.`);
    }
    if (!templates.some((template) => template.id === input.template)) {
      fail(400, "invalid_input", `Unknown template "${input.template}".`);
    }
    if (!input.name.trim()) {
      fail(400, "invalid_input", "Name is required.");
    }
    if (!input.model.trim()) {
      fail(400, "invalid_input", "Model is required.");
    }
    validateChannel(input.channel, "none");
    assertSharesExist(input.shares);

    const created = Date.now();
    const row: Stored = {
      id,
      name: input.name.trim(),
      template: input.template,
      model: input.model.trim(),
      base_url: input.base_url.trim(),
      api_key_env: input.api_key_env.trim(),
      gate_token: input.gate_token?.trim() ? input.gate_token.trim() : null,
      channel: input.channel.type,
      status: "running",
      shares: input.shares.map((share) => ({ name: share.name, mode: share.mode })),
      mem_limit_mb: input.mem_limit_mb,
      notes: input.notes.trim(),
      created_at: iso(created),
      updated_at: iso(created),
      last_error: null,
      container_id: containerId(),
      started_at: created,
      fail_next: false,
      logs: [],
    };
    row.logs = makeLogs(row);
    rows.unshift(row);
    for (const share of row.shares) attachToShare(share.name, id, share.mode);
    return toDetail(row);
  },

  async patchInstance(id: string, input: PatchInstanceInput): Promise<InstanceDetail> {
    requireAuth();
    await delay(300);
    const row = findRow(id);

    if (input.name !== undefined && !input.name.trim()) {
      fail(400, "invalid_input", "Name is required.");
    }
    if (input.model !== undefined && !input.model.trim()) {
      fail(400, "invalid_input", "Model is required.");
    }
    if (input.mem_limit_mb !== undefined && (input.mem_limit_mb < 128 || input.mem_limit_mb > 8192)) {
      fail(400, "invalid_input", "Memory limit must be between 128 and 8192 MB.");
    }
    if (input.channel) validateChannel(input.channel, row.channel);
    if (input.shares) assertSharesExist(input.shares);

    if (input.name !== undefined) row.name = input.name.trim();
    if (input.model !== undefined) row.model = input.model.trim();
    if (input.base_url !== undefined) row.base_url = input.base_url.trim();
    if (input.api_key_env !== undefined) row.api_key_env = input.api_key_env.trim();
    if (input.gate_token !== undefined && input.gate_token.trim()) {
      row.gate_token = input.gate_token.trim();
    }
    if (input.mem_limit_mb !== undefined) row.mem_limit_mb = input.mem_limit_mb;
    if (input.notes !== undefined) row.notes = input.notes;

    if (input.channel) {
      row.channel = input.channel.type;
    }

    if (input.shares) {
      const next = input.shares.map((share) => ({ name: share.name, mode: share.mode }));
      syncShareLinks(row.id, next);
      row.shares = next;
    }

    row.updated_at = nowIso();
    row.last_error = row.status === "error" ? row.last_error : null;
    return toDetail(row);
  },

  async action(id, action): Promise<LifecycleResult> {
    requireAuth();
    await delay(360);
    const row = findRow(id);

    if (row.fail_next) {
      row.fail_next = false;
      row.status = "error";
      row.last_error = DOCKER_ERROR;
      row.container_id = null;
      row.started_at = null;
      row.updated_at = nowIso();
      fail(502, "docker_error", DOCKER_ERROR);
    }

    if (action === "stop") {
      row.status = "stopped";
      row.started_at = null;
      row.updated_at = nowIso();
      row.logs.push(
        logLine(Date.now(), "INFO", "received SIGTERM, shutting down"),
        logLine(Date.now() + 120, "INFO", "gateway stopped"),
      );
    } else {
      if (!row.container_id) row.container_id = containerId();
      row.status = "running";
      row.started_at = Date.now();
      row.updated_at = nowIso();
      row.last_error = null;
      row.logs.push(...makeLogs({ ...row, created_at: iso(Date.now()) }));
    }

    return { status: row.status };
  },

  async deleteInstance(id, _purge): Promise<void> {
    requireAuth();
    await delay(340);
    const row = findRow(id);
    for (const share of row.shares) detachFromShare(share.name, id);
    rows = rows.filter((candidate) => candidate.id !== id);
  },

  async logs(id, tail): Promise<string> {
    requireAuth();
    await delay(180);
    const row = findRow(id);
    const count = Number.isFinite(tail) && tail > 0 ? Math.min(tail, 5000) : 200;
    return row.logs.slice(-count).join("\n");
  },

  async listShares(): Promise<Share[]> {
    requireAuth();
    await delay();
    return shares.map((share) => ({
      ...share,
      instances: share.instances.map((entry) => ({ ...entry })),
    }));
  },

  async createShare(name: string): Promise<void> {
    requireAuth();
    await delay(260);
    if (!isValidSlug(name)) {
      fail(400, "invalid_input", "Name must be 2-32 characters: lowercase letters, digits, hyphens.");
    }
    if (shares.some((share) => share.name === name)) {
      fail(409, "conflict", `Share "${name}" already exists.`);
    }
    shares.push({ name, path: `${FLEET_ROOT}/shares/${name}`, instances: [] });
  },

  async deleteShare(name: string): Promise<void> {
    requireAuth();
    await delay(260);
    const share = findShare(name);
    if (share.instances.length > 0) {
      fail(
        409,
        "conflict",
        `Share "${name}" is still attached to ${share.instances.length} instance${
          share.instances.length > 1 ? "s" : ""
        }. Detach it first.`,
      );
    }
    shares = shares.filter((candidate) => candidate.name !== name);
  },

  async attachShare(name, instance, mode): Promise<void> {
    requireAuth();
    await delay(300);
    const share = findShare(name);
    const row = findRow(instance);
    if (mode !== "rw" && mode !== "ro") fail(400, "invalid_input", "Mode must be rw or ro.");
    if (share.instances.some((entry) => entry.id === instance)) {
      fail(409, "conflict", `Share "${name}" is already attached to "${instance}".`);
    }
    share.instances.push({ id: instance, mode });
    if (!row.shares.some((ref) => ref.name === name)) {
      row.shares.push({ name, mode });
      row.updated_at = nowIso();
    }
  },

  async detachShare(name, instance): Promise<void> {
    requireAuth();
    await delay(280);
    const share = findShare(name);
    if (!share.instances.some((entry) => entry.id === instance)) {
      fail(404, "not_found", `Share "${name}" is not attached to "${instance}".`);
    }
    detachFromShare(name, instance);
    const row = rows.find((candidate) => candidate.id === instance);
    if (row) {
      row.shares = row.shares.filter((ref) => ref.name !== name);
      row.updated_at = nowIso();
    }
  },
};
