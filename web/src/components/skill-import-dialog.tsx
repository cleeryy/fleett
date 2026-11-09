import { useEffect, useRef, useState, type FormEvent } from "react";
import { FileArchive, Loader2 } from "lucide-react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { SkillDetail } from "@/api/types";
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
import { ErrorAlert } from "@/components/error-alert";
import { RichText } from "@/components/rich-text";
import { formatBytes } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { MAX_SKILL_ZIP_BYTES } from "@/lib/skills";

/**
 * Import a zip archive. The controller reads `skill.yaml` and the files, picks
 * the next free version and commits it; importing the same archive twice never
 * conflicts, which is why the dialog reports the version it created.
 */
export function SkillImportDialog({
  open,
  onOpenChange,
  onImported,
  onOpenSkill,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onImported: (detail: SkillDetail) => void;
  onOpenSkill: (name: string) => void;
}) {
  const { t } = useI18n();
  const inputRef = useRef<HTMLInputElement>(null);
  const [file, setFile] = useState<File | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [fileError, setFileError] = useState<string | null>(null);
  const [result, setResult] = useState<SkillDetail | null>(null);

  useEffect(() => {
    if (!open) return;
    setFile(null);
    setPending(false);
    setError(null);
    setFileError(null);
    setResult(null);
    if (inputRef.current) inputRef.current.value = "";
  }, [open]);

  function pick(next: File | null) {
    setFileError(null);
    setError(null);
    if (!next) {
      setFile(null);
      return;
    }
    if (next.size > MAX_SKILL_ZIP_BYTES) {
      setFile(null);
      setFileError(t("skills.import.tooLarge", { limit: formatBytes(MAX_SKILL_ZIP_BYTES) }));
      return;
    }
    setFile(next);
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!file) {
      setFileError(t("skills.import.chooseFile"));
      inputRef.current?.focus();
      return;
    }

    setPending(true);
    setError(null);
    try {
      const detail = await api.importSkill(file);
      setResult(detail);
      onImported(detail);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  const published = result?.versions[result.versions.length - 1] ?? null;

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("skills.import.title")}</DialogTitle>
          <DialogDescription>
            <RichText text={t("skills.import.description")} />
          </DialogDescription>
        </DialogHeader>

        {result ? (
          <div className="space-y-3">
            <div className="space-y-1 rounded-md border p-3">
              <p className="flex flex-wrap items-center gap-2">
                <span className="font-mono text-sm">{result.name}</span>
                <Badge variant="secondary" className="font-mono text-[11px] font-normal">
                  {t("skills.import.published", {
                    version: published?.version ?? result.latest ?? "?",
                  })}
                </Badge>
              </p>
              <p className="text-sm text-muted-foreground">
                {result.description || t("skills.picker.noDescription")}
              </p>
              <p className="text-xs text-muted-foreground">
                {published
                  ? t("skills.import.filesAndSize", {
                      count: published.files_count,
                      size: formatBytes(published.size_bytes),
                    })
                  : t("history.fileCount", { count: result.files.length })}
              </p>
            </div>
            <p className="text-xs text-muted-foreground">
              <RichText text={t("skills.import.latestNote")} />
            </p>
          </div>
        ) : (
          <form onSubmit={onSubmit} noValidate className="space-y-4">
            <div className="space-y-2">
              <label
                htmlFor="skill-zip"
                className="flex cursor-pointer flex-col items-center gap-2 rounded-md border border-dashed px-4 py-8 text-center transition-colors hover:bg-muted/40"
              >
                <FileArchive aria-hidden className="size-5 text-muted-foreground" />
                <span className="text-sm font-medium">
                  {file ? file.name : t("skills.import.chooseFile")}
                </span>
                <span className="text-xs text-muted-foreground">
                  {file
                    ? formatBytes(file.size)
                    : t("skills.import.limit", { limit: formatBytes(MAX_SKILL_ZIP_BYTES) })}
                </span>
              </label>
              <input
                ref={inputRef}
                id="skill-zip"
                type="file"
                accept=".zip,application/zip,application/x-zip-compressed"
                className="sr-only"
                aria-describedby="skill-zip-help"
                onChange={(event) => pick(event.target.files?.[0] ?? null)}
              />
              <p id="skill-zip-help" className="text-xs text-muted-foreground">
                {t("skills.import.safetyNote")}
              </p>
              {fileError ? <p className="text-xs text-destructive">{fileError}</p> : null}
            </div>

            {error ? <ErrorAlert error={error} /> : null}

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={pending}
              >
                {t("common.cancel")}
              </Button>
              <Button type="submit" disabled={pending || !file}>
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <FileArchive aria-hidden />}
                {t("skills.import.confirm")}
              </Button>
            </DialogFooter>
          </form>
        )}

        {result ? (
          <DialogFooter>
            <Button variant="outline" onClick={() => onOpenChange(false)}>
              {t("common.close")}
            </Button>
            <Button onClick={() => onOpenSkill(result.name)}>{t("skills.import.openSkill")}</Button>
          </DialogFooter>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
