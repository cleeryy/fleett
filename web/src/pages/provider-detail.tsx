import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import {
  Boxes,
  CheckCircle2,
  Loader2,
  MoreHorizontal,
  Pencil,
  Plug,
  RefreshCw,
  RotateCw,
  Trash2,
  XCircle,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { ProviderRestart, ProviderSummary, ProviderTestResult } from "@/api/types";
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
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { CopyButton } from "@/components/copy-button";
import { DataList, Section } from "@/components/data-list";
import { DeleteProviderDialog } from "@/components/provider-delete-dialog";
import { ProviderFormDialog } from "@/components/provider-form-dialog";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { formatDateTime, formatRelative, plural } from "@/lib/format";
import { hrefFor, navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

function Mono({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={cn("font-mono text-xs break-all", className)}>{children}</span>;
}

function CopyValue({ value, label }: { value: string; label: string }) {
  return (
    <span className="flex min-w-0 items-center gap-1">
      <span className="min-w-0 flex-1 font-mono text-xs break-all">{value}</span>
      <CopyButton value={value} label={`Copy ${label}`} size="icon-xs" className="-my-1 shrink-0" />
    </span>
  );
}

function TestResultPanel({ result, baseUrl }: { result: ProviderTestResult; baseUrl: string }) {
  const probeUrl = `${baseUrl.replace(/\/+$/, "")}/models`;
  return (
    <div
      role="status"
      className={cn(
        "rounded-md border p-4",
        result.ok ? "bg-muted/20" : "border-destructive/40 bg-destructive/5",
      )}
    >
      <div className="flex flex-wrap items-center gap-2">
        {result.ok ? (
          <CheckCircle2 aria-hidden className="size-4 text-emerald-500" />
        ) : (
          <XCircle aria-hidden className="size-4 text-destructive" />
        )}
        <p className="text-sm font-medium">{result.ok ? "Connection OK" : "Connection failed"}</p>
        <span className="font-mono text-xs text-muted-foreground sm:ml-auto">
          {result.status === 0 ? "no response" : `HTTP ${result.status}`} · {result.latency_ms} ms
        </span>
      </div>
      {result.error ? (
        <p className="mt-2 font-mono text-xs break-words whitespace-pre-wrap text-destructive">
          {result.error}
        </p>
      ) : null}
      <p className="mt-1.5 font-mono text-xs break-all text-muted-foreground">GET {probeUrl}</p>
    </div>
  );
}

function RotateSecretDialog({
  provider,
  open,
  onOpenChange,
  onRotated,
}: {
  provider: ProviderSummary;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onRotated: () => void;
}) {
  const [secret, setSecret] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [report, setReport] = useState<ProviderRestart[] | null>(null);

  useEffect(() => {
    if (!open) return;
    setSecret("");
    setPending(false);
    setError(null);
    setReport(null);
  }, [open]);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!secret.trim()) return;
    setPending(true);
    setError(null);
    try {
      const result = await api.patchProvider(provider.id, { secret });
      setReport(result.restarted);
      toast.success(`Secret rotated for ${provider.name}.`);
      onRotated();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  const failed = report?.filter((entry) => entry.status !== "healthy").length ?? 0;

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh_-_2rem)] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            Rotate secret for <span className="font-mono">{provider.name}</span>
          </DialogTitle>
          <DialogDescription>
            {provider.instances_count === 0
              ? "No instance uses this provider: the new secret applies to the next instance that picks it."
              : `Re-encrypts the credential, then restarts ${provider.instances_count} ${plural(
                  provider.instances_count,
                  "instance",
                )} one by one. The report below says which came back.`}
          </DialogDescription>
        </DialogHeader>

        {report === null ? (
          <form onSubmit={onSubmit} noValidate className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="rotate-secret">New API key</Label>
              <Input
                id="rotate-secret"
                type="password"
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
                value={secret}
                autoFocus
                aria-describedby="rotate-secret-help"
                onChange={(event) => setSecret(event.target.value)}
              />
              <p id="rotate-secret-help" className="text-xs text-muted-foreground">
                Write-only: never returned by the API. The old secret is not kept, so a failed instance
                stays on the new credential until you fix it.
              </p>
            </div>

            {error ? <ErrorAlert error={error} /> : null}

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={pending}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={pending || !secret.trim()}>
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <RotateCw aria-hidden />}
                Rotate secret
              </Button>
            </DialogFooter>
          </form>
        ) : (
          <div className="space-y-4">
            <div className="space-y-2">
              <p className="text-sm font-medium">Restart report</p>
              {report.length === 0 ? (
                <p className="rounded-md border px-3 py-4 text-sm text-muted-foreground">
                  No instance uses this provider. The new secret is ready.
                </p>
              ) : (
                <ul className="divide-y rounded-md border">
                  {report.map((entry) => (
                    <li key={entry.id} className="space-y-1 px-3 py-2.5">
                      <div className="flex flex-wrap items-center justify-between gap-2">
                        <a
                          href={hrefFor(`/instances/${entry.id}`)}
                          className="min-w-0 truncate font-mono text-sm underline-offset-4 hover:underline"
                        >
                          {entry.id}
                        </a>
                        <Badge
                          variant={entry.status === "healthy" ? "secondary" : "destructive"}
                          className="font-mono text-xs font-normal"
                        >
                          {entry.status}
                        </Badge>
                      </div>
                      {entry.error ? (
                        <p className="font-mono text-xs break-words whitespace-pre-wrap text-destructive">
                          {entry.error}
                        </p>
                      ) : null}
                    </li>
                  ))}
                </ul>
              )}
              {failed > 0 ? (
                <p className="text-xs text-muted-foreground">
                  {failed} {plural(failed, "instance")} did not come back. The new credential is active;
                  check the error, then restart or fix the instance.
                </p>
              ) : null}
            </div>

            <DialogFooter>
              <Button autoFocus onClick={() => onOpenChange(false)}>
                Done
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

function DetailSkeleton() {
  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <Skeleton className="h-7 w-48" />
        <Skeleton className="h-4 w-80" />
      </div>
      <Skeleton className="h-9 w-64" />
      <div className="grid gap-6 lg:grid-cols-2">
        <Skeleton className="h-64 w-full rounded-md" />
        <Skeleton className="h-64 w-full rounded-md" />
      </div>
    </div>
  );
}

export function ProviderDetailPage({ id }: { id: string }) {
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [editOpen, setEditOpen] = useState(false);
  const [rotateOpen, setRotateOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<ProviderTestResult | null>(null);
  const [testError, setTestError] = useState<ApiError | null>(null);
  const detail = useResource(() => api.getProvider(id), [id, nonce]);

  async function testConnection() {
    setTesting(true);
    setTestError(null);
    setTestResult(null);
    try {
      setTestResult(await api.testProvider(id));
    } catch (cause) {
      setTestError(toApiError(cause));
    } finally {
      setTesting(false);
    }
  }

  if (detail.loading && !detail.data) return <DetailSkeleton />;

  if (!detail.data) {
    return (
      <>
        <PageHeader
          title={id}
          actions={
            <Button asChild variant="outline">
              <a href="#/providers">Back to providers</a>
            </Button>
          }
        />
        {detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null}
      </>
    );
  }

  const provider = detail.data;
  const linked = provider.instances_count;

  return (
    <>
      <PageHeader
        title={
          <span className="flex flex-wrap items-center gap-3">
            {provider.name}
            <Badge variant="outline" className="font-mono text-xs font-normal">
              {provider.kind}
            </Badge>
          </span>
        }
        description={
          <span className="flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-xs">
            <span>{provider.id}</span>
            <span aria-hidden>·</span>
            <span>
              {linked} linked {plural(linked, "instance")}
            </span>
          </span>
        }
        actions={
          <>
            <Button
              variant="outline"
              size="icon-sm"
              onClick={() => {
                setNonce((value) => value + 1);
                setTestResult(null);
                setTestError(null);
              }}
              aria-label="Refresh provider"
              title="Refresh provider"
            >
              <RefreshCw aria-hidden className={detail.refreshing ? "animate-spin" : undefined} />
            </Button>
            {canEdit ? (
              <>
                <Button variant="outline" size="sm" onClick={testConnection} disabled={testing}>
                  {testing ? <Loader2 aria-hidden className="animate-spin" /> : <Plug aria-hidden />}
                  Test connection
                </Button>
                <Button variant="outline" size="sm" onClick={() => setRotateOpen(true)}>
                  <RotateCw aria-hidden />
                  Rotate secret
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="icon-sm"
                      aria-label="More actions"
                      title="More actions"
                    >
                      <MoreHorizontal aria-hidden />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem onSelect={() => setEditOpen(true)}>
                      <Pencil aria-hidden />
                      Edit details…
                    </DropdownMenuItem>
                    <DropdownMenuItem variant="destructive" onSelect={() => setDeleteOpen(true)}>
                      <Trash2 aria-hidden />
                      Delete provider…
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </>
            ) : null}
          </>
        }
      />

      {testError ? <ErrorAlert error={testError} /> : null}
      {testResult ? <TestResultPanel result={testResult} baseUrl={provider.base_url} /> : null}

      {!canEdit ? (
        <p className="text-xs text-muted-foreground">
          Read-only: testing, rotating and deleting a provider needs the admin role.
        </p>
      ) : null}

      <div className="grid gap-6 lg:grid-cols-2">
        <Section title="Metadata">
          <DataList
            items={[
              { label: "Name", value: <Mono>{provider.name}</Mono> },
              { label: "Id", value: <CopyValue value={provider.id} label="provider id" /> },
              {
                label: "Kind",
                value: (
                  <Badge variant="outline" className="font-mono text-xs font-normal">
                    {provider.kind}
                  </Badge>
                ),
              },
              { label: "Base URL", value: <CopyValue value={provider.base_url} label="base URL" /> },
              { label: "API key env", value: <Mono>{provider.api_key_env}</Mono> },
              {
                label: "Instances",
                value:
                  linked === 0
                    ? "None"
                    : `${linked} ${plural(linked, "instance")} linked`,
              },
              {
                label: "Created",
                value: (
                  <span className="font-mono text-xs" title={formatRelative(provider.created_at)}>
                    {formatDateTime(provider.created_at)}
                  </span>
                ),
              },
              {
                label: "Updated",
                value: (
                  <span className="font-mono text-xs" title={formatRelative(provider.updated_at)}>
                    {formatDateTime(provider.updated_at)}
                  </span>
                ),
              },
            ]}
          />
        </Section>

        <Section
          title="Linked instances"
          description="Instances that draw their API key from this credential."
        >
          {provider.instances.length ? (
            <>
              <div className="divide-y rounded-md border">
                {provider.instances.map((instance) => (
                  <div
                    key={instance.id}
                    className="flex items-center justify-between gap-3 px-3 py-2"
                  >
                    <a
                      href={hrefFor(`/instances/${instance.id}`)}
                      className="flex min-w-0 items-center gap-2 rounded-sm"
                    >
                      <Boxes aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
                      <span className="min-w-0 truncate text-sm font-medium hover:underline">
                        {instance.name}
                      </span>
                    </a>
                    <span className="shrink-0 font-mono text-xs text-muted-foreground">
                      {instance.id}
                    </span>
                  </div>
                ))}
              </div>
              <p className="text-xs text-muted-foreground">
                Rotating the secret restarts every instance in this list, one at a time. The model stays
                per instance.
              </p>
            </>
          ) : (
            <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
              No instance uses this provider yet. Pick it when you create or duplicate an agent; its
              endpoint and variable name are inherited.
            </p>
          )}
        </Section>
      </div>

      {canEdit ? (
        <>
          <RotateSecretDialog
            provider={provider}
            open={rotateOpen}
            onOpenChange={setRotateOpen}
            onRotated={() => {
              setNonce((value) => value + 1);
              setTestResult(null);
              setTestError(null);
            }}
          />
          <ProviderFormDialog
            provider={provider}
            open={editOpen}
            onOpenChange={setEditOpen}
            onSaved={() => {
              setNonce((value) => value + 1);
              setTestResult(null);
              setTestError(null);
            }}
          />
          <DeleteProviderDialog
            provider={provider}
            open={deleteOpen}
            onOpenChange={setDeleteOpen}
            onDeleted={() => navigate("/providers")}
          />
        </>
      ) : null}

      {detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null}
    </>
  );
}
