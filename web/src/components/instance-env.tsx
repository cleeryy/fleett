import { useEffect, useRef, useState, type ChangeEvent, type ComponentProps } from "react";
import { Check, Loader2, Pencil, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { EnvSuggestion, InstanceDetail } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Section } from "@/components/data-list";
import { ErrorAlert } from "@/components/error-alert";
import { formatDateTime, formatRelative } from "@/lib/format";
import {
  envPayload,
  envRowsFromDetail,
  envRowsMatch,
  newEnvRow,
  validateEnvRows,
  type EnvRow,
} from "@/lib/env";

/**
 * The environment of one instance, edited as a list and written with a single
 * full replace (`PUT /api/instances/{id}/env`). Every apply recreates the
 * container, so the consequence is spelled out next to the button.
 */
export function InstanceEnvironment({
  detail,
  rows,
  suggestions = [],
  onRowsChange,
  onApplied,
}: {
  detail: InstanceDetail;
  rows: EnvRow[];
  /** Template suggestions rendered as one-click additions. */
  suggestions?: EnvSuggestion[];
  onRowsChange: (rows: EnvRow[]) => void;
  onApplied: () => void;
}) {
  const [editingId, setEditingId] = useState<number | null>(null);
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const focusId = useRef<string | null>(null);
  const alertRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!focusId.current) return;
    const node = document.getElementById(focusId.current);
    focusId.current = null;
    node?.focus();
  });

  const errors = attempted ? validateEnvRows(rows, "apply", suggestions).rows : {};
  const dirty = !envRowsMatch(detail.env, rows);
  const suggestionByKey = new Map(suggestions.map((suggestion) => [suggestion.key, suggestion]));
  const available = suggestions.filter(
    (suggestion) => !rows.some((row) => row.key.trim() === suggestion.key),
  );

  function patch(id: number, changes: Partial<EnvRow>) {
    onRowsChange(rows.map((row) => (row.id === id ? { ...row, ...changes } : row)));
  }

  function addRow(seed: { key?: string; value?: string; secret?: boolean } = {}) {
    const row = newEnvRow(seed);
    onRowsChange([...rows, row]);
    setEditingId(row.id);
    focusId.current = `cfg-env-${row.id}-${seed.key ? "value" : "key"}`;
  }

  function editRow(row: EnvRow) {
    setEditingId(row.id);
    focusId.current = `cfg-env-${row.id}-${row.storedKey ? "value" : "key"}`;
  }

  function removeRow(row: EnvRow) {
    onRowsChange(rows.filter((candidate) => candidate.id !== row.id));
    if (editingId === row.id) setEditingId(null);
  }

  function doneRow(row: EnvRow) {
    const rowErrors = validateEnvRows(rows, "apply", suggestions).rows[row.id];
    if (rowErrors) {
      setAttempted(true);
      const suffix = rowErrors.key ? "key" : "value";
      document.getElementById(`cfg-env-${row.id}-${suffix}`)?.focus();
      return;
    }
    setEditingId(null);
  }

  async function apply() {
    setAttempted(true);
    const { rows: rowErrors, count } = validateEnvRows(rows, "apply", suggestions);
    if (count > 0) {
      const [id, rowErrorsForRow] = Object.entries(rowErrors)[0] ?? [];
      if (id && rowErrorsForRow) {
        // Open the offending row: an invalid row may be shown collapsed.
        setEditingId(Number(id));
        focusId.current = `cfg-env-${id}-${rowErrorsForRow.key ? "key" : "value"}`;
      }
      return;
    }

    setPending(true);
    setError(null);
    try {
      await api.putInstanceEnv(detail.id, envPayload(rows, "apply"));
      toast.success("Environment applied. The container was recreated.");
      setEditingId(null);
      setAttempted(false);
      onApplied();
    } catch (cause) {
      setError(toApiError(cause));
      window.requestAnimationFrame(() => alertRef.current?.scrollIntoView({ block: "nearest" }));
    } finally {
      setPending(false);
    }
  }

  function discard() {
    onRowsChange(envRowsFromDetail(detail.env));
    setEditingId(null);
    setAttempted(false);
    setError(null);
  }

  function rowInput(
    row: EnvRow,
    field: "key" | "value",
  ): ComponentProps<typeof Input> & { id: string } {
    const rowErrors = errors[row.id];
    const name = row.key.trim() || "variable";
    const hint = suggestionByKey.get(row.key.trim());
    return {
      id: `cfg-env-${row.id}-${field}`,
      value: row[field],
      autoComplete: "off",
      spellCheck: false,
      className: "font-mono text-xs",
      "aria-label": `${name} ${field === "key" ? "name" : "value"}`,
      "aria-invalid": !!rowErrors?.[field],
      "aria-describedby": rowErrors?.[field] ? `cfg-env-${row.id}-${field}-error` : undefined,
      onChange: (event: ChangeEvent<HTMLInputElement>) =>
        field === "key"
          ? patch(row.id, { key: event.target.value })
          : patch(row.id, { value: event.target.value }),
      ...(field === "key"
        ? {
            placeholder: "VARIABLE_NAME",
            disabled: row.storedKey !== null,
          }
        : {
            type: row.secret ? "password" : "text",
            placeholder:
              row.secret && row.storedSecret
                ? "Leave empty to keep"
                : row.secret
                  ? "Secret value"
                  : hint?.example
                    ? `e.g. ${hint.example}`
                    : "Value",
          }),
    };
  }

  return (
    <Section
      title="Environment"
      description="Variables the agent receives. Secret values are encrypted at rest and never returned."
      action={
        <Button type="button" variant="outline" size="sm" onClick={() => addRow()}>
          <Plus aria-hidden />
          Add variable
        </Button>
      }
    >
      <div className="space-y-3">
        <div className="rounded-md border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-[38%]">Key</TableHead>
                <TableHead>Value</TableHead>
                <TableHead className="hidden w-[130px] sm:table-cell">Updated</TableHead>
                <TableHead className="w-[92px]">
                  <span className="sr-only">Actions</span>
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.length === 0 ? (
                <TableRow>
                  <TableCell colSpan={4} className="py-8 text-center text-sm whitespace-normal text-muted-foreground">
                    No variables. Add an allowlist or a token.
                  </TableCell>
                </TableRow>
              ) : (
                rows.map((row) => {
                  const rowErrors = errors[row.id];
                  const name = row.key.trim() || "variable";
                  if (editingId === row.id) {
                    return (
                      <TableRow key={row.id} className="hover:bg-transparent">
                        <TableCell colSpan={4} className="py-3 whitespace-normal">
                          <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)_auto_auto_auto] sm:items-center">
                            {row.storedKey !== null ? (
                              <div className="flex h-9 min-w-0 items-center px-1">
                                <span className="truncate font-mono text-xs" title={row.key}>
                                  {row.key}
                                </span>
                              </div>
                            ) : (
                              <Input {...rowInput(row, "key")} />
                            )}
                            <Input {...rowInput(row, "value")} />
                            <div className="flex h-9 items-center gap-2 sm:px-1">
                              <Checkbox
                                id={`cfg-env-${row.id}-secret`}
                                checked={row.secret}
                                onCheckedChange={(checked) => patch(row.id, { secret: checked === true })}
                              />
                              <Label
                                htmlFor={`cfg-env-${row.id}-secret`}
                                className="cursor-pointer text-xs font-normal text-muted-foreground"
                              >
                                Secret
                              </Label>
                            </div>
                            <Button
                              type="button"
                              variant="outline"
                              size="sm"
                              onClick={() => doneRow(row)}
                            >
                              <Check aria-hidden />
                              Done
                            </Button>
                            <Button
                              type="button"
                              variant="ghost"
                              size="icon-sm"
                              aria-label={`Remove ${name}`}
                              title={`Remove ${name}`}
                              onClick={() => removeRow(row)}
                            >
                              <Trash2 aria-hidden />
                            </Button>
                          </div>
                          {rowErrors?.key ? (
                            <p id={`cfg-env-${row.id}-key-error`} className="mt-1 text-xs text-destructive">
                              {rowErrors.key}
                            </p>
                          ) : null}
                          {rowErrors?.value ? (
                            <p id={`cfg-env-${row.id}-value-error`} className="mt-1 text-xs text-destructive">
                              {rowErrors.value}
                            </p>
                          ) : null}
                        </TableCell>
                      </TableRow>
                    );
                  }

                  const updated = row.storedKey
                    ? detail.env.find((entry) => entry.key === row.storedKey)?.updated_at
                    : undefined;
                  return (
                    <TableRow key={row.id}>
                      <TableCell className="whitespace-normal">
                        <span className="font-mono text-xs">{row.key}</span>
                        {row.storedKey === null ? (
                          <Badge variant="outline" className="ml-2 text-[11px] font-normal">
                            new
                          </Badge>
                        ) : null}
                      </TableCell>
                      <TableCell className="whitespace-normal">
                        {row.secret ? (
                          <span className="flex flex-wrap items-center gap-2">
                            <span aria-hidden className="font-mono text-xs tracking-widest">
                              ••••
                            </span>
                            <span className="sr-only">Secret value hidden</span>
                            <Badge variant="outline" className="text-[11px] font-normal">
                              secret
                            </Badge>
                            {row.value ? (
                              <span className="text-[11px] text-muted-foreground">
                                {row.storedSecret ? "replacement pending" : "value pending"}
                              </span>
                            ) : null}
                          </span>
                        ) : row.value ? (
                          <span
                            className="block max-w-[28ch] truncate font-mono text-xs"
                            title={row.value}
                          >
                            {row.value}
                          </span>
                        ) : (
                          <span className="text-xs text-muted-foreground">empty</span>
                        )}
                      </TableCell>
                      <TableCell
                        className="hidden text-xs text-muted-foreground sm:table-cell"
                        title={updated ? formatDateTime(updated) : undefined}
                      >
                        {updated ? formatRelative(updated) : "Not saved"}
                      </TableCell>
                      <TableCell>
                        <div className="flex justify-end gap-1">
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon-sm"
                            aria-label={`Edit ${name}`}
                            title={`Edit ${name}`}
                            onClick={() => editRow(row)}
                          >
                            <Pencil aria-hidden />
                          </Button>
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon-sm"
                            aria-label={`Remove ${name}`}
                            title={`Remove ${name}`}
                            onClick={() => removeRow(row)}
                          >
                            <Trash2 aria-hidden />
                          </Button>
                        </div>
                      </TableCell>
                    </TableRow>
                  );
                })
              )}
            </TableBody>
          </Table>
        </div>

        {available.length > 0 ? (
          <div className="space-y-2 rounded-md border border-dashed p-3">
            <p className="text-xs font-medium text-muted-foreground">
              Suggested by the template
            </p>
            <div className="grid gap-1.5">
              {available.map((suggestion) => (
                <button
                  key={suggestion.key}
                  type="button"
                  onClick={() =>
                    addRow({ key: suggestion.key, secret: suggestion.secret ?? false })
                  }
                  className="group flex w-full items-start gap-2 rounded-md border px-3 py-2 text-left transition-colors outline-none hover:bg-muted/50 focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50"
                >
                  <Plus aria-hidden className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 space-y-0.5">
                    <span className="flex flex-wrap items-center gap-2">
                      <span className="font-mono text-xs">{suggestion.key}</span>
                      {suggestion.required ? (
                        <Badge
                          variant="outline"
                          className="border-amber-600/40 text-[11px] font-normal text-amber-700 dark:text-amber-400"
                        >
                          required
                        </Badge>
                      ) : null}
                      {suggestion.secret ? (
                        <Badge variant="outline" className="text-[11px] font-normal">
                          secret
                        </Badge>
                      ) : null}
                    </span>
                    {suggestion.description ? (
                      <span className="block text-xs text-muted-foreground">{suggestion.description}</span>
                    ) : null}
                    {suggestion.example ? (
                      <span className="block font-mono text-[11px] text-muted-foreground">
                        example: {suggestion.example}
                      </span>
                    ) : null}
                  </span>
                </button>
              ))}
            </div>
          </div>
        ) : null}

        {error ? (
          <div ref={alertRef}>
            <ErrorAlert error={error} />
          </div>
        ) : null}

        <div className="flex flex-wrap items-center gap-2 border-t pt-4">
          <Button type="button" onClick={apply} disabled={!dirty || pending}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
            Apply environment
          </Button>
          <Button type="button" variant="outline" disabled={!dirty || pending} onClick={discard}>
            Discard
          </Button>
          <p className="text-xs text-muted-foreground sm:ml-auto sm:max-w-md sm:text-right">
            Apply replaces the whole list and runs{" "}
            <span className="font-mono">docker compose up -d</span>: removed rows are deleted and the
            container is recreated, so the agent is briefly offline.
          </p>
        </div>
      </div>
    </Section>
  );
}
