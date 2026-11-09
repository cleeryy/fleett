import { useEffect, useState, type ReactNode } from "react";
import { Inbox, MessagesSquare, RefreshCw, Send } from "lucide-react";
import { api } from "@/api";
import type { Message, MessageKind } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
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
import {
  MessageDetailSheet,
  MessageKindBadge,
  MessageRoute,
  MessageStatusBadge,
  messagePreview,
  SendMessageDialog,
} from "@/components/messages";
import { formatClock, formatDateTime, formatRelative, plural } from "@/lib/format";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

const ALL = "all";

function FilterSelect({
  id,
  label,
  value,
  onChange,
  disabled,
  children,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="min-w-0 space-y-1.5">
      <Label htmlFor={id} className="text-xs text-muted-foreground">
        {label}
      </Label>
      <Select value={value} onValueChange={onChange} disabled={disabled}>
        <SelectTrigger id={id} className="w-full" aria-label={label}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>{children}</SelectContent>
      </Select>
    </div>
  );
}

function TimeCell({ message }: { message: Message }) {
  return (
    <span className="font-mono text-xs" title={`${formatRelative(message.created_at)} · created`}>
      {formatDateTime(message.created_at)}
    </span>
  );
}

export function MessagesPage() {
  const canEdit = useCanEdit();
  const [instance, setInstance] = useState(ALL);
  const [from, setFrom] = useState(ALL);
  const [to, setTo] = useState(ALL);
  const [kind, setKind] = useState(ALL);
  const [limit, setLimit] = useState("100");
  const [nonce, setNonce] = useState(0);
  const [refreshedAt, setRefreshedAt] = useState<Date | null>(null);
  const [selected, setSelected] = useState<Message | null>(null);
  const [detailOpen, setDetailOpen] = useState(false);
  const [sendOpen, setSendOpen] = useState(false);

  const instances = useResource(() => api.listInstances(), []);
  const messages = useResource(
    () =>
      api.listMessages({
        instance: instance === ALL ? undefined : instance,
        from: from === ALL ? undefined : from,
        to: to === ALL ? undefined : to,
        kind: kind === ALL ? undefined : (kind as MessageKind),
        limit: Number(limit),
      }),
    [instance, from, to, kind, limit, nonce],
  );

  useEffect(() => {
    if (messages.data !== null) setRefreshedAt(new Date());
  }, [messages.data]);

  const rows = messages.data ?? [];
  const instanceList = [...(instances.data ?? [])].sort((left, right) =>
    left.id.localeCompare(right.id),
  );
  const canSend = canEdit && instanceList.length > 0;
  const filtered = instance !== ALL || from !== ALL || to !== ALL || kind !== ALL;

  function open(message: Message) {
    setSelected(message);
    setDetailOpen(true);
  }

  function clearFilters() {
    setInstance(ALL);
    setFrom(ALL);
    setTo(ALL);
    setKind(ALL);
  }

  const emptyState = filtered ? (
    <EmptyState
      icon={Inbox}
      title="No messages match the filters"
      description="Clear a filter or pick another instance. Expired messages are excluded from every read."
      action={
        <Button variant="outline" size="sm" onClick={clearFilters}>
          Clear filters
        </Button>
      }
    />
  ) : (
    <EmptyState
      icon={MessagesSquare}
      title="No messages yet"
      description="Agents write to their peers through the bus. Send one here, or let an agent queue its first message."
      action={
        canSend ? (
          <Button size="sm" onClick={() => setSendOpen(true)}>
            <Send aria-hidden />
            Send message
          </Button>
        ) : undefined
      }
    />
  );

  return (
    <>
      <PageHeader
        title="Messages"
        description={
          messages.data
            ? `${rows.length} ${plural(rows.length, "message")} · newest first · expired messages excluded`
            : "Loading the fleet bus."
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={messages.refreshing ? "animate-spin" : undefined} />
              Refresh
            </Button>
            {canEdit ? (
              <Button
                size="sm"
                onClick={() => setSendOpen(true)}
                disabled={!instances.data || instanceList.length === 0}
              >
                <Send aria-hidden />
                Send message
              </Button>
            ) : null}
          </>
        }
      />

      {messages.error ? <ErrorAlert error={messages.error} onRetry={messages.reload} /> : null}

      <section
        aria-label="Message filters"
        className="grid gap-3 rounded-md border p-3 sm:grid-cols-2 lg:grid-cols-5"
      >
        <FilterSelect
          id="messages-instance"
          label="Instance"
          value={instance}
          onChange={(value) => {
            setInstance(value);
            setFrom(ALL);
            setTo(ALL);
          }}
          disabled={!instances.data}
        >
          <SelectItem value={ALL}>Any instance</SelectItem>
          {instanceList.map((entry) => (
            <SelectItem key={entry.id} value={entry.id}>
              <span className="font-mono">{entry.id}</span>
              <span className="text-muted-foreground">{entry.name}</span>
            </SelectItem>
          ))}
        </FilterSelect>

        <FilterSelect
          id="messages-from"
          label="From"
          value={from}
          onChange={setFrom}
          disabled={!instances.data}
        >
          <SelectItem value={ALL}>Any sender</SelectItem>
          {instanceList.map((entry) => (
            <SelectItem key={entry.id} value={entry.id}>
              <span className="font-mono">{entry.id}</span>
            </SelectItem>
          ))}
        </FilterSelect>

        <FilterSelect
          id="messages-to"
          label="To"
          value={to}
          onChange={setTo}
          disabled={!instances.data}
        >
          <SelectItem value={ALL}>Any recipient</SelectItem>
          {instanceList.map((entry) => (
            <SelectItem key={entry.id} value={entry.id}>
              <span className="font-mono">{entry.id}</span>
            </SelectItem>
          ))}
        </FilterSelect>

        <FilterSelect id="messages-kind" label="Kind" value={kind} onChange={setKind}>
          <SelectItem value={ALL}>Any kind</SelectItem>
          <SelectItem value="ask">ask</SelectItem>
          <SelectItem value="tell">tell</SelectItem>
          <SelectItem value="result">result</SelectItem>
        </FilterSelect>

        <div className="flex items-end gap-2">
          <div className="min-w-0 flex-1">
            <FilterSelect id="messages-limit" label="Limit" value={limit} onChange={setLimit}>
              <SelectItem value="50">50 messages</SelectItem>
              <SelectItem value="100">100 messages</SelectItem>
              <SelectItem value="200">200 messages</SelectItem>
              <SelectItem value="500">500 messages</SelectItem>
            </FilterSelect>
          </div>
          {filtered ? (
            <Button variant="outline" size="sm" className="mb-0 shrink-0" onClick={clearFilters}>
              Clear
            </Button>
          ) : null}
        </div>
      </section>

      <p className="text-xs text-muted-foreground" aria-live="polite">
        {refreshedAt ? `Updated ${formatClock(refreshedAt)}` : "…"}
        {filtered ? " · filters applied" : ""}
      </p>

      <div className="rounded-md border" aria-busy={messages.loading || messages.refreshing}>
        {messages.loading && !messages.data ? (
          <>
            <div className="hidden space-y-3 p-4 md:block">
              {Array.from({ length: 5 }).map((_, index) => (
                <Skeleton key={index} className="h-12 w-full" />
              ))}
            </div>
            <div className="divide-y md:hidden">
              {Array.from({ length: 4 }).map((_, index) => (
                <div key={index} className="space-y-2 p-4">
                  <Skeleton className="h-4 w-40" />
                  <Skeleton className="h-4 w-24" />
                  <Skeleton className="h-3 w-full" />
                </div>
              ))}
            </div>
          </>
        ) : rows.length === 0 ? (
          emptyState
        ) : (
          <>
            <div className="hidden md:block">
              <Table className="table-fixed">
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[170px]">Time</TableHead>
                    <TableHead className="w-[190px]">From → To</TableHead>
                    <TableHead className="w-[80px]">Kind</TableHead>
                    <TableHead className="w-[110px]">Status</TableHead>
                    <TableHead className="w-[70px] text-center">Hops</TableHead>
                    <TableHead>Body</TableHead>
                    <TableHead className="w-[80px]">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((message) => (
                    <TableRow
                      key={message.id}
                      className="cursor-pointer"
                      onClick={() => open(message)}
                    >
                      <TableCell>
                        <TimeCell message={message} />
                      </TableCell>
                      <TableCell>
                        <MessageRoute message={message} />
                      </TableCell>
                      <TableCell>
                        <MessageKindBadge kind={message.kind} />
                      </TableCell>
                      <TableCell>
                        <MessageStatusBadge status={message.status} />
                      </TableCell>
                      <TableCell className="text-center">
                        <span className="font-mono text-xs">{message.hops}</span>
                      </TableCell>
                      <TableCell className="max-w-0">
                        <span className="block truncate text-sm text-muted-foreground">
                          {messagePreview(message.body, 160)}
                        </span>
                      </TableCell>
                      <TableCell className="text-right">
                        <Button
                          variant="outline"
                          size="sm"
                          aria-label={`Open message from ${message.from} to ${message.to}`}
                          onClick={(event) => {
                            event.stopPropagation();
                            open(message);
                          }}
                        >
                          Open
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((message) => (
                <button
                  key={message.id}
                  type="button"
                  className="block w-full space-y-2 p-4 text-left transition-colors hover:bg-muted/40 focus-visible:bg-muted/40"
                  aria-label={`Open message from ${message.from} to ${message.to}`}
                  onClick={() => open(message)}
                >
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <MessageRoute message={message} />
                    <span className="font-mono text-[11px] text-muted-foreground">
                      {formatDateTime(message.created_at)}
                    </span>
                  </div>
                  <div className="flex flex-wrap items-center gap-1.5">
                    <MessageKindBadge kind={message.kind} />
                    <MessageStatusBadge status={message.status} />
                    <Badge variant="outline" className="font-mono text-[11px] font-normal">
                      {message.hops} hop{message.hops === 1 ? "" : "s"}
                    </Badge>
                  </div>
                  <span className="block text-sm text-muted-foreground">
                    {messagePreview(message.body, 180)}
                  </span>
                </button>
              ))}
            </div>
          </>
        )}
      </div>

      <MessageDetailSheet
        message={selected}
        open={detailOpen}
        onOpenChange={(open) => {
          setDetailOpen(open);
          if (!open) setSelected(null);
        }}
        canEdit={canEdit}
        onChanged={() => setNonce((value) => value + 1)}
      />

      <SendMessageDialog
        open={sendOpen}
        onOpenChange={setSendOpen}
        instances={instanceList}
        onSent={() => setNonce((value) => value + 1)}
      />
    </>
  );
}
