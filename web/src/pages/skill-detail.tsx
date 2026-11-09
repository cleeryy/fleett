import { useState } from "react";
import {
  FileArchive,
  FileText,
  GitCommitHorizontal,
  MoreHorizontal,
  RefreshCw,
  RotateCw,
  Sparkles,
  Trash2,
  TriangleAlert,
  Users,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import type { ApiError } from "@/api/errors";
import type { CommitInfo, SkillDetail, SkillFile, SkillVersionInfo } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { CopyButton } from "@/components/copy-button";
import { DataList, Section } from "@/components/data-list";
import { PatchView } from "@/components/diff-view";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { RichText } from "@/components/rich-text";
import { MarkdownView } from "@/components/markdown-view";
import { PageHeader } from "@/components/page-header";
import { SkillCreateDialog } from "@/components/skill-create-dialog";
import { SkillDeleteDialog } from "@/components/skill-delete-dialog";
import { SkillPromoteDialog } from "@/components/skill-promote-dialog";
import { formatBytes, formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { hrefFor, navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import {
  isMarkdownSkillFile,
  nextSkillVersion,
  sortedSkillPaths,
} from "@/lib/skills";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

/* --------------------------------------------------------------------------
 * File content
 * -------------------------------------------------------------------------- */

function SkillFileViewer({ file }: { file: SkillFile }) {
  const { t } = useI18n();
  const markdown = isMarkdownSkillFile(file.path);
  const [view, setView] = useState<"preview" | "source">(markdown ? "preview" : "source");

  return (
    <div className="flex min-h-0 min-w-0 flex-col">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b bg-muted/40 px-3 py-1.5">
        <span className="truncate font-mono text-xs text-muted-foreground">{file.path}</span>
        <div className="flex items-center gap-1">
          {markdown ? (
            <div className="flex items-center gap-0.5 rounded-md bg-muted p-0.5">
              {(["preview", "source"] as const).map((option) => (
                <button
                  key={option}
                  type="button"
                  aria-pressed={view === option}
                  onClick={() => setView(option)}
                  className={cn(
                    "rounded px-2 py-0.5 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring/50",
                    view === option
                      ? "bg-background font-medium text-foreground shadow-xs"
                      : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  {option === "preview" ? t("skills.detail.preview") : t("skills.detail.source")}
                </button>
              ))}
            </div>
          ) : null}
          <CopyButton
            value={file.content}
            label={t("skills.detail.copyFile", { path: file.path })}
            variant="ghost"
            size="icon-xs"
          />
        </div>
      </div>

      {markdown && view === "preview" ? (
        <MarkdownView value={file.content} maxHeight="46vh" />
      ) : (
        <pre
          tabIndex={0}
          role="region"
          aria-label={file.path}
          className="max-h-[46vh] overflow-auto bg-muted/20 p-4 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words"
        >
          {file.content}
        </pre>
      )}
    </div>
  );
}

function FileTree({
  files,
  active,
  onSelect,
}: {
  files: SkillFile[];
  active: string | null;
  onSelect: (path: string) => void;
}) {
  const paths = sortedSkillPaths(files);
  return (
    <ul className="max-h-40 divide-y overflow-auto border-b sm:max-h-[46vh] sm:border-r sm:border-b-0">
      {paths.map((path) => {
        const selected = path === active;
        return (
          <li key={path}>
            <button
              type="button"
              aria-current={selected ? "true" : undefined}
              onClick={() => onSelect(path)}
              className={cn(
                "flex w-full items-center gap-2 px-2.5 py-2 text-left outline-none hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:-outline-offset-2",
                selected && "bg-muted",
              )}
            >
              <FileText aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
              <span className="min-w-0 flex-1 truncate font-mono text-xs">{path}</span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

/* --------------------------------------------------------------------------
 * Versions
 * -------------------------------------------------------------------------- */

/** The commit that introduced a version, not a later promote of the pointer. */
function versionCommit(
  commits: CommitInfo[],
  name: string,
  version: string,
): CommitInfo | null {
  const prefix = `${name}/${version}/`;
  return (
    commits.find((commit) => commit.paths.some((path) => path.startsWith(prefix))) ??
    commits.find((commit) => commit.message.endsWith(`@${version}`)) ??
    null
  );
}

function VersionContent({
  detail,
  version,
  commits,
  historyLoading,
  nonce,
}: {
  detail: SkillDetail;
  version: string;
  commits: CommitInfo[];
  historyLoading: boolean;
  nonce: number;
}) {
  const { t } = useI18n();
  const files = detail.latest === version ? detail.files : null;
  const [activePath, setActivePath] = useState<string | null>(null);
  const commit = files ? null : versionCommit(commits, detail.name, version);
  const diff = useResource(
    () =>
      commit
        ? api.skillGitDiff(detail.name, { to: commit.id })
        : Promise.resolve(null),
    [detail.name, commit?.id, nonce],
  );

  if (files) {
    const paths = sortedSkillPaths(files);
    const active = activePath && paths.includes(activePath) ? activePath : (paths[0] ?? null);
    const file = files.find((candidate) => candidate.path === active) ?? null;

    if (!file) {
      return (
        <p className="p-4 text-sm text-muted-foreground">{t("skills.detail.noReadableFile")}</p>
      );
    }

    return (
      <div className="grid min-w-0 sm:grid-cols-[190px_minmax(0,1fr)]">
        <FileTree files={files} active={active} onSelect={setActivePath} />
        <SkillFileViewer key={file.path} file={file} />
      </div>
    );
  }

  return (
    <div className="min-w-0">
      <p className="border-b bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
        <RichText text={t("skills.detail.servedVersion", { latest: detail.latest ?? "none" })} />
      </p>
      {historyLoading || (diff.loading && !diff.data) || diff.refreshing ? (
        <Skeleton className="m-3 h-40 rounded-md" />
      ) : !commit ? (
        <p className="px-3 py-6 text-sm text-muted-foreground">
          {t("skills.detail.noCommit")}
        </p>
      ) : diff.error ? (
        <div className="p-3">
          <ErrorAlert error={diff.error} onRetry={diff.reload} />
        </div>
      ) : (
        <PatchView
          patch={diff.data?.diff ?? ""}
          empty={t("skills.detail.noTextVersion")}
        />
      )}
    </div>
  );
}

/* --------------------------------------------------------------------------
 * History
 * -------------------------------------------------------------------------- */

function SkillHistory({
  name,
  commits,
  loading,
  error,
  onRetry,
  nonce,
}: {
  name: string;
  commits: CommitInfo[];
  loading: boolean;
  error: ApiError | null;
  onRetry: () => void;
  nonce: number;
}) {
  const { t, lang } = useI18n();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected =
    commits.find((commit) => commit.id === selectedId) ?? commits[0] ?? null;
  const diff = useResource(
    () => (selected ? api.skillGitDiff(name, { to: selected.id }) : Promise.resolve(null)),
    [name, selected?.id, nonce],
  );

  return (
    <Section
      title={t("skills.detail.history")}
      description={t("skills.detail.historyDescription")}
    >
      {loading ? (
        <Skeleton className="h-32 w-full rounded-md" />
      ) : error ? (
        <ErrorAlert error={error} onRetry={onRetry} />
      ) : commits.length === 0 ? (
        <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
          {t("skills.detail.noCommitYet")}
        </p>
      ) : (
        <div className="grid gap-3 lg:grid-cols-[300px_minmax(0,1fr)]">
          <ul className="max-h-[45vh] divide-y overflow-auto rounded-md border">
            {commits.map((commit) => (
              <li key={commit.id}>
                <button
                  type="button"
                  aria-current={commit.id === selected?.id ? "true" : undefined}
                  onClick={() => setSelectedId(commit.id)}
                  className={cn(
                    "w-full space-y-1 px-3 py-2.5 text-left hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:-outline-offset-2",
                    commit.id === selected?.id && "bg-muted",
                  )}
                >
                  <span className="flex items-baseline gap-2">
                    <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
                      {commit.short}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-sm">{commit.message}</span>
                  </span>
                  <span className="flex flex-wrap items-center gap-x-1.5 text-[11px] text-muted-foreground">
                    <span className="max-w-40 truncate">{commit.author}</span>
                    <span aria-hidden>·</span>
                    <time dateTime={commit.at} title={formatDateTime(commit.at)}>
                      {formatRelative(commit.at, lang)}
                    </time>
                    <span aria-hidden>·</span>
                    <span>{t("history.fileCount", { count: commit.paths.length })}</span>
                  </span>
                </button>
              </li>
            ))}
          </ul>

          <div className="min-w-0 overflow-hidden rounded-md border">
            <div className="flex flex-wrap items-center gap-x-2 gap-y-1 border-b px-3 py-2 text-xs text-muted-foreground">
              <GitCommitHorizontal aria-hidden className="size-3.5" />
              {selected ? <span className="font-mono text-foreground">{selected.short}</span> : null}
              <span className="min-w-0 truncate">{selected?.message}</span>
            </div>
            {diff.loading || diff.refreshing ? (
              <Skeleton className="m-3 h-40 rounded-md" />
            ) : diff.error ? (
              <div className="p-3">
                <ErrorAlert error={diff.error} onRetry={diff.reload} />
              </div>
            ) : (
              <PatchView
                patch={diff.data?.diff ?? ""}
                empty={t("skills.detail.noTextCommit")}
              />
            )}
          </div>
        </div>
      )}
    </Section>
  );
}

/* --------------------------------------------------------------------------
 * Page
 * -------------------------------------------------------------------------- */

export function SkillDetailPage({ name }: { name: string }) {
  const { t, lang } = useI18n();
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [selectedVersion, setSelectedVersion] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [promoteOpen, setPromoteOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const detail = useResource(() => api.getSkill(name), [name, nonce]);
  const instances = useResource(() => api.listInstances(), [name, nonce]);
  const log = useResource(() => api.skillGitLog(name, { limit: 50 }), [name, nonce]);

  if (detail.loading && !detail.data) {
    return (
      <div className="space-y-6">
        <div className="space-y-2">
          <Skeleton className="h-8 w-56" />
          <Skeleton className="h-4 w-80" />
        </div>
        <div className="grid gap-3 lg:grid-cols-[300px_minmax(0,1fr)]">
          <Skeleton className="h-64 w-full rounded-md" />
          <Skeleton className="h-64 w-full rounded-md" />
        </div>
      </div>
    );
  }

  if (!detail.data) {
    return (
      <>
        <PageHeader
          title={name}
          actions={
            <Button asChild variant="outline">
              <a href="#/skills">{t("skills.detail.back")}</a>
            </Button>
          }
        />
        {detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null}
      </>
    );
  }

  const skill = detail.data;
  const versionList = [...skill.versions].reverse();
  const activeVersion =
    selectedVersion && skill.versions.some((entry) => entry.version === selectedVersion)
      ? selectedVersion
      : (skill.latest ?? skill.versions[skill.versions.length - 1]?.version ?? null);
  const activeInfo: SkillVersionInfo | null =
    skill.versions.find((entry) => entry.version === activeVersion) ?? null;
  const latestInfo =
    skill.versions.find((entry) => entry.version === skill.latest) ?? null;
  const linked = (instances.data ?? []).filter((instance) =>
    (instance.skills ?? []).some((entry) => entry.name === skill.name),
  );
  const followers = linked.filter((instance) =>
    (instance.skills ?? []).some((entry) => entry.name === skill.name && !entry.version),
  );
  const pinnedCount = (version: string) =>
    linked.filter((instance) =>
      (instance.skills ?? []).some(
        (entry) => entry.name === skill.name && entry.version === version,
      ),
    ).length;

  return (
    <>
      <PageHeader
        title={
          <span className="flex flex-wrap items-center gap-3">
            <span className="font-mono">{skill.name}</span>
            {latestInfo ? (
              <Badge variant="secondary" className="font-mono text-xs font-normal">
                {t("skills.picker.latestBadge", { version: latestInfo.version })}
              </Badge>
            ) : null}
          </span>
        }
        description={
          <span className="space-y-1">
            <span className="block">
              {skill.description || t("skills.picker.noDescription")}
            </span>
            <span className="flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-xs">
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
              <span
                title={t("skills.detail.updatedTitle", {
                  time: formatDateTime(skill.updated_at),
                })}
              >
                {t("skills.picker.updated", {
                  time: formatRelative(skill.updated_at, lang),
                })}
              </span>
            </span>
          </span>
        }
        actions={
          <>
            <Button
              variant="outline"
              size="icon-sm"
              onClick={() => setNonce((value) => value + 1)}
              aria-label={t("skills.detail.refresh")}
              title={t("skills.detail.refresh")}
            >
              <RefreshCw aria-hidden className={detail.refreshing ? "animate-spin" : undefined} />
            </Button>
            {canEdit ? (
              <>
                <Button size="sm" variant="outline" onClick={() => setPromoteOpen(true)}>
                  <RotateCw aria-hidden />
                  {t("skills.detail.promote")}
                </Button>
                <Button size="sm" onClick={() => setCreateOpen(true)}>
                  <Sparkles aria-hidden />
                  {t("skills.detail.publish")}
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="icon-sm"
                      aria-label={t("providers.detail.moreActions")}
                      title={t("providers.detail.moreActions")}
                    >
                      <MoreHorizontal aria-hidden />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem variant="destructive" onSelect={() => setDeleteOpen(true)}>
                      <Trash2 aria-hidden />
                      {t("skills.detail.delete")}
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </>
            ) : null}
          </>
        }
      />

      {detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null}

      <div className="grid gap-6 lg:grid-cols-[300px_minmax(0,1fr)]">
        <Section
          title={t("skills.detail.versions")}
          description={
            activeInfo
              ? t("skills.detail.versionSummary", {
                  count: activeInfo.files_count,
                  size: formatBytes(activeInfo.size_bytes),
                  time: formatRelative(activeInfo.published_at, lang),
                })
              : t("skills.detail.pickVersion")
          }
        >
          <ul className="divide-y overflow-hidden rounded-md border">
            {versionList.map((entry) => {
              const isActive = entry.version === activeVersion;
              const isLatest = entry.version === skill.latest;
              const pins = pinnedCount(entry.version);
              return (
                <li key={entry.version}>
                  <button
                    type="button"
                    aria-current={isActive ? "true" : undefined}
                    onClick={() => setSelectedVersion(entry.version)}
                    className={cn(
                      "w-full space-y-1 px-3 py-2.5 text-left hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:-outline-offset-2",
                      isActive && "bg-muted",
                    )}
                  >
                    <span className="flex flex-wrap items-center gap-2">
                      <span className="font-mono text-sm font-medium">{entry.version}</span>
                      {isLatest ? (
                        <Badge variant="secondary" className="font-mono text-[11px] font-normal">
                          {t("skills.picker.latest")}
                        </Badge>
                      ) : null}
                      {pins > 0 ? (
                        <Badge variant="outline" className="font-mono text-[11px] font-normal">
                          {t("skills.detail.pinned", { count: pins })}
                        </Badge>
                      ) : null}
                    </span>
                    <span className="flex flex-wrap items-center gap-x-1.5 text-[11px] text-muted-foreground">
                      <span>{t("history.fileCount", { count: entry.files_count })}</span>
                      <span aria-hidden>·</span>
                      <span>{formatBytes(entry.size_bytes)}</span>
                      <span aria-hidden>·</span>
                      <time dateTime={entry.published_at} title={formatDateTime(entry.published_at)}>
                        {formatRelative(entry.published_at, lang)}
                      </time>
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
          <p className="text-xs text-muted-foreground">
            <RichText text={t("skills.detail.publishHint")} />
          </p>
        </Section>

        <Section
          title={
            activeVersion
              ? t("skills.detail.contentVersion", { version: activeVersion })
              : t("skills.detail.content")
          }
          description={
            activeVersion === skill.latest
              ? t("skills.detail.contentLatest")
              : t("skills.detail.contentEarlier")
          }
        >
          {activeVersion ? (
            <div className="min-w-0 overflow-hidden rounded-md border">
              <VersionContent
                detail={skill}
                version={activeVersion}
                commits={log.data?.commits ?? []}
                historyLoading={log.loading && !log.data}
                nonce={nonce}
              />
            </div>
          ) : (
            <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
              {t("skills.detail.noVersion")}
            </p>
          )}
        </Section>
      </div>

      <Section
        title={t("providers.detail.linkedInstances")}
        description={t("skills.detail.linkedDescription")}
      >
        {instances.loading && !instances.data ? (
          <Skeleton className="h-24 w-full rounded-md" />
        ) : instances.error ? (
          <ErrorAlert error={instances.error} onRetry={instances.reload} />
        ) : linked.length === 0 ? (
          <div className="rounded-md border">
            <EmptyState
              icon={Users}
              title={t("skills.detail.noLinkedTitle")}
              description={t("skills.detail.noLinkedDescription")}
              action={
                canEdit ? (
                  <Button asChild variant="outline" size="sm">
                    <a href={hrefFor("/instances/new")}>{t("nav.newInstance")}</a>
                  </Button>
                ) : undefined
              }
            />
          </div>
        ) : (
          <>
            <ul className="divide-y rounded-md border">
              {linked.map((instance) => {
                const ref = (instance.skills ?? []).find((entry) => entry.name === skill.name);
                const pin = ref?.version ?? null;
                const mounted = ref?.effective_version ?? null;
                return (
                  <li
                    key={instance.id}
                    className="flex flex-wrap items-center gap-x-3 gap-y-1.5 px-3 py-2.5"
                  >
                    <a
                      href={hrefFor(`/instances/${instance.id}?tab=configuration`)}
                      className="min-w-0 flex-1 truncate text-sm font-medium underline-offset-4 hover:underline"
                    >
                      {instance.name}
                      <span className="ml-2 font-mono text-xs font-normal text-muted-foreground">
                        {instance.id}
                      </span>
                    </a>
                    <Badge variant="outline" className="shrink-0 font-mono text-[11px] font-normal">
                      {pin ?? t("skills.detail.followsLatest")}
                    </Badge>
                    <Badge
                      variant={mounted === null ? "destructive" : "secondary"}
                      className="shrink-0 gap-1 font-mono text-[11px] font-normal"
                      title={
                        mounted === null ? t("skills.picker.unresolvedTitle") : undefined
                      }
                    >
                      {mounted === null ? (
                        <>
                          <TriangleAlert aria-hidden />
                          {t("skills.picker.unresolved")}
                        </>
                      ) : (
                        t("skills.picker.mounted", { version: mounted })
                      )}
                    </Badge>
                  </li>
                );
              })}
            </ul>
            <p className="text-xs text-muted-foreground">
              <RichText
                text={t(
                  followers.length === 1
                    ? "skills.detail.followersOne"
                    : "skills.detail.followersMany",
                  { count: followers.length },
                )}
              />
            </p>
          </>
        )}
      </Section>

      <SkillHistory
        name={name}
        commits={log.data?.commits ?? []}
        loading={log.loading && !log.data}
        error={log.error}
        onRetry={log.reload}
        nonce={nonce}
      />

      <Section title={t("skills.detail.registry")}>
        <DataList
          items={[
            { label: t("common.name"), value: <span className="font-mono">{skill.name}</span> },
            {
              label: t("skills.detail.latestHeader"),
              value: skill.latest ? (
                <span className="font-mono">{skill.latest}</span>
              ) : (
                <span className="text-muted-foreground">{t("skills.create.unset")}</span>
              ),
            },
            {
              label: t("shares.path"),
              value: (
                <span className="font-mono text-xs break-all">
                  skills/{skill.name}/{skill.latest ?? "latest"}
                </span>
              ),
            },
            {
              label: t("skills.detail.versions"),
              value: (
                <span className="flex flex-wrap gap-1">
                  {skill.versions.map((entry) => (
                    <Badge
                      key={entry.version}
                      variant={entry.version === skill.latest ? "secondary" : "outline"}
                      className="font-mono text-[11px] font-normal"
                    >
                      {entry.version} · {t("history.fileCount", { count: entry.files_count })}
                    </Badge>
                  ))}
                </span>
              ),
            },
            {
              label: t("common.created"),
              value: (
                <span
                  className="font-mono text-xs"
                  title={formatRelative(skill.created_at, lang)}
                >
                  {formatDateTime(skill.created_at)}
                </span>
              ),
            },
            {
              label: t("common.updated"),
              value: (
                <span
                  className="font-mono text-xs"
                  title={formatRelative(skill.updated_at, lang)}
                >
                  {formatDateTime(skill.updated_at)}
                </span>
              ),
            },
          ]}
        />
        <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <FileArchive aria-hidden className="size-3.5" />
          {t("skills.detail.registryHint")}
        </p>
      </Section>

      {canEdit ? (
        <>
          <SkillCreateDialog
            open={createOpen}
            onOpenChange={setCreateOpen}
            preset={{
              name: skill.name,
              version: nextSkillVersion(skill.versions.map((entry) => entry.version)),
            }}
            onPublished={(detailNext) => {
              const published = detailNext.versions[detailNext.versions.length - 1];
              toast.success(
                published
                  ? t("skills.detail.publishedVersion", {
                      name: detailNext.name,
                      version: published.version,
                    })
                  : t("skills.detail.published", { name: detailNext.name }),
              );
              setSelectedVersion(published?.version ?? null);
              setNonce((value) => value + 1);
            }}
          />
          <SkillPromoteDialog
            skill={skill}
            open={promoteOpen}
            onOpenChange={setPromoteOpen}
            onPromoted={() => setNonce((value) => value + 1)}
          />
          <SkillDeleteDialog
            skill={skill}
            instances={instances.data ?? []}
            open={deleteOpen}
            onOpenChange={setDeleteOpen}
            onDeleted={() => {
              toast.success(t("skills.detail.deleted", { name: skill.name }));
              navigate("/skills");
            }}
          />
        </>
      ) : null}
    </>
  );
}
