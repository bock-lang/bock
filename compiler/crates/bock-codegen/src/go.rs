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
use bock_ast::{AssignOp, BinOp, Literal, TypeExpr, UnaryOp, Visibility};
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
/// `lo..hi` / `lo..=hi`. Emitted at most once per bundle, gated on a ctx flag
/// (mirrors `OPTIONAL_RUNTIME_GO`).
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

/// True if the module references a `Range` node anywhere (so the range runtime
/// helper must be emitted). Mirrors [`go_module_uses_optional`]. `RangePat`
/// (a match-arm range pattern) does not contain the `Range {` substring, so it
/// is not matched — the helper is only needed for range *values*.
fn go_module_uses_range(items: &[AIRNode]) -> bool {
    items.iter().any(|n| format!("{n:?}").contains("Range {"))
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
/// calls. Mirrors `OPTIONAL_RUNTIME_GO` — the `core.compare` enum decl is not
/// bundled into the single-file entry, so `Ordering`/`Less`/`Equal`/`Greater`
/// and `(x).compare(y)` need this self-contained representation. A value-switch
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
        let mut ctx = GoEmitCtx::new();
        ctx.enum_variants =
            crate::generator::collect_enum_variants(&[(module, std::path::Path::new(""))]);
        ctx.generic_decls =
            crate::generator::collect_generic_decls(&[(module, std::path::Path::new(""))]);
        ctx.collect_record_param_fields(module);
        ctx.collect_async_fns(module);
        ctx.collect_methods(module);
        ctx.collect_optional_returns(module);
        ctx.collect_method_optional_returns(module);
        ctx.collect_fn_and_type_names(module);
        ctx.trait_decls =
            crate::generator::collect_trait_decls(&[(module, std::path::Path::new(""))]);
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

    /// Bundle every module (stdlib + user, dependency-ordered) into one entry
    /// file. Go has no single top-level scope across files, so a cross-module
    /// `use` (DV13) cannot be a real `import "core/compare"` for a lone
    /// `go run main.go`. Instead all module bodies are concatenated into one
    /// `package main` file with a **single merged, deduped `import (...)` block**
    /// (the union of each module's `fmt`/`sync`/`time` needs) and each runtime
    /// prelude (Optional / concurrency) emitted **at most once**. `ImportDecl`s
    /// are dropped. Go uses a native `func main`, so no entry invocation is
    /// appended.
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

        // Pre-scan async fns across ALL modules so cross-module calls between
        // async functions route through the Async-suffix wrappers, then seed a
        // SINGLE shared ctx. Emitting every module through one ctx makes the
        // runtime-once flags and the `needs_*` import flags dedup/merge for free.
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

        let mut ctx = GoEmitCtx::new();
        ctx.async_fns = global_async_fns;
        // Pre-scan enum variants across the whole bundle so a `use`d enum's
        // variants resolve at a construction/pattern site in another module.
        ctx.enum_variants = crate::generator::collect_enum_variants(modules);
        // Pre-scan generic-type declarations so an `impl Box { ... }` recovers
        // the `[T]` declared on `record Box[T]` even across module boundaries.
        ctx.generic_decls = crate::generator::collect_generic_decls(modules);
        // Pre-scan method / Optional-return metadata across every module so a
        // match whose scrutinee calls a function/method defined in another
        // bundled module still type-asserts its payload correctly.
        ctx.trait_decls = crate::generator::collect_trait_decls(modules);
        ctx.derive_self_param_traits();
        for (module, _) in modules {
            ctx.collect_methods(module);
            ctx.collect_optional_returns(module);
            ctx.collect_method_optional_returns(module);
            ctx.collect_record_param_fields(module);
            ctx.collect_fn_and_type_names(module);
        }
        for (i, (module, _)) in modules.iter().enumerate() {
            if i > 0 && !ctx.buf.is_empty() && !ctx.buf.ends_with("\n\n") {
                ctx.buf.push('\n');
            }
            ctx.emit_node(module)?;
        }
        let (body, needs_fmt, needs_sync, needs_time) = ctx.into_parts();

        // One `package main`, one merged/deduped `import (...)` block.
        let mut content = "package main\n".to_string();
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
                content.push_str(&format!("\nimport {}\n", imports[0]));
            } else {
                content.push_str("\nimport (\n");
                for imp in &imports {
                    content.push_str(&format!("\t{imp}\n"));
                }
                content.push_str(")\n");
            }
        }
        content.push('\n');
        content.push_str(&body);

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
    /// Names of `public` methods (declared in impl/class/trait blocks). Used at
    /// desugared method-call sites to pick PascalCase (public) vs camelCase
    /// (private) so the call matches the method definition's Go casing.
    public_methods: HashSet<String>,
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
    /// Set once the concurrency runtime prelude has been emitted into `buf`, so
    /// a single-file **bundle** of several modules (cross-module `use`, DV13)
    /// emits it at most once (a duplicate `type __bockChannel` would not
    /// compile). A lone-module build sets it on first use exactly as before.
    concurrency_runtime_emitted: bool,
    /// Set once the Optional runtime prelude has been emitted into `buf`;
    /// deduped across a bundle exactly as [`Self::concurrency_runtime_emitted`].
    optional_runtime_emitted: bool,
    /// Set once the `Result` runtime prelude has been emitted; deduped across a
    /// bundle exactly as [`Self::optional_runtime_emitted`].
    result_runtime_emitted: bool,
    /// Set once the shared numeric-payload helpers ([`NUMERIC_RUNTIME_GO`]) have
    /// been emitted. Emitted once if *either* the Optional or `Result` runtime is
    /// used, so the two never redeclare `__bockAsInt64`/`__bockAsFloat64`.
    numeric_runtime_emitted: bool,
    /// Set once the [`ORDERING_RUNTIME_GO`] prelude has been emitted; deduped
    /// across a bundle exactly as [`Self::optional_runtime_emitted`].
    ordering_runtime_emitted: bool,
    /// Set once the [`RANGE_RUNTIME_GO`] helper has been emitted; deduped across
    /// a bundle exactly as [`Self::optional_runtime_emitted`] (a duplicate
    /// `func __bockRange` would not compile).
    range_runtime_emitted: bool,
    /// User-enum-variant registry (DV14). Go has no sum type, so a user enum is
    /// a sealed interface + per-variant structs named `{enum}{variant}`
    /// (e.g. `ShapeCircle`). The registry lets a construction emit the variant
    /// struct literal and a `match` emit a *type-switch* (`switch __v :=
    /// s.(type) { case ShapeCircle: … }`) with field extraction, rather than the
    /// broken value-switch on the unqualified variant name. Built-in
    /// Optional/Result pre-seeds are filtered out (Optional has its own
    /// `__bockOption` runtime). Pre-scanned across the bundle.
    enum_variants: crate::generator::EnumVariantRegistry,
    /// Generic-type declaration registry: a record/enum/class name → its
    /// declared generic params. Lets an `impl Box { ... }` block recover the
    /// `[T any]` declared on `record Box[T]` so a Go method receiver emits
    /// `func (self *Box[T]) ...` (Go requires the type-param list on the
    /// receiver) and a construction emits `Box[int64]{...}`. Pre-scanned across
    /// the bundle (mirrors [`Self::enum_variants`]).
    generic_decls: crate::generator::GenericDeclRegistry,
    /// Maps an in-scope variable name → its Go type, used to infer a lambda's
    /// return type. Go infers a bare `func(...) interface{}` for every lambda;
    /// when such a closure is passed to a typed `func(int64) int64` parameter
    /// the assignment fails to compile. Tracking param/binding Go types lets the
    /// lambda emitter recover a concrete return type structurally from the body.
    /// Scoped per function/lambda body and restored on exit.
    var_go_type: HashMap<String, String>,
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
    /// Only `List`-typed fields are recorded. Pre-scanned across the bundle.
    record_field_list_elem: HashMap<String, HashMap<String, String>>,
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
    /// scanned across the bundle (mirrors [`Self::enum_variants`]).
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
            public_methods: HashSet::new(),
            loop_labels: Vec::new(),
            switch_label_depth: 0,
            loop_label_counter: 0,
            fn_optional_ret_elem: HashMap::new(),
            var_optional_elem: HashMap::new(),
            method_optional_ret_elem: HashMap::new(),
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
            range_runtime_emitted: false,
            enum_variants: crate::generator::EnumVariantRegistry::new(),
            generic_decls: crate::generator::GenericDeclRegistry::new(),
            var_go_type: HashMap::new(),
            record_param_fields: HashMap::new(),
            record_field_list_elem: HashMap::new(),
            current_self_record: None,
            trait_decls: crate::generator::TraitDeclRegistry::new(),
            type_names: HashSet::new(),
            go_self_subst: None,
            self_param_traits: HashSet::new(),
            current_fn_ret_type: None,
            current_expected_type: None,
            current_fn_ret_collection_elem: None,
            expected_collection_elem: None,
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
            to_camel_case(name)
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
            }
        }
    }

    /// Pre-scan a module's top-level `RecordDecl`s and, for each generic
    /// record, record which field's declared type is each generic param (in
    /// param order). A construction site then looks up the field value's Go
    /// type per param to emit the explicit `[arg, ...]` instantiation Go
    /// requires. Additive across a bundle (mirrors the other `collect_*`).
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
            if generic_params.is_empty() {
                continue;
            }
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

    /// True when the real `core.compare.Ordering` enum is bundled into this
    /// program (its `Less` variant is a registered user enum variant). Under
    /// DV13 bundling, `use core.compare` concatenates the actual `enum Ordering`
    /// decl into the entry file; its `Less`/`Equal`/`Greater` references and
    /// matches then use the user-enum representation (sealed-interface variant
    /// structs `OrderingLess{}`), not the prelude `__bockOrdering` value runtime
    /// used when the enum is *not* bundled (e.g. a bare primitive `compare`).
    fn ordering_enum_bundled(&self) -> bool {
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
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                let (methods, always_export) = match &item.kind {
                    NodeKind::ImplBlock {
                        methods,
                        trait_path,
                        ..
                    } => (methods, trait_path.is_some()),
                    NodeKind::TraitDecl { methods, .. } => (methods, true),
                    NodeKind::ClassDecl { methods, .. } => (methods, false),
                    _ => continue,
                };
                for m in methods {
                    if let NodeKind::FnDecl {
                        visibility, name, ..
                    } = &m.kind
                    {
                        if always_export || matches!(visibility, Visibility::Public) {
                            self.public_methods.insert(name.name.clone());
                        }
                    }
                }
            }
        }
    }

    /// Pre-scan top-level functions whose declared return type is `Optional[T]`,
    /// recording `fn name → Go element type` of `T`. This lets a `match` whose
    /// scrutinee is a call to such a function (`match next(it) { Some(x) => ...
    /// }`) type-assert the bound payload to its concrete type. Must run before
    /// any match is emitted, so it covers forward references within the module.
    fn collect_optional_returns(&mut self, module: &AIRNode) {
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
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
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                let methods = match &item.kind {
                    NodeKind::ImplBlock { methods, .. }
                    | NodeKind::ClassDecl { methods, .. }
                    | NodeKind::TraitDecl { methods, .. } => methods,
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
                    }
                }
            }
        }
        for name in &poisoned {
            self.method_optional_ret_elem.remove(name);
        }
    }

    /// If `node` is an `Optional[T]` type expression, return the Go type of its
    /// element `T`; otherwise `None`. Used to type-assert the `interface{}`
    /// payload of the Go Optional runtime back to its concrete element type at
    /// `match` arms. The element type is reachable structurally here because it
    /// lives in the `TypeOptional`/`Optional`-named node, unlike at the
    /// scrutinee expression (whose carried `type_info` is a stub).
    fn optional_elem_go_type(&self, node: &AIRNode) -> Option<String> {
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

    /// The `(K, V)` Go types of a `Map` *value* expression used as the receiver
    /// of a built-in map method. Recovered from a declared `Map[K, V]`
    /// identifier (via [`Self::var_map_kv`]) or a homogeneously-typed map
    /// literal. `None` ⇒ the caller falls back to `interface{}` (never a wrong
    /// type).
    fn map_receiver_kv_go_types(&self, recv: &AIRNode) -> Option<(String, String)> {
        match &recv.kind {
            NodeKind::Identifier { name } => {
                self.var_map_kv.get(&to_camel_case(&name.name)).cloned()
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
                self.var_set_elem.get(&to_camel_case(&name.name)).cloned()
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
                self.var_list_elem.get(&to_camel_case(&name.name)).cloned()
            }
            NodeKind::ListLiteral { elems } => self.infer_homogeneous_elem_type(elems),
            NodeKind::Range { .. } => Some("int64".to_string()),
            _ => None,
        }
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
                self.var_list_elem.get(&to_camel_case(&name.name)).cloned()
            }
            NodeKind::ListLiteral { elems } => self.infer_homogeneous_elem_type(elems),
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
            _ => None,
        }
    }

    /// True when `node` is (or contains, in operand position) an identifier
    /// whose Go type is not in `scope` — i.e. an `interface{}`-typed value an
    /// arithmetic operation cannot soundly operate on. Used to keep arithmetic
    /// type-inference conservative (untyped lambda params stay `interface{}`).
    fn has_unresolved_operand(node: &AIRNode, scope: &HashMap<String, String>) -> bool {
        match &node.kind {
            NodeKind::Identifier { name } => !scope.contains_key(&to_camel_case(&name.name)),
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
                self.var_go_type.get(&to_camel_case(&name.name)).cloned()
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
                let name = to_camel_case(&self.pattern_to_binding_name(pattern));
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
    fn enter_param_go_types(&mut self, params: &[AIRNode]) -> HashMap<String, String> {
        let saved = self.var_go_type.clone();
        for p in params {
            if let NodeKind::Param {
                pattern,
                ty: Some(t),
                ..
            } = &p.kind
            {
                let name = to_camel_case(&self.pattern_to_binding_name(pattern));
                self.var_go_type.insert(name, self.type_to_go(t));
            }
        }
        saved
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
        let Some((base, args)) = self.var_record_type_args.get(&to_camel_case(&name.name)) else {
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
                .get(&to_camel_case(&name.name))
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
                    crate::generator::desugared_list_method(callee, args)
                {
                    if matches!(method, "get" | "first" | "last") {
                        if let NodeKind::Identifier { name } = &recv.kind {
                            if let Some(elem) = self.var_list_elem.get(&to_camel_case(&name.name)) {
                                return Some(elem.clone());
                            }
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
                .get(&to_camel_case(&name.name))
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
            // Optional constructors → tagged runtime struct.
            "Some" => {
                let a = arg_strs
                    .first()
                    .map_or_else(|| "nil".to_string(), |s| s.clone());
                format!("__bockSome({a})")
            }
            "None" => "__bockNone".to_string(),
            // Result constructors → tagged runtime struct (see
            // `RESULT_RUNTIME_GO`), mirroring `Some`/`None`.
            "Ok" => {
                let a = arg_strs
                    .first()
                    .map_or_else(|| "nil".to_string(), |s| s.clone());
                format!("__bockOk({a})")
            }
            "Err" => {
                let a = arg_strs
                    .first()
                    .map_or_else(|| "nil".to_string(), |s| s.clone());
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
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) = crate::generator::desugared_list_method(callee, args)
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
                let o = self.expr_to_string(&o.value)?;
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
                self.needs_time_import = true;
                "time.Now()".to_string()
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
        let code = match method {
            "compare" => {
                let Some(other) = rest.first() else {
                    return Ok(false);
                };
                let other = self.expr_to_string(&other.value)?;
                format!("__bockCompare({recv_str}, {other})")
            }
            "eq" => {
                let Some(other) = rest.first() else {
                    return Ok(false);
                };
                let other = self.expr_to_string(&other.value)?;
                format!("(({recv_str}) == ({other}))")
            }
            "to_string" | "display" => {
                self.needs_fmt_import = true;
                format!("fmt.Sprintf(\"%v\", {recv_str})")
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
            NodeKind::Module { items, .. } => {
                // Cross-module `use` (DV13) → single-file bundling: every
                // module's items are concatenated into the one entry file (one
                // `package main`, one merged `import (...)` block built by the
                // caller) and `ImportDecl`s are dropped. Each runtime prelude is
                // emitted at most once across the bundle, gated on a ctx flag
                // (a duplicate `type __bockChannel`/`__bockOption` would not
                // compile).
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
                // the real `core.compare.Ordering` enum is NOT bundled — when it
                // is, that user enum is authoritative (its variants are the
                // sealed-interface structs `OrderingLess{}`, and `compare`
                // returns it), so the int runtime would be dead and its `Less`
                // constants would shadow nothing the program uses.
                if !self.ordering_runtime_emitted
                    && go_module_uses_ordering(items)
                    && !self.ordering_enum_bundled()
                {
                    self.buf.push_str(ORDERING_RUNTIME_GO);
                    self.buf.push('\n');
                    self.ordering_runtime_emitted = true;
                }
                if !self.range_runtime_emitted && go_module_uses_range(items) {
                    self.buf.push_str(RANGE_RUNTIME_GO);
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
                // Resolved by bundling — the imported module's items are
                // concatenated into this same `package main` file — so the
                // import is a no-op (DV13). A real `import "core/compare"` would
                // not resolve: there is no such package for a `go run main.go`.
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
                    self.emit_method(&name.name, generic_params, method, false)?;
                }
                Ok(())
            }
            NodeKind::TraitDecl { name, methods, .. } => {
                // Traits → Go interfaces. A trait whose methods take a
                // `Self`-typed operand is encoded as an F-bounded *generic*
                // interface — `type Comparable[__Self any] interface {
                // Compare(__Self) Ordering }` — so an impl `func (Key)
                // Compare(Key)` satisfies `Comparable[Key]` and a bound `[T:
                // Comparable]` lowers to `[T Comparable[T]]`. `Self` in the
                // method signatures then renders as the interface's type param.
                let uses_self = self.self_param_traits.contains(&name.name);
                let prev_self_subst = self.go_self_subst.take();
                let head = if uses_self {
                    self.go_self_subst = Some("__Self".to_string());
                    format!("{}[__Self any]", name.name)
                } else {
                    name.name.clone()
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
                            to_pascal_case(&name.name),
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
                for (i, method) in methods.iter().chain(default_methods.iter()).enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    self.emit_method(&target_name, &target_generics, method, use_value_receiver)?;
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
                            // An F-bounded self-param trait constraint is applied
                            // to the type var itself: `[T Comparable[T]]`.
                            if self.self_param_traits.contains(&bound_name) {
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
        let (
            saved_opt_scope,
            saved_list_scope,
            saved_result_scope,
            saved_map_scope,
            saved_set_scope,
        ) = self.enter_param_optional_scope(params);
        if name == "main" || is_void {
            self.emit_block_body(body)?;
        } else {
            let prev_ret = self.current_fn_ret_type.take();
            let prev_ret_coll = self.current_fn_ret_collection_elem.take();
            self.current_fn_ret_type = return_type.map(|t| self.type_to_go(t));
            self.current_fn_ret_collection_elem =
                return_type.and_then(|t| self.collection_elem_go_types(t));
            self.emit_block_body_return(body)?;
            self.current_fn_ret_type = prev_ret;
            self.current_fn_ret_collection_elem = prev_ret_coll;
        }
        self.var_optional_elem = saved_opt_scope;
        self.var_list_elem = saved_list_scope;
        self.var_result_elem = saved_result_scope;
        self.var_map_kv = saved_map_scope;
        self.var_set_elem = saved_set_scope;
        self.var_record_type_args = saved_record_args;
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
        // method kinds. Saved/restored so the bundle-wide default of `/* Self */`
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
            // here while the interface declares it PascalCased. Inherent (`impl
            // Type`) methods keep the public/private casing rule.
            let method_name = if use_value_receiver || matches!(visibility, Visibility::Public) {
                to_pascal_case(&name.name)
            } else {
                to_camel_case(&name.name)
            };
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
            self.indent += 1;
            let old_handler_vars = self.current_handler_vars.clone();
            let expanded = self.expand_effect_names(effect_clause);
            for ename in &expanded {
                self.current_handler_vars
                    .insert(ename.clone(), to_camel_case(ename));
            }
            let saved_record_args = self.var_record_type_args.clone();
            let (
                saved_opt_scope,
                saved_list_scope,
                saved_result_scope,
                saved_map_scope,
                saved_set_scope,
            ) = self.enter_param_optional_scope(rest);
            if return_type.is_some() && !is_void {
                let prev_ret = self.current_fn_ret_type.take();
                let prev_ret_coll = self.current_fn_ret_collection_elem.take();
                self.current_fn_ret_type = return_type.as_deref().map(|t| self.type_to_go(t));
                self.current_fn_ret_collection_elem = return_type
                    .as_deref()
                    .and_then(|t| self.collection_elem_go_types(t));
                self.emit_block_body_return(body)?;
                self.current_fn_ret_type = prev_ret;
                self.current_fn_ret_collection_elem = prev_ret_coll;
            } else {
                self.emit_block_body(body)?;
            }
            self.var_optional_elem = saved_opt_scope;
            self.var_list_elem = saved_list_scope;
            self.var_result_elem = saved_result_scope;
            self.var_map_kv = saved_map_scope;
            self.var_set_elem = saved_set_scope;
            self.var_record_type_args = saved_record_args;
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
                    // Record an `Optional[T]` binding's element type so a later
                    // `match binding { Some(x) => ... }` can type-assert `x`.
                    if let Some(elem) = self.optional_elem_go_type(t) {
                        self.var_optional_elem
                            .insert(to_camel_case(&self.pattern_to_binding_name(pattern)), elem);
                    }
                    // Record a `List[T]` binding's element type so a later
                    // `match binding.get(i) { Some(x) => ... }` can type-assert
                    // the `interface{}` payload.
                    if let Some(elem) = self.list_elem_go_type(t) {
                        self.var_list_elem
                            .insert(to_camel_case(&self.pattern_to_binding_name(pattern)), elem);
                    }
                    // Record a `Map[K, V]` / `Set[E]` binding's element Go types
                    // so a later built-in method (`m.get(k)`, `s.contains(x)`,
                    // …) lowers its inline closures over the concretely-typed
                    // receiver `map[K]V` / `map[E]struct{}`.
                    if let Some(kv) = self.map_kv_go_types(t) {
                        self.var_map_kv
                            .insert(to_camel_case(&self.pattern_to_binding_name(pattern)), kv);
                    }
                    if let Some(elem) = self.set_elem_go_type(t) {
                        self.var_set_elem
                            .insert(to_camel_case(&self.pattern_to_binding_name(pattern)), elem);
                    }
                    // Record a `Result[T, E]` binding's Ok/Err types so a later
                    // `match binding { Ok(v) => ...; Err(e) => ... }` can
                    // type-assert the bound payload.
                    if let Some(elems) = self.result_elem_go_types(t) {
                        self.var_result_elem
                            .insert(to_camel_case(&self.pattern_to_binding_name(pattern)), elems);
                    }
                    // Record a generic-record binding's concrete instantiation
                    // (`let c: ListIter[Int]` → `("ListIter", ["int64"])`) so a
                    // later `match c.next() { Some(x) => ... }` resolves the
                    // generic `Optional[T]` payload to the concrete arg (`int64`)
                    // rather than the undefined-in-caller `T`.
                    if let Some(record_args) = self.record_type_args(t) {
                        self.var_record_type_args.insert(
                            to_camel_case(&self.pattern_to_binding_name(pattern)),
                            record_args,
                        );
                    }
                    let type_str = self.type_to_go(t);
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
                            .insert(to_camel_case(&self.pattern_to_binding_name(pattern)), kv);
                    }
                    if let Some(elem) = self.value_set_elem_go_type(value) {
                        self.var_set_elem
                            .insert(to_camel_case(&self.pattern_to_binding_name(pattern)), elem);
                    }
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
                self.emit_loop_label_prefix(body);
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}for _, {binding} := range ");
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
                    self.var_go_type.insert(to_camel_case(&name.name), elem);
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
                self.emit_loop_label_prefix(body);
                self.writeln("for {");
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
                    self.emit_expr(val)?;
                    self.expected_collection_elem = prev_expected;
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
                if name.name == "None" {
                    self.buf.push_str("__bockNone");
                    return Ok(());
                }
                // Prelude `Ordering` variant → the bare `__bockOrdering` constant
                // (`Less`/`Equal`/`Greater`) of the Ordering runtime, which a
                // value-switch `case Less:` and the `compare` bridge also use —
                // UNLESS the real `core.compare.Ordering` enum is bundled (DV13),
                // in which case the reference is a user-enum variant-struct
                // construction (`OrderingLess{}`), handled by the path below.
                if crate::generator::ordering_variant(&name.name).is_some()
                    && !self.ordering_enum_bundled()
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
                let emitted = if is_prelude_ctor(&name.name) {
                    name.name.clone()
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
                if self.try_emit_list_method(callee, args)? {
                    return Ok(());
                }
                if self.try_emit_primitive_bridge(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_container_method(node, callee, args)? {
                    return Ok(());
                }
                // Desugared instance method call `Call(FieldAccess(recv, m),
                // [recv, ...rest])`: emit `recv.M(rest)` using Go method casing
                // so the receiver flows through the native `self` receiver
                // rather than as a duplicated `interface{}` argument.
                if let Some((recv, method, rest)) =
                    crate::generator::desugared_self_call(callee, args)
                {
                    self.emit_expr(recv)?;
                    let go_method = if self.public_methods.contains(&method.name) {
                        to_pascal_case(&method.name)
                    } else {
                        to_camel_case(&method.name)
                    };
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
                // Record the lambda's typed params so the body's return type can
                // be inferred structurally. Without a concrete return type Go
                // infers `interface{}`, which fails to satisfy a typed
                // `func(int64) int64` parameter at the use site.
                let saved_go_types = self.enter_param_go_types(params);
                let ret_ty = self
                    .infer_go_expr_type(body)
                    .unwrap_or_else(|| "interface{}".to_string());
                let _ = write!(
                    self.buf,
                    "func({}) {ret_ty} {{ return ",
                    param_strs.join(", ")
                );
                self.emit_expr(body)?;
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
                    self.buf.push_str(&format!("{type_name}{type_args}{{"));
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
                let field_types: Vec<String> = elems
                    .iter()
                    .map(|e| {
                        self.infer_go_expr_type(e)
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
                    self.emit_expr(v)?;
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
                // Consume the expected type for THIS IIFE's return only; the
                // branch bodies are separately typed (and may re-set it via a
                // nested `let`), so it must not leak into them.
                let prev_expected = self.current_expected_type.take();
                let _ = write!(self.buf, "func() {iife_ty} {{ if ");
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
                self.current_expected_type = prev_expected;
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
                // `Optional` / `Result` matches dispatch on the runtime tag.
                if go_match_is_optional(arms) {
                    return self.emit_optional_match_expr(scrutinee, arms);
                }
                if go_match_is_result(arms) {
                    return self.emit_result_match_expr(scrutinee, arms);
                }
                // A user-enum match (including the bundled `core.compare.Ordering`
                // enum) dispatches on the dynamic concrete-variant *type*
                // (`OrderingGreater`), so the IIFE must be a *type-switch* — the
                // variant names are Go struct types, not values, so a value-switch
                // (`case OrderingGreater:`) is a compile error. (The prelude
                // `__bockOrdering` value-enum, used when the real enum is NOT
                // bundled, stays a value-switch via the path below.)
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
                // scrutinee and arm bodies are separately typed (and may re-set
                // it via a nested `let`), so it must not leak into them.
                let prev_expected = self.current_expected_type.take();
                let _ = write!(self.buf, "func() {iife_ty} {{ switch ");
                if is_user_enum {
                    // Non-binding type-switch (`switch x.(type)`): the
                    // `core.compare.Ordering` variants are unit (no payload), so
                    // no `__m` binding is needed, which also avoids Go's
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
                            self.buf.push_str("default: return ");
                        } else {
                            self.buf.push_str("case ");
                            self.emit_match_case_condition(pattern)?;
                            self.buf.push_str(": return ");
                        }
                        self.emit_block_as_expr(body)?;
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
        self.buf.push_str("func() interface{} { __opt := ");
        self.emit_expr(scrutinee)?;
        self.buf.push_str("; ");
        self.emit_optional_match_arms(arms, /*as_expr=*/ true, elem.as_deref())?;
        self.buf.push_str(" }()");
        Ok(())
    }

    /// Emit an `Optional` `match` in statement position as an if/else chain on
    /// the runtime tag.
    fn emit_optional_match_stmt(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
    ) -> Result<(), CodegenError> {
        let elem = self.scrutinee_optional_elem(scrutinee);
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}__opt := ");
        self.emit_expr(scrutinee)?;
        self.buf.push('\n');
        self.write_indent();
        self.emit_optional_match_arms(arms, /*as_expr=*/ false, elem.as_deref())?;
        self.buf.push('\n');
        Ok(())
    }

    /// Shared body for [`emit_optional_match_expr`] /
    /// [`emit_optional_match_stmt`]: an if/else chain on the option tag. In
    /// expression mode each arm body is `return`ed; in statement mode the arm
    /// body is emitted as a block.
    fn emit_optional_match_arms(
        &mut self,
        arms: &[AIRNode],
        as_expr: bool,
        some_elem_ty: Option<&str>,
    ) -> Result<(), CodegenError> {
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
                self.buf.push_str("return ");
                self.emit_block_as_expr(body)?;
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
        // Expression mode needs a trailing value if no arm matched. A `;`
        // separates it from the preceding `}` (Go requires a terminator).
        if as_expr {
            self.buf.push_str("; return nil");
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
        self.buf.push_str("func() interface{} { __res := ");
        self.emit_expr(scrutinee)?;
        self.buf.push_str("; ");
        self.emit_result_match_arms(arms, /*as_expr=*/ true, elems.as_ref())?;
        self.buf.push_str(" }()");
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
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}__res := ");
        self.emit_expr(scrutinee)?;
        self.buf.push('\n');
        self.write_indent();
        self.emit_result_match_arms(arms, /*as_expr=*/ false, elems.as_ref())?;
        self.buf.push('\n');
        Ok(())
    }

    /// Shared body for the `Result` match emitters: an if/else chain on the
    /// result tag. `Ok(v)` binds `v` from `__res.v` asserted to the Ok type;
    /// `Err(e)` binds `e` asserted to the Err type. Mirrors
    /// [`Self::emit_optional_match_arms`].
    fn emit_result_match_arms(
        &mut self,
        arms: &[AIRNode],
        as_expr: bool,
        elems: Option<&(String, String)>,
    ) -> Result<(), CodegenError> {
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
                self.buf.push_str("return ");
                self.emit_block_as_expr(body)?;
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
        if as_expr {
            self.buf.push_str("; return nil");
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
        let ind = self.indent_str();
        if user_enum {
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
    fn emit_match_ifchain(
        &mut self,
        scrutinee: &AIRNode,
        arms: &[AIRNode],
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
            let test = self.pattern_test_go(pattern, &root);
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
                    let binds = self.pattern_binds_to_string_go(pattern, &root);
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
            self.pattern_binds_go(pattern, &root)?;
            self.emit_block_body(body)?;
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

    /// Build the boolean test that selects `pat` against the Go expression
    /// `access` (a correctly-typed value at this pattern position). Returns the
    /// empty string for a pattern that always matches (wildcard / bare bind).
    fn pattern_test_go(&self, pat: &AIRNode, access: &str) -> String {
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
                        let child = go_typed_access(f, &format!("{access}.v"));
                        let sub = self.pattern_test_go(f, &child);
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
                String::new()
            }
            NodeKind::TuplePat { elems } => {
                let mut tests = Vec::new();
                for (i, e) in elems.iter().enumerate() {
                    let sub = self.pattern_test_go(e, &format!("{access}.Field{i}"));
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
            NodeKind::OrPat { alternatives } => {
                let alts: Vec<String> = alternatives
                    .iter()
                    .map(|a| {
                        let t = self.pattern_test_go(a, access);
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
        let binds = self.pattern_binds_to_string_go(pat, access);
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
    fn pattern_binds_to_string_go(&self, pat: &AIRNode, access: &str) -> String {
        let mut out = String::new();
        self.collect_binds_go(pat, access, &mut out);
        out
    }

    fn collect_binds_go(&self, pat: &AIRNode, access: &str, out: &mut String) {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => {
                let n = to_camel_case(&name.name);
                let _ = write!(out, "{n} := {access}; _ = {n}; ");
            }
            NodeKind::ConstructorPat { path, fields } => {
                let leaf = path.segments.last().map_or("", |s| s.name.as_str());
                if matches!(leaf, "Some" | "None" | "Ok" | "Err") {
                    if let Some(f) = fields.first() {
                        let child = go_typed_access(f, &format!("{access}.v"));
                        self.collect_binds_go(f, &child, out);
                    }
                } else {
                    // User-enum variant: bind payload fields off the asserted
                    // concrete struct.
                    let variant_ty = self.go_variant_struct(path);
                    for (i, f) in fields.iter().enumerate() {
                        let child = format!("{access}.({variant_ty}).Field{i}");
                        self.collect_binds_go(f, &child, out);
                    }
                }
            }
            NodeKind::RecordPat { path, fields, .. } => {
                let variant_ty = self.go_variant_struct(path);
                for f in fields {
                    let go_field = to_pascal_case(&f.name.name);
                    let child = format!("{access}.({variant_ty}).{go_field}");
                    match &f.pattern {
                        Some(p) => self.collect_binds_go(p, &child, out),
                        None => {
                            let n = to_camel_case(&f.name.name);
                            let _ = write!(out, "{n} := {child}; _ = {n}; ");
                        }
                    }
                }
            }
            NodeKind::TuplePat { elems } => {
                for (i, e) in elems.iter().enumerate() {
                    self.collect_binds_go(e, &format!("{access}.Field{i}"), out);
                }
            }
            NodeKind::OrPat { alternatives } => {
                if let Some(first) = alternatives.first() {
                    self.collect_binds_go(first, access, out);
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
                    let n = to_camel_case(&name.name);
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
                format!("func({}) {}", param_strs.join(", "), self.type_to_go(ret))
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
                format!(
                    "func({}) {}",
                    param_strs.join(", "),
                    self.ast_type_to_go(ret)
                )
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
                // A statement tail (`return`/`break`/`continue`/assignment) is
                // emitted as a statement, never via `emit_expr` (which would
                // fall through to `/* unsupported */` for control-flow nodes).
                if crate::generator::node_is_statement(t) {
                    self.emit_node(t)?;
                    return Ok(());
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
                self.expected_collection_elem = prev_expected;
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
            self.expected_collection_elem = prev_expected;
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

/// Render a literal as a Go value expression — used by the if-chain match
/// lowering to compare a scrutinee against a literal pattern (`<access> == …`).
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
        // `ResultConstruct` now lowers to the tagged Result-runtime constructor
        // `__bockOk(..)` — the same shape the surface `Ok(..)` construction emits
        // and the `Result` match reads — reconciling construction with match.
        let rc = node(
            1,
            NodeKind::ResultConstruct {
                variant: ResultVariant::Ok,
                value: Some(Box::new(int_lit(2, "42"))),
            },
        );
        let out = gen(&module(vec![], vec![rc]));
        assert!(out.contains("__bockOk(42)"), "got: {out}");
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
        assert!(out.contains("__bockErr(\"failed\")"), "got: {out}");
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
                        params: vec![],
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
            out.contains("func (p *Point) Clone() Point {"),
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
        assert_eq!(ctx.map_type_name("Void"), "struct{}");
        assert_eq!(ctx.map_type_name("Any"), "interface{}");
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
                trait_args: vec![],
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
}
