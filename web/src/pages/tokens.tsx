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
import { formatDateTime, formatRelative, plural } from "@/lib/format";
import { useResource } from "@/lib/use-resource";

function RoleBadge({ role }: { role: Role }) {
  return (
    <Badge variant="outline" className="font-mono text-xs font-normal">
      {role}
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
      setFieldError("Enter a name.");
      return;
    }
    if (value.length > 64) {
      setFieldError("64 characters maximum.");
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
              <DialogTitle>Token created</DialogTitle>
              <DialogDescription>
                <span className="font-mono">{created.name}</span> · {created.role}
              </DialogDescription>
            </DialogHeader>

            <Alert>
              <TriangleAlert aria-hidden className="size-4" />
              <AlertTitle>Copy it now</AlertTitle>
              <AlertDescription>
                This token cannot be shown again: the host stores a SHA-256 digest, not the secret. If
                you lose it, revoke it and create a new one.
              </AlertDescription>
            </Alert>

            <div className="space-y-2">
              <Label htmlFor="created-token">Access token</Label>
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
                  label="Copy token"
                  showLabel
                  size="sm"
                  variant="outline"
                  className="shrink-0"
                />
              </div>
            </div>

            <DialogFooter>
              <Button type="button" onClick={() => onOpenChange(false)}>
                Done
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <form onSubmit={onSubmit} noValidate className="space-y-4">
            <DialogHeader>
              <DialogTitle>New access token</DialogTitle>
              <DialogDescription>
                For scripts and CI. The role decides what the token may do, and the secret is shown
                once.
              </DialogDescription>
            </DialogHeader>

            <div className="space-y-2">
              <Label htmlFor="token-name">Name</Label>
              <Input
                id="token-name"
                value={name}
                autoFocus
                autoComplete="off"
                spellCheck={false}
                className="font-mono"
                placeholder="ci-nightly"
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
                  Recorded as the actor of every request made with this token.
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="token-role">Role</Label>
              <Select value={role} onValueChange={(value) => setRole(value as Role)}>
                <SelectTrigger id="token-role" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="viewer">viewer · read-only</SelectItem>
                  <SelectItem value="admin">admin · full access</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                A viewer token can read the API only. An admin token can also mutate and manage
                tokens.
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
                Cancel
              </Button>
              <Button type="submit" disabled={pending}>
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Plus aria-hidden />}
                Create token
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
      toast.success(`Token ${token.name} revoked.`);
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
            Revoke token <span className="font-mono">{token.name}</span>?
          </DialogTitle>
          <DialogDescription>
            The token stops working on its next request. The row and its name stay in the list and in
            the audit trail.
          </DialogDescription>
        </DialogHeader>

        {error ? <ErrorAlert error={error} /> : null}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
            Cancel
          </Button>
          <Button variant="destructive" onClick={onRevoke} disabled={pending}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
            Revoke token
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function TokensPage() {
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
        title="Access tokens"
        description={
          tokens.data
            ? `${rows.length} ${plural(rows.length, "token")} · ${active} active · shown once, stored as a digest`
            : "Loading tokens."
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={tokens.refreshing ? "animate-spin" : undefined} />
              Refresh
            </Button>
            <Button size="sm" onClick={() => setCreateOpen(true)}>
              <Plus aria-hidden />
              New token
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
            title="No tokens yet"
            description="Create one for CI or a script. It is shown once, then only its digest stays on the host."
            action={
              <Button onClick={() => setCreateOpen(true)}>
                <Plus aria-hidden />
                New token
              </Button>
            }
          />
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Name</TableHead>
                    <TableHead className="w-[100px]">Role</TableHead>
                    <TableHead className="w-[190px]">Created</TableHead>
                    <TableHead className="w-[120px]">Last used</TableHead>
                    <TableHead className="w-[190px]">Revoked</TableHead>
                    <TableHead className="w-[100px]">
                      <span className="sr-only">Actions</span>
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
                          title={formatRelative(token.created_at)}
                        >
                          {formatDateTime(token.created_at)}
                        </span>
                      </TableCell>
                      <TableCell>
                        <span
                          className="text-xs text-muted-foreground"
                          title="Not recorded by the controller yet."
                        >
                          —
                        </span>
                      </TableCell>
                      <TableCell>
                        {token.revoked_at ? (
                          <span
                            className="font-mono text-xs"
                            title={formatRelative(token.revoked_at)}
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
                            revoked
                          </Badge>
                        ) : (
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => setRevokeId(token.id)}
                            aria-label={`Revoke token ${token.name}`}
                          >
                            Revoke
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
                      <dt className="text-muted-foreground">Created</dt>
                      <dd className="font-mono">{formatDateTime(token.created_at)}</dd>
                    </div>
                    <div>
                      <dt className="text-muted-foreground">Last used</dt>
                      <dd className="text-muted-foreground" title="Not recorded by the controller yet.">
                        —
                      </dd>
                    </div>
                    <div className="col-span-2">
                      <dt className="text-muted-foreground">Revoked</dt>
                      <dd className="font-mono">
                        {token.revoked_at ? formatDateTime(token.revoked_at) : "—"}
                      </dd>
                    </div>
                  </dl>
                  {token.revoked_at ? (
                    <Badge variant="outline" className="font-mono text-[11px] font-normal">
                      revoked
                    </Badge>
                  ) : (
                    <Button
                      variant="outline"
                      size="sm"
                      className="w-full"
                      onClick={() => setRevokeId(token.id)}
                      aria-label={`Revoke token ${token.name}`}
                    >
                      Revoke
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
          Last used is not tracked by the controller yet. Token creation and revocation appear in the
          audit log.
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
