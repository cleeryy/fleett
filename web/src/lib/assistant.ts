import { useEffect, useRef, useSyncExternalStore } from "react";
import type { AssistantContext, AssistantTask } from "@/api/types";

/**
 * What the console knows about the current page. The panel merges every
 * publisher (most specific last) and turns the result into the `context`
 * object of `POST /api/assistant/chat`. `etag` and `errorText` stay local:
 * they are used to apply a proposal and to brief an "explain" task, never sent
 * as-is.
 */
export interface AssistantScope {
  page?: string;
  instance?: string;
  path?: string;
  /** Buffer the operator is editing, not necessarily what is on disk. */
  fileContent?: string;
  /** Version of the file on disk, for the compare-and-swap write. */
  etag?: string;
  selection?: string;
  schema?: string;
  /** `last_error` of the instance, folded into the explain task message. */
  errorText?: string;
}

interface ScopeLayer {
  id: number;
  scope: AssistantScope;
}

let layers: ScopeLayer[] = [];
let nextLayerId = 1;
let snapshot: AssistantScope = {};
const listeners = new Set<() => void>();

function mergeLayers(): AssistantScope {
  const merged: AssistantScope = {};
  for (const layer of layers) {
    for (const key of Object.keys(layer.scope) as (keyof AssistantScope)[]) {
      const value = layer.scope[key];
      // An empty field never erases a broader layer: a page can say "Files"
      // while the detail page says which instance it belongs to.
      if (value === undefined || value === "") continue;
      merged[key] = value as never;
    }
  }
  return merged;
}

function emit(): void {
  snapshot = mergeLayers();
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Read the merged scope; the panel is the only consumer. */
export function useAssistantScopeValue(): AssistantScope {
  return useSyncExternalStore(subscribe, () => snapshot, () => snapshot);
}

/**
 * Publish a slice of context while the caller is mounted. Later mounts win on
 * a field-by-field basis, so the Files tab can refine what the instance page
 * already declared. Passing `null` withdraws the layer (a hidden tab).
 */
export function useAssistantScope(scope: AssistantScope | null): void {
  const layerId = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (layerId.current === null) return;
      layers = layers.filter((layer) => layer.id !== layerId.current);
      layerId.current = null;
      emit();
    };
  }, []);

  useEffect(() => {
    if (!scope) {
      if (layerId.current !== null) {
        layers = layers.filter((layer) => layer.id !== layerId.current);
        layerId.current = null;
        emit();
      }
      return;
    }

    if (layerId.current === null) {
      const id = nextLayerId;
      nextLayerId += 1;
      layerId.current = id;
      layers = [...layers, { id, scope }];
    } else {
      layers = layers.map((layer) =>
        layer.id === layerId.current ? { ...layer, scope } : layer,
      );
    }
    emit();
  }, [
    scope?.page,
    scope?.instance,
    scope?.path,
    scope?.etag,
    scope?.selection,
    scope?.schema,
    scope?.errorText,
    scope?.fileContent,
  ]);
}

/* --------------------------------------------------------------------------
 * Page and schema hints
 * -------------------------------------------------------------------------- */

const PAGES: Record<string, string> = {
  "/instances": "Instances",
  "/instances/new": "New instance",
  "/providers": "Providers",
  "/shares": "Shares",
  "/templates": "Templates",
  "/schedules": "Schedules",
  "/pipelines": "Pipelines",
  "/incidents": "Incidents",
  "/cost": "Cost",
  "/logs": "Logs",
  "/security": "Security",
  "/deployments": "Deployments",
  "/messages": "Messages",
  "/memory": "Memory",
  "/tokens": "Tokens",
  "/audit": "Audit log",
};

/** Page label derived from the route, until a page publishes its own. */
export function pageForPath(path: string): string {
  if (PAGES[path]) return PAGES[path];
  if (path.startsWith("/instances/")) return "Instance";
  if (path.startsWith("/providers/")) return "Provider details";
  return "Fleett";
}

const SCHEMAS: Record<string, string> = {
  "/instances/new":
    "New instance form: id (slug), name, template, provider, model, base_url, api_key_env, channel (telegram|discord|none), shares, memory limit, LAN access, notes, env[].",
  "/providers":
    "Provider: name (lowercase slug), kind (openai-compatible|anthropic|google), base_url, api_key_env, write-only secret.",
  "/shares": "Shared folder: name, host path, instances attached read-write or read-only.",
  "/templates": "Template: id, name, description, default model, channel, env suggestions (allowlists, never credentials).",
  "/schedules":
    "Schedule: name, cron expression, prompt, timezone, overlap (skip|queue), timeout in seconds, catchup.",
  "/pipelines":
    "Pipeline YAML or JSON: name, trigger (manual|cron|webhook), optional cron, enabled, steps[] where each step has id, instance, prompt, depends_on[], retries, timeout_s, on_failure (stop|continue).",
  "/incidents": "Incident: instance, cause (oom|crashloop|unhealthy|image_missing|heal_failed), status, actions and restart count.",
  "/cost": "Cost report: usage events, pricing per model, monthly budget per instance.",
  "/logs": "Log search: instance, level (info|warn|error), time window and message text.",
  "/security": "Security posture: rate limits, lockout, response headers, token counts.",
  "/deployments": "Deployment: image, instances, canary count, soak time, per-instance results and rollback state.",
  "/messages": "Agent bus message: from, to, kind (ask|tell|result), body, correlation id, TTL.",
  "/memory": "Shared memory entry: namespace (global|shared|instance:<id>|project:<name>), key, value, tags, TTL.",
  "/tokens": "Access token: name, role (admin|viewer). The secret is shown once.",
  "/audit": "Audit entry: actor, role, method, action, target, IP and details.",
};

/** Shape of the page, used when no publisher refines it. */
export function schemaForPath(path: string): string | undefined {
  if (SCHEMAS[path]) return SCHEMAS[path];
  if (path.startsWith("/instances/")) {
    return "Instance: id, name, template, provider, model, channel, memory limit, shares, LAN access, environment (KEY=value; secret entries are encrypted and never leave the controller), files under the data directory and their git history.";
  }
  if (path.startsWith("/providers/")) {
    return "Provider detail: kind, base_url, api_key_env, attached instances. The secret is write-only.";
  }
  return undefined;
}

const TAB_SCHEMAS: Record<string, string> = {
  overview:
    "Instance summary: status, model, provider, base URL, channel, memory limit, shares, LAN access, last error.",
  configuration:
    "Instance configuration: name, provider, model, memory limit, LAN access, notes, plus the environment. An env row is KEY, value and a secret flag: secret values are stored encrypted and never sent.",
  schedules: SCHEMAS["/schedules"],
  incidents: SCHEMAS["/incidents"],
  snapshots: "Snapshot: label, creation time, expiry, size and restore state of the data directory.",
  files:
    "Data directory of the instance: SOUL.md, SKILL.md, config.yaml (rendered), skills/ and logs/. Only markdown, YAML, JSON, text and script files are writable.",
  messages: SCHEMAS["/messages"],
  memory: SCHEMAS["/memory"],
  history: "Git history of the instance: one commit per console change, with paths and a revert action.",
  logs: "Container logs of the instance, newest last.",
  access: "Instance ACL: subject (user or token name) and role (owner|operator|viewer).",
  egress: "Egress policy: mode (deny|allow), allowed FQDNs, CIDRs and ports, plus a test probe.",
  terminal: "One-shot command in the instance container: argv[] or a plain command, timeout.",
  compose: "compose.yaml rendered by the controller; it is regenerated on every configuration change.",
};

export function instanceTabSchema(tab: string): string | undefined {
  return TAB_SCHEMAS[tab];
}

/** Shape of the open file, so the model knows the frontmatter rules. */
export function fileSchemaHint(path: string, rendered: boolean): string {
  const name = path.slice(path.lastIndexOf("/") + 1).toLowerCase();
  if (rendered) {
    return `${name} is regenerated by fleett from the instance configuration. Propose the change, but it has to go through the configuration form.`;
  }
  if (name === "skill.md") {
    return "SKILL.md: YAML frontmatter with `name` and `description` (60 characters maximum), then step-by-step instructions, pitfalls to avoid and an example.";
  }
  if (name === "soul.md") {
    return "SOUL.md: markdown with identity, tone, scope of work and the refusals the agent must keep. No secrets.";
  }
  if (name.endsWith(".yaml") || name.endsWith(".yml")) {
    return "YAML file of the instance data directory.";
  }
  if (name.endsWith(".json")) return "JSON file of the instance data directory.";
  if (name.endsWith(".md")) return "Markdown file of the instance data directory.";
  return "Text file of the instance data directory.";
}

/* --------------------------------------------------------------------------
 * Request context
 * -------------------------------------------------------------------------- */

const MAX_FILE_CHARS = 32_000;
const MAX_SELECTION_CHARS = 4_000;

/** `context` of `POST /api/assistant/chat`, with empty fields omitted. */
export function buildAssistantContext(
  scope: AssistantScope,
  fallbackPage: string,
  selection?: string,
): AssistantContext {
  const page = scope.page ?? fallbackPage;
  const picked = (selection ?? scope.selection ?? "").trim();
  const context: AssistantContext = { page };

  if (scope.instance) context.instance = scope.instance;
  if (scope.path) context.path = scope.path;
  if (picked) context.selection = picked.slice(0, MAX_SELECTION_CHARS);
  if (scope.fileContent) context.file_content = scope.fileContent.slice(0, MAX_FILE_CHARS);
  if (scope.schema) context.schema = scope.schema;
  return context;
}

/** Text selected anywhere outside the assistant panel; empty when collapsed. */
export function pageSelection(): string {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed) return "";
  const anchor = selection.anchorNode;
  const element = anchor instanceof Element ? anchor : (anchor?.parentElement ?? null);
  // Selecting the reply itself must not feed it back into the next prompt.
  if (element?.closest("[data-assistant-panel]")) return "";
  return selection.toString().trim();
}

export function baseName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/* --------------------------------------------------------------------------
 * Apply notifications
 * -------------------------------------------------------------------------- */

/** Pages that show a file or the environment refresh when a proposal lands. */
export const ASSISTANT_APPLIED_EVENT = "fleett:assistant-applied";

export interface AssistantAppliedDetail {
  kind: "file" | "env";
  instance: string;
  path?: string;
}

export function announceAssistantApply(detail: AssistantAppliedDetail): void {
  window.dispatchEvent(new CustomEvent<AssistantAppliedDetail>(ASSISTANT_APPLIED_EVENT, { detail }));
}

/* --------------------------------------------------------------------------
 * Quick tasks
 * -------------------------------------------------------------------------- */

export interface QuickTask {
  id: AssistantTask;
  label: string;
  /** First user message of the task; the `task` field travels beside it. */
  instruction: string;
}

export const QUICK_TASKS: QuickTask[] = [
  {
    id: "write_skill",
    label: "Write a SKILL.md",
    instruction:
      "Write a SKILL.md for this instance. Use YAML frontmatter with `name` and a `description` of 60 characters or fewer, then the steps to follow, the pitfalls to avoid and one example.",
  },
  {
    id: "draft_soul",
    label: "Draft a SOUL.md",
    instruction:
      "Draft a SOUL.md for this instance: identity, tone, scope of work and what the agent must refuse. Keep it short and concrete.",
  },
  {
    id: "env_allowlist",
    label: "Suggest env allowlists",
    instruction:
      "Suggest the environment variables this instance needs, as an allowlist block. Give plausible values for plain variables, mark credentials as secret and leave their values empty.",
  },
  {
    id: "explain_error",
    label: "Explain this error",
    instruction: "Explain this error and give the smallest fix that applies here.",
  },
  {
    id: "config_yaml",
    label: "Propose config.yaml",
    instruction:
      "Propose a complete config.yaml for this instance's model and channel, with a comment on every non-obvious field.",
  },
];

/** The message actually sent for a quick task, with the error folded in. */
export function taskInstruction(task: QuickTask, errorText?: string): string {
  if (task.id !== "explain_error" || !errorText) return task.instruction;
  return `${task.instruction}\n\nThe current error is:\n\`\`\`\n${errorText.slice(0, MAX_SELECTION_CHARS)}\n\`\`\``;
}
