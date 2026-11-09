import type { ProviderSummary } from "@/api/types";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useI18n } from "@/lib/i18n";
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
  const { t } = useI18n();
  return (
    <Select
      value={value || NO_PROVIDER}
      onValueChange={(next) => onChange(next === NO_PROVIDER ? "" : next)}
      disabled={disabled || loading}
    >
      <SelectTrigger id={id} className={cn("w-full", className)} aria-describedby={describedBy}>
        <SelectValue placeholder={loading ? t("providers.picker.loading") : t("providers.picker.placeholder")} />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={NO_PROVIDER}>{t("providers.picker.none")}</SelectItem>
        {providers.map((provider) => (
          <SelectItem key={provider.id} value={provider.id}>
            {provider.name} · {provider.kind}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
