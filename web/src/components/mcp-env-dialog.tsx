import { useEffect, useRef, useState, type FormEvent } from "react";
import { Loader2, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { McpEnvInput, McpServerView } from "@/api/types";
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
import { ErrorAlert } from "@/components/error-alert";
import { useI18n } from "@/lib/i18n";

interface EnvDraft {
  id: string;
  key: string;
  value: string;
  secret: boolean;
  wasSet: boolean;
}

/**
 * The API never returns a value, and `PUT /env` replaces the whole set. The
 * only honest editor is therefore a full re-entry: existing keys keep their
 * names, every value has to be typed again, and removing a row deletes it.
 */
export function McpEnvDialog({
  server,
  open,
  onOpenChange,
  onSaved,
}: {
  server: McpServerView;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved: (next: McpServerView) => void;
}) {
  const { t } = useI18n();
  const nextRow = useRef(0);
  const [rows, setRows] = useState<EnvDraft[]>([]);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [validation, setValidation] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setRows(
      server.env.map((entry) => ({
        id: `env-${(nextRow.current += 1)}`,
        key: entry.key,
        value: "",
        secret: entry.secret,
        wasSet: entry.set,
      })),
    );
    setPending(false);
    setError(null);
    setValidation(null);
  }, [open, server]);

  function addRow() {
    setRows((current) => [
      ...current,
      {
        id: `env-${(nextRow.current += 1)}`,
        key: "",
        value: "",
        secret: false,
        wasSet: false,
      },
    ]);
  }

  function patchRow(id: string, patch: Partial<EnvDraft>) {
    setRows((current) =>
      current.map((row) => (row.id === id ? { ...row, ...patch } : row)),
    );
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setValidation(null);
    setError(null);
    const keys = new Set<string>();
    for (const row of rows) {
      const key = row.key.trim();
      if (!key) {
        setValidation(t("mcp.detail.envKeyRequired"));
        return;
      }
      if (keys.has(key)) {
        setValidation(t("mcp.detail.envDuplicate"));
        return;
      }
      if (row.wasSet && !row.value.trim()) {
        setValidation(t("mcp.detail.envValueRequired", { key }));
        return;
      }
      keys.add(key);
    }
    setPending(true);
    const env: McpEnvInput[] = rows.map((row) => ({
      key: row.key.trim(),
      value: row.value,
      secret: row.secret,
    }));
    try {
      const next = await api.putMcpServerEnv(server.id, env);
      toast.success(t("mcp.detail.envSaved"));
      onSaved(next);
      onOpenChange(false);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t("mcp.detail.envReplaceTitle", { name: server.name })}</DialogTitle>
          <DialogDescription>{t("mcp.detail.envReplaceDescription")}</DialogDescription>
        </DialogHeader>

        <form className="space-y-3" onSubmit={(event) => void submit(event)}>
          {rows.length === 0 ? (
            <p className="rounded-md border border-dashed px-3 py-3 text-sm text-muted-foreground">
              {t("mcp.detail.envEmpty")}
            </p>
          ) : (
            <ul className="space-y-2">
              {rows.map((row) => (
                <li
                  key={row.id}
                  className="grid gap-2 rounded-md border p-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)_auto] sm:items-start"
                >
                  <div className="space-y-1">
                    <Label
                      htmlFor={`${row.id}-key`}
                      className="text-[11px] font-normal text-muted-foreground"
                    >
                      {t("mcp.detail.envKey")}
                    </Label>
                    <Input
                      id={`${row.id}-key`}
                      value={row.key}
                      onChange={(event) => patchRow(row.id, { key: event.target.value })}
                      autoComplete="off"
                      spellCheck={false}
                      className="h-8 font-mono text-xs"
                    />
                  </div>
                  <div className="space-y-1">
                    <div className="flex items-center justify-between gap-2">
                      <Label
                        htmlFor={`${row.id}-value`}
                        className="text-[11px] font-normal text-muted-foreground"
                      >
                        {t("mcp.detail.envValue")}
                      </Label>
                      {row.wasSet ? (
                        <Badge
                          variant="outline"
                          className="px-1.5 py-0 font-mono text-[10px] font-normal text-muted-foreground"
                        >
                          {t("mcp.detail.envSet")}
                        </Badge>
                      ) : null}
                    </div>
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
                      onCheckedChange={(value) => patchRow(row.id, { secret: value === true })}
                    />
                    <Label
                      htmlFor={`${row.id}-secret`}
                      className="text-[11px] font-normal text-muted-foreground"
                    >
                      {t("mcp.detail.envSecret")}
                    </Label>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      onClick={() =>
                        setRows((current) => current.filter((candidate) => candidate.id !== row.id))
                      }
                      aria-label={t("mcp.detail.envRemove", { key: row.key || "?" })}
                      title={t("mcp.detail.envRemove", { key: row.key || "?" })}
                    >
                      <Trash2 aria-hidden />
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          )}

          <div className="flex flex-wrap items-center justify-between gap-2">
            <Button type="button" variant="outline" size="xs" onClick={addRow}>
              <Plus aria-hidden />
              {t("mcp.detail.envAdd")}
            </Button>
            <p className="text-[11px] text-muted-foreground">{t("mcp.detail.envValueHint")}</p>
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
              {pending ? <Loader2 aria-hidden className="animate-spin motion-reduce:animate-none" /> : null}
              {pending ? t("common.saving") : t("common.save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
