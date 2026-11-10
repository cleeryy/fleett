import { useEffect, useState, type FormEvent } from "react";
import { Loader2, Plus, Save, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
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
import { RichText } from "@/components/rich-text";
import { useI18n } from "@/lib/i18n";
import {
  createHeadersFor,
  isValidHeaderName,
  isValidModelName,
  newHeaderRow,
  patchHeadersFor,
  providerHeaderNames,
  type HeaderRow,
} from "@/lib/providers";
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
  /** Optional default model for every call. */
  model: string;
  headers: HeaderRow[];
}

const EMPTY_DRAFT: Draft = {
  name: "",
  kind: "openai-compatible",
  baseUrl: "",
  apiKeyEnv: DEFAULT_ENV,
  secret: "",
  model: "",
  headers: [],
};

type Translate = (key: string, params?: Record<string, string | number>) => string;

function validate(
  draft: Draft,
  creating: boolean,
  t: Translate,
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!isValidProviderName(draft.name.trim())) {
    errors.name = t("providers.form.nameInvalid");
  }
  const invalidUrl = t("providers.form.urlInvalid");
  if (!draft.baseUrl.trim()) {
    errors.baseUrl = t("providers.form.urlRequired");
  } else {
    try {
      const url = new URL(draft.baseUrl.trim());
      if (url.protocol !== "http:" && url.protocol !== "https:") {
        errors.baseUrl = invalidUrl;
      }
    } catch {
      errors.baseUrl = invalidUrl;
    }
  }
  if (!/^[A-Z_][A-Z0-9_]{0,63}$/.test(draft.apiKeyEnv.trim())) {
    errors.apiKeyEnv = t("providers.form.envInvalid");
  }
  if (creating && !draft.secret.trim()) {
    errors.secret = t("providers.form.secretRequired");
  }
  const model = draft.model.trim();
  if (model && !isValidModelName(model)) {
    errors.model = t("providers.form.modelInvalid");
  }
  return errors;
}

interface HeaderIssue {
  rowId: number;
  field: "name" | "value";
  message: string;
}

/** First broken row of the header editor, so the error lands on the field. */
function validateHeaders(rows: HeaderRow[], t: Translate): HeaderIssue | null {
  const seen = new Set<string>();
  for (const row of rows) {
    const name = row.name.trim();
    if (!name && !row.value) continue;
    if (!name) {
      return { rowId: row.id, field: "name", message: t("providers.form.headerNameRequired") };
    }
    if (!isValidHeaderName(name)) {
      return { rowId: row.id, field: "name", message: t("providers.form.headerNameInvalid") };
    }
    const key = name.toLowerCase();
    if (seen.has(key)) {
      return {
        rowId: row.id,
        field: "name",
        message: t("providers.form.headerDuplicate", { name }),
      };
    }
    seen.add(key);
    if (!row.value && !row.stored) {
      return {
        rowId: row.id,
        field: "value",
        message: t("providers.form.headerValueRequired", { name }),
      };
    }
  }
  return null;
}

const FIELD_IDS: Record<string, string> = {
  name: "provider-name",
  baseUrl: "provider-base-url",
  apiKeyEnv: "provider-api-key-env",
  secret: "provider-secret",
  model: "provider-model",
};

/**
 * One dialog for both create (`provider` absent) and metadata edit. The secret
 * is only collected on create: rotating it is its own action on the detail
 * page, because it restarts every attached instance.
 *
 * Extra header values are write-only like the secret: the API returns the
 * names, so a stored row starts empty and means "keep the encrypted value"
 * unless it is removed or a new value is typed.
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
  const { t } = useI18n();
  const editing = provider !== undefined && provider !== null;
  const editingProvider = provider ?? null;
  const storedNames = editingProvider ? providerHeaderNames(editingProvider) : [];
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
            model: provider.model ?? "",
            headers: providerHeaderNames(provider).map((name) =>
              newHeaderRow({ name, stored: true }),
            ),
          }
        : EMPTY_DRAFT,
    );
    setAttempted(false);
    setPending(false);
    setError(null);
    setFieldError(null);
  }, [open, provider]);

  const errors = validate(draft, !editing, t);
  const headerIssue = validateHeaders(draft.headers, t);
  const visibleHeaderIssue = attempted ? headerIssue : null;

  function update<K extends keyof Draft>(key: K, value: Draft[K]) {
    setDraft((current) => ({ ...current, [key]: value }));
    if (fieldError?.key === key) setFieldError(null);
  }

  function patchHeader(id: number, changes: Partial<HeaderRow>) {
    update(
      "headers",
      draft.headers.map((row) => (row.id === id ? { ...row, ...changes } : row)),
    );
    if (fieldError?.key === "headers") setFieldError(null);
  }

  function addHeader() {
    const row = newHeaderRow();
    update("headers", [...draft.headers, row]);
    window.requestAnimationFrame(() =>
      document.getElementById(`provider-header-${row.id}-name`)?.focus(),
    );
  }

  function removeHeader(id: number) {
    update(
      "headers",
      draft.headers.filter((row) => row.id !== id),
    );
  }

  function messageFor(key: string): string | undefined {
    if (fieldError?.key === key) return fieldError.message;
    return attempted ? errors[key] : undefined;
  }

  function focusInvalid() {
    if (attempted && headerIssue) {
      document.getElementById(`provider-header-${headerIssue.rowId}-${headerIssue.field}`)?.focus();
      return;
    }
    const key = Object.keys(errors)[0];
    if (key) document.getElementById(FIELD_IDS[key] ?? key)?.focus();
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setAttempted(true);
    setError(null);
    if (Object.keys(errors).length > 0 || headerIssue) {
      focusInvalid();
      return;
    }

    setPending(true);
    try {
      const model = draft.model.trim();
      const createHeaders = createHeadersFor(draft.headers);
      const body = {
        name: draft.name.trim(),
        kind: draft.kind,
        base_url: draft.baseUrl.trim(),
        api_key_env: draft.apiKeyEnv.trim(),
      };
      const saved = editingProvider
        ? (
            await api.patchProvider(editingProvider.id, {
              ...body,
              model: model || null,
              extra_headers: patchHeadersFor(draft.headers, storedNames),
            })
          ).provider
        : await api.createProvider({
            ...body,
            secret: draft.secret,
            model: model || null,
            ...(Object.keys(createHeaders).length > 0 ? { extra_headers: createHeaders } : {}),
          });
      toast.success(
        editing
          ? t("providers.form.saved", { name: saved.name })
          : t("providers.form.created", { name: saved.name }),
      );
      onSaved(saved);
      onOpenChange(false);
    } catch (cause) {
      const failure = toApiError(cause);
      if (failure.code === "invalid_name" || failure.code === "provider_exists") {
        setFieldError({ key: "name", message: apiErrorMessage(failure, t) });
        window.requestAnimationFrame(() => document.getElementById("provider-name")?.focus());
      } else if (failure.code === "invalid_base_url") {
        setFieldError({ key: "baseUrl", message: apiErrorMessage(failure, t) });
        window.requestAnimationFrame(() => document.getElementById("provider-base-url")?.focus());
      } else if (failure.code === "invalid_api_key_env") {
        setFieldError({ key: "apiKeyEnv", message: apiErrorMessage(failure, t) });
        window.requestAnimationFrame(() => document.getElementById("provider-api-key-env")?.focus());
      } else if (failure.code === "invalid_secret") {
        setFieldError({ key: "secret", message: apiErrorMessage(failure, t) });
      } else if (failure.code === "invalid_model") {
        setFieldError({ key: "model", message: apiErrorMessage(failure, t) });
        window.requestAnimationFrame(() => document.getElementById("provider-model")?.focus());
      } else if (failure.code === "invalid_header") {
        setFieldError({ key: "headers", message: apiErrorMessage(failure, t) });
        window.requestAnimationFrame(() =>
          document.querySelector<HTMLInputElement>('[id^="provider-header-"][id$="-name"]')?.focus(),
        );
      } else {
        setError(failure);
      }
    } finally {
      setPending(false);
    }
  }

  const headerMessage =
    visibleHeaderIssue?.message ??
    (fieldError?.key === "headers" ? fieldError.message : undefined);

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh_-_2rem)] overflow-y-auto">
        <form onSubmit={onSubmit} noValidate className="space-y-4">
          <DialogHeader>
            <DialogTitle>
              {editingProvider ? (
                <>
                  {t("providers.form.editTitle")}{" "}
                  <span className="font-mono">{editingProvider.name}</span>
                </>
              ) : (
                t("providers.form.newTitle")
              )}
            </DialogTitle>
            <DialogDescription>
              {editing
                ? t("providers.form.editDescription")
                : t("providers.form.createDescription")}
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-2">
            <Label htmlFor="provider-name">{t("common.name")}</Label>
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
              {t("providers.form.nameHint")}
            </p>
            {messageFor("name") ? (
              <p id="provider-name-error" className="text-xs text-destructive">
                {messageFor("name")}
              </p>
            ) : null}
          </div>

          <div className="space-y-2">
            <Label htmlFor="provider-kind">{t("providers.form.kind")}</Label>
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
              <RichText text={t("providers.form.kindHint")} />
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="provider-base-url">{t("providers.form.baseUrl")}</Label>
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
              <RichText text={t("providers.form.baseUrlHint")} />
            </p>
            {messageFor("baseUrl") ? (
              <p id="provider-base-url-error" className="text-xs text-destructive">
                {messageFor("baseUrl")}
              </p>
            ) : null}
          </div>

          <div className="space-y-2">
            <Label htmlFor="provider-api-key-env">{t("providers.form.apiKeyEnv")}</Label>
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
              {t("providers.form.apiKeyEnvHint")}
            </p>
            {messageFor("apiKeyEnv") ? (
              <p id="provider-api-key-env-error" className="text-xs text-destructive">
                {messageFor("apiKeyEnv")}
              </p>
            ) : null}
          </div>

          {!editing ? (
            <div className="space-y-2">
              <Label htmlFor="provider-secret">{t("providers.form.apiKey")}</Label>
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
                <RichText text={t("providers.form.secretHint")} />
              </p>
              {messageFor("secret") ? (
                <p id="provider-secret-error" className="text-xs text-destructive">
                  {messageFor("secret")}
                </p>
              ) : null}
            </div>
          ) : null}

          <div className="space-y-2">
            <Label htmlFor="provider-model">{t("providers.form.model")}</Label>
            <Input
              id="provider-model"
              value={draft.model}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder="deepseek-v4.1-flash"
              aria-invalid={!!messageFor("model")}
              aria-describedby={messageFor("model") ? "provider-model-error" : "provider-model-help"}
              onChange={(event) => update("model", event.target.value)}
            />
            <p id="provider-model-help" className="text-xs text-muted-foreground">
              <RichText text={t("providers.form.modelHint")} />
            </p>
            {messageFor("model") ? (
              <p id="provider-model-error" className="text-xs text-destructive">
                {messageFor("model")}
              </p>
            ) : null}
          </div>

          <div className="space-y-2">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <Label>{t("providers.form.headers")}</Label>
              <Button type="button" variant="outline" size="sm" onClick={addHeader}>
                <Plus aria-hidden />
                {t("providers.form.addHeader")}
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">
              <RichText text={t("providers.form.headersHint")} />
            </p>

            {draft.headers.length === 0 ? (
              <p className="rounded-md border border-dashed px-3 py-3 text-xs text-muted-foreground">
                {t("providers.form.noHeaders")}
              </p>
            ) : (
              <div className="space-y-2">
                {draft.headers.map((row, index) => {
                  const rowError =
                    visibleHeaderIssue?.rowId === row.id ? visibleHeaderIssue.message : undefined;
                  const name = row.name.trim() || t("providers.form.headerN", { index: index + 1 });
                  return (
                    <div key={row.id} className="space-y-1">
                      <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_auto] sm:items-center">
                        <Input
                          id={`provider-header-${row.id}-name`}
                          value={row.name}
                          autoComplete="off"
                          spellCheck={false}
                          className="col-span-2 font-mono text-xs sm:col-span-1"
                          placeholder="x-opencode-session"
                          aria-label={t("providers.form.headerNameAria", { name })}
                          aria-invalid={!!rowError && visibleHeaderIssue?.field === "name"}
                          onChange={(event) => patchHeader(row.id, { name: event.target.value })}
                        />
                        <Input
                          id={`provider-header-${row.id}-value`}
                          value={row.value}
                          autoComplete="off"
                          spellCheck={false}
                          className="font-mono text-xs"
                          placeholder={
                            row.stored
                              ? t("providers.form.headerStored")
                              : t("providers.form.headerValue")
                          }
                          aria-label={t("providers.form.headerValueAria", { name })}
                          aria-invalid={!!rowError && visibleHeaderIssue?.field === "value"}
                          onChange={(event) => patchHeader(row.id, { value: event.target.value })}
                        />
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon-sm"
                          className="justify-self-start sm:justify-self-auto"
                          aria-label={t("providers.form.removeHeader", { name })}
                          title={t("providers.form.removeHeader", { name })}
                          onClick={() => removeHeader(row.id)}
                        >
                          <Trash2 aria-hidden />
                        </Button>
                      </div>
                      {rowError ? <p className="text-xs text-destructive">{rowError}</p> : null}
                    </div>
                  );
                })}
              </div>
            )}

            {draft.headers.some((row) => row.stored) ? (
              <p className="text-xs text-muted-foreground">
                {t("providers.form.headerWriteOnly")}
              </p>
            ) : null}

            {headerMessage && !visibleHeaderIssue ? (
              <p className="text-xs text-destructive">{headerMessage}</p>
            ) : null}
          </div>

          {error ? <ErrorAlert error={error} /> : null}

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={pending}
            >
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? (
                <Loader2 aria-hidden className="animate-spin" />
              ) : editing ? (
                <Save aria-hidden />
              ) : (
                <Plus aria-hidden />
              )}
              {editing ? t("providers.form.save") : t("providers.form.create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
