import { useEffect, useId, useRef } from "react";
import { ChevronRight } from "lucide-react";
import { useI18n } from "@/lib/i18n";
import type {
  NavGroup as NavGroupModel,
  NavKey,
  NavTarget,
} from "@/lib/navigation";
import { hrefFor } from "@/lib/router";
import { cn } from "@/lib/utils";

/** One sidebar link, shared by the four spaces and the advanced groups. */
export function NavLink({
  entry,
  active,
  onNavigate,
}: {
  entry: NavTarget;
  active: boolean;
  onNavigate?: () => void;
}) {
  const { t } = useI18n();
  const Icon = entry.icon;
  return (
    <a
      href={hrefFor(entry.path)}
      onClick={onNavigate}
      aria-current={active ? "page" : undefined}
      className={cn(
        "relative flex min-h-11 items-center gap-2.5 rounded-md px-2.5 text-sm outline-none transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 md:min-h-9",
        active
          ? "bg-accent font-medium text-accent-foreground before:absolute before:top-1/2 before:left-0 before:h-4 before:w-0.5 before:-translate-y-1/2 before:rounded-full before:bg-foreground/60 before:content-['']"
          : "text-muted-foreground hover:bg-accent/60 hover:text-foreground",
      )}
    >
      <Icon aria-hidden className="size-4 shrink-0" />
      <span className="truncate">{t(entry.labelKey)}</span>
    </a>
  );
}

/**
 * Collapsible advanced group. The header is a real button (`aria-expanded`,
 * `aria-controls`); the panel is hidden rather than unmounted so the links
 * stay in the DOM for tests and for `aria-controls`.
 */
export function NavGroup({
  group,
  activeKey,
  open,
  admin,
  onToggle,
  onNavigate,
}: {
  group: NavGroupModel;
  activeKey: NavKey | null;
  open: boolean;
  admin: boolean;
  onToggle: () => void;
  onNavigate?: () => void;
}) {
  const { t } = useI18n();
  const Icon = group.icon;
  const entries = group.entries.filter((entry) => !entry.adminOnly || admin);
  // Desktop and mobile render the same model; the panel id must stay unique.
  const instanceId = useId().replace(/:/g, "");
  const panelId = `nav-group-${group.key}-${instanceId}`;
  const hintId = `${panelId}-hint`;
  const rootRef = useRef<HTMLDivElement>(null);
  const wasOpen = useRef(open);

  // A group opened near the bottom of the drawer must reveal its links, not
  // just its hint. Prefer the active link when the group contains it.
  useEffect(() => {
    if (open && !wasOpen.current) {
      const active = rootRef.current?.querySelector<HTMLElement>('[aria-current="page"]');
      const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      (active ?? rootRef.current)?.scrollIntoView({
        block: active ? "nearest" : "start",
        behavior: reduce ? "auto" : "smooth",
      });
    }
    wasOpen.current = open;
  }, [open]);

  if (entries.length === 0) return null;

  return (
    <div ref={rootRef}>
      <button
        type="button"
        aria-expanded={open}
        aria-controls={panelId}
        aria-describedby={hintId}
        onClick={onToggle}
        className="flex min-h-11 w-full items-center gap-2 rounded-md px-2.5 text-left text-xs font-medium text-muted-foreground outline-none transition-colors hover:bg-accent/50 hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 md:min-h-8"
      >
        <Icon aria-hidden className="size-3.5 shrink-0" />
        <span className="flex-1 truncate">{t(group.labelKey)}</span>
        <ChevronRight
          aria-hidden
          className={cn(
            "size-3.5 shrink-0 transition-transform duration-200 motion-reduce:transition-none",
            open && "rotate-90",
          )}
        />
      </button>

      {/* The explanation stays visible when the group is collapsed: the
          disclosure hides destinations, never the reason to open it. */}
      <p id={hintId} className="pr-2.5 pb-1 pl-8 text-xs leading-snug text-muted-foreground">
        {t(group.hintKey)}
      </p>

      <div id={panelId} hidden={!open}>
        <div className="ml-3 space-y-0.5 border-l pl-2">
          {entries.map((entry) => (
            <NavLink
              key={entry.key}
              entry={entry}
              active={entry.key === activeKey}
              onNavigate={onNavigate}
            />
          ))}
        </div>
      </div>
    </div>
  );
}
