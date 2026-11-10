import { Badge } from "@/components/ui/badge";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

const HEALTH_STYLES: Record<string, { dot: string; className: string }> = {
  healthy: {
    dot: "bg-emerald-500",
    className: "border-emerald-600/30 text-emerald-700 dark:text-emerald-400",
  },
  unhealthy: {
    dot: "bg-destructive",
    className: "border-destructive/40 text-destructive",
  },
  starting: {
    dot: "bg-sky-500",
    className: "border-sky-600/30 text-sky-700 dark:text-sky-400",
  },
  none: {
    dot: "bg-muted-foreground",
    className: "text-muted-foreground",
  },
};

export function McpHealthBadge({ health, className }: { health: string; className?: string }) {
  const { t } = useI18n();
  const key = health in HEALTH_STYLES ? health : "none";
  const style = HEALTH_STYLES[key];
  return (
    <Badge
      variant="outline"
      className={cn("gap-1.5 font-mono text-xs font-normal", style.className, className)}
    >
      <span aria-hidden className={cn("size-1.5 shrink-0 rounded-full", style.dot)} />
      {t(`mcp.health.${key}`)}
    </Badge>
  );
}

export function McpKindBadge({ kind }: { kind: string }) {
  return (
    <Badge variant="outline" className="font-mono text-xs font-normal text-muted-foreground">
      {kind}
    </Badge>
  );
}

/** `rw` / `readonly` as the console speaks it. */
export function McpModeBadge({ mode }: { mode: string }) {
  const { t } = useI18n();
  return (
    <Badge variant="secondary" className="font-mono text-[11px] font-normal">
      {mode === "readonly" ? t("instance.mcp.modeReadonly") : t("instance.mcp.modeRw")}
    </Badge>
  );
}
