import { AlertCircle, CheckCircle2, TriangleAlert } from "lucide-react";
import { api } from "@/api";
import type {
  InstanceDetail,
  PersonaSummary,
  ProviderSummary,
  Template,
} from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { AgentCreatedPanel } from "@/components/agent/created";
import { DataList, Section } from "@/components/data-list";
import { ErrorAlert } from "@/components/error-alert";
import { PersonaStateBadge } from "@/components/persona-state-badge";
import { RichText } from "@/components/rich-text";
import { agentTabHref } from "@/lib/agent-tabs";
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";
import { useResource } from "@/lib/use-resource";

/**
 * Overview tab of the agent detail (#73): what the agent is, what it will do
 * next, what it said lately and what needs attention. Every action is a link
 * to an existing surface, never a new mutation.
 */
export function AgentOverviewPanel({
  detail,
  provider,
  persona,
  template,
  canEdit,
  created,
  envWarning,
  onDismissCreated,
  onReview,
}: {
  detail: InstanceDetail;
  provider: ProviderSummary | null;
  persona: PersonaSummary | null;
  template: Template | null;
  canEdit: boolean;
  created: boolean;
  /** At least one warning points at the environment allowlist. */
  envWarning: boolean;
  onDismissCreated: () => void;
  /** Open the configuration tab at the field that needs attention. */
  onReview: () => void;
}) {
  const { t, lang } = useI18n();
  const path = `/instances/${detail.id}`;
  const configureHref = hrefFor(agentTabHref(path, { tab: "configure" }));
  const logsHref = hrefFor(agentTabHref(path, { tab: "activity", view: "logs" }));
  const messagesHref = hrefFor(agentTabHref(path, { tab: "activity", view: "messages" }));
  const schedulesHref = hrefFor("/schedules");

  const schedules = useResource(() => api.listInstanceSchedules(detail.id), [detail.id]);
  const messages = useResource(
    () => api.instanceMessages(detail.id, { view: "all", limit: 5 }),
    [detail.id],
  );

  const upcoming = (schedules.data ?? [])
    .filter((schedule) => schedule.enabled && schedule.next_run_at)
    .sort(
      (left, right) =>
        new Date(left.next_run_at ?? 0).getTime() - new Date(right.next_run_at ?? 0).getTime(),
    )
    .slice(0, 3);

  const warnings = detail.warnings ?? [];

  return (
    <div className="space-y-6">
      {created ? <AgentCreatedPanel detail={detail} onDismiss={onDismissCreated} /> : null}

      <Section title={t("detail.warningsTitle")}>
        {detail.last_error ? (
          <Alert variant="destructive" className="items-start">
            <AlertCircle aria-hidden className="mt-0.5 size-4" />
            <AlertTitle>{t("detail.lastError")}</AlertTitle>
            <AlertDescription>
              <p className="font-mono text-xs break-words whitespace-pre-wrap">
                {detail.last_error}
              </p>
            </AlertDescription>
          </Alert>
        ) : null}
        {warnings.length > 0 ? (
          <Alert className="border-amber-600/40 bg-amber-50/40 dark:bg-amber-950/20">
            <TriangleAlert aria-hidden className="text-amber-600 dark:text-amber-400" />
            <AlertTitle>{t("agent.overview.problemsTitle")}</AlertTitle>
            <AlertDescription>
              <ul className="list-disc space-y-1 pl-4 text-sm text-muted-foreground">
                {warnings.map((warning) => (
                  <li key={warning}>{warning}</li>
                ))}
              </ul>
              {canEdit ? (
                <Button type="button" variant="outline" size="sm" className="mt-2" onClick={onReview}>
                  {envWarning ? t("detail.fixEnv") : t("detail.reviewConfig")}
                </Button>
              ) : null}
            </AlertDescription>
          </Alert>
        ) : null}
        {!detail.last_error && warnings.length === 0 ? (
          <p className="flex items-center gap-2 rounded-md border px-4 py-3 text-sm text-muted-foreground">
            <CheckCircle2 aria-hidden className="size-4 text-emerald-600 dark:text-emerald-400" />
            {t("agent.overview.noProblems")}
          </p>
        ) : null}
      </Section>

      <Section
        title={t("agent.overview.whatTitle")}
        description={t("agent.overview.whatDescription")}
        action={
          <div className="flex flex-wrap items-center gap-2">
            {canEdit ? (
              <Button asChild size="sm" variant="outline">
                <a href={configureHref}>{t("agent.action.personality")}</a>
              </Button>
            ) : null}
            {canEdit ? (
              <Button asChild size="sm" variant="ghost">
                <a href={hrefFor("/library?type=skills")}>{t("agent.action.addSkill")}</a>
              </Button>
            ) : null}
            <Button asChild size="sm" variant="ghost">
              <a href={logsHref}>{t("agent.action.viewLogs")}</a>
            </Button>
          </div>
        }
      >
        <DataList
          items={[
            {
              label: t("clone.personaLabel"),
              value: detail.persona ? (
                <span className="flex flex-wrap items-center gap-2">
                  {persona ? (
                    <a
                      href={hrefFor(`/personas/${persona.id}`)}
                      className="text-sm font-medium underline-offset-4 hover:underline"
                    >
                      {persona.name}
                    </a>
                  ) : (
                    <span className="font-mono text-xs">{detail.persona}</span>
                  )}
                  <PersonaStateBadge state={detail.persona_state} />
                </span>
              ) : (
                <span className="text-muted-foreground">{t("agent.overview.noPersona")}</span>
              ),
            },
            {
              label: t("agent.overview.template"),
              value: (
                <span className="space-y-0.5">
                  <span className="block text-sm">{template?.name ?? detail.template}</span>
                  <span className="block font-mono text-[11px] text-muted-foreground">
                    {detail.template}
                  </span>
                </span>
              ),
            },
            {
              label: t("fleet.model"),
              value: (
                <span className="block space-y-0.5">
                  <span className="block font-mono text-xs break-all">{detail.model}</span>
                  {provider ? (
                    <a
                      href={hrefFor(`/providers/${provider.id}`)}
                      className="text-xs text-muted-foreground underline-offset-4 hover:underline"
                    >
                      {provider.name}
                    </a>
                  ) : null}
                </span>
              ),
            },
            {
              label: t("fleet.channel"),
              value: (
                <span className="font-mono text-xs">{t(`channel.${detail.channel}`)}</span>
              ),
            },
          ]}
        />
      </Section>

      <div className="grid gap-6 lg:grid-cols-2">
        <Section
          title={t("agent.overview.nextRunsTitle")}
          action={
            <Button asChild variant="ghost" size="sm">
              <a href={schedulesHref}>{t("agent.overview.manageSchedules")}</a>
            </Button>
          }
        >
          {schedules.loading && !schedules.data ? (
            <Skeleton className="h-24 w-full rounded-md" />
          ) : schedules.error ? (
            <ErrorAlert error={schedules.error} onRetry={schedules.reload} />
          ) : upcoming.length ? (
            <ul className="divide-y rounded-md border">
              {upcoming.map((schedule) => (
                <li
                  key={schedule.id}
                  className="flex flex-wrap items-baseline justify-between gap-2 px-3 py-2"
                >
                  <div className="min-w-0">
                    <p className="truncate text-sm">{schedule.name}</p>
                    <p className="font-mono text-[11px] text-muted-foreground">
                      {schedule.cron} · {schedule.timezone}
                    </p>
                  </div>
                  <span className="font-mono text-xs">
                    {formatDateTime(schedule.next_run_at ?? "")}
                  </span>
                </li>
              ))}
            </ul>
          ) : (
            <div className="rounded-md border px-4 py-4">
              <p className="text-sm text-muted-foreground">
                {t("agent.overview.nextRunsEmpty")}
              </p>
              <Button asChild variant="link" size="sm" className="mt-1 h-auto p-0">
                <a href={schedulesHref}>{t("agent.overview.manageSchedules")}</a>
              </Button>
            </div>
          )}
        </Section>

        <Section
          title={t("agent.overview.messagesTitle")}
          action={
            <Button asChild variant="ghost" size="sm">
              <a href={messagesHref}>{t("agent.overview.openMessages")}</a>
            </Button>
          }
        >
          {messages.loading && !messages.data ? (
            <Skeleton className="h-24 w-full rounded-md" />
          ) : messages.error ? (
            <ErrorAlert error={messages.error} onRetry={messages.reload} />
          ) : messages.data && messages.data.length > 0 ? (
            <ul className="divide-y rounded-md border">
              {messages.data.map((message) => (
                <li key={message.id}>
                  <a href={messagesHref} className="block space-y-1 px-3 py-2 hover:bg-muted/60">
                    <span className="flex flex-wrap items-center justify-between gap-2">
                      <span className="flex min-w-0 items-center gap-1.5 font-mono text-[11px] text-muted-foreground">
                        <span className="truncate">{message.from}</span>
                        <span aria-hidden>→</span>
                        <span className="truncate">{message.to}</span>
                        <Badge variant="outline" className="font-mono text-[10px] font-normal">
                          {t(`messageKind.${message.kind}`)}
                        </Badge>
                      </span>
                      <span className="shrink-0 text-[11px] text-muted-foreground">
                        {formatRelative(message.created_at, lang)}
                      </span>
                    </span>
                    <span className="line-clamp-2 block text-sm break-words">{message.body}</span>
                  </a>
                </li>
              ))}
            </ul>
          ) : (
            <div className="rounded-md border px-4 py-4">
              <p className="text-sm text-muted-foreground">{t("agent.overview.messagesEmpty")}</p>
              <Button asChild variant="link" size="sm" className="mt-1 h-auto p-0">
                <a href={messagesHref}>{t("agent.overview.openMessages")}</a>
              </Button>
            </div>
          )}
        </Section>
      </div>

      <Section title={t("create.notes")}>
        {detail.notes ? (
          <p className="rounded-md border px-4 py-3 text-sm whitespace-pre-wrap">{detail.notes}</p>
        ) : (
          <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
            {t("detail.noNotes")}
          </p>
        )}
      </Section>

      {canEdit ? null : (
        <p className="text-xs text-muted-foreground">
          <RichText text={t("agent.overview.viewerHint")} />
        </p>
      )}
    </div>
  );
}
