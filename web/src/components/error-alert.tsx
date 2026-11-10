import { AlertCircle } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { CopyButton } from "@/components/copy-button";
import { apiErrorMessage, type ApiError } from "@/api/errors";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

const EGRESS_COMMAND = "sudo fleett egress install";
const SECRETS_COMMAND = "sudo fleett secrets init";

function FixCommand({ command }: { command: string }) {
  const { t } = useI18n();
  return (
    <div className="mt-1 w-full space-y-2">
      <p className="text-xs">{t("errors.runOnHost")}</p>
      <div className="flex items-center gap-2 rounded-md border border-destructive/30 px-2 py-1.5">
        <code className="min-w-0 flex-1 font-mono text-xs break-all text-foreground">{command}</code>
        <CopyButton
          value={command}
          label={t("common.copyCommand")}
          size="icon-xs"
          className="shrink-0"
        />
      </div>
    </div>
  );
}

/**
 * One error surface for the whole console. 502 is a Docker failure: the raw
 * compose output stays visible in monospace, under a translated lead-in.
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
  const { t } = useI18n();
  const upstream =
    error.code === "mcp_upstream" ||
    error.code === "marketplace_unavailable" ||
    error.code === "assistant_provider_error";
  const docker = error.status === 502 && !upstream;
  const egress = error.code === "egress_not_enforced";
  const secrets = error.code === "secrets_key_missing";
  const message = apiErrorMessage(error, t);
  // The compose stderr is the useful part of a driver failure; keep it verbatim.
  const detail = error.code === "driver_error" ? error.message : null;
  const mono = !egress && !secrets && (docker || message.length > 120);

  const title = upstream
    ? t("errors.title.upstream")
    : egress
      ? t("errors.title.egress")
      : secrets
        ? t("errors.title.secrets")
        : docker
          ? t("errors.title.docker")
          : error.status === 401
            ? t("errors.title.unauthorized")
            : error.status === 403
              ? t("errors.title.forbidden")
              : t("errors.title.generic");

  return (
    <Alert variant="destructive" className={cn("items-start", className)}>
      <AlertCircle aria-hidden className="mt-0.5 size-4" />
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>
        <p className={cn("break-words", mono && "font-mono text-xs whitespace-pre-wrap")}>
          {message}
        </p>
        {detail ? (
          <p className="mt-1 max-h-40 overflow-auto font-mono text-xs break-words whitespace-pre-wrap opacity-80">
            {detail}
          </p>
        ) : null}

        {egress ? <FixCommand command={EGRESS_COMMAND} /> : null}
        {secrets ? <FixCommand command={SECRETS_COMMAND} /> : null}

        <p className="mt-1.5 font-mono text-[11px] opacity-70">
          {error.status > 0 ? `HTTP ${error.status}` : t("errors.networkLabel")} · {error.code}
        </p>
        {onRetry ? (
          <Button type="button" variant="outline" size="sm" className="mt-3" onClick={onRetry}>
            {t("common.retry")}
          </Button>
        ) : null}
      </AlertDescription>
    </Alert>
  );
}
