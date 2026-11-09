import { useEffect, useState, type FormEvent } from "react";
import { Fingerprint, Loader2, Sparkles } from "lucide-react";
import { api } from "@/api";
import { apiErrorMessage, toApiError, type ApiError } from "@/api/errors";
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
import { RichText } from "@/components/rich-text";
import { PersonaVariableEditor } from "@/components/persona-vars";
import { useI18n } from "@/lib/i18n";
import { SOUL_SKELETON, SoulHelpButton } from "@/components/soul-help";
import {
  PERSONA_TAG_MAX,
  SOUL_PLACEHOLDER,
  parseTags,
  validateVariableRows,
  variablesFromRows,
  type PersonaVariableRow,
} from "@/lib/personas";
import { isValidSlug, slugify } from "@/lib/validation";

const FIELD_IDS: Record<string, string> = {
  name: "persona-name",
  id: "persona-id",
  tags: "persona-tags",
  soul: "persona-soul",
};

export function PersonaCreateDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (id: string) => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState("");
  const [id, setId] = useState("");
  const [idTouched, setIdTouched] = useState(false);
  const [description, setDescription] = useState("");
  const [tags, setTags] = useState("");
  const [rows, setRows] = useState<PersonaVariableRow[]>([]);
  const [soul, setSoul] = useState("");
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [idConflict, setIdConflict] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setName("");
    setId("");
    setIdTouched(false);
    setDescription("");
    setTags("");
    setRows([]);
    setSoul("");
    setAttempted(false);
    setPending(false);
    setError(null);
    setIdConflict(null);
  }, [open]);

  const trimmedId = id.trim();
  const variableErrors = validateVariableRows(rows, t);
  const parsedTags = parseTags(tags);
  const badTag = parsedTags.find((tag) => tag.length > PERSONA_TAG_MAX);

  const errors: Record<string, string> = {};
  if (!name.trim()) errors.name = t("personas.create.nameRequired");
  else if (name.trim().length > 128) errors.name = t("personas.create.nameTooLong");
  if (!trimmedId) errors.id = t("personas.create.idRequired");
  else if (!isValidSlug(trimmedId)) {
    errors.id = t("personas.create.idFormat");
  } else if (trimmedId === "new") errors.id = t("personas.create.idReserved");
  if (badTag) {
    errors.tags = t("personas.create.tagTooLong", { tag: badTag, max: PERSONA_TAG_MAX });
  }
  if (!soul.trim()) errors.soul = t("personas.create.soulRequired");
  const variablesInvalid = Object.keys(variableErrors).length > 0;

  const idError = idConflict ?? (attempted ? errors.id : undefined);

  function focusField(key: string) {
    document.getElementById(FIELD_IDS[key] ?? key)?.focus();
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setAttempted(true);
    const first = ["name", "id", "tags", "soul"].find((key) => errors[key]);
    if (first) {
      focusField(first);
      return;
    }
    if (variablesInvalid) {
      document.getElementById("persona-variables")?.scrollIntoView({ block: "center" });
      return;
    }

    setPending(true);
    setError(null);
    setIdConflict(null);
    try {
      const created = await api.createPersona({
        id: trimmedId,
        name: name.trim(),
        description: description.trim(),
        tags: parsedTags,
        variables: variablesFromRows(rows),
        soul_md: soul,
      });
      onOpenChange(false);
      onCreated(created.id);
    } catch (cause) {
      const failure = toApiError(cause);
      if (failure.status === 409 && failure.code === "persona_exists") {
        setIdConflict(apiErrorMessage(failure, t));
        window.requestAnimationFrame(() => focusField("id"));
      } else {
        setError(failure);
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={pending ? undefined : onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh_-_2rem)] grid-rows-[auto_minmax(0,1fr)] sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("personas.create.title")}</DialogTitle>
          <DialogDescription>{t("personas.create.description")}</DialogDescription>
        </DialogHeader>

        <form onSubmit={onSubmit} noValidate className="flex min-h-0 flex-col gap-4">
          <div className="-mr-2 min-h-0 flex-1 space-y-4 overflow-y-auto pr-2">
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="persona-name">{t("common.name")}</Label>
                <Input
                  id="persona-name"
                  value={name}
                  autoComplete="off"
                  placeholder={t("personas.create.namePlaceholder")}
                  aria-invalid={attempted && !!errors.name}
                  aria-describedby={attempted && errors.name ? "persona-name-error" : undefined}
                  onChange={(event) => {
                    setName(event.target.value);
                    if (!idTouched) setId(slugify(event.target.value));
                  }}
                />
                {attempted && errors.name ? (
                  <p id="persona-name-error" className="text-xs text-destructive">
                    {errors.name}
                  </p>
                ) : null}
              </div>

              <div className="space-y-2">
                <Label htmlFor="persona-id">{t("personas.create.id")}</Label>
                <Input
                  id="persona-id"
                  value={id}
                  autoComplete="off"
                  spellCheck={false}
                  className="font-mono"
                  placeholder={t("personas.create.idPlaceholder")}
                  aria-invalid={!!idError}
                  aria-describedby={idError ? "persona-id-error" : "persona-id-help"}
                  onChange={(event) => {
                    setId(event.target.value.toLowerCase());
                    setIdTouched(true);
                    setIdConflict(null);
                  }}
                />
                <p id="persona-id-help" className="text-xs text-muted-foreground">
                  <RichText
                    text={t("personas.create.idHint", { id: trimmedId || "…" })}
                  />
                </p>
                {idError ? (
                  <p id="persona-id-error" className="text-xs text-destructive">
                    {idError}
                  </p>
                ) : null}
              </div>
            </div>

            <div className="space-y-2">
              <Label htmlFor="persona-description">{t("common.description")}</Label>
              <Textarea
                id="persona-description"
                rows={2}
                value={description}
                placeholder={t("personas.create.descriptionPlaceholder")}
                onChange={(event) => setDescription(event.target.value)}
              />
            </div>

            <div className="space-y-2">
              <Label htmlFor="persona-tags">
                {t("personas.create.tags")}{" "}
                <span className="font-normal text-muted-foreground">({t("common.optional")})</span>
              </Label>
              <Input
                id="persona-tags"
                value={tags}
                autoComplete="off"
                spellCheck={false}
                className="font-mono text-xs"
                placeholder={t("personas.create.tagsPlaceholder")}
                aria-invalid={attempted && !!errors.tags}
                aria-describedby={attempted && errors.tags ? "persona-tags-error" : "persona-tags-help"}
                onChange={(event) => setTags(event.target.value)}
              />
              <p id="persona-tags-help" className="text-xs text-muted-foreground">
                {t("personas.create.tagsHint")}
              </p>
              {attempted && errors.tags ? (
                <p id="persona-tags-error" className="text-xs text-destructive">
                  {errors.tags}
                </p>
              ) : null}
            </div>

            <div id="persona-variables" className="scroll-mt-24">
              <PersonaVariableEditor
                rows={rows}
                onChange={setRows}
                errors={attempted ? variableErrors : {}}
                idPrefix="persona-new-var"
                disabled={pending}
              />
              {attempted && variablesInvalid ? (
                <p className="mt-1 text-xs text-destructive">
                  {t("personas.create.fixVariables")}
                </p>
              ) : null}
            </div>

            <div className="space-y-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <Label htmlFor="persona-soul">{t("personas.create.soul")}</Label>
                <div className="flex flex-wrap items-center gap-1">
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => setSoul(SOUL_SKELETON)}
                    disabled={pending}
                  >
                    <Sparkles aria-hidden />
                    {t("personas.create.insertSkeleton")}
                  </Button>
                  <SoulHelpButton />
                </div>
              </div>
              <Textarea
                id="persona-soul"
                rows={14}
                value={soul}
                spellCheck={false}
                className="min-h-64 font-mono text-xs leading-relaxed"
                placeholder={SOUL_PLACEHOLDER}
                aria-invalid={attempted && !!errors.soul}
                aria-describedby={attempted && errors.soul ? "persona-soul-error" : "persona-soul-help"}
                onChange={(event) => setSoul(event.target.value)}
              />
              <div className="flex items-center justify-between gap-2">
                <p id="persona-soul-help" className="text-xs text-muted-foreground">
                  <RichText text={t("personas.create.soulHint")} />
                </p>
                <span className="font-mono text-xs text-muted-foreground">
                  {t("personas.create.characters", { count: soul.length })}
                </span>
              </div>
              {attempted && errors.soul ? (
                <p id="persona-soul-error" className="text-xs text-destructive">
                  {errors.soul}
                </p>
              ) : null}
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
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? <Loader2 aria-hidden className="animate-spin" /> : <Fingerprint aria-hidden />}
              {t("personas.create.confirm")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
