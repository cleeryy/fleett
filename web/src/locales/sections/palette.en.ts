/** Command palette lane (#76). */
export const paletteEn: Record<string, string> = {
  // ── Groups ────────────────────────────────────────────────────────────
  "palette.group.recents": "Recent",
  "palette.group.agents": "Agents",
  "palette.group.pages": "Pages",
  "palette.group.library": "Library",
  "palette.group.actions": "Actions",

  // ── States ────────────────────────────────────────────────────────────
  "palette.empty": "Type a name, a page or an action.",
  "palette.loadingLibrary": "Loading the library…",
  "palette.loadingSkill": "Loading skill {name}…",
  "palette.badgeAdvanced": "Advanced",

  // ── Agent rows ────────────────────────────────────────────────────────
  "palette.connectors": "Connectors for {name}",
  "palette.deleteAgent": "Delete {name}",

  // ── Library rows ──────────────────────────────────────────────────────
  "palette.deleteSkill": "Delete skill {name}",

  // ── Actions ───────────────────────────────────────────────────────────
  "palette.createAgent": "Create an agent",
  "palette.testProvider": "Test the connection to {name}",
  "palette.providerTesting": "Testing {name}…",
  "palette.providerOk": "{name}: connection OK.",
  "palette.providerFailed": "{name}: connection failed. Check the key and the endpoint.",
  "palette.viewIncidents": "Open incidents",
  "palette.switchToAdvanced": "Switch to Advanced mode",
  "palette.switchToSimple": "Switch to Simple mode",

  // ── Destructive confirm and history ───────────────────────────────────
  "palette.confirmDelete": "Press Enter again to confirm. Esc cancels.",
  "palette.historyClear": "Clear history",
  "palette.historyCleared": "Search history cleared.",

  // ── Footer hints ──────────────────────────────────────────────────────
  "palette.hintClose": "Close",
  "palette.hintConfirm": "Confirm",
  "palette.hintCancel": "Cancel",
};
