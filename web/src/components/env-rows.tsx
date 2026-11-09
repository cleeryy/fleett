import { useEffect, useRef } from "react";
import { Plus, Trash2 } from "lucide-react";
import type { EnvSuggestion } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RichText } from "@/components/rich-text";
import { newEnvRow, type EnvRow, type EnvRowErrors } from "@/lib/env";
import { useI18n } from "@/lib/i18n";

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
  hints,
  templateName,
}: {
  idPrefix: string;
  rows: EnvRow[];
  onChange: (rows: EnvRow[]) => void;
  errors: Record<number, EnvRowErrors>;
  suggestions?: EnvSuggestion[];
  /** Every suggestion of the template, including keys already added. */
  hints?: EnvSuggestion[];
  templateName?: string;
}) {
  const { t } = useI18n();
  const focusId = useRef<string | null>(null);
  const hintByKey = new Map((hints ?? suggestions).map((suggestion) => [suggestion.key, suggestion]));

  useEffect(() => {
    if (!focusId.current) return;
    const node = document.getElementById(focusId.current);
    focusId.current = null;
    node?.focus();
  });

  function patch(id: number, changes: Partial<EnvRow>) {
    onChange(rows.map((row) => (row.id === id ? { ...row, ...changes } : row)));
  }

  function addRow(seed: { key?: string; secret?: boolean; value?: string } = {}, focus: "key" | "value" = "key") {
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
        <p className="text-sm font-medium">{t("env.title")}</p>
        <p className="text-xs text-muted-foreground">{t("env.description")}</p>
      </div>

      {rows.length > 0 ? (
        <div className="space-y-3">
          {rows.map((row, index) => {
            const rowErrors = errors[row.id];
            const name = row.key.trim() || t("env.variableN", { index: index + 1 });
            const hint = hintByKey.get(row.key.trim());
            const example = hint?.example
              ? t("env.examplePrefix", { value: hint.example })
              : hint?.format
                ? t("env.formatPrefix", { value: hint.format })
                : undefined;
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
                    aria-label={t("env.nameAria", { name })}
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
                        ? t("env.keepSecret")
                        : row.secret
                          ? t("env.secretValue")
                          : hint?.example
                            ? t("env.examplePrefix", { value: hint.example })
                            : t("env.value")
                    }
                    aria-label={t("env.valueAria", { name })}
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
                      {t("env.secret")}
                    </Label>
                  </div>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("env.removeAria", { name })}
                    title={t("env.removeAria", { name })}
                    onClick={() => removeRow(row.id)}
                  >
                    <Trash2 aria-hidden />
                  </Button>
                </div>
                {hint?.required || example ? (
                  <p className="flex flex-wrap items-center gap-1.5 text-[11px] text-muted-foreground">
                    {hint?.required ? (
                      <Badge variant="outline" className="border-amber-600/40 font-normal text-amber-700 dark:text-amber-400">
                        {t("env.required")}
                      </Badge>
                    ) : null}
                    {example ? <span className="font-mono">{example}</span> : null}
                  </p>
                ) : null}
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
          {t("env.empty")}
        </p>
      )}

      <Button type="button" variant="outline" size="sm" onClick={() => addRow()}>
        <Plus aria-hidden />
        {t("env.add")}
      </Button>

      {suggestions.length > 0 ? (
        <div className="space-y-2 border-t pt-3">
          <p className="text-xs font-medium text-muted-foreground">
            {templateName ? (
              <RichText text={t("env.suggestedBy", { template: templateName })} />
            ) : (
              t("env.suggestedByTemplate")
            )}
          </p>
          <div className="grid gap-1.5">
            {suggestions.map((suggestion) => (
              <button
                key={suggestion.key}
                type="button"
                onClick={() =>
                  addRow(
                    { key: suggestion.key, secret: suggestion.secret ?? false },
                    "value",
                  )
                }
                className="group flex w-full items-start gap-2 rounded-md border px-3 py-2 text-left transition-colors outline-none hover:bg-muted/50 focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
              >
                <Plus aria-hidden className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
                <span className="min-w-0 flex-1 space-y-0.5">
                  <span className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-xs">{suggestion.key}</span>
                    {suggestion.required ? (
                      <Badge variant="outline" className="border-amber-600/40 text-[11px] font-normal text-amber-700 dark:text-amber-400">
                        {t("env.required")}
                      </Badge>
                    ) : null}
                    {suggestion.secret ? (
                      <Badge variant="outline" className="text-[11px] font-normal">
                        secret
                      </Badge>
                    ) : null}
                    <span className="text-[11px] text-muted-foreground group-hover:text-foreground">
                      {t("env.addShort")}
                    </span>
                  </span>
                  {suggestion.description ? (
                    <span className="block text-xs text-muted-foreground">{suggestion.description}</span>
                  ) : null}
                  {suggestion.example ? (
                    <span className="block font-mono text-[11px] text-muted-foreground">
                      {t("env.exampleLine", { value: suggestion.example })}
                    </span>
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
