//! Visitor trait for AIR traversal.
//!
//! Implement [`Visitor`] and override only the methods you care about.
//! Each `visit_*` method has a default implementation that walks the node's
//! children by calling the corresponding `walk_*` helper, enabling selective
//! interception without losing traversal for the rest of the tree.

use crate::node::{AIRNode, AirInterpolationPart, NodeKind};

/// A read-only visitor over the AIR tree.
///
/// Override any `visit_*` method to intercept a specific node. Call the
/// corresponding `walk_*` function inside your override to recurse into
/// children, or omit it to prune the traversal at that point.
#[allow(unused_variables)]
pub trait Visitor: Sized {
    fn visit_node(&mut self, node: &AIRNode) {
        walk_node(self, node);
    }
}

/// Dispatches to the appropriate walk helper based on the node's kind.
pub fn walk_node<V: Visitor>(v: &mut V, node: &AIRNode) {
    match &node.kind {
        NodeKind::Module { imports, items, .. } => {
            for n in imports {
                v.visit_node(n);
            }
            for n in items {
                v.visit_node(n);
            }
        }
        NodeKind::ImportDecl { .. } => {}

        // ── Declarations ──────────────────────────────────────────────────
        NodeKind::FnDecl {
            params,
            return_type,
            body,
            ..
        } => {
            for p in params {
                v.visit_node(p);
            }
            if let Some(rt) = return_type {
                v.visit_node(rt);
            }
            v.visit_node(body);
        }
        NodeKind::RecordDecl { .. } => {}
        NodeKind::EnumDecl { variants, .. } => {
            for var in variants {
                v.visit_node(var);
            }
        }
        NodeKind::EnumVariant { payload, .. } => {
            if let crate::node::EnumVariantPayload::Tuple(tys) = payload {
                for ty in tys {
                    v.visit_node(ty);
                }
            }
        }
        NodeKind::ClassDecl { methods, .. } => {
            for m in methods {
                v.visit_node(m);
            }
        }
        NodeKind::TraitDecl { methods, .. } => {
            for m in methods {
                v.visit_node(m);
            }
        }
        NodeKind::ImplBlock {
            target, methods, ..
        } => {
            v.visit_node(target);
            for m in methods {
                v.visit_node(m);
            }
        }
        NodeKind::EffectDecl { operations, .. } => {
            for op in operations {
                v.visit_node(op);
            }
        }
        NodeKind::TypeAlias { ty, .. } => {
            v.visit_node(ty);
        }
        NodeKind::ConstDecl { ty, value, .. } => {
            v.visit_node(ty);
            v.visit_node(value);
        }
        NodeKind::ModuleHandle { handler, .. } => {
            v.visit_node(handler);
        }
        NodeKind::PropertyTest { body, .. } => {
            v.visit_node(body);
        }

        // ── Param ─────────────────────────────────────────────────────────
        NodeKind::Param {
            pattern,
            ty,
            default,
        } => {
            v.visit_node(pattern);
            if let Some(t) = ty {
                v.visit_node(t);
            }
            if let Some(d) = default {
                v.visit_node(d);
            }
        }

        // ── Type expressions ──────────────────────────────────────────────
        NodeKind::TypeNamed { args, .. } => {
            for a in args {
                v.visit_node(a);
            }
        }
        NodeKind::TypeTuple { elems } | NodeKind::TypeFunction { params: elems, .. } => {
            for e in elems {
                v.visit_node(e);
            }
            if let NodeKind::TypeFunction { ret, .. } = &node.kind {
                v.visit_node(ret);
            }
        }
        NodeKind::TypeOptional { inner } => v.visit_node(inner),
        NodeKind::TypeSelf => {}

        // ── Expressions ───────────────────────────────────────────────────
        NodeKind::Literal { .. }
        | NodeKind::Identifier { .. }
        | NodeKind::Placeholder
        | NodeKind::Unreachable => {}

        NodeKind::BinaryOp { left, right, .. }
        | NodeKind::Pipe { left, right }
        | NodeKind::Compose { left, right } => {
            v.visit_node(left);
            v.visit_node(right);
        }
        NodeKind::UnaryOp { operand, .. }
        | NodeKind::Propagate { expr: operand }
        | NodeKind::Await { expr: operand }
        | NodeKind::Move { expr: operand }
        | NodeKind::Borrow { expr: operand }
        | NodeKind::MutableBorrow { expr: operand } => {
            v.visit_node(operand);
        }
        NodeKind::Assign { target, value, .. } => {
            v.visit_node(target);
            v.visit_node(value);
        }
        NodeKind::Call {
            callee,
            args,
            type_args,
        } => {
            v.visit_node(callee);
            for a in args {
                v.visit_node(&a.value);
            }
            for t in type_args {
                v.visit_node(t);
            }
        }
        NodeKind::MethodCall {
            receiver,
            args,
            type_args,
            ..
        } => {
            v.visit_node(receiver);
            for a in args {
                v.visit_node(&a.value);
            }
            for t in type_args {
                v.visit_node(t);
            }
        }
        NodeKind::FieldAccess { object, .. } => v.visit_node(object),
        NodeKind::Index { object, index } => {
            v.visit_node(object);
            v.visit_node(index);
        }
        NodeKind::Lambda { params, body } => {
            for p in params {
                v.visit_node(p);
            }
            v.visit_node(body);
        }
        NodeKind::Range { lo, hi, .. } => {
            v.visit_node(lo);
            v.visit_node(hi);
        }
        NodeKind::RecordConstruct { fields, spread, .. } => {
            for f in fields {
                if let Some(val) = &f.value {
                    v.visit_node(val);
                }
            }
            if let Some(s) = spread {
                v.visit_node(s);
            }
        }
        NodeKind::ListLiteral { elems }
        | NodeKind::SetLiteral { elems }
        | NodeKind::TupleLiteral { elems } => {
            for e in elems {
                v.visit_node(e);
            }
        }
        NodeKind::MapLiteral { entries } => {
            for entry in entries {
                v.visit_node(&entry.key);
                v.visit_node(&entry.value);
            }
        }
        NodeKind::Interpolation { parts } => {
            for part in parts {
                if let AirInterpolationPart::Expr(e) = part {
                    v.visit_node(e);
                }
            }
        }
        NodeKind::ResultConstruct { value, .. } => {
            if let Some(val) = value {
                v.visit_node(val);
            }
        }

        // ── Control flow ──────────────────────────────────────────────────
        NodeKind::If {
            let_pattern,
            condition,
            then_block,
            else_block,
        } => {
            if let Some(pat) = let_pattern {
                v.visit_node(pat);
            }
            v.visit_node(condition);
            v.visit_node(then_block);
            if let Some(eb) = else_block {
                v.visit_node(eb);
            }
        }
        NodeKind::Guard {
            let_pattern,
            condition,
            else_block,
        } => {
            if let Some(pat) = let_pattern {
                v.visit_node(pat);
            }
            v.visit_node(condition);
            v.visit_node(else_block);
        }
        NodeKind::Match { scrutinee, arms } => {
            v.visit_node(scrutinee);
            for arm in arms {
                v.visit_node(arm);
            }
        }
        NodeKind::MatchArm {
            pattern,
            guard,
            body,
        } => {
            v.visit_node(pattern);
            if let Some(g) = guard {
                v.visit_node(g);
            }
            v.visit_node(body);
        }
        NodeKind::For {
            pattern,
            iterable,
            body,
        } => {
            v.visit_node(pattern);
            v.visit_node(iterable);
            v.visit_node(body);
        }
        NodeKind::While { condition, body } => {
            v.visit_node(condition);
            v.visit_node(body);
        }
        NodeKind::Loop { body } => v.visit_node(body),
        NodeKind::Block { stmts, tail } => {
            for s in stmts {
                v.visit_node(s);
            }
            if let Some(t) = tail {
                v.visit_node(t);
            }
        }
        NodeKind::Return { value } | NodeKind::Break { value } => {
            if let Some(val) = value {
                v.visit_node(val);
            }
        }
        NodeKind::Continue => {}

        // ── Ownership ─────────────────────────────────────────────────────
        NodeKind::LetBinding {
            pattern, ty, value, ..
        } => {
            v.visit_node(pattern);
            if let Some(t) = ty {
                v.visit_node(t);
            }
            v.visit_node(value);
        }

        // ── Effects ───────────────────────────────────────────────────────
        NodeKind::EffectOp { args, .. } => {
            for a in args {
                v.visit_node(&a.value);
            }
        }
        NodeKind::HandlingBlock { handlers, body } => {
            for h in handlers {
                v.visit_node(&h.handler);
            }
            v.visit_node(body);
        }
        NodeKind::EffectRef { .. } => {}

        // ── Patterns ──────────────────────────────────────────────────────
        NodeKind::WildcardPat
        | NodeKind::BindPat { .. }
        | NodeKind::LiteralPat { .. }
        | NodeKind::RestPat => {}

        NodeKind::ConstructorPat { fields, .. } => {
            for f in fields {
                v.visit_node(f);
            }
        }
        NodeKind::RecordPat { fields, .. } => {
            for f in fields {
                if let Some(pat) = &f.pattern {
                    v.visit_node(pat);
                }
            }
        }
        NodeKind::TuplePat { elems } => {
            for e in elems {
                v.visit_node(e);
            }
        }
        NodeKind::ListPat { elems, rest } => {
            for e in elems {
                v.visit_node(e);
            }
            if let Some(r) = rest {
                v.visit_node(r);
            }
        }
        NodeKind::OrPat { alternatives } => {
            for alt in alternatives {
                v.visit_node(alt);
            }
        }
        NodeKind::GuardPat { pattern, guard } => {
            v.visit_node(pattern);
            v.visit_node(guard);
        }
        NodeKind::RangePat { lo, hi, .. } => {
            v.visit_node(lo);
            v.visit_node(hi);
        }

        // ── Error recovery ────────────────────────────────────────────────
        NodeKind::Error => {}
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{AIRNode, NodeId, NodeIdGen, NodeKind};
    use bock_ast::{BinOp, Ident, Literal};
    use bock_errors::{FileId, Span};

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }

    fn make_node(id: NodeId, kind: NodeKind) -> AIRNode {
        AIRNode::new(id, dummy_span(), kind)
    }

    struct NodeCounter(usize);
    impl Visitor for NodeCounter {
        fn visit_node(&mut self, node: &AIRNode) {
            self.0 += 1;
            walk_node(self, node);
        }
    }

    #[test]
    fn visitor_counts_binary_op() {
        // `1 + 2` → BinaryOp + Literal + Literal = 3 nodes
        let tree = make_node(
            0,
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(make_node(
                    1,
                    NodeKind::Literal {
                        lit: Literal::Int("1".into()),
                    },
                )),
                right: Box::new(make_node(
                    2,
                    NodeKind::Literal {
                        lit: Literal::Int("2".into()),
                    },
                )),
            },
        );
        let mut counter = NodeCounter(0);
        counter.visit_node(&tree);
        assert_eq!(counter.0, 3);
    }

    #[test]
    fn visitor_counts_block_with_tail() {
        // Block { stmts: [], tail: Literal } → 2 nodes
        let tree = make_node(
            0,
            NodeKind::Block {
                stmts: vec![],
                tail: Some(Box::new(make_node(
                    1,
                    NodeKind::Literal {
                        lit: Literal::Bool(true),
                    },
                ))),
            },
        );
        let mut counter = NodeCounter(0);
        counter.visit_node(&tree);
        assert_eq!(counter.0, 2);
    }

    #[test]
    fn visitor_walks_module() {
        let _gen = NodeIdGen::new();
        let tree = make_node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![
                    make_node(1, NodeKind::Continue),
                    make_node(2, NodeKind::Unreachable),
                ],
            },
        );
        let mut counter = NodeCounter(0);
        counter.visit_node(&tree);
        assert_eq!(counter.0, 3); // Module + 2 items
    }

    #[test]
    fn visitor_can_prune_subtree() {
        struct PruningVisitor;
        impl Visitor for PruningVisitor {
            fn visit_node(&mut self, node: &AIRNode) {
                // Only recurse into non-literal nodes
                if !matches!(node.kind, NodeKind::Literal { .. }) {
                    walk_node(self, node);
                }
            }
        }
        // This is just a compile test — no assertion needed.
        let tree = make_node(
            0,
            NodeKind::BinaryOp {
                op: BinOp::Mul,
                left: Box::new(make_node(
                    1,
                    NodeKind::Literal {
                        lit: Literal::Int("3".into()),
                    },
                )),
                right: Box::new(make_node(
                    2,
                    NodeKind::Literal {
                        lit: Literal::Int("4".into()),
                    },
                )),
            },
        );
        PruningVisitor.visit_node(&tree);
    }

    #[test]
    fn visitor_walks_match() {
        let scrutinee = make_node(
            1,
            NodeKind::Identifier {
                name: Ident {
                    name: "x".into(),
                    span: dummy_span(),
                },
            },
        );
        let arm = make_node(
            2,
            NodeKind::MatchArm {
                pattern: Box::new(make_node(3, NodeKind::WildcardPat)),
                guard: None,
                body: Box::new(make_node(
                    4,
                    NodeKind::Literal {
                        lit: Literal::Bool(true),
                    },
                )),
            },
        );
        let tree = make_node(
            0,
            NodeKind::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![arm],
            },
        );
        let mut counter = NodeCounter(0);
        counter.visit_node(&tree);
        // Match + Identifier + MatchArm + WildcardPat + Literal = 5
        assert_eq!(counter.0, 5);
    }
}
