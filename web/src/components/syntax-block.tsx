import { useMemo } from "react";
import { useI18n } from "@/lib/i18n";
import {
  tokenizeLine,
  type SyntaxKind,
  type SyntaxLanguage,
} from "@/lib/syntax";
import { cn } from "@/lib/utils";

/**
 * Colouring is a reading aid, not a theme: two hues (strings, numbers) plus
 * weight for keys, and everything structural recedes into the muted grey the
 * rest of the console already uses.
 */
const TOKEN_CLASS: Record<SyntaxKind, string> = {
  plain: "",
  key: "font-medium text-foreground",
  string: "text-emerald-700 dark:text-emerald-300",
  number: "text-sky-700 dark:text-sky-300",
  literal: "text-sky-700 dark:text-sky-300",
  comment: "text-muted-foreground",
  punct: "text-muted-foreground/70",
};

/** Above this size the colouring is skipped: a wall of spans is not free. */
const MAX_COLOURED_CHARS = 160_000;

export function SyntaxBlock({
  value,
  language,
  maxHeight,
}: {
  value: string;
  language: SyntaxLanguage;
  maxHeight?: number | string;
}) {
  const { t } = useI18n();
  const lines = useMemo(() => value.split("\n"), [value]);
  const coloured = value.length <= MAX_COLOURED_CHARS;

  return (
    <div
      tabIndex={0}
      role="region"
      aria-label={t("files.syntaxPreview", { language })}
      className="overflow-auto rounded-none outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:-outline-offset-2"
      style={{ maxHeight }}
    >
      <div className="flex min-w-full">
        <pre
          aria-hidden
          className="sticky left-0 z-10 w-12 shrink-0 border-r bg-muted/30 py-3 pr-1 text-right font-mono text-[11px] leading-5 text-muted-foreground select-none"
        >
          {Array.from({ length: lines.length }, (_, index) => index + 1).join("\n")}
        </pre>
        {coloured ? (
          <pre className="px-3 py-3 font-mono text-xs leading-5 whitespace-pre">
            {lines.map((line, index) => (
              <span key={index} className="block">
                {line === "" ? (
                  "\u00a0"
                ) : (
                  tokenizeLine(language, line).map((token, tokenIndex) => (
                    <span key={tokenIndex} className={cn(TOKEN_CLASS[token.kind])}>
                      {token.text}
                    </span>
                  ))
                )}
              </span>
            ))}
          </pre>
        ) : (
          <pre className="px-3 py-3 font-mono text-xs leading-5 whitespace-pre">{value}</pre>
        )}
      </div>
    </div>
  );
}
