//! TypeScript code generator — rule-based (Tier 2) transpilation from AIR to TS.
//!
//! Extends the JavaScript codegen with:
//! - Type annotations on parameters, return types, and bindings
//! - Generics → TS generics (preserved, not erased)
//! - Traits → TS interfaces
//! - Algebraic types → discriminated union types + tagged objects
//! - Type aliases → `type X = ...`

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::PathBuf;

use bock_air::{AIRNode, AirInterpolationPart, EnumVariantPayload, NodeKind, ResultVariant};
use bock_ast::{AssignOp, BinOp, Literal, TypeExpr, UnaryOp, Visibility};
use bock_errors::Span;
use bock_types::AIRModule;

use crate::error::CodegenError;
use crate::generator::{CodeGenerator, GeneratedCode, OutputFile, SourceMap, SourceMapping};
use crate::profile::TargetProfile;

/// Runtime helpers injected when `Channel` / `spawn` appear in a module.
/// See the analogous `CONCURRENCY_RUNTIME_JS` in `js.rs`.
/// Conservative module scan — if the serialized AIR mentions `Channel`
/// or `spawn`, emit the runtime prelude. Unused helpers are trivially
/// dead-code eliminated by downstream TS tooling.
fn module_uses_concurrency(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Channel\"") || s.contains("\"spawn\"")
    })
}

/// Runtime type for Bock `Optional[T]` in TypeScript. The *value*
/// representation is a tagged object — `{ _tag: "Some", _0: v }` or
/// `{ _tag: "None" }` (see [`TsEmitCtx::try_emit_prelude_ctor`] and the `None`
/// identifier in [`TsEmitCtx::emit_expr`]) — so the *type* must be the matching
/// discriminated union, not `T | null`. This mirrors the Go `__bockOption`
/// runtime added in the codegen-correctness workstream: type and value agree,
/// a `match` lowered to `switch (x._tag)` narrows correctly, and the two-variant
/// union is provably exhaustive (so a `string`-returning match needs no
/// `default`).
const OPTIONAL_RUNTIME_TS: &str = "\
// ── Bock Optional runtime ──
type BockOption<T> =
  | { readonly _tag: \"Some\"; readonly _0: T }
  | { readonly _tag: \"None\" };
";

/// True if the module references `Optional`, `Some`, or `None` anywhere, so the
/// Optional runtime type prelude must be emitted. A cheap structural scan over
/// the debug rendering, mirroring [`module_uses_concurrency`] and the Go
/// backend's `go_module_uses_optional`.
fn module_uses_optional(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Optional\"")
            || s.contains("TypeOptional")
            || s.contains("\"Some\"")
            || s.contains("\"None\"")
    })
}

/// Runtime type for Bock `Result[T, E]` in TypeScript. The value representation
/// is a tagged object — `{ _tag: "Ok", _0: v }` or `{ _tag: "Err", _0: e }`
/// (see [`TsEmitCtx::try_emit_prelude_ctor`], the `ResultConstruct` arm, and the
/// `Result`-match lowering) — so the type is the matching discriminated union,
/// not the structural `Ok`/`Err` aliases that previously went undefined. Both
/// arms carry the payload under the same `_0` key the match reads, so a `match r
/// { Ok(v) => …; Err(e) => … }` lowered to `switch (r._tag)` narrows correctly
/// and the two-variant union is provably exhaustive (no `default` needed). This
/// mirrors [`OPTIONAL_RUNTIME_TS`].
const RESULT_RUNTIME_TS: &str = "\
// ── Bock Result runtime ──
type BockResult<T, E> =
  | { readonly _tag: \"Ok\"; readonly _0: T }
  | { readonly _tag: \"Err\"; readonly _0: E };
";

/// True if the module references `Result`, `Ok`, or `Err` anywhere, so the
/// `Result` runtime type prelude must be emitted. Mirrors [`module_uses_optional`].
fn module_uses_result(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Result\"")
            || s.contains("ResultConstruct")
            || s.contains("\"Ok\"")
            || s.contains("\"Err\"")
    })
}

const CONCURRENCY_RUNTIME_TS: &str = "\
// ── Bock concurrency runtime ──
type __BockChannel<T> = {
  send(v: T): void;
  recv(): Promise<T>;
  close(): void;
};
const __bockChannelNew = <T>(): [__BockChannel<T>, __BockChannel<T>] => {
  const queue: T[] = [];
  const waiters: Array<(v: T) => void> = [];
  const ch: __BockChannel<T> = {
    send(v: T) {
      if (waiters.length > 0) { waiters.shift()!(v); } else { queue.push(v); }
    },
    recv(): Promise<T> {
      return new Promise<T>((resolve) => {
        if (queue.length > 0) { resolve(queue.shift()!); }
        else { waiters.push(resolve); }
      });
    },
    close() {}
  };
  return [ch, ch];
};
const __bockSpawn = <T>(x: Promise<T>): Promise<T> => x;
";

/// Runtime helpers for Bock range expressions (`0..n` / `0..=n`) in TypeScript.
/// TS has no native range value, so `for i in 0..n` lowers to
/// `for (const i of range(0, n))`. `range` is half-open, `rangeInclusive`
/// inclusive — matching Python's `range(lo, hi)` / `range(lo, hi + 1)` and
/// Rust's `lo..hi` / `lo..=hi`. Emitted once per bundle, gated on a ctx flag
/// (mirrors [`OPTIONAL_RUNTIME_TS`]).
const RANGE_RUNTIME_TS: &str = "\
// ── Bock range runtime ──
const range = (lo: number, hi: number): number[] => { const r: number[] = []; for (let i = lo; i < hi; i++) r.push(i); return r; };
const rangeInclusive = (lo: number, hi: number): number[] => { const r: number[] = []; for (let i = lo; i <= hi; i++) r.push(i); return r; };
";

/// True if the module references a `Range` node anywhere (so the range runtime
/// prelude must be emitted). Mirrors [`module_uses_optional`]. `RangePat` does
/// not contain the `Range {` substring, so match-arm range patterns do not
/// trigger the (value-only) helpers.
fn module_uses_range(items: &[AIRNode]) -> bool {
    items.iter().any(|n| format!("{n:?}").contains("Range {"))
}

/// TypeScript code generator implementing the `CodeGenerator` trait.
#[derive(Debug)]
pub struct TsGenerator {
    profile: TargetProfile,
}

impl TsGenerator {
    /// Creates a new TypeScript code generator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: TargetProfile::typescript(),
        }
    }
}

impl Default for TsGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeGenerator for TsGenerator {
    fn target(&self) -> &TargetProfile {
        &self.profile
    }

    fn generate_module(&self, module: &AIRModule) -> Result<GeneratedCode, CodegenError> {
        let mut ctx = TsEmitCtx::new();
        ctx.enum_variants =
            crate::generator::collect_enum_variants(&[(module, std::path::Path::new(""))]);
        ctx.generic_decls =
            crate::generator::collect_generic_decls(&[(module, std::path::Path::new(""))]);
        ctx.trait_decls =
            crate::generator::collect_trait_decls(&[(module, std::path::Path::new(""))]);
        ctx.exported_types =
            crate::generator::collect_exported_type_names(&[(module, std::path::Path::new(""))]);
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
            Some("(async () => { await main(); })();\n".to_string())
        } else {
            Some("main();\n".to_string())
        }
    }

    /// Bundle every module (stdlib + user, dependency-ordered) into one entry
    /// file. TypeScript shares JS's single top-level scope, so concatenating
    /// each module's declarations is valid and resolves cross-module `use`
    /// (DV13). `ImportDecl`s are dropped; each runtime prelude is emitted once.
    ///
    /// Diverges from spec §20.6.1 (one output file per module); see the
    /// `OPEN: §20.6.1` note in the bundling PR.
    fn generate_project(
        &self,
        modules: &[(&AIRModule, &std::path::Path)],
    ) -> Result<GeneratedCode, CodegenError> {
        // Bundle only modules the entry program actually `use`s (plus the entry
        // itself) — never the prelude-only stdlib (see `reachable_modules`).
        let reachable = crate::generator::reachable_modules(modules);
        let modules = reachable.as_slice();
        let Some(out_path) = crate::generator::bundle_output_path(modules, self.target()) else {
            return Ok(GeneratedCode { files: vec![] });
        };

        let mut ctx = TsEmitCtx::new();
        ctx.enum_variants = crate::generator::collect_enum_variants(modules);
        ctx.generic_decls = crate::generator::collect_generic_decls(modules);
        ctx.trait_decls = crate::generator::collect_trait_decls(modules);
        ctx.exported_types = crate::generator::collect_exported_type_names(modules);
        for (i, (module, _)) in modules.iter().enumerate() {
            if i > 0 && !ctx.buf.is_empty() && !ctx.buf.ends_with("\n\n") {
                ctx.buf.push('\n');
            }
            ctx.emit_node(module)?;
        }
        let (mut content, mappings) = ctx.finish();

        let main_is_async = modules
            .iter()
            .any(|(m, _)| crate::generator::module_main_fn_is_async(m));
        let invocation = self.entry_invocation(main_is_async);
        crate::generator::append_entry_invocation(&mut content, modules, invocation.as_ref());

        let derived_name = out_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let source_map = SourceMap {
            generated_file: derived_name,
            mappings,
            ..Default::default()
        };
        Ok(GeneratedCode {
            files: vec![OutputFile {
                path: out_path,
                content,
                source_map: Some(source_map),
            }],
        })
    }
}

// ─── Emission context ────────────────────────────────────────────────────────

/// Internal state for TypeScript emission.
struct TsEmitCtx {
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
    /// Names of effects declared in this module (for typing handler vars).
    effect_names: HashSet<String>,
    /// 1-indexed current line in `buf`, maintained incrementally.
    cur_line: u32,
    /// 1-indexed current column (char count) in `buf`, maintained incrementally.
    cur_col: u32,
    /// Byte offset in `buf` up to which (cur_line, cur_col) is accurate.
    scan_pos: usize,
    /// Last (gen_line, gen_col) we recorded — avoids duplicate mappings.
    last_marked: Option<(u32, u32)>,
    /// Collected source-map entries (populated via [`Self::mark_span`]).
    mappings: Vec<SourceMapping>,
    /// Loop-label stack — see [`crate::generator::loop_needs_break_label`].
    /// `break` inside a `switch` exits the switch, so a statement-arm `match`
    /// that wants to `break`/`continue` an enclosing loop uses a labelled jump.
    loop_labels: Vec<Option<String>>,
    /// Depth of statement-arm `switch` emission; > 0 routes `break`/`continue`
    /// to the innermost labelled loop.
    switch_label_depth: usize,
    /// Monotonic counter for unique loop-label names.
    loop_label_counter: usize,
    /// Monotonic counter for unique `match` scrutinee temporaries. A non-trivial
    /// scrutinee (a call, etc.) is hoisted into `const __matchN = <scrutinee>;`
    /// once, so it is evaluated a single time and — crucially for TypeScript —
    /// the discriminated-union narrowing on `__matchN._tag` flows to `__matchN._0`
    /// in the arm bodies. Re-emitting the scrutinee expression inline (the prior
    /// behavior) both double-evaluated it and defeated narrowing (TS2339 on `_0`).
    match_temp_counter: usize,
    /// Set once the Optional runtime prelude has been emitted, so a single-file
    /// **bundle** of several modules (cross-module `use`, DV13) emits it at most
    /// once (a duplicate `type Option<T>` is a TS redeclaration error).
    optional_runtime_emitted: bool,
    /// Set once the `Result` runtime prelude has been emitted; deduped across a
    /// bundle exactly as [`Self::optional_runtime_emitted`] (a duplicate
    /// `type BockResult<T, E>` is a TS redeclaration error).
    result_runtime_emitted: bool,
    /// Set once the concurrency runtime prelude has been emitted; deduped across
    /// a bundle exactly as [`Self::optional_runtime_emitted`].
    concurrency_runtime_emitted: bool,
    /// Set once the range runtime prelude ([`RANGE_RUNTIME_TS`]) has been
    /// emitted; deduped across a bundle exactly as
    /// [`Self::optional_runtime_emitted`] (a duplicate `const range` is a
    /// redeclaration error).
    range_runtime_emitted: bool,
    /// User-enum-variant registry (DV14). Same role as the JS backend's: route
    /// a unit-variant reference to the `{enum}_{variant}` const, a struct/tuple
    /// construction to the factory, and recognise `RecordPat` arms as ADT.
    /// Built-in Optional/Result pre-seeds are filtered out where bespoke
    /// lowering applies. Pre-scanned across the bundle.
    enum_variants: crate::generator::EnumVariantRegistry,
    /// Generic-type declaration registry: a record/enum/class name → its
    /// declared generic params. An `impl Box { ... }` block carries no generic
    /// params of its own (the `T` is declared on `record Box[T]`); this lets the
    /// declaration-merged `interface Box<T>` and the `self: Box<T>` param type
    /// recover them so the merge lands on the generic class. Pre-scanned across
    /// the bundle (mirrors [`Self::enum_variants`]).
    generic_decls: crate::generator::GenericDeclRegistry,
    /// Trait-declaration registry: a trait name → its declared generic params
    /// and methods. Used at each `impl Trait for Type` site to recover the
    /// trait's *default* methods (those carrying a body) so they can be
    /// synthesized onto the implementing type's prototype — the trait interface
    /// alone declares only signatures, so a type relying on an inherited default
    /// would otherwise have no such method. Pre-scanned across the bundle.
    trait_decls: crate::generator::TraitDeclRegistry,
    /// When `Some(target)`, a `Self` type (`TypeSelf`) renders as `target`
    /// rather than the default `this`. Set while emitting ANY `impl` method onto
    /// a concrete target — a synthesized trait default (`other: Self`) AND the
    /// impl's own inherent methods (`fn combine(self, ...) -> Self`) alike. Each
    /// emits as a free prototype function (`Target.prototype.m = function(...)`)
    /// where `this` is not a legal type annotation (`tsc` rejects it with
    /// TS2526), so the concrete target name must be substituted in both the
    /// prototype function and the matching merged-interface signature. Cleared
    /// (None) everywhere else, so trait-*interface* methods keep rendering `Self`
    /// as `this` (valid inside an interface member).
    trait_self_subst: Option<String>,
    /// Names of `public` (exported) top-level types. The declaration-merging
    /// `interface Target { ... }` an `impl` emits must be `export`ed exactly
    /// when the `Target` class is — TS requires all declarations in a merged
    /// declaration to agree on export-ness. Pre-scanned across the bundle.
    exported_types: std::collections::HashSet<String>,
    /// The TS type a value-position expression is being assigned *into* (the
    /// declared type of a `let x: T = <value>`), when known. Set around the
    /// `LetBinding` value emit. An expression-position `match` lowers to an IIFE
    /// (`(() => { switch (s) { … } })()`); when the value is consumed into a
    /// typed binding this annotates the IIFE arrow's return type (`(() : T =>
    /// {…})()`) and — crucially — signals that a value-`switch` over a bare
    /// identifier scrutinee must be hoisted into a temp (`const __matchN = s;
    /// switch (__matchN) …`). Without the hoist, `switch (s)` narrows `s` to the
    /// case's literal type inside each arm, so an arm body re-referencing `s`
    /// (`s === <other-literal>`) trips TS2367 ("no overlap"). Hoisting means the
    /// switch narrows the temp while arm bodies still see the original (un-
    /// narrowed) `s`. `None` outside a typed value-binding context; restored
    /// after the value so it never leaks to a sibling/outer expression.
    current_expected_type: Option<String>,
}

impl TsEmitCtx {
    fn new() -> Self {
        Self {
            buf: String::with_capacity(4096),
            indent: 0,
            effect_ops: HashMap::new(),
            current_handler_vars: HashMap::new(),
            fn_effects: HashMap::new(),
            composite_effects: HashMap::new(),
            record_names: HashSet::new(),
            effect_names: HashSet::new(),
            cur_line: 1,
            cur_col: 1,
            scan_pos: 0,
            last_marked: None,
            mappings: Vec::new(),
            loop_labels: Vec::new(),
            switch_label_depth: 0,
            loop_label_counter: 0,
            match_temp_counter: 0,
            optional_runtime_emitted: false,
            result_runtime_emitted: false,
            concurrency_runtime_emitted: false,
            range_runtime_emitted: false,
            enum_variants: crate::generator::EnumVariantRegistry::new(),
            generic_decls: crate::generator::GenericDeclRegistry::new(),
            trait_decls: crate::generator::TraitDeclRegistry::new(),
            trait_self_subst: None,
            exported_types: std::collections::HashSet::new(),
            current_expected_type: None,
        }
    }

    fn finish(self) -> (String, Vec<SourceMapping>) {
        (self.buf, self.mappings)
    }

    /// Variant info for `path` when its last segment is a registered *user*
    /// enum variant (built-in Optional/Result pre-seeds excluded — those go
    /// through the bespoke tagged-object lowering).
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

    /// Map Bock prelude functions to TypeScript equivalents.
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
                format!("new Promise<void>((__r) => setTimeout(__r, Math.floor(({a}) / 1e6)))")
            }
            _ => return Ok(None),
        };
        Ok(Some(code))
    }

    /// Recognise `Duration.xxx(...)` / `Instant.xxx(...)` associated-function
    /// calls and emit inline arithmetic. Durations are plain numbers
    /// (nanoseconds); Instants are numbers representing ns since
    /// `performance.timeOrigin`. Returns `Ok(true)` if the call was emitted.
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
            ("Instant", "now") => "(performance.now() * 1000000)".to_string(),
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Recognise `Channel.new()`, `spawn(...)`, and method calls on a
    /// channel value (`send`, `recv`, `close`) and emit the TS runtime
    /// helper equivalents.
    fn try_emit_concurrency_call(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
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
        if let NodeKind::Identifier { name: type_name } = &object.kind {
            if type_name.name == "Channel" && field.name == "new" {
                self.buf.push_str("__bockChannelNew()");
                return Ok(true);
            }
        }
        if matches!(field.name.as_str(), "send" | "recv" | "close") {
            // First arg is the receiver duplicate (from desugaring) — skip.
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

    /// Recognise desugared method calls `Call(FieldAccess(recv, m), [recv, ...args])`
    /// on Duration/Instant values and emit inline arithmetic.
    fn try_emit_time_desugared_method(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let NodeKind::FieldAccess { object, field } = &callee.kind else {
            return Ok(false);
        };
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
    /// arithmetic.
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
            "elapsed" => format!("((performance.now() * 1000000) - ({recv_str}))"),
            "duration_since" => {
                let other = arg_strs.first().cloned().unwrap_or_default();
                format!("(({recv_str}) - ({other}))")
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Emit Some/Ok/Err calls as tagged-object constructions, matching the
    /// representation used for user-defined enum variants. Returns true if
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
        let _ = write!(self.buf, "{{ _tag: \"{name}\" as const");
        if let Some(arg) = args.first() {
            self.buf.push_str(", _0: ");
            self.emit_expr(&arg.value)?;
        }
        self.buf.push_str(" }");
        Ok(true)
    }

    /// Emit a built-in `Optional`/`Result` method call to its TS form.
    ///
    /// Recognised via the checker's `recv_kind` annotation
    /// ([`crate::generator::desugared_optional_method`] /
    /// [`crate::generator::desugared_result_method`]). Both types use the tagged
    /// representation (`{ _tag, _0 }`), so the lowering is a ternary on `._tag`,
    /// wrapped in a *generic* arrow IIFE — `(<T,>(__c: BockOption<T>) => …)(recv)`
    /// / `(<T, E>(__c: BockResult<T, E>) => …)(recv)` — so the payload type is
    /// inferred from the receiver (strict-mode clean: no implicit `any`) and the
    /// receiver is evaluated exactly once. Returns `true` if handled.
    fn try_emit_container_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        if let Some((recv, method, rest)) =
            crate::generator::desugared_optional_method(node, callee, args)
        {
            self.emit_tagged_container_method(recv, method, rest, "Some", "<T,>", "BockOption<T>")?;
            return Ok(true);
        }
        if let Some((recv, method, rest)) =
            crate::generator::desugared_result_method(node, callee, args)
        {
            self.emit_tagged_container_method(
                recv,
                method,
                rest,
                "Ok",
                "<T, E>",
                "BockResult<T, E>",
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Lower a tagged-container method on `recv`. `present_tag` is the
    /// payload-carrying tag (`"Some"`/`"Ok"`); `type_params` / `param_ty` type
    /// the generic IIFE param (`<T,>` + `BockOption<T>`, or `<T, E>` +
    /// `BockResult<T, E>`).
    fn emit_tagged_container_method(
        &mut self,
        recv: &AIRNode,
        method: &str,
        rest: &[bock_air::AirArg],
        present_tag: &str,
        type_params: &str,
        param_ty: &str,
    ) -> Result<(), CodegenError> {
        // Pure tag tests read the receiver once → emit inline.
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
        let _ = write!(self.buf, "(({type_params}(__c: {param_ty}) => ");
        match method {
            "unwrap" => {
                let _ = write!(
                    self.buf,
                    "__c._tag === \"{present_tag}\" ? __c._0 : (undefined as never)"
                );
            }
            "unwrap_or" => {
                let _ = write!(self.buf, "__c._tag === \"{present_tag}\" ? __c._0 : (");
                if let Some(d) = rest.first() {
                    self.emit_expr(&d.value)?;
                } else {
                    self.buf.push_str("undefined");
                }
                self.buf.push(')');
            }
            "map" => {
                // The callback's parameter type is the concrete payload type the
                // checker already validated; the generic IIFE param `T` is wider
                // (unconstrained), so feed the payload through `as any` to satisfy
                // strict mode without recovering the concrete type here.
                let _ = write!(
                    self.buf,
                    "__c._tag === \"{present_tag}\" ? {{ _tag: \"{present_tag}\" as const, _0: ("
                );
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf.push_str("(x) => x");
                }
                self.buf.push_str(")(__c._0 as any) } : __c");
            }
            "flat_map" => {
                let _ = write!(self.buf, "__c._tag === \"{present_tag}\" ? (");
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf.push_str("(x) => x");
                }
                self.buf.push_str(")(__c._0 as any) : __c");
            }
            "map_err" => {
                self.buf
                    .push_str("__c._tag === \"Ok\" ? __c : { _tag: \"Err\" as const, _0: (");
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf.push_str("(x) => x");
                }
                self.buf.push_str(")(__c._0 as any) }");
            }
            _ => self.buf.push_str("(undefined as never)"),
        }
        self.buf.push_str(")(");
        self.emit_expr(recv)?;
        self.buf.push_str("))");
        Ok(())
    }

    /// Emit a read-only `List` built-in method call to its TS form.
    ///
    /// Mirrors the JS lowering but stays strict-mode clean: the
    /// `Optional`-returning methods (`get`/`first`/`last`/`index_of`) wrap the
    /// receiver in a *generic* arrow IIFE (`<T,>(__r: ReadonlyArray<T>, …):
    /// BockOption<T> => …`) so the element type `T` is inferred from the
    /// receiver and the result is the typed `BockOption<T>` union the `match`
    /// lowering narrows on `._tag`. The receiver is therefore evaluated exactly
    /// once and no parameter is implicitly `any`.
    fn try_emit_list_method(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) = crate::generator::desugared_list_method(callee, args)
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
                self.buf.push_str(
                    "(<T,>(__r: ReadonlyArray<T>, __i: number): BockOption<T> => \
                     (__i >= 0 && __i < __r.length) ? \
                     { _tag: \"Some\" as const, _0: __r[__i] } : { _tag: \"None\" as const })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&idx.value)?;
                self.buf.push(')');
            }
            "first" => {
                self.buf.push_str(
                    "(<T,>(__r: ReadonlyArray<T>): BockOption<T> => __r.length > 0 ? \
                     { _tag: \"Some\" as const, _0: __r[0] } : { _tag: \"None\" as const })(",
                );
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "last" => {
                self.buf.push_str(
                    "(<T,>(__r: ReadonlyArray<T>): BockOption<T> => __r.length > 0 ? \
                     { _tag: \"Some\" as const, _0: __r[__r.length - 1] } : \
                     { _tag: \"None\" as const })(",
                );
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
                self.buf.push_str(
                    "(<T,>(__r: ReadonlyArray<T>, __x: T): BockOption<number> => \
                     { const __i = __r.indexOf(__x); return __i >= 0 ? \
                     { _tag: \"Some\" as const, _0: __i } : { _tag: \"None\" as const }; })(",
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

    /// Emit a built-in `Map[K, V]` method call to its TS form (native `Map`).
    ///
    /// Recognised via [`crate::generator::desugared_map_method`] (gated on
    /// `recv_kind = "Map"`) and wired *before* [`Self::try_emit_list_method`].
    /// Mirrors the JS lowering but types the generic IIFEs (`<K, V>` params,
    /// `BockOption<V>` return for `get`) so `tsc --strict` narrows correctly.
    /// `get` returns the tagged `Optional` rep (`{ _tag: "Some" as const, _0: v
    /// }` / `{ _tag: "None" as const }`); mutating methods (`set`/`delete`/
    /// `merge`) mutate in place and return the receiver. Returns `true` if
    /// handled.
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
                    "(<K, V>(__m: Map<K, V>, __k: K): BockOption<V> => __m.has(__k) ? \
                     { _tag: \"Some\" as const, _0: __m.get(__k)! } : { _tag: \"None\" as const })(",
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
                self.buf.push_str(
                    "(<K, V>(__m: Map<K, V>, __k: K, __v: V): Map<K, V> => \
                     { __m.set(__k, __v); return __m; })(",
                );
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
                self.buf.push_str(
                    "(<K, V>(__m: Map<K, V>, __k: K): Map<K, V> => \
                     { __m.delete(__k); return __m; })(",
                );
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
                    "(<K, V>(__m: Map<K, V>, __o: Map<K, V>): Map<K, V> => \
                     { for (const [__k, __v] of __o) __m.set(__k, __v); return __m; })(",
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
                    "(<K, V>(__m: Map<K, V>, __f: (k: K, v: V) => boolean): Map<K, V> => \
                     { const __r = new Map<K, V>(); \
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
                self.buf.push_str(
                    "(<K, V>(__m: Map<K, V>, __f: (k: K, v: V) => void): void => \
                     { for (const [__k, __v] of __m) __f(__k, __v); })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&f.value)?;
                self.buf.push(')');
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Emit a built-in `Set[E]` method call to its TS form (native `Set`).
    ///
    /// Recognised via [`crate::generator::desugared_set_method`] (gated on
    /// `recv_kind = "Set"`) and wired *before* [`Self::try_emit_list_method`].
    /// Generic-typed IIFEs (`<E>` params) keep `tsc --strict` happy. Mutating
    /// methods (`add`/`remove`) mutate in place and return the receiver.
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
                self.buf.push_str(
                    "(<E>(__s: Set<E>, __x: E): Set<E> => { __s.add(__x); return __s; })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&x.value)?;
                self.buf.push(')');
            }
            "remove" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "(<E>(__s: Set<E>, __x: E): Set<E> => { __s.delete(__x); return __s; })(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&x.value)?;
                self.buf.push(')');
            }
            "union" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "(<E>(__a: Set<E>, __b: Set<E>): Set<E> => new Set<E>([...__a, ...__b]))(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "intersection" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "(<E>(__a: Set<E>, __b: Set<E>): Set<E> => \
                     new Set<E>([...__a].filter((__x) => __b.has(__x))))(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "difference" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "(<E>(__a: Set<E>, __b: Set<E>): Set<E> => \
                     new Set<E>([...__a].filter((__x) => !__b.has(__x))))(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "is_subset" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "(<E>(__a: Set<E>, __b: Set<E>): boolean => \
                     [...__a].every((__x) => __b.has(__x)))(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "is_superset" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "(<E>(__a: Set<E>, __b: Set<E>): boolean => \
                     [...__b].every((__x) => __a.has(__x)))(",
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
                    "(<E>(__s: Set<E>, __f: (x: E) => boolean): Set<E> => \
                     new Set<E>([...__s].filter(__f)))(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&f.value)?;
                self.buf.push(')');
            }
            "map" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "(<E>(__s: Set<E>, __f: (x: E) => E): Set<E> => \
                     new Set<E>([...__s].map(__f)))(",
                );
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
                self.buf.push_str(
                    "(<E>(__s: Set<E>, __f: (x: E) => void): void => \
                     { for (const __x of __s) __f(__x); })(",
                );
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
    /// `display` on a primitive receiver) to its TS form.
    ///
    /// Mirrors the JS lowering, but uses `as const` tags so the ternary's type
    /// is the discriminated `Ordering` union `tsc` can narrow on `._tag` in the
    /// match. `eq` → `===`; `to_string`/`display` → `String(x)`.
    /// Lower a desugared `String` built-in method call (`recv_kind =
    /// "Primitive:String"`) to its native TypeScript string op. Wired into the
    /// `Call` arm *before* `try_emit_list_method` so a String receiver's
    /// `len`/`contains`/`is_empty` dispatch here, not through the List path.
    ///
    /// `len` is the Unicode SCALAR count (`[...s].length`, iterating by code
    /// point) per spec §18.3 — not `s.length` (UTF-16 code units). `byte_len` is
    /// the UTF-8 byte count via `TextEncoder`. `replace` replaces ALL occurrences
    /// (`replaceAll`). `split` returns a TS array, the List runtime rep.
    fn try_emit_string_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) =
            crate::generator::desugared_string_method(node, callee, args)
        else {
            return Ok(false);
        };
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
        let recv_str = self.expr_to_string(recv)?;
        match method {
            "compare" => {
                let Some(other) = rest.first() else {
                    return Ok(false);
                };
                let other = self.expr_to_string(&other.value)?;
                let _ = write!(
                    self.buf,
                    "(({recv_str}) < ({other}) ? {{ _tag: \"Less\" as const }} : \
                     (({recv_str}) === ({other}) ? {{ _tag: \"Equal\" as const }} : \
                     {{ _tag: \"Greater\" as const }}))"
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

    // ── Type emission ────────────────────────────────────────────────────────

    /// Emit a type expression from an AIR type node to a TS type string.
    fn type_to_ts(&self, node: &AIRNode) -> String {
        match &node.kind {
            NodeKind::TypeNamed { path, args } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let ts_name = self.map_type_name(&name);
                if args.is_empty() {
                    ts_name
                } else {
                    let arg_strs: Vec<String> = args.iter().map(|a| self.type_to_ts(a)).collect();
                    format!("{ts_name}<{}>", arg_strs.join(", "))
                }
            }
            NodeKind::TypeTuple { elems } => {
                let elem_strs: Vec<String> = elems.iter().map(|e| self.type_to_ts(e)).collect();
                format!("[{}]", elem_strs.join(", "))
            }
            NodeKind::TypeFunction { params, ret, .. } => {
                let param_strs: Vec<String> = params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("arg{i}: {}", self.type_to_ts(p)))
                    .collect();
                format!("({}) => {}", param_strs.join(", "), self.type_to_ts(ret))
            }
            NodeKind::TypeOptional { inner } => {
                // `T?` lowers to the tagged Optional runtime union, not `T |
                // null`: the value is `{ _tag: "Some", _0: v }` / `{ _tag:
                // "None" }`, so the type must describe that. See
                // `OPTIONAL_RUNTIME_TS`.
                format!("BockOption<{}>", self.type_to_ts(inner))
            }
            NodeKind::TypeSelf => self
                .trait_self_subst
                .clone()
                .unwrap_or_else(|| "this".into()),
            _ => "unknown".into(),
        }
    }

    /// Map Bock type names to TS equivalents.
    fn map_type_name(&self, name: &str) -> String {
        match name {
            "Int" => "number".into(),
            "Float" => "number".into(),
            "Bool" => "boolean".into(),
            "String" => "string".into(),
            "Void" | "Unit" => "void".into(),
            "List" => "Array".into(),
            "Map" => "Map".into(),
            "Set" => "Set".into(),
            "Any" => "any".into(),
            "Never" => "never".into(),
            // `Result[T, E]` lowers to the tagged-union runtime type, mirroring
            // `Optional[T]` → `BockOption<T>` (see `RESULT_RUNTIME_TS`).
            "Result" => "BockResult".into(),
            // The spelled-out `Optional[T]` (a named type application, distinct
            // from the `T?` shorthand handled by the `TypeOptional` arm) must
            // also lower to the tagged runtime union `BockOption<T>`, matching
            // the emitted `{ _tag: "Some", _0: v }` / `{ _tag: "None" }` value
            // representation. Without this it emitted a bare `Optional<T>`
            // (TS2304, undefined name).
            "Optional" => "BockOption".into(),
            other => other.into(),
        }
    }

    /// Emit an AST TypeExpr to a TS type string (for record fields).
    fn ast_type_to_ts(&self, ty: &TypeExpr) -> String {
        match ty {
            TypeExpr::Named { path, args, .. } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let ts_name = self.map_type_name(&name);
                if args.is_empty() {
                    ts_name
                } else {
                    let arg_strs: Vec<String> =
                        args.iter().map(|a| self.ast_type_to_ts(a)).collect();
                    format!("{ts_name}<{}>", arg_strs.join(", "))
                }
            }
            TypeExpr::Tuple { elems, .. } => {
                let elem_strs: Vec<String> = elems.iter().map(|e| self.ast_type_to_ts(e)).collect();
                format!("[{}]", elem_strs.join(", "))
            }
            TypeExpr::Function { params, ret, .. } => {
                let param_strs: Vec<String> = params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("arg{i}: {}", self.ast_type_to_ts(p)))
                    .collect();
                format!(
                    "({}) => {}",
                    param_strs.join(", "),
                    self.ast_type_to_ts(ret)
                )
            }
            TypeExpr::Optional { inner, .. } => {
                // See the `TypeOptional` arm of `type_to_ts`: the tagged
                // Optional union must match the emitted tagged-object value.
                format!("BockOption<{}>", self.ast_type_to_ts(inner))
            }
            TypeExpr::SelfType { .. } => "this".into(),
        }
    }

    /// Emit generic parameter list: `<T, U extends Foo>`.
    /// Resolve the generic params that apply to an `impl` target: the impl's own
    /// params when present (`impl[T] Box[T] { ... }`), else the params declared
    /// on the target record/enum (`impl Box { ... }` where `T` lives on
    /// `record Box[T]`). Empty for a non-generic target.
    fn impl_target_generics(
        &self,
        impl_params: &[bock_ast::GenericParam],
        target_name: &str,
    ) -> Vec<bock_ast::GenericParam> {
        if !impl_params.is_empty() {
            return impl_params.to_vec();
        }
        self.generic_decls
            .get(target_name)
            .cloned()
            .unwrap_or_default()
    }

    /// Render a *use-site* generic argument list (`<T>`, `<T, U>`) — the bare
    /// param names, no `extends` bounds — for a type reference like `Box<T>`.
    /// Empty string for no params.
    fn generic_param_args(&self, params: &[bock_ast::GenericParam]) -> String {
        if params.is_empty() {
            return String::new();
        }
        let names: Vec<&str> = params.iter().map(|p| p.name.name.as_str()).collect();
        format!("<{}>", names.join(", "))
    }

    /// Render the combined generic-parameter declaration for an impl method's
    /// prototype function: the target type's params (with bounds) followed by
    /// the method's own params. Used because the prototype assignment lives
    /// outside the class, so its function must re-declare the class's `<T>`.
    fn merge_generic_params_to_ts(
        &self,
        target_params: &[bock_ast::GenericParam],
        method_params: &[bock_ast::GenericParam],
    ) -> String {
        let mut merged = target_params.to_vec();
        merged.extend(method_params.iter().cloned());
        self.generic_params_to_ts(&merged)
    }

    fn generic_params_to_ts(&self, params: &[bock_ast::GenericParam]) -> String {
        if params.is_empty() {
            return String::new();
        }
        let items: Vec<String> = params
            .iter()
            .map(|p| {
                if p.bounds.is_empty() {
                    p.name.name.clone()
                } else {
                    let bounds: Vec<String> = p
                        .bounds
                        .iter()
                        .map(|b| {
                            b.segments
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join(".")
                        })
                        .collect();
                    format!("{} extends {}", p.name.name, bounds.join(" & "))
                }
            })
            .collect();
        format!("<{}>", items.join(", "))
    }

    // ── Top-level dispatch ──────────────────────────────────────────────────

    fn emit_node(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.mark_span(node.span);
        match &node.kind {
            NodeKind::Module { items, .. } => {
                // Cross-module `use` (DV13) → single-file bundling: every
                // module's top-level declarations are concatenated into the one
                // entry file and `ImportDecl`s are dropped. Each runtime prelude
                // is emitted at most once across the bundle, gated on a ctx flag
                // (a duplicate `type Option<T>` would be a TS redeclaration).
                if !self.optional_runtime_emitted && module_uses_optional(items) {
                    self.buf.push_str(OPTIONAL_RUNTIME_TS);
                    self.buf.push('\n');
                    self.optional_runtime_emitted = true;
                }
                if !self.result_runtime_emitted && module_uses_result(items) {
                    self.buf.push_str(RESULT_RUNTIME_TS);
                    self.buf.push('\n');
                    self.result_runtime_emitted = true;
                }
                if !self.concurrency_runtime_emitted && module_uses_concurrency(items) {
                    self.buf.push_str(CONCURRENCY_RUNTIME_TS);
                    self.buf.push('\n');
                    self.concurrency_runtime_emitted = true;
                }
                if !self.range_runtime_emitted && module_uses_range(items) {
                    self.buf.push_str(RANGE_RUNTIME_TS);
                    self.buf.push('\n');
                    self.range_runtime_emitted = true;
                }
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    self.emit_node(item)?;
                }
                Ok(())
            }
            NodeKind::ImportDecl { .. } => {
                // Resolved by bundling — the imported module's declarations are
                // concatenated into this same file — so the import is a no-op.
                Ok(())
            }
            NodeKind::FnDecl {
                visibility,
                is_async,
                name,
                generic_params,
                params,
                return_type,
                effect_clause,
                body,
                ..
            } => self.emit_fn_decl(
                *visibility,
                *is_async,
                &name.name,
                generic_params,
                params,
                return_type.as_deref(),
                effect_clause,
                body,
                false,
            ),
            NodeKind::RecordDecl {
                visibility,
                name,
                generic_params,
                fields,
                ..
            } => {
                let export = if matches!(visibility, Visibility::Public) {
                    "export "
                } else {
                    ""
                };
                let generics = self.generic_params_to_ts(generic_params);
                self.record_names.insert(name.name.clone());
                if fields.is_empty() {
                    self.writeln(&format!("{export}class {}{generics} {{}}", name.name));
                } else {
                    self.writeln(&format!("{export}class {}{generics} {{", name.name));
                    self.indent += 1;
                    for f in fields {
                        let ty = self.ast_type_to_ts(&f.ty);
                        self.writeln(&format!("{}: {};", f.name.name, ty));
                    }
                    let init_fields: Vec<String> = fields
                        .iter()
                        .map(|f| format!("{}: {}", f.name.name, self.ast_type_to_ts(&f.ty)))
                        .collect();
                    let destructure: Vec<&str> =
                        fields.iter().map(|f| f.name.name.as_str()).collect();
                    self.writeln(&format!(
                        "constructor({{ {} }}: {{ {} }}) {{",
                        destructure.join(", "),
                        init_fields.join("; "),
                    ));
                    self.indent += 1;
                    for fname in &destructure {
                        self.writeln(&format!("this.{fname} = {fname};"));
                    }
                    self.indent -= 1;
                    self.writeln("}");
                    self.indent -= 1;
                    self.writeln("}");
                }
                Ok(())
            }
            NodeKind::EnumDecl {
                visibility,
                name,
                generic_params,
                variants,
                ..
            } => {
                let export = if matches!(visibility, Visibility::Public) {
                    "export "
                } else {
                    ""
                };
                let generics = self.generic_params_to_ts(generic_params);

                // Emit discriminated union type
                let variant_names: Vec<String> = variants
                    .iter()
                    .filter_map(|v| {
                        if let NodeKind::EnumVariant { name: vn, .. } = &v.kind {
                            Some(format!("{}_{}", name.name, vn.name))
                        } else {
                            None
                        }
                    })
                    .collect();
                if !variant_names.is_empty() {
                    self.writeln(&format!(
                        "{export}type {}{generics} = {};",
                        name.name,
                        variant_names.join(" | "),
                    ));
                    self.buf.push('\n');
                }

                // Emit interface + factory for each variant
                for variant in variants {
                    self.emit_enum_variant(&name.name, generic_params, variant)?;
                }
                Ok(())
            }
            NodeKind::ClassDecl {
                visibility,
                name,
                generic_params,
                fields,
                methods,
                ..
            } => {
                let export = if matches!(visibility, Visibility::Public) {
                    "export "
                } else {
                    ""
                };
                let generics = self.generic_params_to_ts(generic_params);
                self.writeln(&format!("{export}class {}{generics} {{", name.name));
                self.indent += 1;
                // Fields
                for f in fields {
                    let ty = self.ast_type_to_ts(&f.ty);
                    self.writeln(&format!("{}: {};", f.name.name, ty));
                }
                if !fields.is_empty() {
                    self.buf.push('\n');
                }
                // Constructor
                let ctor_params: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, self.ast_type_to_ts(&f.ty)))
                    .collect();
                self.writeln(&format!("constructor({}) {{", ctor_params.join(", ")));
                self.indent += 1;
                for f in fields {
                    self.writeln(&format!("this.{} = {};", f.name.name, f.name.name));
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
            NodeKind::TraitDecl {
                visibility,
                name,
                generic_params,
                methods,
                ..
            } => {
                let export = if matches!(visibility, Visibility::Public) {
                    "export "
                } else {
                    ""
                };
                let generics = self.generic_params_to_ts(generic_params);
                // The trait-self type: the interface name applied to its own
                // generic params, e.g. `Comparable<T>` for `trait Comparable[T]`.
                // The leading `self` param of every trait method is typed to
                // this (it is the receiver), and a bare `other: Self` resolves to
                // it too — without a type, `tsc --strict` flags `self` as an
                // implicit `any` (Q-ts-codegen). Mirrors how an `ImplBlock` types
                // `self` as the impl target.
                let trait_self_ty =
                    format!("{}{}", name.name, self.generic_param_args(generic_params));
                self.writeln(&format!("{export}interface {}{generics} {{", name.name));
                self.indent += 1;
                for (i, method) in methods.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    if let NodeKind::FnDecl {
                        name,
                        generic_params: method_generics,
                        params,
                        return_type,
                        ..
                    } = &method.kind
                    {
                        let m_generics = self.generic_params_to_ts(method_generics);
                        let param_list = self.collect_trait_typed_params(params, &trait_self_ty);
                        let ret = return_type
                            .as_ref()
                            .map(|r| self.type_to_ts(r))
                            .unwrap_or_else(|| "void".into());
                        self.writeln(&format!(
                            "{}{m_generics}({}): {};",
                            name.name,
                            param_list.join(", "),
                            ret,
                        ));
                    }
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::ImplBlock {
                generic_params,
                trait_path,
                trait_args,
                target,
                methods,
                ..
            } => {
                let target_base = self.type_expr_to_string(target);
                // The target's generic params — from the impl's own list when
                // present, else from the record/enum decl (`impl Box { ... }`
                // where `T` is declared on `record Box[T]`). `target_name` is
                // `Box<T>`, used for the merged interface head and the
                // `self: Box<T>` receiver param. Each prototype function
                // re-declares `<T>` (it is a free function outside the class
                // scope) via `merge_generic_params_to_ts`.
                let target_params = self.impl_target_generics(generic_params, &target_base);
                let target_name =
                    format!("{target_base}{}", self.generic_param_args(&target_params));
                // Trait default methods (codegen-completeness P2): for an
                // `impl Trait for Type` block, the trait's default methods that
                // this impl does not override must also be attached to the
                // target's prototype — the trait interface declares only their
                // signatures. We synthesize them exactly like the impl's own
                // methods (same interface sig + prototype function); a default
                // body that calls another trait method via `self.other()`
                // resolves through the same merged interface.
                let default_methods: Vec<AIRNode> = trait_path
                    .as_ref()
                    .map(|tp| {
                        crate::generator::inherited_default_methods(&self.trait_decls, tp, methods)
                    })
                    .unwrap_or_default();
                // Each entry carries whether it is a *synthesized default*
                // method (`true`). The `Self` type renders as the concrete
                // target (`trait_self_subst`) rather than `this` for ALL impl
                // methods — synthesized trait defaults AND the impl's own
                // inherent methods alike — since each emits as a free prototype
                // function (`Target.prototype.m = function(...): Self`) where
                // `this` is not a valid type annotation (TS2526). The boolean is
                // still threaded for clarity / future per-kind handling.
                let all_methods: Vec<(&AIRNode, bool)> = methods
                    .iter()
                    .map(|m| (m, false))
                    .chain(default_methods.iter().map(|m| (m, true)))
                    .collect();
                // Methods are attached via `Target.prototype.m = function(...)`.
                // For `tsc` to accept `p.m(...)` at call sites, the class type
                // must declare those members. We emit a declaration-merging
                // `interface Target { ... }` whose signatures mirror the
                // prototype functions exactly — crucially including the leading
                // `self` parameter (the AIR lowerer prepends the receiver as the
                // first argument and keeps `self` as a declared param, so the
                // call site is `p.m(p, ...)`). The untyped `self` param is typed
                // as the impl target, which also removes the implicit-`any`
                // error inside each method body.
                let mut iface_sigs: Vec<String> = Vec::new();
                for (method, _is_default) in &all_methods {
                    if let NodeKind::FnDecl {
                        is_async,
                        name,
                        generic_params,
                        params,
                        return_type,
                        effect_clause,
                        ..
                    } = &method.kind
                    {
                        // `Self` → the concrete target for every impl method (see
                        // `all_methods`). The merged-interface signature MUST
                        // match the prototype function's signature exactly, so the
                        // same substitution is applied in both loops; a mismatch
                        // (e.g. `this` here, `Target` there) is a declaration-merge
                        // error.
                        let prev_subst = self.trait_self_subst.take();
                        self.trait_self_subst = Some(target_name.clone());
                        let generics = self.generic_params_to_ts(generic_params);
                        let mut all_params = self.collect_impl_typed_params(params, &target_name);
                        if let Some(ep) = self.effects_param(effect_clause) {
                            all_params.push(ep);
                        }
                        let ret_str = build_ts_return_type(
                            *is_async,
                            return_type.as_deref().map(|r| self.type_to_ts(r)),
                        );
                        self.trait_self_subst = prev_subst;
                        iface_sigs.push(format!(
                            "{}{generics}({}){ret_str};",
                            name.name,
                            all_params.join(", "),
                        ));
                    }
                }
                // The declaration-merging `interface` must be `export`ed exactly
                // when the target `class` is — TS rejects a merged declaration
                // whose members disagree on export-ness (TS2395).
                let iface_export = if self.exported_types.contains(&target_base) {
                    "export "
                } else {
                    ""
                };
                if let Some(tp) = trait_path {
                    let trait_base = tp
                        .segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(".");
                    // The trait may itself be generic (`trait P[T]`), emitted as
                    // `interface P<T>`. The `extends` clause must carry the
                    // impl's trait type arguments (`impl P[T] for R[T]` →
                    // `extends P<T>`); without them `tsc` rejects with TS2314
                    // ("Generic type 'P<T>' requires 1 type argument(s)"). The
                    // args are type-expression AIR nodes; render each to its TS
                    // form. Empty `trait_args` ⇒ a non-generic trait, no `<...>`.
                    let trait_name = if trait_args.is_empty() {
                        trait_base
                    } else {
                        let arg_strs: Vec<String> =
                            trait_args.iter().map(|a| self.type_to_ts(a)).collect();
                        format!("{trait_base}<{}>", arg_strs.join(", "))
                    };
                    // Declaration merging: `extends Trait` keeps `new Target()`
                    // assignable to the trait's interface type, while the
                    // concrete signatures (with `self`) make `p.m(p)` resolve.
                    self.writeln(&format!(
                        "{iface_export}interface {target_name} extends {trait_name} {{"
                    ));
                    self.indent += 1;
                    for sig in &iface_sigs {
                        self.writeln(sig);
                    }
                    self.indent -= 1;
                    self.writeln("}");
                    self.writeln(&format!("// impl {trait_name} for {target_name}"));
                } else {
                    self.writeln(&format!("{iface_export}interface {target_name} {{"));
                    self.indent += 1;
                    for sig in &iface_sigs {
                        self.writeln(sig);
                    }
                    self.indent -= 1;
                    self.writeln("}");
                    self.writeln(&format!("// impl {target_name}"));
                }
                for (method, _is_default) in &all_methods {
                    if let NodeKind::FnDecl {
                        is_async,
                        name,
                        generic_params,
                        params,
                        return_type,
                        effect_clause,
                        body,
                        ..
                    } = &method.kind
                    {
                        // Every impl method emits as a free prototype function
                        // (`Target.prototype.m = function(...)`), where `this` is
                        // not a valid type. So a `Self` type — whether in a
                        // synthesized trait default (`other: Self`) or the impl's
                        // own inherent method (`fn combine(self, ...) -> Self`) —
                        // must render as the concrete target. This matches the
                        // merged-interface signature emitted above.
                        let prev_subst = self.trait_self_subst.take();
                        self.trait_self_subst = Some(target_name.clone());
                        let async_kw = if *is_async { "async " } else { "" };
                        // The prototype assignment lives outside the class scope,
                        // so the function itself must re-declare the target's
                        // generic params (`function<T>(self: Box<T>): T`) — they
                        // are NOT in scope from the class. Merge them with the
                        // method's own generics. The `.prototype` reference uses
                        // the *bare* type name (`Box.prototype`, never
                        // `Box<T>.prototype`, which is not valid TS).
                        let generics =
                            self.merge_generic_params_to_ts(&target_params, generic_params);
                        let param_list = self.collect_impl_typed_params(params, &target_name);
                        let effects_param = self.effects_param(effect_clause);
                        let mut all_params = param_list;
                        if let Some(ep) = effects_param {
                            all_params.push(ep);
                        }
                        let ret_str = build_ts_return_type(
                            *is_async,
                            return_type.as_deref().map(|r| self.type_to_ts(r)),
                        );
                        self.writeln(&format!(
                            "{target_base}.prototype.{} = {async_kw}function{generics}({}){ret_str} {{",
                            name.name,
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
                        // Restore after the whole method (signature + body) is
                        // emitted, so any `Self` annotation in the body also
                        // resolves to the concrete target.
                        self.trait_self_subst = prev_subst;
                    }
                }
                Ok(())
            }
            NodeKind::EffectDecl {
                visibility,
                name,
                generic_params,
                components,
                operations,
                ..
            } => {
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
                self.effect_names.insert(name.name.clone());
                // Effects → TS interface
                let export = if matches!(visibility, Visibility::Public) {
                    "export "
                } else {
                    ""
                };
                let generics = self.generic_params_to_ts(generic_params);
                self.writeln(&format!("{export}interface {}{generics} {{", name.name));
                self.indent += 1;
                for op in operations {
                    if let NodeKind::FnDecl {
                        name,
                        params,
                        return_type,
                        ..
                    } = &op.kind
                    {
                        let param_list = self.collect_typed_params(params);
                        let ret = return_type
                            .as_ref()
                            .map(|r| self.type_to_ts(r))
                            .unwrap_or_else(|| "void".into());
                        self.writeln(&format!(
                            "{}({}): {};",
                            name.name,
                            param_list.join(", "),
                            ret,
                        ));
                    }
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::TypeAlias {
                visibility,
                name,
                generic_params,
                ty,
                ..
            } => {
                let export = if matches!(visibility, Visibility::Public) {
                    "export "
                } else {
                    ""
                };
                let generics = self.generic_params_to_ts(generic_params);
                let ty_str = self.type_to_ts(ty);
                self.writeln(&format!("{export}type {}{generics} = {ty_str};", name.name));
                Ok(())
            }
            NodeKind::ConstDecl {
                visibility,
                name,
                ty,
                value,
                ..
            } => {
                let export = if matches!(visibility, Visibility::Public) {
                    "export "
                } else {
                    ""
                };
                let ty_str = self.type_to_ts(ty);
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}{export}const {}: {ty_str} = ", name.name);
                self.emit_expr(value)?;
                self.buf.push_str(";\n");
                Ok(())
            }
            NodeKind::ModuleHandle { effect, handler } => {
                let effect_name = effect.segments.last().map_or("effect", |s| s.name.as_str());
                let var_name = format!("__{}", to_camel_case(effect_name));
                let type_name = effect_name;
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}const {var_name}: {type_name} = ");
                self.emit_expr(handler)?;
                self.buf.push_str(";\n");
                // Register as ambient handler so same-module calls pick it up.
                self.current_handler_vars
                    .insert(effect_name.to_string(), var_name);
                Ok(())
            }
            NodeKind::PropertyTest { name, body, .. } => {
                self.writeln(&format!("// property test: {name}"));
                self.writeln("// (property tests are not emitted in TS output)");
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
        generic_params: &[bock_ast::GenericParam],
        params: &[AIRNode],
        return_type: Option<&AIRNode>,
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
        let generics = self.generic_params_to_ts(generic_params);
        let param_list = self.collect_typed_params(params);
        let effects_param = self.effects_param(effect_clause);
        let mut all_params = param_list;
        if let Some(ep) = effects_param {
            all_params.push(ep);
        }
        let ret_str = build_ts_return_type(is_async, return_type.map(|r| self.type_to_ts(r)));
        if !effect_clause.is_empty() {
            let effect_names = self.expand_effect_names(effect_clause);
            self.fn_effects.insert(name.to_string(), effect_names);
        }
        let ts_name = ts_value_ident(name);
        self.writeln(&format!(
            "{export}{async_kw}function {ts_name}{generics}({}){ret_str} {{",
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
        self.writeln("}");
        Ok(())
    }

    fn emit_class_method(&mut self, method: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::FnDecl {
            is_async,
            name,
            generic_params,
            params,
            return_type,
            effect_clause,
            body,
            ..
        } = &method.kind
        {
            let async_kw = if *is_async { "async " } else { "" };
            let generics = self.generic_params_to_ts(generic_params);
            let param_list = self.collect_typed_params(params);
            let effects_param = self.effects_param(effect_clause);
            let mut all_params = param_list;
            if let Some(ep) = effects_param {
                all_params.push(ep);
            }
            let ret_str = build_ts_return_type(
                *is_async,
                return_type.as_deref().map(|r| self.type_to_ts(r)),
            );
            let method_name = to_camel_case(&name.name);
            self.writeln(&format!(
                "{async_kw}{method_name}{generics}({}){ret_str} {{",
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
            self.writeln("}");
        }
        Ok(())
    }

    /// Collect typed parameter names: `name: Type`.
    fn collect_typed_params(&self, params: &[AIRNode]) -> Vec<String> {
        params
            .iter()
            .filter_map(|p| {
                if let NodeKind::Param {
                    pattern,
                    ty,
                    default,
                } = &p.kind
                {
                    let name = self.pattern_to_binding_name(pattern);
                    let ty_str = ty
                        .as_ref()
                        .map(|t| format!(": {}", self.type_to_ts(t)))
                        .unwrap_or_default();
                    if let Some(def) = default {
                        let mut ctx = TsEmitCtx::new();
                        ctx.indent = self.indent;
                        ctx.enum_variants = self.enum_variants.clone();
                        if ctx.emit_expr_to_string(def).is_ok() {
                            let (def_str, _) = ctx.finish();
                            return Some(format!("{name}{ty_str} = {def_str}"));
                        }
                    }
                    Some(format!("{name}{ty_str}"))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Collect typed parameters for an `impl` method, typing an untyped
    /// receiver (`self`) parameter as the impl target.
    ///
    /// Bock impl methods declare `self` with no type annotation
    /// (`fn sum(self)`), and the AIR lowerer keeps it as a real parameter while
    /// prepending the receiver at call sites (`p.sum(p)`). Without a type, `tsc`
    /// flags `self` as implicit `any`; we substitute the target type so the
    /// method body (`self.x`) and the declaration-merged interface both
    /// type-check. Non-`self` params, and a `self` that already carries an
    /// explicit type, are handled exactly as [`Self::collect_typed_params`].
    fn collect_impl_typed_params(&self, params: &[AIRNode], target_name: &str) -> Vec<String> {
        params
            .iter()
            .filter_map(|p| {
                let NodeKind::Param {
                    pattern,
                    ty,
                    default,
                } = &p.kind
                else {
                    return None;
                };
                let name = self.pattern_to_binding_name(pattern);
                let ty_str = match ty {
                    Some(t) => format!(": {}", self.type_to_ts(t)),
                    None if name == "self" => format!(": {target_name}"),
                    None => String::new(),
                };
                if let Some(def) = default {
                    let mut ctx = TsEmitCtx::new();
                    ctx.indent = self.indent;
                    ctx.enum_variants = self.enum_variants.clone();
                    if ctx.emit_expr_to_string(def).is_ok() {
                        let (def_str, _) = ctx.finish();
                        return Some(format!("{name}{ty_str} = {def_str}"));
                    }
                }
                Some(format!("{name}{ty_str}"))
            })
            .collect()
    }

    /// Collect typed parameters for a **trait declaration** method, typing an
    /// untyped receiver (`self`) as the trait's own interface type
    /// (`trait_self_ty`, e.g. `Comparable<T>`).
    ///
    /// A trait method declares `self` with no annotation (`fn compare(self,
    /// other: Self)`); the AIR lowerer keeps it as a real leading parameter. In
    /// the emitted interface the untyped `self` would otherwise be `tsc
    /// --strict`'s implicit `any`. Typing it to the trait-self type makes the
    /// interface method signature well-typed and keeps it compatible (via
    /// declaration-merging method bivariance) with the concrete `self: Target`
    /// the `ImplBlock` arm emits. A `self` that already carries an explicit type,
    /// and all non-`self` params, are handled exactly as
    /// [`Self::collect_typed_params`] (where a `Self` annotation maps to `this`,
    /// the implementing type — correct for `other: Self`).
    fn collect_trait_typed_params(&self, params: &[AIRNode], trait_self_ty: &str) -> Vec<String> {
        params
            .iter()
            .filter_map(|p| {
                let NodeKind::Param {
                    pattern,
                    ty,
                    default,
                } = &p.kind
                else {
                    return None;
                };
                let name = self.pattern_to_binding_name(pattern);
                let ty_str = match ty {
                    Some(t) => format!(": {}", self.type_to_ts(t)),
                    None if name == "self" => format!(": {trait_self_ty}"),
                    None => String::new(),
                };
                if let Some(def) = default {
                    let mut ctx = TsEmitCtx::new();
                    ctx.indent = self.indent;
                    ctx.enum_variants = self.enum_variants.clone();
                    if ctx.emit_expr_to_string(def).is_ok() {
                        let (def_str, _) = ctx.finish();
                        return Some(format!("{name}{ty_str} = {def_str}"));
                    }
                }
                Some(format!("{name}{ty_str}"))
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

    /// Effects → typed destructured parameter object: `{ log, clock }: { log: Log, clock: Clock }`.
    fn effects_param(&self, effects: &[bock_ast::TypePath]) -> Option<String> {
        if effects.is_empty() {
            return None;
        }
        let expanded = self.expand_effect_names(effects);
        if expanded.is_empty() {
            return None;
        }
        let names: Vec<String> = expanded.iter().map(|n| to_camel_case(n)).collect();
        let type_entries: Vec<String> = expanded
            .iter()
            .zip(names.iter())
            .map(|(orig, camel)| format!("{camel}: {orig}"))
            .collect();
        Some(format!(
            "{{ {} }}: {{ {} }}",
            names.join(", "),
            type_entries.join(", ")
        ))
    }

    /// Build a `{ effect: handler_var, ... }` argument for calling an effectful function.
    fn build_effects_call_arg_ts(&self, fn_name: &str) -> Option<String> {
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

    // ── Enum variant interfaces + factories ──────────────────────────────────

    fn emit_enum_variant(
        &mut self,
        enum_name: &str,
        generic_params: &[bock_ast::GenericParam],
        variant: &AIRNode,
    ) -> Result<(), CodegenError> {
        if let NodeKind::EnumVariant { name, payload } = &variant.kind {
            let vname = &name.name;
            let generics = self.generic_params_to_ts(generic_params);
            let qualified = format!("{enum_name}_{vname}");

            match payload {
                EnumVariantPayload::Unit => {
                    // Interface for unit variant
                    self.writeln(&format!(
                        "interface {qualified}{generics} {{ readonly _tag: \"{vname}\"; }}"
                    ));
                    self.writeln(&format!(
                        "const {qualified}: {qualified} = Object.freeze({{ _tag: \"{vname}\" as const }});"
                    ));
                }
                EnumVariantPayload::Struct(fields) => {
                    // Interface for struct variant
                    self.writeln(&format!("interface {qualified}{generics} {{"));
                    self.indent += 1;
                    self.writeln(&format!("readonly _tag: \"{vname}\";"));
                    for f in fields {
                        let ty = self.ast_type_to_ts(&f.ty);
                        self.writeln(&format!("readonly {}: {};", f.name.name, ty));
                    }
                    self.indent -= 1;
                    self.writeln("}");
                    let field_params: Vec<String> = fields
                        .iter()
                        .map(|f| format!("{}: {}", f.name.name, self.ast_type_to_ts(&f.ty)))
                        .collect();
                    let field_names: Vec<&str> =
                        fields.iter().map(|f| f.name.name.as_str()).collect();
                    self.writeln(&format!(
                        "function {qualified}{generics}({}): {qualified} {{",
                        field_params.join(", "),
                    ));
                    self.indent += 1;
                    self.writeln(&format!(
                        "return {{ _tag: \"{vname}\" as const, {} }};",
                        field_names.join(", "),
                    ));
                    self.indent -= 1;
                    self.writeln("}");
                }
                EnumVariantPayload::Tuple(elems) => {
                    // Interface for tuple variant
                    self.writeln(&format!("interface {qualified}{generics} {{"));
                    self.indent += 1;
                    self.writeln(&format!("readonly _tag: \"{vname}\";"));
                    for (i, elem) in elems.iter().enumerate() {
                        let ty = self.type_to_ts(elem);
                        self.writeln(&format!("readonly _{i}: {ty};"));
                    }
                    self.indent -= 1;
                    self.writeln("}");
                    let param_decls: Vec<String> = elems
                        .iter()
                        .enumerate()
                        .map(|(i, e)| format!("_{i}: {}", self.type_to_ts(e)))
                        .collect();
                    let param_names: Vec<String> =
                        (0..elems.len()).map(|i| format!("_{i}")).collect();
                    self.writeln(&format!(
                        "function {qualified}{generics}({}): {qualified} {{",
                        param_decls.join(", "),
                    ));
                    self.indent += 1;
                    self.writeln(&format!(
                        "return {{ _tag: \"{vname}\" as const, {} }};",
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
        match &node.kind {
            NodeKind::LetBinding {
                is_mut,
                pattern,
                ty,
                value,
                ..
            } => {
                let kw = if *is_mut { "let" } else { "const" };
                let binding = self.pattern_to_ts_destructure(pattern);
                let ty_ts = ty.as_ref().map(|t| self.type_to_ts(t));
                let ty_str = ty_ts.as_ref().map(|t| format!(": {t}")).unwrap_or_default();
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}{kw} {binding}{ty_str} = ");
                // Record the binding's declared type as the expected type for the
                // value, so a value-position `match`/`if` IIFE annotates its arrow
                // return and hoists a bare-identifier scrutinee (avoids the TS2367
                // narrowing — see `current_expected_type`). Restored after so it
                // never leaks to a nested/sibling expression.
                let prev_expected = self.current_expected_type.take();
                self.current_expected_type = ty_ts;
                self.emit_expr(value)?;
                self.current_expected_type = prev_expected;
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
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}if (");
                    self.emit_expr(condition)?;
                    self.buf.push_str(" != null) {\n");
                    self.indent += 1;
                    let binding = self.pattern_to_ts_destructure(pat);
                    self.writeln(&format!("const {binding} = "));
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
                let binding = self.pattern_to_ts_destructure(pattern);
                self.emit_loop_label_prefix(body);
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}for (const {binding} of ");
                self.emit_expr(iterable)?;
                self.buf.push_str(") {\n");
                self.indent += 1;
                self.emit_block_body(body)?;
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
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                self.loop_labels.pop();
                Ok(())
            }
            NodeKind::Loop { body } => {
                self.emit_loop_label_prefix(body);
                self.writeln("while (true) {");
                self.indent += 1;
                self.emit_block_body(body)?;
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
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}/* break value: ");
                    self.emit_expr(val)?;
                    self.buf.push_str(" */\n");
                }
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
                condition,
                else_block,
                ..
            } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}if (!(");
                self.emit_expr(condition)?;
                self.buf.push_str(")) {\n");
                self.indent += 1;
                self.emit_block_body(else_block)?;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => self.emit_match(scrutinee, arms, false),
            NodeKind::Block { stmts, tail } => {
                self.writeln("{");
                self.indent += 1;
                for s in stmts {
                    self.emit_node(s)?;
                }
                if let Some(t) = tail {
                    self.write_indent();
                    self.emit_expr(t)?;
                    self.buf.push_str(";\n");
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::HandlingBlock { handlers, body } => {
                // handling block → scoped handler instantiation
                self.writeln("{");
                self.indent += 1;
                let old_handler_vars = self.current_handler_vars.clone();
                for h in handlers {
                    let effect_name = h
                        .effect
                        .segments
                        .last()
                        .map_or("effect", |s| s.name.as_str());
                    let var_name = format!("__{}", to_camel_case(effect_name));
                    let type_name = effect_name;
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}const {var_name}: {type_name} = ");
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
                    self.buf.push_str("{ _tag: \"None\" as const }");
                } else if let Some(variant) = crate::generator::ordering_variant(&name.name) {
                    // Prelude `Ordering` variant → an inline tagged object (the
                    // self-contained representation the primitive-bridge
                    // `compare` and the `_tag`-switch match also use).
                    let _ = write!(self.buf, "{{ _tag: \"{variant}\" as const }}");
                } else if let Some(enum_name) = self
                    .user_variant_for_name(&name.name)
                    .map(|i| i.enum_name.clone())
                {
                    // A bare unit-variant reference (`Red`) → the frozen
                    // `{enum}_{variant}` const.
                    let _ = write!(self.buf, "{enum_name}_{}", name.name);
                } else {
                    self.buf.push_str(&ts_value_ident(&name.name));
                }
                Ok(())
            }
            NodeKind::BinaryOp { op, left, right } => {
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
                if self.try_emit_time_desugared_method(callee, args)? {
                    return Ok(());
                }
                if self.try_emit_concurrency_call(callee, args)? {
                    return Ok(());
                }
                // Map/Set dispatch precedes the List recogniser so the
                // overlapping method names route by `recv_kind`, not by name.
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
                if self.try_emit_list_method(callee, args)? {
                    return Ok(());
                }
                if self.try_emit_primitive_bridge(node, callee, args)? {
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
                // Pass handler args to effectful function calls.
                let effects_arg = if let NodeKind::Identifier { name } = &callee.kind {
                    self.build_effects_call_arg_ts(&name.name)
                } else {
                    None
                };
                self.emit_expr(callee)?;
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
                let _ = write!(self.buf, ".{}", to_camel_case(&method.name));
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
                let param_list = self.collect_typed_params(params);
                let _ = write!(self.buf, "({}) => ", param_list.join(", "));
                if matches!(body.kind, NodeKind::Block { .. }) {
                    self.buf.push_str("{\n");
                    self.indent += 1;
                    self.emit_block_body(body)?;
                    self.indent -= 1;
                    self.write_indent();
                    self.buf.push('}');
                } else {
                    self.emit_expr(body)?;
                }
                Ok(())
            }
            NodeKind::Pipe { left, right } => self.emit_pipe(left, right),
            NodeKind::Compose { left, right } => {
                let _ = write!(self.buf, "((x: any) => ");
                self.emit_expr(right)?;
                self.buf.push('(');
                self.emit_expr(left)?;
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
                self.emit_expr(expr)?;
                Ok(())
            }
            NodeKind::Range { lo, hi, inclusive } => {
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
                // the `{enum}_{variant}(field, ..)` factory, in field decl
                // order. Plain records keep their object/class form.
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
                        let supplied = fields.iter().find(|f| &f.name.name == fname);
                        match supplied.and_then(|f| f.value.as_ref()) {
                            Some(val) => self.emit_expr(val)?,
                            None => self.buf.push_str(&ts_value_ident(fname)),
                        }
                    }
                    self.buf.push(')');
                    return Ok(());
                }
                let type_name = path.segments.last().map(|s| s.name.as_str()).unwrap_or("");
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
                // TS tuples are arrays with typed positions.
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
                let _ = write!(self.buf, "{{ _tag: \"{tag}\" as const, _0: ");
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
                // IIFE
                self.buf.push_str("(() => {\n");
                self.indent += 1;
                for s in stmts {
                    self.emit_node(s)?;
                }
                if let Some(t) = tail {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}return ");
                    self.emit_expr(t)?;
                    self.buf.push_str(";\n");
                }
                self.indent -= 1;
                self.write_indent();
                self.buf.push_str("})()");
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => {
                // Expression-position match → IIFE. When the value is consumed
                // into a typed binding (`let x: T = match …`), annotate the
                // arrow's return type (`(() : T => {…})()`) so a `T` distinct
                // from the enclosing function's inferred return is respected, and
                // force-hoist a bare-identifier scrutinee so `switch (s)` does not
                // narrow `s` to the case literal inside arm bodies (TS2367). The
                // expected type is taken here so it scopes to this IIFE only and
                // does not leak into the (separately typed) arm-body expressions.
                let expected = self.current_expected_type.take();
                let arrow_ret = expected
                    .as_deref()
                    .map(|t| format!(" : {t}"))
                    .unwrap_or_default();
                let force_hoist = expected.is_some();
                let _ = write!(self.buf, "((){arrow_ret} => {{");
                self.buf.push('\n');
                self.indent += 1;
                self.emit_match(scrutinee, arms, force_hoist)?;
                self.indent -= 1;
                self.write_indent();
                self.buf.push_str("})()");
                self.current_expected_type = expected;
                Ok(())
            }
            // Ownership: erase
            NodeKind::Move { expr }
            | NodeKind::Borrow { expr }
            | NodeKind::MutableBorrow { expr } => self.emit_expr(expr),
            // Effect operation invocation
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
            // Type expressions in expression position: emit the TS type
            NodeKind::TypeNamed { .. }
            | NodeKind::TypeTuple { .. }
            | NodeKind::TypeFunction { .. }
            | NodeKind::TypeOptional { .. }
            | NodeKind::TypeSelf => {
                let ty_str = self.type_to_ts(node);
                let _ = write!(self.buf, "/* {ty_str} */");
                Ok(())
            }
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
            NodeKind::Error => {
                self.buf.push_str("/* error */");
                Ok(())
            }
            _ => {
                self.buf.push_str("/* unsupported */");
                Ok(())
            }
        }
    }

    // ── Match → switch ──────────────────────────────────────────────────────

    /// Lower a `match`. `force_hoist` requests that even a bare-identifier
    /// scrutinee be hoisted into a single `const __matchN = …;` temp before the
    /// `switch` — set for an expression-position match consumed into a typed
    /// binding, so the `switch` narrows the temp (not the original binding) and
    /// arm bodies re-referencing the scrutinee do not trip TS2367. Statement-
    /// position calls pass `false`, preserving the inline `switch (s)` fast-path.
    fn emit_match(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
        force_hoist: bool,
    ) -> Result<(), CodegenError> {
        // Guards, or-patterns, tuple patterns, and nested constructor/record
        // patterns cannot be expressed by the flat `switch` below. Lower those
        // to an if/else-if chain. Additive: the proven Optional / Result /
        // user-enum / value `switch` fast-path is kept for everything else (see
        // `match_needs_ifchain`). The if-chain already casts the scrutinee root
        // to `as any`, which itself defeats the TS2367 narrowing, so `force_hoist`
        // is irrelevant there.
        if crate::generator::match_needs_ifchain(arms) {
            return self.emit_match_ifchain(scrutinee, arms);
        }

        // ADT (dispatch on `._tag`) when any arm is a constructor pattern or a
        // record pattern naming a registered enum variant. The record-pattern
        // case is the struct-payload variant the prior `ConstructorPat`-only
        // check missed (DV14).
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
        // is evaluated once and TS narrowing on `__matchN._tag` reaches the
        // payload access `__matchN._0` in the arm bodies. A bare identifier is
        // already a stable reference — normally left inline. But `force_hoist`
        // (an expression-position value match) hoists it too, so `switch
        // (__matchN)` narrows the temp rather than the original binding, keeping
        // arm bodies that re-reference the scrutinee free of TS2367.
        let temp = if matches!(scrutinee.kind, NodeKind::Identifier { .. }) && !force_hoist {
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

    /// Emit a TS label before a loop iff a contained statement-arm `match`
    /// needs to `break`/`continue` the loop. Pair with `loop_labels.pop()`.
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
                NodeKind::BindPat { name, .. } if !is_adt => {
                    self.writeln("default: {");
                    self.indent += 1;
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}const {} = ", name.name);
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
    /// <body> } else if …` chain. Mirrors the JS lowering; the only difference
    /// is that TS access paths into the scrutinee are cast through `as any` so a
    /// nested access (`__matchN._0._tag`) typechecks without relying on
    /// discriminated-union narrowing flowing through `&&`.
    fn emit_match_ifchain(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        // Single-evaluation root. A bare identifier is stable; anything else is
        // hoisted into `__matchN`. The access root the tests/binds descend from
        // is cast to `any` so nested field access typechecks under `tsc`.
        let root: String = if let NodeKind::Identifier { name } = &scrutinee.kind {
            format!("({} as any)", name.name)
        } else {
            self.match_temp_counter += 1;
            let name = format!("__match{}", self.match_temp_counter);
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}const {name} = ");
            self.emit_expr(scrutinee)?;
            self.buf.push_str(";\n");
            format!("({name} as any)")
        };

        let mut first = true;
        let mut closed = false;
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
            let test = self.pattern_test_ts(pattern, &root);
            let is_catch_all = matches!(
                pattern.kind,
                NodeKind::WildcardPat | NodeKind::BindPat { .. }
            );
            let is_last = idx + 1 == arm_count;
            // See the JS lowering: an unguarded catch-all *or* the final
            // unguarded arm becomes the unconditional `else`, closing the chain
            // so a value-returning function typechecks under `tsc`.
            let unconditional = guard.is_none() && (is_catch_all || is_last);
            let ind = self.indent_str();
            if unconditional {
                if first {
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
                    let g_str = self.expr_to_string(g)?;
                    let binds = self.pattern_binds_to_string_ts(pattern, &root);
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
            self.pattern_binds_ts(pattern, &root)?;
            self.emit_block_body(body)?;
            self.indent -= 1;
            self.writeln("}");
        }
        if !closed && !first {
            self.writeln("else { throw new Error(\"non-exhaustive match\"); }");
        }
        Ok(())
    }

    /// Build the boolean test that selects `pat` against the TS expression
    /// `access` (already `any`-typed by the caller). Mirrors `pattern_test_js`.
    fn pattern_test_ts(&self, pat: &AIRNode, access: &str) -> String {
        match &pat.kind {
            NodeKind::WildcardPat | NodeKind::BindPat { .. } => String::new(),
            NodeKind::LiteralPat { lit } => {
                format!("{access} === {}", ts_literal(lit))
            }
            NodeKind::ConstructorPat { path, fields } => {
                let variant = path.segments.last().map_or("_", |s| s.name.as_str());
                let mut tests = vec![format!("{access}._tag === \"{variant}\"")];
                for (i, field) in fields.iter().enumerate() {
                    let sub = self.pattern_test_ts(field, &format!("{access}._{i}"));
                    if !sub.is_empty() {
                        tests.push(sub);
                    }
                }
                tests.join(" && ")
            }
            NodeKind::RecordPat { path, fields, .. } => {
                let variant = path.segments.last().map_or("_", |s| s.name.as_str());
                let mut tests = Vec::new();
                if self.user_variant_for_path(path).is_some() {
                    tests.push(format!("{access}._tag === \"{variant}\""));
                }
                for f in fields {
                    if let Some(p) = &f.pattern {
                        let sub = self.pattern_test_ts(p, &format!("{access}.{}", f.name.name));
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
                    let sub = self.pattern_test_ts(e, &format!("{access}[{i}]"));
                    if !sub.is_empty() {
                        tests.push(sub);
                    }
                }
                tests.join(" && ")
            }
            NodeKind::OrPat { alternatives } => {
                let alts: Vec<String> = alternatives
                    .iter()
                    .map(|a| {
                        let t = self.pattern_test_ts(a, access);
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

    /// Emit the `const <name> = <access…>;` bindings introduced by `pat`.
    /// Mirrors `pattern_binds_js`.
    fn pattern_binds_ts(&mut self, pat: &AIRNode, access: &str) -> Result<(), CodegenError> {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => {
                let ind = self.indent_str();
                let _ = writeln!(
                    self.buf,
                    "{ind}const {} = {access};",
                    ts_value_ident(&name.name)
                );
            }
            NodeKind::ConstructorPat { fields, .. } => {
                for (i, field) in fields.iter().enumerate() {
                    self.pattern_binds_ts(field, &format!("{access}._{i}"))?;
                }
            }
            NodeKind::RecordPat { fields, .. } => {
                for f in fields {
                    let field_access = format!("{access}.{}", f.name.name);
                    match &f.pattern {
                        Some(p) => self.pattern_binds_ts(p, &field_access)?,
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
                    self.pattern_binds_ts(e, &format!("{access}[{i}]"))?;
                }
            }
            NodeKind::OrPat { alternatives } => {
                if let Some(first) = alternatives.first() {
                    self.pattern_binds_ts(first, access)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Collect `pat`'s bindings as a single-line `const … = …; ` string for the
    /// guard-evaluating IIFE. Mirrors `pattern_binds_to_string_js`.
    fn pattern_binds_to_string_ts(&self, pat: &AIRNode, access: &str) -> String {
        let mut out = String::new();
        self.collect_binds_ts(pat, access, &mut out);
        out
    }

    fn collect_binds_ts(&self, pat: &AIRNode, access: &str, out: &mut String) {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => {
                let _ = write!(out, "const {} = {access}; ", ts_value_ident(&name.name));
            }
            NodeKind::ConstructorPat { fields, .. } => {
                for (i, field) in fields.iter().enumerate() {
                    self.collect_binds_ts(field, &format!("{access}._{i}"), out);
                }
            }
            NodeKind::RecordPat { fields, .. } => {
                for f in fields {
                    let field_access = format!("{access}.{}", f.name.name);
                    match &f.pattern {
                        Some(p) => self.collect_binds_ts(p, &field_access, out),
                        None => {
                            let _ = write!(out, "const {} = {field_access}; ", f.name.name);
                        }
                    }
                }
            }
            NodeKind::TuplePat { elems } => {
                for (i, e) in elems.iter().enumerate() {
                    self.collect_binds_ts(e, &format!("{access}[{i}]"), out);
                }
            }
            NodeKind::OrPat { alternatives } => {
                if let Some(first) = alternatives.first() {
                    self.collect_binds_ts(first, access, out);
                }
            }
            _ => {}
        }
    }

    // ── Pipe operator ───────────────────────────────────────────────────────

    fn emit_pipe(&mut self, left: &AIRNode, right: &AIRNode) -> Result<(), CodegenError> {
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
        self.emit_expr(right)?;
        self.buf.push('(');
        self.emit_expr(left)?;
        self.buf.push(')');
        Ok(())
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn emit_block_body(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::Block { stmts, tail } = &node.kind {
            for s in stmts {
                self.emit_node(s)?;
            }
            if let Some(t) = tail {
                if crate::generator::node_is_statement(t) {
                    self.emit_node(t)?;
                    return Ok(());
                }
                if let NodeKind::Match { scrutinee, arms } = &t.kind {
                    if crate::generator::match_has_statement_arm(arms) {
                        self.emit_match(scrutinee, arms, false)?;
                        return Ok(());
                    }
                }
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}return ");
                self.emit_expr(t)?;
                self.buf.push_str(";\n");
            }
        } else if crate::generator::node_is_statement(node) {
            self.emit_node(node)?;
        } else if let NodeKind::Match { scrutinee, arms } = &node.kind {
            if crate::generator::match_has_statement_arm(arms) {
                self.emit_match(scrutinee, arms, false)?;
            } else {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}return ");
                self.emit_expr(node)?;
                self.buf.push_str(";\n");
            }
        } else {
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}return ");
            self.emit_expr(node)?;
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
        self.emit_expr(node)
    }

    fn pattern_to_binding_name(&self, pat: &AIRNode) -> String {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => ts_value_ident(&name.name),
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

    fn pattern_to_ts_destructure(&self, pat: &AIRNode) -> String {
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

/// Build the `: T` return-type clause for a TS function signature, wrapping
/// the inner type in `Promise<...>` when the function is async. An async
/// function with no declared return type is typed `Promise<void>`.
fn build_ts_return_type(is_async: bool, inner: Option<String>) -> String {
    match (is_async, inner) {
        (true, Some(t)) => format!(": Promise<{t}>"),
        (true, None) => ": Promise<void>".to_string(),
        (false, Some(t)) => format!(": {t}"),
        (false, None) => String::new(),
    }
}

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
/// name) to its TS form: `camelCase`, then escaped against the TS reserved-word
/// set so a binding named e.g. `default`/`type` emits `default_`/`type_` rather
/// than the illegal bare keyword. Apply at every value declaration and reference
/// site so the escaped name is used uniformly; member/method names use bare
/// [`to_camel_case`]. See [`crate::generator::escape_target_keyword`].
fn ts_value_ident(name: &str) -> String {
    crate::generator::escape_target_keyword(
        &to_camel_case(name),
        crate::generator::KeywordTarget::Ts,
    )
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

/// Escape special characters in a JS/TS string literal.
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

/// Render a literal as a TS value expression — used by the if-chain match
/// lowering to compare a scrutinee against a literal pattern (`<access> === …`).
fn ts_literal(lit: &Literal) -> String {
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

/// Escape special characters in a JS/TS template literal.
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
    use bock_ast::{GenericParam, Ident, TypeExpr, TypePath};
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

    fn typed_param_node(id: u32, name: &str, ty_name: &str) -> AIRNode {
        node(
            id,
            NodeKind::Param {
                pattern: Box::new(bind_pat(id + 100, name)),
                ty: Some(Box::new(node(
                    id + 200,
                    NodeKind::TypeNamed {
                        path: type_path(&[ty_name]),
                        args: vec![],
                    },
                ))),
                default: None,
            },
        )
    }

    fn type_node(id: u32, name: &str) -> AIRNode {
        node(
            id,
            NodeKind::TypeNamed {
                path: type_path(&[name]),
                args: vec![],
            },
        )
    }

    /// A parameter with no type annotation (e.g. the `self` receiver of an
    /// impl method, which Bock declares as bare `self`).
    fn untyped_param_node(id: u32, name: &str) -> AIRNode {
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
        let gen = TsGenerator::new();
        let result = gen.generate_module(module).unwrap();
        result.files[0].content.clone()
    }

    fn make_generic_param(name: &str) -> GenericParam {
        GenericParam {
            id: 0,
            span: span(),
            name: ident(name),
            bounds: vec![],
        }
    }

    fn make_bounded_generic_param(name: &str, bounds: &[&str]) -> GenericParam {
        GenericParam {
            id: 0,
            span: span(),
            name: ident(name),
            bounds: bounds.iter().map(|b| type_path(&[b])).collect(),
        }
    }

    fn make_type_expr(name: &str) -> TypeExpr {
        TypeExpr::Named {
            id: 0,
            span: span(),
            path: type_path(&[name]),
            args: vec![],
        }
    }

    fn make_record_field(name: &str, ty_name: &str) -> bock_ast::RecordDeclField {
        bock_ast::RecordDeclField {
            id: 0,
            span: span(),
            name: ident(name),
            ty: make_type_expr(ty_name),
            default: None,
        }
    }

    // ── Basic tests ─────────────────────────────────────────────────────────

    #[test]
    fn implements_code_generator_trait() {
        let gen = TsGenerator::new();
        assert_eq!(gen.target().id, "ts");
    }

    #[test]
    fn empty_module() {
        let m = module(vec![], vec![]);
        let out = gen(&m);
        assert_eq!(out, "");
    }

    #[test]
    fn generate_project_uses_source_mirrored_path_for_ts() {
        let gen = TsGenerator::new();
        let m = module(vec![], vec![]);
        let src_path = std::path::Path::new("src/lib.bock");
        let result = gen.generate_project(&[(&m, src_path)]).unwrap();
        assert_eq!(result.files[0].path, std::path::PathBuf::from("lib.ts"));
    }

    // ── Type annotations ────────────────────────────────────────────────────

    #[test]
    fn function_with_type_annotations() {
        let body = block(10, vec![], Some(id_node(11, "x")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("add"),
                generic_params: vec![],
                params: vec![
                    typed_param_node(2, "x", "Int"),
                    typed_param_node(3, "y", "Int"),
                ],
                return_type: Some(Box::new(type_node(4, "Int"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("x: number"), "got: {out}");
        assert!(out.contains("y: number"), "got: {out}");
        assert!(out.contains("): number"), "got: {out}");
        assert!(out.contains("export function add"));
    }

    #[test]
    fn function_without_type_annotations() {
        let body = block(10, vec![], Some(int_lit(11, "42")));
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
        assert!(out.contains("function answer()"), "got: {out}");
        assert!(!out.contains("export"), "got: {out}");
    }

    // ── Generics ────────────────────────────────────────────────────────────

    #[test]
    fn function_with_generics() {
        let body = block(10, vec![], Some(id_node(11, "x")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("identity"),
                generic_params: vec![make_generic_param("T")],
                params: vec![typed_param_node(2, "x", "T")],
                return_type: Some(Box::new(type_node(3, "T"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("function identity<T>"), "got: {out}");
        assert!(out.contains("x: T"), "got: {out}");
        assert!(out.contains("): T"), "got: {out}");
    }

    #[test]
    fn generics_with_bounds() {
        let body = block(10, vec![], Some(id_node(11, "x")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("sorted"),
                generic_params: vec![make_bounded_generic_param("T", &["Comparable"])],
                params: vec![typed_param_node(2, "x", "T")],
                return_type: Some(Box::new(type_node(3, "T"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("T extends Comparable"), "got: {out}");
    }

    // ── Traits → Interfaces ─────────────────────────────────────────────────

    #[test]
    fn trait_becomes_interface() {
        let method = node(
            2,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("area"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(type_node(3, "Float"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(4, vec![], None)),
            },
        );
        let trait_decl = node(
            1,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Shape"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![method],
            },
        );
        let out = gen(&module(vec![], vec![trait_decl]));
        assert!(out.contains("export interface Shape"), "got: {out}");
        assert!(out.contains("area(): number"), "got: {out}");
    }

    #[test]
    fn trait_with_generics() {
        let method = node(
            2,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("compare"),
                generic_params: vec![],
                params: vec![typed_param_node(3, "other", "T")],
                return_type: Some(Box::new(type_node(4, "Int"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(5, vec![], None)),
            },
        );
        let trait_decl = node(
            1,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Comparable"),
                generic_params: vec![make_generic_param("T")],
                associated_types: vec![],
                methods: vec![method],
            },
        );
        let out = gen(&module(vec![], vec![trait_decl]));
        assert!(out.contains("interface Comparable<T>"), "got: {out}");
        assert!(out.contains("compare(other: T): number"), "got: {out}");
    }

    #[test]
    fn trait_decl_self_param_typed_as_trait_interface() {
        // P2 item 2: a trait method whose leading `self` is untyped must be
        // typed as the trait's own interface type (here `Eq`) — otherwise `tsc
        // --strict` flags `self` as an implicit `any`.
        let method = node(
            2,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("equals"),
                generic_params: vec![],
                params: vec![untyped_param_node(3, "self")],
                return_type: Some(Box::new(type_node(4, "Bool"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(5, vec![], None)),
            },
        );
        let trait_decl = node(
            1,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Eq"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![method],
            },
        );
        let out = gen(&module(vec![], vec![trait_decl]));
        assert!(
            out.contains("equals(self: Eq): boolean"),
            "trait `self` should be typed as the trait interface, got: {out}"
        );
    }

    // ── Records → Interfaces ────────────────────────────────────────────────

    #[test]
    fn record_becomes_interface_and_factory() {
        let record = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Point"),
                generic_params: vec![],
                fields: vec![
                    make_record_field("x", "Float"),
                    make_record_field("y", "Float"),
                ],
            },
        );
        let out = gen(&module(vec![], vec![record]));
        assert!(out.contains("export class Point"), "got: {out}");
        assert!(out.contains("x: number"), "got: {out}");
        assert!(out.contains("y: number"), "got: {out}");
        assert!(
            out.contains("constructor({ x, y }: { x: number; y: number })"),
            "got: {out}"
        );
        assert!(out.contains("this.x = x;"), "got: {out}");
        assert!(out.contains("this.y = y;"), "got: {out}");
    }

    // ── Enums → Discriminated unions ────────────────────────────────────────

    #[test]
    fn enum_becomes_discriminated_union() {
        let variants = vec![
            node(
                2,
                NodeKind::EnumVariant {
                    name: ident("None"),
                    payload: EnumVariantPayload::Unit,
                },
            ),
            node(
                3,
                NodeKind::EnumVariant {
                    name: ident("Some"),
                    payload: EnumVariantPayload::Struct(vec![make_record_field("value", "T")]),
                },
            ),
        ];
        let enum_decl = node(
            1,
            NodeKind::EnumDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Option"),
                generic_params: vec![make_generic_param("T")],
                variants,
            },
        );
        let out = gen(&module(vec![], vec![enum_decl]));
        // Union type
        assert!(
            out.contains("export type Option<T> = Option_None | Option_Some;"),
            "got: {out}"
        );
        // Unit variant
        assert!(out.contains("interface Option_None"), "got: {out}");
        assert!(out.contains("readonly _tag: \"None\""), "got: {out}");
        // Struct variant
        assert!(out.contains("interface Option_Some<T>"), "got: {out}");
        assert!(out.contains("readonly value: T"), "got: {out}");
        assert!(
            out.contains("function Option_Some<T>(value: T): Option_Some"),
            "got: {out}"
        );
    }

    // ── Type aliases ────────────────────────────────────────────────────────

    #[test]
    fn type_alias_emitted() {
        let alias = node(
            1,
            NodeKind::TypeAlias {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("UserId"),
                generic_params: vec![],
                ty: Box::new(type_node(2, "String")),
                where_clause: vec![],
            },
        );
        let out = gen(&module(vec![], vec![alias]));
        assert!(out.contains("export type UserId = string;"), "got: {out}");
    }

    #[test]
    fn generic_type_alias() {
        let alias = node(
            1,
            NodeKind::TypeAlias {
                annotations: vec![],
                visibility: Visibility::Private,
                name: ident("Pair"),
                generic_params: vec![make_generic_param("A"), make_generic_param("B")],
                ty: Box::new(node(
                    2,
                    NodeKind::TypeTuple {
                        elems: vec![type_node(3, "A"), type_node(4, "B")],
                    },
                )),
                where_clause: vec![],
            },
        );
        let out = gen(&module(vec![], vec![alias]));
        assert!(out.contains("type Pair<A, B> = [A, B];"), "got: {out}");
    }

    // ── Effects → typed parameters ──────────────────────────────────────────

    #[test]
    fn effects_as_typed_params() {
        let body = block(10, vec![], None);
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("process"),
                generic_params: vec![],
                params: vec![typed_param_node(2, "data", "String")],
                return_type: None,
                effect_clause: vec![type_path(&["Log"]), type_path(&["Clock"])],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("{ log, clock }: { log: Log, clock: Clock }"),
            "got: {out}"
        );
    }

    // ── Async functions ─────────────────────────────────────────────────────

    #[test]
    fn async_function_with_types() {
        let body = block(10, vec![], Some(str_lit(11, "done")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: true,
                name: ident("fetch"),
                generic_params: vec![],
                params: vec![typed_param_node(2, "url", "String")],
                return_type: Some(Box::new(type_node(3, "String"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("export async function fetch"), "got: {out}");
        assert!(out.contains("url: string"), "got: {out}");
        // Async declared return type is wrapped in Promise<T>.
        assert!(out.contains("): Promise<string>"), "got: {out}");
    }

    #[test]
    fn async_function_without_return_type_is_promise_void() {
        let body = block(10, vec![], None);
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("tick"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("async function tick()"), "got: {out}");
        assert!(out.contains("): Promise<void>"), "got: {out}");
    }

    #[test]
    fn sync_function_return_type_unchanged() {
        let body = block(10, vec![], Some(str_lit(11, "done")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("hello"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(type_node(2, "String"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("function hello(): string"), "got: {out}");
        assert!(!out.contains("Promise"), "got: {out}");
    }

    #[test]
    fn entry_invocation_async_main_ts() {
        let inv = TsGenerator::new().entry_invocation(true).unwrap();
        assert!(inv.contains("async () =>"));
        assert!(inv.contains("await main()"));
    }

    #[test]
    fn generate_project_async_main_wraps_entry_ts() {
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
        let gen = TsGenerator::new();
        let src_path = std::path::Path::new("src/main.bock");
        let out = gen.generate_project(&[(&m, src_path)]).unwrap();
        let src = &out.files[0].content;
        assert_eq!(out.files[0].path, std::path::PathBuf::from("main.ts"));
        assert!(src.contains("async function main()"), "got: {src}");
        assert!(
            src.contains("(async () => { await main(); })();"),
            "got: {src}"
        );
    }

    // ── Let bindings with type annotations ──────────────────────────────────

    #[test]
    fn let_binding_with_type() {
        let stmt = node(
            1,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(2, "x")),
                ty: Some(Box::new(type_node(3, "Int"))),
                value: Box::new(int_lit(4, "42")),
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
                body: Box::new(block(6, vec![stmt], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("const x: number = 42;"), "got: {out}");
    }

    #[test]
    fn mutable_binding_with_type() {
        let stmt = node(
            1,
            NodeKind::LetBinding {
                is_mut: true,
                pattern: Box::new(bind_pat(2, "count")),
                ty: Some(Box::new(type_node(3, "Int"))),
                value: Box::new(int_lit(4, "0")),
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
                body: Box::new(block(6, vec![stmt], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("let count: number = 0;"), "got: {out}");
    }

    // ── Type mapping ────────────────────────────────────────────────────────

    #[test]
    fn type_mapping_primitives() {
        let ctx = TsEmitCtx::new();
        assert_eq!(ctx.map_type_name("Int"), "number");
        assert_eq!(ctx.map_type_name("Float"), "number");
        assert_eq!(ctx.map_type_name("Bool"), "boolean");
        assert_eq!(ctx.map_type_name("String"), "string");
        assert_eq!(ctx.map_type_name("Void"), "void");
        assert_eq!(ctx.map_type_name("Unit"), "void");
        assert_eq!(ctx.map_type_name("List"), "Array");
        assert_eq!(ctx.map_type_name("CustomType"), "CustomType");
    }

    #[test]
    fn optional_type_emitted() {
        // `T?` must lower to the tagged `BockOption<T>` union (not `T | null`):
        // the runtime value is `{ _tag: "Some", _0: v }` / `{ _tag: "None" }`,
        // so the type has to describe that for `tsc` to accept it (Q-ts-codegen).
        let ctx = TsEmitCtx::new();
        let opt = node(
            1,
            NodeKind::TypeOptional {
                inner: Box::new(type_node(2, "String")),
            },
        );
        assert_eq!(ctx.type_to_ts(&opt), "BockOption<string>");
    }

    #[test]
    fn generic_type_args() {
        let ctx = TsEmitCtx::new();
        let list_of_int = node(
            1,
            NodeKind::TypeNamed {
                path: type_path(&["List"]),
                args: vec![type_node(2, "Int")],
            },
        );
        assert_eq!(ctx.type_to_ts(&list_of_int), "Array<number>");
    }

    #[test]
    fn function_type() {
        let ctx = TsEmitCtx::new();
        let fn_type = node(
            1,
            NodeKind::TypeFunction {
                params: vec![type_node(2, "Int"), type_node(3, "String")],
                ret: Box::new(type_node(4, "Bool")),
                effects: vec![],
            },
        );
        assert_eq!(
            ctx.type_to_ts(&fn_type),
            "(arg0: number, arg1: string) => boolean"
        );
    }

    #[test]
    fn tuple_type() {
        let ctx = TsEmitCtx::new();
        let tuple = node(
            1,
            NodeKind::TypeTuple {
                elems: vec![type_node(2, "Int"), type_node(3, "String")],
            },
        );
        assert_eq!(ctx.type_to_ts(&tuple), "[number, string]");
    }

    // ── Constants with types ────────────────────────────────────────────────

    #[test]
    fn const_with_type() {
        let c = node(
            1,
            NodeKind::ConstDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("PI"),
                ty: Box::new(type_node(2, "Float")),
                value: Box::new(node(
                    3,
                    NodeKind::Literal {
                        lit: Literal::Float("3.14159".into()),
                    },
                )),
            },
        );
        let out = gen(&module(vec![], vec![c]));
        assert!(
            out.contains("export const PI: number = 3.14159;"),
            "got: {out}"
        );
    }

    // ── Class with types ────────────────────────────────────────────────────

    #[test]
    fn class_with_typed_fields() {
        let class = node(
            1,
            NodeKind::ClassDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Point"),
                generic_params: vec![],
                base: None,
                traits: vec![],
                fields: vec![
                    make_record_field("x", "Float"),
                    make_record_field("y", "Float"),
                ],
                methods: vec![],
            },
        );
        let out = gen(&module(vec![], vec![class]));
        assert!(out.contains("export class Point"), "got: {out}");
        assert!(out.contains("x: number;"), "got: {out}");
        assert!(out.contains("y: number;"), "got: {out}");
        assert!(
            out.contains("constructor(x: number, y: number)"),
            "got: {out}"
        );
    }

    // ── Effect declarations → interfaces ────────────────────────────────────

    #[test]
    fn effect_becomes_interface() {
        let op = node(
            2,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("log"),
                generic_params: vec![],
                params: vec![typed_param_node(3, "msg", "String")],
                return_type: Some(Box::new(type_node(4, "Void"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(5, vec![], None)),
            },
        );
        let effect = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Logger"),
                generic_params: vec![],
                components: vec![],
                operations: vec![op],
            },
        );
        let out = gen(&module(vec![], vec![effect]));
        assert!(out.contains("interface Logger"), "got: {out}");
        assert!(out.contains("log(msg: string): void"), "got: {out}");
    }

    // ── Ownership erasure ───────────────────────────────────────────────────

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
        let stmts = vec![
            node(
                5,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(6, "a")),
                    ty: None,
                    value: Box::new(move_expr),
                },
            ),
            node(
                7,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(8, "b")),
                    ty: None,
                    value: Box::new(borrow_expr),
                },
            ),
        ];
        let f = node(
            9,
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
                body: Box::new(block(10, stmts, None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("const a = x;"), "got: {out}");
        assert!(out.contains("const b = y;"), "got: {out}");
    }

    // ── String interpolation ────────────────────────────────────────────────

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
        let stmt = node(
            3,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(4, "msg")),
                ty: None,
                value: Box::new(interp),
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
                body: Box::new(block(6, vec![stmt], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("`Hello, ${name}!`"), "got: {out}");
    }

    // ── Collections ─────────────────────────────────────────────────────────

    #[test]
    fn collections() {
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
        let stmts = vec![
            node(
                11,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(12, "xs")),
                    ty: None,
                    value: Box::new(list),
                },
            ),
            node(
                13,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(14, "m")),
                    ty: None,
                    value: Box::new(map),
                },
            ),
            node(
                15,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(16, "s")),
                    ty: None,
                    value: Box::new(set),
                },
            ),
        ];
        let f = node(
            17,
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
                body: Box::new(block(18, stmts, None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("[1, 2, 3]"), "got: {out}");
        assert!(out.contains("new Map("), "got: {out}");
        assert!(out.contains("new Set("), "got: {out}");
    }

    // ── Result types with as const ──────────────────────────────────────────

    #[test]
    fn result_construct_has_as_const() {
        let ok = node(
            1,
            NodeKind::ResultConstruct {
                variant: ResultVariant::Ok,
                value: Some(Box::new(int_lit(2, "42"))),
            },
        );
        let stmt = node(
            3,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(4, "r")),
                ty: None,
                value: Box::new(ok),
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
                body: Box::new(block(6, vec![stmt], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        // Reconciled on the `_0` payload key the `Result` match reads.
        assert!(
            out.contains("{ _tag: \"Ok\" as const, _0: 42 }"),
            "got: {out}"
        );
    }

    // ── Record construct ────────────────────────────────────────────────────

    #[test]
    fn record_construct() {
        let rc = node(
            1,
            NodeKind::RecordConstruct {
                path: type_path(&["Point"]),
                fields: vec![
                    AirRecordField {
                        name: ident("x"),
                        value: Some(Box::new(int_lit(2, "1"))),
                    },
                    AirRecordField {
                        name: ident("y"),
                        value: Some(Box::new(int_lit(3, "2"))),
                    },
                ],
                spread: None,
            },
        );
        let stmt = node(
            4,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(5, "p")),
                ty: None,
                value: Box::new(rc),
            },
        );
        let f = node(
            6,
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
                body: Box::new(block(7, vec![stmt], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("{ x: 1, y: 2 }"), "got: {out}");
    }

    #[test]
    fn to_camel_case_converts_snake_case() {
        assert_eq!(to_camel_case("create_user"), "createUser");
        assert_eq!(to_camel_case("get_all_items"), "getAllItems");
        assert_eq!(to_camel_case("Log"), "log");
        assert_eq!(to_camel_case("createUser"), "createUser");
        assert_eq!(to_camel_case("_"), "_");
        assert_eq!(to_camel_case(""), "");
    }

    #[test]
    fn snake_case_fn_becomes_camel_case_ts() {
        let body = block(2, vec![], Some(int_lit(3, "42")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("create_user"),
                generic_params: vec![],
                params: vec![typed_param_node(4, "name", "String")],
                return_type: Some(Box::new(type_node(5, "Int"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("function createUser("),
            "expected camelCase function name, got: {out}"
        );
        assert!(
            out.contains("name: string"),
            "expected type annotations, got: {out}"
        );
    }

    // ── Prelude function mapping tests ──────────────────────────────────────

    /// Helper: generate TypeScript for a module with a `main` function containing a single call.
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

    /// Helper: generate TypeScript for a nullary prelude call (no args).
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
        let out = gen_prelude_call("println", str_lit(12, "hello"));
        assert!(
            out.contains("console.log("),
            "println should map to console.log, got: {out}"
        );
        assert!(
            !out.contains("println("),
            "should not emit bare println(, got: {out}"
        );
    }

    #[test]
    fn prelude_print_maps_to_process_stdout() {
        let out = gen_prelude_call("print", str_lit(12, "hello"));
        assert!(
            out.contains("process.stdout.write(String("),
            "print should map to process.stdout.write, got: {out}"
        );
    }

    #[test]
    fn prelude_debug_maps_to_console_debug() {
        let out = gen_prelude_call("debug", str_lit(12, "val"));
        assert!(
            out.contains("console.debug("),
            "debug should map to console.debug, got: {out}"
        );
    }

    #[test]
    fn prelude_assert_maps_to_throw() {
        let arg = node(
            12,
            NodeKind::Literal {
                lit: Literal::Bool(true),
            },
        );
        let out = gen_prelude_call("assert", arg);
        assert!(
            out.contains("if (!true) throw new Error(\"assertion failed\")"),
            "assert should map to if-throw, got: {out}"
        );
    }

    #[test]
    fn prelude_todo_maps_to_throw_not_implemented() {
        let out = gen_prelude_call_no_args("todo");
        assert!(
            out.contains("throw new Error(\"not implemented\")"),
            "todo should map to throw, got: {out}"
        );
    }

    #[test]
    fn prelude_unreachable_maps_to_throw_unreachable() {
        let out = gen_prelude_call_no_args("unreachable");
        assert!(
            out.contains("throw new Error(\"unreachable\")"),
            "unreachable should map to throw, got: {out}"
        );
    }

    #[test]
    fn non_prelude_call_passes_through() {
        let out = gen_prelude_call("my_custom_func", str_lit(12, "arg"));
        assert!(
            out.contains("myCustomFunc("),
            "non-prelude call should use camelCase, got: {out}"
        );
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
                        params: vec![typed_param_node(3, "msg", "String")],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], None)),
                    },
                )],
            },
        );

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
        // TS: inner({ logger: __logger })
        assert!(
            out.contains("inner({ logger: __logger })"),
            "handling block should pass handler to effectful call, got: {out}"
        );
        assert!(
            out.contains("const __logger: Logger = stdoutLogger()"),
            "handling block should instantiate handler with type, got: {out}"
        );
    }

    #[test]
    fn record_becomes_class() {
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
            out.contains("export class ConsoleLogger {}"),
            "empty record should be an empty exported class, got: {out}"
        );
    }

    #[test]
    fn impl_emits_interface_extension_for_declaration_merging() {
        use bock_air::AirHandlerPair;
        let _ = AirHandlerPair {
            effect: type_path(&["X"]),
            handler: Box::new(id_node(0, "x")),
        };

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
                        params: vec![typed_param_node(3, "msg", "String")],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], None)),
                    },
                )],
            },
        );

        let rec = node(
            5,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("StdLogger"),
                generic_params: vec![],
                fields: vec![],
            },
        );

        let impl_block = node(
            10,
            NodeKind::ImplBlock {
                annotations: vec![],
                trait_path: Some(type_path(&["Logger"])),
                trait_args: vec![],
                target: Box::new(type_node(11, "StdLogger")),
                generic_params: vec![],
                methods: vec![node(
                    12,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("log"),
                        generic_params: vec![],
                        params: vec![typed_param_node(13, "msg", "String")],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(14, vec![], None)),
                    },
                )],
                where_clause: vec![],
            },
        );

        let out = gen(&module(vec![], vec![effect_decl, rec, impl_block]));
        assert!(
            out.contains("interface StdLogger extends Logger {"),
            "impl should emit interface extension for declaration merging, got: {out}"
        );
        // The merged interface also declares the concrete method signatures so
        // `x.log(...)` call sites resolve against the class type.
        assert!(
            out.contains("log(msg: string): void;"),
            "merged interface should declare the method signature, got: {out}"
        );
        assert!(
            out.contains("StdLogger.prototype.log"),
            "impl should attach method to prototype, got: {out}"
        );
    }

    #[test]
    fn generic_trait_impl_extends_clause_carries_type_args() {
        // GAP-A: `impl P[T] for R[T]` for a generic `trait P[T]` must emit
        // `interface R<T> extends P<T>` — the `extends` clause carries the
        // impl's trait type-args. Without them `tsc` rejects with TS2314.
        let rec = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("R"),
                generic_params: vec![make_generic_param("T")],
                fields: vec![make_record_field("v", "T")],
            },
        );
        let impl_block = node(
            10,
            NodeKind::ImplBlock {
                annotations: vec![],
                trait_path: Some(type_path(&["P"])),
                trait_args: vec![node(
                    11,
                    NodeKind::TypeNamed {
                        path: type_path(&["T"]),
                        args: vec![],
                    },
                )],
                target: Box::new(node(
                    12,
                    NodeKind::TypeNamed {
                        path: type_path(&["R"]),
                        args: vec![node(
                            13,
                            NodeKind::TypeNamed {
                                path: type_path(&["T"]),
                                args: vec![],
                            },
                        )],
                    },
                )),
                generic_params: vec![],
                methods: vec![node(
                    14,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("f"),
                        generic_params: vec![],
                        params: vec![untyped_param_node(15, "self")],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(16, vec![], None)),
                    },
                )],
                where_clause: vec![],
            },
        );
        let out = gen(&module(vec![], vec![rec, impl_block]));
        assert!(
            out.contains("interface R<T> extends P<T> {"),
            "generic trait impl's extends clause must carry `<T>`, got: {out}"
        );
    }

    #[test]
    fn optional_named_type_maps_to_bock_option() {
        // The spelled-out `Optional[T]` named type must lower to `BockOption<T>`
        // (matching the `T?` shorthand and the emitted tagged value), not a
        // bare `Optional<T>` (TS2304 undefined name).
        let ctx = TsEmitCtx::new();
        let opt = node(
            1,
            NodeKind::TypeNamed {
                path: type_path(&["Optional"]),
                args: vec![node(
                    2,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                )],
            },
        );
        assert_eq!(ctx.type_to_ts(&opt), "BockOption<string>");
    }

    #[test]
    fn impl_self_method_typed_and_declaration_merged() {
        // Q-ts-codegen defect 1: an inherent impl method with a bare `self`
        // receiver. The AIR keeps `self` as a real param and prepends the
        // receiver at call sites. TS must (a) type `self` as the impl target
        // (no implicit `any`) and (b) declare the method on the class via a
        // merged interface so `p.sum(p)` type-checks.
        let rec = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Point"),
                generic_params: vec![],
                fields: vec![make_record_field("x", "Int"), make_record_field("y", "Int")],
            },
        );
        let body = block(
            20,
            vec![],
            Some(node(
                21,
                NodeKind::BinaryOp {
                    op: BinOp::Add,
                    left: Box::new(node(
                        22,
                        NodeKind::FieldAccess {
                            object: Box::new(id_node(23, "self")),
                            field: ident("x"),
                        },
                    )),
                    right: Box::new(node(
                        24,
                        NodeKind::FieldAccess {
                            object: Box::new(id_node(25, "self")),
                            field: ident("y"),
                        },
                    )),
                },
            )),
        );
        let impl_block = node(
            10,
            NodeKind::ImplBlock {
                annotations: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(type_node(11, "Point")),
                generic_params: vec![],
                methods: vec![node(
                    12,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("sum"),
                        generic_params: vec![],
                        params: vec![untyped_param_node(13, "self")],
                        return_type: Some(Box::new(type_node(14, "Int"))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(body),
                    },
                )],
                where_clause: vec![],
            },
        );
        let out = gen(&module(vec![], vec![rec, impl_block]));
        assert!(
            out.contains("interface Point {"),
            "inherent impl should emit a declaration-merging interface, got: {out}"
        );
        assert!(
            out.contains("sum(self: Point): number;"),
            "merged interface should declare the self-typed method, got: {out}"
        );
        assert!(
            out.contains("Point.prototype.sum = function(self: Point): number {"),
            "prototype function should type the self param as the target, got: {out}"
        );
    }

    /// A plain inherent `impl` method that names `Self` in its return AND its
    /// parameter type must render `Self` as the concrete target (`Counter`), not
    /// the `this` type. Each impl method emits as a free prototype function
    /// (`Counter.prototype.m = function(...): this`), and `tsc` rejects a `this`
    /// type outside a class/interface member (TS2526). Before the P4 fix
    /// `trait_self_subst` was set only for synthesized trait *default* methods;
    /// an inherent-impl `Self` lowered to `this`. Both the merged-interface
    /// signature and the prototype function must agree, or declaration merging
    /// breaks.
    #[test]
    fn self_in_plain_impl_resolves_to_target_not_this() {
        let rec = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Counter"),
                generic_params: vec![],
                fields: vec![make_record_field("value", "Int")],
            },
        );
        let other_param = node(
            30,
            NodeKind::Param {
                pattern: Box::new(bind_pat(31, "other")),
                ty: Some(Box::new(node(32, NodeKind::TypeSelf))),
                default: None,
            },
        );
        let impl_block = node(
            10,
            NodeKind::ImplBlock {
                annotations: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(type_node(11, "Counter")),
                generic_params: vec![],
                methods: vec![node(
                    12,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("combine"),
                        generic_params: vec![],
                        params: vec![untyped_param_node(13, "self"), other_param],
                        return_type: Some(Box::new(node(14, NodeKind::TypeSelf))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(15, vec![], None)),
                    },
                )],
                where_clause: vec![],
            },
        );
        let out = gen(&module(vec![], vec![rec, impl_block]));
        assert!(
            !out.contains(": this"),
            "Self must not lower to the `this` type (TS2526), got: {out}"
        );
        assert!(
            out.contains("combine(self: Counter, other: Counter): Counter;"),
            "merged interface should render Self as the target in param & return, got: {out}"
        );
        assert!(
            out.contains(
                "Counter.prototype.combine = function(self: Counter, other: Counter): Counter {"
            ),
            "prototype function should render Self as the target in param & return, got: {out}"
        );
    }

    #[test]
    fn optional_runtime_prelude_and_value_type_agree() {
        // Q-ts-codegen defect 2: the Optional *type* and *value* must agree.
        // A function returning `Int?` gets `BockOption<number>`, the prelude
        // type is emitted, and `Some`/`None` lower to the matching tagged
        // objects.
        let body = block(
            20,
            vec![],
            Some(node(
                21,
                NodeKind::Call {
                    callee: Box::new(id_node(22, "Some")),
                    args: vec![AirArg {
                        label: None,
                        value: int_lit(23, "7"),
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
                name: ident("pick"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(node(
                    2,
                    NodeKind::TypeOptional {
                        inner: Box::new(type_node(3, "Int")),
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("type BockOption<T> ="),
            "Optional runtime type prelude should be emitted, got: {out}"
        );
        assert!(
            out.contains("): BockOption<number> {"),
            "Optional return type should be BockOption<number>, got: {out}"
        );
        assert!(
            out.contains("{ _tag: \"Some\" as const, _0: 7 }"),
            "Some should lower to the matching tagged-object value, got: {out}"
        );
    }

    /// A `match` whose scrutinee is a call (not a bare identifier) must hoist it
    /// into a single `const __matchN = …;`. Re-emitting the call inline at the
    /// switch head and in each payload binding both double-evaluated it and
    /// defeated TS discriminated-union narrowing (TS2339 on `_0`, since
    /// `f()._0` is a fresh, un-narrowed expression). The hoisted temp is a
    /// stable reference TS narrows correctly.
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
            "payload binding should read the hoisted temp (for narrowing), got: {out}"
        );
        // The call must not be re-emitted inline (single evaluation).
        assert!(
            !out.contains("f()._tag") && !out.contains("f()._0"),
            "call scrutinee must not be re-emitted inline, got: {out}"
        );
    }

    /// Q-match-exprpos (P4): an expression-position value `match` over a bare
    /// identifier, bound into a typed `let`. The IIFE arrow is annotated with the
    /// binding type (`(() : boolean => …)()`), and the bare scrutinee is hoisted
    /// into a temp (`const __matchN = n; switch (__matchN) …`) so the `switch`
    /// narrows the temp — not the original `n`. Without the hoist, `switch (n)`
    /// narrows `n` to the case literal inside each arm, so an arm body
    /// re-referencing `n` (`n === <other-literal>`) trips TS2367.
    #[test]
    fn expr_position_value_match_hoists_scrutinee_and_annotates_iife() {
        // let flag: Bool = match n { 0 => n; _ => n }   (in a fn returning Int)
        let zero_arm = node(
            20,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    21,
                    NodeKind::LiteralPat {
                        lit: Literal::Int("0".into()),
                    },
                )),
                guard: None,
                body: Box::new(block(22, vec![], Some(id_node(23, "n")))),
            },
        );
        let default_arm = node(
            30,
            NodeKind::MatchArm {
                pattern: Box::new(node(31, NodeKind::WildcardPat)),
                guard: None,
                body: Box::new(block(32, vec![], Some(id_node(33, "n")))),
            },
        );
        let m = node(
            40,
            NodeKind::Match {
                scrutinee: Box::new(id_node(41, "n")),
                arms: vec![zero_arm, default_arm],
            },
        );
        let let_flag = node(
            50,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(51, "flag")),
                ty: Some(Box::new(type_node(52, "Int"))),
                value: Box::new(m),
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
                params: vec![{
                    node(
                        2,
                        NodeKind::Param {
                            pattern: Box::new(bind_pat(3, "n")),
                            ty: Some(Box::new(type_node(4, "Int"))),
                            default: None,
                        },
                    )
                }],
                return_type: Some(Box::new(type_node(5, "Int"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(6, vec![let_flag], Some(id_node(7, "flag")))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("(() : number => {"),
            "value-position match IIFE arrow should be annotated with the binding type, got: {out}"
        );
        assert!(
            out.contains("const __match1 = n;"),
            "bare-identifier scrutinee must be hoisted in expression position, got: {out}"
        );
        assert!(
            out.contains("switch (__match1)"),
            "switch should dispatch on the hoisted temp, not the original binding, got: {out}"
        );
    }

    // ── Generic impl interface-merge (DV12 / P1-b2) ───────────────────────────

    fn generic_param(id: u32, name: &str) -> GenericParam {
        GenericParam {
            id,
            span: span(),
            name: ident(name),
            bounds: vec![],
        }
    }

    fn named_type(id: u32, name: &str) -> AIRNode {
        node(
            id,
            NodeKind::TypeNamed {
                path: type_path(&[name]),
                args: vec![],
            },
        )
    }

    /// `record Box[T] { value: T }`.
    fn generic_box_record() -> AIRNode {
        node(
            10,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                name: ident("Box"),
                generic_params: vec![generic_param(11, "T")],
                fields: vec![bock_ast::RecordDeclField {
                    id: 12,
                    span: span(),
                    name: ident("value"),
                    ty: TypeExpr::Named {
                        id: 13,
                        span: span(),
                        path: type_path(&["T"]),
                        args: vec![],
                    },
                    default: None,
                }],
            },
        )
    }

    /// `impl Box { fn get(self) -> T { return self.value } }`.
    fn generic_box_impl() -> AIRNode {
        let self_param = node(
            20,
            NodeKind::Param {
                pattern: Box::new(bind_pat(21, "self")),
                ty: None,
                default: None,
            },
        );
        let body = block(
            22,
            vec![],
            Some(node(
                23,
                NodeKind::Return {
                    value: Some(Box::new(node(
                        24,
                        NodeKind::FieldAccess {
                            object: Box::new(id_node(25, "self")),
                            field: ident("value"),
                        },
                    ))),
                },
            )),
        );
        let method = node(
            26,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("get"),
                generic_params: vec![],
                params: vec![self_param],
                return_type: Some(Box::new(named_type(27, "T"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        node(
            30,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(named_type(31, "Box")),
                where_clause: vec![],
                methods: vec![method],
            },
        )
    }

    #[test]
    fn generic_impl_merges_onto_generic_class() {
        // `impl Box { ... }` for `record Box[T]` must declaration-merge onto the
        // generic class: `interface Box<T>`, `self: Box<T>`, and a prototype
        // function that re-declares `<T>` (it lives outside the class scope).
        let out = gen(&module(
            vec![],
            vec![generic_box_record(), generic_box_impl()],
        ));
        assert!(
            out.contains("interface Box<T> {"),
            "merged interface should carry `<T>`, got: {out}"
        );
        assert!(
            out.contains("get(self: Box<T>): T;"),
            "interface signature should type `self` as `Box<T>`, got: {out}"
        );
        assert!(
            out.contains("Box.prototype.get = function<T>(self: Box<T>): T {"),
            "prototype function should re-declare `<T>` and reference `Box.prototype`, got: {out}"
        );
    }
}
