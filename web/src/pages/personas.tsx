import { useState } from "react";
import { Fingerprint, Plus, RefreshCw, Users } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import type { PersonaSummary } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { PersonaCreateDialog } from "@/components/persona-create-dialog";
import { formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { hrefFor, navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

function PersonaCard({ persona }: { persona: PersonaSummary }) {
  const { t, lang } = useI18n();
  return (
    <a
      href={hrefFor(`/personas/${persona.id}`)}
      className="group flex min-w-0 flex-col gap-3 rounded-md border p-4 outline-none transition-colors hover:bg-muted/40 focus-visible:ring-[3px] focus-visible:ring-ring/50"
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-sm font-medium group-hover:underline">{persona.name}</p>
          <p className="truncate font-mono text-xs text-muted-foreground">{persona.id}</p>
        </div>
        <Badge
          variant="outline"
          className="shrink-0 gap-1 font-mono text-[11px] font-normal text-muted-foreground"
          title={t(
            persona.instances_count === 1
              ? "personas.card.instancesOne"
              : "personas.card.instancesMany",
            { count: persona.instances_count },
          )}
        >
          <Users aria-hidden />
          {persona.instances_count}
        </Badge>
      </div>

      <p className="line-clamp-2 min-h-10 text-sm text-muted-foreground">
        {persona.description || persona.excerpt || t("skills.picker.noDescription")}
      </p>

      {persona.tags.length > 0 ? (
        <div className="flex flex-wrap gap-1">
          {persona.tags.map((tag) => (
            <Badge key={tag} variant="secondary" className="font-mono text-[11px] font-normal">
              {tag}
            </Badge>
          ))}
        </div>
      ) : (
        <span className="text-xs text-muted-foreground">{t("personas.card.noTags")}</span>
      )}

      <p className="mt-auto flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
        <span>
          {t(
            persona.variables.length === 1
              ? "personas.picker.variableOne"
              : "personas.picker.variableMany",
            { count: persona.variables.length },
          )}
        </span>
        <span aria-hidden>·</span>
        <span title={persona.updated_at}>
          {t("skills.picker.updated", { time: formatRelative(persona.updated_at, lang) })}
        </span>
      </p>
    </a>
  );
}

export function PersonasPage() {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [createOpen, setCreateOpen] = useState(false);
  const personas = useResource(() => api.listPersonas(), [nonce]);
  const rows = personas.data ?? [];

  return (
    <>
      <PageHeader
        title={t("nav.personas")}
        description={
          personas.data
            ? t(rows.length === 1 ? "personas.countOne" : "personas.countMany", {
                count: rows.length,
              })
            : t("personas.loading")
        }
        actions={
          <>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setNonce((value) => value + 1)}
              disabled={personas.refreshing}
            >
              <RefreshCw aria-hidden className={personas.refreshing ? "animate-spin" : undefined} />
              {t("common.refresh")}
            </Button>
            {canEdit ? (
              <Button size="sm" onClick={() => setCreateOpen(true)}>
                <Plus aria-hidden />
                {t("personas.create.title")}
              </Button>
            ) : null}
          </>
        }
      />

      {!canEdit ? (
        <p className="text-xs text-muted-foreground">
          {t("personas.readOnly")}
        </p>
      ) : null}

      {personas.error ? <ErrorAlert error={personas.error} onRetry={personas.reload} /> : null}

      {personas.loading && !personas.data ? (
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {Array.from({ length: 6 }).map((_, index) => (
            <div key={index} className="space-y-3 rounded-md border p-4">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-3 w-24" />
              <Skeleton className="h-8 w-full" />
              <Skeleton className="h-4 w-32" />
            </div>
          ))}
        </div>
      ) : rows.length === 0 ? (
        <div className="rounded-md border">
          <EmptyState
            icon={Fingerprint}
            title={t("personas.emptyTitle")}
            description={t("personas.emptyDescription")}
            action={
              canEdit ? (
                <Button onClick={() => setCreateOpen(true)}>
                  <Plus aria-hidden />
                  {t("personas.emptyAction")}
                </Button>
              ) : undefined
            }
          />
        </div>
      ) : (
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {rows.map((persona) => (
            <PersonaCard key={persona.id} persona={persona} />
          ))}
        </div>
      )}

      {rows.length > 0 ? (
        <p className="text-xs text-muted-foreground">
          {t("personas.footerHint")}
        </p>
      ) : null}

      {canEdit ? (
        <PersonaCreateDialog
          open={createOpen}
          onOpenChange={setCreateOpen}
          onCreated={(id) => {
            toast.success(t("personas.created"));
            navigate(`/personas/${id}`);
          }}
        />
      ) : null}
    </>
  );
}
