import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import {
  AlertCircle,
  CopyPlus,
  Loader2,
  MoreHorizontal,
  Play,
  RefreshCw,
  RotateCw,
  Square,
  Trash2,
  TriangleAlert,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
import type { ChannelType, EnvEntry, EnvSuggestion, InstanceDetail, PatchInstanceInput, PersonaSummary, ProviderSummary, ShareRef, SkillRef } from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { CodeBlock } from "@/components/code-block";
import { CopyButton } from "@/components/copy-button";
import { DataList, Section } from "@/components/data-list";
import { DeleteInstanceDialog } from "@/components/delete-instance-dialog";
import { DuplicateInstanceDialog } from "@/components/duplicate-instance-dialog";
import { InstanceMemoryTab, InstanceMessagesTab } from "@/components/instance-bus";
import { InstanceEnvironment } from "@/components/instance-env";
import { InstanceLogs } from "@/components/instance-logs";
import { InstanceFiles } from "@/components/instance-files";
import { InstanceHistory } from "@/components/instance-history";
import {
  InstanceAccess,
  InstanceEgress,
  InstanceIncidents,
  InstanceSnapshots,
  InstanceTerminal,
} from "@/components/instance-ops";
import { InstanceSchedules } from "@/components/schedules";
import { ErrorAlert } from "@/components/error-alert";
import { RichText } from "@/components/rich-text";
import { PageHeader } from "@/components/page-header";
import { PersonaStateBadge } from "@/components/persona-state-badge";
import { PersonaSelect } from "@/components/persona-select";
import { PersonaVariableInputs } from "@/components/persona-vars";
import { SharePicker } from "@/components/share-picker";
import { SkillPicker } from "@/components/skill-picker";
import { StatusBadge } from "@/components/status-badge";
import {
  ASSISTANT_APPLIED_EVENT,
  instanceTabSchema,
  useAssistantScope,
  type AssistantAppliedDetail,
} from "@/lib/assistant";
import { envRowsFromDetail, envRowsMatch, type EnvRow } from "@/lib/env";
import { INSTANCES_CHANGED_EVENT } from "@/lib/events";
import { formatDateTime, formatRelative, formatUptime } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { applySucceeded, compactVariables } from "@/lib/personas";
import {
  MAX_SKILLS_PER_INSTANCE,
  skillRefPayload,
  skillRefsFromDetail,
  tooManySkills,
} from "@/lib/skills";
import { navigate, hrefFor, useRoute } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { isHttpUrl } from "@/lib/validation";
import { cn } from "@/lib/utils";

function Mono({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={cn("font-mono text-xs break-all", className)}>{children}</span>;
}

function CopyValue({ value, label }: { value: string; label: string }) {
  const { t } = useI18n();
  return (
    <span className="flex min-w-0 items-center gap-1">
      <span className="min-w-0 flex-1 font-mono text-xs break-all">{value}</span>
      <CopyButton
        value={value}
        label={t("providers.detail.copyLabel", { label })}
        size="icon-xs"
        className="-my-1 shrink-0"
      />
    </span>
  );
}

function tokenEnv(detail: InstanceDetail): string | null {
  const env =
    detail.channel === "telegram"
      ? "TELEGRAM_BOT_TOKEN"
      : detail.channel === "discord"
        ? "DISCORD_BOT_TOKEN"
        : null;
  if (!env) return null;
  return detail.env.some((entry) => entry.key === env) ? env : null;
}

function hasEnv(detail: InstanceDetail, key: string): boolean {
  return detail.env.some((entry) => entry.key === key);
}

function OverviewTab({
  detail,
  provider,
  persona,
  canEdit,
  onConfigure,
}: {
  detail: InstanceDetail;
  provider: ProviderSummary | null;
  persona: PersonaSummary | null;
  canEdit: boolean;
  onConfigure: () => void;
}) {
  const { t, lang } = useI18n();
  const env = tokenEnv(detail);
  const suppliedByProvider = !!detail.provider;
  const providerKeySet = suppliedByProvider || hasEnv(detail, "HERMES_GATE_PROFILE_TOKEN");
  const secretCount = detail.env.filter((entry) => entry.secret).length;

  return (
    <div className="space-y-6">
      {detail.last_error ? (
        <Alert variant="destructive" className="items-start">
          <AlertCircle aria-hidden className="mt-0.5 size-4" />
          <AlertTitle>{t("detail.lastError")}</AlertTitle>
          <AlertDescription>
            <p className="font-mono text-xs break-words whitespace-pre-wrap">{detail.last_error}</p>
          </AlertDescription>
        </Alert>
      ) : null}

      <div className="grid gap-6 lg:grid-cols-2">
        <Section title={t("detail.runtime")}>
          <DataList
            items={[
              { label: t("fleet.model"), value: <Mono>{detail.model}</Mono> },
              {
                label: t("providers.form.baseUrl"),
                value: <CopyValue value={detail.base_url} label={t("providers.detail.baseUrl")} />,
              },
              ...(detail.provider
                ? [
                    {
                      label: t("clone.providerLabel"),
                      value: (
                        <span className="flex flex-wrap items-center gap-2">
                          <a
                            href={hrefFor(`/providers/${detail.provider}`)}
                            className="font-mono text-xs underline-offset-4 hover:underline"
                          >
                            {provider?.name ?? detail.provider}
                          </a>
                          {provider ? (
                            <Badge variant="outline" className="font-mono text-xs font-normal">
                              {provider.kind}
                            </Badge>
                          ) : null}
                          <span className="text-xs text-muted-foreground">
                            {t("detail.suppliesKey")}
                          </span>
                        </span>
                      ),
                    },
                  ]
                : []),
              {
                label: t("providers.form.apiKeyEnv"),
                value: <Mono>{detail.api_key_env}</Mono>,
              },
              {
                label: t("detail.providerKey"),
                value: suppliedByProvider ? (
                  <span className="flex flex-wrap items-center gap-2">
                    <Badge variant="secondary" className="font-mono text-xs font-normal">
                      provider
                    </Badge>
                    <Mono className="text-muted-foreground">
                      {provider?.name ?? detail.provider}
                    </Mono>
                  </span>
                ) : providerKeySet ? (
                  <span className="flex flex-wrap items-center gap-2">
                    <Badge variant="outline" className="font-mono text-xs font-normal">
                      set
                    </Badge>
                    <Mono className="text-muted-foreground">HERMES_GATE_PROFILE_TOKEN</Mono>
                  </span>
                ) : (
                  <div className="space-y-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <Badge
                        variant="outline"
                        className="border-destructive/40 font-mono text-xs font-normal text-destructive"
                      >
                        missing
                      </Badge>
                      {canEdit ? (
                        <Button
                          type="button"
                          variant="link"
                          size="sm"
                          className="h-auto p-0"
                          onClick={onConfigure}
                        >
                          {t("detail.addToken")}
                        </Button>
                      ) : null}
                    </div>
                    <p className="text-xs text-muted-foreground">{t("detail.noModelCall")}</p>
                  </div>
                ),
              },
              {
                label: t("create.memoryLimit"),
                value: `${detail.mem_limit_mb}\u00A0MB`,
              },
              {
                label: t("create.lanAccess"),
                value: (
                  <div className="space-y-1">
                    <Badge
                      variant={detail.allow_lan ? "secondary" : "outline"}
                      className="font-mono text-xs font-normal"
                    >
                      {detail.allow_lan ? t("common.enabled") : t("create.restricted")}
                    </Badge>
                    <p className="text-xs text-muted-foreground">
                      {detail.allow_lan ? t("detail.lanOn") : t("detail.lanOff")}
                    </p>
                  </div>
                ),
              },
            ]}
          />
        </Section>

        <Section
          title={t("clone.personaLabel")}
          action={
            canEdit ? (
              <Button type="button" variant="ghost" size="sm" onClick={onConfigure}>
                {t("providers.list.manage")}
              </Button>
            ) : (
              <Button asChild variant="ghost" size="sm">
                <a href="#/personas">{t("detail.browse")}</a>
              </Button>
            )
          }
        >
          {detail.persona ? (
            <div className="space-y-3 rounded-md border p-4">
              <div className="flex flex-wrap items-center gap-2">
                {persona ? (
                  <a
                    href={hrefFor(`/personas/${persona.id}`)}
                    className="text-sm font-medium underline-offset-4 hover:underline"
                  >
                    {persona.name}
                  </a>
                ) : (
                  <span className="font-mono text-sm">{detail.persona}</span>
                )}
                <PersonaStateBadge state={detail.persona_state} />
              </div>
              <p className="text-xs text-muted-foreground">
                {detail.persona_state === "diverged"
                  ? t("detail.personaDiverged")
                  : t("detail.personaRendered")}
              </p>
              <dl className="grid grid-cols-2 gap-2 text-xs">
                <div className="min-w-0">
                  <dt className="text-muted-foreground">{t("detail.valuesSet")}</dt>
                  <dd className="font-mono">
                    {t("create.overrides", {
                      count: Object.keys(compactVariables(detail.persona_vars ?? {})).length,
                    })}
                  </dd>
                </div>
                <div className="min-w-0">
                  <dt className="text-muted-foreground">{t("detail.personaId")}</dt>
                  <dd className="truncate font-mono">{detail.persona}</dd>
                </div>
              </dl>
            </div>
          ) : (
            <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
              {t("detail.noPersona")}
            </p>
          )}
        </Section>

        <Section
          title={t("nav.skills")}
          action={
            canEdit ? (
              <Button type="button" variant="ghost" size="sm" onClick={onConfigure}>
                {t("providers.list.manage")}
              </Button>
            ) : (
              <Button asChild variant="ghost" size="sm">
                <a href="#/skills">{t("detail.browse")}</a>
              </Button>
            )
          }
        >
          {(detail.skills ?? []).length ? (
            <ul className="divide-y rounded-md border">
              {(detail.skills ?? []).map((skill) => (
                <li key={skill.name} className="flex flex-wrap items-center gap-x-2 gap-y-1 px-3 py-2">
                  <a
                    href={hrefFor(`/skills/${skill.name}`)}
                    className="min-w-0 flex-1 truncate font-mono text-sm underline-offset-4 hover:underline"
                  >
                    {skill.name}
                  </a>
                  <Badge variant="outline" className="shrink-0 font-mono text-[11px] font-normal">
                    {skill.version ?? "follows latest"}
                  </Badge>
                  <Badge
                    variant={skill.effective_version ? "secondary" : "destructive"}
                    className="shrink-0 font-mono text-[11px] font-normal"
                  >
                    {skill.effective_version ? `mounted ${skill.effective_version}` : "unresolved"}
                  </Badge>
                </li>
              ))}
            </ul>
          ) : (
            <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
              <RichText text={t("detail.noSkill")} />
            </p>
          )}
        </Section>

        <Section title={t("detail.container")}>
          {detail.container ? (
            <DataList
              items={[
                {
                  label: t("detail.containerId"),
                  value: (
                    <CopyValue
                      value={detail.container.id}
                      label={t("detail.containerIdLabel")}
                    />
                  ),
                },
                { label: t("deployments.image"), value: <Mono>{detail.container.image}</Mono> },
                {
                  label: t("fleet.uptime"),
                  value:
                    detail.status === "running" ? (
                      <span className="font-mono text-xs">{formatUptime(detail.container.uptime_s, true)}</span>
                    ) : (
                      <span className="text-muted-foreground">{t("detail.notRunning")}</span>
                    ),
                },
              ]}
            />
          ) : (
            <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
              {t("detail.noContainer")}
            </p>
          )}
        </Section>

        <Section title={t("fleet.channel")}>
          <DataList
            items={[
              {
                label: t("detail.type"),
                value: (
                  <span className="font-mono text-xs">{t(`channel.${detail.channel}`)}</span>
                ),
              },
              {
                label: t("create.token"),
                value: env ? (
                  <span className="flex flex-wrap items-center gap-2">
                    <Badge variant="outline" className="font-mono text-xs font-normal">
                      set
                    </Badge>
                    <Mono className="text-muted-foreground">{env}</Mono>
                  </span>
                ) : (
                  <span className="text-muted-foreground">
                    {detail.channel === "none" ? t("detail.noChannel") : t("detail.noToken")}
                  </span>
                ),
              },
            ]}
          />
        </Section>

        <Section
          title={t("env.title")}
          action={
            canEdit ? (
              <Button type="button" variant="ghost" size="sm" onClick={onConfigure}>
                {t("providers.list.manage")}
              </Button>
            ) : null
          }
        >
          <DataList
            items={[
              {
                label: t("detail.variables"),
                value: detail.env.length
                  ? t(
                      detail.env.length === 1
                        ? "personas.picker.variableOne"
                        : "personas.picker.variableMany",
                      { count: detail.env.length },
                    )
                  : t("common.none"),
              },
              {
                label: t("health.secrets"),
                value: secretCount ? t("detail.encrypted", { count: secretCount }) : t("common.none"),
              },
            ]}
          />
        </Section>

        <Section
          title={t("nav.shares")}
          action={
            <Button asChild variant="ghost" size="sm">
              <a href="#/shares">{t("providers.list.manage")}</a>
            </Button>
          }
        >
          {detail.shares.length ? (
            <div className="flex flex-wrap gap-1.5 rounded-md border p-3">
              {detail.shares.map((share) => (
                <Badge key={share.name} variant="outline" className="gap-1 font-mono text-xs font-normal">
                  {share.name}
                  <span className="text-muted-foreground">{share.mode}</span>
                </Badge>
              ))}
            </div>
          ) : (
            <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
              {t("detail.noShares")}
            </p>
          )}
        </Section>

        <Section title={t("detail.storage")}>
          <DataList
            items={[
              {
                label: t("detail.dataDir"),
                value: (
                  <CopyValue value={detail.data_dir} label={t("detail.dataDirLabel")} />
                ),
              },
              {
                label: t("clone.composeProject"),
                value: <Mono>fleett-{detail.id}</Mono>,
              },
            ]}
          />
        </Section>

        <Section title={t("detail.timeline")}>
          <DataList
            items={[
              {
                label: t("common.created"),
                value: (
                  <span
                    className="font-mono text-xs"
                    title={formatRelative(detail.created_at, lang)}
                  >
                    {formatDateTime(detail.created_at)}
                  </span>
                ),
              },
              {
                label: t("common.updated"),
                value: (
                  <span
                    className="font-mono text-xs"
                    title={formatRelative(detail.updated_at, lang)}
                  >
                    {formatDateTime(detail.updated_at)}
                  </span>
                ),
              },
            ]}
          />
        </Section>
      </div>

      <Section title={t("create.notes")}>
        {detail.notes ? (
          <p className="rounded-md border px-4 py-3 text-sm whitespace-pre-wrap">{detail.notes}</p>
        ) : (
          <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
            {t("detail.noNotes")}
          </p>
        )}
      </Section>
    </div>
  );
}

interface ConfigDraft {
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

function toConfigDraft(detail: InstanceDetail): ConfigDraft {
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

function ConfigurationTab({
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

const TAB_PAGES: Record<string, string> = {
  overview: "detail.tab.overview",
  configuration: "detail.tab.configuration",
  schedules: "nav.schedules",
  incidents: "nav.incidents",
  snapshots: "detail.tab.snapshots",
  files: "files.title",
  messages: "nav.messages",
  memory: "nav.memory",
  history: "skills.detail.history",
  logs: "nav.logs",
  access: "detail.tab.access",
  egress: "detail.tab.egress",
  terminal: "detail.tab.terminal",
  compose: "detail.tab.compose",
};

/** Tabs that only an editor may open; a viewer falls back to the overview. */
const EDIT_ONLY_TABS = new Set(["configuration", "terminal"]);

/**
 * The palette links to a tab as `?tab=logs`, so one keystroke from anywhere
 * lands on the right panel. Unknown or forbidden values fall back to Overview.
 */
function requestedTab(value: string | null, canEdit: boolean): string {
  if (!value || !Object.hasOwn(TAB_PAGES, value)) return "overview";
  if (!canEdit && EDIT_ONLY_TABS.has(value)) return "overview";
  return value;
}

function DetailSkeleton() {
  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <Skeleton className="h-7 w-44" />
        <Skeleton className="h-4 w-72" />
      </div>
      <Skeleton className="h-9 w-80" />
      <div className="space-y-2">
        <Skeleton className="h-11 w-full rounded-md" />
        <Skeleton className="h-11 w-full rounded-md" />
        <Skeleton className="h-11 w-full rounded-md" />
      </div>
    </div>
  );
}

export function InstanceDetailPage({ id }: { id: string }) {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const route = useRoute();
  const tabParam = route.query.get("tab");
  const focusEnv = route.query.get("focus") === "env";
  const focusEnvParam = route.query.get("env");
  const [nonce, setNonce] = useState(0);
  const [tab, setTab] = useState(() => requestedTab(tabParam, canEdit));
  const appliedTabParam = useRef(tabParam);
  const focusEnvApplied = useRef<string | null>(null);
  // The Files tab is mounted on first visit and then kept alive (hidden), so an
  // unsaved buffer survives a trip to Overview and back.
  const [filesMounted, setFilesMounted] = useState(false);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [duplicateOpen, setDuplicateOpen] = useState(false);

  const detail = useResource(() => api.getInstance(id), [id, nonce]);
  const providers = useResource(() => api.listProviders(), []);
  const personas = useResource(() => api.listPersonas(), []);
  const templatesResource = useResource(() => api.listTemplates(), []);

  // The assistant sees the tab, the instance and its last error. The Files tab
  // refines this with the open path and buffer.
  useAssistantScope({
    page: t(TAB_PAGES[tab] ?? "detail.tab.instance"),
    instance: id,
    schema: instanceTabSchema(tab),
    errorText: detail.data?.last_error ?? undefined,
  });

  // An environment applied from the assistant is a normal PATCH: refresh so the
  // Configuration tab and the warnings show the new state.
  useEffect(() => {
    function onApplied(event: Event) {
      const applied = (event as CustomEvent<AssistantAppliedDetail>).detail;
      if (!applied || applied.instance !== id || applied.kind !== "env") return;
      setNonce((value) => value + 1);
    }
    window.addEventListener(ASSISTANT_APPLIED_EVENT, onApplied);
    return () => window.removeEventListener(ASSISTANT_APPLIED_EVENT, onApplied);
  }, [id]);

  // A lifecycle action from the command palette targets this instance too.
  useEffect(() => {
    function onChanged() {
      setNonce((value) => value + 1);
    }
    window.addEventListener(INSTANCES_CHANGED_EVENT, onChanged);
    return () => window.removeEventListener(INSTANCES_CHANGED_EVENT, onChanged);
  }, []);

  // The palette can send a second command while this page is already open.
  useEffect(() => {
    if (tabParam === appliedTabParam.current) return;
    appliedTabParam.current = tabParam;
    setTab(requestedTab(tabParam, canEdit));
  }, [tabParam, canEdit]);

  // The setup success links straight to the channel allowlist:
  // `?tab=configuration&focus=env&env=DISCORD_ALLOWED_USERS`. Reveal the row
  // that holds the variable (or the suggestion that adds it) once per load.
  useEffect(() => {
    if (!focusEnv || tab !== "configuration") return;
    const data = detail.data;
    if (!data) return;
    const template = templatesResource.data?.find((candidate) => candidate.id === data.template);
    const key =
      (focusEnvParam && /^[A-Z_][A-Z0-9_]*$/.test(focusEnvParam) ? focusEnvParam : null) ??
      template?.env.find((entry) => entry.required && /ALLOW(?:ED)?_USERS/.test(entry.key))?.key ??
      null;
    // Without an explicit key, wait for the template suggestions that name it.
    if (!key && templatesResource.data === null && !templatesResource.error) return;
    const marker = `${data.id}:${key ?? "env"}`;
    if (focusEnvApplied.current === marker) return;
    focusEnvApplied.current = marker;
    // The configuration tab grows while shares, personas and skills load. Keep
    // the row in view for a short window instead of scrolling into a page that
    // is still assembling; a click, wheel or key hands control back.
    let stopped = false;
    let observer: ResizeObserver | null = null;
    const stop = () => {
      stopped = true;
      observer?.disconnect();
    };
    const reveal = () => {
      if (stopped) return;
      const row = key ? document.querySelector<HTMLElement>(`[data-env-key="${key}"]`) : null;
      const suggestion = key
        ? document.querySelector<HTMLButtonElement>(`[data-env-suggestion="${key}"]`)
        : null;
      const target = row ?? suggestion ?? document.getElementById("instance-env");
      if (!target) return;
      const rect = target.getBoundingClientRect();
      const visible = rect.top >= 72 && rect.bottom <= window.innerHeight;
      if (!visible) target.scrollIntoView({ block: "center" });
      if (row) row.querySelector<HTMLButtonElement>("button")?.focus({ preventScroll: true });
      else suggestion?.focus({ preventScroll: true });
    };
    observer = new ResizeObserver(reveal);
    observer.observe(document.getElementById("main") ?? document.body);
    const stopTimer = window.setTimeout(stop, 4000);
    const timers = [80, 420, 900, 1500].map((delay) => window.setTimeout(reveal, delay));
    const cancelEvents: (keyof WindowEventMap)[] = ["pointerdown", "wheel", "touchstart", "keydown"];
    for (const type of cancelEvents) window.addEventListener(type, stop, { once: true, passive: true });
    return () => {
      stopped = true;
      window.clearTimeout(stopTimer);
      for (const timer of timers) window.clearTimeout(timer);
      for (const type of cancelEvents) window.removeEventListener(type, stop);
      observer?.disconnect();
    };
  }, [focusEnv, focusEnvParam, tab, detail.data, templatesResource.data]);

  const [configDraft, setConfigDraft] = useState<ConfigDraft | null>(null);
  const [envRows, setEnvRows] = useState<EnvRow[] | null>(null);
  const configDraftFor = useRef<string | null>(null);
  const configResetPending = useRef(false);
  const envResetPending = useRef(false);
  const envServerRef = useRef<EnvEntry[] | null>(null);

  useEffect(() => {
    const data = detail.data;
    if (!data) return;
    // Keep edits across tab switches and background reloads, but take the
    // server copy after a save. A config save must not drop unsaved variable
    // edits, and an env apply must not drop unsaved config edits. An untouched
    // variable list follows the server, so a rotated gate key shows up.
    const newInstance = configDraftFor.current !== data.id;
    const serverChanged = envServerRef.current !== data.env;
    const envClean =
      envRows === null || (envServerRef.current !== null && envRowsMatch(envServerRef.current, envRows));
    const resetDraft = newInstance || configResetPending.current;
    const resetEnv = newInstance || envResetPending.current || (serverChanged && envClean);
    if (!resetDraft && !resetEnv) return;
    if (resetDraft) setConfigDraft(toConfigDraft(data));
    if (resetEnv) setEnvRows(envRowsFromDetail(data.env));
    configDraftFor.current = data.id;
    configResetPending.current = false;
    envResetPending.current = false;
    envServerRef.current = data.env;
  }, [detail.data, envRows]);

  useEffect(() => {
    if (tab === "files") setFilesMounted(true);
  }, [tab]);

  function handleSaved() {
    configResetPending.current = true;
    setNonce((value) => value + 1);
  }

  function handleEnvApplied() {
    envResetPending.current = true;
    setNonce((value) => value + 1);
  }

  async function run(action: "start" | "stop" | "restart") {
    setPendingAction(action);
    setActionError(null);
    try {
      await api.action(id, action);
      toast.success(
        t("detail.actionDone", {
          id,
          state: t(
            action === "stop"
              ? "status.stopped"
              : action === "start"
                ? "status.started"
                : "status.restarted",
          ),
        }),
      );
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setPendingAction(null);
      setNonce((value) => value + 1);
    }
  }

  if (detail.loading && !detail.data) return <DetailSkeleton />;

  if (!detail.data) {
    return (
      <>
        <PageHeader
          title={id}
          actions={
            <Button asChild variant="outline">
              <a href="#/instances">{t("shell.backToInstances")}</a>
            </Button>
          }
        />
        {detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null}
      </>
    );
  }

  const data = detail.data;
  const running = data.status === "running";
  const busy = pendingAction !== null;
  const provider = data.provider
    ? (providers.data?.find((candidate) => candidate.id === data.provider) ?? null)
    : null;
  const persona = data.persona
    ? (personas.data?.find((candidate) => candidate.id === data.persona) ?? null)
    : null;
  const templateEnv =
    templatesResource.data?.find((candidate) => candidate.id === data.template)?.env ?? [];
  // Deep link from the setup success: the allowlist variable to reveal.
  const highlightEnvKey = focusEnv
    ? ((focusEnvParam && /^[A-Z_][A-Z0-9_]*$/.test(focusEnvParam) ? focusEnvParam : null) ??
      templateEnv.find((entry) => entry.required && /ALLOW(?:ED)?_USERS/.test(entry.key))?.key ??
      null)
    : null;
  const uptime =
    running && data.container ? formatUptime(data.container.uptime_s) : null;
  const warnings = data.warnings ?? [];
  const envWarning = warnings.some((warning) => /required|allowlist|environment|is not set/i.test(warning));

  function reviewWarnings() {
    setTab("configuration");
    window.setTimeout(() => {
      const node = document.getElementById(envWarning ? "instance-env" : "cfg-name");
      if (envWarning) node?.scrollIntoView({ block: "start" });
      node?.querySelector?.("input")?.focus();
      if (node instanceof HTMLInputElement) node.focus();
    }, 60);
  }

  return (
    <>
      <PageHeader
        title={
          <span className="flex flex-wrap items-center gap-3">
            {data.name}
            <StatusBadge status={data.status} />
            {data.persona && data.persona_state === "diverged" ? (
              <PersonaStateBadge state="diverged" />
            ) : null}
          </span>
        }
        description={
          <span className="flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-xs">
            <span>{data.id}</span>
            <span aria-hidden>·</span>
            <span>{data.template}</span>
            <span aria-hidden>·</span>
            <span>{data.container ? data.container.id : t("detail.noContainerShort")}</span>
            {uptime ? (
              <>
                <span aria-hidden>·</span>
                <span>{t("detail.uptimeUp", { uptime })}</span>
              </>
            ) : null}
          </span>
        }
        actions={
          <>
            <Button
              variant="outline"
              size="icon-sm"
              onClick={() => setNonce((value) => value + 1)}
              aria-label={t("detail.refresh")}
              title={t("detail.refresh")}
            >
              <RefreshCw aria-hidden className={detail.refreshing ? "animate-spin" : undefined} />
            </Button>
            {canEdit ? (
              <>
                {running ? (
                  <Button size="sm" onClick={() => run("stop")} disabled={busy}>
                    {pendingAction === "stop" ? (
                      <Loader2 aria-hidden className="animate-spin" />
                    ) : (
                      <Square aria-hidden />
                    )}
                    {t("action.stop")}
                  </Button>
                ) : (
                  <Button size="sm" onClick={() => run("start")} disabled={busy}>
                    {pendingAction === "start" ? (
                      <Loader2 aria-hidden className="animate-spin" />
                    ) : (
                      <Play aria-hidden />
                    )}
                    {t("action.start")}
                  </Button>
                )}
                <Button variant="outline" size="sm" onClick={() => run("restart")} disabled={busy}>
                  {pendingAction === "restart" ? (
                    <Loader2 aria-hidden className="animate-spin" />
                  ) : (
                    <RotateCw aria-hidden />
                  )}
                  {t("action.restart")}
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="icon-sm"
                      aria-label={t("providers.detail.moreActions")}
                      title={t("providers.detail.moreActions")}
                      disabled={busy}
                    >
                      <MoreHorizontal aria-hidden />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem onSelect={() => setDuplicateOpen(true)}>
                      <CopyPlus aria-hidden />
                      {t("detail.duplicate")}
                    </DropdownMenuItem>
                    <DropdownMenuItem variant="destructive" onSelect={() => setDeleteOpen(true)}>
                      <Trash2 aria-hidden />
                      {t("detail.deleteInstance")}
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </>
            ) : null}
          </>
        }
      />

      {actionError ? <ErrorAlert error={actionError} /> : null}

      {warnings.length > 0 ? (
        <Alert className="border-amber-600/40 bg-amber-50/40 dark:bg-amber-950/20">
          <TriangleAlert aria-hidden className="text-amber-600 dark:text-amber-400" />
          <AlertTitle>{t("detail.warningsTitle")}</AlertTitle>
          <AlertDescription>
            <ul className="list-disc space-y-1 pl-4 text-sm text-muted-foreground">
              {warnings.map((warning) => (
                <li key={warning}>{warning}</li>
              ))}
            </ul>
            {canEdit ? (
              <Button type="button" variant="outline" size="sm" className="mt-2" onClick={reviewWarnings}>
                {envWarning ? t("detail.fixEnv") : t("detail.reviewConfig")}
              </Button>
            ) : null}
          </AlertDescription>
        </Alert>
      ) : null}

      <Tabs value={tab} onValueChange={setTab}>
        <div className="overflow-x-auto">
          <TabsList className="w-full justify-start sm:w-fit">
            <TabsTrigger value="overview">{t(TAB_PAGES.overview)}</TabsTrigger>
            {canEdit ? (
              <TabsTrigger value="configuration">{t(TAB_PAGES.configuration)}</TabsTrigger>
            ) : null}
            <TabsTrigger value="schedules">{t(TAB_PAGES.schedules)}</TabsTrigger>
            <TabsTrigger value="incidents">{t(TAB_PAGES.incidents)}</TabsTrigger>
            <TabsTrigger value="snapshots">{t(TAB_PAGES.snapshots)}</TabsTrigger>
            <TabsTrigger value="files">{t(TAB_PAGES.files)}</TabsTrigger>
            <TabsTrigger value="messages">{t(TAB_PAGES.messages)}</TabsTrigger>
            <TabsTrigger value="memory">{t(TAB_PAGES.memory)}</TabsTrigger>
            <TabsTrigger value="history">{t(TAB_PAGES.history)}</TabsTrigger>
            <TabsTrigger value="logs">{t(TAB_PAGES.logs)}</TabsTrigger>
            <TabsTrigger value="access">{t(TAB_PAGES.access)}</TabsTrigger>
            <TabsTrigger value="egress">{t(TAB_PAGES.egress)}</TabsTrigger>
            {canEdit ? (
              <TabsTrigger value="terminal">{t(TAB_PAGES.terminal)}</TabsTrigger>
            ) : null}
            <TabsTrigger value="compose">{t(TAB_PAGES.compose)}</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="overview" className="mt-4">
          <OverviewTab
            detail={data}
            provider={provider}
            persona={persona}
            canEdit={canEdit}
            onConfigure={() => setTab("configuration")}
          />
        </TabsContent>

        {canEdit ? (
          <TabsContent value="configuration" className="mt-4">
            {configDraft && envRows ? (
              <ConfigurationTab
                detail={data}
                provider={provider}
                templateEnv={templateEnv}
                highlightEnvKey={highlightEnvKey}
                draft={configDraft}
                onDraftChange={setConfigDraft}
                envRows={envRows}
                onEnvRowsChange={setEnvRows}
                onSaved={handleSaved}
                onEnvApplied={handleEnvApplied}
              />
            ) : (
              <Skeleton className="h-64 w-full rounded-md" />
            )}
          </TabsContent>
        ) : null}

        <TabsContent value="schedules" className="mt-4">
          <InstanceSchedules instanceId={data.id} canEdit={canEdit} />
        </TabsContent>

        <TabsContent value="incidents" className="mt-4">
          <InstanceIncidents id={data.id} />
        </TabsContent>

        <TabsContent value="snapshots" className="mt-4">
          <InstanceSnapshots id={data.id} />
        </TabsContent>

        {/* `forceMount` keeps the editor mounted while another tab is shown;
            the `hidden` class keeps an inactive panel out of the a11y tree. */}
        <TabsContent
          value="files"
          forceMount
          className={cn("mt-4", tab !== "files" && "hidden")}
        >
          {filesMounted ? (
            <InstanceFiles
              id={data.id}
              canEdit={canEdit}
              active={tab === "files"}
              onRestarted={() => setNonce((value) => value + 1)}
              onFileSaved={() => setNonce((value) => value + 1)}
            />
          ) : null}
        </TabsContent>

        <TabsContent value="messages" className="mt-4">
          <InstanceMessagesTab id={data.id} canEdit={canEdit} />
        </TabsContent>

        <TabsContent value="memory" className="mt-4">
          <InstanceMemoryTab id={data.id} canEdit={canEdit} />
        </TabsContent>

        <TabsContent value="history" className="mt-4">
          <InstanceHistory id={data.id} canEdit={canEdit} />
        </TabsContent>

        <TabsContent value="logs" className="mt-4">
          <InstanceLogs id={data.id} />
        </TabsContent>
        <TabsContent value="access" className="mt-4">
          <InstanceAccess id={data.id} />
        </TabsContent>
        <TabsContent value="egress" className="mt-4">
          <InstanceEgress id={data.id} />
        </TabsContent>
        {canEdit ? (
          <TabsContent value="terminal" className="mt-4">
            <InstanceTerminal id={data.id} />
          </TabsContent>
        ) : null}
        <TabsContent value="compose" className="mt-4 space-y-3">
          <p className="text-sm text-muted-foreground">
            <RichText text={t("detail.composeHint")} />
          </p>
          <CodeBlock value={data.compose_yaml} title={`instances/${data.id}/compose.yaml`} />
        </TabsContent>
      </Tabs>

      {canEdit ? (
        <>
          <DuplicateInstanceDialog
            source={data}
            open={duplicateOpen}
            onOpenChange={setDuplicateOpen}
            onCloned={(clone) => {
              toast.success(t("detail.duplicated", { source: data.id, clone: clone.id }));
              navigate(`/instances/${clone.id}`);
            }}
          />
          <DeleteInstanceDialog
            instance={{ id: data.id, name: data.name }}
            open={deleteOpen}
            onOpenChange={setDeleteOpen}
            onDeleted={() => {
              toast.success(t("fleet.deletedId", { id: data.id }));
              navigate("/instances");
            }}
          />
        </>
      ) : null}
    </>
  );
}
