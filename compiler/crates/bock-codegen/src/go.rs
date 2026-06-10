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

use bock_air::{
    AIRNode, AirInterpolationPart, EnumVariantPayload, NodeKind, ResultVariant, Visitor,
};
use bock_ast::{AssignOp, BinOp, Literal, TypeExpr, UnaryOp, Visibility};
use bock_types::AIRModule;

use crate::error::CodegenError;
use crate::generator::{CodeGenerator, GeneratedCode, OutputFile, SourceMap};
use crate::profile::TargetProfile;

/// Collects the value-identifier names *referenced* anywhere in a subtree.
///
/// Used by the Go emitter's unused-binding guard: Go rejects a `let`-bound local
/// that is never read (`declared and not used`), which Bock permits (a binding
/// kept only for its side effect, or shadowed later). After emitting such a
/// binding we append `_ = name` iff the name is not referenced in the rest of the
/// block — this collector computes that reference set.
///
/// Every [`NodeKind::Identifier`] is counted as a reference. Binding *patterns*
/// (`let x = …`'s `x`) are not identifiers, so they are correctly excluded — only
/// genuine *uses* land here. Conservative by construction: a name that appears in
/// any form (a nested closure, a struct-field value, an interpolation) is seen, so
/// the guard never silences a binding that is actually read.
struct IdentUseCollector {
    used: HashSet<String>,
}

impl Visitor for IdentUseCollector {
    fn visit_node(&mut self, node: &AIRNode) {
        if let NodeKind::Identifier { name } = &node.kind {
            self.used.insert(name.name.clone());
        }
        bock_air::visitor::walk_node(self, node);
    }
}

/// The set of value-identifier names referenced anywhere in `node` (see
/// [`IdentUseCollector`]).
fn collect_used_idents(node: &AIRNode) -> HashSet<String> {
    let mut c = IdentUseCollector {
        used: HashSet::new(),
    };
    c.visit_node(node);
    c.used
}

/// Conservative module scan for `Channel` / `spawn` references.
fn go_module_uses_concurrency(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Channel\"") || s.contains("\"spawn\"")
    })
}

/// Whether a Go loop needs a label so a statement-arm `match`'s `break`/
/// `continue` can target the loop instead of the inner `switch`.
///
/// A label is only required when the jumping `match` lowers to a Go `switch`
/// (where a bare `break` would exit the switch, not the loop). An `Optional`
/// `match` lowers to an `if __opt.tag == "Some" { ... } else { ... }` chain
/// instead — a bare `break`/`continue` there already targets the enclosing
/// `for`, so labelling it produces a *defined-and-not-used* label that Go
/// rejects. This refines the shared [`crate::generator::loop_needs_break_label`]
/// for Go's lowering: it returns true only when a non-Optional statement-arm
/// `match` with a `break`/`continue` is present (not nested under another loop).
fn go_loop_needs_label(body: &AIRNode) -> bool {
    /// Does `node` perform a loop `break`/`continue` reachable from a match arm
    /// without crossing into a nested loop or function?
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
    /// Find a *switch*-lowered (non-Optional) statement-arm match that jumps the
    /// loop, not crossing into a nested loop or function.
    fn find(node: &AIRNode) -> bool {
        match &node.kind {
            NodeKind::For { .. }
            | NodeKind::While { .. }
            | NodeKind::Loop { .. }
            | NodeKind::FnDecl { .. }
            | NodeKind::Lambda { .. } => false,
            NodeKind::Match { arms, .. } => {
                // Optional/Result matches lower to if/else where bare
                // break/continue already target the loop — no label needed for
                // *this* match.
                let this_needs_label = !go_match_is_optional(arms)
                    && !go_match_is_result(arms)
                    && crate::generator::match_has_statement_arm(arms)
                    && arms.iter().any(|a| {
                        matches!(&a.kind, NodeKind::MatchArm { body, .. } if arm_has_jump(body))
                    });
                // Even a non-jumping (or Optional) match may *contain* a nested
                // switch-lowered match that jumps the loop, so always recurse
                // into the arms.
                this_needs_label
                    || arms
                        .iter()
                        .any(|a| matches!(&a.kind, NodeKind::MatchArm { body, .. } if find(body)))
            }
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

/// Decide whether a Go `match` should lower to a *type*-switch
/// (`switch v := s.(type) { case T: }`) rather than a *value*-switch
/// (`switch s { case 5: }`).
///
/// Constructor and record patterns dispatch on the scrutinee's dynamic type
/// (enum variants are distinct Go structs), so any such pattern forces a
/// type-switch. Literal and bind patterns dispatch on value. A match whose
/// arms are only wildcard/bind patterns defaults to a value-switch.
fn go_match_is_type_switch(arms: &[AIRNode]) -> bool {
    arms.iter().any(|arm| {
        matches!(
            &arm.kind,
            NodeKind::MatchArm { pattern, .. }
                if matches!(
                    pattern.kind,
                    NodeKind::ConstructorPat { .. } | NodeKind::RecordPat { .. }
                )
        )
    })
}

/// True if any arm is a catch-all (`_` or a bind pattern), which lowers to a Go
/// `default:` case.
fn go_match_has_default_arm(arms: &[AIRNode]) -> bool {
    arms.iter().any(|arm| {
        matches!(
            &arm.kind,
            NodeKind::MatchArm { pattern, .. }
                if matches!(pattern.kind, NodeKind::WildcardPat | NodeKind::BindPat { .. })
        )
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

/// Runtime helpers for Bock `Optional[T]` in Go. Go has no sum type, so an
/// optional is a tagged struct: `tag` is `"Some"` or `"None"`, `v` carries the
/// payload for `Some`. `__bockSome`/`__bockNone` are the constructors; matches
/// dispatch on `.tag` and read `.v` for the bound value.
///
/// `__bockAsInt64` / `__bockAsFloat64` recover a numeric payload from the
/// `interface{}` box. Bock's `Int`/`Float` are Go `int64`/`float64`, but a
/// payload constructed from an *untyped Go constant* — e.g. `Some(10)` →
/// `__bockSome(10)` — boxes a Go `int` (the default type of an untyped integer
/// constant), not an `int64`. A hard `.(int64)` assertion on that box panics
/// (`interface {} is int, not int64`). These helpers widen the common numeric
/// boxings instead, so a `Some(x)` payload bound for typed use works whether it
/// came from a literal, a typed variable, or arithmetic.
const OPTIONAL_RUNTIME_GO: &str = "// ── Bock Optional runtime ──
type __bockOption struct {
	tag string
	v   interface{}
}

func __bockSome(v interface{}) __bockOption { return __bockOption{tag: \"Some\", v: v} }

var __bockNone = __bockOption{tag: \"None\"}
";

/// Shared numeric-widening helpers used by both the `Optional` and `Result`
/// runtimes to recover an `int64`/`float64` payload from the `interface{}` box.
///
/// A payload constructed from an *untyped Go constant* — e.g. `Some(10)` /
/// `Ok(10)` → `__bockSome(10)` / `__bockOk(10)` — boxes a Go `int` (the default
/// type of an untyped integer constant), not an `int64`. A hard `.(int64)`
/// assertion on that box panics (`interface {} is int, not int64`). These helpers
/// widen the common numeric boxings instead. Emitted once if *either* container
/// runtime is used (its own emit flag), so the two runtimes never redeclare them.
const NUMERIC_RUNTIME_GO: &str = "// ── Bock numeric payload helpers ──
func __bockAsInt64(v interface{}) int64 {
	switch n := v.(type) {
	case int64:
		return n
	case int:
		return int64(n)
	case int32:
		return int64(n)
	case float64:
		return int64(n)
	default:
		return 0
	}
}

func __bockAsFloat64(v interface{}) float64 {
	switch n := v.(type) {
	case float64:
		return n
	case float32:
		return float64(n)
	case int64:
		return float64(n)
	case int:
		return float64(n)
	default:
		return 0
	}
}
";

/// Runtime for Bock `Result[T, E]` in Go. Mirrors `OPTIONAL_RUNTIME_GO`: a
/// tagged struct (`tag` is `"Ok"`/`"Err"`, `v` carries the payload), with
/// `__bockOk`/`__bockErr` constructors. A `match r { Ok(v) => …; Err(e) => … }`
/// dispatches on `.tag` and reads `.v` for the bound value — the same tag-switch
/// the Optional match uses, not the user-enum type-switch (`case Ok:` against an
/// undefined Go type) the broken codegen produced.
const RESULT_RUNTIME_GO: &str = "// ── Bock Result runtime ──
type __bockResult struct {
	tag string
	v   interface{}
}

func __bockOk(v interface{}) __bockResult { return __bockResult{tag: \"Ok\", v: v} }

func __bockErr(v interface{}) __bockResult { return __bockResult{tag: \"Err\", v: v} }
";

/// Runtime helper for Bock range expressions (`0..n` / `0..=n`) in Go. Go has
/// no native range *value*, so `for i in 0..n` lowers to
/// `for _, i := range __bockRange(0, n, false)`; this builds the `[]int64`
/// slice with half-open (`inclusive=false`) or inclusive (`inclusive=true`)
/// bounds, matching Python's `range(lo, hi)` / `range(lo, hi + 1)` and Rust's
/// `lo..hi` / `lo..=hi`. Emitted once into the shared `bock_runtime.go`
/// (per-module path) or inlined at most once (single-module path), gated on a
/// ctx flag (mirrors `OPTIONAL_RUNTIME_GO`).
const RANGE_RUNTIME_GO: &str = "// ── Bock range runtime ──
func __bockRange(lo int64, hi int64, inclusive bool) []int64 {
	end := hi
	if inclusive {
		end = hi + 1
	}
	r := make([]int64, 0)
	for i := lo; i < end; i++ {
		r = append(r, i)
	}
	return r
}
";

/// Integer exponentiation helper for the `**` operator on integer operands.
/// Go has no `**` and `math.Pow` returns `float64` (losing integer precision and
/// type), so an `Int ** Int` lowers to a call to this helper, which does
/// fast exponentiation-by-squaring and stays in `int64`. A negative exponent
/// yields `0` (an integer power with a negative exponent has no `int64` value;
/// Bock callers using fractional results use `Float ** Float`, which routes to
/// `math.Pow`). Gated by [`go_module_uses_int_pow`] so it is emitted only when a
/// `**` with non-float operands is present.
const INT_POW_RUNTIME_GO: &str = "// ── Bock integer-power runtime ──
func __bockIntPow(base int64, exp int64) int64 {
	if exp < 0 {
		return 0
	}
	result := int64(1)
	for exp > 0 {
		if exp&1 == 1 {
			result *= base
		}
		base *= base
		exp >>= 1
	}
	return result
}
";

/// True if the module references a `Range` node anywhere (so the range runtime
/// helper must be emitted). Mirrors [`go_module_uses_optional`]. `RangePat`
/// (a match-arm range pattern) does not contain the `Range {` substring, so it
/// is not matched — the helper is only needed for range *values*.
fn go_module_uses_range(items: &[AIRNode]) -> bool {
    items.iter().any(|n| format!("{n:?}").contains("Range {"))
}

/// True if the module contains any `**` (`BinOp::Pow`) operator (so the
/// integer-power runtime helper must be emitted). The float path lowers to
/// `math.Pow`; the int path calls [`INT_POW_RUNTIME_GO`]'s `__bockIntPow`. We
/// emit the helper whenever *any* `**` is present (Go tolerates an unused
/// package-level func, so a float-only program harmlessly carries it), rather
/// than re-deriving operand types here. Mirrors [`go_module_uses_range`]'s
/// structural debug scan: a `BinaryOp { op: Pow` renders that substring.
fn go_module_uses_int_pow(items: &[AIRNode]) -> bool {
    items.iter().any(|n| format!("{n:?}").contains("op: Pow"))
}

/// Runtime helper for the DQ29 `"deep"` equality lane: `==`/`!=` whose operand
/// (transitively) involves a `List`/`Map`/`Set` — Go has no `==` for slices or
/// maps ("can only be compared to nil", a compile error). `reflect.DeepEqual`
/// gives exactly the §18.5 semantics: element-wise for slices, content-based
/// and ORDER-INDEPENDENT for maps (Bock `Map` and `Set` both lower to Go
/// maps), recursive through struct fields, and IEEE for floats (`NaN`-holding
/// values are not deeply equal — the DQ10 caveat). Needs `import "reflect"`
/// wherever it is emitted. The shallow lanes never route here: Go struct /
/// interface `==` is already field-wise.
const DEEP_EQ_RUNTIME_GO: &str = "// ── Bock structural equality runtime ──
func __bockDeepEq(a any, b any) bool {
	return reflect.DeepEqual(a, b)
}
";

/// True if the module contains a `"deep"`-lane equality (so
/// [`DEEP_EQ_RUNTIME_GO`] must be emitted and `\"reflect\"` imported). Mirrors
/// [`go_module_uses_range`]'s structural debug scan over the checker's
/// `user_eq` metadata stamp.
fn go_module_uses_deep_eq(items: &[AIRNode]) -> bool {
    items
        .iter()
        .any(|n| format!("{n:?}").contains("\"user_eq\": String(\"deep\")"))
}

/// True if the module references `Optional`, `Some`, or `None` anywhere (so the
/// Optional runtime prelude must be emitted). A cheap structural scan over the
/// debug rendering, mirroring `go_module_uses_concurrency`.
fn go_module_uses_optional(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Optional\"")
            || s.contains("TypeOptional")
            || s.contains("\"Some\"")
            || s.contains("\"None\"")
    })
}

/// True if the module references `Result`, `Ok`, or `Err` anywhere (so the
/// `Result` runtime prelude must be emitted). Mirrors [`go_module_uses_optional`].
fn go_module_uses_result(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Result\"")
            || s.contains("ResultConstruct")
            || s.contains("\"Ok\"")
            || s.contains("\"Err\"")
    })
}

/// The prelude `Ordering` runtime: a small enum type with the three variants as
/// package-level constants, plus a generic `compare` helper the primitive bridge
/// calls. Mirrors `OPTIONAL_RUNTIME_GO` — when the `core.compare` enum decl is
/// not among the reached modules, `Ordering`/`Less`/`Equal`/`Greater` and
/// `(x).compare(y)` need this self-contained representation. A value-switch
/// `case Less:` (the existing Go match lowering for these arms) matches a
/// `__bockOrdering` constant directly.
const ORDERING_RUNTIME_GO: &str = "// ── Bock Ordering runtime ──
type __bockOrdering int

const (
	Less __bockOrdering = iota - 1
	Equal
	Greater
)

func __bockCompare[T int64 | float64 | string | rune | int | uint64 | float32](a, b T) __bockOrdering {
	if a < b {
		return Less
	}
	if a == b {
		return Equal
	}
	return Greater
}
";

/// The `__bockOrdered` constraint a `[T: Comparable]` sealed-core bound lowers to
/// (GAP-C): the ordered primitive type-set, so a generic fn's `a.compare(b)` /
/// `a > b` can use `<`/`==`/`>`. Self-contained (no `cmp` import), matching
/// `__bockCompare`'s set. Emitted independently of the rest of the Ordering
/// runtime: a `[T: Comparable]`-bounded fn (`max_of[T: Comparable]`) needs the
/// constraint even when the module never references `Ordering`/`compare` (which
/// is what gates [`ORDERING_RUNTIME_GO`]). Deduped against that block so the type
/// is never defined twice.
const ORDERED_CONSTRAINT_GO: &str = "// ── Bock ordered constraint ──
type __bockOrdered interface {
	~int64 | ~float64 | ~string | ~rune | ~int | ~uint64 | ~float32
}
";

/// True if the module references the prelude `Ordering` enum, any of its
/// variants, or a `compare` method call (lowered to an `Ordering` runtime
/// value). Gates emission of [`ORDERING_RUNTIME_GO`], mirroring
/// [`go_module_uses_optional`].
fn go_module_uses_ordering(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Ordering\"")
            || s.contains("\"Less\"")
            || s.contains("\"Equal\"")
            || s.contains("\"Greater\"")
            || s.contains("\"compare\"")
    })
}

/// True if a `match`\'s arms dispatch on the prelude `Ordering` variants
/// (`Less`/`Equal`/`Greater`), so the Go backend emits a *value*-switch over the
/// `__bockOrdering` constants rather than the type-switch it uses for user
/// enums. Recognised by any constructor pattern whose final segment is an
/// `Ordering` variant.
fn go_match_is_ordering(arms: &[AIRNode]) -> bool {
    arms.iter().any(|arm| {
        if let NodeKind::MatchArm { pattern, .. } = &arm.kind {
            if let NodeKind::ConstructorPat { path, .. } = &pattern.kind {
                return path
                    .segments
                    .last()
                    .and_then(|s| crate::generator::ordering_variant(&s.name))
                    .is_some();
            }
        }
        false
    })
}

/// True if a `match`\'s arms dispatch on the `Optional` constructors
/// `Some`/`None` (so the Go backend emits a tag-based switch over
/// `__bockOption`). Recognised by a constructor pattern whose final path
/// segment is `Some` or `None`.
fn go_match_is_optional(arms: &[AIRNode]) -> bool {
    arms.iter().any(|arm| {
        if let NodeKind::MatchArm { pattern, .. } = &arm.kind {
            if let NodeKind::ConstructorPat { path, .. } = &pattern.kind {
                return path
                    .segments
                    .last()
                    .is_some_and(|seg| matches!(seg.name.as_str(), "Some" | "None"));
            }
        }
        false
    })
}

/// True if a `match`'s arms dispatch on the `Result` constructors `Ok`/`Err`
/// (so the Go backend emits a tag-based switch over `__bockResult`, mirroring
/// [`go_match_is_optional`]). Without this, an `Ok`/`Err` constructor pattern
/// would route to the user-enum type-switch (`case Ok:` against an undefined Go
/// type) — the defect this fixes.
fn go_match_is_result(arms: &[AIRNode]) -> bool {
    arms.iter().any(|arm| {
        if let NodeKind::MatchArm { pattern, .. } = &arm.kind {
            if let NodeKind::ConstructorPat { path, .. } = &pattern.kind {
                return path
                    .segments
                    .last()
                    .is_some_and(|seg| matches!(seg.name.as_str(), "Ok" | "Err"));
            }
        }
        false
    })
}

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
        // Shared pre-pass: hoist value-position diverging control flow (see
        // `hoist_value_cf`) into declare-then-assign temp blocks.
        let module =
            &crate::generator::hoist_value_cf(crate::generator::lower_blanket_into(module.clone()));
        let mut ctx = GoEmitCtx::new();
        ctx.enum_variants =
            crate::generator::collect_enum_variants(&[(module, std::path::Path::new(""))]);
        ctx.generic_decls =
            crate::generator::collect_generic_decls(&[(module, std::path::Path::new(""))]);
        ctx.collect_record_param_fields(module);
        ctx.collect_async_fns(module);
        ctx.collect_methods(module);
        ctx.collect_type_aliases(module);
        ctx.collect_optional_returns(module);
        ctx.collect_method_optional_returns(module);
        // `trait_decls` must precede `collect_fn_and_type_names` so the latter can
        // record which generic fns carry a *sealed-core* bound lowered to a Go
        // built-in constraint (GAP-C — `fn_sealed_bound`).
        ctx.trait_decls =
            crate::generator::collect_trait_decls(&[(module, std::path::Path::new(""))]);
        ctx.const_names =
            crate::generator::collect_const_names(&[(module, std::path::Path::new(""))]);
        ctx.collect_fn_and_type_names(module);
        ctx.derive_self_param_traits();
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

    /// Emit a per-module **native Go package tree** (spec §20.6.1; DQ19
    /// resolved): each module the entry program reaches through a real `use` is
    /// emitted to its **own** `.go` file under `build/go/`, all in one
    /// `package main`.
    ///
    /// ## Package model (flat, single `package main`)
    ///
    /// Go requires exactly one package per directory, and same-package symbols
    /// are visible across files **without** any import. So the cleanest model
    /// that is genuinely per-file and runs via `go run .` keeps every emitted
    /// file in `build/go/` as `package main`: a function/record/enum declared
    /// in `core.option`'s file is referenced directly from `main`'s file, no
    /// inter-file import. The flat layout (filenames flatten the dotted module
    /// path — `module core.option` ⇒ `core.option.go`) avoids the subdirectory
    /// that would make Go treat a module as a *separate* package. §20.6.1 allows
    /// "the target ecosystem's conventions," and one package across files is
    /// Go's. (Project mode — S6 — may refine this toward real subpackages with
    /// capitalized exports.)
    ///
    /// `ImportDecl`s therefore emit nothing (same package). The runtime preludes
    /// (Optional / Result / numeric / Ordering / concurrency / range) are
    /// emitted **once** into a shared `bock_runtime.go`; consuming files use the
    /// runtime symbols directly (same package). The minimal `go.mod` (module
    /// name + go version) run affordance is emitted by the **scaffolder** in
    /// project mode (S6a / DV18), not by codegen, so `go run .` resolves the
    /// package. Go uses a native `func main`, so no entry invocation is appended.
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
        // itself), dependency-ordered — never the prelude-only stdlib.
        let reachable = crate::generator::reachable_modules(modules);
        let modules = reachable.as_slice();
        if modules.is_empty() {
            return Ok(GeneratedCode { files: vec![] });
        }

        let entry_idx = modules
            .iter()
            .position(|(m, _)| crate::generator::module_declares_main_fn(m))
            .unwrap_or(modules.len() - 1);

        // Pre-scan async fns across ALL modules so cross-module calls between
        // async functions route through the Async-suffix wrappers.
        let mut global_async_fns: HashSet<String> = HashSet::new();
        for (module, _) in modules {
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

        // A template ctx carries the program-wide analysis (enum variants,
        // generics, trait/method/Optional-return metadata) collected across the
        // whole reachable set so a reference in one file to a symbol declared in
        // another lowers identically to the bundling path. Each per-module ctx
        // is forked from it.
        let mut template = GoEmitCtx::new();
        template.async_fns = global_async_fns;
        template.enum_variants = crate::generator::collect_enum_variants(modules);
        template.generic_decls = crate::generator::collect_generic_decls(modules);
        template.trait_decls = crate::generator::collect_trait_decls(modules);
        template.const_names = crate::generator::collect_const_names(modules);
        template.derive_self_param_traits();
        // Aliases first across the whole reachable set: a fn in one module may
        // return a `type` alias declared in another, and the Optional/Result
        // return scan below must see through it.
        for (module, _) in modules {
            template.collect_type_aliases(module);
        }
        for (module, _) in modules {
            template.collect_methods(module);
            template.collect_optional_returns(module);
            template.collect_method_optional_returns(module);
            template.collect_record_param_fields(module);
            template.collect_fn_and_type_names(module);
        }
        // Effect-op resolution needs the whole reachable set: a bare op in one
        // module may belong to an effect declared in another (§10 + DV13).
        template.seed_effect_registries(modules);

        let mut files: Vec<OutputFile> = Vec::with_capacity(modules.len() + 2);
        for (i, (module, source_path)) in modules.iter().enumerate() {
            let mut ctx = template.fork();
            ctx.per_module = true;
            ctx.emit_node(module)?;
            let (body, needs) = ctx.into_parts();

            // Each per-module file is `package main` with its own per-file
            // `import (...)` block (Go imports are per-file).
            let mut content = "package main\n".to_string();
            content.push_str(&needs.render_block());
            content.push('\n');
            content.push_str(&body);

            let rel = if i == entry_idx {
                std::path::PathBuf::from("main.go")
            } else {
                go_module_filename(module, source_path, self.target())
            };
            let generated_file = rel
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            files.push(OutputFile {
                path: rel,
                content,
                source_map: Some(SourceMap {
                    generated_file,
                    ..Default::default()
                }),
            });
        }

        // Shared runtime file: emit exactly the preludes the whole program uses,
        // once, in their own `package main` file (same package → visible to all).
        if let Some(runtime) = self.build_runtime_file(modules, &template) {
            files.push(OutputFile {
                path: std::path::PathBuf::from("bock_runtime.go"),
                content: runtime,
                source_map: None,
            });
        }

        // Manifest emission moved to the project-mode scaffolder (S6a / DV18):
        // codegen emits only the per-module *source* package in all modes; the
        // `go.mod` run affordance is emitted by `GoScaffolder` in project mode
        // only (never under `--source-only`). See `scaffold.rs`.

        Ok(GeneratedCode { files })
    }

    /// Transpile `@test` functions into a `bock_test.go` file (S7).
    ///
    /// `go test` runs `func TestXxx(t *testing.T)` in `package main` (same
    /// package → the test can call the program's unexported functions). Each
    /// Bock `@test` becomes one such function, with `expect(...)` assertion
    /// chains lowered to `if <neg> { t.Errorf(...) }`. `framework` is ignored:
    /// `go test` (stdlib `testing`) is the universal Go framework (§20.6.2).
    fn generate_tests(
        &self,
        modules: &[(&AIRModule, &std::path::Path)],
        _framework: &str,
    ) -> Result<crate::generator::TestArtifacts, CodegenError> {
        let reachable = crate::generator::reachable_modules(modules);
        let modules = reachable.as_slice();
        let tests = crate::generator::collect_test_fns(modules);
        if tests.is_empty() {
            return Ok(crate::generator::TestArtifacts::default());
        }

        // Same program-wide analysis `generate_project` builds, so test bodies
        // lower references (function casing, enum variants, Optional returns)
        // identically to the runtime package.
        let mut global_async_fns: HashSet<String> = HashSet::new();
        for (module, _) in modules {
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
        let mut template = GoEmitCtx::new();
        template.async_fns = global_async_fns;
        template.enum_variants = crate::generator::collect_enum_variants(modules);
        template.generic_decls = crate::generator::collect_generic_decls(modules);
        template.trait_decls = crate::generator::collect_trait_decls(modules);
        template.const_names = crate::generator::collect_const_names(modules);
        template.derive_self_param_traits();
        // Aliases first across the whole reachable set: a fn in one module may
        // return a `type` alias declared in another, and the Optional/Result
        // return scan below must see through it.
        for (module, _) in modules {
            template.collect_type_aliases(module);
        }
        for (module, _) in modules {
            template.collect_methods(module);
            template.collect_optional_returns(module);
            template.collect_method_optional_returns(module);
            template.collect_record_param_fields(module);
            template.collect_fn_and_type_names(module);
        }
        template.seed_effect_registries(modules);

        let mut ctx = template.fork();
        ctx.per_module = true;
        for (test_fn, _module_path) in &tests {
            let NodeKind::FnDecl { name, body, .. } = &test_fn.kind else {
                continue;
            };
            let go_name = go_test_fn_name(&name.name);
            ctx.buf.push('\n');
            ctx.writeln(&format!("func {go_name}(t *testing.T) {{"));
            ctx.indent += 1;
            ctx.emit_go_test_body(body)?;
            ctx.indent -= 1;
            ctx.writeln("}");
        }

        let (body, needs) = ctx.into_parts();
        // The test file is `package main`; build its import block (testing plus
        // whatever the test bodies pull in — fmt/strings/…). gofmt-sorted order.
        let mut imports: Vec<&str> = vec!["\"testing\""];
        if needs.fmt {
            imports.push("\"fmt\"");
        }
        if needs.strconv {
            imports.push("\"strconv\"");
        }
        if needs.strings {
            imports.push("\"strings\"");
        }
        if needs.sync {
            imports.push("\"sync\"");
        }
        if needs.time {
            imports.push("\"time\"");
        }
        if needs.utf8 {
            imports.push("\"unicode/utf8\"");
        }
        imports.sort_unstable();
        let mut content = String::from("package main\n\nimport (\n");
        for imp in &imports {
            content.push_str(&format!("\t{imp}\n"));
        }
        content.push_str(")\n");
        content.push_str(&body);

        Ok(crate::generator::TestArtifacts {
            files: vec![OutputFile {
                path: std::path::PathBuf::from("bock_test.go"),
                content,
                source_map: None,
            }],
            entry_append: None,
        })
    }
}

/// The flat output filename for one non-entry module in the per-module Go
/// package: the declared dotted module-path kept verbatim (`module core.option`
/// ⇒ `core.option.go`), so every emitted file lives directly in `build/go/`
/// (one package per directory — no subdirectory, which Go would treat as a
/// separate package). A module with no declared path falls back to its
/// source-mirrored file name.
///
/// The dots are **kept** (not flattened to `_`) deliberately: Go reserves the
/// `_test.go` filename suffix for test files (excluded from a normal `go build`
/// / `go run .`), so `module core.test` flattened to `core_test.go` would
/// silently vanish from the build. `core.test.go` does not match `_test.go` and
/// compiles as an ordinary package file. (Go also reserves `_GOOS.go` /
/// `_GOARCH.go` suffixes, which the dot form likewise avoids.)
fn go_module_filename(
    module: &AIRModule,
    source_path: &std::path::Path,
    target: &TargetProfile,
) -> std::path::PathBuf {
    match crate::generator::module_path_string(module) {
        Some(path) if !path.is_empty() => std::path::PathBuf::from(format!("{path}.go")),
        _ => crate::generator::derive_output_path(source_path, target)
            .file_name()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("module.go")),
    }
}

impl GoGenerator {
    /// Build the shared `bock_runtime.go` for the per-module path: `package main`
    /// plus exactly the runtime preludes any reached module references, emitted
    /// once (a duplicate `type __bockOption` / `__bockChannel` across files would
    /// not compile). Returns `None` when no prelude is needed.
    ///
    /// The selection mirrors the bundling path's per-prelude gating:
    /// numeric helpers are emitted when either container runtime is present
    /// (both use them); the bespoke int-`Ordering` runtime is emitted only when
    /// the real `core.compare.Ordering` enum is NOT reachable (otherwise that
    /// user enum is authoritative and the int runtime would be dead + shadow it).
    fn build_runtime_file(
        &self,
        modules: &[(&AIRModule, &std::path::Path)],
        template: &GoEmitCtx,
    ) -> Option<String> {
        let mut uses_concurrency = false;
        let mut uses_optional = false;
        let mut uses_result = false;
        let mut uses_ordering = false;
        let mut uses_range = false;
        let mut uses_int_pow = false;
        let mut uses_deep_eq = false;
        for (module, _) in modules {
            if let NodeKind::Module { items, .. } = &module.kind {
                uses_concurrency |= go_module_uses_concurrency(items);
                uses_optional |= go_module_uses_optional(items);
                uses_result |= go_module_uses_result(items);
                uses_ordering |= go_module_uses_ordering(items);
                uses_range |= go_module_uses_range(items);
                uses_int_pow |= go_module_uses_int_pow(items);
                uses_deep_eq |= go_module_uses_deep_eq(items);
            }
        }
        // The real `core.compare.Ordering` enum is authoritative when reachable
        // (its `Less` is a registered user variant in the shared registry).
        let ordering_enum_reachable = template
            .enum_variants
            .get("Less")
            .is_some_and(|info| info.enum_name == "Ordering");

        let emit_ordering = uses_ordering && !ordering_enum_reachable;
        // A `[T: Comparable]`-bounded generic fn lowers `T` to `T __bockOrdered`
        // (GAP-C). That constraint type must be defined even when the program
        // never references `Ordering`/`compare` (which gates the rest of the
        // Ordering runtime). `fn_sealed_bound` is populated on the template's
        // program-wide pre-scan.
        let emit_ordered_constraint = !template.fn_sealed_bound.is_empty();
        if !(uses_concurrency
            || uses_optional
            || uses_result
            || uses_range
            || uses_int_pow
            || uses_deep_eq
            || emit_ordering
            || emit_ordered_constraint)
        {
            return None;
        }

        let mut content = String::from("package main\n\n");
        // `__bockDeepEq` is the only runtime helper with a stdlib dependency;
        // its import lives here (Go imports are per-file, and the consuming
        // modules only *call* the helper).
        if uses_deep_eq {
            content.push_str("import \"reflect\"\n\n");
        }
        if uses_concurrency {
            content.push_str(CONCURRENCY_RUNTIME_GO);
            content.push('\n');
        }
        if uses_optional {
            content.push_str(OPTIONAL_RUNTIME_GO);
            content.push('\n');
        }
        if uses_result {
            content.push_str(RESULT_RUNTIME_GO);
            content.push('\n');
        }
        if uses_optional || uses_result {
            content.push_str(NUMERIC_RUNTIME_GO);
            content.push('\n');
        }
        if emit_ordering {
            content.push_str(ORDERING_RUNTIME_GO);
            content.push('\n');
        }
        // The `__bockOrdered` constraint: needed by a sealed-bound generic fn,
        // and (since it was split out of the Ordering block) also whenever the
        // Ordering runtime itself is emitted, so a `compare`-using generic still
        // resolves it. Emitted once — `emit_ordering` no longer carries it.
        if emit_ordered_constraint || emit_ordering {
            content.push_str(ORDERED_CONSTRAINT_GO);
            content.push('\n');
        }
        if uses_range {
            content.push_str(RANGE_RUNTIME_GO);
            content.push('\n');
        }
        if uses_int_pow {
            content.push_str(INT_POW_RUNTIME_GO);
            content.push('\n');
        }
        if uses_deep_eq {
            content.push_str(DEEP_EQ_RUNTIME_GO);
            content.push('\n');
        }
        // Each runtime block is joined with a trailing `\n`, which leaves a blank
        // line at EOF; gofmt wants exactly one terminating newline (§20.6.2
        // codegen-formatter agreement — the output must be gofmt-clean).
        let content = format!("{}\n", content.trim_end());
        Some(content)
    }
}

// ─── Emission context ────────────────────────────────────────────────────────

/// Internal state for Go emission.
///
/// `Clone` is derived so the per-module path ([`GoGenerator::generate_project`])
/// can pre-scan the whole program's cross-module analysis once into a template
/// ctx and [`GoEmitCtx::fork`] it per module file (resetting only the per-file
/// emission state). Every field is itself `Clone`.
#[derive(Clone)]
struct GoEmitCtx {
    buf: String,
    indent: usize,
    /// Track whether we need `"fmt"` import.
    needs_fmt_import: bool,
    /// Track whether we need `"sync"` import.
    needs_sync_import: bool,
    /// Track whether we need `"time"` import.
    needs_time_import: bool,
    /// Track whether we need `"strings"` import (String built-in methods).
    needs_strings_import: bool,
    /// Track whether we need `"unicode/utf8"` import (`String.len` scalar count).
    needs_utf8_import: bool,
    /// Track whether we need `"math"` import (numeric `Float` math methods).
    needs_math_import: bool,
    /// Track whether we need `"unicode"` import (`Char`/`trim_start`/`trim_end`
    /// predicates via `unicode.IsSpace`/`IsLetter`/`IsDigit`).
    needs_unicode_import: bool,
    /// Track whether we need `"strconv"` import (`Int.try_from`/`Float.try_from`
    /// string parsing via `strconv.ParseInt`/`strconv.ParseFloat`).
    needs_strconv_import: bool,
    /// Track whether we need `"reflect"` import (the DQ29 `__bockDeepEq`
    /// structural-equality helper, single-module path only — the per-module
    /// path imports it inside the shared `bock_runtime.go` instead).
    needs_reflect_import: bool,
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
    /// Names of `public` methods (declared in impl/class/trait blocks). Used at
    /// desugared method-call sites to pick PascalCase (public) vs camelCase
    /// (private) so the call matches the method definition's Go casing.
    public_methods: HashSet<String>,
    /// `(target type name, method name)` pairs that have an *inherent* (`impl
    /// Type { ... }`, no `trait_path`) or *class* method definition. A trait
    /// impl (`impl Trait for Type`) whose method merely forwards to the
    /// same-named inherent method (`fn render(self) { self.render() }`) is a
    /// redundant self-recursive forwarder in Go once the inherent method is
    /// exported to satisfy the interface directly — both would emit the same
    /// PascalCase Go name on the receiver, and the forwarder body's
    /// `self.render()` would resolve back to itself. Such a trait-impl method is
    /// skipped when an inherent definition already covers it. Keyed on the
    /// PascalCased Go method name (the trait-side casing) so a private inherent
    /// method exported via `public_methods` still matches.
    inherent_methods: HashSet<(String, String)>,
    /// PascalCased names of every record/class field declared in the program.
    /// Go forbids a struct having a field and a method with the same name, so a
    /// public method whose PascalCased Go name collides with a field name
    /// (e.g. `core.error`'s `SimpleError { message }` + `fn message(self)`) is
    /// suffixed `Method` by [`Self::go_method_name`] at the declaration (trait
    /// interface + receiver) and every call site so they agree.
    record_field_names: HashSet<String>,
    /// Loop-label stack. In Go, `break` inside a `switch` exits the switch, not
    /// an enclosing `for`. When a statement-arm `match` (lowered to a `switch`)
    /// contains a `break`/`continue` meant for the loop, the loop is given a
    /// label and the jump is emitted as `break <label>` / `continue <label>`.
    /// An entry is pushed for every active loop; `Some` once a label has been
    /// allocated for it. Only allocated labels are emitted (Go errors on an
    /// unused label).
    loop_labels: Vec<Option<String>>,
    /// When > 0, `break`/`continue` are being emitted inside a `switch` arm and
    /// must target the innermost labelled loop rather than the switch.
    switch_label_depth: usize,
    /// Monotonic counter for unique loop-label names.
    loop_label_counter: usize,
    /// Monotonic counter for unique guard-let discriminant temp names
    /// (`__guard0`, `__guard1`, …), so two `guard (let …)` statements in the same
    /// block do not collide.
    guard_counter: usize,
    /// Monotonic counter for unique `?`-propagation temp names (`__try0`,
    /// `__try1`, …). Go has no native `?`; each propagate hoists the operand into
    /// a `__tryN` local before its unwrap-or-early-return lowering.
    try_counter: usize,
    /// Monotonic counter for unique tuple-destructuring-`let` temp names
    /// (`__tup0`, `__tup1`, …). Go has no tuple destructuring; a
    /// `let (a, b) = expr` hoists `expr` into a `__tupN` struct local and binds
    /// each name off its `.Field{i}`, so two such lets in one block do not collide.
    let_tuple_counter: usize,
    /// Depth of enclosing *expression-position* `loop` IIFEs (`let r = loop { …
    /// break <v> }`). Bock's `loop` is a value-producing expression whose
    /// `break <v>` yields the loop's value; Go's `for`+`break` carries no value.
    /// When this is > 0 the innermost loop body is the IIFE's body, so a
    /// `break <v>` lowers to `return <v>` (out of the IIFE), not a value-dropping
    /// `break`. Saved/restored around nested statement-position loops, whose
    /// `break <v>` is a different (still value-dropping) case.
    loop_expr_depth: usize,
    /// Maps a function name → the Go element type of its `Optional[T]` return
    /// (`int64` for `-> Int?`). Pre-scanned across the module so a `match`
    /// whose scrutinee is a call (`match next(it) { Some(x) => ... }`) can
    /// type-assert the bound payload. Functions not returning an Optional are
    /// absent.
    fn_optional_ret_elem: HashMap<String, String>,
    /// Maps an in-scope variable name → the Go element type of its `Optional[T]`
    /// (e.g. an `o: Int?` parameter or a `let o: Int? = ...` binding maps to
    /// `int64`). Lets a `match o { Some(x) => ... }` type-assert `__opt.v` to
    /// the concrete element type instead of leaving it `interface{}`. The Go
    /// Optional runtime stores the payload as `interface{}`, so without this
    /// assertion any typed use of the bound value (`x + 10`) fails Go
    /// compilation. Scoped per function body and restored on exit.
    var_optional_elem: HashMap<String, String>,
    /// Maps an in-scope variable name → its declared type-expression AIR node
    /// (an `Optional[Result[(Int, Int), String]]` param maps to that
    /// `TypeOptional`/`TypeNamed` node). The single-element `var_*_elem` maps
    /// only record the *one-level* peeled Go type, which is not enough to
    /// type-assert the payload of a *nested* constructor pattern: a
    /// `match v { Some(Ok((a, b))) => … }` must peel Optional → Result → Tuple
    /// to assert the boxed `interface{}` payload to its concrete tuple struct
    /// (`struct{ Field0 int64; Field1 int64 }`) before `.Field0` reads.
    /// Threaded through the pattern-bind/test recursion (peeling Optional on
    /// `Some`, Result on `Ok`/`Err`) so a nested tuple pattern lands on the
    /// concrete struct type. Scoped per function body and restored on exit.
    var_decl_type_node: HashMap<String, AIRNode>,
    /// Maps a *method* name → the Go element type of its `Optional[T]` return
    /// (`int64` for `fn next(self) -> Int?`). Pre-scanned across every
    /// impl/class/trait block so a `match` whose scrutinee is a method call
    /// (`match it.next() { Some(x) => ... }`, the shape `for x in <Iterable>`
    /// desugars to) can type-assert the bound payload. This is the method-call
    /// analogue of [`Self::fn_optional_ret_elem`]. Keyed by method name only
    /// (Go codegen sees the AIR, not the checker's per-type `method_types`); if
    /// two methods share a name but return different Optional element types, the
    /// entry is poisoned (left absent) so the payload falls back to the runtime
    /// `interface{}` — conservative, never wrong, only un-type-asserted.
    method_optional_ret_elem: HashMap<String, String>,
    /// Maps a method name → the concrete generic-record instantiation it returns
    /// (`("ListIterator", ["int64"])` for `Bag.iter() -> ListIterator[Int]`),
    /// for methods whose declared return type is a concrete generic-record
    /// apply (no remaining type params). Lets an *untyped* binding of such a
    /// call (`__it := bag.Iter()`, the `for x in <Iterable>` desugar) record the
    /// binding's record args ([`Self::var_record_type_args`]) so the
    /// subsequent `match __it.next() { Some(x) => ... }` resolves the generic
    /// `Optional[T]` payload to the concrete arg (`int64`) — `T` is undefined in
    /// the calling fn (`main`). Keyed by method name only; poisoned (left
    /// absent) on a name clash with disagreeing args, as
    /// [`Self::method_optional_ret_elem`].
    method_ret_record_args: HashMap<String, (String, Vec<String>)>,
    /// Maps a method name → its declared return type rendered as Go (`stock_value
    /// → "float64"`). Lets `infer_go_expr_type` resolve a `recv.method()` call's
    /// type so a `.map((p) => p.stock_value())` combinator sizes its result slice
    /// as `[]float64` (not the erased `[]interface{}` whose elements a later
    /// `fold`'s `acc + v` can't add). Keyed by method name only; poisoned (left
    /// absent) on a name clash with disagreeing Go return types — mirrors
    /// [`Self::method_optional_ret_elem`]. A return type still naming an in-scope
    /// generic param is skipped (it is the generic signature, not concrete).
    method_return_go_types: HashMap<String, String>,
    /// Maps an in-scope variable name bound to a lambda → that lambda's inferred
    /// Go return type (`clip_fn → "[]float64"` for `let clip_fn = (d) => clip(d,
    /// ..)`). Lets a compose desugar `normalize >> clip_fn` (lowered to
    /// `(__compose_x) => clip_fn(normalize(__compose_x))`) resolve its own return
    /// type from the outer local lambda `clip_fn`, so the emitted closure is
    /// `func(x []float64) []float64` rather than `func(x []float64) interface{}`
    /// (the latter not assignable to a `Fn(List[Float]) -> List[Float]` callee).
    /// Function-scoped, restored on body exit alongside `var_go_type`.
    var_lambda_ret: HashMap<String, String>,
    /// Maps an in-scope variable name → its concrete generic record
    /// instantiation `(base record name, concrete Go type-args)` — e.g. a `let
    /// c: ListIter[Int]` binding or a `c: Counter[Int]` parameter maps to
    /// `("ListIter", ["int64"])`. Used to resolve a method-call scrutinee's
    /// `Optional[T]` payload at a CONCRETE call site: `method_optional_ret_elem`
    /// stores the *generic* element (`"T"`, the record's type param), undefined
    /// in the concrete caller (`main`); this lets `match c.next() { Some(x) =>
    /// ... }` assert the payload to the instantiation's arg (`int64`) instead of
    /// the bare `T`. Scoped per function/method body and restored on exit.
    var_record_type_args: HashMap<String, (String, Vec<String>)>,
    /// Maps an in-scope variable name → the Go element type of its `List[T]`
    /// (e.g. a `let nums: List[Int] = ...` binding maps to `int64`). The
    /// read-only `List` built-ins `get`/`first`/`last` return `Optional[T]`
    /// whose payload is the list element; this lets a `match nums.get(i) {
    /// Some(x) => ... }` type-assert the `interface{}` payload to the element
    /// type, the same way [`Self::var_optional_elem`] handles direct
    /// `Optional[T]` bindings. Scoped per function body and restored on exit.
    var_list_elem: HashMap<String, String>,
    /// Maps an in-scope variable name → `(key_go_type, val_go_type)` of its
    /// `Map[K, V]` (e.g. a `let m: Map[String, Int] = ...` binding maps to
    /// `("string", "int64")`). The built-in `Map` methods lower to inline
    /// `func(__m map[K]V, …) …` closures whose parameter type must match the
    /// concretely-typed receiver `map[K]V`; this records the declared key/value
    /// Go types so the closure is well-typed (Go does not pass a `map[string]
    /// int64` where a `map[interface{}]interface{}` is expected). Scoped per
    /// function body and restored on exit (mirrors [`Self::var_list_elem`]).
    var_map_kv: HashMap<String, (String, String)>,
    /// Maps an in-scope variable name → the Go element type of its `Set[E]`
    /// (e.g. a `let s: Set[Int] = ...` binding maps to `int64`). The Set
    /// analogue of [`Self::var_map_kv`]: the built-in `Set` methods lower to
    /// inline closures over `map[E]struct{}`, so the element type must match the
    /// concretely-typed receiver. Scoped per function body and restored on exit.
    var_set_elem: HashMap<String, String>,
    /// Maps an in-scope variable name → `(ok_go_type, err_go_type)` of its
    /// `Result[T, E]` (e.g. an `r: Result[Int, String]` param maps to
    /// `("int64", "string")`). The Result analogue of [`Self::var_optional_elem`]:
    /// a `match r { Ok(v) => ...; Err(e) => ... }` type-asserts the `interface{}`
    /// payload to the concrete Ok/Err type rather than leaving it `interface{}`.
    /// Scoped per function body and restored on exit.
    var_result_elem: HashMap<String, (String, String)>,
    /// Maps a free-function name → `(ok_go_type, err_go_type)` of its
    /// `Result[T, E]` return, so a `match parse(s) { Ok(n) => ... }` on a call
    /// scrutinee type-asserts the bound payload. The Result analogue of
    /// [`Self::fn_optional_ret_elem`]; functions not returning a Result are absent.
    fn_result_ret_elem: HashMap<String, (String, String)>,
    /// Set once the concurrency runtime prelude has been emitted into `buf` in
    /// the single-module self-contained path ([`GoGenerator::generate_module`]),
    /// so a module referencing it more than once still inlines it at most once (a
    /// duplicate `type __bockChannel` would not compile). The per-module project
    /// path emits the runtime once into the shared `bock_runtime.go`.
    concurrency_runtime_emitted: bool,
    /// Set once the Optional runtime prelude has been emitted into `buf`;
    /// deduped exactly as [`Self::concurrency_runtime_emitted`].
    optional_runtime_emitted: bool,
    /// Set once the `Result` runtime prelude has been emitted; deduped exactly as
    /// [`Self::optional_runtime_emitted`].
    result_runtime_emitted: bool,
    /// Set once the shared numeric-payload helpers ([`NUMERIC_RUNTIME_GO`]) have
    /// been emitted. Emitted once if *either* the Optional or `Result` runtime is
    /// used, so the two never redeclare `__bockAsInt64`/`__bockAsFloat64`.
    numeric_runtime_emitted: bool,
    /// Set once the [`ORDERING_RUNTIME_GO`] prelude has been emitted; deduped
    /// exactly as [`Self::optional_runtime_emitted`].
    ordering_runtime_emitted: bool,
    /// Set once [`ORDERED_CONSTRAINT_GO`] (`__bockOrdered`) has been emitted in
    /// the single-file inline path; deduped exactly as
    /// [`Self::ordering_runtime_emitted`].
    ordered_constraint_emitted: bool,
    /// Set once the [`RANGE_RUNTIME_GO`] helper has been emitted; deduped exactly
    /// as [`Self::optional_runtime_emitted`] (a duplicate `func __bockRange`
    /// would not compile).
    range_runtime_emitted: bool,
    /// Set once the [`INT_POW_RUNTIME_GO`] helper (`__bockIntPow`) has been
    /// emitted; deduped exactly as [`Self::range_runtime_emitted`] (a duplicate
    /// `func __bockIntPow` would not compile).
    int_pow_runtime_emitted: bool,
    /// Set once the [`DEEP_EQ_RUNTIME_GO`] helper (`__bockDeepEq`) has been
    /// emitted; deduped exactly as [`Self::range_runtime_emitted`].
    deep_eq_runtime_emitted: bool,
    /// User-enum-variant registry (DV14). Go has no sum type, so a user enum is
    /// a sealed interface + per-variant structs named `{enum}{variant}`
    /// (e.g. `ShapeCircle`). The registry lets a construction emit the variant
    /// struct literal and a `match` emit a *type-switch* (`switch __v :=
    /// s.(type) { case ShapeCircle: … }`) with field extraction, rather than the
    /// broken value-switch on the unqualified variant name. Built-in
    /// Optional/Result pre-seeds are filtered out (Optional has its own
    /// `__bockOption` runtime). Pre-scanned across the reached modules.
    enum_variants: crate::generator::EnumVariantRegistry,
    /// Type-alias registry: alias name → its underlying type-expression AIR node
    /// (`type ParseResult = Result[MarkdownNode, ParseError]` →
    /// `ParseResult → TypeNamed(Result[...])`). Go has no transparent alias to a
    /// *runtime* type the way Bock does: a function returning the alias
    /// `ParseResult` must lower to the `__bockResult` runtime struct (so a `match`
    /// on its value dispatches on `.tag`), and `result_elem_go_types` /
    /// `collect_optional_returns` must see *through* the alias to record the
    /// Ok/Err payload types. The emitter resolves an alias name to its target via
    /// this map. Pre-scanned across the reached modules (mirrors
    /// [`Self::enum_variants`]).
    type_aliases: HashMap<String, AIRNode>,
    /// Declared names of module-scope `const`s, pre-scanned across the reachable
    /// program. Emitted verbatim at both declaration and use so the two agree —
    /// `to_pascal_case` (`FIZZ_NUM` → `FIZZNUM`) at the def and `go_fn_name`
    /// (`fizzNUM`) at the use otherwise disagree. `SCREAMING_SNAKE` is a valid,
    /// exported Go identifier. See [`crate::generator::collect_const_names`].
    const_names: std::collections::HashSet<String>,
    /// Generic-type declaration registry: a record/enum/class name → its
    /// declared generic params. Lets an `impl Box { ... }` block recover the
    /// `[T any]` declared on `record Box[T]` so a Go method receiver emits
    /// `func (self *Box[T]) ...` (Go requires the type-param list on the
    /// receiver) and a construction emits `Box[int64]{...}`. Pre-scanned across
    /// the reached modules (mirrors [`Self::enum_variants`]).
    generic_decls: crate::generator::GenericDeclRegistry,
    /// Method-level type-parameter lowering registry (DQ28). Go forbids type
    /// parameters on methods (`func (b Box[T]) Map[U](..)` is a syntax error),
    /// but Bock keeps the surface (`Box[T].map[U]`); the Go backend lowers such a
    /// method to a *free function* `func Box_Map[T, U](self Box[T], ..) ..`,
    /// keyed `<TypeName>_<MethodGoName>` for collision-free naming (free
    /// functions support multiple type params natively — no monomorphization).
    /// This map records, per Bock *method name*, the owning Go type name, so a
    /// call site `box.map(f)` can be rewritten to `Box_Map(box, f)`. Keyed by
    /// method name only (codegen sees the AIR, not the checker's per-type method
    /// table); if two distinct types declare a generic method of the same name
    /// the entry is *poisoned* (removed) — the call site then falls back to the
    /// ordinary method-dispatch form, which is at worst un-lowered, never wrong
    /// for the unambiguous types. Pre-scanned across the reached modules.
    method_freefn_lowered: HashMap<String, String>,
    /// Maps an in-scope variable name → its Go type, used to infer a lambda's
    /// return type. Go infers a bare `func(...) interface{}` for every lambda;
    /// when such a closure is passed to a typed `func(int64) int64` parameter
    /// the assignment fails to compile. Tracking param/binding Go types lets the
    /// lambda emitter recover a concrete return type structurally from the body.
    /// Scoped per function/lambda body and restored on exit.
    var_go_type: HashMap<String, String>,
    /// Stack of value names already declared in each *Go block scope* currently
    /// open, innermost frame last. Used to lower a shadowing `let` correctly:
    /// Bock permits re-binding the same name in one block (the immutable-update
    /// idiom, `let acc = …; let acc = f(acc)`), but Go's `:=` rejects a
    /// re-declaration with no new variable on the left side. When a `let`'s name
    /// is already in the *innermost* frame, the binding lowers to a plain
    /// assignment (`acc = …`) instead of a fresh declaration (`acc := …` /
    /// `var acc T = …`). A new frame is pushed on entry to each Go block body
    /// (see [`Self::emit_block_body_inner`]) and popped on exit; the function's
    /// body frame is pre-seeded with the parameter names (via
    /// [`Self::pending_scope_seed`]) so a `let` shadowing a parameter (same Go
    /// scope) also reassigns. A name first declared in a *nested* block is not in
    /// an outer frame, so it stays a `:=` declaration — Go permits that legal
    /// inner-scope shadow.
    go_declared_scopes: Vec<HashSet<String>>,
    /// Names to merge into the next Go block frame pushed by
    /// [`Self::emit_block_body_inner`]. Set at function/method entry to the
    /// parameter names so the function body's frame (which shares the function's
    /// single Go scope — there is no extra brace for the body) treats a `let`
    /// shadowing a parameter as a reassignment. Consumed (taken) by the next
    /// frame push so it never leaks into a nested block.
    pending_scope_seed: Option<Vec<String>>,
    /// Maps a declare-only temp name (from the shared value-CF hoist) → the Go
    /// type inferred for its `var __bock_cf_N T` declaration. Go has no
    /// deferred-init `var x` (it needs a type), so a block emitter pre-scans each
    /// declare-only `let` paired with its following relocated control-flow
    /// statement, infers the result type structurally, and records it here for
    /// the `LetBinding` emitter. See [`Self::seed_decl_only_types`].
    decl_only_types: HashMap<String, String>,
    /// Maps a generic record's name → for each generic param (in declaration
    /// order) the field name whose declared type is exactly that param. Lets a
    /// construction `Box { value: 42 }` emit the explicit instantiation
    /// `Box[int64]{...}` Go requires (Go does *not* infer struct type args from
    /// composite-literal field values). `None` for a param not directly named by
    /// any field's type (then the arg falls back to `any`). Pre-scanned.
    record_param_fields: HashMap<String, Vec<Option<String>>>,
    /// Maps a record name → (field name → the Go element type of that field's
    /// `List[...]` declared type). Lets a built-in list method on a `self.field`
    /// receiver inside a (generic) method type its inline closure's `[]<elem>`
    /// parameter correctly: inside `fn next(self)` of `record ListIter[T] { xs:
    /// List[T] }`, `self.xs.get(i)` must take `[]T` (T is in scope on the
    /// receiver), not `[]interface{}` (which a `[]T` argument does not satisfy).
    /// Only `List`-typed fields are recorded. Pre-scanned across the reached modules.
    record_field_list_elem: HashMap<String, HashMap<String, String>>,
    /// Maps a record name → (field name → the Go `(key, value)` types of that
    /// field's `Map[K, V]` declared type). The `Map` analogue of
    /// [`Self::record_field_list_elem`]: lets a built-in map method on a
    /// `record.field` receiver (`report.by_category.get(k)` for `by_category:
    /// Map[String, Float]`) type its inline closure's `map[K]V`/`K`/`V`
    /// parameters from the field's declared key/value types rather than the
    /// erased `map[interface{}]interface{}` Go rejects against the concrete
    /// struct field. Only `Map`-typed fields are recorded. Pre-scanned.
    record_field_map_kv: HashMap<String, HashMap<String, (String, String)>>,
    /// Maps a record name → (field name → the Go type of that field), for every
    /// field of every record. The general scalar analogue of
    /// [`Self::record_field_list_elem`]/[`Self::record_field_map_kv`]: lets
    /// [`Self::infer_go_expr_type`] resolve a bare `obj.field` access (e.g. a
    /// `.map((b) => b.id)` closure body where `b: Block` and `record Block { id:
    /// Int }`) to the field's concrete Go type (`int64`), so the result slice is
    /// sized `[]int64` rather than the erased `[]interface{}` Go rejects against
    /// a declared `[]int64` return. Pre-scanned across the reached modules.
    record_field_go_type: HashMap<String, HashMap<String, String>>,
    /// Maps a record name → its generic-param names in declaration order
    /// (`"SortedSet" → ["T"]`). Lets a construction site substitute a field's
    /// declared list-element type (`record SortedSet[T] { items: List[T] }` →
    /// elem `T`) with the construct's resolved concrete type args, so an empty
    /// `[]` field literal emits `[]Key{}` for `SortedSet[Key]{…}` (or `[]T{}`
    /// when the construct is itself generic) rather than the erased
    /// `[]interface{}{}` Go rejects against the `[]T` struct field. Pre-scanned.
    record_generic_param_names: HashMap<String, Vec<String>>,
    /// The base name of the record whose method body is currently being emitted
    /// (`"ListIter"` inside `impl ListIter`'s methods), so a `self.field` list
    /// receiver resolves through [`Self::record_field_list_elem`]. Set at method
    /// entry, restored on exit; `None` outside an impl method body.
    current_self_record: Option<String>,
    /// Trait-declaration registry. Used at each `impl Trait for Type` site to
    /// recover the trait's *default* methods (those carrying a body) so a
    /// receiver method is synthesized on the target — the Go interface declares
    /// only the signature, so a type relying on an inherited default would
    /// otherwise fail to satisfy the interface and have no such method. Pre-
    /// scanned across the reached modules (mirrors [`Self::enum_variants`]).
    trait_decls: crate::generator::TraitDeclRegistry,
    /// Names of all top-level types (records, enums, traits, classes). A public
    /// Bock function whose PascalCased Go name collides with one of these (e.g.
    /// `public fn key` → `Key`, colliding with `record Key`) is renamed via
    /// [`Self::go_fn_name`] — Go has one namespace for types and functions, and
    /// PascalCasing erases the `key`/`Key` case distinction Bock relies on.
    type_names: HashSet<String>,
    /// When `Some(target)`, a `Self` type (`TypeSelf`) renders as `target`
    /// rather than the `/* Self */` placeholder. Set while emitting a trait-impl
    /// method on a concrete target — most relevant for a *synthesized default
    /// method* whose source uses `Self` (e.g. `other: Self`), which must become
    /// the concrete receiver type so the Go method signature is valid. Cleared
    /// everywhere else.
    go_self_subst: Option<String>,
    /// Trait names whose methods take a `Self`-typed operand (e.g.
    /// `Comparable`/`Equatable`, whose `compare`/`eq` take `other: Self`). Such
    /// traits are encoded as F-bounded *generic* interfaces in Go (`type
    /// Comparable[T any] interface { Compare(T) Ordering }`) and a bound `[T:
    /// Comparable]` lowers to `[T Comparable[T]]` — a plain Go interface cannot
    /// name the implementing type. Derived from [`Self::trait_decls`].
    self_param_traits: HashSet<String>,
    /// The Go return type of the function/method whose body is currently being
    /// emitted in *return position*. An `if`/`match` in expression position
    /// lowers to an IIFE; typing that IIFE with this concrete return type
    /// (`func() Ordering { … }`) rather than `func() interface{}` makes its
    /// result assignable where the concrete type is required (e.g. a user-enum
    /// `Ordering` return — `interface{}` does not satisfy a named interface).
    /// `None` outside a typed return body. The match/if IIFE also emits a
    /// trailing `panic("unreachable")` instead of `return nil` when typed, since
    /// a concrete (non-interface) return type has no `nil`.
    current_fn_ret_type: Option<String>,
    /// The enclosing function's declared return type *node*, kept when it is a
    /// function type (`Fn(Int) -> Int`). A lambda in tail-return position
    /// (`fn compose(...) -> Fn(Int) -> Int { (x) => f(g(x)) }`) is otherwise
    /// emitted with `interface{}` params and an `interface{}` return — not
    /// assignable to the declared `func(int64) int64` return. This lets the
    /// return-position emitter type that lambda's params/return from the declared
    /// function type. `None` outside a typed return body (or when the return type
    /// is not a function type). Saved/restored alongside `current_fn_ret_type`.
    current_fn_ret_type_node: Option<AIRNode>,
    /// The Go type a value-position expression is being assigned *into*, when
    /// known and distinct from the enclosing function's return type. Set around a
    /// `let x: T = <value>`'s value emit. An expression-position `match` lowers
    /// to an IIFE whose return type must be the *binding*'s declared `T`, not the
    /// function's return type (`current_fn_ret_type`) — a `let x: T = match …`
    /// where `T` ≠ the fn return otherwise emits `func() <RetType> { … }()` whose
    /// result is not assignable to `T`. When set (and not `interface{}`), the
    /// match/if IIFE prefers this over `current_fn_ret_type`. `None` outside a
    /// typed binding context; consumed (taken) around the value emit so it never
    /// leaks to a sibling/outer expression. Additive: when absent the IIFE keeps
    /// using `current_fn_ret_type`, preserving the working Optional/Result/enum
    /// return-position behavior.
    current_expected_type: Option<String>,
    /// Expected collection element Go types for a collection literal emitted in
    /// a *typed context* (a `let x: List[T] = [...]`). A collection literal
    /// infers its element type from its elements, but an EMPTY literal (`[]`)
    /// or one whose elements infer looser than the declaration cannot — and the
    /// `interface{}` fallback then mismatches the declared `[]T`. When set, a
    /// `List`/`Set` literal uses `.0` as its element type and a `Map` literal
    /// uses `(.0, .1)` as `(key, value)`, so the literal matches the declared
    /// container. `None` outside a typed-collection binding context. Consumed
    /// (taken) at the literal so it never leaks to a nested/sibling literal.
    expected_collection_elem: Option<(String, Option<String>)>,
    /// The enclosing function's *return* collection element Go types, when its
    /// return type is a `List[T]` / `Set[T]` / `Map[K, V]`. A collection literal
    /// in `return` position adopts these so a generic `fn single[T](x: T) ->
    /// List[T] { return [x] }` emits `[]T{x}` rather than the `[]interface{}{x}`
    /// the bare-literal inference falls back to (which is not assignable to the
    /// `[]T` return). Set at fn/method entry from the return type, restored on
    /// exit; `None` for a non-collection or absent return type.
    current_fn_ret_collection_elem: Option<(String, Option<String>)>,
    /// Signatures of top-level generic functions, keyed by fn name: the declared
    /// generic-param names and each value param's declared type node. Used at a
    /// call site to type an *untyped lambda argument* (`Filter(it, (x) => x >
    /// 2)`): the non-lambda arguments bind the fn's type params to concrete Go
    /// types, and the lambda's `Fn(T) -> U` parameter type is then specialised
    /// (`func(int64) bool`) so the emitted closure's param is `x int64`, not the
    /// `interface{}` default an unannotated param falls back to (which both
    /// breaks `x > 2` arithmetic and mismatches the `func(int64) bool`
    /// parameter). Only generic fns are recorded (a non-generic fn's lambda arg
    /// already types correctly). Pre-scanned across the reached modules.
    fn_signatures: HashMap<String, GoFnSig>,
    /// Names of generic fns whose bound was lowered to a Go built-in constraint
    /// from a sealed-core trait (`[T: Comparable]` → `[T __bockOrdered]`, GAP-C).
    /// Under such a constraint Go infers `T` from an *untyped* constant arg as the
    /// default type (`int`, not `int64`), mismatching an `int64`-typed
    /// destination, so the call site must synthesise an explicit type arg
    /// (`max2[int64](9, 7)`) even though the signature touches no container. Set
    /// during the same pre-scan as [`Self::fn_signatures`].
    fn_sealed_bound: std::collections::HashSet<String>,
    /// Maps a *non-generic* top-level fn name → its rendered Go return type
    /// (`"key" → "Key"`). Lets [`Self::infer_go_expr_type`] type a call to a
    /// concrete constructor/helper so a list literal of such calls (`[key(3),
    /// key(1)]`) infers the homogeneous element type `Key` and emits `[]Key{…}`
    /// — which in turn lets a generic callee taking that slice (`from_list`,
    /// `max_of`) infer its element type rather than collapsing to
    /// `[]interface{}` / `[any]`. Generic fns live in [`Self::fn_signatures`].
    fn_return_go_types: HashMap<String, String>,
    /// Maps a *non-generic* top-level fn name → its value params' declared type
    /// nodes (the same `Vec<Option<AIRNode>>` shape [`Self::fn_signatures`] holds
    /// for generic fns). A non-generic fn taking a concrete `Fn(Todo) -> Bool`
    /// parameter — e.g. `count_where(todos, pred)` — must still pin an untyped
    /// `(t) => t.done` lambda argument to `func(t Todo) bool`; without this map
    /// the lambda erases to `func(t interface{}) bool` and `t.Done` fails (Go has
    /// no field on `interface{}`). Pre-scanned alongside `fn_signatures`; consumed
    /// at the call site as the lambda-specialisation fallback when the callee is
    /// not generic.
    fn_param_types: HashMap<String, Vec<Option<AIRNode>>>,
    /// The concrete Go parameter types an *untyped lambda argument* should adopt
    /// at its current call site, derived from the callee's generic signature
    /// specialised by the other arguments ([`Self::fn_signatures`]). A lambda's
    /// own params carry no source annotation (`(x) => x > 2`), so without this
    /// they default to `interface{}` — which both breaks the body's arithmetic
    /// and mismatches the typed `func(int64) bool` callee parameter. Set just
    /// before emitting such an argument, consumed (taken) by the lambda emit so
    /// it never leaks to a nested lambda. `None` for an ordinarily-typed lambda.
    expected_lambda_param_types: Option<Vec<String>>,
    /// A forced Go return type for the *next* lambda emitted, consumed (taken) by
    /// the `Lambda` arm. A predicate combinator (`filter`/`find`/`any`/`all`)
    /// always takes a `Bool`-returning closure, but the body may be a method call
    /// (`(p) => p.in_stock()`) or a `match` whose Go return type
    /// [`Self::infer_go_expr_type`] cannot recover — leaving the closure typed
    /// `func(T) interface{}`, which then fails `if __f(__x)` (`non-boolean
    /// condition`). Setting this to `Some("bool")` pins the predicate's return so
    /// the closure type-checks. `None` for an ordinarily-inferred lambda.
    forced_lambda_ret: Option<String>,
    /// True in the **per-module native-package** emission path
    /// ([`GoGenerator::generate_project`], the sole real-build path). When set,
    /// the `Module` arm does **not** inline the runtime preludes (they are
    /// emitted once into the shared `bock_runtime.go` by `generate_project`) —
    /// each module is its own `package main` file and same-package symbols are
    /// visible without an import. When clear, the module is emitted as a single
    /// self-contained file with its runtime preludes inlined — the
    /// [`GoGenerator::generate_module`] path used by unit tests.
    per_module: bool,
}

/// The set of Go stdlib packages the emitted body needs imported, gathered as
/// it is generated. Rendered into a single deduped `import (...)` block by
/// [`GoImportNeeds::render_block`]. Shared by the two emission entry points
/// ([`GoEmitCtx::into_parts`] for the per-module path and
/// [`GoEmitCtx::finish`] for the single-module path) so the import logic lives
/// in one place.
#[derive(Default, Clone, Copy)]
struct GoImportNeeds {
    fmt: bool,
    sync: bool,
    time: bool,
    strings: bool,
    utf8: bool,
    math: bool,
    unicode: bool,
    strconv: bool,
    reflect: bool,
}

impl GoImportNeeds {
    /// Render the needed packages as a Go `import` clause (`import "x"` for a
    /// single package, an `import (...)` block for several), or the empty string
    /// when nothing is needed. The order matches `gofmt`'s lexical sort.
    fn render_block(self) -> String {
        // Packages are pushed in `gofmt`'s lexical sort order.
        let mut imports = Vec::new();
        if self.fmt {
            imports.push("\"fmt\"");
        }
        if self.math {
            imports.push("\"math\"");
        }
        if self.reflect {
            imports.push("\"reflect\"");
        }
        if self.strconv {
            imports.push("\"strconv\"");
        }
        if self.strings {
            imports.push("\"strings\"");
        }
        if self.sync {
            imports.push("\"sync\"");
        }
        if self.time {
            imports.push("\"time\"");
        }
        if self.unicode {
            imports.push("\"unicode\"");
        }
        if self.utf8 {
            imports.push("\"unicode/utf8\"");
        }
        if imports.is_empty() {
            return String::new();
        }
        if imports.len() == 1 {
            return format!("\nimport {}\n", imports[0]);
        }
        let mut block = String::from("\nimport (\n");
        for imp in &imports {
            block.push_str(&format!("\t{imp}\n"));
        }
        block.push_str(")\n");
        block
    }
}

/// A recorded generic-function signature ([`GoEmitCtx::fn_signatures`]): the
/// declared generic-param names, each value param's declared type node, and the
/// return type node. Used to specialise an untyped lambda argument at a call
/// site (bind the type params from the non-lambda args, substitute into the
/// lambda's `Fn(...)` param type) and to infer a generic call's result type.
type GoFnSig = (Vec<String>, Vec<Option<AIRNode>>, Option<AIRNode>);

impl GoEmitCtx {
    fn new() -> Self {
        Self {
            buf: String::with_capacity(4096),
            indent: 0,
            needs_fmt_import: false,
            needs_sync_import: false,
            needs_time_import: false,
            needs_strings_import: false,
            needs_utf8_import: false,
            needs_math_import: false,
            needs_unicode_import: false,
            needs_strconv_import: false,
            needs_reflect_import: false,
            package_name: "main".into(),
            effect_ops: HashMap::new(),
            current_handler_vars: HashMap::new(),
            fn_effects: HashMap::new(),
            composite_effects: HashMap::new(),
            public_fns: HashSet::new(),
            void_effect_ops: HashSet::new(),
            async_fns: HashSet::new(),
            public_methods: HashSet::new(),
            inherent_methods: HashSet::new(),
            record_field_names: HashSet::new(),
            loop_labels: Vec::new(),
            switch_label_depth: 0,
            loop_label_counter: 0,
            guard_counter: 0,
            try_counter: 0,
            let_tuple_counter: 0,
            loop_expr_depth: 0,
            fn_optional_ret_elem: HashMap::new(),
            var_optional_elem: HashMap::new(),
            var_decl_type_node: HashMap::new(),
            method_optional_ret_elem: HashMap::new(),
            method_ret_record_args: HashMap::new(),
            method_return_go_types: HashMap::new(),
            var_lambda_ret: HashMap::new(),
            var_record_type_args: HashMap::new(),
            var_list_elem: HashMap::new(),
            var_map_kv: HashMap::new(),
            var_set_elem: HashMap::new(),
            var_result_elem: HashMap::new(),
            fn_result_ret_elem: HashMap::new(),
            concurrency_runtime_emitted: false,
            optional_runtime_emitted: false,
            result_runtime_emitted: false,
            numeric_runtime_emitted: false,
            ordering_runtime_emitted: false,
            ordered_constraint_emitted: false,
            range_runtime_emitted: false,
            int_pow_runtime_emitted: false,
            deep_eq_runtime_emitted: false,
            enum_variants: crate::generator::EnumVariantRegistry::new(),
            type_aliases: HashMap::new(),
            const_names: std::collections::HashSet::new(),
            generic_decls: crate::generator::GenericDeclRegistry::new(),
            method_freefn_lowered: HashMap::new(),
            var_go_type: HashMap::new(),
            go_declared_scopes: Vec::new(),
            pending_scope_seed: None,
            decl_only_types: HashMap::new(),
            record_param_fields: HashMap::new(),
            record_field_list_elem: HashMap::new(),
            record_field_map_kv: HashMap::new(),
            record_field_go_type: HashMap::new(),
            record_generic_param_names: HashMap::new(),
            current_self_record: None,
            trait_decls: crate::generator::TraitDeclRegistry::new(),
            type_names: HashSet::new(),
            go_self_subst: None,
            self_param_traits: HashSet::new(),
            current_fn_ret_type: None,
            current_fn_ret_type_node: None,
            current_expected_type: None,
            current_fn_ret_collection_elem: None,
            fn_signatures: HashMap::new(),
            fn_sealed_bound: std::collections::HashSet::new(),
            fn_return_go_types: HashMap::new(),
            fn_param_types: HashMap::new(),
            expected_lambda_param_types: None,
            forced_lambda_ret: None,
            expected_collection_elem: None,
            per_module: false,
        }
    }

    /// Clone the program-wide cross-module *analysis* state into a fresh
    /// emission context for one file of the per-module tree, resetting only the
    /// per-file emission state (output buffer + indent, the `needs_*` per-file
    /// import flags, and the runtime-once flags). The analysis registries
    /// (`enum_variants`, `trait_decls`, method/Optional-return metadata, …) are
    /// carried so a reference in one file to a symbol declared in another
    /// resolves correctly across the per-module tree.
    fn fork(&self) -> Self {
        let mut c = self.clone();
        c.buf = String::with_capacity(4096);
        c.indent = 0;
        c.needs_fmt_import = false;
        c.needs_sync_import = false;
        c.needs_time_import = false;
        c.needs_strings_import = false;
        c.needs_utf8_import = false;
        c.needs_math_import = false;
        c.needs_unicode_import = false;
        c.needs_strconv_import = false;
        c.needs_reflect_import = false;
        c.concurrency_runtime_emitted = false;
        c.optional_runtime_emitted = false;
        c.result_runtime_emitted = false;
        c.numeric_runtime_emitted = false;
        c.ordering_runtime_emitted = false;
        c.ordered_constraint_emitted = false;
        c.range_runtime_emitted = false;
        c.int_pow_runtime_emitted = false;
        c.deep_eq_runtime_emitted = false;
        c.per_module = false;
        c
    }

    /// Pre-seed the effect registries (`effect_ops`, `composite_effects`,
    /// `void_effect_ops`) from every module's top-level `EffectDecl`s. In the
    /// per-module path each module is emitted by its own forked context, so a
    /// bare op `log(...)` used in `main` whose effect `Log` is declared in
    /// another module must be recognised without having emitted the declaring
    /// module first (cross-module effects, §10). Mirrors the Python / JS / TS /
    /// Rust backends' equivalents.
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
            }
        }
    }

    /// The Go type to use for an expression-position `if`/`match` IIFE return.
    ///
    /// Prefers the binding's *expected* type ([`Self::current_expected_type`],
    /// set around a `let x: T = …` value emit) when known and concrete, so a
    /// value-position `let x: T = match …` produces `func() T { … }()` —
    /// assignable to `T` even when `T` differs from the enclosing function's
    /// return type. An `interface{}` expected type is ignored (it carries no more
    /// information than the untyped fallback and would suppress a more specific
    /// `current_fn_ret_type`). Falls back to the function's return type
    /// ([`Self::current_fn_ret_type`]) for the return-position case
    /// (`return match …`). `None` ⇒ the caller emits the `interface{}` fallback.
    fn expected_iife_type(&self) -> Option<String> {
        match self.current_expected_type.as_deref() {
            Some(t) if t != "interface{}" => Some(t.to_string()),
            _ => self.current_fn_ret_type.clone(),
        }
    }

    /// Populate [`Self::self_param_traits`] from the already-built
    /// [`Self::trait_decls`] registry. Call after `trait_decls` is set.
    fn derive_self_param_traits(&mut self) {
        for (name, info) in &self.trait_decls {
            if crate::generator::trait_uses_self_operand(info) {
                self.self_param_traits.insert(name.clone());
            }
        }
    }

    /// Whether `name` is bound as a *local value* (parameter, `let`, match /
    /// guard bind, typed loop var) at the current emission point, in which case
    /// it must shadow a same-named public module function
    /// (Q-go-runtime-helper-shadowing). When any `core.string` item is imported
    /// the whole module is reached and its public fns (`lines`, `repeat`, …)
    /// enter [`Self::public_fns`]; without this check the identifier emitter
    /// PascalCased EVERY bare reference, so a `lines: List[String]` parameter
    /// used in `for line in lines` emitted `range Lines` — the helper
    /// *function* — which Go rejects. Bock scoping says the local wins; the
    /// checker already resolved it that way, so the Go spelling must too.
    ///
    /// Locals are tracked in two places, both keyed by the Go-escaped name and
    /// both populated only as emission *reaches* the binding (so a reference
    /// preceding a later same-named `let` still resolves to the module fn):
    /// [`Self::var_go_type`] (params on fn/method/lambda entry, typed loop vars,
    /// typed binds — saved/restored per scope) and the
    /// [`Self::go_declared_scopes`] frames (every `let`/bind the open blocks
    /// declared, seeded with the parameter names). Checked across *all* open
    /// frames — an outer fn-scope binding shadows inside nested blocks too.
    fn local_shadows_public_fn(&self, name: &str) -> bool {
        if !self.public_fns.contains(name) {
            return false;
        }
        let key = go_value_ident(name);
        self.var_go_type.contains_key(&key)
            || self
                .go_declared_scopes
                .iter()
                .any(|frame| frame.contains(&key))
    }

    /// The Go identifier for a top-level Bock function reference, applying the
    /// public/private PascalCase/camelCase rule and then disambiguating a public
    /// name that collides with a top-level type. Go has a single namespace for
    /// types and functions, and PascalCasing collapses Bock's `key`/`Key` case
    /// distinction onto one identifier; when a public function's Go name equals a
    /// declared type name (`func Key` vs `type Key`), the function is suffixed
    /// with `Fn` (`KeyFn`). The same rule is applied at the declaration site and
    /// every call/reference site so they always agree.
    fn go_fn_name(&self, name: &str) -> String {
        if self.public_fns.contains(name) {
            let pascal = to_pascal_case(name);
            if self.type_names.contains(&pascal) {
                format!("{pascal}Fn")
            } else {
                pascal
            }
        } else {
            // Private fns and bare value references both route here; escape so a
            // `camelCase` name colliding with a Go keyword (`default`, `range`,
            // `type`, …) is mangled identically at the declaration and every
            // reference, keeping them in sync.
            go_value_ident(name)
        }
    }

    /// Pre-scan every module's top-level type declarations (records, enums,
    /// traits, classes) into [`Self::type_names`], and every `public` top-level
    /// function name into [`Self::public_fns`], so [`Self::go_fn_name`] can
    /// detect a function-name/type-name collision at *any* call site — including
    /// a call that precedes the function's declaration in emission order.
    /// Mirrors the other pre-scans.
    fn collect_fn_and_type_names(&mut self, module: &AIRNode) {
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                match &item.kind {
                    NodeKind::RecordDecl { name, .. }
                    | NodeKind::EnumDecl { name, .. }
                    | NodeKind::TraitDecl { name, .. }
                    | NodeKind::ClassDecl { name, .. } => {
                        self.type_names.insert(name.name.clone());
                    }
                    NodeKind::FnDecl {
                        visibility, name, ..
                    } if matches!(visibility, Visibility::Public) && name.name != "main" => {
                        self.public_fns.insert(name.name.clone());
                    }
                    _ => {}
                }
                // Record every *generic* top-level fn's signature (generic-param
                // names + each value param's declared type) so a call site can
                // specialise an untyped lambda argument to the concrete Go type
                // (see `fn_signatures`). Recorded regardless of visibility — the
                // embedded `core.iter` combinators are public, but a user's
                // private generic fn taking a lambda needs the same treatment.
                if let NodeKind::FnDecl {
                    name,
                    generic_params,
                    params,
                    ..
                } = &item.kind
                {
                    if !generic_params.is_empty() {
                        let gp_names: Vec<String> =
                            generic_params.iter().map(|p| p.name.name.clone()).collect();
                        let param_tys: Vec<Option<AIRNode>> = params
                            .iter()
                            .map(|p| match &p.kind {
                                NodeKind::Param { ty, .. } => ty.as_deref().cloned(),
                                _ => None,
                            })
                            .collect();
                        let ret_ty = if let NodeKind::FnDecl { return_type, .. } = &item.kind {
                            return_type.as_deref().cloned()
                        } else {
                            None
                        };
                        // A generic param whose sealed-core bound was lowered to a
                        // Go built-in constraint defeats Go's untyped-constant
                        // inference (GAP-C), so the call site must synthesise an
                        // explicit type arg — record the fn.
                        let has_sealed_bound = generic_params.iter().any(|p| {
                            p.bounds.iter().any(|b| {
                                let bn = b
                                    .segments
                                    .iter()
                                    .map(|s| s.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(".");
                                crate::generator::is_unimplemented_sealed_core_trait(
                                    &bn,
                                    &self.trait_decls,
                                )
                            })
                        });
                        if has_sealed_bound {
                            self.fn_sealed_bound.insert(name.name.clone());
                        }
                        self.fn_signatures
                            .insert(name.name.clone(), (gp_names, param_tys, ret_ty));
                    } else if let NodeKind::FnDecl { return_type, .. } = &item.kind {
                        // A *non-generic* fn: record its rendered Go return type so
                        // a call (`key(3)`) can be typed for homogeneous list-elem
                        // inference. Skip `Void`/`Unit` returns (no usable type).
                        if let Some(ret) = return_type.as_deref() {
                            if !Self::is_void_type(ret) {
                                self.fn_return_go_types
                                    .insert(name.name.clone(), self.type_to_go(ret));
                            }
                        }
                        // Record the value params' declared type nodes so an
                        // untyped lambda argument to a concrete `Fn(...)` param
                        // (`count_where(todos, (t) => t.done)`) can be specialised
                        // to `func(t Todo) bool` rather than `func(t interface{})`.
                        let param_tys: Vec<Option<AIRNode>> = params
                            .iter()
                            .map(|p| match &p.kind {
                                NodeKind::Param { ty, .. } => ty.as_deref().cloned(),
                                _ => None,
                            })
                            .collect();
                        if param_tys.iter().any(Option::is_some) {
                            self.fn_param_types.insert(name.name.clone(), param_tys);
                        }
                    }
                }
            }
        }
    }

    /// Pre-scan a module's top-level `RecordDecl`s and, for each generic
    /// record, record which field's declared type is each generic param (in
    /// param order). A construction site then looks up the field value's Go
    /// type per param to emit the explicit `[arg, ...]` instantiation Go
    /// requires. Additive across the reached modules (mirrors the other `collect_*`).
    fn collect_record_param_fields(&mut self, module: &AIRModule) {
        let NodeKind::Module { items, .. } = &module.kind else {
            return;
        };
        for item in items {
            let NodeKind::RecordDecl {
                name,
                generic_params,
                fields,
                ..
            } = &item.kind
            else {
                continue;
            };
            // Record each `List[...]`-typed field's Go element type, keyed by
            // field name — used to type a `self.field.get(i)` list-method
            // receiver's closure inside the record's methods. Done for every
            // record (generic or not): a non-generic record may still hold a
            // `List[Int]` field whose method-side receiver needs `[]int64`.
            let list_fields: HashMap<String, String> = fields
                .iter()
                .filter_map(|f| {
                    Self::list_field_elem_type(&f.ty)
                        .map(|elem_ty| (f.name.name.clone(), self.ast_type_to_go(elem_ty)))
                })
                .collect();
            if !list_fields.is_empty() {
                self.record_field_list_elem
                    .insert(name.name.clone(), list_fields);
            }
            // Record each `Map[K, V]`-typed field's Go key/value types, keyed by
            // field name — used to type a `record.field.get(k)` map-method
            // receiver's inline closure (`map[K]V` / `K` / `V`) from the field's
            // declared types rather than the erased `map[interface{}]interface{}`.
            let map_fields: HashMap<String, (String, String)> = fields
                .iter()
                .filter_map(|f| {
                    Self::map_field_kv_type(&f.ty).map(|(k, v)| {
                        (
                            f.name.name.clone(),
                            (self.ast_type_to_go(k), self.ast_type_to_go(v)),
                        )
                    })
                })
                .collect();
            if !map_fields.is_empty() {
                self.record_field_map_kv
                    .insert(name.name.clone(), map_fields);
            }
            // Record EVERY field's Go type, keyed by field name — lets
            // `infer_go_expr_type` resolve a bare `obj.field` access (a
            // `.map((b) => b.id)` closure body) to the field's concrete Go type,
            // sizing the result slice concretely rather than as `[]interface{}`.
            let field_go_types: HashMap<String, String> = fields
                .iter()
                .map(|f| (f.name.name.clone(), self.ast_type_to_go(&f.ty)))
                .collect();
            if !field_go_types.is_empty() {
                self.record_field_go_type
                    .insert(name.name.clone(), field_go_types);
            }
            if generic_params.is_empty() {
                continue;
            }
            self.record_generic_param_names.insert(
                name.name.clone(),
                generic_params.iter().map(|p| p.name.name.clone()).collect(),
            );
            let per_param: Vec<Option<String>> = generic_params
                .iter()
                .map(|gp| {
                    fields
                        .iter()
                        .find(|f| Self::ast_type_is_param(&f.ty, &gp.name.name))
                        .map(|f| f.name.name.clone())
                })
                .collect();
            self.record_param_fields
                .insert(name.name.clone(), per_param);
        }
    }

    /// If `ty` is a `List[Elem]` named type, return its element `TypeExpr`,
    /// else `None`. Used to record a record field's list element type for
    /// method-side receiver typing.
    fn list_field_elem_type(ty: &TypeExpr) -> Option<&TypeExpr> {
        match ty {
            TypeExpr::Named { path, args, .. }
                if args.len() == 1 && path.segments.last().is_some_and(|s| s.name == "List") =>
            {
                args.first()
            }
            _ => None,
        }
    }

    /// The `(key, value)` type expressions of a `Map[K, V]`-typed field, or
    /// `None` for any other type. The `Map` analogue of
    /// [`Self::list_field_elem_type`]; used to populate
    /// [`Self::record_field_map_kv`].
    fn map_field_kv_type(ty: &TypeExpr) -> Option<(&TypeExpr, &TypeExpr)> {
        match ty {
            TypeExpr::Named { path, args, .. }
                if args.len() == 2 && path.segments.last().is_some_and(|s| s.name == "Map") =>
            {
                Some((args.first()?, args.get(1)?))
            }
            _ => None,
        }
    }

    /// True when `ty` is a bare named type whose single segment is `param`
    /// (i.e. the field is declared with exactly the generic param `T`, not
    /// `List[T]` or some other composite).
    fn ast_type_is_param(ty: &TypeExpr, param: &str) -> bool {
        matches!(
            ty,
            TypeExpr::Named { path, args, .. }
                if args.is_empty()
                    && path.segments.len() == 1
                    && path.segments[0].name == param
        )
    }

    /// Variant info for `path` when its last segment is a registered *user*
    /// enum variant (built-in Optional/Result pre-seeds excluded — Optional has
    /// its own `__bockOption` runtime, handled by the bespoke `go_match_is_*`
    /// paths).
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

    /// True when the real `core.compare.Ordering` enum is reachable in this
    /// program (its `Less` variant is a registered user enum variant). When
    /// `core.compare` is `use`d, the actual `enum Ordering` decl is emitted; its
    /// `Less`/`Equal`/`Greater` references and matches then use the user-enum
    /// representation (sealed-interface variant structs `OrderingLess{}`), not
    /// the prelude `__bockOrdering` value runtime used when the enum is *not*
    /// reachable (e.g. a bare primitive `compare`).
    fn ordering_enum_reachable(&self) -> bool {
        self.enum_variants
            .get("Less")
            .is_some_and(|info| info.enum_name == "Ordering")
    }

    /// True if every arm of a `match` is a registered user enum variant pattern
    /// (constructor / record / unit), so the match lowers to a Go *type-switch*
    /// over the sealed-interface concrete types with field extraction.
    fn go_match_is_user_enum(&self, arms: &[AIRNode]) -> bool {
        let mut saw_variant = false;
        for arm in arms {
            let NodeKind::MatchArm { pattern, .. } = &arm.kind else {
                continue;
            };
            match &pattern.kind {
                NodeKind::ConstructorPat { path, .. } | NodeKind::RecordPat { path, .. }
                    if self.user_variant_for_path(path).is_some() =>
                {
                    saw_variant = true;
                }
                // Any constructor / record pattern that is NOT a registered
                // user variant disqualifies the type-switch lowering.
                NodeKind::ConstructorPat { .. } | NodeKind::RecordPat { .. } => return false,
                // A trailing `_` / bind arm is a permissible default.
                NodeKind::WildcardPat | NodeKind::BindPat { .. } => {}
                _ => return false,
            }
        }
        saw_variant
    }

    /// True if any arm of a user-enum type-switch binds a payload field from the
    /// concrete `__v` (a `ConstructorPat`/`RecordPat` with at least one non-`_`
    /// sub-pattern). When no arm does, the statement-position type-switch binds
    /// `__v` but never reads it — Go's "declared and not used" — unless a
    /// `default: panic(... __v)` consumes it. See [`Self::emit_match`].
    fn go_user_enum_match_binds_payload(arms: &[AIRNode]) -> bool {
        arms.iter().any(|arm| {
            let NodeKind::MatchArm { pattern, .. } = &arm.kind else {
                return false;
            };
            match &pattern.kind {
                NodeKind::ConstructorPat { fields, .. } => fields
                    .iter()
                    .any(|f| !matches!(f.kind, NodeKind::WildcardPat)),
                NodeKind::RecordPat { fields, .. } => fields.iter().any(|f| {
                    f.pattern
                        .as_ref()
                        .is_none_or(|p| !matches!(p.kind, NodeKind::WildcardPat))
                }),
                _ => false,
            }
        })
    }

    /// True if any arm's top-level pattern is a bare `BindPat` (`x => …`,
    /// `mut x => …`). Such a match cannot use the value-switch IIFE in expression
    /// position: a bind has no value to `case` on, so the switch lowering emits
    /// the broken `case interface{}:` and drops the bound name. The
    /// expression-position emitter routes these to the if-chain IIFE instead
    /// (which binds `x := root` in an unconditional `else`). Statement position is
    /// unaffected — `emit_match`'s `value_switch_binds` already handles it via the
    /// `switch __v := scrutinee; __v` form. Mirrors the `match_needs_ifchain`
    /// gate without touching that shared single-discriminant fast-path.
    fn go_value_match_has_bind_arm(arms: &[AIRNode]) -> bool {
        arms.iter().any(|arm| {
            matches!(
                &arm.kind,
                NodeKind::MatchArm { pattern, .. }
                    if matches!(pattern.kind, NodeKind::BindPat { .. })
            )
        })
    }

    /// True if any arm matches a *plain* (non-enum-variant) record with only
    /// bind/wildcard fields (`Point { x, .. }`, `Point { x, y }`). Such an arm is
    /// not "structured" by `match_needs_ifchain` (no nested sub-pattern), so a
    /// value-position match made solely of these stays on the value-switch
    /// fast-path — which emits the broken `case Point:` (a Go *type* in expression
    /// position) and drops the field binds (`undefined: x`). A plain record is a
    /// concrete struct, not a sealed-interface value, so it has no value/type to
    /// switch on at all; route the match to the if-chain IIFE, whose
    /// `pattern_test_go` / `collect_binds_go` read each field directly off
    /// `access.<Field>`. (A record arm with a *literal* field — `Point { x: 0 }` —
    /// is already structured, so `match_needs_ifchain` diverts it; this only
    /// covers the all-bind/wildcard plain-record arm.) Does not touch the shared
    /// `match_needs_ifchain`.
    fn go_value_match_has_plain_record_arm(&self, arms: &[AIRNode]) -> bool {
        arms.iter().any(|arm| {
            let NodeKind::MatchArm { pattern, .. } = &arm.kind else {
                return false;
            };
            matches!(&pattern.kind, NodeKind::RecordPat { path, .. }
                if self.user_variant_for_path(path).is_none())
        })
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

    /// Pre-scan all impl/class/trait blocks for `public` method names so call
    /// sites can match the Go method casing (PascalCase public, camelCase
    /// private).
    ///
    /// Trait methods — both those declared in a `TraitDecl` and those of an
    /// `impl Trait for Type` block — are recorded *regardless of Bock
    /// visibility*: Go interface methods are always emitted exported
    /// (PascalCase, see the `TraitDecl` emission), so the method must be
    /// PascalCased everywhere (interface signature, receiver method, and call
    /// site) for the type to satisfy the interface. A `private` trait default
    /// method would otherwise be PascalCased in the interface but camelCased at
    /// the call site, and the call would not resolve. Inherent (`impl Type`)
    /// and class methods keep the public-only rule.
    fn collect_methods(&mut self, module: &AIRNode) {
        // Collect every record/class field's PascalCased Go name so
        // `go_method_name` can detect a field/method name collision Go
        // forbids on a struct (`SimpleError { message }` + a `message`
        // method). The shared collector (used identically by js/ts/py) walks
        // every record/class regardless of where the colliding method is
        // declared (a separate `impl` block). `collect_methods` is called once
        // per reachable module to build a *program-wide* set on the template
        // ctx (see `generate_project`), so we extend rather than replace.
        self.record_field_names
            .extend(crate::generator::collect_record_field_names(
                module,
                to_pascal_case,
            ));
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                // `inherent_target` is `Some(type name)` for an inherent (`impl
                // Type`, no trait) or class block — used to record
                // `(type, method)` so a redundant same-named trait-impl forwarder
                // can be skipped. A trait impl (`impl Trait for Type`) is not an
                // inherent definition (it is the forwarder we may skip).
                let (methods, always_export, inherent_target) = match &item.kind {
                    NodeKind::ImplBlock {
                        methods,
                        trait_path,
                        target,
                        ..
                    } => {
                        let inherent = if trait_path.is_none() {
                            Some(self.type_expr_to_string(target))
                        } else {
                            None
                        };
                        (methods, trait_path.is_some(), inherent)
                    }
                    NodeKind::TraitDecl { methods, .. } => (methods, true, None),
                    NodeKind::ClassDecl { methods, name, .. } => {
                        (methods, false, Some(name.name.clone()))
                    }
                    _ => continue,
                };
                for m in methods {
                    if let NodeKind::FnDecl {
                        visibility,
                        name,
                        generic_params,
                        ..
                    } = &m.kind
                    {
                        if always_export || matches!(visibility, Visibility::Public) {
                            self.public_methods.insert(name.name.clone());
                        }
                        if let Some(ty) = &inherent_target {
                            // Key on the PascalCased Go method name: a trait
                            // declares its methods exported (PascalCase), so a
                            // skip check from the trait-impl side compares against
                            // that casing. The inherent method itself is exported
                            // to the same Go name when its name is in
                            // `public_methods` (see `emit_method_body`).
                            self.inherent_methods
                                .insert((ty.clone(), to_pascal_case(&name.name)));
                            // DQ28: an inherent/class method that declares its own
                            // type parameters (`Box[T].map[U]`) cannot be a Go
                            // method (Go forbids method type params). Record it for
                            // free-function lowering, keyed by the Bock method
                            // name → owning type. Poison the entry if a second type
                            // declares a generic method of the same name (ambiguous
                            // at the by-name call site): set the value to a sentinel
                            // so the lookup treats it as absent.
                            if !generic_params.is_empty() {
                                use std::collections::hash_map::Entry;
                                match self.method_freefn_lowered.entry(name.name.clone()) {
                                    Entry::Vacant(e) => {
                                        e.insert(ty.clone());
                                    }
                                    Entry::Occupied(mut e) => {
                                        if e.get() != ty {
                                            // Ambiguous: two types, same generic
                                            // method name. Poison with a sentinel
                                            // that names no real type.
                                            e.insert(String::new());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The Go method name for a Bock method, applying the public/private
    /// PascalCase/camelCase rule and then disambiguating a public method whose
    /// PascalCased name collides with a struct field name. Go forbids a struct
    /// having a field and a method with the same name, so when a public method's
    /// Go name (`Message`) equals a record/class field name (`SimpleError`'s
    /// `message` field), the method is suffixed `Method` (`MessageMethod`). The
    /// same rule is applied at the trait-interface declaration, the receiver
    /// method, and every call site so they always agree. Private methods are
    /// camelCased and never collide with a (PascalCased) field name.
    fn go_method_name(&self, name: &str, is_public: bool) -> String {
        if is_public {
            // Shared collision policy (js/ts/py route through the same helper):
            // a public method whose PascalCased name equals a field name gets a
            // `Method` suffix (`Message` → `MessageMethod`).
            crate::generator::disambiguate_method_name(
                to_pascal_case(name),
                &self.record_field_names,
                "Method",
            )
        } else {
            to_camel_case(name)
        }
    }

    /// The owning Go type name of a method that is *free-function-lowered* for
    /// DQ28 (a method with its own type parameters, e.g. `Box[T].map[U]`), or
    /// `None` when the method name is not lowered or is ambiguous (the poison
    /// sentinel — an empty type name — reads as absent). When `Some(ty)`, a call
    /// site `recv.method(args)` lowers to `<FreeFnName>(recv, args)` and the
    /// declaration emits a free function instead of a Go method.
    fn freefn_lowered_type(&self, method_name: &str) -> Option<&str> {
        self.method_freefn_lowered
            .get(method_name)
            .map(String::as_str)
            .filter(|ty| !ty.is_empty())
    }

    /// The Go free-function name a DQ28-lowered method lowers to:
    /// `<TypeName>_<MethodGoName>` (`Box` + `Map` → `Box_Map`). The method name
    /// is PascalCased via [`Self::go_method_name`] (a public method matches its
    /// every call site; a private one camelCases) so the declaration and call
    /// site always agree. The `<Type>_` prefix guarantees collision-free naming
    /// across types that share a method name.
    fn freefn_lowered_name(&self, type_name: &str, method_name: &str, is_public: bool) -> String {
        format!(
            "{type_name}_{}",
            self.go_method_name(method_name, is_public)
        )
    }

    /// Pre-scan top-level functions whose declared return type is `Optional[T]`,
    /// recording `fn name → Go element type` of `T`. This lets a `match` whose
    /// scrutinee is a call to such a function (`match next(it) { Some(x) => ...
    /// }`) type-assert the bound payload to its concrete type. Must run before
    /// any match is emitted, so it covers forward references within the module.
    /// Pre-scan every top-level `type X = …` alias, recording `X → underlying
    /// type AIR node`. Lets the emitter resolve an alias *name* to its target Go
    /// type ([`Self::resolve_type_alias`]) wherever it appears — a function
    /// return/param type, or a `Result`/`Optional` element scan — so an alias to a
    /// runtime container (`type ParseResult = Result[...]`) lowers identically to
    /// the inlined container. Only non-generic aliases are recorded (a generic
    /// alias `type Pair[A, B] = (A, B)` would need substitution the emitter does
    /// not perform; it is left to fall through to its existing rendering).
    fn collect_type_aliases(&mut self, module: &AIRNode) {
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                if let NodeKind::TypeAlias {
                    name,
                    generic_params,
                    ty,
                    ..
                } = &item.kind
                {
                    if generic_params.is_empty() {
                        self.type_aliases
                            .entry(name.name.clone())
                            .or_insert_with(|| (**ty).clone());
                    }
                }
            }
        }
    }

    /// If `node` is a `TypeNamed` naming a recorded non-generic type alias, return
    /// its underlying type AIR node (following at most one level — Bock aliases do
    /// not chain in practice, and a bounded depth avoids any cycle). `None` for a
    /// non-alias or non-`TypeNamed` node.
    fn resolve_type_alias(&self, node: &AIRNode) -> Option<&AIRNode> {
        if let NodeKind::TypeNamed { path, args } = &node.kind {
            if args.is_empty() {
                if let Some(seg) = path.segments.last() {
                    return self.type_aliases.get(&seg.name);
                }
            }
        }
        None
    }

    fn collect_optional_returns(&mut self, module: &AIRNode) {
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                // Effect operations are dispatched by bare name (`read(key)` with
                // the handler resolved implicitly), so the AIR lowers a call to one
                // as `Call(Identifier "read", ...)` — indistinguishable from a free
                // fn at the call site. Record each effect op's `Optional`/`Result`
                // return into the by-name maps so a `match` on such a call (or on a
                // binding of it) type-asserts the boxed payload concretely. Without
                // this, the bound `interface{}` payload fails a later concrete use
                // (effect-showcase: `raw := storage.read(key); match raw { Some(v)
                // => return v }`, `v` wanted as `string`).
                if let NodeKind::EffectDecl { operations, .. } = &item.kind {
                    for op in operations {
                        if let NodeKind::FnDecl {
                            name,
                            return_type: Some(rt),
                            ..
                        } = &op.kind
                        {
                            if let Some(elem) = self.optional_elem_go_type(rt) {
                                self.fn_optional_ret_elem
                                    .entry(name.name.clone())
                                    .or_insert(elem);
                            }
                            if let Some(elems) = self.result_elem_go_types(rt) {
                                self.fn_result_ret_elem
                                    .entry(name.name.clone())
                                    .or_insert(elems);
                            }
                        }
                    }
                }
                if let NodeKind::FnDecl {
                    name,
                    return_type: Some(rt),
                    ..
                } = &item.kind
                {
                    if let Some(elem) = self.optional_elem_go_type(rt) {
                        self.fn_optional_ret_elem.insert(name.name.clone(), elem);
                    }
                    // Same pre-scan for `Result[T, E]` returns, so a `match
                    // parse(s) { Ok(n) => ... }` on a call scrutinee asserts the
                    // bound payload's Ok/Err type (mirrors the Optional path).
                    if let Some(elems) = self.result_elem_go_types(rt) {
                        self.fn_result_ret_elem.insert(name.name.clone(), elems);
                    }
                }
            }
        }
    }

    /// Pre-scan every impl/class/trait block for methods whose declared return
    /// type is `Optional[T]`, recording `method name → Go element type` of `T`.
    /// This lets a `match` whose scrutinee is a method call
    /// (`match it.next() { Some(x) => ... }`) type-assert the bound payload to
    /// its concrete element type — the shape `for x in <user-Iterable>` desugars
    /// to (a `loop`/`while` over `it.next(): T?`). Without it the payload stays
    /// the runtime `interface{}` and any typed use (`sum + x`) fails `go build`.
    ///
    /// Keyed by method name only — the Go backend works from the AIR, not the
    /// checker's per-type `method_types`. If the same method name appears on two
    /// types with *different* Optional element types, the entry is poisoned (its
    /// value cleared and a sentinel recorded) so resolution returns `None` and
    /// the payload safely falls back to `interface{}`. Must run before any match
    /// is emitted so it covers forward references within the module.
    fn collect_method_optional_returns(&mut self, module: &AIRNode) {
        // Methods sharing a name but disagreeing on element type are ambiguous;
        // track them here so the final map omits them entirely.
        let mut poisoned: HashSet<String> = HashSet::new();
        let mut poisoned_record: HashSet<String> = HashSet::new();
        let mut poisoned_go: HashSet<String> = HashSet::new();
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                // The item's in-scope generic-param names: an impl's own plus
                // the target record's (`impl ListIterator { ... }` inherits the
                // `[T]` from `record ListIterator[T]`); a trait's declared
                // params. A method whose record return names one of these is
                // *not* a concrete return (it is the generic declaration), so it
                // is excluded from `method_ret_record_args`.
                let (methods, item_params): (&Vec<AIRNode>, Vec<String>) = match &item.kind {
                    NodeKind::ImplBlock {
                        methods,
                        generic_params,
                        target,
                        ..
                    } => {
                        let mut ps: Vec<String> =
                            generic_params.iter().map(|p| p.name.name.clone()).collect();
                        let target_name = self.type_expr_to_string(target);
                        if let Some(decl) = self.generic_decls.get(&target_name) {
                            ps.extend(decl.iter().map(|p| p.name.name.clone()));
                        }
                        (methods, ps)
                    }
                    NodeKind::ClassDecl {
                        methods,
                        generic_params,
                        ..
                    }
                    | NodeKind::TraitDecl {
                        methods,
                        generic_params,
                        ..
                    } => (
                        methods,
                        generic_params.iter().map(|p| p.name.name.clone()).collect(),
                    ),
                    _ => continue,
                };
                for m in methods {
                    if let NodeKind::FnDecl {
                        name,
                        return_type: Some(rt),
                        ..
                    } = &m.kind
                    {
                        if let Some(elem) = self.optional_elem_go_type(rt) {
                            match self.method_optional_ret_elem.get(&name.name) {
                                Some(existing) if *existing != elem => {
                                    poisoned.insert(name.name.clone());
                                }
                                _ => {
                                    self.method_optional_ret_elem
                                        .insert(name.name.clone(), elem);
                                }
                            }
                        }
                        // A method returning a *concrete* generic-record apply
                        // (`iter() -> ListIterator[Int]`, no remaining param) is
                        // recorded so an untyped binding of its call resolves the
                        // record args (`for x in bag` → `__it := bag.Iter()`).
                        // A return still naming an in-scope generic param (the
                        // `Iterable[T]` trait decl's `iter() -> ListIterator[T]`)
                        // is skipped — it is the generic signature, not a
                        // concrete return, and would falsely poison the concrete
                        // impl's entry.
                        if let Some(args) = self.record_type_args(rt) {
                            let is_concrete =
                                !args.1.iter().any(|a| item_params.iter().any(|p| p == a));
                            if is_concrete {
                                match self.method_ret_record_args.get(&name.name) {
                                    Some(existing) if *existing != args => {
                                        poisoned_record.insert(name.name.clone());
                                    }
                                    _ => {
                                        self.method_ret_record_args.insert(name.name.clone(), args);
                                    }
                                }
                            }
                        }
                        // Record the method's concrete Go return type, so
                        // `infer_go_expr_type` can type a `recv.method()` call
                        // (chiefly a `.map`/`.filter` closure body). A return
                        // type that still names an in-scope generic param is
                        // skipped — it is the generic signature, not concrete, and
                        // the calling site (a different fn) has no such `T`.
                        if !Self::type_mentions_params(rt, &item_params) {
                            let go_ty = self.type_to_go(rt);
                            match self.method_return_go_types.get(&name.name) {
                                Some(existing) if *existing != go_ty => {
                                    poisoned_go.insert(name.name.clone());
                                }
                                _ => {
                                    self.method_return_go_types.insert(name.name.clone(), go_ty);
                                }
                            }
                        }
                    }
                }
            }
        }
        for name in &poisoned {
            self.method_optional_ret_elem.remove(name);
        }
        for name in &poisoned_record {
            self.method_ret_record_args.remove(name);
        }
        for name in &poisoned_go {
            self.method_return_go_types.remove(name);
        }
    }

    /// If `node` is an `Optional[T]` type expression, return the Go type of its
    /// element `T`; otherwise `None`. Used to type-assert the `interface{}`
    /// payload of the Go Optional runtime back to its concrete element type at
    /// `match` arms. The element type is reachable structurally here because it
    /// lives in the `TypeOptional`/`Optional`-named node, unlike at the
    /// scrutinee expression (whose carried `type_info` is a stub).
    fn optional_elem_go_type(&self, node: &AIRNode) -> Option<String> {
        // See through a `type X = Optional[T]` alias (mirrors
        // `result_elem_go_types`).
        if let Some(target) = self.resolve_type_alias(node) {
            return self.optional_elem_go_type(target);
        }
        match &node.kind {
            NodeKind::TypeOptional { inner } => Some(self.type_to_go(inner)),
            NodeKind::TypeNamed { path, args } => {
                let is_optional = path.segments.last().is_some_and(|s| s.name == "Optional");
                if is_optional {
                    Some(
                        args.first()
                            .map_or_else(|| "interface{}".to_string(), |a| self.type_to_go(a)),
                    )
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// If `node` is a `Result[T, E]` type expression, return the Go types of its
    /// `Ok` and `Err` payloads `(T, E)`; otherwise `None`. The Result analogue of
    /// [`Self::optional_elem_go_type`]: used to type-assert the `interface{}`
    /// payload of the Go Result runtime back to its concrete Ok/Err type at a
    /// `match` arm. A missing arg defaults to `interface{}`.
    fn result_elem_go_types(&self, node: &AIRNode) -> Option<(String, String)> {
        // See through a `type X = Result[T, E]` alias so a fn declared to return
        // the alias still records its Ok/Err payload types.
        if let Some(target) = self.resolve_type_alias(node) {
            return self.result_elem_go_types(target);
        }
        if let NodeKind::TypeNamed { path, args } = &node.kind {
            if path.segments.last().is_some_and(|s| s.name == "Result") {
                let ok = args
                    .first()
                    .map_or_else(|| "interface{}".to_string(), |a| self.type_to_go(a));
                let err = args
                    .get(1)
                    .map_or_else(|| "interface{}".to_string(), |a| self.type_to_go(a));
                return Some((ok, err));
            }
        }
        None
    }

    /// If `node` is a `List[T]` type expression, return the Go type of its
    /// element `T`; otherwise `None`. The read-only `List` built-ins
    /// `get`/`first`/`last` return `Optional[T]` over the list element, so a
    /// `match` on such a call must type-assert the `interface{}` payload to this
    /// element type. Reached structurally from the receiver's declared
    /// `List[T]` type (its element is unrecoverable from the runtime
    /// `[]interface{}` value alone).
    fn list_elem_go_type(&self, node: &AIRNode) -> Option<String> {
        if let NodeKind::TypeNamed { path, args } = &node.kind {
            if path.segments.last().is_some_and(|s| s.name == "List") {
                return args.first().map(|a| self.type_to_go(a));
            }
        }
        None
    }

    /// If `node` is a `Map[K, V]` type expression, return the Go types of its
    /// key and value `(K, V)`; otherwise `None`. The `Map` analogue of
    /// [`Self::list_elem_go_type`]: the built-in `Map` methods lower to inline
    /// closures over the concretely-typed receiver `map[K]V`, so a typed `let m:
    /// Map[K, V]` binding records these into [`Self::var_map_kv`]. A missing arg
    /// defaults to `interface{}`.
    fn map_kv_go_types(&self, node: &AIRNode) -> Option<(String, String)> {
        if let NodeKind::TypeNamed { path, args } = &node.kind {
            if path.segments.last().is_some_and(|s| s.name == "Map") {
                let k = args
                    .first()
                    .map_or_else(|| "interface{}".to_string(), |a| self.type_to_go(a));
                let v = args
                    .get(1)
                    .map_or_else(|| "interface{}".to_string(), |a| self.type_to_go(a));
                return Some((k, v));
            }
        }
        None
    }

    /// If `node` is a `Set[E]` type expression, return the Go type of its
    /// element `E`; otherwise `None`. The `Set` analogue of
    /// [`Self::list_elem_go_type`], recorded into [`Self::var_set_elem`].
    fn set_elem_go_type(&self, node: &AIRNode) -> Option<String> {
        if let NodeKind::TypeNamed { path, args } = &node.kind {
            if path.segments.last().is_some_and(|s| s.name == "Set") {
                return Some(
                    args.first()
                        .map_or_else(|| "interface{}".to_string(), |a| self.type_to_go(a)),
                );
            }
        }
        None
    }

    /// If `node` is an `Optional[T]` (or `T?`) type expression, return its inner
    /// `T` type node. The *node*-returning analogue of
    /// [`Self::optional_elem_go_type`]: lets the pattern recursion peel one
    /// Optional layer and keep descending a nested constructor pattern
    /// (`Some(Ok((a, b)))`) so a leaf tuple pattern lands on a concrete tuple
    /// type node. Sees through a `type X = Optional[T]` alias.
    fn optional_inner_type_node<'a>(&'a self, node: &'a AIRNode) -> Option<&'a AIRNode> {
        if let Some(target) = self.resolve_type_alias(node) {
            return self.optional_inner_type_node(target);
        }
        match &node.kind {
            NodeKind::TypeOptional { inner } => Some(inner),
            NodeKind::TypeNamed { path, args }
                if path.segments.last().is_some_and(|s| s.name == "Optional") =>
            {
                args.first()
            }
            _ => None,
        }
    }

    /// Clone `ret`'s function-type node when the declared return type is a
    /// function type (`Fn(Int) -> Int`), else `None`. Kept on
    /// [`Self::current_fn_ret_type_node`] so a lambda in tail-return position can
    /// take its param/return Go types from the declared function type. (Does not
    /// peel a `type` alias to a function type — a function returning such an
    /// alias is vanishingly rare in the v1 examples and the lambda still emits
    /// correctly via inference, just un-typed.)
    fn fn_type_ret_node(ret: Option<&AIRNode>) -> Option<AIRNode> {
        let node = ret?;
        if matches!(node.kind, NodeKind::TypeFunction { .. }) {
            Some(node.clone())
        } else {
            None
        }
    }

    /// The `(param_go_types, return_go_type)` of a `TypeFunction` node, each
    /// rendered as Go. Used to type a lambda emitted in return position from the
    /// enclosing function's declared `Fn(...) -> ...` return type. Returns `None`
    /// when `node` is not a function type.
    fn fn_type_go_signature(&self, node: &AIRNode) -> Option<(Vec<String>, String)> {
        if let NodeKind::TypeFunction { params, ret, .. } = &node.kind {
            let param_tys = params.iter().map(|p| self.type_to_go(p)).collect();
            return Some((param_tys, self.type_to_go(ret)));
        }
        None
    }

    /// When `tail` is a bare lambda being emitted in *tail-return position* and
    /// the enclosing function's declared return type is a function type
    /// (`fn compose(...) -> Fn(Int) -> Int { (x) => f(g(x)) }`), pin the lambda's
    /// param/return Go types from that declared type, so the emitted closure is
    /// `func(x int64) int64` rather than the inference fallback
    /// `func(x interface{}) interface{}` (not assignable to the declared return).
    /// Returns the saved `(expected_lambda_param_types, forced_lambda_ret)` so the
    /// caller restores them after the emit; a no-op (saved state unchanged) when
    /// the tail is not a lambda or the return type is not a function type.
    #[allow(clippy::type_complexity)]
    fn pin_return_lambda_types(&mut self, tail: &AIRNode) -> (Option<Vec<String>>, Option<String>) {
        let saved = (
            self.expected_lambda_param_types.clone(),
            self.forced_lambda_ret.clone(),
        );
        if !matches!(tail.kind, NodeKind::Lambda { .. }) {
            return saved;
        }
        if let Some(node) = &self.current_fn_ret_type_node {
            if let Some((param_tys, ret_ty)) = self.fn_type_go_signature(node) {
                self.expected_lambda_param_types = Some(param_tys);
                self.forced_lambda_ret = Some(ret_ty);
            }
        }
        saved
    }

    /// If `node` is a `Result[T, E]` type expression, return its `(T, E)` inner
    /// type nodes. The *node*-returning analogue of
    /// [`Self::result_elem_go_types`], used to peel a Result layer while
    /// descending a nested constructor pattern. Sees through a
    /// `type X = Result[T, E]` alias.
    fn result_inner_type_nodes<'a>(
        &'a self,
        node: &'a AIRNode,
    ) -> Option<(&'a AIRNode, Option<&'a AIRNode>)> {
        if let Some(target) = self.resolve_type_alias(node) {
            return self.result_inner_type_nodes(target);
        }
        if let NodeKind::TypeNamed { path, args } = &node.kind {
            if path.segments.last().is_some_and(|s| s.name == "Result") {
                return args.first().map(|ok| (ok, args.get(1)));
            }
        }
        None
    }

    /// The `(K, V)` Go types of a `Map` *value* expression used as the receiver
    /// of a built-in map method. Recovered from a declared `Map[K, V]`
    /// identifier (via [`Self::var_map_kv`]) or a homogeneously-typed map
    /// literal. `None` ⇒ the caller falls back to `interface{}` (never a wrong
    /// type).
    /// The Go type of a compose-desugar lambda's sole parameter.
    ///
    /// `f >> g` lowers (in shared AIR) to `(__compose_x) => g(f(__compose_x))`
    /// with an *untyped* `__compose_x`. The composed value's input type is the
    /// input type of the inner function `f`, so recover `f`'s first declared
    /// parameter type (via [`Self::fn_param_types`]) and render it as Go. Returns
    /// `None` for any non-compose lambda, or when `f`'s param type can't be
    /// resolved (then the param stays `interface{}`, never a wrong type).
    fn compose_lambda_param_go_type(&self, params: &[AIRNode], body: &AIRNode) -> Option<String> {
        // Exactly one param, a `BindPat` (the synthetic `__compose_x`).
        let [param] = params else {
            return None;
        };
        let NodeKind::Param {
            pattern, ty: None, ..
        } = &param.kind
        else {
            return None;
        };
        let NodeKind::BindPat { name, .. } = &pattern.kind else {
            return None;
        };
        if name.name != "__compose_x" {
            return None;
        }
        // Body is `g(f(__compose_x))`; reach the inner call `f(__compose_x)`.
        let NodeKind::Call {
            args: outer_args, ..
        } = &body.kind
        else {
            return None;
        };
        let inner = &outer_args.first()?.value;
        let NodeKind::Call { callee: f, .. } = &inner.kind else {
            return None;
        };
        self.compose_input_go_type(f)
    }

    /// The Go type of the *input* a composed callee `f` accepts — used to type a
    /// `>>`-compose lambda's synthetic `__compose_x` parameter.
    ///
    /// A named function (`Identifier`) yields its first declared parameter type.
    /// A *nested* compose (chained `f >> g >> h` desugars to a `(__compose_x) =>
    /// h(g(f(__compose_x)))` whose innermost callee `f` is itself a compose
    /// lambda) recurses through that lambda's own desugared shape to the
    /// innermost named function. Without the recursion an `f >> g >> h` chain's
    /// *outer* compose param fell back to `interface{}` on Go (only the innermost
    /// compose's param was typed), so passing `composeX` into the typed inner
    /// closure needed a type assertion Go rejected (Q-nested-compose-jstsgo, Go
    /// portion). Mirrors py's `emit_callee`/rust's `emit_callee_rs` parens
    /// strategy at the *typing* level.
    fn compose_input_go_type(&self, callee: &AIRNode) -> Option<String> {
        match &callee.kind {
            NodeKind::Identifier { name } => {
                let first_param = self.fn_param_types.get(&name.name)?.first()?.as_ref()?;
                Some(self.type_to_go(first_param))
            }
            // A nested compose lambda (`(__compose_x) => g(f(__compose_x))`): its
            // own input type is whatever its innermost composed callee `f`
            // accepts. Recurse through the same desugared shape.
            NodeKind::Lambda { params, body } => self.compose_lambda_param_go_type(params, body),
            _ => None,
        }
    }

    fn map_receiver_kv_go_types(&self, recv: &AIRNode) -> Option<(String, String)> {
        match &recv.kind {
            NodeKind::Identifier { name } => {
                self.var_map_kv.get(&go_value_ident(&name.name)).cloned()
            }
            NodeKind::MapLiteral { entries } => {
                let keys: Vec<&AIRNode> = entries.iter().map(|e| &e.key).collect();
                let vals: Vec<&AIRNode> = entries.iter().map(|e| &e.value).collect();
                match (
                    self.infer_homogeneous_elem_type_refs(&keys),
                    self.infer_homogeneous_elem_type_refs(&vals),
                ) {
                    (Some(k), Some(v)) => Some((k, v)),
                    _ => None,
                }
            }
            // A `self.field` map receiver inside an impl method: the field's
            // `Map[K, V]` types are recorded per record (mirrors the `List`
            // case in `list_receiver_elem_go_type`).
            NodeKind::FieldAccess { object, field } if matches!(&object.kind, NodeKind::Identifier { name } if name.name == "self") =>
            {
                let record = self.current_self_record.as_ref()?;
                self.record_field_map_kv
                    .get(record)
                    .and_then(|m| m.get(&field.name))
                    .cloned()
            }
            // A `value.field` map receiver where `value` is a variable of a known
            // record type (`report.by_category.get(k)` for `report: Report`,
            // `record Report { by_category: Map[String, Float] }`). The variable's
            // Go type names the record; the field's recorded `Map[K, V]` types
            // give the closure's `map[K]V` rather than the erased
            // `map[interface{}]interface{}` Go rejects against the concrete field.
            NodeKind::FieldAccess { object, field } => {
                let NodeKind::Identifier { name } = &object.kind else {
                    return None;
                };
                let obj_go_ty = self.var_go_type.get(&go_value_ident(&name.name))?;
                let record = Self::go_type_record_head(obj_go_ty);
                self.record_field_map_kv
                    .get(record)
                    .and_then(|m| m.get(&field.name))
                    .cloned()
            }
            _ => None,
        }
    }

    /// The Go element type of a `List` *value* expression, so an untyped binding
    /// to it (`let updated = items.map(..)`, `let evens = xs.filter(..)`) records
    /// its element type — letting a *chained* combinator on the binding
    /// (`updated.map((it) => it.title)`) type its closure param and a later use as
    /// a typed call argument keep `[]T` rather than erasing to `[]interface{}`.
    ///
    /// Handles a homogeneous list literal directly, and the closure-taking
    /// combinators `filter`/`map`/`flat_map` whose *receiver* element type is
    /// recoverable: `filter` preserves the element; `map` yields the closure's
    /// inferred return type (with the receiver element pinned as the closure
    /// param type); `flat_map` yields that return type's slice element. `None`
    /// when the element can't be recovered (the caller leaves the binding untyped
    /// — never a wrong type).
    fn value_list_elem_go_type(&mut self, value: &AIRNode) -> Option<String> {
        // A homogeneous list literal / typed `List[T]` identifier directly.
        if let Some(slice) = self.infer_go_expr_type(value) {
            if let Some(elem) = slice.strip_prefix("[]") {
                return Some(elem.to_string());
            }
        }
        // A builtin String-method call returning a concretely-typed list
        // (`s.split(..)` → `[]string`): the binding's element is known without
        // any combinator analysis (Q-go-split-combinator-typing — lets a `let
        // raw = s.split(..)` record `string` so a chain on the binding types).
        if let Some(elem) = Self::string_list_builtin_elem(value) {
            return Some(elem);
        }
        // A list combinator (`xs.map(..)`, `xs.filter(..)`) reaches codegen as the
        // desugared `Call(FieldAccess(xs, "map"), [cb])`, not a `MethodCall`;
        // recognise it through the shared desugar resolver.
        let NodeKind::Call { callee, args, .. } = &value.kind else {
            return None;
        };
        let (recv, method, rest) =
            crate::generator::desugared_list_functional_method(value, callee, args)?;
        // The cheap `&self` receiver resolver covers bindings/literals and the
        // element-preserving chained combinators; recurse through this *value*
        // resolver otherwise so a `map`/`flat_map` link in the chain (whose
        // element is its closure's return type) doesn't sever element recovery
        // for everything chained after it (`split(..).map(..).filter(..)`,
        // Q-go-split-combinator-typing).
        let recv_elem = self
            .list_receiver_elem_go_type(recv)
            .or_else(|| self.value_list_elem_go_type(recv))?;
        match method {
            "filter" => Some(recv_elem),
            "map" | "flat_map" => {
                let cb = rest.first()?;
                let NodeKind::Lambda { params, body } = &cb.value.kind else {
                    return None;
                };
                let saved = self.enter_param_go_types_with_expected(params, Some(&[recv_elem]));
                let ret = self.infer_block_tail_type(body);
                self.var_go_type = saved;
                let ret = ret?;
                if method == "flat_map" {
                    ret.strip_prefix("[]").map(str::to_string)
                } else {
                    Some(ret)
                }
            }
            _ => None,
        }
    }

    /// The element Go type of a `Set` *value* expression used as the receiver of
    /// a built-in set method. Recovered from a declared `Set[E]` identifier (via
    /// [`Self::var_set_elem`]) or a homogeneously-typed set literal. `None` ⇒
    /// `interface{}` fallback.
    fn set_receiver_elem_go_type(&self, recv: &AIRNode) -> Option<String> {
        match &recv.kind {
            NodeKind::Identifier { name } => {
                self.var_set_elem.get(&go_value_ident(&name.name)).cloned()
            }
            NodeKind::SetLiteral { elems } => self.infer_homogeneous_elem_type(elems),
            _ => None,
        }
    }

    /// Infer the `(K, V)` Go types of a `Map`-typed *value* expression — a map
    /// literal, a known `Map` identifier, or a `Map` built-in method that
    /// returns the receiver map (`set`/`delete`/`merge`/`filter`). Lets an
    /// untyped `let m2 = base.set(k, v)` propagate `base`'s key/value types onto
    /// `m2` so a subsequent `m2.get(k)` closure is well-typed. `None` ⇒
    /// `interface{}` fallback.
    fn value_map_kv_go_types(&self, value: &AIRNode) -> Option<(String, String)> {
        if let Some(kv) = self.map_receiver_kv_go_types(value) {
            return Some(kv);
        }
        if let NodeKind::Call { callee, args, .. } = &value.kind {
            if let Some((recv, method, _)) =
                crate::generator::desugared_map_method(value, callee, args)
            {
                if matches!(method, "set" | "delete" | "merge" | "filter") {
                    return self.value_map_kv_go_types(recv);
                }
            }
        }
        None
    }

    /// Infer the element Go type of a `Set`-typed *value* expression — a set
    /// literal, a known `Set` identifier, or a `Set` built-in returning the
    /// receiver set (`add`/`remove`/`union`/`intersection`/`difference`/
    /// `filter`/`map`). The `Set` analogue of [`Self::value_map_kv_go_types`].
    fn value_set_elem_go_type(&self, value: &AIRNode) -> Option<String> {
        if let Some(elem) = self.set_receiver_elem_go_type(value) {
            return Some(elem);
        }
        if let NodeKind::Call { callee, args, .. } = &value.kind {
            if let Some((recv, method, _)) =
                crate::generator::desugared_set_method(value, callee, args)
            {
                if matches!(
                    method,
                    "add" | "remove" | "union" | "intersection" | "difference" | "filter" | "map"
                ) {
                    return self.value_set_elem_go_type(recv);
                }
            }
        }
        None
    }

    /// Infer the concrete generic-record instantiation a *value* expression
    /// produces — `("ListIterator", ["int64"])` for `list_iter([1, 2, 3])` or
    /// `bag.iter()`. Resolved for: a call to a generic fn whose return type is a
    /// generic record ([`Self::infer_go_expr_type`]'s `Call` arm), and a method
    /// call (direct or the desugared `Call(FieldAccess(recv, m), [recv, ..])`
    /// shape) whose method has a recorded concrete record return
    /// ([`Self::method_ret_record_args`]). Used to record an untyped binding's
    /// record args so a later `match binding.next() { Some(x) => ... }` asserts
    /// the payload concretely. `None` when not structurally determinable.
    fn value_record_type_args(&self, value: &AIRNode) -> Option<(String, Vec<String>)> {
        // A method whose declared return is a concrete generic record.
        match &value.kind {
            NodeKind::MethodCall { method, .. } => {
                if let Some(args) = self.method_ret_record_args.get(&method.name) {
                    return Some(args.clone());
                }
            }
            NodeKind::Call { callee, args, .. } => {
                // The AIR also lowers `recv.m(rest)` to `Call(FieldAccess(recv,
                // m), [recv, ...])`.
                if let Some((_recv, method, _)) =
                    crate::generator::desugared_self_call(callee, args)
                {
                    if let Some(ra) = self.method_ret_record_args.get(&method.name) {
                        return Some(ra.clone());
                    }
                }
            }
            _ => {}
        }
        // A free generic-fn call resolving to a concrete record return
        // (`list_iter([...])` → `ListIterator[int64]`): parse the rendered type.
        let go_ty = self.infer_go_expr_type(value)?;
        let open = go_ty.find('[')?;
        if !go_ty.ends_with(']') {
            return None;
        }
        let base = go_ty[..open].to_string();
        if self.generic_decls.get(&base).is_none_or(|p| p.is_empty()) {
            return None;
        }
        let arg_str = &go_ty[open + 1..go_ty.len() - 1];
        let args = Self::split_top_level_commas(arg_str);
        if args.is_empty() {
            return None;
        }
        Some((base, args))
    }

    /// Infer the `(ok, err)` Go payload types a `Result`-typed *value* expression
    /// produces. The `Result` analogue of [`Self::value_map_kv_go_types`]:
    /// resolves a bare `Ok(v)` / `Err(e)` constructor's present arm from the
    /// payload expression (the absent arm stays `interface{}`), and a call to a
    /// function whose declared return is a `Result` via the existing
    /// [`Self::scrutinee_result_elems`] (fn-return / variable maps). Used to
    /// record an *untyped* binding's `(ok, err)` into [`Self::var_result_elem`]
    /// (`step1 := eval(...)`, with no `: Result[..]` annotation), so a later
    /// `match step1 { Ok(v) => ...; Err(e) => ... }` type-asserts the
    /// `interface{}` payload concretely rather than binding it bare (which leaves
    /// `v` as `interface{}` and fails a later use expecting the concrete type).
    fn value_result_elem_go_types(&self, value: &AIRNode) -> Option<(String, String)> {
        if let NodeKind::Call { callee, args, .. } = &value.kind {
            if let NodeKind::Identifier { name } = &callee.kind {
                match name.name.as_str() {
                    "Ok" => {
                        let ok = args
                            .first()
                            .and_then(|a| self.infer_go_expr_type(&a.value))
                            .unwrap_or_else(|| "interface{}".to_string());
                        return Some((ok, "interface{}".to_string()));
                    }
                    "Err" => {
                        let err = args
                            .first()
                            .and_then(|a| self.infer_go_expr_type(&a.value))
                            .unwrap_or_else(|| "interface{}".to_string());
                        return Some(("interface{}".to_string(), err));
                    }
                    _ => {}
                }
            }
        }
        self.scrutinee_result_elems(value)
    }

    /// For a `List[T]` / `Set[T]` / `Map[K, V]` type expression, the declared
    /// Go element types as `(elem_or_key, value)`: `List`/`Set` yield
    /// `(T, None)`; `Map` yields `(K, Some(V))`. A missing type arg defaults to
    /// `interface{}`. `None` for any non-collection type. Used to set
    /// [`Self::expected_collection_elem`] so a literal in a typed binding adopts
    /// the declared element type(s).
    /// If `node` is a *generic record* instantiation (`ListIter[Int]`), return
    /// its base name and the Go-rendered concrete type-args (`("ListIter",
    /// ["int64"])`). `None` for a non-record type, a non-generic record, or a
    /// record with no type-args. Used to record [`Self::var_record_type_args`]
    /// so a method-call scrutinee's generic `Optional[T]` payload can be resolved
    /// to the concrete instantiation at the call site.
    fn record_type_args(&self, node: &AIRNode) -> Option<(String, Vec<String>)> {
        let NodeKind::TypeNamed { path, args } = &node.kind else {
            return None;
        };
        if args.is_empty() {
            return None;
        }
        let base = path.segments.last().map(|s| s.name.clone())?;
        // Only generic records (those with a declared param list) qualify; this
        // keeps the map free of `List`/`Map`/etc. and other non-record applies.
        let params = self.generic_decls.get(&base)?;
        if params.is_empty() {
            return None;
        }
        let arg_strs: Vec<String> = args.iter().map(|a| self.type_to_go(a)).collect();
        Some((base, arg_strs))
    }

    fn collection_elem_go_types(&self, node: &AIRNode) -> Option<(String, Option<String>)> {
        let NodeKind::TypeNamed { path, args } = &node.kind else {
            return None;
        };
        let name = path.segments.last().map(|s| s.name.as_str())?;
        let arg = |i: usize| {
            args.get(i)
                .map_or_else(|| "interface{}".to_string(), |a| self.type_to_go(a))
        };
        match name {
            "List" | "Set" => Some((arg(0), None)),
            "Map" => Some((arg(0), Some(arg(1)))),
            _ => None,
        }
    }

    /// The Go element type a `for x in <iterable>` loop binds, when
    /// structurally recoverable:
    /// - an identifier whose declared `List[T]` element type is in
    ///   [`Self::var_list_elem`] (a typed `let` / parameter),
    /// - a list literal whose elements infer to one homogeneous Go type,
    /// - a range (`a..b` / `a..=b`), which yields `int64`.
    ///
    /// Returns `None` otherwise; the loop variable is then left out of the type
    /// scope and inference falls back to `interface{}` — never a wrong type.
    fn for_loop_elem_go_type(&self, iterable: &AIRNode) -> Option<String> {
        match &iterable.kind {
            NodeKind::Identifier { name } => {
                self.var_list_elem.get(&go_value_ident(&name.name)).cloned()
            }
            NodeKind::ListLiteral { elems } => self.infer_homogeneous_elem_type(elems),
            NodeKind::Range { .. } => Some("int64".to_string()),
            // `for p in s.split(",")`: the builtin lowers to `strings.Split`,
            // a concrete `[]string` (Q-go-split-combinator-typing).
            _ => Self::string_list_builtin_elem(iterable),
        }
    }

    /// The concrete Go type a builtin *String-method* call lowers to, keyed off
    /// the same checker receiver-kind annotation [`Self::try_emit_string_method`]
    /// dispatches on (so a user method named `trim` on a non-String receiver is
    /// never mistaken for the builtin). This table mirrors that emitter's
    /// lowerings exactly: `split` → `strings.Split` (`[]string`), the
    /// string-transforming methods → `string`, the length queries → `int64`, the
    /// predicates → `bool`, and the optional-returning lookups → the
    /// `__bockOption` runtime struct. Previously codegen had no return type for
    /// any of these, so a combinator chained onto `split()` — or a `.map((p) =>
    /// p.trim())` link feeding a chain — erased to `interface{}`, which Go
    /// rejects against the lowering's concrete types
    /// (Q-go-split-combinator-typing — the builtin-method sibling of the #256
    /// chained-combinator fix).
    fn string_builtin_return_go_type(
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Option<String> {
        if crate::generator::primitive_recv_kind(node) != Some("String") {
            return None;
        }
        let (_, field, _) = crate::generator::desugared_self_call(callee, args)?;
        let ty = match field.name.as_str() {
            "to_upper" | "to_lower" | "trim" | "trim_start" | "trim_end" | "reverse" | "repeat"
            | "replace" | "slice" | "substring" | "to_string" | "display" => "string",
            "split" => "[]string",
            "len" | "length" | "count" | "byte_len" => "int64",
            "is_empty" | "contains" | "starts_with" | "ends_with" => "bool",
            "char_at" | "index_of" => "__bockOption",
            _ => return None,
        };
        Some(ty.to_string())
    }

    /// The Go element type of a builtin *String-method* call returning a
    /// `List` — today only `split`, whose lowering is a concrete `[]string`
    /// (Q-go-split-combinator-typing). The slice-element view of
    /// [`Self::string_builtin_return_go_type`] for the list-element resolvers.
    fn string_list_builtin_elem(recv: &AIRNode) -> Option<String> {
        let NodeKind::Call { callee, args, .. } = &recv.kind else {
            return None;
        };
        Self::string_builtin_return_go_type(recv, callee, args)?
            .strip_prefix("[]")
            .map(str::to_string)
    }

    /// The Go slice element type of a `List` *value* expression used as the
    /// receiver of a built-in list method (`get`/`concat`/…). The list-method
    /// closures take a `[]<elem>` parameter that must match the receiver's now
    /// concretely-typed slice. Recovered from a declared `List[T]` identifier
    /// (via [`Self::var_list_elem`]) or a homogeneously-typed list literal;
    /// `None` otherwise, in which case the receiver is `[]interface{}` and the
    /// `interface{}` element default matches.
    fn list_receiver_elem_go_type(&self, recv: &AIRNode) -> Option<String> {
        match &recv.kind {
            NodeKind::Identifier { name } => {
                let key = go_value_ident(&name.name);
                // A `List[T]` binding records its element type directly. Fall back
                // to the variable's full Go type (`var_go_type`) when the
                // dedicated list-element map has no entry — a typed *lambda
                // parameter* (`(data) => data.filter(..)` for a `Fn(List[T]) ->
                // ..` return type) is recorded only there as `[]T`, so peeling the
                // `[]` prefix recovers the element. Without this the chained
                // `.filter`/`.map` closure stayed `func(x interface{})` and its
                // `[]interface{}` result did not satisfy the typed `Fn` return.
                self.var_list_elem.get(&key).cloned().or_else(|| {
                    self.var_go_type
                        .get(&key)
                        .and_then(|t| t.strip_prefix("[]"))
                        .map(str::to_string)
                })
            }
            NodeKind::ListLiteral { elems } => self.infer_homogeneous_elem_type(elems),
            // A *chained* combinator whose receiver is itself a closure-taking
            // list method (`numbers.filter(..).map(..)`): the outer method's
            // receiver is the desugared `Call(FieldAccess(numbers, "filter"),
            // [numbers, cb])`. `filter`/`find` preserve the element type, so the
            // element is recoverable from the inner receiver without inferring a
            // closure return type (which would need `&mut self`). This keeps a
            // `.filter(..).map(..)` chain's outer `.map` closure typed
            // `func(n int64)` and its result `[]int64` rather than the erased
            // `interface{}`/`[]interface{}` that Go rejects. `map`/`flat_map`
            // receivers (whose element is the closure's return type) are recovered
            // by the `&mut self` fallback in `try_emit_list_functional_method`.
            NodeKind::Call { callee, args, .. } => {
                if let Some((inner_recv, method, _)) =
                    crate::generator::desugared_list_functional_method(recv, callee, args)
                {
                    match method {
                        "filter" | "find" => self.list_receiver_elem_go_type(inner_recv),
                        _ => None,
                    }
                } else {
                    // Not a list combinator: a builtin String-method call that
                    // returns a concretely-typed list (`s.split(..)` →
                    // `strings.Split` → `[]string`) still carries a known
                    // element (Q-go-split-combinator-typing).
                    Self::string_list_builtin_elem(recv)
                }
            }
            // A `self.field` list receiver inside an impl method (`self.xs.get(i)`
            // in `record ListIter[T] { xs: List[T] }`): the field's `List[...]`
            // element type is recorded per record. `T` is in scope on the
            // method receiver, so the closure correctly takes `[]T`.
            NodeKind::FieldAccess { object, field } if matches!(&object.kind, NodeKind::Identifier { name } if name.name == "self") =>
            {
                let record = self.current_self_record.as_ref()?;
                self.record_field_list_elem
                    .get(record)
                    .and_then(|m| m.get(&field.name))
                    .cloned()
            }
            // A `value.field` list receiver where `value` is a variable of a known
            // record type (`b.items.get(i)` for `b: Box[T]`, `record Box[T] {
            // items: List[T] }`). The variable's Go type (`Box[T]`) names the
            // record; the field's recorded `List[...]` element type (`T`, in scope
            // as the enclosing generic fn's type param) gives the closure's `[]T`
            // element rather than the `[]interface{}` default — which a `[]T`
            // field-access argument does not satisfy under Go's type rules. (GAP-A:
            // a generic free fn reading `b.items.get(i)` previously emitted the
            // `.get` closure with a `[]interface{}` parameter and bound the `Some`
            // payload as `interface{}`, both rejected against `[]T`/`T`.)
            NodeKind::FieldAccess { object, field } => {
                let NodeKind::Identifier { name } = &object.kind else {
                    return None;
                };
                let obj_go_ty = self.var_go_type.get(&go_value_ident(&name.name))?;
                let record = Self::go_type_record_head(obj_go_ty);
                self.record_field_list_elem
                    .get(record)
                    .and_then(|m| m.get(&field.name))
                    .cloned()
            }
            _ => None,
        }
    }

    /// The record/type head of a Go type rendering: the identifier before any
    /// generic `[...]` arg list (`Box[T]` → `Box`, `Box[int64]` → `Box`, `Box` →
    /// `Box`). Used to key [`Self::record_field_list_elem`] from a variable's
    /// recorded Go type when resolving a `value.field` list receiver.
    fn go_type_record_head(go_ty: &str) -> &str {
        go_ty.split('[').next().unwrap_or(go_ty).trim()
    }

    /// Parse the per-field Go types out of a tuple struct rendering
    /// (`struct{ Field0 int64; Field1 string }` → `["int64", "string"]`). The
    /// inverse of `type_to_go`'s `TypeTuple` arm. Returns an empty vec for any
    /// non-tuple-struct string. Used to pin a tuple literal's field types from a
    /// declared tuple return/binding type when element inference falls short.
    fn parse_tuple_struct_field_types(go_ty: &str) -> Vec<String> {
        let inner = match go_ty
            .trim()
            .strip_prefix("struct{")
            .and_then(|s| s.strip_suffix('}'))
        {
            Some(s) => s.trim(),
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        for field in inner.split(';') {
            let field = field.trim();
            if field.is_empty() {
                continue;
            }
            // Each field is `Field<N> <ty>`; the type is everything after the
            // first whitespace-separated token.
            match field.split_once(char::is_whitespace) {
                Some((name, ty)) if name.starts_with("Field") => out.push(ty.trim().to_string()),
                _ => return Vec::new(),
            }
        }
        out
    }

    /// True when `node` is (or contains, in operand position) an identifier
    /// whose Go type is not in `scope` — i.e. an `interface{}`-typed value an
    /// arithmetic operation cannot soundly operate on. Used to keep arithmetic
    /// type-inference conservative (untyped lambda params stay `interface{}`).
    fn has_unresolved_operand(node: &AIRNode, scope: &HashMap<String, String>) -> bool {
        match &node.kind {
            NodeKind::Identifier { name } => !scope.contains_key(&go_value_ident(&name.name)),
            NodeKind::UnaryOp { operand, .. } => Self::has_unresolved_operand(operand, scope),
            NodeKind::BinaryOp { left, right, .. } => {
                Self::has_unresolved_operand(left, scope)
                    || Self::has_unresolved_operand(right, scope)
            }
            _ => false,
        }
    }

    /// Best-effort structural inference of an expression's Go type. Reaches the
    /// cases needed to (a) instantiate a generic struct construction
    /// (`Box[int64]{...}`) and (b) give a lambda a concrete return type rather
    /// than `interface{}`. Handles literals, in-scope identifiers (via
    /// [`Self::var_go_type`]), arithmetic/comparison binary ops, and unary ops.
    /// Returns `None` when the type can't be determined structurally — callers
    /// fall back to `any`/`interface{}`, never a wrong type.
    /// Structurally unify a generic-param *pattern* go-type (a `type_to_go`
    /// rendering that still names the type params, e.g. `ListIterator[T]` or
    /// `[]T`) against a *concrete* go-type string, recording each param's
    /// concrete binding into `bindings`. A bare pattern that is exactly a
    /// generic-param name binds that param to the whole concrete string;
    /// otherwise the two must share a structural skeleton (same brackets/commas
    /// in the same places) and the unification recurses into the differing
    /// segments. Conservative: a structural mismatch simply records nothing
    /// (the caller then leaves the lambda param untyped — never wrong, only
    /// loose). Only the `[`/`]`/`,` skeleton is parsed; this covers the generic
    /// container / iterator shapes the combinators use.
    fn unify_go_pattern(
        pattern: &str,
        concrete: &str,
        gp_names: &[String],
        bindings: &mut HashMap<String, String>,
    ) {
        let pat = pattern.trim();
        let con = concrete.trim();
        // A bare param name binds to the entire concrete type.
        if gp_names.iter().any(|g| g == pat) {
            bindings
                .entry(pat.to_string())
                .or_insert_with(|| con.to_string());
            return;
        }
        // Split each into (head, bracketed-args) on the first top-level `[`.
        let split = |s: &str| -> Option<(String, String)> {
            let open = s.find('[')?;
            if !s.ends_with(']') {
                return None;
            }
            Some((s[..open].to_string(), s[open + 1..s.len() - 1].to_string()))
        };
        // A slice prefix `[]elem` — split into the `[]` marker and the element.
        if let (Some(pe), Some(ce)) = (pat.strip_prefix("[]"), con.strip_prefix("[]")) {
            Self::unify_go_pattern(pe, ce, gp_names, bindings);
            return;
        }
        match (split(pat), split(con)) {
            (Some((ph, pa)), Some((ch, ca))) if ph == ch => {
                let p_args = Self::split_top_level_commas(&pa);
                let c_args = Self::split_top_level_commas(&ca);
                if p_args.len() == c_args.len() {
                    for (pp, cc) in p_args.iter().zip(c_args.iter()) {
                        Self::unify_go_pattern(pp, cc, gp_names, bindings);
                    }
                }
            }
            _ => {}
        }
    }

    /// Split a go-type-arg list on top-level commas (commas not nested inside a
    /// `[...]`). `int64, []string` → `["int64", "[]string"]`.
    fn split_top_level_commas(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut start = 0usize;
        for (i, ch) in s.char_indices() {
            match ch {
                '[' | '(' => depth += 1,
                ']' | ')' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(s[start..i].trim().to_string());
                    start = i + 1;
                }
                _ => {}
            }
        }
        let last = s[start..].trim();
        if !last.is_empty() {
            out.push(last.to_string());
        }
        out
    }

    /// Bind a generic fn's type params to concrete go-types from its call's
    /// *non-lambda* arguments (a lambda argument is what we are specialising, so
    /// it can't drive the binding). Each argument whose Go type infers is
    /// unified against the matching declared param type. Returns the
    /// `param-name → go-type` bindings discovered.
    fn bind_fn_type_params(
        &self,
        gp_names: &[String],
        param_tys: &[Option<AIRNode>],
        args: &[bock_air::AirArg],
    ) -> HashMap<String, String> {
        let mut bindings: HashMap<String, String> = HashMap::new();
        for (i, arg) in args.iter().enumerate() {
            if matches!(arg.value.kind, NodeKind::Lambda { .. }) {
                continue;
            }
            let Some(pty) = param_tys.get(i).and_then(|p| p.as_ref()) else {
                continue;
            };
            let Some(arg_go) = self.infer_go_expr_type(&arg.value) else {
                continue;
            };
            let pattern = self.type_to_go(pty);
            Self::unify_go_pattern(&pattern, &arg_go, gp_names, &mut bindings);
        }
        bindings
    }

    /// Substitute the `param-name → go-type` bindings into a `Fn(...)` parameter
    /// type, returning the concrete Go parameter types (`[int64]` for
    /// `Fn(T) -> Bool` with `T → int64`). `None` when the param type is not a
    /// function type or a needed binding is missing (caller leaves the lambda
    /// param untyped).
    fn specialise_lambda_param_types(
        &self,
        fn_param_ty: &AIRNode,
        gp_names: &[String],
        bindings: &HashMap<String, String>,
    ) -> Option<Vec<String>> {
        // A callee param declared via a `type` alias to a function type
        // (`type Predicate = Fn(Int) -> Bool`) is a `TypeNamed`; see through it
        // to the underlying `TypeFunction` so the lambda argument still gets its
        // param types (`func(x int64) bool`, not the erased `interface{}`).
        if let Some(target) = self.resolve_type_alias(fn_param_ty) {
            return self.specialise_lambda_param_types(target, gp_names, bindings);
        }
        let NodeKind::TypeFunction { params, .. } = &fn_param_ty.kind else {
            return None;
        };
        let mut out = Vec::with_capacity(params.len());
        for p in params {
            let rendered = self.type_to_go(p);
            // Substitute each bound param name token-for-token. The rendered form
            // names params verbatim (`T`, `[]T`), so a binding maps them.
            let resolved = if let Some(b) = bindings.get(rendered.trim()) {
                b.clone()
            } else {
                let mut r = rendered.clone();
                for g in gp_names {
                    if let Some(b) = bindings.get(g) {
                        r = Self::replace_type_token(&r, g, b);
                    }
                }
                // If any generic param token remains unbound, give up (untyped).
                if gp_names.iter().any(|g| Self::contains_type_token(&r, g)) {
                    return None;
                }
                r
            };
            out.push(resolved);
        }
        Some(out)
    }

    /// Replace whole-identifier occurrences of `token` in a go-type string with
    /// `repl` (so `T` in `[]T` becomes the binding, without clobbering a `T`
    /// inside a longer identifier like `Tree`).
    fn replace_type_token(s: &str, token: &str, repl: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < s.len() {
            if s[i..].starts_with(token) {
                let before_ok = i == 0 || !Self::is_ident_byte(bytes[i - 1]);
                let after_idx = i + token.len();
                let after_ok = after_idx >= s.len() || !Self::is_ident_byte(bytes[after_idx]);
                if before_ok && after_ok {
                    out.push_str(repl);
                    i = after_idx;
                    continue;
                }
            }
            // Push one char (handle UTF-8 boundaries safely).
            let ch = s[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    /// True when `s` contains `token` as a whole identifier (used to detect an
    /// unbound generic param remaining after substitution).
    fn contains_type_token(s: &str, token: &str) -> bool {
        Self::replace_type_token(s, token, "\0") != *s
    }

    fn is_ident_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }

    /// The Go type-name a `RecordConstruct` lowers its struct literal to: the
    /// record/struct-variant name (`Item`, or `ShapeCircle` for a struct-variant
    /// construction). Mirrors the `type_name` computation in the `RecordConstruct`
    /// emission so [`Self::infer_go_expr_type`] can type a list literal of
    /// record-constructs (`[Item{...}, Item{...}]` → `[]Item`) instead of erasing
    /// it to `[]interface{}` (the GAP-A defect: `infer_go_expr_type` had no
    /// `RecordConstruct` arm, so the homogeneous-element inference failed and the
    /// `Box[T] { items: List[T] }` field literal became `[]interface{}{…}`, which
    /// `go build` rejects against the struct's `[]Item` field).
    fn record_construct_go_type_name(&self, path: &bock_ast::TypePath) -> String {
        if let Some(info) = self.user_variant_for_path(path) {
            let variant = path.segments.last().map_or("", |s| s.name.as_str());
            format!("{}{variant}", info.enum_name)
        } else {
            path.segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".")
        }
    }

    /// Build a `param → concrete Go type` substitution for a record construction
    /// from the record's declared generic-param names and the resolved type-arg
    /// suffix (`"[Key]"`, `"[T]"`, `"[any]"`). A param with no positional arg
    /// (malformed/empty suffix) is omitted. Used to specialise a `List[T]` field
    /// literal's element type at the construction site.
    fn record_param_substitution(
        &self,
        record_name: &str,
        type_args: &str,
    ) -> HashMap<String, String> {
        let mut subst = HashMap::new();
        let Some(params) = self.record_generic_param_names.get(record_name) else {
            return subst;
        };
        let args = Self::split_type_arg_suffix(type_args);
        for (param, arg) in params.iter().zip(args.iter()) {
            subst.insert(param.clone(), arg.clone());
        }
        subst
    }

    /// Split a Go type-argument suffix (`"[A, B[C]]"`) into its top-level args
    /// (`["A", "B[C]"]`), respecting bracket nesting so a nested instantiation is
    /// not split at its inner comma. Returns `[]` for an empty/absent suffix.
    fn split_type_arg_suffix(suffix: &str) -> Vec<String> {
        let inner = suffix
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or("");
        let mut args = Vec::new();
        let mut depth = 0usize;
        let mut cur = String::new();
        for ch in inner.chars() {
            match ch {
                '[' => {
                    depth += 1;
                    cur.push(ch);
                }
                ']' => {
                    depth = depth.saturating_sub(1);
                    cur.push(ch);
                }
                ',' if depth == 0 => {
                    args.push(cur.trim().to_string());
                    cur.clear();
                }
                _ => cur.push(ch),
            }
        }
        if !cur.trim().is_empty() {
            args.push(cur.trim().to_string());
        }
        args
    }

    /// Substitute whole-token generic-param occurrences in a Go type string
    /// using `subst` (`"T"` with `{T: "Key"}` → `"Key"`; `"[]T"` is handled by
    /// the caller, which passes the bare element type). Only an exact match is
    /// substituted — composite element types are rare for record list fields, so
    /// this keeps the rewrite token-precise rather than risking a substring hit.
    fn apply_type_subst(elem: &str, subst: &HashMap<String, String>) -> String {
        subst.get(elem).cloned().unwrap_or_else(|| elem.to_string())
    }

    /// Pre-scan a block's statements for declare-only temps (from the shared
    /// value-CF hoist) and record the Go type each `var __bock_cf_N T` needs.
    /// A declare-only `let` is always immediately followed by its relocated
    /// control-flow statement, whose branch values (`temp = <value>` assignments)
    /// determine the temp's type; infer it via [`Self::infer_assigned_temp_type`].
    /// Idempotent — re-records on each block entry, scoped to the temps it sees.
    fn seed_decl_only_types(&mut self, stmts: &[AIRNode]) {
        for (i, s) in stmts.iter().enumerate() {
            let NodeKind::LetBinding { pattern, .. } = &s.kind else {
                continue;
            };
            if !s.metadata.contains_key(crate::generator::DECL_ONLY_META) {
                continue;
            }
            let name = self.pattern_to_go_binding(pattern);
            // The relocated CF is the next statement; its value arms were
            // rewritten to `name = <value>` assignments, so infer the temp's Go
            // type from the assigned values. Falls back to `interface{}` (still
            // valid Go) when no assignment's value type is determinable.
            if let Some(next) = stmts.get(i + 1) {
                if let Some(ty) = self.infer_assigned_temp_type(next, &name) {
                    self.decl_only_types.insert(name, ty);
                }
            }
        }
    }

    /// Infer the common Go type assigned to temp `name` anywhere within `node`
    /// (the relocated control-flow statement of a value-CF hoist). Scans every
    /// `name = <value>` assignment and unifies their value types; returns `None`
    /// when they disagree or none is determinable. Does not descend into nested
    /// functions/lambdas.
    fn infer_assigned_temp_type(&self, node: &AIRNode, name: &str) -> Option<String> {
        fn collect<'a>(node: &'a AIRNode, name: &str, out: &mut Vec<&'a AIRNode>) {
            match &node.kind {
                NodeKind::Assign { target, value, .. } => {
                    if matches!(&target.kind, NodeKind::Identifier { name: n } if go_value_ident(&n.name) == name)
                    {
                        out.push(value);
                    }
                }
                NodeKind::FnDecl { .. } | NodeKind::Lambda { .. } => {}
                NodeKind::Block { stmts, tail } => {
                    for s in stmts {
                        collect(s, name, out);
                    }
                    if let Some(t) = tail {
                        collect(t, name, out);
                    }
                }
                NodeKind::If {
                    then_block,
                    else_block,
                    ..
                } => {
                    collect(then_block, name, out);
                    if let Some(e) = else_block {
                        collect(e, name, out);
                    }
                }
                NodeKind::Match { arms, .. } => {
                    for arm in arms {
                        if let NodeKind::MatchArm { body, .. } = &arm.kind {
                            collect(body, name, out);
                        }
                    }
                }
                NodeKind::Loop { body } | NodeKind::While { body, .. } => {
                    collect(body, name, out);
                }
                _ => {}
            }
        }
        let mut values = Vec::new();
        collect(node, name, &mut values);
        // Unify only the values whose Go type is *determinable*. An assignment
        // whose value can't be typed here (e.g. a pattern-bound `v` not yet in
        // `var_go_type`) does not constrain — mirroring how `infer_branchy_expr_type`
        // skips value-less arms. This recovers `int64` from `bockCf0 = (0-1)` even
        // when a sibling `bockCf0 = v` is opaque, rather than collapsing to the
        // unassignable `interface{}`. If the determinable ones disagree, give up.
        let mut common: Option<String> = None;
        for v in values {
            let Some(ty) = self.infer_block_tail_type(v) else {
                continue;
            };
            match &common {
                Some(c) if *c != ty => return None,
                Some(_) => {}
                None => common = Some(ty),
            }
        }
        common
    }

    /// Infer the Go value type produced by an `if`/`match` expression by
    /// inferring the type of each branch/arm's *tail* (value) and requiring them
    /// to agree. Used to type an untyped `let m = if (..) { Text } else { Image }`
    /// binding's IIFE: the inferred enum (`MessageType`) becomes the IIFE return
    /// type so a variant value is assignable, rather than the `interface{}` /
    /// enclosing-fn-return fallback. Returns `None` when any branch's type can't
    /// be inferred or the branches disagree (the caller then leaves the binding
    /// untyped, preserving the prior behavior — never a wrong type). Branches
    /// that *only* early-return (no value tail, e.g. a `return Err(..)` arm) are
    /// skipped: they exit the enclosing function rather than contributing a value.
    fn infer_branchy_expr_type(&self, node: &AIRNode) -> Option<String> {
        match &node.kind {
            NodeKind::If {
                then_block,
                else_block,
                ..
            } => {
                let then_ty = self.infer_block_tail_type(then_block);
                let else_ty = else_block
                    .as_deref()
                    .and_then(|e| self.infer_block_tail_type(e));
                match (then_ty, else_ty) {
                    (Some(a), Some(b)) if a == b => Some(a),
                    // One branch only early-returns (no value) — adopt the other.
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    _ => None,
                }
            }
            NodeKind::Match { arms, .. } => {
                let mut common: Option<String> = None;
                for arm in arms {
                    let NodeKind::MatchArm { body, .. } = &arm.kind else {
                        continue;
                    };
                    let Some(ty) = self.infer_block_tail_type(body) else {
                        // A value-less arm (early-return) does not constrain.
                        continue;
                    };
                    match &common {
                        Some(c) if *c != ty => return None,
                        Some(_) => {}
                        None => common = Some(ty),
                    }
                }
                common
            }
            _ => None,
        }
    }

    /// Infer the Go type of a block's value tail: for a `Block` the tail
    /// expression's type (a value-producing tail only — a statement tail like an
    /// early `return` yields `None`, as the block contributes no value), and for
    /// a bare expression body the expression's type. A nested `if`/`match` tail
    /// recurses through [`Self::infer_branchy_expr_type`]. `None` when no value
    /// type is determinable.
    fn infer_block_tail_type(&self, node: &AIRNode) -> Option<String> {
        match &node.kind {
            NodeKind::Block { tail, .. } => {
                let t = tail.as_deref()?;
                if crate::generator::node_is_statement(t) {
                    return None;
                }
                self.infer_block_tail_type(t)
            }
            NodeKind::If { .. } | NodeKind::Match { .. } => self.infer_branchy_expr_type(node),
            _ => self.infer_go_expr_type(node),
        }
    }

    fn infer_go_expr_type(&self, node: &AIRNode) -> Option<String> {
        match &node.kind {
            NodeKind::Literal { lit } => match lit {
                Literal::Int(_) => Some("int64".to_string()),
                Literal::Float(_) => Some("float64".to_string()),
                Literal::Bool(_) => Some("bool".to_string()),
                Literal::String(_) => Some("string".to_string()),
                Literal::Char(_) => Some("rune".to_string()),
                Literal::Unit => None,
            },
            NodeKind::Identifier { name } => {
                // A bare reference to a *unit* user-enum variant (`Text`,
                // `HealthCheck`) types to its owning sealed-interface enum
                // (`MessageType`, `Route`), so an untyped `let t = Text` — or an
                // `if`/`match` whose arms yield such variants — infers the enum
                // type rather than collapsing to `interface{}`/the enclosing fn's
                // return type. Bound locals/params still win (a variable shadowing
                // is impossible — variant names are PascalCase, value idents are
                // camelCase — but check the var map first for symmetry).
                if let Some(t) = self.var_go_type.get(&go_value_ident(&name.name)) {
                    return Some(t.clone());
                }
                // Bare `None` lowers to the runtime `__bockOption` (the nullary
                // Optional constructor); type it so a value-CF arm yielding `None`
                // unifies with a sibling `Some(x)` arm (both `__bockOption`).
                if name.name == "None" {
                    return Some("__bockOption".to_string());
                }
                self.user_variant_for_name(&name.name)
                    .map(|info| info.enum_name.clone())
            }
            NodeKind::Interpolation { .. } => Some("string".to_string()),
            NodeKind::UnaryOp { op, operand } => match op {
                UnaryOp::Not => Some("bool".to_string()),
                UnaryOp::Neg | UnaryOp::BitNot => self.infer_go_expr_type(operand),
            },
            NodeKind::BinaryOp { op, left, right } => match op {
                BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or
                | BinOp::Is => Some("bool".to_string()),
                BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Rem
                | BinOp::Pow
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor => {
                    // An arithmetic op is only soundly typed when neither operand
                    // is an *unresolved* identifier: a `func(x interface{}) ...`
                    // body of `x * 2` would not type-check in Go regardless of
                    // the literal, so leave the return type as `interface{}`
                    // rather than inferring a type the operation can't satisfy.
                    if Self::has_unresolved_operand(left, &self.var_go_type)
                        || Self::has_unresolved_operand(right, &self.var_go_type)
                    {
                        return None;
                    }
                    self.infer_go_expr_type(left)
                        .or_else(|| self.infer_go_expr_type(right))
                }
                BinOp::Compose => None,
            },
            // Collection literals so a nested collection (`[[1], [2]]`,
            // `{"k": [1, 2]}`) types its element concretely. A literal whose
            // elements infer to a single homogeneous Go type yields that
            // container type; otherwise `None` (callers fall back to
            // `interface{}`, never a wrong type).
            NodeKind::ListLiteral { elems } => self
                .infer_homogeneous_elem_type(elems)
                .map(|e| format!("[]{e}")),
            NodeKind::SetLiteral { elems } => self
                .infer_homogeneous_elem_type(elems)
                .map(|e| format!("map[{e}]struct{{}}")),
            NodeKind::MapLiteral { entries } => {
                let keys: Vec<&AIRNode> = entries.iter().map(|e| &e.key).collect();
                let vals: Vec<&AIRNode> = entries.iter().map(|e| &e.value).collect();
                match (
                    self.infer_homogeneous_elem_type_refs(&keys),
                    self.infer_homogeneous_elem_type_refs(&vals),
                ) {
                    (Some(k), Some(v)) => Some(format!("map[{k}]{v}")),
                    _ => None,
                }
            }
            // A record/struct-variant construction (`Item { id: 1 }`,
            // `Box[Item] { items: … }`) types to its Go struct name plus the
            // explicit type-arg suffix the emission would write (`Item`, or
            // `Box[int64]` when a param is recoverable from a directly-typed
            // field). This lets a list literal of record-constructs
            // (`[Item{…}, Item{…}]`) infer the homogeneous element type `Item` so
            // the `Box[T] { items: List[T] }` field literal emits `[]Item{…}`
            // rather than the erased `[]interface{}{…}` (GAP-A). Type args are
            // inferred from field values only (the `current_expected_type` used by
            // `expected_construct_type_args` names the *outer* binding, not the
            // per-element record); a generic param not directly typed by a field
            // falls back to `any`, matching the emission's loose-but-valid form.
            NodeKind::RecordConstruct { path, fields, .. } => {
                // A struct-payload variant construction (`GetUser { id: id }`)
                // types to its owning sealed-interface enum (`Route`), not the
                // variant struct (`RouteGetUser`): the value is boxed into the
                // interface at its use site, so an untyped binding / `if`-`else`
                // branch infers the assignable enum type.
                if let Some(info) = self.user_variant_for_path(path) {
                    return Some(info.enum_name.clone());
                }
                let type_name = self.record_construct_go_type_name(path);
                let type_args = self.infer_construct_type_args(&type_name, fields);
                Some(format!("{type_name}{type_args}"))
            }
            // A call to a known generic fn (`list_iter([]int64{...})`) resolves
            // to its return type with the type params bound from the arguments
            // (`ListIterator[int64]`), so a downstream call (`filter(it, ..)`)
            // can in turn bind its own params and specialise its lambda arg.
            NodeKind::Call { callee, args, .. } => {
                // A builtin String-method call types to its lowering's concrete
                // Go type (`p.trim()` → `string`, `s.split(..)` → `[]string`),
                // so a closure body / binding built from one infers concretely
                // (Q-go-split-combinator-typing). Checked before the user-method
                // return lookup: the receiver-kind annotation proves this is the
                // builtin, not a same-named user method.
                if let Some(t) = Self::string_builtin_return_go_type(node, callee, args) {
                    return Some(t);
                }
                let name = match &callee.kind {
                    NodeKind::Identifier { name } => name,
                    // A method call `recv.method(...)` lowers to a `Call` whose
                    // callee is a `FieldAccess`. Resolve it to the method's
                    // recorded Go return type (keyed by method name; the pre-scan
                    // omits names shared by methods with disagreeing returns, so a
                    // present entry is unambiguous). This types a
                    // `.map((p) => p.stock_value())` closure body to `float64`,
                    // sizing the result slice as `[]float64` rather than the
                    // erased `[]interface{}` whose elements a later `fold`'s
                    // `acc + v` cannot add.
                    NodeKind::FieldAccess { field, .. } => {
                        return self.method_return_go_types.get(&field.name).cloned();
                    }
                    _ => return None,
                };
                // The Optional/Result constructors lower to the runtime tagged
                // structs `__bockOption` / `__bockResult`, so a value-position
                // `if`/`match`/`loop` whose arms yield `Some(x)`/`None` (or `Ok`/
                // `Err`) infers that runtime type — letting the shared value-CF
                // hoist's `var __bock_cf_N __bockOption` be assignable to an
                // `Optional[T]`-returning fn (else it falls back to `interface{}`,
                // which Go rejects assigning into the typed return).
                match name.name.as_str() {
                    "Some" | "None" => return Some("__bockOption".to_string()),
                    "Ok" | "Err" => return Some("__bockResult".to_string()),
                    _ => {}
                }
                // A call to a local variable bound to a lambda (`clip_fn(x)`)
                // resolves to that lambda's recorded return type.
                if let Some(r) = self.var_lambda_ret.get(&go_value_ident(&name.name)) {
                    return Some(r.clone());
                }
                // A tuple-payload variant construction (`Circle(10)`) types to its
                // owning sealed-interface enum (`Shape`), mirroring the unit /
                // struct-payload variant cases — the variant struct is boxed into
                // the interface at its use site.
                if let Some(info) = self.user_variant_for_name(&name.name) {
                    return Some(info.enum_name.clone());
                }
                // A non-generic fn (`key`) resolves directly to its recorded Go
                // return type — no type-param binding needed. This is what types a
                // `[key(3), key(1)]` literal as `[]Key`.
                let Some((gp_names, param_tys, ret_ty)) = self.fn_signatures.get(&name.name) else {
                    return self.fn_return_go_types.get(&name.name).cloned();
                };
                let ret = ret_ty.as_ref()?;
                let bindings = self.bind_fn_type_params(gp_names, param_tys, args);
                let mut rendered = self.type_to_go(ret);
                for g in gp_names {
                    if let Some(b) = bindings.get(g) {
                        rendered = Self::replace_type_token(&rendered, g, b);
                    }
                }
                // Only return a fully-resolved type (no generic param left).
                if gp_names
                    .iter()
                    .any(|g| Self::contains_type_token(&rendered, g))
                {
                    return None;
                }
                Some(rendered)
            }
            // A bare `obj.field` access where `obj` is a variable of a known
            // record type resolves to the field's recorded Go type. This types a
            // `.map((b) => b.id)` closure body to `int64` (for `b: Block`,
            // `record Block { id: Int }`), sizing the `map` result slice as
            // `[]int64` rather than the erased `[]interface{}` Go rejects against
            // a declared `[]int64` return. `self.field` resolves through the
            // current-impl record; a non-identifier object (a chained access) is
            // left unresolved (conservative — `interface{}`, never wrong).
            NodeKind::FieldAccess { object, field } => {
                let record = match &object.kind {
                    NodeKind::Identifier { name } if name.name == "self" => {
                        self.current_self_record.clone()?
                    }
                    NodeKind::Identifier { name } => {
                        let obj_go_ty = self.var_go_type.get(&go_value_ident(&name.name))?;
                        Self::go_type_record_head(obj_go_ty).to_string()
                    }
                    _ => return None,
                };
                self.record_field_go_type
                    .get(&record)
                    .and_then(|m| m.get(&field.name))
                    .cloned()
            }
            // A `recv.method()` call resolves to the method's recorded Go return
            // type. Keyed by method name only; the pre-scan poisons (omits) any
            // name shared by methods with disagreeing return types, so a present
            // entry is unambiguous. Lets a `.map((p) => p.stock_value())` closure
            // body type to `float64`, sizing the result slice as `[]float64`.
            // A `MethodCall` node (the non-desugared method-call form) resolves
            // the same way as the `Call`-with-`FieldAccess` form above.
            NodeKind::MethodCall { method, .. } => {
                self.method_return_go_types.get(&method.name).cloned()
            }
            _ => None,
        }
    }

    /// Infer a single homogeneous Go element type for a collection literal's
    /// elements: `Some(ty)` iff the literal is non-empty and EVERY element
    /// infers (via [`Self::infer_go_expr_type`]) to the *same* concrete Go type.
    /// An empty literal, an element whose type can't be inferred, or a mix of
    /// types yields `None` — the caller then emits `interface{}`, which is never
    /// wrong (only loose). The `has_unresolved_operand` guard inside
    /// `infer_go_expr_type` already keeps arithmetic over unresolved identifiers
    /// from inferring an unsound type.
    fn infer_homogeneous_elem_type(&self, elems: &[AIRNode]) -> Option<String> {
        let refs: Vec<&AIRNode> = elems.iter().collect();
        self.infer_homogeneous_elem_type_refs(&refs)
    }

    /// `&AIRNode`-slice variant of [`Self::infer_homogeneous_elem_type`] (used
    /// for `MapLiteral` keys/values, which are not stored as a contiguous
    /// `&[AIRNode]`).
    fn infer_homogeneous_elem_type_refs(&self, elems: &[&AIRNode]) -> Option<String> {
        let mut iter = elems.iter();
        let first = self.infer_go_expr_type(iter.next()?)?;
        for e in iter {
            if self.infer_go_expr_type(e)? != first {
                return None;
            }
        }
        Some(first)
    }

    /// Build the explicit type-argument suffix (`[int64]`, `[int64, string]`)
    /// for a generic struct construction. For each of the target record's
    /// generic params (in declaration order) it finds the field whose declared
    /// type is exactly that param, then infers that field value's Go type. A
    /// param with no directly-typed field, or a value whose type can't be
    /// inferred, falls back to `any` (still a valid, if loose, instantiation).
    /// Returns `""` for a non-generic / unregistered type.
    /// The explicit Go type-argument suffix (`[int64]`) for a generic struct
    /// construction, recovered from the *declared* binding/expected type when it
    /// names this exact record (`current_expected_type == "ListIter[int64]"` for
    /// a `ListIter { ... }` construction). Returns `Some("[int64]")` then,
    /// `None` when there is no expected type, it names a different type, or it
    /// carries no args. More robust than field-value inference: it works when a
    /// generic param appears only *nested* in a field type (`xs: List[T]`),
    /// where no field is typed exactly `T`.
    fn expected_construct_type_args(&self, type_name: &str) -> Option<String> {
        let expected = self.current_expected_type.as_deref()?;
        let rest = expected.strip_prefix(type_name)?;
        // The remainder must be exactly a `[...]` type-arg list (so `ListIter`
        // does not match a hypothetical `ListIterator`); reject an empty suffix
        // (`ListIter` with no args) and anything not enclosed in brackets.
        if rest.starts_with('[') && rest.ends_with(']') && rest.len() > 2 {
            Some(rest.to_string())
        } else {
            None
        }
    }

    fn infer_construct_type_args(
        &self,
        type_name: &str,
        fields: &[bock_air::AirRecordField],
    ) -> String {
        let Some(per_param) = self.record_param_fields.get(type_name) else {
            return String::new();
        };
        if per_param.is_empty() {
            return String::new();
        }
        let args: Vec<String> = per_param
            .iter()
            .map(|field_name| {
                field_name
                    .as_ref()
                    .and_then(|fname| {
                        fields
                            .iter()
                            .find(|f| &f.name.name == fname)
                            .and_then(|f| f.value.as_deref())
                            .and_then(|v| self.infer_go_expr_type(v))
                    })
                    .unwrap_or_else(|| "any".to_string())
            })
            .collect();
        format!("[{}]", args.join(", "))
    }

    /// Record the `Optional[T]`, `List[T]`, `Map[K, V]`, `Set[E]`, and
    /// `Result[T, E]` element Go types of a function/lambda's parameters into the
    /// variable scopes, so a `match param { Some(x) => ... }` (direct Optional),
    /// `match param.get(i) { Some(x) => ... }` (List/Map built-in), or a `Set`
    /// membership test inside the body type-checks against the concrete element
    /// type. Returns the previous `(var_optional_elem, var_list_elem,
    /// var_result_elem, var_map_kv, var_set_elem)` scopes so the caller can
    /// restore them on exit (Go has no block-scoped reset here).
    #[allow(clippy::type_complexity)]
    fn enter_param_optional_scope(
        &mut self,
        params: &[AIRNode],
    ) -> (
        HashMap<String, String>,
        HashMap<String, String>,
        HashMap<String, (String, String)>,
        HashMap<String, (String, String)>,
        HashMap<String, String>,
    ) {
        let saved_opt = self.var_optional_elem.clone();
        let saved_list = self.var_list_elem.clone();
        let saved_result = self.var_result_elem.clone();
        let saved_map = self.var_map_kv.clone();
        let saved_set = self.var_set_elem.clone();
        for p in params {
            if let NodeKind::Param {
                pattern,
                ty: Some(t),
                ..
            } = &p.kind
            {
                let name = self.pattern_to_binding_name(pattern);
                // Record the full declared type node so a `match` whose
                // scrutinee is this param can peel a *nested* Optional/Result to
                // assert a tuple payload to its concrete struct (see
                // `var_decl_type_node`).
                self.var_decl_type_node.insert(name.clone(), (**t).clone());
                if let Some(elem) = self.optional_elem_go_type(t) {
                    self.var_optional_elem.insert(name.clone(), elem);
                }
                if let Some(elem) = self.list_elem_go_type(t) {
                    self.var_list_elem.insert(name.clone(), elem);
                }
                if let Some(kv) = self.map_kv_go_types(t) {
                    self.var_map_kv.insert(name.clone(), kv);
                }
                if let Some(elem) = self.set_elem_go_type(t) {
                    self.var_set_elem.insert(name.clone(), elem);
                }
                if let Some(elems) = self.result_elem_go_types(t) {
                    self.var_result_elem.insert(name.clone(), elems);
                }
                // A generic-record-typed param (`c: Counter[Int]`) records its
                // concrete instantiation so a `match c.next() { Some(x) => ... }`
                // can resolve the generic `Optional[T]` payload to the concrete
                // arg (`int64`) — see `scrutinee_optional_elem`.
                if let Some(record_args) = self.record_type_args(t) {
                    self.var_record_type_args.insert(name, record_args);
                }
            }
        }
        (saved_opt, saved_list, saved_result, saved_map, saved_set)
    }

    /// Record each typed param's Go type into [`Self::var_go_type`] so the
    /// body's expression types can be inferred (chiefly to give a lambda a
    /// concrete return type). Returns the previous map so the caller can restore
    /// it on exit. Untyped params are skipped (left absent → inference yields
    /// the `interface{}` fallback, never a wrong type).
    /// Record each param's Go type into the variable scope so the body's
    /// `infer_go_expr_type` sees concrete param types. A param whose source type
    /// is absent (an untyped lambda param) takes its type from the positional
    /// `expected` entry (the callee-specialised type, e.g. `int64`) when
    /// present, so `x > 2` / `x * 2` type-check and the lambda's inferred return
    /// type is concrete. Returns the previous scope for restore on exit. Pass
    /// `None` for `expected` when there are no specialised types (an ordinary
    /// typed lambda / fn body).
    /// The emitted Go binding names of a function/method's value parameters,
    /// used to pre-seed the body's Go block frame for shadowing-`let` tracking.
    fn param_binding_names(&self, params: &[AIRNode]) -> Vec<String> {
        params
            .iter()
            .filter_map(|p| match &p.kind {
                NodeKind::Param { pattern, .. } => {
                    let n = self.pattern_to_binding_name(pattern);
                    (n != "_").then_some(n)
                }
                _ => None,
            })
            .collect()
    }

    fn enter_param_go_types_with_expected(
        &mut self,
        params: &[AIRNode],
        expected: Option<&[String]>,
    ) -> HashMap<String, String> {
        let saved = self.var_go_type.clone();
        for (i, p) in params.iter().enumerate() {
            if let NodeKind::Param { pattern, ty, .. } = &p.kind {
                let name = self.pattern_to_binding_name(pattern);
                let go_ty = ty
                    .as_deref()
                    .map(|t| self.type_to_go(t))
                    .or_else(|| expected.and_then(|e| e.get(i).cloned()));
                if let Some(g) = go_ty {
                    self.var_go_type.insert(name, g);
                }
            }
        }
        saved
    }

    /// Render lambda params with explicit Go types drawn from `types` (one per
    /// param, positionally) — used when a lambda argument is specialised to a
    /// callee's concrete parameter types. A param with its own source
    /// annotation keeps it; otherwise the positional `types` entry is used.
    fn collect_param_strs_with_types(&self, params: &[AIRNode], types: &[String]) -> Vec<String> {
        params
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                if let NodeKind::Param { pattern, ty, .. } = &p.kind {
                    let name = self.pattern_to_binding_name(pattern);
                    let type_str = ty
                        .as_ref()
                        .map(|t| self.type_to_go(t))
                        .or_else(|| types.get(i).cloned())
                        .unwrap_or_else(|| "interface{}".into());
                    Some(format!("{name} {type_str}"))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Resolve the Go element type to assert for the payload of a `Some` bound in
    /// a `match` on `scrutinee`. Reachable for the common, structurally
    /// determinable cases: an identifier (parameter or typed `let`), a call to a
    /// function with a known `Optional[T]` return, and a *method call* whose
    /// method has a known `Optional[T]` return (`match it.next() { Some(x) =>
    /// ... }`, the shape `for x in <Iterable>` desugars to). Returns `None` when
    /// the element type cannot be determined structurally, in which case the
    /// binding is left as the runtime `interface{}` (no regression: that is the
    /// prior behavior, and `${v}`-style interpolation still works).
    /// Resolve a method-call's `Optional[T]` payload element to its CONCRETE Go
    /// type at the call site. `method_optional_ret_elem` stores the *generic*
    /// element as written on the method (`"T"`, the record's type param), which
    /// is undefined in a concrete caller such as `main`. When `receiver` is a
    /// variable bound to a concrete generic-record instantiation (recorded in
    /// [`Self::var_record_type_args`], e.g. `c: ListIter[Int]` →
    /// `("ListIter", ["int64"])`), and `elem` names one of that record's generic
    /// params, substitute the param with the corresponding concrete arg
    /// (`"T"` → `"int64"`). Otherwise `elem` is already concrete (a non-generic
    /// method, or a param-less return) and is returned unchanged.
    fn resolve_concrete_method_elem(&self, receiver: &AIRNode, elem: &str) -> String {
        let NodeKind::Identifier { name } = &receiver.kind else {
            return elem.to_string();
        };
        let Some((base, args)) = self.var_record_type_args.get(&go_value_ident(&name.name)) else {
            return elem.to_string();
        };
        let Some(params) = self.generic_decls.get(base) else {
            return elem.to_string();
        };
        // Find the generic param whose name equals `elem`, then map to the arg.
        if let Some(idx) = params.iter().position(|p| p.name.name == elem) {
            if let Some(concrete) = args.get(idx) {
                return concrete.clone();
            }
        }
        elem.to_string()
    }

    fn scrutinee_optional_elem(&self, scrutinee: &AIRNode) -> Option<String> {
        match &scrutinee.kind {
            NodeKind::Identifier { name } => self
                .var_optional_elem
                .get(&go_value_ident(&name.name))
                .cloned(),
            // A direct method call (`it.next()`).
            NodeKind::MethodCall {
                receiver, method, ..
            } => {
                let elem = self.method_optional_ret_elem.get(&method.name).cloned()?;
                Some(self.resolve_concrete_method_elem(receiver, &elem))
            }
            NodeKind::Call { callee, args, .. } => {
                // The read-only `List` built-ins `get`/`first`/`last` return
                // `Optional[<elem>]`. When the receiver is a variable with a
                // known `List[T]` element type, that element type is the payload
                // type — resolve it from `var_list_elem` before the generic
                // method-call path (whose `method_optional_ret_elem` only knows
                // *user-defined* methods, never the List built-ins).
                if let Some((recv, method, _)) =
                    crate::generator::desugared_list_method(scrutinee, callee, args)
                {
                    if matches!(method, "get" | "first" | "last") {
                        // The same receiver-element resolver the `.get` closure
                        // uses: a `List[T]` identifier (via `var_list_elem`), a
                        // homogeneous list literal, `self.field`, or a generic
                        // record param's `value.field` (`b.items.get(i)` for
                        // `b: Box[T]`). Without the last case the `Some(x)` payload
                        // stayed `interface{}` and a `return x` of a `[]T`-typed
                        // field element failed `go build` (GAP-A).
                        if let Some(elem) = self.list_receiver_elem_go_type(recv) {
                            return Some(elem);
                        }
                    }
                }
                // `Map.get(k)` returns `Optional[V]`; resolve the payload to the
                // map's value Go type so `match m.get(k) { Some(x) => … }`
                // type-asserts `x` to `V` rather than `interface{}`.
                if let Some((recv, "get", _)) =
                    crate::generator::desugared_map_method(scrutinee, callee, args)
                {
                    if let Some((_k, v)) = self.map_receiver_kv_go_types(recv) {
                        return Some(v);
                    }
                }
                match &callee.kind {
                    // Free-function call (`firstPositive(a, b)`).
                    NodeKind::Identifier { name } => {
                        self.fn_optional_ret_elem.get(&name.name).cloned()
                    }
                    // The AIR also lowers `recv.method(rest)` into
                    // `Call(FieldAccess(recv, method), [recv, ...rest])`; resolve
                    // it the same way as a direct `MethodCall` so both desugar
                    // shapes get a type-asserted payload.
                    NodeKind::FieldAccess { object, field } => {
                        crate::generator::desugared_self_call(callee, args)?;
                        let elem = self.method_optional_ret_elem.get(&field.name).cloned()?;
                        Some(self.resolve_concrete_method_elem(object, &elem))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Resolve the `(ok_go_type, err_go_type)` to assert for the payload of an
    /// `Ok`/`Err` bound in a `match` on `scrutinee`. The Result analogue of
    /// [`Self::scrutinee_optional_elem`]: an identifier (parameter or typed
    /// `let`) or a call to a function with a known `Result[T, E]` return.
    /// Returns `None` when the types cannot be determined structurally, in which
    /// case the payload falls back to the runtime `interface{}` (never wrong,
    /// only un-asserted).
    fn scrutinee_result_elems(&self, scrutinee: &AIRNode) -> Option<(String, String)> {
        match &scrutinee.kind {
            NodeKind::Identifier { name } => self
                .var_result_elem
                .get(&go_value_ident(&name.name))
                .cloned(),
            NodeKind::Call { callee, args, .. } => match &callee.kind {
                NodeKind::Identifier { name } => self.fn_result_ret_elem.get(&name.name).cloned(),
                NodeKind::FieldAccess { .. }
                    if crate::generator::desugared_self_call(callee, args).is_some() =>
                {
                    None
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// The declared type-expression AIR node of a match scrutinee, when it is a
    /// variable (parameter or typed `let`) recorded in [`Self::var_decl_type_node`].
    /// Returns `None` for any other scrutinee shape (a call, a method call, …) —
    /// the pattern recursion then leaves a nested tuple payload un-asserted (the
    /// prior `interface{}` behavior; never wrong, only un-typed). Used to seed
    /// the declared-type threading through the if-chain pattern lowering so a
    /// `match v { Some(Ok((a, b))) => … }` asserts its tuple payload.
    fn scrutinee_decl_type_node(&self, scrutinee: &AIRNode) -> Option<&AIRNode> {
        if let NodeKind::Identifier { name } = &scrutinee.kind {
            return self.var_decl_type_node.get(&go_value_ident(&name.name));
        }
        None
    }

    /// The Go payload type of an *argument expression* whose static type is
    /// `Optional[T]`, when structurally recoverable. Extends
    /// [`Self::scrutinee_optional_elem`] (identifiers via `var_optional_elem`,
    /// calls via the fn/method return-element maps) with the bare-constructor
    /// case `Some(<expr>)` / `None`, whose payload type is inferred from the
    /// payload expression. Used to pin a generic free-fn's `Optional[T]`
    /// type-parameter at the call site (Go cannot infer it: the runtime
    /// `__bockOption` struct carries no `[T]`).
    fn arg_optional_elem(&self, arg: &AIRNode) -> Option<String> {
        if let NodeKind::Call { callee, args, .. } = &arg.kind {
            if let NodeKind::Identifier { name } = &callee.kind {
                match name.name.as_str() {
                    "Some" => return args.first().and_then(|a| self.infer_go_expr_type(&a.value)),
                    // `None` carries no payload type; nothing to bind.
                    "None" => return None,
                    _ => {}
                }
            }
        }
        self.scrutinee_optional_elem(arg)
    }

    /// The `(ok, err)` Go payload types of an *argument expression* whose static
    /// type is `Result[T, E]`, when recoverable. The `Result` analogue of
    /// [`Self::arg_optional_elem`]: identifiers / calls via
    /// [`Self::scrutinee_result_elems`], plus the bare `Ok(<expr>)` / `Err(<expr>)`
    /// constructors (only the present arm's type is inferable from a bare
    /// constructor, so the other stays `None`).
    fn arg_result_elems(&self, arg: &AIRNode) -> (Option<String>, Option<String>) {
        if let NodeKind::Call { callee, args, .. } = &arg.kind {
            if let NodeKind::Identifier { name } = &callee.kind {
                match name.name.as_str() {
                    "Ok" => {
                        return (
                            args.first().and_then(|a| self.infer_go_expr_type(&a.value)),
                            None,
                        )
                    }
                    "Err" => {
                        return (
                            None,
                            args.first().and_then(|a| self.infer_go_expr_type(&a.value)),
                        )
                    }
                    _ => {}
                }
            }
        }
        match self.scrutinee_result_elems(arg) {
            Some((ok, err)) => (Some(ok), Some(err)),
            None => (None, None),
        }
    }

    /// The `Optional[T]` inner / `Result[T, E]` arg type-param names of a
    /// declared parameter (or return) AIR type, when the type is one of those
    /// containers. Returns the param-name tokens (`["T"]` for `Optional[T]`,
    /// `["T", "E"]` for `Result[T, E]`) so the caller can pair them with the
    /// argument's recovered element types. `None` for any other type.
    fn container_type_param_names(node: &AIRNode) -> Option<(&'static str, Vec<&str>)> {
        match &node.kind {
            NodeKind::TypeOptional { inner } => {
                Some(("Optional", vec![Self::type_param_token(inner)?]))
            }
            NodeKind::TypeNamed { path, args } => {
                let name = path.segments.last().map(|s| s.name.as_str())?;
                match name {
                    "Optional" => Some(("Optional", vec![Self::type_param_token(args.first()?)?])),
                    "Result" => {
                        let t = Self::type_param_token(args.first()?)?;
                        let e = Self::type_param_token(args.get(1)?)?;
                        Some(("Result", vec![t, e]))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The bare type-parameter name a type node names, if it is a single
    /// unparameterised `TypeNamed` segment (`T` → `Some("T")`). `None` for any
    /// composite or primitive type — only a bare name can be bound positionally
    /// from a container's recovered element type.
    fn type_param_token(node: &AIRNode) -> Option<&str> {
        if let NodeKind::TypeNamed { path, args } = &node.kind {
            if args.is_empty() && path.segments.len() == 1 {
                return path.segments.first().map(|s| s.name.as_str());
            }
        }
        None
    }

    /// Synthesise the explicit Go type-arguments (`[int64]`, `[int64, string]`)
    /// for a call to a generic free function whose source omits them, so a Go
    /// type-parameter that the runtime cannot infer is pinned without a
    /// turbofish at the Bock level.
    ///
    /// Go can only infer a type-parameter from a value argument's static type.
    /// A parameter declared `Optional[T]` / `Result[T, E]` lowers to the
    /// *monomorphic* runtime struct `__bockOption` / `__bockResult` (payload
    /// `interface{}`), so its `T`/`E` are invisible to Go inference and a call
    /// like `or_else(empty, Some(7))` fails `cannot infer T`. This recovers each
    /// type-parameter's concrete Go type from, in order:
    ///
    /// 1. an `Optional[T]` / `Result[T, E]` argument's recovered element type
    ///    ([`Self::arg_optional_elem`] / [`Self::arg_result_elems`]),
    /// 2. an ordinary (non-container, non-lambda) argument unified against its
    ///    declared param type ([`Self::bind_fn_type_params`] — pins e.g.
    ///    `get_or`'s `fallback: T`),
    /// 3. the call's *expected* result type ([`Self::current_expected_type`],
    ///    a typed `let x: Ty = …`) unified against the declared return type.
    ///
    /// Returns `Some([go-type, …])` in declaration order, with any
    /// type-parameter still unresolved after the three sources filled with `any`
    /// (such a parameter appears *only* behind the erased runtime, so `any` is
    /// its only consistent type — e.g. `and_then`'s mapped `U`). Returns `None`
    /// — emitting no turbofish, so ordinary Go inference runs — when the
    /// signature names no `Optional`/`Result` container at all (the `core.iter`
    /// generic-record combinators, which Go already infers and must not change).
    fn synthesize_go_type_args(
        &self,
        gp_names: &[String],
        param_tys: &[Option<AIRNode>],
        ret_ty: Option<&AIRNode>,
        args: &[bock_air::AirArg],
        force: bool,
    ) -> Option<Vec<String>> {
        if gp_names.is_empty() {
            return None;
        }
        // Only intervene for a fn whose signature involves the monomorphic
        // `Optional`/`Result` runtime — the case Go's own inference cannot
        // handle (`__bockOption`/`__bockResult` carry no `[T]`) — or one whose
        // sealed-core bound was lowered to a built-in constraint (`force`), under
        // which Go infers an untyped constant as the default type (`int`, not
        // `int64`). A purely record/collection-generic fn (`core.iter`'s
        // `ListIterator[T]` combinators) is left bare so Go infers it as before —
        // no regression.
        let touches_container = param_tys
            .iter()
            .flatten()
            .chain(ret_ty)
            .any(Self::type_mentions_container);
        // A type param that appears in *no* value parameter cannot be inferred by
        // Go from the arguments — it is pinned only by the return type (e.g.
        // `fn empty[T]() -> SortedSet[T]`, a zero-arg generic constructor, or any
        // return-only param). Such a call always needs an explicit turbofish,
        // synthesised below from the expected destination type. (A param that
        // *does* appear in a value parameter is left to Go's own inference unless
        // a container/sealed-bound reason forces the turbofish.)
        let param_go_types: Vec<String> = param_tys
            .iter()
            .flatten()
            .map(|p| self.type_to_go(p))
            .collect();
        let has_return_only_param = gp_names.iter().any(|g| {
            !param_go_types
                .iter()
                .any(|p| Self::contains_type_token(p, g))
        });
        if !touches_container && !force && !has_return_only_param {
            return None;
        }
        let mut bindings: HashMap<String, String> = HashMap::new();

        // (2) Ordinary argument unification (bare `T`, `List[T]`, etc.).
        for (k, v) in self.bind_fn_type_params(gp_names, param_tys, args) {
            bindings.entry(k).or_insert(v);
        }

        // (1) Container arguments: pair each declared `Optional[T]`/`Result[T,E]`
        // param's type-param tokens with the argument's recovered element types.
        for (i, arg) in args.iter().enumerate() {
            let Some(pty) = param_tys.get(i).and_then(|p| p.as_ref()) else {
                continue;
            };
            let Some((container, tokens)) = Self::container_type_param_names(pty) else {
                continue;
            };
            match container {
                "Optional" => {
                    if let (Some(token), Some(elem)) =
                        (tokens.first(), self.arg_optional_elem(&arg.value))
                    {
                        if gp_names.iter().any(|g| g == *token) {
                            bindings.entry((*token).to_string()).or_insert(elem);
                        }
                    }
                }
                "Result" => {
                    let (ok, err) = self.arg_result_elems(&arg.value);
                    if let (Some(token), Some(ty)) = (tokens.first(), ok) {
                        if gp_names.iter().any(|g| g == *token) {
                            bindings.entry((*token).to_string()).or_insert(ty);
                        }
                    }
                    if let (Some(token), Some(ty)) = (tokens.get(1), err) {
                        if gp_names.iter().any(|g| g == *token) {
                            bindings.entry((*token).to_string()).or_insert(ty);
                        }
                    }
                }
                _ => {}
            }
        }

        // (3) Expected result type unified against the declared return type. The
        // typed-`let` binding sets `current_expected_type` to the rendered Go
        // type of the destination, e.g. `[]int64` for `let xs: List[Int] =
        // to_list(...)`, which unifies against `List[T]` → `[]T` to pin `T`.
        if let (Some(ret), Some(expected)) = (ret_ty, self.current_expected_type.as_deref()) {
            if expected != "interface{}" {
                let pattern = self.type_to_go(ret);
                Self::unify_go_pattern(&pattern, expected, gp_names, &mut bindings);
            }
        }

        // Every type-parameter is filled: a pinned one with its concrete Go
        // type, an unresolved one with `any`. An unresolved param appears *only*
        // behind the erased `Optional`/`Result` runtime (a bare `T`, `List[T]`,
        // `Fn(T) -> …`, etc. would have been bound above from an argument or the
        // return), so `any` is the only type it can take and never conflicts —
        // e.g. `and_then`'s `U` (the mapped `Ok` type) is invisible to the call
        // and harmlessly erased, while its `E` is pinned from the `Result[T, E]`
        // argument so Go no longer fails `cannot infer E`.
        let out: Vec<String> = gp_names
            .iter()
            .map(|g| {
                bindings
                    .get(g)
                    .cloned()
                    .unwrap_or_else(|| "any".to_string())
            })
            .collect();
        Some(out)
    }

    /// True if a declared AIR type *mentions* the monomorphic `Optional` /
    /// `Result` runtime anywhere within it (directly, or nested inside a
    /// collection / function type). Gates [`Self::synthesize_go_type_args`] —
    /// only such a signature defeats Go's own type-parameter inference.
    fn type_mentions_container(node: &AIRNode) -> bool {
        match &node.kind {
            NodeKind::TypeOptional { .. } => true,
            NodeKind::TypeNamed { path, args } => {
                let is_container = path
                    .segments
                    .last()
                    .is_some_and(|s| matches!(s.name.as_str(), "Optional" | "Result"));
                is_container || args.iter().any(Self::type_mentions_container)
            }
            NodeKind::TypeFunction { params, ret, .. } => {
                params.iter().any(Self::type_mentions_container)
                    || Self::type_mentions_container(ret)
            }
            NodeKind::TypeTuple { elems } => elems.iter().any(Self::type_mentions_container),
            _ => false,
        }
    }

    /// Returns `true` if the AIR type node mentions any of the named generic
    /// params (a bare `T`, or `T` nested inside `List[T]` / `Optional[T]` /
    /// `(T, U)` / a function type). Used to skip recording a method's Go return
    /// type when it is still generic (the concrete caller has no such `T`).
    fn type_mentions_params(node: &AIRNode, params: &[String]) -> bool {
        match &node.kind {
            NodeKind::TypeNamed { path, args } => {
                let names_param = path
                    .segments
                    .last()
                    .is_some_and(|s| params.iter().any(|p| p == &s.name));
                names_param || args.iter().any(|a| Self::type_mentions_params(a, params))
            }
            NodeKind::TypeOptional { inner } => Self::type_mentions_params(inner, params),
            NodeKind::TypeFunction {
                params: ps, ret, ..
            } => {
                ps.iter().any(|p| Self::type_mentions_params(p, params))
                    || Self::type_mentions_params(ret, params)
            }
            NodeKind::TypeTuple { elems } => {
                elems.iter().any(|e| Self::type_mentions_params(e, params))
            }
            _ => false,
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

    /// Returns `true` if the AST `TypeExpr` represents `Void` or `Unit` (the
    /// `TypeExpr` analogue of [`Self::is_void_type`]). Used by `ast_type_to_go`
    /// to render a `Fn(...) -> Void` as a Go `func(...)` with no result type.
    fn ast_type_is_void(ty: &TypeExpr) -> bool {
        match ty {
            TypeExpr::Named { path, args, .. } if args.is_empty() => path
                .segments
                .last()
                .is_some_and(|s| s.name == "Void" || s.name == "Unit"),
            TypeExpr::Tuple { elems, .. } => elems.is_empty(),
            _ => false,
        }
    }

    /// Returns the emitted body and import flags without building the preamble.
    fn into_parts(self) -> (String, GoImportNeeds) {
        (
            self.buf,
            GoImportNeeds {
                fmt: self.needs_fmt_import,
                sync: self.needs_sync_import,
                time: self.needs_time_import,
                strings: self.needs_strings_import,
                utf8: self.needs_utf8_import,
                math: self.needs_math_import,
                unicode: self.needs_unicode_import,
                strconv: self.needs_strconv_import,
                reflect: self.needs_reflect_import,
            },
        )
    }

    fn finish(self) -> String {
        let mut header = format!("package {}\n", self.package_name);
        let needs = GoImportNeeds {
            fmt: self.needs_fmt_import,
            sync: self.needs_sync_import,
            time: self.needs_time_import,
            strings: self.needs_strings_import,
            utf8: self.needs_utf8_import,
            math: self.needs_math_import,
            unicode: self.needs_unicode_import,
            strconv: self.needs_strconv_import,
            reflect: self.needs_reflect_import,
        };
        header.push_str(&needs.render_block());
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

    /// The Go string for an `Optional`/`Result` constructor's payload, casting a
    /// *numeric literal* payload to its concrete Go type (`int64` / `float64`).
    ///
    /// The runtime boxes the payload as `interface{}`. A bare Go integer literal
    /// (`__bockSome(7)`) is an *untyped constant* whose default boxed dynamic
    /// type is Go `int`, not `int64` — so a later `.(int64)` payload assertion
    /// (or a generic `.(T)` with `T` instantiated as `int64`) panics
    /// `interface {} is int, not int64`. The read-side widening helpers
    /// (`__bockAsInt64`) mask this for *concrete* `int64` assertions, but a
    /// generic free fn's body asserts the bare type parameter `T`, which has no
    /// widening. Boxing the literal as `int64(7)` / `float64(..)` makes the
    /// dynamic type match the instantiation, so both assertion forms succeed.
    /// Non-literal and non-numeric payloads are passed through unchanged.
    fn box_payload_str(&self, arg: Option<&bock_air::AirArg>, arg_strs: &[String]) -> String {
        let rendered = arg_strs
            .first()
            .map_or_else(|| "nil".to_string(), |s| s.clone());
        let Some(arg) = arg else {
            return rendered;
        };
        match Self::numeric_literal_go_type(&arg.value) {
            Some(go_ty) => format!("{go_ty}({rendered})"),
            None => rendered,
        }
    }

    /// The Go numeric type (`int64`/`float64`) of an expression that is a numeric
    /// literal — directly, or under a unary negation (`-1`) — else `None`. Used
    /// to box an `Optional`/`Result` payload literal at its concrete dynamic type
    /// (see [`Self::box_payload_str`]).
    /// Decide whether a `**` (`BinOp::Pow`) should lower to the floating-point
    /// path (`math.Pow`) or the integer path (`__bockIntPow`). Returns `true` if
    /// either operand is statically float-typed (a `Float` literal, or a binding
    /// inferred to `float64`/`float32`). When neither operand resolves to a float
    /// — the common `2 ** 10` integer case, or an unresolved operand — the integer
    /// helper is chosen, which keeps exact integer precision. Both operands are
    /// coerced to the chosen numeric type at the call site, so a mixed
    /// `Int ** Float` still routes to `math.Pow` and type-checks.
    fn pow_is_float(&self, left: &AIRNode, right: &AIRNode) -> bool {
        let is_float = |this: &Self, n: &AIRNode| -> bool {
            matches!(
                this.infer_go_expr_type(n).as_deref(),
                Some("float64") | Some("float32")
            )
        };
        is_float(self, left) || is_float(self, right)
    }

    fn numeric_literal_go_type(node: &AIRNode) -> Option<&'static str> {
        match &node.kind {
            NodeKind::Literal { lit } => match lit {
                Literal::Int(_) => Some("int64"),
                Literal::Float(_) => Some("float64"),
                _ => None,
            },
            NodeKind::UnaryOp {
                op: UnaryOp::Neg,
                operand,
            } => Self::numeric_literal_go_type(operand),
            _ => None,
        }
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
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                // Route through an installed `Clock` handler if one is in scope;
                // otherwise fall through to the host primitive (default).
                if let Some(handler) = self.clock_handler_var() {
                    format!("{handler}.{}({a})", to_pascal_case("sleep"))
                } else {
                    // sleep(d) returns a chan struct{} so `await` (= `<-ch`)
                    // works uniformly. The goroutine holds for `d` nanos, then
                    // closes ch.
                    self.needs_time_import = true;
                    format!("(func() <-chan struct{{}} {{ __ch := make(chan struct{{}}); go func() {{ time.Sleep(time.Duration({a})); close(__ch) }}(); return __ch }})()")
                }
            }
            // Optional constructors → tagged runtime struct.
            "Some" => {
                let a = self.box_payload_str(args.first(), &arg_strs);
                format!("__bockSome({a})")
            }
            "None" => "__bockNone".to_string(),
            // Result constructors → tagged runtime struct (see
            // `RESULT_RUNTIME_GO`), mirroring `Some`/`None`.
            "Ok" => {
                let a = self.box_payload_str(args.first(), &arg_strs);
                format!("__bockOk({a})")
            }
            "Err" => {
                let a = self.box_payload_str(args.first(), &arg_strs);
                format!("__bockErr({a})")
            }
            _ => return Ok(None),
        };
        Ok(Some(code))
    }

    /// Emit a built-in `Optional`/`Result` method call to its Go form.
    ///
    /// Recognised via the checker's `recv_kind` annotation
    /// ([`crate::generator::desugared_optional_method`] /
    /// [`crate::generator::desugared_result_method`]). The tagged runtime structs
    /// (`__bockOption`/`__bockResult`) carry the payload as `interface{}` in `.v`
    /// and the tag in `.tag`, so a method lowers to a Go closure IIFE that tests
    /// `.tag` and recovers the payload. The payload Go type (for `unwrap`/
    /// `unwrap_or`) is resolved from the receiver's declared `Optional[T]` /
    /// `Result[T, E]` type; when unknown it stays `interface{}` (works for `%v`
    /// interpolation, the conservative fallback the Optional match also uses).
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
            let elem = self.scrutinee_optional_elem(recv);
            self.emit_tagged_container_method(
                recv,
                method,
                rest,
                "Some",
                "__bockSome",
                "__bockNone",
                elem.as_deref(),
            )?;
            return Ok(true);
        }
        if let Some((recv, method, rest)) =
            crate::generator::desugared_result_method(node, callee, args)
        {
            let elems = self.scrutinee_result_elems(recv);
            let ok = elems.as_ref().map(|(o, _)| o.as_str());
            self.emit_tagged_container_method(
                recv,
                method,
                rest,
                "Ok",
                "__bockOk",
                "__bockErr",
                ok,
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Lower a tagged-container method on `recv` to a Go closure IIFE.
    /// `present_tag` is the payload-carrying tag (`"Some"`/`"Ok"`);
    /// `present_ctor`/`other_ctor` are the runtime constructors; `payload_ty` is
    /// the Go type the payload is asserted to (`None` → bare `interface{}`).
    #[allow(clippy::too_many_arguments)]
    fn emit_tagged_container_method(
        &mut self,
        recv: &AIRNode,
        method: &str,
        rest: &[bock_air::AirArg],
        present_tag: &str,
        present_ctor: &str,
        other_ctor: &str,
        payload_ty: Option<&str>,
    ) -> Result<(), CodegenError> {
        // The closure binds the receiver once as `__c` (a tagged struct).
        // Tag tests: `is_some`/`is_ok` and `is_none`/`is_err`.
        match method {
            "is_some" | "is_ok" => {
                self.buf.push('(');
                self.emit_expr(recv)?;
                let _ = write!(self.buf, ".tag == \"{present_tag}\")");
                return Ok(());
            }
            "is_none" | "is_err" => {
                self.buf.push('(');
                self.emit_expr(recv)?;
                let _ = write!(self.buf, ".tag != \"{present_tag}\")");
                return Ok(());
            }
            _ => {}
        }
        // Recover the payload as its concrete type (numeric boxings widened via
        // the shared helpers; otherwise a type assertion; else bare `.v`).
        let payload_expr = |ty: Option<&str>| -> String {
            match ty {
                Some("int64") => "__bockAsInt64(__c.v)".to_string(),
                Some("float64") => "__bockAsFloat64(__c.v)".to_string(),
                Some(t) => format!("__c.v.({t})"),
                None => "__c.v".to_string(),
            }
        };
        match method {
            "unwrap" | "unwrap_or" => {
                let ret_ty = payload_ty.unwrap_or("interface{}");
                let payload = payload_expr(payload_ty);
                let _ = write!(
                    self.buf,
                    "func(__c {recv_ty}) {ret_ty} {{ if __c.tag == \"{present_tag}\" {{ return {payload} }}; return ",
                    recv_ty = self.container_runtime_ty(present_ctor),
                );
                if method == "unwrap_or" {
                    if let Some(d) = rest.first() {
                        self.emit_expr(&d.value)?;
                    } else {
                        // No default supplied — fall back to the zero value.
                        self.zero_value_for(ret_ty);
                    }
                } else {
                    // `unwrap` on the empty case panics (no default supplied).
                    self.zero_value_for(ret_ty);
                }
                self.buf.push_str(" }(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "map" => {
                // Apply the callback to the payload and rewrap as the present
                // variant; the empty/other variant passes through unchanged.
                let recv_ty = self.container_runtime_ty(present_ctor);
                let payload = payload_expr(payload_ty);
                let _ = write!(
                    self.buf,
                    "func(__c {recv_ty}) {recv_ty} {{ if __c.tag == \"{present_tag}\" {{ return {present_ctor}("
                );
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf
                        .push_str("func(x interface{}) interface{} { return x }");
                }
                let _ = write!(self.buf, "({payload})) }}; return __c }}(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "flat_map" => {
                let recv_ty = self.container_runtime_ty(present_ctor);
                let payload = payload_expr(payload_ty);
                let _ = write!(
                    self.buf,
                    "func(__c {recv_ty}) {recv_ty} {{ if __c.tag == \"{present_tag}\" {{ return "
                );
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf
                        .push_str("func(x interface{}) interface{} { return x }");
                }
                let _ = write!(self.buf, "({payload}).({recv_ty}) }}; return __c }}(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            "map_err" => {
                let recv_ty = self.container_runtime_ty(present_ctor);
                let _ = write!(
                    self.buf,
                    "func(__c {recv_ty}) {recv_ty} {{ if __c.tag != \"{present_tag}\" {{ return {other_ctor}("
                );
                if let Some(f) = rest.first() {
                    self.emit_expr(&f.value)?;
                } else {
                    self.buf
                        .push_str("func(x interface{}) interface{} { return x }");
                }
                self.buf.push_str("(__c.v)) }; return __c }(");
                self.emit_expr(recv)?;
                self.buf.push(')');
            }
            _ => {
                self.buf.push_str("nil");
            }
        }
        Ok(())
    }

    /// The Go runtime struct type for a container, keyed by its present-variant
    /// constructor (`__bockSome` → `__bockOption`, `__bockOk` → `__bockResult`).
    fn container_runtime_ty(&self, present_ctor: &str) -> &'static str {
        if present_ctor == "__bockOk" {
            "__bockResult"
        } else {
            "__bockOption"
        }
    }

    /// Emit a Go zero value for `ty` (used as the `unwrap`-on-empty fallback).
    fn zero_value_for(&mut self, ty: &str) {
        let zero = match ty {
            "int64" | "float64" | "int" | "float32" | "int32" => "0",
            "string" => "\"\"",
            "bool" => "false",
            _ => "nil",
        };
        self.buf.push_str(zero);
    }

    /// Emit a read-only `List` built-in method call to its Go form.
    ///
    /// Lists are `[]interface{}`. `len`/`length`/`count` wrap in `int64(...)`;
    /// `is_empty` compares the length. `Optional`-returning methods
    /// (`get`/`first`/`last`/`index_of`) build the tagged Optional runtime
    /// (`__bockSome(v)` / `__bockNone`) inside an immediately-called closure so
    /// the receiver is evaluated once and bounds are checked. `contains` /
    /// `index_of` / `concat` / `join` use inline closures (no top-level helper
    /// injection needed). The `__bockSome` payload is `interface{}`; a `match`
    /// arm binding it re-asserts the element type via the existing Optional
    /// resolver (`scrutinee_optional_elem`), which now resolves
    /// `get`/`first`/`last` on a typed `List[T]` receiver.
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
        let recv_str = self.expr_to_string(recv)?;
        // The receiver's Go slice element type. Lists are now concretely typed
        // (`[]int64`, etc.), so the closure parameter type (`__r []<elem>`) must
        // match the receiver — a `[]int64` argument does NOT convert to a
        // `[]interface{}` parameter in Go. When the element type can't be
        // recovered the receiver is still `[]interface{}` (the literal/inference
        // fallback), so `interface{}` is the correct, matching default.
        let elem = self
            .list_receiver_elem_go_type(recv)
            .unwrap_or_else(|| "interface{}".to_string());
        let slice = format!("[]{elem}");
        let code = match method {
            "len" | "length" | "count" => format!("int64(len({recv_str}))"),
            "is_empty" => format!("(len({recv_str}) == 0)"),
            "get" => {
                let Some(idx) = rest.first() else {
                    return Ok(false);
                };
                let i = self.expr_to_string(&idx.value)?;
                format!(
                    "func(__r {slice}, __i int64) __bockOption {{ \
                     if __i >= 0 && __i < int64(len(__r)) {{ return __bockSome(__r[__i]) }}; \
                     return __bockNone }}({recv_str}, {i})"
                )
            }
            "first" => format!(
                "func(__r {slice}) __bockOption {{ \
                 if len(__r) > 0 {{ return __bockSome(__r[0]) }}; \
                 return __bockNone }}({recv_str})"
            ),
            "last" => format!(
                "func(__r {slice}) __bockOption {{ \
                 if len(__r) > 0 {{ return __bockSome(__r[len(__r)-1]) }}; \
                 return __bockNone }}({recv_str})"
            ),
            "contains" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.needs_fmt_import = true;
                let x = self.expr_to_string(&x.value)?;
                // Compare on the `%v` string form, not raw `interface{}` `==`:
                // a list literal boxes Go `int`/`float64` while a typed `Int`
                // variable is `int64`, so `int(30) == int64(30)` is *false*
                // under Go's type-and-value interface equality. The checker
                // guarantees `contains(x: T)` on `List[T]` (same T), so the two
                // operands always denote the same Bock type — `%v` normalises
                // only the int/int64 boxing difference. `__x` stays
                // `interface{}` (a typed argument boxes into it).
                format!(
                    "func(__r {slice}, __x interface{{}}) bool {{ \
                     __xs := fmt.Sprintf(\"%v\", __x); \
                     for _, __e := range __r {{ if fmt.Sprintf(\"%v\", __e) == __xs {{ return true }} }}; \
                     return false }}({recv_str}, {x})"
                )
            }
            "index_of" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                self.needs_fmt_import = true;
                let x = self.expr_to_string(&x.value)?;
                // See `contains` for why this compares `%v` forms, not `==`.
                format!(
                    "func(__r {slice}, __x interface{{}}) __bockOption {{ \
                     __xs := fmt.Sprintf(\"%v\", __x); \
                     for __i, __e := range __r {{ if fmt.Sprintf(\"%v\", __e) == __xs {{ return __bockSome(int64(__i)) }} }}; \
                     return __bockNone }}({recv_str}, {x})"
                )
            }
            "concat" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                // The `__o` IIFE parameter is `[]elem` (the receiver's element
                // type), so the argument list literal must also be `[]elem{...}`
                // — a `[]interface{}{x}` argument is not assignable to a `[]T`
                // parameter in Go. Thread the receiver's element type into the
                // literal as its expected collection element (extends #144's
                // return-position typed-literal fix to argument position).
                let prev_expected = self.expected_collection_elem.take();
                if matches!(
                    o.value.kind,
                    NodeKind::ListLiteral { .. }
                        | NodeKind::MapLiteral { .. }
                        | NodeKind::SetLiteral { .. }
                ) {
                    self.expected_collection_elem = Some((elem.clone(), None));
                }
                let o = self.expr_to_string(&o.value)?;
                self.expected_collection_elem = prev_expected;
                format!(
                    "func(__r {slice}, __o {slice}) {slice} {{ \
                     __v := make({slice}, 0, len(__r)+len(__o)); \
                     __v = append(__v, __r...); __v = append(__v, __o...); \
                     return __v }}({recv_str}, {o})"
                )
            }
            "join" => {
                let Some(sep) = rest.first() else {
                    return Ok(false);
                };
                self.needs_fmt_import = true;
                let sep = self.expr_to_string(&sep.value)?;
                format!(
                    "func(__r {slice}, __sep string) string {{ \
                     __s := \"\"; \
                     for __i, __e := range __r {{ if __i > 0 {{ __s += __sep }}; \
                     __s += fmt.Sprintf(\"%v\", __e) }}; \
                     return __s }}({recv_str}, {sep})"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Emit an in-place `List` mutator (`push`/`append`, DQ18) in **statement
    /// position** to its Go form.
    ///
    /// Recognised via [`crate::generator::desugared_list_mutating_method`]. Go
    /// grows a slice by *reassignment* — `recv = append(recv, x)` — so unlike the
    /// other backends (which emit a value-less `recv.push(x)`) this is an
    /// assignment statement, emitted only from `emit_stmt`. The checker types
    /// `push`/`append` as `Void`, so the call always appears in statement
    /// position, and the ownership pass guarantees the receiver is a `mut` lvalue
    /// (a `let mut` slice, a `mut` parameter, or a field reachable through a
    /// `mut` receiver), so the same place expression is a valid assignment
    /// target on the left of `=`. A field receiver lowers to its Go-cased place
    /// (`r.Items = append(r.Items, x)`) via `expr_to_string`.
    ///
    /// Returns `false` (no statement emitted) when the call is not a recognised
    /// in-place `List` mutator, so the caller falls back to the generic
    /// expression-statement path.
    fn try_emit_list_mutating_stmt(
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
        let recv_str = self.expr_to_string(recv)?;
        let x = self.expr_to_string(&x.value)?;
        self.write_indent();
        let _ = write!(self.buf, "{recv_str} = append({recv_str}, {x})");
        self.buf.push('\n');
        Ok(true)
    }

    /// Render a closure (lambda) argument as a typed Go func literal, with its
    /// parameter types pinned to `param_types`, and report the func literal's
    /// inferred return type.
    ///
    /// Used by [`Self::try_emit_list_functional_method`]: a bare `(x) => x * 2`
    /// argument would otherwise emit `func(x interface{}) interface{}`, whose
    /// return does not assign into a concrete `[]int64`. Pinning the params to the
    /// receiver's element type (`[]int64` → `int64`) lets the Go backend infer a
    /// concrete return type, which the caller uses both to size the result slice
    /// and to decide whether a type assertion is needed.
    fn render_typed_closure_go(
        &mut self,
        closure: &AIRNode,
        param_types: &[String],
    ) -> Result<(String, Option<String>), CodegenError> {
        self.render_typed_closure_go_ret(closure, param_types, None)
    }

    /// As [`Self::render_typed_closure_go`], but a `forced_ret` of `Some(ty)` pins
    /// the closure's Go return type (a predicate combinator's `bool`) rather than
    /// inferring it from the body. The returned `Option<String>` ret reflects the
    /// forced type when given, so the caller's result-elem logic agrees.
    fn render_typed_closure_go_ret(
        &mut self,
        closure: &AIRNode,
        param_types: &[String],
        forced_ret: Option<&str>,
    ) -> Result<(String, Option<String>), CodegenError> {
        let ret = if let Some(t) = forced_ret {
            Some(t.to_string())
        } else if let NodeKind::Lambda { params, body } = &closure.kind {
            let saved = self.enter_param_go_types_with_expected(params, Some(param_types));
            // Use the block-tail inference (not the bare-expr one): a `.map`
            // closure whose body is a block / `if` / `match` (e.g.
            // `(t) => { if (t.id == id) { t.complete() } else { t } }`) needs its
            // value tail typed so the result slice is `[]Todo`, not `[]interface{}`
            // (`infer_go_expr_type` alone returns `None` for a block/if body).
            let r = self.infer_block_tail_type(body);
            self.var_go_type = saved;
            r
        } else {
            None
        };
        let prev = self.expected_lambda_param_types.take();
        self.expected_lambda_param_types = Some(param_types.to_vec());
        let prev_forced = self.forced_lambda_ret.take();
        self.forced_lambda_ret = forced_ret.map(str::to_string);
        let code = self.expr_to_string(closure)?;
        self.forced_lambda_ret = prev_forced;
        self.expected_lambda_param_types = prev;
        Ok((code, ret))
    }

    /// The Go element type to use for a `List` combinator's *result* slice, drawn
    /// from the binding's expected type (`current_expected_type`, e.g. `[]string`
    /// → `string`) when it is a slice, else from the closure's inferred return
    /// `cb_ret`, else `interface{}`.
    fn list_result_elem_go(&self, cb_ret: Option<&str>) -> String {
        if let Some(t) = self.current_expected_type.as_deref() {
            if let Some(elem) = t.strip_prefix("[]") {
                return elem.to_string();
            }
        }
        cb_ret.unwrap_or("interface{}").to_string()
    }

    /// Emit a functional (closure-taking) `List` built-in method call to its Go
    /// form.
    ///
    /// Recognised via [`crate::generator::desugared_list_functional_method`]. Go
    /// has neither methods on slices nor a usable `map` selector (`map` is a
    /// keyword), so each combinator lowers to an immediately-invoked func literal
    /// that drives a `for _, __x := range __r` loop, applying the closure
    /// (rendered with its parameter types pinned to the receiver's element type,
    /// via [`Self::render_typed_closure_go`]). The closure is evaluated *once* —
    /// the desugared `recv.map(recv, cb)` shape the generic fall-through emits
    /// otherwise produces `expected selector …, found 'map'` (a parse error,
    /// `map` being reserved) or `.filter undefined`. `map`/`flat_map` size their
    /// result from the binding's expected element type; `fold`/`reduce` thread an
    /// accumulator; `find` returns the tagged Optional runtime; `any`/`all`
    /// short-circuit to `bool`; `for_each` returns nothing.
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
        let recv_str = self.expr_to_string(recv)?;
        // Recover the receiver's element type. `list_receiver_elem_go_type`
        // handles direct bindings/literals and the element-preserving chained
        // combinators (`filter`/`find`). A chained `map`/`flat_map` receiver's
        // element is the *closure's return type*, which needs the `&mut self`
        // block-tail inference in `value_list_elem_go_type` — so fall back to it
        // when the cheap `&self` resolver yields nothing. This is the
        // `.filter(..).map(..).map(..)` (Q-go-chained-combinator-typing) case:
        // the outermost `.map`'s `interface{}` element flips to the concrete
        // element threaded through the whole chain.
        let elem = self
            .list_receiver_elem_go_type(recv)
            .or_else(|| self.value_list_elem_go_type(recv))
            .unwrap_or_else(|| "interface{}".to_string());
        let slice = format!("[]{elem}");
        let code = match method {
            "map" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let (f, cb_ret) =
                    self.render_typed_closure_go(&cb.value, std::slice::from_ref(&elem))?;
                let out = self.list_result_elem_go(cb_ret.as_deref());
                // Assert the closure result into the declared element type unless
                // the closure's inferred return type *already* matches it exactly
                // (asserting a concrete value onto its own type is a Go compile
                // error; appending an `interface{}` to a concrete `[]T` without
                // an assertion is also an error). When `out` is `interface{}` no
                // assertion is needed (anything assigns to `interface{}`).
                let push = if out != "interface{}" && cb_ret.as_deref() != Some(out.as_str()) {
                    format!("__out = append(__out, __f(__x).({out}))")
                } else {
                    "__out = append(__out, __f(__x))".to_string()
                };
                format!(
                    "func(__r {slice}) []{out} {{ \
                     __f := {f}; \
                     __out := make([]{out}, 0, len(__r)); \
                     for _, __x := range __r {{ {push} }}; \
                     return __out }}({recv_str})"
                )
            }
            "flat_map" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let (f, cb_ret) =
                    self.render_typed_closure_go(&cb.value, std::slice::from_ref(&elem))?;
                // The closure returns a slice; its element is the result element.
                let out = self.current_expected_type.as_deref().map_or_else(
                    || {
                        cb_ret
                            .as_deref()
                            .and_then(|r| r.strip_prefix("[]"))
                            .unwrap_or("interface{}")
                            .to_string()
                    },
                    |t| t.strip_prefix("[]").unwrap_or("interface{}").to_string(),
                );
                format!(
                    "func(__r {slice}) []{out} {{ \
                     __f := {f}; \
                     __out := make([]{out}, 0, len(__r)); \
                     for _, __x := range __r {{ __out = append(__out, __f(__x)...) }}; \
                     return __out }}({recv_str})"
                )
            }
            "filter" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                // The predicate returns `Bool`; pin it so `if __f(__x)` is a Go
                // boolean condition (the body may be a method call / `match` whose
                // Go return type is otherwise erased to `interface{}`).
                let (f, _) = self.render_typed_closure_go_ret(
                    &cb.value,
                    std::slice::from_ref(&elem),
                    Some("bool"),
                )?;
                format!(
                    "func(__r {slice}) {slice} {{ \
                     __f := {f}; \
                     __out := make({slice}, 0, len(__r)); \
                     for _, __x := range __r {{ if __f(__x) {{ __out = append(__out, __x) }} }}; \
                     return __out }}({recv_str})"
                )
            }
            "find" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let (f, _) = self.render_typed_closure_go_ret(
                    &cb.value,
                    std::slice::from_ref(&elem),
                    Some("bool"),
                )?;
                format!(
                    "func(__r {slice}) __bockOption {{ \
                     __f := {f}; \
                     for _, __x := range __r {{ if __f(__x) {{ return __bockSome(__x) }} }}; \
                     return __bockNone }}({recv_str})"
                )
            }
            "any" | "all" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let (f, _) = self.render_typed_closure_go_ret(
                    &cb.value,
                    std::slice::from_ref(&elem),
                    Some("bool"),
                )?;
                // `any`: true if some element matches; `all`: true unless one fails.
                if method == "any" {
                    format!(
                        "func(__r {slice}) bool {{ \
                         __f := {f}; \
                         for _, __x := range __r {{ if __f(__x) {{ return true }} }}; \
                         return false }}({recv_str})"
                    )
                } else {
                    format!(
                        "func(__r {slice}) bool {{ \
                         __f := {f}; \
                         for _, __x := range __r {{ if !__f(__x) {{ return false }} }}; \
                         return true }}({recv_str})"
                    )
                }
            }
            "reduce" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                // No seed: first element is the accumulator. Accumulator type is
                // the element type.
                let (f, _) =
                    self.render_typed_closure_go(&cb.value, &[elem.clone(), elem.clone()])?;
                format!(
                    "func(__r {slice}) {elem} {{ \
                     __f := {f}; \
                     __acc := __r[0]; \
                     for __i := 1; __i < len(__r); __i++ {{ __acc = __f(__acc, __r[__i]) }}; \
                     return __acc }}({recv_str})"
                )
            }
            "fold" => {
                let (Some(init), Some(cb)) = (rest.first(), rest.get(1)) else {
                    return Ok(false);
                };
                let acc_ty = self
                    .infer_go_expr_type(&init.value)
                    .or_else(|| self.current_expected_type.clone())
                    .unwrap_or_else(|| "interface{}".to_string());
                let init_str = self.expr_to_string(&init.value)?;
                let (f, _) =
                    self.render_typed_closure_go(&cb.value, &[acc_ty.clone(), elem.clone()])?;
                format!(
                    "func(__r {slice}, __acc {acc_ty}) {acc_ty} {{ \
                     __f := {f}; \
                     for _, __x := range __r {{ __acc = __f(__acc, __x) }}; \
                     return __acc }}({recv_str}, {init_str})"
                )
            }
            "for_each" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let (f, _) =
                    self.render_typed_closure_go(&cb.value, std::slice::from_ref(&elem))?;
                format!(
                    "func(__r {slice}) {{ \
                     __f := {f}; \
                     for _, __x := range __r {{ __f(__x) }} }}({recv_str})"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Emit a built-in `Map[K, V]` method call to its Go form (native
    /// `map[K]V`, building on P3-α's typed map literals/decls).
    ///
    /// Recognised via [`crate::generator::desugared_map_method`] (gated on
    /// `recv_kind = "Map"`) and wired *before* [`Self::try_emit_list_method`],
    /// so a `Map` receiver's `get`/`contains_key`/`len` no longer route through
    /// the `List` path (which passed the `map[K]V` where a `[]interface{}` slice
    /// closure expected, and cast the key to `int64`). `get` uses the Go
    /// comma-ok form (`__v, __ok := __m[__k]`) → the `__bockSome`/`__bockNone`
    /// Optional runtime. Mutating methods (`set`/`delete`/`merge`) copy then
    /// mutate and return the new map (Bock map value semantics). The inline
    /// closures are typed `map[K]V` from the receiver's declared element types
    /// (recovered from [`Self::map_receiver_kv_go_types`]; `interface{}`
    /// fallback when unknown). Returns `true` if handled.
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
        let recv_str = self.expr_to_string(recv)?;
        let (k_ty, v_ty) = self
            .map_receiver_kv_go_types(recv)
            .unwrap_or_else(|| ("interface{}".to_string(), "interface{}".to_string()));
        let map_ty = format!("map[{k_ty}]{v_ty}");
        let code = match method {
            "len" | "length" | "count" => format!("int64(len({recv_str}))"),
            "is_empty" => format!("(len({recv_str}) == 0)"),
            "contains_key" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                let k = self.expr_to_string(&k.value)?;
                format!(
                    "func(__m {map_ty}, __k {k_ty}) bool {{ _, __ok := __m[__k]; return __ok }}\
                     ({recv_str}, {k})"
                )
            }
            "get" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                let k = self.expr_to_string(&k.value)?;
                format!(
                    "func(__m {map_ty}, __k {k_ty}) __bockOption {{ \
                     if __v, __ok := __m[__k]; __ok {{ return __bockSome(__v) }}; \
                     return __bockNone }}({recv_str}, {k})"
                )
            }
            "set" => {
                let (Some(k), Some(v)) = (rest.first(), rest.get(1)) else {
                    return Ok(false);
                };
                let k = self.expr_to_string(&k.value)?;
                let v = self.expr_to_string(&v.value)?;
                format!(
                    "func(__m {map_ty}, __k {k_ty}, __v {v_ty}) {map_ty} {{ \
                     __r := make({map_ty}, len(__m)+1); \
                     for __mk, __mv := range __m {{ __r[__mk] = __mv }}; \
                     __r[__k] = __v; return __r }}({recv_str}, {k}, {v})"
                )
            }
            "delete" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                let k = self.expr_to_string(&k.value)?;
                format!(
                    "func(__m {map_ty}, __k {k_ty}) {map_ty} {{ \
                     __r := make({map_ty}, len(__m)); \
                     for __mk, __mv := range __m {{ __r[__mk] = __mv }}; \
                     delete(__r, __k); return __r }}({recv_str}, {k})"
                )
            }
            "merge" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!(
                    "func(__m {map_ty}, __o {map_ty}) {map_ty} {{ \
                     __r := make({map_ty}, len(__m)+len(__o)); \
                     for __mk, __mv := range __m {{ __r[__mk] = __mv }}; \
                     for __ok, __ov := range __o {{ __r[__ok] = __ov }}; \
                     return __r }}({recv_str}, {o})"
                )
            }
            "filter" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&f.value)?;
                format!(
                    "func(__m {map_ty}, __f func({k_ty}, {v_ty}) bool) {map_ty} {{ \
                     __r := make({map_ty}); \
                     for __mk, __mv := range __m {{ if __f(__mk, __mv) {{ __r[__mk] = __mv }} }}; \
                     return __r }}({recv_str}, {f})"
                )
            }
            "keys" => format!(
                "func(__m {map_ty}) []{k_ty} {{ \
                 __r := make([]{k_ty}, 0, len(__m)); \
                 for __mk := range __m {{ __r = append(__r, __mk) }}; \
                 return __r }}({recv_str})"
            ),
            "values" => format!(
                "func(__m {map_ty}) []{v_ty} {{ \
                 __r := make([]{v_ty}, 0, len(__m)); \
                 for _, __mv := range __m {{ __r = append(__r, __mv) }}; \
                 return __r }}({recv_str})"
            ),
            "entries" | "to_list" => format!(
                "func(__m {map_ty}) [][2]interface{{}} {{ \
                 __r := make([][2]interface{{}}, 0, len(__m)); \
                 for __mk, __mv := range __m {{ __r = append(__r, [2]interface{{}}{{__mk, __mv}}) }}; \
                 return __r }}({recv_str})"
            ),
            "for_each" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&f.value)?;
                format!(
                    "func(__m {map_ty}, __f func({k_ty}, {v_ty})) {{ \
                     for __mk, __mv := range __m {{ __f(__mk, __mv) }} }}({recv_str}, {f})"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Emit a built-in `Set[E]` method call to its Go form (native
    /// `map[E]struct{}`, building on P3-α's typed set literals/decls).
    ///
    /// Recognised via [`crate::generator::desugared_set_method`] (gated on
    /// `recv_kind = "Set"`) and wired *before* [`Self::try_emit_list_method`].
    /// `contains` is a comma-ok membership test; the set algebra builds new
    /// `map[E]struct{}` values. Mutating methods (`add`/`remove`) copy then
    /// mutate and return the new set. The inline closures are typed `map[E]
    /// struct{}` from the receiver's declared element type
    /// ([`Self::set_receiver_elem_go_type`]; `interface{}` fallback). Returns
    /// `true` if handled.
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
        let recv_str = self.expr_to_string(recv)?;
        let e_ty = self
            .set_receiver_elem_go_type(recv)
            .unwrap_or_else(|| "interface{}".to_string());
        let set_ty = format!("map[{e_ty}]struct{{}}");
        let code = match method {
            "len" | "length" | "count" => format!("int64(len({recv_str}))"),
            "is_empty" => format!("(len({recv_str}) == 0)"),
            "contains" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                let x = self.expr_to_string(&x.value)?;
                format!(
                    "func(__s {set_ty}, __x {e_ty}) bool {{ _, __ok := __s[__x]; return __ok }}\
                     ({recv_str}, {x})"
                )
            }
            "add" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                let x = self.expr_to_string(&x.value)?;
                format!(
                    "func(__s {set_ty}, __x {e_ty}) {set_ty} {{ \
                     __r := make({set_ty}, len(__s)+1); \
                     for __sk := range __s {{ __r[__sk] = struct{{}}{{}} }}; \
                     __r[__x] = struct{{}}{{}}; return __r }}({recv_str}, {x})"
                )
            }
            "remove" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                let x = self.expr_to_string(&x.value)?;
                format!(
                    "func(__s {set_ty}, __x {e_ty}) {set_ty} {{ \
                     __r := make({set_ty}, len(__s)); \
                     for __sk := range __s {{ __r[__sk] = struct{{}}{{}} }}; \
                     delete(__r, __x); return __r }}({recv_str}, {x})"
                )
            }
            "union" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!(
                    "func(__a {set_ty}, __b {set_ty}) {set_ty} {{ \
                     __r := make({set_ty}, len(__a)+len(__b)); \
                     for __k := range __a {{ __r[__k] = struct{{}}{{}} }}; \
                     for __k := range __b {{ __r[__k] = struct{{}}{{}} }}; \
                     return __r }}({recv_str}, {o})"
                )
            }
            "intersection" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!(
                    "func(__a {set_ty}, __b {set_ty}) {set_ty} {{ \
                     __r := make({set_ty}); \
                     for __k := range __a {{ if _, __ok := __b[__k]; __ok {{ \
                     __r[__k] = struct{{}}{{}} }} }}; \
                     return __r }}({recv_str}, {o})"
                )
            }
            "difference" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!(
                    "func(__a {set_ty}, __b {set_ty}) {set_ty} {{ \
                     __r := make({set_ty}); \
                     for __k := range __a {{ if _, __ok := __b[__k]; !__ok {{ \
                     __r[__k] = struct{{}}{{}} }} }}; \
                     return __r }}({recv_str}, {o})"
                )
            }
            "is_subset" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!(
                    "func(__a {set_ty}, __b {set_ty}) bool {{ \
                     for __k := range __a {{ if _, __ok := __b[__k]; !__ok {{ return false }} }}; \
                     return true }}({recv_str}, {o})"
                )
            }
            "is_superset" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!(
                    "func(__a {set_ty}, __b {set_ty}) bool {{ \
                     for __k := range __b {{ if _, __ok := __a[__k]; !__ok {{ return false }} }}; \
                     return true }}({recv_str}, {o})"
                )
            }
            "filter" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&f.value)?;
                format!(
                    "func(__s {set_ty}, __f func({e_ty}) bool) {set_ty} {{ \
                     __r := make({set_ty}); \
                     for __k := range __s {{ if __f(__k) {{ __r[__k] = struct{{}}{{}} }} }}; \
                     return __r }}({recv_str}, {f})"
                )
            }
            "map" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&f.value)?;
                format!(
                    "func(__s {set_ty}, __f func({e_ty}) {e_ty}) {set_ty} {{ \
                     __r := make({set_ty}); \
                     for __k := range __s {{ __r[__f(__k)] = struct{{}}{{}} }}; \
                     return __r }}({recv_str}, {f})"
                )
            }
            "to_list" => format!(
                "func(__s {set_ty}) []{e_ty} {{ \
                 __r := make([]{e_ty}, 0, len(__s)); \
                 for __k := range __s {{ __r = append(__r, __k) }}; \
                 return __r }}({recv_str})"
            ),
            "for_each" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&f.value)?;
                format!(
                    "func(__s {set_ty}, __f func({e_ty})) {{ \
                     for __k := range __s {{ __f(__k) }} }}({recv_str}, {f})"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
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
                // Route through an installed `Clock` handler's `now_monotonic`
                // op if one is in scope; otherwise emit the host primitive.
                if let Some(handler) = self.clock_handler_var() {
                    format!("{handler}.{}()", to_pascal_case("now_monotonic"))
                } else {
                    self.needs_time_import = true;
                    "time.Now()".to_string()
                }
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Lower a primitive trait-bridge method call (`compare`/`eq`/`to_string`/
    /// `display` on a primitive receiver) to its Go form.
    ///
    /// `(1).compare(2)` resolves to `Ordering`; this routes it through the
    /// generic Ordering-runtime helper `__bockCompare`, returning a
    /// `__bockOrdering` constant the value-switch / construction sides use. `eq`
    /// → `==`; `to_string`/`display` → `fmt.Sprintf("%v", x)`.
    /// Lower a desugared `String` built-in method call (`recv_kind =
    /// "Primitive:String"`) to its native Go string op. Wired into the `Call`
    /// arm *before* `try_emit_list_method` so a String receiver's
    /// `len`/`contains`/`is_empty` dispatch here, not through the List path —
    /// which is the misrouting that broke `String.contains` (the `[]interface{}`
    /// `fmt.Sprintf("%v", …)` linear scan failed to compile against a `string`).
    ///
    /// `len` is the Unicode SCALAR count (`utf8.RuneCountInString(s)`) per spec
    /// §18.3 — Go's `len(s)` is the BYTE length, so `byte_len` maps to it.
    /// `replace` replaces ALL occurrences (`strings.ReplaceAll`). `split` returns
    /// `[]string`, which the read-only `List` built-ins (`len`/…) accept.
    ///
    /// Gated on `recv_kind = "Primitive:String"` directly (not the cross-backend
    /// [`crate::generator::desugared_string_method`] subset) so Go can lower the
    /// wider resolved String surface — `slice`/`substring`/`char_at`/`index_of`/
    /// `repeat`/`reverse`/`trim_start`/`trim_end` — to native ops, matching the
    /// Rust backend. Indexing/`reverse` are **rune-aware** (`[]rune(s)`) so the
    /// scalar-index semantics of §18.3 hold for multibyte input — a plain `s[i]`
    /// would be a byte. `char_at`/`index_of` build the tagged `Optional` runtime
    /// (`__bockSome(v)` / `__bockNone`); `index_of` converts `strings.Index`'s
    /// byte offset to a rune index.
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
            "len" | "length" | "count" => {
                self.needs_utf8_import = true;
                format!("int64(utf8.RuneCountInString({recv_str}))")
            }
            "byte_len" => format!("int64(len({recv_str}))"),
            "is_empty" => format!("(len({recv_str}) == 0)"),
            "to_upper" => {
                self.needs_strings_import = true;
                format!("strings.ToUpper({recv_str})")
            }
            "to_lower" => {
                self.needs_strings_import = true;
                format!("strings.ToLower({recv_str})")
            }
            "trim" => {
                self.needs_strings_import = true;
                format!("strings.TrimSpace({recv_str})")
            }
            "trim_start" => {
                self.needs_strings_import = true;
                self.needs_unicode_import = true;
                format!("strings.TrimLeftFunc({recv_str}, unicode.IsSpace)")
            }
            "trim_end" => {
                self.needs_strings_import = true;
                self.needs_unicode_import = true;
                format!("strings.TrimRightFunc({recv_str}, unicode.IsSpace)")
            }
            // `reverse` reverses by Unicode scalar (rune), not byte.
            "reverse" => format!(
                "func(__r []rune) string {{ for __i, __j := 0, len(__r)-1; __i < __j; __i, __j = __i+1, __j-1 {{ __r[__i], __r[__j] = __r[__j], __r[__i] }}; return string(__r) }}([]rune({recv_str}))"
            ),
            "to_string" | "display" => format!("({recv_str})"),
            "repeat" => {
                let Some(n) = arg0(self)? else {
                    return Ok(false);
                };
                self.needs_strings_import = true;
                format!("strings.Repeat({recv_str}, int({n}))")
            }
            "contains" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                self.needs_strings_import = true;
                format!("strings.Contains({recv_str}, {p})")
            }
            "starts_with" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                self.needs_strings_import = true;
                format!("strings.HasPrefix({recv_str}, {p})")
            }
            "ends_with" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                self.needs_strings_import = true;
                format!("strings.HasSuffix({recv_str}, {p})")
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
                self.needs_strings_import = true;
                format!("strings.ReplaceAll({recv_str}, {from}, {to})")
            }
            "split" => {
                let Some(sep) = arg0(self)? else {
                    return Ok(false);
                };
                self.needs_strings_import = true;
                format!("strings.Split({recv_str}, {sep})")
            }
            // `slice`/`substring(start, end)`: scalar-index half-open substring
            // (spec §18.3). Indexed over `[]rune` so the indices are scalar
            // positions; `start`/`end` are clamped into range so out-of-bounds
            // does not panic.
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
                format!(
                    "func(__r []rune, __a, __b int) string {{ if __a < 0 {{ __a = 0 }}; if __b > len(__r) {{ __b = len(__r) }}; if __a > __b {{ __a = __b }}; return string(__r[__a:__b]) }}([]rune({recv_str}), int({start}), int({end}))"
                )
            }
            // `char_at(i)` returns `Optional[Char]` — `None` when out of range. A
            // Bock `Char` is a Go `rune`.
            "char_at" => {
                let Some(i) = arg0(self)? else {
                    return Ok(false);
                };
                format!(
                    "func(__r []rune, __i int) __bockOption {{ if __i >= 0 && __i < len(__r) {{ return __bockSome(__r[__i]) }}; return __bockNone }}([]rune({recv_str}), int({i}))"
                )
            }
            // `index_of(needle)` returns `Optional[Int]` — the scalar index of the
            // first match, or `None`. `strings.Index` yields a *byte* offset, so
            // convert it to a rune index via the prefix rune count.
            "index_of" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                self.needs_strings_import = true;
                self.needs_utf8_import = true;
                format!(
                    "func(__s, __p string) __bockOption {{ __b := strings.Index(__s, __p); if __b < 0 {{ return __bockNone }}; return __bockSome(int64(utf8.RuneCountInString(__s[:__b]))) }}({recv_str}, {p})"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Q-prim-assoc: lower a primitive associated-conversion call
    /// (`Float.from(x)` / `Int.try_from(s)` / `String.from(c)`) to Go's native
    /// conversion. `from` is a Go type conversion (`float64(x)` / `int64(x)` /
    /// `string(x)`; a Bock `Char` is a `rune`, so `string(rune)` yields the
    /// single-character string). `try_from` parses via `strconv.Parse{Int,Float}`
    /// inside an IIFE returning the `__bockResult` runtime struct, the `Err`
    /// payload built with the in-package `ConvertErrorFn` constructor (Go emits
    /// everything into `package main`, so it is always visible). Returns `true`
    /// when handled.
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
            ("Float", "from") => format!("float64({arg_str})"),
            ("Int", "from") => format!("int64({arg_str})"),
            ("String", "from") => format!("string({arg_str})"),
            ("Int", "try_from") => {
                self.needs_strconv_import = true;
                self.needs_strings_import = true;
                format!(
                    "func(__s string) __bockResult {{ __v, __err := strconv.ParseInt(strings.TrimSpace(__s), 10, 64); \
                     if __err != nil {{ return __bockErr(ConvertErrorFn(\"cannot parse \\\"\" + __s + \"\\\" as Int\")) }}; \
                     return __bockOk(__v) }}({arg_str})"
                )
            }
            ("Float", "try_from") => {
                self.needs_strconv_import = true;
                self.needs_strings_import = true;
                format!(
                    "func(__s string) __bockResult {{ __v, __err := strconv.ParseFloat(strings.TrimSpace(__s), 64); \
                     if __err != nil {{ return __bockErr(ConvertErrorFn(\"cannot parse \\\"\" + __s + \"\\\" as Float\")) }}; \
                     return __bockOk(__v) }}({arg_str})"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Lower a desugared numeric/`Char`/`Bool` primitive method (`recv_kind =
    /// "Primitive:Int" | "Primitive:Float" | "Primitive:Char" | "Primitive:Bool"`)
    /// to its native Go form. Covers the conversion and math methods the checker
    /// resolves on the scalar primitives — `to_float`/`to_int`/`abs`/`min`/`max`/
    /// `clamp`/`floor`/`ceil`/`round`/`sqrt`/… . Wired into the `Call` arm
    /// alongside [`Self::try_emit_string_method`], before the generic
    /// desugared-self-call fall-through (which would emit `n.toFloat(n)`).
    /// `math.*` operates on `float64`, so the `Float` math methods round-trip the
    /// `float64` receiver through `math`. `compare`/`eq`/`to_string`/`display`/
    /// `hash_code` stay on the primitive *bridge* path. A Bock `Int` is a Go
    /// `int64`, `Float` a `float64`, `Char` a `rune`.
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
            // Conversions. `Float.to_int` truncates toward zero via `math.Trunc`,
            // which also yields a *runtime* (non-constant) float64 — Go rejects a
            // direct `int64(3.9)` on a literal receiver (an untyped/typed float
            // *constant* not exactly representable as an int).
            ("Int", "to_float") => format!("float64({recv_str})"),
            ("Float", "to_int") => {
                self.needs_math_import = true;
                format!("int64(math.Trunc({recv_str}))")
            }
            ("Char", "to_int") => format!("int64({recv_str})"),
            ("Bool", "to_int") => {
                format!("func(__b bool) int64 {{ if __b {{ return 1 }}; return 0 }}({recv_str})")
            }
            // Int math. Go 1.21+ has builtin `min`/`max`; `abs` is via a cast to
            // float64 and back to keep it inline.
            ("Int", "abs") => {
                self.needs_math_import = true;
                format!("int64(math.Abs(float64({recv_str})))")
            }
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
            // Float math (`math.*` works on `float64`).
            ("Float", "abs") => {
                self.needs_math_import = true;
                format!("math.Abs({recv_str})")
            }
            ("Float", "floor") => {
                self.needs_math_import = true;
                format!("math.Floor({recv_str})")
            }
            ("Float", "ceil") => {
                self.needs_math_import = true;
                format!("math.Ceil({recv_str})")
            }
            ("Float", "round") => {
                self.needs_math_import = true;
                format!("math.Round({recv_str})")
            }
            ("Float", "sqrt") => {
                self.needs_math_import = true;
                format!("math.Sqrt({recv_str})")
            }
            ("Float", "is_nan") => {
                self.needs_math_import = true;
                format!("math.IsNaN({recv_str})")
            }
            ("Float", "is_infinite") => {
                self.needs_math_import = true;
                format!("math.IsInf({recv_str}, 0)")
            }
            // Bool.
            ("Bool", "negate") => format!("(!({recv_str}))"),
            // Char (a Go `rune`).
            ("Char", "to_upper") => {
                self.needs_unicode_import = true;
                format!("unicode.ToUpper({recv_str})")
            }
            ("Char", "to_lower") => {
                self.needs_unicode_import = true;
                format!("unicode.ToLower({recv_str})")
            }
            ("Char", "is_alpha") => {
                self.needs_unicode_import = true;
                format!("unicode.IsLetter({recv_str})")
            }
            ("Char", "is_digit") => {
                self.needs_unicode_import = true;
                format!("unicode.IsDigit({recv_str})")
            }
            ("Char", "is_whitespace") => {
                self.needs_unicode_import = true;
                format!("unicode.IsSpace({recv_str})")
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
        let Some((recv, method, rest, prim)) =
            crate::generator::primitive_bridge_call(node, callee, args)
        else {
            return Ok(false);
        };
        // A concrete primitive receiver uses the typed `__bockCompare` helper.
        self.emit_bridge_method(recv, method, rest, false, Some(prim))
    }

    /// Lower a sealed-core-trait bridge method on a *bounded generic type
    /// variable* (`a.eq(b)` / `a.compare(b)` inside `eq_check[T: Equatable]`) to
    /// its Go form (GAP-C). The generic analogue of
    /// [`Self::try_emit_primitive_bridge`]: the `[T Equatable]` bound is rewritten
    /// to Go's built-in constraint (`comparable` / `__bockOrdered`) at the
    /// signature, so `==` and the inline ordering comparison type-check. `compare`
    /// uses an inline comparison (not the typed `__bockCompare` helper, whose
    /// named constraint a `T __bockOrdered` does not satisfy). Fires only when the
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
        // A bounded *generic* type var has no concrete primitive kind; the
        // Char-display special-case below does not apply.
        self.emit_bridge_method(recv, method, rest, true, None)
    }

    /// Shared body of the primitive / trait-bound bridges. `generic` selects the
    /// generic-bound lowering for `compare`: an inline `if a < b … ` expression
    /// producing an `__bockOrdering` (the typed `__bockCompare` helper's named
    /// constraint is not satisfied by a `T __bockOrdered`-bounded type var). `eq`
    /// (`==`) and `to_string`/`display` (`fmt.Sprintf`) are identical either way.
    fn emit_bridge_method(
        &mut self,
        recv: &AIRNode,
        method: &str,
        rest: &[bock_air::AirArg],
        generic: bool,
        recv_prim: Option<&str>,
    ) -> Result<bool, CodegenError> {
        let recv_str = self.expr_to_string(recv)?;
        let code = match method {
            "compare" => {
                let Some(other) = rest.first() else {
                    return Ok(false);
                };
                let other = self.expr_to_string(&other.value)?;
                if generic {
                    // The Ordering runtime (gated on `"compare"` appearing in the
                    // module AST) is already emitted at module top, so `Less`/
                    // `Equal`/`Greater`/`__bockOrdering` are in scope here.
                    format!(
                        "func() __bockOrdering {{ if ({recv_str}) < ({other}) {{ return Less }}; \
                         if ({recv_str}) == ({other}) {{ return Equal }}; return Greater }}()"
                    )
                } else {
                    format!("__bockCompare({recv_str}, {other})")
                }
            }
            "eq" => {
                let Some(other) = rest.first() else {
                    return Ok(false);
                };
                let other = self.expr_to_string(&other.value)?;
                format!("(({recv_str}) == ({other}))")
            }
            "to_string" | "display" => {
                // A `Char` lowers to Go `rune` (an alias of `int32`); `fmt.Sprintf
                // ("%v", r)` would print its integer code point ('A' → "65"), not
                // the character. `string(rune)` renders the scalar as its UTF-8
                // text. Every other primitive keeps the `%v` formatting (an int
                // prints as its digits, a float/bool/string as itself).
                if recv_prim == Some("Char") {
                    format!("string({recv_str})")
                } else {
                    self.needs_fmt_import = true;
                    format!("fmt.Sprintf(\"%v\", {recv_str})")
                }
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Recognise desugared method calls on Duration/Instant values.
    ///
    /// `node` is the full `Call` AIR node, consulted only to *exclude* primitive
    /// receivers: [`is_time_method_name`] alone is ambiguous (`abs` is both
    /// `Duration.abs` and `Int.abs`/`Float.abs`), so when the checker has stamped
    /// `recv_kind = "Primitive:<Ty>"` on the call this is a numeric method, not a
    /// time method — bail so [`Self::try_emit_numeric_method`] handles it.
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
                // `instant.elapsed()` is derived: time-since-`recv`. Route the
                // "now" read through an installed `Clock` handler if in scope —
                // `NowMonotonic()` yields a `time.Time`, so the span is
                // `now.Sub(recv)` as nanoseconds; otherwise read the host
                // monotonic clock via `time.Since(recv)` (default).
                if let Some(handler) = self.clock_handler_var() {
                    format!(
                        "int64({handler}.{}().Sub({recv_str}))",
                        to_pascal_case("now_monotonic")
                    )
                } else {
                    self.needs_time_import = true;
                    format!("int64(time.Since({recv_str}))")
                }
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
            NodeKind::Module { items, .. } => {
                if self.per_module {
                    // Per-module native-package path (S3): each module is its own
                    // `package main` file and the runtime preludes live once in
                    // the shared `bock_runtime.go` (same package → visible). So
                    // this file inlines NO prelude; it just emits its own items.
                    // `ImportDecl`s are a no-op (same package — no inter-file
                    // import). The per-file `import (...)` block (fmt/sync/…) is
                    // rendered by `into_parts` from the `needs_*` flags the body
                    // sets as it emits.
                    //
                    // `@test` functions are transpiled separately into `go test`
                    // files (project mode, §20.6.2 — see `generate_tests`), never
                    // into the runtime package: their `expect(...)` assertion DSL
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
                    return Ok(());
                }
                // Single-module self-contained emit (`generate_module`, used by
                // unit tests): the module's runtime preludes are inlined into
                // this one file's buffer and `ImportDecl`s are dropped. Each
                // prelude is emitted at most once, gated on a ctx flag (a
                // duplicate `type __bockChannel`/`__bockOption` would not
                // compile). The per-module *project* path never takes this branch
                // (it sets `per_module` and emits preludes once into the shared
                // `bock_runtime.go`).
                if !self.concurrency_runtime_emitted && go_module_uses_concurrency(items) {
                    self.buf.push_str(CONCURRENCY_RUNTIME_GO);
                    self.buf.push('\n');
                    self.concurrency_runtime_emitted = true;
                }
                let uses_optional = go_module_uses_optional(items);
                let uses_result = go_module_uses_result(items);
                if !self.optional_runtime_emitted && uses_optional {
                    self.buf.push_str(OPTIONAL_RUNTIME_GO);
                    self.buf.push('\n');
                    self.optional_runtime_emitted = true;
                }
                if !self.result_runtime_emitted && uses_result {
                    self.buf.push_str(RESULT_RUNTIME_GO);
                    self.buf.push('\n');
                    self.result_runtime_emitted = true;
                }
                // Shared numeric-payload helpers: emit once if either container
                // runtime is present (both use them; emitting from each would
                // redeclare them).
                if !self.numeric_runtime_emitted && (uses_optional || uses_result) {
                    self.buf.push_str(NUMERIC_RUNTIME_GO);
                    self.buf.push('\n');
                    self.numeric_runtime_emitted = true;
                }
                // The bespoke `__bockOrdering` value runtime is emitted only when
                // the real `core.compare.Ordering` enum is NOT reachable — when
                // it is, that user enum is authoritative (its variants are the
                // sealed-interface structs `OrderingLess{}`, and `compare`
                // returns it), so the int runtime would be dead and its `Less`
                // constants would shadow nothing the program uses.
                let emit_ordering = !self.ordering_runtime_emitted
                    && go_module_uses_ordering(items)
                    && !self.ordering_enum_reachable();
                if emit_ordering {
                    self.buf.push_str(ORDERING_RUNTIME_GO);
                    self.buf.push('\n');
                    self.ordering_runtime_emitted = true;
                }
                // The `__bockOrdered` constraint: needed by a `[T: Comparable]`
                // sealed-bound generic fn, or whenever the Ordering runtime above
                // is emitted (the constraint was split out of that block). Deduped
                // with its own flag so it is defined at most once.
                if !self.ordered_constraint_emitted
                    && (emit_ordering || !self.fn_sealed_bound.is_empty())
                {
                    self.buf.push_str(ORDERED_CONSTRAINT_GO);
                    self.buf.push('\n');
                    self.ordered_constraint_emitted = true;
                }
                if !self.range_runtime_emitted && go_module_uses_range(items) {
                    self.buf.push_str(RANGE_RUNTIME_GO);
                    self.buf.push('\n');
                    self.range_runtime_emitted = true;
                }
                if !self.int_pow_runtime_emitted && go_module_uses_int_pow(items) {
                    self.buf.push_str(INT_POW_RUNTIME_GO);
                    self.buf.push('\n');
                    self.int_pow_runtime_emitted = true;
                }
                if !self.deep_eq_runtime_emitted && go_module_uses_deep_eq(items) {
                    self.buf.push_str(DEEP_EQ_RUNTIME_GO);
                    self.buf.push('\n');
                    self.deep_eq_runtime_emitted = true;
                    self.needs_reflect_import = true;
                }
                // `@test` functions are transpiled separately into `go test` files
                // (project mode, §20.6.2 — see `generate_tests`), never into the
                // runtime package.
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
                Ok(())
            }
            NodeKind::ImportDecl { .. } => {
                // Single-module self-contained emit: a Bock `use` is a no-op (no
                // sibling file to import from). The per-module project path keeps
                // one `package main` across files, so a same-package symbol is
                // also visible without a Go `import` — the per-item visit here is
                // a no-op in both paths.
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
            } => {
                // Fold any `where`-clause trait bounds onto the generic params so
                // the `[T Constraint]` type-param constraint is emitted for a
                // `where`-bounded fn — local or imported (the imported fn is
                // emitted in its own module file with its reconstructed
                // `where`-clause, PR #286).
                let merged = crate::generator::merge_where_bounds_into_generics(
                    generic_params,
                    where_clause,
                );
                self.emit_fn_decl(
                    *visibility,
                    *is_async,
                    &name.name,
                    &merged,
                    params,
                    return_type.as_deref(),
                    effect_clause,
                    body,
                )
            }
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
                    self.emit_method(&name.name, generic_params, method, false)?;
                }
                Ok(())
            }
            NodeKind::TraitDecl {
                name,
                methods,
                generic_params,
                ..
            } => {
                // Traits → Go interfaces. A trait whose methods take a
                // `Self`-typed operand is encoded as an F-bounded *generic*
                // interface — `type Comparable[__Self any] interface {
                // Compare(__Self) Ordering }` — so an impl `func (Key)
                // Compare(Key)` satisfies `Comparable[Key]` and a bound `[T:
                // Comparable]` lowers to `[T Comparable[T]]`. `Self` in the
                // method signatures then renders as the interface's type param.
                //
                // A trait that declares its own generic params
                // (`trait Iterable[T] { fn iter(self) -> ListIterator[T] }`)
                // must carry them on the interface header too, or the param
                // appears `undefined` in a method signature (`Iter()
                // ListIterator[T]` with no `[T any]` → Go `undefined: T`). The
                // declared params and the synthesized `__Self` (if any) are both
                // threaded into the header.
                let uses_self = self.self_param_traits.contains(&name.name);
                let prev_self_subst = self.go_self_subst.take();
                let mut header_params: Vec<String> = Vec::new();
                if uses_self {
                    self.go_self_subst = Some("__Self".to_string());
                    header_params.push("__Self any".to_string());
                }
                for p in generic_params {
                    header_params.push(format!("{} any", p.name.name));
                }
                let head = if header_params.is_empty() {
                    name.name.clone()
                } else {
                    format!("{}[{}]", name.name, header_params.join(", "))
                };
                self.writeln(&format!("type {head} interface {{"));
                self.indent += 1;
                for method in methods {
                    if let NodeKind::FnDecl {
                        name,
                        params,
                        return_type,
                        ..
                    } = &method.kind
                    {
                        // Drop the leading `self` receiver — a Go interface
                        // method's receiver is implicit, so only the remaining
                        // operands form the signature (the AIR keeps `self` as a
                        // real leading param, as for impl methods).
                        let rest = match params.first().map(crate::generator::param_binds_self) {
                            Some(Some(_)) => &params[1..],
                            _ => &params[..],
                        };
                        let param_strs = self.collect_param_type_strs(rest);
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
                            self.go_method_name(&name.name, true),
                            param_strs.join(", "),
                        ));
                    }
                }
                self.indent -= 1;
                self.writeln("}");
                self.go_self_subst = prev_self_subst;
                Ok(())
            }
            NodeKind::ImplBlock {
                generic_params,
                target,
                methods,
                trait_path,
                ..
            } => {
                let target_name = self.type_expr_to_string(target);
                // The receiver's type-param list. Go requires the parameters on a
                // generic type's method receiver: `func (self *Box[T]) ...`. The
                // params come from the impl's own list when present, else from the
                // record/enum decl (the common `impl Box { ... }` where `T` is
                // declared on `record Box[T]`, not the impl).
                let target_generics = self.impl_target_generics(generic_params, &target_name);
                // Value receivers for trait/effect impls so `Handler{}` satisfies
                // the interface; pointer receivers for inherent `impl T { ... }`.
                let use_value_receiver = trait_path.is_some();
                // Trait default methods (codegen-completeness P2): synthesize a
                // receiver method on the target for every default method this
                // impl does not override, so the type satisfies the interface
                // (which declares the default's signature) and a call resolves.
                // A default body calling another trait method (`self.other(..)`)
                // resolves through the same receiver methods.
                let default_methods: Vec<AIRNode> = trait_path
                    .as_ref()
                    .map(|tp| {
                        crate::generator::inherited_default_methods(&self.trait_decls, tp, methods)
                    })
                    .unwrap_or_default();
                let mut emitted_any = false;
                for method in methods.iter().chain(default_methods.iter()) {
                    // Skip a trait-impl method that duplicates an inherent
                    // (`impl Type`) / class method of the same Go name. The
                    // inherent method is the real implementation and — being in
                    // `public_methods` because the trait declares it — is now
                    // exported to the same PascalCase Go name, so it satisfies the
                    // interface directly. Emitting the trait-impl method too would
                    // be a duplicate-method Go error, and a forwarder body
                    // (`fn render(self) { self.render() }`) would resolve back to
                    // itself (`Render() { return self.Render() }`) — infinite
                    // recursion. Default methods (synthesized, real bodies) and
                    // non-duplicated trait methods are unaffected.
                    if trait_path.is_some() {
                        if let NodeKind::FnDecl { name, .. } = &method.kind {
                            let go_name = to_pascal_case(&name.name);
                            if self
                                .inherent_methods
                                .contains(&(target_name.clone(), go_name))
                            {
                                continue;
                            }
                        }
                    }
                    if emitted_any {
                        self.buf.push('\n');
                    }
                    self.emit_method(&target_name, &target_generics, method, use_value_receiver)?;
                    emitted_any = true;
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
                ty,
                ..
            } => {
                // Render the alias to its underlying Go type so a value of the
                // alias type is the concrete container/struct (`type ParseResult =
                // Result[...]` → `type ParseResult = __bockResult`, whose `.tag`
                // a `match` reads). A *generic* alias keeps the prior `interface{}`
                // erasure (the emitter does not substitute its params, and the
                // alias registry skips it), so its param list is preserved.
                if generic_params.is_empty() {
                    let underlying = self.type_to_go(ty);
                    self.writeln(&format!("type {} = {underlying}", name.name));
                } else {
                    let type_params = self.format_generic_params(generic_params);
                    self.writeln(&format!("type {}{type_params} = interface{{}}", name.name));
                }
                Ok(())
            }
            NodeKind::ConstDecl {
                name, value, ty, ..
            } => {
                let type_str = format!(" {}", self.type_to_go(ty));
                let ind = self.indent_str();
                // Emit the const's declared name verbatim (not pascal-cased) so it
                // matches the verbatim spelling the `Identifier` use-site arm emits
                // for a known const — `to_pascal_case` strips underscores
                // (`FIZZ_NUM` → `FIZZNUM`) while the use site keeps `FIZZ_NUM`, an
                // undefined-identifier error. `SCREAMING_SNAKE` is a valid exported
                // Go identifier.
                let _ = write!(self.buf, "{ind}var {}{type_str} = ", name.name);
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
            // A bare `expr?` statement (`save_task(task)?`): the success value is
            // discarded, but the failure path must still early-return the
            // propagated error/None from the enclosing function. Emit the unwrap
            // prelude only — the success payload is unused, and asserting it (e.g.
            // a `Result[Void, _]` whose `Ok(())` boxes `nil`) would panic.
            NodeKind::Propagate { expr: inner } => {
                let _ = self.emit_try_unwrap(inner)?;
                Ok(())
            }
            _ => {
                // DQ18: an in-place `List` mutator (`push`/`append`) in
                // statement position lowers to Go's slice-growth idiom
                // `recv = append(recv, x)` — an assignment statement, not the
                // value-less call the other backends emit. Intercept it here,
                // before the generic expression-statement fall-through (Go has no
                // expression form for in-place append).
                if let NodeKind::Call { callee, args, .. } = &node.kind {
                    if self.try_emit_list_mutating_stmt(node, callee, args)? {
                        return Ok(());
                    }
                }
                self.write_indent();
                self.emit_expr(node)?;
                self.buf.push('\n');
                Ok(())
            }
        }
    }

    // ── Generics ────────────────────────────────────────────────────────────

    /// Resolve the generic params that apply to an `impl` target. Prefers the
    /// impl's own params (`impl[T] Box[T] { ... }`); falls back to the generic
    /// params declared on the target record/enum (`impl Box { ... }` where `T`
    /// is declared on `record Box[T]`). Returns an empty slice for a
    /// non-generic target.
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

    /// Render a *use-site* generic argument list (`[T]`) from generic params —
    /// the bare names only, never the `any`/bound clause. Used on a method
    /// receiver type (`*Box[T]`) where the params are already in scope.
    fn format_generic_param_args(&self, params: &[bock_ast::GenericParam]) -> String {
        if params.is_empty() {
            return String::new();
        }
        let names: Vec<&str> = params.iter().map(|p| p.name.name.as_str()).collect();
        format!("[{}]", names.join(", "))
    }

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
                            let bound_name = b
                                .segments
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join(".");
                            // A compiler-provided sealed-core bound (`Equatable`/…)
                            // with no user `impl` maps to Go's built-in constraint
                            // (GAP-C): `comparable` for equality/hashing, the
                            // self-contained `__bockOrdered` set for ordering, `any`
                            // for stringable. There is no `Equatable` type in Go, so
                            // the verbatim bound would be `undefined`.
                            if crate::generator::is_unimplemented_sealed_core_trait(
                                &bound_name,
                                &self.trait_decls,
                            ) {
                                match bound_name.as_str() {
                                    "Equatable" | "Hashable" => "comparable".to_string(),
                                    "Comparable" => "__bockOrdered".to_string(),
                                    _ => "any".to_string(),
                                }
                            } else if self.self_param_traits.contains(&bound_name) {
                                // An F-bounded self-param trait constraint is applied
                                // to the type var itself: `[T Comparable[T]]`.
                                format!("{bound_name}[{}]", p.name.name)
                            } else {
                                bound_name
                            }
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
        // The program entry point is always Go's `func main()`, never the
        // exported `func Main()` that PascalCasing a `public fn main` would
        // produce (Go would then report "function main is undeclared").
        let is_entry_point = name == "main";
        if is_public && !is_entry_point {
            self.public_fns.insert(name.to_string());
        }
        // `main` stays Go's bare `func main`; every other function goes through
        // `go_fn_name`, which applies the public/private casing rule and renames
        // a public name colliding with a top-level type (`key` → `KeyFn` when a
        // `record Key` exists).
        let fn_name = if is_entry_point {
            to_camel_case(name)
        } else {
            self.go_fn_name(name)
        };
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
        let saved_record_args = self.var_record_type_args.clone();
        let saved_lambda_ret = self.var_lambda_ret.clone();
        let saved_decl_type = self.var_decl_type_node.clone();
        let (
            saved_opt_scope,
            saved_list_scope,
            saved_result_scope,
            saved_map_scope,
            saved_set_scope,
        ) = self.enter_param_optional_scope(params);
        // Record each typed param's Go type (`b: Box[T]` → `var_go_type["b"] =
        // "Box[T]"`) so a `value.field.get(i)` list receiver inside the body can
        // recover the field's `[]T` element type (GAP-A). `current_self_record`
        // already covers `self.field`; this covers a non-self generic-record
        // param. Restored alongside the other param scopes on exit.
        let saved_go_types = self.enter_param_go_types_with_expected(params, None);
        // Seed the function body's Go block frame with the parameter names so a
        // `let` that shadows a parameter (the same Go scope — the body gets no
        // extra brace) lowers to a reassignment, not a re-declaration.
        self.pending_scope_seed = Some(self.param_binding_names(params));
        if name == "main" || is_void {
            self.emit_block_body(body)?;
        } else {
            let prev_ret = self.current_fn_ret_type.take();
            let prev_ret_coll = self.current_fn_ret_collection_elem.take();
            let prev_ret_node = self.current_fn_ret_type_node.take();
            self.current_fn_ret_type = return_type.map(|t| self.type_to_go(t));
            self.current_fn_ret_collection_elem =
                return_type.and_then(|t| self.collection_elem_go_types(t));
            self.current_fn_ret_type_node = Self::fn_type_ret_node(return_type);
            self.emit_block_body_return(body)?;
            self.current_fn_ret_type = prev_ret;
            self.current_fn_ret_collection_elem = prev_ret_coll;
            self.current_fn_ret_type_node = prev_ret_node;
        }
        self.var_optional_elem = saved_opt_scope;
        self.var_list_elem = saved_list_scope;
        self.var_result_elem = saved_result_scope;
        self.var_map_kv = saved_map_scope;
        self.var_set_elem = saved_set_scope;
        self.var_go_type = saved_go_types;
        self.var_record_type_args = saved_record_args;
        self.var_lambda_ret = saved_lambda_ret;
        self.var_decl_type_node = saved_decl_type;
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
                    Some(self.pattern_to_binding_name(pattern))
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
        target_generics: &[bock_ast::GenericParam],
        method: &AIRNode,
        use_value_receiver: bool,
    ) -> Result<(), CodegenError> {
        // Inside ANY impl method (trait or plain inherent), a `Self` type
        // resolves to the concrete target (the receiver type) — whether it
        // appears in a synthesized trait default (`other: Self`) or an inherent
        // method's own signature (`fn combine(self, ...) -> Self`). Previously
        // this was gated on `use_value_receiver` (trait impls only), so an
        // inherent-impl `Self` lowered to the `/* Self */` placeholder and
        // produced an invalid Go signature. `receiver_type` is in scope for both
        // method kinds. Saved/restored so the ctx-wide default of `/* Self */`
        // is unchanged outside impl methods.
        let prev_self_subst = self.go_self_subst.take();
        self.go_self_subst = Some(receiver_type.to_string());
        // The base record name (`ListIter` from `ListIter[T]` / `ListIter`) so a
        // `self.field` list receiver inside the body resolves its element type
        // via `record_field_list_elem`. Restored on exit.
        let prev_self_record = self.current_self_record.take();
        let base = receiver_type
            .split_once('[')
            .map_or(receiver_type, |(b, _)| b)
            .to_string();
        self.current_self_record = Some(base);
        let result =
            self.emit_method_body(receiver_type, target_generics, method, use_value_receiver);
        self.current_self_record = prev_self_record;
        self.go_self_subst = prev_self_subst;
        result
    }

    fn emit_method_body(
        &mut self,
        receiver_type: &str,
        target_generics: &[bock_ast::GenericParam],
        method: &AIRNode,
        use_value_receiver: bool,
    ) -> Result<(), CodegenError> {
        if let NodeKind::FnDecl {
            visibility,
            name,
            generic_params,
            params,
            return_type,
            effect_clause,
            body,
            ..
        } = &method.kind
        {
            // A trait-impl method (`use_value_receiver`) is PascalCased
            // regardless of Bock visibility: Go interface methods are always
            // exported (the `TraitDecl` emission PascalCases them), so the
            // receiver method and the call site must match. A `private` trait
            // default method (e.g. `not_equals`) would otherwise be camelCased
            // here while the interface declares it PascalCased. An inherent
            // method whose name a trait *also* declares (so it is in
            // `public_methods`) is exported too: it is the real implementation
            // that satisfies the interface (`impl Button { fn render }` →
            // `Render`), and every call site already PascalCases a
            // `public_methods` name — declaration and dispatch must agree.
            // Otherwise inherent methods keep the public/private casing rule.
            let is_public_method = use_value_receiver
                || matches!(visibility, Visibility::Public)
                || self.public_methods.contains(&name.name);
            let method_name = self.go_method_name(&name.name, is_public_method);
            // An associated function (no `self` receiver, e.g. a `From` impl's
            // `from`) has no Go-static equivalent: emit a free function named
            // `<Type>_<Method>` (reusing the DQ28 free-function naming) with no
            // receiver. The call site (`is_associated_call`) rewrites
            // `Type.method(args)` to the same `Type_Method(args)`.
            let receiver_base = receiver_type
                .split_once('[')
                .map_or(receiver_type, |(b, _)| b);
            if crate::generator::is_associated_impl_method(method, &self.effect_ops) {
                return self.emit_associated_fn(
                    receiver_base,
                    target_generics,
                    method,
                    is_public_method,
                );
            }
            // The AIR keeps `self` as a leading `Param` and method bodies refer
            // to `self.Field`. Name the Go receiver `self` and drop the leading
            // `self` param so the body resolves with no rewrite — otherwise the
            // receiver was `p` while the body referenced an undefined `self`,
            // and `self` also leaked in as a stray `interface{}` parameter.
            let (receiver_var, rest) = match params.first().map(crate::generator::param_binds_self)
            {
                Some(Some(_)) => ("self".to_string(), &params[1..]),
                _ => (
                    receiver_type
                        .chars()
                        .next()
                        .unwrap_or('r')
                        .to_lowercase()
                        .to_string(),
                    &params[..],
                ),
            };
            let param_strs = self.collect_param_strs(rest);
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
            // DQ28: a method declaring its own type parameters (`Box[T].map[U]`)
            // is free-function-lowered — Go forbids method type params. Emit
            // `func Box_Map[T any, U any](self Box[T], ..) ..` (the receiver
            // becomes a leading `self` *parameter*; the receiver's and the
            // method's type params combine on the free function, which Go allows).
            // Every call site is rewritten to `Box_Map(box, ..)` by the call
            // emitter. A non-generic method keeps the idiomatic Go receiver form.
            let receiver_base = receiver_type
                .split_once('[')
                .map_or(receiver_type, |(b, _)| b);
            let freefn_lowered = !generic_params.is_empty()
                && self.freefn_lowered_type(&name.name) == Some(receiver_base);
            if freefn_lowered {
                // Combine receiver type params (`[T any]`) with the method's own
                // (`[U any]`) into one Go free-function type-param list.
                let mut combined = target_generics.to_vec();
                combined.extend(generic_params.iter().cloned());
                let type_params = self.format_generic_params(&combined);
                let receiver_args = self.format_generic_param_args(target_generics);
                let self_param = format!("{receiver_var} {receiver_type}{receiver_args}");
                let mut freefn_params = vec![self_param];
                freefn_params.extend(all_params);
                let fn_name = self.freefn_lowered_name(receiver_base, &name.name, is_public_method);
                self.writeln(&format!(
                    "func {fn_name}{type_params}({}){ret} {{",
                    freefn_params.join(", "),
                ));
            } else {
                let receiver_prefix = if use_value_receiver { "" } else { "*" };
                // Go binds a generic type's params on the receiver itself:
                // `func (self *Box[T]) ...`. The bare-name arg list (`[T]`) brings
                // `T` into scope for the receiver type, params, and body.
                let receiver_generics = self.format_generic_param_args(target_generics);
                self.writeln(&format!(
                    "func ({receiver_var} {receiver_prefix}{receiver_type}{receiver_generics}) \
                     {method_name}({}){ret} {{",
                    all_params.join(", "),
                ));
            }
            self.indent += 1;
            let old_handler_vars = self.current_handler_vars.clone();
            let expanded = self.expand_effect_names(effect_clause);
            for ename in &expanded {
                self.current_handler_vars
                    .insert(ename.clone(), to_camel_case(ename));
            }
            let saved_record_args = self.var_record_type_args.clone();
            let saved_decl_type = self.var_decl_type_node.clone();
            let (
                saved_opt_scope,
                saved_list_scope,
                saved_result_scope,
                saved_map_scope,
                saved_set_scope,
            ) = self.enter_param_optional_scope(rest);
            // Seed the method body's Go block frame with the receiver var and the
            // value parameter names (same Go scope as the body) so a shadowing
            // `let` reassigns rather than re-declares.
            let mut method_seed = self.param_binding_names(rest);
            method_seed.push(receiver_var.clone());
            self.pending_scope_seed = Some(method_seed);
            if return_type.is_some() && !is_void {
                let prev_ret = self.current_fn_ret_type.take();
                let prev_ret_coll = self.current_fn_ret_collection_elem.take();
                let prev_ret_node = self.current_fn_ret_type_node.take();
                self.current_fn_ret_type = return_type.as_deref().map(|t| self.type_to_go(t));
                self.current_fn_ret_collection_elem = return_type
                    .as_deref()
                    .and_then(|t| self.collection_elem_go_types(t));
                self.current_fn_ret_type_node = Self::fn_type_ret_node(return_type.as_deref());
                self.emit_block_body_return(body)?;
                self.current_fn_ret_type = prev_ret;
                self.current_fn_ret_collection_elem = prev_ret_coll;
                self.current_fn_ret_type_node = prev_ret_node;
            } else {
                self.emit_block_body(body)?;
            }
            self.var_optional_elem = saved_opt_scope;
            self.var_list_elem = saved_list_scope;
            self.var_result_elem = saved_result_scope;
            self.var_map_kv = saved_map_scope;
            self.var_set_elem = saved_set_scope;
            self.var_record_type_args = saved_record_args;
            self.var_decl_type_node = saved_decl_type;
            self.current_handler_vars = old_handler_vars;
            self.indent -= 1;
            self.writeln("}");
        }
        Ok(())
    }

    /// Emit an impl/trait **associated function** (no `self` receiver) as a Go
    /// free function `func <Type>_<Method>(params) ret { ... }`.
    ///
    /// Go has no static methods, so an associated function — e.g. a `From` impl's
    /// `from(value) -> Self` — cannot attach to the type. It is emitted as a free
    /// function whose name carries the type prefix (`Foot_From`), matching the
    /// `Type.method(args)` → `Type_Method(args)` rewrite at the call site
    /// (`is_associated_call`). The `<Type>_` prefix keeps the name collision-free
    /// across types sharing a method name.
    fn emit_associated_fn(
        &mut self,
        receiver_base: &str,
        target_generics: &[bock_ast::GenericParam],
        method: &AIRNode,
        is_public_method: bool,
    ) -> Result<(), CodegenError> {
        let NodeKind::FnDecl {
            name,
            generic_params,
            params,
            return_type,
            effect_clause,
            body,
            ..
        } = &method.kind
        else {
            return Ok(());
        };
        let fn_name = self.freefn_lowered_name(receiver_base, &name.name, is_public_method);
        // Combine the target's type params with the method's own onto the free
        // function (Go forbids method type params, but a free function may carry
        // both — mirrors the DQ28 free-function lowering).
        let mut combined = target_generics.to_vec();
        combined.extend(generic_params.iter().cloned());
        let type_params = self.format_generic_params(&combined);
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
        let saved_record_args = self.var_record_type_args.clone();
        let saved_decl_type = self.var_decl_type_node.clone();
        let (
            saved_opt_scope,
            saved_list_scope,
            saved_result_scope,
            saved_map_scope,
            saved_set_scope,
        ) = self.enter_param_optional_scope(params);
        self.pending_scope_seed = Some(self.param_binding_names(params));
        if return_type.is_some() && !is_void {
            let prev_ret = self.current_fn_ret_type.take();
            let prev_ret_coll = self.current_fn_ret_collection_elem.take();
            let prev_ret_node = self.current_fn_ret_type_node.take();
            self.current_fn_ret_type = return_type.as_deref().map(|t| self.type_to_go(t));
            self.current_fn_ret_collection_elem = return_type
                .as_deref()
                .and_then(|t| self.collection_elem_go_types(t));
            self.current_fn_ret_type_node = Self::fn_type_ret_node(return_type.as_deref());
            self.emit_block_body_return(body)?;
            self.current_fn_ret_type = prev_ret;
            self.current_fn_ret_collection_elem = prev_ret_coll;
            self.current_fn_ret_type_node = prev_ret_node;
        } else {
            self.emit_block_body(body)?;
        }
        self.var_optional_elem = saved_opt_scope;
        self.var_list_elem = saved_list_scope;
        self.var_result_elem = saved_result_scope;
        self.var_map_kv = saved_map_scope;
        self.var_set_elem = saved_set_scope;
        self.var_record_type_args = saved_record_args;
        self.var_decl_type_node = saved_decl_type;
        self.current_handler_vars = old_handler_vars;
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    fn collect_param_strs(&self, params: &[AIRNode]) -> Vec<String> {
        params
            .iter()
            .filter_map(|p| {
                if let NodeKind::Param { pattern, ty, .. } = &p.kind {
                    let name = self.pattern_to_binding_name(pattern);
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

    /// The in-scope `Clock` effect handler variable, if one is installed.
    ///
    /// When `Some`, the `Clock` time operations (`Instant.now`, `sleep`,
    /// `elapsed`) are routed through the handler instead of inlining the host
    /// primitive (Q-clock-handler-routing, §18.3.1/§18.4); when `None`, no
    /// handler is in scope and the default host primitive is emitted.
    fn clock_handler_var(&self) -> Option<&str> {
        self.current_handler_vars.get("Clock").map(String::as_str)
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
                // Declare-only temp from the shared value-CF hoist: Go needs a
                // type for `var x T`. The owning block pre-scanned this temp
                // against its following control-flow statement and recorded the
                // inferred Go type in `decl_only_types`; emit `var x T`. A typed
                // annotation, if present, wins. Falls back to `interface{}` when
                // the type cannot be inferred (the relocated CF still assigns it).
                if node.metadata.contains_key(crate::generator::DECL_ONLY_META) {
                    let binding = self.pattern_to_go_binding(pattern);
                    let type_str = ty
                        .as_ref()
                        .map(|t| self.type_to_go(t))
                        .or_else(|| self.decl_only_types.get(&binding).cloned())
                        .unwrap_or_else(|| "interface{}".to_string());
                    self.var_go_type.insert(binding.clone(), type_str.clone());
                    let ind = self.indent_str();
                    let _ = writeln!(self.buf, "{ind}var {binding} {type_str}");
                    return Ok(());
                }
                // `let x = expr?` — the `?` operand is unwrapped (or the enclosing
                // function returns early) into a temp, then bound to `x`. Handled
                // before the general `let` paths so the early-return statements are
                // emitted at statement position (Go has no expression-level
                // early-return). Only a plain `BindPat` target is handled here; a
                // destructuring `let (a, b) = expr?` is rare and falls through.
                if let NodeKind::Propagate { expr: inner } = &value.kind {
                    if matches!(pattern.kind, NodeKind::BindPat { .. }) {
                        let binding = self.pattern_to_go_binding(pattern);
                        let payload = self.emit_try_unwrap(inner)?;
                        let ind = self.indent_str();
                        let _ = writeln!(self.buf, "{ind}{binding} := {payload}");
                        self.writeln(&format!("_ = {binding}"));
                        // Record the Ok element type so a later typed use of the
                        // binding resolves, and track it for shadowing-`let`.
                        if let Some((ok, _)) = self.scrutinee_result_elems(inner) {
                            self.var_go_type.insert(binding.clone(), ok);
                        } else if let Some(elem) = self.scrutinee_optional_elem(inner) {
                            self.var_go_type.insert(binding.clone(), elem);
                        }
                        if binding != "_" {
                            self.go_record_declared(&binding);
                        }
                        return Ok(());
                    }
                }
                // Tuple-destructuring `let (a, b, …) = expr`. Go has no tuple
                // destructuring and `pattern_to_binding_name` collapses a tuple
                // pattern to its *first* element — so without this every later
                // name was dropped (`undefined: total`) and the first bound the
                // whole struct. Hoist the value into a `__tupN` struct local and
                // bind each element off its `.Field{i}` (recursing through the
                // shared bind emitter for a nested element pattern). A bare-`_`
                // tuple pattern still binds nothing.
                if let NodeKind::TuplePat { elems } = &pattern.kind {
                    let n = self.let_tuple_counter;
                    self.let_tuple_counter += 1;
                    let tmp = format!("__tup{n}");
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}{tmp} := ");
                    // The declared tuple type (when annotated) types each field;
                    // otherwise the struct literal/return value carries its own
                    // Go types and the `.Field{i}` reads inherit them.
                    self.emit_expr(value)?;
                    self.buf.push('\n');
                    self.writeln(&format!("_ = {tmp}"));
                    let decl_ty = ty.as_deref();
                    let field_tys = self.tuple_field_decl_tys(decl_ty, elems.len());
                    for (i, e) in elems.iter().enumerate() {
                        let access = format!("{tmp}.Field{i}");
                        let mut binds = String::new();
                        self.collect_binds_go(
                            e,
                            &access,
                            field_tys.get(i).and_then(|t| *t),
                            &mut binds,
                        );
                        for stmt in binds.split("; ") {
                            let stmt = stmt.trim();
                            if stmt.is_empty() {
                                continue;
                            }
                            self.writeln(stmt);
                            if let Some(name) = stmt.split_whitespace().next() {
                                if name != "_" {
                                    self.go_record_declared(name);
                                }
                            }
                        }
                    }
                    return Ok(());
                }
                let binding = self.pattern_to_go_binding(pattern);
                // Shadowing re-bind of a name already declared in this Go block
                // (the immutable-update idiom, `let acc = …; let acc = f(acc)`,
                // and the todo-list example's `let list = list.add(…)`). Go's
                // `:=` / `var` reject a re-declaration ("no new variables on left
                // side of :="), so lower it to a plain assignment `acc = …`. Only
                // a simple `BindPat` participates — a tuple/record destructure or
                // `_` is not the rebind idiom and keeps its declaration. The
                // existing `var_*`-scope recording below is skipped for a reassign
                // (the name's type is fixed by its first declaration), but the
                // value's expected-type hint is preserved so a branchy RHS still
                // lowers to a correctly-typed IIFE.
                if matches!(pattern.kind, NodeKind::BindPat { .. })
                    && binding != "_"
                    && self.go_name_declared_in_block(&binding)
                {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}{binding} = ");
                    let prev_expected_type = self.current_expected_type.take();
                    if let Some(existing) = self.var_go_type.get(&binding) {
                        self.current_expected_type = Some(existing.clone());
                    }
                    let prev_expected = self.expected_collection_elem.take();
                    if matches!(
                        value.kind,
                        NodeKind::ListLiteral { .. }
                            | NodeKind::MapLiteral { .. }
                            | NodeKind::SetLiteral { .. }
                    ) {
                        if let Some(t) = ty {
                            self.expected_collection_elem = self.collection_elem_go_types(t);
                        }
                    }
                    self.emit_expr(value)?;
                    self.current_expected_type = prev_expected_type;
                    self.expected_collection_elem = prev_expected;
                    self.buf.push('\n');
                    return Ok(());
                }
                if let Some(t) = ty {
                    // Record the full declared type node so a later `match` on
                    // this binding can peel a *nested* Optional/Result to assert
                    // a tuple payload to its concrete struct (mirrors the param
                    // path; see `var_decl_type_node`).
                    self.var_decl_type_node
                        .insert(self.pattern_to_binding_name(pattern), (**t).clone());
                    // Record an `Optional[T]` binding's element type so a later
                    // `match binding { Some(x) => ... }` can type-assert `x`.
                    if let Some(elem) = self.optional_elem_go_type(t) {
                        self.var_optional_elem
                            .insert(self.pattern_to_binding_name(pattern), elem);
                    }
                    // Record a `List[T]` binding's element type so a later
                    // `match binding.get(i) { Some(x) => ... }` can type-assert
                    // the `interface{}` payload.
                    if let Some(elem) = self.list_elem_go_type(t) {
                        self.var_list_elem
                            .insert(self.pattern_to_binding_name(pattern), elem);
                    }
                    // Record a `Map[K, V]` / `Set[E]` binding's element Go types
                    // so a later built-in method (`m.get(k)`, `s.contains(x)`,
                    // …) lowers its inline closures over the concretely-typed
                    // receiver `map[K]V` / `map[E]struct{}`.
                    if let Some(kv) = self.map_kv_go_types(t) {
                        self.var_map_kv
                            .insert(self.pattern_to_binding_name(pattern), kv);
                    }
                    if let Some(elem) = self.set_elem_go_type(t) {
                        self.var_set_elem
                            .insert(self.pattern_to_binding_name(pattern), elem);
                    }
                    // Record a `Result[T, E]` binding's Ok/Err types so a later
                    // `match binding { Ok(v) => ...; Err(e) => ... }` can
                    // type-assert the bound payload.
                    if let Some(elems) = self.result_elem_go_types(t) {
                        self.var_result_elem
                            .insert(self.pattern_to_binding_name(pattern), elems);
                    }
                    // Record a generic-record binding's concrete instantiation
                    // (`let c: ListIter[Int]` → `("ListIter", ["int64"])`) so a
                    // later `match c.next() { Some(x) => ... }` resolves the
                    // generic `Optional[T]` payload to the concrete arg (`int64`)
                    // rather than the undefined-in-caller `T`.
                    if let Some(record_args) = self.record_type_args(t) {
                        self.var_record_type_args
                            .insert(self.pattern_to_binding_name(pattern), record_args);
                    }
                    let type_str = self.type_to_go(t);
                    // Record the binding's rendered Go type so a later use as a
                    // call argument (`max_of(noKeys)` where `noKeys: List[Key]`)
                    // resolves to `[]Key`, letting a generic callee bind its
                    // element type from the argument rather than collapsing to
                    // `[any]`. Function-scoped: params save/restore `var_go_type`.
                    if let NodeKind::BindPat { name, .. } = &pattern.kind {
                        self.var_go_type
                            .insert(go_value_ident(&name.name), type_str.clone());
                    }
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}var {binding} {type_str} = ");
                    // When the binding *value* is itself a collection literal,
                    // it takes its element type(s) from the declared type, so an
                    // empty `[]` (or under-inferred literal) matches the declared
                    // `[]T` / `map[K]V` rather than falling back to
                    // `[]interface{}`. Guarded to the top-level literal so the
                    // hint never leaks to a nested/argument literal whose own
                    // type may differ.
                    let prev_expected = self.expected_collection_elem.take();
                    if matches!(
                        value.kind,
                        NodeKind::ListLiteral { .. }
                            | NodeKind::MapLiteral { .. }
                            | NodeKind::SetLiteral { .. }
                    ) {
                        self.expected_collection_elem = self.collection_elem_go_types(t);
                    }
                    // The binding's declared Go type is the expected type for the
                    // value expression. An expression-position `match`/`if` lowers
                    // to an IIFE whose return must be this `T` (not the enclosing
                    // function's return type), so `let x: T = match …` is
                    // assignable. Restored after the value so it never leaks.
                    let prev_expected_type = self.current_expected_type.take();
                    self.current_expected_type = Some(type_str.clone());
                    self.emit_expr(value)?;
                    self.current_expected_type = prev_expected_type;
                    self.expected_collection_elem = prev_expected;
                    self.buf.push('\n');
                } else {
                    // Propagate a `Map`/`Set` element type onto an untyped
                    // binding whose value returns a map/set (`let m2 =
                    // base.set(k, v)`, `let s2 = s.add(x)`), so a later built-in
                    // on `m2`/`s2` lowers its inline closure over the concrete
                    // `map[K]V` / `map[E]struct{}` rather than `interface{}`.
                    if let Some(kv) = self.value_map_kv_go_types(value) {
                        self.var_map_kv
                            .insert(self.pattern_to_binding_name(pattern), kv);
                    }
                    if let Some(elem) = self.value_set_elem_go_type(value) {
                        self.var_set_elem
                            .insert(self.pattern_to_binding_name(pattern), elem);
                    }
                    // Record an untyped binding's concrete generic-record args
                    // when its value is a call returning one (`__it := bag.Iter()`
                    // → `("ListIterator", ["int64"])`), so a later
                    // `match __it.next() { Some(x) => ... }` asserts the payload
                    // to the concrete arg. This is the `for x in <Iterable>`
                    // desugar case, whose gensym binding carries no annotation.
                    if let Some(record_args) = self.value_record_type_args(value) {
                        self.var_record_type_args
                            .insert(self.pattern_to_binding_name(pattern), record_args);
                    }
                    // Record an untyped `Result`-typed binding's `(ok, err)` Go
                    // payload types when its value is a call to a `Result`-returning
                    // fn (`step1 := eval(...)`) or a bare `Ok(v)`/`Err(e)`. Without
                    // this, a later `match step1 { Ok(v) => ... }` binds `v` from the
                    // boxed `interface{}` payload un-asserted, leaving `v` as
                    // `interface{}` and failing a downstream use that expects the
                    // concrete type (e.g. `eval(OpMul, v, 2.0)` wanting `float64`).
                    if let Some(elems) = self.value_result_elem_go_types(value) {
                        self.var_result_elem
                            .insert(self.pattern_to_binding_name(pattern), elems);
                    }
                    // Record an untyped `Optional`-typed binding's element Go type
                    // when its value is a call/method returning `Optional[T]`
                    // (`raw := storage.read(key)`). Reuses the scrutinee resolver,
                    // which already maps an effect-/user-method's `Optional[T]`
                    // return to its concrete element. Without this, a later
                    // `match raw { Some(v) => ... }` binds `v` from the boxed
                    // `interface{}` un-asserted, so a typed-IIFE arm (`return v`
                    // where `T` is `string`) fails the Go build.
                    if let Some(elem) = self.scrutinee_optional_elem(value) {
                        self.var_optional_elem
                            .insert(self.pattern_to_binding_name(pattern), elem);
                    }
                    // Record an untyped `List`-typed binding's Go element type — a
                    // homogeneous list literal (`let items = [Item{1}, Item{2}]` →
                    // `Item`) or a list-combinator result (`let updated =
                    // items.map(..)`, `let evens = xs.filter(..)`). Without this a
                    // later `updated.map((it) => …)` cannot type the closure param
                    // `it` to `Item` (it falls back to `interface{}`, so `it.title`
                    // and the `[]Item` result both fail), and a use of the binding
                    // as a typed call argument erases to `[]interface{}`.
                    if let Some(elem) = self.value_list_elem_go_type(value) {
                        let name = self.pattern_to_binding_name(pattern);
                        self.var_list_elem.insert(name.clone(), elem.clone());
                        self.var_go_type.insert(name, format!("[]{elem}"));
                    }
                    // Record an untyped binding to a record-returning value (`let
                    // form = create_form()` → `FormState`), so a later built-in
                    // collection method on one of its fields (`form.fields.keys()`,
                    // `form.fields.get(k)`) resolves the field's declared `Map[K,
                    // V]` / `List[T]` types through `map_receiver_kv_go_types` /
                    // the list analogue rather than erasing to the
                    // `map[interface{}]interface{}` / `[]interface{}` Go rejects
                    // against the concretely-typed struct field. Scoped to records
                    // that actually have such a field (the only consumers), so this
                    // never over-records a binding's Go type.
                    if !matches!(value.kind, NodeKind::Lambda { .. }) {
                        if let Some(go_ty) = self.infer_go_expr_type(value) {
                            let head = Self::go_type_record_head(&go_ty);
                            if self.record_field_map_kv.contains_key(head)
                                || self.record_field_list_elem.contains_key(head)
                            {
                                self.var_go_type
                                    .insert(self.pattern_to_binding_name(pattern), go_ty);
                            }
                        }
                    }
                    // Record an untyped binding to a lambda → the lambda's inferred
                    // Go return type, so a later compose `f >> binding` can resolve
                    // its own output type from `binding` (the outer local lambda).
                    if let NodeKind::Lambda { params, body } = &value.kind {
                        let saved = self.enter_param_go_types_with_expected(params, None);
                        let ret = self.infer_block_tail_type(body);
                        self.var_go_type = saved;
                        if let Some(r) = ret {
                            self.var_lambda_ret
                                .insert(self.pattern_to_binding_name(pattern), r);
                        }
                    }
                    // An untyped `let m = if (..) { Text } else { Image }` lowers
                    // its value to an expression IIFE. Without an expected type the
                    // IIFE falls back to the enclosing fn's return type
                    // (`current_fn_ret_type`, e.g. `__bockResult` inside a
                    // `Result`-returning fn), which a user-enum variant value
                    // (`MessageType`) is not assignable to. Infer the value's Go
                    // type structurally (the enum a variant branch/arm yields) and
                    // record it as the binding's expected type so the IIFE is typed
                    // `func() MessageType { … }`. Scoped to the value emit; never
                    // leaks. Only `if`/`match` values need this — a direct value
                    // emit is already concretely typed.
                    let prev_expected_type = self.current_expected_type.take();
                    if matches!(value.kind, NodeKind::If { .. } | NodeKind::Match { .. }) {
                        if let Some(inferred) = self.infer_branchy_expr_type(value) {
                            self.current_expected_type = Some(inferred.clone());
                            // Also record it for the binding so a later use of the
                            // binding (e.g. as a struct field) resolves to the enum.
                            self.var_go_type
                                .insert(self.pattern_to_binding_name(pattern), inferred);
                        }
                    }
                    let ind = self.indent_str();
                    // Bock `Int` is `int64`, but Go infers an *untyped integer
                    // constant* (`0`, `lo + 1`) as the default `int` under `:=`.
                    // A later mix with an `int64` value (`i >= int64(len(xs))`,
                    // `total + p` where `p: Int`, or a struct field `Level int64`)
                    // then fails `mismatched types int and int64`. When the value's
                    // structural Go type is `int64`, pin the binding with an explicit
                    // `var x int64 = …` and record it so downstream uses agree. Only
                    // `int64` needs this — `float64`/`bool`/`string` literals already
                    // infer to the matching Go type under `:=`. Skipped when the value
                    // is an `if`/`match`/`loop` (those lower to a typed IIFE) or a
                    // collection/record literal (handled above), to keep the targeted
                    // surface minimal.
                    //
                    // Restricted to value kinds whose `int64` Go type is *reliable*:
                    // an integer literal, integer arithmetic (`BinaryOp`/`UnaryOp`),
                    // or a known-`int64` identifier. A *call* is deliberately
                    // excluded — a generic fn (`firstOr[T](single(9), -1)`)
                    // monomorphizes `T` to Go's untyped-constant default `int`, so
                    // its *actual* return type is `int`, not the `int64`
                    // `infer_go_expr_type` predicts from the Bock `Int`; pinning
                    // `var x int64` there fails `cannot use … (int) as int64`.
                    let pin_int64_kind = matches!(
                        value.kind,
                        NodeKind::Literal { .. }
                            | NodeKind::BinaryOp { .. }
                            | NodeKind::UnaryOp { .. }
                            | NodeKind::Identifier { .. }
                    );
                    let pin_int64 = pin_int64_kind
                        && self.infer_go_expr_type(value).as_deref() == Some("int64");
                    if pin_int64 {
                        self.var_go_type
                            .insert(self.pattern_to_binding_name(pattern), "int64".to_string());
                        let _ = write!(self.buf, "{ind}var {binding} int64 = ");
                    } else {
                        let _ = write!(self.buf, "{ind}{binding} := ");
                    }
                    self.emit_expr(value)?;
                    self.current_expected_type = prev_expected_type;
                    self.buf.push('\n');
                }
                // Record this name as declared in the current Go block scope so a
                // later same-name `let` in the same block reassigns (see the
                // shadowing short-circuit above). Only a simple `BindPat`
                // introduces a tracked declaration.
                if matches!(pattern.kind, NodeKind::BindPat { .. }) && binding != "_" {
                    self.go_record_declared(&binding);
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
                let mut binding = self.pattern_to_go_binding(pattern);
                // Go rejects a `range` loop variable that is never read
                // (`declared and not used`), which Bock permits (`for x in data {
                // count = count + 1 }`). When the bound name is a plain
                // identifier not referenced in the body, emit `_` instead.
                if let NodeKind::BindPat { name, .. } = &pattern.kind {
                    let go_name = go_value_ident(&name.name);
                    if go_name == binding && !collect_used_idents(body).contains(&name.name) {
                        binding = "_".to_string();
                    }
                }
                self.emit_loop_label_prefix(body);
                let ind = self.indent_str();
                // `for _, _ := range x` is invalid Go ("no new variables on left
                // side of :="); when the value var is discarded too, drop the
                // assignment entirely (`for range x`).
                if binding == "_" {
                    let _ = write!(self.buf, "{ind}for range ");
                } else {
                    let _ = write!(self.buf, "{ind}for _, {binding} := range ");
                }
                self.emit_expr(iterable)?;
                self.buf.push_str(" {\n");
                self.indent += 1;
                // Record the loop variable's element Go type into the body scope
                // so element arithmetic / typed returns type-check (Go ranges
                // over a concretely-typed `[]T` / range yield a `T`, not
                // `interface{}`). Recoverable when the iterable is a known
                // `List[T]` identifier, a homogeneously-typed list literal, or a
                // range (`int64`). Saved/restored around the body — Go has no
                // block-scoped reset here. Unrecoverable ⇒ left absent, so
                // inference yields the `interface{}` fallback, never a wrong
                // type.
                let saved_go_types = self.var_go_type.clone();
                if let (NodeKind::BindPat { name, .. }, Some(elem)) =
                    (&pattern.kind, self.for_loop_elem_go_type(iterable))
                {
                    self.var_go_type.insert(go_value_ident(&name.name), elem);
                }
                self.emit_block_body(body)?;
                self.var_go_type = saved_go_types;
                self.indent -= 1;
                self.writeln("}");
                self.loop_labels.pop();
                Ok(())
            }
            NodeKind::While { condition, body } => {
                self.emit_loop_label_prefix(body);
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}for ");
                self.emit_expr(condition)?;
                self.buf.push_str(" {\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                self.loop_labels.pop();
                Ok(())
            }
            NodeKind::Loop { body } => {
                // A statement-position `loop` is a plain `for {}`. Its body is no
                // longer the value-IIFE body of any enclosing expression-`loop`, so
                // reset `loop_expr_depth` for the body: a `break <v>` here is the
                // (value-dropping) statement case, not a `return`.
                let saved_loop_expr = self.loop_expr_depth;
                self.loop_expr_depth = 0;
                self.emit_loop_label_prefix(body);
                self.writeln("for {");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                self.loop_labels.pop();
                self.loop_expr_depth = saved_loop_expr;
                Ok(())
            }
            NodeKind::Return { value } => {
                if let Some(val) = value {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}return ");
                    // A collection literal in return position adopts the
                    // function's return collection element type(s), so `return
                    // [x]` in `fn single[T](x: T) -> List[T]` emits `[]T{x}` (not
                    // the `[]interface{}{x}` bare-literal inference falls back to,
                    // which is not assignable to the `[]T` return). Guarded to a
                    // top-level collection literal and consumed at the literal so
                    // it never leaks to a nested/argument literal.
                    let prev_expected = self.expected_collection_elem.take();
                    if matches!(
                        val.kind,
                        NodeKind::ListLiteral { .. }
                            | NodeKind::MapLiteral { .. }
                            | NodeKind::SetLiteral { .. }
                    ) {
                        self.expected_collection_elem = self.current_fn_ret_collection_elem.clone();
                    }
                    // A generic-record construction in explicit-`return` position
                    // adopts the function's return type for its args (see
                    // `emit_block_body_inner`'s tail-return arm).
                    let prev_expected_type = self.current_expected_type.take();
                    if matches!(
                        val.kind,
                        NodeKind::RecordConstruct { .. } | NodeKind::TupleLiteral { .. }
                    ) {
                        self.current_expected_type = self.current_fn_ret_type.clone();
                    }
                    self.emit_expr(val)?;
                    self.expected_collection_elem = prev_expected;
                    self.current_expected_type = prev_expected_type;
                    self.buf.push('\n');
                } else {
                    self.writeln("return");
                }
                Ok(())
            }
            NodeKind::Break { value } => {
                // Inside an expression-position `loop` IIFE, `break <v>` is the
                // loop's value: lower it to `return <v>` (out of the IIFE). The
                // value-dropping `// break value:` comment below would otherwise
                // discard it and leave the IIFE with no return at all.
                if self.loop_expr_depth > 0 {
                    if let Some(val) = value {
                        let ind = self.indent_str();
                        let _ = write!(self.buf, "{ind}return ");
                        self.emit_expr(val)?;
                        self.buf.push('\n');
                        return Ok(());
                    }
                    // A bare `break` inside a value `loop` cannot produce the
                    // loop's value; fall through to the ordinary `break` below
                    // (the IIFE's trailing `panic` covers the unreachable tail).
                }
                if let Some(val) = value {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}// break value: ");
                    self.emit_expr(val)?;
                    self.buf.push('\n');
                }
                // Inside a statement-arm `switch`, a bare `break` would exit
                // the switch; target the enclosing loop's label instead.
                if self.switch_label_depth > 0 {
                    if let Some(label) = self.innermost_loop_label() {
                        self.writeln(&format!("break {label}"));
                        return Ok(());
                    }
                }
                self.writeln("break");
                Ok(())
            }
            NodeKind::Continue => {
                // `continue` already targets the loop even from inside a switch,
                // but use the label when one is in scope for symmetry/clarity.
                if self.switch_label_depth > 0 {
                    if let Some(label) = self.innermost_loop_label() {
                        self.writeln(&format!("continue {label}"));
                        return Ok(());
                    }
                }
                self.writeln("continue");
                Ok(())
            }
            NodeKind::Guard {
                let_pattern,
                condition,
                else_block,
            } => {
                if let Some(pat) = let_pattern {
                    return self.emit_guard_let(pat, condition, else_block);
                }
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
                self.seed_decl_only_types(stmts);
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
                    self.seed_decl_only_types(stmts);
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
                if name.name == "None" {
                    self.buf.push_str("__bockNone");
                    return Ok(());
                }
                // Prelude `Ordering` variant → the bare `__bockOrdering` constant
                // (`Less`/`Equal`/`Greater`) of the Ordering runtime, which a
                // value-switch `case Less:` and the `compare` bridge also use —
                // UNLESS the real `core.compare.Ordering` enum is reachable, in
                // which case the reference is a user-enum variant-struct
                // construction (`OrderingLess{}`), handled by the path below.
                if crate::generator::ordering_variant(&name.name).is_some()
                    && !self.ordering_enum_reachable()
                {
                    self.buf.push_str(&name.name);
                    return Ok(());
                }
                // A unit-variant reference (`Empty`) → an empty variant-struct
                // literal `ShapeEmpty{}`.
                if let Some(enum_name) = self
                    .user_variant_for_name(&name.name)
                    .map(|i| i.enum_name.clone())
                {
                    let _ = write!(self.buf, "{enum_name}{}{{}}", name.name);
                    return Ok(());
                }
                // A module-scope `const` is emitted verbatim at its declaration;
                // spell its use site identically (the `go_fn_name` transform would
                // camelCase a SCREAMING_SNAKE name, e.g. `FIZZ_NUM` → `fizzNUM`).
                if self.const_names.contains(&name.name) {
                    self.buf.push_str(&name.name);
                    return Ok(());
                }
                let emitted = if is_prelude_ctor(&name.name) {
                    name.name.clone()
                } else if self.local_shadows_public_fn(&name.name) {
                    // An in-scope local (param/`let`/bind) shadows a same-named
                    // public module fn — spell the *local*, not the PascalCased
                    // helper (Q-go-runtime-helper-shadowing).
                    go_value_ident(&name.name)
                } else {
                    // Routes a public name colliding with a type through the
                    // `Fn`-suffix rename (`key` → `KeyFn`); a private name is
                    // camelCased.
                    self.go_fn_name(&name.name)
                };
                self.buf.push_str(&emitted);
                Ok(())
            }
            NodeKind::BinaryOp { op, left, right } => {
                // `+` on two `List[T]` operands is concatenation. Go has no `+`
                // for slices (`operator + not defined on []T`), so build a fresh
                // slice with `append(append([]T{}, a...), b...)`. The element type
                // comes from a list-typed operand or the binding's expected type.
                if matches!(op, BinOp::Add) && crate::generator::is_list_concat(node, left, right) {
                    let elem = self
                        .list_receiver_elem_go_type(left)
                        .or_else(|| self.list_receiver_elem_go_type(right))
                        .or_else(|| {
                            self.current_expected_type
                                .as_deref()
                                .and_then(|t| t.strip_prefix("[]"))
                                .map(str::to_string)
                        })
                        .unwrap_or_else(|| "interface{}".to_string());
                    // A list-literal operand must adopt `elem` as its element type
                    // — a `[]interface{}{x}` literal is not assignable to the
                    // `[]elem` slice `append` builds. Thread the expected element
                    // into each literal operand (mirrors the `concat` method).
                    let emit_operand =
                        |this: &mut Self, n: &AIRNode| -> Result<String, CodegenError> {
                            let prev = this.expected_collection_elem.take();
                            if matches!(n.kind, NodeKind::ListLiteral { .. }) {
                                this.expected_collection_elem = Some((elem.clone(), None));
                            }
                            let s = this.expr_to_string(n);
                            this.expected_collection_elem = prev;
                            s
                        };
                    let l = emit_operand(self, left)?;
                    let r = emit_operand(self, right)?;
                    let _ = write!(self.buf, "append(append([]{elem}{{}}, {l}...), {r}...)");
                    return Ok(());
                }
                // `a ** b`: Go has no `**`. A *float* power lowers to `math.Pow`
                // (which takes and returns `float64`); an *integer* power lowers
                // to the `__bockIntPow` runtime helper (stays in `int64`, exact).
                // Operands are coerced to the chosen numeric type so a mixed
                // `2 ** f` (Int literal ** Float) still type-checks.
                if matches!(op, BinOp::Pow) {
                    let l = self.expr_to_string(left)?;
                    let r = self.expr_to_string(right)?;
                    if self.pow_is_float(left, right) {
                        self.needs_math_import = true;
                        let _ = write!(self.buf, "math.Pow(float64({l}), float64({r}))");
                    } else {
                        let _ = write!(self.buf, "__bockIntPow(int64({l}), int64({r}))");
                    }
                    return Ok(());
                }
                // Ordering operators on a user `Comparable` type lower through the
                // type's `Compare` (Go structs are not ordered, so native `<` is a
                // compile error). `Compare` returns the sealed `Ordering` interface;
                // a type assertion against the variant struct yields the boolean,
                // wrapped in an IIFE (Go's comma-ok assertion is statement-only):
                // `a < b` ⇒ `… .(OrderingLess); return __ok`, `a <= b` ⇒
                // `… .(OrderingGreater); return !__ok`, etc.
                if crate::generator::is_user_compare(node) {
                    if let Some((tag, is_eq)) = crate::generator::user_compare_variant(*op) {
                        let recv = self.expr_to_string(left)?;
                        let other = self.expr_to_string(right)?;
                        let method = self.go_method_name("compare", true);
                        let neg = if is_eq { "" } else { "!" };
                        let _ = write!(
                            self.buf,
                            "func() bool {{ _, __ok := ({recv}.{method}({other})).(Ordering{tag}); return {neg}__ok }}()"
                        );
                        return Ok(());
                    }
                }
                // DQ29 (§18.5 structural Equatable): a stamped `==`/`!=` whose
                // operand has an explicit `impl Equatable` dispatches through
                // its `Eq` method (Go's native struct `==` is field-wise and
                // would silently ignore the user's custom equality). The
                // `"deep"` lane — operands involving a `List`/`Map`/`Set`
                // (no native `==`: "slice/map can only be compared to nil") —
                // lowers through the `__bockDeepEq` runtime helper, whose
                // `reflect.DeepEqual` is element-wise for slices and
                // order-independent for maps (Bock `Map`/`Set`). The
                // `"structural"` and `"generic"` lanes stay native: Go struct/
                // interface equality is already field-wise (tag-then-payload
                // for the enum interface form), and a `comparable`-constrained
                // type param compares natively.
                if matches!(op, BinOp::Eq | BinOp::Ne) {
                    match crate::generator::user_eq_kind(node) {
                        Some("impl") => {
                            let recv = self.expr_to_string(left)?;
                            let other = self.expr_to_string(right)?;
                            let method = self.go_method_name("eq", true);
                            let neg = if *op == BinOp::Ne { "!" } else { "" };
                            let _ = write!(self.buf, "{neg}({recv}).{method}({other})");
                            return Ok(());
                        }
                        Some("deep") => {
                            let recv = self.expr_to_string(left)?;
                            let other = self.expr_to_string(right)?;
                            let neg = if *op == BinOp::Ne { "!" } else { "" };
                            let _ = write!(self.buf, "{neg}__bockDeepEq({recv}, {other})");
                            return Ok(());
                        }
                        _ => {}
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
                    // `Pow` is lowered above (math.Pow / __bockIntPow) and never
                    // reaches this arm; kept for match exhaustiveness.
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
                // A call whose callee names a registered tuple variant is a
                // construction → the variant-struct literal
                // `ShapeRect{Field0: 3.0, Field1: 4.0}`.
                if let NodeKind::Identifier { name } = &callee.kind {
                    if let Some(enum_name) = self
                        .user_variant_for_name(&name.name)
                        .map(|i| i.enum_name.clone())
                    {
                        let _ = write!(self.buf, "{enum_name}{}{{", name.name);
                        for (i, arg) in args.iter().enumerate() {
                            if i > 0 {
                                self.buf.push_str(", ");
                            }
                            let _ = write!(self.buf, "Field{i}: ");
                            self.emit_expr(&arg.value)?;
                        }
                        self.buf.push('}');
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
                // checker's `recv_kind = "Primitive:String"`, not by name alone —
                // the fix for `String.contains` being misrouted to the List scan.
                if self.try_emit_string_method(node, callee, args)? {
                    return Ok(());
                }
                // Numeric/Char/Bool primitive methods (`to_float`/`abs`/`sqrt`/…)
                // likewise route by the checker's `recv_kind = "Primitive:Int|…"`
                // before the generic fall-through, which would emit `n.toFloat(n)`.
                if self.try_emit_numeric_method(node, callee, args)? {
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
                // lowers to Go's native conversion, NOT the free-function form
                // below (`Float_from` is undefined).
                if self.try_emit_primitive_conversion(node, callee, args)? {
                    return Ok(());
                }
                // Associated-function call (`Type.method(args)` — stamped by the
                // lowerer, no `self` prepended). Go has no static methods, so the
                // definition is a free function `Type_Method(...)`
                // (`emit_associated_fn`); emit the matching free-function call.
                // The `public_methods` check picks the same Pascal/camel casing
                // the definition used (trait-impl associated fns are always
                // exported).
                if crate::generator::is_associated_call(node) {
                    if let NodeKind::FieldAccess { object, field } = &callee.kind {
                        if let NodeKind::Identifier { name: type_name } = &object.kind {
                            let is_public = self.public_methods.contains(&field.name);
                            let fn_name =
                                self.freefn_lowered_name(&type_name.name, &field.name, is_public);
                            let _ = write!(self.buf, "{fn_name}(");
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
                // [recv, ...rest])`: emit `recv.M(rest)` using Go method casing
                // so the receiver flows through the native `self` receiver
                // rather than as a duplicated `interface{}` argument.
                if let Some((recv, method, rest)) =
                    crate::generator::desugared_self_call(callee, args)
                {
                    // DQ28 free-function lowering: a generic method
                    // (`box.map(f)`) lowers to a free function call
                    // `Box_Map(box, f)` — the receiver leads as the first
                    // argument. The method name uniquely identifies the type
                    // (poisoned otherwise), so the rewrite is unambiguous.
                    if let Some(ty) = self.freefn_lowered_type(&method.name).map(str::to_string) {
                        let is_public = self.public_methods.contains(&method.name);
                        let fn_name = self.freefn_lowered_name(&ty, &method.name, is_public);
                        let _ = write!(self.buf, "{fn_name}(");
                        self.emit_expr(recv)?;
                        for arg in rest {
                            self.buf.push_str(", ");
                            self.emit_expr(&arg.value)?;
                        }
                        self.buf.push(')');
                        return Ok(());
                    }
                    self.emit_expr(recv)?;
                    let go_method = self
                        .go_method_name(&method.name, self.public_methods.contains(&method.name));
                    let _ = write!(self.buf, ".{go_method}(");
                    for (i, arg) in rest.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.emit_expr(&arg.value)?;
                    }
                    self.buf.push(')');
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
                        let go_name = self.go_fn_name(&name.name);
                        self.buf.push_str(&format!("{go_name}Async"));
                    } else {
                        self.emit_expr(callee)?;
                    }
                } else {
                    self.emit_expr(callee)?;
                }
                // When the callee is a known generic fn, recover its signature so
                // we can (a) synthesise explicit Go type-arguments the source
                // omits but Go cannot infer (a `Optional[T]`/`Result[T, E]` param
                // erases `T`/`E` from the monomorphic runtime struct), and (b)
                // specialise each untyped lambda argument to the concrete Go param
                // type (`func(int64) bool` for `filter(it, (x) => x > 2)`).
                let fn_sig = if let NodeKind::Identifier { name } = &callee.kind {
                    self.fn_signatures.get(&name.name).cloned()
                } else {
                    None
                };
                // Synthesise the turbofish only when the source carried none.
                // An explicit source `f[Ty](..)` (`type_args`) always wins.
                let callee_sealed_bound = matches!(&callee.kind, NodeKind::Identifier { name }
                    if self.fn_sealed_bound.contains(&name.name));
                let synthesized_type_args = if type_args.is_empty() {
                    fn_sig.as_ref().and_then(|(gp, ptys, ret)| {
                        // A container-touching signature defeats Go's own inference
                        // (the `Optional`/`Result` runtime erases `T`); a
                        // sealed-core-bound fn defeats untyped-constant inference
                        // (GAP-C). Either forces explicit type args.
                        self.synthesize_go_type_args(
                            gp,
                            ptys,
                            ret.as_ref(),
                            args,
                            callee_sealed_bound,
                        )
                    })
                } else {
                    None
                };
                let type_arg_str = if let Some(syn) = &synthesized_type_args {
                    format!("[{}]", syn.join(", "))
                } else {
                    self.format_generic_args(type_args)
                };
                self.buf.push_str(&type_arg_str);
                self.buf.push('(');
                // Bind the callee's type params from the (synthesised args, when
                // available, else the non-lambda arguments) so each untyped lambda
                // argument can be specialised to the concrete Go param type rather
                // than the `interface{}` default that breaks the body and
                // mismatches the typed callee param. The synthesised binding is
                // strictly more complete than the arg-only one — it also pins a
                // `T` that only appears behind `Optional[T]`/`Result[T, E]`, which
                // is exactly the `filter`/`and_then` lambda case.
                let lambda_bindings = fn_sig
                    .as_ref()
                    .map(|(gp, ptys, _)| {
                        let binds = match (&synthesized_type_args, gp.len()) {
                            (Some(syn), n) if syn.len() == n => {
                                gp.iter().cloned().zip(syn.iter().cloned()).collect()
                            }
                            _ => self.bind_fn_type_params(gp, ptys, args),
                        };
                        (gp.clone(), ptys.clone(), binds)
                    })
                    .or_else(|| {
                        // Non-generic callee: no type params to bind, but its
                        // concrete `Fn(...)` param types still pin an untyped lambda
                        // argument (`count_where(todos, (t) => t.done)` →
                        // `func(t Todo) bool`). Empty gp/bindings means
                        // `specialise_lambda_param_types` renders the param types
                        // verbatim.
                        if let NodeKind::Identifier { name } = &callee.kind {
                            self.fn_param_types
                                .get(&name.name)
                                .map(|ptys| (Vec::new(), ptys.clone(), HashMap::new()))
                        } else {
                            None
                        }
                    });
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    let prev_lambda = self.expected_lambda_param_types.take();
                    let prev_forced_ret = self.forced_lambda_ret.take();
                    let prev_coll = self.expected_collection_elem.take();
                    if matches!(arg.value.kind, NodeKind::Lambda { .. }) {
                        if let Some((gp, ptys, binds)) = &lambda_bindings {
                            if let Some(pty) = ptys.get(i).and_then(|p| p.as_ref()) {
                                self.expected_lambda_param_types =
                                    self.specialise_lambda_param_types(pty, gp, binds);
                                // A `Fn(...) -> Void` callee parameter pins the
                                // lambda's return to the Void marker so the closure
                                // emits `func(...) { <stmts> }` (no result, no
                                // `return`). Without this, a `() => println(...)`
                                // call argument would emit `func() interface{} {
                                // return fmt.Println(...) }` — both a type mismatch
                                // against the `func()` parameter and a Go arity
                                // error (`fmt.Println` returns `(int, error)`). The
                                // parameter may name a `type Handler = Fn() -> Void`
                                // alias (react-components' `EventHandler`), so peel
                                // one alias layer first.
                                let resolved = self.resolve_type_alias(pty).unwrap_or(pty);
                                if let NodeKind::TypeFunction { ret, .. } = &resolved.kind {
                                    if Self::is_void_type(ret) {
                                        self.forced_lambda_ret = Some("struct{}".to_string());
                                    }
                                }
                            }
                        }
                    } else if matches!(
                        arg.value.kind,
                        NodeKind::ListLiteral { .. }
                            | NodeKind::MapLiteral { .. }
                            | NodeKind::SetLiteral { .. }
                    ) {
                        // A collection literal argument (most importantly an empty
                        // `[]` / `{}` whose own elements can't infer a type) adopts
                        // the callee's declared parameter element type, so it emits
                        // `[]int64{}` against a `List[Int]` param rather than the
                        // erased `[]interface{}{}` Go rejects. The param type comes
                        // from the non-generic `fn_param_types` record.
                        if let NodeKind::Identifier { name } = &callee.kind {
                            if let Some(pty) = self
                                .fn_param_types
                                .get(&name.name)
                                .and_then(|ptys| ptys.get(i))
                                .and_then(|p| p.as_ref())
                            {
                                self.expected_collection_elem = self.collection_elem_go_types(pty);
                            }
                        }
                    }
                    self.emit_expr(&arg.value)?;
                    self.expected_lambda_param_types = prev_lambda;
                    self.forced_lambda_ret = prev_forced_ret;
                    self.expected_collection_elem = prev_coll;
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
                // DQ28 free-function lowering (the non-desugared `MethodCall`
                // shape): `box.map(f)` → `Box_Map(box, f)`, receiver-first.
                if let Some(ty) = self.freefn_lowered_type(&method.name).map(str::to_string) {
                    let is_public = self.public_methods.contains(&method.name);
                    let fn_name = self.freefn_lowered_name(&ty, &method.name, is_public);
                    let _ = write!(self.buf, "{fn_name}(");
                    self.emit_expr(receiver)?;
                    for arg in args {
                        self.buf.push_str(", ");
                        self.emit_expr(&arg.value)?;
                    }
                    self.buf.push(')');
                    return Ok(());
                }
                self.emit_expr(receiver)?;
                // `MethodCall` dispatches a method through Go method casing. A
                // method whose name collides with a struct field is suffixed
                // identically here and at the declaration (`go_method_name`).
                let go_method =
                    self.go_method_name(&method.name, self.public_methods.contains(&method.name));
                let _ = write!(self.buf, ".{go_method}");
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
                // An untyped lambda argument adopts the callee's specialised
                // parameter types when known (`expected_lambda_param_types`,
                // e.g. `func(int64) bool` for `filter(it, (x) => x > 2)`), so
                // the body's arithmetic type-checks and the closure satisfies
                // the typed callee parameter. Consume the hint so it never
                // leaks to a nested lambda in the body.
                let expected_params = self.expected_lambda_param_types.take();
                // A `f >> g` compose desugars (in shared AIR) to `(__compose_x) =>
                // g(f(__compose_x))` with an *untyped* `__compose_x`. Recover its
                // Go type from `f`'s first declared parameter type so the emitted
                // closure is `func(x []float64) ...` rather than `func(x
                // interface{}) ...` — the latter is not assignable to a typed
                // `Fn(List[Float]) -> List[Float]` callee parameter.
                let compose_param = if expected_params.is_none() {
                    self.compose_lambda_param_go_type(params, body)
                } else {
                    None
                };
                let param_strs = match (&expected_params, &compose_param) {
                    (Some(tys), _) if tys.len() == params.len() => {
                        self.collect_param_strs_with_types(params, tys)
                    }
                    (None, Some(ty)) => {
                        self.collect_param_strs_with_types(params, std::slice::from_ref(ty))
                    }
                    _ => self.collect_param_strs(params),
                };
                // Record the lambda's typed params so the body's return type can
                // be inferred structurally. Without a concrete return type Go
                // infers `interface{}`, which fails to satisfy a typed
                // `func(int64) int64` parameter at the use site.
                let scope_expected = expected_params
                    .as_deref()
                    .or(compose_param.as_ref().map(std::slice::from_ref));
                let saved_go_types =
                    self.enter_param_go_types_with_expected(params, scope_expected);
                // A predicate combinator pins the return type to `bool` (consumed
                // here so it never leaks to a nested lambda); otherwise infer it.
                // Use the block-tail inference so a lambda whose body is a block /
                // `if` / `match` (`(t) => { if (..) { t.complete() } else { t } }`)
                // gets a concrete return type (`Todo`) rather than `interface{}` —
                // this must agree with the result-slice element type the
                // list-combinator emitter derives from the same inference, or
                // `append(__out, __f(__x))` mismatches `[]Todo` vs `interface{}`.
                let forced_ret = self.forced_lambda_ret.take();
                // A `Fn(...) -> Void` callee pins `forced_ret` to the Void value
                // type `struct{}` (and the *type* now renders `func(...)` with no
                // result, see `type_to_go`'s `TypeFunction` arm). Such a lambda is
                // void: its closure must be `func(...) { <stmts> }` — no result
                // type, no `return`. Emitting `func() struct{} { return <body> }`
                // is doubly wrong: it does not match the `func()` parameter type,
                // and a void-call body (`println(...)` → Go's `fmt.Println`, which
                // returns `(int, error)`) makes `return fmt.Println(...)` a Go
                // arity error. When `forced_ret` is unset, a lambda whose body tail
                // is a void call (a bare `() => println(...)`) is likewise void.
                let body_tail = match &body.kind {
                    NodeKind::Block { tail: Some(t), .. } => t.as_ref(),
                    NodeKind::Block { tail: None, .. } => body,
                    _ => body,
                };
                let is_void_lambda = forced_ret.as_deref() == Some("struct{}")
                    || (forced_ret.is_none()
                        && expected_params.is_none()
                        && compose_param.is_none()
                        && self.is_void_call(body_tail));
                if is_void_lambda {
                    // Statement-style void closure: `func(params) { <stmts> }`. The
                    // body's effective tail void call is emitted as a statement
                    // (never `return`d). Mirrors the function-body void path.
                    let _ = write!(self.buf, "func({}) {{ ", param_strs.join(", "));
                    let prev_ret = self.current_fn_ret_type.take();
                    let prev_expected = self.current_expected_type.take();
                    if let NodeKind::Block { stmts, .. } = &body.kind {
                        for s in stmts {
                            self.emit_node(s)?;
                            self.buf.push_str("; ");
                        }
                    }
                    self.emit_expr(body_tail)?;
                    self.current_fn_ret_type = prev_ret;
                    self.current_expected_type = prev_expected;
                    self.buf.push_str(" }");
                    self.var_go_type = saved_go_types;
                    return Ok(());
                }
                let ret_ty = forced_ret.unwrap_or_else(|| {
                    self.infer_block_tail_type(body)
                        .unwrap_or_else(|| "interface{}".to_string())
                });
                let _ = write!(
                    self.buf,
                    "func({}) {ret_ty} {{ return ",
                    param_strs.join(", ")
                );
                // The lambda body is a fresh return scope: a `match`/`if`/`loop`
                // in its tail lowers to an IIFE whose type is *this lambda's*
                // return type, not the enclosing function's. Without resetting
                // these, an inner match IIFE in a `filter`/`map` lambda inherited
                // the outer fn's return type (chat-protocol: a `bool` match body
                // typed `func() []Message`, the surrounding `filter`'s return).
                let prev_ret = self.current_fn_ret_type.take();
                let prev_expected = self.current_expected_type.take();
                self.current_fn_ret_type = (ret_ty != "interface{}").then(|| ret_ty.clone());
                self.emit_expr(body)?;
                self.current_fn_ret_type = prev_ret;
                self.current_expected_type = prev_expected;
                self.buf.push_str(" }");
                self.var_go_type = saved_go_types;
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
                // Go has no native range *value*; lower to the injected
                // `__bockRange(lo, hi, inclusive)` helper (a `[]int64`), so
                // `for _, i := range <range>` iterates the materialised slice.
                // The runtime is emitted once at the Module arm
                // (`go_module_uses_range`).
                self.buf.push_str("__bockRange(");
                self.emit_expr(lo)?;
                self.buf.push_str(", ");
                self.emit_expr(hi)?;
                let _ = write!(self.buf, ", {inclusive})");
                Ok(())
            }
            NodeKind::RecordConstruct {
                path,
                fields,
                spread,
            } => {
                // A struct-variant construction (`Circle { radius: .. }`) → the
                // `{enum}{variant}` struct literal `ShapeCircle{Radius: ..}`
                // (field name `to_pascal_case`d). Plain records keep their path.
                let type_name = self.record_construct_go_type_name(path);
                // Go requires an explicit type-argument list on a generic
                // struct literal (`Box[int64]{...}`); it does NOT infer the args
                // from the field values. Prefer the declared/expected binding
                // type's concrete args (`let c: ListIter[Int] = ListIter { ... }`
                // → `[int64]`), which works even when a param appears only
                // *nested* in a field type (`record ListIter[T] { xs: List[T] }`,
                // where no field is typed exactly `T` so field-inference yields
                // `any`). Fall back to inferring each param's type from the field
                // value that names it directly.
                let type_args = self
                    .expected_construct_type_args(&type_name)
                    .unwrap_or_else(|| self.infer_construct_type_args(&type_name, fields));
                if let Some(sp) = spread {
                    // Go has no struct-spread syntax (`Point{..p}`), so a record
                    // spread lowers to an IIFE that copies the base value, then
                    // assigns each override field, then returns the copy:
                    //   func() T { __s := <base>; __s.Field = val; …; return __s }()
                    // The base is the spread expression; the overrides are the
                    // explicitly-given fields. (A struct copy in Go is a value
                    // copy, so this does not mutate the base.)
                    let _ = write!(self.buf, "func() {type_name}{type_args} {{ __s := ");
                    self.emit_expr(sp)?;
                    self.buf.push_str("; ");
                    for f in fields {
                        let _ = write!(self.buf, "__s.{} = ", to_pascal_case(&f.name.name));
                        if let Some(val) = &f.value {
                            self.emit_expr(val)?;
                        } else {
                            self.buf.push_str(&to_camel_case(&f.name.name));
                        }
                        self.buf.push_str("; ");
                    }
                    self.buf.push_str("return __s }()");
                } else {
                    // A field whose declared type is `List[..]` (registered in
                    // `record_field_list_elem`) supplies the expected element type
                    // for a list-literal value, so an empty `[]` / under-inferred
                    // literal field emits `[]<elem>{}` matching the struct's `[]T`
                    // field rather than the erased `[]interface{}{}` Go rejects.
                    // The element type is the field's declared param (`T`),
                    // substituted with the construct's resolved concrete args
                    // (`SortedSet[Key]` → `T` ↦ `Key`).
                    let record_name = path.segments.last().map(|s| s.name.clone());
                    let param_subst = record_name
                        .as_deref()
                        .map(|rn| self.record_param_substitution(rn, &type_args))
                        .unwrap_or_default();
                    self.buf.push_str(&format!("{type_name}{type_args}{{"));
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        let _ = write!(self.buf, "{}: ", to_pascal_case(&f.name.name));
                        if let Some(val) = &f.value {
                            let field_elem = record_name.as_deref().and_then(|rn| {
                                self.record_field_list_elem
                                    .get(rn)
                                    .and_then(|m| m.get(&f.name.name))
                                    .map(|e| Self::apply_type_subst(e, &param_subst))
                            });
                            let prev_expected = self.expected_collection_elem.take();
                            if let (Some(elem), true) = (
                                field_elem,
                                matches!(
                                    val.kind,
                                    NodeKind::ListLiteral { .. } | NodeKind::SetLiteral { .. }
                                ),
                            ) {
                                self.expected_collection_elem = Some((elem, None));
                            }
                            self.emit_expr(val)?;
                            self.expected_collection_elem = prev_expected;
                        } else {
                            self.buf.push_str(&to_camel_case(&f.name.name));
                        }
                    }
                    self.buf.push('}');
                }
                Ok(())
            }
            NodeKind::ListLiteral { elems } => {
                // A declared binding type (`let x: List[T] = ...`) takes priority
                // so an empty `[]` matches the declared `[]T`; otherwise infer a
                // homogeneous element type so `[1, 2, 3]` emits `[]int64{...}`
                // (not `[]interface{}{...}`), letting element arithmetic / typed
                // iteration / typed returns compile. Falls back to `interface{}`
                // when neither is available (empty literal with no declared
                // type, mixed types, unresolved operands).
                let expected = self.expected_collection_elem.take();
                let elem_ty = expected
                    .map(|(e, _)| e)
                    .or_else(|| self.infer_homogeneous_elem_type(elems))
                    .unwrap_or_else(|| "interface{}".to_string());
                let _ = write!(self.buf, "[]{elem_ty}{{");
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
                // A declared `Map[K, V]` binding type takes priority (so an
                // empty `{}` matches `map[K]V`); otherwise infer key and value
                // element types separately so `{"a": 1}` emits
                // `map[string]int64{...}`. Either falling back to `interface{}`.
                let expected = self.expected_collection_elem.take();
                let keys: Vec<&AIRNode> = entries.iter().map(|e| &e.key).collect();
                let vals: Vec<&AIRNode> = entries.iter().map(|e| &e.value).collect();
                let (exp_key, exp_val) = match expected {
                    Some((k, v)) => (Some(k), v),
                    None => (None, None),
                };
                let key_ty = exp_key
                    .or_else(|| self.infer_homogeneous_elem_type_refs(&keys))
                    .unwrap_or_else(|| "interface{}".to_string());
                let val_ty = exp_val
                    .or_else(|| self.infer_homogeneous_elem_type_refs(&vals))
                    .unwrap_or_else(|| "interface{}".to_string());
                let _ = write!(self.buf, "map[{key_ty}]{val_ty}{{");
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
                // Go doesn't have sets; use map[T]struct{}. A declared `Set[T]`
                // binding type takes priority (empty set matches); otherwise
                // infer a homogeneous element type so `#{1, 2}` emits
                // `map[int64]struct{}{...}`.
                let expected = self.expected_collection_elem.take();
                let elem_ty = expected
                    .map(|(e, _)| e)
                    .or_else(|| self.infer_homogeneous_elem_type(elems))
                    .unwrap_or_else(|| "interface{}".to_string());
                let _ = write!(self.buf, "map[{elem_ty}]struct{{}}{{");
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
                // Go has no tuples; a `(a, b)` value is a struct with numbered
                // fields — the SAME representation `type_to_go` gives a tuple
                // *type* (`struct{ Field0 T0; Field1 T1 }`) and the match
                // pattern reads (`.Field0`). Emitting a `[...]interface{}` array
                // here (the prior lowering) produced a value whose type did not
                // match the `struct{…}` parameter type, so a tuple argument
                // failed `go build`. Build the matching struct literal instead,
                // inferring each field's element type (falling back to
                // `interface{}` when it can't be inferred).
                // A declared tuple return / binding type (`-> (Int, Int)` →
                // `struct{ Field0 int64; Field1 int64 }`) pins each field's Go
                // type, so a `return (count, total)` whose elements only infer to
                // `interface{}` (an untyped `let` binding) still emits the
                // concrete struct the declared return type demands. Per field:
                // the expected field type wins, else structural inference, else
                // `interface{}`.
                let expected_fields = self
                    .current_expected_type
                    .as_deref()
                    .map(Self::parse_tuple_struct_field_types)
                    .unwrap_or_default();
                let field_types: Vec<String> = elems
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        expected_fields
                            .get(i)
                            .cloned()
                            .or_else(|| self.infer_go_expr_type(e))
                            .unwrap_or_else(|| "interface{}".to_string())
                    })
                    .collect();
                let fields: Vec<String> = field_types
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("Field{i} {t}"))
                    .collect();
                let _ = write!(self.buf, "struct{{ {} }}{{", fields.join("; "));
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
                            // This literal lands inside a `fmt.Sprintf` FORMAT
                            // string, so a literal `%` must be doubled to `%%` —
                            // unescaped it pairs with the following bytes as a
                            // verb (`"${n}% pass"` → format `"%v% pass"` →
                            // `95%!p(MISSING)ass`), a SILENT cross-target output
                            // divergence: the build stays green and only Go
                            // corrupts (Q-go-percent-interpolation).
                            // `escape_go_string` never emits `%`, so escaping
                            // before doubling cannot double an escape.
                            self.buf.push_str(&escape_go_string(s).replace('%', "%%"));
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
                // Construct the tagged Result-runtime struct (`__bockOk`/
                // `__bockErr`) — the same shape the surface `Ok(..)`/`Err(..)`
                // construction emits and the `Result` match reads on `.tag`. The
                // old Go-idiomatic `v, nil` / `nil, err` multi-return shape
                // disagreed with the tag-dispatched match, so reconcile on the
                // tagged struct.
                let ctor = match variant {
                    ResultVariant::Ok => "__bockOk",
                    ResultVariant::Err => "__bockErr",
                };
                let _ = write!(self.buf, "{ctor}(");
                if let Some(v) = value {
                    // Box a numeric-literal payload at its concrete Go type (see
                    // `box_payload_str`) so a later `.(int64)` / generic `.(T)`
                    // payload assertion does not panic on the `int` default.
                    if let Some(go_ty) = Self::numeric_literal_go_type(v) {
                        let _ = write!(self.buf, "{go_ty}(");
                        self.emit_expr(v)?;
                        self.buf.push(')');
                    } else {
                        self.emit_expr(v)?;
                    }
                } else {
                    self.buf.push_str("nil");
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
                // If in expression position: Go doesn't have ternary; emit as
                // IIFE. Type it with the binding's expected type when known (a
                // `let x: T = if …`), else the enclosing function's return type
                // (`func() Ordering { … }`) so a named/concrete result is
                // assignable; the `else` falls back to a typed zero only for the
                // untyped form (a concrete return type always has both branches
                // in a Bock `if`-expression).
                let iife_ty = self
                    .expected_iife_type()
                    .unwrap_or_else(|| "interface{}".to_string());
                // Each branch of an `if`-expression produces the SAME type as the
                // whole `if`, so a nested `if`/`match` in a branch tail must
                // inherit this IIFE's concrete type — otherwise it falls back to
                // the enclosing fn's return type (e.g. a nested
                // `func() __bockResult` inside an `else` of a `MessageType` `if`).
                // Propagate the concrete type into the branch bodies (a nested
                // `let` save/restores it, so it never wrongly overrides a binding
                // type); the `interface{}` fallback is left absent so the branches
                // keep inferring their own types.
                let prev_expected = self.current_expected_type.take();
                let branch_expected = (iife_ty != "interface{}").then(|| iife_ty.clone());
                // A non-`interface{}` IIFE type is the typed-zero source for a
                // void-call branch tail (`emit_arm_body_return`).
                let iife_ret = (iife_ty != "interface{}").then(|| iife_ty.clone());
                let _ = write!(self.buf, "func() {iife_ty} {{ if ");
                self.emit_expr(condition)?;
                self.buf.push_str(" { ");
                self.current_expected_type = branch_expected.clone();
                self.emit_arm_body_return(then_block, iife_ret.as_deref())?;
                self.buf.push_str(" } else { ");
                if let Some(eb) = else_block {
                    self.current_expected_type = branch_expected;
                    self.emit_arm_body_return(eb, iife_ret.as_deref())?;
                } else {
                    self.buf.push_str("return nil");
                }
                self.buf.push_str(" } }()");
                self.current_expected_type = prev_expected;
                Ok(())
            }
            NodeKind::Block { stmts, tail } => {
                if stmts.is_empty() {
                    if let Some(t) = tail {
                        return self.emit_expr(t);
                    }
                }
                // A block with statements in expression position (e.g. an
                // `if`/`match` arm body `{ let id = ...; GetUser { id: id } }`)
                // lowers to an IIFE that runs the statements then returns the
                // tail. The earlier `func() interface{} { return <tail> }` form
                // both DROPPED the statements (so a `let` binding used by the tail
                // was `undefined`) and erased the result to `interface{}` (so a
                // variant struct returned as the tail was not assignable to the
                // enclosing sealed-interface IIFE type, e.g. `Route`). Type the
                // IIFE with the expected type — the binding's `current_expected_type`
                // or the enclosing fn's return type (`expected_iife_type`) — and
                // emit the full block body via `emit_block_body_return`, which runs
                // the statements and returns the (typed) tail. A typed IIFE closes
                // with `panic("unreachable")` (it always returns through the tail);
                // the untyped form keeps `return nil`.
                let iife_ret = self.expected_iife_type();
                let iife_ty = iife_ret.as_deref().unwrap_or("interface{}");
                // Consume the expected type for THIS IIFE's return only; the block
                // body re-types its own tail (and may re-set it via a nested
                // `let`), so it must not leak into the statements.
                let prev_expected = self.current_expected_type.take();
                // The body's tail terminates the IIFE when it is a value
                // expression (emitted as `return <tail>`) or a terminating
                // statement (`return`/`break`/`continue`). Only when the block can
                // fall through (no tail, or an assignment tail) do we need the
                // trailing fallback — emitting it unconditionally would leave dead
                // code after a `return`. (Go does not reject unreachable code, but
                // the conditional keeps the output clean.)
                let body_falls_through = match tail {
                    Some(t) => crate::generator::node_is_statement(t) && !Self::tail_terminates(t),
                    None => true,
                };
                let _ = writeln!(self.buf, "func() {iife_ty} {{");
                self.indent += 1;
                self.emit_block_body_return(node)?;
                if body_falls_through {
                    if iife_ty == "interface{}" {
                        self.writeln("return nil");
                    } else {
                        self.writeln("panic(\"unreachable\")");
                    }
                }
                self.indent -= 1;
                self.write_indent();
                self.buf.push_str("}()");
                self.current_expected_type = prev_expected;
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => {
                // Guards, or-/tuple/list/range patterns, and nested
                // constructor/record patterns cannot ride the value/type
                // `switch` IIFE below (its `case <cond>` form has no slot for a
                // relational/length test or a fall-through guard — they collapse
                // to a broken `case interface{}:`). Route them to the shared
                // if/else-if chain, wrapped in a typed IIFE whose arm bodies
                // `return` the match's value.
                //
                // This `match_needs_ifchain` check comes BEFORE the Optional/Result
                // tag-switch fast-paths (mirroring the statement-position
                // `emit_match`, which checks it first): a *nested* Optional/Result
                // pattern (`Some(Ok(n))`) is structured, so the tag-switch
                // `emit_optional_match_expr` cannot bind its nested payload
                // (`undefined: n`). The if-chain's `collect_binds_go` recurses
                // through the nested `Some`/`Ok` tags and binds `n` off the typed
                // `.v` payloads. A *flat* `Some(x)`/`Ok(v)` match is not structured,
                // so it still takes the tag-switch fast-path below.
                //
                // Two further go.rs-local diversions (NOT in the shared
                // `match_needs_ifchain`, which keeps these on the switch fast-path —
                // correct only for *statement* position, where `emit_match` binds
                // `__v`): a bare-bind arm (`x => …`, `mut x => …`) has no value to
                // switch on, so the value-switch IIFE emits `case interface{}:` (a
                // *type* in expression position) and drops the name; and a *plain*
                // record arm (`Point { x, .. }`) is a concrete struct, not a
                // sealed-interface value, so `case Point:` is likewise invalid. The
                // if-chain IIFE tests/binds every pattern kind (a bare bind → an
                // unconditional `else` with `x := root`; a plain record → field
                // reads off `root.X`).
                if crate::generator::match_needs_ifchain(arms)
                    || Self::go_value_match_has_bind_arm(arms)
                    || self.go_value_match_has_plain_record_arm(arms)
                {
                    let iife_ret = self.expected_iife_type();
                    let iife_ty = iife_ret.as_deref().unwrap_or("interface{}");
                    // Each arm yields the SAME type as the whole match, so a
                    // nested branchy tail inherits the concrete IIFE type; a
                    // nested `let` save/restores it. Mirrors the switch path.
                    let prev_expected = self.current_expected_type.take();
                    self.current_expected_type =
                        (iife_ty != "interface{}").then(|| iife_ty.to_string());
                    let _ = writeln!(self.buf, "func() {iife_ty} {{");
                    self.indent += 1;
                    let res =
                        self.emit_match_ifchain_inner(scrutinee, arms, /*emit_return=*/ true);
                    self.indent -= 1;
                    self.current_expected_type = prev_expected;
                    res?;
                    // A Bock match is exhaustive, but Go cannot prove the if-chain
                    // is total, so a trailing `panic` keeps the IIFE well-typed.
                    self.write_indent();
                    self.buf.push_str("panic(\"unreachable\")\n");
                    self.write_indent();
                    self.buf.push_str("}()");
                    return Ok(());
                }
                // Flat `Optional` / `Result` matches dispatch on the runtime tag
                // (`Some(x)`/`None`, `Ok(v)`/`Err(e)`). A *nested* such match
                // (`Some(Ok(n))`) was diverted to the if-chain above, so only the
                // single-level tag-switch reaches here.
                if go_match_is_optional(arms) {
                    return self.emit_optional_match_expr(scrutinee, arms);
                }
                if go_match_is_result(arms) {
                    return self.emit_result_match_expr(scrutinee, arms);
                }
                // A user-enum match (including a reachable `core.compare.Ordering`
                // enum) dispatches on the dynamic concrete-variant *type*
                // (`OrderingGreater`), so the IIFE must be a *type-switch* — the
                // variant names are Go struct types, not values, so a value-switch
                // (`case OrderingGreater:`) is a compile error. (The prelude
                // `__bockOrdering` value-enum, used when the real enum is NOT
                // reachable, stays a value-switch via the path below.)
                let is_user_enum = self.go_match_is_user_enum(arms);
                // Type the IIFE so its result is assignable where a
                // concrete/named type is required — `interface{}` does not
                // satisfy a named interface like the user `Ordering`. Prefer the
                // binding's *expected* type (`let x: T = match …`) when known and
                // concrete: a value-position match binds into `T`, which need not
                // equal the enclosing function's return type. Otherwise fall back
                // to the function's return type (the return-position case:
                // `return match …`), preserving the working Optional/Result/enum
                // behavior. A typed IIFE closes with `panic("unreachable")` (a
                // Bock match is exhaustive) rather than `return nil`, which has no
                // value for a concrete return type.
                let iife_ret = self.expected_iife_type();
                let iife_ty = iife_ret.as_deref().unwrap_or("interface{}");
                // Consume the expected type for THIS IIFE's return only; the
                // scrutinee is a different (matched) type and must not inherit it.
                // Each arm produces the SAME type as the whole match, so a nested
                // `if`/`match` in an arm tail DOES inherit the concrete IIFE type
                // (else it falls back to the enclosing fn's return type). A nested
                // `let` save/restores it, so it never wrongly overrides a binding.
                let prev_expected = self.current_expected_type.take();
                let arm_expected = (iife_ty != "interface{}").then(|| iife_ty.to_string());
                // A user-enum match whose arms extract payload fields
                // (`Heading { level, text } => …`) must narrow on `__v` so each
                // case can read `__v.Level` / `__v.Text`. A payload-less enum
                // match (the `core.compare.Ordering` unit variants) keeps the
                // non-binding `switch x.(type)` (binding `__v` would be Go's
                // "declared and not used").
                let user_enum_binds = is_user_enum && Self::go_user_enum_match_binds_payload(arms);
                let _ = write!(self.buf, "func() {iife_ty} {{ switch ");
                if user_enum_binds {
                    self.buf.push_str("__v := ");
                    self.emit_expr(scrutinee)?;
                    self.buf.push_str(".(type) { ");
                } else if is_user_enum {
                    // Non-binding type-switch (`switch x.(type)`): the
                    // `core.compare.Ordering` variants are unit (no payload), so
                    // no `__v` binding is needed, which also avoids Go's
                    // "declared and not used" on a payload-less match.
                    self.emit_expr(scrutinee)?;
                    self.buf.push_str(".(type) { ");
                } else {
                    self.emit_expr(scrutinee)?;
                    self.buf.push_str(" { ");
                }
                // Match in expression position: emit as IIFE with switch. Each
                // arm body is terminated with `;` so consecutive single-line
                // `case`/`default` clauses are separated — Go requires a
                // statement terminator between a `case` body's trailing `return`
                // and the next `case`/`default` keyword (a bare space is a
                // "unexpected keyword case" syntax error).
                for arm in arms {
                    if let NodeKind::MatchArm { pattern, body, .. } = &arm.kind {
                        if matches!(pattern.kind, NodeKind::WildcardPat) {
                            self.buf.push_str("default: ");
                        } else {
                            self.buf.push_str("case ");
                            self.emit_match_case_condition(pattern)?;
                            self.buf.push_str(": ");
                        }
                        // Bind the narrowed `__v`'s payload fields for this arm
                        // (`level := __v.Level; _ = level`) before the body.
                        if user_enum_binds {
                            self.emit_user_enum_arm_bindings(pattern)?;
                        }
                        self.current_expected_type = arm_expected.clone();
                        self.emit_arm_body_return(body, iife_ret.as_deref())?;
                        self.buf.push_str("; ");
                    }
                }
                // `}; <fallthrough>` — the switch's closing brace and the IIFE's
                // fallthrough are two statements on one line, so they need an
                // explicit separator (a bare `} return` is a Go syntax error:
                // "unexpected keyword return at end of statement"). A typed IIFE
                // uses `panic` (no `nil` for a concrete type); the untyped form
                // keeps `return nil`.
                if iife_ret.is_some() {
                    self.buf.push_str("}; panic(\"unreachable\") }()");
                } else {
                    self.buf.push_str("}; return nil }()");
                }
                self.current_expected_type = prev_expected;
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
            // An expression-position `loop` (`let r = loop { … break <v> }`):
            // Bock's `loop` is a value-producing expression terminated by a
            // `break <v>`. Go's `for` carries no value, so lower it to an IIFE
            // `func() T { for { … } }()` whose body's `break <v>` becomes
            // `return <v>` (handled via `loop_expr_depth` in the `Break` arm). The
            // loop is infinite — it only leaves through a `break <v>` — so the
            // trailing `panic("unreachable")` after the `for` is never reached but
            // satisfies Go's "missing return" check.
            NodeKind::Loop { body } => {
                let iife_ret = self.expected_iife_type();
                let iife_ty = iife_ret.as_deref().unwrap_or("interface{}");
                // The IIFE consumes the expected type for its own return; the loop
                // body re-types its own `break` values, so it must not leak into
                // the statements within.
                let prev_expected = self.current_expected_type.take();
                let _ = writeln!(self.buf, "func() {iife_ty} {{");
                self.indent += 1;
                self.loop_expr_depth += 1;
                self.emit_loop_label_prefix(body);
                self.writeln("for {");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
                self.loop_labels.pop();
                self.loop_expr_depth -= 1;
                if iife_ty == "interface{}" {
                    self.writeln("return nil");
                } else {
                    self.writeln("panic(\"unreachable\")");
                }
                self.indent -= 1;
                self.write_indent();
                self.buf.push_str("}()");
                self.current_expected_type = prev_expected;
                Ok(())
            }
            _ => {
                self.buf.push_str("/* unsupported */");
                Ok(())
            }
        }
    }

    // ── Match → switch/if-else ──────────────────────────────────────────────

    /// Push a loop scope, emitting a Go label before the loop iff a contained
    /// statement-arm `match` needs to `break`/`continue` the loop (Go's `break`
    /// otherwise exits the inner `switch`). Must be paired with a
    /// `self.loop_labels.pop()` after the loop body is emitted.
    fn emit_loop_label_prefix(&mut self, body: &AIRNode) {
        if go_loop_needs_label(body) {
            self.loop_label_counter += 1;
            let label = format!("__bockLoop{}", self.loop_label_counter);
            let ind = self.indent_str();
            // A Go label sits in column-0-ish; we keep current indent for
            // readability — gofmt would re-align but the program is valid.
            let _ = writeln!(self.buf, "{ind}{label}:");
            self.loop_labels.push(Some(label));
        } else {
            self.loop_labels.push(None);
        }
    }

    /// The label of the innermost loop, if one was allocated. Used by
    /// `break`/`continue` emitted inside a statement-arm `switch`.
    fn innermost_loop_label(&self) -> Option<&str> {
        self.loop_labels.last().and_then(|l| l.as_deref())
    }

    /// Emit an `Optional` `match` in expression position as an IIFE that
    /// dispatches on the runtime tag (`__bockOption.tag`). `Some(v)` binds
    /// `v` from `.v` (as `interface{}`); `None`/`_` is the fallthrough.
    fn emit_optional_match_expr(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        let elem = self.scrutinee_optional_elem(scrutinee);
        // Type the IIFE with the binding's *expected* type (a typed `let x: T =
        // match …` or a function-return `return match …`, both surfaced through
        // `current_expected_type`) when concrete, so the IIFE result is
        // assignable to the destination — a bare `interface{}` IIFE is not
        // assignable to a named `__bockOption` / `[]T` / `int64` / `T` return.
        // When no concrete expected type is in scope (e.g. the match is nested
        // in a string interpolation, whose `%v` consumes `interface{}`), fall
        // back to the untyped IIFE, preserving the existing behavior.
        let iife_ret = self.typed_match_iife_type();
        let iife_ty = iife_ret.as_deref().unwrap_or("interface{}");
        let prev_expected = self.current_expected_type.take();
        // A collection-typed IIFE (`func() []T { … }`) propagates its element
        // type to the arm bodies' collection literals, so `to_list`'s `[x]`/`[]`
        // arms emit `[]T{x}` / `[]T{}` rather than the `[]interface{}` default
        // (which is not assignable to the `[]T` return). Re-applied per arm
        // inside the arms emitter (a literal `take()`s the hint, so a single
        // top-level set would only reach the first arm).
        let iife_coll = iife_ret.as_deref().and_then(Self::rendered_collection_elem);
        let _ = write!(self.buf, "func() {iife_ty} {{ __opt := ");
        self.emit_expr(scrutinee)?;
        self.buf.push_str("; ");
        self.emit_optional_match_arms(
            arms,
            /*as_expr=*/ true,
            elem.as_deref(),
            iife_ret.is_some(),
            iife_coll.as_ref(),
            iife_ret.as_deref(),
        )?;
        self.buf.push_str(" }()");
        self.current_expected_type = prev_expected;
        Ok(())
    }

    /// Parse a rendered Go collection type string into its element Go type(s) for
    /// [`Self::expected_collection_elem`]: `[]int64` → `("int64", None)`,
    /// `map[string]int64` → `("string", Some("int64"))`. `None` for a
    /// non-collection rendering. Used to propagate a collection-typed IIFE
    /// return into the arm bodies' literals.
    fn rendered_collection_elem(ty: &str) -> Option<(String, Option<String>)> {
        let ty = ty.trim();
        if let Some(elem) = ty.strip_prefix("[]") {
            return Some((elem.to_string(), None));
        }
        if let Some(rest) = ty.strip_prefix("map[") {
            // Split on the matching close bracket of the key type (top-level).
            let mut depth = 0i32;
            for (i, ch) in rest.char_indices() {
                match ch {
                    '[' => depth += 1,
                    ']' if depth == 0 => {
                        let key = rest[..i].to_string();
                        let val = rest[i + 1..].to_string();
                        return Some((key, Some(val)));
                    }
                    ']' => depth -= 1,
                    _ => {}
                }
            }
        }
        None
    }

    /// The Go type a typed expression-position `Optional`/`Result` match IIFE
    /// should return: the binding's *expected* type ([`Self::current_expected_type`],
    /// set around a typed `let x: T = …` value emit or a function-return tail)
    /// when known and concrete. Unlike [`Self::expected_iife_type`] this does
    /// **not** fall back to the enclosing function's return type: an
    /// `Optional`/`Result` match nested in a non-return position (a string
    /// interpolation argument, say) must stay the untyped `interface{}` IIFE its
    /// `%v` consumer expects, never adopt the function's unrelated return type.
    /// `None` ⇒ the caller emits the `interface{}` fallback.
    fn typed_match_iife_type(&self) -> Option<String> {
        match self.current_expected_type.as_deref() {
            Some(t) if t != "interface{}" => Some(t.to_string()),
            _ => None,
        }
    }

    /// True when `node` is an *expression-position* `Optional`/`Result` match —
    /// a `match` over `Some`/`None` or `Ok`/`Err` whose arms are all
    /// value-producing (no statement arm). Such a match lowers to a typed IIFE
    /// (`func() __bockOption { … }()`) whose return must be assignable to the
    /// enclosing return/binding type; the callers use this to propagate the
    /// expected type into [`Self::current_expected_type`], mirroring the
    /// generic-record-construction case.
    fn is_expr_optional_or_result_match(node: &AIRNode) -> bool {
        if let NodeKind::Match { arms, .. } = &node.kind {
            !crate::generator::match_has_statement_arm(arms)
                && (go_match_is_optional(arms) || go_match_is_result(arms))
        } else {
            false
        }
    }

    /// Emit an `Optional` `match` in statement position as an if/else chain on
    /// the runtime tag.
    fn emit_optional_match_stmt(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        let elem = self.scrutinee_optional_elem(scrutinee);
        // Wrap the `__opt := …` binding + tag chain in a Go block so the scrutinee
        // temp is scoped to this match. Two sequential statement matches in one
        // function (`match a {…}` then `match b {…}`) otherwise both emit
        // `__opt := …` in the same scope → `no new variables on left side of :=`.
        let ind = self.indent_str();
        let _ = writeln!(self.buf, "{ind}{{");
        self.indent += 1;
        let ind2 = self.indent_str();
        let _ = write!(self.buf, "{ind2}__opt := ");
        self.emit_expr(scrutinee)?;
        self.buf.push('\n');
        self.write_indent();
        self.emit_optional_match_arms(
            arms,
            /*as_expr=*/ false,
            elem.as_deref(),
            false,
            None,
            None,
        )?;
        self.buf.push('\n');
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    /// Shared body for [`emit_optional_match_expr`] /
    /// [`emit_optional_match_stmt`]: an if/else chain on the option tag. In
    /// expression mode each arm body is `return`ed; in statement mode the arm
    /// body is emitted as a block.
    #[allow(clippy::too_many_arguments)]
    fn emit_optional_match_arms(
        &mut self,
        arms: &[AIRNode],
        as_expr: bool,
        some_elem_ty: Option<&str>,
        typed_iife: bool,
        iife_coll: Option<&(String, Option<String>)>,
        iife_ty: Option<&str>,
    ) -> Result<(), CodegenError> {
        // Save the caller's pending collection-element hint: the arm bodies
        // re-set it per-arm (`iife_coll`), so it must be restored on exit rather
        // than clobbered (a match-expr may itself be a collection-literal element).
        let saved_coll = self.expected_collection_elem.take();
        let mut first = true;
        let arm_count = arms.len();
        for (idx, arm) in arms.iter().enumerate() {
            let NodeKind::MatchArm { pattern, body, .. } = &arm.kind else {
                continue;
            };
            let is_last = idx + 1 == arm_count;
            // Determine the tag test and any bound name. The final arm is
            // rendered as a plain `else` so the if-chain is exhaustive from
            // Go\'s control-flow view (Bock matches are exhaustive). Its bound
            // name (e.g. the `Some(v)` value) is still extracted.
            // `bind` is the payload name (the `v` in `Some(v)`); `bind_is_payload`
            // is true only when it binds the `Some` payload (not a catch-all
            // binding of the whole option), so the `interface{}` payload type
            // assertion applies to exactly that case.
            let (cond, bind, bind_is_payload): (String, Option<String>, bool) = match &pattern.kind
            {
                NodeKind::ConstructorPat { path, fields } => {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    let bind = fields.first().map(|f| self.pattern_to_binding_name(f));
                    let is_payload = bind.is_some() && variant == "Some";
                    if is_last {
                        (String::new(), bind, is_payload)
                    } else {
                        (format!("__opt.tag == \"{variant}\""), bind, is_payload)
                    }
                }
                // Wildcard / bind pattern → catch-all.
                _ => (String::new(), None, false),
            };
            if first {
                first = false;
                if cond.is_empty() {
                    self.buf.push('{');
                } else {
                    let _ = write!(self.buf, "if {cond} {{");
                }
            } else if cond.is_empty() {
                self.buf.push_str(" else {");
            } else {
                let _ = write!(self.buf, " else if {cond} {{");
            }
            self.buf.push(' ');
            if let Some(name) = &bind {
                if name != "_" {
                    // The runtime stores the payload as `interface{}`. Assert it
                    // back to the concrete element type so typed use of the bound
                    // value (`x + 10`, a typed call) compiles. The element type
                    // comes from the scrutinee's `Optional[T]` (resolved
                    // structurally by the caller); when unknown, fall back to the
                    // bare `interface{}` payload — no regression, but typed use
                    // would not compile, which only happens if the scrutinee's
                    // element type is not structurally determinable.
                    match (bind_is_payload, some_elem_ty) {
                        // Numeric element types are recovered through the
                        // widening helpers rather than a hard `.(int64)` /
                        // `.(float64)` assertion: a payload constructed from an
                        // untyped Go constant (`Some(10)` → `__bockSome(10)`)
                        // boxes a Go `int`/`float64`, on which `.(int64)` panics.
                        (true, Some("int64")) => {
                            let _ =
                                write!(self.buf, "{name} := __bockAsInt64(__opt.v); _ = {name}; ");
                        }
                        (true, Some("float64")) => {
                            let _ = write!(
                                self.buf,
                                "{name} := __bockAsFloat64(__opt.v); _ = {name}; "
                            );
                        }
                        (true, Some(ty)) => {
                            let _ = write!(self.buf, "{name} := __opt.v.({ty}); _ = {name}; ");
                        }
                        _ => {
                            let _ = write!(self.buf, "{name} := __opt.v; _ = {name}; ");
                        }
                    }
                }
            }
            if as_expr {
                // Re-apply the IIFE's collection element per arm: a list/map/set
                // literal `take()`s the hint, so without re-setting it only the
                // first arm's literal would adopt the `[]T` element (`to_list`'s
                // `Some` arm), leaving the `None` arm's `[]` as `[]interface{}`.
                self.expected_collection_elem = iife_coll.cloned();
                // A void-call arm tail emits as a statement + discarded zero
                // (`return println(..)` is a Go arity error).
                self.emit_arm_body_return(body, iife_ty)?;
                self.buf.push(' ');
            } else {
                self.buf.push('\n');
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.write_indent();
            }
            self.buf.push('}');
        }
        self.expected_collection_elem = saved_coll;
        // Expression mode needs a trailing value if no arm matched. A `;`
        // separates it from the preceding `}` (Go requires a terminator). A
        // *typed* IIFE has no `nil` of its concrete return type, so it closes
        // with `panic` (a Bock match is exhaustive, so this is unreachable);
        // the untyped form keeps `return nil`.
        if as_expr {
            if typed_iife {
                self.buf.push_str("; panic(\"unreachable\")");
            } else {
                self.buf.push_str("; return nil");
            }
        }
        Ok(())
    }

    /// Emit a `Result` `match` in expression position as an IIFE that dispatches
    /// on the runtime tag (`__bockResult.tag`). Mirrors
    /// [`Self::emit_optional_match_expr`].
    fn emit_result_match_expr(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        let elems = self.scrutinee_result_elems(scrutinee);
        // Type the IIFE with the expected destination type when concrete (see
        // [`Self::emit_optional_match_expr`]); else the untyped fallback.
        let iife_ret = self.typed_match_iife_type();
        let iife_ty = iife_ret.as_deref().unwrap_or("interface{}");
        let prev_expected = self.current_expected_type.take();
        let iife_coll = iife_ret.as_deref().and_then(Self::rendered_collection_elem);
        let _ = write!(self.buf, "func() {iife_ty} {{ __res := ");
        self.emit_expr(scrutinee)?;
        self.buf.push_str("; ");
        self.emit_result_match_arms(
            arms,
            /*as_expr=*/ true,
            elems.as_ref(),
            iife_ret.is_some(),
            iife_coll.as_ref(),
            iife_ret.as_deref(),
        )?;
        self.buf.push_str(" }()");
        self.current_expected_type = prev_expected;
        Ok(())
    }

    /// Emit a `Result` `match` in statement position as an if/else chain on the
    /// runtime tag. Mirrors [`Self::emit_optional_match_stmt`].
    fn emit_result_match_stmt(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        let elems = self.scrutinee_result_elems(scrutinee);
        // Scope the `__res := …` temp to a Go block so sequential statement
        // matches in one function don't collide on `__res` (`no new variables on
        // left side of :=`). See `emit_optional_match_stmt`.
        let ind = self.indent_str();
        let _ = writeln!(self.buf, "{ind}{{");
        self.indent += 1;
        let ind2 = self.indent_str();
        let _ = write!(self.buf, "{ind2}__res := ");
        self.emit_expr(scrutinee)?;
        self.buf.push('\n');
        self.write_indent();
        self.emit_result_match_arms(
            arms,
            /*as_expr=*/ false,
            elems.as_ref(),
            false,
            None,
            None,
        )?;
        self.buf.push('\n');
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    /// Shared body for the `Result` match emitters: an if/else chain on the
    /// result tag. `Ok(v)` binds `v` from `__res.v` asserted to the Ok type;
    /// `Err(e)` binds `e` asserted to the Err type. Mirrors
    /// [`Self::emit_optional_match_arms`].
    #[allow(clippy::too_many_arguments)]
    fn emit_result_match_arms(
        &mut self,
        arms: &[AIRNode],
        as_expr: bool,
        elems: Option<&(String, String)>,
        typed_iife: bool,
        iife_coll: Option<&(String, Option<String>)>,
        iife_ty: Option<&str>,
    ) -> Result<(), CodegenError> {
        // See `emit_optional_match_arms`: preserve the caller's pending
        // collection-element hint across the per-arm re-application.
        let saved_coll = self.expected_collection_elem.take();
        let mut first = true;
        let arm_count = arms.len();
        for (idx, arm) in arms.iter().enumerate() {
            let NodeKind::MatchArm { pattern, body, .. } = &arm.kind else {
                continue;
            };
            let is_last = idx + 1 == arm_count;
            // `(cond, bind, variant)`: the final arm is a plain `else` (Bock
            // matches are exhaustive), but its payload bind is still extracted.
            let (cond, bind, variant): (String, Option<String>, Option<&str>) = match &pattern.kind
            {
                NodeKind::ConstructorPat { path, fields } => {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    let bind = fields.first().map(|f| self.pattern_to_binding_name(f));
                    if is_last {
                        (String::new(), bind, Some(variant))
                    } else {
                        (format!("__res.tag == \"{variant}\""), bind, Some(variant))
                    }
                }
                _ => (String::new(), None, None),
            };
            if first {
                first = false;
                if cond.is_empty() {
                    self.buf.push('{');
                } else {
                    let _ = write!(self.buf, "if {cond} {{");
                }
            } else if cond.is_empty() {
                self.buf.push_str(" else {");
            } else {
                let _ = write!(self.buf, " else if {cond} {{");
            }
            self.buf.push(' ');
            if let Some(name) = &bind {
                if name != "_" {
                    // Assert the `interface{}` payload to the concrete Ok/Err
                    // type. Numeric payloads from untyped Go constants are widened
                    // via the shared helpers (a hard `.(int64)` would panic, see
                    // `NUMERIC_RUNTIME_GO`). When the type is unknown, bind the
                    // bare `interface{}` payload (never wrong, only un-asserted).
                    let payload_ty = match (variant, elems) {
                        (Some("Ok"), Some((ok, _))) => Some(ok.as_str()),
                        (Some("Err"), Some((_, err))) => Some(err.as_str()),
                        _ => None,
                    };
                    match payload_ty {
                        Some("int64") => {
                            let _ =
                                write!(self.buf, "{name} := __bockAsInt64(__res.v); _ = {name}; ");
                        }
                        Some("float64") => {
                            let _ = write!(
                                self.buf,
                                "{name} := __bockAsFloat64(__res.v); _ = {name}; "
                            );
                        }
                        Some(ty) => {
                            let _ = write!(self.buf, "{name} := __res.v.({ty}); _ = {name}; ");
                        }
                        None => {
                            let _ = write!(self.buf, "{name} := __res.v; _ = {name}; ");
                        }
                    }
                }
            }
            if as_expr {
                // See `emit_optional_match_arms`: re-apply the IIFE collection
                // element per arm so every arm's literal adopts it, not just the
                // first.
                self.expected_collection_elem = iife_coll.cloned();
                // A void-call arm tail (`Err(e) => println(..)`) must emit the
                // call as a statement + discarded zero, not `return println(..)`
                // (a Go arity error: `fmt.Println` returns `(int, error)`).
                self.emit_arm_body_return(body, iife_ty)?;
                self.buf.push(' ');
            } else {
                self.buf.push('\n');
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.write_indent();
            }
            self.buf.push('}');
        }
        self.expected_collection_elem = saved_coll;
        if as_expr {
            if typed_iife {
                self.buf.push_str("; panic(\"unreachable\")");
            } else {
                self.buf.push_str("; return nil");
            }
        }
        Ok(())
    }

    fn emit_match(&mut self, scrutinee: &AIRNode, arms: &[AIRNode]) -> Result<(), CodegenError> {
        // Guards, or-patterns, tuple patterns, and nested constructor/record
        // patterns cannot be expressed by the value/type `switch` below (a
        // failed guard's `break` exits the switch; an or-pattern has no single
        // discriminant; a nested sub-pattern's bindings are lost). Lower those
        // to an if/else-if chain. This takes priority over the Optional/Result
        // fast-paths so e.g. `Some(Ok(v))` (an Optional-leaf match that is still
        // nested) routes here. Additive: everything else keeps its existing
        // switch / tag-chain lowering (see `match_needs_ifchain`).
        if crate::generator::match_needs_ifchain(arms) {
            return self.emit_match_ifchain(scrutinee, arms);
        }
        // A plain (non-enum-variant) record pattern with only bind/wildcard
        // fields (`Point { x, .. }`) is a concrete Go struct, not a
        // sealed-interface value: it has no type/value to `switch` on
        // (`switch __v.(type) { case Point: }` is invalid — `__v` is not an
        // interface). Route it to the if-chain, which reads each field directly
        // off `access.<Field>`. Mirrors the expression-position diversion. Does
        // not touch the shared `match_needs_ifchain`.
        if self.go_value_match_has_plain_record_arm(arms) {
            return self.emit_match_ifchain(scrutinee, arms);
        }
        // `Optional` / `Result` matches dispatch on the runtime tag, not a Go
        // type/value switch.
        if go_match_is_optional(arms) {
            return self.emit_optional_match_stmt(scrutinee, arms);
        }
        if go_match_is_result(arms) {
            return self.emit_result_match_stmt(scrutinee, arms);
        }
        // A user enum lowers to a type-switch over the sealed-interface concrete
        // variant structs, binding each arm's payload fields from `__v`.
        let user_enum = self.go_match_is_user_enum(arms);
        // The prelude `Ordering` is a `__bockOrdering` *value* enum (constants),
        // so its match is a value-switch (`switch o { case Less: }`), never the
        // type-switch user enums use — `Less` is a constant, not a Go type.
        let ordering = !user_enum && go_match_is_ordering(arms);
        // Choose value-switch (`switch v { case 5: }`) vs type-switch
        // (`switch v := s.(type) { case T: }`) by pattern kind: constructor /
        // record patterns dispatch on dynamic type; literal / bind patterns
        // dispatch on value. `Ordering` is forced to a value-switch.
        let type_switch = !ordering && (user_enum || go_match_is_type_switch(arms));
        // A value-switch arm may bind the whole scrutinee (`x => …`). The
        // scrutinee is bound into `__v` via the switch's init clause so the arm
        // can emit `x := __v` — without this the `default:` discarded the name
        // and the body referenced an undefined variable (the Go binding-drop
        // defect). Only needed for the value-switch path; the type-switches
        // already bind `__v`.
        let value_switch_binds = !user_enum
            && !type_switch
            && arms.iter().any(|arm| {
                matches!(
                    &arm.kind,
                    NodeKind::MatchArm { pattern, .. } if matches!(pattern.kind, NodeKind::BindPat { .. })
                )
            });
        // The user-enum type-switch binds `__v` only when something reads it:
        // an arm that extracts a payload field (`__v.Radius`) or the trailing
        // `default: panic(... __v)` added for a non-exhaustive (catch-all-free)
        // match. A payload-less, catch-all-bearing match (e.g. `match ord {
        // Greater => …  _ => {} }`) binds nothing — emit a non-binding
        // `switch s.(type)` so Go does not reject an unused `__v`. Mirrors the
        // expression-position IIFE path (see `emit_match` expr lowering).
        let user_enum_default_panic = user_enum && !go_match_has_default_arm(arms);
        let user_enum_binds_v =
            user_enum && (Self::go_user_enum_match_binds_payload(arms) || user_enum_default_panic);
        let ind = self.indent_str();
        if user_enum && !user_enum_binds_v {
            // Non-binding type-switch: no arm reads `__v` and no `__v`-consuming
            // default panic follows, so binding it would be "declared and not
            // used".
            let _ = write!(self.buf, "{ind}switch ");
            self.emit_expr(scrutinee)?;
            self.buf.push_str(".(type) {\n");
        } else if user_enum {
            // A *narrowing* type-switch: `switch __v := s.(type)` rebinds `__v`
            // to the concrete variant struct in each case, so the arm can read
            // its payload fields (`__v.Radius`). (The non-narrowing
            // `switch __v := s; __v.(type)` form does not give `__v` the
            // concrete type in the cases.)
            let _ = write!(self.buf, "{ind}switch __v := ");
            self.emit_expr(scrutinee)?;
            self.buf.push_str(".(type) {\n");
        } else if type_switch {
            let _ = write!(self.buf, "{ind}switch __v := ");
            self.emit_expr(scrutinee)?;
            self.buf.push_str("; __v.(type) {\n");
        } else if value_switch_binds {
            // `switch __v := <scrutinee>; __v { … }` — evaluate once, give the
            // value a name so a bind arm can alias it.
            let _ = write!(self.buf, "{ind}switch __v := ");
            self.emit_expr(scrutinee)?;
            self.buf.push_str("; __v {\n");
        } else {
            let _ = write!(self.buf, "{ind}switch ");
            self.emit_expr(scrutinee)?;
            self.buf.push_str(" {\n");
        }
        self.indent += 1;
        self.switch_label_depth += 1;
        for arm in arms {
            self.emit_match_arm(arm, user_enum, value_switch_binds)?;
        }
        // Bock matches are exhaustive, but Go can't prove a type-switch covers
        // every implementor of a sealed interface (nor a value-switch every
        // `__bockOrdering` constant), so a function that returns a value after
        // the switch would fail to compile ("missing return"). When no arm is a
        // catch-all (`_` / bind), add a `default: panic(...)` so the switch is
        // total from Go's control-flow view.
        if (user_enum || ordering) && !go_match_has_default_arm(arms) {
            let di = self.indent_str();
            if user_enum {
                self.needs_fmt_import = true;
                let _ = write!(
                    self.buf,
                    "{di}default:\n{di}\tpanic(fmt.Sprintf(\"unreachable match arm: %v\", __v))\n"
                );
            } else {
                // Value-switch (`Ordering`): the scrutinee is not bound to a
                // local, so panic with a static message.
                let _ = write!(
                    self.buf,
                    "{di}default:\n{di}\tpanic(\"unreachable match arm\")\n"
                );
            }
        }
        self.switch_label_depth -= 1;
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    // ── Match → if/else-if chain (guards, or-/tuple/nested patterns) ──────────

    /// Lower a `match` whose arms cannot be expressed by a value/type `switch`
    /// (see [`crate::generator::match_needs_ifchain`]) to an `if <test> {
    /// <binds>; <body> } else if …` chain.
    ///
    /// The scrutinee is evaluated once into `__match` (a typed local), so nested
    /// tests/binds read off a single stable, typed value. Each arm contributes
    /// one `if`/`else if`; an unguarded catch-all (or the final unguarded arm,
    /// since Bock matches are exhaustive) becomes the unconditional `else`. A
    /// chain not closed by an `else` gets a trailing `else { panic(...) }` so a
    /// value-returning function still compiles (Go cannot prove exhaustiveness).
    ///
    /// Unlike the `switch` lowering, a bare `break`/`continue` in an arm body
    /// targets the enclosing `for` directly (there is no switch to escape), so
    /// `switch_label_depth` is deliberately left untouched.
    ///
    /// Statement position: arm bodies run as statements (`emit_return = false`).
    /// The expression-position caller (`func() T { … }()`) passes
    /// `emit_return = true` so each arm body's tail becomes a `return`, yielding
    /// the match's value.
    fn emit_match_ifchain(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        self.emit_match_ifchain_inner(scrutinee, arms, /*emit_return=*/ false)
    }

    fn emit_match_ifchain_inner(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
        emit_return: bool,
    ) -> Result<(), CodegenError> {
        // Single-evaluation root. A bare identifier is already a stable, typed
        // reference (emit it through the normal expression path so its name
        // matches the rest of the program); anything else is hoisted into a
        // typed `__match` local. Either way, leave the cursor indented at the
        // chain's column so the leading `if` lines up.
        let ind = self.indent_str();
        let root: String = if matches!(scrutinee.kind, NodeKind::Identifier { .. }) {
            let r = self.expr_to_string(scrutinee)?;
            self.write_indent();
            r
        } else {
            let _ = write!(self.buf, "{ind}__match := ");
            self.emit_expr(scrutinee)?;
            self.buf.push('\n');
            self.write_indent();
            "__match".to_string()
        };

        // The scrutinee's declared type, cloned to an owned node so it survives
        // the `&mut self` bind emit below. Threads through the pattern lowering
        // so a *nested tuple* payload (`Some(Ok((a, b)))`) is asserted to its
        // concrete tuple struct rather than read off a bare `interface{}`.
        let decl_ty: Option<AIRNode> = self.scrutinee_decl_type_node(scrutinee).cloned();

        let arm_count = arms.len();
        let mut first = true;
        let mut closed = false;
        for (idx, arm) in arms.iter().enumerate() {
            let NodeKind::MatchArm {
                pattern,
                guard,
                body,
            } = &arm.kind
            else {
                continue;
            };
            let test = self.pattern_test_go(pattern, &root, decl_ty.as_ref());
            let is_catch_all = matches!(
                pattern.kind,
                NodeKind::WildcardPat | NodeKind::BindPat { .. }
            );
            let is_last = idx + 1 == arm_count;
            let unconditional = guard.is_none() && (is_catch_all || is_last);

            if unconditional {
                if first {
                    self.buf.push('{');
                } else {
                    self.buf.push_str(" else {");
                }
                closed = true;
            } else {
                let mut cond = if test.is_empty() {
                    "true".to_string()
                } else {
                    test
                };
                if let Some(g) = guard {
                    // The guard may reference the arm's pattern bindings; they
                    // are only introduced inside the arm body, so evaluate the
                    // guard in an anonymous func that re-introduces them. A
                    // failed guard then falls through to the next `else if` (the
                    // fall-through a `switch` could not express).
                    let g_str = self.expr_to_string(g)?;
                    let binds =
                        self.pattern_binds_to_string_go_typed(pattern, &root, decl_ty.as_ref());
                    let guard_test = if binds.is_empty() {
                        format!("({g_str})")
                    } else {
                        format!("func() bool {{ {binds}return ({g_str}) }}()")
                    };
                    if cond == "true" {
                        cond = guard_test;
                    } else {
                        cond = format!("{cond} && {guard_test}");
                    }
                }
                if first {
                    let _ = write!(self.buf, "if {cond} {{");
                } else {
                    let _ = write!(self.buf, " else if {cond} {{");
                }
            }
            first = false;
            self.buf.push('\n');
            self.indent += 1;
            self.pattern_binds_go_typed(pattern, &root, decl_ty.as_ref())?;
            if emit_return {
                self.emit_block_body_return(body)?;
            } else {
                self.emit_block_body(body)?;
            }
            self.indent -= 1;
            self.write_indent();
            self.buf.push('}');
        }
        // A chain with no unconditional arm (all guarded, or no catch-all) needs
        // a trailing panic so a value-returning function compiles and an
        // unmatched scrutinee fails loudly. Bock matches are exhaustive, so this
        // is only ever reached if a guard chain is non-total.
        if !closed && !first {
            self.buf.push_str(" else {\n");
            self.indent += 1;
            self.writeln("panic(\"non-exhaustive match\")");
            self.indent -= 1;
            self.write_indent();
            self.buf.push('}');
        }
        self.buf.push('\n');
        Ok(())
    }

    /// Lower `guard (let PAT = COND) else { … }` (Go has no `let-else`).
    ///
    /// The pattern's bindings must stay live for the rest of the enclosing block
    /// (the whole point of guard-let), and the else arm must diverge (Bock's
    /// guard semantics guarantee it — `return`/`break`/`continue`/`panic`). The
    /// prior boolean `if !(cond)` lowering both negated a non-bool discriminant
    /// (`!(__bockResult{…})` — a `go build` error) and dropped the bound names
    /// (`undefined: v`). This emits:
    ///
    /// ```text
    /// __guardN := <cond>
    /// if !(<discriminant test>) { <else, diverges> }
    /// <name> := <typed payload of __guardN>   // bindings live after the guard
    /// ```
    ///
    /// `Ok`/`Some` payloads are asserted to their concrete element type (recovered
    /// from the scrutinee's `Result`/`Optional` element), so typed downstream use
    /// (`compare(guess, target)`) compiles; other constructor / record / tuple
    /// patterns reuse the shared bind emitter. A bare-bind pattern (always
    /// matches) emits only the binding (the else is unreachable).
    fn emit_guard_let(
        &mut self,
        pat: &AIRNode,
        condition: &AIRNode,
        else_block: &AIRNode,
    ) -> Result<(), CodegenError> {
        let n = self.guard_counter;
        self.guard_counter += 1;
        let guard_tmp = format!("__guard{n}");

        // Evaluate the discriminant once into a typed local (`:=`), so the test
        // and the payload bindings read off one stable value.
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}{guard_tmp} := ");
        self.emit_expr(condition)?;
        self.buf.push('\n');

        // The else arm runs when the pattern does NOT match; it diverges.
        let test = self.pattern_test_go(pat, &guard_tmp, None);
        if !test.is_empty() {
            self.writeln(&format!("if !({test}) {{"));
            self.indent += 1;
            self.emit_block_body(else_block)?;
            self.indent -= 1;
            self.writeln("}");
        } else {
            // A bare-bind/wildcard pattern always matches; bind `__guard` itself
            // so it is not "declared and not used", and drop the dead else arm.
            self.writeln(&format!("_ = {guard_tmp}"));
        }

        // Introduce the pattern's bindings into the *enclosing* scope (live after
        // the guard). Constructor payloads get a concrete-typed assertion.
        self.emit_guard_let_binds(pat, condition, &guard_tmp)?;
        Ok(())
    }

    /// Emit the bindings a guard-let pattern introduces, into the current
    /// (enclosing) Go block scope. Reuses the concrete payload typing the
    /// Optional/Result match arms apply (`__bockAsInt64`, `.v.(string)`, …) for an
    /// `Ok`/`Some` payload bind; everything else falls back to the shared
    /// [`Self::pattern_binds_go`] emitter. Each emitted name is recorded in the
    /// block's declared-name frame so a later same-name `let` reassigns.
    fn emit_guard_let_binds(
        &mut self,
        pat: &AIRNode,
        condition: &AIRNode,
        access: &str,
    ) -> Result<(), CodegenError> {
        if let NodeKind::ConstructorPat { path, fields } = &pat.kind {
            let leaf = path.segments.last().map_or("", |s| s.name.as_str());
            // Optional/Result single-payload constructor: assert the boxed
            // `interface{}` payload to its concrete element type so typed use of
            // the bound value compiles.
            if matches!(leaf, "Some" | "Ok" | "Err") {
                if let Some(f) = fields.first() {
                    if let NodeKind::BindPat { name, .. } = &f.kind {
                        let bind = go_value_ident(&name.name);
                        if bind != "_" {
                            let elem_ty = match leaf {
                                "Some" => self.scrutinee_optional_elem(condition),
                                "Ok" => self.scrutinee_result_elems(condition).map(|(ok, _)| ok),
                                _ => self.scrutinee_result_elems(condition).map(|(_, e)| e),
                            };
                            let rhs = match elem_ty.as_deref() {
                                Some("int64") => format!("__bockAsInt64({access}.v)"),
                                Some("float64") => format!("__bockAsFloat64({access}.v)"),
                                Some(ty) => format!("{access}.v.({ty})"),
                                None => format!("{access}.v"),
                            };
                            self.writeln(&format!("{bind} := {rhs}"));
                            self.writeln(&format!("_ = {bind}"));
                            self.go_record_declared(&bind);
                            return Ok(());
                        }
                    }
                    // Nested payload pattern (`Ok(Ok(v))`, `Some((a, b))`): the
                    // payload is re-asserted to its runtime struct where needed
                    // and the shared emitter recurses.
                    let child = go_typed_access(f, &format!("{access}.v"));
                    return self.emit_guard_let_binds_generic(f, &child);
                }
                return Ok(());
            }
        }
        // User-enum / record / tuple / bind patterns: the shared bind emitter
        // already extracts payloads off the asserted variant struct.
        self.emit_guard_let_binds_generic(pat, access)
    }

    /// Fallback guard-let binder: emit `pat`'s bindings off `access` via the
    /// shared [`Self::pattern_binds_go`] machinery, then record each bound name in
    /// the current Go block frame so a later same-name `let` reassigns.
    fn emit_guard_let_binds_generic(
        &mut self,
        pat: &AIRNode,
        access: &str,
    ) -> Result<(), CodegenError> {
        self.pattern_binds_go(pat, access)?;
        let mut binds = String::new();
        self.collect_binds_go(pat, access, None, &mut binds);
        for stmt in binds.split("; ") {
            // Each emitted bind is `name := …`; record the `name`.
            if let Some(name) = stmt.split_whitespace().next() {
                if name != "_" {
                    self.go_record_declared(name);
                }
            }
        }
        Ok(())
    }

    /// Lower a `?` propagation operand (Go has no native `?`).
    ///
    /// Emits, at statement position, the unwrap-or-early-return prelude for
    /// `<inner>?`:
    ///
    /// ```text
    /// __tryN := <inner>
    /// if __tryN.tag == "Err" { return __tryN }     // Result: propagate the Err
    /// // or, for an Optional operand:
    /// if __tryN.tag == "None" { return __bockNone() }
    /// ```
    ///
    /// and returns the Go expression that reads the success payload
    /// (`__tryN.v` asserted to the concrete `Ok`/`Some` element type when known).
    /// The enclosing function returns a `__bockResult` / `__bockOption`, so an
    /// `Err` operand re-propagates directly (`return __tryN`) and a `None` operand
    /// returns `__bockNone()`. Bock's type checker guarantees the operand's
    /// container and error type are compatible with the enclosing return type, so
    /// no zero-value reconstruction of a *different* error type is needed.
    ///
    /// `?` is only reached in statement-adjacent positions in practice (`let x =
    /// e?`, a bare `e?` statement, a tail `e?`); a `?` nested inside a larger
    /// expression (`Ok(f()? + 1)`) is not hoisted by this path — the inner emit
    /// goes through the normal expression emitter. None of the v1 examples nest
    /// `?` that way.
    fn emit_try_unwrap(&mut self, inner: &AIRNode) -> Result<String, CodegenError> {
        let n = self.try_counter;
        self.try_counter += 1;
        let tmp = format!("__try{n}");

        // Evaluate the operand once.
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}{tmp} := ");
        self.emit_expr(inner)?;
        self.buf.push('\n');

        // Decide Result vs Optional. Both lower to a runtime-tag check, but the
        // failure tag (`Err` vs `None`) and propagation value differ. Preference:
        //   1. the operand's recoverable `Result`/`Optional` element type;
        //   2. otherwise the enclosing function's return container
        //      (`__bockResult` → Result, `__bockOption` → Optional);
        //   3. default to Result — the overwhelmingly common case, and
        //      `return __tryN` is always a valid `__bockResult` to propagate.
        let result_elems = self.scrutinee_result_elems(inner);
        let opt_elem = self.scrutinee_optional_elem(inner);
        let is_optional = if result_elems.is_some() {
            false
        } else if opt_elem.is_some() {
            true
        } else {
            self.current_fn_ret_type.as_deref() == Some("__bockOption")
        };
        if is_optional {
            // Optional operand: a `None` short-circuits to the enclosing fn's
            // `None`. `__bockNone` is the runtime `__bockOption` None value.
            self.writeln(&format!("if {tmp}.tag == \"None\" {{"));
            self.indent += 1;
            self.writeln("return __bockNone");
            self.indent -= 1;
            self.writeln("}");
            Ok(self.try_payload_access(&tmp, opt_elem.as_deref()))
        } else {
            self.writeln(&format!("if {tmp}.tag == \"Err\" {{"));
            self.indent += 1;
            self.writeln(&format!("return {tmp}"));
            self.indent -= 1;
            self.writeln("}");
            let ok_ty = result_elems.map(|(ok, _)| ok);
            Ok(self.try_payload_access(&tmp, ok_ty.as_deref()))
        }
    }

    /// The Go expression that reads a `?`-unwrapped payload from `tmp.v`, asserted
    /// to the concrete element type `elem` when known. Numeric payloads use the
    /// widening helpers (an untyped Go constant boxed as `int`/`float64` would
    /// panic on a hard `.(int64)` assertion); everything else uses a direct type
    /// assertion, falling back to the bare `interface{}` payload when the element
    /// type is not statically recoverable.
    fn try_payload_access(&self, tmp: &str, elem: Option<&str>) -> String {
        match elem {
            Some("int64") => format!("__bockAsInt64({tmp}.v)"),
            Some("float64") => format!("__bockAsFloat64({tmp}.v)"),
            // A `Void`/unit payload (`Result[Void, _]` → `Ok(())` boxes `nil`)
            // and the `interface{}` fallback both read the raw payload — a hard
            // `.(struct{})` / `.(interface{})` assertion on a boxed `nil` panics.
            Some("struct{}") | Some("interface{}") | Some("any") | None => format!("{tmp}.v"),
            Some(ty) => format!("{tmp}.v.({ty})"),
        }
    }

    /// The Go expression reading an Optional/Result *leaf* payload bind off the
    /// runtime container `access` (`access.v`), asserted to the concrete element
    /// `elem` when known. The pattern-match analogue of [`Self::try_payload_access`]
    /// (which takes a bare temp name): a `match v { Some(n) if (n > 0) => … }`
    /// binds `n := access.v.(int64)` so typed use of `n` compiles. Numeric
    /// payloads use the widening helpers (a boxed untyped Go constant would panic
    /// on a hard `.(int64)`); `Void`/unit/`interface{}` and the unknown-type
    /// fallback read the raw payload (a hard assertion on a boxed `nil` panics).
    fn payload_access_go(&self, access: &str, elem: Option<&str>) -> String {
        match elem {
            Some("int64") => format!("__bockAsInt64({access}.v)"),
            Some("float64") => format!("__bockAsFloat64({access}.v)"),
            Some("struct{}") | Some("interface{}") | Some("any") | None => {
                format!("{access}.v")
            }
            Some(ty) => format!("{access}.v.({ty})"),
        }
    }

    /// Peel one declared-type layer for an Optional/Result constructor tag,
    /// returning the inner type node carried by the matched payload: `Some`/`None`
    /// peel `Optional[T]` → `T`; `Ok` peel `Result[T, E]` → `T`; `Err` → `E`.
    /// Returns `None` when the declared type is unknown or does not match the
    /// tag's container (the payload then stays the runtime `interface{}` — never
    /// wrong, only un-asserted). The result is `cloned` so the borrow of
    /// `decl_ty` does not outlive the recursion step.
    fn peel_constructor_decl_ty(
        &self,
        leaf: &str,
        decl_ty: Option<&AIRNode>,
    ) -> Option<Box<AIRNode>> {
        let ty = decl_ty?;
        match leaf {
            "Some" | "None" => self
                .optional_inner_type_node(ty)
                .map(|n| Box::new(n.clone())),
            "Ok" => self
                .result_inner_type_nodes(ty)
                .map(|(ok, _)| Box::new(ok.clone())),
            "Err" => self
                .result_inner_type_nodes(ty)
                .and_then(|(_, err)| err)
                .map(|n| Box::new(n.clone())),
            _ => None,
        }
    }

    /// The Go access expression for the payload of an Optional/Result
    /// constructor pattern's sole field. `Some(Ok(…))` re-asserts the boxed
    /// `interface{}` payload to the inner container runtime struct
    /// (`.v.(__bockResult)`), via [`go_typed_access`]. A nested *tuple* payload
    /// (`Some(Ok((a, b)))`) instead asserts the payload to its concrete tuple
    /// struct (`.v.(struct{ Field0 int64; Field1 int64 })`) so the subsequent
    /// `.Field0`/`.Field1` reads type-check — without it the field access lands
    /// on a bare `interface{}` and fails `go build`. `child_ty` is the
    /// peeled declared type of the payload (the tuple type, when known).
    fn constructor_child_access_go(
        &self,
        child: &AIRNode,
        access: &str,
        child_ty: Option<&AIRNode>,
    ) -> String {
        if let NodeKind::TuplePat { .. } = &child.kind {
            if let Some(t) = child_ty {
                if let NodeKind::TypeTuple { .. } = &t.kind {
                    let struct_ty = self.type_to_go(t);
                    return format!("{access}.v.({struct_ty})");
                }
            }
        }
        go_typed_access(child, &format!("{access}.v"))
    }

    /// The per-field declared type nodes of a tuple pattern position, recovered
    /// from a declared `TypeTuple` of the expected arity. Returns a vector of
    /// `Option<&AIRNode>` (one per field, `None` where the declared type is
    /// unknown or not a matching tuple) so the caller can thread each field's
    /// type into the element sub-pattern.
    fn tuple_field_decl_tys<'a>(
        &self,
        decl_ty: Option<&'a AIRNode>,
        arity: usize,
    ) -> Vec<Option<&'a AIRNode>> {
        if let Some(t) = decl_ty {
            if let NodeKind::TypeTuple { elems } = &t.kind {
                if elems.len() == arity {
                    return elems.iter().map(Some).collect();
                }
            }
        }
        vec![None; arity]
    }

    /// Build the boolean test that selects `pat` against the Go expression
    /// `access` (a correctly-typed value at this pattern position). Returns the
    /// empty string for a pattern that always matches (wildcard / bare bind).
    ///
    /// `decl_ty`, when present, is the declared type-expression node of the
    /// value at this position (recovered from the match scrutinee's declared
    /// type and peeled as the recursion descends `Some`/`Ok`/`Err`). It is only
    /// needed to type-assert a *nested tuple* payload (`Some(Ok((a, b)))`) to its
    /// concrete tuple struct before reading `.Field0`; every other arm ignores
    /// it and passes `None` down (the value at that position is already concrete).
    fn pattern_test_go(&self, pat: &AIRNode, access: &str, decl_ty: Option<&AIRNode>) -> String {
        match &pat.kind {
            NodeKind::WildcardPat | NodeKind::BindPat { .. } => String::new(),
            NodeKind::LiteralPat { lit } => {
                format!("{access} == {}", go_literal(lit))
            }
            NodeKind::ConstructorPat { path, fields } => {
                let leaf = path.segments.last().map_or("", |s| s.name.as_str());
                // Optional / Result dispatch on the runtime `.tag`; the payload
                // is `<access>.v` (an `interface{}` the child must re-assert).
                if matches!(leaf, "Some" | "None" | "Ok" | "Err") {
                    let mut tests = vec![format!("{access}.tag == \"{leaf}\"")];
                    if let Some(f) = fields.first() {
                        // Peel one declared-type layer matching this tag so a
                        // nested tuple payload can be asserted to its struct.
                        let child_ty = self.peel_constructor_decl_ty(leaf, decl_ty);
                        let child =
                            self.constructor_child_access_go(f, access, child_ty.as_deref());
                        let sub = self.pattern_test_go(f, &child, child_ty.as_deref());
                        if !sub.is_empty() {
                            tests.push(sub);
                        }
                    }
                    return tests.join(" && ");
                }
                // User enum: a sealed-interface value; test via a comma-ok type
                // assertion to the concrete variant struct.
                let variant_ty = self.go_variant_struct(path);
                format!("func() bool {{ _, ok := {access}.({variant_ty}); return ok }}()")
            }
            NodeKind::RecordPat { path, fields, .. } => {
                if self.user_variant_for_path(path).is_some() {
                    let variant_ty = self.go_variant_struct(path);
                    // Field sub-tests would require binding the asserted struct;
                    // a struct-variant record pattern with nested field patterns
                    // is rare — test the variant type and let binds extract.
                    let _ = fields;
                    return format!(
                        "func() bool {{ _, ok := {access}.({variant_ty}); return ok }}()"
                    );
                }
                // A *plain* record (`Point { x: 0, y }`) is already the concrete
                // struct, so test its field sub-patterns directly off
                // `access.<Field>` (a literal field constrains the arm; a bind /
                // wildcard field adds no test). Without these tests every plain-
                // record arm matched unconditionally, so `Point { x: 0, y: 0 }`
                // shadowed every later `Point { … }` arm.
                let mut tests = Vec::new();
                for f in fields {
                    if let Some(p) = &f.pattern {
                        let go_field = to_pascal_case(&f.name.name);
                        let sub = self.pattern_test_go(p, &format!("{access}.{go_field}"), None);
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
                // The access is already the concrete tuple struct (the parent
                // `Some`/`Ok` peel asserted it). Per-field types come from the
                // declared tuple type when known.
                let field_tys = self.tuple_field_decl_tys(decl_ty, elems.len());
                let mut tests = Vec::new();
                for (i, e) in elems.iter().enumerate() {
                    let sub = self.pattern_test_go(
                        e,
                        &format!("{access}.Field{i}"),
                        field_tys.get(i).and_then(|t| *t),
                    );
                    if !sub.is_empty() {
                        tests.push(sub);
                    }
                }
                if tests.is_empty() {
                    String::new()
                } else {
                    tests.join(" && ")
                }
            }
            NodeKind::ListPat { elems, rest } => {
                // Bock lists are Go slices (`[]T`). `[a, b]` requires an exact
                // length; `[a, ..rest]` requires at least len(elems). Element
                // sub-patterns are tested positionally (`access[i]`); the rest
                // binds the tail slice and adds no test. Mirrors `pattern_test_js`.
                let n = elems.len();
                let len_test = if rest.is_some() {
                    format!("len({access}) >= {n}")
                } else {
                    format!("len({access}) == {n}")
                };
                let mut tests = vec![len_test];
                for (i, e) in elems.iter().enumerate() {
                    let sub = self.pattern_test_go(e, &format!("{access}[{i}]"), None);
                    if !sub.is_empty() {
                        tests.push(sub);
                    }
                }
                tests.join(" && ")
            }
            NodeKind::RangePat { lo, hi, inclusive } => {
                // `lo..hi` → `access >= lo && access < hi`; `lo..=hi` uses `<=`.
                // Mirrors `pattern_test_js`.
                let lo_s = range_bound_to_go(lo);
                let hi_s = range_bound_to_go(hi);
                let upper = if *inclusive { "<=" } else { "<" };
                format!("{access} >= {lo_s} && {access} {upper} {hi_s}")
            }
            NodeKind::OrPat { alternatives } => {
                let alts: Vec<String> = alternatives
                    .iter()
                    .map(|a| {
                        let t = self.pattern_test_go(a, access, decl_ty);
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

    /// Emit the `name := <access…>; _ = name` bindings introduced by `pat`,
    /// recursing into nested constructor / record / tuple sub-patterns. The
    /// trailing `_ = name` keeps an unused binding from failing `go build`.
    fn pattern_binds_go(&mut self, pat: &AIRNode, access: &str) -> Result<(), CodegenError> {
        self.pattern_binds_go_typed(pat, access, None)
    }

    /// As [`Self::pattern_binds_go`], but threading the scrutinee's declared type
    /// node so a nested tuple payload is bound off a struct-asserted access.
    fn pattern_binds_go_typed(
        &mut self,
        pat: &AIRNode,
        access: &str,
        decl_ty: Option<&AIRNode>,
    ) -> Result<(), CodegenError> {
        let binds = self.pattern_binds_to_string_go_typed(pat, access, decl_ty);
        if binds.is_empty() {
            return Ok(());
        }
        // `pattern_binds_to_string_go` emits each `name := …; _ = name; `
        // separated by `; `; split onto its own indented line for readability.
        for stmt in binds.split("; ") {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            self.writeln(stmt);
        }
        Ok(())
    }

    /// Collect `pat`'s bindings as a single-line string of `name := …; _ = name;
    /// ` statements. Shared by [`Self::pattern_binds_go`] (statement position)
    /// and the guard-evaluating anonymous func in [`Self::emit_match_ifchain`].
    /// Threads the scrutinee's declared type node so a nested tuple payload
    /// (`Some(Ok((a, b)))`) is bound off a struct-asserted access rather than a
    /// bare `interface{}`.
    fn pattern_binds_to_string_go_typed(
        &self,
        pat: &AIRNode,
        access: &str,
        decl_ty: Option<&AIRNode>,
    ) -> String {
        let mut out = String::new();
        self.collect_binds_go(pat, access, decl_ty, &mut out);
        out
    }

    fn collect_binds_go(
        &self,
        pat: &AIRNode,
        access: &str,
        decl_ty: Option<&AIRNode>,
        out: &mut String,
    ) {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => {
                let n = go_value_ident(&name.name);
                let _ = write!(out, "{n} := {access}; _ = {n}; ");
            }
            NodeKind::ConstructorPat { path, fields } => {
                let leaf = path.segments.last().map_or("", |s| s.name.as_str());
                if matches!(leaf, "Some" | "None" | "Ok" | "Err") {
                    if let Some(f) = fields.first() {
                        // Peel one declared-type layer for this tag so a nested
                        // tuple payload is asserted to its concrete struct before
                        // its `.Field0`/`.Field1` reads (mirrors `pattern_test_go`).
                        let child_ty = self.peel_constructor_decl_ty(leaf, decl_ty);
                        // A *leaf* payload bind (`Some(n)`, `Ok(v)`) with a known
                        // concrete element type asserts the boxed `interface{}`
                        // payload to that type, so typed use inside a guard or
                        // arm body (`n > 0`) type-checks — the same payload typing
                        // the guard-let binder and the tag-switch arms apply. A
                        // nested constructor/tuple/record payload re-asserts via
                        // `constructor_child_access_go` and recurses.
                        if let NodeKind::BindPat { name, .. } = &f.kind {
                            let n = go_value_ident(&name.name);
                            if n != "_" {
                                let elem = child_ty.as_deref().map(|t| self.type_to_go(t));
                                let rhs = self.payload_access_go(access, elem.as_deref());
                                let _ = write!(out, "{n} := {rhs}; _ = {n}; ");
                                return;
                            }
                        }
                        let child =
                            self.constructor_child_access_go(f, access, child_ty.as_deref());
                        self.collect_binds_go(f, &child, child_ty.as_deref(), out);
                    }
                } else {
                    // User-enum variant: bind payload fields off the asserted
                    // concrete struct.
                    let variant_ty = self.go_variant_struct(path);
                    for (i, f) in fields.iter().enumerate() {
                        let child = format!("{access}.({variant_ty}).Field{i}");
                        self.collect_binds_go(f, &child, None, out);
                    }
                }
            }
            NodeKind::RecordPat { path, fields, .. } => {
                // A registered enum *variant* record (`Shape::Rect { w, h }`) is a
                // sealed-interface value, so its fields are read off a concrete
                // type assertion (`access.(ShapeRect).W`). A *plain* record
                // (`Point { x, y }`) is already a concrete struct — asserting
                // `access.(Point)` is invalid Go ("p is not an interface"), so its
                // fields are read directly (`access.X`). Mirrors the
                // `user_variant_for_path` gate in `pattern_test_go`.
                let base = if self.user_variant_for_path(path).is_some() {
                    let variant_ty = self.go_variant_struct(path);
                    format!("{access}.({variant_ty})")
                } else {
                    access.to_string()
                };
                for f in fields {
                    let go_field = to_pascal_case(&f.name.name);
                    let child = format!("{base}.{go_field}");
                    match &f.pattern {
                        Some(p) => self.collect_binds_go(p, &child, None, out),
                        None => {
                            let n = to_camel_case(&f.name.name);
                            let _ = write!(out, "{n} := {child}; _ = {n}; ");
                        }
                    }
                }
            }
            NodeKind::TuplePat { elems } => {
                let field_tys = self.tuple_field_decl_tys(decl_ty, elems.len());
                for (i, e) in elems.iter().enumerate() {
                    self.collect_binds_go(
                        e,
                        &format!("{access}.Field{i}"),
                        field_tys.get(i).and_then(|t| *t),
                        out,
                    );
                }
            }
            NodeKind::ListPat { elems, rest } => {
                for (i, e) in elems.iter().enumerate() {
                    self.collect_binds_go(e, &format!("{access}[{i}]"), None, out);
                }
                // `..rest` binds the remaining elements as a tail slice
                // (`rest := access[n:]`); a bare `..` (RestPat) or absent rest
                // binds nothing. Mirrors `pattern_binds_js`.
                if let Some(r) = rest {
                    if let NodeKind::BindPat { name, .. } = &r.kind {
                        let nm = go_value_ident(&name.name);
                        let _ = write!(out, "{nm} := {access}[{}:]; _ = {nm}; ", elems.len());
                    }
                }
            }
            NodeKind::OrPat { alternatives } => {
                if let Some(first) = alternatives.first() {
                    self.collect_binds_go(first, access, decl_ty, out);
                }
            }
            _ => {}
        }
    }

    /// The Go struct type name for a user-enum variant path (`ShapeRect`), or the
    /// joined path as a fallback.
    fn go_variant_struct(&self, path: &bock_ast::TypePath) -> String {
        if let Some(info) = self.user_variant_for_path(path) {
            let variant = path.segments.last().map_or("", |s| s.name.as_str());
            format!("{}{variant}", info.enum_name)
        } else {
            path.segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("")
        }
    }

    fn emit_match_arm(
        &mut self,
        arm: &AIRNode,
        user_enum: bool,
        value_switch_binds: bool,
    ) -> Result<(), CodegenError> {
        if let NodeKind::MatchArm {
            pattern,
            guard,
            body,
        } = &arm.kind
        {
            let ind = self.indent_str();
            match &pattern.kind {
                NodeKind::WildcardPat | NodeKind::BindPat { .. } => {
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
            // For a user enum type-switch, bind the arm's payload fields from
            // the concrete `__v` (`radius := __v.Radius`, `w := __v.Field0`).
            if user_enum {
                self.emit_user_enum_arm_bindings(pattern)?;
            }
            // Value-switch bind arm (`x => …`): alias the named scrutinee `__v`
            // so the body's references resolve (the Go binding-drop fix).
            if value_switch_binds {
                if let NodeKind::BindPat { name, .. } = &pattern.kind {
                    let n = go_value_ident(&name.name);
                    self.writeln(&format!("{n} := __v; _ = {n}"));
                }
            }
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

    /// Bind a user-enum arm's payload fields from the type-switched `__v`.
    ///
    /// Inside a Go type-switch case `case ShapeCircle:`, `__v` has the concrete
    /// variant-struct type, so each bound field reads directly off it:
    /// - struct variant (`Circle { radius }`): `radius := __v.Radius`
    ///   (the struct field is `to_pascal_case` of the Bock field name).
    /// - tuple variant (`Rect(w, h)`): `w := __v.Field0; h := __v.Field1`.
    /// - unit variant: nothing to bind.
    ///
    /// Each binding is followed by `_ = name` so an arm that does not use every
    /// payload field still compiles (Go errors on an unused local).
    fn emit_user_enum_arm_bindings(&mut self, pattern: &AIRNode) -> Result<(), CodegenError> {
        match &pattern.kind {
            NodeKind::ConstructorPat { fields, .. } => {
                for (i, field) in fields.iter().enumerate() {
                    let name = self.pattern_to_binding_name(field);
                    if name == "_" {
                        continue;
                    }
                    self.writeln(&format!("{name} := __v.Field{i}; _ = {name}"));
                }
            }
            NodeKind::RecordPat { fields, .. } => {
                for f in fields {
                    let go_field = to_pascal_case(&f.name.name);
                    let bind = match &f.pattern {
                        Some(p) => self.pattern_to_binding_name(p),
                        // Shorthand `{ radius }` binds a variable named `radius`.
                        None => to_camel_case(&f.name.name),
                    };
                    if bind == "_" {
                        continue;
                    }
                    self.writeln(&format!("{bind} := __v.{go_field}; _ = {bind}"));
                }
            }
            _ => {}
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
                // A user enum variant is a `{enum}{variant}` struct type
                // (`ShapeRect`); fall back to the joined path otherwise.
                let variant_name = if let Some(info) = self.user_variant_for_path(path) {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    format!("{}{variant}", info.enum_name)
                } else {
                    path.segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("")
                };
                self.buf.push_str(&variant_name);
            }
            NodeKind::RecordPat { path, .. } => {
                let type_name = if let Some(info) = self.user_variant_for_path(path) {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    format!("{}{variant}", info.enum_name)
                } else {
                    path.segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(".")
                };
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

    /// If `name` is one of the three collection types, emit the concrete Go
    /// container type with its element/key/value types recovered from `args`
    /// (each mapped to Go via `arg_to_go`), rather than the `interface{}`-erased
    /// `map_type_name` fallback:
    /// - `List[T]`  → `[]T`
    /// - `Set[T]`   → `map[T]struct{}`
    /// - `Map[K,V]` → `map[K]V`
    ///
    /// A missing arg defaults to `interface{}` (e.g. a bare `List` with no type
    /// argument), preserving the prior erased behavior for the untyped case.
    /// Returns `None` for any non-collection type so callers fall through to the
    /// `Optional`/`Result` runtime-struct and generic-struct paths unchanged.
    fn collection_type_to_go<T>(
        &self,
        name: &str,
        args: &[T],
        arg_to_go: impl Fn(&T) -> String,
    ) -> Option<String> {
        let elem = |i: usize| {
            args.get(i)
                .map_or_else(|| "interface{}".to_string(), &arg_to_go)
        };
        match name {
            "List" => Some(format!("[]{}", elem(0))),
            "Set" => Some(format!("map[{}]struct{{}}", elem(0))),
            "Map" => Some(format!("map[{}]{}", elem(0), elem(1))),
            _ => None,
        }
    }

    fn type_to_go(&self, node: &AIRNode) -> String {
        match &node.kind {
            NodeKind::TypeNamed { path, args } => {
                // A non-generic `type X = …` alias renders as its underlying Go
                // type (`type ParseResult = Result[...]` → `__bockResult`), so a
                // value of the alias type is the runtime container a `match`
                // dispatches on. Resolved before the collection/runtime mapping so
                // an alias to `List[T]`/`Result`/`Optional` lowers correctly.
                if let Some(target) = self.resolve_type_alias(node) {
                    return self.type_to_go(target);
                }
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                // The three collection types are NOT erased to an `interface{}`
                // element: a declared `List[Int]` must emit `[]int64` (not
                // `[]interface{}`) so element arithmetic / iteration / typed
                // returns compile. Emit the concrete Go container recursively
                // over the type args, BEFORE the `map_type_name`
                // `is_mapped_runtime` fallback (which would erase them).
                if let Some(collection) =
                    self.collection_type_to_go(&name, args, |a| self.type_to_go(a))
                {
                    return collection;
                }
                let go_name = self.map_type_name(&name);
                // Runtime container types (`__bockOption`, `__bockResult`) carry
                // their payload as `interface{}`, not as a Go generic parameter;
                // never append `[T]` to such a mapped runtime type.
                let is_mapped_runtime = go_name != name;
                if args.is_empty() || is_mapped_runtime {
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
                // A `Fn(...) -> Void` lowers to a Go `func(...)` with NO result
                // type. Rendering the Void return as `struct{}` (its value type)
                // would make `func() struct{}`, which is a function that must
                // `return struct{}{}` — but a Void-returning closure body emits
                // no return, so the signatures would not match. Drop the result.
                if Self::is_void_type(ret) {
                    format!("func({})", param_strs.join(", "))
                } else {
                    format!("func({}) {}", param_strs.join(", "), self.type_to_go(ret))
                }
            }
            NodeKind::TypeOptional { inner } => {
                // `T?` lowers to the tagged Optional runtime struct, not a Go
                // pointer — pointers can\'t represent `Some(nil-able-T)` vs
                // `None`, and the match dispatches on the tag.
                let _ = inner;
                "__bockOption".to_string()
            }
            NodeKind::TypeSelf => self
                .go_self_subst
                .clone()
                .unwrap_or_else(|| "/* Self */".into()),
            _ => "interface{}".into(),
        }
    }

    fn map_type_name(&self, name: &str) -> String {
        match name {
            "Int" => "int64".into(),
            "Float" => "float64".into(),
            "Bool" => "bool".into(),
            "String" => "string".into(),
            // Bock `Char` is a Unicode scalar; Go's `rune` (`int32`). A char
            // literal `'A'` already emits a Go rune literal (`go_literal`), so a
            // `let c: Char` annotation must render `rune`, not the undefined
            // identifier `Char`.
            "Char" => "rune".into(),
            "Void" | "Unit" => "struct{}".into(),
            "List" => "[]interface{}".into(),
            "Map" => "map[string]interface{}".into(),
            "Set" => "map[interface{}]struct{}".into(),
            "Any" => "interface{}".into(),
            "Never" => "interface{}".into(),
            "Channel" => "*__bockChannel".into(),
            "Optional" => "__bockOption".into(),
            // `Result[T, E]` lowers to the tagged Result-runtime struct (the
            // `[T, E]` args are dropped — `is_mapped_runtime` in the callers
            // suppresses the generic suffix), mirroring `Optional`.
            "Result" => "__bockResult".into(),
            // §18.3.1 builtin time types: a `Duration` value lowers to a
            // signed-nanosecond `int64`, and an `Instant` to `time.Time`
            // (`time.Now()`). They are NOT user-defined types, so as annotations
            // (e.g. on a `Clock` handler's `now_monotonic() -> Instant` /
            // `sleep(duration: Duration)`) they must render their concrete Go
            // forms, not the undefined identifiers. (The `time` import is driven
            // by the value sites — `time.Now()` / `time.Since(...)` — that any
            // `Instant`-typed program also exercises.)
            "Duration" => "int64".into(),
            "Instant" => "time.Time".into(),
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
                // See `type_to_go`: emit concrete `[]T` / `map[K]V` /
                // `map[T]struct{}` for the three collection types rather than
                // erasing their element type to `interface{}`.
                if let Some(collection) =
                    self.collection_type_to_go(&name, args, |a| self.ast_type_to_go(a))
                {
                    return collection;
                }
                let go_name = self.map_type_name(&name);
                let is_mapped_runtime = go_name != name;
                if args.is_empty() || is_mapped_runtime {
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
                // `Fn(...) -> Void` → Go `func(...)` (no result type). See the
                // AIR `TypeFunction` arm in `type_to_go` for the rationale.
                if Self::ast_type_is_void(ret) {
                    format!("func({})", param_strs.join(", "))
                } else {
                    format!(
                        "func({}) {}",
                        param_strs.join(", "),
                        self.ast_type_to_go(ret)
                    )
                }
            }
            TypeExpr::Optional { inner, .. } => {
                let _ = inner;
                "__bockOption".to_string()
            }
            TypeExpr::SelfType { .. } => "/* Self */".into(),
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn emit_block_body(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.emit_block_body_inner(node, false)
    }

    /// Emit a `@test` function body (S7), lowering `expect(...)` assertion chains
    /// to Go `if <neg> { t.Errorf(...) }` guards and falling back to the normal
    /// statement emitter for any other statement. Sets `needs_reflect` when an
    /// equality assertion (`reflect.DeepEqual`) is emitted.
    fn emit_go_test_body(&mut self, body: &AIRNode) -> Result<(), CodegenError> {
        let stmts_and_tail: Vec<&AIRNode> = match &body.kind {
            NodeKind::Block { stmts, tail } => stmts.iter().chain(tail.as_deref()).collect(),
            _ => vec![body],
        };
        for stmt in stmts_and_tail {
            if let Some((assertion, actual, expected)) = crate::generator::classify_assertion(stmt)
            {
                let a = self.expr_to_string(actual)?;
                use crate::generator::TestAssertion as T;
                match assertion {
                    T::Equal => {
                        // Go `==` follows the constant-conversion rules (an
                        // untyped literal `3` compares equal to an `int64`), so it
                        // handles ints/floats/strings/bools and comparable structs
                        // (the `__bockOption`/`__bockResult` value runtimes) without
                        // the type-mismatch `reflect.DeepEqual(int64, int)` pitfall.
                        // Slice/map equality (uncommon in `@test`) would need
                        // `reflect.DeepEqual`; that is a known follow-up.
                        let e = match expected {
                            Some(e) => self.expr_to_string(e)?,
                            None => "nil".to_string(),
                        };
                        self.writeln(&format!("if ({a}) != ({e}) {{"));
                        self.indent += 1;
                        self.writeln(&format!("t.Errorf(\"expected %v, got %v\", {e}, {a})"));
                        self.indent -= 1;
                        self.writeln("}");
                    }
                    T::BeTrue => {
                        self.writeln(&format!("if !({a}) {{"));
                        self.indent += 1;
                        self.writeln("t.Errorf(\"expected true, got false\")");
                        self.indent -= 1;
                        self.writeln("}");
                    }
                    T::BeFalse => {
                        self.writeln(&format!("if {a} {{"));
                        self.indent += 1;
                        self.writeln("t.Errorf(\"expected false, got true\")");
                        self.indent -= 1;
                        self.writeln("}");
                    }
                    T::BeSome | T::BeNone | T::BeOk | T::BeErr => {
                        let tag = match assertion {
                            T::BeSome => "Some",
                            T::BeNone => "None",
                            T::BeOk => "Ok",
                            _ => "Err",
                        };
                        self.writeln(&format!("if ({a}).tag != \"{tag}\" {{"));
                        self.indent += 1;
                        self.writeln(&format!("t.Errorf(\"expected {tag}, got %v\", ({a}).tag)"));
                        self.indent -= 1;
                        self.writeln("}");
                    }
                }
            } else {
                self.emit_node(stmt)?;
            }
        }
        Ok(())
    }

    fn emit_block_body_return(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        self.emit_block_body_inner(node, true)
    }

    fn emit_block_body_inner(
        &mut self,
        node: &AIRNode,
        emit_return: bool,
    ) -> Result<(), CodegenError> {
        // Open a fresh Go block scope for shadowing-`let` tracking. The frame is
        // pre-seeded with any pending parameter names (function/method entry), so
        // a `let` shadowing a parameter — which is the *same* Go scope, the body
        // gets no extra brace — reassigns rather than re-declares. Popped on exit
        // regardless of which return path the body takes.
        let mut frame = HashSet::new();
        if let Some(seed) = self.pending_scope_seed.take() {
            frame.extend(seed);
        }
        self.go_declared_scopes.push(frame);
        let result = self.emit_block_body_inner_scoped(node, emit_return);
        self.go_declared_scopes.pop();
        result
    }

    /// Whether `name` is already declared in the innermost open Go block scope.
    /// A shadowing `let` of such a name must lower to a plain assignment (`=`),
    /// not a fresh `:=` / `var` declaration (which Go rejects as "no new
    /// variables on left side of :=").
    fn go_name_declared_in_block(&self, name: &str) -> bool {
        self.go_declared_scopes
            .last()
            .is_some_and(|frame| frame.contains(name))
    }

    /// Record `name` as declared in the innermost open Go block scope.
    fn go_record_declared(&mut self, name: &str) {
        if let Some(frame) = self.go_declared_scopes.last_mut() {
            frame.insert(name.to_string());
        }
    }

    fn emit_block_body_inner_scoped(
        &mut self,
        node: &AIRNode,
        emit_return: bool,
    ) -> Result<(), CodegenError> {
        if let NodeKind::Block { stmts, tail } = &node.kind {
            if stmts.is_empty() && tail.is_none() {
                self.writeln("// empty");
                return Ok(());
            }
            // Type the declare-only temps this block introduces (value-CF hoist).
            self.seed_decl_only_types(stmts);
            for (i, s) in stmts.iter().enumerate() {
                self.emit_node(s)?;
                // Go rejects a `let`-bound local never read (`declared and not
                // used`); Bock permits it (a binding kept for its call's side
                // effect, e.g. context-audit's `let payment = process(...)`). When
                // a simple `let x = …` binding's name is referenced nowhere in the
                // rest of this block, emit `_ = x` to satisfy Go without dropping
                // the side-effecting initializer. Restricted to a plain `BindPat`
                // (not `_`, tuple/record patterns, or a `:=`-from-`if let` form):
                // the conservative reference scan over the *remaining* siblings and
                // tail never silences a name that is actually used.
                if let NodeKind::LetBinding { pattern, .. } = &s.kind {
                    if let NodeKind::BindPat { name, .. } = &pattern.kind {
                        let go_name = go_value_ident(&name.name);
                        if go_name != "_" {
                            let used_after = stmts[i + 1..]
                                .iter()
                                .chain(tail.as_deref())
                                .any(|n| collect_used_idents(n).contains(&name.name));
                            if !used_after {
                                self.writeln(&format!("_ = {go_name}"));
                            }
                        }
                    }
                }
            }
            if let Some(t) = tail {
                // A statement tail (`return`/`break`/`continue`/assignment) is
                // emitted as a statement, never via `emit_expr` (which would
                // fall through to `/* unsupported */` for control-flow nodes).
                if crate::generator::node_is_statement(t) {
                    self.emit_node(t)?;
                    return Ok(());
                }
                // DQ18: an in-place `List` mutator (`push`/`append`) in *tail*
                // position (a single-statement loop body `{ acc.push(x) }`) is a
                // `Void` call, so it carries no value to return/emit — lower it to
                // Go's slice-growth assignment statement, not the value-less call
                // form `emit_expr` would otherwise emit.
                if let NodeKind::Call { callee, args, .. } = &t.kind {
                    if self.try_emit_list_mutating_stmt(t, callee, args)? {
                        return Ok(());
                    }
                }
                // A `match` with statement arms has no value; emit it in
                // statement position (a Go `switch`) rather than as an
                // expression IIFE, regardless of whether a return was wanted.
                if let NodeKind::Match { scrutinee, arms } = &t.kind {
                    if crate::generator::match_has_statement_arm(arms) {
                        self.emit_match(scrutinee, arms)?;
                        return Ok(());
                    }
                }
                let should_return = emit_return && !self.is_void_call(t);
                // A collection literal in *tail-return* position adopts the
                // function's return collection element type(s), mirroring the
                // explicit-`return` arm, so `fn single[T](x: T) -> List[T] { [x]
                // }` emits `[]T{x}` rather than `[]interface{}{x}`.
                let prev_expected = self.expected_collection_elem.take();
                if should_return
                    && matches!(
                        t.kind,
                        NodeKind::ListLiteral { .. }
                            | NodeKind::MapLiteral { .. }
                            | NodeKind::SetLiteral { .. }
                    )
                {
                    self.expected_collection_elem = self.current_fn_ret_collection_elem.clone();
                }
                // A generic-record construction in tail-return position adopts
                // the function's rendered return type as its expected type, so
                // `fn list_iter[T](xs: List[T]) -> ListIterator[T] {
                // ListIterator { xs: xs, cursor: 0 } }` emits `ListIterator[T]{
                // ... }` rather than the field-inference `[any]` fallback (Go
                // requires explicit type args on a generic struct literal).
                let prev_expected_type = self.current_expected_type.take();
                if should_return
                    && (matches!(
                        t.kind,
                        NodeKind::RecordConstruct { .. } | NodeKind::TupleLiteral { .. }
                    ) || Self::is_expr_optional_or_result_match(t))
                {
                    self.current_expected_type = self.current_fn_ret_type.clone();
                }
                // A lambda returned directly (`-> Fn(Int) -> Int { (x) => … }`)
                // takes its param/return types from the declared function type.
                let saved_lambda_hints = if should_return {
                    self.pin_return_lambda_types(t)
                } else {
                    (
                        self.expected_lambda_param_types.clone(),
                        self.forced_lambda_ret.clone(),
                    )
                };
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
                self.expected_lambda_param_types = saved_lambda_hints.0;
                self.forced_lambda_ret = saved_lambda_hints.1;
                self.expected_collection_elem = prev_expected;
                self.current_expected_type = prev_expected_type;
            }
        } else if crate::generator::node_is_statement(node) {
            // A bare statement body (`break`/`continue`/`return`/assignment):
            // emit through the statement path, never as an expression.
            self.emit_node(node)?;
        } else {
            // Single expression as body.
            if let NodeKind::Match { scrutinee, arms } = &node.kind {
                if crate::generator::match_has_statement_arm(arms) {
                    self.emit_match(scrutinee, arms)?;
                    return Ok(());
                }
            }
            let should_return = emit_return && !self.is_void_call(node);
            let prev_expected = self.expected_collection_elem.take();
            if should_return
                && matches!(
                    node.kind,
                    NodeKind::ListLiteral { .. }
                        | NodeKind::MapLiteral { .. }
                        | NodeKind::SetLiteral { .. }
                )
            {
                self.expected_collection_elem = self.current_fn_ret_collection_elem.clone();
            }
            // See the block-tail arm: a generic-record construction — or an
            // expression-position `Optional`/`Result` match — as the sole body
            // expression adopts the function's return type, so its IIFE result
            // is assignable to the declared return type.
            let prev_expected_type = self.current_expected_type.take();
            if should_return
                && (matches!(
                    node.kind,
                    NodeKind::RecordConstruct { .. } | NodeKind::TupleLiteral { .. }
                ) || Self::is_expr_optional_or_result_match(node))
            {
                self.current_expected_type = self.current_fn_ret_type.clone();
            }
            // A lambda body returned directly (`-> Fn(Int) -> Int { (x) => … }`,
            // or the single-expression form `compose(...) { (x) => f(g(x)) }`)
            // takes its param/return types from the declared function type.
            let saved_lambda_hints = if should_return {
                self.pin_return_lambda_types(node)
            } else {
                (
                    self.expected_lambda_param_types.clone(),
                    self.forced_lambda_ret.clone(),
                )
            };
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
            self.expected_lambda_param_types = saved_lambda_hints.0;
            self.forced_lambda_ret = saved_lambda_hints.1;
            self.expected_collection_elem = prev_expected;
            self.current_expected_type = prev_expected_type;
        }
        Ok(())
    }

    /// Whether a statement-position tail node *unconditionally terminates* its
    /// enclosing block — a `return`/`break`/`continue`. Used by the block
    /// expression-IIFE to decide whether a trailing fallback
    /// (`return nil` / `panic("unreachable")`) is reachable: when the tail
    /// terminates, the fallback would be dead code after a `return`. An
    /// assignment tail (the other statement-tail shape) does not terminate, so
    /// the fallback is still needed there.
    fn tail_terminates(node: &AIRNode) -> bool {
        matches!(
            node.kind,
            NodeKind::Return { .. } | NodeKind::Break { .. } | NodeKind::Continue
        )
    }

    /// Emit an `if`/`match` arm body as the value of its enclosing expression
    /// IIFE — i.e. as a `return <expr>`, but with a void-call tail handled
    /// correctly. A `println(...)` / void effect-op call returns `(int, error)`
    /// (Go's `fmt.Println`) or nothing, so `return println(...)` is a Go arity
    /// error (`too many return values have (int, error) want T`). When the body's
    /// effective tail is a void call, emit the call as a *statement* followed by
    /// `return <zero>` (the IIFE result is discarded — these arise only when the
    /// whole match/if is in statement position). Otherwise emit the normal
    /// `return <body>`. `iife_ty` is the enclosing IIFE's return type; a non-`None`
    /// value uses a typed zero, `None` uses `nil`.
    fn emit_arm_body_return(
        &mut self,
        body: &AIRNode,
        iife_ty: Option<&str>,
    ) -> Result<(), CodegenError> {
        // The effective tail is the block tail (when the body is a `{ ... }`
        // block) or the body itself (a bare expression arm).
        let tail = match &body.kind {
            NodeKind::Block { tail: Some(t), .. } => t.as_ref(),
            NodeKind::Block { tail: None, .. } => body, // emitted below as-is
            _ => body,
        };
        if self.is_void_call(tail) {
            // Emit any leading block statements, then the void call as a
            // statement, then a discarded zero return.
            if let NodeKind::Block { stmts, .. } = &body.kind {
                for s in stmts {
                    self.emit_node(s)?;
                    self.buf.push_str("; ");
                }
            }
            self.emit_expr(tail)?;
            self.buf.push_str("; return ");
            match iife_ty {
                Some(ty) => self.zero_value_for(ty),
                None => self.buf.push_str("nil"),
            }
            return Ok(());
        }
        self.buf.push_str("return ");
        self.emit_block_as_expr(body)
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
            // A bound value name, keyword-escaped. This is the single Go
            // value-binding funnel: params, `let` bindings, and the
            // scope-inference map keys all derive from it, so the escape lands
            // identically everywhere and never strips back to a bare keyword (the
            // outer callers no longer re-run `to_camel_case`, which would drop a
            // trailing escape `_`).
            NodeKind::BindPat { name, .. } => go_value_ident(&name.name),
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

/// Map a Bock `@test` function name to a valid Go test function name
/// (`TestXxx`), as `go test` requires (S7).
///
/// A leading `test`/`test_` is stripped so the conventional `test_add` does not
/// become the stuttering `TestTestAdd`; the remainder is PascalCased and
/// `Test`-prefixed. A name that is *only* `test` (no suffix) keeps a stable
/// disambiguating form (`TestTest` → `Test_` is invalid, so it becomes
/// `TestCase`). Empty input falls back to `TestCase`.
fn go_test_fn_name(bock_name: &str) -> String {
    let stripped = bock_name
        .strip_prefix("test_")
        .or_else(|| bock_name.strip_prefix("test"))
        .unwrap_or(bock_name);
    let pascal = to_pascal_case(stripped);
    if pascal.is_empty() {
        "TestCase".to_string()
    } else {
        format!("Test{pascal}")
    }
}

/// True for Bock's built-in Optional/Result constructors, which must be
/// emitted verbatim (PascalCase preserved) so generated Go code can match
/// the runtime prelude's `Some`/`None`/`Ok`/`Err` types.
fn is_prelude_ctor(s: &str) -> bool {
    matches!(s, "Some" | "None" | "Ok" | "Err")
}

/// Convert a Bock *value* identifier (a param, local binding, or private
/// free-function name) to its Go form: `camelCase`, then escaped against the Go
/// keyword set so a binding named e.g. `default`/`range`/`type` emits
/// `default_`/`range_`/`type_` rather than the illegal bare keyword. Apply at
/// every value declaration and reference site **and** the type-inference
/// scope-map keys, so the escaped name is used uniformly and the maps stay
/// aligned with the emitted name. Member/field/method names and exported
/// (PascalCased) names use bare casing — a keyword is legal as a Go field name,
/// and PascalCasing already lifts a name out of the lowercase keyword set.
/// See [`crate::generator::escape_target_keyword`].
fn go_value_ident(name: &str) -> String {
    crate::generator::escape_target_keyword(
        &to_camel_case(name),
        crate::generator::KeywordTarget::Go,
    )
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

/// Render a literal as a Go value expression — used by the if-chain match
/// lowering to compare a scrutinee against a literal pattern (`<access> == …`).
/// Render a `RangePat` bound (`lo`/`hi`) as a Go expression. Range bounds are
/// literals (`1..10`) or a const identifier (`MIN..MAX`); anything else falls
/// back to the wrapped literal/identifier text, or `0` for an unrecognised node.
/// Mirrors `range_bound_to_js`.
fn range_bound_to_go(node: &AIRNode) -> String {
    match &node.kind {
        NodeKind::LiteralPat { lit } => go_literal(lit),
        NodeKind::Literal { lit } => go_literal(lit),
        NodeKind::Identifier { name } => go_value_ident(&name.name),
        _ => "0".to_string(),
    }
}

fn go_literal(lit: &Literal) -> String {
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
        Literal::String(s) => format!("\"{}\"", escape_go_string(s)),
        Literal::Unit => "nil".to_string(),
    }
}

/// Wrap a raw `interface{}` access (a container's `.v` payload) with the type
/// assertion the *child* pattern needs to read it as a typed value. An Optional
/// / Result child re-asserts to its runtime struct so `.tag`/`.v` are reachable;
/// everything else (bind / wildcard / literal / tuple) reads the raw value.
fn go_typed_access(child: &AIRNode, raw_access: &str) -> String {
    if let NodeKind::ConstructorPat { path, .. } = &child.kind {
        let leaf = path.segments.last().map_or("", |s| s.name.as_str());
        match leaf {
            "Some" | "None" => return format!("{raw_access}.(__bockOption)"),
            "Ok" | "Err" => return format!("{raw_access}.(__bockResult)"),
            _ => {}
        }
    }
    raw_access.to_string()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{AirArg, AirMapEntry, AirRecordField};
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
    fn self_operand_trait_becomes_f_bounded_generic_interface() {
        // P2 item 4: a trait whose method takes a `Self` operand
        // (`compare(self, other: Self)`) is encoded as an F-bounded generic
        // interface so an impl `func (Key) Compare(Key)` can satisfy it and a
        // bound `[T: Comparable]` lowers to `[T Comparable[T]]`. The leading
        // `self` receiver is dropped (implicit in a Go interface method); `Self`
        // renders as the interface's `__Self` type param.
        let self_param = node(
            10,
            NodeKind::Param {
                pattern: Box::new(bind_pat(11, "self")),
                ty: None,
                default: None,
            },
        );
        let other_param = node(
            12,
            NodeKind::Param {
                pattern: Box::new(bind_pat(13, "other")),
                ty: Some(Box::new(node(14, NodeKind::TypeSelf))),
                default: None,
            },
        );
        let method = node(
            2,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("compare"),
                generic_params: vec![],
                params: vec![self_param, other_param],
                return_type: Some(Box::new(node(
                    20,
                    NodeKind::TypeNamed {
                        path: type_path(&["Bool"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(3, vec![], None)),
            },
        );
        let t = node(
            1,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Comparable"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![method],
            },
        );
        let out = gen(&module(vec![], vec![t]));
        assert!(
            out.contains("type Comparable[__Self any] interface {"),
            "self-operand trait should be an F-bounded generic interface, got: {out}"
        );
        assert!(
            out.contains("Compare(__Self)"),
            "the `self` receiver is dropped and `Self` renders as `__Self`, got: {out}"
        );
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

    /// Q-go-enum-return-boxing: a value-position `if` whose branches yield enum
    /// variants, returned where the declared type is the sealed enum interface,
    /// must lower to a `func() Shape { ... }()` closure (not `func() interface{}`),
    /// so the boxed variant struct is assignable to the `Shape` return.
    #[test]
    fn enum_variant_if_branches_box_into_sealed_interface() {
        // enum Shape { Circle, Square }  (both unit for brevity)
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
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                    node(
                        3,
                        NodeKind::EnumVariant {
                            name: ident("Square"),
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                ],
            },
        );
        // fn pick(big: Bool) -> Shape { if (big) { Circle } else { Square } }
        let if_expr = node(
            10,
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(id_node(11, "big")),
                then_block: Box::new(block(12, vec![], Some(id_node(13, "Circle")))),
                else_block: Some(Box::new(block(14, vec![], Some(id_node(15, "Square"))))),
            },
        );
        let f = node(
            20,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("pick"),
                generic_params: vec![],
                params: vec![typed_param_node(21, "big", "Bool")],
                return_type: Some(Box::new(node(
                    22,
                    NodeKind::TypeNamed {
                        path: type_path(&["Shape"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(23, vec![], Some(if_expr))),
            },
        );
        let out = gen(&module(vec![], vec![e, f]));
        assert!(
            out.contains("func() Shape {"),
            "the value-position if must be a `func() Shape` closure (boxed into \
             the sealed interface), not a bare-interface closure, got: {out}"
        );
        assert!(
            !out.contains("func() interface{} { if "),
            "the if-closure must not fall back to a bare interface, got: {out}"
        );
        assert!(
            out.contains("return ShapeCircle{}") && out.contains("return ShapeSquare{}"),
            "each branch returns its variant struct, got: {out}"
        );
    }

    /// Q-go-enum-return-boxing: an UNTYPED `let m = if (..) { Circle } else
    /// { Square }` infers the variant's owning enum, so the value closure is
    /// typed `func() Shape` rather than the enclosing fn's unrelated return type.
    #[test]
    fn untyped_let_if_over_variants_infers_enum_iife_type() {
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
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                    node(
                        3,
                        NodeKind::EnumVariant {
                            name: ident("Square"),
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                ],
            },
        );
        // fn run() -> Void { let m = if (true) { Circle } else { Square } }
        let let_binding = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "m")),
                ty: None,
                value: Box::new(node(
                    12,
                    NodeKind::If {
                        let_pattern: None,
                        condition: Box::new(bool_lit(13, true)),
                        then_block: Box::new(block(14, vec![], Some(id_node(15, "Circle")))),
                        else_block: Some(Box::new(block(16, vec![], Some(id_node(17, "Square"))))),
                    },
                )),
            },
        );
        let f = node(
            20,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("run"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(23, vec![let_binding], None)),
            },
        );
        let out = gen(&module(vec![], vec![e, f]));
        assert!(
            out.contains("m := func() Shape {"),
            "an untyped let over variant branches infers the `Shape` closure \
             type, got: {out}"
        );
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

    /// Q-clock-handler-routing: inside a `with Clock` function the §18.3.1 time
    /// builtins route through the in-scope `clock` handler — `Instant.now()` →
    /// `clock.NowMonotonic()`, `sleep(d)` → `clock.Sleep(d)`, and the derived
    /// `start.elapsed()` via `clock.NowMonotonic().Sub(start)` — NOT the inlined
    /// host primitives (`time.Now()` / `time.Sleep`).
    #[test]
    fn clock_time_ops_route_through_handler() {
        let out = gen(&module(vec![], vec![clock_timed_fn()]));
        assert!(out.contains("clock.NowMonotonic()"), "got: {out}");
        assert!(out.contains("clock.Sleep("), "got: {out}");
        assert!(
            !out.contains("time.Now()"),
            "host clock primitive leaked past the handler: {out}"
        );
        assert!(
            !out.contains("time.Sleep"),
            "host sleep primitive leaked past the handler: {out}"
        );
    }

    /// `Duration` / `Instant` used as type annotations must render their Go
    /// value representations (`int64` / `time.Time`), not the undefined
    /// identifiers, so a `Clock` handler impl compiles (Q-clock-handler-routing
    /// supporting fix).
    #[test]
    fn builtin_time_types_map_to_go() {
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("span"),
                generic_params: vec![],
                params: vec![typed_param_node(2, "d", "Duration")],
                return_type: Some(Box::new(node(
                    3,
                    NodeKind::TypeNamed {
                        path: type_path(&["Instant"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(10, vec![], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("d int64"), "Duration annotation: {out}");
        assert!(out.contains("time.Time"), "Instant annotation: {out}");
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
        // GAP-C: `Comparable` is a sealed-core trait with no user `impl` in this
        // module, so the bound lowers to Go's self-contained ordered constraint
        // `__bockOrdered` (there is no `Comparable` type in Go). A user-declared
        // `Comparable` trait would keep its name (see `use_core_compare` exec).
        assert!(
            out.contains("func Constrained[T __bockOrdered](value T) T {"),
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

    /// A guarded arm now lowers to the shared if/else-if chain: the arm's
    /// condition tests the *pattern* AND the *guard* (`x == 1 && (ok)`), so a
    /// failed guard falls through to the next arm — the fall-through the prior
    /// `case 1: if ok { … }` lowering could not express (its `break` exited the
    /// whole switch).
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
            out.contains("if x == 1 && (ok) {"),
            "guard should test pattern AND guard in an if-chain, got: {out}"
        );
        assert!(
            !out.contains("switch"),
            "a guarded match must not use a switch, got: {out}"
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
        // An untyped integer-literal binding is pinned to `int64` (Bock `Int`):
        // a bare `x := 42` would be Go's default `int`, which then fails to mix
        // with an `int64` value downstream. See the `pin_int64` path.
        assert!(out.contains("var x int64 = 42"), "got: {out}");
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

    /// Q-match-exprpos (P4): a value-position `let flag: Bool = match n { … }`
    /// inside a function returning `String`. The expression-position match IIFE
    /// must take its return type from the *binding's* declared type (`bool`), not
    /// the enclosing function's return type (`string`) — otherwise the IIFE
    /// (`func() string { … }()`) is not assignable to `var flag bool`. The fix
    /// records the declared `let` type as `current_expected_type` and prefers it
    /// for the IIFE return.
    #[test]
    fn expr_position_match_uses_binding_type_not_fn_ret() {
        let m = node(
            10,
            NodeKind::Match {
                scrutinee: Box::new(id_node(11, "n")),
                arms: vec![
                    node(
                        12,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(
                                13,
                                NodeKind::LiteralPat {
                                    lit: Literal::Int("0".into()),
                                },
                            )),
                            guard: None,
                            body: Box::new(block(14, vec![], Some(bool_lit(15, true)))),
                        },
                    ),
                    node(
                        16,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(17, NodeKind::WildcardPat)),
                            guard: None,
                            body: Box::new(block(18, vec![], Some(bool_lit(19, false)))),
                        },
                    ),
                ],
            },
        );
        let let_flag = node(
            20,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(21, "flag")),
                ty: Some(Box::new(node(
                    22,
                    NodeKind::TypeNamed {
                        path: type_path(&["Bool"]),
                        args: vec![],
                    },
                ))),
                value: Box::new(m),
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("decide"),
                generic_params: vec![],
                params: vec![typed_param_node(2, "n", "Int")],
                return_type: Some(Box::new(node(
                    3,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(4, vec![let_flag], Some(str_lit(5, "x")))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("var flag bool = func() bool {"),
            "IIFE must be typed with the binding type (bool), not the fn return (string), got: {out}"
        );
        assert!(
            !out.contains("func() string {"),
            "the match IIFE must not be typed with the function return type, got: {out}"
        );
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
        // The loop variable is *referenced* in the body, so it keeps its name.
        let stmt = node(
            1,
            NodeKind::For {
                pattern: Box::new(bind_pat(2, "item")),
                iterable: Box::new(id_node(3, "items")),
                body: Box::new(block(4, vec![id_node(5, "item")], None)),
            },
        );
        let out = gen(&module(vec![], vec![stmt]));
        assert!(out.contains("for _, item := range items {"), "got: {out}");
    }

    #[test]
    fn for_loop_unused_var_drops_to_for_range() {
        // An unused loop variable would make Go reject `for _, item := range`
        // ("declared and not used"); `for _, _ := range` is itself invalid ("no
        // new variables on left side of :="). The emitter drops both to the bare
        // `for range items` form.
        let stmt = node(
            1,
            NodeKind::For {
                pattern: Box::new(bind_pat(2, "item")),
                iterable: Box::new(id_node(3, "items")),
                body: Box::new(block(4, vec![], None)),
            },
        );
        let out = gen(&module(vec![], vec![stmt]));
        assert!(out.contains("for range items {"), "got: {out}");
        assert!(!out.contains("for _, item"), "got: {out}");
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

    /// Q-go-percent-interpolation: a literal `%` inside an interpolated string
    /// lands in a `fmt.Sprintf` FORMAT string and must double to `%%` — left
    /// single it pairs with the following bytes as a verb (`"${n}% pass"` →
    /// `95%!p(MISSING)ass`), a silent cross-target output divergence (the build
    /// stays green and only Go corrupts).
    #[test]
    fn interpolation_escapes_literal_percent() {
        let interp = node(
            1,
            NodeKind::Interpolation {
                parts: vec![
                    AirInterpolationPart::Expr(Box::new(id_node(2, "n"))),
                    AirInterpolationPart::Literal("% pass, 100%% raw".into()),
                ],
            },
        );
        let out = gen(&module(vec![], vec![interp]));
        assert!(
            out.contains(r#"fmt.Sprintf("%v%% pass, 100%%%% raw", n)"#),
            "literal % must be doubled in the Sprintf format string, got: {out}"
        );
    }

    /// Q-go-runtime-helper-shadowing: a parameter named after a public module
    /// fn (`lines` — the `core.string` helper shape) must be spelled as the
    /// LOCAL (`lines`) at every reference, not the PascalCased helper
    /// (`Lines`) — here in for-in iterable position, the dogfood repro.
    #[test]
    fn local_param_shadows_public_fn_rename() {
        let pub_lines = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("lines"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(11, vec![], None)),
            },
        );
        let loop_stmt = node(
            20,
            NodeKind::For {
                pattern: Box::new(bind_pat(21, "line")),
                iterable: Box::new(id_node(22, "lines")),
                body: Box::new(block(23, vec![id_node(24, "line")], None)),
            },
        );
        let count_fn = node(
            30,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("count"),
                generic_params: vec![],
                params: vec![param_node(31, "lines")],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(32, vec![loop_stmt], None)),
            },
        );
        let out = gen(&module(vec![], vec![pub_lines, count_fn]));
        assert!(
            out.contains("range lines {"),
            "an in-scope param must shadow the public-fn rename, got: {out}"
        );
        assert!(
            !out.contains("range Lines"),
            "the PascalCased helper must not be referenced for the local, got: {out}"
        );
    }

    /// Q-go-split-combinator-typing: the builtin String-method return table
    /// mirrors `try_emit_string_method`'s lowerings (`split` → `[]string`,
    /// transforms → `string`, …), gated on the checker's receiver-kind
    /// annotation so a same-named user method is never mistaken for the
    /// builtin.
    #[test]
    fn string_builtin_return_type_mirrors_lowering() {
        let build = |method: &str, tag: Option<&str>| {
            let object = id_node(5, "s");
            let callee = node(
                6,
                NodeKind::FieldAccess {
                    object: Box::new(object),
                    field: ident(method),
                },
            );
            // The lowerer clones the receiver into the leading self arg: same
            // NodeId as the field-access object (see `desugared_self_call`).
            let args = vec![
                AirArg {
                    label: None,
                    value: id_node(5, "s"),
                },
                AirArg {
                    label: None,
                    value: str_lit(7, ","),
                },
            ];
            let mut call = node(
                8,
                NodeKind::Call {
                    callee: Box::new(callee.clone()),
                    args: args.clone(),
                    type_args: vec![],
                },
            );
            if let Some(tag) = tag {
                call.metadata.insert(
                    bock_types::checker::RECV_KIND_META_KEY.to_string(),
                    bock_air::Value::String(tag.to_string()),
                );
            }
            (call, callee, args)
        };
        for (method, expect) in [
            ("split", Some("[]string")),
            ("trim", Some("string")),
            ("to_upper", Some("string")),
            ("len", Some("int64")),
            ("contains", Some("bool")),
            ("char_at", Some("__bockOption")),
            ("frobnicate", None),
        ] {
            let (call, callee, args) = build(method, Some("Primitive:String"));
            assert_eq!(
                GoEmitCtx::string_builtin_return_go_type(&call, &callee, &args).as_deref(),
                expect,
                "method {method}"
            );
        }
        // No checker annotation / non-String receiver → not the builtin.
        let (call, callee, args) = build("split", None);
        assert_eq!(
            GoEmitCtx::string_builtin_return_go_type(&call, &callee, &args),
            None
        );
        let (call, callee, args) = build("split", Some("User:Tokenizer"));
        assert_eq!(
            GoEmitCtx::string_builtin_return_go_type(&call, &callee, &args),
            None
        );
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
        // A homogeneous integer list literal now infers a concrete element
        // type (`[]int64`), not the erased `[]interface{}` — so element
        // arithmetic / typed iteration / typed returns compile (P3-α item 1b).
        assert!(out.contains("[]int64{1, 2, 3}"), "got: {out}");
    }

    /// A list literal with no concretely-inferable common element type (here a
    /// mixed int/string literal) falls back to the erased `[]interface{}` —
    /// never a wrong concrete type (P3-α item 1b).
    #[test]
    fn list_literal_mixed_falls_back_to_interface() {
        let l = node(
            1,
            NodeKind::ListLiteral {
                elems: vec![int_lit(2, "1"), str_lit(3, "x")],
            },
        );
        let out = gen(&module(vec![], vec![l]));
        assert!(out.contains("[]interface{}{1, \"x\"}"), "got: {out}");
    }

    /// An empty list literal cannot infer an element type, so it falls back to
    /// `[]interface{}` when emitted with no declared-type context.
    #[test]
    fn empty_list_literal_falls_back_to_interface() {
        let l = node(1, NodeKind::ListLiteral { elems: vec![] });
        let out = gen(&module(vec![], vec![l]));
        assert!(out.contains("[]interface{}{}"), "got: {out}");
    }

    /// A homogeneous map literal infers its key and value element types
    /// separately (`map[string]int64`), not the erased
    /// `map[interface{}]interface{}` (P3-α item 1b).
    #[test]
    fn map_literal_infers_key_and_value() {
        let entry = AirMapEntry {
            key: str_lit(2, "a"),
            value: int_lit(3, "1"),
        };
        let m = node(
            1,
            NodeKind::MapLiteral {
                entries: vec![entry],
            },
        );
        let out = gen(&module(vec![], vec![m]));
        assert!(out.contains("map[string]int64{\"a\": 1}"), "got: {out}");
    }

    /// A homogeneous set literal infers a concrete element type
    /// (`map[int64]struct{}`).
    #[test]
    fn set_literal_infers_elem() {
        let s = node(
            1,
            NodeKind::SetLiteral {
                elems: vec![int_lit(2, "1"), int_lit(3, "2")],
            },
        );
        let out = gen(&module(vec![], vec![s]));
        assert!(
            out.contains("map[int64]struct{}{1: {}, 2: {}}"),
            "got: {out}"
        );
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
        // `ResultConstruct` lowers to the tagged Result-runtime constructor
        // `__bockOk(..)` — the same shape the surface `Ok(..)` construction emits
        // and the `Result` match reads — reconciling construction with match. A
        // numeric-*literal* payload is boxed at its concrete Go type
        // (`int64(42)`), so the `interface{}` box's dynamic type is `int64`, not
        // the untyped-constant default `int` — a later `.(int64)` / generic
        // `.(T)` payload assertion would otherwise panic (`box_payload_str`).
        let rc = node(
            1,
            NodeKind::ResultConstruct {
                variant: ResultVariant::Ok,
                value: Some(Box::new(int_lit(2, "42"))),
            },
        );
        let out = gen(&module(vec![], vec![rc]));
        assert!(out.contains("__bockOk(int64(42))"), "got: {out}");
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
        // A string payload is *not* boxed — only numeric literals are cast.
        assert!(out.contains("__bockErr(\"failed\")"), "got: {out}");
    }

    #[test]
    fn numeric_literal_go_type_recognises_int_float_and_negation() {
        // Bare int / float literals carry their concrete Go boxing type; a unary
        // negation is transparent; a string / identifier is not numeric.
        assert_eq!(
            GoEmitCtx::numeric_literal_go_type(&int_lit(1, "7")),
            Some("int64")
        );
        let flt = node(
            2,
            NodeKind::Literal {
                lit: Literal::Float("1.5".into()),
            },
        );
        assert_eq!(GoEmitCtx::numeric_literal_go_type(&flt), Some("float64"));
        let neg = node(
            3,
            NodeKind::UnaryOp {
                op: UnaryOp::Neg,
                operand: Box::new(int_lit(4, "1")),
            },
        );
        assert_eq!(GoEmitCtx::numeric_literal_go_type(&neg), Some("int64"));
        assert_eq!(
            GoEmitCtx::numeric_literal_go_type(&str_lit(5, "x")),
            None,
            "a string literal is not a numeric payload"
        );
        assert_eq!(
            GoEmitCtx::numeric_literal_go_type(&id_node(6, "v")),
            None,
            "a variable reference is not a numeric literal"
        );
    }

    #[test]
    fn rendered_collection_elem_parses_slice_and_map() {
        // A collection-typed IIFE return propagates its element type(s) to the
        // arm-body literals; the rendering is parsed back from the Go type string.
        assert_eq!(
            GoEmitCtx::rendered_collection_elem("[]int64"),
            Some(("int64".to_string(), None))
        );
        assert_eq!(
            GoEmitCtx::rendered_collection_elem("map[string]int64"),
            Some(("string".to_string(), Some("int64".to_string())))
        );
        // A nested map value keeps its inner brackets balanced.
        assert_eq!(
            GoEmitCtx::rendered_collection_elem("map[string][]int64"),
            Some(("string".to_string(), Some("[]int64".to_string())))
        );
        // A non-collection type yields nothing (the IIFE stays scalar-typed).
        assert_eq!(GoEmitCtx::rendered_collection_elem("__bockOption"), None);
        assert_eq!(GoEmitCtx::rendered_collection_elem("int64"), None);
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
                        // Instance method leads with `self` (real lowering).
                        params: vec![param_node(4, "self")],
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
        assert!(
            out.contains("func (self *Counter) Increment()"),
            "got: {out}"
        );
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
                trait_args: vec![],
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
                        // Instance method leads with `self`; a no-`self` method is
                        // an associated function (emitted as a free function).
                        params: vec![param_node(7, "self")],
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
            out.contains("func (self *Point) Distance() float64 {"),
            "got: {out}"
        );
    }

    /// A plain inherent `impl` method that names `Self` in its return type must
    /// resolve `Self` to the receiver type (`Point`), not the `/* Self */`
    /// placeholder. Before P3-α item 6-go-self, `go_self_subst` was set only for
    /// trait impls (value receivers), so an inherent-impl `Self` lowered to the
    /// placeholder and produced an invalid Go signature.
    #[test]
    fn self_in_plain_impl_resolves_to_receiver_type() {
        let imp = node(
            1,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
                trait_args: vec![],
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
                        name: ident("clone"),
                        generic_params: vec![],
                        // Instance method leads with `self` (real lowering).
                        params: vec![param_node(6, "self")],
                        return_type: Some(Box::new(node(4, NodeKind::TypeSelf))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(5, vec![], None)),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![imp]));
        assert!(
            out.contains("func (self *Point) Clone() Point {"),
            "Self should resolve to the receiver type Point, got: {out}"
        );
        assert!(
            !out.contains("/* Self */"),
            "Self placeholder must not leak, got: {out}"
        );
    }

    /// A record construction with a spread base (`Point { y: 9, ..p }`) lowers
    /// to a copy-then-override IIFE — Go has no struct-spread syntax — rather
    /// than dropping the `..p` base (P3-α item 5).
    #[test]
    fn record_spread_lowers_to_iife() {
        let spread_base = id_node(10, "p");
        let rc = node(
            1,
            NodeKind::RecordConstruct {
                path: type_path(&["Point"]),
                fields: vec![AirRecordField {
                    name: ident("y"),
                    value: Some(Box::new(int_lit(2, "9"))),
                }],
                spread: Some(Box::new(spread_base)),
            },
        );
        let out = gen(&module(vec![], vec![rc]));
        assert!(
            out.contains("func() Point { __s := p; __s.Y = 9; return __s }()"),
            "spread should copy base then override, got: {out}"
        );
        assert!(
            !out.contains("/* spread */"),
            "the dropped-spread TODO must be gone, got: {out}"
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

    /// A `public fn main` must still emit Go\'s entry `func main()`, not the
    /// PascalCased `func Main()` (codegen-correctness defect 6).
    #[test]
    fn public_main_emits_entry_point() {
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(2, vec![], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("func main() {"), "got: {out}");
        assert!(!out.contains("func Main"), "got: {out}");
    }

    /// The Optional runtime prelude is emitted only when the module uses
    /// `Optional`/`Some`/`None` (codegen-correctness defect 4).
    #[test]
    fn optional_runtime_gated_on_use() {
        // A module that constructs `Some`/`None` pulls in the runtime.
        let some_call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "Some")),
                args: vec![AirArg {
                    label: None,
                    value: int_lit(12, "1"),
                }],
                type_args: vec![],
            },
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
                body: Box::new(block(2, vec![some_call], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("type __bockOption struct"), "got: {out}");
        assert!(out.contains("__bockSome("), "got: {out}");

        // A module that does not mention Optional gets no prelude.
        let f2 = node(
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
                body: Box::new(block(2, vec![], None)),
            },
        );
        let out2 = gen(&module(vec![], vec![f2]));
        assert!(!out2.contains("__bockOption"), "got: {out2}");
    }

    #[test]
    fn result_runtime_gated_and_constructed() {
        // A module that constructs `Ok`/`Err` pulls in the Result runtime + the
        // shared numeric helpers, and lowers the constructors to `__bockOk`/
        // `__bockErr` (not the old `v, nil` multi-return).
        let ok_call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "Ok")),
                args: vec![AirArg {
                    label: None,
                    value: int_lit(12, "7"),
                }],
                type_args: vec![],
            },
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
                body: Box::new(block(2, vec![ok_call], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("type __bockResult struct"), "got: {out}");
        assert!(out.contains("__bockOk("), "got: {out}");
        // The shared numeric helpers are emitted exactly once.
        assert_eq!(
            out.matches("func __bockAsInt64").count(),
            1,
            "numeric helpers must be emitted once; got: {out}"
        );
    }

    /// The Go Optional runtime stores the `Some` payload as `interface{}`. A
    /// `match` arm binding it (`Some(x)`) must type-assert to the scrutinee's
    /// concrete element type so typed use (`x + 10`) compiles. The element type
    /// is resolved structurally from the `Optional[T]` parameter scrutinee.
    #[test]
    fn optional_match_some_payload_type_asserted() {
        // fn addTen(o: Int?) -> Int { match o { Some(x) => return x; None => return 0 } }
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
                name: ident("addTen"),
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
                body: Box::new(block(3, vec![match_stmt], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        // The `Int` element type is recovered through the widening helper
        // `__bockAsInt64` rather than a hard `.(int64)` assertion: a payload
        // boxed from an untyped Go constant (`Some(10)`) is a Go `int`, on which
        // `.(int64)` panics at runtime.
        assert!(
            out.contains("x := __bockAsInt64(__opt.v);"),
            "Some payload should be recovered via the int64 widening helper, got: {out}"
        );
    }

    /// Build an `impl Counter { fn next(self) -> Int? { ... } }` whose method
    /// has an `Optional[Int]` return type, plus a `match it.next() { Some(x) =>
    /// return x; None => return 0 }` driver function. Used to exercise the
    /// method-call-scrutinee payload resolution (the `core.iter` desugar shape).
    fn iterator_module_with_method_match() -> AIRNode {
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
        let next_method = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("next"),
                generic_params: vec![],
                params: vec![param_node(11, "self")],
                return_type: Some(Box::new(opt_int_ty)),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    12,
                    vec![node(
                        13,
                        NodeKind::Return {
                            value: Some(Box::new(node(
                                14,
                                NodeKind::Call {
                                    callee: Box::new(id_node(15, "None")),
                                    args: vec![],
                                    type_args: vec![],
                                },
                            ))),
                        },
                    )],
                    None,
                )),
            },
        );
        let imp = node(
            5,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(node(
                    6,
                    NodeKind::TypeNamed {
                        path: type_path(&["Counter"]),
                        args: vec![],
                    },
                )),
                where_clause: vec![],
                methods: vec![next_method],
            },
        );
        // fn drive(it: Counter) -> Int {
        //   match it.next() { Some(x) => return x; None => return 0 }
        // }
        let scrutinee = node(
            60,
            NodeKind::MethodCall {
                receiver: Box::new(id_node(61, "it")),
                method: ident("next"),
                type_args: vec![],
                args: vec![],
            },
        );
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
            70,
            NodeKind::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![some_arm, none_arm],
            },
        );
        let drive = node(
            80,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("drive"),
                generic_params: vec![],
                params: vec![typed_param_node(81, "it", "Counter")],
                return_type: Some(Box::new(node(
                    82,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(83, vec![match_stmt], None)),
            },
        );
        module(vec![], vec![imp, drive])
    }

    #[test]
    fn optional_match_method_call_scrutinee_payload_resolved() {
        // The scrutinee `it.next()` is a method call whose method returns
        // `Int?`; the bound `Some` payload must be recovered as `int64` (via the
        // widening helper), not left as bare `interface{}`. This is the
        // `core.iter` `for x in <Iterable>` desugar shape — regression-locking
        // the Go method-call-scrutinee defect.
        let out = gen(&iterator_module_with_method_match());
        assert!(
            out.contains("x := __bockAsInt64(__opt.v);"),
            "method-call-scrutinee Some payload should be resolved to int64, got: {out}"
        );
    }

    /// Build a `loop { match it.next() { Some(x) => { ... } None => break } }`
    /// driver — the exact statement-position desugar shape, where the 2-arm
    /// Optional match lowers to `if/else` and a bare `break` already exits the
    /// `for`. No loop label may be allocated (Go rejects an unused label).
    fn loop_with_optional_match_break() -> AIRNode {
        let scrutinee = node(
            60,
            NodeKind::MethodCall {
                receiver: Box::new(id_node(61, "it")),
                method: ident("next"),
                type_args: vec![],
                args: vec![],
            },
        );
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
                // Some(x) => { sum = sum + x } — a statement-style arm body.
                body: Box::new(block(
                    43,
                    vec![node(
                        44,
                        NodeKind::Assign {
                            op: AssignOp::Assign,
                            target: Box::new(id_node(45, "sum")),
                            value: Box::new(node(
                                46,
                                NodeKind::BinaryOp {
                                    op: BinOp::Add,
                                    left: Box::new(id_node(47, "sum")),
                                    right: Box::new(id_node(48, "x")),
                                },
                            )),
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
                    vec![node(53, NodeKind::Break { value: None })],
                    None,
                )),
            },
        );
        let match_stmt = node(
            70,
            NodeKind::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![some_arm, none_arm],
            },
        );
        let loop_node = node(
            71,
            NodeKind::Loop {
                body: Box::new(block(72, vec![match_stmt], None)),
            },
        );
        let f = node(
            80,
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
                body: Box::new(block(81, vec![loop_node], None)),
            },
        );
        module(vec![], vec![f])
    }

    #[test]
    fn optional_match_break_loop_has_no_unused_label() {
        // A 2-arm Some/None match lowers to `if __opt.tag == "Some" { ... } else
        // { break }`; the bare `break` already exits the `for`, so no
        // `__bockLoopN:` label must be emitted (Go errors on an unused label).
        let out = gen(&loop_with_optional_match_break());
        assert!(
            !out.contains("__bockLoop"),
            "Optional match-in-loop must not allocate an unused loop label, got: {out}"
        );
        // The bare `break` is still present and targets the enclosing `for`.
        assert!(out.contains("break"), "expected a break, got: {out}");
    }

    #[test]
    fn go_loop_label_skipped_for_optional_match_but_kept_for_switch_match() {
        // An Optional match (`Some`/`None`) lowers to if/else: bare break ⇒ no
        // label needed.
        let opt_break = node(
            1,
            NodeKind::Match {
                scrutinee: Box::new(id_node(2, "o")),
                arms: vec![
                    node(
                        3,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(
                                4,
                                NodeKind::ConstructorPat {
                                    path: type_path(&["Some"]),
                                    fields: vec![bind_pat(5, "x")],
                                },
                            )),
                            guard: None,
                            body: Box::new(block(6, vec![], Some(id_node(7, "x")))),
                        },
                    ),
                    node(
                        8,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(
                                9,
                                NodeKind::ConstructorPat {
                                    path: type_path(&["None"]),
                                    fields: vec![],
                                },
                            )),
                            guard: None,
                            body: Box::new(block(
                                10,
                                vec![node(11, NodeKind::Break { value: None })],
                                None,
                            )),
                        },
                    ),
                ],
            },
        );
        assert!(
            !go_loop_needs_label(&opt_break),
            "Optional match-in-loop should not need a label"
        );
        // A non-Optional value-switch match with a `break` arm DOES need a label
        // (bare break would exit the Go switch, not the loop).
        let switch_break = node(
            20,
            NodeKind::Match {
                scrutinee: Box::new(id_node(21, "i")),
                arms: vec![
                    node(
                        22,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(
                                23,
                                NodeKind::LiteralPat {
                                    lit: Literal::Int("5".into()),
                                },
                            )),
                            guard: None,
                            body: Box::new(block(
                                24,
                                vec![node(25, NodeKind::Break { value: None })],
                                None,
                            )),
                        },
                    ),
                    node(
                        26,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(27, NodeKind::WildcardPat)),
                            guard: None,
                            body: Box::new(block(28, vec![], None)),
                        },
                    ),
                ],
            },
        );
        assert!(
            go_loop_needs_label(&switch_break),
            "non-Optional switch match with break should need a label"
        );
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
        assert_eq!(ctx.map_type_name("Char"), "rune");
        assert_eq!(ctx.map_type_name("Void"), "struct{}");
        assert_eq!(ctx.map_type_name("Any"), "interface{}");
    }

    #[test]
    fn parse_tuple_struct_field_types_round_trips_type_to_go() {
        // Inverse of `type_to_go`'s `TypeTuple` arm: parse the per-field Go types
        // back out of the rendered `struct{ Field0 T0; Field1 T1 }`.
        assert_eq!(
            GoEmitCtx::parse_tuple_struct_field_types("struct{ Field0 int64; Field1 int64 }"),
            vec!["int64".to_string(), "int64".to_string()]
        );
        assert_eq!(
            GoEmitCtx::parse_tuple_struct_field_types("struct{ Field0 int64; Field1 string }"),
            vec!["int64".to_string(), "string".to_string()]
        );
        // A non-tuple-struct string yields no fields (callers fall back to
        // element inference).
        assert!(GoEmitCtx::parse_tuple_struct_field_types("[]int64").is_empty());
        assert!(GoEmitCtx::parse_tuple_struct_field_types("int64").is_empty());
    }

    /// Build a `TypeNamed { path: [name], args }` AIR node.
    fn type_named(name: &str, args: Vec<AIRNode>) -> AIRNode {
        node(
            900,
            NodeKind::TypeNamed {
                path: type_path(&[name]),
                args,
            },
        )
    }

    /// The three collection types emit a concrete Go container with their
    /// element/key/value types recovered recursively, NOT the erased
    /// `interface{}` element (P3-α item 1a).
    #[test]
    fn type_to_go_collections_carry_element_types() {
        let ctx = GoEmitCtx::new();
        let int_ty = || type_named("Int", vec![]);
        let str_ty = || type_named("String", vec![]);

        assert_eq!(
            ctx.type_to_go(&type_named("List", vec![int_ty()])),
            "[]int64"
        );
        assert_eq!(
            ctx.type_to_go(&type_named("Set", vec![int_ty()])),
            "map[int64]struct{}"
        );
        assert_eq!(
            ctx.type_to_go(&type_named("Map", vec![str_ty(), int_ty()])),
            "map[string]int64"
        );
        // Recursive: a list of maps.
        let inner_map = type_named("Map", vec![str_ty(), int_ty()]);
        assert_eq!(
            ctx.type_to_go(&type_named("List", vec![inner_map])),
            "[]map[string]int64"
        );
        // A bare collection with no type arg keeps the erased element.
        assert_eq!(ctx.type_to_go(&type_named("List", vec![])), "[]interface{}");
    }

    /// Lifting the collection element type must NOT disturb the genuine runtime
    /// structs `Optional`/`Result`, which still erase their payload to the
    /// tagged runtime struct (`__bockOption` / `__bockResult`) — the regression
    /// the P3-α item 1a change was warned against.
    #[test]
    fn type_to_go_runtime_structs_unchanged() {
        let ctx = GoEmitCtx::new();
        let int_ty = || type_named("Int", vec![]);
        let str_ty = || type_named("String", vec![]);
        assert_eq!(
            ctx.type_to_go(&type_named("Optional", vec![int_ty()])),
            "__bockOption"
        );
        assert_eq!(
            ctx.type_to_go(&type_named("Result", vec![int_ty(), str_ty()])),
            "__bockResult"
        );
    }

    /// `optional_inner_type_node` / `result_inner_type_nodes` peel one container
    /// layer for the nested-pattern declared-type threading: an
    /// `Optional[Result[(Int, Int), String]]` peels to its `Result[…]`, which
    /// peels to the `(Int, Int)` tuple and the `String` err. Rendering the peeled
    /// tuple node reproduces the concrete struct the nested tuple payload is
    /// asserted to.
    #[test]
    fn peel_optional_result_tuple_decl_type_nodes() {
        let ctx = GoEmitCtx::new();
        let int_ty = || type_named("Int", vec![]);
        let str_ty = || type_named("String", vec![]);
        let tuple_ty = node(
            910,
            NodeKind::TypeTuple {
                elems: vec![int_ty(), int_ty()],
            },
        );
        let result_ty = type_named("Result", vec![tuple_ty, str_ty()]);
        let opt_ty = type_named("Optional", vec![result_ty]);

        // Optional → Result.
        let inner = ctx
            .optional_inner_type_node(&opt_ty)
            .expect("peels Optional");
        assert!(matches!(inner.kind, NodeKind::TypeNamed { .. }));
        // Result → (tuple ok, string err).
        let (ok, err) = ctx.result_inner_type_nodes(inner).expect("peels Result");
        assert_eq!(
            ctx.type_to_go(ok),
            "struct{ Field0 int64; Field1 int64 }",
            "the Ok payload is the concrete tuple struct"
        );
        assert_eq!(ctx.type_to_go(err.expect("err arg present")), "string");
        // A non-container type peels to nothing.
        assert!(ctx.optional_inner_type_node(&int_ty()).is_none());
        assert!(ctx.result_inner_type_nodes(&int_ty()).is_none());
    }

    /// `peel_constructor_decl_ty` maps a tag to the inner declared type it carries
    /// (`Some`/`Ok` → ok/elem, `Err` → err), and `tuple_field_decl_tys` splits a
    /// declared tuple type into per-field nodes (or `None` on arity mismatch).
    #[test]
    fn constructor_and_tuple_decl_type_peeling() {
        let ctx = GoEmitCtx::new();
        let int_ty = || type_named("Int", vec![]);
        let str_ty = || type_named("String", vec![]);
        let result_ty = type_named("Result", vec![int_ty(), str_ty()]);
        let opt_ty = type_named("Optional", vec![result_ty.clone()]);

        // `Some` peels Optional → Result (a runtime container, renders __bockResult).
        let some_inner = ctx
            .peel_constructor_decl_ty("Some", Some(&opt_ty))
            .expect("Some peels Optional");
        assert_eq!(ctx.type_to_go(&some_inner), "__bockResult");
        // `Ok` peels Result → Int; `Err` → String.
        let ok_inner = ctx
            .peel_constructor_decl_ty("Ok", Some(&result_ty))
            .expect("Ok peels Result");
        assert_eq!(ctx.type_to_go(&ok_inner), "int64");
        let err_inner = ctx
            .peel_constructor_decl_ty("Err", Some(&result_ty))
            .expect("Err peels Result");
        assert_eq!(ctx.type_to_go(&err_inner), "string");
        // Unknown declared type ⇒ no peel.
        assert!(ctx.peel_constructor_decl_ty("Some", None).is_none());

        // A 2-tuple splits into two field nodes; an arity mismatch yields Nones.
        let tuple_ty = node(
            911,
            NodeKind::TypeTuple {
                elems: vec![int_ty(), str_ty()],
            },
        );
        let fields = ctx.tuple_field_decl_tys(Some(&tuple_ty), 2);
        assert_eq!(fields.len(), 2);
        assert_eq!(ctx.type_to_go(fields[0].expect("field 0")), "int64");
        assert_eq!(ctx.type_to_go(fields[1].expect("field 1")), "string");
        // Arity mismatch / unknown ⇒ all None.
        assert!(ctx
            .tuple_field_decl_tys(Some(&tuple_ty), 3)
            .iter()
            .all(Option::is_none));
        assert!(ctx
            .tuple_field_decl_tys(None, 2)
            .iter()
            .all(Option::is_none));
    }

    /// `fn_type_go_signature` renders a declared `Fn(Int) -> Int` to its Go
    /// param/return types, used to type a lambda returned in tail position;
    /// `fn_type_ret_node` only keeps a function-typed return.
    #[test]
    fn fn_type_signature_for_returned_lambda() {
        let ctx = GoEmitCtx::new();
        let int_ty = || type_named("Int", vec![]);
        let fn_ty = node(
            912,
            NodeKind::TypeFunction {
                params: vec![int_ty()],
                ret: Box::new(int_ty()),
                effects: Vec::new(),
            },
        );
        let (params, ret) = ctx.fn_type_go_signature(&fn_ty).expect("is a fn type");
        assert_eq!(params, vec!["int64".to_string()]);
        assert_eq!(ret, "int64");
        // A non-function return type yields no signature / no kept node.
        assert!(ctx.fn_type_go_signature(&int_ty()).is_none());
        assert!(GoEmitCtx::fn_type_ret_node(Some(&int_ty())).is_none());
        assert!(GoEmitCtx::fn_type_ret_node(Some(&fn_ty)).is_some());
    }

    /// `payload_access_go` asserts an Optional/Result *leaf* payload bind to its
    /// concrete element type — numeric via the widening helpers, others via a
    /// direct assertion — and reads the raw payload for unit/unknown types (a hard
    /// assertion on a boxed `nil` would panic).
    #[test]
    fn payload_access_typed_leaf_bind() {
        let ctx = GoEmitCtx::new();
        assert_eq!(
            ctx.payload_access_go("opt", Some("int64")),
            "__bockAsInt64(opt.v)"
        );
        assert_eq!(
            ctx.payload_access_go("opt", Some("float64")),
            "__bockAsFloat64(opt.v)"
        );
        assert_eq!(
            ctx.payload_access_go("res", Some("string")),
            "res.v.(string)"
        );
        assert_eq!(ctx.payload_access_go("opt", Some("struct{}")), "opt.v");
        assert_eq!(ctx.payload_access_go("opt", None), "opt.v");
    }

    /// `specialise_lambda_param_types` sees through a `type` alias to a function
    /// type so a lambda argument bound to a `Predicate = Fn(Int) -> Bool`
    /// parameter is typed `func(x int64) bool`, not the erased `interface{}`.
    #[test]
    fn lambda_param_types_see_through_fn_type_alias() {
        let mut ctx = GoEmitCtx::new();
        let int_ty = || type_named("Int", vec![]);
        let bool_ty = || type_named("Bool", vec![]);
        let fn_ty = node(
            913,
            NodeKind::TypeFunction {
                params: vec![int_ty()],
                ret: Box::new(bool_ty()),
                effects: Vec::new(),
            },
        );
        // Register `type Predicate = Fn(Int) -> Bool`.
        ctx.type_aliases.insert("Predicate".to_string(), fn_ty);
        let alias = type_named("Predicate", vec![]);
        let tys = ctx
            .specialise_lambda_param_types(&alias, &[], &HashMap::new())
            .expect("alias resolves to a fn type");
        assert_eq!(tys, vec!["int64".to_string()]);
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

    /// Go forbids a struct having a field and a method with the same name. A
    /// record whose field name collides with a method's PascalCased Go name (the
    /// `core.error` shape: `record SimpleError { message: String }` +
    /// `fn message(self) -> String`) must emit the method under a disambiguated
    /// name (`MessageMethod`) at the *trait interface*, the *receiver method*,
    /// and every *call site* so they agree — while the field stays `Message`.
    /// Q-go-error-message: pre-S6b this emitted both `Message` field and
    /// `Message()` method on `SimpleError`, which `go build` rejects.
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
                    ty: TypeExpr::Named {
                        id: 0,
                        span: span(),
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                    default: None,
                }],
            },
        );
        // trait Error { fn message(self) -> String }
        let trait_decl = node(
            2,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Error"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![node(
                    3,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("message"),
                        generic_params: vec![],
                        params: vec![param_node(4, "self")],
                        return_type: Some(Box::new(type_named_node(5, "String"))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(6, vec![], None)),
                    },
                )],
            },
        );
        // impl Error for SimpleError { public fn message(self) -> String { self.message } }
        let method = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("message"),
                generic_params: vec![],
                params: vec![param_node(11, "self")],
                return_type: Some(Box::new(type_named_node(12, "String"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    13,
                    vec![],
                    Some(node(
                        14,
                        NodeKind::FieldAccess {
                            object: Box::new(id_node(15, "self")),
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
                target: Box::new(type_named_node(21, "SimpleError")),
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
                return_type: Some(Box::new(type_named_node(32, "String"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    33,
                    vec![],
                    Some(node(
                        34,
                        NodeKind::MethodCall {
                            receiver: Box::new(id_node(35, "e")),
                            method: ident("message"),
                            type_args: vec![],
                            args: vec![],
                        },
                    )),
                )),
            },
        );
        let out = gen(&module(
            vec![],
            vec![record_decl, trait_decl, impl_block, read_fn],
        ));
        // The field stays `Message`.
        assert!(
            out.contains("Message\tstring"),
            "field should remain `Message`, got: {out}"
        );
        // The method (interface, receiver, call site) is disambiguated.
        assert!(
            out.contains("MessageMethod() string"),
            "trait interface should declare `MessageMethod()`, got: {out}"
        );
        assert!(
            out.contains("SimpleError) MessageMethod()"),
            "receiver method should be `MessageMethod()`, got: {out}"
        );
        assert!(
            out.contains(".MessageMethod()"),
            "call site should be `.MessageMethod()`, got: {out}"
        );
        // The body still reads the field (`self.Message`), and no plain
        // `Message()` method (the colliding form Go rejects) is emitted.
        assert!(
            out.contains("return self.Message"),
            "method body should read the field `self.Message`, got: {out}"
        );
        assert!(
            !out.contains(") Message() string"),
            "must NOT emit a `Message()` method colliding with the field, got: {out}"
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
                // Instance method leads with `self` (real lowering); a no-`self`
                // method is an associated function (free function, no receiver).
                params: vec![
                    param_node(14, "self"),
                    typed_param_node(11, "msg", "String"),
                ],
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
                trait_args: vec![],
                generic_params: vec![],
                where_clause: vec![],
                methods: vec![method],
            },
        );
        let out = gen(&module(vec![], vec![record_decl, impl_block]));
        assert!(
            out.contains("func (self StdoutLogger) Log("),
            "impl method should use value receiver, got: {out}"
        );
        assert!(
            !out.contains("func (self *StdoutLogger) Log("),
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

    // ── Generics codegen (DV12 / P1-b2) ───────────────────────────────────────

    fn generic_param(id: u32, name: &str) -> bock_ast::GenericParam {
        bock_ast::GenericParam {
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
    fn generic_method_receiver_carries_type_params() {
        // `impl Box { ... }` for `record Box[T]` must emit
        // `func (self *Box[T]) get() T` — Go requires the type-param list on the
        // receiver, recovered from the record decl since the impl has none.
        let out = gen(&module(
            vec![],
            vec![generic_box_record(), generic_box_impl()],
        ));
        assert!(
            out.contains("func (self *Box[T]) get() T {"),
            "generic method receiver should carry `[T]`, got: {out}"
        );
    }

    /// `impl Box { fn map[U](self, f: Fn(T) -> U) -> Box[U] { Box { value:
    /// f(self.value) } } }`.
    fn generic_box_map_impl() -> AIRNode {
        let self_param = node(
            120,
            NodeKind::Param {
                pattern: Box::new(bind_pat(121, "self")),
                ty: None,
                default: None,
            },
        );
        // `f: Fn(T) -> U`
        let f_ty = node(
            122,
            NodeKind::TypeFunction {
                params: vec![named_type(123, "T")],
                ret: Box::new(named_type(124, "U")),
                effects: vec![],
            },
        );
        let f_param = node(
            125,
            NodeKind::Param {
                pattern: Box::new(bind_pat(126, "f")),
                ty: Some(Box::new(f_ty)),
                default: None,
            },
        );
        // Body: `Box { value: f(self.value) }`
        let call_f = node(
            127,
            NodeKind::Call {
                callee: Box::new(id_node(128, "f")),
                type_args: vec![],
                args: vec![AirArg {
                    label: None,
                    value: node(
                        129,
                        NodeKind::FieldAccess {
                            object: Box::new(id_node(130, "self")),
                            field: ident("value"),
                        },
                    ),
                }],
            },
        );
        let construct = node(
            131,
            NodeKind::RecordConstruct {
                path: type_path(&["Box"]),
                fields: vec![bock_air::AirRecordField {
                    name: ident("value"),
                    value: Some(Box::new(call_f)),
                }],
                spread: None,
            },
        );
        let body = block(132, vec![], Some(construct));
        let ret_ty = node(
            133,
            NodeKind::TypeNamed {
                path: type_path(&["Box"]),
                args: vec![named_type(134, "U")],
            },
        );
        let method = node(
            135,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("map"),
                generic_params: vec![generic_param(136, "U")],
                params: vec![self_param, f_param],
                return_type: Some(Box::new(ret_ty)),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        node(
            137,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(named_type(138, "Box")),
                where_clause: vec![],
                methods: vec![method],
            },
        )
    }

    #[test]
    fn method_level_type_params_lower_to_free_function() {
        // DQ28: Go forbids method type params, so `Box[T].map[U]` lowers to a
        // free function `func Box_Map[T any, U any](self Box[T], f func(T) U)
        // Box[U]` (the receiver becomes a leading `self` parameter; the
        // receiver's `T` and the method's `U` combine on the free function).
        let out = gen(&module(
            vec![],
            vec![generic_box_record(), generic_box_map_impl()],
        ));
        assert!(
            out.contains("func Box_Map[T any, U any](self Box[T], f func(T) U) Box[U] {"),
            "method-generic should free-function-lower with combined type params, got: {out}"
        );
        // The invalid `func (self *Box[T]) Map[U](..)` (Go syntax error) must NOT
        // be emitted.
        assert!(
            !out.contains(") Map["),
            "must not emit a Go method with type params, got: {out}"
        );
    }

    #[test]
    fn method_level_type_param_call_site_rewrites_to_free_function() {
        // A call `b.map(f)` to the free-function-lowered `Box.map[U]` rewrites to
        // `Box_Map(b, f)` (receiver-first), for both the `MethodCall` and the
        // desugared `Call(FieldAccess(b, map), [b, f])` shapes.
        let recv = id_node(200, "b");
        let cb = node(
            201,
            NodeKind::Lambda {
                params: vec![param_node(202, "x")],
                body: Box::new(block(
                    203,
                    vec![],
                    Some(node(
                        204,
                        NodeKind::BinaryOp {
                            op: BinOp::Mul,
                            left: Box::new(id_node(205, "x")),
                            right: Box::new(int_lit(206, "2")),
                        },
                    )),
                )),
            },
        );
        let call = node(
            207,
            NodeKind::MethodCall {
                receiver: Box::new(recv),
                method: ident("map"),
                type_args: vec![],
                args: vec![AirArg {
                    label: None,
                    value: cb,
                }],
            },
        );
        let let_stmt = node(
            208,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(209, "r")),
                ty: None,
                value: Box::new(call),
            },
        );
        let out = gen(&module(
            vec![],
            vec![generic_box_record(), generic_box_map_impl(), let_stmt],
        ));
        assert!(
            out.contains("Box_Map(b, "),
            "call site should rewrite to the free-function call `Box_Map(b, ..)`, got: {out}"
        );
        assert!(
            !out.contains("b.Map("),
            "call site must not keep the Go method-call form, got: {out}"
        );
    }

    #[test]
    fn generic_construct_emits_explicit_type_args() {
        // `Box { value: 42 }` → `Box[int64]{Value: 42}` (Go does not infer
        // struct type args from composite-literal fields).
        let construct = node(
            40,
            NodeKind::RecordConstruct {
                path: type_path(&["Box"]),
                fields: vec![bock_air::AirRecordField {
                    name: ident("value"),
                    value: Some(Box::new(int_lit(41, "42"))),
                }],
                spread: None,
            },
        );
        let let_stmt = node(
            42,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(43, "b")),
                ty: None,
                value: Box::new(construct),
            },
        );
        let out = gen(&module(vec![], vec![generic_box_record(), let_stmt]));
        assert!(
            out.contains("Box[int64]{Value: 42}"),
            "generic construction should carry explicit `[int64]`, got: {out}"
        );
    }

    #[test]
    fn generic_fn_return_list_literal_uses_param_type() {
        // GAP-C: `fn single[T](x: T) -> List[T] { return [x] }` must emit
        // `return []T{x}`, not `[]interface{}{x}` (which a `[]T` return rejects).
        let list_t = node(
            61,
            NodeKind::TypeNamed {
                path: type_path(&["List"]),
                args: vec![named_type(62, "T")],
            },
        );
        let body = block(
            63,
            vec![],
            Some(node(
                64,
                NodeKind::Return {
                    value: Some(Box::new(node(
                        65,
                        NodeKind::ListLiteral {
                            elems: vec![id_node(66, "x")],
                        },
                    ))),
                },
            )),
        );
        let f = node(
            67,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("single"),
                generic_params: vec![generic_param(68, "T")],
                params: vec![node(
                    69,
                    NodeKind::Param {
                        pattern: Box::new(bind_pat(70, "x")),
                        ty: Some(Box::new(named_type(71, "T"))),
                        default: None,
                    },
                )],
                return_type: Some(Box::new(list_t)),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("return []T{x}"),
            "generic fn returning a list literal should use `[]T`, got: {out}"
        );
    }

    #[test]
    fn generic_construct_uses_declared_type_args_for_nested_param() {
        // GAP-C/D plumbing: `let c: ListIter[Int] = ListIter { xs: [...] }` for
        // `record ListIter[T] { xs: List[T] }` must emit `ListIter[int64]{...}`.
        // Field inference yields `any` here (no field is typed exactly `T`; `xs`
        // is `List[T]`), so the construction must adopt the declared binding
        // type's concrete args.
        let record = node(
            10,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                name: ident("ListIter"),
                generic_params: vec![generic_param(11, "T")],
                fields: vec![bock_ast::RecordDeclField {
                    id: 12,
                    span: span(),
                    name: ident("xs"),
                    ty: TypeExpr::Named {
                        id: 13,
                        span: span(),
                        path: type_path(&["List"]),
                        args: vec![TypeExpr::Named {
                            id: 14,
                            span: span(),
                            path: type_path(&["T"]),
                            args: vec![],
                        }],
                    },
                    default: None,
                }],
            },
        );
        let construct = node(
            20,
            NodeKind::RecordConstruct {
                path: type_path(&["ListIter"]),
                fields: vec![bock_air::AirRecordField {
                    name: ident("xs"),
                    value: Some(Box::new(node(
                        21,
                        NodeKind::ListLiteral {
                            elems: vec![int_lit(22, "1")],
                        },
                    ))),
                }],
                spread: None,
            },
        );
        let let_stmt = node(
            23,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(24, "c")),
                ty: Some(Box::new(node(
                    25,
                    NodeKind::TypeNamed {
                        path: type_path(&["ListIter"]),
                        args: vec![named_type(26, "Int")],
                    },
                ))),
                value: Box::new(construct),
            },
        );
        let out = gen(&module(vec![], vec![record, let_stmt]));
        assert!(
            out.contains("ListIter[int64]{"),
            "construction should adopt the declared binding type's `[int64]`, got: {out}"
        );
        assert!(
            !out.contains("ListIter[any]{"),
            "construction must NOT fall back to `[any]` when a declared type is present, got: {out}"
        );
    }

    #[test]
    fn lambda_return_type_inferred_from_body() {
        // `(n: Int) => n + 1` → `func(n int64) int64 { return (n + 1) }`, not
        // `interface{}` (which fails to satisfy a typed `func(int64) int64`).
        let lambda = node(
            50,
            NodeKind::Lambda {
                params: vec![typed_param_node(51, "n", "Int")],
                body: Box::new(node(
                    52,
                    NodeKind::BinaryOp {
                        op: BinOp::Add,
                        left: Box::new(id_node(53, "n")),
                        right: Box::new(int_lit(54, "1")),
                    },
                )),
            },
        );
        let let_stmt = node(
            55,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(56, "inc")),
                ty: None,
                value: Box::new(lambda),
            },
        );
        let out = gen(&module(vec![], vec![let_stmt]));
        assert!(
            out.contains("func(n int64) int64 {"),
            "lambda should infer `int64` return type, got: {out}"
        );
        assert!(
            !out.contains("func(n int64) interface{}"),
            "lambda should NOT fall back to interface{{}} return, got: {out}"
        );
    }

    #[test]
    fn unify_go_pattern_binds_iterator_and_slice_params() {
        let gp = vec!["T".to_string(), "U".to_string()];
        // `ListIterator[T]` against `ListIterator[int64]` binds T → int64.
        let mut b = HashMap::new();
        GoEmitCtx::unify_go_pattern("ListIterator[T]", "ListIterator[int64]", &gp, &mut b);
        assert_eq!(b.get("T"), Some(&"int64".to_string()));
        // `[]T` against `[]string` binds T → string.
        let mut b2 = HashMap::new();
        GoEmitCtx::unify_go_pattern("[]T", "[]string", &gp, &mut b2);
        assert_eq!(b2.get("T"), Some(&"string".to_string()));
        // A bare param binds to the whole concrete type.
        let mut b3 = HashMap::new();
        GoEmitCtx::unify_go_pattern("U", "map[string]int64", &gp, &mut b3);
        assert_eq!(b3.get("U"), Some(&"map[string]int64".to_string()));
        // A structural mismatch records nothing (conservative).
        let mut b4 = HashMap::new();
        GoEmitCtx::unify_go_pattern("ListIterator[T]", "int64", &gp, &mut b4);
        assert!(b4.is_empty());
    }

    #[test]
    fn split_top_level_commas_respects_nesting() {
        assert_eq!(
            GoEmitCtx::split_top_level_commas("int64, []string"),
            vec!["int64".to_string(), "[]string".to_string()]
        );
        // A comma nested inside `[...]` is not a top-level separator.
        assert_eq!(
            GoEmitCtx::split_top_level_commas("map[string]int64, T"),
            vec!["map[string]int64".to_string(), "T".to_string()]
        );
        assert_eq!(
            GoEmitCtx::split_top_level_commas("int64"),
            vec!["int64".to_string()]
        );
    }

    #[test]
    fn replace_type_token_only_swaps_whole_identifiers() {
        // `T` in `[]T` is replaced; `T` inside `Tree` is not.
        assert_eq!(
            GoEmitCtx::replace_type_token("[]T", "T", "int64"),
            "[]int64"
        );
        assert_eq!(GoEmitCtx::replace_type_token("Tree", "T", "int64"), "Tree");
        assert_eq!(
            GoEmitCtx::replace_type_token("func(T) T", "T", "int64"),
            "func(int64) int64"
        );
        assert!(GoEmitCtx::contains_type_token("ListIterator[T]", "T"));
        assert!(!GoEmitCtx::contains_type_token("ListIterator[int64]", "T"));
    }

    #[test]
    fn generic_trait_with_type_param_is_a_generic_interface() {
        // A trait that declares its own generic param (`Iterable[T]`) becomes a
        // generic Go interface so a method signature naming `T` is in scope.
        let method = node(
            2,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("iter"),
                generic_params: vec![],
                params: vec![node(
                    10,
                    NodeKind::Param {
                        pattern: Box::new(bind_pat(11, "self")),
                        ty: None,
                        default: None,
                    },
                )],
                return_type: Some(Box::new(node(
                    20,
                    NodeKind::TypeNamed {
                        path: type_path(&["ListIterator"]),
                        args: vec![named_type(21, "T")],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(3, vec![], None)),
            },
        );
        let t = node(
            1,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Iterable"),
                generic_params: vec![generic_param(30, "T")],
                associated_types: vec![],
                methods: vec![method],
            },
        );
        let out = gen(&module(vec![], vec![t]));
        assert!(
            out.contains("type Iterable[T any] interface {"),
            "generic trait should carry its type param on the interface, got: {out}"
        );
        assert!(
            out.contains("Iter() ListIterator[T]"),
            "the method return must keep `[T]`, got: {out}"
        );
    }

    // ── Per-module native-package tree (S3) ─────────────────────────────────

    fn mod_path(segs: &[&str]) -> bock_ast::ModulePath {
        bock_ast::ModulePath {
            segments: segs.iter().map(|s| ident(s)).collect(),
            span: span(),
        }
    }

    /// A module node with a declared dotted `path` (e.g. `core.option`).
    fn module_with_path(path: &[&str], imports: Vec<AIRNode>, items: Vec<AIRNode>) -> AIRNode {
        node(
            0,
            NodeKind::Module {
                path: Some(mod_path(path)),
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
                path: mod_path(path),
                items: bock_ast::ImportItems::Named(vec![bock_ast::ImportedName {
                    span: span(),
                    name: ident(name),
                    alias: None,
                }]),
            },
        )
    }

    /// A bare `fn <name>() -> <tail>` declaration with the given visibility.
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
    fn per_module_emits_native_go_package_tree() {
        // entry `module main` uses `mathutil.add_one`; `module mathutil` exports
        // a `public fn add_one`. Per-module emission must produce the native Go
        // *source* package: `main.go` (one `package main`) and the flat
        // `mathutil.go` (same package — the call site needs no import) —
        // separate files, not a single collapsed file. The `go.mod` run
        // affordance is emitted by the scaffolder (project mode), NOT codegen
        // (S6a / DV18).
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

        let gen = GoGenerator::new();
        let out = gen
            .generate_project(&[
                (&main_mod, std::path::Path::new("src/main.bock")),
                (&util_mod, std::path::Path::new("src/mathutil.bock")),
            ])
            .unwrap();
        let by_name = |p: &str| out.files.iter().find(|f| f.path == std::path::Path::new(p));
        let main_file = by_name("main.go").expect("main.go emitted");
        let util_file = by_name("mathutil.go").expect("flat mathutil.go emitted");
        // Codegen no longer emits the manifest (S6a / DV18) — the scaffolder
        // owns the `go.mod` in project mode.
        assert!(
            by_name("go.mod").is_none(),
            "codegen must NOT emit go.mod — the scaffolder owns it (S6a)"
        );

        assert!(
            main_file.content.starts_with("package main"),
            "main.go must be `package main`; got:\n{}",
            main_file.content
        );
        // Same package → the cross-module call needs no import statement.
        assert!(
            !main_file.content.contains("import \"mathutil\""),
            "main.go must NOT import the sibling (same package); got:\n{}",
            main_file.content
        );
        // The exported fn is PascalCased on emit; the call site matches.
        assert!(
            util_file.content.contains("func AddOne("),
            "mathutil.go must carry the exported fn; got:\n{}",
            util_file.content
        );
        assert!(
            main_file.content.contains("AddOne("),
            "main.go must call the cross-module fn; got:\n{}",
            main_file.content
        );
    }

    #[test]
    fn per_module_nested_module_flattens_filename() {
        // A nested `core.option` module flattens to a single flat file
        // `core.option.go` (one package per dir — no subdirectory; dots kept so
        // a `core.test` module never collides with Go's `_test.go` suffix).
        let opt_mod = module_with_path(
            &["core", "option"],
            vec![],
            vec![fn_decl_tail(
                20,
                Visibility::Public,
                "get_or",
                int_lit(22, "0"),
            )],
        );
        let main_mod = module_with_path(
            &["main"],
            vec![import_named(5, &["core", "option"], "get_or")],
            vec![fn_decl_tail(
                1,
                Visibility::Private,
                "main",
                node(
                    10,
                    NodeKind::Call {
                        callee: Box::new(id_node(11, "get_or")),
                        args: vec![],
                        type_args: vec![],
                    },
                ),
            )],
        );
        let gen = GoGenerator::new();
        let out = gen
            .generate_project(&[
                (&main_mod, std::path::Path::new("src/main.bock")),
                (&opt_mod, std::path::Path::new("src/core/option.bock")),
            ])
            .unwrap();
        let by_name = |p: &str| out.files.iter().find(|f| f.path == std::path::Path::new(p));
        by_name("core.option.go").expect("nested module flattens to core.option.go");
        // No subdirectory: there must be no `core/option.go`.
        assert!(
            by_name("core/option.go").is_none(),
            "go must NOT emit a subdirectory package file"
        );
    }

    /// `fn f() { let x = if (c) { 1 } else { return 0 }  x }` — value-position
    /// `if` with a diverging else. The shared value-CF hoist lowers it to a
    /// `var __bockCf0 T` (type inferred from the assigned arm values) plus
    /// statement-form assignment, never `/* unsupported */` or an IIFE that
    /// captures the `return`.
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
            !out.contains("/* unsupported */"),
            "diverging value-if must not emit `/* unsupported */`, got: {out}"
        );
        // The temp is declared with an inferred Go type (`int64` from the arms).
        // Go's `go_value_ident` strips the leading `__`, so the name is `bockCf0`.
        assert!(
            out.contains("var bockCf0 int64"),
            "must declare a typed temp `var bockCf0 int64`, got: {out}"
        );
        assert!(
            out.contains("bockCf0 = 1"),
            "value arm must assign the temp, got: {out}"
        );
        assert!(
            out.contains("return 0"),
            "diverging arm must keep its return, got: {out}"
        );
    }

    // ── Q-guard-let-shared (go) ───────────────────────────────────────────────

    /// `guard (let Ok(v) = cond) else { return … }` must test the discriminant
    /// tag and bind the payload into the *enclosing* scope (live after the
    /// guard), not negate the non-bool `__bockResult` and drop the binding.
    #[test]
    fn go_guard_let_tests_tag_and_binds_payload() {
        // guard (let Ok(v) = res) else { return }
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
                condition: Box::new(id_node(13, "res")),
                else_block: Box::new(block(
                    14,
                    vec![node(15, NodeKind::Return { value: None })],
                    None,
                )),
            },
        );
        // fn check() -> Void { guard …; }
        let f = node(
            20,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("check"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(21, vec![guard], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        // The discriminant is hoisted into a `__guard` temp and tested by tag,
        // never negated as a bool.
        assert!(
            out.contains("__guard0 := res"),
            "guard-let must hoist the discriminant, got: {out}"
        );
        assert!(
            out.contains("if !(__guard0.tag == \"Ok\")"),
            "guard-let must test the tag, got: {out}"
        );
        assert!(
            !out.contains("if !(res)"),
            "guard-let must not negate the non-bool discriminant, got: {out}"
        );
        // The else arm diverges, then the payload binding lands after the `if`.
        assert!(
            out.contains("v := __guard0.v"),
            "guard-let must bind the payload after the guard, got: {out}"
        );
    }

    // ── Q-let-shadow-const (go) ───────────────────────────────────────────────

    /// A shadowing `let` re-binding a name already declared in the block lowers
    /// to a reassignment (`acc = …`), not a colliding re-declaration
    /// (`acc := …` / `var acc … = …` — Go's "no new variables on left side").
    #[test]
    fn go_let_shadow_rebinds_as_assignment() {
        let stmts = vec![
            // let acc = 1
            node(
                10,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(11, "acc")),
                    ty: None,
                    value: Box::new(int_lit(12, "1")),
                },
            ),
            // let acc = acc + 2  (shadow → reassignment)
            node(
                13,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(14, "acc")),
                    ty: None,
                    value: Box::new(node(
                        15,
                        NodeKind::BinaryOp {
                            op: BinOp::Add,
                            left: Box::new(id_node(16, "acc")),
                            right: Box::new(int_lit(17, "2")),
                        },
                    )),
                },
            ),
        ];
        let f = node(
            20,
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
                body: Box::new(block(21, stmts, Some(id_node(22, "acc")))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        // First binding declares; the second reassigns.
        assert!(
            out.contains("acc = (acc + 2)"),
            "the shadowing re-bind must be an assignment, got: {out}"
        );
        // No second declaration of `acc`.
        assert!(
            out.matches("acc :=").count() + out.matches("var acc").count() <= 1,
            "a shadowing re-bind must not re-declare `acc`, got: {out}"
        );
    }

    /// A `let` shadowing a *parameter* (the same Go scope as the body) must also
    /// reassign, not re-declare.
    #[test]
    fn go_let_shadow_of_param_rebinds_as_assignment() {
        // fn bump(n) { let n = n + 1; n }
        let let_n = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "n")),
                ty: None,
                value: Box::new(node(
                    12,
                    NodeKind::BinaryOp {
                        op: BinOp::Add,
                        left: Box::new(id_node(13, "n")),
                        right: Box::new(int_lit(14, "1")),
                    },
                )),
            },
        );
        let f = node(
            20,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("bump"),
                generic_params: vec![],
                params: vec![typed_param_node(21, "n", "Int")],
                return_type: Some(Box::new(node(
                    22,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(23, vec![let_n], Some(id_node(24, "n")))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("n = (n + 1)"),
            "a `let` shadowing a param must reassign, got: {out}"
        );
        assert!(
            !out.contains("n := (n + 1)") && !out.contains("var n int64 = (n + 1)"),
            "a `let` shadowing a param must not re-declare, got: {out}"
        );
    }

    // ── Q-propagate-operator-noop (go) ────────────────────────────────────────

    /// `let v = expr?` must unwrap the `Ok`/`Some` payload and early-return the
    /// propagated error/None on failure — not pass `expr` through unchanged
    /// (which bound the whole `__bockResult` and never short-circuited).
    #[test]
    fn go_propagate_let_unwraps_and_early_returns() {
        // let v = f()?
        let let_v = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "v")),
                ty: None,
                value: Box::new(node(
                    12,
                    NodeKind::Propagate {
                        expr: Box::new(node(
                            13,
                            NodeKind::Call {
                                callee: Box::new(id_node(14, "f")),
                                args: vec![],
                                type_args: vec![],
                            },
                        )),
                    },
                )),
            },
        );
        // fn g() -> Result[Int, String] { let v = f()?; Ok(v) }
        let body_tail = node(
            15,
            NodeKind::Call {
                callee: Box::new(id_node(16, "Ok")),
                args: vec![AirArg {
                    label: None,
                    value: id_node(17, "v"),
                }],
                type_args: vec![],
            },
        );
        let f = node(
            20,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("g"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(node(
                    22,
                    NodeKind::TypeNamed {
                        path: type_path(&["Result"]),
                        args: vec![
                            node(
                                23,
                                NodeKind::TypeNamed {
                                    path: type_path(&["Int"]),
                                    args: vec![],
                                },
                            ),
                            node(
                                24,
                                NodeKind::TypeNamed {
                                    path: type_path(&["String"]),
                                    args: vec![],
                                },
                            ),
                        ],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(21, vec![let_v], Some(body_tail))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        // The operand is hoisted; failure early-returns the propagated Err.
        assert!(
            out.contains("__try0 := f()"),
            "propagate must hoist the operand, got: {out}"
        );
        assert!(
            out.contains("if __try0.tag == \"Err\""),
            "propagate must check the Err tag, got: {out}"
        );
        assert!(
            out.contains("return __try0"),
            "propagate must early-return the propagated Err, got: {out}"
        );
        // The success payload is bound to `v` (not the whole `__bockResult`).
        assert!(
            out.contains("v := __try0.v") || out.contains("v := __bockAsInt64(__try0.v)"),
            "propagate must bind the unwrapped payload, got: {out}"
        );
        // It is no longer a no-op passthrough.
        assert!(
            !out.contains("v := f()\n"),
            "propagate must not pass the operand through unchanged, got: {out}"
        );
    }

    /// Build a single-param fn whose body is `return match scrutinee { arms }`,
    /// for exercising the expression-position match lowering.
    fn return_match_fn(name: &str, param: &str, ty: &str, arms: Vec<AIRNode>) -> AIRNode {
        let match_node = node(
            500,
            NodeKind::Match {
                scrutinee: Box::new(id_node(501, param)),
                arms,
            },
        );
        let ret = node(
            502,
            NodeKind::Return {
                value: Some(Box::new(match_node)),
            },
        );
        node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![typed_param_node(2, param, ty)],
                return_type: Some(Box::new(node(
                    3,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(4, vec![], Some(ret))),
            },
        )
    }

    /// Q-list-range-pattern-shared: a list-pattern `match` in expression position
    /// (`return match items { [] => …; [only] => …; [first, ..rest] => … }`) must
    /// route to the if-chain (the shared recogniser now flags `ListPat`), emitting
    /// a `len(...)` test per arm and positional element / `..rest` slice binds —
    /// not the broken `switch` whose every arm collapsed to `case interface{}:`.
    #[test]
    fn go_list_pattern_expr_match_lowers_to_ifchain_with_binds() {
        let empty_arm = node(
            20,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    21,
                    NodeKind::ListPat {
                        elems: vec![],
                        rest: None,
                    },
                )),
                guard: None,
                body: Box::new(block(22, vec![], Some(str_lit(23, "empty")))),
            },
        );
        let single_arm = node(
            30,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    31,
                    NodeKind::ListPat {
                        elems: vec![bind_pat(32, "only")],
                        rest: None,
                    },
                )),
                guard: None,
                body: Box::new(block(33, vec![], Some(id_node(34, "only")))),
            },
        );
        let head_rest_arm = node(
            40,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    41,
                    NodeKind::ListPat {
                        elems: vec![bind_pat(42, "first")],
                        rest: Some(Box::new(bind_pat(43, "rest"))),
                    },
                )),
                guard: None,
                body: Box::new(block(44, vec![], Some(id_node(45, "first")))),
            },
        );
        let else_arm = node(
            46,
            NodeKind::MatchArm {
                pattern: Box::new(node(47, NodeKind::WildcardPat)),
                guard: None,
                body: Box::new(block(48, vec![], Some(str_lit(49, "other")))),
            },
        );
        let f = return_match_fn(
            "DescribeList",
            "items",
            "List",
            vec![empty_arm, single_arm, head_rest_arm, else_arm],
        );
        let out = gen(&module(vec![], vec![f]));
        // No broken `case interface{}:` placeholder.
        assert!(
            !out.contains("case interface{}"),
            "list-pattern match must not emit a broken `case interface{{}}`, got: {out}"
        );
        // `[]` → exact length 0.
        assert!(
            out.contains("len(items) == 0"),
            "`[]` arm should test len == 0, got: {out}"
        );
        // `[only]` → length 1 and binds `only := items[0]`.
        assert!(
            out.contains("len(items) == 1") && out.contains("only := items[0]"),
            "`[only]` should test len == 1 and bind `only`, got: {out}"
        );
        // `[first, ..rest]` → length >= 1, binds first and a rest slice.
        assert!(
            out.contains("len(items) >= 1"),
            "`[first, ..rest]` should test len >= 1, got: {out}"
        );
        assert!(
            out.contains("first := items[0]") && out.contains("rest := items[1:]"),
            "`[first, ..rest]` should bind `first` and `rest := items[1:]`, got: {out}"
        );
        // The arm bodies return their values (expression-position IIFE).
        assert!(
            out.contains("return \"empty\""),
            "arm bodies must return their value, got: {out}"
        );
    }

    /// Q-list-range-pattern-shared: a range-pattern `match` in expression position
    /// must route to the if-chain with a relational bounds test (`>= lo && < hi`
    /// exclusive, `<=` inclusive) — not a broken `switch`.
    #[test]
    fn go_range_pattern_expr_match_lowers_to_ifchain_with_bounds() {
        let lo_arm = node(
            20,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    21,
                    NodeKind::RangePat {
                        lo: Box::new(int_lit(22, "1")),
                        hi: Box::new(int_lit(23, "10")),
                        inclusive: false,
                    },
                )),
                guard: None,
                body: Box::new(block(24, vec![], Some(str_lit(25, "a")))),
            },
        );
        let hi_arm = node(
            30,
            NodeKind::MatchArm {
                pattern: Box::new(node(
                    31,
                    NodeKind::RangePat {
                        lo: Box::new(int_lit(32, "10")),
                        hi: Box::new(int_lit(33, "20")),
                        inclusive: true,
                    },
                )),
                guard: None,
                body: Box::new(block(34, vec![], Some(str_lit(35, "b")))),
            },
        );
        let else_arm = node(
            40,
            NodeKind::MatchArm {
                pattern: Box::new(node(41, NodeKind::WildcardPat)),
                guard: None,
                body: Box::new(block(42, vec![], Some(str_lit(43, "c")))),
            },
        );
        let f = return_match_fn("ClassifyRange", "n", "Int", vec![lo_arm, hi_arm, else_arm]);
        let out = gen(&module(vec![], vec![f]));
        assert!(
            !out.contains("case interface{}"),
            "range-pattern match must not emit a broken `case interface{{}}`, got: {out}"
        );
        // Exclusive `1..10` → `n >= 1 && n < 10`.
        assert!(
            out.contains("n >= 1") && out.contains("n < 10"),
            "`1..10` should test `n >= 1 && n < 10`, got: {out}"
        );
        // Inclusive `10..=20` → `n >= 10 && n <= 20`.
        assert!(
            out.contains("n >= 10") && out.contains("n <= 20"),
            "`10..=20` should test `n >= 10 && n <= 20`, got: {out}"
        );
    }

    fn float_lit(id: u32, val: &str) -> AIRNode {
        node(
            id,
            NodeKind::Literal {
                lit: Literal::Float(val.into()),
            },
        )
    }

    fn pow_fn(name: &str, ret_ty: &str, left: AIRNode, right: AIRNode) -> AIRNode {
        let pow = node(
            10,
            NodeKind::BinaryOp {
                op: BinOp::Pow,
                left: Box::new(left),
                right: Box::new(right),
            },
        );
        node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(node(
                    2,
                    NodeKind::TypeNamed {
                        path: type_path(&[ret_ty]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(3, vec![], Some(pow))),
            },
        )
    }

    #[test]
    fn pow_int_lowers_to_int_pow_helper() {
        // `2 ** 10` (Int ** Int) must NOT emit the broken `(2 /* pow */ 10)`
        // (a Go syntax error) — it lowers to the `__bockIntPow` runtime helper
        // with both operands coerced to `int64`, and the helper is emitted.
        let f = pow_fn("p", "Int", int_lit(11, "2"), int_lit(12, "10"));
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("__bockIntPow(int64(2), int64(10))"),
            "Int pow should lower to __bockIntPow, got: {out}"
        );
        assert!(
            !out.contains("/* pow */"),
            "Int pow must not emit the broken `/* pow */` form, got: {out}"
        );
        assert!(
            out.contains("func __bockIntPow("),
            "the integer-power runtime helper must be emitted, got: {out}"
        );
    }

    #[test]
    fn pow_float_lowers_to_math_pow() {
        // `2.0 ** 3.0` (Float ** Float) lowers to `math.Pow` (float64 in/out)
        // and pulls in the `math` import.
        let f = pow_fn("p", "Float", float_lit(11, "2.0"), float_lit(12, "3.0"));
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("math.Pow(float64(2.0), float64(3.0))"),
            "Float pow should lower to math.Pow, got: {out}"
        );
        assert!(
            out.contains("\"math\""),
            "Float pow must import the math package, got: {out}"
        );
        assert!(
            !out.contains("/* pow */"),
            "Float pow must not emit the broken `/* pow */` form, got: {out}"
        );
    }

    fn match_arm(id: u32, pattern: AIRNode, body_str: &str) -> AIRNode {
        node(
            id,
            NodeKind::MatchArm {
                pattern: Box::new(pattern),
                guard: None,
                body: Box::new(block(id + 1, vec![], Some(str_lit(id + 2, body_str)))),
            },
        )
    }

    fn constructor_pat(id: u32, name: &str, fields: Vec<AIRNode>) -> AIRNode {
        node(
            id,
            NodeKind::ConstructorPat {
                path: type_path(&[name]),
                fields,
            },
        )
    }

    #[test]
    fn go_valpos_bind_match_routes_to_ifchain_and_binds() {
        // Q-go-valpos-bind-match: `return match n { x => "got ${x}" }`. A bare
        // bind has no value to switch on, so the value-switch IIFE emitted the
        // broken `case interface{}:` and dropped `x`. It must route to the
        // if-chain, binding `x := n` in an unconditional `else`.
        let arm = match_arm(20, bind_pat(21, "x"), "got it");
        let f = return_match_fn("EchoBinding", "n", "Int", vec![arm]);
        let out = gen(&module(vec![], vec![f]));
        assert!(
            !out.contains("case interface{}"),
            "bind-pattern match must not emit `case interface{{}}`, got: {out}"
        );
        assert!(
            out.contains("x := n"),
            "the bind arm must introduce `x := n`, got: {out}"
        );
    }

    #[test]
    fn go_valpos_nested_optional_match_binds_nested_payload() {
        // Q-go-nested-optional-match: `match val { Some(Ok(n)) => …; Some(Err(e))
        // => …; None => … }`. The nested arm must route to the if-chain (the flat
        // tag-switch dropped the nested `n`), testing both `.tag`s and binding `n`
        // off the typed nested `.v` payload — never `undefined: n`.
        let some_ok = constructor_pat(
            20,
            "Some",
            vec![constructor_pat(21, "Ok", vec![bind_pat(22, "n")])],
        );
        let some_err = constructor_pat(
            30,
            "Some",
            vec![constructor_pat(31, "Err", vec![bind_pat(32, "e")])],
        );
        let none = constructor_pat(40, "None", vec![]);
        let arms = vec![
            match_arm(50, some_ok, "got n"),
            match_arm(53, some_err, "err e"),
            match_arm(56, none, "nothing"),
        ];
        let f = return_match_fn("NestedUnwrap", "val", "Optional", arms);
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("val.tag == \"Some\"") && out.contains(".tag == \"Ok\""),
            "nested Optional/Result match must test both tags, got: {out}"
        );
        assert!(
            out.contains("n := "),
            "the nested `Ok(n)` payload must be bound, got: {out}"
        );
        assert!(
            !out.contains("case interface{}"),
            "nested match must not emit a broken `case interface{{}}`, got: {out}"
        );
    }

    #[test]
    fn go_valpos_plain_record_match_routes_to_ifchain_and_binds() {
        // Q-plainrecord-valpos-match: `match p { Point { x, .. } => "x=${x}" }`. A
        // plain record is a concrete struct (no sealed-interface type/value to
        // switch on), so the value/type-switch emitted the broken `case Point:`
        // and dropped `x`. It must route to the if-chain, reading `x := p.X`.
        let rec_pat = node(
            20,
            NodeKind::RecordPat {
                path: type_path(&["Point"]),
                fields: vec![bock_air::AirRecordPatternField {
                    name: ident("x"),
                    pattern: None,
                }],
                rest: true,
            },
        );
        let arm = match_arm(30, rec_pat, "x val");
        let f = return_match_fn("GetX", "p", "Point", vec![arm]);
        let out = gen(&module(vec![], vec![f]));
        assert!(
            !out.contains("case Point") && !out.contains("case interface{}"),
            "plain-record match must not emit `case Point` / `case interface{{}}`, got: {out}"
        );
        assert!(
            out.contains("x := p.X"),
            "the plain-record field must be bound as `x := p.X`, got: {out}"
        );
    }

    /// A `Fn(...) -> Void` *parameter* type lowers to a Go `func(...)` with NO
    /// result type — `func() struct{}` (the Void *value* type as a result) is a
    /// function that must `return struct{}{}`, which a void closure body never
    /// does, so the closure would not satisfy the parameter. Guards Item 1's
    /// type-lowering fix (`type_to_go`'s `TypeFunction` arm).
    #[test]
    fn fn_void_param_type_lowers_to_bare_func() {
        // fn on_click(handler: Fn() -> Void) -> Void { handler() }
        let fn_void_ty = node(
            2,
            NodeKind::TypeFunction {
                params: vec![],
                ret: Box::new(type_named_node(3, "Void")),
                effects: vec![],
            },
        );
        let handler_param = node(
            4,
            NodeKind::Param {
                pattern: Box::new(bind_pat(5, "handler")),
                ty: Some(Box::new(fn_void_ty)),
                default: None,
            },
        );
        let call = node(
            6,
            NodeKind::Call {
                callee: Box::new(id_node(7, "handler")),
                args: vec![],
                type_args: vec![],
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("on_click"),
                generic_params: vec![],
                params: vec![handler_param],
                return_type: Some(Box::new(type_named_node(8, "Void"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(9, vec![call], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("handler func()"),
            "`Fn() -> Void` param should lower to `func()`, got: {out}"
        );
        assert!(
            !out.contains("func() struct{}"),
            "`Fn() -> Void` must NOT emit `func() struct{{}}`, got: {out}"
        );
    }

    /// An inherent (`impl Type`) method whose name a `trait` the type implements
    /// also declares is exported (`Render`, not `render`) so it satisfies the Go
    /// interface directly, AND the redundant same-named `impl Trait for Type`
    /// forwarder (`fn render(self) { self.render() }`) is skipped — emitting it
    /// would produce `func (T) Render() { return self.Render() }`, infinite
    /// recursion. Guards Item 2 (method-name casing + no self-recursive
    /// forwarder).
    #[test]
    fn inherent_method_exported_for_trait_and_no_recursive_forwarder() {
        // trait Component { fn render(self) -> String }
        let trait_method = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("render"),
                generic_params: vec![],
                params: vec![param_node(11, "self")],
                return_type: Some(Box::new(type_named_node(12, "String"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(13, vec![], None)),
            },
        );
        let trait_decl = node(
            14,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Component"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![trait_method],
            },
        );
        // impl Button { fn render(self) -> String { "x" } }  (private, inherent)
        let inherent_method = node(
            20,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("render"),
                generic_params: vec![],
                params: vec![param_node(21, "self")],
                return_type: Some(Box::new(type_named_node(22, "String"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(23, vec![], Some(str_lit(24, "x")))),
            },
        );
        let inherent_impl = node(
            25,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(type_named_node(26, "Button")),
                where_clause: vec![],
                methods: vec![inherent_method],
            },
        );
        // impl Component for Button { fn render(self) -> String { self.render() } }
        let forwarder_method = node(
            30,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("render"),
                generic_params: vec![],
                params: vec![param_node(31, "self")],
                return_type: Some(Box::new(type_named_node(32, "String"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(
                    33,
                    vec![],
                    Some(node(
                        34,
                        NodeKind::MethodCall {
                            receiver: Box::new(id_node(35, "self")),
                            method: ident("render"),
                            type_args: vec![],
                            args: vec![],
                        },
                    )),
                )),
            },
        );
        let trait_impl = node(
            36,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: Some(type_path(&["Component"])),
                trait_args: vec![],
                target: Box::new(type_named_node(37, "Button")),
                where_clause: vec![],
                methods: vec![forwarder_method],
            },
        );
        let out = gen(&module(vec![], vec![trait_decl, inherent_impl, trait_impl]));
        // The inherent method is exported to `Render`.
        assert!(
            out.contains("Button) Render() string"),
            "inherent method should be exported `Render`, got: {out}"
        );
        // The lowercase inherent name must not survive.
        assert!(
            !out.contains("Button) render() string"),
            "inherent method must not stay lowercase `render`, got: {out}"
        );
        // The self-recursive forwarder must be skipped (only ONE `Render` body).
        assert_eq!(
            out.matches("Button) Render()").count(),
            1,
            "exactly one `Render` method should be emitted on Button, got: {out}"
        );
        assert!(
            !out.contains("return self.Render()"),
            "the self-recursive forwarder must be eliminated, got: {out}"
        );
    }
}
