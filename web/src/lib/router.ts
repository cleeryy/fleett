import { useSyncExternalStore } from "react";

export interface Route {
  path: string;
  query: URLSearchParams;
}

function readHash(): string {
  const raw = window.location.hash.replace(/^#/, "");
  return raw === "" ? "/" : raw;
}

let cachedKey = "\u0000";
let cachedRoute: Route = { path: "/", query: new URLSearchParams() };

function snapshot(): Route {
  const key = readHash();
  if (key !== cachedKey) {
    const queryIndex = key.indexOf("?");
    const path = queryIndex === -1 ? key : key.slice(0, queryIndex);
    const search = queryIndex === -1 ? "" : key.slice(queryIndex + 1);
    cachedKey = key;
    cachedRoute = { path: path || "/", query: new URLSearchParams(search) };
  }
  return cachedRoute;
}

const listeners = new Set<() => void>();

function emit(): void {
  for (const listener of listeners) listener();
}

/**
 * Navigation guard for unsaved forms. Returning false keeps the current route:
 * a direct hash change is reverted before subscribers see it, so the page is
 * not unmounted (useSyncExternalStore flushes synchronously).
 */
let routeGuard: (() => boolean) | null = null;
let appliedHash = window.location.hash;
let bypassGuard = false;
let attached = false;

export function setRouteGuard(guard: (() => boolean) | null): void {
  routeGuard = guard;
}

function handleHashChange(): void {
  if (bypassGuard) {
    bypassGuard = false;
    appliedHash = window.location.hash;
    emit();
    return;
  }

  if (routeGuard && !routeGuard()) {
    if (window.location.hash !== appliedHash) {
      window.history.replaceState(null, "", appliedHash || "#/");
    }
    return;
  }

  appliedHash = window.location.hash;
  emit();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  if (!attached) {
    window.addEventListener("hashchange", handleHashChange);
    attached = true;
  }
  return () => {
    listeners.delete(listener);
  };
}

export function useRoute(): Route {
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

export function navigate(to: string, options: { replace?: boolean } = {}): void {
  const path = to.startsWith("/") ? to : `/${to}`;
  const target = `#${path}`;

  if (routeGuard && !routeGuard()) return;

  if (options.replace) {
    window.history.replaceState(null, "", target);
    appliedHash = target;
    emit();
    return;
  }

  if (window.location.hash === target) {
    emit();
    return;
  }

  bypassGuard = true;
  window.location.hash = target;
}

export function hrefFor(path: string): string {
  return `#${path.startsWith("/") ? path : `/${path}`}`;
}
