import {
  Ban,
  Check,
  Clock,
  Loader2,
  TriangleAlert,
  Upload,
  X,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { formatBytes } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import type { UploadItem } from "@/lib/use-uploads";
import { cn } from "@/lib/utils";

/**
 * Receipt for one drop: every file keeps its row until the operator clears it,
 * so a `409` never loses the selection, only asks a question.
 */
export function FileUploadsList({
  items,
  onOverwrite,
  onSkip,
  onRetry,
  onDismiss,
  onClear,
}: {
  items: UploadItem[];
  onOverwrite: (key: string) => void;
  onSkip: (key: string) => void;
  onRetry: (key: string) => void;
  onDismiss: (key: string) => void;
  onClear: () => void;
}) {
  const { t } = useI18n();
  const done = items.filter((item) => item.status === "done").length;
  const pending = items.filter(
    (item) => item.status === "queued" || item.status === "uploading",
  ).length;
  const conflicts = items.filter((item) => item.status === "conflict").length;

  return (
    <section
      aria-label={t("files.uploads.title")}
      className="overflow-hidden rounded-md border"
    >
      <div className="flex flex-wrap items-center gap-2 border-b bg-muted/30 px-3 py-1.5">
        <Upload aria-hidden className="size-3.5 text-muted-foreground" />
        <span className="text-xs font-medium">{t("files.uploads.title")}</span>
        <span className="text-[11px] text-muted-foreground">
          {done > 0 ? t("files.uploads.doneCount", { count: done }) : null}
          {done > 0 && (pending > 0 || conflicts > 0) ? " · " : null}
          {pending > 0 ? t("files.uploads.pendingCount", { count: pending }) : null}
          {pending > 0 && conflicts > 0 ? " · " : null}
          {conflicts > 0 ? t("files.uploads.conflictsCount", { count: conflicts }) : null}
        </span>
        <Button
          variant="ghost"
          size="xs"
          className="ml-auto"
          onClick={onClear}
          disabled={items.every((item) => item.status !== "done" && item.status !== "skipped")}
        >
          {t("files.uploads.clearFinished")}
        </Button>
      </div>

      <ul className="max-h-64 divide-y overflow-auto">
        {items.map((item) => (
          <FileUploadRow
            key={item.key}
            item={item}
            onOverwrite={onOverwrite}
            onSkip={onSkip}
            onRetry={onRetry}
            onDismiss={onDismiss}
          />
        ))}
      </ul>
    </section>
  );
}

function FileUploadRow({
  item,
  onOverwrite,
  onSkip,
  onRetry,
  onDismiss,
}: {
  item: UploadItem;
  onOverwrite: (key: string) => void;
  onSkip: (key: string) => void;
  onRetry: (key: string) => void;
  onDismiss: (key: string) => void;
}) {
  const { t } = useI18n();
  const executable = item.error?.code === "executable_file";

  return (
    <li className="flex flex-col gap-1 px-3 py-2 sm:flex-row sm:items-start sm:gap-x-3">
      <div className="flex min-w-0 items-start gap-2 sm:flex-1">
        <StatusMark status={item.status} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-baseline gap-x-2">
            <span className="min-w-0 truncate font-mono text-xs">{item.name}</span>
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {formatBytes(item.size)}
            </span>
            <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
              → {item.directory}
            </span>
          </div>

          <p
            className={cn(
              "text-[11px]",
              item.status === "done"
                ? "text-muted-foreground"
                : item.status === "conflict"
                  ? "text-foreground/90"
                  : item.status === "skipped" || item.status === "queued"
                    ? "text-muted-foreground"
                    : "text-destructive",
            )}
          >
            {describe(item, t)}
          </p>

          {executable ? (
            <p className="text-[11px] text-muted-foreground">{t("files.uploads.executables")}</p>
          ) : null}

          {item.backup ? (
            <p className="truncate font-mono text-[11px] text-muted-foreground">
              {t("files.uploads.previousBytes", { size: item.backup })}
            </p>
          ) : null}
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-1 pl-6 sm:shrink-0 sm:pl-0">
        <StatusBadge status={item.status} />
        {item.status === "conflict" || item.status === "skipped" ? (
          <>
            <Button variant="outline" size="xs" onClick={() => onOverwrite(item.key)}>
              {t("files.uploads.overwrite")}
            </Button>
            {item.status === "conflict" ? (
              <Button variant="ghost" size="xs" onClick={() => onSkip(item.key)}>
                {t("files.uploads.skip")}
              </Button>
            ) : null}
          </>
        ) : null}
        {item.status === "error" ? (
          <Button variant="outline" size="xs" onClick={() => onRetry(item.key)}>
            {t("common.retry")}
          </Button>
        ) : null}
        {item.status === "uploading" || item.status === "queued" ? null : (
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label={t("files.uploads.removeAria", { name: item.name })}
            title={t("files.uploads.removeTitle")}
            onClick={() => onDismiss(item.key)}
          >
            <X aria-hidden />
          </Button>
        )}
      </div>
    </li>
  );
}

function StatusMark({ status }: { status: UploadItem["status"] }) {
  const className = "mt-0.5 size-4 shrink-0";
  switch (status) {
    case "uploading":
      return <Loader2 aria-hidden className={cn(className, "animate-spin text-muted-foreground")} />;
    case "queued":
      return <Clock aria-hidden className={cn(className, "text-muted-foreground")} />;
    case "done":
      return <Check aria-hidden className={cn(className, "text-emerald-700 dark:text-emerald-400")} />;
    case "conflict":
      return (
        <TriangleAlert aria-hidden className={cn(className, "text-amber-600 dark:text-amber-400")} />
      );
    case "skipped":
      return <Ban aria-hidden className={cn(className, "text-muted-foreground")} />;
    default:
      return <TriangleAlert aria-hidden className={cn(className, "text-destructive")} />;
  }
}

function StatusBadge({ status }: { status: UploadItem["status"] }) {
  const { t } = useI18n();
  const labels: Record<UploadItem["status"], string> = {
    queued: t("files.uploads.statusQueued"),
    uploading: t("files.uploads.statusUploading"),
    done: t("files.uploads.statusDone"),
    conflict: t("files.uploads.statusConflict"),
    refused: t("files.uploads.statusRefused"),
    too_large: t("files.uploads.statusTooLarge"),
    skipped: t("files.uploads.statusSkipped"),
    error: t("files.uploads.statusError"),
  };
  if (status === "uploading" || status === "queued") {
    return (
      <Badge variant="secondary" className="px-1.5 py-0 text-[10px] font-normal">
        {labels[status]}
      </Badge>
    );
  }
  return (
    <Badge variant="outline" className="px-1.5 py-0 text-[10px] font-normal">
      {labels[status]}
    </Badge>
  );
}

function describe(
  item: UploadItem,
  t: (key: string, params?: Record<string, string | number>) => string,
): string {
  switch (item.status) {
    case "queued":
      return item.overwrite ? t("files.uploads.queuedOverwrite") : t("files.uploads.queued");
    case "uploading":
      return t("files.uploads.uploading");
    case "done":
      return item.path ? t("files.uploads.writtenTo", { path: item.path }) : t("files.uploads.done");
    case "conflict":
      return t("files.uploads.conflict");
    case "skipped":
      return t("files.uploads.skipped");
    case "refused":
      return item.error?.message ?? item.reason ?? t("files.uploads.refused");
    case "too_large":
      return item.error?.message ?? item.reason ?? t("files.uploads.tooLarge");
    default:
      return item.error?.message ?? t("files.uploads.failed");
  }
}
