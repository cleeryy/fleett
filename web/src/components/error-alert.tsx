import { AlertCircle } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { CopyButton } from "@/components/copy-button";
import type { ApiError } from "@/api/errors";
import { cn } from "@/lib/utils";

const EGRESS_COMMAND = "sudo fleett egress install";
const SECRETS_COMMAND = "sudo fleett secrets init";

function FixCommand({ command }: { command: string }) {
  return (
    <div className="mt-1 w-full space-y-2">
      <p className="text-xs">Run this on the host, then retry:</p>
      <div className="flex items-center gap-2 rounded-md border border-destructive/30 px-2 py-1.5">
        <code className="min-w-0 flex-1 font-mono text-xs break-all text-foreground">{command}</code>
        <CopyButton value={command} label="Copy command" size="icon-xs" className="shrink-0" />
      </div>
    </div>
  );
}

/**
 * One error surface for the whole console. 502 is a Docker failure: show the
 * raw compose message in monospace instead of hiding it behind generic copy.
 * 409 `egress_not_enforced` and `secrets_key_missing` are host prerequisites,
 * not bugs: spell out the exact command that unblocks them.
 */
export function ErrorAlert({
  error,
  onRetry,
  className,
}: {
  error: ApiError;
  onRetry?: () => void;
  className?: string;
}) {
  const docker = error.status === 502;
  const egress = error.code === "egress_not_enforced";
  const secrets = error.code === "secrets_key_missing";
  const mono = !egress && !secrets && (docker || error.message.length > 120);

  const title = egress
    ? "LAN isolation is not enforced"
    : secrets
      ? "Secrets encryption key missing"
      : docker
        ? "Docker operation failed"
        : error.status === 401
          ? "Not signed in"
          : error.status === 403
            ? "Change refused"
            : "Request failed";

  return (
    <Alert variant="destructive" className={cn("items-start", className)}>
      <AlertCircle aria-hidden className="mt-0.5 size-4" />
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>
        <p className={cn("break-words", mono && "font-mono text-xs whitespace-pre-wrap")}>
          {error.message}
        </p>

        {egress ? <FixCommand command={EGRESS_COMMAND} /> : null}
        {secrets ? <FixCommand command={SECRETS_COMMAND} /> : null}

        <p className="mt-1.5 font-mono text-[11px] opacity-70">
          {error.status > 0 ? `HTTP ${error.status}` : "network"} · {error.code}
        </p>
        {onRetry ? (
          <Button type="button" variant="outline" size="sm" className="mt-3" onClick={onRetry}>
            Retry
          </Button>
        ) : null}
      </AlertDescription>
    </Alert>
  );
}
