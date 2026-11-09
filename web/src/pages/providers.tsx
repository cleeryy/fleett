import { useEffect, useState } from "react";
import { KeyRound, Plug, Plus, RefreshCw, Trash2 } from "lucide-react";
import { api } from "@/api";
import type { ProviderSummary } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { DeleteProviderDialog } from "@/components/provider-delete-dialog";
import { ProviderFormDialog } from "@/components/provider-form-dialog";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { formatDateTime, formatRelative, plural } from "@/lib/format";
import { hrefFor, navigate, useRoute } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

function KindBadge({ kind }: { kind: string }) {
  return (
    <Badge variant="outline" className="font-mono text-xs font-normal">
      {kind}
    </Badge>
  );
}

function InstancesCount({ count }: { count: number }) {
  if (count === 0) return <span className="font-mono text-xs text-muted-foreground">0</span>;
  return (
    <span className="flex items-center gap-1.5 text-sm">
      <span className="font-mono">{count}</span>
      <span className="text-xs text-muted-foreground">{plural(count, "instance")}</span>
    </span>
  );
}

function ProviderActions({
  provider,
  canEdit,
  onDelete,
}: {
  provider: ProviderSummary;
  canEdit: boolean;
  onDelete: () => void;
}) {
  return (
    <div className="flex items-center gap-1.5">
      <Button asChild variant="outline" size="sm">
        <a href={hrefFor(`/providers/${provider.id}`)}>Manage</a>
      </Button>
      {canEdit ? (
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={`Delete provider ${provider.name}`}
          title={`Delete provider ${provider.name}`}
          onClick={onDelete}
        >
          <Trash2 aria-hidden />
        </Button>
      ) : null}
    </div>
  );
}

export function ProvidersPage() {
  const canEdit = useCanEdit();
  const route = useRoute();
  const createRequested = route.query.get("new") === "1";
  const [nonce, setNonce] = useState(0);
  const [createOpen, setCreateOpen] = useState(createRequested);
  const [deleteTarget, setDeleteTarget] = useState<ProviderSummary | null>(null);
  const providers = useResource(() => api.listProviders(), [nonce]);

  // The command palette links here as `?new=1` to open the form directly.
  useEffect(() => {
    if (createRequested) setCreateOpen(true);
  }, [createRequested]);

  function handleCreateOpenChange(open: boolean) {
    setCreateOpen(open);
    // Drop the one-shot query so a reload does not reopen the form.
    if (!open && createRequested) navigate("/providers", { replace: true });
  }

  const rows = providers.data ?? [];
  const linked = rows.reduce((total, provider) => total + provider.instances_count, 0);

  return (
    <>
      <PageHeader
        title="Providers"
        description={
          providers.data
            ? `${rows.length} ${plural(rows.length, "provider")} · ${linked} linked ${plural(
                linked,
                "instance",
              )}`
            : "Loading providers."
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={providers.refreshing ? "animate-spin" : undefined} />
              Refresh
            </Button>
            {canEdit ? (
              <Button size="sm" onClick={() => setCreateOpen(true)}>
                <Plus aria-hidden />
                New provider
              </Button>
            ) : null}
          </>
        }
      />

      {providers.error ? <ErrorAlert error={providers.error} onRetry={providers.reload} /> : null}

      <div className="rounded-md border" aria-busy={providers.loading || providers.refreshing}>
        {providers.loading && !providers.data ? (
          <div className="space-y-3 p-4">
            {Array.from({ length: 3 }).map((_, index) => (
              <Skeleton key={index} className="h-12 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={Plug}
            title="No providers yet"
            description={
              canEdit
                ? "A provider is one API credential several agents can share. Create it once, then pick it when you create or duplicate an instance."
                : "No provider exists. Your role is read-only; ask an admin to create one."
            }
            action={
              canEdit ? (
                <Button onClick={() => setCreateOpen(true)}>
                  <Plus aria-hidden />
                  New provider
                </Button>
              ) : undefined
            }
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Name</TableHead>
                    <TableHead className="w-[140px]">Kind</TableHead>
                    <TableHead>Base URL</TableHead>
                    <TableHead className="hidden lg:table-cell">API key env</TableHead>
                    <TableHead className="w-[130px]">Instances</TableHead>
                    <TableHead className="hidden w-[150px] lg:table-cell">Updated</TableHead>
                    <TableHead className="w-[150px]">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((provider) => (
                    <TableRow key={provider.id}>
                      <TableCell>
                        <a
                          href={hrefFor(`/providers/${provider.id}`)}
                          className="block max-w-[260px] rounded-sm"
                        >
                          <span className="flex min-w-0 items-center gap-2">
                            <KeyRound aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
                            <span className="min-w-0 truncate text-sm font-medium hover:underline">
                              {provider.name}
                            </span>
                          </span>
                          <span className="block truncate pl-5 font-mono text-xs text-muted-foreground">
                            {provider.id}
                          </span>
                        </a>
                      </TableCell>
                      <TableCell>
                        <KindBadge kind={provider.kind} />
                      </TableCell>
                      <TableCell>
                        <span
                          className="block max-w-[280px] truncate font-mono text-xs"
                          title={provider.base_url}
                        >
                          {provider.base_url}
                        </span>
                      </TableCell>
                      <TableCell className="hidden lg:table-cell">
                        <span className="font-mono text-xs">{provider.api_key_env}</span>
                      </TableCell>
                      <TableCell>
                        <InstancesCount count={provider.instances_count} />
                      </TableCell>
                      <TableCell className="hidden lg:table-cell">
                        <span
                          className="font-mono text-xs"
                          title={formatDateTime(provider.updated_at)}
                        >
                          {formatRelative(provider.updated_at)}
                        </span>
                      </TableCell>
                      <TableCell className="text-right">
                        <div className="flex justify-end">
                          <ProviderActions
                            provider={provider}
                            canEdit={canEdit}
                            onDelete={() => setDeleteTarget(provider)}
                          />
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((provider) => (
                <div key={provider.id} className="space-y-3 p-4">
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <div className="flex items-center gap-2">
                        <KeyRound aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
                        <a
                          href={hrefFor(`/providers/${provider.id}`)}
                          className="min-w-0 truncate text-sm font-medium"
                        >
                          {provider.name}
                        </a>
                      </div>
                      <p className="truncate pl-5 font-mono text-xs text-muted-foreground">
                        {provider.id}
                      </p>
                    </div>
                    <KindBadge kind={provider.kind} />
                  </div>
                  <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-xs">
                    <div className="col-span-2 min-w-0">
                      <dt className="text-muted-foreground">Base URL</dt>
                      <dd className="truncate font-mono" title={provider.base_url}>
                        {provider.base_url}
                      </dd>
                    </div>
                    <div className="min-w-0">
                      <dt className="text-muted-foreground">API key env</dt>
                      <dd className="truncate font-mono">{provider.api_key_env}</dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">Instances</dt>
                      <dd>
                        <InstancesCount count={provider.instances_count} />
                      </dd>
                    </div>
                    <div className="col-span-2">
                      <dt className="text-muted-foreground">Updated</dt>
                      <dd className="font-mono" title={formatDateTime(provider.updated_at)}>
                        {formatRelative(provider.updated_at)}
                      </dd>
                    </div>
                  </dl>
                  <div className="flex items-center justify-end gap-2">
                    <ProviderActions
                      provider={provider}
                      canEdit={canEdit}
                      onDelete={() => setDeleteTarget(provider)}
                    />
                  </div>
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      <ProviderFormDialog
        open={createOpen}
        onOpenChange={handleCreateOpenChange}
        onSaved={() => setNonce((value) => value + 1)}
      />

      <DeleteProviderDialog
        provider={deleteTarget}
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        onDeleted={() => {
          setDeleteTarget(null);
          setNonce((value) => value + 1);
        }}
      />
    </>
  );
}
