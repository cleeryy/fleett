import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import type { InstanceStatus } from "@/api/types";

const STATUS_STYLES: Record<InstanceStatus, { dot: string; className: string }> = {
  running: {
    dot: "bg-emerald-500",
    className: "border-emerald-600/30 text-emerald-700 dark:text-emerald-400",
  },
  stopped: {
    dot: "bg-muted-foreground",
    className: "text-muted-foreground",
  },
  created: {
    dot: "bg-sky-500",
    className: "border-sky-600/30 text-sky-700 dark:text-sky-400",
  },
  error: {
    dot: "bg-destructive",
    className: "border-destructive/40 text-destructive",
  },
  unknown: {
    dot: "bg-muted-foreground",
    className: "text-muted-foreground",
  },
};

export function StatusBadge({ status, className }: { status: string; className?: string }) {
  const key = (status in STATUS_STYLES ? status : "unknown") as InstanceStatus;
  const style = STATUS_STYLES[key];
  return (
    <Badge
      variant="outline"
      className={cn("gap-1.5 font-mono text-xs font-normal", style.className, className)}
    >
      <span aria-hidden className={cn("size-1.5 shrink-0 rounded-full", style.dot)} />
      {key}
    </Badge>
  );
}
