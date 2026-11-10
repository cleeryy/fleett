import { useState } from "react";
import { FlaskConical } from "lucide-react";
import {
  mockConfig,
  setMockScenario,
  type MockAssistantMode,
  type MockDoctorMode,
  type MockOnboardingMode,
} from "@/api/mock";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { RichText } from "@/components/rich-text";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

const ONBOARDING_OPTIONS: { value: MockOnboardingMode; labelKey: string }[] = [
  { value: "empty", labelKey: "mock.onboardingEmpty" },
  { value: "partial", labelKey: "mock.onboardingPartial" },
  { value: "complete", labelKey: "mock.onboardingComplete" },
];

const DOCTOR_OPTIONS: { value: MockDoctorMode; labelKey: string }[] = [
  { value: "mixed", labelKey: "mock.doctorMixed" },
  { value: "ok", labelKey: "mock.doctorOk" },
  { value: "error", labelKey: "mock.doctorError" },
];

const ASSISTANT_OPTIONS: { value: MockAssistantMode; labelKey: string }[] = [
  { value: "live", labelKey: "mock.assistantLive" },
  { value: "not-selected", labelKey: "mock.assistantNotSelected" },
  { value: "unconfigured", labelKey: "mock.assistantNoProvider" },
  { value: "unknown", labelKey: "mock.assistantUnknown" },
  { value: "upstream", labelKey: "mock.assistantUpstream" },
];

/**
 * Mock-only scenario switch for the first-run assistant, the provider states and
 * the diagnostics. It reseeds the in-memory demo and keeps the choices in the
 * URL, so a reload shows the same state. Never rendered against the controller.
 */
export function MockPreviewBar({
  onChange,
  className,
}: {
  onChange: () => void;
  className?: string;
}) {
  const { t } = useI18n();
  const [onboarding, setOnboarding] = useState<MockOnboardingMode>(mockConfig.onboarding);
  const [doctor, setDoctor] = useState<MockDoctorMode>(mockConfig.doctor);
  const [assistant, setAssistant] = useState<MockAssistantMode>(mockConfig.assistant);

  function apply(next: {
    onboarding?: MockOnboardingMode;
    doctor?: MockDoctorMode;
    assistant?: MockAssistantMode;
  }) {
    if (next.onboarding) setOnboarding(next.onboarding);
    if (next.doctor) setDoctor(next.doctor);
    if (next.assistant) setAssistant(next.assistant);
    setMockScenario(next);
    onChange();
  }

  return (
    <section
      aria-labelledby="mock-preview-title"
      className={cn("space-y-3 rounded-md border border-dashed p-4", className)}
    >
      <div className="flex flex-wrap items-center gap-2">
        <FlaskConical aria-hidden className="size-4 text-muted-foreground" />
        <h2 id="mock-preview-title" className="text-sm font-medium">
          {t("mock.title")}
        </h2>
        <Badge variant="outline" className="font-mono text-[11px] font-normal">
          {t("mock.badge")}
        </Badge>
      </div>
      <p className="text-xs text-muted-foreground">
        <RichText text={t("mock.description")} />
      </p>
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-1.5">
          <p className="text-xs font-medium text-muted-foreground">{t("mock.firstRun")}</p>
          <div
            role="group"
            aria-label={t("mock.firstRunAria")}
            className="flex flex-wrap gap-1.5"
          >
            {ONBOARDING_OPTIONS.map((option) => (
              <Button
                key={option.value}
                type="button"
                size="sm"
                variant={onboarding === option.value ? "secondary" : "outline"}
                aria-pressed={onboarding === option.value}
                onClick={() => apply({ onboarding: option.value })}
              >
                {t(option.labelKey)}
              </Button>
            ))}
          </div>
        </div>
        <div className="space-y-1.5">
          <p className="text-xs font-medium text-muted-foreground">{t("health.title")}</p>
          <div
            role="group"
            aria-label={t("mock.diagnosticsAria")}
            className="flex flex-wrap gap-1.5"
          >
            {DOCTOR_OPTIONS.map((option) => (
              <Button
                key={option.value}
                type="button"
                size="sm"
                variant={doctor === option.value ? "secondary" : "outline"}
                aria-pressed={doctor === option.value}
                onClick={() => apply({ doctor: option.value })}
              >
                {t(option.labelKey)}
              </Button>
            ))}
          </div>
        </div>
        <div className="space-y-1.5 sm:col-span-2">
          <p className="text-xs font-medium text-muted-foreground">{t("mock.assistantLabel")}</p>
          <div
            role="group"
            aria-label={t("mock.assistantAria")}
            className="flex flex-wrap gap-1.5"
          >
            {ASSISTANT_OPTIONS.map((option) => (
              <Button
                key={option.value}
                type="button"
                size="sm"
                variant={assistant === option.value ? "secondary" : "outline"}
                aria-pressed={assistant === option.value}
                onClick={() => apply({ assistant: option.value })}
              >
                {t(option.labelKey)}
              </Button>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
