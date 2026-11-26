import { useState, type FormEvent } from "react";
import { Loader2, RotateCw, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
import type {
  ChannelType,
  EnvSuggestion,
  InstanceDetail,
  PatchInstanceInput,
  ProviderSummary,
  ShareRef,
  SkillRef,
} from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { ErrorAlert } from "@/components/error-alert";
import { InstanceEnvironment } from "@/components/instance-env";
import { PersonaSelect } from "@/components/persona-select";
import { PersonaStateBadge } from "@/components/persona-state-badge";
import { PersonaVariableInputs } from "@/components/persona-vars";
import { RichText } from "@/components/rich-text";
import { SharePicker } from "@/components/share-picker";
import { SkillPicker } from "@/components/skill-picker";
import type { EnvRow } from "@/lib/env";
import { useI18n } from "@/lib/i18n";
import { applySucceeded, compactVariables } from "@/lib/personas";
import {
  MAX_SKILLS_PER_INSTANCE,
  skillRefPayload,
  skillRefsFromDetail,
  tooManySkills,
} from "@/lib/skills";
import { useResource } from "@/lib/use-resource";
import { isHttpUrl } from "@/lib/validation";

export interface ConfigDraft {
  name: string;
  model: string;
  base_url: string;
  api_key_env: string;
  gate_token: string;
  mem_limit_mb: string;
  allow_lan: boolean;
  channel: ChannelType;
  token: string;
  persona: string;
  personaVars: Record<string, string>;
  notes: string;
  shares: ShareRef[];
  skills: SkillRef[];
}

const CHANNEL_TYPES: ChannelType[] = ["none", "telegram", "discord"];

export function hasEnv(detail: InstanceDetail, key: string): boolean {
  return detail.env.some((entry) => entry.key === key);
}

/**
 * `PATCH` merges persona values: a value sets the key, `null` deletes it. Send
 * one for every stored key the form cleared, so an emptied override really
 * disappears.
 */
function personaVariablesPatch(
  stored: Record<string, string>,
  next: Record<string, string>,
): Record<string, string | null> {
  const patch: Record<string, string | null> = {};
  const kept = new Map<string, string>();
  for (const [key, value] of Object.entries(next)) {
    if (value.trim()) kept.set(key, value.trim());
  }
  for (const [key, value] of kept) patch[key] = value;
  for (const key of Object.keys(stored)) {
    if (!kept.has(key)) patch[key] = null;
  }
  return patch;
}

export function toConfigDraft(detail: InstanceDetail): ConfigDraft {
  return {
    name: detail.name,
    model: detail.model,
    base_url: detail.base_url,
    api_key_env: detail.api_key_env,
    gate_token: "",
    mem_limit_mb: String(detail.mem_limit_mb),
    allow_lan: detail.allow_lan,
    channel: (CHANNEL_TYPES.includes(detail.channel as ChannelType) ? detail.channel : "none") as ChannelType,
    token: "",
    persona: detail.persona ?? "",
    personaVars: { ...(detail.persona_vars ?? {}) },
    notes: detail.notes,
    shares: detail.shares.map((share) => ({ ...share })),
    skills: skillRefsFromDetail(detail.skills),
  };
}

type Translate = (key: string, params?: Record<string, string | number>) => string;

function validateConfig(
  draft: ConfigDraft,
  originalChannel: string,
  t: Translate,
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!draft.name.trim()) errors.name = t("create.errors.nameRequired");
  if (!draft.model.trim()) errors.model = t("create.errors.modelRequired");
  if (!draft.base_url.trim()) errors.base_url = t("create.errors.urlRequired");
  else if (!isHttpUrl(draft.base_url.trim())) errors.base_url = t("create.errors.urlInvalid");
  if (!draft.api_key_env.trim()) errors.api_key_env = t("create.errors.envRequired");
  else if (!/^[A-Z_][A-Z0-9_]*$/.test(draft.api_key_env.trim()))
    errors.api_key_env = t("create.errors.envFormat");
  const memory = Number(draft.mem_limit_mb);
  if (!Number.isFinite(memory) || memory < 128 || memory > 8192) {
    errors.mem_limit_mb = t("create.errors.memoryRange");
  }
  if (draft.channel !== "none" && draft.channel !== originalChannel && !draft.token.trim()) {
    errors.token = t("detail.tokenRequired");
  }
  if (tooManySkills(draft.skills)) {
    errors.skills = t("create.errors.tooManySkills", { max: MAX_SKILLS_PER_INSTANCE });
  }
  return errors;
}

const CONFIG_FIELD_IDS: Record<string, string> = {
  name: "cfg-name",
  model: "cfg-model",
  base_url: "cfg-base-url",
  api_key_env: "cfg-api-key-env",
  mem_limit_mb: "cfg-mem-limit",
  token: "cfg-token",
};

export function AgentConfigurePanel({
  detail,
  provider,
  templateEnv,
  highlightEnvKey,
  draft,
  onDraftChange,
  envRows,
  onEnvRowsChange,
  onSaved,
  onEnvApplied,
}: {
  detail: InstanceDetail;
  provider: ProviderSummary | null;
  templateEnv: EnvSuggestion[];
  highlightEnvKey?: string | null;
  draft: ConfigDraft;
  onDraftChange: (next: ConfigDraft) => void;
  envRows: EnvRow[];
  onEnvRowsChange: (rows: EnvRow[]) => void;
  onSaved: () => void;
  onEnvApplied: () => void;
}) {
  const { t } = useI18n();
  const sharesResource = useResource(() => api.listShares(), []);
  const personas = useResource(() => api.listPersonas(), []);
  const skillRegistry = useResource(() => api.listSkills(), []);
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [reapplying, setReapplying] = useState(false);
  const [reapplyError, setReapplyError] = useState<string | null>(null);

  const errors = validateConfig(draft, detail.channel, t);
  const showErrors = attempted;
  const providerKeySet = hasEnv(detail, "HERMES_GATE_PROFILE_TOKEN");
  const dirty = JSON.stringify(draft) !== JSON.stringify(toConfigDraft(detail));
  const personaChanged = draft.persona !== (detail.persona ?? "");
  const varsDirty =
    JSON.stringify(compactVariables(draft.personaVars)) !==
    JSON.stringify(compactVariables(detail.persona_vars ?? {}));
  const personaDirty = personaChanged || varsDirty;
  const selectedPersona =
    (personas.data ?? []).find((persona) => persona.id === draft.persona) ?? null;
  const divergedActive =
    !!draft.persona &&
    detail.persona === draft.persona &&
    detail.persona_state === "diverged";
  // Host prerequisites sit on the field that needs them, like egress on the
  // network checkbox: the provider token needs the secrets key.
  const egressError = error?.code === "egress_not_enforced" ? error : null;
  const secretsError = error?.code === "secrets_key_missing" ? error : null;

  function update<K extends keyof ConfigDraft>(key: K, value: ConfigDraft[K]) {
    onDraftChange({ ...draft, [key]: value } as ConfigDraft);
  }

  function selectPersona(id: string) {
    const persona = (personas.data ?? []).find((candidate) => candidate.id === id) ?? null;
    const kept: Record<string, string> = {};
    for (const variable of persona?.variables ?? []) {
      if (draft.personaVars[variable.key] !== undefined) {
        kept[variable.key] = draft.personaVars[variable.key];
      }
    }
    onDraftChange({ ...draft, persona: id, personaVars: kept });
    setReapplyError(null);
  }

  async function reapplyPersona() {
    if (!draft.persona) return;
    setReapplying(true);
    setReapplyError(null);
    try {
      const variables = compactVariables(draft.personaVars);
      // Start from what is on screen: a pending variable edit must not be
      // silently dropped by a reapply.
      if (personaDirty) {
        await api.patchInstance(detail.id, {
          ...(personaChanged ? { persona: draft.persona || null } : {}),
          persona_vars: personaVariablesPatch(detail.persona_vars ?? {}, draft.personaVars),
        });
      }
      const result = await api.applyPersona(draft.persona, {
        instances: [detail.id],
        variables,
      });
      const entry = result.results.find((candidate) => candidate.id === detail.id) ?? result.results[0];
      if (!entry || !applySucceeded(entry.status)) {
        setReapplyError(entry?.error ?? t("detail.personaNotApplied"));
      } else {
        toast.success(t("detail.reapplied"));
      }
      onSaved();
    } catch (cause) {
      setReapplyError(apiErrorMessage(toApiError(cause), t));
    } finally {
      setReapplying(false);
    }
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const keys = Object.keys(errors);
    if (keys.length > 0) {
      setAttempted(true);
      document.getElementById(CONFIG_FIELD_IDS[keys[0]] ?? keys[0])?.focus();
      return;
    }

    setPending(true);
    setError(null);
    try {
      const patch: PatchInstanceInput = {
        name: draft.name.trim(),
        model: draft.model.trim(),
        base_url: draft.base_url.trim(),
        api_key_env: draft.api_key_env.trim(),
        gate_token: draft.gate_token.trim() || undefined,
        mem_limit_mb: Number(draft.mem_limit_mb),
        allow_lan: draft.allow_lan,
        notes: draft.notes,
        ...(personaDirty
          ? {
              ...(personaChanged ? { persona: draft.persona || null } : {}),
              persona_vars: personaVariablesPatch(detail.persona_vars ?? {}, draft.personaVars),
            }
          : {}),
        channel:
          draft.channel === "none"
            ? { type: "none" }
            : draft.token.trim()
              ? { type: draft.channel, token: draft.token.trim() }
              : { type: draft.channel },
        shares: draft.shares,
        skills: skillRefPayload(draft.skills),
      };
      await api.patchInstance(detail.id, patch);
      toast.success(t("detail.saved"));
      setAttempted(false);
      setError(null);
      onDraftChange({ ...draft, gate_token: "" });
      onSaved();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="max-w-2xl space-y-8">
      <form onSubmit={onSubmit} noValidate className="space-y-6">
        {error && !egressError && !secretsError ? <ErrorAlert error={error} /> : null}

        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="cfg-name">{t("common.name")}</Label>
            <Input
              id="cfg-name"
              value={draft.name}
              autoComplete="off"
              aria-invalid={showErrors && !!errors.name}
              aria-describedby={showErrors && errors.name ? "cfg-name-error" : undefined}
              onChange={(event) => update("name", event.target.value)}
            />
            {showErrors && errors.name ? (
              <p id="cfg-name-error" className="text-xs text-destructive">
                {errors.name}
              </p>
            ) : null}
          </div>

          <div className="space-y-2">
            <Label htmlFor="cfg-model">{t("fleet.model")}</Label>
            <Input
              id="cfg-model"
              value={draft.model}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              aria-invalid={showErrors && !!errors.model}
              aria-describedby={showErrors && errors.model ? "cfg-model-error" : undefined}
              onChange={(event) => update("model", event.target.value)}
            />
            {showErrors && errors.model ? (
              <p id="cfg-model-error" className="text-xs text-destructive">
                {errors.model}
              </p>
            ) : null}
          </div>
        </div>

        <div className="space-y-2">
          <Label htmlFor="cfg-base-url">{t("providers.form.baseUrl")}</Label>
          <Input
            id="cfg-base-url"
            value={draft.base_url}
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
            aria-invalid={showErrors && !!errors.base_url}
            aria-describedby={showErrors && errors.base_url ? "cfg-base-url-error" : undefined}
            onChange={(event) => update("base_url", event.target.value)}
          />
          {showErrors && errors.base_url ? (
            <p id="cfg-base-url-error" className="text-xs text-destructive">
              {errors.base_url}
            </p>
          ) : null}
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="cfg-api-key-env">{t("providers.form.apiKeyEnv")}</Label>
            <Input
              id="cfg-api-key-env"
              value={draft.api_key_env}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              aria-invalid={showErrors && !!errors.api_key_env}
              aria-describedby={showErrors && errors.api_key_env ? "cfg-api-key-env-error" : undefined}
              onChange={(event) => update("api_key_env", event.target.value)}
            />
            {showErrors && errors.api_key_env ? (
              <p id="cfg-api-key-env-error" className="text-xs text-destructive">
                {errors.api_key_env}
              </p>
            ) : null}
          </div>

          <div className="space-y-2">
            <Label htmlFor="cfg-mem-limit">{t("create.memoryLimit")}</Label>
            <Input
              id="cfg-mem-limit"
              type="number"
              min={128}
              max={8192}
              step={128}
              value={draft.mem_limit_mb}
              className="font-mono"
              aria-invalid={showErrors && !!errors.mem_limit_mb}
              aria-describedby={showErrors && errors.mem_limit_mb ? "cfg-mem-limit-error" : undefined}
              onChange={(event) => update("mem_limit_mb", event.target.value)}
            />
            {showErrors && errors.mem_limit_mb ? (
              <p id="cfg-mem-limit-error" className="text-xs text-destructive">
                {errors.mem_limit_mb}
              </p>
            ) : null}
          </div>
        </div>

        {provider ? (
          <div className="space-y-1 rounded-md border bg-muted/20 px-3 py-2.5">
            <p className="text-sm">
              <RichText text={t("create.suppliedBy", { name: provider.name })} />
            </p>
            <p className="text-xs text-muted-foreground">{t("detail.providerSuppliedHint")}</p>
          </div>
        ) : (
          <div className="space-y-2">
            <Label htmlFor="cfg-gate-token">{t("clone.providerToken")}</Label>
            <Input
              id="cfg-gate-token"
              type="password"
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              value={draft.gate_token}
              placeholder={
                providerKeySet ? t("detail.keepCurrentToken") : t("create.notSet")
              }
              aria-describedby="cfg-gate-token-help"
              onChange={(event) => update("gate_token", event.target.value)}
            />
            <p id="cfg-gate-token-help" className="text-xs text-muted-foreground">
              <RichText
                text={
                  providerKeySet ? t("detail.keyStored") : t("detail.keyMissing")
                }
              />
            </p>
            {secretsError ? <ErrorAlert error={secretsError} /> : null}
          </div>
        )}

        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="cfg-channel">{t("fleet.channel")}</Label>
            <Select value={draft.channel} onValueChange={(value) => update("channel", value as ChannelType)}>
              <SelectTrigger id="cfg-channel" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {CHANNEL_TYPES.map((type) => (
                  <SelectItem key={type} value={type}>
                    {t(`channel.${type}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {draft.channel !== "none" ? (
            <div className="space-y-2">
              <Label htmlFor="cfg-token">{t("clone.botToken")}</Label>
              <Input
                id="cfg-token"
                type="password"
                autoComplete="off"
                placeholder={t("detail.keepCurrentToken")}
                className="font-mono"
                aria-invalid={showErrors && !!errors.token}
                aria-describedby={
                  showErrors && errors.token ? "cfg-token-error" : "cfg-token-help"
                }
                onChange={(event) => update("token", event.target.value)}
              />
              <p id="cfg-token-help" className="text-xs text-muted-foreground">
                {detail.channel !== "none" && detail.channel === draft.channel
                  ? t("detail.tokenKeepHint")
                  : t("detail.tokenSwitchHint")}
              </p>
              {showErrors && errors.token ? (
                <p id="cfg-token-error" className="text-xs text-destructive">
                  {errors.token}
                </p>
              ) : null}
            </div>
          ) : null}
        </div>

        <fieldset className="min-w-0 space-y-3">
          <legend className="text-sm font-medium">{t("create.network")}</legend>
          <div className="flex items-start gap-3 rounded-md border p-4">
            <Checkbox
              id="cfg-allow-lan"
              className="mt-0.5"
              checked={draft.allow_lan}
              aria-describedby="cfg-allow-lan-help"
              onCheckedChange={(value) => {
                update("allow_lan", value === true);
                if (egressError) setError(null);
              }}
            />
            <div className="min-w-0 space-y-1">
              <Label htmlFor="cfg-allow-lan" className="cursor-pointer">
                {t("create.allowLan")}
              </Label>
              <p id="cfg-allow-lan-help" className="text-xs text-muted-foreground">
                {t("create.allowLanHint")}
              </p>
              <p className="text-xs text-muted-foreground">
                <RichText
                  text={draft.allow_lan ? t("create.lanEnabled") : t("create.lanRestricted")}
                />
              </p>
            </div>
          </div>
          {egressError ? <ErrorAlert error={egressError} /> : null}
        </fieldset>

        <fieldset className="min-w-0 space-y-3">
          <legend className="text-sm font-medium">{t("clone.personaLabel")}</legend>
          <div className="space-y-2">
            <PersonaSelect
              id="cfg-persona"
              value={draft.persona}
              onChange={selectPersona}
              personas={personas.data ?? []}
              loading={personas.loading && !personas.data}
              describedBy="cfg-persona-help"
            />
            <p id="cfg-persona-help" className="text-xs text-muted-foreground">
              <RichText text={t("detail.personaHint")} />
            </p>
            <p className="text-xs text-muted-foreground">
              <a href="#/personas" className="underline underline-offset-4">
                {t("create.managePersonas")}
              </a>
            </p>
            {personas.error ? (
              <p className="text-xs text-destructive">
                {t("clone.personasLoadFailed", {
                  message: apiErrorMessage(personas.error, t),
                })}{" "}
                <button
                  type="button"
                  className="underline underline-offset-4"
                  onClick={personas.reload}
                >
                  {t("common.retry")}
                </button>
              </p>
            ) : null}
          </div>

          {selectedPersona ? (
            <>
              <div className="rounded-md border p-4">
                <p className="text-sm font-medium">
                  {selectedPersona.name}{" "}
                  <PersonaStateBadge state={detail.persona === selectedPersona.id ? detail.persona_state : undefined} />
                </p>
                <p className="mt-1 text-xs text-muted-foreground">
                  {selectedPersona.description}
                </p>
              </div>
              <PersonaVariableInputs
                variables={selectedPersona.variables}
                values={draft.personaVars}
                onChange={(personaVars) => update("personaVars", personaVars)}
                idPrefix="cfg-persona-var"
                disabled={pending || reapplying}
              />
            </>
          ) : (
            <p className="rounded-md border px-3 py-2.5 text-xs text-muted-foreground">
              {t("detail.noPersonaConfig")}
            </p>
          )}

          {divergedActive ? (
            <Alert className="border-amber-600/40 bg-amber-50/40 dark:bg-amber-950/20">
              <TriangleAlert aria-hidden className="text-amber-600 dark:text-amber-400" />
              <AlertTitle>{t("detail.divergedTitle")}</AlertTitle>
              <AlertDescription>
                <p className="text-sm text-muted-foreground">
                  <RichText text={t("detail.divergedBody")} />
                </p>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  className="mt-2"
                  onClick={() => void reapplyPersona()}
                  disabled={pending || reapplying}
                >
                  {reapplying ? (
                    <Loader2 aria-hidden className="animate-spin" />
                  ) : (
                    <RotateCw aria-hidden />
                  )}
                  {t("detail.reapplyPersona")}
                </Button>
                {reapplyError ? (
                  <p className="mt-2 text-xs text-destructive">{reapplyError}</p>
                ) : null}
              </AlertDescription>
            </Alert>
          ) : null}
        </fieldset>

        <fieldset className="min-w-0 space-y-3">
          <legend className="text-sm font-medium">{t("nav.shares")}</legend>
          <SharePicker
            shares={sharesResource.data ?? []}
            value={draft.shares}
            onChange={(next) => update("shares", next)}
            loading={sharesResource.loading && !sharesResource.data}
            error={sharesResource.error}
            onRetry={sharesResource.reload}
          />
        </fieldset>

        <fieldset className="min-w-0 space-y-3">
          <legend className="text-sm font-medium">{t("nav.skills")}</legend>
          <SkillPicker
            skills={skillRegistry.data ?? []}
            value={draft.skills}
            onChange={(next) => update("skills", next)}
            loading={skillRegistry.loading && !skillRegistry.data}
            error={skillRegistry.error}
            onRetry={skillRegistry.reload}
            disabled={pending}
            idPrefix="cfg-skill"
            effective={Object.fromEntries(
              (detail.skills ?? []).map((skill) => [skill.name, skill.effective_version]),
            )}
          />
          {showErrors && errors.skills ? (
            <p className="text-xs text-destructive">{errors.skills}</p>
          ) : null}
        </fieldset>

        <div className="space-y-2">
          <Label htmlFor="cfg-notes">{t("create.notes")}</Label>
          <Textarea id="cfg-notes" rows={3} value={draft.notes} onChange={(event) => update("notes", event.target.value)} />
        </div>

        <div className="flex flex-wrap items-center gap-2 border-t pt-4">
          <Button type="submit" disabled={!dirty || pending}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
            {t("common.save")}
          </Button>
          <Button
            type="button"
            variant="outline"
            disabled={!dirty || pending}
            onClick={() => {
              onDraftChange(toConfigDraft(detail));
              setAttempted(false);
            }}
          >
            {t("env.discard")}
          </Button>
          <p className="text-xs text-muted-foreground sm:ml-auto">
            <RichText text={t("detail.saveHint")} />
          </p>
        </div>
      </form>

      <div id="instance-env" className="scroll-mt-24">
        <InstanceEnvironment
          detail={detail}
          rows={envRows}
          suggestions={templateEnv}
          highlightKey={highlightEnvKey}
          onRowsChange={onEnvRowsChange}
          onApplied={onEnvApplied}
        />
      </div>
    </div>
  );
}
