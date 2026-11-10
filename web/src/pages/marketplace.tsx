import { useEffect, useMemo, useState, type FormEvent } from "react";
import {
  ArrowUpCircle,
  CloudDownload,
  Eye,
  Loader2,
  RefreshCw,
  Search,
  Store,
  TriangleAlert,
  X,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError } from "@/api/errors";
import type { MarketplaceEntry } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { MarketplacePreviewDialog } from "@/components/marketplace-preview-dialog";
import { PageHeader } from "@/components/page-header";
import { useI18n } from "@/lib/i18n";
import {
  refreshMarketplaceUpdates,
  useMarketplaceUpdates,
} from "@/lib/marketplace";
import { hrefFor } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

const PROVIDER_DOT: Record<string, string> = {
  local: "bg-muted-foreground",
  git: "bg-sky-500",
  github: "bg-foreground",
  "skills.sh": "bg-amber-500",
};

function MarketplaceCard({
  entry,
  onPreview,
}: {
  entry: MarketplaceEntry;
  onPreview: (entry: MarketplaceEntry) => void;
}) {
  const { t } = useI18n();
  return (
    <li className="flex min-w-0 flex-col gap-3 rounded-md border p-4 transition-colors focus-within:ring-[3px] focus-within:ring-ring/30 hover:border-foreground/20 hover:bg-muted/20">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate font-mono text-sm font-medium" title={entry.name}>
            {entry.name}
          </p>
          <p className="mt-1 line-clamp-2 min-h-10 text-sm text-muted-foreground">
            {entry.description || t("skills.picker.noDescription")}
          </p>
        </div>
        <Badge
          variant="outline"
          className="shrink-0 gap-1.5 font-mono text-[11px] font-normal"
          title={entry.source ? t("marketplace.card.sourceTitle", { source: entry.source }) : undefined}
        >
          <span
            aria-hidden
            className={cn("size-1.5 shrink-0 rounded-full", PROVIDER_DOT[entry.provider] ?? "bg-muted-foreground")}
          />
          {entry.provider}
        </Badge>
      </div>

      {entry.tags && entry.tags.length > 0 ? (
        <ul aria-label={t("marketplace.card.tagsAria", { name: entry.name })} className="flex flex-wrap gap-1">
          {entry.tags.map((tag) => (
            <li key={tag}>
              <Badge variant="secondary" className="font-mono text-[11px] font-normal">
                {tag}
              </Badge>
            </li>
          ))}
        </ul>
      ) : (
        <span className="min-h-5" aria-hidden />
      )}

      <div className="mt-auto flex flex-wrap items-end justify-between gap-2">
        <div className="min-w-0 space-y-0.5 text-[11px] text-muted-foreground">
          <p className="truncate">
            {entry.author
              ? t("marketplace.card.by", { author: entry.author })
              : t("marketplace.card.noAuthor")}
          </p>
          <p className="flex flex-wrap items-center gap-x-2 gap-y-0.5 font-mono">
            {entry.version ? <span>{t("marketplace.card.version", { version: entry.version })}</span> : null}
            {entry.commit ? (
              <span title={entry.commit}>
                {t("marketplace.card.commit", { commit: entry.commit.slice(0, 7) })}
              </span>
            ) : null}
          </p>
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => onPreview(entry)}
          aria-label={t("marketplace.card.previewAria", { name: entry.name })}
        >
          <Eye aria-hidden />
          {t("marketplace.card.preview")}
        </Button>
      </div>
    </li>
  );
}

export function MarketplacePage() {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [source, setSource] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  const [syncing, setSyncing] = useState(false);
  const [preview, setPreview] = useState<MarketplaceEntry | null>(null);
  const [previewOpen, setPreviewOpen] = useState(false);
  const search = useResource(
    () =>
      api.marketplaceSearch({
        ...(debounced ? { q: debounced } : {}),
        ...(source ? { source } : {}),
        limit: 60,
      }),
    [debounced, source, nonce],
  );
  const updates = useMarketplaceUpdates();

  useEffect(() => {
    const timer = window.setTimeout(() => setDebounced(query.trim()), 300);
    return () => window.clearTimeout(timer);
  }, [query]);

  const sources = search.data?.sources ?? [];
  const entries = search.data?.entries ?? [];
  const totalCount = sources.reduce((sum, item) => sum + item.count, 0);
  const degraded = sources.filter((item) => item.status !== "ok");
  const updateCount = updates.data?.updates.length ?? 0;
  const warnings = useMemo(() => updates.data?.warnings ?? [], [updates.data]);

  function openPreview(entry: MarketplaceEntry) {
    setPreview(entry);
    setPreviewOpen(true);
  }

  async function syncIndex() {
    setSyncing(true);
    try {
      const outcome = await api.marketplaceSync();
      toast.success(
        outcome.commit
          ? t("marketplace.syncDoneCommit", {
              skills: outcome.skills,
              source: outcome.source,
              commit: outcome.commit.slice(0, 7),
            })
          : t("marketplace.syncDone", { skills: outcome.skills, source: outcome.source }),
      );
      setNonce((value) => value + 1);
      refreshMarketplaceUpdates();
    } catch (cause) {
      toast.error(apiErrorMessage(toApiError(cause), t));
    } finally {
      setSyncing(false);
    }
  }

  function refresh() {
    setNonce((value) => value + 1);
    refreshMarketplaceUpdates();
  }

  const emptyState = (() => {
    if (search.loading && !search.data) return null;
    if (search.error && !search.data) return null;
    if (entries.length > 0) return null;
    if (source) {
      return {
        title: t("marketplace.emptySourceTitle", { source }),
        description: t("marketplace.emptySourceDescription"),
      };
    }
    if (sources.length === 0) {
      return {
        title: t("marketplace.emptyCatalogueTitle"),
        description: t("marketplace.emptyCatalogueDescription"),
      };
    }
    return {
      title: t("marketplace.emptyTitle"),
      description: t("marketplace.emptyDescription"),
    };
  })();

  return (
    <>
      <PageHeader
        title={t("nav.marketplace")}
        description={
          search.data
            ? t("marketplace.summary", {
                count: entries.length,
                sources: sources.length,
              })
            : t("marketplace.description")
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={refresh} disabled={search.refreshing}>
              <RefreshCw
                aria-hidden
                className={search.refreshing ? "animate-spin motion-reduce:animate-none" : undefined}
              />
              {t("common.refresh")}
            </Button>
            {canEdit ? (
              <Button variant="outline" size="sm" onClick={() => void syncIndex()} disabled={syncing}>
                {syncing ? (
                  <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
                ) : (
                  <CloudDownload aria-hidden />
                )}
                {syncing ? t("marketplace.syncing") : t("marketplace.sync")}
              </Button>
            ) : null}
          </>
        }
      />

      {!canEdit ? <p className="text-xs text-muted-foreground">{t("marketplace.readOnly")}</p> : null}

      {updateCount > 0 ? (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-md border border-sky-600/30 bg-sky-50/40 px-3 py-2 text-sm dark:bg-sky-950/20">
          <ArrowUpCircle aria-hidden className="size-4 shrink-0 text-sky-700 dark:text-sky-400" />
          <p className="min-w-0 flex-1">
            {t(updateCount === 1 ? "marketplace.updatesOne" : "marketplace.updatesMany", {
              count: updateCount,
            })}
          </p>
          <Button asChild variant="outline" size="sm">
            <a href={hrefFor("/skills")}>{t("marketplace.updatesAction")}</a>
          </Button>
        </div>
      ) : null}

      {warnings.map((warning) => (
        <p
          key={warning}
          role="status"
          className="flex items-start gap-2 rounded-md border px-3 py-2 text-xs text-muted-foreground"
        >
          <TriangleAlert aria-hidden className="mt-0.5 size-3.5 shrink-0 text-amber-600 dark:text-amber-400" />
          <span className="min-w-0 break-words">
            {t("marketplace.updatesWarning", { message: warning })}
          </span>
        </p>
      ))}

      {degraded.map((item) => (
        <div
          key={item.source}
          role="status"
          className="flex items-start gap-2 rounded-md border border-amber-600/40 bg-amber-50/40 px-3 py-2 text-sm dark:bg-amber-950/20"
        >
          <TriangleAlert
            aria-hidden
            className="mt-0.5 size-4 shrink-0 text-amber-700 dark:text-amber-400"
          />
          <p className="min-w-0 break-words text-muted-foreground">
            {item.message
              ? t(
                  item.status === "disabled"
                    ? "marketplace.sourceDisabled"
                    : "marketplace.sourceError",
                  { source: item.source, message: item.message },
                )
              : t("marketplace.sourceUnavailable", { source: item.source })}
          </p>
        </div>
      ))}

      <form
        role="search"
        onSubmit={(event: FormEvent<HTMLFormElement>) => event.preventDefault()}
        className="flex flex-col gap-3 lg:flex-row lg:items-center"
      >
        <div className="relative min-w-0 flex-1">
          <Search
            aria-hidden
            className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground"
          />
          <Input
            type="text"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t("marketplace.searchPlaceholder")}
            aria-label={t("marketplace.searchLabel")}
            autoComplete="off"
            spellCheck={false}
            className="pl-9 pr-9"
          />
          {query ? (
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              className="absolute top-1/2 right-1.5 -translate-y-1/2"
              onClick={() => setQuery("")}
              aria-label={t("marketplace.clearSearch")}
              title={t("marketplace.clearSearch")}
            >
              <X aria-hidden />
            </Button>
          ) : null}
        </div>

        {!search.data ? (
          <div aria-hidden className="flex flex-wrap items-center gap-1.5">
            {[84, 76, 92].map((width) => (
              <Skeleton key={width} className="h-7 rounded-full" style={{ width }} />
            ))}
          </div>
        ) : (
          <div
            role="group"
            aria-label={t("marketplace.sourcesLabel")}
            className="flex flex-wrap items-center gap-1.5"
          >
            <button
              type="button"
              aria-pressed={source === null}
              onClick={() => setSource(null)}
              className={cn(
                "inline-flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-xs outline-none transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50",
                source === null
                  ? "border-foreground/30 bg-foreground text-background"
                  : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
              )}
            >
              {t("marketplace.allSources")}
              <span className="font-mono tabular-nums opacity-70">{totalCount}</span>
            </button>
            {sources.map((item) => {
              const active = source === item.source;
              return (
                <button
                  key={item.source}
                  type="button"
                  aria-pressed={active}
                  onClick={() => setSource(item.source)}
                  className={cn(
                    "inline-flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-xs outline-none transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50",
                    active
                      ? "border-foreground/30 bg-foreground text-background"
                      : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                  )}
                >
                  <span
                    aria-hidden
                    className={cn(
                      "size-1.5 shrink-0 rounded-full",
                      active
                        ? "bg-background"
                        : item.status === "ok"
                          ? "bg-emerald-500"
                          : item.status === "error"
                            ? "bg-destructive"
                            : "bg-muted-foreground",
                    )}
                  />
                  <span className="font-mono">{item.source}</span>
                  <span className="font-mono tabular-nums opacity-70">{item.count}</span>
                  <span className="sr-only">
                    {t(
                      item.status === "ok"
                        ? "marketplace.sourceStatus.ok"
                        : item.status === "disabled"
                          ? "marketplace.sourceStatus.disabled"
                          : "marketplace.sourceStatus.error",
                    )}
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </form>

      <p aria-live="polite" className="text-xs text-muted-foreground">
        {search.loading && !search.data
          ? t("marketplace.loading")
          : t(entries.length === 1 ? "marketplace.resultOne" : "marketplace.resultMany", {
              count: entries.length,
            })}
      </p>

      {search.error ? <ErrorAlert error={search.error} onRetry={search.reload} /> : null}

      {search.loading && !search.data ? (
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {Array.from({ length: 6 }).map((_, index) => (
            <div key={index} className="space-y-3 rounded-md border p-4">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-3 w-24" />
              <Skeleton className="h-8 w-full" />
              <Skeleton className="h-4 w-32" />
            </div>
          ))}
        </div>
      ) : emptyState ? (
        <div className="rounded-md border">
          <EmptyState
            icon={Store}
            title={emptyState.title}
            description={emptyState.description}
            action={
              canEdit && !source ? (
                <Button variant="outline" onClick={() => void syncIndex()} disabled={syncing}>
                  <CloudDownload aria-hidden />
                  {t("marketplace.sync")}
                </Button>
              ) : undefined
            }
          />
        </div>
      ) : (
        <ul
          aria-label={t("marketplace.resultsAria")}
          aria-busy={search.refreshing}
          className={cn(
            "grid gap-3 sm:grid-cols-2 xl:grid-cols-3",
            search.refreshing && "opacity-70 transition-opacity",
          )}
        >
          {entries.map((entry) => (
            <MarketplaceCard key={`${entry.provider}:${entry.source ?? entry.name}`} entry={entry} onPreview={openPreview} />
          ))}
        </ul>
      )}

      <MarketplacePreviewDialog
        open={previewOpen}
        onOpenChange={setPreviewOpen}
        entry={preview}
        canEdit={canEdit}
        onInstalled={() => setNonce((value) => value + 1)}
      />
    </>
  );
}
