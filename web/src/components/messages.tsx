import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { CheckCheck, Loader2, Send } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { InstanceSummary, Message, MessageKind, MessageStatus } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DataList } from "@/components/data-list";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Textarea } from "@/components/ui/textarea";
import { ErrorAlert } from "@/components/error-alert";
import { CopyButton } from "@/components/copy-button";
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** `docs/BUS.md`: 16 KiB per body, counted in bytes, not characters. */
export const MESSAGE_BODY_LIMIT = 16 * 1024;

const encoder = new TextEncoder();

export function messageBytes(value: string): number {
  return encoder.encode(value).length;
}

export function messagePreview(body: string, max = 160): string {
  const flat = body.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

const KIND_STYLES: Record<MessageKind, string> = {
  ask: "border-sky-600/30 text-sky-700 dark:text-sky-400",
  tell: "text-muted-foreground",
  result: "border-emerald-600/30 text-emerald-700 dark:text-emerald-400",
};

const STATUS_STYLES: Record<MessageStatus, { dot: string; className: string }> = {
  queued: {
    dot: "bg-amber-500",
    className: "border-amber-600/30 text-amber-700 dark:text-amber-400",
  },
  delivered: {
    dot: "bg-sky-500",
    className: "border-sky-600/30 text-sky-700 dark:text-sky-400",
  },
  read: {
    dot: "bg-emerald-500",
    className: "border-emerald-600/30 text-emerald-700 dark:text-emerald-400",
  },
};

export function MessageKindBadge({ kind }: { kind: MessageKind }) {
  const { t } = useI18n();
  return (
    <Badge variant="outline" className={cn("font-mono text-xs font-normal", KIND_STYLES[kind])}>
      {t(`messageKind.${kind}`)}
    </Badge>
  );
}

export function MessageStatusBadge({ status }: { status: MessageStatus }) {
  const { t } = useI18n();
  const style = STATUS_STYLES[status] ?? STATUS_STYLES.queued;
  return (
    <Badge
      variant="outline"
      className={cn("gap-1.5 font-mono text-xs font-normal", style.className)}
    >
      <span aria-hidden className={cn("size-1.5 shrink-0 rounded-full", style.dot)} />
      {t(`messageStatus.${status}`)}
    </Badge>
  );
}

export function MessageRoute({ message }: { message: Message }) {
  return (
    <span className="flex min-w-0 flex-wrap items-center gap-1 font-mono text-xs">
      <span className="break-all">{message.from}</span>
      <span aria-hidden className="text-muted-foreground">
        →
      </span>
      <span className="break-all">{message.to}</span>
    </span>
  );
}

/**
 * Full message, its delivery timestamps and the ack action. The recipient is
 * the instance the message is addressed to: only it can acknowledge.
 */
export function MessageDetailSheet({
  message,
  open,
  onOpenChange,
  canEdit,
  onChanged,
}: {
  message: Message | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  canEdit: boolean;
  onChanged: () => void;
}) {
  const { t } = useI18n();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setPending(false);
    setError(null);
  }, [open, message?.id]);

  async function acknowledge() {
    if (!message) return;
    setPending(true);
    setError(null);
    try {
      await api.ackMessage(message.to, message.id);
      toast.success(t("messages.acknowledged", { instance: message.to }));
      onChanged();
      onOpenChange(false);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  const canAck = canEdit && message !== null && message.status !== "read";

  return (
    <Sheet open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <SheetContent className="w-full gap-0 p-0 sm:max-w-xl">
        <SheetHeader className="border-b p-4 pr-12">
          <SheetTitle className="flex flex-wrap items-center gap-2">
            {t("messages.detailTitle")}
            {message ? <MessageKindBadge kind={message.kind} /> : null}
            {message ? <MessageStatusBadge status={message.status} /> : null}
          </SheetTitle>
          <SheetDescription asChild>
            <div className="flex min-w-0 flex-wrap items-center gap-1 font-mono text-xs">
              {message ? (
                <>
                  <span className="break-all">{message.from}</span>
                  <span aria-hidden>→</span>
                  <span className="break-all">{message.to}</span>
                  <span aria-hidden className="text-muted-foreground/60">
                    ·
                  </span>
                  <span className="break-all text-muted-foreground">{message.id}</span>
                </>
              ) : (
                t("messages.noSelection")
              )}
            </div>
          </SheetDescription>
        </SheetHeader>

        {message ? (
          <>
            <div className="min-h-0 flex-1 space-y-5 overflow-y-auto p-4">
              <section className="space-y-2">
                <h3 className="text-xs font-medium text-muted-foreground">
                  {t("messages.body")}
                </h3>
                <pre
                  tabIndex={0}
                  role="region"
                  aria-label={t("messages.bodyAria")}
                  className="max-h-80 overflow-auto rounded-md border bg-muted/20 p-3 font-mono text-xs leading-relaxed break-words whitespace-pre-wrap"
                >
                  {message.body}
                </pre>
                <p className="text-xs text-muted-foreground">
                  {t("messages.bodySize", {
                    bytes: messageBytes(message.body).toLocaleString(),
                  })}
                </p>
              </section>

              <DataList
                items={[
                  {
                    label: t("messages.correlation"),
                    value: <Mono>{message.correlation_id || "—"}</Mono>,
                  },
                  {
                    label: t("messages.hops"),
                    value: (
                      <span className="font-mono text-xs">
                        {message.hops} / 8
                        <span className="ml-2 text-muted-foreground">
                          {message.hops === 0
                            ? t("messages.conversationStart")
                            : t(
                                message.hops === 1
                                  ? "messages.roundTripOne"
                                  : "messages.roundTripMany",
                                { count: message.hops },
                              )}
                        </span>
                      </span>
                    ),
                  },
                  { label: t("messages.created"), value: <When iso={message.created_at} /> },
                  { label: t("messages.expires"), value: <When iso={message.expires_at} /> },
                  {
                    label: t("messages.delivered"),
                    value: message.delivered_at ? (
                      <When iso={message.delivered_at} />
                    ) : (
                      <span className="text-xs text-muted-foreground">
                        {t("messages.notFetched")}
                      </span>
                    ),
                  },
                  {
                    label: t("messages.read"),
                    value: message.read_at ? (
                      <When iso={message.read_at} />
                    ) : (
                      <span className="text-xs text-muted-foreground">
                        {t("messages.notAcknowledged")}
                      </span>
                    ),
                  },
                  {
                    label: t("messages.messageId"),
                    value: (
                      <span className="flex items-start gap-2">
                        <span className="min-w-0 flex-1 font-mono text-xs break-all">{message.id}</span>
                        <CopyButton
                          value={message.id}
                          label={t("messages.copyId")}
                          size="icon-xs"
                          className="-my-1 shrink-0"
                        />
                      </span>
                    ),
                  },
                ]}
              />

              {error ? <ErrorAlert error={error} /> : null}
            </div>

            <SheetFooter className="border-t p-4">
              {canAck ? (
                <Button type="button" onClick={() => void acknowledge()} disabled={pending}>
                  {pending ? <Loader2 aria-hidden className="animate-spin" /> : <CheckCheck aria-hidden />}
                  {t("messages.acknowledge", { instance: message.to })}
                </Button>
              ) : (
                <p className="text-xs text-muted-foreground">
                  {message.status === "read"
                    ? t("messages.readBy", {
                        instance: message.to,
                        time: formatDateTime(message.read_at ?? message.created_at),
                      })
                    : t("messages.ackHint")}
                </p>
              )}
            </SheetFooter>
          </>
        ) : null}
      </SheetContent>
    </Sheet>
  );
}

function Mono({ children }: { children: ReactNode }) {
  return <span className="font-mono text-xs break-all">{children}</span>;
}

function When({ iso }: { iso: string }) {
  const { lang } = useI18n();
  return (
    <span className="font-mono text-xs" title={formatRelative(iso, lang)}>
      {formatDateTime(iso)}
    </span>
  );
}

const TTL_OPTIONS = [
  { value: "300", labelKey: "messages.ttl5m" },
  { value: "3600", labelKey: "messages.ttl1h" },
  { value: "86400", labelKey: "messages.ttl24h" },
  { value: "604800", labelKey: "messages.ttl7d" },
  { value: "2592000", labelKey: "messages.ttl30d" },
];

/**
 * Compose a bus message. The sender select is admin-only on the controller, so
 * the instance tab locks it to the instance it belongs to.
 */
export function SendMessageDialog({
  open,
  onOpenChange,
  instances,
  defaultFrom,
  lockSender = false,
  onSent,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  instances: InstanceSummary[];
  defaultFrom?: string;
  lockSender?: boolean;
  onSent: (message: Message) => void;
}) {
  const { t } = useI18n();
  const ordered = useMemo(
    () => [...instances].sort((left, right) => left.id.localeCompare(right.id)),
    [instances],
  );

  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [kind, setKind] = useState<MessageKind>("tell");
  const [body, setBody] = useState("");
  const [correlation, setCorrelation] = useState("");
  const [ttl, setTtl] = useState("86400");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [fieldErrors, setFieldErrors] = useState<{ to?: string; body?: string }>({});

  useEffect(() => {
    if (!open) return;
    const sender = defaultFrom ?? ordered[0]?.id ?? "";
    setFrom(sender);
    setTo(ordered.find((instance) => instance.id !== sender)?.id ?? "");
    setKind("tell");
    setBody("");
    setCorrelation("");
    setTtl("86400");
    setPending(false);
    setError(null);
    setFieldErrors({});
    // Only reset on open: a late `instances` response must not wipe a draft.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, defaultFrom]);

  const bytes = messageBytes(body);
  const overLimit = bytes > MESSAGE_BODY_LIMIT;

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const nextErrors: { to?: string; body?: string } = {};
    if (!from) nextErrors.to = t("messages.chooseSender");
    if (!to) nextErrors.to = t("messages.chooseRecipient");
    if (from && to && from === to) nextErrors.to = t("messages.selfMessage");
    const trimmed = body.trim();
    if (!trimmed) nextErrors.body = t("messages.bodyRequired");
    else if (messageBytes(trimmed) > MESSAGE_BODY_LIMIT) {
      nextErrors.body = t("messages.bodyTooLarge");
    }
    setFieldErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) return;

    setPending(true);
    setError(null);
    try {
      const created = await api.sendMessage(from, {
        to,
        kind,
        body,
        correlation_id: correlation.trim() || undefined,
        ttl_s: Number(ttl),
      });
      toast.success(t("messages.queued", { instance: to }));
      onSent(created);
      onOpenChange(false);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-lg">
        <form onSubmit={onSubmit} noValidate className="space-y-4">
          <DialogHeader>
            <DialogTitle>{t("messages.send")}</DialogTitle>
            <DialogDescription>{t("messages.sendDescription")}</DialogDescription>
          </DialogHeader>

          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="message-from">{t("messages.from")}</Label>
              {lockSender && defaultFrom ? (
                <p
                  id="message-from"
                  className="flex h-9 items-center rounded-md border border-input bg-muted/30 px-3 font-mono text-sm"
                >
                  {defaultFrom}
                </p>
              ) : (
                <Select
                  value={from}
                  onValueChange={(value) => {
                    setFrom(value);
                    if (to === value) setTo("");
                  }}
                  disabled={ordered.length === 0}
                >
                  <SelectTrigger
                    id="message-from"
                    className="w-full"
                    aria-label={t("messages.sendingInstance")}
                  >
                    <SelectValue placeholder={t("messages.chooseInstance")} />
                  </SelectTrigger>
                  <SelectContent>
                    {ordered.map((instance) => (
                      <SelectItem key={instance.id} value={instance.id}>
                        <span className="font-mono">{instance.id}</span>
                        <span className="text-muted-foreground">{instance.name}</span>
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="message-to">{t("messages.to")}</Label>
              <Select value={to} onValueChange={setTo} disabled={ordered.length === 0}>
                <SelectTrigger
                  id="message-to"
                  className="w-full"
                  aria-label={t("messages.recipientInstance")}
                  aria-invalid={!!fieldErrors.to}
                  aria-describedby={fieldErrors.to ? "message-to-error" : undefined}
                >
                  <SelectValue placeholder={t("messages.chooseInstance")} />
                </SelectTrigger>
                <SelectContent>
                  {ordered
                    .filter((instance) => instance.id !== from)
                    .map((instance) => (
                      <SelectItem key={instance.id} value={instance.id}>
                        <span className="font-mono">{instance.id}</span>
                        <span className="text-muted-foreground">{instance.name}</span>
                      </SelectItem>
                    ))}
                </SelectContent>
              </Select>
              {fieldErrors.to ? (
                <p id="message-to-error" className="text-xs text-destructive">
                  {fieldErrors.to}
                </p>
              ) : null}
            </div>

            <div className="space-y-2">
              <Label htmlFor="message-kind">{t("messages.kind")}</Label>
              <Select value={kind} onValueChange={(value) => setKind(value as MessageKind)}>
                <SelectTrigger
                  id="message-kind"
                  className="w-full"
                  aria-label={t("messages.kindAria")}
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="ask">{t("messages.kindAsk")}</SelectItem>
                  <SelectItem value="tell">{t("messages.kindTell")}</SelectItem>
                  <SelectItem value="result">{t("messages.kindResult")}</SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-2">
              <Label htmlFor="message-ttl">{t("messages.ttl")}</Label>
              <Select value={ttl} onValueChange={setTtl}>
                <SelectTrigger
                  id="message-ttl"
                  className="w-full"
                  aria-label={t("messages.ttl")}
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {TTL_OPTIONS.map((option) => (
                    <SelectItem key={option.value} value={option.value}>
                      {t(option.labelKey)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>

          <div className="space-y-2">
            <div className="flex items-baseline justify-between gap-3">
              <Label htmlFor="message-body">{t("messages.body")}</Label>
              <span
                className={cn(
                  "font-mono text-xs",
                  overLimit ? "text-destructive" : "text-muted-foreground",
                )}
              >
                {bytes.toLocaleString()} / 16 KiB
              </span>
            </div>
            <Textarea
              id="message-body"
              rows={6}
              value={body}
              autoFocus
              placeholder={t("messages.bodyPlaceholder")}
              aria-invalid={!!fieldErrors.body || overLimit}
              aria-describedby={fieldErrors.body ? "message-body-error" : "message-body-help"}
              onChange={(event) => setBody(event.target.value)}
            />
            {fieldErrors.body ? (
              <p id="message-body-error" className="text-xs text-destructive">
                {fieldErrors.body}
              </p>
            ) : (
              <p id="message-body-help" className="text-xs text-muted-foreground">
                {t("messages.bodyHint")}
              </p>
            )}
          </div>

          <div className="space-y-2">
            <Label htmlFor="message-correlation">{t("messages.correlationId")}</Label>
            <Input
              id="message-correlation"
              value={correlation}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder={t("messages.correlationPlaceholder")}
              onChange={(event) => setCorrelation(event.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              {t("messages.correlationHint")}
            </p>
          </div>

          {error ? <ErrorAlert error={error} /> : null}

          <DialogFooter>
            <Button
              variant="outline"
              type="button"
              onClick={() => onOpenChange(false)}
              disabled={pending}
            >
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending || ordered.length === 0}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Send aria-hidden />}
              {t("messages.send")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
