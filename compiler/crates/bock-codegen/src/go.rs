//! Go code generator — rule-based (Tier 2) transpilation from AIR to Go.
//!
//! Handles all capability gaps:
//! - Records → structs
//! - Traits → interfaces
//! - Algebraic types → structs with tag field + type switch
//! - Pattern matching → switch/type-switch/if-else chains
//! - Effects → interface parameters
//! - Ownership → erased (Go is GC)
//! - Generics → Go type parameters (Go 1.18+)
//! - Concurrency → goroutines/channels
//! - Error handling → `(value, error)` return tuples
//! - String interpolation → `fmt.Sprintf`

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::PathBuf;

use bock_air::{AIRNode, AirInterpolationPart, EnumVariantPayload, NodeKind, ResultVariant};
use bock_ast::{AssignOp, BinOp, ImportItems, Literal, TypeExpr, UnaryOp, Visibility};
use bock_types::AIRModule;

use crate::error::CodegenError;
use crate::generator::{CodeGenerator, GeneratedCode, OutputFile, SourceMap};
use crate::profile::TargetProfile;

/// Conservative module scan for `Channel` / `spawn` references.
fn go_module_uses_concurrency(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Channel\"") || s.contains("\"spawn\"")
    })
}

/// Runtime helpers for Bock concurrency in Go. A Channel is a wrapper
/// over `chan interface{}` so the generic shape is simple; `spawn`
/// launches a goroutine whose result is piped through a 1-element
/// buffered channel (matching the existing Go async-fn wrapper
/// convention — cf. F.4.3).
const CONCURRENCY_RUNTIME_GO: &str = "\
// ── Bock concurrency runtime ──
type __bockChannel struct {
\tq chan interface{}
}

func __bockChannelNew() (*__bockChannel, *__bockChannel) {
\tc := &__bockChannel{q: make(chan interface{}, 1024)}
\treturn c, c
}
func (c *__bockChannel) send(v interface{}) { c.q <- v }
func (c *__bockChannel) recv() interface{}  { return <-c.q }
func (c *__bockChannel) close()              {}

// __bockSpawn launches the passed channel-returning async computation.
// In practice the Go async-fn lowerer already wraps bodies in goroutines,
// so this is the identity on a receive channel.
func __bockSpawn(ch interface{}) interface{} { return ch }
";

/// Go code generator implementing the `CodeGenerator` trait.
#[derive(Debug)]
pub struct GoGenerator {
    profile: TargetProfile,
}

impl GoGenerator {
    /// Creates a new Go code generator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: TargetProfile::go(),
        }
    }
}

impl Default for GoGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeGenerator for GoGenerator {
    fn target(&self) -> &TargetProfile {
        &self.profile
    }

    fn generate_module(&self, module: &AIRModule) -> Result<GeneratedCode, CodegenError> {
        let mut ctx = GoEmitCtx::new();
        ctx.collect_async_fns(module);
        ctx.emit_node(module)?;
        let content = ctx.finish();
        let source_map = SourceMap {
            generated_file: "output.go".to_string(),
            ..Default::default()
        };
        Ok(GeneratedCode {
            files: vec![OutputFile {
                path: PathBuf::from("output.go"),
                content,
            }],
            source_map: Some(source_map),
        })
    }

    fn generate_project(&self, modules: &[&AIRModule]) -> Result<GeneratedCode, CodegenError> {
        let mut combined_body = String::new();
        let mut needs_fmt = false;
        let mut needs_sync = false;
        let mut needs_time = false;

        // Pre-scan async fns across all modules so cross-module calls
        // between async functions route through the Async-suffix wrappers.
        let mut global_async_fns: HashSet<String> = HashSet::new();
        for module in modules {
            if let NodeKind::Module { items, .. } = &module.kind {
                for item in items {
                    if let NodeKind::FnDecl {
                        is_async: true,
                        name,
                        ..
                    } = &item.kind
                    {
                        global_async_fns.insert(name.name.clone());
                    }
                }
            }
        }
        for module in modules {
            let mut ctx = GoEmitCtx::new();
            ctx.async_fns = global_async_fns.clone();
            ctx.emit_node(module)?;
            let (body, fmt, sync, time) = ctx.into_parts();
            needs_fmt |= fmt;
            needs_sync |= sync;
            needs_time |= time;
            if !combined_body.is_empty() && !body.is_empty() {
                combined_body.push('\n');
            }
            combined_body.push_str(&body);
        }

        // Build single preamble with merged imports
        let mut header = "package main\n".to_string();
        let mut imports = Vec::new();
        if needs_fmt {
            imports.push("\"fmt\"");
        }
        if needs_sync {
            imports.push("\"sync\"");
        }
        if needs_time {
            imports.push("\"time\"");
        }
        if !imports.is_empty() {
            if imports.len() == 1 {
                header.push_str(&format!("\nimport {}\n", imports[0]));
            } else {
                header.push_str("\nimport (\n");
                for imp in &imports {
                    header.push_str(&format!("\t{imp}\n"));
                }
                header.push_str(")\n");
            }
        }
        header.push('\n');
        header.push_str(&combined_body);

        let source_map = SourceMap {
            generated_file: "output.go".to_string(),
            ..Default::default()
        };
        Ok(GeneratedCode {
            files: vec![OutputFile {
                path: PathBuf::from("output.go"),
                content: header,
            }],
            source_map: Some(source_map),
        })
    }
}

// ─── Emission context ────────────────────────────────────────────────────────

/// Internal state for Go emission.
struct GoEmitCtx {
    buf: String,
    indent: usize,
    /// Track whether we need `"fmt"` import.
    needs_fmt_import: bool,
    /// Track whether we need `"sync"` import.
    needs_sync_import: bool,
    /// Track whether we need `"time"` import.
    needs_time_import: bool,
    /// Package name (defaults to "main").
    package_name: String,
    /// Maps effect operation name → effect type name (e.g., "log" → "Logger").
    effect_ops: HashMap<String, String>,
    /// Maps effect type name → current handler variable name in scope.
    current_handler_vars: HashMap<String, String>,
    /// Maps function name → effect type names from its `with` clause.
    fn_effects: HashMap<String, Vec<String>>,
    /// Maps composite effect name → component effect names.
    composite_effects: HashMap<String, Vec<String>>,
    /// Names of public (exported) functions — emitted as PascalCase at call sites.
    public_fns: HashSet<String>,
    /// Names of effect operations that return Void — emitted without a `return` prefix.
    void_effect_ops: HashSet<String>,
    /// Bock names of top-level async functions. Call-site identifiers in this
    /// set are rewritten to `fnNameAsync` so callers receive the channel form
    /// of the function (goroutine started, `<-chan T` returned). Without this,
    /// `await task()` would try to receive from a `T`, not `chan T`.
    async_fns: HashSet<String>,
}

impl GoEmitCtx {
    fn new() -> Self {
        Self {
            buf: String::with_capacity(4096),
            indent: 0,
            needs_fmt_import: false,
            needs_sync_import: false,
            needs_time_import: false,
            package_name: "main".into(),
            effect_ops: HashMap::new(),
            current_handler_vars: HashMap::new(),
            fn_effects: HashMap::new(),
            composite_effects: HashMap::new(),
            public_fns: HashSet::new(),
            void_effect_ops: HashSet::new(),
            async_fns: HashSet::new(),
        }
    }

    /// Pre-scan the module for top-level `async fn` names. Must be populated
    /// before any Call node is emitted so the Async-suffix rewrite at call
    /// sites covers both forward and backward references within the module.
    fn collect_async_fns(&mut self, module: &AIRNode) {
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                if let NodeKind::FnDecl {
                    is_async: true,
                    name,
                    ..
                } = &item.kind
                {
                    self.async_fns.insert(name.name.clone());
                }
            }
        }
    }

    /// Returns `true` if the AIR type node represents `Void` or `Unit`.
    fn is_void_type(node: &AIRNode) -> bool {
        if let NodeKind::TypeNamed { path, .. } = &node.kind {
            if let Some(last) = path.segments.last() {
                return last.name == "Void" || last.name == "Unit";
            }
        }
        if let NodeKind::TypeTuple { elems } = &node.kind {
            return elems.is_empty();
        }
        false
    }

    /// Returns the emitted body and import flags without building the preamble.
    fn into_parts(self) -> (String, bool, bool, bool) {
        (
            self.buf,
            self.needs_fmt_import,
            self.needs_sync_import,
            self.needs_time_import,
        )
    }

    fn finish(self) -> String {
        let mut header = format!("package {}\n", self.package_name);
        let mut imports = Vec::new();
        if self.needs_fmt_import {
            imports.push("\"fmt\"");
        }
        if self.needs_sync_import {
            imports.push("\"sync\"");
        }
        if self.needs_time_import {
            imports.push("\"time\"");
        }
        if !imports.is_empty() {
            if imports.len() == 1 {
                header.push_str(&format!("\nimport {}\n", imports[0]));
            } else {
                header.push_str("\nimport (\n");
                for imp in &imports {
                    header.push_str(&format!("\t{imp}\n"));
                }
                header.push_str(")\n");
            }
        }
        header.push('\n');
        header.push_str(&self.buf);
        header
    }

    fn indent_str(&self) -> String {
        "\t".repeat(self.indent)
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

    /// Map Bock prelude functions to Go equivalents.
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
                self.needs_fmt_import = true;
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("fmt.Println({a})")
            }
            "print" => {
                self.needs_fmt_import = true;
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("fmt.Print({a})")
            }
            "debug" => {
                self.needs_fmt_import = true;
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("fmt.Printf(\"%+v\\n\", {a})")
            }
            "assert" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("if !{a} {{ panic(\"assertion failed\") }}")
            }
            "todo" => "panic(\"not implemented\")".to_string(),
            "unreachable" => "panic(\"unreachable\")".to_string(),
            "sleep" => {
                // sleep(d) returns a chan struct{} so `await` (= `<-ch`) works
                // uniformly. The goroutine holds for `d` nanos, then closes ch.
                self.needs_time_import = true;
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("(func() <-chan struct{{}} {{ __ch := make(chan struct{{}}); go func() {{ time.Sleep(time.Duration({a})); close(__ch) }}(); return __ch }})()")
            }
            _ => return Ok(None),
        };
        Ok(Some(code))
    }

    /// Recognise `Duration.xxx(...)` / `Instant.xxx(...)` associated-function
    /// calls and emit inline Go code. Duration values are `int64` nanoseconds
    /// (matching `time.Duration`); Instants are `time.Time` (monotonic via
    /// `time.Now()`).
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
            ("Duration", "zero") => "int64(0)".to_string(),
            ("Duration", "nanos") => format!("int64({})", arg0()),
            ("Duration", "micros") => format!("(int64({}) * 1000)", arg0()),
            ("Duration", "millis") => format!("(int64({}) * 1000000)", arg0()),
            ("Duration", "seconds") => format!("(int64({}) * 1000000000)", arg0()),
            ("Duration", "minutes") => format!("(int64({}) * 60000000000)", arg0()),
            ("Duration", "hours") => format!("(int64({}) * 3600000000000)", arg0()),
            ("Instant", "now") => {
                self.needs_time_import = true;
                "time.Now()".to_string()
            }
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
    /// channel value. Emits calls into the Go runtime helper code
    /// (injected at top-of-module).
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
            "as_millis" => format!("(({recv_str}) / 1000000)"),
            "as_seconds" => format!("(({recv_str}) / 1000000000)"),
            "is_zero" => format!("(({recv_str}) == 0)"),
            "is_negative" => format!("(({recv_str}) < 0)"),
            "abs" => {
                format!("(func(__d int64) int64 {{ if __d < 0 {{ return -__d }}; return __d }}({recv_str}))")
            }
            "elapsed" => {
                self.needs_time_import = true;
                format!("int64(time.Since({recv_str}))")
            }
            "duration_since" => {
                let other = arg_strs.first().cloned().unwrap_or_default();
                format!("int64(({recv_str}).Sub({other}))")
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
                if go_module_uses_concurrency(items) {
                    self.buf.push_str(CONCURRENCY_RUNTIME_GO);
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
                    .join("/");
                match items {
                    ImportItems::Module => {
                        self.writeln(&format!("import \"{path_str}\""));
                    }
                    ImportItems::Named(names) => {
                        // Go imports are module-level; named imports are expressed as qualified access.
                        let _ = names;
                        self.writeln(&format!("import \"{path_str}\""));
                    }
                    ImportItems::Glob => {
                        self.writeln(&format!("import . \"{path_str}\""));
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
            ),
            NodeKind::RecordDecl {
                name,
                generic_params,
                fields,
                ..
            } => {
                let type_params = self.format_generic_params(generic_params);
                self.writeln(&format!("type {}{type_params} struct {{", name.name));
                self.indent += 1;
                for f in fields {
                    let type_str = self.ast_type_to_go(&f.ty);
                    self.writeln(&format!("{}\t{type_str}", to_pascal_case(&f.name.name)));
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::EnumDecl {
                name,
                generic_params,
                variants,
                ..
            } => {
                // Go doesn't have algebraic types; use interface + variant structs.
                let type_params = self.format_generic_params(generic_params);
                // Emit the interface (sealed by convention).
                self.writeln(&format!("type {}{type_params} interface {{", name.name));
                self.indent += 1;
                self.writeln(&format!("is{}()", name.name));
                self.indent -= 1;
                self.writeln("}");
                // Emit each variant as a struct implementing the interface.
                for variant in variants {
                    self.buf.push('\n');
                    self.emit_enum_variant(&name.name, generic_params, variant)?;
                }
                Ok(())
            }
            NodeKind::ClassDecl {
                name,
                generic_params,
                fields,
                methods,
                ..
            } => {
                // Emit struct.
                let type_params = self.format_generic_params(generic_params);
                self.writeln(&format!("type {}{type_params} struct {{", name.name));
                self.indent += 1;
                for f in fields {
                    let type_str = self.ast_type_to_go(&f.ty);
                    self.writeln(&format!("{}\t{type_str}", to_pascal_case(&f.name.name)));
                }
                self.indent -= 1;
                self.writeln("}");
                // Constructor function.
                if !fields.is_empty() {
                    self.buf.push('\n');
                    let params: Vec<String> = fields
                        .iter()
                        .map(|f| {
                            let fname = to_camel_case(&f.name.name);
                            let type_str = self.ast_type_to_go(&f.ty);
                            format!("{fname} {type_str}")
                        })
                        .collect();
                    self.writeln(&format!(
                        "func New{}({}) *{} {{",
                        name.name,
                        params.join(", "),
                        name.name
                    ));
                    self.indent += 1;
                    let field_inits: Vec<String> = fields
                        .iter()
                        .map(|f| {
                            format!(
                                "{}: {},",
                                to_pascal_case(&f.name.name),
                                to_camel_case(&f.name.name)
                            )
                        })
                        .collect();
                    self.writeln(&format!("return &{} {{", name.name));
                    self.indent += 1;
                    for init in &field_inits {
                        self.writeln(init);
                    }
                    self.indent -= 1;
                    self.writeln("}");
                    self.indent -= 1;
                    self.writeln("}");
                }
                // Methods.
                for method in methods {
                    self.buf.push('\n');
                    self.emit_method(&name.name, method, false)?;
                }
                Ok(())
            }
            NodeKind::TraitDecl { name, methods, .. } => {
                // Traits → Go interfaces.
                self.writeln(&format!("type {} interface {{", name.name));
                self.indent += 1;
                for method in methods {
                    if let NodeKind::FnDecl {
                        name,
                        params,
                        return_type,
                        ..
                    } = &method.kind
                    {
                        let param_strs = self.collect_param_type_strs(params);
                        let is_void = return_type.as_deref().is_some_and(Self::is_void_type);
                        let ret = if is_void {
                            String::new()
                        } else {
                            return_type
                                .as_deref()
                                .map(|t| format!(" {}", self.type_to_go(t)))
                                .unwrap_or_default()
                        };
                        self.writeln(&format!(
                            "{}({}){ret}",
                            to_pascal_case(&name.name),
                            param_strs.join(", "),
                        ));
                    }
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::ImplBlock {
                target,
                methods,
                trait_path,
                ..
            } => {
                let target_name = self.type_expr_to_string(target);
                // Value receivers for trait/effect impls so `Handler{}` satisfies
                // the interface; pointer receivers for inherent `impl T { ... }`.
                let use_value_receiver = trait_path.is_some();
                for (i, method) in methods.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    self.emit_method(&target_name, method, use_value_receiver)?;
                }
                Ok(())
            }
            NodeKind::EffectDecl {
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
                    self.composite_effects.insert(name.name.clone(), comp_names);
                    return Ok(());
                }
                // Record effect operations for Call → handler.op rewriting.
                for op in operations {
                    if let NodeKind::FnDecl {
                        name: op_name,
                        return_type,
                        ..
                    } = &op.kind
                    {
                        self.effect_ops
                            .insert(op_name.name.clone(), name.name.clone());
                        if return_type.as_deref().is_some_and(Self::is_void_type) {
                            self.void_effect_ops.insert(op_name.name.clone());
                        }
                    }
                }
                // Effects → Go interfaces.
                let type_params = self.format_generic_params(generic_params);
                self.writeln(&format!("type {}{type_params} interface {{", name.name));
                self.indent += 1;
                for op in operations {
                    if let NodeKind::FnDecl {
                        name,
                        params,
                        return_type,
                        ..
                    } = &op.kind
                    {
                        let param_strs = self.collect_param_type_strs(params);
                        let is_void = return_type.as_deref().is_some_and(Self::is_void_type);
                        let ret = if is_void {
                            String::new()
                        } else {
                            return_type
                                .as_deref()
                                .map(|t| format!(" {}", self.type_to_go(t)))
                                .unwrap_or_default()
                        };
                        self.writeln(&format!(
                            "{}({}){ret}",
                            to_pascal_case(&name.name),
                            param_strs.join(", "),
                        ));
                    }
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::TypeAlias {
                name,
                generic_params,
                ..
            } => {
                let type_params = self.format_generic_params(generic_params);
                self.writeln(&format!("type {}{type_params} = interface{{}}", name.name));
                Ok(())
            }
            NodeKind::ConstDecl {
                name, value, ty, ..
            } => {
                let type_str = format!(" {}", self.type_to_go(ty));
                let ind = self.indent_str();
                let _ = write!(
                    self.buf,
                    "{ind}var {}{type_str} = ",
                    to_pascal_case(&name.name)
                );
                self.emit_expr(value)?;
                self.buf.push('\n');
                Ok(())
            }
            NodeKind::ModuleHandle { effect, handler } => {
                let effect_name = effect.segments.last().map_or("effect", |s| s.name.as_str());
                let var_name = format!("__{}", to_camel_case(effect_name));
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}var {var_name} {effect_name} = ");
                self.emit_expr(handler)?;
                self.buf.push('\n');
                // Register the module-scoped handler so effectful function
                // calls at module level pick it up.
                self.current_handler_vars
                    .insert(effect_name.to_string(), var_name);
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
            _ => {
                self.write_indent();
                self.emit_expr(node)?;
                self.buf.push('\n');
                Ok(())
            }
        }
    }

    // ── Generics ────────────────────────────────────────────────────────────

    fn format_generic_params(&self, params: &[bock_ast::GenericParam]) -> String {
        if params.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = params
            .iter()
            .map(|p| {
                if p.bounds.is_empty() {
                    format!("{} any", p.name.name)
                } else {
                    let bound_strs: Vec<String> = p
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
                    format!("{} {}", p.name.name, bound_strs.join(" | "))
                }
            })
            .collect();
        format!("[{}]", parts.join(", "))
    }

    fn format_generic_args(&self, args: &[AIRNode]) -> String {
        if args.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = args.iter().map(|a| self.type_to_go(a)).collect();
        format!("[{}]", parts.join(", "))
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
    ) -> Result<(), CodegenError> {
        let is_public = matches!(visibility, Visibility::Public);
        let fn_name = if is_public {
            to_pascal_case(name)
        } else {
            to_camel_case(name)
        };
        if is_public {
            self.public_fns.insert(name.to_string());
        }
        let type_params = self.format_generic_params(generic_params);
        let param_strs = self.collect_param_strs(params);
        let effects = self.effects_params(effect_clause);
        let mut all_params = param_strs.clone();
        all_params.extend(effects.clone());
        let is_void = return_type.is_some_and(Self::is_void_type);
        let ret = if is_void {
            String::new()
        } else {
            return_type
                .map(|t| format!(" {}", self.type_to_go(t)))
                .unwrap_or_default()
        };
        if !effect_clause.is_empty() {
            let effect_names = self.expand_effect_names(effect_clause);
            self.fn_effects.insert(name.to_string(), effect_names);
        }
        self.writeln(&format!(
            "func {fn_name}{type_params}({}){ret} {{",
            all_params.join(", "),
        ));
        self.indent += 1;
        let old_handler_vars = self.current_handler_vars.clone();
        let expanded = self.expand_effect_names(effect_clause);
        for ename in &expanded {
            self.current_handler_vars
                .insert(ename.clone(), to_camel_case(ename));
        }
        if name == "main" || is_void {
            self.emit_block_body(body)?;
        } else {
            self.emit_block_body_return(body)?;
        }
        self.current_handler_vars = old_handler_vars;
        self.indent -= 1;
        self.writeln("}");

        // Async wrapper: every `async fn` gets a companion `FnAsync` that
        // starts a goroutine and returns a buffered `<-chan T` (or
        // `<-chan struct{}` for void returns). `main` is skipped — Go's
        // entry point is always `func main()` and wrapping it would be dead
        // code the linker would complain about.
        if is_async && name != "main" {
            self.buf.push('\n');
            self.emit_async_wrapper(
                &fn_name,
                &type_params,
                params,
                return_type,
                is_void,
                &effects,
            )?;
        }
        Ok(())
    }

    /// Emit the `FnNameAsync` companion for an `async fn`. The wrapper starts
    /// a goroutine, invokes the sync body with the caller's arguments, and
    /// returns the result over a buffered channel. Callers `await`
    /// (= `<-chan T`) to observe completion.
    fn emit_async_wrapper(
        &mut self,
        sync_fn_name: &str,
        type_params: &str,
        params: &[AIRNode],
        return_type: Option<&AIRNode>,
        is_void: bool,
        effects: &[String],
    ) -> Result<(), CodegenError> {
        let async_fn_name = format!("{sync_fn_name}Async");
        let param_strs = self.collect_param_strs(params);
        let mut all_params = param_strs;
        all_params.extend(effects.iter().cloned());
        let chan_ty = if is_void {
            "struct{}".to_string()
        } else {
            return_type
                .map(|t| self.type_to_go(t))
                .unwrap_or_else(|| "interface{}".to_string())
        };
        self.writeln(&format!(
            "func {async_fn_name}{type_params}({}) <-chan {chan_ty} {{",
            all_params.join(", "),
        ));
        self.indent += 1;
        self.writeln(&format!("__ch := make(chan {chan_ty}, 1)"));
        self.writeln("go func() {");
        self.indent += 1;
        // Forward the sync function's arguments verbatim. Param names are
        // the camel-cased binding names the wrapper receives.
        let call_args: Vec<String> = params
            .iter()
            .filter_map(|p| {
                if let NodeKind::Param { pattern, .. } = &p.kind {
                    Some(to_camel_case(&self.pattern_to_binding_name(pattern)))
                } else {
                    None
                }
            })
            .chain(effects.iter().map(|e| {
                // Effects params look like `name EffectType`; recover the
                // name before the first space.
                e.split_whitespace().next().unwrap_or("").to_string()
            }))
            .collect();
        let call_site = format!("{sync_fn_name}({})", call_args.join(", "));
        if is_void {
            self.writeln(&call_site);
            self.writeln("__ch <- struct{}{}");
        } else {
            self.writeln(&format!("__ch <- {call_site}"));
        }
        self.indent -= 1;
        self.writeln("}()");
        self.writeln("return __ch");
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    fn emit_method(
        &mut self,
        receiver_type: &str,
        method: &AIRNode,
        use_value_receiver: bool,
    ) -> Result<(), CodegenError> {
        if let NodeKind::FnDecl {
            visibility,
            name,
            params,
            return_type,
            effect_clause,
            body,
            ..
        } = &method.kind
        {
            let method_name = if matches!(visibility, Visibility::Public) {
                to_pascal_case(&name.name)
            } else {
                to_camel_case(&name.name)
            };
            let receiver_var = receiver_type
                .chars()
                .next()
                .unwrap_or('r')
                .to_lowercase()
                .to_string();
            let param_strs = self.collect_param_strs(params);
            let effects = self.effects_params(effect_clause);
            let mut all_params = param_strs;
            all_params.extend(effects);
            let is_void = return_type.as_deref().is_some_and(Self::is_void_type);
            let ret = if is_void {
                String::new()
            } else {
                return_type
                    .as_deref()
                    .map(|t| format!(" {}", self.type_to_go(t)))
                    .unwrap_or_default()
            };
            let receiver_prefix = if use_value_receiver { "" } else { "*" };
            self.writeln(&format!(
                "func ({receiver_var} {receiver_prefix}{receiver_type}) {method_name}({}){ret} {{",
                all_params.join(", "),
            ));
            self.indent += 1;
            let old_handler_vars = self.current_handler_vars.clone();
            let expanded = self.expand_effect_names(effect_clause);
            for ename in &expanded {
                self.current_handler_vars
                    .insert(ename.clone(), to_camel_case(ename));
            }
            if return_type.is_some() && !is_void {
                self.emit_block_body_return(body)?;
            } else {
                self.emit_block_body(body)?;
            }
            self.current_handler_vars = old_handler_vars;
            self.indent -= 1;
            self.writeln("}");
        }
        Ok(())
    }

    fn collect_param_strs(&self, params: &[AIRNode]) -> Vec<String> {
        params
            .iter()
            .filter_map(|p| {
                if let NodeKind::Param { pattern, ty, .. } = &p.kind {
                    let name = to_camel_case(&self.pattern_to_binding_name(pattern));
                    let type_str = ty
                        .as_ref()
                        .map(|t| format!(" {}", self.type_to_go(t)))
                        .unwrap_or_else(|| " interface{}".into());
                    Some(format!("{name}{type_str}"))
                } else {
                    None
                }
            })
            .collect()
    }

    fn collect_param_type_strs(&self, params: &[AIRNode]) -> Vec<String> {
        params
            .iter()
            .filter_map(|p| {
                if let NodeKind::Param { ty, .. } = &p.kind {
                    let type_str = ty
                        .as_ref()
                        .map(|t| self.type_to_go(t))
                        .unwrap_or_else(|| "interface{}".into());
                    Some(type_str)
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

    /// Effects → interface parameters: `log Log, clock Clock`.
    fn effects_params(&self, effects: &[bock_ast::TypePath]) -> Vec<String> {
        let expanded = self.expand_effect_names(effects);
        expanded
            .iter()
            .map(|name| format!("{} {}", to_camel_case(name), name))
            .collect()
    }

    /// Build `handler_var, ...` arguments for calling an effectful function.
    fn build_effects_call_args_go(&self, fn_name: &str) -> Option<String> {
        let effects = self.fn_effects.get(fn_name)?;
        let entries: Vec<String> = effects
            .iter()
            .filter_map(|e| {
                let handler_var = self.current_handler_vars.get(e)?;
                Some(handler_var.clone())
            })
            .collect();
        if entries.is_empty() {
            return None;
        }
        Some(entries.join(", "))
    }

    // ── Enum variant structs ────────────────────────────────────────────────

    fn emit_enum_variant(
        &mut self,
        enum_name: &str,
        generic_params: &[bock_ast::GenericParam],
        variant: &AIRNode,
    ) -> Result<(), CodegenError> {
        if let NodeKind::EnumVariant { name, payload } = &variant.kind {
            let vname = &name.name;
            let type_params = self.format_generic_params(generic_params);
            match payload {
                EnumVariantPayload::Unit => {
                    self.writeln(&format!("type {enum_name}{vname}{type_params} struct{{}}"));
                }
                EnumVariantPayload::Struct(fields) => {
                    self.writeln(&format!("type {enum_name}{vname}{type_params} struct {{"));
                    self.indent += 1;
                    for f in fields {
                        let type_str = self.ast_type_to_go(&f.ty);
                        self.writeln(&format!("{}\t{type_str}", to_pascal_case(&f.name.name)));
                    }
                    self.indent -= 1;
                    self.writeln("}");
                }
                EnumVariantPayload::Tuple(elems) => {
                    self.writeln(&format!("type {enum_name}{vname}{type_params} struct {{"));
                    self.indent += 1;
                    for (i, elem) in elems.iter().enumerate() {
                        let type_str = self.type_to_go(elem);
                        self.writeln(&format!("Field{i}\t{type_str}"));
                    }
                    self.indent -= 1;
                    self.writeln("}");
                }
            }
            // Implement the interface marker method.
            self.buf.push('\n');
            self.writeln(&format!(
                "func ({enum_name}{vname}{type_params}) is{enum_name}() {{}}"
            ));
        }
        Ok(())
    }

    // ── Statements ──────────────────────────────────────────────────────────

    fn emit_stmt(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::LetBinding {
                pattern, value, ty, ..
            } => {
                let binding = self.pattern_to_go_binding(pattern);
                if let Some(t) = ty {
                    let type_str = self.type_to_go(t);
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}var {binding} {type_str} = ");
                    self.emit_expr(value)?;
                    self.buf.push('\n');
                } else {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}{binding} := ");
                    self.emit_expr(value)?;
                    self.buf.push('\n');
                }
                Ok(())
            }
            NodeKind::If {
                let_pattern,
                condition,
                then_block,
                else_block,
            } => {
                if let Some(pat) = let_pattern {
                    let binding = self.pattern_to_go_binding(pat);
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}{binding} := ");
                    self.emit_expr(condition)?;
                    self.buf.push('\n');
                    self.writeln(&format!("if {binding} != nil {{"));
                    self.indent += 1;
                    self.emit_block_body(then_block)?;
                    self.indent -= 1;
                } else {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}if ");
                    self.emit_expr(condition)?;
                    self.buf.push_str(" {\n");
                    self.indent += 1;
                    self.emit_block_body(then_block)?;
                    self.indent -= 1;
                }
                if let Some(else_b) = else_block {
                    if matches!(else_b.kind, NodeKind::If { .. }) {
                        let ind = self.indent_str();
                        let _ = write!(self.buf, "{ind}}} else ");
                        // Emit the if without leading indent.
                        self.emit_if_continued(else_b)?;
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
                let binding = self.pattern_to_go_binding(pattern);
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}for _, {binding} := range ");
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
                let _ = write!(self.buf, "{ind}for ");
                self.emit_expr(condition)?;
                self.buf.push_str(" {\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::Loop { body } => {
                self.writeln("for {");
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
                    self.buf.push('\n');
                } else {
                    self.writeln("return");
                }
                Ok(())
            }
            NodeKind::Break { value } => {
                if let Some(val) = value {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}// break value: ");
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
                let mut new_var_names = Vec::with_capacity(handlers.len());
                for h in handlers {
                    let effect_name = h
                        .effect
                        .segments
                        .last()
                        .map_or("effect", |s| s.name.as_str());
                    let var_name = format!("__{}", to_camel_case(effect_name));
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}{var_name} := ");
                    self.emit_expr(&h.handler)?;
                    self.buf.push('\n');
                    self.current_handler_vars
                        .insert(effect_name.to_string(), var_name.clone());
                    new_var_names.push(var_name);
                }
                // Suppress Go's "declared but not used" error when a handler
                // is declared in an outer handling scope and only referenced
                // indirectly through inner handling blocks (or not at all).
                for v in &new_var_names {
                    self.writeln(&format!("_ = {v}"));
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

    /// Emit an if statement that continues after an `} else`.
    fn emit_if_continued(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } = &node.kind
        {
            let _ = write!(self.buf, "if ");
            self.emit_expr(condition)?;
            self.buf.push_str(" {\n");
            self.indent += 1;
            self.emit_block_body(then_block)?;
            self.indent -= 1;
            if let Some(else_b) = else_block {
                if matches!(else_b.kind, NodeKind::If { .. }) {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}}} else ");
                    return self.emit_if_continued(else_b);
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
                        self.buf.push_str(&escape_go_string(s));
                        self.buf.push('"');
                    }
                    Literal::Unit => self.buf.push_str("nil"),
                }
                Ok(())
            }
            NodeKind::Identifier { name } => {
                let emitted = if is_prelude_ctor(&name.name) {
                    name.name.clone()
                } else if self.public_fns.contains(&name.name) {
                    to_pascal_case(&name.name)
                } else {
                    to_camel_case(&name.name)
                };
                self.buf.push_str(&emitted);
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
                    BinOp::Pow => " /* pow */ ",
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
                    BinOp::Is => " == ",
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
                    UnaryOp::BitNot => "^",
                };
                self.buf.push_str(op_str);
                self.emit_expr(operand)?;
                Ok(())
            }
            NodeKind::Call {
                callee,
                args,
                type_args,
            } => {
                // Effect operation Call → handler.Op rewriting.
                if let NodeKind::Identifier { name } = &callee.kind {
                    if let Some(effect_name) = self.effect_ops.get(&name.name).cloned() {
                        if let Some(handler_var) =
                            self.current_handler_vars.get(&effect_name).cloned()
                        {
                            let _ =
                                write!(self.buf, "{}.{}", handler_var, to_pascal_case(&name.name));
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
                    self.build_effects_call_args_go(&name.name)
                } else {
                    None
                };
                // Route async-fn calls through their `Async`-suffix wrapper
                // so callers receive a `<-chan T` instead of `T` — the sync
                // body is only invoked from inside its own wrapper.
                if let NodeKind::Identifier { name } = &callee.kind {
                    if self.async_fns.contains(&name.name) {
                        let go_name = if self.public_fns.contains(&name.name) {
                            to_pascal_case(&name.name)
                        } else {
                            to_camel_case(&name.name)
                        };
                        self.buf.push_str(&format!("{go_name}Async"));
                    } else {
                        self.emit_expr(callee)?;
                    }
                } else {
                    self.emit_expr(callee)?;
                }
                let type_arg_str = self.format_generic_args(type_args);
                self.buf.push_str(&type_arg_str);
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
                let _ = write!(self.buf, ".{}", to_pascal_case(&method.name));
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
                let _ = write!(self.buf, ".{}", to_pascal_case(&field.name));
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
                // Determine return type from body context (best effort).
                let _ = write!(
                    self.buf,
                    "func({}) interface{{}} {{ return ",
                    param_strs.join(", ")
                );
                self.emit_expr(body)?;
                self.buf.push_str(" }");
                Ok(())
            }
            NodeKind::Pipe { left, right } => self.emit_pipe(left, right),
            NodeKind::Compose { left, right } => {
                // `f >> g` → `func(x interface{}) interface{} { return g(f(x)) }`
                let _ = write!(self.buf, "func(x interface{{}}) interface{{}} {{ return ");
                self.emit_expr(right)?;
                self.buf.push('(');
                self.emit_expr(left)?;
                self.buf.push_str("(x)) }");
                Ok(())
            }
            NodeKind::Await { expr } => {
                // Go uses goroutines/channels; await maps to channel receive.
                self.buf.push_str("<-");
                self.emit_expr(expr)?;
                Ok(())
            }
            NodeKind::Propagate { expr } => {
                // Go error propagation would require special handling;
                // just emit the expression for now.
                self.emit_expr(expr)?;
                Ok(())
            }
            NodeKind::Range { lo, hi, inclusive } => {
                // Go doesn't have range expressions as values;
                // emit as a comment-annotated slice or helper call.
                self.needs_fmt_import = true;
                self.buf.push_str("/* range */ nil");
                let _ = (lo, hi, inclusive);
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
                if let Some(_sp) = spread {
                    // Go doesn't have spread syntax; emit TODO comment.
                    self.buf.push_str(&format!("{type_name}{{"));
                    self.buf.push_str("/* spread */ ");
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        let _ = write!(self.buf, "{}: ", to_pascal_case(&f.name.name));
                        if let Some(val) = &f.value {
                            self.emit_expr(val)?;
                        } else {
                            self.buf.push_str(&to_camel_case(&f.name.name));
                        }
                    }
                    self.buf.push('}');
                } else {
                    self.buf.push_str(&format!("{type_name}{{"));
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        let _ = write!(self.buf, "{}: ", to_pascal_case(&f.name.name));
                        if let Some(val) = &f.value {
                            self.emit_expr(val)?;
                        } else {
                            self.buf.push_str(&to_camel_case(&f.name.name));
                        }
                    }
                    self.buf.push('}');
                }
                Ok(())
            }
            NodeKind::ListLiteral { elems } => {
                self.buf.push_str("[]interface{}{");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(e)?;
                }
                self.buf.push('}');
                Ok(())
            }
            NodeKind::MapLiteral { entries } => {
                self.buf.push_str("map[interface{}]interface{}{");
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
                // Go doesn't have sets; use map[T]struct{}.
                self.buf.push_str("map[interface{}]struct{}{");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(e)?;
                    self.buf.push_str(": {}");
                }
                self.buf.push('}');
                Ok(())
            }
            NodeKind::TupleLiteral { elems } => {
                // Go doesn't have tuples; emit as a struct literal or slice.
                self.buf.push_str("[...]interface{}{");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(e)?;
                }
                self.buf.push('}');
                Ok(())
            }
            NodeKind::Interpolation { parts } => {
                self.needs_fmt_import = true;
                self.buf.push_str("fmt.Sprintf(\"");
                let mut args = Vec::new();
                for part in parts {
                    match part {
                        AirInterpolationPart::Literal(s) => {
                            self.buf.push_str(&escape_go_string(s));
                        }
                        AirInterpolationPart::Expr(expr) => {
                            self.buf.push_str("%v");
                            args.push(expr.clone());
                        }
                    }
                }
                self.buf.push('"');
                for arg in &args {
                    self.buf.push_str(", ");
                    self.emit_expr(arg)?;
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::Placeholder => {
                self.buf.push('_');
                Ok(())
            }
            NodeKind::Unreachable => {
                self.buf.push_str("panic(\"unreachable\")");
                Ok(())
            }
            NodeKind::ResultConstruct { variant, value } => {
                match variant {
                    ResultVariant::Ok => {
                        if let Some(v) = value {
                            self.emit_expr(v)?;
                            self.buf.push_str(", nil");
                        } else {
                            self.buf.push_str("nil, nil");
                        }
                    }
                    ResultVariant::Err => {
                        self.needs_fmt_import = true;
                        if let Some(v) = value {
                            self.buf.push_str("nil, ");
                            self.emit_expr(v)?;
                        } else {
                            self.buf.push_str("nil, fmt.Errorf(\"error\")");
                        }
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
                // If in expression position: Go doesn't have ternary;
                // emit as IIFE.
                self.buf.push_str("func() interface{} { if ");
                self.emit_expr(condition)?;
                self.buf.push_str(" { return ");
                self.emit_block_as_expr(then_block)?;
                self.buf.push_str(" } else { return ");
                if let Some(eb) = else_block {
                    self.emit_block_as_expr(eb)?;
                } else {
                    self.buf.push_str("nil");
                }
                self.buf.push_str(" } }()");
                Ok(())
            }
            NodeKind::Block { stmts, tail } => {
                if stmts.is_empty() {
                    if let Some(t) = tail {
                        return self.emit_expr(t);
                    }
                }
                // Fallback: IIFE.
                self.buf.push_str("func() interface{} { return ");
                if let Some(t) = tail {
                    self.emit_expr(t)?;
                } else {
                    self.buf.push_str("nil");
                }
                self.buf.push_str(" }()");
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => {
                // Match in expression position: emit as IIFE with switch.
                self.buf.push_str("func() interface{} { switch ");
                self.emit_expr(scrutinee)?;
                self.buf.push_str(" { ");
                for arm in arms {
                    if let NodeKind::MatchArm { pattern, body, .. } = &arm.kind {
                        if matches!(pattern.kind, NodeKind::WildcardPat) {
                            self.buf.push_str("default: return ");
                        } else {
                            self.buf.push_str("case ");
                            self.emit_match_case_condition(pattern)?;
                            self.buf.push_str(": return ");
                        }
                        self.emit_block_as_expr(body)?;
                        self.buf.push(' ');
                    }
                }
                self.buf.push_str("} return nil }()");
                Ok(())
            }
            // Ownership nodes: erase in Go.
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
                    to_pascal_case(&operation.name)
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
            // Type expressions: erased in Go expression context.
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

    // ── Match → switch/if-else ──────────────────────────────────────────────

    fn emit_match(&mut self, scrutinee: &AIRNode, arms: &[AIRNode]) -> Result<(), CodegenError> {
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}switch __v := ");
        self.emit_expr(scrutinee)?;
        self.buf.push_str("; __v.(type) {\n");
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
            match &pattern.kind {
                NodeKind::WildcardPat => {
                    let _ = write!(self.buf, "{ind}default:");
                }
                _ => {
                    let _ = write!(self.buf, "{ind}case ");
                    self.emit_match_case_condition(pattern)?;
                    self.buf.push(':');
                }
            }
            self.buf.push('\n');
            self.indent += 1;
            if let Some(g) = guard {
                let gi = self.indent_str();
                let _ = write!(self.buf, "{gi}if ");
                self.emit_expr(g)?;
                self.buf.push_str(" {\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
            } else {
                self.emit_block_body(body)?;
            }
        }
        Ok(())
    }

    fn emit_match_case_condition(&mut self, pat: &AIRNode) -> Result<(), CodegenError> {
        match &pat.kind {
            NodeKind::WildcardPat => {
                self.buf.push('_');
            }
            NodeKind::BindPat { name, .. } => {
                let _ = name;
                self.buf.push_str("interface{}");
            }
            NodeKind::LiteralPat { lit } => match lit {
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
                    self.buf.push_str(&escape_go_string(s));
                    self.buf.push('"');
                }
                Literal::Unit => self.buf.push_str("nil"),
            },
            NodeKind::ConstructorPat { path, .. } => {
                let variant_name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("");
                self.buf.push_str(&variant_name);
            }
            NodeKind::RecordPat { path, .. } => {
                let type_name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                self.buf.push_str(&type_name);
            }
            NodeKind::TuplePat { .. } => {
                self.buf.push_str("interface{}");
            }
            _ => {
                self.buf.push_str("interface{}");
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

    // ── Type emission ───────────────────────────────────────────────────────

    fn type_to_go(&self, node: &AIRNode) -> String {
        match &node.kind {
            NodeKind::TypeNamed { path, args } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let go_name = self.map_type_name(&name);
                if args.is_empty() {
                    go_name
                } else {
                    let arg_strs: Vec<String> = args.iter().map(|a| self.type_to_go(a)).collect();
                    format!("{go_name}[{}]", arg_strs.join(", "))
                }
            }
            NodeKind::TypeTuple { elems } => {
                // Go doesn't have tuples; emit as struct with numbered fields.
                if elems.is_empty() {
                    "struct{}".into()
                } else {
                    let fields: Vec<String> = elems
                        .iter()
                        .enumerate()
                        .map(|(i, e)| format!("Field{i} {}", self.type_to_go(e)))
                        .collect();
                    format!("struct{{ {} }}", fields.join("; "))
                }
            }
            NodeKind::TypeFunction { params, ret, .. } => {
                let param_strs: Vec<String> = params.iter().map(|p| self.type_to_go(p)).collect();
                format!("func({}) {}", param_strs.join(", "), self.type_to_go(ret))
            }
            NodeKind::TypeOptional { inner } => {
                format!("*{}", self.type_to_go(inner))
            }
            NodeKind::TypeSelf => "/* Self */".into(),
            _ => "interface{}".into(),
        }
    }

    fn map_type_name(&self, name: &str) -> String {
        match name {
            "Int" => "int64".into(),
            "Float" => "float64".into(),
            "Bool" => "bool".into(),
            "String" => "string".into(),
            "Void" | "Unit" => "struct{}".into(),
            "List" => "[]interface{}".into(),
            "Map" => "map[string]interface{}".into(),
            "Set" => "map[interface{}]struct{}".into(),
            "Any" => "interface{}".into(),
            "Never" => "interface{}".into(),
            "Channel" => "*__bockChannel".into(),
            other => other.into(),
        }
    }

    fn ast_type_to_go(&self, ty: &TypeExpr) -> String {
        match ty {
            TypeExpr::Named { path, args, .. } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let go_name = self.map_type_name(&name);
                if args.is_empty() {
                    go_name
                } else {
                    let arg_strs: Vec<String> =
                        args.iter().map(|a| self.ast_type_to_go(a)).collect();
                    format!("{go_name}[{}]", arg_strs.join(", "))
                }
            }
            TypeExpr::Tuple { elems, .. } => {
                if elems.is_empty() {
                    "struct{}".into()
                } else {
                    let fields: Vec<String> = elems
                        .iter()
                        .enumerate()
                        .map(|(i, e)| format!("Field{i} {}", self.ast_type_to_go(e)))
                        .collect();
                    format!("struct{{ {} }}", fields.join("; "))
                }
            }
            TypeExpr::Function { params, ret, .. } => {
                let param_strs: Vec<String> =
                    params.iter().map(|p| self.ast_type_to_go(p)).collect();
                format!(
                    "func({}) {}",
                    param_strs.join(", "),
                    self.ast_type_to_go(ret)
                )
            }
            TypeExpr::Optional { inner, .. } => {
                format!("*{}", self.ast_type_to_go(inner))
            }
            TypeExpr::SelfType { .. } => "/* Self */".into(),
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn emit_block_body(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.emit_block_body_inner(node, false)
    }

    fn emit_block_body_return(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.emit_block_body_inner(node, true)
    }

    fn emit_block_body_inner(
        &mut self,
        node: &AIRNode,
        emit_return: bool,
    ) -> Result<(), CodegenError> {
        if let NodeKind::Block { stmts, tail } = &node.kind {
            if stmts.is_empty() && tail.is_none() {
                self.writeln("// empty");
                return Ok(());
            }
            for s in stmts {
                self.emit_node(s)?;
            }
            if let Some(t) = tail {
                let should_return = emit_return && !self.is_void_call(t);
                if should_return {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}return ");
                    self.emit_expr(t)?;
                    self.buf.push('\n');
                } else {
                    self.write_indent();
                    self.emit_expr(t)?;
                    self.buf.push('\n');
                }
            }
        } else {
            // Single expression as body.
            let should_return = emit_return && !self.is_void_call(node);
            if should_return {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}return ");
                self.emit_expr(node)?;
                self.buf.push('\n');
            } else {
                self.write_indent();
                self.emit_expr(node)?;
                self.buf.push('\n');
            }
        }
        Ok(())
    }

    /// Returns `true` if the expression is a call to a known void function
    /// (prelude or a Void-returning effect operation).
    fn is_void_call(&self, node: &AIRNode) -> bool {
        if let NodeKind::Call { callee, .. } = &node.kind {
            if let NodeKind::Identifier { name } = &callee.kind {
                if matches!(
                    name.name.as_str(),
                    "println" | "print" | "debug" | "assert" | "todo" | "unreachable"
                ) {
                    return true;
                }
                if self.void_effect_ops.contains(&name.name) {
                    return true;
                }
            }
        }
        false
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
                // Go doesn't have tuple destructuring; use first element.
                elems
                    .first()
                    .map(|e| self.pattern_to_binding_name(e))
                    .unwrap_or_else(|| "_".into())
            }
            NodeKind::RecordPat { fields, .. } => fields
                .first()
                .map(|f| to_camel_case(&f.name.name))
                .unwrap_or_else(|| "_".into()),
            _ => "_".into(),
        }
    }

    fn pattern_to_go_binding(&self, pat: &AIRNode) -> String {
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

/// True for Bock's built-in Optional/Result constructors, which must be
/// emitted verbatim (PascalCase preserved) so generated Go code can match
/// the runtime prelude's `Some`/`None`/`Ok`/`Err` types.
fn is_prelude_ctor(s: &str) -> bool {
    matches!(s, "Some" | "None" | "Ok" | "Err")
}

/// Convert a name to `camelCase` (Go unexported).
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

/// Convert a name to `PascalCase` (Go exported).
fn to_pascal_case(s: &str) -> String {
    if s.is_empty() || s == "_" {
        return s.to_string();
    }
    // If it's snake_case, convert to PascalCase.
    if s.contains('_') {
        let parts: Vec<&str> = s.split('_').filter(|p| !p.is_empty()).collect();
        let mut result = String::new();
        for part in &parts {
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
    // Already PascalCase or camelCase — uppercase first letter.
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty string guaranteed by caller");
    let mut result = first.to_uppercase().to_string();
    result.extend(chars);
    result
}

/// Escape special characters in a Go string literal.
fn escape_go_string(s: &str) -> String {
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
        let gen = GoGenerator::new();
        let result = gen.generate_module(module).unwrap();
        result.files[0].content.clone()
    }

    // ── Basic tests ─────────────────────────────────────────────────────────

    #[test]
    fn implements_code_generator_trait() {
        let gen = GoGenerator::new();
        assert_eq!(gen.target().id, "go");
    }

    #[test]
    fn empty_module() {
        let m = module(vec![], vec![]);
        let out = gen(&m);
        assert!(out.contains("package main"), "got: {out}");
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
        assert!(out.contains("func answer()"), "got: {out}");
        assert!(out.contains("return 42"), "got: {out}");
    }

    #[test]
    fn public_function_is_pascal_case() {
        let body = block(2, vec![], Some(int_lit(3, "42")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("getAnswer"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("func GetAnswer()"), "got: {out}");
    }

    #[test]
    fn function_with_params_and_types() {
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
            out.contains("func Add(a int64, b int64) int64 {"),
            "got: {out}"
        );
        assert!(out.contains("(a + b)"), "got: {out}");
    }

    #[test]
    fn record_to_struct() {
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
                        ty: TypeExpr::Named {
                            id: 0,
                            span: span(),
                            path: type_path(&["Float"]),
                            args: vec![],
                        },
                        default: None,
                    },
                    bock_ast::RecordDeclField {
                        id: 1,
                        span: span(),
                        name: ident("y"),
                        ty: TypeExpr::Named {
                            id: 1,
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
        assert!(out.contains("type Point struct {"), "got: {out}");
        assert!(out.contains("X\tfloat64"), "got: {out}");
        assert!(out.contains("Y\tfloat64"), "got: {out}");
    }

    #[test]
    fn trait_to_interface() {
        let t = node(
            1,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Drawable"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("draw"),
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
        assert!(out.contains("type Drawable interface {"), "got: {out}");
        assert!(out.contains("Draw()"), "got: {out}");
    }

    #[test]
    fn enum_to_interface_and_structs() {
        let e = node(
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
                                ty: TypeExpr::Named {
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
        let out = gen(&module(vec![], vec![e]));
        assert!(out.contains("type Shape interface {"), "got: {out}");
        assert!(out.contains("isShape()"), "got: {out}");
        assert!(out.contains("type ShapeCircle struct {"), "got: {out}");
        assert!(out.contains("Radius\tfloat64"), "got: {out}");
        assert!(out.contains("type ShapeNone struct{}"), "got: {out}");
        assert!(
            out.contains("func (ShapeCircle) isShape() {}"),
            "got: {out}"
        );
        assert!(out.contains("func (ShapeNone) isShape() {}"), "got: {out}");
    }

    #[test]
    fn effects_as_interface_params() {
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
                visibility: Visibility::Public,
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
            out.contains("func Process(data interface{}, log Log, clock Clock)"),
            "got: {out}"
        );
        assert!(out.contains("log.Info(msg)"), "got: {out}");
    }

    #[test]
    fn generics_with_type_params() {
        let body = block(2, vec![], Some(id_node(3, "value")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("identity"),
                generic_params: vec![bock_ast::GenericParam {
                    id: 10,
                    span: span(),
                    name: ident("T"),
                    bounds: vec![],
                }],
                params: vec![typed_param_node(2, "value", "T")],
                return_type: Some(Box::new(node(
                    4,
                    NodeKind::TypeNamed {
                        path: type_path(&["T"]),
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
            out.contains("func Identity[T any](value T) T {"),
            "got: {out}"
        );
    }

    #[test]
    fn generics_with_bounds() {
        let body = block(2, vec![], Some(id_node(3, "value")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("constrained"),
                generic_params: vec![bock_ast::GenericParam {
                    id: 10,
                    span: span(),
                    name: ident("T"),
                    bounds: vec![type_path(&["Comparable"])],
                }],
                params: vec![typed_param_node(2, "value", "T")],
                return_type: Some(Box::new(node(
                    4,
                    NodeKind::TypeNamed {
                        path: type_path(&["T"]),
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
            out.contains("func Constrained[T Comparable](value T) T {"),
            "got: {out}"
        );
    }

    #[test]
    fn match_to_switch() {
        let m = node(
            1,
            NodeKind::Match {
                scrutinee: Box::new(id_node(2, "x")),
                arms: vec![
                    node(
                        3,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(
                                4,
                                NodeKind::LiteralPat {
                                    lit: Literal::Int("1".into()),
                                },
                            )),
                            guard: None,
                            body: Box::new(block(5, vec![], Some(str_lit(6, "one")))),
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
        let out = gen(&module(vec![], vec![m]));
        assert!(out.contains("switch"), "got: {out}");
        assert!(out.contains("default:"), "got: {out}");
    }

    #[test]
    fn match_arm_guard_emits_if() {
        let m = node(
            1,
            NodeKind::Match {
                scrutinee: Box::new(id_node(2, "x")),
                arms: vec![node(
                    3,
                    NodeKind::MatchArm {
                        pattern: Box::new(node(
                            4,
                            NodeKind::LiteralPat {
                                lit: Literal::Int("1".into()),
                            },
                        )),
                        guard: Some(Box::new(id_node(5, "ok"))),
                        body: Box::new(block(
                            6,
                            vec![node(7, NodeKind::Return { value: None })],
                            None,
                        )),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![m]));
        assert!(
            out.contains("if ok {"),
            "guard should emit real if-statement, got: {out}"
        );
        assert!(
            !out.contains("// guard"),
            "guard should not be a comment, got: {out}"
        );
    }

    #[test]
    fn let_binding() {
        let l = node(
            1,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(2, "x")),
                ty: None,
                value: Box::new(int_lit(3, "42")),
            },
        );
        let out = gen(&module(vec![], vec![l]));
        assert!(out.contains("x := 42"), "got: {out}");
    }

    #[test]
    fn let_binding_with_type() {
        let l = node(
            1,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(2, "x")),
                ty: Some(Box::new(node(
                    4,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                value: Box::new(int_lit(3, "42")),
            },
        );
        let out = gen(&module(vec![], vec![l]));
        assert!(out.contains("var x int64 = 42"), "got: {out}");
    }

    #[test]
    fn if_else() {
        let stmt = node(
            1,
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(bool_lit(2, true)),
                then_block: Box::new(block(3, vec![], Some(int_lit(4, "1")))),
                else_block: Some(Box::new(block(5, vec![], Some(int_lit(6, "0"))))),
            },
        );
        let out = gen(&module(vec![], vec![stmt]));
        assert!(out.contains("if true {"), "got: {out}");
        assert!(out.contains("} else {"), "got: {out}");
    }

    #[test]
    fn for_loop() {
        let stmt = node(
            1,
            NodeKind::For {
                pattern: Box::new(bind_pat(2, "item")),
                iterable: Box::new(id_node(3, "items")),
                body: Box::new(block(4, vec![], None)),
            },
        );
        let out = gen(&module(vec![], vec![stmt]));
        assert!(out.contains("for _, item := range items {"), "got: {out}");
    }

    #[test]
    fn while_loop() {
        let stmt = node(
            1,
            NodeKind::While {
                condition: Box::new(bool_lit(2, true)),
                body: Box::new(block(3, vec![], None)),
            },
        );
        let out = gen(&module(vec![], vec![stmt]));
        assert!(out.contains("for true {"), "got: {out}");
    }

    #[test]
    fn infinite_loop() {
        let stmt = node(
            1,
            NodeKind::Loop {
                body: Box::new(block(2, vec![], None)),
            },
        );
        let out = gen(&module(vec![], vec![stmt]));
        assert!(out.contains("for {"), "got: {out}");
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
        let out = gen(&module(vec![], vec![interp]));
        assert!(out.contains("fmt.Sprintf"), "got: {out}");
        assert!(out.contains("Hello, %v!"), "got: {out}");
        assert!(out.contains("import \"fmt\""), "got: {out}");
    }

    #[test]
    fn record_construction() {
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
        let out = gen(&module(vec![], vec![rc]));
        assert!(out.contains("Point{X: 1, Y: 2}"), "got: {out}");
    }

    #[test]
    fn list_literal() {
        let l = node(
            1,
            NodeKind::ListLiteral {
                elems: vec![int_lit(2, "1"), int_lit(3, "2"), int_lit(4, "3")],
            },
        );
        let out = gen(&module(vec![], vec![l]));
        assert!(out.contains("[]interface{}{1, 2, 3}"), "got: {out}");
    }

    #[test]
    fn effect_decl_to_interface() {
        let ed = node(
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
                            name: ident("info"),
                            generic_params: vec![],
                            params: vec![typed_param_node(3, "msg", "String")],
                            return_type: None,
                            effect_clause: vec![],
                            where_clause: vec![],
                            body: Box::new(block(4, vec![], None)),
                        },
                    ),
                    node(
                        5,
                        NodeKind::FnDecl {
                            annotations: vec![],
                            visibility: Visibility::Public,
                            is_async: false,
                            name: ident("error"),
                            generic_params: vec![],
                            params: vec![typed_param_node(6, "msg", "String")],
                            return_type: None,
                            effect_clause: vec![],
                            where_clause: vec![],
                            body: Box::new(block(7, vec![], None)),
                        },
                    ),
                ],
            },
        );
        let out = gen(&module(vec![], vec![ed]));
        assert!(out.contains("type Logger interface {"), "got: {out}");
        assert!(out.contains("Info(string)"), "got: {out}");
        assert!(out.contains("Error(string)"), "got: {out}");
    }

    #[test]
    fn result_construct_ok() {
        let rc = node(
            1,
            NodeKind::ResultConstruct {
                variant: ResultVariant::Ok,
                value: Some(Box::new(int_lit(2, "42"))),
            },
        );
        let out = gen(&module(vec![], vec![rc]));
        assert!(out.contains("42, nil"), "got: {out}");
    }

    #[test]
    fn result_construct_err() {
        let rc = node(
            1,
            NodeKind::ResultConstruct {
                variant: ResultVariant::Err,
                value: Some(Box::new(str_lit(2, "failed"))),
            },
        );
        let out = gen(&module(vec![], vec![rc]));
        assert!(out.contains("nil, \"failed\""), "got: {out}");
    }

    #[test]
    fn class_to_struct_with_methods() {
        let cls = node(
            1,
            NodeKind::ClassDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Counter"),
                generic_params: vec![],
                base: None,
                traits: vec![],
                fields: vec![bock_ast::RecordDeclField {
                    id: 0,
                    span: span(),
                    name: ident("count"),
                    ty: TypeExpr::Named {
                        id: 0,
                        span: span(),
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                    default: None,
                }],
                methods: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("increment"),
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
        let out = gen(&module(vec![], vec![cls]));
        assert!(out.contains("type Counter struct {"), "got: {out}");
        assert!(out.contains("Count\tint64"), "got: {out}");
        assert!(out.contains("func NewCounter("), "got: {out}");
        assert!(out.contains("func (c *Counter) Increment()"), "got: {out}");
    }

    #[test]
    fn lambda_expression() {
        let lam = node(
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
        let out = gen(&module(vec![], vec![lam]));
        assert!(
            out.contains("func(x interface{}) interface{} { return (x * 2) }"),
            "got: {out}"
        );
    }

    #[test]
    fn impl_block_methods() {
        let imp = node(
            1,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
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
                        name: ident("distance"),
                        generic_params: vec![],
                        params: vec![],
                        return_type: Some(Box::new(node(
                            4,
                            NodeKind::TypeNamed {
                                path: type_path(&["Float"]),
                                args: vec![],
                            },
                        ))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(5, vec![], Some(int_lit(6, "0")))),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![imp]));
        assert!(
            out.contains("func (p *Point) Distance() float64 {"),
            "got: {out}"
        );
    }

    #[test]
    fn concurrency_goroutine() {
        // Async function → goroutine pattern with channel.
        // The await expression maps to channel receive.
        let body = block(
            3,
            vec![],
            Some(node(
                4,
                NodeKind::Await {
                    expr: Box::new(id_node(5, "ch")),
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
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
        assert!(out.contains("func FetchData()"), "got: {out}");
        assert!(out.contains("<-ch"), "got: {out}");
    }

    #[test]
    fn async_fn_emits_goroutine_wrapper() {
        // Async function with Int return → sync body + FnAsync wrapper
        // returning `<-chan int`.
        let body = block(3, vec![], Some(int_lit(4, "42")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: true,
                name: ident("task1"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(node(
                    5,
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
            out.contains("func Task1() int64 {"),
            "sync body missing: {out}"
        );
        assert!(
            out.contains("func Task1Async() <-chan int64 {"),
            "async wrapper missing: {out}"
        );
        assert!(out.contains("__ch := make(chan int64, 1)"), "got: {out}");
        assert!(out.contains("go func() {"), "got: {out}");
        assert!(out.contains("__ch <- Task1()"), "got: {out}");
        assert!(out.contains("return __ch"), "got: {out}");
    }

    #[test]
    fn async_main_no_wrapper() {
        // main is Go's entry — skip the wrapper to avoid dead code.
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
        assert!(out.contains("func main() {"), "got: {out}");
        assert!(!out.contains("mainAsync"), "got: {out}");
    }

    #[test]
    fn async_call_rewritten_to_async_wrapper() {
        // Calling `task1()` from another async fn should route through
        // `Task1Async()` so callers can `await` (= `<-`) the channel.
        let task1 = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: true,
                name: ident("task1"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(node(
                    11,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(12, vec![], Some(int_lit(13, "1")))),
            },
        );
        // caller body: let a = task1(); let b = task1(); await a; await b
        let call_task1 = |id: u32| {
            node(
                id,
                NodeKind::Call {
                    callee: Box::new(id_node(id + 1, "task1")),
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
        let caller_body = block(
            20,
            vec![
                let_stmt(30, "a", call_task1(31)),
                let_stmt(40, "b", call_task1(41)),
                let_stmt(50, "ra", await_id(51, "a")),
                let_stmt(60, "rb", await_id(61, "b")),
            ],
            None,
        );
        let caller = node(
            100,
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
                body: Box::new(caller_body),
            },
        );
        let out = gen(&module(vec![], vec![task1, caller]));
        // Concurrent goroutines: both bindings start channels.
        assert!(out.contains("a := Task1Async()"), "got: {out}");
        assert!(out.contains("b := Task1Async()"), "got: {out}");
        // Awaits receive from the channels.
        assert!(out.contains("ra := <-a"), "got: {out}");
        assert!(out.contains("rb := <-b"), "got: {out}");
    }

    #[test]
    fn break_continue() {
        let brk = node(1, NodeKind::Break { value: None });
        let cont = node(2, NodeKind::Continue);
        let out = gen(&module(vec![], vec![brk, cont]));
        assert!(out.contains("break"), "got: {out}");
        assert!(out.contains("continue"), "got: {out}");
    }

    #[test]
    fn guard_statement() {
        let g = node(
            1,
            NodeKind::Guard {
                let_pattern: None,
                condition: Box::new(bool_lit(2, true)),
                else_block: Box::new(block(
                    3,
                    vec![node(4, NodeKind::Return { value: None })],
                    None,
                )),
            },
        );
        let out = gen(&module(vec![], vec![g]));
        assert!(out.contains("if !(true)"), "got: {out}");
    }

    #[test]
    fn ownership_erased() {
        let borrow = node(
            1,
            NodeKind::Borrow {
                expr: Box::new(id_node(2, "x")),
            },
        );
        let mv = node(
            3,
            NodeKind::Move {
                expr: Box::new(id_node(4, "y")),
            },
        );
        let out = gen(&module(vec![], vec![borrow, mv]));
        assert!(out.contains("x"), "got: {out}");
        assert!(out.contains("y"), "got: {out}");
        // Should NOT contain borrow/move keywords.
        assert!(!out.contains("&x"), "got: {out}");
    }

    #[test]
    fn type_mapping() {
        let ctx = GoEmitCtx::new();
        assert_eq!(ctx.map_type_name("Int"), "int64");
        assert_eq!(ctx.map_type_name("Float"), "float64");
        assert_eq!(ctx.map_type_name("Bool"), "bool");
        assert_eq!(ctx.map_type_name("String"), "string");
        assert_eq!(ctx.map_type_name("Void"), "struct{}");
        assert_eq!(ctx.map_type_name("Any"), "interface{}");
    }

    #[test]
    fn naming_conventions() {
        assert_eq!(to_camel_case("hello_world"), "helloWorld");
        assert_eq!(to_camel_case("HelloWorld"), "helloWorld");
        assert_eq!(to_camel_case("already"), "already");
        assert_eq!(to_pascal_case("hello_world"), "HelloWorld");
        assert_eq!(to_pascal_case("helloWorld"), "HelloWorld");
        assert_eq!(to_pascal_case("Already"), "Already");
    }

    #[test]
    fn escape_go_string_special_chars() {
        assert_eq!(escape_go_string("hello\nworld"), "hello\\nworld");
        assert_eq!(escape_go_string("tab\there"), "tab\\there");
        assert_eq!(escape_go_string("quote\"here"), "quote\\\"here");
    }

    // ── End-to-end: syntax validation ───────────────────────────────────────

    #[test]
    #[ignore] // requires `go` to be installed
    fn generated_go_passes_vet() {
        let body = block(
            2,
            vec![],
            Some(node(
                3,
                NodeKind::Interpolation {
                    parts: vec![
                        AirInterpolationPart::Literal("Hello, ".into()),
                        AirInterpolationPart::Expr(Box::new(id_node(4, "name"))),
                    ],
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("greet"),
                generic_params: vec![],
                params: vec![typed_param_node(5, "name", "String")],
                return_type: Some(Box::new(node(
                    6,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let code = gen(&module(vec![], vec![f]));

        // Write to temp file and run go vet.
        let dir = std::env::temp_dir().join("bock_go_test");
        let _ = std::fs::create_dir_all(&dir);
        let file_path = dir.join("output.go");
        std::fs::write(&file_path, &code).unwrap();

        let output = std::process::Command::new("go")
            .args(["vet", file_path.to_str().unwrap()])
            .output();
        match output {
            Ok(o) => {
                if !o.status.success() {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    panic!("go vet failed:\n{stderr}\n\nGenerated code:\n{code}");
                }
            }
            Err(e) => {
                panic!("Failed to run go vet: {e}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore] // requires `go` to be installed
    fn generated_go_compiles_and_runs() {
        // Build a complete Go program that prints "42".
        let body = block(
            2,
            vec![node(
                3,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(4, "x")),
                    ty: None,
                    value: Box::new(int_lit(5, "42")),
                },
            )],
            None,
        );
        let main_fn = node(
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
        let code = gen(&module(vec![], vec![main_fn]));

        let dir = std::env::temp_dir().join("bock_go_run_test");
        let _ = std::fs::create_dir_all(&dir);
        let file_path = dir.join("main.go");
        std::fs::write(&file_path, &code).unwrap();

        let output = std::process::Command::new("go")
            .args(["build", file_path.to_str().unwrap()])
            .current_dir(&dir)
            .output();
        match output {
            Ok(o) => {
                if !o.status.success() {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    panic!("go build failed:\n{stderr}\n\nGenerated code:\n{code}");
                }
            }
            Err(e) => {
                panic!("Failed to run go build: {e}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expr_match_no_unused_var() {
        // Expression-position match should not emit unused `__v`.
        let match_expr = node(
            1,
            NodeKind::Match {
                scrutinee: Box::new(id_node(2, "x")),
                arms: vec![
                    node(
                        3,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(
                                4,
                                NodeKind::LiteralPat {
                                    lit: Literal::Int("1".into()),
                                },
                            )),
                            guard: None,
                            body: Box::new(block(5, vec![], Some(str_lit(6, "one")))),
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
        // Emit in expression context via a let binding.
        let let_node = node(
            20,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(21, "result")),
                ty: None,
                value: Box::new(match_expr),
            },
        );
        let out = gen(&module(vec![], vec![let_node]));
        assert!(
            !out.contains("__v"),
            "expression-position match should not emit __v, got: {out}"
        );
        assert!(
            out.contains("switch x"),
            "should emit switch with scrutinee directly, got: {out}"
        );
    }

    // ── Prelude function mapping tests ──────────────────────────────────────

    /// Helper: generate Go for a module with a `main` function containing a single call.
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

    /// Helper: generate Go for a nullary prelude call (no args).
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
    fn prelude_println_maps_to_fmt_println() {
        let out = gen_prelude_call("println", str_lit(12, "hello"));
        assert!(
            out.contains("fmt.Println("),
            "println should map to fmt.Println, got: {out}"
        );
        assert!(
            !out.contains("println("),
            "should not emit bare println(, got: {out}"
        );
    }

    #[test]
    fn prelude_print_maps_to_fmt_print() {
        let out = gen_prelude_call("print", str_lit(12, "hello"));
        assert!(
            out.contains("fmt.Print("),
            "print should map to fmt.Print, got: {out}"
        );
    }

    #[test]
    fn prelude_debug_maps_to_fmt_printf() {
        let out = gen_prelude_call("debug", str_lit(12, "val"));
        assert!(
            out.contains("fmt.Printf(\"%+v\\n\", "),
            "debug should map to fmt.Printf, got: {out}"
        );
    }

    #[test]
    fn prelude_assert_maps_to_panic() {
        let out = gen_prelude_call("assert", bool_lit(12, true));
        assert!(
            out.contains("if !true { panic(\"assertion failed\") }"),
            "assert should map to if-panic, got: {out}"
        );
    }

    #[test]
    fn prelude_todo_maps_to_panic_not_implemented() {
        let out = gen_prelude_call_no_args("todo");
        assert!(
            out.contains("panic(\"not implemented\")"),
            "todo should map to panic, got: {out}"
        );
    }

    #[test]
    fn prelude_unreachable_maps_to_panic_unreachable() {
        let out = gen_prelude_call_no_args("unreachable");
        assert!(
            out.contains("panic(\"unreachable\")"),
            "unreachable should map to panic, got: {out}"
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
        // Go: inner(__logger)
        assert!(
            out.contains("inner(__logger)"),
            "handling block should pass handler to effectful call, got: {out}"
        );
        assert!(
            out.contains("__logger := stdoutLogger()"),
            "handling block should instantiate handler, got: {out}"
        );
    }

    // ── C.8 Go effect codegen polish tests ──────────────────────────────────

    fn type_named_node(id: u32, name: &str) -> AIRNode {
        node(
            id,
            NodeKind::TypeNamed {
                path: type_path(&[name]),
                args: vec![],
            },
        )
    }

    /// Effect interface: Void-returning operations emit no return type.
    #[test]
    fn effect_interface_drops_void_return_type() {
        let void_op = node(
            2,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("log"),
                generic_params: vec![],
                params: vec![typed_param_node(3, "msg", "String")],
                return_type: Some(Box::new(type_named_node(4, "Void"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(5, vec![], None)),
            },
        );
        let effect_decl = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Logger"),
                generic_params: vec![],
                components: vec![],
                operations: vec![void_op],
            },
        );
        let out = gen(&module(vec![], vec![effect_decl]));
        assert!(
            out.contains("type Logger interface {"),
            "should emit interface, got: {out}"
        );
        assert!(
            out.contains("Log(string)\n"),
            "Void op should have no return type, got: {out}"
        );
        assert!(
            !out.contains("Log(string) struct{}"),
            "Void op should NOT emit struct{{}} return, got: {out}"
        );
    }

    /// Public effectful function: Void return type is dropped in Go signature.
    #[test]
    fn fn_decl_drops_void_return_type() {
        let f = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("do_thing"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(type_named_node(11, "Void"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(12, vec![], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("func DoThing() {"),
            "Void fn should have no return type, got: {out}"
        );
        assert!(
            !out.contains("DoThing() struct{}"),
            "should not emit struct{{}} return, got: {out}"
        );
    }

    /// Public function call sites emit PascalCase matching their definition.
    #[test]
    fn call_site_uses_pascal_case_for_public_fn() {
        let pub_fn = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("do_thing"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(12, vec![], None)),
            },
        );
        let call = node(
            20,
            NodeKind::Call {
                callee: Box::new(id_node(21, "do_thing")),
                args: vec![],
                type_args: vec![],
            },
        );
        let main_fn = node(
            30,
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
                body: Box::new(block(31, vec![], Some(call))),
            },
        );
        let out = gen(&module(vec![], vec![pub_fn, main_fn]));
        assert!(
            out.contains("DoThing()"),
            "call to public fn should be PascalCase, got: {out}"
        );
        assert!(
            !out.contains("doThing()"),
            "call should NOT use camelCase for public fn, got: {out}"
        );
    }

    /// Trait/effect impl blocks use value receivers so `Handler{}` satisfies the interface.
    #[test]
    fn impl_block_methods_use_value_receivers() {
        let record_decl = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("StdoutLogger"),
                generic_params: vec![],
                fields: vec![],
            },
        );
        let method = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("log"),
                generic_params: vec![],
                params: vec![typed_param_node(11, "msg", "String")],
                return_type: Some(Box::new(type_named_node(12, "Void"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(13, vec![], None)),
            },
        );
        let impl_block = node(
            20,
            NodeKind::ImplBlock {
                annotations: vec![],
                target: Box::new(type_named_node(21, "StdoutLogger")),
                trait_path: Some(type_path(&["Logger"])),
                generic_params: vec![],
                where_clause: vec![],
                methods: vec![method],
            },
        );
        let out = gen(&module(vec![], vec![record_decl, impl_block]));
        assert!(
            out.contains("func (s StdoutLogger) Log("),
            "impl method should use value receiver, got: {out}"
        );
        assert!(
            !out.contains("func (s *StdoutLogger) Log("),
            "impl method should NOT use pointer receiver, got: {out}"
        );
    }

    /// Module-level `handle` declares a var AND registers it so module-level
    /// calls to effectful functions pick it up.
    #[test]
    fn module_handle_registers_handler_for_calls() {
        use bock_air::AirHandlerPair;
        let _ = AirHandlerPair {
            effect: type_path(&["Logger"]),
            handler: Box::new(str_lit(999, "placeholder")),
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
                        return_type: Some(Box::new(type_named_node(4, "Void"))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(5, vec![], None)),
                    },
                )],
            },
        );

        let effectful_fn = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("do_log"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![type_path(&["Logger"])],
                where_clause: vec![],
                body: Box::new(block(11, vec![], None)),
            },
        );

        let module_handle = node(
            20,
            NodeKind::ModuleHandle {
                effect: type_path(&["Logger"]),
                handler: Box::new(node(
                    21,
                    NodeKind::Call {
                        callee: Box::new(id_node(22, "StdoutLogger")),
                        args: vec![],
                        type_args: vec![],
                    },
                )),
            },
        );

        let main_call = node(
            30,
            NodeKind::Call {
                callee: Box::new(id_node(31, "do_log")),
                args: vec![],
                type_args: vec![],
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
                body: Box::new(block(41, vec![], Some(main_call))),
            },
        );

        let out = gen(&module(
            vec![],
            vec![effect_decl, effectful_fn, module_handle, main_fn],
        ));
        assert!(
            out.contains("var __logger Logger = stdoutLogger()"),
            "module handle should declare var, got: {out}"
        );
        assert!(
            out.contains("DoLog(__logger)"),
            "module-level call should receive __logger, got: {out}"
        );
    }

    /// Handling block suppresses Go "declared but not used" errors for handler vars.
    #[test]
    fn handling_block_emits_unused_suppression() {
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
                        return_type: Some(Box::new(type_named_node(4, "Void"))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(5, vec![], None)),
                    },
                )],
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
                body: Box::new(block(33, vec![], Some(str_lit(34, "body")))),
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
        let out = gen(&module(vec![], vec![effect_decl, main_fn]));
        assert!(
            out.contains("_ = __logger"),
            "should suppress unused-var error for handler, got: {out}"
        );
    }

    /// Void effect operations (e.g., log) are not wrapped in `return` when a
    /// tail expression in a Void-returning function.
    #[test]
    fn void_effect_op_tail_not_wrapped_in_return() {
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
                        return_type: Some(Box::new(type_named_node(4, "Void"))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(5, vec![], None)),
                    },
                )],
            },
        );
        let log_call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "log")),
                args: vec![bock_air::AirArg {
                    label: None,
                    value: str_lit(12, "hello"),
                }],
                type_args: vec![],
            },
        );
        let caller = node(
            20,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("do_log"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(type_named_node(21, "Void"))),
                effect_clause: vec![type_path(&["Logger"])],
                where_clause: vec![],
                body: Box::new(block(22, vec![], Some(log_call))),
            },
        );
        let out = gen(&module(vec![], vec![effect_decl, caller]));
        assert!(
            out.contains("logger.Log("),
            "effect op should be rewritten as handler.Method, got: {out}"
        );
        assert!(
            !out.contains("return logger.Log("),
            "Void effect op in Void fn should NOT be preceded by `return`, got: {out}"
        );
    }
}
