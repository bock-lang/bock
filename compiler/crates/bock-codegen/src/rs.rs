//! Rust code generator — rule-based (Tier 2) transpilation from AIR to Rust.
//!
//! The most direct mapping of any target — Bock's ownership model was designed
//! to map cleanly to Rust:
//!
//! - Owned values → owned values (direct)
//! - Immutable borrow → `&T`
//! - Mutable borrow → `&mut T`
//! - Move → move semantics (direct)
//! - `@managed` → `Rc<T>` (single-threaded) / `Arc<T>` (concurrent)
//! - Records → structs
//! - Enums → enums (with variants)
//! - Traits → traits, Impls → impl blocks (nearly 1:1)
//! - Effects → `&dyn EffectTrait` parameters
//! - Pattern matching → native `match`
//! - Generics → monomorphized (preserved)
//! - String interpolation → `format!()` macro

use std::collections::HashMap;
use std::fmt::Write;
use std::path::PathBuf;

use bock_air::{AIRNode, AirInterpolationPart, EnumVariantPayload, NodeKind, ResultVariant};
use bock_ast::{AssignOp, BinOp, ImportItems, Literal, TypeExpr, UnaryOp, Visibility};
use bock_types::AIRModule;

use crate::error::CodegenError;
use crate::generator::{CodeGenerator, GeneratedCode, OutputFile, SourceMap};
use crate::profile::TargetProfile;

/// Conservative module scan for `Channel` / `spawn` references.
fn rs_module_uses_concurrency(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Channel\"") || s.contains("\"spawn\"")
    })
}

/// Runtime helpers for Bock concurrency in Rust. Backed by
/// `tokio::sync::mpsc::unbounded_channel`.
const CONCURRENCY_RUNTIME_RS: &str = "\
// ── Bock concurrency runtime ──
use std::sync::Arc;
pub struct __BockChannel<T> {
    tx: tokio::sync::mpsc::UnboundedSender<T>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<T>>,
}
pub fn __bock_channel_new<T: Send + 'static>() -> (Arc<__BockChannel<T>>, Arc<__BockChannel<T>>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let ch = Arc::new(__BockChannel { tx, rx: tokio::sync::Mutex::new(rx) });
    (ch.clone(), ch)
}
impl<T> __BockChannel<T> {
    pub fn send(&self, v: T) { let _ = self.tx.send(v); }
    pub async fn recv(&self) -> T {
        let mut guard = self.rx.lock().await;
        guard.recv().await.expect(\"channel closed\")
    }
    pub fn close(&self) {}
}
pub fn __bock_spawn<T: Send + 'static>(f: impl std::future::Future<Output = T> + Send + 'static) -> tokio::task::JoinHandle<T> {
    tokio::spawn(f)
}
";

/// Rust code generator implementing the `CodeGenerator` trait.
#[derive(Debug)]
pub struct RsGenerator {
    profile: TargetProfile,
}

impl RsGenerator {
    /// Creates a new Rust code generator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: TargetProfile::rust(),
        }
    }
}

impl Default for RsGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeGenerator for RsGenerator {
    fn target(&self) -> &TargetProfile {
        &self.profile
    }

    fn generate_module(&self, module: &AIRModule) -> Result<GeneratedCode, CodegenError> {
        let mut ctx = RsEmitCtx::new();
        ctx.emit_node(module)?;
        let content = ctx.finish();
        let source_map = SourceMap {
            generated_file: "output.rs".to_string(),
            ..Default::default()
        };
        Ok(GeneratedCode {
            files: vec![OutputFile {
                path: PathBuf::from("output.rs"),
                content,
            }],
            source_map: Some(source_map),
        })
    }
}

// ─── Emission context ────────────────────────────────────────────────────────

/// Internal state for Rust emission.
struct RsEmitCtx {
    buf: String,
    indent: usize,
    /// Track whether we need `use std::rc::Rc;` at the top.
    needs_rc_import: bool,
    /// Track whether we need `use std::sync::Arc;` at the top.
    needs_arc_import: bool,
    /// Names bound in the current block whose Call value is wrapped in
    /// `tokio::spawn(...)` because the binding is later `await`ed within the
    /// same block. Rust futures are lazy, so without this, sequential
    /// `.await` on each binding would serialise the work. See
    /// [`Self::collect_task_bindings`].
    task_bound_names: std::collections::HashSet<String>,
    /// Maps effect operation name → effect type name (e.g., "log" → "Logger").
    effect_ops: HashMap<String, String>,
    /// Maps effect type name → current handler variable name in scope.
    current_handler_vars: HashMap<String, String>,
    /// Maps function name → effect type names from its `with` clause.
    fn_effects: HashMap<String, Vec<String>>,
    /// Maps composite effect name → component effect names.
    composite_effects: HashMap<String, Vec<String>>,
}

impl RsEmitCtx {
    fn new() -> Self {
        Self {
            buf: String::with_capacity(4096),
            indent: 0,
            needs_rc_import: false,
            needs_arc_import: false,
            task_bound_names: std::collections::HashSet::new(),
            effect_ops: HashMap::new(),
            current_handler_vars: HashMap::new(),
            fn_effects: HashMap::new(),
            composite_effects: HashMap::new(),
        }
    }

    fn finish(mut self) -> String {
        if self.buf.is_empty() {
            return self.buf;
        }
        let mut prefix = String::from(
            "#![allow(unused_variables, unused_imports, unused_parens, dead_code, non_upper_case_globals)]\n\n",
        );
        if self.needs_rc_import {
            prefix.push_str("use std::rc::Rc;\n");
        }
        if self.needs_arc_import {
            prefix.push_str("use std::sync::Arc;\n");
        }
        if !prefix.ends_with("\n\n") {
            prefix.push('\n');
        }
        self.buf.insert_str(0, &prefix);
        self.buf
    }

    fn indent_str(&self) -> String {
        "    ".repeat(self.indent)
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
        self.emit_expr(node)?;
        let s = self.buf[start..].to_string();
        self.buf.truncate(start);
        Ok(s)
    }

    /// Map Bock prelude functions to Rust equivalents.
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
                format!("println!(\"{{}}\", {a})")
            }
            "print" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("print!(\"{{}}\", {a})")
            }
            "debug" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("dbg!(&{a})")
            }
            "assert" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("assert!({a})")
            }
            "todo" => "todo!()".to_string(),
            "unreachable" => "unreachable!()".to_string(),
            "sleep" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!(
                    "tokio::time::sleep(std::time::Duration::from_nanos(({a}) as u64))"
                )
            }
            _ => return Ok(None),
        };
        Ok(Some(code))
    }

    /// Recognise `Duration.xxx(...)` / `Instant.xxx(...)` associated-function
    /// calls and emit equivalent Rust `std::time` usage. Durations are i64
    /// nanoseconds; Instants are `std::time::Instant`.
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
            ("Duration", "zero") => "0i64".to_string(),
            ("Duration", "nanos") => format!("(({}) as i64)", arg0()),
            ("Duration", "micros") => format!("((({}) as i64) * 1_000)", arg0()),
            ("Duration", "millis") => format!("((({}) as i64) * 1_000_000)", arg0()),
            ("Duration", "seconds") => format!("((({}) as i64) * 1_000_000_000)", arg0()),
            ("Duration", "minutes") => format!("((({}) as i64) * 60_000_000_000)", arg0()),
            ("Duration", "hours") => format!("((({}) as i64) * 3_600_000_000_000)", arg0()),
            ("Instant", "now") => "std::time::Instant::now()".to_string(),
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Recognise desugared method calls on Duration/Instant values.
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

    /// Recognise `Channel.new()`, `spawn(...)`, and method calls on a
    /// channel value. Emits the Rust runtime helper equivalents using
    /// `tokio::sync::mpsc` under the hood.
    fn try_emit_concurrency_call(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        if let NodeKind::Identifier { name } = &callee.kind {
            if name.name == "spawn" {
                // spawn(x) — x is expected to be an async fn invocation
                // (a Future) in Bock. In Rust we wrap it in `async move`
                // so tokio::spawn can take ownership.
                self.buf.push_str("__bock_spawn(async move { ");
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(&arg.value)?;
                    self.buf.push_str(".await");
                }
                self.buf.push_str(" })");
                return Ok(true);
            }
        }
        let NodeKind::FieldAccess { object, field } = &callee.kind else {
            return Ok(false);
        };
        if let NodeKind::Identifier { name: type_name } = &object.kind {
            if type_name.name == "Channel" && field.name == "new" {
                self.buf.push_str("__bock_channel_new()");
                return Ok(true);
            }
        }
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

    /// Recognise instance methods on Duration/Instant values.
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
            "as_millis" => format!("(({recv_str}) / 1_000_000)"),
            "as_seconds" => format!("(({recv_str}) / 1_000_000_000)"),
            "is_zero" => format!("(({recv_str}) == 0)"),
            "is_negative" => format!("(({recv_str}) < 0)"),
            "abs" => format!("(({recv_str}) as i64).abs()"),
            "elapsed" => {
                format!("(({recv_str}).elapsed().as_nanos() as i64)")
            }
            "duration_since" => {
                let other = arg_strs.first().cloned().unwrap_or_default();
                format!(
                    "((({recv_str}).saturating_duration_since({other})).as_nanos() as i64)"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    // ── Type emission ────────────────────────────────────────────────────────

    /// Emit an AIR type node to a Rust type string.
    fn type_to_rs(&mut self, node: &AIRNode) -> String {
        match &node.kind {
            NodeKind::TypeNamed { path, args } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                let rs_name = self.map_type_name(&name);
                if args.is_empty() {
                    rs_name
                } else {
                    let arg_strs: Vec<String> = args.iter().map(|a| self.type_to_rs(a)).collect();
                    format!("{rs_name}<{}>", arg_strs.join(", "))
                }
            }
            NodeKind::TypeTuple { elems } => {
                let elem_strs: Vec<String> = elems.iter().map(|e| self.type_to_rs(e)).collect();
                format!("({})", elem_strs.join(", "))
            }
            NodeKind::TypeFunction { params, ret, .. } => {
                let param_strs: Vec<String> = params.iter().map(|p| self.type_to_rs(p)).collect();
                format!("fn({}) -> {}", param_strs.join(", "), self.type_to_rs(ret))
            }
            NodeKind::TypeOptional { inner } => {
                format!("Option<{}>", self.type_to_rs(inner))
            }
            NodeKind::TypeSelf => "Self".into(),
            _ => "_".into(),
        }
    }

    /// Map Bock type names to Rust equivalents.
    fn map_type_name(&mut self, name: &str) -> String {
        match name {
            "Int" => "i64".into(),
            "Float" => "f64".into(),
            "Bool" => "bool".into(),
            "String" => "String".into(),
            "Void" | "Unit" => "()".into(),
            "List" => "Vec".into(),
            "Map" => "std::collections::HashMap".into(),
            "Set" => "std::collections::HashSet".into(),
            "Any" => "Box<dyn std::any::Any>".into(),
            "Never" => "!".into(),
            "Optional" => "Option".into(),
            "Rc" => {
                self.needs_rc_import = true;
                "Rc".into()
            }
            "Arc" => {
                self.needs_arc_import = true;
                "Arc".into()
            }
            other => other.into(),
        }
    }

    /// Emit an AST TypeExpr to a Rust type string (for record fields, etc.).
    fn ast_type_to_rs(&mut self, ty: &TypeExpr) -> String {
        match ty {
            TypeExpr::Named { path, args, .. } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                let rs_name = self.map_type_name(&name);
                if args.is_empty() {
                    rs_name
                } else {
                    let arg_strs: Vec<String> =
                        args.iter().map(|a| self.ast_type_to_rs(a)).collect();
                    format!("{rs_name}<{}>", arg_strs.join(", "))
                }
            }
            TypeExpr::Tuple { elems, .. } => {
                let elem_strs: Vec<String> = elems.iter().map(|e| self.ast_type_to_rs(e)).collect();
                format!("({})", elem_strs.join(", "))
            }
            TypeExpr::Function { params, ret, .. } => {
                let param_strs: Vec<String> =
                    params.iter().map(|p| self.ast_type_to_rs(p)).collect();
                format!(
                    "fn({}) -> {}",
                    param_strs.join(", "),
                    self.ast_type_to_rs(ret)
                )
            }
            TypeExpr::Optional { inner, .. } => {
                format!("Option<{}>", self.ast_type_to_rs(inner))
            }
            TypeExpr::SelfType { .. } => "Self".into(),
        }
    }

    /// Emit generic parameter list: `<T, U: Foo>`.
    fn generic_params_to_rs(&self, params: &[bock_ast::GenericParam]) -> String {
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
                                .join("::")
                        })
                        .collect();
                    format!("{}: {}", p.name.name, bounds.join(" + "))
                }
            })
            .collect();
        format!("<{}>", items.join(", "))
    }

    /// Emit where clause: `where T: Foo, U: Bar`.
    fn where_clause_to_rs(&self, clauses: &[bock_ast::TypeConstraint]) -> String {
        if clauses.is_empty() {
            return String::new();
        }
        let items: Vec<String> = clauses
            .iter()
            .map(|c| {
                let bounds: Vec<String> = c
                    .bounds
                    .iter()
                    .map(|b| {
                        b.segments
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join("::")
                    })
                    .collect();
                format!("{}: {}", c.param.name, bounds.join(" + "))
            })
            .collect();
        format!("\nwhere\n    {}", items.join(",\n    "))
    }

    // ── Top-level dispatch ──────────────────────────────────────────────────

    fn emit_node(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::Module { imports, items, .. } => {
                if rs_module_uses_concurrency(items) {
                    self.buf.push_str(CONCURRENCY_RUNTIME_RS);
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
                    .join("::");
                match items {
                    ImportItems::Module => {
                        self.writeln(&format!("use {path_str};"));
                    }
                    ImportItems::Named(names) => {
                        let names_str = names
                            .iter()
                            .map(|n| n.name.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.writeln(&format!("use {path_str}::{{{names_str}}};"));
                    }
                    ImportItems::Glob => {
                        self.writeln(&format!("use {path_str}::*;"));
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
                where_clause,
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
                where_clause,
                body,
            ),
            NodeKind::RecordDecl {
                visibility,
                name,
                generic_params,
                fields,
                ..
            } => {
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                self.writeln(&format!("{vis}struct {}{generics} {{", name.name));
                self.indent += 1;
                for f in fields {
                    let ty = self.ast_type_to_rs(&f.ty);
                    self.writeln(&format!("pub {}: {ty},", to_snake_case(&f.name.name)));
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::EnumDecl {
                visibility,
                name,
                generic_params,
                variants,
                ..
            } => {
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                self.writeln(&format!("{vis}enum {}{generics} {{", name.name));
                self.indent += 1;
                for variant in variants {
                    self.emit_enum_variant(variant)?;
                }
                self.indent -= 1;
                self.writeln("}");
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
                // Rust has no classes; emit as struct + impl block.
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                self.writeln(&format!("{vis}struct {}{generics} {{", name.name));
                self.indent += 1;
                for f in fields {
                    let ty = self.ast_type_to_rs(&f.ty);
                    self.writeln(&format!("pub {}: {ty},", to_snake_case(&f.name.name)));
                }
                self.indent -= 1;
                self.writeln("}");
                self.buf.push('\n');
                // impl block for methods
                if !methods.is_empty() {
                    self.writeln(&format!("impl{generics} {}{generics} {{", name.name));
                    self.indent += 1;
                    for (i, method) in methods.iter().enumerate() {
                        if i > 0 {
                            self.buf.push('\n');
                        }
                        self.emit_method(method)?;
                    }
                    self.indent -= 1;
                    self.writeln("}");
                }
                Ok(())
            }
            NodeKind::TraitDecl {
                visibility,
                name,
                generic_params,
                methods,
                ..
            } => {
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                self.writeln(&format!("{vis}trait {}{generics} {{", name.name));
                self.indent += 1;
                for (i, method) in methods.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    self.emit_trait_method(method)?;
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::ImplBlock {
                generic_params,
                trait_path,
                target,
                where_clause,
                methods,
                ..
            } => {
                let generics = self.generic_params_to_rs(generic_params);
                let target_name = self.type_expr_to_string(target);
                let where_cl = self.where_clause_to_rs(where_clause);
                if let Some(tp) = trait_path {
                    let trait_name = tp
                        .segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("::");
                    self.writeln(&format!(
                        "impl{generics} {trait_name} for {target_name}{where_cl} {{"
                    ));
                } else {
                    self.writeln(&format!("impl{generics} {target_name}{where_cl} {{"));
                }
                let suppress_vis = trait_path.is_some();
                self.indent += 1;
                for (i, method) in methods.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    self.emit_method_inner(method, suppress_vis)?;
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::EffectDecl {
                visibility,
                name,
                components,
                generic_params,
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
                    self.composite_effects
                        .insert(name.name.clone(), comp_names);
                    return Ok(());
                }
                // Record effect operations for Call → handler.op rewriting.
                for op in operations {
                    if let NodeKind::FnDecl { name: op_name, .. } = &op.kind {
                        self.effect_ops
                            .insert(op_name.name.clone(), name.name.clone());
                    }
                }
                // Effects → Rust traits with `&dyn` usage.
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                self.writeln(&format!("{vis}trait {}{generics} {{", name.name));
                self.indent += 1;
                for (i, op) in operations.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    self.emit_trait_method(op)?;
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
                where_clause,
                ..
            } => {
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                let ty_str = self.type_to_rs(ty);
                let where_cl = self.where_clause_to_rs(where_clause);
                self.writeln(&format!(
                    "{vis}type {}{generics}{where_cl} = {ty_str};",
                    name.name
                ));
                Ok(())
            }
            NodeKind::ConstDecl {
                visibility,
                name,
                value,
                ty,
                ..
            } => {
                let vis = vis_str(*visibility);
                let ty_str = self.type_to_rs(ty);
                let ind = self.indent_str();
                let _ = write!(
                    self.buf,
                    "{ind}{vis}const {}: {ty_str} = ",
                    to_upper_snake_case(&name.name)
                );
                self.emit_expr(value)?;
                self.buf.push_str(";\n");
                Ok(())
            }
            NodeKind::ModuleHandle { effect, handler } => {
                // Module-level `handle` becomes a `const` whose type is the
                // concrete handler struct. Referring to `&CONST` in call
                // positions produces a valid `&impl Trait` borrow without
                // the `Sync`/`'static` requirements that `static &dyn Trait`
                // would impose. The handler is registered as the default
                // for this effect, so subsequent effectful calls pass it
                // implicitly unless a local handling block overrides it.
                let effect_name =
                    effect.segments.last().map_or("effect", |s| s.name.as_str());
                let const_name =
                    format!("__{}_HANDLER", to_snake_case(effect_name).to_uppercase());
                let handler_type = record_construct_type(handler);
                let ind = self.indent_str();
                if let Some(type_name) = handler_type {
                    let _ = write!(self.buf, "{ind}const {const_name}: {type_name} = ");
                    self.emit_expr(handler)?;
                    self.buf.push_str(";\n");
                    self.current_handler_vars
                        .insert(effect_name.to_string(), const_name);
                } else {
                    // Fallback for non-literal handlers: emit a comment so the
                    // output is still valid Rust but the handler must be
                    // provided at every call site.
                    let _ = write!(self.buf, "{ind}// module handle: {effect_name} with ");
                    self.emit_expr(handler)?;
                    self.buf.push('\n');
                }
                Ok(())
            }
            NodeKind::PropertyTest { name, .. } => {
                self.writeln(&format!("// property test: {name}"));
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
        where_clause: &[bock_ast::TypeConstraint],
        body: &AIRNode,
    ) -> Result<(), CodegenError> {
        let vis = vis_str(visibility);
        let async_kw = if is_async { "async " } else { "" };
        let generics = self.generic_params_to_rs(generic_params);
        let param_strs = self.collect_param_strs(params);
        let effects = self.effects_params(effect_clause);
        let mut all_params = param_strs;
        all_params.extend(effects);
        let ret = return_type
            .map(|t| format!(" -> {}", self.type_to_rs(t)))
            .unwrap_or_default();
        let where_cl = self.where_clause_to_rs(where_clause);
        if !effect_clause.is_empty() {
            let effect_names = self.expand_effect_names(effect_clause);
            self.fn_effects.insert(name.to_string(), effect_names);
        }
        let fn_name = to_snake_case(name);
        // `async fn main` needs a runtime attribute — tokio drives the future
        // to completion on a multi-threaded executor, matching the Bock
        // interpreter's async runtime model.
        if is_async && fn_name == "main" {
            self.writeln("#[tokio::main]");
        }
        self.writeln(&format!(
            "{vis}{async_kw}fn {fn_name}{generics}({}){ret}{where_cl} {{",
            all_params.join(", "),
        ));
        self.indent += 1;
        let old_handler_vars = self.current_handler_vars.clone();
        let expanded = self.expand_effect_names(effect_clause);
        for ename in &expanded {
            self.current_handler_vars
                .insert(ename.clone(), to_snake_case(ename));
        }
        self.emit_block_body(body)?;
        self.current_handler_vars = old_handler_vars;
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    /// Emit a method inside an impl block (with `&self` / `&mut self`).
    /// If `suppress_vis` is true, visibility qualifiers are omitted (e.g. trait impl methods).
    fn emit_method_inner(
        &mut self,
        method: &AIRNode,
        suppress_vis: bool,
    ) -> Result<(), CodegenError> {
        if let NodeKind::FnDecl {
            visibility,
            is_async,
            name,
            generic_params,
            params,
            return_type,
            effect_clause,
            where_clause,
            body,
            ..
        } = &method.kind
        {
            let vis = if suppress_vis {
                ""
            } else {
                vis_str(*visibility)
            };
            let async_kw = if *is_async { "async " } else { "" };
            let generics = self.generic_params_to_rs(generic_params);
            let param_strs = self.collect_param_strs(params);
            let effects = self.effects_params(effect_clause);
            let mut all_params = vec!["&self".to_string()];
            all_params.extend(param_strs);
            all_params.extend(effects);
            let ret = return_type
                .as_deref()
                .map(|t| format!(" -> {}", self.type_to_rs(t)))
                .unwrap_or_default();
            let where_cl = self.where_clause_to_rs(where_clause);
            let fn_name = to_snake_case(&name.name);
            self.writeln(&format!(
                "{vis}{async_kw}fn {fn_name}{generics}({}){ret}{where_cl} {{",
                all_params.join(", "),
            ));
            self.indent += 1;
            let old_handler_vars = self.current_handler_vars.clone();
            let expanded = self.expand_effect_names(effect_clause);
            for ename in &expanded {
                self.current_handler_vars
                    .insert(ename.clone(), to_snake_case(ename));
            }
            self.emit_block_body(body)?;
            self.current_handler_vars = old_handler_vars;
            self.indent -= 1;
            self.writeln("}");
        }
        Ok(())
    }

    /// Emit a method with visibility preserved.
    fn emit_method(&mut self, method: &AIRNode) -> Result<(), CodegenError> {
        self.emit_method_inner(method, false)
    }

    /// Emit a trait method signature (may or may not have a body).
    fn emit_trait_method(&mut self, method: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::FnDecl {
            is_async,
            name,
            generic_params,
            params,
            return_type,
            effect_clause,
            where_clause,
            body,
            ..
        } = &method.kind
        {
            let async_kw = if *is_async { "async " } else { "" };
            let generics = self.generic_params_to_rs(generic_params);
            let param_strs = self.collect_param_strs(params);
            let effects = self.effects_params(effect_clause);
            let mut all_params = vec!["&self".to_string()];
            all_params.extend(param_strs);
            all_params.extend(effects);
            let ret = return_type
                .as_deref()
                .map(|t| format!(" -> {}", self.type_to_rs(t)))
                .unwrap_or_default();
            let where_cl = self.where_clause_to_rs(where_clause);
            let fn_name = to_snake_case(&name.name);

            // Check if body is an empty block (trait method signature without default impl).
            let has_body = if let NodeKind::Block { stmts, tail } = &body.kind {
                !stmts.is_empty() || tail.is_some()
            } else {
                true
            };

            if has_body {
                self.writeln(&format!(
                    "{async_kw}fn {fn_name}{generics}({}){ret}{where_cl} {{",
                    all_params.join(", "),
                ));
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
            } else {
                self.writeln(&format!(
                    "{async_kw}fn {fn_name}{generics}({}){ret}{where_cl};",
                    all_params.join(", "),
                ));
            }
        }
        Ok(())
    }

    fn collect_param_strs(&mut self, params: &[AIRNode]) -> Vec<String> {
        let mut result = Vec::new();
        for p in params {
            if let NodeKind::Param {
                pattern,
                ty,
                default,
            } = &p.kind
            {
                let name = to_snake_case(&self.pattern_to_binding_name(pattern));
                let type_ann = ty
                    .as_ref()
                    .map(|t| format!(": {}", self.type_to_rs(t)))
                    .unwrap_or_else(|| ": _".into());
                if let Some(def) = default {
                    // Rust doesn't have default params; emit a comment.
                    let mut ctx = RsEmitCtx::new();
                    ctx.indent = self.indent;
                    if ctx.emit_expr(def).is_ok() {
                        let def_str = ctx.buf;
                        result.push(format!("{name}{type_ann} /* = {def_str} */"));
                        continue;
                    }
                }
                result.push(format!("{name}{type_ann}"));
            }
        }
        result
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

    /// Effects → `&impl EffectTrait` parameters (argument-position impl trait).
    /// This gives each effectful function a fresh generic parameter per effect,
    /// so handlers can be any concrete type implementing the effect trait while
    /// keeping the ownership story simple: the caller retains ownership and the
    /// function borrows for its body.
    fn effects_params(&self, effects: &[bock_ast::TypePath]) -> Vec<String> {
        let expanded = self.expand_effect_names(effects);
        expanded
            .iter()
            .map(|name| {
                let param_name = to_snake_case(name);
                format!("{param_name}: &impl {name}")
            })
            .collect()
    }

    /// Build `&handler_var, ...` arguments for calling an effectful function.
    fn build_effects_call_args_rs(&self, fn_name: &str) -> Option<String> {
        let effects = self.fn_effects.get(fn_name)?;
        let entries: Vec<String> = effects
            .iter()
            .filter_map(|e| {
                let handler_var = self.current_handler_vars.get(e)?;
                Some(format!("&{handler_var}"))
            })
            .collect();
        if entries.is_empty() {
            return None;
        }
        Some(entries.join(", "))
    }

    // ── Enum variant ────────────────────────────────────────────────────────

    fn emit_enum_variant(&mut self, variant: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::EnumVariant { name, payload } = &variant.kind {
            let vname = &name.name;
            match payload {
                EnumVariantPayload::Unit => {
                    self.writeln(&format!("{vname},"));
                }
                EnumVariantPayload::Struct(fields) => {
                    self.writeln(&format!("{vname} {{"));
                    self.indent += 1;
                    for f in fields {
                        let ty = self.ast_type_to_rs(&f.ty);
                        self.writeln(&format!("{}: {ty},", to_snake_case(&f.name.name)));
                    }
                    self.indent -= 1;
                    self.writeln("},");
                }
                EnumVariantPayload::Tuple(elems) => {
                    let types: Vec<String> = elems.iter().map(|e| self.type_to_rs(e)).collect();
                    self.writeln(&format!("{vname}({}),", types.join(", ")));
                }
            }
        }
        Ok(())
    }

    // ── Statements ──────────────────────────────────────────────────────────

    fn emit_stmt(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::LetBinding {
                pattern,
                value,
                ty,
                is_mut,
            } => {
                let binding = self.pattern_to_rs_binding(pattern);
                let type_ann = ty
                    .as_ref()
                    .map(|t| format!(": {}", self.type_to_rs(t)))
                    .unwrap_or_default();
                let mut_kw = if *is_mut { "mut " } else { "" };
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}let {mut_kw}{binding}{type_ann} = ");
                let wrap_task = matches!(&value.kind, NodeKind::Call { .. })
                    && self.task_bound_names.contains(&binding);
                if wrap_task {
                    self.buf.push_str("tokio::spawn(");
                    self.emit_expr(value)?;
                    self.buf.push(')');
                } else {
                    self.emit_expr(value)?;
                }
                self.buf.push_str(";\n");
                Ok(())
            }
            NodeKind::If {
                let_pattern,
                condition,
                then_block,
                else_block,
            } => {
                let ind = self.indent_str();
                if let Some(pat) = let_pattern {
                    let binding = self.pattern_to_rs_binding(pat);
                    let _ = write!(self.buf, "{ind}if let Some({binding}) = ");
                    self.emit_expr(condition)?;
                    self.buf.push_str(" {\n");
                } else {
                    let _ = write!(self.buf, "{ind}if ");
                    self.emit_expr(condition)?;
                    self.buf.push_str(" {\n");
                }
                self.indent += 1;
                self.emit_block_body(then_block)?;
                self.indent -= 1;
                if let Some(else_b) = else_block {
                    if matches!(else_b.kind, NodeKind::If { .. }) {
                        let ind = self.indent_str();
                        let _ = write!(self.buf, "{ind}}} else ");
                        // Remove leading indent from recursive call
                        self.emit_if_continuing(else_b)?;
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
                let binding = self.pattern_to_rs_binding(pattern);
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}for {binding} in ");
                self.emit_expr(iterable)?;
                self.buf.push_str(" {\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::While { condition, body } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}while ");
                self.emit_expr(condition)?;
                self.buf.push_str(" {\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::Loop { body } => {
                self.writeln("loop {");
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
                    let _ = write!(self.buf, "{ind}break ");
                    self.emit_expr(val)?;
                    self.buf.push_str(";\n");
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
                let _ = write!(self.buf, "{ind}if !(");
                self.emit_expr(condition)?;
                self.buf.push_str(") {\n");
                self.indent += 1;
                self.emit_block_body(else_block)?;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => self.emit_match(scrutinee, arms),
            NodeKind::Block { stmts, tail } => {
                for s in stmts {
                    self.emit_node(s)?;
                }
                if let Some(t) = tail {
                    self.write_indent();
                    self.emit_expr(t)?;
                    self.buf.push('\n');
                }
                Ok(())
            }
            NodeKind::HandlingBlock { handlers, body } => {
                // handling block → scoped handler instantiation
                self.writeln("{");
                self.indent += 1;
                let old_handler_vars = self.current_handler_vars.clone();
                for h in handlers {
                    let effect_name =
                        h.effect.segments.last().map_or("effect", |s| s.name.as_str());
                    let var_name = format!("__{}", to_snake_case(effect_name));
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}let {var_name} = ");
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
                        self.buf.push('\n');
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
                    AssignOp::Assign => " = ",
                    AssignOp::AddAssign => " += ",
                    AssignOp::SubAssign => " -= ",
                    AssignOp::MulAssign => " *= ",
                    AssignOp::DivAssign => " /= ",
                    AssignOp::RemAssign => " %= ",
                };
                self.buf.push_str(op_str);
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

    /// Helper for chained if/else if without extra indent.
    fn emit_if_continuing(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::If {
            let_pattern,
            condition,
            then_block,
            else_block,
        } = &node.kind
        {
            if let Some(pat) = let_pattern {
                let binding = self.pattern_to_rs_binding(pat);
                let _ = write!(self.buf, "if let Some({binding}) = ");
                self.emit_expr(condition)?;
                self.buf.push_str(" {\n");
            } else {
                let _ = write!(self.buf, "if ");
                self.emit_expr(condition)?;
                self.buf.push_str(" {\n");
            }
            self.indent += 1;
            self.emit_block_body(then_block)?;
            self.indent -= 1;
            if let Some(else_b) = else_block {
                if matches!(else_b.kind, NodeKind::If { .. }) {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}}} else ");
                    self.emit_if_continuing(else_b)?;
                    return Ok(());
                }
                self.writeln("} else {");
                self.indent += 1;
                self.emit_block_body(else_b)?;
                self.indent -= 1;
            }
            self.writeln("}");
        }
        Ok(())
    }

    // ── Expressions ─────────────────────────────────────────────────────────

    fn emit_expr(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::Literal { lit } => {
                match lit {
                    Literal::Int(s) => {
                        self.buf.push_str(s);
                        self.buf.push_str("_i64");
                    }
                    Literal::Float(s) => {
                        self.buf.push_str(s);
                        self.buf.push_str("_f64");
                    }
                    Literal::Bool(b) => {
                        self.buf.push_str(if *b { "true" } else { "false" });
                    }
                    Literal::Char(s) => {
                        self.buf.push('\'');
                        self.buf.push_str(s);
                        self.buf.push('\'');
                    }
                    Literal::String(s) => {
                        self.buf.push('"');
                        self.buf.push_str(&escape_rs_string(s));
                        self.buf.push('"');
                        self.buf.push_str(".to_string()");
                    }
                    Literal::Unit => self.buf.push_str("()"),
                }
                Ok(())
            }
            NodeKind::Identifier { name } => {
                self.buf.push_str(&identifier_to_rs(&name.name));
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
                    BinOp::Pow => ".pow(",
                    BinOp::Eq => " == ",
                    BinOp::Ne => " != ",
                    BinOp::Lt => " < ",
                    BinOp::Le => " <= ",
                    BinOp::Gt => " > ",
                    BinOp::Ge => " >= ",
                    BinOp::And => " && ",
                    BinOp::Or => " || ",
                    BinOp::BitAnd => " & ",
                    BinOp::BitOr => " | ",
                    BinOp::BitXor => " ^ ",
                    BinOp::Compose => " /* compose */ ",
                    BinOp::Is => " /* is */ ",
                };
                self.buf.push_str(op_str);
                if *op == BinOp::Pow {
                    self.emit_expr(right)?;
                    self.buf.push(')');
                } else {
                    self.emit_expr(right)?;
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::UnaryOp { op, operand } => {
                let op_str = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                    UnaryOp::BitNot => "!",
                };
                self.buf.push_str(op_str);
                self.emit_expr(operand)?;
                Ok(())
            }
            NodeKind::Call { callee, args, .. } => {
                // Rewrite bare effect operation calls: log(...) → handler.log(...)
                if let NodeKind::Identifier { name } = &callee.kind {
                    if let Some(effect_name) = self.effect_ops.get(&name.name).cloned() {
                        if let Some(handler_var) =
                            self.current_handler_vars.get(&effect_name).cloned()
                        {
                            let _ = write!(
                                self.buf,
                                "{}.{}",
                                handler_var,
                                to_snake_case(&name.name)
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
                if let Some(code) = self.map_prelude_call(callee, args)? {
                    self.buf.push_str(&code);
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
                // Pass handler args to effectful function calls.
                let effects_args = if let NodeKind::Identifier { name } = &callee.kind {
                    self.build_effects_call_args_rs(&name.name)
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
                if let Some(ea) = effects_args {
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
                let _ = write!(self.buf, ".{}", to_snake_case(&method.name));
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
                let _ = write!(self.buf, ".{}", to_snake_case(&field.name));
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
                let param_strs = self.collect_param_strs(params);
                let _ = write!(self.buf, "|{}| ", param_strs.join(", "));
                self.emit_expr(body)?;
                Ok(())
            }
            NodeKind::Pipe { left, right } => self.emit_pipe(left, right),
            NodeKind::Compose { left, right } => {
                // `f >> g` → `|x| g(f(x))`
                let _ = write!(self.buf, "|x| ");
                self.emit_expr(right)?;
                self.buf.push('(');
                self.emit_expr(left)?;
                self.buf.push_str("(x))");
                Ok(())
            }
            NodeKind::Await { expr } => {
                // `await x` where `x` was spawned above becomes
                // `x.await.unwrap()` — `tokio::spawn` returns a `JoinHandle<T>`
                // whose `.await` yields `Result<T, JoinError>`, so we unwrap
                // to restore the original `T` type the user wrote.
                let is_spawned_handle = if let NodeKind::Identifier { name } = &expr.kind {
                    self.task_bound_names.contains(&to_snake_case(&name.name))
                } else {
                    false
                };
                self.emit_expr(expr)?;
                if is_spawned_handle {
                    self.buf.push_str(".await.unwrap()");
                } else {
                    self.buf.push_str(".await");
                }
                Ok(())
            }
            NodeKind::Propagate { expr } => {
                self.emit_expr(expr)?;
                self.buf.push('?');
                Ok(())
            }
            NodeKind::Range { lo, hi, inclusive } => {
                self.emit_expr(lo)?;
                if *inclusive {
                    self.buf.push_str("..=");
                } else {
                    self.buf.push_str("..");
                }
                self.emit_expr(hi)?;
                Ok(())
            }
            NodeKind::RecordConstruct {
                path,
                fields,
                spread,
            } => {
                let type_name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                self.buf.push_str(&type_name);
                self.buf.push_str(" { ");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    let fname = to_snake_case(&f.name.name);
                    if let Some(val) = &f.value {
                        let _ = write!(self.buf, "{fname}: ");
                        self.emit_expr(val)?;
                    } else {
                        self.buf.push_str(&fname);
                    }
                }
                if let Some(sp) = spread {
                    if !fields.is_empty() {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str("..");
                    self.emit_expr(sp)?;
                }
                self.buf.push_str(" }");
                Ok(())
            }
            NodeKind::ListLiteral { elems } => {
                self.buf.push_str("vec![");
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
                if entries.is_empty() {
                    self.buf.push_str("std::collections::HashMap::new()");
                } else {
                    self.buf.push_str("std::collections::HashMap::from([");
                    for (i, entry) in entries.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.buf.push('(');
                        self.emit_expr(&entry.key)?;
                        self.buf.push_str(", ");
                        self.emit_expr(&entry.value)?;
                        self.buf.push(')');
                    }
                    self.buf.push_str("])");
                }
                Ok(())
            }
            NodeKind::SetLiteral { elems } => {
                if elems.is_empty() {
                    self.buf.push_str("std::collections::HashSet::new()");
                } else {
                    self.buf.push_str("std::collections::HashSet::from([");
                    for (i, e) in elems.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.emit_expr(e)?;
                    }
                    self.buf.push_str("])");
                }
                Ok(())
            }
            NodeKind::TupleLiteral { elems } => {
                self.buf.push('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(e)?;
                }
                if elems.len() == 1 {
                    self.buf.push(',');
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::Interpolation { parts } => {
                // `format!("...", args)`
                self.buf.push_str("format!(\"");
                let mut format_args: Vec<String> = Vec::new();
                for part in parts {
                    match part {
                        AirInterpolationPart::Literal(s) => {
                            self.buf.push_str(&escape_format_string(s));
                        }
                        AirInterpolationPart::Expr(expr) => {
                            self.buf.push_str("{}");
                            let mut sub = RsEmitCtx::new();
                            sub.indent = self.indent;
                            sub.emit_expr(expr)?;
                            format_args.push(sub.buf);
                        }
                    }
                }
                self.buf.push('"');
                for arg in format_args {
                    self.buf.push_str(", ");
                    self.buf.push_str(&arg);
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::Placeholder => {
                self.buf.push('_');
                Ok(())
            }
            NodeKind::Unreachable => {
                self.buf.push_str("unreachable!()");
                Ok(())
            }
            NodeKind::ResultConstruct { variant, value } => {
                match variant {
                    ResultVariant::Ok => {
                        self.buf.push_str("Ok(");
                        if let Some(v) = value {
                            self.emit_expr(v)?;
                        } else {
                            self.buf.push_str("()");
                        }
                        self.buf.push(')');
                    }
                    ResultVariant::Err => {
                        self.buf.push_str("Err(");
                        if let Some(v) = value {
                            self.emit_expr(v)?;
                        } else {
                            self.buf.push_str("()");
                        }
                        self.buf.push(')');
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
                // If in expression position.
                self.buf.push_str("if ");
                self.emit_expr(condition)?;
                self.buf.push_str(" { ");
                self.emit_block_as_expr(then_block)?;
                self.buf.push_str(" } else { ");
                if let Some(eb) = else_block {
                    self.emit_block_as_expr(eb)?;
                } else {
                    self.buf.push_str("()");
                }
                self.buf.push_str(" }");
                Ok(())
            }
            NodeKind::Block { stmts, tail } => {
                if stmts.is_empty() {
                    if let Some(t) = tail {
                        return self.emit_expr(t);
                    }
                }
                // Block in expression position: `{ stmts; tail }`
                self.buf.push_str("{\n");
                self.indent += 1;
                for s in stmts {
                    self.emit_node(s)?;
                }
                if let Some(t) = tail {
                    self.write_indent();
                    self.emit_expr(t)?;
                    self.buf.push('\n');
                }
                self.indent -= 1;
                self.write_indent();
                self.buf.push('}');
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => {
                // Match in expression position.
                self.buf.push_str("match ");
                self.emit_expr(scrutinee)?;
                self.buf.push_str(" {\n");
                self.indent += 1;
                for arm in arms {
                    self.emit_match_arm(arm)?;
                }
                self.indent -= 1;
                self.write_indent();
                self.buf.push('}');
                Ok(())
            }
            // Ownership nodes: direct mapping to Rust.
            NodeKind::Move { expr } => {
                // Move semantics are default in Rust, just emit the expression.
                self.emit_expr(expr)
            }
            NodeKind::Borrow { expr } => {
                self.buf.push('&');
                self.emit_expr(expr)?;
                Ok(())
            }
            NodeKind::MutableBorrow { expr } => {
                self.buf.push_str("&mut ");
                self.emit_expr(expr)?;
                Ok(())
            }
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
                    to_snake_case(effect_name),
                    to_snake_case(&operation.name)
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
            // Type expressions in expression context.
            NodeKind::TypeNamed { .. }
            | NodeKind::TypeTuple { .. }
            | NodeKind::TypeFunction { .. }
            | NodeKind::TypeOptional { .. }
            | NodeKind::TypeSelf => {
                self.buf.push_str("/* type */");
                Ok(())
            }
            NodeKind::EffectRef { path } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
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

    // ── Match ───────────────────────────────────────────────────────────────

    fn emit_match(&mut self, scrutinee: &AIRNode, arms: &[AIRNode]) -> Result<(), CodegenError> {
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}match ");
        self.emit_expr(scrutinee)?;
        self.buf.push_str(" {\n");
        self.indent += 1;
        for arm in arms {
            self.emit_match_arm(arm)?;
        }
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    fn emit_match_arm(&mut self, arm: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::MatchArm {
            pattern,
            guard,
            body,
        } = &arm.kind
        {
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}");
            self.emit_pattern(pattern)?;
            if let Some(g) = guard {
                self.buf.push_str(" if ");
                self.emit_expr(g)?;
            }
            self.buf.push_str(" => ");
            // Single-expression body → inline; otherwise block.
            if let NodeKind::Block { stmts, tail } = &body.kind {
                if stmts.is_empty() {
                    if let Some(t) = tail {
                        self.emit_expr(t)?;
                        self.buf.push_str(",\n");
                        return Ok(());
                    }
                }
                self.buf.push_str("{\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
            } else {
                self.emit_expr(body)?;
                self.buf.push_str(",\n");
            }
        }
        Ok(())
    }

    fn emit_pattern(&mut self, pat: &AIRNode) -> Result<(), CodegenError> {
        match &pat.kind {
            NodeKind::WildcardPat => {
                self.buf.push('_');
            }
            NodeKind::BindPat { name, is_mut } => {
                if *is_mut {
                    self.buf.push_str("mut ");
                }
                self.buf.push_str(&to_snake_case(&name.name));
            }
            NodeKind::LiteralPat { lit } => match lit {
                Literal::Int(s) => {
                    self.buf.push_str(s);
                    self.buf.push_str("_i64");
                }
                Literal::Float(s) => self.buf.push_str(s),
                Literal::Bool(b) => self.buf.push_str(if *b { "true" } else { "false" }),
                Literal::Char(s) => {
                    self.buf.push('\'');
                    self.buf.push_str(s);
                    self.buf.push('\'');
                }
                Literal::String(s) => {
                    self.buf.push('"');
                    self.buf.push_str(&escape_rs_string(s));
                    self.buf.push('"');
                }
                Literal::Unit => self.buf.push_str("()"),
            },
            NodeKind::ConstructorPat { path, fields } => {
                let variant_name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                if fields.is_empty() {
                    self.buf.push_str(&variant_name);
                } else {
                    let _ = write!(self.buf, "{variant_name}(");
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.emit_pattern(f)?;
                    }
                    self.buf.push(')');
                }
            }
            NodeKind::RecordPat { path, fields, rest } => {
                let type_name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                let _ = write!(self.buf, "{type_name} {{ ");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    let field_name = to_snake_case(&f.name.name);
                    if let Some(pat) = &f.pattern {
                        let _ = write!(self.buf, "{field_name}: ");
                        self.emit_pattern(pat)?;
                    } else {
                        self.buf.push_str(&field_name);
                    }
                }
                if *rest {
                    if !fields.is_empty() {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str("..");
                }
                self.buf.push_str(" }");
            }
            NodeKind::TuplePat { elems } => {
                self.buf.push('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_pattern(e)?;
                }
                self.buf.push(')');
            }
            NodeKind::ListPat { elems, rest } => {
                self.buf.push('[');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_pattern(e)?;
                }
                if let Some(r) = rest {
                    if !elems.is_empty() {
                        self.buf.push_str(", ");
                    }
                    self.emit_pattern(r)?;
                    self.buf.push_str(" @ ..");
                }
                self.buf.push(']');
            }
            NodeKind::OrPat { alternatives } => {
                for (i, p) in alternatives.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(" | ");
                    }
                    self.emit_pattern(p)?;
                }
            }
            NodeKind::GuardPat { pattern, guard } => {
                self.emit_pattern(pattern)?;
                self.buf.push_str(" if ");
                self.emit_expr(guard)?;
            }
            NodeKind::RangePat { lo, hi, inclusive } => {
                self.emit_pattern(lo)?;
                if *inclusive {
                    self.buf.push_str("..=");
                } else {
                    self.buf.push_str("..");
                }
                self.emit_pattern(hi)?;
            }
            NodeKind::RestPat => {
                self.buf.push_str("..");
            }
            _ => {
                self.buf.push('_');
            }
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
            if stmts.is_empty() && tail.is_none() {
                // Empty block body.
                return Ok(());
            }
            // Concurrent-pattern detection: names bound in this block whose
            // Call RHS should be scheduled via `tokio::spawn` because the
            // same name is later `await`ed in the same block. Rust futures
            // are lazy, so without spawning, sequential `.await` calls on
            // each binding would serialise the work.
            let task_bindings = Self::collect_task_bindings(stmts);
            let prev = std::mem::replace(&mut self.task_bound_names, task_bindings);
            for s in stmts {
                self.emit_node(s)?;
            }
            self.task_bound_names = prev;
            if let Some(t) = tail {
                // Tail expression without semicolon (Rust implicit return).
                self.write_indent();
                self.emit_expr(t)?;
                self.buf.push('\n');
            }
        } else {
            // Single expression as body (implicit return).
            self.write_indent();
            self.emit_expr(node)?;
            self.buf.push('\n');
        }
        Ok(())
    }

    /// Scan a sequence of block statements and return the set of bound names
    /// that are later `await`ed as bare identifiers within the same block.
    /// The caller wraps those LetBindings' Call values in `tokio::spawn`.
    ///
    /// Only direct `let name = call(...)` bindings qualify. Non-call RHS are
    /// skipped (nothing to spawn). The binding must be awaited in the same
    /// flat block — nested scopes are ignored because we can't prove the
    /// binding is still live once control leaves the block.
    fn collect_task_bindings(
        stmts: &[AIRNode],
    ) -> std::collections::HashSet<String> {
        let mut awaited: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for s in stmts {
            Self::collect_awaited_identifiers(s, &mut awaited);
        }
        let mut out = std::collections::HashSet::new();
        for s in stmts {
            if let NodeKind::LetBinding { pattern, value, .. } = &s.kind {
                if let NodeKind::BindPat { name, .. } = &pattern.kind {
                    let rs_name = to_snake_case(&name.name);
                    if matches!(&value.kind, NodeKind::Call { .. })
                        && awaited.contains(&rs_name)
                    {
                        out.insert(rs_name);
                    }
                }
            }
        }
        out
    }

    /// Walk an AIR subtree and record every `await name` where `name` is a
    /// bare identifier. Nested function / lambda bodies are not descended —
    /// an inner closure awaiting the name doesn't imply the outer block
    /// wants a task.
    fn collect_awaited_identifiers(
        node: &AIRNode,
        out: &mut std::collections::HashSet<String>,
    ) {
        match &node.kind {
            NodeKind::Await { expr } => {
                if let NodeKind::Identifier { name } = &expr.kind {
                    out.insert(to_snake_case(&name.name));
                }
                Self::collect_awaited_identifiers(expr, out);
            }
            NodeKind::Lambda { .. } | NodeKind::FnDecl { .. } => {
                // Don't cross function boundaries.
            }
            NodeKind::Block { stmts, tail } => {
                for s in stmts {
                    Self::collect_awaited_identifiers(s, out);
                }
                if let Some(t) = tail {
                    Self::collect_awaited_identifiers(t, out);
                }
            }
            NodeKind::LetBinding { value, .. } => {
                Self::collect_awaited_identifiers(value, out);
            }
            NodeKind::Call { callee, args, .. } => {
                Self::collect_awaited_identifiers(callee, out);
                for a in args {
                    Self::collect_awaited_identifiers(&a.value, out);
                }
            }
            NodeKind::MethodCall { receiver, args, .. } => {
                Self::collect_awaited_identifiers(receiver, out);
                for a in args {
                    Self::collect_awaited_identifiers(&a.value, out);
                }
            }
            NodeKind::BinaryOp { left, right, .. } => {
                Self::collect_awaited_identifiers(left, out);
                Self::collect_awaited_identifiers(right, out);
            }
            NodeKind::UnaryOp { operand, .. } => {
                Self::collect_awaited_identifiers(operand, out);
            }
            NodeKind::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                Self::collect_awaited_identifiers(condition, out);
                Self::collect_awaited_identifiers(then_block, out);
                if let Some(e) = else_block {
                    Self::collect_awaited_identifiers(e, out);
                }
            }
            NodeKind::While { condition, body } => {
                Self::collect_awaited_identifiers(condition, out);
                Self::collect_awaited_identifiers(body, out);
            }
            NodeKind::For { iterable, body, .. } => {
                Self::collect_awaited_identifiers(iterable, out);
                Self::collect_awaited_identifiers(body, out);
            }
            NodeKind::Return { value: Some(v) } | NodeKind::Break { value: Some(v) } => {
                Self::collect_awaited_identifiers(v, out);
            }
            NodeKind::Assign { value, .. } => {
                Self::collect_awaited_identifiers(value, out);
            }
            NodeKind::TupleLiteral { elems }
            | NodeKind::ListLiteral { elems } => {
                for e in elems {
                    Self::collect_awaited_identifiers(e, out);
                }
            }
            _ => {}
        }
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
            NodeKind::BindPat { name, .. } => to_snake_case(&name.name),
            NodeKind::WildcardPat => "_".into(),
            NodeKind::TuplePat { elems } => {
                format!(
                    "({})",
                    elems
                        .iter()
                        .map(|e| self.pattern_to_binding_name(e))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            _ => "_".into(),
        }
    }

    fn pattern_to_rs_binding(&self, pat: &AIRNode) -> String {
        self.pattern_to_binding_name(pat)
    }

    fn type_expr_to_string(&mut self, node: &AIRNode) -> String {
        match &node.kind {
            NodeKind::TypeNamed { path, args } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                if args.is_empty() {
                    name
                } else {
                    let arg_strs: Vec<String> = args.iter().map(|a| self.type_to_rs(a)).collect();
                    format!("{name}<{}>", arg_strs.join(", "))
                }
            }
            NodeKind::Identifier { name } => name.name.clone(),
            _ => "Unknown".into(),
        }
    }
}

// ─── Utility functions ───────────────────────────────────────────────────────

/// Visibility keyword.
fn vis_str(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "pub ",
        Visibility::Private => "",
        Visibility::Internal => "pub(crate) ",
    }
}

/// If `node` is a record construction, return the fully-qualified type path
/// used in the constructor. Used by module-level `handle` emission to pick a
/// concrete type annotation for the synthesised `const`.
fn record_construct_type(node: &AIRNode) -> Option<String> {
    if let NodeKind::RecordConstruct { path, .. } = &node.kind {
        let joined = path
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::");
        Some(joined)
    } else {
        None
    }
}

/// Emit a Bock identifier as a Rust identifier — PascalCase names are
/// preserved verbatim (they are types, enum variants, or tuple-struct
/// constructors), while everything else is converted to snake_case.
fn identifier_to_rs(s: &str) -> String {
    if s.chars().next().is_some_and(char::is_uppercase) {
        s.to_string()
    } else {
        to_snake_case(s)
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

/// Convert a `PascalCase` or `camelCase` name to `snake_case`.
fn to_snake_case(s: &str) -> String {
    if s.is_empty() || s == "_" {
        return s.to_string();
    }
    if s.contains('_') && !s.chars().any(|c| c.is_uppercase()) {
        return s.to_string();
    }
    if !s.chars().any(|c| c.is_uppercase()) {
        return s.to_string();
    }
    if s.len() == 1 {
        return s.to_lowercase();
    }

    let mut result = String::with_capacity(s.len() + 4);
    let chars: Vec<char> = s.chars().collect();

    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_uppercase() {
            let prev_is_upper = i > 0 && chars[i - 1].is_uppercase();
            let prev_is_underscore = i > 0 && chars[i - 1] == '_';
            let next_is_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
            if i > 0 && !prev_is_underscore && (!prev_is_upper || next_is_lower) {
                result.push('_');
            }
            result.push(
                ch.to_lowercase()
                    .next()
                    .expect("lowercase yields at least one char"),
            );
        } else {
            result.push(ch);
        }
    }
    result
}

/// Convert a name to `UPPER_SNAKE_CASE` for constants.
fn to_upper_snake_case(s: &str) -> String {
    to_snake_case(s).to_uppercase()
}

/// Escape special characters in a Rust string literal.
fn escape_rs_string(s: &str) -> String {
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

/// Escape special characters in a `format!()` format string.
fn escape_format_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            _ => out.push(ch),
        }
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{AirArg, AirMapEntry, AirRecordField};
    use bock_ast::{GenericParam, Ident, ImportedName, ModulePath, RecordDeclField, TypePath};
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

    fn mod_path(segments: &[&str]) -> ModulePath {
        ModulePath {
            segments: segments.iter().map(|s| ident(s)).collect(),
            span: span(),
        }
    }

    fn imported_name(name: &str) -> ImportedName {
        ImportedName {
            span: span(),
            name: ident(name),
            alias: None,
        }
    }

    fn record_field(name: &str, ty_name: &str) -> RecordDeclField {
        RecordDeclField {
            id: 0,
            span: span(),
            name: ident(name),
            ty: TypeExpr::Named {
                id: 0,
                path: type_path(&[ty_name]),
                args: vec![],
                span: span(),
            },
            default: None,
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
        let gen = RsGenerator::new();
        let result = gen.generate_module(module).unwrap();
        result.files[0].content.clone()
    }

    /// Run `rustc --edition 2021 --crate-type lib` to validate syntax.
    fn check_rs_syntax(code: &str) -> bool {
        use std::io::Write;
        use std::process::Command;
        let id = std::thread::current().id();
        let dir = std::env::temp_dir().join(format!("bock_rs_test_{id:?}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_output.rs");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(code.as_bytes()).unwrap();
        }
        let output = Command::new("rustc")
            .args([
                "--edition",
                "2021",
                "--crate-type",
                "lib",
                "-o",
                dir.join("test_output.rlib").to_str().unwrap(),
            ])
            .arg(&path)
            .stderr(std::process::Stdio::piped())
            .output();
        match output {
            Ok(o) => {
                if !o.status.success() {
                    eprintln!("rustc stderr: {}", String::from_utf8_lossy(&o.stderr));
                }
                o.status.success()
            }
            Err(_) => false,
        }
    }

    // ── Basic tests ─────────────────────────────────────────────────────────

    #[test]
    fn implements_code_generator_trait() {
        let gen = RsGenerator::new();
        assert_eq!(gen.target().id, "rust");
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
        assert!(out.contains("fn answer()"), "got: {out}");
        assert!(out.contains("42"), "got: {out}");
    }

    #[test]
    fn public_function_with_params() {
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
                params: vec![
                    typed_param_node(2, "a", "Int"),
                    typed_param_node(3, "b", "Int"),
                ],
                return_type: Some(Box::new(node(
                    4,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("pub fn add(a: i64, b: i64) -> i64"),
            "got: {out}"
        );
        assert!(out.contains("(a + b)"), "got: {out}");
    }

    #[test]
    fn record_to_struct() {
        let record = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Point"),
                generic_params: vec![],
                fields: vec![record_field("x", "Float"), record_field("y", "Float")],
            },
        );
        let out = gen(&module(vec![], vec![record]));
        assert!(out.contains("pub struct Point {"), "got: {out}");
        assert!(out.contains("pub x: f64,"), "got: {out}");
        assert!(out.contains("pub y: f64,"), "got: {out}");
    }

    #[test]
    fn enum_with_variants() {
        let e = node(
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
                            name: ident("Rgb"),
                            payload: EnumVariantPayload::Struct(vec![
                                record_field("r", "Int"),
                                record_field("g", "Int"),
                            ]),
                        },
                    ),
                    node(
                        7,
                        NodeKind::EnumVariant {
                            name: ident("Custom"),
                            payload: EnumVariantPayload::Tuple(vec![node(
                                8,
                                NodeKind::TypeNamed {
                                    path: type_path(&["String"]),
                                    args: vec![],
                                },
                            )]),
                        },
                    ),
                ],
            },
        );
        let out = gen(&module(vec![], vec![e]));
        assert!(out.contains("pub enum Color {"), "got: {out}");
        assert!(out.contains("Red,"), "got: {out}");
        assert!(out.contains("Green,"), "got: {out}");
        assert!(out.contains("Rgb {"), "got: {out}");
        assert!(out.contains("r: i64,"), "got: {out}");
        assert!(out.contains("Custom(String),"), "got: {out}");
    }

    #[test]
    fn trait_declaration() {
        let t = node(
            1,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Printable"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("print"),
                        generic_params: vec![],
                        params: vec![],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(3, vec![], None)),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![t]));
        assert!(out.contains("pub trait Printable {"), "got: {out}");
        assert!(out.contains("fn print(&self);"), "got: {out}");
    }

    #[test]
    fn impl_block() {
        let imp = node(
            1,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: Some(type_path(&["Printable"])),
                target: Box::new(node(
                    2,
                    NodeKind::TypeNamed {
                        path: type_path(&["Point"]),
                        args: vec![],
                    },
                )),
                where_clause: vec![],
                methods: vec![node(
                    3,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("print"),
                        generic_params: vec![],
                        params: vec![],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], Some(str_lit(5, "point")))),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![imp]));
        assert!(out.contains("impl Printable for Point {"), "got: {out}");
        assert!(out.contains("fn print(&self)"), "got: {out}");
    }

    #[test]
    fn effect_as_trait() {
        let eff = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Log"),
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
        let out = gen(&module(vec![], vec![eff]));
        assert!(out.contains("pub trait Log {"), "got: {out}");
        assert!(out.contains("fn log(&self, msg: String)"), "got: {out}");
    }

    #[test]
    fn function_with_effects() {
        let body = block(3, vec![], Some(int_lit(4, "0")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("process"),
                generic_params: vec![],
                params: vec![typed_param_node(2, "data", "String")],
                return_type: Some(Box::new(node(
                    5,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![type_path(&["Log"]), type_path(&["Clock"])],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("pub fn process(data: String, log: &impl Log, clock: &impl Clock) -> i64"),
            "got: {out}"
        );
    }

    #[test]
    fn ownership_borrow() {
        let borrow = node(
            1,
            NodeKind::Borrow {
                expr: Box::new(id_node(2, "x")),
            },
        );
        let m = module(
            vec![],
            vec![node(
                3,
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
                    body: Box::new(block(4, vec![], Some(borrow))),
                },
            )],
        );
        let out = gen(&m);
        assert!(out.contains("&x"), "got: {out}");
    }

    #[test]
    fn ownership_mutable_borrow() {
        let mborrow = node(
            1,
            NodeKind::MutableBorrow {
                expr: Box::new(id_node(2, "x")),
            },
        );
        let m = module(
            vec![],
            vec![node(
                3,
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
                    body: Box::new(block(4, vec![], Some(mborrow))),
                },
            )],
        );
        let out = gen(&m);
        assert!(out.contains("&mut x"), "got: {out}");
    }

    #[test]
    fn let_binding_with_mut() {
        let let_node = node(
            1,
            NodeKind::LetBinding {
                is_mut: true,
                pattern: Box::new(bind_pat(2, "x")),
                ty: Some(Box::new(node(
                    3,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                value: Box::new(int_lit(4, "42")),
            },
        );
        let m = module(
            vec![],
            vec![node(
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
                    body: Box::new(block(6, vec![let_node], None)),
                },
            )],
        );
        let out = gen(&m);
        assert!(out.contains("let mut x: i64 = 42_i64;"), "got: {out}");
    }

    #[test]
    fn match_expression() {
        let m_node = node(
            1,
            NodeKind::Match {
                scrutinee: Box::new(id_node(2, "color")),
                arms: vec![
                    node(
                        3,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(
                                4,
                                NodeKind::ConstructorPat {
                                    path: type_path(&["Color", "Red"]),
                                    fields: vec![],
                                },
                            )),
                            guard: None,
                            body: Box::new(block(5, vec![], Some(str_lit(6, "red")))),
                        },
                    ),
                    node(
                        7,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(8, NodeKind::WildcardPat)),
                            guard: None,
                            body: Box::new(block(9, vec![], Some(str_lit(10, "other")))),
                        },
                    ),
                ],
            },
        );
        let f = node(
            11,
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
                body: Box::new(block(12, vec![m_node], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("match color"), "got: {out}");
        assert!(out.contains("Color::Red =>"), "got: {out}");
        assert!(out.contains("_ =>"), "got: {out}");
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
        let f = node(
            3,
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
                body: Box::new(block(4, vec![], Some(interp))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("format!(\"Hello, {}!\", name)"), "got: {out}");
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
                value: Some(Box::new(str_lit(4, "oops"))),
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
                body: Box::new(block(6, vec![], Some(ok))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("Ok(42_i64)"), "got: {out}");

        let f2 = node(
            7,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test2"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(8, vec![], Some(err))),
            },
        );
        let out2 = gen(&module(vec![], vec![f2]));
        assert!(out2.contains("Err(\"oops\".to_string())"), "got: {out2}");
    }

    #[test]
    fn vec_literal() {
        let list = node(
            1,
            NodeKind::ListLiteral {
                elems: vec![int_lit(2, "1"), int_lit(3, "2"), int_lit(4, "3")],
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
                body: Box::new(block(6, vec![], Some(list))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("vec![1_i64, 2_i64, 3_i64]"), "got: {out}");
    }

    #[test]
    fn propagate_operator() {
        let prop = node(
            1,
            NodeKind::Propagate {
                expr: Box::new(node(
                    2,
                    NodeKind::Call {
                        callee: Box::new(id_node(3, "parse")),
                        args: vec![],
                        type_args: vec![],
                    },
                )),
            },
        );
        let f = node(
            4,
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
                body: Box::new(block(5, vec![], Some(prop))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("parse()?"), "got: {out}");
    }

    #[test]
    fn range_expression() {
        let range = node(
            1,
            NodeKind::Range {
                lo: Box::new(int_lit(2, "0")),
                hi: Box::new(int_lit(3, "10")),
                inclusive: false,
            },
        );
        let range_incl = node(
            4,
            NodeKind::Range {
                lo: Box::new(int_lit(5, "0")),
                hi: Box::new(int_lit(6, "10")),
                inclusive: true,
            },
        );
        let f = node(
            7,
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
                body: Box::new(block(8, vec![], Some(range))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("0_i64..10_i64"), "got: {out}");

        let f2 = node(
            9,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test2"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(10, vec![], Some(range_incl))),
            },
        );
        let out2 = gen(&module(vec![], vec![f2]));
        assert!(out2.contains("0_i64..=10_i64"), "got: {out2}");
    }

    #[test]
    fn generics_with_bounds() {
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("show"),
                generic_params: vec![GenericParam {
                    id: 100,
                    span: span(),
                    name: ident("T"),
                    bounds: vec![type_path(&["Display"])],
                }],
                params: vec![typed_param_node(2, "val", "T")],
                return_type: Some(Box::new(node(
                    3,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(4, vec![], Some(id_node(5, "val")))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("pub fn show<T: Display>(val: T) -> String"),
            "got: {out}"
        );
    }

    #[test]
    fn type_alias() {
        let alias = node(
            1,
            NodeKind::TypeAlias {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Coord"),
                generic_params: vec![],
                ty: Box::new(node(
                    2,
                    NodeKind::TypeTuple {
                        elems: vec![
                            node(
                                3,
                                NodeKind::TypeNamed {
                                    path: type_path(&["Float"]),
                                    args: vec![],
                                },
                            ),
                            node(
                                4,
                                NodeKind::TypeNamed {
                                    path: type_path(&["Float"]),
                                    args: vec![],
                                },
                            ),
                        ],
                    },
                )),
                where_clause: vec![],
            },
        );
        let out = gen(&module(vec![], vec![alias]));
        assert!(out.contains("pub type Coord = (f64, f64);"), "got: {out}");
    }

    #[test]
    fn const_declaration() {
        let c = node(
            1,
            NodeKind::ConstDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("MaxSize"),
                ty: Box::new(node(
                    2,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                )),
                value: Box::new(int_lit(3, "100")),
            },
        );
        let out = gen(&module(vec![], vec![c]));
        assert!(
            out.contains("pub const MAX_SIZE: i64 = 100_i64;"),
            "got: {out}"
        );
    }

    #[test]
    fn import_declaration() {
        let imp = node(
            1,
            NodeKind::ImportDecl {
                path: mod_path(&["std", "io"]),
                items: ImportItems::Named(vec![imported_name("Read"), imported_name("Write")]),
            },
        );
        let out = gen(&module(vec![imp], vec![]));
        assert!(out.contains("use std::io::{Read, Write};"), "got: {out}");
    }

    #[test]
    fn for_loop() {
        let body = block(
            3,
            vec![node(
                4,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(5, "y")),
                    ty: None,
                    value: Box::new(id_node(6, "x")),
                },
            )],
            None,
        );
        let for_node = node(
            1,
            NodeKind::For {
                pattern: Box::new(bind_pat(2, "x")),
                iterable: Box::new(id_node(7, "items")),
                body: Box::new(body),
            },
        );
        let f = node(
            8,
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
                body: Box::new(block(9, vec![for_node], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("for x in items {"), "got: {out}");
        assert!(out.contains("let y = x;"), "got: {out}");
    }

    #[test]
    fn await_expression() {
        let aw = node(
            1,
            NodeKind::Await {
                expr: Box::new(node(
                    2,
                    NodeKind::Call {
                        callee: Box::new(id_node(3, "fetch")),
                        args: vec![],
                        type_args: vec![],
                    },
                )),
            },
        );
        let f = node(
            4,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(5, vec![], Some(aw))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("async fn test()"), "got: {out}");
        assert!(out.contains("fetch().await"), "got: {out}");
    }

    #[test]
    fn async_main_gets_tokio_main_attribute() {
        let body = block(2, vec![], None);
        let f = node(
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
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("#[tokio::main]"), "got: {out}");
        assert!(out.contains("async fn main()"), "got: {out}");
    }

    #[test]
    fn sync_main_no_tokio_attribute() {
        let body = block(2, vec![], None);
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
        let out = gen(&module(vec![], vec![f]));
        assert!(!out.contains("#[tokio::main]"), "got: {out}");
        assert!(out.contains("fn main()"), "got: {out}");
    }

    #[test]
    fn concurrent_pattern_spawns_tasks() {
        // Two async calls bound to locals, then awaited later in same block —
        // should wrap each in `tokio::spawn` and unwrap JoinHandles on await.
        let call_fetch = |id: u32, name: &str| {
            node(
                id,
                NodeKind::Call {
                    callee: Box::new(id_node(id + 1, name)),
                    args: vec![],
                    type_args: vec![],
                },
            )
        };
        let let_stmt = |id: u32, name: &str, val: AIRNode| {
            node(
                id,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(id + 1, name)),
                    ty: None,
                    value: Box::new(val),
                },
            )
        };
        let await_id = |id: u32, name: &str| {
            node(
                id,
                NodeKind::Await {
                    expr: Box::new(id_node(id + 1, name)),
                },
            )
        };
        let body = block(
            10,
            vec![
                let_stmt(20, "a", call_fetch(21, "task1")),
                let_stmt(30, "b", call_fetch(31, "task2")),
                let_stmt(40, "ra", await_id(41, "a")),
                let_stmt(50, "rb", await_id(51, "b")),
            ],
            Some(id_node(60, "ra")),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("run"),
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
            out.contains("let a = tokio::spawn(task1());"),
            "task1 should be spawned, got: {out}"
        );
        assert!(
            out.contains("let b = tokio::spawn(task2());"),
            "task2 should be spawned, got: {out}"
        );
        assert!(
            out.contains("let ra = a.await.unwrap();"),
            "join handle `a` should be unwrapped on await, got: {out}"
        );
        assert!(
            out.contains("let rb = b.await.unwrap();"),
            "join handle `b` should be unwrapped on await, got: {out}"
        );
    }

    #[test]
    fn sequential_await_no_spawn() {
        // `let a = await task1()` directly awaits — no spawn wrap.
        let await_call = node(
            20,
            NodeKind::Await {
                expr: Box::new(node(
                    21,
                    NodeKind::Call {
                        callee: Box::new(id_node(22, "task1")),
                        args: vec![],
                        type_args: vec![],
                    },
                )),
            },
        );
        let let_stmt = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "a")),
                ty: None,
                value: Box::new(await_call),
            },
        );
        let body = block(30, vec![let_stmt], Some(id_node(40, "a")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("run"),
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
            !out.contains("tokio::spawn"),
            "sequential await should not spawn, got: {out}"
        );
        assert!(out.contains("let a = task1().await;"), "got: {out}");
    }

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
        let f = node(
            4,
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
                body: Box::new(block(5, vec![], Some(rc))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("Point { x: 1_i64, y: 2_i64 }"), "got: {out}");
    }

    #[test]
    fn map_literal() {
        let map = node(
            1,
            NodeKind::MapLiteral {
                entries: vec![AirMapEntry {
                    key: str_lit(2, "key"),
                    value: int_lit(3, "42"),
                }],
            },
        );
        let f = node(
            4,
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
                body: Box::new(block(5, vec![], Some(map))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("std::collections::HashMap::from([(\"key\".to_string(), 42_i64)])"),
            "got: {out}"
        );
    }

    #[test]
    fn tuple_literal() {
        let tup = node(
            1,
            NodeKind::TupleLiteral {
                elems: vec![int_lit(2, "1"), str_lit(3, "hello"), bool_lit(4, true)],
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
                body: Box::new(block(6, vec![], Some(tup))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("(1_i64, \"hello\".to_string(), true)"),
            "got: {out}"
        );
    }

    #[test]
    fn unreachable_expression() {
        let unr = node(1, NodeKind::Unreachable);
        let f = node(
            2,
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
                body: Box::new(block(3, vec![], Some(unr))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("unreachable!()"), "got: {out}");
    }

    #[test]
    fn escape_strings() {
        assert_eq!(escape_rs_string("hello"), "hello");
        assert_eq!(escape_rs_string("he\"llo"), "he\\\"llo");
        assert_eq!(escape_rs_string("new\nline"), "new\\nline");
    }

    #[test]
    fn escape_format_strings() {
        assert_eq!(escape_format_string("hello"), "hello");
        assert_eq!(escape_format_string("{test}"), "{{test}}");
    }

    #[test]
    fn to_snake_case_conversions() {
        assert_eq!(to_snake_case("hello"), "hello");
        assert_eq!(to_snake_case("HelloWorld"), "hello_world");
        assert_eq!(to_snake_case("camelCase"), "camel_case");
        assert_eq!(to_snake_case("HTTPClient"), "http_client");
        assert_eq!(to_snake_case("_"), "_");
    }

    #[test]
    fn to_upper_snake_case_conversions() {
        assert_eq!(to_upper_snake_case("MaxSize"), "MAX_SIZE");
        assert_eq!(to_upper_snake_case("httpClient"), "HTTP_CLIENT");
    }

    // ── End-to-end syntax validation tests ──────────────────────────────────

    #[test]
    #[ignore]
    fn e2e_simple_function_compiles() {
        let body = block(2, vec![], Some(int_lit(3, "42")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("answer"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(node(
                    4,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            check_rs_syntax(&out),
            "Generated Rust does not compile:\n{out}"
        );
    }

    #[test]
    #[ignore]
    fn e2e_struct_compiles() {
        let record = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Point"),
                generic_params: vec![],
                fields: vec![record_field("x", "Float"), record_field("y", "Float")],
            },
        );
        let out = gen(&module(vec![], vec![record]));
        assert!(
            check_rs_syntax(&out),
            "Generated Rust does not compile:\n{out}"
        );
    }

    #[test]
    #[ignore]
    fn e2e_enum_compiles() {
        let e = node(
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
                            name: ident("Rgb"),
                            payload: EnumVariantPayload::Struct(vec![record_field("r", "Int")]),
                        },
                    ),
                    node(
                        5,
                        NodeKind::EnumVariant {
                            name: ident("Custom"),
                            payload: EnumVariantPayload::Tuple(vec![node(
                                6,
                                NodeKind::TypeNamed {
                                    path: type_path(&["String"]),
                                    args: vec![],
                                },
                            )]),
                        },
                    ),
                ],
            },
        );
        let out = gen(&module(vec![], vec![e]));
        assert!(
            check_rs_syntax(&out),
            "Generated Rust does not compile:\n{out}"
        );
    }

    #[test]
    #[ignore]
    fn e2e_trait_and_impl_compiles() {
        let trait_decl = node(
            1,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Greet"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("greet"),
                        generic_params: vec![],
                        params: vec![],
                        return_type: Some(Box::new(node(
                            3,
                            NodeKind::TypeNamed {
                                path: type_path(&["String"]),
                                args: vec![],
                            },
                        ))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], None)),
                    },
                )],
            },
        );
        let struct_decl = node(
            10,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Person"),
                generic_params: vec![],
                fields: vec![record_field("name", "String")],
            },
        );
        let impl_block = node(
            20,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: Some(type_path(&["Greet"])),
                target: Box::new(node(
                    21,
                    NodeKind::TypeNamed {
                        path: type_path(&["Person"]),
                        args: vec![],
                    },
                )),
                where_clause: vec![],
                methods: vec![node(
                    22,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("greet"),
                        generic_params: vec![],
                        params: vec![],
                        return_type: Some(Box::new(node(
                            23,
                            NodeKind::TypeNamed {
                                path: type_path(&["String"]),
                                args: vec![],
                            },
                        ))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(24, vec![], Some(str_lit(25, "hello")))),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![trait_decl, struct_decl, impl_block]));
        assert!(
            check_rs_syntax(&out),
            "Generated Rust does not compile:\n{out}"
        );
    }

    // ── Prelude function mapping tests ──────────────────────────────────────

    /// Helper: generate Rust for a module with a `main` function containing a single call.
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

    /// Helper: generate Rust for a nullary prelude call (no args).
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
    fn prelude_println_maps_to_println_macro() {
        let out = gen_prelude_call("println", str_lit(12, "hello"));
        assert!(
            out.contains("println!(\"{}\", "),
            "println should map to println! macro, got: {out}"
        );
        assert!(
            !out.contains("println("),
            "should not emit bare println(, got: {out}"
        );
    }

    #[test]
    fn prelude_print_maps_to_print_macro() {
        let out = gen_prelude_call("print", str_lit(12, "hello"));
        assert!(
            out.contains("print!(\"{}\", "),
            "print should map to print! macro, got: {out}"
        );
    }

    #[test]
    fn prelude_debug_maps_to_dbg_macro() {
        let out = gen_prelude_call("debug", str_lit(12, "val"));
        assert!(
            out.contains("dbg!(&"),
            "debug should map to dbg! macro, got: {out}"
        );
    }

    #[test]
    fn prelude_assert_maps_to_assert_macro() {
        let out = gen_prelude_call("assert", bool_lit(12, true));
        assert!(
            out.contains("assert!("),
            "assert should map to assert! macro, got: {out}"
        );
    }

    #[test]
    fn prelude_todo_maps_to_todo_macro() {
        let out = gen_prelude_call_no_args("todo");
        assert!(
            out.contains("todo!()"),
            "todo should map to todo! macro, got: {out}"
        );
    }

    #[test]
    fn prelude_unreachable_maps_to_unreachable_macro() {
        let out = gen_prelude_call_no_args("unreachable");
        assert!(
            out.contains("unreachable!()"),
            "unreachable should map to unreachable! macro, got: {out}"
        );
    }

    #[test]
    fn non_prelude_call_passes_through() {
        let out = gen_prelude_call("my_custom_func", str_lit(12, "arg"));
        assert!(
            out.contains("my_custom_func("),
            "non-prelude call should use snake_case, got: {out}"
        );
    }

    #[test]
    fn handling_block_passes_handlers_to_effectful_call() {
        use bock_air::AirHandlerPair;

        // effect Logger { fn log(msg: String) -> Void }
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

        // fn inner() -> String with Logger { ... }
        let inner_fn = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("inner"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(node(
                    11,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![type_path(&["Logger"])],
                where_clause: vec![],
                body: Box::new(block(12, vec![], Some(str_lit(13, "hello")))),
            },
        );

        // fn main() { handling (Logger with StdoutLogger {}) { inner() } }
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
        // inner() should receive the handler: inner(&__logger)
        assert!(
            out.contains("inner(&__logger)"),
            "handling block should pass handler to effectful call, got: {out}"
        );
        // The handling block should instantiate the handler. The PascalCase
        // identifier is preserved, since it names a type/constructor in Rust.
        assert!(
            out.contains("let __logger = StdoutLogger()"),
            "handling block should instantiate handler, got: {out}"
        );
    }

    #[test]
    fn nested_handling_blocks_shadow_handlers() {
        use bock_air::AirHandlerPair;

        // effect Logger { fn log(msg: String) -> Void }
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

        // fn inner() -> String with Logger { ... }
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

        // Nested handling: inner handling block shadows outer
        let inner_call = node(
            20,
            NodeKind::Call {
                callee: Box::new(id_node(21, "inner")),
                args: vec![],
                type_args: vec![],
            },
        );
        let inner_handling = node(
            30,
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path(&["Logger"]),
                    handler: Box::new(id_node(31, "inner_logger")),
                }],
                body: Box::new(block(32, vec![], Some(inner_call))),
            },
        );
        let outer_handling = node(
            40,
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path(&["Logger"]),
                    handler: Box::new(id_node(41, "outer_logger")),
                }],
                body: Box::new(block(42, vec![inner_handling], None)),
            },
        );
        let main_fn = node(
            50,
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
                body: Box::new(block(51, vec![outer_handling], None)),
            },
        );

        let out = gen(&module(vec![], vec![effect_decl, inner_fn, main_fn]));
        // Inner handling should shadow: inner(&__logger) where __logger = inner_logger
        assert!(
            out.contains("let __logger = inner_logger"),
            "inner handling should shadow outer handler, got: {out}"
        );
        assert!(
            out.contains("inner(&__logger)"),
            "call should use innermost handler, got: {out}"
        );
    }
}
