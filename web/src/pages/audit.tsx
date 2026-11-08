import { useState } from "react";
import { RefreshCw, ScrollText } from "lucide-react";
import { api } from "@/api";
import type { AuditEntry } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { formatDateTime, formatRelative, plural } from "@/lib/format";
import { useResource } from "@/lib/use-resource";

function RoleBadge({ role }: { role: string }) {
  return (
    <Badge
      variant={role === "admin" ? "default" : "secondary"}
      className="font-mono text-xs font-normal"
    >
      {role}
    </Badge>
  );
}

function detailsTitle(entry: AuditEntry): string | undefined {
  if (entry.details === undefined || entry.details === null) return undefined;
  try {
    return JSON.stringify(entry.details);
  } catch {
    return undefined;
  }
}

export function AuditPage() {
  const [limit, setLimit] = useState("50");
  const audit = useResource(() => api.listAudit(Number(limit)), [limit]);
  const rows = audit.data ?? [];

  return (
    <>
      <PageHeader
        title="Audit log"
        description={
          audit.data
            ? `${rows.length} ${plural(rows.length, "action")} · newest first · successful changes only`
            : "Loading the audit trail."
        }
        actions={
          <>
            <Select value={limit} onValueChange={setLimit}>
              <SelectTrigger className="w-32" aria-label="Entries to load">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="50">50 entries</SelectItem>
                <SelectItem value="200">200 entries</SelectItem>
              </SelectContent>
            </Select>
            <Button variant="outline" size="sm" onClick={audit.reload}>
              <RefreshCw aria-hidden className={audit.refreshing ? "animate-spin" : undefined} />
              Refresh
            </Button>
          </>
        }
      />

      {audit.error ? <ErrorAlert error={audit.error} onRetry={audit.reload} /> : null}

      <div className="rounded-md border">
        {audit.loading && !audit.data ? (
          <div className="space-y-3 p-4">
            {Array.from({ length: 4 }).map((_, index) => (
              <Skeleton key={index} className="h-10 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={ScrollText}
            title="No changes recorded yet"
            description="Every successful create, update, lifecycle, share or token action lands here, newest first."
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[180px]">Time</TableHead>
                    <TableHead>Actor</TableHead>
                    <TableHead className="w-[100px]">Role</TableHead>
                    <TableHead>Action</TableHead>
                    <TableHead>Target</TableHead>
                    <TableHead className="w-[140px]">IP</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((entry) => (
                    <TableRow key={entry.id}>
                      <TableCell>
                        <span className="font-mono text-xs" title={formatRelative(entry.at)}>
                          {formatDateTime(entry.at)}
                        </span>
                      </TableCell>
                      <TableCell>
                        <span className="block text-sm break-all">{entry.actor}</span>
                        <span className="block font-mono text-[11px] text-muted-foreground">
                          {entry.method}
                        </span>
                      </TableCell>
                      <TableCell>
                        <RoleBadge role={entry.role} />
                      </TableCell>
                      <TableCell>
                        <span className="font-mono text-xs" title={detailsTitle(entry)}>
                          {entry.action}
                        </span>
                      </TableCell>
                      <TableCell>
                        <span className="font-mono text-xs break-all">{entry.target}</span>
                      </TableCell>
                      <TableCell>
                        {entry.ip ? (
                          <span className="font-mono text-xs">{entry.ip}</span>
                        ) : (
                          <span className="text-xs text-muted-foreground">—</span>
                        )}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((entry) => (
                <div key={entry.id} className="space-y-2 p-4">
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <p className="font-mono text-xs">{formatDateTime(entry.at)}</p>
                      <p className="mt-0.5 truncate text-sm">{entry.actor}</p>
                    </div>
                    <RoleBadge role={entry.role} />
                  </div>
                  <dl className="grid grid-cols-2 gap-x-4 gap-y-1.5 text-xs">
                    <div className="col-span-2">
                      <dt className="text-muted-foreground">Action</dt>
                      <dd className="font-mono">{entry.action}</dd>
                    </div>
                    <div className="col-span-2">
                      <dt className="text-muted-foreground">Target</dt>
                      <dd className="font-mono break-all">{entry.target}</dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">Method</dt>
                      <dd className="font-mono">{entry.method}</dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">IP</dt>
                      <dd className="font-mono">{entry.ip ?? "—"}</dd>
                    </div>
                  </dl>
                </div>
              ))}
            </div>
          </>
        )}
      </div>
    </>
  );
}
