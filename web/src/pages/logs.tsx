import { useEffect, useMemo, useRef, useState } from "react";
import {
  Activity,
  ChevronDown,
  Download,
  Loader2,
  RefreshCw,
  ScrollText,
  Search,
} from "lucide-react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { LogEntry, LogLevel, LogQueryParams } from "@/api/types";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
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
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { Highlight } from "@/components/highlight";
import { OpsBadge } from "@/components/ops-badge";
import { PageHeader } from "@/components/page-header";
import { RichText } from "@/components/rich-text";
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { useResource } from "@/lib/use-resource";

const SINCE_OPTIONS = [
  { value: "all", labelKey: "logs.sinceAll" },
  { value: "30m", labelKey: "logs.since30m" },
  { value: "1h", labelKey: "logs.since1h" },
  { value: "24h", labelKey: "logs.since24h" },
  { value: "7d", labelKey: "logs.since7d" },
];

function mergeEntries(previous: LogEntry[], incoming: LogEntry[]): LogEntry[] {
  const byId = new Map(previous.map((entry) => [entry.id, entry]));
  for (const entry of incoming) byId.set(entry.id, entry);
  return [...byId.values()]
    .sort((left, right) => Date.parse(right.at) - Date.parse(left.at) || right.id - left.id)
    .slice(0, 500);
}

function toIso(value: string): string | undefined {
  if (!value) return undefined;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? undefined : date.toISOString();
}

export function LogsPage() {
  const { t, lang } = useI18n();
  const [instances, setInstances] = useState<string[]>([]);
  const [q, setQ] = useState("");
  const [level, setLevel] = useState<"all" | LogLevel>("all");
  const [since, setSince] = useState("24h");
  const [until, setUntil] = useState("");
  const [limit, setLimit] = useState("100");
  const [nonce, setNonce] = useState(0);
  const [follow, setFollow] = useState(false);
  const [live, setLive] = useState<LogEntry[]>([]);
  const [followError, setFollowError] = useState<ApiError | null>(null);
  const [exporting, setExporting] = useState<"json" | "csv" | null>(null);
  const [exportError, setExportError] = useState<ApiError | null>(null);
  const cursor = useRef<string | null>(null);

  const params: LogQueryParams = useMemo(
    () => ({
      ...(instances.length > 0 ? { instances } : {}),
      ...(q.trim() ? { q: q.trim() } : {}),
      ...(level === "all" ? {} : { level }),
      ...(since === "all" ? {} : { since }),
      ...(until ? { until: toIso(until) } : {}),
      limit: Number(limit),
    }),
    [instances, q, level, since, until, limit],
  );

  const filterKey = JSON.stringify(params);
  const paramsRef = useRef(params);
  paramsRef.current = params;

  const logs = useResource(() => api.searchLogs(params), [filterKey, nonce]);
  const fleet = useResource(() => api.listInstances(), []);

  // The SSE endpoint authenticates with the session cookie or a bearer header,
  // and EventSource cannot send the header. Rather than break bearer-only
  // deployments, `follow` polls every 2 s and resumes from the newest line.
  useEffect(() => {
    if (!follow) return;
    setLive([]);
    setFollowError(null);
    cursor.current = new Date().toISOString();
    let alive = true;
    let busy = false;
    const tick = async () => {
      if (busy) return;
      busy = true;
      try {
        const result = await api.searchLogs({
          ...paramsRef.current,
          ...(cursor.current ? { since: cursor.current } : {}),
          limit: 200,
        });
        if (!alive) return;
        if (result.entries.length > 0) {
          cursor.current = result.entries[0].at;
          setLive((previous) => mergeEntries(previous, result.entries));
        }
        setFollowError(null);
      } catch (cause) {
        if (alive) setFollowError(toApiError(cause));
      } finally {
        busy = false;
      }
    };
    void tick();
    const timer = window.setInterval(() => void tick(), 2_000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
    // The filter key restarts the cursor when the query changes on purpose.
  }, [follow, filterKey]);

  const rows = useMemo(() => {
    const base = logs.data?.entries ?? [];
    if (!follow) return base;
    return mergeEntries(live, base);
  }, [logs.data, live, follow]);

  const instanceOptions = (fleet.data ?? []).map((row) => row.id).sort();

  function toggleInstance(id: string, checked: boolean) {
    setInstances((previous) =>
      checked ? [...new Set([...previous, id])].sort() : previous.filter((value) => value !== id),
    );
  }

  async function download(format: "json" | "csv") {
    setExporting(format);
    setExportError(null);
    try {
      const { blob, filename } = await api.exportLogs(params, format);
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = filename === "fleett-export" ? `fleett-logs.${format}` : filename;
      link.rel = "noopener";
      document.body.appendChild(link);
      link.click();
      link.remove();
      window.setTimeout(() => URL.revokeObjectURL(url), 1_000);
    } catch (cause) {
      setExportError(toApiError(cause));
    } finally {
      setExporting(null);
    }
  }

  const instanceLabel =
    instances.length === 0
      ? t("logs.allInstances")
      : t(instances.length === 1 ? "fleet.countOne" : "fleet.countMany", {
          count: instances.length,
        });

  return (
    <>
      <PageHeader
        title={t("nav.logs")}
        description={
          logs.data
            ? t(rows.length === 1 ? "logs.countOne" : "logs.countMany", { count: rows.length })
            : t("logs.loading")
        }
        actions={
          <>
            <Button
              variant={follow ? "default" : "outline"}
              size="sm"
              aria-pressed={follow}
              title={t("logs.followTitle")}
              onClick={() => setFollow((value) => !value)}
            >
              {follow ? (
                <Loader2 aria-hidden className="animate-spin" />
              ) : (
                <Activity aria-hidden />
              )}
              {follow ? t("logs.following") : t("logs.follow")}
            </Button>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={logs.refreshing ? "animate-spin" : undefined} />
              {t("common.refresh")}
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="sm" disabled={exporting !== null}>
                  {exporting ? <Loader2 aria-hidden className="animate-spin" /> : <Download aria-hidden />}
                  {t("logs.export")}
                  <ChevronDown aria-hidden />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onSelect={() => void download("json")}>
                  {t("logs.exportJson")}
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => void download("csv")}>
                  {t("logs.exportCsv")}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </>
        }
      />

      {follow ? (
        <p className="text-xs text-muted-foreground" role="status">
          <RichText text={t("logs.followHint")} />
        </p>
      ) : null}

      {exportError ? <ErrorAlert error={exportError} /> : null}
      {followError ? <ErrorAlert error={followError} /> : null}
      {logs.error ? <ErrorAlert error={logs.error} onRetry={logs.reload} /> : null}

      <div className="flex flex-col gap-3 lg:flex-row lg:flex-wrap lg:items-center">
        <div className="relative w-full lg:max-w-xs">
          <Search
            aria-hidden
            className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground"
          />
          <Input
            value={q}
            onChange={(event) => setQ(event.target.value)}
            placeholder={t("logs.searchPlaceholder")}
            aria-label={t("logs.searchAria")}
            className="pl-8"
          />
        </div>

        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="outline" className="justify-between lg:w-52">
              {instanceLabel}
              <ChevronDown aria-hidden />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="max-h-80 w-60 overflow-y-auto">
            <DropdownMenuLabel>{t("nav.instances")}</DropdownMenuLabel>
            <DropdownMenuCheckboxItem
              checked={instances.length === 0}
              onSelect={(event) => event.preventDefault()}
              onCheckedChange={() => setInstances([])}
            >
              {t("logs.allInstances")}
            </DropdownMenuCheckboxItem>
            <DropdownMenuSeparator />
            {instanceOptions.length === 0 ? (
              <p className="px-2 py-1.5 text-xs text-muted-foreground">
                {t("logs.noInstance")}
              </p>
            ) : (
              instanceOptions.map((id) => (
                <DropdownMenuCheckboxItem
                  key={id}
                  checked={instances.includes(id)}
                  onSelect={(event) => event.preventDefault()}
                  onCheckedChange={(checked) => toggleInstance(id, checked === true)}
                >
                  <span className="font-mono text-xs">{id}</span>
                </DropdownMenuCheckboxItem>
              ))
            )}
          </DropdownMenuContent>
        </DropdownMenu>

        <Select value={level} onValueChange={(value) => setLevel(value as "all" | LogLevel)}>
          <SelectTrigger className="w-full lg:w-36" aria-label={t("logs.levelAria")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t("logs.levelAll")}</SelectItem>
            <SelectItem value="info">{t("logs.levelInfo")}</SelectItem>
            <SelectItem value="warn">{t("logs.levelWarn")}</SelectItem>
            <SelectItem value="error">{t("logs.levelError")}</SelectItem>
          </SelectContent>
        </Select>

        <Select value={since} onValueChange={setSince}>
          <SelectTrigger className="w-full lg:w-40" aria-label={t("logs.sinceAria")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {SINCE_OPTIONS.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {t(option.labelKey)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        <div className="flex items-center gap-2">
          <label htmlFor="logs-until" className="text-xs whitespace-nowrap text-muted-foreground">
            {t("logs.until")}
          </label>
          <Input
            id="logs-until"
            type="datetime-local"
            value={until}
            className="w-full font-mono text-xs lg:w-52"
            onChange={(event) => setUntil(event.target.value)}
          />
        </div>

        <Select value={limit} onValueChange={setLimit}>
          <SelectTrigger className="w-full lg:w-32" aria-label={t("logs.limitAria")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="100">{t("logs.limitN", { count: 100 })}</SelectItem>
            <SelectItem value="250">{t("logs.limitN", { count: 250 })}</SelectItem>
            <SelectItem value="500">{t("logs.limitN", { count: 500 })}</SelectItem>
          </SelectContent>
        </Select>

        <p className="text-xs text-muted-foreground lg:ml-auto" aria-live="polite">
          {logs.data ? t("logs.matched", { count: logs.data.count }) : "…"}
        </p>
      </div>

      <div className="rounded-md border" aria-busy={logs.loading || logs.refreshing}>
        {logs.loading && !logs.data ? (
          <div className="space-y-2 p-4">
            {Array.from({ length: 6 }).map((_, index) => (
              <Skeleton key={index} className="h-8 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={ScrollText}
            title={t("logs.emptyTitle")}
            description={t("logs.emptyDescription")}
            action={
              <Button
                variant="outline"
                onClick={() => {
                  setQ("");
                  setInstances([]);
                  setLevel("all");
                  setSince("7d");
                  setUntil("");
                }}
              >
                {t("common.clearFilters")}
              </Button>
            }
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[180px]">{t("audit.time")}</TableHead>
                    <TableHead className="w-[120px]">{t("messages.instance")}</TableHead>
                    <TableHead className="w-[90px]">{t("logs.level")}</TableHead>
                    <TableHead>{t("logs.message")}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((entry) => (
                    <TableRow key={entry.id}>
                      <TableCell>
                        <span
                          className="block font-mono text-xs"
                          title={formatRelative(entry.at, lang)}
                        >
                          {formatDateTime(entry.at)}
                        </span>
                      </TableCell>
                      <TableCell>
                        <a
                          href={`#/instances/${encodeURIComponent(entry.instance)}`}
                          className="font-mono text-xs underline-offset-4 hover:underline"
                        >
                          {entry.instance}
                        </a>
                      </TableCell>
                      <TableCell>
                        <OpsBadge status={entry.level} />
                      </TableCell>
                      <TableCell className="font-mono text-xs whitespace-pre-wrap">
                        <Highlight text={entry.message} needle={q} />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <ul className="divide-y md:hidden">
              {rows.map((entry) => (
                <li key={entry.id} className="space-y-1.5 p-3">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-[11px] text-muted-foreground">
                      {formatDateTime(entry.at)}
                    </span>
                    <a
                      href={`#/instances/${encodeURIComponent(entry.instance)}`}
                      className="font-mono text-xs"
                    >
                      {entry.instance}
                    </a>
                    <OpsBadge status={entry.level} className="ml-auto" />
                  </div>
                  <p className="font-mono text-xs break-words whitespace-pre-wrap">
                    <Highlight text={entry.message} needle={q} />
                  </p>
                </li>
              ))}
            </ul>
          </>
        )}
      </div>
    </>
  );
}
