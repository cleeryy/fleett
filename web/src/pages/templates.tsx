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
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

export function TemplatesPage() {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const templates = useResource(() => api.listTemplates(), []);
  const rows = templates.data ?? [];

  return (
    <>
      <PageHeader
        title={t("nav.templates")}
        description={
          templates.data
            ? t(rows.length === 1 ? "templates.countOne" : "templates.countMany", {
                count: rows.length,
              })
            : t("templates.loading")
        }
        actions={
          <Button variant="outline" size="sm" onClick={templates.reload}>
            <RefreshCw aria-hidden className={templates.refreshing ? "animate-spin" : undefined} />
            {t("common.refresh")}
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
            title={t("templates.emptyTitle")}
            description={t("templates.emptyDescription")}
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[200px]">{t("templates.template")}</TableHead>
                    <TableHead>{t("common.description")}</TableHead>
                    <TableHead className="w-[240px]">{t("templates.defaultModel")}</TableHead>
                    <TableHead className="w-[110px]">{t("templates.channel")}</TableHead>
                    {canEdit ? (
                      <TableHead className="w-[90px]">
                        <span className="sr-only">{t("common.actions")}</span>
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
                      <TableCell className="font-mono text-xs">
                        {t(`channel.${template.channel}`)}
                      </TableCell>
                      {canEdit ? (
                        <TableCell className="text-right">
                          <Button asChild variant="outline" size="sm">
                            <a
                              href={hrefFor(`/instances/new?template=${encodeURIComponent(template.id)}`)}
                              aria-label={t("templates.useAria", { name: template.name })}
                            >
                              {t("templates.use")}
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
                      <dt className="text-muted-foreground">{t("templates.defaultModel")}</dt>
                      <dd className="truncate font-mono">{template.default_model}</dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">{t("templates.channel")}</dt>
                      <dd className="font-mono">{t(`channel.${template.channel}`)}</dd>
                    </div>
                  </dl>
                  {canEdit ? (
                    <Button asChild variant="outline" size="sm">
                      <a
                        href={hrefFor(`/instances/new?template=${encodeURIComponent(template.id)}`)}
                        aria-label={t("templates.useAria", { name: template.name })}
                      >
                        {t("templates.useTemplate")}
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
