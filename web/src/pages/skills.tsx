import { useState } from "react";
import { FileArchive, Plus, RefreshCw, Sparkles, Users } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import type { SkillSummary } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { SkillCreateDialog } from "@/components/skill-create-dialog";
import { SkillImportDialog } from "@/components/skill-import-dialog";
import { formatDateTime, formatRelative, plural } from "@/lib/format";
import { hrefFor, navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

function SkillRow({ skill }: { skill: SkillSummary }) {
  return (
    <li>
      <a
        href={hrefFor(`/skills/${skill.name}`)}
        className="grid gap-x-4 gap-y-1 px-4 py-3 outline-none transition-colors hover:bg-muted/40 focus-visible:ring-[3px] focus-visible:ring-ring/50 sm:grid-cols-[minmax(0,1fr)_96px_92px_96px_116px] sm:items-center"
      >
        <div className="min-w-0">
          <p className="truncate font-mono text-sm font-medium">{skill.name}</p>
          <p className="line-clamp-2 text-sm text-muted-foreground">
            {skill.description || "No description."}
          </p>
          <p className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted-foreground sm:hidden">
            <span>latest {skill.latest ?? "unset"}</span>
            <span aria-hidden>·</span>
            <span>
              {skill.versions.length} {plural(skill.versions.length, "version")}
            </span>
            <span aria-hidden>·</span>
            <span>
              {skill.instances_count} {plural(skill.instances_count, "instance")}
            </span>
            <span aria-hidden>·</span>
            <span title={formatDateTime(skill.updated_at)}>
              updated {formatRelative(skill.updated_at)}
            </span>
          </p>
        </div>
        <div className="hidden sm:block">
          {skill.latest ? (
            <Badge variant="secondary" className="font-mono text-[11px] font-normal">
              {skill.latest}
            </Badge>
          ) : (
            <span className="text-xs text-muted-foreground">unset</span>
          )}
        </div>
        <div className="hidden font-mono text-xs sm:block">
          {skill.versions.length}
          <span className="sr-only"> versions</span>
        </div>
        <div className="hidden items-center gap-1 font-mono text-xs sm:flex">
          <Users aria-hidden className="size-3.5 text-muted-foreground" />
          {skill.instances_count}
          <span className="sr-only"> instances</span>
        </div>
        <div
          className="hidden truncate text-xs text-muted-foreground sm:block"
          title={formatDateTime(skill.updated_at)}
        >
          {formatRelative(skill.updated_at)}
        </div>
      </a>
    </li>
  );
}

export function SkillsPage() {
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [createOpen, setCreateOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const skills = useResource(() => api.listSkills(), [nonce]);
  const rows = skills.data ?? [];

  return (
    <>
      <PageHeader
        title="Skills"
        description={
          skillSummaryText(rows, skills.data !== null)
        }
        actions={
          <>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setNonce((value) => value + 1)}
              disabled={skills.refreshing}
            >
              <RefreshCw aria-hidden className={skills.refreshing ? "animate-spin" : undefined} />
              Refresh
            </Button>
            {canEdit ? (
              <>
                <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
                  <FileArchive aria-hidden />
                  Import zip
                </Button>
                <Button size="sm" onClick={() => setCreateOpen(true)}>
                  <Plus aria-hidden />
                  Publish skill
                </Button>
              </>
            ) : null}
          </>
        }
      />

      {!canEdit ? (
        <p className="text-xs text-muted-foreground">
          Read-only: publishing, promoting and deleting a skill needs a write-capable account.
        </p>
      ) : null}

      {skills.error ? <ErrorAlert error={skills.error} onRetry={skills.reload} /> : null}

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
            title="No skills yet"
            description="A skill is one small tree of text files an agent reads while it works: a SKILL.md plus whatever it references. The registry keeps one copy, versions it, and mounts it read-only in every instance that declares it."
            action={
              canEdit ? (
                <div className="flex flex-wrap items-center justify-center gap-2">
                  <Button onClick={() => setCreateOpen(true)}>
                    <Plus aria-hidden />
                    Publish the first skill
                  </Button>
                  <Button variant="outline" onClick={() => setImportOpen(true)}>
                    <FileArchive aria-hidden />
                    Import a zip
                  </Button>
                </div>
              ) : undefined
            }
          />
        </div>
      ) : (
        <div className="rounded-md border">
          <div className="hidden grid-cols-[minmax(0,1fr)_96px_92px_96px_116px] gap-x-4 border-b bg-muted/30 px-4 py-2 text-xs font-medium text-muted-foreground sm:grid">
            <span>Skill</span>
            <span>Latest</span>
            <span>Versions</span>
            <span>Instances</span>
            <span>Updated</span>
          </div>
          <ul className="divide-y">
            {rows.map((skill) => (
              <SkillRow key={skill.name} skill={skill} />
            ))}
          </ul>
        </div>
      )}

      {rows.length > 0 ? (
        <p className="text-xs text-muted-foreground">
          Open a skill to read its versions, compare commits and promote one. A publish never
          overwrites: the registry keeps every version, and instances follow{" "}
          <span className="font-mono">latest</span> until they pin one.
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
                `Published ${detail.name} ${detail.versions[detail.versions.length - 1]?.version ?? ""}.`,
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

function skillSummaryText(rows: SkillSummary[], loaded: boolean): string {
  if (!loaded) return "Loading the skill registry.";
  if (rows.length === 0) {
    return "One versioned registry, mounted read-only in the instances that declare it.";
  }
  const versions = rows.reduce((total, skill) => total + skill.versions.length, 0);
  const mounts = rows.reduce((total, skill) => total + skill.instances_count, 0);
  return `${rows.length} ${plural(rows.length, "skill")} · ${versions} ${plural(
    versions,
    "version",
  )} · ${mounts} ${plural(mounts, "mount")} · mounted read-only at /opt/data/skills`;
}
