// Pure line-level editing of the `[strictness]` table in `bock.project`.
//
// `bock.project` is TOML, but this module deliberately avoids a TOML
// library: a parse → mutate → serialize round-trip would normalize away the
// user's formatting and drop comments. Instead `setStrictness` performs a
// line-level edit that mirrors the compiler's own rewrite logic
// (`bock-cli/src/promote.rs::update_strictness`), preserving every line it
// does not have to touch — including comments and the spacing/trailing
// comment on the `default = …` line itself.
//
// This module is vscode-free so the headless unit suite can exercise it;
// `strictness.ts` owns the status-bar item, QuickPick, and file IO.

/** The §1.4 strictness ladder, lowest to highest. */
export const STRICTNESS_LEVELS = ['sketch', 'development', 'production'] as const;

/** One rung of the §1.4 strictness ladder. */
export type StrictnessLevel = (typeof STRICTNESS_LEVELS)[number];

/** Type guard for {@link StrictnessLevel}. */
export function isStrictnessLevel(value: string): value is StrictnessLevel {
  return (STRICTNESS_LEVELS as readonly string[]).includes(value);
}

// `[strictness]` header line, allowing surrounding whitespace and a trailing
// comment. Any *other* `[...]`-header line ends the section.
const STRICTNESS_HEADER_RE = /^\s*\[strictness\]\s*(?:#.*)?$/;
const ANY_HEADER_RE = /^\s*\[/;

// `default = <value>` inside the `[strictness]` table. Captures:
//   1 indentation   2 `default` + spacing around `=`   3 the value token
//   4 trailing whitespace + optional comment
// The value token is a quoted string or a bare token; `[^#\s]` keeps a
// trailing `# comment` out of the value.
const DEFAULT_KEY_RE =
  /^(\s*)(default\s*=\s*)("(?:[^"\\]|\\.)*"|'[^']*'|[^#\s]+)(\s*(?:#.*)?)$/;

/**
 * Read the project's declared strictness from `bock.project` text: the
 * `default` key of the `[strictness]` table. Mirrors the compiler
 * (`promote.rs::read_strictness`): a missing table, missing key, or
 * unrecognized value all fall back to `sketch`, the spec's default.
 */
export function getStrictness(tomlText: string): StrictnessLevel {
  let inSection = false;
  for (const line of tomlText.split(/\r?\n/)) {
    if (ANY_HEADER_RE.test(line)) {
      inSection = STRICTNESS_HEADER_RE.test(line);
      continue;
    }
    if (!inSection) continue;
    const m = DEFAULT_KEY_RE.exec(line);
    if (m) {
      const value = unquote(m[3]);
      if (isStrictnessLevel(value)) return value;
    }
  }
  return 'sketch';
}

/**
 * Return `tomlText` with the `[strictness]` table's `default` key set to
 * `level`, preserving all other lines, comments, indentation, and the
 * original newline style/trailing newline. Handles, in order:
 *
 * 1. key present in `[strictness]` → rewrite the value in place (indent,
 *    `=` spacing, and any trailing comment survive);
 * 2. table present but key absent → insert `default = "<level>"` directly
 *    after the table header;
 * 3. table absent → append a `[strictness]` table (blank-line separated)
 *    at the end of the file.
 *
 * Idempotent: applying the same level twice yields identical text.
 */
export function setStrictness(
  tomlText: string,
  level: StrictnessLevel,
): string {
  const eol = tomlText.includes('\r\n') ? '\r\n' : '\n';
  const hadTrailingNewline = /\r?\n$/.test(tomlText);
  // Split on the EOL so lines keep no terminators; a trailing newline
  // yields one empty final element, dropped here and restored at the end.
  const lines = tomlText.split(/\r?\n/);
  if (hadTrailingNewline) lines.pop();

  let inSection = false;
  let headerIdx: number | undefined;
  let rewritten = false;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (ANY_HEADER_RE.test(line)) {
      inSection = STRICTNESS_HEADER_RE.test(line);
      if (inSection && headerIdx === undefined) headerIdx = i;
      continue;
    }
    if (!inSection) continue;
    const m = DEFAULT_KEY_RE.exec(line);
    if (m) {
      lines[i] = `${m[1]}${m[2]}"${level}"${m[4]}`;
      rewritten = true;
      break;
    }
  }

  if (!rewritten) {
    if (headerIdx !== undefined) {
      lines.splice(headerIdx + 1, 0, `default = "${level}"`);
    } else {
      // Entirely empty input: don't leave a stray blank first line.
      if (lines.length === 1 && lines[0] === '') lines.pop();
      if (lines.length > 0 && lines[lines.length - 1].trim() !== '') {
        lines.push('');
      }
      lines.push('[strictness]', `default = "${level}"`);
    }
  }

  let out = lines.join(eol);
  if (hadTrailingNewline) out += eol;
  return out;
}

/** Strip one layer of matching single or double quotes, if present. */
function unquote(token: string): string {
  if (
    token.length >= 2 &&
    ((token.startsWith('"') && token.endsWith('"')) ||
      (token.startsWith("'") && token.endsWith("'")))
  ) {
    return token.slice(1, -1);
  }
  return token;
}
