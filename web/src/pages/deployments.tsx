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
} from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { useIdentity } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

const DAYS = [
  { value: "mon", labelKey: "day.mon" },
  { value: "tue", labelKey: "day.tue" },
  { value: "wed", labelKey: "day.wed" },
  { value: "thu", labelKey: "day.thu" },
  { value: "fri", labelKey: "day.fri" },
  { value: "sat", labelKey: "day.sat" },
  { value: "sun", labelKey: "day.sun" },
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
  const { t, lang } = useI18n();
  const data = deployment.data;
  const rows = data ? results(data) : [];

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="w-full sm:max-w-xl">
        <SheetHeader>
          <SheetTitle>{t("deployments.detailTitle")}</SheetTitle>
          <SheetDescription>
            {data ? (
              <span className="font-mono">{data.id}</span>
            ) : (
              t("deployments.loadingRow")
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
                    {t("deployments.rollback")}
                  </Badge>
                ) : null}
                {data.canary > 0 ? (
                  <Badge variant="outline" className="font-normal">
                    {t("deployments.canaryBadge", { count: data.canary })}
                  </Badge>
                ) : null}
                {data.soak_s > 0 ? (
                  <Badge variant="outline" className="font-normal">
                    {t("deployments.soakBadge", { seconds: data.soak_s })}
                  </Badge>
                ) : null}
              </div>

              <dl className="grid grid-cols-2 gap-x-4 gap-y-2 rounded-md border p-3 text-xs">
                <div className="col-span-2">
                  <dt className="text-muted-foreground">{t("deployments.image")}</dt>
                  <dd className="font-mono break-all">{data.image}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">{t("deployments.started")}</dt>
                  <dd className="font-mono">
                    {data.started_at ? formatDateTime(data.started_at) : "—"}
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">{t("deployments.finished")}</dt>
                  <dd className="font-mono">
                    {data.finished_at
                      ? formatRelative(data.finished_at, lang)
                      : t("deployments.inProgress")}
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">{t("deployments.createdBy")}</dt>
                  <dd className="font-mono break-all">{data.created_by}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">{t("nav.instances")}</dt>
                  <dd className="font-mono">{data.instances.length}</dd>
                </div>
                {data.started_at && data.finished_at ? (
                  <div>
                    <dt className="text-muted-foreground">{t("deployments.wallTime")}</dt>
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
                  <p className="text-sm font-medium">{t("deployments.deferredRequest")}</p>
                  <pre className="overflow-auto font-mono text-[11px] whitespace-pre-wrap">
                    {JSON.stringify(data.result?.request ?? {}, null, 2)}
                  </pre>
                </div>
              ) : null}

              <div className="space-y-2">
                <h3 className="text-sm font-medium">{t("deployments.perInstance")}</h3>
                {rows.length === 0 ? (
                  <p className="rounded-md border px-3 py-4 text-sm text-muted-foreground">
                    {t("deployments.noResult")}
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
                          {row.from ?? t("deployments.noPreviousImage")} → {row.to}
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
  const { t } = useI18n();
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
      setError(toApiError(new Error(t("deployments.window.nameRequired"))));
      return;
    }
    if (days.length === 0) {
      setError(toApiError(new Error(t("deployments.window.dayRequired"))));
      return;
    }
    if (startMinute === null || endMinute === null) {
      setError(toApiError(new Error(t("deployments.window.timeInvalid"))));
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
      toast.success(
        target ? t("deployments.window.saved") : t("deployments.window.created"),
      );
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
          <DialogTitle>
            {target ? t("deployments.window.editTitle", { name: target.name }) : t("deployments.window.title")}
          </DialogTitle>
          <DialogDescription>
            {t("deployments.window.description")}
            <span className="mt-1 block font-mono text-[11px]">
              {t("deployments.window.bounds")}
            </span>
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}
          <div className="space-y-2">
            <Label htmlFor="window-name">{t("common.name")}</Label>
            <Input
              id="window-name"
              value={name}
              autoComplete="off"
              onChange={(event) => setName(event.target.value)}
            />
          </div>
          <fieldset className="space-y-2">
            <legend className="text-sm font-medium">{t("deployments.window.days")}</legend>
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
                    {t(day.labelKey)}
                  </Label>
                </div>
              ))}
            </div>
          </fieldset>
          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-2">
              <Label htmlFor="window-start">{t("deployments.window.start")}</Label>
              <Input
                id="window-start"
                type="time"
                value={start}
                className="font-mono"
                onChange={(event) => setStart(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="window-end">{t("deployments.window.end")}</Label>
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
              {t("common.enabled")}
            </Label>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              {t("deployments.window.save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function DeploymentsPage() {
  const { t } = useI18n();
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
      toast.success(t("deployments.window.deleted", { name: deleteTarget.name }));
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
        title={t("nav.deployments")}
        description={
          deployments.data
            ? t("deployments.summary", {
                count: rows.length,
                gate: windowStatus.data?.open
                  ? t("deployments.windowOpen")
                  : t("deployments.maintenanceGated"),
              })
            : t("deployments.loading")
        }
        actions={
          <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
            <RefreshCw
              aria-hidden
              className={deployments.refreshing || windows.refreshing ? "animate-spin" : undefined}
            />
            {t("common.refresh")}
          </Button>
        }
      />

      <Tabs value={tab} onValueChange={setTab}>
        <div className="overflow-x-auto">
          <TabsList className="w-full justify-start sm:w-fit">
            <TabsTrigger value="deployments">{t("nav.deployments")}</TabsTrigger>
            <TabsTrigger value="windows">{t("deployments.windowsTab")}</TabsTrigger>
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
              <SelectTrigger className="w-full sm:w-44" aria-label={t("fleet.filterAria")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{t("incidents.allStatuses")}</SelectItem>
                {STATUSES.filter(Boolean).map((value) => (
                  <SelectItem key={value} value={value}>
                    {t(`status.${value}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select value={limit} onValueChange={setLimit}>
              <SelectTrigger className="w-full sm:w-36" aria-label={t("deployments.rowsAria")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="50">{t("deployments.rowsN", { count: 50 })}</SelectItem>
                <SelectItem value="200">{t("deployments.rowsN", { count: 200 })}</SelectItem>
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground sm:ml-auto" aria-live="polite">
              {t("schedules.shown", { count: rows.length })}
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
                title={t("deployments.emptyTitle")}
                description={t("deployments.emptyDescription")}
              />
            ) : (
              <>
                <div className="hidden md:block">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead className="w-[160px]">{t("deployments.startedPlanned")}</TableHead>
                        <TableHead>{t("deployments.image")}</TableHead>
                        <TableHead className="w-[120px]">{t("common.status")}</TableHead>
                        <TableHead className="w-[130px]">{t("nav.instances")}</TableHead>
                        <TableHead className="w-[130px]">{t("deployments.canarySoak")}</TableHead>
                        <TableHead className="w-[90px]">
                          <span className="sr-only">{t("common.open")}</span>
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
                              {t("common.open")}
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
                        {t("deployments.mobileSummary", {
                          count: deployment.instances.length,
                          canary: deployment.canary,
                          soak: deployment.soak_s,
                        })}
                      </p>
                      <Button variant="outline" size="sm" onClick={() => setSelected(deployment.id)}>
                        {t("deployments.openDeployment")}
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
              label={
                windowStatus.data?.open
                  ? t("deployments.windowOpen")
                  : t("deployments.windowClosed")
              }
            />
            <span className="text-xs text-muted-foreground">
              {windowStatus.data?.enforce
                ? t("deployments.enforceActive")
                : t("deployments.gateInert")}
            </span>
            {windowStatus.data?.next_open ? (
              <span className="font-mono text-xs text-muted-foreground">
                {t("deployments.nextOpen", {
                  time: formatUtcDateTime(windowStatus.data.next_open),
                })}
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
                  {t("deployments.newWindow")}
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
                title={t("deployments.noWindowTitle")}
                description={t("deployments.noWindowDescription")}
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
                      {t("deployments.newWindow")}
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
                            {t("common.disabled")}
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
                          aria-label={t("deployments.editWindowAria", { name: window.name })}
                          title={t("deployments.editWindowAria", { name: window.name })}
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
                          aria-label={t("deployments.deleteWindowAria", { name: window.name })}
                          title={t("deployments.deleteWindowAria", { name: window.name })}
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
            {t("deployments.windowFootnote")}
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
        title={t("deployments.deleteWindowTitle")}
        description={
          deleteTarget
            ? t("deployments.deleteWindowDescription", { name: deleteTarget.name })
            : undefined
        }
        confirmLabel={t("deployments.deleteWindow")}
        destructive
        pending={deleting}
        error={deleteError}
        onConfirm={() => void removeWindow()}
      />
    </>
  );
}
