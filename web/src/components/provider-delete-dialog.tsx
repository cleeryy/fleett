import { useEffect, useState } from "react";
import { AlertCircle, Loader2, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { ProviderInUseDetails, ProviderInstanceRef, ProviderSummary } from "@/api/types";
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
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";

/** `details.instances` of a `409 provider_in_use`, defensively parsed. */
function inUseInstances(error: ApiError): ProviderInstanceRef[] {
  const details = (error.details ?? null) as Partial<ProviderInUseDetails> | null;
  if (!Array.isArray(details?.instances)) return [];
  return details.instances.filter(
    (entry): entry is ProviderInstanceRef =>
      typeof entry === "object" &&
      entry !== null &&
      typeof (entry as ProviderInstanceRef).id === "string" &&
      typeof (entry as ProviderInstanceRef).name === "string",
  );
}

/**
 * Delete one provider. A provider still attached to an instance answers `409
 * provider_in_use`: the instances are listed inline so the operator knows what
 * to move first, and nothing is deleted.
 */
export function DeleteProviderDialog({
  provider,
  open,
  onOpenChange,
  onDeleted,
}: {
  provider: ProviderSummary | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDeleted: () => void;
}) {
  const { t } = useI18n();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setPending(false);
    setError(null);
  }, [open]);

  if (!provider) return null;

  const attached = error?.status === 409 && error.code === "provider_in_use" ? inUseInstances(error) : null;

  async function onDelete() {
    if (!provider) return;
    setPending(true);
    setError(null);
    try {
      await api.deleteProvider(provider.id);
      toast.success(t("providers.delete.done", { name: provider.name }));
      onDeleted();
      onOpenChange(false);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {t("providers.delete.title")}{" "}
            <span className="font-mono">{provider.name}</span>?
          </DialogTitle>
          <DialogDescription>
            {t("providers.delete.description")}
          </DialogDescription>
        </DialogHeader>

        {attached ? (
          <Alert variant="destructive" className="items-start">
            <AlertCircle aria-hidden className="mt-0.5 size-4" />
            <AlertTitle>{t("providers.delete.inUseTitle")}</AlertTitle>
            <AlertDescription>
              <p>
                {t(
                  attached.length === 1
                    ? "providers.delete.inUseOne"
                    : "providers.delete.inUseMany",
                  { count: attached.length },
                )}
              </p>
              <ul className="mt-2 divide-y rounded-md border border-destructive/30">
                {attached.map((instance) => (
                  <li key={instance.id} className="flex items-center justify-between gap-3 px-3 py-2">
                    <a
                      href={hrefFor(`/instances/${instance.id}`)}
                      className="min-w-0 truncate text-sm font-medium underline-offset-4 hover:underline"
                    >
                      {instance.name}
                    </a>
                    <span className="shrink-0 font-mono text-xs">{instance.id}</span>
                  </li>
                ))}
              </ul>
            </AlertDescription>
          </Alert>
        ) : error ? (
          <ErrorAlert error={error} />
        ) : null}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
            {attached ? t("common.close") : t("common.cancel")}
          </Button>
          {attached ? null : (
            <Button variant="destructive" onClick={onDelete} disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Trash2 aria-hidden />}
              {t("providers.delete.confirm")}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
