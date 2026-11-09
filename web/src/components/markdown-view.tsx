import type { ReactNode } from "react";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/**
 * Small, dependency-free Markdown preview for skill files. It renders React
 * nodes (never HTML), so a SKILL.md can be read without an XSS surface, and it
 * covers only what an agent-facing document uses: headings, paragraphs, lists,
 * quotes, rules, fenced code and inline code/emphasis/links.
 */
export function MarkdownView({
  value,
  maxHeight = "50vh",
  className,
}: {
  value: string;
  maxHeight?: string;
  className?: string;
}) {
  const { t } = useI18n();
  return (
    <div
      tabIndex={0}
      role="region"
      aria-label={t("files.markdownPreview")}
      className={cn("overflow-auto p-4 text-sm leading-relaxed", className)}
      style={{ maxHeight }}
    >
      <div className="space-y-3">{renderMarkdown(value)}</div>
    </div>
  );
}

function renderMarkdown(source: string): ReactNode[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const blocks: ReactNode[] = [];
  let index = 0;
  let key = 0;

  while (index < lines.length) {
    const line = lines[index];

    if (!line.trim()) {
      index += 1;
      continue;
    }

    // Fenced code: everything up to the closing fence, verbatim.
    const fence = /^\s*```\s*([\w-]*)\s*$/.exec(line);
    if (fence) {
      const body: string[] = [];
      index += 1;
      while (index < lines.length && !/^\s*```\s*$/.test(lines[index])) {
        body.push(lines[index]);
        index += 1;
      }
      index += 1;
      blocks.push(
        <pre
          key={key++}
          className="overflow-auto rounded-md border bg-muted/30 p-3 font-mono text-xs leading-relaxed"
        >
          {body.join("\n")}
        </pre>,
      );
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      const level = heading[1].length;
      const sizes: Record<number, string> = {
        1: "text-lg font-semibold",
        2: "text-base font-semibold",
        3: "text-sm font-semibold",
        4: "text-sm font-medium",
        5: "text-sm font-medium",
        6: "text-sm font-medium",
      };
      blocks.push(
        <p
          key={key++}
          role="heading"
          aria-level={level}
          className={cn("tracking-tight", sizes[level])}
        >
          {renderInline(heading[2])}
        </p>,
      );
      index += 1;
      continue;
    }

    if (/^\s*(---+|\*\*\*+|___+)\s*$/.test(line)) {
      blocks.push(<hr key={key++} className="border-border" />);
      index += 1;
      continue;
    }

    if (/^\s*>/.test(line)) {
      const quote: string[] = [];
      while (index < lines.length && /^\s*>/.test(lines[index])) {
        quote.push(lines[index].replace(/^\s*>\s?/, ""));
        index += 1;
      }
      blocks.push(
        <blockquote
          key={key++}
          className="border-l-2 border-border pl-3 text-muted-foreground"
        >
          {renderInline(quote.join(" "))}
        </blockquote>,
      );
      continue;
    }

    const ordered = /^\s*\d+[.)]\s+/.test(line);
    const unordered = /^\s*[-*+]\s+/.test(line);
    if (ordered || unordered) {
      const pattern = ordered ? /^\s*\d+[.)]\s+/ : /^\s*[-*+]\s+/;
      const items: string[] = [];
      while (index < lines.length && pattern.test(lines[index])) {
        items.push(lines[index].replace(pattern, ""));
        index += 1;
      }
      const ListTag = ordered ? "ol" : "ul";
      blocks.push(
        <ListTag
          key={key++}
          className={cn(
            "space-y-1 pl-5",
            ordered ? "list-decimal" : "list-disc",
            "[&>li]:pl-0.5",
          )}
        >
          {items.map((item, itemIndex) => (
            <li key={itemIndex}>{renderInline(item)}</li>
          ))}
        </ListTag>,
      );
      continue;
    }

    // Paragraph: consecutive plain lines, soft-broken with a space.
    const paragraph: string[] = [];
    while (
      index < lines.length &&
      lines[index].trim() &&
      !/^\s*```/.test(lines[index]) &&
      !/^(#{1,6})\s+/.test(lines[index]) &&
      !/^\s*>/.test(lines[index]) &&
      !/^\s*\d+[.)]\s+/.test(lines[index]) &&
      !/^\s*[-*+]\s+/.test(lines[index]) &&
      !/^\s*(---+|\*\*\*+|___+)\s*$/.test(lines[index])
    ) {
      paragraph.push(lines[index].trim());
      index += 1;
    }
    blocks.push(
      <p key={key++} className="text-foreground/90">
        {renderInline(paragraph.join(" "))}
      </p>,
    );
  }

  return blocks;
}

const INLINE_RE = /(`[^`]+`|\*\*[^**]+\*\*|__[^__]+__|\*[^*\s][^*]*\*|_[^_\s][^_]*_|\[[^\]]+\]\([^)\s]+\))/g;

/** Inline code, strong, emphasis and http(s) links; everything else is text. */
function renderInline(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let last = 0;
  let match: RegExpExecArray | null;
  INLINE_RE.lastIndex = 0;
  let key = 0;

  while ((match = INLINE_RE.exec(text)) !== null) {
    if (match.index > last) nodes.push(text.slice(last, match.index));
    const token = match[0];

    if (token.startsWith("`")) {
      nodes.push(
        <code
          key={`code-${key++}`}
          className="rounded bg-muted px-1 py-0.5 font-mono text-[0.85em]"
        >
          {token.slice(1, -1)}
        </code>,
      );
    } else if (token.startsWith("**") || token.startsWith("__")) {
      nodes.push(
        <strong key={`strong-${key++}`} className="font-semibold">
          {token.slice(2, -2)}
        </strong>,
      );
    } else if (token.startsWith("[")) {
      const link = /^\[([^\]]+)\]\(([^)\s]+)\)$/.exec(token);
      const href = link?.[2] ?? "";
      const safe = /^(https?:\/\/|#|\/)/i.test(href);
      nodes.push(
        safe ? (
          <a
            key={`link-${key++}`}
            href={href}
            className="underline underline-offset-4"
            {...(/^https?:/i.test(href) ? { target: "_blank", rel: "noreferrer" } : {})}
          >
            {link?.[1] ?? token}
          </a>
        ) : (
          token
        ),
      );
    } else {
      nodes.push(
        <em key={`em-${key++}`}>
          {token.slice(1, -1)}
        </em>,
      );
    }
    last = match.index + token.length;
  }

  if (last < text.length) nodes.push(text.slice(last));
  return nodes;
}
