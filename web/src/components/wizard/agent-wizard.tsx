import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { Loader2, Plus } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
import type {
  AgentLanguage,
  AgentDraft,
} from "@/lib/agent-draft";
import {
  WIZARD_FIELD_IDS,
  WIZARD_STEP_COUNT,
  clearAgentDraft,
  defaultAgentDraft,
  draftHasContent,
  loadAgentDraft,
  saveAgentDraft,
  templateChannel,
  usageDefaults,
  usageForTemplate,
  validateWizardDraft,
  validateWizardStep,
} from "@/lib/agent-draft";
import type { PersonaSummary, ProviderSummary, Template } from "@/api/types";
import { Button } from "@/components/ui/button";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { StepPurpose } from "@/components/wizard/step-purpose";
import { StepIdentity } from "@/components/wizard/step-identity";
import { StepConnection } from "@/components/wizard/step-connection";
import { StepReview } from "@/components/wizard/step-review";
import { WizardProgress } from "@/components/wizard/wizard-progress";
import { envPayload, validateEnvRows } from "@/lib/env";
import { compactVariables } from "@/lib/personas";
import { skillRefPayload } from "@/lib/skills";
import { useI18n } from "@/lib/i18n";
import { hrefFor, navigate, useRoute } from "@/lib/router";
import { useResource } from "@/lib/use-resource";

function providerPatch(
  current: AgentDraft,
  id: string,
  list: ProviderSummary[],
): AgentDraft {
  const chosen = list.find((provider) => provider.id === id) ?? null;
  return {
    ...current,
    provider: id,
    providerTouched: true,
    // The provider supplies the secret: drop any per-instance token typed
    // before. Switching back to none keeps the fields and restores the token.
    providerToken: id ? "" : current.providerToken,
    baseUrl: chosen ? chosen.base_url : current.baseUrl,
    apiKeyEnv: chosen ? chosen.api_key_env : current.apiKeyEnv,
  };
}

function templatePatch(
  current: AgentDraft,
  id: string,
  list: Template[],
): AgentDraft {
  const previous = list.find((template) => template.id === current.template);
  const next = list.find((template) => template.id === id);
  const keepModel = current.model.trim() !== "" && current.model !== previous?.default_model;
  const nextChannel = templateChannel(next?.channel);
  return {
    ...current,
    template: id,
    model: keepModel ? current.model : (next?.default_model ?? ""),
    channel: !current.channelTouched && nextChannel ? nextChannel : current.channel,
  };
}

/**
 * Guided four-step creation (#72). Every answer lives in one persisted draft,
 * so the expert form at `#/instances/new?form=expert` can pick it up, and a
 * reload never starts from zero.
 */
export function AgentWizard() {
  const { t, lang } = useI18n();
  const route = useRoute();
  const templates = useResource(() => api.listTemplates(), []);
  const shares = useResource(() => api.listShares(), []);
  const providers = useResource(() => api.listProviders(), []);
  const personas = useResource(() => api.listPersonas(), []);
  const skillRegistry = useResource(() => api.listSkills(), []);

  const [initialDraft] = useState<AgentDraft | null>(() => loadAgentDraft());
  const [draft, setDraft] = useState<AgentDraft>(() =>
    initialDraft ?? { ...defaultAgentDraft(), language: lang },
  );
  const [resumePending, setResumePending] = useState(initialDraft !== null);
  const [attempted, setAttempted] = useState<number | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<ApiError | null>(null);
  const [serverIdError, setServerIdError] = useState<string | null>(null);
  const [advanced, setAdvanced] = useState(false);
  const [saved, setSaved] = useState(false);
  const [presetNonce, setPresetNonce] = useState(0);

  const step = draft.step;
  const hydrated = !resumePending;

  const draftRef = useRef(draft);
  draftRef.current = draft;
  const hydratedRef = useRef(hydrated);
  hydratedRef.current = hydrated;
  const completedRef = useRef(false);

  const patch = useCallback((changes: Partial<AgentDraft>) => {
    setDraft((current) => ({ ...current, ...changes }));
    setResumePending(false);
    setSubmitError(null);
    setServerIdError(null);
  }, []);

  const patchWith = useCallback((updater: (current: AgentDraft) => AgentDraft) => {
    setDraft(updater);
    setResumePending(false);
    setSubmitError(null);
    setServerIdError(null);
  }, []);

  // Presets and reference pruning run without counting as a user edit: they
  // must not dismiss the "resume?" prompt before the user answered it.
  const patchSilent = useCallback((updater: (current: AgentDraft) => AgentDraft) => {
    setDraft(updater);
  }, []);

  // Debounced autosave (400 ms). The unmount effect flushes the last edit so a
  // hop to the expert form never loses the answer being typed.
  useEffect(() => {
    if (!hydrated || completedRef.current) return;
    const timer = window.setTimeout(() => {
      const current = draftRef.current;
      if (draftHasContent(current)) {
        saveAgentDraft(current);
        setSaved(true);
      } else {
        clearAgentDraft();
        setSaved(false);
      }
    }, 400);
    return () => window.clearTimeout(timer);
  }, [draft, hydrated]);

  useEffect(
    () => () => {
      if (hydratedRef.current && !completedRef.current) {
        if (draftHasContent(draftRef.current)) saveAgentDraft(draftRef.current);
        else clearAgentDraft();
      }
    },
    [],
  );

  // Focus follows the step heading; the progress line is announced politely.
  const firstRender = useRef(true);
  useEffect(() => {
    if (firstRender.current) {
      firstRender.current = false;
      return;
    }
    document.getElementById("wizard-step-heading")?.focus();
  }, [step]);

  const templateList = templates.data ?? [];
  const selectedTemplate = templateList.find((template) => template.id === draft.template) ?? null;
  const templateEnv = selectedTemplate?.env ?? [];
  const errors = validateWizardStep(draft, step, templateEnv, t);
  const showErrors = attempted === step;

  const personaList = personas.data ?? [];
  const selectedPersona = personaList.find((persona) => persona.id === draft.persona) ?? null;
  const providerList = providers.data ?? [];
  const selectedProvider = providerList.find((provider) => provider.id === draft.provider) ?? null;
  const shareList = shares.data ?? [];
  const skillList = skillRegistry.data ?? [];
  const envSuggestions = (selectedTemplate?.env ?? []).filter(
    (suggestion) => !draft.env.some((row) => row.key.trim() === suggestion.key),
  );
  const envErrors =
    showErrors && step === 3
      ? validateEnvRows(draft.env, "create", selectedTemplate?.env ?? [], t).rows
      : {};

  const egressError = submitError?.code === "egress_not_enforced" ? submitError : null;
  const secretsError = submitError?.code === "secrets_key_missing" ? submitError : null;
  const secretEnv = draft.env.some((row) => row.secret);
  const inlineEgress = egressError !== null && step === 3 && advanced;
  const inlineSecrets =
    secretsError !== null && step === 3 && advanced && (!draft.provider || secretEnv);

  const mcpPreset = route.query.get("mcp")?.trim() || null;

  function languageValue(language: AgentLanguage): string {
    return t(language === "fr" ? "wizard.language.valueFr" : "wizard.language.valueEn");
  }

  function personaPatch(
    current: AgentDraft,
    id: string,
    list: PersonaSummary[],
  ): AgentDraft {
    const persona = list.find((candidate) => candidate.id === id) ?? null;
    // Keep only the values that still exist on the new persona.
    const kept: Record<string, string> = {};
    for (const variable of persona?.variables ?? []) {
      if (current.personaVars[variable.key] !== undefined) {
        kept[variable.key] = current.personaVars[variable.key];
      }
    }
    if (
      persona?.variables.some((variable) => variable.key === "language") &&
      !kept.language
    ) {
      kept.language = languageValue(current.language);
    }
    return { ...current, persona: id, personaVars: kept };
  }

  // ── Query presets (?template, ?usage, ?persona, ?skill) ────────────────
  const appliedPresets = useRef({
    usage: false,
    template: false,
    persona: false,
    skill: false,
  });
  const usageParam = route.query.get("usage");

  useEffect(() => {
    if (appliedPresets.current.usage || !templates.data) return;
    appliedPresets.current.usage = true;
    if (usageParam !== "discord" && usageParam !== "watch" && usageParam !== "dev") return;
    const defaults = usageDefaults(usageParam, templates.data);
    patchSilent((current) => {
      const previous = templates.data?.find((template) => template.id === current.template);
      const keepModel =
        current.model.trim() !== "" && current.model !== previous?.default_model;
      return {
        ...current,
        usage: usageParam,
        template: defaults.template?.id ?? "",
        model: keepModel ? current.model : (defaults.template?.default_model ?? ""),
        channel: defaults.channel,
        channelTouched: true,
      };
    });
  }, [templates.data, usageParam, presetNonce, patchSilent]);

  useEffect(() => {
    if (appliedPresets.current.template || !templates.data) return;
    appliedPresets.current.template = true;
    if (usageParam === "discord" || usageParam === "watch" || usageParam === "dev") return;
    const id = route.query.get("template");
    if (!id) return;
    const template = templates.data.find((candidate) => candidate.id === id);
    if (!template) return;
    patchSilent((current) => {
      const previous = templates.data?.find((candidate) => candidate.id === current.template);
      const keepModel =
        current.model.trim() !== "" && current.model !== previous?.default_model;
      return {
        ...current,
        template: template.id,
        model: keepModel ? current.model : template.default_model,
        channel: current.channelTouched
          ? current.channel
          : (templateChannel(template.channel) ?? current.channel),
        usage: current.usage || usageForTemplate(template),
      };
    });
  }, [templates.data, usageParam, route.query, presetNonce, patchSilent]);

  useEffect(() => {
    if (appliedPresets.current.persona || !personas.data) return;
    appliedPresets.current.persona = true;
    const id = route.query.get("persona");
    if (!id) return;
    if (!personas.data.some((persona) => persona.id === id)) return;
    patchSilent((current) => personaPatch(current, id, personas.data ?? []));
  }, [personas.data, route.query, presetNonce, patchSilent]);

  useEffect(() => {
    if (appliedPresets.current.skill || !skillRegistry.data) return;
    appliedPresets.current.skill = true;
    const requested = route.query
      .getAll("skill")
      .flatMap((value) => value.split(","))
      .map((value) => value.trim())
      .filter(Boolean);
    if (requested.length === 0) return;
    const known = new Set(skillRegistry.data.map((skill) => skill.name));
    const refs = requested
      .filter((name) => known.has(name))
      .map((name) => ({ name, version: null }));
    if (refs.length === 0) return;
    patchSilent((current) => ({
      ...current,
      skills: [
        ...current.skills.filter((skill) => !refs.some((ref) => ref.name === skill.name)),
        ...refs,
      ],
    }));
  }, [skillRegistry.data, route.query, presetNonce, patchSilent]);

  // One provider is the common case: preselect it instead of asking a question
  // with a single answer.
  useEffect(() => {
    if (!providers.data) return;
    if (draft.providerTouched || draft.provider) return;
    if (providers.data.length !== 1) return;
    patchSilent((current) => providerPatch(current, providers.data?.[0]?.id ?? "", providers.data ?? []));
  }, [providers.data, draft.provider, draft.providerTouched, patchSilent]);

  // Drop references that vanished on the host since the draft was written.
  useEffect(() => {
    if (!templates.data || !draft.template) return;
    if (templates.data.some((template) => template.id === draft.template)) return;
    patchSilent((current) => ({ ...current, template: "", model: "" }));
  }, [templates.data, draft.template, patchSilent]);

  useEffect(() => {
    if (!providers.data || !draft.provider) return;
    if (providers.data.some((provider) => provider.id === draft.provider)) return;
    patchSilent((current) => ({ ...current, provider: "", providerTouched: true }));
  }, [providers.data, draft.provider, patchSilent]);

  useEffect(() => {
    if (!personas.data || !draft.persona) return;
    if (personas.data.some((persona) => persona.id === draft.persona)) return;
    patchSilent((current) => ({ ...current, persona: "", personaVars: {} }));
  }, [personas.data, draft.persona, patchSilent]);

  // A card clicked before the template list arrived left no template behind;
  // resolve it as soon as the list is available.
  useEffect(() => {
    if (!templates.data || !draft.usage || draft.usage === "other" || draft.template) return;
    const defaults = usageDefaults(draft.usage, templates.data);
    if (!defaults.template) return;
    patchSilent((current) => ({
      ...current,
      template: defaults.template?.id ?? "",
      model: current.model || (defaults.template?.default_model ?? ""),
      channel: defaults.channel,
      channelTouched: true,
    }));
  }, [templates.data, draft.usage, draft.template, patchSilent]);

  // A draft written by the expert form (or a bare ?template link) has no usage
  // card yet; pick the one that matches its template.
  useEffect(() => {
    if (!templates.data || draft.usage || !draft.template) return;
    const template = templates.data.find((candidate) => candidate.id === draft.template);
    if (!template) return;
    patchSilent((current) => ({ ...current, usage: usageForTemplate(template) }));
  }, [templates.data, draft.usage, draft.template, patchSilent]);

  // ── User handlers ──────────────────────────────────────────────────────
  function selectTemplate(id: string) {
    const list = templates.data ?? [];
    patchWith((current) => templatePatch(current, id, list));
  }

  function selectPersona(id: string) {
    const list = personas.data ?? [];
    patchWith((current) => personaPatch(current, id, list));
  }

  function selectProvider(id: string) {
    const list = providers.data ?? [];
    patchWith((current) => providerPatch(current, id, list));
  }

  function selectLanguage(language: AgentLanguage) {
    const value = languageValue(language);
    patchWith((current) => {
      const persona = (personas.data ?? []).find(
        (candidate) => candidate.id === current.persona,
      );
      const hasLanguage = persona?.variables.some((variable) => variable.key === "language") ?? false;
      return {
        ...current,
        language,
        personaVars: hasLanguage
          ? { ...current.personaVars, language: value }
          : current.personaVars,
      };
    });
  }

  function handleProviderSaved(provider: ProviderSummary) {
    providers.setData((previous) => {
      const list = previous ?? [];
      return list.some((candidate) => candidate.id === provider.id)
        ? list.map((candidate) => (candidate.id === provider.id ? provider : candidate))
        : [...list, provider];
    });
    patchWith((current) => providerPatch(current, provider.id, [provider]));
  }

  function resumeDraft() {
    setResumePending(false);
  }

  function restartDraft() {
    clearAgentDraft();
    // A link that opened this page (?template, ?usage, ?persona, ?skill) still
    // describes the agent being started: replay it on the fresh draft.
    appliedPresets.current = {
      usage: false,
      template: false,
      persona: false,
      skill: false,
    };
    setPresetNonce((nonce) => nonce + 1);
    setDraft({ ...defaultAgentDraft(), language: lang });
    setResumePending(false);
    setAttempted(null);
    setSubmitError(null);
    setServerIdError(null);
    setAdvanced(false);
    setSaved(false);
  }

  // ── Validation and navigation ─────────────────────────────────────────
  function targetFor(stepIndex: number, key: string): string {
    if (stepIndex === 0 && key === "template") {
      return draft.usage === "other" ? "wizard-template" : "wizard-purpose";
    }
    if (key === "id") return "wizard-name";
    if (key.startsWith("env-")) {
      const [, rowId, field] = key.split("-");
      return `wizard-env-${rowId}-${field}`;
    }
    return WIZARD_FIELD_IDS[key] ?? key;
  }

  function focusWhenReady(id: string, attempts = 10): void {
    window.requestAnimationFrame(() => {
      const node = document.getElementById(id);
      if (node) {
        node.focus();
        return;
      }
      if (attempts > 0) focusWhenReady(id, attempts - 1);
    });
  }

  function goNext() {
    const stepErrors = validateWizardStep(draft, step, templateEnv, t);
    const keys = Object.keys(stepErrors);
    if (keys.length > 0) {
      setAttempted(step);
      focusWhenReady(targetFor(step, keys[0]));
      return;
    }
    setAttempted(null);
    patch({ step: Math.min(step + 1, WIZARD_STEP_COUNT - 1) });
  }

  function goBack() {
    setAttempted(null);
    patch({ step: Math.max(step - 1, 0) });
  }

  async function create() {
    const stepErrors = validateWizardDraft(draft, templateEnv, t);
    for (let index = 0; index < WIZARD_STEP_COUNT; index += 1) {
      const keys = Object.keys(stepErrors[index]);
      if (keys.length === 0) continue;
      patch({ step: index });
      setAttempted(index);
      if (index === 3) setAdvanced(true);
      focusWhenReady(targetFor(index, keys[0]));
      return;
    }

    setSubmitting(true);
    setSubmitError(null);
    setServerIdError(null);
    try {
      const detail = await api.createInstance({
        id: draft.id.trim(),
        name: draft.name.trim(),
        template: draft.template,
        model: draft.model.trim(),
        base_url: draft.baseUrl.trim(),
        api_key_env: draft.apiKeyEnv.trim(),
        provider: draft.provider || undefined,
        persona: draft.persona || null,
        persona_vars: draft.persona ? compactVariables(draft.personaVars) : undefined,
        gate_token: draft.provider ? undefined : draft.providerToken.trim() || undefined,
        channel:
          draft.channel === "none"
            ? { type: "none" }
            : { type: draft.channel, token: draft.token },
        shares: draft.shares,
        skills: skillRefPayload(draft.skills),
        mem_limit_mb: Number(draft.memLimit),
        allow_lan: draft.allowLan,
        notes: draft.notes.trim(),
        env: envPayload(draft.env, "create"),
      });

      if (mcpPreset) {
        try {
          await api.attachInstanceMcpServer(detail.id, mcpPreset);
        } catch (cause) {
          toast.warning(
            t("wizard.mcpAttachFailed", {
              server: mcpPreset,
              message: apiErrorMessage(cause, t),
            }),
          );
        }
      }

      completedRef.current = true;
      clearAgentDraft();
      toast.success(t("wizard.created", { id: detail.id }));
      navigate(`/instances/${encodeURIComponent(detail.id)}?created=1`);
    } catch (cause) {
      const error = toApiError(cause);
      if (error.status === 409 && error.code === "instance_exists") {
        patch({ step: 1 });
        setAttempted(1);
        setServerIdError(apiErrorMessage(error, t));
        focusWhenReady("wizard-name");
      } else if (error.code === "missing_persona_variables") {
        patch({ step: 1 });
        setAttempted(null);
        setSubmitError(error);
        const firstVariable = selectedPersona?.variables[0]?.key;
        focusWhenReady(
          firstVariable ? `wizard-persona-var-${firstVariable}` : "wizard-persona",
        );
        window.requestAnimationFrame(() =>
          document.querySelector("[data-persona-block]")?.scrollIntoView({ block: "center" }),
        );
      } else if (
        error.code === "egress_not_enforced" ||
        error.code === "secrets_key_missing"
      ) {
        patch({ step: 3 });
        setAdvanced(true);
        setAttempted(null);
        setSubmitError(error);
        const egress = error.code === "egress_not_enforced";
        const field = egress
          ? "wizard-allow-lan"
          : secretEnv || draft.provider
            ? null
            : "wizard-provider-token";
        const marker = egress
          ? "[data-egress-alert]"
          : secretEnv
            ? "[data-env-secrets-alert]"
            : "[data-secrets-alert]";
        window.requestAnimationFrame(() => {
          if (field) document.getElementById(field)?.focus({ preventScroll: true });
          document.querySelector(marker)?.scrollIntoView({ block: "center" });
        });
      } else {
        setSubmitError(error);
      }
    } finally {
      setSubmitting(false);
    }
  }

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (submitting) return;
    if (step === WIZARD_STEP_COUNT - 1) void create();
    else goNext();
  }

  const resumeName = draft.name.trim() || draft.id || t("wizard.resume.unnamed");

  return (
    <>
      <PageHeader
        title={t("wizard.title")}
        description={t("wizard.description")}
        actions={
          <a
            href={hrefFor("/instances/new?form=expert")}
            className="inline-flex min-h-11 items-center text-sm text-muted-foreground underline underline-offset-4 hover:text-foreground sm:min-h-0"
          >
            {t("wizard.advancedConfig")}
          </a>
        }
      />

      <div className="mx-auto w-full max-w-2xl space-y-6">
        {resumePending ? (
          <div
            role="region"
            aria-label={t("wizard.resume.aria")}
            className="flex flex-col gap-3 rounded-md border border-primary/40 bg-accent/30 p-4 sm:flex-row sm:items-center sm:justify-between"
          >
            <div className="min-w-0 space-y-1">
              <p className="text-sm">{t("wizard.resume.title", { name: resumeName })}</p>
              <p className="text-xs text-muted-foreground">{t("wizard.resume.secrets")}</p>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button
                type="button"
                variant="outline"
                className="h-11 sm:h-9"
                onClick={restartDraft}
              >
                {t("wizard.resume.restart")}
              </Button>
              <Button type="button" className="h-11 sm:h-9" onClick={resumeDraft}>
                {t("wizard.resume.resume")}
              </Button>
            </div>
          </div>
        ) : null}

        <WizardProgress step={step} name={t(`wizard.step.${stepName(step)}.short`)} saved={saved} />

        {submitError && !inlineEgress && !inlineSecrets ? (
          <ErrorAlert error={submitError} />
        ) : null}

        <form onSubmit={handleSubmit} noValidate className="space-y-6">
          {step === 0 ? (
            <StepPurpose
              draft={draft}
              patch={patch}
              patchWith={patchWith}
              errors={errors}
              showErrors={showErrors}
              templates={templateList}
              loading={templates.loading && !templates.data}
              error={templates.error}
              onRetry={templates.reload}
              selectedTemplate={selectedTemplate}
              onSelectTemplate={selectTemplate}
            />
          ) : null}

          {step === 1 ? (
            <StepIdentity
              draft={draft}
              patch={patch}
              patchWith={patchWith}
              errors={errors}
              showErrors={showErrors}
              personas={personaList}
              personasLoading={personas.loading && !personas.data}
              personasError={personas.error}
              onRetryPersonas={personas.reload}
              selectedPersona={selectedPersona}
              onSelectPersona={selectPersona}
              onSelectLanguage={selectLanguage}
              serverIdError={serverIdError}
            />
          ) : null}

          {step === 2 ? (
            <StepConnection
              draft={draft}
              patch={patch}
              patchWith={patchWith}
              errors={errors}
              showErrors={showErrors}
              providers={providerList}
              providersLoading={providers.loading && !providers.data}
              providersError={providers.error}
              onRetryProviders={providers.reload}
              selectedProvider={selectedProvider}
              onSelectProvider={selectProvider}
              onProviderSaved={handleProviderSaved}
            />
          ) : null}

          {step === 3 ? (
            <StepReview
              draft={draft}
              patch={patch}
              patchWith={patchWith}
              errors={errors}
              showErrors={showErrors}
              template={selectedTemplate}
              persona={selectedPersona}
              provider={selectedProvider}
              shares={shareList}
              sharesLoading={shares.loading && !shares.data}
              sharesError={shares.error}
              onRetryShares={shares.reload}
              skills={skillList}
              skillsLoading={skillRegistry.loading && !skillRegistry.data}
              skillsError={skillRegistry.error}
              onRetrySkills={skillRegistry.reload}
              envSuggestions={envSuggestions}
              envErrors={envErrors}
              advanced={advanced}
              onAdvancedChange={setAdvanced}
              egressError={egressError}
              secretsError={secretsError}
              mcpPreset={mcpPreset}
            />
          ) : null}

          <div className="flex flex-wrap items-center justify-between gap-3 border-t pt-4">
            <Button
              type="button"
              variant="outline"
              className="h-11 sm:h-9"
              onClick={goBack}
              disabled={step === 0 || submitting}
            >
              {t("common.back")}
            </Button>
            <Button type="submit" className="h-11 sm:h-9" disabled={submitting}>
              {submitting ? (
                <Loader2 aria-hidden className="animate-spin" />
              ) : step === WIZARD_STEP_COUNT - 1 ? (
                <Plus aria-hidden />
              ) : null}
              {step === WIZARD_STEP_COUNT - 1 ? t("wizard.submit") : t("create.continue")}
            </Button>
          </div>
        </form>
      </div>
    </>
  );
}

/** Short label for the progress line; kept out of the component body for stable keys. */
function stepName(step: number): "purpose" | "identity" | "connection" | "review" {
  switch (step) {
    case 1:
      return "identity";
    case 2:
      return "connection";
    case 3:
      return "review";
    default:
      return "purpose";
  }
}
