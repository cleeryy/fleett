import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";
import type { DiffHunk, DiffLine } from "@/lib/diff";

const LINE_ROW: Record<DiffLine["kind"], string> = {
  add: "bg-emerald-500/10",
  remove: "bg-destructive/10",
  context: "",
};

const LINE_SIGN: Record<DiffLine["kind"], string> = {
  add: "text-emerald-700 dark:text-emerald-400",
  remove: "text-destructive",
  context: "text-transparent",
};

const SIGN: Record<DiffLine["kind"], string> = { add: "+", remove: "-", context: " " };

/** Client-side preview of the unsaved buffer, grouped like a unified diff. */
export function LineDiff({
  hunks,
  empty,
}: {
  hunks: DiffHunk[];
  empty?: string;
}) {
  const { t } = useI18n();
  if (hunks.length === 0) {
    return (
      <p className="px-3 py-5 text-sm text-muted-foreground">
        {empty ?? t("files.noChanges")}
      </p>
    );
  }

  return (
    <div
      tabIndex={0}
      role="region"
      aria-label={t("files.changesPreview")}
      className="max-h-[45vh] overflow-auto bg-muted/10 font-mono text-xs leading-5"
    >
      {hunks.map((hunk) => (
        <div key={`${hunk.oldStart}-${hunk.newStart}-${hunk.lines.length}`}>
          <div className="sticky left-0 w-fit bg-muted px-3 py-0.5 text-[11px] text-muted-foreground">
            @@ -{hunk.oldStart},{hunk.oldCount} +{hunk.newStart},{hunk.newCount} @@
          </div>
          {hunk.lines.map((line, index) => (
            <div
              key={`${line.kind}-${index}`}
              className={cn("flex w-max min-w-full items-baseline", LINE_ROW[line.kind])}
            >
              <span
                aria-hidden
                className="w-8 shrink-0 px-1 text-right text-[11px] text-muted-foreground/70 select-none sm:w-10"
              >
                {line.oldLine ?? ""}
              </span>
              <span
                aria-hidden
                className="w-8 shrink-0 px-1 text-right text-[11px] text-muted-foreground/70 select-none sm:w-10"
              >
                {line.newLine ?? ""}
              </span>
              <span aria-hidden className={cn("w-4 shrink-0 text-center select-none", LINE_SIGN[line.kind])}>
                {SIGN[line.kind]}
              </span>
              <span className="pr-3 whitespace-pre">{line.text === "" ? "\u00a0" : line.text}</span>
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}

type PatchKind = "meta" | "file" | "hunk" | "add" | "remove" | "context";

const PATCH_ROW: Record<PatchKind, string> = {
  meta: "text-muted-foreground",
  file: "text-muted-foreground",
  hunk: "bg-muted/60 text-muted-foreground",
  add: "bg-emerald-500/10 text-emerald-800 dark:text-emerald-300",
  remove: "bg-destructive/10 text-destructive",
  context: "text-foreground/90",
};

function classify(line: string): PatchKind {
  if (line.startsWith("@@")) return "hunk";
  if (
    line.startsWith("diff --git ") ||
    line.startsWith("index ") ||
    line.startsWith("new file mode ") ||
    line.startsWith("deleted file mode ") ||
    line.startsWith("old mode ") ||
    line.startsWith("new mode ") ||
    line.startsWith("similarity index ") ||
    line.startsWith("rename ") ||
    line.startsWith("Binary files ") ||
    line.startsWith("\\ No newline")
  ) {
    return "meta";
  }
  if (line.startsWith("--- ") || line.startsWith("+++ ")) return "file";
  if (line.startsWith("+")) return "add";
  if (line.startsWith("-")) return "remove";
  return "context";
}

/** Unified patch text from `GET /git/diff`, coloured line by line. */
export function PatchView({
  patch,
  empty,
}: {
  patch: string;
  empty?: string;
}) {
  const { t } = useI18n();
  if (!patch.trim()) {
    return (
      <p className="px-3 py-6 text-sm text-muted-foreground">
        {empty ?? t("files.emptyPatch")}
      </p>
    );
  }

  const lines = patch.replace(/\n$/, "").split("\n");

  return (
    <div
      tabIndex={0}
      role="region"
      aria-label={t("files.patch")}
      className="max-h-[55vh] overflow-auto bg-muted/10 font-mono text-xs leading-5"
    >
      {lines.map((line, index) => {
        const kind = classify(line);
        return (
          <div
            key={index}
            className={cn("w-max min-w-full px-3 whitespace-pre", PATCH_ROW[kind])}
          >
            {line === "" ? "\u00a0" : line}
          </div>
        );
      })}
    </div>
  );
}
