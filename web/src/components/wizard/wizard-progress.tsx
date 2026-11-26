import { Check } from "lucide-react";
import { WIZARD_STEP_COUNT } from "@/lib/agent-draft";
import { useI18n } from "@/lib/i18n";

/**
 * Progress header: the current step is announced politely, the bar is
 * decorative (the text carries the state) and stops animating when the user
 * asked for reduced motion.
 */
export function WizardProgress({
  step,
  name,
  saved,
}: {
  step: number;
  name: string;
  saved: boolean;
}) {
  const { t } = useI18n();
  const current = step + 1;
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1">
        <p className="text-sm font-medium" aria-live="polite">
          {t("create.stepOf", { current, total: WIZARD_STEP_COUNT, name })}
        </p>
        {saved ? (
          <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
            <Check aria-hidden className="size-3" />
            {t("wizard.saved")}
          </span>
        ) : null}
      </div>
      <div
        role="progressbar"
        aria-label={t("wizard.progressAria")}
        aria-valuemin={0}
        aria-valuemax={WIZARD_STEP_COUNT}
        aria-valuenow={current}
        className="h-1 overflow-hidden rounded-full bg-muted"
      >
        <div
          className="h-full rounded-full bg-primary transition-[width] duration-300 ease-out motion-reduce:transition-none"
          style={{ width: `${(current / WIZARD_STEP_COUNT) * 100}%` }}
        />
      </div>
    </div>
  );
}
