//! Python code generator — rule-based (Tier 2) transpilation from AIR to Python.
//!
//! Handles all capability gaps:
//! - Records → `@dataclass` classes
//! - Algebraic types → dataclasses with `_tag` discriminant
//! - Pattern matching → native `match`/`case` (Python 3.10+)
//! - Effects → keyword arguments
//! - Ownership → erased (Python is GC)
//! - Generics → `TypeVar` + `Generic[T]` (so `T` resolves at class-eval time)
//! - Type hints on all declarations

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::Write;
use std::path::PathBuf;

use bock_air::{AIRNode, AirInterpolationPart, EnumVariantPayload, NodeKind, ResultVariant};
use bock_ast::{AssignOp, BinOp, Literal, TypeExpr, UnaryOp, Visibility};
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

/// True if the module references `Optional`, `Some`, or `None` anywhere, so the
/// Optional runtime prelude must be emitted. A cheap structural scan over the
/// debug rendering, mirroring [`py_module_uses_concurrency`] and the Go/TS
/// backends' `*_module_uses_optional`.
fn py_module_uses_optional(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Optional\"")
            || s.contains("TypeOptional")
            || s.contains("\"Some\"")
            || s.contains("\"None\"")
    })
}

/// True if the module references `Result`, `Ok`, or `Err` anywhere, so the
/// `Result` runtime prelude must be emitted. Mirrors [`py_module_uses_optional`].
fn py_module_uses_result(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Result\"")
            || s.contains("ResultConstruct")
            || s.contains("\"Ok\"")
            || s.contains("\"Err\"")
    })
}

/// True if the module references the prelude `Ordering` enum, any of its
/// variants, or a `compare` method call (which the primitive bridge lowers to an
/// `Ordering` runtime value). Gates emission of [`ORDERING_RUNTIME_PY`], mirroring
/// [`py_module_uses_optional`].
fn py_module_uses_ordering(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Ordering\"")
            || s.contains("\"Less\"")
            || s.contains("\"Equal\"")
            || s.contains("\"Greater\"")
            || s.contains("\"compare\"")
    })
}

/// Runtime for Bock `Optional[T]` in Python. The *value* representation mirrors
/// JS/TS/Go: a tagged value with a `Some` payload or a `None` marker. Python's
/// `None` is a keyword (and a distinct concept), so Bock's `None` must NOT
/// collide with it — it lowers to the singleton `_bock_none`, and `Some(x)` to
/// `_BockSome(x)`. `__match_args__` lets `case _BockSome(v):` bind the payload
/// positionally; `case _BockNone():` matches the marker. This keeps type and
/// value in agreement and makes `match o { Some(x) => ...; None => ... }` lower
/// to valid structural pattern matching (the old codegen emitted bare
/// `Some`/`None` with no definitions and `case None():`, a `SyntaxError`).
const OPTIONAL_RUNTIME_PY: &str = "\
# ── Bock Optional runtime ──
class _BockSome:
    __match_args__ = ('_0',)
    __slots__ = ('_0',)
    def __init__(self, _0):
        self._0 = _0
    def __repr__(self):
        return f'Some({self._0!r})'

class _BockNone:
    __slots__ = ()
    def __repr__(self):
        return 'None'

_bock_none = _BockNone()
";

/// Runtime for Bock `Result[T, E]` in Python. Mirrors `OPTIONAL_RUNTIME_PY`: the
/// `Ok` payload and the `Err` payload each live in a distinct class with
/// `__match_args__` so `case _BockOk(v):` / `case _BockErr(e):` bind the payload
/// positionally — the same shape the surface `Ok(..)`/`Err(..)` construction
/// emits (`_BockOk(..)` / `_BockErr(..)`). The old codegen emitted bare
/// `Ok(..)`/`case Ok(_0=n):` against undefined names; this keeps construction and
/// match in agreement on the same runtime classes.
const RESULT_RUNTIME_PY: &str = "\
# ── Bock Result runtime ──
class _BockOk:
    __match_args__ = ('_0',)
    __slots__ = ('_0',)
    def __init__(self, _0):
        self._0 = _0
    def __repr__(self):
        return f'Ok({self._0!r})'

class _BockErr:
    __match_args__ = ('_0',)
    __slots__ = ('_0',)
    def __init__(self, _0):
        self._0 = _0
    def __repr__(self):
        return f'Err({self._0!r})'
";

/// The prelude `Ordering` runtime: the three variants of `core.compare.Ordering`
/// as singleton instances of distinct classes, matchable by `case` and emitted
/// for construction. Mirrors `OPTIONAL_RUNTIME_PY` — the `core.compare` enum
/// declaration is not bundled into the single-file entry, so the primitive
/// bridge (`(x).compare(y)`) and any bare `Less`/`Equal`/`Greater` need this
/// self-contained representation. Each class is empty (no payload), so
/// `case _BockOrderingLess():` matches the corresponding singleton.
const ORDERING_RUNTIME_PY: &str = "\
# ── Bock Ordering runtime ──
class _BockOrderingLess:
    __slots__ = ()
    def __repr__(self):
        return 'Less'

class _BockOrderingEqual:
    __slots__ = ()
    def __repr__(self):
        return 'Equal'

class _BockOrderingGreater:
    __slots__ = ()
    def __repr__(self):
        return 'Greater'

_bock_less = _BockOrderingLess()
_bock_equal = _BockOrderingEqual()
_bock_greater = _BockOrderingGreater()
";

/// The Ordering-runtime *singleton* name for an `Ordering` variant
/// (`Less`→`_bock_less`, …). Used at construction sites.
fn ordering_singleton_py(variant: &str) -> &'static str {
    match variant {
        "Less" => "_bock_less",
        "Equal" => "_bock_equal",
        _ => "_bock_greater",
    }
}

/// The Ordering-runtime *class* name for an `Ordering` variant
/// (`Less`→`_BockOrderingLess`, …). Used as a `case` pattern.
fn ordering_class_py(variant: &str) -> &'static str {
    match variant {
        "Less" => "_BockOrderingLess",
        "Equal" => "_BockOrderingEqual",
        _ => "_BockOrderingGreater",
    }
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
        ctx.enum_variants =
            crate::generator::collect_enum_variants(&[(module, std::path::Path::new(""))]);
        ctx.trait_decls =
            crate::generator::collect_trait_decls(&[(module, std::path::Path::new(""))]);
        ctx.emit_node(module)?;
        let content = ctx.finish();
        let source_map = SourceMap {
            generated_file: String::new(),
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
            Some("if __name__ == \"__main__\":\n    asyncio.run(main())\n".to_string())
        } else {
            Some("if __name__ == \"__main__\":\n    main()\n".to_string())
        }
    }

    /// Bundle every module (stdlib + user, dependency-ordered) into one entry
    /// file. Python module top-level defs share one namespace, so concatenating
    /// each module's defs is valid and resolves cross-module `use` (DV13).
    /// `ImportDecl`s are dropped; `import` preamble lines and runtime preludes
    /// are emitted once. `finish` prepends the merged `import …` preamble after
    /// all module bodies are emitted (so accumulated `needs_*` flags are seen).
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

        let mut ctx = PyEmitCtx::new();
        ctx.enum_variants = crate::generator::collect_enum_variants(modules);
        ctx.trait_decls = crate::generator::collect_trait_decls(modules);
        for (i, (module, _)) in modules.iter().enumerate() {
            if i > 0 && !ctx.buf.is_empty() && !ctx.buf.ends_with("\n\n") {
                ctx.buf.push('\n');
            }
            ctx.emit_node(module)?;
        }
        let mut content = ctx.finish();

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
    /// Set once the Optional runtime prelude has been emitted, so a single-file
    /// **bundle** of several modules (cross-module `use`, DV13) emits it at most
    /// once (redefining the `_BockSome`/`_BockNone` helpers is wasteful and
    /// risks shadowing surprises).
    optional_runtime_emitted: bool,
    /// Set once the `Result` runtime prelude has been emitted; deduped across a
    /// bundle exactly as [`Self::optional_runtime_emitted`] (redefining the
    /// `_BockOk`/`_BockErr` classes is wasteful).
    result_runtime_emitted: bool,
    /// Set once the [`ORDERING_RUNTIME_PY`] prelude has been emitted; deduped
    /// across a bundle exactly as [`Self::optional_runtime_emitted`].
    ordering_runtime_emitted: bool,
    /// Set once the concurrency runtime prelude has been emitted; deduped across
    /// a bundle exactly as [`Self::optional_runtime_emitted`].
    concurrency_runtime_emitted: bool,
    /// Set when an enum decl emits a `Name = Union[...]` alias, so the preamble
    /// imports `Union` from `typing`.
    needs_union_import: bool,
    /// Typing-import needs accumulated while lowering type annotations. These
    /// are `Cell`s because `type_to_py`/`ast_type_to_py` (where the relevant
    /// `Callable`/`Any`/`Self`/`Never`/`TypeVar` names are emitted) take
    /// `&self` — many of their call sites borrow `self` immutably inside
    /// closures, so promoting them to `&mut self` would fight the borrow
    /// checker. The `finish` preamble reads them to emit a single merged
    /// `from typing import …` line.
    needs_typing_callable: Cell<bool>,
    needs_typing_any: Cell<bool>,
    needs_typing_self: Cell<bool>,
    needs_typing_never: Cell<bool>,
    /// Set when a generic decl emits `T = TypeVar("T")`, so the preamble
    /// imports `TypeVar` (and `Generic`, used in the class base list).
    needs_typing_typevar: Cell<bool>,
    /// Names already emitted as `T = TypeVar("T")`, deduped across the bundle so
    /// a type parameter shared by several decls is declared exactly once.
    emitted_typevars: std::collections::HashSet<String>,
    /// User-enum-variant registry (DV14). Routes a construction/pattern to the
    /// `{enum}_{variant}` dataclass and recognises a unit variant (needs `()`
    /// instantiation). Built-in Optional/Result pre-seeds filtered out where
    /// the bespoke `_BockSome`/`_BockNone` lowering applies. Pre-scanned across
    /// the bundle.
    enum_variants: crate::generator::EnumVariantRegistry,
    /// The bundle's user-declared traits (keyed by name). Distinguishes a
    /// `T: Equatable` bound that is a real user trait from the compiler-provided
    /// sealed-core conformance, which must drop the `bound=` on the `TypeVar` and
    /// lower `.eq`/`.compare` to native operators (GAP-C). See
    /// [`crate::generator::is_unimplemented_sealed_core_trait`].
    trait_decls: crate::generator::TraitDeclRegistry,
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
            optional_runtime_emitted: false,
            result_runtime_emitted: false,
            ordering_runtime_emitted: false,
            concurrency_runtime_emitted: false,
            needs_union_import: false,
            needs_typing_callable: Cell::new(false),
            needs_typing_any: Cell::new(false),
            needs_typing_self: Cell::new(false),
            needs_typing_never: Cell::new(false),
            needs_typing_typevar: Cell::new(false),
            emitted_typevars: std::collections::HashSet::new(),
            enum_variants: crate::generator::EnumVariantRegistry::new(),
            trait_decls: crate::generator::TraitDeclRegistry::new(),
        }
    }

    fn finish(mut self) -> String {
        // An empty module emits nothing at all — not even a preamble (an empty
        // `.py` is the expected output, and a bare `from __future__` import on
        // its own would be surprising noise).
        if self.buf.is_empty() {
            return self.buf;
        }
        let mut preamble = String::new();
        // PEP 563: defer evaluation of every annotation to a string. A method
        // declared inside a class body that annotates a parameter with the class
        // itself — `class Tag: def equals(self, other: Tag)`, emitted for an
        // `impl Eq for Tag` whose `other: Self` resolves to `Tag` — references a
        // name that is not yet bound while the class body executes, raising
        // `NameError` at import time. `from __future__ import annotations` makes
        // all annotations lazy strings, so such (and any other) forward
        // references never evaluate eagerly. It must be the first statement in
        // the module, so it is prepended ahead of every other import.
        preamble.push_str("from __future__ import annotations\n");
        if self.needs_asyncio_import {
            preamble.push_str("import asyncio\n");
        }
        if self.needs_time_import {
            preamble.push_str("import time\n");
        }
        // Merge every `typing` need into one `from typing import …` line so a
        // module that uses, e.g., both a `Callable` annotation and a generic
        // type does not emit two separate (potentially conflicting) imports.
        let mut typing_names: Vec<&str> = Vec::new();
        if self.needs_union_import {
            typing_names.push("Union");
        }
        if self.needs_typing_callable.get() {
            typing_names.push("Callable");
        }
        if self.needs_typing_any.get() {
            typing_names.push("Any");
        }
        if self.needs_typing_self.get() {
            typing_names.push("Self");
        }
        if self.needs_typing_never.get() {
            typing_names.push("Never");
        }
        if self.needs_typing_typevar.get() {
            // `Generic` is always paired with `TypeVar`: a generic class lists
            // `Generic[T, …]` in its bases and `T` is a `TypeVar`.
            typing_names.push("TypeVar");
            typing_names.push("Generic");
        }
        if !typing_names.is_empty() {
            // Stable, de-duplicated ordering for deterministic output.
            typing_names.sort_unstable();
            typing_names.dedup();
            preamble.push_str(&format!("from typing import {}\n", typing_names.join(", ")));
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

    /// Variant info for `path` when its last segment is a registered *user*
    /// enum variant (built-in Optional/Result pre-seeds excluded — those go
    /// through the bespoke `_BockSome`/`_BockNone` lowering).
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

    /// Render a pattern node to a Python `case` sub-pattern string by running
    /// [`Self::emit_pattern`] against a scratch slice of the buffer. Lets a
    /// constructor / record field embed a *nested* sub-pattern (`_BockSome(x)`,
    /// `_BockOk(v)`, a nested tuple) instead of a flat binding name — the fix for
    /// `Some(Ok(v))` losing its inner bindings.
    fn pattern_to_py(&mut self, pat: &AIRNode) -> Result<String, CodegenError> {
        let start = self.buf.len();
        self.emit_pattern(pat)?;
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
            // Optional `Some(x)` constructor → tagged runtime value (see
            // `OPTIONAL_RUNTIME_PY`). `None` is not a call; it lowers in the
            // `Identifier` arm to the `_bock_none` singleton.
            "Some" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("_BockSome({a})")
            }
            // Result `Ok(x)` / `Err(e)` constructors → tagged runtime values
            // (see `RESULT_RUNTIME_PY`), mirroring the `Some` handling above so
            // construction agrees with the `case _BockOk(..)` / `_BockErr(..)`
            // match arms.
            "Ok" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("_BockOk({a})")
            }
            "Err" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("_BockErr({a})")
            }
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

    /// Emit a built-in `Optional`/`Result` method call to its Python form.
    ///
    /// Recognised via the checker's `recv_kind` annotation
    /// ([`crate::generator::desugared_optional_method`] /
    /// [`crate::generator::desugared_result_method`]) so the overloaded names
    /// (`unwrap`/`unwrap_or`/`map`) dispatch to the right `isinstance` test on the
    /// tagged runtime classes (`_BockSome`/`_BockOk` carry the payload as `._0`).
    /// The receiver is bound once in a `lambda` so it is evaluated exactly once.
    /// Returns `true` if handled.
    fn try_emit_container_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        if let Some((recv, method, rest)) =
            crate::generator::desugared_optional_method(node, callee, args)
        {
            // Optional: present = `_BockSome`; the "map" reconstruction also uses
            // `_BockSome` for the present case and the receiver `__c` (a
            // `_bock_none`) for the empty case.
            self.emit_tagged_container_method(recv, method, rest, "_BockSome", "_BockSome")?;
            return Ok(true);
        }
        if let Some((recv, method, rest)) =
            crate::generator::desugared_result_method(node, callee, args)
        {
            self.emit_tagged_container_method(recv, method, rest, "_BockOk", "_BockErr")?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Lower a tagged-container method on `recv`. `present_cls` is the
    /// payload-carrying runtime class (`_BockSome`/`_BockOk`); `err_cls` is the
    /// other class (`_BockNone` for Optional — unused as a constructor since the
    /// empty case passes the receiver through; `_BockErr` for Result, used by
    /// `map_err`).
    fn emit_tagged_container_method(
        &mut self,
        recv: &AIRNode,
        method: &str,
        rest: &[bock_air::AirArg],
        present_cls: &str,
        err_cls: &str,
    ) -> Result<(), CodegenError> {
        // Tag tests read the receiver once → emit inline.
        match method {
            "is_some" | "is_ok" => {
                self.buf.push_str("isinstance(");
                self.emit_expr(recv)?;
                let _ = write!(self.buf, ", {present_cls})");
                return Ok(());
            }
            "is_none" | "is_err" => {
                self.buf.push_str("(not isinstance(");
                self.emit_expr(recv)?;
                let _ = write!(self.buf, ", {present_cls}))");
                return Ok(());
            }
            _ => {}
        }
        self.buf.push_str("(lambda __c: ");
        match method {
            "unwrap" => {
                let _ = write!(
                    self.buf,
                    "__c._0 if isinstance(__c, {present_cls}) else None"
                );
            }
            "unwrap_or" => {
                let _ = write!(self.buf, "__c._0 if isinstance(__c, {present_cls}) else (");
                if let Some(d) = rest.first() {
                    self.emit_expr(&d.value)?;
                } else {
                    self.buf.push_str("None");
                }
                self.buf.push(')');
            }
            "map" => {
                let _ = write!(self.buf, "{present_cls}((");
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf.push_str("lambda x: x");
                }
                let _ = write!(
                    self.buf,
                    ")(__c._0)) if isinstance(__c, {present_cls}) else __c"
                );
            }
            "flat_map" => {
                let _ = write!(self.buf, "(");
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf.push_str("lambda x: x");
                }
                let _ = write!(
                    self.buf,
                    ")(__c._0) if isinstance(__c, {present_cls}) else __c"
                );
            }
            "map_err" => {
                let _ = write!(self.buf, "{err_cls}((");
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf.push_str("lambda x: x");
                }
                let _ = write!(
                    self.buf,
                    ")(__c._0)) if isinstance(__c, {err_cls}) else __c"
                );
            }
            _ => self.buf.push_str("None"),
        }
        self.buf.push_str(")(");
        self.emit_expr(recv)?;
        self.buf.push(')');
        Ok(())
    }

    /// Emit a read-only `List` built-in method call to its Python form.
    ///
    /// Python lists are native, so `len`/`is_empty`/`contains`/`concat` map to
    /// `len(r)`/`(len(r) == 0)`/`(x in r)`/`(r + o)`. `Optional`-returning
    /// methods (`get`/`first`/`last`/`index_of`) build the tagged Optional
    /// runtime values (`_BockSome(v)` / `_bock_none`); they wrap the receiver in
    /// a `lambda` so it is evaluated exactly once.
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
                self.buf.push_str("len(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "is_empty" => {
                self.buf.push_str("(len(");
                self.emit_expr(recv)?;
                self.buf.push_str(") == 0)");
            }
            "get" => {
                let Some(idx) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("(lambda __r, __i: _BockSome(__r[__i]) if 0 <= __i < len(__r) else _bock_none)(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&idx.value)?;
                self.buf.push(')');
            }
            "first" => {
                self.buf
                    .push_str("(lambda __r: _BockSome(__r[0]) if len(__r) > 0 else _bock_none)(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "last" => {
                self.buf
                    .push_str("(lambda __r: _BockSome(__r[-1]) if len(__r) > 0 else _bock_none)(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "contains" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(&x.value)?;
                self.buf.push_str(" in ");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "index_of" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "(lambda __r, __x: _BockSome(__r.index(__x)) if __x in __r else _bock_none)(",
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
                self.buf.push_str(" + ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "join" => {
                let Some(sep) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(&sep.value)?;
                self.buf.push_str(").join(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Emit a built-in `Map[K, V]` method call to its Python form (native
    /// `dict`).
    ///
    /// Recognised via [`crate::generator::desugared_map_method`] (gated on
    /// `recv_kind = "Map"`) and wired *before* [`Self::try_emit_list_method`],
    /// so a `Map` receiver's `get`/`contains_key`/`len` no longer route through
    /// the `List` path (where `get` would index `__m[__i]` instead of testing
    /// key membership, and `set`/`contains_key` would call non-existent `dict`
    /// methods). `get` returns the tagged `Optional` rep
    /// (`_BockSome(v)`/`_bock_none`). Mutating methods (`set`/`delete`/`merge`)
    /// mutate in place via the `(side_effect, recv)[1]` tuple idiom (Python
    /// lambdas are expression-only) and return the receiver. Returns `true` if
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
                self.buf.push_str("len(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "is_empty" => {
                self.buf.push_str("(len(");
                self.emit_expr(recv)?;
                self.buf.push_str(") == 0)");
            }
            "contains_key" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(&k.value)?;
                self.buf.push_str(" in ");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "get" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str(
                    "(lambda __m, __k: _BockSome(__m[__k]) if __k in __m else _bock_none)(",
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
                    .push_str("(lambda __m, __k, __v: (__m.__setitem__(__k, __v), __m)[1])(");
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
                    .push_str("(lambda __m, __k: (__m.pop(__k, None), __m)[1])(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&k.value)?;
                self.buf.push(')');
            }
            "merge" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("(lambda __m, __o: (__m.update(__o), __m)[1])(");
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
                    "(lambda __m, __f: {__k: __v for __k, __v in __m.items() if __f(__k, __v)})(",
                );
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&f.value)?;
                self.buf.push(')');
            }
            "keys" => {
                self.buf.push_str("list(");
                self.emit_expr(recv)?;
                self.buf.push_str(".keys())");
            }
            "values" => {
                self.buf.push_str("list(");
                self.emit_expr(recv)?;
                self.buf.push_str(".values())");
            }
            "entries" | "to_list" => {
                self.buf.push_str("list(");
                self.emit_expr(recv)?;
                self.buf.push_str(".items())");
            }
            "for_each" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str("[(");
                self.emit_expr(&f.value)?;
                self.buf.push_str(")(__k, __v) for __k, __v in (");
                self.emit_expr(recv)?;
                self.buf.push_str(").items()]");
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Emit a built-in `Set[E]` method call to its Python form (native `set`).
    ///
    /// Recognised via [`crate::generator::desugared_set_method`] (gated on
    /// `recv_kind = "Set"`) and wired *before* [`Self::try_emit_list_method`].
    /// Set algebra maps to Python's operators (`|`/`&`/`-`/`<=`/`>=`). Mutating
    /// methods (`add`/`remove`) mutate in place via the `(side_effect, recv)[1]`
    /// idiom and return the receiver.
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
                self.buf.push_str("len(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "is_empty" => {
                self.buf.push_str("(len(");
                self.emit_expr(recv)?;
                self.buf.push_str(") == 0)");
            }
            "contains" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(&x.value)?;
                self.buf.push_str(" in ");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "add" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.buf
                    .push_str("(lambda __s, __x: (__s.add(__x), __s)[1])(");
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
                    .push_str("(lambda __s, __x: (__s.discard(__x), __s)[1])(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&x.value)?;
                self.buf.push(')');
            }
            "union" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(" | ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "intersection" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(" & ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "difference" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(" - ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "is_subset" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(" <= ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "is_superset" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push('(');
                self.emit_expr(recv)?;
                self.buf.push_str(" >= ");
                self.emit_expr(&o.value)?;
                self.buf.push(')');
            }
            "filter" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str("{__x for __x in (");
                self.emit_expr(recv)?;
                self.buf.push_str(") if (");
                self.emit_expr(&f.value)?;
                self.buf.push_str(")(__x)}");
            }
            "map" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str("{(");
                self.emit_expr(&f.value)?;
                self.buf.push_str(")(__x) for __x in (");
                self.emit_expr(recv)?;
                self.buf.push_str(")}");
            }
            "to_list" => {
                self.buf.push_str("list(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "for_each" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str("[(");
                self.emit_expr(&f.value)?;
                self.buf.push_str(")(__x) for __x in (");
                self.emit_expr(recv)?;
                self.buf.push_str(")]");
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Lower a primitive trait-bridge method call (`compare`/`eq`/`to_string`/
    /// `display` on a primitive receiver) to its Python form.
    ///
    /// `(1).compare(2)` resolves to `Ordering`; this produces the
    /// Ordering-runtime singleton (`_bock_less` / `_bock_equal` /
    /// `_bock_greater`) via a conditional expression, matching the
    /// construction/`case` sides. `eq` → `==`; `to_string`/`display` → `str(x)`.
    /// Lower a desugared `String` built-in method call (`recv_kind =
    /// "Primitive:String"`) to its native Python string op. Wired into the
    /// `Call` arm *before* `try_emit_list_method` so a String receiver's
    /// `len`/`contains`/`is_empty` dispatch here, not through the List path.
    ///
    /// `len` is the Unicode SCALAR count: Python `str` is a sequence of code
    /// points, so `len(s)` already yields the scalar count (spec §18.3).
    /// `byte_len` encodes to UTF-8 first (`len(s.encode())`). `replace` replaces
    /// ALL occurrences (Python's default). `split` returns a Python list, the
    /// List runtime rep.
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
            "len" | "length" | "count" => format!("len({recv_str})"),
            "byte_len" => format!("len(({recv_str}).encode())"),
            "is_empty" => format!("(len({recv_str}) == 0)"),
            "to_upper" => format!("({recv_str}).upper()"),
            "to_lower" => format!("({recv_str}).lower()"),
            "trim" => format!("({recv_str}).strip()"),
            "contains" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!("(({p}) in ({recv_str}))")
            }
            "starts_with" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).startswith({p})")
            }
            "ends_with" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).endswith({p})")
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
                format!("({recv_str}).replace({from}, {to})")
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
        self.emit_bridge_method(recv, method, rest)
    }

    /// Lower a sealed-core-trait bridge method on a *bounded generic type
    /// variable* (`a.eq(b)` / `a.compare(b)` inside `eq_check[T: Equatable]`) to
    /// its Python form (GAP-C). The method body is identical to the
    /// `Primitive:<Ty>` bridge; the `bound=Equatable` on the `TypeVar` is
    /// separately dropped (see the generic-decl emission). Fires only when the
    /// bound trait is sealed-core and NOT a user-declared trait.
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
        self.emit_bridge_method(recv, method, rest)
    }

    /// Shared body of the primitive / trait-bound bridges: emit the native Python
    /// form of `compare` (the `_bock_less`/`_bock_equal`/`_bock_greater`
    /// conditional), `eq` (`==`), or `to_string`/`display` (`str(..)`).
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
                    "(_bock_less if ({recv_str}) < ({other}) else \
                     (_bock_equal if ({recv_str}) == ({other}) else _bock_greater))"
                );
            }
            "eq" => {
                let Some(other) = rest.first() else {
                    return Ok(false);
                };
                let other = self.expr_to_string(&other.value)?;
                let _ = write!(self.buf, "(({recv_str}) == ({other}))");
            }
            "to_string" | "display" => {
                let _ = write!(self.buf, "str({recv_str})");
            }
            _ => return Ok(false),
        }
        Ok(true)
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
            NodeKind::Module { items, .. } => {
                // Cross-module `use` (DV13) → single-file bundling: every
                // module's top-level declarations are concatenated into the one
                // entry file and `ImportDecl`s are dropped. Each runtime prelude
                // is emitted at most once across the bundle, gated on a ctx flag.
                if !self.optional_runtime_emitted && py_module_uses_optional(items) {
                    self.buf.push_str(OPTIONAL_RUNTIME_PY);
                    self.buf.push('\n');
                    self.optional_runtime_emitted = true;
                }
                if !self.result_runtime_emitted && py_module_uses_result(items) {
                    self.buf.push_str(RESULT_RUNTIME_PY);
                    self.buf.push('\n');
                    self.result_runtime_emitted = true;
                }
                if !self.ordering_runtime_emitted && py_module_uses_ordering(items) {
                    self.buf.push_str(ORDERING_RUNTIME_PY);
                    self.buf.push('\n');
                    self.ordering_runtime_emitted = true;
                }
                if !self.concurrency_runtime_emitted && py_module_uses_concurrency(items) {
                    self.buf.push_str(CONCURRENCY_RUNTIME_PY);
                    self.buf.push('\n');
                    self.concurrency_runtime_emitted = true;
                }
                // Pre-scan impl blocks so we can attach their methods to the
                // target record/class body instead of leaving them as orphan
                // module-level functions with a `self` parameter. Both trait
                // impls (`impl Trait for T`) and bare inherent impls (`impl T`)
                // are folded; only impls whose target is a record/class
                // declared in this module are consumed (others stay as-is).
                self.impls_by_target.clear();
                let class_targets: std::collections::HashSet<String> = items
                    .iter()
                    .filter_map(|it| match &it.kind {
                        NodeKind::RecordDecl { name, .. } | NodeKind::ClassDecl { name, .. } => {
                            Some(name.name.clone())
                        }
                        _ => None,
                    })
                    .collect();
                let mut consumed_impls: std::collections::HashSet<bock_air::NodeId> =
                    std::collections::HashSet::new();
                for item in items.iter() {
                    if let NodeKind::ImplBlock { target, .. } = &item.kind {
                        if let Some(target_name) = ast_type_name(target) {
                            if class_targets.contains(&target_name) {
                                self.impls_by_target
                                    .entry(target_name)
                                    .or_default()
                                    .push(item.clone());
                                consumed_impls.insert(item.id);
                            }
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
            NodeKind::ImportDecl { .. } => {
                // Resolved by bundling — the imported module's declarations are
                // concatenated into this same file — so the import is a no-op
                // (DV13). A real `from core.compare import ...` would fail at
                // run time: that module is not on `sys.path` for a lone
                // `main.py`, which is exactly the defect bundling fixes.
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
                fields,
                generic_params,
                ..
            } => {
                // A `record R[T] { … }` needs `T` to resolve at class-eval time:
                // emit `T = TypeVar("T")` and add `Generic[T, …]` to the bases,
                // else the field annotation `value: T` raises `NameError`
                // (DV12, Python slice).
                self.emit_typevars(generic_params);
                // Pull any previously-collected `impl Trait for Name` blocks
                // so their methods become part of this class body and the
                // class inherits from every implemented trait — giving real
                // method dispatch (a bare instance has no orphan methods).
                let impls = self.impls_by_target.remove(&name.name).unwrap_or_default();
                let mut bases: Vec<String> = impls
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
                bases.extend(self.generic_base(generic_params));
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
            NodeKind::EnumDecl {
                name,
                variants,
                generic_params,
                ..
            } => {
                self.needs_dataclass_import = true;
                // A generic `enum E[T]` whose variants carry `T`-typed payloads
                // needs `T = TypeVar("T")` so those field annotations resolve.
                // (Full generic-enum codegen — `Generic[T]` variant bases — is
                // tracked separately under DV12/P1; the TypeVar declaration is
                // the minimum that keeps the module from raising `NameError`.)
                self.emit_typevars(generic_params);
                for (i, variant) in variants.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    self.emit_enum_variant(&name.name, variant)?;
                }
                // A union type alias so the enum's *name* (`Shape`) resolves as
                // a type annotation — `def area(s: Shape)` evaluates `Shape` at
                // def time, so without this alias the module raises `NameError`
                // before `main` ever runs (DV14).
                let variant_types: Vec<String> = variants
                    .iter()
                    .filter_map(|v| {
                        if let NodeKind::EnumVariant { name: vname, .. } = &v.kind {
                            Some(format!("{}_{}", name.name, vname.name))
                        } else {
                            None
                        }
                    })
                    .collect();
                if !variant_types.is_empty() {
                    self.needs_union_import = true;
                    self.writeln(&format!(
                        "{} = Union[{}]",
                        name.name,
                        variant_types.join(", ")
                    ));
                }
                Ok(())
            }
            NodeKind::ClassDecl {
                name,
                fields,
                methods,
                generic_params,
                ..
            } => {
                // A generic `class C[T]` needs `T = TypeVar("T")` + a
                // `Generic[T, …]` base so `T`-typed members resolve (DV12).
                self.emit_typevars(generic_params);
                let bases = self.generic_base(generic_params);
                let base_list = if bases.is_empty() {
                    String::new()
                } else {
                    format!("({})", bases.join(", "))
                };
                self.writeln(&format!("class {}{base_list}:", name.name));
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
                        let rest = match params.first().map(crate::generator::param_binds_self) {
                            Some(Some(_)) => &params[1..],
                            _ => &params[..],
                        };
                        let param_strs = self.collect_param_strs(rest);
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
                            let rest = match params.first().map(crate::generator::param_binds_self)
                            {
                                Some(Some(_)) => &params[1..],
                                _ => &params[..],
                            };
                            let param_strs = self.collect_param_strs(rest);
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
                let _ = write!(
                    self.buf,
                    "{ind}{}{type_hint} = ",
                    py_value_ident(&name.name)
                );
                self.emit_expr(value)?;
                self.buf.push('\n');
                Ok(())
            }
            NodeKind::ModuleHandle { effect, handler } => {
                // Emit `__<effect>: Effect = Handler()` at module scope and
                // register it as the default handler. Effectful calls later
                // in the module will pick it up via `current_handler_vars`
                // unless a local handling block overrides it.
                let effect_name = effect.segments.last().map_or("effect", |s| s.name.as_str());
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

    // ── Generic type parameters ─────────────────────────────────────────────

    /// Emit `T = TypeVar("T")` for each generic parameter not already declared,
    /// deduped across the whole bundle via [`Self::emitted_typevars`]. A param
    /// with a single bound (`T: Comparable`) becomes
    /// `T = TypeVar("T", bound=Comparable)` so static checkers see the
    /// constraint; multiple bounds collapse to the first (Python `TypeVar`
    /// takes one `bound=`). Sets [`Self::needs_typing_typevar`] when anything is
    /// emitted so the preamble imports `TypeVar`/`Generic`.
    fn emit_typevars(&mut self, generic_params: &[bock_ast::GenericParam]) {
        for gp in generic_params {
            let name = gp.name.name.clone();
            if !self.emitted_typevars.insert(name.clone()) {
                continue;
            }
            self.needs_typing_typevar.set(true);
            // A bound becomes `bound=<Name>`. Python's `TypeVar` accepts a
            // single `bound`; if Bock ever allows several, the first wins and
            // the rest are dropped (a static-checker approximation only —
            // Python erases generics at runtime regardless). A compiler-provided
            // sealed-core bound (`Equatable`/…) with no user `impl` is dropped
            // entirely: there is no such Python class, so `bound=Equatable` raises
            // `NameError` at def time (GAP-C). The `.eq`/`.compare` call is lowered
            // to a native operator by `try_emit_trait_bound_bridge`.
            let bound = gp
                .bounds
                .first()
                .and_then(|tp| tp.segments.last())
                .filter(|seg| {
                    !crate::generator::is_unimplemented_sealed_core_trait(
                        &seg.name,
                        &self.trait_decls,
                    )
                })
                .map(|seg| format!(", bound={}", self.map_type_name(&seg.name)))
                .unwrap_or_default();
            self.writeln(&format!("{name} = TypeVar(\"{name}\"{bound})"));
        }
    }

    /// Build the `Generic[T, …]` base-class fragment for a generic decl. Returns
    /// an empty `Vec` when there are no type parameters. Also sets
    /// [`Self::needs_typing_typevar`] (the typevars are emitted separately by
    /// [`Self::emit_typevars`]).
    fn generic_base(&self, generic_params: &[bock_ast::GenericParam]) -> Vec<String> {
        if generic_params.is_empty() {
            return Vec::new();
        }
        self.needs_typing_typevar.set(true);
        let names: Vec<String> = generic_params
            .iter()
            .map(|gp| gp.name.name.clone())
            .collect();
        vec![format!("Generic[{}]", names.join(", "))]
    }

    // ── Function declarations ───────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn emit_fn_decl(
        &mut self,
        _visibility: Visibility,
        is_async: bool,
        name: &str,
        generic_params: &[bock_ast::GenericParam],
        params: &[AIRNode],
        return_type: Option<&AIRNode>,
        effect_clause: &[bock_ast::TypePath],
        body: &AIRNode,
    ) -> Result<(), CodegenError> {
        if is_async {
            self.needs_asyncio_import = true;
        }
        // A generic `fn f[T](…) -> T` references `T` in its param/return
        // annotations, which Python evaluates at def time — declare
        // `T = TypeVar("T")` first so those names resolve (DV12).
        self.emit_typevars(generic_params);
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
        let fn_name = py_value_ident(name);
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
            // The AIR keeps `self` as a leading `Param`; Python methods need
            // exactly one explicit `self`. Skip the bound `self` param if
            // present so it isn't emitted twice (`def m(self, self)`).
            let rest = match params.first().map(crate::generator::param_binds_self) {
                Some(Some(_)) => &params[1..],
                _ => &params[..],
            };
            let param_strs = self.collect_param_strs(rest);
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

    /// Collect parameter strings for a `def`/method signature.
    ///
    /// Each emitted param carries its `: type` annotation and any default.
    /// Use [`Self::collect_param_strs_bare`] for `lambda` params, where Python
    /// forbids type annotations (`lambda x: int: body` is a `SyntaxError`).
    fn collect_param_strs(&self, params: &[AIRNode]) -> Vec<String> {
        self.collect_param_strs_inner(params, true)
    }

    /// Collect bare parameter names (no `: type` annotations) for a `lambda`.
    ///
    /// A Python `lambda` parameter list cannot carry annotations: emitting one
    /// produces `lambda x: int: body`, where the first `:` ends the parameter
    /// list, so the type hint becomes a second, syntactically invalid `:`.
    fn collect_param_strs_bare(&self, params: &[AIRNode]) -> Vec<String> {
        self.collect_param_strs_inner(params, false)
    }

    /// Shared implementation of [`Self::collect_param_strs`] and
    /// [`Self::collect_param_strs_bare`]. When `hints` is `false`, the `: type`
    /// annotation is omitted (required for `lambda` params).
    fn collect_param_strs_inner(&self, params: &[AIRNode], hints: bool) -> Vec<String> {
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
                    let type_hint = if hints {
                        ty.as_ref()
                            .map(|t| format!(": {}", self.type_to_py(t)))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    if let Some(def) = default {
                        let mut ctx = PyEmitCtx::new();
                        ctx.indent = self.indent;
                        ctx.enum_variants = self.enum_variants.clone();
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
                    let effect_name = h
                        .effect
                        .segments
                        .last()
                        .map_or("effect", |s| s.name.as_str());
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
                // Bock's Optional `None` constructor must not collide with
                // Python's `None` keyword: it lowers to the `_bock_none`
                // singleton of the Optional runtime (see `OPTIONAL_RUNTIME_PY`).
                // Python's own `None` (Void/Unit) is emitted as a `Literal::Unit`,
                // not an identifier, so this rewrite is unambiguous.
                if name.name == "None" {
                    self.buf.push_str("_bock_none");
                } else if let Some(variant) = crate::generator::ordering_variant(&name.name) {
                    // Prelude `Ordering` variant → the Ordering-runtime singleton
                    // (`_bock_less` / `_bock_equal` / `_bock_greater`). The
                    // `core.compare` enum decl is not bundled single-file, so the
                    // runtime stands in (mirrors `_bock_none`).
                    self.buf.push_str(ordering_singleton_py(variant));
                } else if let Some(enum_name) = self
                    .user_variant_for_name(&name.name)
                    .map(|i| i.enum_name.clone())
                {
                    // A unit-variant reference (`Empty`) → an instance of its
                    // `@dataclass(frozen=True)` class: `Shape_Empty()`.
                    let _ = write!(self.buf, "{enum_name}_{}()", name.name);
                } else {
                    self.buf.push_str(&identifier_to_py(&name.name));
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
                // A call whose callee names a registered tuple variant is a
                // construction (`Rect(3.0, 4.0)` → `Shape_Rect(3.0, 4.0)`).
                // Handled here so the callee emits the bare class name, not the
                // unit-variant `Shape_Rect()` the `Identifier` arm would.
                if let NodeKind::Identifier { name } = &callee.kind {
                    if let Some(enum_name) = self
                        .user_variant_for_name(&name.name)
                        .map(|i| i.enum_name.clone())
                    {
                        let _ = write!(self.buf, "{enum_name}_{}(", name.name);
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
                if self.try_emit_list_method(node, callee, args)? {
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
                // Desugared instance method call `Call(FieldAccess(recv, m),
                // [recv, ...rest])`: emit `recv.m(rest)` so the receiver binds
                // Python's `self` rather than being passed twice.
                if let Some((recv, method, rest)) =
                    crate::generator::desugared_self_call(callee, args)
                {
                    self.emit_expr(recv)?;
                    let _ = write!(self.buf, ".{}", to_snake_case(&method.name));
                    self.buf.push('(');
                    for (i, arg) in rest.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.emit_expr(&arg.value)?;
                    }
                    self.buf.push(')');
                    return Ok(());
                }
                // Rewrite bare effect operation calls: log(...) → handler.log(...)
                if let NodeKind::Identifier { name } = &callee.kind {
                    if let Some(effect_name) = self.effect_ops.get(&name.name).cloned() {
                        if let Some(handler_var) =
                            self.current_handler_vars.get(&effect_name).cloned()
                        {
                            let _ =
                                write!(self.buf, "{}.{}", handler_var, to_snake_case(&name.name));
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
                // Python `lambda` params take no type annotations — see
                // `collect_param_strs_bare`.
                let param_strs = self.collect_param_strs_bare(params);
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
                // A struct-variant construction (`Circle { radius: 2.0 }`) → an
                // instance of the `{enum}_{variant}` dataclass, built with
                // keyword args (`Shape_Circle(radius=2.0)`). Plain records keep
                // their dotted path name.
                let type_name = if let Some(info) = self.user_variant_for_path(path) {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    format!("{}_{variant}", info.enum_name)
                } else {
                    path.segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(".")
                };
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
                let has_newline = parts.iter().any(|p| {
                    matches!(p,
                        AirInterpolationPart::Literal(s) if s.contains('\n')
                    )
                });
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
                // Construct the Result-runtime classes (`_BockOk`/`_BockErr`) —
                // the same shape the surface `Ok(..)`/`Err(..)` construction and
                // the `case _BockOk(..)`/`_BockErr(..)` match use. The old
                // dict-with-`value`/`error`-keys shape disagreed with the match
                // (which reads the runtime classes), so reconcile on the classes.
                let cls = match variant {
                    ResultVariant::Ok => "_BockOk",
                    ResultVariant::Err => "_BockErr",
                };
                let _ = write!(self.buf, "{cls}(");
                if let Some(v) = value {
                    self.emit_expr(v)?;
                } else {
                    self.buf.push_str("None");
                }
                self.buf.push(')');
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
                self.buf.push_str(&py_value_ident(&name.name));
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
                // Optional `Some`/`None` patterns dispatch on the Optional
                // runtime classes (see `OPTIONAL_RUNTIME_PY`), not on a bare
                // `Some(...)` / `None()` class (the latter is undefined, and
                // `case None():` is a Python `SyntaxError`). `_BockSome` exposes
                // `__match_args__ = ('_0',)` so the payload binds positionally.
                let leaf = path.segments.last().map_or("", |s| s.name.as_str());
                match leaf {
                    "Some" => {
                        if let Some(f) = fields.first() {
                            // Recurse so a *nested* payload pattern (`Some(Ok(v))`)
                            // keeps its inner bindings, instead of flattening to a
                            // bare name / `_` (which dropped `v`).
                            let sub = self.pattern_to_py(f)?;
                            let _ = write!(self.buf, "_BockSome({sub})");
                        } else {
                            self.buf.push_str("_BockSome(_)");
                        }
                        return Ok(());
                    }
                    "None" => {
                        self.buf.push_str("_BockNone()");
                        return Ok(());
                    }
                    // Result `Ok`/`Err` patterns dispatch on the Result runtime
                    // classes (see `RESULT_RUNTIME_PY`), mirroring `Some`/`None`.
                    // Both carry a single payload bound positionally via
                    // `__match_args__ = ('_0',)`.
                    "Ok" | "Err" => {
                        let cls = if leaf == "Ok" { "_BockOk" } else { "_BockErr" };
                        if let Some(f) = fields.first() {
                            let sub = self.pattern_to_py(f)?;
                            let _ = write!(self.buf, "{cls}({sub})");
                        } else {
                            let _ = write!(self.buf, "{cls}(_)");
                        }
                        return Ok(());
                    }
                    _ => {}
                }
                // Prelude `Ordering` variant pattern → its Ordering-runtime class
                // (`case _BockOrderingLess():`), matching the singleton the
                // construction/bridge side produces.
                if let Some(variant) = crate::generator::ordering_variant(leaf) {
                    let _ = write!(self.buf, "{}()", ordering_class_py(variant));
                    return Ok(());
                }
                let variant_name = if let Some(info) = self.user_variant_for_path(path) {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    format!("{}_{variant}", info.enum_name)
                } else {
                    path.segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("_")
                };
                if fields.is_empty() {
                    let _ = write!(self.buf, "{variant_name}()");
                } else {
                    let mut field_pats: Vec<String> = Vec::with_capacity(fields.len());
                    for (i, f) in fields.iter().enumerate() {
                        // Recurse so a nested sub-pattern keeps its inner bindings.
                        let sub = self.pattern_to_py(f)?;
                        field_pats.push(format!("_{i}={sub}"));
                    }
                    let _ = write!(self.buf, "{variant_name}({})", field_pats.join(", "));
                }
            }
            NodeKind::RecordPat { path, fields, .. } => {
                let type_name = if let Some(info) = self.user_variant_for_path(path) {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    format!("{}_{variant}", info.enum_name)
                } else {
                    path.segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("_")
                };
                let mut field_pats: Vec<String> = Vec::with_capacity(fields.len());
                for f in fields {
                    let field_name = to_snake_case(&f.name.name);
                    if let Some(pat) = &f.pattern {
                        // Recurse so a nested record/constructor/tuple sub-pattern
                        // keeps its inner bindings.
                        let sub = self.pattern_to_py(pat)?;
                        field_pats.push(format!("{field_name}={sub}"));
                    } else {
                        // Shorthand `{ radius }` ≡ `{ radius: radius }`. Emit the
                        // keyword form `radius=radius` so the bind is by field
                        // name, not by `__match_args__` position (a dataclass's
                        // positional order is field-decl order *plus* the trailing
                        // `_tag`, so a bare positional sub-pattern would mis-bind
                        // multi-field variants).
                        field_pats.push(format!("{field_name}={field_name}"));
                    }
                }
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
            // Python `match`/`case` supports native or-patterns: `case 1 | 2 | 3:`.
            // Without this, an `OrPat` fell through to the `_` catch-all, so
            // `1 | 2 | 3 => …` collapsed to `case _:` — shadowing every later arm
            // ("wildcard makes remaining patterns unreachable").
            NodeKind::OrPat { alternatives } => {
                for (i, alt) in alternatives.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(" | ");
                    }
                    self.emit_pattern(alt)?;
                }
            }
            _ => {
                self.buf.push('_');
            }
        }
        Ok(())
    }

    /// Emit a `match` *expression* (each arm yields a value, no statement arm)
    /// as a nested Python conditional over the scrutinee, which the caller has
    /// bound to `__v` via the enclosing `(lambda __v: …)(<scrutinee>)`.
    ///
    /// Each non-final arm becomes `<body> if <test on __v> else (<rest>)`. The
    /// arm body may reference a pattern binding (the `x` in `Some(x)`, or a
    /// whole-scrutinee bind pattern); since a Python conditional can't introduce
    /// a binding, the body is wrapped in an immediately-applied
    /// `(lambda <bind>: <body>)(<value>)` so the name resolves. Patterns:
    ///
    /// - `Some(x)` → test `isinstance(__v, _BockSome)`, bind `x = __v._0`
    /// - `None`    → test `isinstance(__v, _BockNone)`
    /// - literal   → test `__v == <lit>`
    /// - wildcard / bind / final arm → the `else` (catch-all); a bind pattern
    ///   binds the whole scrutinee
    ///
    /// This replaces an earlier stub that emitted a hardcoded `… if False else …`
    /// chain (which always selected the *last* arm and never bound the payload),
    /// mis-compiling every expression-position `match` whose arms were not all
    /// `return`s — e.g. `let r = match o { Some(x) => x + 1; None => 0 }`.
    fn emit_match_expr(
        &mut self,
        _scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        self.emit_match_expr_from(arms, 0)
    }

    /// Tail of [`Self::emit_match_expr`]: emit the conditional for `arms[idx..]`.
    fn emit_match_expr_from(&mut self, arms: &[AIRNode], idx: usize) -> Result<(), CodegenError> {
        let Some(NodeKind::MatchArm { pattern, body, .. }) = arms.get(idx).map(|a| &a.kind) else {
            // No arm at this index: Bock matches are exhaustive, so this is
            // unreachable, but emit a defined value to keep the expression valid.
            self.buf.push_str("None");
            return Ok(());
        };
        let is_last = idx + 1 >= arms.len();
        let is_catch_all = matches!(
            pattern.kind,
            NodeKind::WildcardPat | NodeKind::BindPat { .. }
        );
        // The final arm, or any catch-all, is the unconditional `else` value.
        if is_last || is_catch_all {
            return self.emit_arm_value(pattern, body, /*whole_scrutinee_bind=*/ true);
        }
        // Otherwise: `<value> if <test> else (<rest>)`.
        self.emit_arm_value(pattern, body, /*whole_scrutinee_bind=*/ false)?;
        self.buf.push_str(" if ");
        self.emit_match_expr_test(pattern)?;
        self.buf.push_str(" else (");
        self.emit_match_expr_from(arms, idx + 1)?;
        self.buf.push(')');
        Ok(())
    }

    /// Emit the boolean test (over the bound `__v`) that selects `pattern`.
    fn emit_match_expr_test(&mut self, pattern: &AIRNode) -> Result<(), CodegenError> {
        match &pattern.kind {
            NodeKind::ConstructorPat { path, .. } => {
                let leaf = path.segments.last().map_or("", |s| s.name.as_str());
                let cls: String = match leaf {
                    "Some" => "_BockSome".to_string(),
                    "None" => "_BockNone".to_string(),
                    // Result `Ok`/`Err` test against the Result-runtime classes,
                    // mirroring `Some`/`None`.
                    "Ok" => "_BockOk".to_string(),
                    "Err" => "_BockErr".to_string(),
                    other => {
                        // Prelude `Ordering` variants test against the
                        // Ordering-runtime class so an expression-position match
                        // (`lambda __v: isinstance(__v, _BockOrderingLess) …`)
                        // recognises the singleton the bridge/construction
                        // produces.
                        if let Some(v) = crate::generator::ordering_variant(other) {
                            ordering_class_py(v).to_string()
                        } else if let Some(info) = self.user_variant_for_path(path) {
                            // A user-enum variant tests against its dataclass
                            // `{enum}_{variant}` — the same class the statement
                            // `emit_pattern` and the construction side produce.
                            // Without this the test used the bare variant leaf
                            // (`isinstance(__v, Red)`), an undefined name.
                            format!("{}_{other}", info.enum_name)
                        } else {
                            other.to_string()
                        }
                    }
                };
                let _ = write!(self.buf, "isinstance(__v, {cls})");
            }
            NodeKind::LiteralPat { .. } => {
                self.buf.push_str("__v == ");
                self.emit_pattern(pattern)?;
            }
            // Catch-alls never produce a test (handled as the `else`).
            _ => self.buf.push_str("True"),
        }
        Ok(())
    }

    /// Emit one arm's value, binding any pattern variable via an applied lambda
    /// so it resolves inside the conditional. `whole_scrutinee_bind` allows a
    /// bind pattern in `else` position to capture the whole scrutinee (`__v`).
    fn emit_arm_value(
        &mut self,
        pattern: &AIRNode,
        body: &AIRNode,
        whole_scrutinee_bind: bool,
    ) -> Result<(), CodegenError> {
        match &pattern.kind {
            // `Some(x)` / `Ok(x)` / `Err(e)` bind the payload `__v._0`.
            NodeKind::ConstructorPat { path, fields }
                if path
                    .segments
                    .last()
                    .is_some_and(|s| matches!(s.name.as_str(), "Some" | "Ok" | "Err")) =>
            {
                if let Some(f) = fields.first() {
                    let name = self.pattern_to_binding_name(f);
                    if name != "_" {
                        let _ = write!(self.buf, "(lambda {name}: ");
                        self.emit_block_as_expr(body)?;
                        self.buf.push_str(")(__v._0)");
                        return Ok(());
                    }
                }
                self.emit_block_as_expr(body)
            }
            // A bind pattern (`x => …`) captures the whole scrutinee.
            NodeKind::BindPat { name, .. } if whole_scrutinee_bind => {
                let bind = py_value_ident(&name.name);
                let _ = write!(self.buf, "(lambda {bind}: ");
                self.emit_block_as_expr(body)?;
                self.buf.push_str(")(__v)");
                Ok(())
            }
            _ => self.emit_block_as_expr(body),
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
                // `Result[T, E]` lowers to the tagged Result-runtime classes, not
                // a subscripted generic — the value is `_BockOk(...)` /
                // `_BockErr(...)`, so the annotation is the union `_BockOk |
                // _BockErr` with no `[T, E]` (which would be a Python error on a
                // union). Mirrors the `TypeOptional` arm below.
                if name == "Result" {
                    return "_BockOk | _BockErr".to_string();
                }
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
                self.needs_typing_callable.set(true);
                let param_strs: Vec<String> = params.iter().map(|p| self.type_to_py(p)).collect();
                format!(
                    "Callable[[{}], {}]",
                    param_strs.join(", "),
                    self.type_to_py(ret)
                )
            }
            NodeKind::TypeOptional { inner } => {
                // `T?` lowers to the tagged Optional runtime, not `T | None`:
                // the value is `_BockSome(...)` / `_bock_none`, so the annotation
                // must describe those classes for type and value to agree (mirrors
                // Go's `__bockOption` and TS's `BockOption<T>`). The element type
                // `T` is preserved as a comment for readability; Python does not
                // enforce annotations at runtime.
                let _ = inner;
                "_BockSome | _BockNone".to_string()
            }
            NodeKind::TypeSelf => {
                self.needs_typing_self.set(true);
                "Self".into()
            }
            _ => {
                self.needs_typing_any.set(true);
                "Any".into()
            }
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
            "Any" => {
                self.needs_typing_any.set(true);
                "Any".into()
            }
            "Never" => {
                self.needs_typing_never.set(true);
                "Never".into()
            }
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
                // See the `Result` case in `type_to_py`: lowers to the tagged
                // Result-runtime union, no subscript.
                if name == "Result" {
                    return "_BockOk | _BockErr".to_string();
                }
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
                self.needs_typing_callable.set(true);
                let param_strs: Vec<String> =
                    params.iter().map(|p| self.ast_type_to_py(p)).collect();
                format!(
                    "Callable[[{}], {}]",
                    param_strs.join(", "),
                    self.ast_type_to_py(ret)
                )
            }
            TypeExpr::Optional { inner, .. } => {
                // See the `TypeOptional` arm of `type_to_py`: the tagged Optional
                // runtime classes must match the emitted tagged value.
                let _ = inner;
                "_BockSome | _BockNone".to_string()
            }
            TypeExpr::SelfType { .. } => {
                self.needs_typing_self.set(true);
                "Self".into()
            }
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
    fn collect_task_bindings(stmts: &[AIRNode]) -> std::collections::HashSet<String> {
        let mut awaited: std::collections::HashSet<String> = std::collections::HashSet::new();
        for s in stmts {
            Self::collect_awaited_identifiers(s, &mut awaited);
        }
        let mut out = std::collections::HashSet::new();
        for s in stmts {
            if let NodeKind::LetBinding { pattern, value, .. } = &s.kind {
                if let NodeKind::BindPat { name, .. } = &pattern.kind {
                    let py_name = py_value_ident(&name.name);
                    if matches!(&value.kind, NodeKind::Call { .. }) && awaited.contains(&py_name) {
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
    fn collect_awaited_identifiers(node: &AIRNode, out: &mut std::collections::HashSet<String>) {
        match &node.kind {
            NodeKind::Await { expr } => {
                if let NodeKind::Identifier { name } = &expr.kind {
                    out.insert(py_value_ident(&name.name));
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
            NodeKind::TupleLiteral { elems } | NodeKind::ListLiteral { elems } => {
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
                // A statement tail (`break`/`continue`/`return`/assignment) is
                // emitted as a statement, never wrapped in `return`.
                if crate::generator::node_is_statement(t) {
                    self.emit_node(t)?;
                    return Ok(());
                }
                // A `match` with statement arms yields no value: emit a Python
                // `match`/`case` statement, not a `return (lambda ...)`.
                if let NodeKind::Match { scrutinee, arms } = &t.kind {
                    if crate::generator::match_has_statement_arm(arms) {
                        self.emit_match(scrutinee, arms)?;
                        return Ok(());
                    }
                }
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}return ");
                self.emit_expr(t)?;
                self.buf.push('\n');
            }
        } else if crate::generator::node_is_statement(node) {
            // A bare statement body (`break`/`continue`/`return`/assignment).
            self.emit_node(node)?;
        } else if let NodeKind::Match { scrutinee, arms } = &node.kind {
            if crate::generator::match_has_statement_arm(arms) {
                self.emit_match(scrutinee, arms)?;
            } else {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}return ");
                self.emit_expr(node)?;
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
            NodeKind::BindPat { name, .. } => py_value_ident(&name.name),
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
        py_value_ident(s)
    }
}

/// Convert a Bock *value* identifier (a param, local binding, or free-function
/// name) to its Python form: `snake_case`, then escaped against the Python
/// keyword set so a binding named e.g. `def` emits `def_` rather than the
/// illegal bare keyword. Apply at every value declaration and reference site so
/// the escaped name is used uniformly; member/method names use bare
/// [`to_snake_case`]. See [`crate::generator::escape_target_keyword`].
fn py_value_ident(name: &str) -> String {
    crate::generator::escape_target_keyword(
        &to_snake_case(name),
        crate::generator::KeywordTarget::Python,
    )
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

    /// An EXPRESSION-position user-enum `match` (a `match` consumed as a value,
    /// here bound into a `let`) lowers to a `(lambda __v: …)(scrutinee)` whose
    /// per-arm test is `isinstance(__v, <cls>)`. The variant class must be
    /// resolved through the registry to its `{enum}_{variant}` dataclass
    /// (`isinstance(__v, Light_Red)`), NOT the bare variant leaf name
    /// (`isinstance(__v, Red)`, an undefined name → `NameError`). Mirrors the
    /// statement-position `emit_pattern` resolution (Q-match-exprpos P4).
    #[test]
    fn expr_position_user_enum_match_test_resolves_variant_class() {
        let enum_decl = node(
            1,
            NodeKind::EnumDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Light"),
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
                ],
            },
        );
        // let n: Int = match l { Red => 1; _ => 0 }   (a value-position match)
        let red_arm = node(
            20,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    21,
                    NodeKind::ConstructorPat {
                        path: type_path(&["Red"]),
                        fields: vec![],
                    },
                )),
                guard: None,
                body: Box::new(block(22, vec![], Some(int_lit(23, "1")))),
            },
        );
        let default_arm = node(
            30,
            NodeKind::MatchArm {
                pattern: Box::new(node(31, NodeKind::WildcardPat)),
                guard: None,
                body: Box::new(block(32, vec![], Some(int_lit(33, "0")))),
            },
        );
        let m = node(
            40,
            NodeKind::Match {
                scrutinee: Box::new(id_node(41, "l")),
                arms: vec![red_arm, default_arm],
            },
        );
        let let_n = node(
            50,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(51, "n")),
                ty: Some(Box::new(node(
                    52,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                value: Box::new(m),
            },
        );
        let f = node(
            5,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("rank"),
                generic_params: vec![],
                params: vec![param_node(6, "l")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(7, vec![let_n], None)),
            },
        );
        let out = gen(&module(vec![], vec![enum_decl, f]));
        assert!(
            out.contains("isinstance(__v, Light_Red)"),
            "expr-position variant test must resolve to the dataclass Light_Red, got: {out}"
        );
        assert!(
            !out.contains("isinstance(__v, Red)"),
            "must not test against the bare variant leaf name (undefined), got: {out}"
        );
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
        assert!(
            !out.contains("f\"Hello"),
            "single-line should not appear: {out}"
        );
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
        assert!(
            !out.contains("f\"\"\""),
            "should not use triple quotes: {out}"
        );
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
        // Reconciled on the `_BockOk`/`_BockErr` runtime classes the `Result`
        // match reads (the old dict-with-`value`/`error`-keys shape disagreed).
        assert!(out.contains("_BockOk(42)"), "got: {out}");
        assert!(out.contains("_BockErr(\"failed\")"), "got: {out}");
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
        let src_path = std::path::Path::new("src/main.bock");
        let out = gen.generate_project(&[(&m, src_path)]).unwrap();
        let src = &out.files[0].content;
        assert_eq!(out.files[0].path, std::path::PathBuf::from("main.py"));
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

    /// Q-py-optional: the Python Optional runtime is emitted when a module uses
    /// Optional/`Some`/`None`; `Some(x)` and `None` lower to the tagged runtime
    /// values (`_BockSome(...)` / `_bock_none`), and an Optional `match` lowers
    /// to structural arms (`case _BockSome(x):` / `case _BockNone():`) — not the
    /// old bare `Some`/`None` (undefined) and `case None():` (a SyntaxError).
    #[test]
    fn optional_runtime_construct_and_match() {
        // fn describe(o: Int?) -> Int {
        //   match o { Some(x) => return x; None => return Some(0); }  (Some forces construction)
        // }
        let opt_int_ty = node(
            200,
            NodeKind::TypeOptional {
                inner: Box::new(node(
                    201,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                )),
            },
        );
        let o_param = node(
            30,
            NodeKind::Param {
                pattern: Box::new(bind_pat(31, "o")),
                ty: Some(Box::new(opt_int_ty)),
                default: None,
            },
        );
        // Construct Some(1) and None in the body so the prelude + constructors
        // are exercised.
        let some_call = node(
            70,
            NodeKind::Call {
                callee: Box::new(id_node(71, "Some")),
                args: vec![AirArg {
                    label: None,
                    value: int_lit(72, "1"),
                }],
                type_args: vec![],
            },
        );
        let none_ref = id_node(73, "None");
        let some_arm = node(
            40,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    41,
                    NodeKind::ConstructorPat {
                        path: type_path(&["Some"]),
                        fields: vec![bind_pat(42, "x")],
                    },
                )),
                guard: None,
                body: Box::new(block(
                    43,
                    vec![node(
                        44,
                        NodeKind::Return {
                            value: Some(Box::new(id_node(45, "x"))),
                        },
                    )],
                    None,
                )),
            },
        );
        let none_arm = node(
            50,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    51,
                    NodeKind::ConstructorPat {
                        path: type_path(&["None"]),
                        fields: vec![],
                    },
                )),
                guard: None,
                body: Box::new(block(
                    52,
                    vec![node(
                        53,
                        NodeKind::Return {
                            value: Some(Box::new(int_lit(54, "0"))),
                        },
                    )],
                    None,
                )),
            },
        );
        let match_stmt = node(
            60,
            NodeKind::Match {
                scrutinee: Box::new(id_node(61, "o")),
                arms: vec![some_arm, none_arm],
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
                params: vec![o_param],
                return_type: Some(Box::new(node(
                    2,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    3,
                    vec![
                        node(
                            80,
                            NodeKind::LetBinding {
                                is_mut: false,
                                pattern: Box::new(bind_pat(81, "a")),
                                ty: None,
                                value: Box::new(some_call),
                            },
                        ),
                        node(
                            82,
                            NodeKind::LetBinding {
                                is_mut: false,
                                pattern: Box::new(bind_pat(83, "b")),
                                ty: None,
                                value: Box::new(none_ref),
                            },
                        ),
                        match_stmt,
                    ],
                    None,
                )),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        // Runtime prelude is present.
        assert!(out.contains("class _BockSome:"), "got: {out}");
        assert!(out.contains("class _BockNone:"), "got: {out}");
        assert!(out.contains("_bock_none = _BockNone()"), "got: {out}");
        // Constructors lower to the runtime values, not bare `Some`/`None`.
        assert!(out.contains("_BockSome(1)"), "got: {out}");
        assert!(out.contains("b = _bock_none"), "got: {out}");
        // Match arms are structural, not `Some(...)` / `case None():`.
        assert!(out.contains("case _BockSome(x):"), "got: {out}");
        assert!(out.contains("case _BockNone():"), "got: {out}");
        assert!(!out.contains("case None()"), "got: {out}");
    }

    /// An Optional `match` in *expression* position (value of a `let`) with
    /// *non-`return`* arms must lower to a real conditional over the bound
    /// scrutinee that tests the tag and binds the payload — NOT the old stub
    /// `(lambda __v: <some> if False else <none>)` (which always selected the
    /// last arm and never bound the payload). Regression-locking the Python
    /// expression-position Optional-match defect.
    #[test]
    fn optional_match_in_expression_position_binds_payload() {
        // fn pick(o: Int?) -> Int { let r = match o { Some(x) => x + 1; None => 0 }; return r }
        let opt_int_ty = node(
            200,
            NodeKind::TypeOptional {
                inner: Box::new(node(
                    201,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                )),
            },
        );
        let o_param = node(
            30,
            NodeKind::Param {
                pattern: Box::new(bind_pat(31, "o")),
                ty: Some(Box::new(opt_int_ty)),
                default: None,
            },
        );
        // Some(x) => x + 1  (a value, not a `return`).
        let some_arm = node(
            40,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    41,
                    NodeKind::ConstructorPat {
                        path: type_path(&["Some"]),
                        fields: vec![bind_pat(42, "x")],
                    },
                )),
                guard: None,
                body: Box::new(block(
                    43,
                    vec![],
                    Some(node(
                        44,
                        NodeKind::BinaryOp {
                            op: BinOp::Add,
                            left: Box::new(id_node(45, "x")),
                            right: Box::new(int_lit(46, "1")),
                        },
                    )),
                )),
            },
        );
        // None => 0
        let none_arm = node(
            50,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    51,
                    NodeKind::ConstructorPat {
                        path: type_path(&["None"]),
                        fields: vec![],
                    },
                )),
                guard: None,
                body: Box::new(block(52, vec![], Some(int_lit(53, "0")))),
            },
        );
        let match_expr = node(
            60,
            NodeKind::Match {
                scrutinee: Box::new(id_node(61, "o")),
                arms: vec![some_arm, none_arm],
            },
        );
        // let r = <match_expr>  (match appears in expression position).
        let let_r = node(
            70,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(71, "r")),
                ty: None,
                value: Box::new(match_expr),
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("pick"),
                generic_params: vec![],
                params: vec![o_param],
                return_type: Some(Box::new(node(
                    2,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    3,
                    vec![
                        let_r,
                        node(
                            80,
                            NodeKind::Return {
                                value: Some(Box::new(id_node(81, "r"))),
                            },
                        ),
                    ],
                    None,
                )),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        // No hardcoded `if False` stub.
        assert!(
            !out.contains("if False"),
            "expression-position match must not emit the `if False` stub, got: {out}"
        );
        // Tests the tag and binds the payload via an applied lambda.
        assert!(
            out.contains("isinstance(__v, _BockSome)"),
            "expected a tag test, got: {out}"
        );
        assert!(
            out.contains("(lambda x:") && out.contains(")(__v._0)"),
            "expected the Some payload bound from __v._0, got: {out}"
        );
    }

    // ── Lambdas, typing imports, and generics (DV12 + lambda fix) ─────────────

    /// A `GenericParam` named `name` with optional single trait `bound`.
    fn generic_param(name: &str, bound: Option<&str>) -> bock_ast::GenericParam {
        bock_ast::GenericParam {
            id: 0,
            span: span(),
            name: ident(name),
            bounds: bound.map(|b| vec![type_path(&[b])]).unwrap_or_default(),
        }
    }

    /// A record field whose declared type is the bare named type `ty_name`
    /// (e.g. a type parameter `T`).
    fn named_field(field: &str, ty_name: &str) -> bock_ast::RecordDeclField {
        bock_ast::RecordDeclField {
            id: 0,
            span: span(),
            name: ident(field),
            ty: bock_ast::TypeExpr::Named {
                id: 0,
                span: span(),
                path: type_path(&[ty_name]),
                args: vec![],
            },
            default: None,
        }
    }

    #[test]
    fn lambda_params_have_no_type_hints() {
        // `(x: Int) => x + 1` must emit `lambda x: …`, never `lambda x: int: …`
        // (the latter is a Python `SyntaxError` — the bug this fix closes).
        let lambda = node(
            1,
            NodeKind::Lambda {
                params: vec![typed_param_node(2, "x", "Int")],
                body: Box::new(node(
                    3,
                    NodeKind::BinaryOp {
                        op: BinOp::Add,
                        left: Box::new(id_node(4, "x")),
                        right: Box::new(int_lit(5, "1")),
                    },
                )),
            },
        );
        let body = block(
            6,
            vec![node(
                7,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(8, "inc")),
                    ty: None,
                    value: Box::new(lambda),
                },
            )],
            None,
        );
        let f = node(
            9,
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
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("lambda x: "),
            "lambda must emit a bare param list, got: {out}"
        );
        assert!(
            !out.contains("lambda x: int"),
            "lambda param must NOT carry a type hint (SyntaxError), got: {out}"
        );
    }

    #[test]
    fn fn_type_param_emits_callable_import() {
        // A parameter of function type lowers to `Callable[[int], int]`, which
        // must be imported from `typing` or it raises `NameError`.
        let f_param = node(
            2,
            NodeKind::Param {
                pattern: Box::new(bind_pat(3, "f")),
                ty: Some(Box::new(node(
                    4,
                    NodeKind::TypeFunction {
                        params: vec![node(
                            5,
                            NodeKind::TypeNamed {
                                path: type_path(&["Int"]),
                                args: vec![],
                            },
                        )],
                        ret: Box::new(node(
                            6,
                            NodeKind::TypeNamed {
                                path: type_path(&["Int"]),
                                args: vec![],
                            },
                        )),
                        effects: vec![],
                    },
                ))),
                default: None,
            },
        );
        let body = block(7, vec![], Some(int_lit(8, "0")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("apply"),
                generic_params: vec![],
                params: vec![f_param],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("from typing import Callable"),
            "Callable annotation needs its typing import, got: {out}"
        );
        assert!(
            out.contains("f: Callable[[int], int]"),
            "expected the Callable annotation, got: {out}"
        );
    }

    #[test]
    fn generic_record_emits_typevar_and_generic() {
        // `record Box[T] { value: T }` must declare `T = TypeVar("T")`, list
        // `Generic[T]` in the class bases, and import both from `typing`, or
        // the field annotation `value: T` raises `NameError` at class-eval time.
        let rec = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Box"),
                generic_params: vec![generic_param("T", None)],
                fields: vec![named_field("value", "T")],
            },
        );
        let out = gen(&module(vec![], vec![rec]));
        assert!(
            out.contains("from typing import Generic, TypeVar"),
            "expected merged typing import, got: {out}"
        );
        assert!(
            out.contains("T = TypeVar(\"T\")"),
            "expected a TypeVar declaration, got: {out}"
        );
        assert!(
            out.contains("class Box(Generic[T]):"),
            "expected Generic[T] in the class bases, got: {out}"
        );
        assert!(out.contains("value: T"), "got: {out}");
    }

    #[test]
    fn bounded_type_param_emits_typevar_bound() {
        // `fn describe[T: Named](x: T) -> ...` → `T = TypeVar("T", bound=Named)`.
        let body = block(3, vec![], Some(int_lit(4, "0")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("describe"),
                generic_params: vec![generic_param("T", Some("Named"))],
                params: vec![typed_param_node(2, "x", "T")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("T = TypeVar(\"T\", bound=Named)"),
            "expected a bounded TypeVar, got: {out}"
        );
    }

    #[test]
    fn shared_type_param_typevar_is_deduped() {
        // Two generic records sharing the parameter name `T` must declare
        // `T = TypeVar("T")` exactly once across the bundle.
        let box_a = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Boxed"),
                generic_params: vec![generic_param("T", None)],
                fields: vec![named_field("value", "T")],
            },
        );
        let box_b = node(
            2,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Wrapped"),
                generic_params: vec![generic_param("T", None)],
                fields: vec![named_field("inner", "T")],
            },
        );
        let out = gen(&module(vec![], vec![box_a, box_b]));
        let typevar_count = out.matches("T = TypeVar(\"T\")").count();
        assert_eq!(
            typevar_count, 1,
            "shared type param T must be declared exactly once, got {typevar_count} in: {out}"
        );
    }
}
