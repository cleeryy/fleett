import { useEffect, useState } from "react";
import { Loader2, Plug, RotateCcw, Save, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type {
  AssistantConfig,
  AssistantProviderSource,
  ProviderSummary,
} from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { ErrorAlert } from "@/components/error-alert";
import { ASSISTANT_CONFIG_EVENT } from "@/lib/events";
import { useI18n } from "@/lib/i18n";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

/**
 * `422 assistant_provider_unknown` carries the ids that still resolve, either
 * as plain strings or as `{id,name}` pairs. `null` means "no filter": every
 * provider in the list can be picked.
 */
export function validProviderIds(details: unknown): string[] | null {
  if (!details || typeof details !== "object") return null;
  const raw = (details as { valid?: unknown }).valid;
  if (!Array.isArray(raw)) return null;
  const ids: string[] = [];
  for (const entry of raw) {
    if (typeof entry === "string" && entry.trim()) {
      ids.push(entry.trim());
    } else if (entry && typeof entry === "object") {
      const id = (entry as { id?: unknown }).id;
      if (typeof id === "string" && id.trim()) ids.push(id.trim());
    }
  }
  return ids;
}

/** Small label after the provider chip: where the resolution came from. */
export function AssistantSourceLabel({
  source,
  className,
}: {
  source: AssistantProviderSource;
  className?: string;
}) {
  const { t } = useI18n();
  if (source === "none") return null;
  return (
    <span
      className={cn("text-[11px] text-muted-foreground", className)}
      title={t("assistant.source.explain")}
    >
      {t(`assistant.source.${source}`)}
    </span>
  );
}

function ProviderSelect({
  id,
  options,
  value,
  disabled,
  onChange,
}: {
  id: string;
  options: ProviderSummary[];
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  const { t } = useI18n();
  return (
    <Select value={value} onValueChange={onChange} disabled={disabled}>
      <SelectTrigger id={id} className="w-full sm:max-w-72">
        <SelectValue placeholder={t("assistant.choosePlaceholder")} />
      </SelectTrigger>
      <SelectContent>
        {options.map((provider) => (
          <SelectItem key={provider.id} value={provider.id}>
            {provider.name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

/** Shared data: the provider list, filtered down to the ids that still resolve. */
function useProviderChoices(valid: string[] | null) {
  const providers = useResource(() => api.listProviders(), []);
  const rows = providers.data ?? [];
  const options = valid === null ? rows : rows.filter((row) => valid.includes(row.id));
  return { providers, options };
}

/**
 * The assistant borrows one provider. This block is the single place that
 * changes it: the assistant panel shows it when no provider was picked, and
 * the Providers page shows it for every state (resolved, unselected, broken).
 */
export function AssistantProviderSettings({
  config,
  error,
  canEdit,
  onResolved,
  surface,
}: {
  config: AssistantConfig | null;
  /** Only `422 assistant_provider_unknown` is handled here. */
  error?: ApiError | null;
  canEdit: boolean;
  onResolved: (config: AssistantConfig) => void;
  surface: "panel" | "card";
}) {
  const { t } = useI18n();
  const unknown = error?.code === "assistant_provider_unknown";
  const valid = unknown ? validProviderIds(error?.details) : null;
  const { providers, options } = useProviderChoices(valid);
  const [choice, setChoice] = useState("");
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<ApiError | null>(null);
  /** Latest config broadcast by another surface that saved first. */
  const [shared, setShared] = useState<AssistantConfig | null>(null);

  useEffect(() => {
    function onConfig(event: Event) {
      const detail = (event as CustomEvent<AssistantConfig>).detail;
      if (detail) setShared(detail);
    }
    window.addEventListener(ASSISTANT_CONFIG_EVENT, onConfig);
    return () => window.removeEventListener(ASSISTANT_CONFIG_EVENT, onConfig);
  }, []);

  // A fresh config from the controller supersedes a broadcast from elsewhere.
  useEffect(() => {
    setShared(null);
  }, [config]);

  // A save elsewhere wins until this surface saves again.
  const source = shared ?? config;
  const resolved = source?.provider ?? null;
  const lockedByEnv = source?.provider_source === "env";
  const defaultId = resolved?.id ?? options[0]?.id ?? "";
  const selected = options.some((option) => option.id === choice) ? choice : defaultId;
  const changed = selected !== (resolved?.id ?? "");
  const nothingToPick = providers.loading && !providers.data;

  async function save(provider: string | null): Promise<void> {
    setPending(true);
    setFailure(null);
    try {
      const next = await api.updateAssistantConfig({ provider });
      toast.success(
        provider
          ? t("assistant.chooseSaved", { name: next.provider?.name ?? provider })
          : t("assistant.resetDone"),
      );
      setChoice("");
      setShared(next);
      window.dispatchEvent(
        new CustomEvent<AssistantConfig>(ASSISTANT_CONFIG_EVENT, { detail: next }),
      );
      onResolved(next);
    } catch (cause) {
      setFailure(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  // The caller owns the "no provider at all" message: this block only picks
  // among providers that exist (unselected, broken reference, or resolved).
  const picking =
    unknown || resolved !== null || source?.reason === "not_selected";
  if (!picking) return null;

  const title =
    surface === "card"
      ? t("providers.assistant.title")
      : unknown
        ? t("assistant.repairTitle")
        : t("assistant.chooseTitle");
  const body = unknown
    ? t("assistant.repairBody")
    : resolved && surface === "card"
      ? t("providers.assistant.description")
      : t("assistant.chooseBody");

  const selectId = surface === "panel" ? "assistant-provider-panel" : "assistant-provider-select";

  const form = (
    <div className="mt-3 space-y-2">
      {nothingToPick ? (
        <Skeleton className="h-9 w-full sm:w-72" />
      ) : providers.error ? (
        <ErrorAlert error={providers.error} onRetry={providers.reload} />
      ) : options.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("assistant.chooseEmpty")}</p>
      ) : (
        <div className="flex flex-wrap items-end gap-2">
          <div className="min-w-0 space-y-1.5">
            <Label htmlFor={selectId} className="text-xs">
              {t("assistant.chooseLabel")}
            </Label>
            <ProviderSelect
              id={selectId}
              options={options}
              value={selected}
              disabled={pending || !canEdit || lockedByEnv}
              onChange={setChoice}
            />
          </div>
          {canEdit && !lockedByEnv ? (
            <Button
              type="button"
              size="sm"
              className="h-9"
              disabled={pending || !selected || (!changed && resolved !== null)}
              onClick={() => void save(selected)}
            >
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Save aria-hidden />}
              {t("assistant.chooseSave")}
            </Button>
          ) : null}
          {canEdit && !lockedByEnv && resolved !== null && source?.provider_source === "setting" ? (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              className="h-9"
              disabled={pending}
              onClick={() => void save(null)}
            >
              <RotateCcw aria-hidden />
              {t("assistant.resetToAuto")}
            </Button>
          ) : null}
        </div>
      )}

      {lockedByEnv && canEdit ? (
        <p className="text-xs text-muted-foreground">{t("assistant.source.envLocked")}</p>
      ) : null}
      {!canEdit ? (
        <p className="text-xs text-muted-foreground">{t("assistant.chooseReadOnly")}</p>
      ) : null}
      {failure ? <ErrorAlert error={failure} /> : null}
    </div>
  );

  if (surface === "panel") {
    return (
      <Alert variant={unknown ? "destructive" : "default"} className="items-start">
        <Plug aria-hidden className="mt-0.5" />
        <AlertTitle>{title}</AlertTitle>
        <AlertDescription>
          <p>{body}</p>
          {form}
        </AlertDescription>
      </Alert>
    );
  }

  return (
    <section id="assistant-provider" className="scroll-mt-20 space-y-1">
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
        <h2 className="flex items-center gap-1.5 text-sm font-medium">
          {unknown ? (
            <TriangleAlert aria-hidden className="size-3.5 text-destructive" />
          ) : null}
          {title}
        </h2>
        {resolved ? <AssistantSourceLabel source={source?.provider_source ?? "none"} /> : null}
      </div>
      <p className="text-xs text-muted-foreground">{body}</p>
      {form}
    </section>
  );
}
