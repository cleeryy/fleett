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
import { RichText } from "@/components/rich-text";
import { hasTranslation, useI18n } from "@/lib/i18n";
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
  const { t } = useI18n();
  const status = useResource(() => api.securityStatus(), []);
  const authMode = (mode: string) => {
    const key = `authMode.${mode}`;
    return hasTranslation(key) ? t(key) : mode;
  };
  const data: SecurityStatus | null = status.data;

  return (
    <>
      <PageHeader
        title={t("nav.security")}
        description={
          data
            ? t("security.summary", { mode: authMode(data.auth_mode) })
            : t("security.loading")
        }
        actions={
          <Button variant="outline" size="sm" onClick={status.reload}>
            <RefreshCw aria-hidden className={status.refreshing ? "animate-spin" : undefined} />
            {t("common.refresh")}
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
          title={t("security.unavailableTitle")}
          description={t("security.unavailableDescription")}
        />
      ) : (
        <>
          <div className="grid gap-4 lg:grid-cols-2">
            <Card title={t("security.rateLimits")}>
              <div className="flex items-center gap-2">
                <OpsBadge
                  status={data.rate_limit.enabled ? "ok" : "closed"}
                  label={
                    data.rate_limit.enabled ? t("common.enabled") : t("common.disabled")
                  }
                />
                <span className="text-xs text-muted-foreground">
                  {t("security.perClient")}
                </span>
              </div>
              <dl>
                <Row label={t("security.apiBucket")} value={<span className="font-mono">{data.rate_limit.limits.per_min} / min</span>} />
                <Row
                  label={t("security.authBucket")}
                  value={<span className="font-mono">{data.rate_limit.limits.auth_per_min} / min</span>}
                />
                <Row
                  label={t("security.heavyBucket")}
                  value={<span className="font-mono">{data.rate_limit.limits.heavy_per_min} / min</span>}
                />
              </dl>
              <p className="text-xs text-muted-foreground">
                <RichText text={t("security.rateLimitHint")} />
              </p>
            </Card>

            <Card title={t("security.lockout")}>
              <div className="flex flex-wrap items-center gap-2">
                <OpsBadge
                  status={data.lockout.failures === 0 ? "closed" : "ok"}
                  label={
                    data.lockout.failures === 0 ? t("common.disabled") : t("common.enabled")
                  }
                />
                <span className="text-xs text-muted-foreground">
                  {t(
                    data.lockout.locked_ips === 1
                      ? "security.lockedOne"
                      : "security.lockedMany",
                    { count: data.lockout.locked_ips },
                  )}
                </span>
              </div>
              <dl>
                <Row
                  label={t("security.failuresTolerated")}
                  value={<span className="font-mono">{data.lockout.failures}</span>}
                />
                <Row
                  label={t("security.slidingWindow")}
                  value={<span className="font-mono">{data.lockout.window_s} s</span>}
                />
                <Row
                  label={t("security.lockoutDuration")}
                  value={<span className="font-mono">{data.lockout.lockout_s} s</span>}
                />
              </dl>
              <p className="text-xs text-muted-foreground">
                <RichText text={t("security.lockoutHint")} />
              </p>
            </Card>

            <Card title={t("security.headers")}>
              <div className="flex items-center gap-2">
                <Badge variant="outline" className="font-mono text-xs font-normal">
                  {data.headers.x_content_type_options}
                </Badge>
                <Badge variant="outline" className="font-mono text-xs font-normal">
                  {data.headers.x_frame_options}
                </Badge>
                <OpsBadge
                  status={data.headers.hsts ? "ok" : "closed"}
                  label={data.headers.hsts ? t("security.hsts") : t("security.noHsts")}
                />
              </div>
              <div>
                <HeaderValue label="Content-Security-Policy" value={data.headers.csp} />
                <HeaderValue label="Referrer-Policy" value={data.headers.referrer_policy} />
                <HeaderValue label="Permissions-Policy" value={data.headers.permissions_policy} />
              </div>
              <p className="text-xs text-muted-foreground">
                <RichText text={t("security.headersHint")} />
              </p>
            </Card>

            <Card title={t("security.tokens")}>
              <div className="flex items-center gap-2">
                <KeyRound aria-hidden className="size-4 text-muted-foreground" />
                <span className="text-xs text-muted-foreground">
                  {t("security.tokensDigest")}
                </span>
              </div>
              <dl>
                <Row label={t("security.activeTokens")} value={<span className="font-mono">{data.tokens.active}</span>} />
                <Row
                  label={t("security.expiredTokens")}
                  value={<span className="font-mono">{data.tokens.expired_or_revoked}</span>}
                />
                <Row
                  label={t("security.authMode")}
                  value={<span className="font-mono">{authMode(data.auth_mode)}</span>}
                />
              </dl>
              <p className="text-xs text-muted-foreground">{t("security.tokensHint")}</p>
            </Card>
          </div>

          <p className="text-xs text-muted-foreground">
            <RichText text={t("security.footer")} />
          </p>
        </>
      )}
    </>
  );
}
