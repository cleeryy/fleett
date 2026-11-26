import { useId, useState } from "react";
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
import { useI18n } from "@/lib/i18n";
import { useUiMode, type UiMode } from "@/lib/mode";
import {
  recommendMode,
  type OnboardingAnswers,
  type OnboardingGoal,
  type OnboardingScale,
} from "@/lib/onboarding";
import { useCanEdit } from "@/lib/session";

const SCALES: OnboardingScale[] = ["one", "few", "many"];
const MODES: UiMode[] = ["simple", "advanced"];

/**
 * First-sign-in questionnaire (issue #75): one screen, three questions. It has
 * no close button and ignores Escape and outside clicks until both required
 * answers are given; confirming applies the recommended console mode and
 * persists the answers in the per-browser onboarding memory so the dialog
 * never comes back.
 */
export function OnboardingQuestionnaire({
  open,
  onAnswered,
}: {
  open: boolean;
  onAnswered: (answers: OnboardingAnswers) => void;
}) {
  const { t } = useI18n();
  const { setMode } = useUiMode();
  const canEdit = useCanEdit();
  const idPrefix = useId();
  const [goal, setGoal] = useState<OnboardingGoal | null>(null);
  const [scale, setScale] = useState<OnboardingScale | null>(null);
  // `null` means "not chosen by hand": the recommendation follows the answers.
  const [chosenMode, setChosenMode] = useState<UiMode | null>(null);

  // Viewers cannot create an assistant, so the creating goal is not offered.
  const goals: OnboardingGoal[] = canEdit
    ? ["create", "explore", "administer"]
    : ["explore", "administer"];

  // The rule applies as soon as one answer makes it decisive: administering
  // or expecting many agents recommends the advanced console right away.
  const recommended = recommendMode(goal ?? "explore", scale ?? "one");
  const mode = chosenMode ?? recommended;
  const complete = goal !== null && scale !== null;

  function submit() {
    if (!goal || !scale) return;
    const answers: OnboardingAnswers = { goal, scale, mode };
    setMode(answers.mode);
    onAnswered(answers);
  }

  return (
    <Dialog open={open} onOpenChange={() => {}}>
      <DialogContent
        showCloseButton={false}
        className="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-md"
        onEscapeKeyDown={(event) => event.preventDefault()}
        onInteractOutside={(event) => event.preventDefault()}
      >
        <DialogHeader>
          <DialogTitle>{t("onboarding.title")}</DialogTitle>
          <DialogDescription>{t("onboarding.description")}</DialogDescription>
        </DialogHeader>

        <form
          className="space-y-3"
          onSubmit={(event) => {
            event.preventDefault();
            submit();
          }}
        >
          <div className="space-y-4">
            <fieldset className="min-w-0 space-y-1.5">
              <legend className="text-sm font-medium">{t("onboarding.goal.title")}</legend>
              <RadioGroup
                value={goal ?? ""}
                onValueChange={(value) => setGoal(value as OnboardingGoal)}
                className="gap-1.5"
              >
                {goals.map((option) => {
                  const id = `${idPrefix}-goal-${option}`;
                  return (
                    <Label
                      key={option}
                      htmlFor={id}
                      className="flex min-h-9 cursor-pointer items-center gap-2 rounded-md border px-3 py-1.5 font-normal has-[:focus-visible]:border-ring has-[:focus-visible]:ring-[3px] has-[:focus-visible]:ring-ring/50"
                    >
                      <RadioGroupItem value={option} id={id} />
                      <span>{t(`onboarding.goal.${option}`)}</span>
                    </Label>
                  );
                })}
              </RadioGroup>
            </fieldset>

            <fieldset className="min-w-0 space-y-1.5">
              <legend className="text-sm font-medium">{t("onboarding.scale.title")}</legend>
              <RadioGroup
                value={scale ?? ""}
                onValueChange={(value) => setScale(value as OnboardingScale)}
                className="gap-1.5"
              >
                {SCALES.map((option) => {
                  const id = `${idPrefix}-scale-${option}`;
                  return (
                    <Label
                      key={option}
                      htmlFor={id}
                      className="flex min-h-9 cursor-pointer items-center gap-2 rounded-md border px-3 py-1.5 font-normal has-[:focus-visible]:border-ring has-[:focus-visible]:ring-[3px] has-[:focus-visible]:ring-ring/50"
                    >
                      <RadioGroupItem value={option} id={id} />
                      <span>{t(`onboarding.scale.${option}`)}</span>
                    </Label>
                  );
                })}
              </RadioGroup>
            </fieldset>

            <fieldset className="min-w-0 space-y-1.5">
              <legend className="text-sm font-medium">{t("onboarding.mode.title")}</legend>
              <RadioGroup
                value={mode}
                onValueChange={(value) => setChosenMode(value as UiMode)}
                className="gap-1.5"
              >
                {MODES.map((option) => {
                  const id = `${idPrefix}-mode-${option}`;
                  const label = t(`onboarding.mode.${option}`);
                  return (
                    <Label
                      key={option}
                      htmlFor={id}
                      className="flex min-h-9 cursor-pointer items-center gap-2 rounded-md border px-3 py-1.5 font-normal has-[:focus-visible]:border-ring has-[:focus-visible]:ring-[3px] has-[:focus-visible]:ring-ring/50"
                    >
                      <RadioGroupItem value={option} id={id} />
                      <span>
                        {label}
                        {option === recommended ? t("onboarding.mode.recommended") : null}
                      </span>
                    </Label>
                  );
                })}
              </RadioGroup>
              <p className="text-xs text-muted-foreground">{t("onboarding.mode.hint")}</p>
            </fieldset>
          </div>

          <DialogFooter>
            <Button type="submit" disabled={!complete}>
              {t("onboarding.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
