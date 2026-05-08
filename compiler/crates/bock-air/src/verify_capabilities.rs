//! Capability verification pass — the final C-AIR pass.
//!
//! This pass runs after context interpretation, validation, and composition.
//! It performs:
//!
//! 1. **Effect handler completeness**: every `EffectOp` invocation is either inside
//!    a `handling` block that handles that effect, or the enclosing function declares
//!    the effect in its `with` clause.
//! 2. **Capability propagation verification**: if a function body uses capabilities
//!    (via `@requires` on called declarations), the calling function must also
//!    declare those capabilities.
//! 3. **Production completeness**: all modules have `@context`, all public functions
//!    have `@context`, all capabilities are declared, all effects are declared.
//!
//! # Diagnostic codes
//!
//! | Code  | Description                                           |
//! |-------|-------------------------------------------------------|
//! | E8020 | Unhandled effect operation (no handler or `with` clause) |
//! | E8021 | Missing capability declaration (callee requires it)   |
//! | E8022 | Production: module missing `@context`                 |
//! | E8023 | Production: public function missing `@context`        |
//! | W8020 | Effect declared in `with` clause but never used       |
//! | W8021 | Capability declared but never used by body            |

use std::collections::HashSet;

use bock_ast::Visibility;
use bock_errors::{DiagnosticBag, DiagnosticCode};

use crate::node::{AIRNode, NodeKind};
use crate::stubs::Capability;

/// Mode for capability verification — controls how strict the checks are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMode {
    /// Development mode: only verify effect handler completeness.
    Development,
    /// Production mode: full verification — all modules and public functions
    /// must have `@context`, all capabilities declared, all effects handled.
    Production,
}

/// A completeness report produced by [`verify_capabilities`].
///
/// Summarises the verification results for use by `bock check --context`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletenessReport {
    /// Total number of modules analysed.
    pub total_modules: usize,
    /// Number of modules with `@context` annotations.
    pub modules_with_context: usize,
    /// Total number of public functions analysed.
    pub total_public_fns: usize,
    /// Number of public functions with `@context` annotations.
    pub public_fns_with_context: usize,
    /// Total number of effect operations found.
    pub total_effect_ops: usize,
    /// Number of effect operations that are properly handled.
    pub handled_effect_ops: usize,
    /// All capabilities declared across the tree.
    pub declared_capabilities: HashSet<String>,
    /// All capabilities required (used) across the tree.
    pub used_capabilities: HashSet<String>,
}

impl CompletenessReport {
    /// Returns `true` if the codebase is fully complete for production.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.total_modules == self.modules_with_context
            && self.total_public_fns == self.public_fns_with_context
            && self.total_effect_ops == self.handled_effect_ops
            && self
                .used_capabilities
                .is_subset(&self.declared_capabilities)
    }

    /// Returns a human-readable summary of the completeness report.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "Modules with @context: {}/{}",
            self.modules_with_context, self.total_modules
        ));
        lines.push(format!(
            "Public functions with @context: {}/{}",
            self.public_fns_with_context, self.total_public_fns
        ));
        lines.push(format!(
            "Effect operations handled: {}/{}",
            self.handled_effect_ops, self.total_effect_ops
        ));
        if !self.used_capabilities.is_empty() {
            let undeclared: Vec<_> = self
                .used_capabilities
                .difference(&self.declared_capabilities)
                .collect();
            if undeclared.is_empty() {
                lines.push("All capabilities declared.".to_string());
            } else {
                lines.push(format!("Undeclared capabilities: {:?}", undeclared));
            }
        }
        if self.is_complete() {
            lines.push("Status: COMPLETE".to_string());
        } else {
            lines.push("Status: INCOMPLETE".to_string());
        }
        lines.join("\n")
    }
}

/// Verify capabilities and effect handling across one or more modules.
///
/// This is the final C-AIR pass. It checks:
/// - All effect operations are handled (either by `handling` blocks or `with` clauses).
/// - All capabilities used by child declarations are declared by parent scopes.
/// - In production mode: all modules and public functions have `@context`.
///
/// Returns a tuple of ([`DiagnosticBag`], [`CompletenessReport`]).
#[must_use]
pub fn verify_capabilities(
    modules: &[&AIRNode],
    mode: VerificationMode,
) -> (DiagnosticBag, CompletenessReport) {
    let mut diags = DiagnosticBag::new();
    let mut report = CompletenessReport {
        total_modules: 0,
        modules_with_context: 0,
        total_public_fns: 0,
        public_fns_with_context: 0,
        total_effect_ops: 0,
        handled_effect_ops: 0,
        declared_capabilities: HashSet::new(),
        used_capabilities: HashSet::new(),
    };

    for module in modules {
        verify_module(module, mode, &mut diags, &mut report);
    }

    (diags, report)
}

/// Verify a single module node.
fn verify_module(
    module: &AIRNode,
    mode: VerificationMode,
    diags: &mut DiagnosticBag,
    report: &mut CompletenessReport,
) {
    if let NodeKind::Module { items, .. } = &module.kind {
        report.total_modules += 1;
        if module.context.is_some() {
            report.modules_with_context += 1;
        } else if mode == VerificationMode::Production {
            diags.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 8022,
                },
                "module is missing @context annotation (required in production mode)".to_string(),
                module.span,
            );
        }

        // Collect module-level declared capabilities.
        let module_caps = collect_declared_capabilities(module);
        for cap in &module_caps {
            report.declared_capabilities.insert(cap.name.clone());
        }

        // Collect module-level handled effects (from ModuleHandle declarations).
        let module_handled_effects = collect_module_handled_effects(items);

        for item in items {
            verify_item(
                item,
                mode,
                &module_caps,
                &module_handled_effects,
                diags,
                report,
            );
        }
    }
}

/// Collect effects handled at module level via `handle Effect with handler` declarations.
fn collect_module_handled_effects(items: &[AIRNode]) -> HashSet<String> {
    let mut handled = HashSet::new();
    for item in items {
        if let NodeKind::ModuleHandle { effect, .. } = &item.kind {
            let name = effect
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            handled.insert(name);
        }
    }
    handled
}

/// Verify a top-level item (function, class, trait, etc.).
fn verify_item(
    item: &AIRNode,
    mode: VerificationMode,
    parent_caps: &HashSet<Capability>,
    module_handled_effects: &HashSet<String>,
    diags: &mut DiagnosticBag,
    report: &mut CompletenessReport,
) {
    match &item.kind {
        NodeKind::FnDecl {
            visibility,
            name,
            effect_clause,
            body,
            ..
        } => {
            // Track public function context completeness.
            if *visibility == Visibility::Public {
                report.total_public_fns += 1;
                if item.context.is_some() {
                    report.public_fns_with_context += 1;
                } else if mode == VerificationMode::Production {
                    diags.error(
                        DiagnosticCode {
                            prefix: 'E',
                            number: 8023,
                        },
                        format!(
                            "public function `{}` is missing @context annotation (required in production mode)",
                            name.name
                        ),
                        item.span,
                    );
                }
            }

            // Collect function-level declared capabilities.
            let mut fn_caps = parent_caps.clone();
            let fn_declared = collect_declared_capabilities(item);
            for cap in &fn_declared {
                fn_caps.insert(cap.clone());
                report.declared_capabilities.insert(cap.name.clone());
            }

            // Collect declared effects from the `with` clause.
            let mut declared_effects: HashSet<String> = module_handled_effects.clone();
            for eff in effect_clause {
                let eff_name = eff
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                declared_effects.insert(eff_name);
            }

            // Track which declared effects are actually used.
            let mut used_effects: HashSet<String> = HashSet::new();

            // Verify the function body.
            verify_body(
                body,
                &fn_caps,
                &declared_effects,
                &mut used_effects,
                diags,
                report,
            );

            // Warn about declared but unused effects (only in production mode).
            if mode == VerificationMode::Production {
                for eff in effect_clause {
                    let eff_name = eff
                        .segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(".");
                    if !used_effects.contains(&eff_name) {
                        diags.warning(
                            DiagnosticCode {
                                prefix: 'W',
                                number: 8020,
                            },
                            format!(
                                "effect `{eff_name}` declared in `with` clause of `{}` but never used",
                                name.name
                            ),
                            item.span,
                        );
                    }
                }
            }
        }
        NodeKind::ClassDecl {
            visibility,
            name,
            methods,
            ..
        } => {
            if *visibility == Visibility::Public {
                report.total_public_fns += 1; // counts as public item
                if item.context.is_some() {
                    report.public_fns_with_context += 1;
                } else if mode == VerificationMode::Production {
                    diags.error(
                        DiagnosticCode {
                            prefix: 'E',
                            number: 8023,
                        },
                        format!(
                            "public class `{}` is missing @context annotation (required in production mode)",
                            name.name
                        ),
                        item.span,
                    );
                }
            }
            let mut class_caps = parent_caps.clone();
            let class_declared = collect_declared_capabilities(item);
            for cap in &class_declared {
                class_caps.insert(cap.clone());
                report.declared_capabilities.insert(cap.name.clone());
            }
            for method in methods {
                verify_item(
                    method,
                    mode,
                    &class_caps,
                    module_handled_effects,
                    diags,
                    report,
                );
            }
        }
        NodeKind::TraitDecl {
            visibility,
            name,
            methods,
            ..
        } => {
            if *visibility == Visibility::Public {
                report.total_public_fns += 1;
                if item.context.is_some() {
                    report.public_fns_with_context += 1;
                } else if mode == VerificationMode::Production {
                    diags.error(
                        DiagnosticCode {
                            prefix: 'E',
                            number: 8023,
                        },
                        format!(
                            "public trait `{}` is missing @context annotation (required in production mode)",
                            name.name
                        ),
                        item.span,
                    );
                }
            }
            let mut trait_caps = parent_caps.clone();
            let trait_declared = collect_declared_capabilities(item);
            for cap in &trait_declared {
                trait_caps.insert(cap.clone());
                report.declared_capabilities.insert(cap.name.clone());
            }
            for method in methods {
                verify_item(
                    method,
                    mode,
                    &trait_caps,
                    module_handled_effects,
                    diags,
                    report,
                );
            }
        }
        NodeKind::ImplBlock { methods, .. } => {
            let mut impl_caps = parent_caps.clone();
            let impl_declared = collect_declared_capabilities(item);
            for cap in &impl_declared {
                impl_caps.insert(cap.clone());
                report.declared_capabilities.insert(cap.name.clone());
            }
            for method in methods {
                verify_item(
                    method,
                    mode,
                    &impl_caps,
                    module_handled_effects,
                    diags,
                    report,
                );
            }
        }
        NodeKind::RecordDecl {
            visibility, name, ..
        }
        | NodeKind::EnumDecl {
            visibility, name, ..
        } if *visibility == Visibility::Public => {
            report.total_public_fns += 1;
            if item.context.is_some() {
                report.public_fns_with_context += 1;
            } else if mode == VerificationMode::Production {
                diags.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 8023,
                    },
                    format!(
                        "public type `{}` is missing @context annotation (required in production mode)",
                        name.name
                    ),
                    item.span,
                );
            }
        }
        _ => {}
    }
}

/// Collect capabilities declared via `@requires` on a node's context.
fn collect_declared_capabilities(node: &AIRNode) -> HashSet<Capability> {
    node.context
        .as_ref()
        .map(|ctx| ctx.capabilities.clone())
        .unwrap_or_default()
}

/// Verify a function/method body for effect handling and capability usage.
///
/// `declared_effects` tracks effects that are handled (via `handling` blocks or
/// `with` clause). `used_effects` accumulates which declared effects were actually used.
fn verify_body(
    node: &AIRNode,
    declared_caps: &HashSet<Capability>,
    declared_effects: &HashSet<String>,
    used_effects: &mut HashSet<String>,
    diags: &mut DiagnosticBag,
    report: &mut CompletenessReport,
) {
    match &node.kind {
        NodeKind::EffectOp {
            effect, operation, ..
        } => {
            let effect_name = effect
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            report.total_effect_ops += 1;
            if declared_effects.contains(&effect_name) {
                report.handled_effect_ops += 1;
                used_effects.insert(effect_name);
            } else {
                diags.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 8020,
                    },
                    format!(
                        "effect operation `{}.{}` has no handler; \
                         add a `handling` block or declare `with {}` in the function signature",
                        effect_name, operation.name, effect_name
                    ),
                    node.span,
                );
            }
        }
        NodeKind::HandlingBlock { handlers, body, .. } => {
            // Collect effects handled by this handling block.
            let mut inner_effects = declared_effects.clone();
            for handler in handlers {
                let eff_name = handler
                    .effect
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                inner_effects.insert(eff_name);
            }
            // Verify handler expressions.
            for handler in handlers {
                verify_body(
                    &handler.handler,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
            // Verify body with additional handled effects.
            verify_body(
                body,
                declared_caps,
                &inner_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::Call { callee, args, .. } => {
            // Check if the callee has capabilities we need.
            check_callee_capabilities(callee, declared_caps, diags, report);
            // Recurse into callee and args.
            verify_body(
                callee,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            for arg in args {
                verify_body(
                    &arg.value,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
        }
        NodeKind::MethodCall { receiver, args, .. } => {
            verify_body(
                receiver,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            for arg in args {
                verify_body(
                    &arg.value,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
        }
        _ => {
            // Recurse into all children generically.
            verify_body_children(
                node,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
    }
}

/// Check if a callee node requires capabilities not declared in the current scope.
fn check_callee_capabilities(
    callee: &AIRNode,
    declared_caps: &HashSet<Capability>,
    diags: &mut DiagnosticBag,
    report: &mut CompletenessReport,
) {
    // If the callee has a context block with capabilities, the caller must declare them too.
    if let Some(ctx) = &callee.context {
        for cap in &ctx.capabilities {
            report.used_capabilities.insert(cap.name.clone());
            if !declared_caps.contains(cap) {
                diags.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 8021,
                    },
                    format!(
                        "callee requires capability `{}` which is not declared in the current scope; \
                         add `@requires(Capability.{})` to the enclosing function or module",
                        cap.name, cap.name
                    ),
                    callee.span,
                );
            }
        }
    }
    // Also check the node's own capabilities slot (populated by T-AIR).
    for cap in &callee.capabilities {
        report.used_capabilities.insert(cap.name.clone());
        if !declared_caps.contains(cap) {
            diags.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 8021,
                },
                format!(
                    "callee requires capability `{}` which is not declared in the current scope",
                    cap.name
                ),
                callee.span,
            );
        }
    }
}

/// Recurse into children of a node for body verification.
fn verify_body_children(
    node: &AIRNode,
    declared_caps: &HashSet<Capability>,
    declared_effects: &HashSet<String>,
    used_effects: &mut HashSet<String>,
    diags: &mut DiagnosticBag,
    report: &mut CompletenessReport,
) {
    match &node.kind {
        NodeKind::Block { stmts, tail, .. } => {
            for stmt in stmts {
                verify_body(
                    stmt,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
            if let Some(t) = tail {
                verify_body(
                    t,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
        }
        NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            verify_body(
                condition,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            verify_body(
                then_block,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            if let Some(e) = else_block {
                verify_body(
                    e,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
        }
        NodeKind::Match {
            scrutinee, arms, ..
        } => {
            verify_body(
                scrutinee,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            for arm in arms {
                verify_body(
                    arm,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
        }
        NodeKind::MatchArm {
            pattern,
            guard,
            body,
            ..
        } => {
            verify_body(
                pattern,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            if let Some(g) = guard {
                verify_body(
                    g,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
            verify_body(
                body,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            verify_body(
                pattern,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            verify_body(
                iterable,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            verify_body(
                body,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::While {
            condition, body, ..
        } => {
            verify_body(
                condition,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            verify_body(
                body,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::Loop { body, .. } => {
            verify_body(
                body,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::LetBinding { value, .. } => {
            verify_body(
                value,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::Return { value: Some(v), .. } => {
            verify_body(
                v,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::BinaryOp { left, right, .. } => {
            verify_body(
                left,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            verify_body(
                right,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::UnaryOp { operand, .. } => {
            verify_body(
                operand,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::Assign { target, value, .. } => {
            verify_body(
                target,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            verify_body(
                value,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::FieldAccess { object, .. } => {
            verify_body(
                object,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::Index { object, index, .. } => {
            verify_body(
                object,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            verify_body(
                index,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::Lambda { body, .. } => {
            verify_body(
                body,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::Pipe { left, right, .. } | NodeKind::Compose { left, right, .. } => {
            verify_body(
                left,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            verify_body(
                right,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::Await { expr, .. }
        | NodeKind::Propagate { expr, .. }
        | NodeKind::Move { expr, .. }
        | NodeKind::Borrow { expr, .. }
        | NodeKind::MutableBorrow { expr, .. } => {
            verify_body(
                expr,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::Guard {
            condition,
            else_block,
            ..
        } => {
            verify_body(
                condition,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            verify_body(
                else_block,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::ListLiteral { elems, .. }
        | NodeKind::SetLiteral { elems, .. }
        | NodeKind::TupleLiteral { elems, .. } => {
            for elem in elems {
                verify_body(
                    elem,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
        }
        NodeKind::MapLiteral { entries, .. } => {
            for entry in entries {
                verify_body(
                    &entry.key,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
                verify_body(
                    &entry.value,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
        }
        NodeKind::Range { lo, hi, .. } => {
            verify_body(
                lo,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
            verify_body(
                hi,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::RecordConstruct { fields, spread, .. } => {
            for field in fields {
                if let Some(v) = &field.value {
                    verify_body(
                        v,
                        declared_caps,
                        declared_effects,
                        used_effects,
                        diags,
                        report,
                    );
                }
            }
            if let Some(s) = spread {
                verify_body(
                    s,
                    declared_caps,
                    declared_effects,
                    used_effects,
                    diags,
                    report,
                );
            }
        }
        NodeKind::Interpolation { parts, .. } => {
            for part in parts {
                if let crate::node::AirInterpolationPart::Expr(e) = part {
                    verify_body(
                        e,
                        declared_caps,
                        declared_effects,
                        used_effects,
                        diags,
                        report,
                    );
                }
            }
        }
        NodeKind::ResultConstruct { value: Some(v), .. } => {
            verify_body(
                v,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        NodeKind::Break { value: Some(v), .. } => {
            verify_body(
                v,
                declared_caps,
                declared_effects,
                used_effects,
                diags,
                report,
            );
        }
        // Leaf nodes and other variants don't need recursion.
        _ => {}
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{AirHandlerPair, NodeIdGen, NodeKind};
    use crate::stubs::{Capability, ContextBlock};
    use bock_ast::{Ident, TypePath, Visibility};
    use bock_errors::Span;
    use std::collections::HashSet;

    fn test_span() -> Span {
        Span::dummy()
    }

    fn ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: test_span(),
        }
    }

    fn type_path(name: &str) -> TypePath {
        TypePath {
            segments: vec![ident(name)],
            span: test_span(),
        }
    }

    fn empty_block(id_gen: &NodeIdGen) -> AIRNode {
        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        )
    }

    fn fn_decl(
        id_gen: &NodeIdGen,
        name: &str,
        visibility: Visibility,
        effect_clause: Vec<TypePath>,
        body: AIRNode,
    ) -> AIRNode {
        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause,
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    fn module_with(id_gen: &NodeIdGen, items: Vec<AIRNode>) -> AIRNode {
        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items,
            },
        )
    }

    fn effect_op_node(id_gen: &NodeIdGen, effect: &str, operation: &str) -> AIRNode {
        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::EffectOp {
                effect: type_path(effect),
                operation: ident(operation),
                args: vec![],
            },
        )
    }

    fn handling_block(
        id_gen: &NodeIdGen,
        handlers: Vec<(&str, AIRNode)>,
        body: AIRNode,
    ) -> AIRNode {
        let pairs = handlers
            .into_iter()
            .map(|(name, handler)| AirHandlerPair {
                effect: type_path(name),
                handler: Box::new(handler),
            })
            .collect();
        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::HandlingBlock {
                handlers: pairs,
                body: Box::new(body),
            },
        )
    }

    // ── Effect handler completeness ──────────────────────────────────────────

    #[test]
    fn effect_op_with_with_clause_is_handled() {
        let id_gen = NodeIdGen::new();
        let effect_op = effect_op_node(&id_gen, "Log", "info");
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![effect_op],
                tail: None,
            },
        );
        let func = fn_decl(
            &id_gen,
            "do_stuff",
            Visibility::Private,
            vec![type_path("Log")],
            body,
        );
        let module = module_with(&id_gen, vec![func]);

        let (diags, report) = verify_capabilities(&[&module], VerificationMode::Development);
        assert_eq!(diags.error_count(), 0);
        assert_eq!(report.total_effect_ops, 1);
        assert_eq!(report.handled_effect_ops, 1);
    }

    #[test]
    fn effect_op_without_handler_produces_error() {
        let id_gen = NodeIdGen::new();
        let effect_op = effect_op_node(&id_gen, "Log", "info");
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![effect_op],
                tail: None,
            },
        );
        // No effect_clause, no handling block.
        let func = fn_decl(&id_gen, "do_stuff", Visibility::Private, vec![], body);
        let module = module_with(&id_gen, vec![func]);

        let (diags, report) = verify_capabilities(&[&module], VerificationMode::Development);
        assert_eq!(diags.error_count(), 1);
        assert_eq!(report.total_effect_ops, 1);
        assert_eq!(report.handled_effect_ops, 0);
    }

    #[test]
    fn effect_op_inside_handling_block_is_handled() {
        let id_gen = NodeIdGen::new();
        let effect_op = effect_op_node(&id_gen, "Log", "info");
        let inner_body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![effect_op],
                tail: None,
            },
        );
        let handler_expr = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Identifier {
                name: ident("console_handler"),
            },
        );
        let handling = handling_block(&id_gen, vec![("Log", handler_expr)], inner_body);
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![handling],
                tail: None,
            },
        );
        let func = fn_decl(&id_gen, "do_stuff", Visibility::Private, vec![], body);
        let module = module_with(&id_gen, vec![func]);

        let (diags, report) = verify_capabilities(&[&module], VerificationMode::Development);
        assert_eq!(diags.error_count(), 0);
        assert_eq!(report.total_effect_ops, 1);
        assert_eq!(report.handled_effect_ops, 1);
    }

    #[test]
    fn unhandled_effect_not_covered_by_handling_block() {
        let id_gen = NodeIdGen::new();
        // EffectOp for "Db" but handling block only handles "Log".
        let effect_op = effect_op_node(&id_gen, "Db", "query");
        let inner_body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![effect_op],
                tail: None,
            },
        );
        let handler_expr = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Identifier {
                name: ident("console_handler"),
            },
        );
        let handling = handling_block(&id_gen, vec![("Log", handler_expr)], inner_body);
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![handling],
                tail: None,
            },
        );
        let func = fn_decl(&id_gen, "do_stuff", Visibility::Private, vec![], body);
        let module = module_with(&id_gen, vec![func]);

        let (diags, _) = verify_capabilities(&[&module], VerificationMode::Development);
        assert_eq!(diags.error_count(), 1, "Db effect should be unhandled");
    }

    // ── Production mode completeness ─────────────────────────────────────────

    #[test]
    fn production_module_without_context_error() {
        let id_gen = NodeIdGen::new();
        let module = module_with(&id_gen, vec![]);

        let (diags, report) = verify_capabilities(&[&module], VerificationMode::Production);
        assert!(
            diags.error_count() > 0,
            "should error on module without @context"
        );
        assert_eq!(report.total_modules, 1);
        assert_eq!(report.modules_with_context, 0);
    }

    #[test]
    fn production_module_with_context_ok() {
        let id_gen = NodeIdGen::new();
        let mut module = module_with(&id_gen, vec![]);
        module.context = Some(ContextBlock {
            context_text: Some("Payment module.".to_string()),
            ..Default::default()
        });

        let (diags, report) = verify_capabilities(&[&module], VerificationMode::Production);
        assert_eq!(diags.error_count(), 0);
        assert_eq!(report.modules_with_context, 1);
    }

    #[test]
    fn production_public_fn_without_context_error() {
        let id_gen = NodeIdGen::new();
        let body = empty_block(&id_gen);
        let func = fn_decl(&id_gen, "process", Visibility::Public, vec![], body);
        let mut module = module_with(&id_gen, vec![func]);
        module.context = Some(ContextBlock {
            context_text: Some("Module.".to_string()),
            ..Default::default()
        });

        let (diags, report) = verify_capabilities(&[&module], VerificationMode::Production);
        assert!(
            diags.error_count() > 0,
            "should error on public fn without context"
        );
        assert_eq!(report.total_public_fns, 1);
        assert_eq!(report.public_fns_with_context, 0);
    }

    #[test]
    fn production_public_fn_with_context_ok() {
        let id_gen = NodeIdGen::new();
        let body = empty_block(&id_gen);
        let mut func = fn_decl(&id_gen, "process", Visibility::Public, vec![], body);
        func.context = Some(ContextBlock {
            context_text: Some("Process payments.".to_string()),
            ..Default::default()
        });
        let mut module = module_with(&id_gen, vec![func]);
        module.context = Some(ContextBlock {
            context_text: Some("Module.".to_string()),
            ..Default::default()
        });

        let (diags, report) = verify_capabilities(&[&module], VerificationMode::Production);
        assert_eq!(diags.error_count(), 0);
        assert_eq!(report.total_public_fns, 1);
        assert_eq!(report.public_fns_with_context, 1);
    }

    #[test]
    fn production_private_fn_without_context_ok() {
        let id_gen = NodeIdGen::new();
        let body = empty_block(&id_gen);
        let func = fn_decl(&id_gen, "helper", Visibility::Private, vec![], body);
        let mut module = module_with(&id_gen, vec![func]);
        module.context = Some(ContextBlock {
            context_text: Some("Module.".to_string()),
            ..Default::default()
        });

        let (diags, _) = verify_capabilities(&[&module], VerificationMode::Production);
        assert_eq!(diags.error_count(), 0);
    }

    #[test]
    fn development_mode_no_context_requirements() {
        let id_gen = NodeIdGen::new();
        let body = empty_block(&id_gen);
        let func = fn_decl(&id_gen, "process", Visibility::Public, vec![], body);
        let module = module_with(&id_gen, vec![func]);

        let (diags, _) = verify_capabilities(&[&module], VerificationMode::Development);
        // Development mode should not error on missing context.
        assert_eq!(diags.error_count(), 0);
    }

    // ── Capability propagation ───────────────────────────────────────────────

    #[test]
    fn callee_capability_not_declared_error() {
        let id_gen = NodeIdGen::new();
        // Simulate a call to a function that requires Network.
        let mut callee_node = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Identifier {
                name: ident("fetch_data"),
            },
        );
        callee_node.context = Some(ContextBlock {
            capabilities: {
                let mut s = HashSet::new();
                s.insert(Capability::new("Network"));
                s
            },
            ..Default::default()
        });
        let call = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Call {
                callee: Box::new(callee_node),
                args: vec![],
                type_args: vec![],
            },
        );
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![call],
                tail: None,
            },
        );
        // Caller does not declare Network.
        let func = fn_decl(&id_gen, "process", Visibility::Private, vec![], body);
        let module = module_with(&id_gen, vec![func]);

        let (diags, _) = verify_capabilities(&[&module], VerificationMode::Development);
        assert!(
            diags.error_count() > 0,
            "should error on missing capability declaration"
        );
    }

    #[test]
    fn callee_capability_declared_ok() {
        let id_gen = NodeIdGen::new();
        let mut callee_node = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Identifier {
                name: ident("fetch_data"),
            },
        );
        callee_node.context = Some(ContextBlock {
            capabilities: {
                let mut s = HashSet::new();
                s.insert(Capability::new("Network"));
                s
            },
            ..Default::default()
        });
        let call = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Call {
                callee: Box::new(callee_node),
                args: vec![],
                type_args: vec![],
            },
        );
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![call],
                tail: None,
            },
        );
        let mut func = fn_decl(&id_gen, "process", Visibility::Private, vec![], body);
        func.context = Some(ContextBlock {
            capabilities: {
                let mut s = HashSet::new();
                s.insert(Capability::new("Network"));
                s
            },
            ..Default::default()
        });
        let module = module_with(&id_gen, vec![func]);

        let (diags, _) = verify_capabilities(&[&module], VerificationMode::Development);
        assert_eq!(diags.error_count(), 0);
    }

    #[test]
    fn module_level_capability_covers_children() {
        let id_gen = NodeIdGen::new();
        let mut callee_node = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Identifier {
                name: ident("fetch_data"),
            },
        );
        callee_node.context = Some(ContextBlock {
            capabilities: {
                let mut s = HashSet::new();
                s.insert(Capability::new("Network"));
                s
            },
            ..Default::default()
        });
        let call = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Call {
                callee: Box::new(callee_node),
                args: vec![],
                type_args: vec![],
            },
        );
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![call],
                tail: None,
            },
        );
        // Function does NOT declare Network, but module does.
        let func = fn_decl(&id_gen, "process", Visibility::Private, vec![], body);
        let mut module = module_with(&id_gen, vec![func]);
        module.context = Some(ContextBlock {
            capabilities: {
                let mut s = HashSet::new();
                s.insert(Capability::new("Network"));
                s
            },
            ..Default::default()
        });

        let (diags, _) = verify_capabilities(&[&module], VerificationMode::Development);
        assert_eq!(
            diags.error_count(),
            0,
            "module-level capability should cover children"
        );
    }

    // ── Completeness report ──────────────────────────────────────────────────

    #[test]
    fn completeness_report_complete() {
        let report = CompletenessReport {
            total_modules: 1,
            modules_with_context: 1,
            total_public_fns: 2,
            public_fns_with_context: 2,
            total_effect_ops: 3,
            handled_effect_ops: 3,
            declared_capabilities: ["Network".to_string()].into(),
            used_capabilities: ["Network".to_string()].into(),
        };
        assert!(report.is_complete());
        assert!(report.summary().contains("COMPLETE"));
    }

    #[test]
    fn completeness_report_incomplete() {
        let report = CompletenessReport {
            total_modules: 2,
            modules_with_context: 1,
            total_public_fns: 3,
            public_fns_with_context: 2,
            total_effect_ops: 1,
            handled_effect_ops: 0,
            declared_capabilities: HashSet::new(),
            used_capabilities: ["Network".to_string()].into(),
        };
        assert!(!report.is_complete());
        assert!(report.summary().contains("INCOMPLETE"));
    }

    // ── Unused effect warning ────────────────────────────────────────────────

    #[test]
    fn unused_effect_in_with_clause_warns_production() {
        let id_gen = NodeIdGen::new();
        let body = empty_block(&id_gen);
        // Declare "Log" in with clause but never use it.
        let mut func = fn_decl(
            &id_gen,
            "do_stuff",
            Visibility::Private,
            vec![type_path("Log")],
            body,
        );
        func.context = Some(ContextBlock::default());
        let mut module = module_with(&id_gen, vec![func]);
        module.context = Some(ContextBlock {
            context_text: Some("Module.".to_string()),
            ..Default::default()
        });

        let (diags, _) = verify_capabilities(&[&module], VerificationMode::Production);
        assert!(
            diags.warning_count() > 0,
            "should warn about unused effect in with clause"
        );
    }

    #[test]
    fn unused_effect_in_with_clause_no_warn_development() {
        let id_gen = NodeIdGen::new();
        let body = empty_block(&id_gen);
        let func = fn_decl(
            &id_gen,
            "do_stuff",
            Visibility::Private,
            vec![type_path("Log")],
            body,
        );
        let module = module_with(&id_gen, vec![func]);

        let (diags, _) = verify_capabilities(&[&module], VerificationMode::Development);
        assert_eq!(
            diags.warning_count(),
            0,
            "development mode should not warn about unused effects"
        );
    }

    // ── Multi-module ─────────────────────────────────────────────────────────

    #[test]
    fn multi_module_verification() {
        let id_gen = NodeIdGen::new();

        let body1 = empty_block(&id_gen);
        let mut func1 = fn_decl(&id_gen, "f1", Visibility::Public, vec![], body1);
        func1.context = Some(ContextBlock {
            context_text: Some("F1.".to_string()),
            ..Default::default()
        });
        let mut mod1 = module_with(&id_gen, vec![func1]);
        mod1.context = Some(ContextBlock {
            context_text: Some("Module 1.".to_string()),
            ..Default::default()
        });

        let body2 = empty_block(&id_gen);
        let mut func2 = fn_decl(&id_gen, "f2", Visibility::Public, vec![], body2);
        func2.context = Some(ContextBlock {
            context_text: Some("F2.".to_string()),
            ..Default::default()
        });
        let mut mod2 = module_with(&id_gen, vec![func2]);
        mod2.context = Some(ContextBlock {
            context_text: Some("Module 2.".to_string()),
            ..Default::default()
        });

        let (diags, report) = verify_capabilities(&[&mod1, &mod2], VerificationMode::Production);
        assert_eq!(diags.error_count(), 0);
        assert_eq!(report.total_modules, 2);
        assert_eq!(report.modules_with_context, 2);
        assert_eq!(report.total_public_fns, 2);
        assert_eq!(report.public_fns_with_context, 2);
        assert!(report.is_complete());
    }

    // ── Module-level handle ──────────────────────────────────────────────────

    #[test]
    fn module_handle_covers_effect_ops() {
        let id_gen = NodeIdGen::new();
        let effect_op = effect_op_node(&id_gen, "Log", "info");
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![effect_op],
                tail: None,
            },
        );
        // No with clause on function.
        let func = fn_decl(&id_gen, "do_stuff", Visibility::Private, vec![], body);
        // Module-level handle for Log.
        let module_handle = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::ModuleHandle {
                effect: type_path("Log"),
                handler: Box::new(AIRNode::new(
                    id_gen.next(),
                    test_span(),
                    NodeKind::Identifier {
                        name: ident("console_handler"),
                    },
                )),
            },
        );
        let module = module_with(&id_gen, vec![module_handle, func]);

        let (diags, report) = verify_capabilities(&[&module], VerificationMode::Development);
        assert_eq!(diags.error_count(), 0);
        assert_eq!(report.total_effect_ops, 1);
        assert_eq!(report.handled_effect_ops, 1);
    }
}
