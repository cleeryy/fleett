import { Fingerprint, LayoutTemplate, Server, Sparkles, Store } from "lucide-react";
import { HubCard } from "@/components/hub-card";
import { PageHeader } from "@/components/page-header";
import { useI18n } from "@/lib/i18n";

/**
 * Bibliothèque space (#71). A thin bridge to the five existing destinations
 * while the unified library page (#74) is built; it never duplicates their
 * logic and every link keeps its legacy route.
 */
export function LibraryPage() {
  const { t } = useI18n();

  return (
    <>
      <PageHeader title={t("nav.library")} description={t("shell.library.description")} />

      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
        <HubCard
          href="#/templates"
          icon={LayoutTemplate}
          title={t("shell.library.templates")}
          description={t("shell.library.templatesDescription")}
        />
        <HubCard
          href="#/personas"
          icon={Fingerprint}
          title={t("shell.library.personas")}
          description={t("shell.library.personasDescription")}
        />
        <HubCard
          href="#/skills"
          icon={Sparkles}
          title={t("shell.library.skills")}
          description={t("shell.library.skillsDescription")}
        />
        <HubCard
          href="#/mcp-servers"
          icon={Server}
          title={t("shell.library.connectors")}
          description={t("shell.library.connectorsDescription")}
        />
        <HubCard
          href="#/marketplace"
          icon={Store}
          title={t("shell.library.marketplace")}
          description={t("shell.library.marketplaceDescription")}
        />
      </div>

      <p className="text-sm text-muted-foreground text-pretty">{t("shell.library.note")}</p>
    </>
  );
}
