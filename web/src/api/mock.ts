import { ApiError } from "./errors";
import type {
  AuditEntry,
  AuthConfig,
  AuthMode,
  ChannelType,
  CloneInstanceInput,
  CommitInfo,
  CreateInstanceInput,
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
  InstanceStatus,
  InstanceSummary,
  LifecycleResult,
  MkdirResult,
  PatchInstanceInput,
  RevertInput,
  RevertResult,
  Role,
  Share,
  ShareMode,
  Template,
  TokenInfo,
  WriteFileInput,
  WriteFileResult,
} from "./types";
import { toPatch } from "@/lib/diff";
import { tokenStore } from "@/lib/token";
import { isValidSlug } from "@/lib/validation";

/**
 * In-memory adapter used when VITE_MOCK is on (default in development).
 * It implements every endpoint in API.md, including the error statuses the UI
 * has to render: 401, 403, 404, 409 and 502.
 */

const DEMO_TOKEN = "demo";
const DEMO_VIEWER_TOKEN = "demo-viewer";
const IMAGE = "hermes-agent:0.22.2-local";
const FLEETT_ROOT = "/opt/fleett";
const DOCKER_ERROR =
  "compose up failed (exit 1): Error response from daemon: pull access denied for hermes-agent:0.22.2-local, " +
  "repository does not exist or may require 'docker login': denied: requested access to the resource is denied";

const EGRESS_ERROR =
  "LAN access is disabled for this instance, but the host egress firewall is not installed. " +
  "The agent could reach private networks, so the change was refused.";

const SECRETS_ERROR =
  "no secrets key is configured; run `sudo fleett secrets init` (or set FLEETT_SECRETS_KEY) and retry";

/**
 * Auth advertised by the mock, from `VITE_MOCK_AUTH`:
 * `bearer` (default), `oidc`, or `both`. In `oidc`/`both`, the Authentik
 * button opens a demo admin session so the console is usable without a backend.
 */
export const mockAuthMode: AuthMode = (() => {
  const raw = (import.meta.env.VITE_MOCK_AUTH as string | undefined)?.trim().toLowerCase();
  return raw === "oidc" || raw === "both" ? raw : "bearer";
})();

/**
 * Switchable mock behaviour, so the UI can be exercised in both worlds.
 * `egressEnforced` mirrors `fleett egress install` on the host: when false,
 * creating a restricted instance (or turning LAN access off) fails with
 * 409 `egress_not_enforced`, exactly like the controller.
 * Set `VITE_MOCK_EGRESS_ENFORCED=0` (dev/build) or assign `mockConfig.egressEnforced = false`.
 * `secretsKey` mirrors `fleett secrets init`: when false, storing a provider
 * token fails with 409 `secrets_key_missing`.
 */
export const mockConfig = {
  egressEnforced: (import.meta.env.VITE_MOCK_EGRESS_ENFORCED ?? "1") !== "0",
  secretsKey: (import.meta.env.VITE_MOCK_SECRETS_KEY ?? "1") !== "0",
};

function assertEgress(allowLan: boolean): void {
  if (!allowLan && !mockConfig.egressEnforced) {
    fail(409, "egress_not_enforced", EGRESS_ERROR);
  }
}

function assertSecretsKey(token: string | null | undefined): void {
  if (token && !mockConfig.secretsKey) {
    fail(409, "secrets_key_missing", SECRETS_ERROR);
  }
}

interface StoredEnv {
  key: string;
  value: string;
  secret: boolean;
  updated_at: string;
}

interface Stored {
  id: string;
  name: string;
  template: string;
  model: string;
  base_url: string;
  api_key_env: string;
  /** Write-only secret; only its presence is exposed through the env list. */
  gate_token: string | null;
  /** Write-only secret for the platform bot; never returned. */
  channel_token: string | null;
  channel: ChannelType;
  status: InstanceStatus;
  shares: { name: string; mode: ShareMode }[];
  mem_limit_mb: number;
  /** true = the compose service also joins `fleett-lan`. */
  allow_lan: boolean;
  notes: string;
  created_at: string;
  updated_at: string;
  last_error: string | null;
  container_id: string | null;
  started_at: number | null;
  /** Extra variables, excluding the gate and channel tokens. */
  env: StoredEnv[];
  /** The next lifecycle call fails once, to exercise the 502 path. */
  fail_next: boolean;
  logs: string[];
}

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const SEEDED_AT = Date.now();

const ADMIN_SESSION: Identity = {
  authenticated: true,
  actor: "cleeryy@example.com",
  role: "admin",
  method: "session",
  user: { sub: "mock-1", email: "cleeryy@example.com", name: "Cleeryy" },
};

const ADMIN_BEARER: Identity = {
  authenticated: true,
  actor: "demo",
  role: "admin",
  method: "bearer",
};

const VIEWER_BEARER: Identity = {
  authenticated: true,
  actor: "demo-viewer",
  role: "viewer",
  method: "bearer",
};

/** Set by the mock SSO button; survives reloads only in memory. */
let mockSession: Identity | null = null;

let rows: Stored[] = [];
let shares: Share[] = [];
let templates: Template[] = [];
let tokens: TokenInfo[] = [];
let audit: AuditEntry[] = [];

// ---------------------------------------------------------------------------
// helpers

function delay(ms = 140 + Math.random() * 180): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function fail(status: number, code: string, message: string, details?: unknown): never {
  throw new ApiError(status, code, message, details);
}

function currentIdentity(): Identity | null {
  if (mockSession) return mockSession;
  const token = tokenStore.get();
  if (token === DEMO_TOKEN) return ADMIN_BEARER;
  if (token === DEMO_VIEWER_TOKEN) return VIEWER_BEARER;
  return null;
}

function requireAuth(): Identity {
  const identity = currentIdentity();
  if (!identity) {
    fail(401, "unauthorized", "Missing or invalid bearer token.");
  }
  return identity;
}

/** Every mutation: admin only, like the controller's role middleware. */
function requireWrite(): Identity {
  const identity = requireAuth();
  if (identity.role !== "admin") {
    fail(403, "forbidden", "the viewer role is read-only");
  }
  return identity;
}

function requireAdmin(): Identity {
  const identity = requireAuth();
  if (identity.role !== "admin") {
    fail(403, "forbidden", "this endpoint requires the admin role");
  }
  return identity;
}

function randomHex(bytes: number): string {
  const buffer = new Uint8Array(bytes);
  crypto.getRandomValues(buffer);
  return Array.from(buffer, (b) => b.toString(16).padStart(2, "0")).join("");
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

const GATE_ENV = "HERMES_GATE_PROFILE_TOKEN";

function gateKeys(row: Stored): string[] {
  const keys = [GATE_ENV];
  if (row.api_key_env && row.api_key_env !== GATE_ENV) keys.push(row.api_key_env);
  return keys;
}

function channelEnv(channel: ChannelType): string | null {
  if (channel === "telegram") return "TELEGRAM_BOT_TOKEN";
  if (channel === "discord") return "DISCORD_BOT_TOKEN";
  return null;
}

/** Rows the controller owns: the gate and platform tokens are always secret. */
function forcedEnv(row: Stored): StoredEnv[] {
  const entries: StoredEnv[] = [];
  if (row.gate_token) {
    for (const key of gateKeys(row)) {
      entries.push({ key, value: row.gate_token, secret: true, updated_at: row.updated_at });
    }
  }
  const channel = channelEnv(row.channel);
  if (channel && row.channel_token) {
    entries.push({ key: channel, value: row.channel_token, secret: true, updated_at: row.updated_at });
  }
  return entries;
}

function envEntries(row: Stored): StoredEnv[] {
  const seen = new Set<string>();
  const entries: StoredEnv[] = [];
  for (const entry of [...forcedEnv(row), ...row.env]) {
    if (seen.has(entry.key)) continue;
    seen.add(entry.key);
    entries.push(entry);
  }
  return entries.sort((left, right) => left.key.localeCompare(right.key));
}

function envKeys(row: Stored): string[] {
  return envEntries(row).map((entry) => entry.key);
}

/** Display name of a clone when the request does not name it, like the controller. */
function cloneDefaultName(source: string): string {
  const suffix = " copy";
  const base = source.trim().slice(0, 128 - suffix.length);
  return base ? `${base}${suffix}` : "copy";
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

  // Secrets are declared as Compose interpolations and read from the
  // `docker compose` process environment; no plaintext file on disk.
  const environment = [
    `      HERMES_PROFILE: ${row.id}`,
    `      HERMES_MODEL: ${row.model}`,
    `      HERMES_BASE_URL: ${row.base_url}`,
    channelLine,
  ];
  if (row.gate_token) {
    environment.push("      HERMES_GATE_PROFILE_TOKEN: ${HERMES_GATE_PROFILE_TOKEN}");
  }
  if (row.channel === "telegram") {
    environment.push("      TELEGRAM_BOT_TOKEN: ${TELEGRAM_BOT_TOKEN}");
  }
  if (row.channel === "discord") {
    environment.push("      DISCORD_BOT_TOKEN: ${DISCORD_BOT_TOKEN}");
  }

  const networkLines = ["    networks:", "      - fleett"];
  if (row.allow_lan) networkLines.push("      - fleett-lan");
  const networkDefs = ["networks:", "  fleett:", "    external: true"];
  if (row.allow_lan) networkDefs.push("  fleett-lan:", "    external: true");

  return [
    "# Managed by fleett. Edit through the API: hand edits are overwritten.",
    `name: fleett-${row.id}`,
    "",
    "services:",
    "  hermes:",
    `    image: ${IMAGE}`,
    "    restart: unless-stopped",
    '    user: "10000:10000"',
    "    environment:",
    ...environment,
    "    volumes:",
    ...mounts,
    ...networkLines,
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
    ...networkDefs,
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
    allow_lan: row.allow_lan,
    notes: row.notes,
    created_at: row.created_at,
    updated_at: row.updated_at,
    last_error: row.last_error,
    data_dir: `${FLEETT_ROOT}/instances/${row.id}/data`,
    env: envEntries(row).map((entry) =>
      entry.secret
        ? { key: entry.key, secret: true, updated_at: entry.updated_at }
        : { key: entry.key, secret: false, updated_at: entry.updated_at, value: entry.value },
    ),
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
    allow_lan: row.allow_lan,
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
      logLine(start + 420, "INFO", `creating container fleett-${row.id}`),
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
  allow_lan?: boolean;
  notes?: string;
  last_error?: string | null;
  gate_token?: string;
  env?: { key: string; value: string; secret?: boolean }[];
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
    channel_token: input.channel === "none" ? null : `seed-${input.id}-channel-token`,
    channel: input.channel,
    status: input.status,
    shares: input.shares,
    mem_limit_mb: input.mem_limit_mb ?? 1536,
    allow_lan: input.allow_lan ?? false,
    notes: input.notes ?? "",
    created_at: iso(created),
    updated_at: iso(created + 2 * HOUR),
    last_error: input.last_error ?? null,
    container_id: input.status === "created" ? null : containerId(),
    started_at: running && input.uptime_hours !== null ? SEEDED_AT - input.uptime_hours * HOUR : null,
    env: (input.env ?? []).map((entry) => ({
      key: entry.key,
      value: entry.value,
      secret: entry.secret ?? false,
      updated_at: iso(created + 2 * HOUR),
    })),
    fail_next: input.id === "workshop",
    logs: [],
  };
  row.logs = makeLogs(row);
  return row;
}

// ---------------------------------------------------------------------------
// instance files and history (docs/FILES.md, docs/GIT.md)
//
// A small in-memory tree per instance plus a one-commit-per-save history. It
// mirrors the controller closely enough to exercise every screen state: etags,
// rendered files, read-only extensions, drift and revert.

const EDITABLE_EXTENSIONS = ["md", "yaml", "yml", "json", "txt", "toml"];
const MAX_CONTENT_BYTES = 512 * 1024;
const RENDERED_NOTE =
  "this file is rendered by fleett from the instance configuration; a manual edit is overwritten on the next PATCH, so change the instance instead";

interface MockFile {
  content: string;
  mtime: number;
  rendered: boolean;
}

interface MockFs {
  files: Map<string, MockFile>;
  dirs: Set<string>;
}

interface MockCommit {
  info: CommitInfo;
  /** Repository relative path -> content, as of that commit. */
  files: Record<string, string>;
}

interface MockRepo {
  commits: MockCommit[];
}

interface MockAuthor {
  name: string;
  email: string;
}

const filesystems = new Map<string, MockFs>();
const repositories = new Map<string, MockRepo>();

let auditSeq = 1000;
let commitSeq = 0;

function recordAudit(
  identity: Identity,
  action: string,
  target: string,
  details?: unknown,
): void {
  auditSeq += 1;
  audit.unshift({
    id: auditSeq,
    at: nowIso(),
    actor: identity.actor,
    role: identity.role,
    method: identity.method,
    action,
    target,
    ip: identity.method === "session" ? "10.1.20.5" : null,
    details,
  });
}

const textEncoder = new TextEncoder();

function byteLength(text: string): number {
  return textEncoder.encode(text).length;
}

function isoSecond(ms: number): string {
  return new Date(ms).toISOString().replace(/\.\d{3}Z$/, "Z");
}

/** 64 lowercase hex characters, like the SHA-256 the controller returns. */
function mockEtag(text: string): string {
  const hash32 = (seed: number): number => {
    let hash = 0x811c9dc5 ^ seed;
    for (let index = 0; index < text.length; index += 1) {
      hash ^= text.charCodeAt(index);
      hash = Math.imul(hash, 0x01000193);
    }
    return hash >>> 0;
  };
  let out = "";
  for (let seed = 0; seed < 8; seed += 1) {
    out += hash32(seed * 0x9e3779b9).toString(16).padStart(8, "0");
  }
  return out;
}

function parentPath(path: string): string {
  if (path === "/") return "/";
  const index = path.lastIndexOf("/");
  return index <= 0 ? "/" : path.slice(0, index);
}

function baseName(path: string): string {
  if (path === "/") return "/";
  return path.slice(path.lastIndexOf("/") + 1);
}

function normalizeVirtual(raw: string | null | undefined): string {
  const value = (raw ?? "/").trim();
  if (!value) return "/";
  if (!value.startsWith("/")) {
    fail(400, "invalid_path", "paths are virtual and must start with \"/\"");
  }
  if (value.includes("\0")) {
    fail(400, "invalid_path", "a path cannot contain a NUL byte");
  }
  if (value.split("/").some((segment) => segment === "..")) {
    fail(400, "invalid_path", "\"..\" is not allowed in a virtual path");
  }
  const segments = value.split("/").filter((segment) => segment !== "" && segment !== ".");
  return `/${segments.join("/")}`;
}

function repoPathFor(virtualPath: string): string {
  return virtualPath === "/" ? "data" : `data${virtualPath}`;
}

function virtualForRepo(repoPath: string): string {
  return repoPath.startsWith("data/") ? repoPath.slice(4) : "/";
}

function resolveRepoPath(raw: string): string {
  const value = raw.trim();
  if (value.startsWith("data/")) return value;
  return repoPathFor(normalizeVirtual(value));
}

function isEditablePath(path: string): boolean {
  const name = baseName(path);
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return false;
  return EDITABLE_EXTENSIONS.includes(name.slice(dot + 1).toLowerCase());
}

function isRenderedPath(path: string): boolean {
  return path === "/config.yaml" || path === "/healthcheck.py";
}

/** Mirrors `history::is_tracked`: the editor allowlist minus runtime files. */
function isTrackedRepoPath(repoPath: string): boolean {
  if (!repoPath.startsWith("data/")) return false;
  const relative = repoPath.slice(5);
  const segments = relative.split("/");
  if (segments.some((segment) => ["logs", "sessions", "node_modules", "__pycache__"].includes(segment))) {
    return false;
  }
  const name = segments[segments.length - 1] ?? "";
  if (name === "healthcheck.py" || name === "secrets.env") return false;
  if (name.includes(".bak-")) return false;
  const virtual = `/${relative}`;
  if (virtual === "/SOUL.md" || virtual === "/config.yaml") return true;
  return isEditablePath(virtual);
}

function createFs(): MockFs {
  return { files: new Map(), dirs: new Set(["/"]) };
}

function ensureDir(fs: MockFs, path: string): void {
  let current = path;
  for (;;) {
    fs.dirs.add(current);
    if (current === "/") break;
    current = parentPath(current);
  }
}

function putFile(fs: MockFs, path: string, content: string, mtime: number, rendered = false): void {
  ensureDir(fs, parentPath(path));
  fs.files.set(path, { content, mtime, rendered });
}

function directoryMtime(fs: MockFs, path: string): number {
  let newest = SEEDED_AT - 30 * DAY;
  for (const [filePath, file] of fs.files) {
    if (parentPath(filePath) === path) newest = Math.max(newest, file.mtime);
  }
  for (const dir of fs.dirs) {
    if (dir !== path && parentPath(dir) === path) newest = Math.max(newest, directoryMtime(fs, dir));
  }
  return newest;
}

function listDirectory(fs: MockFs, directory: string): FileEntry[] {
  const entries: FileEntry[] = [];
  for (const candidate of fs.dirs) {
    if (candidate === "/" || parentPath(candidate) !== directory) continue;
    entries.push({
      name: baseName(candidate),
      path: candidate,
      type: "dir",
      size: 4096,
      mtime: isoSecond(directoryMtime(fs, candidate)),
      editable: false,
      rendered: false,
    });
  }
  for (const [filePath, file] of fs.files) {
    if (parentPath(filePath) !== directory) continue;
    entries.push({
      name: baseName(filePath),
      path: filePath,
      type: "file",
      size: byteLength(file.content),
      mtime: isoSecond(file.mtime),
      editable: isEditablePath(filePath) && !file.rendered,
      rendered: file.rendered,
    });
  }
  return entries.sort((left, right) => {
    if (left.type !== right.type) return left.type === "dir" ? -1 : 1;
    return left.name.toLowerCase().localeCompare(right.name.toLowerCase());
  });
}

function trackedSnapshot(fs: MockFs): Record<string, string> {
  const snapshot: Record<string, string> = {};
  for (const [path, file] of fs.files) {
    const repoPath = repoPathFor(path);
    if (!isTrackedRepoPath(repoPath)) continue;
    snapshot[repoPath] = file.content;
  }
  return snapshot;
}

function diffPaths(before: Record<string, string>, after: Record<string, string>): string[] {
  const paths = new Set([...Object.keys(before), ...Object.keys(after)]);
  return [...paths].filter((path) => before[path] !== after[path]).sort();
}

function authorFor(identity: Identity): MockAuthor {
  const name = identity.user?.name?.trim();
  const email = identity.user?.email?.trim();
  if (name || email) {
    return { name: name || email || identity.actor, email: email || `${identity.actor}@fleett.local` };
  }
  return { name: identity.actor, email: `${identity.actor}@fleett.local` };
}

function commitMessage(kind: "ui" | "sync", paths: string[]): string {
  const head = paths[0] ?? "";
  const extra = paths.length > 1 ? ` (+${paths.length - 1} more)` : "";
  return `${kind}: ${head}${extra}`;
}

function commitId(seed: string): string {
  commitSeq += 1;
  return mockEtag(`${seed}:${commitSeq}`).slice(0, 40);
}

function repoFor(id: string): MockRepo {
  const existing = repositories.get(id);
  if (existing) return existing;
  fsFor(id);
  const created: MockRepo = { commits: [] };
  repositories.set(id, created);
  return created;
}

function recordCommit(
  id: string,
  fs: MockFs,
  author: MockAuthor,
  kind: "ui" | "sync",
): CommitInfo | null {
  const repo = repoFor(id);
  const snapshot = trackedSnapshot(fs);
  const head = repo.commits[0];
  const changed = diffPaths(head?.files ?? {}, snapshot);
  if (changed.length === 0) return null;

  const message = commitMessage(kind, changed);
  const at = isoSecond(Date.now());
  const fullId = commitId(`${id}:${message}:${at}`);
  const info: CommitInfo = {
    id: fullId,
    short: fullId.slice(0, 7),
    author: author.name,
    email: author.email,
    at,
    message,
    paths: changed,
  };
  repo.commits.unshift({ info, files: snapshot });
  return info;
}

/** Record one save/delete in the instance history, like `history::record_ui_write`. */
function recordUiWrite(id: string, fs: MockFs, identity: Identity, virtualPath: string): void {
  const repoPath = repoPathFor(virtualPath);
  if (!isTrackedRepoPath(repoPath)) return;
  recordCommit(id, fs, authorFor(identity), "ui");
}

function backupStamp(): string {
  const now = new Date();
  const pad = (value: number) => String(value).padStart(2, "0");
  return (
    `${now.getUTCFullYear()}${pad(now.getUTCMonth() + 1)}${pad(now.getUTCDate())}` +
    `-${pad(now.getUTCHours())}${pad(now.getUTCMinutes())}${pad(now.getUTCSeconds())}`
  );
}

function nextBackupPath(fs: MockFs, path: string): string {
  const base = `${path}.bak-${backupStamp()}`;
  let candidate = base;
  let index = 1;
  while (fs.files.has(candidate)) {
    candidate = `${base}-${index}`;
    index += 1;
  }
  return candidate;
}

function resolveCommit(commits: MockCommit[], revision: string | undefined): MockCommit | undefined {
  if (!revision || !revision.trim()) return commits[0];
  const value = revision.trim();
  const head = /^HEAD(?:~(\d+))?$/.exec(value);
  if (head) return commits[Number(head[1] ?? 0)];
  return commits.find((commit) => commit.info.id === value || commit.info.id.startsWith(value));
}

function buildDiff(before: Record<string, string>, after: Record<string, string>): string {
  const paths = [...new Set([...Object.keys(before), ...Object.keys(after)])].sort();
  return paths.map((path) => toPatch(path, before[path] ?? "", after[path] ?? "")).join("");
}

function copyCommit(info: CommitInfo): CommitInfo {
  return { ...info, paths: [...info.paths] };
}

// --- seeded content -------------------------------------------------------

function templateLine(template: string): string {
  if (template === "family") return "the family assistant";
  if (template === "ops") return "an operations assistant";
  if (template === "sandbox") return "a disposable sandbox assistant";
  return "a patient research assistant";
}

function soulText(row: Stored, withBoundaries = true): string {
  const lines = [
    "# SOUL",
    "",
    `You are ${row.name}, ${templateLine(row.template)}.`,
    "",
    "## Voice",
    "- Answer in the user's language, French or English.",
    "- Short paragraphs. No filler, no flattery.",
    '- Say "I do not know" before guessing.',
    "",
    "## Scope",
    row.channel === "none"
      ? "- Web search, memory and the files in this home. No shell, no Docker."
      : `- Web search, memory and the ${row.channel} channel. No shell, no Docker.`,
    "- Read shared folders read-only unless the share is mounted rw.",
  ];
  if (withBoundaries) {
    lines.push(
      "",
      "## Boundaries",
      "- Never echo a token, a key or a private URL.",
      "- Ask before writing outside /opt/data.",
    );
  }
  return `${lines.join("\n")}\n`;
}

function configText(row: Stored): string {
  const channel =
    row.channel === "none"
      ? ["channel:", "  type: none"]
      : [
          "channel:",
          `  type: ${row.channel}`,
          `  token_env: ${row.channel === "telegram" ? "TELEGRAM_BOT_TOKEN" : "DISCORD_BOT_TOKEN"}`,
        ];
  return [
    "# Rendered by fleett from the instance configuration.",
    "# A manual edit is overwritten on the next PATCH: change the instance instead.",
    `profile: ${row.id}`,
    `model: ${row.model}`,
    `base_url: ${row.base_url}`,
    ...channel,
    "memory:",
    "  backend: sqlite",
    "  path: /opt/data/state.db",
    "tools:",
    "  - web.search",
    "  - memory.write",
    "  - file.read",
    "logging:",
    "  level: info",
    "",
  ].join("\n");
}

const HEALTHCHECK_SCRIPT = [
  "#!/usr/bin/env python3",
  '"""Probe used by the container healthcheck. Rendered by fleett."""',
  "import json",
  "import socket",
  "import sys",
  "",
  'SOCKET = "/opt/data/gateway.sock"',
  "",
  "def main() -> int:",
  "    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)",
  "    client.settimeout(2)",
  "    try:",
  "        client.connect(SOCKET)",
  '        client.sendall(b"identify\\n")',
  '        reply = client.recv(256).decode("utf-8", "replace")',
  "    except OSError as error:",
  '        print(json.dumps({"ok": False, "error": str(error)}))',
  "        return 1",
  "    finally:",
  "        client.close()",
  '    print(json.dumps({"ok": reply.startswith("hermes")}))',
  "    return 0",
  "",
  'if __name__ == "__main__":',
  "    sys.exit(main())",
  "",
].join("\n");

const NOTE_V1 = ["# Weekly summary", "", "Run every Friday at 17:00.", ""].join("\n");

const NOTE_V2 = [
  "# Weekly summary",
  "",
  "Run every Friday at 17:00.",
  "",
  "- Pull the week's sessions from memory.",
  "- Group by project, not by day.",
  "",
].join("\n");

const NOTE_V3 = [
  "# Weekly summary",
  "",
  "Run every Friday at 17:00.",
  "",
  "- Pull the week's sessions from memory.",
  "- Group by project, not by day.",
  "- Send one message per project, with the links.",
  "",
].join("\n");

function gatewayLog(row: Stored): string {
  const base = SEEDED_AT - 20 * 60_000;
  return (
    [
      logLine(base, "INFO", "hermes 0.22.2 starting"),
      logLine(base + 180, "INFO", `profile=${row.id} model=${row.model}`),
      logLine(base + 420, "INFO", "gateway listening on 127.0.0.1:9119"),
      logLine(base + 900, "INFO", "scheduler started (cti every 15m)"),
    ].join("\n") + "\n"
  );
}

function pushSeedCommit(
  repo: MockRepo,
  row: Stored,
  files: Record<string, string>,
  kind: "ui" | "sync",
  author: string,
  email: string,
  atMs: number,
): void {
  const changed = diffPaths(repo.commits[0]?.files ?? {}, files);
  const message = commitMessage(kind, changed);
  const fullId = commitId(`${row.id}:${message}:${atMs}`);
  repo.commits.unshift({
    info: {
      id: fullId,
      short: fullId.slice(0, 7),
      author,
      email,
      at: isoSecond(atMs),
      message,
      paths: changed,
    },
    files,
  });
}

function seedInstanceData(row: Stored, history: "demo" | "fresh"): void {
  const fs = createFs();
  const base = SEEDED_AT - 3 * DAY;
  putFile(fs, "/SOUL.md", soulText(row), SEEDED_AT - 2 * DAY);
  putFile(fs, "/config.yaml", configText(row), base, true);
  putFile(fs, "/healthcheck.py", HEALTHCHECK_SCRIPT, base, true);
  putFile(fs, "/skills/note.md", NOTE_V3, SEEDED_AT - 30 * 60_000);
  putFile(fs, "/logs/gateway.log", gatewayLog(row), SEEDED_AT - 15 * 60_000);
  filesystems.set(row.id, fs);

  const repo: MockRepo = { commits: [] };
  repositories.set(row.id, repo);

  if (history === "demo") {
    const first = {
      ...trackedSnapshot(fs),
      "data/SOUL.md": soulText(row, false),
      "data/skills/note.md": NOTE_V1,
    };
    const second = { ...first, "data/SOUL.md": soulText(row) };
    const third = { ...second, "data/skills/note.md": NOTE_V2 };
    pushSeedCommit(repo, row, first, "sync", "fleett", "fleett@localhost", base);
    pushSeedCommit(repo, row, second, "ui", "Cleeryy", "cleeryy@example.com", base + DAY);
    pushSeedCommit(
      repo,
      row,
      third,
      "ui",
      "Cleeryy",
      "cleeryy@example.com",
      SEEDED_AT - 40 * 60_000,
    );
    return;
  }

  const snapshot = trackedSnapshot(fs);
  pushSeedCommit(repo, row, snapshot, "sync", "fleett", "fleett@localhost", base);
}

function fsFor(id: string): MockFs {
  const existing = filesystems.get(id);
  if (existing) return existing;
  const row = findRow(id);
  seedInstanceData(row, "fresh");
  return filesystems.get(id) as MockFs;
}

function cloneInstanceData(sourceId: string, targetId: string): void {
  const source = fsFor(sourceId);
  const copy = createFs();
  for (const dir of source.dirs) copy.dirs.add(dir);
  for (const [path, file] of source.files) copy.files.set(path, { ...file });
  filesystems.set(targetId, copy);
  repositories.set(targetId, { commits: [] });
  recordCommit(targetId, copy, { name: "fleett", email: "fleett@localhost" }, "sync");
}

export function resetMock(): void {
  const telegramEnv = [
    {
      key: "TELEGRAM_ALLOWED_USERS",
      description:
        "Comma-separated Telegram user ids allowed to talk to the agent; without it every message is denied.",
    },
    {
      key: "GATEWAY_ALLOW_ALL_USERS",
      description: "Set to true to accept every user on the platform, with no allowlist. Use with care.",
    },
  ];
  const discordEnv = [
    {
      key: "DISCORD_ALLOWED_USERS",
      description: "Comma-separated Discord user ids allowed to talk to the agent.",
    },
    {
      key: "DISCORD_ALLOWED_ROLES",
      description: "Comma-separated Discord role ids allowed to talk to the agent.",
    },
    {
      key: "DISCORD_ALLOWED_CHANNELS",
      description: "Comma-separated Discord channel ids where the agent answers.",
    },
    {
      key: "DISCORD_ALLOW_ALL_USERS",
      description: "Set to true to accept every Discord user, with no allowlist. Use with care.",
    },
  ];

  templates = [
    {
      id: "guest",
      name: "Guest",
      description: "Web and memory only. No infrastructure access.",
      default_model: "deepseek-v4.1-flash",
      channel: "telegram",
      env: telegramEnv,
    },
    {
      id: "guest-discord",
      name: "Guest (Discord)",
      description: "Same restrictions as guest, connected to Discord.",
      default_model: "deepseek-v4.1-flash",
      channel: "discord",
      env: discordEnv,
    },
    {
      id: "family",
      name: "Family",
      description: "Everyday assistant with a shared photo archive.",
      default_model: "deepseek-v4.1-flash",
      channel: "discord",
      env: discordEnv,
    },
    {
      id: "ops",
      name: "Ops",
      description: "Homelab diagnostics. Read access to operations shares.",
      default_model: "muse-spark-1.3-contributor",
      channel: "none",
      env: [],
    },
    {
      id: "sandbox",
      name: "Sandbox",
      description: "Free execution, isolated network, disposable data.",
      default_model: "deepseek-v4.1-flash",
      channel: "none",
      env: [],
    },
  ];

  shares = [
    {
      name: "findings",
      path: `${FLEETT_ROOT}/shares/findings`,
      instances: [
        { id: "alice", mode: "rw" },
        { id: "ops", mode: "ro" },
      ],
    },
    {
      name: "media",
      path: `${FLEETT_ROOT}/shares/media`,
      instances: [{ id: "margaret", mode: "ro" }],
    },
    {
      name: "runbooks",
      path: `${FLEETT_ROOT}/shares/runbooks`,
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
      env: [{ key: "TELEGRAM_ALLOWED_USERS", value: "81459823" }],
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
      env: [
        { key: "DISCORD_ALLOWED_USERS", value: "310472918374" },
        { key: "DISCORD_ALLOWED_ROLES", value: "987654321098" },
      ],
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
      allow_lan: true,
      notes: "Homelab diagnostics. No Docker socket. Reaches the management VLAN.",
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

  tokens = [
    {
      id: "tok_seed_ci",
      name: "ci-nightly",
      role: "viewer",
      created_at: iso(SEEDED_AT - 9 * DAY),
      revoked_at: null,
    },
  ];

  audit = [
    {
      id: 6,
      at: iso(SEEDED_AT - 35 * 60_000),
      actor: "cleeryy@example.com",
      role: "admin",
      method: "session",
      action: "instance.restart",
      target: "alice",
      ip: "10.1.20.5",
    },
    {
      id: 5,
      at: iso(SEEDED_AT - 2 * HOUR),
      actor: "cleeryy@example.com",
      role: "admin",
      method: "session",
      action: "share.attach",
      target: "findings",
      ip: "10.1.20.5",
      details: { instance: "alice", mode: "rw" },
    },
    {
      id: 4,
      at: iso(SEEDED_AT - 5 * HOUR),
      actor: "fleett-cli",
      role: "admin",
      method: "bearer",
      action: "instance.patch",
      target: "school",
      ip: "203.0.113.24",
    },
    {
      id: 3,
      at: iso(SEEDED_AT - 9 * HOUR),
      actor: "cleeryy@example.com",
      role: "admin",
      method: "session",
      action: "share.create",
      target: "runbooks",
      ip: "10.1.20.5",
    },
    {
      id: 2,
      at: iso(SEEDED_AT - DAY),
      actor: "cleeryy@example.com",
      role: "admin",
      method: "session",
      action: "token.create",
      target: "tok_seed_ci",
      ip: "10.1.20.5",
      details: { name: "ci-nightly", role: "viewer" },
    },
    {
      id: 1,
      at: iso(SEEDED_AT - 2 * DAY),
      actor: "cleeryy@example.com",
      role: "admin",
      method: "session",
      action: "instance.create",
      target: "alice",
    },
  ];

  filesystems.clear();
  repositories.clear();
  for (const row of rows) seedInstanceData(row, "demo");
}

resetMock();

// ---------------------------------------------------------------------------
// adapter

const ENV_KEY_RE = /^[A-Z_][A-Z0-9_]{0,63}$/;
const RESERVED_ENV = new Set(["HERMES_UID", "HERMES_GID", "PATH", "HOME"]);

/** Same grammar and error codes as the controller (`invalid_env`, `reserved_env`). */
function assertEnvInputs(
  vars: EnvInput[],
  options: { requireValue: boolean; requireSecret: boolean },
): void {
  const seen = new Set<string>();
  for (const variable of vars) {
    if (RESERVED_ENV.has(variable.key) || variable.key.startsWith("FLEETT_")) {
      fail(400, "reserved_env", `environment variable "${variable.key}" is reserved`);
    }
    if (!ENV_KEY_RE.test(variable.key)) {
      fail(
        400,
        "invalid_env",
        `environment variable "${variable.key}" must match ^[A-Z_][A-Z0-9_]{0,63}$`,
      );
    }
    if (seen.has(variable.key)) {
      fail(400, "duplicate_env", `environment variable "${variable.key}" is listed twice`);
    }
    seen.add(variable.key);
    const value = variable.value;
    if (value !== undefined && value !== null) {
      if (value.length > 8192) {
        fail(400, "invalid_env", `environment variable "${variable.key}" value is longer than 8 KiB`);
      }
      if (/[\n\r\0]/.test(value)) {
        fail(400, "invalid_env", `environment variable "${variable.key}" value cannot span lines`);
      }
    } else if (options.requireValue) {
      fail(400, "invalid_env", `environment variable "${variable.key}" requires a value`);
    }
    if (options.requireSecret && variable.secret === undefined) {
      fail(
        400,
        "invalid_env",
        `environment variable "${variable.key}" must say whether it is a secret`,
      );
    }
  }
}

/** Keys a supplied gate or channel token owns: the token value always wins. */
function forcedKeys(row: Stored): Set<string> {
  const keys = new Set<string>();
  if (row.gate_token) for (const key of gateKeys(row)) keys.add(key);
  const channel = channelEnv(row.channel);
  if (channel && row.channel_token) keys.add(channel);
  return keys;
}

/** A write replaces the environment, so remember when the container restarts. */
function applyEnvChange(row: Stored): void {
  if (!row.container_id) row.container_id = containerId();
  row.status = "running";
  row.started_at = Date.now();
  row.updated_at = nowIso();
  row.last_error = null;
  row.logs.push(logLine(Date.now(), "INFO", "environment changed, container recreated"));
}

function validateChannel(channel: { type: ChannelType; token?: string }, previous: ChannelType): void {
  if (channel.type === "none") return;
  if (!channel.token && channel.type !== previous) {
    fail(400, "invalid_input", `A token is required when switching the channel to ${channel.type}.`);
  }
}

export const mockApi: FleetApi = {
  async health(): Promise<Health> {
    await delay(60);
    return { ok: true, version: "0.2.0", driver: "mock", docker: { ok: true } };
  },

  async authConfig(): Promise<AuthConfig> {
    await delay(40);
    const oidc = mockAuthMode === "oidc" || mockAuthMode === "both";
    const bearer = mockAuthMode === "bearer" || mockAuthMode === "both";
    return {
      mode: mockAuthMode,
      oidc: { enabled: oidc, login_url: "/auth/login" },
      bearer: { enabled: bearer },
    };
  },

  async me(): Promise<Identity> {
    await delay(120);
    const identity = requireAuth();
    return { ...identity, user: identity.user ? { ...identity.user } : undefined };
  },

  async signInWithOidc(_loginUrl: string): Promise<void> {
    await delay(420);
    if (mockAuthMode === "bearer") {
      fail(404, "not_found", "OIDC is not enabled");
    }
    mockSession = {
      ...ADMIN_SESSION,
      user: ADMIN_SESSION.user ? { ...ADMIN_SESSION.user } : undefined,
    };
  },

  async logout(): Promise<void> {
    await delay(120);
    mockSession = null;
    tokenStore.clear();
  },

  async checkAuth(): Promise<void> {
    await delay(220);
    requireAuth();
  },

  async listTemplates(): Promise<Template[]> {
    requireAuth();
    await delay();
    return templates.map((template) => ({
      ...template,
      env: template.env.map((suggestion) => ({ ...suggestion })),
    }));
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

  async putInstanceEnv(id: string, env: EnvInput[]): Promise<InstanceDetail> {
    requireWrite();
    await delay(340);
    const row = findRow(id);
    assertEnvInputs(env, { requireValue: false, requireSecret: true });

    const existing = envEntries(row);
    const resolved: StoredEnv[] = env.map((variable) => {
      const current = existing.find((entry) => entry.key === variable.key);
      if (variable.value !== undefined && variable.value !== null) {
        return {
          key: variable.key,
          value: variable.value,
          secret: variable.secret === true,
          updated_at: nowIso(),
        };
      }
      if (current?.secret && variable.secret === true) {
        // A secret never left the controller, so sending it back without a
        // value is how the console keeps it.
        return { ...current, updated_at: nowIso() };
      }
      if (!current) {
        fail(400, "invalid_env", `environment variable "${variable.key}" is new: a value is required`);
      }
      fail(400, "invalid_env", `environment variable "${variable.key}" is not a secret: a value is required`);
    });

    if (resolved.some((entry) => entry.secret)) assertSecretsKey("x");

    // A full replace drops every key it leaves out, gate and channel tokens
    // included.
    const requested = new Set(env.map((variable) => variable.key));
    if (row.gate_token && gateKeys(row).some((key) => !requested.has(key))) {
      row.gate_token = null;
    }
    const channelKey = channelEnv(row.channel);
    if (channelKey && row.channel_token && !requested.has(channelKey)) {
      row.channel_token = null;
    }

    row.env = resolved.filter((entry) => !forcedKeys(row).has(entry.key));
    applyEnvChange(row);
    return toDetail(row);
  },

  async deleteInstanceEnv(id: string, key: string): Promise<void> {
    requireWrite();
    await delay(280);
    const row = findRow(id);
    if (row.gate_token && gateKeys(row).includes(key)) {
      row.gate_token = null;
      applyEnvChange(row);
      return;
    }
    if (row.channel_token && channelEnv(row.channel) === key) {
      row.channel_token = null;
      applyEnvChange(row);
      return;
    }
    if (!row.env.some((entry) => entry.key === key)) {
      fail(404, "not_found", `instance "${id}" has no environment variable "${key}"`);
    }
    row.env = row.env.filter((entry) => entry.key !== key);
    applyEnvChange(row);
  },

  async createInstance(input: CreateInstanceInput): Promise<InstanceDetail> {
    requireWrite();
    await delay(320);

    const id = input.id.trim();
    if (!isValidSlug(id)) {
      fail(400, "invalid_input", "Id must be 2-32 characters: lowercase letters, digits, hyphens.");
    }
    if (rows.some((row) => row.id === id)) {
      fail(409, "instance_exists", `Instance "${id}" already exists.`);
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
    assertEnvInputs(input.env ?? [], { requireValue: true, requireSecret: true });
    assertSecretsKey(input.gate_token);
    assertSecretsKey(input.channel.type === "none" ? undefined : input.channel.token);
    assertSecretsKey((input.env ?? []).some((variable) => variable.secret === true) ? "env" : undefined);
    assertEgress(input.allow_lan === true);

    const created = Date.now();
    const gateToken = input.gate_token?.trim() ? input.gate_token.trim() : null;
    const channelToken =
      input.channel.type === "none" ? null : (input.channel.token?.trim() ?? null);
    const row: Stored = {
      id,
      name: input.name.trim(),
      template: input.template,
      model: input.model.trim(),
      base_url: input.base_url.trim(),
      api_key_env: input.api_key_env.trim(),
      gate_token: gateToken,
      channel_token: channelToken,
      channel: input.channel.type,
      status: "running",
      shares: input.shares.map((share) => ({ name: share.name, mode: share.mode })),
      mem_limit_mb: input.mem_limit_mb,
      allow_lan: input.allow_lan === true,
      notes: input.notes.trim(),
      created_at: iso(created),
      updated_at: iso(created),
      last_error: null,
      container_id: containerId(),
      started_at: created,
      env: [],
      fail_next: false,
      logs: [],
    };
    // Gate and platform tokens win over an entry with the same key, like the
    // controller: the token is written under that name.
    const owned = forcedKeys(row);
    row.env = (input.env ?? [])
      .filter((variable) => !owned.has(variable.key))
      .map((variable) => ({
        key: variable.key,
        value: variable.value ?? "",
        secret: variable.secret === true,
        updated_at: iso(created),
      }));
    row.logs = makeLogs(row);
    rows.unshift(row);
    for (const share of row.shares) attachToShare(share.name, id, share.mode);
    seedInstanceData(row, "fresh");
    return toDetail(row);
  },

  async cloneInstance(id: string, input: CloneInstanceInput): Promise<InstanceDetail> {
    requireWrite();
    await delay(420);
    const source = findRow(id);

    const newId = input.new_id.trim();
    if (!isValidSlug(newId)) {
      fail(400, "invalid_id", "Id must be 2-32 characters: lowercase letters, digits, hyphens.");
    }
    if (newId === id) {
      fail(400, "invalid_id", "The new id must differ from the source id.");
    }
    if (rows.some((row) => row.id === newId)) {
      fail(409, "instance_exists", `Instance "${newId}" already exists.`);
    }
    if (input.name !== undefined && !input.name.trim()) {
      fail(400, "invalid_name", "Name must not be empty.");
    }
    if (input.model !== undefined && !input.model.trim()) {
      fail(400, "invalid_model", "Model must not be empty.");
    }
    assertSecretsKey(input.gate_token);
    assertSecretsKey(input.channel_token);

    // Tokens are never inherited, exactly like the controller: every token the
    // source holds has to be replaced before anything is written.
    const sourceKeys = envKeys(source);
    const sourceChannelEnv = channelEnv(source.channel);
    const gateToken = input.gate_token?.trim() ?? "";
    const channelToken = input.channel_token?.trim() ?? "";
    const missing: string[] = [];
    if (!gateToken) {
      missing.push(...gateKeys(source).filter((key) => sourceKeys.includes(key)));
    }
    if (!channelToken && sourceChannelEnv && sourceKeys.includes(sourceChannelEnv)) {
      missing.push(sourceChannelEnv);
    }
    if (missing.length > 0) {
      fail(
        400,
        "clone_token_required",
        `Clones never inherit secrets. Supply a replacement for ${missing.join(", ")}.`,
        { source: id, missing },
      );
    }

    const copyData = input.copy_data !== false;
    const created = Date.now();
    const inherited =
      input.shares_inherit === false
        ? []
        : source.shares.filter((ref) => shares.some((share) => share.name === ref.name));

    const row: Stored = {
      id: newId,
      name: input.name?.trim() || cloneDefaultName(source.name),
      template: source.template,
      model: input.model?.trim() || source.model,
      base_url: source.base_url,
      api_key_env: source.api_key_env,
      // Only the replacement token: the source secret is never copied.
      gate_token: gateToken || null,
      channel_token: source.channel === "none" ? null : channelToken || null,
      channel: source.channel,
      status: "running",
      shares: inherited.map((ref) => ({ ...ref })),
      mem_limit_mb: source.mem_limit_mb,
      allow_lan: source.allow_lan,
      notes: source.notes,
      created_at: iso(created),
      updated_at: iso(created),
      last_error: null,
      container_id: containerId(),
      started_at: created,
      // Non-secret variables follow the clone; encrypted values never do.
      env: source.env
        .filter((entry) => !entry.secret)
        .map((entry) => ({ key: entry.key, value: entry.value, secret: false, updated_at: iso(created) })),
      fail_next: false,
      logs: [],
    };
    row.logs = makeLogs(row);
    row.logs.unshift(
      logLine(
        created,
        "INFO",
        `clone of "${source.id}" (${copyData ? "data copied" : "fresh data"}, ` +
          `${row.shares.length} share${row.shares.length === 1 ? "" : "s"} inherited)`,
      ),
    );
    rows.unshift(row);
    for (const share of row.shares) attachToShare(share.name, newId, share.mode);
    if (copyData) {
      cloneInstanceData(id, newId);
    } else {
      seedInstanceData(row, "fresh");
    }
    return toDetail(row);
  },

  async patchInstance(id: string, input: PatchInstanceInput): Promise<InstanceDetail> {
    requireWrite();
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
    if (input.env) assertEnvInputs(input.env, { requireValue: false, requireSecret: false });
    assertSecretsKey(input.gate_token);
    // Only a transition to restricted needs the host firewall; a stale
    // allow_lan=false in a full form save must not block unrelated edits.
    if (input.allow_lan === false && row.allow_lan) assertEgress(false);

    let nextGate = input.gate_token?.trim() || row.gate_token;
    const nextChannel = input.channel ? input.channel.type : row.channel;
    let nextChannelToken = row.channel_token;
    if (input.channel) {
      nextChannelToken =
        input.channel.type === "none" ? null : (input.channel.token?.trim() || row.channel_token);
    }

    let nextEnv = row.env.map((entry) => ({ ...entry }));
    if (input.env) {
      const entries = new Map(nextEnv.map((entry) => [entry.key, entry]));
      const owned = forcedKeys({
        ...row,
        gate_token: nextGate,
        channel: nextChannel,
        channel_token: nextChannelToken,
      });
      const channelKey = channelEnv(nextChannel);
      for (const variable of input.env) {
        if (variable.value === undefined || variable.value === null) {
          if (owned.has(variable.key)) {
            if (gateKeys(row).includes(variable.key)) nextGate = null;
            else if (variable.key === channelKey) nextChannelToken = null;
          } else {
            entries.delete(variable.key);
          }
          continue;
        }
        if (owned.has(variable.key)) {
          if (variable.key === channelKey) nextChannelToken = variable.value;
          else nextGate = variable.value;
          continue;
        }
        const current = entries.get(variable.key);
        entries.set(variable.key, {
          key: variable.key,
          value: variable.value,
          secret: variable.secret ?? current?.secret ?? false,
          updated_at: nowIso(),
        });
      }
      nextEnv = [...entries.values()];
    }

    // Refuse to leave a store that holds a secret when the host has no key,
    // before touching anything (mirrors `check_storable`).
    const prospective: Stored = {
      ...row,
      gate_token: nextGate,
      channel: nextChannel,
      channel_token: nextChannelToken,
      env: nextEnv,
    };
    if (envEntries(prospective).some((entry) => entry.secret)) assertSecretsKey("x");

    if (input.name !== undefined) row.name = input.name.trim();
    if (input.model !== undefined) row.model = input.model.trim();
    if (input.base_url !== undefined) row.base_url = input.base_url.trim();
    if (input.api_key_env !== undefined) row.api_key_env = input.api_key_env.trim();
    if (input.mem_limit_mb !== undefined) row.mem_limit_mb = input.mem_limit_mb;
    if (input.allow_lan !== undefined) row.allow_lan = input.allow_lan;
    if (input.notes !== undefined) row.notes = input.notes;
    row.gate_token = nextGate;
    row.channel = nextChannel;
    row.channel_token = nextChannelToken;
    row.env = nextEnv;

    if (input.shares) {
      const next = input.shares.map((share) => ({ name: share.name, mode: share.mode }));
      syncShareLinks(row.id, next);
      row.shares = next;
    }

    if (input.env) {
      applyEnvChange(row);
    } else {
      row.updated_at = nowIso();
    }
    row.last_error = row.status === "error" ? row.last_error : null;
    return toDetail(row);
  },

  async action(id, action): Promise<LifecycleResult> {
    requireWrite();
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
    requireWrite();
    await delay(340);
    const row = findRow(id);
    for (const share of row.shares) detachFromShare(share.name, id);
    rows = rows.filter((candidate) => candidate.id !== id);
    filesystems.delete(id);
    repositories.delete(id);
  },

  async logs(id, tail): Promise<string> {
    requireAuth();
    await delay(180);
    const row = findRow(id);
    const count = Number.isFinite(tail) && tail > 0 ? Math.min(tail, 5000) : 200;
    return row.logs.slice(-count).join("\n");
  },

  async listFiles(id: string, path: string): Promise<FileEntry[]> {
    requireAuth();
    await delay(140);
    const fs = fsFor(id);
    const target = normalizeVirtual(path);
    if (fs.files.has(target)) {
      fail(400, "not_a_directory", `"${target}" is a file`);
    }
    if (!fs.dirs.has(target)) {
      fail(404, "not_found", `"${target}" does not exist`);
    }
    return listDirectory(fs, target);
  },

  async readFile(id: string, path: string): Promise<FileContent> {
    requireAuth();
    await delay(120);
    const fs = fsFor(id);
    const target = normalizeVirtual(path);
    const file = fs.files.get(target);
    if (!file) {
      if (fs.dirs.has(target)) {
        fail(400, "not_a_file", `"${target}" is a directory`);
      }
      fail(404, "not_found", `"${target}" does not exist`);
    }
    return {
      path: target,
      content: file.content,
      etag: mockEtag(file.content),
      size: byteLength(file.content),
      mtime: isoSecond(file.mtime),
      editable: isEditablePath(target) && !file.rendered,
      rendered: file.rendered,
      ...(file.rendered ? { note: RENDERED_NOTE } : {}),
    };
  },

  async writeFile(id: string, input: WriteFileInput): Promise<WriteFileResult> {
    const identity = requireWrite();
    await delay(260);
    const row = findRow(id);
    const fs = fsFor(id);
    const target = normalizeVirtual(input.path);

    if (target === "/") {
      fail(400, "not_a_file", "the data directory itself cannot be edited");
    }
    const existing = fs.files.get(target);
    if (fs.dirs.has(target)) {
      fail(400, "not_a_file", `"${target}" is a directory`);
    }
    if (existing?.rendered || isRenderedPath(target)) {
      fail(409, "file_regenerated", `"${target}" is rendered by fleett and cannot be edited here`, {
        file: target,
        rendered: true,
        hint: RENDERED_NOTE,
      });
    }
    if (!isEditablePath(target)) {
      fail(
        409,
        "file_not_editable",
        `"${target}" is read-only: only ${EDITABLE_EXTENSIONS.map((ext) => `.${ext}`).join(", ")} files are editable`,
        { file: target, extensions: EDITABLE_EXTENSIONS },
      );
    }
    const size = byteLength(input.content);
    if (size > MAX_CONTENT_BYTES) {
      fail(413, "file_too_large", `the file is ${size} bytes, above the ${MAX_CONTENT_BYTES}-byte limit of the editor`, {
        size,
        limit: MAX_CONTENT_BYTES,
      });
    }
    if (input.content.includes("\0")) {
      fail(415, "binary_file", "the content must not contain a NUL byte");
    }
    const parent = parentPath(target);
    if (!fs.dirs.has(parent)) {
      fail(404, "not_found", `the folder "${parent}" does not exist`);
    }

    const current = existing ? mockEtag(existing.content) : null;
    const expected = (input.etag ?? "").trim();
    const matches = input.force
      ? current !== null
      : existing
        ? expected === current
        : expected === "";
    if (!matches) {
      fail(409, "etag_mismatch", "the file changed since it was loaded: reload it and retry", {
        current,
      });
    }

    let backup: string | undefined;
    if (existing) {
      backup = nextBackupPath(fs, target);
      fs.files.set(backup, { content: existing.content, mtime: Date.now(), rendered: false });
    }
    const writtenAt = Date.now();
    fs.files.set(target, { content: input.content, mtime: writtenAt, rendered: false });
    recordUiWrite(id, fs, identity, target);
    recordAudit(identity, "instance.file.write", row.id, {
      path: target,
      size,
      backup: backup ?? null,
      restart: input.restart === true,
    });

    const result: WriteFileResult = {
      path: target,
      etag: mockEtag(input.content),
      size,
      mtime: isoSecond(writtenAt),
      editable: true,
      rendered: false,
      restarted: false,
    };
    if (backup) result.backup = backup;

    if (input.restart) {
      row.status = "running";
      row.started_at = Date.now();
      row.updated_at = nowIso();
      row.last_error = null;
      row.logs.push(logLine(Date.now(), "INFO", `restarted after ${target} edit`));
      result.restarted = true;
      result.status = row.status;
    }
    return result;
  },

  async deleteFile(id: string, path: string): Promise<void> {
    const identity = requireWrite();
    await delay(200);
    const fs = fsFor(id);
    const target = normalizeVirtual(path);
    if (target === "/") {
      fail(400, "not_a_file", "the data directory itself cannot be deleted");
    }

    const file = fs.files.get(target);
    if (file) {
      if (file.rendered) {
        fail(409, "file_regenerated", `"${target}" is rendered by fleett and cannot be deleted`, {
          file: target,
          rendered: true,
          hint: RENDERED_NOTE,
        });
      }
      fs.files.delete(target);
      recordUiWrite(id, fs, identity, target);
      recordAudit(identity, "instance.file.delete", id, { path: target });
      return;
    }

    if (fs.dirs.has(target)) {
      const occupied =
        [...fs.files.keys()].some((candidate) => parentPath(candidate) === target) ||
        [...fs.dirs].some((candidate) => candidate !== target && parentPath(candidate) === target);
      if (occupied) {
        fail(409, "directory_not_empty", `"${target}" still holds entries: delete them first`);
      }
      fs.dirs.delete(target);
      recordAudit(identity, "instance.file.delete", id, { path: target });
      return;
    }

    fail(404, "not_found", `"${target}" does not exist`);
  },

  async mkdir(id: string, path: string): Promise<MkdirResult> {
    const identity = requireWrite();
    await delay(180);
    const fs = fsFor(id);
    const target = normalizeVirtual(path);
    if (target === "/") {
      fail(409, "already_exists", "the data directory already exists");
    }
    if (fs.dirs.has(target) || fs.files.has(target)) {
      fail(409, "already_exists", `"${target}" already exists`);
    }
    const parent = parentPath(target);
    if (!fs.dirs.has(parent)) {
      fail(404, "not_found", `the folder "${parent}" does not exist`);
    }
    ensureDir(fs, target);
    recordAudit(identity, "instance.file.mkdir", id, { path: target });
    return { path: target, type: "dir" };
  },

  async gitLog(id: string, options: GitLogOptions = {}): Promise<GitLog> {
    requireAuth();
    await delay(140);
    const repo = repoFor(id);
    const limit = options.limit ?? 50;
    if (!Number.isInteger(limit) || limit < 1 || limit > 500) {
      fail(400, "invalid_limit", "limit must be between 1 and 500");
    }
    let commits = repo.commits;
    if (options.path && options.path.trim()) {
      const wanted = resolveRepoPath(options.path);
      if (!isTrackedRepoPath(wanted)) {
        fail(400, "untracked_path", `"${options.path}" is not versioned`);
      }
      commits = commits.filter((commit) => commit.info.paths.includes(wanted));
    }
    return { commits: commits.slice(0, limit).map((commit) => copyCommit(commit.info)) };
  },

  async gitDiff(id: string, options: GitDiffOptions = {}): Promise<GitDiff> {
    requireAuth();
    await delay(160);
    const repo = repoFor(id);
    if (repo.commits.length === 0) {
      return { from: "", to: "", diff: "" };
    }
    const to = resolveCommit(repo.commits, options.to);
    if (!to) {
      fail(404, "not_found", `commit "${options.to}" does not exist`);
    }
    let from: MockCommit | undefined;
    if (options.from && options.from.trim()) {
      from = resolveCommit(repo.commits, options.from);
      if (!from) {
        fail(404, "not_found", `commit "${options.from}" does not exist`);
      }
    } else {
      from = repo.commits[repo.commits.indexOf(to) + 1];
    }
    return {
      from: from?.info.id ?? "",
      to: to.info.id,
      diff: buildDiff(from?.files ?? {}, to.files),
    };
  },

  async gitStatus(id: string): Promise<GitStatus> {
    requireAuth();
    await delay(140);
    const repo = repoFor(id);
    const fs = fsFor(id);
    const head = repo.commits[0]?.files ?? {};
    const current = trackedSnapshot(fs);
    const modified = Object.keys(head)
      .filter((path) => current[path] !== head[path])
      .sort();
    const untracked = Object.keys(current)
      .filter((path) => !(path in head))
      .sort();
    return { modified, untracked };
  },

  async gitRevert(id: string, input: RevertInput): Promise<RevertResult> {
    const identity = requireWrite();
    await delay(320);
    const row = findRow(id);
    const repo = repoFor(id);
    const fs = fsFor(id);
    const target = resolveCommit(repo.commits, input.commit);
    if (!target) {
      fail(404, "not_found", `commit "${input.commit}" does not exist`);
    }

    const requested = (input.paths && input.paths.length > 0 ? input.paths : target.info.paths).map(
      resolveRepoPath,
    );
    for (const repoPath of requested) {
      if (!isTrackedRepoPath(repoPath)) {
        fail(400, "untracked_path", `"${repoPath}" is not versioned`);
      }
    }

    const restored: string[] = [];
    const stamp = Date.now();
    for (const repoPath of requested) {
      const virtualPath = virtualForRepo(repoPath);
      const desired = target.files[repoPath];
      const current = fs.files.get(virtualPath);

      if (desired === undefined) {
        if (!current) continue;
        fs.files.set(nextBackupPath(fs, virtualPath), { ...current, mtime: stamp });
        fs.files.delete(virtualPath);
        restored.push(repoPath);
        continue;
      }
      if (current && current.content === desired) continue;
      if (current) {
        fs.files.set(nextBackupPath(fs, virtualPath), { ...current, mtime: stamp });
      }
      ensureDir(fs, parentPath(virtualPath));
      fs.files.set(virtualPath, {
        content: desired,
        mtime: stamp,
        rendered: isRenderedPath(virtualPath),
      });
      restored.push(repoPath);
    }

    const commit = recordCommit(id, fs, authorFor(identity), "ui");
    let restarted = false;
    if (input.restart) {
      row.status = "running";
      row.started_at = Date.now();
      row.updated_at = nowIso();
      row.last_error = null;
      row.logs.push(logLine(Date.now(), "INFO", `restarted after restoring ${target.info.short}`));
      restarted = true;
    }
    recordAudit(identity, "instance.git.revert", row.id, {
      source: target.info.id,
      paths: restored,
      commit: commit?.id ?? null,
    });
    return {
      source: target.info.id,
      paths: restored,
      commit,
      restarted,
      status: row.status,
    };
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
    requireWrite();
    await delay(260);
    if (!isValidSlug(name)) {
      fail(400, "invalid_input", "Name must be 2-32 characters: lowercase letters, digits, hyphens.");
    }
    if (shares.some((share) => share.name === name)) {
      fail(409, "conflict", `Share "${name}" already exists.`);
    }
    shares.push({ name, path: `${FLEETT_ROOT}/shares/${name}`, instances: [] });
  },

  async deleteShare(name: string): Promise<void> {
    requireWrite();
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
    requireWrite();
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
    requireWrite();
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

  async listTokens(): Promise<TokenInfo[]> {
    requireAdmin();
    await delay();
    return tokens.map((token) => ({ ...token }));
  },

  async createToken(input: CreateTokenInput): Promise<CreatedToken> {
    requireAdmin();
    await delay(280);
    const name = input.name.trim();
    if (!name) {
      fail(400, "invalid_name", "name is required");
    }
    if (name.length > 64) {
      fail(400, "invalid_name", "name must be at most 64 characters");
    }
    const role: Role = input.role === "admin" ? "admin" : "viewer";
    const created_at = nowIso();
    const record: TokenInfo = {
      id: `tok_${randomHex(8)}`,
      name,
      role,
      created_at,
      revoked_at: null,
    };
    tokens = [record, ...tokens];
    return {
      id: record.id,
      name,
      role,
      created_at,
      token: `flt_${randomHex(16)}`,
    };
  },

  async revokeToken(id: string): Promise<void> {
    requireAdmin();
    await delay(240);
    const token = tokens.find((candidate) => candidate.id === id);
    if (!token) {
      fail(404, "not_found", `Token "${id}" does not exist.`);
    }
    if (!token.revoked_at) token.revoked_at = nowIso();
  },

  async listAudit(limit: number): Promise<AuditEntry[]> {
    requireAdmin();
    await delay(160);
    const count = Number.isFinite(limit) && limit > 0 ? Math.min(Math.floor(limit), 1000) : 200;
    return audit.slice(0, count).map((entry) => ({ ...entry }));
  },
};
