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
import {
  dateInputValue,
  formatCost,
  formatDateTime,
  formatPercentValue,
  formatRate,
  formatRelative,
  formatTokens,
} from "@/lib/format";
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
      setError(toApiError(new Error("Pick an instance and enter a limit between 0 and 100000.")));
      return;
    }
    setPending(true);
    setError(null);
    try {
      await api.setBudget(instanceId, { monthly_usd: value, enabled });
      toast.success(`Budget saved for ${instanceId}.`);
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
          <DialogTitle>{budget ? `Budget · ${budget.instance_id}` : "Set a budget"}</DialogTitle>
          <DialogDescription>
            A monthly USD limit per instance. Crossing it emits a cost event; it does not stop the
            agent.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}
          <div className="space-y-2">
            <Label htmlFor="budget-instance">Instance</Label>
            <Select value={instanceId} onValueChange={setInstanceId} disabled={!!budget}>
              <SelectTrigger id="budget-instance" className="w-full">
                <SelectValue placeholder="Pick an instance" />
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
            <Label htmlFor="budget-monthly">Monthly limit (USD)</Label>
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
              Enabled
            </Label>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              Save budget
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
      setError(toApiError(new Error("Enter a model name.")));
      return;
    }
    if (!Number.isFinite(input.input_per_mtok) || !Number.isFinite(input.output_per_mtok)) {
      setError(toApiError(new Error("Enter numeric prices per million tokens.")));
      return;
    }
    setPending(true);
    setError(null);
    try {
      await api.setPricing(input);
      toast.success(`Price saved for ${input.model}.`);
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
          <DialogTitle>{pricing ? `New price version · ${pricing.model}` : "Add a price"}</DialogTitle>
          <DialogDescription>
            Prices are versioned by <span className="font-mono">effective_from</span>. A new date adds
            a row; historical reports keep the price effective on each event day.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          {error ? <ErrorAlert error={error} /> : null}
          <div className="space-y-2">
            <Label htmlFor="pricing-model">Model</Label>
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
              <Label htmlFor="pricing-input">Input / Mtok</Label>
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
              <Label htmlFor="pricing-output">Output / Mtok</Label>
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
              <Label htmlFor="pricing-currency">Currency</Label>
              <Input
                id="pricing-currency"
                value={currency}
                maxLength={8}
                className="font-mono"
                onChange={(event) => setCurrency(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="pricing-effective">Effective from</Label>
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
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              Save price
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function CostPage() {
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
      toast.success(`Price for ${deleteTarget.model} deleted.`);
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
        title="Cost"
        description={
          data
            ? `${formatCost(data.totals.cost_usd, data.currency)} across ${formatTokens(data.totals.total_tokens)} tokens · ${data.groups.length} groups · ${data.group_by}`
            : "Loading the cost report."
        }
        actions={
          <Button variant="outline" size="sm" onClick={() => setNonce((value) => value + 1)}>
            <RefreshCw aria-hidden className={report.refreshing ? "animate-spin" : undefined} />
            Refresh
          </Button>
        }
      />

      <Tabs value={tab} onValueChange={setTab}>
        <div className="overflow-x-auto">
          <TabsList className="w-full justify-start sm:w-fit">
            <TabsTrigger value="usage">Usage</TabsTrigger>
            <TabsTrigger value="pricing">Pricing</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="usage" className="mt-4 space-y-4">
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <div className="space-y-1">
              <Label htmlFor="cost-from">From</Label>
              <Input
                id="cost-from"
                type="date"
                value={from}
                className="font-mono"
                onChange={(event) => setFrom(event.target.value)}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="cost-to">To</Label>
              <Input
                id="cost-to"
                type="date"
                value={to}
                className="font-mono"
                onChange={(event) => setTo(event.target.value)}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="cost-group">Group by</Label>
              <Select value={groupBy} onValueChange={(value) => setGroupBy(value as CostGroupBy)}>
                <SelectTrigger id="cost-group" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="instance">Instance</SelectItem>
                  <SelectItem value="model">Model</SelectItem>
                  <SelectItem value="day">Day</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="flex items-end">
              <p className="text-xs text-muted-foreground">
                Dates are interpreted in UTC. The range is inclusive at{" "}
                <span className="font-mono">from</span>; a date <span className="font-mono">to</span>{" "}
                covers the whole day.
              </p>
            </div>
          </div>

          {report.error ? <ErrorAlert error={report.error} onRetry={report.reload} /> : null}

          {data ? (
            <>
              <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
                <StatCard
                  label="Cost in range"
                  value={formatCost(data.totals.cost_usd, data.currency)}
                  hint={`${formatTokens(data.totals.total_tokens)} tokens`}
                />
                <StatCard
                  label="Prompt / completion"
                  value={`${formatTokens(data.totals.prompt_tokens)} / ${formatTokens(data.totals.completion_tokens)}`}
                  hint={`${data.totals.events} events`}
                />
                <StatCard
                  label="Projected month"
                  value={formatCost(data.projection.projected_usd, data.currency)}
                  hint={`${formatRate(data.projection.elapsed_fraction)} of ${data.projection.month} elapsed`}
                />
                <StatCard
                  label="Budgets exceeded"
                  value={String(data.budgets.filter((budget) => budget.exceeded).length)}
                  hint={data.budgets.length > 0 ? `${data.budgets.length} configured` : "none configured"}
                />
              </div>

              <div className="grid gap-6 lg:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
                <section className="space-y-2">
                  <div className="flex items-center justify-between gap-2">
                    <h2 className="text-sm font-medium">Breakdown by {data.group_by}</h2>
                    <span className="text-xs text-muted-foreground">{data.groups.length} groups</span>
                  </div>
                  <div className="rounded-md border">
                    {data.groups.length === 0 ? (
                      <EmptyState
                        icon={CircleDollarSign}
                        title="No usage in this range"
                        description="The controller records a cost event when an instance ingests usage."
                      />
                    ) : (
                      <>
                        <div className="hidden sm:block">
                          <Table>
                            <TableHeader>
                              <TableRow>
                                <TableHead>{data.group_by}</TableHead>
                                <TableHead className="w-[120px]">Tokens</TableHead>
                                <TableHead className="w-[100px]">Events</TableHead>
                                <TableHead className="w-[110px]">Cost</TableHead>
                                <TableHead className="w-[90px]">Share</TableHead>
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
                                  {formatTokens(group.total_tokens)} tokens · {group.events} events
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
                  <h2 className="text-sm font-medium">Top agents</h2>
                  <div className="rounded-md border">
                    {data.top_agents.length === 0 ? (
                      <p className="px-3 py-6 text-center text-sm text-muted-foreground">
                        No agent spent tokens in this range.
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
                                {formatTokens(agent.total_tokens)} tokens
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
                  <h2 className="text-sm font-medium">Budgets</h2>
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
                      Set budget
                    </Button>
                  ) : null}
                </div>
                {budgets.error ? <ErrorAlert error={budgets.error} onRetry={budgets.reload} /> : null}
                <div className="rounded-md border">
                  {budgetRows.length === 0 ? (
                    <EmptyState
                      icon={CircleDollarSign}
                      title="No budget configured"
                      description="A budget emits a cost event once an instance reaches its monthly limit."
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
                            Set budget
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
                              <TableHead>Instance</TableHead>
                              <TableHead className="w-[110px]">Month</TableHead>
                              <TableHead className="w-[110px]">Limit</TableHead>
                              <TableHead className="w-[110px]">Used</TableHead>
                              <TableHead className="w-[160px]">Usage</TableHead>
                              <TableHead className="w-[110px]">Projected</TableHead>
                              <TableHead className="w-[90px]">
                                <span className="sr-only">Actions</span>
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
                                      {budget.enabled ? "enabled" : "disabled"}
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
                                      aria-label={`${budget.instance_id} budget usage`}
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
                                      aria-label={`Edit budget for ${budget.instance_id}`}
                                      title={`Edit budget for ${budget.instance_id}`}
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
                                <dt className="text-muted-foreground">Limit</dt>
                                <dd className="font-mono">{formatCost(budget.monthly_usd)}</dd>
                              </div>
                              <div>
                                <dt className="text-muted-foreground">Used</dt>
                                <dd className="font-mono">{formatCost(budget.used_usd)}</dd>
                              </div>
                              <div>
                                <dt className="text-muted-foreground">Percent</dt>
                                <dd className="font-mono">{formatPercentValue(budget.percent)}</dd>
                              </div>
                              <div>
                                <dt className="text-muted-foreground">Projected</dt>
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
                                Edit budget
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
              Prices per million tokens. Historical reports use the price effective on each event day.
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
                Add price
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
                title="No prices configured"
                description="An unpriced model records tokens but costs zero."
              />
            ) : (
              <>
                <div className="hidden md:block">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Model</TableHead>
                        <TableHead className="w-[130px]">Input / Mtok</TableHead>
                        <TableHead className="w-[130px]">Output / Mtok</TableHead>
                        <TableHead className="w-[100px]">Currency</TableHead>
                        <TableHead className="w-[140px]">Effective from</TableHead>
                        <TableHead className="w-[150px]">Updated</TableHead>
                        <TableHead className="w-[110px]">
                          <span className="sr-only">Actions</span>
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
                            {formatRelative(price.updated_at)}
                          </TableCell>
                          <TableCell>
                            {admin ? (
                              <div className="flex justify-end gap-1">
                                <Button
                                  variant="ghost"
                                  size="icon-sm"
                                  aria-label={`New price version for ${price.model}`}
                                  title={`New price version for ${price.model}`}
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
                                  aria-label={`Delete price for ${price.model}`}
                                  title={`Delete price for ${price.model}`}
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
                          <dt className="text-muted-foreground">Input / Mtok</dt>
                          <dd className="font-mono">{formatCost(price.input_per_mtok, price.currency)}</dd>
                        </div>
                        <div>
                          <dt className="text-muted-foreground">Output / Mtok</dt>
                          <dd className="font-mono">{formatCost(price.output_per_mtok, price.currency)}</dd>
                        </div>
                        <div>
                          <dt className="text-muted-foreground">Effective</dt>
                          <dd className="font-mono">{price.effective_from}</dd>
                        </div>
                        <div>
                          <dt className="text-muted-foreground">Updated</dt>
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
                            New version
                          </Button>
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              setDeleteError(null);
                              setDeleteTarget(price);
                            }}
                          >
                            Delete
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
            Budgets:{" "}
            {budgetRows.length === 0
              ? "none configured"
              : `${budgetRows.length} configured, ${budgetRows.filter((row) => row.enabled).length} enabled`}
            .
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
        title="Delete model price"
        description={
          deleteTarget
            ? `Delete every price version for ${deleteTarget.model}? Reports then value new events at zero.`
            : undefined
        }
        confirmLabel="Delete price"
        destructive
        pending={deleting}
        error={deleteError}
        onConfirm={() => void removePricing()}
      />
    </>
  );
}
