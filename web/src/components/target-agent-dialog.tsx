import { useEffect, useRef, useState } from "react";
import { CheckCircle2, Loader2, Send, XCircle } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
import type { InstanceSummary } from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { ErrorAlert } from "@/components/error-alert";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

export type AttachTarget =
  | { kind: "skill"; name: string }
  | { kind: "connector"; id: string; name: string };

interface AttachReport {
  id: string;
  ok: boolean;
  /** A 409 on a connector means it was already attached. */
  duplicate?: boolean;
  error?: string;
}

/**
 * Attach an installed skill or a deployed connector to several agents at once.
 * A skill replaces the mount list while preserving every existing pin; a
 * connector attach that answers 409 counts as already attached, never as a
 * failure. The dialog stays open on a per-agent report.
 */
export function TargetAgentDialog({
  open,
  onOpenChange,
  target,
  onAttached,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  target: AttachTarget | null;
  onAttached?: () => void;
}) {
  const { t } = useI18n();
  // `target` is cleared before the exit animation ends; keep the last value so
  // the closing frame keeps its title and row labels.
  const lastTarget = useRef<AttachTarget | null>(null);
  if (target) lastTarget.current = target;
  const shownTarget = target ?? lastTarget.current;
  const [instances, setInstances] = useState<InstanceSummary[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<ApiError | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [pending, setPending] = useState(false);
  const [report, setReport] = useState<AttachReport[] | null>(null);

  useEffect(() => {
    if (!open) {
      setInstances(null);
      setSelected(new Set());
      setPending(false);
      setReport(null);
      setLoadError(null);
      setLoading(false);
      return;
    }
    let alive = true;
    setInstances(null);
    setSelected(new Set());
    setPending(false);
    setReport(null);
    setLoadError(null);
    setLoading(true);
    api.listInstances().then(
      (rows) => {
        if (!alive) return;
        setInstances(rows);
        setLoading(false);
      },
      (cause: unknown) => {
        if (!alive) return;
        setLoadError(toApiError(cause));
        setLoading(false);
      },
    );
    return () => {
      alive = false;
    };
  }, [open, target, reloadKey]);

  const rows = instances ?? [];
  const attached = (instance: InstanceSummary): boolean =>
    shownTarget?.kind === "skill"
      ? Boolean(instance.skills?.some((ref) => ref.name === shownTarget.name))
      : false;
  const selectable = rows.filter((instance) => !attached(instance));

  function toggle(id: string) {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function submit() {
    if (!target || selected.size === 0) return;
    setPending(true);
    const results: AttachReport[] = [];
    for (const id of selected) {
      const instance = rows.find((row) => row.id === id);
      try {
        if (target.kind === "skill") {
          const mounts = (instance?.skills ?? []).map((ref) => ({
            name: ref.name,
            version: ref.version,
          }));
          if (!mounts.some((ref) => ref.name === target.name)) {
            mounts.push({ name: target.name, version: null });
          }
          await api.patchInstance(id, { skills: mounts });
        } else {
          await api.attachInstanceMcpServer(id, target.id);
        }
        results.push({ id, ok: true });
      } catch (cause) {
        const failure = toApiError(cause);
        if (target.kind === "connector" && failure.status === 409) {
          results.push({ id, ok: true, duplicate: true });
        } else {
          results.push({ id, ok: false, error: apiErrorMessage(failure, t) });
        }
      }
    }
    setReport(results);
    const failed = results.filter((entry) => !entry.ok).length;
    if (failed === 0) {
      toast.success(
        t(
          results.length === 1
            ? "library.attach.toastDoneOne"
            : "library.attach.toastDoneMany",
          { name: target.name, count: results.length },
        ),
      );
    } else {
      toast.warning(
        t("library.attach.toastPartial", {
          failed,
          total: results.length,
          name: target.name,
        }),
      );
    }
    setPending(false);
    onAttached?.();
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh_-_2rem)] grid-rows-[auto_minmax(0,1fr)] sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {t("library.attach.title", { name: shownTarget?.name ?? "" })}
          </DialogTitle>
          <DialogDescription>
            {shownTarget?.kind === "connector"
              ? t("library.attach.descriptionConnector")
              : t("library.attach.descriptionSkill")}
          </DialogDescription>
        </DialogHeader>

        {report === null ? (
          <>
            <div className="-mr-2 min-h-0 space-y-4 overflow-y-auto pr-2">
              <div className="flex items-center justify-between gap-2">
                <p className="text-sm font-medium">{t("nav.instances")}</p>
                <div className="flex items-center gap-2 text-xs">
                  <button
                    type="button"
                    className="underline-offset-4 hover:underline disabled:opacity-50"
                    disabled={selectable.length === 0 || selected.size === selectable.length}
                    onClick={() => setSelected(new Set(selectable.map((row) => row.id)))}
                  >
                    {t("personas.apply.selectAll")}
                  </button>
                  <span aria-hidden className="text-muted-foreground">
                    ·
                  </span>
                  <button
                    type="button"
                    className="underline-offset-4 hover:underline disabled:opacity-50"
                    disabled={selected.size === 0}
                    onClick={() => setSelected(new Set())}
                  >
                    {t("personas.apply.clear")}
                  </button>
                </div>
              </div>

              {loading ? (
                <div className="space-y-2 rounded-md border p-3" aria-busy="true">
                  <span role="status" className="sr-only">
                    {t("library.attach.loading")}
                  </span>
                  {Array.from({ length: 3 }).map((_, index) => (
                    <Skeleton key={index} className="h-9 w-full" />
                  ))}
                </div>
              ) : loadError ? (
                <ErrorAlert error={loadError} onRetry={() => setReloadKey((value) => value + 1)} />
              ) : rows.length === 0 ? (
                <p className="rounded-md border px-3 py-4 text-sm text-muted-foreground">
                  {t("library.attach.noInstances")}
                </p>
              ) : (
                <div className="divide-y rounded-md border">
                  {rows.map((instance) => {
                    const already = attached(instance);
                    const checked = selected.has(instance.id);
                    return (
                      <label
                        key={instance.id}
                        className={cn(
                          "flex items-start gap-3 px-3 py-2.5",
                          already ? "opacity-70" : "cursor-pointer",
                          checked ? "bg-muted/40" : !already && "hover:bg-muted/20",
                        )}
                      >
                        <Checkbox
                          checked={checked || already}
                          disabled={already || pending}
                          className="mt-0.5"
                          onCheckedChange={() => toggle(instance.id)}
                          aria-label={t("library.attach.applyTo", {
                            name: shownTarget?.name ?? "",
                            agent: instance.name,
                          })}
                        />
                        <span className="min-w-0 flex-1 space-y-1">
                          <span className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                            <span className="truncate text-sm font-medium">{instance.name}</span>
                            <span className="font-mono text-xs text-muted-foreground">
                              {instance.id}
                            </span>
                          </span>
                          <span className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
                            {already ? (
                              <Badge
                                variant="secondary"
                                className="text-[11px] font-normal"
                              >
                                {t("library.attach.alreadyAttached")}
                              </Badge>
                            ) : null}
                            {instance.status !== "running" ? (
                              <Badge
                                variant="outline"
                                className="font-mono text-[11px] font-normal text-muted-foreground"
                              >
                                {t(`status.${instance.status}`)}
                              </Badge>
                            ) : null}
                          </span>
                        </span>
                      </label>
                    );
                  })}
                </div>
              )}
            </div>

            <DialogFooter className="border-t pt-4">
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={pending}
              >
                {t("common.cancel")}
              </Button>
              <Button
                type="button"
                onClick={() => void submit()}
                disabled={pending || selected.size === 0}
              >
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Send aria-hidden />}
                {t("library.attach.confirm", { count: selected.size || "…" })}
              </Button>
            </DialogFooter>
          </>
        ) : (
          <div className="min-h-0 space-y-4 overflow-y-auto">
            <div className="space-y-2">
              <p className="text-sm font-medium">{t("library.attach.report")}</p>
              <ul className="divide-y rounded-md border">
                {report.map((entry) => {
                  const instance = rows.find((row) => row.id === entry.id);
                  return (
                    <li key={entry.id} className="space-y-1 px-3 py-2.5">
                      <div className="flex flex-wrap items-center justify-between gap-2">
                        <span className="min-w-0 truncate text-sm">
                          <span className="font-medium">{instance?.name ?? entry.id}</span>{" "}
                          <span className="font-mono text-xs text-muted-foreground">{entry.id}</span>
                        </span>
                        <span className="flex items-center gap-1.5">
                          {entry.ok ? (
                            <CheckCircle2 aria-hidden className="size-3.5 text-emerald-500" />
                          ) : (
                            <XCircle aria-hidden className="size-3.5 text-destructive" />
                          )}
                          <Badge
                            variant={entry.ok ? "secondary" : "destructive"}
                            className="text-xs font-normal"
                          >
                            {entry.ok
                              ? t("library.attach.statusOk")
                              : t("library.attach.statusFailed")}
                          </Badge>
                        </span>
                      </div>
                      {entry.error ? (
                        <p className="font-mono text-xs break-words whitespace-pre-wrap text-destructive">
                          {entry.error}
                        </p>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
              {report.some((entry) => !entry.ok) ? (
                <Alert className="border-amber-600/40 bg-amber-50/40 dark:bg-amber-950/20">
                  <AlertTitle>{t("library.attach.partialTitle")}</AlertTitle>
                  <AlertDescription>
                    <p className="text-sm text-muted-foreground">
                      {t("library.attach.partialBody")}
                    </p>
                  </AlertDescription>
                </Alert>
              ) : null}
            </div>

            <DialogFooter>
              <Button autoFocus onClick={() => onOpenChange(false)}>
                {t("common.done")}
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
