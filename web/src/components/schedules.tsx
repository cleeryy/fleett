import { useEffect, useMemo, useState, type FormEvent } from "react";
import { CalendarClock, History, Loader2, Pencil, Play, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { CreateScheduleInput, Schedule, ScheduleRun, ScheduleView } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
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
import { formatDateTime, formatDurationMs, formatRelative } from "@/lib/format";
import { useResource } from "@/lib/use-resource";

interface InstanceOption {
  id: string;
  name: string;
}

const CRON_HINT = "Five fields, UTC: minute hour day-of-month month day-of-week.";

function ScheduleFormDialog({
  open,
  onOpenChange,
  instances,
  fixedInstance,
  schedule,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  instances: InstanceOption[];
  fixedInstance?: string;
  schedule?: Schedule | null;
  onSaved: (schedule: Schedule) => void;
}) {
  const editing = schedule !== null && schedule !== undefined;
  const [instanceId, setInstanceId] = useState(fixedInstance ?? instances[0]?.id ?? "");
  const [name, setName] = useState("");
  const [cron, setCron] = useState("0 7 * * *");
  const [prompt, setPrompt] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [timezone, setTimezone] = useState("UTC");
  const [overlap, setOverlap] = useState<"skip" | "queue">("skip");
  const [timeout, setTimeoutValue] = useState("300");
  const [catchup, setCatchup] = useState(false);
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setInstanceId(fixedInstance ?? schedule?.instance_id ?? instances[0]?.id ?? "");
    setName(schedule?.name ?? "");
    setCron(schedule?.cron ?? "0 7 * * *");
    setPrompt(schedule?.prompt ?? "");
    setEnabled(schedule?.enabled ?? true);
    setTimezone(schedule?.timezone ?? "UTC");
    setOverlap(schedule?.overlap ?? "skip");
    setTimeoutValue(String(schedule?.timeout ?? 300));
    setCatchup(schedule?.catchup ?? false);
    setAttempted(false);
    setError(null);
  }, [open, schedule, fixedInstance]);

  const errors: Record<string, string> = {};
  if (!instanceId) errors.instance = "Pick an instance.";
  if (!name.trim()) errors.name = "Enter a name.";
  if (!cron.trim()) errors.cron = "Enter a cron expression.";
  else if (cron.trim().split(/\s+/).length !== 5) errors.cron = "Use exactly five fields.";
  if (!prompt.trim()) errors.prompt = "Describe what the agent should do.";
  const timeoutNumber = Number(timeout);
  if (!Number.isInteger(timeoutNumber) || timeoutNumber < 1 || timeoutNumber > 3_600) {
    errors.timeout = "Use 1 to 3600 seconds.";
  }
  const showErrors = attempted;

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setAttempted(true);
    if (Object.keys(errors).length > 0) {
      document.getElementById(`schedule-${Object.keys(errors)[0]}`)?.focus();
      return;
    }
    setPending(true);
    setError(null);
    const input: CreateScheduleInput = {
      name: name.trim(),
      cron: cron.trim(),
      prompt: prompt.trim(),
      enabled,
      timezone: timezone.trim() || "UTC",
      overlap,
      timeout: timeoutNumber,
      catchup,
    };
    try {
      const saved =
        editing && schedule
          ? await api.patchSchedule(schedule.id, input)
          : await api.createSchedule(instanceId, input);
      toast.success(editing ? "Schedule saved." : "Schedule created.");
      onOpenChange(false);
      onSaved(saved);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{editing ? "Edit schedule" : "New schedule"}</DialogTitle>
          <DialogDescription>
            The controller runs the prompt inside the instance container on the cron cadence. Times
            are UTC.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={submit} noValidate className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}

          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="schedule-instance">Instance</Label>
              <Select
                value={instanceId}
                onValueChange={setInstanceId}
                disabled={editing || !!fixedInstance}
              >
                <SelectTrigger id="schedule-instance" className="w-full">
                  <SelectValue placeholder="Pick an instance" />
                </SelectTrigger>
                <SelectContent>
                  {instances.map((instance) => (
                    <SelectItem key={instance.id} value={instance.id}>
                      {instance.name} · {instance.id}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {showErrors && errors.instance ? (
                <p className="text-xs text-destructive">{errors.instance}</p>
              ) : null}
            </div>

            <div className="space-y-2">
              <Label htmlFor="schedule-name">Name</Label>
              <Input
                id="schedule-name"
                value={name}
                autoComplete="off"
                className="font-mono"
                aria-invalid={showErrors && !!errors.name}
                onChange={(event) => setName(event.target.value)}
              />
              {showErrors && errors.name ? (
                <p className="text-xs text-destructive">{errors.name}</p>
              ) : null}
            </div>
          </div>

          <div className="grid gap-4 sm:grid-cols-[minmax(0,1fr)_160px]">
            <div className="space-y-2">
              <Label htmlFor="schedule-cron">Cron</Label>
              <Input
                id="schedule-cron"
                value={cron}
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
                aria-invalid={showErrors && !!errors.cron}
                aria-describedby="schedule-cron-help"
                onChange={(event) => setCron(event.target.value)}
              />
              <p id="schedule-cron-help" className="text-xs text-muted-foreground">
                {CRON_HINT}
              </p>
              {showErrors && errors.cron ? (
                <p className="text-xs text-destructive">{errors.cron}</p>
              ) : null}
            </div>

            <div className="space-y-2">
              <Label htmlFor="schedule-timeout">Timeout (s)</Label>
              <Input
                id="schedule-timeout"
                type="number"
                min={1}
                max={3_600}
                value={timeout}
                className="font-mono"
                aria-invalid={showErrors && !!errors.timeout}
                onChange={(event) => setTimeoutValue(event.target.value)}
              />
              {showErrors && errors.timeout ? (
                <p className="text-xs text-destructive">{errors.timeout}</p>
              ) : null}
            </div>
          </div>

          <div className="space-y-2">
            <Label htmlFor="schedule-prompt">Prompt</Label>
            <Textarea
              id="schedule-prompt"
              rows={5}
              value={prompt}
              className="font-mono text-xs"
              aria-invalid={showErrors && !!errors.prompt}
              placeholder="Summarize yesterday's sessions and send one digest."
              onChange={(event) => setPrompt(event.target.value)}
            />
            {showErrors && errors.prompt ? (
              <p className="text-xs text-destructive">{errors.prompt}</p>
            ) : null}
          </div>

          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="schedule-timezone">Timezone</Label>
              <Input
                id="schedule-timezone"
                value={timezone}
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
                aria-describedby="schedule-timezone-help"
                onChange={(event) => setTimezone(event.target.value)}
              />
              <p id="schedule-timezone-help" className="text-xs text-muted-foreground">
                Stored for reference; cron evaluation is UTC for now.
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="schedule-overlap">When the previous run is late</Label>
              <Select value={overlap} onValueChange={(value) => setOverlap(value as "skip" | "queue")}>
                <SelectTrigger id="schedule-overlap" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="skip">Skip the occurrence</SelectItem>
                  <SelectItem value="queue">Queue one run</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-x-6 gap-y-3">
            <div className="flex items-center gap-2">
              <Checkbox
                id="schedule-enabled"
                checked={enabled}
                onCheckedChange={(value) => setEnabled(value === true)}
              />
              <Label htmlFor="schedule-enabled" className="cursor-pointer text-sm font-normal">
                Enabled
              </Label>
            </div>
            <div className="flex items-center gap-2">
              <Checkbox
                id="schedule-catchup"
                checked={catchup}
                onCheckedChange={(value) => setCatchup(value === true)}
              />
              <Label htmlFor="schedule-catchup" className="cursor-pointer text-sm font-normal">
                Catch up one missed run
              </Label>
            </div>
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              {editing ? "Save schedule" : "Create schedule"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function RunHistorySheet({
  open,
  onOpenChange,
  instanceId,
  schedules,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  instanceId: string | null;
  schedules: Schedule[];
}) {
  const [nonce, setNonce] = useState(0);
  const [status, setStatus] = useState("all");
  const runs = useResource(
    () => (instanceId ? api.listRuns(instanceId, 100) : Promise.resolve<ScheduleRun[]>([])),
    [instanceId, nonce, open],
  );
  const names = useMemo(
    () => new Map(schedules.map((schedule) => [schedule.id, schedule.name])),
    [schedules],
  );
  const rows = (runs.data ?? []).filter((run) => status === "all" || run.status === status);

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="w-full sm:max-w-xl">
        <SheetHeader>
          <SheetTitle>Run history</SheetTitle>
          <SheetDescription>
            {instanceId ? (
              <>
                Latest runs of <span className="font-mono">{instanceId}</span>, newest first.
              </>
            ) : (
              "Pick an instance to read its runs."
            )}
          </SheetDescription>
        </SheetHeader>

        <div className="flex items-center gap-2 px-4">
          <Select value={status} onValueChange={setStatus}>
            <SelectTrigger className="w-40" aria-label="Filter runs by status">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All statuses</SelectItem>
              <SelectItem value="ok">ok</SelectItem>
              <SelectItem value="error">error</SelectItem>
              <SelectItem value="timeout">timeout</SelectItem>
              <SelectItem value="skipped">skipped</SelectItem>
              <SelectItem value="running">running</SelectItem>
            </SelectContent>
          </Select>
          <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
            Refresh
          </Button>
          <span className="ml-auto text-xs text-muted-foreground">{rows.length} runs</span>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4">
          {runs.error ? <ErrorAlert error={runs.error} onRetry={runs.reload} /> : null}
          {runs.loading && !runs.data ? (
            <div className="space-y-2">
              {Array.from({ length: 4 }).map((_, index) => (
                <Skeleton key={index} className="h-20 w-full" />
              ))}
            </div>
          ) : rows.length === 0 ? (
            <EmptyState
              icon={History}
              title="No runs recorded"
              description="A run appears here after the schedule fires or you run it by hand."
            />
          ) : (
            <ul className="divide-y rounded-md border">
              {rows.map((run) => (
                <li key={run.id} className="space-y-2 p-3">
                  <div className="flex flex-wrap items-start justify-between gap-2">
                    <div className="min-w-0">
                      <p className="truncate text-sm font-medium">
                        {names.get(run.schedule_id) ?? run.schedule_id}
                      </p>
                      <p className="font-mono text-[11px] text-muted-foreground">
                        {formatDateTime(run.started_at)}
                      </p>
                    </div>
                    <OpsBadge status={run.status} />
                  </div>
                  <dl className="grid grid-cols-3 gap-2 text-xs">
                    <div>
                      <dt className="text-muted-foreground">Exit</dt>
                      <dd className="font-mono">{run.exit_code ?? "—"}</dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">Duration</dt>
                      <dd className="font-mono">{formatDurationMs(run.duration_ms)}</dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">Started</dt>
                      <dd className="font-mono">{formatRelative(run.started_at)}</dd>
                    </div>
                  </dl>
                  {run.output ? (
                    <details className="group">
                      <summary className="cursor-pointer text-xs text-muted-foreground hover:text-foreground">
                        Output
                      </summary>
                      <pre className="mt-1 max-h-56 overflow-auto rounded-md border bg-muted/30 p-2 font-mono text-[11px] whitespace-pre-wrap">
                        {run.output}
                      </pre>
                    </details>
                  ) : null}
                </li>
              ))}
            </ul>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}

/** Per-instance schedule management, used by the instance detail tab. */
export function InstanceSchedules({ instanceId, canEdit }: { instanceId: string; canEdit: boolean }) {
  const [nonce, setNonce] = useState(0);
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<Schedule | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<Schedule | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<ApiError | null>(null);
  const [runningId, setRunningId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);
  const schedules = useResource(() => api.listInstanceSchedules(instanceId), [instanceId, nonce]);
  const rows = schedules.data ?? [];

  async function runNow(schedule: Schedule) {
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
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">
          {rows.length === 0
            ? "No schedules yet."
            : `${rows.length} schedule${rows.length === 1 ? "" : "s"} · cron fields are UTC`}
        </p>
        <div className="flex items-center gap-2">
          <Button variant="outline" size="sm" onClick={() => setHistoryOpen(true)}>
            <History aria-hidden />
            Run history
          </Button>
          {canEdit ? (
            <Button
              size="sm"
              onClick={() => {
                setEditing(null);
                setFormOpen(true);
              }}
            >
              <Plus aria-hidden />
              New schedule
            </Button>
          ) : null}
        </div>
      </div>

      {actionError ? <ErrorAlert error={actionError} /> : null}
      {schedules.error ? <ErrorAlert error={schedules.error} onRetry={schedules.reload} /> : null}

      <div className="rounded-md border">
        {schedules.loading && !schedules.data ? (
          <div className="space-y-2 p-4">
            {Array.from({ length: 3 }).map((_, index) => (
              <Skeleton key={index} className="h-12 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={CalendarClock}
            title="No schedules"
            description="A cron schedule runs a prompt inside this instance and keeps its run history."
            action={
              canEdit ? (
                <Button
                  size="sm"
                  onClick={() => {
                    setEditing(null);
                    setFormOpen(true);
                  }}
                >
                  <Plus aria-hidden />
                  New schedule
                </Button>
              ) : undefined
            }
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Name</TableHead>
                    <TableHead className="w-[140px]">Cron</TableHead>
                    <TableHead className="w-[160px]">Next run</TableHead>
                    <TableHead className="w-[150px]">Last run</TableHead>
                    <TableHead className="w-[150px]">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((schedule) => (
                    <TableRow key={schedule.id}>
                      <TableCell>
                        <span className="block text-sm font-medium">{schedule.name}</span>
                        <span
                          className="block max-w-[320px] truncate text-xs text-muted-foreground"
                          title={schedule.prompt}
                        >
                          {schedule.prompt}
                        </span>
                      </TableCell>
                      <TableCell>
                        <span className="font-mono text-xs">{schedule.cron}</span>
                        {!schedule.enabled ? (
                          <Badge variant="outline" className="ml-1 text-[11px] font-normal">
                            disabled
                          </Badge>
                        ) : null}
                      </TableCell>
                      <TableCell className="font-mono text-xs">
                        {schedule.next_run_at ? formatDateTime(schedule.next_run_at) : "—"}
                      </TableCell>
                      <TableCell>
                        <span className="font-mono text-xs">
                          {schedule.last_run_at ? formatRelative(schedule.last_run_at) : "never"}
                        </span>
                      </TableCell>
                      <TableCell>
                        <div className="flex justify-end gap-1">
                          {canEdit ? (
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label={`Run ${schedule.name} now`}
                              title={`Run ${schedule.name} now`}
                              disabled={runningId === schedule.id}
                              onClick={() => void runNow(schedule)}
                            >
                              {runningId === schedule.id ? (
                                <Loader2 aria-hidden className="animate-spin" />
                              ) : (
                                <Play aria-hidden />
                              )}
                            </Button>
                          ) : null}
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            aria-label={`History of ${schedule.name}`}
                            title={`History of ${schedule.name}`}
                            onClick={() => setHistoryOpen(true)}
                          >
                            <History aria-hidden />
                          </Button>
                          {canEdit ? (
                            <>
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                aria-label={`Edit ${schedule.name}`}
                                title={`Edit ${schedule.name}`}
                                onClick={() => {
                                  setEditing(schedule);
                                  setFormOpen(true);
                                }}
                              >
                                <Pencil aria-hidden />
                              </Button>
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                aria-label={`Delete ${schedule.name}`}
                                title={`Delete ${schedule.name}`}
                                onClick={() => {
                                  setDeleteError(null);
                                  setDeleteTarget(schedule);
                                }}
                              >
                                <Trash2 aria-hidden />
                              </Button>
                            </>
                          ) : null}
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((schedule) => (
                <div key={schedule.id} className="space-y-3 p-4">
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0">
                      <p className="text-sm font-medium">{schedule.name}</p>
                      <p className="font-mono text-xs text-muted-foreground">{schedule.cron}</p>
                    </div>
                    {schedule.enabled ? (
                      <Badge variant="outline" className="text-[11px] font-normal">
                        enabled
                      </Badge>
                    ) : (
                      <Badge variant="outline" className="text-[11px] font-normal text-muted-foreground">
                        disabled
                      </Badge>
                    )}
                  </div>
                  <p className="text-xs text-muted-foreground">{schedule.prompt}</p>
                  <dl className="grid grid-cols-2 gap-2 text-xs">
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
                  </dl>
                  <div className="flex flex-wrap justify-end gap-1">
                    {canEdit ? (
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={runningId === schedule.id}
                        onClick={() => void runNow(schedule)}
                      >
                        {runningId === schedule.id ? (
                          <Loader2 aria-hidden className="animate-spin" />
                        ) : (
                          <Play aria-hidden />
                        )}
                        Run
                      </Button>
                    ) : null}
                    <Button variant="outline" size="sm" onClick={() => setHistoryOpen(true)}>
                      History
                    </Button>
                    {canEdit ? (
                      <>
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
                      </>
                    ) : null}
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
        instances={[{ id: instanceId, name: instanceId }]}
        fixedInstance={instanceId}
        schedule={editing}
        onSaved={() => setNonce((value) => value + 1)}
      />
      <RunHistorySheet
        open={historyOpen}
        onOpenChange={setHistoryOpen}
        instanceId={instanceId}
        schedules={rows}
      />
      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(next) => {
          if (!next) setDeleteTarget(null);
        }}
        title="Delete schedule"
        description={
          deleteTarget
            ? `Delete ${deleteTarget.name} and its run history? This cannot be undone.`
            : undefined
        }
        confirmLabel="Delete schedule"
        destructive
        pending={deleting}
        error={deleteError}
        onConfirm={() => void remove()}
      />
    </div>
  );
}

/** Global schedule table row actions, reused by the /schedules page. */
export function ScheduleRowActions({
  schedule,
  canEdit,
  onRun,
  onHistory,
  onEdit,
  onDelete,
  running,
}: {
  schedule: ScheduleView;
  canEdit: boolean;
  onRun: () => void;
  onHistory: () => void;
  onEdit: () => void;
  onDelete: () => void;
  running: boolean;
}) {
  return (
    <div className="flex justify-end gap-1">
      {canEdit ? (
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={`Run ${schedule.name} now`}
          title={`Run ${schedule.name} now`}
          disabled={running}
          onClick={onRun}
        >
          {running ? <Loader2 aria-hidden className="animate-spin" /> : <Play aria-hidden />}
        </Button>
      ) : null}
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label={`History of ${schedule.name}`}
        title={`History of ${schedule.name}`}
        onClick={onHistory}
      >
        <History aria-hidden />
      </Button>
      {canEdit ? (
        <>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={`Edit ${schedule.name}`}
            title={`Edit ${schedule.name}`}
            onClick={onEdit}
          >
            <Pencil aria-hidden />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={`Delete ${schedule.name}`}
            title={`Delete ${schedule.name}`}
            onClick={onDelete}
          >
            <Trash2 aria-hidden />
          </Button>
        </>
      ) : null}
    </div>
  );
}

export { ScheduleFormDialog, RunHistorySheet };
export type { InstanceOption };
