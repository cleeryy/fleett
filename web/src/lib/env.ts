import type { EnvEntry, EnvInput, EnvSuggestion } from "@/api/types";

/**
 * One editable environment variable. The controller encrypts `secret: true`
 * values and never returns them, so a stored secret is edited blind: an empty
 * value means "keep what is stored", a typed value replaces it.
 */
export interface EnvRow {
  /** Stable React key; unique for the life of the editor. */
  id: number;
  key: string;
  value: string;
  secret: boolean;
  /** Key as stored on the controller, `null` for a row that is not saved yet. */
  storedKey: string | null;
  /** The controller holds an encrypted value for this row. */
  storedSecret: boolean;
}

export interface EnvRowErrors {
  key?: string;
  value?: string;
}

export type EnvMode = "create" | "apply";

/** Grammar from API.md: `^[A-Z_][A-Z0-9_]{0,63}$`. */
const ENV_KEY = /^[A-Z_][A-Z0-9_]{0,63}$/;

const RESERVED_KEYS = new Set(["HERMES_UID", "HERMES_GID", "PATH", "HOME"]);

let nextRowId = 1;

export function newEnvRow(partial: Partial<Omit<EnvRow, "id">> = {}): EnvRow {
  const id = nextRowId;
  nextRowId += 1;
  return {
    id,
    key: "",
    value: "",
    secret: false,
    storedKey: null,
    storedSecret: false,
    ...partial,
  };
}

export function envRowsFromDetail(entries: EnvEntry[]): EnvRow[] {
  return entries.map((entry) =>
    newEnvRow({
      key: entry.key,
      value: entry.secret ? "" : (entry.value ?? ""),
      secret: entry.secret,
      storedKey: entry.key,
      storedSecret: entry.secret,
    }),
  );
}

export function envKeyError(key: string): string | null {
  if (!key) return "Enter a variable name.";
  if (!ENV_KEY.test(key)) return "Use up to 64 uppercase letters, digits and underscores.";
  if (key.startsWith("FLEETT_") || RESERVED_KEYS.has(key)) return `${key} is reserved by the container.`;
  return null;
}

export function envValueError(value: string): string | null {
  if (value.length > 8192) return "Use 8192 characters or fewer.";
  if (/[\n\r\0]/.test(value)) return "Use a single line.";
  return null;
}

/** Check one value against the format a template declares. */
export function envSuggestionValueError(
  format: string | undefined,
  value: string,
): string | null {
  if (!value) return null;
  const trimmed = value.trim();
  switch (format) {
    case "csv-of-ids":
      return /^\d+(\s*,\s*\d+)*$/.test(trimmed)
        ? null
        : "Use comma-separated numeric ids, for example 81459823,310472918374.";
    case "bool":
      return /^(true|false|1|0|yes|no)$/i.test(trimmed)
        ? null
        : "Use true or false.";
    case "url":
      return isHttpUrl(trimmed) ? null : "Use an absolute http or https URL.";
    default:
      return null;
  }
}

function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

/**
 * Validate every row. `create` treats all of them as new (a secret needs a
 * value); `apply` accepts an empty value on a stored secret, which the full
 * replace sends without a value so the controller keeps it. Template
 * suggestions add the required and format rules the controller enforces.
 */
export function validateEnvRows(
  rows: EnvRow[],
  mode: EnvMode,
  suggestions: EnvSuggestion[] = [],
): { rows: Record<number, EnvRowErrors>; count: number } {
  const errors: Record<number, EnvRowErrors> = {};
  const seen = new Set<string>();
  const suggestionByKey = new Map(suggestions.map((suggestion) => [suggestion.key, suggestion]));
  let count = 0;

  for (const row of rows) {
    const key = row.key.trim();
    const rowErrors: EnvRowErrors = {};
    const suggestion = suggestionByKey.get(key);

    const keyProblem = envKeyError(key);
    if (keyProblem) {
      rowErrors.key = keyProblem;
    } else if (seen.has(key)) {
      rowErrors.key = "Already used above.";
    } else {
      seen.add(key);
    }

    const valueProblem = envValueError(row.value);
    if (valueProblem) {
      rowErrors.value = valueProblem;
    } else if (!keyProblem) {
      const keepsStored = mode === "apply" && row.value === "" && row.storedSecret;
      if (row.secret && !row.value && !keepsStored) {
        rowErrors.value = "Enter a value for this secret.";
      } else if (!row.secret && !row.value && row.storedSecret) {
        rowErrors.value = "Enter a value to store this variable in clear text.";
      } else if (!row.value && !keepsStored && suggestion?.required) {
        rowErrors.value = "Required by the template: the gateway refuses every message without it.";
      } else {
        const formatProblem = envSuggestionValueError(suggestion?.format, row.value);
        if (formatProblem) rowErrors.value = formatProblem;
      }
    }

    if (rowErrors.key || rowErrors.value) {
      errors[row.id] = rowErrors;
      count += 1;
    }
  }

  return { rows: errors, count };
}

/** Request body for create and for the full replace. */
export function envPayload(rows: EnvRow[], mode: EnvMode): EnvInput[] {
  return rows.map((row) => {
    const key = row.key.trim();
    if (mode === "apply" && row.secret && row.value === "" && row.storedSecret) {
      return { key, secret: true };
    }
    return { key, value: row.value, secret: row.secret };
  });
}

/** True when the rows still describe exactly what the controller stores. */
export function envRowsMatch(entries: EnvEntry[], rows: EnvRow[]): boolean {
  if (entries.length !== rows.length) return false;
  const byKey = new Map(entries.map((entry) => [entry.key, entry]));
  const seen = new Set<string>();

  for (const row of rows) {
    const key = row.key.trim();
    if (!key || seen.has(key)) return false;
    seen.add(key);
    const entry = byKey.get(key);
    if (!entry) return false;
    if (entry.secret !== row.secret) return false;
    if (row.secret) {
      if (row.value !== "") return false;
    } else if ((entry.value ?? "") !== row.value) {
      return false;
    }
  }

  return true;
}
