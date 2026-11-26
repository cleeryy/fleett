import type { AgentDraft } from "@/lib/agent-draft";

/** Shared shape every step receives from the wizard orchestrator. */
export interface StepFields {
  draft: AgentDraft;
  /** Merge a validated patch and mark the draft as user-edited. */
  patch: (changes: Partial<AgentDraft>) => void;
  /** Derive the next draft from the current one, user-edited. */
  patchWith: (updater: (current: AgentDraft) => AgentDraft) => void;
  errors: Record<string, string>;
  showErrors: boolean;
}

/**
 * One visible step title, also the focus target after a step change. `id` is
 * stable so the orchestrator can move focus without prop plumbing.
 */
export function StepHeading({ title, description }: { title: string; description?: string }) {
  return (
    <div className="space-y-1">
      <h2
        id="wizard-step-heading"
        tabIndex={-1}
        className="rounded-sm text-lg font-semibold tracking-tight outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
      >
        {title}
      </h2>
      {description ? <p className="text-sm text-muted-foreground">{description}</p> : null}
    </div>
  );
}

export function FieldError({ id, message }: { id: string; message?: string }) {
  if (!message) return null;
  return (
    <p id={id} className="text-xs text-destructive">
      {message}
    </p>
  );
}
