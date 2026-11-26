import { toast } from "sonner";
import { Segmented } from "@/components/segmented";
import { useI18n } from "@/lib/i18n";
import { useUiMode, type UiMode } from "@/lib/mode";

/**
 * Simple / Advanced switch (#71). The preference lives in `useUiMode`; this
 * component only presents it and confirms the change with a toast, so the
 * sidebar never changes under the operator without saying so.
 */
export function ModeToggle({
  className,
  size = "sm",
  fullWidth = false,
}: {
  className?: string;
  size?: "sm" | "md";
  fullWidth?: boolean;
}) {
  const { mode, setMode } = useUiMode();
  const { t } = useI18n();

  function change(next: UiMode) {
    if (next === mode) return;
    setMode(next);
    toast.success(next === "advanced" ? t("shell.modeAdvancedOn") : t("shell.modeSimpleOn"));
  }

  return (
    <Segmented
      value={mode}
      onChange={change}
      label={t("shell.mode")}
      size={size}
      fullWidth={fullWidth}
      className={className}
      options={[
        { value: "simple", label: t("shell.modeSimple"), title: t("shell.modeSimpleHint") },
        {
          value: "advanced",
          label: t("shell.modeAdvanced"),
          title: t("shell.modeAdvancedHint"),
        },
      ]}
    />
  );
}
