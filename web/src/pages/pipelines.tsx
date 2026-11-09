import { useEffect, useMemo, useState, type FormEvent } from "react";
import {
  ArrowLeft,
  Ban,
  FileCode2,
  History,
  Loader2,
  Pencil,
  Play,
  Plus,
  RefreshCw,
  Trash2,
  Workflow,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type {
  Pipeline,
  PipelineRun,
  PipelineRunDetail,
} from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { OpsBadge } from "@/components/ops-badge";
import { PageHeader } from "@/components/page-header";
import { RichText } from "@/components/rich-text";
import { formatDateTime, formatDurationMs, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { parsePipelineYaml, pipelineToYaml, PipelineYamlError } from "@/lib/pipeline-yaml";
import { useIdentity } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

const SAMPLE = `name: release
trigger: manual
steps:
  - id: produce
    instance: alice
    prompt: "Draft the release notes from the last 20 commits."

  - id: verify
    instance: ops
    depends_on: [produce]
    prompt: |
      Review this draft and answer with the corrected text only:
      {{steps.produce.output}}
    retries: 1
    timeout_s: 300
    on_failure: stop

  - id: publish
    instance: alice
    depends_on: [verify]
    prompt: "Publish: {{steps.verify.output}}"
`;

function PipelineFormDialog({
  open,
  onOpenChange,
  pipeline,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  pipeline: Pipeline | null;
  onSaved: () => void;
}) {
  const { t } = useI18n();
  const [text, setText] = useState(SAMPLE);
  const [parseError, setParseError] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [pending, setPending] = useState(false);

  useEffect(() => {
    if (!open) return;
    setText(pipeline ? pipelineToYaml(pipeline) : SAMPLE);
    setParseError(null);
    setError(null);
  }, [open, pipeline]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setParseError(null);
    setError(null);
    let input;
    try {
      input = parsePipelineYaml(text);
    } catch (cause) {
      setParseError(
        cause instanceof PipelineYamlError
          ? cause.message
          : t("pipelines.form.invalid"),
      );
      return;
    }
    setPending(true);
    try {
      if (pipeline) await api.patchPipeline(pipeline.id, input);
      else await api.createPipeline(input);
      toast.success(pipeline ? t("pipelines.form.saved") : t("pipelines.form.created"));
      onOpenChange(false);
      onSaved();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[88dvh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>
            {pipeline ? t("pipelines.form.editTitle", { name: pipeline.name }) : t("pipelines.form.newTitle")}
          </DialogTitle>
          <DialogDescription>{t("pipelines.form.description")}</DialogDescription>
        </DialogHeader>

        <form onSubmit={submit} className="space-y-3">
          {error ? <ErrorAlert error={error} /> : null}
          <div className="space-y-1.5">
            <label htmlFor="pipeline-yaml" className="text-sm font-medium">
              {t("pipelines.form.definition")}
            </label>
            <Textarea
              id="pipeline-yaml"
              value={text}
              rows={18}
              spellCheck={false}
              className="min-h-[320px] font-mono text-xs leading-relaxed"
              aria-invalid={parseError !== null}
              aria-describedby={parseError ? "pipeline-yaml-error" : "pipeline-yaml-help"}
              onChange={(event) => {
                setText(event.target.value);
                if (parseError) setParseError(null);
              }}
            />
            {parseError ? (
              <p id="pipeline-yaml-error" className="text-xs text-destructive" role="alert">
                {parseError}
              </p>
            ) : (
              <p id="pipeline-yaml-help" className="text-xs text-muted-foreground">
                <RichText text={t("pipelines.form.fieldsHint")} />
              </p>
            )}
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : <FileCode2 aria-hidden />}
              {pipeline ? t("pipelines.form.save") : t("pipelines.form.create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function RunDetailView({
  pipelineId,
  runId,
  onBack,
  onCancelled,
}: {
  pipelineId: string;
  runId: string;
  onBack: () => void;
  onCancelled: () => void;
}) {
  const { t, lang } = useI18n();
  const admin = useIdentity()?.role === "admin";
  const [nonce, setNonce] = useState(0);
  const [status, setStatus] = useState("all");
  const [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const detail = useResource(() => api.getPipelineRun(pipelineId, runId), [pipelineId, runId, nonce]);
  const run: PipelineRunDetail | null = detail.data;

  // A run started from the console finishes in the background; poll while it
  // is not terminal instead of asking the operator to hit refresh.
  useEffect(() => {
    if (!run || (run.status !== "running" && run.status !== "pending")) return;
    const timer = window.setTimeout(() => setNonce((value) => value + 1), 2_000);
    return () => window.clearTimeout(timer);
  }, [run]);

  async function cancel() {
    setCancelling(true);
    setError(null);
    try {
      await api.cancelPipelineRun(pipelineId, runId);
      toast.success(t("pipelines.run.cancelled"));
      setNonce((value) => value + 1);
      onCancelled();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setCancelling(false);
    }
  }

  if (detail.loading && !run) {
    return <Skeleton className="h-48 w-full rounded-md" />;
  }
  if (!run) {
    return detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null;
  }

  const steps = run.steps.filter((step) => status === "all" || step.status === status);
  const cancellable = run.status === "running" || run.status === "pending";

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" size="sm" onClick={onBack}>
          <ArrowLeft aria-hidden />
          {t("pipelines.runs")}
        </Button>
        <OpsBadge status={run.status} />
        <span className="font-mono text-xs text-muted-foreground">{run.id}</span>
        <div className="ml-auto flex items-center gap-2">
          <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
            <RefreshCw aria-hidden className={detail.refreshing ? "animate-spin" : undefined} />
            {t("common.refresh")}
          </Button>
          {cancellable && admin ? (
            <Button variant="outline" size="sm" disabled={cancelling} onClick={() => void cancel()}>
              {cancelling ? <Loader2 aria-hidden className="animate-spin" /> : <Ban aria-hidden />}
              {t("pipelines.run.cancel")}
            </Button>
          ) : null}
        </div>
      </div>

      {error ? <ErrorAlert error={error} /> : null}
      {run.error ? (
        <p className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 font-mono text-xs break-words whitespace-pre-wrap text-destructive">
          {run.error}
        </p>
      ) : null}

      <dl className="grid grid-cols-2 gap-x-4 gap-y-2 rounded-md border p-3 text-xs sm:grid-cols-4">
        <div>
          <dt className="text-muted-foreground">{t("pipelines.run.trigger")}</dt>
          <dd className="font-mono">{run.trigger}</dd>
        </div>
        <div>
          <dt className="text-muted-foreground">{t("deployments.started")}</dt>
          <dd className="font-mono">{run.started_at ? formatDateTime(run.started_at) : "—"}</dd>
        </div>
        <div>
          <dt className="text-muted-foreground">{t("deployments.finished")}</dt>
          <dd className="font-mono">
            {run.finished_at
              ? formatRelative(run.finished_at, lang)
              : t("status.running")}
          </dd>
        </div>
        <div>
          <dt className="text-muted-foreground">{t("pipelines.run.stepsOk")}</dt>
          <dd className="font-mono">
            {run.steps.filter((step) => step.status === "ok").length}/{run.steps.length}
          </dd>
        </div>
      </dl>

      {run.context ? (
        <details className="rounded-md border">
          <summary className="cursor-pointer px-3 py-2 text-sm">
            {t("pipelines.run.context")}
          </summary>
          <pre className="overflow-auto border-t p-3 font-mono text-[11px] whitespace-pre-wrap">
            {run.context}
          </pre>
        </details>
      ) : null}

      <div className="flex flex-wrap items-center gap-2">
        <Select value={status} onValueChange={setStatus}>
          <SelectTrigger className="w-44" aria-label={t("pipelines.run.filterAria")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t("pipelines.run.allSteps")}</SelectItem>
            <SelectItem value="ok">{t("status.ok")}</SelectItem>
            <SelectItem value="failed">{t("status.failed")}</SelectItem>
            <SelectItem value="running">{t("status.running")}</SelectItem>
            <SelectItem value="pending">{t("status.pending")}</SelectItem>
            <SelectItem value="skipped">{t("status.skipped")}</SelectItem>
          </SelectContent>
        </Select>
        <span className="text-xs text-muted-foreground">
          {t("pipelines.run.stepsShown", { shown: steps.length, total: run.steps.length })}
        </span>
      </div>

      {steps.length === 0 ? (
        <p className="rounded-md border px-3 py-6 text-center text-sm text-muted-foreground">
          {t("pipelines.run.noStep")}
        </p>
      ) : (
        <ul className="space-y-2">
          {steps.map((step) => (
            <li key={`${step.run_id}-${step.step_id}`} className="space-y-2 rounded-md border p-3">
              <div className="flex flex-wrap items-start justify-between gap-2">
                <div className="min-w-0">
                  <p className="font-mono text-sm font-medium">{step.step_id}</p>
                  <p className="font-mono text-[11px] text-muted-foreground">
                    {t("pipelines.run.attempt", {
                      instance: step.instance_id,
                      attempt: step.attempt,
                    })}
                  </p>
                </div>
                <div className="flex items-center gap-2">
                  {step.started_at && step.finished_at ? (
                    <span className="font-mono text-[11px] text-muted-foreground">
                      {formatDurationMs(
                        Date.parse(step.finished_at) - Date.parse(step.started_at),
                      )}
                    </span>
                  ) : null}
                  <OpsBadge status={step.status} />
                </div>
              </div>
              {step.error ? (
                <p className="rounded-md border border-destructive/30 px-2 py-1.5 font-mono text-xs break-words whitespace-pre-wrap text-destructive">
                  {step.error}
                </p>
              ) : null}
              {step.output ? (
                <details>
                  <summary className="cursor-pointer text-xs text-muted-foreground hover:text-foreground">
                    {t("pipelines.run.output")}
                  </summary>
                  <pre className="mt-1 max-h-64 overflow-auto rounded-md border bg-muted/30 p-2 font-mono text-[11px] whitespace-pre-wrap">
                    {step.output}
                  </pre>
                </details>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function PipelineRunsSheet({
  open,
  onOpenChange,
  pipeline,
  initialRunId,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  pipeline: Pipeline | null;
  initialRunId?: string | null;
}) {
  const [nonce, setNonce] = useState(0);
  const { t } = useI18n();
  const [runId, setRunId] = useState<string | null>(null);
  const runs = useResource(
    () => (pipeline ? api.listPipelineRuns(pipeline.id, 50) : Promise.resolve<PipelineRun[]>([])),
    [pipeline?.id, nonce, open],
  );

  useEffect(() => {
    if (open) setRunId(initialRunId ?? null);
  }, [open, initialRunId, pipeline?.id]);

  return (
    <Sheet
      open={open}
      onOpenChange={(value) => {
        onOpenChange(value);
        if (!value) setRunId(null);
      }}
    >
      <SheetContent className="w-full sm:max-w-2xl">
        <SheetHeader>
          <SheetTitle>{runId ? t("pipelines.run.detailTitle") : t("pipelines.runsTitle")}</SheetTitle>
          <SheetDescription>
            {pipeline ? (
              <>
                <span className="font-mono">{pipeline.name}</span>,{" "}
                {t("pipelines.newestFirst")}
              </>
            ) : (
              t("pipelines.pickPipeline")
            )}
          </SheetDescription>
        </SheetHeader>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4">
          {pipeline && runId ? (
            <RunDetailView
              pipelineId={pipeline.id}
              runId={runId}
              onBack={() => setRunId(null)}
              onCancelled={() => setNonce((value) => value + 1)}
            />
          ) : runs.error ? (
            <ErrorAlert error={runs.error} onRetry={runs.reload} />
          ) : runs.loading && !runs.data ? (
            <div className="space-y-2">
              {Array.from({ length: 4 }).map((_, index) => (
                <Skeleton key={index} className="h-16 w-full" />
              ))}
            </div>
          ) : (runs.data ?? []).length === 0 ? (
            <EmptyState
              icon={History}
              title={t("pipelines.run.emptyTitle")}
              description={t("pipelines.run.emptyDescription")}
            />
          ) : (
            <ul className="divide-y rounded-md border">
              {(runs.data ?? []).map((run) => (
                <li key={run.id}>
                  <button
                    type="button"
                    onClick={() => setRunId(run.id)}
                    className="flex w-full items-start justify-between gap-3 px-3 py-2.5 text-left outline-none hover:bg-muted/50 focus-visible:bg-muted/50"
                  >
                    <span className="min-w-0">
                      <span className="block font-mono text-xs">{run.id}</span>
                      <span className="block text-xs text-muted-foreground">
                        {run.started_at ? formatDateTime(run.started_at) : t("status.pending")}{" "}
                        · {run.trigger}
                      </span>
                    </span>
                    <span className="flex shrink-0 items-center gap-2">
                      <OpsBadge status={run.status} />
                      <span className="text-xs text-muted-foreground">›</span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          {pipeline && !runId ? (
            <div className="mt-3 flex justify-end">
              <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
                <RefreshCw aria-hidden className={runs.refreshing ? "animate-spin" : undefined} />
                {t("common.refresh")}
              </Button>
            </div>
          ) : null}
        </div>
      </SheetContent>
    </Sheet>
  );
}

export function PipelinesPage() {
  const { t, lang } = useI18n();
  const admin = useIdentity()?.role === "admin";
  const [nonce, setNonce] = useState(0);
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<Pipeline | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<Pipeline | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<ApiError | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);
  const [runningId, setRunningId] = useState<string | null>(null);
  const [runsPipeline, setRunsPipeline] = useState<Pipeline | null>(null);
  const [initialRunId, setInitialRunId] = useState<string | null>(null);

  const pipelines = useResource(() => api.listPipelines(), [nonce]);
  const rows = pipelines.data ?? [];
  const stepInstances = useMemo(
    () => new Set(rows.flatMap((pipeline) => pipeline.steps.map((step) => step.instance))),
    [rows],
  );

  async function run(pipeline: Pipeline) {
    setRunningId(pipeline.id);
    setActionError(null);
    try {
      const run = await api.runPipeline(pipeline.id);
      toast.success(
        t("pipelines.run.started", { name: pipeline.name, status: t(`status.${run.status}`) }),
      );
      setRunsPipeline(pipeline);
      setInitialRunId(run.id);
      setNonce((value) => value + 1);
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setRunningId(null);
    }
  }

  async function remove() {
    if (!deleteTarget) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      await api.deletePipeline(deleteTarget.id);
      toast.success(t("pipelines.deleted", { name: deleteTarget.name }));
      setDeleteTarget(null);
      setNonce((value) => value + 1);
    } catch (cause) {
      setDeleteError(toApiError(cause));
    } finally {
      setDeleting(false);
    }
  }

  return (
    <>
      <PageHeader
        title={t("nav.pipelines")}
        description={
          pipelines.data
            ? t("pipelines.summary", {
                count: rows.length,
                instances: stepInstances.size,
              })
            : t("pipelines.loading")
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={pipelines.refreshing ? "animate-spin" : undefined} />
              {t("common.refresh")}
            </Button>
            {admin ? (
              <Button
                size="sm"
                onClick={() => {
                  setEditing(null);
                  setFormOpen(true);
                }}
              >
                <Plus aria-hidden />
                {t("pipelines.new")}
              </Button>
            ) : null}
          </>
        }
      />

      {actionError ? <ErrorAlert error={actionError} /> : null}
      {pipelines.error ? <ErrorAlert error={pipelines.error} onRetry={pipelines.reload} /> : null}

      <div className="rounded-md border">
        {pipelines.loading && !pipelines.data ? (
          <div className="space-y-2 p-4">
            {Array.from({ length: 4 }).map((_, index) => (
              <Skeleton key={index} className="h-14 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={Workflow}
            title={t("pipelines.emptyTitle")}
            description={t("pipelines.emptyDescription")}
            action={
              admin ? (
                <Button
                  size="sm"
                  onClick={() => {
                    setEditing(null);
                    setFormOpen(true);
                  }}
                >
                  <Plus aria-hidden />
                  {t("pipelines.new")}
                </Button>
              ) : undefined
            }
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("pipelines.pipeline")}</TableHead>
                    <TableHead className="w-[150px]">{t("pipelines.run.trigger")}</TableHead>
                    <TableHead className="w-[110px]">{t("pipelines.steps")}</TableHead>
                    <TableHead className="w-[120px]">{t("common.updated")}</TableHead>
                    <TableHead className="w-[220px]">
                      <span className="sr-only">{t("common.actions")}</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((pipeline) => (
                    <TableRow key={pipeline.id}>
                      <TableCell>
                        <span className="block text-sm font-medium">{pipeline.name}</span>
                        <span className="block font-mono text-xs text-muted-foreground">
                          {pipeline.id}
                        </span>
                      </TableCell>
                      <TableCell>
                        <span className="flex flex-wrap items-center gap-1">
                          <Badge variant="outline" className="font-mono text-xs font-normal">
                            {pipeline.trigger}
                          </Badge>
                          {pipeline.cron ? (
                            <span className="font-mono text-[11px] text-muted-foreground">
                              {pipeline.cron}
                            </span>
                          ) : null}
                        </span>
                      </TableCell>
                      <TableCell>
                        <span className="text-sm">{pipeline.steps.length}</span>
                        <span className="block font-mono text-[11px] text-muted-foreground">
                          {pipeline.steps.map((step) => step.instance).join(", ")}
                        </span>
                      </TableCell>
                      <TableCell className="text-xs text-muted-foreground">
                        {formatRelative(pipeline.updated_at, lang)}
                      </TableCell>
                      <TableCell>
                        <div className="flex justify-end gap-1">
                          {admin ? (
                            <>
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                aria-label={t("pipelines.runAria", { name: pipeline.name })}
                                title={t("pipelines.runAria", { name: pipeline.name })}
                                disabled={!pipeline.enabled || runningId === pipeline.id}
                                onClick={() => void run(pipeline)}
                              >
                                {runningId === pipeline.id ? (
                                  <Loader2 aria-hidden className="animate-spin" />
                                ) : (
                                  <Play aria-hidden />
                                )}
                              </Button>
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                aria-label={t("deployments.editWindowAria", { name: pipeline.name })}
                                title={t("deployments.editWindowAria", { name: pipeline.name })}
                                onClick={() => {
                                  setEditing(pipeline);
                                  setFormOpen(true);
                                }}
                              >
                                <Pencil aria-hidden />
                              </Button>
                            </>
                          ) : null}
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            aria-label={t("pipelines.runsAria", { name: pipeline.name })}
                            title={t("pipelines.runsAria", { name: pipeline.name })}
                            onClick={() => {
                              setInitialRunId(null);
                              setRunsPipeline(pipeline);
                            }}
                          >
                            <History aria-hidden />
                          </Button>
                          {admin ? (
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label={t("deployments.deleteWindowAria", { name: pipeline.name })}
                              title={t("deployments.deleteWindowAria", { name: pipeline.name })}
                              onClick={() => {
                                setDeleteError(null);
                                setDeleteTarget(pipeline);
                              }}
                            >
                              <Trash2 aria-hidden />
                            </Button>
                          ) : null}
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((pipeline) => (
                <div key={pipeline.id} className="space-y-3 p-4">
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0">
                      <p className="text-sm font-medium">{pipeline.name}</p>
                      <p className="font-mono text-xs text-muted-foreground">{pipeline.id}</p>
                    </div>
                    <Badge variant="outline" className="font-mono text-xs font-normal">
                      {pipeline.trigger}
                    </Badge>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {t("pipelines.mobileSteps", {
                      count: pipeline.steps.length,
                      instances: [...new Set(pipeline.steps.map((step) => step.instance))].join(
                        ", ",
                      ),
                    })}
                  </p>
                  {pipeline.cron ? (
                    <p className="font-mono text-xs text-muted-foreground">{pipeline.cron}</p>
                  ) : null}
                  <div className="flex flex-wrap justify-end gap-1">
                    {admin ? (
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={!pipeline.enabled || runningId === pipeline.id}
                        onClick={() => void run(pipeline)}
                      >
                        {runningId === pipeline.id ? (
                          <Loader2 aria-hidden className="animate-spin" />
                        ) : (
                          <Play aria-hidden />
                        )}
                        {t("common.runNow")}
                      </Button>
                    ) : null}
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        setInitialRunId(null);
                        setRunsPipeline(pipeline);
                      }}
                    >
                      {t("pipelines.runs")}
                    </Button>
                    {admin ? (
                      <>
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            setEditing(pipeline);
                            setFormOpen(true);
                          }}
                        >
                          {t("common.edit")}
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            setDeleteError(null);
                            setDeleteTarget(pipeline);
                          }}
                        >
                          {t("common.delete")}
                        </Button>
                      </>
                    ) : null}
                  </div>
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      <PipelineFormDialog
        open={formOpen}
        onOpenChange={setFormOpen}
        pipeline={editing}
        onSaved={() => setNonce((value) => value + 1)}
      />
      <PipelineRunsSheet
        open={runsPipeline !== null}
        onOpenChange={(open) => {
          if (!open) {
            setRunsPipeline(null);
            setInitialRunId(null);
          }
        }}
        pipeline={runsPipeline}
        initialRunId={initialRunId}
      />
      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        title={t("pipelines.deleteTitle")}
        description={
          deleteTarget
            ? t("pipelines.deleteDescription", { name: deleteTarget.name })
            : undefined
        }
        confirmLabel={t("pipelines.deleteTitle")}
        destructive
        pending={deleting}
        error={deleteError}
        onConfirm={() => void remove()}
      />
    </>
  );
}
