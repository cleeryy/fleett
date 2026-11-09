/** Apple platforms show the command glyph; everything else spells out Ctrl. */
function detectApple(): boolean {
  if (typeof navigator === "undefined") return false;
  const data = (navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData;
  const value = data?.platform || navigator.platform || navigator.userAgent;
  return /mac|iphone|ipad|ipod/i.test(value);
}

export const IS_APPLE = detectApple();

/** Label shown in buttons: `⌘K` or `Ctrl+K`. */
export const COMMAND_KEY_LABEL = IS_APPLE ? "⌘K" : "Ctrl+K";

/** `aria-keyshortcuts` value for the same shortcut. */
export const COMMAND_KEY_ARIA = "Meta+K Control+K";
