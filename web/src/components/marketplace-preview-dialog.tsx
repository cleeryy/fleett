import { useEffect, useState } from "react";
import {
  CheckCircle2,
  Download,
  FileText,
  Loader2,
  Terminal,
  TriangleAlert,
} from "lucide-react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type {
  MarketplaceEntry,
  MarketplacePreview,
  MarketplaceScriptsDetails,
  SkillDetail,
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
import { Skeleton } from "@/components/ui/skeleton";
import { CopyButton } from "@/components/copy-button";
import { ErrorAlert } from "@/components/error-alert";
import { MarkdownView } from "@/components/markdown-view";
import { formatBytes } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";
import { refreshMarketplaceUpdates } from "@/lib/marketplace";

function scriptPathsOf(error: ApiError): string[] {
  const details = error.details as MarketplaceScriptsDetails | undefined;
  if (!details || !Array.isArray(details.scripts)) return [];
  return details.scripts.filter((path): path is string => typeof path === "string");
}

/**
 * Preview before install: the file tree, the rendered SKILL.md and, when the
 * skill ships executable content, an explicit script confirmation. The
 * controller refuses an unconfirmed install, so the dialog also reacts to the
 * `409 scripts_require_confirmation` instead of trusting a stale preview.
 */
export function MarketplacePreviewDialog({
  open,
  onOpenChange,
  entry,
  canEdit,
  onInstalled,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  entry: MarketplaceEntry | null;
  canEdit: boolean;
  onInstalled?: (detail: SkillDetail) => void;
}) {
  const { t } = useI18n();
  const source = entry?.source ?? null;
  const reference = entry?.ref;
  const [preview, setPreview] = useState<MarketplacePreview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [pending, setPending] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [forcedScripts, setForcedScripts] = useState<string[]>([]);
  const [result, setResult] = useState<SkillDetail | null>(null);
  const [reloadKey, setReloadKey] = useState(0);

  useEffect(() => {
    if (!open || !source) return;
    let alive = true;
    setPreview(null);
    setError(null);
    setResult(null);
    setConfirmed(false);
    setForcedScripts([]);
    setLoading(true);
    api.marketplacePreview({ source, ...(reference ? { ref: reference } : {}) }).then(
      (value) => {
        if (!alive) return;
        setPreview(value);
        setLoading(false);
      },
      (cause: unknown) => {
        if (!alive) return;
        setError(toApiError(cause));
        setLoading(false);
      },
    );
    return () => {
      alive = false;
    };
  }, [open, source, reference, reloadKey]);

  const scripts = forcedScripts.length > 0 ? forcedScripts : (preview?.scripts ?? []);
  const needsConfirmation = scripts.length > 0;
  const installed = result?.versions[result.versions.length - 1] ?? null;
  const title = entry?.name ?? "";

  async function install() {
    if (!source) return;
    setPending(true);
    setError(null);
    try {
      const detail = await api.marketplaceInstall({
        source,
        ...(reference ? { ref: reference } : {}),
        ...(needsConfirmation ? { confirm_scripts: true } : {}),
      });
      setResult(detail);
      refreshMarketplaceUpdates();
      onInstalled?.(detail);
    } catch (cause) {
      const failure = toApiError(cause);
      const paths = scriptPathsOf(failure);
      if (failure.code === "scripts_require_confirmation" && paths.length > 0) {
        setForcedScripts(paths);
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
      <DialogContent className="max-h-[92dvh] grid-rows-[auto_minmax(0,1fr)_auto] gap-0 overflow-hidden p-0 sm:max-w-3xl">
        <DialogHeader className="space-y-1.5 border-b px-5 py-4 pr-12 text-left">
          <DialogTitle className="flex flex-wrap items-center gap-2">
            <span className="min-w-0 break-words">
              {t("marketplace.preview.title", { name: title })}
            </span>
            {entry?.version ? (
              <Badge variant="secondary" className="font-mono text-[11px] font-normal">
                {entry.version}
              </Badge>
            ) : null}
          </DialogTitle>
          <DialogDescription>
            {entry?.description || preview?.description || t("skills.picker.noDescription")}
          </DialogDescription>
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
            {entry?.author ? <span>{t("marketplace.card.by", { author: entry.author })}</span> : null}
            {entry?.provider ? (
              <>
                <span aria-hidden>·</span>
                <span className="font-mono">{entry.provider}</span>
              </>
            ) : null}
            {entry?.commit ? (
              <>
                <span aria-hidden>·</span>
                <span className="font-mono">{t("marketplace.card.commit", { commit: entry.commit.slice(0, 7) })}</span>
              </>
            ) : null}
          </div>
        </DialogHeader>

        <div className="min-h-0 space-y-4 overflow-y-auto px-5 py-4">
          {!source ? (
            <p className="rounded-md border border-dashed px-3 py-4 text-sm text-muted-foreground">
              {t("marketplace.preview.noSource")}
            </p>
          ) : null}

          {loading ? (
            <div className="space-y-3" aria-busy="true">
              <span role="status" className="sr-only">
                {t("marketplace.preview.loading")}
              </span>
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-40 w-full rounded-md" />
              <Skeleton className="h-24 w-full rounded-md" />
            </div>
          ) : null}

          {!loading && error && !preview ? (
            <ErrorAlert error={error} onRetry={() => setReloadKey((value) => value + 1)} />
          ) : null}

          {result ? (
            <div className="flex flex-col items-center gap-2 px-2 py-8 text-center">
              <CheckCircle2 aria-hidden className="size-8 text-emerald-600 dark:text-emerald-400" />
              <p className="text-sm font-medium">
                {installed
                  ? t("marketplace.preview.installed", {
                      name: result.name,
                      version: installed.version,
                    })
                  : t("marketplace.preview.installedNoVersion", { name: result.name })}
              </p>
              <p className="max-w-md text-sm text-muted-foreground">
                {t("marketplace.preview.doneDescription")}
              </p>
            </div>
          ) : null}

          {!result && preview ? (
            <>
              {needsConfirmation ? (
                <div
                  role="alert"
                  className="space-y-2 rounded-md border border-amber-600/40 bg-amber-50/50 px-3 py-2.5 dark:bg-amber-950/20"
                >
                  <p className="flex items-center gap-2 text-sm font-medium text-amber-800 dark:text-amber-300">
                    <TriangleAlert aria-hidden className="size-4 shrink-0" />
                    {t("marketplace.preview.scriptsTitle", { count: scripts.length })}
                  </p>
                  <p className="text-sm text-muted-foreground">
                    {t("marketplace.preview.scriptsBody")}
                  </p>
                  <ul className="space-y-1">
                    {scripts.map((path) => (
                      <li key={path} className="flex items-center gap-2 font-mono text-xs">
                        <Terminal
                          aria-hidden
                          className="size-3.5 shrink-0 text-amber-700 dark:text-amber-400"
                        />
                        <span className="min-w-0 break-all">{path}</span>
                      </li>
                    ))}
                  </ul>
                  <div className="flex items-start gap-2 pt-1">
                    <Checkbox
                      id="marketplace-scripts-confirm"
                      checked={confirmed}
                      onCheckedChange={(value) => setConfirmed(value === true)}
                    />
                    <Label
                      htmlFor="marketplace-scripts-confirm"
                      className="text-sm font-normal leading-snug"
                    >
                      {t("marketplace.preview.scriptsConfirm")}
                    </Label>
                  </div>
                </div>
              ) : null}

              <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_240px]">
                <div className="min-w-0 overflow-hidden rounded-md border">
                  <div className="flex items-center gap-2 border-b bg-muted/40 px-3 py-1.5">
                    <FileText aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
                    <span className="truncate font-mono text-xs text-muted-foreground">
                      {t("marketplace.preview.skillMd")}
                    </span>
                    <CopyButton
                      value={preview.skill_md}
                      label={t("skills.detail.copyFile", { path: "SKILL.md" })}
                      size="icon-xs"
                      className="ml-auto"
                    />
                  </div>
                  {preview.skill_md.trim() ? (
                    <MarkdownView value={preview.skill_md} maxHeight="38vh" />
                  ) : (
                    <p className="px-3 py-5 text-sm text-muted-foreground">
                      {t("marketplace.preview.emptySkillMd")}
                    </p>
                  )}
                </div>

                <div className="min-w-0 space-y-2">
                  <div className="flex items-baseline justify-between gap-2">
                    <p className="text-sm font-medium">{t("marketplace.preview.filesTitle")}</p>
                    <p className="font-mono text-[11px] text-muted-foreground">
                      {t("marketplace.preview.filesSummary", {
                        count: preview.files.length,
                        size: formatBytes(preview.total_bytes),
                      })}
                    </p>
                  </div>
                  <ul className="max-h-56 divide-y overflow-auto rounded-md border">
                    {preview.files.map((file) => (
                      <li key={file.path} className="flex items-center gap-2 px-2.5 py-1.5">
                        {file.script ? (
                          <Terminal
                            aria-hidden
                            className="size-3.5 shrink-0 text-amber-700 dark:text-amber-400"
                          />
                        ) : (
                          <FileText
                            aria-hidden
                            className="size-3.5 shrink-0 text-muted-foreground"
                          />
                        )}
                        <span
                          className="min-w-0 flex-1 truncate font-mono text-[11px]"
                          title={file.path}
                        >
                          {file.path}
                        </span>
                        {file.script ? (
                          <Badge
                            variant="outline"
                            title={t("marketplace.preview.scriptTitle")}
                            className="shrink-0 border-amber-600/40 px-1.5 py-0 text-[10px] font-normal text-amber-800 dark:text-amber-300"
                          >
                            {t("marketplace.preview.script")}
                          </Badge>
                        ) : null}
                        <span className="shrink-0 font-mono text-[10px] text-muted-foreground">
                          {formatBytes(file.size)}
                        </span>
                      </li>
                    ))}
                  </ul>
                  <dl className="space-y-1 rounded-md border px-2.5 py-2 text-[11px]">
                    <div className="flex gap-2">
                      <dt className="shrink-0 text-muted-foreground">
                        {t("marketplace.preview.source")}
                      </dt>
                      <dd className="min-w-0 flex-1 truncate font-mono" title={preview.source ?? source ?? ""}>
                        {preview.source ?? source}
                      </dd>
                    </div>
                    {preview.commit ? (
                      <div className="flex gap-2">
                        <dt className="shrink-0 text-muted-foreground">
                          {t("marketplace.preview.commit")}
                        </dt>
                        <dd className="min-w-0 flex-1 truncate font-mono" title={preview.commit}>
                          {preview.commit.slice(0, 7)}
                        </dd>
                      </div>
                    ) : null}
                  </dl>
                </div>
              </div>

              {error ? <ErrorAlert error={error} /> : null}
            </>
          ) : null}
        </div>

        <DialogFooter className="border-t px-5 py-3">
          {result ? (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                {t("common.close")}
              </Button>
              <Button asChild>
                <a href={hrefFor(`/skills/${encodeURIComponent(result.name)}`)}>
                  {t("marketplace.preview.openSkill")}
                </a>
              </Button>
            </>
          ) : (
            <>
              <p className="mr-auto hidden text-xs text-muted-foreground sm:block">
                {needsConfirmation && !confirmed
                  ? t("marketplace.preview.confirmHint")
                  : t("marketplace.preview.footerHint")}
              </p>
              <Button
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={pending}
              >
                {t("common.cancel")}
              </Button>
              {canEdit ? (
                <Button
                  onClick={() => void install()}
                  disabled={
                    pending ||
                    loading ||
                    !preview ||
                    !source ||
                    (needsConfirmation && !confirmed)
                  }
                >
                  {pending ? (
                    <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
                  ) : (
                    <Download aria-hidden />
                  )}
                  {pending
                    ? t("marketplace.preview.installing")
                    : t("marketplace.preview.install")}
                </Button>
              ) : null}
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
