// Thin wrapper around `marked` for rendering spec markdown and hover bodies.
// Centralized so feature modules don't each configure marked independently.

import { marked } from 'marked';

marked.setOptions({
  gfm: true,
  breaks: false,
});

export function renderMarkdown(source: string): string {
  return marked.parse(source, { async: false }) as string;
}
