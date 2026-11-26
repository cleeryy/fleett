import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Fingerprint, LayoutTemplate, Plug, Plus, RefreshCw, Sparkles } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { apiErrorMessage, toApiError } from "@/api/errors";
import type {
  InstanceSummary,
  MarketplaceEntry,
  MarketplaceUpdateInfo,
  McpCatalogEntry,
  McpServerView,
  PersonaSummary,
  SkillDetail,
  SkillSummary,
  Template,
} from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { LibraryCard, libraryActionClass } from "@/components/library-card";
import {
  LibraryFilters,
  parseLibraryType,
  type LibrarySkillSource,
  type LibraryType,
} from "@/components/library-filters";
import {
  LibraryConnectorPreviewDialog,
  LibraryPersonaPreviewDialog,
  LibrarySkillPreviewDialog,
  LibraryTemplatePreviewDialog,
  type LibrarySkillPreview,
} from "@/components/library-preview-dialog";
import { MarketplacePreviewDialog } from "@/components/marketplace-preview-dialog";
import { McpDeployDialog } from "@/components/mcp-deploy-dialog";
import { PageHeader } from "@/components/page-header";
import { PersonaApplyDialog } from "@/components/persona-apply-dialog";
import { PersonaCreateDialog } from "@/components/persona-create-dialog";
import { SkillImportDialog } from "@/components/skill-import-dialog";
import { TargetAgentDialog, type AttachTarget } from "@/components/target-agent-dialog";
import { useI18n } from "@/lib/i18n";
import { refreshMarketplaceUpdates, useMarketplaceUpdates } from "@/lib/marketplace";
import { hrefFor, navigate, useRoute } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

type PreviewTarget =
  | { kind: "template"; template: Template }
  | { kind: "persona"; persona: PersonaSummary }
  | { kind: "skill"; target: LibrarySkillPreview }
  | { kind: "connector"; entry: McpCatalogEntry; deployed: McpServerView | null };

function matchesQuery(needle: string, fields: Array<string | undefined>): boolean {
  if (!needle) return true;
  return fields.some((field) => field?.toLowerCase().includes(needle));
}

/**
 * Single entry point for templates, personas, skills and connectors. The active
 * pill lives in `?type=` so a refresh or the back button lands on the same
 * view; the search debounces locally and only reaches the marketplace from two
 * characters.
 */
export function LibraryPage() {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const route = useRoute();
  const type = parseLibraryType(route.query.get("type"));
  const [query, setQuery] = useState("");
  const [needle, setNeedle] = useState("");
  const [skillSource, setSkillSource] = useState<LibrarySkillSource>("all");
  const [nonce, setNonce] = useState(0);

  const [preview, setPreview] = useState<PreviewTarget | null>(null);
  const [marketplacePreview, setMarketplacePreview] = useState<MarketplaceEntry | null>(null);
  const [marketplacePreviewOpen, setMarketplacePreviewOpen] = useState(false);
  const [applyPersona, setApplyPersona] = useState<PersonaSummary | null>(null);
  const [applyInstances, setApplyInstances] = useState<InstanceSummary[] | null>(null);
  const [createPersonaOpen, setCreatePersonaOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [deployOpen, setDeployOpen] = useState(false);
  const [deployCatalog, setDeployCatalog] = useState<McpCatalogEntry | null>(null);
  const [attachTarget, setAttachTarget] = useState<AttachTarget | null>(null);

  useEffect(() => {
    const timer = window.setTimeout(() => setNeedle(query.trim()), 200);
    return () => window.clearTimeout(timer);
  }, [query]);

  const templates = useResource(() => api.listTemplates(), [nonce]);
  const personas = useResource(() => api.listPersonas(), [nonce]);
  const skills = useResource(() => api.listSkills(), [nonce]);
  const catalog = useResource(() => api.listMcpCatalog(), [nonce]);
  const servers = useResource(() => api.listMcpServers(), [nonce]);
  const updates = useMarketplaceUpdates();

  const marketplaceEnabled =
    needle.length >= 2 &&
    (type === "all" || (type === "skills" && skillSource !== "local"));
  const marketplace = useResource<{ term: string; entries: MarketplaceEntry[] } | null>(
    () =>
      marketplaceEnabled
        ? api
            .marketplaceSearch({ q: needle, limit: 60 })
            .then((data) => ({ term: needle, entries: data.entries }))
        : Promise.resolve(null),
    [marketplaceEnabled, needle],
  );
  // `useResource` keeps the previous answer while refreshing; the term tags it
  // so a stale page of hits never renders under a new query.
  const marketplaceEntries =
    marketplace.data?.term === needle ? marketplace.data.entries : [];
  const marketplacePending =
    marketplaceEnabled &&
    !marketplace.error &&
    (marketplace.loading || marketplace.data?.term !== needle);

  const updateMap = useMemo<Map<string, MarketplaceUpdateInfo>>(
    () =>
      new Map(
        (updates.data?.updates ?? []).map((entry) => [entry.name, entry] as const),
      ),
    [updates.data],
  );
  const installedSkills = useMemo<Map<string, SkillSummary>>(
    () =>
      new Map(
        (skills.data ?? []).map((skill) => [skill.name, skill] as const),
      ),
    [skills.data],
  );
  const deployedServers = useMemo<Map<string, McpServerView>>(
    () =>
      new Map(
        (servers.data ?? []).map((server) => [server.id, server] as const),
      ),
    [servers.data],
  );

  const haystack = needle.toLowerCase();
  const templateRows = (templates.data ?? []).filter((template) =>
    matchesQuery(haystack, [template.id, template.name, template.description]),
  );
  const personaRows = (personas.data ?? []).filter((persona) =>
    matchesQuery(haystack, [
      persona.id,
      persona.name,
      persona.description,
      persona.excerpt,
      persona.tags.join(" "),
    ]),
  );
  const skillRows = (skills.data ?? []).filter((skill) =>
    matchesQuery(haystack, [skill.name, skill.description, skill.source]),
  );
  // The Local / Marketplace chip only narrows the skills pill; other pills
  // ignore a filter the user cannot see.
  const localSkillRows =
    type === "skills" && skillSource === "marketplace" ? [] : skillRows;
  const connectorRows = (catalog.data ?? []).filter((entry) =>
    matchesQuery(haystack, [entry.id, entry.name, entry.description, entry.tags.join(" ")]),
  );
  const visibleLocalSkillNames = new Set(localSkillRows.map((skill) => skill.name));
  const marketplaceRows = marketplaceEnabled
    ? marketplaceEntries.filter((entry) => {
        if (type === "skills" && skillSource === "marketplace") return true;
        // A registry copy already on screen is not repeated; an installed hit
        // whose registry card the query filtered out still appears here.
        return !installedSkills.has(entry.name) || !visibleLocalSkillNames.has(entry.name);
      })
    : [];

  const counts: Partial<Record<LibraryType, number>> = {
    templates: templates.data?.length,
    personas: personas.data?.length,
    skills: skills.data?.length,
    connectors: catalog.data?.length,
  };
  if (templates.data && personas.data && skills.data && catalog.data) {
    counts.all =
      templates.data.length + personas.data.length + skills.data.length + catalog.data.length;
  }

  const visibleCount =
    (type === "all" || type === "templates" ? templateRows.length : 0) +
    (type === "all" || type === "personas" ? personaRows.length : 0) +
    (type === "all" || type === "skills" ? localSkillRows.length + marketplaceRows.length : 0) +
    (type === "all" || type === "connectors" ? connectorRows.length : 0);

  const loading =
    type === "templates"
      ? templates.loading && !templates.data
      : type === "personas"
        ? personas.loading && !personas.data
        : type === "skills"
          ? (skills.loading && !skills.data) || marketplacePending
          : type === "connectors"
            ? (catalog.loading && !catalog.data) || (servers.loading && !servers.data)
            : (templates.loading && !templates.data) ||
              (personas.loading && !personas.data) ||
              (skills.loading && !skills.data) ||
              (catalog.loading && !catalog.data) ||
              (servers.loading && !servers.data);

  const refreshing =
    templates.refreshing ||
    personas.refreshing ||
    skills.refreshing ||
    catalog.refreshing ||
    servers.refreshing ||
    marketplace.refreshing;

  const blockingError =
    ((type === "all" || type === "templates") && Boolean(templates.error && !templates.data)) ||
    ((type === "all" || type === "personas") && Boolean(personas.error && !personas.data)) ||
    ((type === "all" || type === "skills") && Boolean(skills.error && !skills.data)) ||
    (marketplaceEnabled && Boolean(marketplace.error && marketplace.data?.term !== needle)) ||
    ((type === "all" || type === "connectors") && Boolean(catalog.error && !catalog.data)) ||
    (type === "connectors" && Boolean(servers.error && !servers.data));

  function changeType(next: LibraryType) {
    if (next === type) return;
    navigate(next === "all" ? "/library" : `/library?type=${next}`, { replace: true });
  }

  async function openPersonaApply(persona: PersonaSummary) {
    try {
      const rows = await api.listInstances();
      setApplyInstances(rows);
      setApplyPersona(persona);
      setPreview(null);
    } catch (cause) {
      toast.error(
        t("library.apply.loadFailed", { message: apiErrorMessage(toApiError(cause), t) }),
      );
    }
  }

  function installable(entry: MarketplaceEntry): boolean {
    return Boolean(entry.source || entry.url);
  }

  function openMarketplacePreview(entry: MarketplaceEntry) {
    setMarketplacePreview(
      entry.source ? entry : { ...entry, source: `url:${entry.url ?? ""}` },
    );
    setMarketplacePreviewOpen(true);
  }

  function openDeploy(entry: McpCatalogEntry | null) {
    setDeployCatalog(entry);
    setDeployOpen(true);
  }

  function handleInstalled(detail: SkillDetail) {
    setMarketplacePreviewOpen(false);
    setNonce((value) => value + 1);
    refreshMarketplaceUpdates();
    toast.success(t("library.toast.installed", { name: detail.name }));
    setAttachTarget({ kind: "skill", name: detail.name });
  }

  function handleDeployed(server: McpServerView) {
    setDeployOpen(false);
    setNonce((value) => value + 1);
    toast.success(t("library.toast.deployed", { name: server.name }));
    setAttachTarget({ kind: "connector", id: server.id, name: server.name });
  }

  function handleImported(detail: SkillDetail) {
    setImportOpen(false);
    setNonce((value) => value + 1);
    refreshMarketplaceUpdates();
    toast.success(t("library.toast.imported", { name: detail.name }));
    setAttachTarget({ kind: "skill", name: detail.name });
  }

  const empty = blockingError
    ? null
    : (() => {
        if (visibleCount > 0) return null;
        if (type === "skills" && skillSource === "marketplace" && needle.length < 2) {
          return {
            icon: Sparkles,
            title: t("library.empty.marketplacePromptTitle"),
            description: t("library.empty.marketplacePromptDescription"),
            action: null,
          };
        }
        if (needle) {
          const marketplaceOnly = type === "skills" && skillSource === "marketplace";
          return {
            icon: Sparkles,
            title: marketplaceOnly
              ? t("library.empty.marketplaceNoResultTitle")
              : t("library.empty.filteredTitle"),
            description: marketplaceOnly
              ? t("library.empty.marketplaceNoResultDescription")
              : t("library.empty.filteredDescription", { query: needle }),
            action: (
              <Button variant="outline" size="sm" onClick={() => setQuery("")}>
                {t("common.clearFilters")}
              </Button>
            ),
          };
        }
        switch (type) {
          case "templates":
            return {
              icon: LayoutTemplate,
              title: t("library.empty.templatesTitle"),
              description: t("library.empty.templatesDescription"),
              action: canEdit ? (
                <Button asChild>
                  <a href={hrefFor("/instances/new")}>{t("library.action.createAgent")}</a>
                </Button>
              ) : null,
            };
          case "personas":
            return {
              icon: Fingerprint,
              title: t("library.empty.personasTitle"),
              description: t("library.empty.personasDescription"),
              action: canEdit ? (
                <Button onClick={() => setCreatePersonaOpen(true)}>
                  <Plus aria-hidden />
                  {t("library.action.createPersona")}
                </Button>
              ) : null,
            };
          case "skills":
            return {
              icon: Sparkles,
              title: t("library.empty.skillsTitle"),
              description: t("library.empty.skillsDescription"),
              action: canEdit ? (
                <Button onClick={() => setImportOpen(true)}>
                  <Plus aria-hidden />
                  {t("library.action.importSkill")}
                </Button>
              ) : null,
            };
          case "connectors":
            return {
              icon: Plug,
              title: t("library.empty.connectorsTitle"),
              description: t("library.empty.connectorsDescription"),
              action: canEdit ? (
                <Button onClick={() => openDeploy(null)}>
                  <Plus aria-hidden />
                  {t("library.action.deployConnector")}
                </Button>
              ) : null,
            };
          default:
            return {
              icon: LayoutTemplate,
              title: t("library.empty.allTitle"),
              description: t("library.empty.allDescription"),
              action: canEdit ? (
                <Button asChild>
                  <a href={hrefFor("/instances/new")}>{t("library.action.createAgent")}</a>
                </Button>
              ) : null,
            };
        }
      })();

  const cards: ReactNode[] = [];

  if (type === "all" || type === "templates") {
    cards.push(
      ...templateRows.map((template) => (
        <LibraryCard
          key={`template:${template.id}`}
          kind="template"
          title={template.name}
          subtitle={template.id}
          description={template.description}
          source="local"
          extraMeta={
            <>
              <span className="max-w-40 truncate font-mono text-[11px]" title={template.default_model}>
                {template.default_model}
              </span>
              <Badge variant="outline" className="font-mono text-[10px] font-normal">
                {t(`channel.${template.channel}`)}
              </Badge>
            </>
          }
          action={
            canEdit ? (
              <Button asChild variant="outline" size="sm" className={libraryActionClass}>
                <a
                  href={hrefFor(`/instances/new?template=${encodeURIComponent(template.id)}`)}
                  aria-label={t("library.card.useAria", { name: template.name })}
                >
                  {t("library.action.use")}
                </a>
              </Button>
            ) : undefined
          }
          onOpen={() => setPreview({ kind: "template", template })}
        />
      )),
    );
  }

  if (type === "all" || type === "personas") {
    cards.push(
      ...personaRows.map((persona) => (
        <LibraryCard
          key={`persona:${persona.id}`}
          kind="persona"
          title={persona.name}
          subtitle={persona.id}
          description={persona.description || persona.excerpt || ""}
          source="local"
          agents={persona.instances_count}
          action={
            canEdit ? (
              <Button
                variant="outline"
                size="sm"
                className={libraryActionClass}
                onClick={() => void openPersonaApply(persona)}
                aria-label={t("library.card.applyAria", { name: persona.name })}
              >
                {t("library.action.apply")}
              </Button>
            ) : undefined
          }
          onOpen={() => setPreview({ kind: "persona", persona })}
        />
      )),
    );
  }

  if (type === "all" || type === "skills") {
    cards.push(
      ...localSkillRows.map((skill) => {
        const update = updateMap.get(skill.name);
        return (
          <LibraryCard
            key={`skill:${skill.name}`}
            kind="skill"
            title={skill.name}
            description={skill.description}
            source={skill.commit ? "git" : "local"}
            gitSha={skill.commit}
            version={skill.latest}
            state={update ? "update" : skill.source ? "upToDate" : "installed"}
            updateVersion={update?.upstream_version}
            agents={skill.instances_count}
            action={
              <Button asChild variant="outline" size="sm" className={libraryActionClass}>
                <a
                  href={hrefFor(`/skills/${encodeURIComponent(skill.name)}`)}
                  aria-label={t("library.card.openActionAria", { name: skill.name })}
                >
                  {t("library.action.open")}
                </a>
              </Button>
            }
            onOpen={() => setPreview({ kind: "skill", target: { kind: "skill", skill } })}
          />
        );
      }),
      ...marketplaceRows.map((entry) => {
        const installedSkill = installedSkills.get(entry.name);
        const installed = Boolean(installedSkill);
        const update = installed ? updateMap.get(entry.name) : undefined;
        return (
          <LibraryCard
            key={`marketplace:${entry.provider}:${entry.source ?? entry.name}`}
            kind="marketplace"
            title={entry.name}
            description={entry.description}
            source="marketplace"
            author={entry.author}
            version={entry.version}
            state={
              installed
                ? update
                  ? "update"
                  : installedSkill?.source
                    ? "upToDate"
                    : "installed"
                : undefined
            }
            updateVersion={update?.upstream_version}
            agents={installedSkill?.instances_count}
            extraMeta={
              <Badge variant="outline" className="font-mono text-[10px] font-normal">
                {entry.provider}
              </Badge>
            }
            action={
              installed ? (
                <Button asChild variant="outline" size="sm" className={libraryActionClass}>
                  <a
                    href={hrefFor(`/skills/${encodeURIComponent(entry.name)}`)}
                    aria-label={t("library.card.openActionAria", { name: entry.name })}
                  >
                    {t("library.action.open")}
                  </a>
                </Button>
              ) : canEdit ? (
                <Button
                  variant="outline"
                  size="sm"
                  className={libraryActionClass}
                  onClick={() => openMarketplacePreview(entry)}
                  aria-label={t("library.card.installAria", { name: entry.name })}
                >
                  {t("library.action.install")}
                </Button>
              ) : undefined
            }
            onOpen={() =>
              installed || !installable(entry)
                ? setPreview({
                    kind: "skill",
                    target: { kind: "marketplace", entry, installed },
                  })
                : openMarketplacePreview(entry)
            }
          />
        );
      }),
    );
  }

  if (type === "all" || type === "connectors") {
    cards.push(
      ...connectorRows.map((entry) => {
        const deployed = deployedServers.get(entry.id);
        const required = entry.env.filter((variable) => variable.required).length;
        return (
          <LibraryCard
            key={`connector:${entry.id}`}
            kind="connector"
            title={entry.name}
            subtitle={entry.id}
            description={entry.description}
            source="local"
            state={deployed ? "installed" : undefined}
            agents={deployed?.instances.length}
            extraMeta={
              <>
                <Badge variant="outline" className="font-mono text-[10px] font-normal">
                  {entry.kind}
                </Badge>
                {!deployed && required > 0 ? (
                  <span className="text-[11px] text-muted-foreground">
                    {t(
                      required === 1
                        ? "library.card.envRequiredOne"
                        : "library.card.envRequiredMany",
                      { count: required },
                    )}
                  </span>
                ) : null}
              </>
            }
            action={
              deployed ? (
                <Button asChild variant="outline" size="sm" className={libraryActionClass}>
                  <a
                    href={hrefFor(`/mcp-servers/${encodeURIComponent(deployed.id)}`)}
                    aria-label={t("library.card.openActionAria", { name: entry.name })}
                  >
                    {t("library.action.open")}
                  </a>
                </Button>
              ) : canEdit ? (
                <Button
                  variant="outline"
                  size="sm"
                  className={libraryActionClass}
                  onClick={() => openDeploy(entry)}
                  aria-label={t("library.card.deployAria", { name: entry.name })}
                >
                  {t("library.action.deploy")}
                </Button>
              ) : undefined
            }
            onOpen={() => setPreview({ kind: "connector", entry, deployed: deployed ?? null })}
          />
        );
      }),
    );
  }

  return (
    <>
      <PageHeader
        title={t("library.title")}
        description={t("library.description")}
        actions={
          <Button
            variant="outline"
            size="sm"
            onClick={() => setNonce((value) => value + 1)}
            disabled={refreshing}
          >
            <RefreshCw
              aria-hidden
              className={refreshing ? "animate-spin motion-reduce:animate-none" : undefined}
            />
            {t("common.refresh")}
          </Button>
        }
      />

      {!canEdit ? <p className="text-xs text-muted-foreground">{t("library.readOnly")}</p> : null}

      <LibraryFilters
        type={type}
        counts={counts}
        onTypeChange={changeType}
        query={query}
        onQueryChange={setQuery}
        skillSource={skillSource}
        onSkillSourceChange={setSkillSource}
      />

      <div
        role="tabpanel"
        id="library-results"
        aria-labelledby={`library-tab-${type}`}
        className="space-y-3"
      >
        <p aria-live="polite" className="text-xs text-muted-foreground">
          {loading
            ? t("common.loading")
            : t(visibleCount === 1 ? "library.count.one" : "library.count.many", {
                count: visibleCount,
              })}
          {marketplaceEnabled ? (
            <>
              <span aria-hidden> · </span>
              {t("library.count.marketplace", { count: marketplaceRows.length })}
            </>
          ) : null}
        </p>

        {(type === "all" || type === "templates") && templates.error ? (
          <ErrorAlert error={templates.error} onRetry={templates.reload} />
        ) : null}
        {(type === "all" || type === "personas") && personas.error ? (
          <ErrorAlert error={personas.error} onRetry={personas.reload} />
        ) : null}
        {(type === "all" || type === "skills") && skills.error ? (
          <ErrorAlert error={skills.error} onRetry={skills.reload} />
        ) : null}
        {marketplaceEnabled && marketplace.error ? (
          <ErrorAlert error={marketplace.error} onRetry={marketplace.reload} />
        ) : null}
        {(type === "all" || type === "connectors") && catalog.error ? (
          <ErrorAlert error={catalog.error} onRetry={catalog.reload} />
        ) : null}
        {(type === "all" || type === "connectors") && servers.error ? (
          <ErrorAlert error={servers.error} onRetry={servers.reload} />
        ) : null}

        {loading ? (
          <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {Array.from({ length: 6 }).map((_, index) => (
              <div key={index} className="space-y-3 rounded-md border p-4">
                <div className="flex items-center gap-3">
                  <div className="size-9 animate-pulse rounded-md bg-muted" aria-hidden />
                  <div className="min-w-0 flex-1 space-y-1.5">
                    <div className="h-3.5 w-32 animate-pulse rounded bg-muted" aria-hidden />
                    <div className="h-3 w-20 animate-pulse rounded bg-muted" aria-hidden />
                  </div>
                </div>
                <div className="h-8 w-full animate-pulse rounded bg-muted" aria-hidden />
                <div className="h-3 w-28 animate-pulse rounded bg-muted" aria-hidden />
              </div>
            ))}
          </div>
        ) : empty ? (
          <div className="rounded-md border">
            <EmptyState
              icon={empty.icon}
              title={empty.title}
              description={empty.description}
              action={empty.action ?? undefined}
            />
          </div>
        ) : (
          <ul
            aria-busy={refreshing}
            className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3"
          >
            {cards}
          </ul>
        )}
      </div>

      <LibraryTemplatePreviewDialog
        open={preview?.kind === "template"}
        template={preview?.kind === "template" ? preview.template : null}
        canEdit={canEdit}
        onOpenChange={(open) => {
          if (!open) setPreview(null);
        }}
      />

      <LibraryPersonaPreviewDialog
        open={preview?.kind === "persona"}
        persona={preview?.kind === "persona" ? preview.persona : null}
        canEdit={canEdit}
        onUse={(persona) => navigate(`/instances/new?persona=${encodeURIComponent(persona.id)}`)}
        onApply={(persona) => void openPersonaApply(persona)}
        onOpenChange={(open) => {
          if (!open) setPreview(null);
        }}
      />

      <LibraryConnectorPreviewDialog
        open={preview?.kind === "connector"}
        entry={preview?.kind === "connector" ? preview.entry : null}
        deployed={preview?.kind === "connector" ? preview.deployed : null}
        canEdit={canEdit}
        onDeploy={(entry) => {
          setPreview(null);
          openDeploy(entry);
        }}
        onOpenChange={(open) => {
          if (!open) setPreview(null);
        }}
      />

      <LibrarySkillPreviewDialog
        open={preview?.kind === "skill"}
        target={preview?.kind === "skill" ? preview.target : null}
        canEdit={canEdit}
        onAttach={(name) => {
          setPreview(null);
          setAttachTarget({ kind: "skill", name });
        }}
        onImport={() => {
          setPreview(null);
          setImportOpen(true);
        }}
        onOpenChange={(open) => {
          if (!open) setPreview(null);
        }}
      />

      <MarketplacePreviewDialog
        open={marketplacePreviewOpen}
        onOpenChange={(open) => {
          if (!open) setMarketplacePreviewOpen(false);
        }}
        entry={marketplacePreview}
        canEdit={canEdit}
        onInstalled={handleInstalled}
      />

      {canEdit ? (
        <>
          <PersonaCreateDialog
            open={createPersonaOpen}
            onOpenChange={setCreatePersonaOpen}
            onCreated={() => {
              toast.success(t("personas.created"));
              setNonce((value) => value + 1);
            }}
          />
          <SkillImportDialog
            open={importOpen}
            onOpenChange={setImportOpen}
            onImported={handleImported}
            onOpenSkill={(name) => navigate(`/skills/${encodeURIComponent(name)}`)}
          />
          <McpDeployDialog
            open={deployOpen}
            onOpenChange={setDeployOpen}
            catalog={deployCatalog}
            onDeployed={handleDeployed}
          />
          <TargetAgentDialog
            open={attachTarget !== null}
            target={attachTarget}
            onOpenChange={(open) => {
              if (!open) setAttachTarget(null);
            }}
            onAttached={() => setNonce((value) => value + 1)}
          />
          {applyPersona && applyInstances ? (
            <PersonaApplyDialog
              persona={applyPersona}
              instances={applyInstances}
              open
              onOpenChange={(open) => {
                if (!open) setApplyPersona(null);
              }}
              onApplied={() => setNonce((value) => value + 1)}
            />
          ) : null}
        </>
      ) : null}
    </>
  );
}
