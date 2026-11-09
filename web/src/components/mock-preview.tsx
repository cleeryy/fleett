import { useState } from "react";
import { FlaskConical } from "lucide-react";
import {
  mockConfig,
  setMockScenario,
  type MockDoctorMode,
  type MockOnboardingMode,
} from "@/api/mock";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

const ONBOARDING_OPTIONS: { value: MockOnboardingMode; label: string }[] = [
  { value: "empty", label: "0 of 4" },
  { value: "partial", label: "2 of 4" },
  { value: "complete", label: "Complete" },
];

const DOCTOR_OPTIONS: { value: MockDoctorMode; label: string }[] = [
  { value: "mixed", label: "1 warning + 1 error" },
  { value: "ok", label: "All pass" },
  { value: "error", label: "Several errors" },
];

/**
 * Mock-only scenario switch for the first-run assistant and the diagnostics.
 * It reseeds the in-memory demo and keeps both choices in the URL, so a reload
 * shows the same state. Never rendered against the real controller.
 */
export function MockPreviewBar({
  onChange,
  className,
}: {
  onChange: () => void;
  className?: string;
}) {
  const [onboarding, setOnboarding] = useState<MockOnboardingMode>(mockConfig.onboarding);
  const [doctor, setDoctor] = useState<MockDoctorMode>(mockConfig.doctor);

  function apply(next: { onboarding?: MockOnboardingMode; doctor?: MockDoctorMode }) {
    if (next.onboarding) setOnboarding(next.onboarding);
    if (next.doctor) setDoctor(next.doctor);
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
          Demo preview
        </h2>
        <Badge variant="outline" className="font-mono text-[11px] font-normal">
          mock only
        </Badge>
      </div>
      <p className="text-xs text-muted-foreground">
        Switching a scenario reseeds the in-memory demo. Both choices stay in the URL
        (<span className="font-mono">mock_onboarding</span>,{" "}
        <span className="font-mono">mock_doctor</span>) so a reload shows the same state.
      </p>
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-1.5">
          <p className="text-xs font-medium text-muted-foreground">First run</p>
          <div role="group" aria-label="First-run preview" className="flex flex-wrap gap-1.5">
            {ONBOARDING_OPTIONS.map((option) => (
              <Button
                key={option.value}
                type="button"
                size="sm"
                variant={onboarding === option.value ? "secondary" : "outline"}
                aria-pressed={onboarding === option.value}
                onClick={() => apply({ onboarding: option.value })}
              >
                {option.label}
              </Button>
            ))}
          </div>
        </div>
        <div className="space-y-1.5">
          <p className="text-xs font-medium text-muted-foreground">Diagnostics</p>
          <div role="group" aria-label="Diagnostics preview" className="flex flex-wrap gap-1.5">
            {DOCTOR_OPTIONS.map((option) => (
              <Button
                key={option.value}
                type="button"
                size="sm"
                variant={doctor === option.value ? "secondary" : "outline"}
                aria-pressed={doctor === option.value}
                onClick={() => apply({ doctor: option.value })}
              >
                {option.label}
              </Button>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
