// Small pure string helpers shared across feature modules.
//
// These have no `vscode` (or any other) dependency, so they can be imported
// freely by both the live extension code and the headless unit suite.

/**
 * Trim `s` and clamp it to at most `n` characters, appending a single-glyph
 * ellipsis (`…`) when truncation occurs.
 *
 * The ellipsis itself counts toward the limit: a string longer than `n` is
 * cut to `n - 1` characters plus the `…`, so the result is never longer than
 * `n`. A nullish input is treated as the empty string.
 *
 * @param s the source string (nullish is coerced to `''`)
 * @param n the maximum length of the returned string, including the ellipsis
 */
export function truncate(s: string, n: number): string {
  const trimmed = (s ?? '').trim();
  return trimmed.length > n ? `${trimmed.slice(0, n - 1)}…` : trimmed;
}
