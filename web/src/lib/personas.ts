import type { PersonaVariable } from "@/api/types";

/** Same grammar the controller enforces on a persona variable key. */
export const PERSONA_VAR_KEY_RE = /^[a-z][a-z0-9_]{0,31}$/;

/** Longest tag the controller accepts. */
export const PERSONA_TAG_MAX = 32;

/** One editable row; `id` only exists to keep React keys stable while typing. */
export interface PersonaVariableRow {
  id: string;
  key: string;
  description: string;
  default: string;
}

let rowSequence = 0;

export function newVariableRow(variable?: Partial<PersonaVariable>): PersonaVariableRow {
  rowSequence += 1;
  return {
    id: `var-${rowSequence}`,
    key: variable?.key ?? "",
    description: variable?.description ?? "",
    default: variable?.default ?? "",
  };
}

export function rowsFromVariables(variables: PersonaVariable[]): PersonaVariableRow[] {
  return variables.map((variable) => newVariableRow(variable));
}

/** Drop empty rows (a key typed then cleared) and trim every field. */
export function variablesFromRows(rows: PersonaVariableRow[]): PersonaVariable[] {
  return rows
    .filter((row) => row.key.trim() || row.description.trim() || row.default.trim())
    .map((row) => {
      const key = row.key.trim();
      const description = row.description.trim();
      const fallback = row.default.trim();
      return {
        key,
        description,
        ...(fallback ? { default: fallback } : {}),
      };
    });
}

/** Errors keyed by row id, so the table can point at the exact cell. */
export function validateVariableRows(
  rows: PersonaVariableRow[],
): Record<string, { key?: string; description?: string }> {
  const errors: Record<string, { key?: string; description?: string }> = {};
  const seen = new Map<string, number>();
  for (const row of rows) {
    const key = row.key.trim();
    if (!key && !row.description.trim() && !row.default.trim()) continue;
    const rowErrors: { key?: string; description?: string } = {};
    if (!key) rowErrors.key = "Enter a key.";
    else if (!PERSONA_VAR_KEY_RE.test(key)) {
      rowErrors.key = "Lowercase letters, digits and underscores; start with a letter.";
    }
    if (key) seen.set(key, (seen.get(key) ?? 0) + 1);
    if (Object.keys(rowErrors).length > 0) errors[row.id] = rowErrors;
  }
  for (const row of rows) {
    const key = row.key.trim();
    if (key && (seen.get(key) ?? 0) > 1) {
      errors[row.id] = { ...errors[row.id], key: "This key is already listed." };
    }
  }
  return errors;
}

/** "ops, tone, infra" → ["ops","tone","infra"], trimmed and deduplicated. */
export function parseTags(raw: string): string[] {
  const tags: string[] = [];
  for (const candidate of raw.split(",")) {
    const tag = candidate.trim();
    if (!tag) continue;
    if (!tags.includes(tag)) tags.push(tag);
  }
  return tags;
}

export function formatTags(tags: string[]): string {
  return tags.join(", ");
}

/** Drop empty overrides and trim the rest. */
export function compactVariables(values: Record<string, string>): Record<string, string> {
  const result: Record<string, string> = {};
  for (const [key, value] of Object.entries(values)) {
    if (typeof value === "string" && value.trim()) result[key] = value.trim();
  }
  return result;
}

/** Apply reports are per-instance; accept the usual success spellings. */
export function applySucceeded(status: string): boolean {
  return status === "applied" || status === "ok" || status === "healthy";
}

/** `{{key}}` occurrences in a Markdown template, for hints and limits. */
export function templateVariables(soul: string): string[] {
  const found = new Set<string>();
  for (const match of soul.matchAll(/\{\{\s*([a-z][a-z0-9_]{0,31})\s*\}\}/g)) {
    found.add(match[1]);
  }
  return [...found];
}
