export const SLUG_RE = /^[a-z0-9][a-z0-9-]{0,30}[a-z0-9]$/;

export function isValidSlug(value: string): boolean {
  return SLUG_RE.test(value);
}

/**
 * Provider names follow the controller grammar (`[a-z0-9-]{2,63}`, starting
 * with a letter or digit). Unlike an instance id, a trailing hyphen is allowed.
 */
export const PROVIDER_NAME_RE = /^[a-z0-9][a-z0-9-]{1,62}$/;

export function isValidProviderName(value: string): boolean {
  return PROVIDER_NAME_RE.test(value);
}

/** Extensions the instance editor accepts, mirroring `docs/FILES.md`. */
export const EDITABLE_EXTENSIONS = ["md", "yaml", "yml", "json", "txt", "toml"] as const;

/** Human list for banners and hints. */
export const EDITABLE_EXTENSIONS_LABEL = ".md, .yaml, .yml, .json, .txt, .toml";

export function isEditableFileName(name: string): boolean {
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return false;
  return (EDITABLE_EXTENSIONS as readonly string[]).includes(name.slice(dot + 1).toLowerCase());
}

export function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}
