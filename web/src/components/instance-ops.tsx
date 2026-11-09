import { useEffect, useRef, useState, type FormEvent } from "react";
import {
  ArrowRightLeft,
  Check,
  Camera,
  Loader2,
  Play,
  Plus,
  RefreshCw,
  ShieldCheck,
  Terminal,
  Trash2,
  TriangleAlert,
  X,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type {
  AclRole,
  EgressMode,
  EgressPolicyView,
  ExecResult,
  Incident,
  Snapshot,
  SnapshotDiff,
} from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
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
import { Textarea } from "@/components/ui/textarea";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { OpsBadge } from "@/components/ops-badge";
import {
  formatBytes,
  formatDateTime,
  formatDurationMs,
  formatRelative,
} from "@/lib/format";
import { useIdentity } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

const ROLE_OPTIONS: AclRole[] = ["owner", "operator", "viewer"];

export function InstanceIncidents({ id }: { id: string }) {
  const identity = useIdentity();
  const admin = identity?.role === "admin";
  const [nonce, setNonce] = useState(0);
  const [closing, setClosing] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const incidents = useResource(() => api.instanceIncidents(id, 100), [id, nonce]);
  const rows = incidents.data ?? [];

  async function close(incident: Incident) {
    setClosing(incident.id);
    setError(null);
    try {
      await api.closeIncident(incident.id);
      toast.success("Incident closed; the heal budget was reset.");
      setNonce((value) => value + 1);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setClosing(null);
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">
          {rows.length === 0
            ? "No incident recorded for this instance."
            : `${rows.filter((incident) => incident.status === "open").length} open · ${rows.length} recorded`}
        </p>
        <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
          <RefreshCw aria-hidden className={incidents.refreshing ? "animate-spin" : undefined} />
          Refresh
        </Button>
      </div>
      {error ? <ErrorAlert error={error} /> : null}
      {incidents.error ? <ErrorAlert error={incidents.error} onRetry={incidents.reload} /> : null}
      <div className="rounded-md border">
        {incidents.loading && !incidents.data ? (
          <div className="space-y-2 p-4">
            {Array.from({ length: 2 }).map((_, index) => (
              <Skeleton key={index} className="h-16 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={TriangleAlert}
            title="No incidents"
            description="A clean stop never opens an incident. An unhealthy container opens one after the configured streak."
          />
        ) : (
          <ul className="divide-y">
            {rows.map((incident) => (
              <li key={incident.id} className="space-y-2 p-4">
                <div className="flex flex-wrap items-start justify-between gap-2">
                  <div className="min-w-0">
                    <p className="font-mono text-xs">{incident.cause.replace(/_/g, " ")}</p>
                    <p className="text-[11px] text-muted-foreground">
                      opened {formatDateTime(incident.opened_at)} · last seen{" "}
                      {formatRelative(incident.last_seen_at)}
                    </p>
                  </div>
                  <OpsBadge status={incident.status} />
                </div>
                <p className="text-sm">{incident.detail}</p>
                <p className="text-xs text-muted-foreground">
                  {incident.actions.length} watchdog action
                  {incident.actions.length === 1 ? "" : "s"} · restart count {incident.restart_count}
                </p>
                {incident.status === "open" && admin ? (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={closing === incident.id}
                    onClick={() => void close(incident)}
                  >
                    {closing === incident.id ? <Loader2 aria-hidden className="animate-spin" /> : null}
                    Close incident
                  </Button>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function CreateSnapshotDialog({
  open,
  onOpenChange,
  instanceId,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  instanceId: string;
  onCreated: () => void;
}) {
  const [label, setLabel] = useState("");
  const [ttl, setTtl] = useState("604800");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setLabel("");
    setTtl("604800");
    setError(null);
  }, [open]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setPending(true);
    setError(null);
    try {
      await api.createSnapshot(instanceId, {
        ...(label.trim() ? { label: label.trim() } : {}),
        ttl_s: Number(ttl),
      });
      toast.success("Snapshot created.");
      onOpenChange(false);
      onCreated();
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
          <DialogTitle>Take a snapshot</DialogTitle>
          <DialogDescription>
            Captures <span className="font-mono">data/</span>, the compose file, rendered config and
            environment rows. Secrets stay encrypted.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}
          <div className="space-y-2">
            <Label htmlFor="snapshot-label">Label</Label>
            <Input
              id="snapshot-label"
              value={label}
              placeholder="before upgrade"
              maxLength={128}
              onChange={(event) => setLabel(event.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="snapshot-ttl">Keep for</Label>
            <Select value={ttl} onValueChange={setTtl}>
              <SelectTrigger id="snapshot-ttl" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="86400">1 day</SelectItem>
                <SelectItem value="604800">7 days</SelectItem>
                <SelectItem value="2592000">30 days</SelectItem>
                <SelectItem value="7776000">90 days</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Camera aria-hidden />}
              Take snapshot
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function CompareDialog({
  open,
  onOpenChange,
  instanceId,
  snapshots,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  instanceId: string;
  snapshots: Snapshot[];
}) {
  const [a, setA] = useState("");
  const [b, setB] = useState("");
  const [diff, setDiff] = useState<SnapshotDiff | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setA(snapshots[0]?.id ?? "");
    setB(snapshots[1]?.id ?? "");
    setDiff(null);
    setError(null);
  }, [open, snapshots]);

  async function compare() {
    if (!a || !b) {
      setError(toApiError(new Error("Pick two snapshots.")));
      return;
    }
    if (a === b) {
      setError(toApiError(new Error("Pick two different snapshots.")));
      return;
    }
    setPending(true);
    setError(null);
    try {
      setDiff(await api.compareSnapshots(instanceId, a, b));
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  const label = (id: string) => {
    const snapshot = snapshots.find((candidate) => candidate.id === id);
    return snapshot ? `${snapshot.label || "snapshot"} · ${formatDateTime(snapshot.created_at)}` : id;
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Compare snapshots</DialogTitle>
          <DialogDescription>
            File-level comparison, no extraction. A is the left side, B the right side.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="compare-a">Snapshot A</Label>
              <Select value={a} onValueChange={setA}>
                <SelectTrigger id="compare-a" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {snapshots.map((snapshot) => (
                    <SelectItem key={snapshot.id} value={snapshot.id}>
                      {snapshot.label || "snapshot"} · {formatDateTime(snapshot.created_at)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-2">
              <Label htmlFor="compare-b">Snapshot B</Label>
              <Select value={b} onValueChange={setB}>
                <SelectTrigger id="compare-b" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {snapshots.map((snapshot) => (
                    <SelectItem key={snapshot.id} value={snapshot.id}>
                      {snapshot.label || "snapshot"} · {formatDateTime(snapshot.created_at)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>
          <Button type="button" size="sm" disabled={pending} onClick={() => void compare()}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : <ArrowRightLeft aria-hidden />}
            Compare
          </Button>

          {diff ? (
            <div className="space-y-3">
              <p className="text-sm">
                <span className="font-mono">{label(diff.a)}</span> →{" "}
                <span className="font-mono">{label(diff.b)}</span>
              </p>
              <div className="grid grid-cols-3 gap-2 text-center">
                <div className="rounded-md border p-2">
                  <p className="font-mono text-lg">{diff.added.length}</p>
                  <p className="text-xs text-muted-foreground">added</p>
                </div>
                <div className="rounded-md border p-2">
                  <p className="font-mono text-lg">{diff.removed.length}</p>
                  <p className="text-xs text-muted-foreground">removed</p>
                </div>
                <div className="rounded-md border p-2">
                  <p className="font-mono text-lg">{diff.changed.length}</p>
                  <p className="text-xs text-muted-foreground">changed</p>
                </div>
              </div>
              {[
                { title: "Added", paths: diff.added.map((file) => file.path) },
                { title: "Removed", paths: diff.removed.map((file) => file.path) },
                { title: "Changed", paths: diff.changed.map((file) => file.path) },
              ].map((group) =>
                group.paths.length > 0 ? (
                  <div key={group.title} className="space-y-1">
                    <p className="text-sm font-medium">{group.title}</p>
                    <ul className="rounded-md border">
                      {group.paths.map((path) => (
                        <li key={path} className="border-b px-2 py-1 font-mono text-[11px] last:border-b-0">
                          {path}
                        </li>
                      ))}
                    </ul>
                  </div>
                ) : null,
              )}
              <p className="text-xs text-muted-foreground">{diff.unchanged} unchanged files.</p>
            </div>
          ) : null}
        </div>
      </DialogContent>
    </Dialog>
  );
}

export function InstanceSnapshots({ id }: { id: string }) {
  const identity = useIdentity();
  const admin = identity?.role === "admin";
  const [nonce, setNonce] = useState(0);
  const [createOpen, setCreateOpen] = useState(false);
  const [compareOpen, setCompareOpen] = useState(false);
  const [restoreTarget, setRestoreTarget] = useState<Snapshot | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<Snapshot | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [approach, setApproach] = useState<string | null>(null);
  const snapshots = useResource(() => api.listSnapshots(id), [id, nonce]);
  const rows = snapshots.data ?? [];

  async function restore() {
    if (!restoreTarget) return;
    setPending(true);
    setError(null);
    try {
      const report = await api.restoreSnapshot(restoreTarget.id);
      toast.success(
        `Restored ${report.files} files. Previous data kept at ${report.previous_data_dir ?? "the .pre-restore directory"}.`,
      );
      setRestoreTarget(null);
      setApproach(report.instance_id);
      setNonce((value) => value + 1);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  async function remove() {
    if (!deleteTarget) return;
    setPending(true);
    setError(null);
    try {
      await api.deleteSnapshot(deleteTarget.id);
      toast.success("Snapshot deleted.");
      setDeleteTarget(null);
      setNonce((value) => value + 1);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">
          {rows.length === 0
            ? "No restore point yet."
            : `${rows.length} restore point${rows.length === 1 ? "" : "s"} · newest first`}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={rows.length < 2}
            onClick={() => setCompareOpen(true)}
          >
            <ArrowRightLeft aria-hidden />
            Compare
          </Button>
          <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
            <RefreshCw aria-hidden className={snapshots.refreshing ? "animate-spin" : undefined} />
            Refresh
          </Button>
          {admin ? (
            <Button size="sm" onClick={() => setCreateOpen(true)}>
              <Camera aria-hidden />
              Take snapshot
            </Button>
          ) : null}
        </div>
      </div>

      {approach ? (
        <p className="text-xs text-muted-foreground" role="status">
          Restore finished: {approach} was stopped, data moved aside, restored, and started again.
        </p>
      ) : null}
      {error ? <ErrorAlert error={error} /> : null}
      {snapshots.error ? <ErrorAlert error={snapshots.error} onRetry={snapshots.reload} /> : null}

      <div className="rounded-md border">
        {snapshots.loading && !snapshots.data ? (
          <div className="space-y-2 p-4">
            {Array.from({ length: 3 }).map((_, index) => (
              <Skeleton key={index} className="h-14 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={Camera}
            title="No snapshots"
            description="An auto snapshot is taken before upgrades, environment writes and git reverts. Take one by hand before a risky change."
            action={
              admin ? (
                <Button size="sm" onClick={() => setCreateOpen(true)}>
                  <Camera aria-hidden />
                  Take snapshot
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
                    <TableHead>Label</TableHead>
                    <TableHead className="w-[170px]">Created</TableHead>
                    <TableHead className="w-[100px]">Size</TableHead>
                    <TableHead className="w-[170px]">Expires</TableHead>
                    <TableHead className="w-[110px]">Status</TableHead>
                    <TableHead className="w-[130px]">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((snapshot) => (
                    <TableRow key={snapshot.id}>
                      <TableCell>
                        <span className="block text-sm">{snapshot.label || "untitled"}</span>
                        <span className="block font-mono text-[11px] text-muted-foreground">
                          {snapshot.id}
                        </span>
                      </TableCell>
                      <TableCell className="font-mono text-xs">
                        {formatDateTime(snapshot.created_at)}
                      </TableCell>
                      <TableCell className="font-mono text-xs">{formatBytes(snapshot.size_bytes)}</TableCell>
                      <TableCell className="font-mono text-xs">{formatDateTime(snapshot.expires_at)}</TableCell>
                      <TableCell>
                        <OpsBadge status={snapshot.status} />
                      </TableCell>
                      <TableCell>
                        {admin ? (
                          <div className="flex justify-end gap-1">
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label={`Restore ${snapshot.label || snapshot.id}`}
                              title="Restore this snapshot"
                              onClick={() => {
                                setError(null);
                                setRestoreTarget(snapshot);
                              }}
                            >
                              <Play aria-hidden />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label={`Delete ${snapshot.label || snapshot.id}`}
                              title="Delete this snapshot"
                              onClick={() => {
                                setError(null);
                                setDeleteTarget(snapshot);
                              }}
                            >
                              <Trash2 aria-hidden />
                            </Button>
                          </div>
                        ) : null}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
            <ul className="divide-y md:hidden">
              {rows.map((snapshot) => (
                <li key={snapshot.id} className="space-y-2 p-4">
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0">
                      <p className="text-sm">{snapshot.label || "untitled"}</p>
                      <p className="font-mono text-[11px] text-muted-foreground">
                        {formatDateTime(snapshot.created_at)}
                      </p>
                    </div>
                    <OpsBadge status={snapshot.status} />
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {formatBytes(snapshot.size_bytes)} · expires {formatDateTime(snapshot.expires_at)}
                  </p>
                  {admin ? (
                    <div className="flex justify-end gap-1">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => {
                          setError(null);
                          setRestoreTarget(snapshot);
                        }}
                      >
                        Restore
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => {
                          setError(null);
                          setDeleteTarget(snapshot);
                        }}
                      >
                        Delete
                      </Button>
                    </div>
                  ) : null}
                </li>
              ))}
            </ul>
          </>
        )}
      </div>

      <CreateSnapshotDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        instanceId={id}
        onCreated={() => setNonce((value) => value + 1)}
      />
      <CompareDialog
        open={compareOpen}
        onOpenChange={setCompareOpen}
        instanceId={id}
        snapshots={rows}
      />
      <ConfirmDialog
        open={restoreTarget !== null}
        onOpenChange={(open) => {
          if (!open) setRestoreTarget(null);
        }}
        title="Restore snapshot"
        description={
          restoreTarget ? (
            <>
              Restore <span className="font-mono">{restoreTarget.label || restoreTarget.id}</span>?
              The instance is stopped, its current data is moved to a kept{" "}
              <span className="font-mono">.pre-restore-*</span> directory, then restored and started.
              Environment and compose are restored too.
            </>
          ) : undefined
        }
        confirmLabel="Restore snapshot"
        destructive
        pending={pending}
        error={error}
        onConfirm={() => void restore()}
      />
      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        title="Delete snapshot"
        description={
          deleteTarget
            ? `Delete ${deleteTarget.label || deleteTarget.id} and its archive? This cannot be undone.`
            : undefined
        }
        confirmLabel="Delete snapshot"
        destructive
        pending={pending}
        error={error}
        onConfirm={() => void remove()}
      />
    </div>
  );
}

function tokenizeCommand(input: string): string[] {
  const args: string[] = [];
  let current = "";
  let quote: string | null = null;
  let escaped = false;
  for (const char of input.trim()) {
    if (escaped) {
      current += char;
      escaped = false;
    } else if (char === "\\") {
      escaped = true;
    } else if (quote) {
      if (char === quote) quote = null;
      else current += char;
    } else if (char === '"' || char === "'") {
      quote = char;
    } else if (/\s/.test(char)) {
      if (current) {
        args.push(current);
        current = "";
      }
    } else {
      current += char;
    }
  }
  if (current) args.push(current);
  return args;
}

const QUICK_COMMANDS = ["pwd", "ls /opt/data", "df -h", "ps", "env", "uname -a"];

export function InstanceTerminal({ id }: { id: string }) {
  const [command, setCommand] = useState("ls /opt/data");
  const [timeout, setTimeoutValue] = useState("30");
  const [result, setResult] = useState<ExecResult | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  async function run(event?: FormEvent<HTMLFormElement>) {
    event?.preventDefault();
    const argv = tokenizeCommand(command);
    if (argv.length === 0) {
      setError(toApiError(new Error("Enter a command.")));
      return;
    }
    setPending(true);
    setError(null);
    setResult(null);
    try {
      setResult(
        await api.exec(id, {
          argv,
          timeout_s: Math.min(Math.max(Number(timeout) || 30, 1), 300),
        }),
      );
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="space-y-3">
      <Alert>
        <Terminal aria-hidden />
        <AlertTitle>One-shot command</AlertTitle>
        <AlertDescription>
          Runs an allowlisted binary with argv, no shell, no stdin and no PTY. Timeout is 1–300 s;
          each stream is capped at 256 KiB. Every attempt is audited.
        </AlertDescription>
      </Alert>

      <form onSubmit={run} className="space-y-3">
        <div className="flex flex-col gap-2 sm:flex-row sm:items-end">
          <div className="min-w-0 flex-1 space-y-2">
            <Label htmlFor="exec-command">Command</Label>
            <Input
              ref={inputRef}
              id="exec-command"
              value={command}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder="ls /opt/data"
              onChange={(event) => setCommand(event.target.value)}
            />
          </div>
          <div className="w-full space-y-2 sm:w-28">
            <Label htmlFor="exec-timeout">Timeout (s)</Label>
            <Input
              id="exec-timeout"
              type="number"
              min={1}
              max={300}
              value={timeout}
              className="font-mono"
              onChange={(event) => setTimeoutValue(event.target.value)}
            />
          </div>
          <Button type="submit" disabled={pending}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Play aria-hidden />}
            Run
          </Button>
        </div>
        <div className="flex flex-wrap gap-1.5">
          {QUICK_COMMANDS.map((quick) => (
            <Button
              key={quick}
              type="button"
              variant="outline"
              size="sm"
              className="font-mono text-xs"
              onClick={() => {
                setCommand(quick);
                inputRef.current?.focus();
              }}
            >
              {quick}
            </Button>
          ))}
        </div>
      </form>

      {error ? <ErrorAlert error={error} /> : null}

      {result ? (
        <div className="space-y-2">
          <div className="flex flex-wrap items-center gap-2">
            <OpsBadge
              status={result.timed_out ? "timeout" : result.exit_code === 0 ? "ok" : "error"}
              label={
                result.timed_out
                  ? "timed out"
                  : result.exit_code === null
                    ? "no exit code"
                    : `exit ${result.exit_code}`
              }
            />
            <span className="font-mono text-xs text-muted-foreground">
              {formatDurationMs(result.duration_ms)}
            </span>
            {result.truncated ? (
              <Badge variant="outline" className="font-normal">
                truncated
              </Badge>
            ) : null}
          </div>
          <div className="rounded-md border bg-muted/20">
            <p className="border-b px-3 py-1.5 text-xs font-medium text-muted-foreground">stdout</p>
            <pre className="max-h-96 overflow-auto p-3 font-mono text-xs whitespace-pre-wrap">
              {result.stdout || "(empty)"}
            </pre>
          </div>
          {result.stderr ? (
            <div className="rounded-md border border-destructive/30">
              <p className="border-b px-3 py-1.5 text-xs font-medium text-destructive">stderr</p>
              <pre className="max-h-64 overflow-auto p-3 font-mono text-xs whitespace-pre-wrap text-destructive">
                {result.stderr}
              </pre>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

export function InstanceAccess({ id }: { id: string }) {
  const identity = useIdentity();
  const admin = identity?.role === "admin";
  const [nonce, setNonce] = useState(0);
  const [draft, setDraft] = useState<{ subject: string; role: AclRole }[] | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const acl = useResource(() => api.getInstanceAcl(id), [id, nonce]);

  useEffect(() => {
    if (acl.data) setDraft(acl.data.acl.map((item) => ({ subject: item.subject, role: item.role })));
  }, [acl.data]);

  const rows = draft ?? [];
  const dirty = acl.data
    ? JSON.stringify(acl.data.acl.map(({ subject, role }) => ({ subject, role }))) !==
      JSON.stringify(rows)
    : false;

  async function save() {
    const seen = new Set<string>();
    for (const row of rows) {
      const subject = row.subject.trim();
      if (!subject) {
        setError(toApiError(new Error("Every grant needs a subject (e-mail, group or token name).")));
        return;
      }
      if (seen.has(subject)) {
        setError(toApiError(new Error(`Subject ${subject} is listed twice.`)));
        return;
      }
      seen.add(subject);
    }
    setPending(true);
    setError(null);
    try {
      const saved = await api.putInstanceAcl(
        id,
        rows.map((row) => ({ subject: row.subject.trim(), role: row.role })),
      );
      setDraft(saved.acl.map((item) => ({ subject: item.subject, role: item.role })));
      toast.success("Instance grants saved.");
      setNonce((value) => value + 1);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">
          Per-instance grants. The level is read from SQLite on every request, so a save applies to
          the next call.
        </p>
        <div className="flex items-center gap-2">
          {admin ? (
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                setDraft([...rows, { subject: "", role: "viewer" }])
              }
            >
              <Plus aria-hidden />
              Add grant
            </Button>
          ) : null}
          <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
            <RefreshCw aria-hidden className={acl.refreshing ? "animate-spin" : undefined} />
            Refresh
          </Button>
        </div>
      </div>

      {!admin ? (
        <Alert>
          <ShieldCheck aria-hidden />
          <AlertTitle>Read-only for this account</AlertTitle>
          <AlertDescription>
            Only an admin or an owner of the instance can change its ACL.
          </AlertDescription>
        </Alert>
      ) : null}
      {error ? <ErrorAlert error={error} /> : null}
      {acl.error ? <ErrorAlert error={acl.error} onRetry={acl.reload} /> : null}

      <div className="rounded-md border">
        {acl.loading && !acl.data ? (
          <div className="space-y-2 p-4">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={ShieldCheck}
            title="No explicit grant"
            description="Admins always reach every instance. A grant lets another subject read, operate or own this one."
            action={
              admin ? (
                <Button size="sm" onClick={() => setDraft([{ subject: "", role: "viewer" }])}>
                  <Plus aria-hidden />
                  Add grant
                </Button>
              ) : undefined
            }
          />
        ) : (
          <ul className="divide-y">
            {rows.map((row, index) => (
              <li key={index} className="grid gap-2 p-3 sm:grid-cols-[minmax(0,1fr)_150px_auto] sm:items-center">
                {admin ? (
                  <Input
                    value={row.subject}
                    aria-label={`Subject ${index + 1}`}
                    placeholder="cle@example.com"
                    className="font-mono text-xs"
                    onChange={(event) =>
                      setDraft(
                        rows.map((candidate, position) =>
                          position === index ? { ...candidate, subject: event.target.value } : candidate,
                        ),
                      )
                    }
                  />
                ) : (
                  <span className="font-mono text-xs break-all">{row.subject}</span>
                )}
                {admin ? (
                  <Select
                    value={row.role}
                    onValueChange={(value) =>
                      setDraft(
                        rows.map((candidate, position) =>
                          position === index
                            ? { ...candidate, role: value as AclRole }
                            : candidate,
                        ),
                      )
                    }
                  >
                    <SelectTrigger aria-label={`Role for ${row.subject || `grant ${index + 1}`}`}>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {ROLE_OPTIONS.map((role) => (
                        <SelectItem key={role} value={role}>
                          {role}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                ) : (
                  <Badge variant="outline" className="w-fit font-mono text-xs font-normal">
                    {row.role}
                  </Badge>
                )}
                {admin ? (
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={`Remove ${row.subject || `grant ${index + 1}`}`}
                    title="Remove this grant"
                    onClick={() => setDraft(rows.filter((_, position) => position !== index))}
                  >
                    <X aria-hidden />
                  </Button>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </div>

      {admin ? (
        <div className="flex flex-wrap items-center gap-2 border-t pt-3">
          <Button type="button" onClick={() => void save()} disabled={!dirty || pending}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Check aria-hidden />}
            Save grants
          </Button>
          <Button
            type="button"
            variant="outline"
            disabled={!dirty || pending}
            onClick={() => {
              setDraft(
                acl.data?.acl.map((item) => ({ subject: item.subject, role: item.role })) ?? [],
              );
              setError(null);
            }}
          >
            Discard
          </Button>
          <p className="text-xs text-muted-foreground sm:ml-auto">
            A save replaces every row. Duplicates and empty subjects are refused.
          </p>
        </div>
      ) : null}
    </div>
  );
}

function egressList(value: string): string[] {
  return value
    .split(/[\n,]/)
    .map((item) => item.trim())
    .filter(Boolean);
}

export function InstanceEgress({ id }: { id: string }) {
  const identity = useIdentity();
  const admin = identity?.role === "admin";
  const [nonce, setNonce] = useState(0);
  const [draft, setDraft] = useState<{
    mode: EgressMode;
    fqdns: string;
    cidrs: string;
    ports: string;
  } | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [host, setHost] = useState("api.github.com");
  const [port, setPort] = useState("443");
  const [testResult, setTestResult] = useState<Awaited<ReturnType<typeof api.testEgress>> | null>(null);
  const [testing, setTesting] = useState(false);
  const policy = useResource(() => api.getEgress(id), [id, nonce]);
  const data: EgressPolicyView | null = policy.data;

  useEffect(() => {
    if (!data) return;
    setDraft({
      mode: data.mode,
      fqdns: data.fqdns.join("\n"),
      cidrs: data.cidrs.join("\n"),
      ports: data.ports.join(", "),
    });
  }, [data]);

  const dirty =
    !!data &&
    !!draft &&
    (draft.mode !== data.mode ||
      JSON.stringify(egressList(draft.fqdns)) !== JSON.stringify(data.fqdns) ||
      JSON.stringify(egressList(draft.cidrs)) !== JSON.stringify(data.cidrs) ||
      JSON.stringify(
        egressList(draft.ports.replace(/\s/g, "")).map((value) => Number(value)),
      ) !== JSON.stringify(data.ports));

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!draft) return;
    const ports = egressList(draft.ports.replace(/\s/g, "")).map((value) => Number(value));
    if (ports.some((value) => !Number.isInteger(value) || value < 1 || value > 65_535)) {
      setError(toApiError(new Error("Ports must be integers between 1 and 65535.")));
      return;
    }
    setPending(true);
    setError(null);
    try {
      const saved = await api.putEgress(id, {
        mode: draft.mode,
        fqdns: egressList(draft.fqdns),
        cidrs: egressList(draft.cidrs),
        ports,
      });
      toast.success("Egress policy saved. The container was restarted.");
      setDraft({
        mode: saved.mode,
        fqdns: saved.fqdns.join("\n"),
        cidrs: saved.cidrs.join("\n"),
        ports: saved.ports.join(", "),
      });
      setNonce((value) => value + 1);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  async function test(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setTesting(true);
    setTestResult(null);
    setError(null);
    try {
      setTestResult(await api.testEgress(id, host.trim(), Number(port)));
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setTesting(false);
    }
  }

  return (
    <div className="space-y-4">
      {policy.error ? <ErrorAlert error={policy.error} onRetry={policy.reload} /> : null}
      {error ? <ErrorAlert error={error} /> : null}

      {policy.loading && !data ? (
        <Skeleton className="h-64 w-full" />
      ) : data ? (
        <>
          {data.warnings.length > 0 ? (
            <Alert className="border-amber-600/40">
              <TriangleAlert aria-hidden className="text-amber-600 dark:text-amber-400" />
              <AlertTitle>Enforcement notes</AlertTitle>
              <AlertDescription>
                <ul className="list-disc space-y-1 pl-4">
                  {data.warnings.map((warning) => (
                    <li key={warning}>{warning}</li>
                  ))}
                </ul>
              </AlertDescription>
            </Alert>
          ) : null}

          <div className="flex flex-wrap items-center gap-2">
            <Badge variant="outline" className="font-mono text-xs font-normal">
              source {data.source}
            </Badge>
            <Badge variant="outline" className="font-mono text-xs font-normal">
              mode {data.mode}
            </Badge>
            {data.proxy ? (
              <span className="font-mono text-xs text-muted-foreground">{data.proxy}</span>
            ) : null}
            {data.updated_at ? (
              <span className="text-xs text-muted-foreground">
                updated {formatDateTime(data.updated_at)}
              </span>
            ) : null}
          </div>

          <form onSubmit={save} className="space-y-4 rounded-md border p-4">
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="egress-mode">Mode</Label>
                <Select
                  value={draft?.mode ?? data.mode}
                  onValueChange={(value) =>
                    setDraft((previous) =>
                      previous ? { ...previous, mode: value as EgressMode } : previous,
                    )
                  }
                  disabled={!admin}
                >
                  <SelectTrigger id="egress-mode" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="deny">deny — allowlist</SelectItem>
                    <SelectItem value="allow">allow — blocklist</SelectItem>
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">
                  Deny with an empty policy blocks everything; allow with an empty policy permits
                  everything.
                </p>
              </div>
              <div className="space-y-2">
                <Label htmlFor="egress-ports">Ports</Label>
                <Input
                  id="egress-ports"
                  value={draft?.ports ?? data.ports.join(", ")}
                  placeholder="443, 80 (empty = every port)"
                  className="font-mono"
                  disabled={!admin}
                  onChange={(event) =>
                    setDraft((previous) =>
                      previous ? { ...previous, ports: event.target.value } : previous,
                    )
                  }
                />
              </div>
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="egress-fqdns">Domains (one per line)</Label>
                <Textarea
                  id="egress-fqdns"
                  rows={5}
                  value={draft?.fqdns ?? data.fqdns.join("\n")}
                  placeholder={"api.telegram.org\n*.opencode.ai"}
                  className="font-mono text-xs"
                  disabled={!admin}
                  onChange={(event) =>
                    setDraft((previous) =>
                      previous ? { ...previous, fqdns: event.target.value } : previous,
                    )
                  }
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="egress-cidrs">CIDRs (one per line)</Label>
                <Textarea
                  id="egress-cidrs"
                  rows={5}
                  value={draft?.cidrs ?? data.cidrs.join("\n")}
                  placeholder="10.0.0.0/8"
                  className="font-mono text-xs"
                  disabled={!admin}
                  onChange={(event) =>
                    setDraft((previous) =>
                      previous ? { ...previous, cidrs: event.target.value } : previous,
                    )
                  }
                />
              </div>
            </div>
            {admin ? (
              <div className="flex flex-wrap items-center gap-2">
                <Button type="submit" disabled={!dirty || pending}>
                  {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
                  Save policy
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  disabled={!dirty || pending}
                  onClick={() =>
                    setDraft({
                      mode: data.mode,
                      fqdns: data.fqdns.join("\n"),
                      cidrs: data.cidrs.join("\n"),
                      ports: data.ports.join(", "),
                    })
                  }
                >
                  Discard
                </Button>
                <p className="text-xs text-muted-foreground sm:ml-auto">
                  Saving rewrites compose and restarts the instance.
                </p>
              </div>
            ) : null}
          </form>

          <form onSubmit={test} className="space-y-3 rounded-md border p-4">
            <div>
              <p className="text-sm font-medium">Test a destination</p>
              <p className="text-xs text-muted-foreground">
                The policy decision and its enforcement are reported separately; a reachable proxy
                does not prove the ACL matches.
              </p>
            </div>
            <div className="flex flex-col gap-2 sm:flex-row sm:items-end">
              <div className="min-w-0 flex-1 space-y-2">
                <Label htmlFor="egress-test-host">Host</Label>
                <Input
                  id="egress-test-host"
                  value={host}
                  className="font-mono"
                  onChange={(event) => setHost(event.target.value)}
                />
              </div>
              <div className="w-full space-y-2 sm:w-28">
                <Label htmlFor="egress-test-port">Port</Label>
                <Input
                  id="egress-test-port"
                  type="number"
                  min={1}
                  max={65_535}
                  value={port}
                  className="font-mono"
                  onChange={(event) => setPort(event.target.value)}
                />
              </div>
              <Button type="submit" variant="outline" disabled={testing}>
                {testing ? <Loader2 aria-hidden className="animate-spin" /> : null}
                Test
              </Button>
            </div>
            {testResult ? (
              <div className="space-y-1 rounded-md border p-3">
                <div className="flex flex-wrap items-center gap-2">
                  <OpsBadge status={testResult.allowed ? "ok" : "error"} label={testResult.allowed ? "allowed" : "denied"} />
                  <OpsBadge
                    status={testResult.enforced ? "ok" : "closed"}
                    label={testResult.enforced ? "enforced" : "not enforced"}
                  />
                  <span className="font-mono text-xs text-muted-foreground">
                    {testResult.host}:{testResult.port} · {testResult.mode}
                  </span>
                </div>
                <p className="text-sm">{testResult.reason}</p>
                {testResult.enforcement ? (
                  <p className="text-xs text-muted-foreground">{testResult.enforcement}</p>
                ) : null}
              </div>
            ) : null}
          </form>
        </>
      ) : null}
    </div>
  );
}
