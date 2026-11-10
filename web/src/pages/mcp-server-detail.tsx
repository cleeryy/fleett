import { useState } from "react";
import {
  Activity,
  FlaskConical,
  KeyRound,
  Loader2,
  MoreHorizontal,
  Play,
  RefreshCw,
  RotateCw,
  Square,
  Trash2,
  TriangleAlert,
  Wrench,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
import type { McpTestResult } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { DataList, Section } from "@/components/data-list";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { McpEnvDialog } from "@/components/mcp-env-dialog";
import { McpIncidents } from "@/components/mcp-incidents";
import { McpLogs } from "@/components/mcp-logs";
import { McpHealthBadge, McpKindBadge } from "@/components/mcp-status";
import { PageHeader } from "@/components/page-header";
import { StatusBadge } from "@/components/status-badge";
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

function attachedAgentsOf(error: ApiError): string[] {
  const details = error.details as { instances?: unknown } | undefined;
  if (!details || !Array.isArray(details.instances)) return [];
  return details.instances.filter((id): id is string => typeof id === "string");
}

function DetailSkeleton() {
  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <Skeleton className="h-7 w-52" />
        <Skeleton className="h-4 w-80" />
      </div>
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_320px]">
        <Skeleton className="h-64 w-full rounded-md" />
        <Skeleton className="h-64 w-full rounded-md" />
      </div>
    </div>
  );
}

export function McpServerDetailPage({ id }: { id: string }) {
  const { t, lang } = useI18n();
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<McpTestResult | null>(null);
  const [testError, setTestError] = useState<ApiError | null>(null);
  const [envOpen, setEnvOpen] = useState(false);
  const [envToRemove, setEnvToRemove] = useState<string | null>(null);
  const [removingEnv, setRemovingEnv] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deletePending, setDeletePending] = useState(false);
  const [deleteError, setDeleteError] = useState<ApiError | null>(null);
  const [deleteAttached, setDeleteAttached] = useState<string[]>([]);
  const [purge, setPurge] = useState(false);
  const detail = useResource(() => api.getMcpServer(id), [id, nonce]);

  if (detail.loading && !detail.data) return <DetailSkeleton />;

  if (!detail.data) {
    return (
      <>
        <PageHeader
          title={t("mcp.detail.notFound")}
          actions={
            <Button asChild variant="outline">
              <a href="#/mcp-servers">{t("nav.mcpServers")}</a>
            </Button>
          }
        />
        {detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null}
      </>
    );
  }

  const server = detail.data;
  const running = server.status === "running";
  const busy = pendingAction !== null || testing;
  const attachments = server.instances.length;

  async function run(action: "start" | "stop" | "restart") {
    setPendingAction(action);
    setActionError(null);
    setTestError(null);
    try {
      const next = await api.mcpServerAction(id, action);
      toast.success(
        t("mcp.detail.actionDone", {
          name: next.name,
          state: t(
            action === "stop"
              ? "status.stopped"
              : action === "start"
                ? "status.running"
                : "status.restarted",
          ),
        }),
      );
      setTestResult(null);
      setNonce((value) => value + 1);
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setPendingAction(null);
    }
  }

  async function test() {
    setTesting(true);
    setTestError(null);
    setTestResult(null);
    try {
      const result = await api.testMcpServer(id);
      setTestResult(result);
      toast.success(t("mcp.detail.testToast", { latency: result.latency_ms }));
      setNonce((value) => value + 1);
    } catch (cause) {
      setTestError(toApiError(cause));
    } finally {
      setTesting(false);
    }
  }

  async function removeEnv() {
    if (!envToRemove) return;
    setRemovingEnv(true);
    const key = envToRemove;
    try {
      await api.deleteMcpServerEnv(id, key);
      toast.success(t("mcp.detail.envRemoved", { key }));
      setEnvToRemove(null);
      setNonce((value) => value + 1);
    } catch (cause) {
      toast.error(apiErrorMessage(toApiError(cause), t));
      setEnvToRemove(null);
    } finally {
      setRemovingEnv(false);
    }
  }

  async function remove() {
    setDeletePending(true);
    setDeleteError(null);
    try {
      await api.deleteMcpServer(id, purge);
      toast.success(t("mcp.detail.deleted", { name: server.name }));
      navigate("/mcp-servers");
    } catch (cause) {
      const failure = toApiError(cause);
      setDeleteError(failure);
      const attached = attachedAgentsOf(failure);
      if (attached.length > 0) setDeleteAttached(attached);
    } finally {
      setDeletePending(false);
    }
  }

  return (
    <>
      <PageHeader
        title={
          <span className="flex flex-wrap items-center gap-3">
            <span className="min-w-0 break-words">{server.name}</span>
            <McpKindBadge kind={server.kind} />
            <StatusBadge status={server.status} labelPrefix="mcp.status" />
            <McpHealthBadge health={server.health} />
          </span>
        }
        description={
          <span className="space-y-1">
            <span className="block">
              {server.description || t("mcp.detail.descriptionFallback")}
            </span>
            <span className="flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-xs">
              <span>{server.id}</span>
              <span aria-hidden>·</span>
              <span>{t("mcp.detail.limitsValue", { cpu: server.cpu_limit, mem: server.mem_limit_mb })}</span>
              <span aria-hidden>·</span>
              <span>
                {t(
                  attachments === 1 ? "mcp.detail.agentsOne" : "mcp.detail.agentsMany",
                  { count: attachments },
                )}
              </span>
            </span>
          </span>
        }
        actions={
          <>
            <Button
              variant="outline"
              size="icon-sm"
              onClick={() => setNonce((value) => value + 1)}
              aria-label={t("common.refresh")}
              title={t("common.refresh")}
            >
              <RefreshCw
                aria-hidden
                className={detail.refreshing ? "animate-spin motion-reduce:animate-none" : undefined}
              />
            </Button>
            {canEdit ? (
              <>
                <Button variant="outline" size="sm" onClick={() => void test()} disabled={busy}>
                  {testing ? (
                    <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
                  ) : (
                    <FlaskConical aria-hidden />
                  )}
                  {testing ? t("mcp.detail.testing") : t("mcp.detail.test")}
                </Button>
                {running ? (
                  <Button size="sm" onClick={() => void run("stop")} disabled={busy}>
                    {pendingAction === "stop" ? (
                      <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
                    ) : (
                      <Square aria-hidden />
                    )}
                    {t("action.stop")}
                  </Button>
                ) : (
                  <Button size="sm" onClick={() => void run("start")} disabled={busy}>
                    {pendingAction === "start" ? (
                      <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
                    ) : (
                      <Play aria-hidden />
                    )}
                    {t("action.start")}
                  </Button>
                )}
                <Button variant="outline" size="sm" onClick={() => void run("restart")} disabled={busy}>
                  {pendingAction === "restart" ? (
                    <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
                  ) : (
                    <RotateCw aria-hidden />
                  )}
                  {t("action.restart")}
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="icon-sm"
                      aria-label={t("mcp.detail.moreActions")}
                      title={t("mcp.detail.moreActions")}
                      disabled={busy}
                    >
                      <MoreHorizontal aria-hidden />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem
                      variant="destructive"
                      onSelect={() => {
                        setDeleteAttached(server.instances);
                        setDeleteError(null);
                        setPurge(false);
                        setDeleteOpen(true);
                      }}
                    >
                      <Trash2 aria-hidden />
                      {t("mcp.detail.delete")}
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </>
            ) : null}
          </>
        }
      />

      {actionError ? <ErrorAlert error={actionError} /> : null}

      {testResult ? (
        <div
          role="status"
          className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-md border border-emerald-600/40 bg-emerald-50/40 px-3 py-2 text-sm dark:bg-emerald-950/20"
        >
          <Activity aria-hidden className="size-4 shrink-0 text-emerald-700 dark:text-emerald-400" />
          <span className="min-w-0 flex-1">
            {t("mcp.detail.testOk", {
              latency: testResult.latency_ms,
              tools: testResult.tools_count,
              protocol: testResult.protocol_version,
            })}
          </span>
        </div>
      ) : null}

      {testError ? <ErrorAlert error={testError} onRetry={() => void test()} /> : null}

      {server.last_error ? (
        <div className="flex items-start gap-2 rounded-md border border-destructive/40 px-3 py-2">
          <TriangleAlert aria-hidden className="mt-0.5 size-4 shrink-0 text-destructive" />
          <div className="min-w-0 space-y-0.5">
            <p className="text-sm font-medium text-destructive">{t("mcp.detail.lastError")}</p>
            <p className="font-mono text-xs break-words whitespace-pre-wrap">{server.last_error}</p>
          </div>
        </div>
      ) : null}

      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_320px]">
        <div className="min-w-0 space-y-6">
          <Section
            title={t("mcp.detail.endpoint")}
            description={t("mcp.detail.endpointHint")}
            action={
              server.endpoint ? (
                <CopyButton
                  value={server.endpoint}
                  label={t("mcp.detail.endpointCopy")}
                  size="icon-sm"
                  variant="outline"
                />
              ) : undefined
            }
          >
            {server.endpoint ? (
              <div className="flex items-start gap-2 rounded-md border bg-muted/20 px-3 py-2">
                <span className="min-w-0 flex-1 font-mono text-xs break-all">{server.endpoint}</span>
              </div>
            ) : (
              <p className="rounded-md border border-dashed px-3 py-2 text-sm text-muted-foreground">
                {t("mcp.detail.endpointNone")}
              </p>
            )}
            {server.command.length > 0 ? (
              <p className="font-mono text-[11px] break-all text-muted-foreground">
                {t("mcp.detail.command")}: {server.command.join(" ")}
              </p>
            ) : (
              <p className="text-[11px] text-muted-foreground">{t("mcp.detail.commandNone")}</p>
            )}
          </Section>

          <Section
            title={
              server.tools_count === 0
                ? t("mcp.detail.tools")
                : `${t("mcp.detail.tools")} (${server.tools_count})`
            }
            description={
              server.last_probe_at
                ? t("mcp.detail.toolsHint", { time: formatRelative(server.last_probe_at, lang) })
                : t("mcp.detail.toolsNever")
            }
          >
            {server.tools.length === 0 ? (
              <div className="rounded-md border">
                <EmptyState
                  icon={Wrench}
                  title={t("mcp.detail.toolsEmpty")}
                  description={t("mcp.detail.toolsEmptyDescription")}
                  action={
                    canEdit ? (
                      <Button variant="outline" size="sm" onClick={() => void test()} disabled={busy}>
                        <FlaskConical aria-hidden />
                        {t("mcp.detail.test")}
                      </Button>
                    ) : undefined
                  }
                />
              </div>
            ) : (
              <>
                <ul className="divide-y rounded-md border">
                  {server.tools.map((tool) => (
                    <li key={tool.name} className="space-y-0.5 px-3 py-2">
                      <p className="font-mono text-xs font-medium break-all">{tool.name}</p>
                      {tool.description ? (
                        <p className="line-clamp-2 text-xs text-muted-foreground">
                          {tool.description}
                        </p>
                      ) : null}
                    </li>
                  ))}
                </ul>
                <p className="flex flex-wrap items-center gap-1 font-mono text-[10px] text-muted-foreground">
                  <span>{t("mcp.detail.toolsHash")}</span>
                  <span className="break-all">{server.tools_hash}</span>
                </p>
              </>
            )}
          </Section>

          <Section title={t("mcp.detail.logs")}>
            <McpLogs serverId={id} />
          </Section>

          <Section title={t("mcp.detail.incidents")} description={t("mcp.detail.incidentsHint")}>
            <McpIncidents serverId={id} />
          </Section>
        </div>

        <div className="min-w-0 space-y-6">
          <Section
            title={t("mcp.detail.overview")}
            description={t("mcp.detail.shared")}
          >
            <DataList
              items={[
                {
                  label: t("mcp.detail.image"),
                  value: (
                    <span className="font-mono text-xs break-all">{server.image || "—"}</span>
                  ),
                },
                {
                  label: t("mcp.detail.host"),
                  value: <span className="font-mono text-xs">{server.host}</span>,
                },
                {
                  label: t("mcp.detail.port"),
                  value: <span className="font-mono text-xs">{server.port}</span>,
                },
                {
                  label: t("mcp.detail.path"),
                  value: <span className="font-mono text-xs break-all">{server.path}</span>,
                },
                {
                  label: t("mcp.detail.persist"),
                  value: (
                    <span className="text-sm text-muted-foreground">
                      {server.persist ? t("mcp.detail.persistOn") : t("mcp.detail.persistOff")}
                    </span>
                  ),
                },
                {
                  label: t("mcp.detail.lastProbe"),
                  value: server.last_probe_at ? (
                    <span
                      className="font-mono text-xs"
                      title={formatDateTime(server.last_probe_at)}
                    >
                      {formatRelative(server.last_probe_at, lang)}
                    </span>
                  ) : (
                    <span className="text-muted-foreground">{t("mcp.health.none")}</span>
                  ),
                },
                {
                  label: t("mcp.detail.created"),
                  value: (
                    <span className="font-mono text-xs" title={formatDateTime(server.created_at)}>
                      {formatRelative(server.created_at, lang)}
                    </span>
                  ),
                },
                {
                  label: t("mcp.detail.updated"),
                  value: (
                    <span className="font-mono text-xs" title={formatDateTime(server.updated_at)}>
                      {formatRelative(server.updated_at, lang)}
                    </span>
                  ),
                },
              ]}
            />
          </Section>

          <Section
            title={t("mcp.detail.env")}
            description={t("mcp.detail.envHint")}
            action={
              canEdit ? (
                <Button variant="outline" size="xs" onClick={() => setEnvOpen(true)}>
                  {t("mcp.detail.envReplace")}
                </Button>
              ) : undefined
            }
          >
            {server.env.length === 0 ? (
              <p className="rounded-md border px-3 py-4 text-sm text-muted-foreground">
                {t("mcp.detail.envEmpty")}
              </p>
            ) : (
              <ul className="divide-y rounded-md border">
                {server.env.map((entry) => (
                  <li key={entry.key} className="flex flex-wrap items-center gap-2 px-3 py-2">
                    <KeyRound aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
                    <span className="min-w-0 flex-1 truncate font-mono text-xs" title={entry.key}>
                      {entry.key}
                    </span>
                    {entry.secret ? (
                      <Badge
                        variant="outline"
                        className="shrink-0 border-amber-600/40 px-1.5 py-0 text-[10px] font-normal text-amber-800 dark:text-amber-300"
                      >
                        {t("mcp.detail.envSecret")}
                      </Badge>
                    ) : null}
                    <Badge
                      variant="outline"
                      className={cn(
                        "shrink-0 px-1.5 py-0 font-mono text-[10px] font-normal",
                        entry.set ? "text-foreground" : "text-muted-foreground",
                      )}
                    >
                      {entry.set ? t("mcp.detail.envSet") : t("mcp.detail.envUnset")}
                    </Badge>
                    {canEdit ? (
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-xs"
                        onClick={() => setEnvToRemove(entry.key)}
                        aria-label={t("mcp.detail.envRemove", { key: entry.key })}
                        title={t("mcp.detail.envRemove", { key: entry.key })}
                      >
                        <Trash2 aria-hidden />
                      </Button>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
          </Section>
        </div>
      </div>

      {canEdit ? (
        <>
          <McpEnvDialog
            server={server}
            open={envOpen}
            onOpenChange={setEnvOpen}
            onSaved={() => setNonce((value) => value + 1)}
          />

          <ConfirmDialog
            open={envToRemove !== null}
            onOpenChange={(open) => {
              if (!open) setEnvToRemove(null);
            }}
            title={t("mcp.detail.envRemoveTitle", { key: envToRemove ?? "" })}
            description={t("mcp.detail.envRemoveDescription")}
            confirmLabel={t("common.remove")}
            destructive
            pending={removingEnv}
            onConfirm={() => void removeEnv()}
          />

          <ConfirmDialog
            open={deleteOpen}
            onOpenChange={(open) => {
              if (!open && !deletePending) setDeleteOpen(false);
            }}
            title={t("mcp.detail.deleteTitle", { name: server.name })}
            description={t("mcp.detail.deleteDescription")}
            confirmLabel={t("common.delete")}
            destructive
            pending={deletePending}
            error={deleteError}
            onConfirm={() => void remove()}
          >
            <div className="space-y-3">
              {deleteAttached.length > 0 ? (
                <p className="flex items-start gap-2 rounded-md border border-amber-600/40 bg-amber-50/40 px-3 py-2 text-sm dark:bg-amber-950/20">
                  <TriangleAlert
                    aria-hidden
                    className="mt-0.5 size-4 shrink-0 text-amber-700 dark:text-amber-400"
                  />
                  <span className="min-w-0 break-words">
                    {t("mcp.detail.deleteAttached", { agents: deleteAttached.join(", ") })}
                  </span>
                </p>
              ) : null}
              <div className="flex items-start gap-2">
                <Checkbox
                  id="mcp-delete-purge"
                  checked={purge}
                  onCheckedChange={(value) => setPurge(value === true)}
                />
                <Label htmlFor="mcp-delete-purge" className="font-normal">
                  {t("mcp.detail.deletePurge")}
                </Label>
              </div>
            </div>
          </ConfirmDialog>
        </>
      ) : null}
    </>
  );
}
