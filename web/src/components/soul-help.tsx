import { useState } from "react";
import { CircleHelp } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { CopyButton } from "@/components/copy-button";
import { RichText } from "@/components/rich-text";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

const SKELETON = [
  "# SOUL",
  "",
  "You are {{agent_name}}, a {{tone}} assistant.",
  "",
  "## Identity",
  "- Who this agent is, and who it works for.",
  "",
  "## Voice",
  "- Language, length and tone. What it never does.",
  "",
  "## Scope",
  "- What it can read, write and reach. What stays out of bounds.",
  "",
  "## Refusals",
  "- The requests it declines, and what it says instead.",
  "",
  "## Response format",
  "- How an answer is laid out: recap, body, next step.",
].join("\n");

export const SOUL_SKELETON = SKELETON;

/** The "What is SOUL.md?" help panel, reachable from every persona editor. */
export function SoulHelpDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useI18n();
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh_-_2rem)] grid-rows-[auto_minmax(0,1fr)] sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("personas.soulHelp.title")}</DialogTitle>
          <DialogDescription>{t("personas.soulHelp.description")}</DialogDescription>
        </DialogHeader>

        <div className="-mr-2 min-h-0 space-y-5 overflow-y-auto pr-2">
          <div className="space-y-2">
            <h3 className="text-sm font-medium">{t("personas.soulHelp.whereTitle")}</h3>
            <p className="text-sm text-muted-foreground">
              <RichText text={t("personas.soulHelp.whereBody")} />
            </p>
          </div>

          <div className="space-y-2">
            <h3 className="text-sm font-medium">{t("personas.soulHelp.contentTitle")}</h3>
            <ul className="space-y-2 text-sm text-muted-foreground">
              <li>
                <span className="text-foreground">{t("personas.soulHelp.identity")}</span>
                {t("personas.soulHelp.identityBody")}
              </li>
              <li>
                <span className="text-foreground">{t("personas.soulHelp.tone")}</span>
                {t("personas.soulHelp.toneBody")}
              </li>
              <li>
                <span className="text-foreground">{t("personas.soulHelp.scope")}</span>
                {t("personas.soulHelp.scopeBody")}
              </li>
              <li>
                <span className="text-foreground">{t("personas.soulHelp.refusals")}</span>
                {t("personas.soulHelp.refusalsBody")}
              </li>
              <li>
                <span className="text-foreground">{t("personas.soulHelp.format")}</span>
                {t("personas.soulHelp.formatBody")}
              </li>
            </ul>
          </div>

          <div className="space-y-2">
            <h3 className="text-sm font-medium">{t("personas.soulHelp.variablesTitle")}</h3>
            <p className="text-sm text-muted-foreground">
              <RichText text={t("personas.soulHelp.variablesBody")} />
            </p>
          </div>

          <div className="space-y-2">
            <div className="flex items-center justify-between gap-2">
              <h3 className="text-sm font-medium">{t("personas.soulHelp.skeletonTitle")}</h3>
              <CopyButton
                value={SKELETON}
                label={t("personas.soulHelp.copySkeleton")}
                showLabel
                size="sm"
                variant="outline"
              />
            </div>
            <pre className="overflow-auto rounded-md border bg-muted/20 p-3 font-mono text-xs leading-relaxed whitespace-pre">
              {SKELETON}
            </pre>
          </div>

          <div className="space-y-2">
            <h3 className="text-sm font-medium">{t("personas.soulHelp.driftTitle")}</h3>
            <p className="text-sm text-muted-foreground">
              <RichText text={t("personas.soulHelp.driftBody")} />
            </p>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

export function SoulHelpButton({
  label,
  className,
}: {
  label?: string;
  className?: string;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className={cn(className)}
        onClick={() => setOpen(true)}
      >
        <CircleHelp aria-hidden />
        {label ?? t("personas.soulHelp.title")}
      </Button>
      <SoulHelpDialog open={open} onOpenChange={setOpen} />
    </>
  );
}
