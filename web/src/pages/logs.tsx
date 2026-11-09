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
import { OpsBadge } from "@/components/ops-badge";
import { PageHeader } from "@/components/page-header";
import { formatDateTime, formatRelative, plural } from "@/lib/format";
import { useResource } from "@/lib/use-resource";

const SINCE_OPTIONS = [
  { value: "all", label: "Any time" },
  { value: "30m", label: "Last 30 min" },
  { value: "1h", label: "Last hour" },
  { value: "24h", label: "Last 24 h" },
  { value: "7d", label: "Last 7 days" },
];

function Highlight({ text, needle }: { text: string; needle: string }) {
  const query = needle.trim();
  if (!query) return <>{text}</>;
  const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const parts = text.split(new RegExp(`(${escaped})`, "gi"));
  return (
    <>
      {parts.map((part, index) =>
        part.toLowerCase() === query.toLowerCase() ? (
          <mark
            key={`${part}-${index}`}
            className="rounded-[2px] bg-amber-200 px-0.5 text-amber-950 dark:bg-amber-500/40 dark:text-amber-50"
          >
            {part}
          </mark>
        ) : (
          <span key={`${part}-${index}`}>{part}</span>
        ),
      )}
    </>
  );
}

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
      ? "All instances"
      : `${instances.length} ${plural(instances.length, "instance")}`;

  return (
    <>
      <PageHeader
        title="Logs"
        description={
          logs.data
            ? `${rows.length} ${plural(rows.length, "line")} · newest first · redacted on ingestion`
            : "Searching the persisted log center."
        }
        actions={
          <>
            <Button
              variant={follow ? "default" : "outline"}
              size="sm"
              aria-pressed={follow}
              title="Poll the latest lines every 2 seconds. SSE is not used because EventSource cannot send the bearer header."
              onClick={() => setFollow((value) => !value)}
            >
              {follow ? (
                <Loader2 aria-hidden className="animate-spin" />
              ) : (
                <Activity aria-hidden />
              )}
              {follow ? "Following" : "Follow"}
            </Button>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={logs.refreshing ? "animate-spin" : undefined} />
              Refresh
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="sm" disabled={exporting !== null}>
                  {exporting ? <Loader2 aria-hidden className="animate-spin" /> : <Download aria-hidden />}
                  Export
                  <ChevronDown aria-hidden />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onSelect={() => void download("json")}>Export JSON</DropdownMenuItem>
                <DropdownMenuItem onSelect={() => void download("csv")}>Export CSV</DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </>
        }
      />

      {follow ? (
        <p className="text-xs text-muted-foreground" role="status">
          Live follow polls every 2 seconds (latest cursor). The SSE stream exists, but{" "}
          <span className="font-mono">EventSource</span> cannot send the bearer header, so this console
          deliberately uses the search endpoint instead.
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
            placeholder="Search message text"
            aria-label="Search log messages"
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
            <DropdownMenuLabel>Instances</DropdownMenuLabel>
            <DropdownMenuCheckboxItem
              checked={instances.length === 0}
              onSelect={(event) => event.preventDefault()}
              onCheckedChange={() => setInstances([])}
            >
              All instances
            </DropdownMenuCheckboxItem>
            <DropdownMenuSeparator />
            {instanceOptions.length === 0 ? (
              <p className="px-2 py-1.5 text-xs text-muted-foreground">No instance reported yet.</p>
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
          <SelectTrigger className="w-full lg:w-36" aria-label="Minimum level">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All levels</SelectItem>
            <SelectItem value="info">info and up</SelectItem>
            <SelectItem value="warn">warn and up</SelectItem>
            <SelectItem value="error">error only</SelectItem>
          </SelectContent>
        </Select>

        <Select value={since} onValueChange={setSince}>
          <SelectTrigger className="w-full lg:w-40" aria-label="Since when">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {SINCE_OPTIONS.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        <div className="flex items-center gap-2">
          <label htmlFor="logs-until" className="text-xs whitespace-nowrap text-muted-foreground">
            Until
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
          <SelectTrigger className="w-full lg:w-32" aria-label="Lines to load">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="100">100 lines</SelectItem>
            <SelectItem value="250">250 lines</SelectItem>
            <SelectItem value="500">500 lines</SelectItem>
          </SelectContent>
        </Select>

        <p className="text-xs text-muted-foreground lg:ml-auto" aria-live="polite">
          {logs.data ? `${logs.data.count} matched` : "…"}
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
            title="No log line matches"
            description="Widen the time window, clear the query, or pick another level. The log center keeps a bounded retention window."
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
                Clear filters
              </Button>
            }
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[180px]">Time</TableHead>
                    <TableHead className="w-[120px]">Instance</TableHead>
                    <TableHead className="w-[90px]">Level</TableHead>
                    <TableHead>Message</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((entry) => (
                    <TableRow key={entry.id}>
                      <TableCell>
                        <span className="block font-mono text-xs" title={formatRelative(entry.at)}>
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
