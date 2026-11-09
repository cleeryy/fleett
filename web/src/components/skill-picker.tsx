import type { ApiError } from "@/api/errors";
import type { SkillRef, SkillSummary } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { ErrorAlert } from "@/components/error-alert";
import { formatRelative, plural } from "@/lib/format";
import { hrefFor } from "@/lib/router";
import {
  FOLLOW_LATEST,
  MAX_SKILLS_PER_INSTANCE,
  effectiveVersion,
  tooManySkills,
} from "@/lib/skills";
import { cn } from "@/lib/utils";

/**
 * Multi-select of registry skills with a version pin per entry. A row without
 * a pin follows `latest`; a pinned row keeps that version across promotes.
 * Used by the create wizard and the Configuration tab, so both send the same
 * `skills: [{name, version?}]` list.
 */
export function SkillPicker({
  skills,
  value,
  onChange,
  loading = false,
  error,
  onRetry,
  disabled = false,
  idPrefix = "skill",
  effective,
  className,
}: {
  skills: SkillSummary[];
  value: SkillRef[];
  onChange: (next: SkillRef[]) => void;
  loading?: boolean;
  error?: ApiError | null;
  onRetry?: () => void;
  disabled?: boolean;
  idPrefix?: string;
  /** Versions the controller reports as mounted, by skill name. */
  effective?: Record<string, string | null>;
  className?: string;
}) {
  const over = tooManySkills(value);

  function toggle(name: string, checked: boolean) {
    onChange(
      checked
        ? [...value, { name, version: null }]
        : value.filter((ref) => ref.name !== name),
    );
  }

  function setPin(name: string, pin: string) {
    onChange(
      value.map((ref) =>
        ref.name === name ? { ...ref, version: pin === FOLLOW_LATEST ? null : pin } : ref,
      ),
    );
  }

  return (
    <div className={cn("space-y-3", className)}>
      <p className="text-sm text-muted-foreground">
        Skills are mounted read-only at{" "}
        <span className="font-mono">/opt/data/skills/&lt;name&gt;</span>. A pin keeps one
        version; leaving <span className="font-mono">latest</span> means the instance follows the
        registry pointer.
      </p>

      {error ? <ErrorAlert error={error} onRetry={onRetry} /> : null}

      {loading ? (
        <div className="space-y-2">
          {Array.from({ length: 3 }).map((_, index) => (
            <Skeleton key={index} className="h-16 w-full rounded-md" />
          ))}
        </div>
      ) : skills.length === 0 ? (
        <p className="rounded-md border px-4 py-6 text-sm text-muted-foreground">
          The registry is empty.{" "}
          <a
            href={hrefFor("/skills")}
            className="underline underline-offset-4 hover:text-foreground"
          >
            Publish a skill
          </a>{" "}
          first, then mount it here.
        </p>
      ) : (
        <div className="divide-y rounded-md border">
          {skills.map((skill) => {
            const selected = value.find((ref) => ref.name === skill.name);
            const pin = selected?.version ?? null;
            const mounted = selected
              ? effective && Object.hasOwn(effective, skill.name)
                ? effective[skill.name]
                : effectiveVersion(selected, skill.versions, skill.latest)
              : null;
            const stale = !!pin && !skill.versions.includes(pin);
            return (
              <div
                key={skill.name}
                className="grid grid-cols-[auto_minmax(0,1fr)] items-start gap-x-3 gap-y-2 px-3 py-2.5 sm:flex sm:flex-wrap sm:items-center"
              >
                <Checkbox
                  id={`${idPrefix}-${skill.name}`}
                  className="mt-0.5 sm:mt-0"
                  checked={!!selected}
                  disabled={disabled || (!selected && value.length >= MAX_SKILLS_PER_INSTANCE)}
                  aria-describedby={`${idPrefix}-${skill.name}-help`}
                  onCheckedChange={(checked) => toggle(skill.name, checked === true)}
                />
                <label
                  htmlFor={`${idPrefix}-${skill.name}`}
                  className="min-w-0 cursor-pointer select-none sm:flex-1"
                >
                  <span className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-sm font-medium">{skill.name}</span>
                    <Badge variant="secondary" className="font-mono text-[11px] font-normal">
                      {skill.latest ? `latest ${skill.latest}` : "no latest"}
                    </Badge>
                  </span>
                  <span
                    id={`${idPrefix}-${skill.name}-help`}
                    className="mt-0.5 block truncate text-xs font-normal text-muted-foreground"
                  >
                    {skill.description || "No description."} ·{" "}
                    {skill.versions.length} {plural(skill.versions.length, "version")} ·{" "}
                    {skill.instances_count} {plural(skill.instances_count, "instance")}
                  </span>
                </label>

                <div className="col-start-2 flex flex-wrap items-center gap-2 sm:contents">
                <Select
                  value={pin ?? FOLLOW_LATEST}
                  disabled={disabled || !selected}
                  onValueChange={(next) => setPin(skill.name, next)}
                >
                  <SelectTrigger
                    className="w-40"
                    size="sm"
                    aria-label={`Version of ${skill.name}`}
                    aria-invalid={stale}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={FOLLOW_LATEST}>
                      latest{skill.latest ? ` (${skill.latest})` : ""}
                    </SelectItem>
                    {skill.versions.map((version) => (
                      <SelectItem key={version} value={version}>
                        {version}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>

                {selected ? (
                  <Badge
                    variant={stale || mounted === null ? "destructive" : "outline"}
                    className="shrink-0 gap-1 font-mono text-[11px] font-normal"
                    title={
                      stale
                        ? `${pin} is not published any more`
                        : mounted === null
                          ? "the reference cannot be resolved"
                          : `mounted version: ${mounted}`
                    }
                  >
                    {mounted === null ? "unresolved" : `mounted ${mounted}`}
                  </Badge>
                ) : (
                  <span className="text-[11px] text-muted-foreground sm:w-24 sm:shrink-0 sm:text-right">
                    updated {formatRelative(skill.updated_at)}
                  </span>
                )}
                </div>
              </div>
            );
          })}
        </div>
      )}

      <div className="space-y-1">
        <p className="text-xs text-muted-foreground">
          {value.length} of {MAX_SKILLS_PER_INSTANCE} skills selected
          {value.filter((ref) => !ref.version).length > 0
            ? ` · ${value.filter((ref) => !ref.version).length} following latest`
            : ""}
          .
        </p>
        <p className="text-xs text-muted-foreground">
          Applying the list rewrites <span className="font-mono">compose.yaml</span> and restarts the
          instance. A promote restarts only the instances that follow{" "}
          <span className="font-mono">latest</span>; pinned ones keep their version.
        </p>
        {over ? (
          <p className="text-xs text-destructive">
            An instance mounts at most {MAX_SKILLS_PER_INSTANCE} skills: remove{" "}
            {value.length - MAX_SKILLS_PER_INSTANCE} before saving.
          </p>
        ) : null}
      </div>
    </div>
  );
}
