import { useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from "react";
import { Check, Loader2, Plus } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { ChannelType, ShareRef } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { SharePicker } from "@/components/share-picker";
import { isHttpUrl, isValidSlug } from "@/lib/validation";
import { navigate, setRouteGuard, useRoute } from "@/lib/router";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

const STEPS = ["Identity", "Template", "Model & network", "Channel", "Shares", "Review"] as const;
const LAST_STEP = STEPS.length - 1;

const CHANNELS: { value: ChannelType; label: string; hint: string }[] = [
  { value: "none", label: "None", hint: "No chat channel" },
  { value: "telegram", label: "Telegram", hint: "Bot token required" },
  { value: "discord", label: "Discord", hint: "Bot token required" },
];

interface Draft {
  name: string;
  id: string;
  idTouched: boolean;
  template: string;
  model: string;
  baseUrl: string;
  apiKeyEnv: string;
  providerToken: string;
  channel: ChannelType;
  channelTouched: boolean;
  token: string;
  shares: ShareRef[];
  memLimit: string;
  allowLan: boolean;
  notes: string;
}

const DEFAULT_DRAFT: Draft = {
  name: "",
  id: "",
  idTouched: false,
  template: "",
  model: "",
  baseUrl: "https://opencode.ai/inference/go/openai/v1",
  apiKeyEnv: "HERMES_GATE_PROFILE_TOKEN",
  providerToken: "",
  channel: "none",
  channelTouched: false,
  token: "",
  shares: [],
  memLimit: "1536",
  allowLan: false,
  notes: "",
};

const FIELD_IDS: Record<string, string> = {
  name: "name",
  id: "id",
  template: "template-group",
  model: "model",
  baseUrl: "base-url",
  apiKeyEnv: "api-key-env",
  memLimit: "mem-limit",
  token: "channel-token",
};

function templateChannel(value: string | undefined): ChannelType | null {
  return value === "telegram" || value === "discord" || value === "none" ? value : null;
}

function slugify(value: string): string {
  return value
    .normalize("NFD")
    .replace(/[\u0300-\u036f]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32)
    .replace(/-+$/g, "");
}

function validate(draft: Draft, step: number): Record<string, string> {
  const errors: Record<string, string> = {};

  if (step === 0) {
    if (!draft.name.trim()) errors.name = "Enter a name.";
    else if (draft.name.trim().length > 48) errors.name = "Use 48 characters or fewer.";
    if (!draft.id.trim()) errors.id = "Enter an id.";
    else if (!isValidSlug(draft.id.trim())) errors.id = "Use 2-32 characters: lowercase letters, digits, hyphens.";
    else if (draft.id.trim() === "new") errors.id = "This id is reserved.";
  }

  if (step === 1 && !draft.template) errors.template = "Select a template.";

  if (step === 2) {
    if (!draft.model.trim()) errors.model = "Enter a model.";
    if (!draft.baseUrl.trim()) errors.baseUrl = "Enter a base URL.";
    else if (!isHttpUrl(draft.baseUrl.trim())) errors.baseUrl = "Use an absolute http or https URL.";
    if (!draft.apiKeyEnv.trim()) errors.apiKeyEnv = "Enter an environment variable name.";
    else if (!/^[A-Z_][A-Z0-9_]*$/.test(draft.apiKeyEnv.trim()))
      errors.apiKeyEnv = "Use uppercase letters, digits and underscores.";
    const memory = Number(draft.memLimit);
    if (!Number.isFinite(memory) || memory < 128 || memory > 8192)
      errors.memLimit = "Use 128 to 8192 MB.";
  }

  if (step === 3 && draft.channel !== "none" && !draft.token.trim()) {
    errors.token = "Enter the bot token.";
  }

  return errors;
}

function FieldError({ id, message }: { id: string; message?: string }) {
  if (!message) return null;
  return (
    <p id={id} className="text-xs text-destructive">
      {message}
    </p>
  );
}

function ReviewRow({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="grid gap-1 px-4 py-3 sm:grid-cols-[170px_minmax(0,1fr)] sm:gap-4">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="min-w-0 text-sm">{value}</dd>
    </div>
  );
}

export function NewInstancePage() {
  const route = useRoute();
  const templates = useResource(() => api.listTemplates(), []);
  const shares = useResource(() => api.listShares(), []);

  const [draft, setDraft] = useState<Draft>(DEFAULT_DRAFT);
  const [step, setStep] = useState(0);
  const [attempted, setAttempted] = useState<number | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<ApiError | null>(null);
  const [serverIdError, setServerIdError] = useState<string | null>(null);
  const [cancelOpen, setCancelOpen] = useState(false);
  const presetApplied = useRef(false);
  const leaveAllowed = useRef(false);

  const errors = validate(draft, step);
  const showErrors = attempted === step;

  const dirty = useMemo(() => JSON.stringify(draft) !== JSON.stringify(DEFAULT_DRAFT), [draft]);
  const errorCount = useMemo(
    () => STEPS.reduce((total, _label, index) => total + Object.keys(validate(draft, index)).length, 0),
    [draft],
  );

  useEffect(() => {
    if (presetApplied.current || !templates.data) return;
    const preset = route.query.get("template");
    if (!preset) return;
    const template = templates.data.find((candidate) => candidate.id === preset);
    if (!template) return;
    presetApplied.current = true;
    setDraft((previous) => ({
      ...previous,
      template: template.id,
      model: template.default_model,
      channel: templateChannel(template.channel) ?? previous.channel,
    }));
  }, [route.query, templates.data]);

  // A dirty wizard must not lose six steps to a reload, Back or a nav click.
  useEffect(() => {
    if (!dirty) return;

    const onBeforeUnload = (event: BeforeUnloadEvent) => {
      if (leaveAllowed.current) return;
      event.preventDefault();
      event.returnValue = "";
    };

    setRouteGuard(() => {
      if (leaveAllowed.current) return true;
      const leave = window.confirm("Discard this instance? The form is not saved.");
      if (leave) leaveAllowed.current = true;
      return leave;
    });

    window.addEventListener("beforeunload", onBeforeUnload);
    return () => {
      window.removeEventListener("beforeunload", onBeforeUnload);
      setRouteGuard(null);
    };
  }, [dirty]);

  function leaveWizard(path: string) {
    leaveAllowed.current = true;
    navigate(path);
  }

  function update<K extends keyof Draft>(key: K, value: Draft[K]) {
    setDraft((previous) => ({ ...previous, [key]: value }) as Draft);
  }

  function focusField(key: string) {
    document.getElementById(FIELD_IDS[key] ?? key)?.focus();
  }

  function next() {
    const stepErrors = validate(draft, step);
    const keys = Object.keys(stepErrors);
    if (keys.length > 0) {
      setAttempted(step);
      focusField(keys[0]);
      return;
    }
    setStep((current) => Math.min(current + 1, LAST_STEP));
  }

  function back() {
    setStep((current) => Math.max(current - 1, 0));
  }

  function selectTemplate(id: string) {
    const previousDefault = templates.data?.find((template) => template.id === draft.template)?.default_model;
    const nextTemplate = templates.data?.find((template) => template.id === id);
    const nextChannel = templateChannel(nextTemplate?.channel);
    setDraft((current) => ({
      ...current,
      template: id,
      model:
        !current.model || current.model === previousDefault
          ? (nextTemplate?.default_model ?? current.model)
          : current.model,
      channel: !current.channelTouched && nextChannel ? nextChannel : current.channel,
    }));
  }

  function selectChannel(value: ChannelType) {
    setDraft((current) => ({ ...current, channel: value, channelTouched: true }));
  }

  async function create() {
    for (let index = 0; index < LAST_STEP; index += 1) {
      const stepErrors = validate(draft, index);
      const keys = Object.keys(stepErrors);
      if (keys.length > 0) {
        setStep(index);
        setAttempted(index);
        focusField(keys[0]);
        return;
      }
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
        gate_token: draft.providerToken.trim() || undefined,
        channel: draft.channel === "none" ? { type: "none" } : { type: draft.channel, token: draft.token },
        shares: draft.shares,
        mem_limit_mb: Number(draft.memLimit),
        allow_lan: draft.allowLan,
        notes: draft.notes.trim(),
      });
      toast.success(`Instance ${detail.id} created.`);
      leaveWizard(`/instances/${detail.id}`);
    } catch (cause) {
      const error = toApiError(cause);
      if (error.status === 409 && error.code !== "egress_not_enforced") {
        setServerIdError(error.message);
        setStep(0);
        setAttempted(0);
        window.requestAnimationFrame(() => document.getElementById("id")?.focus());
      } else {
        setSubmitError(error);
        if (error.code === "egress_not_enforced") {
          // The host prerequisite lives on the network field: put the user there.
          setStep(2);
          setAttempted(null);
          window.requestAnimationFrame(() => {
            document.getElementById("wizard-allow-lan")?.focus({ preventScroll: true });
            document.querySelector("[data-egress-alert]")?.scrollIntoView({ block: "center" });
          });
        }
      }
    } finally {
      setSubmitting(false);
    }
  }

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (step === LAST_STEP) void create();
    else next();
  }

  function requestCancel() {
    if (dirty) setCancelOpen(true);
    else navigate("/instances");
  }

  const templateList = templates.data ?? [];
  const shareList = shares.data ?? [];
  const egressError = submitError?.code === "egress_not_enforced" ? submitError : null;

  return (
    <>
      <PageHeader
        title="New instance"
        description={`Step ${step + 1} of ${STEPS.length} · ${STEPS[step]}`}
      />

      <div className="grid gap-8 lg:grid-cols-[200px_minmax(0,1fr)]">
        <nav aria-label="Progress" className="hidden lg:block">
          <ol className="space-y-1">
            {STEPS.map((label, index) => {
              const current = index === step;
              const done = index < step;
              return (
                <li key={label}>
                  <button
                    type="button"
                    onClick={() => {
                      if (index < step) setStep(index);
                    }}
                    disabled={index > step}
                    aria-current={current ? "step" : undefined}
                    className={cn(
                      "flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm transition-colors",
                      current && "bg-accent font-medium text-accent-foreground",
                      !current && done && "text-muted-foreground hover:bg-accent/50 hover:text-foreground",
                      index > step && "cursor-not-allowed text-muted-foreground/50",
                    )}
                  >
                    <span className="w-5 font-mono text-xs">{String(index + 1).padStart(2, "0")}</span>
                    <span className="flex-1">{label}</span>
                    {done ? <Check aria-hidden className="size-3.5" /> : null}
                  </button>
                </li>
              );
            })}
          </ol>
          {errorCount > 0 && attempted !== null ? (
            <p className="mt-3 px-2 text-xs text-muted-foreground">
              {errorCount} {errorCount === 1 ? "field needs" : "fields need"} attention.
            </p>
          ) : null}
        </nav>

        <div className="min-w-0 max-w-2xl space-y-6">
          {submitError && !(egressError && step === 2) ? <ErrorAlert error={submitError} /> : null}

          <form onSubmit={handleSubmit} noValidate className="space-y-6">
            {step === 0 ? (
              <div className="space-y-5">
                <div className="space-y-2">
                  <Label htmlFor="name">Name</Label>
                  <Input
                    id="name"
                    value={draft.name}
                    autoComplete="off"
                    placeholder="Alice"
                    aria-invalid={showErrors && !!errors.name}
                    aria-describedby={showErrors && errors.name ? "name-error" : undefined}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        name: event.target.value,
                        id: current.idTouched ? current.id : slugify(event.target.value),
                      }))
                    }
                  />
                  <FieldError id="name-error" message={showErrors ? errors.name : undefined} />
                </div>

                <div className="space-y-2">
                  <Label htmlFor="id">Id</Label>
                  <Input
                    id="id"
                    value={draft.id}
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono"
                    placeholder="alice"
                    aria-invalid={(showErrors && !!errors.id) || !!serverIdError}
                    aria-describedby={
                      (showErrors && errors.id) || serverIdError ? "id-error" : "id-help"
                    }
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        id: event.target.value.toLowerCase(),
                        idTouched: true,
                      }))
                    }
                  />
                  <p id="id-help" className="text-xs text-muted-foreground">
                    Names the compose project{" "}
                    <span className="font-mono">fleett-{draft.id || "…"}</span> and the directory{" "}
                    <span className="font-mono">instances/{draft.id || "…"}</span>.
                  </p>
                  <FieldError id="id-error" message={serverIdError ?? (showErrors ? errors.id : undefined)} />
                </div>

                <div className="space-y-2">
                  <Label htmlFor="notes">
                    Notes <span className="font-normal text-muted-foreground">(optional)</span>
                  </Label>
                  <Textarea
                    id="notes"
                    rows={3}
                    value={draft.notes}
                    placeholder="Who is this agent for, what it may touch."
                    onChange={(event) => update("notes", event.target.value)}
                  />
                </div>
              </div>
            ) : null}

            {step === 1 ? (
              <div className="space-y-3">
                {templates.error ? <ErrorAlert error={templates.error} onRetry={templates.reload} /> : null}
                {templates.loading && !templates.data ? (
                  <div className="space-y-3">
                    {Array.from({ length: 3 }).map((_, index) => (
                      <Skeleton key={index} className="h-24 w-full rounded-md" />
                    ))}
                  </div>
                ) : (
                  <RadioGroup
                    id="template-group"
                    tabIndex={-1}
                    aria-label="Template"
                    aria-invalid={showErrors && !!errors.template}
                    value={draft.template}
                    onValueChange={selectTemplate}
                    className="gap-3"
                  >
                    {templateList.map((template) => {
                      const selected = draft.template === template.id;
                      return (
                        <div
                          key={template.id}
                          onClick={() => selectTemplate(template.id)}
                          className={cn(
                            "flex cursor-pointer items-start gap-3 rounded-md border p-4 transition-colors",
                            selected ? "border-primary bg-accent/40" : "hover:bg-muted/50",
                          )}
                        >
                          <RadioGroupItem
                            value={template.id}
                            id={`tpl-${template.id}`}
                            aria-labelledby={`tpl-${template.id}-label`}
                            className="mt-0.5"
                          />
                          <div className="min-w-0 flex-1 space-y-1">
                            <div className="flex flex-wrap items-center gap-2">
                              <span id={`tpl-${template.id}-label`} className="text-sm font-medium">
                                {template.name}
                              </span>
                              <span className="font-mono text-xs text-muted-foreground">{template.id}</span>
                            </div>
                            <p className="text-sm text-muted-foreground">{template.description}</p>
                            <p className="font-mono text-xs text-muted-foreground">
                              {template.default_model} · channel {template.channel}
                            </p>
                          </div>
                        </div>
                      );
                    })}
                  </RadioGroup>
                )}
                <FieldError id="template-error" message={showErrors ? errors.template : undefined} />
                {!templates.loading && !templates.error && templateList.length === 0 ? (
                  <p className="text-sm text-muted-foreground">
                    No templates found. Add <span className="font-mono">templates/&lt;id&gt;/config.yaml</span> on
                    the host, then reload.
                  </p>
                ) : null}
              </div>
            ) : null}

            {step === 2 ? (
              <div className="space-y-5">
                <div className="space-y-2">
                  <Label htmlFor="model">Model</Label>
                  <Input
                    id="model"
                    value={draft.model}
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono"
                    placeholder="deepseek-v4.1-flash"
                    aria-invalid={showErrors && !!errors.model}
                    aria-describedby={showErrors && errors.model ? "model-error" : undefined}
                    onChange={(event) => update("model", event.target.value)}
                  />
                  <FieldError id="model-error" message={showErrors ? errors.model : undefined} />
                </div>

                <div className="space-y-2">
                  <Label htmlFor="base-url">Base URL</Label>
                  <Input
                    id="base-url"
                    value={draft.baseUrl}
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono"
                    aria-invalid={showErrors && !!errors.baseUrl}
                    aria-describedby={showErrors && errors.baseUrl ? "base-url-error" : "base-url-help"}
                    onChange={(event) => update("baseUrl", event.target.value)}
                  />
                  <p id="base-url-help" className="text-xs text-muted-foreground">
                    OpenAI-compatible endpoint used by the agent.
                  </p>
                  <FieldError id="base-url-error" message={showErrors ? errors.baseUrl : undefined} />
                </div>

                <div className="space-y-2">
                  <Label htmlFor="api-key-env">API key env</Label>
                  <Input
                    id="api-key-env"
                    value={draft.apiKeyEnv}
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono"
                    aria-invalid={showErrors && !!errors.apiKeyEnv}
                    aria-describedby={showErrors && errors.apiKeyEnv ? "api-key-env-error" : "api-key-env-help"}
                    onChange={(event) => update("apiKeyEnv", event.target.value)}
                  />
                  <p id="api-key-env-help" className="text-xs text-muted-foreground">
                    Environment variable that holds the provider key.
                  </p>
                  <FieldError id="api-key-env-error" message={showErrors ? errors.apiKeyEnv : undefined} />
                </div>

                <div className="space-y-2">
                  <Label htmlFor="provider-token">
                    Provider token{" "}
                    <span className="font-normal text-muted-foreground">(optional, recommended)</span>
                  </Label>
                  <Input
                    id="provider-token"
                    type="password"
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono"
                    value={draft.providerToken}
                    aria-describedby="provider-token-help"
                    onChange={(event) => update("providerToken", event.target.value)}
                  />
                  <p id="provider-token-help" className="text-xs text-muted-foreground">
                    Stored in <span className="font-mono">secrets.env</span> (0600) and never returned by the
                    API. Without it the agent cannot call the model.
                  </p>
                </div>

                <div className="space-y-2">
                  <Label htmlFor="mem-limit">Memory limit (MB)</Label>
                  <Input
                    id="mem-limit"
                    type="number"
                    min={128}
                    max={8192}
                    step={128}
                    value={draft.memLimit}
                    className="w-40 font-mono"
                    aria-invalid={showErrors && !!errors.memLimit}
                    aria-describedby={showErrors && errors.memLimit ? "mem-limit-error" : undefined}
                    onChange={(event) => update("memLimit", event.target.value)}
                  />
                  <FieldError id="mem-limit-error" message={showErrors ? errors.memLimit : undefined} />
                </div>

                <div className="space-y-3 rounded-md border p-4">
                  <p className="text-sm font-medium">Network</p>
                  <div className="flex items-start gap-3">
                    <Checkbox
                      id="wizard-allow-lan"
                      className="mt-0.5"
                      checked={draft.allowLan}
                      aria-describedby="wizard-allow-lan-help"
                      onCheckedChange={(value) => {
                        update("allowLan", value === true);
                        if (egressError) setSubmitError(null);
                      }}
                    />
                    <div className="min-w-0 space-y-1">
                      <Label htmlFor="wizard-allow-lan" className="cursor-pointer">
                        Allow LAN access
                      </Label>
                      <p id="wizard-allow-lan-help" className="text-xs text-muted-foreground">
                        The agent can reach 10/8, 172.16/12 and 192.168/16, including Proxmox, OPNsense
                        and Infisical. Only enable for agents you trust.
                      </p>
                      <p className="text-xs text-muted-foreground">
                        {draft.allowLan ? (
                          <>
                            Joins <span className="font-mono">fleett-lan</span> on top of the restricted{" "}
                            <span className="font-mono">fleett</span> network.
                          </>
                        ) : (
                          <>
                            Stays on the restricted <span className="font-mono">fleett</span> network. No
                            route to the LAN.
                          </>
                        )}
                      </p>
                    </div>
                  </div>
                  {egressError ? (
                    <div data-egress-alert>
                      <ErrorAlert error={egressError} />
                    </div>
                  ) : null}
                </div>
              </div>
            ) : null}

            {step === 3 ? (
              <div className="space-y-5">
                <RadioGroup
                  aria-label="Channel"
                  value={draft.channel}
                  onValueChange={(value) => selectChannel(value as ChannelType)}
                  className="grid gap-3 sm:grid-cols-3"
                >
                  {CHANNELS.map((channel) => {
                    const selected = draft.channel === channel.value;
                    return (
                      <div
                        key={channel.value}
                        onClick={() => selectChannel(channel.value)}
                        className={cn(
                          "cursor-pointer rounded-md border p-3 transition-colors",
                          selected ? "border-primary bg-accent/40" : "hover:bg-muted/50",
                        )}
                      >
                        <div className="flex items-center gap-2">
                          <RadioGroupItem value={channel.value} id={`channel-${channel.value}`} />
                          <Label htmlFor={`channel-${channel.value}`} className="cursor-pointer">
                            {channel.label}
                          </Label>
                        </div>
                        <p className="mt-1 pl-6 text-xs text-muted-foreground">{channel.hint}</p>
                      </div>
                    );
                  })}
                </RadioGroup>

                {draft.channel !== "none" ? (
                  <div className="space-y-2">
                    <Label htmlFor="channel-token">Bot token</Label>
                    <Input
                      id="channel-token"
                      type="password"
                      autoComplete="off"
                      spellCheck={false}
                      className="font-mono"
                      aria-invalid={showErrors && !!errors.token}
                      aria-describedby={showErrors && errors.token ? "channel-token-error" : "channel-token-help"}
                      onChange={(event) => update("token", event.target.value)}
                    />
                    <p id="channel-token-help" className="text-xs text-muted-foreground">
                      Written to <span className="font-mono">secrets.env</span> with mode 0600. The API never
                      returns it.
                    </p>
                    <FieldError id="channel-token-error" message={showErrors ? errors.token : undefined} />
                  </div>
                ) : null}
              </div>
            ) : null}

            {step === 4 ? (
              <SharePicker
                shares={shareList}
                value={draft.shares}
                onChange={(next) => update("shares", next)}
                loading={shares.loading && !shares.data}
                error={shares.error}
                onRetry={shares.reload}
              />
            ) : null}

            {step === 5 ? (
              <div className="space-y-4">
                <dl className="divide-y rounded-md border">
                  <ReviewRow label="Name" value={draft.name || "—"} />
                  <ReviewRow label="Id" value={<span className="font-mono">{draft.id || "—"}</span>} />
                  <ReviewRow label="Template" value={<span className="font-mono">{draft.template || "—"}</span>} />
                  <ReviewRow label="Model" value={<span className="font-mono break-all">{draft.model}</span>} />
                  <ReviewRow
                    label="Base URL"
                    value={<span className="font-mono text-xs break-all">{draft.baseUrl}</span>}
                  />
                  <ReviewRow label="API key env" value={<span className="font-mono">{draft.apiKeyEnv}</span>} />
                  <ReviewRow
                    label="Provider token"
                    value={draft.providerToken.trim() ? "•••••••• set" : "Not set"}
                  />
                  <ReviewRow
                    label="Memory limit"
                    value={<span className="font-mono">{draft.memLimit}&nbsp;MB</span>}
                  />
                  <ReviewRow
                    label="LAN access"
                    value={
                      draft.allowLan ? (
                        <span>
                          Enabled · <span className="font-mono text-xs">fleett-lan</span>
                        </span>
                      ) : (
                        <span>
                          Restricted · <span className="font-mono text-xs">fleett</span>
                        </span>
                      )
                    }
                  />
                  <ReviewRow label="Channel" value={draft.channel} />
                  {draft.channel !== "none" ? <ReviewRow label="Token" value="•••••••• set" /> : null}
                  <ReviewRow
                    label="Shares"
                    value={
                      draft.shares.length ? (
                        <span className="flex flex-wrap gap-1">
                          {draft.shares.map((share) => (
                            <Badge key={share.name} variant="outline" className="gap-1 font-mono text-xs font-normal">
                              {share.name}
                              <span className="text-muted-foreground">{share.mode}</span>
                            </Badge>
                          ))}
                        </span>
                      ) : (
                        "None"
                      )
                    }
                  />
                  <ReviewRow label="Notes" value={draft.notes || "—"} />
                </dl>
                <p className="text-xs text-muted-foreground">
                  Creating writes the compose project and runs{" "}
                  <span className="font-mono">docker compose up -d</span>. The container starts immediately.
                </p>
              </div>
            ) : null}

            <div className="flex flex-wrap items-center justify-between gap-3 border-t pt-4">
              <Button type="button" variant="ghost" onClick={requestCancel}>
                Cancel
              </Button>
              <div className="flex items-center gap-2">
                <Button type="button" variant="outline" onClick={back} disabled={step === 0 || submitting}>
                  Back
                </Button>
                <Button type="submit" disabled={submitting}>
                  {step === LAST_STEP ? (
                    submitting ? (
                      <Loader2 aria-hidden className="animate-spin" />
                    ) : (
                      <Plus aria-hidden />
                    )
                  ) : null}
                  {step === LAST_STEP ? "Create instance" : "Continue"}
                </Button>
              </div>
            </div>
          </form>
        </div>
      </div>

      <Dialog open={cancelOpen} onOpenChange={setCancelOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Discard this instance?</DialogTitle>
            <DialogDescription>The form is not saved. Nothing is written to the host.</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCancelOpen(false)}>
              Keep editing
            </Button>
            <Button variant="destructive" onClick={() => leaveWizard("/instances")}>
              Discard
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
