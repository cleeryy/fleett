import { useEffect, useRef, useState } from "react";
import {
  CopyPlus,
  Loader2,
  MoreHorizontal,
  Play,
  RefreshCw,
  RotateCw,
  Square,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { InstanceDetail } from "@/api/types";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { AgentActivityPanel } from "@/components/agent/activity";
import { AgentAdvancedPanel } from "@/components/agent/advanced";
import {
  AgentConfigurePanel,
  toConfigDraft,
  type ConfigDraft,
} from "@/components/agent/configure";
import { AgentFilesPanel } from "@/components/agent/files";
import { AgentOverviewPanel } from "@/components/agent/overview";
import { CopyButton } from "@/components/copy-button";
import { DeleteInstanceDialog } from "@/components/delete-instance-dialog";
import { DuplicateInstanceDialog } from "@/components/duplicate-instance-dialog";
import { ErrorAlert } from "@/components/error-alert";
import { PageHeader } from "@/components/page-header";
import { PersonaStateBadge } from "@/components/persona-state-badge";
import { StatusBadge } from "@/components/status-badge";
import {
  AGENT_TAB_LABEL_KEYS,
  agentTabHref,
  isAgentTab,
  legacyTabOf,
  resolveAgentTab,
  type ActivityView,
  type AgentTab,
  type AgentTabState,
} from "@/lib/agent-tabs";
import {
  ASSISTANT_APPLIED_EVENT,
  instanceTabSchema,
  useAssistantScope,
  type AssistantAppliedDetail,
} from "@/lib/assistant";
import { envRowsFromDetail, envRowsMatch, type EnvRow } from "@/lib/env";
import { INSTANCES_CHANGED_EVENT } from "@/lib/events";
import { formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { useUiMode } from "@/lib/mode";
import { navigate, useRoute } from "@/lib/router";
import { useCanEdit } from "@/lib/session";
import { useResource } from "@/lib/use-resource";
import { cn } from "@/lib/utils";

/** Per-browser memory of the creation success panel, keyed by instance id. */
const CREATED_KEY = "fleett.onboard.created";

function readCreatedMarker(): string | null {
  try {
    return localStorage.getItem(CREATED_KEY);
  } catch {
    return null;
  }
}

function DetailSkeleton() {
  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <Skeleton className="h-7 w-44" />
        <Skeleton className="h-4 w-72" />
      </div>
      <Skeleton className="h-9 w-80" />
      <div className="space-y-2">
        <Skeleton className="h-11 w-full rounded-md" />
        <Skeleton className="h-11 w-full rounded-md" />
        <Skeleton className="h-11 w-full rounded-md" />
      </div>
    </div>
  );
}

export function InstanceDetailPage({ id }: { id: string }) {
  const { t, lang } = useI18n();
  const canEdit = useCanEdit();
  const { simple } = useUiMode();
  const route = useRoute();
  const tabParam = route.query.get("tab");
  const viewParam = route.query.get("view");
  const sectionParam = route.query.get("section");
  const createdParam = route.query.get("created");

  const [tabState, setTabState] = useState<AgentTabState>(() =>
    resolveAgentTab(tabParam, viewParam, sectionParam, canEdit),
  );
  const appliedQuery = useRef(
    `${tabParam ?? ""}\u0000${viewParam ?? ""}\u0000${sectionParam ?? ""}\u0000${canEdit}`,
  );
  const focusEnv = route.query.get("focus") === "env";
  const focusEnvParam = route.query.get("env");
  const focusEnvApplied = useRef<string | null>(null);
  // The Files tab is mounted on first visit and then kept alive (hidden), so an
  // unsaved buffer survives a trip to Overview and back.
  const [filesMounted, setFilesMounted] = useState(false);
  const [nonce, setNonce] = useState(0);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ApiError | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [duplicateOpen, setDuplicateOpen] = useState(false);
  const [createdMarker, setCreatedMarker] = useState(readCreatedMarker);
  const [createdDismissed, setCreatedDismissed] = useState(false);

  const detail = useResource(() => api.getInstance(id), [id, nonce]);
  const providers = useResource(() => api.listProviders(), []);
  const personas = useResource(() => api.listPersonas(), []);
  const templatesResource = useResource(() => api.listTemplates(), []);

  const tab = tabState.tab;
  const activityView: ActivityView = tabState.view ?? "logs";
  const created =
    createdParam === "1" && !createdDismissed && createdMarker !== id;

  // The controller still indexes its schemas by the legacy tab ids.
  useAssistantScope({
    page: t(AGENT_TAB_LABEL_KEYS[tab]),
    instance: id,
    schema: instanceTabSchema(legacyTabOf(tabState)),
    errorText: detail.data?.last_error ?? undefined,
  });

  // An environment applied from the assistant is a normal PATCH: refresh so the
  // Configuration tab and the warnings show the new state.
  useEffect(() => {
    function onApplied(event: Event) {
      const applied = (event as CustomEvent<AssistantAppliedDetail>).detail;
      if (!applied || applied.instance !== id || applied.kind !== "env") return;
      setNonce((value) => value + 1);
    }
    window.addEventListener(ASSISTANT_APPLIED_EVENT, onApplied);
    return () => window.removeEventListener(ASSISTANT_APPLIED_EVENT, onApplied);
  }, [id]);

  // A lifecycle action from the command palette targets this instance too.
  useEffect(() => {
    function onChanged() {
      setNonce((value) => value + 1);
    }
    window.addEventListener(INSTANCES_CHANGED_EVENT, onChanged);
    return () => window.removeEventListener(INSTANCES_CHANGED_EVENT, onChanged);
  }, []);

  // The palette, a bookmark or the assistant can send a new `?tab=` while this
  // page is already open. New ids and legacy ids both resolve here.
  useEffect(() => {
    const key = `${tabParam ?? ""}\u0000${viewParam ?? ""}\u0000${sectionParam ?? ""}\u0000${canEdit}`;
    if (key === appliedQuery.current) return;
    appliedQuery.current = key;
    setTabState(resolveAgentTab(tabParam, viewParam, sectionParam, canEdit));
  }, [tabParam, viewParam, sectionParam, canEdit]);

  // The setup success links straight to the channel allowlist:
  // `?tab=configuration&focus=env&env=DISCORD_ALLOWED_USERS`. Reveal the row
  // that holds the variable (or the suggestion that adds it) once per load.
  useEffect(() => {
    if (!focusEnv || tab !== "configure") return;
    const data = detail.data;
    if (!data) return;
    const template = templatesResource.data?.find((candidate) => candidate.id === data.template);
    const key =
      (focusEnvParam && /^[A-Z_][A-Z0-9_]*$/.test(focusEnvParam) ? focusEnvParam : null) ??
      template?.env.find((entry) => entry.required && /ALLOW(?:ED)?_USERS/.test(entry.key))?.key ??
      null;
    // Without an explicit key, wait for the template suggestions that name it.
    if (!key && templatesResource.data === null && !templatesResource.error) return;
    const marker = `${data.id}:${key ?? "env"}`;
    if (focusEnvApplied.current === marker) return;
    focusEnvApplied.current = marker;
    // The configuration tab grows while shares, personas and skills load. Keep
    // the row in view for a short window instead of scrolling into a page that
    // is still assembling; a click, wheel or key hands control back.
    let stopped = false;
    let observer: ResizeObserver | null = null;
    const stop = () => {
      stopped = true;
      observer?.disconnect();
    };
    const reveal = () => {
      if (stopped) return;
      const row = key ? document.querySelector<HTMLElement>(`[data-env-key="${key}"]`) : null;
      const suggestion = key
        ? document.querySelector<HTMLButtonElement>(`[data-env-suggestion="${key}"]`)
        : null;
      const target = row ?? suggestion ?? document.getElementById("instance-env");
      if (!target) return;
      const rect = target.getBoundingClientRect();
      const visible = rect.top >= 72 && rect.bottom <= window.innerHeight;
      if (!visible) target.scrollIntoView({ block: "center" });
      if (row) row.querySelector<HTMLButtonElement>("button")?.focus({ preventScroll: true });
      else suggestion?.focus({ preventScroll: true });
    };
    observer = new ResizeObserver(reveal);
    observer.observe(document.getElementById("main") ?? document.body);
    const stopTimer = window.setTimeout(stop, 4000);
    const timers = [80, 420, 900, 1500].map((delay) => window.setTimeout(reveal, delay));
    const cancelEvents: (keyof WindowEventMap)[] = ["pointerdown", "wheel", "touchstart", "keydown"];
    for (const type of cancelEvents) window.addEventListener(type, stop, { once: true, passive: true });
    return () => {
      stopped = true;
      window.clearTimeout(stopTimer);
      for (const timer of timers) window.clearTimeout(timer);
      for (const type of cancelEvents) window.removeEventListener(type, stop);
      observer?.disconnect();
    };
  }, [focusEnv, focusEnvParam, tab, detail.data, templatesResource.data]);

  const [configDraft, setConfigDraft] = useState<ConfigDraft | null>(null);
  const [envRows, setEnvRows] = useState<EnvRow[] | null>(null);
  const configDraftFor = useRef<string | null>(null);
  const configResetPending = useRef(false);
  const envResetPending = useRef(false);
  const envServerRef = useRef<InstanceDetail["env"] | null>(null);

  useEffect(() => {
    const data = detail.data;
    if (!data) return;
    // Keep edits across tab switches and background reloads, but take the
    // server copy after a save. A config save must not drop unsaved variable
    // edits, and an env apply must not drop unsaved config edits. An untouched
    // variable list follows the server, so a rotated gate key shows up.
    const newInstance = configDraftFor.current !== data.id;
    const serverChanged = envServerRef.current !== data.env;
    const envClean =
      envRows === null || (envServerRef.current !== null && envRowsMatch(envServerRef.current, envRows));
    const resetDraft = newInstance || configResetPending.current;
    const resetEnv = newInstance || envResetPending.current || (serverChanged && envClean);
    if (!resetDraft && !resetEnv) return;
    if (resetDraft) setConfigDraft(toConfigDraft(data));
    if (resetEnv) setEnvRows(envRowsFromDetail(data.env));
    configDraftFor.current = data.id;
    configResetPending.current = false;
    envResetPending.current = false;
    envServerRef.current = data.env;
  }, [detail.data, envRows]);

  useEffect(() => {
    if (tab === "files") setFilesMounted(true);
  }, [tab]);

  function openTab(next: AgentTab, view?: ActivityView) {
    const state: AgentTabState =
      next === "activity"
        ? { tab: next, view: view ?? (tab === "activity" ? activityView : "logs") }
        : { tab: next };
    navigate(agentTabHref(route.path, state), { replace: true });
  }

  function openActivityView(next: ActivityView) {
    navigate(agentTabHref(route.path, { tab: "activity", view: next }), { replace: true });
  }

  function handleSaved() {
    configResetPending.current = true;
    setNonce((value) => value + 1);
  }

  function handleEnvApplied() {
    envResetPending.current = true;
    setNonce((value) => value + 1);
  }

  function dismissCreated() {
    try {
      localStorage.setItem(CREATED_KEY, id);
    } catch {
      /* storage can be blocked (private mode); the panel stays dismissed in memory */
    }
    setCreatedMarker(id);
    setCreatedDismissed(true);
    const query = new URLSearchParams(route.query);
    query.delete("created");
    const search = query.toString();
    navigate(`${route.path}${search ? `?${search}` : ""}`, { replace: true });
  }

  async function run(action: "start" | "stop" | "restart") {
    setPendingAction(action);
    setActionError(null);
    try {
      await api.action(id, action);
      toast.success(
        t("detail.actionDone", {
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
    } catch (cause) {
      setActionError(toApiError(cause));
    } finally {
      setPendingAction(null);
      setNonce((value) => value + 1);
    }
  }

  if (detail.loading && !detail.data) return <DetailSkeleton />;

  if (!detail.data) {
    return (
      <>
        <PageHeader
          title={id}
          actions={
            <Button asChild variant="outline">
              <a href="#/instances">{t("shell.backToInstances")}</a>
            </Button>
          }
        />
        {detail.error ? <ErrorAlert error={detail.error} onRetry={detail.reload} /> : null}
      </>
    );
  }

  const data = detail.data;
  const running = data.status === "running";
  const busy = pendingAction !== null;
  const provider = data.provider
    ? (providers.data?.find((candidate) => candidate.id === data.provider) ?? null)
    : null;
  const persona = data.persona
    ? (personas.data?.find((candidate) => candidate.id === data.persona) ?? null)
    : null;
  const template =
    templatesResource.data?.find((candidate) => candidate.id === data.template) ?? null;
  const templateEnv = template?.env ?? [];
  // Deep link from the setup success: the allowlist variable to reveal.
  const highlightEnvKey = focusEnv
    ? ((focusEnvParam && /^[A-Z_][A-Z0-9_]*$/.test(focusEnvParam) ? focusEnvParam : null) ??
      templateEnv.find((entry) => entry.required && /ALLOW(?:ED)?_USERS/.test(entry.key))?.key ??
      null)
    : null;
  const warnings = data.warnings ?? [];
  const envWarning = warnings.some((warning) =>
    /required|allowlist|environment|is not set/i.test(warning),
  );

  function reviewWarnings() {
    navigate(agentTabHref(route.path, { tab: "configure" }), { replace: true });
    window.setTimeout(() => {
      const node = document.getElementById(envWarning ? "instance-env" : "cfg-name");
      if (envWarning) node?.scrollIntoView({ block: "start" });
      node?.querySelector?.("input")?.focus();
      if (node instanceof HTMLInputElement) node.focus();
    }, 120);
  }

  const tabs: AgentTab[] = canEdit
    ? ["overview", "configure", "files", "activity", "advanced"]
    : ["overview", "files", "activity", "advanced"];
  // Simple mode hides Advanced from the bar; a deep link still renders it.
  const visibleTabs = simple ? tabs.filter((candidate) => candidate !== "advanced") : tabs;

  return (
    <>
      <PageHeader
        title={
          <span className="flex flex-wrap items-center gap-3">
            {data.name}
            <StatusBadge status={data.status} />
            <PersonaStateBadge state={data.persona ? data.persona_state : "none"} />
          </span>
        }
        description={
          <span className="flex flex-col gap-1">
            <span className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
              <span>{persona?.name ?? data.persona ?? t("agent.header.noPersona")}</span>
              <span aria-hidden>·</span>
              <span>{t(`channel.${data.channel}`)}</span>
              <span aria-hidden>·</span>
              <span>{t("agent.header.lastActivity", { when: formatRelative(data.updated_at, lang) })}</span>
            </span>
            <span className="flex min-w-0 items-center gap-1">
              <span className="font-mono text-xs break-all">{data.id}</span>
              <CopyButton
                value={data.id}
                label={t("agent.header.copyId")}
                size="icon-xs"
                className="-my-1 shrink-0"
              />
            </span>
          </span>
        }
        actions={
          <>
            <Button
              variant="outline"
              size="icon-sm"
              className={canEdit ? "hidden sm:inline-flex" : undefined}
              onClick={() => setNonce((value) => value + 1)}
              aria-label={t("detail.refresh")}
              title={t("detail.refresh")}
            >
              <RefreshCw aria-hidden className={detail.refreshing ? "animate-spin" : undefined} />
            </Button>
            {canEdit ? (
              <>
                {running ? (
                  <Button size="sm" onClick={() => run("stop")} disabled={busy}>
                    {pendingAction === "stop" ? (
                      <Loader2 aria-hidden className="animate-spin" />
                    ) : (
                      <Square aria-hidden />
                    )}
                    {t("action.stop")}
                  </Button>
                ) : (
                  <Button size="sm" onClick={() => run("start")} disabled={busy}>
                    {pendingAction === "start" ? (
                      <Loader2 aria-hidden className="animate-spin" />
                    ) : (
                      <Play aria-hidden />
                    )}
                    {t("action.start")}
                  </Button>
                )}
                <Button
                  variant="outline"
                  size="sm"
                  className="hidden sm:inline-flex"
                  onClick={() => run("restart")}
                  disabled={busy}
                >
                  {pendingAction === "restart" ? (
                    <Loader2 aria-hidden className="animate-spin" />
                  ) : (
                    <RotateCw aria-hidden />
                  )}
                  {t("action.restart")}
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="outline"
                      size="icon-sm"
                      aria-label={t("providers.detail.moreActions")}
                      title={t("providers.detail.moreActions")}
                      disabled={busy}
                    >
                      <MoreHorizontal aria-hidden />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem
                      className="sm:hidden"
                      disabled={busy}
                      onSelect={() => run("restart")}
                    >
                      <RotateCw aria-hidden />
                      {t("action.restart")}
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      className="sm:hidden"
                      onSelect={() => setNonce((value) => value + 1)}
                    >
                      <RefreshCw aria-hidden />
                      {t("detail.refresh")}
                    </DropdownMenuItem>
                    <DropdownMenuSeparator className="sm:hidden" />
                    <DropdownMenuItem onSelect={() => setDuplicateOpen(true)}>
                      <CopyPlus aria-hidden />
                      {t("detail.duplicate")}
                    </DropdownMenuItem>
                    <DropdownMenuSeparator />
                    <DropdownMenuItem variant="destructive" onSelect={() => setDeleteOpen(true)}>
                      <Trash2 aria-hidden />
                      {t("detail.deleteInstance")}
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </>
            ) : null}
          </>
        }
      />

      {actionError ? <ErrorAlert error={actionError} /> : null}

      <Tabs
        value={tab}
        onValueChange={(value) => {
          if (isAgentTab(value)) openTab(value);
        }}
      >
        <div className="-mx-1 overflow-x-auto px-1">
          <TabsList className="h-auto w-max justify-start sm:h-9">
            {visibleTabs.map((candidate) => (
              <TabsTrigger
                key={candidate}
                value={candidate}
                className="min-h-11 px-3 sm:min-h-0"
              >
                {t(AGENT_TAB_LABEL_KEYS[candidate])}
              </TabsTrigger>
            ))}
          </TabsList>
        </div>

        <TabsContent value="overview" className="mt-4">
          <AgentOverviewPanel
            detail={data}
            provider={provider}
            persona={persona}
            template={template}
            canEdit={canEdit}
            created={created}
            onDismissCreated={dismissCreated}
            envWarning={envWarning}
            onReview={reviewWarnings}
          />
        </TabsContent>

        {canEdit ? (
          <TabsContent value="configure" className="mt-4">
            {configDraft && envRows ? (
              <AgentConfigurePanel
                detail={data}
                provider={provider}
                templateEnv={templateEnv}
                highlightEnvKey={highlightEnvKey}
                draft={configDraft}
                onDraftChange={setConfigDraft}
                envRows={envRows}
                onEnvRowsChange={setEnvRows}
                onSaved={handleSaved}
                onEnvApplied={handleEnvApplied}
              />
            ) : (
              <Skeleton className="h-64 w-full rounded-md" />
            )}
          </TabsContent>
        ) : null}

        {/* `forceMount` keeps the editor mounted while another tab is shown;
            the `hidden` class keeps an inactive panel out of the a11y tree. */}
        <TabsContent
          value="files"
          forceMount
          className={cn("mt-4", tab !== "files" && "hidden")}
        >
          {filesMounted ? (
            <AgentFilesPanel
              id={data.id}
              canEdit={canEdit}
              active={tab === "files"}
              onRestarted={() => setNonce((value) => value + 1)}
              onFileSaved={() => setNonce((value) => value + 1)}
            />
          ) : null}
        </TabsContent>

        <TabsContent value="activity" className="mt-4">
          <AgentActivityPanel
            id={data.id}
            canEdit={canEdit}
            view={activityView}
            onViewChange={openActivityView}
          />
        </TabsContent>

        <TabsContent value="advanced" className="mt-4">
          <AgentAdvancedPanel detail={data} canEdit={canEdit} section={tabState.section} />
        </TabsContent>
      </Tabs>

      {canEdit ? (
        <>
          <DuplicateInstanceDialog
            source={data}
            open={duplicateOpen}
            onOpenChange={setDuplicateOpen}
            onCloned={(clone) => {
              toast.success(t("detail.duplicated", { source: data.id, clone: clone.id }));
              navigate(`/instances/${clone.id}`);
            }}
          />
          <DeleteInstanceDialog
            instance={{ id: data.id, name: data.name }}
            open={deleteOpen}
            onOpenChange={setDeleteOpen}
            onDeleted={() => {
              toast.success(t("fleet.deletedId", { id: data.id }));
              navigate("/instances");
            }}
          />
        </>
      ) : null}
    </>
  );
}
