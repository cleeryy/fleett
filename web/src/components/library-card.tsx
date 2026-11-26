import type { ReactNode } from "react";
import {
  ArrowUpCircle,
  Fingerprint,
  LayoutTemplate,
  Plug,
  Sparkles,
  Store,
  Users,
  type LucideIcon,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { useI18n } from "@/lib/i18n";

export type LibraryCardKind = "template" | "persona" | "skill" | "marketplace" | "connector";

export type LibraryCardSource = "local" | "git" | "marketplace";

/** Installed state shared by skills, marketplace entries and connectors. */
export type LibraryCardState = "installed" | "upToDate" | "update";

const ICONS: Record<LibraryCardKind, LucideIcon> = {
  template: LayoutTemplate,
  persona: Fingerprint,
  skill: Sparkles,
  marketplace: Store,
  connector: Plug,
};

/**
 * One card for the five former pages: same tile, title, two-line description,
 * source line, meta row and a single action. The whole card opens the preview
 * through an overlay button, so the action stays a real sibling control.
 */
export function LibraryCard({
  kind,
  title,
  subtitle,
  description,
  source,
  gitSha,
  author,
  version,
  state,
  updateVersion,
  agents,
  extraMeta,
  action,
  onOpen,
}: {
  kind: LibraryCardKind;
  title: string;
  /** Monospace identifier under the title (template or connector id). */
  subtitle?: string;
  description: string;
  source: LibraryCardSource;
  gitSha?: string;
  author?: string;
  version?: string | null;
  state?: LibraryCardState;
  updateVersion?: string;
  agents?: number;
  extraMeta?: ReactNode;
  action?: ReactNode;
  onOpen: () => void;
}) {
  const { t } = useI18n();
  const Icon = ICONS[kind];

  return (
    <li className="relative flex min-w-0 flex-col gap-3 rounded-md border p-4 transition-colors hover:border-foreground/20 hover:bg-muted/20 focus-within:ring-[3px] focus-within:ring-ring/50">
      <button
        type="button"
        onClick={onOpen}
        aria-label={t("library.card.openAria", { name: title })}
        className="absolute inset-0 z-0 rounded-md outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
      />

      <div className="pointer-events-none relative z-10 flex min-w-0 items-start gap-3">
        <div className="flex size-9 shrink-0 items-center justify-center rounded-md border bg-muted/40">
          <Icon aria-hidden className="size-4 text-muted-foreground" />
        </div>
        <div className="min-w-0 flex-1">
          <p className="text-balance text-sm font-medium">{title}</p>
          {subtitle ? (
            <p className="truncate font-mono text-xs text-muted-foreground" title={subtitle}>
              {subtitle}
            </p>
          ) : null}
        </div>
      </div>

      <p className="pointer-events-none relative z-10 line-clamp-2 min-h-10 text-sm text-muted-foreground">
        {description || t("skills.picker.noDescription")}
      </p>

      <p className="pointer-events-none relative z-10 flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1 text-[11px] text-muted-foreground">
        {source === "local" ? <span>{t("library.card.source.local")}</span> : null}
        {source === "git" ? (
          <span className="font-mono" title={gitSha}>
            {t("library.card.source.git", { sha: (gitSha ?? "").slice(0, 7) })}
          </span>
        ) : null}
        {source === "marketplace" ? (
          <>
            <span className="font-mono">{t("library.card.source.marketplace")}</span>
            <span aria-hidden>·</span>
            <span className="min-w-0 truncate">
              {author
                ? t("marketplace.card.by", { author })
                : t("marketplace.card.noAuthor")}
            </span>
          </>
        ) : null}
      </p>

      <div className="pointer-events-none relative z-10 mt-auto flex flex-wrap items-center justify-between gap-2">
        <div className="pointer-events-none flex min-w-0 flex-wrap items-center gap-1.5">
          {version ? (
            <Badge variant="secondary" className="font-mono text-[11px] font-normal">
              {version}
            </Badge>
          ) : null}
          {state === "installed" ? (
            <Badge variant="secondary" className="text-[11px] font-normal">
              {t("library.card.state.installed")}
            </Badge>
          ) : null}
          {state === "upToDate" ? (
            <Badge
              variant="outline"
              className="border-emerald-600/40 text-[11px] font-normal text-emerald-700 dark:text-emerald-400"
            >
              {t("library.card.state.upToDate")}
            </Badge>
          ) : null}
          {state === "update" ? (
            <Badge
              variant="outline"
              className="gap-1 border-sky-600/40 text-[11px] font-normal text-sky-700 dark:text-sky-400"
              title={t("library.card.state.updateTitle", { version: updateVersion ?? "" })}
            >
              <ArrowUpCircle aria-hidden />
              {t("library.card.state.update")}
            </Badge>
          ) : null}
          {agents !== undefined ? (
            <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
              <Users aria-hidden className="size-3" />
              {t(
                agents === 1 ? "library.card.agentsOne" : "library.card.agentsMany",
                { count: agents },
              )}
            </span>
          ) : null}
          {extraMeta}
        </div>
        {action ? <div className="pointer-events-auto shrink-0">{action}</div> : null}
      </div>
    </li>
  );
}

/** Action sizing shared by every card: 44px on touch, compact on pointer screens. */
export const libraryActionClass = "h-11 sm:h-8";
