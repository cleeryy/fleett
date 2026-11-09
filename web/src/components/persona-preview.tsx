import { useEffect, useState, type ReactNode } from "react";
import { Loader2, RefreshCw } from "lucide-react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { PersonaPreview } from "@/api/types";
import { Button } from "@/components/ui/button";
import { CopyButton } from "@/components/copy-button";
import { Skeleton } from "@/components/ui/skeleton";
import { compactVariables } from "@/lib/personas";
import { cn } from "@/lib/utils";

/**
 * Rendered SOUL.md for a persona. When `instance` is set the controller uses
 * that instance's stored values; otherwise the panel renders `variables` over
 * the persona defaults. Typing is debounced, the previous render stays visible
 * while the next one arrives.
 */
export function PersonaPreviewPanel({
  personaId,
  variables = {},
  instance,
  title = "Rendered SOUL.md",
  description,
  maxHeight = "48vh",
  className,
  refreshKey,
}: {
  personaId: string | null;
  variables?: Record<string, string>;
  instance?: string;
  title?: string;
  description?: ReactNode;
  maxHeight?: string;
  className?: string;
  /** Change this value to re-render after the saved persona changed. */
  refreshKey?: string | number;
}) {
  const [result, setResult] = useState<PersonaPreview | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [loading, setLoading] = useState(false);
  const [nonce, setNonce] = useState(0);
  const variablesKey = JSON.stringify(compactVariables(variables));
  const missing = result?.missing ?? [];

  useEffect(() => {
    if (!personaId) {
      setResult(null);
      setError(null);
      setLoading(false);
      return;
    }
    let alive = true;
    setLoading(true);
    setError(null);
    const payload = instance
      ? { instance }
      : { variables: JSON.parse(variablesKey) as Record<string, string> };
    const timer = window.setTimeout(() => {
      api.previewPersona(personaId, payload).then(
        (value) => {
          if (!alive) return;
          setResult(value);
          setLoading(false);
        },
        (cause: unknown) => {
          if (!alive) return;
          setError(toApiError(cause));
          setLoading(false);
        },
      );
    }, 250);
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [personaId, instance, variablesKey, nonce, refreshKey]);

  return (
    <div className={cn("overflow-hidden rounded-md border", className)}>
      <div className="flex flex-wrap items-center justify-between gap-2 border-b bg-muted/40 px-3 py-2">
        <div className="min-w-0">
          <p className="text-sm font-medium">{title}</p>
          {description ? (
            <p className="text-xs text-muted-foreground">{description}</p>
          ) : null}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {loading ? (
            <span
              aria-live="polite"
              className="flex items-center gap-1.5 text-xs text-muted-foreground"
            >
              <Loader2 aria-hidden className="size-3.5 animate-spin" />
              Rendering…
            </span>
          ) : (
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label="Refresh preview"
              title="Refresh preview"
              onClick={() => setNonce((value) => value + 1)}
              disabled={!personaId}
            >
              <RefreshCw aria-hidden />
            </Button>
          )}
          <CopyButton
            value={result?.rendered ?? ""}
            label="Copy rendered SOUL.md"
            size="icon-xs"
            disabled={!result?.rendered}
          />
        </div>
      </div>

      {!personaId ? (
        <p className="px-4 py-8 text-sm text-muted-foreground">
          Pick a persona to see the rendered SOUL.md.
        </p>
      ) : error ? (
        <div className="space-y-2 px-4 py-4">
          <p className="text-sm text-destructive">Preview unavailable: {error.message}</p>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setNonce((value) => value + 1)}
          >
            <RefreshCw aria-hidden />
            Retry
          </Button>
        </div>
      ) : result ? (
        <>
          <pre
            tabIndex={0}
            role="region"
            aria-label={title}
            className="overflow-auto bg-muted/10 p-4 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words"
            style={{ maxHeight }}
          >
            {result.rendered || "This SOUL.md is empty."}
          </pre>
          {missing.length > 0 ? (
            <p className="border-t px-3 py-2 text-xs text-amber-700 dark:text-amber-400">
              Missing: {missing.map((key) => `{{${key}}}`).join(", ")}. They stay visible in the
              text until a value or a default is set.
            </p>
          ) : null}
          {loading ? (
            <div className="border-t px-3 py-1.5">
              <div className="h-1 w-16 animate-pulse rounded-full bg-muted" aria-hidden />
            </div>
          ) : null}
        </>
      ) : (
        <div className="space-y-2 p-4">
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-4 w-3/4" />
        </div>
      )}
    </div>
  );
}
