import { useEffect, useState } from "react";
import { Check, Loader2, Plus, X } from "lucide-react";
import { api } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
import type { ChannelType, ProviderSummary, ProviderTestResult } from "@/api/types";
import { ErrorAlert } from "@/components/error-alert";
import { ProviderFormDialog } from "@/components/provider-form-dialog";
import { ProviderSelect } from "@/components/provider-select";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Skeleton } from "@/components/ui/skeleton";
import { FieldError, StepHeading, type StepFields } from "@/components/wizard/wizard-step";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

const CHANNELS: { value: ChannelType; titleKey: string; hintKey: string }[] = [
  { value: "discord", titleKey: "wizard.channel.discord", hintKey: "wizard.channel.discordHint" },
  { value: "telegram", titleKey: "wizard.channel.telegram", hintKey: "wizard.channel.telegramHint" },
  { value: "none", titleKey: "wizard.channel.none", hintKey: "wizard.channel.noneHint" },
];

export function StepConnection({
  draft,
  patch,
  patchWith,
  errors,
  showErrors,
  providers,
  providersLoading,
  providersError,
  onRetryProviders,
  selectedProvider,
  onSelectProvider,
  onProviderSaved,
}: StepFields & {
  providers: ProviderSummary[];
  providersLoading: boolean;
  providersError: ApiError | null;
  onRetryProviders: () => void;
  selectedProvider: ProviderSummary | null;
  onSelectProvider: (id: string) => void;
  onProviderSaved: (provider: ProviderSummary) => void;
}) {
  const { t } = useI18n();
  const [dialogOpen, setDialogOpen] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<ProviderTestResult | null>(null);
  const [testError, setTestError] = useState<ApiError | null>(null);

  useEffect(() => {
    setTestResult(null);
    setTestError(null);
  }, [draft.provider]);

  async function testProvider() {
    if (!draft.provider) return;
    setTesting(true);
    setTestResult(null);
    setTestError(null);
    try {
      setTestResult(await api.testProvider(draft.provider));
    } catch (cause) {
      setTestError(toApiError(cause));
    } finally {
      setTesting(false);
    }
  }

  return (
    <div className="space-y-5">
      <StepHeading
        title={t("wizard.step.connection.title")}
        description={t("wizard.step.connection.description")}
      />

      <div className="space-y-2">
        {providers.length === 0 ? (
          <p className="text-sm font-medium">{t("wizard.connection.provider")}</p>
        ) : (
          <Label htmlFor="wizard-provider">{t("wizard.connection.provider")}</Label>
        )}
        {providersError && providers.length === 0 ? (
          <ErrorAlert error={providersError} onRetry={onRetryProviders} />
        ) : null}
        {providersLoading && providers.length === 0 ? (
          <Skeleton className="h-9 w-full rounded-md" />
        ) : providers.length === 0 ? (
          <div className="space-y-3 rounded-md border border-dashed p-4">
            <p className="text-sm font-medium">{t("wizard.connection.noProviderTitle")}</p>
            <p className="text-sm text-muted-foreground">{t("wizard.connection.noProviderHint")}</p>
            <Button
              type="button"
              variant="outline"
              className="h-11 sm:h-9"
              onClick={() => setDialogOpen(true)}
            >
              <Plus aria-hidden />
              {t("wizard.connection.createProvider")}
            </Button>
            <p className="text-xs text-muted-foreground">
              {t("wizard.connection.noProviderContinue")}
            </p>
          </div>
        ) : (
          <>
            <ProviderSelect
              id="wizard-provider"
              value={draft.provider}
              onChange={onSelectProvider}
              providers={providers}
              describedBy="wizard-provider-hint"
            />
            <p id="wizard-provider-hint" className="text-xs text-muted-foreground">
              {t("wizard.connection.providerHint")}
            </p>
            {providersError ? (
              <p className="text-xs text-destructive">
                {t("wizard.connection.providersLoadFailed", {
                  message: apiErrorMessage(providersError, t),
                })}{" "}
                <button
                  type="button"
                  className="underline underline-offset-4"
                  onClick={onRetryProviders}
                >
                  {t("common.retry")}
                </button>
              </p>
            ) : null}
          </>
        )}

        {selectedProvider ? (
          <div className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-md border bg-muted/20 px-3 py-2.5">
            <p className="min-w-0 flex-1 text-sm">
              {t("wizard.connection.usesProvider", { name: selectedProvider.name })}
            </p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="h-11 sm:h-8"
              onClick={() => void testProvider()}
              disabled={testing}
            >
              {testing ? <Loader2 aria-hidden className="animate-spin" /> : null}
              {testing ? t("wizard.connection.testing") : t("wizard.connection.test")}
            </Button>
            <span
              role="status"
              aria-live="polite"
              className={cn(
                "inline-flex items-center gap-1 text-xs",
                testResult
                  ? testResult.ok
                    ? "text-emerald-600 dark:text-emerald-400"
                    : "text-destructive"
                  : "text-muted-foreground",
              )}
            >
              {testing ? null : testResult ? (
                testResult.ok ? (
                  <>
                    <Check aria-hidden className="size-3.5" />
                    {t("wizard.connection.testOk", { ms: testResult.latency_ms })}
                  </>
                ) : (
                  <>
                    <X aria-hidden className="size-3.5" />
                    {t("wizard.connection.testFailed")}
                  </>
                )
              ) : null}
            </span>
          </div>
        ) : providers.length > 0 ? (
          <p className="text-xs text-muted-foreground">
            {t("wizard.connection.noProviderSelected")}
          </p>
        ) : null}

        {testResult && !testResult.ok && testResult.error ? (
          <p className="font-mono text-xs break-words whitespace-pre-wrap text-destructive">
            {testResult.error}
          </p>
        ) : null}
        {testError ? <ErrorAlert error={testError} /> : null}
      </div>

      <div className="space-y-2">
        <Label htmlFor="wizard-model">{t("wizard.connection.model")}</Label>
        <Input
          id="wizard-model"
          value={draft.model}
          autoComplete="off"
          spellCheck={false}
          className="font-mono"
          placeholder="deepseek-v4.1-flash"
          aria-invalid={showErrors && !!errors.model}
          aria-describedby={showErrors && errors.model ? "wizard-model-error" : "wizard-model-help"}
          onChange={(event) => patch({ model: event.target.value })}
        />
        <p id="wizard-model-help" className="text-xs text-muted-foreground">
          {t("wizard.connection.modelHint")}
        </p>
        <FieldError id="wizard-model-error" message={showErrors ? errors.model : undefined} />
      </div>

      <div className="space-y-2">
        <p id="wizard-channel-label" className="text-sm font-medium">
          {t("wizard.connection.channel")}
        </p>
        <RadioGroup
          value={draft.channel}
          onValueChange={(value) =>
            patchWith((current) => ({
              ...current,
              channel: value as ChannelType,
              channelTouched: true,
            }))
          }
          aria-labelledby="wizard-channel-label"
          className="grid gap-3 sm:grid-cols-3"
        >
          {CHANNELS.map((channel) => {
            const selected = draft.channel === channel.value;
            return (
              <div
                key={channel.value}
                onClick={() =>
                  patchWith((current) => ({
                    ...current,
                    channel: channel.value,
                    channelTouched: true,
                  }))
                }
                className={cn(
                  "cursor-pointer rounded-md border p-3 transition-colors",
                  selected ? "border-primary bg-accent/40" : "hover:bg-muted/50",
                )}
              >
                <div className="flex items-center gap-2">
                  <RadioGroupItem
                    value={channel.value}
                    id={`wizard-channel-${channel.value}`}
                    aria-describedby={`wizard-channel-${channel.value}-hint`}
                  />
                  <Label
                    htmlFor={`wizard-channel-${channel.value}`}
                    className="cursor-pointer"
                  >
                    {t(channel.titleKey)}
                  </Label>
                </div>
                <p
                  id={`wizard-channel-${channel.value}-hint`}
                  className="mt-1 pl-6 text-xs text-muted-foreground"
                >
                  {t(channel.hintKey)}
                </p>
              </div>
            );
          })}
        </RadioGroup>
      </div>

      {draft.channel !== "none" ? (
        <div className="space-y-2">
          <Label htmlFor="wizard-channel-token">{t("wizard.connection.botToken")}</Label>
          <Input
            id="wizard-channel-token"
            type="password"
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
            value={draft.token}
            aria-invalid={showErrors && !!errors.token}
            aria-describedby={
              showErrors && errors.token
                ? "wizard-channel-token-error"
                : "wizard-channel-token-help"
            }
            onChange={(event) => patch({ token: event.target.value })}
          />
          <p id="wizard-channel-token-help" className="text-xs text-muted-foreground">
            {t("wizard.connection.botTokenHint")}
          </p>
          <FieldError
            id="wizard-channel-token-error"
            message={showErrors ? errors.token : undefined}
          />
        </div>
      ) : null}

      <ProviderFormDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        onSaved={onProviderSaved}
      />
    </div>
  );
}
