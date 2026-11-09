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
  Terminal,
  Trash2,
  TriangleAlert,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { ChannelType, EnvEntry, EnvSuggestion, InstanceDetail, PatchInstanceInput, PersonaSummary, ProviderSummary, ShareRef } from "@/api/types";
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
import { EmptyState } from "@/components/empty-state";
import { InstanceMemoryTab, InstanceMessagesTab } from "@/components/instance-bus";
import { InstanceEnvironment } from "@/components/instance-env";
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
import { PageHeader } from "@/components/page-header";
import { PersonaStateBadge } from "@/components/persona-state-badge";
import { PersonaSelect } from "@/components/persona-select";
import { PersonaVariableInputs } from "@/components/persona-vars";
import { SharePicker } from "@/components/share-picker";
import { StatusBadge } from "@/components/status-badge";
import {
  ASSISTANT_APPLIED_EVENT,
  instanceTabSchema,
  useAssistantScope,
  type AssistantAppliedDetail,
} from "@/lib/assistant";
import { envRowsFromDetail, envRowsMatch, type EnvRow } from "@/lib/env";
import { formatClock, formatDateTime, formatRelative, formatUptime, plural } from "@/lib/format";
import { applySucceeded, compactVariables } from "@/lib/personas";
import { navigate, hrefFor } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { isHttpUrl } from "@/lib/validation";
import { cn } from "@/lib/utils";

function Mono({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={cn("font-mono text-xs break-all", className)}>{children}</span>;
}

function CopyValue({ value, label }: { value: string; label: string }) {
  return (
    <span className="flex min-w-0 items-center gap-1">
      <span className="min-w-0 flex-1 font-mono text-xs break-all">{value}</span>
      <CopyButton
        value={value}
        label={`Copy ${label}`}
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
  const env = tokenEnv(detail);
  const suppliedByProvider = !!detail.provider;
  const providerKeySet = suppliedByProvider || hasEnv(detail, "HERMES_GATE_PROFILE_TOKEN");
  const secretCount = detail.env.filter((entry) => entry.secret).length;

  return (
    <div className="space-y-6">
      {detail.last_error ? (
        <Alert variant="destructive" className="items-start">
          <AlertCircle aria-hidden className="mt-0.5 size-4" />
          <AlertTitle>Last error</AlertTitle>
          <AlertDescription>
            <p className="font-mono text-xs break-words whitespace-pre-wrap">{detail.last_error}</p>
          </AlertDescription>
        </Alert>
      ) : null}

      <div className="grid gap-6 lg:grid-cols-2">
        <Section title="Runtime">
          <DataList
            items={[
              { label: "Model", value: <Mono>{detail.model}</Mono> },
              { label: "Base URL", value: <CopyValue value={detail.base_url} label="base URL" /> },
              ...(detail.provider
                ? [
                    {
                      label: "Provider",
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
                            supplies the API key
                          </span>
                        </span>
                      ),
                    },
                  ]
                : []),
              { label: "API key env", value: <Mono>{detail.api_key_env}</Mono> },
              {
                label: "Provider key",
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
                          Add the token
                        </Button>
                      ) : null}
                    </div>
                    <p className="text-xs text-muted-foreground">The agent cannot call the model.</p>
                  </div>
                ),
              },
              { label: "Memory limit", value: `${detail.mem_limit_mb}\u00A0MB` },
              {
                label: "LAN access",
                value: (
                  <div className="space-y-1">
                    <Badge
                      variant={detail.allow_lan ? "secondary" : "outline"}
                      className="font-mono text-xs font-normal"
                    >
                      {detail.allow_lan ? "enabled" : "restricted"}
                    </Badge>
                    <p className="text-xs text-muted-foreground">
                      {detail.allow_lan
                        ? "Can reach 10/8, 172.16/12 and 192.168/16."
                        : "No LAN route. The agent only joins the fleett network."}
                    </p>
                  </div>
                ),
              },
            ]}
          />
        </Section>

        <Section
          title="Persona"
          action={
            canEdit ? (
              <Button type="button" variant="ghost" size="sm" onClick={onConfigure}>
                Manage
              </Button>
            ) : (
              <Button asChild variant="ghost" size="sm">
                <a href="#/personas">Browse</a>
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
                  ? "data/SOUL.md was edited by hand after the persona was applied. Reapply the persona in Configuration to take it back over, or keep the manual text."
                  : "data/SOUL.md is rendered from this persona; applying it rewrites the file and restarts the instance."}
              </p>
              <dl className="grid grid-cols-2 gap-2 text-xs">
                <div className="min-w-0">
                  <dt className="text-muted-foreground">Values set</dt>
                  <dd className="font-mono">
                    {Object.keys(compactVariables(detail.persona_vars ?? {})).length}{" "}
                    {plural(
                      Object.keys(compactVariables(detail.persona_vars ?? {})).length,
                      "override",
                    )}
                  </dd>
                </div>
                <div className="min-w-0">
                  <dt className="text-muted-foreground">Persona id</dt>
                  <dd className="truncate font-mono">{detail.persona}</dd>
                </div>
              </dl>
            </div>
          ) : (
            <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
              No persona. The instance keeps the SOUL.md its template provides; assign one in
              Configuration to render a shared personality.
            </p>
          )}
        </Section>

        <Section title="Container">
          {detail.container ? (
            <DataList
              items={[
                { label: "Container id", value: <CopyValue value={detail.container.id} label="container id" /> },
                { label: "Image", value: <Mono>{detail.container.image}</Mono> },
                {
                  label: "Uptime",
                  value:
                    detail.status === "running" ? (
                      <span className="font-mono text-xs">{formatUptime(detail.container.uptime_s, true)}</span>
                    ) : (
                      <span className="text-muted-foreground">Not running</span>
                    ),
                },
              ]}
            />
          ) : (
            <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
              No container. Start the instance to create one.
            </p>
          )}
        </Section>

        <Section title="Channel">
          <DataList
            items={[
              { label: "Type", value: <span className="font-mono text-xs">{detail.channel}</span> },
              {
                label: "Token",
                value: env ? (
                  <span className="flex flex-wrap items-center gap-2">
                    <Badge variant="outline" className="font-mono text-xs font-normal">
                      set
                    </Badge>
                    <Mono className="text-muted-foreground">{env}</Mono>
                  </span>
                ) : (
                  <span className="text-muted-foreground">
                    {detail.channel === "none" ? "No channel" : "No token stored"}
                  </span>
                ),
              },
            ]}
          />
        </Section>

        <Section
          title="Environment"
          action={
            canEdit ? (
              <Button type="button" variant="ghost" size="sm" onClick={onConfigure}>
                Manage
              </Button>
            ) : null
          }
        >
          <DataList
            items={[
              {
                label: "Variables",
                value: detail.env.length
                  ? `${detail.env.length} ${plural(detail.env.length, "variable")}`
                  : "None",
              },
              {
                label: "Secrets",
                value: secretCount ? `${secretCount} encrypted` : "None",
              },
            ]}
          />
        </Section>

        <Section
          title="Shares"
          action={
            <Button asChild variant="ghost" size="sm">
              <a href="#/shares">Manage</a>
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
            <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">No shares attached.</p>
          )}
        </Section>

        <Section title="Storage">
          <DataList
            items={[
              { label: "Data directory", value: <CopyValue value={detail.data_dir} label="data directory" /> },
              { label: "Compose project", value: <Mono>fleett-{detail.id}</Mono> },
            ]}
          />
        </Section>

        <Section title="Timeline">
          <DataList
            items={[
              {
                label: "Created",
                value: (
                  <span className="font-mono text-xs" title={formatRelative(detail.created_at)}>
                    {formatDateTime(detail.created_at)}
                  </span>
                ),
              },
              {
                label: "Updated",
                value: (
                  <span className="font-mono text-xs" title={formatRelative(detail.updated_at)}>
                    {formatDateTime(detail.updated_at)}
                  </span>
                ),
              },
            ]}
          />
        </Section>
      </div>

      <Section title="Notes">
        {detail.notes ? (
          <p className="rounded-md border px-4 py-3 text-sm whitespace-pre-wrap">{detail.notes}</p>
        ) : (
          <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">No notes.</p>
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
  };
}

function validateConfig(draft: ConfigDraft, originalChannel: string): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!draft.name.trim()) errors.name = "Enter a name.";
  if (!draft.model.trim()) errors.model = "Enter a model.";
  if (!draft.base_url.trim()) errors.base_url = "Enter a base URL.";
  else if (!isHttpUrl(draft.base_url.trim())) errors.base_url = "Use an absolute http or https URL.";
  if (!draft.api_key_env.trim()) errors.api_key_env = "Enter an environment variable name.";
  else if (!/^[A-Z_][A-Z0-9_]*$/.test(draft.api_key_env.trim()))
    errors.api_key_env = "Use uppercase letters, digits and underscores.";
  const memory = Number(draft.mem_limit_mb);
  if (!Number.isFinite(memory) || memory < 128 || memory > 8192) errors.mem_limit_mb = "Use 128 to 8192 MB.";
  if (draft.channel !== "none" && draft.channel !== originalChannel && !draft.token.trim())
    errors.token = "A token is required after switching channel type.";
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
  draft: ConfigDraft;
  onDraftChange: (next: ConfigDraft) => void;
  envRows: EnvRow[];
  onEnvRowsChange: (rows: EnvRow[]) => void;
  onSaved: () => void;
  onEnvApplied: () => void;
}) {
  const sharesResource = useResource(() => api.listShares(), []);
  const personas = useResource(() => api.listPersonas(), []);
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [reapplying, setReapplying] = useState(false);
  const [reapplyError, setReapplyError] = useState<string | null>(null);

  const errors = validateConfig(draft, detail.channel);
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
        setReapplyError(entry?.error ?? "The controller did not apply the persona.");
      } else {
        toast.success("Persona reapplied: SOUL.md rewritten and the instance restarted.");
      }
      onSaved();
    } catch (cause) {
      setReapplyError(toApiError(cause).message);
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
      };
      await api.patchInstance(detail.id, patch);
      toast.success("Configuration saved.");
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
            <Label htmlFor="cfg-name">Name</Label>
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
            <Label htmlFor="cfg-model">Model</Label>
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
          <Label htmlFor="cfg-base-url">Base URL</Label>
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
            <Label htmlFor="cfg-api-key-env">API key env</Label>
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
            <Label htmlFor="cfg-mem-limit">Memory limit (MB)</Label>
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
              Credential supplied by provider{" "}
              <a
                href={hrefFor(`/providers/${provider.id}`)}
                className="font-mono underline-offset-4 hover:underline"
              >
                {provider.name}
              </a>
              .
            </p>
            <p className="text-xs text-muted-foreground">
              No per-instance key is stored. Base URL and API key env stay editable here; rotate the
              secret on the provider to change the credential for every attached instance.
            </p>
          </div>
        ) : (
          <div className="space-y-2">
            <Label htmlFor="cfg-gate-token">Provider token</Label>
            <Input
              id="cfg-gate-token"
              type="password"
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              value={draft.gate_token}
              placeholder={providerKeySet ? "Keep current token" : "Not set"}
              aria-describedby="cfg-gate-token-help"
              onChange={(event) => update("gate_token", event.target.value)}
            />
            <p id="cfg-gate-token-help" className="text-xs text-muted-foreground">
              {providerKeySet
                ? "A provider key is stored. Leave empty to keep it."
                : "No provider key stored. The agent cannot call the model without one."}{" "}
              Encrypted at rest and never returned by the API. The host needs its secrets key
              (<span className="font-mono">sudo fleett secrets init</span>).
            </p>
            {secretsError ? <ErrorAlert error={secretsError} /> : null}
          </div>
        )}

        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="cfg-channel">Channel</Label>
            <Select value={draft.channel} onValueChange={(value) => update("channel", value as ChannelType)}>
              <SelectTrigger id="cfg-channel" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {CHANNEL_TYPES.map((type) => (
                  <SelectItem key={type} value={type}>
                    {type}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {draft.channel !== "none" ? (
            <div className="space-y-2">
              <Label htmlFor="cfg-token">Bot token</Label>
              <Input
                id="cfg-token"
                type="password"
                autoComplete="off"
                placeholder="Keep current token"
                className="font-mono"
                aria-invalid={showErrors && !!errors.token}
                aria-describedby={
                  showErrors && errors.token ? "cfg-token-error" : "cfg-token-help"
                }
                onChange={(event) => update("token", event.target.value)}
              />
              <p id="cfg-token-help" className="text-xs text-muted-foreground">
                {detail.channel !== "none" && detail.channel === draft.channel
                  ? "Empty keeps the stored token. A new value rotates it."
                  : "Required when switching channel type."}
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
          <legend className="text-sm font-medium">Network</legend>
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
                Allow LAN access
              </Label>
              <p id="cfg-allow-lan-help" className="text-xs text-muted-foreground">
                The agent can reach 10/8, 172.16/12 and 192.168/16, including Proxmox, OPNsense and
                Infisical. Only enable for agents you trust.
              </p>
              <p className="text-xs text-muted-foreground">
                {draft.allow_lan ? (
                  <>
                    Joins <span className="font-mono">fleett-lan</span> on top of the restricted{" "}
                    <span className="font-mono">fleett</span> network.
                  </>
                ) : (
                  <>
                    Stays on the restricted <span className="font-mono">fleett</span> network. No route to
                    the LAN.
                  </>
                )}
              </p>
            </div>
          </div>
          {egressError ? <ErrorAlert error={egressError} /> : null}
        </fieldset>

        <fieldset className="min-w-0 space-y-3">
          <legend className="text-sm font-medium">Persona</legend>
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
              Renders a shared SOUL.md into{" "}
              <span className="font-mono">data/SOUL.md</span>. Values stay on this instance; saving
              rewrites the file and restarts the container.
            </p>
            <p className="text-xs text-muted-foreground">
              <a href="#/personas" className="underline underline-offset-4">
                Manage personas
              </a>
            </p>
            {personas.error ? (
              <p className="text-xs text-destructive">
                Personas could not be loaded: {personas.error.message}{" "}
                <button
                  type="button"
                  className="underline underline-offset-4"
                  onClick={personas.reload}
                >
                  Retry
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
              No persona: the SOUL.md currently on disk stays untouched.
            </p>
          )}

          {divergedActive ? (
            <Alert className="border-amber-600/40 bg-amber-50/40 dark:bg-amber-950/20">
              <TriangleAlert aria-hidden className="text-amber-600 dark:text-amber-400" />
              <AlertTitle>SOUL.md was edited by hand</AlertTitle>
              <AlertDescription>
                <p className="text-sm text-muted-foreground">
                  The file on disk no longer matches this persona. Reapplying rewrites{" "}
                  <span className="font-mono">data/SOUL.md</span> from the persona and restarts the
                  instance; unsaved variable values above are saved first.
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
                  Reapply persona
                </Button>
                {reapplyError ? (
                  <p className="mt-2 text-xs text-destructive">{reapplyError}</p>
                ) : null}
              </AlertDescription>
            </Alert>
          ) : null}
        </fieldset>

        <fieldset className="min-w-0 space-y-3">
          <legend className="text-sm font-medium">Shares</legend>
          <SharePicker
            shares={sharesResource.data ?? []}
            value={draft.shares}
            onChange={(next) => update("shares", next)}
            loading={sharesResource.loading && !sharesResource.data}
            error={sharesResource.error}
            onRetry={sharesResource.reload}
          />
        </fieldset>

        <div className="space-y-2">
          <Label htmlFor="cfg-notes">Notes</Label>
          <Textarea id="cfg-notes" rows={3} value={draft.notes} onChange={(event) => update("notes", event.target.value)} />
        </div>

        <div className="flex flex-wrap items-center gap-2 border-t pt-4">
          <Button type="submit" disabled={!dirty || pending}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
            Save changes
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
            Discard
          </Button>
          <p className="text-xs text-muted-foreground sm:ml-auto">
            Saving rewrites <span className="font-mono">compose.yaml</span> and recreates the container.
          </p>
        </div>
      </form>

      <div id="instance-env" className="scroll-mt-24">
        <InstanceEnvironment
          detail={detail}
          rows={envRows}
          suggestions={templateEnv}
          onRowsChange={onEnvRowsChange}
          onApplied={onEnvApplied}
        />
      </div>
    </div>
  );
}

function LogsTab({ id }: { id: string }) {
  const [tail, setTail] = useState("200");
  const [nonce, setNonce] = useState(0);
  const [fetchedAt, setFetchedAt] = useState<Date | null>(null);
  const logs = useResource(() => api.logs(id, Number(tail)), [id, tail, nonce]);
  const preRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    if (logs.data !== null) setFetchedAt(new Date());
  }, [logs.data]);

  useEffect(() => {
    const node = preRef.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [logs.data]);

  const text = logs.data ?? "";
  const lineCount = text ? text.split("\n").length : 0;

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <Select value={tail} onValueChange={setTail}>
          <SelectTrigger className="w-32" aria-label="Number of lines">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {["100", "200", "500", "1000"].map((value) => (
              <SelectItem key={value} value={value}>
                {value} lines
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
          <RefreshCw aria-hidden className={logs.refreshing ? "animate-spin" : undefined} />
          Refresh
        </Button>
        <CopyButton value={text} label="Copy logs" showLabel size="sm" variant="outline" disabled={!text} />
        <span className="text-xs text-muted-foreground sm:ml-auto">
          {text ? `${lineCount} lines` : "No output"}
          {fetchedAt ? ` · ${formatClock(fetchedAt)}` : ""}
        </span>
      </div>

      {logs.error ? <ErrorAlert error={logs.error} onRetry={logs.reload} /> : null}

      {logs.loading && logs.data === null ? (
        <Skeleton className="h-64 w-full rounded-md" />
      ) : text ? (
        <pre
          ref={preRef}
          tabIndex={0}
          role="region"
          aria-label="Container logs"
          className="max-h-[60vh] overflow-auto rounded-md border bg-muted/20 p-4 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words"
        >
          {text}
        </pre>
      ) : (
        <div className="rounded-md border">
          <EmptyState
            icon={Terminal}
            title="No output"
            description="The container may be stopped, or it has not written a log yet."
            action={
              <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
                <RefreshCw aria-hidden />
                Refresh
              </Button>
            }
          />
        </div>
      )}
    </div>
  );
}

const TAB_PAGES: Record<string, string> = {
  overview: "Overview",
  configuration: "Configuration",
  schedules: "Schedules",
  incidents: "Incidents",
  snapshots: "Snapshots",
  files: "Files",
  messages: "Messages",
  memory: "Memory",
  history: "History",
  logs: "Logs",
  access: "Access",
  egress: "Egress",
  terminal: "Terminal",
  compose: "Compose",
};

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
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [tab, setTab] = useState("overview");
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
    page: TAB_PAGES[tab] ?? "Instance",
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
        `${id} ${action === "stop" ? "stopped" : action === "start" ? "started" : "restarted"}.`,
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
              <a href="#/instances">Back to instances</a>
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
            <span>{data.container ? data.container.id : "no container"}</span>
            {uptime ? (
              <>
                <span aria-hidden>·</span>
                <span>up {uptime}</span>
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
              aria-label="Refresh instance"
              title="Refresh instance"
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
                    Stop
                  </Button>
                ) : (
                  <Button size="sm" onClick={() => run("start")} disabled={busy}>
                    {pendingAction === "start" ? (
                      <Loader2 aria-hidden className="animate-spin" />
                    ) : (
                      <Play aria-hidden />
                    )}
                    Start
                  </Button>
                )}
                <Button variant="outline" size="sm" onClick={() => run("restart")} disabled={busy}>
                  {pendingAction === "restart" ? (
                    <Loader2 aria-hidden className="animate-spin" />
                  ) : (
                    <RotateCw aria-hidden />
                  )}
                  Restart
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="icon-sm"
                      aria-label="More actions"
                      title="More actions"
                      disabled={busy}
                    >
                      <MoreHorizontal aria-hidden />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem onSelect={() => setDuplicateOpen(true)}>
                      <CopyPlus aria-hidden />
                      Duplicate instance…
                    </DropdownMenuItem>
                    <DropdownMenuItem variant="destructive" onSelect={() => setDeleteOpen(true)}>
                      <Trash2 aria-hidden />
                      Delete instance…
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
          <AlertTitle>Needs attention</AlertTitle>
          <AlertDescription>
            <ul className="list-disc space-y-1 pl-4 text-sm text-muted-foreground">
              {warnings.map((warning) => (
                <li key={warning}>{warning}</li>
              ))}
            </ul>
            {canEdit ? (
              <Button type="button" variant="outline" size="sm" className="mt-2" onClick={reviewWarnings}>
                {envWarning ? "Fix environment" : "Review configuration"}
              </Button>
            ) : null}
          </AlertDescription>
        </Alert>
      ) : null}

      <Tabs value={tab} onValueChange={setTab}>
        <div className="overflow-x-auto">
          <TabsList className="w-full justify-start sm:w-fit">
            <TabsTrigger value="overview">Overview</TabsTrigger>
            {canEdit ? <TabsTrigger value="configuration">Configuration</TabsTrigger> : null}
            <TabsTrigger value="schedules">Schedules</TabsTrigger>
            <TabsTrigger value="incidents">Incidents</TabsTrigger>
            <TabsTrigger value="snapshots">Snapshots</TabsTrigger>
            <TabsTrigger value="files">Files</TabsTrigger>
            <TabsTrigger value="messages">Messages</TabsTrigger>
            <TabsTrigger value="memory">Memory</TabsTrigger>
            <TabsTrigger value="history">History</TabsTrigger>
            <TabsTrigger value="logs">Logs</TabsTrigger>
            <TabsTrigger value="access">Access</TabsTrigger>
            <TabsTrigger value="egress">Egress</TabsTrigger>
            {canEdit ? <TabsTrigger value="terminal">Terminal</TabsTrigger> : null}
            <TabsTrigger value="compose">Compose</TabsTrigger>
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
          <LogsTab id={data.id} />
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
            <span className="font-mono">compose.yaml</span> is the source of truth. The controller rewrites it on
            every configuration change.
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
              toast.success(`Duplicated ${data.id} as ${clone.id}.`);
              navigate(`/instances/${clone.id}`);
            }}
          />
          <DeleteInstanceDialog
            instance={{ id: data.id, name: data.name }}
            open={deleteOpen}
            onOpenChange={setDeleteOpen}
            onDeleted={() => {
              toast.success(`Instance ${data.id} deleted.`);
              navigate("/instances");
            }}
          />
        </>
      ) : null}
    </>
  );
}
