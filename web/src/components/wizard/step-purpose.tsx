import { Code2, MessageSquare, MoreHorizontal, Rss } from "lucide-react";
import type { ApiError } from "@/api/errors";
import type { Template } from "@/api/types";
import { ErrorAlert } from "@/components/error-alert";
import { RichText } from "@/components/rich-text";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { FieldError, StepHeading, type StepFields } from "@/components/wizard/wizard-step";
import { usageDefaults, type WizardUsage } from "@/lib/agent-draft";
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";
import { cn } from "@/lib/utils";

const USAGES = [
  { value: "discord", icon: MessageSquare },
  { value: "watch", icon: Rss },
  { value: "dev", icon: Code2 },
  { value: "other", icon: MoreHorizontal },
] as const satisfies ReadonlyArray<{ value: WizardUsage; icon: typeof MessageSquare }>;

export function StepPurpose({
  draft,
  patch,
  patchWith,
  errors,
  showErrors,
  templates,
  loading,
  error,
  onRetry,
  selectedTemplate,
  onSelectTemplate,
}: StepFields & {
  templates: Template[];
  loading: boolean;
  error: ApiError | null;
  onRetry: () => void;
  selectedTemplate: Template | null;
  onSelectTemplate: (id: string) => void;
}) {
  const { t } = useI18n();

  function selectUsage(usage: WizardUsage) {
    if (usage === "other") {
      patch({ usage });
      return;
    }
    const defaults = usageDefaults(usage, templates);
    patchWith((current) => {
      const previous = templates.find((candidate) => candidate.id === current.template);
      const keepModel =
        current.model.trim() !== "" && current.model !== previous?.default_model;
      return {
        ...current,
        usage,
        template: defaults.template?.id ?? "",
        model: keepModel ? current.model : (defaults.template?.default_model ?? ""),
        channel: defaults.channel,
        channelTouched: true,
      };
    });
  }

  return (
    <div className="space-y-5">
      <StepHeading
        title={t("wizard.step.purpose.title")}
        description={t("wizard.step.purpose.description")}
      />

      {error ? <ErrorAlert error={error} onRetry={onRetry} /> : null}
      {loading && templates.length === 0 ? (
        <div className="space-y-3">
          {Array.from({ length: 4 }).map((_, index) => (
            <Skeleton key={index} className="h-24 w-full rounded-md" />
          ))}
        </div>
      ) : (
        <RadioGroup
          id="wizard-purpose"
          tabIndex={-1}
          value={draft.usage}
          onValueChange={(value) => selectUsage(value as WizardUsage)}
          aria-label={t("wizard.usage.aria")}
          aria-invalid={showErrors && !!errors.template}
          aria-describedby={
            showErrors && errors.template ? "wizard-purpose-error" : undefined
          }
          className="grid gap-3 sm:grid-cols-2"
        >
          {USAGES.map((usage) => {
            const Icon = usage.icon;
            const selected = draft.usage === usage.value;
            return (
              <div
                key={usage.value}
                onClick={() => selectUsage(usage.value)}
                className={cn(
                  "flex cursor-pointer items-start gap-3 rounded-md border p-4 transition-colors",
                  selected ? "border-primary bg-accent/40" : "hover:bg-muted/50",
                )}
              >
                <RadioGroupItem
                  value={usage.value}
                  id={`wizard-usage-${usage.value}`}
                  className="mt-1"
                  aria-describedby={`wizard-usage-${usage.value}-hint`}
                />
                <div className="min-w-0 flex-1 space-y-1">
                  <div className="flex items-center gap-2">
                    <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                    <Label htmlFor={`wizard-usage-${usage.value}`} className="cursor-pointer">
                      {t(`wizard.usage.${usage.value}.title`)}
                    </Label>
                  </div>
                  <p
                    id={`wizard-usage-${usage.value}-hint`}
                    className="text-sm text-muted-foreground"
                  >
                    {t(`wizard.usage.${usage.value}.hint`)}
                  </p>
                </div>
              </div>
            );
          })}
        </RadioGroup>
      )}

      {draft.usage === "other" ? (
        <div className="space-y-2">
          <Label htmlFor="wizard-template">{t("wizard.usage.otherTemplate")}</Label>
          <Select value={draft.template || undefined} onValueChange={onSelectTemplate}>
            <SelectTrigger
              id="wizard-template"
              className="w-full"
              aria-invalid={showErrors && !!errors.template}
              aria-describedby={
                showErrors && errors.template ? "wizard-template-error" : "wizard-template-help"
              }
            >
              <SelectValue placeholder={t("wizard.usage.otherTemplatePlaceholder")} />
            </SelectTrigger>
            <SelectContent>
              {templates.map((template) => (
                <SelectItem key={template.id} value={template.id}>
                  {template.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <p id="wizard-template-help" className="text-xs text-muted-foreground">
            <RichText text={t("wizard.usage.otherTemplateHint")} />
          </p>
          <FieldError
            id="wizard-template-error"
            message={showErrors ? errors.template : undefined}
          />
        </div>
      ) : (
        <>
          {selectedTemplate ? (
            <p className="text-xs text-muted-foreground">
              {t("wizard.usage.selected", {
                name: selectedTemplate.name,
                model: selectedTemplate.default_model,
              })}
            </p>
          ) : null}
          <FieldError id="wizard-purpose-error" message={showErrors ? errors.template : undefined} />
        </>
      )}

      {!loading && !error && templates.length === 0 ? (
        <p className="rounded-md border px-4 py-4 text-sm text-muted-foreground">
          {t("wizard.usage.noTemplatesLead")}{" "}
          <a href={hrefFor("/templates")} className="underline underline-offset-4">
            {t("wizard.usage.noTemplatesAction")}
          </a>{" "}
          {t("wizard.usage.noTemplatesTail")}
        </p>
      ) : null}
    </div>
  );
}
