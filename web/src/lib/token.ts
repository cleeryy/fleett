const KEY = "fleett.token";
// Key written before the rename; still read so an open tab stays signed in.
const LEGACY_KEY = "fleet.token";

/** Bearer token for the control plane, scoped to the browser tab. */
export const tokenStore = {
  get(): string | null {
    try {
      return sessionStorage.getItem(KEY) ?? sessionStorage.getItem(LEGACY_KEY);
    } catch {
      return null;
    }
  },
  set(token: string): void {
    try {
      sessionStorage.setItem(KEY, token);
      sessionStorage.removeItem(LEGACY_KEY);
    } catch {
      /* private mode: the token just won't survive a reload */
    }
  },
  clear(): void {
    try {
      sessionStorage.removeItem(KEY);
      sessionStorage.removeItem(LEGACY_KEY);
    } catch {
      /* ignore */
    }
  },
};
