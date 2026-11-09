import { useEffect, useRef } from "react";
import { Plus, Trash2 } from "lucide-react";
import type { EnvSuggestion } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { newEnvRow, type EnvRow, type EnvRowErrors } from "@/lib/env";

/**
 * Row editor used by the create wizard. Values of secret rows are write-only:
 * the controller never returns them, so a stored secret shows as an empty
 * field that means "keep it".
 */
export function EnvironmentRows({
  idPrefix,
  rows,
  onChange,
  errors,
  suggestions = [],
  templateName,
}: {
  idPrefix: string;
  rows: EnvRow[];
  onChange: (rows: EnvRow[]) => void;
  errors: Record<number, EnvRowErrors>;
  suggestions?: EnvSuggestion[];
  templateName?: string;
}) {
  const focusId = useRef<string | null>(null);

  useEffect(() => {
    if (!focusId.current) return;
    const node = document.getElementById(focusId.current);
    focusId.current = null;
    node?.focus();
  });

  function patch(id: number, changes: Partial<EnvRow>) {
    onChange(rows.map((row) => (row.id === id ? { ...row, ...changes } : row)));
  }

  function addRow(seed: { key?: string; secret?: boolean } = {}, focus: "key" | "value" = "key") {
    const row = newEnvRow(seed);
    onChange([...rows, row]);
    focusId.current = `${idPrefix}-${row.id}-${focus}`;
  }

  function removeRow(id: number) {
    onChange(rows.filter((row) => row.id !== id));
  }

  return (
    <div className="space-y-3 rounded-md border p-4">
      <div>
        <p className="text-sm font-medium">Environment</p>
        <p className="text-xs text-muted-foreground">
          Extra variables passed to the agent, such as allowlists. Secrets are encrypted at rest and
          never returned by the API.
        </p>
      </div>

      {rows.length > 0 ? (
        <div className="space-y-3">
          {rows.map((row, index) => {
            const rowErrors = errors[row.id];
            const name = row.key.trim() || `variable ${index + 1}`;
            return (
              <div key={row.id} className="space-y-1">
                <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)_auto_auto] sm:items-center">
                  <Input
                    id={`${idPrefix}-${row.id}-key`}
                    value={row.key}
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono text-xs"
                    placeholder="VARIABLE_NAME"
                    aria-label={`${name} name`}
                    aria-invalid={!!rowErrors?.key}
                    aria-describedby={rowErrors?.key ? `${idPrefix}-${row.id}-key-error` : undefined}
                    onChange={(event) => patch(row.id, { key: event.target.value })}
                  />
                  <Input
                    id={`${idPrefix}-${row.id}-value`}
                    type={row.secret ? "password" : "text"}
                    value={row.value}
                    autoComplete="off"
                    spellCheck={false}
                    className="font-mono text-xs"
                    placeholder={
                      row.secret && row.storedSecret
                        ? "Leave empty to keep the stored secret"
                        : row.secret
                          ? "Secret value"
                          : "Value"
                    }
                    aria-label={`${name} value`}
                    aria-invalid={!!rowErrors?.value}
                    aria-describedby={rowErrors?.value ? `${idPrefix}-${row.id}-value-error` : undefined}
                    onChange={(event) => patch(row.id, { value: event.target.value })}
                  />
                  <div className="flex h-9 items-center gap-2 sm:px-1">
                    <Checkbox
                      id={`${idPrefix}-${row.id}-secret`}
                      checked={row.secret}
                      onCheckedChange={(checked) => patch(row.id, { secret: checked === true })}
                    />
                    <Label
                      htmlFor={`${idPrefix}-${row.id}-secret`}
                      className="cursor-pointer text-xs font-normal text-muted-foreground"
                    >
                      Secret
                    </Label>
                  </div>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={`Remove ${name}`}
                    title={`Remove ${name}`}
                    onClick={() => removeRow(row.id)}
                  >
                    <Trash2 aria-hidden />
                  </Button>
                </div>
                {rowErrors?.key ? (
                  <p id={`${idPrefix}-${row.id}-key-error`} className="text-xs text-destructive">
                    {rowErrors.key}
                  </p>
                ) : null}
                {rowErrors?.value ? (
                  <p id={`${idPrefix}-${row.id}-value-error`} className="text-xs text-destructive">
                    {rowErrors.value}
                  </p>
                ) : null}
              </div>
            );
          })}
        </div>
      ) : (
        <p className="rounded-md border border-dashed px-3 py-4 text-xs text-muted-foreground">
          No variables. Add one below, or pick a suggestion.
        </p>
      )}

      <Button type="button" variant="outline" size="sm" onClick={() => addRow()}>
        <Plus aria-hidden />
        Add variable
      </Button>

      {suggestions.length > 0 ? (
        <div className="space-y-2 border-t pt-3">
          <p className="text-xs font-medium text-muted-foreground">
            Suggested by {templateName ? <span className="font-mono">{templateName}</span> : "this template"}
          </p>
          <div className="grid gap-1.5">
            {suggestions.map((suggestion) => (
              <button
                key={suggestion.key}
                type="button"
                onClick={() => addRow({ key: suggestion.key, secret: suggestion.secret ?? false }, "value")}
                className="group flex w-full items-start gap-2 rounded-md border px-3 py-2 text-left transition-colors outline-none hover:bg-muted/50 focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
              >
                <Plus aria-hidden className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
                <span className="min-w-0 flex-1 space-y-0.5">
                  <span className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-xs">{suggestion.key}</span>
                    {suggestion.secret ? (
                      <Badge variant="outline" className="text-[11px] font-normal">
                        secret
                      </Badge>
                    ) : null}
                    <span className="text-[11px] text-muted-foreground group-hover:text-foreground">Add</span>
                  </span>
                  {suggestion.description ? (
                    <span className="block text-xs text-muted-foreground">{suggestion.description}</span>
                  ) : null}
                </span>
              </button>
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}
