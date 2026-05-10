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
use bock_ast::{AssignOp, BinOp, ImportItems, Literal, TypeExpr, UnaryOp, Visibility};
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
        }
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
                format!("{} | null", self.type_to_ts(inner))
            }
            NodeKind::TypeSelf => "this".into(),
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
                format!("{} | null", self.ast_type_to_ts(inner))
            }
            TypeExpr::SelfType { .. } => "this".into(),
        }
    }

    /// Emit generic parameter list: `<T, U extends Foo>`.
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
            NodeKind::Module { imports, items, .. } => {
                if module_uses_concurrency(items) {
                    self.buf.push_str(CONCURRENCY_RUNTIME_TS);
                    self.buf.push('\n');
                }
                for imp in imports {
                    self.emit_node(imp)?;
                }
                if !imports.is_empty() && !items.is_empty() {
                    self.buf.push('\n');
                }
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    self.emit_node(item)?;
                }
                Ok(())
            }
            NodeKind::ImportDecl { path, items } => {
                let path_str = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                match items {
                    ImportItems::Module => {
                        self.writeln(&format!("// import {path_str}"));
                    }
                    ImportItems::Named(names) => {
                        let names_str = names
                            .iter()
                            .map(|n| n.name.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.writeln(&format!("// import {{ {names_str} }} from {path_str}"));
                    }
                    ImportItems::Glob => {
                        self.writeln(&format!("// import * from {path_str}"));
                    }
                }
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
                        let param_list = self.collect_typed_params(params);
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
                trait_path,
                target,
                methods,
                ..
            } => {
                let target_name = self.type_expr_to_string(target);
                if let Some(tp) = trait_path {
                    let trait_name = tp
                        .segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(".");
                    // Declaration merging: make the class satisfy the trait/effect
                    // so `.prototype.x = ...` below type-checks and `new Target()`
                    // is assignable to the trait's interface type.
                    self.writeln(&format!(
                        "interface {target_name} extends {trait_name} {{}}"
                    ));
                    self.writeln(&format!("// impl {trait_name} for {target_name}"));
                } else {
                    self.writeln(&format!("// impl {target_name}"));
                }
                for method in methods {
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
                        self.writeln(&format!(
                            "{target_name}.prototype.{} = {async_kw}function{generics}({}){ret_str} {{",
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
        let ts_name = to_camel_case(name);
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
                let ty_str = ty
                    .as_ref()
                    .map(|t| format!(": {}", self.type_to_ts(t)))
                    .unwrap_or_default();
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}{kw} {binding}{ty_str} = ");
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
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}for (const {binding} of ");
                self.emit_expr(iterable)?;
                self.buf.push_str(") {\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::While { condition, body } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}while (");
                self.emit_expr(condition)?;
                self.buf.push_str(") {\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::Loop { body } => {
                self.writeln("while (true) {");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
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
                    self.buf.push_str(" */ break;\n");
                } else {
                    self.writeln("break;");
                }
                Ok(())
            }
            NodeKind::Continue => {
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
                match variant {
                    ResultVariant::Ok => {
                        self.buf.push_str("{ _tag: \"Ok\" as const, value: ");
                        if let Some(v) = value {
                            self.emit_expr(v)?;
                        } else {
                            self.buf.push_str("undefined");
                        }
                        self.buf.push_str(" }");
                    }
                    ResultVariant::Err => {
                        self.buf.push_str("{ _tag: \"Err\" as const, error: ");
                        if let Some(v) = value {
                            self.emit_expr(v)?;
                        } else {
                            self.buf.push_str("undefined");
                        }
                        self.buf.push_str(" }");
                    }
                }
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
                // IIFE
                self.buf.push_str("(() => {\n");
                self.indent += 1;
                self.emit_match(scrutinee, arms)?;
                self.indent -= 1;
                self.write_indent();
                self.buf.push_str("})()");
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

    fn emit_match(&mut self, scrutinee: &AIRNode, arms: &[AIRNode]) -> Result<(), CodegenError> {
        let is_adt = arms.iter().any(|arm| {
            if let NodeKind::MatchArm { pattern, .. } = &arm.kind {
                matches!(pattern.kind, NodeKind::ConstructorPat { .. })
            } else {
                false
            }
        });

        if is_adt {
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}switch (");
            self.emit_expr(scrutinee)?;
            self.buf.push_str("._tag) {\n");
        } else {
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}switch (");
            self.emit_expr(scrutinee)?;
            self.buf.push_str(") {\n");
        }
        self.indent += 1;
        for arm in arms {
            self.emit_match_arm(arm, is_adt, scrutinee)?;
        }
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    fn emit_match_arm(
        &mut self,
        arm: &AIRNode,
        is_adt: bool,
        scrutinee: &AIRNode,
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
                    self.emit_expr(scrutinee)?;
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
                            self.emit_expr(scrutinee)?;
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
                                self.emit_expr(scrutinee)?;
                                let _ = writeln!(self.buf, ".{field_name};");
                            } else {
                                let ind = self.indent_str();
                                let _ = write!(self.buf, "{ind}const {field_name} = ");
                                self.emit_expr(scrutinee)?;
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
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}return ");
                self.emit_expr(t)?;
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
        let ctx = TsEmitCtx::new();
        let opt = node(
            1,
            NodeKind::TypeOptional {
                inner: Box::new(type_node(2, "String")),
            },
        );
        assert_eq!(ctx.type_to_ts(&opt), "string | null");
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
        assert!(out.contains("\"Ok\" as const"), "got: {out}");
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
            out.contains("interface StdLogger extends Logger {}"),
            "impl should emit interface extension for declaration merging, got: {out}"
        );
        assert!(
            out.contains("StdLogger.prototype.log"),
            "impl should attach method to prototype, got: {out}"
        );
    }
}
