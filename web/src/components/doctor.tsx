import { useEffect, useState } from "react";
import {
  CircleCheck,
  CircleX,
  ExternalLink,
  RefreshCw,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import { api } from "@/api";
import type { DoctorCheck, DoctorCheckStatus, DoctorReport } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { CommandLine } from "@/components/command-line";
import { ErrorAlert } from "@/components/error-alert";
import { hasTranslation, useI18n } from "@/lib/i18n";
import { formatClock } from "@/lib/format";
import { hrefFor } from "@/lib/router";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

const TONE: Record<DoctorCheckStatus, string> = {
  ok: "text-emerald-600 dark:text-emerald-400",
  warn: "text-amber-600 dark:text-amber-400",
  error: "text-destructive",
};

const ICON: Record<DoctorCheckStatus, LucideIcon> = {
  ok: CircleCheck,
  warn: TriangleAlert,
  error: CircleX,
};

const GROUP_KEY: Record<DoctorCheckStatus, string> = {
  error: "health.groupErrors",
  warn: "health.groupWarnings",
  ok: "health.groupPassing",
};

const GROUP_CLASS: Record<DoctorCheckStatus, string> = {
  error: "border-destructive/30",
  warn: "border-amber-600/30",
  ok: "",
};

function isUrl(value: string): boolean {
  return /^https?:\/\//i.test(value);
}

function isInternalPath(value: string): boolean {
  // Console routes only: a fix can legitimately start with `/etc/...` (a host
  // path), which is a command to read, not a page to open.
  return /^\/(instances|providers|setup|security|tokens|audit|shares|templates|personas|skills|logs|cost|deployments|incidents|messages|memory|pipelines|schedules)(\/|\?|$)/.test(
    value,
  );
}

/** A fix is a command to copy, or a page to open when the controller sends one. */
function Fix({ fix }: { fix: string }) {
  const { t } = useI18n();
  if (isUrl(fix) || isInternalPath(fix)) {
    return (
      <a
        href={isUrl(fix) ? fix : hrefFor(fix)}
        {...(isUrl(fix) ? { target: "_blank", rel: "noreferrer" } : {})}
        className="inline-flex items-center gap-1.5 rounded-sm text-sm font-medium underline-offset-4 hover:underline"
      >
        <ExternalLink aria-hidden className="size-3.5 shrink-0" />
        {t("health.openFix")}
      </a>
    );
  }
  // The controller wraps host paths and settings in Markdown backticks; they
  // would end up in the copied command.
  return (
    <CommandLine
      value={fix.replace(/`/g, "")}
      label={t("health.copyFix")}
      badge={t("health.fixBadge")}
    />
  );
}

/**
 * A check can carry both a `fix` (copied) and a `link` (opened): the assistant
 * check has a command for the env var and a page to pick the provider.
 */
function CheckLink({ link }: { link: string }) {
  const { t } = useI18n();
  const external = isUrl(link);
  return (
    <Button asChild variant="outline" size="sm" className="h-8">
      <a
        href={external ? link : hrefFor(link)}
        {...(external ? { target: "_blank", rel: "noreferrer" } : {})}
      >
        {external ? <ExternalLink aria-hidden /> : null}
        {t("health.openLink")}
      </a>
    </Button>
  );
}

/** Check labels are server copy; map the stable id to translator keys. */
function checkTitle(
  check: DoctorCheck,
  t: (key: string) => string,
): string {
  const key = `health.check.${check.id}`;
  return hasTranslation(key) ? t(key) : check.label;
}

function CheckRow({ check }: { check: DoctorCheck }) {
  const { t } = useI18n();
  const Icon = ICON[check.status];
  return (
    <li className="flex gap-3 px-3 py-3 sm:px-4">
      <Icon aria-hidden className={cn("mt-0.5 size-4 shrink-0", TONE[check.status])} />
      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
          <p className="text-sm font-medium">{checkTitle(check, t)}</p>
          <span className="font-mono text-[11px] text-muted-foreground">{check.id}</span>
          {check.status === "error" ? (
            <Badge
              variant="outline"
              className="border-destructive/40 text-[10px] font-normal text-destructive"
            >
              {t("health.badgeError")}
            </Badge>
          ) : check.status === "warn" ? (
            <Badge
              variant="outline"
              className="border-amber-600/40 text-[10px] font-normal text-amber-700 dark:text-amber-400"
            >
              {t("health.badgeWarning")}
            </Badge>
          ) : null}
        </div>
        <p className="text-sm break-words text-muted-foreground">{check.detail}</p>
        {check.fix ? <Fix fix={check.fix} /> : null}
        {check.link ? (
          <div className="mt-1">
            <CheckLink link={check.link} />
          </div>
        ) : null}
      </div>
    </li>
  );
}

function Summary({ report }: { report: DoctorReport }) {
  const { t } = useI18n();
  const errors = report.checks.filter((check) => check.status === "error").length;
  const warnings = report.checks.filter((check) => check.status === "warn").length;

  if (errors > 0) {
    return (
      <Badge variant="outline" className="gap-1.5 border-destructive/40 text-destructive">
        <CircleX aria-hidden />
        {t(errors === 1 ? "health.errorCountOne" : "health.errorCountMany", { count: errors })}
        {warnings > 0
          ? t(warnings === 1 ? "health.warningSuffixOne" : "health.warningSuffixMany", {
              count: warnings,
            })
          : ""}
      </Badge>
    );
  }
  if (warnings > 0) {
    return (
      <Badge
        variant="outline"
        className="gap-1.5 border-amber-600/40 text-amber-700 dark:text-amber-400"
      >
        <TriangleAlert aria-hidden />
        {t(warnings === 1 ? "health.readyOne" : "health.readyMany", { count: warnings })}
      </Badge>
    );
  }
  return (
    <Badge
      variant="outline"
      className="gap-1.5 border-emerald-600/30 text-emerald-700 dark:text-emerald-400"
    >
      <CircleCheck aria-hidden />
      {t("health.allPassed")}
    </Badge>
  );
}

function ReportSkeleton() {
  return (
    <ul className="divide-y rounded-md border">
      {Array.from({ length: 5 }).map((_, index) => (
        <li key={index} className="flex gap-3 px-4 py-3">
          <Skeleton className="size-4 shrink-0 rounded-full" />
          <div className="w-full space-y-1.5">
            <Skeleton className="h-3.5 w-40" />
            <Skeleton className="h-3 w-4/5" />
          </div>
        </li>
      ))}
    </ul>
  );
}

/**
 * The ten host checks, grouped by severity. Read-only: it only calls
 * `GET /api/doctor`, which never writes or reaches the network.
 */
export function DoctorPanel({ nonce = 0, className }: { nonce?: number; className?: string }) {
  const { t } = useI18n();
  const doctor = useResource(() => api.doctor(), [nonce]);
  const [checkedAt, setCheckedAt] = useState<Date | null>(null);

  useEffect(() => {
    if (doctor.data) setCheckedAt(new Date());
  }, [doctor.data]);

  const report = doctor.data;
  const groups: { status: DoctorCheckStatus; checks: DoctorCheck[] }[] = report
    ? (["error", "warn", "ok"] as const)
        .map((status) => ({
          status,
          checks: report.checks.filter((check) => check.status === status),
        }))
        .filter((group) => group.checks.length > 0)
    : [];

  return (
    <div className={cn("space-y-3", className)} aria-busy={doctor.loading || doctor.refreshing}>
      <div className="flex flex-wrap items-center gap-2">
        {report ? <Summary report={report} /> : null}
        <span className="text-xs text-muted-foreground" aria-live="polite">
          {checkedAt
            ? t("health.checkedAt", { time: formatClock(checkedAt) })
            : doctor.loading
              ? t("health.runningNow")
              : ""}
        </span>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="ml-auto"
          onClick={doctor.reload}
        >
          <RefreshCw aria-hidden className={doctor.refreshing ? "animate-spin" : undefined} />
          {t("common.refresh")}
        </Button>
      </div>

      {doctor.error ? <ErrorAlert error={doctor.error} onRetry={doctor.reload} /> : null}

      {doctor.loading && !report ? <ReportSkeleton /> : null}

      {report && report.checks.length === 0 ? (
        <p className="rounded-md border px-4 py-6 text-center text-sm text-muted-foreground">
          {t("health.noChecks")}
        </p>
      ) : null}

      {groups.map((group) => (
        <div key={group.status} className="space-y-2">
          <h3 className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
            {t(GROUP_KEY[group.status])}
            <span className="font-mono">{group.checks.length}</span>
          </h3>
          <ul className={cn("divide-y rounded-md border", GROUP_CLASS[group.status])}>
            {group.checks.map((check) => (
              <CheckRow key={check.id} check={check} />
            ))}
          </ul>
        </div>
      ))}

      <p className="text-xs text-muted-foreground">
        {t("health.footnote")}
      </p>
    </div>
  );
}

/** The same report behind a dialog, for the Fleet page and its setup card. */
export function DoctorDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useI18n();
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="top-[5dvh] max-h-[90dvh] translate-y-0 gap-0 overflow-hidden p-0 sm:max-w-2xl">
        <DialogHeader className="border-b px-4 py-3 pr-12">
          <DialogTitle>{t("health.title")}</DialogTitle>
          <DialogDescription>{t("health.dialogDescription")}</DialogDescription>
        </DialogHeader>
        <div className="overflow-y-auto overscroll-contain p-4">
          <DoctorPanel />
        </div>
      </DialogContent>
    </Dialog>
  );
}
