import { useMemo, useState } from "react";
import {
  KeyRound,
  Plus,
  RefreshCw,
  Search,
  Server,
  Users,
  Wrench,
  X,
} from "lucide-react";
import { api } from "@/api";
import type { McpCatalogEntry, McpServerView } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { McpDeployDialog } from "@/components/mcp-deploy-dialog";
import { McpHealthBadge, McpKindBadge } from "@/components/mcp-status";
import { PageHeader } from "@/components/page-header";
import { StatusBadge } from "@/components/status-badge";
import { formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { hrefFor, navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

function ServerCard({ server }: { server: McpServerView }) {
  const { t, lang } = useI18n();
  const agents = server.instances.length;
  return (
    <li>
      <a
        href={hrefFor(`/mcp-servers/${server.id}`)}
        className="flex min-w-0 flex-col gap-3 rounded-md border p-4 outline-none transition-colors hover:border-foreground/20 hover:bg-muted/20 focus-visible:ring-[3px] focus-visible:ring-ring/50"
      >
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <p className="flex min-w-0 items-center gap-2">
              <Server aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
              <span className="truncate text-sm font-medium" title={server.name}>{server.name}</span>
            </p>
            <p className="truncate pl-5 font-mono text-xs text-muted-foreground">{server.id}</p>
          </div>
          <McpKindBadge kind={server.kind} />
        </div>

        <p className="line-clamp-2 min-h-10 text-sm text-muted-foreground">
          {server.description || t("mcp.detail.descriptionFallback")}
        </p>

        <div className="flex flex-wrap items-center gap-1.5">
          <StatusBadge status={server.status} labelPrefix="mcp.status" />
          <McpHealthBadge health={server.health} />
        </div>

        {server.last_error ? (
          <p className="line-clamp-1 text-[11px] break-words text-destructive">{server.last_error}</p>
        ) : null}

        <dl className="mt-auto grid grid-cols-2 gap-x-3 gap-y-1.5 text-[11px]">
          <div className="col-span-2 min-w-0">
            <dt className="text-muted-foreground">{t("mcp.list.endpoint")}</dt>
            <dd className="truncate font-mono" title={server.endpoint ?? server.command.join(" ")}>
              {server.endpoint ?? (server.command.join(" ") || "—")}
            </dd>
          </div>
          <div>
            <dt className="text-muted-foreground">{t("mcp.list.agentsLabel")}</dt>
            <dd className="flex items-center gap-1">
              <Users aria-hidden className="size-3 text-muted-foreground" />
              {agents === 0
                ? t("mcp.list.noAgents")
                : t(agents === 1 ? "mcp.list.agentsOne" : "mcp.list.agentsMany", { count: agents })}
            </dd>
          </div>
          <div>
            <dt className="text-muted-foreground">{t("mcp.detail.tools")}</dt>
            <dd className="flex items-center gap-1">
              <Wrench aria-hidden className="size-3 text-muted-foreground" />
              {server.tools_count === 0 ? t("mcp.list.toolsNone") : server.tools_count}
            </dd>
          </div>
        </dl>

        <p className="text-[11px] text-muted-foreground">
          {t("mcp.list.updated", { time: formatRelative(server.updated_at, lang) })}
        </p>
      </a>
    </li>
  );
}

function CatalogCard({
  entry,
  deployed,
  canEdit,
  onDeploy,
}: {
  entry: McpCatalogEntry;
  deployed?: McpServerView;
  canEdit: boolean;
  onDeploy: (entry: McpCatalogEntry) => void;
}) {
  const { t } = useI18n();
  const required = entry.env.filter((item) => item.required);
  const optional = entry.env.filter((item) => !item.required);
  return (
    <li className="flex min-w-0 flex-col gap-3 rounded-md border p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-sm font-medium" title={entry.name}>{entry.name}</p>
          <p className="truncate font-mono text-xs text-muted-foreground">{entry.id}</p>
        </div>
        <div className="flex shrink-0 flex-col items-end gap-1">
          <McpKindBadge kind={entry.kind} />
          {deployed ? (
            <Badge variant="secondary" className="font-mono text-[10px] font-normal">
              {t("mcp.list.deployedBadge")}
            </Badge>
          ) : null}
        </div>
      </div>

      <p className="line-clamp-3 min-h-15 text-sm text-muted-foreground">
        {entry.description || t("mcp.detail.descriptionFallback")}
      </p>

      <p className="truncate font-mono text-[11px] text-muted-foreground" title={entry.image}>
        {entry.image || "—"}
      </p>

      {deployed ? (
        <p className="text-[11px] text-muted-foreground">
          {t("mcp.list.toolsKnown", { count: deployed.tools_count })}
        </p>
      ) : null}

      {entry.tags.length > 0 ? (
        <ul className="flex flex-wrap gap-1">
          {entry.tags.map((tag) => (
            <li key={tag}>
              <Badge variant="secondary" className="font-mono text-[11px] font-normal">
                {tag}
              </Badge>
            </li>
          ))}
        </ul>
      ) : null}

      <div className="mt-auto space-y-1.5">
        {entry.env.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">{t("mcp.list.noEnv")}</p>
        ) : (
          <>
            {required.length > 0 ? (
              <div className="space-y-1">
                <p className="text-[11px] text-muted-foreground">{t("mcp.list.requiredEnv")}</p>
                <ul className="space-y-1">
                  {required.map((item) => (
                    <li key={item.key} className="flex items-center gap-1.5 font-mono text-[11px]">
                      <KeyRound aria-hidden className="size-3 shrink-0 text-muted-foreground" />
                      <span className="min-w-0 flex-1 truncate">{item.key}</span>
                      {item.secret ? (
                        <Badge
                          variant="outline"
                          className="shrink-0 border-amber-600/40 px-1.5 py-0 text-[10px] font-normal text-amber-800 dark:text-amber-300"
                        >
                          {t("mcp.list.secret")}
                        </Badge>
                      ) : null}
                      <Badge
                        variant="outline"
                        className="shrink-0 px-1.5 py-0 text-[10px] font-normal text-muted-foreground"
                      >
                        {t("mcp.list.required")}
                      </Badge>
                    </li>
                  ))}
                </ul>
              </div>
            ) : null}
            {optional.length > 0 ? (
              <details className="group">
                <summary className="cursor-pointer text-[11px] text-muted-foreground select-none">
                  {t("mcp.list.optionalEnv")}
                </summary>
                <ul className="mt-1 space-y-1">
                  {optional.map((item) => (
                    <li key={item.key} className="flex items-center gap-1.5 font-mono text-[11px]">
                      <KeyRound aria-hidden className="size-3 shrink-0 text-muted-foreground" />
                      <span className="min-w-0 flex-1 truncate">{item.key}</span>
                      {item.secret ? (
                        <Badge
                          variant="outline"
                          className="shrink-0 px-1.5 py-0 text-[10px] font-normal text-muted-foreground"
                        >
                          {t("mcp.list.secret")}
                        </Badge>
                      ) : null}
                    </li>
                  ))}
                </ul>
              </details>
            ) : null}
          </>
        )}

        {deployed ? (
          <Button asChild variant="outline" size="sm" className="w-full">
            <a
              href={hrefFor(`/mcp-servers/${deployed.id}`)}
              aria-label={`${t("mcp.list.open")} ${entry.name}`}
            >
              {t("mcp.list.open")}
            </a>
          </Button>
        ) : canEdit ? (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="w-full"
            onClick={() => onDeploy(entry)}
            aria-label={t("mcp.list.deployAria", { name: entry.name })}
          >
            {t("mcp.list.deployAction")}
          </Button>
        ) : null}
      </div>
    </li>
  );
}

export function McpServersPage() {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const [tab, setTab] = useState("servers");
  const [nonce, setNonce] = useState(0);
  const [query, setQuery] = useState("");
  const [deployOpen, setDeployOpen] = useState(false);
  const [deployCatalog, setDeployCatalog] = useState<McpCatalogEntry | null>(null);
  const servers = useResource(() => api.listMcpServers(), [nonce]);
  const catalog = useResource(() => api.listMcpCatalog(), [nonce]);

  const rows = servers.data ?? [];
  const entries = catalog.data ?? [];
  const filteredEntries = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return entries;
    return entries.filter((entry) =>
      `${entry.id} ${entry.name} ${entry.description} ${entry.tags.join(" ")}`
        .toLowerCase()
        .includes(needle),
    );
  }, [entries, query]);
  const attachments = rows.reduce((total, server) => total + server.instances.length, 0);
  const healthy = rows.filter((server) => server.health === "healthy").length;

  function openDeploy(entry: McpCatalogEntry | null) {
    setDeployCatalog(entry);
    setDeployOpen(true);
  }

  return (
    <>
      <PageHeader
        title={t("nav.mcpServers")}
        description={
          servers.data
            ? t("mcp.summary", {
                servers: rows.length,
                agents: attachments,
                healthy,
              })
            : t("mcp.loading")
        }
        actions={
          <>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setNonce((value) => value + 1)}
              disabled={servers.refreshing}
            >
              <RefreshCw
                aria-hidden
                className={servers.refreshing ? "animate-spin motion-reduce:animate-none" : undefined}
              />
              {t("common.refresh")}
            </Button>
            {canEdit ? (
              <Button size="sm" onClick={() => openDeploy(null)}>
                <Plus aria-hidden />
                {t("mcp.list.deploy")}
              </Button>
            ) : null}
          </>
        }
      />

      {!canEdit ? <p className="text-xs text-muted-foreground">{t("mcp.readOnly")}</p> : null}

      {servers.error ? <ErrorAlert error={servers.error} onRetry={servers.reload} /> : null}

      <Tabs value={tab} onValueChange={setTab}>
        <div className="overflow-x-auto">
          <TabsList className="w-full justify-start sm:w-fit">
            <TabsTrigger value="servers">
              {t("mcp.tab.deployed")}
              <span className="w-4 text-right font-mono text-[11px] opacity-70">
                {servers.data ? rows.length : null}
              </span>
            </TabsTrigger>
            <TabsTrigger value="catalogue">
              {t("mcp.tab.catalogue")}
              <span className="w-4 text-right font-mono text-[11px] opacity-70">
                {catalog.data ? entries.length : null}
              </span>
            </TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="servers" className="mt-4">
          {servers.loading && !servers.data ? (
            <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
              {Array.from({ length: 3 }).map((_, index) => (
                <div key={index} className="space-y-3 rounded-md border p-4">
                  <Skeleton className="h-4 w-36" />
                  <Skeleton className="h-3 w-20" />
                  <Skeleton className="h-8 w-full" />
                  <Skeleton className="h-4 w-28" />
                </div>
              ))}
            </div>
          ) : servers.error && !servers.data ? null : rows.length === 0 ? (
            <div className="rounded-md border">
              <EmptyState
                icon={Server}
                title={t("mcp.list.emptyTitle")}
                description={t("mcp.list.emptyDescription")}
                action={
                  <Button variant="outline" onClick={() => setTab("catalogue")}>
                    {t("mcp.list.emptyAction")}
                  </Button>
                }
              />
            </div>
          ) : (
            <ul
              aria-busy={servers.refreshing}
              className={cn(
                "grid gap-3 sm:grid-cols-2 xl:grid-cols-3",
                servers.refreshing && "opacity-70 transition-opacity",
              )}
            >
              {rows.map((server) => (
                <ServerCard key={server.id} server={server} />
              ))}
            </ul>
          )}
        </TabsContent>

        <TabsContent value="catalogue" className="mt-4 space-y-3">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
            <div className="relative min-w-0 flex-1 sm:max-w-sm">
              <Search
                aria-hidden
                className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground"
              />
              <Input
                type="text"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder={t("mcp.list.searchCatalogue")}
                aria-label={t("mcp.list.searchCatalogue")}
                autoComplete="off"
                spellCheck={false}
                className="pl-9 pr-9"
              />
              {query ? (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  className="absolute top-1/2 right-1.5 -translate-y-1/2"
                  onClick={() => setQuery("")}
                  aria-label={t("common.clearFilters")}
                  title={t("common.clearFilters")}
                >
                  <X aria-hidden />
                </Button>
              ) : null}
            </div>
            <p className="text-xs text-muted-foreground">{t("mcp.list.compatible")}</p>
          </div>

          {catalog.error ? <ErrorAlert error={catalog.error} onRetry={catalog.reload} /> : null}

          {catalog.loading && !catalog.data ? (
            <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
              {Array.from({ length: 4 }).map((_, index) => (
                <div key={index} className="space-y-3 rounded-md border p-4">
                  <Skeleton className="h-4 w-32" />
                  <Skeleton className="h-3 w-20" />
                  <Skeleton className="h-12 w-full" />
                  <Skeleton className="h-4 w-40" />
                </div>
              ))}
            </div>
          ) : catalog.error && !catalog.data ? null : filteredEntries.length === 0 ? (
            <div className="rounded-md border">
              <EmptyState
                icon={Server}
                title={t("mcp.list.catalogueEmpty")}
                description={t("mcp.list.catalogueEmptyDescription")}
                action={
                  query ? (
                    <Button variant="outline" size="sm" onClick={() => setQuery("")}>
                      {t("common.clearFilters")}
                    </Button>
                  ) : undefined
                }
              />
            </div>
          ) : (
            <ul className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
              {filteredEntries.map((entry) => (
                <CatalogCard
                  key={entry.id}
                  entry={entry}
                  deployed={rows.find((server) => server.id === entry.id)}
                  canEdit={canEdit}
                  onDeploy={openDeploy}
                />
              ))}
            </ul>
          )}
        </TabsContent>
      </Tabs>

      <McpDeployDialog
        open={deployOpen}
        onOpenChange={setDeployOpen}
        catalog={deployCatalog}
        onDeployed={(server) => {
          setDeployOpen(false);
          setNonce((value) => value + 1);
          navigate(`/mcp-servers/${server.id}`);
        }}
      />
    </>
  );
}
