import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { Loader2, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { CreateMcpServerInput, McpCatalogEntry, McpServerView } from "@/api/types";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { ErrorAlert } from "@/components/error-alert";
import { useI18n } from "@/lib/i18n";

interface EnvRow {
  id: string;
  key: string;
  value: string;
  secret: boolean;
  required: boolean;
  locked: boolean;
}

const KINDS = ["streamable-http", "sse", "stdio"];

function Field({
  label,
  hint,
  htmlFor,
  children,
}: {
  label: string;
  hint?: string;
  htmlFor: string;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1">
      <Label htmlFor={htmlFor}>{label}</Label>
      {children}
      {hint ? <p className="text-[11px] text-muted-foreground">{hint}</p> : null}
    </div>
  );
}

/**
 * Deploy one MCP server, either from a catalogue entry (prefilled, with its
 * required variables) or from a complete custom spec. The container publishes
 * no port: agents reach it by its Docker DNS name.
 */
export function McpDeployDialog({
  open,
  onOpenChange,
  catalog,
  onDeployed,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  catalog: McpCatalogEntry | null;
  onDeployed: (server: McpServerView) => void;
}) {
  const { t } = useI18n();
  const nextRow = useRef(0);
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [kind, setKind] = useState("streamable-http");
  const [image, setImage] = useState("");
  const [command, setCommand] = useState("");
  const [port, setPort] = useState("8080");
  const [path, setPath] = useState("/mcp");
  const [persist, setPersist] = useState(false);
  const [cpu, setCpu] = useState("0.5");
  const [mem, setMem] = useState("256");
  const [env, setEnv] = useState<EnvRow[]>([]);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [validation, setValidation] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setId(catalog?.id ?? "");
    setName(catalog?.name ?? "");
    setDescription(catalog?.description ?? "");
    setKind(catalog?.kind ?? "streamable-http");
    setImage(catalog?.image ?? "");
    setCommand((catalog?.command ?? []).join(" "));
    setPort(String(catalog?.port ?? 8080));
    setPath(catalog?.path ?? "/mcp");
    setPersist(false);
    setCpu("0.5");
    setMem("256");
    setEnv(
      (catalog?.env ?? []).map((entry) => ({
        id: `env-${(nextRow.current += 1)}`,
        key: entry.key,
        value: "",
        secret: entry.secret,
        required: entry.required,
        locked: true,
      })),
    );
    setPending(false);
    setError(null);
    setValidation(null);
  }, [open, catalog]);

  function addRow() {
    setEnv((rows) => [
      ...rows,
      {
        id: `env-${(nextRow.current += 1)}`,
        key: "",
        value: "",
        secret: false,
        required: false,
        locked: false,
      },
    ]);
  }

  function patchRow(id: string, patch: Partial<EnvRow>) {
    setEnv((rows) => rows.map((row) => (row.id === id ? { ...row, ...patch } : row)));
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setValidation(null);
    setError(null);
    const trimmedId = id.trim();
    if (!/^[a-z0-9][a-z0-9-]{1,31}$/.test(trimmedId)) {
      setValidation(t("mcp.deploy.invalidId"));
      return;
    }
    const portNumber = Number(port);
    if (!Number.isInteger(portNumber) || portNumber < 1 || portNumber > 65_535) {
      setValidation(t("mcp.deploy.portRange"));
      return;
    }
    if (kind !== "stdio" && !image.trim()) {
      setValidation(t("mcp.deploy.imageRequired"));
      return;
    }
    const keys = new Set<string>();
    for (const row of env) {
      const key = row.key.trim();
      if (!key) {
        setValidation(t("mcp.deploy.envKeyRequired"));
        return;
      }
      if (keys.has(key)) {
        setValidation(t("mcp.deploy.envDuplicate"));
        return;
      }
      keys.add(key);
      if (row.required && !row.value.trim()) {
        setValidation(t("mcp.deploy.envValueRequired", { key }));
        return;
      }
    }

    const input: CreateMcpServerInput = catalog
      ? {
          catalog: catalog.id,
          id: trimmedId,
          name: name.trim() || trimmedId,
          description: description.trim(),
          port: portNumber,
          path: path.trim() || "/mcp",
          persist,
          cpu_limit: Number(cpu) || 0.5,
          mem_limit_mb: Number(mem) || 256,
          env: env.map((row) => ({
            key: row.key.trim(),
            value: row.value,
            secret: row.secret,
          })),
        }
      : {
          id: trimmedId,
          name: name.trim() || trimmedId,
          description: description.trim(),
          kind,
          image: image.trim(),
          command: command.trim() ? command.trim().split(/\s+/) : [],
          port: portNumber,
          path: path.trim() || "/mcp",
          persist,
          cpu_limit: Number(cpu) || 0.5,
          mem_limit_mb: Number(mem) || 256,
          env: env.map((row) => ({
            key: row.key.trim(),
            value: row.value,
            secret: row.secret,
          })),
        };

    setPending(true);
    try {
      const server = await api.createMcpServer(input);
      toast.success(t("mcp.deploy.done", { name: server.name }));
      onDeployed(server);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[92dvh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {catalog ? t("mcp.deploy.title", { name: catalog.name }) : t("mcp.deploy.titleCustom")}
          </DialogTitle>
          <DialogDescription>{t("mcp.deploy.description")}</DialogDescription>
        </DialogHeader>

        <form className="space-y-4" onSubmit={(event) => void submit(event)}>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label={t("mcp.deploy.id")} hint={t("mcp.deploy.idHint")} htmlFor="mcp-deploy-id">
              <Input
                id="mcp-deploy-id"
                value={id}
                onChange={(event) => setId(event.target.value)}
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
                required
              />
            </Field>
            <Field label={t("mcp.deploy.name")} htmlFor="mcp-deploy-name">
              <Input
                id="mcp-deploy-name"
                value={name}
                onChange={(event) => setName(event.target.value)}
                autoComplete="off"
              />
            </Field>
          </div>

          <Field
            label={t("mcp.deploy.descriptionLabel")}
            htmlFor="mcp-deploy-description"
          >
            <Textarea
              id="mcp-deploy-description"
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              rows={2}
            />
          </Field>

          {catalog ? (
            <p className="rounded-md border bg-muted/30 px-3 py-2 font-mono text-[11px] break-all text-muted-foreground">
              {t("mcp.deploy.catalogNote", { kind: catalog.kind, image: catalog.image || "—" })}
            </p>
          ) : (
            <>
              <div className="grid gap-3 sm:grid-cols-2">
                <Field label={t("mcp.deploy.kind")} htmlFor="mcp-deploy-kind">
                  <Select value={kind} onValueChange={setKind}>
                    <SelectTrigger id="mcp-deploy-kind" className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {KINDS.map((option) => (
                        <SelectItem key={option} value={option}>
                          {option}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </Field>
                <Field label={t("mcp.deploy.image")} htmlFor="mcp-deploy-image">
                  <Input
                    id="mcp-deploy-image"
                    value={image}
                    onChange={(event) => setImage(event.target.value)}
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono"
                  />
                </Field>
              </div>
              <Field
                label={t("mcp.deploy.command")}
                hint={t("mcp.deploy.commandHint")}
                htmlFor="mcp-deploy-command"
              >
                <Input
                  id="mcp-deploy-command"
                  value={command}
                  onChange={(event) => setCommand(event.target.value)}
                  autoComplete="off"
                  spellCheck={false}
                  className="font-mono"
                />
              </Field>
            </>
          )}

          <div className="grid gap-3 sm:grid-cols-4">
            <Field label={t("mcp.deploy.port")} htmlFor="mcp-deploy-port">
              <Input
                id="mcp-deploy-port"
                type="number"
                min={1}
                max={65_535}
                value={port}
                onChange={(event) => setPort(event.target.value)}
                className="font-mono"
              />
            </Field>
            <Field label={t("mcp.deploy.path")} htmlFor="mcp-deploy-path">
              <Input
                id="mcp-deploy-path"
                value={path}
                onChange={(event) => setPath(event.target.value)}
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
              />
            </Field>
            <Field label={t("mcp.deploy.cpu")} htmlFor="mcp-deploy-cpu">
              <Input
                id="mcp-deploy-cpu"
                type="number"
                min={0.1}
                step={0.1}
                value={cpu}
                onChange={(event) => setCpu(event.target.value)}
                className="font-mono"
              />
            </Field>
            <Field label={t("mcp.deploy.mem")} htmlFor="mcp-deploy-mem">
              <Input
                id="mcp-deploy-mem"
                type="number"
                min={32}
                step={32}
                value={mem}
                onChange={(event) => setMem(event.target.value)}
                className="font-mono"
              />
            </Field>
          </div>

          <div className="flex items-start gap-2">
            <Checkbox
              id="mcp-deploy-persist"
              checked={persist}
              onCheckedChange={(value) => setPersist(value === true)}
            />
            <Label htmlFor="mcp-deploy-persist" className="font-normal">
              {t("mcp.deploy.persist")}
            </Label>
          </div>

          <div className="space-y-2">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div>
                <p className="text-sm font-medium">{t("mcp.deploy.env")}</p>
                <p className="text-[11px] text-muted-foreground">{t("mcp.deploy.envHint")}</p>
              </div>
              <Button type="button" variant="outline" size="xs" onClick={addRow}>
                <Plus aria-hidden />
                {t("mcp.deploy.addEnv")}
              </Button>
            </div>

            {env.length === 0 ? (
              <p className="rounded-md border border-dashed px-3 py-2 text-xs text-muted-foreground">
                {t("mcp.list.noEnv")}
              </p>
            ) : (
              <ul className="space-y-2">
                {env.map((row) => (
                  <li key={row.id} className="grid gap-2 rounded-md border p-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_auto] sm:items-start">
                    <div className="space-y-1">
                      <Label
                        htmlFor={`${row.id}-key`}
                        className="text-[11px] font-normal text-muted-foreground"
                      >
                        {t("mcp.deploy.key")}
                      </Label>
                      <Input
                        id={`${row.id}-key`}
                        value={row.key}
                        onChange={(event) => patchRow(row.id, { key: event.target.value })}
                        readOnly={row.locked}
                        autoComplete="off"
                        spellCheck={false}
                        className="h-8 font-mono text-xs"
                      />
                      {row.required ? (
                        <p className="text-[10px] text-amber-700 dark:text-amber-400">
                          {t("mcp.deploy.envRequired")}
                        </p>
                      ) : null}
                    </div>
                    <div className="space-y-1">
                      <Label
                        htmlFor={`${row.id}-value`}
                        className="text-[11px] font-normal text-muted-foreground"
                      >
                        {t("mcp.deploy.value")}
                      </Label>
                      <Input
                        id={`${row.id}-value`}
                        type={row.secret ? "password" : "text"}
                        value={row.value}
                        onChange={(event) => patchRow(row.id, { value: event.target.value })}
                        autoComplete="new-password"
                        spellCheck={false}
                        className="h-8 font-mono text-xs"
                      />
                    </div>
                    <div className="flex items-center gap-2 sm:pt-5">
                      <Checkbox
                        id={`${row.id}-secret`}
                        checked={row.secret}
                        onCheckedChange={(value) =>
                          patchRow(row.id, { secret: value === true })
                        }
                      />
                      <Label
                        htmlFor={`${row.id}-secret`}
                        className="text-[11px] font-normal text-muted-foreground"
                      >
                        {t("mcp.deploy.secret")}
                      </Label>
                      {row.required ? null : (
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon-xs"
                          onClick={() =>
                            setEnv((rows) => rows.filter((candidate) => candidate.id !== row.id))
                          }
                          aria-label={t("mcp.deploy.removeEnv", { key: row.key || "?" })}
                          title={t("mcp.deploy.removeEnv", { key: row.key || "?" })}
                        >
                          <Trash2 aria-hidden />
                        </Button>
                      )}
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </div>

          {validation ? (
            <p role="alert" className="text-sm text-destructive">
              {validation}
            </p>
          ) : null}
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
                <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" />
              ) : null}
              {pending ? t("mcp.deploy.deploying") : t("mcp.deploy.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
