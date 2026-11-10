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
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { providerHeaderNames } from "@/lib/providers";
import { hrefFor, navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

function Mono({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={cn("font-mono text-xs break-all", className)}>{children}</span>;
}

function CopyValue({ value, label }: { value: string; label: string }) {
  const { t } = useI18n();
  return (
    <span className="flex min-w-0 items-center gap-1">
      <span className="min-w-0 flex-1 font-mono text-xs break-all">{value}</span>
      <CopyButton
        value={value}
        label={t("providers.detail.copyLabel", { label })}
        size="icon-xs"
        className="-my-1 shrink-0"
      />
    </span>
  );
}

function TestResultPanel({ result, baseUrl }: { result: ProviderTestResult; baseUrl: string }) {
  const { t } = useI18n();
  const probeUrl = `${baseUrl.replace(/\/+$/, "")}/models`;
  const bothOk = result.ok && result.models_ok && result.completion_ok;
  const title = bothOk
    ? t("providers.detail.testOk")
    : result.models_ok
      ? t("providers.detail.testPartial")
      : t("providers.detail.testFailed");

  function statusLabel(status: number | undefined): string {
    if (status === undefined) return t("providers.detail.notRun");
    if (status === 0) return t("providers.detail.noResponse");
    return `HTTP ${status}`;
  }

  const probes = [
    {
      key: "models",
      label: t("providers.detail.testModels"),
      hint: t("providers.detail.testModelsHint"),
      ok: result.models_ok,
      status: result.models_status,
    },
    {
      key: "completion",
      label: t("providers.detail.testCompletion"),
      hint: t("providers.detail.testCompletionHint"),
      ok: result.completion_ok,
      status: result.completion_status,
    },
  ];

  return (
    <div
      role="status"
      className={cn(
        "rounded-md border p-4",
        bothOk ? "bg-muted/20" : "border-destructive/40 bg-destructive/5",
      )}
    >
      <div className="flex flex-wrap items-center gap-2">
        {bothOk ? (
          <CheckCircle2 aria-hidden className="size-4 text-emerald-500" />
        ) : (
          <XCircle aria-hidden className="size-4 text-destructive" />
        )}
        <p className="text-sm font-medium">{title}</p>
        <span className="font-mono text-xs text-muted-foreground sm:ml-auto">
          {result.latency_ms} ms
        </span>
      </div>

      <ul className="mt-3 divide-y rounded-md border bg-background/60">
        {probes.map((probe) => (
          <li key={probe.key} className="flex items-start gap-2.5 px-3 py-2">
            {probe.ok ? (
              <CheckCircle2 aria-hidden className="mt-0.5 size-3.5 shrink-0 text-emerald-500" />
            ) : (
              <XCircle aria-hidden className="mt-0.5 size-3.5 shrink-0 text-destructive" />
            )}
            <div className="min-w-0 flex-1">
              <p className="text-xs font-medium">{probe.label}</p>
              <p className="text-[11px] break-words text-muted-foreground">{probe.hint}</p>
            </div>
            <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
              {statusLabel(probe.status)}
            </span>
          </li>
        ))}
      </ul>

      {result.error && !bothOk ? (
        <p className="mt-2 font-mono text-xs break-words whitespace-pre-wrap text-destructive">
          {result.error}
        </p>
      ) : null}
      {!result.completion_ok && result.models_ok ? (
        <p className="mt-1.5 text-xs text-muted-foreground">
          {t("providers.detail.testPartialBody")}
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
  const { t } = useI18n();
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
      toast.success(t("providers.detail.rotated", { name: provider.name }));
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
            {t("providers.detail.rotateTitle")}{" "}
            <span className="font-mono">{provider.name}</span>
          </DialogTitle>
          <DialogDescription>
            {provider.instances_count === 0
              ? t("providers.detail.rotateNone")
              : t("providers.detail.rotateMany", { count: provider.instances_count })}
          </DialogDescription>
        </DialogHeader>

        {report === null ? (
          <form onSubmit={onSubmit} noValidate className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="rotate-secret">{t("providers.detail.newApiKey")}</Label>
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
                {t("providers.detail.rotateHint")}
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
                {t("common.cancel")}
              </Button>
              <Button type="submit" disabled={pending || !secret.trim()}>
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <RotateCw aria-hidden />}
                {t("providers.detail.rotateSecret")}
              </Button>
            </DialogFooter>
          </form>
        ) : (
          <div className="space-y-4">
            <div className="space-y-2">
              <p className="text-sm font-medium">{t("providers.detail.restartReport")}</p>
              {report.length === 0 ? (
                <p className="rounded-md border px-3 py-4 text-sm text-muted-foreground">
                  {t("providers.detail.reportEmpty")}
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
                          {t(`health.${entry.status}`)}
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
                  {t(
                    failed === 1
                      ? "providers.detail.failedOne"
                      : "providers.detail.failedMany",
                    { count: failed },
                  )}
                </p>
              ) : null}
            </div>

            <DialogFooter>
              <Button autoFocus onClick={() => onOpenChange(false)}>
                {t("common.done")}
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
  const { t, lang } = useI18n();
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
              <a href="#/providers">{t("providers.detail.back")}</a>
            </Button>
          }
        />
        {detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null}
      </>
    );
  }

  const provider = detail.data;
  const linked = provider.instances_count;
  const headerNames = providerHeaderNames(provider);

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
              {t(linked === 1 ? "providers.detail.linkedOne" : "providers.detail.linkedMany", {
                count: linked,
              })}
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
              aria-label={t("providers.detail.refresh")}
              title={t("providers.detail.refresh")}
            >
              <RefreshCw aria-hidden className={detail.refreshing ? "animate-spin" : undefined} />
            </Button>
            {canEdit ? (
              <>
                <Button variant="outline" size="sm" onClick={testConnection} disabled={testing}>
                  {testing ? <Loader2 aria-hidden className="animate-spin" /> : <Plug aria-hidden />}
                  {t("providers.detail.testConnection")}
                </Button>
                <Button variant="outline" size="sm" onClick={() => setRotateOpen(true)}>
                  <RotateCw aria-hidden />
                  {t("providers.detail.rotateSecret")}
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="icon-sm"
                      aria-label={t("providers.detail.moreActions")}
                      title={t("providers.detail.moreActions")}
                    >
                      <MoreHorizontal aria-hidden />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem onSelect={() => setEditOpen(true)}>
                      <Pencil aria-hidden />
                      {t("providers.detail.editDetails")}
                    </DropdownMenuItem>
                    <DropdownMenuItem variant="destructive" onSelect={() => setDeleteOpen(true)}>
                      <Trash2 aria-hidden />
                      {t("providers.detail.deleteProvider")}
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
          {t("providers.detail.readOnly")}
        </p>
      ) : null}

      <div className="grid gap-6 lg:grid-cols-2">
        <Section title={t("providers.detail.metadata")}>
          <DataList
            items={[
              { label: t("common.name"), value: <Mono>{provider.name}</Mono> },
              {
                label: t("providers.detail.id"),
                value: <CopyValue value={provider.id} label={t("providers.detail.providerId")} />,
              },
              {
                label: t("providers.form.kind"),
                value: (
                  <Badge variant="outline" className="font-mono text-xs font-normal">
                    {provider.kind}
                  </Badge>
                ),
              },
              {
                label: t("providers.form.baseUrl"),
                value: <CopyValue value={provider.base_url} label={t("providers.detail.baseUrl")} />,
              },
              {
                label: t("providers.form.apiKeyEnv"),
                value: <Mono>{provider.api_key_env}</Mono>,
              },
              {
                label: t("providers.form.model"),
                value: provider.model ? (
                  <Mono>{provider.model}</Mono>
                ) : (
                  <span className="text-xs text-muted-foreground">
                    {t("providers.detail.noModel")}
                  </span>
                ),
              },
              {
                label: t("providers.form.headers"),
                value:
                  headerNames.length > 0 ? (
                    <span className="flex flex-wrap gap-1">
                      {headerNames.map((name) => (
                        <Badge
                          key={name}
                          variant="outline"
                          className="font-mono text-[11px] font-normal"
                        >
                          {name}
                        </Badge>
                      ))}
                    </span>
                  ) : (
                    <span className="text-xs text-muted-foreground">
                      {t("providers.detail.noHeaders")}
                    </span>
                  ),
              },
              {
                label: t("nav.instances"),
                value:
                  linked === 0
                    ? t("providers.detail.noneLinked")
                    : t("providers.detail.linkedValue", { count: linked }),
              },
              {
                label: t("common.created"),
                value: (
                  <span
                    className="font-mono text-xs"
                    title={formatRelative(provider.created_at, lang)}
                  >
                    {formatDateTime(provider.created_at)}
                  </span>
                ),
              },
              {
                label: t("common.updated"),
                value: (
                  <span
                    className="font-mono text-xs"
                    title={formatRelative(provider.updated_at, lang)}
                  >
                    {formatDateTime(provider.updated_at)}
                  </span>
                ),
              },
            ]}
          />
          <p className="text-xs text-muted-foreground">{t("providers.detail.headersHint")}</p>
        </Section>

        <Section
          title={t("providers.detail.linkedInstances")}
          description={t("providers.detail.linkedDescription")}
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
                {t("providers.detail.rotatingHint")}
              </p>
            </>
          ) : (
            <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
              {t("providers.detail.noLinked")}
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
