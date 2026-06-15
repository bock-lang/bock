//! JavaScript code generator — rule-based (Tier 2) transpilation from AIR to JS.
//!
//! Handles all capability gaps:
//! - Algebraic types → tagged objects: `{ _tag: "Variant", ...fields }`
//! - Pattern matching → `switch` on `_tag` + destructuring
//! - Effects → destructured parameter object
//! - Ownership → erased (JS is GC)
//! - Async → `async`/`await` (native)
//! - Generics → erased (JS is dynamically typed)

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::PathBuf;

use bock_air::{AIRNode, AirInterpolationPart, EnumVariantPayload, NodeKind, ResultVariant};
use bock_ast::{AssignOp, BinOp, Literal, UnaryOp, Visibility};
use bock_errors::Span;
use bock_types::AIRModule;

use crate::error::CodegenError;
use crate::generator::{CodeGenerator, GeneratedCode, OutputFile, SourceMap, SourceMapping};
use crate::profile::TargetProfile;

/// Runtime helpers injected at the top of any module that references
/// `Channel.new`, `spawn`, or calls `.send` / `.recv` on a channel. Kept in
/// an IIFE so the symbols are globally reachable without name mangling.
const CONCURRENCY_RUNTIME_JS: &str = "\
// ── Bock concurrency runtime ──
const __bockChannelNew = () => {
  const queue = [];
  const waiters = [];
  const ch = {
    send(v) {
      if (waiters.length > 0) { waiters.shift()(v); } else { queue.push(v); }
    },
    recv() {
      return new Promise((resolve) => {
        if (queue.length > 0) { resolve(queue.shift()); }
        else { waiters.push(resolve); }
      });
    },
    close() {}
  };
  return [ch, ch];
};
const __bockSpawn = (x) => x;
";

/// Runtime helpers for Bock range expressions (`0..n` / `0..=n`), injected at
/// the top of any module that uses a `Range`. JS has no native range value, so
/// `for i in 0..n` lowers to `for (const i of range(0, n))`; these define
/// `range`/`rangeInclusive` as eager `Array` builders matching Bock's
/// half-open (`range`) and inclusive (`rangeInclusive`) bound semantics — the
/// same semantics Python's `range(lo, hi)` / `range(lo, hi + 1)` and Rust's
/// `lo..hi` / `lo..=hi` produce. Emitted once into the shared `_bock_runtime.js`
/// (per-module path) or inlined at most once (single-module path), gated on a
/// ctx flag (mirrors the concurrency runtime).
const RANGE_RUNTIME_JS: &str = "\
// ── Bock range runtime ──
const range = (lo, hi) => { const r = []; for (let i = lo; i < hi; i++) r.push(i); return r; };
const rangeInclusive = (lo, hi) => { const r = []; for (let i = lo; i <= hi; i++) r.push(i); return r; };
";

/// True if the module references a `Range` node anywhere (so the range runtime
/// prelude must be emitted). A cheap structural scan over the debug rendering,
/// mirroring [`EmitCtx::module_uses_concurrency`]. `RangePat` (a match-arm range
/// pattern) does not contain the `Range {` substring, so it is not matched —
/// the helpers are only needed for range *values*.
fn js_module_uses_range(items: &[AIRNode]) -> bool {
    items.iter().any(|n| format!("{n:?}").contains("Range {"))
}

/// Runtime helper for DQ29 structural equality (`==`/`!=` on records, enums,
/// tuples, `List`/`Map`/`Set`, `Optional`/`Result`, and bounded generics): JS
/// `===` on two objects is reference identity, so every stamped equality (see
/// [`crate::generator::user_eq_kind`], lanes `"structural"`/`"deep"`/
/// `"deep_custom"`/`"generic"`) lowers to `__bockEq(a, b)` instead.
///
/// The `"deep_custom"` lane (DQ31: a container whose element tree carries a
/// custom `impl Equatable`) needs no special handling here — `__bockEq` already
/// dispatches through an element's `eq` method when present (the `a.eq` check
/// below), so the custom element equality is honored inside the collection,
/// including `Map` key-matching and `Set` membership.
///
/// Semantics:
/// - non-objects fall through to `===` — which keeps the IEEE `NaN !== NaN`
///   Float behavior (the DQ10 caveat) and native string/number/boolean
///   equality;
/// - a value carrying an `eq` method (an explicit `impl Equatable` attached to
///   the prototype) dispatches through it, so custom equality is honored even
///   for elements *inside* collections;
/// - arrays (Bock `List` and tuples) compare element-wise;
/// - `Map`/`Set` compare by content, ORDER-INDEPENDENTLY, with a fast path via
///   native key lookup (primitive keys) and a deep-equality scan fallback;
/// - everything else (records, tagged enum/Optional/Result objects) compares
///   by own enumerable properties, which includes the `_tag` discriminant.
///
/// Emitted once into the shared `_bock_runtime.js` (per-module path) or
/// inlined at most once (single-module path), gated on a ctx flag (mirrors the
/// range runtime).
const EQ_RUNTIME_JS: &str = "\
// ── Bock structural equality runtime ──
const __bockEq = (a, b) => {
  if (a === b) return true;
  if (typeof a !== \"object\" || typeof b !== \"object\" || a === null || b === null) {
    return a === b;
  }
  if (typeof a.eq === \"function\" && typeof b.eq === \"function\") return a.eq(a, b);
  if (Array.isArray(a)) {
    if (!Array.isArray(b) || a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) { if (!__bockEq(a[i], b[i])) return false; }
    return true;
  }
  if (a instanceof Map) {
    if (!(b instanceof Map) || a.size !== b.size) return false;
    for (const [k, v] of a) {
      if (b.has(k)) { if (!__bockEq(b.get(k), v)) return false; continue; }
      let found = false;
      for (const [bk, bv] of b) { if (__bockEq(k, bk) && __bockEq(v, bv)) { found = true; break; } }
      if (!found) return false;
    }
    return true;
  }
  if (a instanceof Set) {
    if (!(b instanceof Set) || a.size !== b.size) return false;
    for (const x of a) {
      if (b.has(x)) continue;
      let found = false;
      for (const y of b) { if (__bockEq(x, y)) { found = true; break; } }
      if (!found) return false;
    }
    return true;
  }
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  for (const k of ka) { if (!__bockEq(a[k], b[k])) return false; }
  return true;
};
";

/// True if the module contains an equality that must lower through
/// [`EQ_RUNTIME_JS`]'s `__bockEq`: a `BinaryOp` the checker stamped with a
/// non-`"impl"` [`bock_types::checker::USER_EQ_META_KEY`] lane, or an
/// `a.eq(b)` bridge call on an `Equatable`-bounded generic receiver (whose
/// instantiation may be a record). Cheap debug-rendering scan, mirroring
/// [`js_module_uses_range`]. The `"impl"` lane dispatches through the type's
/// own `eq` method and needs no helper.
fn js_module_uses_eq(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let dbg = format!("{n:?}");
        dbg.contains("\"user_eq\": String(\"structural\")")
            || dbg.contains("\"user_eq\": String(\"deep\")")
            || dbg.contains("\"user_eq\": String(\"deep_custom\")")
            || dbg.contains("\"user_eq\": String(\"generic\")")
            || dbg.contains("TraitBound:Equatable")
    })
}

/// The shared per-module runtime module name (without extension). In the
/// per-module (native-import) emission path the concurrency and range runtime
/// helpers live in one file — `_bock_runtime.js` at the build root — and every
/// emitted module imports the named helpers it references (`__bockChannelNew`,
/// `range`, …). A single shared definition avoids redeclaring `const
/// __bockChannelNew` / `const range` across files (a duplicate top-level
/// `const` is a redeclaration error in an ES module).
const RUNTIME_MODULE_JS: &str = "_bock_runtime";

/// JavaScript code generator implementing the `CodeGenerator` trait.
#[derive(Debug)]
pub struct JsGenerator {
    profile: TargetProfile,
}

impl JsGenerator {
    /// Creates a new JavaScript code generator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: TargetProfile::javascript(),
        }
    }
}

impl Default for JsGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeGenerator for JsGenerator {
    fn target(&self) -> &TargetProfile {
        &self.profile
    }

    fn generate_module(&self, module: &AIRModule) -> Result<GeneratedCode, CodegenError> {
        // Shared pre-pass: hoist value-position diverging control flow into
        // declare-then-assign temp blocks so the diverging arms emit as
        // statements rather than `/* unsupported */`.
        let module =
            &crate::generator::hoist_value_cf(crate::generator::lower_blanket_into(module.clone()));
        let mut ctx = EmitCtx::new();
        ctx.enum_variants =
            crate::generator::collect_enum_variants(&[(module, std::path::Path::new(""))]);
        ctx.trait_decls =
            crate::generator::collect_trait_decls(&[(module, std::path::Path::new(""))]);
        ctx.class_fields =
            crate::generator::collect_class_fields(&[(module, std::path::Path::new(""))]);
        ctx.const_names =
            crate::generator::collect_const_names(&[(module, std::path::Path::new(""))]);
        ctx.emit_node(module)?;
        let (content, mappings) = ctx.finish();
        let source_map = SourceMap {
            generated_file: String::new(),
            mappings,
            ..Default::default()
        };
        Ok(GeneratedCode {
            files: vec![OutputFile {
                path: PathBuf::new(),
                content,
                source_map: Some(source_map),
            }],
        })
    }

    fn entry_invocation(&self, main_is_async: bool) -> Option<String> {
        if main_is_async {
            // Wrap in an async IIFE so top-level await isn't required — keeps
            // the generated script runnable as both an ES module and a script.
            Some("(async () => { await main(); })();\n".to_string())
        } else {
            Some("main();\n".to_string())
        }
    }

    /// Emit a per-module **native ES-module import tree** (spec §20.6.1; DQ19
    /// resolved): each module the entry program reaches through a real `use` is
    /// emitted to its **own** `.js` file, and cross-module references resolve
    /// through real ESM `import { … } from "./…"`. This is the sole `bock build`
    /// output path.
    ///
    /// Output-path mapping is keyed on each module's *declared* path, not its
    /// on-disk source path, so the file layout and the import specifier agree:
    /// `module core.option` ⇒ `core/option.js` and `import … from
    /// "./core/option.js"`. The **entry** module (the one declaring `main`, else
    /// the last in dependency order) is always emitted as `main.js` so the run
    /// model is stable.
    ///
    /// To run under `node main.js`, the emitted tree is ESM (relative specifiers
    /// carry `.js`, declarations use `export`); the minimal `package.json`
    /// `{"type":"module"}` run affordance — which makes Node treat the `.js`
    /// files as ES modules — is emitted by the **scaffolder** in project mode
    /// (S6a / DV18), not by codegen, so `--source-only` output is bare source.
    /// The concurrency and range runtime
    /// helpers are emitted **once** into a shared `_bock_runtime.js`
    /// (see `RUNTIME_MODULE_JS`); every module that references one imports the
    /// helpers it needs.
    fn generate_project(
        &self,
        modules: &[(&AIRModule, &std::path::Path)],
    ) -> Result<GeneratedCode, CodegenError> {
        // Shared pre-pass: hoist value-position diverging control flow (see
        // `hoist_value_cf`) on every module before any registry collection or
        // emission, so all targets emit valid statement-form code.
        let hoisted: Vec<(AIRModule, &std::path::Path)> = modules
            .iter()
            .map(|(m, p)| {
                (
                    crate::generator::hoist_value_cf(crate::generator::lower_blanket_into(
                        (*m).clone(),
                    )),
                    *p,
                )
            })
            .collect();
        let modules: Vec<(&AIRModule, &std::path::Path)> =
            hoisted.iter().map(|(m, p)| (m, *p)).collect();
        let modules = modules.as_slice();
        // Emit only modules the entry program actually `use`s (plus the entry
        // itself), dependency-ordered — never the prelude-only stdlib.
        let reachable = crate::generator::reachable_modules(modules);
        let modules = reachable.as_slice();
        if modules.is_empty() {
            return Ok(GeneratedCode { files: vec![] });
        }

        // The entry module names `main.js`; every other module is placed at the
        // path mirrored from its declared module-path.
        let entry_idx = modules
            .iter()
            .position(|(m, _)| crate::generator::module_declares_main_fn(m))
            .unwrap_or(modules.len() - 1);

        // Registries collected across the whole reachable set so a reference in
        // one file to a type declared in another lowers identically to bundling.
        let enum_variants = crate::generator::collect_enum_variants(modules);
        let trait_decls = crate::generator::collect_trait_decls(modules);
        let record_names = crate::generator::collect_record_names(modules);
        let class_fields = crate::generator::collect_class_fields(modules);
        let const_names = crate::generator::collect_const_names(modules);
        let public_symbols = crate::generator::collect_public_symbols_for_esm(modules);
        // Program-wide field/method name-collision set (camelCased). Built across
        // *all* reachable modules so a call site in `main.js` to a renamed method
        // declared in `core/error.js` agrees with that declaration — the method
        // and its cross-module call sites must rename identically.
        let mut field_method_collisions = HashSet::new();
        for (module, _) in modules {
            field_method_collisions.extend(crate::generator::collect_record_field_names(
                module,
                to_camel_case,
            ));
        }

        let main_is_async = modules
            .iter()
            .any(|(m, _)| crate::generator::module_main_fn_is_async(m));
        let invocation = self.entry_invocation(main_is_async);

        let mut files: Vec<OutputFile> = Vec::with_capacity(modules.len() + 2);
        let mut runtime_concurrency = false;
        let mut runtime_range = false;
        let mut runtime_eq = false;

        for (i, (module, source_path)) in modules.iter().enumerate() {
            let own_path = crate::generator::module_path_string(module).unwrap_or_default();
            let mut ctx = EmitCtx::new();
            ctx.per_module = true;
            ctx.enum_variants = enum_variants.clone();
            ctx.trait_decls = trait_decls.clone();
            // Record names need the whole reachable set so a cross-module record
            // construction (`use`d from another module) lowers to `new Name(...)`
            // rather than a bare object literal that drops its prototype methods.
            ctx.record_names = record_names.clone();
            // Class field-orders need the whole reachable set too: a cross-module
            // `class` construction must lower to its positional `new Name(...)`.
            ctx.class_fields = class_fields.clone();
            ctx.const_names = const_names.clone();
            // Program-wide collision set so renamed methods and their
            // cross-module call sites agree (see above).
            ctx.field_method_collisions = field_method_collisions.clone();
            // Effect-op resolution needs the whole reachable set: a bare op in
            // one module may belong to an effect declared in another.
            ctx.seed_effect_registries(modules);
            // The entry file is always `main.js` at the root, so it imports
            // siblings as if it lived at the root regardless of its declared
            // path — pass the empty self-path for the relative-specifier base.
            ctx.self_module_path = if i == entry_idx {
                String::new()
            } else {
                own_path.clone()
            };
            ctx.implicit_imports =
                crate::generator::implicit_esm_imports_for(module, &public_symbols, &own_path);
            ctx.public_symbols = public_symbols.clone();
            ctx.export_names = crate::generator::exportable_value_names(module);
            ctx.emit_node(module)?;
            runtime_concurrency |= ctx.needs_runtime_concurrency;
            runtime_range |= ctx.needs_runtime_range;
            runtime_eq |= ctx.needs_runtime_eq;
            let (mut content, mappings) = ctx.finish();

            // The entry file gets the `main()` invocation appended exactly once.
            if i == entry_idx && crate::generator::module_declares_main_fn(module) {
                if let Some(invoc) = invocation.as_ref() {
                    if !content.is_empty() && !content.ends_with('\n') {
                        content.push('\n');
                    }
                    content.push_str(invoc);
                }
            }

            let out_path = self.module_output_path(module, source_path, i == entry_idx);
            let generated_file = out_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            files.push(OutputFile {
                path: out_path,
                content,
                source_map: Some(SourceMap {
                    generated_file,
                    mappings,
                    ..Default::default()
                }),
            });
        }

        // Shared runtime module with exactly the helpers referenced, each
        // `export`ed so consuming modules can `import { … }` them.
        if runtime_concurrency || runtime_range || runtime_eq {
            let mut content = String::new();
            if runtime_concurrency {
                content.push_str(&export_runtime_consts(CONCURRENCY_RUNTIME_JS));
                content.push('\n');
            }
            if runtime_range {
                content.push_str(&export_runtime_consts(RANGE_RUNTIME_JS));
                content.push('\n');
            }
            if runtime_eq {
                content.push_str(&export_runtime_consts(EQ_RUNTIME_JS));
                content.push('\n');
            }
            files.push(OutputFile {
                path: PathBuf::from(format!("{RUNTIME_MODULE_JS}.js")),
                content,
                source_map: Some(SourceMap {
                    generated_file: format!("{RUNTIME_MODULE_JS}.js"),
                    ..Default::default()
                }),
            });
        }

        // Run-affordance emission moved to the project-mode scaffolder (S6a /
        // DV18): codegen emits only the per-module `.js` *source* tree in all
        // modes; the `package.json` `{"type":"module"}` run affordance is
        // emitted by `JsScaffolder` in project mode only (never under
        // `--source-only`). See `scaffold.rs`.

        Ok(GeneratedCode { files })
    }

    /// Transpile `@test` functions into a Vitest/Jest `bock.test.js` file (S7).
    ///
    /// `framework` selects the idiom: `"jest"` uses Jest's global `describe`/
    /// `it`/`expect`; anything else uses Vitest (`import { describe, it, expect }
    /// from "vitest"`). Both share the `expect(actual).toEqual/toBe(...)` API, so
    /// the per-assertion lowering is identical; only the framework import differs.
    /// The functions under test are imported by name from their emitted modules.
    fn generate_tests(
        &self,
        modules: &[(&AIRModule, &std::path::Path)],
        framework: &str,
    ) -> Result<crate::generator::TestArtifacts, CodegenError> {
        crate::generator::js_ts_generate_tests(
            modules,
            framework,
            &self.target().conventions.file_extension,
            // JS imports the emitted `.js` siblings directly.
            "js",
            |module, source_path, is_entry| self.module_output_path(module, source_path, is_entry),
            |ctx_modules| {
                let enum_variants = crate::generator::collect_enum_variants(ctx_modules);
                let trait_decls = crate::generator::collect_trait_decls(ctx_modules);
                let record_names = crate::generator::collect_record_names(ctx_modules);
                let class_fields = crate::generator::collect_class_fields(ctx_modules);
                let const_names = crate::generator::collect_const_names(ctx_modules);
                let mut field_method_collisions = HashSet::new();
                for (module, _) in ctx_modules {
                    field_method_collisions.extend(crate::generator::collect_record_field_names(
                        module,
                        to_camel_case,
                    ));
                }
                let mut ctx = EmitCtx::new();
                ctx.per_module = true;
                ctx.enum_variants = enum_variants;
                ctx.trait_decls = trait_decls;
                ctx.record_names = record_names;
                ctx.class_fields = class_fields;
                ctx.const_names = const_names;
                ctx.field_method_collisions = field_method_collisions;
                ctx.seed_effect_registries(ctx_modules);
                Box::new(JsTestEmitter { ctx })
            },
        )
    }
}

/// Adapter wrapping a JS [`EmitCtx`] so the shared js/ts test-file builder
/// ([`crate::generator::js_ts_generate_tests`]) can drive expression lowering
/// without depending on the concrete (private) emit-context type.
struct JsTestEmitter {
    ctx: EmitCtx,
}

impl crate::generator::JsTsExprEmitter for JsTestEmitter {
    fn expr_to_string(&mut self, node: &AIRNode) -> Result<String, CodegenError> {
        self.ctx.expr_to_string(node)
    }
}

impl JsGenerator {
    /// Output path for one module in the per-module native-import tree.
    ///
    /// The entry module is always `main.js` (mirrored from its source path) so
    /// the run model `node main.js` is stable. Every other module is placed at
    /// the path mirrored from its **declared** module-path so the file location
    /// and the relative import specifier agree:
    /// `module core.option` ⇒ `core/option.js`. A module without a declared path
    /// falls back to its source-mirrored path.
    fn module_output_path(
        &self,
        module: &AIRModule,
        source_path: &std::path::Path,
        is_entry: bool,
    ) -> PathBuf {
        if is_entry {
            return crate::generator::derive_output_path(source_path, self.target());
        }
        match crate::generator::module_path_string(module) {
            Some(path) if !path.is_empty() => {
                let rel: PathBuf = path.split('.').collect();
                rel.with_extension(&self.target().conventions.file_extension)
            }
            _ => crate::generator::derive_output_path(source_path, self.target()),
        }
    }
}

/// Rewrite a runtime-prelude string (a sequence of top-level `const NAME = …`
/// definitions, see [`CONCURRENCY_RUNTIME_JS`] / [`RANGE_RUNTIME_JS`]) so each
/// top-level `const` becomes an `export const`. Used to build the shared
/// `_bock_runtime.js`, whose helpers consuming modules import by name. Only
/// lines that *begin* a top-level `const` (column 0) are rewritten; nested
/// `const`s inside a helper body are indented and so left untouched.
fn export_runtime_consts(runtime: &str) -> String {
    runtime
        .lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix("const ") {
                format!("export const {rest}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ─── Emission context ────────────────────────────────────────────────────────

/// Internal state for JS emission.
struct EmitCtx {
    buf: String,
    indent: usize,
    /// Maps effect operation name → effect type name (e.g., "log" → "Logger").
    effect_ops: HashMap<String, String>,
    /// Maps effect type name → current handler variable name in scope.
    current_handler_vars: HashMap<String, String>,
    /// Maps function name → effect type names from its `with` clause.
    fn_effects: HashMap<String, Vec<String>>,
    /// Maps composite effect name → component effect names.
    composite_effects: HashMap<String, Vec<String>>,
    /// Names of records declared in this module (emitted as classes).
    record_names: HashSet<String>,
    /// Names of `class` declarations mapped to their **field names in
    /// declaration order**, pre-scanned across the reachable program. A Bock
    /// `class` emits a *positional* `constructor(a, b)` (unlike a record's
    /// destructured `constructor({ a, b })`), so a `class` literal `T { a: x, b:
    /// y }` must lower to `new T(x, y)` with arguments ordered by the declared
    /// field order — not the bare object literal the record path emits (whose
    /// prototype methods would be unreachable). See
    /// [`crate::generator::collect_class_fields`].
    class_fields: HashMap<String, Vec<String>>,
    /// Declared names of module-scope `const`s, pre-scanned across the reachable
    /// program. A const identifier is emitted verbatim at both its declaration
    /// and every use so the two agree (the `to_camel_case` transform would
    /// otherwise mangle a `SCREAMING_SNAKE` use site, e.g. `FIZZ_NUM` → `fizzNUM`,
    /// against the verbatim-emitted definition). See [`crate::generator::collect_const_names`].
    const_names: HashSet<String>,
    /// 1-indexed current line in `buf`, maintained incrementally.
    cur_line: u32,
    /// 1-indexed current column (char count) in `buf`, maintained incrementally.
    cur_col: u32,
    /// Byte offset in `buf` up to which (cur_line, cur_col) is accurate.
    scan_pos: usize,
    /// Last (gen_line, gen_col) we recorded — avoids recording duplicates
    /// when multiple nested nodes share the same output position.
    last_marked: Option<(u32, u32)>,
    /// Collected source-map entries (populated via [`Self::mark_span`]).
    mappings: Vec<SourceMapping>,
    /// Loop-label stack — see [`crate::generator::loop_needs_break_label`]. In
    /// JS, `break` inside a `switch` exits the switch, so a statement-arm
    /// `match` (lowered to a `switch`) that wants to `break`/`continue` an
    /// enclosing loop must use a labelled jump. `Some` once a label is
    /// allocated for a loop; only allocated labels are emitted.
    loop_labels: Vec<Option<String>>,
    /// Depth of statement-arm `switch` emission; when > 0, `break`/`continue`
    /// target the innermost labelled loop rather than the switch.
    switch_label_depth: usize,
    /// Monotonic counter for unique loop-label names.
    loop_label_counter: usize,
    /// Monotonic counter for unique `match` scrutinee temporaries. A non-trivial
    /// scrutinee (a call, etc.) is hoisted into `const __matchN = <scrutinee>;`
    /// once, so it is evaluated a single time. Re-emitting the scrutinee inline
    /// in every arm (the prior behavior) double-evaluated it — a real bug for a
    /// scrutinee with side effects, e.g. a stateful iterator's `match next(it)`.
    match_temp_counter: usize,
    /// Set once the concurrency runtime prelude has been emitted in the
    /// single-module self-contained path ([`JsGenerator::generate_module`]), so
    /// a module that references the runtime more than once still inlines it at
    /// most once. (The per-module project path emits the runtime once into the
    /// shared `_bock_runtime.js` instead.)
    concurrency_runtime_emitted: bool,
    /// Set once the range runtime prelude ([`RANGE_RUNTIME_JS`]) has been
    /// emitted in the single-module self-contained path, so the
    /// `range`/`rangeInclusive` helpers inline at most once (a duplicate `const
    /// range` would be a redeclaration error). Deduped exactly as
    /// [`Self::concurrency_runtime_emitted`].
    range_runtime_emitted: bool,
    /// Set once the structural-equality runtime ([`EQ_RUNTIME_JS`]) has been
    /// emitted in the single-module self-contained path. Deduped exactly as
    /// [`Self::range_runtime_emitted`].
    eq_runtime_emitted: bool,
    /// User-enum-variant registry (DV14). Maps a variant name to its enum so a
    /// unit-variant reference lowers to the frozen `{enum}_{variant}` const, a
    /// struct/tuple construction lowers to the `{enum}_{variant}(..)` factory,
    /// and a `match` recognises struct-payload (`RecordPat`) arms as ADT
    /// variants. Pre-scanned across the reached modules. The built-in
    /// Optional/Result pre-seeds are filtered out where bespoke lowering applies.
    enum_variants: crate::generator::EnumVariantRegistry,
    /// Trait-declaration registry. Used at each `impl Trait for Type` site to
    /// recover the trait's *default* methods (those carrying a body) so they can
    /// be attached to the target's prototype alongside the impl's own methods —
    /// a type relying on an inherited default would otherwise have no such
    /// method. Pre-scanned across the reached modules (mirrors
    /// [`Self::enum_variants`]).
    trait_decls: crate::generator::TraitDeclRegistry,
    /// True in the **per-module native-import** emission path
    /// ([`JsGenerator::generate_project`], the sole real-build path). When set,
    /// the `Module` arm emits real ESM `import { … } from "./…"` for
    /// cross-module references, records which shared-runtime helpers the module
    /// needs (instead of inlining them), and a trailing `export { … }`
    /// re-exports the module's public non-function declarations (functions
    /// export inline). When clear, the module is emitted as a single
    /// self-contained file with its runtime preludes inlined — the
    /// [`JsGenerator::generate_module`] path used by unit tests.
    per_module: bool,
    /// In the per-module path, records that this module references the
    /// concurrency runtime (`Channel`/`spawn`) — so `generate_project` emits the
    /// concurrency helpers into the shared `_bock_runtime.js` and this module
    /// imports the names it needs from it (rather than inlining the prelude,
    /// which would redeclare `const __bockChannelNew` across files).
    needs_runtime_concurrency: bool,
    /// As [`Self::needs_runtime_concurrency`], for the range runtime
    /// (`range`/`rangeInclusive`).
    needs_runtime_range: bool,
    /// As [`Self::needs_runtime_concurrency`], for the DQ29 structural-equality
    /// runtime (`__bockEq`).
    needs_runtime_eq: bool,
    /// Implicit cross-module imports for the per-module path — names this module
    /// references but neither declares locally nor imports via an explicit `use`
    /// (e.g. a §18.2-prelude trait used as a base in an `impl`). Computed in
    /// `generate_project`; emitted as ESM imports by the `Module` arm.
    implicit_imports: Vec<crate::generator::ImplicitEsmImport>,
    /// Map of every reachable public symbol → its declaring module + kind, used
    /// to spell an explicit `use`d name the way its declaration emits it (a
    /// function is camelCased; other kinds keep their raw name).
    public_symbols: HashMap<String, crate::generator::EsmSymbol>,
    /// The declared dotted module-path of the file currently being emitted
    /// (`core.option`), or empty for the entry file (always `main.js` at the
    /// build root). Drives the relative-specifier computation for every emitted
    /// `import`.
    self_module_path: String,
    /// Public, exportable value declarations this module declares — listed in
    /// the trailing `export { … }` of the per-module file. Functions are skipped
    /// there (they export inline via `emit_fn_decl`); every other kind (records,
    /// enums + variants, traits, classes, effects, consts) is re-exported.
    /// Computed in `generate_project`.
    export_names: Vec<crate::generator::EsmExport>,
    /// Camel-cased record/class field names in the module being emitted, used to
    /// disambiguate a method whose camelCased name collides with a field name
    /// (`core.error`'s `message` field + `message()` method). A JS instance
    /// field shadows a same-named prototype method, so the *method* is renamed
    /// (`messageMethod`) at its prototype attachment, its class-body emission,
    /// and every call site via [`Self::js_method_name`]; the field keeps its
    /// name. Populated at the start of the `Module` arm (shared collector with
    /// go/ts/py).
    field_method_collisions: HashSet<String>,
    /// Per-JS-lexical-block stack of simple `let`/`const` binding state. Bock
    /// permits re-binding (`let x = …; let x = …`) which shadows the prior
    /// binding in the same scope; JS `const`/`let` forbid re-declaration in one
    /// block scope. Each frame records the JS idents already declared in the
    /// block and, of those, which need `let` (because they are re-bound or
    /// assigned later). The first declaration of a re-bound name uses `let`;
    /// every subsequent binding of the same name emits a plain assignment
    /// (`x = …`) rather than a redeclaration. See [`LetScope`].
    let_scopes: Vec<LetScope>,
    /// Monotonic counter for unique `?`-propagation temporaries (`__try0`,
    /// `__try1`, …). See [`EmitCtx::hoist_propagates`].
    propagate_temp_counter: usize,
    /// Maps a `Propagate` node (keyed by its `&AIRNode` address) to the JS temp
    /// that [`EmitCtx::hoist_propagates`] bound its unwrapped payload into. The
    /// `Propagate` arm of [`EmitCtx::emit_expr`] reads this to emit `<temp>._0`
    /// in place of the wrapped value. The address is stable across the
    /// pre-statement hoist walk and the subsequent expression emission because
    /// both traverse the *same* borrowed AIR node tree.
    propagate_temps: HashMap<usize, String>,
    /// When set, a block-body tail expression is **discarded** — emitted as a
    /// bare expression statement (`<value>;`) rather than `return <value>;`. A
    /// loop body (`for`/`while`/`loop`) and a statement-position `if`/`match`
    /// arm are statement context: their tail is not the enclosing function's
    /// value, so `return`ing it would abort the function on the first iteration
    /// (e.g. `for (…) { return console.log(i); }` exits `main` after one line).
    /// Set for the duration of a loop body via [`EmitCtx::emit_loop_body`] and
    /// for a block's non-tail statements in [`EmitCtx::emit_block_body_inner`];
    /// cleared (saved/restored) when entering a genuine value context — a
    /// lambda body, a value-position block/`match` IIFE — so their tail still
    /// `return`s the body's value. See the TS backend's `ValueSink::Discard`
    /// (#240) for the mirror of this design.
    discard_tail: bool,
}

/// One JS lexical block's `let`/`const` binding state — see
/// [`EmitCtx::let_scopes`].
#[derive(Default)]
struct LetScope {
    /// Simple JS idents already emitted as a declaration in this block.
    declared: HashSet<String>,
    /// Of the block's simple bindings, those that are re-bound or assigned and
    /// so must be declared with `let` (not `const`) at their first declaration.
    needs_let: HashSet<String>,
}

impl EmitCtx {
    fn new() -> Self {
        Self {
            buf: String::with_capacity(4096),
            indent: 0,
            effect_ops: HashMap::new(),
            current_handler_vars: HashMap::new(),
            fn_effects: HashMap::new(),
            composite_effects: HashMap::new(),
            record_names: HashSet::new(),
            class_fields: HashMap::new(),
            const_names: HashSet::new(),
            cur_line: 1,
            cur_col: 1,
            scan_pos: 0,
            last_marked: None,
            mappings: Vec::new(),
            loop_labels: Vec::new(),
            switch_label_depth: 0,
            loop_label_counter: 0,
            match_temp_counter: 0,
            concurrency_runtime_emitted: false,
            range_runtime_emitted: false,
            eq_runtime_emitted: false,
            enum_variants: crate::generator::EnumVariantRegistry::new(),
            trait_decls: crate::generator::TraitDeclRegistry::new(),
            per_module: false,
            needs_runtime_concurrency: false,
            needs_runtime_range: false,
            needs_runtime_eq: false,
            implicit_imports: Vec::new(),
            public_symbols: HashMap::new(),
            self_module_path: String::new(),
            export_names: Vec::new(),
            field_method_collisions: HashSet::new(),
            let_scopes: Vec::new(),
            propagate_temp_counter: 0,
            propagate_temps: HashMap::new(),
            discard_tail: false,
        }
    }

    /// Disambiguate an *already-rendered* JS method member name against the
    /// program's field names: when the rendered name collides with a field name,
    /// append a `Method` suffix (`message` → `messageMethod`), else return it
    /// unchanged.
    ///
    /// JS renders method names two ways — the prototype attachment and
    /// `FieldAccess`-in-call-position use the raw Bock name, while
    /// `emit_class_method` and the `MethodCall` arm use `to_camel_case` — so this
    /// helper takes whatever each site already produced and only adds the suffix,
    /// preserving each site's existing casing (a switch to camelCase would change
    /// snake_case method names, breaking call sites that still spell them raw).
    /// For the field/method collisions this targets (single-word field names like
    /// `message`, where raw == camelCase) the renderings agree. Shared policy
    /// with go/ts/py (see [`crate::generator::disambiguate_method_name`]).
    fn js_method_name(&self, rendered: &str) -> String {
        crate::generator::disambiguate_method_name(
            rendered.to_string(),
            &self.field_method_collisions,
            "Method",
        )
    }

    /// Returns the variant info for `path` if its last segment is a registered
    /// user enum variant. The built-in `Optional`/`Result` pre-seeds
    /// (`Some`/`None`/`Ok`/`Err`) are excluded — those are lowered by the
    /// bespoke tagged-object paths (`try_emit_prelude_ctor`, the `None`
    /// identifier special case, `ResultConstruct`), which must not change.
    fn user_variant_for_path(
        &self,
        path: &bock_ast::TypePath,
    ) -> Option<&crate::generator::EnumVariantInfo> {
        let info = crate::generator::registered_variant(&self.enum_variants, path)?;
        if matches!(info.enum_name.as_str(), "Optional" | "Result") {
            return None;
        }
        Some(info)
    }

    /// As [`Self::user_variant_for_path`] but keyed by a bare identifier name.
    fn user_variant_for_name(&self, name: &str) -> Option<&crate::generator::EnumVariantInfo> {
        let info = self.enum_variants.get(name)?;
        if matches!(info.enum_name.as_str(), "Optional" | "Result") {
            return None;
        }
        Some(info)
    }

    fn finish(self) -> (String, Vec<SourceMapping>) {
        (self.buf, self.mappings)
    }

    /// Pre-seed the effect registries (`effect_ops`, `composite_effects`) from
    /// every module's top-level `EffectDecl`s. In the per-module path each
    /// module is emitted by its own context, so a bare op `log(...)` used in
    /// `main` whose effect `Log` is declared in another module would not be
    /// recognised as an effect op (and not rewritten to `logger.log(...)`)
    /// without pre-seeding from the whole reachable set. Mirrors how
    /// `enum_variants` / `trait_decls` are collected across the reached modules,
    /// and the Python backend's `PyEmitCtx::seed_effect_registries`.
    fn seed_effect_registries(&mut self, modules: &[(&AIRModule, &std::path::Path)]) {
        for (module, _) in modules {
            let NodeKind::Module { items, .. } = &module.kind else {
                continue;
            };
            for item in items {
                let NodeKind::EffectDecl {
                    name,
                    components,
                    operations,
                    ..
                } = &item.kind
                else {
                    continue;
                };
                if !components.is_empty() {
                    let comp_names: Vec<String> = components
                        .iter()
                        .map(|tp| {
                            tp.segments
                                .last()
                                .map_or("effect".to_string(), |s| s.name.clone())
                        })
                        .collect();
                    self.composite_effects.insert(name.name.clone(), comp_names);
                    continue;
                }
                for op in operations {
                    if let NodeKind::FnDecl { name: op_name, .. } = &op.kind {
                        self.effect_ops
                            .insert(op_name.name.clone(), name.name.clone());
                    }
                }
            }
        }
    }

    /// Emit the per-module ESM `import` statements at the top of the file: the
    /// shared-runtime import (concurrency / range helpers), the explicit
    /// cross-module `use` imports, and the implicit prelude imports (e.g. a base
    /// trait used in an `impl`). Grouped one `import { … } from "./…"` per
    /// source module, with the relative specifier computed from this file's
    /// declared module-path. Called once at the top of the `Module` arm in the
    /// per-module path.
    fn emit_esm_imports(&mut self, imports: &[AIRNode]) -> Result<(), CodegenError> {
        // The JS backend always emits `.js` files; the relative specifier
        // carries that extension (ESM + Node require an explicit extension).
        let ext = "js";

        // Shared runtime: `import { … } from "./…/_bock_runtime.js"`.
        if self.needs_runtime_concurrency || self.needs_runtime_range || self.needs_runtime_eq {
            let mut names: Vec<&str> = Vec::new();
            if self.needs_runtime_concurrency {
                names.extend(["__bockChannelNew", "__bockSpawn"]);
            }
            if self.needs_runtime_range {
                names.extend(["range", "rangeInclusive"]);
            }
            if self.needs_runtime_eq {
                names.push("__bockEq");
            }
            let spec = crate::generator::esm_relative_specifier(
                &self.self_module_path,
                RUNTIME_MODULE_JS,
                ext,
            );
            self.writeln(&format!(
                "import {{ {} }} from \"{spec}\";",
                names.join(", ")
            ));
        }

        // Explicit `use` imports → real ESM imports. A `use`d *function* is
        // imported under its camelCased name (matching its `export function`
        // form); other kinds keep their raw name. An explicit rename (`as`)
        // applies the same transform to the alias.
        for import in imports {
            if let NodeKind::ImportDecl { path, items } = &import.kind {
                let target_path = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                if target_path.is_empty() {
                    continue;
                }
                let spec = crate::generator::esm_relative_specifier(
                    &self.self_module_path,
                    &target_path,
                    ext,
                );
                match items {
                    bock_ast::ImportItems::Named(named) => {
                        let rendered: Vec<String> = named
                            .iter()
                            // A `use`d runtime-prelude *value* name (`Optional`,
                            // `Some`, …) lowers inline and is never a real export;
                            // a type-only symbol (an enum *type* name, a type
                            // alias) has no JS binding. Neither may appear in a
                            // real JS import.
                            .filter(|n| {
                                if crate::generator::ESM_RUNTIME_PRELUDE_NAMES
                                    .contains(&n.name.name.as_str())
                                {
                                    return false;
                                }
                                match self.public_symbols.get(&n.name.name) {
                                    Some(s) => s.kind.is_js_value(),
                                    // Unknown symbol: keep it (conservative — a
                                    // genuinely-missing import surfaces loudly).
                                    None => true,
                                }
                            })
                            .map(|n| {
                                let is_fn = self
                                    .public_symbols
                                    .get(&n.name.name)
                                    .map(|s| s.is_fn())
                                    .unwrap_or(false);
                                let src = self.esm_emit_name(&n.name.name, is_fn);
                                match &n.alias {
                                    Some(alias) => {
                                        let dst = self.esm_emit_name(&alias.name, is_fn);
                                        if dst == src {
                                            src
                                        } else {
                                            format!("{src} as {dst}")
                                        }
                                    }
                                    None => src,
                                }
                            })
                            .collect();
                        if !rendered.is_empty() {
                            self.writeln(&format!(
                                "import {{ {} }} from \"{spec}\";",
                                rendered.join(", ")
                            ));
                        }
                    }
                    // `use Foo` / `use Foo.*` — Bock brings the module's exported
                    // names into scope unqualified. ESM has no namespace-flatten
                    // import; the consuming references are resolved as implicit
                    // imports (below) by name, so a module/glob import needs no
                    // statement here.
                    bock_ast::ImportItems::Module | bock_ast::ImportItems::Glob => {}
                }
            }
        }

        // Implicit imports: prelude-visible names referenced but not explicitly
        // `use`d (e.g. a base trait), grouped per declaring module, deterministic.
        // Type-only kinds (an enum *type* name, a type alias) have no JS binding,
        // so they are skipped here — a JS reference to them is erased.
        let mut by_module: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for imp in &self.implicit_imports {
            if !imp.kind.is_js_value() {
                continue;
            }
            by_module
                .entry(imp.module_path.clone())
                .or_default()
                .push(esm_emit_name_static(&imp.name, imp.is_fn()));
        }
        for (module_path, mut names) in by_module {
            names.sort_unstable();
            names.dedup();
            let spec =
                crate::generator::esm_relative_specifier(&self.self_module_path, &module_path, ext);
            self.writeln(&format!(
                "import {{ {} }} from \"{spec}\";",
                names.join(", ")
            ));
        }
        Ok(())
    }

    /// Spell `name` the way the JS backend emits its declaration / call sites: a
    /// function is camelCased (and keyword-escaped) via [`js_value_ident`]; any
    /// other declaration kind keeps its raw name. Used so an `import`/`export`
    /// statement binds exactly the identifier the code references.
    fn esm_emit_name(&self, name: &str, is_fn: bool) -> String {
        esm_emit_name_static(name, is_fn)
    }

    /// Emit the trailing `export { … }` for this module's public **non-function**
    /// declarations (records, enums + variants, traits, classes, effects,
    /// consts). Functions export inline via [`Self::emit_fn_decl`] and so are
    /// skipped here; type aliases are erased in JS. Emits nothing when there is
    /// nothing to re-export.
    fn emit_trailing_exports(&mut self) {
        let mut names: Vec<String> = self
            .export_names
            .iter()
            .filter(|e| !e.is_fn)
            .map(|e| esm_emit_name_static(&e.name, e.is_fn))
            .collect();
        if names.is_empty() {
            return;
        }
        names.sort_unstable();
        names.dedup();
        if !self.buf.is_empty() && !self.buf.ends_with('\n') {
            self.buf.push('\n');
        }
        self.writeln(&format!("export {{ {} }};", names.join(", ")));
    }

    /// Bring `cur_line` / `cur_col` up to date with everything appended to
    /// `buf` since the last sync.
    fn sync_pos(&mut self) {
        if self.scan_pos >= self.buf.len() {
            return;
        }
        let slice = &self.buf[self.scan_pos..];
        for ch in slice.chars() {
            if ch == '\n' {
                self.cur_line += 1;
                self.cur_col = 1;
            } else {
                self.cur_col += 1;
            }
        }
        self.scan_pos = self.buf.len();
    }

    /// Record a mapping from the current generated position to the start of
    /// `span`. Dedupes consecutive recordings at the same output position.
    fn mark_span(&mut self, span: Span) {
        if span.start == 0 && span.end == 0 {
            return;
        }
        self.sync_pos();
        let key = (self.cur_line, self.cur_col);
        if self.last_marked == Some(key) {
            return;
        }
        self.last_marked = Some(key);
        self.mappings.push(SourceMapping {
            gen_line: self.cur_line,
            gen_col: self.cur_col,
            src_line: 0,
            src_col: 0,
            src_offset: span.start as u32,
            src_file_id: span.file.0,
        });
    }

    fn indent_str(&self) -> String {
        "  ".repeat(self.indent)
    }

    fn write_indent(&mut self) {
        let indent = self.indent_str();
        self.buf.push_str(&indent);
    }

    fn writeln(&mut self, s: &str) {
        self.write_indent();
        self.buf.push_str(s);
        self.buf.push('\n');
    }

    // ── Prelude function mapping ──────────────────────────────────────────

    /// Emit an expression into a temporary buffer and return the string.
    fn expr_to_string(&mut self, node: &AIRNode) -> Result<String, CodegenError> {
        let start = self.buf.len();
        // Snapshot source-map state so mappings recorded during the scratch
        // emission (which will be truncated and possibly re-emitted elsewhere)
        // don't leak into the final output.
        let saved_line = self.cur_line;
        let saved_col = self.cur_col;
        let saved_scan = self.scan_pos;
        let saved_marked = self.last_marked;
        let mappings_len = self.mappings.len();
        self.emit_expr(node)?;
        let s = self.buf[start..].to_string();
        self.buf.truncate(start);
        self.cur_line = saved_line;
        self.cur_col = saved_col;
        self.scan_pos = saved_scan;
        self.last_marked = saved_marked;
        self.mappings.truncate(mappings_len);
        Ok(s)
    }

    /// Map Bock prelude functions to JS equivalents.
    /// Returns `Some(code)` if the call is a prelude function, `None` otherwise.
    fn map_prelude_call(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<Option<String>, CodegenError> {
        let name = match &callee.kind {
            NodeKind::Identifier { name } => name.name.as_str(),
            _ => return Ok(None),
        };
        let arg_strs: Vec<String> = args
            .iter()
            .map(|a| self.expr_to_string(&a.value))
            .collect::<Result<_, _>>()?;
        let code = match name {
            "println" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("console.log({a})")
            }
            "print" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("process.stdout.write(String({a}))")
            }
            "debug" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("console.debug({a})")
            }
            "assert" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("if (!{a}) throw new Error(\"assertion failed\")")
            }
            "todo" => "throw new Error(\"not implemented\")".to_string(),
            "unreachable" => "throw new Error(\"unreachable\")".to_string(),
            "sleep" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                // Route through an installed `Clock` handler if one is in scope;
                // otherwise fall through to the host primitive (default).
                if let Some(handler) = self.clock_handler_var() {
                    format!("{handler}.sleep({a})")
                } else {
                    // Duration is ns → setTimeout takes ms.
                    format!("new Promise((__r) => setTimeout(__r, Math.floor(({a}) / 1e6)))")
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(code))
    }

    /// Decide whether to inject the concurrency runtime prelude. For
    /// simplicity we scan the serialized AIR for `Channel` / `spawn`
    /// references — a false positive just adds a few dozen bytes of dead
    /// helper code, which JS runtimes elide at GC.
    fn module_uses_concurrency(&self, items: &[AIRNode]) -> bool {
        items.iter().any(Self::node_uses_concurrency)
    }

    fn node_uses_concurrency(node: &AIRNode) -> bool {
        let serialized = format!("{node:?}");
        serialized.contains("\"Channel\"") || serialized.contains("\"spawn\"")
    }

    /// Recognise `Channel.new()`, `spawn(...)`, and `ch.send(...)` /
    /// `ch.recv()` as desugared method calls. Emits the runtime-helper
    /// form when matched.
    fn try_emit_concurrency_call(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        // Global spawn(...)
        if let NodeKind::Identifier { name } = &callee.kind {
            if name.name == "spawn" {
                self.buf.push_str("__bockSpawn(");
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(&arg.value)?;
                }
                self.buf.push(')');
                return Ok(true);
            }
        }
        let NodeKind::FieldAccess { object, field } = &callee.kind else {
            return Ok(false);
        };
        // Associated call: Channel.new()
        if let NodeKind::Identifier { name: type_name } = &object.kind {
            if type_name.name == "Channel" && field.name == "new" {
                self.buf.push_str("__bockChannelNew()");
                return Ok(true);
            }
        }
        // Desugared method call on a channel: the AIR lowerer re-inserts
        // the receiver as the first arg (`tx.send(v)` → `send(tx, v)`);
        // strip it before emitting `tx.send(v)`.
        if matches!(field.name.as_str(), "send" | "recv" | "close") {
            self.emit_expr(object)?;
            let _ = write!(self.buf, ".{}", field.name);
            self.buf.push('(');
            for (i, arg) in args.iter().skip(1).enumerate() {
                if i > 0 {
                    self.buf.push_str(", ");
                }
                self.emit_expr(&arg.value)?;
            }
            self.buf.push(')');
            return Ok(true);
        }
        Ok(false)
    }

    /// Recognise `Duration.xxx(...)` / `Instant.xxx(...)` associated-function
    /// calls and emit inline arithmetic. Durations are plain Numbers
    /// (nanoseconds); Instants are Numbers representing ns since `performance.timeOrigin`.
    ///
    /// Returns `Ok(true)` if the call was emitted.
    fn try_emit_time_assoc_call(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let NodeKind::FieldAccess { object, field } = &callee.kind else {
            return Ok(false);
        };
        let NodeKind::Identifier { name: type_name } = &object.kind else {
            return Ok(false);
        };
        let arg_strs: Vec<String> = args
            .iter()
            .map(|a| self.expr_to_string(&a.value))
            .collect::<Result<_, _>>()?;
        let arg0 = || arg_strs.first().cloned().unwrap_or_default();
        let code = match (type_name.name.as_str(), field.name.as_str()) {
            ("Duration", "zero") => "0".to_string(),
            ("Duration", "nanos") => arg0(),
            ("Duration", "micros") => format!("(({}) * 1000)", arg0()),
            ("Duration", "millis") => format!("(({}) * 1000000)", arg0()),
            ("Duration", "seconds") => format!("(({}) * 1000000000)", arg0()),
            ("Duration", "minutes") => format!("(({}) * 60000000000)", arg0()),
            ("Duration", "hours") => format!("(({}) * 3600000000000)", arg0()),
            ("Instant", "now") => {
                // Route through an installed `Clock` handler's `now_monotonic`
                // op if one is in scope; otherwise emit the host primitive.
                if let Some(handler) = self.clock_handler_var() {
                    format!("{handler}.now_monotonic()")
                } else {
                    "(performance.now() * 1000000)".to_string()
                }
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Recognise desugared method calls `Call(FieldAccess(recv, m), [recv, ...args])`
    /// on Duration/Instant values and emit inline arithmetic. Returns true if
    /// the call was emitted.
    ///
    /// `node` is the full `Call` AIR node, consulted only to *exclude* primitive
    /// receivers: [`is_time_method_name`] alone is ambiguous (`abs` is both
    /// `Duration.abs` and `Int.abs`/`Float.abs`), so when the checker has stamped
    /// `recv_kind = "Primitive:<Ty>"` this is a numeric method, not a time method —
    /// bail so [`Self::try_emit_numeric_method`] handles it.
    fn try_emit_time_desugared_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        if crate::generator::primitive_recv_kind(node).is_some() {
            return Ok(false);
        }
        let NodeKind::FieldAccess { object, field } = &callee.kind else {
            return Ok(false);
        };
        // Skip associated-fn form: `Type.method(...)`.
        if let NodeKind::Identifier { name } = &object.kind {
            if matches!(name.name.as_str(), "Duration" | "Instant") {
                return Ok(false);
            }
        }
        if !is_time_method_name(&field.name) {
            return Ok(false);
        }
        let remaining: Vec<bock_air::AirArg> = args.iter().skip(1).cloned().collect();
        self.try_emit_time_method(object, &field.name, &remaining)
    }

    /// Recognise instance methods on Duration/Instant values and emit inline
    /// arithmetic. Returns `Ok(true)` if the call was emitted.
    fn try_emit_time_method(
        &mut self,
        receiver: &AIRNode,
        method: &str,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let recv_str = self.expr_to_string(receiver)?;
        let arg_strs: Vec<String> = args
            .iter()
            .map(|a| self.expr_to_string(&a.value))
            .collect::<Result<_, _>>()?;
        let code = match method {
            "as_nanos" => format!("({recv_str})"),
            "as_millis" => format!("Math.floor(({recv_str}) / 1000000)"),
            "as_seconds" => format!("Math.floor(({recv_str}) / 1000000000)"),
            "is_zero" => format!("(({recv_str}) === 0)"),
            "is_negative" => format!("(({recv_str}) < 0)"),
            "abs" => format!("Math.abs({recv_str})"),
            "elapsed" => {
                // `instant.elapsed()` is derived: `now - instant`. Route the
                // "now" read through an installed `Clock` handler if in scope;
                // otherwise read the host monotonic clock (default).
                if let Some(handler) = self.clock_handler_var() {
                    format!("({handler}.now_monotonic() - ({recv_str}))")
                } else {
                    format!("((performance.now() * 1000000) - ({recv_str}))")
                }
            }
            "duration_since" => {
                let other = arg_strs.first().cloned().unwrap_or_default();
                format!("(({recv_str}) - ({other}))")
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Emit Some/Ok/Err calls as tagged-object constructions, matching
    /// the representation user-defined enum variants use. Returns true if
    /// the call was handled.
    fn try_emit_prelude_ctor(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let name = match &callee.kind {
            NodeKind::Identifier { name } => name.name.as_str(),
            _ => return Ok(false),
        };
        if !matches!(name, "Some" | "Ok" | "Err") {
            return Ok(false);
        }
        let _ = write!(self.buf, "{{ _tag: \"{name}\"");
        if let Some(arg) = args.first() {
            self.buf.push_str(", _0: ");
            self.emit_expr(&arg.value)?;
        }
        self.buf.push_str(" }");
        Ok(true)
    }

    /// Q-prim-assoc: lower a primitive associated-conversion call
    /// (`Float.from(x)` / `Int.try_from(s)` / `String.from(c)`) to JS's native
    /// conversion. `from` is an infallible value coercion; `try_from` parses a
    /// `String` and returns the Bock `Result` tagged-object shape
    /// (`{ _tag: "Ok"/"Err", _0: … }`), the `Err` payload a `ConvertError`
    /// (in scope via the `Result[T, ConvertError]` return-type import). Returns
    /// `true` when handled.
    fn try_emit_primitive_conversion(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((target, method, arg)) =
            crate::generator::primitive_conversion_call(node, callee, args)
        else {
            return Ok(false);
        };
        let arg_str = self.expr_to_string(arg)?;
        let code = match (target, method) {
            // `from`: infallible coercion. Int/sized-int -> Float and
            // sized-int -> Int are identity on a JS `number`; Char -> String is
            // already a single-char string, kept parenthesised for safety.
            ("Float" | "Int" | "String", "from") => format!("({arg_str})"),
            // `Int.try_from(s)`: strict integer parse. Reject anything that is
            // not an optionally-signed run of digits (JS `parseInt` is lenient).
            ("Int", "try_from") => format!(
                "((__s) => /^[+-]?[0-9]+$/.test(__s.trim()) \
                 ? {{ _tag: \"Ok\", _0: Number.parseInt(__s.trim(), 10) }} \
                 : {{ _tag: \"Err\", _0: new ConvertError({{ message: \
                 `cannot parse '${{__s}}' as Int` }}) }})({arg_str})"
            ),
            // `Float.try_from(s)`: parse a float; reject empty / non-numeric
            // (`Number("")` is 0, so guard the empty/whitespace case explicitly).
            ("Float", "try_from") => format!(
                "((__s) => {{ const __t = __s.trim(); const __n = Number(__t); \
                 return (__t.length > 0 && !Number.isNaN(__n)) \
                 ? {{ _tag: \"Ok\", _0: __n }} \
                 : {{ _tag: \"Err\", _0: new ConvertError({{ message: \
                 `cannot parse '${{__s}}' as Float` }}) }}; }})({arg_str})"
            ),
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Emit a built-in `Optional`/`Result` method call to its JS form.
    ///
    /// Recognised via the checker's `recv_kind` annotation
    /// ([`crate::generator::desugared_optional_method`] /
    /// [`crate::generator::desugared_result_method`]) so the overloaded names
    /// (`unwrap`/`unwrap_or`/`map`) dispatch to the right tag test. Both types use
    /// the inline tagged-object representation (`{ _tag: "Some"/"Ok", _0: v }` /
    /// `{ _tag: "None"/"Err", _0: e }`), so the lowering is a ternary on `._tag`.
    /// The receiver is wrapped in an IIFE (`((__o) => …)(recv)`) so it is
    /// evaluated exactly once even when read several times (`map`, the default
    /// branch). Returns `true` if the call was handled.
    fn try_emit_container_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        if let Some((recv, method, rest)) =
            crate::generator::desugared_optional_method(node, callee, args)
        {
            self.emit_tagged_container_method(recv, method, rest, "Some")?;
            return Ok(true);
        }
        if let Some((recv, method, rest)) =
            crate::generator::desugared_result_method(node, callee, args)
        {
            self.emit_tagged_container_method(recv, method, rest, "Ok")?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Lower a tagged-container method on `recv` to JS. `present_tag` is the
    /// "payload-carrying" tag (`"Some"` for `Optional`, `"Ok"` for `Result`); the
    /// predicate methods (`is_some`/`is_ok` vs `is_none`/`is_err`) and the
    /// payload extraction (`unwrap`/`unwrap_or`/`map`) are expressed against it.
    fn emit_tagged_container_method(
        &mut self,
        recv: &AIRNode,
        method: &str,
        rest: &[bock_air::AirArg],
        present_tag: &str,
    ) -> Result<(), CodegenError> {
        // `is_some`/`is_ok` and `is_none`/`is_err` are pure tag tests; emit
        // inline without an IIFE (the receiver is read once).
        match method {
            "is_some" | "is_ok" => {
                self.buf.push('(');
                self.emit_expr(recv)?;
                let _ = write!(self.buf, "._tag === \"{present_tag}\")");
                return Ok(());
            }
            "is_none" | "is_err" => {
                self.buf.push('(');
                self.emit_expr(recv)?;
                let _ = write!(self.buf, "._tag !== \"{present_tag}\")");
                return Ok(());
            }
            _ => {}
        }
        // The remaining methods read the receiver more than once, so bind it in
        // an IIFE.
        self.buf.push_str("((__c) => ");
        match method {
            "unwrap" => {
                let _ = write!(
                    self.buf,
                    "__c._tag === \"{present_tag}\" ? __c._0 : undefined"
                );
            }
            "unwrap_or" => {
                let _ = write!(self.buf, "__c._tag === \"{present_tag}\" ? __c._0 : ");
                if let Some(d) = rest.first() {
                    self.emit_expr(&d.value)?;
                } else {
                    self.buf.push_str("undefined");
                }
            }
            "map" => {
                let _ = write!(
                    self.buf,
                    "__c._tag === \"{present_tag}\" ? {{ _tag: \"{present_tag}\", _0: ("
                );
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf.push_str("(x) => x");
                }
                self.buf.push_str(")(__c._0) } : __c");
            }
            "flat_map" => {
                let _ = write!(self.buf, "__c._tag === \"{present_tag}\" ? (");
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf.push_str("(x) => x");
                }
                self.buf.push_str(")(__c._0) : __c");
            }
            "map_err" => {
                // Transform only the `Err` payload; an `Ok` passes through.
                let _ = write!(
                    self.buf,
                    "__c._tag === \"{present_tag}\" ? __c : {{ _tag: \"Err\", _0: ("
                );
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf.push_str("(x) => x");
                }
                self.buf.push_str(")(__c._0) }");
            }
            _ => {
                // Unreachable: the recogniser only admits the methods above.
                self.buf.push_str("undefined");
            }
        }
        self.buf.push_str(")(");
        self.emit_expr(recv)?;
        self.buf.push(')');
        Ok(())
    }

    /// Emit a read-only `List` built-in method call to its JS form.
    ///
    /// Recognised via [`crate::generator::desugared_list_method`] in the `Call`
    /// arm. `Optional`-returning methods (`get`/`first`/`last`/`index_of`) emit
    /// the same tagged-object representation user enum variants use
    /// (`{ _tag: "Some", _0: v }` / `{ _tag: "None" }`). Methods that need the
    /// receiver more than once (`get`/`first`/`last`/`index_of`) wrap it in an
    /// IIFE so the receiver expression is evaluated exactly once.
    fn try_emit_list_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) =
            crate::generator::desugared_list_method(node, callee, args)
        else {
            return Ok(false);
        };
        match method {
            "len" | "length" | "count" => {
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").length");
            }
            "is_empty" => {
                self.buf.push_str("((");
                self.emit_expr(recv)?;
                self.buf.push_str(").length === 0)");
            }
            "get" => {
                let Some(idx) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__r, __i) => (__i >= 0 && __i < __r.length) ? ");
                self.buf
                    .push_str("{ _tag: \"Some\", _0: __r[__i] } : { _tag: \"None\" })(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&idx.value)?;
                self.buf.push(')');
            }
            "first" => {
                self.buf.push_str("((__r) => __r.length > 0 ? ");
                self.buf
                    .push_str("{ _tag: \"Some\", _0: __r[0] } : { _tag: \"None\" })(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "last" => {
                self.buf.push_str("((__r) => __r.length > 0 ? ");
                self.buf
                    .push_str("{ _tag: \"Some\", _0: __r[__r.length - 1] } : { _tag: \"None\" })(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "contains" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").includes(");
                self.emit_expr(&x.value)?;
                self.buf.push(')');
            }
            "index_of" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__r, __x) => { const __i = __r.indexOf(__x); ");
                self.buf.push_str(
                    "return __i >= 0 ? { _tag: \"Some\", _0: __i } : { _tag: \"None\" }; })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&x.value)?;
                self.buf.push(')');
            }
            "concat" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").concat(");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "join" => {
                let Some(sep) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").join(");
                self.emit_expr(&sep.value)?;
                self.buf.push(')');
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Emit an in-place `List` mutator (`push`/`append`, DQ18) to its JS form.
    ///
    /// Recognised via [`crate::generator::desugared_list_mutating_method`] in the
    /// `Call` arm. JS arrays carry a native `push`, so `recv.push(x)` lowers
    /// directly. The checker types these as `Void`, so they appear in statement
    /// position; the receiver is a `mut` lvalue (ownership-enforced), evaluated
    /// once.
    fn try_emit_list_mutating_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, _method, rest)) =
            crate::generator::desugared_list_mutating_method(node, callee, args)
        else {
            return Ok(false);
        };
        let Some(x) = rest.first() else {
            return Ok(false);
        };
        self.buf.push('(');
        self.emit_expr(recv)?;
        self.buf.push_str(").push(");
        self.emit_expr(&x.value)?;
        self.buf.push(')');
        Ok(true)
    }

    /// Emit a DQ30 in-place `List` mutator
    /// (`pop`/`remove_at`/`insert`/`reverse`/`set`) to its JS form.
    ///
    /// Recognised via [`crate::generator::desugared_list_inplace_mutator`]. JS
    /// arrays are reference values, so an IIFE parameter (`__r`) aliases the
    /// receiver and native mutations through it are visible to the caller —
    /// the same single-evaluation trick the read-only Optional-returning
    /// methods use:
    ///
    /// - `pop` → length-check + native `arr.pop()`, wrapped into the tagged
    ///   Optional rep (`{ _tag: "Some", _0: v }` / `{ _tag: "None" }`);
    /// - `remove_at(i)` → bounds-check + `arr.splice(i, 1)[0]`;
    /// - `insert(i, x)` → bounds-check (`0..=len`) + `arr.splice(i, 0, x)`;
    /// - `reverse` → native in-place `arr.reverse()`;
    /// - `set(i, x)` → bounds-check + `arr[i] = x` — the check is load-bearing:
    ///   native JS index-assign past the end silently *extends* the array.
    ///
    /// The bounds checks throw with the normalized abort message
    /// `List.<op>: index <i> out of bounds (len <n>)`, following the DQ23
    /// integer-division zero-check convention (`throw new Error(...)`).
    fn try_emit_list_inplace_mutator(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) =
            crate::generator::desugared_list_inplace_mutator(node, callee, args)
        else {
            return Ok(false);
        };
        match method {
            "pop" => {
                self.buf.push_str(
                    "((__r) => __r.length > 0 ? { _tag: \"Some\", _0: __r.pop() } : \
                     { _tag: \"None\" })(",
                );
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "remove_at" => {
                let Some(idx) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "((__r, __i) => { if (__i < 0 || __i >= __r.length) { throw new Error(\
                     \"List.remove_at: index \" + __i + \" out of bounds (len \" + __r.length + \")\"); } \
                     return __r.splice(__i, 1)[0]; })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&idx.value)?;
                self.buf.push(')');
            }
            "insert" => {
                let (Some(idx), Some(x)) = (rest.first(), rest.get(1)) else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "((__r, __i, __x) => { if (__i < 0 || __i > __r.length) { throw new Error(\
                     \"List.insert: index \" + __i + \" out of bounds (len \" + __r.length + \")\"); } \
                     __r.splice(__i, 0, __x); })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&idx.value)?;
                self.buf.push_str(", ");
                self.emit_expr(&x.value)?;
                self.buf.push(')');
            }
            "reverse" => {
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").reverse()");
            }
            "set" => {
                let (Some(idx), Some(x)) = (rest.first(), rest.get(1)) else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "((__r, __i, __x) => { if (__i < 0 || __i >= __r.length) { throw new Error(\
                     \"List.set: index \" + __i + \" out of bounds (len \" + __r.length + \")\"); } \
                     __r[__i] = __x; })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&idx.value)?;
                self.buf.push_str(", ");
                self.emit_expr(&x.value)?;
                self.buf.push(')');
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Emit a functional (closure-taking) `List` built-in method call to its JS
    /// form.
    ///
    /// Recognised via [`crate::generator::desugared_list_functional_method`] in
    /// the `Call` arm. JS arrays carry native `map`/`filter`/`reduce`/`forEach`/
    /// `some`/`every`/`flatMap`, so the closure is passed *once* (no duplicated
    /// receiver — the desugared `recv.map(recv, cb)` shape that the generic
    /// fall-through would otherwise emit is what broke). `fold(init, cb)` maps to
    /// `reduce(cb, init)`; `find` wraps the native `.find` result (element or
    /// `undefined`) into the tagged `Optional` representation user enum variants
    /// use.
    fn try_emit_list_functional_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) =
            crate::generator::desugared_list_functional_method(node, callee, args)
        else {
            return Ok(false);
        };
        match method {
            "map" | "filter" | "for_each" | "any" | "all" | "flat_map" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let native = match method {
                    "map" => "map",
                    "filter" => "filter",
                    "for_each" => "forEach",
                    "any" => "some",
                    "all" => "every",
                    "flat_map" => "flatMap",
                    _ => unreachable!(),
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                let _ = write!(self.buf, ").{native}(");
                self.emit_expr(&cb.value)?;
                self.buf.push(')');
            }
            "reduce" => {
                // Bock `reduce((a, b) => ...)` has no seed: the first element is
                // the initial accumulator, matching JS `.reduce(cb)`.
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").reduce(");
                self.emit_expr(&cb.value)?;
                self.buf.push(')');
            }
            "fold" => {
                // Bock `fold(init, (acc, x) => ...)` → JS `reduce(cb, init)`.
                let (Some(init), Some(cb)) = (rest.first(), rest.get(1)) else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").reduce(");
                self.emit_expr(&cb.value)?;
                self.buf.push_str(", ");
                self.emit_expr(&init.value)?;
                self.buf.push(')');
            }
            "find" => {
                // Native `.find` yields the element or `undefined`; wrap into the
                // tagged `Optional` representation.
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str("((__r) => { const __m = __r.find(");
                self.emit_expr(&cb.value)?;
                self.buf.push_str(
                    "); return __m === undefined ? { _tag: \"None\" } : { _tag: \"Some\", _0: __m }; })(",
                );
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Emit a built-in `Map[K, V]` method call to its JS form (native `Map`).
    ///
    /// Recognised via [`crate::generator::desugared_map_method`] (gated on the
    /// checker's `recv_kind = "Map"` annotation) and wired into the `Call` arm
    /// *before* [`Self::try_emit_list_method`], so a `Map` receiver's
    /// `get`/`contains_key`/`len` dispatch here rather than through the `List`
    /// path. `get` returns the same tagged-`Optional` representation the rest of
    /// codegen uses (`{ _tag: "Some", _0: v }` / `{ _tag: "None" }`). Mutating
    /// methods (`set`/`delete`/`merge`) mutate in place and return the receiver,
    /// matching the checker's `-> Map[K, V]` return type (full value-vs-`mut
    /// self` semantics is DQ18 → P4). Returns `true` if the call was handled.
    fn try_emit_map_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) = crate::generator::desugared_map_method(node, callee, args)
        else {
            return Ok(false);
        };
        match method {
            "len" | "length" | "count" => {
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").size");
            }
            "is_empty" => {
                self.buf.push_str("((");
                self.emit_expr(recv)?;
                self.buf.push_str(").size === 0)");
            }
            "contains_key" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").has(");
                self.emit_expr(&k.value)?;
                self.buf.push(')');
            }
            "get" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "((__m, __k) => __m.has(__k) ? { _tag: \"Some\", _0: __m.get(__k) } : \
                     { _tag: \"None\" })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&k.value)?;
                self.buf.push(')');
            }
            "set" => {
                let (Some(k), Some(v)) = (rest.first(), rest.get(1)) else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__m, __k, __v) => { __m.set(__k, __v); return __m; })(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&k.value)?;
                self.buf.push_str(", ");
                self.emit_expr(&v.value)?;
                self.buf.push(')');
            }
            "delete" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__m, __k) => { __m.delete(__k); return __m; })(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&k.value)?;
                self.buf.push(')');
            }
            "merge" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "((__m, __o) => { for (const [__k, __v] of __o) __m.set(__k, __v); \
                     return __m; })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "filter" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "((__m, __f) => { const __r = new Map(); \
                     for (const [__k, __v] of __m) if (__f(__k, __v)) __r.set(__k, __v); \
                     return __r; })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&f.value)?;
                self.buf.push(')');
            }
            "keys" => {
                self.buf.push_str("[...(");
                self.emit_expr(recv)?;
                self.buf.push_str(").keys()]");
            }
            "values" => {
                self.buf.push_str("[...(");
                self.emit_expr(recv)?;
                self.buf.push_str(").values()]");
            }
            "entries" | "to_list" => {
                self.buf.push_str("[...(");
                self.emit_expr(recv)?;
                self.buf.push_str(").entries()]");
            }
            "for_each" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__m, __f) => { for (const [__k, __v] of __m) __f(__k, __v); })(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&f.value)?;
                self.buf.push(')');
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Emit a built-in `Set[E]` method call to its JS form (native `Set`).
    ///
    /// Recognised via [`crate::generator::desugared_set_method`] (gated on
    /// `recv_kind = "Set"`) and wired *before* [`Self::try_emit_list_method`],
    /// so a `Set` receiver's `contains`/`len`/`filter`/`map` no longer route
    /// through the `List` path. Mutating methods (`add`/`remove`) mutate in
    /// place and return the receiver. Returns `true` if handled.
    fn try_emit_set_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) = crate::generator::desugared_set_method(node, callee, args)
        else {
            return Ok(false);
        };
        match method {
            "len" | "length" | "count" => {
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").size");
            }
            "is_empty" => {
                self.buf.push_str("((");
                self.emit_expr(recv)?;
                self.buf.push_str(").size === 0)");
            }
            "contains" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(").has(");
                self.emit_expr(&x.value)?;
                self.buf.push(')');
            }
            "add" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__s, __x) => { __s.add(__x); return __s; })(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&x.value)?;
                self.buf.push(')');
            }
            "remove" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__s, __x) => { __s.delete(__x); return __s; })(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&x.value)?;
                self.buf.push(')');
            }
            "union" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__a, __b) => new Set([...__a, ...__b]))(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "intersection" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__a, __b) => new Set([...__a].filter((__x) => __b.has(__x))))(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "difference" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__a, __b) => new Set([...__a].filter((__x) => !__b.has(__x))))(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "is_subset" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__a, __b) => [...__a].every((__x) => __b.has(__x)))(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "is_superset" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__a, __b) => [...__b].every((__x) => __a.has(__x)))(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "filter" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__s, __f) => new Set([...__s].filter(__f)))(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&f.value)?;
                self.buf.push(')');
            }
            "map" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__s, __f) => new Set([...__s].map(__f)))(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&f.value)?;
                self.buf.push(')');
            }
            "to_list" => {
                self.buf.push_str("[...(");
                self.emit_expr(recv)?;
                self.buf.push_str(")]");
            }
            "for_each" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("((__s, __f) => { for (const __x of __s) __f(__x); })(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&f.value)?;
                self.buf.push(')');
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Lower a primitive trait-bridge method call (`compare`/`eq`/`to_string`/
    /// `display` on a primitive receiver) to its JS form.
    ///
    /// `(1).compare(2)` resolves in the checker to `Ordering`, but a JS `number`
    /// has no `.compare`; this lowers it to a ternary that produces the same
    /// tagged-object `Ordering` value the construction/match sides use
    /// (`{ _tag: "Less" }` / `…"Equal"` / `…"Greater"`). `eq` becomes `===`;
    /// `to_string`/`display` become `String(x)`.
    /// Lower a desugared `String` built-in method call (`recv_kind =
    /// "Primitive:String"`) to its native JavaScript string op. Wired into the
    /// `Call` arm *before* `try_emit_list_method` so a String receiver's
    /// `len`/`contains`/`is_empty` dispatch here, not through the List path.
    ///
    /// `len` is the Unicode SCALAR count (`[...s].length`, which iterates by code
    /// point) per spec §18.3 — not `s.length` (UTF-16 code units). `byte_len` is
    /// the UTF-8 byte count via `TextEncoder`. `replace` replaces ALL occurrences
    /// (`replaceAll`). `split` returns a JS array, which is the List runtime rep.
    ///
    /// Gated on `recv_kind = "Primitive:String"` directly (not the cross-backend
    /// [`crate::generator::desugared_string_method`] subset) so JS can lower the
    /// wider resolved String surface — `slice`/`substring`/`char_at`/`index_of`/
    /// `repeat`/`reverse`/`trim_start`/`trim_end` — to native ops, matching the
    /// Rust backend without widening the shared `STRING_METHODS` const (which
    /// would force every backend to handle the extra names). `slice`/`reverse`
    /// iterate by code point (`[...s]`) to honour the scalar-index semantics.
    /// `char_at`/`index_of` return the inline tagged `Optional`
    /// (`{ _tag: "Some", _0: v }` / `{ _tag: "None" }`).
    fn try_emit_string_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        if crate::generator::primitive_recv_kind(node) != Some("String") {
            return Ok(false);
        }
        let Some((recv, field, rest)) = crate::generator::desugared_self_call(callee, args) else {
            return Ok(false);
        };
        let method = field.name.as_str();
        let recv_str = self.expr_to_string(recv)?;
        let arg0 = |this: &mut Self| -> Result<Option<String>, CodegenError> {
            rest.first()
                .map(|a| this.expr_to_string(&a.value))
                .transpose()
        };
        let code = match method {
            "len" | "length" | "count" => format!("[...({recv_str})].length"),
            "byte_len" => format!("new TextEncoder().encode({recv_str}).length"),
            "is_empty" => format!("(({recv_str}).length === 0)"),
            "to_upper" => format!("({recv_str}).toUpperCase()"),
            "to_lower" => format!("({recv_str}).toLowerCase()"),
            "trim" => format!("({recv_str}).trim()"),
            "trim_start" => format!("({recv_str}).trimStart()"),
            "trim_end" => format!("({recv_str}).trimEnd()"),
            "reverse" => format!("[...({recv_str})].reverse().join('')"),
            "to_string" | "display" => format!("String({recv_str})"),
            "repeat" => {
                let Some(n) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).repeat({n})")
            }
            "contains" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).includes({p})")
            }
            "starts_with" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).startsWith({p})")
            }
            "ends_with" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).endsWith({p})")
            }
            "replace" => {
                let Some(from) = arg0(self)? else {
                    return Ok(false);
                };
                let Some(to) = rest
                    .get(1)
                    .map(|a| self.expr_to_string(&a.value))
                    .transpose()?
                else {
                    return Ok(false);
                };
                format!("({recv_str}).replaceAll({from}, {to})")
            }
            "split" => {
                let Some(sep) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).split({sep})")
            }
            // `slice`/`substring(start, end)` are scalar-index half-open
            // substrings (spec §18.3 — indices count Unicode scalars, not UTF-16
            // code units). Iterate by code point via the spread so multibyte input
            // is handled correctly, then `slice`/`join` the resulting array.
            "slice" | "substring" => {
                let Some(start) = arg0(self)? else {
                    return Ok(false);
                };
                let Some(end) = rest
                    .get(1)
                    .map(|a| self.expr_to_string(&a.value))
                    .transpose()?
                else {
                    return Ok(false);
                };
                format!("[...({recv_str})].slice({start}, {end}).join('')")
            }
            // `char_at(i)` returns `Optional[Char]` — `None` when out of range.
            "char_at" => {
                let Some(i) = arg0(self)? else {
                    return Ok(false);
                };
                format!(
                    "((__s, __i) => __i >= 0 && __i < __s.length ? {{ _tag: \"Some\", _0: __s[__i] }} : {{ _tag: \"None\" }})([...({recv_str})], {i})"
                )
            }
            // `index_of(needle)` returns `Optional[Int]` — the scalar index of the
            // first match, or `None`. JS `indexOf` is a UTF-16 code-unit offset, so
            // convert it to a scalar index via the code-point prefix length.
            "index_of" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!(
                    "((__s, __p) => {{ const __b = __s.indexOf(__p); return __b >= 0 ? {{ _tag: \"Some\", _0: [...__s.slice(0, __b)].length }} : {{ _tag: \"None\" }}; }})({recv_str}, {p})"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Lower a desugared numeric/`Char`/`Bool` primitive method (`recv_kind =
    /// "Primitive:Int" | "Primitive:Float" | "Primitive:Char" | "Primitive:Bool"`)
    /// to its native JavaScript form. Covers the conversion and math methods the
    /// checker resolves on the scalar primitives — `to_float`/`to_int`/`abs`/`min`/
    /// `max`/`clamp`/`floor`/`ceil`/`round`/`sqrt`/… — none of which exist as
    /// methods on a JS `number`/`boolean`/string-char. Wired into the `Call` arm
    /// alongside [`Self::try_emit_string_method`], before the generic
    /// desugared-self-call fall-through (which would emit `n.to_float(n)`).
    /// `compare`/`eq`/`to_string`/`display`/`hash_code` stay on the primitive
    /// *bridge* path. `Char` is a single-code-point JS string.
    fn try_emit_numeric_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let prim = match crate::generator::primitive_recv_kind(node) {
            Some(p @ ("Int" | "Float" | "Char" | "Bool")) => p,
            _ => return Ok(false),
        };
        let Some((recv, field, rest)) = crate::generator::desugared_self_call(callee, args) else {
            return Ok(false);
        };
        let method = field.name.as_str();
        let recv_str = self.expr_to_string(recv)?;
        let arg = |this: &mut Self, i: usize| -> Result<Option<String>, CodegenError> {
            rest.get(i)
                .map(|a| this.expr_to_string(&a.value))
                .transpose()
        };
        let code = match (prim, method) {
            // Conversions. `to_float`/`to_int` are runtime no-ops on a JS `number`,
            // but `to_int` truncates toward zero (Bock `Float.to_int`).
            ("Int", "to_float") => format!("({recv_str})"),
            ("Float", "to_int") => format!("Math.trunc({recv_str})"),
            ("Char", "to_int") => format!("(({recv_str}).codePointAt(0))"),
            ("Bool", "to_int") => format!("(({recv_str}) ? 1 : 0)"),
            // Int math.
            ("Int", "abs") => format!("Math.abs({recv_str})"),
            ("Int" | "Float", "min") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("Math.min({recv_str}, {o})")
            }
            ("Int" | "Float", "max") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("Math.max({recv_str}, {o})")
            }
            ("Int" | "Float", "clamp") => {
                let (Some(lo), Some(hi)) = (arg(self, 0)?, arg(self, 1)?) else {
                    return Ok(false);
                };
                format!("Math.min(Math.max({recv_str}, {lo}), {hi})")
            }
            ("Int", "shift_left") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("(({recv_str}) << ({o}))")
            }
            ("Int", "shift_right") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("(({recv_str}) >> ({o}))")
            }
            // Float math.
            ("Float", "abs") => format!("Math.abs({recv_str})"),
            ("Float", "floor") => format!("Math.floor({recv_str})"),
            ("Float", "ceil") => format!("Math.ceil({recv_str})"),
            ("Float", "round") => format!("Math.round({recv_str})"),
            ("Float", "sqrt") => format!("Math.sqrt({recv_str})"),
            ("Float", "is_nan") => format!("Number.isNaN({recv_str})"),
            ("Float", "is_infinite") => format!("(!Number.isFinite({recv_str}))"),
            // Bool.
            ("Bool", "negate") => format!("(!({recv_str}))"),
            // Char (a one-code-point JS string).
            ("Char", "to_upper") => format!("({recv_str}).toUpperCase()"),
            ("Char", "to_lower") => format!("({recv_str}).toLowerCase()"),
            ("Char", "is_alpha") => format!("(/\\p{{L}}/u.test({recv_str}))"),
            ("Char", "is_digit") => format!("(/[0-9]/.test({recv_str}))"),
            ("Char", "is_whitespace") => format!("(/\\s/.test({recv_str}))"),
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    fn try_emit_primitive_bridge(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest, _prim)) =
            crate::generator::primitive_bridge_call(node, callee, args)
        else {
            return Ok(false);
        };
        self.emit_bridge_method(recv, method, rest)
    }

    /// Lower a sealed-core-trait bridge method on a *bounded generic type
    /// variable* (`a.eq(b)` / `a.compare(b)` inside `eq_check[T: Equatable]`) to
    /// its JS form (GAP-C). JS generics are erased, so only the method call needs
    /// lowering — `a.eq(b)` becomes `a === b`, etc. — and the body is identical to
    /// the `Primitive:<Ty>` bridge. Fires only when the bound trait is sealed-core
    /// and NOT a user-declared trait (a user trait's `impl` provides the method).
    fn try_emit_trait_bound_bridge(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest, _tr)) =
            crate::generator::trait_bound_bridge_call(node, callee, args, &self.trait_decls)
        else {
            return Ok(false);
        };
        // DQ29: unlike the `Primitive:<Ty>` bridge (whose receiver is a known
        // scalar, where `===` is correct), a bounded `T: Equatable` receiver
        // may be instantiated with a RECORD — JS `===` would be reference
        // identity — so `a.eq(b)` lowers through the `__bockEq` structural
        // helper, which falls back to `===` for primitives.
        if method == "eq" {
            let Some(other) = rest.first() else {
                return Ok(false);
            };
            let recv_str = self.expr_to_string(recv)?;
            let other = self.expr_to_string(&other.value)?;
            let _ = write!(self.buf, "__bockEq({recv_str}, {other})");
            return Ok(true);
        }
        self.emit_bridge_method(recv, method, rest)
    }

    /// Shared body of the primitive / trait-bound bridges: emit the native JS form
    /// of `compare` (the `Ordering` ternary), `eq` (`===`), or `to_string`/
    /// `display` (`String(..)`).
    fn emit_bridge_method(
        &mut self,
        recv: &AIRNode,
        method: &str,
        rest: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let recv_str = self.expr_to_string(recv)?;
        match method {
            "compare" => {
                let Some(other) = rest.first() else {
                    return Ok(false);
                };
                let other = self.expr_to_string(&other.value)?;
                let _ = write!(
                    self.buf,
                    "(({recv_str}) < ({other}) ? {{ _tag: \"Less\" }} : \
                     (({recv_str}) === ({other}) ? {{ _tag: \"Equal\" }} : {{ _tag: \"Greater\" }}))"
                );
            }
            "eq" => {
                let Some(other) = rest.first() else {
                    return Ok(false);
                };
                let other = self.expr_to_string(&other.value)?;
                let _ = write!(self.buf, "(({recv_str}) === ({other}))");
            }
            "to_string" | "display" => {
                let _ = write!(self.buf, "String({recv_str})");
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    // ── Top-level dispatch ──────────────────────────────────────────────────

    fn emit_node(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.mark_span(node.span);
        match &node.kind {
            NodeKind::Module { items, imports, .. } => {
                // Field/method name-collision set (camelCased, to match the
                // method-name casing). A method whose camelCased name equals a
                // field name is renamed via `js_method_name`. In the per-module
                // path this is pre-seeded program-wide by `generate_project` so a
                // call site in one file agrees with the renamed method declared
                // in another; we *extend* here so the single-module
                // `generate_module` path (no pre-seed) is also covered.
                self.field_method_collisions
                    .extend(crate::generator::collect_record_field_names(
                        node,
                        to_camel_case,
                    ));
                if self.per_module {
                    // Per-module native-import path (the real build): each module
                    // is emitted to its own `.js` file and the runtime helpers
                    // live in the shared `_bock_runtime.js`. Record which runtime
                    // helpers this module references; `generate_project` emits
                    // them once into the shared module, and `emit_esm_imports`
                    // imports the referenced names here.
                    if self.module_uses_concurrency(items) {
                        self.needs_runtime_concurrency = true;
                    }
                    if js_module_uses_range(items) {
                        self.needs_runtime_range = true;
                    }
                    if js_module_uses_eq(items) {
                        self.needs_runtime_eq = true;
                    }
                    // Real ESM imports (runtime, explicit `use`, implicit prelude)
                    // at the top of the file, before any declaration.
                    self.emit_esm_imports(imports)?;
                } else {
                    // Single-module self-contained emit (`generate_module`, used
                    // by unit tests): the module's runtime preludes are inlined
                    // into this one file and `ImportDecl`s are dropped. The
                    // concurrency / range runtimes are inlined at most once,
                    // gated on a ctx flag.
                    if !self.concurrency_runtime_emitted && self.module_uses_concurrency(items) {
                        self.buf.push_str(CONCURRENCY_RUNTIME_JS);
                        self.buf.push('\n');
                        self.concurrency_runtime_emitted = true;
                    }
                    if !self.range_runtime_emitted && js_module_uses_range(items) {
                        self.buf.push_str(RANGE_RUNTIME_JS);
                        self.buf.push('\n');
                        self.range_runtime_emitted = true;
                    }
                    if !self.eq_runtime_emitted && js_module_uses_eq(items) {
                        self.buf.push_str(EQ_RUNTIME_JS);
                        self.buf.push('\n');
                        self.eq_runtime_emitted = true;
                    }
                }
                // `@test` functions are transpiled separately into Vitest/Jest
                // test files (project mode, §20.6.2 — see `generate_tests`), never
                // into the runtime module tree: their `expect(...)` assertion DSL
                // has no runtime definition in the emitted source.
                let mut first = true;
                for item in items.iter() {
                    if crate::generator::fn_is_test(item) {
                        continue;
                    }
                    if !first {
                        self.buf.push('\n');
                    }
                    first = false;
                    self.emit_node(item)?;
                }
                // Per-module path: re-export the public non-function declarations
                // (functions export inline). Emitted once after all items.
                if self.per_module {
                    self.emit_trailing_exports();
                }
                Ok(())
            }
            NodeKind::ImportDecl { .. } => {
                // Bock `use` is resolved by the real ESM imports emitted up front
                // by `emit_esm_imports` from the `Module` arm (per-module path),
                // or dropped entirely in the single-module self-contained path.
                // Either way, the per-item visit here is a no-op.
                Ok(())
            }
            NodeKind::FnDecl {
                visibility,
                is_async,
                name,
                params,
                effect_clause,
                body,
                ..
            } => self.emit_fn_decl(
                *visibility,
                *is_async,
                &name.name,
                params,
                effect_clause,
                body,
                false,
            ),
            NodeKind::RecordDecl { name, fields, .. } => {
                // Record → class (supports prototype-based `impl` method attachment).
                self.record_names.insert(name.name.clone());
                if fields.is_empty() {
                    self.writeln(&format!("class {} {{}}", name.name));
                } else {
                    let field_names: Vec<&str> =
                        fields.iter().map(|f| f.name.name.as_str()).collect();
                    self.writeln(&format!("class {} {{", name.name));
                    self.indent += 1;
                    self.writeln(&format!("constructor({{ {} }}) {{", field_names.join(", "),));
                    self.indent += 1;
                    for f in &field_names {
                        self.writeln(&format!("this.{f} = {f};"));
                    }
                    self.indent -= 1;
                    self.writeln("}");
                    self.indent -= 1;
                    self.writeln("}");
                }
                Ok(())
            }
            NodeKind::EnumDecl { name, variants, .. } => {
                // ADTs → tagged object factory functions.
                for variant in variants {
                    self.emit_enum_variant(&name.name, variant)?;
                }
                Ok(())
            }
            NodeKind::ClassDecl {
                name,
                fields,
                methods,
                ..
            } => {
                // Register the class's positional field order so a `class`
                // literal lowers to `new Name(...)` (see `class_fields`). A
                // pre-pass already seeds this across the reachable set; re-record
                // here so the single-module emit path is correct even when the
                // pre-pass is not run.
                self.class_fields.insert(
                    name.name.clone(),
                    fields.iter().map(|f| f.name.name.clone()).collect(),
                );
                self.writeln(&format!("class {} {{", name.name));
                self.indent += 1;
                // Constructor
                let field_names: Vec<&str> = fields.iter().map(|f| f.name.name.as_str()).collect();
                self.writeln(&format!("constructor({}) {{", field_names.join(", ")));
                self.indent += 1;
                for f in &field_names {
                    self.writeln(&format!("this.{f} = {f};"));
                }
                self.indent -= 1;
                self.writeln("}");
                // Methods
                for method in methods {
                    self.buf.push('\n');
                    self.emit_class_method(method)?;
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::TraitDecl { name, methods, .. } => {
                // Traits → comment + method stubs as a "mixin" object.
                self.writeln(&format!("// trait {}", name.name));
                self.writeln(&format!("const {} = {{", name.name));
                self.indent += 1;
                for (i, method) in methods.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    if let NodeKind::FnDecl {
                        name, params, body, ..
                    } = &method.kind
                    {
                        let param_names = self.collect_param_names(params);
                        self.writeln(&format!("{}({}) {{", name.name, param_names.join(", ")));
                        self.indent += 1;
                        self.emit_block_body(body)?;
                        self.indent -= 1;
                        self.writeln("},");
                    }
                }
                self.indent -= 1;
                self.writeln("};");
                Ok(())
            }
            NodeKind::ImplBlock {
                trait_path,
                target,
                methods,
                ..
            } => {
                // impl → comment + attach methods to prototype.
                let target_name = self.type_expr_to_string(target);
                if let Some(tp) = trait_path {
                    let trait_name = tp
                        .segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(".");
                    self.writeln(&format!("// impl {trait_name} for {target_name}"));
                } else {
                    self.writeln(&format!("// impl {target_name}"));
                }
                // Trait default methods (codegen-completeness P2): synthesize
                // every default method the impl does not override onto the
                // target's prototype, alongside the impl's own methods. JS is
                // untyped, so a default method emits identically to an impl
                // method; a default body that calls another trait method via
                // `self.other(self, ...)` resolves through the same prototype.
                let default_methods: Vec<AIRNode> = trait_path
                    .as_ref()
                    .map(|tp| {
                        crate::generator::inherited_default_methods(&self.trait_decls, tp, methods)
                    })
                    .unwrap_or_default();
                for method in methods.iter().chain(default_methods.iter()) {
                    if let NodeKind::FnDecl {
                        is_async,
                        name,
                        params,
                        effect_clause,
                        body,
                        ..
                    } = &method.kind
                    {
                        let async_kw = if *is_async { "async " } else { "" };
                        let param_names = self.collect_param_names(params);
                        let effects_param = self.effects_param(effect_clause);
                        let mut all_params = param_names;
                        if let Some(ep) = effects_param {
                            all_params.push(ep);
                        }
                        // An associated function (no `self` receiver, e.g. a
                        // `From` impl's `from`) is a *static* method on the
                        // class object, reached as `Type.method(...)`. An
                        // instance method attaches to the prototype. Emitting an
                        // associated fn on the prototype would make
                        // `Type.method` undefined at the call site.
                        let attach = if crate::generator::is_associated_impl_method(
                            method,
                            &self.effect_ops,
                        ) {
                            ""
                        } else {
                            ".prototype"
                        };
                        self.writeln(&format!(
                            "{target_name}{attach}.{} = {async_kw}function({}) {{",
                            self.js_method_name(&name.name),
                            all_params.join(", "),
                        ));
                        self.indent += 1;
                        let old_handler_vars = self.current_handler_vars.clone();
                        let expanded = self.expand_effect_names(effect_clause);
                        for ename in &expanded {
                            self.current_handler_vars
                                .insert(ename.clone(), to_camel_case(ename));
                        }
                        self.emit_block_body(body)?;
                        self.current_handler_vars = old_handler_vars;
                        self.indent -= 1;
                        self.writeln("};");
                    }
                }
                Ok(())
            }
            NodeKind::EffectDecl {
                name,
                components,
                operations,
                ..
            } => {
                // Composite effect: register expansion and emit comment.
                if !components.is_empty() {
                    let comp_names: Vec<String> = components
                        .iter()
                        .map(|tp| {
                            tp.segments
                                .last()
                                .map_or("effect".to_string(), |s| s.name.clone())
                        })
                        .collect();
                    self.writeln(&format!(
                        "// composite effect {} = {}",
                        name.name,
                        comp_names.join(" + ")
                    ));
                    // A composite effect is a compile-time grouping with no
                    // runtime representation (functions expand it to its
                    // component handler params). But a *public* composite
                    // effect appears in this module's `export { … }` list, so
                    // it still needs a concrete binding to export — otherwise
                    // the ESM export references an undefined name. Emit a frozen
                    // marker object recording the component names.
                    let marker = comp_names
                        .iter()
                        .map(|c| format!("\"{c}\""))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.writeln(&format!(
                        "const {} = Object.freeze({{ __composite: [{}] }});",
                        name.name, marker
                    ));
                    self.composite_effects.insert(name.name.clone(), comp_names);
                    return Ok(());
                }
                // Record effect operations for Call → handler.op rewriting.
                for op in operations {
                    if let NodeKind::FnDecl { name: op_name, .. } = &op.kind {
                        self.effect_ops
                            .insert(op_name.name.clone(), name.name.clone());
                    }
                }
                // Effects → abstract class with methods that throw.
                self.writeln(&format!("class {} {{", name.name));
                self.indent += 1;
                for op in operations {
                    if let NodeKind::FnDecl { name, params, .. } = &op.kind {
                        let param_names = self.collect_param_names(params);
                        self.writeln(&format!(
                            "{}({}) {{",
                            to_camel_case(&name.name),
                            param_names.join(", "),
                        ));
                        self.indent += 1;
                        self.writeln("throw new Error(\"not implemented\");");
                        self.indent -= 1;
                        self.writeln("}");
                    }
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::TypeAlias { name, .. } => {
                // Type aliases are erased in JS.
                self.writeln(&format!("// type {} = ...", name.name));
                Ok(())
            }
            NodeKind::ConstDecl { name, value, .. } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}const {} = ", name.name);
                self.emit_expr(value)?;
                self.buf.push_str(";\n");
                Ok(())
            }
            NodeKind::ModuleHandle { effect, handler } => {
                let effect_name = effect.segments.last().map_or("effect", |s| s.name.as_str());
                let var_name = format!("__{}", to_camel_case(effect_name));
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}const {var_name} = ");
                self.emit_expr(handler)?;
                self.buf.push_str(";\n");
                // Register as ambient handler so same-module calls pick it up.
                self.current_handler_vars
                    .insert(effect_name.to_string(), var_name);
                Ok(())
            }
            NodeKind::PropertyTest { name, body, .. } => {
                self.writeln(&format!("// property test: {name}"));
                self.writeln("// (property tests are not emitted in JS output)");
                let _ = body;
                Ok(())
            }
            // Statement / expression nodes at top level:
            NodeKind::LetBinding { .. }
            | NodeKind::If { .. }
            | NodeKind::For { .. }
            | NodeKind::While { .. }
            | NodeKind::Loop { .. }
            | NodeKind::Return { .. }
            | NodeKind::Break { .. }
            | NodeKind::Continue
            | NodeKind::Guard { .. }
            | NodeKind::Match { .. }
            | NodeKind::Block { .. }
            | NodeKind::HandlingBlock { .. }
            | NodeKind::Assign { .. } => self.emit_stmt(node),
            // Expression nodes that appear as statements:
            _ => {
                // A `?` in this statement-position expression (e.g. a bare
                // `save(x)?`) hoists to a pre-statement temp + early-return.
                let only_propagate = self.hoist_propagates(node)?
                    && matches!(&node.kind, NodeKind::Propagate { .. });
                // A bare `expr?` statement's whole value is the hoisted temp's
                // payload; once hoisted (and the success path falls through),
                // there is nothing left to emit as its own statement.
                if only_propagate {
                    return Ok(());
                }
                self.write_indent();
                self.emit_expr(node)?;
                self.buf.push_str(";\n");
                Ok(())
            }
        }
    }

    // ── Function declarations ───────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn emit_fn_decl(
        &mut self,
        visibility: Visibility,
        is_async: bool,
        name: &str,
        params: &[AIRNode],
        effect_clause: &[bock_ast::TypePath],
        body: &AIRNode,
        _is_method: bool,
    ) -> Result<(), CodegenError> {
        let export = if matches!(visibility, Visibility::Public) {
            "export "
        } else {
            ""
        };
        let async_kw = if is_async { "async " } else { "" };
        let param_names = self.collect_param_names(params);
        let effects_param = self.effects_param(effect_clause);
        let mut all_params = param_names;
        if let Some(ep) = effects_param {
            all_params.push(ep);
        }
        if !effect_clause.is_empty() {
            let effect_names = self.expand_effect_names(effect_clause);
            self.fn_effects.insert(name.to_string(), effect_names);
        }
        let js_name = js_value_ident(name);
        self.writeln(&format!(
            "{export}{async_kw}function {js_name}({}) {{",
            all_params.join(", "),
        ));
        self.indent += 1;
        let old_handler_vars = self.current_handler_vars.clone();
        let expanded = self.expand_effect_names(effect_clause);
        for ename in &expanded {
            self.current_handler_vars
                .insert(ename.clone(), to_camel_case(ename));
        }
        self.emit_fn_body_seeded(params, body)?;
        self.current_handler_vars = old_handler_vars;
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    fn emit_class_method(&mut self, method: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::FnDecl {
            is_async,
            name,
            params,
            effect_clause,
            body,
            ..
        } = &method.kind
        {
            let async_kw = if *is_async { "async " } else { "" };
            let param_names = self.collect_param_names(params);
            let effects_param = self.effects_param(effect_clause);
            let mut all_params = param_names;
            if let Some(ep) = effects_param {
                all_params.push(ep);
            }
            let method_name = self.js_method_name(&to_camel_case(&name.name));
            self.writeln(&format!(
                "{async_kw}{method_name}({}) {{",
                all_params.join(", "),
            ));
            self.indent += 1;
            let old_handler_vars = self.current_handler_vars.clone();
            let expanded = self.expand_effect_names(effect_clause);
            for ename in &expanded {
                self.current_handler_vars
                    .insert(ename.clone(), to_camel_case(ename));
            }
            self.emit_fn_body_seeded(params, body)?;
            self.current_handler_vars = old_handler_vars;
            self.indent -= 1;
            self.writeln("}");
        }
        Ok(())
    }

    fn collect_param_names(&self, params: &[AIRNode]) -> Vec<String> {
        params
            .iter()
            .filter_map(|p| {
                if let NodeKind::Param {
                    pattern, default, ..
                } = &p.kind
                {
                    let name = self.pattern_to_binding_name(pattern);
                    if let Some(def) = default {
                        let mut ctx = EmitCtx::new();
                        ctx.indent = self.indent;
                        ctx.enum_variants = self.enum_variants.clone();
                        if ctx.emit_expr_to_string(def).is_ok() {
                            let (def_str, _) = ctx.finish();
                            return Some(format!("{name} = {def_str}"));
                        }
                    }
                    Some(name)
                } else {
                    None
                }
            })
            .collect()
    }

    fn emit_expr_to_string(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.emit_expr(node)
    }

    /// Expand effect names, replacing composite effects with their components.
    fn expand_effect_names(&self, effects: &[bock_ast::TypePath]) -> Vec<String> {
        let mut result = Vec::new();
        for tp in effects {
            let name = tp
                .segments
                .last()
                .map_or("effect".to_string(), |s| s.name.clone());
            if let Some(components) = self.composite_effects.get(&name) {
                result.extend(components.iter().cloned());
            } else {
                result.push(name);
            }
        }
        result
    }

    /// The in-scope `Clock` effect handler variable, if one is installed.
    ///
    /// When `Some`, the `Clock` time operations (`Instant.now`, `sleep`,
    /// `elapsed`) are routed through the handler instead of inlining the host
    /// primitive (Q-clock-handler-routing, §18.3.1/§18.4); when `None`, no
    /// handler is in scope and the default host primitive is emitted.
    fn clock_handler_var(&self) -> Option<&str> {
        self.current_handler_vars.get("Clock").map(String::as_str)
    }

    /// Effects → destructured parameter object: `{ log, clock }`.
    fn effects_param(&self, effects: &[bock_ast::TypePath]) -> Option<String> {
        if effects.is_empty() {
            return None;
        }
        let expanded = self.expand_effect_names(effects);
        if expanded.is_empty() {
            return None;
        }
        let names: Vec<String> = expanded.iter().map(|n| to_camel_case(n)).collect();
        Some(format!("{{ {} }}", names.join(", ")))
    }

    /// Build a `{ effect: handler_var, ... }` argument for calling an effectful function.
    /// Returns `None` if the callee has no registered effects or no handlers are in scope.
    fn build_effects_call_arg_js(&self, fn_name: &str) -> Option<String> {
        let effects = self.fn_effects.get(fn_name)?;
        let entries: Vec<String> = effects
            .iter()
            .filter_map(|e| {
                let handler_var = self.current_handler_vars.get(e)?;
                let param_name = to_camel_case(e);
                Some(format!("{param_name}: {handler_var}"))
            })
            .collect();
        if entries.is_empty() {
            return None;
        }
        Some(format!("{{ {} }}", entries.join(", ")))
    }

    // ── Enum variant factories ──────────────────────────────────────────────

    fn emit_enum_variant(
        &mut self,
        enum_name: &str,
        variant: &AIRNode,
    ) -> Result<(), CodegenError> {
        if let NodeKind::EnumVariant { name, payload } = &variant.kind {
            let vname = &name.name;
            match payload {
                EnumVariantPayload::Unit => {
                    self.writeln(&format!(
                        "const {enum_name}_{vname} = Object.freeze({{ _tag: \"{vname}\" }});"
                    ));
                }
                EnumVariantPayload::Struct(fields) => {
                    let field_names: Vec<&str> =
                        fields.iter().map(|f| f.name.name.as_str()).collect();
                    self.writeln(&format!(
                        "function {enum_name}_{vname}({}) {{",
                        field_names.join(", ")
                    ));
                    self.indent += 1;
                    self.writeln(&format!(
                        "return {{ _tag: \"{vname}\", {} }};",
                        field_names.join(", ")
                    ));
                    self.indent -= 1;
                    self.writeln("}");
                }
                EnumVariantPayload::Tuple(elems) => {
                    let param_names: Vec<String> =
                        (0..elems.len()).map(|i| format!("_{i}")).collect();
                    self.writeln(&format!(
                        "function {enum_name}_{vname}({}) {{",
                        param_names.join(", ")
                    ));
                    self.indent += 1;
                    self.writeln(&format!(
                        "return {{ _tag: \"{vname}\", {} }};",
                        param_names
                            .iter()
                            .enumerate()
                            .map(|(i, p)| format!("_{i}: {p}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    self.indent -= 1;
                    self.writeln("}");
                }
            }
        }
        Ok(())
    }

    // ── Statements ──────────────────────────────────────────────────────────

    fn emit_stmt(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.mark_span(node.span);
        // Hoist any `?` in this statement's value into pre-statement temps +
        // early-returns (see `hoist_propagates`). The value-carrying statement
        // arms (`let`/assign/return/expr-stmt) all funnel through here.
        self.hoist_propagates(node)?;
        match &node.kind {
            NodeKind::LetBinding {
                is_mut,
                pattern,
                value,
                ..
            } => {
                // Declare-only temp from the shared value-CF hoist: emit a bare
                // `let name;` (no initialiser); the relocated control flow that
                // follows assigns it on every non-diverging path.
                if node.metadata.contains_key(crate::generator::DECL_ONLY_META) {
                    if let NodeKind::BindPat { name, .. } = &pattern.kind {
                        let ind = self.indent_str();
                        let js_name = js_value_ident(&name.name);
                        self.mark_simple_let_declared(&js_name);
                        let _ = writeln!(self.buf, "{ind}let {js_name};");
                        return Ok(());
                    }
                }
                let ind = self.indent_str();
                // A simple `let name = …` is subject to JS redeclaration rules.
                // Bock allows re-binding the same name in one scope (shadowing);
                // JS does not, so the second-and-later binding of a simple name
                // becomes a plain assignment, and the first declaration uses
                // `let` (not `const`) when the name is later re-bound/assigned.
                if let NodeKind::BindPat { name, .. } = &pattern.kind {
                    let js_name = js_value_ident(&name.name);
                    if self.simple_let_redeclared(&js_name) {
                        let _ = write!(self.buf, "{ind}{js_name} = ");
                        self.emit_expr(value)?;
                        self.buf.push_str(";\n");
                        return Ok(());
                    }
                    let needs_let = *is_mut || self.simple_let_needs_let(&js_name);
                    let kw = if needs_let { "let" } else { "const" };
                    self.mark_simple_let_declared(&js_name);
                    let _ = write!(self.buf, "{ind}{kw} {js_name} = ");
                    self.emit_expr(value)?;
                    self.buf.push_str(";\n");
                    return Ok(());
                }
                let kw = if *is_mut { "let" } else { "const" };
                let binding = self.pattern_to_js_destructure(pattern);
                let _ = write!(self.buf, "{ind}{kw} {binding} = ");
                self.emit_expr(value)?;
                self.buf.push_str(";\n");
                Ok(())
            }
            NodeKind::If {
                let_pattern,
                condition,
                then_block,
                else_block,
            } => {
                if let Some(pat) = let_pattern {
                    // if-let → check + destructure
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}if (");
                    self.emit_expr(condition)?;
                    self.buf.push_str(" != null) {\n");
                    self.indent += 1;
                    let binding = self.pattern_to_js_destructure(pat);
                    self.writeln(&format!("const {binding} = "));
                    // Fix: remove trailing newline, add the condition expr
                    // Actually, for if-let, condition is the value being matched.
                    // We'll just emit the block body.
                    self.emit_block_body(then_block)?;
                    self.indent -= 1;
                } else {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}if (");
                    self.emit_expr(condition)?;
                    self.buf.push_str(") {\n");
                    self.indent += 1;
                    self.emit_block_body(then_block)?;
                    self.indent -= 1;
                }
                if let Some(else_b) = else_block {
                    if matches!(else_b.kind, NodeKind::If { .. }) {
                        let ind = self.indent_str();
                        let _ = write!(self.buf, "{ind}}} else ");
                        // Emit the else-if inline (no indent push for the `if` keyword).
                        self.emit_stmt(else_b)?;
                        return Ok(());
                    }
                    self.writeln("} else {");
                    self.indent += 1;
                    self.emit_block_body(else_b)?;
                    self.indent -= 1;
                }
                self.writeln("}");
                Ok(())
            }
            NodeKind::For {
                pattern,
                iterable,
                body,
            } => {
                let binding = self.pattern_to_js_destructure(pattern);
                self.emit_loop_label_prefix(body);
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}for (const {binding} of ");
                self.emit_expr(iterable)?;
                self.buf.push_str(") {\n");
                self.indent += 1;
                self.emit_loop_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                self.loop_labels.pop();
                Ok(())
            }
            NodeKind::While { condition, body } => {
                self.emit_loop_label_prefix(body);
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}while (");
                self.emit_expr(condition)?;
                self.buf.push_str(") {\n");
                self.indent += 1;
                self.emit_loop_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                self.loop_labels.pop();
                Ok(())
            }
            NodeKind::Loop { body } => {
                self.emit_loop_label_prefix(body);
                self.writeln("while (true) {");
                self.indent += 1;
                self.emit_loop_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                self.loop_labels.pop();
                Ok(())
            }
            NodeKind::Return { value } => {
                if let Some(val) = value {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}return ");
                    self.emit_expr(val)?;
                    self.buf.push_str(";\n");
                } else {
                    self.writeln("return;");
                }
                Ok(())
            }
            NodeKind::Break { value } => {
                if let Some(val) = value {
                    // JS break doesn't support values; emit as comment + break.
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}/* break value: ");
                    self.emit_expr(val)?;
                    self.buf.push_str(" */\n");
                }
                // Inside a statement-arm `switch`, a bare `break` exits the
                // switch; target the enclosing loop label instead.
                if self.switch_label_depth > 0 {
                    if let Some(label) = self.innermost_loop_label() {
                        self.writeln(&format!("break {label};"));
                        return Ok(());
                    }
                }
                self.writeln("break;");
                Ok(())
            }
            NodeKind::Continue => {
                if self.switch_label_depth > 0 {
                    if let Some(label) = self.innermost_loop_label() {
                        self.writeln(&format!("continue {label};"));
                        return Ok(());
                    }
                }
                self.writeln("continue;");
                Ok(())
            }
            NodeKind::Guard {
                let_pattern,
                condition,
                else_block,
            } => {
                if let Some(pat) = let_pattern {
                    // `guard (let pat = expr) else { … }`: evaluate `expr` once,
                    // run the else (which must diverge) when `pat` does not
                    // match, then bind `pat`'s names into the *enclosing* scope
                    // so they are in scope for the statements after the guard.
                    self.match_temp_counter += 1;
                    let tmp = format!("__guard{}", self.match_temp_counter);
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}const {tmp} = ");
                    self.emit_expr(condition)?;
                    self.buf.push_str(";\n");
                    let test = self.pattern_test_js(pat, &tmp);
                    // A bare bind / wildcard pattern always matches → no `if`.
                    if !test.is_empty() {
                        let ind = self.indent_str();
                        let _ = writeln!(self.buf, "{ind}if (!({test})) {{");
                        self.indent += 1;
                        self.emit_block_body(else_block)?;
                        self.indent -= 1;
                        self.writeln("}");
                    }
                    // Bindings land in the enclosing scope (no nested block).
                    self.pattern_binds_js(pat, &tmp)?;
                } else {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}if (!(");
                    self.emit_expr(condition)?;
                    self.buf.push_str(")) {\n");
                    self.indent += 1;
                    self.emit_block_body(else_block)?;
                    self.indent -= 1;
                    self.writeln("}");
                }
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => self.emit_match(scrutinee, arms),
            NodeKind::Block { stmts, tail } => {
                // A statement-position block is its own JS `{}` lexical scope, so
                // it gets its own `let` scope frame (a name re-bound inside is
                // independent of the enclosing block's bindings).
                self.writeln("{");
                self.indent += 1;
                self.enter_let_scope(node);
                for s in stmts {
                    self.emit_node(s)?;
                }
                if let Some(t) = tail {
                    self.write_indent();
                    self.emit_expr(t)?;
                    self.buf.push_str(";\n");
                }
                self.leave_let_scope();
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::HandlingBlock { handlers, body } => {
                // handling block → scoped handler instantiation. The emitted
                // `{ … }` is its own JS lexical block, so it gets a fresh `let`
                // scope frame: a name first bound in one `handling` block and
                // re-bound in a *sibling* `handling` block is two independent
                // declarations (each block-scoped), not a redeclaration. Without
                // a fresh frame the redeclaration tracker would carry the prior
                // block's `declared` set into this one and rewrite the second
                // `let x = …` into a bare `x = …`, referencing a name that went
                // out of scope when the first block closed (ReferenceError under
                // strict mode; a leaked global in sloppy mode).
                self.writeln("{");
                self.indent += 1;
                self.enter_let_scope(body);
                let old_handler_vars = self.current_handler_vars.clone();
                for h in handlers {
                    let effect_name = h
                        .effect
                        .segments
                        .last()
                        .map_or("effect", |s| s.name.as_str());
                    let var_name = format!("__{}", to_camel_case(effect_name));
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}const {var_name} = ");
                    self.emit_expr(&h.handler)?;
                    self.buf.push_str(";\n");
                    self.current_handler_vars
                        .insert(effect_name.to_string(), var_name);
                }
                if let NodeKind::Block { stmts, tail } = &body.kind {
                    for s in stmts {
                        self.emit_node(s)?;
                    }
                    if let Some(t) = tail {
                        self.write_indent();
                        self.emit_expr(t)?;
                        self.buf.push_str(";\n");
                    }
                } else {
                    self.emit_stmt(body)?;
                }
                self.current_handler_vars = old_handler_vars;
                self.leave_let_scope();
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::Assign { op, target, value } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}");
                self.emit_expr(target)?;
                let op_str = match op {
                    AssignOp::Assign => "=",
                    AssignOp::AddAssign => "+=",
                    AssignOp::SubAssign => "-=",
                    AssignOp::MulAssign => "*=",
                    AssignOp::DivAssign => "/=",
                    AssignOp::RemAssign => "%=",
                };
                let _ = write!(self.buf, " {op_str} ");
                self.emit_expr(value)?;
                self.buf.push_str(";\n");
                Ok(())
            }
            _ => {
                // Fallback: emit as expression statement.
                self.write_indent();
                self.emit_expr(node)?;
                self.buf.push_str(";\n");
                Ok(())
            }
        }
    }

    // ── Expressions ─────────────────────────────────────────────────────────

    fn emit_expr(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.mark_span(node.span);
        match &node.kind {
            NodeKind::Literal { lit } => {
                match lit {
                    Literal::Int(s) => self.buf.push_str(s),
                    Literal::Float(s) => self.buf.push_str(s),
                    Literal::Bool(b) => self.buf.push_str(if *b { "true" } else { "false" }),
                    Literal::Char(s) => {
                        self.buf.push('\'');
                        self.buf.push_str(s);
                        self.buf.push('\'');
                    }
                    Literal::String(s) => {
                        self.buf.push('"');
                        self.buf.push_str(&escape_js_string(s));
                        self.buf.push('"');
                    }
                    Literal::Unit => self.buf.push_str("undefined"),
                }
                Ok(())
            }
            NodeKind::Identifier { name } => {
                if name.name == "None" {
                    self.buf.push_str("{ _tag: \"None\" }");
                } else if let Some(variant) = crate::generator::ordering_variant(&name.name) {
                    // Prelude `Ordering` variant → an inline tagged object, the
                    // same self-contained representation the primitive-bridge
                    // `compare` and the `_tag`-switch match use (when the
                    // `core.compare` enum decl is not among the reached modules).
                    let _ = write!(self.buf, "{{ _tag: \"{variant}\" }}");
                } else if let Some(enum_name) = self
                    .user_variant_for_name(&name.name)
                    .map(|i| i.enum_name.clone())
                {
                    // A bare unit-variant reference (`Red`) → the frozen
                    // `{enum}_{variant}` const emitted by `emit_enum_variant`.
                    let _ = write!(self.buf, "{enum_name}_{}", name.name);
                } else if self.const_names.contains(&name.name) {
                    // A module-scope `const` is emitted verbatim at its
                    // declaration; spell its use site identically so the two agree
                    // (the `to_camel_case` transform would mangle a SCREAMING_SNAKE
                    // name, e.g. `FIZZ_NUM` → `fizzNUM`).
                    self.buf.push_str(&name.name);
                } else {
                    self.buf.push_str(&js_value_ident(&name.name));
                }
                Ok(())
            }
            NodeKind::BinaryOp { op, left, right } => {
                // `+` on two `List[T]` operands is concatenation: spread both into
                // a fresh array (`[...a, ...b]`). JS's native `+` would coerce the
                // arrays to strings and concatenate *those*, a silent bug.
                if matches!(op, BinOp::Add) && crate::generator::is_list_concat(node, left, right) {
                    self.buf.push_str("[...");
                    self.emit_expr(left)?;
                    self.buf.push_str(", ...");
                    self.emit_expr(right)?;
                    self.buf.push(']');
                    return Ok(());
                }
                // Integer `/` and `%` (DQ23, §3.6): JS `/` is float division and
                // `Math.trunc(a / 0)` yields `Infinity` rather than aborting, so
                // lower to a self-contained IIFE that (a) aborts on a zero divisor
                // and (b) truncates toward zero. JS `%` already takes the sign of
                // the dividend, so the remainder needs only the zero-abort. Passing
                // both operands as IIFE arguments evaluates each exactly once.
                if matches!(op, BinOp::Div | BinOp::Rem) && crate::generator::is_int_arith(node) {
                    let body = if matches!(op, BinOp::Div) {
                        "Math.trunc(__a / __b)"
                    } else {
                        "__a % __b"
                    };
                    self.buf.push_str("((__a, __b) => { if (__b === 0) { throw new Error(\"integer division or modulo by zero\"); } return ");
                    self.buf.push_str(body);
                    self.buf.push_str("; })(");
                    self.emit_expr(left)?;
                    self.buf.push_str(", ");
                    self.emit_expr(right)?;
                    self.buf.push(')');
                    return Ok(());
                }
                // Ordering operators on a user `Comparable` type lower through
                // `compare` (native `<` on two objects coerces to `NaN`). The
                // tagged `Ordering` is read off `._tag`, matching how a
                // hand-written `a.compare(b)` lowers — the receiver is passed both
                // as the JS method receiver and as the explicit `self` argument.
                if crate::generator::is_user_compare(node) {
                    if let Some((tag, is_eq)) = crate::generator::user_compare_variant(*op) {
                        let recv = self.expr_to_string(left)?;
                        let other = self.expr_to_string(right)?;
                        let eq = if is_eq { "===" } else { "!==" };
                        let _ = write!(
                            self.buf,
                            "(({recv}).compare({recv}, {other})._tag {eq} \"{tag}\")"
                        );
                        return Ok(());
                    }
                }
                // DQ29 (§18.5 structural Equatable): a stamped `==`/`!=`
                // cannot use native `===` (reference identity on objects).
                // The `"impl"` lane dispatches through the explicit
                // `impl Equatable`'s `eq` — receiver passed as both the JS
                // method receiver and the explicit `self` argument, matching
                // how a hand-written `a.eq(b)` lowers (Q-js-user-equality-
                // reference, #339). The structural lanes lower through the
                // `__bockEq` runtime helper.
                if matches!(op, BinOp::Eq | BinOp::Ne) {
                    if let Some(kind) = crate::generator::user_eq_kind(node) {
                        let recv = self.expr_to_string(left)?;
                        let other = self.expr_to_string(right)?;
                        let neg = if *op == BinOp::Ne { "!" } else { "" };
                        if kind == "impl" {
                            let _ = write!(self.buf, "{neg}(({recv}).eq({recv}, {other}))");
                        } else {
                            let _ = write!(self.buf, "{neg}__bockEq({recv}, {other})");
                        }
                        return Ok(());
                    }
                }
                self.buf.push('(');
                self.emit_expr(left)?;
                let op_str = match op {
                    BinOp::Add => " + ",
                    BinOp::Sub => " - ",
                    BinOp::Mul => " * ",
                    BinOp::Div => " / ",
                    BinOp::Rem => " % ",
                    BinOp::Pow => " ** ",
                    BinOp::Eq => " === ",
                    BinOp::Ne => " !== ",
                    BinOp::Lt => " < ",
                    BinOp::Le => " <= ",
                    BinOp::Gt => " > ",
                    BinOp::Ge => " >= ",
                    BinOp::And => " && ",
                    BinOp::Or => " || ",
                    BinOp::BitAnd => " & ",
                    BinOp::BitOr => " | ",
                    BinOp::BitXor => " ^ ",
                    BinOp::Compose => " /* >> */ ",
                    BinOp::Is => " instanceof ",
                };
                self.buf.push_str(op_str);
                self.emit_expr(right)?;
                self.buf.push(')');
                Ok(())
            }
            NodeKind::UnaryOp { op, operand } => {
                let op_str = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                    UnaryOp::BitNot => "~",
                };
                self.buf.push_str(op_str);
                self.emit_expr(operand)?;
                Ok(())
            }
            NodeKind::Call { callee, args, .. } => {
                if let Some(code) = self.map_prelude_call(callee, args)? {
                    self.buf.push_str(&code);
                    return Ok(());
                }
                if self.try_emit_prelude_ctor(callee, args)? {
                    return Ok(());
                }
                if self.try_emit_time_assoc_call(callee, args)? {
                    return Ok(());
                }
                if self.try_emit_time_desugared_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_concurrency_call(callee, args)? {
                    return Ok(());
                }
                // Map/Set method dispatch runs *before* the List recogniser so
                // the overlapping method names (`len`/`contains`/`filter`/`map`/
                // `to_list`) and the Map/Set-only `get`/`set`/`add`/`keys`/… are
                // routed by the checker's `recv_kind`, not by name alone.
                if self.try_emit_map_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_set_method(node, callee, args)? {
                    return Ok(());
                }
                // String method dispatch runs *before* the List recogniser so the
                // overlapping `len`/`contains`/`is_empty` names route by the
                // checker's `recv_kind = "Primitive:String"`, not by name alone.
                if self.try_emit_string_method(node, callee, args)? {
                    return Ok(());
                }
                // Numeric/Char/Bool primitive methods (`to_float`/`abs`/`sqrt`/…)
                // likewise route by the checker's `recv_kind = "Primitive:Int|…"`
                // before the generic fall-through, which would emit `n.to_float(n)`.
                if self.try_emit_numeric_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_list_mutating_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_list_inplace_mutator(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_list_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_list_functional_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_primitive_bridge(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_trait_bound_bridge(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_container_method(node, callee, args)? {
                    return Ok(());
                }
                // Rewrite bare effect operation calls: log(...) → handler.log(...)
                if let NodeKind::Identifier { name } = &callee.kind {
                    if let Some(effect_name) = self.effect_ops.get(&name.name).cloned() {
                        if let Some(handler_var) =
                            self.current_handler_vars.get(&effect_name).cloned()
                        {
                            let _ = write!(self.buf, "{}.{}", handler_var, name.name);
                            self.buf.push('(');
                            for (i, arg) in args.iter().enumerate() {
                                if i > 0 {
                                    self.buf.push_str(", ");
                                }
                                self.emit_expr(&arg.value)?;
                            }
                            self.buf.push(')');
                            return Ok(());
                        }
                    }
                }
                // Q-prim-assoc: a primitive associated-conversion call
                // (`Float.from(x)` / `Int.try_from(s)` / `String.from(c)`)
                // lowers to JS's native conversion, NOT the static-member form
                // below (`Float.from` is undefined on the host `number`).
                if self.try_emit_primitive_conversion(node, callee, args)? {
                    return Ok(());
                }
                // An associated-function call (`Type.method(args)` — stamped by
                // the lowerer, no `self` prepended) is a *static* method on the
                // class object. Emit `Type.method(args)` with the type name
                // preserved (JS class names are PascalCase, matching the Bock
                // type name); the generic fall-through would camel-case the
                // receiver identifier into a non-existent value (`typeValue`).
                if crate::generator::is_associated_call(node) {
                    if let NodeKind::FieldAccess { object, field } = &callee.kind {
                        if let NodeKind::Identifier { name: type_name } = &object.kind {
                            let _ = write!(
                                self.buf,
                                "{}.{}",
                                type_name.name,
                                self.js_method_name(&field.name)
                            );
                            self.buf.push('(');
                            for (i, arg) in args.iter().enumerate() {
                                if i > 0 {
                                    self.buf.push_str(", ");
                                }
                                self.emit_expr(&arg.value)?;
                            }
                            self.buf.push(')');
                            return Ok(());
                        }
                    }
                }
                // A trait/record method call lowers to `Call(FieldAccess(recv,
                // method), [recv, ...])` (the receiver is re-passed as `self`,
                // sharing the receiver's NodeId — see `desugared_self_call`).
                // When the method name collides with a field name, the *method*
                // was renamed at its declaration (`<name>Method`); rename the
                // call's member access to match so it resolves. A genuine field
                // *read* (bare `FieldAccess`, not in call position) and a
                // field-closure call `(p.f)(x)` (distinct receiver nodes) keep
                // the field name. Shared policy with go/ts/py. Unlike Python's
                // implicit `self`, JS prototype functions take an explicit `self`
                // param, so all `args` (the re-passed receiver included) are kept.
                if let NodeKind::FieldAccess { object, field } = &callee.kind {
                    if crate::generator::desugared_self_call(callee, args).is_some() {
                        // The generic fall-through emits the field name raw, so
                        // disambiguate against the raw name (matches the raw
                        // prototype attachment).
                        let renamed = self.js_method_name(&field.name);
                        if renamed != field.name {
                            self.emit_expr(object)?;
                            let _ = write!(self.buf, ".{renamed}");
                            self.buf.push('(');
                            for (i, arg) in args.iter().enumerate() {
                                if i > 0 {
                                    self.buf.push_str(", ");
                                }
                                self.emit_expr(&arg.value)?;
                            }
                            self.buf.push(')');
                            return Ok(());
                        }
                    }
                }
                // Pass handler args to effectful function calls.
                let effects_arg = if let NodeKind::Identifier { name } = &callee.kind {
                    self.build_effects_call_arg_js(&name.name)
                } else {
                    None
                };
                self.emit_callee(callee)?;
                self.buf.push('(');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(&arg.value)?;
                }
                if let Some(ea) = effects_arg {
                    if !args.is_empty() {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str(&ea);
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                if self.try_emit_time_method(receiver, &method.name, args)? {
                    return Ok(());
                }
                self.emit_expr(receiver)?;
                let _ = write!(
                    self.buf,
                    ".{}",
                    self.js_method_name(&to_camel_case(&method.name))
                );
                self.buf.push('(');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(&arg.value)?;
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::FieldAccess { object, field } => {
                self.emit_expr(object)?;
                let _ = write!(self.buf, ".{}", field.name);
                Ok(())
            }
            NodeKind::Index { object, index } => {
                self.emit_expr(object)?;
                self.buf.push('[');
                self.emit_expr(index)?;
                self.buf.push(']');
                Ok(())
            }
            NodeKind::Lambda { params, body } => {
                let param_names = self.collect_param_names(params);
                let _ = write!(self.buf, "({}) => ", param_names.join(", "));
                // If body is a block, emit with braces; otherwise inline.
                if matches!(body.kind, NodeKind::Block { .. }) {
                    self.buf.push_str("{\n");
                    self.indent += 1;
                    // A lambda body is a fresh function-body tail context: its
                    // tail is the lambda's return value, so clear any active
                    // `discard_tail` (e.g. from an enclosing loop body) for the
                    // duration of the body.
                    let prev = std::mem::replace(&mut self.discard_tail, false);
                    let r = self.emit_block_body(body);
                    self.discard_tail = prev;
                    r?;
                    self.indent -= 1;
                    self.write_indent();
                    self.buf.push('}');
                } else {
                    self.emit_expr(body)?;
                }
                Ok(())
            }
            NodeKind::Pipe { left, right } => {
                // Pipe `a |> f` → `f(a)`.
                // If right is a Call with a Placeholder, substitute left for it.
                self.emit_pipe(left, right)
            }
            NodeKind::Compose { left, right } => {
                // `f >> g` → `((x) => g(f(x)))`. A composed callee (`left`/`right`)
                // that is itself a `Compose`/`Lambda` must be parenthesized: a bare
                // arrow `(x) => …` followed by `(x)` parses as `(x) => (…(x))`,
                // binding the call to the arrow's body rather than invoking the
                // arrow. `emit_callee` wraps those forms. In practice the AIR lowers
                // `>>` to a `Lambda` before codegen (so chained `>>` reaches the
                // `Call` arm, not here), making this a defensive fall-through.
                let _ = write!(self.buf, "((x) => ");
                self.emit_callee(right)?;
                self.buf.push('(');
                self.emit_callee(left)?;
                self.buf.push_str("(x)))");
                Ok(())
            }
            NodeKind::Await { expr } => {
                self.buf.push_str("(await ");
                self.emit_expr(expr)?;
                self.buf.push(')');
                Ok(())
            }
            NodeKind::Propagate { expr } => {
                // `expr?` is desugared by the pre-statement `hoist_propagates`
                // pass into `const __tryN = <expr>; if (__tryN is failure) return
                // __tryN;`, which records `__tryN` for this node. Here the
                // operator evaluates to the unwrapped payload `__tryN._0`. If no
                // temp was recorded (a `?` in an un-hoisted position, e.g. inside
                // a short-circuited `&&` operand), fall back to evaluating the
                // inner expression — preserving the prior pass-through behavior
                // rather than emitting an undefined temp reference.
                if let Some(tmp) = self.propagate_temps.get(&(node as *const AIRNode as usize)) {
                    let _ = write!(self.buf, "{tmp}._0");
                    Ok(())
                } else {
                    self.emit_expr(expr)
                }
            }
            NodeKind::Range { lo, hi, inclusive } => {
                // No native range in JS; emit a helper call.
                if *inclusive {
                    self.buf.push_str("rangeInclusive(");
                } else {
                    self.buf.push_str("range(");
                }
                self.emit_expr(lo)?;
                self.buf.push_str(", ");
                self.emit_expr(hi)?;
                self.buf.push(')');
                Ok(())
            }
            NodeKind::RecordConstruct {
                path,
                fields,
                spread,
            } => {
                // A struct-variant construction (`Circle { radius: 2.0 }`) →
                // the `{enum}_{variant}(field, ..)` factory function, passing
                // field values in declaration order. Only fires for registered
                // user variants; plain records keep their object/class form.
                let struct_variant = if spread.is_none() {
                    self.user_variant_for_path(path).and_then(|info| {
                        if let crate::generator::VariantPayloadKind::Struct(field_order) =
                            &info.payload
                        {
                            Some((info.enum_name.clone(), field_order.clone()))
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                if let Some((enum_name, field_order)) = struct_variant {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    let _ = write!(self.buf, "{enum_name}_{variant}(");
                    for (i, fname) in field_order.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        // Emit the value supplied for this field, in the
                        // factory's parameter order (the decl order).
                        let supplied = fields.iter().find(|f| &f.name.name == fname);
                        match supplied.and_then(|f| f.value.as_ref()) {
                            Some(val) => self.emit_expr(val)?,
                            // Shorthand `{ radius }` ≡ `{ radius: radius }` — the
                            // RHS is a value reference, so escape like any ident.
                            None => self.buf.push_str(&js_value_ident(fname)),
                        }
                    }
                    self.buf.push(')');
                    return Ok(());
                }
                let type_name = path.segments.last().map(|s| s.name.as_str()).unwrap_or("");
                // A Bock `class` lowers to a *positional* `constructor(a, b)`
                // (unlike a record's destructured `constructor({ a, b })`), so a
                // class literal must construct as `new T(a_value, b_value)` with
                // values ordered by the *declared* field order — not the literal's
                // field order, and not a bare object literal (whose prototype
                // methods would be unreachable). Falls through to the
                // record/object path only when this is not a known class.
                if let Some(field_order) = self.class_fields.get(type_name).cloned() {
                    let _ = write!(self.buf, "new {type_name}(");
                    for (i, fname) in field_order.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        let supplied = fields.iter().find(|f| &f.name.name == fname);
                        match supplied.and_then(|f| f.value.as_ref()) {
                            Some(val) => self.emit_expr(val)?,
                            // A field present in the literal as shorthand
                            // (`T { label }` ≡ `T { label: label }`) — the RHS is
                            // a value reference; otherwise (field omitted, only
                            // possible with a `..base` spread) read it off `base`.
                            None if supplied.is_some() => {
                                self.buf.push_str(&js_value_ident(fname));
                            }
                            None => match spread {
                                Some(sp) => {
                                    self.emit_expr(sp)?;
                                    let _ = write!(self.buf, ".{}", js_value_ident(fname));
                                }
                                None => self.buf.push_str("undefined"),
                            },
                        }
                    }
                    self.buf.push(')');
                    return Ok(());
                }
                let is_class = self.record_names.contains(type_name);
                if is_class {
                    let _ = write!(self.buf, "new {type_name}(");
                    if fields.is_empty() && spread.is_none() {
                        self.buf.push(')');
                        return Ok(());
                    }
                }
                if let Some(sp) = spread {
                    self.buf.push_str("{ ...");
                    self.emit_expr(sp)?;
                    if !fields.is_empty() {
                        self.buf.push_str(", ");
                    }
                } else {
                    self.buf.push_str("{ ");
                }
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    if let Some(val) = &f.value {
                        let _ = write!(self.buf, "{}: ", f.name.name);
                        self.emit_expr(val)?;
                    } else {
                        // Shorthand: { name }
                        self.buf.push_str(&f.name.name);
                    }
                }
                self.buf.push_str(" }");
                if is_class {
                    self.buf.push(')');
                }
                Ok(())
            }
            NodeKind::ListLiteral { elems } => {
                self.buf.push('[');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(e)?;
                }
                self.buf.push(']');
                Ok(())
            }
            NodeKind::MapLiteral { entries } => {
                self.buf.push_str("new Map([");
                for (i, entry) in entries.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.buf.push('[');
                    self.emit_expr(&entry.key)?;
                    self.buf.push_str(", ");
                    self.emit_expr(&entry.value)?;
                    self.buf.push(']');
                }
                self.buf.push_str("])");
                Ok(())
            }
            NodeKind::SetLiteral { elems } => {
                self.buf.push_str("new Set([");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(e)?;
                }
                self.buf.push_str("])");
                Ok(())
            }
            NodeKind::TupleLiteral { elems } => {
                // JS doesn't have tuples; emit as an array.
                self.buf.push('[');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(e)?;
                }
                self.buf.push(']');
                Ok(())
            }
            NodeKind::Interpolation { parts } => {
                self.buf.push('`');
                for part in parts {
                    match part {
                        AirInterpolationPart::Literal(s) => {
                            self.buf.push_str(&escape_template_literal(s));
                        }
                        AirInterpolationPart::Expr(expr) => {
                            self.buf.push_str("${");
                            self.emit_expr(expr)?;
                            self.buf.push('}');
                        }
                    }
                }
                self.buf.push('`');
                Ok(())
            }
            NodeKind::Placeholder => {
                self.buf.push('_');
                Ok(())
            }
            NodeKind::Unreachable => {
                self.buf
                    .push_str("(() => { throw new Error(\"unreachable\"); })()");
                Ok(())
            }
            NodeKind::ResultConstruct { variant, value } => {
                // Use the `_0` payload key — the same shape the surface
                // `Ok(..)`/`Err(..)` construction (`try_emit_prelude_ctor`) emits
                // and the `Result` match reads — so construction and match agree
                // (the old `value`/`error` keys were never read by the match).
                let tag = match variant {
                    ResultVariant::Ok => "Ok",
                    ResultVariant::Err => "Err",
                };
                let _ = write!(self.buf, "{{ _tag: \"{tag}\", _0: ");
                if let Some(v) = value {
                    self.emit_expr(v)?;
                } else {
                    self.buf.push_str("undefined");
                }
                self.buf.push_str(" }");
                Ok(())
            }
            NodeKind::Assign { op, target, value } => {
                self.emit_expr(target)?;
                let op_str = match op {
                    AssignOp::Assign => " = ",
                    AssignOp::AddAssign => " += ",
                    AssignOp::SubAssign => " -= ",
                    AssignOp::MulAssign => " *= ",
                    AssignOp::DivAssign => " /= ",
                    AssignOp::RemAssign => " %= ",
                };
                self.buf.push_str(op_str);
                self.emit_expr(value)?;
                Ok(())
            }
            NodeKind::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                // Ternary for expression-position if.
                self.buf.push('(');
                self.emit_expr(condition)?;
                self.buf.push_str(" ? ");
                self.emit_block_as_expr(then_block)?;
                self.buf.push_str(" : ");
                if let Some(eb) = else_block {
                    self.emit_block_as_expr(eb)?;
                } else {
                    self.buf.push_str("undefined");
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::Block { stmts, tail } => {
                // Blocks in expression position → IIFE. The IIFE body is its own
                // JS lexical scope, so it gets its own `let` scope frame — a name
                // re-bound across two *sibling* IIFEs (e.g. two arms of an
                // expression-position `match`) is two independent declarations,
                // not a redeclaration.
                self.buf.push_str("(() => {\n");
                self.indent += 1;
                self.enter_let_scope(node);
                for s in stmts {
                    self.emit_node(s)?;
                }
                if let Some(t) = tail {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}return ");
                    self.emit_expr(t)?;
                    self.buf.push_str(";\n");
                }
                self.leave_let_scope();
                self.indent -= 1;
                self.write_indent();
                self.buf.push_str("})()");
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => {
                // Match in expression position → IIFE with switch. The IIFE
                // arrow returns the matched arm's value, so the arm bodies must
                // `return` their tail — clear any active `discard_tail` (e.g. a
                // statement-position context such as an enclosing loop body or a
                // non-tail statement) for the IIFE body, restored after.
                self.buf.push_str("(() => {\n");
                self.indent += 1;
                let prev = std::mem::replace(&mut self.discard_tail, false);
                let r = self.emit_match(scrutinee, arms);
                self.discard_tail = prev;
                r?;
                self.indent -= 1;
                self.write_indent();
                self.buf.push_str("})()");
                Ok(())
            }
            // Ownership nodes: erase in JS.
            NodeKind::Move { expr }
            | NodeKind::Borrow { expr }
            | NodeKind::MutableBorrow { expr } => self.emit_expr(expr),
            // Effect operation invocation.
            NodeKind::EffectOp {
                effect,
                operation,
                args,
            } => {
                let effect_name = effect.segments.last().map_or("effect", |s| s.name.as_str());
                let _ = write!(
                    self.buf,
                    "{}.{}",
                    to_camel_case(effect_name),
                    operation.name
                );
                self.buf.push('(');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(&arg.value)?;
                }
                self.buf.push(')');
                Ok(())
            }
            // Type expressions: erased in JS.
            NodeKind::TypeNamed { .. }
            | NodeKind::TypeTuple { .. }
            | NodeKind::TypeFunction { .. }
            | NodeKind::TypeOptional { .. }
            | NodeKind::TypeSelf => {
                self.buf.push_str("/* type */");
                Ok(())
            }
            // EffectRef in expression position:
            NodeKind::EffectRef { path } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                self.buf.push_str(&name);
                Ok(())
            }
            // Error node:
            NodeKind::Error => {
                self.buf.push_str("/* error */");
                Ok(())
            }
            // Fallback for any other node kinds appearing in expression position.
            _ => {
                self.buf.push_str("/* unsupported */");
                Ok(())
            }
        }
    }

    // ── Match → switch ──────────────────────────────────────────────────────

    /// Emit a JS label before a loop iff a contained statement-arm `match`
    /// needs to `break`/`continue` the loop (JS `break` otherwise exits the
    /// inner `switch`). Pair with `self.loop_labels.pop()` after the loop body.
    fn emit_loop_label_prefix(&mut self, body: &AIRNode) {
        if crate::generator::loop_needs_break_label(body) {
            self.loop_label_counter += 1;
            let label = format!("__bockLoop{}", self.loop_label_counter);
            self.writeln(&format!("{label}:"));
            self.loop_labels.push(Some(label));
        } else {
            self.loop_labels.push(None);
        }
    }

    /// Label of the innermost loop, if one was allocated.
    fn innermost_loop_label(&self) -> Option<&str> {
        self.loop_labels.last().and_then(|l| l.as_deref())
    }

    fn emit_match(&mut self, scrutinee: &AIRNode, arms: &[AIRNode]) -> Result<(), CodegenError> {
        // Guards, or-patterns, tuple patterns, and nested constructor/record
        // patterns cannot be expressed by the flat `switch` below (a failed
        // guard's `break` exits the switch instead of falling through; an
        // or-pattern collapses to a single `default:`; a tuple has no single
        // discriminant; a nested sub-pattern's bindings are lost). Lower those
        // to an if/else-if chain instead. Additive: the proven Optional /
        // Result / user-enum / value `switch` fast-path is kept for everything
        // else (see `match_needs_ifchain`).
        if crate::generator::match_needs_ifchain(arms) || match_has_unswitchable_pattern(arms) {
            // List patterns (`[]`, `[first, ..rest]`) and range patterns
            // (`1..10`) have no single switch discriminant — every arm would
            // collapse to a `default:`, emitting more than one `default` clause
            // (a JS `SyntaxError`). The shared `match_needs_ifchain`
            // (generator.rs) does not yet recognise these, so the js emitter
            // routes them to the if/else-if chain itself. See OPEN in the PR
            // body for the shared-side gap.
            return self.emit_match_ifchain(scrutinee, arms);
        }

        // A tag-based (ADT) match dispatches on `._tag`. This is true when any
        // arm is a constructor pattern (`Some(x)`, `Rect(w, h)`) *or* a record
        // pattern whose path is a registered enum variant (`Circle { radius }`).
        // The latter is the case the prior `ConstructorPat`-only check missed:
        // a struct-payload variant lowers to a `RecordPat`, so an all-struct
        // match never set `is_adt` and every arm fell to `default:` (DV14).
        let is_adt = arms.iter().any(|arm| {
            let NodeKind::MatchArm { pattern, .. } = &arm.kind else {
                return false;
            };
            match &pattern.kind {
                NodeKind::ConstructorPat { .. } => true,
                NodeKind::RecordPat { path, .. } => self.user_variant_for_path(path).is_some(),
                _ => false,
            }
        });

        // Hoist a non-trivial scrutinee into a single `const __matchN = …;` so it
        // is evaluated once (re-emitting it in every arm double-evaluated it — a
        // real bug for a scrutinee with side effects). A bare identifier is
        // already a stable reference, so leave it inline.
        let temp = if matches!(scrutinee.kind, NodeKind::Identifier { .. }) {
            None
        } else {
            self.match_temp_counter += 1;
            let name = format!("__match{}", self.match_temp_counter);
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}const {name} = ");
            self.emit_expr(scrutinee)?;
            self.buf.push_str(";\n");
            Some(name)
        };

        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}switch (");
        self.emit_scrutinee_ref(scrutinee, temp.as_deref())?;
        if is_adt {
            self.buf.push_str("._tag) {\n");
        } else {
            self.buf.push_str(") {\n");
        }
        self.indent += 1;
        self.switch_label_depth += 1;
        for arm in arms {
            self.emit_match_arm(arm, is_adt, scrutinee, temp.as_deref())?;
        }
        self.switch_label_depth -= 1;
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    /// Emit a reference to the match scrutinee: the hoisted temp name when one
    /// was introduced, else the scrutinee expression inline (a bare identifier).
    fn emit_scrutinee_ref(
        &mut self,
        scrutinee: &AIRNode,
        temp: Option<&str>,
    ) -> Result<(), CodegenError> {
        match temp {
            Some(name) => {
                self.buf.push_str(name);
                Ok(())
            }
            None => self.emit_expr(scrutinee),
        }
    }

    fn emit_match_arm(
        &mut self,
        arm: &AIRNode,
        is_adt: bool,
        scrutinee: &AIRNode,
        temp: Option<&str>,
    ) -> Result<(), CodegenError> {
        if let NodeKind::MatchArm {
            pattern,
            guard,
            body,
        } = &arm.kind
        {
            match &pattern.kind {
                NodeKind::WildcardPat => {
                    self.writeln("default: {");
                }
                NodeKind::BindPat { name, is_mut } if !is_adt => {
                    // Bind pattern as default with variable binding. A `mut x`
                    // arm binding may be reassigned in the body (`x = x + 1`),
                    // so it must be declared `let`, not `const`.
                    self.writeln("default: {");
                    self.indent += 1;
                    let kw = if *is_mut { "let" } else { "const" };
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}{kw} {} = ", js_value_ident(&name.name));
                    self.emit_scrutinee_ref(scrutinee, temp)?;
                    self.buf.push_str(";\n");
                    self.indent -= 1;
                }
                NodeKind::LiteralPat { lit } => {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}case ");
                    match lit {
                        Literal::Int(s) => self.buf.push_str(s),
                        Literal::Float(s) => self.buf.push_str(s),
                        Literal::Bool(b) => self.buf.push_str(if *b { "true" } else { "false" }),
                        Literal::Char(s) => {
                            self.buf.push('\'');
                            self.buf.push_str(s);
                            self.buf.push('\'');
                        }
                        Literal::String(s) => {
                            self.buf.push('"');
                            self.buf.push_str(&escape_js_string(s));
                            self.buf.push('"');
                        }
                        Literal::Unit => self.buf.push_str("undefined"),
                    }
                    self.buf.push_str(": {\n");
                }
                NodeKind::ConstructorPat { path, fields } => {
                    let variant_name = path.segments.last().map_or("_", |s| s.name.as_str());
                    self.writeln(&format!("case \"{variant_name}\": {{"));
                    // Destructure fields from the scrutinee.
                    if !fields.is_empty() {
                        self.indent += 1;
                        for (i, field) in fields.iter().enumerate() {
                            let binding = self.pattern_to_binding_name(field);
                            let ind = self.indent_str();
                            let _ = write!(self.buf, "{ind}const {binding} = ");
                            self.emit_scrutinee_ref(scrutinee, temp)?;
                            let _ = writeln!(self.buf, "._{i};");
                        }
                        self.indent -= 1;
                    }
                }
                NodeKind::RecordPat { path, fields, .. } => {
                    let variant_name = path.segments.last().map_or("_", |s| s.name.as_str());
                    if is_adt {
                        self.writeln(&format!("case \"{variant_name}\": {{"));
                    } else {
                        self.writeln("default: {");
                    }
                    if !fields.is_empty() {
                        self.indent += 1;
                        for f in fields {
                            let field_name = &f.name.name;
                            if let Some(pat) = &f.pattern {
                                let binding = self.pattern_to_binding_name(pat);
                                let ind = self.indent_str();
                                let _ = write!(self.buf, "{ind}const {binding} = ");
                                self.emit_scrutinee_ref(scrutinee, temp)?;
                                let _ = writeln!(self.buf, ".{field_name};");
                            } else {
                                let ind = self.indent_str();
                                let _ = write!(self.buf, "{ind}const {field_name} = ");
                                self.emit_scrutinee_ref(scrutinee, temp)?;
                                let _ = writeln!(self.buf, ".{field_name};");
                            }
                        }
                        self.indent -= 1;
                    }
                }
                _ => {
                    // Fallback: emit as default case.
                    self.writeln("default: {");
                }
            }

            self.indent += 1;
            if let Some(g) = guard {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}if (!(");
                self.emit_expr(g)?;
                self.buf.push_str(")) break;\n");
            }
            self.emit_block_body(body)?;
            self.writeln("break;");
            self.indent -= 1;
            self.writeln("}");
        }
        Ok(())
    }

    // ── Match → if/else-if chain (guards, or-/tuple/nested patterns) ──────────

    /// Lower a `match` whose arms cannot be expressed by a flat `switch` (see
    /// [`crate::generator::match_needs_ifchain`]) to an `if (<test>) { <binds>;
    /// <body> } else if …` chain.
    ///
    /// The scrutinee is evaluated once into `__matchN` (a non-identifier
    /// scrutinee would otherwise be re-evaluated in every arm's test). Each arm
    /// contributes one `if`/`else if`; a catch-all pattern (wildcard or bare
    /// bind) with no guard becomes a plain `else`. Failed guards therefore fall
    /// through to the next `else if`, which is the semantics a `switch` could
    /// not express. Bock matches are exhaustive, so a chain with no `else` is
    /// safe.
    fn emit_match_ifchain(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        // Single-evaluation: a bare identifier is already stable, so reuse it as
        // the access root; anything else is hoisted into `__matchN`.
        let root: String = if let NodeKind::Identifier { name } = &scrutinee.kind {
            name.name.clone()
        } else {
            self.match_temp_counter += 1;
            let name = format!("__match{}", self.match_temp_counter);
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}const {name} = ");
            self.emit_expr(scrutinee)?;
            self.buf.push_str(";\n");
            name
        };

        let mut first = true;
        let mut closed = false; // an unconditional `else` (or bare block) ended the chain
        let arm_count = arms.len();
        for (idx, arm) in arms.iter().enumerate() {
            let NodeKind::MatchArm {
                pattern,
                guard,
                body,
            } = &arm.kind
            else {
                continue;
            };
            let test = self.pattern_test_js(pattern, &root);
            let is_catch_all = matches!(
                pattern.kind,
                NodeKind::WildcardPat | NodeKind::BindPat { .. }
            );
            let is_last = idx + 1 == arm_count;
            // An unconditional `else` is emitted when the arm cannot fail to
            // match at its position: a catch-all (wildcard / bare bind) with no
            // guard, or the final arm with no guard (Bock matches are
            // exhaustive, so the last unguarded arm is guaranteed reached). This
            // also closes the chain so a value-returning function typechecks.
            let unconditional = guard.is_none() && (is_catch_all || is_last);
            let ind = self.indent_str();
            if unconditional {
                if first {
                    // No preceding `if`: emit a bare block so bindings/body run.
                    let _ = writeln!(self.buf, "{ind}{{");
                } else {
                    let _ = writeln!(self.buf, "{ind}else {{");
                }
                closed = true;
            } else {
                let mut cond = if test.is_empty() {
                    "true".to_string()
                } else {
                    test
                };
                if let Some(g) = guard {
                    // The guard may reference the arm's pattern bindings (`x if
                    // (x > 0)`). Those bindings are introduced *inside* the arm
                    // body, so they are not in scope in the surrounding `else
                    // if` condition. Evaluate the guard in an arrow-IIFE that
                    // first re-introduces the bindings, so a failed guard still
                    // falls through to the next `else if` (the fall-through a
                    // `switch` could not express).
                    let g_str = self.expr_to_string(g)?;
                    let binds = self.pattern_binds_to_string_js(pattern, &root);
                    let guard_test = if binds.is_empty() {
                        format!("({g_str})")
                    } else {
                        format!("(() => {{ {binds}return ({g_str}); }})()")
                    };
                    if cond == "true" {
                        cond = guard_test;
                    } else {
                        cond = format!("{cond} && {guard_test}");
                    }
                }
                if first {
                    let _ = writeln!(self.buf, "{ind}if ({cond}) {{");
                } else {
                    let _ = writeln!(self.buf, "{ind}else if ({cond}) {{");
                }
            }
            first = false;
            self.indent += 1;
            self.pattern_binds_js(pattern, &root)?;
            self.emit_block_body(body)?;
            self.indent -= 1;
            self.writeln("}");
        }
        // If every arm was conditional (all guarded, or no catch-all), close the
        // chain with a throw so a value-returning function still typechecks and
        // a genuinely unmatched scrutinee fails loudly rather than silently.
        if !closed && !first {
            self.writeln("else { throw new Error(\"non-exhaustive match\"); }");
        }
        Ok(())
    }

    /// Build the boolean test that selects `pat` against the JS expression
    /// `access`. Returns the empty string for a pattern that always matches (a
    /// wildcard or bare bind), so the caller can render it as an `else`.
    fn pattern_test_js(&self, pat: &AIRNode, access: &str) -> String {
        match &pat.kind {
            NodeKind::WildcardPat | NodeKind::BindPat { .. } => String::new(),
            NodeKind::LiteralPat { lit } => {
                format!("{access} === {}", js_literal(lit))
            }
            NodeKind::ConstructorPat { path, fields } => {
                let variant = path.segments.last().map_or("_", |s| s.name.as_str());
                let mut tests = vec![format!("{access}._tag === \"{variant}\"")];
                for (i, field) in fields.iter().enumerate() {
                    let sub = self.pattern_test_js(field, &format!("{access}._{i}"));
                    if !sub.is_empty() {
                        tests.push(sub);
                    }
                }
                tests.join(" && ")
            }
            NodeKind::RecordPat { path, fields, .. } => {
                let variant = path.segments.last().map_or("_", |s| s.name.as_str());
                // A registered enum variant dispatches on `._tag`; a plain record
                // (not an enum variant) has no tag, so only its field sub-tests
                // apply.
                let mut tests = Vec::new();
                if self.user_variant_for_path(path).is_some() {
                    tests.push(format!("{access}._tag === \"{variant}\""));
                }
                for f in fields {
                    if let Some(p) = &f.pattern {
                        let sub = self.pattern_test_js(p, &format!("{access}.{}", f.name.name));
                        if !sub.is_empty() {
                            tests.push(sub);
                        }
                    }
                }
                if tests.is_empty() {
                    String::new()
                } else {
                    tests.join(" && ")
                }
            }
            NodeKind::TuplePat { elems } => {
                let mut tests = vec![format!("Array.isArray({access})")];
                for (i, e) in elems.iter().enumerate() {
                    let sub = self.pattern_test_js(e, &format!("{access}[{i}]"));
                    if !sub.is_empty() {
                        tests.push(sub);
                    }
                }
                tests.join(" && ")
            }
            NodeKind::ListPat { elems, rest } => {
                // `[a, b]` requires an array of exactly len(elems); `[a, ..rest]`
                // requires at least len(elems). Element sub-patterns are tested
                // positionally; the rest binds the slice and adds no test.
                let n = elems.len();
                let len_test = if rest.is_some() {
                    format!("{access}.length >= {n}")
                } else {
                    format!("{access}.length === {n}")
                };
                let mut tests = vec![format!("Array.isArray({access})"), len_test];
                for (i, e) in elems.iter().enumerate() {
                    let sub = self.pattern_test_js(e, &format!("{access}[{i}]"));
                    if !sub.is_empty() {
                        tests.push(sub);
                    }
                }
                tests.join(" && ")
            }
            NodeKind::RangePat { lo, hi, inclusive } => {
                // `lo..hi` → `access >= lo && access < hi`; `lo..=hi` uses `<=`.
                let lo_s = range_bound_to_js(lo);
                let hi_s = range_bound_to_js(hi);
                let upper = if *inclusive { "<=" } else { "<" };
                format!("{access} >= {lo_s} && {access} {upper} {hi_s}")
            }
            NodeKind::OrPat { alternatives } => {
                let alts: Vec<String> = alternatives
                    .iter()
                    .map(|a| {
                        let t = self.pattern_test_js(a, access);
                        if t.is_empty() {
                            "true".to_string()
                        } else {
                            format!("({t})")
                        }
                    })
                    .collect();
                alts.join(" || ")
            }
            _ => String::new(),
        }
    }

    /// Emit the `const <name> = <access…>;` bindings introduced by `pat`,
    /// recursing into nested constructor / record / tuple sub-patterns. An
    /// or-pattern binds against its first alternative (all alternatives bind the
    /// same names by Bock's rules).
    fn pattern_binds_js(&mut self, pat: &AIRNode, access: &str) -> Result<(), CodegenError> {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => {
                let js = js_value_ident(&name.name);
                // Skip a self-binding (`const n = n`): when an arm's bind name
                // equals the scrutinee access (e.g. `match n { n if … }`), the
                // name already refers to the value. Emitting `const n = n` is
                // both redundant and a `let`/`const` TDZ self-reference error.
                if js != access {
                    let ind = self.indent_str();
                    let _ = writeln!(self.buf, "{ind}const {js} = {access};");
                }
            }
            NodeKind::ConstructorPat { fields, .. } => {
                for (i, field) in fields.iter().enumerate() {
                    self.pattern_binds_js(field, &format!("{access}._{i}"))?;
                }
            }
            NodeKind::RecordPat { fields, .. } => {
                for f in fields {
                    let field_access = format!("{access}.{}", f.name.name);
                    match &f.pattern {
                        Some(p) => self.pattern_binds_js(p, &field_access)?,
                        // Shorthand `{ radius }` binds `radius` to `<access>.radius`.
                        None => {
                            let ind = self.indent_str();
                            let _ =
                                writeln!(self.buf, "{ind}const {} = {field_access};", f.name.name);
                        }
                    }
                }
            }
            NodeKind::TuplePat { elems } => {
                for (i, e) in elems.iter().enumerate() {
                    self.pattern_binds_js(e, &format!("{access}[{i}]"))?;
                }
            }
            NodeKind::ListPat { elems, rest } => {
                for (i, e) in elems.iter().enumerate() {
                    self.pattern_binds_js(e, &format!("{access}[{i}]"))?;
                }
                // `..rest` binds the remaining elements as a slice; a bare `..`
                // (RestPat) or absent rest binds nothing.
                if let Some(r) = rest {
                    if let NodeKind::BindPat { name, .. } = &r.kind {
                        let ind = self.indent_str();
                        let _ = writeln!(
                            self.buf,
                            "{ind}const {} = {access}.slice({});",
                            js_value_ident(&name.name),
                            elems.len()
                        );
                    }
                }
            }
            NodeKind::OrPat { alternatives } => {
                if let Some(first) = alternatives.first() {
                    self.pattern_binds_js(first, access)?;
                }
            }
            // Wildcard / literal: nothing to bind.
            _ => {}
        }
        Ok(())
    }

    /// Collect the bindings introduced by `pat` as a single-line string of
    /// `const … = …; ` statements (used to re-introduce them inside the
    /// guard-evaluating IIFE — see [`Self::emit_match_ifchain`]).
    fn pattern_binds_to_string_js(&self, pat: &AIRNode, access: &str) -> String {
        let mut out = String::new();
        self.collect_binds_js(pat, access, &mut out);
        out
    }

    fn collect_binds_js(&self, pat: &AIRNode, access: &str, out: &mut String) {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => {
                let js = js_value_ident(&name.name);
                // Skip a self-binding (`const n = n`) — redundant and a TDZ
                // error inside the guard-evaluating IIFE. See `pattern_binds_js`.
                if js != access {
                    let _ = write!(out, "const {js} = {access}; ");
                }
            }
            NodeKind::ConstructorPat { fields, .. } => {
                for (i, field) in fields.iter().enumerate() {
                    self.collect_binds_js(field, &format!("{access}._{i}"), out);
                }
            }
            NodeKind::RecordPat { fields, .. } => {
                for f in fields {
                    let field_access = format!("{access}.{}", f.name.name);
                    match &f.pattern {
                        Some(p) => self.collect_binds_js(p, &field_access, out),
                        None => {
                            let _ = write!(out, "const {} = {field_access}; ", f.name.name);
                        }
                    }
                }
            }
            NodeKind::TuplePat { elems } => {
                for (i, e) in elems.iter().enumerate() {
                    self.collect_binds_js(e, &format!("{access}[{i}]"), out);
                }
            }
            NodeKind::ListPat { elems, rest } => {
                for (i, e) in elems.iter().enumerate() {
                    self.collect_binds_js(e, &format!("{access}[{i}]"), out);
                }
                if let Some(r) = rest {
                    if let NodeKind::BindPat { name, .. } = &r.kind {
                        let _ = write!(
                            out,
                            "const {} = {access}.slice({}); ",
                            js_value_ident(&name.name),
                            elems.len()
                        );
                    }
                }
            }
            NodeKind::OrPat { alternatives } => {
                if let Some(first) = alternatives.first() {
                    self.collect_binds_js(first, access, out);
                }
            }
            _ => {}
        }
    }

    // ── Pipe operator ───────────────────────────────────────────────────────

    fn emit_pipe(&mut self, left: &AIRNode, right: &AIRNode) -> Result<(), CodegenError> {
        // `left |> right` → `right(left)`
        // If right is a Call with Placeholder, substitute left for it.
        if let NodeKind::Call { callee, args, .. } = &right.kind {
            let has_placeholder = args
                .iter()
                .any(|a| matches!(a.value.kind, NodeKind::Placeholder));
            if has_placeholder {
                self.emit_expr(callee)?;
                self.buf.push('(');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    if matches!(arg.value.kind, NodeKind::Placeholder) {
                        self.emit_expr(left)?;
                    } else {
                        self.emit_expr(&arg.value)?;
                    }
                }
                self.buf.push(')');
                return Ok(());
            }
        }
        // Simple case: `right(left)`. `right` is a callee, so parenthesize it
        // when it is a bare arrow (`Lambda`/`Compose`) — otherwise the trailing
        // `(left)` binds to the arrow body instead of invoking it.
        self.emit_callee(right)?;
        self.buf.push('(');
        self.emit_expr(left)?;
        self.buf.push(')');
        Ok(())
    }

    /// Emit an expression in **callee** position, parenthesizing it when its
    /// surface syntax would otherwise swallow the trailing argument list.
    ///
    /// The case that matters is a bare arrow callee: `(x) => body` followed by
    /// `(arg)` parses in JS as `(x) => (body(arg))` — the call binds to the body,
    /// never invoking the arrow. Wrapping it as `((x) => body)(arg)` makes the
    /// call apply to the arrow itself. This arises when the AIR compose desugar
    /// (`f >> g` → `(__compose_x) => g(f(__compose_x))`) **nests**: a chained
    /// `>>` lowers the inner compose to a `Lambda` (or a `Compose` still awaiting
    /// lowering), which then appears as the callee `f`/`g` inside the call.
    /// Mirrors the python (`emit_callee`) and rust (`emit_callee_rs`) backends.
    fn emit_callee(&mut self, callee: &AIRNode) -> Result<(), CodegenError> {
        if matches!(
            callee.kind,
            NodeKind::Lambda { .. } | NodeKind::Compose { .. }
        ) {
            self.buf.push('(');
            self.emit_expr(callee)?;
            self.buf.push(')');
            Ok(())
        } else {
            self.emit_expr(callee)
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    /// Returns true if `js_name` has already been declared in the innermost
    /// `let` scope, so a further binding of it must be a plain assignment rather
    /// than a `const`/`let` re-declaration (which JS rejects).
    fn simple_let_redeclared(&self, js_name: &str) -> bool {
        self.let_scopes
            .last()
            .is_some_and(|s| s.declared.contains(js_name))
    }

    /// Returns true if `js_name` is re-bound or assigned later in its block, so
    /// its first declaration must use `let` (not `const`) to allow reassignment.
    fn simple_let_needs_let(&self, js_name: &str) -> bool {
        self.let_scopes
            .last()
            .is_some_and(|s| s.needs_let.contains(js_name))
    }

    /// Record that `js_name` has now been declared in the innermost `let` scope.
    fn mark_simple_let_declared(&mut self, js_name: &str) {
        if let Some(s) = self.let_scopes.last_mut() {
            s.declared.insert(js_name.to_string());
        }
    }

    /// Push a fresh `let` scope for a JS block, pre-scanning `block`'s direct
    /// statements to find which simple `let`-bound names are re-bound or
    /// assigned within the block (so their first declaration emits `let`). Only
    /// the block's own statements are scanned — nested blocks open their own
    /// scopes, so a name re-bound only in a nested block does not force `let`
    /// here. Returns the depth to which [`Self::leave_let_scope`] should unwind.
    fn enter_let_scope(&mut self, block: &AIRNode) {
        let mut needs_let = HashSet::new();
        if let NodeKind::Block { stmts, tail } = &block.kind {
            let mut seen: HashSet<String> = HashSet::new();
            let mut visit = |n: &AIRNode, needs_let: &mut HashSet<String>| {
                match &n.kind {
                    NodeKind::LetBinding { pattern, .. } => {
                        if let NodeKind::BindPat { name, .. } = &pattern.kind {
                            let js = js_value_ident(&name.name);
                            // A re-binding of an already-seen name needs `let`.
                            if !seen.insert(js.clone()) {
                                needs_let.insert(js);
                            }
                        }
                    }
                    NodeKind::Assign { target, .. } => {
                        if let NodeKind::Identifier { name } = &target.kind {
                            needs_let.insert(js_value_ident(&name.name));
                        }
                    }
                    _ => {}
                }
            };
            for s in stmts {
                visit(s, &mut needs_let);
            }
            if let Some(t) = tail {
                visit(t, &mut needs_let);
            }
        }
        self.let_scopes.push(LetScope {
            declared: HashSet::new(),
            needs_let,
        });
    }

    /// Pop the innermost `let` scope pushed by [`Self::enter_let_scope`].
    fn leave_let_scope(&mut self) {
        self.let_scopes.pop();
    }

    fn emit_block_body(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.enter_let_scope(node);
        let r = self.emit_block_body_inner(node);
        self.leave_let_scope();
        r
    }

    /// Emit a **loop body** (`for`/`while`/`loop`). A loop body is statement
    /// position: its tail expression is discarded (a Bock loop evaluates to
    /// Unit; the body's value is not the function's value). The default
    /// [`Self::emit_block_body`] treats a tail as a function-body return, which
    /// for a loop body emits `return console.log(i);` — aborting the function on
    /// the first iteration (the loop runs once, then the fn exits). Setting
    /// [`Self::discard_tail`] for the body's duration routes the tail to a bare
    /// expression statement instead. The flag is saved/restored so it never
    /// leaks past the loop, and any nested lambda / value-position IIFE clears
    /// it (their tail is genuinely returned). A `break v` value still flows
    /// through the separate `/* break value */` path, not this discard flag.
    fn emit_loop_body(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        let prev = std::mem::replace(&mut self.discard_tail, true);
        let r = self.emit_block_body(node);
        self.discard_tail = prev;
        r
    }

    /// Lower every `?` (`Propagate`) reachable in `stmt`'s own evaluation into a
    /// pre-statement hoist, then record the unwrapped-payload temp so the
    /// `Propagate` arm of [`Self::emit_expr`] substitutes `<temp>._0` in place.
    ///
    /// `expr?` is a value-position operator that, on the failure tag (`Err` /
    /// `None`), must **early-return** the wrapped value from the enclosing
    /// function — which JS cannot express inside an arbitrary sub-expression (an
    /// IIFE's `return` would only exit the IIFE). So for each `?` we emit, before
    /// the consuming statement:
    /// ```js
    /// const __tryN = <inner>;
    /// if (__tryN._tag === "Err" || __tryN._tag === "None") return __tryN;
    /// ```
    /// and the operator itself then evaluates to `__tryN._0`. The failure-tag
    /// test (`Err`/`None`) covers both `Result` and `Optional`, which share the
    /// `{ _tag, _0 }` tagged-object representation; the `Propagate` node carries
    /// no type annotation to distinguish them, so the test keys off the failure
    /// tags rather than the success tag.
    ///
    /// The walk visits in evaluation (innermost-first) order and stops at scope
    /// boundaries — `lambda`/`block` bodies and the branch bodies of
    /// `if`/`match`/`loop` — because a `?` inside those belongs to that nested
    /// statement scope and is hoisted when *it* is emitted. It also does not
    /// descend the short-circuited operand of `&&`/`||` (whose evaluation is
    /// conditional). Returns whether any `?` was hoisted.
    fn hoist_propagates(&mut self, stmt: &AIRNode) -> Result<bool, CodegenError> {
        let mut found: Vec<&AIRNode> = Vec::new();
        collect_propagates_in_expr(stmt, &mut found);
        let any = !found.is_empty();
        for prop in found {
            if let NodeKind::Propagate { expr } = &prop.kind {
                let n = self.propagate_temp_counter;
                self.propagate_temp_counter += 1;
                let tmp = format!("__try{n}");
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}const {tmp} = ");
                // `expr` may itself contain `?` already hoisted above (nested
                // `g(f(x)?)?`); those temps are registered, so emit substitutes.
                self.emit_expr(expr)?;
                self.buf.push_str(";\n");
                let _ = writeln!(
                    self.buf,
                    "{ind}if ({tmp}._tag === \"Err\" || {tmp}._tag === \"None\") return {tmp};"
                );
                self.propagate_temps
                    .insert(prop as *const AIRNode as usize, tmp);
            }
        }
        Ok(any)
    }

    /// Emit a function/method body whose top-level `let` scope is pre-seeded with
    /// the function's `params` as already-declared names. A Bock `let x = …` that
    /// shadows a parameter `x` is the same block scope as the JS parameter, so it
    /// must lower to a plain assignment (`x = …`) rather than a `let`/`const`
    /// redeclaration (which JS rejects). Used by [`Self::emit_fn_decl`] /
    /// [`Self::emit_class_method`] in place of [`Self::emit_block_body`].
    fn emit_fn_body_seeded(
        &mut self,
        params: &[AIRNode],
        body: &AIRNode,
    ) -> Result<(), CodegenError> {
        self.enter_let_scope(body);
        if let Some(scope) = self.let_scopes.last_mut() {
            for p in params {
                if let NodeKind::Param { pattern, .. } = &p.kind {
                    if let NodeKind::BindPat { name, .. } = &pattern.kind {
                        let js = js_value_ident(&name.name);
                        scope.needs_let.insert(js.clone());
                        scope.declared.insert(js);
                    }
                }
            }
        }
        let r = self.emit_block_body_inner(body);
        self.leave_let_scope();
        r
    }

    fn emit_block_body_inner(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::Block { stmts, tail } = &node.kind {
            // Every non-tail statement is statement position: its value is
            // discarded. A statement-position `if`/`match` whose branch/arm body
            // ends in an expression (e.g. `println(...)`) must NOT `return` that
            // value — doing so aborts the function before the statements after
            // the `if`/`match` run. Activate `discard_tail` for the non-tail
            // statements (restored before the tail, which keeps function-body
            // return semantics). A nested loop/lambda overrides this within its
            // own body, so the discard applies only to the immediate
            // statement-position control flow.
            let prev_discard = std::mem::replace(&mut self.discard_tail, true);
            let mut stmt_res = Ok(());
            for s in stmts {
                stmt_res = self.emit_node(s);
                if stmt_res.is_err() {
                    break;
                }
            }
            self.discard_tail = prev_discard;
            stmt_res?;
            if let Some(t) = tail {
                // A statement tail (`break`/`continue`/`return`/assignment) is
                // emitted as a statement, never wrapped in `return`.
                if crate::generator::node_is_statement(t) {
                    self.emit_node(t)?;
                    return Ok(());
                }
                // A loop / while / for / guard / handling-block in tail position
                // has no JS expression form (its value, if any, was already
                // hoisted into a preceding temp by the shared value-CF pre-pass,
                // leaving a value-less construct here). Emit it as a statement —
                // `return while (…)` / `return /* unsupported */` is what the
                // fall-through `return <expr>` would otherwise produce.
                if tail_is_statement_form(t) {
                    self.emit_node(t)?;
                    return Ok(());
                }
                // A `match` with statement arms yields no value: emit it as a
                // statement `switch`, not as an IIFE.
                if let NodeKind::Match { scrutinee, arms } = &t.kind {
                    if crate::generator::match_has_statement_arm(arms) {
                        self.emit_match(scrutinee, arms)?;
                        return Ok(());
                    }
                }
                // A diverging-intrinsic tail (`todo()`/`unreachable()`) lowers to
                // a bare `throw` statement; `return throw …` is invalid JS, so
                // emit it as a statement.
                if js_call_is_diverging(t) {
                    self.write_indent();
                    self.emit_expr(t)?;
                    self.buf.push_str(";\n");
                    return Ok(());
                }
                // A `?` in the tail value (e.g. body tail `find_task(id)?`)
                // hoists to a pre-`return` temp + early-return.
                self.hoist_propagates(t)?;
                self.emit_tail_value(t)?;
            }
        } else if crate::generator::node_is_statement(node) || tail_is_statement_form(node) {
            self.emit_node(node)?;
        } else if js_call_is_diverging(node) {
            self.write_indent();
            self.emit_expr(node)?;
            self.buf.push_str(";\n");
        } else if let NodeKind::Match { scrutinee, arms } = &node.kind {
            if crate::generator::match_has_statement_arm(arms) {
                self.emit_match(scrutinee, arms)?;
            } else {
                self.hoist_propagates(node)?;
                self.emit_tail_value(node)?;
            }
        } else {
            // Single expression as body.
            self.hoist_propagates(node)?;
            self.emit_tail_value(node)?;
        }
        Ok(())
    }

    /// Emit a block-body tail *value* expression. In a function-body /
    /// value-context block this is `return <value>;`. In a statement-position
    /// block ([`Self::discard_tail`] set — a loop body, or a statement-position
    /// `if`/`match` branch) the value is discarded, emitted as a bare expression
    /// statement `<value>;`; a `return` there would abort the enclosing function
    /// on the first loop iteration (the fizzbuzz / chat-protocol silent
    /// truncation bug). Callers that need an early-return-on-`?` must call
    /// [`Self::hoist_propagates`] first (this just renders the value).
    fn emit_tail_value(&mut self, value: &AIRNode) -> Result<(), CodegenError> {
        let ind = self.indent_str();
        if self.discard_tail {
            self.buf.push_str(&ind);
            self.emit_expr(value)?;
            self.buf.push_str(";\n");
        } else {
            let _ = write!(self.buf, "{ind}return ");
            self.emit_expr(value)?;
            self.buf.push_str(";\n");
        }
        Ok(())
    }

    fn emit_block_as_expr(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::Block { stmts, tail } = &node.kind {
            if stmts.is_empty() {
                if let Some(t) = tail {
                    return self.emit_expr(t);
                }
            }
        }
        // Fallback: emit as IIFE.
        self.emit_expr(node)
    }

    fn pattern_to_binding_name(&self, pat: &AIRNode) -> String {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => js_value_ident(&name.name),
            NodeKind::WildcardPat => "_".into(),
            NodeKind::TuplePat { elems } => {
                format!(
                    "[{}]",
                    elems
                        .iter()
                        .map(|e| self.pattern_to_binding_name(e))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            NodeKind::RecordPat { fields, .. } => {
                format!(
                    "{{ {} }}",
                    fields
                        .iter()
                        .map(|f| to_camel_case(&f.name.name).to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            _ => "_".into(),
        }
    }

    fn pattern_to_js_destructure(&self, pat: &AIRNode) -> String {
        self.pattern_to_binding_name(pat)
    }

    fn type_expr_to_string(&self, node: &AIRNode) -> String {
        match &node.kind {
            NodeKind::TypeNamed { path, .. } => path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("."),
            NodeKind::Identifier { name } => name.name.clone(),
            _ => "Unknown".into(),
        }
    }
}

// ─── Utility functions ───────────────────────────────────────────────────────

/// Returns true if `name` is the identifier of a Duration or Instant instance
/// method. Used to recognise `d.as_millis()` / `i.elapsed()` calls during codegen.
fn is_time_method_name(name: &str) -> bool {
    matches!(
        name,
        "as_nanos"
            | "as_millis"
            | "as_seconds"
            | "is_zero"
            | "is_negative"
            | "abs"
            | "elapsed"
            | "duration_since"
    )
}

/// Convert a name to `camelCase` (handles `snake_case`, `PascalCase`, and already `camelCase`).
/// Convert a Bock *value* identifier (a param, local binding, or free-function
/// name) to its JS form: `camelCase`, then escaped against the JS reserved-word
/// set so a binding named e.g. `default` emits `default_` rather than the
/// illegal bare keyword. Apply at every value declaration and reference site so
/// the escaped name is used uniformly; member/method names use bare
/// True when a value-position node lowers to a JS **statement-form** construct
/// that has no expression form — a `loop`/`while`/`for`, a `guard`, a nested
/// `block`, or a `handling` block. In tail position such a node must be emitted
/// as a statement (via [`EmitCtx::emit_node`]), never wrapped in `return <expr>`
/// (which would yield invalid JS such as `return while (…)` or, for a value-less
/// loop the expression lowering can't represent, `return /* unsupported */`).
///
/// Value-*bearing* loops/ifs/matches are rewritten upstream by the shared
/// value-CF pre-pass ([`crate::generator::hoist_value_cf`]) into a declare-then-
/// assign temp, so any such construct still present in tail position is
/// value-less and is safe to emit as a bare statement.
fn tail_is_statement_form(node: &AIRNode) -> bool {
    matches!(
        node.kind,
        NodeKind::Loop { .. }
            | NodeKind::While { .. }
            | NodeKind::For { .. }
            | NodeKind::Guard { .. }
            | NodeKind::HandlingBlock { .. }
    )
}

/// Collect every `?` (`Propagate`) node reachable in `node`'s **own evaluation**
/// into `out`, in evaluation (innermost-first) order — see
/// [`EmitCtx::hoist_propagates`].
///
/// The walk follows only sub-expressions that are unconditionally evaluated as
/// part of the enclosing statement: a call's callee + args, a method call's
/// receiver + args, both operands of a non-short-circuiting binary op, a unary
/// operand, field/index access, `await`, a record/list/tuple/set element, a
/// pipe/compose operand, an interpolation hole, an assignment RHS, and a
/// `let`/`Propagate`/`Return` payload. It deliberately does **not** descend
/// into nested scopes (`Block`/`Lambda`) or the conditional/branch sub-trees of
/// `If`/`Match`/`Loop`/`While`/`For`/`Guard` (whose own `?`s are hoisted when
/// that nested statement is emitted), nor into the short-circuited operand of
/// `&&`/`||`.
fn collect_propagates_in_expr<'a>(node: &'a AIRNode, out: &mut Vec<&'a AIRNode>) {
    match &node.kind {
        NodeKind::Propagate { expr } => {
            // Inner `?` first (nested `f(x)?` → hoist `f(x)`'s `?` before this).
            collect_propagates_in_expr(expr, out);
            out.push(node);
        }
        NodeKind::Call { callee, args, .. } => {
            collect_propagates_in_expr(callee, out);
            for a in args {
                collect_propagates_in_expr(&a.value, out);
            }
        }
        NodeKind::MethodCall { receiver, args, .. } => {
            collect_propagates_in_expr(receiver, out);
            for a in args {
                collect_propagates_in_expr(&a.value, out);
            }
        }
        NodeKind::BinaryOp { op, left, right } => {
            collect_propagates_in_expr(left, out);
            // `&&` / `||` short-circuit: the right operand is only sometimes
            // evaluated, so a `?` there cannot be unconditionally hoisted.
            if !matches!(op, BinOp::And | BinOp::Or) {
                collect_propagates_in_expr(right, out);
            }
        }
        NodeKind::UnaryOp { operand, .. } => collect_propagates_in_expr(operand, out),
        NodeKind::FieldAccess { object, .. } => collect_propagates_in_expr(object, out),
        NodeKind::Index { object, index } => {
            collect_propagates_in_expr(object, out);
            collect_propagates_in_expr(index, out);
        }
        NodeKind::Await { expr } => collect_propagates_in_expr(expr, out),
        NodeKind::Pipe { left, right } | NodeKind::Compose { left, right } => {
            collect_propagates_in_expr(left, out);
            collect_propagates_in_expr(right, out);
        }
        NodeKind::Range { lo, hi, .. } => {
            collect_propagates_in_expr(lo, out);
            collect_propagates_in_expr(hi, out);
        }
        NodeKind::RecordConstruct { fields, spread, .. } => {
            for f in fields {
                if let Some(v) = &f.value {
                    collect_propagates_in_expr(v, out);
                }
            }
            if let Some(s) = spread {
                collect_propagates_in_expr(s, out);
            }
        }
        NodeKind::ListLiteral { elems }
        | NodeKind::SetLiteral { elems }
        | NodeKind::TupleLiteral { elems } => {
            for e in elems {
                collect_propagates_in_expr(e, out);
            }
        }
        NodeKind::MapLiteral { entries } => {
            for e in entries {
                collect_propagates_in_expr(&e.key, out);
                collect_propagates_in_expr(&e.value, out);
            }
        }
        NodeKind::Interpolation { parts } => {
            for p in parts {
                if let AirInterpolationPart::Expr(e) = p {
                    collect_propagates_in_expr(e, out);
                }
            }
        }
        NodeKind::Assign { value, .. } => collect_propagates_in_expr(value, out),
        NodeKind::LetBinding { value, .. } => collect_propagates_in_expr(value, out),
        NodeKind::Return { value: Some(v) } => collect_propagates_in_expr(v, out),
        // Leaf, nested-scope, and conditional-control-flow nodes are not
        // descended: their `?`s (if any) belong to a different statement scope.
        _ => {}
    }
}

/// True when `node` is a call to a diverging intrinsic (`todo()` /
/// `unreachable()`), which lowers to a bare `throw new Error(...)` *statement* —
/// it never produces a value. In value-tail position the throw must be emitted
/// as its own statement: `return throw …` is a JS `SyntaxError`.
fn js_call_is_diverging(node: &AIRNode) -> bool {
    if let NodeKind::Call { callee, .. } = &node.kind {
        if let NodeKind::Identifier { name } = &callee.kind {
            return matches!(name.name.as_str(), "todo" | "unreachable");
        }
    }
    false
}

/// [`to_camel_case`] (a keyword is legal as a member name). See
/// [`crate::generator::escape_target_keyword`].
///
/// Beyond the shared reserved-word set, JS forbids `eval` and `arguments` as a
/// binding or function name in strict mode (and every emitted ESM module is
/// strict). These are not reserved *keywords*, so they are not in the shared
/// list; the js emitter escapes them here so e.g. a Bock `fn eval(...)` emits
/// `function eval_(...)` rather than the strict-mode `SyntaxError`.
fn js_value_ident(name: &str) -> String {
    let escaped = crate::generator::escape_target_keyword(
        &to_camel_case(name),
        crate::generator::KeywordTarget::Js,
    );
    if matches!(escaped.as_str(), "eval" | "arguments") {
        format!("{escaped}_")
    } else {
        escaped
    }
}

/// Spell `name` the way the JS backend emits the symbol's declaration / call
/// sites for an `import`/`export` specifier in the per-module path: a function
/// is camelCased and keyword-escaped via [`js_value_ident`]; any other kind
/// (records, enum variants, classes, traits, effects, consts) keeps its raw
/// name. A free function (not a method) so both `EmitCtx` and `generate_project`
/// helpers can call it.
/// True if any arm of `arms` matches against a list pattern (`[]`, `[a, ..b]`)
/// or a range pattern (`1..10`, `1..=10`). Neither has a single `switch`
/// discriminant — every such arm lowers to a `default:`, which is a JS
/// `SyntaxError` ("more than one default clause") once there are two of them.
/// The js emitter routes these to the if/else-if chain instead.
fn match_has_unswitchable_pattern(arms: &[AIRNode]) -> bool {
    arms.iter().any(|arm| {
        matches!(
            &arm.kind,
            NodeKind::MatchArm { pattern, .. }
                if matches!(pattern.kind, NodeKind::ListPat { .. } | NodeKind::RangePat { .. })
        )
    })
}

fn esm_emit_name_static(name: &str, is_fn: bool) -> String {
    if is_fn {
        js_value_ident(name)
    } else {
        name.to_string()
    }
}

fn to_camel_case(s: &str) -> String {
    if s.is_empty() || s == "_" {
        return s.to_string();
    }
    // If already camelCase (starts lowercase, no underscores), return as-is.
    if !s.contains('_') && s.starts_with(|c: char| c.is_lowercase()) {
        return s.to_string();
    }
    // If it's snake_case, convert to camelCase.
    if s.contains('_') {
        let parts: Vec<&str> = s.split('_').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return s.to_string();
        }
        let mut result = parts[0].to_lowercase();
        for part in &parts[1..] {
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                result.push(
                    first
                        .to_uppercase()
                        .next()
                        .expect("uppercase yields at least one char"),
                );
                result.extend(chars);
            }
        }
        return result;
    }
    // If PascalCase, lowercase first letter.
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty string guaranteed by caller");
    let mut result = first.to_lowercase().to_string();
    result.extend(chars);
    result
}

/// Escape special characters in a JS string literal.
fn escape_js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render a literal as a JS value expression — used by the if-chain match
/// lowering to compare a scrutinee against a literal pattern (`<access> === …`).
/// Render a `RangePat` bound (`lo`/`hi`) as a JS expression. Range bounds are
/// literals (`1..10`) or a const identifier (`MIN..MAX`); anything else falls
/// back to the wrapped literal/identifier text, or `0` for an unrecognised node.
fn range_bound_to_js(node: &AIRNode) -> String {
    match &node.kind {
        NodeKind::LiteralPat { lit } => js_literal(lit),
        NodeKind::Literal { lit } => js_literal(lit),
        NodeKind::Identifier { name } => js_value_ident(&name.name),
        _ => "0".to_string(),
    }
}

fn js_literal(lit: &Literal) -> String {
    match lit {
        Literal::Int(s) | Literal::Float(s) => s.clone(),
        Literal::Bool(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Literal::Char(s) => format!("'{s}'"),
        Literal::String(s) => format!("\"{}\"", escape_js_string(s)),
        Literal::Unit => "undefined".to_string(),
    }
}

/// Escape special characters in a JS template literal.
fn escape_template_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '`' => out.push_str("\\`"),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            _ => out.push(ch),
        }
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{AirArg, AirRecordField};
    use bock_ast::{Ident, TypePath};
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

    fn type_path(segments: &[&str]) -> TypePath {
        TypePath {
            segments: segments.iter().map(|s| ident(s)).collect(),
            span: span(),
        }
    }

    fn node(id: u32, kind: NodeKind) -> AIRNode {
        AIRNode::new(id, span(), kind)
    }

    fn int_lit(id: u32, val: &str) -> AIRNode {
        node(
            id,
            NodeKind::Literal {
                lit: Literal::Int(val.into()),
            },
        )
    }

    fn str_lit(id: u32, val: &str) -> AIRNode {
        node(
            id,
            NodeKind::Literal {
                lit: Literal::String(val.into()),
            },
        )
    }

    fn bool_lit(id: u32, val: bool) -> AIRNode {
        node(
            id,
            NodeKind::Literal {
                lit: Literal::Bool(val),
            },
        )
    }

    fn id_node(id: u32, name: &str) -> AIRNode {
        node(id, NodeKind::Identifier { name: ident(name) })
    }

    fn bind_pat(id: u32, name: &str) -> AIRNode {
        node(
            id,
            NodeKind::BindPat {
                name: ident(name),
                is_mut: false,
            },
        )
    }

    fn param_node(id: u32, name: &str) -> AIRNode {
        node(
            id,
            NodeKind::Param {
                pattern: Box::new(bind_pat(id + 100, name)),
                ty: None,
                default: None,
            },
        )
    }

    fn block(id: u32, stmts: Vec<AIRNode>, tail: Option<AIRNode>) -> AIRNode {
        node(
            id,
            NodeKind::Block {
                stmts,
                tail: tail.map(Box::new),
            },
        )
    }

    fn module(imports: Vec<AIRNode>, items: Vec<AIRNode>) -> AIRNode {
        node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports,
                items,
            },
        )
    }

    fn gen(module: &AIRNode) -> String {
        let gen = JsGenerator::new();
        let result = gen.generate_module(module).unwrap();
        result.files[0].content.clone()
    }

    // ── Basic tests ─────────────────────────────────────────────────────────

    #[test]
    fn implements_code_generator_trait() {
        let gen = JsGenerator::new();
        assert_eq!(gen.target().id, "js");
    }

    #[test]
    fn empty_module() {
        let m = module(vec![], vec![]);
        let out = gen(&m);
        assert_eq!(out, "");
    }

    #[test]
    fn simple_function() {
        let body = block(2, vec![], Some(int_lit(3, "42")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("answer"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("function answer()"));
        assert!(out.contains("return 42;"));
    }

    /// Build a desugared `recv.method(extra)` Call in the AIR shape the lowerer
    /// produces (receiver cloned into the FieldAccess object and the leading
    /// self arg, sharing a NodeId).
    fn list_method_call(method: &str, extra: Vec<AIRNode>) -> AIRNode {
        let recv = id_node(5, "nums");
        let callee = node(
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
        node(
            7,
            NodeKind::Call {
                callee: Box::new(callee),
                args,
                type_args: vec![],
            },
        )
    }

    #[test]
    fn list_len_emits_length_property() {
        let body = block(2, vec![], Some(list_method_call("len", vec![])));
        let f = node(
            1,
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
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("(nums).length"), "got: {out}");
        // Must NOT emit the verbatim double-pass `nums.len(nums)`.
        assert!(!out.contains("nums.len("), "got: {out}");
    }

    #[test]
    fn list_get_emits_tagged_optional_with_bounds_check() {
        let body = block(
            2,
            vec![],
            Some(list_method_call("get", vec![int_lit(8, "1")])),
        );
        let f = node(
            1,
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
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("_tag: \"Some\""), "got: {out}");
        assert!(out.contains("_tag: \"None\""), "got: {out}");
        assert!(
            out.contains("__i < __r.length"),
            "bounds check missing, got: {out}"
        );
    }

    #[test]
    fn function_with_params() {
        let body = block(
            5,
            vec![],
            Some(node(
                6,
                NodeKind::BinaryOp {
                    op: BinOp::Add,
                    left: Box::new(id_node(7, "a")),
                    right: Box::new(id_node(8, "b")),
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("add"),
                generic_params: vec![],
                params: vec![param_node(2, "a"), param_node(3, "b")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("export function add(a, b)"));
        assert!(out.contains("(a + b)"));
    }

    #[test]
    fn async_function() {
        let body = block(
            3,
            vec![],
            Some(node(
                4,
                NodeKind::Await {
                    expr: Box::new(node(
                        5,
                        NodeKind::Call {
                            callee: Box::new(id_node(6, "fetch")),
                            args: vec![AirArg {
                                label: None,
                                value: str_lit(7, "https://example.com"),
                            }],
                            type_args: vec![],
                        },
                    )),
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("fetchData"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("async function fetchData()"));
        assert!(out.contains("await fetch"));
    }

    #[test]
    fn effects_as_destructured_params() {
        let body = block(
            3,
            vec![node(
                4,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(5, "msg")),
                    ty: None,
                    value: Box::new(str_lit(6, "hello")),
                },
            )],
            Some(node(
                7,
                NodeKind::EffectOp {
                    effect: type_path(&["Log"]),
                    operation: ident("info"),
                    args: vec![AirArg {
                        label: None,
                        value: id_node(8, "msg"),
                    }],
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("process"),
                generic_params: vec![],
                params: vec![param_node(2, "data")],
                return_type: None,
                effect_clause: vec![type_path(&["Log"]), type_path(&["Clock"])],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("function process(data, { log, clock })"));
        assert!(out.contains("log.info(msg)"));
    }

    /// Q-clock-handler-routing: inside a `with Clock` function the §18.3.1 time
    /// builtins route through the in-scope `clock` handler — `Instant.now()` →
    /// `clock.now_monotonic()`, `sleep(d)` → `clock.sleep(d)`,
    /// `start.elapsed()` → `clock.now_monotonic() - start` — NOT the inlined
    /// host primitives (`performance.now()` / `setTimeout`).
    #[test]
    fn clock_time_ops_route_through_handler() {
        let out = gen(&module(vec![], vec![clock_timed_fn()]));
        assert!(out.contains("clock.now_monotonic()"), "got: {out}");
        assert!(out.contains("clock.sleep("), "got: {out}");
        assert!(
            !out.contains("performance.now()"),
            "host clock primitive leaked past the handler: {out}"
        );
        assert!(
            !out.contains("setTimeout"),
            "host sleep primitive leaked past the handler: {out}"
        );
    }

    /// Builds `fn timed() with Clock { let start = Instant.now(); sleep(
    /// Duration.millis(1)); let d = start.elapsed() }` — the `with Clock` clause
    /// puts the `clock` handler in scope so the time builtins route through it.
    fn clock_timed_fn() -> AIRNode {
        let instant_now = node(
            40,
            NodeKind::Call {
                callee: Box::new(node(
                    41,
                    NodeKind::FieldAccess {
                        object: Box::new(id_node(42, "Instant")),
                        field: ident("now"),
                    },
                )),
                args: vec![],
                type_args: vec![],
            },
        );
        let duration_millis = node(
            50,
            NodeKind::Call {
                callee: Box::new(node(
                    51,
                    NodeKind::FieldAccess {
                        object: Box::new(id_node(52, "Duration")),
                        field: ident("millis"),
                    },
                )),
                args: vec![AirArg {
                    label: None,
                    value: int_lit(53, "1"),
                }],
                type_args: vec![],
            },
        );
        let sleep_call = node(
            60,
            NodeKind::Call {
                callee: Box::new(id_node(61, "sleep")),
                args: vec![AirArg {
                    label: None,
                    value: duration_millis,
                }],
                type_args: vec![],
            },
        );
        let elapsed_call = node(
            70,
            NodeKind::MethodCall {
                receiver: Box::new(id_node(71, "start")),
                method: ident("elapsed"),
                type_args: vec![],
                args: vec![],
            },
        );
        let body = block(
            30,
            vec![
                node(
                    31,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(32, "start")),
                        ty: None,
                        value: Box::new(instant_now),
                    },
                ),
                sleep_call,
                node(
                    33,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(34, "d")),
                        ty: None,
                        value: Box::new(elapsed_call),
                    },
                ),
            ],
            None,
        );
        node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("timed"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![type_path(&["Clock"])],
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    #[test]
    fn enum_to_tagged_objects() {
        let enum_decl = node(
            1,
            NodeKind::EnumDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Shape"),
                generic_params: vec![],
                variants: vec![
                    node(
                        2,
                        NodeKind::EnumVariant {
                            name: ident("Circle"),
                            payload: EnumVariantPayload::Struct(vec![bock_ast::RecordDeclField {
                                id: 0,
                                span: span(),
                                name: ident("radius"),
                                ty: bock_ast::TypeExpr::Named {
                                    id: 0,
                                    span: span(),
                                    path: type_path(&["Float"]),
                                    args: vec![],
                                },
                                default: None,
                            }]),
                        },
                    ),
                    node(
                        3,
                        NodeKind::EnumVariant {
                            name: ident("None"),
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                ],
            },
        );
        let out = gen(&module(vec![], vec![enum_decl]));
        assert!(out.contains("function Shape_Circle(radius)"));
        assert!(out.contains("_tag: \"Circle\""));
        assert!(out.contains("Shape_None = Object.freeze({ _tag: \"None\" })"));
    }

    #[test]
    fn match_on_tagged_objects() {
        let scrutinee = id_node(10, "shape");
        let arms = vec![
            node(
                11,
                NodeKind::MatchArm {
                    pattern: Box::new(node(
                        12,
                        NodeKind::ConstructorPat {
                            path: type_path(&["Shape", "Circle"]),
                            fields: vec![bind_pat(13, "r")],
                        },
                    )),
                    guard: None,
                    body: Box::new(block(
                        14,
                        vec![],
                        Some(node(
                            15,
                            NodeKind::BinaryOp {
                                op: BinOp::Mul,
                                left: Box::new(id_node(16, "r")),
                                right: Box::new(id_node(17, "r")),
                            },
                        )),
                    )),
                },
            ),
            node(
                18,
                NodeKind::MatchArm {
                    pattern: Box::new(node(19, NodeKind::WildcardPat)),
                    guard: None,
                    body: Box::new(block(20, vec![], Some(int_lit(21, "0")))),
                },
            ),
        ];
        let match_stmt = node(
            9,
            NodeKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
        );
        // Wrap match in a function for statement context
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("area"),
                generic_params: vec![],
                params: vec![param_node(2, "shape")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(3, vec![match_stmt], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("switch (shape._tag)"));
        assert!(out.contains("case \"Circle\""));
        assert!(out.contains("const r = shape._0;"));
        assert!(out.contains("default:"));
    }

    #[test]
    fn ownership_erased() {
        let move_expr = node(
            1,
            NodeKind::Move {
                expr: Box::new(id_node(2, "x")),
            },
        );
        let borrow_expr = node(
            3,
            NodeKind::Borrow {
                expr: Box::new(id_node(4, "y")),
            },
        );
        let mut_borrow_expr = node(
            5,
            NodeKind::MutableBorrow {
                expr: Box::new(id_node(6, "z")),
            },
        );
        let body = block(
            7,
            vec![
                node(
                    8,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(9, "a")),
                        ty: None,
                        value: Box::new(move_expr),
                    },
                ),
                node(
                    10,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(11, "b")),
                        ty: None,
                        value: Box::new(borrow_expr),
                    },
                ),
                node(
                    12,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(13, "c")),
                        ty: None,
                        value: Box::new(mut_borrow_expr),
                    },
                ),
            ],
            None,
        );
        let f = node(
            0,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        // Ownership annotations should be erased; just the values remain.
        assert!(out.contains("const a = x;"));
        assert!(out.contains("const b = y;"));
        assert!(out.contains("const c = z;"));
    }

    #[test]
    fn let_binding_mut_uses_let() {
        let binding = node(
            1,
            NodeKind::LetBinding {
                is_mut: true,
                pattern: Box::new(bind_pat(2, "count")),
                ty: None,
                value: Box::new(int_lit(3, "0")),
            },
        );
        let f = node(
            0,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(4, vec![binding], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("let count = 0;"));
    }

    #[test]
    fn string_interpolation() {
        let interp = node(
            1,
            NodeKind::Interpolation {
                parts: vec![
                    AirInterpolationPart::Literal("Hello, ".into()),
                    AirInterpolationPart::Expr(Box::new(id_node(2, "name"))),
                    AirInterpolationPart::Literal("!".into()),
                ],
            },
        );
        let binding = node(
            3,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(4, "msg")),
                ty: None,
                value: Box::new(interp),
            },
        );
        let f = node(
            0,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("greet"),
                generic_params: vec![],
                params: vec![param_node(5, "name")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(6, vec![binding], Some(id_node(7, "msg")))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("`Hello, ${name}!`"));
    }

    #[test]
    fn list_map_set_literals() {
        let list = node(
            1,
            NodeKind::ListLiteral {
                elems: vec![int_lit(2, "1"), int_lit(3, "2"), int_lit(4, "3")],
            },
        );
        let map = node(
            5,
            NodeKind::MapLiteral {
                entries: vec![bock_air::AirMapEntry {
                    key: str_lit(6, "a"),
                    value: int_lit(7, "1"),
                }],
            },
        );
        let set = node(
            8,
            NodeKind::SetLiteral {
                elems: vec![int_lit(9, "1"), int_lit(10, "2")],
            },
        );
        let body = block(
            11,
            vec![
                node(
                    12,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(13, "xs")),
                        ty: None,
                        value: Box::new(list),
                    },
                ),
                node(
                    14,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(15, "m")),
                        ty: None,
                        value: Box::new(map),
                    },
                ),
                node(
                    16,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(17, "s")),
                        ty: None,
                        value: Box::new(set),
                    },
                ),
            ],
            None,
        );
        let f = node(
            0,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("collections"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("[1, 2, 3]"));
        assert!(out.contains("new Map([[\"a\", 1]])"));
        assert!(out.contains("new Set([1, 2])"));
    }

    #[test]
    fn record_construction() {
        let rec = node(
            1,
            NodeKind::RecordConstruct {
                path: type_path(&["User"]),
                fields: vec![
                    AirRecordField {
                        name: ident("name"),
                        value: Some(Box::new(str_lit(2, "Alice"))),
                    },
                    AirRecordField {
                        name: ident("age"),
                        value: Some(Box::new(int_lit(3, "30"))),
                    },
                ],
                spread: None,
            },
        );
        let binding = node(
            4,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(5, "user")),
                ty: None,
                value: Box::new(rec),
            },
        );
        let f = node(
            0,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(6, vec![binding], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("{ name: \"Alice\", age: 30 }"));
    }

    #[test]
    fn control_flow() {
        let if_stmt = node(
            1,
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(bool_lit(2, true)),
                then_block: Box::new(block(3, vec![], Some(int_lit(4, "1")))),
                else_block: Some(Box::new(block(5, vec![], Some(int_lit(6, "2"))))),
            },
        );
        let for_stmt = node(
            7,
            NodeKind::For {
                pattern: Box::new(bind_pat(8, "x")),
                iterable: Box::new(id_node(9, "items")),
                body: Box::new(block(10, vec![], None)),
            },
        );
        let while_stmt = node(
            11,
            NodeKind::While {
                condition: Box::new(bool_lit(12, true)),
                body: Box::new(block(
                    13,
                    vec![node(14, NodeKind::Break { value: None })],
                    None,
                )),
            },
        );
        let body = block(15, vec![if_stmt, for_stmt, while_stmt], None);
        let f = node(
            0,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("flow"),
                generic_params: vec![],
                params: vec![param_node(16, "items")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("if (true)"));
        assert!(out.contains("} else {"));
        assert!(out.contains("for (const x of items)"));
        assert!(out.contains("while (true)"));
        assert!(out.contains("break;"));
    }

    #[test]
    fn lambda_and_pipe() {
        let lambda = node(
            1,
            NodeKind::Lambda {
                params: vec![param_node(2, "x")],
                body: Box::new(node(
                    3,
                    NodeKind::BinaryOp {
                        op: BinOp::Mul,
                        left: Box::new(id_node(4, "x")),
                        right: Box::new(int_lit(5, "2")),
                    },
                )),
            },
        );
        let pipe = node(
            6,
            NodeKind::Pipe {
                left: Box::new(int_lit(7, "5")),
                right: Box::new(id_node(8, "double")),
            },
        );
        let body = block(
            9,
            vec![
                node(
                    10,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(11, "double")),
                        ty: None,
                        value: Box::new(lambda),
                    },
                ),
                node(
                    12,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(13, "result")),
                        ty: None,
                        value: Box::new(pipe),
                    },
                ),
            ],
            None,
        );
        let f = node(
            0,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("(x) => (x * 2)"));
        assert!(out.contains("double(5)"));
    }

    #[test]
    fn result_construct() {
        let ok = node(
            1,
            NodeKind::ResultConstruct {
                variant: ResultVariant::Ok,
                value: Some(Box::new(int_lit(2, "42"))),
            },
        );
        let err = node(
            3,
            NodeKind::ResultConstruct {
                variant: ResultVariant::Err,
                value: Some(Box::new(str_lit(4, "failed"))),
            },
        );
        let body = block(
            5,
            vec![
                node(
                    6,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(7, "good")),
                        ty: None,
                        value: Box::new(ok),
                    },
                ),
                node(
                    8,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(9, "bad")),
                        ty: None,
                        value: Box::new(err),
                    },
                ),
            ],
            None,
        );
        let f = node(
            0,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        // Reconciled on the `_0` payload key the `Result` match reads.
        assert!(out.contains("{ _tag: \"Ok\", _0: 42 }"), "got: {out}");
        assert!(
            out.contains("{ _tag: \"Err\", _0: \"failed\" }"),
            "got: {out}"
        );
    }

    #[test]
    fn class_declaration() {
        let method_body = block(10, vec![], Some(id_node(11, "undefined")));
        let method = node(
            5,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("greet"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(method_body),
            },
        );
        let cls = node(
            1,
            NodeKind::ClassDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Person"),
                generic_params: vec![],
                base: None,
                traits: vec![],
                fields: vec![bock_ast::RecordDeclField {
                    id: 0,
                    span: span(),
                    name: ident("name"),
                    ty: bock_ast::TypeExpr::Named {
                        id: 0,
                        span: span(),
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                    default: None,
                }],
                methods: vec![method],
            },
        );
        let out = gen(&module(vec![], vec![cls]));
        assert!(out.contains("class Person {"));
        assert!(out.contains("constructor(name)"));
        assert!(out.contains("this.name = name;"));
        assert!(out.contains("greet()"));
    }

    /// A `class T { a, b }` literal must construct via the class's **positional**
    /// constructor — `new T(a_value, b_value)` in *field-declaration order* — not
    /// the bare object literal the record path would emit (whose prototype
    /// methods would be unreachable). Q-class-codegen.
    #[test]
    fn class_literal_constructs_positionally() {
        fn class_field(name: &str) -> bock_ast::RecordDeclField {
            bock_ast::RecordDeclField {
                id: 0,
                span: span(),
                name: ident(name),
                ty: bock_ast::TypeExpr::Named {
                    id: 0,
                    span: span(),
                    path: type_path(&["String"]),
                    args: vec![],
                },
                default: None,
            }
        }
        let cls = node(
            1,
            NodeKind::ClassDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Button"),
                generic_params: vec![],
                base: None,
                traits: vec![],
                fields: vec![class_field("label"), class_field("on_click")],
                methods: vec![],
            },
        );
        // Construct with the fields supplied OUT of declaration order
        // (`on_click` before `label`) — the emitter must still pass them in
        // declaration order positionally.
        let construct = node(
            10,
            NodeKind::RecordConstruct {
                path: type_path(&["Button"]),
                fields: vec![
                    AirRecordField {
                        name: ident("on_click"),
                        value: Some(Box::new(str_lit(11, "click"))),
                    },
                    AirRecordField {
                        name: ident("label"),
                        value: Some(Box::new(str_lit(12, "Submit"))),
                    },
                ],
                spread: None,
            },
        );
        let main_fn = node(
            20,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    21,
                    vec![node(
                        22,
                        NodeKind::LetBinding {
                            is_mut: false,
                            pattern: Box::new(bind_pat(23, "b")),
                            ty: None,
                            value: Box::new(construct),
                        },
                    )],
                    None,
                )),
            },
        );
        let out = gen(&module(vec![], vec![cls, main_fn]));
        // Positional construction in declaration order — NOT a bare object literal.
        assert!(
            out.contains(r#"new Button("Submit", "click")"#),
            "expected positional `new Button(...)` in declaration order, got:\n{out}"
        );
        assert!(
            !out.contains("{ label:"),
            "class literal must not emit a bare object literal:\n{out}"
        );
    }

    #[test]
    fn const_declaration() {
        let c = node(
            1,
            NodeKind::ConstDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("PI"),
                ty: Box::new(node(
                    2,
                    NodeKind::TypeNamed {
                        path: type_path(&["Float"]),
                        args: vec![],
                    },
                )),
                value: Box::new(node(
                    3,
                    NodeKind::Literal {
                        lit: Literal::Float("3.14159".into()),
                    },
                )),
            },
        );
        let out = gen(&module(vec![], vec![c]));
        assert!(out.contains("const PI = 3.14159;"));
    }

    #[test]
    fn record_declaration() {
        let rec = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Point"),
                generic_params: vec![],
                fields: vec![
                    bock_ast::RecordDeclField {
                        id: 0,
                        span: span(),
                        name: ident("x"),
                        ty: bock_ast::TypeExpr::Named {
                            id: 0,
                            span: span(),
                            path: type_path(&["Float"]),
                            args: vec![],
                        },
                        default: None,
                    },
                    bock_ast::RecordDeclField {
                        id: 0,
                        span: span(),
                        name: ident("y"),
                        ty: bock_ast::TypeExpr::Named {
                            id: 0,
                            span: span(),
                            path: type_path(&["Float"]),
                            args: vec![],
                        },
                        default: None,
                    },
                ],
            },
        );
        let out = gen(&module(vec![], vec![rec]));
        assert!(out.contains("class Point {"));
        assert!(out.contains("constructor({ x, y })"));
        assert!(out.contains("this.x = x;"));
        assert!(out.contains("this.y = y;"));
    }

    // ── End-to-end tests (node --check + node execution) ────────────────────

    fn has_node() -> bool {
        std::process::Command::new("which")
            .arg("node")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Run generated JS through `node --check` for syntax validation.
    fn check_js_syntax(code: &str) -> bool {
        use std::io::Write;
        let mut child = std::process::Command::new("node")
            .arg("--check")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn node");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(code.as_bytes())
            .unwrap();
        child.wait().unwrap().success()
    }

    /// Run generated JS with `node` and capture stdout.
    fn run_js(code: &str) -> String {
        let output = std::process::Command::new("node")
            .arg("-e")
            .arg(code)
            .output()
            .expect("failed to run node");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    #[ignore]
    fn e2e_hello_world() {
        if !has_node() {
            return;
        }
        // fn main() { console.log("Hello, World!") }
        let body = block(
            2,
            vec![],
            Some(node(
                3,
                NodeKind::Call {
                    callee: Box::new(node(
                        4,
                        NodeKind::FieldAccess {
                            object: Box::new(id_node(5, "console")),
                            field: ident("log"),
                        },
                    )),
                    args: vec![AirArg {
                        label: None,
                        value: str_lit(6, "Hello, World!"),
                    }],
                    type_args: vec![],
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\nmain();\n");
        assert!(check_js_syntax(&full));
        assert_eq!(run_js(&full), "Hello, World!");
    }

    #[test]
    #[ignore]
    fn e2e_arithmetic() {
        if !has_node() {
            return;
        }
        let body = block(
            2,
            vec![],
            Some(node(
                3,
                NodeKind::BinaryOp {
                    op: BinOp::Add,
                    left: Box::new(int_lit(4, "10")),
                    right: Box::new(int_lit(5, "32")),
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("calc"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\nconsole.log(calc());\n");
        assert!(check_js_syntax(&full));
        assert_eq!(run_js(&full), "42");
    }

    #[test]
    #[ignore]
    fn e2e_if_else() {
        if !has_node() {
            return;
        }
        let if_stmt = node(
            3,
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(node(
                    4,
                    NodeKind::BinaryOp {
                        op: BinOp::Gt,
                        left: Box::new(id_node(5, "x")),
                        right: Box::new(int_lit(6, "0")),
                    },
                )),
                then_block: Box::new(block(7, vec![], Some(str_lit(8, "positive")))),
                else_block: Some(Box::new(block(
                    9,
                    vec![],
                    Some(str_lit(10, "non-positive")),
                ))),
            },
        );
        let body = block(2, vec![if_stmt], None);
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("classify"),
                generic_params: vec![],
                params: vec![param_node(11, "x")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\nconsole.log(classify(5));\nconsole.log(classify(-1));\n");
        assert!(check_js_syntax(&full));
        let output = run_js(&full);
        assert!(output.contains("positive"));
        assert!(output.contains("non-positive"));
    }

    #[test]
    #[ignore]
    fn e2e_for_loop() {
        if !has_node() {
            return;
        }
        let body = block(
            2,
            vec![
                node(
                    3,
                    NodeKind::LetBinding {
                        is_mut: true,
                        pattern: Box::new(bind_pat(4, "sum")),
                        ty: None,
                        value: Box::new(int_lit(5, "0")),
                    },
                ),
                node(
                    6,
                    NodeKind::For {
                        pattern: Box::new(bind_pat(7, "x")),
                        iterable: Box::new(node(
                            8,
                            NodeKind::ListLiteral {
                                elems: vec![int_lit(9, "1"), int_lit(10, "2"), int_lit(11, "3")],
                            },
                        )),
                        body: Box::new(block(
                            12,
                            vec![node(
                                13,
                                NodeKind::Assign {
                                    op: AssignOp::AddAssign,
                                    target: Box::new(id_node(14, "sum")),
                                    value: Box::new(id_node(15, "x")),
                                },
                            )],
                            None,
                        )),
                    },
                ),
            ],
            Some(id_node(16, "sum")),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("total"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\nconsole.log(total());\n");
        assert!(check_js_syntax(&full));
        assert_eq!(run_js(&full), "6");
    }

    #[test]
    #[ignore]
    fn e2e_tagged_objects() {
        if !has_node() {
            return;
        }
        // enum Color { Red, Green, Blue }
        let enum_decl = node(
            1,
            NodeKind::EnumDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Color"),
                generic_params: vec![],
                variants: vec![
                    node(
                        2,
                        NodeKind::EnumVariant {
                            name: ident("Red"),
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                    node(
                        3,
                        NodeKind::EnumVariant {
                            name: ident("Green"),
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                    node(
                        4,
                        NodeKind::EnumVariant {
                            name: ident("Blue"),
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                ],
            },
        );
        let code = gen(&module(vec![], vec![enum_decl]));
        let full =
            format!("{code}\nconsole.log(Color_Red._tag);\nconsole.log(Color_Green._tag);\n");
        assert!(check_js_syntax(&full));
        let output = run_js(&full);
        assert!(output.contains("Red"));
        assert!(output.contains("Green"));
    }

    #[test]
    #[ignore]
    fn e2e_match_switch() {
        if !has_node() {
            return;
        }
        // Match on literal values
        let match_node = node(
            3,
            NodeKind::Match {
                scrutinee: Box::new(id_node(4, "n")),
                arms: vec![
                    node(
                        5,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(
                                6,
                                NodeKind::LiteralPat {
                                    lit: Literal::Int("1".into()),
                                },
                            )),
                            guard: None,
                            body: Box::new(block(7, vec![], Some(str_lit(8, "one")))),
                        },
                    ),
                    node(
                        9,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(
                                10,
                                NodeKind::LiteralPat {
                                    lit: Literal::Int("2".into()),
                                },
                            )),
                            guard: None,
                            body: Box::new(block(11, vec![], Some(str_lit(12, "two")))),
                        },
                    ),
                    node(
                        13,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(14, NodeKind::WildcardPat)),
                            guard: None,
                            body: Box::new(block(15, vec![], Some(str_lit(16, "other")))),
                        },
                    ),
                ],
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("describe"),
                generic_params: vec![],
                params: vec![param_node(2, "n")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(17, vec![match_node], None)),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\nconsole.log(describe(1));\nconsole.log(describe(2));\nconsole.log(describe(99));\n");
        assert!(check_js_syntax(&full));
        let output = run_js(&full);
        assert!(output.contains("one"));
        assert!(output.contains("two"));
        assert!(output.contains("other"));
    }

    /// A literal + bind value match keeps the `switch` fast-path (no if-chain).
    #[test]
    fn match_literal_bind_stays_switch() {
        let arms = vec![
            node(
                5,
                NodeKind::MatchArm {
                    pattern: Box::new(node(
                        6,
                        NodeKind::LiteralPat {
                            lit: Literal::Int("0".into()),
                        },
                    )),
                    guard: None,
                    body: Box::new(block(7, vec![], Some(str_lit(8, "zero")))),
                },
            ),
            node(
                9,
                NodeKind::MatchArm {
                    pattern: Box::new(node(
                        10,
                        NodeKind::BindPat {
                            name: ident("x"),
                            is_mut: false,
                        },
                    )),
                    guard: None,
                    body: Box::new(block(11, vec![], Some(id_node(12, "x")))),
                },
            ),
        ];
        let m = node(
            3,
            NodeKind::Match {
                scrutinee: Box::new(id_node(4, "n")),
                arms,
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("label"),
                generic_params: vec![],
                params: vec![param_node(2, "n")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(13, vec![m], None)),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        assert!(
            code.contains("switch (n)"),
            "expected switch fast-path, got:\n{code}"
        );
        assert!(
            !code.contains("else if"),
            "should not use if-chain, got:\n{code}"
        );
    }

    /// A guarded arm lowers to an if/else-if chain whose failed guard falls
    /// through to the next arm (the value-`switch` could not express this).
    #[test]
    fn match_guard_lowers_to_ifchain() {
        let guarded = |id: u32, label: &str| {
            node(
                id,
                NodeKind::MatchArm {
                    pattern: Box::new(node(
                        id + 1,
                        NodeKind::BindPat {
                            name: ident("x"),
                            is_mut: false,
                        },
                    )),
                    guard: Some(Box::new(node(
                        id + 2,
                        NodeKind::BinaryOp {
                            op: BinOp::Gt,
                            left: Box::new(id_node(id + 3, "x")),
                            right: Box::new(node(
                                id + 4,
                                NodeKind::Literal {
                                    lit: Literal::Int("0".into()),
                                },
                            )),
                        },
                    ))),
                    body: Box::new(block(id + 5, vec![], Some(str_lit(id + 6, label)))),
                },
            )
        };
        let arms = vec![
            guarded(5, "pos"),
            node(
                20,
                NodeKind::MatchArm {
                    pattern: Box::new(node(21, NodeKind::WildcardPat)),
                    guard: None,
                    body: Box::new(block(22, vec![], Some(str_lit(23, "other")))),
                },
            ),
        ];
        let m = node(
            3,
            NodeKind::Match {
                scrutinee: Box::new(id_node(4, "n")),
                arms,
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("classify"),
                generic_params: vec![],
                params: vec![param_node(2, "n")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(30, vec![m], None)),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        assert!(
            !code.contains("switch"),
            "guard match must not use switch, got:\n{code}"
        );
        assert!(
            code.contains("else"),
            "guard match must chain to a fallthrough, got:\n{code}"
        );
        // The guard binding is re-introduced inside the condition's IIFE.
        assert!(
            code.contains("const x = n;"),
            "guard must bind x, got:\n{code}"
        );
    }

    #[test]
    #[ignore]
    fn e2e_string_interpolation() {
        if !has_node() {
            return;
        }
        let body = block(
            2,
            vec![],
            Some(node(
                3,
                NodeKind::Interpolation {
                    parts: vec![
                        AirInterpolationPart::Literal("Hello, ".into()),
                        AirInterpolationPart::Expr(Box::new(id_node(4, "name"))),
                        AirInterpolationPart::Literal("! You are ".into()),
                        AirInterpolationPart::Expr(Box::new(id_node(5, "age"))),
                        AirInterpolationPart::Literal(" years old.".into()),
                    ],
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("greet"),
                generic_params: vec![],
                params: vec![param_node(6, "name"), param_node(7, "age")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\nconsole.log(greet(\"Alice\", 30));\n");
        assert!(check_js_syntax(&full));
        assert_eq!(run_js(&full), "Hello, Alice! You are 30 years old.");
    }

    #[test]
    #[ignore]
    fn e2e_lambda_and_method_call() {
        if !has_node() {
            return;
        }
        let body = block(
            2,
            vec![node(
                3,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(4, "nums")),
                    ty: None,
                    value: Box::new(node(
                        5,
                        NodeKind::ListLiteral {
                            elems: vec![int_lit(6, "1"), int_lit(7, "2"), int_lit(8, "3")],
                        },
                    )),
                },
            )],
            Some(node(
                9,
                NodeKind::MethodCall {
                    receiver: Box::new(node(
                        10,
                        NodeKind::MethodCall {
                            receiver: Box::new(id_node(11, "nums")),
                            method: ident("map"),
                            type_args: vec![],
                            args: vec![AirArg {
                                label: None,
                                value: node(
                                    12,
                                    NodeKind::Lambda {
                                        params: vec![param_node(13, "x")],
                                        body: Box::new(node(
                                            14,
                                            NodeKind::BinaryOp {
                                                op: BinOp::Mul,
                                                left: Box::new(id_node(15, "x")),
                                                right: Box::new(int_lit(16, "2")),
                                            },
                                        )),
                                    },
                                ),
                            }],
                        },
                    )),
                    method: ident("join"),
                    type_args: vec![],
                    args: vec![AirArg {
                        label: None,
                        value: str_lit(17, ", "),
                    }],
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("transform"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\nconsole.log(transform());\n");
        assert!(check_js_syntax(&full));
        assert_eq!(run_js(&full), "2, 4, 6");
    }

    #[test]
    #[ignore]
    fn e2e_while_loop() {
        if !has_node() {
            return;
        }
        let body = block(
            2,
            vec![
                node(
                    3,
                    NodeKind::LetBinding {
                        is_mut: true,
                        pattern: Box::new(bind_pat(4, "i")),
                        ty: None,
                        value: Box::new(int_lit(5, "0")),
                    },
                ),
                node(
                    6,
                    NodeKind::LetBinding {
                        is_mut: true,
                        pattern: Box::new(bind_pat(7, "result")),
                        ty: None,
                        value: Box::new(int_lit(8, "1")),
                    },
                ),
                node(
                    9,
                    NodeKind::While {
                        condition: Box::new(node(
                            10,
                            NodeKind::BinaryOp {
                                op: BinOp::Lt,
                                left: Box::new(id_node(11, "i")),
                                right: Box::new(id_node(12, "n")),
                            },
                        )),
                        body: Box::new(block(
                            13,
                            vec![
                                node(
                                    14,
                                    NodeKind::Assign {
                                        op: AssignOp::MulAssign,
                                        target: Box::new(id_node(15, "result")),
                                        value: Box::new(int_lit(16, "2")),
                                    },
                                ),
                                node(
                                    17,
                                    NodeKind::Assign {
                                        op: AssignOp::AddAssign,
                                        target: Box::new(id_node(18, "i")),
                                        value: Box::new(int_lit(19, "1")),
                                    },
                                ),
                            ],
                            None,
                        )),
                    },
                ),
            ],
            Some(id_node(20, "result")),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("pow2"),
                generic_params: vec![],
                params: vec![param_node(21, "n")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\nconsole.log(pow2(10));\n");
        assert!(check_js_syntax(&full));
        assert_eq!(run_js(&full), "1024");
    }

    #[test]
    #[ignore]
    fn e2e_async_await() {
        if !has_node() {
            return;
        }
        // async fn delayed() { return await Promise.resolve(42) }
        let body = block(
            2,
            vec![],
            Some(node(
                3,
                NodeKind::Await {
                    expr: Box::new(node(
                        4,
                        NodeKind::Call {
                            callee: Box::new(node(
                                5,
                                NodeKind::FieldAccess {
                                    object: Box::new(id_node(6, "Promise")),
                                    field: ident("resolve"),
                                },
                            )),
                            args: vec![AirArg {
                                label: None,
                                value: int_lit(7, "42"),
                            }],
                            type_args: vec![],
                        },
                    )),
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("delayed"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\ndelayed().then(v => console.log(v));\n");
        assert!(check_js_syntax(&full));
        assert_eq!(run_js(&full), "42");
    }

    #[test]
    fn to_camel_case_converts() {
        // PascalCase → camelCase
        assert_eq!(to_camel_case("Log"), "log");
        assert_eq!(to_camel_case("Clock"), "clock");
        assert_eq!(to_camel_case("IO"), "iO");
        assert_eq!(to_camel_case(""), "");
        // snake_case → camelCase
        assert_eq!(to_camel_case("create_user"), "createUser");
        assert_eq!(to_camel_case("get_all_items"), "getAllItems");
        // Already camelCase → unchanged
        assert_eq!(to_camel_case("createUser"), "createUser");
        assert_eq!(to_camel_case("x"), "x");
        // Underscore → unchanged
        assert_eq!(to_camel_case("_"), "_");
    }

    #[test]
    fn snake_case_fn_becomes_camel_case() {
        let body = block(2, vec![], Some(int_lit(3, "42")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("create_user"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("function createUser()"),
            "expected camelCase function name, got: {out}"
        );
    }

    #[test]
    fn escape_js_string_works() {
        assert_eq!(escape_js_string("hello"), "hello");
        assert_eq!(escape_js_string("he\"llo"), "he\\\"llo");
        assert_eq!(escape_js_string("line\nbreak"), "line\\nbreak");
    }

    #[test]
    fn escape_template_literal_works() {
        assert_eq!(escape_template_literal("hello"), "hello");
        assert_eq!(escape_template_literal("cost: $5"), "cost: \\$5");
        assert_eq!(escape_template_literal("back`tick"), "back\\`tick");
    }

    // ── Prelude function mapping tests ──────────────────────────────────────

    /// Helper: generate JS for a module with a `main` function containing a single call.
    fn gen_prelude_call(func_name: &str, arg: AIRNode) -> String {
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, func_name)),
                args: vec![AirArg {
                    label: None,
                    value: arg,
                }],
                type_args: vec![],
            },
        );
        let body = block(2, vec![call], None);
        let f = node(
            1,
            NodeKind::FnDecl {
                name: ident("main"),
                params: vec![],
                return_type: None,
                body: Box::new(body),
                generic_params: vec![],
                visibility: Visibility::Private,
                annotations: vec![],
                effect_clause: vec![],
                where_clause: vec![],
                is_async: false,
            },
        );
        gen(&module(vec![], vec![f]))
    }

    /// Helper: generate JS for a nullary prelude call (no args).
    fn gen_prelude_call_no_args(func_name: &str) -> String {
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, func_name)),
                args: vec![],
                type_args: vec![],
            },
        );
        let body = block(2, vec![call], None);
        let f = node(
            1,
            NodeKind::FnDecl {
                name: ident("main"),
                params: vec![],
                return_type: None,
                body: Box::new(body),
                generic_params: vec![],
                visibility: Visibility::Private,
                annotations: vec![],
                effect_clause: vec![],
                where_clause: vec![],
                is_async: false,
            },
        );
        gen(&module(vec![], vec![f]))
    }

    #[test]
    fn prelude_println_maps_to_console_log() {
        let code = gen_prelude_call("println", str_lit(12, "Hello"));
        assert!(
            code.contains("console.log("),
            "expected console.log, got: {code}"
        );
        assert!(
            !code.contains("println("),
            "should not contain bare println, got: {code}"
        );
    }

    #[test]
    fn prelude_print_maps_to_process_stdout_write() {
        let code = gen_prelude_call("print", str_lit(12, "no newline"));
        assert!(
            code.contains("process.stdout.write(String("),
            "expected process.stdout.write, got: {code}"
        );
    }

    #[test]
    fn prelude_debug_maps_to_console_debug() {
        let code = gen_prelude_call("debug", str_lit(12, "val"));
        assert!(
            code.contains("console.debug("),
            "expected console.debug, got: {code}"
        );
    }

    #[test]
    fn prelude_assert_maps_to_throw() {
        let code = gen_prelude_call("assert", bool_lit(12, true));
        assert!(
            code.contains("if (!true) throw new Error(\"assertion failed\")"),
            "expected assert mapping, got: {code}"
        );
    }

    #[test]
    fn prelude_todo_maps_to_throw_not_implemented() {
        let code = gen_prelude_call_no_args("todo");
        assert!(
            code.contains("throw new Error(\"not implemented\")"),
            "expected todo mapping, got: {code}"
        );
    }

    #[test]
    fn prelude_unreachable_maps_to_throw_unreachable() {
        let code = gen_prelude_call_no_args("unreachable");
        assert!(
            code.contains("throw new Error(\"unreachable\")"),
            "expected unreachable mapping, got: {code}"
        );
    }

    #[test]
    fn non_prelude_call_unaffected() {
        let code = gen_prelude_call("my_custom_func", str_lit(12, "arg"));
        assert!(
            code.contains("myCustomFunc("),
            "expected normal call emission, got: {code}"
        );
    }

    // ── Effect declaration tests ────────────────────────────────────────────

    #[test]
    fn effect_decl_becomes_class() {
        let effect = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Logger"),
                generic_params: vec![],
                components: vec![],
                operations: vec![
                    node(
                        2,
                        NodeKind::FnDecl {
                            annotations: vec![],
                            visibility: Visibility::Public,
                            is_async: false,
                            name: ident("log"),
                            generic_params: vec![],
                            params: vec![param_node(3, "level"), param_node(4, "msg")],
                            return_type: None,
                            effect_clause: vec![],
                            where_clause: vec![],
                            body: Box::new(block(5, vec![], None)),
                        },
                    ),
                    node(
                        6,
                        NodeKind::FnDecl {
                            annotations: vec![],
                            visibility: Visibility::Public,
                            is_async: false,
                            name: ident("flush"),
                            generic_params: vec![],
                            params: vec![],
                            return_type: None,
                            effect_clause: vec![],
                            where_clause: vec![],
                            body: Box::new(block(7, vec![], None)),
                        },
                    ),
                ],
            },
        );
        let out = gen(&module(vec![], vec![effect]));
        assert!(out.contains("class Logger {"), "got: {out}");
        assert!(out.contains("log(level, msg) {"), "got: {out}");
        assert!(out.contains("flush() {"), "got: {out}");
        assert!(
            out.contains("throw new Error(\"not implemented\");"),
            "got: {out}"
        );
    }

    #[test]
    fn effect_decl_empty_operations() {
        let effect = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Empty"),
                generic_params: vec![],
                components: vec![],
                operations: vec![],
            },
        );
        let out = gen(&module(vec![], vec![effect]));
        assert!(out.contains("class Empty {"), "got: {out}");
        assert!(out.contains("}"), "got: {out}");
    }

    #[test]
    fn handling_block_passes_handlers_to_effectful_call() {
        use bock_air::AirHandlerPair;

        let effect_decl = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Logger"),
                generic_params: vec![],
                components: vec![],
                operations: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("log"),
                        generic_params: vec![],
                        params: vec![param_node(3, "msg")],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], None)),
                    },
                )],
            },
        );

        // fn inner() with Logger
        let inner_fn = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("inner"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![type_path(&["Logger"])],
                where_clause: vec![],
                body: Box::new(block(12, vec![], Some(str_lit(13, "hello")))),
            },
        );

        let call_inner = node(
            20,
            NodeKind::Call {
                callee: Box::new(id_node(21, "inner")),
                args: vec![],
                type_args: vec![],
            },
        );
        let handling = node(
            30,
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path(&["Logger"]),
                    handler: Box::new(node(
                        31,
                        NodeKind::Call {
                            callee: Box::new(id_node(32, "StdoutLogger")),
                            args: vec![],
                            type_args: vec![],
                        },
                    )),
                }],
                body: Box::new(block(33, vec![], Some(call_inner))),
            },
        );
        let main_fn = node(
            40,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(41, vec![handling], None)),
            },
        );

        let out = gen(&module(vec![], vec![effect_decl, inner_fn, main_fn]));
        // JS: inner({ logger: __logger })
        assert!(
            out.contains("inner({ logger: __logger })"),
            "handling block should pass handler to effectful call, got: {out}"
        );
        assert!(
            out.contains("const __logger = stdoutLogger()"),
            "handling block should instantiate handler, got: {out}"
        );
    }

    #[test]
    fn sibling_handling_blocks_do_not_share_let_scope() {
        use bock_air::AirHandlerPair;

        // Two *sibling* `handling` blocks, each `let part = …` under the SAME
        // name. Each block lowers to its own `{ … }` JS lexical scope, so both
        // must declare a fresh `const part` — neither may be rewritten into a
        // bare `part = …` assignment against the other (which would reference a
        // name that went out of scope when the first block closed). Regression
        // for Q-js-handling-let-redeclaration.
        let make_handling = |id: u32, val: &str| {
            node(
                id,
                NodeKind::HandlingBlock {
                    handlers: vec![AirHandlerPair {
                        effect: type_path(&["Logger"]),
                        handler: Box::new(node(
                            id + 1,
                            NodeKind::Call {
                                callee: Box::new(id_node(id + 2, "StdoutLogger")),
                                args: vec![],
                                type_args: vec![],
                            },
                        )),
                    }],
                    body: Box::new(block(
                        id + 3,
                        vec![let_binding(id + 4, "part", false, str_lit(id + 6, val))],
                        Some(id_node(id + 7, "part")),
                    )),
                },
            )
        };
        let main_fn = node(
            40,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    41,
                    vec![make_handling(50, "first"), make_handling(70, "second")],
                    None,
                )),
            },
        );

        let out = gen(&module(vec![], vec![main_fn]));
        assert_eq!(
            out.matches("const part = ").count(),
            2,
            "each sibling handling block should declare its own `const part`, got: {out}"
        );
        assert!(
            !out.contains("\n  part = "),
            "no sibling handling block may rewrite its `let part` into a bare \
             assignment, got: {out}"
        );
    }

    #[test]
    fn composite_effect_expands_to_components() {
        use bock_air::AirHandlerPair;

        // effect Logger { fn log(msg: String) -> Void }
        let logger_decl = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Logger"),
                generic_params: vec![],
                components: vec![],
                operations: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("log"),
                        generic_params: vec![],
                        params: vec![param_node(3, "msg")],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], None)),
                    },
                )],
            },
        );

        // effect Clock { fn now() -> Int }
        let clock_decl = node(
            5,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Clock"),
                generic_params: vec![],
                components: vec![],
                operations: vec![node(
                    6,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("now"),
                        generic_params: vec![],
                        params: vec![],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(7, vec![], None)),
                    },
                )],
            },
        );

        // effect ServiceStack = Logger + Clock
        let composite_decl = node(
            8,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("ServiceStack"),
                generic_params: vec![],
                components: vec![type_path(&["Logger"]), type_path(&["Clock"])],
                operations: vec![],
            },
        );

        // fn serve(request) with ServiceStack → should expand to Logger + Clock params
        let serve_fn = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("serve"),
                generic_params: vec![],
                params: vec![param_node(11, "request")],
                return_type: None,
                effect_clause: vec![type_path(&["ServiceStack"])],
                where_clause: vec![],
                body: Box::new(block(12, vec![], Some(str_lit(13, "ok")))),
            },
        );

        // main with handling block
        let call_serve = node(
            20,
            NodeKind::Call {
                callee: Box::new(id_node(21, "serve")),
                args: vec![bock_air::AirArg {
                    label: None,
                    value: str_lit(22, "GET /"),
                }],
                type_args: vec![],
            },
        );
        let handling = node(
            30,
            NodeKind::HandlingBlock {
                handlers: vec![
                    AirHandlerPair {
                        effect: type_path(&["Logger"]),
                        handler: Box::new(node(
                            31,
                            NodeKind::Call {
                                callee: Box::new(id_node(32, "StdLogger")),
                                args: vec![],
                                type_args: vec![],
                            },
                        )),
                    },
                    AirHandlerPair {
                        effect: type_path(&["Clock"]),
                        handler: Box::new(node(
                            33,
                            NodeKind::Call {
                                callee: Box::new(id_node(34, "StdClock")),
                                args: vec![],
                                type_args: vec![],
                            },
                        )),
                    },
                ],
                body: Box::new(block(35, vec![], Some(call_serve))),
            },
        );
        let main_fn = node(
            40,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(41, vec![handling], None)),
            },
        );

        let out = gen(&module(
            vec![],
            vec![logger_decl, clock_decl, composite_decl, serve_fn, main_fn],
        ));

        // Composite effect should emit a comment, not a class.
        assert!(
            out.contains("// composite effect ServiceStack = Logger + Clock"),
            "composite effect should be a comment, got: {out}"
        );
        assert!(
            !out.contains("class ServiceStack"),
            "composite effect should NOT generate a class, got: {out}"
        );
        // A *public* composite effect is also listed in the per-module
        // `export { … }`, so it must have a concrete binding to export (an
        // unbound name is an ESM "Export 'X' is not defined" error). It emits a
        // frozen marker object recording its component names.
        assert!(
            out.contains(
                "const ServiceStack = Object.freeze({ __composite: [\"Logger\", \"Clock\"] });"
            ),
            "composite effect should emit an exportable binding, got: {out}"
        );

        // serve should have expanded handler params for Logger + Clock.
        assert!(
            out.contains("function serve(request, { logger, clock })"),
            "serve should have expanded effect params, got: {out}"
        );

        // Calling serve from handling block should pass both handlers.
        assert!(
            out.contains("logger: __logger") && out.contains("clock: __clock"),
            "call should pass expanded handler args, got: {out}"
        );
    }

    #[test]
    fn record_becomes_class_for_prototype_impls() {
        use bock_air::AirHandlerPair;

        let rec = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("ConsoleLogger"),
                generic_params: vec![],
                fields: vec![],
            },
        );
        let out = gen(&module(vec![], vec![rec]));
        assert!(
            out.contains("class ConsoleLogger {}"),
            "empty record should be an empty class, got: {out}"
        );
        let _ = AirHandlerPair {
            // keep import used
            effect: type_path(&["X"]),
            handler: Box::new(id_node(0, "x")),
        };
    }

    #[test]
    fn record_construct_of_declared_record_uses_new() {
        let rec = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("ConsoleLogger"),
                generic_params: vec![],
                fields: vec![],
            },
        );
        let construct = node(
            2,
            NodeKind::RecordConstruct {
                path: type_path(&["ConsoleLogger"]),
                fields: vec![],
                spread: None,
            },
        );
        let let_stmt = node(
            3,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(4, "x")),
                ty: None,
                value: Box::new(construct),
            },
        );
        let f = node(
            5,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(6, vec![let_stmt], None)),
            },
        );
        let out = gen(&module(vec![], vec![rec, f]));
        assert!(
            out.contains("new ConsoleLogger()"),
            "declared record construct should use `new`, got: {out}"
        );
    }

    #[test]
    fn module_handle_registers_handler_for_same_module_calls() {
        use bock_air::AirHandlerPair;
        let _ = AirHandlerPair {
            effect: type_path(&["X"]),
            handler: Box::new(id_node(0, "x")),
        };

        // effect Logger { fn log(msg) }
        let effect_decl = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Logger"),
                generic_params: vec![],
                components: vec![],
                operations: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("log"),
                        generic_params: vec![],
                        params: vec![param_node(3, "msg")],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], None)),
                    },
                )],
            },
        );

        // record StdoutLogger {}
        let rec = node(
            5,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("StdoutLogger"),
                generic_params: vec![],
                fields: vec![],
            },
        );

        // fn greet() with Logger { log("hi") }
        let greet = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("greet"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![type_path(&["Logger"])],
                where_clause: vec![],
                body: Box::new(block(11, vec![], Some(str_lit(12, "hi")))),
            },
        );

        // handle Logger with StdoutLogger {}
        let module_handle = node(
            20,
            NodeKind::ModuleHandle {
                effect: type_path(&["Logger"]),
                handler: Box::new(node(
                    21,
                    NodeKind::RecordConstruct {
                        path: type_path(&["StdoutLogger"]),
                        fields: vec![],
                        spread: None,
                    },
                )),
            },
        );

        // fn main() { greet() }
        let call_greet = node(
            30,
            NodeKind::Call {
                callee: Box::new(id_node(31, "greet")),
                args: vec![],
                type_args: vec![],
            },
        );
        let main_fn = node(
            32,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(33, vec![], Some(call_greet))),
            },
        );

        let out = gen(&module(
            vec![],
            vec![effect_decl, rec, greet, module_handle, main_fn],
        ));

        // Module handle creates __logger with new.
        assert!(
            out.contains("const __logger = new StdoutLogger()"),
            "module handle should use `new` on declared record, got: {out}"
        );
        // Calls in main() pick up the module-level handler.
        assert!(
            out.contains("greet({ logger: __logger })"),
            "module handle should be threaded into effectful calls, got: {out}"
        );
    }

    // ── Async entry point ───────────────────────────────────────────────────

    #[test]
    fn entry_invocation_sync_main() {
        let inv = JsGenerator::new().entry_invocation(false).unwrap();
        assert_eq!(inv, "main();\n");
    }

    #[test]
    fn entry_invocation_async_main() {
        let inv = JsGenerator::new().entry_invocation(true).unwrap();
        assert!(inv.contains("async () =>"));
        assert!(inv.contains("await main()"));
    }

    #[test]
    fn generate_project_async_main_wraps_entry() {
        let main_fn = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(2, vec![], None)),
            },
        );
        let m = module(vec![], vec![main_fn]);
        let gen = JsGenerator::new();
        let src_path = std::path::Path::new("src/main.bock");
        let out = gen.generate_project(&[(&m, src_path)]).unwrap();
        let src = &out.files[0].content;
        assert_eq!(out.files[0].path, std::path::PathBuf::from("main.js"));
        assert!(src.contains("async function main()"), "got: {src}");
        assert!(
            src.contains("(async () => { await main(); })();"),
            "async entry wrapper missing, got: {src}"
        );
    }

    /// A module node with a declared dotted `path` (e.g. `core.option`), used by
    /// the per-module emission tests where the file layout and the relative
    /// import specifier are keyed on the declared module-path.
    fn module_with_path(path: &[&str], imports: Vec<AIRNode>, items: Vec<AIRNode>) -> AIRNode {
        node(
            0,
            NodeKind::Module {
                path: Some(bock_ast::ModulePath {
                    segments: path.iter().map(|s| ident(s)).collect(),
                    span: span(),
                }),
                annotations: vec![],
                imports,
                items,
            },
        )
    }

    /// An `import <path>.{ name }` AIR node (a single-item `Named` import).
    fn import_named(id: u32, path: &[&str], name: &str) -> AIRNode {
        node(
            id,
            NodeKind::ImportDecl {
                path: bock_ast::ModulePath {
                    segments: path.iter().map(|s| ident(s)).collect(),
                    span: span(),
                },
                items: bock_ast::ImportItems::Named(vec![bock_ast::ImportedName {
                    span: span(),
                    name: ident(name),
                    alias: None,
                }]),
            },
        )
    }

    /// A bare `fn <name>() -> <tail>` declaration with the given visibility and a
    /// single tail expression as its body.
    fn fn_decl_tail(id: u32, vis: Visibility, name: &str, tail: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: vis,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(id + 1, vec![], Some(tail))),
            },
        )
    }

    #[test]
    fn per_module_emits_native_esm_import_tree() {
        // entry `module main` uses `mathutil.add_one`; `module mathutil` exports a
        // `public fn add_one`. Per-module emission must produce `main.js` (with a
        // real `import { addOne } from "./mathutil.js"` — note the camelCase) and
        // `mathutil.js` — a real import tree, not a single collapsed file. The
        // `package.json` run affordance is emitted by the scaffolder (project
        // mode), NOT codegen (S6a / DV18).
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "add_one")),
                args: vec![bock_air::AirArg {
                    label: None,
                    value: int_lit(12, "6"),
                }],
                type_args: vec![],
            },
        );
        let main_mod = module_with_path(
            &["main"],
            vec![import_named(5, &["mathutil"], "add_one")],
            vec![fn_decl_tail(1, Visibility::Private, "main", call)],
        );
        let util_mod = module_with_path(
            &["mathutil"],
            vec![],
            vec![fn_decl_tail(
                20,
                Visibility::Public,
                "add_one",
                int_lit(22, "7"),
            )],
        );

        let gen = JsGenerator::new();
        let out = gen
            .generate_project(&[
                (&main_mod, std::path::Path::new("src/main.bock")),
                (&util_mod, std::path::Path::new("src/mathutil.bock")),
            ])
            .unwrap();

        let by_name = |p: &str| out.files.iter().find(|f| f.path == std::path::Path::new(p));
        let main_file = by_name("main.js").expect("main.js emitted");
        let util_file = by_name("mathutil.js").expect("mathutil.js emitted");
        // Codegen no longer emits the run affordance (S6a / DV18) — the
        // scaffolder owns the `package.json` in project mode.
        assert!(
            by_name("package.json").is_none(),
            "codegen must NOT emit package.json — the scaffolder owns it (S6a)"
        );

        assert!(
            main_file
                .content
                .contains("import { addOne } from \"./mathutil.js\";"),
            "main.js must import the camelCased fn from the sibling; got:\n{}",
            main_file.content
        );
        assert!(
            main_file.content.contains("main();"),
            "main.js must carry the entry invocation; got:\n{}",
            main_file.content
        );
        assert!(
            util_file.content.contains("export function addOne("),
            "mathutil.js must export the fn inline; got:\n{}",
            util_file.content
        );
    }

    #[test]
    fn per_module_reexports_record_and_constructs_cross_module() {
        // entry uses `shapes.Point`; `module shapes` declares `public record
        // Point`. Per-module emission must re-export `Point` from `shapes.js`
        // (records do not export inline in JS) and `main.js` must import it.
        let point = node(
            30,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Point"),
                generic_params: vec![],
                fields: vec![],
            },
        );
        let shapes_mod = module_with_path(&["shapes"], vec![], vec![point]);
        let ctor = node(
            10,
            NodeKind::RecordConstruct {
                path: type_path(&["Point"]),
                fields: vec![],
                spread: None,
            },
        );
        let main_mod = module_with_path(
            &["main"],
            vec![import_named(5, &["shapes"], "Point")],
            vec![fn_decl_tail(1, Visibility::Private, "main", ctor)],
        );

        let gen = JsGenerator::new();
        let out = gen
            .generate_project(&[
                (&main_mod, std::path::Path::new("src/main.bock")),
                (&shapes_mod, std::path::Path::new("src/shapes.bock")),
            ])
            .unwrap();
        let by_name = |p: &str| out.files.iter().find(|f| f.path == std::path::Path::new(p));
        let main_file = by_name("main.js").expect("main.js emitted");
        let shapes_file = by_name("shapes.js").expect("shapes.js emitted");
        assert!(
            shapes_file.content.contains("export { Point };"),
            "shapes.js must re-export the record; got:\n{}",
            shapes_file.content
        );
        assert!(
            main_file
                .content
                .contains("import { Point } from \"./shapes.js\";"),
            "main.js must import the record; got:\n{}",
            main_file.content
        );
        // Cross-module record construction must lower to `new Point(...)`, not a
        // bare object literal (record_names seeded across the reachable set).
        assert!(
            main_file.content.contains("new Point("),
            "cross-module record construction must use `new`; got:\n{}",
            main_file.content
        );
    }

    /// A `match` whose scrutinee is a call must be hoisted into a single
    /// `const __matchN = …;` so it is evaluated once. Re-emitting the call
    /// inline in each arm double-evaluated it — a real bug for a scrutinee with
    /// side effects (e.g. a stateful iterator's `match next(it)`).
    #[test]
    fn match_call_scrutinee_hoisted_to_temp() {
        // match f() { Some(x) => x; None => 0 }
        let scrutinee = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "f")),
                args: vec![],
                type_args: vec![],
            },
        );
        let some_arm = node(
            20,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    21,
                    NodeKind::ConstructorPat {
                        path: type_path(&["Some"]),
                        fields: vec![bind_pat(22, "x")],
                    },
                )),
                guard: None,
                body: Box::new(block(23, vec![], Some(id_node(24, "x")))),
            },
        );
        let none_arm = node(
            30,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    31,
                    NodeKind::ConstructorPat {
                        path: type_path(&["None"]),
                        fields: vec![],
                    },
                )),
                guard: None,
                body: Box::new(block(32, vec![], Some(int_lit(33, "0")))),
            },
        );
        let match_stmt = node(
            40,
            NodeKind::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![some_arm, none_arm],
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("run"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(2, vec![match_stmt], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("const __match1 = f();"),
            "call scrutinee should be hoisted to a temp, got: {out}"
        );
        assert!(
            out.contains("switch (__match1._tag)"),
            "switch should dispatch on the hoisted temp, got: {out}"
        );
        assert!(
            out.contains("const x = __match1._0;"),
            "payload binding should read the hoisted temp, got: {out}"
        );
        assert!(
            !out.contains("f()._tag") && !out.contains("f()._0"),
            "call scrutinee must not be re-emitted inline, got: {out}"
        );
    }

    #[test]
    fn method_colliding_with_field_is_disambiguated() {
        // record SimpleError { message: String }
        let record_decl = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("SimpleError"),
                generic_params: vec![],
                fields: vec![bock_ast::RecordDeclField {
                    id: 0,
                    span: span(),
                    name: ident("message"),
                    ty: bock_ast::TypeExpr::Named {
                        id: 0,
                        span: span(),
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                    default: None,
                }],
            },
        );
        // impl Error for SimpleError { fn message(self) { self.message } }
        let method = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("message"),
                generic_params: vec![],
                params: vec![param_node(11, "self")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    12,
                    vec![],
                    Some(node(
                        13,
                        NodeKind::FieldAccess {
                            object: Box::new(id_node(14, "self")),
                            field: ident("message"),
                        },
                    )),
                )),
            },
        );
        let impl_block = node(
            20,
            NodeKind::ImplBlock {
                annotations: vec![],
                target: Box::new(node(
                    21,
                    NodeKind::TypeNamed {
                        path: type_path(&["SimpleError"]),
                        args: vec![],
                    },
                )),
                trait_path: Some(type_path(&["Error"])),
                trait_args: vec![],
                generic_params: vec![],
                where_clause: vec![],
                methods: vec![method],
            },
        );
        // fn read(e: SimpleError) { e.message() }  → Call(FieldAccess(e,message),[e])
        let read_fn = node(
            30,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("read"),
                generic_params: vec![],
                params: vec![param_node(31, "e")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    32,
                    vec![],
                    Some(node(
                        33,
                        NodeKind::Call {
                            callee: Box::new(node(
                                34,
                                NodeKind::FieldAccess {
                                    // The lowerer reuses the *same* receiver node
                                    // in both the field-access object and the self
                                    // arg; `desugared_self_call` keys on the shared
                                    // NodeId, so the test must too.
                                    object: Box::new(id_node(35, "e")),
                                    field: ident("message"),
                                },
                            )),
                            type_args: vec![],
                            args: vec![AirArg {
                                label: None,
                                value: id_node(35, "e"),
                            }],
                        },
                    )),
                )),
            },
        );
        let out = gen(&module(vec![], vec![record_decl, impl_block, read_fn]));
        // The field is still set on the instance under its own name.
        assert!(
            out.contains("this.message = message"),
            "field should remain `message`, got: {out}"
        );
        // The method (prototype) and call site are renamed to `messageMethod`.
        assert!(
            out.contains("SimpleError.prototype.messageMethod = "),
            "prototype method should be `messageMethod`, got: {out}"
        );
        assert!(
            out.contains(".messageMethod(e)"),
            "call site should be `.messageMethod(e)`, got: {out}"
        );
        // The method body still *reads* the field via `self.message`.
        assert!(
            out.contains("return self.message;"),
            "method body should read the field `self.message`, got: {out}"
        );
    }

    // ── js codegen fixes (examples audit) ────────────────────────────────────

    /// Helpers for the audit-fix tests below.
    fn let_binding(id: u32, name: &str, is_mut: bool, value: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::LetBinding {
                is_mut,
                pattern: Box::new(node(
                    id + 1,
                    NodeKind::BindPat {
                        name: ident(name),
                        is_mut,
                    },
                )),
                ty: None,
                value: Box::new(value),
            },
        )
    }

    fn fn_decl(id: u32, name: &str, params: Vec<AIRNode>, body: AIRNode) -> AIRNode {
        node(
            id,
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

    fn match_arm(id: u32, pattern: AIRNode, guard: Option<AIRNode>, body: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::MatchArm {
                pattern: Box::new(pattern),
                guard: guard.map(Box::new),
                body: Box::new(body),
            },
        )
    }

    #[test]
    fn rebound_let_lowers_to_assignment_not_redeclaration() {
        // fn f() { let acc = 1; let acc = 2; let acc = 3 }
        let body = block(
            1,
            vec![
                let_binding(10, "acc", false, int_lit(11, "1")),
                let_binding(20, "acc", false, int_lit(21, "2")),
                let_binding(30, "acc", false, int_lit(31, "3")),
            ],
            None,
        );
        let out = gen(&module(vec![], vec![fn_decl(2, "f", vec![], body)]));
        // First binding declares `let` (re-bound later), subsequent ones assign.
        assert!(
            out.contains("let acc = 1;"),
            "first re-bound `let` should declare with `let`, got: {out}"
        );
        assert_eq!(
            out.matches("acc = ").count(),
            3,
            "all three bindings should reference `acc`, got: {out}"
        );
        assert!(
            !out.contains("const acc"),
            "a re-bound binding must not emit `const acc`, got: {out}"
        );
    }

    #[test]
    fn let_shadowing_a_param_lowers_to_assignment() {
        // fn f(x) { let x = x + 1; x }  — `let x` shadows the param in the same
        // JS block scope, so it must become an assignment, not a redeclaration.
        let rebind = let_binding(
            10,
            "x",
            false,
            node(
                12,
                NodeKind::BinaryOp {
                    op: BinOp::Add,
                    left: Box::new(id_node(13, "x")),
                    right: Box::new(int_lit(14, "1")),
                },
            ),
        );
        let body = block(1, vec![rebind], Some(id_node(15, "x")));
        let out = gen(&module(
            vec![],
            vec![fn_decl(2, "f", vec![param_node(3, "x")], body)],
        ));
        assert!(
            out.contains("x = (x + 1);"),
            "let shadowing a param should assign, got: {out}"
        );
        assert!(
            !out.contains("let x = (x + 1)") && !out.contains("const x = (x + 1)"),
            "let shadowing a param must not redeclare `x`, got: {out}"
        );
    }

    #[test]
    fn sibling_iife_blocks_do_not_share_let_scope() {
        // Two arms of an expression-position match each `let x = …`. Lowered to
        // sibling IIFEs, each is its own scope, so both may use a fresh `const`.
        let arm1 = match_arm(
            40,
            node(
                41,
                NodeKind::LiteralPat {
                    lit: Literal::Int("0".into()),
                },
            ),
            None,
            block(
                42,
                vec![let_binding(43, "x", false, int_lit(44, "1"))],
                Some(id_node(45, "x")),
            ),
        );
        let arm2 = match_arm(
            50,
            node(51, NodeKind::WildcardPat),
            None,
            block(
                52,
                vec![let_binding(53, "x", false, int_lit(54, "2"))],
                Some(id_node(55, "x")),
            ),
        );
        let m = node(
            60,
            NodeKind::Match {
                scrutinee: Box::new(id_node(61, "n")),
                arms: vec![arm1, arm2],
            },
        );
        let body = block(1, vec![], Some(m));
        let out = gen(&module(
            vec![],
            vec![fn_decl(2, "f", vec![param_node(3, "n")], body)],
        ));
        // Both arm bodies independently declare `const x` (separate IIFE scopes);
        // neither is rewritten into an assignment against the other.
        assert_eq!(
            out.matches("const x = ").count(),
            2,
            "sibling IIFE arms should each declare their own `const x`, got: {out}"
        );
    }

    #[test]
    fn list_pattern_match_routes_to_ifchain() {
        // match xs { [] => 0; [first, ..rest] => first }
        let empty_arm = match_arm(
            10,
            node(
                11,
                NodeKind::ListPat {
                    elems: vec![],
                    rest: None,
                },
            ),
            None,
            block(12, vec![], Some(int_lit(13, "0"))),
        );
        let cons_arm = match_arm(
            20,
            node(
                21,
                NodeKind::ListPat {
                    elems: vec![bind_pat(22, "first")],
                    rest: Some(Box::new(bind_pat(23, "rest"))),
                },
            ),
            None,
            block(24, vec![], Some(id_node(25, "first"))),
        );
        let m = node(
            30,
            NodeKind::Match {
                scrutinee: Box::new(id_node(31, "xs")),
                arms: vec![empty_arm, cons_arm],
            },
        );
        let body = block(1, vec![], Some(m));
        let out = gen(&module(
            vec![],
            vec![fn_decl(2, "f", vec![param_node(3, "xs")], body)],
        ));
        // List-pattern matches must use the if/else-if chain (never a `switch`
        // with multiple `default`s), with array length tests and a `..rest`
        // slice binding.
        assert!(
            !out.contains("switch"),
            "list-pattern match must not lower to a switch, got: {out}"
        );
        assert!(
            out.contains("xs.length === 0"),
            "empty-list arm should test `length === 0`, got: {out}"
        );
        // The cons arm is the final unguarded arm, so it is the chain's `else`
        // (Bock matches are exhaustive) — no explicit length test, but it binds
        // the head element and the `..rest` slice.
        assert!(
            out.contains("const first = xs[0];"),
            "cons arm should bind the head element, got: {out}"
        );
        assert!(
            out.contains("const rest = xs.slice(1);"),
            "`..rest` should bind the trailing slice, got: {out}"
        );
    }

    #[test]
    fn range_pattern_match_routes_to_ifchain() {
        // match n { 1..10 => "lo"; _ => "hi" }
        let range_arm = match_arm(
            10,
            node(
                11,
                NodeKind::RangePat {
                    lo: Box::new(int_lit(12, "1")),
                    hi: Box::new(int_lit(13, "10")),
                    inclusive: false,
                },
            ),
            None,
            block(14, vec![], Some(str_lit(15, "lo"))),
        );
        let wild_arm = match_arm(
            20,
            node(21, NodeKind::WildcardPat),
            None,
            block(22, vec![], Some(str_lit(23, "hi"))),
        );
        let m = node(
            30,
            NodeKind::Match {
                scrutinee: Box::new(id_node(31, "n")),
                arms: vec![range_arm, wild_arm],
            },
        );
        let body = block(1, vec![], Some(m));
        let out = gen(&module(
            vec![],
            vec![fn_decl(2, "f", vec![param_node(3, "n")], body)],
        ));
        assert!(
            !out.contains("switch"),
            "range-pattern match must not lower to a switch, got: {out}"
        );
        assert!(
            out.contains("n >= 1 && n < 10"),
            "exclusive range should test `>= lo && < hi`, got: {out}"
        );
    }

    #[test]
    fn mut_bind_arm_emits_let_not_const() {
        // match n { mut x => { x = x + 1; x } }
        let mut_pat = node(
            11,
            NodeKind::BindPat {
                name: ident("x"),
                is_mut: true,
            },
        );
        let assign = node(
            12,
            NodeKind::Assign {
                op: AssignOp::Assign,
                target: Box::new(id_node(13, "x")),
                value: Box::new(node(
                    14,
                    NodeKind::BinaryOp {
                        op: BinOp::Add,
                        left: Box::new(id_node(15, "x")),
                        right: Box::new(int_lit(16, "1")),
                    },
                )),
            },
        );
        let arm = match_arm(
            10,
            mut_pat,
            None,
            block(17, vec![assign], Some(id_node(18, "x"))),
        );
        let m = node(
            30,
            NodeKind::Match {
                scrutinee: Box::new(id_node(31, "n")),
                arms: vec![arm],
            },
        );
        let body = block(1, vec![], Some(m));
        let out = gen(&module(
            vec![],
            vec![fn_decl(2, "f", vec![param_node(3, "n")], body)],
        ));
        assert!(
            out.contains("let x = n;"),
            "a `mut` arm binding should declare with `let`, got: {out}"
        );
        assert!(
            !out.contains("const x = n;"),
            "a `mut` arm binding must not be `const`, got: {out}"
        );
    }

    #[test]
    fn guard_let_binds_pattern_into_enclosing_scope() {
        // fn f(s) { guard (let Ok(v) = parse(s)) else { return }; v }
        let guard = node(
            10,
            NodeKind::Guard {
                let_pattern: Some(Box::new(node(
                    11,
                    NodeKind::ConstructorPat {
                        path: type_path(&["Ok"]),
                        fields: vec![bind_pat(12, "v")],
                    },
                ))),
                condition: Box::new(node(
                    13,
                    NodeKind::Call {
                        callee: Box::new(id_node(14, "parse")),
                        type_args: vec![],
                        args: vec![AirArg {
                            label: None,
                            value: id_node(15, "s"),
                        }],
                    },
                )),
                else_block: Box::new(block(
                    16,
                    vec![node(17, NodeKind::Return { value: None })],
                    None,
                )),
            },
        );
        let body = block(1, vec![guard], Some(id_node(18, "v")));
        let out = gen(&module(
            vec![],
            vec![fn_decl(2, "f", vec![param_node(3, "s")], body)],
        ));
        // The pattern is tested and `v` is bound for use *after* the guard.
        assert!(
            out.contains("._tag === \"Ok\""),
            "guard should test the constructor tag, got: {out}"
        );
        assert!(
            out.contains("const v = "),
            "guard should bind `v` into the enclosing scope, got: {out}"
        );
    }

    #[test]
    fn eval_identifier_is_escaped_for_strict_mode() {
        // fn eval() { 0 }
        let body = block(1, vec![], Some(int_lit(11, "0")));
        let out = gen(&module(vec![], vec![fn_decl(2, "eval", vec![], body)]));
        assert!(
            out.contains("function eval_("),
            "a fn named `eval` must be escaped to `eval_` (strict mode), got: {out}"
        );
        assert!(
            !out.contains("function eval("),
            "bare `function eval(` is a strict-mode SyntaxError, got: {out}"
        );
    }

    /// Build `fn f() { let x = if (c) { 1 } else { return 0 }  x }` — a value-
    /// position `if` whose else branch diverges via `return`. The shared
    /// value-CF hoist must lower it to a declare-then-assign temp, never an IIFE
    /// (which would capture the `return`) or `/* unsupported */`.
    fn diverging_value_if_fn() -> AIRNode {
        let then_b = block(2, vec![], Some(int_lit(3, "1")));
        let ret = node(
            5,
            NodeKind::Return {
                value: Some(Box::new(int_lit(6, "0"))),
            },
        );
        let else_b = block(4, vec![], Some(ret));
        let if_node = node(
            1,
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(id_node(7, "c")),
                then_block: Box::new(then_b),
                else_block: Some(Box::new(else_b)),
            },
        );
        let let_x = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "x")),
                ty: None,
                value: Box::new(if_node),
            },
        );
        let body = block(20, vec![let_x], Some(id_node(21, "x")));
        let f = node(
            30,
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
        module(vec![], vec![f])
    }

    #[test]
    fn diverging_value_if_hoists_to_stmt_form_no_iife() {
        let out = gen(&diverging_value_if_fn());
        assert!(
            !out.contains("/* unsupported */"),
            "diverging value-if must not emit `/* unsupported */`, got: {out}"
        );
        // The value arm assigns the hoisted temp; the diverging arm keeps return.
        assert!(
            out.contains("bockCf0 = MessageType") || out.contains("bockCf0 = 1"),
            "value arm must assign the temp, got: {out}"
        );
        assert!(
            out.contains("return 0"),
            "diverging arm must keep its return (not wrapped in an IIFE), got: {out}"
        );
    }

    // ── `?` propagation (Q-propagate-operator-noop) ──────────────────────────

    fn call_node(id: u32, callee: &str, args: Vec<AIRNode>) -> AIRNode {
        node(
            id,
            NodeKind::Call {
                callee: Box::new(id_node(id + 1, callee)),
                type_args: vec![],
                args: args
                    .into_iter()
                    .map(|value| AirArg { label: None, value })
                    .collect(),
            },
        )
    }

    fn propagate(id: u32, expr: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::Propagate {
                expr: Box::new(expr),
            },
        )
    }

    fn ctor_call(id: u32, name: &str, arg: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::Call {
                callee: Box::new(id_node(id + 1, name)),
                type_args: vec![],
                args: vec![AirArg {
                    label: None,
                    value: arg,
                }],
            },
        )
    }

    #[test]
    fn propagate_in_let_rhs_unwraps_and_early_returns() {
        // fn f(x) { let v = g(x)?  Ok(v) }
        let let_v = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "v")),
                ty: None,
                value: Box::new(propagate(12, call_node(13, "g", vec![id_node(15, "x")]))),
            },
        );
        let tail = ctor_call(20, "Ok", id_node(22, "v"));
        let body = block(1, vec![let_v], Some(tail));
        let out = gen(&module(
            vec![],
            vec![fn_decl(2, "f", vec![param_node(3, "x")], body)],
        ));
        // The `?` must hoist into a temp, early-return on the failure tag, and
        // bind the unwrapped payload — never pass the wrapped value through.
        assert!(
            out.contains("g(x)"),
            "the propagated expr must be evaluated, got: {out}"
        );
        assert!(
            out.contains("_tag === \"Err\"") || out.contains("_tag === \"None\""),
            "`?` must early-return on the failure tag, got: {out}"
        );
        assert!(
            out.contains("return __try"),
            "`?` must early-return the wrapped failure value, got: {out}"
        );
        assert!(
            out.contains("const v = __try0._0") || out.contains("const v = __try1._0"),
            "`?` must bind the unwrapped payload (`._0`), got: {out}"
        );
    }

    #[test]
    fn propagate_in_statement_position_early_returns() {
        // fn f(x) { save(x)?  Ok(0) }
        let stmt = propagate(10, call_node(11, "save", vec![id_node(13, "x")]));
        let tail = ctor_call(20, "Ok", int_lit(22, "0"));
        let body = block(1, vec![stmt], Some(tail));
        let out = gen(&module(
            vec![],
            vec![fn_decl(2, "f", vec![param_node(3, "x")], body)],
        ));
        assert!(
            out.contains("save(x)"),
            "the propagated call must be evaluated, got: {out}"
        );
        assert!(
            out.contains("_tag === \"Err\"") || out.contains("_tag === \"None\""),
            "statement-position `?` must early-return on the failure tag, got: {out}"
        );
        assert!(
            !out.contains("/* unsupported */"),
            "statement-position `?` must not emit `/* unsupported */`, got: {out}"
        );
    }

    // ── Tail-position value-less control flow (guessing-game `loop`) ──────────

    #[test]
    fn tail_position_loop_emits_as_statement_not_unsupported() {
        // fn f() { loop { break } }  — a value-less loop in tail position must
        // lower to a `while (true) { … }` statement, never `return /* … */`.
        let brk = node(40, NodeKind::Break { value: None });
        let loop_body = block(41, vec![brk], None);
        let lp = node(
            42,
            NodeKind::Loop {
                body: Box::new(loop_body),
            },
        );
        let body = block(1, vec![], Some(lp));
        let out = gen(&module(vec![], vec![fn_decl(2, "f", vec![], body)]));
        assert!(
            !out.contains("/* unsupported */"),
            "a tail-position value-less loop must not emit `/* unsupported */`, got: {out}"
        );
        assert!(
            out.contains("while (true)"),
            "a tail-position loop must lower to `while (true)`, got: {out}"
        );
        assert!(
            !out.contains("return while"),
            "a loop must not be wrapped in `return`, got: {out}"
        );
    }

    #[test]
    fn tail_position_guard_emits_as_statement() {
        // fn f(s) { guard (let Ok(v) = parse(s)) else { return } }
        // (guard as the block tail) must emit as a statement, not `return …`.
        let guard = node(
            10,
            NodeKind::Guard {
                let_pattern: Some(Box::new(node(
                    11,
                    NodeKind::ConstructorPat {
                        path: type_path(&["Ok"]),
                        fields: vec![bind_pat(12, "v")],
                    },
                ))),
                condition: Box::new(call_node(13, "parse", vec![id_node(16, "s")])),
                else_block: Box::new(block(
                    17,
                    vec![node(18, NodeKind::Return { value: None })],
                    None,
                )),
            },
        );
        let body = block(1, vec![], Some(guard));
        let out = gen(&module(
            vec![],
            vec![fn_decl(2, "f", vec![param_node(3, "s")], body)],
        ));
        assert!(
            !out.contains("/* unsupported */"),
            "a tail-position guard must not emit `/* unsupported */`, got: {out}"
        );
        assert!(
            out.contains("._tag === \"Ok\""),
            "the guard's pattern test must survive, got: {out}"
        );
    }

    #[test]
    fn diverging_intrinsic_tail_is_a_bare_statement_not_return_throw() {
        // fn f() -> Int { todo() }  — `todo()` lowers to a bare `throw`; emitting
        // `return throw …` is a JS SyntaxError, so the tail must be a statement.
        let body = block(1, vec![], Some(call_node(10, "todo", vec![])));
        let out = gen(&module(vec![], vec![fn_decl(2, "f", vec![], body)]));
        assert!(
            out.contains("throw new Error(\"not implemented\")"),
            "`todo()` must lower to a throw, got: {out}"
        );
        assert!(
            !out.contains("return throw"),
            "`return throw …` is invalid JS; the throw must be a bare statement, got: {out}"
        );
    }

    // ── Loop / statement-position tails must be discarded, not `return`ed ─────
    //
    // A loop (`for`/`while`/`loop`) body's final expression — and a
    // statement-position `if`/`match` arm's tail — is discarded in Bock (these
    // are statements, not the function's value). The JS backend's
    // `emit_block_body_inner` had wrapped every block tail in `return`, so
    // e.g. `for i in … { println(i) }` emitted `for (…) { return
    // console.log(i); }` — the `return` aborts the function on iteration 1
    // (fizzbuzz printed one line, then `main` returned; it exited 0, so the
    // exit-code-only exec audit hid the truncation). These pin the discard
    // behaviour for each statement context, and guard that a genuine
    // value-returning tail (function body, lambda, value-position `match` IIFE)
    // still `return`s.

    /// `1..=hi` inclusive range over a `count` literal.
    fn incl_range(id: u32, lo: &str, hi: &str) -> AIRNode {
        node(
            id,
            NodeKind::Range {
                lo: Box::new(int_lit(id + 1, lo)),
                hi: Box::new(int_lit(id + 2, hi)),
                inclusive: true,
            },
        )
    }

    #[test]
    fn for_loop_body_tail_call_is_discarded_not_returned() {
        // fn main() { for i in 1..=3 { println(i) } }
        let loop_body = block(
            30,
            vec![],
            Some(call_node(31, "println", vec![id_node(33, "i")])),
        );
        let for_loop = node(
            10,
            NodeKind::For {
                pattern: Box::new(bind_pat(11, "i")),
                iterable: Box::new(incl_range(20, "1", "3")),
                body: Box::new(loop_body),
            },
        );
        let out = gen(&module(
            vec![],
            vec![fn_decl(1, "main", vec![], block(2, vec![for_loop], None))],
        ));
        assert!(
            !out.contains("return console.log"),
            "a for-loop body's tail call must be a discarded statement, not a \
             `return` (which aborts the loop after one iteration); got:\n{out}"
        );
        assert!(
            out.contains("console.log(i);"),
            "the loop body should still emit the call as a statement; got:\n{out}"
        );
    }

    #[test]
    fn while_loop_body_tail_call_is_discarded_not_returned() {
        // fn main() { while (true) { println("x") } }
        let loop_body = block(
            30,
            vec![],
            Some(call_node(31, "println", vec![str_lit(33, "x")])),
        );
        let while_loop = node(
            10,
            NodeKind::While {
                condition: Box::new(bool_lit(20, true)),
                body: Box::new(loop_body),
            },
        );
        let out = gen(&module(
            vec![],
            vec![fn_decl(1, "main", vec![], block(2, vec![while_loop], None))],
        ));
        assert!(
            !out.contains("return console.log"),
            "a while-loop body's tail call must be a discarded statement, not a \
             `return`; got:\n{out}"
        );
        assert!(
            out.contains("console.log(\"x\");"),
            "the loop body should still emit the call as a statement; got:\n{out}"
        );
    }

    #[test]
    fn infinite_loop_body_tail_call_is_discarded_not_returned() {
        // fn main() { loop { println("x") } }
        let loop_body = block(
            30,
            vec![],
            Some(call_node(31, "println", vec![str_lit(33, "x")])),
        );
        let inf_loop = node(
            10,
            NodeKind::Loop {
                body: Box::new(loop_body),
            },
        );
        let out = gen(&module(
            vec![],
            vec![fn_decl(1, "main", vec![], block(2, vec![inf_loop], None))],
        ));
        assert!(
            !out.contains("return console.log"),
            "a `loop` body's tail call must be a discarded statement, not a \
             `return`; got:\n{out}"
        );
    }

    #[test]
    fn statement_match_arm_tail_call_is_discarded_not_returned() {
        // fn run(r) { match r { Ok(v) => println(v); Err(e) => println(e) }; println("done") }
        // The trailing statement makes the `match` non-tail (statement position),
        // so its arm tails are discarded, not returned — a `return` inside the
        // `switch` would skip the `println("done")` after the match.
        let ok_arm = match_arm(
            20,
            node(
                21,
                NodeKind::ConstructorPat {
                    path: type_path(&["Ok"]),
                    fields: vec![bind_pat(22, "v")],
                },
            ),
            None,
            block(
                23,
                vec![],
                Some(call_node(24, "println", vec![id_node(26, "v")])),
            ),
        );
        let err_arm = match_arm(
            30,
            node(
                31,
                NodeKind::ConstructorPat {
                    path: type_path(&["Err"]),
                    fields: vec![bind_pat(32, "e")],
                },
            ),
            None,
            block(
                33,
                vec![],
                Some(call_node(34, "println", vec![id_node(36, "e")])),
            ),
        );
        let match_stmt = node(
            40,
            NodeKind::Match {
                scrutinee: Box::new(id_node(41, "r")),
                arms: vec![ok_arm, err_arm],
            },
        );
        let trailer = call_node(50, "println", vec![str_lit(52, "done")]);
        let body = block(3, vec![match_stmt], Some(trailer));
        let out = gen(&module(
            vec![],
            vec![fn_decl(1, "run", vec![param_node(2, "r")], body)],
        ));
        assert!(
            out.contains("console.log(v);") && out.contains("console.log(e);"),
            "statement-position match arms should emit their tail call as a \
             discarded statement; got:\n{out}"
        );
        assert!(
            !out.contains("return console.log(v);") && !out.contains("return console.log(e);"),
            "a statement-position match arm's tail call must be a discarded \
             statement, not a `return` (which would skip the code after the \
             match); got:\n{out}"
        );
    }

    #[test]
    fn loop_in_function_body_does_not_discard_the_function_tail() {
        // fn total() { let mut sum = 0; for i in 1..=3 { sum = sum + i }; sum }
        // The loop body discards its (absent) tail, but the function's own tail
        // `sum` must still `return` — the discard must not leak past the loop.
        let assign = node(
            40,
            NodeKind::Assign {
                op: AssignOp::Assign,
                target: Box::new(id_node(41, "sum")),
                value: Box::new(node(
                    42,
                    NodeKind::BinaryOp {
                        op: BinOp::Add,
                        left: Box::new(id_node(43, "sum")),
                        right: Box::new(id_node(44, "i")),
                    },
                )),
            },
        );
        let for_loop = node(
            10,
            NodeKind::For {
                pattern: Box::new(bind_pat(11, "i")),
                iterable: Box::new(incl_range(20, "1", "3")),
                body: Box::new(block(30, vec![assign], None)),
            },
        );
        let body = block(
            2,
            vec![let_binding(5, "sum", true, int_lit(6, "0")), for_loop],
            Some(id_node(7, "sum")),
        );
        let out = gen(&module(vec![], vec![fn_decl(1, "total", vec![], body)]));
        assert!(
            out.contains("return sum;"),
            "the function-body tail after a loop must still `return`; got:\n{out}"
        );
    }

    #[test]
    fn value_position_match_in_loop_body_still_returns_arm_values() {
        // fn main() { for i in 1..=3 { let s = match i { 1 => "one"; _ => "many" } } }
        // The `match` is in value position (a `let` initialiser), so its IIFE
        // arms must `return` their value even though the enclosing loop body is a
        // discard context — the discard must not leak into the value IIFE.
        let arm1 = match_arm(
            60,
            node(
                61,
                NodeKind::LiteralPat {
                    lit: Literal::Int("1".into()),
                },
            ),
            None,
            str_lit(62, "one"),
        );
        let arm_def = match_arm(
            70,
            node(71, NodeKind::WildcardPat),
            None,
            str_lit(72, "many"),
        );
        let match_expr = node(
            50,
            NodeKind::Match {
                scrutinee: Box::new(id_node(51, "i")),
                arms: vec![arm1, arm_def],
            },
        );
        let let_s = let_binding(40, "s", false, match_expr);
        let for_loop = node(
            10,
            NodeKind::For {
                pattern: Box::new(bind_pat(11, "i")),
                iterable: Box::new(incl_range(20, "1", "3")),
                body: Box::new(block(30, vec![let_s], None)),
            },
        );
        let out = gen(&module(
            vec![],
            vec![fn_decl(1, "main", vec![], block(2, vec![for_loop], None))],
        ));
        assert!(
            out.contains("return \"one\";") && out.contains("return \"many\";"),
            "a value-position `match` IIFE inside a loop body must still `return` \
             its arm values; got:\n{out}"
        );
    }

    #[test]
    fn lambda_in_loop_body_still_returns_its_tail() {
        // fn main() { for i in 1..=3 { let f = (x) => { x } } }
        // The lambda body's tail is the lambda's return value; the enclosing
        // loop's discard context must not turn it into a bare statement.
        let lambda = node(
            50,
            NodeKind::Lambda {
                params: vec![param_node(51, "x")],
                body: Box::new(block(52, vec![], Some(id_node(53, "x")))),
            },
        );
        let let_f = let_binding(40, "f", false, lambda);
        let for_loop = node(
            10,
            NodeKind::For {
                pattern: Box::new(bind_pat(11, "i")),
                iterable: Box::new(incl_range(20, "1", "3")),
                body: Box::new(block(30, vec![let_f], None)),
            },
        );
        let out = gen(&module(
            vec![],
            vec![fn_decl(1, "main", vec![], block(2, vec![for_loop], None))],
        ));
        assert!(
            out.contains("return x;"),
            "a lambda body's tail inside a loop must still `return`; got:\n{out}"
        );
    }

    #[test]
    fn function_body_tail_call_still_returns() {
        // Regression guard: a plain value-returning function-body tail still
        // emits `return` — the discard only applies in statement position.
        // fn greet() { println("hi") }   (last expr is the body's value)
        let body = block(
            2,
            vec![],
            Some(call_node(10, "println", vec![str_lit(12, "hi")])),
        );
        let out = gen(&module(vec![], vec![fn_decl(1, "greet", vec![], body)]));
        assert!(
            out.contains("return console.log(\"hi\");"),
            "a function-body tail call must still `return`; got:\n{out}"
        );
    }

    /// End-to-end: a `for` loop printing N lines must print all N lines, not 1.
    /// This is the fizzbuzz silent-truncation bug — the loop body's tail
    /// `println` previously `return`ed from `main` after the first iteration.
    #[test]
    fn e2e_for_loop_prints_all_iterations_not_just_first() {
        if !has_node() {
            return;
        }
        // fn main() { for i in 1..=5 { println(i) } }
        let loop_body = block(
            30,
            vec![],
            Some(call_node(31, "println", vec![id_node(33, "i")])),
        );
        let for_loop = node(
            10,
            NodeKind::For {
                pattern: Box::new(bind_pat(11, "i")),
                iterable: Box::new(incl_range(20, "1", "5")),
                body: Box::new(loop_body),
            },
        );
        let code = gen(&module(
            vec![],
            vec![fn_decl(1, "main", vec![], block(2, vec![for_loop], None))],
        ));
        let full = format!("{code}\nmain();\n");
        assert!(check_js_syntax(&full), "emitted JS must be valid:\n{full}");
        let out = run_js(&full);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines.len(),
            5,
            "the loop must print all 5 iterations (a `return` in the body would \
             stop after the first); got {} line(s):\n{out}\n--- source ---\n{full}",
            lines.len()
        );
        assert_eq!(lines, vec!["1", "2", "3", "4", "5"], "got:\n{out}");
    }
}
