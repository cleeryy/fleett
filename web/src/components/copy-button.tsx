import { useEffect, useRef, useState } from "react";
import { Check, Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { copyText } from "@/lib/clipboard";
import { cn } from "@/lib/utils";

export function CopyButton({
  value,
  label = "Copy",
  showLabel = false,
  size = "icon-sm",
  variant = "ghost",
  className,
  disabled,
}: {
  value: string;
  label?: string;
  showLabel?: boolean;
  size?: "icon-xs" | "icon-sm" | "sm";
  variant?: "ghost" | "outline" | "secondary";
  className?: string;
  disabled?: boolean;
}) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    };
  }, []);

  async function onCopy() {
    const ok = await copyText(value);
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
        aria-label={label}
        title={label}
        onClick={onCopy}
        disabled={disabled}
      >
        {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
        {showLabel ? <span>{copied ? "Copied" : "Copy"}</span> : null}
      </Button>
      <span className="sr-only" aria-live="polite">
        {copied ? "Copied to clipboard" : ""}
      </span>
    </>
  );
}
