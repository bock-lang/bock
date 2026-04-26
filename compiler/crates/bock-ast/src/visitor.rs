//! Visitor trait for AST traversal.
//!
//! Implement [`Visitor`] and override only the methods you care about.
//! Each `visit_*` method has a default implementation that walks the node's
//! children by calling the appropriate `walk_*` helper.

use crate::*;

/// A read-only visitor over the Bock AST.
///
/// Override any `visit_*` method to intercept that node type.
/// Call the corresponding `walk_*` function inside your override to recurse
/// into children, or omit it to stop the traversal at that node.
#[allow(unused_variables)]
pub trait Visitor: Sized {
    fn visit_module(&mut self, node: &Module) {
        walk_module(self, node);
    }
    fn visit_item(&mut self, node: &Item) {
        walk_item(self, node);
    }
    fn visit_fn_decl(&mut self, node: &FnDecl) {
        walk_fn_decl(self, node);
    }
    fn visit_record_decl(&mut self, node: &RecordDecl) {
        walk_record_decl(self, node);
    }
    fn visit_enum_decl(&mut self, node: &EnumDecl) {
        walk_enum_decl(self, node);
    }
    fn visit_class_decl(&mut self, node: &ClassDecl) {
        walk_class_decl(self, node);
    }
    fn visit_trait_decl(&mut self, node: &TraitDecl) {
        walk_trait_decl(self, node);
    }
    fn visit_impl_block(&mut self, node: &ImplBlock) {
        walk_impl_block(self, node);
    }
    fn visit_effect_decl(&mut self, node: &EffectDecl) {
        walk_effect_decl(self, node);
    }
    fn visit_type_alias_decl(&mut self, node: &TypeAliasDecl) {
        walk_type_alias_decl(self, node);
    }
    fn visit_const_decl(&mut self, node: &ConstDecl) {
        walk_const_decl(self, node);
    }
    fn visit_module_handle_decl(&mut self, node: &ModuleHandleDecl) {
        walk_module_handle_decl(self, node);
    }
    fn visit_property_test_decl(&mut self, node: &PropertyTestDecl) {
        walk_property_test_decl(self, node);
    }
    fn visit_import_decl(&mut self, node: &ImportDecl) {}
    fn visit_expr(&mut self, node: &Expr) {
        walk_expr(self, node);
    }
    fn visit_stmt(&mut self, node: &Stmt) {
        walk_stmt(self, node);
    }
    fn visit_block(&mut self, node: &Block) {
        walk_block(self, node);
    }
    fn visit_pattern(&mut self, node: &Pattern) {
        walk_pattern(self, node);
    }
    fn visit_type_expr(&mut self, node: &TypeExpr) {
        walk_type_expr(self, node);
    }
    fn visit_param(&mut self, node: &Param) {
        walk_param(self, node);
    }
    fn visit_match_arm(&mut self, node: &MatchArm) {
        walk_match_arm(self, node);
    }
    fn visit_annotation(&mut self, node: &Annotation) {
        walk_annotation(self, node);
    }
    fn visit_generic_param(&mut self, node: &GenericParam) {
        walk_generic_param(self, node);
    }
    fn visit_enum_variant(&mut self, node: &EnumVariant) {
        walk_enum_variant(self, node);
    }
    fn visit_type_constraint(&mut self, node: &TypeConstraint) {
        walk_type_constraint(self, node);
    }
}

// ─── Walk helpers ─────────────────────────────────────────────────────────────

pub fn walk_module<V: Visitor>(v: &mut V, node: &Module) {
    for imp in &node.imports {
        v.visit_import_decl(imp);
    }
    for item in &node.items {
        v.visit_item(item);
    }
}

pub fn walk_item<V: Visitor>(v: &mut V, node: &Item) {
    match node {
        Item::Fn(d) => v.visit_fn_decl(d),
        Item::Record(d) => v.visit_record_decl(d),
        Item::Enum(d) => v.visit_enum_decl(d),
        Item::Class(d) => v.visit_class_decl(d),
        Item::Trait(d) | Item::PlatformTrait(d) => v.visit_trait_decl(d),
        Item::Impl(d) => v.visit_impl_block(d),
        Item::Effect(d) => v.visit_effect_decl(d),
        Item::TypeAlias(d) => v.visit_type_alias_decl(d),
        Item::Const(d) => v.visit_const_decl(d),
        Item::ModuleHandle(d) => v.visit_module_handle_decl(d),
        Item::PropertyTest(d) => v.visit_property_test_decl(d),
        Item::Error { .. } => {} // Error nodes are silently skipped by visitors.
    }
}

pub fn walk_fn_decl<V: Visitor>(v: &mut V, node: &FnDecl) {
    for a in &node.annotations {
        v.visit_annotation(a);
    }
    for gp in &node.generic_params {
        v.visit_generic_param(gp);
    }
    for p in &node.params {
        v.visit_param(p);
    }
    if let Some(ret) = &node.return_type {
        v.visit_type_expr(ret);
    }
    for tc in &node.where_clause {
        v.visit_type_constraint(tc);
    }
    if let Some(body) = &node.body {
        v.visit_block(body);
    }
}

pub fn walk_record_decl<V: Visitor>(v: &mut V, node: &RecordDecl) {
    for a in &node.annotations {
        v.visit_annotation(a);
    }
    for gp in &node.generic_params {
        v.visit_generic_param(gp);
    }
    for tc in &node.where_clause {
        v.visit_type_constraint(tc);
    }
    for f in &node.fields {
        v.visit_type_expr(&f.ty);
        if let Some(def) = &f.default {
            v.visit_expr(def);
        }
    }
}

pub fn walk_enum_decl<V: Visitor>(v: &mut V, node: &EnumDecl) {
    for a in &node.annotations {
        v.visit_annotation(a);
    }
    for gp in &node.generic_params {
        v.visit_generic_param(gp);
    }
    for tc in &node.where_clause {
        v.visit_type_constraint(tc);
    }
    for var in &node.variants {
        v.visit_enum_variant(var);
    }
}

pub fn walk_class_decl<V: Visitor>(v: &mut V, node: &ClassDecl) {
    for a in &node.annotations {
        v.visit_annotation(a);
    }
    for gp in &node.generic_params {
        v.visit_generic_param(gp);
    }
    for tc in &node.where_clause {
        v.visit_type_constraint(tc);
    }
    for f in &node.fields {
        v.visit_type_expr(&f.ty);
    }
    for m in &node.methods {
        v.visit_fn_decl(m);
    }
}

pub fn walk_trait_decl<V: Visitor>(v: &mut V, node: &TraitDecl) {
    for a in &node.annotations {
        v.visit_annotation(a);
    }
    for gp in &node.generic_params {
        v.visit_generic_param(gp);
    }
    for m in &node.methods {
        v.visit_fn_decl(m);
    }
}

pub fn walk_impl_block<V: Visitor>(v: &mut V, node: &ImplBlock) {
    for a in &node.annotations {
        v.visit_annotation(a);
    }
    for gp in &node.generic_params {
        v.visit_generic_param(gp);
    }
    v.visit_type_expr(&node.target);
    for tc in &node.where_clause {
        v.visit_type_constraint(tc);
    }
    for ta in &node.type_assignments {
        v.visit_type_expr(&ta.type_expr);
    }
    for m in &node.methods {
        v.visit_fn_decl(m);
    }
}

pub fn walk_effect_decl<V: Visitor>(v: &mut V, node: &EffectDecl) {
    for a in &node.annotations {
        v.visit_annotation(a);
    }
    for gp in &node.generic_params {
        v.visit_generic_param(gp);
    }
    for op in &node.operations {
        v.visit_fn_decl(op);
    }
}

pub fn walk_type_alias_decl<V: Visitor>(v: &mut V, node: &TypeAliasDecl) {
    for a in &node.annotations {
        v.visit_annotation(a);
    }
    for gp in &node.generic_params {
        v.visit_generic_param(gp);
    }
    v.visit_type_expr(&node.ty);
    for tc in &node.where_clause {
        v.visit_type_constraint(tc);
    }
}

pub fn walk_const_decl<V: Visitor>(v: &mut V, node: &ConstDecl) {
    for a in &node.annotations {
        v.visit_annotation(a);
    }
    v.visit_type_expr(&node.ty);
    v.visit_expr(&node.value);
}

pub fn walk_module_handle_decl<V: Visitor>(v: &mut V, node: &ModuleHandleDecl) {
    v.visit_expr(&node.handler);
}

pub fn walk_property_test_decl<V: Visitor>(v: &mut V, node: &PropertyTestDecl) {
    for b in &node.bindings {
        v.visit_type_expr(&b.ty);
    }
    v.visit_block(&node.body);
}

pub fn walk_block<V: Visitor>(v: &mut V, node: &Block) {
    for s in &node.stmts {
        v.visit_stmt(s);
    }
    if let Some(tail) = &node.tail {
        v.visit_expr(tail);
    }
}

pub fn walk_stmt<V: Visitor>(v: &mut V, node: &Stmt) {
    match node {
        Stmt::Let(s) => {
            v.visit_pattern(&s.pattern);
            if let Some(ty) = &s.ty {
                v.visit_type_expr(ty);
            }
            v.visit_expr(&s.value);
        }
        Stmt::Expr(e) => v.visit_expr(e),
        Stmt::For(s) => {
            v.visit_pattern(&s.pattern);
            v.visit_expr(&s.iterable);
            v.visit_block(&s.body);
        }
        Stmt::While(s) => {
            v.visit_expr(&s.condition);
            v.visit_block(&s.body);
        }
        Stmt::Loop(s) => v.visit_block(&s.body),
        Stmt::Guard(s) => {
            if let Some(pat) = &s.let_pattern {
                v.visit_pattern(pat);
            }
            v.visit_expr(&s.condition);
            v.visit_block(&s.else_block);
        }
        Stmt::Handling(s) => {
            for h in &s.handlers {
                v.visit_expr(&h.handler);
            }
            v.visit_block(&s.body);
        }
        Stmt::Empty => {}
    }
}

pub fn walk_expr<V: Visitor>(v: &mut V, node: &Expr) {
    match node {
        Expr::Literal { .. }
        | Expr::Identifier { .. }
        | Expr::Continue { .. }
        | Expr::Unreachable { .. }
        | Expr::Placeholder { .. } => {}

        Expr::Binary { left, right, .. }
        | Expr::Pipe { left, right, .. }
        | Expr::Compose { left, right, .. } => {
            v.visit_expr(left);
            v.visit_expr(right);
        }
        Expr::Unary { operand, .. }
        | Expr::Try { expr: operand, .. }
        | Expr::Await { expr: operand, .. } => {
            v.visit_expr(operand);
        }
        Expr::Assign { target, value, .. } => {
            v.visit_expr(target);
            v.visit_expr(value);
        }
        Expr::Call {
            callee,
            args,
            type_args,
            ..
        } => {
            v.visit_expr(callee);
            for a in args {
                v.visit_expr(&a.value);
            }
            for t in type_args {
                v.visit_type_expr(t);
            }
        }
        Expr::MethodCall {
            receiver,
            args,
            type_args,
            ..
        } => {
            v.visit_expr(receiver);
            for a in args {
                v.visit_expr(&a.value);
            }
            for t in type_args {
                v.visit_type_expr(t);
            }
        }
        Expr::FieldAccess { object, .. } => v.visit_expr(object),
        Expr::Index { object, index, .. } => {
            v.visit_expr(object);
            v.visit_expr(index);
        }
        Expr::Lambda { params, body, .. } => {
            for p in params {
                v.visit_param(p);
            }
            v.visit_expr(body);
        }
        Expr::If {
            condition,
            let_pattern,
            then_block,
            else_block,
            ..
        } => {
            if let Some(pat) = let_pattern {
                v.visit_pattern(pat);
            }
            v.visit_expr(condition);
            v.visit_block(then_block);
            if let Some(else_e) = else_block {
                v.visit_expr(else_e);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            v.visit_expr(scrutinee);
            for arm in arms {
                v.visit_match_arm(arm);
            }
        }
        Expr::Loop { body, .. } => v.visit_block(body),
        Expr::Block { block, .. } => v.visit_block(block),
        Expr::RecordConstruct { fields, spread, .. } => {
            for f in fields {
                if let Some(val) = &f.value {
                    v.visit_expr(val);
                }
            }
            if let Some(s) = spread {
                v.visit_expr(&s.expr);
            }
        }
        Expr::ListLiteral { elems, .. }
        | Expr::SetLiteral { elems, .. }
        | Expr::TupleLiteral { elems, .. } => {
            for e in elems {
                v.visit_expr(e);
            }
        }
        Expr::MapLiteral { entries, .. } => {
            for (k, val) in entries {
                v.visit_expr(k);
                v.visit_expr(val);
            }
        }
        Expr::Range { lo, hi, .. } => {
            v.visit_expr(lo);
            v.visit_expr(hi);
        }
        Expr::Return { value, .. } | Expr::Break { value, .. } => {
            if let Some(val) = value {
                v.visit_expr(val);
            }
        }
        Expr::Interpolation { parts, .. } => {
            for part in parts {
                if let InterpolationPart::Expr(e) = part {
                    v.visit_expr(e);
                }
            }
        }
        Expr::Is {
            expr, type_expr, ..
        } => {
            v.visit_expr(expr);
            v.visit_type_expr(type_expr);
        }
    }
}

pub fn walk_pattern<V: Visitor>(v: &mut V, node: &Pattern) {
    match node {
        Pattern::Wildcard { .. }
        | Pattern::Bind { .. }
        | Pattern::MutBind { .. }
        | Pattern::Literal { .. }
        | Pattern::Rest { .. } => {}

        Pattern::Constructor { fields, .. } => {
            for f in fields {
                v.visit_pattern(f);
            }
        }
        Pattern::Record { fields, .. } => {
            for f in fields {
                if let Some(pat) = &f.pattern {
                    v.visit_pattern(pat);
                }
            }
        }
        Pattern::Tuple { elems, .. } => {
            for e in elems {
                v.visit_pattern(e);
            }
        }
        Pattern::List { elems, rest, .. } => {
            for e in elems {
                v.visit_pattern(e);
            }
            if let Some(r) = rest {
                v.visit_pattern(r);
            }
        }
        Pattern::Or { alternatives, .. } => {
            for alt in alternatives {
                v.visit_pattern(alt);
            }
        }
        Pattern::Range { lo, hi, .. } => {
            v.visit_pattern(lo);
            v.visit_pattern(hi);
        }
    }
}

pub fn walk_type_expr<V: Visitor>(v: &mut V, node: &TypeExpr) {
    match node {
        TypeExpr::SelfType { .. } => {}
        TypeExpr::Named { args, .. } => {
            for a in args {
                v.visit_type_expr(a);
            }
        }
        TypeExpr::Tuple { elems, .. } => {
            for e in elems {
                v.visit_type_expr(e);
            }
        }
        TypeExpr::Function { params, ret, .. } => {
            for p in params {
                v.visit_type_expr(p);
            }
            v.visit_type_expr(ret);
        }
        TypeExpr::Optional { inner, .. } => v.visit_type_expr(inner),
    }
}

pub fn walk_param<V: Visitor>(v: &mut V, node: &Param) {
    v.visit_pattern(&node.pattern);
    if let Some(ty) = &node.ty {
        v.visit_type_expr(ty);
    }
    if let Some(def) = &node.default {
        v.visit_expr(def);
    }
}

pub fn walk_match_arm<V: Visitor>(v: &mut V, node: &MatchArm) {
    v.visit_pattern(&node.pattern);
    if let Some(guard) = &node.guard {
        v.visit_expr(guard);
    }
    v.visit_expr(&node.body);
}

pub fn walk_annotation<V: Visitor>(v: &mut V, node: &Annotation) {
    for arg in &node.args {
        v.visit_expr(&arg.value);
    }
}

pub fn walk_generic_param<V: Visitor>(_v: &mut V, _node: &GenericParam) {
    // Bounds are TypePath (identifiers), not TypeExpr — nothing to recurse into.
}

pub fn walk_enum_variant<V: Visitor>(v: &mut V, node: &EnumVariant) {
    match node {
        EnumVariant::Unit { .. } => {}
        EnumVariant::Struct { fields, .. } => {
            for f in fields {
                v.visit_type_expr(&f.ty);
                if let Some(def) = &f.default {
                    v.visit_expr(def);
                }
            }
        }
        EnumVariant::Tuple { tys, .. } => {
            for ty in tys {
                v.visit_type_expr(ty);
            }
        }
    }
}

pub fn walk_type_constraint<V: Visitor>(_v: &mut V, _node: &TypeConstraint) {
    // Bounds are TypePath (identifiers), not TypeExpr — nothing to recurse into.
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_errors::FileId;

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

    struct ExprCounter(usize);
    impl Visitor for ExprCounter {
        fn visit_expr(&mut self, node: &Expr) {
            self.0 += 1;
            walk_expr(self, node);
        }
    }

    #[test]
    fn visitor_counts_exprs_in_binary() {
        // `1 + 2` has 3 exprs: the binary, and each literal child
        let e = Expr::Binary {
            id: 0,
            span: dummy_span(),
            op: BinOp::Add,
            left: Box::new(Expr::Literal {
                id: 1,
                span: dummy_span(),
                lit: Literal::Int("1".into()),
            }),
            right: Box::new(Expr::Literal {
                id: 2,
                span: dummy_span(),
                lit: Literal::Int("2".into()),
            }),
        };
        let mut counter = ExprCounter(0);
        counter.visit_expr(&e);
        assert_eq!(counter.0, 3);
    }

    #[test]
    fn visitor_walks_module() {
        let m = Module {
            id: 0,
            span: dummy_span(),
            doc: vec![],
            path: None,
            imports: vec![],
            items: vec![Item::Const(ConstDecl {
                id: 1,
                span: dummy_span(),
                annotations: vec![],
                visibility: Visibility::Public,
                name: dummy_ident("X"),
                ty: TypeExpr::Named {
                    id: 2,
                    span: dummy_span(),
                    path: TypePath {
                        segments: vec![dummy_ident("Int")],
                        span: dummy_span(),
                    },
                    args: vec![],
                },
                value: Expr::Literal {
                    id: 3,
                    span: dummy_span(),
                    lit: Literal::Int("1".into()),
                },
            })],
        };
        let mut counter = ExprCounter(0);
        counter.visit_module(&m);
        assert_eq!(counter.0, 1); // just the literal
    }
}
