import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent as ReactDragEvent,
  type KeyboardEvent,
} from "react";
import {
  ArrowUp,
  Download,
  FileDiff,
  FilePlus2,
  FileText,
  Folder,
  FolderDown,
  FolderPlus,
  Info,
  Loader2,
  Lock,
  RefreshCw,
  Regex,
  RotateCw,
  Save,
  ScanSearch,
  Search,
  Trash2,
  TriangleAlert,
  Upload,
  X,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";
import type { FileEntry, FileSearchHit, FileSearchResult } from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { EmptyState } from "@/components/empty-state";
import { ErrorAlert } from "@/components/error-alert";
import { LineDiff } from "@/components/diff-view";
import { FileUploadsList } from "@/components/file-uploads-list";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { MarkdownView } from "@/components/markdown-view";
import { Segmented } from "@/components/segmented";
import { Skeleton } from "@/components/ui/skeleton";
import { SyntaxBlock } from "@/components/syntax-block";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { diffLines } from "@/lib/diff";
import {
  ASSISTANT_APPLIED_EVENT,
  fileSchemaHint,
  useAssistantScope,
  type AssistantAppliedDetail,
} from "@/lib/assistant";
import { saveBlob } from "@/lib/download";
import { formatBytes, formatDateTime, formatRelative } from "@/lib/format";
import { setRouteGuard } from "@/lib/router";
import { isMarkdownPath, syntaxLanguage } from "@/lib/syntax";
import { useResource } from "@/lib/use-resource";
import { MAX_UPLOAD_FILE_BYTES, MAX_UPLOAD_TOTAL_BYTES, useUploads } from "@/lib/use-uploads";
import { EDITABLE_EXTENSIONS_LABEL, isEditableFileName } from "@/lib/validation";
import { cn } from "@/lib/utils";

interface OpenFile {
  path: string;
  name: string;
  /** Bytes last read from (or written to) the controller. */
  content: string;
  /** `null` for a file that exists only in this tab. */
  etag: string | null;
  size: number;
  mtime: string;
  editable: boolean;
  rendered: boolean;
  note?: string;
  isNew: boolean;
}

type Conflict =
  | { kind: "etag"; current: string | null }
  | { kind: "rendered"; message: string }
  | { kind: "not_editable"; message: string };

type DialogKind = "new-file" | "new-folder" | "delete" | "force" | "reload" | null;

function baseName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

const FILES_SCHEMA =
  "Data directory of the instance: SOUL.md, SKILL.md, config.yaml (rendered), skills/ and logs/. Only markdown, YAML, JSON, text and script files are writable.";

function parentPath(path: string): string {
  if (path === "/") return "/";
  const index = path.lastIndexOf("/");
  return index <= 0 ? "/" : path.slice(0, index);
}

function joinPath(directory: string, name: string): string {
  return directory === "/" ? `/${name}` : `${directory}/${name}`;
}

/** How a file is shown: editable text, rendered Markdown, coloured config. */
type FileView = "source" | "preview" | "highlight";

function defaultView(path: string, editable: boolean): FileView {
  if (!isMarkdownPath(path)) return "source";
  // A Markdown file nobody can save opens rendered: reading is the point.
  return editable ? "source" : "preview";
}

/** One line of the editor and of every read-only view: `leading-5`. */
const LINE_HEIGHT = 20;

/** Character range of one 1-based line, for the search reveal. */
function lineBounds(text: string, line: number): { start: number; end: number } {
  let start = 0;
  for (let index = 1; index < line; index += 1) {
    const next = text.indexOf("\n", start);
    if (next === -1) return { start: text.length, end: text.length };
    start = next + 1;
  }
  const end = text.indexOf("\n", start);
  return { start, end: end === -1 ? text.length : end };
}

/** Touch devices: focusing the editor pops the keyboard, so only scroll. */
function coarsePointer(): boolean {
  return typeof window !== "undefined" && window.matchMedia("(pointer: coarse)").matches;
}

const EXTERNAL_LINK_RE = /\[[^\]]*\]\(https?:\/\//i;

/** `10 MiB per file · 32 MiB per drop`, straight from the client caps. */
const UPLOAD_LIMITS_LABEL = `${MAX_UPLOAD_FILE_BYTES / (1024 * 1024)} MiB per file · ${
  MAX_UPLOAD_TOTAL_BYTES / (1024 * 1024)
} MiB per drop`;


function Breadcrumb({ path, onNavigate }: { path: string; onNavigate: (next: string) => void }) {  const segments = path === "/" ? [] : path.split("/").filter(Boolean);
  const crumbs = [
    { label: "data", path: "/" },
    ...segments.map((segment, index) => ({
      label: segment,
      path: `/${segments.slice(0, index + 1).join("/")}`,
    })),
  ];

  return (
    <nav aria-label="Folder" className="min-w-0">
      <ol className="flex flex-wrap items-center gap-x-1.5 gap-y-1 font-mono text-xs">
        {crumbs.map((crumb, index) => (
          <li key={crumb.path} className="flex min-w-0 items-center gap-1.5">
            {index > 0 ? (
              <span aria-hidden className="text-muted-foreground">
                /
              </span>
            ) : null}
            {index === crumbs.length - 1 ? (
              <span aria-current="page" className="max-w-56 truncate font-medium">
                {crumb.label}
              </span>
            ) : (
              <button
                type="button"
                className="max-w-40 truncate rounded-sm px-1 text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                onClick={() => onNavigate(crumb.path)}
              >
                {crumb.label}
              </button>
            )}
          </li>
        ))}
      </ol>
    </nav>
  );
}

/**
 * Hits of one search, scoped to the folder in the header: the path, the line
 * and the trimmed text, so a click is a decision and not a guess.
 */
function SearchHits({
  cwd,
  query,
  result,
  error,
  searching,
  onOpen,
}: {
  cwd: string;
  query: string;
  result: FileSearchResult | null;
  error: ApiError | null;
  searching: boolean;
  onOpen: (hit: FileSearchHit) => void;
}) {
  if (error) {
    return (
      <div className="min-h-0 flex-1 overflow-auto p-3">
        <ErrorAlert error={error} />
      </div>
    );
  }

  if (!result) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center p-6 text-sm text-muted-foreground">
        {searching ? "Walking the folder…" : null}
      </div>
    );
  }

  if (result.results.length === 0) {
    return (
      <div className="min-h-0 flex-1 p-6 text-center text-sm text-muted-foreground">
        No match for <span className="font-mono">{query}</span> in{" "}
        <span className="font-mono">{cwd}</span>.
        <p className="mt-1 text-[11px]">
          {result.scanned} {result.scanned === 1 ? "file" : "files"} scanned. Files above 1 MiB and
          binaries are skipped.
        </p>
      </div>
    );
  }

  return (
    <>
      <ul className="min-h-0 flex-1 divide-y overflow-auto">
        {result.results.map((hit) => (
          <li key={`${hit.path}:${hit.line}`}>
            <button
              type="button"
              className="w-full px-3 py-2 text-left hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:-outline-offset-2"
              onClick={() => onOpen(hit)}
            >
              <span className="flex items-baseline gap-1 font-mono text-[11px] text-muted-foreground">
                <span className="min-w-0 truncate">{hit.path}</span>
                <span className="shrink-0">:{hit.line}</span>
              </span>
              <span className="block truncate font-mono text-xs">{hit.preview || "\u00a0"}</span>
            </button>
          </li>
        ))}
      </ul>
      {result.truncated ? (
        <p className="border-t px-3 py-1.5 text-[11px] text-muted-foreground">
          Stopped at the hit or file cap: narrow the folder or the pattern to see the rest.
        </p>
      ) : null}
    </>
  );
}

export function InstanceFiles({
  id,
  canEdit,
  active,
  onRestarted,
  onFileSaved,
}: {
  id: string;
  canEdit: boolean;
  /** False while another tab is shown; coming back refreshes a clean buffer. */
  active: boolean;
  /** A save with `restart` changed the instance status: refresh the header. */
  onRestarted?: () => void;
  /** Any successful write or delete; lets the page refresh persona drift badges. */
  onFileSaved?: () => void;
}) {
  const [cwd, setCwd] = useState("/");
  const listing = useResource(() => api.listFiles(id, cwd), [id, cwd]);

  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [open, setOpen] = useState<OpenFile | null>(null);
  const [draft, setDraft] = useState("");
  const [loadingFile, setLoadingFile] = useState(false);
  const [fileError, setFileError] = useState<ApiError | null>(null);
  const [saveError, setSaveError] = useState<ApiError | null>(null);
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const [saving, setSaving] = useState<"save" | "restart" | null>(null);
  const [preview, setPreview] = useState(false);
  const [selection, setSelection] = useState("");

  const [dialog, setDialog] = useState<DialogKind>(null);
  const [name, setName] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);
  const [dialogError, setDialogError] = useState<ApiError | null>(null);
  const [dialogPending, setDialogPending] = useState(false);

  const [view, setView] = useState<FileView>("source");
  const [revealLine, setRevealLine] = useState<number | null>(null);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [downloadError, setDownloadError] = useState<ApiError | null>(null);

  const [query, setQuery] = useState("");
  const [regexMode, setRegexMode] = useState(false);
  const [searchResult, setSearchResult] = useState<FileSearchResult | null>(null);
  const [searchError, setSearchError] = useState<ApiError | null>(null);
  const [searching, setSearching] = useState(false);
  const [dropping, setDropping] = useState(false);

  const requestId = useRef(0);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const gutterRef = useRef<HTMLPreElement>(null);
  const readOnlyRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const dragDepth = useRef(0);
  const searchSeq = useRef(0);
  const wasActive = useRef(active);

  const uploads = useUploads({
    id,
    directory: cwd,
    onUploaded: () => {
      listing.reload();
      onFileSaved?.();
    },
  });

  const dirty = open ? (open.isNew ? true : draft !== open.content) : false;
  const entries = listing.data ?? [];

  // The assistant sees the open buffer, its ETag and the current selection so a
  // proposal can be diffed and applied with compare-and-swap. Refs keep the
  // apply listener reading the live values without re-subscribing per keystroke.
  const openRef = useRef(open);
  openRef.current = open;
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const loadFileRef = useRef(loadFile);
  loadFileRef.current = loadFile;

  useAssistantScope(
    active
      ? {
          page: "Files",
          instance: id,
          path: open?.path,
          fileContent: open ? draft : undefined,
          etag: open?.etag ?? undefined,
          selection: selection || undefined,
          schema: open ? fileSchemaHint(open.path, open.rendered) : FILES_SCHEMA,
        }
      : null,
  );

  useEffect(() => {
    setSelection("");
  }, [open?.path]);

  // An applied proposal went through PUT /files/content: refresh the listing
  // and, when the same file is open and clean, reload its buffer.
  useEffect(() => {
    function onApplied(event: Event) {
      const applied = (event as CustomEvent<AssistantAppliedDetail>).detail;
      if (!applied || applied.instance !== id || applied.kind !== "file") return;
      listing.reload();
      const current = openRef.current;
      if (!applied.path || current?.path !== applied.path) return;
      if (dirtyRef.current) {
        toast.info(`${current.name} changed on disk. Your unsaved buffer was kept.`);
        return;
      }
      void loadFileRef.current(applied.path, { keepView: true });
    }
    window.addEventListener(ASSISTANT_APPLIED_EVENT, onApplied);
    return () => window.removeEventListener(ASSISTANT_APPLIED_EVENT, onApplied);
  }, [id, listing.reload]);

  useEffect(() => {
    setCwd("/");
    setSelectedPath(null);
    setOpen(null);
    setDraft("");
    setFileError(null);
    setSaveError(null);
    setConflict(null);
    setPreview(false);
    setDialog(null);
    setView("source");
    setRevealLine(null);
    setQuery("");
    setSearchResult(null);
    setSearchError(null);
    setDownloadError(null);
    searchSeq.current += 1;
    requestId.current += 1;
  }, [id]);

  // A search is scoped to one folder: walking into another one starts over
  // instead of showing hits the header would no longer explain.
  useEffect(() => {
    searchSeq.current += 1;
    setSearchResult(null);
    setSearchError(null);
    setSearching(false);
  }, [cwd]);

  // The panel stays mounted behind the other tabs; on return, refresh what is
  // on disk so a revert or an outside edit cannot linger silently. A dirty
  // buffer is never replaced: saving it would be a deliberate conflict instead.
  useEffect(() => {
    if (active && !wasActive.current) {
      listing.reload();
      if (!dirty && open && !open.isNew) void loadFile(open.path, { keepView: true });
    }
    wasActive.current = active;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  useEffect(() => {
    if (!dirty) return;
    const handleBeforeUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", handleBeforeUnload);
    setRouteGuard(() =>
      window.confirm(`Discard unsaved changes to ${open?.name ?? "this file"}?`),
    );
    return () => {
      window.removeEventListener("beforeunload", handleBeforeUnload);
      setRouteGuard(null);
    };
  }, [dirty, open?.name]);

  const diff = useMemo(
    () => (open && dirty ? diffLines(open.content, draft) : null),
    [open, draft, dirty],
  );
  const lineCount = useMemo(() => draft.split("\n").length, [draft]);
  const draftBytes = useMemo(() => new TextEncoder().encode(draft).length, [draft]);

  function closeDialog() {
    setDialog(null);
    setName("");
    setNameError(null);
    setDialogError(null);
    setDialogPending(false);
  }

  function handleDialogOpenChange(next: boolean) {
    if (!next) closeDialog();
  }

  function confirmDiscard(): boolean {
    if (!dirty) return true;
    return window.confirm(`Discard unsaved changes to ${open?.name ?? "this file"}?`);
  }

  async function loadFile(
    path: string,
    options: { line?: number; keepView?: boolean } = {},
  ) {
    const ticket = requestId.current + 1;
    requestId.current = ticket;
    setSelectedPath(path);
    setLoadingFile(true);
    setFileError(null);
    setSaveError(null);
    setConflict(null);
    setPreview(false);
    try {
      const content = await api.readFile(id, path);
      if (requestId.current !== ticket) return;
      setOpen({ ...content, name: baseName(content.path), isNew: false });
      setDraft(content.content);
      // A search hit always lands in the text, even for a file that would
      // otherwise open rendered; a silent reload keeps the view the reader chose.
      if (options.line) setView("source");
      else if (!options.keepView) {
        setView(defaultView(content.path, content.editable && canEdit));
      }
      setRevealLine(options.line ?? null);
    } catch (cause) {
      if (requestId.current !== ticket) return;
      setOpen(null);
      setRevealLine(null);
      setFileError(toApiError(cause));
    } finally {
      if (requestId.current === ticket) setLoadingFile(false);
    }
  }

  function requestOpen(path: string, line?: number) {
    if (!confirmDiscard()) return;
    void loadFile(path, { line });
  }

  function closeFile() {
    if (!confirmDiscard()) return;
    requestId.current += 1;
    setSelectedPath(null);
    setOpen(null);
    setDraft("");
    setFileError(null);
    setSaveError(null);
    setConflict(null);
    setPreview(false);
    setRevealLine(null);
    setView("source");
  }

  function openEntry(entry: FileEntry) {
    if (entry.type === "dir") {
      setCwd(entry.path);
      return;
    }
    requestOpen(entry.path);
  }

  function validateNewName(value: string): string | null {
    const trimmed = value.trim();
    if (!trimmed) return "Enter a name.";
    if (trimmed === "." || trimmed === "..") return "That name is reserved.";
    if (trimmed.includes("/")) return "Use a plain name: it is created in the current folder.";
    if (entries.some((entry) => entry.name === trimmed)) return "An entry with this name already exists.";
    if (dialog === "new-file" && !isEditableFileName(trimmed)) {
      return `Use an editable extension: ${EDITABLE_EXTENSIONS_LABEL}.`;
    }
    return null;
  }

  function createFile() {
    const error = validateNewName(name);
    if (error) {
      setNameError(error);
      return;
    }
    const trimmed = name.trim();
    const path = joinPath(cwd, trimmed);
    requestId.current += 1;
    setOpen({
      path,
      name: trimmed,
      content: "",
      etag: null,
      size: 0,
      mtime: new Date().toISOString(),
      editable: true,
      rendered: false,
      isNew: true,
    });
    setSelectedPath(path);
    setDraft("");
    setFileError(null);
    setSaveError(null);
    setConflict(null);
    setPreview(false);
    setView("source");
    setRevealLine(null);
    closeDialog();
  }

  async function createFolder() {
    const error = validateNewName(name);
    if (error) {
      setNameError(error);
      return;
    }
    setDialogPending(true);
    setDialogError(null);
    try {
      const result = await api.mkdir(id, joinPath(cwd, name.trim()));
      toast.success(`Created ${result.path}.`);
      listing.reload();
      closeDialog();
    } catch (cause) {
      setDialogError(toApiError(cause));
    } finally {
      setDialogPending(false);
    }
  }

  async function save(options: { restart?: boolean; force?: boolean; etag?: string | null } = {}) {
    if (!open) return;
    setSaving(options.restart ? "restart" : "save");
    setSaveError(null);
    try {
      const result = await api.writeFile(id, {
        path: open.path,
        content: draft,
        etag: options.etag !== undefined ? options.etag : open.etag,
        restart: options.restart === true,
        force: options.force === true,
      });
      const name = baseName(result.path);
      setOpen({
        path: result.path,
        name,
        content: draft,
        etag: result.etag,
        size: result.size,
        mtime: result.mtime,
        editable: result.editable,
        rendered: result.rendered,
        isNew: false,
      });
      setConflict(null);
      setPreview(false);
      listing.reload();
      if (result.restarted) onRestarted?.();
      onFileSaved?.();
      const message = options.force
        ? `Overwrote ${name}.`
        : options.restart
          ? `Saved ${name} and restarted ${id}.`
          : `Saved ${name}.`;
      toast.success(message, result.backup ? { description: `Previous version: ${result.backup}` } : undefined);
    } catch (cause) {
      const error = toApiError(cause);
      if (error.code === "etag_mismatch") {
        const details = error.details as { current?: string | null } | null;
        setConflict({ kind: "etag", current: details?.current ?? null });
      } else if (error.code === "file_regenerated") {
        setConflict({ kind: "rendered", message: error.message });
      } else if (error.code === "file_not_editable") {
        setConflict({ kind: "not_editable", message: error.message });
      } else {
        setSaveError(error);
      }
    } finally {
      setSaving(null);
    }
  }

  async function deleteFile() {
    if (!open || open.isNew) return;
    setDialogPending(true);
    setDialogError(null);
    try {
      await api.deleteFile(id, open.path);
      toast.success(`Deleted ${open.name}.`);
      requestId.current += 1;
      setOpen(null);
      setSelectedPath(null);
      setDraft("");
      setPreview(false);
      setRevealLine(null);
      setView("source");
      listing.reload();
      onFileSaved?.();
      closeDialog();
    } catch (cause) {
      setDialogError(toApiError(cause));
    } finally {
      setDialogPending(false);
    }
  }

  function onEditorKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Escape") {
      event.currentTarget.blur();
      return;
    }
    if (event.key !== "Tab" || event.shiftKey) return;
    event.preventDefault();
    const node = event.currentTarget;
    const start = node.selectionStart;
    const end = node.selectionEnd;
    const next = `${draft.slice(0, start)}  ${draft.slice(end)}`;
    setDraft(next);
    requestAnimationFrame(() => {
      const field = textareaRef.current;
      if (!field) return;
      field.selectionStart = start + 2;
      field.selectionEnd = start + 2;
    });
  }

  /* ------------------------------------------------------------------------
   * Download
   * ---------------------------------------------------------------------- */

  async function downloadFile(path: string) {
    setDownloading(path);
    setDownloadError(null);
    try {
      const { blob, filename } = await api.downloadFile(id, path);
      saveBlob(blob, filename || baseName(path));
    } catch (cause) {
      setDownloadError(toApiError(cause));
    } finally {
      setDownloading(null);
    }
  }

  async function downloadFolder() {
    setDownloading("folder");
    setDownloadError(null);
    try {
      const { blob, filename } = await api.downloadArchive(id, cwd);
      saveBlob(blob, filename);
    } catch (cause) {
      setDownloadError(toApiError(cause));
    } finally {
      setDownloading(null);
    }
  }

  /* ------------------------------------------------------------------------
   * Search
   * ---------------------------------------------------------------------- */

  async function runSearch() {
    const needle = query.trim();
    if (!needle) {
      clearSearch();
      return;
    }
    const ticket = searchSeq.current + 1;
    searchSeq.current = ticket;
    setSearching(true);
    setSearchError(null);
    try {
      const result = await api.searchFiles(id, {
        q: needle,
        path: cwd,
        regex: regexMode,
        limit: 200,
      });
      if (searchSeq.current !== ticket) return;
      setSearchResult(result);
    } catch (cause) {
      if (searchSeq.current !== ticket) return;
      setSearchResult(null);
      setSearchError(toApiError(cause));
    } finally {
      if (searchSeq.current === ticket) setSearching(false);
    }
  }

  function clearSearch() {
    searchSeq.current += 1;
    setSearching(false);
    setQuery("");
    setSearchResult(null);
    setSearchError(null);
  }

  function openHit(hit: FileSearchHit) {
    // The hit names a line of the text: read it in the editor, not rendered.
    requestOpen(hit.path, hit.line);
  }

  // Reveal the line of a search hit once the buffer is on screen. Both views
  // use `leading-5`, so one line is one LINE_HEIGHT step.
  useEffect(() => {
    if (revealLine === null || !open) return;
    const line = revealLine;
    const offset = Math.max(0, (line - 1) * LINE_HEIGHT - LINE_HEIGHT * 2);
    const frame = requestAnimationFrame(() => {
      const field = textareaRef.current;
      if (field) {
        field.scrollTop = offset;
        if (!coarsePointer()) {
          const bounds = lineBounds(draft, line);
          field.focus({ preventScroll: true });
          field.setSelectionRange(bounds.start, bounds.end);
          field.scrollTop = offset;
        }
      } else if (readOnlyRef.current) {
        readOnlyRef.current.scrollTop = offset;
      }
      setRevealLine(null);
    });
    return () => cancelAnimationFrame(frame);
  }, [revealLine, open, draft]);

  /* ------------------------------------------------------------------------
   * Upload (drop or picker), one request per file
   * ---------------------------------------------------------------------- */

  function pickFiles() {
    fileInputRef.current?.click();
  }

  function onDragEnter(event: ReactDragEvent<HTMLDivElement>) {
    if (!event.dataTransfer.types.includes("Files")) return;
    // A viewer has no upload gesture; the drop is still swallowed so a stray
    // file never navigates the console away from the page.
    event.preventDefault();
    if (!canEdit) return;
    dragDepth.current += 1;
    setDropping(true);
  }

  function onDragOver(event: ReactDragEvent<HTMLDivElement>) {
    if (!event.dataTransfer.types.includes("Files")) return;
    // Preventing the default is what makes the drop legal in the browser.
    event.preventDefault();
    event.dataTransfer.dropEffect = canEdit ? "copy" : "none";
  }

  function onDragLeave() {
    dragDepth.current = Math.max(0, dragDepth.current - 1);
    if (dragDepth.current === 0) setDropping(false);
  }

  function onDrop(event: ReactDragEvent<HTMLDivElement>) {
    event.preventDefault();
    dragDepth.current = 0;
    setDropping(false);
    if (!canEdit) {
      toast.info("This account can download and read files, not upload them.");
      return;
    }
    const files = Array.from(event.dataTransfer.files ?? []);
    if (files.length > 0) uploads.enqueue(files);
  }

  // A viewer may browse and copy, never type: the controller would refuse the
  // save anyway, and an editor that cannot save is a trap.
  const editableFile = open ? open.editable && canEdit : false;
  const canSave = open && editableFile ? open.isNew || draft !== open.content : false;
  // Grow with the content, then scroll: a fixed 50vh box wastes half the page
  // on a three-line file and still cuts a long one.
  const editorHeight = Math.min(Math.max(lineCount + 1, 8), 26) * 20 + 24;
  const folderCount = entries.filter((entry) => entry.type === "dir").length;
  const fileCount = entries.length - folderCount;

  // Which readings a file offers. Markdown renders, config files colour; a
  // plain text file has only its source.
  const markdown = open ? isMarkdownPath(open.path) : false;
  const language = open ? syntaxLanguage(open.path) : null;
  const viewOptions: Array<{ value: FileView; label: string; title: string }> = markdown
    ? [
        { value: "source", label: "Source", title: "Show the Markdown source" },
        { value: "preview", label: "Preview", title: "Render the Markdown" },
      ]
    : language
      ? [
          { value: "source", label: "Source", title: "Edit the text" },
          {
            value: "highlight",
            label: "Coloured",
            title: `Read ${language.toUpperCase()} with syntax colouring`,
          },
        ]
      : [];
  const showsRendered = view === "preview" && markdown;
  const showsHighlight = view === "highlight" && language !== null;
  const externalLinks = markdown && EXTERNAL_LINK_RE.test(draft);
  const searchActive = searching || searchResult !== null || searchError !== null;

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <Breadcrumb path={cwd} onNavigate={setCwd} />
        <div className="flex flex-wrap items-center gap-1.5">
          {canEdit ? (
            <>
              <Button
                size="sm"
                variant="outline"
                onClick={pickFiles}
                disabled={uploads.busy}
                title={`Upload files into ${cwd} (${UPLOAD_LIMITS_LABEL})`}
              >
                {uploads.busy ? (
                  <Loader2 aria-hidden className="animate-spin" />
                ) : (
                  <Upload aria-hidden />
                )}
                Upload
              </Button>
              <input
                ref={fileInputRef}
                type="file"
                multiple
                hidden
                onChange={(event) => {
                  const files = Array.from(event.target.files ?? []);
                  event.target.value = "";
                  if (files.length > 0) uploads.enqueue(files);
                }}
              />
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  setName("");
                  setNameError(null);
                  setDialogError(null);
                  setDialog("new-file");
                }}
              >
                <FilePlus2 aria-hidden />
                New file
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  setName("");
                  setNameError(null);
                  setDialogError(null);
                  setDialog("new-folder");
                }}
              >
                <FolderPlus aria-hidden />
                New folder
              </Button>
            </>
          ) : null}
          <Button
            size="sm"
            variant="outline"
            onClick={() => void downloadFolder()}
            disabled={downloading !== null}
            title={`Download ${cwd} as a .zip archive (built on the controller)`}
          >
            {downloading === "folder" ? (
              <Loader2 aria-hidden className="animate-spin" />
            ) : (
              <FolderDown aria-hidden />
            )}
            Download .zip
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            onClick={listing.reload}
            aria-label="Refresh files"
            title="Refresh files"
          >
            <RefreshCw aria-hidden className={listing.refreshing ? "animate-spin" : undefined} />
          </Button>
        </div>
      </div>

      <form
        className="flex flex-wrap items-center gap-1.5"
        onSubmit={(event) => {
          event.preventDefault();
          void runSearch();
        }}
      >
        <div className="relative min-w-0 flex-1 sm:max-w-sm">
          <Search
            aria-hidden
            className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground"
          />
          <Input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={`Search in ${cwd}`}
            aria-label={`Search the text of ${cwd}`}
            className="pl-8 font-mono text-xs"
            spellCheck={false}
          />
        </div>
        <Button
          type="button"
          size="sm"
          variant={regexMode ? "default" : "outline"}
          aria-pressed={regexMode}
          title="Treat the search as a regular expression instead of plain text"
          onClick={() => setRegexMode((value) => !value)}
        >
          <Regex aria-hidden />
          Regex
        </Button>
        <Button type="submit" size="sm" variant="outline" disabled={searching || !query.trim()}>
          {searching ? <Loader2 aria-hidden className="animate-spin" /> : <ScanSearch aria-hidden />}
          Search
        </Button>
        {query || searchActive ? (
          <Button type="button" size="sm" variant="ghost" onClick={clearSearch}>
            <X aria-hidden />
            Clear
          </Button>
        ) : null}
        <span className="hidden text-[11px] text-muted-foreground sm:inline">
          Literal by default · 200 hits, 50 000 files max
        </span>
      </form>

      {uploads.items.length > 0 ? (
        <FileUploadsList
          items={uploads.items}
          onOverwrite={uploads.overwrite}
          onSkip={uploads.skip}
          onRetry={uploads.retry}
          onDismiss={uploads.dismiss}
          onClear={uploads.clearDone}
        />
      ) : null}

      {uploads.notice ? (
        <p className="text-xs text-muted-foreground" role="status">
          {uploads.notice}
        </p>
      ) : null}

      {downloadError ? <ErrorAlert error={downloadError} /> : null}

      <div
        className="relative"
        onDragEnter={onDragEnter}
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        onDrop={onDrop}
      >
        {dropping ? (
          <div className="pointer-events-none absolute inset-0 z-20 flex items-center justify-center rounded-md border-2 border-dashed border-ring bg-background/90 p-6 text-center">
            <div className="space-y-1">
              <Upload aria-hidden className="mx-auto size-5 text-muted-foreground" />
              <p className="text-sm font-medium">
                Drop to upload into <span className="font-mono">{cwd}</span>
              </p>
              <p className="text-xs text-muted-foreground">
                {UPLOAD_LIMITS_LABEL} · executables are refused
              </p>
            </div>
          </div>
        ) : null}

        <div className="grid gap-3 lg:grid-cols-[300px_minmax(0,1fr)]">
        <div className="flex max-h-[70vh] flex-col overflow-hidden rounded-md border">
          <div className="flex items-center justify-between gap-2 border-b bg-muted/30 px-3 py-1.5 text-xs text-muted-foreground">
            {searchActive ? (
              <span className="flex min-w-0 items-center gap-1.5">
                {searching ? <Loader2 aria-hidden className="size-3 shrink-0 animate-spin" /> : null}
                <span className="truncate">
                  {searchResult
                    ? `${searchResult.results.length} ${searchResult.results.length === 1 ? "hit" : "hits"} · ${searchResult.scanned} scanned`
                    : searching
                      ? "Searching…"
                      : "Search failed"}
                </span>
                {searchResult?.truncated ? (
                  <Badge variant="outline" className="px-1 py-0 text-[10px] font-normal">
                    truncated
                  </Badge>
                ) : null}
              </span>
            ) : (
              <span>
                {folderCount} {folderCount === 1 ? "folder" : "folders"} · {fileCount}{" "}
                {fileCount === 1 ? "file" : "files"}
              </span>
            )}
            {listing.refreshing ? <Loader2 aria-hidden className="size-3 animate-spin" /> : null}
          </div>

          {searchActive ? (
            <SearchHits
              cwd={cwd}
              query={query}
              result={searchResult}
              error={searchError}
              searching={searching}
              onOpen={openHit}
            />
          ) : listing.loading && !listing.data ? (
            <div className="space-y-2 p-3">
              <Skeleton className="h-8 w-full" />
              <Skeleton className="h-8 w-full" />
              <Skeleton className="h-8 w-full" />
            </div>
          ) : listing.error && !listing.data ? (
            <div className="p-3">
              <ErrorAlert error={listing.error} onRetry={listing.reload} />
            </div>
          ) : (
            <ul className="min-h-0 flex-1 divide-y overflow-auto">
              {cwd !== "/" ? (
                <li>
                  <button
                    type="button"
                    className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:-outline-offset-2"
                    onClick={() => setCwd(parentPath(cwd))}
                  >
                    <ArrowUp aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                    <span className="font-mono text-xs">..</span>
                  </button>
                </li>
              ) : null}

              {entries.map((entry) => {
                const selected = selectedPath === entry.path;
                return (
                  <li
                    key={entry.path}
                    className={cn(
                      "flex items-stretch hover:bg-muted/60",
                      selected && "bg-muted",
                    )}
                  >
                    <button
                      type="button"
                      aria-current={selected ? "true" : undefined}
                      className="flex min-w-0 flex-1 items-center gap-2 px-3 py-2 text-left text-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:-outline-offset-2"
                      onClick={() => openEntry(entry)}
                    >
                      {entry.type === "dir" ? (
                        <Folder aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                      ) : (
                        <FileText aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                      )}
                      <span className="min-w-0 flex-1 truncate font-mono text-xs">{entry.name}</span>
                      {entry.rendered ? (
                        <span className="shrink-0 text-[10px] tracking-wide text-muted-foreground uppercase">
                          rendered
                        </span>
                      ) : entry.type === "file" && !entry.editable ? (
                        <span className="shrink-0">
                          <Lock aria-hidden className="size-3 text-muted-foreground" />
                          <span className="sr-only">Read-only</span>
                        </span>
                      ) : null}
                      {entry.type === "file" ? (
                        <span className="shrink-0 text-[11px] text-muted-foreground">
                          {formatBytes(entry.size)}
                        </span>
                      ) : null}
                    </button>
                    {entry.type === "file" ? (
                      <button
                        type="button"
                        className="shrink-0 px-2 text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:-outline-offset-2"
                        aria-label={`Download ${entry.name}`}
                        title="Download file"
                        onClick={() => void downloadFile(entry.path)}
                      >
                        {downloading === entry.path ? (
                          <Loader2 aria-hidden className="size-3.5 animate-spin" />
                        ) : (
                          <Download aria-hidden className="size-3.5" />
                        )}
                      </button>
                    ) : null}
                  </li>
                );
              })}

              {entries.length === 0 ? (
                <li className="px-3 py-8 text-center text-sm text-muted-foreground">
                  This folder is empty.
                </li>
              ) : null}
            </ul>
          )}
        </div>

        <div className="min-w-0">
          {loadingFile ? (
            <div className="space-y-2 rounded-md border p-3">
              <Skeleton className="h-9 w-full" />
              <Skeleton className="h-64 w-full" />
            </div>
          ) : fileError ? (
            <div className="space-y-3 rounded-md border p-3">
              <ErrorAlert error={fileError} onRetry={selectedPath ? () => void loadFile(selectedPath) : undefined} />
              <Button variant="outline" size="sm" onClick={closeFile}>
                Close
              </Button>
            </div>
          ) : open ? (
            <div className="overflow-hidden rounded-md border">
              <div className="flex flex-wrap items-start gap-2 border-b bg-muted/30 px-3 py-2">
                <FileText aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="font-mono text-xs break-all">{open.name}</span>
                    {dirty ? (
                      <Badge variant="secondary" className="px-1.5 py-0 text-[10px] font-normal">
                        Unsaved
                      </Badge>
                    ) : null}
                    {open.isNew ? (
                      <Badge variant="outline" className="px-1.5 py-0 text-[10px] font-normal">
                        New
                      </Badge>
                    ) : null}
                    {open.rendered ? (
                      <Badge variant="outline" className="px-1.5 py-0 text-[10px] font-normal">
                        Rendered
                      </Badge>
                    ) : !canEdit ? (
                      <Badge variant="outline" className="px-1.5 py-0 text-[10px] font-normal">
                        Read-only account
                      </Badge>
                    ) : !open.editable ? (
                      <Badge variant="outline" className="px-1.5 py-0 text-[10px] font-normal">
                        Read-only
                      </Badge>
                    ) : null}
                  </div>
                  <p className="truncate font-mono text-[11px] text-muted-foreground">{open.path}</p>
                </div>
                <span
                  className="shrink-0 text-[11px] text-muted-foreground"
                  title={formatDateTime(open.mtime)}
                >
                  {open.isNew ? "not saved yet" : `${formatBytes(open.size)} · ${formatRelative(open.mtime)}`}
                </span>
                <div className="flex shrink-0 items-center gap-0.5">
                  {!open.isNew ? (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={`Download ${open.name}`}
                      title="Download file"
                      onClick={() => void downloadFile(open.path)}
                      disabled={downloading !== null}
                    >
                      {downloading === open.path ? (
                        <Loader2 aria-hidden className="animate-spin" />
                      ) : (
                        <Download aria-hidden />
                      )}
                    </Button>
                  ) : null}
                  {canEdit && !open.rendered && !open.isNew ? (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={`Delete ${open.name}`}
                      title="Delete file"
                      onClick={() => {
                        setDialogError(null);
                        setDialog("delete");
                      }}
                    >
                      <Trash2 aria-hidden />
                    </Button>
                  ) : null}
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Close file"
                    title="Close file"
                    onClick={closeFile}
                  >
                    <X aria-hidden />
                  </Button>
                </div>
              </div>

              {open.rendered ? (
                <Alert className="m-3 mb-0">
                  <Info aria-hidden />
                  <AlertTitle>Rendered by fleett</AlertTitle>
                  <AlertDescription>
                    <p>
                      {open.note
                        ? `${open.note.charAt(0).toUpperCase()}${open.note.slice(1)}`
                        : "fleett regenerates this file from the instance configuration on every change. A manual write is refused: change the instance instead."}
                    </p>
                  </AlertDescription>
                </Alert>
              ) : !canEdit ? (
                <Alert className="m-3 mb-0">
                  <Lock aria-hidden />
                  <AlertTitle>Read-only account</AlertTitle>
                  <AlertDescription>
                    <p>
                      This role can browse, read and copy files but not save them. Ask an admin for
                      the change, or sign in with a write-capable account.
                    </p>
                  </AlertDescription>
                </Alert>
              ) : !open.editable ? (
                <Alert className="m-3 mb-0">
                  <Lock aria-hidden />
                  <AlertTitle>Read-only file</AlertTitle>
                  <AlertDescription>
                    <p>
                      Only {EDITABLE_EXTENSIONS_LABEL} files can be edited here. This one can be
                      read, copied and deleted, not saved.
                    </p>
                  </AlertDescription>
                </Alert>
              ) : null}

              {conflict?.kind === "etag" ? (
                <Alert variant="destructive" className="m-3 mb-0 items-start">
                  <TriangleAlert aria-hidden className="mt-0.5" />
                  <AlertTitle>
                    {conflict.current ? "This file changed since you opened it" : "This file no longer exists"}
                  </AlertTitle>
                  <AlertDescription>
                    <p>
                      {conflict.current
                        ? "Another tab, or a change on the host, wrote a newer version. Nothing has been saved. Reload to take it, or keep your text and overwrite it (the previous bytes go to a .bak copy)."
                        : "It was deleted on disk. Reload, or keep your text by creating the file again."}
                    </p>
                    <div className="mt-3 flex flex-wrap items-center gap-2">
                      <Button size="sm" variant="outline" onClick={() => setDialog("reload")}>
                        Reload
                      </Button>
                      {conflict.current ? (
                        <Button size="sm" variant="outline" onClick={() => setDialog("force")}>
                          Keep my version
                        </Button>
                      ) : (
                        <Button size="sm" onClick={() => void save({ etag: null })} disabled={saving !== null}>
                          Save as new file
                        </Button>
                      )}
                      <CopyButton value={draft} label="Copy my text" showLabel size="sm" />
                    </div>
                  </AlertDescription>
                </Alert>
              ) : null}

              {conflict?.kind === "rendered" ? (
                <Alert variant="destructive" className="m-3 mb-0 items-start">
                  <TriangleAlert aria-hidden className="mt-0.5" />
                  <AlertTitle>Rendered by fleett</AlertTitle>
                  <AlertDescription>
                    <p>{conflict.message}</p>
                  </AlertDescription>
                </Alert>
              ) : null}

              {conflict?.kind === "not_editable" ? (
                <Alert variant="destructive" className="m-3 mb-0 items-start">
                  <TriangleAlert aria-hidden className="mt-0.5" />
                  <AlertTitle>This file cannot be saved</AlertTitle>
                  <AlertDescription>
                    <p>{conflict.message}</p>
                  </AlertDescription>
                </Alert>
              ) : null}

              {saveError ? (
                <div className="m-3 mb-0">
                  <ErrorAlert error={saveError} />
                </div>
              ) : null}

              {viewOptions.length > 1 ? (
                <div className="flex flex-wrap items-center justify-between gap-2 border-b bg-muted/20 px-3 py-1.5">
                  <span className="text-[11px] text-muted-foreground">
                    {showsRendered
                      ? "Rendered Markdown · images, tables and raw HTML stay text."
                      : showsHighlight && language
                        ? `Coloured ${language.toUpperCase()} · reading view, edit in Source`
                        : "Plain source view"}
                  </span>
                  <Segmented
                    value={view}
                    options={viewOptions}
                    onChange={setView}
                    label={`${open.name} view`}
                  />
                </div>
              ) : null}

              {showsRendered ? (
                <>
                  <MarkdownView value={draft} maxHeight={`${editorHeight}px`} />
                  <p className="border-t px-3 py-1.5 text-[11px] text-muted-foreground">
                    {externalLinks
                      ? "Links to web addresses open in a new tab."
                      : "Plain preview: images, tables and raw HTML are not rendered."}
                    {dirty ? " Showing your unsaved text." : null}
                  </p>
                </>
              ) : showsHighlight && language ? (
                <SyntaxBlock value={draft} language={language} maxHeight={editorHeight} />
              ) : editableFile ? (
                preview && diff ? (
                  <div>
                    <div className="flex items-center justify-between gap-2 border-b px-3 py-1.5 text-xs text-muted-foreground">
                      <span>Changes not saved yet</span>
                      <span className="font-mono">
                        +{diff.added} −{diff.removed}
                      </span>
                    </div>
                    <LineDiff hunks={diff.hunks} empty="No changes yet." />
                    {diff.coarse ? (
                      <p className="border-t px-3 py-1.5 text-[11px] text-muted-foreground">
                        The file is too large to align line by line: the preview shows a full
                        replacement.
                      </p>
                    ) : null}
                  </div>
                ) : (
                <div className="flex overflow-hidden" style={{ height: editorHeight }}>
                  <pre
                    ref={gutterRef}
                    aria-hidden
                    className="w-12 shrink-0 overflow-hidden border-r bg-muted/30 py-3 pr-1 text-right font-mono text-[11px] leading-5 text-muted-foreground select-none"
                  >
                    {Array.from({ length: lineCount }, (_, index) => index + 1).join("\n")}
                  </pre>
                  <textarea
                    ref={textareaRef}
                    value={draft}
                    wrap="off"
                    spellCheck={false}
                    autoCapitalize="off"
                    autoCorrect="off"
                    aria-label={`Edit ${open.name}`}
                    aria-describedby="editor-hint"
                    className="h-full min-w-0 flex-1 resize-none bg-transparent px-3 py-3 font-mono text-xs leading-5 outline-none"
                    onChange={(event) => setDraft(event.target.value)}
                    onSelect={(event) => {
                      const field = event.currentTarget;
                      const start = field.selectionStart;
                      const end = field.selectionEnd;
                      setSelection(end > start ? field.value.slice(start, end) : "");
                    }}
                    onScroll={() => {
                      if (gutterRef.current && textareaRef.current) {
                        gutterRef.current.scrollTop = textareaRef.current.scrollTop;
                      }
                    }}
                    onKeyDown={onEditorKeyDown}
                  />
                </div>
                )
              ) : (
                <div
                  ref={readOnlyRef}
                  tabIndex={0}
                  role="region"
                  aria-label={`${open.name} content`}
                  className="overflow-auto rounded-none outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:-outline-offset-2"
                  style={{ maxHeight: editorHeight }}
                >
                  <div className="flex min-w-full">
                    <pre
                      aria-hidden
                      className="sticky left-0 z-10 w-12 shrink-0 border-r bg-muted/30 py-3 pr-1 text-right font-mono text-[11px] leading-5 text-muted-foreground select-none"
                    >
                      {Array.from({ length: lineCount }, (_, index) => index + 1).join("\n")}
                    </pre>
                    <pre className="px-3 py-3 font-mono text-xs leading-5 whitespace-pre">{draft}</pre>
                  </div>
                </div>
              )}

              <div className="flex flex-wrap items-center gap-2 border-t px-3 py-2">
                <span className="text-[11px] text-muted-foreground">
                  {lineCount} {lineCount === 1 ? "line" : "lines"} · {formatBytes(draftBytes)}
                </span>
                {editableFile && !preview && view === "source" ? (
                  <span id="editor-hint" className="hidden text-[11px] text-muted-foreground sm:inline">
                    Tab inserts two spaces · Esc leaves the editor
                  </span>
                ) : null}
                <div className="ml-auto flex flex-wrap items-center gap-2">
                  {view !== "source" ? (
                    <CopyButton value={draft} label="Copy file" showLabel size="sm" variant="outline" />
                  ) : editableFile ? (
                    <Button
                      variant="ghost"
                      size="sm"
                      aria-pressed={preview}
                      onClick={() => setPreview((value) => !value)}
                    >
                      <FileDiff aria-hidden />
                      {preview ? "Hide changes" : "Preview changes"}
                    </Button>
                  ) : (
                    <CopyButton value={draft} label="Copy file" showLabel size="sm" variant="outline" />
                  )}
                  {canEdit && editableFile ? (
                    <>
                      <Button size="sm" disabled={!canSave || saving !== null} onClick={() => void save()}>
                        {saving === "save" ? <Loader2 aria-hidden className="animate-spin" /> : <Save aria-hidden />}
                        Save
                      </Button>
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={!canSave || saving !== null}
                        onClick={() => void save({ restart: true })}
                      >
                        {saving === "restart" ? (
                          <Loader2 aria-hidden className="animate-spin" />
                        ) : (
                          <RotateCw aria-hidden />
                        )}
                        Save & restart
                      </Button>
                    </>
                  ) : null}
                </div>
              </div>

            </div>
          ) : (
            <div className="rounded-md border">
              <EmptyState
                icon={FileText}
                title="No file open"
                description={`Pick a file on the left. Editable files (${EDITABLE_EXTENSIONS_LABEL}) open in the editor; the rest open read-only.`}
                action={
                  canEdit ? (
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        setName("");
                        setNameError(null);
                        setDialogError(null);
                        setDialog("new-file");
                      }}
                    >
                      <FilePlus2 aria-hidden />
                      New file
                    </Button>
                  ) : undefined
                }
              />
            </div>
          )}
        </div>
        </div>
      </div>

      <Dialog open={dialog === "new-file" || dialog === "new-folder"} onOpenChange={handleDialogOpenChange}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{dialog === "new-folder" ? "New folder" : "New file"}</DialogTitle>
            <DialogDescription>
              {dialog === "new-folder" ? "Creates one folder in " : "Created in "}
              <span className="font-mono">{cwd}</span>
              {dialog === "new-folder"
                ? ". Nothing is committed until a file inside it changes."
                : ` on the first save. Editable extensions: ${EDITABLE_EXTENSIONS_LABEL}.`}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            <Label htmlFor="file-name">Name</Label>
            <Input
              id="file-name"
              value={name}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              aria-invalid={nameError !== null}
              aria-describedby={nameError ? "file-name-error" : undefined}
              onChange={(event) => {
                setName(event.target.value);
                setNameError(null);
              }}
              onKeyDown={(event) => {
                if (event.key !== "Enter") return;
                event.preventDefault();
                if (dialog === "new-folder") void createFolder();
                else createFile();
              }}
            />
            {nameError ? (
              <p id="file-name-error" className="text-xs text-destructive">
                {nameError}
              </p>
            ) : null}
          </div>
          {dialog === "new-folder" && dialogError ? <ErrorAlert error={dialogError} /> : null}
          <DialogFooter>
            <Button variant="outline" onClick={closeDialog} disabled={dialogPending}>
              Cancel
            </Button>
            {dialog === "new-folder" ? (
              <Button onClick={() => void createFolder()} disabled={dialogPending}>
                {dialogPending ? <Loader2 aria-hidden className="animate-spin" /> : null}
                Create folder
              </Button>
            ) : (
              <Button onClick={createFile}>Create file</Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={dialog === "delete"}
        onOpenChange={handleDialogOpenChange}
        title={`Delete ${open?.name ?? "file"}?`}
        description={
          <>
            <span className="font-mono">{open?.path}</span> is removed from the instance home.
            Editable files are versioned, so an earlier copy can be restored from the History
            tab.
          </>
        }
        confirmLabel="Delete file"
        destructive
        pending={dialogPending}
        error={dialogError}
        onConfirm={() => void deleteFile()}
      />

      <ConfirmDialog
        open={dialog === "force"}
        onOpenChange={handleDialogOpenChange}
        title="Overwrite the newer version?"
        description="Your text replaces what is on disk now. The current bytes are copied to a .bak sibling first, and the overwrite is recorded as a new commit."
        confirmLabel="Overwrite"
        pending={saving !== null}
        onConfirm={() => {
          closeDialog();
          void save({ force: true });
        }}
      />

      <ConfirmDialog
        open={dialog === "reload"}
        onOpenChange={handleDialogOpenChange}
        title="Reload from disk?"
        description="Your text is discarded and the version on disk is loaded. Copy your text first if you want to keep it."
        confirmLabel="Reload"
        onConfirm={() => {
          const path = open?.path;
          closeDialog();
          if (path) void loadFile(path);
        }}
      />
    </div>
  );
}
