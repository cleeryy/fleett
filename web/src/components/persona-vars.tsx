import { useEffect, useRef } from "react";
import { Plus, Trash2 } from "lucide-react";
import type { PersonaVariable } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { RichText } from "@/components/rich-text";
import { useI18n } from "@/lib/i18n";
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
  const { t } = useI18n();
  if (variables.length === 0) {
    return (
      <div className={className}>
        <p className="rounded-md border px-3 py-2.5 text-xs text-muted-foreground">
          {t("personas.vars.noVariables")}
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
                    ? t("personas.vars.defaultValue", { value: variable.default })
                    : t("personas.vars.noDefault")
                }
                aria-label={t("personas.vars.valueFor", { key: variable.key })}
                aria-describedby={`${id}-help`}
                onChange={(event) =>
                  onChange({ ...values, [variable.key]: event.target.value })
                }
              />
              <p id={`${id}-help`} className="sr-only">
                {t("personas.vars.help", { description: variable.description })}
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
  const { t } = useI18n();
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
          <p className="text-sm font-medium">{t("personas.vars.title")}</p>
          <p className="text-xs text-muted-foreground">
            <RichText text={t("personas.vars.editorHint")} />
          </p>
        </div>
        <Button type="button" variant="outline" size="sm" onClick={addRow} disabled={disabled}>
          <Plus aria-hidden />
          {t("env.add")}
        </Button>
      </div>

      {rows.length === 0 ? (
        <p className="rounded-md border border-dashed px-3 py-4 text-xs text-muted-foreground">
          {t("personas.vars.editorEmpty")}
        </p>
      ) : (
        <div className="space-y-3">
          <div className="hidden gap-2 px-0 text-[11px] text-muted-foreground sm:grid sm:grid-cols-[minmax(0,180px)_minmax(0,1fr)_minmax(0,160px)_2.25rem]">
            <span>{t("personas.vars.key")}</span>
            <span>{t("common.description")}</span>
            <span>{t("personas.vars.defaultOptional")}</span>
            <span className="sr-only">{t("common.remove")}</span>
          </div>
          {rows.map((row, index) => {
            const rowErrors = errors[row.id];
            const label = row.key.trim() || t("env.variableN", { index: index + 1 });
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
                    aria-label={t("personas.vars.keyAria", { name: label })}
                    aria-invalid={!!rowErrors?.key}
                    aria-describedby={rowErrors?.key ? `${idPrefix}-${row.id}-key-error` : undefined}
                    onChange={(event) => patch(row.id, { key: event.target.value })}
                  />
                  <Input
                    id={`${idPrefix}-${row.id}-description`}
                    value={row.description}
                    autoComplete="off"
                    disabled={disabled}
                    placeholder={t("personas.vars.descriptionPlaceholder")}
                    aria-label={t("personas.vars.descriptionAria", { name: label })}
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
                    placeholder={t("personas.vars.defaultPlaceholder")}
                    aria-label={t("personas.vars.defaultAria", { name: label })}
                    onChange={(event) => patch(row.id, { default: event.target.value })}
                  />
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("env.removeAria", { name: label })}
                    title={t("env.removeAria", { name: label })}
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
