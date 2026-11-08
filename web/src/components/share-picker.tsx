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
        Shares are mounted at <span className="font-mono">/opt/data/shared/&lt;name&gt;</span>.{" "}
        <span className="font-mono">rw</span> allows writes, <span className="font-mono">ro</span> is
        read-only.
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
          No shares exist yet. Create them from{" "}
          <a
            href="#/shares"
            target="_blank"
            rel="noreferrer"
            className="underline underline-offset-4 hover:text-foreground"
          >
            Shares
          </a>{" "}
          in a second tab.
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
                      {share.instances.length}{" "}
                      {share.instances.length === 1 ? "attachment" : "attachments"}
                    </span>
                  </span>
                </Label>
                <Select
                  value={selected?.mode ?? "rw"}
                  onValueChange={(mode) => setMode(share.name, mode as "rw" | "ro")}
                  disabled={!selected}
                >
                  <SelectTrigger className="w-24" aria-label={`Mode for ${share.name}`}>
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
          ? "No shares selected."
          : `${value.length} ${value.length === 1 ? "share" : "shares"} selected.`}
      </p>
    </div>
  );
}
