import { useEffect, useMemo, useRef, useState } from "react";
import { FileText, Loader2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { AssistantProposal, EnvInput } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { CopyButton } from "@/components/copy-button";
import { ErrorAlert } from "@/components/error-alert";
import { LineDiff } from "@/components/diff-view";
import { announceAssistantApply } from "@/lib/assistant";
import { diffLines } from "@/lib/diff";

/** File the console already loaded for this path, so no refetch is needed. */
export interface LoadedProposalFile {
  path: string;
  content: string;
  etag: string | null;
}

interface BaseFile {
  content: string;
  etag: string | null;
}

const KIND_LABEL: Record<AssistantProposal["kind"], string> = {
  file: "File",
  env: "Environment",
  yaml: "YAML",
};

function proposalTitle(proposal: AssistantProposal): string {
  if (proposal.kind === "env") return "Environment variables";
  if (proposal.path) return proposal.path;
  return "config.yaml";
}

/**
 * One assistant answer that can be applied. A file proposal shows a real diff
 * against the bytes on disk (or the open buffer) and is written with the ETag
 * that was loaded, so an edit that landed in between is a visible conflict
 * instead of a silent overwrite. Nothing is written until Apply is pressed.
 */
export function AssistantProposalCard({
  proposal,
  instance,
  canEdit,
  loaded,
}: {
  proposal: AssistantProposal;
  instance?: string;
  canEdit: boolean;
  loaded?: LoadedProposalFile | null;
}) {
  const [base, setBase] = useState<BaseFile | null>(null);
  const [baseError, setBaseError] = useState<ApiError | null>(null);
  const [applying, setApplying] = useState(false);
  const [applyError, setApplyError] = useState<ApiError | null>(null);
  const [applied, setApplied] = useState(false);
  const [rejected, setRejected] = useState(false);
  // The diff is frozen against the version the proposal was written for: once
  // applied, the card keeps showing what changed instead of collapsing to zero.
  const loadedRef = useRef(loaded);
  loadedRef.current = loaded;

  const target = proposal.path;
  const hasContent = proposal.content !== undefined;
  const copyValue =
    proposal.kind === "env"
      ? (proposal.env ?? []).map((variable) => `${variable.key}=${variable.value ?? ""}`).join("\n")
      : (proposal.content ?? "");

  useEffect(() => {
    if (proposal.kind === "env" || !target) {
      setBase(null);
      setBaseError(null);
      return;
    }
    const current = loadedRef.current;
    if (current && current.path === target) {
      setBase({ content: current.content, etag: current.etag });
      setBaseError(null);
      return;
    }
    if (!instance) {
      // No instance in the context: show the patch without a base. Apply stays
      // disabled, and the card still explains where the file lives.
      setBase({ content: "", etag: null });
      setBaseError(null);
      return;
    }
    let alive = true;
    setBase(null);
    setBaseError(null);
    api.readFile(instance, target).then(
      (file) => {
        if (alive) setBase({ content: file.content, etag: file.etag });
      },
      (cause: unknown) => {
        if (!alive) return;
        const error = toApiError(cause);
        // A file that does not exist yet is a normal proposal: everything is
        // an addition and the write creates it.
        if (error.status === 404) setBase({ content: "", etag: null });
        else setBaseError(error);
      },
    );
    return () => {
      alive = false;
    };
    // Loaded once: later edits must not rewrite the diff the operator is reading.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instance, proposal.kind, target]);

  const diff = useMemo(() => {
    if (proposal.kind === "env" || proposal.content === undefined) return null;
    if (proposal.kind === "yaml" && !target) return null;
    return diffLines(base?.content ?? "", proposal.content);
  }, [base, proposal.kind, proposal.content, target]);

  const envVars = proposal.env ?? [];
  const isNewFile = target !== undefined && instance !== undefined && base !== null && base.etag === null;
  const busy = applying || (proposal.kind !== "env" && base === null);

  async function applyEnv(): Promise<void> {
    if (!instance) return;
    const detail = await api.getInstance(instance);
    const existing = new Set(detail.env.map((entry) => entry.key));
    // Only new keys are written: an existing variable is never blanked or
    // reflagged by a proposal that could not see its value.
    const additions: EnvInput[] = envVars
      .filter((variable) => !existing.has(variable.key))
      .map((variable) => ({
        key: variable.key,
        value: variable.value ?? "",
        secret: variable.secret === true,
      }));
    if (additions.length === 0) {
      toast.info("Nothing to add: every variable in this proposal already exists.");
      return;
    }
    await api.patchInstance(instance, { env: additions }, { viaAssistant: true });
    toast.success(
      `Added ${additions.length} ${additions.length === 1 ? "variable" : "variables"} to ${instance}.`,
      {
        description: "Fill their values in Configuration. Existing variables were left untouched.",
      },
    );
    setApplied(true);
    announceAssistantApply({ kind: "env", instance });
  }

  async function applyFile(): Promise<void> {
    if (!instance || !target || proposal.content === undefined) return;
    const result = await api.writeFile(
      instance,
      { path: target, content: proposal.content, etag: base?.etag ?? null },
      { viaAssistant: true },
    );
    toast.success(`Applied ${target}.`, {
      description: result.backup
        ? `Recorded as a new commit. Previous version: ${result.backup}`
        : "Recorded as a new commit.",
    });
    setApplied(true);
    announceAssistantApply({ kind: "file", instance, path: target });
  }

  async function apply(): Promise<void> {
    if (applying) return;
    setApplying(true);
    setApplyError(null);
    try {
      if (proposal.kind === "env") await applyEnv();
      else await applyFile();
    } catch (cause) {
      setApplyError(toApiError(cause));
    } finally {
      setApplying(false);
    }
  }

  function reject(): void {
    setRejected(true);
    setApplyError(null);
  }

  const applyBlocked =
    !canEdit ||
    !instance ||
    rejected ||
    applied ||
    (proposal.kind === "env" ? envVars.length === 0 : !target || !hasContent || busy);

  return (
    <div className="space-y-2 rounded-md border bg-muted/20 p-3">
      <div className="flex flex-wrap items-center gap-2">
        <FileText aria-hidden className="size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate font-mono text-xs">{proposalTitle(proposal)}</span>
        <Badge variant="outline" className="font-normal">
          {KIND_LABEL[proposal.kind]}
        </Badge>
        {isNewFile && target ? (
          <Badge variant="outline" className="font-normal">
            New file
          </Badge>
        ) : null}
        {applied ? <Badge className="font-normal">Applied</Badge> : null}
        {rejected ? (
          <Badge variant="outline" className="font-normal">
            Rejected
          </Badge>
        ) : null}
      </div>

      {proposal.kind === "env" ? (
        <div className="overflow-hidden rounded-md border bg-background">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Variable</TableHead>
                <TableHead>Value</TableHead>
                <TableHead className="w-24">Stored as</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {envVars.map((variable) => (
                <TableRow key={variable.key}>
                  <TableCell className="font-mono text-xs">{variable.key}</TableCell>
                  <TableCell className="max-w-40 truncate font-mono text-xs">
                    {variable.value !== undefined && variable.value !== "" ? (
                      <span title={variable.value}>{variable.value}</span>
                    ) : (
                      <span className="text-muted-foreground">—</span>
                    )}
                  </TableCell>
                  <TableCell>
                    {variable.secret ? (
                      <Badge variant="outline" className="font-normal">
                        secret
                      </Badge>
                    ) : (
                      <span className="text-xs text-muted-foreground">plain</span>
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          <p className="border-t px-3 py-2 text-[11px] text-muted-foreground">
            Values are proposed for plain variables only: fill every secret in the Configuration
            tab after applying. A variable that already exists is left untouched.
          </p>
        </div>
      ) : proposal.kind === "yaml" && !target ? (
        <div className="space-y-1">
          <pre className="max-h-64 overflow-auto rounded-md border bg-background p-3 font-mono text-xs leading-5">
            {proposal.content ?? ""}
          </pre>
          <p className="text-[11px] text-muted-foreground">
            config.yaml is rendered by fleett: copy the fields you need into the configuration form.
          </p>
        </div>
      ) : baseError ? (
        <ErrorAlert error={baseError} />
      ) : base === null ? (
        <p className="flex items-center gap-2 px-1 py-3 text-xs text-muted-foreground" aria-live="polite">
          <Loader2 aria-hidden className="size-3 animate-spin" />
          Loading the current file…
        </p>
      ) : diff ? (
        <div className="overflow-hidden rounded-md border bg-background">
          <div className="flex items-center justify-between gap-2 border-b px-3 py-1.5 text-[11px] text-muted-foreground">
            <span>Proposed changes</span>
            <span className="font-mono">
              +{diff.added} −{diff.removed}
            </span>
          </div>
          <LineDiff hunks={diff.hunks} empty="The proposed file is identical to the one on disk." />
          {diff.coarse ? (
            <p className="border-t px-3 py-1.5 text-[11px] text-muted-foreground">
              The file is too large to align line by line: the diff shows a full replacement.
            </p>
          ) : null}
        </div>
      ) : null}

      {applyError ? <ErrorAlert error={applyError} /> : null}
      {!instance ? (
        <p className="text-[11px] text-muted-foreground">
          Open an instance to apply this proposal.
        </p>
      ) : null}

      <div className="flex flex-wrap items-center gap-2">
        {rejected ? (
          <span className="text-xs text-muted-foreground">Rejected. Nothing was changed.</span>
        ) : applied ? null : (
          <Button
            size="sm"
            onClick={() => void apply()}
            disabled={applyBlocked}
            title={
              !canEdit
                ? "This account is read-only."
                : !instance
                  ? "Open an instance first."
                  : undefined
            }
          >
            {applying ? <Loader2 aria-hidden className="animate-spin" /> : null}
            {applying ? "Applying…" : "Apply"}
          </Button>
        )}
        <CopyButton
          value={copyValue}
          label="Copy proposal"
          showLabel
          size="sm"
          variant="outline"
          disabled={copyValue.length === 0}
        />
        {!rejected && !applied ? (
          <Button size="sm" variant="ghost" onClick={reject} disabled={applying}>
            Reject
          </Button>
        ) : null}
      </div>

      {applied ? (
        <p className="text-[11px] text-muted-foreground">
          Applied through the regular route: the write kept its backup, git commit and audit entry,
          marked as coming from the assistant.
        </p>
      ) : null}
    </div>
  );
}
