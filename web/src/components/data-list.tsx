import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

export interface DataItem {
  label: string;
  value: ReactNode;
  className?: string;
}

export function DataList({ items }: { items: DataItem[] }) {
  return (
    <dl className="divide-y rounded-md border">
      {items.map((item) => (
        <div
          key={item.label}
          className="grid gap-1 px-4 py-3 sm:grid-cols-[170px_minmax(0,1fr)] sm:items-start sm:gap-4"
        >
          <dt className="text-xs text-muted-foreground">{item.label}</dt>
          <dd className={cn("min-w-0 text-sm", item.className)}>{item.value}</dd>
        </div>
      ))}
    </dl>
  );
}

export function Section({
  title,
  description,
  action,
  children,
  className,
}: {
  title: string;
  description?: string;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("space-y-3", className)}>
      <div className="flex items-center justify-between gap-3">
        <div>
          <h2 className="text-sm font-medium">{title}</h2>
          {description ? <p className="text-xs text-muted-foreground">{description}</p> : null}
        </div>
        {action}
      </div>
      {children}
    </section>
  );
}
