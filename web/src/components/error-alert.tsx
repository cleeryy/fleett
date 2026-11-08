import { AlertCircle } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import type { ApiError } from "@/api/errors";
import { cn } from "@/lib/utils";

/**
 * One error surface for the whole console. 502 is a Docker failure: show the
 * raw compose message in monospace instead of hiding it behind generic copy.
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
  const mono = docker || error.message.length > 120;

  return (
    <Alert variant="destructive" className={cn("items-start", className)}>
      <AlertCircle aria-hidden className="mt-0.5 size-4" />
      <AlertTitle>{docker ? "Docker operation failed" : "Request failed"}</AlertTitle>
      <AlertDescription>
        <p className={cn("break-words", mono && "font-mono text-xs whitespace-pre-wrap")}>
          {error.message}
        </p>
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
