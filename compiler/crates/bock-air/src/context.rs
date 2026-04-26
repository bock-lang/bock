//! Context annotation interpreter — transforms parsed AST annotations into
//! structured [`ContextBlock`] data attached to AIR nodes.
//!
//! This is the C-AIR pass. It walks the AIR tree and, for every node that
//! carries annotations (`@context`, `@requires`, `@performance`, `@invariant`,
//! `@security`, `@domain`), extracts structured information into a
//! [`ContextBlock`] on the node.

use bock_ast::{Annotation, AnnotationArg, Expr, Literal};
use bock_errors::{DiagnosticBag, DiagnosticCode};

use crate::node::{AIRNode, NodeKind};
use crate::stubs::{
    BehavioralModifier, ByteSize, Capability, ContextBlock, ContextMarker, Duration,
    PerformanceBudget, SecurityInfo, SizeUnit, TimeUnit, KNOWN_CAPABILITIES,
};

/// Interprets context annotations on all nodes in an AIR tree.
///
/// Walks the tree rooted at `root`, processing annotations on each declaration
/// node. Populates the `context` slot on nodes that have context annotations.
/// Returns a [`DiagnosticBag`] containing any warnings or errors encountered
/// (e.g. unknown capability names).
#[must_use]
pub fn interpret_context(root: &mut AIRNode) -> DiagnosticBag {
    let mut diags = DiagnosticBag::new();
    interpret_node(root, &mut diags);
    diags
}

/// Recursively interpret context annotations on a single node and its children.
fn interpret_node(node: &mut AIRNode, diags: &mut DiagnosticBag) {
    // First, process this node's annotations if it has any.
    let annotations = extract_annotations(&node.kind);
    if !annotations.is_empty() {
        let block = build_context_block(&annotations, diags);
        if !block.is_empty() {
            node.context = Some(block);
        }
    }

    // Then recurse into children.
    interpret_children(node, diags);
}

/// Extract the annotations slice from a node kind, if it carries annotations.
fn extract_annotations(kind: &NodeKind) -> Vec<Annotation> {
    match kind {
        NodeKind::FnDecl { annotations, .. }
        | NodeKind::RecordDecl { annotations, .. }
        | NodeKind::EnumDecl { annotations, .. }
        | NodeKind::ClassDecl { annotations, .. }
        | NodeKind::TraitDecl { annotations, .. }
        | NodeKind::ImplBlock { annotations, .. }
        | NodeKind::EffectDecl { annotations, .. }
        | NodeKind::TypeAlias { annotations, .. }
        | NodeKind::ConstDecl { annotations, .. } => annotations.clone(),
        _ => Vec::new(),
    }
}

/// Recurse into child nodes of the given node.
fn interpret_children(node: &mut AIRNode, diags: &mut DiagnosticBag) {
    match &mut node.kind {
        NodeKind::Module { imports, items, .. } => {
            for child in imports.iter_mut().chain(items.iter_mut()) {
                interpret_node(child, diags);
            }
        }
        NodeKind::FnDecl {
            params,
            return_type,
            body,
            ..
        } => {
            for p in params.iter_mut() {
                interpret_node(p, diags);
            }
            if let Some(rt) = return_type.as_mut() {
                interpret_node(rt, diags);
            }
            interpret_node(body, diags);
        }
        NodeKind::ClassDecl { methods, .. } => {
            for m in methods.iter_mut() {
                interpret_node(m, diags);
            }
        }
        NodeKind::TraitDecl { methods, .. } => {
            for m in methods.iter_mut() {
                interpret_node(m, diags);
            }
        }
        NodeKind::ImplBlock {
            methods, target, ..
        } => {
            interpret_node(target, diags);
            for m in methods.iter_mut() {
                interpret_node(m, diags);
            }
        }
        NodeKind::EffectDecl { operations, .. } => {
            for op in operations.iter_mut() {
                interpret_node(op, diags);
            }
        }
        NodeKind::EnumDecl { variants, .. } => {
            for v in variants.iter_mut() {
                interpret_node(v, diags);
            }
        }
        NodeKind::Block { stmts, tail, .. } => {
            for stmt in stmts.iter_mut() {
                interpret_node(stmt, diags);
            }
            if let Some(t) = tail.as_mut() {
                interpret_node(t, diags);
            }
        }
        NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            interpret_node(condition, diags);
            interpret_node(then_block, diags);
            if let Some(e) = else_block.as_mut() {
                interpret_node(e, diags);
            }
        }
        NodeKind::Match {
            scrutinee, arms, ..
        } => {
            interpret_node(scrutinee, diags);
            for arm in arms.iter_mut() {
                interpret_node(arm, diags);
            }
        }
        NodeKind::MatchArm {
            pattern,
            guard,
            body,
            ..
        } => {
            interpret_node(pattern, diags);
            if let Some(g) = guard.as_mut() {
                interpret_node(g, diags);
            }
            interpret_node(body, diags);
        }
        NodeKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            interpret_node(pattern, diags);
            interpret_node(iterable, diags);
            interpret_node(body, diags);
        }
        NodeKind::While {
            condition, body, ..
        } => {
            interpret_node(condition, diags);
            interpret_node(body, diags);
        }
        NodeKind::Loop { body, .. } => {
            interpret_node(body, diags);
        }
        NodeKind::LetBinding { value, .. } => {
            interpret_node(value, diags);
        }
        NodeKind::HandlingBlock { body, handlers, .. } => {
            interpret_node(body, diags);
            for h in handlers.iter_mut() {
                interpret_node(&mut h.handler, diags);
            }
        }
        // Leaf nodes and other variants don't need recursion for context.
        _ => {}
    }
}

/// Build a [`ContextBlock`] from a list of annotations on a single node.
fn build_context_block(annotations: &[Annotation], diags: &mut DiagnosticBag) -> ContextBlock {
    let mut block = ContextBlock::default();

    for ann in annotations {
        let name = ann.name.name.as_str();
        match name {
            "context" => interpret_context_annotation(ann, &mut block),
            "requires" => interpret_requires_annotation(ann, &mut block, diags),
            "performance" => interpret_performance_annotation(ann, &mut block, diags),
            "invariant" => interpret_invariant_annotation(ann, &mut block, diags),
            "security" => interpret_security_annotation(ann, &mut block, diags),
            "domain" => interpret_domain_annotation(ann, &mut block, diags),
            "concurrent" => block.modifiers.push(BehavioralModifier::Concurrent),
            "managed" => block.modifiers.push(BehavioralModifier::Managed),
            "deterministic" => block.modifiers.push(BehavioralModifier::Deterministic),
            "inline" => block.modifiers.push(BehavioralModifier::Inline),
            "cold" => block.modifiers.push(BehavioralModifier::Cold),
            "hot" => block.modifiers.push(BehavioralModifier::Hot),
            "deprecated" => {
                let reason = first_string_arg(&ann.args);
                block.modifiers.push(BehavioralModifier::Deprecated(reason));
            }
            // Other annotations (@derive, @test, etc.) are not context annotations.
            _ => {}
        }
    }

    block
}

// ─── @context ────────────────────────────────────────────────────────────────

/// Interpret a `@context("...")` annotation.
///
/// Extracts the free-form text and parses structured markers like `@intent:`,
/// `@assumption:`, `@constraint:`, `@security:`.
fn interpret_context_annotation(ann: &Annotation, block: &mut ContextBlock) {
    if let Some(text) = first_string_arg(&ann.args) {
        // Extract structured markers from the text.
        block.markers.extend(extract_markers(&text));
        block.context_text = Some(text);
    }
}

/// Extract `@tag: text` markers from a free-form context string.
fn extract_markers(text: &str) -> Vec<ContextMarker> {
    let mut markers = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('@') {
            if let Some((tag, value)) = rest.split_once(':') {
                let tag = tag.trim().to_string();
                let value = value.trim().to_string();
                if !tag.is_empty() {
                    markers.push(ContextMarker { tag, text: value });
                }
            }
        }
    }
    markers
}

// ─── @requires ───────────────────────────────────────────────────────────────

/// Interpret a `@requires(Capability.Network, Capability.Storage)` annotation.
///
/// Each argument should be a field-access expression `Capability.Name` or a
/// simple identifier `Name`.
fn interpret_requires_annotation(
    ann: &Annotation,
    block: &mut ContextBlock,
    diags: &mut DiagnosticBag,
) {
    for arg in &ann.args {
        match resolve_capability_name(&arg.value) {
            Some(name) => {
                if is_known_capability(&name) {
                    block.capabilities.insert(Capability::new(&name));
                } else {
                    diags.error(
                        DiagnosticCode {
                            prefix: 'E',
                            number: 8001,
                        },
                        format!("unknown capability `{name}`"),
                        arg.value.span(),
                    );
                }
            }
            None => {
                diags.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 8002,
                    },
                    "expected capability name (e.g. `Capability.Network` or `Network`)".to_string(),
                    arg.value.span(),
                );
            }
        }
    }
}

/// Extract a capability name from an expression.
///
/// Handles:
/// - `Capability.Network` → `"Network"`
/// - `Network` → `"Network"`
fn resolve_capability_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::FieldAccess { field, .. } => Some(field.name.clone()),
        Expr::Identifier { name, .. } => Some(name.name.clone()),
        _ => None,
    }
}

/// Check if a capability name is in the known taxonomy.
fn is_known_capability(name: &str) -> bool {
    KNOWN_CAPABILITIES.contains(&name)
}

// ─── @performance ────────────────────────────────────────────────────────────

/// Interpret a `@performance(max_latency: 100.ms, max_memory: 50.mb)` annotation.
///
/// Since the parser strips named argument labels, we identify parameters by
/// the suffix method call: `.ms`/`.s`/`.us`/`.ns` → duration, `.mb`/`.gb`/`.kb`/`.b` → byte size.
fn interpret_performance_annotation(
    ann: &Annotation,
    block: &mut ContextBlock,
    diags: &mut DiagnosticBag,
) {
    let mut budget = PerformanceBudget {
        max_latency: None,
        max_memory: None,
    };

    for arg in &ann.args {
        if let Some(d) = parse_duration(&arg.value) {
            budget.max_latency = Some(d);
            continue;
        }
        if let Some(b) = parse_byte_size(&arg.value) {
            budget.max_memory = Some(b);
            continue;
        }
        diags.error(
            DiagnosticCode {
                prefix: 'E',
                number: 8003,
            },
            "expected duration (e.g. `100.ms`) or byte size (e.g. `50.mb`)".to_string(),
            arg.value.span(),
        );
    }

    block.performance = Some(budget);
}

/// Try to parse a method-call expression as a duration value (e.g. `100.ms`).
fn parse_duration(expr: &Expr) -> Option<Duration> {
    if let Expr::MethodCall {
        receiver, method, ..
    } = expr
    {
        let unit = match method.name.as_str() {
            "ns" => TimeUnit::Ns,
            "us" => TimeUnit::Us,
            "ms" => TimeUnit::Ms,
            "s" => TimeUnit::S,
            _ => return None,
        };
        let value = extract_numeric_value(receiver)?;
        return Some(Duration { value, unit });
    }
    None
}

/// Try to parse a method-call expression as a byte size value (e.g. `50.mb`).
fn parse_byte_size(expr: &Expr) -> Option<ByteSize> {
    if let Expr::MethodCall {
        receiver, method, ..
    } = expr
    {
        let unit = match method.name.as_str() {
            "b" => SizeUnit::B,
            "kb" => SizeUnit::Kb,
            "mb" => SizeUnit::Mb,
            "gb" => SizeUnit::Gb,
            _ => return None,
        };
        let value = extract_numeric_value(receiver)?;
        return Some(ByteSize { value, unit });
    }
    None
}

/// Extract a numeric value (int or float) from a literal expression.
fn extract_numeric_value(expr: &Expr) -> Option<f64> {
    if let Expr::Literal { lit, .. } = expr {
        match lit {
            Literal::Int(s) => s.parse::<f64>().ok(),
            Literal::Float(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    } else {
        None
    }
}

// ─── @invariant ──────────────────────────────────────────────────────────────

/// Interpret an `@invariant(expr)` annotation.
///
/// Preserves the expression as a string representation. Also validates that the
/// expression is structurally boolean-typed (comparison, logical, or negation).
fn interpret_invariant_annotation(
    ann: &Annotation,
    block: &mut ContextBlock,
    diags: &mut DiagnosticBag,
) {
    if ann.args.is_empty() {
        diags.error(
            DiagnosticCode {
                prefix: 'E',
                number: 8004,
            },
            "@invariant requires an expression argument".to_string(),
            ann.span,
        );
        return;
    }
    for arg in &ann.args {
        if !is_boolean_expr(&arg.value) {
            diags.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 8010,
                },
                "@invariant expression must be boolean-typed (comparison, logical, or call)"
                    .to_string(),
                arg.value.span(),
            );
        }
        block.invariants.push(expr_to_string(&arg.value));
    }
}

/// Check if an expression is structurally boolean-typed.
///
/// Returns `true` for:
/// - Comparison operators (`<`, `<=`, `>`, `>=`, `==`, `!=`)
/// - Logical operators (`&&`, `||`)
/// - Unary not (`!`)
/// - Boolean literals
/// - Function/method calls (assumed to return bool — can't verify without types)
/// - Identifiers (could be bool variables — can't verify without types)
fn is_boolean_expr(expr: &Expr) -> bool {
    use bock_ast::BinOp;
    match expr {
        Expr::Binary { op, .. } => matches!(
            op,
            BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or
        ),
        Expr::Unary { op, .. } => matches!(op, bock_ast::UnaryOp::Not),
        Expr::Literal {
            lit: Literal::Bool(_),
            ..
        } => true,
        // Calls, method calls, and identifiers could return bool; accept them.
        Expr::Call { .. } | Expr::MethodCall { .. } | Expr::Identifier { .. } => true,
        _ => false,
    }
}

// ─── @security ───────────────────────────────────────────────────────────────

/// Interpret a `@security(level: "confidential", pii: true)` annotation.
///
/// Since the parser strips named labels, we use positional/typed heuristics:
/// the first string argument is the level, the first boolean is the pii flag.
fn interpret_security_annotation(
    ann: &Annotation,
    block: &mut ContextBlock,
    diags: &mut DiagnosticBag,
) {
    let mut level: Option<String> = None;
    let mut pii: Option<bool> = None;

    for arg in &ann.args {
        match &arg.value {
            Expr::Literal {
                lit: Literal::String(s),
                ..
            } => {
                if level.is_none() {
                    level = Some(s.clone());
                }
            }
            Expr::Literal {
                lit: Literal::Bool(b),
                ..
            } => {
                if pii.is_none() {
                    pii = Some(*b);
                }
            }
            _ => {
                diags.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 8005,
                    },
                    "expected string level or boolean pii flag in @security".to_string(),
                    arg.value.span(),
                );
            }
        }
    }

    if level.is_none() && pii.is_none() && ann.args.is_empty() {
        diags.error(
            DiagnosticCode {
                prefix: 'E',
                number: 8005,
            },
            "@security requires at least a level or pii argument".to_string(),
            ann.span,
        );
        return;
    }

    block.security = Some(SecurityInfo {
        level: level.unwrap_or_default(),
        pii: pii.unwrap_or(false),
    });
}

// ─── @domain ─────────────────────────────────────────────────────────────────

/// Interpret a `@domain("e-commerce", "checkout")` annotation.
fn interpret_domain_annotation(
    ann: &Annotation,
    block: &mut ContextBlock,
    diags: &mut DiagnosticBag,
) {
    for arg in &ann.args {
        match &arg.value {
            Expr::Literal {
                lit: Literal::String(s),
                ..
            } => {
                block.domains.push(s.clone());
            }
            _ => {
                diags.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 8006,
                    },
                    "expected string argument in @domain".to_string(),
                    arg.value.span(),
                );
            }
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Extract the first string argument from an annotation's args.
fn first_string_arg(args: &[AnnotationArg]) -> Option<String> {
    for arg in args {
        if let Expr::Literal {
            lit: Literal::String(s),
            ..
        } = &arg.value
        {
            return Some(s.clone());
        }
    }
    None
}

/// Convert an expression to a string representation for invariant storage.
fn expr_to_string(expr: &Expr) -> String {
    match expr {
        Expr::Literal { lit, .. } => match lit {
            Literal::Int(s) | Literal::Float(s) | Literal::Char(s) | Literal::String(s) => {
                s.clone()
            }
            Literal::Bool(b) => b.to_string(),
            Literal::Unit => "()".to_string(),
        },
        Expr::Identifier { name, .. } => name.name.clone(),
        Expr::Binary {
            op, left, right, ..
        } => {
            format!("{} {op:?} {}", expr_to_string(left), expr_to_string(right))
        }
        Expr::FieldAccess { object, field, .. } => {
            format!("{}.{}", expr_to_string(object), field.name)
        }
        Expr::MethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            let args_str: Vec<String> = args.iter().map(|a| expr_to_string(&a.value)).collect();
            format!(
                "{}.{}({})",
                expr_to_string(receiver),
                method.name,
                args_str.join(", ")
            )
        }
        Expr::Call { callee, args, .. } => {
            let args_str: Vec<String> = args.iter().map(|a| expr_to_string(&a.value)).collect();
            format!("{}({})", expr_to_string(callee), args_str.join(", "))
        }
        Expr::Unary { op, operand, .. } => {
            format!("{op:?}{}", expr_to_string(operand))
        }
        _ => "<expr>".to_string(),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeIdGen, NodeKind};
    use bock_ast::{Ident, Visibility};
    use bock_errors::Span;

    /// Helper to create a span for testing.
    fn test_span() -> Span {
        Span::dummy()
    }

    /// Helper to create a string literal expression.
    fn str_expr(s: &str) -> Expr {
        Expr::Literal {
            id: 0,
            span: test_span(),
            lit: Literal::String(s.to_string()),
        }
    }

    /// Helper to create a bool literal expression.
    fn bool_expr(b: bool) -> Expr {
        Expr::Literal {
            id: 0,
            span: test_span(),
            lit: Literal::Bool(b),
        }
    }

    /// Helper to create an int literal expression.
    fn int_expr(n: &str) -> Expr {
        Expr::Literal {
            id: 0,
            span: test_span(),
            lit: Literal::Int(n.to_string()),
        }
    }

    /// Helper to create a `Capability.Name` field-access expression.
    fn capability_expr(name: &str) -> Expr {
        Expr::FieldAccess {
            id: 0,
            span: test_span(),
            object: Box::new(Expr::Identifier {
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

    /// Helper to create a method-call expression like `100.ms`.
    fn method_call_expr(value: &str, method: &str) -> Expr {
        Expr::MethodCall {
            id: 0,
            span: test_span(),
            receiver: Box::new(int_expr(value)),
            method: Ident {
                name: method.to_string(),
                span: test_span(),
            },
            type_args: vec![],
            args: vec![],
        }
    }

    /// Helper to create an annotation.
    fn ann(name: &str, args: Vec<Expr>) -> Annotation {
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

    /// Helper to create a minimal FnDecl AIR node with annotations.
    fn fn_node(id_gen: &NodeIdGen, annotations: Vec<Annotation>) -> AIRNode {
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
                visibility: Visibility::Public,
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

    #[test]
    fn context_free_form_text() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![ann("context", vec![str_expr("Payment processing module.")])],
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert_eq!(
            ctx.context_text.as_deref(),
            Some("Payment processing module.")
        );
    }

    #[test]
    fn context_with_markers() {
        let id_gen = NodeIdGen::new();
        let text = "\n  Payment processing module.\n  @intent: Process and validate payments.\n  @constraint: Must complete within 500ms p99.\n";
        let mut node = fn_node(&id_gen, vec![ann("context", vec![str_expr(text)])]);
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert_eq!(ctx.markers.len(), 2);
        assert_eq!(ctx.markers[0].tag, "intent");
        assert_eq!(ctx.markers[0].text, "Process and validate payments.");
        assert_eq!(ctx.markers[1].tag, "constraint");
        assert_eq!(ctx.markers[1].text, "Must complete within 500ms p99.");
    }

    #[test]
    fn requires_known_capabilities() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![ann(
                "requires",
                vec![capability_expr("Network"), capability_expr("Storage")],
            )],
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert!(ctx.capabilities.contains(&Capability::new("Network")));
        assert!(ctx.capabilities.contains(&Capability::new("Storage")));
    }

    #[test]
    fn requires_unknown_capability_produces_diagnostic() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![ann("requires", vec![capability_expr("Teleporter")])],
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 1);
    }

    #[test]
    fn performance_parsed() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![ann(
                "performance",
                vec![method_call_expr("100", "ms"), method_call_expr("50", "mb")],
            )],
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        let perf = ctx.performance.as_ref().unwrap();
        let lat = perf.max_latency.unwrap();
        assert!((lat.value - 100.0).abs() < f64::EPSILON);
        assert_eq!(lat.unit, TimeUnit::Ms);
        let mem = perf.max_memory.unwrap();
        assert!((mem.value - 50.0).abs() < f64::EPSILON);
        assert_eq!(mem.unit, SizeUnit::Mb);
    }

    #[test]
    fn invariant_preserved() {
        let id_gen = NodeIdGen::new();
        let invariant_expr = Expr::Binary {
            id: 0,
            span: test_span(),
            op: bock_ast::BinOp::Le,
            left: Box::new(Expr::MethodCall {
                id: 0,
                span: test_span(),
                receiver: Box::new(Expr::Identifier {
                    id: 0,
                    span: test_span(),
                    name: Ident {
                        name: "result".to_string(),
                        span: test_span(),
                    },
                }),
                method: Ident {
                    name: "len".to_string(),
                    span: test_span(),
                },
                type_args: vec![],
                args: vec![],
            }),
            right: Box::new(Expr::MethodCall {
                id: 0,
                span: test_span(),
                receiver: Box::new(Expr::Identifier {
                    id: 0,
                    span: test_span(),
                    name: Ident {
                        name: "input".to_string(),
                        span: test_span(),
                    },
                }),
                method: Ident {
                    name: "len".to_string(),
                    span: test_span(),
                },
                type_args: vec![],
                args: vec![],
            }),
        };
        let mut node = fn_node(&id_gen, vec![ann("invariant", vec![invariant_expr])]);
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert_eq!(ctx.invariants.len(), 1);
        assert!(ctx.invariants[0].contains("result.len()"));
        assert!(ctx.invariants[0].contains("input.len()"));
    }

    #[test]
    fn invariant_empty_args_error() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(&id_gen, vec![ann("invariant", vec![])]);
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 1);
    }

    #[test]
    fn security_level_and_pii() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        let sec = ctx.security.as_ref().unwrap();
        assert_eq!(sec.level, "confidential");
        assert!(sec.pii);
    }

    #[test]
    fn security_level_only() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(&id_gen, vec![ann("security", vec![str_expr("public")])]);
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        let sec = ctx.security.as_ref().unwrap();
        assert_eq!(sec.level, "public");
        assert!(!sec.pii);
    }

    #[test]
    fn domain_tags() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![ann(
                "domain",
                vec![str_expr("e-commerce"), str_expr("checkout")],
            )],
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert_eq!(ctx.domains, vec!["e-commerce", "checkout"]);
    }

    #[test]
    fn multiple_annotations_combined() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![
                ann("context", vec![str_expr("Process payments.")]),
                ann(
                    "requires",
                    vec![capability_expr("Network"), capability_expr("Crypto")],
                ),
                ann("domain", vec![str_expr("payments")]),
            ],
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert_eq!(ctx.context_text.as_deref(), Some("Process payments."));
        assert!(ctx.capabilities.contains(&Capability::new("Network")));
        assert!(ctx.capabilities.contains(&Capability::new("Crypto")));
        assert_eq!(ctx.domains, vec!["payments"]);
    }

    #[test]
    fn non_context_annotations_ignored() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(&id_gen, vec![ann("derive", vec![str_expr("Debug")])]);
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        assert!(node.context.is_none());
    }

    #[test]
    fn module_with_annotated_children() {
        let id_gen = NodeIdGen::new();
        let child = fn_node(&id_gen, vec![ann("domain", vec![str_expr("billing")])]);
        let mut module = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![child],
            },
        );
        let diags = interpret_context(&mut module);
        assert_eq!(diags.error_count(), 0);
        // The child should have context, not the module.
        assert!(module.context.is_none());
        if let NodeKind::Module { items, .. } = &module.kind {
            let ctx = items[0].context.as_ref().unwrap();
            assert_eq!(ctx.domains, vec!["billing"]);
        } else {
            panic!("expected module");
        }
    }

    #[test]
    fn requires_simple_identifier() {
        let id_gen = NodeIdGen::new();
        let ident = Expr::Identifier {
            id: 0,
            span: test_span(),
            name: Ident {
                name: "Network".to_string(),
                span: test_span(),
            },
        };
        let mut node = fn_node(&id_gen, vec![ann("requires", vec![ident])]);
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert!(ctx.capabilities.contains(&Capability::new("Network")));
    }

    // ── Behavioral modifier tests ─────────────────────────────────────────────

    #[test]
    fn concurrent_modifier_stored() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(&id_gen, vec![ann("concurrent", vec![])]);
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert_eq!(ctx.modifiers.len(), 1);
        assert_eq!(ctx.modifiers[0], BehavioralModifier::Concurrent);
    }

    #[test]
    fn deprecated_with_reason_stored() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![ann("deprecated", vec![str_expr("use new_fn")])],
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert_eq!(ctx.modifiers.len(), 1);
        assert_eq!(
            ctx.modifiers[0],
            BehavioralModifier::Deprecated(Some("use new_fn".to_string()))
        );
    }

    #[test]
    fn deprecated_without_reason_stored() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(&id_gen, vec![ann("deprecated", vec![])]);
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert_eq!(ctx.modifiers.len(), 1);
        assert_eq!(ctx.modifiers[0], BehavioralModifier::Deprecated(None));
    }

    #[test]
    fn managed_modifier_stored() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(&id_gen, vec![ann("managed", vec![])]);
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert_eq!(ctx.modifiers.len(), 1);
        assert_eq!(ctx.modifiers[0], BehavioralModifier::Managed);
    }

    #[test]
    fn multiple_modifiers_combined() {
        let id_gen = NodeIdGen::new();
        let mut node = fn_node(
            &id_gen,
            vec![
                ann("inline", vec![]),
                ann("hot", vec![]),
                ann("deterministic", vec![]),
            ],
        );
        let diags = interpret_context(&mut node);
        assert_eq!(diags.error_count(), 0);
        let ctx = node.context.as_ref().unwrap();
        assert_eq!(ctx.modifiers.len(), 3);
        assert_eq!(ctx.modifiers[0], BehavioralModifier::Inline);
        assert_eq!(ctx.modifiers[1], BehavioralModifier::Hot);
        assert_eq!(ctx.modifiers[2], BehavioralModifier::Deterministic);
    }

    // ── Capability taxonomy tests ────────────────────────────────────────────

    #[test]
    fn all_16_spec_capabilities_recognized() {
        let spec_caps = [
            "Network",
            "Storage",
            "Crypto",
            "GPU",
            "Camera",
            "Microphone",
            "Location",
            "Notifications",
            "Bluetooth",
            "Biometrics",
            "Clipboard",
            "SystemProcess",
            "FFI",
            "Environment",
            "Clock",
            "Random",
        ];
        assert_eq!(KNOWN_CAPABILITIES.len(), 16);
        for cap in &spec_caps {
            assert!(
                is_known_capability(cap),
                "capability `{cap}` should be recognized"
            );
        }
    }

    #[test]
    fn non_spec_capabilities_rejected() {
        // Old names that were renamed or removed.
        for cap in &["Process", "Timer", "System", "UI", "Audio", "Console"] {
            assert!(
                !is_known_capability(cap),
                "capability `{cap}` should NOT be recognized"
            );
        }
    }
}
