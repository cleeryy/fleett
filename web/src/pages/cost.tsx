import { useEffect, useMemo, useState, type FormEvent } from "react";
import { CircleDollarSign, Loader2, Pencil, Plus, RefreshCw, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { BudgetStatus, CostGroupBy, Pricing, PricingInput } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { OpsBadge } from "@/components/ops-badge";
import { PageHeader } from "@/components/page-header";
import { RichText } from "@/components/rich-text";
import {
  dateInputValue,
  formatCost,
  formatDateTime,
  formatPercentValue,
  formatRate,
  formatRelative,
  formatTokens,
} from "@/lib/format";
import { useI18n } from "@/lib/i18n";
import { useIdentity } from "@/lib/session";
import { useResource } from "@/lib/use-resource";

function StatCard({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="rounded-md border px-4 py-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="mt-1 font-mono text-lg font-medium">{value}</p>
      {hint ? <p className="mt-0.5 text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  );
}

function BudgetDialog({
  open,
  onOpenChange,
  budget,
  instances,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  budget: BudgetStatus | null;
  instances: { id: string; name: string }[];
  onSaved: () => void;
}) {
  const { t } = useI18n();
  const [instanceId, setInstanceId] = useState("");
  const [monthly, setMonthly] = useState("25");
  const [enabled, setEnabled] = useState(true);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setInstanceId(budget?.instance_id ?? instances[0]?.id ?? "");
    setMonthly(budget ? String(budget.monthly_usd) : "25");
    setEnabled(budget?.enabled ?? true);
    setError(null);
  }, [open, budget, instances]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = Number(monthly);
    if (!instanceId || !Number.isFinite(value) || value < 0 || value > 100_000) {
      setError(toApiError(new Error(t("cost.budget.invalid"))));
      return;
    }
    setPending(true);
    setError(null);
    try {
      await api.setBudget(instanceId, { monthly_usd: value, enabled });
      toast.success(t("cost.budget.saved", { instance: instanceId }));
      onOpenChange(false);
      onSaved();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next);
        if (!next) setError(null);
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {budget ? t("cost.budget.titleEdit", { instance: budget.instance_id }) : t("cost.budget.titleNew")}
          </DialogTitle>
          <DialogDescription>{t("cost.budget.description")}</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}
          <div className="space-y-2">
            <Label htmlFor="budget-instance">{t("messages.instance")}</Label>
            <Select value={instanceId} onValueChange={setInstanceId} disabled={!!budget}>
              <SelectTrigger id="budget-instance" className="w-full">
                <SelectValue placeholder={t("cost.budget.pickInstance")} />
              </SelectTrigger>
              <SelectContent>
                {instances.map((instance) => (
                  <SelectItem key={instance.id} value={instance.id}>
                    {instance.name} · {instance.id}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-2">
            <Label htmlFor="budget-monthly">{t("cost.budget.monthlyLimit")}</Label>
            <Input
              id="budget-monthly"
              type="number"
              min={0}
              max={100_000}
              step="0.01"
              value={monthly}
              className="font-mono"
              onChange={(event) => setMonthly(event.target.value)}
            />
          </div>
          <div className="flex items-center gap-2">
            <Checkbox
              id="budget-enabled"
              checked={enabled}
              onCheckedChange={(value) => setEnabled(value === true)}
            />
            <Label htmlFor="budget-enabled" className="cursor-pointer text-sm font-normal">
              {t("common.enabled")}
            </Label>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              {t("cost.budget.save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function PricingDialog({
  open,
  onOpenChange,
  pricing,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  pricing: Pricing | null;
  onSaved: () => void;
}) {
  const { t } = useI18n();
  const [model, setModel] = useState("");
  const [inputPrice, setInputPrice] = useState("0.27");
  const [outputPrice, setOutputPrice] = useState("1.10");
  const [currency, setCurrency] = useState("USD");
  const [effective, setEffective] = useState(dateInputValue());
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  useEffect(() => {
    if (!open) return;
    setModel(pricing?.model ?? "");
    setInputPrice(String(pricing?.input_per_mtok ?? 0.27));
    setOutputPrice(String(pricing?.output_per_mtok ?? 1.1));
    setCurrency(pricing?.currency ?? "USD");
    setEffective(pricing?.effective_from ?? dateInputValue());
    setError(null);
  }, [open, pricing]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const input: PricingInput = {
      model: model.trim(),
      input_per_mtok: Number(inputPrice),
      output_per_mtok: Number(outputPrice),
      currency: currency.trim() || "USD",
      effective_from: effective,
    };
    if (!input.model) {
      setError(toApiError(new Error(t("cost.pricing.modelRequired"))));
      return;
    }
    if (!Number.isFinite(input.input_per_mtok) || !Number.isFinite(input.output_per_mtok)) {
      setError(toApiError(new Error(t("cost.pricing.priceInvalid"))));
      return;
    }
    setPending(true);
    setError(null);
    try {
      await api.setPricing(input);
      toast.success(t("cost.pricing.saved", { model: input.model }));
      onOpenChange(false);
      onSaved();
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {pricing
              ? t("cost.pricing.titleEdit", { model: pricing.model })
              : t("cost.pricing.titleNew")}
          </DialogTitle>
          <DialogDescription>
            <RichText text={t("cost.pricing.description")} />
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}
          <div className="space-y-2">
            <Label htmlFor="pricing-model">{t("fleet.model")}</Label>
            <Input
              id="pricing-model"
              value={model}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              disabled={!!pricing}
              onChange={(event) => setModel(event.target.value)}
            />
          </div>
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="pricing-input">{t("cost.pricing.input")}</Label>
              <Input
                id="pricing-input"
                type="number"
                min={0}
                step="0.01"
                value={inputPrice}
                className="font-mono"
                onChange={(event) => setInputPrice(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="pricing-output">{t("cost.pricing.output")}</Label>
              <Input
                id="pricing-output"
                type="number"
                min={0}
                step="0.01"
                value={outputPrice}
                className="font-mono"
                onChange={(event) => setOutputPrice(event.target.value)}
              />
            </div>
          </div>
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="pricing-currency">{t("cost.pricing.currency")}</Label>
              <Input
                id="pricing-currency"
                value={currency}
                maxLength={8}
                className="font-mono"
                onChange={(event) => setCurrency(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="pricing-effective">{t("cost.pricing.effective")}</Label>
              <Input
                id="pricing-effective"
                type="date"
                value={effective}
                className="font-mono"
                onChange={(event) => setEffective(event.target.value)}
              />
            </div>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              {t("cost.pricing.save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function CostPage() {
  const { t, lang } = useI18n();
  const identity = useIdentity();
  const admin = identity?.role === "admin";
  const [tab, setTab] = useState("usage");
  const [nonce, setNonce] = useState(0);
  const [from, setFrom] = useState(() => {
    const now = new Date();
    return dateInputValue(new Date(now.getFullYear(), now.getMonth(), 1));
  });
  const [to, setTo] = useState(() => dateInputValue());
  const [groupBy, setGroupBy] = useState<CostGroupBy>("instance");
  const [budgetOpen, setBudgetOpen] = useState(false);
  const [budgetTarget, setBudgetTarget] = useState<BudgetStatus | null>(null);
  const [pricingOpen, setPricingOpen] = useState(false);
  const [pricingTarget, setPricingTarget] = useState<Pricing | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<Pricing | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<ApiError | null>(null);

  const report = useResource(() => api.costReport({ from, to, group_by: groupBy }), [
    from,
    to,
    groupBy,
    nonce,
  ]);
  const budgets = useResource(() => api.listBudgets(), [nonce]);
  const pricing = useResource(() => api.listPricing(), [nonce]);
  const instances = useResource(() => api.listInstances(), []);
  const instanceOptions = useMemo(
    () => (instances.data ?? []).map((row) => ({ id: row.id, name: row.name })),
    [instances.data],
  );
  const data = report.data;

  const budgetRows = useMemo(() => budgets.data ?? [], [budgets.data]);
  const totalCost = data?.totals.cost_usd ?? 0;

  async function removePricing() {
    if (!deleteTarget) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      await api.deletePricing(deleteTarget.model);
      toast.success(t("cost.pricing.deleted", { model: deleteTarget.model }));
      setDeleteTarget(null);
      setNonce((value) => value + 1);
    } catch (cause) {
      setDeleteError(toApiError(cause));
    } finally {
      setDeleting(false);
    }
  }

  return (
    <>
      <PageHeader
        title={t("nav.cost")}
        description={
          data
            ? t("cost.summary", {
                cost: formatCost(data.totals.cost_usd, data.currency),
                tokens: formatTokens(data.totals.total_tokens),
                groups: data.groups.length,
                groupBy: t(`cost.groupBy.${data.group_by}`),
              })
            : t("cost.loading")
        }
        actions={
          <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
            <RefreshCw aria-hidden className={report.refreshing ? "animate-spin" : undefined} />
            {t("common.refresh")}
          </Button>
        }
      />

      <Tabs value={tab} onValueChange={setTab}>
        <div className="overflow-x-auto">
          <TabsList className="w-full justify-start sm:w-fit">
            <TabsTrigger value="usage">{t("cost.usageTab")}</TabsTrigger>
            <TabsTrigger value="pricing">{t("cost.pricingTab")}</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="usage" className="mt-4 space-y-4">
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <div className="space-y-1">
              <Label htmlFor="cost-from">{t("cost.from")}</Label>
              <Input
                id="cost-from"
                type="date"
                value={from}
                className="font-mono"
                onChange={(event) => setFrom(event.target.value)}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="cost-to">{t("cost.to")}</Label>
              <Input
                id="cost-to"
                type="date"
                value={to}
                className="font-mono"
                onChange={(event) => setTo(event.target.value)}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="cost-group">{t("cost.groupByLabel")}</Label>
              <Select value={groupBy} onValueChange={(value) => setGroupBy(value as CostGroupBy)}>
                <SelectTrigger id="cost-group" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="instance">{t("cost.groupBy.instance")}</SelectItem>
                  <SelectItem value="model">{t("cost.groupBy.model")}</SelectItem>
                  <SelectItem value="day">{t("cost.groupBy.day")}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="flex items-end">
              <p className="text-xs text-muted-foreground">
                <RichText text={t("cost.dateHint")} />
              </p>
            </div>
          </div>

          {report.error ? <ErrorAlert error={report.error} onRetry={report.reload} /> : null}

          {data ? (
            <>
              <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
                <StatCard
                  label={t("cost.stat.range")}
                  value={formatCost(data.totals.cost_usd, data.currency)}
                  hint={t("cost.stat.tokensHint", {
                    count: formatTokens(data.totals.total_tokens),
                  })}
                />
                <StatCard
                  label={t("cost.stat.promptCompletion")}
                  value={`${formatTokens(data.totals.prompt_tokens)} / ${formatTokens(data.totals.completion_tokens)}`}
                  hint={t("cost.stat.eventsHint", { count: data.totals.events })}
                />
                <StatCard
                  label={t("cost.stat.projected")}
                  value={formatCost(data.projection.projected_usd, data.currency)}
                  hint={t("cost.stat.elapsed", {
                    rate: formatRate(data.projection.elapsed_fraction),
                    month: data.projection.month,
                  })}
                />
                <StatCard
                  label={t("cost.stat.budgetsExceeded")}
                  value={String(data.budgets.filter((budget) => budget.exceeded).length)}
                  hint={
                    data.budgets.length > 0
                      ? t("cost.stat.configured", { count: data.budgets.length })
                      : t("cost.stat.noneConfigured")
                  }
                />
              </div>

              <div className="grid gap-6 lg:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
                <section className="space-y-2">
                  <div className="flex items-center justify-between gap-2">
                    <h2 className="text-sm font-medium">
                      {t("cost.breakdown", { groupBy: t(`cost.groupBy.${data.group_by}`) })}
                    </h2>
                    <span className="text-xs text-muted-foreground">
                      {t(
                        data.groups.length === 1
                          ? "cost.groupCountOne"
                          : "cost.groupCountMany",
                        { count: data.groups.length },
                      )}
                    </span>
                  </div>
                  <div className="rounded-md border">
                    {data.groups.length === 0 ? (
                      <EmptyState
                        icon={CircleDollarSign}
                        title={t("cost.noUsageTitle")}
                        description={t("cost.noUsageDescription")}
                      />
                    ) : (
                      <>
                        <div className="hidden sm:block">
                          <Table>
                            <TableHeader>
                              <TableRow>
                                <TableHead>{t(`cost.groupBy.${data.group_by}`)}</TableHead>
                                <TableHead className="w-[120px]">{t("cost.tokens")}</TableHead>
                                <TableHead className="w-[100px]">{t("cost.events")}</TableHead>
                                <TableHead className="w-[110px]">{t("cost.cost")}</TableHead>
                                <TableHead className="w-[90px]">{t("cost.share")}</TableHead>
                              </TableRow>
                            </TableHeader>
                            <TableBody>
                              {data.groups.map((group) => (
                                <TableRow key={group.key}>
                                  <TableCell className="font-mono text-xs break-all">{group.key}</TableCell>
                                  <TableCell className="font-mono text-xs">
                                    {formatTokens(group.total_tokens)}
                                  </TableCell>
                                  <TableCell className="font-mono text-xs">{group.events}</TableCell>
                                  <TableCell className="font-mono text-xs">
                                    {formatCost(group.cost_usd, data.currency)}
                                  </TableCell>
                                  <TableCell className="text-xs text-muted-foreground">
                                    {formatPercentValue(totalCost > 0 ? (group.cost_usd / totalCost) * 100 : 0)}
                                  </TableCell>
                                </TableRow>
                              ))}
                            </TableBody>
                          </Table>
                        </div>
                        <ul className="divide-y sm:hidden">
                          {data.groups.map((group) => (
                            <li key={group.key} className="flex items-start justify-between gap-3 p-3">
                              <div className="min-w-0">
                                <p className="truncate font-mono text-xs">{group.key}</p>
                                <p className="text-xs text-muted-foreground">
                                  {t("cost.groupLine", {
                                    tokens: formatTokens(group.total_tokens),
                                    events: group.events,
                                  })}
                                </p>
                              </div>
                              <span className="font-mono text-xs">
                                {formatCost(group.cost_usd, data.currency)}
                              </span>
                            </li>
                          ))}
                        </ul>
                      </>
                    )}
                  </div>
                </section>

                <section className="space-y-2">
                  <h2 className="text-sm font-medium">{t("cost.topAgents")}</h2>
                  <div className="rounded-md border">
                    {data.top_agents.length === 0 ? (
                      <p className="px-3 py-6 text-center text-sm text-muted-foreground">
                        {t("cost.noAgents")}
                      </p>
                    ) : (
                      <ul className="divide-y">
                        {data.top_agents.map((agent, index) => (
                          <li key={agent.instance_id} className="flex items-center gap-3 px-3 py-2.5">
                            <span className="w-5 shrink-0 font-mono text-xs text-muted-foreground">
                              {index + 1}
                            </span>
                            <div className="min-w-0 flex-1">
                              <p className="truncate font-mono text-xs">{agent.instance_id}</p>
                              <p className="text-[11px] text-muted-foreground">
                                {t("cost.tokensOnly", {
                                  tokens: formatTokens(agent.total_tokens),
                                })}
                              </p>
                            </div>
                            <span className="font-mono text-xs">
                              {formatCost(agent.cost_usd, data.currency)}
                            </span>
                          </li>
                        ))}
                      </ul>
                    )}
                  </div>
                </section>
              </div>

              <section className="space-y-2">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <h2 className="text-sm font-medium">{t("cost.budgets")}</h2>
                  {admin ? (
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        setBudgetTarget(null);
                        setBudgetOpen(true);
                      }}
                    >
                      <Plus aria-hidden />
                      {t("cost.setBudget")}
                    </Button>
                  ) : null}
                </div>
                {budgets.error ? <ErrorAlert error={budgets.error} onRetry={budgets.reload} /> : null}
                <div className="rounded-md border">
                  {budgetRows.length === 0 ? (
                    <EmptyState
                      icon={CircleDollarSign}
                      title={t("cost.noBudgetTitle")}
                      description={t("cost.noBudgetDescription")}
                      action={
                        admin ? (
                          <Button
                            size="sm"
                            onClick={() => {
                              setBudgetTarget(null);
                              setBudgetOpen(true);
                            }}
                          >
                            <Plus aria-hidden />
                            {t("cost.setBudget")}
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
                              <TableHead>{t("messages.instance")}</TableHead>
                              <TableHead className="w-[110px]">{t("cost.month")}</TableHead>
                              <TableHead className="w-[110px]">{t("cost.limit")}</TableHead>
                              <TableHead className="w-[110px]">{t("cost.used")}</TableHead>
                              <TableHead className="w-[160px]">{t("cost.usage")}</TableHead>
                              <TableHead className="w-[110px]">{t("cost.projected")}</TableHead>
                              <TableHead className="w-[90px]">
                                <span className="sr-only">{t("common.actions")}</span>
                              </TableHead>
                            </TableRow>
                          </TableHeader>
                          <TableBody>
                            {budgetRows.map((budget) => (
                              <TableRow key={budget.instance_id}>
                                <TableCell>
                                  <span className="block font-mono text-xs">{budget.instance_id}</span>
                                  {budget.exceeded ? (
                                    <OpsBadge status="exceeded" />
                                  ) : (
                                    <span className="text-[11px] text-muted-foreground">
                                      {budget.enabled ? t("common.enabled") : t("common.disabled")}
                                    </span>
                                  )}
                                </TableCell>
                                <TableCell className="font-mono text-xs">{budget.month}</TableCell>
                                <TableCell className="font-mono text-xs">
                                  {formatCost(budget.monthly_usd)}
                                </TableCell>
                                <TableCell className="font-mono text-xs">
                                  {formatCost(budget.used_usd)}
                                </TableCell>
                                <TableCell>
                                  <div className="flex items-center gap-2">
                                    <progress
                                      value={Math.min(100, budget.percent)}
                                      max={100}
                                      aria-label={t("cost.budgetUsageAria", {
                                        instance: budget.instance_id,
                                      })}
                                      className="h-1.5 w-20 overflow-hidden rounded-full accent-emerald-600"
                                    >
                                      {formatPercentValue(budget.percent)}
                                    </progress>
                                    <span className="text-xs">{formatPercentValue(budget.percent)}</span>
                                  </div>
                                </TableCell>
                                <TableCell className="font-mono text-xs">
                                  {formatCost(budget.projected_month_usd)}
                                </TableCell>
                                <TableCell className="text-right">
                                  {admin ? (
                                    <Button
                                      variant="ghost"
                                      size="icon-sm"
                                      aria-label={t("cost.editBudgetAria", {
                                        instance: budget.instance_id,
                                      })}
                                      title={t("cost.editBudgetAria", {
                                        instance: budget.instance_id,
                                      })}
                                      onClick={() => {
                                        setBudgetTarget(budget);
                                        setBudgetOpen(true);
                                      }}
                                    >
                                      <Pencil aria-hidden />
                                    </Button>
                                  ) : null}
                                </TableCell>
                              </TableRow>
                            ))}
                          </TableBody>
                        </Table>
                      </div>
                      <ul className="divide-y md:hidden">
                        {budgetRows.map((budget) => (
                          <li key={budget.instance_id} className="space-y-2 p-4">
                            <div className="flex items-center justify-between gap-2">
                              <span className="font-mono text-sm">{budget.instance_id}</span>
                              <OpsBadge status={budget.exceeded ? "exceeded" : "ok"} />
                            </div>
                            <dl className="grid grid-cols-2 gap-2 text-xs">
                              <div>
                                <dt className="text-muted-foreground">{t("cost.limit")}</dt>
                                <dd className="font-mono">{formatCost(budget.monthly_usd)}</dd>
                              </div>
                              <div>
                                <dt className="text-muted-foreground">{t("cost.used")}</dt>
                                <dd className="font-mono">{formatCost(budget.used_usd)}</dd>
                              </div>
                              <div>
                                <dt className="text-muted-foreground">{t("cost.percent")}</dt>
                                <dd className="font-mono">{formatPercentValue(budget.percent)}</dd>
                              </div>
                              <div>
                                <dt className="text-muted-foreground">{t("cost.projected")}</dt>
                                <dd className="font-mono">{formatCost(budget.projected_month_usd)}</dd>
                              </div>
                            </dl>
                            {admin ? (
                              <Button
                                variant="outline"
                                size="sm"
                                onClick={() => {
                                  setBudgetTarget(budget);
                                  setBudgetOpen(true);
                                }}
                              >
                                {t("cost.editBudget")}
                              </Button>
                            ) : null}
                          </li>
                        ))}
                      </ul>
                    </>
                  )}
                </div>
              </section>
            </>
          ) : report.loading ? (
            <div className="space-y-3">
              <Skeleton className="h-24 w-full" />
              <Skeleton className="h-64 w-full" />
            </div>
          ) : null}
        </TabsContent>

        <TabsContent value="pricing" className="mt-4 space-y-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <p className="text-sm text-muted-foreground">
              {t("cost.pricingHint")}
            </p>
            {admin ? (
              <Button
                size="sm"
                onClick={() => {
                  setPricingTarget(null);
                  setPricingOpen(true);
                }}
              >
                <Plus aria-hidden />
                {t("cost.addPrice")}
              </Button>
            ) : null}
          </div>
          {pricing.error ? <ErrorAlert error={pricing.error} onRetry={pricing.reload} /> : null}
          <div className="rounded-md border">
            {pricing.loading && !pricing.data ? (
              <div className="space-y-2 p-4">
                {Array.from({ length: 3 }).map((_, index) => (
                  <Skeleton key={index} className="h-12 w-full" />
                ))}
              </div>
            ) : (pricing.data ?? []).length === 0 ? (
              <EmptyState
                icon={CircleDollarSign}
                title={t("cost.noPricesTitle")}
                description={t("cost.noPricesDescription")}
              />
            ) : (
              <>
                <div className="hidden md:block">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>{t("fleet.model")}</TableHead>
                        <TableHead className="w-[130px]">{t("cost.pricing.input")}</TableHead>
                        <TableHead className="w-[130px]">{t("cost.pricing.output")}</TableHead>
                        <TableHead className="w-[100px]">{t("cost.pricing.currency")}</TableHead>
                        <TableHead className="w-[140px]">{t("cost.pricing.effective")}</TableHead>
                        <TableHead className="w-[150px]">{t("common.updated")}</TableHead>
                        <TableHead className="w-[110px]">
                          <span className="sr-only">{t("common.actions")}</span>
                        </TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {(pricing.data ?? []).map((price) => (
                        <TableRow key={price.model}>
                          <TableCell className="font-mono text-xs break-all">{price.model}</TableCell>
                          <TableCell className="font-mono text-xs">
                            {formatCost(price.input_per_mtok, price.currency)}
                          </TableCell>
                          <TableCell className="font-mono text-xs">
                            {formatCost(price.output_per_mtok, price.currency)}
                          </TableCell>
                          <TableCell>
                            <Badge variant="outline" className="font-mono text-xs font-normal">
                              {price.currency}
                            </Badge>
                          </TableCell>
                          <TableCell className="font-mono text-xs">{price.effective_from}</TableCell>
                          <TableCell className="text-xs text-muted-foreground">
                            {formatRelative(price.updated_at, lang)}
                          </TableCell>
                          <TableCell>
                            {admin ? (
                              <div className="flex justify-end gap-1">
                                <Button
                                  variant="ghost"
                                  size="icon-sm"
                                  aria-label={t("cost.newPriceAria", { model: price.model })}
                                  title={t("cost.newPriceAria", { model: price.model })}
                                  onClick={() => {
                                    setPricingTarget(price);
                                    setPricingOpen(true);
                                  }}
                                >
                                  <Pencil aria-hidden />
                                </Button>
                                <Button
                                  variant="ghost"
                                  size="icon-sm"
                                  aria-label={t("cost.deletePriceAria", { model: price.model })}
                                  title={t("cost.deletePriceAria", { model: price.model })}
                                  onClick={() => {
                                    setDeleteError(null);
                                    setDeleteTarget(price);
                                  }}
                                >
                                  <Trash2 aria-hidden />
                                </Button>
                              </div>
                            ) : null}
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
                <ul className="divide-y md:hidden">
                  {(pricing.data ?? []).map((price) => (
                    <li key={price.model} className="space-y-2 p-4">
                      <p className="font-mono text-sm break-all">{price.model}</p>
                      <dl className="grid grid-cols-2 gap-2 text-xs">
                        <div>
                          <dt className="text-muted-foreground">{t("cost.pricing.input")}</dt>
                          <dd className="font-mono">{formatCost(price.input_per_mtok, price.currency)}</dd>
                        </div>
                        <div>
                          <dt className="text-muted-foreground">{t("cost.pricing.output")}</dt>
                          <dd className="font-mono">{formatCost(price.output_per_mtok, price.currency)}</dd>
                        </div>
                        <div>
                          <dt className="text-muted-foreground">{t("cost.effective")}</dt>
                          <dd className="font-mono">{price.effective_from}</dd>
                        </div>
                        <div>
                          <dt className="text-muted-foreground">{t("common.updated")}</dt>
                          <dd className="font-mono">{formatDateTime(price.updated_at)}</dd>
                        </div>
                      </dl>
                      {admin ? (
                        <div className="flex justify-end gap-1">
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              setPricingTarget(price);
                              setPricingOpen(true);
                            }}
                          >
                            {t("cost.newVersion")}
                          </Button>
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              setDeleteError(null);
                              setDeleteTarget(price);
                            }}
                          >
                            {t("common.delete")}
                          </Button>
                        </div>
                      ) : null}
                    </li>
                  ))}
                </ul>
              </>
            )}
          </div>
          <p className="text-xs text-muted-foreground">
            {t("cost.budgetFooter", {
              count: budgetRows.length,
              enabled: budgetRows.filter((row) => row.enabled).length,
            })}
          </p>
        </TabsContent>
      </Tabs>

      <BudgetDialog
        open={budgetOpen}
        onOpenChange={setBudgetOpen}
        budget={budgetTarget}
        instances={instanceOptions}
        onSaved={() => setNonce((value) => value + 1)}
      />
      <PricingDialog
        open={pricingOpen}
        onOpenChange={setPricingOpen}
        pricing={pricingTarget}
        onSaved={() => setNonce((value) => value + 1)}
      />
      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        title={t("cost.deletePriceTitle")}
        description={
          deleteTarget
            ? t("cost.deletePriceDescription", { model: deleteTarget.model })
            : undefined
        }
        confirmLabel={t("cost.deletePrice")}
        destructive
        pending={deleting}
        error={deleteError}
        onConfirm={() => void removePricing()}
      />
    </>
  );
}
