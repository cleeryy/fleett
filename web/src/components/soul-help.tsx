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
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh_-_2rem)] grid-rows-[auto_minmax(0,1fr)] sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>What is SOUL.md?</DialogTitle>
          <DialogDescription>
            The instructions an agent reads at startup: who it is, how it speaks, what it may
            touch, what it refuses and how it lays out an answer.
          </DialogDescription>
        </DialogHeader>

        <div className="-mr-2 min-h-0 space-y-5 overflow-y-auto pr-2">
          <div className="space-y-2">
            <h3 className="text-sm font-medium">Where it lives</h3>
            <p className="text-sm text-muted-foreground">
              Every instance keeps its own{" "}
              <span className="font-mono text-xs text-foreground">data/SOUL.md</span>. fleett writes
              it when you assign a persona; the agent reads it on startup, so a change applies
              after a restart. The file is versioned like the rest of the home.
            </p>
          </div>

          <div className="space-y-2">
            <h3 className="text-sm font-medium">What goes in it</h3>
            <ul className="space-y-2 text-sm text-muted-foreground">
              <li>
                <span className="text-foreground">Identity</span>: who the agent is, who it works
                for, what it admits not to know.
              </li>
              <li>
                <span className="text-foreground">Tone</span>: language, answer length, warmth,
                what it never sounds like.
              </li>
              <li>
                <span className="text-foreground">Scope</span>: the files, shares, tools and
                network it may use, and the parts that stay off-limits.
              </li>
              <li>
                <span className="text-foreground">Refusals</span>: the requests it declines, and
                what it says instead of complying.
              </li>
              <li>
                <span className="text-foreground">Response format</span>: the shape of an answer
                (recap, body, next step) and when to ask a question.
              </li>
            </ul>
          </div>

          <div className="space-y-2">
            <h3 className="text-sm font-medium">Variables</h3>
            <p className="text-sm text-muted-foreground">
              Write a placeholder as{" "}
              <span className="font-mono text-xs text-foreground">{"{{agent_name}}"}</span>: a key of
              lowercase letters, digits and underscores, starting with a letter. Anything else, such
              as <span className="font-mono text-xs text-foreground">{"{{MODEL}}"}</span>, is left
              untouched for the instance. Each variable has a description and an optional default;
              the value is stored on the instance, so one persona renders differently for two agents
              without copying the text. A placeholder with no value and no default stays visible and
              is listed as missing in the preview.
            </p>
          </div>

          <div className="space-y-2">
            <div className="flex items-center justify-between gap-2">
              <h3 className="text-sm font-medium">A usable skeleton</h3>
              <CopyButton value={SKELETON} label="Copy the SOUL.md skeleton" showLabel size="sm" variant="outline" />
            </div>
            <pre className="overflow-auto rounded-md border bg-muted/20 p-3 font-mono text-xs leading-relaxed whitespace-pre">
              {SKELETON}
            </pre>
          </div>

          <div className="space-y-2">
            <h3 className="text-sm font-medium">Applied, then diverged</h3>
            <p className="text-sm text-muted-foreground">
              Applying a persona rewrites{" "}
              <span className="font-mono text-xs text-foreground">data/SOUL.md</span>, snapshots the
              previous file and restarts the instance. If someone edits that file by hand
              afterwards, the instance is marked{" "}
              <span className="text-foreground">diverged</span>: the persona is still assigned, but
              the text on disk no longer matches it. Reapply the persona to take the file back over, or
              keep the drift if the manual text is what you want.
            </p>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

export function SoulHelpButton({
  label = "What is SOUL.md?",
  className,
}: {
  label?: string;
  className?: string;
}) {
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
        {label}
      </Button>
      <SoulHelpDialog open={open} onOpenChange={setOpen} />
    </>
  );
}
