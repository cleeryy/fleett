import { useEffect, useMemo, useRef, useState } from "react";
import {
  CheckCircle2,
  GitBranch,
  History as HistoryIcon,
  Loader2,
  RefreshCw,
  RotateCcw,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { CommitInfo, GitStatus } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { RichText } from "@/components/rich-text";
import { PatchView } from "@/components/diff-view";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { summarizePatch } from "@/lib/diff";
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

function statusText(
  status: GitStatus,
  t: (key: string, params?: Record<string, string | number>) => string,
): string {
  const parts: string[] = [];
  if (status.modified.length > 0) {
    parts.push(t("history.modified", { count: status.modified.length }));
  }
  if (status.untracked.length > 0) {
    parts.push(t("history.newFiles", { count: status.untracked.length }));
  }
  return parts.length > 0 ? parts.join(" · ") : t("history.clean");
}

function CommitRow({
  commit,
  selected,
  onSelect,
}: {
  commit: CommitInfo;
  selected: boolean;
  onSelect: () => void;
}) {
  const { t, lang } = useI18n();
  return (
    <button
      type="button"
      aria-current={selected ? "true" : undefined}
      className={cn(
        "w-full space-y-1 px-3 py-2.5 text-left hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:-outline-offset-2",
        selected && "bg-muted",
      )}
      onClick={onSelect}
    >
      <span className="flex items-baseline gap-2">
        <span className="shrink-0 font-mono text-[11px] text-muted-foreground">{commit.short}</span>
        <span className="min-w-0 flex-1 truncate text-sm">{commit.message}</span>
      </span>
      <span className="flex flex-wrap items-center gap-x-1.5 text-[11px] text-muted-foreground">
        <span className="max-w-40 truncate">{commit.author}</span>
        <span aria-hidden>·</span>
        <time dateTime={commit.at} title={formatDateTime(commit.at)}>
          {formatRelative(commit.at, lang)}
        </time>
        {commit.paths.length > 1 ? (
          <>
            <span aria-hidden>·</span>
            <span>{t("history.fileCount", { count: commit.paths.length })}</span>
          </>
        ) : null}
      </span>
    </button>
  );
}

export function InstanceHistory({ id, canEdit }: { id: string; canEdit: boolean }) {
  const { t, lang } = useI18n();
  const [nonce, setNonce] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [revertOpen, setRevertOpen] = useState(false);
  const [restart, setRestart] = useState(false);
  const [reverting, setReverting] = useState(false);
  const [revertError, setRevertError] = useState<ApiError | null>(null);
  const pendingSelection = useRef<string | null>(null);

  const log = useResource(() => api.gitLog(id, { limit: 50 }), [id, nonce]);
  const status = useResource(() => api.gitStatus(id), [id, nonce]);
  const commits = useMemo(() => log.data?.commits ?? [], [log.data]);
  const selected = commits.find((commit) => commit.id === selectedId) ?? null;

  const diff = useResource(
    () => (selectedId ? api.gitDiff(id, { to: selectedId }) : Promise.resolve(null)),
    [id, selectedId, nonce],
  );

  useEffect(() => {
    if (commits.length === 0) {
      setSelectedId(null);
      return;
    }
    const preferred = pendingSelection.current;
    if (preferred && commits.some((commit) => commit.id === preferred)) {
      pendingSelection.current = null;
      setSelectedId(preferred);
      return;
    }
    setSelectedId((current) =>
      current && commits.some((commit) => commit.id === current) ? current : commits[0].id,
    );
  }, [commits]);

  useEffect(() => {
    setSelectedId(null);
    setRevertOpen(false);
    setRevertError(null);
    pendingSelection.current = null;
  }, [id]);

  const summary = useMemo(() => summarizePatch(diff.data?.diff ?? ""), [diff.data]);
  const drift = status.data
    ? status.data.modified.length + status.data.untracked.length > 0
    : false;

  function refresh() {
    setNonce((value) => value + 1);
  }

  async function onRevert() {
    if (!selected) return;
    setReverting(true);
    setRevertError(null);
    try {
      const result = await api.gitRevert(id, { commit: selected.id, restart });
      if (result.commit) {
        pendingSelection.current = result.commit.id;
        toast.success(
          t("history.restored", { count: result.paths.length, commit: selected.short }),
          {
            description: result.restarted
              ? t("history.restoredRestarted", { commit: result.commit.short })
              : t("history.restoredRecorded", { commit: result.commit.short }),
          },
        );
      } else {
        toast.info(t("history.alreadyMatches"));
      }
      setRevertOpen(false);
      setNonce((value) => value + 1);
    } catch (cause) {
      setRevertError(toApiError(cause));
    } finally {
      setReverting(false);
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="flex min-w-0 flex-wrap items-center gap-2 text-sm" aria-live="polite">
          <GitBranch aria-hidden className="size-4 shrink-0 text-muted-foreground" />
          {status.loading && !status.data ? (
            <span className="text-muted-foreground">{t("history.reading")}</span>
          ) : status.data && drift ? (
            <>
              <Badge variant="secondary" className="font-normal">
                {statusText(status.data, t)}
              </Badge>
              <span className="text-xs text-muted-foreground">{t("history.sinceLastCommit")}</span>
            </>
          ) : (
            <span className="flex items-center gap-1.5 text-muted-foreground">
              <CheckCircle2 aria-hidden className="size-3.5" />
              {t("history.clean")}
            </span>
          )}
        </div>
        <Button variant="outline" size="sm" className="ml-auto" onClick={refresh}>
          <RefreshCw aria-hidden className={log.refreshing || status.refreshing ? "animate-spin" : undefined} />
          {t("common.refresh")}
        </Button>
      </div>

      {log.loading && !log.data ? (
        <Skeleton className="h-64 w-full rounded-md" />
      ) : log.error && !log.data ? (
        <ErrorAlert error={log.error} onRetry={log.reload} />
      ) : commits.length === 0 ? (
        <div className="rounded-md border">
          <EmptyState
            icon={HistoryIcon}
            title={t("history.emptyTitle")}
            description={t("history.emptyDescription")}
            action={
              <Button variant="outline" size="sm" onClick={refresh}>
                <RefreshCw aria-hidden />
                {t("common.refresh")}
              </Button>
            }
          />
        </div>
      ) : (
        <div className="grid gap-3 lg:grid-cols-[320px_minmax(0,1fr)]">
          <div className="overflow-hidden rounded-md border">
            <div className="border-b bg-muted/30 px-3 py-1.5 text-xs text-muted-foreground">
              {t("history.commitCount", { count: commits.length })}
            </div>
            <ul className="max-h-[65vh] divide-y overflow-auto">
              {commits.map((commit) => (
                <li key={commit.id}>
                  <CommitRow
                    commit={commit}
                    selected={commit.id === selectedId}
                    onSelect={() => setSelectedId(commit.id)}
                  />
                </li>
              ))}
            </ul>
          </div>

          <div className="min-w-0 overflow-hidden rounded-md border">
            {selected ? (
              <>
                <div className="space-y-2 border-b px-3 py-3">
                  <p className="text-sm font-medium break-words">{selected.message}</p>
                  <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
                    <span className="text-foreground">{selected.author}</span>
                    <span className="font-mono break-all">&lt;{selected.email}&gt;</span>
                    <span aria-hidden>·</span>
                    <time dateTime={selected.at} title={formatRelative(selected.at, lang)}>
                      {formatDateTime(selected.at)}
                    </time>
                    <span aria-hidden>·</span>
                    <span className="font-mono">{selected.short}</span>
                  </div>
                  {selected.paths.length > 0 ? (
                    <div className="flex flex-wrap gap-1 pt-1">
                      {selected.paths.map((path) => (
                        <Badge key={path} variant="outline" className="font-mono text-[11px] font-normal">
                          {path}
                        </Badge>
                      ))}
                    </div>
                  ) : (
                    <p className="text-xs text-muted-foreground">
                      {t("history.changedNoFile")}
                    </p>
                  )}
                </div>

                <div className="flex flex-wrap items-center justify-between gap-2 border-b px-3 py-2">
                  <span className="font-mono text-[11px] text-muted-foreground">
                    {diff.loading || diff.refreshing
                      ? t("history.loadingPatch")
                      : summary.files > 0
                        ? t("history.summary", {
                            count: summary.files,
                            added: summary.added,
                            removed: summary.removed,
                          })
                        : t("history.noTextChange")}
                  </span>
                  {canEdit ? (
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={selected.paths.length === 0}
                      onClick={() => {
                        setRevertError(null);
                        setRestart(false);
                        setRevertOpen(true);
                      }}
                    >
                      <RotateCcw aria-hidden />
                      {t("history.restoreVersion")}
                    </Button>
                  ) : null}
                </div>

                {diff.loading || diff.refreshing ? (
                  <Skeleton className="m-3 h-48 rounded-md" />
                ) : diff.error ? (
                  <div className="p-3">
                    <ErrorAlert error={diff.error} onRetry={diff.reload} />
                  </div>
                ) : (
                  <PatchView patch={diff.data?.diff ?? ""} empty={t("files.emptyPatch")} />
                )}
              </>
            ) : (
              <div className="p-3">
                <Skeleton className="h-48 rounded-md" />
              </div>
            )}
          </div>
        </div>
      )}

      <Dialog open={revertOpen} onOpenChange={setRevertOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {t("history.restoreTitle", { commit: selected?.short ?? "" })}
            </DialogTitle>
            <DialogDescription>{t("history.restoreDescription")}</DialogDescription>
          </DialogHeader>

          <div className="min-w-0 space-y-3">
            <div className="overflow-hidden rounded-md border">
              <p className="border-b bg-muted/30 px-3 py-1.5 text-xs text-muted-foreground">
                {selected
                  ? t("history.pathsInCommit", { count: selected.paths.length })
                  : t("history.paths")}
              </p>
              <ul className="max-h-48 divide-y overflow-auto">
                {(selected?.paths ?? []).map((path) => (
                  <li key={path} className="px-3 py-1.5 font-mono text-xs break-all">
                    {path}
                  </li>
                ))}
              </ul>
            </div>

            <div className="flex items-start gap-3">
              <Checkbox
                id="revert-restart"
                className="mt-0.5"
                checked={restart}
                aria-describedby="revert-restart-help"
                onCheckedChange={(value) => setRestart(value === true)}
              />
              <div className="min-w-0 space-y-1">
                <Label htmlFor="revert-restart" className="cursor-pointer">
                  {t("history.restartAfter")}
                </Label>
                <p id="revert-restart-help" className="text-xs text-muted-foreground">
                  <RichText text={t("history.restartHint")} />
                </p>
              </div>
            </div>

            {revertError ? <ErrorAlert error={revertError} /> : null}
          </div>

          <DialogFooter>
            <Button variant="outline" onClick={() => setRevertOpen(false)} disabled={reverting}>
              {t("common.cancel")}
            </Button>
            <Button onClick={() => void onRevert()} disabled={reverting || !selected}>
              {reverting ? <Loader2 aria-hidden className="animate-spin" /> : <RotateCcw aria-hidden />}
              {t("history.restoreConfirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
