import { useState } from "react";
import { Brain, Inbox, MessagesSquare, Plus, RefreshCw, Send, CheckCheck } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { MemoryEntry, Message, MessageStatus, MessageBox } from "@/api/types";
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
import {
  MemoryDetailSheet,
  MemoryFormDialog,
  MemoryTagList,
  NamespaceBadge,
  memoryPreview,
} from "@/components/memory";
import {
  MessageDetailSheet,
  MessageKindBadge,
  MessageRoute,
  MessageStatusBadge,
  messagePreview,
  SendMessageDialog,
} from "@/components/messages";
import { formatDateTime, formatRelative, plural } from "@/lib/format";
import { useResource } from "@/lib/use-resource";

const ALL = "all";

function ViewSelect({ value, onChange }: { value: MessageBox; onChange: (value: MessageBox) => void }) {
  return (
    <Select value={value} onValueChange={(next) => onChange(next as MessageBox)}>
      <SelectTrigger className="w-32" aria-label="Mailbox view">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="inbox">Inbox</SelectItem>
        <SelectItem value="outbox">Outbox</SelectItem>
        <SelectItem value="all">Both</SelectItem>
      </SelectContent>
    </Select>
  );
}

function StatusSelect({
  value,
  onChange,
}: {
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <Select value={value} onValueChange={onChange}>
      <SelectTrigger className="w-36" aria-label="Filter by delivery status">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={ALL}>Any status</SelectItem>
        <SelectItem value="queued">queued</SelectItem>
        <SelectItem value="delivered">delivered</SelectItem>
        <SelectItem value="read">read</SelectItem>
      </SelectContent>
    </Select>
  );
}

/**
 * The mailbox of one instance. Reading the inbox stamps `delivered_at` on the
 * controller, so the tab is both a view and the delivery trigger.
 */
export function InstanceMessagesTab({ id, canEdit }: { id: string; canEdit: boolean }) {
  const [view, setView] = useState<MessageBox>("inbox");
  const [status, setStatus] = useState(ALL);
  const [nonce, setNonce] = useState(0);
  const [selected, setSelected] = useState<Message | null>(null);
  const [detailOpen, setDetailOpen] = useState(false);
  const [sendOpen, setSendOpen] = useState(false);
  const [acking, setAcking] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);

  const instances = useResource(() => api.listInstances(), []);
  const messages = useResource(
    () =>
      api.instanceMessages(id, {
        view,
        status: status === ALL ? undefined : (status as MessageStatus),
        limit: 100,
      }),
    [id, view, status, nonce],
  );

  const rows = messages.data ?? [];
  const refresh = () => setNonce((value) => value + 1);

  function open(message: Message) {
    setSelected(message);
    setDetailOpen(true);
  }

  async function ack(message: Message) {
    setAcking(message.id);
    setActionError(null);
    try {
      await api.ackMessage(message.to, message.id);
      toast.success(`Acknowledged as ${message.to}.`);
      refresh();
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setAcking(null);
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <ViewSelect value={view} onChange={setView} />
        <StatusSelect value={status} onChange={setStatus} />
        <Button variant="outline" size="sm" onClick={refresh}>
          <RefreshCw aria-hidden className={messages.refreshing ? "animate-spin" : undefined} />
          Refresh
        </Button>
        {canEdit ? (
          <Button
            size="sm"
            className="sm:ml-auto"
            onClick={() => setSendOpen(true)}
            disabled={!instances.data || instances.data.length === 0}
          >
            <Send aria-hidden />
            Send message
          </Button>
        ) : null}
      </div>

      <p className="text-xs text-muted-foreground">
        {messages.data
          ? `${rows.length} ${plural(rows.length, "message")} · ${
              view === "inbox" ? "received by" : view === "outbox" ? "sent by" : "exchanged with"
            } ${id}`
          : "Loading messages."}
      </p>

      {actionError ? <ErrorAlert error={actionError} /> : null}
      {messages.error ? <ErrorAlert error={messages.error} onRetry={messages.reload} /> : null}

      <div className="rounded-md border" aria-busy={messages.loading || messages.refreshing}>
        {messages.loading && !messages.data ? (
          <div className="space-y-3 p-4">
            {Array.from({ length: 3 }).map((_, index) => (
              <Skeleton key={index} className="h-14 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={view === "inbox" ? Inbox : MessagesSquare}
            title={
              view === "inbox"
                ? "The inbox is empty"
                : view === "outbox"
                  ? "Nothing sent yet"
                  : "No messages for this instance"
            }
            description={
              canEdit
                ? "Queue a message to another instance; it stays here until the recipient reads and acknowledges it."
                : "No message matches this view. The read-only account cannot send or acknowledge messages."
            }
            action={
              canEdit ? (
                <Button
                  size="sm"
                  onClick={() => setSendOpen(true)}
                  disabled={!instances.data || instances.data.length === 0}
                >
                  <Send aria-hidden />
                  Send message
                </Button>
              ) : undefined
            }
          />
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
                    <TableHead className="w-[60px] text-center">Hops</TableHead>
                    <TableHead>Body</TableHead>
                    <TableHead className="w-[150px]">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((message) => {
                    const canAck =
                      canEdit && message.to === id && message.status !== "read";
                    return (
                      <TableRow
                        key={message.id}
                        className="cursor-pointer"
                        onClick={() => open(message)}
                      >
                        <TableCell>
                          <span
                            className="font-mono text-xs"
                            title={formatRelative(message.created_at)}
                          >
                            {formatDateTime(message.created_at)}
                          </span>
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
                            {messagePreview(message.body, 120)}
                          </span>
                        </TableCell>
                        <TableCell>
                          <div className="flex justify-end gap-1.5">
                            {canAck ? (
                              <Button
                                variant="outline"
                                size="icon-sm"
                                aria-label={`Acknowledge message from ${message.from}`}
                                title={`Acknowledge as ${id}`}
                                disabled={acking === message.id}
                                onClick={(event) => {
                                  event.stopPropagation();
                                  void ack(message);
                                }}
                              >
                                <CheckCheck aria-hidden />
                              </Button>
                            ) : null}
                            <Button
                              variant="outline"
                              size="sm"
                              aria-label={`Open message from ${message.from}`}
                              onClick={(event) => {
                                event.stopPropagation();
                                open(message);
                              }}
                            >
                              Open
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((message) => {
                const canAck = canEdit && message.to === id && message.status !== "read";
                return (
                  <div key={message.id} className="space-y-2 p-4">
                    <button
                      type="button"
                      className="block w-full space-y-2 text-left"
                      aria-label={`Open message from ${message.from}`}
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
                        {messagePreview(message.body, 160)}
                      </span>
                    </button>
                    {canAck ? (
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={acking === message.id}
                        onClick={() => void ack(message)}
                      >
                        <CheckCheck aria-hidden />
                        Acknowledge
                      </Button>
                    ) : null}
                  </div>
                );
              })}
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
        onChanged={refresh}
      />

      <SendMessageDialog
        open={sendOpen}
        onOpenChange={setSendOpen}
        instances={instances.data ?? []}
        defaultFrom={id}
        lockSender
        onSent={refresh}
      />
    </div>
  );
}

/** Private notes of one instance (`instance:<id>`), newest first. */
export function InstanceMemoryTab({ id, canEdit }: { id: string; canEdit: boolean }) {
  const namespace = `instance:${id}`;
  const [nonce, setNonce] = useState(0);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [detailOpen, setDetailOpen] = useState(false);
  const [formOpen, setFormOpen] = useState(false);
  const [editEntry, setEditEntry] = useState<MemoryEntry | null>(null);

  const entries = useResource(
    () => api.listMemory({ namespace, limit: 100 }),
    [namespace, nonce],
  );
  const instances = useResource(() => api.listInstances(), []);

  const rows = entries.data ?? [];
  const selected = selectedId === null ? null : (rows.find((entry) => entry.id === selectedId) ?? null);
  const refresh = () => setNonce((value) => value + 1);

  function open(entry: MemoryEntry) {
    setSelectedId(entry.id);
    setDetailOpen(true);
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <p className="min-w-0 text-xs text-muted-foreground">
          Namespace <span className="font-mono">{namespace}</span> · private notes, readable by every
          authenticated client
        </p>
        <div className="flex w-full flex-wrap gap-2 sm:ml-auto sm:w-auto">
          <Button variant="outline" size="sm" onClick={refresh}>
            <RefreshCw aria-hidden className={entries.refreshing ? "animate-spin" : undefined} />
            Refresh
          </Button>
          {canEdit ? (
            <Button
              size="sm"
              onClick={() => {
                setEditEntry(null);
                setFormOpen(true);
              }}
            >
              <Plus aria-hidden />
              New entry
            </Button>
          ) : null}
        </div>
      </div>

      {entries.error ? <ErrorAlert error={entries.error} onRetry={entries.reload} /> : null}

      <div className="rounded-md border" aria-busy={entries.loading || entries.refreshing}>
        {entries.loading && !entries.data ? (
          <div className="space-y-3 p-4">
            {Array.from({ length: 3 }).map((_, index) => (
              <Skeleton key={index} className="h-14 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={Brain}
            title="No notes in this namespace"
            description={
              canEdit
                ? "Store what this agent learned: a runbook, a contract or a lesson. Promote a durable one to global later."
                : "This instance has not written a private note yet."
            }
            action={
              canEdit ? (
                <Button
                  size="sm"
                  onClick={() => {
                    setEditEntry(null);
                    setFormOpen(true);
                  }}
                >
                  <Plus aria-hidden />
                  New entry
                </Button>
              ) : undefined
            }
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table className="table-fixed">
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[170px]">Key</TableHead>
                    <TableHead className="w-[150px]">Namespace</TableHead>
                    <TableHead className="w-[55px]">Version</TableHead>
                    <TableHead className="w-[140px]">Tags</TableHead>
                    <TableHead className="w-[150px]">Updated</TableHead>
                    <TableHead>Value</TableHead>
                    <TableHead className="w-[70px]">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((entry) => (
                    <TableRow
                      key={entry.id}
                      className="cursor-pointer"
                      onClick={() => open(entry)}
                    >
                      <TableCell>
                        <span className="block font-mono text-sm break-all">{entry.key}</span>
                        <span className="block text-xs text-muted-foreground">{entry.owner}</span>
                      </TableCell>
                      <TableCell>
                        <NamespaceBadge namespace={entry.namespace} />
                      </TableCell>
                      <TableCell>
                        <span className="font-mono text-xs">v{entry.version}</span>
                      </TableCell>
                      <TableCell>
                        <MemoryTagList tags={entry.tags} compact />
                      </TableCell>
                      <TableCell>
                        <span
                          className="font-mono text-xs"
                          title={formatRelative(entry.updated_at)}
                        >
                          {formatDateTime(entry.updated_at)}
                        </span>
                      </TableCell>
                      <TableCell className="max-w-0">
                        <span className="block truncate text-sm text-muted-foreground">
                          {memoryPreview(entry.value, 120)}
                        </span>
                      </TableCell>
                      <TableCell className="text-right">
                        <Button
                          variant="outline"
                          size="sm"
                          aria-label={`Open memory entry ${entry.key}`}
                          onClick={(event) => {
                            event.stopPropagation();
                            open(entry);
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
              {rows.map((entry) => (
                <button
                  key={entry.id}
                  type="button"
                  className="block w-full space-y-2 p-4 text-left transition-colors hover:bg-muted/40 focus-visible:bg-muted/40"
                  aria-label={`Open memory entry ${entry.key}`}
                  onClick={() => open(entry)}
                >
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <span className="font-mono text-sm break-all">{entry.key}</span>
                    <span className="font-mono text-[11px] text-muted-foreground">
                      v{entry.version}
                    </span>
                  </div>
                  <div className="flex flex-wrap items-center gap-1.5">
                    <NamespaceBadge namespace={entry.namespace} />
                    {entry.tags.map((tag) => (
                      <Badge
                        key={tag}
                        variant="outline"
                        className="font-mono text-[11px] font-normal"
                      >
                        {tag}
                      </Badge>
                    ))}
                  </div>
                  <span className="block text-sm text-muted-foreground">
                    {memoryPreview(entry.value, 140)}
                  </span>
                  <span className="block font-mono text-[11px] text-muted-foreground">
                    {formatDateTime(entry.updated_at)}
                  </span>
                </button>
              ))}
            </div>
          </>
        )}
      </div>

      <MemoryDetailSheet
        entry={selected}
        open={detailOpen && selected !== null}
        onOpenChange={(open) => {
          setDetailOpen(open);
          if (!open) setSelectedId(null);
        }}
        canEdit={canEdit}
        onChanged={refresh}
        onEdit={(entry) => {
          setEditEntry(entry);
          setFormOpen(true);
        }}
      />

      <MemoryFormDialog
        open={formOpen}
        onOpenChange={(open) => {
          setFormOpen(open);
          if (!open) setEditEntry(null);
        }}
        instances={instances.data ?? []}
        entry={editEntry}
        defaultNamespace={namespace}
        onSaved={refresh}
      />
    </div>
  );
}
