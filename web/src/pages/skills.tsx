import { useState } from "react";
import { ArrowUpCircle, FileArchive, Plus, RefreshCw, Sparkles, Users } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import type { MarketplaceUpdateInfo, SkillSummary } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { RichText } from "@/components/rich-text";
import { PageHeader } from "@/components/page-header";
import { SkillCreateDialog } from "@/components/skill-create-dialog";
import { SkillImportDialog } from "@/components/skill-import-dialog";
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { refreshMarketplaceUpdates, useMarketplaceUpdates } from "@/lib/marketplace";
import { hrefFor, navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

function SkillRow({
  skill,
  update,
}: {
  skill: SkillSummary;
  update?: MarketplaceUpdateInfo;
}) {
  const { t, lang } = useI18n();
  return (
    <li>
      <a
        href={hrefFor(`/skills/${skill.name}`)}
        className="grid gap-x-4 gap-y-1 px-4 py-3 outline-none transition-colors hover:bg-muted/40 focus-visible:ring-[3px] focus-visible:ring-ring/50 sm:grid-cols-[minmax(0,1fr)_96px_92px_96px_116px] sm:items-center"
      >
        <div className="min-w-0">
          <p className="flex min-w-0 items-center gap-2">
            <span className="truncate font-mono text-sm font-medium" title={skill.name}>{skill.name}</span>
            {update ? (
              <Badge
                variant="outline"
                className="shrink-0 gap-1 border-sky-600/40 font-mono text-[10px] font-normal text-sky-700 dark:text-sky-400"
                title={t("skills.list.updateBadgeTitle", {
                  version: update.upstream_version ?? "",
                  installed: update.installed_version,
                })}
              >
                <ArrowUpCircle aria-hidden />
                {t("skills.list.updateBadge", { version: update.upstream_version ?? "" })}
              </Badge>
            ) : null}
          </p>
          <p className="line-clamp-2 text-sm text-muted-foreground">
            {skill.description || t("skills.picker.noDescription")}
          </p>
          <p className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted-foreground sm:hidden">
            <span>
              {t("skills.list.latest")} {skill.latest ?? t("skills.create.unset")}
            </span>
            <span aria-hidden>·</span>
            <span>
              {t(
                skill.versions.length === 1
                  ? "skills.picker.versionOne"
                  : "skills.picker.versionMany",
                { count: skill.versions.length },
              )}
            </span>
            <span aria-hidden>·</span>
            <span>
              {t(
                skill.instances_count === 1
                  ? "skills.picker.instanceOne"
                  : "skills.picker.instanceMany",
                { count: skill.instances_count },
              )}
            </span>
            <span aria-hidden>·</span>
            <span title={formatDateTime(skill.updated_at)}>
              {t("skills.picker.updated", { time: formatRelative(skill.updated_at, lang) })}
            </span>
          </p>
        </div>
        <div className="hidden sm:block">
          {skill.latest ? (
            <Badge variant="secondary" className="font-mono text-[11px] font-normal">
              {skill.latest}
            </Badge>
          ) : (
            <span className="text-xs text-muted-foreground">{t("skills.create.unset")}</span>
          )}
        </div>
        <div className="hidden font-mono text-xs sm:block">
          {skill.versions.length}
          <span className="sr-only"> {t("skills.list.versionsSr")}</span>
        </div>
        <div className="hidden items-center gap-1 font-mono text-xs sm:flex">
          <Users aria-hidden className="size-3.5 text-muted-foreground" />
          {skill.instances_count}
          <span className="sr-only"> {t("skills.list.instancesSr")}</span>
        </div>
        <div
          className="hidden truncate text-xs text-muted-foreground sm:block"
          title={formatDateTime(skill.updated_at)}
        >
          {formatRelative(skill.updated_at, lang)}
        </div>
      </a>
    </li>
  );
}

export function SkillsPage() {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [createOpen, setCreateOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const skills = useResource(() => api.listSkills(), [nonce]);
  const updates = useMarketplaceUpdates();
  const rows = skills.data ?? [];
  const updatesByName = new Map(
    (updates.data?.updates ?? []).map((entry) => [entry.name, entry]),
  );

  function refresh() {
    setNonce((value) => value + 1);
    refreshMarketplaceUpdates();
  }

  return (
    <>
      <PageHeader
        title={t("nav.skills")}
        description={skillSummaryText(rows, skills.data !== null, t)}
        actions={
          <>
            <Button
              variant="outline"
              size="sm"
              onClick={refresh}
              disabled={skills.refreshing}
            >
              <RefreshCw aria-hidden className={skills.refreshing ? "animate-spin" : undefined} />
              {t("common.refresh")}
            </Button>
            {canEdit ? (
              <>
                <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
                  <FileArchive aria-hidden />
                  {t("skills.list.importZip")}
                </Button>
                <Button size="sm" onClick={() => setCreateOpen(true)}>
                  <Plus aria-hidden />
                  {t("skills.list.publish")}
                </Button>
              </>
            ) : null}
          </>
        }
      />

      {!canEdit ? (
        <p className="text-xs text-muted-foreground">
          {t("skills.list.readOnly")}
        </p>
      ) : null}

      {skills.error ? <ErrorAlert error={skills.error} onRetry={skills.reload} /> : null}

      {updatesByName.size > 0 ? (
        <div className="flex items-start gap-2 rounded-md border border-sky-600/30 bg-sky-50/40 px-3 py-2 text-sm dark:bg-sky-950/20">
          <ArrowUpCircle aria-hidden className="mt-0.5 size-4 shrink-0 text-sky-700 dark:text-sky-400" />
          <p className="min-w-0 text-muted-foreground">
            {t(
              updatesByName.size === 1 ? "skills.list.updatesOne" : "skills.list.updatesMany",
              { count: updatesByName.size },
            )}
          </p>
        </div>
      ) : null}

      {skills.loading && !skills.data ? (
        <div className="space-y-2 rounded-md border p-2">
          {Array.from({ length: 5 }).map((_, index) => (
            <div key={index} className="space-y-2 px-2 py-3">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-3 w-72" />
            </div>
          ))}
        </div>
      ) : rows.length === 0 ? (
        <div className="rounded-md border">
          <EmptyState
            icon={Sparkles}
            title={t("skills.list.emptyTitle")}
            description={t("skills.list.emptyDescription")}
            action={
              canEdit ? (
                <div className="flex flex-wrap items-center justify-center gap-2">
                  <Button onClick={() => setCreateOpen(true)}>
                    <Plus aria-hidden />
                    {t("skills.list.emptyAction")}
                  </Button>
                  <Button variant="outline" onClick={() => setImportOpen(true)}>
                    <FileArchive aria-hidden />
                    {t("skills.list.importAction")}
                  </Button>
                </div>
              ) : undefined
            }
          />
        </div>
      ) : (
        <div className="rounded-md border">
          <div className="hidden grid-cols-[minmax(0,1fr)_96px_92px_96px_116px] gap-x-4 border-b bg-muted/30 px-4 py-2 text-xs font-medium text-muted-foreground sm:grid">
            <span>{t("skills.list.skill")}</span>
            <span>{t("skills.list.latestHeader")}</span>
            <span>{t("skills.list.versions")}</span>
            <span>{t("nav.instances")}</span>
            <span>{t("common.updated")}</span>
          </div>
          <ul className="divide-y">
            {rows.map((skill) => (
              <SkillRow key={skill.name} skill={skill} update={updatesByName.get(skill.name)} />
            ))}
          </ul>
        </div>
      )}

      {rows.length > 0 ? (
        <p className="text-xs text-muted-foreground">
          <RichText text={t("skills.list.footerHint")} />
        </p>
      ) : null}

      {canEdit ? (
        <>
          <SkillCreateDialog
            open={createOpen}
            onOpenChange={setCreateOpen}
            knownSkills={rows}
            onPublished={(detail) => {
              toast.success(
                t("skills.list.published", {
                  name: detail.name,
                  version: detail.versions[detail.versions.length - 1]?.version ?? "",
                }),
              );
              navigate(`/skills/${detail.name}`);
            }}
          />
          <SkillImportDialog
            open={importOpen}
            onOpenChange={setImportOpen}
            onImported={() => setNonce((value) => value + 1)}
            onOpenSkill={(name) => navigate(`/skills/${name}`)}
          />
        </>
      ) : null}
    </>
  );
}

function skillSummaryText(
  rows: SkillSummary[],
  loaded: boolean,
  t: (key: string, params?: Record<string, string | number>) => string,
): string {
  if (!loaded) return t("skills.list.loading");
  if (rows.length === 0) return t("skills.list.summaryEmpty");
  const versions = rows.reduce((total, skill) => total + skill.versions.length, 0);
  const mounts = rows.reduce((total, skill) => total + skill.instances_count, 0);
  return t("skills.list.summary", {
    skills: rows.length,
    versions,
    mounts,
  });
}
