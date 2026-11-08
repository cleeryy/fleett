import { AlertCircle } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { CopyButton } from "@/components/copy-button";
import type { ApiError } from "@/api/errors";
import { cn } from "@/lib/utils";

const EGRESS_COMMAND = "sudo fleett egress install";

/**
 * One error surface for the whole console. 502 is a Docker failure: show the
 * raw compose message in monospace instead of hiding it behind generic copy.
 * 409 `egress_not_enforced` is a prerequisite, not a bug: spell out the exact
 * command that unblocks it.
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
  const mono = !egress && (docker || error.message.length > 120);

  return (
    <Alert variant="destructive" className={cn("items-start", className)}>
      <AlertCircle aria-hidden className="mt-0.5 size-4" />
      <AlertTitle>
        {egress
          ? "LAN isolation is not enforced"
          : docker
            ? "Docker operation failed"
            : "Request failed"}
      </AlertTitle>
      <AlertDescription>
        <p className={cn("break-words", mono && "font-mono text-xs whitespace-pre-wrap")}>
          {error.message}
        </p>

        {egress ? (
          <div className="mt-1 w-full space-y-2">
            <p className="text-xs">Run this on the host, then retry:</p>
            <div className="flex items-center gap-2 rounded-md border border-destructive/30 px-2 py-1.5">
              <code className="min-w-0 flex-1 font-mono text-xs break-all text-foreground">
                {EGRESS_COMMAND}
              </code>
              <CopyButton
                value={EGRESS_COMMAND}
                label="Copy command"
                size="icon-xs"
                className="shrink-0"
              />
            </div>
          </div>
        ) : null}

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
