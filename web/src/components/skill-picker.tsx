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
import { RichText } from "@/components/rich-text";
import { formatRelative } from "@/lib/format";
import { useI18n } from "@/lib/i18n";
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
  const { t, lang } = useI18n();
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
        <RichText text={t("skills.picker.hint")} />
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
          {t("skills.picker.emptyLead")}{" "}
          <a
            href={hrefFor("/skills")}
            className="underline underline-offset-4 hover:text-foreground"
          >
            {t("skills.picker.emptyAction")}
          </a>{" "}
          {t("skills.picker.emptyTail")}
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
                      {skill.latest
                        ? t("skills.picker.latestBadge", { version: skill.latest })
                        : t("skills.picker.noLatest")}
                    </Badge>
                  </span>
                  <span
                    id={`${idPrefix}-${skill.name}-help`}
                    className="mt-0.5 block truncate text-xs font-normal text-muted-foreground"
                  >
                    {skill.description || t("skills.picker.noDescription")} ·{" "}
                    {t(
                      skill.versions.length === 1
                        ? "skills.picker.versionOne"
                        : "skills.picker.versionMany",
                      { count: skill.versions.length },
                    )}{" "}
                    ·{" "}
                    {t(
                      skill.instances_count === 1
                        ? "skills.picker.instanceOne"
                        : "skills.picker.instanceMany",
                      { count: skill.instances_count },
                    )}
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
                    aria-label={t("skills.picker.versionOf", { name: skill.name })}
                    aria-invalid={stale}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={FOLLOW_LATEST}>
                      {skill.latest
                        ? t("skills.picker.latestWith", { version: skill.latest })
                        : t("skills.picker.latest")}
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
                        ? t("skills.picker.staleTitle", { version: pin })
                        : mounted === null
                          ? t("skills.picker.unresolvedTitle")
                          : t("skills.picker.mountedTitle", { version: mounted })
                    }
                  >
                    {mounted === null
                      ? t("skills.picker.unresolved")
                      : t("skills.picker.mounted", { version: mounted })}
                  </Badge>
                ) : (
                  <span className="text-[11px] text-muted-foreground sm:w-24 sm:shrink-0 sm:text-right">
                    {t("skills.picker.updated", { time: formatRelative(skill.updated_at, lang) })}
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
          {t(
            value.length === 1
              ? "skills.picker.selectedCountOne"
              : "skills.picker.selectedCountMany",
            { count: value.length, max: MAX_SKILLS_PER_INSTANCE },
          )}
          {value.filter((ref) => !ref.version).length > 0
            ? t("skills.picker.followingLatest", {
                count: value.filter((ref) => !ref.version).length,
              })
            : ""}
          .
        </p>
        <p className="text-xs text-muted-foreground">
          <RichText text={t("skills.picker.applyHint")} />
        </p>
        {over ? (
          <p className="text-xs text-destructive">
            {t("skills.picker.tooMany", {
              max: MAX_SKILLS_PER_INSTANCE,
              count: value.length - MAX_SKILLS_PER_INSTANCE,
            })}
          </p>
        ) : null}
      </div>
    </div>
  );
}
