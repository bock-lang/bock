//! Python code generator — rule-based (Tier 2) transpilation from AIR to Python.
//!
//! Handles all capability gaps:
//! - Records → `@dataclass` classes
//! - Algebraic types → dataclasses with `_tag` discriminant
//! - Pattern matching → native `match`/`case` (Python 3.10+)
//! - Effects → keyword arguments
//! - Ownership → erased (Python is GC)
//! - Generics → erased (Python uses runtime typing)
//! - Type hints on all declarations

use std::collections::HashMap;
use std::fmt::Write;
use std::path::PathBuf;

use bock_air::{AIRNode, AirInterpolationPart, EnumVariantPayload, NodeKind, ResultVariant};
use bock_ast::{AssignOp, BinOp, ImportItems, Literal, TypeExpr, UnaryOp, Visibility};
use bock_types::AIRModule;

use crate::error::CodegenError;
use crate::generator::{CodeGenerator, GeneratedCode, OutputFile, SourceMap};
use crate::profile::TargetProfile;

/// Runtime helpers for Bock concurrency in Python. Backed by
/// `asyncio.Queue`, which supports multiple producers and a single
/// consumer awaiting `.get()` at a time. Injected at the top of any
/// module that references `Channel` or `spawn`.
/// Conservative module scan for `Channel` / `spawn` references.
fn py_module_uses_concurrency(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Channel\"") || s.contains("\"spawn\"")
    })
}

const CONCURRENCY_RUNTIME_PY: &str = "\
# ── Bock concurrency runtime ──
import asyncio as __bock_asyncio

class __BockChannel:
    __slots__ = ('_q',)
    def __init__(self):
        self._q = __bock_asyncio.Queue()
    def send(self, v):
        self._q.put_nowait(v)
    async def recv(self):
        return await self._q.get()
    def close(self):
        pass

def __bock_channel_new():
    ch = __BockChannel()
    return (ch, ch)

def __bock_spawn(x):
    # If already a coroutine, wrap it in a Task so it starts eagerly.
    if __bock_asyncio.iscoroutine(x):
        return __bock_asyncio.create_task(x)
    return x
";

/// Python code generator implementing the `CodeGenerator` trait.
#[derive(Debug)]
pub struct PyGenerator {
    profile: TargetProfile,
}

impl PyGenerator {
    /// Creates a new Python code generator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: TargetProfile::python(),
        }
    }
}

impl Default for PyGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeGenerator for PyGenerator {
    fn target(&self) -> &TargetProfile {
        &self.profile
    }

    fn generate_module(&self, module: &AIRModule) -> Result<GeneratedCode, CodegenError> {
        let mut ctx = PyEmitCtx::new();
        ctx.emit_node(module)?;
        let content = ctx.finish();
        let source_map = SourceMap {
            generated_file: "output.py".to_string(),
            ..Default::default()
        };
        Ok(GeneratedCode {
            files: vec![OutputFile {
                path: PathBuf::from("output.py"),
                content,
            }],
            source_map: Some(source_map),
        })
    }

    fn entry_invocation(&self, main_is_async: bool) -> Option<String> {
        if main_is_async {
            Some(
                "if __name__ == \"__main__\":\n    asyncio.run(main())\n"
                    .to_string(),
            )
        } else {
            Some("if __name__ == \"__main__\":\n    main()\n".to_string())
        }
    }
}

// ─── Emission context ────────────────────────────────────────────────────────

/// Internal state for Python emission.
struct PyEmitCtx {
    buf: String,
    indent: usize,
    needs_dataclass_import: bool,
    needs_abc_import: bool,
    /// Set when any `async def` is emitted; forces `import asyncio` in the
    /// preamble so awaited calls, `asyncio.run`, and `asyncio.create_task`
    /// resolve at runtime.
    needs_asyncio_import: bool,
    /// Set when Duration/Instant codegen emits `time.monotonic_ns()`.
    needs_time_import: bool,
    /// Names bound in the current block whose call value should be wrapped
    /// in `asyncio.create_task(...)` because the binding is later `await`ed
    /// within the same block. See [`Self::collect_task_bindings`].
    task_bound_names: std::collections::HashSet<String>,
    /// Maps effect operation name → effect type name (e.g., "log" → "Logger").
    effect_ops: HashMap<String, String>,
    /// Maps effect type name → current handler variable name in scope.
    current_handler_vars: HashMap<String, String>,
    /// Maps function name → effect type names from its `with` clause.
    fn_effects: HashMap<String, Vec<String>>,
    /// Maps composite effect name → component effect names.
    composite_effects: HashMap<String, Vec<String>>,
    /// Monotonically-increasing counter used to generate unique handler
    /// variable names per handling block. Python lacks block scope for
    /// `=` bindings, so without a suffix, nested `handling (...)` blocks
    /// would overwrite each other's handler variables.
    handling_counter: usize,
    /// Trait impls keyed by target record name, collected up front from the
    /// current module's items so `RecordDecl` emission can inline the impl
    /// methods as class members instead of leaving orphan module-level
    /// functions that never get bound to the handler instance.
    impls_by_target: HashMap<String, Vec<AIRNode>>,
}

impl PyEmitCtx {
    fn new() -> Self {
        Self {
            buf: String::with_capacity(4096),
            indent: 0,
            needs_dataclass_import: false,
            needs_abc_import: false,
            needs_asyncio_import: false,
            needs_time_import: false,
            task_bound_names: std::collections::HashSet::new(),
            effect_ops: HashMap::new(),
            current_handler_vars: HashMap::new(),
            fn_effects: HashMap::new(),
            composite_effects: HashMap::new(),
            handling_counter: 0,
            impls_by_target: HashMap::new(),
        }
    }

    fn finish(mut self) -> String {
        let mut preamble = String::new();
        if self.needs_asyncio_import {
            preamble.push_str("import asyncio\n");
        }
        if self.needs_time_import {
            preamble.push_str("import time\n");
        }
        if self.needs_abc_import {
            preamble.push_str("from abc import ABC, abstractmethod\n");
        }
        if self.needs_dataclass_import {
            preamble.push_str("from dataclasses import dataclass\n");
        }
        if !preamble.is_empty() {
            preamble.push('\n');
            self.buf.insert_str(0, &preamble);
        }
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

    /// Map Bock prelude functions to Python equivalents.
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
                format!("print({a})")
            }
            "print" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("print({a}, end=\"\")")
            }
            "debug" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("print(repr({a}))")
            }
            "assert" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("assert {a}")
            }
            "todo" => "raise NotImplementedError()".to_string(),
            "unreachable" => "raise RuntimeError(\"unreachable\")".to_string(),
            "sleep" => {
                self.needs_asyncio_import = true;
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                // Duration is ns → asyncio.sleep takes seconds.
                format!("asyncio.sleep(({a}) / 1_000_000_000)")
            }
            _ => return Ok(None),
        };
        Ok(Some(code))
    }

    /// Recognise `Duration.xxx(...)` / `Instant.xxx(...)` associated-function
    /// calls and emit inline arithmetic. Durations are ints (nanoseconds);
    /// Instants are ints representing `time.monotonic_ns()`.
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
            ("Duration", "micros") => format!("(({}) * 1_000)", arg0()),
            ("Duration", "millis") => format!("(({}) * 1_000_000)", arg0()),
            ("Duration", "seconds") => format!("(({}) * 1_000_000_000)", arg0()),
            ("Duration", "minutes") => format!("(({}) * 60_000_000_000)", arg0()),
            ("Duration", "hours") => format!("(({}) * 3_600_000_000_000)", arg0()),
            ("Instant", "now") => {
                self.needs_time_import = true;
                "time.monotonic_ns()".to_string()
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Recognise `Channel.new()`, `spawn(...)`, and method calls on a
    /// channel value (`send`, `recv`, `close`) and emit the Python
    /// runtime helper equivalents.
    fn try_emit_concurrency_call(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        if let NodeKind::Identifier { name } = &callee.kind {
            if name.name == "spawn" {
                self.buf.push_str("__bock_spawn(");
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
            "as_millis" => format!("(({recv_str}) // 1_000_000)"),
            "as_seconds" => format!("(({recv_str}) // 1_000_000_000)"),
            "is_zero" => format!("(({recv_str}) == 0)"),
            "is_negative" => format!("(({recv_str}) < 0)"),
            "abs" => format!("abs({recv_str})"),
            "elapsed" => {
                self.needs_time_import = true;
                format!("(time.monotonic_ns() - ({recv_str}))")
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

    // ── Top-level dispatch ──────────────────────────────────────────────────

    fn emit_node(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::Module { imports, items, .. } => {
                if py_module_uses_concurrency(items) {
                    self.buf.push_str(CONCURRENCY_RUNTIME_PY);
                    self.buf.push('\n');
                }
                for imp in imports {
                    self.emit_node(imp)?;
                }
                if !imports.is_empty() && !items.is_empty() {
                    self.buf.push('\n');
                }
                // Pre-scan trait impls so we can attach their methods to the
                // target record's class body instead of leaving them as
                // orphan module-level functions with a `self` parameter.
                self.impls_by_target.clear();
                let mut consumed_impls: std::collections::HashSet<bock_air::NodeId> =
                    std::collections::HashSet::new();
                for item in items.iter() {
                    if let NodeKind::ImplBlock {
                        trait_path: Some(_),
                        target,
                        ..
                    } = &item.kind
                    {
                        if let Some(target_name) = ast_type_name(target) {
                            self.impls_by_target
                                .entry(target_name)
                                .or_default()
                                .push(item.clone());
                            consumed_impls.insert(item.id);
                        }
                    }
                }
                for (i, item) in items.iter().enumerate() {
                    if consumed_impls.contains(&item.id) {
                        continue;
                    }
                    if i > 0 && !self.buf.is_empty() && !self.buf.ends_with("\n\n") {
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
                        self.writeln(&format!("import {path_str}"));
                    }
                    ImportItems::Named(names) => {
                        let names_str = names
                            .iter()
                            .map(|n| to_snake_case(&n.name.name))
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.writeln(&format!("from {path_str} import {names_str}"));
                    }
                    ImportItems::Glob => {
                        self.writeln(&format!("from {path_str} import *"));
                    }
                }
                Ok(())
            }
            NodeKind::FnDecl {
                visibility,
                is_async,
                name,
                params,
                return_type,
                effect_clause,
                body,
                ..
            } => self.emit_fn_decl(
                *visibility,
                *is_async,
                &name.name,
                params,
                return_type.as_deref(),
                effect_clause,
                body,
            ),
            NodeKind::RecordDecl { name, fields, .. } => {
                // Pull any previously-collected `impl Trait for Name` blocks
                // so their methods become part of this class body and the
                // class inherits from every implemented trait — giving real
                // method dispatch (a bare instance has no orphan methods).
                let impls = self.impls_by_target.remove(&name.name).unwrap_or_default();
                let bases: Vec<String> = impls
                    .iter()
                    .filter_map(|im| {
                        if let NodeKind::ImplBlock {
                            trait_path: Some(tp),
                            ..
                        } = &im.kind
                        {
                            tp.segments.last().map(|s| s.name.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                let base_list = if bases.is_empty() {
                    String::new()
                } else {
                    format!("({})", bases.join(", "))
                };
                // `@dataclass` is only appropriate when the class actually
                // carries data. Empty handler structs are cleaner as plain
                // classes — `@dataclass` on an ABC subclass without fields
                // adds no value and drags in the dataclass metaclass.
                if !fields.is_empty() {
                    self.needs_dataclass_import = true;
                    self.writeln("@dataclass");
                }
                self.writeln(&format!("class {}{base_list}:", name.name));
                self.indent += 1;
                let has_members = !fields.is_empty()
                    || impls
                        .iter()
                        .any(|im| matches!(&im.kind, NodeKind::ImplBlock { methods, .. } if !methods.is_empty()));
                if !has_members {
                    self.writeln("pass");
                } else {
                    for f in fields {
                        let type_hint = self.ast_type_to_py(&f.ty);
                        self.writeln(&format!("{}: {type_hint}", to_snake_case(&f.name.name)));
                    }
                    for im in &impls {
                        if let NodeKind::ImplBlock { methods, .. } = &im.kind {
                            for method in methods {
                                self.buf.push('\n');
                                self.emit_class_method(method)?;
                            }
                        }
                    }
                }
                self.indent -= 1;
                Ok(())
            }
            NodeKind::EnumDecl { name, variants, .. } => {
                self.needs_dataclass_import = true;
                for (i, variant) in variants.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
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
                self.writeln(&format!("class {}:", name.name));
                self.indent += 1;
                // __init__
                if !fields.is_empty() {
                    let params: Vec<String> = fields
                        .iter()
                        .map(|f| {
                            let fname = to_snake_case(&f.name.name);
                            let type_hint = self.ast_type_to_py(&f.ty);
                            format!("{fname}: {type_hint}")
                        })
                        .collect();
                    self.writeln(&format!("def __init__(self, {}):", params.join(", ")));
                    self.indent += 1;
                    for f in fields {
                        let fname = to_snake_case(&f.name.name);
                        self.writeln(&format!("self.{fname} = {fname}"));
                    }
                    self.indent -= 1;
                }
                // Methods
                for method in methods {
                    self.buf.push('\n');
                    self.emit_class_method(method)?;
                }
                if fields.is_empty() && methods.is_empty() {
                    self.writeln("pass");
                }
                self.indent -= 1;
                Ok(())
            }
            NodeKind::TraitDecl { name, methods, .. } => {
                // Traits → abstract base class (comment + class with pass methods).
                self.writeln(&format!("# trait {}", name.name));
                self.writeln(&format!("class {}:", name.name));
                self.indent += 1;
                if methods.is_empty() {
                    self.writeln("pass");
                } else {
                    for (i, method) in methods.iter().enumerate() {
                        if i > 0 {
                            self.buf.push('\n');
                        }
                        self.emit_class_method(method)?;
                    }
                }
                self.indent -= 1;
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
                    self.writeln(&format!("# impl {trait_name} for {target_name}"));
                } else {
                    self.writeln(&format!("# impl {target_name}"));
                }
                for method in methods {
                    if let NodeKind::FnDecl {
                        is_async,
                        name,
                        params,
                        return_type,
                        effect_clause,
                        body,
                        ..
                    } = &method.kind
                    {
                        if *is_async {
                            self.needs_asyncio_import = true;
                        }
                        let async_kw = if *is_async { "async " } else { "" };
                        let param_strs = self.collect_param_strs(params);
                        let effects = self.effects_params(effect_clause);
                        let mut all_params = vec!["self".to_string()];
                        all_params.extend(param_strs);
                        all_params.extend(effects);
                        let ret = return_type
                            .as_deref()
                            .map(|t| format!(" -> {}", self.type_to_py(t)))
                            .unwrap_or_default();
                        let fn_name = to_snake_case(&name.name);
                        self.writeln(&format!(
                            "{async_kw}def {fn_name}({}){}:",
                            all_params.join(", "),
                            ret,
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
                        "# composite effect {} = {}",
                        name.name,
                        comp_names.join(" + ")
                    ));
                    self.composite_effects
                        .insert(name.name.clone(), comp_names);
                    return Ok(());
                }
                // Record effect operations for Call → handler.op rewriting.
                for op in operations {
                    if let NodeKind::FnDecl {
                        name: op_name, ..
                    } = &op.kind
                    {
                        self.effect_ops
                            .insert(op_name.name.clone(), name.name.clone());
                    }
                }
                // Effects → abstract base class with @abstractmethod.
                self.needs_abc_import = true;
                self.writeln(&format!("class {}(ABC):", name.name));
                self.indent += 1;
                if operations.is_empty() {
                    self.writeln("pass");
                } else {
                    for (i, op) in operations.iter().enumerate() {
                        if i > 0 {
                            self.buf.push('\n');
                        }
                        if let NodeKind::FnDecl {
                            name,
                            params,
                            return_type,
                            ..
                        } = &op.kind
                        {
                            self.writeln("@abstractmethod");
                            let param_strs = self.collect_param_strs(params);
                            let mut all_params = vec!["self".to_string()];
                            all_params.extend(param_strs);
                            let ret = return_type
                                .as_deref()
                                .map(|t| format!(" -> {}", self.type_to_py(t)))
                                .unwrap_or_default();
                            let fn_name = to_snake_case(&name.name);
                            self.writeln(&format!(
                                "def {fn_name}({}){}:",
                                all_params.join(", "),
                                ret,
                            ));
                            self.indent += 1;
                            self.writeln("...");
                            self.indent -= 1;
                        }
                    }
                }
                self.indent -= 1;
                Ok(())
            }
            NodeKind::TypeAlias { name, .. } => {
                self.writeln(&format!("# type {} = ...", name.name));
                Ok(())
            }
            NodeKind::ConstDecl {
                name, value, ty, ..
            } => {
                let type_hint = format!(": {}", self.type_to_py(ty));
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}{}{type_hint} = ", to_snake_case(&name.name));
                self.emit_expr(value)?;
                self.buf.push('\n');
                Ok(())
            }
            NodeKind::ModuleHandle { effect, handler } => {
                // Emit `__<effect>: Effect = Handler()` at module scope and
                // register it as the default handler. Effectful calls later
                // in the module will pick it up via `current_handler_vars`
                // unless a local handling block overrides it.
                let effect_name =
                    effect.segments.last().map_or("effect", |s| s.name.as_str());
                let var_name = format!("__{}", to_snake_case(effect_name));
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}{var_name}: {effect_name} = ");
                self.emit_expr(handler)?;
                self.buf.push('\n');
                self.current_handler_vars
                    .insert(effect_name.to_string(), var_name);
                Ok(())
            }
            NodeKind::PropertyTest { name, body, .. } => {
                self.writeln(&format!("# property test: {name}"));
                self.writeln("# (property tests are not emitted in Python output)");
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
                self.buf.push('\n');
                Ok(())
            }
        }
    }

    // ── Function declarations ───────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn emit_fn_decl(
        &mut self,
        _visibility: Visibility,
        is_async: bool,
        name: &str,
        params: &[AIRNode],
        return_type: Option<&AIRNode>,
        effect_clause: &[bock_ast::TypePath],
        body: &AIRNode,
    ) -> Result<(), CodegenError> {
        if is_async {
            self.needs_asyncio_import = true;
        }
        let async_kw = if is_async { "async " } else { "" };
        let param_strs = self.collect_param_strs(params);
        let effects = self.effects_params(effect_clause);
        let mut all_params = param_strs;
        all_params.extend(effects);
        let ret = return_type
            .map(|t| format!(" -> {}", self.type_to_py(t)))
            .unwrap_or_default();
        if !effect_clause.is_empty() {
            let effect_names = self.expand_effect_names(effect_clause);
            self.fn_effects.insert(name.to_string(), effect_names);
        }
        let fn_name = to_snake_case(name);
        self.writeln(&format!(
            "{async_kw}def {fn_name}({}){}:",
            all_params.join(", "),
            ret,
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
        Ok(())
    }

    fn emit_class_method(&mut self, method: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::FnDecl {
            is_async,
            name,
            params,
            return_type,
            effect_clause,
            body,
            ..
        } = &method.kind
        {
            if *is_async {
                self.needs_asyncio_import = true;
            }
            let async_kw = if *is_async { "async " } else { "" };
            let param_strs = self.collect_param_strs(params);
            let effects = self.effects_params(effect_clause);
            let mut all_params = vec!["self".to_string()];
            all_params.extend(param_strs);
            all_params.extend(effects);
            let ret = return_type
                .as_deref()
                .map(|t| format!(" -> {}", self.type_to_py(t)))
                .unwrap_or_default();
            let fn_name = to_snake_case(&name.name);
            self.writeln(&format!(
                "{async_kw}def {fn_name}({}){}:",
                all_params.join(", "),
                ret,
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
        }
        Ok(())
    }

    fn collect_param_strs(&self, params: &[AIRNode]) -> Vec<String> {
        params
            .iter()
            .filter_map(|p| {
                if let NodeKind::Param {
                    pattern,
                    ty,
                    default,
                } = &p.kind
                {
                    let name = to_snake_case(&self.pattern_to_binding_name(pattern));
                    let type_hint = ty
                        .as_ref()
                        .map(|t| format!(": {}", self.type_to_py(t)))
                        .unwrap_or_default();
                    if let Some(def) = default {
                        let mut ctx = PyEmitCtx::new();
                        ctx.indent = self.indent;
                        if ctx.emit_expr(def).is_ok() {
                            let def_str = ctx.buf;
                            return Some(format!("{name}{type_hint} = {def_str}"));
                        }
                    }
                    Some(format!("{name}{type_hint}"))
                } else {
                    None
                }
            })
            .collect()
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

    /// Effects → keyword arguments: `*, log: Log, clock: Clock`.
    fn effects_params(&self, effects: &[bock_ast::TypePath]) -> Vec<String> {
        if effects.is_empty() {
            return vec![];
        }
        let expanded = self.expand_effect_names(effects);
        let mut result = vec!["*".to_string()];
        for name in &expanded {
            let param_name = to_snake_case(name);
            result.push(format!("{param_name}: {name}"));
        }
        result
    }

    /// Build `effect=handler_var, ...` keyword arguments for calling an effectful function.
    fn build_effects_call_args_py(&self, fn_name: &str) -> Option<String> {
        let effects = self.fn_effects.get(fn_name)?;
        let entries: Vec<String> = effects
            .iter()
            .filter_map(|e| {
                let handler_var = self.current_handler_vars.get(e)?;
                let param_name = to_snake_case(e);
                Some(format!("{param_name}={handler_var}"))
            })
            .collect();
        if entries.is_empty() {
            return None;
        }
        Some(entries.join(", "))
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
                    self.writeln("@dataclass(frozen=True)");
                    self.writeln(&format!("class {enum_name}_{vname}:"));
                    self.indent += 1;
                    self.writeln(&format!("_tag: str = \"{vname}\""));
                    self.indent -= 1;
                }
                EnumVariantPayload::Struct(fields) => {
                    self.writeln("@dataclass");
                    self.writeln(&format!("class {enum_name}_{vname}:"));
                    self.indent += 1;
                    for f in fields {
                        let type_hint = self.ast_type_to_py(&f.ty);
                        self.writeln(&format!("{}: {type_hint}", to_snake_case(&f.name.name)));
                    }
                    self.writeln(&format!("_tag: str = \"{vname}\""));
                    self.indent -= 1;
                }
                EnumVariantPayload::Tuple(elems) => {
                    self.writeln("@dataclass");
                    self.writeln(&format!("class {enum_name}_{vname}:"));
                    self.indent += 1;
                    for (i, elem) in elems.iter().enumerate() {
                        let type_hint = self.type_to_py(elem);
                        self.writeln(&format!("_{i}: {type_hint}"));
                    }
                    self.writeln(&format!("_tag: str = \"{vname}\""));
                    self.indent -= 1;
                }
            }
        }
        Ok(())
    }

    // ── Statements ──────────────────────────────────────────────────────────

    fn emit_stmt(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::LetBinding {
                pattern, value, ty, ..
            } => {
                let binding = self.pattern_to_py_binding(pattern);
                let type_hint = ty
                    .as_ref()
                    .map(|t| format!(": {}", self.type_to_py(t)))
                    .unwrap_or_default();
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}{binding}{type_hint} = ");
                let wrap_task = matches!(&value.kind, NodeKind::Call { .. })
                    && self.task_bound_names.contains(&binding);
                if wrap_task {
                    self.needs_asyncio_import = true;
                    self.buf.push_str("asyncio.create_task(");
                    self.emit_expr(value)?;
                    self.buf.push(')');
                } else {
                    self.emit_expr(value)?;
                }
                self.buf.push('\n');
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
                    let binding = self.pattern_to_py_binding(pat);
                    let _ = write!(self.buf, "{ind}{binding} = ");
                    self.emit_expr(condition)?;
                    self.buf.push('\n');
                    self.writeln(&format!("if {binding} is not None:"));
                    self.indent += 1;
                    self.emit_block_body(then_block)?;
                    self.indent -= 1;
                } else {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}if ");
                    self.emit_expr(condition)?;
                    self.buf.push_str(":\n");
                    self.indent += 1;
                    self.emit_block_body(then_block)?;
                    self.indent -= 1;
                }
                if let Some(else_b) = else_block {
                    if matches!(else_b.kind, NodeKind::If { .. }) {
                        let ind = self.indent_str();
                        let _ = write!(self.buf, "{ind}el");
                        self.emit_stmt(else_b)?;
                        return Ok(());
                    }
                    self.writeln("else:");
                    self.indent += 1;
                    self.emit_block_body(else_b)?;
                    self.indent -= 1;
                }
                Ok(())
            }
            NodeKind::For {
                pattern,
                iterable,
                body,
            } => {
                let binding = self.pattern_to_py_binding(pattern);
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}for {binding} in ");
                self.emit_expr(iterable)?;
                self.buf.push_str(":\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                Ok(())
            }
            NodeKind::While { condition, body } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}while ");
                self.emit_expr(condition)?;
                self.buf.push_str(":\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                Ok(())
            }
            NodeKind::Loop { body } => {
                self.writeln("while True:");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                Ok(())
            }
            NodeKind::Return { value } => {
                if let Some(val) = value {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}return ");
                    self.emit_expr(val)?;
                    self.buf.push('\n');
                } else {
                    self.writeln("return");
                }
                Ok(())
            }
            NodeKind::Break { value } => {
                if let Some(val) = value {
                    // Python break doesn't support values; emit as comment + break.
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}# break value: ");
                    self.emit_expr(val)?;
                    self.buf.push('\n');
                    self.writeln("break");
                } else {
                    self.writeln("break");
                }
                Ok(())
            }
            NodeKind::Continue => {
                self.writeln("continue");
                Ok(())
            }
            NodeKind::Guard {
                condition,
                else_block,
                ..
            } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}if not (");
                self.emit_expr(condition)?;
                self.buf.push_str("):\n");
                self.indent += 1;
                self.emit_block_body(else_block)?;
                self.indent -= 1;
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
                // handling block → handler variable bindings then body.
                // Each handling block gets a fresh numeric suffix so nested
                // blocks do not overwrite each other's handler variables —
                // Python has function scope, not block scope, for `=`, so
                // `__logger = X()` in an inner block would otherwise stomp
                // the outer binding permanently.
                let old_handler_vars = self.current_handler_vars.clone();
                self.handling_counter += 1;
                let suffix = format!("_h{}", self.handling_counter);
                for h in handlers {
                    let effect_name =
                        h.effect.segments.last().map_or("effect", |s| s.name.as_str());
                    let var_name = format!("__{}{suffix}", to_snake_case(effect_name));
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}{var_name}: {effect_name} = ");
                    self.emit_expr(&h.handler)?;
                    self.buf.push('\n');
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
                self.buf.push('\n');
                Ok(())
            }
            _ => {
                self.write_indent();
                self.emit_expr(node)?;
                self.buf.push('\n');
                Ok(())
            }
        }
    }

    // ── Expressions ─────────────────────────────────────────────────────────

    fn emit_expr(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::Literal { lit } => {
                match lit {
                    Literal::Int(s) => self.buf.push_str(s),
                    Literal::Float(s) => self.buf.push_str(s),
                    Literal::Bool(b) => self.buf.push_str(if *b { "True" } else { "False" }),
                    Literal::Char(s) => {
                        self.buf.push('\'');
                        self.buf.push_str(s);
                        self.buf.push('\'');
                    }
                    Literal::String(s) => {
                        self.buf.push('"');
                        self.buf.push_str(&escape_py_string(s));
                        self.buf.push('"');
                    }
                    Literal::Unit => self.buf.push_str("None"),
                }
                Ok(())
            }
            NodeKind::Identifier { name } => {
                self.buf.push_str(&identifier_to_py(&name.name));
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
                    BinOp::Eq => " == ",
                    BinOp::Ne => " != ",
                    BinOp::Lt => " < ",
                    BinOp::Le => " <= ",
                    BinOp::Gt => " > ",
                    BinOp::Ge => " >= ",
                    BinOp::And => " and ",
                    BinOp::Or => " or ",
                    BinOp::BitAnd => " & ",
                    BinOp::BitOr => " | ",
                    BinOp::BitXor => " ^ ",
                    BinOp::Compose => " # compose ",
                    BinOp::Is => " is ",
                };
                self.buf.push_str(op_str);
                self.emit_expr(right)?;
                self.buf.push(')');
                Ok(())
            }
            NodeKind::UnaryOp { op, operand } => {
                let op_str = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not ",
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
                            let _ = write!(self.buf, "{}.{}", handler_var, to_snake_case(&name.name));
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
                let effects_args = if let NodeKind::Identifier { name } = &callee.kind {
                    self.build_effects_call_args_py(&name.name)
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
                let _ = write!(self.buf, "lambda {}: ", param_strs.join(", "));
                self.emit_expr(body)?;
                Ok(())
            }
            NodeKind::Pipe { left, right } => self.emit_pipe(left, right),
            NodeKind::Compose { left, right } => {
                // `f >> g` → `lambda x: g(f(x))`
                let _ = write!(self.buf, "lambda x: ");
                self.emit_expr(right)?;
                self.buf.push('(');
                self.emit_expr(left)?;
                self.buf.push_str("(x))");
                Ok(())
            }
            NodeKind::Await { expr } => {
                self.buf.push_str("(await ");
                self.emit_expr(expr)?;
                self.buf.push(')');
                Ok(())
            }
            NodeKind::Propagate { expr } => {
                // Python doesn't have `?`; just emit the expression.
                self.emit_expr(expr)?;
                Ok(())
            }
            NodeKind::Range { lo, hi, inclusive } => {
                self.buf.push_str("range(");
                self.emit_expr(lo)?;
                self.buf.push_str(", ");
                self.emit_expr(hi)?;
                if *inclusive {
                    self.buf.push_str(" + 1");
                }
                self.buf.push(')');
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
                    .join(".");
                if let Some(sp) = spread {
                    // Spread: create dict, update, then construct
                    self.buf.push_str(&format!("{type_name}(**{{**vars("));
                    self.emit_expr(sp)?;
                    self.buf.push_str("), ");
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        let _ = write!(self.buf, "\"{}\": ", to_snake_case(&f.name.name));
                        if let Some(val) = &f.value {
                            self.emit_expr(val)?;
                        } else {
                            self.buf.push_str(&to_snake_case(&f.name.name));
                        }
                    }
                    self.buf.push_str("})");
                } else {
                    self.buf.push_str(&type_name);
                    self.buf.push('(');
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        let _ = write!(self.buf, "{}=", to_snake_case(&f.name.name));
                        if let Some(val) = &f.value {
                            self.emit_expr(val)?;
                        } else {
                            self.buf.push_str(&to_snake_case(&f.name.name));
                        }
                    }
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
                self.buf.push('{');
                for (i, entry) in entries.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(&entry.key)?;
                    self.buf.push_str(": ");
                    self.emit_expr(&entry.value)?;
                }
                self.buf.push('}');
                Ok(())
            }
            NodeKind::SetLiteral { elems } => {
                if elems.is_empty() {
                    self.buf.push_str("set()");
                } else {
                    self.buf.push('{');
                    for (i, e) in elems.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.emit_expr(e)?;
                    }
                    self.buf.push('}');
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
                let has_newline = parts.iter().any(|p| matches!(p,
                    AirInterpolationPart::Literal(s) if s.contains('\n')
                ));
                if has_newline {
                    self.buf.push_str("f\"\"\"");
                } else {
                    self.buf.push_str("f\"");
                }
                for part in parts {
                    match part {
                        AirInterpolationPart::Literal(s) => {
                            if has_newline {
                                self.buf.push_str(&escape_fstring_triple(s));
                            } else {
                                self.buf.push_str(&escape_fstring(s));
                            }
                        }
                        AirInterpolationPart::Expr(expr) => {
                            self.buf.push('{');
                            self.emit_expr(expr)?;
                            self.buf.push('}');
                        }
                    }
                }
                if has_newline {
                    self.buf.push_str("\"\"\"");
                } else {
                    self.buf.push('"');
                }
                Ok(())
            }
            NodeKind::Placeholder => {
                self.buf.push('_');
                Ok(())
            }
            NodeKind::Unreachable => {
                self.buf.push_str("raise RuntimeError(\"unreachable\")");
                Ok(())
            }
            NodeKind::ResultConstruct { variant, value } => {
                match variant {
                    ResultVariant::Ok => {
                        self.buf.push_str("{\"_tag\": \"Ok\", \"value\": ");
                        if let Some(v) = value {
                            self.emit_expr(v)?;
                        } else {
                            self.buf.push_str("None");
                        }
                        self.buf.push('}');
                    }
                    ResultVariant::Err => {
                        self.buf.push_str("{\"_tag\": \"Err\", \"error\": ");
                        if let Some(v) = value {
                            self.emit_expr(v)?;
                        } else {
                            self.buf.push_str("None");
                        }
                        self.buf.push('}');
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
                self.emit_block_as_expr(then_block)?;
                self.buf.push_str(" if ");
                self.emit_expr(condition)?;
                self.buf.push_str(" else ");
                if let Some(eb) = else_block {
                    self.emit_block_as_expr(eb)?;
                } else {
                    self.buf.push_str("None");
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::Block { stmts, tail } => {
                // Blocks in expression position: emit last expression.
                // Python doesn't have IIFEs like JS; if there are stmts we just
                // emit the tail (best effort).
                if stmts.is_empty() {
                    if let Some(t) = tail {
                        return self.emit_expr(t);
                    }
                }
                // Fallback: wrap in a lambda (only works for simple cases).
                self.buf.push_str("(lambda: ");
                if let Some(t) = tail {
                    self.emit_expr(t)?;
                } else {
                    self.buf.push_str("None");
                }
                self.buf.push_str(")()");
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => {
                // Match in expression position: not directly supported in Python.
                // Emit as IIFE-like lambda with internal match.
                // For simplicity, try to emit as a series of ternary if-else.
                self.buf.push_str("(lambda __v: ");
                self.emit_match_expr(scrutinee, arms)?;
                self.buf.push_str(")(");
                self.emit_expr(scrutinee)?;
                self.buf.push(')');
                Ok(())
            }
            // Ownership nodes: erase in Python.
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
            // Type expressions: erased in Python expression context.
            NodeKind::TypeNamed { .. }
            | NodeKind::TypeTuple { .. }
            | NodeKind::TypeFunction { .. }
            | NodeKind::TypeOptional { .. }
            | NodeKind::TypeSelf => {
                self.buf.push_str("# type");
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
                self.buf.push_str("# error");
                Ok(())
            }
            _ => {
                self.buf.push_str("# unsupported");
                Ok(())
            }
        }
    }

    // ── Match → match/case (Python 3.10+) ───────────────────────────────────

    fn emit_match(&mut self, scrutinee: &AIRNode, arms: &[AIRNode]) -> Result<(), CodegenError> {
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}match ");
        self.emit_expr(scrutinee)?;
        self.buf.push_str(":\n");
        self.indent += 1;
        for arm in arms {
            self.emit_match_arm(arm)?;
        }
        self.indent -= 1;
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
            let _ = write!(self.buf, "{ind}case ");
            self.emit_pattern(pattern)?;
            if let Some(g) = guard {
                self.buf.push_str(" if ");
                self.emit_expr(g)?;
            }
            self.buf.push_str(":\n");
            self.indent += 1;
            self.emit_block_body(body)?;
            self.indent -= 1;
        }
        Ok(())
    }

    fn emit_pattern(&mut self, pat: &AIRNode) -> Result<(), CodegenError> {
        match &pat.kind {
            NodeKind::WildcardPat => {
                self.buf.push('_');
            }
            NodeKind::BindPat { name, .. } => {
                self.buf.push_str(&to_snake_case(&name.name));
            }
            NodeKind::LiteralPat { lit } => match lit {
                Literal::Int(s) => self.buf.push_str(s),
                Literal::Float(s) => self.buf.push_str(s),
                Literal::Bool(b) => self.buf.push_str(if *b { "True" } else { "False" }),
                Literal::Char(s) => {
                    self.buf.push('\'');
                    self.buf.push_str(s);
                    self.buf.push('\'');
                }
                Literal::String(s) => {
                    self.buf.push('"');
                    self.buf.push_str(&escape_py_string(s));
                    self.buf.push('"');
                }
                Literal::Unit => self.buf.push_str("None"),
            },
            NodeKind::ConstructorPat { path, fields } => {
                let variant_name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("_");
                if fields.is_empty() {
                    let _ = write!(self.buf, "{variant_name}()");
                } else {
                    let field_pats: Vec<String> = fields
                        .iter()
                        .enumerate()
                        .map(|(i, f)| {
                            let name = self.pattern_to_binding_name(f);
                            format!("_{i}={name}")
                        })
                        .collect();
                    let _ = write!(self.buf, "{variant_name}({})", field_pats.join(", "));
                }
            }
            NodeKind::RecordPat { path, fields, .. } => {
                let type_name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("_");
                let field_pats: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let field_name = to_snake_case(&f.name.name);
                        if let Some(pat) = &f.pattern {
                            let binding = self.pattern_to_binding_name(pat);
                            format!("{field_name}={binding}")
                        } else {
                            field_name
                        }
                    })
                    .collect();
                let _ = write!(self.buf, "{type_name}({})", field_pats.join(", "));
            }
            NodeKind::TuplePat { elems } => {
                self.buf.push('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_pattern(e)?;
                }
                if elems.len() == 1 {
                    self.buf.push(',');
                }
                self.buf.push(')');
            }
            _ => {
                self.buf.push('_');
            }
        }
        Ok(())
    }

    /// Emit a match expression as nested ternary (best effort for expression context).
    fn emit_match_expr(
        &mut self,
        _scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        // Simple fallback: just emit first arm body or None
        for (i, arm) in arms.iter().enumerate() {
            if let NodeKind::MatchArm { body, pattern, .. } = &arm.kind {
                if i > 0 {
                    self.buf.push_str(" if False else ");
                }
                if matches!(pattern.kind, NodeKind::WildcardPat) || i == arms.len() - 1 {
                    self.emit_block_as_expr(body)?;
                    return Ok(());
                }
                self.emit_block_as_expr(body)?;
            }
        }
        self.buf.push_str("None");
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

    // ── Type emission ───────────────────────────────────────────────────────

    fn type_to_py(&self, node: &AIRNode) -> String {
        match &node.kind {
            NodeKind::TypeNamed { path, args } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let py_name = self.map_type_name(&name);
                if args.is_empty() {
                    py_name
                } else {
                    let arg_strs: Vec<String> = args.iter().map(|a| self.type_to_py(a)).collect();
                    format!("{py_name}[{}]", arg_strs.join(", "))
                }
            }
            NodeKind::TypeTuple { elems } => {
                let elem_strs: Vec<String> = elems.iter().map(|e| self.type_to_py(e)).collect();
                format!("tuple[{}]", elem_strs.join(", "))
            }
            NodeKind::TypeFunction { params, ret, .. } => {
                let param_strs: Vec<String> = params.iter().map(|p| self.type_to_py(p)).collect();
                format!(
                    "Callable[[{}], {}]",
                    param_strs.join(", "),
                    self.type_to_py(ret)
                )
            }
            NodeKind::TypeOptional { inner } => {
                format!("{} | None", self.type_to_py(inner))
            }
            NodeKind::TypeSelf => "Self".into(),
            _ => "Any".into(),
        }
    }

    fn map_type_name(&self, name: &str) -> String {
        match name {
            "Int" => "int".into(),
            "Float" => "float".into(),
            "Bool" => "bool".into(),
            "String" => "str".into(),
            "Void" | "Unit" => "None".into(),
            "List" => "list".into(),
            "Map" => "dict".into(),
            "Set" => "set".into(),
            "Any" => "Any".into(),
            "Never" => "Never".into(),
            other => other.into(),
        }
    }

    fn ast_type_to_py(&self, ty: &TypeExpr) -> String {
        match ty {
            TypeExpr::Named { path, args, .. } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let py_name = self.map_type_name(&name);
                if args.is_empty() {
                    py_name
                } else {
                    let arg_strs: Vec<String> =
                        args.iter().map(|a| self.ast_type_to_py(a)).collect();
                    format!("{py_name}[{}]", arg_strs.join(", "))
                }
            }
            TypeExpr::Tuple { elems, .. } => {
                let elem_strs: Vec<String> = elems.iter().map(|e| self.ast_type_to_py(e)).collect();
                format!("tuple[{}]", elem_strs.join(", "))
            }
            TypeExpr::Function { params, ret, .. } => {
                let param_strs: Vec<String> =
                    params.iter().map(|p| self.ast_type_to_py(p)).collect();
                format!(
                    "Callable[[{}], {}]",
                    param_strs.join(", "),
                    self.ast_type_to_py(ret)
                )
            }
            TypeExpr::Optional { inner, .. } => {
                format!("{} | None", self.ast_type_to_py(inner))
            }
            TypeExpr::SelfType { .. } => "Self".into(),
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    /// Scan a sequence of block statements and return the set of bound names
    /// that are later `await`ed as bare identifiers within the same block.
    /// The caller wraps those LetBindings' Call values in `asyncio.create_task`.
    ///
    /// Only direct `let name = call(...)` bindings qualify. Non-call RHS are
    /// skipped (not awaitable work we can parallelise). The binding must be
    /// awaited in the same flat block — nested scopes are ignored because we
    /// can't prove the binding is still live once control leaves the block.
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
                    let py_name = to_snake_case(&name.name);
                    if matches!(&value.kind, NodeKind::Call { .. })
                        && awaited.contains(&py_name)
                    {
                        out.insert(py_name);
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

    fn emit_block_body(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::Block { stmts, tail } = &node.kind {
            if stmts.is_empty() && tail.is_none() {
                self.writeln("pass");
                return Ok(());
            }
            // Concurrent-pattern detection: names bound in this block whose
            // Call RHS should be scheduled as tasks because the same name is
            // later `await`ed in the same block. Python coroutines don't run
            // until awaited, so without this, independent async calls
            // serialise — wrapping with `asyncio.create_task` preserves the
            // author's concurrent intent (JS/TS get this for free because
            // Promises are eager).
            let task_bindings = Self::collect_task_bindings(stmts);
            let prev = std::mem::replace(&mut self.task_bound_names, task_bindings);
            for s in stmts {
                self.emit_node(s)?;
            }
            self.task_bound_names = prev;
            if let Some(t) = tail {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}return ");
                self.emit_expr(t)?;
                self.buf.push('\n');
            }
        } else {
            // Single expression as body.
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}return ");
            self.emit_expr(node)?;
            self.buf.push('\n');
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
            NodeKind::RecordPat { fields, .. } => {
                // Python doesn't have destructuring; use first field name or underscore
                fields
                    .first()
                    .map(|f| to_snake_case(&f.name.name))
                    .unwrap_or_else(|| "_".into())
            }
            _ => "_".into(),
        }
    }

    fn pattern_to_py_binding(&self, pat: &AIRNode) -> String {
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

/// Convert a `PascalCase` or `camelCase` name to `snake_case`.
/// Extract the unqualified type name from an `impl` target AIR node.
/// Returns `None` for types that aren't simple named references
/// (tuples, function types, etc.).
fn ast_type_name(node: &AIRNode) -> Option<String> {
    if let NodeKind::TypeNamed { path, .. } = &node.kind {
        path.segments.last().map(|s| s.name.clone())
    } else {
        None
    }
}

/// Emit a Bock identifier as a Python identifier. PascalCase names are
/// preserved — they denote classes, ABC traits, or enum variant constructors,
/// all of which stay PascalCase in Python by convention.
fn identifier_to_py(s: &str) -> String {
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

fn to_snake_case(s: &str) -> String {
    // If already snake_case or a single word, return as-is
    if s.is_empty() || s == "_" {
        return s.to_string();
    }
    // Don't convert if it's already snake_case (contains underscores but no uppercase)
    if s.contains('_') && !s.chars().any(|c| c.is_uppercase()) {
        return s.to_string();
    }
    // Don't convert simple lowercase words or all-uppercase words
    if !s.chars().any(|c| c.is_uppercase()) {
        return s.to_string();
    }
    // Special case: single char
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

/// Escape special characters in a Python string literal.
fn escape_py_string(s: &str) -> String {
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

/// Escape special characters in a Python f-string.
fn escape_fstring(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escape special characters in a triple-quoted Python f-string.
/// Newlines pass through literally; quotes don't need escaping.
fn escape_fstring_triple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
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
        let gen = PyGenerator::new();
        let result = gen.generate_module(module).unwrap();
        result.files[0].content.clone()
    }

    // ── Basic tests ─────────────────────────────────────────────────────────

    #[test]
    fn implements_code_generator_trait() {
        let gen = PyGenerator::new();
        assert_eq!(gen.target().id, "python");
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
        assert!(out.contains("def answer():"), "got: {out}");
        assert!(out.contains("return 42"), "got: {out}");
    }

    #[test]
    fn function_with_params_and_type_hints() {
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
            out.contains("def add(a: int, b: int) -> int:"),
            "got: {out}"
        );
        assert!(out.contains("(a + b)"), "got: {out}");
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
        assert!(out.contains("async def fetch_data():"), "got: {out}");
        assert!(out.contains("await fetch"), "got: {out}");
    }

    #[test]
    fn effects_as_keyword_args() {
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
        assert!(
            out.contains("def process(data, *, log: Log, clock: Clock):"),
            "got: {out}"
        );
        assert!(out.contains("log.info(msg)"), "got: {out}");
    }

    #[test]
    fn record_to_dataclass() {
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
        assert!(
            out.contains("from dataclasses import dataclass"),
            "got: {out}"
        );
        assert!(out.contains("@dataclass"), "got: {out}");
        assert!(out.contains("class Point:"), "got: {out}");
        assert!(out.contains("x: float"), "got: {out}");
        assert!(out.contains("y: float"), "got: {out}");
    }

    #[test]
    fn enum_to_dataclass_variants() {
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
        assert!(
            out.contains("from dataclasses import dataclass"),
            "got: {out}"
        );
        assert!(out.contains("@dataclass"), "got: {out}");
        assert!(out.contains("class Shape_Circle:"), "got: {out}");
        assert!(out.contains("radius: float"), "got: {out}");
        assert!(out.contains("_tag: str = \"Circle\""), "got: {out}");
        assert!(out.contains("class Shape_None:"), "got: {out}");
        assert!(out.contains("_tag: str = \"None\""), "got: {out}");
    }

    #[test]
    fn match_to_match_case() {
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
        assert!(out.contains("match shape:"), "got: {out}");
        assert!(out.contains("case Shape_Circle(_0=r):"), "got: {out}");
        assert!(out.contains("case _:"), "got: {out}");
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
        assert!(out.contains("a = x"), "got: {out}");
        assert!(out.contains("b = y"), "got: {out}");
        assert!(out.contains("c = z"), "got: {out}");
    }

    #[test]
    fn string_interpolation_fstring() {
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
        assert!(out.contains("f\"Hello, {name}!\""), "got: {out}");
    }

    #[test]
    fn multiline_interpolation_uses_triple_quoted_fstring() {
        let interp = node(
            1,
            NodeKind::Interpolation {
                parts: vec![
                    AirInterpolationPart::Literal("=== ".into()),
                    AirInterpolationPart::Expr(Box::new(id_node(2, "title"))),
                    AirInterpolationPart::Literal(" ===\n".into()),
                    AirInterpolationPart::Expr(Box::new(id_node(3, "msg"))),
                    AirInterpolationPart::Literal("\n================".into()),
                ],
            },
        );
        let binding = node(
            4,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(5, "result")),
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
                name: ident("banner"),
                generic_params: vec![],
                params: vec![param_node(6, "title"), param_node(7, "msg")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(8, vec![binding], Some(id_node(9, "result")))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("f\"\"\"=== {title} ===\n{msg}\n================\"\"\""),
            "got: {out}"
        );
        // Single-line interpolation should still use regular f-string
        assert!(!out.contains("f\"Hello"), "single-line should not appear: {out}");
    }

    #[test]
    fn single_line_interpolation_still_uses_regular_fstring() {
        let interp = node(
            1,
            NodeKind::Interpolation {
                parts: vec![
                    AirInterpolationPart::Literal("Hi ".into()),
                    AirInterpolationPart::Expr(Box::new(id_node(2, "name"))),
                ],
            },
        );
        let binding = node(
            3,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(4, "greeting")),
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
                body: Box::new(block(6, vec![binding], Some(id_node(7, "greeting")))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("f\"Hi {name}\""), "got: {out}");
        assert!(!out.contains("f\"\"\""), "should not use triple quotes: {out}");
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
        assert!(out.contains("[1, 2, 3]"), "got: {out}");
        assert!(out.contains("{\"a\": 1}"), "got: {out}");
        assert!(out.contains("{1, 2}"), "got: {out}");
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
        assert!(out.contains("User(name=\"Alice\", age=30)"), "got: {out}");
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
        assert!(out.contains("if True:"), "got: {out}");
        assert!(out.contains("else:"), "got: {out}");
        assert!(out.contains("for x in items:"), "got: {out}");
        assert!(out.contains("while True:"), "got: {out}");
        assert!(out.contains("break"), "got: {out}");
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
        assert!(out.contains("lambda x: (x * 2)"), "got: {out}");
        assert!(out.contains("double(5)"), "got: {out}");
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
        // PI should become snake case, but since it's all-caps we leave it
        assert!(out.contains("= 3.14159"), "got: {out}");
        assert!(out.contains(": float"), "got: {out}");
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
        assert!(out.contains("class Person:"), "got: {out}");
        assert!(out.contains("def __init__(self, name: str):"), "got: {out}");
        assert!(out.contains("self.name = name"), "got: {out}");
        assert!(out.contains("def greet(self):"), "got: {out}");
    }

    #[test]
    fn boolean_operators() {
        let expr = node(
            1,
            NodeKind::BinaryOp {
                op: BinOp::And,
                left: Box::new(bool_lit(2, true)),
                right: Box::new(bool_lit(3, false)),
            },
        );
        let not_expr = node(
            4,
            NodeKind::UnaryOp {
                op: UnaryOp::Not,
                operand: Box::new(bool_lit(5, true)),
            },
        );
        let body = block(
            6,
            vec![
                node(
                    7,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(8, "a")),
                        ty: None,
                        value: Box::new(expr),
                    },
                ),
                node(
                    9,
                    NodeKind::LetBinding {
                        is_mut: false,
                        pattern: Box::new(bind_pat(10, "b")),
                        ty: None,
                        value: Box::new(not_expr),
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
        assert!(out.contains("(True and False)"), "got: {out}");
        assert!(out.contains("not True"), "got: {out}");
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
        assert!(
            out.contains("{\"_tag\": \"Ok\", \"value\": 42}"),
            "got: {out}"
        );
        assert!(
            out.contains("{\"_tag\": \"Err\", \"error\": \"failed\"}"),
            "got: {out}"
        );
    }

    #[test]
    fn to_snake_case_conversions() {
        assert_eq!(to_snake_case("fetchData"), "fetch_data");
        assert_eq!(to_snake_case("MyClass"), "my_class");
        assert_eq!(to_snake_case("already_snake"), "already_snake");
        assert_eq!(to_snake_case("simple"), "simple");
        assert_eq!(to_snake_case("HTMLParser"), "html_parser");
        assert_eq!(to_snake_case("x"), "x");
        assert_eq!(to_snake_case("_"), "_");
    }

    // ── End-to-end tests (python3 --check + python3 execution) ──────────────

    fn has_python3() -> bool {
        std::process::Command::new("which")
            .arg("python3")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Run generated Python through `python3 -m py_compile` for syntax validation.
    fn check_py_syntax(code: &str) -> bool {
        let dir = std::env::temp_dir();
        let path = dir.join("bock_test_output.py");
        std::fs::write(&path, code).expect("failed to write temp file");
        let result = std::process::Command::new("python3")
            .arg("-m")
            .arg("py_compile")
            .arg(&path)
            .output()
            .expect("failed to spawn python3");
        let _ = std::fs::remove_file(&path);
        result.status.success()
    }

    /// Run generated Python with `python3` and capture stdout.
    fn run_py(code: &str) -> String {
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(code)
            .output()
            .expect("failed to run python3");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    #[ignore]
    fn e2e_hello_world() {
        if !has_python3() {
            return;
        }
        let body = block(
            2,
            vec![],
            Some(node(
                3,
                NodeKind::Call {
                    callee: Box::new(id_node(4, "print")),
                    args: vec![AirArg {
                        label: None,
                        value: str_lit(5, "Hello, World!"),
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
        let full = format!("{code}\nmain()\n");
        assert!(
            check_py_syntax(&full),
            "Python syntax check failed:\n{full}"
        );
        assert_eq!(run_py(&full), "Hello, World!");
    }

    #[test]
    #[ignore]
    fn e2e_arithmetic() {
        if !has_python3() {
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
        let full = format!("{code}\nprint(calc())\n");
        assert!(
            check_py_syntax(&full),
            "Python syntax check failed:\n{full}"
        );
        assert_eq!(run_py(&full), "42");
    }

    #[test]
    #[ignore]
    fn e2e_if_else() {
        if !has_python3() {
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
                then_block: Box::new(block(
                    7,
                    vec![node(
                        8,
                        NodeKind::Return {
                            value: Some(Box::new(str_lit(9, "positive"))),
                        },
                    )],
                    None,
                )),
                else_block: Some(Box::new(block(
                    10,
                    vec![node(
                        11,
                        NodeKind::Return {
                            value: Some(Box::new(str_lit(12, "non-positive"))),
                        },
                    )],
                    None,
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
                params: vec![param_node(13, "x")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\nprint(classify(5))\nprint(classify(-1))\n");
        assert!(
            check_py_syntax(&full),
            "Python syntax check failed:\n{full}"
        );
        let output = run_py(&full);
        assert!(output.contains("positive"), "got: {output}");
        assert!(output.contains("non-positive"), "got: {output}");
    }

    #[test]
    #[ignore]
    fn e2e_for_loop() {
        if !has_python3() {
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
        let full = format!("{code}\nprint(total())\n");
        assert!(
            check_py_syntax(&full),
            "Python syntax check failed:\n{full}"
        );
        assert_eq!(run_py(&full), "6");
    }

    #[test]
    #[ignore]
    fn e2e_dataclass() {
        if !has_python3() {
            return;
        }
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
        let code = gen(&module(vec![], vec![rec]));
        let full = format!("{code}\np = Point(x=1.0, y=2.0)\nprint(f\"{{p.x}}, {{p.y}}\")\n");
        assert!(
            check_py_syntax(&full),
            "Python syntax check failed:\n{full}"
        );
        let output = run_py(&full);
        assert!(output.contains("1.0, 2.0"), "got: {output}");
    }

    #[test]
    #[ignore]
    fn e2e_match_statement() {
        if !has_python3() {
            return;
        }
        // match on literal values
        let scrutinee = id_node(10, "x");
        let arms = vec![
            node(
                11,
                NodeKind::MatchArm {
                    pattern: Box::new(node(
                        12,
                        NodeKind::LiteralPat {
                            lit: Literal::Int("1".into()),
                        },
                    )),
                    guard: None,
                    body: Box::new(block(
                        13,
                        vec![node(
                            14,
                            NodeKind::Return {
                                value: Some(Box::new(str_lit(15, "one"))),
                            },
                        )],
                        None,
                    )),
                },
            ),
            node(
                16,
                NodeKind::MatchArm {
                    pattern: Box::new(node(17, NodeKind::WildcardPat)),
                    guard: None,
                    body: Box::new(block(
                        18,
                        vec![node(
                            19,
                            NodeKind::Return {
                                value: Some(Box::new(str_lit(20, "other"))),
                            },
                        )],
                        None,
                    )),
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
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("describe"),
                generic_params: vec![],
                params: vec![param_node(2, "x")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(3, vec![match_stmt], None)),
            },
        );
        let code = gen(&module(vec![], vec![f]));
        let full = format!("{code}\nprint(describe(1))\nprint(describe(99))\n");
        assert!(
            check_py_syntax(&full),
            "Python syntax check failed:\n{full}"
        );
        let output = run_py(&full);
        assert!(output.contains("one"), "got: {output}");
        assert!(output.contains("other"), "got: {output}");
    }

    // ── Prelude function mapping tests ──────────────────────────────────────

    /// Helper: generate Python for a module with a `main` function containing a single call.
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

    /// Helper: generate Python for a nullary prelude call (no args).
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
    fn prelude_println_maps_to_print() {
        let out = gen_prelude_call("println", str_lit(12, "hello"));
        assert!(
            out.contains("print("),
            "println should map to print, got: {out}"
        );
        assert!(
            !out.contains("println("),
            "should not emit bare println(, got: {out}"
        );
    }

    #[test]
    fn prelude_print_maps_to_print_no_newline() {
        let out = gen_prelude_call("print", str_lit(12, "hello"));
        assert!(
            out.contains("print(") && out.contains("end=\"\""),
            "print should map to print with end=\"\", got: {out}"
        );
    }

    #[test]
    fn prelude_debug_maps_to_repr() {
        let out = gen_prelude_call("debug", str_lit(12, "val"));
        assert!(
            out.contains("print(repr("),
            "debug should map to print(repr(...)), got: {out}"
        );
    }

    #[test]
    fn prelude_assert_maps_to_assert() {
        let out = gen_prelude_call("assert", bool_lit(12, true));
        assert!(
            out.contains("assert "),
            "assert should map to Python assert, got: {out}"
        );
        assert!(
            !out.contains("assert("),
            "should not emit assert as function call, got: {out}"
        );
    }

    #[test]
    fn prelude_todo_maps_to_not_implemented_error() {
        let out = gen_prelude_call_no_args("todo");
        assert!(
            out.contains("raise NotImplementedError()"),
            "todo should map to raise NotImplementedError, got: {out}"
        );
    }

    #[test]
    fn prelude_unreachable_maps_to_runtime_error() {
        let out = gen_prelude_call_no_args("unreachable");
        assert!(
            out.contains("raise RuntimeError(\"unreachable\")"),
            "unreachable should map to raise RuntimeError, got: {out}"
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

    // ── Effect declaration tests ────────────────────────────────────────────

    #[test]
    fn effect_decl_becomes_abc() {
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
                            params: vec![
                                typed_param_node(3, "level", "String"),
                                typed_param_node(4, "msg", "String"),
                            ],
                            return_type: Some(Box::new(node(
                                5,
                                NodeKind::TypeNamed {
                                    path: type_path(&["Void"]),
                                    args: vec![],
                                },
                            ))),
                            effect_clause: vec![],
                            where_clause: vec![],
                            body: Box::new(block(6, vec![], None)),
                        },
                    ),
                    node(
                        7,
                        NodeKind::FnDecl {
                            annotations: vec![],
                            visibility: Visibility::Public,
                            is_async: false,
                            name: ident("flush"),
                            generic_params: vec![],
                            params: vec![],
                            return_type: Some(Box::new(node(
                                8,
                                NodeKind::TypeNamed {
                                    path: type_path(&["Void"]),
                                    args: vec![],
                                },
                            ))),
                            effect_clause: vec![],
                            where_clause: vec![],
                            body: Box::new(block(9, vec![], None)),
                        },
                    ),
                ],
            },
        );
        let out = gen(&module(vec![], vec![effect]));
        assert!(
            out.contains("from abc import ABC, abstractmethod"),
            "got: {out}"
        );
        assert!(out.contains("class Logger(ABC):"), "got: {out}");
        assert!(out.contains("@abstractmethod"), "got: {out}");
        assert!(
            out.contains("def log(self, level: str, msg: str) -> None:"),
            "got: {out}"
        );
        assert!(out.contains("def flush(self) -> None:"), "got: {out}");
        assert!(out.contains("        ..."), "got: {out}");
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
        assert!(out.contains("class Empty(ABC):"), "got: {out}");
        assert!(out.contains("    pass"), "got: {out}");
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
        // Python: inner(logger=__logger_h<N>) — handling blocks get a fresh
        // numeric suffix so nested blocks don't shadow each other.
        assert!(
            out.contains("inner(logger=__logger_h"),
            "handling block should pass handler to effectful call, got: {out}"
        );
        // Handler constructors are PascalCase in Python — they name a class.
        assert!(
            out.contains(": Logger = StdoutLogger()"),
            "handling block should instantiate handler, got: {out}"
        );
    }

    // ── Async / concurrent patterns ────────────────────────────────────────

    #[test]
    fn async_function_imports_asyncio() {
        let body = block(3, vec![], None);
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
        assert!(out.contains("import asyncio"), "got: {out}");
        assert!(out.contains("async def tick():"), "got: {out}");
    }

    #[test]
    fn sync_module_has_no_asyncio_import() {
        let body = block(3, vec![], Some(int_lit(4, "1")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("one"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(!out.contains("import asyncio"), "got: {out}");
    }

    #[test]
    fn entry_invocation_async_main_python() {
        let inv = PyGenerator::new().entry_invocation(true).unwrap();
        assert!(inv.contains("asyncio.run(main())"), "got: {inv}");
    }

    #[test]
    fn entry_invocation_sync_main_python() {
        let inv = PyGenerator::new().entry_invocation(false).unwrap();
        assert_eq!(inv, "if __name__ == \"__main__\":\n    main()\n");
    }

    #[test]
    fn generate_project_async_main_uses_asyncio_run() {
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
        let gen = PyGenerator::new();
        let out = gen.generate_project(&[&m]).unwrap();
        let src = &out.files[0].content;
        assert!(src.contains("import asyncio"), "got: {src}");
        assert!(src.contains("async def main():"), "got: {src}");
        assert!(src.contains("asyncio.run(main())"), "got: {src}");
    }

    #[test]
    fn concurrent_pattern_wraps_in_create_task() {
        // Block:
        //   let a = task1()
        //   let b = task2()
        //   let ra = await a
        //   let rb = await b
        //   return ra
        let call_task = |id: u32, name: &str| {
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
                let_stmt(20, "a", call_task(21, "task1")),
                let_stmt(30, "b", call_task(31, "task2")),
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
            out.contains("a = asyncio.create_task(task1())"),
            "task1 should be scheduled as a task, got: {out}"
        );
        assert!(
            out.contains("b = asyncio.create_task(task2())"),
            "task2 should be scheduled as a task, got: {out}"
        );
        assert!(out.contains("ra = (await a)"), "got: {out}");
        assert!(out.contains("rb = (await b)"), "got: {out}");
    }

    #[test]
    fn sequential_await_no_task_wrapping() {
        // `let a = await task1()` directly awaits — no task wrap needed.
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
            !out.contains("create_task"),
            "sequential await should not wrap in create_task, got: {out}"
        );
        assert!(out.contains("a = (await task1())"), "got: {out}");
    }

    #[test]
    fn non_call_rhs_not_wrapped_in_task() {
        // `let a = 42 ; ... await a` — RHS is not a Call, so we can't wrap.
        let let_stmt = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "a")),
                ty: None,
                value: Box::new(int_lit(12, "42")),
            },
        );
        let await_a = node(
            20,
            NodeKind::Await {
                expr: Box::new(id_node(21, "a")),
            },
        );
        let body = block(30, vec![let_stmt], Some(await_a));
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
        assert!(!out.contains("create_task"), "got: {out}");
        assert!(out.contains("a = 42"), "got: {out}");
    }
}
