import type { ApiError } from "@/api/errors";
import type { Share, ShareRef } from "@/api/types";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { ErrorAlert } from "@/components/error-alert";
import { RichText } from "@/components/rich-text";
import { useI18n } from "@/lib/i18n";

export function SharePicker({
  shares,
  value,
  onChange,
  loading,
  error,
  onRetry,
}: {
  shares: Share[];
  value: ShareRef[];
  onChange: (next: ShareRef[]) => void;
  loading?: boolean;
  error?: ApiError | null;
  onRetry?: () => void;
}) {
  const { t } = useI18n();
  function toggle(name: string, checked: boolean) {
    onChange(
      checked ? [...value, { name, mode: "rw" }] : value.filter((share) => share.name !== name),
    );
  }

  function setMode(name: string, mode: "rw" | "ro") {
    onChange(value.map((share) => (share.name === name ? { ...share, mode } : share)));
  }

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        <RichText text={t("shares.picker.hint")} />
      </p>

      {error ? <ErrorAlert error={error} onRetry={onRetry} /> : null}

      {loading ? (
        <div className="space-y-2">
          {Array.from({ length: 3 }).map((_, index) => (
            <Skeleton key={index} className="h-14 w-full rounded-md" />
          ))}
        </div>
      ) : shares.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("shares.picker.emptyLead")}{" "}
          <a
            href="#/shares"
            target="_blank"
            rel="noreferrer"
            className="underline underline-offset-4 hover:text-foreground"
          >
            {t("nav.shares")}
          </a>{" "}
          {t("shares.picker.emptyTail")}
        </p>
      ) : (
        <div className="divide-y rounded-md border">
          {shares.map((share) => {
            const selected = value.find((entry) => entry.name === share.name);
            return (
              <div key={share.name} className="flex items-center justify-between gap-3 px-3 py-2.5">
                <Label className="flex min-w-0 cursor-pointer items-center gap-3">
                  <Checkbox
                    id={`share-${share.name}`}
                    checked={!!selected}
                    onCheckedChange={(checked) => toggle(share.name, checked === true)}
                  />
                  <span className="min-w-0">
                    <span className="block font-mono text-sm">{share.name}</span>
                    <span className="block text-xs font-normal text-muted-foreground">
                      {t(
                        share.instances.length === 1
                          ? "shares.picker.attachmentOne"
                          : "shares.picker.attachmentMany",
                        { count: share.instances.length },
                      )}
                    </span>
                  </span>
                </Label>
                <Select
                  value={selected?.mode ?? "rw"}
                  onValueChange={(mode) => setMode(share.name, mode as "rw" | "ro")}
                  disabled={!selected}
                >
                  <SelectTrigger className="w-24" aria-label={t("shares.picker.modeFor", { name: share.name })}>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="rw">rw</SelectItem>
                    <SelectItem value="ro">ro</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            );
          })}
        </div>
      )}

      <p className="text-xs text-muted-foreground">
        {value.length === 0
          ? t("shares.picker.selectedNone")
          : t(
              value.length === 1 ? "shares.picker.selectedOne" : "shares.picker.selectedMany",
              { count: value.length },
            )}
      </p>
    </div>
  );
}
