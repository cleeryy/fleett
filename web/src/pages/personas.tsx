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
import { formatRelative, plural } from "@/lib/format";
import { hrefFor, navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

function PersonaCard({ persona }: { persona: PersonaSummary }) {
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
          title={`${persona.instances_count} ${plural(persona.instances_count, "instance")} using this persona`}
        >
          <Users aria-hidden />
          {persona.instances_count}
        </Badge>
      </div>

      <p className="line-clamp-2 min-h-10 text-sm text-muted-foreground">
        {persona.description || persona.excerpt || "No description."}
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
        <span className="text-xs text-muted-foreground">No tags</span>
      )}

      <p className="mt-auto flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
        <span>
          {persona.variables.length} {plural(persona.variables.length, "variable")}
        </span>
        <span aria-hidden>·</span>
        <span title={persona.updated_at}>updated {formatRelative(persona.updated_at)}</span>
      </p>
    </a>
  );
}

export function PersonasPage() {
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const [createOpen, setCreateOpen] = useState(false);
  const personas = useResource(() => api.listPersonas(), [nonce]);
  const rows = personas.data ?? [];

  return (
    <>
      <PageHeader
        title="Personas"
        description={
          personas.data
            ? `${rows.length} ${plural(rows.length, "persona")} · reusable SOUL.md with variables · applied into data/SOUL.md`
            : "Loading personas."
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
              Refresh
            </Button>
            {canEdit ? (
              <Button size="sm" onClick={() => setCreateOpen(true)}>
                <Plus aria-hidden />
                New persona
              </Button>
            ) : null}
          </>
        }
      />

      {!canEdit ? (
        <p className="text-xs text-muted-foreground">
          Read-only: creating and applying a persona needs a write-capable account.
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
            title="No personas yet"
            description="A persona is one SOUL.md you can give to any number of agents. Write it once, add variables such as {{agent_name}} or {{tone}}, and each instance renders its own text."
            action={
              canEdit ? (
                <Button onClick={() => setCreateOpen(true)}>
                  <Plus aria-hidden />
                  Create the first persona
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
          Open a persona to edit its SOUL.md, preview the rendered text and apply it to instances.
        </p>
      ) : null}

      {canEdit ? (
        <PersonaCreateDialog
          open={createOpen}
          onOpenChange={setCreateOpen}
          onCreated={(id) => {
            toast.success("Persona created.");
            navigate(`/personas/${id}`);
          }}
        />
      ) : null}
    </>
  );
}
