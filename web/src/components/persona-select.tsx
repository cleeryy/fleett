import type { PersonaSummary } from "@/api/types";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** Radix Select forbids an empty item value; callers map this back to "". */
export const NO_PERSONA = "none";

/**
 * Optional persona picker shared by the create wizard, the clone dialog and the
 * Configuration tab. `""` means "no persona: the instance keeps its own
 * SOUL.md".
 */
export function PersonaSelect({
  id,
  value,
  onChange,
  personas,
  loading = false,
  disabled = false,
  describedBy,
  className,
}: {
  id?: string;
  value: string;
  onChange: (next: string) => void;
  personas: PersonaSummary[];
  loading?: boolean;
  disabled?: boolean;
  describedBy?: string;
  className?: string;
}) {
  const { t } = useI18n();
  return (
    <Select
      value={value || NO_PERSONA}
      onValueChange={(next) => onChange(next === NO_PERSONA ? "" : next)}
      disabled={disabled || loading}
    >
      <SelectTrigger id={id} className={cn("w-full", className)} aria-describedby={describedBy}>
        <SelectValue placeholder={loading ? t("personas.picker.loading") : t("personas.picker.placeholder")} />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={NO_PERSONA}>{t("personas.picker.none")}</SelectItem>
        {personas.map((persona) => (
          <SelectItem key={persona.id} value={persona.id}>
            {persona.name} ·{" "}
            {t(
              persona.variables.length === 1
                ? "personas.picker.variableOne"
                : "personas.picker.variableMany",
              { count: persona.variables.length },
            )}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
