import type { ReactNode } from "react";
import type { ApiError } from "@/api/errors";
import type {
  EnvSuggestion,
  PersonaSummary,
  ProviderSummary,
  Share,
  SkillSummary,
  Template,
} from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { EnvironmentRows } from "@/components/env-rows";
import { ErrorAlert } from "@/components/error-alert";
import { RichText } from "@/components/rich-text";
import { SharePicker } from "@/components/share-picker";
import { SkillPicker } from "@/components/skill-picker";
import { FieldError, StepHeading, type StepFields } from "@/components/wizard/wizard-step";
import type { EnvRowErrors } from "@/lib/env";
import { useI18n } from "@/lib/i18n";

function SummaryRow({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="grid gap-1 px-4 py-3 sm:grid-cols-[170px_minmax(0,1fr)] sm:gap-4">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="min-w-0 text-sm">{value}</dd>
    </div>
  );
}

export function StepReview({
  draft,
  patch,
  errors,
  showErrors,
  template,
  persona,
  provider,
  shares,
  sharesLoading,
  sharesError,
  onRetryShares,
  skills,
  skillsLoading,
  skillsError,
  onRetrySkills,
  envSuggestions,
  envErrors,
  advanced,
  onAdvancedChange,
  egressError,
  secretsError,
  mcpPreset,
}: StepFields & {
  template: Template | null;
  persona: PersonaSummary | null;
  provider: ProviderSummary | null;
  shares: Share[];
  sharesLoading: boolean;
  sharesError: ApiError | null;
  onRetryShares: () => void;
  skills: SkillSummary[];
  skillsLoading: boolean;
  skillsError: ApiError | null;
  onRetrySkills: () => void;
  envSuggestions: EnvSuggestion[];
  envErrors: Record<number, EnvRowErrors>;
  advanced: boolean;
  onAdvancedChange: (open: boolean) => void;
  egressError: ApiError | null;
  secretsError: ApiError | null;
  mcpPreset: string | null;
}) {
  const { t, lang } = useI18n();
  const memoryMb = Number(draft.memLimit);
  const memory = Number.isFinite(memoryMb) && memoryMb > 0
    ? `${new Intl.NumberFormat(lang === "fr" ? "fr-FR" : "en-US", {
        maximumFractionDigits: 1,
      }).format(memoryMb / 1024)} ${t("wizard.units.gb")}`
    : "—";
  const envHasSecrets = draft.env.some((row) => row.secret);

  return (
    <div className="space-y-5">
      <StepHeading
        title={t("wizard.step.review.title")}
        description={t("wizard.step.review.description")}
      />

      <p className="font-mono text-sm break-words" aria-label={t("wizard.review.summaryAria")}>
        {draft.name.trim() || "…"} · {t(`wizard.channelSummary.${draft.channel}`)} ·{" "}
        {draft.model || "…"} · {memory}
      </p>

      <dl className="divide-y rounded-md border">
        <SummaryRow
          label={t("wizard.review.personality")}
          value={
            persona ? (
              <span className="flex flex-wrap items-center gap-2">
                <span>{persona.name}</span>
                <Badge variant="outline" className="font-mono text-xs font-normal">
                  {t("wizard.review.overrides", {
                    count: Object.keys(draft.personaVars).filter((key) => draft.personaVars[key].trim())
                      .length,
                  })}
                </Badge>
              </span>
            ) : (
              t("wizard.review.nonePersona")
            )
          }
        />
        <SummaryRow
          label={t("wizard.review.channel")}
          value={t(`wizard.channel.${draft.channel}`)}
        />
        <SummaryRow
          label={t("wizard.review.model")}
          value={
            <span className="space-x-2">
              <span className="font-mono break-all">{draft.model || "—"}</span>
              {provider ? (
                <span className="text-xs text-muted-foreground">
                  {t("wizard.review.viaProvider", { name: provider.name })}
                </span>
              ) : null}
            </span>
          }
        />
        <SummaryRow
          label={t("wizard.review.template")}
          value={
            template ? (
              <span className="font-mono text-xs">{template.name}</span>
            ) : (
              t("wizard.review.noneGeneric")
            )
          }
        />
        <SummaryRow
          label={t("wizard.review.skills")}
          value={
            draft.skills.length ? (
              <span className="flex flex-wrap gap-1">
                {draft.skills.map((skill) => (
                  <Badge
                    key={skill.name}
                    variant="outline"
                    className="gap-1 font-mono text-xs font-normal"
                  >
                    {skill.name}
                    <span className="text-muted-foreground">
                      {skill.version ?? t("wizard.review.latest")}
                    </span>
                  </Badge>
                ))}
              </span>
            ) : (
              t("wizard.review.noneGeneric")
            )
          }
        />
        <SummaryRow
          label={t("wizard.review.shares")}
          value={
            draft.shares.length ? (
              <span className="flex flex-wrap gap-1">
                {draft.shares.map((share) => (
                  <Badge
                    key={share.name}
                    variant="outline"
                    className="gap-1 font-mono text-xs font-normal"
                  >
                    {share.name}
                    <span className="text-muted-foreground">{share.mode}</span>
                  </Badge>
                ))}
              </span>
            ) : (
              t("wizard.review.noneGeneric")
            )
          }
        />
        {mcpPreset ? (
          <SummaryRow
            label={t("wizard.review.mcp")}
            value={<span className="font-mono text-xs">{mcpPreset}</span>}
          />
        ) : null}
      </dl>

      {mcpPreset ? (
        <p className="text-xs text-muted-foreground">{t("wizard.review.mcpHint")}</p>
      ) : null}

      <details
        open={advanced}
        onToggle={(event) => onAdvancedChange(event.currentTarget.open)}
        data-advanced-options
        className="rounded-md border"
      >
        <summary className="cursor-pointer px-4 py-3 text-sm font-medium select-none">
          {t("wizard.review.advanced")}
        </summary>
        <div className="space-y-6 border-t p-4">
          <p className="text-xs text-muted-foreground">{t("wizard.review.advancedHint")}</p>

          <div className="space-y-2">
            <Label htmlFor="wizard-mem-limit">{t("wizard.review.memory")}</Label>
            <Input
              id="wizard-mem-limit"
              type="number"
              min={128}
              max={8192}
              step={128}
              value={draft.memLimit}
              className="w-40 font-mono"
              aria-invalid={showErrors && !!errors.memLimit}
              aria-describedby={
                showErrors && errors.memLimit ? "wizard-mem-limit-error" : "wizard-mem-limit-help"
              }
              onChange={(event) => patch({ memLimit: event.target.value })}
            />
            <p id="wizard-mem-limit-help" className="text-xs text-muted-foreground">
              {t("wizard.review.memoryHint")}
            </p>
            <FieldError
              id="wizard-mem-limit-error"
              message={showErrors ? errors.memLimit : undefined}
            />
          </div>

          <div className="space-y-3 rounded-md border p-4">
            <p className="text-sm font-medium">{t("wizard.review.network")}</p>
            <div className="flex items-start gap-3">
              <Checkbox
                id="wizard-allow-lan"
                className="mt-0.5"
                checked={draft.allowLan}
                aria-describedby="wizard-allow-lan-help"
                onCheckedChange={(value) => patch({ allowLan: value === true })}
              />
              <div className="min-w-0 space-y-1">
                <Label htmlFor="wizard-allow-lan" className="cursor-pointer">
                  {t("create.allowLan")}
                </Label>
                <p id="wizard-allow-lan-help" className="text-xs text-muted-foreground">
                  {t("create.allowLanHint")}
                </p>
              </div>
            </div>
            {egressError ? (
              <div data-egress-alert>
                <ErrorAlert error={egressError} />
              </div>
            ) : null}
          </div>

          {!draft.provider ? (
            <div className="space-y-2">
              <Label htmlFor="wizard-provider-token">
                {t("wizard.review.gateToken")}{" "}
                <span className="font-normal text-muted-foreground">({t("common.optional")})</span>
              </Label>
              <Input
                id="wizard-provider-token"
                type="password"
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
                value={draft.providerToken}
                aria-describedby="wizard-provider-token-help"
                onChange={(event) => patch({ providerToken: event.target.value })}
              />
              <p id="wizard-provider-token-help" className="text-xs text-muted-foreground">
                <RichText text={t("wizard.review.gateTokenHint")} />
              </p>
              {secretsError && !envHasSecrets ? (
                <div data-secrets-alert>
                  <ErrorAlert error={secretsError} />
                </div>
              ) : null}
            </div>
          ) : null}

          <div className="space-y-2" data-env-secrets-alert>
            <EnvironmentRows
              idPrefix="wizard-env"
              rows={draft.env}
              onChange={(env) => patch({ env })}
              errors={envErrors}
              suggestions={envSuggestions}
              hints={template?.env}
              templateName={template?.name}
            />
            {secretsError && envHasSecrets ? <ErrorAlert error={secretsError} /> : null}
          </div>

          <SharePicker
            shares={shares}
            value={draft.shares}
            onChange={(next) => patch({ shares: next })}
            loading={sharesLoading}
            error={sharesError}
            onRetry={onRetryShares}
          />

          <div className="space-y-2" id="wizard-skill" tabIndex={-1}>
            <SkillPicker
              skills={skills}
              value={draft.skills}
              onChange={(next) => patch({ skills: next })}
              loading={skillsLoading}
              error={skillsError}
              onRetry={onRetrySkills}
              idPrefix="wizard-skill"
            />
            <FieldError id="wizard-skill-error" message={showErrors ? errors.skills : undefined} />
          </div>

          <div className="space-y-2">
            <Label htmlFor="wizard-advanced-notes">
              {t("wizard.identity.notes")}{" "}
              <span className="font-normal text-muted-foreground">({t("common.optional")})</span>
            </Label>
            <Textarea
              id="wizard-advanced-notes"
              rows={3}
              value={draft.notes}
              placeholder={t("wizard.identity.notesPlaceholder")}
              onChange={(event) => patch({ notes: event.target.value })}
            />
          </div>
        </div>
      </details>
    </div>
  );
}
