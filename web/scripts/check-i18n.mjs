#!/usr/bin/env node
/**
 * Guard against hardcoded interface copy.
 *
 * Scans `src/pages` and `src/components` (the shadcn registry under
 * `components/ui` renders no app copy and is skipped) for string literals that
 * look like human text:
 *
 *   - JSX text nodes:        <p>Sign out</p>, a lone line between tags
 *   - copy props:            placeholder="…", title="…", aria-label="…", …
 *   - user-facing calls:     toast.error("…"), setFieldError("…"), …
 *   - ApiError messages:     new ApiError(401, code, "…")
 *   - interpolated copy:     toast.success(`Instance ${id} deleted.`)
 *
 * No dependencies: plain source scanning, comment- and string-aware enough for
 * this codebase. Strings inside `t(...)` are ignored.
 *
 * Usage:
 *   node scripts/check-i18n.mjs            # human report, exit 1 on findings
 *   node scripts/check-i18n.mjs --strict   # also fail on dictionary drift
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const SRC = join(ROOT, "src");

/**
 * Literals that are deliberately not translated: product names, protocol and
 * unit words. Keep this list short; each entry needs a reason next to it.
 */
const ALLOWED = new Set([
  "fleett", // product name
  "FLEETT_TOKEN", // env var shown as the token field placeholder
  "EN",
  "FR",
  "LAN", // network vocabulary
  "HTTP",
  "HTTPS",
  "API",
  "JSON",
  "YAML",
  "Git",
  "GitOps",
  "Docker",
  "Docker Compose",
  "SKILL.md",
  "SOUL.md",
  "config.yaml",
  "sessionStorage",
  "UTC",
  "OK",
  "CPU",
  "RAM",
  "ID",
  "URL",
  "MCP",
  "OIDC",
  "SSO",
  "TLS",
  "SSH",
  "CSV",
  "TTL",
  "USD",
  "Authentik",
  "Hermes",
  "Telegram",
  "Discord",
  "OpenAI",
  "Anthropic",
  "Google",
  "Markdown",
  "PDF",
  "ZIP",
  "MiB",
  "KiB",
  "GiB",
  "KB",
  "MB",
  "GB",
  "ms",
  "Esc",
  "Regex",
  "Cron",
  "Content-Security-Policy",
  "Referrer-Policy",
  "Permissions-Policy",
  "ls /opt/data",
  "d",
  "h",
  "m",
  "s",
]);

/** Props whose string value is interface copy and must go through `t()`. */
const COPY_PROPS = [
  "placeholder",
  "title",
  "aria-label",
  "aria-description",
  "alt",
  "label",
  "confirmLabel",
  "description",
  "emptyLabel",
  "hint",
];

/** Calls whose string argument is shown to a human. */
const COPY_CALLS = [
  "toast.success",
  "toast.error",
  "toast.info",
  "toast.warning",
  "toast.message",
  "toast.loading",
  "setError",
  "setFieldError",
  "window.confirm",
  "window.alert",
];

/** Human-looking text: words, a sentence shape, no code syntax. */
function looksLikeCopy(raw) {
  const text = raw.trim();
  if (text.length < 2) return false;
  if (ALLOWED.has(text)) return false;
  if (!/[A-Za-zÀ-ÿ]/.test(text)) return false;

  if (/^[@#./~]/.test(text)) return false; // path, command, selector
  if (/^[a-z0-9_.:/-]+$/.test(text)) return false; // identifier, css token
  if (/^[A-Z0-9_]+$/.test(text)) return false; // constant
  if (/^[\w-]+(\.[\w-]+)+$/.test(text)) return false; // file name / host
  if (/^[0-9.%\s]+$/.test(text)) return false;
  if (/^[a-z-]+(\s+[a-z-]+)*$/.test(text)) return false; // css-ish
  if (/^\$\{/.test(text)) return false;

  // JSX boundaries can glue punctuation to the sentence.
  const core = text.replace(/^[\s→·|,.;:!?()[\]-]+|[\s→·|,.;:!?()[\]-]+$/g, "");
  if (!core) return false;

  // Code punctuation between two operators is not a text node.
  if (/[()?!;]|&&|\|\|/.test(core)) return false;

  // At least a word of two letters, and either a space or a capitalized word.
  return /\s/.test(core) || /^[A-ZÀ-Ý][a-zà-ÿ]/.test(core);
}

function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const stat = statSync(full);
    if (stat.isDirectory()) {
      if (entry === "ui") continue; // shadcn registry
      out.push(...walk(full));
    } else if (/\.(tsx?|jsx?)$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

const findings = [];
function report(file, line, kind, text) {
  findings.push({ file: relative(ROOT, file), line, kind, text: text.trim() });
}

/**
 * Blank out comments and string contents while keeping quotes, so structural
 * regexes cannot match inside a literal. Line count is preserved.
 */
function maskComments(source) {
  let out = "";
  let state = "code"; // code | line | block | single | double | template
  for (let i = 0; i < source.length; i += 1) {
    const char = source[i];
    const next = source[i + 1];
    if (state === "code") {
      if (char === "/" && next === "/") {
        state = "line";
        out += "  ";
        i += 1;
      } else if (char === "/" && next === "*") {
        state = "block";
        out += "  ";
        i += 1;
      } else if (char === '"') {
        state = "double";
        out += char;
      } else if (char === "'") {
        state = "single";
        out += char;
      } else if (char === "`") {
        state = "template";
        out += char;
      } else {
        out += char;
      }
      continue;
    }
    if (state === "line") {
      if (char === "\n") {
        state = "code";
        out += char;
      } else out += " ";
      continue;
    }
    if (state === "block") {
      if (char === "*" && next === "/") {
        state = "code";
        out += "  ";
        i += 1;
      } else out += char === "\n" ? "\n" : " ";
      continue;
    }
    if (state === "double" || state === "single") {
      const quote = state === "double" ? '"' : "'";
      if (char === "\\") {
        out += "  ";
        i += 1;
      } else if (char === quote) {
        state = "code";
        out += char;
      } else out += char === "\n" ? "\n" : " ";
      continue;
    }
    if (state === "template") {
      if (char === "\\") {
        out += "  ";
        i += 1;
      } else if (char === "`") {
        state = "code";
        out += char;
      } else out += char === "\n" ? "\n" : " ";
    }
  }
  return out;
}

/** Value of a quoted literal starting at `start` (index of the opening quote). */
function readQuoted(line, start) {
  const quote = line[start];
  let value = "";
  for (let i = start + 1; i < line.length; i += 1) {
    const char = line[i];
    if (char === "\\") {
      value += line[i + 1] ?? "";
      i += 1;
      continue;
    }
    if (char === quote) return { value, end: i };
    value += char;
  }
  return null;
}

function scanFile(file) {
  const source = readFileSync(file, "utf8");
  const code = maskComments(source);
  const lines = source.split("\n");
  const maskedLines = code.split("\n");

  lines.forEach((raw, index) => {
    const lineNo = index + 1;
    const masked = maskedLines[index] ?? "";

    // 1. Copy props with a literal value: placeholder="…", title='…'.
    for (const prop of COPY_PROPS) {
      const re = new RegExp(`\\b${prop.replace("-", "\\-")}\\s*=\\s*`, "g");
      let match;
      while ((match = re.exec(masked)) !== null) {
        const at = match.index + match[0].length;
        const char = masked[at];
        if (char === '"' || char === "'") {
          const literal = readQuoted(raw, at);
          if (literal && looksLikeCopy(literal.value)) {
            report(file, lineNo, `${prop}=`, literal.value);
          }
        }
      }
    }

    // 2. Copy calls: toast.error("…"), setFieldError("…"), …
    for (const call of COPY_CALLS) {
      const re = new RegExp(`${call.replace(".", "\\.")}\\(\\s*`, "g");
      let match;
      while ((match = re.exec(masked)) !== null) {
        const at = match.index + match[0].length;
        const char = masked[at];
        if (char === '"' || char === "'") {
          const literal = readQuoted(raw, at);
          if (literal && looksLikeCopy(literal.value)) {
            report(file, lineNo, `${call}()`, literal.value);
          }
        }
      }
    }

    // 3. ApiError messages: the literal that reads like a sentence.
    if (/\bnew\s+ApiError\(/.test(raw)) {
      for (const literal of raw.matchAll(/"([^"]{4,})"/g)) {
        if (looksLikeCopy(literal[1])) report(file, lineNo, "new ApiError()", literal[1]);
      }
    }

    // 4. JSX text on one line: >Sign out</Button>
    for (const node of raw.matchAll(/>([^<>{}"'`]+)</g)) {
      const text = node[1];
      if (looksLikeCopy(text)) report(file, lineNo, "jsx text", text);
    }

    // 5. A lone line of copy between tags (prettier layout):
    //      <Button …>
    //        Sign out
    //      </Button>
    const trimmed = raw.trim();
    if (
      trimmed &&
      !/[<>{}=;:[\]()]/.test(trimmed) &&
      !trimmed.startsWith('"') &&
      !trimmed.startsWith("'") &&
      !trimmed.startsWith("//") &&
      !trimmed.startsWith("*") &&
      !trimmed.endsWith(",") &&
      looksLikeCopy(trimmed)
    ) {
      // Previous non-empty line must close a JSX tag, otherwise it is code.
      let previous = "";
      for (let back = index - 1; back >= 0; back -= 1) {
        const candidate = lines[back].trim();
        if (candidate) {
          previous = candidate;
          break;
        }
      }
      if (/[>]$/.test(previous) || /^<[A-Za-z]/.test(previous)) {
        report(file, lineNo, "jsx text", trimmed);
      }
    }
  });
}

function dictionaryKeys(path) {
  const source = readFileSync(path, "utf8");
  const keys = [];
  const re = /^\s*"([^"]+)"\s*:/gm;
  let match;
  while ((match = re.exec(source)) !== null) keys.push(match[1]);
  return keys;
}

function main() {
  const files = walk(join(SRC, "pages")).concat(walk(join(SRC, "components")));
  for (const file of files) scanFile(file);

  findings.sort((left, right) =>
    `${left.file}:${String(left.line).padStart(5, "0")}`.localeCompare(
      `${right.file}:${String(right.line).padStart(5, "0")}`,
    ),
  );

  for (const finding of findings) {
    console.log(
      `${finding.file}:${finding.line}: ${finding.kind} ${JSON.stringify(finding.text)}`,
    );
  }

  const en = dictionaryKeys(join(SRC, "locales", "en.ts"));
  const fr = dictionaryKeys(join(SRC, "locales", "fr.ts"));
  const enSet = new Set(en);
  const frSet = new Set(fr);
  const missingFr = en.filter((key) => !frSet.has(key));
  const missingEn = fr.filter((key) => !enSet.has(key));
  const duplicates = en.filter((key, index) => en.indexOf(key) !== index);

  if (missingFr.length || missingEn.length || duplicates.length) {
    console.log("\nDictionary drift:");
    for (const key of missingFr) console.log(`  missing in fr.ts: ${key}`);
    for (const key of missingEn) console.log(`  missing in en.ts: ${key}`);
    for (const key of duplicates) console.log(`  duplicate in en.ts: ${key}`);
  }

  if (findings.length === 0) {
    console.log(`i18n: no hardcoded copy in ${files.length} files (${en.length} keys).`);
  } else {
    console.log(`\ni18n: ${findings.length} suspicious literal(s) in ${files.length} files.`);
    console.log("Wrap copy in t(...) or add a justified entry to ALLOWED in scripts/check-i18n.mjs.");
  }

  const drift = missingFr.length + missingEn.length + duplicates.length;
  const strict = process.argv.includes("--strict");
  process.exit(findings.length > 0 || (strict && drift > 0) ? 1 : 0);
}

main();
