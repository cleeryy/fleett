import { useState } from "react";
import { RefreshCw } from "lucide-react";
import { api } from "@/api";
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
import { ErrorAlert } from "@/components/error-alert";
import { useI18n } from "@/lib/i18n";
import { useResource } from "@/lib/use-resource";

const TAILS = [50, 100, 200, 500];

/** Plain tail viewer: one request per refresh, no stream to keep alive. */
export function McpLogs({ serverId }: { serverId: string }) {
  const { t } = useI18n();
  const [tail, setTail] = useState("100");
  const [nonce, setNonce] = useState(0);
  const logs = useResource(
    () => api.mcpServerLogs(serverId, { tail: Number(tail) }),
    [serverId, tail, nonce],
  );
  const text = logs.data ?? "";

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <Select value={tail} onValueChange={setTail}>
          <SelectTrigger size="sm" className="w-28" aria-label={t("mcp.detail.logsTail")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {TAILS.map((value) => (
              <SelectItem key={value} value={String(value)}>
                {value}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <p className="min-w-0 flex-1 text-xs text-muted-foreground">
          {t("mcp.detail.logsHint", { count: Number(tail) })}
        </p>
        <CopyButton
          value={text}
          label={t("mcp.detail.logsCopy")}
          size="icon-sm"
          variant="outline"
          disabled={!text}
        />
        <Button
          type="button"
          variant="outline"
          size="icon-sm"
          onClick={() => setNonce((value) => value + 1)}
          aria-label={t("common.refresh")}
          title={t("common.refresh")}
        >
          <RefreshCw
            aria-hidden
            className={logs.refreshing ? "animate-spin motion-reduce:animate-none" : undefined}
          />
        </Button>
      </div>

      {logs.error ? <ErrorAlert error={logs.error} onRetry={logs.reload} /> : null}

      {logs.loading && !logs.data ? (
        <Skeleton className="h-48 w-full rounded-md" />
      ) : (
        <pre
          tabIndex={0}
          role="region"
          aria-label={t("mcp.detail.logs")}
          aria-busy={logs.refreshing}
          className="max-h-72 min-h-32 overflow-auto rounded-md border bg-muted/20 p-3 font-mono text-[11px] leading-relaxed break-words whitespace-pre-wrap"
        >
          {text.trim() ? text : t("mcp.detail.logsEmpty")}
        </pre>
      )}
    </div>
  );
}
