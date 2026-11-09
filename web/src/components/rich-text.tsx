import type { ReactNode } from "react";

/**
 * Renders copy that embeds technical tokens between backticks:
 * ``Mount at `/opt/data/shared/<name>` `` → the path is monospaced, the rest is
 * plain text. Keeps one translatable sentence per language instead of
 * fragmenting it around JSX elements.
 */
export function RichText({ text }: { text: string }) {
  const parts = text.split(/`([^`]+)`/g);
  const nodes: ReactNode[] = parts.map((part, index) =>
    index % 2 === 1 ? (
      <span key={index} className="font-mono">
        {part}
      </span>
    ) : (
      part
    ),
  );
  return <>{nodes}</>;
}
