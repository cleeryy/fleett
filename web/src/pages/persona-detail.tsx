import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import {
  History as HistoryIcon,
  Loader2,
  MoreHorizontal,
  RefreshCw,
  RotateCcw,
  Send,
  Trash2,
  Users,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { PatchPersonaInput, PersonaDetail } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
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
import { Textarea } from "@/components/ui/textarea";
import { CodeBlock } from "@/components/code-block";
import { DataList, Section } from "@/components/data-list";
import { PatchView } from "@/components/diff-view";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { PersonaApplyDialog } from "@/components/persona-apply-dialog";
import { PersonaDeleteDialog } from "@/components/persona-delete-dialog";
import { PersonaPreviewPanel } from "@/components/persona-preview";
import { PersonaStateBadge } from "@/components/persona-state-badge";
import { PersonaVariableEditor, PersonaVariableInputs } from "@/components/persona-vars";
import { SOUL_SKELETON, SoulHelpButton } from "@/components/soul-help";
import { formatDateTime, formatRelative, plural } from "@/lib/format";
import {
  PERSONA_TAG_MAX,
  formatTags,
  parseTags,
  rowsFromVariables,
  templateVariables,
  validateVariableRows,
  variablesFromRows,
  type PersonaVariableRow,
} from "@/lib/personas";
import { hrefFor, navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

const FIELD_IDS: Record<string, string> = {
  name: "persona-edit-name",
  tags: "persona-edit-tags",
  soul: "persona-edit-soul",
};

interface PersonaDraft {
  name: string;
  description: string;
  tags: string;
  rows: PersonaVariableRow[];
  soul: string;
}

function toDraft(detail: PersonaDetail): PersonaDraft {
  return {
    name: detail.name,
    description: detail.description,
    tags: formatTags(detail.tags),
    rows: rowsFromVariables(detail.variables),
    soul: detail.soul_md,
  };
}

/** Compare meaningful values only: row ids and tag spacing do not count. */
function fingerprint(draft: PersonaDraft): string {
  return JSON.stringify({
    name: draft.name.trim(),
    description: draft.description.trim(),
    tags: parseTags(draft.tags),
    variables: variablesFromRows(draft.rows),
    soul: draft.soul,
  });
}

function Mono({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={cn("font-mono text-xs break-all", className)}>{children}</span>;
}

function DetailSkeleton() {
  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <Skeleton className="h-7 w-56" />
        <Skeleton className="h-4 w-80" />
      </div>
      <div className="grid gap-6 lg:grid-cols-2">
        <Skeleton className="h-56 w-full rounded-md" />
        <Skeleton className="h-56 w-full rounded-md" />
      </div>
      <Skeleton className="h-72 w-full rounded-md" />
    </div>
  );
}

function VariablesReadOnly({ detail }: { detail: PersonaDetail }) {
  if (detail.variables.length === 0) {
    return (
      <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
        No variables: the same SOUL.md is used by every instance.
      </p>
    );
  }
  return (
    <ul className="divide-y rounded-md border">
      {detail.variables.map((variable) => (
        <li key={variable.key} className="grid gap-1 px-4 py-2.5 sm:grid-cols-[180px_minmax(0,1fr)] sm:gap-4">
          <span className="font-mono text-xs">{"{{" + variable.key + "}}"}</span>
          <span className="min-w-0 space-y-0.5">
            <span className="block text-sm text-muted-foreground">{variable.description}</span>
            <span className="block text-xs text-muted-foreground">
              Default: {variable.default ? <span className="font-mono">{variable.default}</span> : "none"}
            </span>
          </span>
        </li>
      ))}
    </ul>
  );
}

function PersonaHistory({ id, nonce }: { id: string; nonce: number }) {
  const log = useResource(() => api.personaGitLog(id, { limit: 20 }), [id, nonce]);
  const commits = log.data?.commits ?? [];
  const [selectedId, setSelectedId] = useState<string | null>(null);

  useEffect(() => {
    setSelectedId((current) =>
      current && commits.some((commit) => commit.id === current)
        ? current
        : (commits[0]?.id ?? null),
    );
  }, [commits]);

  const diff = useResource(
    () => (selectedId ? api.personaGitDiff(id, { to: selectedId }) : Promise.resolve(null)),
    [id, selectedId, nonce],
  );
  const selected = commits.find((commit) => commit.id === selectedId) ?? null;

  return (
    <Section
      title="Version history"
      description="Every save of the persona, committed to the personas repository."
    >
      {log.loading && !log.data ? (
        <Skeleton className="h-32 w-full rounded-md" />
      ) : log.error ? (
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-md border px-4 py-3">
          <p className="text-sm text-muted-foreground">
            Version history is not available on this server yet. Saves still update the persona.
            {log.error.status > 0 ? (
              <span className="ml-1 font-mono text-xs">HTTP {log.error.status}</span>
            ) : null}
          </p>
          <Button variant="outline" size="sm" onClick={log.reload}>
            <RefreshCw aria-hidden />
            Retry
          </Button>
        </div>
      ) : commits.length === 0 ? (
        <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
          No saved version yet. Editing and saving the persona creates the first one.
        </p>
      ) : (
        <div className="grid gap-3 lg:grid-cols-[320px_minmax(0,1fr)]">
          <ul className="max-h-[45vh] divide-y overflow-auto rounded-md border">
            {commits.map((commit) => (
              <li key={commit.id}>
                <button
                  type="button"
                  aria-current={commit.id === selectedId ? "true" : undefined}
                  onClick={() => setSelectedId(commit.id)}
                  className={cn(
                    "w-full space-y-1 px-3 py-2.5 text-left hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:-outline-offset-2",
                    commit.id === selectedId && "bg-muted",
                  )}
                >
                  <span className="flex items-baseline gap-2">
                    <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
                      {commit.short}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-sm">{commit.message}</span>
                  </span>
                  <span className="flex flex-wrap items-center gap-x-1.5 text-[11px] text-muted-foreground">
                    <span className="max-w-40 truncate">{commit.author}</span>
                    <span aria-hidden>·</span>
                    <time dateTime={commit.at} title={formatDateTime(commit.at)}>
                      {formatRelative(commit.at)}
                    </time>
                  </span>
                </button>
              </li>
            ))}
          </ul>

          <div className="min-w-0 overflow-hidden rounded-md border">
            <div className="flex flex-wrap items-center gap-x-2 gap-y-1 border-b px-3 py-2 text-xs text-muted-foreground">
              <HistoryIcon aria-hidden className="size-3.5" />
              {selected ? (
                <>
                  <span className="font-mono text-foreground">{selected.short}</span>
                  <span aria-hidden>·</span>
                  <span className="font-mono">
                    {selected.paths.length} {plural(selected.paths.length, "file")}
                  </span>
                </>
              ) : (
                <span>Pick a version.</span>
              )}
            </div>
            {diff.loading || diff.refreshing ? (
              <Skeleton className="m-3 h-40 rounded-md" />
            ) : diff.error ? (
              <div className="p-3">
                <ErrorAlert error={diff.error} onRetry={diff.reload} />
              </div>
            ) : (
              <PatchView
                patch={diff.data?.diff ?? ""}
                empty="This version did not change a text file."
              />
            )}
          </div>
        </div>
      )}
    </Section>
  );
}

export function PersonaDetailPage({ id }: { id: string }) {
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [applyOpen, setApplyOpen] = useState(false);
  const [applyDefault, setApplyDefault] = useState<string[] | undefined>(undefined);
  const detail = useResource(() => api.getPersona(id), [id, nonce]);
  const instances = useResource(() => api.listInstances(), [id, nonce]);

  const [draft, setDraft] = useState<PersonaDraft | null>(null);
  const [attempted, setAttempted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<ApiError | null>(null);
  const draftFor = useRef<string | null>(null);
  const resetPending = useRef(false);

  const [previewInstance, setPreviewInstance] = useState("sample");
  const [previewVars, setPreviewVars] = useState<Record<string, string>>({});

  useEffect(() => {
    if (!detail.data) return;
    if (draftFor.current !== detail.data.id || resetPending.current) {
      setDraft(toDraft(detail.data));
      draftFor.current = detail.data.id;
      resetPending.current = false;
    }
  }, [detail.data]);

  useEffect(() => {
    setPreviewInstance("sample");
    setPreviewVars({});
    setApplyOpen(false);
    setDeleteOpen(false);
    setAttempted(false);
    setSaveError(null);
  }, [id]);

  if (detail.loading && !detail.data) return <DetailSkeleton />;

  if (!detail.data) {
    return (
      <>
        <PageHeader
          title={id}
          actions={
            <Button asChild variant="outline">
              <a href="#/personas">Back to personas</a>
            </Button>
          }
        />
        {detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null}
      </>
    );
  }

  const persona = detail.data;
  const linked = (instances.data ?? []).filter((instance) => instance.persona === persona.id);
  const diverged = linked.filter((instance) => instance.persona_state === "diverged");
  const merged = draft ? fingerprint(draft) : fingerprint(toDraft(persona));
  const dirty = draft !== null && merged !== fingerprint(toDraft(persona));
  const variableErrors = draft ? (attempted ? validateVariableRows(draft.rows) : {}) : {};
  const parsedTags = draft ? parseTags(draft.tags) : persona.tags;
  const badTag = parsedTags.find((tag) => tag.length > PERSONA_TAG_MAX);
  const undeclared = draft
    ? templateVariables(draft.soul).filter(
        (key) => key !== "agent_name" && !draft.rows.some((row) => row.key.trim() === key),
      )
    : [];

  const errors: Record<string, string> = {};
  if (draft) {
    if (!draft.name.trim()) errors.name = "Enter a name.";
    else if (draft.name.trim().length > 128) errors.name = "Use 128 characters or fewer.";
    if (badTag) errors.tags = `"${badTag}" is longer than ${PERSONA_TAG_MAX} characters.`;
    if (!draft.soul.trim()) errors.soul = "SOUL.md must not be empty.";
  }
  const showErrors = attempted && draft !== null;

  function update(changes: Partial<PersonaDraft>) {
    setDraft((current) => (current ? { ...current, ...changes } : current));
  }

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!draft) return;
    setAttempted(true);
    const first = ["name", "tags", "soul"].find((key) => errors[key]);
    if (first) {
      document.getElementById(FIELD_IDS[first])?.focus();
      return;
    }
    if (Object.keys(validateVariableRows(draft.rows)).length > 0) {
      document.getElementById("persona-edit-variables")?.scrollIntoView({ block: "center" });
      return;
    }

    setSaving(true);
    setSaveError(null);
    try {
      const patch: PatchPersonaInput = {
        name: draft.name.trim(),
        description: draft.description.trim(),
        tags: parsedTags,
        variables: variablesFromRows(draft.rows),
        soul_md: draft.soul,
      };
      await api.patchPersona(id, patch);
      toast.success("Persona saved.");
      setAttempted(false);
      resetPending.current = true;
      setNonce((value) => value + 1);
    } catch (cause) {
      setSaveError(toApiError(cause));
    } finally {
      setSaving(false);
    }
  }

  const previewDescription =
    previewInstance === "sample"
      ? "Sample values only: nothing is stored."
      : `Using the values stored on ${previewInstance}. Save the editor to include unsaved text.`;

  return (
    <>
      <PageHeader
        title={
          <span className="flex flex-wrap items-center gap-3">
            {persona.name}
            {persona.tags.map((tag) => (
              <Badge key={tag} variant="secondary" className="font-mono text-[11px] font-normal">
                {tag}
              </Badge>
            ))}
          </span>
        }
        description={
          <span className="space-y-1">
            <span className="block">{persona.description || "No description."}</span>
            <span className="flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-xs">
              <span>{persona.id}</span>
              <span aria-hidden>·</span>
              <span>
                {persona.variables.length} {plural(persona.variables.length, "variable")}
              </span>
              <span aria-hidden>·</span>
              <span>
                {persona.instances_count} {plural(persona.instances_count, "instance")}
              </span>
              <span aria-hidden>·</span>
              <span>updated {formatRelative(persona.updated_at)}</span>
            </span>
          </span>
        }
        actions={
          <>
            <Button
              variant="outline"
              size="icon-sm"
              onClick={() => setNonce((value) => value + 1)}
              aria-label="Refresh persona"
              title="Refresh persona"
            >
              <RefreshCw aria-hidden className={detail.refreshing ? "animate-spin" : undefined} />
            </Button>
            {canEdit ? (
              <>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={(instances.data?.length ?? 0) === 0}
                  onClick={() => {
                    setApplyDefault(undefined);
                    setApplyOpen(true);
                  }}
                >
                  <Send aria-hidden />
                  Apply to instances
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="icon-sm"
                      aria-label="More actions"
                      title="More actions"
                    >
                      <MoreHorizontal aria-hidden />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem variant="destructive" onSelect={() => setDeleteOpen(true)}>
                      <Trash2 aria-hidden />
                      Delete persona…
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </>
            ) : null}
          </>
        }
      />

      {detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null}

      {diverged.length > 0 && canEdit ? (
        <div className="flex flex-wrap items-center gap-3 rounded-md border border-amber-600/40 bg-amber-50/40 px-4 py-3 text-sm dark:bg-amber-950/20">
          <PersonaStateBadge state="diverged" />
          <p className="min-w-0 flex-1 text-muted-foreground">
            {diverged.length} {plural(diverged.length, "instance")} edited{" "}
            <span className="font-mono">SOUL.md</span> by hand after the persona was applied.
          </p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              setApplyDefault(diverged.map((instance) => instance.id));
              setApplyOpen(true);
            }}
          >
            <RotateCcw aria-hidden />
            Reapply to {diverged.length}
          </Button>
        </div>
      ) : null}

      {canEdit ? (
        draft ? (
        <form onSubmit={save} noValidate className="space-y-6">
          <Section title="Metadata">
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="persona-edit-name">Name</Label>
                <Input
                  id="persona-edit-name"
                  value={draft.name}
                  autoComplete="off"
                  aria-invalid={showErrors && !!errors.name}
                  aria-describedby={showErrors && errors.name ? "persona-edit-name-error" : undefined}
                  onChange={(event) => update({ name: event.target.value })}
                />
                {showErrors && errors.name ? (
                  <p id="persona-edit-name-error" className="text-xs text-destructive">
                    {errors.name}
                  </p>
                ) : null}
              </div>
              <div className="space-y-2">
                <Label htmlFor="persona-edit-tags">Tags</Label>
                <Input
                  id="persona-edit-tags"
                  value={draft.tags}
                  autoComplete="off"
                  spellCheck={false}
                  className="font-mono text-xs"
                  placeholder="ops, tone, infra"
                  aria-invalid={showErrors && !!errors.tags}
                  aria-describedby={showErrors && errors.tags ? "persona-edit-tags-error" : undefined}
                  onChange={(event) => update({ tags: event.target.value })}
                />
                {showErrors && errors.tags ? (
                  <p id="persona-edit-tags-error" className="text-xs text-destructive">
                    {errors.tags}
                  </p>
                ) : null}
              </div>
              <div className="space-y-2 sm:col-span-2">
                <Label htmlFor="persona-edit-description">Description</Label>
                <Textarea
                  id="persona-edit-description"
                  rows={2}
                  value={draft.description}
                  onChange={(event) => update({ description: event.target.value })}
                />
              </div>
            </div>
          </Section>

          <div id="persona-edit-variables" className="scroll-mt-24">
            <PersonaVariableEditor
              rows={draft.rows}
              onChange={(rows) => update({ rows })}
              errors={variableErrors}
              idPrefix="persona-edit-var"
              disabled={saving}
            />
            {Object.keys(variableErrors).length > 0 ? (
              <p className="mt-1 text-xs text-destructive">
                Fix the variable rows marked above before saving.
              </p>
            ) : null}
          </div>

          <Section title="SOUL.md">
            <div className="space-y-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <p className="text-xs text-muted-foreground">
                  Markdown read by the agent at startup. Variables are written{" "}
                  <span className="font-mono">{"{{key}}"}</span>.
                </p>
                <div className="flex items-center gap-1">
                  {!draft.soul.trim() ? (
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => update({ soul: SOUL_SKELETON })}
                    >
                      Insert a skeleton
                    </Button>
                  ) : null}
                  <SoulHelpButton />
                </div>
              </div>
              <Textarea
                id="persona-edit-soul"
                rows={18}
                value={draft.soul}
                spellCheck={false}
                className="min-h-72 font-mono text-xs leading-relaxed"
                aria-invalid={showErrors && !!errors.soul}
                aria-describedby={showErrors && errors.soul ? "persona-edit-soul-error" : undefined}
                onChange={(event) => update({ soul: event.target.value })}
              />
              <div className="flex flex-wrap items-center justify-between gap-2">
                <p className="text-xs text-muted-foreground">
                  {templateVariables(draft.soul).length}{" "}
                  {plural(templateVariables(draft.soul).length, "placeholder")} in the text.
                </p>
                <span className="font-mono text-xs text-muted-foreground">
                  {draft.soul.length} characters
                </span>
              </div>
              {showErrors && errors.soul ? (
                <p id="persona-edit-soul-error" className="text-xs text-destructive">
                  {errors.soul}
                </p>
              ) : null}
              {undeclared.length > 0 ? (
                <p className="text-xs text-amber-700 dark:text-amber-400">
                  Placeholders without a variable:{" "}
                  {undeclared.map((key) => `{{${key}}}`).join(", ")}. Add them above, or the preview
                  will report them as missing.
                </p>
              ) : null}
            </div>
          </Section>

          <div className="flex flex-wrap items-center gap-2 border-t pt-4">
            <Button type="submit" disabled={!dirty || saving}>
              {saving ? <Loader2 aria-hidden className="animate-spin" /> : null}
              Save persona
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={!dirty || saving}
              onClick={() => {
                setDraft(toDraft(persona));
                setAttempted(false);
                setSaveError(null);
              }}
            >
              Discard
            </Button>
            <p className="text-xs text-muted-foreground sm:ml-auto">
              {dirty
                ? "Unsaved changes."
                : `Saved ${formatDateTime(persona.updated_at)}. Applying a persona rewrites data/SOUL.md and restarts the instance.`}
            </p>
          </div>

          {saveError ? <ErrorAlert error={saveError} /> : null}
        </form>
        ) : (
          <Skeleton className="h-72 w-full rounded-md" />
        )
      ) : (
        <div className="space-y-6">
          <Section title="Metadata">
            <DataList
              items={[
                { label: "Name", value: persona.name },
                { label: "Id", value: <Mono>{persona.id}</Mono> },
                {
                  label: "Description",
                  value: persona.description || <span className="text-muted-foreground">None</span>,
                },
                {
                  label: "Tags",
                  value: persona.tags.length ? (
                    <span className="flex flex-wrap gap-1">
                      {persona.tags.map((tag) => (
                        <Badge key={tag} variant="secondary" className="font-mono text-[11px] font-normal">
                          {tag}
                        </Badge>
                      ))}
                    </span>
                  ) : (
                    <span className="text-muted-foreground">None</span>
                  ),
                },
                {
                  label: "Instances",
                  value: `${persona.instances_count} ${plural(persona.instances_count, "instance")}`,
                },
                {
                  label: "Updated",
                  value: (
                    <span className="font-mono text-xs" title={formatRelative(persona.updated_at)}>
                      {formatDateTime(persona.updated_at)}
                    </span>
                  ),
                },
              ]}
            />
          </Section>
          <Section title="Variables">
            <VariablesReadOnly detail={persona} />
          </Section>
          <Section title="SOUL.md">
            <CodeBlock value={persona.soul_md} title="SOUL.md" maxHeight="50vh" />
          </Section>
        </div>
      )}

      <Section
        title="Preview"
        description="The SOUL.md an instance would read, with variables rendered."
        action={canEdit ? undefined : <SoulHelpButton />}
      >
        <div className="grid gap-4 lg:grid-cols-[280px_minmax(0,1fr)]">
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="persona-preview-instance">Values</Label>
              <Select
                value={previewInstance}
                onValueChange={(value) => {
                  setPreviewInstance(value);
                  setPreviewVars({});
                }}
              >
                <SelectTrigger id="persona-preview-instance" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="sample">Sample values</SelectItem>
                  {(instances.data ?? []).map((instance) => (
                    <SelectItem key={instance.id} value={instance.id}>
                      {instance.name} · {instance.id}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                Pick an instance to use the values it has stored for this persona.
              </p>
            </div>

            {previewInstance === "sample" ? (
              persona.variables.length > 0 ? (
                <PersonaVariableInputs
                  variables={persona.variables}
                  values={previewVars}
                  onChange={setPreviewVars}
                  idPrefix="persona-preview-var"
                />
              ) : (
                <p className="rounded-md border px-3 py-2.5 text-xs text-muted-foreground">
                  This persona takes no variables.
                </p>
              )
            ) : (
              <p className="rounded-md border bg-muted/20 px-3 py-2.5 text-xs text-muted-foreground">
                Using the values saved on{" "}
                <span className="font-mono text-foreground">{previewInstance}</span>.
              </p>
            )}
          </div>

          <PersonaPreviewPanel
            personaId={persona.id}
            variables={previewVars}
            instance={previewInstance === "sample" ? undefined : previewInstance}
            description={previewDescription}
            maxHeight="40vh"
            refreshKey={persona.updated_at}
          />
        </div>
      </Section>

      <Section
        title="Linked instances"
        description="Instances whose SOUL.md comes from this persona."
        action={
          linked.length > 0 && canEdit ? (
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                setApplyDefault(undefined);
                setApplyOpen(true);
              }}
            >
              <Send aria-hidden />
              Apply…
            </Button>
          ) : null
        }
      >
        {instances.loading && !instances.data ? (
          <Skeleton className="h-24 w-full rounded-md" />
        ) : linked.length === 0 ? (
          <div className="rounded-md border">
            <EmptyState
              icon={Users}
              title="No instance uses this persona"
              description="Apply it to an existing instance from here, or pick it when you create the next one."
              action={
                canEdit ? (
                  <Button asChild variant="outline" size="sm">
                    <a href={hrefFor("/instances/new")}>New instance</a>
                  </Button>
                ) : undefined
              }
            />
          </div>
        ) : (
          <ul className="divide-y rounded-md border">
            {linked.map((instance) => (
              <li
                key={instance.id}
                className="flex flex-wrap items-center gap-x-3 gap-y-1.5 px-3 py-2.5"
              >
                <a
                  href={hrefFor(`/instances/${instance.id}`)}
                  className="min-w-0 flex-1 truncate text-sm font-medium underline-offset-4 hover:underline"
                >
                  {instance.name}
                  <span className="ml-2 font-mono text-xs font-normal text-muted-foreground">
                    {instance.id}
                  </span>
                </a>
                <PersonaStateBadge state={instance.persona_state} />
                {instance.persona_state === "diverged" && canEdit ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      setApplyDefault([instance.id]);
                      setApplyOpen(true);
                    }}
                  >
                    <RotateCcw aria-hidden />
                    Reapply
                  </Button>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </Section>

      <PersonaHistory id={id} nonce={nonce} />

      {canEdit ? (
        <>
          <PersonaApplyDialog
            persona={persona}
            instances={instances.data ?? []}
            open={applyOpen}
            onOpenChange={setApplyOpen}
            onApplied={() => setNonce((value) => value + 1)}
            defaultSelected={applyDefault}
          />
          <PersonaDeleteDialog
            persona={persona}
            instances={instances.data ?? []}
            open={deleteOpen}
            onOpenChange={setDeleteOpen}
            onDeleted={() => {
              toast.success(`Persona ${persona.id} deleted.`);
              navigate("/personas");
            }}
          />
        </>
      ) : null}
    </>
  );
}
