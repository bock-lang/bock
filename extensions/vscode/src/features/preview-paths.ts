// Pure path logic for the target-preview feature (`bock.showTargetPreview`).
//
// This module is deliberately free of any `vscode` import so the headless
// Mocha + ts-node unit suite can exercise it directly. `target-preview.ts`
// owns the editor/UI/process side and delegates all path reasoning here.
//
// ── Emitted-tree layout (verified against the real compiler) ───────────────
//
// `bock build -t <target> --source-only` (run from the project root, i.e. the
// directory containing `bock.project`) emits one file per module under
// `build/<target>/`. The per-target layout, mirrored from
// `bock-codegen::generator::derive_output_path` / `module_tree_relpath` and
// confirmed by building a nested-module project with the real binary:
//
// | target | entry module                  | non-entry module `a.b`          |
// |--------|-------------------------------|---------------------------------|
// | js     | source-mirrored: `main.js`    | declared path: `a/b.js`         |
// | ts     | source-mirrored: `main.ts`    | declared path: `a/b.ts`         |
// | python | source-mirrored: `main.py`    | declared path: `a/b.py`         |
// | rust   | always `src/main.rs`          | `src/a/b.rs` (+ wiring files)   |
// | go     | always `main.go`              | flat, dots kept: `a.b.go`       |
//
// "Source-mirrored" means: take the source path relative to the project root,
// drop a leading `src/` component if present, and swap `.bock` for the
// target's extension (`src/utils/parse.bock` → `utils/parse.<ext>`). A module
// with no declared `module` path falls back to its source-mirrored path
// (basename only for Go, whose per-module package is a single flat
// directory).

/** The transpilation targets `bock build -t` accepts, in QuickPick order. */
export const TARGETS = ['js', 'ts', 'python', 'rust', 'go'] as const;

/** One of the five built-in transpilation targets. */
export type Target = (typeof TARGETS)[number];

/** File extension (without dot) the compiler emits for each target. */
export const TARGET_EXTENSIONS: Record<Target, string> = {
  js: 'js',
  ts: 'ts',
  python: 'py',
  rust: 'rs',
  go: 'go',
};

/** VS Code language id used to highlight the emitted file, per target. */
export const TARGET_LANGUAGE_IDS: Record<Target, string> = {
  js: 'javascript',
  ts: 'typescript',
  python: 'python',
  rust: 'rust',
  go: 'go',
};

/**
 * Walk up from `startDir` looking for a directory containing `bock.project`.
 * Returns the project-root directory, or `undefined` when no marker is found
 * up to (and including) the filesystem root.
 *
 * The filesystem is injected as an `exists` predicate so tests can drive the
 * walk over a fake tree. Paths are treated purely lexically: `startDir`
 * should be absolute and normalized (as `path.dirname(document.fsPath)` is).
 */
export function findProjectRoot(
  startDir: string,
  exists: (p: string) => boolean,
  sep = '/',
): string | undefined {
  let dir = startDir;
  // Bounded by path depth: each iteration strips one trailing component.
  for (;;) {
    const marker = dir.endsWith(sep)
      ? `${dir}bock.project`
      : `${dir}${sep}bock.project`;
    if (exists(marker)) return dir;
    const cut = dir.lastIndexOf(sep);
    if (cut < 0) return undefined;
    // Keep the separator for the filesystem root ("/" or "C:\"), drop it
    // elsewhere; stop once the parent step no longer shrinks the path.
    const parent = cut === 0 ? sep : dir.slice(0, cut);
    if (parent === dir) return undefined;
    dir = parent;
  }
}

/**
 * Extract the dotted module path from a Bock source's first `module`
 * declaration (`module utils.parse` → `"utils.parse"`). Returns `undefined`
 * when the file declares no module (the compiler then falls back to a
 * source-mirrored output path, and so do we).
 */
export function parseModuleDecl(source: string): string | undefined {
  for (const raw of source.split(/\r?\n/)) {
    const m = /^\s*module\s+([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\s*$/.exec(
      raw,
    );
    if (m) return m[1];
  }
  return undefined;
}

/** Relative build directory (POSIX form) for a target: `build/<target>`. */
export function buildDirFor(target: Target): string {
  return `build/${target}`;
}

/** Normalize a relative path to POSIX separators and strip `./` prefixes. */
function toPosix(rel: string): string {
  let p = rel.replace(/\\/g, '/');
  while (p.startsWith('./')) p = p.slice(2);
  return p;
}

/**
 * The source-mirrored output stem: source path relative to the project root,
 * leading `src/` dropped, `.bock` extension removed.
 * (`src/utils/parse.bock` → `utils/parse`.)
 */
function sourceMirroredStem(relSourcePath: string): string {
  let p = toPosix(relSourcePath);
  if (p.startsWith('src/')) p = p.slice('src/'.length);
  return p.replace(/\.bock$/, '');
}

/**
 * Ordered candidate paths (POSIX, relative to `build/<target>/`) where the
 * compiler may have emitted the module that lives at `relSourcePath`
 * (relative to the project root). `modulePath` is the source's declared
 * dotted module path, when known.
 *
 * Order matters: the declared-module-path mapping is what the compiler uses
 * for every non-entry module, so it comes first; the source-mirrored path
 * covers entry modules and undeclared files; the fixed entry names
 * (`src/main.rs` / `main.go` / `main.<ext>`) come last, and only for sources
 * whose stem is `main`, so a non-entry module can never silently map onto
 * the project's entry file.
 */
export function emittedCandidates(
  relSourcePath: string,
  target: Target,
  modulePath?: string,
): string[] {
  const ext = TARGET_EXTENSIONS[target];
  const stem = sourceMirroredStem(relSourcePath);
  const mirrored = `${stem}.${ext}`;
  const moduleTree = modulePath
    ? `${modulePath.split('.').join('/')}.${ext}`
    : undefined;
  const baseName = stem.split('/').pop() ?? stem;
  const isMainStem = baseName === 'main';

  const out: string[] = [];
  const push = (c: string | undefined): void => {
    if (c && !out.includes(c)) out.push(c);
  };

  switch (target) {
    case 'js':
    case 'ts':
    case 'python':
      push(moduleTree);
      push(mirrored);
      if (isMainStem) push(`main.${ext}`);
      break;
    case 'rust':
      // The whole emitted crate lives under an extra `src/` inside
      // `build/rust/`; the entry module is always `src/main.rs`.
      push(moduleTree && `src/${moduleTree}`);
      push(`src/${mirrored}`);
      if (isMainStem) push('src/main.rs');
      break;
    case 'go':
      // Flat single-package directory: declared dotted path kept verbatim
      // (`a.b` → `a.b.go`); fallback is the bare file name.
      push(modulePath && `${modulePath}.go`);
      push(`${baseName}.go`);
      if (isMainStem) push('main.go');
      break;
  }
  return out;
}

/**
 * Map an active source file to its emitted file inside an actual build tree.
 *
 * @param relSourcePath source path relative to the project root
 * @param target the build target
 * @param emittedFiles listing of the emitted tree, as paths relative to
 *   `build/<target>/` (either separator style)
 * @param modulePath the source's declared dotted module path, if any
 * @returns the matching entry from `emittedFiles` (verbatim, so the caller
 *   can join it back onto the real directory), or `undefined` when no
 *   candidate is present in the listing
 */
export function resolveEmittedFile(
  relSourcePath: string,
  target: Target,
  emittedFiles: readonly string[],
  modulePath?: string,
): string | undefined {
  const normalized = new Map<string, string>();
  for (const f of emittedFiles) {
    const key = toPosix(f);
    if (!normalized.has(key)) normalized.set(key, f);
  }
  for (const candidate of emittedCandidates(relSourcePath, target, modulePath)) {
    const hit = normalized.get(candidate);
    if (hit !== undefined) return hit;
  }
  return undefined;
}
