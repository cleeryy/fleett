import { Badge } from "@/components/ui/badge";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/**
 * One status badge for the operations surfaces (runs, pipelines, incidents,
 * deployments). The instance lifecycle keeps its own `StatusBadge`; this one
 * covers the states those features introduce.
 */
const TONES: Record<string, { dot: string; className: string }> = {
  ok: { dot: "bg-emerald-500", className: "border-emerald-600/30 text-emerald-700 dark:text-emerald-400" },
  healthy: { dot: "bg-emerald-500", className: "border-emerald-600/30 text-emerald-700 dark:text-emerald-400" },
  running: { dot: "bg-sky-500", className: "border-sky-600/30 text-sky-700 dark:text-sky-400" },
  info: { dot: "bg-sky-500", className: "border-sky-600/30 text-sky-700 dark:text-sky-400" },
  warn: { dot: "bg-amber-500", className: "border-amber-600/30 text-amber-700 dark:text-amber-400" },
  canary: { dot: "bg-amber-500", className: "border-amber-600/30 text-amber-700 dark:text-amber-400" },
  scheduled: { dot: "bg-sky-500", className: "border-sky-600/30 text-sky-700 dark:text-sky-400" },
  pending: { dot: "bg-muted-foreground", className: "text-muted-foreground" },
  skipped: { dot: "bg-muted-foreground", className: "text-muted-foreground" },
  cancelled: { dot: "bg-muted-foreground", className: "text-muted-foreground" },
  closed: { dot: "bg-muted-foreground", className: "text-muted-foreground" },
  error: { dot: "bg-destructive", className: "border-destructive/40 text-destructive" },
  failed: { dot: "bg-destructive", className: "border-destructive/40 text-destructive" },
  timeout: { dot: "bg-destructive", className: "border-destructive/40 text-destructive" },
  open: { dot: "bg-destructive", className: "border-destructive/40 text-destructive" },
  exceeded: { dot: "bg-destructive", className: "border-destructive/40 text-destructive" },
  rolled_back: { dot: "bg-amber-500", className: "border-amber-600/30 text-amber-700 dark:text-amber-400" },
  restoring: { dot: "bg-amber-500", className: "border-amber-600/30 text-amber-700 dark:text-amber-400" },
};

const FALLBACK = { dot: "bg-muted-foreground", className: "text-muted-foreground" };

export function OpsBadge({
  status,
  label,
  className,
}: {
  status: string;
  label?: string;
  className?: string;
}) {
  const { t } = useI18n();
  const tone = TONES[status] ?? FALLBACK;
  // Known states get localized copy; an unknown backend state keeps its raw
  // identifier (with underscores spelled out) rather than inventing a label.
  const key = `status.${status}`;
  const known = status in TONES;
  const text = label ?? (known ? t(key) : status.replace(/_/g, " "));
  return (
    <Badge
      variant="outline"
      className={cn("gap-1.5 font-mono text-xs font-normal", tone.className, className)}
    >
      <span aria-hidden className={cn("size-1.5 shrink-0 rounded-full", tone.dot)} />
      {text}
    </Badge>
  );
}
