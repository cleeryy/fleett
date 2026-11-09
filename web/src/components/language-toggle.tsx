import { Button } from "@/components/ui/button";
import { useI18n, type Lang } from "@/lib/i18n";
import { cn } from "@/lib/utils";

const OPTIONS: { value: Lang; label: string }[] = [
  { value: "en", label: "EN" },
  { value: "fr", label: "FR" },
];

/**
 * EN / FR switch. Placed next to the theme toggle in the app shell (the mobile
 * drawer renders the same side navigation, so it is covered too) and on the
 * sign-in screen, which lives outside the shell.
 */
export function LanguageToggle({ className }: { className?: string }) {
  const { lang, setLang, t } = useI18n();
  return (
    <div
      role="group"
      aria-label={t("shell.language")}
      className={cn("inline-flex items-center gap-0.5 rounded-md border p-0.5", className)}
    >
      {OPTIONS.map((option) => {
        const active = lang === option.value;
        return (
          <Button
            key={option.value}
            type="button"
            size="xs"
            variant={active ? "secondary" : "ghost"}
            aria-pressed={active}
            aria-label={option.value === "en" ? t("shell.languageEn") : t("shell.languageFr")}
            className={cn("min-w-8 px-1.5 font-mono", !active && "text-muted-foreground")}
            onClick={() => setLang(option.value)}
          >
            {option.label}
          </Button>
        );
      })}
    </div>
  );
}
