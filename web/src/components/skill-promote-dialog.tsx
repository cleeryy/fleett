import { useEffect, useState } from "react";
import { CheckCircle2, Loader2, RotateCw, TriangleAlert, XCircle } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { InstanceSummary, SkillDetail, SkillPromoteResult } from "@/api/types";
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
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { ErrorAlert } from "@/components/error-alert";
import { plural } from "@/lib/format";
import { hrefFor } from "@/lib/router";

function followerOf(instance: InstanceSummary, name: string): boolean {
  return (instance.skills ?? []).some((skill) => skill.name === name && !skill.version);
}

function pinnedOf(instance: InstanceSummary, name: string): boolean {
  return (instance.skills ?? []).some((skill) => skill.name === name && !!skill.version);
}

/**
 * Move the `latest` pointer. The controller restarts each instance that follows
 * latest, one at a time, and answers with one row per instance; pinned
 * instances are left alone. The report stays on screen so a failed restart can
 * be read, not just counted.
 */
export function SkillPromoteDialog({
  skill,
  open,
  onOpenChange,
  onPromoted,
}: {
  skill: SkillDetail;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onPromoted: (latest: string) => void;
}) {
  const [version, setVersion] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [result, setResult] = useState<SkillPromoteResult | null>(null);
  const [instances, setInstances] = useState<InstanceSummary[] | null>(null);
  const [instancesError, setInstancesError] = useState<ApiError | null>(null);
  const [reloadKey, setReloadKey] = useState(0);

  useEffect(() => {
    if (!open) return;
    // Default to the newest version that is not already latest (a promote can
    // also roll back), so the form is actionable for a two-version skill.
    const newestFirst = skill.versions.map((entry) => entry.version).reverse();
    setVersion(newestFirst.find((candidate) => candidate !== skill.latest) ?? newestFirst[0] ?? "");
    setPending(false);
    setError(null);
    setResult(null);
  }, [open, skill.latest, skill.versions]);

  useEffect(() => {
    if (!open) return;
    let alive = true;
    setInstances(null);
    setInstancesError(null);
    api.listInstances().then(
      (list) => {
        if (alive) setInstances(list);
      },
      (cause: unknown) => {
        if (alive) setInstancesError(toApiError(cause));
      },
    );
    return () => {
      alive = false;
    };
  }, [open, reloadKey]);

  const followers = (instances ?? []).filter((instance) => followerOf(instance, skill.name));
  const pinned = (instances ?? []).filter((instance) => pinnedOf(instance, skill.name));
  const alreadyLatest = !!version && version === skill.latest;
  const options = [...skill.versions].reverse();

  async function confirm() {
    if (!version) return;
    setPending(true);
    setError(null);
    try {
      const response = await api.promoteSkill(skill.name, version);
      setResult(response);
      const failed = response.results.filter((entry) => entry.status === "failed").length;
      const restarted = response.results.length - failed;
      if (failed === 0) {
        toast.success(
          `latest is ${response.latest}${restarted ? ` · ${restarted} restarted` : ""}.`,
        );
      } else {
        toast.error(`${failed} of ${response.results.length} restarts failed.`);
      }
      onPromoted(response.latest);
    } catch (cause) {
      setError(toApiError(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Promote {skill.name}</DialogTitle>
          <DialogDescription>
            Moves <span className="font-mono">latest</span> to the chosen version, then restarts
            every instance that follows it, one at a time. Pinned instances are not touched.
          </DialogDescription>
        </DialogHeader>

        {result ? (
          <div className="space-y-3">
            <p className="flex flex-wrap items-center gap-2 text-sm">
              <CheckCircle2 aria-hidden className="size-4 text-emerald-600 dark:text-emerald-400" />
              <span>
                <span className="font-mono">latest</span> is now{" "}
                <span className="font-mono font-medium">{result.latest}</span>
              </span>
              <Badge variant="outline" className="font-mono text-[11px] font-normal">
                {result.results.filter((entry) => entry.status !== "failed").length} restarted
                {result.results.some((entry) => entry.status === "failed")
                  ? ` · ${result.results.filter((entry) => entry.status === "failed").length} failed`
                  : ""}
              </Badge>
            </p>

            {result.results.length === 0 ? (
              <p className="rounded-md border px-3 py-2.5 text-sm text-muted-foreground">
                No instance follows <span className="font-mono">latest</span>: nothing was
                restarted. Pinned instances keep their version until you change the pin.
              </p>
            ) : (
              <ul className="divide-y rounded-md border">
                {result.results.map((entry) => {
                  const failed = entry.status === "failed";
                  return (
                    <li key={entry.id} className="space-y-1 px-3 py-2.5">
                      <div className="flex flex-wrap items-center gap-2">
                        <a
                          href={hrefFor(`/instances/${entry.id}`)}
                          className="min-w-0 flex-1 truncate text-sm font-medium underline-offset-4 hover:underline"
                        >
                          {entry.id}
                        </a>
                        <Badge
                          variant={failed ? "destructive" : "secondary"}
                          className="shrink-0 gap-1 font-mono text-[11px] font-normal"
                        >
                          {failed ? <XCircle aria-hidden /> : <CheckCircle2 aria-hidden />}
                          {entry.status}
                        </Badge>
                      </div>
                      {entry.error ? (
                        <p className="font-mono text-xs break-words text-destructive">
                          {entry.error}
                        </p>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
            )}

            {pinned.length > 0 ? (
              <p className="text-xs text-muted-foreground">
                {pinned.length} {plural(pinned.length, "instance")} pinned to a version and left
                untouched.
              </p>
            ) : null}
          </div>
        ) : (
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="skill-promote-version">Version to promote</Label>
              <Select value={version} onValueChange={setVersion} disabled={pending}>
                <SelectTrigger id="skill-promote-version" className="w-full font-mono">
                  <SelectValue placeholder="Pick a version" />
                </SelectTrigger>
                <SelectContent>
                  {options.map((entry) => (
                    <SelectItem key={entry.version} value={entry.version}>
                      {entry.version}
                      {entry.version === skill.latest ? " (current latest)" : ""}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {alreadyLatest ? (
                <p className="flex items-start gap-1.5 text-xs text-amber-700 dark:text-amber-400">
                  <TriangleAlert aria-hidden className="mt-0.5 size-3.5 shrink-0" />
                  This version is already latest: promoting it restarts the followers again without
                  changing what they mount.
                </p>
              ) : null}
            </div>

            <div className="space-y-2">
              <p className="text-sm font-medium">
                {instances === null
                  ? "Loading instances…"
                  : `${followers.length} ${plural(followers.length, "instance")} ${
                      followers.length === 1 ? "follows" : "follow"
                    } latest and will restart`}
              </p>
              {instancesError ? (
                <ErrorAlert error={instancesError} onRetry={() => setReloadKey((value) => value + 1)} />
              ) : instances === null ? (
                <Skeleton className="h-16 w-full rounded-md" />
              ) : followers.length === 0 ? (
                <p className="rounded-md border px-3 py-2.5 text-sm text-muted-foreground">
                  No instance follows latest. The pointer moves; nothing restarts.
                </p>
              ) : (
                <ul className="max-h-48 divide-y overflow-auto rounded-md border">
                  {followers.map((instance) => (
                    <li key={instance.id} className="flex items-center gap-2 px-3 py-2">
                      <a
                        href={hrefFor(`/instances/${instance.id}`)}
                        className="min-w-0 flex-1 truncate text-sm underline-offset-4 hover:underline"
                      >
                        {instance.name}
                      </a>
                      <span className="shrink-0 font-mono text-xs text-muted-foreground">
                        {instance.id}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
              {pinned.length > 0 ? (
                <p className="text-xs text-muted-foreground">
                  {pinned.length} {plural(pinned.length, "instance")} pinned:{" "}
                  {pinned
                    .map(
                      (instance) =>
                        `${instance.id}@${
                          (instance.skills ?? []).find((entry) => entry.name === skill.name)
                            ?.version ?? "?"
                        }`,
                    )
                    .join(", ")}
                  .
                </p>
              ) : null}
            </div>

            {error ? <ErrorAlert error={error} /> : null}
          </div>
        )}

        <DialogFooter>
          {result ? (
            <Button onClick={() => onOpenChange(false)}>Close</Button>
          ) : (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
                Cancel
              </Button>
              <Button
                onClick={() => void confirm()}
                disabled={pending || !version}
              >
                {pending ? <Loader2 aria-hidden className="animate-spin" /> : <RotateCw aria-hidden />}
                Promote and restart {followers.length}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
