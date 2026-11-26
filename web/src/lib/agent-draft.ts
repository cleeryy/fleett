import type { ChannelType, EnvSuggestion, ShareRef, SkillRef, Template } from "@/api/types";
import { newEnvRow, validateEnvRows, type EnvRow } from "@/lib/env";
import { MAX_SKILLS_PER_INSTANCE, tooManySkills } from "@/lib/skills";
import { isValidSlug } from "@/lib/validation";

/**
 * One draft shared by the guided creation wizard (#72) and the expert form
 * (`#/instances/new?form=expert`). It is persisted under
 * `localStorage["fleett.draft.agent"]` so a reload, a language switch or a hop
 * to the expert form never costs the answers already given.
 *
 * Only the tenant-side fields of `CreateInstanceInput` live here: no derived
 * data (resource lists, provider metadata), so a stale draft can never send
 * values the controller does not accept without being validated first.
 */
export type WizardUsage = "discord" | "watch" | "dev" | "other";
export type AgentLanguage = "fr" | "en";

export interface AgentDraft {
  /** Which "what is it for?" card picked the template. */
  usage: WizardUsage | "";
  name: string;
  id: string;
  /** `true` once the id was typed by hand in the expert form. */
  idTouched: boolean;
  template: string;
  model: string;
  provider: string;
  /** `true` once the provider was chosen (or cleared) by hand. */
  providerTouched: boolean;
  persona: string;
  personaVars: Record<string, string>;
  language: AgentLanguage;
  notes: string;
  baseUrl: string;
  apiKeyEnv: string;
  providerToken: string;
  channel: ChannelType;
  /** `true` once the channel was chosen by hand, so a template does not override it. */
  channelTouched: boolean;
  token: string;
  shares: ShareRef[];
  skills: SkillRef[];
  memLimit: string;
  allowLan: boolean;
  env: EnvRow[];
  /** Wizard resume point; ignored by the expert form. */
  step: number;
}

export const AGENT_DRAFT_KEY = "fleett.draft.agent";
export const AGENT_DRAFT_VERSION = 1;
export const AGENT_DRAFT_TTL_MS = 7 * 24 * 60 * 60 * 1000;
export const WIZARD_STEP_COUNT = 4;

export const DEFAULT_BASE_URL = "https://opencode.ai/inference/go/openai/v1";
export const DEFAULT_API_KEY_ENV = "HERMES_GATE_PROFILE_TOKEN";
export const DEFAULT_MEM_LIMIT = "1536";

/** Fresh defaults; never share the mutable arrays between callers. */
export function defaultAgentDraft(): AgentDraft {
  return {
    usage: "",
    name: "",
    id: "",
    idTouched: false,
    template: "",
    model: "",
    provider: "",
    providerTouched: false,
    persona: "",
    personaVars: {},
    language: "fr",
    notes: "",
    baseUrl: DEFAULT_BASE_URL,
    apiKeyEnv: DEFAULT_API_KEY_ENV,
    providerToken: "",
    channel: "none",
    channelTouched: false,
    token: "",
    shares: [],
    skills: [],
    memLimit: DEFAULT_MEM_LIMIT,
    allowLan: false,
    env: [],
    step: 0,
  };
}

interface StoredAgentDraft {
  version: number;
  savedAt: number;
  draft: unknown;
}

function asString(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function asBoolean(value: unknown, fallback = false): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function isChannelType(value: unknown): value is ChannelType {
  return value === "telegram" || value === "discord" || value === "none";
}

function isUsage(value: unknown): value is WizardUsage {
  return value === "discord" || value === "watch" || value === "dev" || value === "other";
}

function normalizeShares(value: unknown): ShareRef[] {
  if (!Array.isArray(value)) return [];
  const out: ShareRef[] = [];
  for (const item of value) {
    if (!item || typeof item !== "object") continue;
    const record = item as Record<string, unknown>;
    const name = asString(record.name).trim();
    if (!name) continue;
    out.push({ name, mode: record.mode === "ro" ? "ro" : "rw" });
  }
  return out;
}

function normalizeSkills(value: unknown): SkillRef[] {
  if (!Array.isArray(value)) return [];
  const out: SkillRef[] = [];
  for (const item of value) {
    if (!item || typeof item !== "object") continue;
    const record = item as Record<string, unknown>;
    const name = asString(record.name).trim();
    if (!name) continue;
    const version = typeof record.version === "string" && record.version.trim() ? record.version.trim() : null;
    out.push({ name, version });
  }
  return out;
}

function normalizeEnvRows(value: unknown): EnvRow[] {
  if (!Array.isArray(value)) return [];
  const rows: EnvRow[] = [];
  for (const item of value) {
    if (!item || typeof item !== "object") continue;
    const record = item as Record<string, unknown>;
    rows.push(
      newEnvRow({
        key: asString(record.key),
        value: asString(record.value),
        secret: asBoolean(record.secret),
      }),
    );
  }
  return rows;
}

function normalizeVariables(value: unknown): Record<string, string> {
  if (!value || typeof value !== "object") return {};
  const out: Record<string, string> = {};
  for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
    if (typeof entry === "string") out[key] = entry;
  }
  return out;
}

/**
 * Read a stored draft defensively: `localStorage` survives releases, so a
 * stale shape must degrade to defaults, never throw during render.
 */
export function normalizeAgentDraft(raw: unknown): AgentDraft {
  const base = defaultAgentDraft();
  if (!raw || typeof raw !== "object") return base;
  const draft = raw as Record<string, unknown>;
  const step = typeof draft.step === "number" && Number.isFinite(draft.step) ? draft.step : 0;
  return {
    usage: isUsage(draft.usage) ? draft.usage : "",
    name: asString(draft.name),
    id: asString(draft.id),
    idTouched: asBoolean(draft.idTouched),
    template: asString(draft.template),
    model: asString(draft.model),
    provider: asString(draft.provider),
    providerTouched: asBoolean(draft.providerTouched),
    persona: asString(draft.persona),
    personaVars: normalizeVariables(draft.personaVars),
    language: draft.language === "en" ? "en" : "fr",
    notes: asString(draft.notes),
    baseUrl: asString(draft.baseUrl, base.baseUrl),
    apiKeyEnv: asString(draft.apiKeyEnv, base.apiKeyEnv),
    providerToken: asString(draft.providerToken),
    channel: isChannelType(draft.channel) ? draft.channel : "none",
    channelTouched: asBoolean(draft.channelTouched),
    token: asString(draft.token),
    shares: normalizeShares(draft.shares),
    skills: normalizeSkills(draft.skills),
    memLimit: asString(draft.memLimit, base.memLimit),
    allowLan: asBoolean(draft.allowLan),
    env: normalizeEnvRows(draft.env),
    step: Math.max(0, Math.min(WIZARD_STEP_COUNT - 1, Math.round(step))),
  };
}

/** A draft worth offering to resume: defaults alone are not. */
export function draftHasContent(draft: AgentDraft): boolean {
  return Boolean(
    draft.name.trim() ||
      draft.id.trim() ||
      draft.template ||
      draft.model.trim() ||
      draft.provider ||
      draft.persona ||
      draft.providerToken.trim() ||
      draft.token.trim() ||
      draft.notes.trim() ||
      draft.shares.length ||
      draft.skills.length ||
      draft.env.length,
  );
}

/**
 * Stored draft for this browser, or `null` when absent, unreadable, written by
 * an older shape or older than the 7-day TTL. Expired blobs are removed.
 */
export function loadAgentDraft(now: number = Date.now()): AgentDraft | null {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(AGENT_DRAFT_KEY);
  } catch {
    return null; // private mode / storage disabled
  }
  if (!raw) return null;

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    clearAgentDraft();
    return null;
  }
  if (!parsed || typeof parsed !== "object") {
    clearAgentDraft();
    return null;
  }
  const stored = parsed as Partial<StoredAgentDraft>;
  if (stored.version !== AGENT_DRAFT_VERSION || typeof stored.savedAt !== "number") {
    clearAgentDraft();
    return null;
  }
  if (!Number.isFinite(stored.savedAt) || now - stored.savedAt > AGENT_DRAFT_TTL_MS) {
    clearAgentDraft();
    return null;
  }
  const draft = normalizeAgentDraft(stored.draft);
  if (!draftHasContent(draft)) {
    clearAgentDraft();
    return null;
  }
  return draft;
}

/**
 * Persist the draft; failures (private mode, quota) are not fatal.
 *
 * Credentials never reach `localStorage`: the bot token, the per-agent gate
 * token and every secret env value are stripped. They stay in memory for the
 * current session and are asked again after a reload, which is the same
 * trade-off the API makes when it refuses to return a stored secret.
 */
export function saveAgentDraft(draft: AgentDraft, now: number = Date.now()): void {
  const safe: AgentDraft = {
    ...draft,
    token: "",
    providerToken: "",
    env: draft.env.map((row) => (row.secret ? { ...row, value: "" } : row)),
  };
  const payload: StoredAgentDraft = {
    version: AGENT_DRAFT_VERSION,
    savedAt: now,
    draft: safe,
  };
  try {
    localStorage.setItem(AGENT_DRAFT_KEY, JSON.stringify(payload));
  } catch {
    /* storage unavailable: the form still works, it just cannot resume */
  }
}

export function clearAgentDraft(): void {
  try {
    localStorage.removeItem(AGENT_DRAFT_KEY);
  } catch {
    /* ignore */
  }
}

/** `telegram` / `discord` / `none`, or `null` for an unexpected template value. */
export function templateChannel(value: string | undefined): ChannelType | null {
  return value === "telegram" || value === "discord" || value === "none" ? value : null;
}

/** Template + channel a usage card stands for. */
export function usageDefaults(
  usage: Exclude<WizardUsage, "other">,
  templates: Template[],
): { template: Template | null; channel: ChannelType } {
  const byId = (id: string) => templates.find((template) => template.id === id) ?? null;
  const firstWithChannel = (channel: ChannelType) =>
    templates.find((template) => template.channel === channel) ?? null;
  const matching = (pattern: RegExp) =>
    templates.find((template) => pattern.test(`${template.id} ${template.name}`)) ?? null;
  const fallback = templates[0] ?? null;

  switch (usage) {
    case "discord":
      return {
        template: byId("guest-discord") ?? firstWithChannel("discord") ?? fallback,
        // The card names the connector: a fallback template must not turn a
        // Discord assistant into a Telegram one.
        channel: "discord",
      };
    case "watch":
      return {
        template:
          matching(/watch|veille|monitor|digest|summary|résumé|resume/i) ??
          byId("solo") ??
          firstWithChannel("none") ??
          fallback,
        channel: "none",
      };
    case "dev":
      return {
        template:
          byId("sandbox") ??
          matching(/dev|sandbox|code/i) ??
          byId("solo") ??
          firstWithChannel("none") ??
          fallback,
        channel: "none",
      };
  }
}

/** Best usage card for a template that came from a link or the expert form. */
export function usageForTemplate(template: Template | undefined): WizardUsage {
  if (!template) return "other";
  if (template.channel === "discord") return "discord";
  return "other";
}

type Translate = (key: string, params?: Record<string, string | number>) => string;

/** Ids of the fields a step error can point at. */
export const WIZARD_FIELD_IDS: Record<string, string> = {
  template: "wizard-template",
  purpose: "wizard-purpose",
  name: "wizard-name",
  id: "wizard-id",
  model: "wizard-model",
  token: "wizard-channel-token",
  memLimit: "wizard-mem-limit",
  skills: "wizard-skill",
};

/**
 * Validate one wizard step. Later steps never leak into the current one: the
 * wizard asks one question at a time.
 */
export function validateWizardStep(
  draft: AgentDraft,
  step: number,
  templateEnv: EnvSuggestion[] = [],
  t: Translate = (key) => key,
): Record<string, string> {
  const errors: Record<string, string> = {};

  if (step === 0) {
    if (!draft.template) errors.template = t("wizard.errors.templateRequired");
  }

  if (step === 1) {
    if (!draft.name.trim()) errors.name = t("wizard.errors.nameRequired");
    else if (draft.name.trim().length > 48) errors.name = t("wizard.errors.nameTooLong");
    if (!draft.id.trim()) errors.id = t("wizard.errors.idRequired");
    else if (!isValidSlug(draft.id.trim())) errors.id = t("wizard.errors.idFormat");
    else if (draft.id.trim() === "new") errors.id = t("wizard.errors.idReserved");
  }

  if (step === 2) {
    if (!draft.model.trim()) errors.model = t("wizard.errors.modelRequired");
    if (draft.channel !== "none" && !draft.token.trim()) {
      errors.token = t("wizard.errors.tokenRequired");
    }
  }

  if (step === 3) {
    const memory = Number(draft.memLimit);
    if (!Number.isFinite(memory) || memory < 128 || memory > 8192) {
      errors.memLimit = t("wizard.errors.memoryRange");
    }
    const envErrors = validateEnvRows(draft.env, "create", templateEnv, t).rows;
    for (const row of draft.env) {
      const rowErrors = envErrors[row.id];
      if (rowErrors?.key) errors[`env-${row.id}-key`] = rowErrors.key;
      if (rowErrors?.value) errors[`env-${row.id}-value`] = rowErrors.value;
    }
    if (tooManySkills(draft.skills)) {
      errors.skills = t("wizard.errors.tooManySkills", { max: MAX_SKILLS_PER_INSTANCE });
    }
  }

  return errors;
}

/** Every step at once, for the final create action. */
export function validateWizardDraft(
  draft: AgentDraft,
  templateEnv: EnvSuggestion[] = [],
  t: Translate = (key) => key,
): Record<number, Record<string, string>> {
  const out: Record<number, Record<string, string>> = {};
  for (let step = 0; step < WIZARD_STEP_COUNT; step += 1) {
    out[step] = validateWizardStep(draft, step, templateEnv, t);
  }
  return out;
}
