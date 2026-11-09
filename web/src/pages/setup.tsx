import { useState } from "react";
import { isMock } from "@/api";
import { DoctorPanel } from "@/components/doctor";
import { MockPreviewBar } from "@/components/mock-preview";
import { OnboardingSection } from "@/components/onboarding";
import { PageHeader } from "@/components/page-header";

/** Scroll to the diagnostics section, respecting reduced motion. */
function revealDiagnostics() {
  const target = document.getElementById("diagnostics");
  if (!target) return;
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  target.scrollIntoView({ behavior: reduce ? "auto" : "smooth", block: "start" });
}

/**
 * Setup: the four first-run steps and the doctor report. Readable by every
 * role; neither section writes to the host, so an operator can diagnose a
 * broken fleet without an admin account.
 */
export function SetupPage() {
  const [nonce, setNonce] = useState(0);

  return (
    <>
      <PageHeader
        title="Setup"
        description="The first-run checklist and a read-only report of the host. Open to every role; running a fix is up to an operator with shell access."
      />

      {isMock ? (
        <MockPreviewBar onChange={() => setNonce((value) => value + 1)} />
      ) : null}

      <OnboardingSection nonce={nonce} onRunDiagnostics={revealDiagnostics} />

      <section id="diagnostics" aria-labelledby="diagnostics-heading" className="scroll-mt-20 space-y-3">
        <div className="space-y-1">
          <h2 id="diagnostics-heading" className="text-base font-semibold">
            Diagnostics
          </h2>
          <p className="text-sm text-muted-foreground">
            Ten checks of the host and the fleet: API, Docker, agent image, secrets, egress,
            providers, instances, permissions, disk and DNS.
          </p>
        </div>
        <div className="rounded-lg border p-4 sm:p-5">
          <DoctorPanel nonce={nonce} />
        </div>
      </section>
    </>
  );
}
