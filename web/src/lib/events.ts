/**
 * Lifecycle actions can start from the command palette, outside any screen.
 * The list and detail pages listen for this to refresh instead of showing a
 * status the operator already changed.
 */
export const INSTANCES_CHANGED_EVENT = "fleett:instances-changed";

export function notifyInstancesChanged(): void {
  window.dispatchEvent(new Event(INSTANCES_CHANGED_EVENT));
}

/**
 * The assistant provider can be changed from the panel or from the Providers
 * page; both surfaces reload the resolved config instead of showing a stale
 * provider.
 */
export const ASSISTANT_CONFIG_EVENT = "fleett:assistant-config";

export function notifyAssistantConfigChanged(): void {
  window.dispatchEvent(new Event(ASSISTANT_CONFIG_EVENT));
}
