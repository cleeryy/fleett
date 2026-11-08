import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { ErrorAlert } from "@/components/error-alert";

export interface DeletableInstance {
  id: string;
  name: string;
}

export function DeleteInstanceDialog({
  instance,
  open,
  onOpenChange,
  onDeleted,
}: {
  instance: DeletableInstance | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDeleted: () => void;
}) {
  const [purge, setPurge] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setPurge(false);
    setError(null);
    setPending(false);
  }, [open]);

  async function onConfirm() {
    if (!instance) return;
    setPending(true);
    setError(null);
    try {
      await api.deleteInstance(instance.id, purge);
      onOpenChange(false);
      onDeleted();
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
          <DialogTitle>Delete instance {instance?.id ?? ""}?</DialogTitle>
          <DialogDescription>
            The container is removed with <span className="font-mono">docker compose down</span>. Choose what
            happens to its data directory.
          </DialogDescription>
        </DialogHeader>

        <RadioGroup
          value={purge ? "purge" : "keep"}
          onValueChange={(value) => setPurge(value === "purge")}
          className="gap-3"
          disabled={pending}
        >
          <div className="flex items-start gap-3 rounded-md border p-3">
            <RadioGroupItem value="keep" id="delete-keep" className="mt-0.5" />
            <div className="space-y-0.5">
              <Label htmlFor="delete-keep">Keep data</Label>
              <p className="text-xs text-muted-foreground">
                <span className="font-mono">instances/{instance?.id}/data</span> stays on disk. Recreating the
                same id reuses it.
              </p>
            </div>
          </div>
          <div className="flex items-start gap-3 rounded-md border p-3">
            <RadioGroupItem value="purge" id="delete-purge" className="mt-0.5" />
            <div className="space-y-0.5">
              <Label htmlFor="delete-purge">Delete data</Label>
              <p className="text-xs text-muted-foreground">
                The data directory is removed. This cannot be undone.
              </p>
            </div>
          </div>
        </RadioGroup>

        {error ? <ErrorAlert error={error} /> : null}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
            Cancel
          </Button>
          <Button variant="destructive" onClick={onConfirm} disabled={pending}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
            Delete instance
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
