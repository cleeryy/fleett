import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import {
  AlertCircle,
  Loader2,
  MoreHorizontal,
  Play,
  RefreshCw,
  RotateCw,
  Square,
  Terminal,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { ChannelType, InstanceDetail, PatchInstanceInput, ShareRef } from "@/api/types";
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
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { SharePicker } from "@/components/share-picker";
import { StatusBadge } from "@/components/status-badge";
import { formatClock, formatDateTime, formatRelative, formatUptime } from "@/lib/format";
import { navigate } from "@/lib/router";
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
  return detail.env_keys.includes(env) ? env : null;
}

function OverviewTab({ detail, onConfigure }: { detail: InstanceDetail; onConfigure: () => void }) {
  const env = tokenEnv(detail);
  const providerKeySet = detail.env_keys.includes("HERMES_GATE_PROFILE_TOKEN");

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
              { label: "API key env", value: <Mono>{detail.api_key_env}</Mono> },
              {
                label: "Provider key",
                value: providerKeySet ? (
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
                      <Button
                        type="button"
                        variant="link"
                        size="sm"
                        className="h-auto p-0"
                        onClick={onConfigure}
                      >
                        Add the token
                      </Button>
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
  notes: string;
  shares: ShareRef[];
}

const CHANNEL_TYPES: ChannelType[] = ["none", "telegram", "discord"];

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
  draft,
  onDraftChange,
  onSaved,
}: {
  detail: InstanceDetail;
  draft: ConfigDraft;
  onDraftChange: (next: ConfigDraft) => void;
  onSaved: () => void;
}) {
  const sharesResource = useResource(() => api.listShares(), []);
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  const errors = validateConfig(draft, detail.channel);
  const showErrors = attempted;
  const providerKeySet = detail.env_keys.includes("HERMES_GATE_PROFILE_TOKEN");
  const dirty = JSON.stringify(draft) !== JSON.stringify(toConfigDraft(detail));
  // The prerequisite sits on the network field, so the alert belongs there.
  const egressError = error?.code === "egress_not_enforced" ? error : null;

  function update<K extends keyof ConfigDraft>(key: K, value: ConfigDraft[K]) {
    onDraftChange({ ...draft, [key]: value } as ConfigDraft);
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
    <form onSubmit={onSubmit} noValidate className="max-w-2xl space-y-6">
      {error && !egressError ? <ErrorAlert error={error} /> : null}

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
          Written to <span className="font-mono">secrets.env</span> (0600) and never returned by the API.
        </p>
      </div>

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
  const [nonce, setNonce] = useState(0);
  const [tab, setTab] = useState("overview");
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);

  const detail = useResource(() => api.getInstance(id), [id, nonce]);

  const [configDraft, setConfigDraft] = useState<ConfigDraft | null>(null);
  const configDraftFor = useRef<string | null>(null);
  const configResetPending = useRef(false);

  useEffect(() => {
    const data = detail.data;
    if (!data) return;
    // Keep edits across tab switches and background reloads, but take the
    // server copy after a save.
    if (configDraftFor.current === data.id && !configResetPending.current) return;
    configDraftFor.current = data.id;
    configResetPending.current = false;
    setConfigDraft(toConfigDraft(data));
  }, [detail.data]);

  function handleConfigSaved() {
    configResetPending.current = true;
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
  const uptime =
    running && data.container ? formatUptime(data.container.uptime_s) : null;

  return (
    <>
      <PageHeader
        title={
          <span className="flex flex-wrap items-center gap-3">
            {data.name}
            <StatusBadge status={data.status} />
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
            {running ? (
              <Button size="sm" onClick={() => run("stop")} disabled={busy}>
                {pendingAction === "stop" ? <Loader2 aria-hidden className="animate-spin" /> : <Square aria-hidden />}
                Stop
              </Button>
            ) : (
              <Button size="sm" onClick={() => run("start")} disabled={busy}>
                {pendingAction === "start" ? <Loader2 aria-hidden className="animate-spin" /> : <Play aria-hidden />}
                Start
              </Button>
            )}
            <Button variant="outline" size="sm" onClick={() => run("restart")} disabled={busy}>
              {pendingAction === "restart" ? <Loader2 aria-hidden className="animate-spin" /> : <RotateCw aria-hidden />}
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
                <DropdownMenuItem variant="destructive" onSelect={() => setDeleteOpen(true)}>
                  <Trash2 aria-hidden />
                  Delete instance…
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </>
        }
      />

      {actionError ? <ErrorAlert error={actionError} /> : null}

      <Tabs value={tab} onValueChange={setTab}>
        <div className="overflow-x-auto">
          <TabsList className="w-full justify-start sm:w-fit">
            <TabsTrigger value="overview">Overview</TabsTrigger>
            <TabsTrigger value="configuration">Configuration</TabsTrigger>
            <TabsTrigger value="logs">Logs</TabsTrigger>
            <TabsTrigger value="compose">Compose</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="overview" className="mt-4">
          <OverviewTab detail={data} onConfigure={() => setTab("configuration")} />
        </TabsContent>

        <TabsContent value="configuration" className="mt-4">
          {configDraft ? (
            <ConfigurationTab
              detail={data}
              draft={configDraft}
              onDraftChange={setConfigDraft}
              onSaved={handleConfigSaved}
            />
          ) : (
            <Skeleton className="h-64 w-full rounded-md" />
          )}
        </TabsContent>

        <TabsContent value="logs" className="mt-4">
          <LogsTab id={data.id} />
        </TabsContent>

        <TabsContent value="compose" className="mt-4 space-y-3">
          <p className="text-sm text-muted-foreground">
            <span className="font-mono">compose.yaml</span> is the source of truth. The controller rewrites it on
            every configuration change.
          </p>
          <CodeBlock value={data.compose_yaml} title={`instances/${data.id}/compose.yaml`} />
        </TabsContent>
      </Tabs>

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
  );
}
