import { useCallback, useRef, useState } from "react";
import { api } from "@/api";
import { toApiError, type ApiError } from "@/api/errors";

/** Mirrors `docs/FILES.md`: one file, then the whole request. */
export const MAX_UPLOAD_FILE_BYTES = 10 * 1024 * 1024;
export const MAX_UPLOAD_TOTAL_BYTES = 32 * 1024 * 1024;

export type UploadStatus =
  | "queued"
  | "uploading"
  | "done"
  | "conflict"
  | "refused"
  | "too_large"
  | "skipped"
  | "error";

export interface UploadItem {
  key: string;
  file: File;
  name: string;
  size: number;
  /** Folder the drop targeted; frozen, so navigating away never reroutes it. */
  directory: string;
  status: UploadStatus;
  /** Set once the operator accepted replacing the existing file. */
  overwrite: boolean;
  /** Local refusal (size, name): never reaches the controller. */
  reason?: string;
  /** Controller refusal or failure. */
  error?: ApiError;
  path?: string;
  backup?: string;
}

let uploadSeq = 0;

function nextKey(): string {
  uploadSeq += 1;
  return `upload-${uploadSeq}`;
}

/**
 * Upload queue: one request per file, run one after another. A `409
 * file_exists` stops that file only — the rest of the batch keeps going and the
 * refused name stays listed with an Overwrite action, so the selection is never
 * lost.
 */
export function useUploads({
  id,
  directory,
  onUploaded,
}: {
  id: string;
  /** Folder a new drop targets (the current one). */
  directory: string;
  onUploaded: () => void;
}) {
  const [items, setItems] = useState<UploadItem[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const directoryRef = useRef(directory);
  directoryRef.current = directory;
  const onUploadedRef = useRef(onUploaded);
  onUploadedRef.current = onUploaded;
  const chain = useRef<Promise<void>>(Promise.resolve());

  const patch = useCallback((key: string, changes: Partial<UploadItem>) => {
    setItems((previous) =>
      previous.map((item) => (item.key === key ? { ...item, ...changes } : item)),
    );
  }, []);

  const run = useCallback(
    async (batch: UploadItem[]) => {
      let written = 0;
      for (const item of batch) {
        patch(item.key, { status: "uploading", error: undefined, reason: undefined });
        try {
          const result = await api.uploadFile(id, item.directory, item.file, {
            overwrite: item.overwrite,
          });
          const file = result.files[0];
          written += 1;
          patch(item.key, {
            status: "done",
            path: file?.path,
            size: file?.size ?? item.size,
            backup: file?.backup,
          });
        } catch (cause) {
          const error = toApiError(cause);
          if (error.code === "file_exists") {
            patch(item.key, { status: "conflict", error });
          } else if (
            error.code === "executable_file" ||
            error.code === "file_regenerated" ||
            error.code === "file_not_editable"
          ) {
            patch(item.key, { status: "refused", error });
          } else if (error.code === "file_too_large") {
            patch(item.key, { status: "too_large", error });
          } else {
            patch(item.key, { status: "error", error });
          }
        }
      }
      if (written > 0) onUploadedRef.current();
    },
    [id, patch],
  );

  const enqueue = useCallback(
    (files: File[]) => {
      if (files.length === 0) return;
      const target = directoryRef.current;
      const total = files.reduce((sum, file) => sum + file.size, 0);
      // The 32 MiB cap is per request; the console sends one file per request,
      // so a large drop still lands — the notice says what is going on.
      setNotice(
        total > MAX_UPLOAD_TOTAL_BYTES
          ? `This drop is ${(total / (1024 * 1024)).toFixed(1)} MiB, over the 32 MiB one upload request accepts. Files are sent one at a time, so it will go through in sequence.`
          : null,
      );

      const batch: UploadItem[] = files.map((file) => {
        const name = file.name;
        const base: UploadItem = {
          key: nextKey(),
          file,
          name,
          size: file.size,
          directory: target,
          status: "queued",
          overwrite: false,
        };
        if (file.size > MAX_UPLOAD_FILE_BYTES) {
          return {
            ...base,
            status: "too_large",
            reason: `${name} is ${Math.ceil(file.size / (1024 * 1024))} MiB: one file carries at most 10 MiB.`,
          };
        }
        if (!name || name.includes("/") || name.includes("\\") || name === "." || name === "..") {
          return {
            ...base,
            status: "refused",
            reason: `"${name || "unnamed"}" is not a plain file name.`,
          };
        }
        return base;
      });

      // Newest batch on top, keeping the previous results below it.
      setItems((previous) => [...batch, ...previous]);
      const runnable = batch.filter((item) => item.status === "queued");
      if (runnable.length === 0) return;
      chain.current = chain.current.then(() => run(runnable));
    },
    [run],
  );

  const overwrite = useCallback(
    (key: string) => {
      const item = items.find((candidate) => candidate.key === key);
      if (!item || (item.status !== "conflict" && item.status !== "skipped")) return;
      const next: UploadItem = {
        ...item,
        status: "queued",
        overwrite: true,
        error: undefined,
      };
      setItems((previous) => previous.map((entry) => (entry.key === key ? next : entry)));
      chain.current = chain.current.then(() => run([next]));
    },
    [items, run],
  );

  const skip = useCallback((key: string) => {
    patch(key, { status: "skipped" });
  }, [patch]);

  const retry = useCallback(
    (key: string) => {
      const item = items.find((candidate) => candidate.key === key);
      if (!item) return;
      const next: UploadItem = { ...item, status: "queued", error: undefined };
      setItems((previous) => previous.map((entry) => (entry.key === key ? next : entry)));
      chain.current = chain.current.then(() => run([next]));
    },
    [items, run],
  );

  const dismiss = useCallback((key: string) => {
    setItems((previous) => previous.filter((item) => item.key !== key));
  }, []);

  /** Drops the finished rows, never a pending decision. */
  const clearDone = useCallback(() => {
    setItems((previous) =>
      previous.filter((item) => item.status !== "done" && item.status !== "skipped"),
    );
    setNotice(null);
  }, []);

  const busy = items.some((item) => item.status === "queued" || item.status === "uploading");

  return { items, notice, busy, enqueue, overwrite, skip, retry, dismiss, clearDone };
}
