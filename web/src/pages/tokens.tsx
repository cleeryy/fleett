import { useEffect, useState, type FormEvent } from "react";
import { KeyRound, Loader2, Plus, RefreshCw, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { CreatedToken, Role, TokenInfo } from "@/api/types";
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
import { CopyButton } from "@/components/copy-button";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { formatDateTime, formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { useResource } from "@/lib/use-resource";

function RoleBadge({ role }: { role: Role }) {
  const { t } = useI18n();
  return (
    <Badge variant="outline" className="font-mono text-xs font-normal">
      {t(`role.${role}`)}
    </Badge>
  );
}

function CreateTokenDialog({
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
  const [role, setRole] = useState<Role>("viewer");
  const [pending, setPending] = useState(false);
  const [created, setCreated] = useState<CreatedToken | null>(null);
  const [fieldError, setFieldError] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setName("");
    setRole("viewer");
    setPending(false);
    setCreated(null);
    setFieldError(null);
    setError(null);
  }, [open]);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = name.trim();
    if (!value) {
      setFieldError(t("tokens.nameRequired"));
      return;
    }
    if (value.length > 64) {
      setFieldError(t("tokens.nameTooLong"));
      return;
    }
    setPending(true);
    setFieldError(null);
    setError(null);
    try {
      const result = await api.createToken({ name: value, role });
      setCreated(result);
      onCreated();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent>
        {created ? (
          <div className="space-y-4">
            <DialogHeader>
              <DialogTitle>{t("tokens.createdTitle")}</DialogTitle>
              <DialogDescription>
                <span className="font-mono">{created.name}</span> · {t(`role.${created.role}`)}
              </DialogDescription>
            </DialogHeader>

            <Alert>
              <TriangleAlert aria-hidden className="size-4" />
              <AlertTitle>{t("tokens.copyNow")}</AlertTitle>
              <AlertDescription>{t("tokens.copyNowBody")}</AlertDescription>
            </Alert>

            <div className="space-y-2">
              <Label htmlFor="created-token">{t("login.accessToken")}</Label>
              <div className="flex items-center gap-2">
                <Input
                  id="created-token"
                  readOnly
                  value={created.token}
                  spellCheck={false}
                  className="font-mono"
                  onFocus={(event) => event.currentTarget.select()}
                />
                <CopyButton
                  value={created.token}
                  label={t("tokens.copyToken")}
                  showLabel
                  size="sm"
                  variant="outline"
                  className="shrink-0"
                />
              </div>
            </div>

            <DialogFooter>
              <Button type="button" onClick={() => onOpenChange(false)}>
                {t("common.done")}
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <form onSubmit={onSubmit} noValidate className="space-y-4">
            <DialogHeader>
              <DialogTitle>{t("tokens.newTitle")}</DialogTitle>
              <DialogDescription>{t("tokens.newDescription")}</DialogDescription>
            </DialogHeader>

            <div className="space-y-2">
              <Label htmlFor="token-name">{t("common.name")}</Label>
              <Input
                id="token-name"
                value={name}
                autoFocus
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
                placeholder={t("tokens.namePlaceholder")}
                aria-invalid={!!fieldError}
                aria-describedby={fieldError ? "token-name-error" : "token-name-help"}
                onChange={(event) => setName(event.target.value)}
              />
              {fieldError ? (
                <p id="token-name-error" className="text-xs text-destructive">
                  {fieldError}
                </p>
              ) : (
                <p id="token-name-help" className="text-xs text-muted-foreground">
                  {t("tokens.nameHint")}
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="token-role">{t("audit.role")}</Label>
              <Select value={role} onValueChange={(value) => setRole(value as Role)}>
                <SelectTrigger id="token-role" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="viewer">{t("tokens.roleViewer")}</SelectItem>
                  <SelectItem value="admin">{t("tokens.roleAdmin")}</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                {t("tokens.roleHint")}
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
              <Button type="submit" disabled={pending}>
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Plus aria-hidden />}
                {t("tokens.create")}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}

function RevokeTokenDialog({
  token,
  open,
  onOpenChange,
  onRevoked,
}: {
  token: TokenInfo | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onRevoked: () => void;
}) {
  const { t } = useI18n();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setPending(false);
    setError(null);
  }, [open]);

  if (!token) return null;

  async function onRevoke() {
    if (!token) return;
    setPending(true);
    setError(null);
    try {
      await api.revokeToken(token.id);
      toast.success(t("tokens.revoked", { name: token.name }));
      onRevoked();
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
            {t("tokens.revokeTitle")} <span className="font-mono">{token.name}</span>?
          </DialogTitle>
          <DialogDescription>{t("tokens.revokeDescription")}</DialogDescription>
        </DialogHeader>

        {error ? <ErrorAlert error={error} /> : null}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
            {t("common.cancel")}
          </Button>
          <Button variant="destructive" onClick={onRevoke} disabled={pending}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
            {t("tokens.revoke")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function TokensPage() {
  const { t, lang } = useI18n();
  const [nonce, setNonce] = useState(0);
  const tokens = useResource(() => api.listTokens(), [nonce]);
  const [createOpen, setCreateOpen] = useState(false);
  const [revokeId, setRevokeId] = useState<string | null>(null);

  const rows = tokens.data ?? [];
  const active = rows.filter((token) => !token.revoked_at).length;
  const revokeTarget = revokeId ? (rows.find((token) => token.id === revokeId) ?? null) : null;

  return (
    <>
      <PageHeader
        title={t("tokens.title")}
        description={
          tokens.data
            ? t("tokens.summary", { count: rows.length, active })
            : t("tokens.loading")
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={tokens.refreshing ? "animate-spin" : undefined} />
              {t("common.refresh")}
            </Button>
            <Button size="sm" onClick={() => setCreateOpen(true)}>
              <Plus aria-hidden />
              {t("tokens.new")}
            </Button>
          </>
        }
      />

      {tokens.error ? <ErrorAlert error={tokens.error} onRetry={tokens.reload} /> : null}

      <div className="rounded-md border">
        {tokens.loading && !tokens.data ? (
          <div className="space-y-3 p-4">
            {Array.from({ length: 3 }).map((_, index) => (
              <Skeleton key={index} className="h-10 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={KeyRound}
            title={t("tokens.emptyTitle")}
            description={t("tokens.emptyDescription")}
            action={
              <Button onClick={() => setCreateOpen(true)}>
                <Plus aria-hidden />
                {t("tokens.new")}
              </Button>
            }
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("common.name")}</TableHead>
                    <TableHead className="w-[100px]">{t("audit.role")}</TableHead>
                    <TableHead className="w-[190px]">{t("common.created")}</TableHead>
                    <TableHead className="w-[120px]">{t("tokens.lastUsed")}</TableHead>
                    <TableHead className="w-[190px]">{t("tokens.revokedHeader")}</TableHead>
                    <TableHead className="w-[100px]">
                      <span className="sr-only">{t("common.actions")}</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((token) => (
                    <TableRow key={token.id}>
                      <TableCell>
                        <span
                          className={
                            token.revoked_at
                              ? "font-mono text-sm text-muted-foreground line-through"
                              : "font-mono text-sm"
                          }
                        >
                          {token.name}
                        </span>
                        <span className="block font-mono text-[11px] text-muted-foreground">
                          {token.id}
                        </span>
                      </TableCell>
                      <TableCell>
                        <RoleBadge role={token.role} />
                      </TableCell>
                      <TableCell>
                        <span
                          className="font-mono text-xs"
                          title={formatRelative(token.created_at, lang)}
                        >
                          {formatDateTime(token.created_at)}
                        </span>
                      </TableCell>
                      <TableCell>
                        <span
                          className="text-xs text-muted-foreground"
                          title={t("tokens.lastUsedHint")}
                        >
                          —
                        </span>
                      </TableCell>
                      <TableCell>
                        {token.revoked_at ? (
                          <span
                            className="font-mono text-xs"
                            title={formatRelative(token.revoked_at, lang)}
                          >
                            {formatDateTime(token.revoked_at)}
                          </span>
                        ) : (
                          <span className="text-xs text-muted-foreground">—</span>
                        )}
                      </TableCell>
                      <TableCell className="text-right">
                        {token.revoked_at ? (
                          <Badge variant="outline" className="font-mono text-[11px] font-normal">
                            {t("status.revoked")}
                          </Badge>
                        ) : (
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => setRevokeId(token.id)}
                            aria-label={t("tokens.revokeAria", { name: token.name })}
                          >
                            {t("tokens.revoke")}
                          </Button>
                        )}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((token) => (
                <div key={token.id} className="space-y-3 p-4">
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <p
                        className={
                          token.revoked_at
                            ? "truncate font-mono text-sm text-muted-foreground line-through"
                            : "truncate font-mono text-sm"
                        }
                      >
                        {token.name}
                      </p>
                      <p className="truncate font-mono text-[11px] text-muted-foreground">{token.id}</p>
                    </div>
                    <RoleBadge role={token.role} />
                  </div>
                  <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-xs">
                    <div>
                      <dt className="text-muted-foreground">{t("common.created")}</dt>
                      <dd className="font-mono">{formatDateTime(token.created_at)}</dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">{t("tokens.lastUsed")}</dt>
                      <dd className="text-muted-foreground" title={t("tokens.lastUsedHint")}>
                        —
                      </dd>
                    </div>
                    <div className="col-span-2">
                      <dt className="text-muted-foreground">{t("tokens.revokedHeader")}</dt>
                      <dd className="font-mono">
                        {token.revoked_at ? formatDateTime(token.revoked_at) : "—"}
                      </dd>
                    </div>
                  </dl>
                  {token.revoked_at ? (
                    <Badge variant="outline" className="font-mono text-[11px] font-normal">
                      {t("status.revoked")}
                    </Badge>
                  ) : (
                    <Button
                      variant="outline"
                      size="sm"
                      className="w-full"
                      onClick={() => setRevokeId(token.id)}
                      aria-label={t("tokens.revokeAria", { name: token.name })}
                    >
                      {t("tokens.revoke")}
                    </Button>
                  )}
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      {rows.length > 0 ? (
        <p className="text-xs text-muted-foreground">
          {t("tokens.footer")}
        </p>
      ) : null}

      <CreateTokenDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        onCreated={() => setNonce((value) => value + 1)}
      />

      <RevokeTokenDialog
        token={revokeTarget}
        open={revokeTarget !== null}
        onOpenChange={(open) => {
          if (!open) setRevokeId(null);
        }}
        onRevoked={() => setNonce((value) => value + 1)}
      />
    </>
  );
}
