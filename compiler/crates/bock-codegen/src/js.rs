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
        let mut ctx = EmitCtx::new();
        ctx.enum_variants =
            crate::generator::collect_enum_variants(&[(module, std::path::Path::new(""))]);
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

    /// Bundle every module (stdlib + user, dependency-ordered) into one entry
    /// file. JS has no module wrapper — top-level declarations live in one
    /// scope — so concatenating each module's declarations is valid and makes a
    /// cross-module `use` (DV13) resolve against the same file. `ImportDecl`s
    /// are dropped during emission; the concurrency runtime is emitted once.
    ///
    /// This diverges from spec §20.6.1 (one output file per module). See the
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

        let mut ctx = EmitCtx::new();
        ctx.enum_variants = crate::generator::collect_enum_variants(modules);
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
    /// Set once the concurrency runtime prelude has been emitted, so a
    /// single-file **bundle** of several modules (cross-module `use`, DV13)
    /// emits the runtime at most once. A lone-module build sets it on first use
    /// exactly as before.
    concurrency_runtime_emitted: bool,
    /// User-enum-variant registry (DV14). Maps a variant name to its enum so a
    /// unit-variant reference lowers to the frozen `{enum}_{variant}` const, a
    /// struct/tuple construction lowers to the `{enum}_{variant}(..)` factory,
    /// and a `match` recognises struct-payload (`RecordPat`) arms as ADT
    /// variants. Pre-scanned across the bundle. The built-in Optional/Result
    /// pre-seeds are filtered out where bespoke lowering already applies.
    enum_variants: crate::generator::EnumVariantRegistry,
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
            enum_variants: crate::generator::EnumVariantRegistry::new(),
        }
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
                // Duration is ns → setTimeout takes ms.
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("new Promise((__r) => setTimeout(__r, Math.floor(({a}) / 1e6)))")
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
            ("Instant", "now") => "(performance.now() * 1000000)".to_string(),
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Recognise desugared method calls `Call(FieldAccess(recv, m), [recv, ...args])`
    /// on Duration/Instant values and emit inline arithmetic. Returns true if
    /// the call was emitted.
    fn try_emit_time_desugared_method(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
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

    /// Lower a primitive trait-bridge method call (`compare`/`eq`/`to_string`/
    /// `display` on a primitive receiver) to its JS form.
    ///
    /// `(1).compare(2)` resolves in the checker to `Ordering`, but a JS `number`
    /// has no `.compare`; this lowers it to a ternary that produces the same
    /// tagged-object `Ordering` value the construction/match sides use
    /// (`{ _tag: "Less" }` / `…"Equal"` / `…"Greater"`). `eq` becomes `===`;
    /// `to_string`/`display` become `String(x)`.
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
            NodeKind::Module { items, .. } => {
                // Cross-module `use` (DV13) is realized by single-file bundling:
                // every module body is concatenated into the one entry file and
                // `ImportDecl`s are dropped (the imported symbols are present in
                // the same file). The concurrency runtime is emitted at most once
                // across the bundle, gated on a ctx flag.
                if !self.concurrency_runtime_emitted && self.module_uses_concurrency(items) {
                    self.buf.push_str(CONCURRENCY_RUNTIME_JS);
                    self.buf.push('\n');
                    self.concurrency_runtime_emitted = true;
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
                // Bock `use` is resolved by bundling — every imported module's
                // top-level declarations are concatenated into this same file —
                // so the import statement itself is a no-op (DV13).
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
                for method in methods {
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
                        self.writeln(&format!(
                            "{target_name}.prototype.{} = {async_kw}function({}) {{",
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
        let js_name = to_camel_case(name);
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
            let method_name = to_camel_case(&name.name);
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
            self.emit_block_body(body)?;
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
        match &node.kind {
            NodeKind::LetBinding {
                is_mut,
                pattern,
                value,
                ..
            } => {
                let kw = if *is_mut { "let" } else { "const" };
                let binding = self.pattern_to_js_destructure(pattern);
                let ind = self.indent_str();
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
            NodeKind::Match { scrutinee, arms } => self.emit_match(scrutinee, arms),
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
                    // `compare` and the `_tag`-switch match use (the
                    // `core.compare` enum decl is not bundled single-file).
                    let _ = write!(self.buf, "{{ _tag: \"{variant}\" }}");
                } else if let Some(enum_name) = self
                    .user_variant_for_name(&name.name)
                    .map(|i| i.enum_name.clone())
                {
                    // A bare unit-variant reference (`Red`) → the frozen
                    // `{enum}_{variant}` const emitted by `emit_enum_variant`.
                    let _ = write!(self.buf, "{enum_name}_{}", name.name);
                } else {
                    self.buf.push_str(&to_camel_case(&name.name));
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
                    self.build_effects_call_arg_js(&name.name)
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
                let param_names = self.collect_param_names(params);
                let _ = write!(self.buf, "({}) => ", param_names.join(", "));
                // If body is a block, emit with braces; otherwise inline.
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
            NodeKind::Pipe { left, right } => {
                // Pipe `a |> f` → `f(a)`.
                // If right is a Call with a Placeholder, substitute left for it.
                self.emit_pipe(left, right)
            }
            NodeKind::Compose { left, right } => {
                // `f >> g` → `(x) => g(f(x))`
                let _ = write!(self.buf, "((x) => ");
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
                // `expr?` → JS doesn't have this; just emit the expression.
                // In a real compiler this would emit try/catch. For now, passthrough.
                self.emit_expr(expr)?;
                Ok(())
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
                            // Shorthand `{ radius }` ≡ `{ radius: radius }`.
                            None => self.buf.push_str(&to_camel_case(fname)),
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
                // Blocks in expression position → IIFE.
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
                // Match in expression position → IIFE with switch.
                self.buf.push_str("(() => {\n");
                self.indent += 1;
                self.emit_match(scrutinee, arms)?;
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
                NodeKind::BindPat { name, .. } if !is_adt => {
                    // Bind pattern as default with variable binding.
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
        // Simple case: `right(left)`
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
                // A statement tail (`break`/`continue`/`return`/assignment) is
                // emitted as a statement, never wrapped in `return`.
                if crate::generator::node_is_statement(t) {
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
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}return ");
                self.emit_expr(t)?;
                self.buf.push_str(";\n");
            }
        } else if crate::generator::node_is_statement(node) {
            self.emit_node(node)?;
        } else if let NodeKind::Match { scrutinee, arms } = &node.kind {
            if crate::generator::match_has_statement_arm(arms) {
                self.emit_match(scrutinee, arms)?;
            } else {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}return ");
                self.emit_expr(node)?;
                self.buf.push_str(";\n");
            }
        } else {
            // Single expression as body.
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
        // Fallback: emit as IIFE.
        self.emit_expr(node)
    }

    fn pattern_to_binding_name(&self, pat: &AIRNode) -> String {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => to_camel_case(&name.name),
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
}
