import type { ReactNode } from "react";
import {
  ChevronRight,
  RefreshCw,
  Rocket,
  ScrollText,
  ShieldCheck,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import { api } from "@/api";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { OpsBadge } from "@/components/ops-badge";
import { PageHeader } from "@/components/page-header";
import { formatClock, formatDateTime, formatRelative } from "@/lib/format";
import { hasTranslation, useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";
import { useResource } from "@/lib/use-resource";

function CauseLabel({ cause }: { cause: string }) {
  const { t } = useI18n();
  const key = `incidentCause.${cause}`;
  return <span>{hasTranslation(key) ? t(key) : cause.replace(/_/g, " ")}</span>;
}

function RowsSkeleton({ rows = 4 }: { rows?: number }) {
  return (
    <div className="space-y-3 p-4" aria-hidden>
      {Array.from({ length: rows }).map((_, index) => (
        <Skeleton key={index} className="h-5 w-full" />
      ))}
    </div>
  );
}

function Block({
  icon: Icon,
  title,
  meta,
  hint,
  viewAll,
  viewAllLabel,
  children,
}: {
  icon: LucideIcon;
  title: string;
  meta?: string;
  hint: string;
  viewAll: string;
  viewAllLabel: string;
  children: ReactNode;
}) {
  return (
    <section className="flex flex-col overflow-hidden rounded-lg border bg-card">
      <header className="flex items-start gap-3 border-b px-4 py-3">
        <Icon aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1">
          <h2 className="flex flex-wrap items-baseline gap-x-2 text-sm font-semibold">
            {title}
            {meta ? <span className="font-normal text-muted-foreground">{meta}</span> : null}
          </h2>
          <p className="text-xs text-muted-foreground text-pretty">{hint}</p>
        </div>
        <a
          href={viewAll}
          className="mt-0.5 shrink-0 self-start rounded-md px-1.5 py-1 text-xs font-medium text-muted-foreground underline-offset-4 outline-none transition-colors hover:text-foreground hover:underline focus-visible:ring-[3px] focus-visible:ring-ring/50"
        >
          {viewAllLabel}
        </a>
      </header>
      <div className="flex-1">{children}</div>
    </section>
  );
}

/**
 * Activité (#71): a bounded, read-only answer to "what happened?".
 *
 * Three blocks, five or six rows each, one link to the full page. No mutation
 * and no new API method: the incidents, logs and deployments endpoints already
 * carry what the feed needs.
 */
export function ActivityPage() {
  const { t, lang } = useI18n();

  const incidentsQuery = useResource(
    () => api.listIncidents({ status: "open", limit: 5 }),
    [],
  );
  const logsQuery = useResource(() => api.searchLogs({ limit: 6, since: "24h" }), []);
  const deploymentsQuery = useResource(() => api.listDeployments({ limit: 5 }), []);

  const incidents = incidentsQuery.data ?? [];
  const entries = logsQuery.data?.entries ?? [];
  const deployments = deploymentsQuery.data ?? [];
  const refreshing =
    incidentsQuery.refreshing || logsQuery.refreshing || deploymentsQuery.refreshing;

  function refreshAll() {
    incidentsQuery.reload();
    logsQuery.reload();
    deploymentsQuery.reload();
  }

  return (
    <>
      <PageHeader
        title={t("nav.activity")}
        description={t("shell.activity.description")}
        actions={
          <Button variant="outline" size="sm" onClick={refreshAll} disabled={refreshing}>
            <RefreshCw aria-hidden className={refreshing ? "animate-spin" : undefined} />
            {t("common.refresh")}
          </Button>
        }
      />

      <div className="grid items-start gap-4 lg:grid-cols-3">
        <div className="min-w-0 lg:col-span-3">
          <Block
            icon={TriangleAlert}
            title={t("shell.activity.incidentsTitle")}
            meta={
              incidents.length > 0
                ? t(
                    incidents.length === 1
                      ? "shell.activity.openOne"
                      : "shell.activity.openMany",
                    { count: incidents.length },
                  )
                : undefined
            }
            hint={t("shell.activity.incidentsHint")}
            viewAll="#/incidents"
            viewAllLabel={t("shell.activity.incidentsViewAll")}
          >
            {incidentsQuery.error ? (
              <ErrorAlert error={incidentsQuery.error} onRetry={incidentsQuery.reload} className="m-3" />
            ) : null}
            {incidentsQuery.loading && !incidentsQuery.data ? (
              <RowsSkeleton rows={3} />
            ) : incidentsQuery.data === null ? null : incidents.length === 0 ? (
              <EmptyState
                icon={ShieldCheck}
                title={t("shell.activity.incidentsEmptyTitle")}
                description={t("shell.activity.incidentsEmptyDescription")}
                action={
                  <Button asChild variant="outline" size="sm">
                    <a href="#/instances">{t("shell.activity.incidentsEmptyAction")}</a>
                  </Button>
                }
                className="py-8"
              />
            ) : (
              <ul className="divide-y">
                {incidents.map((incident) => (
                  <li key={incident.id}>
                    <a
                      href={hrefFor(`/instances/${encodeURIComponent(incident.instance_id)}`)}
                      className="group flex min-h-11 items-center gap-3 px-4 py-3 outline-none transition-colors hover:bg-accent/30 focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:ring-inset"
                    >
                      <span aria-hidden className="size-2 shrink-0 rounded-full bg-destructive" />
                      <span className="min-w-0 flex-1">
                        <span className="flex flex-wrap items-center gap-2">
                          <span className="truncate text-sm font-medium">
                            {incident.instance_id}
                          </span>
                          <OpsBadge status={incident.status} />
                          <time
                            dateTime={incident.opened_at}
                            title={formatDateTime(incident.opened_at)}
                            className="text-[11px] text-muted-foreground"
                          >
                            {formatRelative(incident.opened_at, lang)}
                          </time>
                        </span>
                        <span className="mt-1 flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
                          <CauseLabel cause={incident.cause} />
                          <span aria-hidden>·</span>
                          <span className="min-w-0 truncate">{incident.detail}</span>
                        </span>
                      </span>
                      <ChevronRight
                        aria-hidden
                        className="size-4 shrink-0 text-muted-foreground/50 transition-transform group-hover:translate-x-0.5 motion-reduce:transition-none"
                      />
                    </a>
                  </li>
                ))}
              </ul>
            )}
          </Block>
        </div>

        <div className="min-w-0 lg:col-span-2">
          <Block
            icon={ScrollText}
            title={t("shell.activity.logsTitle")}
            hint={t("shell.activity.logsHint")}
            viewAll="#/logs"
            viewAllLabel={t("shell.activity.logsViewAll")}
          >
            {logsQuery.error ? (
              <ErrorAlert error={logsQuery.error} onRetry={logsQuery.reload} className="m-3" />
            ) : null}
            {logsQuery.loading && !logsQuery.data ? (
              <RowsSkeleton rows={5} />
            ) : logsQuery.data === null ? null : entries.length === 0 ? (
              <EmptyState
                icon={ScrollText}
                title={t("shell.activity.logsEmptyTitle")}
                description={t("shell.activity.logsEmptyDescription")}
                action={
                  <Button asChild variant="outline" size="sm">
                    <a href="#/logs">{t("shell.activity.logsEmptyAction")}</a>
                  </Button>
                }
                className="py-8"
              />
            ) : (
              <ul className="divide-y">
                {entries.map((entry) => (
                  <li
                    key={entry.id}
                    className="flex min-h-11 items-center gap-2.5 px-4 py-2.5"
                  >
                    <time
                      dateTime={entry.at}
                      title={formatDateTime(entry.at)}
                      className="w-16 shrink-0 font-mono text-[11px] text-muted-foreground"
                    >
                      {formatClock(new Date(entry.at))}
                    </time>
                    <OpsBadge
                      status={entry.level}
                      label={t(`shell.activity.level.${entry.level}`)}
                      className="shrink-0"
                    />
                    <p className="min-w-0 flex-1 truncate text-xs">
                      <span className="font-mono text-muted-foreground">{entry.instance}</span>{" "}
                      <span>{entry.message}</span>
                    </p>
                  </li>
                ))}
              </ul>
            )}
          </Block>
        </div>

        <div className="min-w-0 lg:col-span-1">
          <Block
            icon={Rocket}
            title={t("shell.activity.deploymentsTitle")}
            hint={t("shell.activity.deploymentsHint")}
            viewAll="#/deployments"
            viewAllLabel={t("shell.activity.deploymentsViewAll")}
          >
            {deploymentsQuery.error ? (
              <ErrorAlert
                error={deploymentsQuery.error}
                onRetry={deploymentsQuery.reload}
                className="m-3"
              />
            ) : null}
            {deploymentsQuery.loading && !deploymentsQuery.data ? (
              <RowsSkeleton rows={4} />
            ) : deploymentsQuery.data === null ? null : deployments.length === 0 ? (
              <EmptyState
                icon={Rocket}
                title={t("shell.activity.deploymentsEmptyTitle")}
                description={t("shell.activity.deploymentsEmptyDescription")}
                action={
                  <Button asChild variant="outline" size="sm">
                    <a href="#/deployments">{t("shell.activity.deploymentsEmptyAction")}</a>
                  </Button>
                }
                className="py-8"
              />
            ) : (
              <ul className="divide-y">
                {deployments.map((deployment) => (
                  <li key={deployment.id} className="px-4 py-3">
                    <div className="flex min-h-6 items-center gap-2">
                      <OpsBadge status={deployment.status} className="shrink-0" />
                      <span
                        className="min-w-0 flex-1 truncate font-mono text-xs"
                        title={deployment.image}
                      >
                        {deployment.image}
                      </span>
                    </div>
                    <p className="mt-1 text-[11px] text-muted-foreground">
                      {deployment.started_at
                        ? formatRelative(deployment.started_at, lang)
                        : t("shell.activity.notStarted")}
                      <span aria-hidden> · </span>
                      {t(
                        deployment.instances.length === 1
                          ? "shell.activity.agentOne"
                          : "shell.activity.agentMany",
                        { count: deployment.instances.length },
                      )}
                    </p>
                  </li>
                ))}
              </ul>
            )}
          </Block>
        </div>
      </div>
    </>
  );
}
