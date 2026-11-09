import { useEffect, useRef, useState, type FormEvent } from "react";
import { CopyPlus, Loader2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { CloneInstanceInput, CloneTokenDetails, InstanceDetail } from "@/api/types";
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
import { ErrorAlert } from "@/components/error-alert";
import { PersonaPreviewPanel } from "@/components/persona-preview";
import { PersonaSelect } from "@/components/persona-select";
import { PersonaVariableInputs } from "@/components/persona-vars";
import { ProviderSelect } from "@/components/provider-select";
import { compactVariables } from "@/lib/personas";
import { isValidSlug } from "@/lib/validation";
import { useResource } from "@/lib/use-resource";

const GATE_ENV = "HERMES_GATE_PROFILE_TOKEN";
const ID_MAX = 32;

/**
 * `POST /api/instances/{id}/clone` never copies secrets: every token the source
 * holds needs a replacement in the request. These helpers name the keys the
 * source is holding, so the dialog can ask for exactly those.
 */
function gateKeys(source: InstanceDetail): string[] {
  const keys = [GATE_ENV];
  if (source.api_key_env && source.api_key_env !== GATE_ENV) keys.push(source.api_key_env);
  return keys;
}

function channelKey(source: InstanceDetail): string | null {
  if (source.channel === "telegram") return "TELEGRAM_BOT_TOKEN";
  if (source.channel === "discord") return "DISCORD_BOT_TOKEN";
  return null;
}

function holdsEnv(source: InstanceDetail, key: string): boolean {
  return source.env.some((entry) => entry.key === key);
}

/** Same defaults as the controller: `<id>-copy`, `<name> copy`, both within limits. */
function defaultCloneId(id: string): string {
  const suffix = "-copy";
  return `${id.slice(0, ID_MAX - suffix.length).replace(/-+$/, "")}${suffix}`;
}

function defaultCloneName(name: string): string {
  const suffix = " copy";
  const base = name.trim().slice(0, 128 - suffix.length);
  return base ? `${base}${suffix}` : "copy";
}

/** `details.missing` of a `400 clone_token_required`. */
function cloneMissing(error: ApiError): string[] {
  const details = (error.details ?? null) as Partial<CloneTokenDetails> | null;
  const missing = details?.missing;
  if (!Array.isArray(missing)) return [];
  return missing.filter((key): key is string => typeof key === "string");
}

const FIELD_IDS: Record<string, string> = {
  gate_token: "duplicate-gate-token",
  channel_token: "duplicate-channel-token",
  id: "duplicate-id",
  name: "duplicate-name",
  provider: "duplicate-provider",
};

export function DuplicateInstanceDialog({
  source,
  open,
  onOpenChange,
  onCloned,
}: {
  source: InstanceDetail;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCloned: (clone: InstanceDetail) => void;
}) {
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const [model, setModel] = useState("");
  const [providerId, setProviderId] = useState("");
  const [personaId, setPersonaId] = useState("");
  const [personaVars, setPersonaVars] = useState<Record<string, string>>({});
  const [gateToken, setGateToken] = useState("");
  const [channelToken, setChannelToken] = useState("");
  const [copyData, setCopyData] = useState(true);
  const [inheritShares, setInheritShares] = useState(true);
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [missing, setMissing] = useState<string[]>([]);
  const [idConflict, setIdConflict] = useState<string | null>(null);
  const providers = useResource(() => api.listProviders(), []);
  const personas = useResource(() => api.listPersonas(), []);

  // The page refreshes the source while the dialog is open; only the props
  // captured when it opened may seed the draft.
  const sourceRef = useRef(source);
  sourceRef.current = source;

  useEffect(() => {
    if (!open) return;
    const current = sourceRef.current;
    setId(defaultCloneId(current.id));
    setName(defaultCloneName(current.name));
    setModel("");
    setProviderId(current.provider ?? "");
    // A diverged SOUL.md is hand-written: default to keeping it rather than
    // silently replacing it with the persona render.
    setPersonaId(current.persona && current.persona_state !== "diverged" ? current.persona : "");
    setPersonaVars(
      current.persona && current.persona_state !== "diverged"
        ? { ...(current.persona_vars ?? {}) }
        : {},
    );
    setGateToken("");
    setChannelToken("");
    setCopyData(true);
    setInheritShares(true);
    setAttempted(false);
    setPending(false);
    setError(null);
    setMissing([]);
    setIdConflict(null);
  }, [open]);

  const providerKeys = gateKeys(source);
  const botKey = channelKey(source);
  const gateEnvPresent = providerKeys.some((key) => holdsEnv(source, key));
  const botRequired = botKey !== null && holdsEnv(source, botKey);

  const providerSelected = providerId !== "";
  const selectedProvider = (providers.data ?? []).find((provider) => provider.id === providerId) ?? null;
  const providerLabel = selectedProvider?.name ?? providerId;
  const personaList = personas.data ?? [];
  const selectedPersona = personaList.find((persona) => persona.id === personaId) ?? null;

  const missingProvider = missing.filter((key) => key !== botKey);
  const missingChannel = botKey ? missing.filter((key) => key === botKey) : [];
  // A refusal names keys the read missed, so it also makes them required. A
  // source on a provider holds no gate secret of its own, yet a clone detached
  // from it still needs one: the provider was supplying that credential.
  const providerNeeded =
    !providerSelected && (gateEnvPresent || source.provider !== undefined || missingProvider.length > 0);
  const botNeeded = botRequired || missingChannel.length > 0;
  // A source holding a bot token always gets the field; so does a refusal that
  // names it, should the read have missed the key.
  const showBotField = botKey !== null && botNeeded;

  const errors: Record<string, string> = {};
  const trimmedId = id.trim();
  if (!trimmedId) errors.id = "Enter an id.";
  else if (!isValidSlug(trimmedId)) errors.id = "Use 2-32 characters: lowercase letters, digits, hyphens.";
  else if (trimmedId === source.id) errors.id = "Use an id other than the source id.";
  if (!name.trim()) errors.name = "Enter a name.";
  if (providerNeeded && !gateToken.trim()) {
    errors.gate_token = "Enter a replacement provider token.";
  }
  if (botNeeded && !channelToken.trim()) {
    errors.channel_token = "Enter a replacement bot token.";
  }

  const providerError = !providerSelected && !gateToken.trim()
    ? missingProvider.length > 0
      ? `The source holds ${missingProvider.join(", ")}. Clones never inherit secrets; enter a replacement.`
      : attempted
        ? errors.gate_token
        : undefined
    : undefined;
  const channelError = !channelToken.trim()
    ? missingChannel.length > 0
      ? `The source holds ${missingChannel.join(", ")}. Clones never inherit secrets; enter a replacement.`
      : attempted
        ? errors.channel_token
        : undefined
    : undefined;
  const idError = idConflict ?? (attempted ? errors.id : undefined);
  const nameError = attempted ? errors.name : undefined;

  function focusField(key: string) {
    document.getElementById(FIELD_IDS[key] ?? key)?.focus();
  }

  function selectProvider(next: string) {
    setProviderId(next);
    if (next) {
      // A provider supplies the gate credential; drop any typed token and the
      // refusal that named it.
      setGateToken("");
      setMissing((previous) => previous.filter((key) => key === botKey));
      if (error?.code === "clone_token_required") setError(null);
    }
  }

  function selectPersona(next: string) {
    const persona = personaList.find((candidate) => candidate.id === next) ?? null;
    setPersonaId(next);
    setPersonaVars((current) => {
      const kept: Record<string, string> = {};
      for (const variable of persona?.variables ?? []) {
        if (current[variable.key] !== undefined) kept[variable.key] = current[variable.key];
      }
      return kept;
    });
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setAttempted(true);

    const first = ["id", "name", "gate_token", "channel_token"].find((key) => errors[key]);
    if (first) {
      focusField(first);
      return;
    }

    const input: CloneInstanceInput = {
      new_id: trimmedId,
      name: name.trim(),
      model: model.trim() || undefined,
      provider: providerId,
      gate_token: providerSelected ? undefined : gateToken.trim() || undefined,
      channel_token: channelToken.trim() || undefined,
      shares_inherit: inheritShares,
      copy_data: copyData,
    };

    setPending(true);
    setError(null);
    try {
      const clone = await api.cloneInstance(source.id, input);
      // The clone endpoint has no persona field: assigning one is a normal PATCH
      // on the new instance, which renders SOUL.md once and restarts it.
      let created = clone;
      if (personaId) {
        try {
          created = await api.patchInstance(clone.id, {
            persona: personaId,
            persona_vars: compactVariables(personaVars),
          });
        } catch (cause) {
          toast.error(`Clone created, but the persona was not applied: ${toApiError(cause).message}`);
        }
      }
      onOpenChange(false);
      onCloned(created);
    } catch (cause) {
      const failure = toApiError(cause);
      if (failure.status === 409 && failure.code === "instance_exists") {
        // The only id-specific refusal: that slug is taken.
        setIdConflict(failure.message);
        window.requestAnimationFrame(() => focusField("id"));
      } else if (failure.status === 400 && failure.code === "clone_token_required") {
        // Keep every field as typed; mark the keys the source is holding.
        const keys = cloneMissing(failure);
        setMissing(keys);
        // Focus the first field that needs a value, in reading order.
        const target = keys.some((key) => key !== botKey)
          ? providerSelected
            ? "provider"
            : "gate_token"
          : "channel_token";
        window.requestAnimationFrame(() => focusField(target));
      } else {
        setError(failure);
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh_-_2rem)] grid-rows-[auto_minmax(0,1fr)]">
        <DialogHeader>
          <DialogTitle>Duplicate {source.id}</DialogTitle>
          <DialogDescription>
            Creates an independent instance with its own compose project, container and data
            directory. Secrets are never copied.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={onSubmit} noValidate className="flex min-h-0 flex-col gap-4">
          <div className="-mr-2 min-h-0 flex-1 space-y-4 overflow-y-auto pr-2">
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="duplicate-id">New id</Label>
                <Input
                  id="duplicate-id"
                  value={id}
                  autoComplete="off"
                  spellCheck={false}
                  className="font-mono"
                  aria-invalid={!!idError}
                  aria-describedby={idError ? "duplicate-id-error" : "duplicate-id-help"}
                  onChange={(event) => {
                    setId(event.target.value.toLowerCase());
                    setIdConflict(null);
                  }}
                />
                <p id="duplicate-id-help" className="text-xs text-muted-foreground">
                  Compose project
                  <span className="block font-mono break-all">fleett-{trimmedId || "…"}</span>
                </p>
                {idError ? (
                  <p id="duplicate-id-error" className="text-xs text-destructive">
                    {idError}
                  </p>
                ) : null}
              </div>

              <div className="space-y-2">
                <Label htmlFor="duplicate-name">Name</Label>
                <Input
                  id="duplicate-name"
                  value={name}
                  autoComplete="off"
                  aria-invalid={!!nameError}
                  aria-describedby={nameError ? "duplicate-name-error" : undefined}
                  onChange={(event) => setName(event.target.value)}
                />
                {nameError ? (
                  <p id="duplicate-name-error" className="text-xs text-destructive">
                    {nameError}
                  </p>
                ) : null}
              </div>
            </div>

            <div className="space-y-2">
              <Label htmlFor="duplicate-model">
                Model <span className="font-normal text-muted-foreground">(optional)</span>
              </Label>
              <Input
                id="duplicate-model"
                value={model}
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
                placeholder={source.model}
                aria-describedby="duplicate-model-help"
                onChange={(event) => setModel(event.target.value)}
              />
              <p id="duplicate-model-help" className="text-xs text-muted-foreground">
                Leave empty to keep <span className="font-mono">{source.model}</span>.
              </p>
            </div>

            <div className="space-y-4 rounded-md border p-4">
              <div className="space-y-1">
                <p className="text-sm font-medium">Credential</p>
                <p className="text-xs text-muted-foreground">
                  A provider is a reference, not a secret: the clone can keep the source provider without a
                  token. The chat bot token stays per instance.
                </p>
              </div>

              <div className="space-y-2">
                <Label htmlFor="duplicate-provider">Provider</Label>
                <ProviderSelect
                  id="duplicate-provider"
                  value={providerId}
                  onChange={selectProvider}
                  providers={providers.data ?? []}
                  loading={providers.loading && !providers.data}
                  describedBy="duplicate-provider-help"
                />
                <p id="duplicate-provider-help" className="text-xs text-muted-foreground">
                  {source.provider ? (
                    <>
                      The source uses a provider and the clone inherits it by default. Choosing{" "}
                      <span className="text-foreground">None</span> requires a replacement token.
                    </>
                  ) : (
                    <>
                      The source has no provider. Pick one to share a credential, or leave{" "}
                      <span className="text-foreground">None</span> to give the clone its own token.
                    </>
                  )}
                </p>
                {providers.error ? (
                  <p className="text-xs text-destructive">
                    Providers could not be loaded: {providers.error.message}{" "}
                    <button
                      type="button"
                      className="underline underline-offset-4"
                      onClick={providers.reload}
                    >
                      Retry
                    </button>
                  </p>
                ) : null}
              </div>

              {providerSelected ? (
                <div className="space-y-1 rounded-md border bg-muted/20 px-3 py-2.5">
                  <p className="text-sm">
                    Credential supplied by provider{" "}
                    <span className="font-mono">{providerLabel}</span>.
                  </p>
                  <p className="text-xs text-muted-foreground">
                    No token is copied or required. Rotating that secret later restarts every attached
                    instance, this clone included.
                  </p>
                </div>
              ) : (
                <div className="space-y-2">
                  <Label htmlFor="duplicate-gate-token">
                    Provider token{" "}
                    <span className="font-normal text-muted-foreground">
                      {providerNeeded ? "(required)" : "(optional)"}
                    </span>
                  </Label>
                  <Input
                    id="duplicate-gate-token"
                    type="password"
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono"
                    value={gateToken}
                    aria-invalid={!!providerError}
                    aria-describedby={
                      providerError
                        ? "duplicate-gate-token-help duplicate-gate-token-error"
                        : "duplicate-gate-token-help"
                    }
                    onChange={(event) => {
                      setGateToken(event.target.value);
                      if (missingProvider.length > 0) {
                        setMissing((previous) => previous.filter((key) => key === botKey));
                      }
                    }}
                  />
                  {providerError ? (
                    <p id="duplicate-gate-token-error" className="text-xs text-destructive">
                      {providerError}
                    </p>
                  ) : null}
                  <p id="duplicate-gate-token-help" className="text-xs text-muted-foreground">
                    {providerError ? null : gateEnvPresent ? (
                      <>
                        The source holds{" "}
                        <span className="font-mono break-all">
                          {providerKeys.filter((key) => holdsEnv(source, key)).join(", ")}
                        </span>
                        .{" "}
                      </>
                    ) : source.provider ? (
                      <>The source draws its key from a provider; detaching needs a replacement token. </>
                    ) : missingProvider.length > 0 ? (
                      <>
                        The source holds{" "}
                        <span className="font-mono break-all">{missingProvider.join(", ")}</span>.{" "}
                      </>
                    ) : null}
                    Encrypted at rest and never returned by the API.
                  </p>
                </div>
              )}

              {showBotField && botKey ? (
                <div className="space-y-2">
                  <Label htmlFor="duplicate-channel-token">
                    Bot token <span className="font-normal text-muted-foreground">(required)</span>
                  </Label>
                  <Input
                    id="duplicate-channel-token"
                    type="password"
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono"
                    value={channelToken}
                    aria-invalid={!!channelError}
                    aria-describedby={
                      channelError
                        ? "duplicate-channel-token-help duplicate-channel-token-error"
                        : "duplicate-channel-token-help"
                    }
                    onChange={(event) => {
                      setChannelToken(event.target.value);
                      if (missingChannel.length > 0) {
                        setMissing((previous) => previous.filter((key) => key !== botKey));
                      }
                    }}
                  />
                  {channelError ? (
                    <p id="duplicate-channel-token-error" className="text-xs text-destructive">
                      {channelError}
                    </p>
                  ) : null}
                  <p id="duplicate-channel-token-help" className="text-xs text-muted-foreground">
                    Replaces <span className="font-mono">{botKey}</span> for the {source.channel} bot. A
                    bot can only serve one instance at a time.
                  </p>
                </div>
              ) : null}
            </div>

            <div className="space-y-4 rounded-md border p-4">
              <div className="space-y-1">
                <p className="text-sm font-medium">
                  Persona <span className="font-normal text-muted-foreground">(optional)</span>
                </p>
                <p className="text-xs text-muted-foreground">
                  Replaces the copied <span className="font-mono">SOUL.md</span> with a rendered
                  persona. The clone keeps its own variable values, so the same persona can read
                  differently here.
                </p>
              </div>

              <div className="space-y-2">
                <PersonaSelect
                  id="duplicate-persona"
                  value={personaId}
                  onChange={selectPersona}
                  personas={personaList}
                  loading={personas.loading && !personas.data}
                  describedBy="duplicate-persona-help"
                />
                <p id="duplicate-persona-help" className="text-xs text-muted-foreground">
                  {source.persona
                    ? source.persona_state === "diverged"
                      ? "The source SOUL.md was edited by hand, so None is selected by default: the copy's SOUL.md is not replaced. Picking a persona overwrites it with the rendered template."
                      : personaId === source.persona
                        ? "Inherited from the source. Choose None to keep the copied SOUL.md instead."
                        : "The source persona is replaced; the rendered template is written after the copy."
                    : "The source has no persona. Pick one to render its SOUL.md into the clone."}
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
                  <PersonaVariableInputs
                    variables={selectedPersona.variables}
                    values={personaVars}
                    onChange={setPersonaVars}
                    idPrefix="duplicate-persona-var"
                    disabled={pending}
                  />
                  <PersonaPreviewPanel
                    personaId={selectedPersona.id}
                    variables={personaVars}
                    description="Rendered from the saved persona; then stored on the clone."
                    maxHeight="26vh"
                  />
                </>
              ) : (
                <p className="rounded-md border px-3 py-2.5 text-xs text-muted-foreground">
                  No persona: the clone keeps{" "}
                  <span className="font-mono">data/SOUL.md</span> from the copy, or the template
                  default when data is not copied.
                </p>
              )}
            </div>

            <div className="space-y-4 rounded-md border p-4">
              <div className="flex items-start gap-3">
                <Checkbox
                  id="duplicate-copy-data"
                  className="mt-0.5"
                  checked={copyData}
                  disabled={pending}
                  aria-describedby="duplicate-copy-data-help"
                  onCheckedChange={(value) => setCopyData(value === true)}
                />
                <div className="min-w-0 space-y-1">
                  <Label htmlFor="duplicate-copy-data" className="cursor-pointer">
                    Copy data
                  </Label>
                  <p id="duplicate-copy-data-help" className="text-xs text-muted-foreground">
                    Copies <span className="font-mono">data/</span> so the clone keeps the same
                    memory and state. Turn off for an empty profile.
                  </p>
                </div>
              </div>

              <div className="flex items-start gap-3">
                <Checkbox
                  id="duplicate-inherit-shares"
                  className="mt-0.5"
                  checked={inheritShares}
                  disabled={pending}
                  aria-describedby="duplicate-inherit-shares-help"
                  onCheckedChange={(value) => setInheritShares(value === true)}
                />
                <div className="min-w-0 space-y-1">
                  <Label htmlFor="duplicate-inherit-shares" className="cursor-pointer">
                    Inherit shares
                  </Label>
                  <p id="duplicate-inherit-shares-help" className="text-xs text-muted-foreground">
                    Attaches the shares the source has today, with the same modes. Shares removed
                    on this host are skipped.
                  </p>
                </div>
              </div>
            </div>
          </div>

          <div className="space-y-4">
            {error ? <ErrorAlert error={error} /> : null}

            <DialogFooter className="border-t pt-4">
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={pending}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={pending}>
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <CopyPlus aria-hidden />}
                Duplicate instance
              </Button>
            </DialogFooter>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
