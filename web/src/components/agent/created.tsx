import { CircleCheck } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import type { InstanceDetail } from "@/api/types";
import { agentTabHref } from "@/lib/agent-tabs";
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";

/** Discord client: the bot is already online, the operator just has to try it. */
const DISCORD_APP_URL = "https://discord.com/app";

/**
 * Success panel shown once after the creation wizard (`?created=1`). It never
 * dumps Compose or JSON: it names the next concrete action for the channel and
 * disappears for good once dismissed (per browser, per instance).
 */
export function AgentCreatedPanel({
  detail,
  onDismiss,
}: {
  detail: InstanceDetail;
  onDismiss: () => void;
}) {
  const { t } = useI18n();
  const path = `/instances/${detail.id}`;
  const configureHref = hrefFor(agentTabHref(path, { tab: "configure" }));
  const logsHref = hrefFor(agentTabHref(path, { tab: "activity", view: "logs" }));
  const allowedUsersHref = hrefFor(
    agentTabHref(
      path,
      { tab: "configure" },
      { focus: "env", env: detail.channel === "discord" ? "DISCORD_ALLOWED_USERS" : null },
    ),
  );

  return (
    <Alert className="border-emerald-600/40 bg-emerald-50/40 dark:bg-emerald-950/20">
      <CircleCheck aria-hidden className="text-emerald-600 dark:text-emerald-400" />
      <AlertTitle>{t("agent.created.title", { name: detail.name })}</AlertTitle>
      <AlertDescription>
        <p className="text-sm text-muted-foreground">{t("agent.created.description")}</p>
        <div className="mt-3 flex flex-wrap items-center gap-2">
          {detail.channel === "discord" ? (
            <>
              <Button asChild size="sm">
                <a href={DISCORD_APP_URL} target="_blank" rel="noreferrer">
                  {t("agent.created.discord")}
                </a>
              </Button>
              <Button asChild size="sm" variant="outline">
                <a href={allowedUsersHref}>{t("agent.created.allowedUsers")}</a>
              </Button>
            </>
          ) : detail.channel === "none" ? (
            <Button asChild size="sm" variant="outline">
              <a href={logsHref}>{t("agent.action.viewLogs")}</a>
            </Button>
          ) : (
            <>
              <Button asChild size="sm" variant="outline">
                <a href={configureHref}>{t("agent.action.configure")}</a>
              </Button>
              <Button asChild size="sm" variant="ghost">
                <a href={logsHref}>{t("agent.action.viewLogs")}</a>
              </Button>
            </>
          )}
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="text-muted-foreground sm:ml-auto"
            onClick={onDismiss}
          >
            {t("agent.created.dismiss")}
          </Button>
        </div>
      </AlertDescription>
    </Alert>
  );
}
