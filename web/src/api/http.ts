import { ApiError } from "./errors";
import { tokenStore } from "@/lib/token";
import type {
  CreateInstanceInput,
  FleetApi,
  Health,
  InstanceDetail,
  InstanceSummary,
  LifecycleResult,
  PatchInstanceInput,
  Share,
  ShareMode,
  Template,
} from "./types";

/** Empty base = same origin. Vite proxies /api to the controller in development. */
const BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? "";

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const token = tokenStore.get();
  const headers = new Headers(init.headers);
  if (token) headers.set("Authorization", `Bearer ${token}`);
  if (init.body !== undefined && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }

  let response: Response;
  try {
    response = await fetch(`${BASE}${path}`, { ...init, headers });
  } catch {
    throw new ApiError(0, "network_error", "Controller unreachable. Check that the backend is running.");
  }

  if (!response.ok) {
    let code = "http_error";
    let message = response.statusText || `HTTP ${response.status}`;
    const contentType = response.headers.get("content-type") ?? "";
    if (contentType.includes("application/json")) {
      const body = (await response.json().catch(() => null)) as {
        error?: { code?: string; message?: string };
      } | null;
      if (body?.error?.code) code = body.error.code;
      if (body?.error?.message) message = body.error.message;
    }
    if (response.status === 401) {
      window.dispatchEvent(new CustomEvent("fleett:unauthorized"));
    }
    throw new ApiError(response.status, code, message);
  }

  if (response.status === 204) return undefined as T;
  const contentType = response.headers.get("content-type") ?? "";
  if (contentType.includes("application/json")) return (await response.json()) as T;
  return (await response.text()) as T;
}

export const httpApi: FleetApi = {
  health: () => request<Health>("/api/health"),

  checkAuth: () => request<void>("/api/auth/check"),

  listTemplates: () => request<Template[]>("/api/templates"),

  listInstances: () => request<InstanceSummary[]>("/api/instances"),

  getInstance: (id) => request<InstanceDetail>(`/api/instances/${encodeURIComponent(id)}`),

  createInstance: (input: CreateInstanceInput) =>
    request<InstanceDetail>("/api/instances", { method: "POST", body: JSON.stringify(input) }),

  patchInstance: (id, input: PatchInstanceInput) =>
    request<InstanceDetail>(`/api/instances/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  action: (id, action) =>
    request<LifecycleResult>(`/api/instances/${encodeURIComponent(id)}/${action}`, { method: "POST" }),

  deleteInstance: (id, purge) =>
    request<void>(`/api/instances/${encodeURIComponent(id)}${purge ? "?purge=true" : ""}`, {
      method: "DELETE",
    }),

  logs: (id, tail) => request<string>(`/api/instances/${encodeURIComponent(id)}/logs?tail=${tail}`),

  listShares: () => request<Share[]>("/api/shares"),

  createShare: (name) =>
    request<void>("/api/shares", { method: "POST", body: JSON.stringify({ name }) }),

  deleteShare: (name) =>
    request<void>(`/api/shares/${encodeURIComponent(name)}`, { method: "DELETE" }),

  attachShare: (name, instance, mode: ShareMode) =>
    request<void>(`/api/shares/${encodeURIComponent(name)}/attach`, {
      method: "POST",
      body: JSON.stringify({ instance, mode }),
    }),

  detachShare: (name, instance) =>
    request<void>(`/api/shares/${encodeURIComponent(name)}/detach`, {
      method: "POST",
      body: JSON.stringify({ instance }),
    }),
};
