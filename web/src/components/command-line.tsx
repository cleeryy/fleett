import { CopyButton } from "@/components/copy-button";
import { cn } from "@/lib/utils";

/**
 * One shell command with a copy button. Long commands wrap instead of scrolling
 * so the whole fix stays readable on a phone.
 */
export function CommandLine({
  value,
  label,
  badge,
  className,
}: {
  value: string;
  label?: string;
  /** Small leading label, e.g. `Fix` in the diagnostics report. */
  badge?: string;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex items-start gap-2 rounded-md border bg-muted/40 px-2.5 py-1.5",
        className,
      )}
    >
      {badge ? (
        <span className="shrink-0 pt-0.5 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
          {badge}
        </span>
      ) : null}
      <code className="min-w-0 flex-1 font-mono text-xs leading-relaxed break-words whitespace-pre-wrap">
        {value}
      </code>
      <CopyButton value={value} label={label} size="icon-xs" className="shrink-0" />
    </div>
  );
}
