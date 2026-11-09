import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { en } from "@/locales/en";
import { fr } from "@/locales/fr";

/**
 * Console internationalisation.
 *
 * The dictionary is a flat `Record<string, string>` keyed by area
 * (`nav.instances`, `errors.not_found`, …). English is the source of truth:
 * a key missing from French falls back to English, a key missing everywhere
 * renders its own name and warns once in development (see `web/I18N.md`).
 */
export type Lang = "en" | "fr";

export type Dict = Record<string, string>;

const DICTS: Record<Lang, Dict> = { en, fr };

const LANG_KEY = "fleett.lang";

function isLang(value: unknown): value is Lang {
  return value === "en" || value === "fr";
}

/** `fr-FR`, `fr`, `FR_ca` → `fr`; anything else → `en`. */
export function normalizeLang(value: string | null | undefined): Lang {
  return value?.trim().toLowerCase().startsWith("fr") ? "fr" : "en";
}

/** Persisted choice first, then the browser's preferred language. */
export function detectLang(): Lang {
  try {
    const stored = localStorage.getItem(LANG_KEY);
    if (isLang(stored)) return stored;
  } catch {
    /* storage can be blocked (private mode); fall through to the browser */
  }
  if (typeof navigator === "undefined") return "en";
  const preferred = navigator.languages?.[0] ?? navigator.language;
  return normalizeLang(preferred);
}

/** `"Instance {name} deleted."` + `{ name: "wiki" }` → `"Instance wiki deleted."` */
export function interpolate(template: string, params?: Record<string, string | number>): string {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) => {
    const value = params[name];
    return value === undefined ? match : String(value);
  });
}

/** Whether a key has copy in either dictionary (used by the API error mapper). */
export function hasTranslation(key: string): boolean {
  return DICTS.en[key] !== undefined || DICTS.fr[key] !== undefined;
}

const warned = new Set<string>();

function translate(lang: Lang, key: string, params?: Record<string, string | number>): string {
  const value = DICTS[lang][key] ?? DICTS.en[key];
  if (value === undefined) {
    if (import.meta.env.DEV && !warned.has(key)) {
      warned.add(key);
      console.warn(`[i18n] missing key "${key}" (add it to locales/en.ts and locales/fr.ts)`);
    }
    return key;
  }
  return interpolate(value, params);
}

export interface I18nValue {
  lang: Lang;
  setLang: (lang: Lang) => void;
  /** Translate `key`; unknown keys fall back to English, then to the key itself. */
  t: (key: string, params?: Record<string, string | number>) => string;
}

const I18nContext = createContext<I18nValue | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(detectLang);

  useEffect(() => {
    document.documentElement.lang = lang;
    try {
      localStorage.setItem(LANG_KEY, lang);
    } catch {
      /* ignore storage failures */
    }
  }, [lang]);

  const setLang = useCallback((next: Lang) => {
    setLangState(next);
  }, []);

  const t = useCallback(
    (key: string, params?: Record<string, string | number>) => translate(lang, key, params),
    [lang],
  );

  const value = useMemo<I18nValue>(() => ({ lang, setLang, t }), [lang, setLang, t]);

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nValue {
  const value = useContext(I18nContext);
  if (!value) throw new Error("useI18n must be used inside <I18nProvider>");
  return value;
}

/** Translator outside React, for helpers that run in effects (e.g. document.title). */
export function translator(lang: Lang) {
  return (key: string, params?: Record<string, string | number>) => translate(lang, key, params);
}
