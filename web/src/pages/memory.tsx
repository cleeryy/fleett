import { useEffect, useMemo, useState } from "react";
import { Brain, Plus, RefreshCw, Search, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { MemoryEntry } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import {
  MemoryDetailSheet,
  MemoryFormDialog,
  MemoryTagList,
  NamespaceBadge,
  memoryPreview,
} from "@/components/memory";
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

const ALL = "all";

export function MemoryPage() {
  const { t, lang } = useI18n();
  const canEdit = useCanEdit();
  const [q, setQ] = useState("");
  const [debouncedQ, setDebouncedQ] = useState("");
  const [tag, setTag] = useState("");
  const [debouncedTag, setDebouncedTag] = useState("");
  const [namespace, setNamespace] = useState(ALL);
  const [nonce, setNonce] = useState(0);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [detailOpen, setDetailOpen] = useState(false);
  const [formOpen, setFormOpen] = useState(false);
  const [editEntry, setEditEntry] = useState<MemoryEntry | null>(null);
  const [purgeOpen, setPurgeOpen] = useState(false);
  const [purging, setPurging] = useState(false);
  const [purgeError, setPurgeError] = useState<ApiError | null>(null);
  const [knownNamespaces, setKnownNamespaces] = useState<string[]>(["global", "shared"]);

  // One debounce for both fields: a tag is often typed after the query.
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setDebouncedQ(q);
      setDebouncedTag(tag);
    }, 300);
    return () => window.clearTimeout(timer);
  }, [q, tag]);

  const activeTags = useMemo(
    () =>
      debouncedTag
        .split(",")
        .map((entry) => entry.trim().toLowerCase())
        .filter(Boolean),
    [debouncedTag],
  );

  const instances = useResource(() => api.listInstances(), []);
  const entries = useResource(
    () =>
      debouncedQ.trim()
        ? api.searchMemory({
            q: debouncedQ,
            namespace: namespace === ALL ? undefined : namespace,
            tags: activeTags,
            limit: 200,
          })
        : api.listMemory({
            namespace: namespace === ALL ? undefined : namespace,
            tag: activeTags[0],
            limit: 200,
          }),
    [debouncedQ, namespace, activeTags, nonce],
  );

  useEffect(() => {
    const data = entries.data;
    if (!data) return;
    setKnownNamespaces((previous) => {
      const next = new Set(previous);
      for (const entry of data) next.add(entry.namespace);
      if (namespace !== ALL) next.add(namespace);
      if (next.size === previous.length) return previous;
      return [...next].sort((left, right) => {
        if (left === "global") return -1;
        if (right === "global") return 1;
        if (left === "shared") return -1;
        if (right === "shared") return 1;
        return left.localeCompare(right);
      });
    });
  }, [entries.data, namespace]);

  // The current backend accepts `tag` on the list endpoint but ignores it, so
  // the page filters the page it received as well. Search already ANDs tags.
  const rows = (entries.data ?? []).filter((entry) =>
    activeTags.every((candidate) => entry.tags.includes(candidate)),
  );
  const selected = selectedId === null ? null : (rows.find((entry) => entry.id === selectedId) ?? null);
  const filtering = debouncedQ.trim() !== "" || activeTags.length > 0 || namespace !== ALL;

  async function purge() {
    setPurging(true);
    setPurgeError(null);
    try {
      const result = await api.purgeExpiredMemory();
      toast.success(
        result.removed === 0
          ? t("memory.noExpired")
          : t(
              result.removed === 1 ? "memory.purgedOne" : "memory.purgedMany",
              { count: result.removed },
            ),
      );
      setPurgeOpen(false);
      setNonce((value) => value + 1);
    } catch (cause) {
      setPurgeError(toApiError(cause));
    } finally {
      setPurging(false);
    }
  }

  function open(entry: MemoryEntry) {
    setSelectedId(entry.id);
    setDetailOpen(true);
  }

  function clearFilters() {
    setQ("");
    setTag("");
    setDebouncedQ("");
    setDebouncedTag("");
    setNamespace(ALL);
  }

  const emptyState = filtering ? (
    <EmptyState
      icon={Search}
      title={t("memory.noMatchTitle")}
      description={t("memory.noMatchDescription")}
      action={
        <Button variant="outline" size="sm" onClick={clearFilters}>
          {t("common.clearFilters")}
        </Button>
      }
    />
  ) : (
    <EmptyState
      icon={Brain}
      title={t("memory.emptyTitle")}
      description={t("memory.emptyDescription")}
      action={
        canEdit ? (
          <Button
            size="sm"
            onClick={() => {
              setEditEntry(null);
              setFormOpen(true);
            }}
          >
            <Plus aria-hidden />
            {t("memory.newEntry")}
          </Button>
        ) : undefined
      }
    />
  );

  return (
    <>
      <PageHeader
        title={t("nav.memory")}
        description={
          entries.data
            ? t(rows.length === 1 ? "memory.countOne" : "memory.countMany", {
                count: rows.length,
              })
            : t("memory.loading")
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={entries.refreshing ? "animate-spin" : undefined} />
              {t("common.refresh")}
            </Button>
            {canEdit ? (
              <>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    setPurgeError(null);
                    setPurgeOpen(true);
                  }}
                >
                  <Trash2 aria-hidden />
                  {t("memory.purge")}
                </Button>
                <Button
                  size="sm"
                  onClick={() => {
                    setEditEntry(null);
                    setFormOpen(true);
                  }}
                >
                  <Plus aria-hidden />
                  {t("memory.newEntry")}
                </Button>
              </>
            ) : null}
          </>
        }
      />

      {entries.error ? <ErrorAlert error={entries.error} onRetry={entries.reload} /> : null}

      <section
        aria-label={t("memory.filtersAria")}
        className="grid gap-3 rounded-md border p-3 sm:grid-cols-2 lg:grid-cols-[minmax(0,1fr)_220px_200px]"
      >
        <div className="space-y-1.5 sm:col-span-2 lg:col-span-1">
          <Label htmlFor="memory-search" className="text-xs text-muted-foreground">
            {t("common.search")}
          </Label>
          <div className="relative">
            <Search
              aria-hidden
              className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground"
            />
            <Input
              id="memory-search"
              value={q}
              onChange={(event) => setQ(event.target.value)}
              placeholder={t("memory.searchPlaceholder")}
              className="pl-8"
              autoComplete="off"
              spellCheck={false}
            />
          </div>
        </div>

        <div className="space-y-1.5">
          <Label htmlFor="memory-namespace-filter" className="text-xs text-muted-foreground">
            {t("memory.namespace")}
          </Label>
          <Select value={namespace} onValueChange={setNamespace}>
            <SelectTrigger
              id="memory-namespace-filter"
              className="w-full"
              aria-label={t("memory.namespaceAria")}
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL}>{t("memory.allNamespaces")}</SelectItem>
              {knownNamespaces.map((value) => (
                <SelectItem key={value} value={value}>
                  <span className="font-mono">{value}</span>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="space-y-1.5">
          <Label htmlFor="memory-tag-filter" className="text-xs text-muted-foreground">
            {t("memory.tags")}
          </Label>
          <Input
            id="memory-tag-filter"
            value={tag}
            onChange={(event) => setTag(event.target.value)}
            placeholder={t("memory.tagsPlaceholder")}
            autoComplete="off"
            spellCheck={false}
            aria-describedby="memory-tag-hint"
          />
        </div>
      </section>

      <p id="memory-tag-hint" className="text-xs text-muted-foreground">
        {entries.refreshing ? t("memory.searching") : t("memory.tagsHint")}
        {filtering ? t("memory.filtersApplied") : ""}
      </p>

      <div className="rounded-md border" aria-busy={entries.loading || entries.refreshing}>
        {entries.loading && !entries.data ? (
          <>
            <div className="hidden space-y-3 p-4 md:block">
              {Array.from({ length: 5 }).map((_, index) => (
                <Skeleton key={index} className="h-12 w-full" />
              ))}
            </div>
            <div className="divide-y md:hidden">
              {Array.from({ length: 4 }).map((_, index) => (
                <div key={index} className="space-y-2 p-4">
                  <Skeleton className="h-4 w-48" />
                  <Skeleton className="h-4 w-24" />
                  <Skeleton className="h-3 w-full" />
                </div>
              ))}
            </div>
          </>
        ) : rows.length === 0 ? (
          emptyState
        ) : (
          <>
            <div className="hidden md:block">
              <Table className="table-fixed">
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[170px]">{t("personas.vars.key")}</TableHead>
                    <TableHead className="w-[150px]">{t("memory.namespace")}</TableHead>
                    <TableHead className="w-[55px]">{t("common.version")}</TableHead>
                    <TableHead className="w-[140px]">{t("memory.tags")}</TableHead>
                    <TableHead className="w-[150px]">{t("common.updated")}</TableHead>
                    <TableHead>{t("assistant.proposal.value")}</TableHead>
                    <TableHead className="w-[70px]">
                      <span className="sr-only">{t("common.actions")}</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((entry) => (
                    <TableRow
                      key={entry.id}
                      className="cursor-pointer"
                      onClick={() => open(entry)}
                    >
                      <TableCell>
                        <span className="block font-mono text-sm break-all">{entry.key}</span>
                        <span className="block text-xs text-muted-foreground">{entry.owner}</span>
                      </TableCell>
                      <TableCell>
                        <NamespaceBadge namespace={entry.namespace} />
                      </TableCell>
                      <TableCell>
                        <span className="font-mono text-xs">v{entry.version}</span>
                      </TableCell>
                      <TableCell>
                        <MemoryTagList tags={entry.tags} compact />
                      </TableCell>
                      <TableCell>
                        <span
                          className="font-mono text-xs"
                          title={formatRelative(entry.updated_at, lang)}
                        >
                          {formatDateTime(entry.updated_at)}
                        </span>
                      </TableCell>
                      <TableCell className="max-w-0">
                        <span className="block truncate text-sm text-muted-foreground">
                          {memoryPreview(entry.value, 140)}
                        </span>
                      </TableCell>
                      <TableCell className="text-right">
                        <Button
                          variant="outline"
                          size="sm"
                          aria-label={t("memory.openAria", { key: entry.key })}
                          onClick={(event) => {
                            event.stopPropagation();
                            open(entry);
                          }}
                        >
                          {t("common.open")}
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((entry) => (
                <button
                  key={entry.id}
                  type="button"
                  className="block w-full space-y-2 p-4 text-left transition-colors hover:bg-muted/40 focus-visible:bg-muted/40"
                  aria-label={t("memory.openAria", { key: entry.key })}
                  onClick={() => open(entry)}
                >
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <span className="font-mono text-sm break-all">{entry.key}</span>
                    <span className="font-mono text-[11px] text-muted-foreground">
                      v{entry.version}
                    </span>
                  </div>
                  <div className="flex flex-wrap items-center gap-1.5">
                    <NamespaceBadge namespace={entry.namespace} />
                    <MemoryTagList tags={entry.tags} />
                  </div>
                  <span className="block text-sm text-muted-foreground">
                    {memoryPreview(entry.value, 160)}
                  </span>
                  <span className="block font-mono text-[11px] text-muted-foreground">
                    {formatDateTime(entry.updated_at)}
                  </span>
                </button>
              ))}
            </div>
          </>
        )}
      </div>

      <MemoryDetailSheet
        entry={selected}
        open={detailOpen && selected !== null}
        onOpenChange={(open) => {
          setDetailOpen(open);
          if (!open) setSelectedId(null);
        }}
        canEdit={canEdit}
        onChanged={() => setNonce((value) => value + 1)}
        onEdit={(entry) => {
          setEditEntry(entry);
          setFormOpen(true);
        }}
      />

      <MemoryFormDialog
        open={formOpen}
        onOpenChange={(open) => {
          setFormOpen(open);
          if (!open) setEditEntry(null);
        }}
        instances={instances.data ?? []}
        entry={editEntry}
        onSaved={() => setNonce((value) => value + 1)}
      />

      <ConfirmDialog
        open={purgeOpen}
        onOpenChange={(open) => {
          if (!open) {
            setPurgeOpen(false);
            setPurgeError(null);
          }
        }}
        title={t("memory.purgeTitle")}
        description={t("memory.purgeDescription")}
        confirmLabel={t("memory.purge")}
        destructive
        pending={purging}
        error={purgeError}
        onConfirm={() => void purge()}
      />
    </>
  );
}
