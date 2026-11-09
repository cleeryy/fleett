/**
 * Per-browser memory of the first-run assistant. The host state stays the
 * source of truth (`complete` is derived live by the controller); this only
 * decides whether the Fleet page repeats itself on the same browser.
 */
export interface OnboardingMemory {
  /** The getting-started card was dismissed here. */
  dismissed: boolean;
  /** An unfinished setup was seen: the success banner is for this operator. */
  seen: boolean;
  /** Instance id the success banner already celebrated. */
  celebrated: string | null;
}

const KEY = "fleett.onboarding";

const EMPTY: OnboardingMemory = { dismissed: false, seen: false, celebrated: null };

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
