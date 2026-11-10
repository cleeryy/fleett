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
 * The assistant provider can be picked from the assistant panel or from the
 * Providers page. Whichever saves first broadcasts the resolved config so the
 * other surface does not keep showing the previous state.
 */
export const ASSISTANT_CONFIG_EVENT = "fleett:assistant-config";
