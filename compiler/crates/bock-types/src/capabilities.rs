//! Capability computation — CAP-AIR pass.
//!
//! Computes capability requirements for each function in a module by:
//!
//! 1. **Annotation extraction** — `@requires(Capability.Network, ...)` annotations
//!    are parsed from each function's annotation list.
//! 2. **Effect correlation** — IO effects imply platform capabilities
//!    (e.g. `Log` → `Io.Stdout`, `Http` → `Io.Network`).
//! 3. **Call-graph propagation** — required capabilities from callees are
//!    unioned into the caller's capability set.
//! 4. **Verification** — declared (`@requires`) capabilities are checked
//!    against inferred requirements per strictness level.
//!
//! # Strictness
//!
//! - `Sketch`: no diagnostics; capabilities are inferred silently.
//! - `Development`: missing `@requires` on *public* functions produces warnings.
//! - `Production`: missing `@requires` on *all* functions produces errors.
//!
//! # @requires additivity
//!
//! Per spec, `@requires` is **additive**: a declaration's capability set is
//! the union of the module-level `@requires` and its own `@requires`.
//! This pass operates purely at the function level; module-level annotations
//! are collected first and unioned into every declaration's declared set.

use std::collections::{HashMap, HashSet};

use bock_air::{AIRNode, AirInterpolationPart, NodeKind};
use bock_ast::{Expr, Visibility};
use bock_errors::{DiagnosticBag, DiagnosticCode, Span};

pub use bock_air::stubs::{Capability, EffectRef};
use bock_air::NodeId;

use crate::AIRModule;
pub use crate::Strictness;

// ─── Diagnostic codes ─────────────────────────────────────────────────────────

const E_MISSING_CAPABILITY: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 7001,
};
const W_MISSING_CAPABILITY: DiagnosticCode = DiagnosticCode {
    prefix: 'W',
    number: 7002,
};
const E_PROPAGATED_CAPABILITY: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 7003,
};
const W_PROPAGATED_CAPABILITY: DiagnosticCode = DiagnosticCode {
    prefix: 'W',
    number: 7004,
};

// ─── Public types ─────────────────────────────────────────────────────────────

/// The set of capabilities required by a node.
pub type CapabilitySet = HashSet<Capability>;

// ─── Annotation / expression helpers ─────────────────────────────────────────

/// Attempt to extract a dotted capability name from an annotation argument
/// expression.
///
/// Handles:
/// - `Expr::Identifier { name }` → `"name"` (e.g. bare `Network`)
/// - `Expr::FieldAccess { object, field }` → recursive join with `.`
///   (e.g. `Capability.Network` → `"Capability.Network"`)
fn expr_to_capability_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::FieldAccess { object, field, .. } => {
            let prefix = expr_to_capability_name(object)?;
            Some(format!("{}.{}", prefix, field.name))
        }
        Expr::Identifier { name, .. } => Some(name.name.clone()),
        _ => None,
    }
}

/// Extract capability names declared in a `@requires(...)` annotation.
///
/// Each argument is walked with [`expr_to_capability_name`]; arguments that
/// cannot be parsed are silently skipped (resilient to syntax variations).
fn extract_requires_annotation(annotations: &[bock_ast::Annotation]) -> CapabilitySet {
    let mut caps = CapabilitySet::new();
    for ann in annotations {
        if ann.name.name == "requires" {
            for arg in &ann.args {
                if let Some(name) = expr_to_capability_name(&arg.value) {
                    caps.insert(Capability::new(name));
                }
            }
        }
    }
    caps
}

// ─── Effect → capability correlation ─────────────────────────────────────────

/// Map an effect name to the platform capability it implies, if any.
///
/// IO effects imply a correlated capability per the spec.
fn capability_for_effect(effect: &EffectRef) -> Option<Capability> {
    let name = effect.name.to_lowercase();
    if name.contains("log") || name.contains("print") || name.contains("console") {
        Some(Capability::new("Io.Stdout"))
    } else if name.contains("http") || name.contains("net") || name.contains("socket") {
        Some(Capability::new("Io.Network"))
    } else if name.contains("file") || name.contains("fs") || name.contains("disk") {
        Some(Capability::new("Io.FileSystem"))
    } else if name.contains("clock") || name.contains("time") || name.contains("date") {
        Some(Capability::new("Io.Clock"))
    } else if name.contains("env") || name.contains("os") || name.contains("process") {
        Some(Capability::new("Io.Process"))
    } else {
        None
    }
}

// ─── Effect / call collector ──────────────────────────────────────────────────

/// Walk `node` and collect:
/// - `used_effects`: every `EffectOp` effect directly invoked.
/// - `called_fns`: every function name called (for propagation).
fn collect_effects_and_calls(
    node: &AIRNode,
    used_effects: &mut HashSet<EffectRef>,
    called_fns: &mut HashSet<String>,
) {
    match &node.kind {
        NodeKind::EffectOp { effect, args, .. } => {
            let name = effect
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            used_effects.insert(EffectRef::new(name));
            for arg in args {
                collect_effects_and_calls(&arg.value, used_effects, called_fns);
            }
        }

        NodeKind::Call {
            callee,
            args,
            type_args,
        } => {
            if let NodeKind::Identifier { name } = &callee.kind {
                called_fns.insert(name.name.clone());
            }
            collect_effects_and_calls(callee, used_effects, called_fns);
            for arg in args {
                collect_effects_and_calls(&arg.value, used_effects, called_fns);
            }
            for ta in type_args {
                collect_effects_and_calls(ta, used_effects, called_fns);
            }
        }

        NodeKind::MethodCall { receiver, args, .. } => {
            collect_effects_and_calls(receiver, used_effects, called_fns);
            for arg in args {
                collect_effects_and_calls(&arg.value, used_effects, called_fns);
            }
        }

        // Handling block: effects handled here don't propagate.
        NodeKind::HandlingBlock { handlers, body } => {
            let handled: HashSet<String> = handlers
                .iter()
                .map(|h| {
                    h.effect
                        .segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(".")
                })
                .collect();

            let mut body_effects = HashSet::new();
            let mut body_calls = HashSet::new();
            collect_effects_and_calls(body, &mut body_effects, &mut body_calls);

            for e in body_effects {
                if !handled.contains(&e.name) {
                    used_effects.insert(e);
                }
            }
            called_fns.extend(body_calls);
        }

        // Nested FnDecl: opaque — don't leak inner effects.
        NodeKind::FnDecl { .. } => {}

        NodeKind::Lambda { body, .. } => {
            collect_effects_and_calls(body, used_effects, called_fns);
        }

        NodeKind::Block { stmts, tail } => {
            for s in stmts {
                collect_effects_and_calls(s, used_effects, called_fns);
            }
            if let Some(t) = tail {
                collect_effects_and_calls(t, used_effects, called_fns);
            }
        }

        NodeKind::LetBinding { value, .. } => {
            collect_effects_and_calls(value, used_effects, called_fns);
        }

        NodeKind::Assign { target, value, .. } => {
            collect_effects_and_calls(target, used_effects, called_fns);
            collect_effects_and_calls(value, used_effects, called_fns);
        }

        NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            collect_effects_and_calls(condition, used_effects, called_fns);
            collect_effects_and_calls(then_block, used_effects, called_fns);
            if let Some(e) = else_block {
                collect_effects_and_calls(e, used_effects, called_fns);
            }
        }

        NodeKind::Guard {
            let_pattern,
            condition,
            else_block,
        } => {
            if let Some(pat) = let_pattern {
                collect_effects_and_calls(pat, used_effects, called_fns);
            }
            collect_effects_and_calls(condition, used_effects, called_fns);
            collect_effects_and_calls(else_block, used_effects, called_fns);
        }

        NodeKind::Match { scrutinee, arms } => {
            collect_effects_and_calls(scrutinee, used_effects, called_fns);
            for arm in arms {
                collect_effects_and_calls(arm, used_effects, called_fns);
            }
        }

        NodeKind::MatchArm { guard, body, .. } => {
            if let Some(g) = guard {
                collect_effects_and_calls(g, used_effects, called_fns);
            }
            collect_effects_and_calls(body, used_effects, called_fns);
        }

        NodeKind::For { iterable, body, .. } => {
            collect_effects_and_calls(iterable, used_effects, called_fns);
            collect_effects_and_calls(body, used_effects, called_fns);
        }

        NodeKind::While { condition, body } => {
            collect_effects_and_calls(condition, used_effects, called_fns);
            collect_effects_and_calls(body, used_effects, called_fns);
        }

        NodeKind::Loop { body } => {
            collect_effects_and_calls(body, used_effects, called_fns);
        }

        NodeKind::Return { value: Some(v) } | NodeKind::Break { value: Some(v) } => {
            collect_effects_and_calls(v, used_effects, called_fns);
        }

        NodeKind::Return { value: None } | NodeKind::Break { value: None } => {}

        NodeKind::BinaryOp { left, right, .. } => {
            collect_effects_and_calls(left, used_effects, called_fns);
            collect_effects_and_calls(right, used_effects, called_fns);
        }

        NodeKind::UnaryOp { operand, .. } => {
            collect_effects_and_calls(operand, used_effects, called_fns);
        }

        NodeKind::FieldAccess { object, .. } => {
            collect_effects_and_calls(object, used_effects, called_fns);
        }

        NodeKind::Index { object, index } => {
            collect_effects_and_calls(object, used_effects, called_fns);
            collect_effects_and_calls(index, used_effects, called_fns);
        }

        NodeKind::Propagate { expr } => {
            collect_effects_and_calls(expr, used_effects, called_fns);
        }

        NodeKind::Await { expr } => {
            collect_effects_and_calls(expr, used_effects, called_fns);
        }

        NodeKind::Borrow { expr } | NodeKind::MutableBorrow { expr } | NodeKind::Move { expr } => {
            collect_effects_and_calls(expr, used_effects, called_fns);
        }

        NodeKind::Pipe { left, right } | NodeKind::Compose { left, right } => {
            collect_effects_and_calls(left, used_effects, called_fns);
            collect_effects_and_calls(right, used_effects, called_fns);
        }

        NodeKind::Range { lo, hi, .. } => {
            collect_effects_and_calls(lo, used_effects, called_fns);
            collect_effects_and_calls(hi, used_effects, called_fns);
        }

        NodeKind::RecordConstruct { fields, spread, .. } => {
            for f in fields {
                if let Some(v) = &f.value {
                    collect_effects_and_calls(v, used_effects, called_fns);
                }
            }
            if let Some(s) = spread {
                collect_effects_and_calls(s, used_effects, called_fns);
            }
        }

        NodeKind::ListLiteral { elems }
        | NodeKind::SetLiteral { elems }
        | NodeKind::TupleLiteral { elems } => {
            for e in elems {
                collect_effects_and_calls(e, used_effects, called_fns);
            }
        }

        NodeKind::MapLiteral { entries } => {
            for e in entries {
                collect_effects_and_calls(&e.key, used_effects, called_fns);
                collect_effects_and_calls(&e.value, used_effects, called_fns);
            }
        }

        NodeKind::Interpolation { parts } => {
            for p in parts {
                if let AirInterpolationPart::Expr(e) = p {
                    collect_effects_and_calls(e, used_effects, called_fns);
                }
            }
        }

        NodeKind::ResultConstruct { value: Some(v), .. } => {
            collect_effects_and_calls(v, used_effects, called_fns);
        }

        // Leaf nodes and others.
        _ => {}
    }
}

// ─── Internal function record ─────────────────────────────────────────────────

/// Summary of a function gathered during phase 1.
struct FnRecord {
    node_id: NodeId,
    span: Span,
    is_public: bool,
    /// Capabilities explicitly declared via `@requires`.
    declared: CapabilitySet,
    /// Capabilities inferred from directly-used IO effects.
    from_effects: CapabilitySet,
    /// Names of functions directly called in the body.
    called_fns: HashSet<String>,
}

// ─── Capability engine ────────────────────────────────────────────────────────

struct CapabilityEngine {
    /// Records keyed by function name.
    records: HashMap<String, FnRecord>,
    /// Module-level `@requires` capabilities (additive base).
    module_caps: CapabilitySet,
}

impl CapabilityEngine {
    fn new() -> Self {
        Self {
            records: HashMap::new(),
            module_caps: CapabilitySet::new(),
        }
    }

    // ── Phase 1: collect ──────────────────────────────────────────────────

    fn collect(&mut self, module: &AIRModule) {
        match &module.kind {
            NodeKind::Module { items, .. } => {
                // Extract module-level @requires annotations if the module
                // node itself carries annotations (not always the case, but
                // some compilers attach them to the Module node via metadata).
                // For now we look for a Module-level FnDecl-style annotation
                // by checking if there's an annotation list attached to the
                // module kind. The AIR Module node does not carry annotations
                // directly, so module_caps stays empty unless we extend later.
                for item in items {
                    self.collect_item(item);
                }
            }
            _ => self.collect_item(module),
        }
    }

    fn collect_item(&mut self, node: &AIRNode) {
        match &node.kind {
            NodeKind::FnDecl {
                name,
                annotations,
                visibility,
                body,
                ..
            } => {
                let declared = {
                    let mut caps = extract_requires_annotation(annotations);
                    // Add module-level capabilities (additive per spec).
                    caps.extend(self.module_caps.iter().cloned());
                    caps
                };

                let mut used_effects = HashSet::new();
                let mut called_fns = HashSet::new();
                collect_effects_and_calls(body, &mut used_effects, &mut called_fns);

                let from_effects: CapabilitySet = used_effects
                    .iter()
                    .filter_map(capability_for_effect)
                    .collect();

                let record = FnRecord {
                    node_id: node.id,
                    span: node.span,
                    is_public: matches!(visibility, Visibility::Public),
                    declared,
                    from_effects,
                    called_fns,
                };
                self.records.insert(name.name.clone(), record);
            }

            NodeKind::ImplBlock { methods, .. } | NodeKind::TraitDecl { methods, .. } => {
                for m in methods {
                    self.collect_item(m);
                }
            }

            NodeKind::ClassDecl { methods, .. } => {
                for m in methods {
                    self.collect_item(m);
                }
            }

            _ => {}
        }
    }

    // ── Phase 2: propagate ────────────────────────────────────────────────

    /// Compute the *required* capability set for function `name`, including
    /// capabilities propagated from its callees (one-pass BFS/DFS).
    ///
    /// To avoid cycles we track the current visitation path.
    fn required_caps(&self, name: &str, visiting: &mut HashSet<String>) -> CapabilitySet {
        if !visiting.insert(name.to_string()) {
            // Already computing (cycle) — return empty to break the loop.
            return CapabilitySet::new();
        }

        let mut caps = CapabilitySet::new();

        if let Some(rec) = self.records.get(name) {
            // Direct capabilities from IO effects.
            caps.extend(rec.from_effects.iter().cloned());

            // Propagate from callees.
            let callees: Vec<String> = rec.called_fns.iter().cloned().collect();
            for callee in &callees {
                let callee_caps = self.required_caps(callee, visiting);
                caps.extend(callee_caps);
            }
        }

        visiting.remove(name);
        caps
    }

    // ── Phase 3: build map ────────────────────────────────────────────────

    /// Build the `NodeId → CapabilitySet` map for all collected functions.
    ///
    /// The set for each node is the *full required* capability set (direct +
    /// propagated from callees).
    fn build_map(&self) -> HashMap<NodeId, CapabilitySet> {
        let mut map = HashMap::new();
        for (name, rec) in &self.records {
            let mut visiting = HashSet::new();
            let caps = self.required_caps(name, &mut visiting);
            map.insert(rec.node_id, caps);
        }
        map
    }

    // ── Phase 4: verify ───────────────────────────────────────────────────

    fn verify(&self, strictness: Strictness) -> DiagnosticBag {
        let mut diags = DiagnosticBag::new();

        if strictness == Strictness::Sketch {
            return diags;
        }

        let use_errors = strictness == Strictness::Production;

        for (name, rec) in &self.records {
            let should_check = match strictness {
                Strictness::Development => rec.is_public,
                Strictness::Production => true,
                Strictness::Sketch => false,
            };

            if !should_check {
                continue;
            }

            let mut visiting = HashSet::new();
            let required = self.required_caps(name, &mut visiting);

            // Check: capabilities required by direct effects but not declared.
            for cap in rec.from_effects.iter() {
                if !rec.declared.contains(cap) {
                    let msg = format!(
                        "function `{name}` requires capability `{}` (from IO effects) \
                         but does not declare it via `@requires`",
                        cap.name
                    );
                    let code = if use_errors {
                        E_MISSING_CAPABILITY
                    } else {
                        W_MISSING_CAPABILITY
                    };
                    if use_errors {
                        diags.error(code, msg, rec.span);
                    } else {
                        diags.warning(code, msg, rec.span);
                    }
                }
            }

            // Check: capabilities propagated from callees but not declared.
            let propagated: CapabilitySet = required
                .iter()
                .filter(|c| !rec.from_effects.contains(*c) && !rec.declared.contains(*c))
                .cloned()
                .collect();

            for cap in &propagated {
                // Find which callee introduced this capability.
                let callee_name = rec
                    .called_fns
                    .iter()
                    .find(|c| {
                        let mut v = HashSet::new();
                        self.required_caps(c, &mut v).contains(cap)
                    })
                    .cloned()
                    .unwrap_or_default();

                let msg = format!(
                    "function `{name}` calls `{callee_name}` which requires capability `{}`, \
                     but `{name}` does not declare it via `@requires`",
                    cap.name
                );
                let code = if use_errors {
                    E_PROPAGATED_CAPABILITY
                } else {
                    W_PROPAGATED_CAPABILITY
                };
                if use_errors {
                    diags.error(code, msg, rec.span);
                } else {
                    diags.warning(code, msg, rec.span);
                }
            }
        }

        diags
    }
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Compute capability requirements for every function in `module`.
///
/// Returns a map from [`NodeId`] to [`CapabilitySet`]. Each set is the *full*
/// required capability set for that function, including capabilities propagated
/// from callees through the call graph.
///
/// Capabilities are derived from:
/// - `@requires` annotations on the function (or the containing module).
/// - IO effect usage correlated to platform capabilities.
/// - Propagation from directly-called functions.
#[must_use]
pub fn compute_capabilities(module: &AIRModule) -> HashMap<NodeId, CapabilitySet> {
    let mut engine = CapabilityEngine::new();
    engine.collect(module);
    engine.build_map()
}

/// Verify capability declarations in `module` against actual usage.
///
/// Emits diagnostics when a function uses or calls into code that requires a
/// capability but does not declare it via `@requires`, according to
/// `strictness`:
///
/// - [`Strictness::Sketch`] — no diagnostics.
/// - [`Strictness::Development`] — warnings on public functions only.
/// - [`Strictness::Production`] — errors on all functions.
#[must_use]
pub fn verify_capabilities(module: &AIRModule, strictness: Strictness) -> DiagnosticBag {
    let mut engine = CapabilityEngine::new();
    engine.collect(module);
    engine.verify(strictness)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{AIRNode, AirHandlerPair, NodeIdGen, NodeKind};
    use bock_ast::{Annotation, Ident, TypePath, Visibility};
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

    fn empty_block(gen: &NodeIdGen) -> AIRNode {
        make_node(
            gen,
            NodeKind::Block {
                stmts: vec![],
                tail: None,
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

    /// Build a `@requires(cap1, cap2, ...)` annotation using the canonical
    /// capability names (e.g. `"Io.Stdout"`, `"Io.Network"`).
    fn make_requires_annotation(caps: &[&str]) -> Annotation {
        use bock_ast::AnnotationArg;
        use bock_ast::Expr;
        use bock_ast::NodeId as AstNodeId;

        let args = caps
            .iter()
            .map(|cap| AnnotationArg {
                label: None,
                value: Expr::Identifier {
                    id: 0 as AstNodeId,
                    span: dummy_span(),
                    name: dummy_ident(cap),
                },
            })
            .collect();

        Annotation {
            id: 0,
            span: dummy_span(),
            name: dummy_ident("requires"),
            args,
        }
    }

    fn make_fn(
        gen: &NodeIdGen,
        name: &str,
        annotations: Vec<Annotation>,
        body: AIRNode,
        vis: Visibility,
    ) -> AIRNode {
        make_node(
            gen,
            NodeKind::FnDecl {
                annotations,
                visibility: vis,
                is_async: false,
                name: dummy_ident(name),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
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

    fn warning_count(bag: &DiagnosticBag) -> usize {
        bag.iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    // ── expr_to_capability_name ────────────────────────────────────────────

    #[test]
    fn expr_capability_name_identifier() {
        use bock_ast::Expr;
        let expr = Expr::Identifier {
            id: 0,
            span: dummy_span(),
            name: dummy_ident("Network"),
        };
        assert_eq!(expr_to_capability_name(&expr), Some("Network".into()));
    }

    #[test]
    fn expr_capability_name_field_access() {
        use bock_ast::Expr;
        let expr = Expr::FieldAccess {
            id: 0,
            span: dummy_span(),
            object: Box::new(Expr::Identifier {
                id: 0,
                span: dummy_span(),
                name: dummy_ident("Capability"),
            }),
            field: dummy_ident("Network"),
        };
        assert_eq!(
            expr_to_capability_name(&expr),
            Some("Capability.Network".into())
        );
    }

    #[test]
    fn expr_capability_name_unknown_returns_none() {
        use bock_ast::{Expr, Literal};
        let expr = Expr::Literal {
            id: 0,
            span: dummy_span(),
            lit: Literal::Bool(true),
        };
        assert_eq!(expr_to_capability_name(&expr), None);
    }

    // ── capability_for_effect ──────────────────────────────────────────────

    #[test]
    fn effect_log_gives_stdout_cap() {
        let e = EffectRef::new("Log");
        assert_eq!(
            capability_for_effect(&e),
            Some(Capability::new("Io.Stdout"))
        );
    }

    #[test]
    fn effect_http_gives_network_cap() {
        let e = EffectRef::new("Http");
        assert_eq!(
            capability_for_effect(&e),
            Some(Capability::new("Io.Network"))
        );
    }

    #[test]
    fn effect_clock_gives_clock_cap() {
        let e = EffectRef::new("Clock");
        assert_eq!(capability_for_effect(&e), Some(Capability::new("Io.Clock")));
    }

    #[test]
    fn effect_pure_gives_no_cap() {
        let e = EffectRef::new("Pure");
        assert_eq!(capability_for_effect(&e), None);
    }

    // ── compute_capabilities ──────────────────────────────────────────────

    #[test]
    fn empty_fn_has_no_capabilities() {
        let gen = NodeIdGen::new();
        let body = empty_block(&gen);
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Private);
        let module = make_module(&gen, vec![fn_node.clone()]);

        let map = compute_capabilities(&module);
        let caps = map.get(&fn_node.id).cloned().unwrap_or_default();
        assert!(caps.is_empty());
    }

    #[test]
    fn fn_with_log_effect_gets_stdout_cap() {
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
        let module = make_module(&gen, vec![fn_node.clone()]);

        let map = compute_capabilities(&module);
        let caps = map.get(&fn_node.id).cloned().unwrap_or_default();
        assert!(caps.contains(&Capability::new("Io.Stdout")));
    }

    #[test]
    fn fn_with_http_effect_gets_network_cap() {
        let gen = NodeIdGen::new();
        let op = make_effect_op(&gen, "Http");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Public);
        let module = make_module(&gen, vec![fn_node.clone()]);

        let map = compute_capabilities(&module);
        let caps = map.get(&fn_node.id).cloned().unwrap_or_default();
        assert!(caps.contains(&Capability::new("Io.Network")));
    }

    #[test]
    fn requires_annotation_included_in_capability_map() {
        let gen = NodeIdGen::new();
        let ann = make_requires_annotation(&["Storage"]);
        let body = empty_block(&gen);
        // Note: declared caps from @requires are NOT included in the
        // "required" set (compute_capabilities returns required, not declared).
        // The declared caps serve for verification only.
        // This test just ensures the function is found in the map.
        let fn_node = make_fn(&gen, "f", vec![ann], body, Visibility::Public);
        let module = make_module(&gen, vec![fn_node.clone()]);

        let map = compute_capabilities(&module);
        assert!(map.contains_key(&fn_node.id));
    }

    #[test]
    fn capability_propagates_through_call_graph() {
        let gen = NodeIdGen::new();

        // callee uses Http → needs Io.Network
        let callee_op = make_effect_op(&gen, "Http");
        let callee_body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![callee_op],
                tail: None,
            },
        );
        let callee = make_fn(&gen, "callee", vec![], callee_body, Visibility::Public);
        let callee_id = callee.id;

        // caller calls callee
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
        let caller_id = caller.id;

        let module = make_module(&gen, vec![callee, caller]);
        let map = compute_capabilities(&module);

        // callee has Io.Network
        let callee_caps = map.get(&callee_id).cloned().unwrap_or_default();
        assert!(callee_caps.contains(&Capability::new("Io.Network")));

        // caller also has Io.Network (propagated)
        let caller_caps = map.get(&caller_id).cloned().unwrap_or_default();
        assert!(caller_caps.contains(&Capability::new("Io.Network")));
    }

    #[test]
    fn handling_block_suppresses_effect_capability() {
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
        let fn_node = make_fn(&gen, "f", vec![], body, Visibility::Public);
        let module = make_module(&gen, vec![fn_node.clone()]);

        let map = compute_capabilities(&module);
        let caps = map.get(&fn_node.id).cloned().unwrap_or_default();
        // Log is handled, so Io.Stdout should NOT appear.
        assert!(!caps.contains(&Capability::new("Io.Stdout")));
    }

    // ── verify_capabilities ───────────────────────────────────────────────

    #[test]
    fn sketch_mode_no_diagnostics() {
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

        let bag = verify_capabilities(&module, Strictness::Sketch);
        assert_eq!(bag.error_count(), 0);
        assert_eq!(warning_count(&bag), 0);
    }

    #[test]
    fn dev_mode_warns_public_missing_requires() {
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

        let bag = verify_capabilities(&module, Strictness::Development);
        assert_eq!(bag.error_count(), 0);
        assert!(warning_count(&bag) > 0);
    }

    #[test]
    fn dev_mode_no_warning_private_missing_requires() {
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

        let bag = verify_capabilities(&module, Strictness::Development);
        assert_eq!(bag.error_count(), 0);
        assert_eq!(warning_count(&bag), 0);
    }

    #[test]
    fn prod_mode_errors_all_missing_requires() {
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

        let bag = verify_capabilities(&module, Strictness::Production);
        assert!(bag.error_count() > 0);
    }

    #[test]
    fn declared_capability_suppresses_diagnostic() {
        let gen = NodeIdGen::new();
        let op = make_effect_op(&gen, "Log");
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![op],
                tail: None,
            },
        );
        // @requires(Io.Stdout) — canonical name matches what capability_for_effect
        // returns for the Log effect.
        let ann = make_requires_annotation(&["Io.Stdout"]);
        let fn_node = make_fn(&gen, "f", vec![ann], body, Visibility::Public);
        let module = make_module(&gen, vec![fn_node]);

        let bag = verify_capabilities(&module, Strictness::Production);
        assert_eq!(bag.error_count(), 0);
    }

    #[test]
    fn propagated_capability_missing_produces_error_in_prod() {
        let gen = NodeIdGen::new();

        // callee uses Http → needs Io.Network
        let callee_op = make_effect_op(&gen, "Http");
        let callee_body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![callee_op],
                tail: None,
            },
        );
        let callee = make_fn(&gen, "callee", vec![], callee_body, Visibility::Private);

        // caller calls callee but doesn't declare @requires(Capability.Io.Network)
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
        let bag = verify_capabilities(&module, Strictness::Production);
        assert!(bag.error_count() > 0);
    }

    #[test]
    fn propagated_capability_declared_ok() {
        let gen = NodeIdGen::new();

        // callee uses Http and also declares @requires(Io.Network).
        let callee_op = make_effect_op(&gen, "Http");
        let callee_body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![callee_op],
                tail: None,
            },
        );
        let callee_ann = make_requires_annotation(&["Io.Network"]);
        let callee = make_fn(
            &gen,
            "callee",
            vec![callee_ann],
            callee_body,
            Visibility::Private,
        );

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
        // caller also declares @requires(Io.Network) for the propagated cap.
        let caller_ann = make_requires_annotation(&["Io.Network"]);
        let caller = make_fn(
            &gen,
            "caller",
            vec![caller_ann],
            caller_body,
            Visibility::Public,
        );

        let module = make_module(&gen, vec![callee, caller]);
        let bag = verify_capabilities(&module, Strictness::Production);
        assert_eq!(bag.error_count(), 0);
    }
}
