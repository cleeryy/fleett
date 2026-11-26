import { ArrowRight, Check, ListChecks, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

export type FirstAgentStepId = "create" | "connect" | "test" | "personalize";

/** One checklist row; `done` is always derived from live data by the caller. */
export interface FirstAgentStep {
  id: FirstAgentStepId;
  done: boolean;
  href: string;
}

/**
 * First-agent checklist (issue #75): four steps whose completion comes from
 * real controller data — an agent exists, its connector is set, a message
 * exists, a persona or a skill is attached. There is no manual checkbox: each
 * row links to the screen that performs the step.
 */
export function FirstAgentChecklist({
  steps,
  onDismiss,
}: {
  steps: FirstAgentStep[];
  onDismiss: () => void;
}) {
  const { t } = useI18n();
  const remaining = steps.filter((step) => !step.done).length;

  return (
    <section aria-labelledby="first-agent-title" className="rounded-lg border bg-card p-4 sm:p-5">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <div className="flex items-center gap-2">
            <ListChecks aria-hidden className="size-4 shrink-0 text-muted-foreground" />
            <h2 id="first-agent-title" className="text-base font-semibold tracking-tight">
              {t("checklist.title")}
            </h2>
          </div>
          <p className="text-sm text-muted-foreground">
            {remaining > 0
              ? t(remaining === 1 ? "checklist.remainingOne" : "checklist.remainingMany", {
                  count: remaining,
                })
              : t("checklist.allDone")}
          </p>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          className="-mt-1 -mr-1 shrink-0"
          aria-label={t("checklist.dismiss")}
          title={t("checklist.dismiss")}
          onClick={onDismiss}
        >
          <X aria-hidden />
        </Button>
      </div>

      <ol className="mt-4 divide-y overflow-hidden rounded-md border">
        {steps.map((step, index) => (
          <li
            key={step.id}
            className="flex flex-col gap-2 px-3 py-3 sm:flex-row sm:items-center sm:gap-3 sm:px-4"
          >
            <span
              aria-hidden
              className={cn(
                "flex size-6 shrink-0 items-center justify-center rounded-full border text-[11px] font-medium tabular-nums",
                step.done
                  ? "border-emerald-600/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400"
                  : "text-muted-foreground",
              )}
            >
              {step.done ? <Check className="size-3.5" /> : index + 1}
            </span>
            <div className="min-w-0 flex-1 space-y-0.5">
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <p className={cn("text-sm font-medium", step.done && "text-muted-foreground")}>
                  {t(`checklist.step.${step.id}.title`)}
                </p>
                <span
                  className={cn(
                    "text-xs",
                    step.done ? "text-emerald-700 dark:text-emerald-400" : "font-medium text-foreground",
                  )}
                >
                  {step.done ? t("common.done") : t("checklist.todo")}
                </span>
              </div>
              <p className="text-xs text-muted-foreground">
                {t(`checklist.step.${step.id}.description`)}
              </p>
            </div>
            <a
              href={step.href}
              className="inline-flex shrink-0 items-center gap-1 self-start rounded-sm text-sm font-medium underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none sm:self-center"
            >
              {t(`checklist.step.${step.id}.action`)}
              <ArrowRight aria-hidden className="size-3.5" />
            </a>
          </li>
        ))}
      </ol>
    </section>
  );
}
