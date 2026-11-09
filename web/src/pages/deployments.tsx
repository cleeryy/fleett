import { useEffect, useState, type FormEvent } from "react";
import { CalendarClock, Loader2, Pencil, Plus, RefreshCw, Rocket, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type {
  Deployment,
  DeploymentInstanceResult,
  DeploymentStatus,
  MaintenanceWindow,
  MaintenanceWindowInput,
} from "@/api/types";
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { OpsBadge } from "@/components/ops-badge";
import { PageHeader } from "@/components/page-header";
import {
  formatDateTime,
  formatDurationMs,
  formatMinuteOfDay,
  formatRelative,
  formatUtcDateTime,
  plural,
} from "@/lib/format";
import { useIdentity } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

const DAYS = [
  { value: "mon", label: "Mon" },
  { value: "tue", label: "Tue" },
  { value: "wed", label: "Wed" },
  { value: "thu", label: "Thu" },
  { value: "fri", label: "Fri" },
  { value: "sat", label: "Sat" },
  { value: "sun", label: "Sun" },
];

const STATUSES: Array<"" | DeploymentStatus> = [
  "",
  "scheduled",
  "running",
  "canary",
  "ok",
  "failed",
  "rolled_back",
  "cancelled",
];

function results(deployment: Deployment): DeploymentInstanceResult[] {
  return Array.isArray(deployment.result) ? deployment.result : [];
}

function shift(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "—";
  return formatDurationMs(ms);
}

function DeploymentSheet({
  deploymentId,
  open,
  onOpenChange,
}: {
  deploymentId: string | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const deployment = useResource(
    () => (deploymentId ? api.getDeployment(deploymentId) : Promise.resolve<Deployment | null>(null)),
    [deploymentId, open],
  );
  const data = deployment.data;
  const rows = data ? results(data) : [];

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="w-full sm:max-w-xl">
        <SheetHeader>
          <SheetTitle>Deployment</SheetTitle>
          <SheetDescription>
            {data ? (
              <span className="font-mono">{data.id}</span>
            ) : (
              "Loading the journal row."
            )}
          </SheetDescription>
        </SheetHeader>
        <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4">
          {deployment.error ? (
            <ErrorAlert error={deployment.error} onRetry={deployment.reload} />
          ) : !data ? (
            <Skeleton className="h-40 w-full" />
          ) : (
            <div className="space-y-4">
              <div className="flex flex-wrap items-center gap-2">
                <OpsBadge status={data.status} />
                {data.rollback ? (
                  <Badge variant="outline" className="font-normal">
                    rollback
                  </Badge>
                ) : null}
                {data.canary > 0 ? (
                  <Badge variant="outline" className="font-normal">
                    canary {data.canary}
                  </Badge>
                ) : null}
                {data.soak_s > 0 ? (
                  <Badge variant="outline" className="font-normal">
                    soak {data.soak_s}s
                  </Badge>
                ) : null}
              </div>

              <dl className="grid grid-cols-2 gap-x-4 gap-y-2 rounded-md border p-3 text-xs">
                <div className="col-span-2">
                  <dt className="text-muted-foreground">Image</dt>
                  <dd className="font-mono break-all">{data.image}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Started</dt>
                  <dd className="font-mono">
                    {data.started_at ? formatDateTime(data.started_at) : "—"}
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Finished</dt>
                  <dd className="font-mono">
                    {data.finished_at ? formatRelative(data.finished_at) : "in progress"}
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Created by</dt>
                  <dd className="font-mono break-all">{data.created_by}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Instances</dt>
                  <dd className="font-mono">{data.instances.length}</dd>
                </div>
                {data.started_at && data.finished_at ? (
                  <div>
                    <dt className="text-muted-foreground">Wall time</dt>
                    <dd className="font-mono">
                      {shift(Date.parse(data.finished_at) - Date.parse(data.started_at))}
                    </dd>
                  </div>
                ) : null}
              </dl>

              {data.error ? (
                <p className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 font-mono text-xs whitespace-pre-wrap text-destructive">
                  {data.error}
                </p>
              ) : null}

              {data.status === "scheduled" && !Array.isArray(data.result) ? (
                <div className="space-y-1 rounded-md border p-3">
                  <p className="text-sm font-medium">Deferred request</p>
                  <pre className="overflow-auto font-mono text-[11px] whitespace-pre-wrap">
                    {JSON.stringify(data.result?.request ?? {}, null, 2)}
                  </pre>
                </div>
              ) : null}

              <div className="space-y-2">
                <h3 className="text-sm font-medium">Per-instance result</h3>
                {rows.length === 0 ? (
                  <p className="rounded-md border px-3 py-4 text-sm text-muted-foreground">
                    No per-instance result recorded yet.
                  </p>
                ) : (
                  <ul className="divide-y rounded-md border">
                    {rows.map((row) => (
                      <li key={row.id} className="space-y-1 p-3">
                        <div className="flex flex-wrap items-center justify-between gap-2">
                          <a
                            href={`#/instances/${encodeURIComponent(row.id)}`}
                            className="font-mono text-xs underline-offset-4 hover:underline"
                          >
                            {row.id}
                          </a>
                          <OpsBadge status={row.status} />
                        </div>
                        <p className="font-mono text-[11px] break-all text-muted-foreground">
                          {row.from ?? "no previous image"} → {row.to}
                        </p>
                        {row.error ? (
                          <p className="font-mono text-[11px] break-words whitespace-pre-wrap text-destructive">
                            {row.error}
                          </p>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            </div>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}

function timeValue(minutes: number): string {
  return formatMinuteOfDay(minutes);
}

function parseTime(value: string): number | null {
  const match = /^(\d{2}):(\d{2})$/.exec(value);
  if (!match) return null;
  const hours = Number(match[1]);
  const minutes = Number(match[2]);
  if (hours > 23 || minutes > 59) return null;
  return hours * 60 + minutes;
}

function WindowDialog({
  open,
  onOpenChange,
  window: target,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  window: MaintenanceWindow | null;
  onSaved: () => void;
}) {
  const [name, setName] = useState("");
  const [days, setDays] = useState<string[]>(["mon", "tue", "wed", "thu", "fri"]);
  const [start, setStart] = useState("02:00");
  const [end, setEnd] = useState("05:00");
  const [enabled, setEnabled] = useState(true);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setName(target?.name ?? "nightly");
    setDays(target ? [...target.days] : ["mon", "tue", "wed", "thu", "fri"]);
    setStart(target ? timeValue(target.start_minute) : "02:00");
    setEnd(target ? timeValue(target.end_minute) : "05:00");
    setEnabled(target?.enabled ?? true);
    setError(null);
  }, [open, target]);

  function toggleDay(day: string, checked: boolean) {
    setDays((previous) =>
      checked ? [...new Set([...previous, day])] : previous.filter((value) => value !== day),
    );
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const startMinute = parseTime(start);
    const endMinute = parseTime(end);
    if (!name.trim()) {
      setError(toApiError(new Error("Enter a window name.")));
      return;
    }
    if (days.length === 0) {
      setError(toApiError(new Error("Pick at least one day.")));
      return;
    }
    if (startMinute === null || endMinute === null) {
      setError(toApiError(new Error("Use HH:MM for the start and end times.")));
      return;
    }
    const input: MaintenanceWindowInput = {
      name: name.trim(),
      days,
      start_minute: startMinute,
      end_minute: endMinute,
      enabled,
    };
    setPending(true);
    setError(null);
    try {
      if (target) await api.patchMaintenanceWindow(target.id, input);
      else await api.createMaintenanceWindow(input);
      toast.success(target ? "Maintenance window saved." : "Maintenance window created.");
      onOpenChange(false);
      onSaved();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{target ? `Edit ${target.name}` : "New maintenance window"}</DialogTitle>
          <DialogDescription>
            Recurring UTC ranges. Destructive operations are refused while no enabled window is open.
            <span className="mt-1 block font-mono text-[11px]">
              start inclusive · end exclusive · end before start means overnight
            </span>
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}
          <div className="space-y-2">
            <Label htmlFor="window-name">Name</Label>
            <Input
              id="window-name"
              value={name}
              autoComplete="off"
              onChange={(event) => setName(event.target.value)}
            />
          </div>
          <fieldset className="space-y-2">
            <legend className="text-sm font-medium">Days</legend>
            <div className="flex flex-wrap gap-3">
              {DAYS.map((day) => (
                <div key={day.value} className="flex items-center gap-1.5">
                  <Checkbox
                    id={`window-day-${day.value}`}
                    checked={days.includes(day.value)}
                    onCheckedChange={(value) => toggleDay(day.value, value === true)}
                  />
                  <Label
                    htmlFor={`window-day-${day.value}`}
                    className="cursor-pointer text-sm font-normal"
                  >
                    {day.label}
                  </Label>
                </div>
              ))}
            </div>
          </fieldset>
          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-2">
              <Label htmlFor="window-start">Start (UTC)</Label>
              <Input
                id="window-start"
                type="time"
                value={start}
                className="font-mono"
                onChange={(event) => setStart(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="window-end">End (UTC)</Label>
              <Input
                id="window-end"
                type="time"
                value={end}
                className="font-mono"
                onChange={(event) => setEnd(event.target.value)}
              />
            </div>
          </div>
          <div className="flex items-center gap-2">
            <Checkbox
              id="window-enabled"
              checked={enabled}
              onCheckedChange={(value) => setEnabled(value === true)}
            />
            <Label htmlFor="window-enabled" className="cursor-pointer text-sm font-normal">
              Enabled
            </Label>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              Save window
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function DeploymentsPage() {
  const identity = useIdentity();
  const admin = identity?.role === "admin";
  const [tab, setTab] = useState("deployments");
  const [nonce, setNonce] = useState(0);
  const [status, setStatus] = useState<"" | DeploymentStatus>("");
  const [limit, setLimit] = useState("50");
  const [selected, setSelected] = useState<string | null>(null);
  const [windowOpen, setWindowOpen] = useState(false);
  const [windowTarget, setWindowTarget] = useState<MaintenanceWindow | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<MaintenanceWindow | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<ApiError | null>(null);

  const deployments = useResource(
    () =>
      api.listDeployments({
        limit: Number(limit),
        ...(status ? { status } : {}),
      }),
    [nonce, status, limit],
  );
  const windows = useResource(() => api.listMaintenanceWindows(), [nonce]);
  const windowStatus = useResource(() => api.maintenanceStatus(), [nonce]);
  const rows = deployments.data ?? [];
  const windowRows = windows.data ?? [];

  async function removeWindow() {
    if (!deleteTarget) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      await api.deleteMaintenanceWindow(deleteTarget.id);
      toast.success(`Window ${deleteTarget.name} deleted.`);
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
        title="Deployments"
        description={
          deployments.data
            ? `${rows.length} ${plural(rows.length, "deployment")} in the journal · ${windowStatus.data?.open ? "maintenance window open" : "maintenance gated"}`
            : "Loading the deployment journal."
        }
        actions={
          <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
            <RefreshCw
              aria-hidden
              className={deployments.refreshing || windows.refreshing ? "animate-spin" : undefined}
            />
            Refresh
          </Button>
        }
      />

      <Tabs value={tab} onValueChange={setTab}>
        <div className="overflow-x-auto">
          <TabsList className="w-full justify-start sm:w-fit">
            <TabsTrigger value="deployments">Deployments</TabsTrigger>
            <TabsTrigger value="windows">Maintenance windows</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="deployments" className="mt-4 space-y-3">
          {deployments.error ? (
            <ErrorAlert error={deployments.error} onRetry={deployments.reload} />
          ) : null}
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
            <Select
              value={status || "all"}
              onValueChange={(value) => setStatus(value === "all" ? "" : (value as DeploymentStatus))}
            >
              <SelectTrigger className="w-full sm:w-44" aria-label="Filter by status">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All statuses</SelectItem>
                {STATUSES.filter(Boolean).map((value) => (
                  <SelectItem key={value} value={value}>
                    {value}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select value={limit} onValueChange={setLimit}>
              <SelectTrigger className="w-full sm:w-36" aria-label="Rows to load">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="50">50 rows</SelectItem>
                <SelectItem value="200">200 rows</SelectItem>
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground sm:ml-auto" aria-live="polite">
              {rows.length} shown
            </p>
          </div>

          <div className="rounded-md border">
            {deployments.loading && !deployments.data ? (
              <div className="space-y-2 p-4">
                {Array.from({ length: 4 }).map((_, index) => (
                  <Skeleton key={index} className="h-14 w-full" />
                ))}
              </div>
            ) : rows.length === 0 ? (
              <EmptyState
                icon={Rocket}
                title="No deployment recorded"
                description="Every upgrade — immediate, canary or scheduled — lands in this journal."
              />
            ) : (
              <>
                <div className="hidden md:block">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead className="w-[160px]">Started / planned</TableHead>
                        <TableHead>Image</TableHead>
                        <TableHead className="w-[120px]">Status</TableHead>
                        <TableHead className="w-[130px]">Instances</TableHead>
                        <TableHead className="w-[130px]">Canary / soak</TableHead>
                        <TableHead className="w-[90px]">
                          <span className="sr-only">Open</span>
                        </TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {rows.map((deployment) => (
                        <TableRow key={deployment.id}>
                          <TableCell className="font-mono text-xs">
                            {deployment.started_at ? formatDateTime(deployment.started_at) : "—"}
                          </TableCell>
                          <TableCell className="font-mono text-xs break-all">
                            {deployment.image}
                            <span className="block text-[11px] text-muted-foreground">
                              {deployment.created_by}
                            </span>
                          </TableCell>
                          <TableCell>
                            <OpsBadge status={deployment.status} />
                          </TableCell>
                          <TableCell className="font-mono text-xs">
                            {deployment.instances.length}{" "}
                            <span className="text-muted-foreground">
                              · {deployment.instances.slice(0, 3).join(", ")}
                              {deployment.instances.length > 3 ? "…" : ""}
                            </span>
                          </TableCell>
                          <TableCell className="font-mono text-xs">
                            {deployment.canary} / {deployment.soak_s}s
                          </TableCell>
                          <TableCell className="text-right">
                            <Button variant="outline" size="sm" onClick={() => setSelected(deployment.id)}>
                              Open
                            </Button>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
                <ul className="divide-y md:hidden">
                  {rows.map((deployment) => (
                    <li key={deployment.id} className="space-y-2 p-4">
                      <div className="flex items-start justify-between gap-2">
                        <div className="min-w-0">
                          <p className="font-mono text-xs break-all">{deployment.image}</p>
                          <p className="text-[11px] text-muted-foreground">
                            {deployment.started_at ? formatDateTime(deployment.started_at) : "—"}
                          </p>
                        </div>
                        <OpsBadge status={deployment.status} />
                      </div>
                      <p className="text-xs text-muted-foreground">
                        {deployment.instances.length} instances · canary {deployment.canary} · soak{" "}
                        {deployment.soak_s}s
                      </p>
                      <Button variant="outline" size="sm" onClick={() => setSelected(deployment.id)}>
                        Open deployment
                      </Button>
                    </li>
                  ))}
                </ul>
              </>
            )}
          </div>
        </TabsContent>

        <TabsContent value="windows" className="mt-4 space-y-3">
          <div className="flex flex-wrap items-center gap-2">
            <OpsBadge
              status={windowStatus.data?.open ? "open" : "closed"}
              label={windowStatus.data?.open ? "window open" : "closed"}
            />
            <span className="text-xs text-muted-foreground">
              {windowStatus.data?.enforce
                ? "Enforcement active: an operation outside a window is refused."
                : "No enabled window: the gate is inert."}
            </span>
            {windowStatus.data?.next_open ? (
              <span className="font-mono text-xs text-muted-foreground">
                next open {formatUtcDateTime(windowStatus.data.next_open)}
              </span>
            ) : null}
            <div className="ml-auto">
              {admin ? (
                <Button
                  size="sm"
                  onClick={() => {
                    setWindowTarget(null);
                    setWindowOpen(true);
                  }}
                >
                  <Plus aria-hidden />
                  New window
                </Button>
              ) : null}
            </div>
          </div>

          {windows.error ? <ErrorAlert error={windows.error} onRetry={windows.reload} /> : null}
          <div className="rounded-md border">
            {windows.loading && !windows.data ? (
              <div className="space-y-2 p-4">
                {Array.from({ length: 2 }).map((_, index) => (
                  <Skeleton key={index} className="h-16 w-full" />
                ))}
              </div>
            ) : windowRows.length === 0 ? (
              <EmptyState
                icon={CalendarClock}
                title="No maintenance window"
                description="Without an enabled window, destructive operations are not gated."
                action={
                  admin ? (
                    <Button
                      size="sm"
                      onClick={() => {
                        setWindowTarget(null);
                        setWindowOpen(true);
                      }}
                    >
                      <Plus aria-hidden />
                      New window
                    </Button>
                  ) : undefined
                }
              />
            ) : (
              <ul className="divide-y">
                {windowRows.map((window) => (
                  <li key={window.id} className="flex flex-wrap items-center gap-3 p-4">
                    <div className="min-w-0 flex-1">
                      <p className="text-sm font-medium">
                        {window.name}{" "}
                        {!window.enabled ? (
                          <Badge variant="outline" className="ml-1 text-[11px] font-normal">
                            disabled
                          </Badge>
                        ) : null}
                      </p>
                      <p className="font-mono text-xs text-muted-foreground">
                        {window.days.join(", ")} · {formatMinuteOfDay(window.start_minute)}–
                        {formatMinuteOfDay(window.end_minute)} UTC
                      </p>
                    </div>
                    {admin ? (
                      <div className="flex items-center gap-1">
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={`Edit ${window.name}`}
                          title={`Edit ${window.name}`}
                          onClick={() => {
                            setWindowTarget(window);
                            setWindowOpen(true);
                          }}
                        >
                          <Pencil aria-hidden />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={`Delete ${window.name}`}
                          title={`Delete ${window.name}`}
                          onClick={() => {
                            setDeleteError(null);
                            setDeleteTarget(window);
                          }}
                        >
                          <Trash2 aria-hidden />
                        </Button>
                      </div>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
          </div>
          <p className="text-xs text-muted-foreground">
            Minutes are UTC: start inclusive, end exclusive. An end before the start is an overnight
            window owned by its start day.
          </p>
        </TabsContent>
      </Tabs>

      <DeploymentSheet
        deploymentId={selected}
        open={selected !== null}
        onOpenChange={(open) => {
          if (!open) setSelected(null);
        }}
      />
      <WindowDialog
        open={windowOpen}
        onOpenChange={setWindowOpen}
        window={windowTarget}
        onSaved={() => setNonce((value) => value + 1)}
      />
      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        title="Delete maintenance window"
        description={
          deleteTarget
            ? `Delete ${deleteTarget.name}? Future upgrades lose this gate.`
            : undefined
        }
        confirmLabel="Delete window"
        destructive
        pending={deleting}
        error={deleteError}
        onConfirm={() => void removeWindow()}
      />
    </>
  );
}
