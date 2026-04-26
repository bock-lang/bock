//! Context validation pass — validates context annotations across the AIR tree.
//!
//! This pass runs after [`crate::context::interpret_context`] and checks:
//! 1. **Capability consistency**: child `@requires` propagate to parents (additive).
//! 2. **Security consistency**: security levels don't contradict in parent-child.
//! 3. **Performance budget validity**: values are positive and well-formed.
//! 4. **Completeness**: in strict mode, public items must have context annotations.

use std::collections::HashSet;

use bock_ast::Visibility;
use bock_errors::{DiagnosticBag, DiagnosticCode};

use crate::node::{AIRNode, NodeKind};
use crate::stubs::{security_level_rank, Capability, ContextBlock, SecurityInfo, SECURITY_LEVELS};

/// Strictness level for context validation.
///
/// Three profiles with increasing strictness:
/// - **Lax** (sketch mode): only error-level validations — contradictions, invalid
///   values. No completeness warnings. Auto-inference is assumed at this level.
/// - **Standard** (development mode): lax checks + **warnings** on public items
///   and modules missing context annotations.
/// - **Strict** (production mode): standard checks but missing context annotations
///   become **errors** instead of warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictnessLevel {
    /// Lax / Sketch: only error-level validations (contradictions, invalid values).
    /// No completeness warnings; auto-infer capabilities.
    Lax,
    /// Standard / Development: lax + warnings for public items without context
    /// and undeclared effects.
    Standard,
    /// Strict / Production: standard but missing context / undeclared effects
    /// are errors, not warnings.
    Strict,
}

impl StrictnessLevel {
    /// Map a profile name string to a strictness level.
    ///
    /// Recognised names (case-insensitive):
    /// - `"sketch"` / `"lax"` → [`Lax`](StrictnessLevel::Lax)
    /// - `"development"` / `"standard"` → [`Standard`](StrictnessLevel::Standard)
    /// - `"production"` / `"strict"` → [`Strict`](StrictnessLevel::Strict)
    ///
    /// Returns `None` for unrecognised names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "sketch" | "lax" => Some(Self::Lax),
            "development" | "standard" => Some(Self::Standard),
            "production" | "strict" => Some(Self::Strict),
            _ => None,
        }
    }
}

/// Validates context annotations across the AIR tree.
///
/// Walks the tree and checks:
/// - Security levels are consistent (children don't have lower sensitivity than parents).
/// - Capabilities propagate upward correctly.
/// - Performance budget values are positive.
/// - In strict mode: all public items and modules have context annotations.
///
/// Returns a [`DiagnosticBag`] with any errors/warnings.
#[must_use]
pub fn validate_context(root: &AIRNode, strictness: StrictnessLevel) -> DiagnosticBag {
    let mut diags = DiagnosticBag::new();
    validate_node(root, None, &HashSet::new(), strictness, &mut diags);
    diags
}

/// Validate a single node and recurse into children.
///
/// `parent_security` is the security info inherited from the nearest ancestor with one.
/// `parent_capabilities` is the union of all ancestor-declared capabilities.
fn validate_node(
    node: &AIRNode,
    parent_security: Option<&SecurityInfo>,
    parent_capabilities: &HashSet<Capability>,
    strictness: StrictnessLevel,
    diags: &mut DiagnosticBag,
) {
    // Compute the effective security and capabilities for this node.
    let node_security = node
        .context
        .as_ref()
        .and_then(|c| c.security.as_ref())
        .or(parent_security);

    // @requires is additive: child capabilities union with parent capabilities.
    let mut effective_caps = parent_capabilities.clone();
    if let Some(ctx) = &node.context {
        for cap in &ctx.capabilities {
            effective_caps.insert(cap.clone());
        }
    }

    // Validate this node's context block.
    if let Some(ctx) = &node.context {
        validate_security_consistency(ctx, parent_security, node.span, diags);
        validate_performance_budget(ctx, node.span, diags);
        validate_security_level_known(ctx, node.span, diags);
    }

    // Completeness checking in standard and strict modes.
    if strictness == StrictnessLevel::Standard || strictness == StrictnessLevel::Strict {
        validate_completeness(node, strictness, diags);
    }

    // Recurse into children.
    validate_children(node, node_security, &effective_caps, strictness, diags);
}

/// Check that a node's security level doesn't contradict its parent's.
///
/// A child with *lower* sensitivity than its parent is a contradiction:
/// the child would leak the parent's classification.
fn validate_security_consistency(
    ctx: &ContextBlock,
    parent_security: Option<&SecurityInfo>,
    span: bock_errors::Span,
    diags: &mut DiagnosticBag,
) {
    let Some(child_sec) = &ctx.security else {
        return;
    };
    let Some(parent_sec) = parent_security else {
        return;
    };

    let parent_rank = security_level_rank(&parent_sec.level);
    let child_rank = security_level_rank(&child_sec.level);

    if let (Some(p), Some(c)) = (parent_rank, child_rank) {
        if c < p {
            diags.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 8011,
                },
                format!(
                    "security level `{}` is less restrictive than parent level `{}`",
                    child_sec.level, parent_sec.level
                ),
                span,
            );
        }
    }

    // PII contradiction: parent says pii=true but child says pii=false.
    if parent_sec.pii && !child_sec.pii {
        diags.warning(
            DiagnosticCode {
                prefix: 'W',
                number: 8011,
            },
            "child declares pii=false but parent declares pii=true; PII status is inherited"
                .to_string(),
            span,
        );
    }
}

/// Check that security level strings are recognized.
fn validate_security_level_known(
    ctx: &ContextBlock,
    span: bock_errors::Span,
    diags: &mut DiagnosticBag,
) {
    if let Some(sec) = &ctx.security {
        if !sec.level.is_empty() && security_level_rank(&sec.level).is_none() {
            diags.warning(
                DiagnosticCode {
                    prefix: 'W',
                    number: 8015,
                },
                format!(
                    "unknown security level `{}`; known levels are: {}",
                    sec.level,
                    SECURITY_LEVELS.join(", ")
                ),
                span,
            );
        }
    }
}

/// Check that performance budget values are valid (positive and non-zero).
fn validate_performance_budget(
    ctx: &ContextBlock,
    span: bock_errors::Span,
    diags: &mut DiagnosticBag,
) {
    if let Some(perf) = &ctx.performance {
        if let Some(lat) = &perf.max_latency {
            if lat.value <= 0.0 {
                diags.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 8016,
                    },
                    "performance max_latency must be a positive value".to_string(),
                    span,
                );
            }
        }
        if let Some(mem) = &perf.max_memory {
            if mem.value <= 0.0 {
                diags.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 8016,
                    },
                    "performance max_memory must be a positive value".to_string(),
                    span,
                );
            }
        }
    }
}

/// Check that public items and modules have context annotations.
///
/// In **Standard** mode, missing annotations produce **warnings**.
/// In **Strict** mode, missing annotations produce **errors**.
fn validate_completeness(node: &AIRNode, strictness: StrictnessLevel, diags: &mut DiagnosticBag) {
    let is_strict = strictness == StrictnessLevel::Strict;
    let mode_label = if is_strict { "production" } else { "standard" };

    match &node.kind {
        NodeKind::Module { .. } => {
            if node.context.is_none() {
                if is_strict {
                    diags.error(
                        DiagnosticCode {
                            prefix: 'E',
                            number: 8014,
                        },
                        format!(
                            "module is missing @context annotation (required in {mode_label} mode)"
                        ),
                        node.span,
                    );
                } else {
                    diags.warning(
                        DiagnosticCode {
                            prefix: 'W',
                            number: 8014,
                        },
                        format!("module is missing @context annotation (recommended in {mode_label} mode)"),
                        node.span,
                    );
                }
            }
        }
        NodeKind::FnDecl {
            visibility: Visibility::Public,
            name,
            ..
        }
        | NodeKind::ClassDecl {
            visibility: Visibility::Public,
            name,
            ..
        }
        | NodeKind::TraitDecl {
            visibility: Visibility::Public,
            name,
            ..
        }
        | NodeKind::RecordDecl {
            visibility: Visibility::Public,
            name,
            ..
        }
        | NodeKind::EnumDecl {
            visibility: Visibility::Public,
            name,
            ..
        } => {
            if node.context.is_none() {
                if is_strict {
                    diags.error(
                        DiagnosticCode {
                            prefix: 'E',
                            number: 8013,
                        },
                        format!(
                            "public item `{}` is missing context annotations (required in {mode_label} mode)",
                            name.name
                        ),
                        node.span,
                    );
                } else {
                    diags.warning(
                        DiagnosticCode {
                            prefix: 'W',
                            number: 8013,
                        },
                        format!(
                            "public item `{}` is missing context annotations (recommended in {mode_label} mode)",
                            name.name
                        ),
                        node.span,
                    );
                }
            }
        }
        _ => {}
    }
}

/// Recurse into child nodes, threading security and capability context.
fn validate_children(
    node: &AIRNode,
    parent_security: Option<&SecurityInfo>,
    parent_capabilities: &HashSet<Capability>,
    strictness: StrictnessLevel,
    diags: &mut DiagnosticBag,
) {
    match &node.kind {
        NodeKind::Module { imports, items, .. } => {
            for child in imports.iter().chain(items.iter()) {
                validate_node(
                    child,
                    parent_security,
                    parent_capabilities,
                    strictness,
                    diags,
                );
            }
        }
        NodeKind::FnDecl {
            params,
            return_type,
            body,
            ..
        } => {
            for p in params {
                validate_node(p, parent_security, parent_capabilities, strictness, diags);
            }
            if let Some(rt) = return_type.as_ref() {
                validate_node(rt, parent_security, parent_capabilities, strictness, diags);
            }
            validate_node(
                body,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
        }
        NodeKind::ClassDecl { methods, .. } | NodeKind::TraitDecl { methods, .. } => {
            for m in methods {
                validate_node(m, parent_security, parent_capabilities, strictness, diags);
            }
        }
        NodeKind::ImplBlock {
            target, methods, ..
        } => {
            validate_node(
                target,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
            for m in methods {
                validate_node(m, parent_security, parent_capabilities, strictness, diags);
            }
        }
        NodeKind::EffectDecl { operations, .. } => {
            for op in operations {
                validate_node(op, parent_security, parent_capabilities, strictness, diags);
            }
        }
        NodeKind::EnumDecl { variants, .. } => {
            for v in variants {
                validate_node(v, parent_security, parent_capabilities, strictness, diags);
            }
        }
        NodeKind::Block { stmts, tail, .. } => {
            for stmt in stmts {
                validate_node(
                    stmt,
                    parent_security,
                    parent_capabilities,
                    strictness,
                    diags,
                );
            }
            if let Some(t) = tail.as_ref() {
                validate_node(t, parent_security, parent_capabilities, strictness, diags);
            }
        }
        NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            validate_node(
                condition,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
            validate_node(
                then_block,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
            if let Some(e) = else_block.as_ref() {
                validate_node(e, parent_security, parent_capabilities, strictness, diags);
            }
        }
        NodeKind::Match {
            scrutinee, arms, ..
        } => {
            validate_node(
                scrutinee,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
            for arm in arms {
                validate_node(arm, parent_security, parent_capabilities, strictness, diags);
            }
        }
        NodeKind::MatchArm {
            pattern,
            guard,
            body,
            ..
        } => {
            validate_node(
                pattern,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
            if let Some(g) = guard.as_ref() {
                validate_node(g, parent_security, parent_capabilities, strictness, diags);
            }
            validate_node(
                body,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
        }
        NodeKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            validate_node(
                pattern,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
            validate_node(
                iterable,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
            validate_node(
                body,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
        }
        NodeKind::While {
            condition, body, ..
        } => {
            validate_node(
                condition,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
            validate_node(
                body,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
        }
        NodeKind::Loop { body, .. } => {
            validate_node(
                body,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
        }
        NodeKind::LetBinding { value, .. } => {
            validate_node(
                value,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
        }
        NodeKind::HandlingBlock { body, handlers, .. } => {
            validate_node(
                body,
                parent_security,
                parent_capabilities,
                strictness,
                diags,
            );
            for h in handlers {
                validate_node(
                    &h.handler,
                    parent_security,
                    parent_capabilities,
                    strictness,
                    diags,
                );
            }
        }
        _ => {}
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::interpret_context;
    use crate::node::{NodeIdGen, NodeKind};
    use crate::stubs::{
        ByteSize, Capability, ContextBlock, Duration, PerformanceBudget, SecurityInfo, SizeUnit,
        TimeUnit,
    };
    use bock_ast::{Annotation, Ident, Visibility};
    use bock_errors::Span;

    fn test_span() -> Span {
        Span::dummy()
    }

    fn str_expr(s: &str) -> bock_ast::Expr {
        bock_ast::Expr::Literal {
            id: 0,
            span: test_span(),
            lit: bock_ast::Literal::String(s.to_string()),
        }
    }

    fn bool_expr(b: bool) -> bock_ast::Expr {
        bock_ast::Expr::Literal {
            id: 0,
            span: test_span(),
            lit: bock_ast::Literal::Bool(b),
        }
    }

    fn capability_expr(name: &str) -> bock_ast::Expr {
        bock_ast::Expr::FieldAccess {
            id: 0,
            span: test_span(),
            object: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "Capability".to_string(),
                    span: test_span(),
                },
            }),
            field: Ident {
                name: name.to_string(),
                span: test_span(),
            },
        }
    }

    fn method_call_expr(value: &str, method: &str) -> bock_ast::Expr {
        bock_ast::Expr::MethodCall {
            id: 0,
            span: test_span(),
            receiver: Box::new(bock_ast::Expr::Literal {
                id: 0,
                span: test_span(),
                lit: bock_ast::Literal::Int(value.to_string()),
            }),
            method: Ident {
                name: method.to_string(),
                span: test_span(),
            },
            type_args: vec![],
            args: vec![],
        }
    }

    fn ann(name: &str, args: Vec<bock_ast::Expr>) -> Annotation {
        Annotation {
            id: 0,
            span: test_span(),
            name: Ident {
                name: name.to_string(),
                span: test_span(),
            },
            args: args
                .into_iter()
                .map(|e| bock_ast::AnnotationArg {
                    label: None,
                    value: e,
                })
                .collect(),
        }
    }

    fn fn_node(
        id_gen: &NodeIdGen,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> AIRNode {
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::FnDecl {
                annotations,
                visibility,
                is_async: false,
                name: Ident {
                    name: "test_fn".to_string(),
                    span: test_span(),
                },
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    fn fn_node_named(
        id_gen: &NodeIdGen,
        name: &str,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> AIRNode {
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::FnDecl {
                annotations,
                visibility,
                is_async: false,
                name: Ident {
                    name: name.to_string(),
                    span: test_span(),
                },
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    fn module_with_items(id_gen: &NodeIdGen, items: Vec<AIRNode>) -> AIRNode {
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

    // ── Security consistency tests ──────────────────────────────────────────

    #[test]
    fn security_consistent_levels_no_error() {
        let id_gen = NodeIdGen::new();
        let child = fn_node(
            &id_gen,
            vec![ann("security", vec![str_expr("confidential")])],
            Visibility::Public,
        );
        let mut module = module_with_items(&id_gen, vec![child]);

        // Set parent security manually.
        module.context = Some(ContextBlock {
            security: Some(SecurityInfo {
                level: "internal".to_string(),
                pii: false,
            }),
            ..Default::default()
        });

        // Interpret child context.
        let _ = interpret_context(&mut module);

        let diags = validate_context(&module, StrictnessLevel::Lax);
        // Child confidential >= parent internal: OK.
        assert_eq!(diags.error_count(), 0);
    }

    #[test]
    fn security_child_less_restrictive_than_parent_error() {
        let id_gen = NodeIdGen::new();
        let child = fn_node(
            &id_gen,
            vec![ann("security", vec![str_expr("public")])],
            Visibility::Public,
        );

        let mut module = module_with_items(&id_gen, vec![child]);
        module.context = Some(ContextBlock {
            security: Some(SecurityInfo {
                level: "confidential".to_string(),
                pii: false,
            }),
            ..Default::default()
        });

        // Interpret children context.
        let _ = interpret_context(&mut module);

        let diags = validate_context(&module, StrictnessLevel::Lax);
        assert!(
            diags.error_count() > 0,
            "should error on security level contradiction"
        );
    }

    #[test]
    fn security_pii_inheritance_warning() {
        let id_gen = NodeIdGen::new();
        let mut child = fn_node(
            &id_gen,
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(false)],
            )],
            Visibility::Public,
        );
        let _ = interpret_context(&mut child);

        let mut module = module_with_items(&id_gen, vec![child]);
        module.context = Some(ContextBlock {
            security: Some(SecurityInfo {
                level: "confidential".to_string(),
                pii: true,
            }),
            ..Default::default()
        });

        let diags = validate_context(&module, StrictnessLevel::Lax);
        assert!(
            diags.warning_count() > 0,
            "should warn on PII contradiction"
        );
    }

    #[test]
    fn security_unknown_level_warning() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![ann("security", vec![str_expr("top-secret")])],
            Visibility::Public,
        );
        let _ = interpret_context(&mut node);

        let diags = validate_context(&node, StrictnessLevel::Lax);
        assert!(
            diags.warning_count() > 0,
            "should warn on unknown security level"
        );
    }

    // ── Performance budget tests ────────────────────────────────────────────

    #[test]
    fn performance_valid_budget_no_error() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![ann(
                "performance",
                vec![method_call_expr("100", "ms"), method_call_expr("50", "mb")],
            )],
            Visibility::Public,
        );
        let _ = interpret_context(&mut node);

        let diags = validate_context(&node, StrictnessLevel::Lax);
        assert_eq!(diags.error_count(), 0);
    }

    #[test]
    fn performance_negative_latency_error() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(&id_gen, vec![], Visibility::Public);
        node.context = Some(ContextBlock {
            performance: Some(PerformanceBudget {
                max_latency: Some(Duration {
                    value: -10.0,
                    unit: TimeUnit::Ms,
                }),
                max_memory: None,
            }),
            ..Default::default()
        });

        let diags = validate_context(&node, StrictnessLevel::Lax);
        assert!(diags.error_count() > 0, "should error on negative latency");
    }

    #[test]
    fn performance_zero_memory_error() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(&id_gen, vec![], Visibility::Public);
        node.context = Some(ContextBlock {
            performance: Some(PerformanceBudget {
                max_latency: None,
                max_memory: Some(ByteSize {
                    value: 0.0,
                    unit: SizeUnit::Mb,
                }),
            }),
            ..Default::default()
        });

        let diags = validate_context(&node, StrictnessLevel::Lax);
        assert!(
            diags.error_count() > 0,
            "should error on zero memory budget"
        );
    }

    // ── Completeness tests — lax (sketch) ────────────────────────────────────

    #[test]
    fn completeness_lax_no_warnings() {
        let id_gen = NodeIdGen::new();
        let node = fn_node(&id_gen, vec![], Visibility::Public);
        let module = module_with_items(&id_gen, vec![node]);

        let diags = validate_context(&module, StrictnessLevel::Lax);
        assert_eq!(
            diags.warning_count(),
            0,
            "lax: no warnings on missing context"
        );
        assert_eq!(diags.error_count(), 0, "lax: no errors on missing context");
    }

    #[test]
    fn completeness_lax_private_fn_ok() {
        let id_gen = NodeIdGen::new();
        let node = fn_node(&id_gen, vec![], Visibility::Private);
        let diags = validate_context(&node, StrictnessLevel::Lax);
        assert_eq!(diags.warning_count(), 0);
        assert_eq!(diags.error_count(), 0);
    }

    // ── Completeness tests — standard (development) ─────────────────────────

    #[test]
    fn completeness_standard_public_fn_without_context_warns() {
        let id_gen = NodeIdGen::new();
        let node = fn_node(&id_gen, vec![], Visibility::Public);

        let diags = validate_context(&node, StrictnessLevel::Standard);
        assert!(
            diags.warning_count() > 0,
            "standard mode should warn on public fn without context"
        );
        assert_eq!(
            diags.error_count(),
            0,
            "standard mode should not error on public fn without context"
        );
    }

    #[test]
    fn completeness_standard_private_fn_without_context_ok() {
        let id_gen = NodeIdGen::new();
        let node = fn_node(&id_gen, vec![], Visibility::Private);

        let diags = validate_context(&node, StrictnessLevel::Standard);
        assert_eq!(diags.warning_count(), 0);
        assert_eq!(diags.error_count(), 0);
    }

    #[test]
    fn completeness_standard_module_without_context_warns() {
        let id_gen = NodeIdGen::new();
        let module = module_with_items(&id_gen, vec![]);

        let diags = validate_context(&module, StrictnessLevel::Standard);
        assert!(
            diags.warning_count() > 0,
            "standard mode should warn on module without context"
        );
        assert_eq!(diags.error_count(), 0);
    }

    #[test]
    fn completeness_standard_module_with_context_ok() {
        let id_gen = NodeIdGen::new();
        let mut module = module_with_items(&id_gen, vec![]);
        module.context = Some(ContextBlock {
            context_text: Some("Payment module.".to_string()),
            ..Default::default()
        });

        let diags = validate_context(&module, StrictnessLevel::Standard);
        assert_eq!(diags.warning_count(), 0);
        assert_eq!(diags.error_count(), 0);
    }

    // ── Completeness tests — strict (production) ────────────────────────────

    #[test]
    fn completeness_strict_public_fn_without_context_errors() {
        let id_gen = NodeIdGen::new();
        let node = fn_node(&id_gen, vec![], Visibility::Public);

        let diags = validate_context(&node, StrictnessLevel::Strict);
        assert!(
            diags.error_count() > 0,
            "strict mode should error on public fn without context"
        );
    }

    #[test]
    fn completeness_strict_private_fn_without_context_ok() {
        let id_gen = NodeIdGen::new();
        let node = fn_node(&id_gen, vec![], Visibility::Private);

        let diags = validate_context(&node, StrictnessLevel::Strict);
        assert_eq!(
            diags.error_count(),
            0,
            "strict mode should not error on private fn without context"
        );
    }

    #[test]
    fn completeness_strict_module_without_context_errors() {
        let id_gen = NodeIdGen::new();
        let module = module_with_items(&id_gen, vec![]);

        let diags = validate_context(&module, StrictnessLevel::Strict);
        assert!(
            diags.error_count() > 0,
            "strict mode should error on module without context"
        );
    }

    #[test]
    fn completeness_strict_module_with_context_ok() {
        let id_gen = NodeIdGen::new();
        let mut module = module_with_items(&id_gen, vec![]);
        module.context = Some(ContextBlock {
            context_text: Some("Payment module.".to_string()),
            ..Default::default()
        });

        let diags = validate_context(&module, StrictnessLevel::Strict);
        assert_eq!(diags.error_count(), 0);
    }

    // ── Strictness level mapping tests ──────────────────────────────────────

    #[test]
    fn strictness_from_name_sketch() {
        assert_eq!(
            StrictnessLevel::from_name("sketch"),
            Some(StrictnessLevel::Lax)
        );
        assert_eq!(
            StrictnessLevel::from_name("lax"),
            Some(StrictnessLevel::Lax)
        );
        assert_eq!(
            StrictnessLevel::from_name("Sketch"),
            Some(StrictnessLevel::Lax)
        );
    }

    #[test]
    fn strictness_from_name_development() {
        assert_eq!(
            StrictnessLevel::from_name("development"),
            Some(StrictnessLevel::Standard)
        );
        assert_eq!(
            StrictnessLevel::from_name("standard"),
            Some(StrictnessLevel::Standard)
        );
        assert_eq!(
            StrictnessLevel::from_name("Development"),
            Some(StrictnessLevel::Standard)
        );
    }

    #[test]
    fn strictness_from_name_production() {
        assert_eq!(
            StrictnessLevel::from_name("production"),
            Some(StrictnessLevel::Strict)
        );
        assert_eq!(
            StrictnessLevel::from_name("strict"),
            Some(StrictnessLevel::Strict)
        );
        assert_eq!(
            StrictnessLevel::from_name("Production"),
            Some(StrictnessLevel::Strict)
        );
    }

    #[test]
    fn strictness_from_name_unknown() {
        assert_eq!(StrictnessLevel::from_name(""), None);
        assert_eq!(StrictnessLevel::from_name("debug"), None);
    }

    // ── Three-level differentiation test ────────────────────────────────────

    #[test]
    fn three_levels_differ_on_public_fn_without_context() {
        let id_gen = NodeIdGen::new();

        // Public fn with no context annotations.
        let node_lax = fn_node(&id_gen, vec![], Visibility::Public);
        let node_std = fn_node(&id_gen, vec![], Visibility::Public);
        let node_strict = fn_node(&id_gen, vec![], Visibility::Public);

        let d_lax = validate_context(&node_lax, StrictnessLevel::Lax);
        let d_std = validate_context(&node_std, StrictnessLevel::Standard);
        let d_strict = validate_context(&node_strict, StrictnessLevel::Strict);

        // Lax: no diagnostics at all.
        assert_eq!(
            d_lax.warning_count() + d_lax.error_count(),
            0,
            "lax: silent"
        );
        // Standard: warning but no error.
        assert!(d_std.warning_count() > 0, "standard: warns");
        assert_eq!(d_std.error_count(), 0, "standard: no errors");
        // Strict: error.
        assert!(d_strict.error_count() > 0, "strict: errors");
    }

    #[test]
    fn three_levels_differ_on_module_without_context() {
        let id_gen = NodeIdGen::new();

        let mod_lax = module_with_items(&id_gen, vec![]);
        let mod_std = module_with_items(&id_gen, vec![]);
        let mod_strict = module_with_items(&id_gen, vec![]);

        let d_lax = validate_context(&mod_lax, StrictnessLevel::Lax);
        let d_std = validate_context(&mod_std, StrictnessLevel::Standard);
        let d_strict = validate_context(&mod_strict, StrictnessLevel::Strict);

        assert_eq!(
            d_lax.warning_count() + d_lax.error_count(),
            0,
            "lax: silent"
        );
        assert!(d_std.warning_count() > 0, "standard: warns");
        assert_eq!(d_std.error_count(), 0, "standard: no errors");
        assert!(d_strict.error_count() > 0, "strict: errors");
    }

    // ── Invariant type-check tests ──────────────────────────────────────────

    #[test]
    fn invariant_comparison_expr_ok() {
        let id_gen = NodeIdGen::new();
        let invariant_expr = bock_ast::Expr::Binary {
            id: 0,
            span: test_span(),
            op: bock_ast::BinOp::Le,
            left: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "a".to_string(),
                    span: test_span(),
                },
            }),
            right: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "b".to_string(),
                    span: test_span(),
                },
            }),
        };
        let mut node = fn_node(
            &id_gen,
            vec![ann("invariant", vec![invariant_expr])],
            Visibility::Public,
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0, "comparison invariant should pass");
    }

    #[test]
    fn invariant_arithmetic_expr_error() {
        let id_gen = NodeIdGen::new();
        // `a + b` is not a boolean expression.
        let invariant_expr = bock_ast::Expr::Binary {
            id: 0,
            span: test_span(),
            op: bock_ast::BinOp::Add,
            left: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "a".to_string(),
                    span: test_span(),
                },
            }),
            right: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "b".to_string(),
                    span: test_span(),
                },
            }),
        };
        let mut node = fn_node(
            &id_gen,
            vec![ann("invariant", vec![invariant_expr])],
            Visibility::Public,
        );
        let diags = interpret_context(&mut node);
        assert!(
            diags.error_count() > 0,
            "arithmetic invariant should produce E8010 error"
        );
    }

    #[test]
    fn invariant_logical_expr_ok() {
        let id_gen = NodeIdGen::new();
        let invariant_expr = bock_ast::Expr::Binary {
            id: 0,
            span: test_span(),
            op: bock_ast::BinOp::And,
            left: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "x".to_string(),
                    span: test_span(),
                },
            }),
            right: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "y".to_string(),
                    span: test_span(),
                },
            }),
        };
        let mut node = fn_node(
            &id_gen,
            vec![ann("invariant", vec![invariant_expr])],
            Visibility::Public,
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0, "logical invariant should pass");
    }

    #[test]
    fn invariant_not_expr_ok() {
        let id_gen = NodeIdGen::new();
        let invariant_expr = bock_ast::Expr::Unary {
            id: 0,
            span: test_span(),
            op: bock_ast::UnaryOp::Not,
            operand: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "flag".to_string(),
                    span: test_span(),
                },
            }),
        };
        let mut node = fn_node(
            &id_gen,
            vec![ann("invariant", vec![invariant_expr])],
            Visibility::Public,
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0, "negation invariant should pass");
    }

    #[test]
    fn invariant_negate_numeric_error() {
        let id_gen = NodeIdGen::new();
        // `-x` is a numeric negation, not boolean.
        let invariant_expr = bock_ast::Expr::Unary {
            id: 0,
            span: test_span(),
            op: bock_ast::UnaryOp::Neg,
            operand: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "x".to_string(),
                    span: test_span(),
                },
            }),
        };
        let mut node = fn_node(
            &id_gen,
            vec![ann("invariant", vec![invariant_expr])],
            Visibility::Public,
        );
        let diags = interpret_context(&mut node);
        assert!(
            diags.error_count() > 0,
            "numeric negation invariant should produce E8010 error"
        );
    }

    #[test]
    fn invariant_call_expr_ok() {
        let id_gen = NodeIdGen::new();
        // `is_valid()` — can't verify return type without full types, accepted.
        let invariant_expr = bock_ast::Expr::Call {
            id: 0,
            span: test_span(),
            callee: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "is_valid".to_string(),
                    span: test_span(),
                },
            }),
            args: vec![],
            type_args: vec![],
        };
        let mut node = fn_node(
            &id_gen,
            vec![ann("invariant", vec![invariant_expr])],
            Visibility::Public,
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0, "call invariant should be accepted");
    }

    // ── Capability additive propagation test ────────────────────────────────

    #[test]
    fn capabilities_additive_no_error() {
        let id_gen = NodeIdGen::new();
        let mut child = fn_node(
            &id_gen,
            vec![ann("requires", vec![capability_expr("Crypto")])],
            Visibility::Public,
        );
        let _ = interpret_context(&mut child);

        let mut module = module_with_items(&id_gen, vec![child]);
        module.context = Some(ContextBlock {
            capabilities: {
                let mut s = HashSet::new();
                s.insert(Capability::new("Network"));
                s
            },
            ..Default::default()
        });

        let diags = validate_context(&module, StrictnessLevel::Standard);
        // Additive: module has Network, child has Crypto. No contradiction.
        assert_eq!(diags.error_count(), 0);
    }

    // ── Integration: combined annotations ───────────────────────────────────

    #[test]
    fn full_tree_validation() {
        let id_gen = NodeIdGen::new();

        // Child fn with proper security level >= parent.
        let mut child1 = fn_node_named(
            &id_gen,
            "process_payment",
            vec![
                ann("context", vec![str_expr("Process a payment.")]),
                ann("security", vec![str_expr("secret"), bool_expr(true)]),
                ann("requires", vec![capability_expr("Network")]),
            ],
            Visibility::Public,
        );
        let _ = interpret_context(&mut child1);

        // Private child — no context needed even in strict mode.
        let child2 = fn_node_named(&id_gen, "helper", vec![], Visibility::Private);

        let mut module = module_with_items(&id_gen, vec![child1, child2]);
        module.context = Some(ContextBlock {
            context_text: Some("Payment module.".to_string()),
            security: Some(SecurityInfo {
                level: "confidential".to_string(),
                pii: true,
            }),
            ..Default::default()
        });

        let diags = validate_context(&module, StrictnessLevel::Strict);
        // child1 has secret >= confidential: OK.
        // child2 is private: no completeness warning.
        // module has context: OK.
        assert_eq!(diags.error_count(), 0);
        assert_eq!(diags.warning_count(), 0);
    }
}
