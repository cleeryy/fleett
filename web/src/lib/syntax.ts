/**
 * Small, dependency-free colouring for the configuration files the console can
 * edit (JSON, YAML, TOML). It is a line-at-a-time heuristic, not a parser: it
 * only needs to separate keys, strings, numbers, literals, comments and
 * punctuation so a wall of YAML reads as structure. Anything it does not
 * recognise stays plain text, so a wrong guess costs nothing.
 */

export type SyntaxLanguage = "json" | "yaml" | "toml";

export type SyntaxKind =
  | "plain"
  | "key"
  | "string"
  | "number"
  | "literal"
  | "comment"
  | "punct";

export interface SyntaxToken {
  text: string;
  kind: SyntaxKind;
}

/** Language of a path, or `null` when this file has no colouring. */
export function syntaxLanguage(path: string): SyntaxLanguage | null {
  const name = path.slice(path.lastIndexOf("/") + 1).toLowerCase();
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return null;
  const extension = name.slice(dot + 1);
  if (extension === "json") return "json";
  if (extension === "yaml" || extension === "yml") return "yaml";
  if (extension === "toml") return "toml";
  return null;
}

export function isMarkdownPath(path: string): boolean {
  const name = path.slice(path.lastIndexOf("/") + 1).toLowerCase();
  return name.endsWith(".md") || name.endsWith(".markdown");
}

export function tokenizeLine(language: SyntaxLanguage, line: string): SyntaxToken[] {
  if (language === "json") return tokenizeJson(line);
  if (language === "yaml") return tokenizeYaml(line);
  return tokenizeToml(line);
}

const NUMBER_RE = /^-?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?/;
const JSON_LITERAL_RE = /^(?:true|false|null)\b/;
const YAML_LITERAL_RE = /^(?:(?:true|false|null|yes|no|on|off)\b|~)/i;

function pushToken(tokens: SyntaxToken[], text: string, kind: SyntaxKind): void {
  if (!text) return;
  const last = tokens[tokens.length - 1];
  if (last && last.kind === kind) last.text += text;
  else tokens.push({ text, kind });
}

/** Length of the quoted run starting at `text[0]`, closing quote included. */
function quotedLength(text: string): number {
  const quote = text[0];
  let index = 1;
  while (index < text.length) {
    if (text[index] === "\\" && quote === '"') {
      index += 2;
      continue;
    }
    if (text[index] === quote) return index + 1;
    index += 1;
  }
  return text.length;
}

/** Index of the `#` that starts a comment, ignoring `#` inside quotes. */
function commentIndex(text: string): number {
  let quote: string | null = null;
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index];
    if (quote) {
      if (char === "\\" && quote === '"') {
        index += 1;
        continue;
      }
      if (char === quote) quote = null;
      continue;
    }
    if (char === '"' || char === "'") {
      quote = char;
      continue;
    }
    if (char === "#" && (index === 0 || /\s/.test(text[index - 1]))) return index;
  }
  return -1;
}

function scanValues(
  tokens: SyntaxToken[],
  text: string,
  language: SyntaxLanguage,
): void {
  let index = 0;
  while (index < text.length) {
    const rest = text.slice(index);
    const char = rest[0];

    if (language === "json" && (rest.startsWith("//") || rest.startsWith("/*"))) {
      pushToken(tokens, rest, "comment");
      return;
    }
    if (char === '"' || char === "'") {
      const length = quotedLength(rest);
      pushToken(tokens, rest.slice(0, length), "string");
      index += length;
      continue;
    }
    if ("{}[],:=".includes(char)) {
      pushToken(tokens, char, "punct");
      index += 1;
      continue;
    }
    const number = NUMBER_RE.exec(rest);
    if (number) {
      pushToken(tokens, number[0], "number");
      index += number[0].length;
      continue;
    }
    const literal = (language === "yaml" ? YAML_LITERAL_RE : JSON_LITERAL_RE).exec(rest);
    if (literal) {
      pushToken(tokens, literal[0], "literal");
      index += literal[0].length;
      continue;
    }
    pushToken(tokens, char, "plain");
    index += 1;
  }
}

function tokenizeJson(line: string): SyntaxToken[] {
  const tokens: SyntaxToken[] = [];
  let index = 0;
  while (index < line.length) {
    const rest = line.slice(index);
    if (rest.startsWith("//")) {
      pushToken(tokens, rest, "comment");
      break;
    }
    if (rest.startsWith("/*")) {
      const end = rest.indexOf("*/", 2);
      const stop = end === -1 ? rest.length : end + 2;
      pushToken(tokens, rest.slice(0, stop), "comment");
      index += stop;
      continue;
    }
    const char = rest[0];
    if (char === '"') {
      const length = quotedLength(rest);
      // A string followed by `:` is a member name, not a value.
      pushToken(tokens, rest.slice(0, length), /^\s*:/.test(rest.slice(length)) ? "key" : "string");
      index += length;
      continue;
    }
    if ("{}[],:".includes(char)) {
      pushToken(tokens, char, "punct");
      index += 1;
      continue;
    }
    const number = NUMBER_RE.exec(rest);
    if (number) {
      pushToken(tokens, number[0], "number");
      index += number[0].length;
      continue;
    }
    const literal = JSON_LITERAL_RE.exec(rest);
    if (literal) {
      pushToken(tokens, literal[0], "literal");
      index += literal[0].length;
      continue;
    }
    pushToken(tokens, char, "plain");
    index += 1;
  }
  return tokens;
}

function tokenizeYaml(line: string): SyntaxToken[] {
  const tokens: SyntaxToken[] = [];
  if (/^\s*(---|\.\.\.)\s*$/.test(line)) {
    pushToken(tokens, line, "punct");
    return tokens;
  }
  const hash = commentIndex(line);
  const body = hash === -1 ? line : line.slice(0, hash);
  const tail = hash === -1 ? "" : line.slice(hash);

  // `key:`, `- key:` and nested indentation; a quoted key stays a string.
  const key = /^(\s*(?:-\s+)?)([^\s#'"\[][^:#]*?)(:)(?=\s|$)/.exec(body);
  if (key) {
    pushToken(tokens, key[1], "plain");
    pushToken(tokens, key[2], "key");
    pushToken(tokens, key[3], "punct");
    scanValues(tokens, body.slice(key[0].length), "yaml");
  } else {
    scanValues(tokens, body, "yaml");
  }
  if (tail) pushToken(tokens, tail, "comment");
  return tokens;
}

function tokenizeToml(line: string): SyntaxToken[] {
  const tokens: SyntaxToken[] = [];
  const hash = commentIndex(line);
  const body = hash === -1 ? line : line.slice(0, hash);
  const tail = hash === -1 ? "" : line.slice(hash);

  const table = /^(\s*)(\[\[?[^\]]*\]\]?)(\s*)$/.exec(body);
  if (table) {
    pushToken(tokens, table[1], "plain");
    pushToken(tokens, table[2], "key");
    pushToken(tokens, table[3], "plain");
    if (tail) pushToken(tokens, tail, "comment");
    return tokens;
  }

  const key = /^(\s*)((?:[A-Za-z0-9_-]+|"[^"]*")(?:\s*\.\s*(?:[A-Za-z0-9_-]+|"[^"]*"))*)(\s*=\s*)/.exec(
    body,
  );
  if (key) {
    pushToken(tokens, key[1], "plain");
    pushToken(tokens, key[2], "key");
    pushToken(tokens, key[3], "punct");
    scanValues(tokens, body.slice(key[0].length), "toml");
  } else {
    scanValues(tokens, body, "toml");
  }
  if (tail) pushToken(tokens, tail, "comment");
  return tokens;
}
