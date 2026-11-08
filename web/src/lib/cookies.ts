/**
 * Read a non-HttpOnly cookie by name. Used for the CSRF double-submit header:
 * the controller sets `fleett_csrf` and expects it echoed on every mutation.
 */
export function readCookie(name: string): string | null {
  const source = typeof document === "undefined" ? "" : document.cookie;
  if (!source) return null;
  for (const part of source.split(";")) {
    const trimmed = part.trim();
    if (!trimmed) continue;
    const index = trimmed.indexOf("=");
    if (index === -1) continue;
    if (trimmed.slice(0, index) !== name) continue;
    const value = trimmed.slice(index + 1);
    try {
      return decodeURIComponent(value);
    } catch {
      return value;
    }
  }
  return null;
}
