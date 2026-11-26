import { cn } from "@/lib/utils";

/**
 * Two or three short options in one track, the same control the skill viewer
 * uses. Buttons instead of tabs: the choice is a view of one document, not a
 * navigation step.
 */
export function Segmented<T extends string>({
  value,
  options,
  onChange,
  label,
  className,
  size = "sm",
  fullWidth = false,
}: {
  value: T;
  options: Array<{ value: T; label: string; title?: string }>;
  onChange: (next: T) => void;
  label: string;
  className?: string;
  /** `md` meets the 44px touch target used in the mobile drawer. */
  size?: "sm" | "md";
  fullWidth?: boolean;
}) {
  return (
    <div
      role="group"
      aria-label={label}
      className={cn(
        "flex items-center gap-0.5 rounded-md bg-muted p-0.5",
        fullWidth && "w-full",
        className,
      )}
    >
      {options.map((option) => {
        const selected = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            aria-pressed={selected}
            title={option.title}
            onClick={() => onChange(option.value)}
            className={cn(
              "rounded outline-none focus-visible:ring-2 focus-visible:ring-ring/50",
              size === "sm" ? "px-2 py-0.5 text-xs" : "min-h-11 px-3 py-1.5 text-sm",
              fullWidth && "flex-1",
              selected
                ? "bg-background font-medium text-foreground shadow-xs"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}
