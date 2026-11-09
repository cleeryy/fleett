import { useEffect, useRef, useState } from "react";
import { Check, Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { copyText } from "@/lib/clipboard";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

export function CopyButton({
  value,
  label,
  showLabel = false,
  size = "icon-sm",
  variant = "ghost",
  className,
  disabled,
}: {
  /** A string, or a function read at click time for large values. */
  value: string | (() => string);
  label?: string;
  showLabel?: boolean;
  size?: "icon-xs" | "icon-sm" | "sm";
  variant?: "ghost" | "outline" | "secondary";
  className?: string;
  disabled?: boolean;
}) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    };
  }, []);

  const labelText = label ?? t("common.copy");

  async function onCopy() {
    const text = typeof value === "function" ? value() : value;
    const ok = await copyText(text);
    if (!ok) return;
    setCopied(true);
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 2000);
  }

  return (
    <>
      <Button
        type="button"
        variant={variant}
        size={showLabel ? "sm" : size}
        className={cn(showLabel && "gap-1.5", className)}
        aria-label={labelText}
        title={labelText}
        onClick={onCopy}
        disabled={disabled}
      >
        {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
        {showLabel ? <span>{copied ? t("common.copied") : t("common.copy")}</span> : null}
      </Button>
      <span className="sr-only" aria-live="polite">
        {copied ? t("common.copiedToClipboard") : ""}
      </span>
    </>
  );
}
