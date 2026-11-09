import { useEffect, useRef } from "react";
import { Plus, Trash2 } from "lucide-react";
import type { PersonaVariable } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { newVariableRow, type PersonaVariableRow } from "@/lib/personas";

/**
 * The values an instance (or a preview) supplies for a persona's variables.
 * Empty means "use the persona default"; the API applies it.
 */
export function PersonaVariableInputs({
  variables,
  values,
  onChange,
  idPrefix,
  disabled = false,
  className,
}: {
  variables: PersonaVariable[];
  values: Record<string, string>;
  onChange: (next: Record<string, string>) => void;
  idPrefix: string;
  disabled?: boolean;
  className?: string;
}) {
  if (variables.length === 0) {
    return (
      <div className={className}>
        <p className="rounded-md border px-3 py-2.5 text-xs text-muted-foreground">
          This persona takes no variables: the same text is used everywhere.
        </p>
      </div>
    );
  }

  return (
    <div className={className}>
      <div className="divide-y rounded-md border">
        {variables.map((variable) => {
          const id = `${idPrefix}-${variable.key}`;
          return (
            <div
              key={variable.key}
              className="grid gap-1.5 px-3 py-2.5"
            >
              <div className="min-w-0">
                <p className="font-mono text-xs">{"{{" + variable.key + "}}"}</p>
                <p className="text-xs text-muted-foreground">{variable.description}</p>
              </div>
              <Input
                id={id}
                value={values[variable.key] ?? ""}
                autoComplete="off"
                spellCheck={false}
                disabled={disabled}
                placeholder={
                  variable.default !== undefined && variable.default !== ""
                    ? `Default: ${variable.default}`
                    : "No default: a value is required for a clean render"
                }
                aria-label={`Value for ${variable.key}`}
                aria-describedby={`${id}-help`}
                onChange={(event) =>
                  onChange({ ...values, [variable.key]: event.target.value })
                }
              />
              <p id={`${id}-help`} className="sr-only">
                {variable.description} Leave empty to use the persona default.
              </p>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** Key / description / default editor used by the create dialog and the detail page. */
export function PersonaVariableEditor({
  rows,
  onChange,
  errors = {},
  idPrefix,
  disabled = false,
}: {
  rows: PersonaVariableRow[];
  onChange: (rows: PersonaVariableRow[]) => void;
  errors?: Record<string, { key?: string; description?: string }>;
  idPrefix: string;
  disabled?: boolean;
}) {
  const focusId = useRef<string | null>(null);

  useEffect(() => {
    if (!focusId.current) return;
    const node = document.getElementById(focusId.current);
    focusId.current = null;
    node?.focus();
  });

  function patch(id: string, changes: Partial<PersonaVariableRow>) {
    onChange(rows.map((row) => (row.id === id ? { ...row, ...changes } : row)));
  }

  function addRow() {
    const row = newVariableRow();
    onChange([...rows, row]);
    focusId.current = `${idPrefix}-${row.id}-key`;
  }

  return (
    <div className="space-y-3 rounded-md border p-4">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <p className="text-sm font-medium">Variables</p>
          <p className="text-xs text-muted-foreground">
            Placeholders written as <span className="font-mono">{"{{key}}"}</span> in SOUL.md:
            lowercase letters, digits and underscores, starting with a letter. The value is stored on
            each instance, never on the persona.
          </p>
        </div>
        <Button type="button" variant="outline" size="sm" onClick={addRow} disabled={disabled}>
          <Plus aria-hidden />
          Add variable
        </Button>
      </div>

      {rows.length === 0 ? (
        <p className="rounded-md border border-dashed px-3 py-4 text-xs text-muted-foreground">
          No variables. The SOUL.md is the same for every instance that uses this persona.
        </p>
      ) : (
        <div className="space-y-3">
          <div className="hidden gap-2 px-0 text-[11px] text-muted-foreground sm:grid sm:grid-cols-[minmax(0,180px)_minmax(0,1fr)_minmax(0,160px)_2.25rem]">
            <span>Key</span>
            <span>Description</span>
            <span>Default (optional)</span>
            <span className="sr-only">Remove</span>
          </div>
          {rows.map((row, index) => {
            const rowErrors = errors[row.id];
            const label = row.key.trim() || `variable ${index + 1}`;
            return (
              <div key={row.id} className="space-y-1">
                <div className="grid gap-2 sm:grid-cols-[minmax(0,180px)_minmax(0,1fr)_minmax(0,160px)_2.25rem] sm:items-start">
                  <Input
                    id={`${idPrefix}-${row.id}-key`}
                    value={row.key}
                    autoComplete="off"
                    spellCheck={false}
                    disabled={disabled}
                    className="font-mono text-xs"
                    placeholder="agent_name"
                    aria-label={`${label} key`}
                    aria-invalid={!!rowErrors?.key}
                    aria-describedby={rowErrors?.key ? `${idPrefix}-${row.id}-key-error` : undefined}
                    onChange={(event) => patch(row.id, { key: event.target.value })}
                  />
                  <Input
                    id={`${idPrefix}-${row.id}-description`}
                    value={row.description}
                    autoComplete="off"
                    disabled={disabled}
                    placeholder="What the variable is for"
                    aria-label={`${label} description`}
                    aria-invalid={!!rowErrors?.description}
                    aria-describedby={
                      rowErrors?.description ? `${idPrefix}-${row.id}-description-error` : undefined
                    }
                    onChange={(event) => patch(row.id, { description: event.target.value })}
                  />
                  <Input
                    id={`${idPrefix}-${row.id}-default`}
                    value={row.default}
                    autoComplete="off"
                    disabled={disabled}
                    placeholder="Used when empty"
                    aria-label={`${label} default value`}
                    onChange={(event) => patch(row.id, { default: event.target.value })}
                  />
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={`Remove ${label}`}
                    title={`Remove ${label}`}
                    disabled={disabled}
                    onClick={() => onChange(rows.filter((candidate) => candidate.id !== row.id))}
                  >
                    <Trash2 aria-hidden />
                  </Button>
                </div>
                {rowErrors?.key ? (
                  <p id={`${idPrefix}-${row.id}-key-error`} className="text-xs text-destructive">
                    {rowErrors.key}
                  </p>
                ) : null}
                {rowErrors?.description ? (
                  <p
                    id={`${idPrefix}-${row.id}-description-error`}
                    className="text-xs text-destructive"
                  >
                    {rowErrors.description}
                  </p>
                ) : null}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
