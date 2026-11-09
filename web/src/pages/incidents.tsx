import { useEffect, useMemo, useState, type FormEvent } from "react";
import { HeartPulse, Loader2, RefreshCw, Search, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { HealPolicyInput, HealPolicyView, Incident, IncidentStatus } from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
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
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { OpsBadge } from "@/components/ops-badge";
import { PageHeader } from "@/components/page-header";
import { formatDateTime, formatRelative } from "@/lib/format";
import { hasTranslation, useI18n } from "@/lib/i18n";
import { useIdentity } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

function CauseLabel({ cause }: { cause: string }) {
  const { t } = useI18n();
  const key = `incidentCause.${cause}`;
  return (
    <span className="font-mono text-xs">
      {hasTranslation(key) ? t(key) : cause.replace(/_/g, " ")}
    </span>
  );
}

function IncidentActions({ incident }: { incident: Incident }) {
  const { t } = useI18n();
  if (incident.actions.length === 0) {
    return <span className="text-xs text-muted-foreground">{t("common.none")}</span>;
  }
  return (
    <details>
      <summary className="cursor-pointer text-xs text-muted-foreground hover:text-foreground">
        {t(
          incident.actions.length === 1
            ? "incidents.actionsOne"
            : "incidents.actionsMany",
          { count: incident.actions.length, restarts: incident.restart_count },
        )}
      </summary>
      <ul className="mt-1 space-y-1">
        {incident.actions.map((action) => (
          <li key={`${action.at}-${action.action}`} className="flex flex-wrap items-center gap-2 text-xs">
            <span className="font-mono text-[11px] text-muted-foreground">{formatDateTime(action.at)}</span>
            <span className="font-mono">{action.action}</span>
            <OpsBadge status={action.result} />
            {action.detail ? <span className="text-muted-foreground">{action.detail}</span> : null}
          </li>
        ))}
      </ul>
    </details>
  );
}

function HealDialog({
  instanceId,
  current,
  open,
  onOpenChange,
  onSaved,
}: {
  instanceId: string;
  current: HealPolicyView | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved: (policy: HealPolicyView) => void;
}) {
  const { t } = useI18n();
  const [heal, setHeal] = useState("keep");
  const [unhealthyAfter, setUnhealthyAfter] = useState("");
  const [maxRestarts, setMaxRestarts] = useState("");
  const [backoff, setBackoff] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [saved, setSaved] = useState<HealPolicyView | null>(current);

  useEffect(() => {
    if (!open) return;
    setHeal("keep");
    setUnhealthyAfter("");
    setMaxRestarts("");
    setBackoff("");
    setError(null);
    setSaved(current);
  }, [open, instanceId, current]);

  function reset() {
    setHeal("keep");
    setUnhealthyAfter("");
    setMaxRestarts("");
    setBackoff("");
    setError(null);
    setSaved(current);
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const input: HealPolicyInput = {};
    if (heal !== "keep") input.heal = heal as HealPolicyInput["heal"];
    if (unhealthyAfter.trim()) {
      const value = Number(unhealthyAfter);
      if (!Number.isInteger(value) || value < 1 || value > 100) {
        setError(toApiError(new Error(t("incidents.heal.rangeUnhealthy"))));
        return;
      }
      input.unhealthy_after = value;
    }
    if (maxRestarts.trim()) {
      const value = Number(maxRestarts);
      if (!Number.isInteger(value) || value < 0 || value > 100) {
        setError(toApiError(new Error(t("incidents.heal.rangeMaxRestarts"))));
        return;
      }
      input.max_restarts = value;
    }
    if (backoff.trim()) {
      const value = Number(backoff);
      if (!Number.isInteger(value) || value < 1 || value > 3_600) {
        setError(toApiError(new Error(t("incidents.heal.rangeBackoff"))));
        return;
      }
      input.heal_backoff_s = value;
    }
    if (Object.keys(input).length === 0) {
      setError(toApiError(new Error(t("incidents.heal.noChange"))));
      return;
    }
    setPending(true);
    setError(null);
    try {
      const policy = await api.patchHeal(instanceId, input);
      toast.success(t("incidents.heal.saved", { instance: instanceId }));
      setSaved(policy);
      onSaved(policy);
      onOpenChange(false);
      reset();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  const value = saved ?? current;

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next);
        if (!next) reset();
      }}
    >
      <DialogContent className="max-h-[85dvh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("incidents.heal.title", { instance: instanceId })}</DialogTitle>
          <DialogDescription>{t("incidents.heal.description")}</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}
          {value ? (
            <p className="rounded-md border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
              {t("incidents.heal.lastSaved", {
                heal: hasTranslation(`heal.${value.heal}`)
                  ? t(`heal.${value.heal}`)
                  : value.heal,
                after: value.unhealthy_after,
                max: value.max_restarts,
                backoff: value.heal_backoff_s,
              })}
            </p>
          ) : (
            <Alert>
              <HeartPulse aria-hidden />
              <AlertTitle>{t("incidents.heal.notLoaded")}</AlertTitle>
              <AlertDescription>{t("incidents.heal.notLoadedBody")}</AlertDescription>
            </Alert>
          )}

          <div className="space-y-2">
            <Label htmlFor="heal-mode">{t("incidents.heal.action")}</Label>
            <Select value={heal} onValueChange={setHeal}>
              <SelectTrigger id="heal-mode" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="keep">{t("incidents.heal.keep")}</SelectItem>
                <SelectItem value="off">{t("incidents.heal.observe")}</SelectItem>
                <SelectItem value="restart">{t("incidents.heal.restart")}</SelectItem>
                <SelectItem value="recreate">{t("incidents.heal.recreate")}</SelectItem>
              </SelectContent>
            </Select>
          </div>

          <div className="grid gap-4 sm:grid-cols-3">
            <div className="space-y-2">
              <Label htmlFor="heal-after">{t("incidents.heal.unhealthyAfter")}</Label>
              <Input
                id="heal-after"
                type="number"
                min={1}
                max={100}
                value={unhealthyAfter}
                placeholder={
                  value ? String(value.unhealthy_after) : t("incidents.heal.keepShort")
                }
                className="font-mono"
                onChange={(event) => setUnhealthyAfter(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="heal-max">{t("incidents.heal.maxRestarts")}</Label>
              <Input
                id="heal-max"
                type="number"
                min={0}
                max={100}
                value={maxRestarts}
                placeholder={
                  value ? String(value.max_restarts) : t("incidents.heal.keepShort")
                }
                className="font-mono"
                onChange={(event) => setMaxRestarts(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="heal-backoff">{t("incidents.heal.backoff")}</Label>
              <Input
                id="heal-backoff"
                type="number"
                min={1}
                max={3_600}
                value={backoff}
                placeholder={
                  value ? String(value.heal_backoff_s) : t("incidents.heal.keepShort")
                }
                className="font-mono"
                onChange={(event) => setBackoff(event.target.value)}
              />
            </div>
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              {t("incidents.heal.save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function IncidentsPage() {
  const { t, lang } = useI18n();
  const admin = useIdentity()?.role === "admin";
  const [tab, setTab] = useState("incidents");
  const [nonce, setNonce] = useState(0);
  const [status, setStatus] = useState<"all" | IncidentStatus>("open");
  const [instance, setInstance] = useState("all");
  const [q, setQ] = useState("");
  const [closingId, setClosingId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);
  const [healTarget, setHealTarget] = useState<string | null>(null);
  const [policies, setPolicies] = useState<Record<string, HealPolicyView>>({});

  const incidents = useResource(
    () =>
      api.listIncidents({
        ...(status === "all" ? {} : { status }),
        ...(instance === "all" ? {} : { instance }),
        limit: 200,
      }),
    [nonce, status, instance],
  );
  const instances = useResource(() => api.listInstances(), []);
  const rows = incidents.data ?? [];
  const instanceOptions = useMemo(
    () => [...new Set([...(instances.data ?? []).map((row) => row.id), ...rows.map((row) => row.instance_id)])].sort(),
    [instances.data, rows],
  );
  const filtered = useMemo(() => {
    const needle = q.trim().toLowerCase();
    if (!needle) return rows;
    return rows.filter((incident) =>
      [incident.instance_id, incident.cause, incident.detail].some((value) =>
        value.toLowerCase().includes(needle),
      ),
    );
  }, [rows, q]);
  const openCount = rows.filter((incident) => incident.status === "open").length;

  async function close(incident: Incident) {
    setClosingId(incident.id);
    setActionError(null);
    try {
      await api.closeIncident(incident.id);
      toast.success(t("incidents.closed", { instance: incident.instance_id }));
      setNonce((value) => value + 1);
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setClosingId(null);
    }
  }

  return (
    <>
      <PageHeader
        title={t("nav.incidents")}
        description={
          incidents.data
            ? t("incidents.summary", { open: openCount, total: rows.length })
            : t("incidents.loading")
        }
        actions={
          <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
            <RefreshCw aria-hidden className={incidents.refreshing ? "animate-spin" : undefined} />
            {t("common.refresh")}
          </Button>
        }
      />

      {actionError ? <ErrorAlert error={actionError} /> : null}

      <Tabs value={tab} onValueChange={setTab}>
        <div className="overflow-x-auto">
          <TabsList className="w-full justify-start sm:w-fit">
            <TabsTrigger value="incidents">{t("nav.incidents")}</TabsTrigger>
            <TabsTrigger value="heal">{t("incidents.healPolicies")}</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="incidents" className="mt-4 space-y-3">
          {incidents.error ? <ErrorAlert error={incidents.error} onRetry={incidents.reload} /> : null}

          <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
            <div className="relative w-full sm:max-w-xs">
              <Search
                aria-hidden
                className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground"
              />
              <Input
                value={q}
                onChange={(event) => setQ(event.target.value)}
                placeholder={t("incidents.searchPlaceholder")}
                aria-label={t("incidents.searchAria")}
                className="pl-8"
              />
            </div>
            <Select value={status} onValueChange={(value) => setStatus(value as "all" | IncidentStatus)}>
              <SelectTrigger className="w-full sm:w-40" aria-label={t("fleet.filterAria")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{t("incidents.allStatuses")}</SelectItem>
                <SelectItem value="open">{t("status.open")}</SelectItem>
                <SelectItem value="closed">{t("status.closed")}</SelectItem>
              </SelectContent>
            </Select>
            <Select value={instance} onValueChange={setInstance}>
              <SelectTrigger className="w-full sm:w-48" aria-label={t("schedules.filterAria")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{t("logs.allInstances")}</SelectItem>
                {instanceOptions.map((id) => (
                  <SelectItem key={id} value={id}>
                    {id}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground sm:ml-auto" aria-live="polite">
              {t("schedules.shown", { count: filtered.length })}
            </p>
          </div>

          <div className="rounded-md border">
            {incidents.loading && !incidents.data ? (
              <div className="space-y-2 p-4">
                {Array.from({ length: 4 }).map((_, index) => (
                  <Skeleton key={index} className="h-14 w-full" />
                ))}
              </div>
            ) : filtered.length === 0 ? (
              <EmptyState
                icon={TriangleAlert}
                title={
                  rows.length === 0 ? t("incidents.emptyTitle") : t("incidents.noMatchTitle")
                }
                description={
                  status === "open"
                    ? t("incidents.emptyOpenDescription")
                    : t("incidents.noMatchDescription")
                }
              />
            ) : (
              <>
                <div className="hidden lg:block">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead className="w-[160px]">{t("incidents.opened")}</TableHead>
                        <TableHead className="w-[120px]">{t("messages.instance")}</TableHead>
                        <TableHead className="w-[120px]">{t("incidents.cause")}</TableHead>
                        <TableHead>{t("incidents.detail")}</TableHead>
                        <TableHead className="w-[150px]">{t("common.status")}</TableHead>
                        <TableHead className="w-[110px]">
                          <span className="sr-only">{t("common.actions")}</span>
                        </TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {filtered.map((incident) => (
                        <TableRow key={incident.id}>
                          <TableCell>
                            <span className="block font-mono text-xs">{formatDateTime(incident.opened_at)}</span>
                            <span className="block text-[11px] text-muted-foreground">
                              {t("incidents.lastSeen", {
                                time: formatRelative(incident.last_seen_at, lang),
                              })}
                            </span>
                          </TableCell>
                          <TableCell>
                            <a
                              href={`#/instances/${encodeURIComponent(incident.instance_id)}`}
                              className="font-mono text-xs underline-offset-4 hover:underline"
                            >
                              {incident.instance_id}
                            </a>
                          </TableCell>
                          <TableCell>
                            <CauseLabel cause={incident.cause} />
                          </TableCell>
                          <TableCell className="max-w-[360px] text-xs whitespace-normal">
                            <p>{incident.detail}</p>
                            <IncidentActions incident={incident} />
                          </TableCell>
                          <TableCell>
                            <span className="flex flex-col items-start gap-1">
                              <OpsBadge status={incident.status} />
                              {incident.closed_at ? (
                                <span className="text-[11px] text-muted-foreground">
                                  {formatRelative(incident.closed_at, lang)}
                                </span>
                              ) : null}
                            </span>
                          </TableCell>
                          <TableCell className="text-right">
                            {incident.status === "open" && admin ? (
                              <Button
                                variant="outline"
                                size="sm"
                                disabled={closingId === incident.id}
                                onClick={() => void close(incident)}
                              >
                                {closingId === incident.id ? (
                                  <Loader2 aria-hidden className="animate-spin" />
                                ) : null}
                                {t("incidents.close")}
                              </Button>
                            ) : null}
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>

                <div className="divide-y lg:hidden">
                  {filtered.map((incident) => (
                    <div key={incident.id} className="space-y-3 p-4">
                      <div className="flex items-start justify-between gap-2">
                        <div className="min-w-0">
                          <a
                            href={`#/instances/${encodeURIComponent(incident.instance_id)}`}
                            className="text-sm font-medium"
                          >
                            {incident.instance_id}
                          </a>
                          <p className="font-mono text-[11px] text-muted-foreground">
                            {formatDateTime(incident.opened_at)}
                          </p>
                        </div>
                        <OpsBadge status={incident.status} />
                      </div>
                      <div className="flex flex-wrap items-center gap-2 text-xs">
                        <CauseLabel cause={incident.cause} />
                        <span className="text-muted-foreground">·</span>
                        <span className="text-muted-foreground">
                          {t("incidents.restartCount", { count: incident.restart_count })}
                        </span>
                      </div>
                      <p className="text-xs">{incident.detail}</p>
                      <IncidentActions incident={incident} />
                      {incident.status === "open" && admin ? (
                        <div className="flex justify-end">
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={closingId === incident.id}
                            onClick={() => void close(incident)}
                          >
                            {closingId === incident.id ? (
                              <Loader2 aria-hidden className="animate-spin" />
                            ) : null}
                            {t("incidents.closeIncident")}
                          </Button>
                        </div>
                      ) : null}
                    </div>
                  ))}
                </div>
              </>
            )}
          </div>
        </TabsContent>

        <TabsContent value="heal" className="mt-4 space-y-3">
          <p className="text-sm text-muted-foreground">
            {t("incidents.healIntro")}
            {!admin ? ` ${t("incidents.healReadOnly")}` : ""}
          </p>
          {instances.error ? <ErrorAlert error={instances.error} onRetry={instances.reload} /> : null}
          <div className="rounded-md border">
            {instances.loading && !instances.data ? (
              <div className="space-y-2 p-4">
                {Array.from({ length: 4 }).map((_, index) => (
                  <Skeleton key={index} className="h-12 w-full" />
                ))}
              </div>
            ) : (instances.data ?? []).length === 0 ? (
              <EmptyState
                icon={HeartPulse}
                title={t("incidents.noInstances")}
                description={t("incidents.noInstancesDescription")}
              />
            ) : (
              <ul className="divide-y">
                {(instances.data ?? []).map((row) => {
                  const policy = policies[row.id];
                  return (
                    <li key={row.id} className="flex flex-wrap items-center gap-3 p-4">
                      <div className="min-w-0 flex-1">
                        <p className="text-sm font-medium">{row.name}</p>
                        <p className="font-mono text-xs text-muted-foreground">{row.id}</p>
                      </div>
                      <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                        {policy ? (
                          <>
                            <OpsBadge
                              status={policy.heal === "off" ? "closed" : policy.heal}
                              label={
                                hasTranslation(`heal.${policy.heal}`)
                                  ? t(`heal.${policy.heal}`)
                                  : policy.heal
                              }
                            />
                            <span className="font-mono">
                              {t("incidents.policySummary", {
                                after: policy.unhealthy_after,
                                max: policy.max_restarts,
                                backoff: policy.heal_backoff_s,
                              })}
                            </span>
                          </>
                        ) : (
                          <span>{t("incidents.policyNotLoaded")}</span>
                        )}
                      </div>
                      {admin ? (
                        <Button variant="outline" size="sm" onClick={() => setHealTarget(row.id)}>
                          {t("incidents.editPolicy")}
                        </Button>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        </TabsContent>
      </Tabs>

      <HealDialog
        instanceId={healTarget ?? ""}
        current={healTarget ? (policies[healTarget] ?? null) : null}
        open={healTarget !== null}
        onOpenChange={(open) => {
          if (!open) setHealTarget(null);
        }}
        onSaved={(policy) => setPolicies((previous) => ({ ...previous, [policy.id]: policy }))}
      />
    </>
  );
}
