import { useEffect, useState } from "react";
import { ArrowRight, FileText, Loader2, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type {
  MarketplaceScriptsDetails,
  MarketplaceUpdateInfo,
  MarketplaceUpdateResult,
  SkillDetail,
  SkillFileChangeStatus,
  SkillPromoteResult,
} from "@/api/types";
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
import { Label } from "@/components/ui/label";
import { ErrorAlert } from "@/components/error-alert";
import { useI18n } from "@/lib/i18n";
import { refreshMarketplaceUpdates } from "@/lib/marketplace";
import { nextSkillVersion } from "@/lib/skills";
import { cn } from "@/lib/utils";

const CHANGE_STYLE: Record<SkillFileChangeStatus, { className: string; key: string }> = {
  added: {
    className: "border-emerald-600/40 text-emerald-700 dark:text-emerald-400",
    key: "skills.update.fileAdded",
  },
  modified: {
    className: "border-sky-600/40 text-sky-700 dark:text-sky-400",
    key: "skills.update.fileModified",
  },
  removed: {
    className: "border-destructive/40 text-destructive",
    key: "skills.update.fileRemoved",
  },
};

function PromoteReport({ report }: { report: SkillPromoteResult }) {
  const { t } = useI18n();
  return (
    <div className="space-y-2 rounded-md border p-3">
      <p className="text-sm font-medium">{t("skills.update.promoteReport")}</p>
      <p className="text-xs text-muted-foreground">
        {t("skills.update.promoted", { latest: report.latest })}
      </p>
      {report.results.length > 0 ? (
        <ul className="space-y-1">
          {report.results.map((entry) => (
            <li key={entry.id} className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
              <span className="min-w-0 flex-1 truncate font-mono">{entry.id}</span>
              <Badge
                variant="outline"
                className={cn(
                  "font-mono text-[11px] font-normal",
                  entry.status === "failed"
                    ? "border-destructive/40 text-destructive"
                    : "border-emerald-600/40 text-emerald-700 dark:text-emerald-400",
                )}
              >
                {t(
                  entry.status === "failed"
                    ? "skills.promote.statusFailed"
                    : "skills.promote.statusRestarted",
                )}
              </Badge>
              {entry.error ? (
                <span className="w-full text-[11px] break-words text-destructive">{entry.error}</span>
              ) : null}
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-xs text-muted-foreground">{t("skills.promote.noFollowers")}</p>
      )}
    </div>
  );
}

/**
 * Publish the upstream diff as a new version, then optionally promote it with
 * the same rolling restart as `POST /skills/{name}/promote`. The script
 * confirmation is repeated here because an update refuses unconfirmed scripts
 * even when the initial install was reviewed.
 */
export function SkillUpdateDialog({
  skill,
  upstream,
  followers,
  open,
  onOpenChange,
  onUpdated,
}: {
  skill: SkillDetail;
  upstream: MarketplaceUpdateInfo | null;
  followers: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onUpdated?: (result: MarketplaceUpdateResult) => void;
}) {
  const { t } = useI18n();
  const nextVersion = nextSkillVersion(skill.versions.map((entry) => entry.version));
  const [promote, setPromote] = useState(true);
  const [confirmed, setConfirmed] = useState(false);
  const [scripts, setScripts] = useState<string[]>([]);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [result, setResult] = useState<MarketplaceUpdateResult | null>(null);

  useEffect(() => {
    if (!open) return;
    setPromote(true);
    setConfirmed(false);
    setScripts([]);
    setPending(false);
    setError(null);
    setResult(null);
  }, [open]);

  async function update() {
    if (!upstream) return;
    setPending(true);
    setError(null);
    try {
      const outcome = await api.marketplaceUpdate(skill.name, {
        promote,
        ...(scripts.length > 0 ? { confirm_scripts: true } : {}),
      });
      setResult(outcome);
      refreshMarketplaceUpdates();
      onUpdated?.(outcome);
      toast.success(t("skills.update.doneToast", { name: outcome.name, version: outcome.to }));
    } catch (cause) {
      const failure = toApiError(cause);
      const details = failure.details as MarketplaceScriptsDetails | undefined;
      if (
        failure.code === "scripts_require_confirmation" &&
        Array.isArray(details?.scripts) &&
        details.scripts.length > 0
      ) {
        setScripts(details.scripts.filter((path): path is string => typeof path === "string"));
        setConfirmed(false);
      } else {
        setError(failure);
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t("skills.update.title", { name: skill.name })}</DialogTitle>
          <DialogDescription>
            {result
              ? t("skills.update.doneDescription")
              : upstream
                ? t("skills.update.description", { version: nextVersion })
                : t("skills.update.upstreamMissing")}
          </DialogDescription>
        </DialogHeader>

        {upstream && !result ? (
          <div className="space-y-3">
            <div className="space-y-1.5 rounded-md border p-3">
              <p className="flex flex-wrap items-center gap-2 font-mono text-sm">
                <span>{upstream.installed_version}</span>
                <ArrowRight aria-hidden className="size-3.5 text-muted-foreground" />
                <span className="font-medium">{upstream.upstream_version ?? nextVersion}</span>
              </p>
              <p className="font-mono text-[11px] break-all text-muted-foreground">
                {t("skills.update.source", { source: upstream.source })}
                {upstream.ref ? ` · ${upstream.ref}` : ""}
                {upstream.commit ? ` · ${upstream.commit.slice(0, 7)}` : ""}
              </p>
            </div>

            <div className="flex items-start gap-2">
              <Checkbox
                id="skill-update-promote"
                checked={promote}
                onCheckedChange={(value) => setPromote(value === true)}
              />
              <div className="space-y-0.5">
                <Label htmlFor="skill-update-promote" className="font-normal">
                  {t("skills.update.promote")}
                </Label>
                <p className="text-xs text-muted-foreground">
                  {followers === 0
                    ? t("skills.update.promoteHintNone")
                    : t(
                        followers === 1
                          ? "skills.update.promoteHintOne"
                          : "skills.update.promoteHintMany",
                        { count: followers },
                      )}
                </p>
              </div>
            </div>

            {!promote ? (
              <p className="rounded-md border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
                {t("skills.update.publishOnlyHint", {
                  version: nextVersion,
                  latest: skill.latest ?? t("skills.create.unset"),
                })}
              </p>
            ) : null}

            {scripts.length > 0 ? (
              <div
                role="alert"
                className="space-y-2 rounded-md border border-amber-600/40 bg-amber-50/50 px-3 py-2.5 dark:bg-amber-950/20"
              >
                <p className="flex items-center gap-2 text-sm font-medium text-amber-800 dark:text-amber-300">
                  <TriangleAlert aria-hidden className="size-4 shrink-0" />
                  {t("skills.update.scriptsTitle", { count: scripts.length })}
                </p>
                <ul className="space-y-1">
                  {scripts.map((path) => (
                    <li key={path} className="font-mono text-xs break-all">
                      {path}
                    </li>
                  ))}
                </ul>
                <div className="flex items-start gap-2 pt-1">
                  <Checkbox
                    id="skill-update-scripts"
                    checked={confirmed}
                    onCheckedChange={(value) => setConfirmed(value === true)}
                  />
                  <Label htmlFor="skill-update-scripts" className="font-normal leading-snug">
                    {t("skills.update.scriptsConfirm")}
                  </Label>
                </div>
              </div>
            ) : null}

            {error ? <ErrorAlert error={error} /> : null}
          </div>
        ) : null}

        {result ? (
          <div className="space-y-3">
            <div className="space-y-1 rounded-md border border-emerald-600/40 bg-emerald-50/40 p-3 dark:bg-emerald-950/20">
              <p className="text-sm font-medium">
                {t("skills.update.doneTitle", { version: result.to })}
              </p>
              <p className="flex items-center gap-2 font-mono text-xs text-muted-foreground">
                <span>{result.from}</span>
                <ArrowRight aria-hidden className="size-3" />
                <span>{result.to}</span>
                {result.commit ? (
                  <>
                    <span aria-hidden>·</span>
                    <span>{result.commit.slice(0, 7)}</span>
                  </>
                ) : null}
              </p>
            </div>

            <div className="space-y-2">
              <p className="text-sm font-medium">{t("skills.update.filesTitle")}</p>
              {result.files.length === 0 ? (
                <p className="text-sm text-muted-foreground">{t("skills.update.noFiles")}</p>
              ) : (
                <ul className="divide-y rounded-md border">
                  {result.files.map((change) => {
                    const style = CHANGE_STYLE[change.status] ?? CHANGE_STYLE.modified;
                    return (
                      <li key={change.path} className="flex items-center gap-2 px-3 py-1.5">
                        <FileText
                          aria-hidden
                          className="size-3.5 shrink-0 text-muted-foreground"
                        />
                        <span className="min-w-0 flex-1 truncate font-mono text-xs" title={change.path}>
                          {change.path}
                        </span>
                        <Badge
                          variant="outline"
                          className={cn("shrink-0 font-mono text-[10px] font-normal", style.className)}
                        >
                          {t(style.key)}
                        </Badge>
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>

            {result.promoted ? (
              <PromoteReport report={result.promoted} />
            ) : (
              <p className="rounded-md border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
                {t("skills.update.notPromoted")}
              </p>
            )}
          </div>
        ) : null}

        <DialogFooter>
          {result ? (
            <Button onClick={() => onOpenChange(false)}>{t("common.close")}</Button>
          ) : (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
                {t("common.cancel")}
              </Button>
              <Button
                onClick={() => void update()}
                disabled={pending || !upstream || (scripts.length > 0 && !confirmed)}
              >
                {pending ? (
                  <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
                ) : null}
                {pending ? t("skills.update.running") : t("skills.update.confirm")}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
