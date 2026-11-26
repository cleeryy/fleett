import { FileClock, KeyRound, ListChecks, LockKeyhole, Plug, ShieldCheck } from "lucide-react";
import { HubCard } from "@/components/hub-card";
import { LanguageToggle } from "@/components/language-toggle";
import { ModeToggle } from "@/components/mode-toggle";
import { PageHeader } from "@/components/page-header";
import { useI18n } from "@/lib/i18n";
import { useIdentity } from "@/lib/session";

/**
 * Réglages hub (#71). Each card only summarises and links; the page behind it
 * keeps its own logic and its own RBAC guard. Theme stays in the shell.
 */
export function SettingsPage() {
  const { t } = useI18n();
  const identity = useIdentity();
  const admin = identity?.role === "admin";

  return (
    <>
      <PageHeader title={t("nav.settings")} description={t("shell.settings.description")} />

      <section
        id="preferences"
        aria-labelledby="preferences-heading"
        className="scroll-mt-24 rounded-lg border bg-muted/20 p-4 sm:p-5"
      >
        <div className="space-y-1">
          <h2 id="preferences-heading" className="text-base font-semibold">
            {t("shell.settings.preferences")}
          </h2>
          <p className="text-sm text-muted-foreground text-pretty">
            {t("shell.settings.preferencesHint")}
          </p>
        </div>

        <div className="mt-4 divide-y">
          <div className="flex flex-col gap-3 pb-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="min-w-0 space-y-0.5">
              <p className="text-sm font-medium">{t("shell.settings.mode")}</p>
              <p className="text-xs text-muted-foreground text-pretty">{t("shell.settings.modeHint")}</p>
            </div>
            <ModeToggle size="md" fullWidth className="w-full sm:w-auto" />
          </div>
          <div className="flex flex-col gap-3 pt-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="min-w-0 space-y-0.5">
              <p className="text-sm font-medium">{t("shell.settings.language")}</p>
              <p className="text-xs text-muted-foreground text-pretty">
                {t("shell.settings.languageHint")}
              </p>
            </div>
            <LanguageToggle />
          </div>
        </div>
      </section>

      {!admin ? (
        <p className="flex items-start gap-2 text-sm text-muted-foreground">
          <LockKeyhole aria-hidden className="mt-0.5 size-4 shrink-0" />
          {t("shell.settings.viewerNote")}
        </p>
      ) : null}

      <section aria-labelledby="settings-sections-heading" className="space-y-3">
        <div className="space-y-1">
          <h2 id="settings-sections-heading" className="text-base font-semibold">
            {t("shell.settings.sections")}
          </h2>
          <p className="text-sm text-muted-foreground text-pretty">{t("shell.settings.sectionsHint")}</p>
        </div>

        <div className="grid gap-3 sm:grid-cols-2">
          <HubCard
            href="#/providers"
            icon={Plug}
            title={t("shell.settings.providers")}
            description={t("shell.settings.providersDescription")}
          />
          {admin ? (
            <>
              <HubCard
                href="#/security"
                icon={ShieldCheck}
                title={t("shell.settings.security")}
                description={t("shell.settings.securityDescription")}
              />
              <HubCard
                href="#/tokens"
                icon={KeyRound}
                title={t("shell.settings.tokens")}
                description={t("shell.settings.tokensDescription")}
              />
              <HubCard
                href="#/audit"
                icon={FileClock}
                title={t("shell.settings.audit")}
                description={t("shell.settings.auditDescription")}
              />
            </>
          ) : null}
          <HubCard
            href="#/setup"
            icon={ListChecks}
            title={t("shell.settings.setup")}
            description={t("shell.settings.setupDescription")}
          />
        </div>
      </section>
    </>
  );
}
