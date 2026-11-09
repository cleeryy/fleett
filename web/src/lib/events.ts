/**
 * Lifecycle actions can start from the command palette, outside any screen.
 * The list and detail pages listen for this to refresh instead of showing a
 * status the operator already changed.
 */
export const INSTANCES_CHANGED_EVENT = "fleett:instances-changed";

export function notifyInstancesChanged(): void {
  window.dispatchEvent(new Event(INSTANCES_CHANGED_EVENT));
}
