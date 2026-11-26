import type { UiMode } from "@/lib/mode";

/**
 * Per-browser memory of the first-run assistant. The host state stays the
 * source of truth (`complete` is derived live by the controller); this only
 * decides whether the Fleet page repeats itself on the same browser.
 */
export interface OnboardingAnswers {
  /** What the operator wants to do first. */
  goal: OnboardingGoal;
  /** How many agents the operator expects to run. */
  scale: OnboardingScale;
  /** Detail level confirmed at the end of the questionnaire. */
  mode: UiMode;
}

export type OnboardingGoal = "create" | "explore" | "administer";
export type OnboardingScale = "one" | "few" | "many";

export interface OnboardingMemory {
  /** The getting-started card was dismissed here. */
  dismissed: boolean;
  /** An unfinished setup was seen: the success banner is for this operator. */
  seen: boolean;
  /** Instance id the success banner already celebrated. */
  celebrated: string | null;
  /** Questionnaire answers; `null` until the first-sign-in dialog is answered. */
  answers: OnboardingAnswers | null;
}

const KEY = "fleett.onboarding";

const EMPTY: OnboardingMemory = {
  dismissed: false,
  seen: false,
  celebrated: null,
  answers: null,
};

const GOALS = ["create", "explore", "administer"] as const;
const SCALES = ["one", "few", "many"] as const;
const MODES = ["simple", "advanced"] as const;

function isGoal(value: unknown): value is OnboardingGoal {
  return typeof value === "string" && (GOALS as readonly string[]).includes(value);
}

function isScale(value: unknown): value is OnboardingScale {
  return typeof value === "string" && (SCALES as readonly string[]).includes(value);
}

function isMode(value: unknown): value is UiMode {
  return typeof value === "string" && (MODES as readonly string[]).includes(value);
}

/**
 * Reads stored answers defensively: a payload written by an older console (no
 * `answers` field) still parses, and an unknown answer value degrades to
 * "not answered" instead of throwing.
 */
function readAnswers(value: unknown): OnboardingAnswers | null {
  if (!value || typeof value !== "object") return null;
  const raw = value as Record<string, unknown>;
  if (!isGoal(raw.goal) || !isScale(raw.scale) || !isMode(raw.mode)) return null;
  return { goal: raw.goal, scale: raw.scale, mode: raw.mode };
}

/**
 * The default detail level suggested by the questionnaire: operators who
 * administer the host or expect many agents get the advanced console, everyone
 * else starts simple. The third answer stays editable.
 */
export function recommendMode(goal: OnboardingGoal, scale: OnboardingScale): UiMode {
  return goal === "administer" || scale === "many" ? "advanced" : "simple";
}

export function readOnboardingMemory(): OnboardingMemory {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return EMPTY;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return EMPTY;
    const value = parsed as Partial<OnboardingMemory>;
    return {
      dismissed: value.dismissed === true,
      seen: value.seen === true,
      celebrated: typeof value.celebrated === "string" ? value.celebrated : null,
      answers: readAnswers(value.answers),
    };
  } catch {
    // Storage unavailable (private mode, embedded webview): behave as a first visit.
    return EMPTY;
  }
}

export function writeOnboardingMemory(patch: Partial<OnboardingMemory>): OnboardingMemory {
  const next = { ...readOnboardingMemory(), ...patch };
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    /* ignore storage failures */
  }
  return next;
}
