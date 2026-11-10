import { useEffect, useSyncExternalStore } from "react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { MarketplaceUpdates } from "@/api/types";

/**
 * One upstream-check shared by every screen.
 *
 * `GET /api/marketplace/updates` probes each provider, so the console fetches
 * it once on first mount and reuses the answer for the Skills list, the badge
 * on a skill and the Marketplace banner. A mutation calls
 * [`refreshMarketplaceUpdates`] instead of each page fetching again.
 */
export interface MarketplaceUpdatesState {
  data: MarketplaceUpdates | null;
  error: ApiError | null;
  loading: boolean;
}

let snapshot: MarketplaceUpdatesState = { data: null, error: null, loading: false };
let started = false;
let again = false;
let inflight: Promise<void> | null = null;
const listeners = new Set<() => void>();

function publish(next: MarketplaceUpdatesState): void {
  snapshot = next;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function getSnapshot(): MarketplaceUpdatesState {
  return snapshot;
}

/** Start a probe, or queue one when a probe is already running. */
export function refreshMarketplaceUpdates(): void {
  if (inflight) {
    again = true;
    return;
  }
  publish({ ...snapshot, loading: true });
  inflight = api.marketplaceUpdates().then(
    (data) => {
      publish({ data, error: null, loading: false });
    },
    (cause: unknown) => {
      publish({ ...snapshot, error: toApiError(cause), loading: false });
    },
  );
  void inflight.finally(() => {
    inflight = null;
    if (!again) return;
    again = false;
    refreshMarketplaceUpdates();
  });
}

export function useMarketplaceUpdates(): MarketplaceUpdatesState {
  const state = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  useEffect(() => {
    if (started) return;
    started = true;
    refreshMarketplaceUpdates();
  }, []);
  return state;
}
