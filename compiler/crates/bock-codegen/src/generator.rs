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
    /// Per spec §20.6.1, each source file produces a corresponding target file
    /// at the source-mirrored path. The default implementation invokes
    /// `generate_module` per module, then rewrites each emitted file's path
    /// (and its source map's `generated_file`) using
    /// [`derive_output_path`]. The entry-point invocation, if any, is
    /// appended to the file generated from the module that declares `main`.
    ///
    /// Generators with cross-module concerns (e.g., Go's `package` declaration
    /// or async-function pre-scan) should override this method.
    fn generate_project(
        &self,
        modules: &[(&AIRModule, &Path)],
    ) -> Result<GeneratedCode, CodegenError> {
        let main_is_async = modules.iter().any(|(m, _)| module_main_fn_is_async(m));
        let invocation = self.entry_invocation(main_is_async);

        let mut all_files: Vec<OutputFile> = Vec::with_capacity(modules.len());

        for (module, source_path) in modules {
            let code = self.generate_module(module)?;
            let derived = derive_output_path(source_path, self.target());
            let derived_name = derived
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let needs_invocation = invocation.is_some() && module_declares_main_fn(module);

            for mut file in code.files {
                file.path = derived.clone();
                if let Some(sm) = file.source_map.as_mut() {
                    sm.generated_file = derived_name.clone();
                }
                if needs_invocation {
                    if let Some(invoc) = invocation.as_ref() {
                        if !file.content.is_empty() && !file.content.ends_with('\n') {
                            file.content.push('\n');
                        }
                        file.content.push_str(invoc);
                    }
                }
                all_files.push(file);
            }
        }

        Ok(GeneratedCode { files: all_files })
    }
}

/// Restrict `modules` to those **reachable** from the entry module via real
/// `use` edges, preserving the input (dependency) order.
///
/// `bock build` prepends the entire embedded `core.*` stdlib and makes every
/// user module implicitly depend on all of it (the §18.2 prelude, so core
/// symbols resolve without an explicit `use`). That implicit dependency is
/// correct for *name resolution* but wrong for *bundling*: concatenating a core
/// module a program never references both bloats the output and — until the
/// stdlib is codegen-clean on every target — drags its latent codegen defects
/// into the entry file. Bundling must therefore include only modules the entry
/// program actually reaches through a real `use`.
///
/// Reachability is the transitive closure of each module's `ImportDecl` paths
/// (the explicit `use`s) matched against other modules' declared `module`
/// path — never the synthetic prelude edges, which are not represented as
/// `ImportDecl`s in the AIR. A program with no `use` (e.g. `hello_world`) thus
/// bundles to its entry module alone, exactly matching the pre-bundling
/// single-file run (no regression).
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

    // BFS the explicit-`use` graph from the entry module.
    let mut reachable = vec![false; modules.len()];
    let mut stack = vec![entry_idx];
    reachable[entry_idx] = true;
    while let Some(idx) = stack.pop() {
        for target in use_targets(modules[idx].0) {
            if let Some(&t) = by_path.get(&target) {
                if !reachable[t] {
                    reachable[t] = true;
                    stack.push(t);
                }
            }
        }
    }

    modules
        .iter()
        .enumerate()
        .filter(|(i, _)| reachable[*i])
        .map(|(_, &pair)| pair)
        .collect()
}

/// Choose the output path for a **single-file bundle** of `modules`.
///
/// Cross-module programs are emitted as one entry file (see §20.6.1 OPEN
/// divergence: the per-module tree is collapsed into a single runnable file so
/// the single-file run model — `node main.js`, `python3 main.py`, … — can run
/// an importing program). The bundle is named after the module that declares
/// `main` (the entry point); if none does (e.g. a library), the last module in
/// dependency order names the file. Modules arrive dependency-ordered, so the
/// last one is the top of the dependency graph — the natural entry.
///
/// Returns the source-mirrored output path (e.g. `main.<ext>`) for the chosen
/// module, or `None` when `modules` is empty.
#[must_use]
pub fn bundle_output_path(
    modules: &[(&AIRModule, &Path)],
    target: &TargetProfile,
) -> Option<PathBuf> {
    let entry = modules
        .iter()
        .find(|(m, _)| module_declares_main_fn(m))
        .or_else(|| modules.last())?;
    Some(derive_output_path(entry.1, target))
}

/// Append the entry-point invocation (e.g. `main();`) to a bundled file's
/// content exactly once, when any bundled module declares a top-level `main`.
///
/// Backends with a synthetic entry call (JS/TS/Python) supply `invocation` via
/// [`CodeGenerator::entry_invocation`]; native-entry targets (Rust `fn main`,
/// Go `func main`) pass `None` and this is a no-op. Bundling concatenates every
/// module body into one file, so the invocation must be appended once at the
/// end — never per module.
pub fn append_entry_invocation(
    content: &mut String,
    modules: &[(&AIRModule, &Path)],
    invocation: Option<&String>,
) {
    let Some(invoc) = invocation else { return };
    if !modules.iter().any(|(m, _)| module_declares_main_fn(m)) {
        return;
    }
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(invoc);
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
/// — i.e. it is an or-pattern, a tuple pattern, or a constructor/record pattern
/// carrying a nested structured sub-pattern. See [`match_needs_ifchain`].
fn pattern_needs_ifchain(pat: &AIRNode) -> bool {
    match &pat.kind {
        NodeKind::OrPat { .. } | NodeKind::TuplePat { .. } => true,
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
/// natively per target (see [`desugared_list_method`]). Mutating methods
/// (`push`/`pop`/`insert`/…) are intentionally excluded — their value-vs-`mut
/// self` semantics is an open design question (DQ18) and `core.iter` does not
/// need them.
pub const READ_ONLY_LIST_METHODS: &[&str] = &[
    "len", "length", "count", "is_empty", "get", "contains", "first", "last", "concat", "index_of",
    "join",
];

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
/// Returns the receiver, the (validated) method name, and the remaining
/// (non-self) arguments. The element type of the list is intentionally *not*
/// inspected here: the checker has already type-checked the call, and each
/// backend's lowering is element-type-agnostic for these methods.
#[must_use]
pub fn desugared_list_method<'a>(
    callee: &'a AIRNode,
    args: &'a [AirArg],
) -> Option<(&'a AIRNode, &'a str, &'a [AirArg])> {
    let (recv, field, rest) = desugared_self_call(callee, args)?;
    let method = field.name.as_str();
    if READ_ONLY_LIST_METHODS.contains(&method) {
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
/// bridge constructs one of these per comparison. Under the single-file run
/// model the `core.compare` enum declaration is not bundled into the entry
/// file, so each backend lowers `Ordering`/`Less`/`Equal`/`Greater` to a
/// self-contained representation (Rust's native `std::cmp::Ordering`, a tagged
/// object in JS/TS, a runtime singleton in Python/Go) — the same treatment the
/// built-in `Optional`/`Result` receive.
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
/// categories `Optional` or `Result`.
///
/// Returns the tag (`"Optional"` / `"Result"`) when the node carries a
/// `recv_kind` stamp with that exact value, else `None`. This is the
/// codegen-side reader of the checker→codegen annotation, the disambiguation
/// crux for the overloaded `unwrap`/`unwrap_or`/`map` method names.
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
// single pre-scan over the bundle maps each variant name to its enum and
// payload shape, which each backend consults to qualify constructions
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

/// Pre-scan every module in the bundle and build the [`EnumVariantRegistry`].
///
/// Walks each module's top-level `EnumDecl`s (the only place enum variants are
/// declared) and records every variant. A *pre-scan* — rather than recording
/// variants as their decls are emitted — is required because a use site may
/// precede its enum's declaration in source order (forward reference), and
/// because bundling concatenates modules so a `use`d enum's decl can live in a
/// different module than its construction site. This mirrors the Go backend's
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
/// pre-scan of every `RecordDecl`/`EnumDecl`/`ClassDecl` in the bundle.
///
/// Backends with native generic-receiver / `impl` syntax (Rust `impl<T> T<T>`,
/// Go `func (self *T[T])`, TS declaration-merged `interface T<T>`) need a
/// generic type's parameters at its method-emission site even though the AIR
/// `impl Box { ... }` block carries no generic params of its own — the `T` is
/// declared on the *record*, not the impl. This registry recovers those params
/// at the impl site. A *pre-scan* (rather than recording params as decls are
/// emitted) is required because an `impl` may precede its type's declaration in
/// source order, and because bundling concatenates modules so a `use`d type's
/// decl can live in a different module than its `impl`. Mirrors
/// [`collect_enum_variants`].
pub type GenericDeclRegistry = HashMap<String, Vec<bock_ast::GenericParam>>;

/// Pre-scan every module in the bundle and build the [`GenericDeclRegistry`].
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

/// Pre-scan every module in the bundle and build the [`TraitDeclRegistry`].
///
/// Walks each module's top-level `TraitDecl`s and records the trait's generic
/// params and the full method list. Backends use this at each `impl Trait for
/// Type` site to recover the trait's *default* methods (those carrying a body)
/// so they can be synthesized onto the implementing type — the trait interface
/// alone carries only signatures, so a type that relies on an inherited default
/// would otherwise have no such method at runtime (js/ts/go). A *pre-scan*
/// (rather than recording traits as their decls are emitted) is required because
/// an `impl` may precede its trait's declaration in source order, and because
/// bundling concatenates modules so a `use`d trait's decl can live in a
/// different module than its `impl`. Mirrors [`collect_generic_decls`].
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn reachable_modules_prunes_unused_prelude_modules() {
        // Mirrors a `bock build`: the embedded `core.*` stdlib is prepended in
        // dependency order, then the user `main`. `main` uses NOTHING, so only
        // `main` should be bundled — never the prelude-only stdlib (no
        // regression vs the pre-bundling single-file run).
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
        // module is excluded. Bundling must include the transitive `use`
        // closure (main, util, helper) but drop `unused`.
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
    fn desugared_call(method: &str, extra: Vec<AIRNode>) -> (AIRNode, Vec<AirArg>) {
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
        (callee, args)
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
            let (callee, args) = desugared_call(m, extra);
            let (recv, got_method, rest) =
                desugared_list_method(&callee, &args).expect("should match");
            assert_eq!(got_method, m);
            assert!(matches!(&recv.kind, NodeKind::Identifier { name } if name.name == "nums"));
            assert_eq!(rest.len(), n_extra);
        }
    }

    #[test]
    fn desugared_list_method_rejects_mutating_and_unknown_methods() {
        // Mutating built-ins (deferred to DQ18) and arbitrary method names are
        // NOT recognised — they fall through to each backend's generic path.
        for &m in &["push", "pop", "insert", "remove", "clear", "frobnicate"] {
            let (callee, args) = desugared_call(m, vec![]);
            assert!(
                desugared_list_method(&callee, &args).is_none(),
                "{m} should not be recognised as a read-only List method"
            );
        }
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
        let (callee, args) = desugared_call(method, extra);
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
        let (callee, args) = desugared_call("compare", vec![]);
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
        let (callee, args) = desugared_call("compare", vec![]);
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
        let (callee, args) = desugared_call("unwrap_or", vec![]);
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
        // An empty bundle still carries the built-in Optional/Result entries so
        // one mechanism describes both user and built-in ADTs (B1).
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
        // A `use`d enum in another bundled module is still registered (the
        // pre-scan walks every module, so a forward / cross-module reference
        // resolves).
        let color = enum_decl("Color", vec![enum_variant("Red", EnumVariantPayload::Unit)]);
        let lib = module_named("lib", &[], vec![color]);
        let main_m = module_named("main", &["lib"], vec![fn_decl("main")]);
        let p = std::path::Path::new("x.bock");
        let reg = collect_enum_variants(&[(&lib, p), (&main_m, p)]);
        assert_eq!(reg.get("Red").map(|i| i.enum_name.as_str()), Some("Color"));
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
}
