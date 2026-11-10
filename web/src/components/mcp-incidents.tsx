import { useState } from "react";
import { Loader2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError } from "@/api/errors";
import type { McpIncident } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { ErrorAlert } from "@/components/error-alert";
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

export function McpIncidents({ serverId }: { serverId: string }) {
  const { t, lang } = useI18n();
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [closing, setClosing] = useState<number | null>(null);
  const incidents = useResource(
    () => api.mcpServerIncidents(serverId, { limit: 20 }),
    [serverId, nonce],
  );
  const rows = incidents.data ?? [];

  async function close(incident: McpIncident) {
    setClosing(incident.id);
    try {
      await api.closeMcpIncident(incident.id);
      toast.success(t("mcp.detail.incidentClosed", { id: incident.id }));
      setNonce((value) => value + 1);
    } catch (cause) {
      toast.error(apiErrorMessage(toApiError(cause), t));
    } finally {
      setClosing(null);
    }
  }

  if (incidents.loading && !incidents.data) {
    return <Skeleton className="h-24 w-full rounded-md" />;
  }
  if (incidents.error) {
    return <ErrorAlert error={incidents.error} onRetry={incidents.reload} />;
  }
  if (rows.length === 0) {
    return (
      <p className="rounded-md border px-3 py-4 text-sm text-muted-foreground">
        {t("mcp.detail.incidentsEmpty")}
      </p>
    );
  }

  return (
    <ul className="divide-y rounded-md border">
      {rows.map((incident) => {
        const open = incident.status === "open";
        return (
          <li key={incident.id} className="space-y-1.5 px-3 py-2.5">
            <div className="flex flex-wrap items-center gap-2">
              <Badge
                variant="outline"
                className={cn(
                  "font-mono text-[11px] font-normal",
                  open
                    ? "border-amber-600/40 text-amber-700 dark:text-amber-400"
                    : "text-muted-foreground",
                )}
              >
                {open ? t("status.open") : t("status.closed")}
              </Badge>
              <span className="min-w-0 flex-1 truncate font-mono text-xs" title={incident.cause}>
                {incident.cause}
              </span>
              <time
                dateTime={incident.opened_at}
                title={formatDateTime(incident.opened_at)}
                className="text-[11px] text-muted-foreground"
              >
                {formatRelative(incident.opened_at, lang)}
              </time>
              {open && canEdit ? (
                <Button
                  type="button"
                  variant="outline"
                  size="xs"
                  onClick={() => void close(incident)}
                  disabled={closing === incident.id}
                  aria-label={t("mcp.detail.incidentCloseAria", { id: incident.id })}
                >
                  {closing === incident.id ? (
                    <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
                  ) : null}
                  {t("mcp.detail.incidentClose")}
                </Button>
              ) : null}
            </div>
            {incident.detail ? (
              <p className="text-xs break-words text-muted-foreground">{incident.detail}</p>
            ) : null}
            <p className="flex flex-wrap items-center gap-x-2 text-[11px] text-muted-foreground">
              <span>{t("mcp.detail.incidentRestarts", { count: incident.restart_count })}</span>
              {incident.closed_at ? (
                <>
                  <span aria-hidden>·</span>
                  <time dateTime={incident.closed_at} title={formatDateTime(incident.closed_at)}>
                    {formatRelative(incident.closed_at, lang)}
                  </time>
                </>
              ) : null}
            </p>
          </li>
        );
      })}
    </ul>
  );
}
