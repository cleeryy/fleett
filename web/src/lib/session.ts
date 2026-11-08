import { createContext, useContext } from "react";
import type { Identity } from "@/api/types";

/**
 * Identity of the signed-in request, provided by the app once `/api/auth/me`
 * answers. Pages read it to hide mutations they know the role cannot perform;
 * the controller remains the source of truth (403 is still handled).
 */
export const SessionContext = createContext<Identity | null>(null);

export function useIdentity(): Identity | null {
  return useContext(SessionContext);
}

/** Viewer accounts are read-only; every other identity may mutate. */
export function useCanEdit(): boolean {
  return useContext(SessionContext)?.role !== "viewer";
}

/** Name first, then e-mail, then the raw actor (a token name for bearer clients). */
export function identityLabel(identity: Identity): string {
  const name = identity.user?.name?.trim();
  if (name) return name;
  const email = identity.user?.email?.trim();
  if (email) return email;
  return identity.actor;
}
