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
import { formatDateTime, formatRelative, plural } from "@/lib/format";
import { useIdentity } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

function CauseLabel({ cause }: { cause: string }) {
  return (
    <span className="font-mono text-xs">{cause.replace(/_/g, " ")}</span>
  );
}

function IncidentActions({ incident }: { incident: Incident }) {
  if (incident.actions.length === 0) {
    return <span className="text-xs text-muted-foreground">none</span>;
  }
  return (
    <details>
      <summary className="cursor-pointer text-xs text-muted-foreground hover:text-foreground">
        {incident.actions.length} action{incident.actions.length === 1 ? "" : "s"} · restart{" "}
        {incident.restart_count}
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
        setError(toApiError(new Error("unhealthy_after must be between 1 and 100.")));
        return;
      }
      input.unhealthy_after = value;
    }
    if (maxRestarts.trim()) {
      const value = Number(maxRestarts);
      if (!Number.isInteger(value) || value < 0 || value > 100) {
        setError(toApiError(new Error("max_restarts must be between 0 and 100.")));
        return;
      }
      input.max_restarts = value;
    }
    if (backoff.trim()) {
      const value = Number(backoff);
      if (!Number.isInteger(value) || value < 1 || value > 3_600) {
        setError(toApiError(new Error("heal_backoff_s must be between 1 and 3600.")));
        return;
      }
      input.heal_backoff_s = value;
    }
    if (Object.keys(input).length === 0) {
      setError(toApiError(new Error("Change at least one field, or cancel.")));
      return;
    }
    setPending(true);
    setError(null);
    try {
      const policy = await api.patchHeal(instanceId, input);
      toast.success(`Heal policy saved for ${instanceId}.`);
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
          <DialogTitle>Heal policy · {instanceId}</DialogTitle>
          <DialogDescription>
            The watchdog reads the policy from the instance row. The API reports it after a write, so
            empty fields mean “keep the stored value”.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}
          {value ? (
            <p className="rounded-md border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
              Last saved: <span className="font-mono">{value.heal}</span>, every{" "}
              <span className="font-mono">{value.unhealthy_after}</span> unhealthy samples, up to{" "}
              <span className="font-mono">{value.max_restarts}</span> restarts, backoff{" "}
              <span className="font-mono">{value.heal_backoff_s}s</span>.
            </p>
          ) : (
            <Alert>
              <HeartPulse aria-hidden />
              <AlertTitle>Current policy not loaded</AlertTitle>
              <AlertDescription>
                Only changed fields are sent, so the stored values stay untouched until you save.
              </AlertDescription>
            </Alert>
          )}

          <div className="space-y-2">
            <Label htmlFor="heal-mode">Action when unhealthy</Label>
            <Select value={heal} onValueChange={setHeal}>
              <SelectTrigger id="heal-mode" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="keep">Keep current</SelectItem>
                <SelectItem value="off">Observe only</SelectItem>
                <SelectItem value="restart">Restart the container</SelectItem>
                <SelectItem value="recreate">Recreate the container</SelectItem>
              </SelectContent>
            </Select>
          </div>

          <div className="grid gap-4 sm:grid-cols-3">
            <div className="space-y-2">
              <Label htmlFor="heal-after">Unhealthy after</Label>
              <Input
                id="heal-after"
                type="number"
                min={1}
                max={100}
                value={unhealthyAfter}
                placeholder={value ? String(value.unhealthy_after) : "keep"}
                className="font-mono"
                onChange={(event) => setUnhealthyAfter(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="heal-max">Max restarts</Label>
              <Input
                id="heal-max"
                type="number"
                min={0}
                max={100}
                value={maxRestarts}
                placeholder={value ? String(value.max_restarts) : "keep"}
                className="font-mono"
                onChange={(event) => setMaxRestarts(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="heal-backoff">Backoff (s)</Label>
              <Input
                id="heal-backoff"
                type="number"
                min={1}
                max={3_600}
                value={backoff}
                placeholder={value ? String(value.heal_backoff_s) : "keep"}
                className="font-mono"
                onChange={(event) => setBackoff(event.target.value)}
              />
            </div>
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              Save policy
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function IncidentsPage() {
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
      toast.success(`Incident on ${incident.instance_id} closed.`);
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
        title="Incidents"
        description={
          incidents.data
            ? `${openCount} open in this view · ${rows.length} ${plural(rows.length, "incident")} · watchdog sampling every 30 s`
            : "Loading incidents."
        }
        actions={
          <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
            <RefreshCw aria-hidden className={incidents.refreshing ? "animate-spin" : undefined} />
            Refresh
          </Button>
        }
      />

      {actionError ? <ErrorAlert error={actionError} /> : null}

      <Tabs value={tab} onValueChange={setTab}>
        <div className="overflow-x-auto">
          <TabsList className="w-full justify-start sm:w-fit">
            <TabsTrigger value="incidents">Incidents</TabsTrigger>
            <TabsTrigger value="heal">Heal policies</TabsTrigger>
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
                placeholder="Search instance, cause, detail"
                aria-label="Search incidents"
                className="pl-8"
              />
            </div>
            <Select value={status} onValueChange={(value) => setStatus(value as "all" | IncidentStatus)}>
              <SelectTrigger className="w-full sm:w-40" aria-label="Filter by status">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All statuses</SelectItem>
                <SelectItem value="open">Open</SelectItem>
                <SelectItem value="closed">Closed</SelectItem>
              </SelectContent>
            </Select>
            <Select value={instance} onValueChange={setInstance}>
              <SelectTrigger className="w-full sm:w-48" aria-label="Filter by instance">
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
                title={rows.length === 0 ? "No incidents in this view" : "No incident matches the filters"}
                description={
                  status === "open"
                    ? "A clean stop never opens an incident. The watchdog closes one on the first healthy sample."
                    : "Change the status filter or clear the search."
                }
              />
            ) : (
              <>
                <div className="hidden lg:block">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead className="w-[160px]">Opened</TableHead>
                        <TableHead className="w-[120px]">Instance</TableHead>
                        <TableHead className="w-[120px]">Cause</TableHead>
                        <TableHead>Detail</TableHead>
                        <TableHead className="w-[150px]">Status</TableHead>
                        <TableHead className="w-[110px]">
                          <span className="sr-only">Actions</span>
                        </TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {filtered.map((incident) => (
                        <TableRow key={incident.id}>
                          <TableCell>
                            <span className="block font-mono text-xs">{formatDateTime(incident.opened_at)}</span>
                            <span className="block text-[11px] text-muted-foreground">
                              last seen {formatRelative(incident.last_seen_at)}
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
                                  {formatRelative(incident.closed_at)}
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
                                Close
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
                          restart {incident.restart_count}
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
                            Close incident
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
            Self-healing is opt-in per instance. The watchdog observes every 30 s; a heal policy
            restarts or recreates a container after consecutive unhealthy samples.
            {!admin ? " Your account can read the policies but not change them." : ""}
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
              <EmptyState icon={HeartPulse} title="No instances" description="Create an instance first." />
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
                            <OpsBadge status={policy.heal === "off" ? "closed" : policy.heal} />
                            <span className="font-mono">
                              after {policy.unhealthy_after} · max {policy.max_restarts} · backoff{" "}
                              {policy.heal_backoff_s}s
                            </span>
                          </>
                        ) : (
                          <span>Policy not loaded in this session</span>
                        )}
                      </div>
                      {admin ? (
                        <Button variant="outline" size="sm" onClick={() => setHealTarget(row.id)}>
                          Edit policy
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
