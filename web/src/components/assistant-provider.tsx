import { useEffect, useRef, useState } from "react";
import { Loader2, Plug, Plus, Sparkles, TriangleAlert } from "lucide-react";
import { api } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
import type { AssistantConfig, AssistantProviderSource, ProviderSummary } from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
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
import { toast } from "sonner";
import {
  PROVIDER_SOURCE_HINT_KEY,
  PROVIDER_SOURCE_KEY,
  assistantProviderState,
  type AssistantProviderState,
} from "@/lib/assistant-provider";
import { ASSISTANT_CONFIG_EVENT, notifyAssistantConfigChanged } from "@/lib/events";
import { useI18n } from "@/lib/i18n";
import { hrefFor, useRoute } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

/** Small "env" / "selected" / "auto" note next to the provider chip. */
export function ProviderSourceLabel({ source }: { source: AssistantProviderSource | null }) {
  const { t } = useI18n();
  if (!source || source === "none") return null;
  return (
    <span
      className="text-[11px] text-muted-foreground"
      title={t(PROVIDER_SOURCE_HINT_KEY[source])}
    >
      {t(PROVIDER_SOURCE_KEY[source])}
    </span>
  );
}

/**
 * Select + save for the provider the assistant borrows. Shared by the panel
 * and the Providers page so both surfaces write the same setting. Viewers and
 * admins without a candidate provider get a sentence instead of a dead form.
 */
export function AssistantProviderForm({
  providers,
  current,
  onSaved,
  id,
  className,
}: {
  providers: ProviderSummary[];
  /** Provider already resolved; the select starts on it. */
  current?: string;
  onSaved: (config: AssistantConfig) => void;
  id?: string;
  className?: string;
}) {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const [selected, setSelected] = useState(current ?? "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  if (!canEdit) {
    return <p className="text-xs text-muted-foreground">{t("assistant.providerAdminOnly")}</p>;
  }
  if (providers.length === 0) return null;

  async function save(): Promise<void> {
    if (!selected || saving) return;
    setSaving(true);
    setError(null);
    try {
      const value = await api.patchAssistantConfig({ provider: selected });
      const name = providers.find((provider) => provider.id === selected)?.name ?? selected;
      onSaved(value);
      notifyAssistantConfigChanged();
      toast.success(t("assistant.providerSaved", { name }));
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className={cn("space-y-1.5", className)}>
      <div className="flex flex-col gap-2 sm:flex-row">
        <Select value={selected} onValueChange={setSelected} disabled={saving}>
          <SelectTrigger
            id={id}
            size="sm"
            className="w-full sm:flex-1"
            aria-label={t("assistant.providerField")}
          >
            <SelectValue placeholder={t("assistant.providerPlaceholder")} />
          </SelectTrigger>
          <SelectContent>
            {providers.map((provider) => (
              <SelectItem key={provider.id} value={provider.id}>
                {provider.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button
          type="button"
          size="sm"
          className="w-full shrink-0 sm:w-auto"
          onClick={() => void save()}
          disabled={!selected || saving}
        >
          {saving ? <Loader2 aria-hidden className="animate-spin" /> : null}
          {t("common.save")}
        </Button>
      </div>
      {error ? (
        <p role="alert" className="text-xs text-destructive">
          {apiErrorMessage(error, t)}
        </p>
      ) : null}
    </div>
  );
}

/**
 * Banner above the transcript while no provider answers. `not_selected` offers
 * the select, `no_provider` links to creation and `unknown` repairs a `422`.
 * `resolved` renders nothing: the header chip is enough.
 */
export function AssistantProviderNotice({
  state,
  providers,
  onSaved,
  onNavigate,
  className,
}: {
  state: AssistantProviderState;
  providers: ProviderSummary[];
  onSaved: (config: AssistantConfig) => void;
  onNavigate?: () => void;
  className?: string;
}) {
  const { t } = useI18n();
  const canEdit = useCanEdit();

  if (state.kind === "resolved") return null;

  const valid =
    state.kind === "unknown" && state.valid.length > 0
      ? providers.filter((provider) => state.valid.includes(provider.id))
      : providers;
  const candidates = valid.length > 0 ? valid : providers;

  const unknown = state.kind === "unknown";
  const title =
    state.kind === "not_selected"
      ? t("assistant.notSelectedTitle")
      : unknown
        ? t("assistant.providerUnknownTitle")
        : t("assistant.noProviderTitle");
  const body =
    state.kind === "not_selected"
      ? t("assistant.notSelectedBody")
      : unknown
        ? t("assistant.providerUnknownBody")
        : t("assistant.noProviderBody");

  return (
    <div className={cn("shrink-0 px-4 pt-3", className)}>
      <Alert variant={unknown ? "destructive" : "default"} className="items-start">
        {unknown ? (
          <TriangleAlert aria-hidden className="mt-0.5" />
        ) : (
          <Plug aria-hidden className="mt-0.5" />
        )}
        <AlertTitle>{title}</AlertTitle>
        <AlertDescription className="space-y-2">
          <p>{body}</p>
          {state.kind === "no_provider" ? (
            canEdit ? (
              <Button asChild variant="outline" size="sm">
                <a href={hrefFor("/providers?new=1")} onClick={onNavigate}>
                  {t("assistant.openProviders")}
                </a>
              </Button>
            ) : (
              <p className="text-xs">{t("assistant.providerAdminOnly")}</p>
            )
          ) : candidates.length > 0 ? (
            <AssistantProviderForm providers={candidates} onSaved={onSaved} />
          ) : (
            <Button asChild variant="outline" size="sm">
              <a href={hrefFor("/providers")} onClick={onNavigate}>
                {t("assistant.openProviders")}
              </a>
            </Button>
          )}
        </AlertDescription>
      </Alert>
    </div>
  );
}

/**
 * Providers page card: the same setting, with room for a label and a sentence.
 * Loads the config itself so the panel and the page stay independent.
 */
export function AssistantProviderCard({
  providers,
  providersLoading = false,
  nonce = 0,
  onCreate,
  className,
}: {
  providers: ProviderSummary[];
  providersLoading?: boolean;
  nonce?: number;
  onCreate: () => void;
  className?: string;
}) {
  const { t } = useI18n();
  const route = useRoute();
  const canEdit = useCanEdit();
  const config = useResource(() => api.assistantConfig(), [nonce]);
  const sectionRef = useRef<HTMLElement>(null);
  const focusRequested = route.query.get("assistant") === "1";

  useEffect(() => {
    const reload = () => config.reload();
    window.addEventListener(ASSISTANT_CONFIG_EVENT, reload);
    return () => window.removeEventListener(ASSISTANT_CONFIG_EVENT, reload);
  }, [config.reload]);

  const awaitingProviders = config.data !== null && config.data.provider === null && !config.data.reason;
  const state = awaitingProviders && providersLoading
    ? null
    : assistantProviderState(config.data, config.error, providers);
  const loaded = !config.loading && (config.data !== null || config.error !== null);

  useEffect(() => {
    if (!focusRequested || !loaded) return;
    const node = sectionRef.current;
    if (!node) return;
    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    node.scrollIntoView({ behavior: reduce ? "auto" : "smooth", block: "start" });
    node.focus({ preventScroll: true });
  }, [focusRequested, loaded]);

  if (!loaded) {
    return <Skeleton className={cn("h-24 w-full rounded-lg", className)} />;
  }
  if (!state) return null;
  if (config.data && !config.data.enabled) return null;
  if (config.error && config.error.code !== "assistant_provider_unknown") return null;

  const valid =
    state.kind === "unknown" && state.valid.length > 0
      ? providers.filter((provider) => state.valid.includes(provider.id))
      : providers;
  const candidates = valid.length > 0 ? valid : providers;
  const unknown = state.kind === "unknown";
  const current = state.kind === "resolved" ? state.provider : null;

  return (
    <section
      ref={sectionRef}
      id="assistant"
      tabIndex={-1}
      aria-labelledby="assistant-provider-title"
      className={cn(
        "scroll-mt-20 rounded-lg border p-4 outline-none sm:p-5",
        unknown && "border-destructive/40",
        className,
      )}
    >
      <div className="flex items-start gap-3">
        <Sparkles aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2 id="assistant-provider-title" className="text-sm font-medium">
              {t("assistant.providerCardTitle")}
            </h2>
            {current ? (
              <Badge variant="secondary" className="max-w-48 font-normal">
                <span className="truncate">{current.name}</span>
              </Badge>
            ) : (
              <Badge
                variant="outline"
                className={cn(
                  "font-normal",
                  unknown && "border-destructive/40 text-destructive",
                )}
              >
                {state.kind === "not_selected"
                  ? t("assistant.providerNotSelected")
                  : unknown
                    ? t("assistant.providerMissing")
                    : t("assistant.noProvider")}
              </Badge>
            )}
            {state.kind === "resolved" ? <ProviderSourceLabel source={state.source} /> : null}
          </div>
          <p className="text-xs text-muted-foreground">
            {state.kind === "resolved"
              ? t("assistant.providerCardResolved")
              : state.kind === "not_selected"
                ? t("assistant.notSelectedBody")
                : unknown
                  ? t("assistant.providerUnknownBody")
                  : t("assistant.noProviderBody")}
          </p>
          {state.kind === "no_provider" ? (
            canEdit ? null : (
              <p className="text-xs text-muted-foreground">{t("assistant.providerAdminOnly")}</p>
            )
          ) : providersLoading && candidates.length === 0 ? (
            <Skeleton className="mt-1.5 h-8 w-56" />
          ) : candidates.length > 0 ? (
            <div className="pt-1.5">
              {canEdit ? (
                <Label
                  htmlFor="assistant-provider"
                  className="mb-1.5 block text-xs text-muted-foreground"
                >
                  {t("assistant.providerField")}
                </Label>
              ) : null}
              <AssistantProviderForm
                id="assistant-provider"
                providers={candidates}
                current={current?.id}
                onSaved={(value) => config.setData(() => value)}
                className="max-w-xl"
              />
            </div>
          ) : canEdit ? (
            <div className="pt-1.5">
              <Button type="button" variant="outline" size="sm" onClick={onCreate}>
                <Plus aria-hidden />
                {t("providers.form.newTitle")}
              </Button>
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">{t("assistant.providerAdminOnly")}</p>
          )}
        </div>
      </div>
    </section>
  );
}
