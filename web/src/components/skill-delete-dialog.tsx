import { useEffect, useState } from "react";
import { Loader2, Trash2, TriangleAlert } from "lucide-react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { InstanceSummary, SkillDetail, SkillInUseDetails } from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ErrorAlert } from "@/components/error-alert";
import { plural } from "@/lib/format";
import { hrefFor } from "@/lib/router";

/** `details.instances` of a `409 skill_in_use`: bare ids. */
function inUseInstances(error: ApiError): string[] {
  const details = (error.details ?? null) as Partial<SkillInUseDetails> | null;
  const list = details?.instances;
  if (!Array.isArray(list)) return [];
  return list.filter((entry): entry is string => typeof entry === "string");
}

/**
 * Delete a skill. The controller refuses while an instance still mounts it;
 * that refusal names the instances, and the dialog keeps the list on screen
 * instead of collapsing it into a bare error.
 */
export function SkillDeleteDialog({
  skill,
  instances = [],
  open,
  onOpenChange,
  onDeleted,
}: {
  skill: SkillDetail;
  /** Names next to each id in the 409 report. */
  instances?: InstanceSummary[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDeleted: () => void;
}) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [inUse, setInUse] = useState<string[] | null>(null);

  useEffect(() => {
    if (!open) return;
    setPending(false);
    setError(null);
    setInUse(null);
  }, [open]);

  async function confirm() {
    setPending(true);
    setError(null);
    try {
      await api.deleteSkill(skill.name);
      onDeleted();
    } catch (cause) {
      const failure = toApiError(cause);
      if (failure.status === 409 && failure.code === "skill_in_use") {
        const ids = inUseInstances(failure);
        // A hand-edited reference can leave the details without ids: keep the
        // names the console already knows about rather than an empty list.
        setInUse(ids.length > 0 ? ids : skill.instances);
      } else {
        setError(failure);
      }
    } finally {
      setPending(false);
    }
  }

  const nameFor = (id: string) => instances.find((instance) => instance.id === id)?.name ?? id;

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Delete skill {skill.name}?</DialogTitle>
          <DialogDescription>
            Removes the registry entry and all {skill.versions.length}{" "}
            {plural(skill.versions.length, "version")} from fleett, with its commit history.
            Instances mounting it lose the directory on their next restart.
          </DialogDescription>
        </DialogHeader>

        {inUse !== null ? (
          <div className="min-w-0 space-y-3">
            <Alert className="border-amber-600/40 bg-amber-50/40 dark:bg-amber-950/20">
              <TriangleAlert aria-hidden className="text-amber-600 dark:text-amber-400" />
              <AlertTitle>This skill is still mounted</AlertTitle>
              <AlertDescription>
                <p className="text-sm text-muted-foreground">
                  {inUse.length > 0
                    ? `${inUse.length} ${plural(inUse.length, "instance")} still ${
                        inUse.length === 1 ? "mounts" : "mount"
                      } it. Remove the skill from those instances in Configuration, then delete it.`
                    : "At least one instance still mounts it. Remove it in Configuration, then try again."}
                </p>
              </AlertDescription>
            </Alert>

            {inUse.length > 0 ? (
              <ul className="divide-y rounded-md border">
                {inUse.map((id) => (
                  <li key={id} className="flex items-center justify-between gap-3 px-3 py-2">
                    <a
                      href={hrefFor(`/instances/${id}`)}
                      className="min-w-0 truncate text-sm font-medium underline-offset-4 hover:underline"
                    >
                      {nameFor(id)}
                    </a>
                    <span className="shrink-0 font-mono text-xs text-muted-foreground">{id}</span>
                  </li>
                ))}
              </ul>
            ) : null}
          </div>
        ) : error ? (
          <ErrorAlert error={error} />
        ) : (
          <p className="text-sm text-muted-foreground">
            A pinned instance restarts with the directory missing, so unmount it first. Deletion
            cannot be undone from the console.
          </p>
        )}

        <DialogFooter>
          {inUse !== null ? (
            <Button autoFocus onClick={() => onOpenChange(false)}>
              Close
            </Button>
          ) : (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
                Cancel
              </Button>
              <Button variant="destructive" onClick={() => void confirm()} disabled={pending}>
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Trash2 aria-hidden />}
                Delete skill
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
