import { useEffect, useState, type ReactNode } from "react";
import { Loader2, RefreshCw } from "lucide-react";
import { api } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
import type { PersonaPreview } from "@/api/types";
import { Button } from "@/components/ui/button";
import { CopyButton } from "@/components/copy-button";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/lib/i18n";
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
  title,
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
  const { t } = useI18n();
  const [result, setResult] = useState<PersonaPreview | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [loading, setLoading] = useState(false);
  const [nonce, setNonce] = useState(0);
  const variablesKey = JSON.stringify(compactVariables(variables));
  const missing = result?.missing ?? [];
  const heading = title ?? t("personas.preview.title");

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
          <p className="text-sm font-medium">{heading}</p>
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
              {t("personas.preview.rendering")}
            </span>
          ) : (
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label={t("personas.preview.refresh")}
              title={t("personas.preview.refresh")}
              onClick={() => setNonce((value) => value + 1)}
              disabled={!personaId}
            >
              <RefreshCw aria-hidden />
            </Button>
          )}
          <CopyButton
            value={result?.rendered ?? ""}
            label={t("personas.preview.copy")}
            size="icon-xs"
            disabled={!result?.rendered}
          />
        </div>
      </div>

      {!personaId ? (
        <p className="px-4 py-8 text-sm text-muted-foreground">
          {t("personas.preview.pick")}
        </p>
      ) : error ? (
        <div className="space-y-2 px-4 py-4">
          <p className="text-sm text-destructive">
            {t("personas.preview.unavailable", { message: apiErrorMessage(error, t) })}
          </p>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setNonce((value) => value + 1)}
          >
            <RefreshCw aria-hidden />
            {t("common.retry")}
          </Button>
        </div>
      ) : result ? (
        <>
          <pre
            tabIndex={0}
            role="region"
            aria-label={heading}
            className="overflow-auto bg-muted/10 p-4 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words"
            style={{ maxHeight }}
          >
            {result.rendered || t("personas.preview.empty")}
          </pre>
          {missing.length > 0 ? (
            <p className="border-t px-3 py-2 text-xs text-amber-700 dark:text-amber-400">
              {t("personas.preview.missing", {
                keys: missing.map((key) => `{{${key}}}`).join(", "),
              })}
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
