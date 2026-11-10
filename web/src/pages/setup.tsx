import { useState } from "react";
import { isMock } from "@/api";
import { DoctorPanel } from "@/components/doctor";
import { MockPreviewBar } from "@/components/mock-preview";
import { OnboardingSection } from "@/components/onboarding";
import { PageHeader } from "@/components/page-header";
import { useI18n } from "@/lib/i18n";

/** Scroll to the diagnostics section, respecting reduced motion. */
function revealDiagnostics() {
  const target = document.getElementById("diagnostics");
  if (!target) return;
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  target.scrollIntoView({ behavior: reduce ? "auto" : "smooth", block: "start" });
}

/**
 * Setup: the five first-run steps and the doctor report. Readable by every
 * role; neither section writes to the host, so an operator can diagnose a
 * broken fleet without an admin account.
 */
export function SetupPage() {
  const { t } = useI18n();
  const [nonce, setNonce] = useState(0);

  return (
    <>
      <PageHeader
        title={t("nav.setup")}
        description={t("setup.pageDescription")}
      />

      {isMock ? (
        <MockPreviewBar onChange={() => setNonce((value) => value + 1)} />
      ) : null}

      <OnboardingSection nonce={nonce} onRunDiagnostics={revealDiagnostics} />

      <section id="diagnostics" aria-labelledby="diagnostics-heading" className="scroll-mt-20 space-y-3">
        <div className="space-y-1">
          <h2 id="diagnostics-heading" className="text-base font-semibold">
            {t("health.title")}
          </h2>
          <p className="text-sm text-muted-foreground">{t("health.pageDescription")}</p>
        </div>
        <div className="rounded-lg border p-4 sm:p-5">
          <DoctorPanel nonce={nonce} />
        </div>
      </section>
    </>
  );
}
