import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDown,
  ChevronDown,
  Download,
  Eraser,
  FlaskConical,
  Loader2,
  ScrollText,
  Search,
  TriangleAlert,
} from "lucide-react";
import { api, isMock } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
import { burstMockLogs, mockConfig, type MockLogTransport } from "@/api/mock";
import type { LogEntry } from "@/api/types";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
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
import { CopyButton } from "@/components/copy-button";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { Highlight } from "@/components/highlight";
import { saveBlob } from "@/lib/download";
import { formatDateTime } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import {
  logClock,
  logDay,
  logsToText,
  matchesLogFilter,
  type LogFilterLevel,
} from "@/lib/logs";
import { useLiveLogs, type LiveLogStatus } from "@/lib/use-live-logs";
import { cn } from "@/lib/utils";

const LEVEL_OPTIONS: Array<{ value: LogFilterLevel; labelKey: string }> = [
  { value: "debug", labelKey: "logs.live.levelDebug" },
  { value: "info", labelKey: "logs.live.levelInfo" },
  { value: "warn", labelKey: "logs.live.levelWarn" },
  { value: "error", labelKey: "logs.live.levelError" },
];

const STATUS_STYLES: Record<
  LiveLogStatus,
  { dot: string; text: string; labelKey: string; hintKey: string }
> = {
  live: {
    dot: "bg-emerald-500 animate-pulse motion-reduce:animate-none",
    text: "text-emerald-700 dark:text-emerald-400",
    labelKey: "logs.live.status.live",
    hintKey: "logs.live.liveHint",
  },
  paused: {
    dot: "bg-muted-foreground",
    text: "text-muted-foreground",
    labelKey: "logs.live.status.paused",
    hintKey: "logs.live.pausedHint",
  },
  reconnecting: {
    dot: "bg-amber-500 animate-pulse motion-reduce:animate-none",
    text: "text-amber-700 dark:text-amber-400",
    labelKey: "logs.live.status.reconnecting",
    hintKey: "logs.live.reconnectingHint",
  },
  error: {
    dot: "bg-destructive",
    text: "text-destructive",
    labelKey: "logs.live.status.error",
    hintKey: "logs.live.errorHint",
  },
};

const LEVEL_CLASS: Record<LogEntry["level"], string> = {
  info: "text-muted-foreground",
  warn: "text-amber-700 dark:text-amber-400",
  error: "text-destructive",
};

function LiveStatus({ status }: { status: LiveLogStatus }) {
  const { t } = useI18n();
  const style = STATUS_STYLES[status];
  return (
    <span
      role="status"
      aria-live="polite"
      title={t(style.hintKey)}
      className="inline-flex items-center gap-1.5 text-xs font-medium"
    >
      <span aria-hidden className={cn("size-1.5 shrink-0 rounded-full", style.dot)} />
      <span className={style.text}>{t(style.labelKey)}</span>
    </span>
  );
}

/**
 * Live log console for one instance.
 *
 * The stream is read with `fetch` (the bearer header cannot travel through
 * `EventSource`), coalesced per animation frame, capped to 2 000 lines and
 * kept out of the layout engine with `content-visibility`. Follow mode pins
 * the viewport to the newest line; scrolling up pauses it, and both the pill
 * and a manual scroll back to the bottom resume it.
 */
export function InstanceLogs({ id }: { id: string }) {
  const { t, lang } = useI18n();
  const feed = useLiveLogs(id);

  const [query, setQuery] = useState("");
  const [level, setLevel] = useState<LogFilterLevel>("debug");
  const [following, setFollowing] = useState(true);
  const [pendingCount, setPendingCount] = useState(0);
  const [exporting, setExporting] = useState<"json" | "csv" | null>(null);
  const [exportError, setExportError] = useState<ApiError | null>(null);
  const [mockTransport, setMockTransport] = useState<MockLogTransport>(
    mockConfig.logTransport,
  );

  const viewportRef = useRef<HTMLDivElement>(null);
  const pauseMarkRef = useRef(0);
  const anchorRef = useRef<{ id: number; offset: number } | null>(null);
  const restoringRef = useRef(false);
  const pauseRef = useRef<() => void>(() => undefined);

  const displayed = useMemo(
    () => feed.lines.filter((entry) => matchesLogFilter(entry, query, level)),
    [feed.lines, query, level],
  );
  const filtering = query.trim().length > 0 || level !== "debug";
  const numberFormat = useMemo(() => new Intl.NumberFormat(lang), [lang]);

  const status: LiveLogStatus =
    feed.status === "error"
      ? "error"
      : feed.status === "paused" || !following
        ? "paused"
        : feed.status;

  // New lines keep arriving while the reader is scrolled up; count them so the
  // pill can promise something concrete.
  useEffect(() => {
    if (following) {
      setPendingCount(0);
      return;
    }
    setPendingCount(Math.max(0, feed.received - pauseMarkRef.current));
  }, [feed.received, following]);

  useLayoutEffect(() => {
    const node = viewportRef.current;
    if (!node) return;
    if (following) {
      anchorRef.current = null;
      node.scrollTop = node.scrollHeight;
      return;
    }
    const anchor = anchorRef.current;
    if (!anchor) return;
    const row = node.querySelector<HTMLElement>(`[data-log-id="${anchor.id}"]`);
    if (!row) return;
    const delta =
      row.getBoundingClientRect().top - node.getBoundingClientRect().top - anchor.offset;
    if (Math.abs(delta) > 0.5) {
      restoringRef.current = true;
      node.scrollTop += delta;
      window.requestAnimationFrame(() => {
        restoringRef.current = false;
      });
    }
  }, [displayed, following]);

  useEffect(() => {
    const onSelectionChange = () => {
      const node = viewportRef.current;
      const selection = document.getSelection();
      if (!node || !selection || selection.isCollapsed || selection.rangeCount === 0) return;
      if (!node.contains(selection.getRangeAt(0).commonAncestorContainer)) return;
      pauseRef.current();
    };
    document.addEventListener("selectionchange", onSelectionChange);
    return () => document.removeEventListener("selectionchange", onSelectionChange);
  }, []);

  function captureAnchor() {
    const node = viewportRef.current;
    if (!node) return;
    // At the very top there is nothing above to keep in place, and the first
    // element is the date divider rather than a line.
    if (node.scrollTop <= 1) {
      anchorRef.current = null;
      return;
    }
    const rect = node.getBoundingClientRect();
    const element = document.elementFromPoint(rect.left + 24, rect.top + 8);
    const row = element instanceof HTMLElement ? element.closest<HTMLElement>("[data-log-id]") : null;
    if (!row || !node.contains(row)) return;
    const rowId = Number(row.dataset.logId);
    if (!Number.isFinite(rowId)) return;
    anchorRef.current = {
      id: rowId,
      offset: row.getBoundingClientRect().top - rect.top,
    };
  }

  function pause() {
    if (!following) return;
    captureAnchor();
    pauseMarkRef.current = feed.received;
    setPendingCount(0);
    setFollowing(false);
  }
  useEffect(() => {
    pauseRef.current = pause;
  });

  function resume() {
    anchorRef.current = null;
    pauseMarkRef.current = feed.received;
    setPendingCount(0);
    setFollowing(true);
  }

  function onScroll() {
    if (restoringRef.current) return;
    const node = viewportRef.current;
    if (!node) return;
    const distance = node.scrollHeight - node.scrollTop - node.clientHeight;
    if (distance <= 24) {
      if (!following) resume();
      return;
    }
    if (following) {
      pause();
    } else {
      captureAnchor();
    }
  }

  async function download(format: "json" | "csv") {
    setExporting(format);
    setExportError(null);
    try {
      const { blob, filename } = await api.exportLogs(
        {
          instances: [id],
          ...(query.trim() ? { q: query.trim() } : {}),
          ...(level === "debug" ? {} : { level }),
          limit: 500,
        },
        format,
      );
      saveBlob(blob, filename);
    } catch (cause) {
      setExportError(toApiError(cause));
    } finally {
      setExporting(null);
    }
  }

  function applyMockTransport(next: MockLogTransport) {
    mockConfig.logTransport = next;
    setMockTransport(next);
    feed.retry();
  }

  const countLabel = filtering
    ? t("logs.live.countFiltered", {
        shown: numberFormat.format(displayed.length),
        total: numberFormat.format(feed.lines.length),
      })
    : feed.lines.length === 1
      ? t("logs.live.countOne")
      : t("logs.live.countMany", { count: numberFormat.format(feed.lines.length) });

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <LiveStatus status={status} />
        <span className="font-mono text-xs tabular-nums text-muted-foreground">{countLabel}</span>
        {feed.status === "reconnecting" && !feed.loading ? (
          <span
            role="status"
            className="inline-flex items-center gap-1.5 text-xs text-muted-foreground"
          >
            <Loader2 aria-hidden className="size-3 animate-spin" />
            {t("logs.live.polling")}
          </span>
        ) : null}
      </div>

      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
        <div className="flex w-full min-w-0 items-center gap-2 sm:w-auto sm:flex-1">
          <div className="relative min-w-0 flex-1 sm:max-w-xs">
            <Search
              aria-hidden
              className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground"
            />
            <Input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t("logs.live.searchPlaceholder")}
              aria-label={t("logs.live.searchAria")}
              className="pl-8"
            />
          </div>
          <Select value={level} onValueChange={(value) => setLevel(value as LogFilterLevel)}>
            <SelectTrigger
              className="w-36 shrink-0 sm:w-40"
              aria-label={t("logs.live.levelAria")}
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {LEVEL_OPTIONS.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {t(option.labelKey)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="flex w-full flex-wrap items-center gap-2 sm:ml-auto sm:w-auto">
          <CopyButton
            value={() => logsToText(displayed)}
            label={t("logs.live.copy")}
            showLabel
            size="sm"
            variant="outline"
            disabled={displayed.length === 0}
          />
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="outline"
                size="sm"
                disabled={exporting !== null}
                aria-label={t("logs.export")}
              >
                {exporting ? (
                  <Loader2 aria-hidden className="animate-spin" />
                ) : (
                  <Download aria-hidden />
                )}
                <span className="hidden sm:inline">{t("logs.export")}</span>
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
          <Button
            variant="outline"
            size="sm"
            onClick={feed.clear}
            disabled={feed.lines.length === 0}
            aria-label={t("logs.live.clear")}
            title={t("logs.live.clearHint")}
          >
            <Eraser aria-hidden />
            <span className="hidden sm:inline">{t("logs.live.clear")}</span>
          </Button>
        </div>
      </div>

      {exportError ? <ErrorAlert error={exportError} /> : null}

      {isMock ? (
        <section
          aria-label={t("mock.logs.title")}
          className="flex flex-wrap items-center gap-2 rounded-md border border-dashed p-3"
        >
          <FlaskConical aria-hidden className="size-4 text-muted-foreground" />
          <span className="text-xs font-medium">{t("mock.logs.title")}</span>
          <Button
            type="button"
            size="xs"
            variant="outline"
            onClick={() => burstMockLogs(id, 5_000)}
          >
            {t("mock.logs.burst")}
          </Button>
          <Button
            type="button"
            size="xs"
            variant="outline"
            onClick={() => burstMockLogs(id, 200)}
          >
            {t("mock.logs.burstSmall")}
          </Button>
          <Select
            value={mockTransport}
            onValueChange={(value) => applyMockTransport(value as MockLogTransport)}
          >
            <SelectTrigger
              className="h-7 w-40"
              aria-label={t("mock.logs.transportAria")}
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="live">{t("mock.logs.transportLive")}</SelectItem>
              <SelectItem value="poll">{t("mock.logs.transportPoll")}</SelectItem>
              <SelectItem value="down">{t("mock.logs.transportDown")}</SelectItem>
            </SelectContent>
          </Select>
          <p className="w-full text-[11px] text-muted-foreground">
            {t("mock.logs.description")}
          </p>
        </section>
      ) : null}

      {feed.error && feed.status === "error" && feed.lines.length > 0 ? (
        <div
          role="status"
          className="flex flex-wrap items-center gap-2 text-xs text-destructive"
        >
          <TriangleAlert aria-hidden className="size-3.5 shrink-0" />
          <span className="min-w-0 break-words">{apiErrorMessage(feed.error, t)}</span>
          <Button type="button" size="xs" variant="outline" onClick={feed.retry}>
            {t("common.retry")}
          </Button>
        </div>
      ) : null}

      {feed.error && feed.status === "error" && feed.lines.length === 0 ? (
        <ErrorAlert error={feed.error} onRetry={feed.retry} />
      ) : (
        <div className="relative">
          <div
            ref={viewportRef}
            onScroll={onScroll}
            tabIndex={0}
            role="region"
            aria-label={t("logs.live.ariaLabel")}
            aria-busy={feed.loading}
            className="h-[min(60vh,34rem)] min-h-64 overflow-auto overscroll-contain rounded-md border bg-muted/20 font-mono text-xs"
          >
            {feed.loading ? (
              <div className="space-y-2 p-4" aria-hidden>
                {Array.from({ length: 8 }).map((_, index) => (
                  <Skeleton
                    key={index}
                    className="h-4"
                    style={{ width: `${94 - (index % 5) * 13}%` }}
                  />
                ))}
              </div>
            ) : displayed.length === 0 ? (
              <div className="flex h-full items-center justify-center">
                <EmptyState
                  icon={ScrollText}
                  title={
                    feed.lines.length === 0
                      ? t("logs.live.emptyTitle")
                      : t("logs.live.noMatchTitle")
                  }
                  description={
                    feed.lines.length === 0
                      ? t("logs.live.emptyDescription")
                      : t("logs.live.noMatchDescription")
                  }
                  action={
                    feed.lines.length === 0 ? undefined : (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => {
                          setQuery("");
                          setLevel("debug");
                        }}
                      >
                        {t("common.clearFilters")}
                      </Button>
                    )
                  }
                />
              </div>
            ) : (
              <ol className="m-0 list-none p-0" aria-label={t("logs.live.linesAria")}>
                {displayed.map((entry, index) => {
                  const day = logDay(entry.at);
                  const previousDay = index > 0 ? logDay(displayed[index - 1].at) : null;
                  return (
                    <Fragment key={entry.id}>
                      {day !== previousDay ? (
                        <li className="flex items-center gap-2 px-3 pt-3 pb-1 text-[10px] tracking-wide text-muted-foreground uppercase">
                          <span className="font-mono">{day}</span>
                          <span aria-hidden className="h-px flex-1 bg-border" />
                        </li>
                      ) : null}
                      <li
                        data-log-id={entry.id}
                        className="flex gap-2 px-3 py-1 leading-5 [contain-intrinsic-size:auto_1.75rem] [content-visibility:auto] animate-in fade-in-0 duration-150 motion-reduce:animate-none sm:hover:bg-muted/40"
                      >
                        <time
                          dateTime={entry.at}
                          title={formatDateTime(entry.at)}
                          className="w-[4.1rem] shrink-0 text-[11px] tabular-nums text-muted-foreground/80"
                        >
                          {logClock(entry.at)}
                        </time>
                        <span
                          className={cn(
                            "w-10 shrink-0 pt-px text-[10px] font-medium tracking-wide uppercase",
                            LEVEL_CLASS[entry.level],
                          )}
                        >
                          {entry.level}
                        </span>
                        <span className="min-w-0 flex-1 break-words whitespace-pre-wrap">
                          <Highlight text={entry.message} needle={query} />
                        </span>
                      </li>
                    </Fragment>
                  );
                })}
              </ol>
            )}

            {feed.loading ? (
              <span className="sr-only" role="status">
                {t("logs.live.loading")}
              </span>
            ) : null}
          </div>

          {!following && pendingCount > 0 ? (
            <div className="pointer-events-none absolute inset-x-0 bottom-3 z-10 flex justify-center px-3">
              <Button
                type="button"
                size="sm"
                onClick={resume}
                className="pointer-events-auto animate-in fade-in-0 slide-in-from-bottom-1 shadow-lg motion-reduce:animate-none"
              >
                <ArrowDown aria-hidden />
                {pendingCount === 1
                  ? t("logs.live.newLineOne", { count: pendingCount })
                  : t("logs.live.newLinesMany", { count: pendingCount })}
              </Button>
            </div>
          ) : null}
        </div>
      )}

      <p className="text-[11px] text-muted-foreground">
        {t("logs.live.footer", { count: numberFormat.format(2_000) })}
      </p>
    </div>
  );
}
