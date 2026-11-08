import { CopyButton } from "@/components/copy-button";

export function CodeBlock({
  value,
  title,
  empty = "Nothing to show.",
  maxHeight = "60vh",
}: {
  value: string;
  title?: string;
  empty?: string;
  maxHeight?: string;
}) {
  return (
    <div className="overflow-hidden rounded-md border">
      <div className="flex items-center justify-between gap-3 border-b bg-muted/40 px-3 py-1.5">
        <span className="truncate font-mono text-xs text-muted-foreground">{title}</span>
        <CopyButton value={value} label={`Copy ${title ?? "content"}`} variant="ghost" showLabel size="sm" />
      </div>
      {value ? (
        <pre
          tabIndex={0}
          role="region"
          aria-label={title ?? "Content"}
          className="overflow-auto bg-muted/20 p-4 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words"
          style={{ maxHeight }}
        >
          {value}
        </pre>
      ) : (
        <p className="p-4 text-sm text-muted-foreground">{empty}</p>
      )}
    </div>
  );
}
