// Pure annotation-scanning logic for the annotation insight tree (F1.5.5).
//
// This module is deliberately free of the heavier extension dependencies
// (`vscode-languageclient`, the webview/markdown chain). It imports `vscode`
// only for the `Uri` type, so it can be exercised by the headless
// Mocha + ts-node unit suite, whose CommonJS resolver can't follow the
// `vscode-languageclient/node` package `exports` subpath. `annotations.ts`
// re-exports `scanText` / `AnnotationUsage` from here.

import * as vscode from 'vscode';

/** A single top-level annotation occurrence found in a `.bock` file. */
export interface AnnotationUsage {
  /** Name without the leading `@`. */
  name: string;
  /** Raw parameter text, empty if the annotation has no arguments. */
  params: string;
  /** Workspace file URI where this usage lives. */
  uri: vscode.Uri;
  /** Zero-based line number of the `@name` token. */
  line: number;
  /** Zero-based column of the `@` character. */
  column: number;
}

// Matches a top-level annotation token at the start of a (possibly
// indented) line. We intentionally stop at the first `(` on that line
// and capture the text up to the first unnested `)` when the caller
// asks for full parameters — multi-line parameter lists (e.g. `@context`
// with a triple-quoted string) may have no closing paren on the same
// line, in which case the params field is left empty.
const ANNOTATION_RE = /^[\t ]*@([A-Za-z_][A-Za-z0-9_]*)\b/;

/** Parse annotation usages out of a single file's text. Exported for tests. */
export function scanText(uri: vscode.Uri, text: string): AnnotationUsage[] {
  const out: AnnotationUsage[] = [];
  const lines = text.split(/\r?\n/);
  let inTripleString = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const startedInString = inTripleString;
    inTripleString = nextTripleState(line, inTripleString);
    // Skip the line if it opened fully inside a triple-quoted string.
    // Annotations buried inside a `@context("""...""")` body (e.g.
    // `@intent:`) are documentation markers, not top-level annotations.
    if (startedInString) continue;

    const match = ANNOTATION_RE.exec(line);
    if (!match) continue;
    const name = match[1];
    const column = line.indexOf('@');
    const params = extractParams(line, column);
    out.push({ name, params, uri, line: i, column });
  }
  return out;
}

/**
 * Advance the cross-line "inside a triple-quoted string" state by one line.
 *
 * The previous implementation simply counted every `"""` substring on the
 * line and toggled on an odd count. That is not string/comment-aware: a
 * `"""` sitting inside a `//` line comment or inside an ordinary `"…"`
 * string would flip the state and suppress every annotation on the
 * following lines (a false-negative). This walks the line left-to-right,
 * ignoring `"""` that appears in a line comment or inside a single-line
 * `"…"` string, and only toggles on genuine triple-quote delimiters.
 *
 * Ordinary strings are treated as single-line (the model the rest of the
 * scanner assumes): an unescaped `"` that is not the start of a `"""`
 * opens a string that runs to the next unescaped `"` on the same line.
 */
export function nextTripleState(line: string, inTriple: boolean): boolean {
  if (inTriple) {
    // Inside a triple-quoted string: only a closing `"""` matters; nothing
    // else on the line (comments, quotes) is significant.
    const close = line.indexOf('"""');
    if (close === -1) return true;
    // Resume scanning the remainder of the line after the closing `"""`.
    return nextTripleState(line.slice(close + 3), false);
  }

  let inString = false; // inside an ordinary "…" string
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (inString) {
      if (c === '\\') {
        i++; // skip the escaped character
      } else if (c === '"') {
        inString = false;
      }
      continue;
    }
    // Outside any string.
    if (c === '/' && line[i + 1] === '/') {
      // Line comment — nothing after it can open a triple string.
      return false;
    }
    if (c === '"') {
      if (line[i + 1] === '"' && line[i + 2] === '"') {
        // Genuine triple-quote delimiter: the rest of the line is now
        // inside a triple string until a closing `"""` (handled by the
        // recursive inTriple branch above).
        return nextTripleState(line.slice(i + 3), true);
      }
      inString = true;
    }
  }
  return false;
}

function extractParams(line: string, atColumn: number): string {
  const open = line.indexOf('(', atColumn);
  if (open === -1) return '';
  let depth = 1;
  for (let i = open + 1; i < line.length; i++) {
    const c = line[i];
    if (c === '(') depth++;
    else if (c === ')') {
      depth--;
      if (depth === 0) return line.slice(open + 1, i);
    }
  }
  // Unclosed on this line (e.g. `@context("""` spanning multiple lines).
  return line.slice(open + 1).trimEnd();
}
