import { ArrowUpRight, type LucideIcon } from "lucide-react";

/**
 * A hub card is a summary and a link, never a second copy of the page.
 * Used by Réglages and Bibliothèque.
 */
export function HubCard({
  href,
  icon: Icon,
  title,
  description,
}: {
  href: string;
  icon: LucideIcon;
  title: string;
  description: string;
}) {
  return (
    <a
      href={href}
      className="group flex min-h-11 items-start gap-3 rounded-lg border bg-card p-4 outline-none transition-colors hover:bg-accent/40 focus-visible:ring-[3px] focus-visible:ring-ring/50"
    >
      <span className="flex size-9 shrink-0 items-center justify-center rounded-md border bg-muted/40 text-muted-foreground transition-colors group-hover:text-foreground">
        <Icon aria-hidden className="size-4" />
      </span>
      <span className="min-w-0 flex-1 space-y-1">
        <span className="block text-sm font-medium">{title}</span>
        <span className="block text-sm text-muted-foreground text-pretty">{description}</span>
      </span>
      <ArrowUpRight
        aria-hidden
        className="mt-0.5 size-4 shrink-0 text-muted-foreground/60 transition-transform group-hover:-translate-y-0.5 group-hover:translate-x-0.5 motion-reduce:transition-none"
      />
    </a>
  );
}
