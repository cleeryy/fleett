import type { ProviderSummary } from "@/api/types";

/**
 * One editable extra header of a provider. Values are write-only, exactly like
 * the API key: the controller returns the names, never the values. An empty
 * value on a stored row therefore means "keep the encrypted value".
 */
export interface HeaderRow {
  /** Stable React key; unique for the life of the editor. */
  id: number;
  name: string;
  value: string;
  /** The controller already holds a (encrypted) value for this name. */
  stored: boolean;
}

let nextHeaderId = 1;

export function newHeaderRow(partial: Partial<Omit<HeaderRow, "id">> = {}): HeaderRow {
  const id = nextHeaderId;
  nextHeaderId += 1;
  return { id, name: "", value: "", stored: false, ...partial };
}

/** Names only: `extra_header_names` is all the API can return. */
export function providerHeaderNames(
  provider: Pick<ProviderSummary, "extra_header_names">,
): string[] {
  const names = provider.extra_header_names;
  if (!Array.isArray(names)) return [];
  return names.map((name) => name.trim()).filter(Boolean);
}

/** RFC 7230 `token`, the grammar every HTTP header name follows. */
export const HEADER_NAME_RE = /^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/;

export function isValidHeaderName(value: string): boolean {
  return HEADER_NAME_RE.test(value);
}

/** Model ids carry `/`, `.`, `:` and `-`; whitespace never belongs in one. */
export function isValidModelName(value: string): boolean {
  const model = value.trim();
  return model.length > 0 && model.length <= 200 && !/\s/.test(model);
}

/**
 * Headers to send on `PATCH`: every stored name starts as `null` (removed),
 * a kept-but-untouched row is dropped from the map (the value stays), and a
 * typed value replaces or adds its name.
 */
export function patchHeadersFor(
  rows: HeaderRow[],
  storedNames: string[],
): Record<string, string | null> {
  const patch: Record<string, string | null> = {};
  for (const name of storedNames) patch[name] = null;
  for (const row of rows) {
    const name = row.name.trim();
    if (!name) continue;
    if (row.stored && row.value === "") {
      delete patch[name];
      continue;
    }
    patch[name] = row.value;
  }
  return patch;
}

/** `POST` body: only names that carry a value; nothing to remove yet. */
export function createHeadersFor(rows: HeaderRow[]): Record<string, string> {
  const headers: Record<string, string> = {};
  for (const row of rows) {
    const name = row.name.trim();
    if (name && row.value !== "") headers[name] = row.value;
  }
  return headers;
}
