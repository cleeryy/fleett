import { useEffect, type ReactNode } from "react";
import { api } from "@/api";
import type { InstanceDetail } from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { CodeBlock } from "@/components/code-block";
import { Section } from "@/components/data-list";
import { ErrorAlert } from "@/components/error-alert";
import { InstanceMemoryTab } from "@/components/instance-bus";
import { InstanceMcp } from "@/components/instance-mcp";
import {
  InstanceAccess,
  InstanceEgress,
  InstanceSnapshots,
  InstanceTerminal,
} from "@/components/instance-ops";
import { RichText } from "@/components/rich-text";
import type { AdvancedSection } from "@/lib/agent-tabs";
import { formatDateTime } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { useUiMode } from "@/lib/mode";
import { hrefFor } from "@/lib/router";
import { useResource } from "@/lib/use-resource";

function Block({
  id,
  title,
  description,
  action,
  children,
}: {
  id: AdvancedSection;
  title: string;
  description?: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div id={`agent-advanced-${id}`} className="scroll-mt-24">
      <Section title={title} description={description} action={action}>
        {children}
      </Section>
    </div>
  );
}

/**
 * Advanced tab of the agent detail (#73): one scrollable page with every
 * system panel the console used to spread over nine tabs. In simple mode it is
 * hidden from the tab bar, but a legacy deep link still lands here with a
 * banner that offers the mode switch instead of a 404.
 */
export function AgentAdvancedPanel({
  detail,
  canEdit,
  section,
}: {
  detail: InstanceDetail;
  canEdit: boolean;
  section?: AdvancedSection;
}) {
  const { t } = useI18n();
  const { simple, setMode } = useUiMode();
  const schedules = useResource(() => api.listInstanceSchedules(detail.id), [detail.id]);

  useEffect(() => {
    if (!section) return;
    document.getElementById(`agent-advanced-${section}`)?.scrollIntoView({ block: "start" });
  }, [section]);

  return (
    <div className="space-y-8">
      {simple ? (
        <Alert>
          <AlertTitle>{t("agent.advancedBannerTitle")}</AlertTitle>
          <AlertDescription>
            <p className="text-sm text-muted-foreground">{t("agent.advancedBanner")}</p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="mt-2"
              onClick={() => setMode("advanced")}
            >
              {t("agent.advancedSwitch")}
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}

      <Block
        id="schedules"
        title={t("agent.advanced.schedules")}
        description={t("agent.advanced.schedulesDescription")}
        action={
          <Button asChild variant="outline" size="sm">
            <a href={hrefFor("/schedules")}>{t("agent.advanced.manageSchedules")}</a>
          </Button>
        }
      >
        {schedules.loading && !schedules.data ? (
          <Skeleton className="h-24 w-full rounded-md" />
        ) : schedules.error ? (
          <ErrorAlert error={schedules.error} onRetry={schedules.reload} />
        ) : (schedules.data ?? []).length ? (
          <ul className="divide-y rounded-md border">
            {(schedules.data ?? []).map((schedule) => (
              <li
                key={schedule.id}
                className="flex flex-wrap items-baseline justify-between gap-2 px-3 py-2"
              >
                <div className="min-w-0">
                  <p className="flex flex-wrap items-center gap-2 text-sm">
                    <span className="truncate">{schedule.name}</span>
                    <Badge variant="outline" className="font-mono text-[10px] font-normal">
                      {schedule.enabled ? t("common.enabled") : t("agent.advanced.paused")}
                    </Badge>
                  </p>
                  <p className="font-mono text-[11px] text-muted-foreground">
                    {schedule.cron} · {schedule.timezone}
                  </p>
                </div>
                <span className="font-mono text-xs text-muted-foreground">
                  {schedule.next_run_at ? formatDateTime(schedule.next_run_at) : "—"}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="rounded-md border px-4 py-3 text-sm text-muted-foreground">
            {t("schedules.emptyDescription")}
          </p>
        )}
      </Block>

      <Block
        id="backups"
        title={t("detail.tab.snapshots")}
        description={t("agent.advanced.backupsDescription")}
      >
        <InstanceSnapshots id={detail.id} />
      </Block>

      <Block
        id="connectors"
        title={t("agent.advanced.connectors")}
        description={t("agent.advanced.connectorsDescription")}
      >
        <InstanceMcp id={detail.id} />
      </Block>

      <Block
        id="access"
        title={t("detail.tab.access")}
        description={t("agent.advanced.accessDescription")}
      >
        <InstanceAccess id={detail.id} />
      </Block>

      <Block
        id="network"
        title={t("detail.tab.egress")}
        description={t("agent.advanced.networkDescription")}
      >
        <InstanceEgress id={detail.id} />
      </Block>

      <Block
        id="memory"
        title={t("nav.memory")}
        description={t("agent.advanced.memoryDescription")}
      >
        <InstanceMemoryTab id={detail.id} canEdit={canEdit} />
      </Block>

      {canEdit ? (
        <Block
          id="terminal"
          title={t("detail.tab.terminal")}
          description={t("agent.advanced.terminalDescription")}
        >
          <InstanceTerminal id={detail.id} />
        </Block>
      ) : null}

      <Block
        id="compose"
        title={t("detail.tab.compose")}
        description={t("agent.advanced.composeDescription")}
      >
        <div className="space-y-3">
          <p className="text-sm text-muted-foreground">
            <RichText text={t("detail.composeHint")} />
          </p>
          <CodeBlock value={detail.compose_yaml} title={`instances/${detail.id}/compose.yaml`} />
        </div>
      </Block>
    </div>
  );
}
