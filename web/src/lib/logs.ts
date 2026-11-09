import type { LogEntry, LogLevel } from "@/api/types";

/**
 * Log console helpers, kept out of React so the SSE decoder, the ordering and
 * the filter can be reasoned about on their own.
 *
 * The backend emits `info`, `warn` and `error`. The console also offers
 * `debug` as a threshold: it keeps everything, including a future `debug`
 * level, which is what an operator expects from the most verbose choice.
 */
export type LogFilterLevel = LogLevel | "debug";

const SEVERITY: Record<LogLevel, number> = { info: 0, warn: 1, error: 2 };
const THRESHOLD: Record<LogFilterLevel, number> = { debug: -1, info: 0, warn: 1, error: 2 };

/** `at` ascending, then id, so two lines in the same second stay ordered. */
export function compareLogEntries(left: LogEntry, right: LogEntry): number {
  const leftAt = Date.parse(left.at);
  const rightAt = Date.parse(right.at);
  if (Number.isNaN(leftAt) || Number.isNaN(rightAt)) return left.id - right.id;
  return leftAt - rightAt || left.id - right.id;
}

/** Case-insensitive substring on the message, then the minimum level. */
export function matchesLogFilter(
  entry: LogEntry,
  needle: string,
  level: LogFilterLevel,
): boolean {
  if (SEVERITY[entry.level] < THRESHOLD[level]) return false;
  const query = needle.trim().toLowerCase();
  if (!query) return true;
  return entry.message.toLowerCase().includes(query);
}

/**
 * Dedupe by id and keep the newest `limit` lines. The SSE stream and the
 * polling fallback overlap on purpose (that is how a gap is closed), so every
 * merge has to tolerate the same line twice.
 */
export function mergeLogEntries(
  previous: LogEntry[],
  incoming: LogEntry[],
  limit: number,
): LogEntry[] {
  if (incoming.length === 0) return previous;
  const byId = new Map<number, LogEntry>();
  for (const entry of previous) byId.set(entry.id, entry);
  for (const entry of incoming) byId.set(entry.id, entry);
  const merged = [...byId.values()].sort(compareLogEntries);
  return merged.length > limit ? merged.slice(merged.length - limit) : merged;
}

/** One clipboard line: timestamp, level, message, in the console's own order. */
export function formatLogLine(entry: LogEntry): string {
  return `${entry.at} ${entry.level.toUpperCase()} ${entry.message}`;
}

export function logsToText(entries: LogEntry[]): string {
  return entries.map(formatLogLine).join("\n");
}

/** Largest timestamp in `entries`, or `fallback` when there is none. */
export function latestLogAt(entries: LogEntry[], fallback: string | null = null): string | null {
  let latest = fallback;
  let latestMs = fallback === null ? Number.NEGATIVE_INFINITY : Date.parse(fallback);
  for (const entry of entries) {
    const at = Date.parse(entry.at);
    if (Number.isNaN(at)) continue;
    if (at > latestMs) {
      latest = entry.at;
      latestMs = at;
    }
  }
  return latest;
}

export interface SseFrame {
  event: string;
  data: string;
}

/**
 * Incremental `text/event-stream` decoder for a `fetch` body.
 *
 * `EventSource` cannot send the bearer header, so the console reads the body
 * itself and splits it here. Chunk boundaries may fall anywhere, including
 * inside a multibyte character (the caller decodes with `{ stream: true }`) or
 * between `\r` and `\n`; comments (`: heartbeat`) and unknown fields are
 * ignored.
 */
export class SseParser {
  private buffer = "";
  private event = "";
  private data: string[] = [];

  push(chunk: string): SseFrame[] {
    this.buffer += chunk.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
    const frames: SseFrame[] = [];
    for (;;) {
      const boundary = this.buffer.indexOf("\n\n");
      if (boundary === -1) return frames;
      const block = this.buffer.slice(0, boundary);
      this.buffer = this.buffer.slice(boundary + 2);
      const frame = this.consume(block);
      if (frame) frames.push(frame);
    }
  }

  /** Dispatch a final frame when the server closes without a blank line. */
  flush(): SseFrame[] {
    if (!this.buffer.trim()) {
      this.reset();
      return [];
    }
    const frame = this.consume(this.buffer);
    this.buffer = "";
    return frame ? [frame] : [];
  }

  private consume(block: string): SseFrame | null {
    for (const line of block.split("\n")) {
      if (!line || line.startsWith(":")) continue;
      const colon = line.indexOf(":");
      const field = colon === -1 ? line : line.slice(0, colon);
      let value = colon === -1 ? "" : line.slice(colon + 1);
      if (value.startsWith(" ")) value = value.slice(1);
      if (field === "event") this.event = value;
      else if (field === "data") this.data.push(value);
    }
    if (this.data.length === 0) {
      this.event = "";
      return null;
    }
    const frame = { event: this.event || "message", data: this.data.join("\n") };
    this.reset();
    return frame;
  }

  private reset(): void {
    this.event = "";
    this.data = [];
  }
}

/** `new Date(...).toLocaleTimeString` without the locale surprises. */
export function logClock(at: string): string {
  const date = new Date(at);
  if (Number.isNaN(date.getTime())) return "--:--:--";
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** Local calendar day (`YYYY-MM-DD`), used for the dividers of a long tail. */
export function logDay(at: string): string {
  const date = new Date(at);
  if (Number.isNaN(date.getTime())) return at;
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}
