export const SLUG_RE = /^[a-z0-9][a-z0-9-]{0,30}[a-z0-9]$/;

export function isValidSlug(value: string): boolean {
  return SLUG_RE.test(value);
}

export function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}
