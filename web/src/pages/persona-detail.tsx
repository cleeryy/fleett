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
import { RichText } from "@/components/rich-text";
import { PageHeader } from "@/components/page-header";
import { PersonaApplyDialog } from "@/components/persona-apply-dialog";
import { PersonaDeleteDialog } from "@/components/persona-delete-dialog";
import { PersonaPreviewPanel } from "@/components/persona-preview";
import { PersonaStateBadge } from "@/components/persona-state-badge";
import { PersonaVariableEditor, PersonaVariableInputs } from "@/components/persona-vars";
import { SOUL_SKELETON, SoulHelpButton } from "@/components/soul-help";
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
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
  const { t } = useI18n();
  if (detail.variables.length === 0) {
    return (
      <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
        {t("personas.detail.noVariables")}
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
              {t("personas.detail.defaultLabel")}{" "}
              {variable.default ? (
                <span className="font-mono">{variable.default}</span>
              ) : (
                t("common.none")
              )}
            </span>
          </span>
        </li>
      ))}
    </ul>
  );
}

function PersonaHistory({ id, nonce }: { id: string; nonce: number }) {
  const { t, lang } = useI18n();
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
      title={t("personas.detail.historyTitle")}
      description={t("personas.detail.historyDescription")}
    >
      {log.loading && !log.data ? (
        <Skeleton className="h-32 w-full rounded-md" />
      ) : log.error ? (
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-md border px-4 py-3">
          <p className="text-sm text-muted-foreground">
            {t("personas.detail.historyUnavailable")}
            {log.error.status > 0 ? (
              <span className="ml-1 font-mono text-xs">HTTP {log.error.status}</span>
            ) : null}
          </p>
          <Button variant="outline" size="sm" onClick={log.reload}>
            <RefreshCw aria-hidden />
            {t("common.retry")}
          </Button>
        </div>
      ) : commits.length === 0 ? (
        <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
          {t("personas.detail.noHistory")}
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
                      {formatRelative(commit.at, lang)}
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
                    {t("history.fileCount", { count: selected.paths.length })}
                  </span>
                </>
              ) : (
                <span>{t("personas.detail.pickVersion")}</span>
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
                empty={t("skills.detail.noTextVersion")}
              />
            )}
          </div>
        </div>
      )}
    </Section>
  );
}

export function PersonaDetailPage({ id }: { id: string }) {
  const { t, lang } = useI18n();
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
              <a href="#/personas">{t("personas.detail.back")}</a>
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
  const variableErrors = draft ? (attempted ? validateVariableRows(draft.rows, t) : {}) : {};
  const parsedTags = draft ? parseTags(draft.tags) : persona.tags;
  const badTag = parsedTags.find((tag) => tag.length > PERSONA_TAG_MAX);
  const undeclared = draft
    ? templateVariables(draft.soul).filter(
        (key) => key !== "agent_name" && !draft.rows.some((row) => row.key.trim() === key),
      )
    : [];

  const errors: Record<string, string> = {};
  if (draft) {
    if (!draft.name.trim()) errors.name = t("personas.create.nameRequired");
    else if (draft.name.trim().length > 128) errors.name = t("personas.create.nameTooLong");
    if (badTag) {
      errors.tags = t("personas.create.tagTooLong", { tag: badTag, max: PERSONA_TAG_MAX });
    }
    if (!draft.soul.trim()) errors.soul = t("personas.detail.soulRequired");
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
    if (Object.keys(validateVariableRows(draft.rows, t)).length > 0) {
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
      toast.success(t("personas.detail.saved"));
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
      ? t("personas.detail.previewSample")
      : t("personas.detail.previewInstance", { instance: previewInstance });

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
            <span className="block">
              {persona.description || t("skills.picker.noDescription")}
            </span>
            <span className="flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-xs">
              <span>{persona.id}</span>
              <span aria-hidden>·</span>
              <span>
                {t(
                  persona.variables.length === 1
                    ? "personas.picker.variableOne"
                    : "personas.picker.variableMany",
                  { count: persona.variables.length },
                )}
              </span>
              <span aria-hidden>·</span>
              <span>
                {t(
                  persona.instances_count === 1
                    ? "skills.picker.instanceOne"
                    : "skills.picker.instanceMany",
                  { count: persona.instances_count },
                )}
              </span>
              <span aria-hidden>·</span>
              <span>
                {t("skills.picker.updated", {
                  time: formatRelative(persona.updated_at, lang),
                })}
              </span>
            </span>
          </span>
        }
        actions={
          <>
            <Button
              variant="outline"
              size="icon-sm"
              onClick={() => setNonce((value) => value + 1)}
              aria-label={t("personas.detail.refresh")}
              title={t("personas.detail.refresh")}
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
                  {t("personas.detail.applyToInstances")}
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="icon-sm"
                      aria-label={t("providers.detail.moreActions")}
                      title={t("providers.detail.moreActions")}
                    >
                      <MoreHorizontal aria-hidden />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem variant="destructive" onSelect={() => setDeleteOpen(true)}>
                      <Trash2 aria-hidden />
                      {t("personas.detail.delete")}
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
            <RichText
              text={t(
                diverged.length === 1
                  ? "personas.detail.divergedOne"
                  : "personas.detail.divergedMany",
                { count: diverged.length },
              )}
            />
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
            {t("personas.detail.reapplyTo", { count: diverged.length })}
          </Button>
        </div>
      ) : null}

      {canEdit ? (
        draft ? (
        <form onSubmit={save} noValidate className="space-y-6">
          <Section title={t("providers.detail.metadata")}>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="persona-edit-name">{t("common.name")}</Label>
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
                <Label htmlFor="persona-edit-tags">{t("personas.create.tags")}</Label>
                <Input
                  id="persona-edit-tags"
                  value={draft.tags}
                  autoComplete="off"
                  spellCheck={false}
                  className="font-mono text-xs"
                  placeholder={t("personas.create.tagsPlaceholder")}
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
                <Label htmlFor="persona-edit-description">{t("common.description")}</Label>
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
                {t("personas.detail.fixVariables")}
              </p>
            ) : null}
          </div>

          <Section title={t("personas.create.soul")}>
            <div className="space-y-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <p className="text-xs text-muted-foreground">
                  <RichText text={t("personas.detail.soulHint")} />
                </p>
                <div className="flex items-center gap-1">
                  {!draft.soul.trim() ? (
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => update({ soul: SOUL_SKELETON })}
                    >
                      {t("personas.create.insertSkeleton")}
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
                  {t(
                    templateVariables(draft.soul).length === 1
                      ? "personas.detail.placeholderOne"
                      : "personas.detail.placeholderMany",
                    { count: templateVariables(draft.soul).length },
                  )}
                </p>
                <span className="font-mono text-xs text-muted-foreground">
                  {t("personas.create.characters", { count: draft.soul.length })}
                </span>
              </div>
              {showErrors && errors.soul ? (
                <p id="persona-edit-soul-error" className="text-xs text-destructive">
                  {errors.soul}
                </p>
              ) : null}
              {undeclared.length > 0 ? (
                <p className="text-xs text-amber-700 dark:text-amber-400">
                  {t("personas.detail.undeclared", {
                    keys: undeclared.map((key) => `{{${key}}}`).join(", "),
                  })}
                </p>
              ) : null}
            </div>
          </Section>

          <div className="flex flex-wrap items-center gap-2 border-t pt-4">
            <Button type="submit" disabled={!dirty || saving}>
              {saving ? <Loader2 aria-hidden className="animate-spin" /> : null}
              {t("personas.detail.save")}
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
              {t("env.discard")}
            </Button>
            <p className="text-xs text-muted-foreground sm:ml-auto">
              {dirty
                ? t("personas.detail.unsaved")
                : t("personas.detail.savedAt", {
                    time: formatDateTime(persona.updated_at),
                  })}
            </p>
          </div>

          {saveError ? <ErrorAlert error={saveError} /> : null}
        </form>
        ) : (
          <Skeleton className="h-72 w-full rounded-md" />
        )
      ) : (
        <div className="space-y-6">
          <Section title={t("providers.detail.metadata")}>
            <DataList
              items={[
                { label: t("common.name"), value: persona.name },
                { label: t("providers.detail.id"), value: <Mono>{persona.id}</Mono> },
                {
                  label: t("common.description"),
                  value: persona.description || (
                    <span className="text-muted-foreground">{t("common.none")}</span>
                  ),
                },
                {
                  label: t("personas.create.tags"),
                  value: persona.tags.length ? (
                    <span className="flex flex-wrap gap-1">
                      {persona.tags.map((tag) => (
                        <Badge key={tag} variant="secondary" className="font-mono text-[11px] font-normal">
                          {tag}
                        </Badge>
                      ))}
                    </span>
                  ) : (
                    <span className="text-muted-foreground">{t("common.none")}</span>
                  ),
                },
                {
                  label: t("nav.instances"),
                  value: t(
                    persona.instances_count === 1
                      ? "skills.picker.instanceOne"
                      : "skills.picker.instanceMany",
                    { count: persona.instances_count },
                  ),
                },
                {
                  label: t("common.updated"),
                  value: (
                    <span
                      className="font-mono text-xs"
                      title={formatRelative(persona.updated_at, lang)}
                    >
                      {formatDateTime(persona.updated_at)}
                    </span>
                  ),
                },
              ]}
            />
          </Section>
          <Section title={t("personas.vars.title")}>
            <VariablesReadOnly detail={persona} />
          </Section>
          <Section title={t("personas.create.soul")}>
            <CodeBlock value={persona.soul_md} title="SOUL.md" maxHeight="50vh" />
          </Section>
        </div>
      )}

      <Section
        title={t("personas.detail.preview")}
        description={t("personas.detail.previewDescription")}
        action={canEdit ? undefined : <SoulHelpButton />}
      >
        <div className="grid gap-4 lg:grid-cols-[280px_minmax(0,1fr)]">
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="persona-preview-instance">{t("personas.detail.values")}</Label>
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
                  <SelectItem value="sample">{t("personas.detail.sampleValues")}</SelectItem>
                  {(instances.data ?? []).map((instance) => (
                    <SelectItem key={instance.id} value={instance.id}>
                      {instance.name} · {instance.id}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                {t("personas.detail.valuesHint")}
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
                  {t("personas.vars.noVariables")}
                </p>
              )
            ) : (
              <p className="rounded-md border bg-muted/20 px-3 py-2.5 text-xs text-muted-foreground">
                {t("personas.detail.usingStored")}{" "}
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
        title={t("providers.detail.linkedInstances")}
        description={t("personas.detail.linkedDescription")}
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
              {t("personas.detail.applyEllipsis")}
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
              title={t("personas.detail.noLinkedTitle")}
              description={t("personas.detail.noLinkedDescription")}
              action={
                canEdit ? (
                  <Button asChild variant="outline" size="sm">
                    <a href={hrefFor("/instances/new")}>{t("nav.newInstance")}</a>
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
                    {t("personas.detail.reapply")}
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
              toast.success(t("personas.detail.deleted", { id: persona.id }));
              navigate("/personas");
            }}
          />
        </>
      ) : null}
    </>
  );
}
