import type { PersonaState } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

const LABELS: Record<PersonaState, string> = {
  applied: "applied",
  diverged: "diverged",
  none: "no persona",
};

const DOT: Record<PersonaState, string> = {
  applied: "bg-emerald-500",
  diverged: "bg-amber-500",
  none: "bg-muted-foreground",
};

const TONE: Record<PersonaState, string> = {
  applied: "border-emerald-600/30 text-emerald-700 dark:text-emerald-400",
  diverged: "border-amber-600/40 text-amber-700 dark:text-amber-400",
  none: "text-muted-foreground",
};

/**
 * Drift of an instance's SOUL.md against its persona. `diverged` means the
 * file was edited by hand after the last apply; `applied` means it still
 * matches the rendered persona.
 */
export function PersonaStateBadge({
  state,
  className,
}: {
  state?: PersonaState;
  className?: string;
}) {
  const value: PersonaState = state ?? "none";
  return (
    <Badge
      variant="outline"
      className={cn("gap-1.5 font-mono text-[11px] font-normal", TONE[value], className)}
      title={
        value === "diverged"
          ? "SOUL.md was edited by hand after the persona was applied"
          : value === "applied"
            ? "SOUL.md still matches the persona"
            : "This instance has no persona"
      }
    >
      <span aria-hidden className={cn("size-1.5 shrink-0 rounded-full", DOT[value])} />
      {LABELS[value]}
    </Badge>
  );
}
