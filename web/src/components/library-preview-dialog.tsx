import { useEffect, useRef, useState } from "react";
import { FileArchive, Users } from "lucide-react";
import type {
  MarketplaceEntry,
  McpCatalogEntry,
  McpServerView,
  PersonaSummary,
  SkillSummary,
  Template,
} from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { PersonaPreviewPanel } from "@/components/persona-preview";
import { PersonaVariableInputs } from "@/components/persona-vars";
import { useI18n } from "@/lib/i18n";
import { hrefFor } from "@/lib/router";

/** A skill card is either a registry skill or a marketplace entry. */
export type LibrarySkillPreview =
  | { kind: "skill"; skill: SkillSummary }
  | { kind: "marketplace"; entry: MarketplaceEntry; installed: boolean };

export function LibraryTemplatePreviewDialog({
  open,
  onOpenChange,
  template,
  canEdit,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  template: Template | null;
  canEdit: boolean;
}) {
  const { t } = useI18n();
  // During the exit animation the parent already cleared `template`; keep the
  // last value so the closing frame never flashes an empty dialog.
  const lastTemplate = useRef<Template | null>(null);
  if (template) lastTemplate.current = template;
  const shownTemplate = template ?? lastTemplate.current;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92dvh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {t("library.preview.templateTitle", { name: shownTemplate?.name ?? "" })}
          </DialogTitle>
          <DialogDescription>
            {shownTemplate?.description || t("skills.picker.noDescription")}
          </DialogDescription>
        </DialogHeader>

        {shownTemplate ? (
          <div className="space-y-4">
            <dl className="grid gap-2 rounded-md border px-3 py-2.5 text-xs sm:grid-cols-2">
              <div className="min-w-0">
                <dt className="text-muted-foreground">{t("templates.defaultModel")}</dt>
                <dd className="truncate font-mono" title={shownTemplate.default_model}>
                  {shownTemplate.default_model}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">{t("templates.channel")}</dt>
                <dd className="font-mono">{t(`channel.${shownTemplate.channel}`)}</dd>
              </div>
            </dl>

            <div className="space-y-2">
              <p className="text-sm font-medium">{t("library.preview.envTitle")}</p>
              {shownTemplate.env.length === 0 ? (
                <p className="rounded-md border px-3 py-2.5 text-sm text-muted-foreground">
                  {t("library.preview.noEnv")}
                </p>
              ) : (
                <ul className="divide-y rounded-md border">
                  {shownTemplate.env.map((entry) => (
                    <li key={entry.key} className="space-y-1 px-3 py-2.5">
                      <p className="flex flex-wrap items-center gap-1.5">
                        <span className="font-mono text-xs">{entry.key}</span>
                        {entry.required ? (
                          <Badge variant="outline" className="px-1.5 py-0 text-[10px] font-normal">
                            {t("library.preview.required")}
                          </Badge>
                        ) : null}
                        {entry.secret ? (
                          <Badge
                            variant="outline"
                            className="border-amber-600/40 px-1.5 py-0 text-[10px] font-normal text-amber-800 dark:text-amber-300"
                          >
                            {t("library.preview.secret")}
                          </Badge>
                        ) : null}
                      </p>
                      {entry.description ? (
                        <p className="text-xs text-muted-foreground">{entry.description}</p>
                      ) : null}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </div>
        ) : null}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("common.close")}
          </Button>
          {shownTemplate && canEdit ? (
            <Button asChild>
              <a
                href={hrefFor(`/instances/new?template=${encodeURIComponent(shownTemplate.id)}`)}
              >
                {t("templates.useTemplate")}
              </a>
            </Button>
          ) : null}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function LibraryPersonaPreviewDialog({
  open,
  onOpenChange,
  persona,
  canEdit,
  onUse,
  onApply,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  persona: PersonaSummary | null;
  canEdit: boolean;
  onUse: (persona: PersonaSummary) => void;
  onApply: (persona: PersonaSummary) => void;
}) {
  const { t } = useI18n();
  const [variables, setVariables] = useState<Record<string, string>>({});
  const lastPersona = useRef<PersonaSummary | null>(null);
  if (persona) lastPersona.current = persona;
  const shownPersona = persona ?? lastPersona.current;

  useEffect(() => {
    if (!open) return;
    setVariables({});
  }, [open, persona?.id]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92dvh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {t("library.preview.personaTitle", { name: shownPersona?.name ?? "" })}
          </DialogTitle>
          <DialogDescription>
            {shownPersona?.description || t("skills.picker.noDescription")}
          </DialogDescription>
        </DialogHeader>

        {shownPersona ? (
          <div className="space-y-4">
            <div className="space-y-2">
              <p className="text-sm font-medium">{t("library.preview.variablesTitle")}</p>
              <PersonaVariableInputs
                variables={shownPersona.variables}
                values={variables}
                onChange={setVariables}
                idPrefix="library-persona-var"
              />
            </div>
            <PersonaPreviewPanel
              personaId={shownPersona.id}
              variables={variables}
              title={t("personas.preview.title")}
              maxHeight="34vh"
            />
          </div>
        ) : null}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("common.close")}
          </Button>
          {shownPersona && canEdit ? (
            <>
              <Button variant="outline" onClick={() => onApply(shownPersona)}>
                {t("library.preview.applyPersona")}
              </Button>
              <Button onClick={() => onUse(shownPersona)}>
                {t("library.preview.usePersona")}
              </Button>
            </>
          ) : null}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function LibrarySkillPreviewDialog({
  open,
  onOpenChange,
  target,
  canEdit,
  onAttach,
  onImport,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  target: LibrarySkillPreview | null;
  canEdit: boolean;
  onAttach: (name: string) => void;
  onImport: () => void;
}) {
  const { t } = useI18n();
  const lastTarget = useRef<LibrarySkillPreview | null>(null);
  if (target) lastTarget.current = target;
  const shownTarget = target ?? lastTarget.current;

  const skill = shownTarget?.kind === "skill" ? shownTarget.skill : null;
  const entry = shownTarget?.kind === "marketplace" ? shownTarget.entry : null;
  const installed =
    shownTarget?.kind === "marketplace" ? shownTarget.installed : Boolean(skill);
  const name = skill?.name ?? entry?.name ?? "";
  const installable = Boolean(entry?.source || entry?.url);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92dvh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("library.preview.skillTitle", { name })}</DialogTitle>
          <DialogDescription>
            {skill?.description || entry?.description || t("skills.picker.noDescription")}
          </DialogDescription>
        </DialogHeader>

        {skill ? (
          <dl className="grid gap-2 rounded-md border px-3 py-2.5 text-xs sm:grid-cols-2">
            <div>
              <dt className="text-muted-foreground">{t("library.preview.latest")}</dt>
              <dd className="font-mono">{skill.latest ?? "—"}</dd>
            </div>
            <div>
              <dt className="text-muted-foreground">{t("library.preview.versionsLabel")}</dt>
              <dd>
                {t(
                  skill.versions.length === 1
                    ? "skills.picker.versionOne"
                    : "skills.picker.versionMany",
                  { count: skill.versions.length },
                )}
              </dd>
            </div>
            <div>
              <dt className="text-muted-foreground">{t("mcp.list.agentsLabel")}</dt>
              <dd>
                {t(
                  skill.instances_count === 1
                    ? "skills.picker.instanceOne"
                    : "skills.picker.instanceMany",
                  { count: skill.instances_count },
                )}
              </dd>
            </div>
            <div className="min-w-0">
              <dt className="text-muted-foreground">{t("library.preview.provenance")}</dt>
              <dd className="truncate font-mono" title={skill.source}>
                {skill.source ?? t("library.card.source.local")}
              </dd>
            </div>
          </dl>
        ) : null}

        {entry ? (
          <div className="space-y-3">
            <div className="flex flex-wrap items-center gap-1.5">
              {installed ? (
                <Badge variant="secondary" className="text-[11px] font-normal">
                  {t("library.card.state.installed")}
                </Badge>
              ) : null}
              {entry.version ? (
                <Badge variant="secondary" className="font-mono text-[11px] font-normal">
                  {entry.version}
                </Badge>
              ) : null}
              <Badge variant="outline" className="font-mono text-[11px] font-normal">
                {entry.provider}
              </Badge>
              {entry.commit ? (
                <Badge variant="outline" className="font-mono text-[11px] font-normal">
                  {t("marketplace.card.commit", { commit: entry.commit.slice(0, 7) })}
                </Badge>
              ) : null}
            </div>
            <p className="text-xs text-muted-foreground">
              {entry.author
                ? t("marketplace.card.by", { author: entry.author })
                : t("marketplace.card.noAuthor")}
            </p>
            {entry.tags && entry.tags.length > 0 ? (
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
            {!installed && !installable ? (
              <p className="rounded-md border border-amber-600/40 bg-amber-50/40 px-3 py-2.5 text-sm text-muted-foreground dark:bg-amber-950/20">
                {t("library.preview.notInstallable")}
              </p>
            ) : null}
          </div>
        ) : null}

        <DialogFooter className="flex-wrap">
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("common.close")}
          </Button>
          {canEdit && !installed && !installable ? (
            <Button variant="outline" onClick={onImport}>
              <FileArchive aria-hidden />
              {t("library.preview.importInstead")}
            </Button>
          ) : null}
          {canEdit && installed ? (
            <Button variant="outline" onClick={() => onAttach(name)}>
              {t("library.preview.attachSkill")}
            </Button>
          ) : null}
          {installed && name ? (
            <Button asChild>
              <a href={hrefFor(`/skills/${encodeURIComponent(name)}`)}>
                {t("library.preview.openSkill")}
              </a>
            </Button>
          ) : null}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function LibraryConnectorPreviewDialog({
  open,
  onOpenChange,
  entry,
  deployed,
  canEdit,
  onDeploy,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  entry: McpCatalogEntry | null;
  deployed: McpServerView | null;
  canEdit: boolean;
  onDeploy: (entry: McpCatalogEntry) => void;
}) {
  const { t } = useI18n();
  const lastEntry = useRef<McpCatalogEntry | null>(null);
  if (entry) lastEntry.current = entry;
  const shownEntry = entry ?? lastEntry.current;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92dvh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {t("library.preview.connectorTitle", { name: shownEntry?.name ?? "" })}
          </DialogTitle>
          <DialogDescription>
            {shownEntry?.description || t("skills.picker.noDescription")}
          </DialogDescription>
        </DialogHeader>

        {shownEntry ? (
          <div className="space-y-4">
            <div className="flex flex-wrap items-center gap-1.5">
              <Badge variant="outline" className="font-mono text-[11px] font-normal">
                {shownEntry.kind}
              </Badge>
              {deployed ? (
                <Badge variant="secondary" className="text-[11px] font-normal">
                  {t("library.card.state.installed")}
                </Badge>
              ) : null}
              {deployed ? (
                <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
                  <Users aria-hidden className="size-3" />
                  {t(
                    deployed.instances.length === 1
                      ? "library.card.agentsOne"
                      : "library.card.agentsMany",
                    { count: deployed.instances.length },
                  )}
                </span>
              ) : null}
            </div>

            <dl className="grid gap-2 rounded-md border px-3 py-2.5 text-xs sm:grid-cols-2">
              <div className="min-w-0">
                <dt className="text-muted-foreground">{t("library.preview.image")}</dt>
                <dd className="truncate font-mono" title={shownEntry.image}>
                  {shownEntry.image || "—"}
                </dd>
              </div>
              <div className="min-w-0">
                <dt className="text-muted-foreground">{t("mcp.list.endpoint")}</dt>
                <dd className="truncate font-mono" title={deployed?.endpoint ?? undefined}>
                  {deployed
                    ? (deployed.endpoint ?? (deployed.command.join(" ") || "—"))
                    : t("library.preview.endpointPending")}
                </dd>
              </div>
            </dl>

            <div className="space-y-2">
              <p className="text-sm font-medium">{t("library.preview.envTitle")}</p>
              {shownEntry.env.length === 0 ? (
                <p className="rounded-md border px-3 py-2.5 text-sm text-muted-foreground">
                  {t("mcp.list.noEnv")}
                </p>
              ) : (
                <ul className="divide-y rounded-md border">
                  {shownEntry.env.map((variable) => (
                    <li
                      key={variable.key}
                      className="flex flex-wrap items-center gap-1.5 px-3 py-2"
                    >
                      <span className="font-mono text-xs">{variable.key}</span>
                      {variable.required ? (
                        <Badge variant="outline" className="px-1.5 py-0 text-[10px] font-normal">
                          {t("library.preview.required")}
                        </Badge>
                      ) : null}
                      {variable.secret ? (
                        <Badge
                          variant="outline"
                          className="border-amber-600/40 px-1.5 py-0 text-[10px] font-normal text-amber-800 dark:text-amber-300"
                        >
                          {t("library.preview.secret")}
                        </Badge>
                      ) : null}
                    </li>
                  ))}
                </ul>
              )}
            </div>

            {deployed ? (
              <div className="space-y-2">
                <p className="text-sm font-medium">
                  {t("library.preview.toolsTitle")}
                  <span className="ml-1.5 font-mono text-xs font-normal text-muted-foreground">
                    {deployed.tools_count}
                  </span>
                </p>
                {deployed.tools.length === 0 ? (
                  <p className="rounded-md border px-3 py-2.5 text-sm text-muted-foreground">
                    {t("library.preview.toolsNone")}
                  </p>
                ) : (
                  <ul className="flex flex-wrap gap-1">
                    {deployed.tools.map((tool) => (
                      <li key={tool.name}>
                        <Badge
                          variant="secondary"
                          className="font-mono text-[11px] font-normal"
                        >
                          {tool.name}
                        </Badge>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            ) : null}
          </div>
        ) : null}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("common.close")}
          </Button>
          {deployed ? (
            <Button asChild>
              <a href={hrefFor(`/mcp-servers/${encodeURIComponent(deployed.id)}`)}>
                {t("library.action.open")}
              </a>
            </Button>
          ) : canEdit && shownEntry ? (
            <Button onClick={() => onDeploy(shownEntry)}>{t("library.action.deploy")}</Button>
          ) : null}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
