import { useEffect, useRef, useState, type FormEvent } from "react";
import { FilePlus2, Loader2, Plus, Sparkles, Trash2 } from "lucide-react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { PublishSkillInput, SkillDetail, SkillFile, SkillSummary } from "@/api/types";
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
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { ErrorAlert } from "@/components/error-alert";
import {
  MAX_SKILL_DESCRIPTION,
  MAX_SKILL_FILES,
  MAX_SKILL_FILE_BYTES,
  MAX_SKILL_VERSION_BYTES,
  SKILL_FILE,
  SKILL_SKELETON,
  byteLength,
  isValidSkillName,
  isValidSkillVersion,
  newSkillFileRow,
  nextSkillVersion,
  skillPathError,
  type SkillFileRow,
} from "@/lib/skills";

const FIELD_IDS: Record<string, string> = {
  name: "skill-name",
  version: "skill-version",
  description: "skill-description",
  skill: "skill-md",
};

let rowSeq = 0;
function nextRowId(): string {
  rowSeq += 1;
  return `skill-file-${rowSeq}`;
}

/**
 * Publish one version of a skill: a slug, a version (`vN`), a description and
 * the file list, with `SKILL.md` as the main editor. Publishing never
 * overwrites, so an existing version is reported on the version field.
 */
export function SkillCreateDialog({
  open,
  onOpenChange,
  preset,
  knownSkills = [],
  onPublished,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Fixed name and suggested version, when publishing from a detail page. */
  preset?: { name: string; version: string };
  /** Registry entries, so a new version of an existing skill suggests the next one. */
  knownSkills?: SkillSummary[];
  onPublished: (detail: SkillDetail) => void;
}) {
  const [name, setName] = useState("");
  const [version, setVersion] = useState("");
  const [description, setDescription] = useState("");
  const [skillMd, setSkillMd] = useState("");
  const [rows, setRows] = useState<SkillFileRow[]>([]);
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [versionConflict, setVersionConflict] = useState<string | null>(null);
  const versionTouched = useRef(false);

  useEffect(() => {
    if (!open) return;
    setName(preset?.name ?? "");
    setVersion(preset?.version ?? "v1");
    setDescription("");
    setSkillMd("");
    setRows([]);
    setAttempted(false);
    setPending(false);
    setError(null);
    setVersionConflict(null);
    versionTouched.current = !!preset;
  }, [open, preset]);

  const trimmedName = name.trim();
  const trimmedVersion = version.trim();
  const existing = knownSkills.find((skill) => skill.name === trimmedName) ?? null;

  const pathErrors: Record<string, string | null> = {};
  const seenPaths = new Set<string>([SKILL_FILE]);
  let totalBytes = byteLength(skillMd);
  for (const row of rows) {
    const path = row.path.trim();
    let message = path ? skillPathError(path) : null;
    if (message) {
      // keep the message
    } else if (path === SKILL_FILE) {
      message = `${SKILL_FILE} is the main editor above.`;
    } else if (seenPaths.has(path)) {
      message = "This path is already listed.";
    }
    pathErrors[row.id] = message;
    if (path && !message) seenPaths.add(path);
    totalBytes += byteLength(row.content);
  }

  const errors: Record<string, string> = {};
  if (!trimmedName) errors.name = "Enter a name.";
  else if (!isValidSkillName(trimmedName)) {
    errors.name = "Use 2-32 characters: lowercase letters, digits, hyphens.";
  }
  if (!trimmedVersion) errors.version = "Enter a version, for example v1.";
  else if (!isValidSkillVersion(trimmedVersion)) errors.version = "Use v1, v2, v3…";
  if (description.trim().length > MAX_SKILL_DESCRIPTION) {
    errors.description = `Use ${MAX_SKILL_DESCRIPTION} characters or fewer.`;
  }
  if (!skillMd.trim()) errors.skill = `Write ${SKILL_FILE}. It must not be empty.`;
  else if (byteLength(skillMd) > MAX_SKILL_FILE_BYTES) {
    errors.skill = `A file is at most ${Math.round(MAX_SKILL_FILE_BYTES / 1024)} KiB.`;
  }
  const rowInvalid = Object.values(pathErrors).some((message) => !!message);
  if (rows.some((row) => byteLength(row.content) > MAX_SKILL_FILE_BYTES)) {
    errors.files = `A file is at most ${Math.round(MAX_SKILL_FILE_BYTES / 1024)} KiB.`;
  }
  if (totalBytes > MAX_SKILL_VERSION_BYTES) {
    errors.files = `A version is at most ${Math.round(MAX_SKILL_VERSION_BYTES / 1024)} KiB in total.`;
  }
  if (rows.length + 1 > MAX_SKILL_FILES) {
    errors.files = `A version holds at most ${MAX_SKILL_FILES} files.`;
  }
  const showErrors = attempted;

  function focusField(key: string) {
    const node = document.getElementById(FIELD_IDS[key] ?? key);
    node?.focus();
    node?.scrollIntoView({ block: "center" });
  }

  function updateRow(id: string, changes: Partial<SkillFileRow>) {
    setRows((current) =>
      current.map((row) => (row.id === id ? { ...row, ...changes } : row)),
    );
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setAttempted(true);
    const first = ["name", "version", "description", "skill"].find((key) => errors[key]);
    if (first) {
      focusField(first);
      return;
    }
    if (errors.files || rowInvalid) {
      document.getElementById("skill-files")?.scrollIntoView({ block: "center" });
      return;
    }

    const files: SkillFile[] = [
      { path: SKILL_FILE, content: skillMd },
      ...rows.map((row) => ({ path: row.path.trim(), content: row.content })),
    ];
    const input: PublishSkillInput = {
      name: trimmedName,
      version: trimmedVersion,
      description: description.trim(),
      files,
    };

    setPending(true);
    setError(null);
    setVersionConflict(null);
    try {
      const detail = await api.publishSkill(input);
      onOpenChange(false);
      onPublished(detail);
    } catch (cause) {
      const failure = toApiError(cause);
      if (failure.status === 409 && failure.code === "skill_version_exists") {
        setVersionConflict(failure.message);
        window.requestAnimationFrame(() => focusField("version"));
      } else {
        setError(failure);
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="grid max-h-[calc(100dvh_-_2rem)] grid-rows-[auto_minmax(0,1fr)] sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{preset ? `Publish a new version of ${preset.name}` : "Publish a skill"}</DialogTitle>
          <DialogDescription>
            One version is a tree of text files an agent reads at{" "}
            <span className="font-mono">/opt/data/skills/&lt;name&gt;</span>. Publishing never
            overwrites an existing version.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={onSubmit} noValidate className="flex min-h-0 flex-col gap-4">
          <div className="-mr-2 min-h-0 flex-1 space-y-4 overflow-y-auto pr-2">
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="skill-name">Name</Label>
                <Input
                  id="skill-name"
                  value={name}
                  autoComplete="off"
                  spellCheck={false}
                  readOnly={!!preset}
                  className="font-mono"
                  placeholder="homelab"
                  aria-invalid={showErrors && !!errors.name}
                  aria-describedby={
                    (showErrors && errors.name) || existing ? "skill-name-help" : undefined
                  }
                  onChange={(event) => {
                    const next = event.target.value.toLowerCase();
                    setName(next);
                    if (versionTouched.current) return;
                    const match = knownSkills.find((skill) => skill.name === next.trim());
                    if (match) setVersion(nextSkillVersion(match.versions));
                  }}
                />
                {existing || (showErrors && errors.name) ? (
                  <p
                    id="skill-name-help"
                    className={
                      showErrors && errors.name ? "text-xs text-destructive" : "text-xs text-muted-foreground"
                    }
                  >
                    {showErrors && errors.name
                      ? errors.name
                      : existing
                        ? `${existing.versions.length} version(s) published; latest is ${existing.latest ?? "unset"}.`
                        : ""}
                  </p>
                ) : null}
              </div>

              <div className="space-y-2">
                <Label htmlFor="skill-version">Version</Label>
                <Input
                  id="skill-version"
                  value={version}
                  autoComplete="off"
                  spellCheck={false}
                  readOnly={!!preset}
                  className="font-mono"
                  placeholder="v1"
                  aria-invalid={(showErrors && !!errors.version) || !!versionConflict}
                  aria-describedby="skill-version-help"
                  onChange={(event) => {
                    versionTouched.current = true;
                    setVersion(event.target.value.trim());
                    setVersionConflict(null);
                  }}
                />
                <p
                  id="skill-version-help"
                  className={versionConflict ? "text-xs text-destructive" : "text-xs text-muted-foreground"}
                >
                  {versionConflict ??
                    (preset
                      ? "The next free version; importing the same archive twice never conflicts."
                      : "v1, v2, v3… A version that already exists is refused.")}
                </p>
              </div>
            </div>

            <div className="space-y-2">
              <Label htmlFor="skill-description">
                Description <span className="font-normal text-muted-foreground">(optional)</span>
              </Label>
              <Textarea
                id="skill-description"
                rows={2}
                value={description}
                placeholder="What the skill covers, and when an agent should read it."
                aria-invalid={showErrors && !!errors.description}
                aria-describedby={
                  showErrors && errors.description ? "skill-description-error" : "skill-description-help"
                }
                onChange={(event) => setDescription(event.target.value)}
              />
              <p id="skill-description-help" className="text-xs text-muted-foreground">
                Shown in the registry list. Stored in the generated{" "}
                <span className="font-mono">skill.yaml</span>.
              </p>
              {showErrors && errors.description ? (
                <p id="skill-description-error" className="text-xs text-destructive">
                  {errors.description}
                </p>
              ) : null}
            </div>

            <div className="space-y-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <Label htmlFor="skill-md">{SKILL_FILE}</Label>
                <div className="flex items-center gap-1">
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    disabled={pending}
                    onClick={() => setSkillMd(SKILL_SKELETON)}
                  >
                    <Sparkles aria-hidden />
                    Insert a skeleton
                  </Button>
                  <Badge variant="secondary" className="font-mono text-[11px] font-normal">
                    required
                  </Badge>
                </div>
              </div>
              <Textarea
                id="skill-md"
                rows={12}
                value={skillMd}
                spellCheck={false}
                className="min-h-56 font-mono text-xs leading-relaxed"
                placeholder={"# Skill name\n\nUse this skill when…"}
                aria-invalid={showErrors && !!errors.skill}
                aria-describedby={showErrors && errors.skill ? "skill-md-error" : "skill-md-help"}
                onChange={(event) => setSkillMd(event.target.value)}
              />
              <div className="flex flex-wrap items-center justify-between gap-2">
                <p id="skill-md-help" className="text-xs text-muted-foreground">
                  Markdown read by the agent. Every other file is optional.
                </p>
                <span className="font-mono text-xs text-muted-foreground">
                  {byteLength(skillMd)} bytes
                </span>
              </div>
              {showErrors && errors.skill ? (
                <p id="skill-md-error" className="text-xs text-destructive">
                  {errors.skill}
                </p>
              ) : null}
            </div>

            <div id="skill-files" className="scroll-mt-24 space-y-3">
              <div className="flex items-center justify-between gap-2">
                <div>
                  <p className="text-sm font-medium">Other files</p>
                  <p className="text-xs text-muted-foreground">
                    Notes the skill references. Paths are relative to the version directory.
                  </p>
                </div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={pending || rows.length + 1 >= MAX_SKILL_FILES}
                  onClick={() => setRows((current) => [...current, newSkillFileRow(nextRowId())])}
                >
                  <Plus aria-hidden />
                  Add file
                </Button>
              </div>

              {rows.length === 0 ? (
                <p className="rounded-md border px-3 py-2.5 text-xs text-muted-foreground">
                  No extra files. One <span className="font-mono">SKILL.md</span> is a valid
                  version.
                </p>
              ) : (
                <ul className="space-y-3">
                  {rows.map((row) => {
                    const message = pathErrors[row.id];
                    return (
                      <li key={row.id} className="space-y-2 rounded-md border p-3">
                        <div className="flex items-center gap-2">
                          <FilePlus2 aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                          <Input
                            id={`${row.id}-path`}
                            value={row.path}
                            autoComplete="off"
                            spellCheck={false}
                            className="font-mono text-xs"
                            placeholder="notes/proxmox.md"
                            aria-label="File path"
                            aria-invalid={showErrors && !!message}
                            onChange={(event) => updateRow(row.id, { path: event.target.value })}
                          />
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon-sm"
                            aria-label="Remove file"
                            title="Remove file"
                            disabled={pending}
                            onClick={() =>
                              setRows((current) => current.filter((entry) => entry.id !== row.id))
                            }
                          >
                            <Trash2 aria-hidden />
                          </Button>
                        </div>
                        <Textarea
                          id={`${row.id}-content`}
                          rows={5}
                          value={row.content}
                          spellCheck={false}
                          className="font-mono text-xs leading-relaxed"
                          aria-label={`Content of ${row.path || "the new file"}`}
                          onChange={(event) => updateRow(row.id, { content: event.target.value })}
                        />
                        {showErrors && message ? (
                          <p className="text-xs text-destructive">{message}</p>
                        ) : null}
                      </li>
                    );
                  })}
                </ul>
              )}

              {errors.files ? <p className="text-xs text-destructive">{errors.files}</p> : null}
            </div>

            {error ? <ErrorAlert error={error} /> : null}
          </div>

          <DialogFooter className="border-t pt-4">
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={pending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
              Publish {trimmedVersion || "version"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
