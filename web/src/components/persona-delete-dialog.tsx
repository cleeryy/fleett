import { useEffect, useState } from "react";
import { Loader2, Trash2, TriangleAlert } from "lucide-react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { InstanceSummary, PersonaInUseDetails, PersonaSummary } from "@/api/types";
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
import { RichText } from "@/components/rich-text";
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";

/** `details.instances` of a `409 persona_in_use`: bare ids. */
function inUseInstances(error: ApiError): string[] {
  const details = (error.details ?? null) as Partial<PersonaInUseDetails> | null;
  const list = details?.instances;
  if (!Array.isArray(list)) return [];
  return list.filter((entry): entry is string => typeof entry === "string");
}

/**
 * Delete a persona. The controller refuses while instances still reference it;
 * the refusal names their ids, and the dialog keeps that list on screen instead
 * of collapsing it into a bare error.
 */
export function PersonaDeleteDialog({
  persona,
  instances = [],
  open,
  onOpenChange,
  onDeleted,
}: {
  persona: PersonaSummary;
  /** Used to show a name next to each id in the 409 report. */
  instances?: InstanceSummary[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDeleted: () => void;
}) {
  const { t } = useI18n();
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
      await api.deletePersona(persona.id);
      onDeleted();
    } catch (cause) {
      const failure = toApiError(cause);
      if (failure.status === 409 && failure.code === "persona_in_use") {
        setInUse(inUseInstances(failure));
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
          <DialogTitle>{t("personas.delete.title", { name: persona.name })}</DialogTitle>
          <DialogDescription>
            <RichText text={t("personas.delete.description")} />
          </DialogDescription>
        </DialogHeader>

        {inUse !== null ? (
          <div className="min-w-0 space-y-3">
            <Alert className="border-amber-600/40 bg-amber-50/40 dark:bg-amber-950/20">
              <TriangleAlert aria-hidden className="text-amber-600 dark:text-amber-400" />
              <AlertTitle>{t("personas.delete.inUseTitle")}</AlertTitle>
              <AlertDescription>
                <p className="text-sm text-muted-foreground">
                  {inUse.length > 0
                    ? t(
                        inUse.length === 1
                          ? "personas.delete.inUseOne"
                          : "personas.delete.inUseMany",
                        { count: inUse.length },
                      )
                    : t("personas.delete.inUseUnknown")}
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
            {t("personas.delete.confirmHint")}
          </p>
        )}

        <DialogFooter>
          {inUse !== null ? (
            <Button autoFocus onClick={() => onOpenChange(false)}>
              {t("common.close")}
            </Button>
          ) : (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
                {t("common.cancel")}
              </Button>
              <Button variant="destructive" onClick={() => void confirm()} disabled={pending}>
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Trash2 aria-hidden />}
                {t("personas.delete.confirm")}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
