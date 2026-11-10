import { useEffect, useState } from "react";
import { Loader2, Server, Unplug, Users } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { CopyButton } from "@/components/copy-button";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { McpHealthBadge, McpKindBadge, McpModeBadge } from "@/components/mcp-status";
import { RichText } from "@/components/rich-text";
import { StatusBadge } from "@/components/status-badge";
import { notifyInstancesChanged } from "@/lib/events";
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

/**
 * Attachments of one agent. A deployed server is a fleet service: attaching it
 * here only renders the `mcp_servers` block of `config.yaml`, it never starts a
 * second container, so several agents can share the same tools.
 */
export function InstanceMcp({ id }: { id: string }) {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [selected, setSelected] = useState("");
  const [mode, setMode] = useState("rw");
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const view = useResource(() => api.instanceMcpServers(id), [id, nonce]);
  const deployed = useResource(() => api.listMcpServers(), [id, nonce]);

  const attached = view.data?.servers ?? [];
  const attachedIds = new Set(attached.map((entry) => entry.server));
  const available = (deployed.data ?? []).filter((server) => !attachedIds.has(server.id));

  useEffect(() => {
    if (selected && available.some((server) => server.id === selected)) return;
    setSelected(available[0]?.id ?? "");
  }, [available, selected]);

  async function attach() {
    if (!selected) return;
    setPending("attach");
    setError(null);
    try {
      await api.attachInstanceMcpServer(id, selected, mode);
      toast.success(t("instance.mcp.attached", { name: selected }));
      setNonce((value) => value + 1);
      notifyInstancesChanged();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(null);
    }
  }

  async function detach(server: string) {
    setPending(`detach:${server}`);
    setError(null);
    try {
      await api.detachInstanceMcpServer(id, server);
      toast.success(t("instance.mcp.detached", { name: server }));
      setNonce((value) => value + 1);
      notifyInstancesChanged();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(null);
    }
  }

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        <RichText text={t("instance.mcp.description")} />
      </p>

      {view.error ? <ErrorAlert error={view.error} onRetry={view.reload} /> : null}
      {deployed.error ? <ErrorAlert error={deployed.error} onRetry={deployed.reload} /> : null}
      {error ? <ErrorAlert error={error} /> : null}

      {view.loading && !view.data ? (
        <Skeleton className="h-28 w-full rounded-md" />
      ) : view.error && !view.data ? null : attached.length === 0 ? (
        <div className="rounded-md border">
          <EmptyState
            icon={Server}
            title={t("instance.mcp.emptyTitle")}
            description={t("instance.mcp.emptyDescription")}
            action={
              <Button asChild variant="outline" size="sm">
                <a href={hrefFor("/mcp-servers")}>{t("instance.mcp.emptyAction")}</a>
              </Button>
            }
          />
        </div>
      ) : (
        <ul className="divide-y rounded-md border">
          {attached.map((entry) => {
            const deployedServer = deployed.data?.find(
              (server) => server.id === entry.server,
            );
            const others = Math.max(0, (deployedServer?.instances.length ?? 1) - 1);
            return (
              <li key={entry.server} className="flex flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <a
                    href={hrefFor(`/mcp-servers/${entry.server}`)}
                    className="flex min-w-0 flex-wrap items-baseline gap-x-2 text-sm font-medium underline-offset-4 hover:underline"
                  >
                    <span className="truncate font-mono">{entry.server}</span>
                    <McpKindBadge kind={entry.kind} />
                  </a>
                  <p className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted-foreground">
                    <span>
                      {entry.tools_count === 0
                        ? t("instance.mcp.toolsNone")
                        : t("instance.mcp.toolsCount", { count: entry.tools_count })}
                    </span>
                    {others > 0 ? (
                      <>
                        <span aria-hidden>·</span>
                        <span className="inline-flex items-center gap-1">
                          <Users aria-hidden className="size-3" />
                          {t("instance.mcp.sharedHint", { count: others })}
                        </span>
                      </>
                    ) : null}
                  </p>
                </div>
                <McpModeBadge mode={entry.mode} />
                <StatusBadge status={entry.status} labelPrefix="mcp.status" />
                <McpHealthBadge health={entry.health} />
                {entry.endpoint ? (
                  <CopyButton
                    value={entry.endpoint}
                    label={t("mcp.detail.endpointCopy")}
                    size="icon-xs"
                    variant="outline"
                  />
                ) : null}
                {canEdit ? (
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    onClick={() => void detach(entry.server)}
                    disabled={pending !== null}
                    aria-label={t("instance.mcp.detachAria", { name: entry.server })}
                    title={t("instance.mcp.detachAria", { name: entry.server })}
                  >
                    {pending === `detach:${entry.server}` ? (
                      <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
                    ) : (
                      <Unplug aria-hidden />
                    )}
                  </Button>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}

      {canEdit ? (
        <div className="space-y-3 rounded-md border p-4">
          {deployed.loading && !deployed.data ? (
            <Skeleton className="h-9 w-full rounded-md" />
          ) : (deployed.data ?? []).length === 0 ? (
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-sm text-muted-foreground">{t("instance.mcp.noDeployed")}</p>
              <Button asChild variant="outline" size="sm">
                <a href={hrefFor("/mcp-servers")}>{t("instance.mcp.noDeployedAction")}</a>
              </Button>
            </div>
          ) : available.length === 0 ? (
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-sm text-muted-foreground">{t("instance.mcp.noServers")}</p>
              <Button asChild variant="outline" size="sm">
                <a href={hrefFor("/mcp-servers")}>{t("instance.mcp.noServersAction")}</a>
              </Button>
            </div>
          ) : (
            <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_150px_auto] sm:items-end">
              <div className="space-y-1">
                <label htmlFor="instance-mcp-server" className="text-sm font-medium">
                  {t("instance.mcp.attachLabel")}
                </label>
                <Select value={selected} onValueChange={setSelected}>
                  <SelectTrigger id="instance-mcp-server" className="w-full">
                    <SelectValue placeholder={t("instance.mcp.attachPlaceholder")} />
                  </SelectTrigger>
                  <SelectContent>
                    {available.map((server) => (
                      <SelectItem key={server.id} value={server.id}>
                        <span className="min-w-0 truncate">{server.name}</span>
                        <span className="text-muted-foreground">
                          · {t(`mcp.status.${server.status}`)}
                        </span>
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1">
                <label htmlFor="instance-mcp-mode" className="text-sm font-medium">
                  {t("instance.mcp.mode")}
                </label>
                <Select value={mode} onValueChange={setMode}>
                  <SelectTrigger id="instance-mcp-mode" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="rw">{t("instance.mcp.modeRw")}</SelectItem>
                    <SelectItem value="readonly">{t("instance.mcp.modeReadonly")}</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <Button onClick={() => void attach()} disabled={pending !== null || !selected}>
                {pending === "attach" ? (
                  <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
                ) : null}
                {t("instance.mcp.attach")}
              </Button>
            </div>
          )}
          <p className="text-xs text-muted-foreground">
            <RichText text={t("instance.mcp.recreateHint")} />
          </p>
        </div>
      ) : null}
    </div>
  );
}
