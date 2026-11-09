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
import { formatClock, formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
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
  const { t, lang } = useI18n();
  return (
    <span
      className="font-mono text-xs"
      title={t("messages.createdTitle", { time: formatRelative(message.created_at, lang) })}
    >
      {formatDateTime(message.created_at)}
    </span>
  );
}

export function MessagesPage() {
  const { t } = useI18n();
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
      title={t("messages.noMatchTitle")}
      description={t("messages.noMatchDescription")}
      action={
        <Button variant="outline" size="sm" onClick={clearFilters}>
          {t("common.clearFilters")}
        </Button>
      }
    />
  ) : (
    <EmptyState
      icon={MessagesSquare}
      title={t("messages.emptyTitle")}
      description={t("messages.emptyDescription")}
      action={
        canSend ? (
          <Button size="sm" onClick={() => setSendOpen(true)}>
            <Send aria-hidden />
            {t("messages.send")}
          </Button>
        ) : undefined
      }
    />
  );

  return (
    <>
      <PageHeader
        title={t("nav.messages")}
        description={
          messages.data
            ? t(rows.length === 1 ? "messages.countOne" : "messages.countMany", {
                count: rows.length,
              })
            : t("messages.loading")
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={messages.refreshing ? "animate-spin" : undefined} />
              {t("common.refresh")}
            </Button>
            {canEdit ? (
              <Button
                size="sm"
                onClick={() => setSendOpen(true)}
                disabled={!instances.data || instanceList.length === 0}
              >
                <Send aria-hidden />
                {t("messages.send")}
              </Button>
            ) : null}
          </>
        }
      />

      {messages.error ? <ErrorAlert error={messages.error} onRetry={messages.reload} /> : null}

      <section
        aria-label={t("messages.filtersAria")}
        className="grid gap-3 rounded-md border p-3 sm:grid-cols-2 lg:grid-cols-5"
      >
        <FilterSelect
          id="messages-instance"
          label={t("messages.instance")}
          value={instance}
          onChange={(value) => {
            setInstance(value);
            setFrom(ALL);
            setTo(ALL);
          }}
          disabled={!instances.data}
        >
          <SelectItem value={ALL}>{t("messages.anyInstance")}</SelectItem>
          {instanceList.map((entry) => (
            <SelectItem key={entry.id} value={entry.id}>
              <span className="font-mono">{entry.id}</span>
              <span className="text-muted-foreground">{entry.name}</span>
            </SelectItem>
          ))}
        </FilterSelect>

        <FilterSelect
          id="messages-from"
          label={t("messages.from")}
          value={from}
          onChange={setFrom}
          disabled={!instances.data}
        >
          <SelectItem value={ALL}>{t("messages.anySender")}</SelectItem>
          {instanceList.map((entry) => (
            <SelectItem key={entry.id} value={entry.id}>
              <span className="font-mono">{entry.id}</span>
            </SelectItem>
          ))}
        </FilterSelect>

        <FilterSelect
          id="messages-to"
          label={t("messages.to")}
          value={to}
          onChange={setTo}
          disabled={!instances.data}
        >
          <SelectItem value={ALL}>{t("messages.anyRecipient")}</SelectItem>
          {instanceList.map((entry) => (
            <SelectItem key={entry.id} value={entry.id}>
              <span className="font-mono">{entry.id}</span>
            </SelectItem>
          ))}
        </FilterSelect>

        <FilterSelect id="messages-kind" label={t("messages.kind")} value={kind} onChange={setKind}>
          <SelectItem value={ALL}>{t("messages.anyKind")}</SelectItem>
          <SelectItem value="ask">ask</SelectItem>
          <SelectItem value="tell">tell</SelectItem>
          <SelectItem value="result">result</SelectItem>
        </FilterSelect>

        <div className="flex items-end gap-2">
          <div className="min-w-0 flex-1">
            <FilterSelect id="messages-limit" label={t("messages.limit")} value={limit} onChange={setLimit}>
              <SelectItem value="50">{t("messages.limitN", { count: 50 })}</SelectItem>
              <SelectItem value="100">{t("messages.limitN", { count: 100 })}</SelectItem>
              <SelectItem value="200">{t("messages.limitN", { count: 200 })}</SelectItem>
              <SelectItem value="500">{t("messages.limitN", { count: 500 })}</SelectItem>
            </FilterSelect>
          </div>
          {filtered ? (
            <Button variant="outline" size="sm" className="mb-0 shrink-0" onClick={clearFilters}>
              {t("personas.apply.clear")}
            </Button>
          ) : null}
        </div>
      </section>

      <p className="text-xs text-muted-foreground" aria-live="polite">
        {refreshedAt ? t("common.updatedAt", { time: formatClock(refreshedAt) }) : "…"}
        {filtered ? t("messages.filtersApplied") : ""}
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
                    <TableHead className="w-[170px]">{t("audit.time")}</TableHead>
                    <TableHead className="w-[190px]">{t("messages.route")}</TableHead>
                    <TableHead className="w-[80px]">{t("messages.kind")}</TableHead>
                    <TableHead className="w-[110px]">{t("common.status")}</TableHead>
                    <TableHead className="w-[70px] text-center">{t("messages.hops")}</TableHead>
                    <TableHead>{t("messages.body")}</TableHead>
                    <TableHead className="w-[80px]">
                      <span className="sr-only">{t("common.actions")}</span>
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
                          aria-label={t("messages.openAria", {
                            from: message.from,
                            to: message.to,
                          })}
                          onClick={(event) => {
                            event.stopPropagation();
                            open(message);
                          }}
                        >
                          {t("common.open")}
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
                  aria-label={t("messages.openAria", {
                    from: message.from,
                    to: message.to,
                  })}
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
                      {t(
                        message.hops === 1 ? "messages.hopOne" : "messages.hopMany",
                        { count: message.hops },
                      )}
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
