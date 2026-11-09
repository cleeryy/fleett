/**
 * Line diff shared by the editor preview and the mock history.
 *
 * Small, dependency-free and deterministic: common leading and trailing lines
 * are trimmed first, the middle is aligned with a longest-common-subsequence
 * table, and anything too large falls back to "everything replaced" instead of
 * allocating a matrix that would freeze the tab.
 */

export type DiffKind = "context" | "add" | "remove";

export interface DiffLine {
  kind: DiffKind;
  text: string;
  /** 1-based line number in the old text, `null` for an added line. */
  oldLine: number | null;
  /** 1-based line number in the new text, `null` for a removed line. */
  newLine: number | null;
}

export interface DiffHunk {
  oldStart: number;
  oldCount: number;
  newStart: number;
  newCount: number;
  lines: DiffLine[];
}

export interface DiffResult {
  hunks: DiffHunk[];
  added: number;
  removed: number;
  /** The texts were too large to align; the diff shows a full replacement. */
  coarse: boolean;
}

export interface PatchSummary {
  files: number;
  added: number;
  removed: number;
}

/** Cells of the LCS table above which the diff degrades to a replacement. */
const MAX_CELLS = 1_500_000;
const DEFAULT_CONTEXT = 3;

interface Edit {
  kind: DiffKind;
  text: string;
}

function align(before: string[], after: string[]): { edits: Edit[]; coarse: boolean } {
  if (before.length * after.length > MAX_CELLS) {
    return {
      edits: [
        ...before.map((text): Edit => ({ kind: "remove", text })),
        ...after.map((text): Edit => ({ kind: "add", text })),
      ],
      coarse: true,
    };
  }

  const width = after.length + 1;
  const table = new Int32Array((before.length + 1) * width);
  for (let i = before.length - 1; i >= 0; i -= 1) {
    for (let j = after.length - 1; j >= 0; j -= 1) {
      table[i * width + j] =
        before[i] === after[j]
          ? table[(i + 1) * width + (j + 1)] + 1
          : Math.max(table[(i + 1) * width + j], table[i * width + (j + 1)]);
    }
  }

  const edits: Edit[] = [];
  let i = 0;
  let j = 0;
  while (i < before.length && j < after.length) {
    if (before[i] === after[j]) {
      edits.push({ kind: "context", text: before[i] });
      i += 1;
      j += 1;
    } else if (table[(i + 1) * width + j] >= table[i * width + (j + 1)]) {
      edits.push({ kind: "remove", text: before[i] });
      i += 1;
    } else {
      edits.push({ kind: "add", text: after[j] });
      j += 1;
    }
  }
  while (i < before.length) {
    edits.push({ kind: "remove", text: before[i] });
    i += 1;
  }
  while (j < after.length) {
    edits.push({ kind: "add", text: after[j] });
    j += 1;
  }
  return { edits, coarse: false };
}

function buildHunks(lines: DiffLine[], context: number): DiffHunk[] {
  const ranges: Array<[number, number]> = [];
  lines.forEach((line, index) => {
    if (line.kind === "context") return;
    const start = Math.max(0, index - context);
    const end = Math.min(lines.length - 1, index + context);
    const last = ranges[ranges.length - 1];
    if (last && start <= last[1] + 1) last[1] = Math.max(last[1], end);
    else ranges.push([start, end]);
  });

  return ranges.map(([start, end]) => {
    const slice = lines.slice(start, end + 1);
    const oldLines = slice.filter((line) => line.kind !== "add");
    const newLines = slice.filter((line) => line.kind !== "remove");
    return {
      oldStart: oldLines[0]?.oldLine ?? 0,
      oldCount: oldLines.length,
      newStart: newLines[0]?.newLine ?? 0,
      newCount: newLines.length,
      lines: slice,
    };
  });
}

/** Align two texts and keep `context` unchanged lines around every run. */
export function diffLines(before: string, after: string, context = DEFAULT_CONTEXT): DiffResult {
  const oldLines = before.split("\n");
  const newLines = after.split("\n");

  let prefix = 0;
  while (prefix < oldLines.length && prefix < newLines.length && oldLines[prefix] === newLines[prefix]) {
    prefix += 1;
  }
  let suffix = 0;
  while (
    suffix < oldLines.length - prefix &&
    suffix < newLines.length - prefix &&
    oldLines[oldLines.length - 1 - suffix] === newLines[newLines.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  const middle = align(
    oldLines.slice(prefix, oldLines.length - suffix),
    newLines.slice(prefix, newLines.length - suffix),
  );

  const edits: Edit[] = [
    ...oldLines.slice(0, prefix).map((text): Edit => ({ kind: "context", text })),
    ...middle.edits,
    ...oldLines.slice(oldLines.length - suffix).map((text): Edit => ({ kind: "context", text })),
  ];

  let oldNo = 1;
  let newNo = 1;
  const lines: DiffLine[] = edits.map((edit) => {
    if (edit.kind === "context") {
      const line: DiffLine = { kind: "context", text: edit.text, oldLine: oldNo, newLine: newNo };
      oldNo += 1;
      newNo += 1;
      return line;
    }
    if (edit.kind === "remove") {
      const line: DiffLine = { kind: "remove", text: edit.text, oldLine: oldNo, newLine: null };
      oldNo += 1;
      return line;
    }
    const line: DiffLine = { kind: "add", text: edit.text, oldLine: null, newLine: newNo };
    newNo += 1;
    return line;
  });

  const added = lines.reduce((count, line) => count + (line.kind === "add" ? 1 : 0), 0);
  const removed = lines.reduce((count, line) => count + (line.kind === "remove" ? 1 : 0), 0);
  if (added === 0 && removed === 0) {
    return { hunks: [], added, removed, coarse: middle.coarse };
  }
  return { hunks: buildHunks(lines, context), added, removed, coarse: middle.coarse };
}

/** Unified patch text for one file, the shape `GET /git/diff` returns. */
export function toPatch(path: string, before: string, after: string): string {
  const result = diffLines(before, after);
  if (result.hunks.length === 0) return "";

  const header: string[] = [`diff --git a/${path} b/${path}`];
  if (before === "") {
    header.push("new file mode 100644", "--- /dev/null", `+++ b/${path}`);
  } else if (after === "") {
    header.push("deleted file mode 100644", `--- a/${path}`, "+++ /dev/null");
  } else {
    header.push(`--- a/${path}`, `+++ b/${path}`);
  }

  const body = result.hunks.flatMap((hunk) => [
    `@@ -${hunk.oldStart},${hunk.oldCount} +${hunk.newStart},${hunk.newCount} @@`,
    ...hunk.lines.map(
      (line) => `${line.kind === "add" ? "+" : line.kind === "remove" ? "-" : " "}${line.text}`,
    ),
  ]);

  return `${[...header, ...body].join("\n")}\n`;
}

/** Count files and changed lines in patch text, for the history header. */
export function summarizePatch(text: string): PatchSummary {
  let files = 0;
  let added = 0;
  let removed = 0;
  for (const line of text.split("\n")) {
    if (line.startsWith("diff --git ")) files += 1;
    else if (line.startsWith("+") && !line.startsWith("+++")) added += 1;
    else if (line.startsWith("-") && !line.startsWith("---")) removed += 1;
  }
  return { files, added, removed };
}
