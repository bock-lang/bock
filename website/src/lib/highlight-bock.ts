// Bock-flavored syntax highlighting (rule-based, not a real lexer).
// Ported from CodeBlock.jsx - runs at build time. Returns HTML string.
//
// Highlights: keywords, types, strings, comments, numbers, the |> pipe operator.
// Token CSS classes (tok-kw, tok-ty, tok-st, tok-co, tok-fn, tok-pi, tok-pu,
// tok-nu) are styled in global.css.

const BOCK_KW = new Set([
  'module', 'public', 'fn', 'record', 'effect', 'with',
  'if', 'else', 'match', 'let', 'return', 'true', 'false',
]);

const BOCK_TY_PRIMITIVE = new Set([
  'String', 'Int', 'Float', 'Bool', 'Void',
  'List', 'Optional', 'Document', 'Logger', 'Storage',
]);

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

function span(cls: string, text: string): string {
  const escaped = escapeHtml(text);
  return cls ? `<span class="tok-${cls}">${escaped}</span>` : escaped;
}

export function highlightBock(src: string): string {
  const out: string[] = [];
  const lines = src.split('\n');
  lines.forEach((line, i) => {
    if (i > 0) out.push('\n');
    let rest = line;
    while (rest.length) {
      // comment - consumes rest of line
      const cmt = rest.match(/^\/\/.*/);
      if (cmt) { out.push(span('co', cmt[0])); break; }
      // string literal (handles \\. escapes; template ${…} segments fall under [^"\\] runs)
      const str = rest.match(/^"(?:[^"\\]|\\.)*"/);
      if (str) { out.push(span('st', str[0])); rest = rest.slice(str[0].length); continue; }
      // pipe operator
      if (rest.startsWith('|>')) { out.push(span('pi', '|>')); rest = rest.slice(2); continue; }
      // number
      const num = rest.match(/^\d+(\.\d+)?/);
      if (num) { out.push(span('nu', num[0])); rest = rest.slice(num[0].length); continue; }
      // identifier
      const id = rest.match(/^[A-Za-z_][A-Za-z0-9_]*/);
      if (id) {
        const w = id[0];
        if (BOCK_KW.has(w)) out.push(span('kw', w));
        else if (BOCK_TY_PRIMITIVE.has(w) || /^[A-Z]/.test(w)) out.push(span('ty', w));
        else out.push(span('fn', w));
        rest = rest.slice(w.length);
        continue;
      }
      // punct
      const pu = rest.match(/^[{}()\[\],.:;<>=+\-*/&|]+/);
      if (pu) { out.push(span('pu', pu[0])); rest = rest.slice(pu[0].length); continue; }
      // whitespace
      const ws = rest.match(/^[ \t]+/);
      if (ws) { out.push(span('', ws[0])); rest = rest.slice(ws[0].length); continue; }
      // fallback: single char
      out.push(span('', rest[0]));
      rest = rest.slice(1);
    }
  });
  return out.join('');
}

export function escapeCode(src: string): string {
  return escapeHtml(src);
}
