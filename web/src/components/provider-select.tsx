import type { ProviderSummary } from "@/api/types";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";

/** Radix Select forbids an empty item value; the form maps this back to "". */
export const NO_PROVIDER = "none";

/**
 * Optional provider picker shared by the create wizard and the clone dialog.
 * `""` means "no provider: the instance stores its own token".
 */
export function ProviderSelect({
  id,
  value,
  onChange,
  providers,
  loading = false,
  disabled = false,
  describedBy,
  className,
}: {
  id?: string;
  value: string;
  onChange: (next: string) => void;
  providers: ProviderSummary[];
  loading?: boolean;
  disabled?: boolean;
  describedBy?: string;
  className?: string;
}) {
  return (
    <Select
      value={value || NO_PROVIDER}
      onValueChange={(next) => onChange(next === NO_PROVIDER ? "" : next)}
      disabled={disabled || loading}
    >
      <SelectTrigger id={id} className={cn("w-full", className)} aria-describedby={describedBy}>
        <SelectValue placeholder={loading ? "Loading providers…" : "Select a provider"} />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={NO_PROVIDER}>None — use a per-instance token</SelectItem>
        {providers.map((provider) => (
          <SelectItem key={provider.id} value={provider.id}>
            {provider.name} · {provider.kind}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
