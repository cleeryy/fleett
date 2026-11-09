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
import { formatClock, plural } from "@/lib/format";
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

const GROUP_TITLE: Record<DoctorCheckStatus, string> = {
  error: "Errors",
  warn: "Warnings",
  ok: "Passing",
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
  if (isUrl(fix) || isInternalPath(fix)) {
    return (
      <a
        href={isUrl(fix) ? fix : hrefFor(fix)}
        {...(isUrl(fix) ? { target: "_blank", rel: "noreferrer" } : {})}
        className="inline-flex items-center gap-1.5 rounded-sm text-sm font-medium underline-offset-4 hover:underline"
      >
        <ExternalLink aria-hidden className="size-3.5 shrink-0" />
        Open the fix
      </a>
    );
  }
  // The controller wraps host paths and settings in Markdown backticks; they
  // would end up in the copied command.
  return <CommandLine value={fix.replace(/`/g, "")} label="Copy the fix" badge="Fix" />;
}

function CheckRow({ check }: { check: DoctorCheck }) {
  const Icon = ICON[check.status];
  return (
    <li className="flex gap-3 px-3 py-3 sm:px-4">
      <Icon aria-hidden className={cn("mt-0.5 size-4 shrink-0", TONE[check.status])} />
      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
          <p className="text-sm font-medium">{check.label}</p>
          <span className="font-mono text-[11px] text-muted-foreground">{check.id}</span>
          {check.status === "error" ? (
            <Badge
              variant="outline"
              className="border-destructive/40 text-[10px] font-normal text-destructive"
            >
              Error
            </Badge>
          ) : check.status === "warn" ? (
            <Badge
              variant="outline"
              className="border-amber-600/40 text-[10px] font-normal text-amber-700 dark:text-amber-400"
            >
              Warning
            </Badge>
          ) : null}
        </div>
        <p className="text-sm break-words text-muted-foreground">{check.detail}</p>
        {check.fix ? <Fix fix={check.fix} /> : null}
      </div>
    </li>
  );
}

function Summary({ report }: { report: DoctorReport }) {
  const errors = report.checks.filter((check) => check.status === "error").length;
  const warnings = report.checks.filter((check) => check.status === "warn").length;

  if (errors > 0) {
    return (
      <Badge variant="outline" className="gap-1.5 border-destructive/40 text-destructive">
        <CircleX aria-hidden />
        {errors} {plural(errors, "error")}
        {warnings > 0 ? ` · ${warnings} ${plural(warnings, "warning")}` : ""}
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
        Ready with {warnings} {plural(warnings, "warning")}
      </Badge>
    );
  }
  return (
    <Badge
      variant="outline"
      className="gap-1.5 border-emerald-600/30 text-emerald-700 dark:text-emerald-400"
    >
      <CircleCheck aria-hidden />
      All checks passed
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
          {checkedAt ? `Checked ${formatClock(checkedAt)}` : doctor.loading ? "Running…" : ""}
        </span>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="ml-auto"
          onClick={doctor.reload}
        >
          <RefreshCw aria-hidden className={doctor.refreshing ? "animate-spin" : undefined} />
          Refresh
        </Button>
      </div>

      {doctor.error ? <ErrorAlert error={doctor.error} onRetry={doctor.reload} /> : null}

      {doctor.loading && !report ? <ReportSkeleton /> : null}

      {report && report.checks.length === 0 ? (
        <p className="rounded-md border px-4 py-6 text-center text-sm text-muted-foreground">
          The controller returned no checks.
        </p>
      ) : null}

      {groups.map((group) => (
        <div key={group.status} className="space-y-2">
          <h3 className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
            {GROUP_TITLE[group.status]}
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
        Warnings never fail the report; only an error needs action. Nothing here writes to the host.
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
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="top-[5dvh] max-h-[90dvh] translate-y-0 gap-0 overflow-hidden p-0 sm:max-w-2xl">
        <DialogHeader className="border-b px-4 py-3 pr-12">
          <DialogTitle>Diagnostics</DialogTitle>
          <DialogDescription>
            Ten read-only checks of the host and the fleet. Run a fix, then refresh.
          </DialogDescription>
        </DialogHeader>
        <div className="overflow-y-auto overscroll-contain p-4">
          <DoctorPanel />
        </div>
      </DialogContent>
    </Dialog>
  );
}
