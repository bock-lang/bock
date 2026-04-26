//! Ownership analysis — O-AIR pass.
//!
//! Implements tree-walk ownership analysis over the AIR with
//! divergence-aware branch merging. Detects:
//!
//! - Use-after-move errors
//! - Mutable borrows of non-`mut` variables
//! - Moves inside loop bodies (would double-move on next iteration)
//!
//! # Algorithm
//!
//! Walk the AIR, maintaining a per-variable [`VarOwnership`] map. At
//! control-flow join points (if/else, match, guard) merge branch states:
//! diverging branches are excluded; among non-diverging branches, any move
//! makes the variable considered moved at the join.

use std::collections::{HashMap, HashSet};

use bock_air::stubs::Value;
use bock_air::{AIRNode, AirInterpolationPart, NodeId, NodeKind};
use bock_ast::AssignOp;
use bock_errors::{DiagnosticBag, DiagnosticCode, Span};

// ─── Diagnostic codes ─────────────────────────────────────────────────────────

const E_USE_AFTER_MOVE: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 5001,
};
const E_MUT_BORROW_NEEDS_MUT: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 5002,
};
const E_LOOP_MOVE: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 5003,
};

// ─── Public types ─────────────────────────────────────────────────────────────

/// Convenience alias — the AIR module root node.
pub type AIRModule = AIRNode;

/// Ownership state of a variable at a given program point.
#[derive(Debug, Clone, PartialEq)]
pub enum OwnershipState {
    /// Variable owns its value.
    Owned,
    /// Variable is currently immutably borrowed.
    Borrowed,
    /// Variable is currently mutably borrowed.
    MutBorrowed,
    /// Value has been moved out; variable is invalid.
    Moved,
    /// Annotated `@managed` — GC semantics, no tracking.
    Managed,
}

/// Ownership information for a single binding at a program point.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnershipInfo {
    /// Current ownership state.
    pub state: OwnershipState,
    /// Whether the binding was declared `mut`.
    pub mutable: bool,
    /// Node that established ownership (the binding site).
    pub origin: NodeId,
}

// ─── Internal bookkeeping ─────────────────────────────────────────────────────

#[derive(Clone)]
struct VarOwnership {
    state: OwnershipState,
    is_mut: bool,
    /// The span where the value was moved, if it has been.
    move_site: Option<Span>,
}

// ─── Analyzer ────────────────────────────────────────────────────────────────

struct OwnershipAnalyzer {
    diags: DiagnosticBag,
    env: HashMap<String, VarOwnership>,
    in_loop: bool,
    /// Variable names that existed when the current loop was entered.
    loop_entry_keys: HashSet<String>,
    /// Whether we are inside a `@managed` function body.
    in_managed: bool,
}

impl OwnershipAnalyzer {
    fn new() -> Self {
        Self {
            diags: DiagnosticBag::new(),
            env: HashMap::new(),
            in_loop: false,
            loop_entry_keys: HashSet::new(),
            in_managed: false,
        }
    }

    fn snapshot(&self) -> HashMap<String, VarOwnership> {
        self.env.clone()
    }

    /// Check that `name` is still valid (not moved). Emits a diagnostic if it
    /// has been moved.
    fn check_use(&mut self, name: &str, use_span: Span) {
        if let Some(var) = self.env.get(name) {
            if matches!(var.state, OwnershipState::Moved) {
                let move_site = var.move_site;
                let diag = self.diags.error(
                    E_USE_AFTER_MOVE,
                    format!("use of moved variable `{name}`"),
                    use_span,
                );
                if let Some(ms) = move_site {
                    diag.label(ms, "value moved here");
                }
            }
        }
    }

    /// Mark `name` as moved from `move_span`. Emits loop-move or
    /// double-move diagnostics as needed.
    fn do_move(&mut self, name: &str, move_span: Span) {
        if let Some(var) = self.env.get(name) {
            if matches!(var.state, OwnershipState::Managed) {
                return; // @managed — no tracking
            }
        }

        // Moving a pre-loop variable inside a loop body = double-move error.
        if self.in_loop && self.loop_entry_keys.contains(name) {
            self.diags.error(
                E_LOOP_MOVE,
                format!(
                    "cannot move `{name}` inside a loop \
                     (would be moved on every iteration)"
                ),
                move_span,
            );
        }

        if let Some(var) = self.env.get_mut(name) {
            if matches!(var.state, OwnershipState::Moved) {
                // Already moved — use-after-move.
                let move_site = var.move_site;
                let diag = self.diags.error(
                    E_USE_AFTER_MOVE,
                    format!("use of moved variable `{name}`"),
                    move_span,
                );
                if let Some(ms) = move_site {
                    diag.label(ms, "value moved here");
                }
            } else {
                var.state = OwnershipState::Moved;
                var.move_site = Some(move_span);
            }
        }
    }

    /// Analyze a node whose produced value will be *moved* (transferred).
    ///
    /// For bare identifiers this marks the variable as moved; for everything
    /// else it delegates to the normal analysis (the value comes from a
    /// fresh temporary and nothing in the env is moved).
    fn analyze_move(&mut self, node: &AIRNode) -> bool {
        if let NodeKind::Identifier { name } = &node.kind {
            if let Some(var) = self.env.get(&name.name) {
                if matches!(var.state, OwnershipState::Managed) {
                    return false; // @managed: skip
                }
            }
            // Primitive types (Int, Float, Bool, Char, String) have copy
            // semantics — using them doesn't transfer ownership.
            if node.metadata.get("copy_type") == Some(&Value::Bool(true)) {
                return false;
            }
            self.do_move(&name.name, node.span);
            false
        } else {
            self.analyze_node(node)
        }
    }

    /// Merge branch states after a fork.
    ///
    /// `pre` is the state at the fork entry. `branches` is a list of
    /// (diverges, post-state) pairs. Diverging branches are excluded.
    /// If all branches diverge the join is unreachable and `pre` is returned.
    fn merge_states(
        &self,
        pre: &HashMap<String, VarOwnership>,
        branches: &[(bool, HashMap<String, VarOwnership>)],
    ) -> HashMap<String, VarOwnership> {
        let non_div: Vec<&HashMap<String, VarOwnership>> = branches
            .iter()
            .filter(|(div, _)| !*div)
            .map(|(_, s)| s)
            .collect();

        if non_div.is_empty() {
            // Unreachable join — propagate pre-state unchanged.
            return pre.clone();
        }

        let mut result = pre.clone();
        for name in pre.keys() {
            let any_moved = non_div.iter().any(|state| {
                state
                    .get(name)
                    .is_some_and(|v| matches!(v.state, OwnershipState::Moved))
            });
            if any_moved {
                if let Some(var) = result.get_mut(name) {
                    let move_site = non_div
                        .iter()
                        .filter_map(|state| state.get(name))
                        .find(|v| matches!(v.state, OwnershipState::Moved))
                        .and_then(|v| v.move_site);
                    var.state = OwnershipState::Moved;
                    var.move_site = move_site;
                }
            }
        }
        result
    }

    /// Add a parameter as an owned binding.
    fn bind_param(&mut self, param: &AIRNode) {
        if let NodeKind::Param { pattern, .. } = &param.kind {
            if let NodeKind::BindPat { name, is_mut } = &pattern.kind {
                let state = if self.in_managed {
                    OwnershipState::Managed
                } else {
                    OwnershipState::Owned
                };
                self.env.insert(
                    name.name.clone(),
                    VarOwnership {
                        state,
                        is_mut: *is_mut,
                        move_site: None,
                    },
                );
            }
        }
    }

    /// Add bindings introduced by a pattern (e.g. in `let`, match arms, loops).
    fn bind_pattern(&mut self, pat: &AIRNode) {
        let base_state = if self.in_managed {
            OwnershipState::Managed
        } else {
            OwnershipState::Owned
        };
        match &pat.kind {
            NodeKind::BindPat { name, is_mut } => {
                self.env.insert(
                    name.name.clone(),
                    VarOwnership {
                        state: base_state,
                        is_mut: *is_mut,
                        move_site: None,
                    },
                );
            }
            NodeKind::TuplePat { elems } => {
                for e in elems {
                    self.bind_pattern(e);
                }
            }
            NodeKind::ConstructorPat { fields, .. } => {
                for f in fields {
                    self.bind_pattern(f);
                }
            }
            NodeKind::RecordPat { fields, .. } => {
                for f in fields {
                    if let Some(p) = &f.pattern {
                        self.bind_pattern(p);
                    } else {
                        self.env.insert(
                            f.name.name.clone(),
                            VarOwnership {
                                state: base_state.clone(),
                                is_mut: false,
                                move_site: None,
                            },
                        );
                    }
                }
            }
            NodeKind::ListPat { elems, rest } => {
                for e in elems {
                    self.bind_pattern(e);
                }
                if let Some(r) = rest {
                    self.bind_pattern(r);
                }
            }
            NodeKind::OrPat { alternatives } => {
                if let Some(first) = alternatives.first() {
                    self.bind_pattern(first);
                }
            }
            _ => {}
        }
    }

    /// Returns `true` if this node always diverges (never returns normally).
    #[allow(clippy::too_many_lines)]
    fn analyze_node(&mut self, node: &AIRNode) -> bool {
        match &node.kind {
            // ── Module root ──────────────────────────────────────────────────
            NodeKind::Module { imports, items, .. } => {
                for n in imports {
                    self.analyze_node(n);
                }
                for n in items {
                    self.analyze_node(n);
                }
                false
            }

            // ── Declarations ─────────────────────────────────────────────────
            NodeKind::FnDecl {
                annotations,
                params,
                body,
                ..
            } => {
                let outer = self.snapshot();
                let outer_managed = self.in_managed;
                if annotations.iter().any(|a| a.name.name == "managed") {
                    self.in_managed = true;
                }
                for p in params {
                    self.bind_param(p);
                }
                self.analyze_node(body);
                self.env = outer;
                self.in_managed = outer_managed;
                false
            }
            NodeKind::ImplBlock { methods, .. } => {
                for m in methods {
                    self.analyze_node(m);
                }
                false
            }
            NodeKind::ClassDecl { methods, .. } => {
                for m in methods {
                    self.analyze_node(m);
                }
                false
            }
            NodeKind::TraitDecl { methods, .. } => {
                for m in methods {
                    self.analyze_node(m);
                }
                false
            }
            // Type-level / effect decls carry no runtime ownership.
            NodeKind::RecordDecl { .. }
            | NodeKind::EnumDecl { .. }
            | NodeKind::TypeAlias { .. }
            | NodeKind::EffectDecl { .. }
            | NodeKind::ConstDecl { .. }
            | NodeKind::ImportDecl { .. }
            | NodeKind::ModuleHandle { .. }
            | NodeKind::PropertyTest { .. } => false,

            // ── Block ─────────────────────────────────────────────────────────
            NodeKind::Block { stmts, tail } => {
                let pre_keys: HashSet<String> = self.env.keys().cloned().collect();
                let mut diverges = false;
                for stmt in stmts {
                    if diverges {
                        break;
                    }
                    diverges = self.analyze_node(stmt);
                }
                if !diverges {
                    if let Some(t) = tail {
                        diverges = self.analyze_node(t);
                    }
                }
                // Drop bindings that were introduced in this block scope.
                self.env.retain(|k, _| pre_keys.contains(k));
                diverges
            }

            // ── Let binding ───────────────────────────────────────────────────
            NodeKind::LetBinding {
                is_mut,
                pattern,
                value,
                ..
            } => {
                let is_managed = self.in_managed
                    || node.metadata.get("managed") == Some(&Value::Bool(true));

                self.analyze_move(value);

                let state = if is_managed {
                    OwnershipState::Managed
                } else {
                    OwnershipState::Owned
                };

                // For tracking purposes we only need to insert simple BindPat
                // names; complex patterns are handled by bind_pattern which
                // also adds Owned state. @managed simply overrides.
                let own = VarOwnership {
                    state,
                    is_mut: *is_mut,
                    move_site: None,
                };
                if is_managed {
                    // Insert with Managed state so reads are still fine.
                    if let NodeKind::BindPat { name, .. } = &pattern.kind {
                        self.env.insert(name.name.clone(), own);
                    } else {
                        self.bind_pattern(pattern);
                        // Override all inserted entries to Managed.
                        // (Nested managed patterns are uncommon; this is best-effort.)
                    }
                } else {
                    self.bind_pattern(pattern);
                }
                false
            }

            // ── Assignment ────────────────────────────────────────────────────
            NodeKind::Assign { op, target, value } => {
                match op {
                    AssignOp::Assign => {
                        // Plain assignment: rhs value is moved into lhs.
                        self.analyze_move(value);
                        self.analyze_node(target);
                    }
                    _ => {
                        // Compound assignment: both are reads.
                        self.analyze_node(target);
                        self.analyze_node(value);
                    }
                }
                false
            }

            // ── Identifier use ────────────────────────────────────────────────
            NodeKind::Identifier { name } => {
                self.check_use(&name.name, node.span);
                false
            }

            // ── Explicit ownership operations ─────────────────────────────────
            NodeKind::Move { expr } => {
                self.analyze_move(expr);
                false
            }
            NodeKind::Borrow { expr } => {
                // Immutable borrow — check validity, no ownership change.
                self.analyze_node(expr);
                false
            }
            NodeKind::MutableBorrow { expr } => {
                // Mutable borrow requires `mut` on the binding.
                if let NodeKind::Identifier { name } = &expr.kind {
                    if let Some(var) = self.env.get(&name.name) {
                        if !var.is_mut && !matches!(var.state, OwnershipState::Managed) {
                            self.diags.error(
                                E_MUT_BORROW_NEEDS_MUT,
                                format!(
                                    "cannot mutably borrow `{}`: \
                                     variable not declared `mut`",
                                    name.name
                                ),
                                expr.span,
                            );
                        }
                    }
                }
                self.analyze_node(expr);
                false
            }

            // ── Diverging statements ──────────────────────────────────────────
            NodeKind::Return { value } => {
                if let Some(v) = value {
                    // Return exits the function (and any enclosing loop),
                    // so moves in the return value cannot repeat.
                    let old_in_loop = self.in_loop;
                    self.in_loop = false;
                    self.analyze_move(v);
                    self.in_loop = old_in_loop;
                }
                true
            }
            NodeKind::Break { value } => {
                if let Some(v) = value {
                    // Break exits the loop, so moves in the break value
                    // cannot repeat on the next iteration.
                    let old_in_loop = self.in_loop;
                    self.in_loop = false;
                    self.analyze_move(v);
                    self.in_loop = old_in_loop;
                }
                true
            }
            NodeKind::Continue => true,
            NodeKind::Unreachable => true,

            // ── If / if-let ───────────────────────────────────────────────────
            NodeKind::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                self.analyze_node(condition);

                let pre = self.snapshot();

                let then_div = self.analyze_node(then_block);
                let then_state = self.snapshot();

                self.env = pre.clone();
                let (else_div, else_state) = match else_block {
                    Some(eb) => {
                        let d = self.analyze_node(eb);
                        (d, self.snapshot())
                    }
                    None => {
                        // No else branch = implicitly non-diverging with pre-state.
                        (false, pre.clone())
                    }
                };

                self.env =
                    self.merge_states(&pre, &[(then_div, then_state), (else_div, else_state)]);
                then_div && else_div
            }

            // ── Guard ─────────────────────────────────────────────────────────
            NodeKind::Guard {
                let_pattern,
                condition,
                else_block,
            } => {
                if let Some(pat) = let_pattern {
                    self.analyze_node(pat);
                }
                self.analyze_node(condition);
                // The else_block should diverge; even if it doesn't we still
                // exclude its state from the main path per spec.
                let pre = self.snapshot();
                self.analyze_node(else_block);
                // Main path continues from pre-state (else excluded).
                self.env = pre;
                false
            }

            // ── Match ─────────────────────────────────────────────────────────
            NodeKind::Match { scrutinee, arms } => {
                self.analyze_node(scrutinee);

                let pre = self.snapshot();
                let mut arm_results: Vec<(bool, HashMap<String, VarOwnership>)> =
                    Vec::with_capacity(arms.len());

                for arm in arms {
                    self.env = pre.clone();
                    let div = self.analyze_node(arm);
                    arm_results.push((div, self.snapshot()));
                }

                self.env = self.merge_states(&pre, &arm_results);
                arm_results.iter().all(|(d, _)| *d)
            }

            NodeKind::MatchArm {
                pattern,
                guard,
                body,
            } => {
                let pre_keys: HashSet<String> = self.env.keys().cloned().collect();
                self.bind_pattern(pattern);
                if let Some(g) = guard {
                    self.analyze_node(g);
                }
                let div = self.analyze_node(body);
                // Drop arm-local bindings.
                self.env.retain(|k, _| pre_keys.contains(k));
                div
            }

            // ── Loops ─────────────────────────────────────────────────────────
            NodeKind::For {
                pattern,
                iterable,
                body,
            } => {
                self.analyze_node(iterable);
                let pre = self.snapshot();
                let old_in_loop = self.in_loop;
                let old_loop_keys = std::mem::take(&mut self.loop_entry_keys);
                self.in_loop = true;
                self.loop_entry_keys = pre.keys().cloned().collect();
                self.bind_pattern(pattern);
                self.analyze_node(body);
                self.in_loop = old_in_loop;
                self.loop_entry_keys = old_loop_keys;
                // Loop may execute zero times — restore pre-state.
                self.env = pre;
                false
            }
            NodeKind::While { condition, body } => {
                self.analyze_node(condition);
                let pre = self.snapshot();
                let old_in_loop = self.in_loop;
                let old_loop_keys = std::mem::take(&mut self.loop_entry_keys);
                self.in_loop = true;
                self.loop_entry_keys = pre.keys().cloned().collect();
                self.analyze_node(body);
                self.in_loop = old_in_loop;
                self.loop_entry_keys = old_loop_keys;
                self.env = pre;
                false
            }
            NodeKind::Loop { body } => {
                let pre = self.snapshot();
                let old_in_loop = self.in_loop;
                let old_loop_keys = std::mem::take(&mut self.loop_entry_keys);
                self.in_loop = true;
                self.loop_entry_keys = pre.keys().cloned().collect();
                self.analyze_node(body);
                self.in_loop = old_in_loop;
                self.loop_entry_keys = old_loop_keys;
                self.env = pre;
                false
            }

            // ── Calls ─────────────────────────────────────────────────────────
            NodeKind::Call { callee, args, .. } => {
                self.analyze_node(callee);
                for arg in args {
                    // Arguments are borrows by default; explicit `move` or
                    // `MutableBorrow` wrapping is handled by their own arms.
                    self.analyze_node(&arg.value);
                }
                false
            }
            NodeKind::MethodCall { receiver, args, .. } => {
                self.analyze_node(receiver);
                for arg in args {
                    self.analyze_node(&arg.value);
                }
                false
            }

            // ── Lambda ────────────────────────────────────────────────────────
            NodeKind::Lambda { params, body } => {
                let outer = self.snapshot();
                for p in params {
                    self.bind_param(p);
                }
                self.analyze_node(body);
                self.env = outer;
                false
            }

            // ── Other expressions ─────────────────────────────────────────────
            NodeKind::BinaryOp { left, right, .. } => {
                self.analyze_node(left);
                self.analyze_node(right);
                false
            }
            NodeKind::UnaryOp { operand, .. } => {
                self.analyze_node(operand);
                false
            }
            NodeKind::FieldAccess { object, .. } => {
                self.analyze_node(object);
                false
            }
            NodeKind::Index { object, index } => {
                self.analyze_node(object);
                self.analyze_node(index);
                false
            }
            NodeKind::Propagate { expr } => {
                self.analyze_node(expr);
                false
            }
            NodeKind::Pipe { left, right } | NodeKind::Compose { left, right } => {
                self.analyze_node(left);
                self.analyze_node(right);
                false
            }
            NodeKind::Await { expr } => {
                self.analyze_node(expr);
                false
            }
            NodeKind::Range { lo, hi, .. } => {
                self.analyze_node(lo);
                self.analyze_node(hi);
                false
            }
            NodeKind::RecordConstruct { fields, spread, .. } => {
                for f in fields {
                    if let Some(v) = &f.value {
                        self.analyze_move(v);
                    }
                }
                if let Some(s) = spread {
                    self.analyze_node(s);
                }
                false
            }
            NodeKind::ListLiteral { elems }
            | NodeKind::SetLiteral { elems }
            | NodeKind::TupleLiteral { elems } => {
                for e in elems {
                    self.analyze_move(e);
                }
                false
            }
            NodeKind::MapLiteral { entries } => {
                for entry in entries {
                    self.analyze_move(&entry.key);
                    self.analyze_move(&entry.value);
                }
                false
            }
            NodeKind::Interpolation { parts } => {
                for part in parts {
                    if let AirInterpolationPart::Expr(e) = part {
                        self.analyze_node(e);
                    }
                }
                false
            }
            NodeKind::ResultConstruct { value, .. } => {
                if let Some(v) = value {
                    self.analyze_move(v);
                }
                false
            }
            NodeKind::HandlingBlock { handlers, body } => {
                for h in handlers {
                    self.analyze_node(&h.handler);
                }
                self.analyze_node(body);
                false
            }

            // ── Terminals ─────────────────────────────────────────────────────
            NodeKind::Literal { .. }
            | NodeKind::Placeholder
            | NodeKind::TypeSelf
            | NodeKind::WildcardPat
            | NodeKind::LiteralPat { .. }
            | NodeKind::RestPat
            | NodeKind::TypeNamed { .. }
            | NodeKind::TypeTuple { .. }
            | NodeKind::TypeFunction { .. }
            | NodeKind::TypeOptional { .. }
            | NodeKind::EffectOp { .. }
            | NodeKind::EffectRef { .. }
            | NodeKind::Error => false,

            // ── Patterns (visited via bind_pattern, not analyze_node) ─────────
            NodeKind::BindPat { .. }
            | NodeKind::ConstructorPat { .. }
            | NodeKind::RecordPat { .. }
            | NodeKind::TuplePat { .. }
            | NodeKind::ListPat { .. }
            | NodeKind::OrPat { .. }
            | NodeKind::GuardPat { .. }
            | NodeKind::RangePat { .. } => false,

            // ── Catch-all for future node kinds ──────────────────────────────
            _ => false,
        }
    }
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Perform ownership analysis on an AIR module.
///
/// Returns a [`DiagnosticBag`] containing any ownership violations found.
/// A non-empty bag with errors indicates the program has ownership errors.
///
/// # Analysis performed
///
/// - **Use-after-move**: using a variable after its value was moved out.
/// - **Mutable borrow of non-`mut`**: `&mut x` when `x` was not declared `mut`.
/// - **Move in loop body**: moving a pre-loop variable inside a loop.
/// - **`@managed` escape hatch**: variables with `metadata["managed"] = true`
///   are excluded from ownership tracking.
/// - **Divergence-aware branch merging**: diverging branches (return, break,
///   continue, unreachable) are excluded from join-point state merges.
#[must_use]
pub fn analyze_ownership(module: &AIRModule) -> DiagnosticBag {
    let mut analyzer = OwnershipAnalyzer::new();
    analyzer.analyze_node(module);
    analyzer.diags
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::stubs::Value;
    use bock_air::{AIRNode, AirArg, NodeIdGen, NodeKind};
    use bock_ast::{Ident, Literal};
    use bock_errors::{FileId, Span};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn span() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }

    fn span_at(start: usize, end: usize) -> Span {
        Span {
            file: FileId(0),
            start,
            end,
        }
    }

    fn ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: span(),
        }
    }

    fn node(gen: &NodeIdGen, kind: NodeKind) -> AIRNode {
        AIRNode::new(gen.next(), span(), kind)
    }

    fn node_at(gen: &NodeIdGen, kind: NodeKind, s: usize, e: usize) -> AIRNode {
        AIRNode::new(gen.next(), span_at(s, e), kind)
    }

    fn id_node(gen: &NodeIdGen, name: &str) -> AIRNode {
        node(gen, NodeKind::Identifier { name: ident(name) })
    }

    fn id_node_at(gen: &NodeIdGen, name: &str, s: usize, e: usize) -> AIRNode {
        node_at(gen, NodeKind::Identifier { name: ident(name) }, s, e)
    }

    fn lit_node(gen: &NodeIdGen) -> AIRNode {
        node(
            gen,
            NodeKind::Literal {
                lit: Literal::Int("42".into()),
            },
        )
    }

    fn bind_pat(gen: &NodeIdGen, name: &str, is_mut: bool) -> AIRNode {
        node(
            gen,
            NodeKind::BindPat {
                name: ident(name),
                is_mut,
            },
        )
    }

    fn let_binding(gen: &NodeIdGen, name: &str, is_mut: bool, value: AIRNode) -> AIRNode {
        node(
            gen,
            NodeKind::LetBinding {
                is_mut,
                pattern: Box::new(bind_pat(gen, name, is_mut)),
                ty: None,
                value: Box::new(value),
            },
        )
    }

    fn block(gen: &NodeIdGen, stmts: Vec<AIRNode>, tail: Option<AIRNode>) -> AIRNode {
        node(
            gen,
            NodeKind::Block {
                stmts,
                tail: tail.map(Box::new),
            },
        )
    }

    fn module(gen: &NodeIdGen, items: Vec<AIRNode>) -> AIRNode {
        node(
            gen,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items,
            },
        )
    }

    fn fn_decl(gen: &NodeIdGen, body: AIRNode) -> AIRNode {
        fn_decl_with(gen, body, vec![])
    }

    fn managed_fn_decl(gen: &NodeIdGen, body: AIRNode) -> AIRNode {
        use bock_ast::Annotation;
        fn_decl_with(
            gen,
            body,
            vec![Annotation {
                id: 0,
                span: span(),
                name: ident("managed"),
                args: vec![],
            }],
        )
    }

    fn fn_decl_with(gen: &NodeIdGen, body: AIRNode, annotations: Vec<bock_ast::Annotation>) -> AIRNode {
        node(
            gen,
            NodeKind::FnDecl {
                annotations,
                visibility: bock_ast::Visibility::Public,
                is_async: false,
                name: ident("f"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    fn return_node(gen: &NodeIdGen, val: Option<AIRNode>) -> AIRNode {
        node(
            gen,
            NodeKind::Return {
                value: val.map(Box::new),
            },
        )
    }

    fn move_node(gen: &NodeIdGen, expr: AIRNode) -> AIRNode {
        node(
            gen,
            NodeKind::Move {
                expr: Box::new(expr),
            },
        )
    }

    fn mut_borrow(gen: &NodeIdGen, expr: AIRNode) -> AIRNode {
        node(
            gen,
            NodeKind::MutableBorrow {
                expr: Box::new(expr),
            },
        )
    }

    fn if_node(gen: &NodeIdGen, cond: AIRNode, then: AIRNode, else_: Option<AIRNode>) -> AIRNode {
        node(
            gen,
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(cond),
                then_block: Box::new(then),
                else_block: else_.map(Box::new),
            },
        )
    }

    fn guard_node(gen: &NodeIdGen, cond: AIRNode, else_block: AIRNode) -> AIRNode {
        node(
            gen,
            NodeKind::Guard {
                let_pattern: None,
                condition: Box::new(cond),
                else_block: Box::new(else_block),
            },
        )
    }

    fn loop_node(gen: &NodeIdGen, body: AIRNode) -> AIRNode {
        node(
            gen,
            NodeKind::Loop {
                body: Box::new(body),
            },
        )
    }

    fn match_node(gen: &NodeIdGen, scrutinee: AIRNode, arms: Vec<AIRNode>) -> AIRNode {
        node(
            gen,
            NodeKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
        )
    }

    fn match_arm(gen: &NodeIdGen, pat: AIRNode, body: AIRNode) -> AIRNode {
        node(
            gen,
            NodeKind::MatchArm {
                pattern: Box::new(pat),
                guard: None,
                body: Box::new(body),
            },
        )
    }

    // ── Tests: move detection ─────────────────────────────────────────────────

    #[test]
    fn no_error_simple_borrow() {
        // let data = 42
        // summarize(data)   -- borrow, data still owned
        // use(data)         -- still ok
        let gen = NodeIdGen::new();
        let data_lit = lit_node(&gen);
        let let_data = let_binding(&gen, "data", false, data_lit);
        let use1 = id_node(&gen, "data");
        let use2 = id_node(&gen, "data");
        let call = node(
            &gen,
            NodeKind::Call {
                callee: Box::new(id_node(&gen, "summarize")),
                args: vec![AirArg {
                    label: None,
                    value: use1,
                }],
                type_args: vec![],
            },
        );
        let b = block(&gen, vec![let_data, call, use2], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(
            !diags.has_errors(),
            "expected no errors, got: {:?}",
            diags.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn move_on_let_binding() {
        // let data = 42
        // let archive = data   -- moves data
        // use(data)            -- ERROR: use after move
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let id_data = id_node_at(&gen, "data", 10, 14);
        let let_archive = let_binding(&gen, "archive", false, id_data);
        let use_data = id_node_at(&gen, "data", 20, 24);
        let b = block(&gen, vec![let_data, let_archive, use_data], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| d.code == E_USE_AFTER_MOVE));
    }

    #[test]
    fn explicit_move_node() {
        // let data = 42
        // move data
        // use data  -- ERROR
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let mv = move_node(&gen, id_node_at(&gen, "data", 5, 9));
        let use_data = id_node_at(&gen, "data", 15, 19);
        let b = block(&gen, vec![let_data, mv, use_data], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| d.code == E_USE_AFTER_MOVE));
    }

    #[test]
    fn use_after_move_has_move_site_label() {
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let mv = move_node(&gen, id_node_at(&gen, "data", 5, 9));
        let use_data = id_node_at(&gen, "data", 15, 19);
        let b = block(&gen, vec![let_data, mv, use_data], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        let err = diags.iter().find(|d| d.code == E_USE_AFTER_MOVE).unwrap();
        assert!(
            !err.labels.is_empty(),
            "expected a label pointing to move site"
        );
        assert!(err.labels[0].message.contains("moved"));
    }

    // ── Tests: mutable borrow ─────────────────────────────────────────────────

    #[test]
    fn mut_borrow_of_non_mut_errors() {
        // let data = 42       -- not mut
        // &mut data           -- ERROR
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let mb = mut_borrow(&gen, id_node(&gen, "data"));
        let b = block(&gen, vec![let_data, mb], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| d.code == E_MUT_BORROW_NEEDS_MUT));
    }

    #[test]
    fn mut_borrow_of_mut_ok() {
        // let mut data = 42
        // &mut data           -- OK
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", true, lit_node(&gen));
        let mb = mut_borrow(&gen, id_node(&gen, "data"));
        let b = block(&gen, vec![let_data, mb], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(!diags.has_errors());
    }

    // ── Tests: @managed ───────────────────────────────────────────────────────

    #[test]
    fn managed_skips_ownership_tracking() {
        // @managed let data = 42  (metadata["managed"] = true)
        // let archive = data       -- would be a move error, but @managed skips
        // use data                 -- no error
        let gen = NodeIdGen::new();
        let mut let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let_data
            .metadata
            .insert("managed".into(), Value::Bool(true));
        let id_data = id_node(&gen, "data");
        let let_archive = let_binding(&gen, "archive", false, id_data);
        let use_data = id_node(&gen, "data");
        let b = block(&gen, vec![let_data, let_archive, use_data], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(!diags.has_errors());
    }

    #[test]
    fn managed_fn_suppresses_move_errors() {
        // @managed
        // fn f() {
        //   let data = 42
        //   let a = data   -- move
        //   let b = data   -- would be use-after-move, but @managed suppresses
        // }
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let id_data1 = id_node(&gen, "data");
        let let_a = let_binding(&gen, "a", false, id_data1);
        let id_data2 = id_node(&gen, "data");
        let let_b = let_binding(&gen, "b", false, id_data2);
        let b = block(&gen, vec![let_data, let_a, let_b], None);
        let m = module(&gen, vec![managed_fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(
            !diags.has_errors(),
            "expected no errors in @managed fn, got: {:?}",
            diags.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn non_managed_fn_still_errors_on_reuse() {
        // fn f() {
        //   let data = 42
        //   let a = data
        //   let b = data   -- ERROR: use after move
        // }
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let id_data1 = id_node(&gen, "data");
        let let_a = let_binding(&gen, "a", false, id_data1);
        let id_data2 = id_node_at(&gen, "data", 20, 24);
        let let_b = let_binding(&gen, "b", false, id_data2);
        let b = block(&gen, vec![let_data, let_a, let_b], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| d.code == E_USE_AFTER_MOVE));
    }

    #[test]
    fn managed_fn_loop_move_suppressed() {
        // @managed
        // fn f() {
        //   let data = 42
        //   loop { let _ = data }   -- would be E5003 loop-move, but @managed suppresses
        // }
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let id_data = id_node(&gen, "data");
        let let_discard = let_binding(&gen, "_d", false, id_data);
        let loop_body = block(&gen, vec![let_discard], None);
        let lp = loop_node(&gen, loop_body);
        let b = block(&gen, vec![let_data, lp], None);
        let m = module(&gen, vec![managed_fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(
            !diags.has_errors(),
            "expected no errors in @managed fn loop, got: {:?}",
            diags.iter().collect::<Vec<_>>()
        );
    }

    // ── Tests: diverging branches ─────────────────────────────────────────────

    #[test]
    fn guard_else_diverges_data_still_owned() {
        // guard (cond) else { return }   -- else diverges
        // use data                        -- data still owned
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let cond = lit_node(&gen);
        let else_block = block(&gen, vec![return_node(&gen, None)], None);
        let guard = guard_node(&gen, cond, else_block);
        let use_data = id_node(&gen, "data");
        let b = block(&gen, vec![let_data, guard, use_data], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(!diags.has_errors());
    }

    #[test]
    fn if_with_diverging_then_data_still_owned() {
        // let data = 42
        // if (cond) { consume(data); return }   -- then diverges
        // use data                               -- data still owned
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let cond = lit_node(&gen);
        // then: move data then return
        let id_data = id_node(&gen, "data");
        let let_archive = let_binding(&gen, "archive", false, id_data);
        let ret = return_node(&gen, None);
        let then_block = block(&gen, vec![let_archive, ret], None);
        let if_node_ = if_node(&gen, cond, then_block, None);
        let use_data = id_node(&gen, "data");
        let b = block(&gen, vec![let_data, if_node_, use_data], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(!diags.has_errors());
    }

    #[test]
    fn if_non_diverging_branch_moves_makes_moved_at_join() {
        // let data = 42
        // if (cond) { let archive = data }   -- non-diverging, moves data
        // use data                            -- ERROR
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let cond = lit_node(&gen);
        let id_data = id_node(&gen, "data");
        let let_archive = let_binding(&gen, "archive", false, id_data);
        let then_block = block(&gen, vec![let_archive], None);
        let if_node_ = if_node(&gen, cond, then_block, None);
        let use_data = id_node_at(&gen, "data", 30, 34);
        let b = block(&gen, vec![let_data, if_node_, use_data], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| d.code == E_USE_AFTER_MOVE));
    }

    #[test]
    fn if_both_branches_diverge_join_uses_pre_state() {
        // let data = 42
        // if (cond) { return } else { return }  -- both diverge
        // (unreachable join — no error)
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let cond = lit_node(&gen);
        let then_block = block(&gen, vec![return_node(&gen, None)], None);
        let else_block = block(&gen, vec![return_node(&gen, None)], None);
        let if_node_ = if_node(&gen, cond, then_block, Some(else_block));
        let b = block(&gen, vec![let_data, if_node_], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(!diags.has_errors());
    }

    // ── Tests: loop move detection ────────────────────────────────────────────

    #[test]
    fn move_inside_loop_is_error() {
        // let data = 42
        // loop { let archive = data }   -- ERROR: moving pre-loop var in loop
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let id_data = id_node_at(&gen, "data", 10, 14);
        let let_archive = let_binding(&gen, "archive", false, id_data);
        let loop_body = block(&gen, vec![let_archive], None);
        let lp = loop_node(&gen, loop_body);
        let b = block(&gen, vec![let_data, lp], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| d.code == E_LOOP_MOVE));
    }

    #[test]
    fn variable_defined_inside_loop_can_be_moved() {
        // loop { let tmp = 42; let _ = tmp }  -- OK: tmp is fresh each iteration
        let gen = NodeIdGen::new();
        let let_tmp = let_binding(&gen, "tmp", false, lit_node(&gen));
        let id_tmp = id_node(&gen, "tmp");
        let let_discard = let_binding(&gen, "_unused", false, id_tmp);
        let loop_body = block(&gen, vec![let_tmp, let_discard], None);
        let lp = loop_node(&gen, loop_body);
        let b = block(&gen, vec![lp], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(!diags.has_errors());
    }

    // ── Tests: match with diverging arms ─────────────────────────────────────

    #[test]
    fn match_all_arms_diverge_no_use_after_move() {
        // let data = 42
        // match x {
        //   Ok(v) => { let _ = data; return }   -- diverges
        //   Err(_) => return                     -- diverges
        // }
        // (unreachable join)
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let scrutinee = lit_node(&gen);
        let id_data = id_node(&gen, "data");
        let let_discard = let_binding(&gen, "_d", false, id_data);
        let ret1 = return_node(&gen, None);
        let arm1_body = block(&gen, vec![let_discard, ret1], None);
        let arm1 = match_arm(&gen, node(&gen, NodeKind::WildcardPat), arm1_body);
        let ret2 = return_node(&gen, None);
        let arm2_body = block(&gen, vec![ret2], None);
        let arm2 = match_arm(&gen, node(&gen, NodeKind::WildcardPat), arm2_body);
        let m_node = match_node(&gen, scrutinee, vec![arm1, arm2]);
        let b = block(&gen, vec![let_data, m_node], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(!diags.has_errors());
    }

    #[test]
    fn match_non_diverging_arm_moves_is_error_after() {
        // let data = 42
        // match x {
        //   A => { let _ = data }  -- non-diverging, moves data
        //   B => return             -- diverges
        // }
        // use data  -- ERROR: moved in non-diverging arm
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let scrutinee = lit_node(&gen);
        let id_data = id_node(&gen, "data");
        let let_discard = let_binding(&gen, "_d", false, id_data);
        let arm1_body = block(&gen, vec![let_discard], None);
        let arm1 = match_arm(&gen, node(&gen, NodeKind::WildcardPat), arm1_body);
        let ret = return_node(&gen, None);
        let arm2_body = block(&gen, vec![ret], None);
        let arm2 = match_arm(&gen, node(&gen, NodeKind::WildcardPat), arm2_body);
        let m_node = match_node(&gen, scrutinee, vec![arm1, arm2]);
        let use_data = id_node_at(&gen, "data", 50, 54);
        let b = block(&gen, vec![let_data, m_node, use_data], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| d.code == E_USE_AFTER_MOVE));
    }

    // ── Tests: nested control flow ────────────────────────────────────────────

    #[test]
    fn if_inside_match_arm_nested() {
        // let data = 42
        // match x {
        //   _ => if (cond) { let _ = data } else { return }
        //        -- then: non-div, moves data; else: diverges
        //        -- after if: data is moved (from non-div branch)
        // }
        // use data  -- ERROR
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let scrutinee = lit_node(&gen);
        let cond = lit_node(&gen);
        let id_data = id_node(&gen, "data");
        let let_discard = let_binding(&gen, "_d", false, id_data);
        let then_block = block(&gen, vec![let_discard], None);
        let else_block = block(&gen, vec![return_node(&gen, None)], None);
        let if_expr = if_node(&gen, cond, then_block, Some(else_block));
        let arm_body = block(&gen, vec![if_expr], None);
        let arm = match_arm(&gen, node(&gen, NodeKind::WildcardPat), arm_body);
        let m_node = match_node(&gen, scrutinee, vec![arm]);
        let use_data = id_node_at(&gen, "data", 50, 54);
        let b = block(&gen, vec![let_data, m_node, use_data], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| d.code == E_USE_AFTER_MOVE));
    }

    #[test]
    fn no_false_positive_if_else_both_leave_owned() {
        // let data = 42
        // if (cond) { use(data) } else { use(data) }  -- borrows in both branches
        // use data  -- still owned, no error
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let cond = lit_node(&gen);
        let use1 = id_node(&gen, "data");
        let use2 = id_node(&gen, "data");
        let then_block = block(&gen, vec![use1], None);
        let else_block = block(&gen, vec![use2], None);
        let if_expr = if_node(&gen, cond, then_block, Some(else_block));
        let use_after = id_node(&gen, "data");
        let b = block(&gen, vec![let_data, if_expr, use_after], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(!diags.has_errors());
    }

    #[test]
    fn double_move_error() {
        // let data = 42
        // let a = data   -- moves data
        // let b = data   -- ERROR: use after move
        let gen = NodeIdGen::new();
        let let_data = let_binding(&gen, "data", false, lit_node(&gen));
        let let_a = let_binding(&gen, "a", false, id_node(&gen, "data"));
        let let_b = let_binding(&gen, "b", false, id_node_at(&gen, "data", 20, 24));
        let b = block(&gen, vec![let_data, let_a, let_b], None);
        let m = module(&gen, vec![fn_decl(&gen, b)]);
        let diags = analyze_ownership(&m);
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| d.code == E_USE_AFTER_MOVE));
    }
}
