import { useMemo, useState } from "react";
import { CalendarClock, Loader2, Plus, RefreshCw, Search } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { Schedule, ScheduleView } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { OpsBadge } from "@/components/ops-badge";
import { PageHeader } from "@/components/page-header";
import {
  RunHistorySheet,
  ScheduleFormDialog,
  ScheduleRowActions,
} from "@/components/schedules";
import { formatDateTime, formatRate, formatRelative, plural } from "@/lib/format";
import { useResource } from "@/lib/use-resource";

function Stat({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="rounded-md border px-4 py-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="mt-1 font-mono text-lg font-medium">{value}</p>
      {hint ? <p className="mt-0.5 text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  );
}

export function SchedulesPage() {
  const [nonce, setNonce] = useState(0);
  const [q, setQ] = useState("");
  const [instance, setInstance] = useState("all");
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<Schedule | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ScheduleView | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<ApiError | null>(null);
  const [runningId, setRunningId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);
  const [historyInstance, setHistoryInstance] = useState<string | null>(null);

  const schedules = useResource(() => api.listSchedules(), [nonce]);
  const instances = useResource(() => api.listInstances(), []);
  const rows = schedules.data ?? [];

  const instanceOptions = useMemo(
    () => [...new Set(rows.map((schedule) => schedule.instance_id))].sort(),
    [rows],
  );

  const filtered = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return rows.filter((schedule) => {
      if (instance !== "all" && schedule.instance_id !== instance) return false;
      if (!needle) return true;
      return [schedule.name, schedule.instance_id, schedule.cron, schedule.prompt].some((value) =>
        value.toLowerCase().includes(needle),
      );
    });
  }, [rows, q, instance]);

  const totals = useMemo(() => {
    const enabled = rows.filter((schedule) => schedule.enabled).length;
    const failed = rows.reduce((sum, schedule) => sum + schedule.runs_failed, 0);
    const runs = rows.reduce((sum, schedule) => sum + schedule.runs_total, 0);
    const next = rows
      .map((schedule) => schedule.next_run_at)
      .filter((value): value is string => !!value)
      .sort()[0];
    return { enabled, failed, runs, next, failing: rows.filter((schedule) => schedule.runs_failed > 0).length };
  }, [rows]);

  const historyRows = rows.filter((schedule) => schedule.instance_id === historyInstance);

  async function runNow(schedule: ScheduleView) {
    setRunningId(schedule.id);
    setActionError(null);
    try {
      const run = await api.runSchedule(schedule.id);
      toast.success(
        run.status === "ok" ? `${schedule.name}: run finished ok.` : `${schedule.name}: run ${run.status}.`,
      );
      setNonce((value) => value + 1);
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setRunningId(null);
    }
  }

  async function remove() {
    if (!deleteTarget) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      await api.deleteSchedule(deleteTarget.id);
      toast.success(`Schedule ${deleteTarget.name} deleted.`);
      setDeleteTarget(null);
      setNonce((value) => value + 1);
    } catch (cause) {
      setDeleteError(toApiError(cause));
    } finally {
      setDeleting(false);
    }
  }

  return (
    <>
      <PageHeader
        title="Schedules"
        description={
          schedules.data
            ? `${rows.length} ${plural(rows.length, "schedule")} across ${instanceOptions.length} ${plural(instanceOptions.length, "instance")} · cron fields are UTC; times are shown in your local timezone`
            : "Loading schedules."
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={schedules.refreshing ? "animate-spin" : undefined} />
              Refresh
            </Button>
            <Button
              size="sm"
              disabled={(instances.data?.length ?? 0) === 0}
              onClick={() => {
                setEditing(null);
                setFormOpen(true);
              }}
            >
              <Plus aria-hidden />
              New schedule
            </Button>
          </>
        }
      />

      {actionError ? <ErrorAlert error={actionError} /> : null}
      {schedules.error ? <ErrorAlert error={schedules.error} onRetry={schedules.reload} /> : null}

      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <Stat label="Enabled" value={`${totals.enabled}/${rows.length}`} hint={`${rows.length - totals.enabled} disabled`} />
        <Stat label="Next run" value={totals.next ? formatDateTime(totals.next) : "—"} />
        <Stat
          label="Failure rate"
          value={totals.runs > 0 ? formatRate(totals.failed / totals.runs) : "—"}
          hint={`${totals.failed} failed of ${totals.runs} runs`}
        />
        <Stat label="Schedules failing" value={String(totals.failing)} hint="error or timeout in history" />
      </div>

      <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
        <div className="relative w-full sm:max-w-xs">
          <Search
            aria-hidden
            className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground"
          />
          <Input
            value={q}
            onChange={(event) => setQ(event.target.value)}
            placeholder="Search name, prompt, cron"
            aria-label="Search schedules"
            className="pl-8"
          />
        </div>
        <Select value={instance} onValueChange={setInstance} disabled={!schedules.data}>
          <SelectTrigger className="w-full sm:w-56" aria-label="Filter by instance">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All instances</SelectItem>
            {instanceOptions.map((id) => (
              <SelectItem key={id} value={id}>
                {id}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <p className="text-xs text-muted-foreground sm:ml-auto" aria-live="polite">
          {filtered.length} shown
        </p>
      </div>

      <div className="rounded-md border" aria-busy={schedules.loading || schedules.refreshing}>
        {schedules.loading && !schedules.data ? (
          <div className="space-y-2 p-4">
            {Array.from({ length: 5 }).map((_, index) => (
              <Skeleton key={index} className="h-12 w-full" />
            ))}
          </div>
        ) : filtered.length === 0 ? (
          <EmptyState
            icon={CalendarClock}
            title={rows.length === 0 ? "No schedules yet" : "No schedule matches the filters"}
            description={
              rows.length === 0
                ? "A schedule runs a prompt inside one instance on a cron cadence and keeps its run history."
                : "Clear the search or pick another instance."
            }
            action={
              rows.length === 0 ? (
                <Button
                  size="sm"
                  disabled={(instances.data?.length ?? 0) === 0}
                  onClick={() => {
                    setEditing(null);
                    setFormOpen(true);
                  }}
                >
                  <Plus aria-hidden />
                  New schedule
                </Button>
              ) : (
                <Button
                  variant="outline"
                  onClick={() => {
                    setQ("");
                    setInstance("all");
                  }}
                >
                  Clear filters
                </Button>
              )
            }
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Schedule</TableHead>
                    <TableHead className="w-[140px]">Cron</TableHead>
                    <TableHead className="w-[170px]">Next run</TableHead>
                    <TableHead className="w-[160px]">Last run</TableHead>
                    <TableHead className="w-[110px]">Failure rate</TableHead>
                    <TableHead className="w-[160px]">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {filtered.map((schedule) => (
                    <TableRow key={schedule.id}>
                      <TableCell>
                        <a
                          href={`#/instances/${encodeURIComponent(schedule.instance_id)}`}
                          className="block max-w-[300px] rounded-sm"
                        >
                          <span className="block text-sm font-medium hover:underline">{schedule.name}</span>
                          <span className="block truncate font-mono text-xs text-muted-foreground">
                            {schedule.instance_id}
                          </span>
                        </a>
                      </TableCell>
                      <TableCell>
                        <span className="font-mono text-xs">{schedule.cron}</span>
                        {!schedule.enabled ? (
                          <span className="ml-1 text-[11px] text-muted-foreground">off</span>
                        ) : null}
                      </TableCell>
                      <TableCell className="font-mono text-xs">
                        {schedule.next_run_at ? formatDateTime(schedule.next_run_at) : "—"}
                      </TableCell>
                      <TableCell>
                        {schedule.last_run ? (
                          <span className="flex flex-col items-start gap-1">
                            <OpsBadge status={schedule.last_run.status} />
                            <span className="font-mono text-[11px] text-muted-foreground">
                              {formatRelative(schedule.last_run.started_at)}
                            </span>
                          </span>
                        ) : (
                          <span className="text-xs text-muted-foreground">never</span>
                        )}
                      </TableCell>
                      <TableCell className="font-mono text-xs">
                        {schedule.runs_total > 0
                          ? `${formatRate(schedule.failure_rate)} (${schedule.runs_failed}/${schedule.runs_total})`
                          : "—"}
                      </TableCell>
                      <TableCell>
                        <ScheduleRowActions
                          schedule={schedule}
                          canEdit
                          running={runningId === schedule.id}
                          onRun={() => void runNow(schedule)}
                          onHistory={() => setHistoryInstance(schedule.instance_id)}
                          onEdit={() => {
                            setEditing(schedule);
                            setFormOpen(true);
                          }}
                          onDelete={() => {
                            setDeleteError(null);
                            setDeleteTarget(schedule);
                          }}
                        />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {filtered.map((schedule) => (
                <div key={schedule.id} className="space-y-3 p-4">
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0">
                      <a
                        href={`#/instances/${encodeURIComponent(schedule.instance_id)}`}
                        className="text-sm font-medium"
                      >
                        {schedule.name}
                      </a>
                      <p className="font-mono text-xs text-muted-foreground">{schedule.instance_id}</p>
                    </div>
                    {schedule.last_run ? <OpsBadge status={schedule.last_run.status} /> : null}
                  </div>
                  <dl className="grid grid-cols-2 gap-2 text-xs">
                    <div>
                      <dt className="text-muted-foreground">Cron</dt>
                      <dd className="font-mono">{schedule.cron}</dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">Next</dt>
                      <dd className="font-mono">
                        {schedule.next_run_at ? formatDateTime(schedule.next_run_at) : "—"}
                      </dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">Last</dt>
                      <dd className="font-mono">
                        {schedule.last_run_at ? formatRelative(schedule.last_run_at) : "never"}
                      </dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">Failures</dt>
                      <dd className="font-mono">{formatRate(schedule.failure_rate)}</dd>
                    </div>
                  </dl>
                  <div className="flex justify-end gap-1">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={runningId === schedule.id}
                      onClick={() => void runNow(schedule)}
                    >
                      {runningId === schedule.id ? (
                        <Loader2 aria-hidden className="animate-spin" />
                      ) : null}
                      Run
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setHistoryInstance(schedule.instance_id)}
                    >
                      History
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        setEditing(schedule);
                        setFormOpen(true);
                      }}
                    >
                      Edit
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        setDeleteError(null);
                        setDeleteTarget(schedule);
                      }}
                    >
                      Delete
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      <ScheduleFormDialog
        open={formOpen}
        onOpenChange={setFormOpen}
        instances={(instances.data ?? []).map((row) => ({ id: row.id, name: row.name }))}
        schedule={editing}
        onSaved={() => setNonce((value) => value + 1)}
      />
      <RunHistorySheet
        open={historyInstance !== null}
        onOpenChange={(open) => {
          if (!open) setHistoryInstance(null);
        }}
        instanceId={historyInstance}
        schedules={historyRows}
      />
      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        title="Delete schedule"
        description={
          deleteTarget
            ? `Delete ${deleteTarget.name} on ${deleteTarget.instance_id} and its run history? This cannot be undone.`
            : undefined
        }
        confirmLabel="Delete schedule"
        destructive
        pending={deleting}
        error={deleteError}
        onConfirm={() => void remove()}
      />
    </>
  );
}
