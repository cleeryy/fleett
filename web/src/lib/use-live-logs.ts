import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { LogEntry } from "@/api/types";
import {
  compareLogEntries,
  latestLogAt,
  mergeLogEntries,
} from "@/lib/logs";

/**
 * Live feed for one instance: the last page from the log center, then the SSE
 * stream, with a one-second polling fallback whenever the stream is down.
 *
 * The hook owns the network and the buffer; follow/pause and scroll anchoring
 * are DOM concerns left to the panel. It is deliberately one effect per
 * connection so a re-render (filter, scroll) can never stack a subscription.
 */
export type LiveLogStatus = "live" | "paused" | "reconnecting" | "error";

export interface LiveLogsOptions {
  /** Defaults to true; false parks the feed without tearing the buffer down. */
  active?: boolean;
  initialLimit?: number;
  maxLines?: number;
}

export interface LiveLogsResult {
  /** Oldest first, newest last, at most `maxLines`. */
  lines: LogEntry[];
  status: LiveLogStatus;
  error: ApiError | null;
  /** True until the first page lands (or fails). */
  loading: boolean;
  /** Monotonic count of lines received, used by the "new lines" pill. */
  received: number;
  clear: () => void;
  retry: () => void;
}

const POLL_INTERVAL = 1_000;
const POLL_LIMIT = 500;
const MAX_BACKOFF = 30_000;
const IDLE_TIMEOUT = 30_000;

export function useLiveLogs(instanceId: string, options: LiveLogsOptions = {}): LiveLogsResult {
  const { active = true, initialLimit = 500, maxLines = 2_000 } = options;

  const [lines, setLinesState] = useState<LogEntry[]>([]);
  const [status, setStatus] = useState<LiveLogStatus>("reconnecting");
  const [error, setError] = useState<ApiError | null>(null);
  const [loading, setLoading] = useState(true);
  const [received, setReceived] = useState(0);
  const [loadedId, setLoadedId] = useState<string | null>(null);
  const [retryNonce, setRetryNonce] = useState(0);
  const [hidden, setHidden] = useState(() => document.hidden);

  const linesRef = useRef<LogEntry[]>([]);
  const knownRef = useRef<Set<number>>(new Set());
  const pendingRef = useRef<LogEntry[]>([]);
  const cursorRef = useRef<string | null>(null);
  const receivedRef = useRef(0);
  const rafRef = useRef<number | null>(null);
  const pollBusyRef = useRef(false);
  const streamLiveRef = useRef(false);

  useEffect(() => {
    const onChange = () => setHidden(document.hidden);
    document.addEventListener("visibilitychange", onChange);
    return () => document.removeEventListener("visibilitychange", onChange);
  }, []);

  const flush = useCallback(() => {
    rafRef.current = null;
    if (pendingRef.current.length === 0) return;
    const pending = pendingRef.current;
    pendingRef.current = [];
    const merged = mergeLogEntries(linesRef.current, pending, maxLines);
    linesRef.current = merged;
    knownRef.current = new Set(merged.map((entry) => entry.id));
    cursorRef.current = latestLogAt(merged, cursorRef.current);
    receivedRef.current += pending.length;
    setLinesState(merged);
    setReceived(receivedRef.current);
  }, [maxLines]);

  const accept = useCallback(
    (entries: LogEntry[]) => {
      let added = 0;
      for (const entry of entries) {
        if (!entry || typeof entry.id !== "number" || Number.isNaN(entry.id)) continue;
        if (knownRef.current.has(entry.id)) continue;
        knownRef.current.add(entry.id);
        pendingRef.current.push(entry);
        added += 1;
      }
      if (added === 0) return;
      if (rafRef.current === null) {
        rafRef.current = window.requestAnimationFrame(flush);
      }
    },
    [flush],
  );

  // The first page. A retry starts here again; the stream only runs once this
  // page has landed, so `since` always has a cursor to resume from.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    setStatus(document.hidden ? "paused" : "reconnecting");
    setLoadedId(null);
    linesRef.current = [];
    knownRef.current = new Set();
    pendingRef.current = [];
    cursorRef.current = null;
    receivedRef.current = 0;
    streamLiveRef.current = false;
    setLinesState([]);
    setReceived(0);
    if (rafRef.current !== null) {
      window.cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }

    void api
      .instanceLogs(instanceId, { limit: initialLimit })
      .then((page) => {
        if (cancelled) return;
        const sorted = [...page.entries].sort(compareLogEntries);
        const merged = mergeLogEntries([], sorted, maxLines);
        linesRef.current = merged;
        knownRef.current = new Set(merged.map((entry) => entry.id));
        cursorRef.current = latestLogAt(merged);
        setLinesState(merged);
        setLoading(false);
        setLoadedId(instanceId);
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setLoading(false);
        setError(toApiError(cause));
        setStatus("error");
      });

    return () => {
      cancelled = true;
    };
  }, [instanceId, retryNonce, initialLimit, maxLines]);

  const ready = loadedId === instanceId;

  // One connection at a time. The polling timer is the fallback that keeps
  // lines flowing while the stream reconnects; the stream retries on a 1→30 s
  // exponential backoff and every transition goes through this effect.
  useEffect(() => {
    if (!ready || hidden || !active) {
      if (!ready) return;
      streamLiveRef.current = false;
      setStatus("paused");
      return;
    }

    let stopped = false;
    let attempt = 0;
    let controller: AbortController | null = null;
    let idleTimer: number | null = null;
    let pollTimer: number | null = null;
    let pendingConnect: number | null = null;

    const clearIdle = () => {
      if (idleTimer !== null) {
        window.clearTimeout(idleTimer);
        idleTimer = null;
      }
    };

    const armIdle = () => {
      clearIdle();
      idleTimer = window.setTimeout(() => controller?.abort(), IDLE_TIMEOUT);
    };

    const stopPolling = () => {
      if (pollTimer !== null) {
        window.clearInterval(pollTimer);
        pollTimer = null;
      }
    };

    const pollOnce = async () => {
      if (stopped || pollBusyRef.current) return;
      pollBusyRef.current = true;
      try {
        const since = cursorRef.current;
        const page = await api.instanceLogs(instanceId, {
          ...(since ? { since } : {}),
          limit: POLL_LIMIT,
        });
        if (stopped) return;
        accept(page.entries);
        flush();
        if (streamLiveRef.current) return;
        setError(null);
        setStatus("reconnecting");
      } catch (cause) {
        if (stopped || streamLiveRef.current) return;
        setError(toApiError(cause));
        setStatus("error");
      } finally {
        pollBusyRef.current = false;
      }
    };

    const startPolling = () => {
      if (pollTimer !== null) return;
      void pollOnce();
      pollTimer = window.setInterval(() => void pollOnce(), POLL_INTERVAL);
    };

    const connect = async () => {
      if (stopped) return;
      controller = new AbortController();
      armIdle();
      try {
        await api.streamLogs(
          instanceId,
          {
            onOpen: () => {
              if (stopped) return;
              attempt = 0;
              streamLiveRef.current = true;
              stopPolling();
              setError(null);
              setStatus("live");
              armIdle();
              // Close the gap between the last poll and this subscription.
              window.setTimeout(() => void pollOnce(), 250);
            },
            onActivity: () => {
              if (!stopped) armIdle();
            },
            onEntry: (entry) => {
              if (!stopped) accept([entry]);
            },
          },
          controller.signal,
        );
      } catch {
        // Network error, HTTP error, idle timeout or abort-on-visibility: all
        // of them land here. `stopped` below decides whether to reconnect.
      }
      if (stopped) return;
      streamLiveRef.current = false;
      setStatus("reconnecting");
      startPolling();
      const wait = Math.min(MAX_BACKOFF, 1_000 * 2 ** attempt);
      attempt += 1;
      pendingConnect = window.setTimeout(() => void connect(), wait);
    };

    // A reconnect must catch up on what was missed, even when the stream opens
    // on the first try. The initial poll runs now; the open handler runs a
    // second one to close the tiny gap.
    setStatus("reconnecting");
    startPolling();
    void connect();

    return () => {
      stopped = true;
      streamLiveRef.current = false;
      controller?.abort();
      stopPolling();
      clearIdle();
      if (pendingConnect !== null) window.clearTimeout(pendingConnect);
    };
  }, [instanceId, ready, hidden, active, retryNonce, accept, flush]);

  useEffect(() => {
    return () => {
      if (rafRef.current !== null) window.cancelAnimationFrame(rafRef.current);
    };
  }, []);

  const clear = useCallback(() => {
    if (rafRef.current !== null) {
      window.cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
    pendingRef.current = [];
    linesRef.current = [];
    knownRef.current = new Set();
    setLinesState([]);
  }, []);

  const retry = useCallback(() => {
    setLoadedId(null);
    setError(null);
    setRetryNonce((value) => value + 1);
  }, []);

  return { lines, status, error, loading, received, clear, retry };
}
