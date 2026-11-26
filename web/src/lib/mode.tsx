import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

/**
 * Console presentation mode (issues #71, #73, #75).
 *
 * `simple` is the default for everyone, admins included: four spaces, business
 * vocabulary, agent-oriented flows. `advanced` reveals the automation,
 * data and system groups without removing anything from `simple`. This is a
 * presentation preference, never an authorization boundary: RBAC is enforced
 * by the controller and by the same `adminOnly` route guards in both modes.
 *
 * Stored per browser (`localStorage`), so it survives reloads and sign-out;
 * the controller exposes no per-user preference endpoint yet.
 */
export type UiMode = "simple" | "advanced";

const MODE_KEY = "fleett.mode";

export function readUiMode(): UiMode {
  try {
    return localStorage.getItem(MODE_KEY) === "advanced" ? "advanced" : "simple";
  } catch {
    // Private mode / embedded webview: the default is the product default.
    return "simple";
  }
}

function persist(mode: UiMode): void {
  try {
    localStorage.setItem(MODE_KEY, mode);
  } catch {
    /* ignore storage failures */
  }
}

interface UiModeValue {
  mode: UiMode;
  /** `true` when the console is in `simple` mode. */
  simple: boolean;
  setMode: (mode: UiMode) => void;
  toggleMode: () => void;
}

const UiModeContext = createContext<UiModeValue | null>(null);

export function UiModeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<UiMode>(readUiMode);

  const setMode = useCallback((next: UiMode) => {
    setModeState(next);
    persist(next);
  }, []);

  const toggleMode = useCallback(() => {
    setModeState((current) => {
      const next: UiMode = current === "simple" ? "advanced" : "simple";
      persist(next);
      return next;
    });
  }, []);

  const value = useMemo<UiModeValue>(
    () => ({ mode, simple: mode === "simple", setMode, toggleMode }),
    [mode, setMode, toggleMode],
  );

  return <UiModeContext.Provider value={value}>{children}</UiModeContext.Provider>;
}

/** Reads the presentation mode; the provider lives next to the i18n one. */
export function useUiMode(): UiModeValue {
  const value = useContext(UiModeContext);
  if (!value) throw new Error("useUiMode must be used inside <UiModeProvider>");
  return value;
}
