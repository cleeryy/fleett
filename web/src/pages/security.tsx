import type { ReactNode } from "react";
import { KeyRound, RefreshCw, ShieldCheck } from "lucide-react";
import { api } from "@/api";
import type { SecurityStatus } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { OpsBadge } from "@/components/ops-badge";
import { PageHeader } from "@/components/page-header";
import { useResource } from "@/lib/use-resource";

function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="space-y-3 rounded-md border p-4">
      <h2 className="text-sm font-medium">{title}</h2>
      {children}
    </section>
  );
}

function Row({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="flex flex-wrap items-baseline justify-between gap-2 border-b py-1.5 last:border-b-0">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="min-w-0 text-right text-sm">{value}</dd>
    </div>
  );
}

function HeaderValue({ label, value }: { label: string; value: string }) {
  return (
    <div className="space-y-1 border-b py-1.5 last:border-b-0">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="font-mono text-[11px] break-words">{value || "—"}</p>
    </div>
  );
}

export function SecurityPage() {
  const status = useResource(() => api.securityStatus(), []);
  const data: SecurityStatus | null = status.data;

  return (
    <>
      <PageHeader
        title="Security"
        description={
          data
            ? `Read-only posture · auth mode ${data.auth_mode} · admins only`
            : "Reading the security posture."
        }
        actions={
          <Button variant="outline" size="sm" onClick={status.reload}>
            <RefreshCw aria-hidden className={status.refreshing ? "animate-spin" : undefined} />
            Refresh
          </Button>
        }
      />

      {status.error ? <ErrorAlert error={status.error} onRetry={status.reload} /> : null}

      {status.loading && !data ? (
        <div className="grid gap-4 lg:grid-cols-2">
          <Skeleton className="h-56 w-full" />
          <Skeleton className="h-56 w-full" />
          <Skeleton className="h-56 w-full" />
          <Skeleton className="h-56 w-full" />
        </div>
      ) : !data ? (
        <EmptyState
          icon={ShieldCheck}
          title="Security posture unavailable"
          description="The controller did not answer /api/security/status."
        />
      ) : (
        <>
          <div className="grid gap-4 lg:grid-cols-2">
            <Card title="Rate limits">
              <div className="flex items-center gap-2">
                <OpsBadge
                  status={data.rate_limit.enabled ? "ok" : "closed"}
                  label={data.rate_limit.enabled ? "enabled" : "disabled"}
                />
                <span className="text-xs text-muted-foreground">
                  per client IP, credential and route class
                </span>
              </div>
              <dl>
                <Row label="API bucket" value={<span className="font-mono">{data.rate_limit.limits.per_min} / min</span>} />
                <Row
                  label="Auth bucket"
                  value={<span className="font-mono">{data.rate_limit.limits.auth_per_min} / min</span>}
                />
                <Row
                  label="Heavy bucket"
                  value={<span className="font-mono">{data.rate_limit.limits.heavy_per_min} / min</span>}
                />
              </dl>
              <p className="text-xs text-muted-foreground">
                A <span className="font-mono">0</span> limit disables that bucket. Logs, exec, files
                and exports use the heavy bucket.
              </p>
            </Card>

            <Card title="Authentication lockout">
              <div className="flex flex-wrap items-center gap-2">
                <OpsBadge
                  status={data.lockout.failures === 0 ? "closed" : "ok"}
                  label={data.lockout.failures === 0 ? "disabled" : "enabled"}
                />
                <span className="text-xs text-muted-foreground">
                  {data.lockout.locked_ips} distinct address
                  {data.lockout.locked_ips === 1 ? "" : "es"} currently locked
                </span>
              </div>
              <dl>
                <Row
                  label="Failures tolerated"
                  value={<span className="font-mono">{data.lockout.failures}</span>}
                />
                <Row
                  label="Sliding window"
                  value={<span className="font-mono">{data.lockout.window_s} s</span>}
                />
                <Row
                  label="Lockout duration"
                  value={<span className="font-mono">{data.lockout.lockout_s} s</span>}
                />
              </dl>
              <p className="text-xs text-muted-foreground">
                Failed authentications feed a progressive lockout; the lock itself is audited as{" "}
                <span className="font-mono">auth.lockout</span>.
              </p>
            </Card>

            <Card title="Response headers">
              <div className="flex items-center gap-2">
                <Badge variant="outline" className="font-mono text-xs font-normal">
                  {data.headers.x_content_type_options}
                </Badge>
                <Badge variant="outline" className="font-mono text-xs font-normal">
                  {data.headers.x_frame_options}
                </Badge>
                <OpsBadge
                  status={data.headers.hsts ? "ok" : "closed"}
                  label={data.headers.hsts ? "HSTS" : "no HSTS"}
                />
              </div>
              <div>
                <HeaderValue label="Content-Security-Policy" value={data.headers.csp} />
                <HeaderValue label="Referrer-Policy" value={data.headers.referrer_policy} />
                <HeaderValue label="Permissions-Policy" value={data.headers.permissions_policy} />
              </div>
              <p className="text-xs text-muted-foreground">
                HSTS is sent only when <span className="font-mono">FLEETT_PUBLIC_URL</span> is https.
                Every console, API and error response carries these headers.
              </p>
            </Card>

            <Card title="Tokens and credentials">
              <div className="flex items-center gap-2">
                <KeyRound aria-hidden className="size-4 text-muted-foreground" />
                <span className="text-xs text-muted-foreground">
                  Tokens are stored as SHA-256 digests only
                </span>
              </div>
              <dl>
                <Row label="Active tokens" value={<span className="font-mono">{data.tokens.active}</span>} />
                <Row
                  label="Expired or revoked"
                  value={<span className="font-mono">{data.tokens.expired_or_revoked}</span>}
                />
                <Row label="Auth mode" value={<span className="font-mono">{data.auth_mode}</span>} />
              </dl>
              <p className="text-xs text-muted-foreground">
                Manage credentials on the Tokens page. A revoked token stays addressable until it is
                purged, but never authenticates again.
              </p>
            </Card>
          </div>

          <p className="text-xs text-muted-foreground">
            This page is read-only. The limits and lockout windows come from the controller
            environment (<span className="font-mono">FLEETT_RATE_LIMIT_*</span>,{" "}
            <span className="font-mono">FLEETT_AUTH_LOCKOUT_*</span>).
          </p>
        </>
      )}
    </>
  );
}
