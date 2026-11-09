import { useEffect, useState, type FormEvent } from "react";
import { Loader2, Plus, Save } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { ProviderSummary } from "@/api/types";
import { Button } from "@/components/ui/button";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ErrorAlert } from "@/components/error-alert";
import { isValidProviderName } from "@/lib/validation";

export const PROVIDER_KIND_OPTIONS = [
  { value: "openai-compatible", label: "OpenAI-compatible" },
  { value: "anthropic", label: "Anthropic" },
  { value: "google", label: "Google" },
] as const;

const DEFAULT_ENV = "HERMES_GATE_PROFILE_TOKEN";

interface Draft {
  name: string;
  kind: string;
  baseUrl: string;
  apiKeyEnv: string;
  secret: string;
}

const EMPTY_DRAFT: Draft = {
  name: "",
  kind: "openai-compatible",
  baseUrl: "",
  apiKeyEnv: DEFAULT_ENV,
  secret: "",
};

function validate(draft: Draft, creating: boolean): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!isValidProviderName(draft.name.trim())) {
    errors.name = "Use 2-63 characters: lowercase letters, digits and hyphens.";
  }
  if (!draft.baseUrl.trim()) {
    errors.baseUrl = "Enter a base URL.";
  } else {
    try {
      const url = new URL(draft.baseUrl.trim());
      if (url.protocol !== "http:" && url.protocol !== "https:") {
        errors.baseUrl = "Use an absolute http or https URL.";
      }
    } catch {
      errors.baseUrl = "Use an absolute http or https URL.";
    }
  }
  if (!/^[A-Z_][A-Z0-9_]{0,63}$/.test(draft.apiKeyEnv.trim())) {
    errors.apiKeyEnv = "Use uppercase letters, digits and underscores.";
  }
  if (creating && !draft.secret.trim()) {
    errors.secret = "Enter the API key.";
  }
  return errors;
}

const FIELD_IDS: Record<string, string> = {
  name: "provider-name",
  baseUrl: "provider-base-url",
  apiKeyEnv: "provider-api-key-env",
  secret: "provider-secret",
};

/**
 * One dialog for both create (`provider` absent) and metadata edit. The secret
 * is only collected on create: rotating it is its own action on the detail
 * page, because it restarts every attached instance.
 */
export function ProviderFormDialog({
  provider,
  open,
  onOpenChange,
  onSaved,
}: {
  provider?: ProviderSummary | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved: (provider: ProviderSummary) => void;
}) {
  const editing = provider !== undefined && provider !== null;
  const editingProvider = provider ?? null;
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [fieldError, setFieldError] = useState<{ key: string; message: string } | null>(null);

  useEffect(() => {
    if (!open) return;
    setDraft(
      provider
        ? {
            name: provider.name,
            kind: provider.kind,
            baseUrl: provider.base_url,
            apiKeyEnv: provider.api_key_env,
            secret: "",
          }
        : EMPTY_DRAFT,
    );
    setAttempted(false);
    setPending(false);
    setError(null);
    setFieldError(null);
  }, [open, provider]);

  const errors = validate(draft, !editing);

  function update<K extends keyof Draft>(key: K, value: Draft[K]) {
    setDraft((current) => ({ ...current, [key]: value }));
    if (fieldError?.key === key) setFieldError(null);
  }

  function messageFor(key: string): string | undefined {
    if (fieldError?.key === key) return fieldError.message;
    return attempted ? errors[key] : undefined;
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setAttempted(true);
    setError(null);
    const keys = Object.keys(errors);
    if (keys.length > 0) {
      document.getElementById(FIELD_IDS[keys[0]] ?? keys[0])?.focus();
      return;
    }

    setPending(true);
    try {
      const body = {
        name: draft.name.trim(),
        kind: draft.kind,
        base_url: draft.baseUrl.trim(),
        api_key_env: draft.apiKeyEnv.trim(),
      };
      const saved = editingProvider
        ? (await api.patchProvider(editingProvider.id, body)).provider
        : await api.createProvider({ ...body, secret: draft.secret });
      toast.success(editing ? `Provider ${saved.name} saved.` : `Provider ${saved.name} created.`);
      onSaved(saved);
      onOpenChange(false);
    } catch (cause) {
      const failure = toApiError(cause);
      if (failure.code === "invalid_name" || failure.code === "provider_exists") {
        setFieldError({ key: "name", message: failure.message });
        window.requestAnimationFrame(() => document.getElementById("provider-name")?.focus());
      } else if (failure.code === "invalid_base_url") {
        setFieldError({ key: "baseUrl", message: failure.message });
        window.requestAnimationFrame(() => document.getElementById("provider-base-url")?.focus());
      } else if (failure.code === "invalid_api_key_env") {
        setFieldError({ key: "apiKeyEnv", message: failure.message });
        window.requestAnimationFrame(() => document.getElementById("provider-api-key-env")?.focus());
      } else if (failure.code === "invalid_secret") {
        setFieldError({ key: "secret", message: failure.message });
      } else {
        setError(failure);
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh_-_2rem)] overflow-y-auto">
        <form onSubmit={onSubmit} noValidate className="space-y-4">
          <DialogHeader>
            <DialogTitle>
              {editingProvider ? (
                <>
                  Edit <span className="font-mono">{editingProvider.name}</span>
                </>
              ) : (
                "New provider"
              )}
            </DialogTitle>
            <DialogDescription>
              {editing
                ? "Metadata only. Attached instances keep their current endpoint and variable name; rotating the secret is a separate action that restarts them."
                : "One credential several agents can share. Rotating it later restarts every attached instance."}
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-2">
            <Label htmlFor="provider-name">Name</Label>
            <Input
              id="provider-name"
              value={draft.name}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder="shared"
              aria-invalid={!!messageFor("name")}
              aria-describedby={messageFor("name") ? "provider-name-error" : "provider-name-help"}
              onChange={(event) => update("name", event.target.value.toLowerCase())}
            />
            <p id="provider-name-help" className="text-xs text-muted-foreground">
              Lowercase slug, unique across the fleet.
            </p>
            {messageFor("name") ? (
              <p id="provider-name-error" className="text-xs text-destructive">
                {messageFor("name")}
              </p>
            ) : null}
          </div>

          <div className="space-y-2">
            <Label htmlFor="provider-kind">Kind</Label>
            <Select value={draft.kind} onValueChange={(value) => update("kind", value)}>
              <SelectTrigger id="provider-kind" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {PROVIDER_KIND_OPTIONS.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label} · {option.value}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground">
              Metadata for the console; every kind is tested through its <span className="font-mono">/models</span>{" "}
              endpoint.
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="provider-base-url">Base URL</Label>
            <Input
              id="provider-base-url"
              value={draft.baseUrl}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder="https://opencode.ai/inference/go/openai/v1"
              aria-invalid={!!messageFor("baseUrl")}
              aria-describedby={messageFor("baseUrl") ? "provider-base-url-error" : "provider-base-url-help"}
              onChange={(event) => update("baseUrl", event.target.value)}
            />
            <p id="provider-base-url-help" className="text-xs text-muted-foreground">
              Endpoint new instances inherit. Test connection probes{" "}
              <span className="font-mono">/models</span>.
            </p>
            {messageFor("baseUrl") ? (
              <p id="provider-base-url-error" className="text-xs text-destructive">
                {messageFor("baseUrl")}
              </p>
            ) : null}
          </div>

          <div className="space-y-2">
            <Label htmlFor="provider-api-key-env">API key env</Label>
            <Input
              id="provider-api-key-env"
              value={draft.apiKeyEnv}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder="HERMES_GATE_PROFILE_TOKEN"
              aria-invalid={!!messageFor("apiKeyEnv")}
              aria-describedby={
                messageFor("apiKeyEnv") ? "provider-api-key-env-error" : "provider-api-key-env-help"
              }
              onChange={(event) => update("apiKeyEnv", event.target.value)}
            />
            <p id="provider-api-key-env-help" className="text-xs text-muted-foreground">
              Container variable the secret is injected under. New instances inherit it.
            </p>
            {messageFor("apiKeyEnv") ? (
              <p id="provider-api-key-env-error" className="text-xs text-destructive">
                {messageFor("apiKeyEnv")}
              </p>
            ) : null}
          </div>

          {!editing ? (
            <div className="space-y-2">
              <Label htmlFor="provider-secret">API key</Label>
              <Input
                id="provider-secret"
                type="password"
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
                value={draft.secret}
                aria-invalid={!!messageFor("secret")}
                aria-describedby={messageFor("secret") ? "provider-secret-error" : "provider-secret-help"}
                onChange={(event) => update("secret", event.target.value)}
              />
              <p id="provider-secret-help" className="text-xs text-muted-foreground">
                Write-only: encrypted at rest and never returned by the API. The host needs its secrets key
                (<span className="font-mono">sudo fleett secrets init</span>).
              </p>
              {messageFor("secret") ? (
                <p id="provider-secret-error" className="text-xs text-destructive">
                  {messageFor("secret")}
                </p>
              ) : null}
            </div>
          ) : null}

          {error ? <ErrorAlert error={error} /> : null}

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={pending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? (
                <Loader2 aria-hidden className="animate-spin" />
              ) : editing ? (
                <Save aria-hidden />
              ) : (
                <Plus aria-hidden />
              )}
              {editing ? "Save provider" : "Create provider"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
