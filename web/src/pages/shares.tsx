import { useEffect, useState, type FormEvent } from "react";
import { AlertCircle, FolderTree, Loader2, Plus, RefreshCw, Trash2, Unlink } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { InstanceSummary, Share } from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
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
import { RichText } from "@/components/rich-text";
import { useI18n } from "@/lib/i18n";
import { useCanEdit } from "@/lib/session";
import { isValidSlug } from "@/lib/validation";
import { useResource } from "@/lib/use-resource";

function Attachments({ share }: { share: Share }) {
  const { t } = useI18n();
  if (share.instances.length === 0) {
    return <span className="text-xs text-muted-foreground">{t("shares.notAttached")}</span>;
  }
  return (
    <div className="flex flex-wrap gap-1">
      {share.instances.map((entry) => (
        <Badge key={entry.id} variant="outline" className="gap-1 font-mono text-xs font-normal">
          {entry.id}
          <span className="text-muted-foreground">{entry.mode}</span>
        </Badge>
      ))}
    </div>
  );
}

function CreateShareDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: () => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState("");
  const [pending, setPending] = useState(false);
  const [fieldError, setFieldError] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setName("");
    setPending(false);
    setFieldError(null);
    setError(null);
  }, [open]);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = name.trim();
    if (!isValidSlug(value)) {
      setFieldError(t("clone.idFormat"));
      return;
    }
    setPending(true);
    setFieldError(null);
    setError(null);
    try {
      await api.createShare(value);
      toast.success(t("shares.created", { name: value }));
      onCreated();
      onOpenChange(false);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent>
        <form onSubmit={onSubmit} noValidate className="space-y-4">
          <DialogHeader>
            <DialogTitle>{t("shares.newTitle")}</DialogTitle>
            <DialogDescription>
              <RichText text={t("shares.newDescription")} />
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-2">
            <Label htmlFor="share-name">{t("common.name")}</Label>
            <Input
              id="share-name"
              value={name}
              autoFocus
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder={t("shares.namePlaceholder")}
              aria-invalid={!!fieldError}
              aria-describedby={fieldError ? "share-name-error" : undefined}
              onChange={(event) => setName(event.target.value.toLowerCase())}
            />
            {fieldError ? (
              <p id="share-name-error" className="text-xs text-destructive">
                {fieldError}
              </p>
            ) : null}
          </div>

          {error ? <ErrorAlert error={error} /> : null}

          <DialogFooter>
            <Button variant="outline" type="button" onClick={() => onOpenChange(false)} disabled={pending}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Plus aria-hidden />}
              {t("shares.create")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function ManageShareDialog({
  share,
  instances,
  open,
  onOpenChange,
  onChanged,
}: {
  share: Share | null;
  instances: InstanceSummary[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onChanged: () => void;
}) {
  const { t } = useI18n();
  const [instanceId, setInstanceId] = useState("");
  const [mode, setMode] = useState<"rw" | "ro">("rw");
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setInstanceId("");
    setMode("rw");
    setPending(null);
    setError(null);
  }, [open]);

  if (!share) return null;

  const available = instances.filter((instance) => !share.instances.some((entry) => entry.id === instance.id));

  async function attach() {
    if (!share || !instanceId) return;
    setPending("attach");
    setError(null);
    try {
      await api.attachShare(share.name, instanceId, mode);
      toast.success(t("shares.attached", { instance: instanceId, share: share.name }));
      setInstanceId("");
      onChanged();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(null);
    }
  }

  async function detach(id: string) {
    if (!share) return;
    setPending(id);
    setError(null);
    try {
      await api.detachShare(share.name, id);
      toast.success(t("shares.detached", { instance: id, share: share.name }));
      onChanged();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(null);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {t("shares.shareTitle")} <span className="font-mono">{share.name}</span>
          </DialogTitle>
          <DialogDescription>
            <RichText
              text={t("shares.manageDescription", {
                path: `/opt/data/shared/${share.name}`,
              })}
            />
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <p className="text-sm font-medium">{t("shares.attachedInstances")}</p>
          {share.instances.length === 0 ? (
            <p className="rounded-md border px-3 py-4 text-sm text-muted-foreground">
              {t("shares.notAttachedAny")}
            </p>
          ) : (
            <div className="divide-y rounded-md border">
              {share.instances.map((entry) => (
                <div key={entry.id} className="flex items-center justify-between gap-3 px-3 py-2">
                  <span className="flex items-center gap-2">
                    <span className="font-mono text-sm">{entry.id}</span>
                    <Badge variant="outline" className="font-mono text-xs font-normal">
                      {entry.mode}
                    </Badge>
                  </span>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => detach(entry.id)}
                    disabled={pending !== null}
                    aria-label={t("shares.detachAria", {
                      instance: entry.id,
                      share: share.name,
                    })}
                  >
                    {pending === entry.id ? (
                      <Loader2 aria-hidden className="animate-spin" />
                    ) : (
                      <Unlink aria-hidden />
                    )}
                    {t("shares.detach")}
                  </Button>
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="space-y-3 rounded-md border p-3">
          <p className="text-sm font-medium">{t("shares.attachInstance")}</p>
          {available.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t("shares.allAttached")}</p>
          ) : (
            <div className="flex flex-col gap-3 sm:flex-row sm:items-end">
              <div className="flex-1 space-y-1.5">
                <Label htmlFor="attach-instance">{t("shares.instance")}</Label>
                <Select value={instanceId} onValueChange={setInstanceId}>
                  <SelectTrigger id="attach-instance" className="w-full">
                    <SelectValue placeholder={t("shares.selectInstance")} />
                  </SelectTrigger>
                  <SelectContent>
                    {available.map((instance) => (
                      <SelectItem key={instance.id} value={instance.id}>
                        {instance.name} · {instance.id}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="attach-mode">{t("shares.mode")}</Label>
                <Select value={mode} onValueChange={(value) => setMode(value as "rw" | "ro")}>
                  <SelectTrigger id="attach-mode" className="w-24">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="rw">rw</SelectItem>
                    <SelectItem value="ro">ro</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <Button onClick={attach} disabled={!instanceId || pending !== null}>
                {pending === "attach" ? <Loader2 aria-hidden className="animate-spin" /> : <Plus aria-hidden />}
                {t("shares.attach")}
              </Button>
            </div>
          )}
        </div>

        {error ? <ErrorAlert error={error} /> : null}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("common.close")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function DeleteShareDialog({
  share,
  open,
  onOpenChange,
  onDeleted,
}: {
  share: Share | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDeleted: () => void;
}) {
  const { t } = useI18n();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setPending(false);
    setError(null);
  }, [open]);

  if (!share) return null;

  async function onDelete() {
    if (!share) return;
    setPending(true);
    setError(null);
    try {
      await api.deleteShare(share.name);
      toast.success(t("shares.deleted", { name: share.name }));
      onDeleted();
      onOpenChange(false);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {t("shares.deleteTitle")} <span className="font-mono">{share.name}</span>?
          </DialogTitle>
          <DialogDescription>
            <RichText text={t("shares.deleteDescription", { name: share.name })} />
          </DialogDescription>
        </DialogHeader>

        {share.instances.length > 0 ? (
          <Alert>
            <AlertCircle aria-hidden className="size-4" />
            <AlertTitle>{t("shares.stillAttached")}</AlertTitle>
            <AlertDescription>
              <p>
                {t(
                  share.instances.length === 1
                    ? "shares.stillAttachedBodyOne"
                    : "shares.stillAttachedBodyMany",
                  { count: share.instances.length },
                )}
              </p>
            </AlertDescription>
          </Alert>
        ) : null}

        {error ? <ErrorAlert error={error} /> : null}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
            {t("common.cancel")}
          </Button>
          <Button variant="destructive" onClick={onDelete} disabled={pending}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Trash2 aria-hidden />}
            {t("shares.delete")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function SharesPage() {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const [nonce, setNonce] = useState(0);
  const shares = useResource(() => api.listShares(), [nonce]);
  const instances = useResource(() => api.listInstances(), [nonce]);
  const [createOpen, setCreateOpen] = useState(false);
  const [manageName, setManageName] = useState<string | null>(null);
  const [deleteName, setDeleteName] = useState<string | null>(null);

  const rows = shares.data ?? [];
  const attachmentCount = rows.reduce((total, share) => total + share.instances.length, 0);
  const manageShare = manageName ? rows.find((share) => share.name === manageName) ?? null : null;
  const deleteShare = deleteName ? rows.find((share) => share.name === deleteName) ?? null : null;

  return (
    <>
      <PageHeader
        title={t("nav.shares")}
        description={
          shares.data
            ? t("shares.summary", { shares: rows.length, attachments: attachmentCount })
            : t("shares.loading")
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={shares.refreshing ? "animate-spin" : undefined} />
              {t("common.refresh")}
            </Button>
            {canEdit ? (
              <Button size="sm" onClick={() => setCreateOpen(true)}>
                <Plus aria-hidden />
                {t("shares.newTitle")}
              </Button>
            ) : null}
          </>
        }
      />

      {shares.error ? <ErrorAlert error={shares.error} onRetry={shares.reload} /> : null}

      <div className="rounded-md border">
        {shares.loading && !shares.data ? (
          <div className="space-y-3 p-4">
            {Array.from({ length: 3 }).map((_, index) => (
              <Skeleton key={index} className="h-10 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={FolderTree}
            title={t("shares.emptyTitle")}
            description={canEdit ? t("shares.emptyCreate") : t("shares.emptyReadOnly")}
            action={
              canEdit ? (
                <Button onClick={() => setCreateOpen(true)}>
                  <Plus aria-hidden />
                  {t("shares.newTitle")}
                </Button>
              ) : undefined
            }
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("shares.share")}</TableHead>
                    <TableHead>{t("shares.path")}</TableHead>
                    <TableHead>{t("shares.attachedTo")}</TableHead>
                    {canEdit ? (
                      <TableHead className="w-[180px]">
                        <span className="sr-only">{t("common.actions")}</span>
                      </TableHead>
                    ) : null}
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((share) => (
                    <TableRow key={share.name}>
                      <TableCell className="font-mono text-sm">{share.name}</TableCell>
                      <TableCell className="font-mono text-xs text-muted-foreground">{share.path}</TableCell>
                      <TableCell>
                        <Attachments share={share} />
                      </TableCell>
                      {canEdit ? (
                        <TableCell className="text-right">
                          <div className="flex items-center justify-end gap-1">
                            <Button variant="outline" size="sm" onClick={() => setManageName(share.name)}>
                              {t("providers.list.manage")}
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label={t("shares.deleteAria", { name: share.name })}
                              title={t("shares.deleteAria", { name: share.name })}
                              onClick={() => setDeleteName(share.name)}
                            >
                              <Trash2 aria-hidden />
                            </Button>
                          </div>
                        </TableCell>
                      ) : null}
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((share) => (
                <div key={share.name} className="space-y-3 p-4">
                  <div className="space-y-0.5">
                    <p className="font-mono text-sm">{share.name}</p>
                    <p className="font-mono text-xs break-all text-muted-foreground">{share.path}</p>
                  </div>
                  <Attachments share={share} />
                  {canEdit ? (
                    <div className="flex items-center justify-between gap-2">
                      <Button variant="outline" size="sm" onClick={() => setManageName(share.name)}>
                        {t("providers.list.manage")}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setDeleteName(share.name)}
                        aria-label={t("shares.deleteAria", { name: share.name })}
                      >
                        <Trash2 aria-hidden />
                        {t("common.delete")}
                      </Button>
                    </div>
                  ) : null}
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      <CreateShareDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        onCreated={() => setNonce((value) => value + 1)}
      />

      <ManageShareDialog
        share={manageShare}
        instances={instances.data ?? []}
        open={manageShare !== null}
        onOpenChange={(open) => {
          if (!open) setManageName(null);
        }}
        onChanged={() => setNonce((value) => value + 1)}
      />

      <DeleteShareDialog
        share={deleteShare}
        open={deleteShare !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteName(null);
        }}
        onDeleted={() => setNonce((value) => value + 1)}
      />
    </>
  );
}
