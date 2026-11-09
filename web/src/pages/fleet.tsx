import { useEffect, useMemo, useRef, useState } from "react";
import {
  Boxes,
  ExternalLink,
  Fingerprint,
  Loader2,
  Minus,
  MoreHorizontal,
  MessageSquare,
  Network,
  Play,
  Plus,
  RefreshCw,
  RotateCw,
  Search,
  Send,
  Square,
  Trash2,
  TriangleAlert,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { InstanceDetail, InstanceSummary } from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Skeleton } from "@/components/ui/skeleton";
import { DeleteInstanceDialog } from "@/components/delete-instance-dialog";
import { DoctorDialog } from "@/components/doctor";
import { EgressBadge } from "@/components/egress-badge";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import {
  FleetOnboardingCard,
  OnboardingCompleteCard,
} from "@/components/onboarding";
import { PageHeader } from "@/components/page-header";
import { StatusBadge } from "@/components/status-badge";
import { INSTANCES_CHANGED_EVENT } from "@/lib/events";
import { formatClock, formatUptime } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import {
  readOnboardingMemory,
  writeOnboardingMemory,
  type OnboardingMemory,
} from "@/lib/onboarding";
import { hrefFor, navigate } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

const FILTERS = ["all", "running", "stopped", "created", "error", "unknown"] as const;
type Filter = (typeof FILTERS)[number];

/** Kept across navigation so returning from a detail page preserves the view. */
const persisted = { q: "", status: "all" as Filter };

function ChannelIcon({ channel }: { channel: string }) {
  const Icon = channel === "telegram" ? Send : channel === "discord" ? MessageSquare : Minus;
  return <Icon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />;
}

/** Quiet marker for the exception: this agent can reach the LAN. */
function LanBadge() {
  const { t } = useI18n();
  return (
    <Badge
      variant="secondary"
      className="gap-1 px-1.5 font-mono text-[11px] font-normal"
      aria-label={t("fleet.lanBadge")}
      title={t("fleet.lanBadgeTitle")}
    >
      <Network aria-hidden />
      LAN
    </Badge>
  );
}

/** Persona name plus drift, shown next to the instance name. */
function PersonaBadge({ row, name }: { row: InstanceSummary; name?: string }) {
  const { t } = useI18n();
  if (!row.persona) return null;
  const diverged = row.persona_state === "diverged";
  return (
    <Badge
      variant="outline"
      className={cn(
        "max-w-40 gap-1 px-1.5 font-mono text-[11px] font-normal",
        diverged && "border-amber-600/40 text-amber-700 dark:text-amber-400",
      )}
      title={
        diverged
          ? t("fleet.personaDiverged", { name: name ?? row.persona })
          : t("fleet.personaTitle", { name: name ?? row.persona })
      }
    >
      <Fingerprint aria-hidden />
      <span className="truncate">{name ?? row.persona}</span>
      {diverged ? <span className="shrink-0">· {t("status.diverged")}</span> : null}
    </Badge>
  );
}

function RowMenu({
  row,
  pending,
  onAction,
  onDelete,
}: {
  row: InstanceSummary;
  pending: boolean;
  onAction: (action: "start" | "stop" | "restart") => void;
  onDelete: () => void;
}) {
  const { t } = useI18n();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={t("fleet.rowActions", { name: row.name })}
          title={t("fleet.rowActions", { name: row.name })}
          disabled={pending}
        >
          {pending ? <Loader2 aria-hidden className="animate-spin" /> : <MoreHorizontal aria-hidden />}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-44">
        <DropdownMenuItem disabled={row.status === "running"} onSelect={() => onAction("start")}>
          <Play aria-hidden />
          {t("action.start")}
        </DropdownMenuItem>
        <DropdownMenuItem disabled={row.status !== "running"} onSelect={() => onAction("stop")}>
          <Square aria-hidden />
          {t("action.stop")}
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => onAction("restart")}>
          <RotateCw aria-hidden />
          {t("action.restart")}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem asChild>
          <a href={hrefFor(`/instances/${row.id}`)}>
            <ExternalLink aria-hidden />
            {t("common.open")}
          </a>
        </DropdownMenuItem>
        <DropdownMenuItem variant="destructive" onSelect={onDelete}>
          <Trash2 aria-hidden />
          {t("common.deleteEllipsis")}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

export function FleetPage() {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const [q, setQ] = useState(persisted.q);
  const [status, setStatus] = useState<Filter>(persisted.status);
  const [nonce, setNonce] = useState(0);
  const [details, setDetails] = useState<Record<string, InstanceDetail>>({});
  const [refreshedAt, setRefreshedAt] = useState<Date | null>(null);
  const [pendingId, setPendingId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<InstanceSummary | null>(null);
  const [doctorOpen, setDoctorOpen] = useState(false);
  const [onboardingMemory, setOnboardingMemory] = useState<OnboardingMemory>(readOnboardingMemory);
  const searchRef = useRef<HTMLInputElement>(null);

  const list = useResource(() => api.listInstances(), [nonce]);
  const onboarding = useResource(() => api.onboarding(), [nonce]);
  // One fleet-wide call for the egress badge of every row (issue #60): never
  // one `getEgress` per instance.
  const egress = useResource(() => api.egressSummary(), [nonce]);
  const egressByInstance = useMemo(
    () => new Map((egress.data?.instances ?? []).map((entry) => [entry.instance, entry])),
    [egress.data],
  );
  const personas = useResource(() => api.listPersonas(), []);
  const personaNames = useMemo(
    () => new Map((personas.data ?? []).map((persona) => [persona.id, persona.name])),
    [personas.data],
  );
  const openIncidents = useResource(
    () => api.listIncidents({ status: "open", limit: 100 }),
    [nonce],
  );

  useEffect(() => {
    persisted.q = q;
    persisted.status = status;
  }, [q, status]);

  const setupPending = onboarding.data !== null && !onboarding.data.complete;
  const setupComplete = onboarding.data?.complete === true;
  const setupDone = onboarding.data?.steps.filter((step) => step.done).length ?? 0;
  const setupTotal = onboarding.data?.steps.length ?? 0;

  // Remember this browser saw an unfinished host, so the one-time success
  // banner is only shown to the operator who actually went through the tour.
  useEffect(() => {
    if (!setupPending || onboardingMemory.seen) return;
    setOnboardingMemory(writeOnboardingMemory({ seen: true }));
  }, [setupPending, onboardingMemory.seen]);

  const firstInstance = useMemo(() => {
    const sorted = [...(list.data ?? [])];
    return sorted.sort((left, right) => left.created_at.localeCompare(right.created_at))[0] ?? null;
  }, [list.data]);

  const showOnboardingCard = setupPending && !onboardingMemory.dismissed;
  const showSetupSuccess =
    setupComplete &&
    onboardingMemory.seen &&
    firstInstance !== null &&
    onboardingMemory.celebrated !== firstInstance.id;
  const setupDismissed = setupPending && onboardingMemory.dismissed;

  function dismissOnboarding() {
    setOnboardingMemory(writeOnboardingMemory({ dismissed: true }));
  }

  function showOnboardingAgain() {
    setOnboardingMemory(writeOnboardingMemory({ dismissed: false }));
  }

  function dismissSetupSuccess() {
    if (!firstInstance) return;
    setOnboardingMemory(writeOnboardingMemory({ celebrated: firstInstance.id }));
  }

  // The command palette can start, stop or restart an instance from any screen.
  useEffect(() => {
    function onChanged() {
      setNonce((value) => value + 1);
    }
    window.addEventListener(INSTANCES_CHANGED_EVENT, onChanged);
    return () => window.removeEventListener(INSTANCES_CHANGED_EVENT, onChanged);
  }, []);

  // The list endpoint has no uptime or share modes; hydrate each row from the
  // detail endpoint. Failures are non-fatal: the row keeps its summary values.
  useEffect(() => {
    if (!list.data) return;
    let alive = true;
    Promise.all(list.data.map((row) => api.getInstance(row.id).catch(() => null))).then((results) => {
      if (!alive) return;
      const next: Record<string, InstanceDetail> = {};
      for (const detail of results) if (detail) next[detail.id] = detail;
      setDetails(next);
      setRefreshedAt(new Date());
    });
    return () => {
      alive = false;
    };
  }, [list.data]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, select, [contenteditable=true], [role=menu], [role=listbox], [role=dialog]")) {
        return;
      }
      if (event.key === "/") {
        event.preventDefault();
        searchRef.current?.focus();
        return;
      }
      if (event.key === "c" && canEdit) {
        event.preventDefault();
        navigate("/instances/new");
        return;
      }
      if (["j", "k", "ArrowDown", "ArrowUp"].includes(event.key)) {
        const links = Array.from(document.querySelectorAll<HTMLAnchorElement>("[data-row-link]")).filter(
          (link) => link.getClientRects().length > 0,
        );
        if (links.length === 0) return;
        event.preventDefault();
        const index = links.indexOf(document.activeElement as HTMLAnchorElement);
        const forward = event.key === "j" || event.key === "ArrowDown";
        const next = index === -1 ? 0 : forward ? Math.min(index + 1, links.length - 1) : Math.max(index - 1, 0);
        links[next]?.focus();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [canEdit]);

  const counts = useMemo(() => {
    const result: Record<string, number> = { all: list.data?.length ?? 0 };
    for (const row of list.data ?? []) {
      result[row.status] = (result[row.status] ?? 0) + 1;
    }
    return result;
  }, [list.data]);

  const rows = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return [...(list.data ?? [])]
      .sort((a, b) => a.name.localeCompare(b.name))
      .filter((row) => {
        if (status !== "all" && row.status !== status) return false;
        if (!needle) return true;
        return [row.name, row.id, row.model, row.channel, row.template, ...row.shares].some((value) =>
          value.toLowerCase().includes(needle),
        );
      });
  }, [list.data, q, status]);

  const errorCount = counts.error ?? 0;
  const hasAny = (list.data?.length ?? 0) > 0;

  async function runAction(id: string, action: "start" | "stop" | "restart") {
    setPendingId(id);
    setActionError(null);
    try {
      await api.action(id, action);
      toast.success(
        t("palette.actionDone", {
          id,
          state: t(
            action === "stop"
              ? "status.stopped"
              : action === "start"
                ? "status.started"
                : "status.restarted",
          ),
        }),
      );
      setNonce((value) => value + 1);
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setPendingId(null);
    }
  }

  const emptyState = !hasAny ? (
    <EmptyState
      icon={Boxes}
      title={t("fleet.emptyTitle")}
      description={
        setupPending
          ? t("fleet.emptySetup", { done: setupDone, total: setupTotal })
          : canEdit
            ? t("fleet.emptyCreate")
            : t("fleet.emptyReadOnly")
      }
      action={
        canEdit || setupDismissed ? (
          <div className="flex flex-wrap items-center justify-center gap-2">
            {canEdit ? (
              <Button asChild>
                <a href={hrefFor("/instances/new")}>
                  <Plus aria-hidden />
                  {t("nav.newInstance")}
                </a>
              </Button>
            ) : null}
            {setupDismissed ? (
              <Button variant="outline" onClick={showOnboardingAgain}>
                {t("fleet.showGettingStarted")}
              </Button>
            ) : null}
          </div>
        ) : undefined
      }
    />
  ) : (
    <EmptyState
      icon={Search}
      title={t("fleet.noMatchTitle")}
      description={t("fleet.noMatchDescription")}
      action={
        <Button
          variant="outline"
          onClick={() => {
            setQ("");
            setStatus("all");
          }}
        >
          {t("common.clearFilters")}
        </Button>
      }
    />
  );

  return (
    <>
      <PageHeader
        title={t("nav.instances")}
        description={
          list.data
            ? `${t(
                list.data.length === 1 ? "fleet.countOne" : "fleet.countMany",
                { count: list.data.length },
              )}${errorCount > 0 ? t("fleet.inErrorSuffix", { count: errorCount }) : ""}`
            : t("common.loadingInstances")
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
              <RefreshCw aria-hidden className={list.refreshing ? "animate-spin" : undefined} />
              {t("common.refresh")}
            </Button>
            {canEdit ? (
              <Button asChild size="sm">
                <a href={hrefFor("/instances/new")}>
                  <Plus aria-hidden />
                  {t("nav.newInstance")}
                </a>
              </Button>
            ) : null}
          </>
        }
      />

      {showOnboardingCard && onboarding.data ? (
        <FleetOnboardingCard
          report={onboarding.data}
          onDismiss={dismissOnboarding}
          onOpenDoctor={() => setDoctorOpen(true)}
        />
      ) : null}

      {showSetupSuccess ? (
        <OnboardingCompleteCard
          instances={list.data ?? []}
          onDismiss={dismissSetupSuccess}
          onOpenDiagnostics={() => setDoctorOpen(true)}
        />
      ) : null}

      {actionError ? <ErrorAlert error={actionError} /> : null}
      {list.error ? <ErrorAlert error={list.error} onRetry={list.reload} /> : null}

      {(openIncidents.data?.length ?? 0) > 0 ? (
        <Alert className="border-destructive/40 bg-destructive/5">
          <TriangleAlert aria-hidden className="text-destructive" />
          <AlertTitle>
            {t(
              openIncidents.data?.length === 1 ? "fleet.openIncidentOne" : "fleet.openIncidentMany",
              { count: openIncidents.data?.length ?? 0 },
            )}
          </AlertTitle>
          <AlertDescription>
            <p>
              {t("fleet.affected")}{" "}
              <span className="font-mono">
                {[...new Set((openIncidents.data ?? []).map((incident) => incident.instance_id))].join(", ")}
              </span>
              .
            </p>
            <a href="#/incidents" className="mt-1 text-sm font-medium underline-offset-4 hover:underline">
              {t("fleet.openIncidentList")}
            </a>
          </AlertDescription>
        </Alert>
      ) : null}

      <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
        <div className="relative w-full sm:max-w-xs">
          <Search aria-hidden className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            ref={searchRef}
            value={q}
            onChange={(event) => setQ(event.target.value)}
            placeholder={t("fleet.searchPlaceholder")}
            aria-label={t("fleet.searchAria")}
            className="pl-8"
          />
        </div>
        <Select value={status} onValueChange={(value) => setStatus(value as Filter)} disabled={!list.data}>
          <SelectTrigger className="w-full sm:w-44" aria-label={t("fleet.filterAria")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {FILTERS.map((filter) => (
              <SelectItem key={filter} value={filter}>
                {filter === "all" ? t("common.all") : t(`status.${filter}`)} (
                {counts[filter] ?? 0})
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <p className="text-xs text-muted-foreground sm:ml-auto" aria-live="polite">
          {refreshedAt ? t("common.updatedAt", { time: formatClock(refreshedAt) }) : "…"}
        </p>
      </div>

      <div className="rounded-md border" aria-busy={list.loading || list.refreshing}>
        {list.loading && !list.data ? (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[120px]">{t("common.status")}</TableHead>
                    <TableHead>{t("common.name")}</TableHead>
                    <TableHead className="hidden lg:table-cell">{t("fleet.model")}</TableHead>
                    <TableHead className="hidden md:table-cell">{t("fleet.channel")}</TableHead>
                    <TableHead className="hidden lg:table-cell">{t("nav.shares")}</TableHead>
                    <TableHead className="w-[90px]">
                      <span className="sr-only">{t("common.actions")}</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {Array.from({ length: 4 }).map((_, index) => (
                    <TableRow key={index}>
                      <TableCell>
                        <Skeleton className="h-5 w-20" />
                        <Skeleton className="mt-1.5 h-3 w-12" />
                      </TableCell>
                      <TableCell>
                        <Skeleton className="h-4 w-28" />
                        <Skeleton className="mt-1.5 h-3 w-16" />
                      </TableCell>
                      <TableCell className="hidden lg:table-cell">
                        <Skeleton className="h-4 w-40" />
                      </TableCell>
                      <TableCell className="hidden md:table-cell">
                        <Skeleton className="h-4 w-16" />
                      </TableCell>
                      <TableCell className="hidden lg:table-cell">
                        <Skeleton className="h-5 w-24" />
                      </TableCell>
                      <TableCell>
                        <Skeleton className="ml-auto size-7" />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
            <div className="divide-y md:hidden">
              {Array.from({ length: 4 }).map((_, index) => (
                <div key={index} className="space-y-3 p-4">
                  <div className="flex items-center justify-between gap-3">
                    <div className="space-y-1.5">
                      <Skeleton className="h-4 w-28" />
                      <Skeleton className="h-3 w-16" />
                    </div>
                    <Skeleton className="h-5 w-20" />
                  </div>
                  <Skeleton className="h-3 w-40" />
                  <Skeleton className="h-7 w-full" />
                </div>
              ))}
            </div>
          </>
        ) : rows.length === 0 ? (
          emptyState
        ) : (
          <>
            <div className="hidden md:block">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[120px]">{t("common.status")}</TableHead>
                    <TableHead>{t("common.name")}</TableHead>
                    <TableHead className="hidden lg:table-cell">{t("fleet.model")}</TableHead>
                    <TableHead className="hidden md:table-cell">{t("fleet.channel")}</TableHead>
                    <TableHead className="hidden lg:table-cell">{t("nav.shares")}</TableHead>
                    <TableHead className="w-[90px]">
                      <span className="sr-only">{t("common.actions")}</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((row) => {
                    const detail = details[row.id];
                    const egressEntry = egressByInstance.get(row.id);
                    const uptime =
                      row.status === "running" && detail?.container
                        ? formatUptime(detail.container.uptime_s)
                        : null;
                    return (
                      <TableRow key={row.id}>
                        <TableCell>
                          <div className="flex flex-col items-start gap-1">
                            <StatusBadge status={row.status} />
                            <span className="font-mono text-xs text-muted-foreground">{uptime ?? "—"}</span>
                          </div>
                        </TableCell>
                        <TableCell>
                          <a
                            data-row-link
                            href={hrefFor(`/instances/${row.id}`)}
                            className="block max-w-[260px] rounded-sm"
                          >
                            <span className="flex min-w-0 items-center gap-2">
                              <span className="min-w-0 truncate text-sm font-medium hover:underline">
                                {row.name}
                              </span>
                              {row.allow_lan ? <LanBadge /> : null}
                              {egressEntry ? (
                                <EgressBadge
                                  policy={egressEntry.policy}
                                  proxyReachable={egressEntry.proxy_reachable}
                                />
                              ) : null}
                              <PersonaBadge
                                row={row}
                                name={row.persona ? personaNames.get(row.persona) : undefined}
                              />
                            </span>
                            <span className="block truncate font-mono text-xs text-muted-foreground">
                              {row.id}
                              <span aria-hidden> · </span>
                              {row.template}
                            </span>
                          </a>
                        </TableCell>
                        <TableCell className="hidden lg:table-cell">
                          <span className="block max-w-[220px] truncate font-mono text-xs">{row.model}</span>
                        </TableCell>
                        <TableCell className="hidden md:table-cell">
                          <span className="flex items-center gap-1.5 text-sm">
                            <ChannelIcon channel={row.channel} />
                            {t(`channel.${row.channel}`)}
                          </span>
                        </TableCell>
                        <TableCell className="hidden lg:table-cell">
                          {row.shares.length ? (
                            <div className="flex flex-wrap items-center gap-1">
                              {row.shares.slice(0, 2).map((name) => {
                                const mode = detail?.shares.find((share) => share.name === name)?.mode;
                                return (
                                  <Badge
                                    key={name}
                                    variant="outline"
                                    className="gap-1 font-mono text-xs font-normal"
                                  >
                                    {name}
                                    {mode ? <span className="text-muted-foreground">{mode}</span> : null}
                                  </Badge>
                                );
                              })}
                              {row.shares.length > 2 ? (
                                <span className="text-xs text-muted-foreground">
                                  +{row.shares.length - 2}
                                </span>
                              ) : null}
                            </div>
                          ) : (
                            <span className="text-xs text-muted-foreground">—</span>
                          )}
                        </TableCell>
                        <TableCell className="text-right">
                          {canEdit ? (
                            <RowMenu
                              row={row}
                              pending={pendingId === row.id}
                              onAction={(action) => runAction(row.id, action)}
                              onDelete={() => setDeleteTarget(row)}
                            />
                          ) : (
                            <Button asChild variant="outline" size="sm">
                              <a href={hrefFor(`/instances/${row.id}`)}>{t("common.open")}</a>
                            </Button>
                          )}
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            </div>

            <div className="divide-y md:hidden">
              {rows.map((row) => {
                const detail = details[row.id];
                const egressEntry = egressByInstance.get(row.id);
                const uptime =
                  row.status === "running" && detail?.container
                    ? formatUptime(detail.container.uptime_s)
                    : "—";
                return (
                  <div key={row.id} className="space-y-3 p-4">
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="flex items-center gap-2">
                          <a
                            data-row-link
                            href={hrefFor(`/instances/${row.id}`)}
                            className="min-w-0 truncate text-sm font-medium"
                          >
                            {row.name}
                          </a>
                          {row.allow_lan ? <LanBadge /> : null}
                          {egressEntry ? (
                            <EgressBadge
                              policy={egressEntry.policy}
                              proxyReachable={egressEntry.proxy_reachable}
                            />
                          ) : null}
                          <PersonaBadge
                            row={row}
                            name={row.persona ? personaNames.get(row.persona) : undefined}
                          />
                        </div>
                        <p className="truncate font-mono text-xs text-muted-foreground">
                          {row.id}
                          <span aria-hidden> · </span>
                          {row.template}
                        </p>
                      </div>
                      <StatusBadge status={row.status} />
                    </div>
                    <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-xs">
                      <div className="min-w-0">
                        <dt className="text-muted-foreground">{t("fleet.model")}</dt>
                        <dd className="truncate font-mono">{row.model}</dd>
                      </div>
                      <div>
                        <dt className="text-muted-foreground">{t("fleet.channel")}</dt>
                        <dd className="flex items-center gap-1.5">
                          <ChannelIcon channel={row.channel} />
                          {t(`channel.${row.channel}`)}
                        </dd>
                      </div>
                      <div>
                        <dt className="text-muted-foreground">{t("fleet.uptime")}</dt>
                        <dd className="font-mono">{uptime}</dd>
                      </div>
                      <div className="min-w-0">
                        <dt className="text-muted-foreground">{t("nav.shares")}</dt>
                        <dd className="truncate font-mono">
                          {row.shares.length ? row.shares.join(", ") : "—"}
                        </dd>
                      </div>
                    </dl>
                    <div className="flex items-center justify-end gap-2">
                      <Button asChild variant="outline" size="sm">
                        <a href={hrefFor(`/instances/${row.id}`)}>{t("common.open")}</a>
                      </Button>
                      {canEdit ? (
                        <RowMenu
                          row={row}
                          pending={pendingId === row.id}
                          onAction={(action) => runAction(row.id, action)}
                          onDelete={() => setDeleteTarget(row)}
                        />
                      ) : null}
                    </div>
                  </div>
                );
              })}
            </div>
          </>
        )}
      </div>

      <p className="hidden text-xs text-muted-foreground md:block">
        <span className="font-mono">j</span> <span className="font-mono">k</span>{" "}
        {t("fleet.keysMove")} · {t("fleet.keysEnter")} ·{" "}
        <span className="font-mono">/</span> {t("fleet.keysSearch")}
        {canEdit ? (
          <>
            {" "}· <span className="font-mono">c</span> {t("fleet.keysCreate")}
          </>
        ) : null}
      </p>

      <DeleteInstanceDialog
        instance={
          deleteTarget
            ? { id: deleteTarget.id, name: deleteTarget.name }
            : null
        }
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        onDeleted={() => {
          const id = deleteTarget?.id;
          setDeleteTarget(null);
          toast.success(
            id ? t("fleet.deletedId", { id }) : t("fleet.deleted"),
          );
          setNonce((value) => value + 1);
        }}
      />

      <DoctorDialog open={doctorOpen} onOpenChange={setDoctorOpen} />
    </>
  );
}
