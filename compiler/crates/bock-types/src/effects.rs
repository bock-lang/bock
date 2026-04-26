//! Effect tracking — E-AIR pass.
//!
//! Tracks algebraic-effect usage through the call graph. Verifies that
//! declared effect clauses (`with Log, Clock`) match the effects actually
//! used or propagated from called functions.
//!
//! # Algorithm
//!
//! 1. **Collect** — all top-level function declarations are entered into a map
//!    together with their declared effect clause.
//! 2. **Infer** — for each function, actual effects are collected by walking
//!    the body for `EffectOp` invocations and calls to other known functions.
//! 3. **Propagate** — effects from called functions are added to the caller's
//!    inferred set (one-level; declaration map is built in phase 1).
//! 4. **Check** — declared vs inferred effects are compared per strictness:
//!    - `Sketch` mode: auto-infer (no diagnostics emitted).
//!    - `Development` mode: warn for undeclared effects on *public* functions.
//!    - `Production` mode: error for undeclared effects on *all* functions.
//!
//! # Effect-Capability Correlation
//!
//! IO effects correlate with capabilities (e.g. `Log` → `Io.Stdout`,
//! `Http` → `Io.Network`). The correlation is recorded as a note on
//! the diagnostic for consumption by downstream passes.

use std::collections::{HashMap, HashSet};

use bock_air::{AIRNode, AirInterpolationPart, NodeKind};
use bock_ast::{TypePath, Visibility};
use bock_errors::{DiagnosticBag, DiagnosticCode};

use crate::AIRModule;
pub use bock_air::stubs::EffectRef;

// ─── Diagnostic codes ─────────────────────────────────────────────────────────

const E_UNDECLARED_EFFECT: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 6001,
};
const W_UNDECLARED_EFFECT: DiagnosticCode = DiagnosticCode {
    prefix: 'W',
    number: 6002,
};
const E_PROPAGATED_EFFECT: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 6003,
};
const W_PROPAGATED_EFFECT: DiagnosticCode = DiagnosticCode {
    prefix: 'W',
    number: 6004,
};

// ─── Public types ─────────────────────────────────────────────────────────────

/// Graduated strictness level for effect checking.
///
/// Controls how strictly undeclared or propagated effects are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strictness {
    /// Sketch mode: effects are inferred automatically; no diagnostics emitted.
    ///
    /// Callers need not declare effects — they are computed and accepted as-is.
    Sketch,

    /// Development mode: undeclared effects on *public* functions produce
    /// warnings. Private functions are not checked.
    Development,

    /// Production mode: undeclared effects on *all* functions (public and
    /// private) produce errors.
    Production,
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Convert a [`TypePath`] to an [`EffectRef`] by joining segments with `.`.
fn type_path_to_effect_ref(path: &TypePath) -> EffectRef {
    let name = path
        .segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".");
    EffectRef::new(name)
}

/// Ambient effects are always available and never need to be declared.
///
/// Per spec: `Panic`, `Allocate`, and `Pure` are ambient effects.
fn is_ambient(effect: &EffectRef) -> bool {
    matches!(effect.name.as_str(), "Panic" | "Allocate" | "Pure")
}

/// Returns a best-effort capability name correlated with an IO effect.
///
/// Per the spec, IO effects correlate with platform capabilities. This mapping
/// is used to emit informational notes alongside effect diagnostics.
fn capability_for_effect(effect: &EffectRef) -> Option<String> {
    let name = effect.name.to_lowercase();
    if name.contains("log") || name.contains("print") || name.contains("console") {
        Some("Io.Stdout".into())
    } else if name.contains("http") || name.contains("net") || name.contains("socket") {
        Some("Io.Network".into())
    } else if name.contains("file") || name.contains("fs") || name.contains("disk") {
        Some("Io.FileSystem".into())
    } else if name.contains("clock") || name.contains("time") || name.contains("date") {
        Some("Io.Clock".into())
    } else if name.contains("env") || name.contains("os") || name.contains("process") {
        Some("Io.Process".into())
    } else {
        None
    }
}

// ─── Recursive effect / call collector ───────────────────────────────────────

/// Walk `node` and collect:
/// - `used_effects`: every `EffectOp` effect directly invoked.
/// - `called_fns`: every function name called (for propagation).
///
/// `HandlingBlock` scopes suppress handled effects so they don't propagate.
/// Nested `FnDecl` bodies are treated as opaque.
fn collect_node_effects(
    node: &AIRNode,
    used_effects: &mut HashSet<EffectRef>,
    called_fns: &mut HashSet<String>,
) {
    match &node.kind {
        // Direct effect operation — always records the effect.
        NodeKind::EffectOp { effect, args, .. } => {
            used_effects.insert(type_path_to_effect_ref(effect));
            for arg in args {
                collect_node_effects(&arg.value, used_effects, called_fns);
            }
        }

        // Function call — record callee name for propagation lookup.
        NodeKind::Call {
            callee,
            args,
            type_args,
        } => {
            if let NodeKind::Identifier { name } = &callee.kind {
                called_fns.insert(name.name.clone());
            }
            collect_node_effects(callee, used_effects, called_fns);
            for arg in args {
                collect_node_effects(&arg.value, used_effects, called_fns);
            }
            for ta in type_args {
                collect_node_effects(ta, used_effects, called_fns);
            }
        }

        // Method call — receiver and arguments.
        NodeKind::MethodCall { receiver, args, .. } => {
            collect_node_effects(receiver, used_effects, called_fns);
            for arg in args {
                collect_node_effects(&arg.value, used_effects, called_fns);
            }
        }

        // Handling block: effects handled here do NOT propagate to the caller.
        NodeKind::HandlingBlock { handlers, body } => {
            let handled: HashSet<EffectRef> = handlers
                .iter()
                .map(|h| type_path_to_effect_ref(&h.effect))
                .collect();

            let mut body_effects = HashSet::new();
            let mut body_calls = HashSet::new();
            collect_node_effects(body, &mut body_effects, &mut body_calls);

            // Only propagate effects not suppressed by this handler.
            for e in body_effects {
                if !handled.contains(&e) {
                    used_effects.insert(e);
                }
            }
            called_fns.extend(body_calls);
        }

        // Nested FnDecl: treat as opaque — its effects don't escape.
        NodeKind::FnDecl { .. } => {}

        // Lambda body effects DO propagate to the enclosing function.
        NodeKind::Lambda { body, .. } => {
            collect_node_effects(body, used_effects, called_fns);
        }

        NodeKind::Block { stmts, tail } => {
            for s in stmts {
                collect_node_effects(s, used_effects, called_fns);
            }
            if let Some(t) = tail {
                collect_node_effects(t, used_effects, called_fns);
            }
        }

        NodeKind::LetBinding { value, .. } => {
            collect_node_effects(value, used_effects, called_fns);
        }

        NodeKind::Assign { target, value, .. } => {
            collect_node_effects(target, used_effects, called_fns);
            collect_node_effects(value, used_effects, called_fns);
        }

        NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            collect_node_effects(condition, used_effects, called_fns);
            collect_node_effects(then_block, used_effects, called_fns);
            if let Some(e) = else_block {
                collect_node_effects(e, used_effects, called_fns);
            }
        }

        NodeKind::Guard {
            let_pattern,
            condition,
            else_block,
        } => {
            if let Some(pat) = let_pattern {
                collect_node_effects(pat, used_effects, called_fns);
            }
            collect_node_effects(condition, used_effects, called_fns);
            collect_node_effects(else_block, used_effects, called_fns);
        }

        NodeKind::Match { scrutinee, arms } => {
            collect_node_effects(scrutinee, used_effects, called_fns);
            for arm in arms {
                collect_node_effects(arm, used_effects, called_fns);
            }
        }

        NodeKind::MatchArm { guard, body, .. } => {
            if let Some(g) = guard {
                collect_node_effects(g, used_effects, called_fns);
            }
            collect_node_effects(body, used_effects, called_fns);
        }

        NodeKind::For { iterable, body, .. } => {
            collect_node_effects(iterable, used_effects, called_fns);
            collect_node_effects(body, used_effects, called_fns);
        }

        NodeKind::While { condition, body } => {
            collect_node_effects(condition, used_effects, called_fns);
            collect_node_effects(body, used_effects, called_fns);
        }

        NodeKind::Loop { body } => {
            collect_node_effects(body, used_effects, called_fns);
        }

        NodeKind::Return { value: Some(v) } | NodeKind::Break { value: Some(v) } => {
            collect_node_effects(v, used_effects, called_fns);
        }

        NodeKind::Return { value: None } | NodeKind::Break { value: None } => {}

        NodeKind::BinaryOp { left, right, .. } => {
            collect_node_effects(left, used_effects, called_fns);
            collect_node_effects(right, used_effects, called_fns);
        }

        NodeKind::UnaryOp { operand, .. } => {
            collect_node_effects(operand, used_effects, called_fns);
        }

        NodeKind::FieldAccess { object, .. } => {
            collect_node_effects(object, used_effects, called_fns);
        }

        NodeKind::Index { object, index } => {
            collect_node_effects(object, used_effects, called_fns);
            collect_node_effects(index, used_effects, called_fns);
        }

        NodeKind::Propagate { expr } => {
            collect_node_effects(expr, used_effects, called_fns);
        }

        NodeKind::Await { expr } => {
            collect_node_effects(expr, used_effects, called_fns);
        }

        NodeKind::Borrow { expr } | NodeKind::MutableBorrow { expr } | NodeKind::Move { expr } => {
            collect_node_effects(expr, used_effects, called_fns);
        }

        NodeKind::Pipe { left, right } | NodeKind::Compose { left, right } => {
            collect_node_effects(left, used_effects, called_fns);
            collect_node_effects(right, used_effects, called_fns);
        }

        NodeKind::Range { lo, hi, .. } => {
            collect_node_effects(lo, used_effects, called_fns);
            collect_node_effects(hi, used_effects, called_fns);
        }

        NodeKind::RecordConstruct { fields, spread, .. } => {
            for f in fields {
                if let Some(v) = &f.value {
                    collect_node_effects(v, used_effects, called_fns);
                }
            }
            if let Some(s) = spread {
                collect_node_effects(s, used_effects, called_fns);
            }
        }

        NodeKind::ListLiteral { elems }
        | NodeKind::SetLiteral { elems }
        | NodeKind::TupleLiteral { elems } => {
            for e in elems {
                collect_node_effects(e, used_effects, called_fns);
            }
        }

        NodeKind::MapLiteral { entries } => {
            for e in entries {
                collect_node_effects(&e.key, used_effects, called_fns);
                collect_node_effects(&e.value, used_effects, called_fns);
            }
        }

        NodeKind::Interpolation { parts } => {
            for p in parts {
                if let AirInterpolationPart::Expr(e) = p {
                    collect_node_effects(e, used_effects, called_fns);
                }
            }
        }

        NodeKind::ResultConstruct { value: Some(v), .. } => {
            collect_node_effects(v, used_effects, called_fns);
        }

        NodeKind::ResultConstruct { value: None, .. } => {}

        // Leaf nodes: literals, identifiers, patterns, type exprs, etc.
        _ => {}
    }
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Infer the effects used in a function node without checking declarations.
///
/// Walks the function body and collects all directly-invoked effects via
/// `EffectOp` nodes. This is the inference used in sketch mode and by
/// downstream passes that want to auto-populate effect annotations.
///
/// Returns an empty set if `fn_node` is not a [`NodeKind::FnDecl`].
#[must_use]
pub fn infer_effects(fn_node: &AIRNode) -> HashSet<EffectRef> {
    let mut effects = HashSet::new();
    if let NodeKind::FnDecl { body, .. } = &fn_node.kind {
        let mut called_fns = HashSet::new();
        collect_node_effects(body, &mut effects, &mut called_fns);
    }
    effects
}

/// Track effect usage through the call graph of `module` and emit diagnostics
/// according to `strictness`.
///
/// See the [module-level docs](self) for the algorithm.
///
/// Returns a [`DiagnosticBag`] with all emitted diagnostics.
#[must_use]
pub fn track_effects(module: &AIRModule, strictness: Strictness) -> DiagnosticBag {
    let mut tracker = EffectTracker::new(strictness);
    tracker.collect_declarations(module);
    tracker.check_module(module);
    tracker.diags
}

// ─── Internal tracker ─────────────────────────────────────────────────────────

struct EffectTracker {
    diags: DiagnosticBag,
    strictness: Strictness,
    /// Declared effects keyed by simple function name.
    fn_declared: HashMap<String, HashSet<EffectRef>>,
    /// Composite effect expansions: `IO` → `{Log, Clock}`.
    composite_effects: HashMap<String, HashSet<EffectRef>>,
}

impl EffectTracker {
    fn new(strictness: Strictness) -> Self {
        Self {
            diags: DiagnosticBag::new(),
            strictness,
            fn_declared: HashMap::new(),
            composite_effects: HashMap::new(),
        }
    }

    /// Expand a set of effects by replacing composite effects with their
    /// components. For example, if `IO = Log + Clock`, then `{IO}` becomes
    /// `{Log, Clock}`.
    fn expand_effects(&self, effects: &HashSet<EffectRef>) -> HashSet<EffectRef> {
        let mut expanded = HashSet::new();
        for eff in effects {
            if let Some(components) = self.composite_effects.get(&eff.name) {
                expanded.extend(components.iter().cloned());
            } else {
                expanded.insert(eff.clone());
            }
        }
        expanded
    }

    // ── Phase 1: collect declarations ─────────────────────────────────────

    fn collect_declarations(&mut self, module: &AIRModule) {
        match &module.kind {
            NodeKind::Module { items, .. } => {
                for item in items {
                    self.collect_item_declaration(item);
                }
            }
            _ => self.collect_item_declaration(module),
        }
    }

    fn collect_item_declaration(&mut self, node: &AIRNode) {
        match &node.kind {
            NodeKind::FnDecl {
                name,
                effect_clause,
                ..
            } => {
                let declared: HashSet<EffectRef> =
                    effect_clause.iter().map(type_path_to_effect_ref).collect();
                self.fn_declared.insert(name.name.clone(), declared);
            }
            NodeKind::EffectDecl {
                name, components, ..
            } if !components.is_empty() => {
                let component_refs: HashSet<EffectRef> =
                    components.iter().map(type_path_to_effect_ref).collect();
                self.composite_effects
                    .insert(name.name.clone(), component_refs);
            }
            NodeKind::ImplBlock { methods, .. } | NodeKind::TraitDecl { methods, .. } => {
                for m in methods {
                    self.collect_item_declaration(m);
                }
            }
            NodeKind::ClassDecl { methods, .. } => {
                for m in methods {
                    self.collect_item_declaration(m);
                }
            }
            _ => {}
        }
    }

    // ── Phase 2: check functions ───────────────────────────────────────────

    fn check_module(&mut self, module: &AIRModule) {
        match &module.kind {
            NodeKind::Module { items, .. } => {
                for item in items {
                    self.check_item(item);
                }
            }
            _ => self.check_item(module),
        }
    }

    fn check_item(&mut self, node: &AIRNode) {
        match &node.kind {
            NodeKind::FnDecl { .. } => self.check_fn(node),
            NodeKind::ImplBlock { methods, .. } | NodeKind::TraitDecl { methods, .. } => {
                for m in methods {
                    self.check_item(m);
                }
            }
            NodeKind::ClassDecl { methods, .. } => {
                for m in methods {
                    self.check_item(m);
                }
            }
            _ => {}
        }
    }

    fn check_fn(&mut self, fn_node: &AIRNode) {
        let NodeKind::FnDecl {
            name,
            effect_clause,
            body,
            visibility,
            ..
        } = &fn_node.kind
        else {
            return;
        };

        let fn_span = fn_node.span;
        let fn_name = &name.name;
        let is_public = matches!(visibility, Visibility::Public);

        let raw_declared: HashSet<EffectRef> =
            effect_clause.iter().map(type_path_to_effect_ref).collect();

        // Expand composite effects: `with IO` where `IO = Log + Clock` → `{Log, Clock}`.
        let declared = self.expand_effects(&raw_declared);

        // Collect effects directly used in the body.
        let mut used_effects = HashSet::new();
        let mut called_fns = HashSet::new();
        collect_node_effects(body, &mut used_effects, &mut called_fns);

        // Propagate: effects declared by called functions must also be declared
        // by this function (effect propagation through call graph).
        let mut propagated: HashSet<EffectRef> = HashSet::new();
        for callee in &called_fns {
            if let Some(callee_effects) = self.fn_declared.get(callee) {
                // Expand callee effects in case they also use composites.
                let expanded_callee = self.expand_effects(callee_effects);
                for eff in expanded_callee {
                    if !is_ambient(&eff) && !declared.contains(&eff) {
                        propagated.insert(eff);
                    }
                }
            }
        }

        if self.strictness == Strictness::Sketch {
            // Sketch mode: auto-infer. No diagnostics.
            return;
        }

        let should_check = match self.strictness {
            Strictness::Development => is_public,
            Strictness::Production => true,
            Strictness::Sketch => false,
        };

        if !should_check {
            return;
        }

        let use_errors = self.strictness == Strictness::Production;

        // Check: direct undeclared effects.
        for eff in used_effects
            .iter()
            .filter(|e| !is_ambient(e) && !declared.contains(*e))
        {
            let msg = format!(
                "function `{fn_name}` uses effect `{}` but does not declare it in its `with` clause",
                eff.name
            );
            let code = if use_errors {
                E_UNDECLARED_EFFECT
            } else {
                W_UNDECLARED_EFFECT
            };
            let diag = if use_errors {
                self.diags.error(code, msg, fn_span)
            } else {
                self.diags.warning(code, msg, fn_span)
            };
            if let Some(cap) = capability_for_effect(eff) {
                diag.note(format!(
                    "effect `{}` correlates with capability `{cap}`",
                    eff.name
                ));
            }
        }

        // Check: propagated undeclared effects (from called functions).
        for eff in &propagated {
            let callee_name = called_fns
                .iter()
                .find(|c| self.fn_declared.get(*c).is_some_and(|e| e.contains(eff)))
                .cloned()
                .unwrap_or_default();

            let msg = format!(
                "function `{fn_name}` calls `{callee_name}` which requires effect `{}`, \
                 but `{fn_name}` does not declare it",
                eff.name
            );
            let code = if use_errors {
                E_PROPAGATED_EFFECT
            } else {
                W_PROPAGATED_EFFECT
            };
            let diag = if use_errors {
                self.diags.error(code, msg, fn_span)
            } else {
                self.diags.warning(code, msg, fn_span)
            };
            if let Some(cap) = capability_for_effect(eff) {
                diag.note(format!(
                    "effect `{}` correlates with capability `{cap}`",
                    eff.name
                ));
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{AIRNode, AirHandlerPair, NodeIdGen, NodeKind};
    use bock_ast::{Ident, TypePath, Visibility};
    use bock_errors::{FileId, Severity, Span};

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }

    fn dummy_ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: dummy_span(),
        }
    }

    fn dummy_type_path(name: &str) -> TypePath {
        TypePath {
            segments: vec![dummy_ident(name)],
            span: dummy_span(),
        }
    }

    fn make_node(gen: &NodeIdGen, kind: NodeKind) -> AIRNode {
        AIRNode::new(gen.next(), dummy_span(), kind)
    }

    fn make_fn(
        gen: &NodeIdGen,
        name: &str,
        effects: Vec<&str>,
        body: AIRNode,
        vis: Visibility,
    ) -> AIRNode {
        make_node(
            gen,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: vis,
                is_async: false,
                name: dummy_ident(name),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: effects.into_iter().map(dummy_type_path).collect(),
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    fn make_effect_op(gen: &NodeIdGen, effect: &str) -> AIRNode {
        make_node(
            gen,
            NodeKind::EffectOp {
                effect: dummy_type_path(effect),
                operation: dummy_ident("op"),
                args: vec![],
            },
        )
    }

    fn make_module(gen: &NodeIdGen, items: Vec<AIRNode>) -> AIRNode {
        make_node(
            gen,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items,
            },
        )
    }

    fn empty_block(gen: &NodeIdGen) -> AIRNode {
        make_node(
            gen,
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        )
    }

    fn warning_count(bag: &DiagnosticBag) -> usize {
        bag.iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    // ── infer_effects ─────────────────────────────────────────────────────────

    #[test]
    fn infer_effects_empty_body() {
        let gen = NodeIdGen::new();
        let body = empty_block(&gen);
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Private);
        let effects = infer_effects(&fn_node);
        assert!(effects.is_empty());
    }

    #[test]
    fn infer_effects_direct_effect_op() {
        let gen = NodeIdGen::new();
        let op = make_effect_op(&gen, "Log");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Private);
        let effects = infer_effects(&fn_node);
        assert!(effects.contains(&EffectRef::new("Log")));
    }

    #[test]
    fn infer_effects_multiple_effects() {
        let gen = NodeIdGen::new();
        let log_op = make_effect_op(&gen, "Log");
        let clock_op = make_effect_op(&gen, "Clock");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![log_op, clock_op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Private);
        let effects = infer_effects(&fn_node);
        assert_eq!(effects.len(), 2);
        assert!(effects.contains(&EffectRef::new("Log")));
        assert!(effects.contains(&EffectRef::new("Clock")));
    }

    #[test]
    fn infer_effects_handling_block_suppresses_handled() {
        let gen = NodeIdGen::new();
        let op = make_effect_op(&gen, "Log");
        let inner_body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let handling = make_node(
            &gen,
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: dummy_type_path("Log"),
                    handler: Box::new(empty_block(&gen)),
                }],
                body: Box::new(inner_body),
            },
        );
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![handling],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Private);
        // Log is handled inside the block — must NOT appear in inferred set.
        assert!(!infer_effects(&fn_node).contains(&EffectRef::new("Log")));
    }

    #[test]
    fn infer_effects_returns_empty_for_non_fn() {
        let gen = NodeIdGen::new();
        let node = empty_block(&gen);
        assert!(infer_effects(&node).is_empty());
    }

    // ── track_effects ─────────────────────────────────────────────────────────

    #[test]
    fn sketch_mode_no_diagnostics_for_undeclared() {
        let gen = NodeIdGen::new();
        let op = make_effect_op(&gen, "Log");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Public);
        let module = make_module(&gen, vec![fn_node]);

        let bag = track_effects(&module, Strictness::Sketch);
        assert_eq!(bag.error_count(), 0);
        assert_eq!(warning_count(&bag), 0);
    }

    #[test]
    fn dev_mode_warns_public_undeclared() {
        let gen = NodeIdGen::new();
        let op = make_effect_op(&gen, "Log");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Public);
        let module = make_module(&gen, vec![fn_node]);

        let bag = track_effects(&module, Strictness::Development);
        assert_eq!(bag.error_count(), 0);
        assert!(warning_count(&bag) > 0);
    }

    #[test]
    fn dev_mode_no_warning_for_private() {
        let gen = NodeIdGen::new();
        let op = make_effect_op(&gen, "Log");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Private);
        let module = make_module(&gen, vec![fn_node]);

        let bag = track_effects(&module, Strictness::Development);
        assert_eq!(bag.error_count(), 0);
        assert_eq!(warning_count(&bag), 0);
    }

    #[test]
    fn prod_mode_errors_all_undeclared() {
        let gen = NodeIdGen::new();
        let op = make_effect_op(&gen, "Log");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Private);
        let module = make_module(&gen, vec![fn_node]);

        let bag = track_effects(&module, Strictness::Production);
        assert!(bag.error_count() > 0);
    }

    #[test]
    fn declared_effect_produces_no_diagnostic() {
        let gen = NodeIdGen::new();
        let op = make_effect_op(&gen, "Log");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec!["Log"], body, Visibility::Public);
        let module = make_module(&gen, vec![fn_node]);

        let bag = track_effects(&module, Strictness::Production);
        assert_eq!(bag.error_count(), 0);
    }

    #[test]
    fn ambient_effect_never_flagged() {
        let gen = NodeIdGen::new();
        let op = make_effect_op(&gen, "Panic");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Public);
        let module = make_module(&gen, vec![fn_node]);

        let bag = track_effects(&module, Strictness::Production);
        assert_eq!(bag.error_count(), 0);
    }

    #[test]
    fn propagation_caller_must_declare_callee_effects() {
        let gen = NodeIdGen::new();

        // callee declares Log
        let callee_body = empty_block(&gen);
        let callee = make_fn(&gen, "callee", vec!["Log"], callee_body, Visibility::Public);

        // caller calls callee but doesn't declare Log
        let call_node = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(make_node(
                    &gen,
                    NodeKind::Identifier {
                        name: dummy_ident("callee"),
                    },
                )),
                args: vec![],
                type_args: vec![],
            },
        );
        let caller_body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![call_node],
                tail: None,
            },
        );
        let caller = make_fn(&gen, "caller", vec![], caller_body, Visibility::Public);

        let module = make_module(&gen, vec![callee, caller]);
        let bag = track_effects(&module, Strictness::Production);
        assert!(bag.error_count() > 0);
    }

    #[test]
    fn propagation_caller_declares_callee_effects_ok() {
        let gen = NodeIdGen::new();

        let callee_body = empty_block(&gen);
        let callee = make_fn(&gen, "callee", vec!["Log"], callee_body, Visibility::Public);

        let call_node = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(make_node(
                    &gen,
                    NodeKind::Identifier {
                        name: dummy_ident("callee"),
                    },
                )),
                args: vec![],
                type_args: vec![],
            },
        );
        let caller_body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![call_node],
                tail: None,
            },
        );
        // caller also declares Log
        let caller = make_fn(&gen, "caller", vec!["Log"], caller_body, Visibility::Public);

        let module = make_module(&gen, vec![callee, caller]);
        let bag = track_effects(&module, Strictness::Production);
        assert_eq!(bag.error_count(), 0);
    }

    // ── Composite effect expansion ───────────────────────────────────────────

    fn make_effect_decl(gen: &NodeIdGen, name: &str, components: Vec<&str>) -> AIRNode {
        make_node(
            gen,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: dummy_ident(name),
                generic_params: vec![],
                components: components.into_iter().map(dummy_type_path).collect(),
                operations: vec![],
            },
        )
    }

    #[test]
    fn composite_effect_expands_to_components() {
        let gen = NodeIdGen::new();

        // effect IO = Log + Clock
        let io_decl = make_effect_decl(&gen, "IO", vec!["Log", "Clock"]);

        // fn f() with IO { perform Log.op }
        let op = make_effect_op(&gen, "Log");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec!["IO"], body, Visibility::Public);

        let module = make_module(&gen, vec![io_decl, fn_node]);
        let bag = track_effects(&module, Strictness::Production);
        // Log is covered by IO = Log + Clock, so no error.
        assert_eq!(bag.error_count(), 0);
    }

    #[test]
    fn composite_effect_covers_all_components() {
        let gen = NodeIdGen::new();

        // effect IO = Log + Clock
        let io_decl = make_effect_decl(&gen, "IO", vec!["Log", "Clock"]);

        // fn f() with IO { perform Log.op; perform Clock.op }
        let log_op = make_effect_op(&gen, "Log");
        let clock_op = make_effect_op(&gen, "Clock");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![log_op, clock_op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec!["IO"], body, Visibility::Public);

        let module = make_module(&gen, vec![io_decl, fn_node]);
        let bag = track_effects(&module, Strictness::Production);
        assert_eq!(bag.error_count(), 0);
    }

    #[test]
    fn composite_effect_does_not_cover_unrelated() {
        let gen = NodeIdGen::new();

        // effect IO = Log + Clock
        let io_decl = make_effect_decl(&gen, "IO", vec!["Log", "Clock"]);

        // fn f() with IO { perform Http.op } — Http not in IO
        let op = make_effect_op(&gen, "Http");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec!["IO"], body, Visibility::Public);

        let module = make_module(&gen, vec![io_decl, fn_node]);
        let bag = track_effects(&module, Strictness::Production);
        assert!(bag.error_count() > 0);
    }

    #[test]
    fn composite_effect_propagation_through_call_graph() {
        let gen = NodeIdGen::new();

        // effect IO = Log + Clock
        let io_decl = make_effect_decl(&gen, "IO", vec!["Log", "Clock"]);

        // fn callee() with Log { ... }
        let callee_body = empty_block(&gen);
        let callee = make_fn(&gen, "callee", vec!["Log"], callee_body, Visibility::Public);

        // fn caller() with IO { callee() }
        // IO expands to Log + Clock, which covers callee's Log.
        let call_node = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(make_node(
                    &gen,
                    NodeKind::Identifier {
                        name: dummy_ident("callee"),
                    },
                )),
                args: vec![],
                type_args: vec![],
            },
        );
        let caller_body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![call_node],
                tail: None,
            },
        );
        let caller = make_fn(&gen, "caller", vec!["IO"], caller_body, Visibility::Public);

        let module = make_module(&gen, vec![io_decl, callee, caller]);
        let bag = track_effects(&module, Strictness::Production);
        assert_eq!(bag.error_count(), 0);
    }
}
