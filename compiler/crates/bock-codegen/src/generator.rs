//! Code generator trait and output types.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bock_air::{AIRNode, AirArg, EnumVariantPayload, NodeKind};
use bock_types::AIRModule;

use crate::error::CodegenError;
use crate::profile::TargetProfile;

// ─── GeneratedCode ───────────────────────────────────────────────────────────

/// Output from code generation — consistent across all targets.
#[derive(Debug, Clone)]
pub struct GeneratedCode {
    /// Generated output files (path + content + per-file source map).
    pub files: Vec<OutputFile>,
}

/// A single generated output file.
#[derive(Debug, Clone)]
pub struct OutputFile {
    /// Relative path for the output file.
    pub path: PathBuf,
    /// Generated source code content.
    pub content: String,
    /// Source map for this file's content (optional). Each generated file
    /// owns its own map — multi-file builds produce one map per file.
    pub source_map: Option<SourceMap>,
}

/// Derive the output path for a generated file from its source `.bock` path.
///
/// Per spec §20.6.1, a source file at `src/<path>.bock` produces output at
/// `<path>.<ext>` (relative to the target build directory). Sources outside
/// `src/` keep their full path. The returned `PathBuf` is always relative —
/// callers prepend `build/<target>/`.
///
/// - `src/main.bock` → `main.<ext>`
/// - `src/utils/parse.bock` → `utils/parse.<ext>`
/// - `main.bock` → `main.<ext>` (no `src/` prefix to strip)
///
/// Leading `./` and any other curdir components are normalized away before
/// stripping, so the source path can be supplied either bare or with a
/// `./` prefix as produced by directory traversal.
#[must_use]
pub fn derive_output_path(source_path: &Path, target: &TargetProfile) -> PathBuf {
    use std::path::Component;

    let mut comps: Vec<&std::ffi::OsStr> = source_path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();

    if comps.first().and_then(|c| c.to_str()) == Some("src") {
        comps.remove(0);
    }

    let stripped: PathBuf = comps.iter().collect();
    stripped.with_extension(&target.conventions.file_extension)
}

/// Maps AIR source spans to generated code spans.
///
/// Populated by JS/TS code generators with pointwise mappings from generated
/// `(line, col)` back to source `(line, col)`. For other targets, only the
/// legacy `entries` list (AIR node id → target byte range) is populated.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    /// Legacy entries keyed by AIR node id (present for all targets).
    pub entries: Vec<SourceMapEntry>,
    /// Pointwise position mappings from generated code to source.
    pub mappings: Vec<SourceMapping>,
    /// File name (no directory) this map refers to. Populated by
    /// `generate_project` from the source-mirrored output path.
    pub generated_file: String,
    /// Source files referenced by `mappings`, in file-id order.
    /// Each entry is `(path, optional_inline_content)`.
    pub sources: Vec<SourceInfo>,
}

/// A single source-map entry linking an AIR span to a target span.
#[derive(Debug, Clone)]
pub struct SourceMapEntry {
    /// AIR node id.
    pub air_node_id: u32,
    /// Index into `GeneratedCode::files`.
    pub file_index: usize,
    /// Byte offset in the generated file.
    pub target_start: usize,
    /// Byte length in the generated file.
    pub target_len: usize,
}

/// A single pointwise mapping from a position in generated code to a position
/// in the originating Bock source.
#[derive(Debug, Clone)]
pub struct SourceMapping {
    /// 1-indexed line in the generated file.
    pub gen_line: u32,
    /// 1-indexed column (character count) in the generated file.
    pub gen_col: u32,
    /// 1-indexed source line. `0` means unresolved — call
    /// [`SourceMap::resolve_positions`] with source content to fill this in.
    pub src_line: u32,
    /// 1-indexed source column. `0` when unresolved.
    pub src_col: u32,
    /// Byte offset into the source file; used to (re)compute line/col.
    pub src_offset: u32,
    /// File-registry id of the source file (index into `SourceMap::sources`).
    pub src_file_id: u32,
}

/// Metadata for a source file referenced by a [`SourceMap`].
#[derive(Debug, Clone)]
pub struct SourceInfo {
    /// File path (relative or absolute), as it should appear in the emitted
    /// source-map JSON.
    pub path: String,
    /// Optional inline content — when present, embedded into the `.map` file
    /// via `sourcesContent`.
    pub content: Option<String>,
}

impl SourceMap {
    /// Fills in `src_line` and `src_col` on every mapping by looking up
    /// `src_offset` inside `sources_content`, which is indexed by
    /// `src_file_id`. Mappings whose `src_file_id` is out of range are left
    /// unresolved.
    pub fn resolve_positions(&mut self, sources_content: &[&str]) {
        for m in &mut self.mappings {
            let Some(src) = sources_content.get(m.src_file_id as usize) else {
                continue;
            };
            let (line, col) = byte_to_line_col(src, m.src_offset as usize);
            m.src_line = line;
            m.src_col = col;
        }
    }

    /// Emits a Source Map v3 JSON document referring to this map's
    /// `generated_file` and `sources`. Only mappings whose `src_line` is
    /// non-zero are included.
    #[must_use]
    pub fn to_source_map_v3_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\"version\":3,\"file\":\"");
        out.push_str(&escape_json(&self.generated_file));
        out.push_str("\",\"sourceRoot\":\"\",\"sources\":[");
        for (i, s) in self.sources.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(&escape_json(&s.path));
            out.push('"');
        }
        out.push_str("],\"sourcesContent\":[");
        for (i, s) in self.sources.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            match &s.content {
                Some(c) => {
                    out.push('"');
                    out.push_str(&escape_json(c));
                    out.push('"');
                }
                None => out.push_str("null"),
            }
        }
        out.push_str("],\"names\":[],\"mappings\":\"");
        out.push_str(&encode_vlq_mappings(&self.mappings));
        out.push_str("\"}");
        out
    }
}

/// Convert a UTF-8 byte offset into a 1-indexed (line, column) pair. Column
/// counts Unicode scalar values, not bytes — matching `bock-source`.
fn byte_to_line_col(src: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(src.len());
    let before = &src[..offset];
    let line = before.bytes().filter(|b| *b == b'\n').count() as u32 + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let col = src[line_start..offset].chars().count() as u32 + 1;
    (line, col)
}

/// Minimal JSON string escaper for the small subset of characters that
/// appear in paths and source files.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Encode mappings as a Source Map v3 "mappings" string (semicolons between
/// generated lines, commas between segments, VLQ-encoded deltas).
fn encode_vlq_mappings(mappings: &[SourceMapping]) -> String {
    let mut resolved: Vec<&SourceMapping> = mappings.iter().filter(|m| m.src_line > 0).collect();
    resolved.sort_by_key(|m| (m.gen_line, m.gen_col));

    let mut out = String::new();
    let mut prev_gen_line: u32 = 1;
    let mut prev_gen_col: i64 = 0;
    let mut prev_src_file: i64 = 0;
    let mut prev_src_line: i64 = 0;
    let mut prev_src_col: i64 = 0;

    let mut first_on_line = true;
    for m in resolved {
        while prev_gen_line < m.gen_line {
            out.push(';');
            prev_gen_line += 1;
            prev_gen_col = 0;
            first_on_line = true;
        }
        if !first_on_line {
            out.push(',');
        }
        let gen_col = (m.gen_col as i64) - 1;
        let src_file = m.src_file_id as i64;
        let src_line = (m.src_line as i64) - 1;
        let src_col = (m.src_col as i64) - 1;

        vlq_encode(&mut out, gen_col - prev_gen_col);
        vlq_encode(&mut out, src_file - prev_src_file);
        vlq_encode(&mut out, src_line - prev_src_line);
        vlq_encode(&mut out, src_col - prev_src_col);

        prev_gen_col = gen_col;
        prev_src_file = src_file;
        prev_src_line = src_line;
        prev_src_col = src_col;
        first_on_line = false;
    }
    out
}

/// Base-64 VLQ encode a single signed integer onto `out`.
fn vlq_encode(out: &mut String, value: i64) {
    const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut v: u64 = if value < 0 {
        ((-value as u64) << 1) | 1
    } else {
        (value as u64) << 1
    };
    loop {
        let mut digit = (v & 0x1F) as u8;
        v >>= 5;
        if v != 0 {
            digit |= 0x20;
        }
        out.push(BASE64[digit as usize] as char);
        if v == 0 {
            break;
        }
    }
}

// ─── CodeGenerator trait ─────────────────────────────────────────────────────

/// The trait all per-target code generators implement.
///
/// Each target (JS, TS, Python, Rust, Go) provides a struct that implements
/// this trait. The `generate_module` method transforms a fully-lowered AIR
/// module into target-specific source code.
pub trait CodeGenerator {
    /// Returns the target profile for this generator.
    fn target(&self) -> &TargetProfile;

    /// Returns `true` when the given AIR node should go through Tier 1
    /// AI synthesis (§17.2, Q3 amended).
    ///
    /// The default implementation consults [`TargetProfile::ai_hints`]
    /// via [`crate::ai_synthesis::needs_ai_synthesis`]. Backends that
    /// want per-node overrides (e.g., only non-trivial `match`
    /// expressions) can override this method.
    fn needs_ai_synthesis(&self, node: &bock_air::AIRNode) -> bool {
        crate::ai_synthesis::needs_ai_synthesis(self.target(), node)
    }

    /// Generates target code from a fully-lowered AIR module.
    ///
    /// # Errors
    ///
    /// Returns `CodegenError` if the module contains constructs that cannot
    /// be represented in the target language.
    fn generate_module(&self, module: &AIRModule) -> Result<GeneratedCode, CodegenError>;

    /// Returns the source-code snippet that invokes the user's `main` function
    /// as the entry point for this target, or `None` if the target has a
    /// native entry-point convention (Rust `fn main`, Go `func main`) that
    /// runs without a synthetic call.
    ///
    /// `main_is_async` is `true` when the user's `main` function is declared
    /// `async fn`; targets with native async runtimes (JS, TS, Python) wrap
    /// the call in an event-loop driver in that case.
    ///
    /// Targets that need a trailing invocation (JS, TS, Python) override this
    /// to return e.g. `"main();\n"`. The default `generate_project` appends
    /// the snippet when any module declares a top-level `main` function.
    fn entry_invocation(&self, main_is_async: bool) -> Option<String> {
        let _ = main_is_async;
        None
    }

    /// Generates target code from multiple AIR modules with their source paths.
    ///
    /// Per spec §20.6.1 (DQ19 resolved), each reached module is emitted to its
    /// own target file and cross-module references are wired with the target's
    /// **native** import mechanism (ESM `import`, Python package imports, Rust
    /// `mod`/`use`, Go package files). Every v1 backend (JS, TS, Python, Rust,
    /// Go) overrides this with its per-module native-import emitter, so this is
    /// a **required** method — there is no default. (The single-module
    /// [`Self::generate_module`] is the self-contained, runtime-inlining emit
    /// used by per-backend unit tests, not a multi-module fallback.)
    fn generate_project(
        &self,
        modules: &[(&AIRModule, &Path)],
    ) -> Result<GeneratedCode, CodegenError>;

    /// Transpile the project's `@test` functions into the target's idiomatic test
    /// framework (project mode, §20.6.2).
    ///
    /// `framework` selects the deep-config test-framework variant (`"vitest"` /
    /// `"jest"` for js/ts; `"pytest"` / `"unittest"` for python; ignored for
    /// rust/go, whose frameworks are universal — `cargo test` / `go test`).
    /// Returns the test files to write into the scaffolded project plus an
    /// optional snippet to append to the entry file (Rust wires its inline
    /// `#[cfg(test)] mod` from `src/main.rs`). When the project has no `@test`
    /// functions the returned [`TestArtifacts`] is empty.
    ///
    /// The default implementation returns no test artifacts; every v1 backend
    /// overrides it.
    ///
    /// # Errors
    ///
    /// Returns `CodegenError` if a test body contains a construct that cannot be
    /// represented in the target language.
    fn generate_tests(
        &self,
        modules: &[(&AIRModule, &Path)],
        framework: &str,
    ) -> Result<TestArtifacts, CodegenError> {
        let _ = (modules, framework);
        Ok(TestArtifacts::default())
    }
}

/// The output of [`CodeGenerator::generate_tests`]: the transpiled test files
/// plus an optional snippet appended to the entry file.
///
/// Most targets place their tests in standalone files (`*.test.js`,
/// `test_*.py`, `*_test.go`). Rust uses an inline `#[cfg(test)] mod`, so its
/// `bock_tests.rs` must be wired into `src/main.rs` via a `mod bock_tests;`
/// declaration — carried in [`Self::entry_append`].
#[derive(Debug, Clone, Default)]
pub struct TestArtifacts {
    /// Standalone test files, paths relative to the target build root.
    pub files: Vec<OutputFile>,
    /// A snippet to append verbatim to the entry file's content (Rust's
    /// `mod bock_tests;`), or `None` when no entry wiring is required. The path
    /// of the entry file is target-specific; the build driver knows it.
    pub entry_append: Option<String>,
}

/// Restrict `modules` to those **reachable** from the entry module via real
/// `use` edges, returned in a *deterministic* dependency-before-dependent order
/// (a post-order DFS of the `use` graph with `use` targets visited in declared
/// module-path order). The result is independent of the input slice's order, so
/// the emitted per-module tree is byte-stable across the per-process topo-sort
/// shuffling described below.
///
/// `bock build` prepends the entire embedded `core.*` stdlib and makes every
/// user module implicitly depend on all of it (the §18.2 prelude, so core
/// symbols resolve without an explicit `use`). That implicit dependency is
/// correct for *name resolution* but wrong for *output*: emitting a core
/// module a program never references both bloats the output and — until the
/// stdlib is codegen-clean on every target — drags its latent codegen defects
/// into the build. The emitted tree must therefore include only modules the
/// entry program actually reaches through a real `use`.
///
/// Reachability is the transitive closure of each module's `ImportDecl` paths
/// (the explicit `use`s) matched against other modules' declared `module`
/// path — never the synthetic prelude edges, which are not represented as
/// `ImportDecl`s in the AIR. A program with no `use` (e.g. `hello_world`) thus
/// emits its entry module alone.
///
/// The entry module is the one declaring `main`; absent that (a library), the
/// last module in dependency order. The returned vec borrows from `modules`.
#[must_use]
pub fn reachable_modules<'a>(
    modules: &'a [(&'a AIRModule, &'a Path)],
) -> Vec<(&'a AIRModule, &'a Path)> {
    // Map declared module-path string → index, for resolving `use` targets.
    let path_of = |m: &AIRModule| -> Option<String> {
        if let NodeKind::Module { path: Some(p), .. } = &m.kind {
            Some(
                p.segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
            )
        } else {
            None
        }
    };
    let mut by_path: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, (m, _)) in modules.iter().enumerate() {
        if let Some(p) = path_of(m) {
            by_path.entry(p).or_insert(i);
        }
    }

    // The explicit `use` targets of one module, as path strings.
    let use_targets = |m: &AIRModule| -> Vec<String> {
        let NodeKind::Module { imports, .. } = &m.kind else {
            return vec![];
        };
        imports
            .iter()
            .filter_map(|imp| {
                if let NodeKind::ImportDecl { path, .. } = &imp.kind {
                    Some(
                        path.segments
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join("."),
                    )
                } else {
                    None
                }
            })
            .collect()
    };

    // Entry = the module declaring `main`, else the last (top of dep order).
    let Some(entry_idx) = modules
        .iter()
        .position(|(m, _)| module_declares_main_fn(m))
        .or_else(|| modules.len().checked_sub(1))
    else {
        return vec![];
    };

    // A *deterministic* post-order DFS of the explicit-`use` graph from the
    // entry module: this both prunes to reachable modules and orders them
    // dependencies-before-dependents, with a canonical, input-order-independent
    // result.
    //
    // Determinism matters because `bock build` runs in a fresh process per
    // invocation, and the upstream module list (`air_modules`) is produced by a
    // topological sort whose internal `HashMap`/`HashSet` iteration is seeded
    // randomly per process — so the *same* program's `modules` slice can arrive
    // in different (all valid) topological orders on different runs. Relying on
    // that input order made the emitted module order (entry selection + file
    // emission order) vary run-to-run, which surfaced as a rare, random `bock
    // build` failure once several independent embedded `core.*` modules were
    // reachable. Visiting each module's `use` targets in a fixed order (declared
    // module path, then index) pins the output.
    let mut visited = vec![false; modules.len()];
    let mut order: Vec<usize> = Vec::new();
    // Iterative post-order DFS (recursion-free to avoid deep-graph stack use):
    // `Enter(i)` schedules children then a matching `Exit(i)`; `Exit(i)` appends
    // `i` after all its dependencies, giving dependency-before-dependent order.
    enum Step {
        Enter(usize),
        Exit(usize),
    }
    let mut stack = vec![Step::Enter(entry_idx)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(idx) => {
                if visited[idx] {
                    continue;
                }
                visited[idx] = true;
                stack.push(Step::Exit(idx));
                // Resolve this module's `use` targets to indices and visit them
                // in a deterministic order: by declared module path (stable
                // across runs), then by index as a final tiebreak.
                let mut child_indices: Vec<usize> = use_targets(modules[idx].0)
                    .iter()
                    .filter_map(|target| by_path.get(target).copied())
                    .collect();
                child_indices.sort_by(|&a, &b| {
                    path_of(modules[a].0)
                        .cmp(&path_of(modules[b].0))
                        .then(a.cmp(&b))
                });
                child_indices.dedup();
                // Push in reverse so the smallest-keyed child is processed first
                // (the stack pops LIFO), keeping the emitted order ascending.
                for child in child_indices.into_iter().rev() {
                    if !visited[child] {
                        stack.push(Step::Enter(child));
                    }
                }
            }
            Step::Exit(idx) => order.push(idx),
        }
    }

    order.into_iter().map(|i| modules[i]).collect()
}

/// Returns true if the given AIR module declares a top-level function named
/// `main`. Used by the build pipeline to decide whether to append an
/// entry-point invocation to the generated output of targets without a
/// native main convention.
#[must_use]
pub fn module_declares_main_fn(module: &AIRModule) -> bool {
    let NodeKind::Module { items, .. } = &module.kind else {
        return false;
    };
    items.iter().any(|item| {
        matches!(
            &item.kind,
            NodeKind::FnDecl { name, .. } if name.name == "main"
        )
    })
}

/// The declared module-path of an AIR module as a dotted string
/// (e.g. `core.option`), or `None` if the module declares no `module <path>`.
///
/// Used by the per-module (native-import) emission path to map a module's
/// *declared* path — not its on-disk source path — onto the target's import
/// path. For Python this drives both the emitted file location
/// (`core.option` → `core/option.py`) and the import statement
/// (`from core.option import …`), so the two agree and a multi-file program
/// resolves its imports when run from the build root.
#[must_use]
pub fn module_path_string(module: &AIRModule) -> Option<String> {
    if let NodeKind::Module { path: Some(p), .. } = &module.kind {
        Some(
            p.segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("."),
        )
    } else {
        None
    }
}

/// Returns true if the given AIR module declares a top-level `async fn main`.
/// Used by `generate_project` to select an async-aware entry invocation.
#[must_use]
pub fn module_main_fn_is_async(module: &AIRModule) -> bool {
    let NodeKind::Module { items, .. } = &module.kind else {
        return false;
    };
    items.iter().any(|item| {
        matches!(
            &item.kind,
            NodeKind::FnDecl { name, is_async: true, .. } if name.name == "main"
        )
    })
}

// ─── Transpiled-test extraction + assertion classification (§20.6.2) ─────────
//
// Project mode (§20.6.2) transpiles each Bock `@test` function into the target's
// idiomatic test framework. These shared helpers identify the `@test` functions
// and classify the `expect(actual).<assertion>(expected)` chains in their bodies
// so each per-target test emitter can lower them to its framework's idiom
// without re-implementing the AIR pattern-matching five times.

/// Returns `true` if `node` is a function declaration carrying the `@test`
/// annotation. Matches the discovery rule used by `bock test`
/// (`bock-cli::test::discover_test_functions`): an `@test`-annotated `FnDecl`.
#[must_use]
pub fn fn_is_test(node: &AIRNode) -> bool {
    matches!(
        &node.kind,
        NodeKind::FnDecl { annotations, .. }
            if annotations.iter().any(|a| a.name.name == "test")
    )
}

/// Collect every `@test`-annotated top-level function across the given modules,
/// paired with the module's declared path (dotted, or `""` if anonymous).
///
/// The result preserves module order and within-module declaration order, so the
/// emitted test files are deterministic. Each entry borrows the `FnDecl` node.
#[must_use]
pub fn collect_test_fns<'a>(
    modules: &'a [(&'a AIRModule, &'a Path)],
) -> Vec<(&'a AIRNode, String)> {
    let mut tests = Vec::new();
    for (module, _) in modules {
        let module_path = module_path_string(module).unwrap_or_default();
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                if fn_is_test(item) {
                    tests.push((item, module_path.clone()));
                }
            }
        }
    }
    tests
}

/// A recognized Bock test assertion (`expect(actual).<method>(...)`).
///
/// Each variant carries the framework-agnostic *intent*; the per-target emitter
/// maps it to the idiom (Vitest/Jest `expect().toBe(...)`, pytest `assert`, Rust
/// `assert_eq!`, Go `t.Errorf`, etc.). The assertion methods mirror the
/// interpreter's `register_test_builtins` set (`bock-interp::builtins`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestAssertion {
    /// `expect(actual).to_equal(expected)`
    Equal,
    /// `expect(actual).to_be_true()`
    BeTrue,
    /// `expect(actual).to_be_false()`
    BeFalse,
    /// `expect(actual).to_be_some()`
    BeSome,
    /// `expect(actual).to_be_none()`
    BeNone,
    /// `expect(actual).to_be_ok()`
    BeOk,
    /// `expect(actual).to_be_err()`
    BeErr,
}

impl TestAssertion {
    fn from_method(name: &str) -> Option<Self> {
        match name {
            "to_equal" => Some(Self::Equal),
            "to_be_true" => Some(Self::BeTrue),
            "to_be_false" => Some(Self::BeFalse),
            "to_be_some" => Some(Self::BeSome),
            "to_be_none" => Some(Self::BeNone),
            "to_be_ok" => Some(Self::BeOk),
            "to_be_err" => Some(Self::BeErr),
            _ => None,
        }
    }
}

/// If `stmt` is an `expect(actual).<assertion>(expected?)` chain, classify it.
///
/// Returns `(assertion, actual_expr, expected_expr_opt)` where `actual_expr` is
/// the argument to `expect(...)` and `expected_expr_opt` is the explicit
/// argument to the assertion method (present for [`TestAssertion::Equal`],
/// absent for the nullary predicates). Returns `None` for any statement that is
/// not an `expect(...)`-rooted assertion chain — the emitter falls back to its
/// normal statement lowering for those.
///
/// A Bock method call `recv.m(args)` is lowered (by `bock-air::lower`) to
/// `Call { callee: FieldAccess(recv, m), args: [self=recv, ...args] }` — the
/// receiver is *prepended* as the implicit `self` argument. So an assertion
/// `expect(actual).to_equal(expected)` is:
/// ```text
/// Call {
///   callee: FieldAccess(object: Call{expect, [actual]}, field: to_equal),
///   args: [ self = Call{expect, [actual]}, expected ],
/// }
/// ```
/// The explicit `expected` is therefore `args[1]` (`args[0]` is the self copy).
#[must_use]
pub fn classify_assertion(stmt: &AIRNode) -> Option<(TestAssertion, &AIRNode, Option<&AIRNode>)> {
    let NodeKind::Call { callee, args, .. } = &stmt.kind else {
        return None;
    };
    let NodeKind::FieldAccess { object, field } = &callee.kind else {
        return None;
    };
    let assertion = TestAssertion::from_method(&field.name)?;
    // The receiver object must be `expect(actual)`.
    let NodeKind::Call {
        callee: expect_callee,
        args: expect_args,
        ..
    } = &object.kind
    else {
        return None;
    };
    let NodeKind::Identifier { name } = &expect_callee.kind else {
        return None;
    };
    if name.name != "expect" {
        return None;
    }
    let actual = expect_args.first().map(|a| &a.value)?;
    // `args[0]` is the desugared `self` (a copy of the `expect(...)` receiver);
    // the explicit assertion argument, if any, is `args[1]`.
    let expected = args.get(1).map(|a| &a.value);
    Some((assertion, actual, expected))
}

// ─── ESM per-module emission helpers (js/ts) ────────────────────────────────
//
// The JS and TS backends emit a per-module **native ES-module import tree**
// (spec §20.6.1; DQ19 resolved): each reachable module → its own `.js`/`.ts`
// file, cross-module references resolved with real `import { x } from "./…"`.
// These helpers are shared by both backends because the analysis is purely
// over the AIR (declared symbols, references, declared module paths) and is
// identical for the two targets. Python (`py.rs`) has its own equivalents
// because its import surface (package paths, no relative specifier, a shared
// `*`-runtime) differs enough to not share cleanly.

/// Runtime-prelude *value* names that the JS/TS backends lower **inline** to
/// tagged objects (`{ _tag: "Some", _0: v }`, `{ _tag: "Less" }`, …) — NOT from
/// a cross-module `core.*` import. The implicit-import pass and the
/// public-symbol map must never route these through a `core.option` /
/// `core.compare` import: the declaring module does not actually export them
/// (they are compiler built-ins), so a real import would be an unresolved
/// reference.
///
/// `Ordering` is intentionally **absent**: unlike `Optional`/`Result`, the
/// comparison `Ordering` enum is genuinely *declared* (and exported) by the
/// `core.compare` stdlib module, so a cross-module use of the `Ordering` **type**
/// resolves through a real import; only its *variant values* (`Less`/`Equal`/
/// `Greater`) lower inline and so stay excluded here.
pub const ESM_RUNTIME_PRELUDE_NAMES: &[&str] = &[
    "Optional", "Some", "None", "Result", "Ok", "Err", "Less", "Equal", "Greater",
];

/// The declaration kind of a public symbol exposed by the per-module ESM
/// analysis. Each backend maps this to the right cross-module import form,
/// because the JS and TS emitted shapes differ (a trait is a JS `const` mixin
/// value but a TS `interface` type; an enum *type* name has no JS binding but is
/// a TS type; a type alias is erased in JS but a TS type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EsmDeclKind {
    /// A top-level function (camelCased on emit; a value in both targets).
    Function,
    /// A `const` (a value in both targets).
    Const,
    /// A record (JS/TS `class` — a value; in TS also a type).
    Record,
    /// A `class` (a value; in TS also a type).
    Class,
    /// An enum's **type** name (`Ordering`) — no JS binding; a TS type.
    EnumType,
    /// An enum **variant** value (`Color_Red`) — a value in both targets.
    EnumVariant,
    /// A trait — a JS `const` mixin value; a TS `interface` type.
    Trait,
    /// An effect — a JS/TS `class`/`interface`; treated as a value.
    Effect,
    /// A type alias — erased in JS; a TS type.
    TypeAlias,
}

impl EsmDeclKind {
    /// Whether a symbol of this kind has a runtime binding in **JS** (so a JS
    /// cross-module reference imports it as a value). Type-only kinds (an enum
    /// type name, a TS-only type alias) have no JS binding.
    #[must_use]
    pub fn is_js_value(self) -> bool {
        matches!(
            self,
            EsmDeclKind::Function
                | EsmDeclKind::Const
                | EsmDeclKind::Record
                | EsmDeclKind::Class
                | EsmDeclKind::EnumVariant
                | EsmDeclKind::Trait
                | EsmDeclKind::Effect
        )
    }

    /// Whether a symbol of this kind is imported with TS `import type` (a
    /// pure-type kind: an enum type name, a trait interface, or a type alias).
    /// Value-and-type kinds (records, classes) and pure values import normally.
    #[must_use]
    pub fn is_ts_type_only(self) -> bool {
        matches!(
            self,
            EsmDeclKind::EnumType | EsmDeclKind::Trait | EsmDeclKind::TypeAlias
        )
    }
}

/// One public symbol exposed by the per-module ESM analysis. Carries the
/// declaring module-path and the declaration kind. The kind drives both the
/// emitted-name transform (only a function is camelCased: `get_or` → `getOr`)
/// and the import form each backend selects (value vs `import type` vs skip in
/// JS) — see [`EsmDeclKind`].
#[derive(Debug, Clone)]
pub struct EsmSymbol {
    /// Dotted declared module-path that declares this symbol (e.g. `core.iter`).
    pub module_path: String,
    /// The declaration kind.
    pub kind: EsmDeclKind,
    /// For an [`EsmDeclKind::EnumVariant`], the variant's **bare source name**
    /// (`Electronics`) — distinct from the map key, which is the *emitted*
    /// value-name (`Category_Electronics`). `None` for every other kind.
    ///
    /// A glob-imported (`use models.*`) variant is referenced in AIR by its bare
    /// source name, not its emitted `Enum_Variant` name, so the implicit-import
    /// reference scan must also try the bare spelling — see
    /// [`implicit_esm_imports_for`]. Keeping the import's emitted *name* as the
    /// map key (and only matching on the bare name) means the backends still
    /// import the identifier they actually emit (`Category_Electronics`).
    pub variant_bare_name: Option<String>,
}

impl EsmSymbol {
    /// True if the symbol is a function (camelCased on emit).
    #[must_use]
    pub fn is_fn(&self) -> bool {
        matches!(self.kind, EsmDeclKind::Function)
    }
}

/// Build a map from every **public top-level symbol name** (the raw Bock name)
/// declared across `modules` to its [`EsmSymbol`] (declaring module-path +
/// whether it is a function). Covers functions, records, enums (and each
/// variant's emitted `Enum_Variant` factory/const name), traits, classes,
/// effects, type aliases, and consts.
///
/// The per-module ESM emission path needs this for **implicit imports**: a
/// prelude trait used as a base in an `impl` (`impl Iterable for Bag`, with
/// `Iterable` auto-imported per §18.2) is referenced without an explicit `use`.
/// Emitting one file per module means the consuming file must
/// `import` `Iterable` from `core/iter.js` even though it never appears in an
/// explicit `use`. This map lets the backend add exactly those imports for
/// names a module references but neither declares locally nor imports
/// explicitly. The key is the **raw** Bock name so the reference scan in
/// [`implicit_esm_imports_for`] matches the AIR debug rendering.
///
/// Runtime-prelude names ([`ESM_RUNTIME_PRELUDE_NAMES`]) are excluded — they
/// lower inline. The first declarer wins for a name declared in several modules
/// (deterministic via the dependency order `modules` arrives in).
#[must_use]
pub fn collect_public_symbols_for_esm(
    modules: &[(&AIRModule, &Path)],
) -> HashMap<String, EsmSymbol> {
    let mut map: HashMap<String, EsmSymbol> = HashMap::new();
    for (module, _) in modules {
        let Some(module_path) = module_path_string(module) else {
            continue;
        };
        let NodeKind::Module { items, .. } = &module.kind else {
            continue;
        };
        for item in items {
            let mut record = |name: &str, kind: EsmDeclKind, variant_bare_name: Option<String>| {
                if !ESM_RUNTIME_PRELUDE_NAMES.contains(&name) {
                    map.entry(name.to_string()).or_insert_with(|| EsmSymbol {
                        module_path: module_path.clone(),
                        kind,
                        variant_bare_name,
                    });
                }
            };
            match &item.kind {
                NodeKind::FnDecl {
                    visibility, name, ..
                } => {
                    if matches!(visibility, bock_ast::Visibility::Public) {
                        record(&name.name, EsmDeclKind::Function, None);
                    }
                }
                NodeKind::RecordDecl {
                    visibility, name, ..
                } => {
                    if matches!(visibility, bock_ast::Visibility::Public) {
                        record(&name.name, EsmDeclKind::Record, None);
                    }
                }
                NodeKind::ClassDecl {
                    visibility, name, ..
                } => {
                    if matches!(visibility, bock_ast::Visibility::Public) {
                        record(&name.name, EsmDeclKind::Class, None);
                    }
                }
                NodeKind::TraitDecl {
                    visibility, name, ..
                } => {
                    if matches!(visibility, bock_ast::Visibility::Public) {
                        record(&name.name, EsmDeclKind::Trait, None);
                    }
                }
                NodeKind::EffectDecl {
                    visibility, name, ..
                } => {
                    if matches!(visibility, bock_ast::Visibility::Public) {
                        record(&name.name, EsmDeclKind::Effect, None);
                    }
                }
                NodeKind::TypeAlias {
                    visibility, name, ..
                } => {
                    if matches!(visibility, bock_ast::Visibility::Public) {
                        record(&name.name, EsmDeclKind::TypeAlias, None);
                    }
                }
                NodeKind::ConstDecl {
                    visibility, name, ..
                } => {
                    if matches!(visibility, bock_ast::Visibility::Public) {
                        record(&name.name, EsmDeclKind::Const, None);
                    }
                }
                NodeKind::EnumDecl {
                    visibility,
                    name,
                    variants,
                    ..
                } => {
                    if matches!(visibility, bock_ast::Visibility::Public) {
                        record(&name.name, EsmDeclKind::EnumType, None);
                        for v in variants {
                            if let NodeKind::EnumVariant { name: vname, .. } = &v.kind {
                                // Key on the emitted value-name (`Category_Electronics`)
                                // so the import binds the identifier the backends emit,
                                // but carry the bare source name (`Electronics`) so the
                                // reference scan can match a glob-imported use site.
                                record(
                                    &format!("{}_{}", name.name, vname.name),
                                    EsmDeclKind::EnumVariant,
                                    Some(vname.name.clone()),
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    map
}

/// One **exportable, runtime-valued** public declaration of a module: the raw
/// emitted name plus whether it is a function (camelCased on emit). Returned by
/// [`exportable_value_names`].
#[derive(Debug, Clone)]
pub struct EsmExport {
    /// The symbol's name as the declaration/enum-variant emits it before any
    /// camelCase transform (`get_or`, `Color_Red`, `MAX`).
    pub name: String,
    /// True if the symbol is a function (the backend camelCases it on emit).
    pub is_fn: bool,
}

/// The set of **JS/TS-emitted, exportable** public top-level value declarations
/// a module declares — the names the per-module path lists in a trailing
/// `export { … }` (or, for functions, that the backend exports inline). Covers
/// functions, records, enums (+ each `Enum_Variant`), traits, classes, effects,
/// and consts. **Type aliases are excluded**: they are erased in JS (a comment,
/// no runtime binding) and emitted as an `export type` alias inline in TS, so
/// they need no trailing re-export. Runtime-prelude names are excluded (lowered
/// inline). Each entry carries the function flag so the backend camelCases
/// function names to match their inline `export function` form.
///
/// Used by the **JS** backend's trailing-export pass (TS exports every kind
/// except enum variants inline — see [`enum_variant_value_names`]).
#[must_use]
pub fn exportable_value_names(module: &AIRModule) -> Vec<EsmExport> {
    let mut names: Vec<EsmExport> = Vec::new();
    let mut push = |name: String, is_fn: bool| {
        if !ESM_RUNTIME_PRELUDE_NAMES.contains(&name.as_str()) {
            names.push(EsmExport { name, is_fn });
        }
    };
    let NodeKind::Module { items, .. } = &module.kind else {
        return names;
    };
    for item in items {
        match &item.kind {
            NodeKind::FnDecl {
                visibility, name, ..
            } => {
                if matches!(visibility, bock_ast::Visibility::Public) {
                    push(name.name.clone(), true);
                }
            }
            NodeKind::RecordDecl {
                visibility, name, ..
            }
            | NodeKind::TraitDecl {
                visibility, name, ..
            }
            | NodeKind::ClassDecl {
                visibility, name, ..
            }
            | NodeKind::EffectDecl {
                visibility, name, ..
            }
            | NodeKind::ConstDecl {
                visibility, name, ..
            } => {
                if matches!(visibility, bock_ast::Visibility::Public) {
                    push(name.name.clone(), false);
                }
            }
            NodeKind::EnumDecl {
                visibility,
                name,
                variants,
                ..
            } => {
                if matches!(visibility, bock_ast::Visibility::Public) {
                    for v in variants {
                        if let NodeKind::EnumVariant { name: vname, .. } = &v.kind {
                            push(format!("{}_{}", name.name, vname.name), false);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    names
}

/// The public **enum-variant value names** (`Color_Red`, …) declared in
/// `module` — the names a TS per-module file must re-export in a trailing
/// `export { … }`.
///
/// In the TS backend every public top-level declaration exports inline
/// (`export class`, `export type`, `export function`, `export const`) **except**
/// an enum's per-variant interface / const / factory, which the variant emitter
/// writes without an `export`. The per-module tree needs those exported so a
/// consuming file can import them, so this enumerates exactly the variant value
/// names for the trailing re-export. Variants of a runtime-
/// prelude enum (`Optional` / `Result` / `Ordering`) are excluded — they lower
/// inline.
#[must_use]
pub fn enum_variant_value_names(module: &AIRModule) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let NodeKind::Module { items, .. } = &module.kind else {
        return names;
    };
    for item in items {
        if let NodeKind::EnumDecl {
            visibility,
            name,
            variants,
            ..
        } = &item.kind
        {
            if matches!(visibility, bock_ast::Visibility::Public)
                && !ESM_RUNTIME_PRELUDE_NAMES.contains(&name.name.as_str())
            {
                for v in variants {
                    if let NodeKind::EnumVariant { name: vname, .. } = &v.kind {
                        names.push(format!("{}_{}", name.name, vname.name));
                    }
                }
            }
        }
    }
    names
}

/// Top-level symbol names declared **locally** in `module` (item names plus
/// each enum variant's emitted `Enum_Variant` name) — the names a per-module
/// implicit import must never shadow with a cross-module import.
#[must_use]
pub fn locally_declared_names(module: &AIRModule) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let NodeKind::Module { items, .. } = &module.kind else {
        return names;
    };
    for item in items {
        match &item.kind {
            NodeKind::FnDecl { name, .. }
            | NodeKind::RecordDecl { name, .. }
            | NodeKind::TraitDecl { name, .. }
            | NodeKind::ClassDecl { name, .. }
            | NodeKind::EffectDecl { name, .. }
            | NodeKind::TypeAlias { name, .. }
            | NodeKind::ConstDecl { name, .. } => {
                names.insert(name.name.clone());
            }
            NodeKind::EnumDecl { name, variants, .. } => {
                names.insert(name.name.clone());
                for v in variants {
                    if let NodeKind::EnumVariant { name: vname, .. } = &v.kind {
                        names.insert(format!("{}_{}", name.name, vname.name));
                    }
                }
            }
            _ => {}
        }
    }
    names
}

/// Names brought into scope by `module`'s explicit `use` declarations (the
/// imported leaf names and their aliases) — already emitted as real imports, so
/// the implicit-import pass must skip them.
#[must_use]
pub fn explicitly_imported_names(module: &AIRModule) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let NodeKind::Module { imports, .. } = &module.kind else {
        return names;
    };
    for import in imports {
        if let NodeKind::ImportDecl {
            items: bock_ast::ImportItems::Named(named),
            ..
        } = &import.kind
        {
            for n in named {
                names.insert(n.name.name.clone());
                if let Some(alias) = &n.alias {
                    names.insert(alias.name.clone());
                }
            }
        }
    }
    names
}

/// One implicit cross-module import computed by [`implicit_esm_imports_for`]:
/// the declaring module-path, the raw symbol name, and the declaration kind (so
/// the backend can camelCase a function, route a type to `import type`, or skip
/// a JS type-only name).
#[derive(Debug, Clone)]
pub struct ImplicitEsmImport {
    /// Dotted declared module-path that declares the symbol.
    pub module_path: String,
    /// The symbol's raw Bock name.
    pub name: String,
    /// The declaration kind.
    pub kind: EsmDeclKind,
}

impl ImplicitEsmImport {
    /// True if the symbol is a function (camelCased on emit).
    #[must_use]
    pub fn is_fn(&self) -> bool {
        matches!(self.kind, EsmDeclKind::Function)
    }
}

/// Compute the implicit cross-module imports for `module`: public symbols
/// declared in *other* reachable modules that `module` references but neither
/// declares locally nor imports explicitly.
///
/// "References" is a conservative structural scan of the module's debug
/// rendering for the symbol name as a quoted identifier token. It can only
/// *over*-import a name the program does not really use (a harmless dead
/// import), never *under*-import — so it cannot reintroduce the unresolved
/// reference it exists to fix.
///
/// An **enum variant** needs a second probe. Its map key is the *emitted*
/// value-name (`Category_Electronics`), but a glob-imported (`use models.*`)
/// variant is referenced in AIR by its *bare* source name
/// (`Identifier { name: "Electronics" }`) — the `Enum_Variant` joining happens
/// only at emit time. So for a variant we also scan for the bare source name
/// ([`EsmSymbol::variant_bare_name`]) and, on a match, import the symbol under
/// its emitted key (the identifier the backends actually emit and need bound).
/// Without this the per-module JS/TS file omits the variant import and
/// `ReferenceError`s / TS2304s at every bare-variant use site.
#[must_use]
pub fn implicit_esm_imports_for(
    module: &AIRModule,
    public_symbols: &HashMap<String, EsmSymbol>,
    own_path: &str,
) -> Vec<ImplicitEsmImport> {
    let local = locally_declared_names(module);
    let explicit = explicitly_imported_names(module);
    let rendered = format!("{module:?}");
    let mut out: Vec<ImplicitEsmImport> = Vec::new();
    for (name, sym) in public_symbols {
        if sym.module_path == own_path || local.contains(name) || explicit.contains(name) {
            continue;
        }
        // A reference is the emitted name as a quoted token, or — for an enum
        // variant — its bare source name (the spelling a glob-imported use site
        // carries). Either way the symbol is imported under its emitted key.
        let referenced = rendered.contains(&format!("\"{name}\""))
            || sym
                .variant_bare_name
                .as_ref()
                .is_some_and(|bare| rendered.contains(&format!("\"{bare}\"")));
        if referenced {
            out.push(ImplicitEsmImport {
                module_path: sym.module_path.clone(),
                name: name.clone(),
                kind: sym.kind,
            });
        }
    }
    out
}

/// Collect the names of every **record** declared across `modules` (the names
/// the JS/TS backends emit as classes and construct with `new Name(...)`).
///
/// The per-module path emits each module in its own context, so a cross-module
/// record construction (`handling (Log with ConsoleLog {})` where `ConsoleLog`
/// is `use`d from another module) would not find the record in the local
/// `record_names` set and would mis-lower to a bare object literal `{}` instead
/// of `new ConsoleLog()` (dropping its prototype methods). Pre-seeding
/// `record_names` from the whole reachable set gives every per-module emit
/// context cross-module record visibility. Mirrors
/// [`collect_enum_variants`] / [`collect_trait_decls`].
#[must_use]
pub fn collect_record_names(modules: &[(&AIRModule, &Path)]) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    for (module, _) in modules {
        let NodeKind::Module { items, .. } = &module.kind else {
            continue;
        };
        for item in items {
            if let NodeKind::RecordDecl { name, .. } = &item.kind {
                names.insert(name.name.clone());
            }
        }
    }
    names
}

/// Collect every **class** declared across `modules`, mapping each class name to
/// its **field names in declaration order**.
///
/// A Bock `class` and a `record` both lower to a JS/TS `class`, but with
/// *different constructor shapes*: a `record T { a, b }` emits a destructured
/// `constructor({ a, b })` (so a `T { a: x, b: y }` literal lowers to `new T({ a:
/// x, b: y })`), whereas a `class T { a, b }` emits a **positional**
/// `constructor(a, b)` (so a `T { a: x, b: y }` literal must lower to `new T(x,
/// y)` — arguments in *field-declaration order*, regardless of the literal's
/// field spelling order).
///
/// The js/ts `RecordConstruct` emitters consult this map (kept **separate** from
/// [`collect_record_names`]) to pick the class's positional shape and to order
/// the supplied field values by the declared field order. It is js/ts-only: the
/// shared `record_names` set must stay records-only because py/go/rust derive
/// other behavior from it, and a Bock class's positional construction is a
/// js/ts emission concern. Without it a class literal falls through to the
/// record/object path and emits a bare object literal whose prototype methods
/// are unreachable (`btn.render is not a function`). Mirrors
/// [`collect_record_names`] / [`collect_enum_variants`].
#[must_use]
pub fn collect_class_fields(
    modules: &[(&AIRModule, &Path)],
) -> std::collections::HashMap<String, Vec<String>> {
    let mut classes = std::collections::HashMap::new();
    for (module, _) in modules {
        let NodeKind::Module { items, .. } = &module.kind else {
            continue;
        };
        for item in items {
            if let NodeKind::ClassDecl { name, fields, .. } = &item.kind {
                let field_order = fields.iter().map(|f| f.name.name.clone()).collect();
                classes.insert(name.name.clone(), field_order);
            }
        }
    }
    classes
}

/// Pre-scan every reached module and collect the **declared names of all
/// module-scope `const`s**.
///
/// A const's identifier must be spelled identically at its declaration and at
/// every use site, across all backends. Each backend's value-identifier
/// transform (`to_camel_case` on JS/TS, `to_snake_case` on Python, `to_pascal_case`
/// on Go) mangles a `SCREAMING_SNAKE` const name *differently* at the use site
/// than the declaration emits it (def `FIZZ_NUM` vs use `fizzNUM` on JS/TS; def
/// `fizz_num` vs use `FIZZ_NUM` on Python; def `FIZZNUM` vs use `fizzNUM` on Go),
/// producing a "not defined"/`NameError` at the target. The backends consult this
/// registry at both the `ConstDecl` and `Identifier` arms to emit the const's
/// **verbatim declared name** in both places — `SCREAMING_SNAKE` is a valid
/// identifier in every target. A *pre-scan* (rather than recording consts as
/// their decls are emitted) is required because a use site may precede its
/// const's declaration in source order, and because a `use`d const can live in a
/// different module than its use. Mirrors [`collect_record_names`] /
/// [`collect_enum_variants`].
#[must_use]
pub fn collect_const_names(modules: &[(&AIRModule, &Path)]) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    for (module, _) in modules {
        let NodeKind::Module { items, .. } = &module.kind else {
            continue;
        };
        for item in items {
            if let NodeKind::ConstDecl { name, .. } = &item.kind {
                names.insert(name.name.clone());
            }
        }
    }
    names
}

/// Compute the relative ES-module import specifier from the file that hosts
/// module `from_path` to the file that hosts module `to_path`, both keyed on
/// their **declared** dotted module-paths and laid out at the mirrored path
/// (`core.option` → `core/option.<ext>`). The entry module is always at the
/// build root as `main.<ext>` regardless of its declared path, so callers pass
/// the **empty string** as `from_path` for the entry file.
///
/// Returns a specifier that always begins with `./` or `../` and ends with the
/// target file extension (e.g. `./core/option.js`, `../helper.ts`) — ESM
/// requires a relative specifier to be explicitly relative and, for Node, to
/// carry the file extension.
#[must_use]
pub fn esm_relative_specifier(from_path: &str, to_path: &str, ext: &str) -> String {
    // The directory components of the *source* file (everything but the final
    // segment, which is the file stem). The entry file lives at the root.
    let from_dirs: Vec<&str> = if from_path.is_empty() {
        Vec::new()
    } else {
        let segs: Vec<&str> = from_path.split('.').collect();
        segs[..segs.len().saturating_sub(1)].to_vec()
    };
    let to_segs: Vec<&str> = to_path.split('.').filter(|s| !s.is_empty()).collect();

    // Longest common directory prefix.
    let mut common = 0usize;
    while common < from_dirs.len()
        && common + 1 < to_segs.len()
        && from_dirs[common] == to_segs[common]
    {
        common += 1;
    }

    let ups = from_dirs.len() - common;
    let mut spec = String::new();
    if ups == 0 {
        spec.push_str("./");
    } else {
        for _ in 0..ups {
            spec.push_str("../");
        }
    }
    let down: Vec<&str> = to_segs[common..].to_vec();
    spec.push_str(&down.join("/"));
    spec.push('.');
    spec.push_str(ext);
    spec
}

// ─── Shared js/ts transpiled-test builder (§20.6.2) ──────────────────────────

/// Lowers a single AIR expression to its target string, for the shared js/ts
/// test-file builder. Implemented by a thin adapter over each backend's private
/// emit context so [`js_ts_generate_tests`] can reuse the exact expression
/// lowering the runtime tree uses (function casing, enum/Optional reps, …).
pub trait JsTsExprEmitter {
    /// Render `node` as a target expression string.
    ///
    /// # Errors
    ///
    /// Propagates the backend's [`CodegenError`].
    fn expr_to_string(&mut self, node: &AIRNode) -> Result<String, CodegenError>;
}

/// camelCase a Bock identifier the way the js/ts backends do for value names
/// (so an imported function name matches its `export function` form). Mirrors
/// `bock-codegen::js::to_camel_case` for the common `snake_case`/`PascalCase`
/// inputs `@test` bodies reference.
fn js_camel_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = false;
    for (i, c) in s.chars().enumerate() {
        if c == '_' {
            upper_next = true;
        } else if i == 0 {
            out.push(c.to_ascii_lowercase());
        } else if upper_next {
            out.push(c.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Build the Vitest/Jest test file for the project's `@test` functions (S7),
/// shared by the JS and TS backends (identical apart from the file extension and
/// the concrete emit context, both injected by the caller).
///
/// - `framework`: `"jest"` → Jest globals; anything else → Vitest import.
/// - `file_ext`: the test file's own extension (`"js"` / `"ts"`).
/// - `import_ext`: the extension to use in *import specifiers*. For JS this is
///   `"js"`; for TS it is **also** `"js"` — TS/ESM specifiers reference the
///   emitted `.js`, which `tsc`'s `.js`→`.ts` resolution follows (and a strict
///   `tsc --noEmit` rejects a `.ts` specifier without `allowImportingTsExtensions`).
/// - `output_path`: maps a module to its emitted file path (entry → `main.<ext>`).
/// - `make_emitter`: builds the per-program expression emitter (with the same
///   cross-module registries the runtime tree uses).
///
/// Imports each module's public functions by name and lowers
/// `expect(actual).<assertion>(expected)` chains to the framework's matcher API.
/// Returns a single `bock.test.<file_ext>` file (no entry wiring).
///
/// # Errors
///
/// Propagates [`CodegenError`] from expression lowering.
pub fn js_ts_generate_tests<'a, F, M>(
    modules: &'a [(&'a AIRModule, &'a Path)],
    framework: &str,
    file_ext: &str,
    import_ext: &str,
    output_path: F,
    make_emitter: M,
) -> Result<TestArtifacts, CodegenError>
where
    F: Fn(&'a AIRModule, &'a Path, bool) -> PathBuf,
    M: for<'b> FnOnce(&'b [(&'b AIRModule, &'b Path)]) -> Box<dyn JsTsExprEmitter>,
{
    let reachable = reachable_modules(modules);
    let tests = collect_test_fns(&reachable);
    if tests.is_empty() {
        return Ok(TestArtifacts::default());
    }
    let entry_idx = reachable
        .iter()
        .position(|(m, _)| module_declares_main_fn(m))
        .unwrap_or(reachable.len().saturating_sub(1));

    // Build the per-module import lines: each reachable module's public function
    // names, imported by their camelCased (emitted) name from the module's file.
    let mut import_lines: Vec<String> = Vec::new();
    for (i, (module, source_path)) in reachable.iter().enumerate() {
        // Public functions, imported by their camelCased (emitted) name.
        let mut import_names: Vec<String> = exportable_value_names(module)
            .into_iter()
            .filter(|e| e.is_fn)
            .map(|e| js_camel_case(&e.name))
            .collect();
        // Enum-variant constructors a `@test` body may reference *bare* as a
        // call argument (e.g. `apply_casing("x", Upper)` → emits `Casing_Upper`,
        // the frozen `{enum}_{variant}` const). The runtime tree exports these
        // (js trailing `export { … }`, ts `enum_variant_value_names`), but unlike
        // a function name they are emitted *verbatim* (no camelCase), so import
        // them under their exact value-name. Over-importing an unreferenced
        // variant is a harmless dead import; under-importing a referenced one is
        // a `ReferenceError` at test runtime — so mirror the non-test path and
        // include every public variant value-name.
        import_names.extend(enum_variant_value_names(module));
        if import_names.is_empty() {
            continue;
        }
        let spec = if i == entry_idx {
            // The entry file is `main.<file_ext>`; the specifier references the
            // emitted/served module by its import extension (`main.js`).
            let stem = output_path(module, source_path, true)
                .with_extension(import_ext)
                .display()
                .to_string();
            format!("./{stem}")
        } else {
            let to_path = module_path_string(module).unwrap_or_default();
            esm_relative_specifier("", &to_path, import_ext)
        };
        // Normalize Windows-style separators a Display might emit.
        let spec = spec.replace('\\', "/");
        import_lines.push(format!(
            "import {{ {} }} from \"{spec}\";",
            import_names.join(", ")
        ));
    }
    import_lines.sort_unstable();
    import_lines.dedup();

    let is_jest = framework == "jest";
    let mut out = String::new();
    if is_jest {
        out.push_str("// Jest provides describe/it/expect as globals.\n");
    } else {
        out.push_str("import { describe, it, expect } from \"vitest\";\n");
    }
    for line in &import_lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out.push_str("describe(\"bock tests\", () => {\n");

    let mut emitter = make_emitter(&reachable);
    for (test_fn, _module_path) in &tests {
        let NodeKind::FnDecl { name, body, .. } = &test_fn.kind else {
            continue;
        };
        out.push_str(&format!("  it(\"{}\", () => {{\n", name.name));
        emit_js_test_body(body, emitter.as_mut(), &mut out)?;
        out.push_str("  });\n");
    }
    out.push_str("});\n");

    Ok(TestArtifacts {
        files: vec![OutputFile {
            path: PathBuf::from(format!("bock.test.{file_ext}")),
            content: out,
            source_map: None,
        }],
        entry_append: None,
    })
}

/// Whether a JS/TS `actual` expression must be parenthesized before a `._tag`
/// member access, i.e. whether its emitted form binds looser than member access.
///
/// Atoms and postfix forms — identifiers, literals, calls, method calls, field
/// accesses, and index reads — bind at least as tightly as member access, so a
/// following `._tag` needs no wrapping (and Prettier would strip any redundant
/// parens). Everything else (binary/conditional/await/etc.) is wrapped so the
/// generated test file is both correct and `prettier --check`-clean.
fn js_actual_needs_member_parens(actual: &AIRNode) -> bool {
    !matches!(
        &actual.kind,
        NodeKind::Identifier { .. }
            | NodeKind::Literal { .. }
            | NodeKind::Call { .. }
            | NodeKind::MethodCall { .. }
            | NodeKind::FieldAccess { .. }
            | NodeKind::Index { .. }
    )
}

/// Emit the statements of a js/ts `@test` body into `out`, lowering `expect(...)`
/// assertion chains to the Vitest/Jest matcher API and dropping any non-assertion
/// statement that the matcher set does not cover into a `let` (handled by the
/// expression emitter). Each line is indented four spaces (inside `it(... => {`).
fn emit_js_test_body(
    body: &AIRNode,
    emitter: &mut dyn JsTsExprEmitter,
    out: &mut String,
) -> Result<(), CodegenError> {
    let stmts: Vec<&AIRNode> = match &body.kind {
        NodeKind::Block { stmts, tail } => stmts.iter().chain(tail.as_deref()).collect(),
        _ => vec![body],
    };
    for stmt in stmts {
        if let Some((assertion, actual, expected)) = classify_assertion(stmt) {
            let a = emitter.expr_to_string(actual)?;
            // For tag-discriminating predicates we read `<actual>._tag`. Wrap the
            // actual in parens only when its expression form would otherwise bind
            // looser than the member access (so the emitted `.test` file stays
            // `prettier --check`-clean: prettier strips redundant parens around a
            // call/identifier/member/index, §20.6.2 codegen-formatter agreement).
            let tagged = |a: &str| -> String {
                if js_actual_needs_member_parens(actual) {
                    format!("({a})._tag")
                } else {
                    format!("{a}._tag")
                }
            };
            let line = match assertion {
                TestAssertion::Equal => {
                    let e = match expected {
                        Some(e) => emitter.expr_to_string(e)?,
                        None => "undefined".to_string(),
                    };
                    format!("expect({a}).toEqual({e});")
                }
                TestAssertion::BeTrue => format!("expect({a}).toBe(true);"),
                TestAssertion::BeFalse => format!("expect({a}).toBe(false);"),
                TestAssertion::BeSome => format!("expect({}).toBe(\"Some\");", tagged(&a)),
                TestAssertion::BeNone => format!("expect({}).toBe(\"None\");", tagged(&a)),
                TestAssertion::BeOk => format!("expect({}).toBe(\"Ok\");", tagged(&a)),
                TestAssertion::BeErr => format!("expect({}).toBe(\"Err\");", tagged(&a)),
            };
            out.push_str(&format!("    {line}\n"));
        } else if let NodeKind::LetBinding { pattern, value, .. } = &stmt.kind {
            // A `let` in a test body (e.g. building the value under assertion):
            // lower it to a `const` so the following assertions can reference it.
            let name = match &pattern.kind {
                NodeKind::BindPat { name, .. } => js_camel_case(&name.name),
                _ => continue,
            };
            let v = emitter.expr_to_string(value)?;
            out.push_str(&format!("    const {name} = {v};\n"));
        } else {
            // Any other statement is emitted as an expression statement.
            let s = emitter.expr_to_string(stmt)?;
            out.push_str(&format!("    {s};\n"));
        }
    }
    Ok(())
}

// ─── Native-module emission helpers (rust/go) ───────────────────────────────
//
// The Rust and Go backends emit a per-module **native module tree** (spec
// §20.6.1; DQ19 resolved): each reachable module → its own target file,
// cross-module references resolved with the target's native module system
// (Rust `use crate::<m>::<x>;`; Go same-package symbol visibility). These
// helpers are shared because the analysis — which public symbols a module
// declares, and which symbols declared elsewhere a module references but never
// `use`s explicitly — is purely over the AIR and identical for both targets.
//
// Unlike the ESM helpers, these carry no per-symbol declaration *kind*: Rust
// and Go re-export every public top-level declaration uniformly (Rust via a
// crate-path `use`; Go via the shared package scope), so a flat name→module
// map suffices.

/// Build a map from every **public top-level symbol name** declared across
/// `modules` to the dotted declared module-path that declares it (e.g.
/// `Iterable` → `core.iter`). Covers functions, records, enums (the **type**
/// name), traits, classes, effects, type aliases, and consts.
///
/// The per-module native-module path needs this for **implicit imports**: a
/// §18.2-prelude trait used as an `impl` base (`impl Iterable for Bag`, with
/// `Iterable` auto-imported per §18.2) is referenced without an explicit `use`.
/// Emitting one file per module means the consuming `main.rs` must
/// `use crate::core::iter::Iterable;` even though `Iterable` never appears in
/// an explicit `use`. (Go keeps one package across files, so a same-package
/// symbol is visible without an import; Go uses this map only to know which
/// names are cross-module, not to emit anything.) The map lets the backend add
/// exactly those Rust `use`s for names a module references but neither declares
/// locally nor imports explicitly.
///
/// Enum **variants** are intentionally *not* recorded as separate symbols:
/// Rust accesses a variant through its type (`Ordering::Less`), so importing
/// the enum type suffices, and a synthetic `Ordering_Less` is not a real Rust
/// item to `use`. Go (same-package) needs no imports at all.
///
/// The first declarer wins for a name declared in several modules (the
/// dependency order `modules` arrives in is deterministic — see
/// [`reachable_modules`]).
#[must_use]
pub fn collect_public_symbol_modules(modules: &[(&AIRModule, &Path)]) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    for (module, _) in modules {
        let Some(module_path) = module_path_string(module) else {
            continue;
        };
        let NodeKind::Module { items, .. } = &module.kind else {
            continue;
        };
        for item in items {
            let mut record = |name: &str| {
                map.entry(name.to_string())
                    .or_insert_with(|| module_path.clone());
            };
            match &item.kind {
                NodeKind::FnDecl {
                    visibility, name, ..
                }
                | NodeKind::RecordDecl {
                    visibility, name, ..
                }
                | NodeKind::TraitDecl {
                    visibility, name, ..
                }
                | NodeKind::ClassDecl {
                    visibility, name, ..
                }
                | NodeKind::EffectDecl {
                    visibility, name, ..
                }
                | NodeKind::TypeAlias {
                    visibility, name, ..
                }
                | NodeKind::ConstDecl {
                    visibility, name, ..
                }
                | NodeKind::EnumDecl {
                    visibility, name, ..
                } => {
                    if matches!(visibility, bock_ast::Visibility::Public) {
                        record(&name.name);
                    }
                }
                _ => {}
            }
        }
    }
    map
}

/// Compute the implicit cross-module imports for `module`: public symbols
/// declared in *other* reachable modules that `module` references but neither
/// declares locally nor imports explicitly. Returns `(module_path, name)`
/// pairs.
///
/// "References" is a conservative structural scan of the module's debug
/// rendering for the symbol name as a quoted identifier token (mirroring the
/// Python / ESM equivalents). It can only *over*-import a name the program does
/// not really use — harmless on Rust (a dead `use` is `allow`-ed by the
/// crate-level `#![allow(unused_imports)]`) — never *under*-import, so it
/// cannot reintroduce the unresolved reference it exists to fix.
#[must_use]
pub fn implicit_imports_for(
    module: &AIRModule,
    public_symbols: &HashMap<String, String>,
    own_path: &str,
) -> Vec<(String, String)> {
    let local = locally_declared_names(module);
    let explicit = explicitly_imported_names(module);
    let rendered = format!("{module:?}");
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, declaring_module) in public_symbols {
        if declaring_module == own_path || local.contains(name) || explicit.contains(name) {
            continue;
        }
        if rendered.contains(&format!("\"{name}\"")) {
            out.push((declaring_module.clone(), name.clone()));
        }
    }
    out
}

/// Map a module's *declared* dotted path (`core.option`) to its **relative
/// output path** in a per-module tree, with the target's file extension
/// (`core/option.<ext>`). The entry module is laid out separately (always
/// `main.<ext>` at a stable location), so callers pass non-entry modules here;
/// a module with no declared path falls back to its source-mirrored path.
#[must_use]
pub fn module_tree_relpath(
    module: &AIRModule,
    source_path: &Path,
    target: &TargetProfile,
) -> PathBuf {
    match module_path_string(module) {
        Some(path) if !path.is_empty() => {
            let rel: PathBuf = path.split('.').collect();
            rel.with_extension(&target.conventions.file_extension)
        }
        _ => derive_output_path(source_path, target),
    }
}

// ─── Statement-aware match helpers ──────────────────────────────────────────
//
// Some Bock `match` arms have *statement* bodies — `break`, `continue`,
// `return`, or an assignment. These have no value, so an arm carrying one
// cannot be lowered to an expression form (a ternary, an IIFE, or a value
// `match` arm). Backends that emit `match` as an expression must instead emit
// such a match in **statement position** (a `switch` / if-chain that yields no
// value). The predicates below let every backend agree on what counts as a
// statement arm without duplicating the classification.

/// Returns true if `node` is a statement-like AIR node — one that performs
/// control flow or mutation and yields no usable value in expression position.
///
/// These are exactly the node kinds a target's expression form (ternary, IIFE,
/// value-`match` arm) cannot host: `break`, `continue`, `return`, assignment,
/// and an `if` that yields no value.
///
/// An `if` is statement-like — and so must be emitted in statement position
/// rather than lowered to a ternary / IIFE — exactly when it produces no value:
///
/// - it has **no `else` branch** (a value-less `if` cannot be an expression), or
/// - it has an `else` branch but **both branches are statement bodies** (e.g.
///   `if (c) { return a } else { return b }`), so neither yields a value.
///
/// A value `if/else` (e.g. `let x = if (c) { 1 } else { 2 }`) always has an
/// `else` whose branches end in an *expression* tail, so
/// [`arm_body_is_statement`] returns `false` for them and the `if` stays an
/// expression. `if let … = expr` returning a value is likewise unaffected: with
/// an expression-tail `else` it is not classified here.
#[must_use]
pub fn node_is_statement(node: &AIRNode) -> bool {
    if let NodeKind::If {
        then_block,
        else_block,
        ..
    } = &node.kind
    {
        return match else_block {
            // No `else`: the `if` yields no value, so it is a statement.
            None => true,
            // With an `else`, the `if` is a statement only when *both* branches
            // are statement bodies (neither yields a usable value). A value
            // `if/else` has expression-tail branches and falls through to
            // `false`, keeping it an expression.
            Some(else_b) => arm_body_is_statement(then_block) && arm_body_is_statement(else_b),
        };
    }
    matches!(
        node.kind,
        NodeKind::Break { .. }
            | NodeKind::Continue
            | NodeKind::Return { .. }
            | NodeKind::Assign { .. }
    )
}

/// Returns true if a `match`-arm body is a statement body — either the body is
/// itself a statement node, or it is a `{ ... }` block whose tail is a
/// statement node (or which has no tail at all, e.g. a block ending in a
/// statement with no value).
#[must_use]
pub fn arm_body_is_statement(body: &AIRNode) -> bool {
    if node_is_statement(body) {
        return true;
    }
    if let NodeKind::Block { tail, .. } = &body.kind {
        return match tail {
            Some(t) => node_is_statement(t),
            // A block with no tail expression yields no value.
            None => true,
        };
    }
    false
}

/// Returns true if any arm of a `match` carries a statement body (see
/// [`arm_body_is_statement`]). When true, backends without a statement-admitting
/// expression form (Go, Python, JS, TS) must emit the `match` in statement
/// position rather than as an expression.
#[must_use]
pub fn match_has_statement_arm(arms: &[AIRNode]) -> bool {
    arms.iter().any(
        |arm| matches!(&arm.kind, NodeKind::MatchArm { body, .. } if arm_body_is_statement(body)),
    )
}

/// Returns true if a `match`'s arms require the *if/else-if-chain* lowering
/// (JS, TS, Go) rather than the value/tag `switch` fast-path.
///
/// The value/tag `switch` those backends emit can express only a flat dispatch
/// on a single discriminant (a literal value, or an ADT `._tag`). It
/// **structurally cannot** express:
///
/// - **guards** — a failed guard must fall through to the *next arm*, but a
///   `break` inside a `switch` exits the whole `switch`;
/// - **or-patterns** (`1 | 2 | 3 => …`) — one arm, several discriminants;
/// - **tuple patterns** (`(a, b) => …`) — no single discriminant;
/// - **nested constructor / record patterns** (`Some(Ok(v)) => …`) — the inner
///   pattern must itself be tested and its bindings extracted recursively.
///
/// When any arm needs one of these, the backend lowers the *whole* match to an
/// `if (<test> && <guard?>) { <binds>; <body> } else if …` chain (see each
/// backend's `emit_match_ifchain`). Otherwise the existing `switch` fast-path is
/// kept, so the proven Optional / Result / user-enum / value lowerings do not
/// regress.
///
/// A constructor / record field counts as "nested" only when its sub-pattern is
/// itself refutable or structured — another constructor, record, tuple,
/// or-pattern, or literal. A bare bind (`Some(x)`) or wildcard (`Some(_)`) field
/// is *not* nested: the flat `switch` already extracts those correctly.
#[must_use]
pub fn match_needs_ifchain(arms: &[AIRNode]) -> bool {
    arms.iter().any(|arm| {
        let NodeKind::MatchArm { pattern, guard, .. } = &arm.kind else {
            return false;
        };
        guard.is_some() || pattern_needs_ifchain(pattern)
    })
}

/// True if `pat` (a pattern node) can only be lowered via the if/else-if chain
/// — i.e. it is an or-pattern, a tuple pattern, a **list** pattern (`[]`,
/// `[x]`, `[first, ..rest]`), a **range** pattern (`1..10`, `1..=10`), or a
/// constructor/record pattern carrying a nested structured sub-pattern. See
/// [`match_needs_ifchain`].
///
/// List and range patterns join the always-if-chain set because neither has a
/// single `switch` discriminant: a list match needs a length test plus
/// positional element / `..rest` binds, and a range match is a relational
/// `lo <= x < hi` test. Routing them uniformly through the if-chain on every
/// backend that consults this recogniser (ts, go) lets one shared `pattern_test`
/// / `pattern_binds` per backend handle them, instead of each backend needing a
/// bespoke detour (cf. js's former local `match_has_unswitchable_pattern`).
fn pattern_needs_ifchain(pat: &AIRNode) -> bool {
    match &pat.kind {
        NodeKind::OrPat { .. }
        | NodeKind::TuplePat { .. }
        | NodeKind::ListPat { .. }
        | NodeKind::RangePat { .. } => true,
        NodeKind::ConstructorPat { fields, .. } => fields.iter().any(field_is_structured),
        NodeKind::RecordPat { fields, .. } => fields
            .iter()
            .filter_map(|f| f.pattern.as_deref())
            .any(field_is_structured),
        _ => false,
    }
}

/// True if a constructor / record *field* sub-pattern is structured (anything
/// other than a bare bind or wildcard), so the enclosing match must take the
/// if-chain path to test and bind it recursively.
fn field_is_structured(pat: &AIRNode) -> bool {
    !matches!(&pat.kind, NodeKind::WildcardPat | NodeKind::BindPat { .. })
}

// ─── Shared temp-hoist desugar for value-position diverging control flow ───────
//
// A control-flow expression used in *value position* (a `let` initialiser, a
// `return` value, a call argument, an assignment RHS) whose arms **diverge** —
// one arm yields a value while another exits via `return`/`break`/`continue`/a
// diverging intrinsic (`todo()`/`unreachable()`) — has no clean per-backend
// expression form. A ternary / value-IIFE cannot host a `return` arm (the IIFE
// would capture it), and every backend's value emitter previously fell through
// to `/* unsupported */` (rust/js/go) or `# unsupported` (py) for the diverging
// tail. The chat-protocol example is the canonical case:
//
//     let msg_type = if (raw.starts_with("TEXT|")) { Text }
//                    else { … else { return Err("unknown") } }
//
// [`hoist_value_cf`] rewrites each such value position into a self-contained
// block that every backend already emits correctly through its existing
// statement / `let` / assignment / IIFE machinery:
//
//     {
//       let mut __bock_cf_N            // declared, no initialiser (DeclOnly)
//       <CF in statement position>    // value tails → `__bock_cf_N = v`,
//                                      // diverging tails kept verbatim
//       __bock_cf_N                    // block tail: read the temp
//     }
//
// The control-flow node is kept intact (only relocated to statement position
// with assignment tails), so a backend's structural type inference — e.g. Go's
// `infer_branchy_expr_type` — still fires on it to type the `var` declaration.

/// Metadata key marking a synthesised temp [`NodeKind::LetBinding`] as
/// *declare-only*: it introduces the binding with no initialiser. The shared
/// [`hoist_value_cf`] desugar emits these; every backend's `let` emitter checks
/// this key and emits the bare declaration (`let x;` / `var x T` / Rust deferred
/// `let mut x;`) rather than a `= <value>` initialiser. The carried
/// [`bock_air::stubs::Value::Bool`] is always `true`.
pub const DECL_ONLY_META: &str = "bock_decl_only";

/// Internal metadata key marking a synthesised `{ temp = v; break }` block (from
/// a value-`loop` `break <v>` rewrite) as *splice-flattenable*: the enclosing
/// statement list inlines its statements rather than nesting it, so no `{ … }`
/// block remains in statement position (which a backend would treat as a
/// value-IIFE). Never emitted — consumed entirely within [`hoist_value_cf`].
const SPLICE_BLOCK_META: &str = "bock_splice_block";

/// True when a value-position node is a control-flow construct whose branches
/// **diverge** — at least one branch yields a value AND at least one branch
/// exits via `return`/`break`/`continue`/a diverging intrinsic. These are the
/// nodes [`hoist_value_cf`] rewrites; a construct where *every* branch yields a
/// value already lowers fine via the existing expression paths and is left
/// untouched (so value `if`/`match`/`loop` codegen does not regress).
#[must_use]
pub fn value_cf_diverges(node: &AIRNode) -> bool {
    match &node.kind {
        // A `loop` delivers a value only through a `break <v>`. Hoist it only
        // when it carries at least one value-bearing `break` — a value-less loop
        // (whose result is unit / discarded) has a clean statement form already
        // and must NOT be hoisted (that would leave the temp uninitialised).
        NodeKind::Loop { body } => loop_has_value_break(body),
        NodeKind::If {
            then_block,
            else_block,
            let_pattern: None,
            ..
        } => {
            let branches = [Some(then_block.as_ref()), else_block.as_deref()];
            let any_diverges = branches
                .iter()
                .flatten()
                .any(|b| branch_diverges_or_nested(b));
            let any_value = branches.iter().flatten().any(|b| branch_yields_value(b));
            any_diverges && any_value
        }
        NodeKind::Match { arms, .. } => {
            let bodies: Vec<&AIRNode> = arms
                .iter()
                .filter_map(|a| match &a.kind {
                    NodeKind::MatchArm { body, .. } => Some(body.as_ref()),
                    _ => None,
                })
                .collect();
            let any_diverges = bodies.iter().any(|b| branch_diverges_or_nested(b));
            let any_value = bodies.iter().any(|b| branch_yields_value(b));
            any_diverges && any_value
        }
        NodeKind::Block { tail, .. } => tail.as_deref().is_some_and(value_cf_diverges),
        _ => false,
    }
}

/// True when a branch / arm body used in value position diverges at its tail
/// (a `return`/`break`/`continue`/diverging-intrinsic), or is itself a nested
/// diverging value-CF (an `if`/`match`/`loop` chain whose own branches diverge).
fn branch_diverges_or_nested(node: &AIRNode) -> bool {
    branch_tail_diverges(node) || value_cf_diverges(node)
}

/// True when the *value tail* of `node` diverges — it produces no usable value
/// on any path. That is: a `return`/`break`/`continue` node, a diverging
/// intrinsic call (`todo()`/`unreachable()`), the `Unreachable` node, a block
/// whose tail/last-statement diverges, **or** an `if`/`match` *every* one of
/// whose branches diverges (e.g. `match s { Ok => return …; Err => return … }` —
/// no arm yields a value, so the construct yields none and must not be treated
/// as a value-bearing arm of an enclosing hoist).
fn branch_tail_diverges(node: &AIRNode) -> bool {
    match &node.kind {
        NodeKind::Return { .. } | NodeKind::Break { .. } | NodeKind::Continue => true,
        NodeKind::Unreachable => true,
        NodeKind::Call { .. } => call_is_diverging_intrinsic(node),
        NodeKind::Block { stmts, tail } => match tail {
            Some(t) => branch_tail_diverges(t),
            None => stmts.last().is_some_and(branch_tail_diverges),
        },
        // An `if` with no `else` can fall through (yields a value path), so it
        // does not fully diverge; with an `else`, it diverges iff both branches
        // do.
        NodeKind::If {
            then_block,
            else_block: Some(else_b),
            ..
        } => branch_tail_diverges(then_block) && branch_tail_diverges(else_b),
        NodeKind::Match { arms, .. } => {
            let bodies: Vec<&AIRNode> = arms
                .iter()
                .filter_map(|a| match &a.kind {
                    NodeKind::MatchArm { body, .. } => Some(body.as_ref()),
                    _ => None,
                })
                .collect();
            !bodies.is_empty() && bodies.iter().all(|b| branch_tail_diverges(b))
        }
        _ => false,
    }
}

/// True when a branch / arm body used in value position yields a usable value —
/// its tail is neither a diverging statement nor (recursively) a diverging
/// nested CF on *every* path. A branch that is itself a diverging value-CF still
/// yields a value (its value arm does), so this returns `true` for it.
fn branch_yields_value(node: &AIRNode) -> bool {
    if value_cf_diverges(node) {
        return true;
    }
    !branch_tail_diverges(node)
}

/// True when a `loop` body contains a value-carrying `break <v>` (so the loop
/// produces a value and, in value position, needs the temp-hoist). Does not
/// descend into nested loops — their `break`s target themselves — or into
/// functions/lambdas.
fn loop_has_value_break(body: &AIRNode) -> bool {
    match &body.kind {
        NodeKind::Break { value } => value.is_some(),
        NodeKind::Loop { .. }
        | NodeKind::While { .. }
        | NodeKind::For { .. }
        | NodeKind::FnDecl { .. }
        | NodeKind::Lambda { .. } => false,
        NodeKind::Block { stmts, tail } => {
            stmts.iter().any(loop_has_value_break)
                || tail.as_deref().is_some_and(loop_has_value_break)
        }
        NodeKind::If {
            then_block,
            else_block,
            ..
        } => {
            loop_has_value_break(then_block)
                || else_block.as_deref().is_some_and(loop_has_value_break)
        }
        NodeKind::Match { arms, .. } => arms.iter().any(
            |a| matches!(&a.kind, NodeKind::MatchArm { body, .. } if loop_has_value_break(body)),
        ),
        NodeKind::Guard { else_block, .. } => loop_has_value_break(else_block),
        _ => false,
    }
}

/// True when `node` is a call to a diverging intrinsic (`todo()` /
/// `unreachable()`), matched by callee identifier name. Mirrors the per-backend
/// `call_is_diverging` recognisers so the shared desugar agrees with them.
fn call_is_diverging_intrinsic(node: &AIRNode) -> bool {
    let NodeKind::Call { callee, .. } = &node.kind else {
        return false;
    };
    matches!(
        &callee.kind,
        NodeKind::Identifier { name } if name.name == "todo" || name.name == "unreachable"
    )
}

/// Run the shared temp-hoist desugar over a fully-lowered, type-checked AIR
/// module, returning the rewritten module. Idempotent on trees with no
/// value-position diverging control flow (returns an equivalent tree).
///
/// This is a **codegen pre-pass**: it runs after type-checking and the
/// ownership/effect/capability analyses (so they never see the synthesised
/// declare-only bindings) and before every backend emits, making the rewrite
/// shared once across all five targets. It deliberately lives here rather than
/// in `bock-air`'s S-AIR lowering because the synthesised temp's type is only
/// derivable at codegen (e.g. Go infers it structurally from the relocated
/// control-flow node), and to keep the interpreter and semantic analyses out of
/// the blast radius.
#[must_use]
/// Codegen pre-pass: rewrite every derived-blanket `recv.into()` call in
/// `module` into the resolvable associated call `Target.from(recv)`.
///
/// A derived blanket `Into[Target] for Source` is the bodyless reverse impl the
/// compiler synthesizes from a user `impl From[Source] for Target`. It is
/// *unexecutable* if emitted as an ordinary method call — the AIR lowers
/// `recv.into()` to `Call(FieldAccess(recv, "into"), [recv])`, which dispatches
/// to a non-existent `into` method on every compiled target (JS `recv.into is
/// not a function`, etc.). The executable form is `Target.from(recv)` — the
/// `from` associated function each backend emits for the `From` impl.
///
/// Run **after** type-checking (in each backend's `generate_*`), so a `.into()`
/// that reaches this pass has already resolved to a valid `Into` target: an
/// unrelated-target `.into()` was rejected at check time (`E4012`) and never
/// arrives here. The pass fires only when the module declares exactly one
/// distinct `From` target, making the rewrite's target unambiguous (the
/// documented v1 single-conversion scope). With zero or several `From` impls it
/// is a no-op, leaving the call to its existing lowering. The rewritten `Call`
/// is stamped [`bock_air::lower::ASSOC_CALL_META_KEY`] so the backends emit the
/// static / free-function `from` call.
pub fn lower_blanket_into(module: AIRNode) -> AIRNode {
    let targets = collect_from_targets(&module);
    // Unambiguous only with exactly one distinct `From` target.
    let [target] = targets.as_slice() else {
        return module;
    };
    let mut rewriter = BlanketIntoRewriter {
        next_id: max_node_id(&module) + 1,
        target: target.clone(),
    };
    rewriter.rewrite(module)
}

/// The base name of every `impl From[Source] for Target`'s target type, deduped.
fn collect_from_targets(module: &AIRNode) -> Vec<String> {
    let NodeKind::Module { items, .. } = &module.kind else {
        return Vec::new();
    };
    let mut targets: Vec<String> = items
        .iter()
        .filter_map(|item| {
            let NodeKind::ImplBlock {
                trait_path: Some(tp),
                target,
                ..
            } = &item.kind
            else {
                return None;
            };
            if tp.segments.last().map(|s| s.name.as_str()) != Some("From") {
                return None;
            }
            type_node_base_name(target)
        })
        .collect();
    targets.sort();
    targets.dedup();
    targets
}

/// The base name of a `TypeNamed` AIR node (`Foot` from `Foot` / `Foot[T]`).
fn type_node_base_name(ty: &AIRNode) -> Option<String> {
    if let NodeKind::TypeNamed { path, .. } = &ty.kind {
        path.segments.last().map(|s| s.name.clone())
    } else {
        None
    }
}

struct BlanketIntoRewriter {
    next_id: bock_air::NodeId,
    target: String,
}

impl BlanketIntoRewriter {
    fn fresh_id(&mut self) -> bock_air::NodeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn rewrite(&mut self, mut node: AIRNode) -> AIRNode {
        // Rewrite a desugared `recv.into()` call: `Call(FieldAccess(recv,
        // "into"), [recv])` → `Call(FieldAccess(Identifier(Target), "from"),
        // [recv])` stamped as an associated call. `desugared_self_call` confirms
        // the receiver is re-passed as the lone `self` arg (so this is the
        // blanket `.into()`, never an associated `Type.into()` or a 1-arg method
        // named `into`).
        if let NodeKind::Call { callee, args, .. } = &node.kind {
            if let Some((recv, method, rest)) = desugared_self_call(callee, args) {
                if method.name == "into" && rest.is_empty() {
                    let span = node.span;
                    let recv = recv.clone();
                    let target_id =
                        AIRNode::new(self.fresh_id(), span, ident_node(&self.target, span));
                    let field = AIRNode::new(
                        self.fresh_id(),
                        span,
                        NodeKind::FieldAccess {
                            object: Box::new(target_id),
                            field: bock_ast::Ident {
                                name: "from".to_string(),
                                span,
                            },
                        },
                    );
                    let mut call = AIRNode::new(
                        self.fresh_id(),
                        span,
                        NodeKind::Call {
                            callee: Box::new(field),
                            args: vec![AirArg {
                                label: None,
                                value: self.rewrite(recv),
                            }],
                            type_args: vec![],
                        },
                    );
                    call.metadata.insert(
                        bock_air::lower::ASSOC_CALL_META_KEY.to_string(),
                        bock_air::Value::Bool(true),
                    );
                    return call;
                }
            }
        }
        node.kind = self.rewrite_kind(node.kind);
        node
    }

    fn rewrite_box(&mut self, node: Box<AIRNode>) -> Box<AIRNode> {
        Box::new(self.rewrite(*node))
    }

    fn rewrite_vec(&mut self, nodes: Vec<AIRNode>) -> Vec<AIRNode> {
        nodes.into_iter().map(|n| self.rewrite(n)).collect()
    }

    fn rewrite_args(&mut self, args: Vec<AirArg>) -> Vec<AirArg> {
        args.into_iter()
            .map(|a| AirArg {
                label: a.label,
                value: self.rewrite(a.value),
            })
            .collect()
    }

    /// Recurse into every child that can contain an expression. Mirrors the
    /// structure [`ValueCfHoister`] walks; any arm not listed has no nested
    /// expression a `.into()` could hide in.
    fn rewrite_kind(&mut self, kind: NodeKind) -> NodeKind {
        match kind {
            NodeKind::Module {
                path,
                annotations,
                imports,
                items,
            } => NodeKind::Module {
                path,
                annotations,
                imports,
                items: self.rewrite_vec(items),
            },
            NodeKind::FnDecl {
                annotations,
                visibility,
                is_async,
                name,
                generic_params,
                params,
                return_type,
                effect_clause,
                where_clause,
                body,
            } => NodeKind::FnDecl {
                annotations,
                visibility,
                is_async,
                name,
                generic_params,
                params,
                return_type,
                effect_clause,
                where_clause,
                body: self.rewrite_box(body),
            },
            NodeKind::ImplBlock {
                annotations,
                generic_params,
                trait_path,
                trait_args,
                target,
                where_clause,
                methods,
            } => NodeKind::ImplBlock {
                annotations,
                generic_params,
                trait_path,
                trait_args,
                target,
                where_clause,
                methods: self.rewrite_vec(methods),
            },
            NodeKind::ClassDecl {
                annotations,
                visibility,
                name,
                generic_params,
                base,
                traits,
                fields,
                methods,
            } => NodeKind::ClassDecl {
                annotations,
                visibility,
                name,
                generic_params,
                base,
                traits,
                fields,
                methods: self.rewrite_vec(methods),
            },
            NodeKind::Block { stmts, tail } => NodeKind::Block {
                stmts: self.rewrite_vec(stmts),
                tail: tail.map(|t| self.rewrite_box(t)),
            },
            NodeKind::LetBinding {
                pattern,
                ty,
                value,
                is_mut,
            } => NodeKind::LetBinding {
                pattern,
                ty,
                value: self.rewrite_box(value),
                is_mut,
            },
            NodeKind::Assign { target, op, value } => NodeKind::Assign {
                target: self.rewrite_box(target),
                op,
                value: self.rewrite_box(value),
            },
            NodeKind::Call {
                callee,
                args,
                type_args,
            } => NodeKind::Call {
                callee: self.rewrite_box(callee),
                args: self.rewrite_args(args),
                type_args,
            },
            NodeKind::MethodCall {
                receiver,
                method,
                args,
                type_args,
            } => NodeKind::MethodCall {
                receiver: self.rewrite_box(receiver),
                method,
                args: self.rewrite_args(args),
                type_args,
            },
            NodeKind::FieldAccess { object, field } => NodeKind::FieldAccess {
                object: self.rewrite_box(object),
                field,
            },
            NodeKind::Index { object, index } => NodeKind::Index {
                object: self.rewrite_box(object),
                index: self.rewrite_box(index),
            },
            NodeKind::BinaryOp { op, left, right } => NodeKind::BinaryOp {
                op,
                left: self.rewrite_box(left),
                right: self.rewrite_box(right),
            },
            NodeKind::UnaryOp { op, operand } => NodeKind::UnaryOp {
                op,
                operand: self.rewrite_box(operand),
            },
            NodeKind::Propagate { expr } => NodeKind::Propagate {
                expr: self.rewrite_box(expr),
            },
            NodeKind::Await { expr } => NodeKind::Await {
                expr: self.rewrite_box(expr),
            },
            NodeKind::Move { expr } => NodeKind::Move {
                expr: self.rewrite_box(expr),
            },
            NodeKind::Borrow { expr } => NodeKind::Borrow {
                expr: self.rewrite_box(expr),
            },
            NodeKind::MutableBorrow { expr } => NodeKind::MutableBorrow {
                expr: self.rewrite_box(expr),
            },
            NodeKind::Return { value } => NodeKind::Return {
                value: value.map(|v| self.rewrite_box(v)),
            },
            NodeKind::Lambda { params, body } => NodeKind::Lambda {
                params,
                body: self.rewrite_box(body),
            },
            NodeKind::If {
                let_pattern,
                condition,
                then_block,
                else_block,
            } => NodeKind::If {
                let_pattern,
                condition: self.rewrite_box(condition),
                then_block: self.rewrite_box(then_block),
                else_block: else_block.map(|e| self.rewrite_box(e)),
            },
            NodeKind::Match { scrutinee, arms } => NodeKind::Match {
                scrutinee: self.rewrite_box(scrutinee),
                arms: self.rewrite_vec(arms),
            },
            NodeKind::MatchArm {
                pattern,
                guard,
                body,
            } => NodeKind::MatchArm {
                pattern,
                guard: guard.map(|g| self.rewrite_box(g)),
                body: self.rewrite_box(body),
            },
            NodeKind::Guard {
                let_pattern,
                condition,
                else_block,
            } => NodeKind::Guard {
                let_pattern,
                condition: self.rewrite_box(condition),
                else_block: self.rewrite_box(else_block),
            },
            NodeKind::While { condition, body } => NodeKind::While {
                condition: self.rewrite_box(condition),
                body: self.rewrite_box(body),
            },
            NodeKind::Loop { body } => NodeKind::Loop {
                body: self.rewrite_box(body),
            },
            NodeKind::For {
                pattern,
                iterable,
                body,
            } => NodeKind::For {
                pattern,
                iterable: self.rewrite_box(iterable),
                body: self.rewrite_box(body),
            },
            NodeKind::ListLiteral { elems } => NodeKind::ListLiteral {
                elems: self.rewrite_vec(elems),
            },
            NodeKind::SetLiteral { elems } => NodeKind::SetLiteral {
                elems: self.rewrite_vec(elems),
            },
            NodeKind::TupleLiteral { elems } => NodeKind::TupleLiteral {
                elems: self.rewrite_vec(elems),
            },
            NodeKind::Pipe { left, right } => NodeKind::Pipe {
                left: self.rewrite_box(left),
                right: self.rewrite_box(right),
            },
            NodeKind::Compose { left, right } => NodeKind::Compose {
                left: self.rewrite_box(left),
                right: self.rewrite_box(right),
            },
            NodeKind::Range { lo, hi, inclusive } => NodeKind::Range {
                lo: self.rewrite_box(lo),
                hi: self.rewrite_box(hi),
                inclusive,
            },
            NodeKind::RecordConstruct {
                path,
                fields,
                spread,
            } => NodeKind::RecordConstruct {
                path,
                fields: fields
                    .into_iter()
                    .map(|f| bock_air::AirRecordField {
                        name: f.name,
                        value: f.value.map(|v| self.rewrite_box(v)),
                    })
                    .collect(),
                spread: spread.map(|s| self.rewrite_box(s)),
            },
            NodeKind::MapLiteral { entries } => NodeKind::MapLiteral {
                entries: entries
                    .into_iter()
                    .map(|e| bock_air::AirMapEntry {
                        key: self.rewrite(e.key),
                        value: self.rewrite(e.value),
                    })
                    .collect(),
            },
            NodeKind::Interpolation { parts } => NodeKind::Interpolation {
                parts: parts
                    .into_iter()
                    .map(|p| match p {
                        bock_air::AirInterpolationPart::Expr(e) => {
                            bock_air::AirInterpolationPart::Expr(self.rewrite_box(e))
                        }
                        lit @ bock_air::AirInterpolationPart::Literal(_) => lit,
                    })
                    .collect(),
            },
            NodeKind::ResultConstruct { variant, value } => NodeKind::ResultConstruct {
                variant,
                value: value.map(|v| self.rewrite_box(v)),
            },
            NodeKind::Break { value } => NodeKind::Break {
                value: value.map(|v| self.rewrite_box(v)),
            },
            NodeKind::EffectOp {
                effect,
                operation,
                args,
            } => NodeKind::EffectOp {
                effect,
                operation,
                args: self.rewrite_args(args),
            },
            NodeKind::HandlingBlock { handlers, body } => NodeKind::HandlingBlock {
                handlers: handlers
                    .into_iter()
                    .map(|h| bock_air::AirHandlerPair {
                        effect: h.effect,
                        handler: self.rewrite_box(h.handler),
                    })
                    .collect(),
                body: self.rewrite_box(body),
            },
            // No nested expression position a `.into()` could occupy.
            other => other,
        }
    }
}

/// Build an [`bock_air::NodeKind::Identifier`] holding `name` at `span`.
fn ident_node(name: &str, span: bock_errors::Span) -> NodeKind {
    NodeKind::Identifier {
        name: bock_ast::Ident {
            name: name.to_string(),
            span,
        },
    }
}

pub fn hoist_value_cf(module: AIRNode) -> AIRNode {
    let mut hoister = ValueCfHoister {
        next_id: max_node_id(&module) + 1,
        counter: 0,
        prelude: Vec::new(),
    };
    hoister.rewrite(module)
}

/// Largest [`bock_air::NodeId`] anywhere in `node`. The pre-pass mints fresh
/// ids above this so synthesised nodes never collide with existing ones.
fn max_node_id(node: &AIRNode) -> bock_air::NodeId {
    struct MaxId(bock_air::NodeId);
    impl bock_air::visitor::Visitor for MaxId {
        fn visit_node(&mut self, node: &AIRNode) {
            self.0 = self.0.max(node.id);
            bock_air::visitor::walk_node(self, node);
        }
    }
    let mut m = MaxId(0);
    use bock_air::visitor::Visitor;
    m.visit_node(node);
    m.0
}

struct ValueCfHoister {
    next_id: bock_air::NodeId,
    counter: u32,
    /// Statements to splice **before** the statement currently being rewritten.
    /// A value-position diverging CF pushes its `[temp decl, CF-as-stmt]` here
    /// and yields a temp-read; the enclosing block drains this per statement so
    /// the prelude lands in the right scope (never an IIFE — see [`hoist`]).
    prelude: Vec<AIRNode>,
}

impl ValueCfHoister {
    fn fresh_id(&mut self) -> bock_air::NodeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn fresh_temp_name(&mut self) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("__bock_cf_{n}")
    }

    fn node(&mut self, span: bock_errors::Span, kind: NodeKind) -> AIRNode {
        AIRNode::new(self.fresh_id(), span, kind)
    }

    /// Recursively rewrite a node, hoisting any value-position diverging CF it
    /// contains. Walks the whole tree so nested value positions are covered.
    fn rewrite(&mut self, mut node: AIRNode) -> AIRNode {
        node.kind = self.rewrite_kind(node.kind, node.span);
        node
    }

    fn rewrite_box(&mut self, node: Box<AIRNode>) -> Box<AIRNode> {
        Box::new(self.rewrite(*node))
    }

    /// Rewrite a node used in **value position**: if it is a diverging value-CF,
    /// hoist it into prelude statements (a declare-only temp + the CF in
    /// statement form) and return a read of the temp; otherwise recurse.
    fn rewrite_value(&mut self, node: AIRNode) -> AIRNode {
        if value_cf_diverges(&node) {
            self.hoist(node)
        } else {
            self.rewrite(node)
        }
    }

    fn rewrite_value_box(&mut self, node: Box<AIRNode>) -> Box<AIRNode> {
        Box::new(self.rewrite_value(*node))
    }

    /// Hoist a diverging value-CF `cf`: push `let mut __bock_cf_N` and the CF in
    /// statement form (value tails → `__bock_cf_N = v`, diverging tails kept)
    /// onto the prelude buffer, and return a read of `__bock_cf_N`. The prelude
    /// is later spliced into the enclosing statement list by [`rewrite_stmts`],
    /// so the diverging arms stay in the *enclosing* function/loop scope rather
    /// than being captured by an IIFE.
    fn hoist(&mut self, cf: AIRNode) -> AIRNode {
        let span = cf.span;
        let temp = self.fresh_temp_name();

        // `let mut __bock_cf_N` — declare-only (no initialiser). The value slot
        // carries a placeholder `Unreachable` that backends never emit because
        // the DECL_ONLY_META marker routes them to the bare declaration.
        let decl_pat = self.node(
            span,
            NodeKind::BindPat {
                name: bock_ast::Ident {
                    name: temp.clone(),
                    span,
                },
                is_mut: true,
            },
        );
        let placeholder = self.node(span, NodeKind::Unreachable);
        let mut decl = self.node(
            span,
            NodeKind::LetBinding {
                is_mut: true,
                pattern: Box::new(decl_pat),
                ty: None,
                value: Box::new(placeholder),
            },
        );
        decl.metadata.insert(
            DECL_ONLY_META.to_string(),
            bock_air::stubs::Value::Bool(true),
        );

        // The CF relocated to statement position, value tails → `temp = v`.
        let stmt_cf = self.rewrite_to_assign(cf, &temp);

        self.prelude.push(decl);
        self.prelude.push(stmt_cf);

        self.node(
            span,
            NodeKind::Identifier {
                name: bock_ast::Ident {
                    name: temp.clone(),
                    span,
                },
            },
        )
    }

    /// Rewrite a list of block statements, splicing each statement's hoist
    /// prelude (if any) immediately before it. Saves/restores the prelude buffer
    /// so a hoist inside one statement never leaks into a sibling.
    fn rewrite_stmts(&mut self, stmts: Vec<AIRNode>) -> Vec<AIRNode> {
        let mut out = Vec::with_capacity(stmts.len());
        for stmt in stmts {
            let saved = std::mem::take(&mut self.prelude);
            let rewritten = self.rewrite(stmt);
            let prelude = std::mem::replace(&mut self.prelude, saved);
            out.extend(prelude);
            out.push(rewritten);
        }
        out
    }

    /// Rewrite a function/lambda body whose **block tail is a value position**
    /// (the function's implicit return value). Unlike a bare statement block
    /// (see the `Block` arm of [`Self::rewrite_kind`]), the tail here is hoisted
    /// when it is a diverging value-CF — a function ending in `if c { v } else {
    /// return }` returns the `if`'s value, so it must become a temp. The hoist
    /// prelude is spliced into the body's statement list before the temp-read
    /// tail. Non-block bodies (a bare-expression lambda) are value-hoisted whole.
    fn rewrite_body(&mut self, body: Box<AIRNode>) -> Box<AIRNode> {
        let body = *body;
        let NodeKind::Block { stmts, tail } = body.kind else {
            return Box::new(self.rewrite_value(body));
        };
        let mut out_stmts = self.rewrite_stmts(stmts);
        let new_tail = match tail {
            Some(t) => {
                let saved = std::mem::take(&mut self.prelude);
                let rewritten = self.rewrite_value(*t);
                let prelude = std::mem::replace(&mut self.prelude, saved);
                out_stmts.extend(prelude);
                Some(Box::new(rewritten))
            }
            None => None,
        };
        Box::new(AIRNode::new(
            body.id,
            body.span,
            NodeKind::Block {
                stmts: out_stmts,
                tail: new_tail,
            },
        ))
    }

    /// Rewrite a (now statement-position) control-flow node so each value-
    /// yielding tail becomes `temp = <value>` and each diverging tail is kept.
    /// Recurses through nested `if`/`match`/`block`; a `loop`'s value arrives via
    /// `break <v>`, rewritten to `temp = <v>; break`.
    fn rewrite_to_assign(&mut self, node: AIRNode, temp: &str) -> AIRNode {
        let span = node.span;
        match node.kind {
            NodeKind::Block { stmts, tail } => {
                let mut stmts = self.rewrite_stmts(stmts);
                // The tail becomes `temp = <value>`; any prelude its value hoists
                // must land before that assignment, inside this block.
                if let Some(t) = tail {
                    let saved = std::mem::take(&mut self.prelude);
                    let assigned = self.rewrite_to_assign(*t, temp);
                    let prelude = std::mem::replace(&mut self.prelude, saved);
                    stmts.extend(prelude);
                    stmts.push(assigned);
                }
                AIRNode::new(node.id, span, NodeKind::Block { stmts, tail: None })
            }
            NodeKind::If {
                let_pattern,
                condition,
                then_block,
                else_block,
            } => {
                let condition = self.rewrite_box(condition);
                let then_block = Box::new(self.rewrite_to_assign(*then_block, temp));
                let else_block = else_block.map(|e| Box::new(self.rewrite_to_assign(*e, temp)));
                AIRNode::new(
                    node.id,
                    span,
                    NodeKind::If {
                        let_pattern,
                        condition,
                        then_block,
                        else_block,
                    },
                )
            }
            NodeKind::Match { scrutinee, arms } => {
                let scrutinee = self.rewrite_box(scrutinee);
                let arms = arms
                    .into_iter()
                    .map(|arm| match arm.kind {
                        NodeKind::MatchArm {
                            pattern,
                            guard,
                            body,
                        } => {
                            let body = Box::new(self.rewrite_to_assign(*body, temp));
                            AIRNode::new(
                                arm.id,
                                arm.span,
                                NodeKind::MatchArm {
                                    pattern,
                                    guard,
                                    body,
                                },
                            )
                        }
                        other => AIRNode::new(arm.id, arm.span, other),
                    })
                    .collect();
                AIRNode::new(node.id, span, NodeKind::Match { scrutinee, arms })
            }
            NodeKind::Loop { body } => {
                // The loop value arrives via `break <v>`; rewrite those to
                // `temp = <v>; break`. Nested loops own their own `break`s, so
                // the rewrite does not cross into them.
                let body = Box::new(self.rewrite_breaks_to_assign(*body, temp));
                AIRNode::new(node.id, span, NodeKind::Loop { body })
            }
            // A diverging tail (`return`/`break`/`continue`/diverging call): keep
            // verbatim (rewriting its sub-expressions for any nested hoists).
            _ if branch_tail_diverges(&AIRNode::new(node.id, span, node.kind.clone())) => {
                AIRNode::new(node.id, span, self.rewrite_kind(node.kind, span))
            }
            // A plain value tail: `temp = <value>`. A bare-expression arm body
            // (not a block) whose value itself hoists must keep that prelude with
            // the assignment, so wrap them in a block when a prelude was produced.
            _ => {
                let saved = std::mem::take(&mut self.prelude);
                let value = self.rewrite_value(AIRNode::new(node.id, span, node.kind));
                let prelude = std::mem::replace(&mut self.prelude, saved);
                let assign = self.assign_temp(temp, value, span);
                if prelude.is_empty() {
                    assign
                } else {
                    let mut stmts = prelude;
                    stmts.push(assign);
                    self.node(span, NodeKind::Block { stmts, tail: None })
                }
            }
        }
    }

    /// Within a value-`loop` body, rewrite `break <v>` → `{ temp = v; break }`.
    /// Does not descend into nested loops (their `break`s target themselves) or
    /// into functions/lambdas.
    fn rewrite_breaks_to_assign(&mut self, node: AIRNode, temp: &str) -> AIRNode {
        let span = node.span;
        match node.kind {
            NodeKind::Break { value: Some(v) } => {
                // `break <v>` → a flattenable splice block `{ temp = v; break }`.
                // The enclosing Block arm splices its statements inline so no
                // nested `{ … }` (which a backend treats as a value-IIFE) remains.
                let value = self.rewrite_value(*v);
                let assign = self.assign_temp(temp, value, span);
                let brk = self.node(span, NodeKind::Break { value: None });
                let mut blk = self.node(
                    span,
                    NodeKind::Block {
                        stmts: vec![assign, brk],
                        tail: None,
                    },
                );
                blk.metadata.insert(
                    SPLICE_BLOCK_META.to_string(),
                    bock_air::stubs::Value::Bool(true),
                );
                blk
            }
            NodeKind::Loop { .. }
            | NodeKind::While { .. }
            | NodeKind::For { .. }
            | NodeKind::FnDecl { .. }
            | NodeKind::Lambda { .. } => self.rewrite(AIRNode::new(node.id, span, node.kind)),
            NodeKind::Block { stmts, tail } => {
                let mut out: Vec<AIRNode> = Vec::with_capacity(stmts.len());
                for s in stmts {
                    let r = self.rewrite_breaks_to_assign(s, temp);
                    Self::splice_or_push(&mut out, r);
                }
                // A loop-body block's tail that contains a `break` is a diverging
                // statement (not a value), so the rewritten tail moves into the
                // statement list — keeping it out of value position (an IIFE).
                let new_tail = tail.and_then(|t| {
                    let rewritten = self.rewrite_breaks_to_assign(*t, temp);
                    Self::splice_or_push(&mut out, rewritten);
                    None
                });
                AIRNode::new(
                    node.id,
                    span,
                    NodeKind::Block {
                        stmts: out,
                        tail: new_tail,
                    },
                )
            }
            NodeKind::If {
                let_pattern,
                condition,
                then_block,
                else_block,
            } => {
                let condition = self.rewrite_box(condition);
                let then_block = Box::new(self.rewrite_breaks_to_assign(*then_block, temp));
                let else_block =
                    else_block.map(|e| Box::new(self.rewrite_breaks_to_assign(*e, temp)));
                AIRNode::new(
                    node.id,
                    span,
                    NodeKind::If {
                        let_pattern,
                        condition,
                        then_block,
                        else_block,
                    },
                )
            }
            NodeKind::Match { scrutinee, arms } => {
                let scrutinee = self.rewrite_box(scrutinee);
                let arms = arms
                    .into_iter()
                    .map(|arm| match arm.kind {
                        NodeKind::MatchArm {
                            pattern,
                            guard,
                            body,
                        } => {
                            let body = Box::new(self.rewrite_breaks_to_assign(*body, temp));
                            AIRNode::new(
                                arm.id,
                                arm.span,
                                NodeKind::MatchArm {
                                    pattern,
                                    guard,
                                    body,
                                },
                            )
                        }
                        other => AIRNode::new(arm.id, arm.span, other),
                    })
                    .collect();
                AIRNode::new(node.id, span, NodeKind::Match { scrutinee, arms })
            }
            other => self.rewrite(AIRNode::new(node.id, span, other)),
        }
    }

    /// Push `node` onto `out`, flattening a splice-flattenable block (from a
    /// `break <v>` rewrite) so its `{ temp = v; break }` statements land inline.
    fn splice_or_push(out: &mut Vec<AIRNode>, node: AIRNode) {
        if node.metadata.contains_key(SPLICE_BLOCK_META) {
            if let NodeKind::Block { stmts, tail } = node.kind {
                out.extend(stmts);
                if let Some(t) = tail {
                    out.push(*t);
                }
                return;
            }
        }
        out.push(node);
    }

    /// `temp = <value>` as an `Assign` node.
    fn assign_temp(&mut self, temp: &str, value: AIRNode, span: bock_errors::Span) -> AIRNode {
        let target = self.node(
            span,
            NodeKind::Identifier {
                name: bock_ast::Ident {
                    name: temp.to_string(),
                    span,
                },
            },
        );
        self.node(
            span,
            NodeKind::Assign {
                op: bock_ast::AssignOp::Assign,
                target: Box::new(target),
                value: Box::new(value),
            },
        )
    }

    /// Rewrite the children of a node kind, hoisting value-position children.
    fn rewrite_kind(&mut self, kind: NodeKind, _span: bock_errors::Span) -> NodeKind {
        match kind {
            NodeKind::Module {
                path,
                annotations,
                imports,
                items,
            } => NodeKind::Module {
                path,
                annotations,
                imports: imports.into_iter().map(|n| self.rewrite(n)).collect(),
                items: items.into_iter().map(|n| self.rewrite(n)).collect(),
            },
            NodeKind::FnDecl {
                annotations,
                visibility,
                is_async,
                name,
                generic_params,
                params,
                return_type,
                effect_clause,
                where_clause,
                body,
            } => NodeKind::FnDecl {
                annotations,
                visibility,
                is_async,
                name,
                generic_params,
                params: params.into_iter().map(|p| self.rewrite(p)).collect(),
                return_type,
                effect_clause,
                where_clause,
                body: self.rewrite_body(body),
            },
            NodeKind::ClassDecl {
                annotations,
                visibility,
                name,
                generic_params,
                base,
                traits,
                fields,
                methods,
            } => NodeKind::ClassDecl {
                annotations,
                visibility,
                name,
                generic_params,
                base,
                traits,
                fields,
                methods: methods.into_iter().map(|m| self.rewrite(m)).collect(),
            },
            NodeKind::TraitDecl {
                annotations,
                visibility,
                is_platform,
                name,
                generic_params,
                associated_types,
                methods,
            } => NodeKind::TraitDecl {
                annotations,
                visibility,
                is_platform,
                name,
                generic_params,
                associated_types,
                methods: methods.into_iter().map(|m| self.rewrite(m)).collect(),
            },
            NodeKind::ImplBlock {
                annotations,
                generic_params,
                trait_path,
                trait_args,
                target,
                where_clause,
                methods,
            } => NodeKind::ImplBlock {
                annotations,
                generic_params,
                trait_path,
                trait_args,
                target,
                where_clause,
                methods: methods.into_iter().map(|m| self.rewrite(m)).collect(),
            },
            NodeKind::EffectDecl {
                annotations,
                visibility,
                name,
                generic_params,
                components,
                operations,
            } => NodeKind::EffectDecl {
                annotations,
                visibility,
                name,
                generic_params,
                components,
                operations: operations.into_iter().map(|o| self.rewrite(o)).collect(),
            },
            NodeKind::ConstDecl {
                annotations,
                visibility,
                name,
                ty,
                value,
            } => NodeKind::ConstDecl {
                annotations,
                visibility,
                name,
                ty,
                value: self.rewrite_value_box(value),
            },
            NodeKind::PropertyTest {
                name,
                bindings,
                body,
            } => NodeKind::PropertyTest {
                name,
                bindings,
                body: self.rewrite_box(body),
            },
            NodeKind::LetBinding {
                is_mut,
                pattern,
                ty,
                value,
            } => NodeKind::LetBinding {
                is_mut,
                pattern,
                ty,
                value: self.rewrite_value_box(value),
            },
            NodeKind::Assign { op, target, value } => NodeKind::Assign {
                op,
                target,
                value: self.rewrite_value_box(value),
            },
            NodeKind::Return { value } => NodeKind::Return {
                value: value.map(|v| self.rewrite_value_box(v)),
            },
            NodeKind::Break { value } => NodeKind::Break {
                value: value.map(|v| self.rewrite_value_box(v)),
            },
            NodeKind::Call {
                callee,
                args,
                type_args,
            } => NodeKind::Call {
                callee: self.rewrite_box(callee),
                args: args.into_iter().map(|a| self.rewrite_arg(a)).collect(),
                type_args,
            },
            NodeKind::MethodCall {
                receiver,
                method,
                type_args,
                args,
            } => NodeKind::MethodCall {
                receiver: self.rewrite_box(receiver),
                method,
                type_args,
                args: args.into_iter().map(|a| self.rewrite_arg(a)).collect(),
            },
            NodeKind::Block { stmts, tail } => {
                // A block's tail is hoisted only when the block *itself* is in a
                // value position — which the enclosing value consumer detects via
                // `value_cf_diverges` (it recurses into the block tail) and then
                // routes through `hoist`/`rewrite_to_assign`. Here (a bare /
                // statement-position block) the tail is just recursed into, never
                // hoisted: a `match`/`if` whose *result is discarded* (e.g. a
                // statement-position `match s { … => return …, _ => {} }`) must
                // not be turned into a temp it never assigns.
                let out_stmts = self.rewrite_stmts(stmts);
                let tail = tail.map(|t| self.rewrite_box(t));
                NodeKind::Block {
                    stmts: out_stmts,
                    tail,
                }
            }
            NodeKind::If {
                let_pattern,
                condition,
                then_block,
                else_block,
            } => NodeKind::If {
                let_pattern,
                condition: self.rewrite_box(condition),
                then_block: self.rewrite_box(then_block),
                else_block: else_block.map(|e| self.rewrite_box(e)),
            },
            NodeKind::Match { scrutinee, arms } => NodeKind::Match {
                scrutinee: self.rewrite_box(scrutinee),
                arms: arms.into_iter().map(|a| self.rewrite(a)).collect(),
            },
            NodeKind::MatchArm {
                pattern,
                guard,
                body,
            } => NodeKind::MatchArm {
                pattern,
                guard,
                body: self.rewrite_box(body),
            },
            NodeKind::For {
                pattern,
                iterable,
                body,
            } => NodeKind::For {
                pattern,
                iterable: self.rewrite_box(iterable),
                body: self.rewrite_box(body),
            },
            NodeKind::While { condition, body } => NodeKind::While {
                condition: self.rewrite_box(condition),
                body: self.rewrite_box(body),
            },
            NodeKind::Loop { body } => NodeKind::Loop {
                body: self.rewrite_box(body),
            },
            NodeKind::Guard {
                let_pattern,
                condition,
                else_block,
            } => NodeKind::Guard {
                let_pattern,
                condition: self.rewrite_box(condition),
                else_block: self.rewrite_box(else_block),
            },
            NodeKind::HandlingBlock { handlers, body } => NodeKind::HandlingBlock {
                handlers,
                body: self.rewrite_box(body),
            },
            NodeKind::Lambda { params, body } => NodeKind::Lambda {
                params,
                body: self.rewrite_body(body),
            },
            NodeKind::BinaryOp { op, left, right } => NodeKind::BinaryOp {
                op,
                left: self.rewrite_box(left),
                right: self.rewrite_box(right),
            },
            NodeKind::UnaryOp { op, operand } => NodeKind::UnaryOp {
                op,
                operand: self.rewrite_box(operand),
            },
            NodeKind::FieldAccess { object, field } => NodeKind::FieldAccess {
                object: self.rewrite_box(object),
                field,
            },
            NodeKind::Index { object, index } => NodeKind::Index {
                object: self.rewrite_box(object),
                index: self.rewrite_box(index),
            },
            NodeKind::Propagate { expr } => NodeKind::Propagate {
                expr: self.rewrite_box(expr),
            },
            NodeKind::Await { expr } => NodeKind::Await {
                expr: self.rewrite_box(expr),
            },
            NodeKind::Move { expr } => NodeKind::Move {
                expr: self.rewrite_box(expr),
            },
            NodeKind::Borrow { expr } => NodeKind::Borrow {
                expr: self.rewrite_box(expr),
            },
            NodeKind::MutableBorrow { expr } => NodeKind::MutableBorrow {
                expr: self.rewrite_box(expr),
            },
            // Leaf nodes and node kinds with no value-position children: kept
            // verbatim. (Type expressions, literals, identifiers, patterns,
            // collection literals — collection element/record-field hoisting is
            // out of scope; the diverging-CF shapes never appear there in the
            // exercised examples, and hoisting them would change evaluation
            // order.)
            other => other,
        }
    }

    fn rewrite_arg(&mut self, arg: AirArg) -> AirArg {
        AirArg {
            label: arg.label,
            value: self.rewrite_value(arg.value),
        }
    }
}

/// Decide whether a loop must be given a target label so that a `break`/
/// `continue` inside a statement-arm `match` reaches the loop rather than the
/// `switch` the `match` lowers to.
///
/// In Go and JS/TS, `break` inside a `switch` exits the switch. When a
/// statement-arm `match` (lowered to a `switch`) contains a `break` (or, in
/// Go-style lowering, a `continue`) intended for an enclosing loop, the loop
/// needs a label and the jump must be labelled. This returns true when the
/// loop body contains — without crossing into a nested loop or function — a
/// `match` with a statement arm that performs a `break`/`continue`.
#[must_use]
pub fn loop_needs_break_label(body: &AIRNode) -> bool {
    fn arm_has_jump(node: &AIRNode) -> bool {
        match &node.kind {
            NodeKind::Break { .. } | NodeKind::Continue => true,
            NodeKind::For { .. }
            | NodeKind::While { .. }
            | NodeKind::Loop { .. }
            | NodeKind::FnDecl { .. }
            | NodeKind::Lambda { .. } => false,
            NodeKind::Block { stmts, tail } => {
                stmts.iter().any(arm_has_jump) || tail.as_deref().is_some_and(arm_has_jump)
            }
            NodeKind::If {
                then_block,
                else_block,
                ..
            } => arm_has_jump(then_block) || else_block.as_deref().is_some_and(arm_has_jump),
            NodeKind::Match { arms, .. } => arms
                .iter()
                .any(|a| matches!(&a.kind, NodeKind::MatchArm { body, .. } if arm_has_jump(body))),
            NodeKind::Guard { else_block, .. } => arm_has_jump(else_block),
            _ => false,
        }
    }
    fn find(node: &AIRNode) -> bool {
        match &node.kind {
            NodeKind::For { .. }
            | NodeKind::While { .. }
            | NodeKind::Loop { .. }
            | NodeKind::FnDecl { .. }
            | NodeKind::Lambda { .. } => false,
            NodeKind::Match { arms, .. } => match_has_statement_arm(arms)
                && arms.iter().any(
                    |a| matches!(&a.kind, NodeKind::MatchArm { body, .. } if arm_has_jump(body)),
                ),
            NodeKind::Block { stmts, tail } => {
                stmts.iter().any(find) || tail.as_deref().is_some_and(find)
            }
            NodeKind::If {
                then_block,
                else_block,
                ..
            } => find(then_block) || else_block.as_deref().is_some_and(find),
            NodeKind::Guard { else_block, .. } => find(else_block),
            _ => false,
        }
    }
    find(body)
}

/// If `param` is a method parameter that binds the receiver `self`, return
/// `Some(is_mut)` carrying its mutability; otherwise `None`.
///
/// The AIR keeps `self` as an ordinary leading `Param` whose pattern is a
/// `BindPat { name: "self" }`. Backends with native receivers (Rust `&self` /
/// `&mut self`, Go `func (self *T)`, Python `self`) consume this param to emit
/// the receiver and must not also emit it as a normal positional parameter.
#[must_use]
pub fn param_binds_self(param: &AIRNode) -> Option<bool> {
    let NodeKind::Param { pattern, .. } = &param.kind else {
        return None;
    };
    if let NodeKind::BindPat { name, is_mut } = &pattern.kind {
        if name.name == "self" {
            return Some(*is_mut);
        }
    }
    None
}

/// Recognise a *desugared instance method call*.
///
/// The AIR lowerer rewrites `recv.method(args)` into
/// `Call { callee: FieldAccess(recv, method), args: [recv, ...args] }`, cloning
/// the receiver into both the field-access object and the leading argument
/// (so they share a [`NodeId`](bock_air::NodeId)). This helper detects that
/// shape — callee is a `FieldAccess` whose object is identical to the first
/// argument — and returns the receiver, the method name, and the remaining
/// (non-self) arguments. Targets with native method receivers (Rust, Go,
/// Python) use this to emit `recv.method(rest)` instead of double-passing the
/// receiver. Associated calls (`Type.method(...)`) prepend no self and are not
/// matched here.
#[must_use]
pub fn desugared_self_call<'a>(
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a bock_ast::Ident, &'a [AirArg])> {
    let NodeKind::FieldAccess { object, field } = &callee.kind else {
        return None;
    };
    let first = args.first()?;
    // The lowerer clones the receiver into both positions, so the self arg
    // and the field-access object are the *same* node (same NodeId). A genuine
    // `(p.f)(p)` field-closure call would have two distinct receiver nodes.
    if first.value.id == object.id {
        Some((object.as_ref(), field, &args[1..]))
    } else {
        None
    }
}

/// The read-only / non-mutating `List` built-in methods this codegen lowers
/// natively per target (see [`desugared_list_method`]). The in-place mutators
/// are excluded: `push`/`append` lower via [`desugared_list_mutating_method`]
/// (DQ18) and `pop`/`remove_at`/`insert`/`reverse`/`set` via
/// [`desugared_list_inplace_mutator`] (DQ30).
pub const READ_ONLY_LIST_METHODS: &[&str] = &[
    "len", "length", "count", "is_empty", "get", "contains", "first", "last", "concat", "index_of",
    "join",
];

/// The in-place `List` mutators (DQ18) lowered natively per target via
/// [`desugared_list_mutating_method`]. These resolve in the checker to a `Void`
/// return and require a `mut` receiver (enforced by the ownership pass), so each
/// backend emits them in *statement position* as a value-less mutation:
///
/// - rust / js / ts: `recv.push(x)`
/// - python: `recv.append(x)`
/// - go: `recv = append(recv, x)` (slice growth is reassignment in Go; the `mut`
///   receiver guarantees `recv` is a valid lvalue — a `let mut` binding or a
///   mutable field place)
///
/// `append` is Bock's spelling alias for `push`; both lower identically.
pub const MUTATING_LIST_METHODS: &[&str] = &["push", "append"];

/// The raw `recv_kind` annotation tag the checker stamped on a desugared method
/// call node, if any.
///
/// Returns the verbatim tag string (`"List"`, `"User:Counter"`,
/// `"Primitive:Int"`, …) without stripping any prefix, or `None` when the node
/// carries no `recv_kind` stamp. This is the unprefixed sibling of
/// [`primitive_recv_kind`] / [`trait_bound_recv_kind`], used where a recogniser
/// needs to *distinguish* its own receiver category from any other stamped one
/// (e.g. the built-in `List` recogniser ruling out a same-named user-record
/// method).
#[must_use]
pub fn raw_recv_kind(node: &AIRNode) -> Option<&str> {
    let bock_air::Value::String(tag) =
        node.metadata.get(bock_types::checker::RECV_KIND_META_KEY)?
    else {
        return None;
    };
    Some(tag.as_str())
}

/// True when `node` is a `Call` the lowerer classified as an
/// **associated-function call** (`Type.method(args)` — no `self` prepended), via
/// the [`bock_air::lower::ASSOC_CALL_META_KEY`] stamp.
///
/// Backends use this to emit a static / free-function call keyed by the type
/// name (`Type.method(args)`) instead of the value-receiver method form their
/// generic fall-through would produce — which camel-cases the type name into a
/// non-existent value (`typeValue.method(...)`). The companion to
/// [`assoc_fn_def`], which recognises the matching *definition* shape.
#[must_use]
pub fn is_associated_call(node: &AIRNode) -> bool {
    matches!(
        node.metadata.get(bock_air::lower::ASSOC_CALL_META_KEY),
        Some(bock_air::Value::Bool(true))
    )
}

/// The set of primitive type names that can appear as the *callee* of a
/// canonical primitive associated conversion (`Prim.from(x)` / `Prim.try_from`).
///
/// Restricted to the conversion *targets* the canonical matrix defines
/// (`register_canonical_conversions`): `Int`/`Float` (numeric widening +
/// `TryFrom[String]`), `String` (`From[Char]`). Sized primitives (`Int64`, …)
/// are conversion targets too, but are not in the resolver's type-name vocab,
/// so they never reach codegen as an associated-call callee; `Int`/`Float`/
/// `String` are the v1-reachable callees.
pub const PRIMITIVE_CONVERSION_TARGETS: &[&str] = &["Int", "Float", "String"];

/// Q-prim-assoc: when `node` is a **primitive** associated-conversion call
/// (`Float.from(x)` / `Int.try_from(s)` / `String.from(c)`), returns
/// `(target_prim_name, method, arg)`, where `method` is `"from"` or
/// `"try_from"` and `arg` is the single source-value argument.
///
/// Such a call is stamped [`is_associated_call`] and has the callee shape
/// `FieldAccess(Identifier(Prim), method)` where `Prim` is a
/// [`PRIMITIVE_CONVERSION_TARGETS`] name. Backends emit each target's native
/// conversion rather than the generic associated-call form (`Float.from(x)`),
/// which references a non-existent member on the host primitive (`float.from` is
/// a syntax error in Python; `Float`/`Float_from` are undefined in JS/Go/Rust).
/// Returns `None` for any other call, including user-type associated calls
/// (`Fahrenheit.from(c)`) and non-conversion methods, which keep their existing
/// lowering.
#[must_use]
pub fn primitive_conversion_call<'a>(
    node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a str, &'a str, &'a AIRNode)> {
    if !is_associated_call(node) {
        return None;
    }
    let NodeKind::FieldAccess { object, field } = &callee.kind else {
        return None;
    };
    let NodeKind::Identifier { name } = &object.kind else {
        return None;
    };
    let target = PRIMITIVE_CONVERSION_TARGETS
        .iter()
        .copied()
        .find(|&p| p == name.name)?;
    let method = match field.name.as_str() {
        m @ ("from" | "try_from") => m,
        _ => return None,
    };
    let arg = args.first().map(|a| &a.value)?;
    if args.len() != 1 {
        return None;
    }
    Some((target, method, arg))
}

/// True when an impl/trait `method` (an [`bock_air::NodeKind::FnDecl`]) is an
/// **associated function** — it does not bind a leading `self` receiver, so it
/// is reached as `Type.method(...)` rather than `value.method(...)`.
///
/// Such a method must be emitted as a static / free function (no synthesized
/// receiver, no spurious `self` parameter) on every backend; otherwise the
/// generic impl-method path attaches it as an instance method and the
/// associated call cannot resolve it. The companion to [`is_associated_call`],
/// which recognises the matching *call* shape.
#[must_use]
pub fn assoc_fn_def(method: &AIRNode) -> bool {
    let NodeKind::FnDecl { params, .. } = &method.kind else {
        return false;
    };
    match params.first() {
        Some(first) => param_binds_self(first).is_none(),
        // A zero-parameter impl method (`fn origin() -> T`) binds no `self`.
        None => true,
    }
}

/// True when an impl/trait `method` should be emitted as an associated function
/// — [`assoc_fn_def`] holds **and** the method is not an effect operation.
///
/// An **effect** operation (`effect Log { fn log(message: String) }`) also lacks
/// a `self` receiver, but a handler's `impl Log for ConsoleLog { fn log(...) }`
/// is an *instance* method: it is dispatched as `handler.log(...)` and must
/// satisfy the effect's interface, so it cannot be a static / free function.
/// `effect_ops` maps each known effect operation name to its effect (seeded
/// from every `EffectDecl` before emission), so a method whose name is a key is
/// an effect op and is kept as an instance method.
#[must_use]
pub fn is_associated_impl_method(method: &AIRNode, effect_ops: &HashMap<String, String>) -> bool {
    if !assoc_fn_def(method) {
        return false;
    }
    if let NodeKind::FnDecl { name, .. } = &method.kind {
        if effect_ops.contains_key(&name.name) {
            return false;
        }
    }
    true
}

/// True when a `BinaryOp { op: Add, left, right }` is **list concatenation** and
/// must be lowered to the target's concat idiom rather than a native `+`.
///
/// Two independent signals, either of which suffices:
///
/// 1. The checker's [`bock_types::checker::LIST_CONCAT_META_KEY`] stamp — set on
///    a `+` whose operands resolved to `List[T]`. This is the precise signal for
///    every `+` the checker's body pass reaches.
/// 2. A *syntactic* fallback: one operand is a list literal (`xs + [todo]` /
///    `[head] + tail`). A list literal can only be `+`-combined with another
///    list (numeric/string `+` never has a `[...]` operand), so this is
///    unambiguous — and it covers `+` sites the checker's body pass does not
///    currently visit (e.g. inside `impl` method bodies, which are not yet
///    type-checked), where the stamp is absent.
///
/// Each backend calls this from its `NodeKind::BinaryOp { op: Add, .. }` arm. See
/// the metadata key's docs for the per-target lowering rationale.
#[must_use]
pub fn is_list_concat(node: &AIRNode, left: &AIRNode, right: &AIRNode) -> bool {
    let stamped = matches!(
        node.metadata.get(bock_types::checker::LIST_CONCAT_META_KEY),
        Some(bock_air::Value::Bool(true))
    );
    let has_list_literal = matches!(left.kind, NodeKind::ListLiteral { .. })
        || matches!(right.kind, NodeKind::ListLiteral { .. });
    stamped || has_list_literal
}

/// True when a `BinaryOp { op: Div | Rem, .. }` is **integer** division /
/// remainder and must be lowered to DQ23's cross-target integer semantics (§3.6)
/// rather than the target's native `/` / `%`.
///
/// The signal is the checker's [`bock_types::checker::INT_ARITH_META_KEY`] stamp,
/// set on a `/` or `%` whose two operands both resolved to an integer primitive.
/// A purely syntactic codegen check cannot see that bare identifiers (`a / b`)
/// are integer-typed, so — unlike [`is_list_concat`], which has a list-literal
/// fallback — there is no syntactic fallback here: the stamp is the sole signal.
///
/// Each backend that diverges from the contract on its native operator (JS/TS:
/// float `/`, no zero-abort; Python: floor `//` and floor-`%`) calls this from
/// its `NodeKind::BinaryOp { op: Div | Rem, .. }` arm. Rust and Go already match
/// the contract with native `/` / `%`, so they ignore it. See the metadata key's
/// docs for the per-target lowering rationale.
#[must_use]
pub fn is_int_arith(node: &AIRNode) -> bool {
    matches!(
        node.metadata.get(bock_types::checker::INT_ARITH_META_KEY),
        Some(bock_air::Value::Bool(true))
    )
}

/// True when a `BinaryOp { op: Lt | Le | Gt | Ge, .. }` is an **ordering
/// comparison on a user `Comparable` type** and must be lowered through the
/// type's `compare(self, other)` method rather than the target's native
/// `<` / `<=` / `>` / `>=`.
///
/// The signal is the checker's [`bock_types::checker::USER_COMPARE_META_KEY`]
/// stamp, set on an ordering operator whose operands resolved to a `Named`
/// (record / class) type implementing `Comparable`. A purely syntactic codegen
/// check cannot see that bare identifiers (`a < b`) are a user `Comparable` type,
/// so — like [`is_int_arith`] — the stamp is the sole signal: there is no
/// syntactic fallback.
///
/// Every backend consults this from its `NodeKind::BinaryOp` arm: native `<` on
/// two user values is broken on all five targets (Python `TypeError`, Rust/Go
/// non-comparable structs, JS `NaN`-coercion). Mapping the operator onto the
/// `compare` result (`<` ⇒ `== Less`, `>` ⇒ `== Greater`, `<=` ⇒ `!= Greater`,
/// `>=` ⇒ `!= Less`) reuses the per-target `Ordering` representation the stdlib
/// already emits. See the metadata key's docs for the rationale.
#[must_use]
pub fn is_user_compare(node: &AIRNode) -> bool {
    matches!(
        node.metadata
            .get(bock_types::checker::USER_COMPARE_META_KEY),
        Some(bock_air::Value::Bool(true))
    )
}

/// The DQ29 equality lane a `BinaryOp { op: Eq | Ne, .. }` node was stamped
/// with by the checker ([`bock_types::checker::USER_EQ_META_KEY`]), or `None`
/// for an unstamped (native-equality) comparison.
///
/// Lanes: `"impl"` (explicit `impl Equatable` — dispatch through the type's
/// `eq`), `"structural"` (record/enum/tuple shape — JS/TS need the `__bockEq`
/// helper; natively-structural targets keep `==`), `"deep"` (involves a
/// collection — JS/TS *and* Go route through their deep-equality helpers), and
/// `"generic"` (bounded type var — JS/TS route through `__bockEq`). See the
/// metadata key's docs for the per-target rationale. Like [`is_user_compare`],
/// the stamp is the sole signal: codegen has no type information of its own.
#[must_use]
pub fn user_eq_kind(node: &AIRNode) -> Option<&str> {
    match node.metadata.get(bock_types::checker::USER_EQ_META_KEY) {
        Some(bock_air::Value::String(kind)) => Some(kind.as_str()),
        _ => None,
    }
}

/// True when a `RecordDecl` / `EnumDecl` node carries the checker's
/// [`bock_types::checker::DERIVE_EQ_META_KEY`] stamp — the type conforms to
/// `Equatable` structurally (DQ29) and declares no explicit impl, so the Rust
/// backend adds `PartialEq` to its `#[derive(..)]` list.
#[must_use]
pub fn derives_structural_eq(node: &AIRNode) -> bool {
    matches!(
        node.metadata.get(bock_types::checker::DERIVE_EQ_META_KEY),
        Some(bock_air::Value::Bool(true))
    )
}

/// Map an ordering [`BinOp`](bock_ast::BinOp) (`<` / `<=` / `>` / `>=`) onto the `Ordering`
/// variant name and whether the comparison is an *equality* (`true`) or
/// *inequality* (`false`) against it, for lowering a user-`Comparable`
/// comparison through `compare`:
///
/// | op   | variant     | equality |
/// |------|-------------|----------|
/// | `<`  | `"Less"`    | `true`   |
/// | `>`  | `"Greater"` | `true`   |
/// | `<=` | `"Greater"` | `false`  |
/// | `>=` | `"Less"`    | `false`  |
///
/// `a < b` ⇒ `compare == Less`, `a <= b` ⇒ `compare != Greater`, etc. Returns
/// `None` for any non-ordering operator (the caller only invokes it after
/// [`is_user_compare`], which already restricts to the four ordering ops).
#[must_use]
pub fn user_compare_variant(op: bock_ast::BinOp) -> Option<(&'static str, bool)> {
    use bock_ast::BinOp;
    match op {
        BinOp::Lt => Some(("Less", true)),
        BinOp::Gt => Some(("Greater", true)),
        BinOp::Le => Some(("Greater", false)),
        BinOp::Ge => Some(("Less", false)),
        _ => None,
    }
}

/// True when an expression node is a `Bool` value that must stringify to the
/// canonical lowercase `"true"` / `"false"` (§3.5) — the checker stamped it with
/// [`bock_types::checker::BOOL_STRINGIFY_META_KEY`] because it appears as an
/// `${expr}` interpolation part of `Bool` type.
///
/// Only the Python backend consults this (its `f"{b}"` prints `True`/`False`);
/// JS/TS template literals and Rust/Go formatting already print lowercase.
#[must_use]
pub fn is_bool_stringify(node: &AIRNode) -> bool {
    matches!(
        node.metadata
            .get(bock_types::checker::BOOL_STRINGIFY_META_KEY),
        Some(bock_air::Value::Bool(true))
    )
}

/// Recognise a *desugared `List` built-in method call*.
///
/// Building on the same desugared shape [`desugared_self_call`] detects
/// (`Call { callee: FieldAccess(recv, method), args: [recv, ...rest] }`), this
/// helper additionally requires that `method` is one of the read-only `List`
/// built-ins ([`READ_ONLY_LIST_METHODS`]). It is the shared recogniser each
/// backend wires into its `Call` arm *before* the generic
/// [`desugared_self_call`] / fall-through, so `nums.len()`, `nums.get(i)`,
/// `nums.contains(x)`, etc. are lowered to the target's idiomatic form (e.g.
/// `(nums).length`, a tagged-`Optional` bounds check, …) rather than emitted
/// verbatim as `nums.len(nums)` — which would fail at the target's
/// runtime/compile step.
///
/// `call_node` is the full `Call` AIR node (it holds the `recv_kind`
/// annotation); `callee`/`args` are its fields, passed separately so a backend
/// can call this from inside its `NodeKind::Call { callee, args, .. }` arm.
///
/// Unlike the `Optional`/`Result`/`Map`/`Set` recognisers — which fire *only*
/// on their exact `recv_kind` stamp — this one accepts both a `recv_kind =
/// "List"` stamp *and an absent stamp* (the checker leaves the receiver
/// untagged when its type is an unresolved inference variable, and several
/// existing list fixtures rely on that fall-through). It does, however, *reject*
/// a call carrying any *other* stamp: that rules out a same-named method on a
/// user record (`recv_kind = "User:Counter"`), a primitive, or another
/// container, so a user-defined `len()`/`is_empty()`/`contains(...)` falls
/// through to the user-method path instead of being shadowed by the built-in
/// `List` lowering.
///
/// Returns the receiver, the (validated) method name, and the remaining
/// (non-self) arguments. The element type of the list is intentionally *not*
/// inspected here: the checker has already type-checked the call, and each
/// backend's lowering is element-type-agnostic for these methods.
#[must_use]
pub fn desugared_list_method<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg])> {
    // A stamp other than "List" means the receiver is a user type / primitive /
    // other container; the built-in List lowering must not shadow it. An absent
    // stamp keeps the historical name-only behaviour (unresolved receiver type).
    if !matches!(raw_recv_kind(call_node), None | Some("List")) {
        return None;
    }
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if READ_ONLY_LIST_METHODS.contains(&method) {
        Some((recv, method, rest))
    } else {
        None
    }
}

/// Recognise a *desugared in-place `List` mutator call* (`push`/`append`, DQ18).
///
/// The mutating sibling of [`desugared_list_method`]: same desugared shape
/// (`Call { callee: FieldAccess(recv, method), args: [recv, x] }`), same
/// `recv_kind`-gating (a `recv_kind = "List"` stamp *or* an absent stamp; any
/// other stamp — a user record, a `Map`/`Set`, a primitive — is rejected so a
/// same-named user method is not shadowed), but the method must be one of
/// [`MUTATING_LIST_METHODS`]. Returns the receiver, the validated method name,
/// and the remaining (non-self) arguments (the single pushed element).
///
/// The checker types these calls as `Void` and the ownership pass guarantees the
/// receiver is a `mut` lvalue, so each backend wires this into its `Call` arm
/// (alongside [`desugared_list_method`]) and lowers it to the target's in-place
/// idiom in statement position.
#[must_use]
pub fn desugared_list_mutating_method<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg])> {
    if !matches!(raw_recv_kind(call_node), None | Some("List")) {
        return None;
    }
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if MUTATING_LIST_METHODS.contains(&method) {
        Some((recv, method, rest))
    } else {
        None
    }
}

/// The DQ30 in-place `List` mutators, lowered natively per target via
/// [`desugared_list_inplace_mutator`]. All are `mut self`
/// (`E5004`-enforced like DQ18's `push`/`append`); the per-method contracts:
///
/// - `pop() -> Optional[T]` — removes/returns the **last** element; `None` on
///   empty (emptiness is a normal state, never an abort);
/// - `remove_at(index) -> T` — removes/returns the element at `index`;
///   out-of-bounds (including negative) **aborts** (§10.5 Panic);
/// - `insert(index, value) -> Void` — inserts before `index`; valid range
///   `0..=len` (`len` is the append position); out-of-bounds aborts —
///   explicitly NOT Python's native clamp;
/// - `reverse() -> Void` — reverses in place;
/// - `set(index, value) -> Void` — overwrites the element at `index`;
///   out-of-bounds aborts (JS's native silent array extension and Python's
///   negative indexing are both excluded by explicit bounds checks).
///
/// The synthesized abort checks (js/ts/python/go) throw/raise/panic with the
/// normalized message `List.<op>: index <i> out of bounds (len <n>)`; the Rust
/// backend keeps `Vec`'s native panics (which carry the index and length), per
/// the DQ23 native-abort convention.
pub const INPLACE_LIST_MUTATORS: &[&str] = &["pop", "remove_at", "insert", "reverse", "set"];

/// Recognise a *desugared DQ30 in-place `List` mutator call*
/// (`pop`/`remove_at`/`insert`/`reverse`/`set`).
///
/// The DQ30 sibling of [`desugared_list_mutating_method`]: same desugared shape
/// (`Call { callee: FieldAccess(recv, method), args: [recv, ...rest] }`), but
/// the method must be one of [`INPLACE_LIST_MUTATORS`] — and `set` additionally
/// requires the **explicit** `recv_kind = "List"` stamp (never the absent-stamp
/// fall-through), because `set(k, v)` is also a live `Map` method and an
/// unstamped receiver must not be claimed by the `List` lowering. The other
/// four names are `List`-only today, so they keep the DQ18 `None | "List"`
/// gating (the checker leaves an unresolved-inference receiver unstamped).
///
/// Returns the receiver, the validated method name, and the remaining
/// (non-self) arguments. The ownership pass guarantees the receiver is a `mut`
/// lvalue (E5004), so each backend may mutate the receiver *place* in its
/// lowering (JS/TS/Python mutate the reference; Go reassigns through a
/// pointer where the length changes; Rust borrows `&mut` natively).
#[must_use]
pub fn desugared_list_inplace_mutator<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg])> {
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if !INPLACE_LIST_MUTATORS.contains(&method) {
        return None;
    }
    let gate_ok = if method == "set" {
        matches!(raw_recv_kind(call_node), Some("List"))
    } else {
        matches!(raw_recv_kind(call_node), None | Some("List"))
    };
    if gate_ok {
        Some((recv, method, rest))
    } else {
        None
    }
}

/// The *functional* `List` built-in methods that take a closure argument and
/// must be lowered to each target's native iteration idiom (see
/// [`desugared_list_functional_method`]).
///
/// These resolve in the checker to a concrete return type with a fully typed
/// closure parameter (see `resolve_builtin_method_fn_type` for `List`), but the
/// receiver type `List[T]` has no `.map`/`.filter`/`.reduce`/… method in *any*
/// target — JS/TS arrays have `.map`/`.filter`/`.reduce` but not the desugared
/// `recv.map(recv, cb)` shape; Python lists, Rust `Vec`, and Go slices have no
/// such methods at all. Without a dedicated lowering these fall through to the
/// generic desugared-self-call path, which emits `recv.map(recv, cb)` —
/// array-not-a-callback on TS, "x.map is not a function" on JS, `'list' object
/// has no attribute 'map'` on Python, `no method 'map' for Vec` on Rust, and a
/// keyword/selector parse error on Go (`map` is reserved). This is the surface
/// counterpart to the `core.iter` *free functions* (`map`/`filter`/`fold`/…
/// over `ListIterator[T]`), which already lower correctly.
///
/// The set mirrors the closure-taking `List` methods the checker resolves:
/// `map`/`filter`/`reduce`/`fold`/`for_each`/`find`/`any`/`all`/`flat_map`. The
/// no-closure functional combinators (`take`/`skip`/`reverse`/`sort`/`dedup`/
/// `enumerate`/`zip`/`flatten`/`to_set`/`push`/`pop`/…) are intentionally NOT in
/// this set: they are either non-closure transforms or mutating methods left to
/// their existing paths (DQ18).
pub const FUNCTIONAL_LIST_METHODS: &[&str] = &[
    "map", "filter", "reduce", "fold", "for_each", "find", "any", "all", "flat_map",
];

/// Recognise a *desugared `List` functional (closure-taking) method call*.
///
/// The functional sibling of [`desugared_list_method`]: same desugared shape
/// (`Call { callee: FieldAccess(recv, method), args: [recv, closure, …] }`),
/// same `recv_kind`-gating (accepts a `recv_kind = "List"` stamp *or* an absent
/// stamp, rejects any other stamp so a same-named user-record method or a
/// `Map`/`Set` method is not shadowed — those run *before* this recogniser via
/// their own `recv_kind`), but requires the method to be one of
/// [`FUNCTIONAL_LIST_METHODS`]. Returns the receiver, the validated method name,
/// and the remaining (non-self) arguments (the closure plus, for `fold`, the
/// initial accumulator).
///
/// Each backend wires this into its `Call` arm alongside
/// [`desugared_list_method`] and lowers it to the target's native iteration
/// idiom with the closure passed *once* and correctly (no duplicated receiver).
#[must_use]
pub fn desugared_list_functional_method<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg])> {
    if !matches!(raw_recv_kind(call_node), None | Some("List")) {
        return None;
    }
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if FUNCTIONAL_LIST_METHODS.contains(&method) {
        Some((recv, method, rest))
    } else {
        None
    }
}

// ─── Primitive-bridge method dispatch ────────────────────────────────────────
//
// The §18.2 core traits (Comparable/Equatable/Displayable) cover primitives via
// compiler-registered canonical conformances (the Q-bridge), so `(1).compare(2)`
// type-checks to `Ordering` and `a.eq(b)` to `Bool`. But codegen sees only the
// desugared `Call(FieldAccess(1, "compare"), [1, 2])` — and `i64`/`number`/`int`
// have no `.compare`/`.eq` method, so the generic desugared-self-call lowering
// emits `1.compare(1, 2)` (JS) / `1_i64.compare(2_i64)` (Rust), which fail on
// every target. This module recognises such calls — using the checker's
// `recv_kind` annotation (`bock_types::checker::RECV_KIND_META_KEY`) to confirm
// the receiver is a primitive — so each backend lowers them to the target's
// intrinsic comparison/equality/stringification.

/// The three variants of the prelude `Ordering` enum (`core.compare`), in the
/// order the comparison ladder produces them.
///
/// `Ordering` is the return type of `Comparable.compare`, so the primitive
/// bridge constructs one of these per comparison. When the `core.compare` enum
/// declaration is not among the reached modules, each backend lowers
/// `Ordering`/`Less`/`Equal`/`Greater` to a self-contained representation
/// (Rust's native `std::cmp::Ordering`, a tagged object in JS/TS, a runtime
/// singleton in Python/Go) — the same treatment the built-in `Optional`/
/// `Result` receive.
pub const ORDERING_VARIANTS: &[&str] = &["Less", "Equal", "Greater"];

/// Returns the variant name if `name` is one of the prelude `Ordering` variants
/// (`Less`/`Equal`/`Greater`), else `None`. The returned `&'static str` is the
/// canonical spelling, suitable for emitting into target source.
#[must_use]
pub fn ordering_variant(name: &str) -> Option<&'static str> {
    ORDERING_VARIANTS.iter().copied().find(|&v| v == name)
}

/// The primitive trait-bridge methods this codegen lowers to a target intrinsic.
///
/// `compare`/`eq` are the canonical `Comparable`/`Equatable` methods; `to_string`
/// and `display` are the `Displayable` stringification methods. All resolve in
/// the checker to a known return type (`Ordering`, `Bool`, `String`) and must be
/// lowered to the target's intrinsic because the primitive has no such method in
/// the target language.
pub const PRIMITIVE_BRIDGE_METHODS: &[&str] = &["compare", "eq", "to_string", "display"];

/// The receiver-kind annotation value, parsed into the primitive type name.
///
/// Returns the primitive type's name (e.g. `"Int"`, `"Float"`, `"String"`) when
/// the node carries a `recv_kind = "Primitive:<Ty>"` metadata stamp, else
/// `None`. This is the codegen-side reader of the checker→codegen annotation.
#[must_use]
pub fn primitive_recv_kind(node: &AIRNode) -> Option<&str> {
    let bock_air::Value::String(tag) =
        node.metadata.get(bock_types::checker::RECV_KIND_META_KEY)?
    else {
        return None;
    };
    tag.strip_prefix("Primitive:")
}

/// Recognise a *desugared primitive trait-bridge method call*.
///
/// Building on [`desugared_self_call`], this additionally requires that (a) the
/// `call_node` carries the checker's `recv_kind = "Primitive:<Ty>"` annotation
/// and (b) the method is one of [`PRIMITIVE_BRIDGE_METHODS`]. Returns the
/// receiver node, the method name, the remaining (non-self) arguments, and the
/// primitive type name — everything a backend needs to lower the call to its
/// intrinsic (`x.cmp(&y)` / `x == y` / `x.to_string()` in Rust, the ternary
/// `Ordering` construction in JS/TS/Python/Go, …).
///
/// `call_node` is the full `Call` AIR node (it holds the annotation); `callee`
/// and `args` are its `callee`/`args` fields, passed separately so a backend can
/// call this from inside its `NodeKind::Call { callee, args, .. }` match arm.
#[must_use]
pub fn primitive_bridge_call<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg], &'a str)> {
    let prim = primitive_recv_kind(call_node)?;
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if PRIMITIVE_BRIDGE_METHODS.contains(&method) {
        Some((recv, method, rest, prim))
    } else {
        None
    }
}

/// The receiver-kind annotation value, parsed into the bounding *trait* name.
///
/// Returns the trait name (e.g. `"Equatable"`, `"Comparable"`) when the node
/// carries a `recv_kind = "TraitBound:<Trait>"` metadata stamp, else `None`. The
/// checker stamps this on a method call whose receiver is a bounded type variable
/// (`a.eq(b)` inside `eq_check[T: Equatable]`), recording that the method
/// dispatches through that trait bound rather than a concrete type. The codegen
/// analogue of [`primitive_recv_kind`].
#[must_use]
pub fn trait_bound_recv_kind(node: &AIRNode) -> Option<&str> {
    let bock_air::Value::String(tag) =
        node.metadata.get(bock_types::checker::RECV_KIND_META_KEY)?
    else {
        return None;
    };
    tag.strip_prefix("TraitBound:")
}

/// Recognise a *desugared sealed-core-trait bridge method call* on a bounded
/// generic type variable.
///
/// The generic analogue of [`primitive_bridge_call`]: building on
/// [`desugared_self_call`], this additionally requires that (a) the `call_node`
/// carries the checker's `recv_kind = "TraitBound:<Trait>"` annotation, (b) the
/// trait is one of the compiler-provided sealed core traits
/// ([`bock_types::traits::SEALED_CORE_TRAITS`]) and is NOT declared in
/// `trait_decls` (i.e. it is the primitive conformance, not a user trait that
/// happens to share the name), and (c) the method is one of
/// [`PRIMITIVE_BRIDGE_METHODS`].
///
/// When all three hold the method dispatches through a sealed core trait whose
/// primitive instantiations (`Int`/`String`/`Bool`) have no `.eq`/`.compare`
/// method in any target, so each backend must lower it to the target intrinsic —
/// exactly as the `Primitive:<Ty>` bridge does, but driven by the generic bound
/// rather than a concrete receiver type. Returns the receiver node, the method
/// name, the remaining (non-self) arguments, and the trait name.
///
/// A `TraitBound:<Trait>` whose trait IS user-declared is left to the normal
/// trait-dispatch lowering (the user `impl` provides the method); a non-sealed
/// trait bound is likewise untouched.
#[must_use]
pub fn trait_bound_bridge_call<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
    trait_decls: &TraitDeclRegistry,
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg], &'a str)> {
    let tr = trait_bound_recv_kind(call_node)?;
    if !is_unimplemented_sealed_core_trait(tr, trait_decls) {
        return None;
    }
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if PRIMITIVE_BRIDGE_METHODS.contains(&method) {
        Some((recv, method, rest, tr))
    } else {
        None
    }
}

/// True when `trait_name` is a compiler-provided sealed core trait
/// ([`bock_types::traits::SEALED_CORE_TRAITS`]) that is NOT declared as a user
/// trait in `trait_decls`. Such a bound is the primitive conformance and must be
/// lowered to the target's built-in (native `==`/comparison/stringification and
/// the built-in ordered/equality constraint) rather than referenced as a real
/// trait/interface, which does not exist in any target. Shared by the
/// generic-bound renderers and the method-call bridge so the two stay in lockstep.
#[must_use]
pub fn is_unimplemented_sealed_core_trait(
    trait_name: &str,
    trait_decls: &TraitDeclRegistry,
) -> bool {
    bock_types::traits::SEALED_CORE_TRAITS.contains(&trait_name)
        && !trait_decls.contains_key(trait_name)
}

/// Fold a function/impl `where`-clause's trait bounds onto the matching generic
/// params, returning an owned param list with the constraints attached inline.
///
/// A generic-param bound may be written two ways: inline (`fn f[T: Show]`),
/// where it already lives on `GenericParam.bounds`, or via a `where`-clause
/// (`fn f[T]() where (T: Show)`), where it lives in a separate `where_clause`
/// keyed by the type-var name. The generic-param renderers
/// (`generic_params_to_ts`, the Go type-param constraint emitter) only read
/// `GenericParam.bounds`, so a `where`-clause bound would otherwise be dropped
/// at codegen — emitting `<T>` instead of `<T extends Show>` / `[T any]` instead
/// of `[T Show]`, which fails the target compiler when the body calls a trait
/// method on `T`. This applies to a *locally* defined `where`-bounded fn and,
/// because an imported generic fn is emitted in its own module file carrying its
/// reconstructed `where`-clause (PR #286), to cross-module dispatch too.
///
/// Each constraint's bounds are appended to the param whose name matches
/// `constraint.param`; an inline bound already present is preserved (no dedup is
/// needed — a target's bound list tolerates repeats, and source cannot legally
/// state the same bound both inline and in `where`). Params with no matching
/// constraint are returned unchanged.
#[must_use]
pub fn merge_where_bounds_into_generics(
    generic_params: &[bock_ast::GenericParam],
    where_clause: &[bock_ast::TypeConstraint],
) -> Vec<bock_ast::GenericParam> {
    if where_clause.is_empty() {
        return generic_params.to_vec();
    }
    generic_params
        .iter()
        .map(|p| {
            let mut p = p.clone();
            for constraint in where_clause {
                if constraint.param.name == p.name.name {
                    p.bounds.extend(constraint.bounds.iter().cloned());
                }
            }
            p
        })
        .collect()
}

// ─── Optional / Result built-in method dispatch ──────────────────────────────
//
// `Optional[T]` and `Result[T, E]` expose a small set of built-in methods
// (`is_some`/`unwrap_or`/`map`, `is_ok`/`unwrap`/…) that the checker resolves to
// a concrete return type. But codegen sees only the desugared
// `Call(FieldAccess(recv, m), [recv, …])` — and the *same* method names overlap
// across the two types (`unwrap`/`unwrap_or`/`map` are on both, and on `List`).
// Without disambiguation a backend either double-passes the receiver
// (`o.unwrap_or(o, 0)`, a runtime error in JS) or calls a method the tagged
// representation does not have (`o.is_some` on a TS `{_tag:"None"}` union). The
// checker's `recv_kind` annotation (`RECV_KIND_META_KEY`, value `"Optional"` /
// `"Result"`) records the resolved receiver category on the call node, so each
// backend reads it here to pick the right lowering on the tagged value.

/// The built-in `Optional[T]` methods this codegen lowers on the tagged value.
///
/// `is_some`/`is_none` test the tag; `unwrap`/`unwrap_or` extract the payload (or
/// a default); `map`/`flat_map` transform it. The set mirrors the checker's
/// `Optional` method resolution (`checker.rs`), so every method that type-checks
/// has a lowering.
pub const OPTIONAL_METHODS: &[&str] = &[
    "is_some",
    "is_none",
    "unwrap",
    "unwrap_or",
    "map",
    "flat_map",
];

/// The built-in `Result[T, E]` methods this codegen lowers on the tagged value.
///
/// `is_ok`/`is_err` test the tag; `unwrap`/`unwrap_or` extract the `Ok` payload
/// (or a default); `map`/`map_err` transform the `Ok`/`Err` payload. Mirrors the
/// checker's `Result` method resolution (`checker.rs`).
pub const RESULT_METHODS: &[&str] = &["is_ok", "is_err", "unwrap", "unwrap_or", "map", "map_err"];

/// The receiver-kind annotation value, when it is one of the built-in container
/// categories `Optional`, `Result`, `Map`, or `Set`.
///
/// Returns the tag (`"Optional"` / `"Result"` / `"Map"` / `"Set"`) when the
/// node carries a `recv_kind` stamp with that exact value, else `None`. This is
/// the codegen-side reader of the checker→codegen annotation, the
/// disambiguation crux for the overloaded method names that appear on several
/// built-in containers (`unwrap`/`unwrap_or`/`map` on `Optional`/`Result`;
/// `filter`/`map`/`len`/`contains`/`to_list` across `List`/`Map`/`Set`).
#[must_use]
pub fn container_recv_kind(node: &AIRNode) -> Option<&str> {
    let bock_air::Value::String(tag) =
        node.metadata.get(bock_types::checker::RECV_KIND_META_KEY)?
    else {
        return None;
    };
    match tag.as_str() {
        "Optional" => Some("Optional"),
        "Result" => Some("Result"),
        "Map" => Some("Map"),
        "Set" => Some("Set"),
        _ => None,
    }
}

/// Recognise a *desugared `Optional[T]` built-in method call*.
///
/// Building on [`desugared_self_call`], this additionally requires that (a) the
/// `call_node` carries the checker's `recv_kind = "Optional"` annotation and (b)
/// the method is one of [`OPTIONAL_METHODS`]. Returns the receiver node, the
/// method name, and the remaining (non-self) arguments — everything a backend
/// needs to lower the call on the tagged Optional value
/// (`(o._tag === "Some" ? o._0 : d)` in JS/TS, `o._0 if isinstance(o,_BockSome)
/// else d` in Python, an `__bockOption`-tag test in Go, the native method in
/// Rust).
///
/// `call_node` is the full `Call` AIR node (it holds the annotation); `callee`
/// and `args` are its `callee`/`args` fields, passed separately so a backend can
/// call this from inside its `NodeKind::Call { callee, args, .. }` arm.
#[must_use]
pub fn desugared_optional_method<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg])> {
    if container_recv_kind(call_node) != Some("Optional") {
        return None;
    }
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if OPTIONAL_METHODS.contains(&method) {
        Some((recv, method, rest))
    } else {
        None
    }
}

/// Recognise a *desugared `Result[T, E]` built-in method call*.
///
/// The `Result` counterpart of [`desugared_optional_method`]: requires the
/// `recv_kind = "Result"` annotation and a method in [`RESULT_METHODS`]. Returns
/// the receiver node, the method name, and the remaining (non-self) arguments.
/// The `recv_kind` disambiguation is what lets a backend distinguish
/// `r.unwrap_or(d)` on a `Result` (test `_tag === "Ok"`) from the same call on an
/// `Optional` (test `_tag === "Some"`).
#[must_use]
pub fn desugared_result_method<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg])> {
    if container_recv_kind(call_node) != Some("Result") {
        return None;
    }
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if RESULT_METHODS.contains(&method) {
        Some((recv, method, rest))
    } else {
        None
    }
}

// ─── Map / Set built-in method dispatch ──────────────────────────────────────
//
// `Map[K, V]` and `Set[E]` expose built-in methods (`get`/`set`/`keys`/…,
// `add`/`contains`/`union`/…) that the checker resolves to a concrete return
// type. Codegen sees only the desugared `Call(FieldAccess(recv, m), [recv, …])`,
// and several method names overlap with `List` (`len`/`length`/`count`,
// `filter`, `map`, `to_list`, plus `contains` on `Set`/`List`): without
// disambiguation a `Map`/`Set` receiver's `get`/`len`/`contains_key` is routed
// through the `List` path (`(m).length`, an index-bounds `Optional`), and the
// `Map`/`Set`-only methods (`set`/`add`/`keys`/`values`) fall through to the
// generic desugared-self-call, emitting `m.set(m, k, v)` — undefined on every
// target. The checker's `recv_kind` annotation (`RECV_KIND_META_KEY`, value
// `"Map"` / `"Set"`) records the resolved receiver category on the call node,
// so each backend reads it here to pick the right lowering and — crucially —
// runs the recognisers *before* `desugared_list_method` so the overlapping
// names dispatch by receiver kind, not by method name alone.

/// The built-in `Map[K, V]` methods this codegen lowers natively per target.
///
/// Mirrors the checker's `Map` method resolution (`checker.rs`): `get` returns
/// `Optional[V]`; `set`/`delete`/`merge`/`filter` return the (receiver) map;
/// `keys`/`values`/`entries`/`to_list` return a `List`; `len`/`length`/`count`
/// an `Int`; `contains_key`/`is_empty` a `Bool`; `for_each` `Void`. Membership
/// is spelled `contains_key` (the checker's name); a bare `contains` on a `Map`
/// does not resolve to a built-in (see the PR's Q-map-contains-name note).
pub const MAP_METHODS: &[&str] = &[
    "get",
    "set",
    "delete",
    "merge",
    "filter",
    "keys",
    "values",
    "entries",
    "to_list",
    "len",
    "length",
    "count",
    "contains_key",
    "is_empty",
    "for_each",
];

/// The built-in `Set[E]` methods this codegen lowers natively per target.
///
/// Mirrors the checker's `Set` method resolution (`checker.rs`): `add`/`remove`/
/// `union`/`intersection`/`difference`/`filter`/`map` return the (receiver) set;
/// `contains`/`is_subset`/`is_superset`/`is_empty` a `Bool`; `len`/`length`/
/// `count` an `Int`; `to_list` a `List`; `for_each` `Void`. `contains` here is
/// the *set-membership* test — distinct from `List.contains`, disambiguated by
/// the `recv_kind = "Set"` annotation.
pub const SET_METHODS: &[&str] = &[
    "add",
    "remove",
    "union",
    "intersection",
    "difference",
    "filter",
    "map",
    "contains",
    "is_subset",
    "is_superset",
    "len",
    "length",
    "count",
    "is_empty",
    "to_list",
    "for_each",
];

/// Recognise a *desugared `Map[K, V]` built-in method call*.
///
/// Building on [`desugared_self_call`], this additionally requires that (a) the
/// `call_node` carries the checker's `recv_kind = "Map"` annotation and (b) the
/// method is one of [`MAP_METHODS`]. Returns the receiver node, the method name,
/// and the remaining (non-self) arguments — everything a backend needs to lower
/// the call on the native map representation (`new Map`/`dict`/`HashMap`/
/// `map[K]V`). Each backend wires this into its `Call` arm *before*
/// [`desugared_list_method`] so a `Map` receiver's `get`/`len`/`contains_key`
/// no longer hits the `List` path.
///
/// `call_node` is the full `Call` AIR node (it holds the annotation); `callee`
/// and `args` are its `callee`/`args` fields, passed separately so a backend can
/// call this from inside its `NodeKind::Call { callee, args, .. }` arm.
#[must_use]
pub fn desugared_map_method<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg])> {
    if container_recv_kind(call_node) != Some("Map") {
        return None;
    }
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if MAP_METHODS.contains(&method) {
        Some((recv, method, rest))
    } else {
        None
    }
}

/// Recognise a *desugared `Set[E]` built-in method call*.
///
/// The `Set` counterpart of [`desugared_map_method`]: requires the
/// `recv_kind = "Set"` annotation and a method in [`SET_METHODS`]. Returns the
/// receiver node, the method name, and the remaining (non-self) arguments. The
/// `recv_kind` disambiguation is what lets a backend distinguish `s.contains(x)`
/// on a `Set` (native membership) from the same call on a `List` (a linear
/// scan), and `s.len()`/`s.filter(..)`/`s.map(..)` from their `List` forms.
#[must_use]
pub fn desugared_set_method<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg])> {
    if container_recv_kind(call_node) != Some("Set") {
        return None;
    }
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if SET_METHODS.contains(&method) {
        Some((recv, method, rest))
    } else {
        None
    }
}

// ─── String built-in method dispatch ─────────────────────────────────────────
//
// `String` exposes a set of built-in methods (`len`/`to_upper`/`trim`/
// `contains`/`split`/…) that the checker resolves to a concrete return type
// (`checker.rs`, the `Type::Primitive(PrimitiveType::String)` method table). But
// codegen sees only the desugared `Call(FieldAccess(recv, m), [recv, …])`, and
// several of these method names overlap with `List` (`len`/`length`/`count`,
// `is_empty`, `contains`, `index_of`): without disambiguation a String
// receiver's `contains`/`len` is routed through the `List` path (e.g. Go's
// `[]interface{}` linear scan), which fails to compile against a `string`. The
// remaining String-only methods (`to_upper`/`trim`/`replace`/…) fall through to
// the generic desugared-self-call and emit `s.to_upper(s)` — undefined on every
// target. The checker's `recv_kind` annotation (`RECV_KIND_META_KEY`, value
// `"Primitive:String"`) records the resolved receiver category on the call node,
// so each backend reads it here to pick the native string lowering and —
// crucially — runs this recogniser *before* `desugared_list_method` so the
// overlapping names dispatch by receiver kind, not by method name alone.

/// The built-in `String` methods this codegen lowers to each target's native
/// string ops.
///
/// Mirrors the checker's `String` method resolution
/// (`checker.rs`, `Type::Primitive(PrimitiveType::String)`): `len`/`byte_len`
/// return `Int` (scalar count vs byte count, per spec §18.3); `is_empty`/
/// `contains`/`starts_with`/`ends_with` a `Bool`; `to_upper`/`to_lower`/`trim`/
/// `replace` a `String`; `split` a `List[String]`. The set is intentionally the
/// *minimum-useful* subset that lowers cleanly to a native op on all five
/// targets — methods needing nontrivial index/Unicode semantics (`char_at`,
/// `slice`, `chars`, …) are deferred and fall through to the generic path.
pub const STRING_METHODS: &[&str] = &[
    "len",
    "length",
    "count",
    "byte_len",
    "is_empty",
    "to_upper",
    "to_lower",
    "trim",
    "contains",
    "starts_with",
    "ends_with",
    "replace",
    "split",
];

/// Recognise a *desugared `String` built-in method call*.
///
/// Building on [`desugared_self_call`], this additionally requires that (a) the
/// `call_node` carries the checker's `recv_kind = "Primitive:String"` annotation
/// and (b) the method is one of [`STRING_METHODS`]. Returns the receiver node,
/// the method name, and the remaining (non-self) arguments — everything a
/// backend needs to lower the call to the target's native string op
/// (`s.toUpperCase()` / `s.upper()` / `s.to_uppercase()` / `strings.ToUpper(s)`,
/// `[...s].length` / `len(s)` / `s.chars().count()` / `utf8.RuneCountInString(s)`,
/// …). Each backend wires this into its `Call` arm *before*
/// [`desugared_list_method`] so a String receiver's `len`/`contains` no longer
/// hits the `List` path (the Go `[]interface{}` scan).
///
/// `call_node` is the full `Call` AIR node (it holds the annotation); `callee`
/// and `args` are its `callee`/`args` fields, passed separately so a backend can
/// call this from inside its `NodeKind::Call { callee, args, .. }` arm.
#[must_use]
pub fn desugared_string_method<'a>(
    call_node: &'a AIRNode,
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg])> {
    if primitive_recv_kind(call_node) != Some("String") {
        return None;
    }
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if STRING_METHODS.contains(&method) {
        Some((recv, method, rest))
    } else {
        None
    }
}

// ─── Reserved-keyword escaping ───────────────────────────────────────────────
//
// A Bock value identifier (a parameter, local `let`, or free-function name) is a
// plain word the user chose; nothing stops it colliding with a *target*
// language's reserved word. Before this layer codegen emitted such an identifier
// verbatim, producing source the target rejects at compile/parse time —
// `function getOr(o, default)` (JS/TS/Go reserve `default`), `def: int = …`
// (Python reserves `def`), and so on. Because each backend funnels its
// value-binding names through a single case-conversion (`to_camel_case` for
// JS/TS/Go, `to_snake_case` for Python/Rust), one post-conversion escape step
// per target closes the whole class: a converted name that equals a target
// keyword is suffixed with `_` (`default` → `default_`, `def` → `def_`),
// applied *consistently* at the declaration site, every reference site, and —
// for Go — the type-inference scope-map keys, so they always agree.
//
// Scope: only *value* identifiers are escaped. Member/field/method names (a
// `obj.default` access, a host-method call) are NOT — `default` is a perfectly
// legal member name on every target, and escaping it would break the access.
// Type names are not escaped either (no v1 keyword collides with a Bock type
// name, and they live in a different namespace). The suffix-`_` mangle is
// stable and idempotent (`escape` of an already-escaped name is itself), and
// the chosen suffix never itself reintroduces a keyword.

/// The codegen target whose reserved-word set an identifier is being escaped
/// against. Mirrors the five v1 backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeywordTarget {
    /// JavaScript (`js`).
    Js,
    /// TypeScript (`ts`) — a superset of the JS reserved set.
    Ts,
    /// Python (`python`).
    Python,
    /// Rust (`rust`).
    Rust,
    /// Go (`go`).
    Go,
}

/// JavaScript reserved words and future-reserved words (ES2015+), plus the
/// literal keywords. A value binding named any of these must be escaped.
const JS_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "enum",
    "await",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
];

/// TypeScript reserves everything JS does plus a handful of type-level words
/// that are also illegal as plain bindings in value positions the backend emits.
const TS_EXTRA_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "any",
    "boolean",
    "constructor",
    "declare",
    "get",
    "infer",
    "is",
    "keyof",
    "module",
    "namespace",
    "never",
    "readonly",
    "require",
    "number",
    "object",
    "set",
    "string",
    "symbol",
    "type",
    "undefined",
    "unique",
    "unknown",
    "from",
    "of",
    "async",
];

/// Python 3 keywords (`keyword.kwlist`) plus the soft keywords that are unsafe
/// as bindings in the positions the backend emits.
const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield", "match", "case",
];

/// Rust strict and reserved keywords (2018/2021 editions). Rust *could* use the
/// raw-identifier form (`r#match`) for most of these, but a uniform `_` suffix
/// keeps the escape identical across all targets and avoids the handful of words
/// (`crate`/`self`/`super`/`Self`) that cannot be raw identifiers at all.
const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
    "use", "where", "while", "async", "await", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "union",
];

/// Go keywords (the Go spec's 25 reserved words).
const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];

/// True when `name` is a reserved word in the given target's keyword set.
#[must_use]
pub fn is_target_keyword(name: &str, target: KeywordTarget) -> bool {
    match target {
        KeywordTarget::Js => JS_KEYWORDS.contains(&name),
        KeywordTarget::Ts => JS_KEYWORDS.contains(&name) || TS_EXTRA_KEYWORDS.contains(&name),
        KeywordTarget::Python => PYTHON_KEYWORDS.contains(&name),
        KeywordTarget::Rust => RUST_KEYWORDS.contains(&name),
        KeywordTarget::Go => GO_KEYWORDS.contains(&name),
    }
}

/// Escape `name` (an already case-converted *value* identifier) against the
/// target's reserved-word set: a name that collides with a keyword gets a
/// trailing `_`, otherwise it is returned unchanged.
///
/// Idempotent — the suffixed form is never itself a keyword, so re-escaping is a
/// no-op. Apply this at every site that emits or keys on a Bock value binding
/// (declaration, reference, and the Go scope-inference maps) so the escaped name
/// is used uniformly. Do **not** apply it to member/field/method names or to
/// type names (see the section comment).
#[must_use]
pub fn escape_target_keyword(name: &str, target: KeywordTarget) -> String {
    if is_target_keyword(name, target) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

// ─── Enum-variant registry ──────────────────────────────────────────────────
//
// User-defined enum *declarations* already lower correctly per target (JS
// tagged factories, Rust real `enum`, Go sealed interface + variant structs).
// What every backend lacked was a way to recognise, at a *use* site, that a
// bare `Red` / `Circle { .. }` / `Rect(..)` is an enum variant rather than a
// variable, a record, or a free function — and which enum it belongs to. The
// AIR carries no back-pointer from a variant name to its enum at a construction
// or pattern site (`ConstructorPat`/`RecordPat`/`RecordConstruct` paths hold
// only the variant name, never the enum). This registry closes that gap: a
// single pre-scan over every reached module maps each variant name to its enum
// and payload shape, which each backend consults to qualify constructions
// (`Color_Red`, `Shape::Circle`, `ShapeCircle{..}`) and to dispatch matches.
//
// The built-in `Optional`/`Result` constructors (`Some`/`None`/`Ok`/`Err`) are
// pre-seeded so one mechanism describes both user and built-in ADTs (B1). The
// pre-seeded entries are a *fallback*: each backend keeps its existing bespoke
// Optional/Result lowering and consults the registry only afterwards, so the
// proven Optional/Result paths are never regressed.

/// The payload shape of an enum variant, as needed to lower a construction or
/// a match arm in any target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariantPayloadKind {
    /// A unit variant (`Red`): no payload.
    Unit,
    /// A tuple variant (`Rect(Float, Float)`): positional fields, by arity.
    Tuple(usize),
    /// A struct variant (`Circle { radius: Float }`): named fields, in
    /// declaration order.
    Struct(Vec<String>),
}

/// What the registry knows about one enum variant: the enum it belongs to and
/// its payload shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariantInfo {
    /// The declared name of the owning enum (e.g. `Shape`).
    pub enum_name: String,
    /// The variant's payload shape.
    pub payload: VariantPayloadKind,
}

/// Maps an enum-variant name to its [`EnumVariantInfo`]. Variant names are
/// globally unique within a v1 Bock program (no per-enum namespacing at use
/// sites — `Red`, not `Color.Red`), so a flat map keyed by the bare variant
/// name resolves every construction and pattern site.
pub type EnumVariantRegistry = HashMap<String, EnumVariantInfo>;

/// Pre-scan every reached module and build the [`EnumVariantRegistry`].
///
/// Walks each module's top-level `EnumDecl`s (the only place enum variants are
/// declared) and records every variant. A *pre-scan* — rather than recording
/// variants as their decls are emitted — is required because a use site may
/// precede its enum's declaration in source order (forward reference), and
/// because a `use`d enum's decl can live in a different module than its
/// construction site (cross-module `use`). This mirrors the Go backend's
/// existing `collect_methods` / `collect_optional_returns` pre-scans.
///
/// The built-in `Optional`/`Result` constructors are pre-seeded (B1) so the
/// same registry describes built-in ADTs; backends treat these as a fallback
/// behind their bespoke Optional/Result lowering (see the module comment).
#[must_use]
pub fn collect_enum_variants(modules: &[(&AIRModule, &Path)]) -> EnumVariantRegistry {
    let mut registry = EnumVariantRegistry::new();
    seed_builtin_variants(&mut registry);
    for (module, _) in modules {
        let NodeKind::Module { items, .. } = &module.kind else {
            continue;
        };
        for item in items {
            collect_enum_variants_from_item(item, &mut registry);
        }
    }
    registry
}

/// Pre-seed the built-in `Optional` (`Some`/`None`) and `Result` (`Ok`/`Err`)
/// constructors. `Some`/`Ok`/`Err` carry a single positional payload; `None` is
/// a unit variant.
fn seed_builtin_variants(registry: &mut EnumVariantRegistry) {
    registry.insert(
        "Some".to_string(),
        EnumVariantInfo {
            enum_name: "Optional".to_string(),
            payload: VariantPayloadKind::Tuple(1),
        },
    );
    registry.insert(
        "None".to_string(),
        EnumVariantInfo {
            enum_name: "Optional".to_string(),
            payload: VariantPayloadKind::Unit,
        },
    );
    registry.insert(
        "Ok".to_string(),
        EnumVariantInfo {
            enum_name: "Result".to_string(),
            payload: VariantPayloadKind::Tuple(1),
        },
    );
    registry.insert(
        "Err".to_string(),
        EnumVariantInfo {
            enum_name: "Result".to_string(),
            payload: VariantPayloadKind::Tuple(1),
        },
    );
}

/// Record every variant of a single `EnumDecl` item into `registry`. Non-enum
/// items are ignored.
fn collect_enum_variants_from_item(item: &AIRNode, registry: &mut EnumVariantRegistry) {
    let NodeKind::EnumDecl { name, variants, .. } = &item.kind else {
        return;
    };
    let enum_name = name.name.clone();
    for variant in variants {
        let NodeKind::EnumVariant {
            name: vname,
            payload,
        } = &variant.kind
        else {
            continue;
        };
        let payload = match payload {
            EnumVariantPayload::Unit => VariantPayloadKind::Unit,
            EnumVariantPayload::Tuple(elems) => VariantPayloadKind::Tuple(elems.len()),
            EnumVariantPayload::Struct(fields) => {
                VariantPayloadKind::Struct(fields.iter().map(|f| f.name.name.clone()).collect())
            }
        };
        registry.insert(
            vname.name.clone(),
            EnumVariantInfo {
                enum_name: enum_name.clone(),
                payload,
            },
        );
    }
}

/// Look up the last segment of a `TypePath` (the variant name) in the registry.
/// Returns `None` when the path is empty or the name is not a known variant.
#[must_use]
pub fn registered_variant<'a>(
    registry: &'a EnumVariantRegistry,
    path: &bock_ast::TypePath,
) -> Option<&'a EnumVariantInfo> {
    let last = path.segments.last()?;
    registry.get(&last.name)
}

/// Maps a generic type's declared name to its generic parameters. Built by a
/// pre-scan of every `RecordDecl`/`EnumDecl`/`ClassDecl` across the reached
/// modules.
///
/// Backends with native generic-receiver / `impl` syntax (Rust `impl<T> T<T>`,
/// Go `func (self *T[T])`, TS declaration-merged `interface T<T>`) need a
/// generic type's parameters at its method-emission site even though the AIR
/// `impl Box { ... }` block carries no generic params of its own — the `T` is
/// declared on the *record*, not the impl. This registry recovers those params
/// at the impl site. A *pre-scan* (rather than recording params as decls are
/// emitted) is required because an `impl` may precede its type's declaration in
/// source order, and because a `use`d type's decl can live in a different
/// module than its `impl` (cross-module `use`). Mirrors
/// [`collect_enum_variants`].
pub type GenericDeclRegistry = HashMap<String, Vec<bock_ast::GenericParam>>;

/// Pre-scan every reached module and build the [`GenericDeclRegistry`].
/// Records the generic parameters declared on each top-level `RecordDecl`,
/// `EnumDecl`, and `ClassDecl`. Non-generic decls are recorded with an empty
/// parameter list (their presence still lets a backend distinguish a known
/// concrete type from an unknown one).
#[must_use]
pub fn collect_generic_decls(modules: &[(&AIRModule, &Path)]) -> GenericDeclRegistry {
    let mut registry = GenericDeclRegistry::new();
    for (module, _) in modules {
        let NodeKind::Module { items, .. } = &module.kind else {
            continue;
        };
        for item in items {
            collect_generic_decls_from_item(item, &mut registry);
        }
    }
    registry
}

/// Record one decl's name → generic params into `registry`. Ignores items that
/// are not record/enum/class declarations.
fn collect_generic_decls_from_item(item: &AIRNode, registry: &mut GenericDeclRegistry) {
    let (name, generic_params) = match &item.kind {
        NodeKind::RecordDecl {
            name,
            generic_params,
            ..
        }
        | NodeKind::EnumDecl {
            name,
            generic_params,
            ..
        }
        | NodeKind::ClassDecl {
            name,
            generic_params,
            ..
        } => (name, generic_params),
        _ => return,
    };
    registry.insert(name.name.clone(), generic_params.clone());
}

/// Pre-scan every module's top-level type declarations and collect the names of
/// those declared `public` (records, enums, traits, classes). A backend that
/// emits a declaration-merging companion (TS's `interface Target` that mirrors
/// an `impl`'s prototype methods) needs this: TS requires every declaration in
/// a merged declaration to agree on export-ness, so the companion `interface`
/// must be `export`ed exactly when the target type is. Mirrors
/// [`collect_generic_decls`].
#[must_use]
pub fn collect_exported_type_names(
    modules: &[(&AIRModule, &Path)],
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    for (module, _) in modules {
        let NodeKind::Module { items, .. } = &module.kind else {
            continue;
        };
        for item in items {
            let (visibility, name) = match &item.kind {
                NodeKind::RecordDecl {
                    visibility, name, ..
                }
                | NodeKind::EnumDecl {
                    visibility, name, ..
                }
                | NodeKind::TraitDecl {
                    visibility, name, ..
                }
                | NodeKind::ClassDecl {
                    visibility, name, ..
                } => (visibility, name),
                _ => continue,
            };
            if matches!(visibility, bock_ast::Visibility::Public) {
                names.insert(name.name.clone());
            }
        }
    }
    names
}

// ─── Trait-declaration registry (default methods) ────────────────────────────

/// What the registry knows about one trait declaration: its declared generic
/// parameters and its methods (each an `AIRNode::FnDecl`), partitioned by the
/// caller via [`is_default_method`].
#[derive(Debug, Clone)]
pub struct TraitDeclInfo {
    /// Generic parameters declared on the trait (`trait Comparable[T]`).
    pub generic_params: Vec<bock_ast::GenericParam>,
    /// Every method declared in the trait body, in source order. A method whose
    /// AIR body block is non-empty is a *default* method (see
    /// [`is_default_method`]); an empty body marks a *required* method.
    pub methods: Vec<AIRNode>,
}

/// Maps a trait's declared name to its [`TraitDeclInfo`]. Trait names are
/// globally unique within a Bock program, so a flat map keyed by the bare name
/// resolves every `impl Trait for Type` block to its trait.
pub type TraitDeclRegistry = HashMap<String, TraitDeclInfo>;

/// True when `fn_decl` is a trait **default** method — one that carries a body.
///
/// A required (signature-only) trait method has no body in source
/// (`fn compare(self, other: Self) -> Ordering`); the AIR lowerer represents
/// that absence as an *empty* `Block` (`stmts` empty, no tail expression). A
/// default method (`fn not_eq(self, other: Self) -> Bool { ... }`) lowers to a
/// non-empty block. We therefore detect "has a default body" structurally as
/// "the body block is non-empty".
///
/// HEURISTIC NOTE: this is the empty-block heuristic. It is exact for code
/// produced by the current AIR lowerer (`bock-air::lower::lower_fn` synthesizes
/// `Block { stmts: vec![], tail: None }` for a bodyless method and only for a
/// bodyless method). A user *default* method whose body is literally `{}` (an
/// empty block) would be misclassified as required — but an empty-bodied method
/// returning a non-`Void` type does not type-check, and a `Void` default with an
/// empty body is behaviorally identical to no default, so the misclassification
/// is harmless. A robust, unambiguous fix would be an explicit `has_body` flag
/// on the AIR `FnDecl` (carried from the AST's `Option<Block>`); that is a
/// possible follow-up (a `bock-air` change, out of scope here).
#[must_use]
pub fn is_default_method(fn_decl: &AIRNode) -> bool {
    let NodeKind::FnDecl { body, .. } = &fn_decl.kind else {
        return false;
    };
    match &body.kind {
        NodeKind::Block { stmts, tail } => !stmts.is_empty() || tail.is_some(),
        // A non-Block body is unusual but, if present, counts as a real body.
        _ => true,
    }
}

/// The method name of a `FnDecl` node, or `None` for a non-`FnDecl`.
#[must_use]
pub fn fn_decl_name(fn_decl: &AIRNode) -> Option<&str> {
    if let NodeKind::FnDecl { name, .. } = &fn_decl.kind {
        Some(name.name.as_str())
    } else {
        None
    }
}

/// True if a type-expression node mentions `Self` anywhere (directly or nested
/// inside an optional / tuple / function / generic-arg position).
#[must_use]
fn type_node_mentions_self(node: &AIRNode) -> bool {
    match &node.kind {
        NodeKind::TypeSelf => true,
        NodeKind::TypeOptional { inner } => type_node_mentions_self(inner),
        NodeKind::TypeTuple { elems } => elems.iter().any(type_node_mentions_self),
        NodeKind::TypeFunction { params, ret, .. } => {
            params.iter().any(type_node_mentions_self) || type_node_mentions_self(ret)
        }
        NodeKind::TypeNamed { args, .. } => args.iter().any(type_node_mentions_self),
        _ => false,
    }
}

/// True if any of `trait_info`'s methods reference `Self` in a (non-receiver)
/// parameter type or in the return type.
///
/// A trait with a `Self`-typed operand — `fn compare(self, other: Self) ->
/// Ordering` — cannot be encoded as a plain Go interface used as a generic
/// bound: the interface method would have to be `Compare(Self)`, but a Go
/// interface cannot name the implementing type. The Go backend instead encodes
/// such a trait as an *F-bounded* generic interface (`type Comparable[T any]
/// interface { Compare(T) Ordering }`), satisfied by `func (Key) Compare(Key)`,
/// and lowers a bound `[T: Comparable]` to `[T Comparable[T]]`. This predicate
/// selects the traits that need that treatment; a trait with no `Self` operand
/// (only `self`) stays a plain interface.
#[must_use]
pub fn trait_uses_self_operand(trait_info: &TraitDeclInfo) -> bool {
    trait_info.methods.iter().any(|m| {
        let NodeKind::FnDecl {
            params,
            return_type,
            ..
        } = &m.kind
        else {
            return false;
        };
        // Skip the leading `self` receiver; inspect the remaining param types
        // and the return type for a `Self` mention.
        let param_self = params
            .iter()
            .skip(1)
            .filter_map(|p| {
                if let NodeKind::Param { ty: Some(t), .. } = &p.kind {
                    Some(t.as_ref())
                } else {
                    None
                }
            })
            .any(type_node_mentions_self);
        let ret_self = return_type.as_deref().is_some_and(type_node_mentions_self);
        param_self || ret_self
    })
}

/// Pre-scan every reached module and build the [`TraitDeclRegistry`].
///
/// Walks each module's top-level `TraitDecl`s and records the trait's generic
/// params and the full method list. Backends use this at each `impl Trait for
/// Type` site to recover the trait's *default* methods (those carrying a body)
/// so they can be synthesized onto the implementing type — the trait interface
/// alone carries only signatures, so a type that relies on an inherited default
/// would otherwise have no such method at runtime (js/ts/go). A *pre-scan*
/// (rather than recording traits as their decls are emitted) is required because
/// an `impl` may precede its trait's declaration in source order, and because a
/// `use`d trait's decl can live in a different module than its `impl`
/// (cross-module `use`). Mirrors [`collect_generic_decls`].
#[must_use]
pub fn collect_trait_decls(modules: &[(&AIRModule, &Path)]) -> TraitDeclRegistry {
    let mut registry = TraitDeclRegistry::new();
    for (module, _) in modules {
        let NodeKind::Module { items, .. } = &module.kind else {
            continue;
        };
        for item in items {
            if let NodeKind::TraitDecl {
                name,
                generic_params,
                methods,
                ..
            } = &item.kind
            {
                registry.insert(
                    name.name.clone(),
                    TraitDeclInfo {
                        generic_params: generic_params.clone(),
                        methods: methods.clone(),
                    },
                );
            }
        }
    }
    registry
}

/// Resolve, for an `impl Trait for Type` block, the trait default methods that
/// the impl does **not** override and that must therefore be synthesized onto
/// the target. `trait_path` is the `ImplBlock`'s `trait_path`; `impl_methods`
/// its own methods. Returns *cloned* default-method `FnDecl` nodes, in
/// trait-declaration order. Empty when the impl has no trait, the trait is
/// unknown, or every default is overridden. Returns owned clones (rather than
/// registry borrows) so a backend can iterate them while mutating its own
/// emission buffer, without holding a borrow of the registry across the
/// `&mut self` writes.
#[must_use]
pub fn inherited_default_methods(
    registry: &TraitDeclRegistry,
    trait_path: &bock_ast::TypePath,
    impl_methods: &[AIRNode],
) -> Vec<AIRNode> {
    let Some(trait_name) = trait_path.segments.last().map(|s| s.name.as_str()) else {
        return Vec::new();
    };
    let Some(info) = registry.get(trait_name) else {
        return Vec::new();
    };
    let overridden: std::collections::HashSet<&str> =
        impl_methods.iter().filter_map(fn_decl_name).collect();
    info.methods
        .iter()
        .filter(|m| is_default_method(m))
        .filter(|m| fn_decl_name(m).is_some_and(|n| !overridden.contains(n)))
        .cloned()
        .collect()
}

/// True when an `impl` block (an [`bock_air::NodeKind::ImplBlock`]) declares at
/// least one **instance** method — one that binds `self`, **or** an effect
/// operation (which is dispatched on a handler instance despite taking no
/// `self`; see [`is_associated_impl_method`]).
///
/// An impl whose methods are *all* associated functions (e.g. `impl From[A] for
/// B` with only `from(value)`) contributes no instance contract: implementing it
/// adds only static members. Backends that model trait conformance through
/// instance inheritance / structural interfaces (Python base class, TS
/// `interface … extends Trait`) must wire the trait in only when this returns
/// `true`; otherwise the base/`extends` reference points at a trait with no
/// instance members — often a prelude trait not even emitted into the consuming
/// module, so the reference would be undefined.
#[must_use]
pub fn impl_has_instance_method(
    impl_block: &AIRNode,
    effect_ops: &HashMap<String, String>,
) -> bool {
    let NodeKind::ImplBlock { methods, .. } = &impl_block.kind else {
        return false;
    };
    methods
        .iter()
        .any(|m| !is_associated_impl_method(m, effect_ops))
}

// ─── Field/method name-collision disambiguation ───────────────────────────────
//
// Several stdlib (and user) types declare a *field* and a *method* that share a
// Bock name — the canonical case is `core.error`'s `SimpleError`, which has a
// `message: String` field *and* a `message()` method (the `Error` trait method).
// In Bock these are distinct (`self.message` vs `self.message()`), but most
// target object models collapse a field and a same-named method onto one member
// slot, which breaks at codegen:
//
//   - Go: `go build` rejects a struct with a field and method of the same name.
//   - TS: `class { message: string }` + `interface { message(): string }` is a
//     "Duplicate identifier".
//   - JS: the instance field `this.message` shadows the prototype method, so
//     `obj.message()` is "not a function".
//   - Python: the dataclass field overwrites the method attribute on the class.
//   - Rust: a field and an inherent method *may* share a name, so it is a no-op.
//
// The shared remedy: when a type has a method whose *emitted* name equals one of
// its *emitted* field names, the **method** is renamed (the field keeps its
// name) by appending a disambiguating suffix, and the rename is applied
// identically at the trait-interface declaration, the receiver/impl method, and
// every call site so they always agree. The two helpers below let every backend
// (go/ts/js/py) share this policy — collecting field names in the backend's own
// casing and routing both declarations and call sites through one rename — so
// any future field/method pair is handled uniformly without per-collision code.

/// Collect every record/class field name in the module, mapped through the
/// backend's `cased` name function (`to_pascal_case` for Go, `to_camel_case`
/// for js/ts, identity/snake for Python). Backends use the returned set with
/// [`disambiguate_method_name`] to detect a method whose emitted name collides
/// with a field's emitted name.
///
/// The set is intentionally a *union* across all records/classes in the module
/// (not per-type): it mirrors the Go backend's original behavior and is a safe
/// over-approximation — at worst it renames a method on a type that happens to
/// share a name with an *unrelated* type's field, which is harmless because the
/// rename is applied consistently at the method's declaration and all its call
/// sites. Keeping it module-global keeps the lookup a single `HashSet` shared by
/// declaration and call-site emission, which run at different points.
#[must_use]
pub fn collect_record_field_names<F>(
    module: &AIRNode,
    cased: F,
) -> std::collections::HashSet<String>
where
    F: Fn(&str) -> String,
{
    let mut names = std::collections::HashSet::new();
    if let NodeKind::Module { items, .. } = &module.kind {
        for item in items {
            if let NodeKind::RecordDecl { fields, .. } | NodeKind::ClassDecl { fields, .. } =
                &item.kind
            {
                for f in fields {
                    names.insert(cased(&f.name.name));
                }
            }
        }
    }
    names
}

/// Disambiguate a method's emitted name against the type's field names.
///
/// `cased_name` is the method name already mapped through the backend's casing
/// rule (so the comparison is apples-to-apples with the `field_names` produced
/// by [`collect_record_field_names`] using the *same* casing). When the cased
/// method name is also a field name, the method is renamed by appending
/// `suffix` directly to the cased name — the suffix is the backend's
/// already-cased disambiguator (`"Method"` for Go's Pascal and js/ts's camel,
/// `"_method"` for Python's snake), so `message`/`Message` become
/// `messageMethod`/`MessageMethod`/`message_method`. The cased prefix is left
/// untouched (no re-casing), so camelCase names with internal capitals survive
/// intact. Non-colliding names pass through unchanged.
///
/// Backends call this identically at the method declaration and at every call
/// site, so the renamed method always resolves.
#[must_use]
pub fn disambiguate_method_name(
    cased_name: String,
    field_names: &std::collections::HashSet<String>,
    suffix: &str,
) -> String {
    if field_names.contains(&cased_name) {
        format!("{cased_name}{suffix}")
    } else {
        cased_name
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disambiguate_method_name_suffixes_only_on_collision() {
        let mut fields = std::collections::HashSet::new();
        fields.insert("message".to_string());
        fields.insert("Message".to_string());
        // Camel/snake non-colliding name passes through unchanged.
        assert_eq!(
            disambiguate_method_name("render".to_string(), &fields, "Method"),
            "render"
        );
        // Colliding camel name gets the camel suffix.
        assert_eq!(
            disambiguate_method_name("message".to_string(), &fields, "Method"),
            "messageMethod"
        );
        // Colliding Pascal name (Go) gets the Pascal suffix.
        assert_eq!(
            disambiguate_method_name("Message".to_string(), &fields, "Method"),
            "MessageMethod"
        );
        // Colliding snake name (Python) gets the snake suffix.
        assert_eq!(
            disambiguate_method_name("message".to_string(), &fields, "_method"),
            "message_method"
        );
    }

    #[test]
    fn merge_where_bounds_folds_constraints_onto_matching_param() {
        use bock_ast::{GenericParam, Ident, TypeConstraint, TypePath};
        use bock_errors::{FileId, Span};

        fn span() -> Span {
            Span {
                file: FileId(0),
                start: 0,
                end: 0,
            }
        }
        fn ident(name: &str) -> Ident {
            Ident {
                span: span(),
                name: name.to_string(),
            }
        }
        fn type_path(name: &str) -> TypePath {
            TypePath {
                segments: vec![ident(name)],
                span: span(),
            }
        }
        fn param(name: &str, bounds: Vec<TypePath>) -> GenericParam {
            GenericParam {
                id: 0,
                span: span(),
                name: ident(name),
                bounds,
            }
        }
        fn constraint(param: &str, bounds: Vec<TypePath>) -> TypeConstraint {
            TypeConstraint {
                id: 0,
                span: span(),
                param: ident(param),
                bounds,
            }
        }

        // No where-clause: params pass through unchanged.
        let params = vec![param("T", vec![])];
        let merged = merge_where_bounds_into_generics(&params, &[]);
        assert_eq!(merged, params);

        // `where (T: Ranked)` folds onto T; the unconstrained U is untouched.
        let params = vec![param("T", vec![]), param("U", vec![])];
        let wc = vec![constraint("T", vec![type_path("Ranked")])];
        let merged = merge_where_bounds_into_generics(&params, &wc);
        assert_eq!(
            merged[0]
                .bounds
                .iter()
                .map(|b| &b.segments[0].name)
                .collect::<Vec<_>>(),
            vec!["Ranked"]
        );
        assert!(merged[1].bounds.is_empty());

        // An inline bound is preserved and the where-clause bound is appended.
        let params = vec![param("T", vec![type_path("Show")])];
        let wc = vec![constraint("T", vec![type_path("Ranked")])];
        let merged = merge_where_bounds_into_generics(&params, &wc);
        assert_eq!(
            merged[0]
                .bounds
                .iter()
                .map(|b| &b.segments[0].name)
                .collect::<Vec<_>>(),
            vec!["Show", "Ranked"]
        );
    }

    #[test]
    fn collect_record_field_names_unions_records_and_classes() {
        use bock_ast::{Ident, RecordDeclField, TypeExpr, TypePath, Visibility};
        use bock_errors::{FileId, Span};

        fn span() -> Span {
            Span {
                file: FileId(0),
                start: 0,
                end: 0,
            }
        }
        fn ident(name: &str) -> Ident {
            Ident {
                name: name.to_string(),
                span: span(),
            }
        }
        fn ty() -> TypeExpr {
            TypeExpr::Named {
                id: 0,
                span: span(),
                path: TypePath {
                    segments: vec![ident("String")],
                    span: span(),
                },
                args: vec![],
            }
        }
        fn field(name: &str) -> RecordDeclField {
            RecordDeclField {
                id: 0,
                span: span(),
                name: ident(name),
                ty: ty(),
                default: None,
            }
        }

        let record = AIRNode::new(
            1,
            span(),
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("SimpleError"),
                generic_params: vec![],
                fields: vec![field("message")],
            },
        );
        let class = AIRNode::new(
            2,
            span(),
            NodeKind::ClassDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Handler"),
                generic_params: vec![],
                base: None,
                traits: vec![],
                fields: vec![field("state")],
                methods: vec![],
            },
        );
        let module = AIRNode::new(
            0,
            span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![record, class],
            },
        );

        // Identity casing → raw field names unioned from a record and a class.
        let names = collect_record_field_names(&module, |n| n.to_string());
        assert!(names.contains("message"));
        assert!(names.contains("state"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn output_file_stores_path_and_content() {
        let f = OutputFile {
            path: PathBuf::from("main.js"),
            content: "console.log('hello');".into(),
            source_map: None,
        };
        assert_eq!(f.path, PathBuf::from("main.js"));
        assert!(f.content.contains("console.log"));
    }

    #[test]
    fn generated_code_with_no_source_map() {
        let code = GeneratedCode {
            files: vec![OutputFile {
                path: PathBuf::from("out.py"),
                content: "print('hello')".into(),
                source_map: None,
            }],
        };
        assert_eq!(code.files.len(), 1);
        assert!(code.files[0].source_map.is_none());
    }

    #[test]
    fn derive_output_path_strips_src_prefix() {
        let js = TargetProfile::javascript();
        assert_eq!(
            derive_output_path(Path::new("src/main.bock"), &js),
            PathBuf::from("main.js")
        );
        assert_eq!(
            derive_output_path(Path::new("src/utils/parse.bock"), &js),
            PathBuf::from("utils/parse.js")
        );
        assert_eq!(
            derive_output_path(Path::new("src/api/v1/handler.bock"), &js),
            PathBuf::from("api/v1/handler.js")
        );
    }

    #[test]
    fn derive_output_path_preserves_paths_without_src_prefix() {
        let py = TargetProfile::python();
        assert_eq!(
            derive_output_path(Path::new("main.bock"), &py),
            PathBuf::from("main.py")
        );
        assert_eq!(
            derive_output_path(Path::new("lib/foo.bock"), &py),
            PathBuf::from("lib/foo.py")
        );
    }

    #[test]
    fn derive_output_path_normalizes_leading_curdir() {
        let js = TargetProfile::javascript();
        assert_eq!(
            derive_output_path(Path::new("./src/main.bock"), &js),
            PathBuf::from("main.js")
        );
        assert_eq!(
            derive_output_path(Path::new("./main.bock"), &js),
            PathBuf::from("main.js")
        );
        assert_eq!(
            derive_output_path(Path::new("./src/utils/parse.bock"), &js),
            PathBuf::from("utils/parse.js")
        );
    }

    #[test]
    fn derive_output_path_uses_target_extension() {
        let path = Path::new("src/main.bock");
        assert_eq!(
            derive_output_path(path, &TargetProfile::javascript()),
            PathBuf::from("main.js")
        );
        assert_eq!(
            derive_output_path(path, &TargetProfile::typescript()),
            PathBuf::from("main.ts")
        );
        assert_eq!(
            derive_output_path(path, &TargetProfile::python()),
            PathBuf::from("main.py")
        );
        assert_eq!(
            derive_output_path(path, &TargetProfile::rust()),
            PathBuf::from("main.rs")
        );
        assert_eq!(
            derive_output_path(path, &TargetProfile::go()),
            PathBuf::from("main.go")
        );
    }

    #[test]
    fn esm_relative_specifier_from_entry_root() {
        // Entry (`main.<ext>` at the build root) → a `core.option` sibling.
        assert_eq!(
            esm_relative_specifier("", "core.option", "js"),
            "./core/option.js"
        );
        // Entry → a root-level sibling module.
        assert_eq!(esm_relative_specifier("", "helper", "js"), "./helper.js");
    }

    #[test]
    fn esm_relative_specifier_between_nested_modules() {
        // `helper` (root) → `core.option` (nested): one level down.
        assert_eq!(
            esm_relative_specifier("helper", "core.option", "ts"),
            "./core/option.ts"
        );
        // `core.option` → `core.compare`: same dir, no `../`.
        assert_eq!(
            esm_relative_specifier("core.option", "core.compare", "js"),
            "./compare.js"
        );
        // `a.b.deep` → `helper` (root): climb out of `a/b/`.
        assert_eq!(
            esm_relative_specifier("a.b.deep", "helper", "js"),
            "../../helper.js"
        );
        // `a.b.deep` → `a.c.thing`: climb to the common `a/` then descend.
        assert_eq!(
            esm_relative_specifier("a.b.deep", "a.c.thing", "js"),
            "../c/thing.js"
        );
    }

    #[test]
    fn source_map_default_is_empty() {
        let sm = SourceMap::default();
        assert!(sm.entries.is_empty());
        assert!(sm.mappings.is_empty());
        assert!(sm.sources.is_empty());
    }

    #[test]
    fn byte_to_line_col_basic() {
        let s = "abc\ndef\nghi";
        assert_eq!(byte_to_line_col(s, 0), (1, 1));
        assert_eq!(byte_to_line_col(s, 3), (1, 4));
        assert_eq!(byte_to_line_col(s, 4), (2, 1));
        assert_eq!(byte_to_line_col(s, 8), (3, 1));
    }

    #[test]
    fn resolve_positions_fills_line_col() {
        let mut sm = SourceMap {
            mappings: vec![SourceMapping {
                gen_line: 1,
                gen_col: 1,
                src_line: 0,
                src_col: 0,
                src_offset: 4,
                src_file_id: 0,
            }],
            ..Default::default()
        };
        sm.resolve_positions(&["abc\ndef"]);
        assert_eq!(sm.mappings[0].src_line, 2);
        assert_eq!(sm.mappings[0].src_col, 1);
    }

    #[test]
    fn vlq_encodes_known_values() {
        // Source Map v3 VLQ reference values.
        let mut s = String::new();
        vlq_encode(&mut s, 0);
        assert_eq!(s, "A");
        s.clear();
        vlq_encode(&mut s, 1);
        assert_eq!(s, "C");
        s.clear();
        vlq_encode(&mut s, -1);
        assert_eq!(s, "D");
        s.clear();
        vlq_encode(&mut s, 16);
        assert_eq!(s, "gB");
    }

    #[test]
    fn source_map_v3_json_contains_required_fields() {
        let mut sm = SourceMap {
            generated_file: "output.js".into(),
            ..Default::default()
        };
        sm.sources.push(SourceInfo {
            path: "main.bock".into(),
            content: Some("let x = 1\n".into()),
        });
        sm.mappings.push(SourceMapping {
            gen_line: 1,
            gen_col: 1,
            src_line: 1,
            src_col: 1,
            src_offset: 0,
            src_file_id: 0,
        });
        let json = sm.to_source_map_v3_json();
        assert!(json.contains("\"version\":3"));
        assert!(json.contains("\"file\":\"output.js\""));
        assert!(json.contains("\"sources\":[\"main.bock\"]"));
        assert!(json.contains("\"mappings\":"));
    }

    // ── module_declares_main_fn ─────────────────────────────────────────────

    use bock_air::AIRNode;
    use bock_ast::{Ident, Visibility};
    use bock_errors::{FileId, Span};

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }

    fn ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: dummy_span(),
        }
    }

    fn fn_decl(name: &str) -> AIRNode {
        let body = AIRNode::new(
            1,
            dummy_span(),
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        AIRNode::new(
            0,
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    fn module_with(items: Vec<AIRNode>) -> AIRNode {
        AIRNode::new(
            0,
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items,
            },
        )
    }

    /// A `module <path.segments>` whose `imports` are `use <dep>` of each name
    /// in `uses`, carrying the given top-level `items`.
    fn module_named(path: &str, uses: &[&str], items: Vec<AIRNode>) -> AIRNode {
        use bock_ast::ModulePath;
        let module_path = ModulePath {
            segments: path.split('.').map(ident).collect(),
            span: dummy_span(),
        };
        let imports = uses
            .iter()
            .enumerate()
            .map(|(i, dep)| {
                AIRNode::new(
                    100 + i as u32,
                    dummy_span(),
                    NodeKind::ImportDecl {
                        path: bock_ast::ModulePath {
                            segments: dep.split('.').map(ident).collect(),
                            span: dummy_span(),
                        },
                        items: bock_ast::ImportItems::Glob,
                    },
                )
            })
            .collect();
        AIRNode::new(
            0,
            dummy_span(),
            NodeKind::Module {
                path: Some(module_path),
                annotations: vec![],
                imports,
                items,
            },
        )
    }

    // ── Transpiled-test extraction + assertion classification (S7) ───────────

    /// A `@test`-annotated `fn` named `name` with the given body block.
    fn test_fn_decl(name: &str, body: AIRNode) -> AIRNode {
        use bock_ast::{Annotation, Visibility};
        let annotation = Annotation {
            id: 0,
            name: ident("test"),
            args: vec![],
            span: dummy_span(),
        };
        AIRNode::new(
            0,
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![annotation],
                visibility: Visibility::Private,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    fn identifier(id: u32, name: &str) -> AIRNode {
        AIRNode::new(id, dummy_span(), NodeKind::Identifier { name: ident(name) })
    }

    fn call(id: u32, callee: AIRNode, args: Vec<AIRNode>) -> AIRNode {
        AIRNode::new(
            id,
            dummy_span(),
            NodeKind::Call {
                callee: Box::new(callee),
                args: args
                    .into_iter()
                    .map(|value| AirArg { label: None, value })
                    .collect(),
                type_args: vec![],
            },
        )
    }

    /// Build the lowered AIR for `expect(actual).<method>(expected?)`: a `Call`
    /// whose callee is `FieldAccess(expect(actual), method)` and whose first arg
    /// is the desugared `self` (a copy of the `expect(...)` receiver). This is the
    /// exact shape `bock-air::lower` produces for a method call.
    fn assertion(method: &str, actual: AIRNode, expected: Option<AIRNode>) -> AIRNode {
        let expect_call = call(10, identifier(11, "expect"), vec![actual]);
        let field = AIRNode::new(
            12,
            dummy_span(),
            NodeKind::FieldAccess {
                object: Box::new(expect_call.clone()),
                field: ident(method),
            },
        );
        let mut args = vec![expect_call];
        if let Some(e) = expected {
            args.push(e);
        }
        call(13, field, args)
    }

    #[test]
    fn fn_is_test_detects_test_annotation() {
        let body = AIRNode::new(
            1,
            dummy_span(),
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        assert!(fn_is_test(&test_fn_decl("t", body)));
        assert!(!fn_is_test(&fn_decl("not_a_test")));
    }

    #[test]
    fn collect_test_fns_finds_annotated_functions() {
        let body = AIRNode::new(
            1,
            dummy_span(),
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        let m = module_named(
            "main",
            &[],
            vec![
                fn_decl("main"),
                test_fn_decl("test_a", body.clone()),
                fn_decl("helper"),
                test_fn_decl("test_b", body),
            ],
        );
        let p = std::path::Path::new("x.bock");
        let modules = [(&m, p)];
        let tests = collect_test_fns(&modules);
        assert_eq!(tests.len(), 2);
        let names: Vec<&str> = tests
            .iter()
            .map(|(n, _)| match &n.kind {
                NodeKind::FnDecl { name, .. } => name.name.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(names, vec!["test_a", "test_b"]);
        assert_eq!(tests[0].1, "main");
    }

    #[test]
    fn classify_assertion_recognizes_equal() {
        let stmt = assertion("to_equal", identifier(1, "x"), Some(identifier(2, "y")));
        let (kind, actual, expected) = classify_assertion(&stmt).expect("should classify");
        assert_eq!(kind, TestAssertion::Equal);
        assert!(matches!(&actual.kind, NodeKind::Identifier { name } if name.name == "x"));
        let expected = expected.expect("equal has an expected operand");
        assert!(matches!(&expected.kind, NodeKind::Identifier { name } if name.name == "y"));
    }

    #[test]
    fn classify_assertion_recognizes_nullary_predicates() {
        for (method, expected_kind) in [
            ("to_be_true", TestAssertion::BeTrue),
            ("to_be_false", TestAssertion::BeFalse),
            ("to_be_some", TestAssertion::BeSome),
            ("to_be_none", TestAssertion::BeNone),
            ("to_be_ok", TestAssertion::BeOk),
            ("to_be_err", TestAssertion::BeErr),
        ] {
            let stmt = assertion(method, identifier(1, "v"), None);
            let (kind, _actual, expected) =
                classify_assertion(&stmt).unwrap_or_else(|| panic!("classify {method}"));
            assert_eq!(kind, expected_kind, "method {method}");
            assert!(expected.is_none(), "{method} takes no expected operand");
        }
    }

    #[test]
    fn classify_assertion_rejects_non_assertions() {
        // A plain function call is not an assertion.
        let plain = call(1, identifier(2, "do_thing"), vec![]);
        assert!(classify_assertion(&plain).is_none());
        // A method call whose receiver is not `expect(...)`.
        let other = {
            let recv = call(3, identifier(4, "build"), vec![]);
            let field = AIRNode::new(
                5,
                dummy_span(),
                NodeKind::FieldAccess {
                    object: Box::new(recv.clone()),
                    field: ident("to_equal"),
                },
            );
            call(6, field, vec![recv, identifier(7, "z")])
        };
        assert!(classify_assertion(&other).is_none());
        // An unknown assertion method on `expect(...)`.
        let unknown = assertion("to_be_weird", identifier(8, "v"), None);
        assert!(classify_assertion(&unknown).is_none());
    }

    #[test]
    fn reachable_modules_prunes_unused_prelude_modules() {
        // Mirrors a `bock build`: the embedded `core.*` stdlib is prepended in
        // dependency order, then the user `main`. `main` uses NOTHING, so only
        // `main` should be emitted — never the prelude-only stdlib.
        let core_a = module_named("core.compare", &[], vec![]);
        let core_b = module_named("core.convert", &["core.compare"], vec![]);
        let main_m = module_named("main", &[], vec![fn_decl("main")]);
        let p = std::path::Path::new("x.bock");
        let modules = [(&core_a, p), (&core_b, p), (&main_m, p)];
        let got = reachable_modules(&modules);
        assert_eq!(got.len(), 1, "only the entry module should be reachable");
        assert!(module_declares_main_fn(got[0].0));
    }

    #[test]
    fn reachable_modules_includes_transitive_use_targets() {
        // `main` uses `util`, `util` uses `helper`; an unrelated `unused`
        // module is excluded. The emitted tree must include the transitive
        // `use` closure (main, util, helper) but drop `unused`.
        let helper = module_named("helper", &[], vec![fn_decl("h")]);
        let util = module_named("util", &["helper"], vec![fn_decl("u")]);
        let unused = module_named("unused", &[], vec![fn_decl("x")]);
        let main_m = module_named("main", &["util"], vec![fn_decl("main")]);
        let p = std::path::Path::new("x.bock");
        let modules = [(&helper, p), (&util, p), (&unused, p), (&main_m, p)];
        let got = reachable_modules(&modules);
        let paths: Vec<String> = got
            .iter()
            .map(|(m, _)| {
                let NodeKind::Module { path: Some(pp), .. } = &m.kind else {
                    return String::new();
                };
                pp.segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".")
            })
            .collect();
        assert!(paths.contains(&"main".to_string()));
        assert!(paths.contains(&"util".to_string()));
        assert!(paths.contains(&"helper".to_string()));
        assert!(!paths.contains(&"unused".to_string()), "got: {paths:?}");
        // Dependency order is preserved (helper before util before main).
        let pos = |name: &str| paths.iter().position(|x| x == name).unwrap();
        assert!(pos("helper") < pos("util"));
        assert!(pos("util") < pos("main"));
    }

    #[test]
    fn reachable_modules_order_is_input_order_independent() {
        // The emitted module order must be deterministic regardless of the order
        // the `modules` slice arrives in — the upstream topological sort iterates a
        // `HashMap`/`HashSet` with a per-process random seed, so independent
        // modules can be presented in any (valid) order. `main` uses three
        // mutually-independent cores plus a transitive chain; whatever the input
        // permutation, the reachable order must be byte-identical (and still
        // dependency-before-dependent). This is the guard for the random
        // `bock build` failure once several embedded `core.*` were reachable.
        let leaf = module_named("z.leaf", &[], vec![fn_decl("l")]);
        let a = module_named("core.a", &["z.leaf"], vec![fn_decl("a")]);
        let b = module_named("core.b", &[], vec![fn_decl("b")]);
        let c = module_named("core.c", &[], vec![fn_decl("c")]);
        let main_m = module_named(
            "main",
            &["core.a", "core.b", "core.c"],
            vec![fn_decl("main")],
        );
        let p = std::path::Path::new("x.bock");

        let names = |got: &[(&AIRModule, &std::path::Path)]| -> Vec<String> {
            got.iter()
                .filter_map(|(m, _)| {
                    if let NodeKind::Module { path: Some(pp), .. } = &m.kind {
                        Some(
                            pp.segments
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join("."),
                        )
                    } else {
                        None
                    }
                })
                .collect()
        };

        // Several distinct input permutations of the same module set.
        let perm1 = [(&leaf, p), (&a, p), (&b, p), (&c, p), (&main_m, p)];
        let perm2 = [(&c, p), (&main_m, p), (&b, p), (&leaf, p), (&a, p)];
        let perm3 = [(&main_m, p), (&c, p), (&b, p), (&a, p), (&leaf, p)];
        let o1 = names(&reachable_modules(&perm1));
        let o2 = names(&reachable_modules(&perm2));
        let o3 = names(&reachable_modules(&perm3));
        assert_eq!(o1, o2, "module order must not depend on input order");
        assert_eq!(o1, o3, "module order must not depend on input order");
        // All five reachable, dependency-before-dependent, ties canonical.
        assert_eq!(o1.len(), 5, "got: {o1:?}");
        let pos = |name: &str| o1.iter().position(|x| x == name).unwrap();
        assert!(pos("z.leaf") < pos("core.a"), "got: {o1:?}");
        assert!(pos("core.a") < pos("main"), "got: {o1:?}");
        assert!(pos("core.b") < pos("main"), "got: {o1:?}");
        assert!(pos("core.c") < pos("main"), "got: {o1:?}");
        // `main` (the dependent) is emitted last.
        assert_eq!(o1.last().map(String::as_str), Some("main"), "got: {o1:?}");
    }

    #[test]
    fn module_declares_main_detects_top_level_main() {
        let m = module_with(vec![fn_decl("helper"), fn_decl("main")]);
        assert!(module_declares_main_fn(&m));
    }

    #[test]
    fn module_declares_main_returns_false_when_absent() {
        let m = module_with(vec![fn_decl("helper"), fn_decl("other")]);
        assert!(!module_declares_main_fn(&m));
    }

    #[test]
    fn module_declares_main_returns_false_for_empty_module() {
        let m = module_with(vec![]);
        assert!(!module_declares_main_fn(&m));
    }

    // ── Statement / match / desugar helpers ─────────────────────────────────

    fn n(id: u32, kind: NodeKind) -> AIRNode {
        AIRNode::new(id, dummy_span(), kind)
    }

    fn match_arm(id: u32, body: AIRNode) -> AIRNode {
        n(
            id,
            NodeKind::MatchArm {
                pattern: Box::new(n(id + 1, NodeKind::WildcardPat)),
                guard: None,
                body: Box::new(body),
            },
        )
    }

    #[test]
    fn node_is_statement_classifies_control_flow() {
        assert!(node_is_statement(&n(1, NodeKind::Break { value: None })));
        assert!(node_is_statement(&n(1, NodeKind::Continue)));
        assert!(node_is_statement(&n(1, NodeKind::Return { value: None })));
        assert!(!node_is_statement(&n(
            1,
            NodeKind::Literal {
                lit: bock_ast::Literal::Int("1".into())
            }
        )));
    }

    /// A `{ tail }` block carrying a single tail node.
    fn block_with_tail(id: u32, tail: AIRNode) -> AIRNode {
        n(
            id,
            NodeKind::Block {
                stmts: vec![],
                tail: Some(Box::new(tail)),
            },
        )
    }

    /// A bare `1` literal node (an expression with a usable value).
    fn int_lit(id: u32) -> AIRNode {
        n(
            id,
            NodeKind::Literal {
                lit: bock_ast::Literal::Int("1".into()),
            },
        )
    }

    /// An `if` node: `if <cond> <then_block> [else <else_block>]`. Condition is
    /// a placeholder; only the branch shapes matter for classification.
    fn if_node(id: u32, then_block: AIRNode, else_block: Option<AIRNode>) -> AIRNode {
        n(
            id,
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(n(id + 100, NodeKind::Placeholder)),
                then_block: Box::new(then_block),
                else_block: else_block.map(Box::new),
            },
        )
    }

    #[test]
    fn node_is_statement_classifies_no_else_if_as_statement() {
        // `if (c) { return }` — no else, yields no value → statement (DV15).
        let no_else = if_node(
            1,
            block_with_tail(2, n(3, NodeKind::Return { value: None })),
            None,
        );
        assert!(node_is_statement(&no_else));

        // `if (c) { break }` and `if (c) { continue }` likewise.
        let no_else_break = if_node(
            10,
            block_with_tail(11, n(12, NodeKind::Break { value: None })),
            None,
        );
        assert!(node_is_statement(&no_else_break));
    }

    #[test]
    fn node_is_statement_classifies_all_statement_if_else_as_statement() {
        // `if (c) { return a } else { return b }` — both branches statements,
        // neither yields a value → statement.
        let stmt_both = if_node(
            1,
            block_with_tail(2, n(3, NodeKind::Return { value: None })),
            Some(block_with_tail(4, n(5, NodeKind::Break { value: None }))),
        );
        assert!(node_is_statement(&stmt_both));
    }

    #[test]
    fn node_is_statement_leaves_value_if_else_an_expression() {
        // `let x = if (c) { 1 } else { 2 }` — both branches end in an
        // expression tail, so the `if` yields a value and must stay an
        // expression. Misclassifying it as a statement would break value `if`.
        let value_if = if_node(
            1,
            block_with_tail(2, int_lit(3)),
            Some(block_with_tail(4, int_lit(5))),
        );
        assert!(!node_is_statement(&value_if));
        assert!(!arm_body_is_statement(&value_if));
    }

    #[test]
    fn node_is_statement_leaves_mixed_if_else_an_expression() {
        // One statement branch, one value branch → the `if` can yield a value
        // on the value branch, so it is not a pure statement. Stays expression.
        let mixed = if_node(
            1,
            block_with_tail(2, n(3, NodeKind::Return { value: None })),
            Some(block_with_tail(4, int_lit(5))),
        );
        assert!(!node_is_statement(&mixed));
    }

    #[test]
    fn node_is_statement_handles_else_if_chains() {
        // `if (a) { return } else if (b) { break }` — the `else` is itself a
        // no-else statement `if`, so the whole chain is a statement.
        let inner = if_node(
            20,
            block_with_tail(21, n(22, NodeKind::Break { value: None })),
            None,
        );
        let chain = if_node(
            1,
            block_with_tail(2, n(3, NodeKind::Return { value: None })),
            Some(inner),
        );
        assert!(node_is_statement(&chain));

        // `if (a) { return } else if (b) { 1 } else { 2 }` — the trailing
        // else-if yields a value, so the chain stays an expression.
        let value_inner = if_node(
            30,
            block_with_tail(31, int_lit(32)),
            Some(block_with_tail(33, int_lit(34))),
        );
        let mixed_chain = if_node(
            40,
            block_with_tail(41, n(42, NodeKind::Return { value: None })),
            Some(value_inner),
        );
        assert!(!node_is_statement(&mixed_chain));
    }

    #[test]
    fn arm_body_is_statement_for_block_with_statement_tail() {
        let block_tail_break = n(
            1,
            NodeKind::Block {
                stmts: vec![],
                tail: Some(Box::new(n(2, NodeKind::Break { value: None }))),
            },
        );
        assert!(arm_body_is_statement(&block_tail_break));
        // A block with no tail yields no value → statement.
        let empty = n(
            3,
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        assert!(arm_body_is_statement(&empty));
    }

    #[test]
    fn match_has_statement_arm_detects_break() {
        let arms = vec![
            match_arm(10, n(12, NodeKind::Break { value: None })),
            match_arm(
                20,
                n(
                    22,
                    NodeKind::Literal {
                        lit: bock_ast::Literal::Int("0".into()),
                    },
                ),
            ),
        ];
        assert!(match_has_statement_arm(&arms));

        let value_arms = vec![match_arm(
            30,
            n(
                32,
                NodeKind::Literal {
                    lit: bock_ast::Literal::Int("0".into()),
                },
            ),
        )];
        assert!(!match_has_statement_arm(&value_arms));
    }

    /// A single-segment type path (`Some`, `Ok`, …) for constructor patterns.
    fn ctor_path(name: &str) -> bock_ast::TypePath {
        bock_ast::TypePath {
            segments: vec![ident(name)],
            span: dummy_span(),
        }
    }

    /// A `match` arm with an explicit pattern and optional guard.
    fn arm_with(id: u32, pattern: AIRNode, guard: Option<AIRNode>) -> AIRNode {
        n(
            id,
            NodeKind::MatchArm {
                pattern: Box::new(pattern),
                guard: guard.map(Box::new),
                body: Box::new(int_lit(id + 100)),
            },
        )
    }

    #[test]
    fn match_needs_ifchain_keeps_switch_fast_path_for_simple_matches() {
        // A bind-only / wildcard match (`x => …`, `_ => …`) stays on the switch.
        let bind_arms = vec![
            arm_with(
                1,
                n(
                    2,
                    NodeKind::BindPat {
                        name: ident("x"),
                        is_mut: false,
                    },
                ),
                None,
            ),
            arm_with(3, n(4, NodeKind::WildcardPat), None),
        ];
        assert!(!match_needs_ifchain(&bind_arms));

        // A flat `Some(x)` / `Ok(v)` constructor match (bare-bind fields) stays
        // on the switch — the proven Optional/Result lowering must not regress.
        let flat_ctor = vec![arm_with(
            10,
            n(
                11,
                NodeKind::ConstructorPat {
                    path: ctor_path("Some"),
                    fields: vec![n(
                        12,
                        NodeKind::BindPat {
                            name: ident("x"),
                            is_mut: false,
                        },
                    )],
                },
            ),
            None,
        )];
        assert!(!match_needs_ifchain(&flat_ctor));
    }

    #[test]
    fn match_needs_ifchain_detects_guard() {
        let arms = vec![arm_with(1, n(2, NodeKind::WildcardPat), Some(int_lit(3)))];
        assert!(match_needs_ifchain(&arms));
    }

    #[test]
    fn match_needs_ifchain_detects_or_and_tuple() {
        let or_arm = vec![arm_with(
            1,
            n(
                2,
                NodeKind::OrPat {
                    alternatives: vec![int_lit(3), int_lit(4)],
                },
            ),
            None,
        )];
        assert!(match_needs_ifchain(&or_arm));

        let tuple_arm = vec![arm_with(
            10,
            n(
                11,
                NodeKind::TuplePat {
                    elems: vec![
                        n(
                            12,
                            NodeKind::BindPat {
                                name: ident("a"),
                                is_mut: false,
                            },
                        ),
                        n(
                            13,
                            NodeKind::BindPat {
                                name: ident("b"),
                                is_mut: false,
                            },
                        ),
                    ],
                },
            ),
            None,
        )];
        assert!(match_needs_ifchain(&tuple_arm));
    }

    #[test]
    fn match_needs_ifchain_detects_nested_constructor() {
        // `Some(Ok(v))`: the inner field is itself a constructor → nested.
        let nested = vec![arm_with(
            1,
            n(
                2,
                NodeKind::ConstructorPat {
                    path: ctor_path("Some"),
                    fields: vec![n(
                        3,
                        NodeKind::ConstructorPat {
                            path: ctor_path("Ok"),
                            fields: vec![n(
                                4,
                                NodeKind::BindPat {
                                    name: ident("v"),
                                    is_mut: false,
                                },
                            )],
                        },
                    )],
                },
            ),
            None,
        )];
        assert!(match_needs_ifchain(&nested));
    }

    #[test]
    fn match_needs_ifchain_detects_list_pattern() {
        // `[]`, `[only]`, `[first, ..rest]`: a list pattern has no single
        // `switch` discriminant — every backend that consults the recogniser
        // (ts, go) must route these to the if-chain so elements / `..rest` bind.
        let empty = vec![arm_with(
            1,
            n(
                2,
                NodeKind::ListPat {
                    elems: vec![],
                    rest: None,
                },
            ),
            None,
        )];
        assert!(match_needs_ifchain(&empty));

        let head_rest = vec![arm_with(
            10,
            n(
                11,
                NodeKind::ListPat {
                    elems: vec![n(
                        12,
                        NodeKind::BindPat {
                            name: ident("first"),
                            is_mut: false,
                        },
                    )],
                    rest: Some(Box::new(n(
                        13,
                        NodeKind::BindPat {
                            name: ident("rest"),
                            is_mut: false,
                        },
                    ))),
                },
            ),
            None,
        )];
        assert!(match_needs_ifchain(&head_rest));
    }

    #[test]
    fn match_needs_ifchain_detects_range_pattern() {
        // `1..10` / `1..=10`: a range pattern is a relational test, not a single
        // discriminant, so it cannot ride the `switch` fast-path.
        let range = vec![arm_with(
            1,
            n(
                2,
                NodeKind::RangePat {
                    lo: Box::new(int_lit(3)),
                    hi: Box::new(int_lit(4)),
                    inclusive: false,
                },
            ),
            None,
        )];
        assert!(match_needs_ifchain(&range));
    }

    #[test]
    fn desugared_self_call_matches_shared_receiver_id() {
        // Receiver node with id 5 cloned into both the FieldAccess object and
        // the leading arg — the lowerer\'s desugared-method marker.
        let recv = n(5, NodeKind::Identifier { name: ident("p") });
        let callee = n(
            6,
            NodeKind::FieldAccess {
                object: Box::new(recv.clone()),
                field: ident("m"),
            },
        );
        let args = vec![
            AirArg {
                label: None,
                value: recv,
            },
            AirArg {
                label: None,
                value: n(7, NodeKind::Identifier { name: ident("x") }),
            },
        ];
        let got = desugared_self_call(&callee, &args).expect("should match");
        assert_eq!(got.1.name, "m");
        assert_eq!(got.2.len(), 1); // one non-self arg

        // A genuine field-closure call `(p.f)(p)` has *distinct* receiver
        // nodes (different ids), so it is not treated as a method call.
        let p1 = n(8, NodeKind::Identifier { name: ident("p") });
        let p2 = n(9, NodeKind::Identifier { name: ident("p") });
        let callee2 = n(
            10,
            NodeKind::FieldAccess {
                object: Box::new(p1),
                field: ident("f"),
            },
        );
        let args2 = vec![AirArg {
            label: None,
            value: p2,
        }];
        assert!(desugared_self_call(&callee2, &args2).is_none());
    }

    /// Build a desugared method call `recv.method(extra)` in the AIR shape the
    /// lowerer produces (receiver cloned into both the FieldAccess object and
    /// the leading self arg, sharing a NodeId).
    ///
    /// Returns the (callee, args) pair and the full wrapping `Call` node — the
    /// latter is what carries the checker's `recv_kind` annotation, so a
    /// recogniser that gates on the stamp reads it from there.
    fn desugared_call(method: &str, extra: Vec<AIRNode>) -> (AIRNode, Vec<AirArg>, AIRNode) {
        let recv = n(
            5,
            NodeKind::Identifier {
                name: ident("nums"),
            },
        );
        let callee = n(
            6,
            NodeKind::FieldAccess {
                object: Box::new(recv.clone()),
                field: ident(method),
            },
        );
        let mut args = vec![AirArg {
            label: None,
            value: recv,
        }];
        args.extend(extra.into_iter().map(|value| AirArg { label: None, value }));
        let call_node = n(
            8,
            NodeKind::Call {
                callee: Box::new(callee.clone()),
                args: args.clone(),
                type_args: vec![],
            },
        );
        (callee, args, call_node)
    }

    #[test]
    fn desugared_list_method_matches_read_only_builtins() {
        // Every read-only built-in is recognised, returning the receiver, the
        // method name, and the non-self args.
        for &m in READ_ONLY_LIST_METHODS {
            let extra = match m {
                "get" | "contains" | "index_of" | "concat" | "join" => {
                    vec![n(7, NodeKind::Identifier { name: ident("x") })]
                }
                _ => vec![],
            };
            let n_extra = extra.len();
            let (callee, args, call_node) = desugared_call(m, extra);
            let (recv, got_method, rest) =
                desugared_list_method(&call_node, &callee, &args).expect("should match");
            assert_eq!(got_method, m);
            assert!(matches!(&recv.kind, NodeKind::Identifier { name } if name.name == "nums"));
            assert_eq!(rest.len(), n_extra);
        }
    }

    #[test]
    fn desugared_list_method_rejects_mutating_and_unknown_methods() {
        // Mutating built-ins (DQ18/DQ30 — recognised by their own dedicated
        // recognisers) and arbitrary method names are NOT recognised — they
        // fall through to each backend's generic path.
        for &m in &["push", "pop", "insert", "remove", "clear", "frobnicate"] {
            let (callee, args, call_node) = desugared_call(m, vec![]);
            assert!(
                desugared_list_method(&call_node, &callee, &args).is_none(),
                "{m} should not be recognised as a read-only List method"
            );
        }
    }

    #[test]
    fn desugared_list_inplace_mutator_matches_dq30_methods() {
        // Every DQ30 in-place mutator is recognised (with the `set` exception
        // tested separately), returning receiver, method, and non-self args.
        for &m in INPLACE_LIST_MUTATORS {
            let extra = match m {
                "pop" | "reverse" => vec![],
                "remove_at" => vec![n(7, NodeKind::Identifier { name: ident("i") })],
                _ => vec![
                    n(7, NodeKind::Identifier { name: ident("i") }),
                    n(9, NodeKind::Identifier { name: ident("x") }),
                ],
            };
            let n_extra = extra.len();
            let (callee, args, mut call_node) = desugared_call(m, extra);
            call_node.metadata.insert(
                bock_types::checker::RECV_KIND_META_KEY.to_string(),
                bock_air::Value::String("List".to_string()),
            );
            let (recv, got_method, rest) =
                desugared_list_inplace_mutator(&call_node, &callee, &args).expect("should match");
            assert_eq!(got_method, m);
            assert!(matches!(&recv.kind, NodeKind::Identifier { name } if name.name == "nums"));
            assert_eq!(rest.len(), n_extra);
        }
    }

    #[test]
    fn desugared_list_inplace_mutator_set_requires_explicit_list_stamp() {
        // `set(k, v)` is also a live `Map` method, so the List lowering must
        // only claim it under an explicit `recv_kind = "List"` stamp: an
        // unstamped or Map-stamped `set` falls through to the Map path.
        let extra = vec![
            n(7, NodeKind::Identifier { name: ident("i") }),
            n(9, NodeKind::Identifier { name: ident("x") }),
        ];
        let (callee, args, call_node) = desugared_call("set", extra);
        assert!(
            desugared_list_inplace_mutator(&call_node, &callee, &args).is_none(),
            "unstamped `set` must not be claimed by the List mutator lowering"
        );
        let mut map_stamped = call_node.clone();
        map_stamped.metadata.insert(
            bock_types::checker::RECV_KIND_META_KEY.to_string(),
            bock_air::Value::String("Map".to_string()),
        );
        assert!(
            desugared_list_inplace_mutator(&map_stamped, &callee, &args).is_none(),
            "Map-stamped `set` must not be claimed by the List mutator lowering"
        );
        // `pop` (List-only name) keeps the DQ18 absent-stamp fall-through.
        let (callee_p, args_p, call_p) = desugared_call("pop", vec![]);
        assert!(
            desugared_list_inplace_mutator(&call_p, &callee_p, &args_p).is_some(),
            "unstamped `pop` keeps the absent-stamp fall-through"
        );
    }

    #[test]
    fn desugared_list_inplace_mutator_rejects_user_stamp() {
        // A user record's same-named method (`recv_kind = "User:<name>"`) is
        // never claimed by the built-in mutator lowering.
        for &m in INPLACE_LIST_MUTATORS {
            let (callee, args, mut call_node) = desugared_call(m, vec![]);
            call_node.metadata.insert(
                bock_types::checker::RECV_KIND_META_KEY.to_string(),
                bock_air::Value::String("User:Counter".to_string()),
            );
            assert!(
                desugared_list_inplace_mutator(&call_node, &callee, &args).is_none(),
                "{m} on a user record must not route to the List mutator lowering"
            );
        }
    }

    #[test]
    fn desugared_list_method_accepts_explicit_list_stamp() {
        // A `recv_kind = "List"` stamp (or no stamp) is accepted: the built-in
        // List lowering fires on a genuine list receiver.
        let (callee, args, mut call_node) = desugared_call("len", vec![]);
        call_node.metadata.insert(
            bock_types::checker::RECV_KIND_META_KEY.to_string(),
            bock_air::Value::String("List".to_string()),
        );
        assert!(
            desugared_list_method(&call_node, &callee, &args).is_some(),
            "a `recv_kind = \"List\"` len() must be recognised as the built-in"
        );
    }

    #[test]
    fn desugared_list_method_rejects_same_named_user_record_method() {
        // A user record with its own `len()`/`is_empty()`/`contains()` is stamped
        // `recv_kind = "User:<name>"`; the built-in List lowering must NOT shadow
        // it (Q-r2-codegen-residue item c). The call falls through to the
        // user-method path instead.
        for &m in &["len", "is_empty", "contains", "count", "first"] {
            let extra = if m == "contains" {
                vec![n(7, NodeKind::Identifier { name: ident("x") })]
            } else {
                vec![]
            };
            let (callee, args, mut call_node) = desugared_call(m, extra);
            call_node.metadata.insert(
                bock_types::checker::RECV_KIND_META_KEY.to_string(),
                bock_air::Value::String("User:Counter".to_string()),
            );
            assert!(
                desugared_list_method(&call_node, &callee, &args).is_none(),
                "{m} on a user record (recv_kind=User:Counter) must not route to the List built-in"
            );
        }
    }

    #[test]
    fn desugared_list_functional_method_matches_closure_combinators() {
        // Every functional (closure-taking) built-in is recognised, returning the
        // receiver, the method name, and the non-self args (the closure, plus the
        // seed for `fold`). The closure arg is modelled as a bare identifier here;
        // the recogniser is closure-shape-agnostic.
        for &m in FUNCTIONAL_LIST_METHODS {
            let extra = if m == "fold" {
                vec![
                    n(
                        7,
                        NodeKind::Identifier {
                            name: ident("init"),
                        },
                    ),
                    n(9, NodeKind::Identifier { name: ident("cb") }),
                ]
            } else {
                vec![n(7, NodeKind::Identifier { name: ident("cb") })]
            };
            let n_extra = extra.len();
            let (callee, args, call_node) = desugared_call(m, extra);
            let (recv, got_method, rest) =
                desugared_list_functional_method(&call_node, &callee, &args).expect("should match");
            assert_eq!(got_method, m);
            assert!(matches!(&recv.kind, NodeKind::Identifier { name } if name.name == "nums"));
            assert_eq!(rest.len(), n_extra);
        }
    }

    #[test]
    fn desugared_list_functional_method_rejects_read_only_and_other_stamps() {
        // The read-only built-ins are NOT functional combinators (they route
        // through `desugared_list_method` instead).
        for &m in &["len", "get", "concat", "join", "frobnicate"] {
            let (callee, args, call_node) = desugared_call(m, vec![]);
            assert!(
                desugared_list_functional_method(&call_node, &callee, &args).is_none(),
                "{m} must not be recognised as a functional List method"
            );
        }
        // A non-`List` `recv_kind` (a user record / Map / Set sharing the method
        // name) is rejected so the built-in does not shadow it.
        let extra = vec![n(7, NodeKind::Identifier { name: ident("cb") })];
        let (callee, args, mut call_node) = desugared_call("map", extra);
        call_node.metadata.insert(
            bock_types::checker::RECV_KIND_META_KEY.to_string(),
            bock_air::Value::String("Set".to_string()),
        );
        assert!(
            desugared_list_functional_method(&call_node, &callee, &args).is_none(),
            "map on a Set receiver must not route to the List functional built-in"
        );
    }

    #[test]
    fn is_list_concat_reads_the_checker_stamp() {
        let lhs = n(20, NodeKind::Identifier { name: ident("a") });
        let rhs = n(21, NodeKind::Identifier { name: ident("b") });

        // Two plain (non-list-literal) operands with no stamp → not list concat.
        let plain = n(1, NodeKind::Identifier { name: ident("x") });
        assert!(
            !is_list_concat(&plain, &lhs, &rhs),
            "an unstamped node with non-literal operands is not list concat"
        );

        // The `Bool(true)` checker stamp marks list concat.
        let mut stamped = n(2, NodeKind::Identifier { name: ident("x") });
        stamped.metadata.insert(
            bock_types::checker::LIST_CONCAT_META_KEY.to_string(),
            bock_air::Value::Bool(true),
        );
        assert!(
            is_list_concat(&stamped, &lhs, &rhs),
            "the `Bool(true)` stamp marks list concat"
        );

        // The syntactic fallback fires when an operand is a list literal, even
        // without the stamp (covers `+` sites the checker body pass misses).
        let list_lit = n(22, NodeKind::ListLiteral { elems: vec![] });
        assert!(
            is_list_concat(&plain, &lhs, &list_lit),
            "a list-literal operand marks list concat syntactically"
        );
    }

    // ── Primitive-bridge / Ordering ──────────────────────────────────────────

    #[test]
    fn ordering_variant_recognises_only_the_three_variants() {
        assert_eq!(ordering_variant("Less"), Some("Less"));
        assert_eq!(ordering_variant("Equal"), Some("Equal"));
        assert_eq!(ordering_variant("Greater"), Some("Greater"));
        assert_eq!(ordering_variant("Some"), None);
        assert_eq!(ordering_variant("less"), None);
        assert_eq!(ordering_variant("Ordering"), None);
    }

    /// Build a desugared `recv.method(extra)` call node carrying the checker's
    /// `recv_kind` annotation `tag`, as the consumer sees it post-checking.
    fn annotated_call(
        method: &str,
        tag: &str,
        extra: Vec<AIRNode>,
    ) -> (AIRNode, AIRNode, Vec<AirArg>) {
        let (callee, args, _) = desugared_call(method, extra);
        let mut call = n(
            100,
            NodeKind::Call {
                callee: Box::new(callee.clone()),
                args: args.clone(),
                type_args: vec![],
            },
        );
        call.metadata.insert(
            bock_types::checker::RECV_KIND_META_KEY.to_string(),
            bock_air::Value::String(tag.to_string()),
        );
        (call, callee, args)
    }

    #[test]
    fn primitive_recv_kind_reads_the_annotation() {
        let (call, _, _) = annotated_call("compare", "Primitive:Int", vec![]);
        assert_eq!(primitive_recv_kind(&call), Some("Int"));

        let (call, _, _) = annotated_call("unwrap_or", "Optional", vec![]);
        assert_eq!(primitive_recv_kind(&call), None);

        // No annotation → None.
        let (callee, args, _) = desugared_call("compare", vec![]);
        let bare = n(
            101,
            NodeKind::Call {
                callee: Box::new(callee),
                args,
                type_args: vec![],
            },
        );
        assert_eq!(primitive_recv_kind(&bare), None);
    }

    #[test]
    fn primitive_bridge_call_matches_bridge_methods_on_primitive() {
        for &m in PRIMITIVE_BRIDGE_METHODS {
            let extra = if matches!(m, "compare" | "eq") {
                vec![n(7, NodeKind::Identifier { name: ident("x") })]
            } else {
                vec![]
            };
            let n_extra = extra.len();
            let (call, callee, args) = annotated_call(m, "Primitive:Int", extra);
            let (recv, method, rest, prim) =
                primitive_bridge_call(&call, &callee, &args).expect("should match");
            assert_eq!(method, m);
            assert_eq!(prim, "Int");
            assert_eq!(rest.len(), n_extra);
            assert!(matches!(&recv.kind, NodeKind::Identifier { name } if name.name == "nums"));
        }
    }

    #[test]
    fn primitive_bridge_call_rejects_non_primitive_and_unknown_methods() {
        // Right method, but the receiver is not a primitive → not a bridge call.
        let (call, callee, args) = annotated_call("compare", "User:Point", vec![]);
        assert!(primitive_bridge_call(&call, &callee, &args).is_none());

        // Primitive receiver, but a method the bridge does not cover.
        let (call, callee, args) = annotated_call("frobnicate", "Primitive:Int", vec![]);
        assert!(primitive_bridge_call(&call, &callee, &args).is_none());

        // Primitive receiver + bridge method, but no annotation → not matched.
        let (callee, args, _) = desugared_call("compare", vec![]);
        let bare = n(
            102,
            NodeKind::Call {
                callee: Box::new(callee.clone()),
                args: args.clone(),
                type_args: vec![],
            },
        );
        assert!(primitive_bridge_call(&bare, &callee, &args).is_none());
    }

    #[test]
    fn container_recv_kind_reads_optional_and_result() {
        let (call, _, _) = annotated_call("unwrap_or", "Optional", vec![]);
        assert_eq!(container_recv_kind(&call), Some("Optional"));
        let (call, _, _) = annotated_call("unwrap_or", "Result", vec![]);
        assert_eq!(container_recv_kind(&call), Some("Result"));
        // Non-container tags are not matched.
        let (call, _, _) = annotated_call("unwrap_or", "List", vec![]);
        assert_eq!(container_recv_kind(&call), None);
        let (call, _, _) = annotated_call("compare", "Primitive:Int", vec![]);
        assert_eq!(container_recv_kind(&call), None);
    }

    #[test]
    fn desugared_optional_method_matches_optional_methods() {
        for &m in OPTIONAL_METHODS {
            let extra = if matches!(m, "unwrap_or" | "map" | "flat_map") {
                vec![n(7, NodeKind::Identifier { name: ident("x") })]
            } else {
                vec![]
            };
            let n_extra = extra.len();
            let (call, callee, args) = annotated_call(m, "Optional", extra);
            let (recv, got, rest) =
                desugared_optional_method(&call, &callee, &args).expect("should match");
            assert_eq!(got, m);
            assert_eq!(rest.len(), n_extra);
            assert!(matches!(&recv.kind, NodeKind::Identifier { name } if name.name == "nums"));
            // A `Result`-tagged call must NOT match the Optional recogniser.
            let (call_r, callee_r, args_r) = annotated_call(m, "Result", vec![]);
            assert!(desugared_optional_method(&call_r, &callee_r, &args_r).is_none());
        }
    }

    #[test]
    fn desugared_result_method_matches_result_methods() {
        for &m in RESULT_METHODS {
            let extra = if matches!(m, "unwrap_or" | "map" | "map_err") {
                vec![n(7, NodeKind::Identifier { name: ident("x") })]
            } else {
                vec![]
            };
            let n_extra = extra.len();
            let (call, callee, args) = annotated_call(m, "Result", extra);
            let (recv, got, rest) =
                desugared_result_method(&call, &callee, &args).expect("should match");
            assert_eq!(got, m);
            assert_eq!(rest.len(), n_extra);
            assert!(matches!(&recv.kind, NodeKind::Identifier { name } if name.name == "nums"));
            // An `Optional`-tagged call must NOT match the Result recogniser.
            let (call_o, callee_o, args_o) = annotated_call(m, "Optional", vec![]);
            assert!(desugared_result_method(&call_o, &callee_o, &args_o).is_none());
        }
    }

    #[test]
    fn container_methods_require_the_annotation() {
        // The right method name + receiver shape, but no `recv_kind` annotation
        // → not matched (the disambiguation crux).
        let (callee, args, _) = desugared_call("unwrap_or", vec![]);
        let bare = n(
            103,
            NodeKind::Call {
                callee: Box::new(callee.clone()),
                args: args.clone(),
                type_args: vec![],
            },
        );
        assert!(desugared_optional_method(&bare, &callee, &args).is_none());
        assert!(desugared_result_method(&bare, &callee, &args).is_none());
        // Annotated container, but a method outside the recognised set.
        let (call, callee, args) = annotated_call("frobnicate", "Optional", vec![]);
        assert!(desugared_optional_method(&call, &callee, &args).is_none());
    }

    #[test]
    fn container_recv_kind_reads_map_and_set() {
        let (call, _, _) = annotated_call("get", "Map", vec![]);
        assert_eq!(container_recv_kind(&call), Some("Map"));
        let (call, _, _) = annotated_call("add", "Set", vec![]);
        assert_eq!(container_recv_kind(&call), Some("Set"));
    }

    #[test]
    fn desugared_map_method_matches_map_methods() {
        for &m in MAP_METHODS {
            // `set` takes two args; the others either take one or none — arity is
            // not validated by the recogniser, so pass none and assert it matches.
            let (call, callee, args) = annotated_call(m, "Map", vec![]);
            let (recv, got, _rest) =
                desugared_map_method(&call, &callee, &args).expect("should match");
            assert_eq!(got, m);
            assert!(matches!(&recv.kind, NodeKind::Identifier { name } if name.name == "nums"));
            // A `Set`-tagged call must NOT match the Map recogniser (overlapping
            // names `filter`/`len`/`length`/`count`/`is_empty`/`to_list`/`for_each`).
            let (call_s, callee_s, args_s) = annotated_call(m, "Set", vec![]);
            if SET_METHODS.contains(&m) {
                assert!(desugared_map_method(&call_s, &callee_s, &args_s).is_none());
            }
        }
        // The Map-only membership spelling is `contains_key`, not `contains`
        // (the checker resolves a bare `contains` on a Map to a fresh var).
        let (call, callee, args) = annotated_call("contains", "Map", vec![]);
        assert!(desugared_map_method(&call, &callee, &args).is_none());
    }

    #[test]
    fn desugared_set_method_matches_set_methods() {
        for &m in SET_METHODS {
            let (call, callee, args) = annotated_call(m, "Set", vec![]);
            let (recv, got, _rest) =
                desugared_set_method(&call, &callee, &args).expect("should match");
            assert_eq!(got, m);
            assert!(matches!(&recv.kind, NodeKind::Identifier { name } if name.name == "nums"));
            // A `Map`-tagged call must NOT match the Set recogniser.
            let (call_m, callee_m, args_m) = annotated_call(m, "Map", vec![]);
            if MAP_METHODS.contains(&m) {
                assert!(desugared_set_method(&call_m, &callee_m, &args_m).is_none());
            }
        }
    }

    #[test]
    fn desugared_string_method_matches_string_methods_on_primitive_string() {
        for &m in STRING_METHODS {
            // `replace` takes two extra args, the rest take zero or one; the
            // recogniser is arity-agnostic, so a single placeholder suffices.
            let extra = vec![n(7, NodeKind::Identifier { name: ident("x") })];
            let (call, callee, args) = annotated_call(m, "Primitive:String", extra);
            let (recv, got, rest) =
                desugared_string_method(&call, &callee, &args).expect("should match");
            assert_eq!(got, m);
            assert_eq!(rest.len(), 1);
            assert!(matches!(&recv.kind, NodeKind::Identifier { name } if name.name == "nums"));
            // A non-String primitive receiver must NOT match (e.g. `Int`).
            let (call_i, callee_i, args_i) = annotated_call(m, "Primitive:Int", vec![]);
            assert!(desugared_string_method(&call_i, &callee_i, &args_i).is_none());
        }
    }

    #[test]
    fn desugared_string_method_rejects_unknown_methods_and_missing_annotation() {
        // A String receiver, but a method the recogniser does not cover.
        let (call, callee, args) = annotated_call("frobnicate", "Primitive:String", vec![]);
        assert!(desugared_string_method(&call, &callee, &args).is_none());

        // The right method name + receiver shape, but no `recv_kind` annotation
        // → not matched, so a bare `xs.contains(x)` (a `List`) still falls
        // through to the List recogniser rather than the String one.
        let (callee, args, _) = desugared_call(
            "contains",
            vec![n(7, NodeKind::Identifier { name: ident("x") })],
        );
        let bare = n(
            105,
            NodeKind::Call {
                callee: Box::new(callee.clone()),
                args: args.clone(),
                type_args: vec![],
            },
        );
        assert!(desugared_string_method(&bare, &callee, &args).is_none());
    }

    #[test]
    fn map_set_methods_require_the_annotation() {
        // The right method name + receiver shape, but no `recv_kind` annotation
        // → not matched. A bare `m.get(k)` without the annotation must fall
        // through to the List recogniser, not the Map one.
        let (callee, args, _) = desugared_call("get", vec![]);
        let bare = n(
            104,
            NodeKind::Call {
                callee: Box::new(callee.clone()),
                args: args.clone(),
                type_args: vec![],
            },
        );
        assert!(desugared_map_method(&bare, &callee, &args).is_none());
        assert!(desugared_set_method(&bare, &callee, &args).is_none());
    }

    #[test]
    fn param_binds_self_detects_self_param() {
        let self_p = n(
            1,
            NodeKind::Param {
                pattern: Box::new(n(
                    2,
                    NodeKind::BindPat {
                        name: ident("self"),
                        is_mut: false,
                    },
                )),
                ty: None,
                default: None,
            },
        );
        assert_eq!(param_binds_self(&self_p), Some(false));

        let other = n(
            3,
            NodeKind::Param {
                pattern: Box::new(n(
                    4,
                    NodeKind::BindPat {
                        name: ident("x"),
                        is_mut: false,
                    },
                )),
                ty: None,
                default: None,
            },
        );
        assert_eq!(param_binds_self(&other), None);
    }

    #[test]
    fn loop_needs_break_label_when_match_arm_breaks() {
        // loop body: { match _ { _ => break } }
        let match_node = n(
            1,
            NodeKind::Match {
                scrutinee: Box::new(n(2, NodeKind::Identifier { name: ident("i") })),
                arms: vec![match_arm(3, n(5, NodeKind::Break { value: None }))],
            },
        );
        let body = n(
            6,
            NodeKind::Block {
                stmts: vec![match_node],
                tail: None,
            },
        );
        assert!(loop_needs_break_label(&body));

        // A match whose arms only return values needs no label.
        let value_match = n(
            10,
            NodeKind::Match {
                scrutinee: Box::new(n(11, NodeKind::Identifier { name: ident("i") })),
                arms: vec![match_arm(
                    12,
                    n(
                        14,
                        NodeKind::Literal {
                            lit: bock_ast::Literal::Int("0".into()),
                        },
                    ),
                )],
            },
        );
        let body2 = n(
            15,
            NodeKind::Block {
                stmts: vec![value_match],
                tail: None,
            },
        );
        assert!(!loop_needs_break_label(&body2));
    }

    // ── Enum-variant registry ───────────────────────────────────────────────

    /// Build an `EnumVariant` AIR node with the given payload.
    fn enum_variant(name: &str, payload: EnumVariantPayload) -> AIRNode {
        n(
            0,
            NodeKind::EnumVariant {
                name: ident(name),
                payload,
            },
        )
    }

    /// Build a `struct`-variant field-decl with the given name (type is a
    /// placeholder — `collect_enum_variants` only reads the field name).
    fn record_field(name: &str) -> bock_ast::RecordDeclField {
        bock_ast::RecordDeclField {
            id: 0,
            span: dummy_span(),
            name: ident(name),
            ty: bock_ast::TypeExpr::Named {
                id: 0,
                span: dummy_span(),
                path: bock_ast::TypePath {
                    segments: vec![ident("Int")],
                    span: dummy_span(),
                },
                args: vec![],
            },
            default: None,
        }
    }

    /// Build an `EnumDecl` AIR node named `name` with the given variants.
    fn enum_decl(name: &str, variants: Vec<AIRNode>) -> AIRNode {
        n(
            0,
            NodeKind::EnumDecl {
                annotations: vec![],
                visibility: bock_ast::Visibility::Public,
                name: ident(name),
                generic_params: vec![],
                variants,
            },
        )
    }

    /// A `TypePath` of a single segment (a bare variant name at a use site).
    fn variant_path(name: &str) -> bock_ast::TypePath {
        bock_ast::TypePath {
            segments: vec![ident(name)],
            span: dummy_span(),
        }
    }

    #[test]
    fn collect_enum_variants_records_all_payload_kinds() {
        // enum Shape { Circle { radius } | Rect(_, _) | Empty }
        let shape = enum_decl(
            "Shape",
            vec![
                enum_variant(
                    "Circle",
                    EnumVariantPayload::Struct(vec![record_field("radius")]),
                ),
                enum_variant(
                    "Rect",
                    EnumVariantPayload::Tuple(vec![
                        n(1, NodeKind::Placeholder),
                        n(2, NodeKind::Placeholder),
                    ]),
                ),
                enum_variant("Empty", EnumVariantPayload::Unit),
            ],
        );
        let m = module_named("main", &[], vec![shape]);
        let p = std::path::Path::new("x.bock");
        let reg = collect_enum_variants(&[(&m, p)]);

        let circle = reg.get("Circle").expect("Circle registered");
        assert_eq!(circle.enum_name, "Shape");
        assert_eq!(
            circle.payload,
            VariantPayloadKind::Struct(vec!["radius".to_string()])
        );

        let rect = reg.get("Rect").expect("Rect registered");
        assert_eq!(rect.enum_name, "Shape");
        assert_eq!(rect.payload, VariantPayloadKind::Tuple(2));

        let empty = reg.get("Empty").expect("Empty registered");
        assert_eq!(empty.enum_name, "Shape");
        assert_eq!(empty.payload, VariantPayloadKind::Unit);
    }

    #[test]
    fn collect_enum_variants_pre_seeds_optional_and_result() {
        // An empty module set still carries the built-in Optional/Result entries
        // so one mechanism describes both user and built-in ADTs (B1).
        let reg = collect_enum_variants(&[]);
        assert_eq!(
            reg.get("Some").map(|i| i.enum_name.as_str()),
            Some("Optional")
        );
        assert_eq!(
            reg.get("Some").map(|i| &i.payload),
            Some(&VariantPayloadKind::Tuple(1))
        );
        assert_eq!(
            reg.get("None").map(|i| &i.payload),
            Some(&VariantPayloadKind::Unit)
        );
        assert_eq!(reg.get("Ok").map(|i| i.enum_name.as_str()), Some("Result"));
        assert_eq!(reg.get("Err").map(|i| i.enum_name.as_str()), Some("Result"));
    }

    #[test]
    fn collect_enum_variants_spans_multiple_modules() {
        // A `use`d enum in another reached module is still registered (the
        // pre-scan walks every module, so a forward / cross-module reference
        // resolves).
        let color = enum_decl("Color", vec![enum_variant("Red", EnumVariantPayload::Unit)]);
        let lib = module_named("lib", &[], vec![color]);
        let main_m = module_named("main", &["lib"], vec![fn_decl("main")]);
        let p = std::path::Path::new("x.bock");
        let reg = collect_enum_variants(&[(&lib, p), (&main_m, p)]);
        assert_eq!(reg.get("Red").map(|i| i.enum_name.as_str()), Some("Color"));
    }

    /// A `fn <name>() { <stmts> }` declaration carrying the given body
    /// statements — used to plant a bare-variant reference in a use site.
    fn fn_decl_with_body(name: &str, stmts: Vec<AIRNode>) -> AIRNode {
        let body = AIRNode::new(900, dummy_span(), NodeKind::Block { stmts, tail: None });
        AIRNode::new(
            0,
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    #[test]
    fn implicit_esm_imports_glob_imported_enum_variant_by_bare_name() {
        // `module models` declares `public enum Category { Electronics Clothing }`.
        // `module main` does `use models.*` (a glob import — `module_named` emits
        // ImportItems::Glob) and references the bare variant `Electronics`. The
        // shared collector must produce an implicit import keyed on the *emitted*
        // value-name `Category_Electronics`, even though the AIR only ever spells
        // the bare source name. Without it `main.js`/`main.ts` omit the import and
        // ReferenceError/TS2304 at the use site (inventory-system regression).
        let category = enum_decl(
            "Category",
            vec![
                enum_variant("Electronics", EnumVariantPayload::Unit),
                enum_variant("Clothing", EnumVariantPayload::Unit),
            ],
        );
        let models = module_named("models", &[], vec![category]);
        // `fn use_it() { let _ = Electronics }` — bare-variant reference.
        let use_electronics = AIRNode::new(
            901,
            dummy_span(),
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(AIRNode::new(
                    902,
                    dummy_span(),
                    NodeKind::BindPat {
                        name: ident("_"),
                        is_mut: false,
                    },
                )),
                ty: None,
                value: Box::new(identifier(903, "Electronics")),
            },
        );
        let main_m = module_named(
            "main",
            &["models"],
            vec![fn_decl_with_body("use_it", vec![use_electronics])],
        );
        let p = std::path::Path::new("x.bock");

        let public_symbols = collect_public_symbols_for_esm(&[(&models, p), (&main_m, p)]);
        let imports = implicit_esm_imports_for(&main_m, &public_symbols, "main");

        let variant_import = imports
            .iter()
            .find(|i| i.name == "Category_Electronics")
            .expect(
                "glob-imported bare variant `Electronics` must import as `Category_Electronics`",
            );
        assert_eq!(variant_import.module_path, "models");
        assert_eq!(variant_import.kind, EsmDeclKind::EnumVariant);
        // The unreferenced sibling variant must NOT be over-imported (the scan is
        // by bare name and `Clothing` never appears at a use site).
        assert!(
            !imports.iter().any(|i| i.name == "Category_Clothing"),
            "unreferenced variant `Clothing` must not be imported; got: {imports:?}"
        );
    }

    #[test]
    fn registered_variant_resolves_last_path_segment() {
        let shape = enum_decl(
            "Shape",
            vec![enum_variant("Empty", EnumVariantPayload::Unit)],
        );
        let m = module_named("main", &[], vec![shape]);
        let p = std::path::Path::new("x.bock");
        let reg = collect_enum_variants(&[(&m, p)]);
        // A bare variant path resolves.
        assert_eq!(
            registered_variant(&reg, &variant_path("Empty")).map(|i| i.enum_name.as_str()),
            Some("Shape")
        );
        // An unknown name does not.
        assert!(registered_variant(&reg, &variant_path("Nope")).is_none());
    }

    // ── Generic-decl registry ───────────────────────────────────────────────

    /// `record Box[T] { value: T }`.
    fn generic_record_decl(name: &str, params: &[&str]) -> AIRNode {
        n(
            0,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: bock_ast::Visibility::Public,
                name: ident(name),
                generic_params: params
                    .iter()
                    .map(|p| bock_ast::GenericParam {
                        id: 0,
                        span: dummy_span(),
                        name: ident(p),
                        bounds: vec![],
                    })
                    .collect(),
                fields: vec![record_field("value")],
            },
        )
    }

    #[test]
    fn collect_generic_decls_records_params_and_spans_modules() {
        let boxed = generic_record_decl("Box", &["T"]);
        let pair = generic_record_decl("Pair", &["A", "B"]);
        let plain = generic_record_decl("Plain", &[]);
        let lib = module_named("lib", &[], vec![pair]);
        let main_m = module_named("main", &["lib"], vec![boxed, plain]);
        let p = std::path::Path::new("x.bock");
        let reg = collect_generic_decls(&[(&lib, p), (&main_m, p)]);

        // Single-param generic.
        let box_params = reg.get("Box").expect("Box registered");
        assert_eq!(box_params.len(), 1);
        assert_eq!(box_params[0].name.name, "T");

        // Two-param generic, declaration order preserved, across module boundary.
        let pair_params = reg.get("Pair").expect("Pair registered");
        assert_eq!(pair_params.len(), 2);
        assert_eq!(pair_params[0].name.name, "A");
        assert_eq!(pair_params[1].name.name, "B");

        // Non-generic decl is present with an empty param list.
        assert_eq!(reg.get("Plain").map(Vec::len), Some(0));
        // Unknown type is absent.
        assert!(!reg.contains_key("Nope"));
    }

    // ── Trait-declaration registry ─────────────────────────────────────────

    /// A trait method `FnDecl`. `default_body` controls the body block: when
    /// true a non-empty block (a default method), else an empty block (a
    /// required method). `self_operand` adds a second `other: Self` param.
    fn trait_method(name: &str, default_body: bool, self_operand: bool) -> AIRNode {
        let tail = if default_body {
            Some(Box::new(n(
                50,
                NodeKind::Literal {
                    lit: bock_ast::Literal::Unit,
                },
            )))
        } else {
            None
        };
        let body = n(
            40,
            NodeKind::Block {
                stmts: vec![],
                tail,
            },
        );
        let mut params = vec![n(
            41,
            NodeKind::Param {
                pattern: Box::new(n(
                    42,
                    NodeKind::BindPat {
                        name: ident("self"),
                        is_mut: false,
                    },
                )),
                ty: None,
                default: None,
            },
        )];
        if self_operand {
            params.push(n(
                43,
                NodeKind::Param {
                    pattern: Box::new(n(
                        44,
                        NodeKind::BindPat {
                            name: ident("other"),
                            is_mut: false,
                        },
                    )),
                    ty: Some(Box::new(n(45, NodeKind::TypeSelf))),
                    default: None,
                },
            ));
        }
        n(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params,
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    fn trait_decl(name: &str, methods: Vec<AIRNode>) -> AIRNode {
        n(
            5,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident(name),
                generic_params: vec![],
                associated_types: vec![],
                methods,
            },
        )
    }

    #[test]
    fn is_default_method_uses_empty_block_heuristic() {
        // A non-empty body block (tail expr) → default method.
        assert!(is_default_method(&trait_method("dflt", true, false)));
        // An empty body block → required method.
        assert!(!is_default_method(&trait_method("req", false, false)));
    }

    #[test]
    fn collect_trait_decls_records_methods_and_spans_modules() {
        let eq = trait_decl(
            "Eq",
            vec![
                trait_method("equals", false, true),
                trait_method("not_equals", true, true),
            ],
        );
        let other = trait_decl("Show", vec![trait_method("show", false, false)]);
        let lib = module_named("lib", &[], vec![other]);
        let main_m = module_named("main", &["lib"], vec![eq]);
        let p = std::path::Path::new("x.bock");
        let reg = collect_trait_decls(&[(&lib, p), (&main_m, p)]);

        let eq_info = reg.get("Eq").expect("Eq registered");
        assert_eq!(eq_info.methods.len(), 2);
        // `Show` from the other module is also registered.
        assert!(reg.contains_key("Show"));
    }

    #[test]
    fn inherited_default_methods_excludes_overridden_and_required() {
        // trait Eq { equals (required); not_equals (default) }
        let eq = trait_decl(
            "Eq",
            vec![
                trait_method("equals", false, true),
                trait_method("not_equals", true, true),
            ],
        );
        let m = module_named("main", &[], vec![eq]);
        let p = std::path::Path::new("x.bock");
        let reg = collect_trait_decls(&[(&m, p)]);
        let trait_path = variant_path("Eq");

        // An impl overriding only `equals` inherits the `not_equals` default.
        let impl_methods = vec![fn_decl("equals")];
        let inherited = inherited_default_methods(&reg, &trait_path, &impl_methods);
        assert_eq!(inherited.len(), 1);
        assert_eq!(fn_decl_name(&inherited[0]), Some("not_equals"));

        // An impl overriding the default too inherits nothing.
        let impl_methods = vec![fn_decl("equals"), fn_decl("not_equals")];
        assert!(inherited_default_methods(&reg, &trait_path, &impl_methods).is_empty());

        // The required method is never synthesized even when not overridden.
        let inherited = inherited_default_methods(&reg, &trait_path, &[]);
        assert_eq!(inherited.len(), 1);
        assert_eq!(fn_decl_name(&inherited[0]), Some("not_equals"));
    }

    #[test]
    fn trait_uses_self_operand_detects_self_typed_params() {
        // `equals(self, other: Self)` references `Self` in a non-receiver param.
        let with_self = TraitDeclInfo {
            generic_params: vec![],
            methods: vec![trait_method("equals", false, true)],
        };
        assert!(trait_uses_self_operand(&with_self));

        // `show(self)` has only the receiver — no `Self` operand.
        let without_self = TraitDeclInfo {
            generic_params: vec![],
            methods: vec![trait_method("show", false, false)],
        };
        assert!(!trait_uses_self_operand(&without_self));
    }

    #[test]
    fn collect_exported_type_names_records_only_public_types() {
        let pub_rec = generic_record_decl("Key", &[]); // public by helper
        let priv_rec = n(
            70,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                name: ident("Hidden"),
                generic_params: vec![],
                fields: vec![],
            },
        );
        let m = module_named("main", &[], vec![pub_rec, priv_rec]);
        let p = std::path::Path::new("x.bock");
        let names = collect_exported_type_names(&[(&m, p)]);
        assert!(names.contains("Key"));
        assert!(!names.contains("Hidden"));
    }

    // ── Value-position diverging-CF temp-hoist desugar ───────────────────────

    /// A `return <int>` statement node.
    fn return_int(id: u32) -> AIRNode {
        n(
            id,
            NodeKind::Return {
                value: Some(Box::new(int_lit(id + 1))),
            },
        )
    }

    #[test]
    fn value_cf_diverges_detects_if_with_return_branch() {
        // `if (c) { 1 } else { return 0 }` — one value arm, one diverging arm.
        let node = if_node(
            1,
            block_with_tail(2, int_lit(3)),
            Some(block_with_tail(4, return_int(5))),
        );
        assert!(value_cf_diverges(&node));
    }

    #[test]
    fn value_cf_diverges_skips_plain_value_if() {
        // `if (c) { 1 } else { 2 }` — both arms yield a value; not diverging.
        let node = if_node(
            1,
            block_with_tail(2, int_lit(3)),
            Some(block_with_tail(4, int_lit(5))),
        );
        assert!(!value_cf_diverges(&node));
    }

    #[test]
    fn value_cf_diverges_detects_nested_else_if_chain() {
        // `if (a) { 1 } else { if (b) { 2 } else { return 0 } }` — chat-protocol
        // shape: the diverging `return` is buried in a nested else-if.
        let inner = if_node(
            10,
            block_with_tail(11, int_lit(12)),
            Some(block_with_tail(13, return_int(14))),
        );
        let outer = if_node(1, block_with_tail(2, int_lit(3)), Some(inner));
        assert!(value_cf_diverges(&outer));
    }

    #[test]
    fn value_cf_diverges_hoists_value_loop_only() {
        // A `loop` that yields a value via `break <v>` needs statement-form
        // delivery in value position.
        let value_loop = n(
            1,
            NodeKind::Loop {
                body: Box::new(block_with_tail(
                    2,
                    n(
                        3,
                        NodeKind::Break {
                            value: Some(Box::new(int_lit(4))),
                        },
                    ),
                )),
            },
        );
        assert!(value_cf_diverges(&value_loop));

        // A value-less `loop` (bare `break`, result discarded) has a clean
        // statement form already and must NOT be hoisted (else the temp would be
        // left uninitialised).
        let unit_loop = n(
            10,
            NodeKind::Loop {
                body: Box::new(block_with_tail(11, n(12, NodeKind::Break { value: None }))),
            },
        );
        assert!(!value_cf_diverges(&unit_loop));
    }

    #[test]
    fn value_cf_diverges_detects_match_with_return_arm() {
        // `match s { _ => 1, _ => return }` — one value arm, one diverging.
        let arms = vec![
            match_arm(10, int_lit(12)),
            match_arm(20, n(22, NodeKind::Return { value: None })),
        ];
        let m = n(
            1,
            NodeKind::Match {
                scrutinee: Box::new(n(2, NodeKind::Placeholder)),
                arms,
            },
        );
        assert!(value_cf_diverges(&m));
    }

    /// Extract the single `FnDecl` body block from a hoisted module wrapper.
    fn hoisted_let_block(value: AIRNode) -> AIRNode {
        // fn f() { let x = <value> }
        let let_pat = n(
            900,
            NodeKind::BindPat {
                name: ident("x"),
                is_mut: false,
            },
        );
        let let_binding = n(
            901,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(let_pat),
                ty: None,
                value: Box::new(value),
            },
        );
        let body = n(
            902,
            NodeKind::Block {
                stmts: vec![let_binding],
                tail: None,
            },
        );
        let fn_decl = n(
            903,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("f"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        module_named("main", &[], vec![fn_decl])
    }

    /// Return the statement list of the hoisted module's `fn f` body block.
    fn fn_body_stmts(module: &AIRNode) -> &[AIRNode] {
        let NodeKind::Module { items, .. } = &module.kind else {
            panic!("module");
        };
        let NodeKind::FnDecl { body, .. } = &items[0].kind else {
            panic!("fn");
        };
        let NodeKind::Block { stmts, .. } = &body.kind else {
            panic!("body block");
        };
        stmts
    }

    /// The `let x = …` binding among a statement list.
    fn find_let_x(stmts: &[AIRNode]) -> &AIRNode {
        stmts
            .iter()
            .find(|s| {
                matches!(&s.kind, NodeKind::LetBinding { pattern, .. }
                    if matches!(&pattern.kind, NodeKind::BindPat { name, .. } if name.name == "x"))
            })
            .expect("let x binding")
    }

    #[test]
    fn hoist_rewrites_diverging_let_into_prelude_and_temp_read() {
        // `let x = if (c) { 1 } else { return 0 }` splices, in the enclosing
        // block, before the `let`:
        //   let mut __bock_cf_0
        //   if (c) { __bock_cf_0 = 1 } else { return 0 }
        //   let x = __bock_cf_0
        let value = if_node(
            1,
            block_with_tail(2, int_lit(3)),
            Some(block_with_tail(4, return_int(5))),
        );
        let module = hoist_value_cf(hoisted_let_block(value));
        let stmts = fn_body_stmts(&module);
        assert_eq!(stmts.len(), 3, "decl + CF-stmt + let; got {}", stmts.len());
        // stmts[0]: declare-only temp.
        assert!(
            matches!(&stmts[0].kind, NodeKind::LetBinding { is_mut: true, .. }),
            "first stmt must be the mut temp decl, got {:?}",
            stmts[0].kind
        );
        assert_eq!(
            stmts[0].metadata.get(DECL_ONLY_META),
            Some(&bock_air::stubs::Value::Bool(true)),
            "temp decl must carry the declare-only marker"
        );
        // stmts[1]: relocated `if`, value arm → Assign, diverging arm kept.
        let NodeKind::If {
            then_block,
            else_block,
            ..
        } = &stmts[1].kind
        else {
            panic!("expected relocated If, got {:?}", stmts[1].kind);
        };
        // The value arm's block now ends in an `Assign` statement (the tail was
        // moved into `stmts` as `temp = 1`).
        let NodeKind::Block {
            stmts: then_stmts,
            tail: then_tail,
        } = &then_block.kind
        else {
            panic!("then block");
        };
        let then_last = then_tail
            .as_deref()
            .or_else(|| then_stmts.last())
            .map(|t| &t.kind);
        assert!(
            matches!(then_last, Some(NodeKind::Assign { .. })),
            "value arm must end in an Assign, got {then_last:?}"
        );
        // The diverging arm keeps its `return` (as tail or last statement).
        let NodeKind::Block {
            stmts: else_stmts,
            tail: else_tail,
        } = &else_block.as_ref().unwrap().kind
        else {
            panic!("else block");
        };
        let else_last = else_tail
            .as_deref()
            .or_else(|| else_stmts.last())
            .map(|t| &t.kind);
        assert!(
            matches!(else_last, Some(NodeKind::Return { .. })),
            "diverging arm must keep its return, got {else_last:?}"
        );
        // stmts[2]: `let x = __bock_cf_0` (a temp read, not a Block/IIFE).
        let NodeKind::LetBinding { value, .. } = &find_let_x(stmts).kind else {
            panic!("let x");
        };
        assert!(
            matches!(&value.kind, NodeKind::Identifier { name } if name.name.starts_with("__bock_cf_")),
            "let value must read the temp identifier, got {:?}",
            value.kind
        );
    }

    #[test]
    fn hoist_leaves_plain_value_let_untouched() {
        // `let x = if (c) { 1 } else { 2 }` must NOT be hoisted (no divergence).
        let value = if_node(
            1,
            block_with_tail(2, int_lit(3)),
            Some(block_with_tail(4, int_lit(5))),
        );
        let module = hoist_value_cf(hoisted_let_block(value));
        let stmts = fn_body_stmts(&module);
        assert_eq!(stmts.len(), 1, "no prelude for a plain value if");
        let NodeKind::LetBinding { value, .. } = &stmts[0].kind else {
            panic!("let");
        };
        assert!(
            matches!(&value.kind, NodeKind::If { .. }),
            "plain value if must stay the let's If value, got {:?}",
            value.kind
        );
    }

    #[test]
    fn hoist_rewrites_loop_break_value() {
        // `let x = loop { break 1 }` splices a value-loop whose `break 1` becomes
        // `{ __bock_cf_0 = 1; break }`, then `let x = __bock_cf_0`.
        let loop_value = n(
            1,
            NodeKind::Loop {
                body: Box::new(n(
                    2,
                    NodeKind::Block {
                        stmts: vec![n(
                            3,
                            NodeKind::Break {
                                value: Some(Box::new(int_lit(4))),
                            },
                        )],
                        tail: None,
                    },
                )),
            },
        );
        let module = hoist_value_cf(hoisted_let_block(loop_value));
        let stmts = fn_body_stmts(&module);
        assert_eq!(stmts.len(), 3);
        assert!(matches!(&stmts[1].kind, NodeKind::Loop { .. }));
        // The loop now contains a bare `break` (value hoisted into an Assign).
        let mut found_bare_break = false;
        struct BreakFinder<'a>(&'a mut bool);
        impl bock_air::visitor::Visitor for BreakFinder<'_> {
            fn visit_node(&mut self, node: &AIRNode) {
                if matches!(&node.kind, NodeKind::Break { value: None }) {
                    *self.0 = true;
                }
                bock_air::visitor::walk_node(self, node);
            }
        }
        use bock_air::visitor::Visitor;
        BreakFinder(&mut found_bare_break).visit_node(&stmts[1]);
        assert!(
            found_bare_break,
            "break value must be hoisted into an Assign, leaving a bare break"
        );
    }
}
