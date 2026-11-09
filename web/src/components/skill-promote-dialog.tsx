import { useEffect, useState } from "react";
import { CheckCircle2, Loader2, RotateCw, TriangleAlert, XCircle } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { InstanceSummary, SkillDetail, SkillPromoteResult } from "@/api/types";
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
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { ErrorAlert } from "@/components/error-alert";
import { RichText } from "@/components/rich-text";
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";

function followerOf(instance: InstanceSummary, name: string): boolean {
  return (instance.skills ?? []).some((skill) => skill.name === name && !skill.version);
}

function pinnedOf(instance: InstanceSummary, name: string): boolean {
  return (instance.skills ?? []).some((skill) => skill.name === name && !!skill.version);
}

/**
 * Move the `latest` pointer. The controller restarts each instance that follows
 * latest, one at a time, and answers with one row per instance; pinned
 * instances are left alone. The report stays on screen so a failed restart can
 * be read, not just counted.
 */
export function SkillPromoteDialog({
  skill,
  open,
  onOpenChange,
  onPromoted,
}: {
  skill: SkillDetail;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onPromoted: (latest: string) => void;
}) {
  const { t } = useI18n();
  const [version, setVersion] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [result, setResult] = useState<SkillPromoteResult | null>(null);
  const [instances, setInstances] = useState<InstanceSummary[] | null>(null);
  const [instancesError, setInstancesError] = useState<ApiError | null>(null);
  const [reloadKey, setReloadKey] = useState(0);

  useEffect(() => {
    if (!open) return;
    // Default to the newest version that is not already latest (a promote can
    // also roll back), so the form is actionable for a two-version skill.
    const newestFirst = skill.versions.map((entry) => entry.version).reverse();
    setVersion(newestFirst.find((candidate) => candidate !== skill.latest) ?? newestFirst[0] ?? "");
    setPending(false);
    setError(null);
    setResult(null);
  }, [open, skill.latest, skill.versions]);

  useEffect(() => {
    if (!open) return;
    let alive = true;
    setInstances(null);
    setInstancesError(null);
    api.listInstances().then(
      (list) => {
        if (alive) setInstances(list);
      },
      (cause: unknown) => {
        if (alive) setInstancesError(toApiError(cause));
      },
    );
    return () => {
      alive = false;
    };
  }, [open, reloadKey]);

  const followers = (instances ?? []).filter((instance) => followerOf(instance, skill.name));
  const pinned = (instances ?? []).filter((instance) => pinnedOf(instance, skill.name));
  const alreadyLatest = !!version && version === skill.latest;
  const options = [...skill.versions].reverse();

  async function confirm() {
    if (!version) return;
    setPending(true);
    setError(null);
    try {
      const response = await api.promoteSkill(skill.name, version);
      setResult(response);
      const failed = response.results.filter((entry) => entry.status === "failed").length;
      const restarted = response.results.length - failed;
      if (failed === 0) {
        toast.success(
          restarted === 0
            ? t("skills.promote.toastOkNoRestart", { version: response.latest })
            : t("skills.promote.toastOk", { version: response.latest, restarted }),
        );
      } else {
        toast.error(
          t("skills.promote.toastFailed", { failed, total: response.results.length }),
        );
      }
      onPromoted(response.latest);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("skills.promote.title", { name: skill.name })}</DialogTitle>
          <DialogDescription>
            <RichText text={t("skills.promote.description")} />
          </DialogDescription>
        </DialogHeader>

        {result ? (
          <div className="space-y-3">
            <p className="flex flex-wrap items-center gap-2 text-sm">
              <CheckCircle2 aria-hidden className="size-4 text-emerald-600 dark:text-emerald-400" />
              <span>
                {t("skills.promote.nowLatest")}{" "}
                <span className="font-mono font-medium">{result.latest}</span>
              </span>
              <Badge variant="outline" className="font-mono text-[11px] font-normal">
                {t("skills.promote.restartedCount", {
                  count: result.results.filter((entry) => entry.status !== "failed").length,
                })}
                {result.results.some((entry) => entry.status === "failed")
                  ? t("skills.promote.failedSuffix", {
                      count: result.results.filter((entry) => entry.status === "failed").length,
                    })
                  : ""}
              </Badge>
            </p>

            {result.results.length === 0 ? (
              <p className="rounded-md border px-3 py-2.5 text-sm text-muted-foreground">
                <RichText text={t("skills.promote.noFollowers")} />
              </p>
            ) : (
              <ul className="divide-y rounded-md border">
                {result.results.map((entry) => {
                  const failed = entry.status === "failed";
                  return (
                    <li key={entry.id} className="space-y-1 px-3 py-2.5">
                      <div className="flex flex-wrap items-center gap-2">
                        <a
                          href={hrefFor(`/instances/${entry.id}`)}
                          className="min-w-0 flex-1 truncate text-sm font-medium underline-offset-4 hover:underline"
                        >
                          {entry.id}
                        </a>
                        <Badge
                          variant={failed ? "destructive" : "secondary"}
                          className="shrink-0 gap-1 font-mono text-[11px] font-normal"
                        >
                          {failed ? <XCircle aria-hidden /> : <CheckCircle2 aria-hidden />}
                          {failed
                            ? t("skills.promote.statusFailed")
                            : t("skills.promote.statusRestarted")}
                        </Badge>
                      </div>
                      {entry.error ? (
                        <p className="font-mono text-xs break-words text-destructive">
                          {entry.error}
                        </p>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
            )}

            {pinned.length > 0 ? (
              <p className="text-xs text-muted-foreground">
                {t(
                  pinned.length === 1
                    ? "skills.promote.pinnedOne"
                    : "skills.promote.pinnedMany",
                  { count: pinned.length },
                )}
              </p>
            ) : null}
          </div>
        ) : (
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="skill-promote-version">{t("skills.promote.versionLabel")}</Label>
              <Select value={version} onValueChange={setVersion} disabled={pending}>
                <SelectTrigger id="skill-promote-version" className="w-full font-mono">
                  <SelectValue placeholder={t("skills.promote.versionPlaceholder")} />
                </SelectTrigger>
                <SelectContent>
                  {options.map((entry) => (
                    <SelectItem key={entry.version} value={entry.version}>
                      {entry.version}
                      {entry.version === skill.latest
                        ? t("skills.promote.currentLatest")
                        : ""}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {alreadyLatest ? (
                <p className="flex items-start gap-1.5 text-xs text-amber-700 dark:text-amber-400">
                  <TriangleAlert aria-hidden className="mt-0.5 size-3.5 shrink-0" />
                  {t("skills.promote.alreadyLatest")}
                </p>
              ) : null}
            </div>

            <div className="space-y-2">
              <p className="text-sm font-medium">
                {instances === null
                  ? t("skills.promote.loadingInstances")
                  : t(
                      followers.length === 1
                        ? "skills.promote.followersOne"
                        : "skills.promote.followersMany",
                      { count: followers.length },
                    )}
              </p>
              {instancesError ? (
                <ErrorAlert error={instancesError} onRetry={() => setReloadKey((value) => value + 1)} />
              ) : instances === null ? (
                <Skeleton className="h-16 w-full rounded-md" />
              ) : followers.length === 0 ? (
                <p className="rounded-md border px-3 py-2.5 text-sm text-muted-foreground">
                  {t("skills.promote.noFollowersShort")}
                </p>
              ) : (
                <ul className="max-h-48 divide-y overflow-auto rounded-md border">
                  {followers.map((instance) => (
                    <li key={instance.id} className="flex items-center gap-2 px-3 py-2">
                      <a
                        href={hrefFor(`/instances/${instance.id}`)}
                        className="min-w-0 flex-1 truncate text-sm underline-offset-4 hover:underline"
                      >
                        {instance.name}
                      </a>
                      <span className="shrink-0 font-mono text-xs text-muted-foreground">
                        {instance.id}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
              {pinned.length > 0 ? (
                <p className="text-xs text-muted-foreground">
                  {t(
                    pinned.length === 1
                      ? "skills.promote.pinnedListOne"
                      : "skills.promote.pinnedListMany",
                    { count: pinned.length },
                  )}{" "}
                  {pinned
                    .map(
                      (instance) =>
                        `${instance.id}@${
                          (instance.skills ?? []).find((entry) => entry.name === skill.name)
                            ?.version ?? "?"
                        }`,
                    )
                    .join(", ")}
                  .
                </p>
              ) : null}
            </div>

            {error ? <ErrorAlert error={error} /> : null}
          </div>
        )}

        <DialogFooter>
          {result ? (
            <Button onClick={() => onOpenChange(false)}>{t("common.close")}</Button>
          ) : (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
                {t("common.cancel")}
              </Button>
              <Button
                onClick={() => void confirm()}
                disabled={pending || !version}
              >
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <RotateCw aria-hidden />}
                {t("skills.promote.confirm", { count: followers.length })}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
