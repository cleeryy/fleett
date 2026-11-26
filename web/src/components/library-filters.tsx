import { useRef } from "react";
import { Search, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Segmented } from "@/components/segmented";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

export const LIBRARY_TYPES = ["all", "templates", "personas", "skills", "connectors"] as const;

export type LibraryType = (typeof LIBRARY_TYPES)[number];

export type LibrarySkillSource = "all" | "local" | "marketplace";

export function parseLibraryType(raw: string | null): LibraryType {
  return LIBRARY_TYPES.includes(raw as LibraryType) ? (raw as LibraryType) : "all";
}

/**
 * Pills as real tabs: roving tabindex, arrow keys and `aria-controls` pointing
 * at the single results panel. Counts keep a fixed slot so a late answer never
 * shifts the row.
 */
export function LibraryFilters({
  type,
  counts,
  onTypeChange,
  query,
  onQueryChange,
  skillSource,
  onSkillSourceChange,
}: {
  type: LibraryType;
  counts: Partial<Record<LibraryType, number>>;
  onTypeChange: (next: LibraryType) => void;
  query: string;
  onQueryChange: (next: string) => void;
  skillSource: LibrarySkillSource;
  onSkillSourceChange: (next: LibrarySkillSource) => void;
}) {
  const { t } = useI18n();
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);

  function moveFocus(index: number) {
    const next = LIBRARY_TYPES[(index + LIBRARY_TYPES.length) % LIBRARY_TYPES.length];
    onTypeChange(next);
    tabRefs.current[LIBRARY_TYPES.indexOf(next)]?.focus();
  }

  return (
    <div className="space-y-3">
      <div className="-mx-1 px-1">
        <div
          role="tablist"
          aria-label={t("library.pills.label")}
          className="flex flex-wrap items-center gap-1.5 sm:w-fit sm:flex-nowrap"
        >
          {LIBRARY_TYPES.map((value, index) => {
            const active = value === type;
            const count = counts[value];
            return (
              <button
                key={value}
                ref={(node) => {
                  tabRefs.current[index] = node;
                }}
                type="button"
                role="tab"
                id={`library-tab-${value}`}
                aria-selected={active}
                aria-controls="library-results"
                tabIndex={active ? 0 : -1}
                onClick={() => onTypeChange(value)}
                onKeyDown={(event) => {
                  if (event.key === "ArrowRight") {
                    event.preventDefault();
                    moveFocus(index + 1);
                  } else if (event.key === "ArrowLeft") {
                    event.preventDefault();
                    moveFocus(index - 1);
                  } else if (event.key === "Home") {
                    event.preventDefault();
                    moveFocus(0);
                  } else if (event.key === "End") {
                    event.preventDefault();
                    moveFocus(LIBRARY_TYPES.length - 1);
                  }
                }}
                className={cn(
                  "inline-flex min-h-11 shrink-0 items-center gap-1.5 rounded-full border px-3 text-xs outline-none transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 sm:min-h-8",
                  active
                    ? "border-foreground/30 bg-foreground text-background"
                    : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                )}
              >
                {t(`library.pill.${value}`)}
                <span
                  aria-hidden
                  className="inline-block w-6 text-right font-mono text-[11px] tabular-nums opacity-70"
                >
                  {count ?? ""}
                </span>
              </button>
            );
          })}
        </div>
      </div>

      <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
        <div className="relative min-w-0 flex-1 sm:max-w-md">
          <Search
            aria-hidden
            className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground"
          />
          <Input
            type="text"
            value={query}
            onChange={(event) => onQueryChange(event.target.value)}
            placeholder={t("library.searchPlaceholder")}
            aria-label={t("library.searchLabel")}
            autoComplete="off"
            spellCheck={false}
            className="h-11 pl-9 pr-12 sm:h-9"
          />
          {query ? (
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              className="absolute top-1/2 right-1.5 size-11 -translate-y-1/2 sm:size-7"
              onClick={() => onQueryChange("")}
              aria-label={t("library.clearSearch")}
              title={t("library.clearSearch")}
            >
              <X aria-hidden />
            </Button>
          ) : null}
        </div>

        {type === "skills" ? (
          <Segmented
            value={skillSource}
            onChange={onSkillSourceChange}
            label={t("library.source.label")}
            options={[
              { value: "all", label: t("library.source.all") },
              { value: "local", label: t("library.source.local") },
              { value: "marketplace", label: t("library.source.marketplace") },
            ]}
          />
        ) : null}
      </div>
    </div>
  );
}
