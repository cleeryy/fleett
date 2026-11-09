import { useEffect, useRef, useState } from "react";
import { CheckCircle2, Loader2, Send, XCircle } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type {
  InstanceSummary,
  PersonaApplyEntry,
  PersonaSummary,
} from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ErrorAlert } from "@/components/error-alert";
import { PersonaStateBadge } from "@/components/persona-state-badge";
import { PersonaVariableInputs } from "@/components/persona-vars";
import { applySucceeded, compactVariables } from "@/lib/personas";
import { hrefFor } from "@/lib/router";
import { cn } from "@/lib/utils";

/**
 * `POST /api/personas/{id}/apply` on several instances at once. The dialog
 * stays open on a per-instance report so a partial failure is readable.
 */
export function PersonaApplyDialog({
  persona,
  instances,
  open,
  onOpenChange,
  onApplied,
  defaultSelected,
}: {
  persona: PersonaSummary;
  instances: InstanceSummary[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onApplied: () => void;
  /** Ids selected when the dialog opens; defaults to the instances already using it. */
  defaultSelected?: string[];
}) {
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [variables, setVariables] = useState<Record<string, string>>({});
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [missing, setMissing] = useState<string[]>([]);
  const [report, setReport] = useState<PersonaApplyEntry[] | null>(null);
  const opened = useRef(false);

  useEffect(() => {
    if (!open) {
      opened.current = false;
      return;
    }
    // Reset once per opening: a refresh after the report must not wipe it.
    if (opened.current) return;
    opened.current = true;
    const initial =
      defaultSelected ?? instances.filter((row) => row.persona === persona.id).map((row) => row.id);
    setSelected(new Set(initial));
    setVariables({});
    setPending(false);
    setError(null);
    setMissing([]);
    setReport(null);
  }, [open, defaultSelected, instances, persona.id]);

  function toggle(id: string) {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
    setReport(null);
  }

  async function submit() {
    if (selected.size === 0) return;
    setPending(true);
    setError(null);
    setMissing([]);
    try {
      const result = await api.applyPersona(persona.id, {
        instances: [...selected],
        variables: compactVariables(variables),
      });
      setReport(result.results);
      const failed = result.results.filter((entry) => !applySucceeded(entry.status)).length;
      if (failed === 0) {
        toast.success(
          `Persona applied to ${result.results.length} ${
            result.results.length === 1 ? "instance" : "instances"
          }.`,
        );
      } else {
        toast.warning(`${failed} of ${result.results.length} instances could not take the persona.`);
      }
      onApplied();
    } catch (cause) {
      const failure = toApiError(cause);
      if (failure.code === "missing_persona_variables") {
        const details = (failure.details ?? null) as { missing?: unknown } | null;
        const list = details?.missing;
        setMissing(
          Array.isArray(list) ? list.filter((key): key is string => typeof key === "string") : [],
        );
      }
      setError(failure);
    } finally {
      setPending(false);
    }
  }

  const selectedCount = selected.size;

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh_-_2rem)] grid-rows-[auto_minmax(0,1fr)] sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            Apply <span className="font-mono">{persona.name}</span>
          </DialogTitle>
          <DialogDescription>
            Rewrites <span className="font-mono">data/SOUL.md</span>, snapshots the previous file and
            restarts each selected instance. A failed instance is reported, never silently skipped.
          </DialogDescription>
        </DialogHeader>

        {report === null ? (
          <>
            <div className="-mr-2 min-h-0 space-y-4 overflow-y-auto pr-2">
              <div className="space-y-2">
                <div className="flex items-center justify-between gap-2">
                  <p className="text-sm font-medium">Instances</p>
                  <div className="flex items-center gap-2 text-xs">
                    <button
                      type="button"
                      className="underline-offset-4 hover:underline disabled:opacity-50"
                      disabled={selectedCount === instances.length}
                      onClick={() => setSelected(new Set(instances.map((row) => row.id)))}
                    >
                      Select all
                    </button>
                    <span aria-hidden className="text-muted-foreground">
                      ·
                    </span>
                    <button
                      type="button"
                      className="underline-offset-4 hover:underline disabled:opacity-50"
                      disabled={selectedCount === 0}
                      onClick={() => setSelected(new Set())}
                    >
                      Clear
                    </button>
                  </div>
                </div>

                {instances.length === 0 ? (
                  <p className="rounded-md border px-3 py-4 text-sm text-muted-foreground">
                    No instance exists yet. Create one before applying a persona.
                  </p>
                ) : (
                  <div className="divide-y rounded-md border">
                    {instances.map((instance) => {
                      const checked = selected.has(instance.id);
                      return (
                        <label
                          key={instance.id}
                          className={cn(
                            "flex cursor-pointer items-start gap-3 px-3 py-2.5",
                            checked ? "bg-muted/40" : "hover:bg-muted/20",
                          )}
                        >
                          <Checkbox
                            checked={checked}
                            className="mt-0.5"
                            onCheckedChange={() => toggle(instance.id)}
                            aria-label={`Apply to ${instance.name}`}
                          />
                          <span className="min-w-0 flex-1 space-y-1">
                            <span className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                              <span className="truncate text-sm font-medium">{instance.name}</span>
                              <span className="font-mono text-xs text-muted-foreground">
                                {instance.id}
                              </span>
                            </span>
                            <span className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
                              {instance.persona === persona.id ? (
                                <PersonaStateBadge state={instance.persona_state} />
                              ) : instance.persona ? (
                                <span>
                                  uses <span className="font-mono">{instance.persona}</span>
                                </span>
                              ) : (
                                <span>no persona</span>
                              )}
                              {instance.status !== "running" ? (
                                <Badge
                                  variant="outline"
                                  className="font-mono text-[11px] font-normal text-muted-foreground"
                                >
                                  {instance.status}
                                </Badge>
                              ) : null}
                            </span>
                          </span>
                        </label>
                      );
                    })}
                  </div>
                )}

                {persona.variables.length > 0 ? (
                  <details className="rounded-md border">
                    <summary className="cursor-pointer px-3 py-2.5 text-sm">
                      Variable overrides{" "}
                      <span className="text-xs text-muted-foreground">(optional)</span>
                    </summary>
                    <div className="space-y-2 px-3 pb-3">
                      <p className="text-xs text-muted-foreground">
                        Merged over each instance's stored values for this apply. Leave empty to keep
                        them and use the persona defaults.
                      </p>
                      <PersonaVariableInputs
                        variables={persona.variables}
                        values={variables}
                        onChange={setVariables}
                        idPrefix="apply-var"
                        disabled={pending}
                      />
                    </div>
                  </details>
                ) : null}

                {missing.length > 0 ? (
                  <Alert className="border-amber-600/40 bg-amber-50/40 dark:bg-amber-950/20">
                    <AlertTitle>Values required</AlertTitle>
                    <AlertDescription>
                      <p className="text-sm text-muted-foreground">
                        The template uses {missing.map((key) => `{{${key}}}`).join(", ")} with no
                        value and no default. Set an override above, or add defaults to the persona.
                      </p>
                    </AlertDescription>
                  </Alert>
                ) : null}

                {error ? <ErrorAlert error={error} /> : null}
              </div>
            </div>

            <DialogFooter className="border-t pt-4">
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={pending}
              >
                Cancel
              </Button>
              <Button type="button" onClick={() => void submit()} disabled={pending || selectedCount === 0}>
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Send aria-hidden />}
                Apply to {selectedCount || "…"} {selectedCount === 1 ? "instance" : "instances"}
              </Button>
            </DialogFooter>
          </>
        ) : (
          <div className="min-h-0 space-y-4 overflow-y-auto">
            <div className="space-y-2">
              <p className="text-sm font-medium">Report</p>
              {report.length === 0 ? (
                <p className="rounded-md border px-3 py-4 text-sm text-muted-foreground">
                  Nothing was applied.
                </p>
              ) : (
                <ul className="divide-y rounded-md border">
                  {report.map((entry) => {
                    const ok = applySucceeded(entry.status);
                    return (
                      <li key={entry.id} className="space-y-1 px-3 py-2.5">
                        <div className="flex flex-wrap items-center justify-between gap-2">
                          <a
                            href={hrefFor(`/instances/${entry.id}`)}
                            className="min-w-0 truncate font-mono text-sm underline-offset-4 hover:underline"
                          >
                            {entry.id}
                          </a>
                          <span className="flex items-center gap-1.5">
                            {ok ? (
                              <CheckCircle2 aria-hidden className="size-3.5 text-emerald-500" />
                            ) : (
                              <XCircle aria-hidden className="size-3.5 text-destructive" />
                            )}
                            <Badge
                              variant={ok ? "secondary" : "destructive"}
                              className="font-mono text-xs font-normal"
                            >
                              {entry.status}
                            </Badge>
                          </span>
                        </div>
                        {entry.error ? (
                          <p className="font-mono text-xs break-words whitespace-pre-wrap text-destructive">
                            {entry.error}
                          </p>
                        ) : null}
                      </li>
                    );
                  })}
                </ul>
              )}
              {report.some((entry) => !applySucceeded(entry.status)) ? (
                <Alert className="border-amber-600/40 bg-amber-50/40 dark:bg-amber-950/20">
                  <AlertTitle>Some instances kept their old SOUL.md</AlertTitle>
                  <AlertDescription>
                    <p className="text-sm text-muted-foreground">
                      Fix the error above, then apply again. The other instances are already on the
                      persona.
                    </p>
                  </AlertDescription>
                </Alert>
              ) : null}
            </div>

            <DialogFooter>
              <Button autoFocus onClick={() => onOpenChange(false)}>
                Done
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
