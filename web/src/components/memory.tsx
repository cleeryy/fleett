import { useEffect, useMemo, useState, type FormEvent } from "react";
import {
  ArrowUpRight,
  Loader2,
  Pencil,
  RotateCcw,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { InstanceSummary, MemoryEntry, MemoryVersion } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DataList } from "@/components/data-list";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { CopyButton } from "@/components/copy-button";
import { ErrorAlert } from "@/components/error-alert";
import { RichText } from "@/components/rich-text";
import { formatDateTime, formatRelative } from "@/lib/format";
import { hasTranslation, useI18n } from "@/lib/i18n";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

/** `docs/MEMORY.md`: 64 KiB per value, counted in bytes. */
export const MEMORY_VALUE_LIMIT = 64 * 1024;

const encoder = new TextEncoder();

export function memoryBytes(value: string): number {
  return encoder.encode(value).length;
}

export function memoryPreview(value: string, max = 180): string {
  const flat = value.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

const NAMESPACE_STYLES = {
  global: "border-primary/30 text-foreground",
  shared: "border-sky-600/30 text-sky-700 dark:text-sky-400",
  instance: "text-muted-foreground",
  project: "border-emerald-600/30 text-emerald-700 dark:text-emerald-400",
};

function namespaceFamily(namespace: string): keyof typeof NAMESPACE_STYLES {
  if (namespace === "global") return "global";
  if (namespace === "shared") return "shared";
  if (namespace.startsWith("instance:")) return "instance";
  if (namespace.startsWith("project:")) return "project";
  return "instance";
}

export function NamespaceBadge({ namespace }: { namespace: string }) {
  return (
    <Badge
      variant="outline"
      className={cn(
        "font-mono text-xs font-normal",
        NAMESPACE_STYLES[namespaceFamily(namespace)],
      )}
    >
      {namespace}
    </Badge>
  );
}

/**
 * Dense table form of a tag list: the first two tags plus a count, so a row
 * stays one line. The detail view and mobile cards show every tag.
 */
export function MemoryTagList({
  tags,
  compact = false,
}: {
  tags: string[];
  compact?: boolean;
}) {
  if (tags.length === 0) {
    return <span className="text-xs text-muted-foreground">—</span>;
  }
  const label =
    compact && tags.length > 2
      ? `${tags.slice(0, 2).join(" · ")} +${tags.length - 2}`
      : tags.join(" · ");
  return (
    <span
      className={
        compact
          ? "block truncate font-mono text-xs text-muted-foreground"
          : "block font-mono text-xs break-words text-muted-foreground"
      }
      title={tags.join(", ")}
    >
      {label}
    </span>
  );
}

const ACTION_STYLES: Record<string, string> = {
  create: "border-emerald-600/30 text-emerald-700 dark:text-emerald-400",
  update: "border-sky-600/30 text-sky-700 dark:text-sky-400",
  rollback: "border-amber-600/30 text-amber-700 dark:text-amber-400",
  delete: "border-destructive/40 text-destructive",
};

function ActionBadge({ action }: { action: string }) {
  const { t } = useI18n();
  const key = `memoryAction.${action}`;
  return (
    <Badge
      variant="outline"
      className={cn("font-mono text-[11px] font-normal", ACTION_STYLES[action] ?? "")}
    >
      {hasTranslation(key) ? t(key) : action}
    </Badge>
  );
}

function When({ iso }: { iso: string }) {
  const { lang } = useI18n();
  return (
    <span className="font-mono text-xs" title={formatRelative(iso, lang)}>
      {formatDateTime(iso)}
    </span>
  );
}

const TTL_OPTIONS = [
  { value: "3600", labelKey: "memory.ttl1h" },
  { value: "86400", labelKey: "memory.ttl1d" },
  { value: "604800", labelKey: "memory.ttl7d" },
  { value: "2592000", labelKey: "memory.ttl30d" },
  { value: "31536000", labelKey: "memory.ttl1y" },
];

function parseTags(text: string): string[] {
  return text
    .split(",")
    .map((tag) => tag.trim())
    .filter(Boolean);
}

type NamespaceKind = "global" | "shared" | "instance" | "project";

function splitNamespace(namespace: string): { kind: NamespaceKind; scope: string } {
  if (namespace === "global") return { kind: "global", scope: "" };
  if (namespace === "shared") return { kind: "shared", scope: "" };
  if (namespace.startsWith("instance:")) {
    return { kind: "instance", scope: namespace.slice("instance:".length) };
  }
  if (namespace.startsWith("project:")) {
    return { kind: "project", scope: namespace.slice("project:".length) };
  }
  return { kind: "shared", scope: "" };
}

/**
 * Create an entry, or edit one: `(namespace, key)` is the identity of a memory
 * row, so both fields are fixed when `entry` is set.
 */
export function MemoryFormDialog({
  open,
  onOpenChange,
  instances,
  entry,
  defaultNamespace,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  instances: InstanceSummary[];
  entry?: MemoryEntry | null;
  /** Preselected namespace for a new entry (the instance tab). */
  defaultNamespace?: string;
  onSaved: (entry: MemoryEntry) => void;
}) {
  const { t } = useI18n();
  const editing = entry !== null && entry !== undefined;
  const ordered = useMemo(
    () => [...instances].sort((left, right) => left.id.localeCompare(right.id)),
    [instances],
  );

  const [kind, setKind] = useState<NamespaceKind>("shared");
  const [scope, setScope] = useState("");
  const [key, setKey] = useState("");
  const [value, setValue] = useState("");
  const [tags, setTags] = useState("");
  const [ttl, setTtl] = useState("none");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [fieldErrors, setFieldErrors] = useState<{ key?: string; value?: string; scope?: string }>({});

  useEffect(() => {
    if (!open) return;
    const initial = entry
      ? splitNamespace(entry.namespace)
      : defaultNamespace
        ? splitNamespace(defaultNamespace)
        : { kind: "shared" as NamespaceKind, scope: "" };
    setKind(initial.kind);
    setScope(initial.scope);
    setKey(entry?.key ?? "");
    setValue(entry?.value ?? "");
    setTags(entry ? entry.tags.join(", ") : "");
    setTtl("none");
    setPending(false);
    setError(null);
    setFieldErrors({});
  }, [open, entry, defaultNamespace]);

  const namespace =
    kind === "global" || kind === "shared"
      ? kind
      : `${kind}:${scope.trim()}`;
  const bytes = memoryBytes(value);
  const overLimit = bytes > MEMORY_VALUE_LIMIT;

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const nextErrors: { key?: string; value?: string; scope?: string } = {};
    if (!editing && !key.trim()) nextErrors.key = t("memory.errors.keyRequired");
    if (!editing && !scope.trim() && (kind === "instance" || kind === "project")) {
      nextErrors.scope =
        kind === "instance" ? t("memory.errors.instanceRequired") : t("memory.errors.projectRequired");
    }
    if (overLimit) nextErrors.value = t("memory.errors.valueTooLarge");
    setFieldErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) return;

    setPending(true);
    setError(null);
    try {
      const nextTags = parseTags(tags);
      if (entry) {
        // POST /api/memory upserts by (namespace, key): the same operation as
        // PUT /api/memory/{id}, and available on every controller version.
        const result = await api.createOrUpdateMemory({
          namespace: entry.namespace,
          key: entry.key,
          value,
          tags: nextTags,
          ttl_s: ttl === "none" ? undefined : Number(ttl),
        });
        toast.success(t("memory.updated"));
        onSaved(result.entry);
      } else {
        const result = await api.createOrUpdateMemory({
          namespace,
          key,
          value,
          tags: nextTags,
          ttl_s: ttl === "none" ? undefined : Number(ttl),
        });
        toast.success(
          result.created ? t("memory.created") : t("memory.upserted"),
        );
        onSaved(result.entry);
      }
      onOpenChange(false);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-lg">
        <form onSubmit={onSubmit} noValidate className="space-y-4">
          <DialogHeader>
            <DialogTitle>
              {editing ? t("memory.editTitle") : t("memory.newTitle")}
            </DialogTitle>
            <DialogDescription>
              {editing ? t("memory.editDescription") : t("memory.newDescription")}
            </DialogDescription>
          </DialogHeader>

          {editing ? (
            <div className="space-y-2">
              <Label>{t("memory.namespaceAndKey")}</Label>
              <div className="flex flex-wrap items-center gap-2 rounded-md border bg-muted/30 px-3 py-2">
                <NamespaceBadge namespace={entry.namespace} />
                <span className="font-mono text-xs break-all">{entry.key}</span>
              </div>
            </div>
          ) : (
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="memory-namespace">{t("memory.namespace")}</Label>
                <Select
                  value={kind}
                  onValueChange={(next) => {
                    const value = next as NamespaceKind;
                    setKind(value);
                    if (value === "instance" && !scope) setScope(ordered[0]?.id ?? "");
                  }}
                >
                  <SelectTrigger
                    id="memory-namespace"
                    className="w-full"
                    aria-label={t("memory.namespaceFamilyAria")}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="global">{t("memory.kindGlobal")}</SelectItem>
                    <SelectItem value="shared">{t("memory.kindShared")}</SelectItem>
                    <SelectItem value="instance">{t("memory.kindInstance")}</SelectItem>
                    <SelectItem value="project">{t("memory.kindProject")}</SelectItem>
                  </SelectContent>
                </Select>
              </div>

              {kind === "instance" ? (
                <div className="space-y-2">
                  <Label htmlFor="memory-instance">{t("messages.instance")}</Label>
                  <Select value={scope} onValueChange={setScope}>
                    <SelectTrigger
                      id="memory-instance"
                      className="w-full"
                      aria-label={t("memory.instanceNamespaceAria")}
                      aria-invalid={!!fieldErrors.scope}
                    >
                      <SelectValue placeholder={t("messages.chooseInstance")} />
                    </SelectTrigger>
                    <SelectContent>
                      {ordered.map((instance) => (
                        <SelectItem key={instance.id} value={instance.id}>
                          <span className="font-mono">{instance.id}</span>
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {fieldErrors.scope ? (
                    <p className="text-xs text-destructive">{fieldErrors.scope}</p>
                  ) : null}
                </div>
              ) : kind === "project" ? (
                <div className="space-y-2">
                  <Label htmlFor="memory-project">{t("memory.project")}</Label>
                  <Input
                    id="memory-project"
                    value={scope}
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono"
                    placeholder="tchounis"
                    aria-invalid={!!fieldErrors.scope}
                    onChange={(event) => setScope(event.target.value)}
                  />
                  {fieldErrors.scope ? (
                    <p className="text-xs text-destructive">{fieldErrors.scope}</p>
                  ) : null}
                </div>
              ) : (
                <div className="space-y-2">
                  <Label>{t("memory.resolvedNamespace")}</Label>
                  <p className="flex h-9 items-center rounded-md border bg-muted/30 px-3 font-mono text-sm">
                    {namespace}
                  </p>
                </div>
              )}

              <div className="space-y-2 sm:col-span-2">
                <Label htmlFor="memory-key">{t("personas.vars.key")}</Label>
                <Input
                  id="memory-key"
                  value={key}
                  autoFocus
                  autoComplete="off"
                  spellCheck={false}
                  className="font-mono"
                  placeholder={t("memory.keyPlaceholder")}
                  aria-invalid={!!fieldErrors.key}
                  aria-describedby={fieldErrors.key ? "memory-key-error" : undefined}
                  onChange={(event) => setKey(event.target.value)}
                />
                {fieldErrors.key ? (
                  <p id="memory-key-error" className="text-xs text-destructive">
                    {fieldErrors.key}
                  </p>
                ) : null}
              </div>
            </div>
          )}

          <div className="space-y-2">
            <div className="flex items-baseline justify-between gap-3">
              <Label htmlFor="memory-value">{t("assistant.proposal.value")}</Label>
              <span
                className={cn(
                  "font-mono text-xs",
                  overLimit ? "text-destructive" : "text-muted-foreground",
                )}
              >
                {t("memory.valueBytes", { bytes: bytes.toLocaleString() })}
              </span>
            </div>
            <Textarea
              id="memory-value"
              rows={7}
              value={value}
              placeholder={t("memory.valuePlaceholder")}
              aria-invalid={!!fieldErrors.value || overLimit}
              aria-describedby={fieldErrors.value ? "memory-value-error" : "memory-value-help"}
              onChange={(event) => setValue(event.target.value)}
            />
            {fieldErrors.value ? (
              <p id="memory-value-error" className="text-xs text-destructive">
                {fieldErrors.value}
              </p>
            ) : (
              <p id="memory-value-help" className="text-xs text-muted-foreground">
                {t("memory.valueHint")}
              </p>
            )}
          </div>

          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="memory-tags">{t("memory.tags")}</Label>
              <Input
                id="memory-tags"
                value={tags}
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
                placeholder={t("memory.tagsExample")}
                onChange={(event) => setTags(event.target.value)}
              />
              <p className="text-xs text-muted-foreground">{t("memory.tagsLimit")}</p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="memory-ttl">{t("memory.expiry")}</Label>
              <Select value={ttl} onValueChange={setTtl}>
                <SelectTrigger
                  id="memory-ttl"
                  className="w-full"
                  aria-label={t("memory.expiry")}
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="none">
                    {editing ? t("memory.keepExpiry") : t("memory.noExpiry")}
                  </SelectItem>
                  {TTL_OPTIONS.map((option) => (
                    <SelectItem key={option.value} value={option.value}>
                      {t(option.labelKey)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>

          {error ? <ErrorAlert error={error} /> : null}

          <DialogFooter>
            <Button
              variant="outline"
              type="button"
              onClick={() => onOpenChange(false)}
              disabled={pending}
            >
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              {editing ? t("common.save") : t("memory.createEntry")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function VersionRow({
  entry,
  version,
  canEdit,
  onRestore,
}: {
  entry: MemoryEntry;
  version: MemoryVersion;
  canEdit: boolean;
  onRestore: (version: number) => void;
}) {
  const { t } = useI18n();
  const current = version.version === entry.version;
  return (
    <li className="relative pl-5">
      <span
        aria-hidden
        className={cn(
          "absolute top-1.5 left-0 size-2 rounded-full border",
          current ? "border-emerald-500 bg-emerald-500" : "border-muted-foreground/40 bg-background",
        )}
      />
      <div className="space-y-1.5">
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-mono text-xs font-medium">v{version.version}</span>
          <ActionBadge action={version.action} />
          {current ? (
            <Badge variant="secondary" className="font-mono text-[11px] font-normal">
              {t("memory.current")}
            </Badge>
          ) : null}
          <span className="text-xs text-muted-foreground">{version.owner}</span>
          <span className="font-mono text-[11px] text-muted-foreground">
            {formatDateTime(version.at)}
          </span>
        </div>
        <p className="line-clamp-3 font-mono text-xs break-words whitespace-pre-wrap text-muted-foreground">
          {version.value || "—"}
        </p>
        {canEdit && !current ? (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => onRestore(version.version)}
            aria-label={t("memory.restoreAria", { version: version.version })}
          >
            <RotateCcw aria-hidden />
            {t("memory.restoreThis")}
          </Button>
        ) : null}
      </div>
    </li>
  );
}

/**
 * Full entry, its append-only history and the four mutations. Every destructive
 * action goes through a confirmation; editing is delegated to the parent so the
 * two dialogs never stack.
 */
export function MemoryDetailSheet({
  entry,
  open,
  onOpenChange,
  canEdit,
  onChanged,
  onEdit,
}: {
  entry: MemoryEntry | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  canEdit: boolean;
  onChanged: () => void;
  onEdit: (entry: MemoryEntry) => void;
}) {
  const { t, lang } = useI18n();
  const [nonce, setNonce] = useState(0);
  const [pending, setPending] = useState<"restore" | "delete" | "promote" | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);
  const [restoreVersion, setRestoreVersion] = useState<number | null>(null);
  const [promoteOpen, setPromoteOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);

  const history = useResource(
    () => (entry ? api.memoryHistory(entry.id) : Promise.resolve([])),
    [entry?.id, nonce],
  );

  useEffect(() => {
    if (!open) return;
    setPending(null);
    setActionError(null);
    setRestoreVersion(null);
    setPromoteOpen(false);
    setDeleteOpen(false);
    setNonce(0);
  }, [open, entry?.id]);

  const bytes = entry ? memoryBytes(entry.value) : 0;

  async function restore(version: number) {
    if (!entry) return;
    setPending("restore");
    setActionError(null);
    try {
      await api.rollbackMemory(entry.id, version);
      toast.success(t("memory.restored", { version, key: entry.key }));
      setRestoreVersion(null);
      onChanged();
      setNonce((value) => value + 1);
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setPending(null);
    }
  }

  async function promote() {
    if (!entry) return;
    setPending("promote");
    setActionError(null);
    try {
      await api.promoteMemory(entry.id, "global");
      toast.success(t("memory.promoted", { key: entry.key }));
      setPromoteOpen(false);
      onChanged();
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setPending(null);
    }
  }

  async function remove() {
    if (!entry) return;
    setPending("delete");
    setActionError(null);
    try {
      await api.deleteMemory(entry.id);
      toast.success(t("memory.deleted", { key: entry.key }));
      setDeleteOpen(false);
      onOpenChange(false);
      onChanged();
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setPending(null);
    }
  }

  return (
    <>
      <Sheet open={open} onOpenChange={pending ? undefined : onOpenChange}>
        <SheetContent className="w-full gap-0 p-0 sm:max-w-xl">
          <SheetHeader className="border-b p-4 pr-12">
            <SheetTitle className="flex flex-wrap items-center gap-2">
              {entry ? (
                <>
                  <span className="font-mono text-base break-all">{entry.key}</span>
                  <Badge variant="secondary" className="font-mono text-[11px] font-normal">
                    v{entry.version}
                  </Badge>
                </>
              ) : (
                t("memory.entryTitle")
              )}
            </SheetTitle>
            <SheetDescription asChild>
              <div className="flex min-w-0 flex-wrap items-center gap-2 text-xs">
                <NamespaceBadge namespace={entry?.namespace ?? "shared"} />
                {entry ? (
                  <span className="text-muted-foreground">
                    {entry.owner} ·{" "}
                    {t("skills.picker.updated", {
                      time: formatRelative(entry.updated_at, lang),
                    })}
                  </span>
                ) : null}
              </div>
            </SheetDescription>
          </SheetHeader>

          {entry ? (
            <>
              <div className="min-h-0 flex-1 space-y-5 overflow-y-auto p-4">
                <section className="space-y-2">
                  <div className="flex items-baseline justify-between gap-3">
                    <h3 className="text-xs font-medium text-muted-foreground">
                      {t("assistant.proposal.value")}
                    </h3>
                    <span className="font-mono text-xs text-muted-foreground">
                      {t("memory.bytes", { bytes: bytes.toLocaleString() })}
                    </span>
                  </div>
                  <pre
                    tabIndex={0}
                    role="region"
                    aria-label={t("memory.valueAria")}
                    className="max-h-[45vh] overflow-auto rounded-md border bg-muted/20 p-3 font-mono text-xs leading-relaxed break-words whitespace-pre-wrap"
                  >
                    {entry.value || "—"}
                  </pre>
                </section>

                <DataList
                  items={[
                    {
                      label: t("memory.namespace"),
                      value: <NamespaceBadge namespace={entry.namespace} />,
                    },
                    {
                      label: t("personas.vars.key"),
                      value: (
                        <span className="flex items-start gap-2">
                          <span className="min-w-0 flex-1 font-mono text-xs break-all">{entry.key}</span>
                          <CopyButton
                            value={entry.key}
                            label={t("memory.copyKey")}
                            size="icon-xs"
                            className="-my-1 shrink-0"
                          />
                        </span>
                      ),
                    },
                    {
                      label: t("common.version"),
                      value: <span className="font-mono text-xs">{entry.version}</span>,
                    },
                    {
                      label: t("memory.owner"),
                      value: <span className="text-sm break-all">{entry.owner}</span>,
                    },
                    { label: t("common.created"), value: <When iso={entry.created_at} /> },
                    { label: t("common.updated"), value: <When iso={entry.updated_at} /> },
                    {
                      label: t("messages.expires"),
                      value: entry.expires_at ? (
                        <When iso={entry.expires_at} />
                      ) : (
                        <span className="text-xs text-muted-foreground">
                          {t("common.never")}
                        </span>
                      ),
                    },
                    {
                      label: t("memory.tags"),
                      value:
                        entry.tags.length > 0 ? (
                          <span className="flex flex-wrap gap-1">
                            {entry.tags.map((tag) => (
                              <Badge
                                key={tag}
                                variant="outline"
                                className="font-mono text-[11px] font-normal"
                              >
                                {tag}
                              </Badge>
                            ))}
                          </span>
                        ) : (
                          <span className="text-xs text-muted-foreground">
                            {t("personas.card.noTags")}
                          </span>
                        ),
                    },
                    {
                      label: t("memory.entryId"),
                      value: (
                        <span className="flex items-start gap-2">
                          <span className="font-mono text-xs">{entry.id}</span>
                          <CopyButton
                            value={String(entry.id)}
                            label={t("memory.copyEntryId")}
                            size="icon-xs"
                            className="-my-1 shrink-0"
                          />
                        </span>
                      ),
                    },
                  ]}
                />

                <section className="space-y-3" aria-labelledby="memory-history-title">
                  <div className="flex items-center justify-between gap-3">
                    <h3 id="memory-history-title" className="text-sm font-medium">
                      {t("skills.detail.history")}
                    </h3>
                    <span className="text-xs text-muted-foreground">
                      {history.data
                        ? t(
                            history.data.length === 1
                              ? "memory.versionOne"
                              : "memory.versionMany",
                            { count: history.data.length },
                          )
                        : "…"}
                    </span>
                  </div>

                  {history.error ? (
                    <ErrorAlert error={history.error} onRetry={history.reload} />
                  ) : null}

                  {history.loading && !history.data ? (
                    <div className="space-y-3">
                      <Skeleton className="h-16 w-full" />
                      <Skeleton className="h-16 w-full" />
                    </div>
                  ) : history.data && history.data.length > 0 ? (
                    <ol className="ml-1 space-y-4 border-l pl-1">
                      {history.data.map((version) => (
                        <VersionRow
                          key={`${version.version}-${version.at}`}
                          entry={entry}
                          version={version}
                          canEdit={canEdit}
                          onRestore={setRestoreVersion}
                        />
                      ))}
                    </ol>
                  ) : (
                    <p className="text-xs text-muted-foreground">{t("memory.noVersions")}</p>
                  )}
                </section>

                {actionError ? <ErrorAlert error={actionError} /> : null}
              </div>

              <SheetFooter className="gap-2 border-t p-4">
                {canEdit ? (
                  <div className="flex flex-wrap gap-2">
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        onOpenChange(false);
                        onEdit(entry);
                      }}
                    >
                      <Pencil aria-hidden />
                      {t("common.edit")}
                    </Button>
                    {entry.namespace !== "global" ? (
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        onClick={() => {
                          setActionError(null);
                          setPromoteOpen(true);
                        }}
                      >
                        <ArrowUpRight aria-hidden />
                        {t("memory.promoteToGlobal")}
                      </Button>
                    ) : null}
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      className="text-destructive hover:text-destructive"
                      onClick={() => {
                        setActionError(null);
                        setDeleteOpen(true);
                      }}
                    >
                      <Trash2 aria-hidden />
                      {t("common.delete")}
                    </Button>
                  </div>
                ) : (
                  <p className="text-xs text-muted-foreground">
                    {t("memory.readOnly")}
                  </p>
                )}
              </SheetFooter>
            </>
          ) : null}
        </SheetContent>
      </Sheet>

      <ConfirmDialog
        open={restoreVersion !== null}
        onOpenChange={(next) => {
          if (!next) {
            setRestoreVersion(null);
            setActionError(null);
          }
        }}
        title={t("memory.restoreTitle", { version: restoreVersion ?? "" })}
        description={t("memory.restoreDescription", { version: restoreVersion ?? "" })}
        confirmLabel={t("memory.restoreVersion")}
        pending={pending === "restore"}
        error={actionError}
        onConfirm={() => {
          if (restoreVersion !== null) void restore(restoreVersion);
        }}
      />

      <ConfirmDialog
        open={promoteOpen}
        onOpenChange={(next) => {
          if (!next) {
            setPromoteOpen(false);
            setActionError(null);
          }
        }}
        title={t("memory.promoteTitle")}
        description={<RichText text={t("memory.promoteDescription", {
          key: entry?.key ?? "",
          namespace: entry?.namespace ?? "",
        })} />}
        confirmLabel={t("memory.promote")}
        pending={pending === "promote"}
        error={pending === "promote" ? null : actionError}
        onConfirm={() => void promote()}
      />

      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={(next) => {
          if (!next) {
            setDeleteOpen(false);
            setActionError(null);
          }
        }}
        title={t("memory.deleteTitle")}
        description={t("memory.deleteDescription", { key: entry?.key ?? "" })}
        confirmLabel={t("memory.deleteEntry")}
        destructive
        pending={pending === "delete"}
        error={pending === "delete" ? null : actionError}
        onConfirm={() => void remove()}
      />
    </>
  );
}
