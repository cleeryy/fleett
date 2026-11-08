import { LayoutTemplate, RefreshCw } from "lucide-react";
import { api } from "@/api";
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
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { plural } from "@/lib/format";
import { hrefFor } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

export function TemplatesPage() {
  const canEdit = useCanEdit();
  const templates = useResource(() => api.listTemplates(), []);
  const rows = templates.data ?? [];

  return (
    <>
      <PageHeader
        title="Templates"
        description={
          templates.data
            ? `${rows.length} ${plural(rows.length, "template")} · read-only · files live in templates/ on the host`
            : "Loading templates."
        }
        actions={
          <Button variant="outline" size="sm" onClick={templates.reload}>
            <RefreshCw aria-hidden className={templates.refreshing ? "animate-spin" : undefined} />
            Refresh
          </Button>
        }
      />

      {templates.error ? <ErrorAlert error={templates.error} onRetry={templates.reload} /> : null}

      <div className="rounded-md border">
        {templates.loading && !templates.data ? (
          <div className="space-y-3 p-4">
            {Array.from({ length: 4 }).map((_, index) => (
              <Skeleton key={index} className="h-12 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={LayoutTemplate}
            title="No templates found"
            description="Add templates/<id>/config.yaml on the host, then refresh. The controller renders the template at creation."
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[200px]">Template</TableHead>
                    <TableHead>Description</TableHead>
                    <TableHead className="w-[240px]">Default model</TableHead>
                    <TableHead className="w-[110px]">Channel</TableHead>
                    {canEdit ? (
                      <TableHead className="w-[90px]">
                        <span className="sr-only">Actions</span>
                      </TableHead>
                    ) : null}
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((template) => (
                    <TableRow key={template.id}>
                      <TableCell>
                        <span className="block text-sm font-medium">{template.name}</span>
                        <span className="block font-mono text-xs text-muted-foreground">{template.id}</span>
                      </TableCell>
                      <TableCell className="text-sm text-muted-foreground">{template.description}</TableCell>
                      <TableCell className="font-mono text-xs">{template.default_model}</TableCell>
                      <TableCell className="font-mono text-xs">{template.channel}</TableCell>
                      {canEdit ? (
                        <TableCell className="text-right">
                          <Button asChild variant="outline" size="sm">
                            <a
                              href={hrefFor(`/instances/new?template=${encodeURIComponent(template.id)}`)}
                              aria-label={`Use ${template.name}`}
                            >
                              Use
                            </a>
                          </Button>
                        </TableCell>
                      ) : null}
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((template) => (
                <div key={template.id} className="space-y-3 p-4">
                  <div className="space-y-0.5">
                    <p className="text-sm font-medium">{template.name}</p>
                    <p className="font-mono text-xs text-muted-foreground">{template.id}</p>
                  </div>
                  <p className="text-sm text-muted-foreground">{template.description}</p>
                  <dl className="grid grid-cols-2 gap-2 text-xs">
                    <div className="min-w-0">
                      <dt className="text-muted-foreground">Default model</dt>
                      <dd className="truncate font-mono">{template.default_model}</dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">Channel</dt>
                      <dd className="font-mono">{template.channel}</dd>
                    </div>
                  </dl>
                  {canEdit ? (
                    <Button asChild variant="outline" size="sm">
                      <a
                        href={hrefFor(`/instances/new?template=${encodeURIComponent(template.id)}`)}
                        aria-label={`Use ${template.name}`}
                      >
                        Use template
                      </a>
                    </Button>
                  ) : null}
                </div>
              ))}
            </div>
          </>
        )}
      </div>
    </>
  );
}
