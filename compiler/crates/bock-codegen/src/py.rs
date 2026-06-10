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

/// True if the module uses the `?` propagate operator anywhere, so the
/// [`PROPAGATE_RUNTIME_PY`] helper (`_bock_try` / `_BockPropagate`) must be
/// emitted. Mirrors [`py_module_uses_optional`]: a structural scan over the debug
/// rendering for the `Propagate` AIR node.
fn py_module_uses_propagate(items: &[AIRNode]) -> bool {
    items.iter().any(|n| format!("{n:?}").contains("Propagate"))
}

/// True if the module uses a `List` functional combinator that lowers to one of
/// the [`LIST_FUNCTIONAL_RUNTIME_PY`] helpers (`reduce`/`fold`/`find`/`for_each`).
/// Gates emission of that prelude, mirroring [`py_module_uses_optional`]. `map`/
/// `filter`/`any`/`all`/`flat_map` lower to Python builtins (`list(map(..))` /
/// `any(..)` / a comprehension) and need no helper, so they don't gate here.
fn py_module_uses_list_functional(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"reduce\"")
            || s.contains("\"fold\"")
            || s.contains("\"find\"")
            || s.contains("\"for_each\"")
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

def _bock_parse_int(s, mk_err):
    try:
        return _BockOk(int(s.strip()))
    except (ValueError, TypeError):
        return _BockErr(mk_err(f\"cannot parse {s!r} as Int\"))

def _bock_parse_float(s, mk_err):
    try:
        t = s.strip()
        if t == '':
            raise ValueError()
        return _BockOk(float(t))
    except (ValueError, TypeError):
        return _BockErr(mk_err(f\"cannot parse {s!r} as Float\"))
";

/// Runtime for the `?` propagate operator in Python. `expr?` lowers to
/// `_bock_try(expr)`: an `Ok`/`Some` value yields its payload; an `Err`/`None`
/// value raises the `_BockPropagate` sentinel carrying the original tagged value.
/// The enclosing function (the one containing the `?`) has its body wrapped in
/// `try: … except _BockPropagate as __p: return __p.value`, so the `Err`/`None`
/// is re-returned unchanged — Rust-`?` semantics.
///
/// The unwrap test is by **class name** (`type(v).__name__`) rather than
/// `isinstance`, so the helper is self-contained: it does not hard-reference
/// `_BockOk`/`_BockSome`, which lets it live in `_bock_runtime` (or be inlined)
/// even when only one of the Optional/Result preludes is present. Anything that
/// is not a recognised success tag (including the `_BockNone` singleton) is
/// treated as the failing case and propagated.
const PROPAGATE_RUNTIME_PY: &str = "\
# ── Bock `?` propagate runtime ──
class _BockPropagate(Exception):
    __slots__ = ('value',)
    def __init__(self, value):
        super().__init__()
        self.value = value

def _bock_try(v):
    if type(v).__name__ in ('_BockOk', '_BockSome'):
        return v._0
    raise _BockPropagate(v)
";

/// The prelude `Ordering` runtime: the three variants of `core.compare.Ordering`
/// as singleton instances of distinct classes, matchable by `case` and emitted
/// for construction. Mirrors `OPTIONAL_RUNTIME_PY` — when the `core.compare`
/// enum declaration is not among the reached modules, the primitive bridge
/// (`(x).compare(y)`) and any bare `Less`/`Equal`/`Greater` need this
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

/// Runtime helpers for the closure-taking `List` combinators whose Python form
/// is not a single builtin expression: `reduce`/`fold` (left folds, no
/// statement-level loop is expressible in a lambda), `find` (returns the tagged
/// `Optional`), and `for_each` (a side-effecting drive returning `None`). `map`/
/// `filter`/`any`/`all`/`flat_map` lower inline to Python builtins and need no
/// helper. `_bock_find` builds the same tagged `Optional` runtime values
/// (`_BockSome`/`_bock_none`) that `OPTIONAL_RUNTIME_PY` defines, so that prelude
/// is co-emitted whenever this one is. Gated by [`py_module_uses_list_functional`].
const LIST_FUNCTIONAL_RUNTIME_PY: &str = "\
# ── Bock List functional-combinator runtime ──
def _bock_reduce(xs, f):
    it = iter(xs)
    acc = next(it)
    for x in it:
        acc = f(acc, x)
    return acc

def _bock_fold(xs, init, f):
    acc = init
    for x in xs:
        acc = f(acc, x)
    return acc

def _bock_find(xs, pred):
    for x in xs:
        if pred(x):
            return _BockSome(x)
    return _bock_none

def _bock_for_each(xs, f):
    for x in xs:
        f(x)
    return None
";

/// Runtime-prelude names that resolve through the shared `_bock_runtime`
/// module (or built-in lowering), NOT through a cross-module import — so the
/// implicit-import pass must never try to import them from a declaring module.
/// These are the §18.2-prelude container/ordering symbols whose Python form is
/// the bespoke tagged runtime (`_BockSome`, …), not the `core.*` declaration.
const RUNTIME_PRELUDE_NAMES: &[&str] = &[
    "Optional", "Some", "None", "Result", "Ok", "Err", "Ordering", "Less", "Equal", "Greater",
];

/// Build a map from every **public top-level symbol name** declared across
/// `modules` to the dotted module-path that declares it (e.g. `Iterable` →
/// `core.iter`). Covers functions, records, enums (and each variant's emitted
/// `Enum_Variant` dataclass name), traits, classes, effects, type aliases, and
/// consts.
///
/// The per-module emission path needs this for **implicit imports**: a prelude
/// trait used as a base class (`impl Iterable for Bag`, with `Iterable`
/// auto-imported per §18.2) is referenced without an explicit `use`. Emitting
/// one file per module means `main.py` must `import` `Iterable` from `core.iter`
/// even though it never appears in an explicit `use`. This map lets
/// `generate_project` add exactly those imports for names a module references
/// but neither declares locally nor imports explicitly.
///
/// Runtime-prelude names (`RUNTIME_PRELUDE_NAMES`) are excluded — they resolve
/// through `_bock_runtime`, not a `core.*` import. The first declarer wins for a
/// name declared in several modules (deterministic via the dependency order
/// `modules` arrives in).
fn collect_public_symbol_modules(
    modules: &[(&AIRModule, &std::path::Path)],
) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    for (module, _) in modules {
        let Some(module_path) = crate::generator::module_path_string(module) else {
            continue;
        };
        let NodeKind::Module { items, .. } = &module.kind else {
            continue;
        };
        for item in items {
            let mut record = |name: &str| {
                if !RUNTIME_PRELUDE_NAMES.contains(&name) {
                    map.entry(name.to_string())
                        .or_insert_with(|| module_path.clone());
                }
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
                } => {
                    if matches!(visibility, Visibility::Public) {
                        record(&name.name);
                    }
                }
                NodeKind::EnumDecl {
                    visibility,
                    name,
                    variants,
                    ..
                } => {
                    if matches!(visibility, Visibility::Public) {
                        record(&name.name);
                        for v in variants {
                            if let NodeKind::EnumVariant { name: vname, .. } = &v.kind {
                                record(&format!("{}_{}", name.name, vname.name));
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

/// Declared module-path of `module`, or empty if it declares none.
fn module_path_string_of(module: &AIRModule) -> String {
    crate::generator::module_path_string(module).unwrap_or_default()
}

/// Top-level symbol names declared **locally** in `module` (item names plus
/// each enum variant's emitted `Enum_Variant` name) — the names a per-module
/// implicit import must never shadow with a cross-module import.
fn locally_declared_names(module: &AIRModule) -> std::collections::HashSet<String> {
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
/// imported leaf names and their aliases) — already emitted as real imports,
/// so the implicit-import pass must skip them.
fn explicitly_imported_names(module: &AIRModule) -> std::collections::HashSet<String> {
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

/// Tally, per identifier name, how many times that name appears purely as a
/// **record/enum/class field label** anywhere in `module` — i.e. in a position
/// that names a *field*, never a cross-module symbol. Covers the four label
/// positions:
///
/// - record / class / enum-struct-variant field **declarations**
///   (`record R { total_value: Float }`),
/// - record-construction labels (`R { total_value: v }`),
/// - record-pattern field labels (`R { total_value }`),
/// - field **access** (`r.total_value`).
///
/// The implicit-import scan ([`implicit_imports_for`]) matches a public symbol
/// name against the module's debug rendering. A field label produces the same
/// quoted-identifier token as a genuine reference, so a record field whose name
/// collides with a sibling module's public function (e.g. `InventorySummary`'s
/// `total_value` field vs. `service.total_value`) was spuriously "referenced",
/// pulling in `from service import total_value`. Because `service` already
/// imports `models`, that creates a Python import **cycle**
/// (`ImportError: cannot import name … (circular import)`).
///
/// Subtracting these label occurrences from the total lets the scan keep its
/// "over-import is harmless" property for true references while never importing
/// a name that appears *only* as a field label.
fn field_label_occurrences(module: &AIRModule) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    fn bump(counts: &mut HashMap<String, usize>, name: &str) {
        *counts.entry(name.to_string()).or_insert(0) += 1;
    }
    fn walk_decl_fields(counts: &mut HashMap<String, usize>, fields: &[bock_ast::RecordDeclField]) {
        for f in fields {
            bump(counts, &f.name.name);
        }
    }
    fn walk(counts: &mut HashMap<String, usize>, node: &AIRNode) {
        match &node.kind {
            NodeKind::RecordDecl { fields, .. } | NodeKind::ClassDecl { fields, .. } => {
                walk_decl_fields(counts, fields);
            }
            NodeKind::EnumVariant {
                payload: bock_air::EnumVariantPayload::Struct(fields),
                ..
            } => {
                walk_decl_fields(counts, fields);
            }
            NodeKind::FieldAccess { field, object } => {
                bump(counts, &field.name);
                walk(counts, object);
            }
            NodeKind::RecordConstruct { fields, spread, .. } => {
                for f in fields {
                    bump(counts, &f.name.name);
                    if let Some(v) = &f.value {
                        walk(counts, v);
                    }
                }
                if let Some(s) = spread {
                    walk(counts, s);
                }
            }
            NodeKind::RecordPat { fields, .. } => {
                for f in fields {
                    bump(counts, &f.name.name);
                    if let Some(p) = &f.pattern {
                        walk(counts, p);
                    }
                }
            }
            _ => {}
        }
        // Recurse into every child node. The match above handles the
        // label-bearing kinds; this generic descent reaches the rest. We render
        // children via the structural API rather than enumerating every
        // `NodeKind`, so new expression kinds are covered automatically.
        for child in child_nodes(node) {
            walk(counts, child);
        }
    }
    walk(&mut counts, module);
    counts
}

/// The directly-owned child `AIRNode`s of `node`, for the field-label walk in
/// [`field_label_occurrences`]. Returns the kind-specific children; the
/// label-bearing kinds (record/field-access/construct/pattern) are descended by
/// the caller's `match`, so here they are skipped to avoid double-counting their
/// label idents.
fn child_nodes(node: &AIRNode) -> Vec<&AIRNode> {
    let mut out: Vec<&AIRNode> = Vec::new();
    macro_rules! p {
        ($e:expr) => {
            out.push($e)
        };
    }
    macro_rules! popt {
        ($e:expr) => {
            if let Some(n) = $e {
                out.push(n)
            }
        };
    }
    macro_rules! pvec {
        ($e:expr) => {
            for n in $e {
                out.push(n)
            }
        };
    }
    match &node.kind {
        NodeKind::Module { imports, items, .. } => {
            pvec!(imports);
            pvec!(items);
        }
        NodeKind::FnDecl {
            params,
            return_type,
            body,
            ..
        } => {
            pvec!(params);
            popt!(return_type.as_deref());
            p!(body);
        }
        NodeKind::EnumDecl { variants, .. } => {
            pvec!(variants);
        }
        // Tuple-payload element types are descended; struct-payload field labels
        // are handled by the caller's match (so they are counted, not skipped).
        NodeKind::EnumVariant {
            payload: EnumVariantPayload::Tuple(types),
            ..
        } => {
            pvec!(types);
        }
        NodeKind::ClassDecl { methods, .. } => {
            // Field labels handled by caller; descend into methods only.
            pvec!(methods);
        }
        NodeKind::TraitDecl { methods, .. } => {
            pvec!(methods);
        }
        NodeKind::ImplBlock {
            target, methods, ..
        } => {
            p!(target);
            pvec!(methods);
        }
        NodeKind::EffectDecl { operations, .. } => {
            pvec!(operations);
        }
        NodeKind::TypeAlias { ty, .. } => p!(ty),
        NodeKind::ConstDecl { ty, value, .. } => {
            p!(ty);
            p!(value);
        }
        NodeKind::ModuleHandle { handler, .. } => p!(handler),
        NodeKind::PropertyTest { body, .. } => p!(body),
        NodeKind::Param {
            pattern,
            ty,
            default,
        } => {
            p!(pattern);
            popt!(ty.as_deref());
            popt!(default.as_deref());
        }
        NodeKind::TypeNamed { args, .. } => {
            pvec!(args);
        }
        NodeKind::TypeTuple { elems } => {
            pvec!(elems);
        }
        NodeKind::TypeFunction { params, ret, .. } => {
            pvec!(params);
            p!(ret);
        }
        NodeKind::TypeOptional { inner } => p!(inner),
        NodeKind::BinaryOp { left, right, .. } => {
            p!(left);
            p!(right);
        }
        NodeKind::UnaryOp { operand, .. } => p!(operand),
        NodeKind::Assign { target, value, .. } => {
            p!(target);
            p!(value);
        }
        NodeKind::Call {
            callee,
            args,
            type_args,
        } => {
            p!(callee);
            for a in args {
                out.push(&a.value);
            }
            pvec!(type_args);
        }
        NodeKind::MethodCall {
            receiver,
            type_args,
            args,
            ..
        } => {
            p!(receiver);
            pvec!(type_args);
            for a in args {
                out.push(&a.value);
            }
        }
        NodeKind::Index { object, index } => {
            p!(object);
            p!(index);
        }
        NodeKind::Propagate { expr }
        | NodeKind::Await { expr }
        | NodeKind::Move { expr }
        | NodeKind::Borrow { expr }
        | NodeKind::MutableBorrow { expr } => p!(expr),
        NodeKind::Lambda { params, body } => {
            pvec!(params);
            p!(body);
        }
        NodeKind::Pipe { left, right } | NodeKind::Compose { left, right } => {
            p!(left);
            p!(right);
        }
        NodeKind::Range { lo, hi, .. } | NodeKind::RangePat { lo, hi, .. } => {
            p!(lo);
            p!(hi);
        }
        NodeKind::ListLiteral { elems }
        | NodeKind::SetLiteral { elems }
        | NodeKind::TupleLiteral { elems }
        | NodeKind::TuplePat { elems } => {
            pvec!(elems);
        }
        NodeKind::MapLiteral { entries } => {
            for e in entries {
                out.push(&e.key);
                out.push(&e.value);
            }
        }
        NodeKind::Interpolation { parts } => {
            for part in parts {
                if let bock_air::AirInterpolationPart::Expr(n) = part {
                    out.push(n.as_ref());
                }
            }
        }
        NodeKind::ResultConstruct { value, .. }
        | NodeKind::Return { value }
        | NodeKind::Break { value } => {
            popt!(value.as_deref());
        }
        NodeKind::If {
            let_pattern,
            condition,
            then_block,
            else_block,
        } => {
            popt!(let_pattern.as_deref());
            p!(condition);
            p!(then_block);
            popt!(else_block.as_deref());
        }
        NodeKind::Guard {
            let_pattern,
            condition,
            else_block,
        } => {
            popt!(let_pattern.as_deref());
            p!(condition);
            p!(else_block);
        }
        NodeKind::Match { scrutinee, arms } => {
            p!(scrutinee);
            pvec!(arms);
        }
        NodeKind::MatchArm {
            pattern,
            guard,
            body,
        } => {
            p!(pattern);
            popt!(guard.as_deref());
            p!(body);
        }
        NodeKind::For {
            pattern,
            iterable,
            body,
        } => {
            p!(pattern);
            p!(iterable);
            p!(body);
        }
        NodeKind::While { condition, body } => {
            p!(condition);
            p!(body);
        }
        NodeKind::Loop { body } => p!(body),
        NodeKind::Block { stmts, tail } => {
            pvec!(stmts);
            popt!(tail.as_deref());
        }
        NodeKind::LetBinding {
            pattern, ty, value, ..
        } => {
            p!(pattern);
            popt!(ty.as_deref());
            p!(value);
        }
        NodeKind::EffectOp { args, .. } => {
            for a in args {
                out.push(&a.value);
            }
        }
        NodeKind::HandlingBlock { handlers, body } => {
            for h in handlers {
                out.push(&h.handler);
            }
            p!(body);
        }
        NodeKind::ConstructorPat { fields, .. } => {
            pvec!(fields);
        }
        NodeKind::ListPat { elems, rest } => {
            pvec!(elems);
            popt!(rest.as_deref());
        }
        NodeKind::OrPat { alternatives } => {
            pvec!(alternatives);
        }
        NodeKind::GuardPat { pattern, guard } => {
            p!(pattern);
            p!(guard);
        }
        // Leaf / label-bearing kinds with no extra children to descend (the
        // latter are handled by the caller's `match`).
        NodeKind::ImportDecl { .. }
        | NodeKind::RecordDecl { .. }
        | NodeKind::FieldAccess { .. }
        | NodeKind::RecordConstruct { .. }
        | NodeKind::RecordPat { .. }
        | NodeKind::TypeSelf
        | NodeKind::Literal { .. }
        | NodeKind::Identifier { .. }
        | NodeKind::Placeholder
        | NodeKind::Unreachable
        | NodeKind::Continue
        | NodeKind::WildcardPat
        | NodeKind::BindPat { .. }
        | NodeKind::LiteralPat { .. }
        | NodeKind::RestPat
        | NodeKind::Error
        | NodeKind::EffectRef { .. } => {}
        // `NodeKind` is `#[non_exhaustive]`. A future kind we have not taught
        // this walker about contributes no field-label children, so the scan
        // falls back to its old (harmless over-import) behavior for it — never
        // under-import.
        _ => {}
    }
    out
}

/// Count quoted-identifier-token occurrences of `name` in `rendered` — the
/// number of `"name"` substrings in the AIR debug dump.
fn quoted_token_count(rendered: &str, name: &str) -> usize {
    rendered.matches(&format!("\"{name}\"")).count()
}

/// Whether a value expression in **binding/expression position** must be lowered
/// to Python *statements* (assigning the binding) rather than emitted as a
/// Python expression.
///
/// Python has no statement-admitting expression form (no value-`loop`, no
/// IIFE), so these constructs cannot ride inside a `let x = …` expression:
///
/// - a `match` whose arms include a statement / diverging body
///   (`_ => { return … }`, see [`crate::generator::match_has_statement_arm`]);
/// - a `loop` / `while` (a value-`loop` yields via `break <v>`, which Python's
///   valueless `break` cannot express);
/// - an `if` that is itself a statement (both branches statement bodies) —
///   it produces no expression value;
/// - a `Block` carrying statements (a tail-only block is fine as an expression).
///
/// When true, [`PyEmitCtx::emit_value_binding`] hoists the construct into real
/// Python statements. Otherwise the existing expression lowering (including the
/// ternary `match`/`if` paths) is used unchanged.
fn value_needs_stmt_form(value: &AIRNode) -> bool {
    match &value.kind {
        NodeKind::Match { arms, .. } => {
            crate::generator::match_has_statement_arm(arms)
                || control_flow_has_raise_branch(value)
                || match_arm_drops_leading_stmts(arms)
        }
        NodeKind::Loop { .. } | NodeKind::While { .. } => true,
        NodeKind::If { .. } => {
            crate::generator::node_is_statement(value) || control_flow_has_raise_branch(value)
        }
        NodeKind::Block { stmts, .. } => !stmts.is_empty(),
        _ => false,
    }
}

/// Whether `node` lowers to a Python **`raise` statement** — a diverging
/// expression that yields no value: a `todo()` / `unreachable()` prelude call
/// (see [`PyEmitCtx::map_prelude_call`]), or the `unreachable` AIR node. Such an
/// expression is valid as a statement but **not** after `return` / `= `
/// (`return raise NotImplementedError()` is a `SyntaxError`), so in value/tail
/// position it must be emitted bare. The fall-through value is supplied by the
/// surrounding control flow — the function simply never returns past the raise.
fn is_raise_expr(node: &AIRNode) -> bool {
    match &node.kind {
        NodeKind::Unreachable => true,
        NodeKind::Call { callee, .. } => matches!(
            &callee.kind,
            NodeKind::Identifier { name }
                if matches!(name.name.as_str(), "todo" | "unreachable")
        ),
        _ => false,
    }
}

/// Whether `node`'s subtree contains a `?` propagate operator that belongs to
/// *this* function/method — used to decide whether the body must be wrapped in
/// the `try: … except _BockPropagate: return …` envelope (see
/// [`PyEmitCtx::emit_fn_body_with_propagate`]). The walk stops at a nested
/// `FnDecl`/`Lambda`/`ClassDecl` boundary: a `?` inside a nested closure or
/// method propagates from *that* inner function, so it gets its own wrapper and
/// must not force one on the enclosing body.
fn body_contains_propagate(node: &AIRNode) -> bool {
    if matches!(node.kind, NodeKind::Propagate { .. }) {
        return true;
    }
    // Do not descend into a nested scope: its `?` is the inner function's.
    if matches!(
        node.kind,
        NodeKind::FnDecl { .. } | NodeKind::Lambda { .. } | NodeKind::ClassDecl { .. }
    ) {
        return false;
    }
    child_nodes(node).iter().any(|c| body_contains_propagate(c))
}

/// The tail/block value of `node` (for an `if`/`match` arm body), unwrapping a
/// single-tail `Block`.
fn unwrap_block_tail(node: &AIRNode) -> &AIRNode {
    if let NodeKind::Block {
        stmts,
        tail: Some(t),
    } = &node.kind
    {
        if stmts.is_empty() {
            return t;
        }
    }
    node
}

/// Whether an **expression-position** `if` (or `match`) has a branch/arm body
/// that lowers to a diverging Python `raise` (`todo()` / `unreachable()`).
/// Such a construct cannot ride inside a ternary (`return raise … if … else …`
/// is a `SyntaxError`), so it must be hoisted to a statement-form `if`/`match`
/// whose non-diverging branches `return` while the diverging branch `raise`s.
fn control_flow_has_raise_branch(node: &AIRNode) -> bool {
    match &node.kind {
        NodeKind::If {
            then_block,
            else_block,
            ..
        } => {
            is_raise_expr(unwrap_block_tail(then_block))
                || control_flow_has_raise_branch(then_block)
                || else_block.as_ref().is_some_and(|eb| {
                    is_raise_expr(unwrap_block_tail(eb)) || control_flow_has_raise_branch(eb)
                })
        }
        NodeKind::Match { arms, .. } => arms.iter().any(|arm| {
            if let NodeKind::MatchArm { body, .. } = &arm.kind {
                is_raise_expr(unwrap_block_tail(body)) || control_flow_has_raise_branch(body)
            } else {
                false
            }
        }),
        _ => false,
    }
}

/// Whether a **value-position** `if` (one consumed as an expression — a
/// function tail, a `return` value) must be lowered to a statement-form
/// `if`/`elif`/`else` rather than a Python ternary.
///
/// The ternary form (`<then> if <cond> else <else>`) emits only each branch's
/// *tail* expression: any statements in a branch block — most importantly a
/// `let` binding — are silently dropped, so a later reference to that binding
/// becomes a `NameError` (the microservice `handle_delete_user` is the canonical
/// case: its `if (authorized) { let role = …; if (role == "admin") … }` lost the
/// `role` binding inside the ternary). Routing such an `if` to statement form
/// (each branch recursing through `emit_block_body`, which emits the `let` then
/// `return`s the tail) preserves the bindings. A branch is "droppable" when its
/// block carries statements (or nests another droppable `if`/`elif`).
fn if_value_needs_stmt_form(node: &AIRNode) -> bool {
    let NodeKind::If {
        then_block,
        else_block,
        ..
    } = &node.kind
    else {
        return false;
    };
    block_has_droppable_stmts(then_block)
        || else_block.as_deref().is_some_and(|eb| {
            if matches!(eb.kind, NodeKind::If { .. }) {
                if_value_needs_stmt_form(eb)
            } else {
                block_has_droppable_stmts(eb)
            }
        })
}

/// True when a block carries leading statements that a value/ternary lowering
/// would drop (it emits only the tail). An empty-statement block is safe as a
/// ternary branch; a block with a `let` / expression statement is not. See
/// [`if_value_needs_stmt_form`].
fn block_has_droppable_stmts(node: &AIRNode) -> bool {
    matches!(&node.kind, NodeKind::Block { stmts, .. } if !stmts.is_empty())
}

/// Whether a **value-position** `match` (one consumed as an expression — a
/// function tail, a `return` value) must be lowered to a statement-form
/// `match`/`case` rather than the `(lambda __v: …)` conditional chain.
///
/// The conditional chain can correctly express a flat dispatch that binds at
/// most a single payload: a literal, range, list, `Some(x)`/`Ok`/`Err`/`None`,
/// a whole-scrutinee bind, or a wildcard. It **cannot** test or bind:
///
/// - guards (it dropped the guard entirely),
/// - or / tuple / nested-constructor / range / list patterns (caught by the
///   shared [`crate::generator::match_needs_ifchain`]),
/// - **record patterns** — even a bare-bind one (`Point { x, .. } => "x=${x}"`),
///   whose field binding the chain left free (`(lambda __v: f"x={x}")(p)` →
///   `NameError: name 'x'`). The shared recogniser treats a bare-bind record
///   field as *not* structured, so it returns false for that shape; this
///   py-local predicate adds record patterns on top so the Python backend routes
///   them to the statement-form `emit_pattern`, which binds `case Point(x=x):`
///   by field name. (Kept py-local rather than widening the shared recogniser,
///   which the if-chain backends consult for their own switch fast-path.)
fn match_value_needs_stmt_form(arms: &[AIRNode]) -> bool {
    crate::generator::match_needs_ifchain(arms)
        || arms.iter().any(|arm| {
            matches!(
                &arm.kind,
                NodeKind::MatchArm { pattern, .. }
                    if matches!(pattern.kind, NodeKind::RecordPat { .. })
            )
        })
        || match_arm_drops_leading_stmts(arms)
}

/// Whether any **value-position** `match` arm carries a leading statement that
/// the `(lambda __v: …)` conditional chain cannot fold into an
/// immediately-applied lambda and would therefore *silently drop*.
///
/// The chain lowers an arm body block with a *value* tail via
/// [`PyEmitCtx::try_emit_block_stmts_as_expr`], which folds a leading simple
/// immutable `let` (`lambda x: …`) or a bare expression statement
/// (`lambda _: …`) into the expression. But a leading construct that has no
/// Python *expression* form — a loop (`for`/`while`/`loop`), an assignment, a
/// `return`/`break`/`continue`, a mutable or destructuring `let`, or a nested
/// block — makes `try_emit_block_stmts_as_expr` bail; the caller then falls
/// back to emitting just the block's tail, dropping the leading statement
/// (e.g. `Ok(n) => { for i in 0..n { log(i) } "ok" }` lost the whole loop).
///
/// When this predicate is true the match is routed to the statement-form
/// `match`/`case` ([`PyEmitCtx::emit_match`]), whose arm bodies recurse through
/// [`PyEmitCtx::emit_block_body`] — emitting each leading statement and then
/// `return`ing the tail — so the side effect runs *and* the value is produced.
/// The check mirrors `try_emit_block_stmts_as_expr`'s bail conditions exactly,
/// so the two stay in agreement (only arms the chain *can* express stay on it).
fn match_arm_drops_leading_stmts(arms: &[AIRNode]) -> bool {
    arms.iter().any(|arm| {
        let NodeKind::MatchArm { body, .. } = &arm.kind else {
            return false;
        };
        // Only a block with both leading statements *and* a value tail rides the
        // lambda chain; a statement-tail / tail-less arm is already routed to
        // statement form by `match_has_statement_arm`, and a tail-only block has
        // nothing to drop.
        let NodeKind::Block {
            stmts,
            tail: Some(_),
        } = &body.kind
        else {
            return false;
        };
        stmts.iter().any(stmt_not_lambda_expressible)
    })
}

/// Whether a leading block statement has no Python *expression* form and so
/// cannot be folded into the `(lambda …: …)` chain by
/// [`PyEmitCtx::try_emit_block_stmts_as_expr`]. Mirrors that method's bail set
/// exactly (see [`match_arm_drops_leading_stmts`]).
fn stmt_not_lambda_expressible(stmt: &AIRNode) -> bool {
    match &stmt.kind {
        // A mutable or non-simple-bind (tuple/record/destructuring) `let` cannot
        // become a `lambda` parameter; a simple immutable `let` can.
        NodeKind::LetBinding {
            is_mut, pattern, ..
        } => *is_mut || PyEmitCtx::simple_bind_name(pattern).is_none(),
        // Statements with no expression form.
        NodeKind::Assign { .. }
        | NodeKind::While { .. }
        | NodeKind::For { .. }
        | NodeKind::Loop { .. }
        | NodeKind::Return { .. }
        | NodeKind::Break { .. }
        | NodeKind::Continue
        | NodeKind::Block { .. } => true,
        // A bare expression statement is foldable via `lambda _: …`.
        _ => false,
    }
}

/// Compute the implicit cross-module imports for `module`: public symbols
/// declared in *other* reachable modules that `module` references but neither
/// declares locally nor imports explicitly. Returns `(module_path, name)`
/// pairs.
///
/// "References" is a conservative structural scan of the module's debug
/// rendering for the symbol name as an identifier token (mirroring
/// [`py_module_uses_optional`] and friends). It can only *over*-import a name
/// the program does not really use, which is harmless (a dead import), never
/// *under*-import — so it cannot reintroduce the `NameError` it exists to fix.
///
/// Exception: a name that appears *only* as a record/enum/class **field label**
/// (declaration, construction, pattern, or `.field` access — see
/// [`field_label_occurrences`]) is **not** a cross-module symbol reference.
/// Importing it anyway was the root cause of a Python import cycle when a record
/// field collided with a sibling module's public function (e.g.
/// `InventorySummary.total_value` vs. `service.total_value`, making `models`
/// import `service` which already imports `models`). We subtract the field-label
/// occurrences so such names are skipped while genuine references still import.
fn implicit_imports_for(
    module: &AIRModule,
    public_symbols: &HashMap<String, String>,
    own_path: &str,
) -> Vec<(String, String)> {
    let local = locally_declared_names(module);
    let explicit = explicitly_imported_names(module);
    let rendered = format!("{module:?}");
    let field_labels = field_label_occurrences(module);
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, declaring_module) in public_symbols {
        if declaring_module == own_path || local.contains(name) || explicit.contains(name) {
            continue;
        }
        // Identifier-token match: the AIR debug rendering quotes identifier
        // names, so `"Iterable"` appears iff the name is referenced. Subtract
        // the field-label occurrences: a name reached *only* through field
        // labels is not a cross-module reference and must not be imported (it
        // would create an import cycle for field/function name collisions).
        let total = quoted_token_count(&rendered, name);
        let labels = field_labels.get(name).copied().unwrap_or(0);
        if total > labels {
            out.push((declaring_module.clone(), name.clone()));
        }
    }
    out
}

/// The shared per-module runtime module name (without extension). In the
/// per-module (native-import) emission path the four runtime preludes
/// (`Optional`, `Result`, `Ordering`, concurrency) live in one file —
/// `_bock_runtime.py` at the build root — and every emitted module imports the
/// names it needs from it. A single shared definition keeps the tagged runtime
/// classes *identical objects* across files, so an `isinstance(x, _BockSome)`
/// in `main.py` still matches a `_BockSome` built in `core/option.py` (separate
/// per-file class definitions would not be `isinstance`-compatible).
const RUNTIME_MODULE_PY: &str = "_bock_runtime";

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
        // Shared pre-pass: hoist value-position diverging control flow (see
        // `hoist_value_cf`) into declare-then-assign temp blocks.
        let module =
            &crate::generator::hoist_value_cf(crate::generator::lower_blanket_into(module.clone()));
        let mut ctx = PyEmitCtx::new();
        ctx.enum_variants =
            crate::generator::collect_enum_variants(&[(module, std::path::Path::new(""))]);
        ctx.trait_decls =
            crate::generator::collect_trait_decls(&[(module, std::path::Path::new(""))]);
        ctx.const_names =
            crate::generator::collect_const_names(&[(module, std::path::Path::new(""))]);
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

    /// Emit a per-module **native import tree** (spec §20.6.1; DQ19 resolved):
    /// each module the entry program reaches through a real `use` is emitted to
    /// its **own** Python file, and cross-module references resolve through real
    /// Python imports (`from core.option import or_else`). This is the sole
    /// `bock build` output path.
    ///
    /// Output-path mapping is keyed on each module's *declared* path, not its
    /// on-disk source path, so the file layout and the import path agree:
    /// `module core.option` ⇒ `core/option.py` and `from core.option import …`.
    /// The **entry** module (the one declaring `main`, else the last in
    /// dependency order) is always emitted as `main.py` so the run model
    /// (`python3 main.py` from the build root) is stable; Python adds the
    /// script's directory to `sys.path`, and `core` resolves as a PEP 420
    /// namespace package (no `__init__.py` needed).
    ///
    /// The four runtime preludes (`Optional`, `Result`, `Ordering`,
    /// concurrency) are emitted **once** into a shared `_bock_runtime.py`
    /// (see `RUNTIME_MODULE_PY`); every module that references one imports it
    /// (`from _bock_runtime import *`). A single shared definition keeps the
    /// tagged runtime classes identical across files so cross-module
    /// `isinstance` checks succeed.
    fn generate_project(
        &self,
        modules: &[(&AIRModule, &std::path::Path)],
    ) -> Result<GeneratedCode, CodegenError> {
        // Shared pre-pass: hoist value-position diverging control flow on every
        // module before registry collection or emission (see `hoist_value_cf`).
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
        // itself), dependency-ordered — never the prelude-only stdlib (see
        // `reachable_modules`).
        let reachable = crate::generator::reachable_modules(modules);
        let modules = reachable.as_slice();
        if modules.is_empty() {
            return Ok(GeneratedCode { files: vec![] });
        }

        // The entry module names `main.py`; every other module is placed at the
        // path mirrored from its declared module-path.
        let entry_idx = modules
            .iter()
            .position(|(m, _)| crate::generator::module_declares_main_fn(m))
            .unwrap_or(modules.len() - 1);

        // Enum-variant / trait registries are collected across the whole
        // reachable set so a reference in one file to a type declared in another
        // lowers identically to the bundling path.
        let enum_variants = crate::generator::collect_enum_variants(modules);
        let trait_decls = crate::generator::collect_trait_decls(modules);
        let const_names = crate::generator::collect_const_names(modules);
        // Map of public symbol → declaring module, for the implicit-import pass.
        let public_symbols = collect_public_symbol_modules(modules);
        // Program-wide field/method name-collision set (snake_cased). Built across
        // *all* reachable modules so a call site in `main.py` to a renamed method
        // declared in `core/error.py` agrees with that declaration.
        let mut field_method_collisions = std::collections::HashSet::new();
        for (module, _) in modules {
            field_method_collisions.extend(crate::generator::collect_record_field_names(
                module,
                to_snake_case,
            ));
        }

        let main_is_async = modules
            .iter()
            .any(|(m, _)| crate::generator::module_main_fn_is_async(m));
        let invocation = self.entry_invocation(main_is_async);

        let mut files: Vec<OutputFile> = Vec::with_capacity(modules.len() + 1);
        // Which runtime preludes any module references — drives `_bock_runtime.py`.
        let mut runtime_optional = false;
        let mut runtime_result = false;
        let mut runtime_ordering = false;
        let mut runtime_concurrency = false;
        let mut runtime_list_functional = false;
        let mut runtime_propagate = false;

        for (i, (module, source_path)) in modules.iter().enumerate() {
            let mut ctx = PyEmitCtx::new();
            ctx.per_module = true;
            // Entry module (declares `main`, emitted as `main.py`) gets the
            // Windows UTF-8 stdout guard in its preamble — entry-only, since it
            // is a process-global side effect.
            ctx.is_entry_module =
                i == entry_idx && crate::generator::module_declares_main_fn(module);
            ctx.enum_variants = enum_variants.clone();
            ctx.trait_decls = trait_decls.clone();
            ctx.const_names = const_names.clone();
            ctx.field_method_collisions = field_method_collisions.clone();
            // Effect-op resolution needs the whole reachable set: a bare op in
            // one module may belong to an effect declared in another.
            ctx.seed_effect_registries(modules);
            ctx.implicit_imports =
                implicit_imports_for(module, &public_symbols, &module_path_string_of(module));
            ctx.emit_node(module)?;
            runtime_optional |= ctx.needs_runtime_optional;
            runtime_result |= ctx.needs_runtime_result;
            runtime_ordering |= ctx.needs_runtime_ordering;
            runtime_concurrency |= ctx.needs_runtime_concurrency;
            runtime_list_functional |= ctx.needs_runtime_list_functional;
            runtime_propagate |= ctx.needs_runtime_propagate;
            let mut content = ctx.finish();

            // The entry file gets the `if __name__ == "__main__": main()`
            // invocation appended (exactly once, only when it declares `main`).
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
            let source_map = SourceMap {
                generated_file,
                ..Default::default()
            };
            files.push(OutputFile {
                path: out_path,
                content,
                source_map: Some(source_map),
            });
        }

        // Emit the shared runtime module with exactly the preludes referenced.
        if runtime_optional
            || runtime_result
            || runtime_ordering
            || runtime_concurrency
            || runtime_list_functional
            || runtime_propagate
        {
            let mut content = String::new();
            // Every runtime name is underscore-prefixed, which `from … import *`
            // skips *unless* the module declares `__all__`. Build `__all__`
            // explicitly from the emitted preludes so the consuming modules'
            // `from _bock_runtime import *` pulls in `_BockSome` / `_bock_none`
            // / … (without it, those names resolve to `NameError` at run time).
            let mut all_names: Vec<&str> = Vec::new();
            if runtime_optional {
                content.push_str(OPTIONAL_RUNTIME_PY);
                content.push('\n');
                all_names.extend(["_BockSome", "_BockNone", "_bock_none"]);
            }
            if runtime_result {
                content.push_str(RESULT_RUNTIME_PY);
                content.push('\n');
                all_names.extend([
                    "_BockOk",
                    "_BockErr",
                    "_bock_parse_int",
                    "_bock_parse_float",
                ]);
            }
            if runtime_ordering {
                content.push_str(ORDERING_RUNTIME_PY);
                content.push('\n');
                all_names.extend([
                    "_BockOrderingLess",
                    "_BockOrderingEqual",
                    "_BockOrderingGreater",
                    "_bock_less",
                    "_bock_equal",
                    "_bock_greater",
                ]);
            }
            if runtime_concurrency {
                content.push_str(CONCURRENCY_RUNTIME_PY);
                content.push('\n');
                all_names.extend(["__BockChannel", "__bock_channel_new", "__bock_spawn"]);
            }
            if runtime_list_functional {
                // `_bock_find` references `_BockSome`/`_bock_none`; the ctx that
                // set `needs_runtime_list_functional` also set
                // `needs_runtime_optional`, so the Optional prelude is already in
                // `content` above this point.
                content.push_str(LIST_FUNCTIONAL_RUNTIME_PY);
                content.push('\n');
                all_names.extend(["_bock_reduce", "_bock_fold", "_bock_find", "_bock_for_each"]);
            }
            if runtime_propagate {
                // `_bock_try` tests success tags by class *name*, so it has no
                // hard reference to `_BockOk`/`_BockSome` and can stand alone even
                // when only one of the Optional/Result preludes is present.
                content.push_str(PROPAGATE_RUNTIME_PY);
                content.push('\n');
                all_names.extend(["_BockPropagate", "_bock_try"]);
            }
            let all_list = all_names
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect::<Vec<_>>()
                .join(", ");
            content.push_str(&format!("__all__ = [{all_list}]\n"));
            files.push(OutputFile {
                path: PathBuf::from(format!("{RUNTIME_MODULE_PY}.py")),
                content,
                source_map: Some(SourceMap {
                    generated_file: format!("{RUNTIME_MODULE_PY}.py"),
                    ..Default::default()
                }),
            });
        }

        Ok(GeneratedCode { files })
    }

    /// Transpile `@test` functions into a `test_bock.py` file (S7).
    ///
    /// `framework`: `"unittest"` emits a `unittest.TestCase` subclass with
    /// `self.assertEqual`/`assertTrue`/…; anything else (default `"pytest"`)
    /// emits module-level `def test_xxx():` with bare `assert` — both discovered
    /// by `pytest` and `python -m unittest`. Functions under test are imported by
    /// name from their emitted modules; the Optional/Result predicate assertions
    /// import the runtime tag classes from `_bock_runtime`.
    fn generate_tests(
        &self,
        modules: &[(&AIRModule, &std::path::Path)],
        framework: &str,
    ) -> Result<crate::generator::TestArtifacts, CodegenError> {
        let reachable = crate::generator::reachable_modules(modules);
        let modules = reachable.as_slice();
        let tests = crate::generator::collect_test_fns(modules);
        if tests.is_empty() {
            return Ok(crate::generator::TestArtifacts::default());
        }
        let entry_idx = modules
            .iter()
            .position(|(m, _)| crate::generator::module_declares_main_fn(m))
            .unwrap_or(modules.len().saturating_sub(1));

        // Cross-module registries, mirroring `generate_project`, so the test
        // bodies lower references identically to the runtime tree.
        let enum_variants = crate::generator::collect_enum_variants(modules);
        let trait_decls = crate::generator::collect_trait_decls(modules);
        let const_names = crate::generator::collect_const_names(modules);
        let mut field_method_collisions = std::collections::HashSet::new();
        for (module, _) in modules {
            field_method_collisions.extend(crate::generator::collect_record_field_names(
                module,
                to_snake_case,
            ));
        }
        let mut ctx = PyEmitCtx::new();
        ctx.per_module = true;
        ctx.enum_variants = enum_variants;
        ctx.trait_decls = trait_decls;
        ctx.const_names = const_names;
        ctx.field_method_collisions = field_method_collisions;
        ctx.seed_effect_registries(modules);

        // Import the functions under test, snake_cased, from each module.
        let mut import_lines: Vec<String> = Vec::new();
        for (i, (module, _)) in modules.iter().enumerate() {
            let fn_names: Vec<String> = crate::generator::exportable_value_names(module)
                .into_iter()
                .filter(|e| e.is_fn)
                .map(|e| to_snake_case(&e.name))
                .collect();
            if fn_names.is_empty() {
                continue;
            }
            let module_import = if i == entry_idx {
                "main".to_string()
            } else {
                crate::generator::module_path_string(module).unwrap_or_else(|| "main".to_string())
            };
            import_lines.push(format!(
                "from {module_import} import {}",
                fn_names.join(", ")
            ));
        }
        import_lines.sort_unstable();
        import_lines.dedup();

        // Import only the Optional/Result runtime tag classes the assertions
        // actually reference — `_bock_runtime.py` only defines the runtimes the
        // program uses, so importing an absent class (e.g. `_BockOk` in an
        // Optional-only program) would be an ImportError at test load.
        let mut runtime_imports: std::collections::BTreeSet<&str> =
            std::collections::BTreeSet::new();
        for (test_fn, _) in &tests {
            if let NodeKind::FnDecl { body, .. } = &test_fn.kind {
                collect_runtime_tag_imports(body, &mut runtime_imports);
            }
        }

        let is_unittest = framework == "unittest";
        let mut out = String::new();
        if is_unittest {
            out.push_str("import unittest\n");
        }
        if !runtime_imports.is_empty() {
            let names: Vec<&str> = runtime_imports.iter().copied().collect();
            out.push_str(&format!("from _bock_runtime import {}\n", names.join(", ")));
        }
        for line in &import_lines {
            out.push_str(line);
            out.push('\n');
        }
        // Black/PEP 8 puts two blank lines between the import block and the first
        // top-level definition (`class`/`def`). Emitting them here keeps the
        // transpiled test file `black --check`-clean (§20.6.2 codegen-formatter
        // agreement), which CI enforces on the certifying lane.
        out.push_str("\n\n");

        if is_unittest {
            out.push_str("class TestBock(unittest.TestCase):\n");
            for (i, (test_fn, _module_path)) in tests.iter().enumerate() {
                let NodeKind::FnDecl { name, body, .. } = &test_fn.kind else {
                    continue;
                };
                if i > 0 {
                    out.push('\n');
                }
                out.push_str(&format!("    def {}(self):\n", to_snake_case(&name.name)));
                ctx.emit_py_test_body(body, true, 2, &mut out)?;
            }
            out.push_str("\n\nif __name__ == \"__main__\":\n    unittest.main()\n");
        } else {
            // Two blank lines between top-level `def`s (Black/PEP 8) and exactly
            // one trailing newline at end of file.
            for (i, (test_fn, _module_path)) in tests.iter().enumerate() {
                let NodeKind::FnDecl { name, body, .. } = &test_fn.kind else {
                    continue;
                };
                if i > 0 {
                    out.push_str("\n\n");
                }
                out.push_str(&format!("def {}():\n", to_snake_case(&name.name)));
                ctx.emit_py_test_body(body, false, 1, &mut out)?;
            }
        }

        Ok(crate::generator::TestArtifacts {
            files: vec![OutputFile {
                path: PathBuf::from("test_bock.py"),
                content: out,
                source_map: None,
            }],
            entry_append: None,
        })
    }
}

impl PyGenerator {
    /// Output path for one module in the per-module native-import tree.
    ///
    /// The entry module is always `main.py` (mirrored from its source path) so
    /// the run model `python3 main.py` is stable. Every other module is placed
    /// at the path mirrored from its **declared** module-path so the file
    /// location and the Python import path agree:
    /// `module core.option` ⇒ `core/option.py` (imported as `core.option`).
    /// A module without a declared path falls back to its source-mirrored path.
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

// ─── Emission context ────────────────────────────────────────────────────────

/// One lexical-block frame on [`PyEmitCtx::shadow_scopes`].
///
/// Python has function scope, not block scope, for `=`. A Bock `let` that
/// shadows a name bound in an enclosing block would therefore, if emitted as a
/// plain `name = …`, permanently stomp the outer binding — code after the nested
/// block then reads the inner value. Each block frame tracks the Python names
/// bound *directly within it* (`bound`) and, for any name it shadows from an
/// enclosing frame, the fresh alias it was renamed to (`renames`). Identifier
/// emission consults the frame stack innermost-first, so a shadowed name resolves
/// to its alias inside the nested block and to the original once the block ends.
#[derive(Default)]
struct ShadowScope {
    /// Python names bound directly in this block (so a *same-block* re-bind is a
    /// plain rebind, never renamed — `let acc = …; let acc = acc + 1`).
    bound: std::collections::HashSet<String>,
    /// Original-python-name → alias for names this block shadows from an
    /// enclosing frame.
    renames: HashMap<String, String>,
}

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
    /// Set when a numeric primitive method emits `math.*` (`Float.floor`/`ceil`/
    /// `sqrt`/`is_nan`/`is_infinite`), forcing `import math` in the preamble.
    needs_math_import: bool,
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
    /// Set once the Optional runtime prelude has been emitted in the
    /// single-module self-contained path ([`PyGenerator::generate_module`]), so
    /// a module referencing it more than once still inlines it at most once
    /// (redefining the `_BockSome`/`_BockNone` helpers is wasteful and risks
    /// shadowing surprises). The per-module project path imports the runtime
    /// from the shared `RUNTIME_MODULE_PY` module instead.
    optional_runtime_emitted: bool,
    /// Set once the `Result` runtime prelude has been emitted; deduped exactly as
    /// [`Self::optional_runtime_emitted`] (redefining the `_BockOk`/`_BockErr`
    /// classes is wasteful).
    result_runtime_emitted: bool,
    /// Set once the [`ORDERING_RUNTIME_PY`] prelude has been emitted; deduped
    /// exactly as [`Self::optional_runtime_emitted`].
    ordering_runtime_emitted: bool,
    /// Set once the concurrency runtime prelude has been emitted; deduped exactly
    /// as [`Self::optional_runtime_emitted`].
    concurrency_runtime_emitted: bool,
    /// Set once the [`LIST_FUNCTIONAL_RUNTIME_PY`] prelude has been emitted;
    /// deduped exactly as [`Self::optional_runtime_emitted`].
    list_functional_runtime_emitted: bool,
    /// Set once the [`PROPAGATE_RUNTIME_PY`] prelude (`_bock_try` /
    /// `_BockPropagate`) has been emitted; deduped exactly as
    /// [`Self::optional_runtime_emitted`].
    propagate_runtime_emitted: bool,
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
    /// Names already emitted as `T = TypeVar("T")`, deduped within the file so
    /// a type parameter shared by several decls is declared exactly once.
    emitted_typevars: std::collections::HashSet<String>,
    /// User-enum-variant registry (DV14). Routes a construction/pattern to the
    /// `{enum}_{variant}` dataclass and recognises a unit variant (needs `()`
    /// instantiation). Built-in Optional/Result pre-seeds filtered out where
    /// the bespoke `_BockSome`/`_BockNone` lowering applies. Pre-scanned across
    /// the reached modules.
    enum_variants: crate::generator::EnumVariantRegistry,
    /// The reached modules' user-declared traits (keyed by name). Distinguishes a
    /// `T: Equatable` bound that is a real user trait from the compiler-provided
    /// sealed-core conformance, which must drop the `bound=` on the `TypeVar` and
    /// lower `.eq`/`.compare` to native operators (GAP-C). See
    /// [`crate::generator::is_unimplemented_sealed_core_trait`].
    trait_decls: crate::generator::TraitDeclRegistry,
    /// True in the **per-module native-import** emission path
    /// ([`PyGenerator::generate_project`], the sole real-build path). When set,
    /// the `Module` arm imports each needed runtime prelude from the shared
    /// `RUNTIME_MODULE_PY` module instead of inlining its definitions, and the
    /// `ImportDecl` arm emits a real `from <module> import …` rather than a
    /// no-op. When clear, the module is emitted as a single self-contained file
    /// with its runtime preludes inlined — the [`PyGenerator::generate_module`]
    /// path used by unit tests.
    per_module: bool,
    /// In the per-module path, records which shared-runtime names this module
    /// must import from `RUNTIME_MODULE_PY`: Optional, Result, Ordering,
    /// concurrency — set from the same structural scans the bundling path uses
    /// to decide whether to inline a prelude. `finish` turns these into the
    /// module's `from _bock_runtime import …` line.
    needs_runtime_optional: bool,
    needs_runtime_result: bool,
    needs_runtime_ordering: bool,
    needs_runtime_concurrency: bool,
    needs_runtime_list_functional: bool,
    /// In the per-module path, set when this module uses the `?` propagate
    /// operator, so it imports `_bock_try` / `_BockPropagate` from the shared
    /// `RUNTIME_MODULE_PY`. Mirrors [`Self::needs_runtime_optional`].
    needs_runtime_propagate: bool,
    /// Implicit cross-module imports for the per-module path, as
    /// `(module_path, symbol_name)` pairs — names this module references but
    /// neither declares locally nor imports via an explicit `use` (e.g. a
    /// §18.2-prelude trait used as a base class). The `Module` arm emits a
    /// `from <module_path> import <symbol_name>` for each, grouped by module,
    /// after the explicit imports. Computed in `generate_project`.
    implicit_imports: Vec<(String, String)>,
    /// Snake-cased record/class field names across the reachable program, used to
    /// disambiguate a method whose snake_cased name collides with a field name
    /// (`core.error`'s `message` field + `message()` method). A `@dataclass`
    /// field overwrites a same-named method attribute on the class, so the
    /// *method* is renamed (`message_method`) at its definition and every call
    /// site via [`Self::py_method_name`]; the field keeps its name. Pre-seeded
    /// program-wide by `generate_project` (and extended per-module by the
    /// `Module` arm for the single-module `generate_module` path). Shared policy
    /// with go/js/ts.
    field_method_collisions: std::collections::HashSet<String>,
    /// Set on the **entry** module (the one declaring `main`, emitted as
    /// `main.py`) in the per-module path. When set, `finish` prepends a
    /// `sys.stdout.reconfigure(encoding="utf-8")` guard so unicode `print`
    /// output is correct on Windows, whose Python defaults stdout to the locale
    /// codepage rather than UTF-8. Entry-only: the reconfigure is a
    /// process-global side effect, so it belongs at the single program entry,
    /// not in every imported module.
    is_entry_module: bool,
    /// Stack of "current loop's value target" used to lower an
    /// **expression-position `loop`** assigned to a binding
    /// (`let r = loop { … break v }`). Python's `break` carries no value, so a
    /// `loop` that yields a value cannot be an expression. When such a loop is
    /// hoisted to statement form by [`Self::emit_value_binding`], the target
    /// variable is pushed here; a `break <value>` inside then lowers to
    /// `<target> = <value>` followed by `break`. `None` is pushed for ordinary
    /// statement-position loops (no value), so a bare `break` stays a bare
    /// `break`. Only the innermost frame is consulted.
    loop_value_targets: Vec<Option<String>>,
    /// Declared names of module-scope `const`s, pre-scanned across the reachable
    /// program. A const is emitted verbatim at both its declaration and every use
    /// so the two agree — the def's `to_snake_case` (`FIZZ_NUM` → `fizz_num`) and
    /// the use site's uppercase-preserving `identifier_to_py` (`FIZZ_NUM`) would
    /// otherwise disagree, raising `NameError`. See
    /// [`crate::generator::collect_const_names`].
    const_names: std::collections::HashSet<String>,
    /// Stack of lexical-block frames for nested-block `let`-shadow renaming (see
    /// [`ShadowScope`]). A frame is pushed on entering a Bock `{ }` block (every
    /// function/method body, value-block, `if`/`else`/`match`-arm/loop/guard
    /// body) and popped on leaving it.
    shadow_scopes: Vec<ShadowScope>,
    /// Monotonic counter for generating fresh shadow-alias names
    /// (`{name}__s{N}`), unique per emission context.
    shadow_counter: usize,
    /// Names to seed into the *next* shadow frame pushed by
    /// [`Self::emit_block_body`] — used to put a function/method's parameters in
    /// the same frame as its body block, so a body-level `let` re-binding a param
    /// is a plain Python rebind (the idiom) while a *nested*-block `let`
    /// shadowing the param is renamed. Drained (cleared) on the next push.
    pending_scope_seed: Vec<String>,
    /// `true` while emitting the **immediate** body of a `for`/`while`/`loop`
    /// (set by [`Self::emit_loop_body`]). A loop body is statement position: its
    /// tail expression is *discarded* (a Bock loop evaluates to Unit), so
    /// [`Self::emit_block_body_inner`] must emit the tail as a bare expression
    /// statement (`<value>`) rather than a function-body `return <value>` — a
    /// `return` inside a loop aborts the enclosing function after one iteration
    /// (the fizzbuzz / inventory-system truncation). Saved/restored around the
    /// loop body and cleared while emitting any *nested* value context (a
    /// value-binding hoist, a value-`if`/`match` arm), so the discard applies
    /// only to the loop's own tail and never leaks into a value position. A
    /// `break v` value still flows through the separate `loop_value_targets`
    /// stack, not this flag.
    in_loop_body_tail: bool,
    /// `true` while emitting the arm/branch bodies of a **statement-position**
    /// control-flow construct — a `match` or an `if`/`else` that sits mid-block
    /// as a side-effecting statement, not as the block/function tail nor a
    /// value-binding RHS (set by [`Self::emit_stmt`]'s `Match` and `If` arms).
    /// Like [`Self::in_loop_body_tail`], such a construct evaluates to Unit:
    /// each arm's/branch's tail expression is *discarded*, so
    /// [`Self::emit_block_body_inner`] must emit it as a bare expression statement
    /// (`<value>`) rather than a function-body `return <value>`. Emitting `return`
    /// here aborts the enclosing function after the matched arm/taken branch runs
    /// — the chat-protocol truncation, where `match decoded { Ok(m) =>
    /// println(..) … }` returned out of `main` after the first arm instead of
    /// falling through to the rest of the body (#259), and its `if`/`else`
    /// sibling (Q-python-ifelse-truncation), where `if c { println(..) } else {
    /// println(..) }` returned out after either branch. Saved/restored around
    /// the arm/branch bodies and cleared while emitting any *nested* value
    /// context (a nested `fn`/method body, a value-binding hoist), so the
    /// discard applies only to the statement construct's own tails and never
    /// leaks into a value position.
    in_stmt_construct_arm: bool,
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
            needs_math_import: false,
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
            list_functional_runtime_emitted: false,
            propagate_runtime_emitted: false,
            needs_union_import: false,
            needs_typing_callable: Cell::new(false),
            needs_typing_any: Cell::new(false),
            needs_typing_self: Cell::new(false),
            needs_typing_never: Cell::new(false),
            needs_typing_typevar: Cell::new(false),
            emitted_typevars: std::collections::HashSet::new(),
            enum_variants: crate::generator::EnumVariantRegistry::new(),
            trait_decls: crate::generator::TraitDeclRegistry::new(),
            per_module: false,
            needs_runtime_optional: false,
            needs_runtime_result: false,
            needs_runtime_ordering: false,
            needs_runtime_concurrency: false,
            needs_runtime_list_functional: false,
            needs_runtime_propagate: false,
            implicit_imports: Vec::new(),
            field_method_collisions: std::collections::HashSet::new(),
            const_names: std::collections::HashSet::new(),
            is_entry_module: false,
            loop_value_targets: Vec::new(),
            shadow_scopes: Vec::new(),
            shadow_counter: 0,
            pending_scope_seed: Vec::new(),
            in_loop_body_tail: false,
            in_stmt_construct_arm: false,
        }
    }

    /// The Python method name for a Bock method, disambiguated against the
    /// program's field names so a method whose snake_cased name collides with a
    /// field gets a `_method` suffix (`message` → `message_method`). Applied
    /// identically at the method definition and every call site (shared policy
    /// with go/js/ts — see [`crate::generator::disambiguate_method_name`]).
    fn py_method_name(&self, name: &str) -> String {
        // A method/associated-fn whose snake-cased name is a Python *keyword*
        // (e.g. a `From` impl's `from`) cannot be a `def` name or an attribute
        // access — `def from()` and `Type.from(...)` are both syntax errors. Such
        // names are escaped with a trailing `_` (`from` → `from_`), applied
        // identically at the definition and every call site. Ordinary member
        // names (`default`, etc.) are legal Python attributes and are not
        // escaped; only true keywords are.
        let snake = to_snake_case(name);
        let escaped =
            if crate::generator::is_target_keyword(&snake, crate::generator::KeywordTarget::Python)
            {
                format!("{snake}_")
            } else {
                snake
            };
        crate::generator::disambiguate_method_name(
            escaped,
            &self.field_method_collisions,
            "_method",
        )
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
        // Windows UTF-8 stdout (entry module only). On Windows, Python's stdout
        // defaults to the locale codepage, so a unicode `print` (`✓`, `→`, CJK,
        // …) raises `UnicodeEncodeError` or mojibakes. Reconfiguring stdout/stderr
        // to UTF-8 (py3.7+ `TextIOWrapper.reconfigure`) makes output consistent
        // with the POSIX targets. It is a process-global side effect, so it is
        // emitted only at the single program entry, never in imported modules.
        // The `getattr` guard keeps it a no-op when the stream is not a
        // reconfigurable `TextIOWrapper` (e.g. already wrapped/redirected).
        if self.is_entry_module {
            preamble.push_str(
                "import sys as _sys\n\
                 if hasattr(_sys.stdout, \"reconfigure\"):\n    \
                 _sys.stdout.reconfigure(encoding=\"utf-8\")\n\
                 if hasattr(_sys.stderr, \"reconfigure\"):\n    \
                 _sys.stderr.reconfigure(encoding=\"utf-8\")\n",
            );
        }
        // Per-module native-import path: pull the runtime-prelude names this
        // module references from the shared `_bock_runtime` module so the
        // tagged runtime classes are shared (and `isinstance`-compatible)
        // across every emitted file (see `RUNTIME_MODULE_PY`). A `*` import
        // is intentional — the runtime exposes a small, fixed, underscore-
        // prefixed surface and the exact set of referenced names varies with
        // how each prelude is used (constructors, singletons, match classes).
        if self.per_module
            && (self.needs_runtime_optional
                || self.needs_runtime_result
                || self.needs_runtime_ordering
                || self.needs_runtime_concurrency
                || self.needs_runtime_list_functional
                || self.needs_runtime_propagate)
        {
            let _ = writeln!(preamble, "from {RUNTIME_MODULE_PY} import *");
        }
        if self.needs_asyncio_import {
            preamble.push_str("import asyncio\n");
        }
        if self.needs_time_import {
            preamble.push_str("import time\n");
        }
        if self.needs_math_import {
            preamble.push_str("import math\n");
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

    /// Emit a `@test` function body (S7) into `out`, lowering `expect(...)`
    /// assertion chains to pytest-style `assert` (or `self.assert*` for
    /// unittest) and other statements to plain expression/`=` statements.
    ///
    /// `use_self` selects the unittest idiom (`self.assertEqual(a, e)`); `indent`
    /// is the base indentation level (1 for a module-level `def`, 2 for a method
    /// inside a `TestCase`). A body with no statements emits `pass`.
    fn emit_py_test_body(
        &mut self,
        body: &AIRNode,
        use_self: bool,
        indent: usize,
        out: &mut String,
    ) -> Result<(), CodegenError> {
        let pad = "    ".repeat(indent);
        let stmts: Vec<&AIRNode> = match &body.kind {
            NodeKind::Block { stmts, tail } => stmts.iter().chain(tail.as_deref()).collect(),
            _ => vec![body],
        };
        let mut emitted_any = false;
        for stmt in stmts {
            emitted_any = true;
            if let Some((assertion, actual, expected)) = crate::generator::classify_assertion(stmt)
            {
                let a = self.expr_to_string(actual)?;
                use crate::generator::TestAssertion as T;
                let line = if use_self {
                    match assertion {
                        T::Equal => {
                            let e = match expected {
                                Some(e) => self.expr_to_string(e)?,
                                None => "None".to_string(),
                            };
                            format!("self.assertEqual({a}, {e})")
                        }
                        T::BeTrue => format!("self.assertTrue({a})"),
                        T::BeFalse => format!("self.assertFalse({a})"),
                        T::BeSome => format!("self.assertIsInstance({a}, _BockSome)"),
                        T::BeNone => format!("self.assertIsInstance({a}, _BockNone)"),
                        T::BeOk => format!("self.assertIsInstance({a}, _BockOk)"),
                        T::BeErr => format!("self.assertIsInstance({a}, _BockErr)"),
                    }
                } else {
                    match assertion {
                        T::Equal => {
                            let e = match expected {
                                Some(e) => self.expr_to_string(e)?,
                                None => "None".to_string(),
                            };
                            format!("assert ({a}) == ({e})")
                        }
                        T::BeTrue => format!("assert ({a}) is True"),
                        T::BeFalse => format!("assert ({a}) is False"),
                        T::BeSome => format!("assert isinstance({a}, _BockSome)"),
                        T::BeNone => format!("assert isinstance({a}, _BockNone)"),
                        T::BeOk => format!("assert isinstance({a}, _BockOk)"),
                        T::BeErr => format!("assert isinstance({a}, _BockErr)"),
                    }
                };
                out.push_str(&format!("{pad}{line}\n"));
            } else if let NodeKind::LetBinding { pattern, value, .. } = &stmt.kind {
                let name = match &pattern.kind {
                    NodeKind::BindPat { name, .. } => to_snake_case(&name.name),
                    _ => {
                        emitted_any = false;
                        continue;
                    }
                };
                let v = self.expr_to_string(value)?;
                out.push_str(&format!("{pad}{name} = {v}\n"));
            } else {
                let s = self.expr_to_string(stmt)?;
                out.push_str(&format!("{pad}{s}\n"));
            }
        }
        if !emitted_any {
            out.push_str(&format!("{pad}pass\n"));
        }
        Ok(())
    }

    /// Pre-seed the effect registries (`effect_ops`, `composite_effects`) from
    /// every module's top-level `EffectDecl`s. In the per-module path each
    /// module is emitted by its own context, so a bare op `log(...)` used in
    /// `main` whose effect `Log` is declared in another module would not be
    /// recognised as an effect op (and not rewritten to `handler.log(...)`)
    /// without pre-seeding from the whole reachable set. Mirrors how
    /// `enum_variants` / `trait_decls` are collected across the reached modules.
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
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                // Route through an installed `Clock` handler if one is in scope;
                // otherwise fall through to the host primitive (default).
                if let Some(handler) = self.clock_handler_var() {
                    format!("{handler}.{}({a})", to_snake_case("sleep"))
                } else {
                    self.needs_asyncio_import = true;
                    // Duration is ns → asyncio.sleep takes seconds.
                    format!("asyncio.sleep(({a}) / 1_000_000_000)")
                }
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

    /// Emit an in-place `List` mutator (`push`/`append`, DQ18) to its Python
    /// form.
    ///
    /// Recognised via [`crate::generator::desugared_list_mutating_method`].
    /// Python lists grow in place with `.append(x)`, so `recv.push(x)` lowers to
    /// `(recv).append(x)`. The checker types these as `Void`, so they appear in
    /// statement position (Python's `list.append` returns `None`); the receiver
    /// is a `mut` lvalue (ownership-enforced), evaluated once.
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
        self.buf.push_str(").append(");
        self.emit_expr(&x.value)?;
        self.buf.push(')');
        Ok(true)
    }

    /// Emit a functional (closure-taking) `List` built-in method call to its
    /// Python form.
    ///
    /// Recognised via [`crate::generator::desugared_list_functional_method`].
    /// `map`/`filter` lower to `list(map(cb, r))` / `list(filter(cb, r))`;
    /// `any`/`all` to the `any(...)`/`all(...)` builtins over `map`; `flat_map`
    /// to a nested comprehension. `reduce`/`fold`/`find`/`for_each` lower to the
    /// `_bock_*` helpers of [`LIST_FUNCTIONAL_RUNTIME_PY`] (a left fold and the
    /// tagged-`Optional` `find` cannot be expressed as a single Python builtin
    /// call). In all cases the closure is passed *once* — the desugared
    /// `recv.map(recv, cb)` shape the generic fall-through would emit fails on a
    /// `list` (`'list' object has no attribute 'map'`).
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
            "map" | "filter" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let _ = write!(self.buf, "list({method}(");
                self.emit_expr(&cb.value)?;
                self.buf.push_str(", ");
                self.emit_expr(recv)?;
                self.buf.push_str("))");
            }
            "any" | "all" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let _ = write!(self.buf, "{method}(map(");
                self.emit_expr(&cb.value)?;
                self.buf.push_str(", ");
                self.emit_expr(recv)?;
                self.buf.push_str("))");
            }
            "flat_map" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str("[__y for __x in ");
                self.emit_expr(recv)?;
                self.buf.push_str(" for __y in (");
                self.emit_expr(&cb.value)?;
                self.buf.push_str(")(__x)]");
            }
            "reduce" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str("_bock_reduce(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&cb.value)?;
                self.buf.push(')');
            }
            "fold" => {
                let (Some(init), Some(cb)) = (rest.first(), rest.get(1)) else {
                    return Ok(false);
                };
                self.buf.push_str("_bock_fold(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&init.value)?;
                self.buf.push_str(", ");
                self.emit_expr(&cb.value)?;
                self.buf.push(')');
            }
            "find" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str("_bock_find(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&cb.value)?;
                self.buf.push(')');
            }
            "for_each" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                self.buf.push_str("_bock_for_each(");
                self.emit_expr(recv)?;
                self.buf.push_str(", ");
                self.emit_expr(&cb.value)?;
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
    ///
    /// Gated on `recv_kind = "Primitive:String"` directly (not the cross-backend
    /// [`crate::generator::desugared_string_method`] subset) so Python can lower
    /// the wider resolved String surface — `slice`/`substring`/`char_at`/
    /// `index_of`/`repeat`/`reverse`/`trim_start`/`trim_end` — to native ops,
    /// matching the Rust backend. Python `str` is already a code-point sequence,
    /// so scalar slicing is plain `s[a:b]` and `reverse` is `s[::-1]`.
    /// `char_at`/`index_of` build the tagged `Optional` runtime (`_BockSome(v)` /
    /// `_bock_none`); the Optional prelude is pulled in by the structural scan
    /// over the (Optional-typed) call.
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
            "len" | "length" | "count" => format!("len({recv_str})"),
            "byte_len" => format!("len(({recv_str}).encode())"),
            "is_empty" => format!("(len({recv_str}) == 0)"),
            "to_upper" => format!("({recv_str}).upper()"),
            "to_lower" => format!("({recv_str}).lower()"),
            "trim" => format!("({recv_str}).strip()"),
            "trim_start" => format!("({recv_str}).lstrip()"),
            "trim_end" => format!("({recv_str}).rstrip()"),
            "reverse" => format!("({recv_str})[::-1]"),
            "to_string" | "display" => format!("str({recv_str})"),
            "repeat" => {
                let Some(n) = arg0(self)? else {
                    return Ok(false);
                };
                format!("(({recv_str}) * ({n}))")
            }
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
            // `slice`/`substring(start, end)`: scalar-index half-open substring
            // (spec §18.3). Python `str` slicing is already code-point based, and
            // out-of-range indices clamp rather than raise — matching the spec.
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
                format!("({recv_str})[{start}:{end}]")
            }
            // `char_at(i)` returns `Optional[Char]` — `None` when out of range.
            "char_at" => {
                let Some(i) = arg0(self)? else {
                    return Ok(false);
                };
                format!(
                    "(lambda __s, __i: _BockSome(__s[__i]) if 0 <= __i < len(__s) else _bock_none)({recv_str}, {i})"
                )
            }
            // `index_of(needle)` returns `Optional[Int]` — scalar index of the
            // first match, or `None`. Python `str.find` is already code-point based.
            "index_of" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!(
                    "(lambda __s, __p: (lambda __b: _BockSome(__b) if __b >= 0 else _bock_none)(__s.find(__p)))({recv_str}, {p})"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Q-prim-assoc: lower a primitive associated-conversion call
    /// (`Float.from(x)` / `Int.try_from(s)` / `String.from(c)`) to Python's
    /// native conversion. CRITICAL on Python: `from` is a keyword, so the
    /// generic associated-call form would emit `Float.from_(...)` (an undefined
    /// name and the wrong shape). `from` becomes `float(...)`/`int(...)`/
    /// `str(...)`; `try_from` calls the self-contained `_bock_parse_int` /
    /// `_bock_parse_float` runtime helpers (which return `_BockOk`/`_BockErr`),
    /// passing a `ConvertError`-factory lambda so the helper need not import the
    /// stdlib type (`ConvertError` is in scope at the call site via the
    /// `Result[T, ConvertError]` return type). Returns `true` when handled.
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
            ("Float", "from") => format!("float({arg_str})"),
            ("Int", "from") => format!("int({arg_str})"),
            ("String", "from") => format!("str({arg_str})"),
            ("Int", "try_from") => {
                self.needs_runtime_result = true;
                format!("_bock_parse_int({arg_str}, lambda __m: ConvertError(message=__m))")
            }
            ("Float", "try_from") => {
                self.needs_runtime_result = true;
                format!("_bock_parse_float({arg_str}, lambda __m: ConvertError(message=__m))")
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Lower a desugared numeric/`Char`/`Bool` primitive method (`recv_kind =
    /// "Primitive:Int" | "Primitive:Float" | "Primitive:Char" | "Primitive:Bool"`)
    /// to its native Python form. Covers the conversion and math methods the
    /// checker resolves on the scalar primitives — `to_float`/`to_int`/`abs`/`min`/
    /// `max`/`clamp`/`floor`/`ceil`/`round`/`sqrt`/… . Wired into the `Call` arm
    /// alongside [`Self::try_emit_string_method`], before the generic
    /// desugared-self-call fall-through (which would emit `n.to_float(n)`).
    /// `floor`/`ceil`/`sqrt` need `math`, so they set `needs_math_import`.
    /// `compare`/`eq`/`to_string`/`display`/`hash_code` stay on the primitive
    /// *bridge* path. `Char` is a one-code-point Python `str`.
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
            // Conversions.
            ("Int", "to_float") => format!("float({recv_str})"),
            ("Float", "to_int") => format!("int({recv_str})"),
            ("Char", "to_int") => format!("ord({recv_str})"),
            ("Bool", "to_int") => format!("(1 if ({recv_str}) else 0)"),
            // Int math.
            ("Int", "abs") => format!("abs({recv_str})"),
            ("Int" | "Float", "min") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("min({recv_str}, {o})")
            }
            ("Int" | "Float", "max") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("max({recv_str}, {o})")
            }
            ("Int" | "Float", "clamp") => {
                let (Some(lo), Some(hi)) = (arg(self, 0)?, arg(self, 1)?) else {
                    return Ok(false);
                };
                format!("min(max({recv_str}, {lo}), {hi})")
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
            ("Float", "abs") => format!("abs({recv_str})"),
            ("Float", "floor") => {
                self.needs_math_import = true;
                format!("float(math.floor({recv_str}))")
            }
            ("Float", "ceil") => {
                self.needs_math_import = true;
                format!("float(math.ceil({recv_str}))")
            }
            ("Float", "round") => format!("float(round({recv_str}))"),
            ("Float", "sqrt") => {
                self.needs_math_import = true;
                format!("math.sqrt({recv_str})")
            }
            ("Float", "is_nan") => {
                self.needs_math_import = true;
                format!("math.isnan({recv_str})")
            }
            ("Float", "is_infinite") => {
                self.needs_math_import = true;
                format!("math.isinf({recv_str})")
            }
            // Bool.
            ("Bool", "negate") => format!("(not ({recv_str}))"),
            // `Bool.to_string()` / `.display()` must yield the canonical lowercase
            // `"true"`/`"false"` (§3.5), not Python's `str(b)` → `"True"`/`"False"`.
            // Handled here (before the primitive *bridge* path that maps
            // `to_string` → `str(..)`) so the Bool case is intercepted.
            ("Bool", "to_string" | "display") => {
                format!("('true' if ({recv_str}) else 'false')")
            }
            // Char (a one-code-point Python `str`).
            ("Char", "to_upper") => format!("({recv_str}).upper()"),
            ("Char", "to_lower") => format!("({recv_str}).lower()"),
            ("Char", "is_alpha") => format!("({recv_str}).isalpha()"),
            ("Char", "is_digit") => format!("({recv_str}).isdigit()"),
            ("Char", "is_whitespace") => format!("({recv_str}).isspace()"),
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
                // Route through an installed `Clock` handler's `now_monotonic`
                // op if one is in scope; otherwise emit the host primitive.
                if let Some(handler) = self.clock_handler_var() {
                    format!("{handler}.{}()", to_snake_case("now_monotonic"))
                } else {
                    self.needs_time_import = true;
                    "time.monotonic_ns()".to_string()
                }
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
                // `instant.elapsed()` is derived: `now - instant`. Route the
                // "now" read through an installed `Clock` handler if in scope;
                // otherwise read the host monotonic clock (default).
                if let Some(handler) = self.clock_handler_var() {
                    format!(
                        "({handler}.{}() - ({recv_str}))",
                        to_snake_case("now_monotonic")
                    )
                } else {
                    self.needs_time_import = true;
                    format!("(time.monotonic_ns() - ({recv_str}))")
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

    // ── Top-level dispatch ──────────────────────────────────────────────────

    fn emit_node(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::Module { items, imports, .. } => {
                // Field/method name-collision set (snake_cased). Pre-seeded
                // program-wide by `generate_project` so a call site in one file
                // agrees with the renamed method declared in another; extended
                // here so the single-module `generate_module` path (no pre-seed)
                // is also covered.
                self.field_method_collisions
                    .extend(crate::generator::collect_record_field_names(
                        node,
                        to_snake_case,
                    ));
                if self.per_module {
                    // Per-module native-import path (the real build): each module
                    // is emitted to its own file and the shared runtime preludes
                    // live in `_bock_runtime.py`. Record which prelude names this
                    // module references; `finish` emits the single
                    // `from _bock_runtime import *` line.
                    if py_module_uses_optional(items) {
                        self.needs_runtime_optional = true;
                    }
                    if py_module_uses_result(items) {
                        self.needs_runtime_result = true;
                    }
                    if py_module_uses_ordering(items) {
                        self.needs_runtime_ordering = true;
                    }
                    if py_module_uses_concurrency(items) {
                        self.needs_runtime_concurrency = true;
                    }
                    if py_module_uses_list_functional(items) {
                        self.needs_runtime_list_functional = true;
                        // `_bock_find` builds tagged `Optional` runtime values, so
                        // the Optional prelude must be present alongside it.
                        self.needs_runtime_optional = true;
                    }
                    if py_module_uses_propagate(items) {
                        self.needs_runtime_propagate = true;
                    }
                } else {
                    // Single-module self-contained emit (`generate_module`, used
                    // by unit tests): the module's runtime preludes are inlined
                    // into this one file and `ImportDecl`s are dropped. Each
                    // prelude is inlined at most once, gated on a ctx flag.
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
                    // `_bock_find` references `_BockSome`/`_bock_none`, so the
                    // Optional prelude (emitted just above when used) must precede
                    // this one; both are inlined in source order here.
                    if !self.list_functional_runtime_emitted
                        && py_module_uses_list_functional(items)
                    {
                        if !self.optional_runtime_emitted {
                            self.buf.push_str(OPTIONAL_RUNTIME_PY);
                            self.buf.push('\n');
                            self.optional_runtime_emitted = true;
                        }
                        self.buf.push_str(LIST_FUNCTIONAL_RUNTIME_PY);
                        self.buf.push('\n');
                        self.list_functional_runtime_emitted = true;
                    }
                    if !self.propagate_runtime_emitted && py_module_uses_propagate(items) {
                        self.buf.push_str(PROPAGATE_RUNTIME_PY);
                        self.buf.push('\n');
                        self.propagate_runtime_emitted = true;
                    }
                }
                // Per-module path: emit the module's cross-module imports as
                // real Python `from <module> import …` statements at the top of
                // the body (the runtime-prelude import is emitted into the
                // preamble by `finish`). The single-module path drops these.
                if self.per_module {
                    for import in imports {
                        self.emit_node(import)?;
                    }
                    // Implicit imports: prelude-visible names this module
                    // references but does not explicitly `use` (e.g. a base
                    // trait). Grouped per declaring module for one import line
                    // each, in deterministic (sorted) order.
                    let mut by_module: std::collections::BTreeMap<String, Vec<String>> =
                        std::collections::BTreeMap::new();
                    for (module_path, name) in &self.implicit_imports {
                        by_module
                            .entry(module_path.clone())
                            .or_default()
                            .push(name.clone());
                    }
                    let import_lines: Vec<String> = by_module
                        .into_iter()
                        .map(|(module_path, mut names)| {
                            names.sort_unstable();
                            names.dedup();
                            format!("from {module_path} import {}", names.join(", "))
                        })
                        .collect();
                    for line in import_lines {
                        self.writeln(&line);
                    }
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
                // A trait/base class becomes a Python base class of every type
                // that subclasses it (`class Sub(Base):`). Python evaluates the
                // base list at `class` statement time, so the base MUST already be
                // defined — else `NameError: name 'Base' is not defined`. Source
                // order does not guarantee this: a `trait` may be declared after
                // the record/class that impls it (chat-protocol's `Serializable`),
                // and a `record`+inlined-impl is emitted at the record's source
                // position, which can precede the trait. Reorder the *type
                // declarations* so each base precedes its subclasses, keeping the
                // emission otherwise stable (Q-py-impl-before-trait, py slice).
                let order = type_decl_emission_order(items, &self.impls_by_target);
                for (idx, &i) in order.iter().enumerate() {
                    let item = &items[i];
                    if consumed_impls.contains(&item.id) {
                        continue;
                    }
                    // `@test` functions are transpiled separately into pytest/
                    // unittest test files (project mode, §20.6.2 — see
                    // `generate_tests`), never into the runtime module tree: their
                    // `expect(...)` assertion DSL has no runtime definition in the
                    // emitted source.
                    if crate::generator::fn_is_test(item) {
                        continue;
                    }
                    if idx > 0 && !self.buf.is_empty() && !self.buf.ends_with("\n\n") {
                        self.buf.push('\n');
                    }
                    self.emit_node(item)?;
                }
                Ok(())
            }
            NodeKind::ImportDecl { path, items } => {
                if !self.per_module {
                    // Single-module self-contained emit: there is no sibling file
                    // to import from, so the import is a no-op. (Only
                    // `generate_module` — the unit-test path — takes this branch;
                    // the per-module project path emits real imports below.)
                    return Ok(());
                }
                // Per-module native-import path (Python S1): emit a real Python
                // import. The module path's dotted form is both the on-disk
                // package path (`core.option` ⇒ `core/option.py`, emitted by
                // `generate_project`) and the import path, so this resolves when
                // the entry is run from the build root (Python adds the script's
                // dir to `sys.path`, and `core` resolves as a PEP 420 namespace
                // package).
                let module_path = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                if module_path.is_empty() {
                    return Ok(());
                }
                match items {
                    bock_ast::ImportItems::Named(names) => {
                        // A braced cross-module enum VARIANT (`use core.compare.
                        // {Ordering, Less, Equal, Greater}`) is NOT a free
                        // module-level symbol in the emitted Python: the py backend
                        // lowers a user enum variant to a dataclass named
                        // `{Enum}_{Variant}` (`Ordering_Less`), never the bare
                        // `Less`, so `from core.compare import Less` raises
                        // `ImportError: cannot import name 'Less'` at runtime. Drop
                        // the (unaliased) variant leaf names here: the variant is
                        // reached at its use sites as the `Ordering_Less` dataclass
                        // (the use-site lowering already emits that name), and the
                        // implicit-import pass (`implicit_imports_for`) pulls that
                        // dataclass into the module. This mirrors the js/ts filter
                        // (js.rs `ImportItems::Named`, which drops non-js-value
                        // leaves) and the Rust fix (rs.rs `emit_cross_module_uses`,
                        // which reaches a variant via its enum type). The enum TYPE
                        // name (`Ordering`) IS a real module-level symbol
                        // (`Ordering = Union[Ordering_Less, …]`) and is kept, as is
                        // any non-variant leaf. (`user_variant_for_name` returns
                        // `Some` only for user enum variants and excludes the
                        // built-in `Optional`/`Result`.) An *aliased* variant
                        // (`{Less as L}`) is left untouched — aliased-variant
                        // rebinding is a separate, unexercised concern.
                        let rendered: Vec<String> = names
                            .iter()
                            .filter(|n| {
                                n.alias.is_some()
                                    || self.user_variant_for_name(&n.name.name).is_none()
                            })
                            .map(|n| match &n.alias {
                                Some(alias) => format!("{} as {}", n.name.name, alias.name),
                                None => n.name.name.clone(),
                            })
                            .collect();
                        if rendered.is_empty() {
                            // A genuinely-empty braced list (`use mod.{}`) keeps the
                            // `import {module_path}` fallback. But if filtering the
                            // variant leaves emptied a *non-empty* original list,
                            // emit nothing — the dropped variants are covered by the
                            // implicit-import pass + use sites, exactly as js does.
                            if names.is_empty() {
                                self.writeln(&format!("import {module_path}"));
                            }
                        } else {
                            self.writeln(&format!(
                                "from {module_path} import {}",
                                rendered.join(", ")
                            ));
                        }
                    }
                    bock_ast::ImportItems::Glob => {
                        self.writeln(&format!("from {module_path} import *"));
                    }
                    bock_ast::ImportItems::Module => {
                        // `use Foo` brings the module's exported names into
                        // scope unqualified in Bock; a `*` import mirrors that.
                        self.writeln(&format!("from {module_path} import *"));
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
                            let trait_name = tp.segments.last().map(|s| s.name.clone())?;
                            // An impl with no instance methods (e.g. `From`, whose
                            // only method `from` is associated) carries no
                            // instance contract and is often a prelude trait not
                            // emitted here, so it must not be a Python base class
                            // (`class Foot(From)` would raise `NameError`). Its
                            // `from` static method is emitted directly on the
                            // class.
                            if crate::generator::impl_has_instance_method(im, &self.effect_ops) {
                                Some(trait_name)
                            } else {
                                None
                            }
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
                        // `py_field_ident`: a field named after a Python
                        // keyword (`pass`) must be escaped (`pass_`) or the
                        // dataclass declaration is a SyntaxError.
                        self.writeln(&format!("{}: {type_hint}", py_field_ident(&f.name.name)));
                    }
                    for method in Self::dedup_impl_methods(&impls) {
                        self.buf.push('\n');
                        self.emit_class_method(method)?;
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
                base,
                traits,
                ..
            } => {
                // A generic `class C[T]` needs `T = TypeVar("T")` + a
                // `Generic[T, …]` base so `T`-typed members resolve (DV12).
                self.emit_typevars(generic_params);
                // Pull any `impl T { … }` / `impl Trait for T { … }` blocks
                // collected up front (Module pre-scan) so their methods become
                // part of THIS class body — the same path records already use.
                // Without this the inherent/trait methods were silently dropped:
                // the emitted class had only `__init__`, so `t.render()` raised
                // `AttributeError` at runtime (Q-class-codegen, py slice).
                let impls = self.impls_by_target.remove(&name.name).unwrap_or_default();
                // Bases: the declared `base` class, then every implemented trait
                // (both the class-decl `traits` list and any `impl Trait for T`
                // trait paths), then `Generic[..]` for generic params. Dedup so a
                // trait named both on the class header and via an impl block isn't
                // listed twice.
                let mut bases: Vec<String> = Vec::new();
                if let Some(b) = base {
                    bases.push(
                        b.segments
                            .last()
                            .map(|s| s.name.clone())
                            .unwrap_or_default(),
                    );
                }
                for tp in traits {
                    if let Some(seg) = tp.segments.last() {
                        bases.push(seg.name.clone());
                    }
                }
                for im in &impls {
                    if let NodeKind::ImplBlock {
                        trait_path: Some(tp),
                        ..
                    } = &im.kind
                    {
                        if let Some(seg) = tp.segments.last() {
                            bases.push(seg.name.clone());
                        }
                    }
                }
                // Order-preserving dedup: a trait named on both the class header
                // and an `impl` block would otherwise repeat in the base list
                // (which Python rejects — duplicate bases are a `TypeError`).
                let mut seen_bases: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                bases.retain(|b| seen_bases.insert(b.clone()));
                bases.extend(self.generic_base(generic_params));
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
                            // `py_field_ident`: keyword-named fields (`pass`)
                            // must be escaped (`pass_`) in the `__init__`
                            // parameter list and attribute assignments alike.
                            let fname = py_field_ident(&f.name.name);
                            let type_hint = self.ast_type_to_py(&f.ty);
                            format!("{fname}: {type_hint}")
                        })
                        .collect();
                    self.writeln(&format!("def __init__(self, {}):", params.join(", ")));
                    self.indent += 1;
                    for f in fields {
                        let fname = py_field_ident(&f.name.name);
                        self.writeln(&format!("self.{fname} = {fname}"));
                    }
                    self.indent -= 1;
                }
                // Names already taken by an inline `class T { fn … }` method
                // (rare in surface Bock, which puts methods in `impl` blocks, but
                // kept for completeness) — so a same-named impl method does not
                // re-emit and shadow them.
                let mut inline_names: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for method in methods {
                    if let NodeKind::FnDecl { name, .. } = &method.kind {
                        inline_names.insert(name.name.clone());
                    }
                    self.buf.push('\n');
                    self.emit_class_method(method)?;
                }
                // Methods pulled in from inherent + trait impl blocks, deduped by
                // name (inherent precedence) so a delegating trait method never
                // overwrites and self-recurses the inherent one.
                let impl_methods: Vec<&AIRNode> = Self::dedup_impl_methods(&impls)
                    .into_iter()
                    .filter(|m| {
                        !matches!(&m.kind, NodeKind::FnDecl { name, .. } if inline_names.contains(&name.name))
                    })
                    .collect();
                let has_impl_methods = !impl_methods.is_empty();
                for method in impl_methods {
                    self.buf.push('\n');
                    self.emit_class_method(method)?;
                }
                if fields.is_empty() && methods.is_empty() && !has_impl_methods {
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
                        // Rename a field-colliding method consistently with the
                        // inlined-impl path (`emit_class_method`) and call sites.
                        let fn_name = self.py_method_name(&name.name);
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
                // Emit the const's declared name verbatim (not snake_cased) so it
                // matches the verbatim spelling the `Identifier` use-site arm emits
                // for a known const — `to_snake_case` would lower `FIZZ_NUM` to
                // `fizz_num` here while the use site keeps `FIZZ_NUM`, a `NameError`.
                let _ = write!(self.buf, "{ind}{}{type_hint} = ", name.name);
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
    /// deduped within the file via [`Self::emitted_typevars`]. A param
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
        // Seed the body's shadow frame with the parameters (so a body-level `let`
        // re-binding a param is a plain rebind, a nested-block one is renamed).
        self.pending_scope_seed = Self::param_value_names(params);
        // A function-body tail is the function's return value, even for a `fn`
        // *nested inside a loop body or a statement-`match`/`if` arm*: clear the discard
        // flags so this body returns its tail rather than dropping it.
        let prev_discard = std::mem::replace(&mut self.in_loop_body_tail, false);
        let prev_match = std::mem::replace(&mut self.in_stmt_construct_arm, false);
        let body_res = self.emit_fn_body(body);
        self.in_loop_body_tail = prev_discard;
        self.in_stmt_construct_arm = prev_match;
        body_res?;
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
            // An associated function (no `self` receiver, e.g. a `From` impl's
            // `from`) is a `@staticmethod`: it is called as `Type.method(...)`
            // and takes no implicit `self`. A regular method takes `self`.
            let is_assoc = crate::generator::is_associated_impl_method(method, &self.effect_ops);
            // The AIR keeps `self` as a leading `Param`; Python methods need
            // exactly one explicit `self`. Skip the bound `self` param if
            // present so it isn't emitted twice (`def m(self, self)`).
            let rest = match params.first().map(crate::generator::param_binds_self) {
                Some(Some(_)) => &params[1..],
                _ => &params[..],
            };
            let param_strs = self.collect_param_strs(rest);
            let effects = self.effects_params(effect_clause);
            let mut all_params = if is_assoc {
                Vec::new()
            } else {
                vec!["self".to_string()]
            };
            all_params.extend(param_strs);
            all_params.extend(effects);
            let ret = return_type
                .as_deref()
                .map(|t| format!(" -> {}", self.type_to_py(t)))
                .unwrap_or_default();
            // A method whose name collides with a field is renamed (`message`
            // → `message_method`); the dataclass field would otherwise overwrite
            // the method attribute. Renamed identically at every call site.
            let fn_name = self.py_method_name(&name.name);
            if is_assoc {
                self.writeln("@staticmethod");
            }
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
            // Seed the body frame with `self` (regular methods only) + the
            // method params (see `emit_fn_decl`). An associated `@staticmethod`
            // has no `self`.
            let mut seed = if is_assoc {
                Vec::new()
            } else {
                vec!["self".to_string()]
            };
            seed.extend(Self::param_value_names(rest));
            self.pending_scope_seed = seed;
            // A method body's tail is its return value — clear any enclosing
            // discard flags (loop-body or statement-`match`/`if` arm) so it returns
            // rather than dropping its tail.
            let prev_discard = std::mem::replace(&mut self.in_loop_body_tail, false);
            let prev_match = std::mem::replace(&mut self.in_stmt_construct_arm, false);
            let body_res = self.emit_fn_body(body);
            self.in_loop_body_tail = prev_discard;
            self.in_stmt_construct_arm = prev_match;
            body_res?;
            self.current_handler_vars = old_handler_vars;
            self.indent -= 1;
        }
        Ok(())
    }

    /// Flatten the methods of a type's impl blocks into the emission order for a
    /// single Python class body, **deduplicating by method name** so the same
    /// `def` is never emitted twice into one class.
    ///
    /// A type can have both an inherent impl (`impl T { fn render }`) and a trait
    /// impl (`impl Trait for T { fn render }`) whose methods share a name. In
    /// Bock those are distinct (the trait method typically delegates to the
    /// inherent one via `self.render()`), but Python has a single per-class
    /// method namespace: emitting both means the second `def render` silently
    /// overwrites the first, and a delegating trait body (`return self.render()`)
    /// then calls *itself* — unbounded recursion (`RecursionError`, seen on
    /// react-components' `Button`). The **inherent** method is the concrete
    /// implementation and the one a direct `btn.render()` call resolves to, so it
    /// wins; a colliding trait method (which would only shadow it and recurse) is
    /// dropped. Method order is otherwise preserved (inherent impls, then trait
    /// impls, in source order).
    fn dedup_impl_methods<'a>(impls: &'a [AIRNode]) -> Vec<&'a AIRNode> {
        // Inherent impls (no trait_path) take precedence, so visit them first;
        // within each group, source order is preserved.
        let mut out: Vec<&'a AIRNode> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let inherent_first = impls.iter().filter(|im| {
            matches!(
                &im.kind,
                NodeKind::ImplBlock {
                    trait_path: None,
                    ..
                }
            )
        });
        let trait_after = impls.iter().filter(|im| {
            matches!(
                &im.kind,
                NodeKind::ImplBlock {
                    trait_path: Some(_),
                    ..
                }
            )
        });
        for im in inherent_first.chain(trait_after) {
            if let NodeKind::ImplBlock { methods, .. } = &im.kind {
                for method in methods {
                    if let NodeKind::FnDecl { name, .. } = &method.kind {
                        if seen.insert(name.name.clone()) {
                            out.push(method);
                        }
                    }
                }
            }
        }
        out
    }

    /// The Python value-names a parameter list binds (simple `BindPat` params
    /// only). Used to seed the function/method body's shadow frame so a body
    /// `let` re-binding a param is a plain rebind, while a nested-block `let`
    /// shadowing the param is renamed.
    fn param_value_names(params: &[AIRNode]) -> Vec<String> {
        params
            .iter()
            .filter_map(|p| {
                if let NodeKind::Param { pattern, .. } = &p.kind {
                    Self::simple_bind_name(pattern)
                } else {
                    None
                }
            })
            .collect()
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

    /// The in-scope `Clock` effect handler variable, if one is installed.
    ///
    /// Returns the emitted name of the handler bound for the `Clock` effect at
    /// the current point (a `with Clock` parameter such as `clock`, or a
    /// `handling (Clock with ...)` block's synthesised `__clock_hN`). When this
    /// is `Some`, the `Clock` time operations (`Instant.now`, `sleep`, `elapsed`)
    /// are routed through the handler instead of inlining the host primitive
    /// (Q-clock-handler-routing, §18.3.1/§18.4); when `None`, no handler is in
    /// scope and the default host primitive is emitted.
    fn clock_handler_var(&self) -> Option<&str> {
        self.current_handler_vars.get("Clock").map(String::as_str)
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
                        // `py_field_ident`: keyword-named payload fields
                        // (`lambda`) must be escaped (`lambda_`).
                        self.writeln(&format!("{}: {type_hint}", py_field_ident(&f.name.name)));
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
                // Nested-block `let`-shadow handling (simple `BindPat` only): a
                // binding that shadows an enclosing-scope name is renamed to a
                // fresh alias so Python's function-scoped `=` doesn't stomp the
                // outer binding. The rename is *planned* now (so the LHS uses the
                // alias) but *committed* only after the RHS is emitted — the RHS
                // reads the prior binding (`let y = y + 10` reads the outer `y`).
                let raw_name = Self::simple_bind_name(pattern);
                let (binding, pending) = match &raw_name {
                    Some(n) => self.plan_shadow_let(n),
                    None => (self.pattern_to_py_binding(pattern), None),
                };
                let type_hint = ty
                    .as_ref()
                    .map(|t| format!(": {}", self.type_to_py(t)))
                    .unwrap_or_default();
                // Declare-only temp from the shared value-CF hoist: Python has no
                // declarations, so pre-bind `name = None`; the relocated control
                // flow that follows assigns it on every non-diverging path.
                if node.metadata.contains_key(crate::generator::DECL_ONLY_META) {
                    let ind = self.indent_str();
                    let _ = writeln!(self.buf, "{ind}{binding}{type_hint} = None");
                    if let Some(n) = &raw_name {
                        self.commit_shadow_let(n, pending);
                    }
                    return Ok(());
                }
                // Expression-position control flow (a value-`loop`, a `match`
                // with a diverging/statement arm, a statement-`if`) cannot be a
                // Python expression. Pre-declare the binding (so it is always
                // bound, including the diverging-arm path) and fill it in via
                // real statements. See `value_needs_stmt_form`.
                if value_needs_stmt_form(value) {
                    let ind = self.indent_str();
                    let _ = writeln!(self.buf, "{ind}{binding}{type_hint} = None");
                    let r = self.emit_value_binding(&binding, value);
                    if let Some(n) = &raw_name {
                        self.commit_shadow_let(n, pending);
                    }
                    return r;
                }
                // `let x = todo()` — the value diverges (a `raise`), so it cannot
                // sit on the RHS of `=`. Emit the raise bare; the binding is never
                // reached.
                if is_raise_expr(value) {
                    self.write_indent();
                    self.emit_expr(value)?;
                    self.buf.push('\n');
                    return Ok(());
                }
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
                // Commit the rename only now — after the RHS read the prior binding.
                if let Some(n) = &raw_name {
                    self.commit_shadow_let(n, pending);
                }
                Ok(())
            }
            NodeKind::If { .. } => {
                // Statement position: a mid-block `if`/`else` is a Unit
                // statement, so each branch's tail expression is discarded
                // (emitted as a bare expression statement) rather than
                // `return`ed — the same contract as the statement-`match` arm
                // below (Q-python-ifelse-truncation; sibling of the #259
                // chat-protocol truncation). Without the flag, a branch whose
                // body is a bare expression (`if c { println(..) }`) emitted
                // `return print(..)`, aborting the enclosing function after
                // the taken branch so every following statement was skipped.
                // Saved/restored, and cleared inside any nested value context
                // (a nested fn/method body, a value-binding hoist), so it
                // scopes only to this statement's own branch tails.
                let prev = std::mem::replace(&mut self.in_stmt_construct_arm, true);
                let r = self.emit_stmt_if(node, false);
                self.in_stmt_construct_arm = prev;
                r
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
                self.loop_value_targets.push(None);
                // Loop body is statement position: a tail expression is
                // discarded, not `return`ed (see `emit_loop_body`).
                self.emit_loop_body(body)?;
                self.loop_value_targets.pop();
                self.indent -= 1;
                Ok(())
            }
            NodeKind::While { condition, body } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}while ");
                self.emit_expr(condition)?;
                self.buf.push_str(":\n");
                self.indent += 1;
                self.loop_value_targets.push(None);
                // Loop body is statement position: a tail expression is
                // discarded, not `return`ed (see `emit_loop_body`).
                self.emit_loop_body(body)?;
                self.loop_value_targets.pop();
                self.indent -= 1;
                Ok(())
            }
            NodeKind::Loop { body } => {
                self.writeln("while True:");
                self.indent += 1;
                // Statement-position loop yields no value; push a `None` frame so
                // a bare `break` inside stays a bare `break` (and isn't mistaken
                // for an enclosing value-loop's break).
                self.loop_value_targets.push(None);
                // Loop body is statement position: a tail expression is
                // discarded, not `return`ed (see `emit_loop_body`).
                self.emit_loop_body(body)?;
                self.loop_value_targets.pop();
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
                    // A value-`loop` hoisted by `emit_value_binding` records its
                    // assignment target; `break <v>` lowers to `<target> = <v>`
                    // then `break`. Python's `break` itself carries no value.
                    if let Some(Some(target)) = self.loop_value_targets.last() {
                        let target = target.clone();
                        let ind = self.indent_str();
                        let _ = write!(self.buf, "{ind}{target} = ");
                        self.emit_expr(val)?;
                        self.buf.push('\n');
                        self.writeln("break");
                    } else {
                        // No value target in scope (statement-position loop):
                        // record the value as a comment, then break.
                        let ind = self.indent_str();
                        let _ = write!(self.buf, "{ind}# break value: ");
                        self.emit_expr(val)?;
                        self.buf.push('\n');
                        self.writeln("break");
                    }
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
                let_pattern,
                condition,
                else_block,
            } => {
                // The guard `else` block is statement position. Per §8.4 it
                // must diverge (`return`/`break`/`continue`/`Never`) — a
                // diverging tail is a statement and unaffected by the discard
                // flag — but the checker does not currently enforce the
                // divergence (surfaced as OPEN with this fix), and for an
                // accepted non-diverging else every other backend (js/ts/go/
                // rust and the interpreter) falls through to the statements
                // after the guard. Without the flag the bare-expression tail
                // lowered to `return print(..)`, silently truncating the
                // function on Python alone — the same early-`return` family as
                // the statement `match`/`if` fixes above.
                let prev = std::mem::replace(&mut self.in_stmt_construct_arm, true);
                let r = self.emit_stmt_guard(let_pattern.as_deref(), condition, else_block);
                self.in_stmt_construct_arm = prev;
                r
            }
            NodeKind::Match { scrutinee, arms } => {
                // Statement position: a mid-block `match` is a Unit statement, so
                // each arm's tail expression is discarded (emitted as a bare
                // expression statement) rather than `return`ed. Without this, an
                // arm whose body is a bare expression (`Ok(m) => println(..)`)
                // emits `return println(..)`, aborting the enclosing function
                // after the matched arm — the chat-protocol truncation. The flag
                // is saved/restored and cleared inside any nested value context
                // (a nested fn/method body, a value-binding hoist), so it scopes
                // only to this match's own arm tails.
                let prev = std::mem::replace(&mut self.in_stmt_construct_arm, true);
                let r = self.emit_match(scrutinee, arms);
                self.in_stmt_construct_arm = prev;
                r
            }
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

    /// Emit a **statement-position** `if` (with its optional `else`/`else if`
    /// chain). `inline` is true for an `else if` continuation: the caller has
    /// already written this line's indentation plus the `el` prefix, so the
    /// emission starts at `if <cond>:` with no leading indent. (The previous
    /// code re-entered the generic statement emitter after writing `el`, which
    /// wrote its own indentation — `el    if (…):`, a Python SyntaxError, for
    /// every mid-block `else if` chain. The tail-position twin
    /// [`Self::emit_tail_control_flow_inline`] already chained correctly.)
    ///
    /// The caller ([`Self::emit_stmt`]'s `If` arm) sets
    /// [`Self::in_stmt_construct_arm`] around the whole chain, so each branch
    /// body's bare-expression tail lowers to a bare statement, never a
    /// function-body `return` (Q-python-ifelse-truncation).
    fn emit_stmt_if(&mut self, node: &AIRNode, inline: bool) -> Result<(), CodegenError> {
        let NodeKind::If {
            let_pattern,
            condition,
            then_block,
            else_block,
        } = &node.kind
        else {
            // Defensive: only `If` nodes are routed here.
            return self.emit_stmt(node);
        };
        if let Some(pat) = let_pattern {
            // `if let` — bind first, then test. Never reached with `inline`
            // (an `else if let` continuation is emitted under a plain `else:`
            // below, because its binding statement needs its own line).
            let ind = self.indent_str();
            let binding = self.pattern_to_py_binding(pat);
            let _ = write!(self.buf, "{ind}{binding} = ");
            self.emit_expr(condition)?;
            self.buf.push('\n');
            self.writeln(&format!("if {binding} is not None:"));
        } else {
            if inline {
                self.buf.push_str("if ");
            } else {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}if ");
            }
            self.emit_expr(condition)?;
            self.buf.push_str(":\n");
        }
        self.indent += 1;
        self.emit_block_body(then_block)?;
        self.indent -= 1;
        if let Some(else_b) = else_block {
            if let NodeKind::If {
                let_pattern: nested_let,
                ..
            } = &else_b.kind
            {
                if nested_let.is_none() {
                    // `else if` → `elif`: write the `el` prefix, then continue
                    // on the same line.
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}el");
                    return self.emit_stmt_if(else_b, true);
                }
                // `else if let` needs its binding statement first — emit the
                // whole continuation indented under a plain `else:`.
                self.writeln("else:");
                self.indent += 1;
                let r = self.emit_stmt_if(else_b, false);
                self.indent -= 1;
                return r;
            }
            self.writeln("else:");
            self.indent += 1;
            self.emit_block_body(else_b)?;
            self.indent -= 1;
        }
        Ok(())
    }

    /// Emit a **statement-position** `guard (cond) else { … }` /
    /// `guard (let PAT = EXPR) else { … }`. The caller ([`Self::emit_stmt`]'s
    /// `Guard` arm) sets [`Self::in_stmt_construct_arm`] around the call so a
    /// bare-expression tail in the `else` block lowers to a bare statement —
    /// see the rationale there; a spec-conforming diverging `else` (§8.4) is a
    /// statement tail and is emitted unchanged.
    fn emit_stmt_guard(
        &mut self,
        let_pattern: Option<&AIRNode>,
        condition: &AIRNode,
        else_block: &AIRNode,
    ) -> Result<(), CodegenError> {
        if let Some(pat) = let_pattern {
            // `guard (let PAT = EXPR) else { ELSE }` — a refutable
            // binding guard. Lower to a two-arm `match` so PAT's bindings
            // (e.g. `val` in `Ok(val)`) are extracted on success and stay
            // in scope after the guard (Python `match` bindings persist as
            // ordinary assignments); the `_` arm runs the diverging ELSE.
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}match ");
            self.emit_expr(condition)?;
            self.buf.push_str(":\n");
            self.indent += 1;
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}case ");
            self.emit_pattern(pat)?;
            self.buf.push_str(":\n");
            self.indent += 1;
            self.writeln("pass");
            self.indent -= 1;
            self.writeln("case _:");
            self.indent += 1;
            self.emit_block_body(else_block)?;
            self.indent -= 1;
            self.indent -= 1;
            return Ok(());
        }
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}if not (");
        self.emit_expr(condition)?;
        self.buf.push_str("):\n");
        self.indent += 1;
        self.emit_block_body(else_block)?;
        self.indent -= 1;
        Ok(())
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
                    // (`_bock_less` / `_bock_equal` / `_bock_greater`). When the
                    // `core.compare` enum decl is not among the reached modules,
                    // the runtime stands in (mirrors `_bock_none`).
                    self.buf.push_str(ordering_singleton_py(variant));
                } else if let Some(enum_name) = self
                    .user_variant_for_name(&name.name)
                    .map(|i| i.enum_name.clone())
                {
                    // A unit-variant reference (`Empty`) → an instance of its
                    // `@dataclass(frozen=True)` class: `Shape_Empty()`.
                    let _ = write!(self.buf, "{enum_name}_{}()", name.name);
                } else if self.const_names.contains(&name.name) {
                    // A module-scope `const` is emitted verbatim at its
                    // declaration (see the `ConstDecl` arm); spell its use site
                    // identically rather than through `identifier_to_py`.
                    self.buf.push_str(&name.name);
                } else {
                    // Resolve through the shadow-scope stack so a reference inside
                    // a nested block reads the (renamed) shadowing binding while
                    // code outside reads the original (see `ShadowScope`).
                    let py = identifier_to_py(&name.name);
                    self.buf.push_str(&self.resolve_shadow_name(&py));
                }
                Ok(())
            }
            NodeKind::BinaryOp { op, left, right } => {
                // Integer `/` and `%` (DQ23, §3.6). Python's native operators do
                // NOT match the contract: `//` *floors* (`-17 // 5 == -4`, the
                // ruling wants `-3`) and `%` follows floor division (`-17 % 5 == 3`,
                // the ruling wants `-2`); `int(a / b)` routes through lossy float
                // true-division and loses precision on large integers. Lower to an
                // integer-only IIFE that truncates toward zero (quotient magnitude
                // from `abs(a) // abs(b)`, sign from the operands) and gives a
                // dividend-sign remainder. The `//` / `%` inside still raise
                // `ZeroDivisionError` on a zero divisor — an abort — matching the
                // other targets.
                if matches!(op, BinOp::Div | BinOp::Rem) && crate::generator::is_int_arith(node) {
                    let lam = if matches!(op, BinOp::Div) {
                        "(lambda __a, __b: (abs(__a) // abs(__b)) * (1 if (__a < 0) == (__b < 0) else -1))("
                    } else {
                        "(lambda __a, __b: (abs(__a) % abs(__b)) * (1 if __a >= 0 else -1))("
                    };
                    self.buf.push_str(lam);
                    self.emit_expr(left)?;
                    self.buf.push_str(", ");
                    self.emit_expr(right)?;
                    self.buf.push(')');
                    return Ok(());
                }
                // Ordering operators on a user `Comparable` type lower through the
                // type's `compare` (Python's `<` on two instances raises
                // `TypeError` unless they define `__lt__`). The returned `Ordering`
                // is one of the `_BockOrdering*` runtime singletons, tested with
                // `isinstance`: `a < b` ⇒ `isinstance(a.compare(b), …Less)`,
                // `a <= b` ⇒ `not isinstance(a.compare(b), …Greater)`, etc.
                if crate::generator::is_user_compare(node) {
                    if let Some((tag, is_eq)) = crate::generator::user_compare_variant(*op) {
                        let recv = self.expr_to_string(left)?;
                        let other = self.expr_to_string(right)?;
                        let class = ordering_class_py(tag);
                        let neg = if is_eq { "" } else { "not " };
                        let _ = write!(
                            self.buf,
                            "({neg}isinstance(({recv}).compare({other}), {class}))"
                        );
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
                if self.try_emit_time_desugared_method(node, callee, args)? {
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
                // Numeric/Char/Bool primitive methods (`to_float`/`abs`/`sqrt`/…)
                // likewise route by the checker's `recv_kind = "Primitive:Int|…"`
                // before the generic fall-through, which would emit `n.to_float(n)`.
                if self.try_emit_numeric_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_list_mutating_method(node, callee, args)? {
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
                // Q-prim-assoc: a primitive associated-conversion call
                // (`Float.from(x)` / `Int.try_from(s)` / `String.from(c)`)
                // lowers to Python's native conversion. CRITICAL: `from` is a
                // Python keyword, so the static-member form below would emit
                // `Float.from_(...)` against an undefined `Float` — a hard error.
                if self.try_emit_primitive_conversion(node, callee, args)? {
                    return Ok(());
                }
                // Associated-function call (`Type.method(args)` — stamped by the
                // lowerer, no `self` prepended) resolves to the `@staticmethod`
                // on the class. Emit `Type.method(args)` with the type name
                // preserved and the method name run through `py_method_name` (so a
                // keyword like `from` → `from_`, matching the `@staticmethod`
                // definition); the generic fall-through would snake-case the type
                // identifier into a non-existent value.
                if crate::generator::is_associated_call(node) {
                    if let NodeKind::FieldAccess { object, field } = &callee.kind {
                        if let NodeKind::Identifier { name: type_name } = &object.kind {
                            let _ = write!(
                                self.buf,
                                "{}.{}",
                                type_name.name,
                                self.py_method_name(&field.name)
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
                // Desugared instance method call `Call(FieldAccess(recv, m),
                // [recv, ...rest])`: emit `recv.m(rest)` so the receiver binds
                // Python's `self` rather than being passed twice.
                if let Some((recv, method, rest)) =
                    crate::generator::desugared_self_call(callee, args)
                {
                    self.emit_expr(recv)?;
                    let _ = write!(self.buf, ".{}", self.py_method_name(&method.name));
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
                self.emit_callee(callee)?;
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
                let _ = write!(self.buf, ".{}", self.py_method_name(&method.name));
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
                // `py_field_ident`: a keyword-named field (`pass`) reads as the
                // escaped attribute (`t.pass_`) — `t.pass` is a SyntaxError —
                // matching the escaped dataclass/`__init__` declaration.
                let _ = write!(self.buf, ".{}", py_field_ident(&field.name));
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
                // `f >> g` → `(lambda x: g(f(x)))`. The whole lambda is wrapped
                // so that a nested compose — emitted here as a `lambda x: ...`
                // for `left`/`right` — is itself parenthesized before the
                // `(x)` call is appended; otherwise Python binds the `(x)` to
                // the inner lambda's body rather than invoking it. (In practice
                // the AIR lowers `>>` to a `Lambda` before codegen, so this arm
                // is a defensive fall-through; `emit_callee` covers the lowered
                // form.)
                self.buf.push_str("(lambda x: ");
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
                // `expr?` → `_bock_try(expr)`: unwrap the `Ok`/`Some` payload, or
                // raise the `_BockPropagate` sentinel (carrying the `Err`/`None`)
                // that the enclosing function's `try/except` re-returns. The wrap
                // is installed by `emit_fn_body_with_propagate` for any function or
                // method whose body contains a `?` (see `body_contains_propagate`).
                self.buf.push_str("_bock_try(");
                self.emit_expr(expr)?;
                self.buf.push(')');
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
                // `py_field_ident` throughout: keyword-named fields (`pass`)
                // construct through their escaped spelling — kwargs
                // (`Tally(pass_=7)`) and spread dict keys (`"pass_": 9`) must
                // match the escaped dataclass field, and `pass=7` is a
                // SyntaxError besides. A shorthand field's *value* is the
                // same-named value binding, whose spelling `py_value_ident`
                // escapes identically.
                if let Some(sp) = spread {
                    // Spread: create dict, update, then construct
                    self.buf.push_str(&format!("{type_name}(**{{**vars("));
                    self.emit_expr(sp)?;
                    self.buf.push_str("), ");
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        let _ = write!(self.buf, "\"{}\": ", py_field_ident(&f.name.name));
                        if let Some(val) = &f.value {
                            self.emit_expr(val)?;
                        } else {
                            self.buf.push_str(&py_value_ident(&f.name.name));
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
                        let _ = write!(self.buf, "{}=", py_field_ident(&f.name.name));
                        if let Some(val) = &f.value {
                            self.emit_expr(val)?;
                        } else {
                            self.buf.push_str(&py_value_ident(&f.name.name));
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
                            // A `Bool`-typed part must print the canonical
                            // lowercase `true`/`false` (§3.5); a bare `f"{b}"`
                            // would print Python's `True`/`False`. The checker
                            // stamps such parts (`is_bool_stringify`); map them
                            // through a lowercasing conditional expression.
                            if crate::generator::is_bool_stringify(expr) {
                                self.buf.push_str("'true' if (");
                                self.emit_expr(expr)?;
                                self.buf.push_str(") else 'false'");
                            } else {
                                self.emit_expr(expr)?;
                            }
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
                // Blocks in expression position. Python `lambda` bodies are
                // expression-only, so leading statements can't live inside an
                // IIFE the way they do in JS. When every leading statement is a
                // pure-expressible `let`/expression statement we fold them into
                // immediately-applied lambdas (`try_emit_block_stmts_as_expr`)
                // so their effects run and their bindings reach the tail.
                if stmts.is_empty() {
                    if let Some(t) = tail {
                        return self.emit_expr(t);
                    }
                } else if self.try_emit_block_stmts_as_expr(stmts, tail.as_deref())? {
                    return Ok(());
                }
                // Fallback for shapes the fold can't model (mutable `let`,
                // assignment, loops): wrap the tail alone (best effort).
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
            // A range pattern (`1..10 => …`) has no Python `case` literal form:
            // lower it to a capture-plus-guard `case __rv if lo <= __rv < hi:`.
            // The capture binds the whole scrutinee so the relational test can run
            // (Python `match`/`case` cannot reference the scrutinee name inside a
            // `case`). A user guard, if any, is AND-ed onto the range test.
            if let NodeKind::RangePat { lo, hi, inclusive } = &pattern.kind {
                let lo_s = range_bound_to_py(lo);
                let hi_s = range_bound_to_py(hi);
                let upper = if *inclusive { "<=" } else { "<" };
                let _ = write!(self.buf, "__rv if {lo_s} <= __rv {upper} {hi_s}");
                if let Some(g) = guard {
                    self.buf.push_str(" and (");
                    self.emit_expr(g)?;
                    self.buf.push(')');
                }
            } else {
                self.emit_pattern(pattern)?;
                if let Some(g) = guard {
                    self.buf.push_str(" if ");
                    self.emit_expr(g)?;
                }
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
                    // `py_field_ident`: a keyword-named field (`pass`)
                    // destructures through its escaped spelling
                    // (`case Tally(pass_=p)`), matching the escaped dataclass
                    // field declaration.
                    let field_name = py_field_ident(&f.name.name);
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
                        // multi-field variants). For a keyword-named field both
                        // sides escape identically (`pass_=pass_`): the bound
                        // value identifier's references go through
                        // `py_value_ident`, which produces the same spelling.
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
            // Python `match`/`case` sequence patterns: `case []:`,
            // `case [only]:`, `case [first, *rest]:`. Without this, every list
            // pattern fell through to the `_` catch-all below, so `[]` and
            // `[first, ..rest]` both became `case _:` — the first shadowing the
            // rest ("wildcard makes remaining patterns unreachable"). The `*rest`
            // star-capture mirrors Bock's `..rest`; a `..` with no binding maps to
            // an anonymous `*_`.
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
                    match &r.kind {
                        NodeKind::BindPat { name, .. } => {
                            let _ = write!(self.buf, "*{}", py_value_ident(&name.name));
                        }
                        // `..` rest with no binding (or a wildcard) captures and
                        // discards the tail.
                        _ => self.buf.push_str("*_"),
                    }
                }
                self.buf.push(']');
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
            NodeKind::ListPat { elems, rest } => {
                // `[a, b]` requires a list of exactly len(elems); `[a, ..rest]`
                // requires at least len(elems). Element literal sub-patterns add
                // positional `__v[i] == <lit>` tests; bind/wildcard elements add
                // none. Mirrors the js/ts/go list test.
                let n = elems.len();
                let len_test = if rest.is_some() {
                    format!("isinstance(__v, list) and len(__v) >= {n}")
                } else {
                    format!("isinstance(__v, list) and len(__v) == {n}")
                };
                self.buf.push_str(&len_test);
                for (i, e) in elems.iter().enumerate() {
                    if let NodeKind::LiteralPat { .. } = &e.kind {
                        self.buf.push_str(&format!(" and __v[{i}] == "));
                        self.emit_pattern(e)?;
                    }
                }
            }
            NodeKind::RangePat { lo, hi, inclusive } => {
                // `lo..hi` → `lo <= __v < hi`; `lo..=hi` → `lo <= __v <= hi`.
                let lo_s = range_bound_to_py(lo);
                let hi_s = range_bound_to_py(hi);
                let upper = if *inclusive { "<=" } else { "<" };
                let _ = write!(self.buf, "{lo_s} <= __v {upper} {hi_s}");
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
            // A list pattern binds its elements positionally (`__v[i]`) and a
            // `..rest` to the tail slice (`__v[n:]`) via an applied lambda, so the
            // names resolve inside the conditional. Wildcard/literal elements bind
            // nothing. Without this the `first`/`rest` in `[first, ..rest] => …`
            // were undefined (a `NameError` at runtime).
            NodeKind::ListPat { elems, rest } => {
                let mut params: Vec<String> = Vec::new();
                let mut argvals: Vec<String> = Vec::new();
                for (i, e) in elems.iter().enumerate() {
                    if let NodeKind::BindPat { name, .. } = &e.kind {
                        params.push(py_value_ident(&name.name));
                        argvals.push(format!("__v[{i}]"));
                    }
                }
                if let Some(r) = rest {
                    if let NodeKind::BindPat { name, .. } = &r.kind {
                        params.push(py_value_ident(&name.name));
                        argvals.push(format!("__v[{}:]", elems.len()));
                    }
                }
                if params.is_empty() {
                    return self.emit_block_as_expr(body);
                }
                let _ = write!(self.buf, "(lambda {}: ", params.join(", "));
                self.emit_block_as_expr(body)?;
                let _ = write!(self.buf, ")({})", argvals.join(", "));
                Ok(())
            }
            _ => self.emit_block_as_expr(body),
        }
    }

    // ── Pipe operator ───────────────────────────────────────────────────────

    /// Emit an expression in callee position, parenthesizing it when its
    /// surface syntax would otherwise swallow the trailing argument list.
    ///
    /// A bare Python `lambda` is the case that matters: `lambda x: body`
    /// followed by `(arg)` parses as `lambda x: (body(arg))` — the call binds
    /// to the body, never invoking the lambda. Wrapping it as `(lambda x:
    /// body)(arg)` makes the call apply to the lambda itself. This shows up
    /// whenever the AIR compose desugar (`f >> g` → `(__compose_x) =>
    /// g(f(__compose_x))`) nests: chained `>>` lowers the inner compose to a
    /// `Lambda`, which then appears as the callee `f` inside `f(__compose_x)`.
    fn emit_callee(&mut self, callee: &AIRNode) -> Result<(), CodegenError> {
        if matches!(callee.kind, NodeKind::Lambda { .. }) {
            self.buf.push('(');
            self.emit_expr(callee)?;
            self.buf.push(')');
            Ok(())
        } else {
            self.emit_expr(callee)
        }
    }

    fn emit_pipe(&mut self, left: &AIRNode, right: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::Call { callee, args, .. } = &right.kind {
            let has_placeholder = args
                .iter()
                .any(|a| matches!(a.value.kind, NodeKind::Placeholder));
            if has_placeholder {
                self.emit_callee(callee)?;
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
        self.emit_callee(right)?;
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

    /// Push a fresh lexical-block frame for nested-block `let`-shadow renaming,
    /// seeded with any names queued in [`Self::pending_scope_seed`] (a function's
    /// parameters, so they share the body frame). The seed is drained.
    fn enter_shadow_scope(&mut self) {
        let mut frame = ShadowScope::default();
        for n in self.pending_scope_seed.drain(..) {
            frame.bound.insert(n);
        }
        self.shadow_scopes.push(frame);
    }

    /// Pop the innermost lexical-block frame pushed by [`Self::enter_shadow_scope`].
    fn leave_shadow_scope(&mut self) {
        self.shadow_scopes.pop();
    }

    /// Whether `py_name` is bound in any *enclosing* frame (every frame except
    /// the innermost), i.e. binding it again in the current block shadows an
    /// outer binding and — on Python's function-scoped `=` — would stomp it.
    fn shadowed_in_outer_scope(&self, py_name: &str) -> bool {
        let n = self.shadow_scopes.len();
        if n < 2 {
            return false;
        }
        self.shadow_scopes[..n - 1]
            .iter()
            .any(|s| s.bound.contains(py_name) || s.renames.contains_key(py_name))
    }

    /// Resolve a Python identifier through the shadow-scope stack: the innermost
    /// frame that renamed `py_name` wins, else the name is unchanged. Used at
    /// every identifier *use* site so a reference inside a shadowing block reads
    /// the alias while code outside reads the original.
    fn resolve_shadow_name(&self, py_name: &str) -> String {
        for s in self.shadow_scopes.iter().rev() {
            if let Some(alias) = s.renames.get(py_name) {
                return alias.clone();
            }
            // A frame that *binds* the name directly (without a rename) stops the
            // search: an inner block's same-name binding is the live one there.
            if s.bound.contains(py_name) {
                return py_name.to_string();
            }
        }
        py_name.to_string()
    }

    /// Plan a `let`-binding's LHS Python name without yet activating it in the
    /// scope frame. A `let`'s RHS reads the *prior* binding (`let y = y + 10`
    /// reads the outer `y`), so the rename must take effect only *after* the RHS
    /// is emitted — [`Self::commit_shadow_let`] does that. Returns the LHS name to
    /// emit (a fresh alias when the binding shadows an enclosing frame, else the
    /// name unchanged) paired with the rename to commit (`Some((orig, alias))`
    /// only for a fresh shadow; `None` for a same-block rebind or first binding).
    fn plan_shadow_let(&mut self, py_name: &str) -> (String, Option<(String, String)>) {
        // No active frame (defensive) → emit verbatim, nothing to commit.
        if self.shadow_scopes.is_empty() {
            return (py_name.to_string(), None);
        }
        // Same-block re-bind: an existing alias or direct binding here is reused
        // (a plain Python rebind), with nothing new to commit.
        if let Some(cur) = self.shadow_scopes.last() {
            if let Some(alias) = cur.renames.get(py_name) {
                return (alias.clone(), None);
            }
            if cur.bound.contains(py_name) {
                return (py_name.to_string(), None);
            }
        }
        if self.shadowed_in_outer_scope(py_name) {
            self.shadow_counter += 1;
            let alias = format!("{py_name}__s{}", self.shadow_counter);
            return (alias.clone(), Some((py_name.to_string(), alias)));
        }
        // A `let` whose name collides with a Python builtin the codegen *itself*
        // emits as a call (`list(map.keys())`, `set(...)`, `map(...)`) must be
        // renamed even when it shadows nothing — binding `list = [...]` rebinds
        // the global `list` for the rest of the function scope, so a later
        // codegen-emitted `list(...)` would raise `TypeError: 'list' object is
        // not callable`. The rename flows to references via the same
        // `renames` map `resolve_shadow_name` consults, so use sites stay in sync.
        if is_shadow_sensitive_py_builtin(py_name) {
            self.shadow_counter += 1;
            let alias = format!("{py_name}__b{}", self.shadow_counter);
            return (alias.clone(), Some((py_name.to_string(), alias)));
        }
        (py_name.to_string(), None)
    }

    /// Activate a planned `let` binding in the current frame, after its RHS has
    /// been emitted. `pending` is the rename returned by [`Self::plan_shadow_let`]
    /// (`Some` only for a fresh shadow); `py_name` is the original name (used to
    /// record a non-shadowing first binding so later same-block rebinds and
    /// nested shadows resolve correctly).
    fn commit_shadow_let(&mut self, py_name: &str, pending: Option<(String, String)>) {
        let Some(cur) = self.shadow_scopes.last_mut() else {
            return;
        };
        if let Some((orig, alias)) = pending {
            cur.renames.insert(orig, alias.clone());
            cur.bound.insert(alias);
        } else {
            cur.bound.insert(py_name.to_string());
        }
    }

    /// Emit a **function/method body**, wrapping it in the `?`-propagate
    /// envelope when the body contains a `?` operator. `expr?` lowers to
    /// `_bock_try(expr)`, which raises `_BockPropagate` on an `Err`/`None`; the
    /// envelope catches that and re-returns the carried value, giving Rust-`?`
    /// early-return semantics:
    ///
    /// ```python
    /// try:
    ///     <body>
    /// except _BockPropagate as __bock_p:
    ///     return __bock_p.value
    /// ```
    ///
    /// A body with no `?` is emitted unchanged (no envelope, no runtime cost).
    fn emit_fn_body(&mut self, body: &AIRNode) -> Result<(), CodegenError> {
        if body_contains_propagate(body) {
            self.writeln("try:");
            self.indent += 1;
            self.emit_block_body(body)?;
            self.indent -= 1;
            self.writeln("except _BockPropagate as __bock_p:");
            self.indent += 1;
            self.writeln("return __bock_p.value");
            self.indent -= 1;
            Ok(())
        } else {
            self.emit_block_body(body)
        }
    }

    /// Emit a block (or bare-body) in statement/`return` position, opening a
    /// fresh shadow-scope frame so a nested-block `let` that shadows an enclosing
    /// binding is renamed rather than stomping it (see [`ShadowScope`]).
    fn emit_block_body(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.enter_shadow_scope();
        let r = self.emit_block_body_inner(node);
        self.leave_shadow_scope();
        r
    }

    /// Emit the **body of a `for`/`while`/`loop`** — statement position, so its
    /// tail expression is *discarded* (a Bock loop evaluates to Unit). Sets
    /// [`Self::in_loop_body_tail`] for the body's duration so
    /// [`Self::emit_block_body_inner`] emits the tail as a bare expression
    /// statement (`<value>`) instead of a function-body `return <value>` — a
    /// `return` inside a loop aborts the enclosing function after one iteration
    /// (the fizzbuzz / inventory-system one-line truncation). The flag is
    /// saved/restored so it never leaks past the loop, and any nested value
    /// context (a value-binding hoist, a value-`if`/`match` arm) clears it — see
    /// [`Self::emit_block_body_inner`]. A `break v` value flows through the
    /// separate `loop_value_targets` stack, not this flag.
    fn emit_loop_body(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        let prev = std::mem::replace(&mut self.in_loop_body_tail, true);
        let r = self.emit_block_body(node);
        self.in_loop_body_tail = prev;
        r
    }

    fn emit_block_body_inner(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
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
                // A bare `loop`/`while`/`for` in tail position yields no value (it
                // exits only via `break`/`return`, and a value-`loop` is rewritten
                // by `emit_value_binding`, not here). Emit it as a Python loop
                // statement — never `return <loop>`, which `emit_expr` would lower
                // to the `# unsupported` fallthrough, silently discarding the whole
                // loop body (the guessing-game `play()` tail-`loop` shape).
                if matches!(
                    t.kind,
                    NodeKind::Loop { .. } | NodeKind::While { .. } | NodeKind::For { .. }
                ) {
                    self.emit_node(t)?;
                    return Ok(());
                }
                // A diverging `raise` expression (`todo()` / `unreachable()`)
                // yields no value: emit it bare, never `return raise …` (a
                // `SyntaxError`).
                if is_raise_expr(t) {
                    self.write_indent();
                    self.emit_expr(t)?;
                    self.buf.push('\n');
                    return Ok(());
                }
                // A value-`if`/`match` with a diverging `raise` branch can't be a
                // ternary (`return raise … if … else …`). Hoist it to a
                // statement-form `if`/`match` whose branches `return`/`raise`.
                if control_flow_has_raise_branch(t) {
                    return self.emit_tail_control_flow(t);
                }
                // A `match` with statement arms yields no value: emit a Python
                // `match`/`case` statement, not a `return (lambda ...)`. A value
                // match whose arms need the structural if-chain (guards,
                // or/tuple/record/range/list patterns, or a nested constructor)
                // is *also* routed here: the `(lambda __v: …)` conditional chain
                // cannot test or bind those — it dropped guards, tested record /
                // tuple / or arms as `if True`, and emitted the arm body with the
                // pattern binding free (`(lambda __v: f"x={x}")(p)` → `NameError`).
                // The statement-form `emit_pattern` binds and tests every pattern
                // kind correctly (each expression arm body becomes `return <v>`).
                if let NodeKind::Match { scrutinee, arms } = &t.kind {
                    if crate::generator::match_has_statement_arm(arms)
                        || match_value_needs_stmt_form(arms)
                    {
                        self.emit_match(scrutinee, arms)?;
                        return Ok(());
                    }
                }
                // A value-position `if` whose branch block carries statements (a
                // `let` binding) can't be a ternary: the ternary emits only each
                // branch's tail, dropping the `let` (a later reference then
                // `NameError`s — the microservice `handle_delete_user` case). Emit
                // it as statement-form `if`/`elif`/`else`, each branch recursing
                // through `emit_block_body` so the binding is kept and the tail
                // `return`ed.
                if if_value_needs_stmt_form(t) {
                    return self.emit_tail_control_flow(t);
                }
                // Plain value-expression tail. In a loop body this is statement
                // position — the value is discarded — so emit a bare expression
                // statement, not `return <value>` (a `return` in a loop aborts
                // the function after one iteration: the fizzbuzz / inventory
                // truncation). Elsewhere this is the function-body tail: `return`.
                self.emit_tail_value_or_discard(t)?;
            }
        } else if crate::generator::node_is_statement(node) {
            // A bare statement body (`break`/`continue`/`return`/assignment).
            self.emit_node(node)?;
        } else if matches!(
            node.kind,
            NodeKind::Loop { .. } | NodeKind::While { .. } | NodeKind::For { .. }
        ) {
            // A bare `loop`/`while`/`for` body yields no value — emit the loop
            // statement, never `return <loop>` (see the tail-position note above).
            self.emit_node(node)?;
        } else if is_raise_expr(node) {
            self.write_indent();
            self.emit_expr(node)?;
            self.buf.push('\n');
        } else if control_flow_has_raise_branch(node) {
            return self.emit_tail_control_flow(node);
        } else if let NodeKind::Match { scrutinee, arms } = &node.kind {
            // See the tail-position note above: a value match needing the
            // structural if-chain (guards, or/tuple/record/range/list, nested
            // constructor) is lowered to statement-form `match`/`case` so every
            // pattern binds and tests correctly, instead of a `(lambda __v: …)`
            // chain that drops guards and leaves pattern bindings free.
            if crate::generator::match_has_statement_arm(arms) || match_value_needs_stmt_form(arms)
            {
                self.emit_match(scrutinee, arms)?;
            } else {
                self.emit_tail_value_or_discard(node)?;
            }
        } else if if_value_needs_stmt_form(node) {
            // See the tail-position note above: a value `if` whose branch block
            // carries a `let` can't be a ternary (the binding would be dropped) —
            // emit statement-form `if`/`elif`/`else`.
            return self.emit_tail_control_flow(node);
        } else {
            // Single expression as body.
            self.emit_tail_value_or_discard(node)?;
        }
        Ok(())
    }

    /// Emit a block/body **tail value expression** in the correct position.
    ///
    /// In a function/method body (the default) the tail is the body's value, so
    /// it is `return <value>`. Inside the body of a `for`/`while`/`loop`
    /// ([`Self::in_loop_body_tail`] set by [`Self::emit_loop_body`]) the tail is
    /// *statement* position — a Bock loop evaluates to Unit, so the value is
    /// discarded — and a `return` would abort the enclosing function after the
    /// first iteration (the fizzbuzz one-line / inventory single-product
    /// truncation). There it is emitted as a bare expression statement. The
    /// arm/branch body of a statement-position `match` or `if`/`else`
    /// ([`Self::in_stmt_construct_arm`], set by [`Self::emit_stmt`]'s `Match`
    /// and `If` arms) is discarded for the same reason — a mid-block
    /// `match`/`if` is a Unit statement, and a `return` there aborts the
    /// enclosing function after the matched arm / taken branch (the
    /// chat-protocol truncation and its if/else sibling,
    /// Q-python-ifelse-truncation).
    fn emit_tail_value_or_discard(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        let ind = self.indent_str();
        if self.in_loop_body_tail || self.in_stmt_construct_arm {
            self.buf.push_str(&ind);
        } else {
            let _ = write!(self.buf, "{ind}return ");
        }
        self.emit_expr(node)?;
        self.buf.push('\n');
        Ok(())
    }

    /// Emit a value-position `if`/`match` that carries a diverging `raise`
    /// branch (`todo()` / `unreachable()`) in **tail/return** position as
    /// statement-form Python: each branch/arm recurses through
    /// [`Self::emit_block_body`], so a non-diverging branch `return`s its value
    /// while the diverging branch `raise`s. This replaces the invalid ternary
    /// (`return raise … if … else …`).
    fn emit_tail_control_flow(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}if ");
                self.emit_expr(condition)?;
                self.buf.push_str(":\n");
                self.indent += 1;
                self.emit_block_body(then_block)?;
                self.indent -= 1;
                if let Some(eb) = else_block {
                    if matches!(eb.kind, NodeKind::If { .. }) {
                        let ind = self.indent_str();
                        let _ = write!(self.buf, "{ind}el");
                        // Chain `elif` by re-emitting the nested `if` inline.
                        return self.emit_tail_control_flow_inline(eb);
                    }
                    self.writeln("else:");
                    self.indent += 1;
                    self.emit_block_body(eb)?;
                    self.indent -= 1;
                }
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => self.emit_match(scrutinee, arms),
            // Not a control-flow node — fall back to a plain tail value (or a
            // bare statement inside a loop body; see `emit_tail_value_or_discard`).
            _ => self.emit_tail_value_or_discard(node),
        }
    }

    /// `elif`-chaining tail for [`Self::emit_tail_control_flow`]: the caller has
    /// already written the `el` prefix, so emit `if <cond>: … (elif/else)`.
    fn emit_tail_control_flow_inline(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        let NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } = &node.kind
        else {
            return self.emit_tail_control_flow(node);
        };
        self.buf.push_str("if ");
        self.emit_expr(condition)?;
        self.buf.push_str(":\n");
        self.indent += 1;
        self.emit_block_body(then_block)?;
        self.indent -= 1;
        if let Some(eb) = else_block {
            if matches!(eb.kind, NodeKind::If { .. }) {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}el");
                return self.emit_tail_control_flow_inline(eb);
            }
            self.writeln("else:");
            self.indent += 1;
            self.emit_block_body(eb)?;
            self.indent -= 1;
        }
        Ok(())
    }

    /// Emit a block (or bare body) in statement position, **assigning** its
    /// value to `target` instead of `return`ing it — the value-producing twin of
    /// [`Self::emit_block_body`]. Used by [`Self::emit_value_binding`] to hoist
    /// an expression-position control-flow construct into Python statements.
    ///
    /// A *diverging* body (one ending in `return`/`break`/`continue`, or a
    /// statement-only block) is emitted as-is with **no** assignment: control
    /// leaves the construct before the binding is read, exactly as in Bock where
    /// such an arm has type `Never` and unifies with the binding's type.
    fn emit_block_body_assigning(
        &mut self,
        target: &str,
        node: &AIRNode,
    ) -> Result<(), CodegenError> {
        // A value-binding RHS is a *value* context: its tail is assigned to
        // `target`, never discarded. Clear any active discard flag (a loop-body
        // tail set by an enclosing `emit_loop_body`, or a statement-`match`/`if`
        // arm set by `emit_stmt`) so it doesn't leak into this value position; a
        // nested loop/statement-construct inside the RHS re-sets the relevant
        // flag. Restored after.
        let prev_discard = std::mem::replace(&mut self.in_loop_body_tail, false);
        let prev_match = std::mem::replace(&mut self.in_stmt_construct_arm, false);
        self.enter_shadow_scope();
        let r = self.emit_block_body_assigning_inner(target, node);
        self.leave_shadow_scope();
        self.in_loop_body_tail = prev_discard;
        self.in_stmt_construct_arm = prev_match;
        r
    }

    fn emit_block_body_assigning_inner(
        &mut self,
        target: &str,
        node: &AIRNode,
    ) -> Result<(), CodegenError> {
        if let NodeKind::Block { stmts, tail } = &node.kind {
            let task_bindings = Self::collect_task_bindings(stmts);
            let prev = std::mem::replace(&mut self.task_bound_names, task_bindings);
            for s in stmts {
                self.emit_node(s)?;
            }
            self.task_bound_names = prev;
            match tail {
                None => {
                    // No tail value. If the block had no statements either, keep
                    // the suite non-empty.
                    if stmts.is_empty() {
                        self.writeln("pass");
                    }
                }
                Some(t) if crate::generator::node_is_statement(t) => {
                    // Diverging / statement tail: emit as a statement (it leaves
                    // the construct; nothing is assigned).
                    self.emit_node(t)?;
                }
                Some(t) => self.emit_value_assign(target, t)?,
            }
        } else if crate::generator::node_is_statement(node) {
            self.emit_node(node)?;
        } else {
            self.emit_value_assign(target, node)?;
        }
        Ok(())
    }

    /// Emit `<target> = <expr>` for a value expression, recursing through nested
    /// control-flow that itself needs statement form (so an arm whose value is
    /// another `match`/`loop`/`if`-statement assigns the same target).
    fn emit_value_assign(&mut self, target: &str, expr: &AIRNode) -> Result<(), CodegenError> {
        if value_needs_stmt_form(expr) {
            return self.emit_value_binding(target, expr);
        }
        // A diverging `raise` (`todo()`/`unreachable()`) cannot be assigned;
        // emit it bare (the assignment target is never reached).
        if is_raise_expr(expr) {
            self.write_indent();
            self.emit_expr(expr)?;
            self.buf.push('\n');
            return Ok(());
        }
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}{target} = ");
        self.emit_expr(expr)?;
        self.buf.push('\n');
        Ok(())
    }

    /// Lower an **expression-position control-flow** value (`match` with
    /// statement/diverging arms, a value-`loop`, or a statement-form `if`) bound
    /// to `target` into Python statements that assign `target`.
    ///
    /// Python has no statement-admitting expression form, so
    /// `let r = loop { … break v }` / `let l = match n { _ => { return … } }`
    /// cannot be emitted as a ternary/IIFE. Callers ([`Self::emit_stmt`]'s
    /// `LetBinding` arm) first declare `target = None` so it is always bound,
    /// then call this to fill it in via real `if`/`while`/`match` statements.
    fn emit_value_binding(&mut self, target: &str, value: &AIRNode) -> Result<(), CodegenError> {
        match &value.kind {
            NodeKind::Block { .. } => self.emit_block_body_assigning(target, value),
            NodeKind::Match { scrutinee, arms } => {
                self.emit_match_assigning(target, scrutinee, arms)
            }
            NodeKind::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}if ");
                self.emit_expr(condition)?;
                self.buf.push_str(":\n");
                self.indent += 1;
                self.emit_block_body_assigning(target, then_block)?;
                self.indent -= 1;
                if let Some(eb) = else_block {
                    if matches!(eb.kind, NodeKind::If { .. }) {
                        let ind = self.indent_str();
                        let _ = write!(self.buf, "{ind}el");
                        // Re-enter via `emit_value_binding` to chain `elif`.
                        self.emit_value_binding_if_chain(target, eb)?;
                    } else {
                        self.writeln("else:");
                        self.indent += 1;
                        self.emit_block_body_assigning(target, eb)?;
                        self.indent -= 1;
                    }
                }
                Ok(())
            }
            NodeKind::Loop { body } => {
                self.writeln("while True:");
                self.indent += 1;
                self.loop_value_targets.push(Some(target.to_string()));
                // The loop's value arrives via `break v` (recorded in
                // `loop_value_targets`), not the body's tail; the body is
                // statement position, so a tail expr is discarded.
                self.emit_loop_body(body)?;
                self.loop_value_targets.pop();
                self.indent -= 1;
                Ok(())
            }
            NodeKind::While { condition, body } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}while ");
                self.emit_expr(condition)?;
                self.buf.push_str(":\n");
                self.indent += 1;
                self.loop_value_targets.push(Some(target.to_string()));
                // The loop's value arrives via `break v` (recorded in
                // `loop_value_targets`), not the body's tail; the body is
                // statement position, so a tail expr is discarded.
                self.emit_loop_body(body)?;
                self.loop_value_targets.pop();
                self.indent -= 1;
                Ok(())
            }
            // Not a hoisted construct: plain assignment.
            _ => self.emit_value_assign(target, value),
        }
    }

    /// Helper for [`Self::emit_value_binding`]'s `elif` chaining: emit the
    /// keyword tail of an `if` (`if <cond>: … elif …`) for an `else if`. The
    /// caller has already written the `el` prefix.
    fn emit_value_binding_if_chain(
        &mut self,
        target: &str,
        node: &AIRNode,
    ) -> Result<(), CodegenError> {
        let NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } = &node.kind
        else {
            return self.emit_value_binding(target, node);
        };
        self.buf.push_str("if ");
        self.emit_expr(condition)?;
        self.buf.push_str(":\n");
        self.indent += 1;
        self.emit_block_body_assigning(target, then_block)?;
        self.indent -= 1;
        if let Some(eb) = else_block {
            if matches!(eb.kind, NodeKind::If { .. }) {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}el");
                self.emit_value_binding_if_chain(target, eb)?;
            } else {
                self.writeln("else:");
                self.indent += 1;
                self.emit_block_body_assigning(target, eb)?;
                self.indent -= 1;
            }
        }
        Ok(())
    }

    /// Emit a `match` (statement form) whose arms **assign** `target` rather than
    /// `return`. Mirrors [`Self::emit_match`] but uses
    /// [`Self::emit_block_body_assigning`] for arm bodies.
    fn emit_match_assigning(
        &mut self,
        target: &str,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}match ");
        self.emit_expr(scrutinee)?;
        self.buf.push_str(":\n");
        self.indent += 1;
        for arm in arms {
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
                self.emit_block_body_assigning(target, body)?;
                self.indent -= 1;
            }
        }
        self.indent -= 1;
        Ok(())
    }

    fn emit_block_as_expr(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::Block { stmts, tail } = &node.kind {
            if stmts.is_empty() {
                if let Some(t) = tail {
                    return self.emit_expr(t);
                }
            } else if self.try_emit_block_stmts_as_expr(stmts, tail.as_deref())? {
                // A block with leading statements *and* a tail value — e.g. a
                // value-position `match` arm `Ok(sum) => { let s = …; … }`. The
                // leading `let`s / side-effecting expression statements are
                // folded into immediately-applied lambdas so they actually run
                // and their bindings are in scope for the tail. See
                // `try_emit_block_stmts_as_expr`.
                return Ok(());
            }
        }
        self.emit_expr(node)
    }

    /// Emit a block's leading statements + tail as a single Python expression by
    /// folding each statement into an immediately-applied lambda, preserving
    /// both the statement's effect and any binding it introduces:
    ///
    /// ```text
    /// { let x = V; REST }         →  (lambda x: <REST>)(V)
    /// { side_effect(); REST }     →  (lambda _: <REST>)(side_effect())
    /// ```
    ///
    /// Python `lambda` bodies are expression-only, so a block with statements in
    /// expression position (a value-position `match`/`if` arm) otherwise loses
    /// its leading statements — the old "best effort" emitted `(lambda: <tail>)()`
    /// and dropped them, leaving later references unbound (the calculator
    /// `let step2 = …` bug) or skipping a side effect (microservice's dropped
    /// `log`).
    ///
    /// Returns `Ok(true)` when the whole block was emitted this way. Returns
    /// `Ok(false)` — emitting nothing — when a leading statement can't be
    /// expressed as a pure expression (mutable `let`, assignment, loop, …); the
    /// caller then falls back to the prior best-effort path. The conservative
    /// gate keeps this from emitting broken code for shapes it can't model.
    fn try_emit_block_stmts_as_expr(
        &mut self,
        stmts: &[AIRNode],
        tail: Option<&AIRNode>,
    ) -> Result<bool, CodegenError> {
        // Every leading statement must be expressible as a value-producing
        // expression: an immutable simple `let`, or a plain expression
        // statement. Anything else (mutable/destructuring `let`, assignment,
        // loop, `return`, nested block) bails so the caller can fall back.
        for s in stmts {
            match &s.kind {
                NodeKind::LetBinding {
                    is_mut, pattern, ..
                } if *is_mut || Self::simple_bind_name(pattern).is_none() => {
                    return Ok(false);
                }
                NodeKind::LetBinding { .. } => {}
                NodeKind::Assign { .. }
                | NodeKind::While { .. }
                | NodeKind::For { .. }
                | NodeKind::Loop { .. }
                | NodeKind::Return { .. }
                | NodeKind::Break { .. }
                | NodeKind::Continue
                | NodeKind::Block { .. } => return Ok(false),
                _ => {}
            }
        }
        self.emit_block_stmt_chain(stmts, tail)?;
        Ok(true)
    }

    /// Recursively emit the `(lambda …: …)(…)` chain validated by
    /// [`Self::try_emit_block_stmts_as_expr`].
    fn emit_block_stmt_chain(
        &mut self,
        stmts: &[AIRNode],
        tail: Option<&AIRNode>,
    ) -> Result<(), CodegenError> {
        let Some((first, rest)) = stmts.split_first() else {
            // No more leading statements — emit the tail value (or `None`).
            return match tail {
                Some(t) => self.emit_expr(t),
                None => {
                    self.buf.push_str("None");
                    Ok(())
                }
            };
        };
        match &first.kind {
            NodeKind::LetBinding { pattern, value, .. } => {
                let name = Self::simple_bind_name(pattern).unwrap_or_else(|| "_".to_string());
                let _ = write!(self.buf, "(lambda {name}: ");
                self.emit_block_stmt_chain(rest, tail)?;
                self.buf.push_str(")(");
                self.emit_expr(value)?;
                self.buf.push(')');
                Ok(())
            }
            // A bare expression statement: bind its value to a throwaway
            // parameter so the effect runs, then continue with the rest.
            _ => {
                self.buf.push_str("(lambda _: ");
                self.emit_block_stmt_chain(rest, tail)?;
                self.buf.push_str(")(");
                self.emit_expr(first)?;
                self.buf.push(')');
                Ok(())
            }
        }
    }

    /// The single Python value-name a *simple* `let` pattern binds, if any: only
    /// a `BindPat` qualifies (tuple/record patterns bind several names or use
    /// Python-side destructuring that shadow-renaming does not rewrite). Used to
    /// gate nested-block `let`-shadow renaming to the simple, common case.
    fn simple_bind_name(pat: &AIRNode) -> Option<String> {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => Some(py_value_ident(&name.name)),
            _ => None,
        }
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
                // Python doesn't have destructuring; use first field name or
                // underscore (keyword-escaped like every value binding).
                fields
                    .first()
                    .map(|f| py_value_ident(&f.name.name))
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

/// Compute a stable emission order over a module's top-level `items` such that a
/// type declaration (`trait` / `record` / `class`) that becomes a Python base
/// class of another (its supertype) is always emitted *before* the subtype.
///
/// Python evaluates a `class Sub(Base):` statement's base list eagerly, so
/// `Base` must already be a bound name when `Sub` is defined; emitting them in
/// source order risks `NameError` when a trait is declared after the type that
/// impls it, or when an inlined-impl record precedes its trait
/// (Q-py-impl-before-trait).
///
/// The reorder is a **stable topological sort**: items are emitted in original
/// order except that any type decl is delayed until all the type decls it
/// depends on (its declared `base`, declared `traits`, and the trait paths of
/// the impl blocks targeting it in `impls_by_target`) have been emitted. Only
/// dependencies on types *declared in this same module* create edges; references
/// to imported/prelude bases never block (they resolve via imports). A
/// dependency cycle (which Python could not represent anyway) degrades
/// gracefully to source order for the involved nodes rather than dropping them.
fn type_decl_emission_order(
    items: &[AIRNode],
    impls_by_target: &HashMap<String, Vec<AIRNode>>,
) -> Vec<usize> {
    use std::collections::HashMap as Map;

    // name → index for every type decl declared in this module. Effects are
    // included because the Python backend also emits an `effect` as an ABC class
    // that an `impl Effect for T` makes a *base* of `T` (`class StubChannel(
    // Channel)`), so an effect declared after its impl is the same base-ordering
    // hazard as a trait.
    let mut decl_index: Map<String, usize> = Map::new();
    for (i, item) in items.iter().enumerate() {
        match &item.kind {
            NodeKind::TraitDecl { name, .. }
            | NodeKind::RecordDecl { name, .. }
            | NodeKind::ClassDecl { name, .. }
            | NodeKind::EffectDecl { name, .. } => {
                decl_index.entry(name.name.clone()).or_insert(i);
            }
            _ => {}
        }
    }

    // deps[i] = set of item indices that item i must follow (its in-module
    // base/trait supertypes). Non-type items have no deps.
    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    let add_dep = |deps: &mut Vec<Vec<usize>>, i: usize, dep_name: &str| {
        if let Some(&j) = decl_index.get(dep_name) {
            if j != i && !deps[i].contains(&j) {
                deps[i].push(j);
            }
        }
    };
    for (i, item) in items.iter().enumerate() {
        let (name, declared_base, declared_traits) = match &item.kind {
            NodeKind::ClassDecl {
                name, base, traits, ..
            } => (Some(name), base.as_ref(), traits.as_slice()),
            NodeKind::RecordDecl { name, .. } | NodeKind::TraitDecl { name, .. } => {
                (Some(name), None, [].as_slice())
            }
            _ => (None, None, [].as_slice()),
        };
        let Some(name) = name else { continue };
        if let Some(b) = declared_base {
            if let Some(seg) = b.segments.last() {
                add_dep(&mut deps, i, &seg.name);
            }
        }
        for tp in declared_traits {
            if let Some(seg) = tp.segments.last() {
                add_dep(&mut deps, i, &seg.name);
            }
        }
        // Trait paths from the impl blocks that fold into this type's body.
        if let Some(impls) = impls_by_target.get(&name.name) {
            for im in impls {
                if let NodeKind::ImplBlock {
                    trait_path: Some(tp),
                    ..
                } = &im.kind
                {
                    if let Some(seg) = tp.segments.last() {
                        add_dep(&mut deps, i, &seg.name);
                    }
                }
            }
        }
    }

    // Stable topological emit: repeatedly take the earliest not-yet-emitted item
    // whose deps are all emitted. If none qualifies (a cycle), take the earliest
    // remaining item to make progress (graceful degradation).
    let n = items.len();
    let mut emitted = vec![false; n];
    let mut order = Vec::with_capacity(n);
    for _ in 0..n {
        let mut pick = None;
        for i in 0..n {
            if emitted[i] {
                continue;
            }
            if deps[i].iter().all(|&d| emitted[d]) {
                pick = Some(i);
                break;
            }
        }
        let i = pick.unwrap_or_else(|| (0..n).find(|&i| !emitted[i]).unwrap_or(0));
        emitted[i] = true;
        order.push(i);
    }
    order
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

/// The Python spelling of a record / class / enum-struct-variant **field**
/// name: `snake_case`, then escaped against the Python keyword set — the same
/// policy as value identifiers ([`py_value_ident`]), extended to field
/// position (Q-python-keyword-record-fields). A Bock field named `pass` emits
/// as `pass_`; left verbatim it is a `SyntaxError` in the dataclass
/// declaration (`pass: int`), the constructor keyword args (`Tally(pass=7)`),
/// the attribute access (`t.pass`), and the match pattern
/// (`case Tally(pass=p)`). Applied identically at every field position —
/// dataclass / `__init__` declaration, constructor kwargs (plain and spread
/// dict keys), field access, and record-pattern destructuring — so the escaped
/// spelling always agrees. A record-pattern *shorthand* (`{ pass }`) binds a
/// value identifier of the same Bock name, which [`py_value_ident`] escapes to
/// the identical spelling at every reference site.
fn py_field_ident(name: &str) -> String {
    py_value_ident(name)
}

/// True when `name` (an already snake_cased Python value identifier) collides
/// with a built-in that the Python backend *emits as a call* in generated code —
/// the collection constructors and functional combinators a list/set/map literal
/// or method lowers to (`list(...)`, `set(...)`, `dict(...)`, `map(...)`,
/// `filter(...)`, `sorted(...)`, `enumerate(...)`, `range(...)`, `len(...)`,
/// `tuple(...)`, `frozenset(...)`, `zip(...)`, `iter(...)`, `print(...)`).
///
/// A local `let list = [...]` rebinds these names for the rest of the
/// (function-scoped) Python frame, so a subsequent codegen-emitted `list(...)`
/// would raise `TypeError: 'list' object is not callable`. [`PyEmitCtx::
/// plan_shadow_let`] renames such bindings to a fresh alias so the builtin stays
/// callable; references resolve through the same alias map. The set is limited to
/// builtins the codegen actually emits (not every Python builtin) so unrelated
/// names are never mangled. Bock keywords (`type`, etc.) are handled separately
/// by [`crate::generator::escape_target_keyword`].
fn is_shadow_sensitive_py_builtin(name: &str) -> bool {
    matches!(
        name,
        "list"
            | "set"
            | "dict"
            | "map"
            | "filter"
            | "sorted"
            | "enumerate"
            | "range"
            | "len"
            | "tuple"
            | "frozenset"
            | "zip"
            | "iter"
            | "next"
            | "print"
            | "str"
            | "int"
            | "float"
            | "bool"
            | "abs"
            | "min"
            | "max"
            | "sum"
            | "round"
    )
}

/// Render a `RangePat` bound (`lo`/`hi`) as a Python expression. Range bounds
/// are literals (`1..10`) or a const identifier (`MIN..MAX`); anything else
/// falls back to `0` for an unrecognised node. Mirrors `range_bound_to_js`.
fn range_bound_to_py(node: &AIRNode) -> String {
    let lit = match &node.kind {
        NodeKind::LiteralPat { lit } | NodeKind::Literal { lit } => Some(lit),
        NodeKind::Identifier { name } => return py_value_ident(&name.name),
        _ => None,
    };
    match lit {
        Some(Literal::Int(s)) | Some(Literal::Float(s)) => s.clone(),
        Some(Literal::Bool(b)) => if *b { "True" } else { "False" }.to_string(),
        Some(Literal::Char(s)) => format!("'{s}'"),
        Some(Literal::String(s)) => format!("\"{}\"", escape_py_string(s)),
        Some(Literal::Unit) | None => "0".to_string(),
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

/// Walk a `@test` body and record the Optional/Result runtime tag classes its
/// predicate assertions (`to_be_some`/`to_be_none`/`to_be_ok`/`to_be_err`)
/// reference, so the Python test file imports exactly those from
/// `_bock_runtime` (which only defines the runtimes the program uses).
fn collect_runtime_tag_imports(node: &AIRNode, out: &mut std::collections::BTreeSet<&'static str>) {
    if let Some((assertion, _actual, _expected)) = crate::generator::classify_assertion(node) {
        use crate::generator::TestAssertion as T;
        match assertion {
            T::BeSome => {
                out.insert("_BockSome");
            }
            T::BeNone => {
                out.insert("_BockNone");
            }
            T::BeOk => {
                out.insert("_BockOk");
            }
            T::BeErr => {
                out.insert("_BockErr");
            }
            _ => {}
        }
    }
    if let NodeKind::Block { stmts, tail } = &node.kind {
        for s in stmts {
            collect_runtime_tag_imports(s, out);
        }
        if let Some(t) = tail {
            collect_runtime_tag_imports(t, out);
        }
    }
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
    use bock_air::{AirArg, AirRecordField, AirRecordPatternField};
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

    /// A module node with a declared dotted `path` (e.g. `core.option`), used
    /// by the per-module emission tests where the file layout and import path
    /// are keyed on the declared module-path.
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

    /// An `import <path>.{ name }` AIR node (a `Named` single-item import).
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

    /// A bare `fn <name>() -> <ret? expr>` declaration with the given visibility
    /// and a single tail expression as its body.
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
    fn per_module_emits_native_import_tree() {
        // entry `module main` uses `mathutil.add_one`; `module mathutil` exports
        // a `public fn add_one`. Per-module emission must produce TWO files —
        // `main.py` (with a real `from mathutil import add_one`) and a separate
        // `mathutil.py` — not a single collapsed file.
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "add_one")),
                args: vec![AirArg {
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
        let mathutil_mod = module_with_path(
            &["mathutil"],
            vec![],
            vec![fn_decl_tail(
                20,
                Visibility::Public,
                "add_one",
                int_lit(22, "7"),
            )],
        );

        let gen = PyGenerator::new();
        let main_path = std::path::Path::new("src/main.bock");
        let util_path = std::path::Path::new("src/mathutil.bock");
        let out = gen
            .generate_project(&[(&main_mod, main_path), (&mathutil_mod, util_path)])
            .unwrap();

        // Two module files (no shared runtime needed here).
        let by_name = |p: &str| out.files.iter().find(|f| f.path == std::path::Path::new(p));
        let main_file = by_name("main.py").expect("main.py emitted");
        let util_file = by_name("mathutil.py").expect("mathutil.py emitted");
        assert!(
            main_file.content.contains("from mathutil import add_one"),
            "main.py must import from the sibling module; got:\n{}",
            main_file.content
        );
        assert!(
            main_file.content.contains("if __name__ == \"__main__\":"),
            "main.py must carry the entry invocation; got:\n{}",
            main_file.content
        );
        assert!(
            util_file.content.contains("def add_one():"),
            "mathutil.py must carry the exported fn; got:\n{}",
            util_file.content
        );
        // The bundling no-op import comment must NOT appear (real import only).
        assert!(
            !main_file.content.contains("# import"),
            "per-module path emits a real import, not a comment"
        );
    }

    #[test]
    fn per_module_shares_optional_runtime() {
        // Two modules both referencing `None` must share ONE `_bock_runtime.py`
        // (so `_bock_none` is the same object across files) and import it.
        let main_mod = module_with_path(
            &["main"],
            vec![import_named(5, &["other"], "thing")],
            vec![fn_decl_tail(
                1,
                Visibility::Private,
                "main",
                id_node(12, "None"),
            )],
        );
        let other_mod = module_with_path(
            &["other"],
            vec![],
            vec![fn_decl_tail(
                20,
                Visibility::Public,
                "thing",
                id_node(22, "None"),
            )],
        );
        let gen = PyGenerator::new();
        let out = gen
            .generate_project(&[
                (&main_mod, std::path::Path::new("src/main.bock")),
                (&other_mod, std::path::Path::new("src/other.bock")),
            ])
            .unwrap();
        let runtime = out
            .files
            .iter()
            .find(|f| f.path == std::path::Path::new("_bock_runtime.py"))
            .expect("_bock_runtime.py emitted once");
        assert!(runtime.content.contains("class _BockNone:"), "got runtime");
        assert!(
            runtime.content.contains("__all__") && runtime.content.contains("\"_bock_none\""),
            "runtime must export underscore names via __all__; got:\n{}",
            runtime.content
        );
        // Every consuming module imports the shared runtime.
        let importers = out
            .files
            .iter()
            .filter(|f| f.content.contains("from _bock_runtime import *"))
            .count();
        assert_eq!(importers, 2, "both modules import the shared runtime");
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

    /// Q-clock-handler-routing: inside a `with Clock` function (where the
    /// `clock` handler is in scope), the §18.3.1 time builtins must route
    /// through the handler — `Instant.now()` → `clock.now_monotonic()`,
    /// `sleep(d)` → `clock.sleep(d)`, `start.elapsed()` → `clock.now_monotonic()
    /// - start` — NOT the inlined host primitives (`time.monotonic_ns()` /
    /// `asyncio.sleep`).
    #[test]
    fn clock_time_ops_route_through_handler() {
        let out = gen(&module(vec![], vec![clock_timed_fn()]));
        assert!(out.contains("clock.now_monotonic()"), "got: {out}");
        assert!(out.contains("clock.sleep("), "got: {out}");
        assert!(
            !out.contains("time.monotonic_ns()"),
            "host clock primitive leaked past the handler: {out}"
        );
        assert!(
            !out.contains("asyncio.sleep("),
            "host sleep primitive leaked past the handler: {out}"
        );
    }

    /// Builds `fn timed() with Clock { let start = Instant.now(); sleep(
    /// Duration.millis(1)); let d = start.elapsed() }`. The `with Clock` clause
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

    /// Builds `print("<s>")` as a bare call node.
    fn print_call(id: u32, s: &str) -> AIRNode {
        node(
            id,
            NodeKind::Call {
                callee: Box::new(id_node(id + 1, "print")),
                args: vec![AirArg {
                    label: None,
                    value: str_lit(id + 2, s),
                }],
                type_args: vec![],
            },
        )
    }

    /// A mid-block (statement-position) `if`/`else if`/`else` whose branches
    /// are bare `print` expressions must lower each branch tail to a *bare
    /// statement*, never a function-body `return`, and chain `else if` as
    /// `elif` (Q-python-ifelse-truncation). Before the fix: each branch
    /// emitted `return print(..)` — aborting the function after the taken
    /// branch so the trailing statement never ran — and the chain emitted
    /// `el    if (..):`, a SyntaxError, because the `el` prefix was followed
    /// by a fully-indented statement `if`. Sibling of the statement-`match`
    /// fix (#259).
    #[test]
    fn stmt_position_if_else_discards_branch_tails_and_chains_elif() {
        // if c1 { print("a") } else if c2 { print("b") } else { print("c") }
        // print("after")
        let chain = node(
            10,
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(id_node(11, "c1")),
                then_block: Box::new(block(12, vec![], Some(print_call(13, "a")))),
                else_block: Some(Box::new(node(
                    20,
                    NodeKind::If {
                        let_pattern: None,
                        condition: Box::new(id_node(21, "c2")),
                        then_block: Box::new(block(22, vec![], Some(print_call(23, "b")))),
                        else_block: Some(Box::new(block(30, vec![], Some(print_call(31, "c"))))),
                    },
                ))),
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("report"),
                generic_params: vec![],
                params: vec![param_node(2, "c1"), param_node(3, "c2")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(4, vec![chain, print_call(40, "after")], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            !out.contains("return print("),
            "statement-if branch tails must be discarded, not returned: {out}"
        );
        assert!(
            out.contains("elif c2:"),
            "`else if` must chain as a single `elif`: {out}"
        );
        assert!(
            !out.contains("el    "),
            "the `el` prefix must not be followed by an indented statement: {out}"
        );
        assert!(
            out.contains("print(\"after\""),
            "the statement after the chain must still be emitted: {out}"
        );
    }

    /// A `guard` whose `else` block does **not** diverge (spec §8.4 requires
    /// divergence, but the checker currently accepts this — surfaced as OPEN)
    /// must still not `return` out of the enclosing function: every other
    /// backend and the interpreter fall through to the statements after the
    /// guard. A spec-conforming diverging `else` is a statement tail and is
    /// unaffected by this. Same early-`return` family as the statement
    /// `match`/`if` truncations.
    #[test]
    fn stmt_guard_nondiverging_else_discards_tail() {
        let guard = node(
            10,
            NodeKind::Guard {
                let_pattern: None,
                condition: Box::new(id_node(11, "ok")),
                else_block: Box::new(block(12, vec![], Some(print_call(13, "warn")))),
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("report"),
                generic_params: vec![],
                params: vec![param_node(2, "ok")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(4, vec![guard, print_call(40, "after")], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            !out.contains("return print("),
            "a non-diverging guard else must fall through, not return: {out}"
        );
        assert!(
            out.contains("print(\"after\""),
            "the statement after the guard must still be emitted: {out}"
        );
    }

    /// Record / enum-struct-variant fields named after Python keywords must be
    /// escaped (`pass` → `pass_`, `lambda` → `lambda_`) at every field
    /// position with one agreed spelling: the dataclass declaration, the
    /// constructor keyword args, attribute access, and record-pattern
    /// destructuring (Q-python-keyword-record-fields; extends #162's value-
    /// identifier escaping to field position). Unescaped, the dataclass
    /// declaration `pass: int` is a SyntaxError before `main` even runs.
    #[test]
    fn keyword_record_fields_escaped_at_every_site() {
        let int_ty = || bock_ast::TypeExpr::Named {
            id: 0,
            span: span(),
            path: type_path(&["Int"]),
            args: vec![],
        };
        let rec = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Tally"),
                generic_params: vec![],
                fields: vec![bock_ast::RecordDeclField {
                    id: 0,
                    span: span(),
                    name: ident("pass"),
                    ty: int_ty(),
                    default: None,
                }],
            },
        );
        let enum_decl = node(
            2,
            NodeKind::EnumDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Gate"),
                generic_params: vec![],
                variants: vec![node(
                    3,
                    NodeKind::EnumVariant {
                        name: ident("Open"),
                        payload: EnumVariantPayload::Struct(vec![bock_ast::RecordDeclField {
                            id: 0,
                            span: span(),
                            name: ident("lambda"),
                            ty: int_ty(),
                            default: None,
                        }]),
                    },
                )],
            },
        );
        // let t = Tally { pass: 7 }
        let let_t = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "t")),
                ty: None,
                value: Box::new(node(
                    12,
                    NodeKind::RecordConstruct {
                        path: type_path(&["Tally"]),
                        fields: vec![AirRecordField {
                            name: ident("pass"),
                            value: Some(Box::new(int_lit(13, "7"))),
                        }],
                        spread: None,
                    },
                )),
            },
        );
        // print(t.pass)
        let access = node(
            20,
            NodeKind::Call {
                callee: Box::new(id_node(21, "print")),
                args: vec![AirArg {
                    label: None,
                    value: node(
                        22,
                        NodeKind::FieldAccess {
                            object: Box::new(id_node(23, "t")),
                            field: ident("pass"),
                        },
                    ),
                }],
                type_args: vec![],
            },
        );
        // match t { Tally { pass: p } => print("x") }
        let m = node(
            30,
            NodeKind::Match {
                scrutinee: Box::new(id_node(31, "t")),
                arms: vec![node(
                    32,
                    NodeKind::MatchArm {
                        pattern: Box::new(node(
                            33,
                            NodeKind::RecordPat {
                                path: type_path(&["Tally"]),
                                fields: vec![AirRecordPatternField {
                                    name: ident("pass"),
                                    pattern: Some(Box::new(bind_pat(34, "p"))),
                                }],
                                rest: false,
                            },
                        )),
                        guard: None,
                        body: Box::new(block(35, vec![], Some(print_call(36, "x")))),
                    },
                )],
            },
        );
        // let g = Open { lambda: 3 }
        let let_g = node(
            40,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(41, "g")),
                ty: None,
                value: Box::new(node(
                    42,
                    NodeKind::RecordConstruct {
                        path: type_path(&["Open"]),
                        fields: vec![AirRecordField {
                            name: ident("lambda"),
                            value: Some(Box::new(int_lit(43, "3"))),
                        }],
                        spread: None,
                    },
                )),
            },
        );
        let f = node(
            5,
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
                body: Box::new(block(6, vec![let_t, access, m, let_g], None)),
            },
        );
        let out = gen(&module(vec![], vec![rec, enum_decl, f]));
        assert!(
            out.contains("pass_: int"),
            "dataclass field must be keyword-escaped: {out}"
        );
        assert!(
            out.contains("Tally(pass_=7)"),
            "constructor kwargs must be keyword-escaped: {out}"
        );
        assert!(
            out.contains("t.pass_"),
            "field access must be keyword-escaped: {out}"
        );
        assert!(
            out.contains("Tally(pass_=p)"),
            "record-pattern destructuring must be keyword-escaped: {out}"
        );
        assert!(
            out.contains("lambda_: int"),
            "enum struct-variant field must be keyword-escaped: {out}"
        );
        assert!(
            out.contains("Gate_Open(lambda_=3)"),
            "variant constructor kwargs must be keyword-escaped: {out}"
        );
        assert!(
            !out.contains("pass:") && !out.contains("pass=") && !out.contains("lambda:"),
            "no field position may emit the unescaped keyword: {out}"
        );
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

    // ── Statement-position loop tails must NOT be `return`ed ─────────────────
    //
    // A loop body's final expression is a *statement* in Bock — the loop
    // evaluates to Unit, the body's value is discarded. The Python backend's
    // shared `emit_block_body` had emitted a tail expression as a function-body
    // `return`, so e.g. `for i in 1..=3 { println(i) }` lowered to
    // `for i in …: return print(i)` — the `return` exits `main` on the FIRST
    // iteration (the loop runs once, then the function returns). fizzbuzz
    // printed one line, inventory-system listed one product. These tests pin
    // the bare-statement discard for each loop kind.

    /// Build a `println(<arg>)` call node.
    fn py_println_call(id: u32, arg: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::Call {
                callee: Box::new(id_node(id + 1, "println")),
                args: vec![AirArg {
                    label: None,
                    value: arg,
                }],
                type_args: vec![],
            },
        )
    }

    #[test]
    fn py_for_loop_body_tail_call_is_statement_not_returned() {
        // fn main() { for i in 1..=3 { println(i) } }
        let range = node(
            20,
            NodeKind::Range {
                lo: Box::new(int_lit(21, "1")),
                hi: Box::new(int_lit(22, "3")),
                inclusive: true,
            },
        );
        let loop_body = block(30, vec![], Some(py_println_call(31, id_node(33, "i"))));
        let for_loop = node(
            10,
            NodeKind::For {
                pattern: Box::new(bind_pat(11, "i")),
                iterable: Box::new(range),
                body: Box::new(loop_body),
            },
        );
        let f = fn_decl_body(0, "main", block(2, vec![for_loop], None));
        let out = gen(&module(vec![], vec![f]));
        assert!(
            !out.contains("return print(i)"),
            "for-loop body tail must be a bare statement, not `return` (would abort \
             main after one iteration); got:\n{out}"
        );
        assert!(
            out.contains("print(i)"),
            "for-loop body tail call must still be emitted; got:\n{out}"
        );
    }

    #[test]
    fn py_while_loop_body_tail_call_is_statement_not_returned() {
        // fn main() { while cond { println(i) } }
        let loop_body = block(30, vec![], Some(py_println_call(31, id_node(33, "i"))));
        let while_loop = node(
            10,
            NodeKind::While {
                condition: Box::new(bool_lit(12, true)),
                body: Box::new(loop_body),
            },
        );
        let f = fn_decl_body(0, "main", block(2, vec![while_loop], None));
        let out = gen(&module(vec![], vec![f]));
        assert!(
            !out.contains("return print(i)"),
            "while-loop body tail must be a bare statement, not `return`; got:\n{out}"
        );
        assert!(
            out.contains("print(i)"),
            "while-loop body tail call must still be emitted; got:\n{out}"
        );
    }

    #[test]
    fn py_infinite_loop_body_tail_call_is_statement_not_returned() {
        // fn main() { loop { println(i) } }
        let loop_body = block(30, vec![], Some(py_println_call(31, id_node(33, "i"))));
        let inf_loop = node(
            10,
            NodeKind::Loop {
                body: Box::new(loop_body),
            },
        );
        let f = fn_decl_body(0, "main", block(2, vec![inf_loop], None));
        let out = gen(&module(vec![], vec![f]));
        assert!(
            !out.contains("return print(i)"),
            "loop body tail must be a bare statement, not `return`; got:\n{out}"
        );
        assert!(
            out.contains("print(i)"),
            "loop body tail call must still be emitted; got:\n{out}"
        );
    }

    #[test]
    fn py_function_body_tail_call_still_returned() {
        // Guard against over-correction: a *function* body tail must still
        // `return` its value (this is NOT statement position).
        // fn answer() { 42 }
        let f = fn_decl_body(0, "answer", block(2, vec![], Some(int_lit(3, "42"))));
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("return 42"),
            "function-body tail must still be returned; got:\n{out}"
        );
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

    /// A `Call` whose callee is a `Lambda` must parenthesize the lambda so the
    /// trailing argument list invokes the lambda, not its body. Without the
    /// grouping, `lambda x: x(42)` parses as `lambda x: (x(42))` — the `(42)`
    /// binds to the body, never calling the lambda.
    ///
    /// This is the shape the AIR compose desugar (`f >> g` →
    /// `(__compose_x) => g(f(__compose_x))`) produces for chained `>>`: the
    /// inner compose lowers to a `Lambda`, which then appears as the callee
    /// `f` in the outer `f(__compose_x)`. See examples/real-world/data-pipeline
    /// (`normalize >> compute_summary >> format_summary`).
    #[test]
    fn py_call_with_lambda_callee_parenthesizes() {
        // (lambda x: x)(42)
        let lambda = node(
            1,
            NodeKind::Lambda {
                params: vec![param_node(2, "x")],
                body: Box::new(id_node(3, "x")),
            },
        );
        let call = node(
            4,
            NodeKind::Call {
                callee: Box::new(lambda),
                args: vec![AirArg {
                    label: None,
                    value: int_lit(5, "42"),
                }],
                type_args: vec![],
            },
        );
        let f = fn_decl_tail(0, Visibility::Private, "test", call);
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("(lambda x: x)(42)"),
            "lambda callee must be parenthesized so it is invoked; got: {out}"
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
                // Instance method leads with `self` (real lowering); a no-`self`
                // method is an associated `@staticmethod`.
                params: vec![param_node(6, "self")],
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

    /// A self-method `fn <name>(self) -> String { "<lit>" }`, for class/impl
    /// fixtures.
    fn self_method_returning(id: u32, name: &str, lit: &str) -> AIRNode {
        node(
            id,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![param_node(id + 1, "self")],
                return_type: Some(Box::new(node(
                    id + 2,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(id + 3, vec![], Some(str_lit(id + 4, lit)))),
            },
        )
    }

    /// An `impl <trait?> for <target>` block carrying `methods`.
    fn impl_block_node(
        id: u32,
        target: &str,
        trait_name: Option<&str>,
        methods: Vec<AIRNode>,
    ) -> AIRNode {
        node(
            id,
            NodeKind::ImplBlock {
                annotations: vec![],
                target: Box::new(node(
                    id + 1,
                    NodeKind::TypeNamed {
                        path: type_path(&[target]),
                        args: vec![],
                    },
                )),
                trait_path: trait_name.map(|t| type_path(&[t])),
                trait_args: vec![],
                generic_params: vec![],
                where_clause: vec![],
                methods,
            },
        )
    }

    /// A `trait <name> { fn <m>(self) -> String }` declaration (ABC stub).
    fn trait_node(id: u32, name: &str, method: &str) -> AIRNode {
        node(
            id,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident(name),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![self_method_returning(id + 50, method, "")],
            },
        )
    }

    /// A `class <name> { <field>: String }` with no inline methods.
    fn class_with_field(id: u32, name: &str, field: &str) -> AIRNode {
        node(
            id,
            NodeKind::ClassDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident(name),
                generic_params: vec![],
                base: None,
                traits: vec![],
                fields: vec![named_field(field, "String")],
                methods: vec![],
            },
        )
    }

    /// Q-class-codegen (py): a `class T` with an inherent `impl T` and a trait
    /// `impl Trait for T` must route BOTH impls' methods into the class body —
    /// the same path records already use — and subclass the trait ABC. Before
    /// the fix the Python backend emitted `class T:` with only `__init__`,
    /// silently DROPPING every impl/trait method.
    #[test]
    fn py_class_attaches_inherent_and_trait_impl_methods() {
        let cls = class_with_field(1, "Widget", "name");
        let inherent = impl_block_node(
            10,
            "Widget",
            None,
            vec![self_method_returning(11, "describe", "a widget")],
        );
        let trait_decl = trait_node(20, "Render", "render");
        let trait_impl = impl_block_node(
            30,
            "Widget",
            Some("Render"),
            vec![self_method_returning(31, "render", "<widget/>")],
        );
        let out = gen(&module(vec![], vec![trait_decl, cls, inherent, trait_impl]));
        // The inherent-impl method is attached to the class body.
        assert!(
            out.contains("def describe(self)"),
            "inherent-impl method must be attached to the class, got:\n{out}"
        );
        // The trait-impl method is attached to the class body.
        assert!(
            out.contains("def render(self)"),
            "trait-impl method must be attached to the class, got:\n{out}"
        );
        // The class subclasses the trait ABC for real dispatch.
        assert!(
            out.contains("class Widget(Render):"),
            "class must subclass the implemented trait, got:\n{out}"
        );
        // No orphan module-level `# impl` functions left behind.
        assert!(
            !out.contains("\ndef describe("),
            "impl method must not leak as a module-level function, got:\n{out}"
        );
    }

    /// Q-class-codegen behavioral check: a generated class actually dispatches
    /// its inherent and trait methods at runtime.
    #[test]
    fn py_class_methods_dispatch_at_runtime() {
        if !has_python3() {
            return;
        }
        let cls = class_with_field(1, "Widget", "name");
        let inherent = impl_block_node(
            10,
            "Widget",
            None,
            vec![self_method_returning(11, "describe", "a widget")],
        );
        let trait_decl = trait_node(20, "Render", "render");
        let trait_impl = impl_block_node(
            30,
            "Widget",
            Some("Render"),
            vec![self_method_returning(31, "render", "<widget/>")],
        );
        let out = gen(&module(vec![], vec![trait_decl, cls, inherent, trait_impl]));
        let program =
            format!("{out}\nw = Widget(name=\"x\")\nprint(w.describe())\nprint(w.render())\n");
        let got = run_py(&program);
        assert_eq!(got, "a widget\n<widget/>", "got:\n{got}\nfrom:\n{out}");
    }

    /// Q-py-impl-before-trait (ordering): a class/record that subclasses a trait
    /// ABC must be emitted AFTER the trait is defined. Here the trait `Render`
    /// is declared textually AFTER the record `Widget` that impls it; naive
    /// source-order emission produced `class Widget(Render):` before `Render`
    /// existed → `NameError`. The fix topologically orders type decls so a base
    /// precedes every subclass.
    #[test]
    fn py_trait_emitted_before_subclassing_record() {
        // record Widget { name: String } — declared FIRST.
        let rec = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Widget"),
                generic_params: vec![],
                fields: vec![named_field("name", "String")],
            },
        );
        let trait_impl = impl_block_node(
            10,
            "Widget",
            Some("Render"),
            vec![self_method_returning(11, "render", "<w/>")],
        );
        // trait Render — declared LAST, after the record + its impl.
        let trait_decl = trait_node(20, "Render", "render");
        let out = gen(&module(vec![], vec![rec, trait_impl, trait_decl]));
        let widget_pos = out
            .find("class Widget(Render):")
            .unwrap_or_else(|| panic!("expected `class Widget(Render):`, got:\n{out}"));
        let render_pos = out
            .find("class Render:")
            .unwrap_or_else(|| panic!("expected `class Render:`, got:\n{out}"));
        assert!(
            render_pos < widget_pos,
            "trait ABC `Render` must be emitted before subclass `Widget`, got:\n{out}"
        );
        // And it must actually import/parse + run without NameError.
        if has_python3() {
            assert!(
                check_py_syntax(&out),
                "ordered output must parse, got:\n{out}"
            );
            let program = format!("{out}\nw = Widget(name=\"x\")\nprint(w.render())\n");
            assert_eq!(run_py(&program), "<w/>", "got from:\n{out}");
        }
    }

    /// Q-class-codegen recursion guard: when an inherent `impl T { fn render }`
    /// and a trait `impl Trait for T { fn render }` share a method name, Python's
    /// single per-class namespace must keep exactly ONE `def render` — the
    /// inherent (concrete) one. Emitting both made the delegating trait body
    /// (`self.render()`) overwrite and call itself → `RecursionError`
    /// (react-components' `Button`).
    #[test]
    fn py_inherent_method_wins_over_colliding_trait_method() {
        let cls = class_with_field(1, "Button", "label");
        // inherent: concrete body.
        let inherent = impl_block_node(
            10,
            "Button",
            None,
            vec![self_method_returning(11, "render", "<button/>")],
        );
        let trait_decl = trait_node(20, "Component", "render");
        // trait impl: delegates to the inherent method (`self.render()`).
        let trait_render = node(
            31,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("render"),
                generic_params: vec![],
                params: vec![param_node(32, "self")],
                return_type: Some(Box::new(node(
                    33,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    34,
                    vec![],
                    Some(node(
                        35,
                        NodeKind::Call {
                            callee: Box::new(node(
                                36,
                                NodeKind::FieldAccess {
                                    object: Box::new(id_node(37, "self")),
                                    field: ident("render"),
                                },
                            )),
                            type_args: vec![],
                            args: vec![],
                        },
                    )),
                )),
            },
        );
        let trait_impl = impl_block_node(30, "Button", Some("Component"), vec![trait_render]);
        let out = gen(&module(vec![], vec![trait_decl, cls, inherent, trait_impl]));
        // Exactly one `def render` in the Button class — count occurrences.
        let count = out.matches("def render(self)").count();
        assert_eq!(
            count,
            2, // one in the `Component` ABC stub, one in `Button`
            "expected one `render` in Button + one in the ABC, got {count}:\n{out}"
        );
        // The kept Button method is the inherent (concrete) one, not the
        // self-delegating trait one.
        assert!(
            out.contains("return \"<button/>\""),
            "Button.render must be the concrete inherent body, got:\n{out}"
        );
        if has_python3() {
            let program = format!("{out}\nb = Button(label=\"x\")\nprint(b.render())\n");
            assert_eq!(run_py(&program), "<button/>", "got from:\n{out}");
        }
    }

    /// A class declared with an inline `base` (another class) must be emitted
    /// after that base class, even when source order puts the subclass first.
    #[test]
    fn py_base_class_emitted_before_subclass() {
        // class Sub (base = Base) — declared FIRST.
        let sub = node(
            1,
            NodeKind::ClassDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Sub"),
                generic_params: vec![],
                base: Some(type_path(&["Base"])),
                traits: vec![],
                fields: vec![named_field("name", "String")],
                methods: vec![],
            },
        );
        // class Base — declared LAST.
        let base = class_with_field(10, "Base", "id");
        let out = gen(&module(vec![], vec![sub, base]));
        let base_pos = out
            .find("class Base:")
            .unwrap_or_else(|| panic!("expected `class Base:`, got:\n{out}"));
        let sub_pos = out
            .find("class Sub(Base):")
            .unwrap_or_else(|| panic!("expected `class Sub(Base):`, got:\n{out}"));
        assert!(
            base_pos < sub_pos,
            "base class must be emitted before subclass, got:\n{out}"
        );
    }

    /// An `effect` is emitted as an `(ABC)` base class that an `impl Effect for
    /// T` makes a base of `T` (`class StubChannel(Channel):`). An effect declared
    /// AFTER its impl must still be emitted before the implementing record, else
    /// the base list `(Channel)` raises `NameError` (chat-protocol's `Channel`).
    #[test]
    fn py_effect_emitted_before_subclassing_record() {
        // record StubChannel {} — declared FIRST, impls the effect.
        let rec = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("StubChannel"),
                generic_params: vec![],
                fields: vec![named_field("tag", "String")],
            },
        );
        let chan_impl = impl_block_node(
            10,
            "StubChannel",
            Some("Channel"),
            vec![self_method_returning(11, "send", "sent")],
        );
        // effect Channel { fn send(self) -> String } — declared LAST.
        let effect = node(
            20,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Channel"),
                generic_params: vec![],
                components: vec![],
                operations: vec![self_method_returning(21, "send", "")],
            },
        );
        let out = gen(&module(vec![], vec![rec, chan_impl, effect]));
        let chan_pos = out
            .find("class Channel(ABC):")
            .unwrap_or_else(|| panic!("expected `class Channel(ABC):`, got:\n{out}"));
        let stub_pos = out
            .find("class StubChannel(Channel):")
            .unwrap_or_else(|| panic!("expected `class StubChannel(Channel):`, got:\n{out}"));
        assert!(
            chan_pos < stub_pos,
            "effect ABC `Channel` must precede the record that impls it, got:\n{out}"
        );
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

    /// A `let`-binding node (immutable, simple bind pattern).
    fn let_node(id: u32, name: &str, value: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(id + 1, name)),
                ty: None,
                value: Box::new(value),
            },
        )
    }

    /// `expr?` — a `Propagate` over `expr`.
    fn propagate(id: u32, expr: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::Propagate {
                expr: Box::new(expr),
            },
        )
    }

    /// `fn() -> Result[..]` whose body is `let v = inner?` then a tail `Ok(v)`.
    /// Exercises the `?` lowering: `_bock_try(..)` + the try/except envelope.
    #[test]
    fn propagate_unwraps_and_wraps_body() {
        let inner_call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "fallible")),
                args: vec![],
                type_args: vec![],
            },
        );
        let body = block(
            2,
            vec![let_node(3, "v", propagate(4, inner_call))],
            Some(node(
                6,
                NodeKind::ResultConstruct {
                    variant: ResultVariant::Ok,
                    value: Some(Box::new(id_node(7, "v"))),
                },
            )),
        );
        let f = fn_decl_body(1, "do_it", body);
        let out = gen(&module(vec![], vec![f]));
        // `?` lowers to the unwrap helper, not a bare passthrough.
        assert!(out.contains("_bock_try(fallible())"), "got: {out}");
        // The function body is wrapped in the propagate envelope.
        assert!(out.contains("try:"), "got: {out}");
        assert!(
            out.contains("except _BockPropagate as __bock_p:"),
            "got: {out}"
        );
        assert!(out.contains("return __bock_p.value"), "got: {out}");
        // The propagate runtime prelude is emitted.
        assert!(out.contains("def _bock_try(v):"), "got: {out}");
    }

    /// A function with no `?` must NOT gain the try/except envelope or the
    /// propagate runtime (no needless cost / behavioural change).
    #[test]
    fn no_propagate_no_envelope() {
        let body = block(2, vec![], Some(int_lit(3, "1")));
        let f = fn_decl_body(1, "plain", body);
        let out = gen(&module(vec![], vec![f]));
        assert!(!out.contains("_bock_try"), "got: {out}");
        assert!(!out.contains("_BockPropagate"), "got: {out}");
    }

    /// `fn() { let y = 1; let z = { let y = y + 10; y * 2 }; y + z }` — the inner
    /// block's `let y` shadows the outer `y` and must be renamed so the outer `y`
    /// is untouched (Python has no block scope for `=`).
    #[test]
    fn nested_block_let_shadow_is_renamed() {
        let add = |id, l: AIRNode, r: AIRNode| {
            node(
                id,
                NodeKind::BinaryOp {
                    op: BinOp::Add,
                    left: Box::new(l),
                    right: Box::new(r),
                },
            )
        };
        let mul = |id, l: AIRNode, r: AIRNode| {
            node(
                id,
                NodeKind::BinaryOp {
                    op: BinOp::Mul,
                    left: Box::new(l),
                    right: Box::new(r),
                },
            )
        };
        // inner block: { let y = y + 10; y * 2 }
        let inner_block = block(
            20,
            vec![let_node(
                21,
                "y",
                add(22, id_node(23, "y"), int_lit(24, "10")),
            )],
            Some(mul(25, id_node(26, "y"), int_lit(27, "2"))),
        );
        let body = block(
            2,
            vec![
                let_node(3, "y", int_lit(4, "1")),
                let_node(5, "z", inner_block),
            ],
            Some(add(8, id_node(9, "y"), id_node(10, "z"))),
        );
        let f = fn_decl_body(1, "nested", body);
        let out = gen(&module(vec![], vec![f]));
        // The inner `let y` is renamed; the outer `y = 1` and the final `y + z`
        // read the original `y`.
        assert!(out.contains("y__s"), "expected a shadow alias, got: {out}");
        assert!(out.contains("y = 1"), "got: {out}");
        // The final tail uses the *un*-aliased outer `y` (it appears as `(y + z)`
        // — the alias name is `y__s1`, which `(y + z)` does not contain).
        assert!(out.contains("return (y + z)"), "got: {out}");
    }

    /// A same-block re-bind (`let acc = …; let acc = acc + 1`) is a plain Python
    /// rebind — no alias, no duplicate.
    #[test]
    fn same_block_rebind_is_not_renamed() {
        let add = |id, l: AIRNode, r: AIRNode| {
            node(
                id,
                NodeKind::BinaryOp {
                    op: BinOp::Add,
                    left: Box::new(l),
                    right: Box::new(r),
                },
            )
        };
        let body = block(
            2,
            vec![
                let_node(3, "acc", int_lit(4, "1")),
                let_node(5, "acc", add(6, id_node(7, "acc"), int_lit(8, "2"))),
            ],
            Some(id_node(9, "acc")),
        );
        let f = fn_decl_body(1, "rebind", body);
        let out = gen(&module(vec![], vec![f]));
        assert!(!out.contains("acc__s"), "must not rename, got: {out}");
        assert!(out.contains("acc = 1"), "got: {out}");
        assert!(out.contains("acc = (acc + 2)"), "got: {out}");
    }

    /// A Void function whose tail is a bare `loop { break }` must emit a
    /// `while True:` statement, never `return # unsupported`.
    #[test]
    fn tail_loop_emits_while_not_unsupported() {
        let loop_body = block(10, vec![node(11, NodeKind::Break { value: None })], None);
        let tail_loop = node(
            5,
            NodeKind::Loop {
                body: Box::new(loop_body),
            },
        );
        let body = block(2, vec![], Some(tail_loop));
        let f = fn_decl_body(1, "spin", body);
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("while True:"), "got: {out}");
        assert!(!out.contains("# unsupported"), "got: {out}");
        assert!(!out.contains("return # unsupported"), "got: {out}");
    }

    /// Helper: a private `fn <name>()` with an explicit body block.
    fn fn_decl_body(id: u32, name: &str, body: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::FnDecl {
                annotations: vec![],
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
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir();
        // Unique filename per call: `cargo test` runs these checks on parallel
        // threads, so a shared fixed path races — one test's `py_compile` reads or
        // removes the file another test just wrote, yielding spurious "must parse"
        // failures (this flaked every CI lane except ubuntu-stable once the new
        // value-position match tests added more concurrent callers).
        let path = dir.join(format!(
            "bock_test_output_{}_{}.py",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
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
        // Normalize CRLF→LF: on Windows python writes `\r\n` line endings, which
        // would fail exact-match assertions against `\n`-terminated expectations.
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n")
            .trim()
            .to_string()
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

    /// Build a `MatchArm` with a wildcard pattern and the given body.
    fn wildcard_arm(id: u32, body: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::MatchArm {
                pattern: Box::new(node(id + 100, NodeKind::WildcardPat)),
                guard: None,
                body: Box::new(body),
            },
        )
    }

    /// A `Block` with the given leading statements and a string tail value.
    fn block_with_tail(id: u32, stmts: Vec<AIRNode>, tail: &str) -> AIRNode {
        node(
            id,
            NodeKind::Block {
                stmts,
                tail: Some(Box::new(str_lit(id + 1, tail))),
            },
        )
    }

    #[test]
    fn valpos_arm_with_loop_leading_stmt_needs_stmt_form() {
        // `_ => { for _ in xs { f() } "v" }` — the leading `for` loop has no
        // Python expression form, so the lambda chain would drop it. The arm
        // must be routed to statement form.
        let loop_stmt = node(
            10,
            NodeKind::For {
                pattern: Box::new(node(11, NodeKind::WildcardPat)),
                iterable: Box::new(id_node(12, "xs")),
                body: Box::new(node(
                    13,
                    NodeKind::Block {
                        stmts: vec![],
                        tail: None,
                    },
                )),
            },
        );
        let arms = vec![wildcard_arm(1, block_with_tail(20, vec![loop_stmt], "v"))];
        assert!(
            match_arm_drops_leading_stmts(&arms),
            "a value-tail arm with a leading `for` loop must route to statement form"
        );
        assert!(match_value_needs_stmt_form(&arms));
        assert!(value_needs_stmt_form(&node(
            30,
            NodeKind::Match {
                scrutinee: Box::new(id_node(31, "j")),
                arms,
            }
        )));
    }

    #[test]
    fn valpos_arm_with_assign_leading_stmt_needs_stmt_form() {
        // `_ => { x = 1; "v" }` — a leading assignment is not lambda-expressible.
        let assign = node(
            10,
            NodeKind::Assign {
                target: Box::new(id_node(11, "x")),
                op: AssignOp::Assign,
                value: Box::new(int_lit(12, "1")),
            },
        );
        let arms = vec![wildcard_arm(1, block_with_tail(20, vec![assign], "v"))];
        assert!(match_arm_drops_leading_stmts(&arms));
    }

    #[test]
    fn valpos_arm_with_mut_let_leading_stmt_needs_stmt_form() {
        // `_ => { let mut x = 1; "v" }` — a mutable `let` cannot be a lambda
        // parameter, so the chain would drop it.
        let mut_let = node(
            10,
            NodeKind::LetBinding {
                is_mut: true,
                pattern: Box::new(node(
                    11,
                    NodeKind::BindPat {
                        name: ident("x"),
                        is_mut: true,
                    },
                )),
                ty: None,
                value: Box::new(int_lit(12, "1")),
            },
        );
        let arms = vec![wildcard_arm(1, block_with_tail(20, vec![mut_let], "v"))];
        assert!(match_arm_drops_leading_stmts(&arms));
    }

    #[test]
    fn valpos_arm_with_simple_let_stays_on_lambda_chain() {
        // `_ => { let x = 1; "v" }` — a simple immutable `let` folds into a
        // `lambda x:` parameter, so the chain handles it and we must NOT force
        // statement form (that would regress the proven lambda-chain path).
        let simple_let = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(node(
                    11,
                    NodeKind::BindPat {
                        name: ident("x"),
                        is_mut: false,
                    },
                )),
                ty: None,
                value: Box::new(int_lit(12, "1")),
            },
        );
        let arms = vec![wildcard_arm(1, block_with_tail(20, vec![simple_let], "v"))];
        assert!(
            !match_arm_drops_leading_stmts(&arms),
            "a simple immutable `let` is lambda-expressible — keep it on the chain"
        );
    }

    #[test]
    fn valpos_arm_with_bare_call_stays_on_lambda_chain() {
        // `_ => { f(); "v" }` — a bare expression statement folds into a
        // `lambda _:` and stays on the chain.
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "f")),
                args: vec![],
                type_args: vec![],
            },
        );
        let arms = vec![wildcard_arm(1, block_with_tail(20, vec![call], "v"))];
        assert!(!match_arm_drops_leading_stmts(&arms));
    }

    #[test]
    fn valpos_arm_tail_only_block_stays_on_lambda_chain() {
        // `_ => { "v" }` — no leading statements, nothing to drop.
        let arms = vec![wildcard_arm(1, block_with_tail(20, vec![], "v"))];
        assert!(!match_arm_drops_leading_stmts(&arms));
    }

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
        // `T = TypeVar("T")` exactly once in the file.
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
                fields: vec![named_field("message", "String")],
            },
        );
        // impl Error for SimpleError { fn message(self) -> String { self.message } }
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
        // fn read(e: SimpleError) -> String { e.message() }
        let read_fn = node(
            30,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("read"),
                generic_params: vec![],
                params: vec![typed_param_node(31, "e", "SimpleError")],
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
                                    // in both the field-access object and the
                                    // self arg; `desugared_self_call` keys on the
                                    // shared NodeId, so the test must too.
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
        // The dataclass field stays `message`.
        assert!(
            out.contains("message: str"),
            "dataclass field should remain `message: str`, got: {out}"
        );
        // The inlined method and the call site are renamed to `message_method`
        // so the dataclass field no longer overwrites the method attribute.
        assert!(
            out.contains("def message_method(self)"),
            "method should be `def message_method`, got: {out}"
        );
        assert!(
            out.contains(".message_method()"),
            "call site should be `.message_method()`, got: {out}"
        );
        // The method body still reads the field via `self.message`.
        assert!(
            out.contains("return self.message"),
            "method body should read the field `self.message`, got: {out}"
        );
        // No bare `def message(self)` that the field would clobber.
        assert!(
            !out.contains("def message(self)"),
            "must NOT emit a `def message(self)` clobbered by the field, got: {out}"
        );
    }

    // ── Python-specific control-flow / import lowering ──────────────────────

    fn call_no_args(id: u32, name: &str) -> AIRNode {
        node(
            id,
            NodeKind::Call {
                callee: Box::new(id_node(id + 1, name)),
                args: vec![],
                type_args: vec![],
            },
        )
    }

    /// `todo()` / `unreachable()` are diverging `raise` expressions; an arbitrary
    /// call or the `Unreachable` node is not (the latter *prints* a `raise` but
    /// is the dedicated node, recognised separately).
    #[test]
    fn is_raise_expr_recognises_todo_and_unreachable() {
        assert!(is_raise_expr(&call_no_args(1, "todo")));
        assert!(is_raise_expr(&call_no_args(3, "unreachable")));
        assert!(is_raise_expr(&node(5, NodeKind::Unreachable)));
        assert!(!is_raise_expr(&call_no_args(6, "compute")));
        assert!(!is_raise_expr(&int_lit(8, "1")));
    }

    /// A `let x = todo()` body emits a bare `raise`, never `x = raise …` (a
    /// `SyntaxError`), and a `todo()` function tail emits a bare `raise`, never
    /// `return raise …`.
    #[test]
    fn todo_in_return_and_let_position_emits_bare_raise() {
        // Tail position: `fn f() { todo() }`.
        let f = fn_decl_tail(1, Visibility::Private, "f", call_no_args(10, "todo"));
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("raise NotImplementedError()"),
            "expected a `raise`, got: {out}"
        );
        assert!(
            !out.contains("return raise"),
            "must NOT emit `return raise …`, got: {out}"
        );
    }

    /// An expression-position `loop` bound into a `let`, yielding via `break v`,
    /// is hoisted to a `while True:` whose `break v` becomes `<target> = v` then
    /// `break` — never the invalid expression `let x = <loop>`.
    #[test]
    fn value_loop_break_hoists_to_while_assign() {
        // `fn f() { let r = loop { break 5 }  r }`
        let break_node = node(
            30,
            NodeKind::Break {
                value: Some(Box::new(int_lit(31, "5"))),
            },
        );
        let loop_node = node(
            20,
            NodeKind::Loop {
                body: Box::new(block(21, vec![], Some(break_node))),
            },
        );
        let let_r = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "r")),
                ty: None,
                value: Box::new(loop_node),
            },
        );
        let body = block(2, vec![let_r], Some(id_node(40, "r")));
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
        assert!(
            out.contains("while True:"),
            "value-loop should hoist to `while True:`, got: {out}"
        );
        // The shared AIR value-CF hoist introduces a `__bock_cf_N` temp: the
        // `break 5` assigns it (`__bock_cf_0 = 5`) then `break`, and `r` reads it.
        assert!(
            out.contains("__bock_cf_0 = 5"),
            "break value should assign the hoisted temp `__bock_cf_0 = 5`, got: {out}"
        );
        assert!(
            out.contains("r = __bock_cf_0"),
            "the let should read the hoisted temp `r = __bock_cf_0`, got: {out}"
        );
        assert!(
            out.contains("break"),
            "the loop should still `break`, got: {out}"
        );
        assert!(
            !out.contains("# unsupported"),
            "must NOT emit `# unsupported`, got: {out}"
        );
    }

    /// A record field declaration whose name matches a sibling-module public
    /// function must NOT be counted as a reference: the field-label occurrence is
    /// subtracted, so the implicit-import scan does not pull in
    /// `from <sibling> import <name>` (which closes a Python import cycle).
    #[test]
    fn field_label_does_not_trigger_implicit_import() {
        // `module models` declares `record Summary { total: Int }`. `total` is a
        // FIELD name; it must not match a sibling `fn total`.
        let summary = node(
            5,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Summary"),
                generic_params: vec![],
                fields: vec![bock_ast::RecordDeclField {
                    id: 6,
                    span: span(),
                    name: ident("total"),
                    ty: bock_ast::TypeExpr::Named {
                        id: 7,
                        span: span(),
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                    default: None,
                }],
            },
        );
        let models = module_with_path(&["models"], vec![], vec![summary]);
        // Public-symbol map says `total` is declared by `service`.
        let mut public_symbols = HashMap::new();
        public_symbols.insert("total".to_string(), "service".to_string());
        let imports = implicit_imports_for(&models, &public_symbols, "models");
        assert!(
            imports.is_empty(),
            "a field named `total` must not implicit-import `service.total`, got: {imports:?}"
        );
    }

    /// `fn f() { let x = if (c) { 1 } else { return 0 }  x }` — value-position
    /// `if` with a diverging else. The shared value-CF hoist pre-binds a temp
    /// and lowers the `if` to statements, never `# unsupported` or an invalid
    /// `lambda` capturing the `return`.
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
    fn diverging_value_if_hoists_to_stmt_form_no_unsupported() {
        let out = gen(&diverging_value_if_fn());
        assert!(
            !out.contains("# unsupported"),
            "diverging value-if must not emit `# unsupported`, got: {out}"
        );
        assert!(
            out.contains("__bock_cf_0 = 1"),
            "value arm must assign the temp, got: {out}"
        );
        assert!(
            out.contains("return 0"),
            "diverging arm must keep its return, got: {out}"
        );
    }

    // ── Value-position match: plain-record / tuple / guard / or / nested ────────
    //
    // These cover the value-position (`match` consumed as a value / function
    // tail) lowering for the pattern kinds that the legacy `(lambda __v: …)`
    // conditional chain could not bind: a bare-bind record field
    // (`Point { x, .. } => "x=${x}"` — Q-plainrecord-valpos-match, py half), a
    // tuple destructure, a guard arm (`n if (n < 0) => …`), an or-pattern, and a
    // nested constructor (`Some(Ok(n)) => …`). The chain emitted the body lambda
    // with the binding free (`(lambda __v: f"x={x}")(p)` → `NameError: name 'x'`),
    // dropped the guard entirely, and tested or/record/tuple arms as `if True`
    // (collapsing every later arm). The fix routes a value-position match needing
    // the if-chain to the statement-form `match`/`case` machinery, which binds
    // and tests every pattern kind correctly.

    /// Build a single-arg `fn <name>(p) -> ...` whose body is a value-position
    /// (tail) `match p { <arms> }`. The arms are expression-bodied.
    fn match_fn(name: &str, arms: Vec<AIRNode>) -> AIRNode {
        let match_node = node(
            900,
            NodeKind::Match {
                scrutinee: Box::new(id_node(901, "p")),
                arms,
            },
        );
        let f = node(
            910,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![param_node(911, "p")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(912, vec![], Some(match_node))),
            },
        );
        module(vec![], vec![f])
    }

    fn record_pat_field(_id: u32, name: &str, pat: Option<AIRNode>) -> AirRecordPatternField {
        AirRecordPatternField {
            name: ident(name),
            pattern: pat.map(Box::new),
        }
    }

    /// `Point { x, .. } => "x=${x}"` — a bare-bind record field in value
    /// position. Must bind `x` from the scrutinee, never emit a free `x`.
    #[test]
    fn py_plainrecord_match_binds_field_in_value_position() {
        let arm = node(
            100,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    101,
                    NodeKind::RecordPat {
                        path: type_path(&["Point"]),
                        fields: vec![record_pat_field(102, "x", None)],
                        rest: true,
                    },
                )),
                guard: None,
                body: Box::new(block(103, vec![], Some(id_node(104, "x")))),
            },
        );
        let out = gen(&match_fn("get_x", vec![arm]));
        // The field bind must be introduced (statement-form `case Point(x=x):`),
        // not left free inside a `(lambda __v: … x …)` chain.
        assert!(
            out.contains("match p:") && out.contains("case Point(x=x):"),
            "plain-record value match must bind the field via case Point(x=x), got:\n{out}"
        );
        assert!(
            !out.contains("(lambda __v: x)"),
            "must not emit the field name free inside a value lambda, got:\n{out}"
        );
        // Inject a real `Point` dataclass (after the leading `from __future__`
        // line, which must stay first) so `case Point(x=x):` has a class to bind.
        let stubbed = out.replacen(
            "from __future__ import annotations\n",
            "from __future__ import annotations\nfrom dataclasses import dataclass as _dc\n@_dc\nclass Point:\n    x: int = 0\n",
            1,
        );
        assert!(
            !has_python3() || check_py_syntax(&stubbed),
            "generated python must parse, got:\n{stubbed}"
        );
    }

    /// `n if (n < 0) => "neg"  _ => "nonneg"` — a guard arm in value position.
    /// The guard test must survive (the legacy chain dropped it, so every input
    /// took the first arm).
    #[test]
    fn py_matcharm_guard_value_position_keeps_guard() {
        let guarded = node(
            200,
            NodeKind::MatchArm {
                pattern: Box::new(bind_pat(201, "n")),
                guard: Some(Box::new(node(
                    202,
                    NodeKind::BinaryOp {
                        op: BinOp::Lt,
                        left: Box::new(id_node(203, "n")),
                        right: Box::new(int_lit(204, "0")),
                    },
                ))),
                body: Box::new(block(205, vec![], Some(str_lit(206, "neg")))),
            },
        );
        let default = node(
            210,
            NodeKind::MatchArm {
                pattern: Box::new(node(211, NodeKind::WildcardPat)),
                guard: None,
                body: Box::new(block(212, vec![], Some(str_lit(213, "nonneg")))),
            },
        );
        let out = gen(&match_fn("classify", vec![guarded, default]));
        assert!(
            out.contains("match p:") && out.contains("case n if (n < 0):"),
            "guard arm in value position must keep its guard test, got:\n{out}"
        );
        assert!(
            !has_python3() || check_py_syntax(&out),
            "generated python must parse, got:\n{out}"
        );
    }

    /// `(0, _) => "zero"  (n, s) => "${n}: ${s}"` — tuple patterns in value
    /// position must bind `n`/`s` and test the literal element.
    #[test]
    fn py_tuple_match_value_position_binds_and_tests() {
        let zero_arm = node(
            300,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    301,
                    NodeKind::TuplePat {
                        elems: vec![
                            node(
                                302,
                                NodeKind::LiteralPat {
                                    lit: Literal::Int("0".into()),
                                },
                            ),
                            node(303, NodeKind::WildcardPat),
                        ],
                    },
                )),
                guard: None,
                body: Box::new(block(304, vec![], Some(str_lit(305, "zero")))),
            },
        );
        let bind_arm = node(
            310,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    311,
                    NodeKind::TuplePat {
                        elems: vec![bind_pat(312, "n"), bind_pat(313, "s")],
                    },
                )),
                guard: None,
                body: Box::new(block(314, vec![], Some(id_node(315, "n")))),
            },
        );
        let out = gen(&match_fn("describe", vec![zero_arm, bind_arm]));
        assert!(
            out.contains("match p:")
                && out.contains("case (0, _):")
                && out.contains("case (n, s):"),
            "tuple value match must test the literal and bind elements, got:\n{out}"
        );
        assert!(
            !has_python3() || check_py_syntax(&out),
            "generated python must parse, got:\n{out}"
        );
    }

    /// `Some(Ok(n)) => "${n}"  …` — a nested constructor in value position must
    /// test the inner `Ok` and bind `n`, not collapse to `isinstance(__v, …)`
    /// with `n` free.
    #[test]
    fn py_nested_constructor_match_value_position_binds_inner() {
        let some_ok = node(
            400,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    401,
                    NodeKind::ConstructorPat {
                        path: type_path(&["Some"]),
                        fields: vec![node(
                            402,
                            NodeKind::ConstructorPat {
                                path: type_path(&["Ok"]),
                                fields: vec![bind_pat(403, "n")],
                            },
                        )],
                    },
                )),
                guard: None,
                body: Box::new(block(404, vec![], Some(id_node(405, "n")))),
            },
        );
        let none_arm = node(
            410,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    411,
                    NodeKind::ConstructorPat {
                        path: type_path(&["None"]),
                        fields: vec![],
                    },
                )),
                guard: None,
                body: Box::new(block(412, vec![], Some(str_lit(413, "none")))),
            },
        );
        let out = gen(&match_fn("nested", vec![some_ok, none_arm]));
        assert!(
            out.contains("match p:") && out.contains("case _BockSome(_BockOk(n)):"),
            "nested constructor value match must test+bind the inner Ok, got:\n{out}"
        );
        assert!(
            !has_python3() || check_py_syntax(&out),
            "generated python must parse, got:\n{out}"
        );
    }

    /// Q-py-valpos-stmt-arms: a value-position `match` arm whose body is a
    /// *block with leading statements* must run those statements and keep their
    /// bindings in scope for the tail. The calculator's chained-computation arm
    /// `Ok(sum) => { let step2 = …; <inner match> }` previously emitted
    /// `(lambda: <tail>)()`, dropping the `let step2` and leaving `step2`
    /// unbound at runtime. The fix folds the `let` into an immediately-applied
    /// lambda: `(lambda step2: <tail>)(<value>)`.
    #[test]
    fn py_valpos_match_arm_block_keeps_leading_let() {
        // Ok(n) => { let doubled = (n + n); doubled }
        let let_stmt = node(
            500,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(501, "doubled")),
                ty: None,
                value: Box::new(node(
                    502,
                    NodeKind::BinaryOp {
                        op: BinOp::Add,
                        left: Box::new(id_node(503, "n")),
                        right: Box::new(id_node(504, "n")),
                    },
                )),
            },
        );
        let ok_arm = node(
            510,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    511,
                    NodeKind::ConstructorPat {
                        path: type_path(&["Ok"]),
                        fields: vec![bind_pat(512, "n")],
                    },
                )),
                guard: None,
                body: Box::new(block(513, vec![let_stmt], Some(id_node(514, "doubled")))),
            },
        );
        let err_arm = node(
            520,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    521,
                    NodeKind::ConstructorPat {
                        path: type_path(&["Err"]),
                        fields: vec![bind_pat(522, "e")],
                    },
                )),
                guard: None,
                body: Box::new(block(523, vec![], Some(id_node(524, "e")))),
            },
        );
        let out = gen(&match_fn("keep_let", vec![ok_arm, err_arm]));
        assert!(
            out.contains("lambda doubled:"),
            "value-position match-arm block must keep its `let doubled` binding, got:\n{out}"
        );
        assert!(
            !out.contains("(lambda: "),
            "must not emit the statement-dropping `(lambda: <tail>)()` form, got:\n{out}"
        );
        assert!(
            !has_python3() || check_py_syntax(&out),
            "generated python must parse, got:\n{out}"
        );
    }
}
