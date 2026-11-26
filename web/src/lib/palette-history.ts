/**
 * Local search history for the command palette (#76).
 *
 * `Récents` is a per-browser shortlist, never a server preference: the
 * controller exposes no endpoint for it and the console must work signed out
 * of the account layer. Entries are small — `{kind, id, label, at}` — only
 * successful navigations are written, and the newest five win.
 */

export type PaletteHistoryKind =
  | "page"
  | "agent"
  | "agent-logs"
  | "agent-files"
  | "agent-env"
  | "agent-connectors"
  | "template"
  | "persona"
  | "skill"
  | "mcp"
  | "marketplace";

export interface PaletteHistoryHit {
  kind: PaletteHistoryKind;
  /** Page path, instance id, persona id, skill name, … */
  id: string;
  label: string;
}

export interface PaletteHistoryEntry extends PaletteHistoryHit {
  /** Epoch milliseconds; ties are broken by array order (newest first). */
  at: number;
}

export const PALETTE_HISTORY_KEY = "fleett.palette.history";
export const PALETTE_HISTORY_LIMIT = 5;

const KINDS: ReadonlySet<string> = new Set<PaletteHistoryKind>([
  "page",
  "agent",
  "agent-logs",
  "agent-files",
  "agent-env",
  "agent-connectors",
  "template",
  "persona",
  "skill",
  "mcp",
  "marketplace",
]);

/** `kind:id`, the dedupe key shared with the live command rows. */
export function paletteHistoryKey(hit: Pick<PaletteHistoryHit, "kind" | "id">): string {
  return `${hit.kind}:${hit.id}`;
}

function isEntry(value: unknown): value is PaletteHistoryEntry {
  if (typeof value !== "object" || value === null) return false;
  const entry = value as Partial<PaletteHistoryEntry>;
  return (
    typeof entry.kind === "string" &&
    KINDS.has(entry.kind) &&
    typeof entry.id === "string" &&
    entry.id.length > 0 &&
    typeof entry.label === "string" &&
    entry.label.length > 0 &&
    typeof entry.at === "number" &&
    Number.isFinite(entry.at)
  );
}

/**
 * Destination a stored hit replays. Kept next to the storage format so a
 * recent row never depends on the query that produced it: `/templates` is
 * still reachable on an empty palette even though the page row only appears
 * once the operator types.
 */
export function paletteHistoryPath(
  hit: Pick<PaletteHistoryHit, "kind" | "id">,
): string | null {
  const id = encodeURIComponent(hit.id);
  switch (hit.kind) {
    case "page":
      return hit.id.startsWith("/") ? hit.id : null;
    case "agent":
      return `/instances/${id}`;
    case "agent-logs":
      return `/instances/${id}?tab=activity&view=logs`;
    case "agent-files":
      return `/instances/${id}?tab=files`;
    case "agent-env":
      return `/instances/${id}?tab=configure&focus=env`;
    case "agent-connectors":
      return `/instances/${id}?tab=advanced&section=connectors`;
    case "template":
      return "/library?type=templates";
    case "persona":
      return `/personas/${id}`;
    case "skill":
      return `/skills/${id}`;
    case "mcp":
      return `/mcp-servers/${id}`;
    case "marketplace":
      return "/library?type=marketplace";
    default:
      return null;
  }
}

/** Newest first, deduped, capped. Malformed or foreign values are dropped. */
export function readPaletteHistory(): PaletteHistoryEntry[] {
  try {
    const raw = localStorage.getItem(PALETTE_HISTORY_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const seen = new Set<string>();
    const entries: PaletteHistoryEntry[] = [];
    for (const value of parsed) {
      if (!isEntry(value)) continue;
      const key = paletteHistoryKey(value);
      if (seen.has(key)) continue;
      seen.add(key);
      entries.push(value);
      if (entries.length === PALETTE_HISTORY_LIMIT) break;
    }
    return entries;
  } catch {
    // Private mode, blocked storage or a hand-edited value: recents stay empty
    // rather than breaking the palette.
    return [];
  }
}

function write(entries: PaletteHistoryEntry[]): void {
  try {
    localStorage.setItem(PALETTE_HISTORY_KEY, JSON.stringify(entries));
  } catch {
    /* ignore storage failures */
  }
}

/** Moves one successful hit to the top and returns the new list. */
export function rememberPaletteHit(hit: PaletteHistoryHit): PaletteHistoryEntry[] {
  const key = paletteHistoryKey(hit);
  const previous = readPaletteHistory().filter((entry) => paletteHistoryKey(entry) !== key);
  const next: PaletteHistoryEntry[] = [
    { kind: hit.kind, id: hit.id, label: hit.label, at: Date.now() },
    ...previous,
  ].slice(0, PALETTE_HISTORY_LIMIT);
  write(next);
  return next;
}

/** Drops the stored list; the palette re-renders its Recents group empty. */
export function clearPaletteHistory(): void {
  try {
    localStorage.removeItem(PALETTE_HISTORY_KEY);
  } catch {
    /* ignore storage failures */
  }
}
