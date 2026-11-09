import { Badge } from "@/components/ui/badge";
import type { EgressPolicyState } from "@/api/types";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

const STYLES: Record<EgressPolicyState, { className: string; dot: string }> = {
  enforced: {
    className: "border-emerald-600/40 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
    dot: "bg-emerald-500",
  },
  legacy: {
    className: "border-muted-foreground/30 bg-muted text-muted-foreground",
    dot: "bg-muted-foreground",
  },
  none: {
    className: "border-dashed text-muted-foreground",
    dot: "bg-transparent ring-1 ring-muted-foreground/60",
  },
};

/**
 * Coarse destination-policy state (issue #60).
 *
 * `legacy` is not an error: it means the instance only has the host LAN filter,
 * with no destination policy. The tooltip spells that out so a grey badge never
 * reads as a silent, unexplained state. `enforced` turns red when the rendered
 * proxy stopped answering, because every outbound connection then fails.
 */
export function EgressBadge({
  policy,
  proxyReachable,
  className,
}: {
  policy: EgressPolicyState;
  /** Only meaningful for `enforced`: false renders the broken-proxy state. */
  proxyReachable?: boolean | null;
  className?: string;
}) {
  const { t } = useI18n();
  const style = STYLES[policy];
  const broken = policy === "enforced" && proxyReachable === false;
  const label = broken ? t("egressBadge.proxyDown") : t(`egressBadge.${policy}`);
  const hint = broken ? t("egressBadge.proxyDownHint") : t(`egressBadge.${policy}Hint`);
  return (
    <Badge
      variant="outline"
      className={cn(
        "gap-1.5 font-mono text-xs font-normal",
        style.className,
        broken && "border-destructive/40 bg-destructive/10 text-destructive",
        className,
      )}
      title={hint}
    >
      <span
        aria-hidden
        className={cn("size-1.5 shrink-0 rounded-full", broken ? "bg-destructive" : style.dot)}
      />
      {label}
    </Badge>
  );
}
