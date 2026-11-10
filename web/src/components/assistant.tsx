import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent,
} from "react";
import {
  ArrowUp,
  Loader2,
  Paperclip,
  Plug,
  ShieldCheck,
  Sparkles,
  Trash2,
  TriangleAlert,
  X,
} from "lucide-react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type {
  AssistantConfig,
  AssistantFinishReason,
  AssistantProposal,
  AssistantRedaction,
  AssistantUsage,
} from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Textarea } from "@/components/ui/textarea";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { AssistantProposalCard, type LoadedProposalFile } from "@/components/assistant-proposal";
import {
  AssistantProviderSettings,
  AssistantSourceLabel,
} from "@/components/assistant-provider";
import { ErrorAlert } from "@/components/error-alert";
import {
  QUICK_TASKS,
  baseName,
  buildAssistantContext,
  pageForPath,
  pageSelection,
  schemaForPath,
  taskInstruction,
  useAssistantScopeValue,
  type QuickTask,
} from "@/lib/assistant";
import { apiErrorMessage } from "@/api/errors";
import { ASSISTANT_CONFIG_EVENT } from "@/lib/events";
import { formatBytes, formatTokens } from "@/lib/format";
import { hasTranslation, useI18n } from "@/lib/i18n";
import { useRoute } from "@/lib/router";
import { useCanEdit } from "@/lib/session";

interface StoredTurn {
  role: "user" | "assistant";
  content: string;
  proposal?: AssistantProposal;
  usage?: AssistantUsage;
  /** The reply stopped at the output ceiling (issue #68). */
  truncated?: boolean;
  finishReason?: AssistantFinishReason;
}

interface Conversation {
  sessionId: string;
  turns: StoredTurn[];
}

function newSessionId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `s-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function newConversation(): Conversation {
  return { sessionId: newSessionId(), turns: [] };
}

const DESKTOP_QUERY = "(min-width: 768px)";

function subscribeDesktop(listener: () => void): () => void {
  const query = window.matchMedia(DESKTOP_QUERY);
  query.addEventListener("change", listener);
  return () => query.removeEventListener("change", listener);
}

function useIsDesktop(): boolean {
  return useSyncExternalStore(
    subscribeDesktop,
    () => window.matchMedia(DESKTOP_QUERY).matches,
    () => false,
  );
}

function DisclosureList({
  title,
  items,
  empty,
}: {
  title: string;
  items: string[];
  empty: string;
}) {
  return (
    <div className="space-y-1">
      <p className="text-xs font-medium">{title}</p>
      {items.length > 0 ? (
        <ul className="space-y-1 text-xs text-muted-foreground">
          {items.map((item, index) => (
            <li key={`${item}-${index}`} className="flex gap-1.5">
              <span aria-hidden className="shrink-0">
                ·
              </span>
              <span className="min-w-0 break-words">{item}</span>
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-xs text-muted-foreground">{empty}</p>
      )}
    </div>
  );
}

/** Floating entry point plus the contextual side panel (a sheet on phones). */
export function AssistantPanel() {
  const { t } = useI18n();
  const canEdit = useCanEdit();
  const route = useRoute();
  const scope = useAssistantScopeValue();
  const desktop = useIsDesktop();

  const [config, setConfig] = useState<AssistantConfig | null>(null);
  const [configError, setConfigError] = useState<ApiError | null>(null);
  const [open, setOpen] = useState(false);
  const [threads, setThreads] = useState<Record<string, Conversation>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [usedTokens, setUsedTokens] = useState(0);
  const [lastRedaction, setLastRedaction] = useState<AssistantRedaction | null>(null);
  const [draft, setDraft] = useState("");
  const [pageSel, setPageSel] = useState("");
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const transcriptRef = useRef<HTMLDivElement>(null);
  /** Last user turn, so a readable parse failure can be retried (issue #69). */
  const lastRequest = useRef<{ text: string; task?: QuickTask } | null>(null);

  const page = scope.page ?? pageForPath(route.path);
  const schema = scope.schema ?? schemaForPath(route.path);
  const threadKey = scope.instance ?? "fleet";
  const conversation = threads[threadKey] ?? null;
  const turns = conversation?.turns ?? [];
  const selection = (scope.selection ?? "").trim();
  const liveSelection = (pageSel || selection).trim();

  const loadConfig = useCallback(async () => {
    try {
      const value = await api.assistantConfig();
      setConfig(value);
      setConfigError(null);
      setUsedTokens(value.used_tokens);
    } catch (cause) {
      setConfig(null);
      setConfigError(toApiError(cause));
    }
  }, []);

  /** Persisted selection accepted: adopt the resolved config without a refetch. */
  const applyConfig = useCallback((value: AssistantConfig) => {
    setConfig(value);
    setConfigError(null);
    setUsedTokens(value.used_tokens);
    setError(null);
  }, []);

  useEffect(() => {
    void loadConfig();
  }, [loadConfig]);

  // The Providers page (or another panel) can save the selection first.
  useEffect(() => {
    function onConfig(event: Event) {
      const detail = (event as CustomEvent<AssistantConfig>).detail;
      if (detail) applyConfig(detail);
    }
    window.addEventListener(ASSISTANT_CONFIG_EVENT, onConfig);
    return () => window.removeEventListener(ASSISTANT_CONFIG_EVENT, onConfig);
  }, [applyConfig]);

  // Usage and the provider resolution are authoritative on the controller:
  // re-read them when the panel opens, so a selection saved elsewhere shows up.
  useEffect(() => {
    if (!open) return;
    let alive = true;
    api
      .assistantConfig()
      .then((value) => {
        if (!alive) return;
        setConfig(value);
        setConfigError(null);
        setUsedTokens(value.used_tokens);
      })
      .catch((cause) => {
        if (!alive) return;
        setConfigError(toApiError(cause));
      });
    return () => {
      alive = false;
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const node = transcriptRef.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [open, turns.length, busy, error]);

  // Keep the chip honest when the operator selects text on the page while the
  // panel is open. Selections inside the panel are ignored.
  useEffect(() => {
    if (!open) return;
    const update = () => setPageSel(pageSelection());
    update();
    document.addEventListener("selectionchange", update);
    return () => document.removeEventListener("selectionchange", update);
  }, [open]);

  useEffect(() => {
    if (!open || !desktop) return;
    const timer = window.setTimeout(() => composerRef.current?.focus({ preventScroll: true }), 0);
    return () => window.clearTimeout(timer);
  }, [open, desktop]);

  useEffect(() => {
    if (!open || !desktop) return;
    function onKey(event: globalThis.KeyboardEvent) {
      // A popover or the sheet handles Escape first: only close the panel when
      // nothing inside consumed the key.
      if (event.key === "Escape" && !event.defaultPrevented) setOpen(false);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, desktop]);

  // `FLEETT_ASSISTANT=off` answers 404: nothing is rendered at all.
  const hidden = config ? !config.enabled : configError ? configError.status === 404 : true;

  const providerMissing = config !== null && config.provider === null;
  const notSelected = providerMissing && config?.reason === "not_selected";
  const noProvider = providerMissing && !notSelected;
  /** The saved reference points at a deleted provider: repair before writing. */
  const repair = configError?.code === "assistant_provider_unknown";
  const notConfigured = notSelected || noProvider || repair || error?.code === "assistant_not_configured";
  const budgetExceeded = error?.code === "assistant_budget_exceeded";
  const composerDisabled = busy || config === null || notConfigured || budgetExceeded;

  const expectedSent = useMemo(() => {
    const sent = [t("assistant.pageContext", { page })];
    if (scope.instance) sent.push(t("assistant.instanceContext", { instance: scope.instance }));
    if (scope.path) {
      const size = scope.fileContent
        ? ` (${formatBytes(new TextEncoder().encode(scope.fileContent).length)})`
        : "";
      sent.push(t("assistant.fileContext", { path: `${scope.path}${size}` }));
    }
    if (liveSelection) {
      sent.push(t("assistant.selectionContext", { count: liveSelection.length }));
    }
    if (schema) sent.push(t("assistant.schemaContext"));
    return sent;
  }, [page, scope.instance, scope.path, scope.fileContent, liveSelection, schema, t]);

  function clearConversation(): void {
    setThreads((previous) => ({ ...previous, [threadKey]: newConversation() }));
    setError(null);
    setLastRedaction(null);
    setDraft("");
    void loadConfig();
  }

  async function send(text: string, task?: QuickTask): Promise<void> {
    const content = text.trim();
    if (!content || busy) return;
    lastRequest.current = task ? { text: content, task } : { text: content };

    const current = threads[threadKey] ?? newConversation();
    const nextTurns: StoredTurn[] = [...current.turns, { role: "user", content }];
    setThreads((previous) => ({
      ...previous,
      [threadKey]: { ...current, turns: nextTurns },
    }));
    setBusy(true);
    setError(null);
    setDraft("");

    try {
      const reply = await api.assistantChat({
        session_id: current.sessionId,
        context: buildAssistantContext(
          { ...scope, schema, selection: pageSelection() || selection },
          page,
        ),
        messages: nextTurns.map(({ role, content: body }) => ({ role, content: body })),
        ...(task ? { task: task.id } : {}),
      });
      setThreads((previous) => {
        const latest = previous[threadKey] ?? current;
        return {
          ...previous,
          [threadKey]: {
            ...latest,
            turns: [
              ...latest.turns,
              {
                role: "assistant",
                content: reply.message,
                proposal: reply.proposal,
                usage: reply.usage,
                truncated: reply.truncated,
                finishReason: reply.finish_reason,
              },
            ],
          },
        };
      });
      setLastRedaction(reply.redaction);
      setUsedTokens((previous) => previous + reply.usage.total);
    } catch (cause) {
      const failure = toApiError(cause);
      setError(failure);
      // The provider can vanish while the panel is open: re-resolve so the
      // chooser or the repair state replaces the stale header.
      if (
        failure.code === "assistant_not_configured" ||
        failure.code === "assistant_provider_unknown"
      ) {
        void loadConfig();
      }
    } finally {
      setBusy(false);
    }
  }

  function runQuickTask(task: QuickTask): void {
    void send(taskInstruction(task, scope.errorText), task);
  }

  /** Retry the last user turn after a readable parse failure (issue #69). */
  function retryLast(): void {
    const last = lastRequest.current;
    if (last) void send(last.text, last.task);
  }

  function submit(): void {
    void send(draft);
  }

  function onComposerKeyDown(event: KeyboardEvent<HTMLTextAreaElement>): void {
    if (event.key !== "Enter" || event.shiftKey) return;
    if (event.nativeEvent.isComposing) return;
    event.preventDefault();
    submit();
  }

  if (hidden) return null;

  const providerSource = config?.provider ? config.provider_source : null;
  // The chip must not say "No provider" when one exists but nothing is
  // selected (#63), nor while the config is still loading.
  const providerChip = config?.provider ? (
    <Badge variant="secondary" className="max-w-40 font-normal">
      <span className="truncate">{config.provider.name}</span>
    </Badge>
  ) : notSelected ? (
    <Badge variant="outline" className="font-normal">
      {t("assistant.notSelectedChip")}
    </Badge>
  ) : repair ? (
    <Badge variant="outline" className="border-destructive/40 font-normal text-destructive">
      {t("assistant.repairChip")}
    </Badge>
  ) : noProvider ? (
    <Badge variant="outline" className="font-normal">
      {t("assistant.noProvider")}
    </Badge>
  ) : null;
  const model = config?.model?.trim();
  const budget = config?.budget_tokens ?? 0;
  const percent = budget > 0 ? Math.min(100, Math.round((usedTokens / budget) * 100)) : 0;
  const contextLabel = [scope.instance, page, scope.path ? baseName(scope.path) : null]
    .filter(Boolean)
    .join(" · ");

  const loadedFile: LoadedProposalFile | null =
    scope.path && scope.fileContent !== undefined
      ? { path: scope.path, content: scope.fileContent, etag: scope.etag ?? null }
      : null;

  const header = (
    <header className="shrink-0 border-b px-4 py-3">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="flex items-center gap-1.5 text-sm font-semibold">
              <Sparkles aria-hidden className="size-4 text-muted-foreground" />
              {t("assistant.title")}
            </h2>
            {providerChip}
            {providerSource ? <AssistantSourceLabel source={providerSource} /> : null}
          </div>
          {config ? (
            <p className="mt-1 flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[11px] text-muted-foreground">
              {model ? (
                <span className="truncate font-mono">{model}</span>
              ) : (
                <span>{t("assistant.noModel")}</span>
              )}
              <span aria-hidden>·</span>
              <span className="flex items-center gap-1.5">
                <span>
                  {t("assistant.tokenUsage", {
                    used: formatTokens(usedTokens),
                    budget: formatTokens(budget),
                  })}
                </span>
                <span
                  className="h-1 w-14 overflow-hidden rounded-full bg-muted"
                  role="progressbar"
                  aria-label={t("assistant.budgetAria")}
                  aria-valuemin={0}
                  aria-valuemax={Math.max(budget, 1)}
                  aria-valuenow={Math.min(usedTokens, budget)}
                >
                  <span
                    className="block h-full rounded-full bg-foreground/30"
                    style={{ width: `${percent}%` }}
                  />
                </span>
              </span>
            </p>
          ) : null}
          <div className="mt-2">
            <Popover>
              <PopoverTrigger asChild>
                <Button type="button" variant="outline" size="xs" className="gap-1.5 font-normal">
                  <ShieldCheck aria-hidden />
                  {t("assistant.whatIsSent")}
                </Button>
              </PopoverTrigger>
              <PopoverContent align="end" className="w-80 p-0">
                <div className="border-b px-3 py-2">
                  <p className="text-xs font-medium">{t("assistant.contextTitle")}</p>
                  <p className="text-[11px] text-muted-foreground">
                    {lastRedaction
                      ? t("assistant.contextSent")
                      : t("assistant.contextWillSend")}
                  </p>
                </div>
                <div className="space-y-3 px-3 py-3">
                  <DisclosureList
                    title={t("assistant.included")}
                    items={lastRedaction ? lastRedaction.sent : expectedSent}
                    empty={t("assistant.noContext")}
                  />
                  <DisclosureList
                    title={t("assistant.leftOnHost")}
                    items={
                      lastRedaction
                        ? lastRedaction.omitted
                        : [
                            t("assistant.withheldEnv"),
                            t("assistant.withheldCredentials"),
                            t("assistant.withheldFiles"),
                          ]
                    }
                    empty={t("assistant.nothingWithheld")}
                  />
                  {lastRedaction?.redacted ? (
                    <p className="border-t pt-2 text-[11px] text-muted-foreground">
                      {t(
                        lastRedaction.redacted === 1
                          ? "assistant.redactedOne"
                          : "assistant.redactedMany",
                        { count: lastRedaction.redacted },
                      )}
                    </p>
                  ) : null}
                </div>
              </PopoverContent>
            </Popover>
          </div>
        </div>
        <div className="flex shrink-0 items-center">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={t("assistant.clearAria")}
            title={t("assistant.clearAria")}
            onClick={clearConversation}
            disabled={turns.length === 0 && !error}
          >
            <Trash2 aria-hidden />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={t("assistant.closeAria")}
            title={t("assistant.closeAria")}
            onClick={() => setOpen(false)}
          >
            <X aria-hidden />
          </Button>
        </div>
      </div>
    </header>
  );

  const banners = (
    <>
      {configError && configError.status !== 404 ? (
        <div className="shrink-0 px-4 pt-3">
          {repair ? (
            <AssistantProviderSettings
              config={config}
              error={configError}
              canEdit={canEdit}
              onResolved={applyConfig}
              surface="panel"
            />
          ) : (
            <ErrorAlert error={configError} onRetry={() => void loadConfig()} />
          )}
        </div>
      ) : null}
      {notSelected ? (
        <div className="shrink-0 px-4 pt-3">
          <AssistantProviderSettings
            config={config}
            canEdit={canEdit}
            onResolved={applyConfig}
            surface="panel"
          />
        </div>
      ) : null}
      {noProvider ? (
        <div className="shrink-0 px-4 pt-3">
          <Alert className="items-start">
            <Plug aria-hidden className="mt-0.5" />
            <AlertTitle>{t("assistant.noProviderTitle")}</AlertTitle>
            <AlertDescription>
              <p>{t("assistant.noProviderBody")}</p>
              {canEdit ? (
                <Button asChild variant="outline" size="sm" className="mt-2">
                  <a href="#/providers?new=1" onClick={() => setOpen(false)}>
                    {t("assistant.openProviders")}
                  </a>
                </Button>
              ) : (
                <p className="mt-2 text-xs">{t("assistant.chooseReadOnly")}</p>
              )}
            </AlertDescription>
          </Alert>
        </div>
      ) : null}
      {budgetExceeded ? (
        <div className="shrink-0 px-4 pt-3">
          <Alert variant="destructive" className="items-start">
            <TriangleAlert aria-hidden className="mt-0.5" />
            <AlertTitle>{t("assistant.budgetTitle")}</AlertTitle>
            <AlertDescription>
              <p>{error ? apiErrorMessage(error, t) : t("assistant.budgetBody")}</p>
              <Button
                size="sm"
                variant="outline"
                className="mt-2"
                onClick={clearConversation}
              >
                {t("assistant.newConversation")}
              </Button>
            </AlertDescription>
          </Alert>
        </div>
      ) : null}
      {error && !notConfigured && !budgetExceeded ? (
        <div className="shrink-0 px-4 pt-3">
          <ErrorAlert
            error={error}
            onRetry={error.code === "assistant_reply_unusable" ? retryLast : undefined}
          />
        </div>
      ) : null}
    </>
  );

  const transcript = (
    <div
      ref={transcriptRef}
      className="min-h-0 flex-1 space-y-4 overflow-y-auto px-4 py-4"
      aria-live="polite"
    >
      {turns.length === 0 && !notConfigured ? (
        <div className="space-y-1 py-6 text-center">
          <p className="text-sm font-medium">{t("assistant.emptyTitle")}</p>
          <p className="text-xs text-muted-foreground">{t("assistant.emptyBody")}</p>
        </div>
      ) : null}

      {turns.map((turn, index) =>
        turn.role === "user" ? (
          <div key={index} className="flex justify-end">
            <p className="max-w-[85%] rounded-md bg-muted px-3 py-2 text-sm break-words whitespace-pre-wrap">
              {turn.content}
            </p>
          </div>
        ) : (
          <div key={index} className="space-y-2">
            <p className="text-sm break-words whitespace-pre-wrap">{turn.content}</p>
            {turn.proposal && !turn.truncated ? (
              <AssistantProposalCard
                proposal={turn.proposal}
                instance={scope.instance}
                canEdit={canEdit}
                loaded={loadedFile}
              />
            ) : null}
            {turn.truncated ? (
              <Alert className="items-start">
                <TriangleAlert aria-hidden className="mt-0.5" />
                <AlertTitle>{t("assistant.truncatedTitle")}</AlertTitle>
                <AlertDescription>
                  <p>{t("assistant.truncatedBody")}</p>
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    className="mt-2"
                    disabled={composerDisabled}
                    onClick={() => void send(t("assistant.continuePrompt"))}
                  >
                    {t("assistant.continue")}
                  </Button>
                </AlertDescription>
              </Alert>
            ) : null}
            {turn.usage ? (
              <p className="text-[11px] text-muted-foreground">
                {t("assistant.usageLine", {
                  total: formatTokens(turn.usage.total),
                  prompt: formatTokens(turn.usage.prompt),
                  completion: formatTokens(turn.usage.completion),
                })}
              </p>
            ) : null}
          </div>
        ),
      )}

      {busy ? (
        <p className="flex items-center gap-2 text-xs text-muted-foreground" aria-live="polite">
          <Loader2 aria-hidden className="size-3 animate-spin" />
          {t("assistant.thinking")}
        </p>
      ) : null}
    </div>
  );

  const footer = (
    <div className="shrink-0 space-y-2 border-t px-4 pt-3 pb-[calc(0.75rem+env(safe-area-inset-bottom))]">
      <div
        className="flex items-center gap-1.5 rounded-md border bg-muted/30 px-2 py-1.5 text-[11px] text-muted-foreground"
        title={t("assistant.attachedContext")}
      >
        <Paperclip aria-hidden className="size-3 shrink-0" />
        <span className="min-w-0 flex-1 truncate font-mono text-foreground/80">
          {contextLabel || "fleett"}
        </span>
        {liveSelection ? (
          <span className="shrink-0">
            {t("assistant.charsSelected", { count: liveSelection.length })}
          </span>
        ) : null}
      </div>

      <div className="flex flex-wrap gap-1.5">
        {QUICK_TASKS.map((task) => (
          <Button
            key={task.id}
            type="button"
            variant="outline"
            size="xs"
            className="shrink-0 font-normal"
            title={task.instruction}
            onClick={() => runQuickTask(task)}
            disabled={composerDisabled}
          >
            {hasTranslation(`assistant.task.${task.id}`)
              ? t(`assistant.task.${task.id}`)
              : task.label}
          </Button>
        ))}
      </div>

      <div className="flex items-end gap-2">
        <Textarea
          ref={composerRef}
          value={draft}
          rows={2}
          aria-label={t("assistant.inputAria")}
          placeholder={
            repair
              ? t("assistant.placeholderRepair")
              : notSelected
                ? t("assistant.placeholderChoose")
                : notConfigured
                  ? t("assistant.placeholderNoProvider")
                  : budgetExceeded
                    ? t("assistant.placeholderBudget")
                    : t("assistant.placeholder")
          }
          className="max-h-40 min-h-16 resize-none"
          disabled={composerDisabled}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={onComposerKeyDown}
        />
        <Button
          type="button"
          size="icon"
          aria-label={t("messages.send")}
          title={t("messages.send")}
          onClick={submit}
          disabled={composerDisabled || draft.trim().length === 0}
        >
          {busy ? <Loader2 aria-hidden className="animate-spin" /> : <ArrowUp aria-hidden />}
        </Button>
      </div>
      <p className="text-[11px] text-muted-foreground">{t("assistant.footerHint")}</p>
    </div>
  );

  const panel = (
    <div data-assistant-panel className="flex h-full min-h-0 flex-col bg-background">
      {header}
      {banners}
      {transcript}
      {footer}
    </div>
  );

  return (
    <>
      {!open ? (
        <Button
          type="button"
          onClick={() => setOpen(true)}
          aria-label={t("assistant.openAria")}
          className="fixed right-4 bottom-[calc(1rem+env(safe-area-inset-bottom))] z-30 h-11 rounded-full px-3.5 shadow-lg"
        >
          <Sparkles aria-hidden />
          <span className="hidden sm:inline">{t("assistant.title")}</span>
        </Button>
      ) : null}

      {desktop ? (
        open ? (
          <aside
            role="dialog"
            aria-label={t("assistant.title")}
            aria-modal={false}
            className="fixed inset-y-0 right-0 z-40 flex w-[420px] max-w-full flex-col border-l bg-background shadow-xl"
          >
            {panel}
          </aside>
        ) : null
      ) : (
        <Sheet open={open} onOpenChange={setOpen}>
          <SheetContent
            side="right"
            showCloseButton={false}
            className="w-full gap-0 p-0 pt-[env(safe-area-inset-top)] sm:max-w-md"
          >
            <SheetHeader className="sr-only">
              <SheetTitle>{t("assistant.title")}</SheetTitle>
              <SheetDescription>{t("assistant.sheetDescription")}</SheetDescription>
            </SheetHeader>
            {panel}
          </SheetContent>
        </Sheet>
      )}
    </>
  );
}
