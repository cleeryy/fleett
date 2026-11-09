import type { CreatePipelineInput, Pipeline, PipelineStep } from "@/api/types";

/**
 * Tiny YAML reader for pipeline documents. The controller stores the canonical
 * JSON, so the console only needs the shape documented in docs/PIPELINES.md:
 * top-level scalars and a `steps` list of scalars, inline lists and `|` block
 * prompts. Anything outside that grammar is reported with a line number
 * instead of being guessed.
 */

export class PipelineYamlError extends Error {
  readonly line: number;

  constructor(message: string, line: number) {
    super(`Line ${line}: ${message}`);
    this.name = "PipelineYamlError";
    this.line = line;
  }
}

interface Line {
  indent: number;
  text: string;
  number: number;
}

function tokenize(source: string): Line[] {
  return source.split(/\r?\n/).map((raw, index) => {
    const expanded = raw.replace(/\t/g, "  ");
    const trimmed = expanded.trim();
    const indent = expanded.length - expanded.trimStart().length;
    return { indent, text: trimmed, number: index + 1 };
  });
}

function meaningful(line: Line): boolean {
  return line.text !== "" && !line.text.startsWith("#");
}

function stripComment(value: string): string {
  const comment = value.search(/\s#/);
  return comment === -1 ? value.trim() : value.slice(0, comment).trim();
}

function parseScalar(value: string, line: number): unknown {
  const text = stripComment(value);
  if (text === "") return null;
  if (text === "true") return true;
  if (text === "false") return false;
  if (text === "null" || text === "~") return null;
  if (/^-?\d+$/.test(text)) return Number(text);
  if (/^-?\d+\.\d+$/.test(text)) return Number(text);
  if (text.startsWith('"') && text.endsWith('"')) {
    try {
      return JSON.parse(text) as string;
    } catch {
      throw new PipelineYamlError("invalid double-quoted string", line);
    }
  }
  if (text.startsWith("'") && text.endsWith("'")) {
    return text.slice(1, -1).replace(/''/g, "'");
  }
  if (text.startsWith("[") && text.endsWith("]")) {
    const inner = text.slice(1, -1).trim();
    if (!inner) return [];
    return splitInline(inner, line).map((part) => parseScalar(part, line));
  }
  return text;
}

function splitInline(value: string, line: number): string[] {
  const parts: string[] = [];
  let current = "";
  let quote: string | null = null;
  for (const char of value) {
    if (quote) {
      current += char;
      if (char === quote) quote = null;
    } else if (char === '"' || char === "'") {
      quote = char;
      current += char;
    } else if (char === ",") {
      parts.push(current.trim());
      current = "";
    } else {
      current += char;
    }
  }
  if (quote) throw new PipelineYamlError("unterminated quoted string", line);
  parts.push(current.trim());
  return parts.filter((part) => part !== "");
}

function blockScalar(lines: Line[], start: number, parentIndent: number, folded: boolean): {
  value: string;
  next: number;
} {
  const collected: Line[] = [];
  let index = start;
  while (index < lines.length) {
    const line = lines[index];
    if (line.text !== "" && line.indent <= parentIndent) break;
    collected.push(line);
    index += 1;
  }
  const nonBlank = collected.filter((line) => line.text !== "");
  const indent = nonBlank.length > 0 ? Math.min(...nonBlank.map((line) => line.indent)) : parentIndent + 2;
  const text = collected.map((line) => line.text.slice(Math.max(0, indent - line.indent))).join("\n");
  return { value: folded ? text.replace(/\n(?!$)/g, " ").trim() : text, next: index };
}

function blockList(lines: Line[], start: number, parentIndent: number): { value: string[]; next: number } {
  const values: string[] = [];
  let index = start;
  while (index < lines.length) {
    const line = lines[index];
    if (!meaningful(line)) {
      index += 1;
      continue;
    }
    if (line.indent <= parentIndent || !line.text.startsWith("- ")) break;
    const scalar = parseScalar(line.text.slice(2), line.number);
    values.push(String(scalar ?? "").trim());
    index += 1;
  }
  return { value: values, next: index };
}

function readValue(
  lines: Line[],
  index: number,
  keyIndent: number,
  raw: string,
): { value: unknown; next: number } {
  const text = raw.trim();
  if (text === "|" || text === "|-" || text === "|+" || text === ">" || text === ">-" || text === ">+") {
    const folded = text.startsWith(">");
    const trailing = text.endsWith("-") ? "" : "\n";
    const started = blockScalar(lines, index + 1, keyIndent, folded);
    return { value: `${started.value}${trailing}`, next: started.next };
  }
  if (text === "") {
    const list = blockList(lines, index + 1, keyIndent);
    if (list.value.length > 0) return { value: list.value, next: list.next };
    return { value: null, next: index + 1 };
  }
  return { value: parseScalar(text, lines[index].number), next: index + 1 };
}

function parseStep(lines: Line[], start: number, itemIndent: number): { step: Record<string, unknown>; next: number } {
  const step: Record<string, unknown> = {};
  const first = lines[start];
  const firstPair = /^-\s+([^:]+):\s*(.*)$/.exec(first.text);
  if (!firstPair) throw new PipelineYamlError("a step must start with `- id: ...`", first.number);
  const firstValue = readValue(lines, start, itemIndent, firstPair[2]);
  step[firstPair[1].trim()] = firstValue.value;
  let index = firstValue.next;

  while (index < lines.length) {
    const line = lines[index];
    if (!meaningful(line)) {
      index += 1;
      continue;
    }
    if (line.indent <= itemIndent) break;
    const pair = /^([^:]+):\s*(.*)$/.exec(line.text);
    if (!pair) throw new PipelineYamlError("expected `key: value`", line.number);
    const parsed = readValue(lines, index, line.indent, pair[2]);
    step[pair[1].trim()] = parsed.value;
    index = parsed.next;
  }
  return { step, next: index };
}

function asString(value: unknown, field: string, line: number, required = true): string {
  if (value === null || value === undefined || value === "") {
    if (required) throw new PipelineYamlError(`${field} is required`, line);
    return "";
  }
  if (typeof value !== "string") throw new PipelineYamlError(`${field} must be a string`, line);
  return value.trim();
}

function buildSteps(value: unknown): PipelineStep[] {
  if (!Array.isArray(value) || value.length === 0) {
    throw new PipelineYamlError("steps must be a non-empty list", 1);
  }
  return value.map((raw, index) => {
    if (typeof raw !== "object" || raw === null) {
      throw new PipelineYamlError(`step ${index + 1} must be a mapping`, 1);
    }
    const source = raw as Record<string, unknown>;
    const step: PipelineStep = {
      id: asString(source.id, "steps[].id", 1),
      instance: asString(source.instance, "steps[].instance", 1),
      prompt: asString(source.prompt, "steps[].prompt", 1),
    };
    const depends = source.depends_on;
    if (Array.isArray(depends)) {
      step.depends_on = depends.map((item) => String(item).trim()).filter(Boolean);
    }
    if (source.retries !== undefined && source.retries !== null) {
      const retries = Number(source.retries);
      if (!Number.isInteger(retries)) throw new PipelineYamlError("retries must be an integer", 1);
      step.retries = retries;
    }
    const timeout = source.timeout_s ?? source.timeout;
    if (timeout !== undefined && timeout !== null) {
      const seconds = Number(timeout);
      if (!Number.isInteger(seconds)) throw new PipelineYamlError("timeout_s must be an integer", 1);
      step.timeout_s = seconds;
    }
    if (source.on_failure !== undefined && source.on_failure !== null) {
      const mode = String(source.on_failure);
      if (mode !== "stop" && mode !== "continue") {
        throw new PipelineYamlError("on_failure must be stop or continue", 1);
      }
      step.on_failure = mode;
    }
    return step;
  });
}

function fromObject(raw: unknown): CreatePipelineInput {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new PipelineYamlError("a pipeline must be a mapping", 1);
  }
  const source = raw as Record<string, unknown>;
  const triggerRaw = source.trigger === undefined || source.trigger === null ? "manual" : String(source.trigger);
  if (!["manual", "cron", "webhook"].includes(triggerRaw)) {
    throw new PipelineYamlError("trigger must be manual, cron or webhook", 1);
  }
  const enabled = source.enabled === undefined || source.enabled === null ? true : source.enabled;
  if (typeof enabled !== "boolean") throw new PipelineYamlError("enabled must be true or false", 1);
  const cron = source.cron === undefined || source.cron === null ? undefined : String(source.cron).trim();
  if (triggerRaw === "cron" && !cron) {
    throw new PipelineYamlError("cron is required when trigger is cron", 1);
  }
  return {
    name: asString(source.name, "name", 1),
    trigger: triggerRaw as CreatePipelineInput["trigger"],
    ...(cron ? { cron } : {}),
    enabled,
    steps: buildSteps(source.steps),
  };
}

export function parsePipelineYaml(text: string): CreatePipelineInput {
  const source = text.trim();
  if (!source) throw new PipelineYamlError("the definition is empty", 1);
  if (source.startsWith("{")) {
    try {
      return fromObject(JSON.parse(source) as unknown);
    } catch (cause) {
      if (cause instanceof PipelineYamlError) throw cause;
      throw new PipelineYamlError(cause instanceof Error ? cause.message : "invalid JSON", 1);
    }
  }

  const lines = tokenize(source);
  const document: Record<string, unknown> = {};
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (!meaningful(line)) {
      index += 1;
      continue;
    }
    if (line.indent !== 0) throw new PipelineYamlError("unexpected indentation", line.number);
    const pair = /^([^:]+):\s*(.*)$/.exec(line.text);
    if (!pair) throw new PipelineYamlError("expected `key: value`", line.number);
    const key = pair[1].trim();
    if (key === "steps") {
      index += 1;
      const items: unknown[] = [];
      while (index < lines.length) {
        const candidate = lines[index];
        if (!meaningful(candidate)) {
          index += 1;
          continue;
        }
        if (candidate.indent === 0) break;
        if (!candidate.text.startsWith("-")) {
          throw new PipelineYamlError("steps must be a list of `- id: ...` items", candidate.number);
        }
        const itemIndent = candidate.indent;
        const parsed = parseStep(lines, index, itemIndent);
        items.push(parsed.step);
        index = parsed.next;
      }
      document.steps = items;
      continue;
    }
    const parsed = readValue(lines, index, line.indent, pair[2]);
    document[key] = parsed.value;
    index = parsed.next;
  }
  return fromObject(document);
}

function scalar(value: string): string {
  return /^[A-Za-z0-9_./ -]+$/.test(value) ? value : JSON.stringify(value);
}

export function pipelineToYaml(pipeline: Pipeline): string {
  const lines = [`name: ${pipeline.name}`, `trigger: ${pipeline.trigger}`];
  if (pipeline.cron) lines.push(`cron: ${scalar(pipeline.cron)}`);
  lines.push(`enabled: ${pipeline.enabled}`, "", "steps:");
  for (const step of pipeline.steps) {
    lines.push(`  - id: ${step.id}`, `    instance: ${step.instance}`);
    const promptLines = step.prompt.replace(/\r\n/g, "\n").replace(/\n$/, "").split("\n");
    if (promptLines.length === 1 && step.prompt.length < 100) {
      lines.push(`    prompt: ${scalar(step.prompt)}`);
    } else {
      lines.push("    prompt: |");
      for (const promptLine of promptLines) lines.push(`      ${promptLine}`);
    }
    if (step.depends_on?.length) lines.push(`    depends_on: [${step.depends_on.join(", ")}]`);
    if (step.retries) lines.push(`    retries: ${step.retries}`);
    if (step.timeout_s) lines.push(`    timeout_s: ${step.timeout_s}`);
    if (step.on_failure) lines.push(`    on_failure: ${step.on_failure}`);
    lines.push("");
  }
  return `${lines.join("\n").trimEnd()}\n`;
}
