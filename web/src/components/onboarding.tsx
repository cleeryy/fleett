import { useMemo, useState } from "react";
import {
  ArrowRight,
  Check,
  CircleCheck,
  ListChecks,
  RefreshCw,
  Stethoscope,
  X,
} from "lucide-react";
import { api } from "@/api";
import type { InstanceSummary, OnboardingReport, OnboardingStep } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { CommandLine } from "@/components/command-line";
import { ErrorAlert } from "@/components/error-alert";
import { StatusBadge } from "@/components/status-badge";
import { plural } from "@/lib/format";
import { hrefFor } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

/**
 * `GET /api/onboarding` names the wizard as `/instances?new=1`; this console
 * gives it its own route.
 */
export function hrefForStep(to: string): string {
  const [path, search = ""] = to.split("?");
  if (path === "/instances" && new URLSearchParams(search).get("new") === "1") {
    return hrefFor("/instances/new");
  }
  return hrefFor(to);
}

export function OnboardingProgress({
  steps,
  className,
}: {
  steps: OnboardingStep[];
  className?: string;
}) {
  const total = steps.length;
  const done = steps.filter((step) => step.done).length;
  const percent = total === 0 ? 0 : Math.round((done / total) * 100);
  return (
    <div className={cn("flex items-center gap-3", className)}>
      <div
        role="progressbar"
        aria-label="Setup progress"
        aria-valuemin={0}
        aria-valuemax={total}
        aria-valuenow={done}
        className="h-1.5 min-w-16 flex-1 overflow-hidden rounded-full bg-muted"
      >
        <div
          className="h-full rounded-full bg-primary transition-[width] duration-300"
          style={{ width: `${percent}%` }}
        />
      </div>
      <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">
        {done} of {total}
      </span>
    </div>
  );
}

function StepIcon({
  done,
  current,
  index,
}: {
  done: boolean;
  current: boolean;
  index: number;
}) {
  return (
    <span
      aria-hidden
      className={cn(
        "flex size-6 shrink-0 items-center justify-center rounded-full border text-[11px] font-medium tabular-nums",
        done
          ? "border-emerald-600/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400"
          : current
            ? "border-foreground/30 text-foreground"
            : "text-muted-foreground",
      )}
    >
      {done ? <Check className="size-3.5" /> : index + 1}
    </span>
  );
}

function StepAction({ step, canEdit }: { step: OnboardingStep; canEdit: boolean }) {
  const action = step.action;
  if (!action) return null;
  if (action.kind === "command") {
    return (
      <CommandLine
        value={action.command}
        label={`Copy the command for ${step.title}`}
        className="mt-1.5"
      />
    );
  }
  if (!canEdit) {
    return <p className="pt-1 text-xs text-muted-foreground">An admin has to run this step.</p>;
  }
  const label =
    step.id === "provider" ? "New provider" : step.id === "instance" ? "New instance" : "Open";
  return (
    <div className="pt-1.5">
      <Button asChild variant="outline" size="sm">
        <a href={hrefForStep(action.to)}>
          {label}
          <ArrowRight aria-hidden />
        </a>
      </Button>
    </div>
  );
}

/** The four steps, with the first unfinished one marked as the next action. */
export function OnboardingSteps({
  report,
  className,
}: {
  report: OnboardingReport;
  className?: string;
}) {
  const canEdit = useCanEdit();
  const currentIndex = report.steps.findIndex((step) => !step.done);

  return (
    <div className={cn("space-y-3", className)}>
      <OnboardingProgress steps={report.steps} />
      <ol className="divide-y overflow-hidden rounded-md border">
        {report.steps.map((step, index) => (
          <li
            key={step.id}
            className={cn("flex gap-3 px-3 py-3 sm:px-4", index === currentIndex && "bg-muted/30")}
          >
            <StepIcon done={step.done} current={index === currentIndex} index={index} />
            <div className="min-w-0 flex-1 space-y-1">
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <p className={cn("text-sm font-medium", step.done && "text-muted-foreground")}>
                  {step.title}
                </p>
                {step.done ? (
                  <span className="text-xs text-muted-foreground">Done</span>
                ) : index === currentIndex ? (
                  <Badge variant="secondary" className="text-[11px] font-normal">
                    Next
                  </Badge>
                ) : null}
                <span className="sr-only">{step.done ? "Done." : "Not done."}</span>
              </div>
              <p className="text-xs text-muted-foreground">{step.description}</p>
              {!step.done ? <StepAction step={step} canEdit={canEdit} /> : null}
            </div>
          </li>
        ))}
      </ol>
    </div>
  );
}

/** Prominent first-run card on the Fleet page, used while the fleet is empty. */
export function FleetOnboardingCard({
  report,
  onDismiss,
  onOpenDoctor,
}: {
  report: OnboardingReport;
  onDismiss: () => void;
  onOpenDoctor: () => void;
}) {
  const remaining = report.steps.filter((step) => !step.done).length;
  return (
    <section aria-labelledby="getting-started-title" className="rounded-lg border bg-card p-4 sm:p-5">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <div className="flex items-center gap-2">
            <ListChecks aria-hidden className="size-4 shrink-0 text-muted-foreground" />
            <h2 id="getting-started-title" className="text-base font-semibold tracking-tight">
              Getting started
            </h2>
          </div>
          <p className="text-sm text-muted-foreground">
            {remaining > 0
              ? `${remaining} ${plural(remaining, "step")} left before your first agent can answer.`
              : "Every prerequisite is in place."}
          </p>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          className="-mt-1 -mr-1 shrink-0"
          aria-label="Dismiss getting started"
          title="Dismiss getting started"
          onClick={onDismiss}
        >
          <X aria-hidden />
        </Button>
      </div>

      <OnboardingSteps report={report} className="mt-4" />

      <div className="mt-4 flex flex-wrap items-center gap-2">
        <Button type="button" variant="outline" size="sm" onClick={onOpenDoctor}>
          <Stethoscope aria-hidden />
          Run diagnostics
        </Button>
        <Button asChild variant="ghost" size="sm">
          <a href={hrefFor("/setup")}>Open setup</a>
        </Button>
        <span className="text-xs text-muted-foreground sm:ml-auto">
          Diagnostics only read the host.
        </span>
      </div>
    </section>
  );
}

function allowlistKeyFor(
  channel: string,
  template: { env: { key: string; required?: boolean }[] } | undefined,
): string | null {
  if (channel !== "discord" && channel !== "telegram") return null;
  const fromTemplate = template?.env.find(
    (entry) => entry.required && /ALLOW(?:ED)?_USERS/.test(entry.key),
  );
  return fromTemplate?.key ?? `${channel.toUpperCase()}_ALLOWED_USERS`;
}

/**
 * Final step: the checklist is done, so the card becomes a way into the first
 * instance and its channel allowlist. Rendered on the Setup page and, once,
 * on the Fleet page for the browser that went through the tour.
 */
export function OnboardingCompleteCard({
  instances,
  onDismiss,
  onOpenDiagnostics,
}: {
  instances: InstanceSummary[];
  onDismiss?: () => void;
  onOpenDiagnostics?: () => void;
}) {
  const canEdit = useCanEdit();
  const templates = useResource(() => api.listTemplates(), []);
  const instance = useMemo(
    () =>
      [...instances].sort((left, right) => left.created_at.localeCompare(right.created_at))[0] ??
      null,
    [instances],
  );
  const template = templates.data?.find((candidate) => candidate.id === instance?.template);
  const allowlistKey = instance ? allowlistKeyFor(instance.channel, template) : null;
  const channelLabel =
    instance?.channel === "discord" ? "Discord" : instance?.channel === "telegram" ? "Telegram" : "";
  const instanceHref = instance
    ? hrefFor(`/instances/${encodeURIComponent(instance.id)}`)
    : hrefFor("/instances");
  const allowlistHref =
    instance && allowlistKey
      ? hrefFor(
          `/instances/${encodeURIComponent(instance.id)}?tab=configuration&focus=env&env=${encodeURIComponent(allowlistKey)}`,
        )
      : null;

  return (
    <section
      aria-labelledby="onboarding-complete-title"
      className="rounded-lg border border-emerald-600/30 bg-emerald-500/[0.04] p-4 sm:p-5"
    >
      <div className="flex items-start gap-3">
        <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-full bg-emerald-500/10 text-emerald-600 dark:text-emerald-400">
          <CircleCheck aria-hidden className="size-5" />
        </span>
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2 id="onboarding-complete-title" className="text-base font-semibold tracking-tight">
              You're set up
            </h2>
            {instance ? <StatusBadge status={instance.status} /> : null}
          </div>
          <p className="text-sm text-muted-foreground">
            {instance ? (
              <>
                <span className="font-medium text-foreground">{instance.name}</span> is your first
                instance. Open it to read the logs, message the agent or change its configuration.
              </>
            ) : (
              "The first instance exists. Open Instances to manage the fleet."
            )}
          </p>
        </div>
        {onDismiss ? (
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            className="-mt-1 -mr-1 shrink-0"
            aria-label="Dismiss setup success"
            title="Dismiss setup success"
            onClick={onDismiss}
          >
            <X aria-hidden />
          </Button>
        ) : null}
      </div>

      <div className="mt-4 flex flex-wrap items-center gap-2 sm:pl-11">
        <Button asChild size="sm">
          <a href={instanceHref}>Open {instance ? instance.name : "Instances"}</a>
        </Button>
        {allowlistHref && allowlistKey && canEdit ? (
          <Button asChild variant="outline" size="sm">
            <a href={allowlistHref}>Check the {channelLabel} allowlist</a>
          </Button>
        ) : null}
        {onOpenDiagnostics ? (
          <Button type="button" variant="ghost" size="sm" onClick={onOpenDiagnostics}>
            <Stethoscope aria-hidden />
            Run diagnostics
          </Button>
        ) : null}
        {onDismiss ? (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="sm:ml-auto"
            onClick={onDismiss}
          >
            Got it
          </Button>
        ) : null}
      </div>

      {allowlistKey && canEdit ? (
        <p className="mt-2 text-xs text-muted-foreground sm:pl-11">
          <span className="font-mono">{allowlistKey}</span> has to list the users allowed to talk to
          the agent; without it, every message is denied.
        </p>
      ) : null}
    </section>
  );
}

/** The Setup page section: steps while incomplete, the success panel after. */
export function OnboardingSection({
  nonce = 0,
  onRunDiagnostics,
}: {
  nonce?: number;
  onRunDiagnostics: () => void;
}) {
  const [localNonce, setLocalNonce] = useState(0);
  const onboarding = useResource(() => api.onboarding(), [nonce, localNonce]);
  const instances = useResource(() => api.listInstances(), [nonce, localNonce]);
  const report = onboarding.data;

  return (
    <section aria-labelledby="getting-started-heading" className="space-y-3">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <h2 id="getting-started-heading" className="text-base font-semibold">
            Getting started
          </h2>
          <p className="text-sm text-muted-foreground">
            Four prerequisites, in the order they have to be satisfied. The state is derived live
            from Docker, the database and the filesystem.
          </p>
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => setLocalNonce((value) => value + 1)}
        >
          <RefreshCw aria-hidden className={onboarding.refreshing ? "animate-spin" : undefined} />
          Refresh
        </Button>
      </div>

      {onboarding.error ? <ErrorAlert error={onboarding.error} onRetry={onboarding.reload} /> : null}

      {onboarding.loading && !report ? <Skeleton className="h-72 w-full rounded-md" /> : null}

      {report?.complete ? (
        <div className="space-y-3">
          <OnboardingProgress steps={report.steps} />
          <OnboardingCompleteCard
            instances={instances.data ?? []}
            onOpenDiagnostics={onRunDiagnostics}
          />
        </div>
      ) : report ? (
        <div className="rounded-lg border p-4 sm:p-5">
          <OnboardingSteps report={report} />
        </div>
      ) : null}
    </section>
  );
}
