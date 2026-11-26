import type { ApiError } from "@/api/errors";
import type { PersonaSummary } from "@/api/types";
import { ErrorAlert } from "@/components/error-alert";
import { PersonaPreviewPanel } from "@/components/persona-preview";
import { PersonaSelect } from "@/components/persona-select";
import { PersonaVariableInputs } from "@/components/persona-vars";
import { RichText } from "@/components/rich-text";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { FieldError, StepHeading, type StepFields } from "@/components/wizard/wizard-step";
import type { AgentLanguage } from "@/lib/agent-draft";
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";
import { slugify } from "@/lib/validation";

export function StepIdentity({
  draft,
  patch,
  patchWith,
  errors,
  showErrors,
  personas,
  personasLoading,
  personasError,
  onRetryPersonas,
  selectedPersona,
  onSelectPersona,
  onSelectLanguage,
  serverIdError,
}: StepFields & {
  personas: PersonaSummary[];
  personasLoading: boolean;
  personasError: ApiError | null;
  onRetryPersonas: () => void;
  selectedPersona: PersonaSummary | null;
  onSelectPersona: (id: string) => void;
  onSelectLanguage: (language: AgentLanguage) => void;
  serverIdError: string | null;
}) {
  const { t } = useI18n();
  const idMessage = serverIdError ?? (showErrors ? errors.id : undefined);

  return (
    <div className="space-y-5">
      <StepHeading
        title={t("wizard.step.identity.title")}
        description={t("wizard.step.identity.description")}
      />

      <div className="space-y-2">
        <Label htmlFor="wizard-name">{t("wizard.identity.name")}</Label>
        <Input
          id="wizard-name"
          value={draft.name}
          autoComplete="off"
          placeholder={t("wizard.identity.namePlaceholder")}
          aria-invalid={showErrors && !!errors.name}
          aria-describedby={showErrors && errors.name ? "wizard-name-error" : undefined}
          onChange={(event) =>
            patchWith((current) => ({
              ...current,
              name: event.target.value,
              id: slugify(event.target.value),
              idTouched: false,
            }))
          }
        />
        <FieldError id="wizard-name-error" message={showErrors ? errors.name : undefined} />
      </div>

      <div className="space-y-2">
        <Label htmlFor="wizard-id">{t("wizard.identity.slug")}</Label>
        <Input
          id="wizard-id"
          value={draft.id}
          readOnly
          spellCheck={false}
          className="font-mono"
          aria-invalid={!!idMessage}
          aria-describedby={idMessage ? "wizard-id-error" : "wizard-id-help"}
        />
        <p id="wizard-id-help" className="text-xs text-muted-foreground">
          <RichText text={t("wizard.identity.slugHint")} />
        </p>
        <FieldError id="wizard-id-error" message={idMessage} />
      </div>

      <div className="space-y-2">
        <p id="wizard-language-label" className="text-sm font-medium">
          {t("wizard.identity.language")}
        </p>
        <RadioGroup
          value={draft.language}
          onValueChange={(value) => onSelectLanguage(value as AgentLanguage)}
          aria-labelledby="wizard-language-label"
          className="flex flex-wrap gap-x-6 gap-y-2 py-1"
        >
          <div className="flex items-center gap-2">
            <RadioGroupItem value="fr" id="wizard-language-fr" />
            <Label htmlFor="wizard-language-fr" className="cursor-pointer font-normal">
              {t("wizard.language.fr")}
            </Label>
          </div>
          <div className="flex items-center gap-2">
            <RadioGroupItem value="en" id="wizard-language-en" />
            <Label htmlFor="wizard-language-en" className="cursor-pointer font-normal">
              {t("wizard.language.en")}
            </Label>
          </div>
        </RadioGroup>
        <p className="text-xs text-muted-foreground">{t("wizard.identity.languageHint")}</p>
      </div>

      <div className="space-y-2" data-persona-block>
        <Label htmlFor="wizard-persona">
          {t("wizard.identity.persona")}{" "}
          <span className="font-normal text-muted-foreground">({t("common.optional")})</span>
        </Label>
        {personasError ? (
          <ErrorAlert error={personasError} onRetry={onRetryPersonas} />
        ) : (
          <PersonaSelect
            id="wizard-persona"
            value={draft.persona}
            onChange={onSelectPersona}
            personas={personas}
            loading={personasLoading}
            describedBy="wizard-persona-hint"
          />
        )}
        <p id="wizard-persona-hint" className="text-xs text-muted-foreground">
          <RichText text={t("wizard.identity.personaHint")} />
        </p>
        <p className="text-xs text-muted-foreground">
          <a href={hrefFor("/personas")} className="underline underline-offset-4">
            {t("wizard.identity.managePersonas")}
          </a>
        </p>

        {selectedPersona ? (
          <div className="space-y-4 pt-1">
            <div className="rounded-md border px-3 py-2.5">
              <p className="truncate text-sm">
                <span className="font-medium">{selectedPersona.name}</span>
                <span className="text-muted-foreground"> — {selectedPersona.description}</span>
              </p>
              <p className="text-xs text-muted-foreground">
                {t(
                  selectedPersona.variables.length === 1
                    ? "wizard.identity.personaVarsOne"
                    : "wizard.identity.personaVarsMany",
                  { count: selectedPersona.variables.length },
                )}
              </p>
            </div>

            {selectedPersona.variables.length > 0 ? (
              <div className="space-y-2">
                <p className="text-sm font-medium">{t("wizard.identity.values")}</p>
                <PersonaVariableInputs
                  variables={selectedPersona.variables}
                  values={draft.personaVars}
                  onChange={(personaVars) => patch({ personaVars })}
                  idPrefix="wizard-persona-var"
                />
              </div>
            ) : null}

            <details className="rounded-md border">
              <summary className="cursor-pointer px-3 py-2 text-sm font-medium select-none">
                {t("wizard.identity.preview")}
              </summary>
              <div className="border-t p-3">
                <PersonaPreviewPanel
                  personaId={selectedPersona.id}
                  variables={draft.personaVars}
                  description={t("wizard.identity.previewHint")}
                  maxHeight="32vh"
                />
              </div>
            </details>
          </div>
        ) : personasLoading ? (
          <Skeleton className="h-10 w-full rounded-md" />
        ) : (
          <p className="rounded-md border px-4 py-4 text-sm text-muted-foreground">
            {t("wizard.identity.noPersona")}
          </p>
        )}
      </div>

      <div className="space-y-2">
        <Label htmlFor="wizard-notes">
          {t("wizard.identity.notes")}{" "}
          <span className="font-normal text-muted-foreground">({t("common.optional")})</span>
        </Label>
        <Textarea
          id="wizard-notes"
          rows={3}
          value={draft.notes}
          placeholder={t("wizard.identity.notesPlaceholder")}
          onChange={(event) => patch({ notes: event.target.value })}
        />
      </div>
    </div>
  );
}
