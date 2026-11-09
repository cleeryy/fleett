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
import { formatDateTime, formatRelative, plural } from "@/lib/format";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

const ALL = "all";

export function MemoryPage() {
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
          ? "No expired entry to remove."
          : `${result.removed} expired ${plural(result.removed, "entry", "entries")} removed.`,
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
      title="No entry matches"
      description="Try fewer words, another namespace or remove the tag filter. Search covers keys, values and tags."
      action={
        <Button variant="outline" size="sm" onClick={clearFilters}>
          Clear filters
        </Button>
      }
    />
  ) : (
    <EmptyState
      icon={Brain}
      title="The fleet memory is empty"
      description="Store a durable lesson, a contract or a runbook. Every agent can read it; entries are searchable by key, value and tags."
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
            New entry
          </Button>
        ) : undefined
      }
    />
  );

  return (
    <>
      <PageHeader
        title="Memory"
        description={
          entries.data
            ? `${rows.length} ${plural(rows.length, "entry", "entries")} · searchable by every agent · plaintext, never secrets`
            : "Loading fleet memory."
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={entries.refreshing ? "animate-spin" : undefined} />
              Refresh
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
                  Purge expired
                </Button>
                <Button
                  size="sm"
                  onClick={() => {
                    setEditEntry(null);
                    setFormOpen(true);
                  }}
                >
                  <Plus aria-hidden />
                  New entry
                </Button>
              </>
            ) : null}
          </>
        }
      />

      {entries.error ? <ErrorAlert error={entries.error} onRetry={entries.reload} /> : null}

      <section
        aria-label="Memory filters"
        className="grid gap-3 rounded-md border p-3 sm:grid-cols-2 lg:grid-cols-[minmax(0,1fr)_220px_200px]"
      >
        <div className="space-y-1.5 sm:col-span-2 lg:col-span-1">
          <Label htmlFor="memory-search" className="text-xs text-muted-foreground">
            Search
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
              placeholder="Search keys, values and tags"
              className="pl-8"
              autoComplete="off"
              spellCheck={false}
            />
          </div>
        </div>

        <div className="space-y-1.5">
          <Label htmlFor="memory-namespace-filter" className="text-xs text-muted-foreground">
            Namespace
          </Label>
          <Select value={namespace} onValueChange={setNamespace}>
            <SelectTrigger
              id="memory-namespace-filter"
              className="w-full"
              aria-label="Filter by namespace"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL}>All namespaces</SelectItem>
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
            Tags
          </Label>
          <Input
            id="memory-tag-filter"
            value={tag}
            onChange={(event) => setTag(event.target.value)}
            placeholder="ops, backup"
            autoComplete="off"
            spellCheck={false}
            aria-describedby="memory-tag-hint"
          />
        </div>
      </section>

      <p id="memory-tag-hint" className="text-xs text-muted-foreground">
        {entries.refreshing ? "Searching…" : "Tags are comma-separated and all must match."}
        {filtering ? " Filters applied." : ""}
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
                    <TableHead className="w-[170px]">Key</TableHead>
                    <TableHead className="w-[150px]">Namespace</TableHead>
                    <TableHead className="w-[55px]">Version</TableHead>
                    <TableHead className="w-[140px]">Tags</TableHead>
                    <TableHead className="w-[150px]">Updated</TableHead>
                    <TableHead>Value</TableHead>
                    <TableHead className="w-[70px]">
                      <span className="sr-only">Actions</span>
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
                          title={formatRelative(entry.updated_at)}
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
                          aria-label={`Open memory entry ${entry.key}`}
                          onClick={(event) => {
                            event.stopPropagation();
                            open(entry);
                          }}
                        >
                          Open
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
                  aria-label={`Open memory entry ${entry.key}`}
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
        title="Purge expired memory?"
        description={
          <>
            Removes every entry whose expiry has passed, with its history and search index. Entries
            that never expire are untouched.
          </>
        }
        confirmLabel="Purge expired"
        destructive
        pending={purging}
        error={purgeError}
        onConfirm={() => void purge()}
      />
    </>
  );
}
