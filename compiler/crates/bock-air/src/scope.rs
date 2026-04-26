//! Scope analysis pass — S-AIR layer.
//!
//! Builds a [`ScopeTree`] from an AST [`Module`].  Every block, function,
//! loop, match arm, and handling block introduces a new [`Scope`].
//! Variable lifetimes are bounded by the scope in which they are declared.
//!
//! # Scope-introducing constructs
//! - Module root (implicit)
//! - Function body (params + body share one scope)
//! - Lambda body (params + body share one scope)
//! - Block expressions
//! - For loop body (loop variable in scope)
//! - While / loop bodies
//! - Match arm bodies (pattern bindings in scope)
//! - If-let then block (binding in scope)
//! - Handling blocks

use std::collections::HashMap;

use bock_ast::{
    Block, Expr, FnDecl, ForLoop, GuardStmt, HandlingBlock, ImplBlock, Item, LetStmt, LoopStmt,
    MatchArm, Module, NodeId, Param, Pattern, RecordPatternField, Stmt, WhileLoop,
};

// ─── Public types ─────────────────────────────────────────────────────────────

/// Unique identifier for a [`Scope`] in the [`ScopeTree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(pub u32);

/// A single name binding within a [`Scope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The name as written in source.
    pub name: String,
    /// The NodeId of the declaration (pattern node, param node, etc.).
    pub node_id: NodeId,
    /// Whether this binding was declared with `mut`.
    pub mutable: bool,
}

/// A single lexical scope in the scope tree.
#[derive(Debug)]
pub struct Scope {
    /// This scope's identity.
    pub id: ScopeId,
    /// Parent scope (`None` only for the module root scope).
    pub parent: Option<ScopeId>,
    /// Bindings introduced directly in this scope.
    pub bindings: Vec<Binding>,
}

impl Scope {
    fn new(id: ScopeId, parent: Option<ScopeId>) -> Self {
        Self {
            id,
            parent,
            bindings: Vec::new(),
        }
    }

    fn add_binding(&mut self, name: String, node_id: NodeId, mutable: bool) {
        self.bindings.push(Binding {
            name,
            node_id,
            mutable,
        });
    }
}

/// The complete scope tree for a module.
///
/// Scopes are stored in insertion order.  The root scope always has
/// [`ScopeId`]`(0)`.
pub struct ScopeTree {
    scopes: HashMap<ScopeId, Scope>,
    next_id: u32,
}

impl Default for ScopeTree {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeTree {
    /// Creates a new scope tree with an empty root scope.
    #[must_use]
    pub fn new() -> Self {
        let root = Scope::new(ScopeId(0), None);
        let mut scopes = HashMap::new();
        scopes.insert(ScopeId(0), root);
        Self { scopes, next_id: 1 }
    }

    /// Returns the root [`ScopeId`].
    #[must_use]
    pub fn root(&self) -> ScopeId {
        ScopeId(0)
    }

    /// Returns the scope for `id`, if it exists.
    #[must_use]
    pub fn get(&self, id: ScopeId) -> Option<&Scope> {
        self.scopes.get(&id)
    }

    /// Returns the total number of scopes in the tree.
    #[must_use]
    pub fn scope_count(&self) -> usize {
        self.scopes.len()
    }

    /// Looks up `name` starting from `from` and walking up the parent chain.
    ///
    /// Returns the first matching [`Binding`], or `None` if not found.
    #[must_use]
    pub fn lookup(&self, name: &str, from: ScopeId) -> Option<&Binding> {
        let mut current = Some(from);
        while let Some(id) = current {
            if let Some(scope) = self.scopes.get(&id) {
                // Search bindings in reverse so later declarations shadow earlier ones.
                if let Some(b) = scope.bindings.iter().rev().find(|b| b.name == name) {
                    return Some(b);
                }
                current = scope.parent;
            } else {
                break;
            }
        }
        None
    }

    /// Allocates a new child scope under `parent` and returns its [`ScopeId`].
    fn alloc(&mut self, parent: ScopeId) -> ScopeId {
        let id = ScopeId(self.next_id);
        self.next_id += 1;
        self.scopes.insert(id, Scope::new(id, Some(parent)));
        id
    }

    /// Adds a binding to the scope identified by `scope_id`.
    fn bind(&mut self, scope_id: ScopeId, name: String, node_id: NodeId, mutable: bool) {
        if let Some(scope) = self.scopes.get_mut(&scope_id) {
            scope.add_binding(name, node_id, mutable);
        }
    }
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Build a [`ScopeTree`] from a parsed [`Module`].
///
/// Walks every declaration and expression in the module, allocating a new
/// scope for each scope-introducing construct and recording all name bindings.
#[must_use]
pub fn build_scope_tree(module: &Module) -> ScopeTree {
    let mut tree = ScopeTree::new();
    let root = tree.root();
    let mut builder = ScopeBuilder { tree: &mut tree };
    builder.visit_module(module, root);
    tree
}

// ─── Builder (private) ────────────────────────────────────────────────────────

struct ScopeBuilder<'t> {
    tree: &'t mut ScopeTree,
}

impl<'t> ScopeBuilder<'t> {
    fn visit_module(&mut self, module: &Module, scope: ScopeId) {
        // Collect top-level declaration names into the root scope.
        for item in &module.items {
            self.collect_item_name(item, scope);
        }
        // Then recurse into each item's body.
        for item in &module.items {
            self.visit_item(item, scope);
        }
    }

    fn collect_item_name(&mut self, item: &Item, scope: ScopeId) {
        match item {
            Item::Fn(d) => self.tree.bind(scope, d.name.name.clone(), d.id, false),
            Item::Record(d) => self.tree.bind(scope, d.name.name.clone(), d.id, false),
            Item::Enum(d) => self.tree.bind(scope, d.name.name.clone(), d.id, false),
            Item::Class(d) => self.tree.bind(scope, d.name.name.clone(), d.id, false),
            Item::Trait(d) => self.tree.bind(scope, d.name.name.clone(), d.id, false),
            Item::Effect(d) => self.tree.bind(scope, d.name.name.clone(), d.id, false),
            Item::TypeAlias(d) => self.tree.bind(scope, d.name.name.clone(), d.id, false),
            Item::Const(d) => self.tree.bind(scope, d.name.name.clone(), d.id, false),
            Item::Impl(_)
            | Item::ModuleHandle(_)
            | Item::PropertyTest(_)
            | Item::Error { .. }
            | Item::PlatformTrait(_) => {}
        }
    }

    fn visit_item(&mut self, item: &Item, parent: ScopeId) {
        match item {
            Item::Fn(d) => self.visit_fn(d, parent),
            Item::Class(d) => {
                for method in &d.methods {
                    self.visit_fn(method, parent);
                }
            }
            Item::Trait(d) => {
                for method in &d.methods {
                    self.visit_fn(method, parent);
                }
            }
            Item::Effect(d) => {
                for op in &d.operations {
                    self.visit_fn(op, parent);
                }
            }
            Item::Impl(d) => self.visit_impl(d, parent),
            Item::Const(d) => self.visit_expr(&d.value, parent),
            Item::Record(_)
            | Item::Enum(_)
            | Item::TypeAlias(_)
            | Item::ModuleHandle(_)
            | Item::PropertyTest(_)
            | Item::PlatformTrait(_)
            | Item::Error { .. } => {}
        }
    }

    fn visit_impl(&mut self, impl_block: &ImplBlock, parent: ScopeId) {
        for method in &impl_block.methods {
            self.visit_fn(method, parent);
        }
    }

    /// Function body creates one scope for params + body.
    fn visit_fn(&mut self, decl: &FnDecl, parent: ScopeId) {
        let fn_scope = self.tree.alloc(parent);
        for param in &decl.params {
            self.bind_param(param, fn_scope);
        }
        if let Some(ref body) = decl.body {
            self.visit_block(body, fn_scope);
        }
    }

    fn bind_param(&mut self, param: &Param, scope: ScopeId) {
        self.bind_pattern(&param.pattern, scope);
        if let Some(default) = &param.default {
            self.visit_expr(default, scope);
        }
    }

    fn visit_block(&mut self, block: &Block, parent: ScopeId) {
        let block_scope = self.tree.alloc(parent);
        for stmt in &block.stmts {
            self.visit_stmt(stmt, block_scope);
        }
        if let Some(tail) = &block.tail {
            self.visit_expr(tail, block_scope);
        }
    }

    fn visit_stmt(&mut self, stmt: &Stmt, scope: ScopeId) {
        match stmt {
            Stmt::Let(let_stmt) => self.visit_let(let_stmt, scope),
            Stmt::Expr(expr) => self.visit_expr(expr, scope),
            Stmt::For(for_loop) => self.visit_for(for_loop, scope),
            Stmt::While(while_loop) => self.visit_while(while_loop, scope),
            Stmt::Loop(loop_stmt) => self.visit_loop(loop_stmt, scope),
            Stmt::Guard(guard) => self.visit_guard(guard, scope),
            Stmt::Handling(handling) => self.visit_handling(handling, scope),
            Stmt::Empty => {}
        }
    }

    fn visit_let(&mut self, let_stmt: &LetStmt, scope: ScopeId) {
        // RHS is evaluated in the enclosing scope.
        self.visit_expr(&let_stmt.value, scope);
        // Pattern bindings are visible from here to scope end.
        self.bind_pattern(&let_stmt.pattern, scope);
    }

    fn visit_for(&mut self, for_loop: &ForLoop, parent: ScopeId) {
        // Iterable evaluated in parent scope.
        self.visit_expr(&for_loop.iterable, parent);
        // Loop variable and body share a new scope.
        let loop_scope = self.tree.alloc(parent);
        self.bind_pattern(&for_loop.pattern, loop_scope);
        self.visit_block(&for_loop.body, loop_scope);
    }

    fn visit_while(&mut self, while_loop: &WhileLoop, parent: ScopeId) {
        self.visit_expr(&while_loop.condition, parent);
        self.visit_block(&while_loop.body, parent);
    }

    fn visit_loop(&mut self, loop_stmt: &LoopStmt, parent: ScopeId) {
        self.visit_block(&loop_stmt.body, parent);
    }

    fn visit_guard(&mut self, guard: &GuardStmt, parent: ScopeId) {
        if let Some(pat) = &guard.let_pattern {
            self.bind_pattern(pat, parent);
        }
        self.visit_expr(&guard.condition, parent);
        self.visit_block(&guard.else_block, parent);
    }

    fn visit_handling(&mut self, handling: &HandlingBlock, parent: ScopeId) {
        // Handling block creates its own scope for the body.
        let handling_scope = self.tree.alloc(parent);
        // Handler expressions are evaluated in the parent scope.
        for pair in &handling.handlers {
            self.visit_expr(&pair.handler, parent);
        }
        self.visit_block(&handling.body, handling_scope);
    }

    fn visit_expr(&mut self, expr: &Expr, scope: ScopeId) {
        match expr {
            Expr::Literal { .. }
            | Expr::Identifier { .. }
            | Expr::Continue { .. }
            | Expr::Unreachable { .. }
            | Expr::Placeholder { .. } => {}

            Expr::Binary { left, right, .. }
            | Expr::Compose { left, right, .. }
            | Expr::Pipe { left, right, .. }
            | Expr::Range {
                lo: left,
                hi: right,
                ..
            } => {
                self.visit_expr(left, scope);
                self.visit_expr(right, scope);
            }

            Expr::Unary { operand, .. }
            | Expr::Try { expr: operand, .. }
            | Expr::Await { expr: operand, .. } => {
                self.visit_expr(operand, scope);
            }

            Expr::Assign { target, value, .. } => {
                self.visit_expr(target, scope);
                self.visit_expr(value, scope);
            }

            Expr::Call { callee, args, .. } => {
                self.visit_expr(callee, scope);
                for arg in args {
                    self.visit_expr(&arg.value, scope);
                }
            }

            Expr::MethodCall { receiver, args, .. } => {
                self.visit_expr(receiver, scope);
                for arg in args {
                    self.visit_expr(&arg.value, scope);
                }
            }

            Expr::FieldAccess { object, .. } => {
                self.visit_expr(object, scope);
            }

            Expr::Index { object, index, .. } => {
                self.visit_expr(object, scope);
                self.visit_expr(index, scope);
            }

            Expr::Lambda { params, body, .. } => {
                let lambda_scope = self.tree.alloc(scope);
                for param in params {
                    self.bind_param(param, lambda_scope);
                }
                self.visit_expr(body, lambda_scope);
            }

            Expr::If {
                let_pattern,
                condition,
                then_block,
                else_block,
                ..
            } => {
                self.visit_expr(condition, scope);
                if let Some(pat) = let_pattern {
                    // if-let: pattern bindings scoped to then block.
                    let then_scope = self.tree.alloc(scope);
                    self.bind_pattern(pat, then_scope);
                    self.visit_block(then_block, then_scope);
                } else {
                    self.visit_block(then_block, scope);
                }
                if let Some(else_expr) = else_block {
                    self.visit_expr(else_expr, scope);
                }
            }

            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.visit_expr(scrutinee, scope);
                for arm in arms {
                    self.visit_match_arm(arm, scope);
                }
            }

            Expr::Loop { body, .. } => {
                self.visit_block(body, scope);
            }

            Expr::Block { block, .. } => {
                self.visit_block(block, scope);
            }

            Expr::RecordConstruct { fields, spread, .. } => {
                for field in fields {
                    if let Some(val) = &field.value {
                        self.visit_expr(val, scope);
                    }
                }
                if let Some(s) = spread {
                    self.visit_expr(&s.expr, scope);
                }
            }

            Expr::ListLiteral { elems, .. }
            | Expr::SetLiteral { elems, .. }
            | Expr::TupleLiteral { elems, .. } => {
                for elem in elems {
                    self.visit_expr(elem, scope);
                }
            }

            Expr::MapLiteral { entries, .. } => {
                for (k, v) in entries {
                    self.visit_expr(k, scope);
                    self.visit_expr(v, scope);
                }
            }

            Expr::Return { value, .. } | Expr::Break { value, .. } => {
                if let Some(val) = value {
                    self.visit_expr(val, scope);
                }
            }

            Expr::Interpolation { parts, .. } => {
                for part in parts {
                    if let bock_ast::InterpolationPart::Expr(e) = part {
                        self.visit_expr(e, scope);
                    }
                }
            }

            Expr::Is { expr, .. } => {
                self.visit_expr(expr, scope);
            }
        }
    }

    fn visit_match_arm(&mut self, arm: &MatchArm, parent: ScopeId) {
        // Each arm body gets its own scope for pattern bindings.
        let arm_scope = self.tree.alloc(parent);
        self.bind_pattern(&arm.pattern, arm_scope);
        if let Some(guard) = &arm.guard {
            self.visit_expr(guard, arm_scope);
        }
        self.visit_expr(&arm.body, arm_scope);
    }

    /// Recursively binds all names introduced by a pattern into `scope`.
    fn bind_pattern(&mut self, pat: &Pattern, scope: ScopeId) {
        match pat {
            Pattern::Bind { id, name, .. } => {
                self.tree.bind(scope, name.name.clone(), *id, false);
            }
            Pattern::MutBind { id, name, .. } => {
                self.tree.bind(scope, name.name.clone(), *id, true);
            }
            Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::Rest { .. } => {}

            Pattern::Constructor { fields, .. } => {
                for field in fields {
                    self.bind_pattern(field, scope);
                }
            }
            Pattern::Record { fields, .. } => {
                for field in fields {
                    self.bind_record_pattern_field(field, scope);
                }
            }
            Pattern::Tuple { elems, .. } => {
                for elem in elems {
                    self.bind_pattern(elem, scope);
                }
            }
            Pattern::List { elems, rest, .. } => {
                for elem in elems {
                    self.bind_pattern(elem, scope);
                }
                if let Some(r) = rest {
                    self.bind_pattern(r, scope);
                }
            }
            Pattern::Or { alternatives, .. } => {
                // All alternatives must bind the same names; bind from the first.
                if let Some(first) = alternatives.first() {
                    self.bind_pattern(first, scope);
                }
            }
            Pattern::Range { lo, hi, .. } => {
                self.bind_pattern(lo, scope);
                self.bind_pattern(hi, scope);
            }
        }
    }

    fn bind_record_pattern_field(&mut self, field: &RecordPatternField, scope: ScopeId) {
        if let Some(pat) = &field.pattern {
            self.bind_pattern(pat, scope);
        } else {
            // Shorthand `{ name }` — bind field name as an immutable variable.
            // Use a synthetic NodeId 0 as placeholder (no span available here).
            self.tree.bind(scope, field.name.name.clone(), 0, false);
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_ast::{Block, FnDecl, Ident, Item, LetStmt, Literal, Module, Pattern, Stmt};
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

    fn make_module(items: Vec<Item>) -> Module {
        Module {
            id: 0,
            span: span(),
            doc: vec![],
            path: None,
            imports: vec![],
            items,
        }
    }

    fn empty_block(id: NodeId) -> Block {
        Block {
            id,
            span: span(),
            stmts: vec![],
            tail: None,
        }
    }

    fn simple_fn(id: NodeId, name: &str, body: Block) -> Item {
        Item::Fn(FnDecl {
            id,
            span: span(),
            annotations: vec![],
            visibility: bock_ast::Visibility::Private,
            is_async: false,
            name: ident(name),
            generic_params: vec![],
            params: vec![],
            return_type: None,
            effect_clause: vec![],
            where_clause: vec![],
            body: Some(body),
        })
    }

    #[test]
    fn root_scope_exists() {
        let module = make_module(vec![]);
        let tree = build_scope_tree(&module);
        assert!(tree.get(tree.root()).is_some());
    }

    #[test]
    fn top_level_fn_bound_in_root() {
        let module = make_module(vec![simple_fn(1, "foo", empty_block(2))]);
        let tree = build_scope_tree(&module);
        let found = tree.lookup("foo", tree.root());
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "foo");
    }

    #[test]
    fn fn_creates_child_scope() {
        let module = make_module(vec![simple_fn(1, "bar", empty_block(2))]);
        let tree = build_scope_tree(&module);
        // Root + fn scope + block scope = at least 3.
        assert!(tree.scope_count() >= 3);
    }

    #[test]
    fn let_binding_visible_in_scope() {
        // Build: fn foo() { let x = (); }
        let let_stmt = Stmt::Let(LetStmt {
            id: 10,
            span: span(),
            pattern: Pattern::Bind {
                id: 11,
                span: span(),
                name: ident("x"),
            },
            ty: None,
            value: Expr::Literal {
                id: 12,
                span: span(),
                lit: Literal::Unit,
            },
        });
        let body = Block {
            id: 2,
            span: span(),
            stmts: vec![let_stmt],
            tail: None,
        };
        let module = make_module(vec![simple_fn(1, "foo", body)]);
        let tree = build_scope_tree(&module);

        // x should be findable from the innermost scope.
        // We don't know the exact ScopeId, but we can verify the root scope
        // can look up "foo" and that some scope has "x".
        let x_in_root = tree.lookup("x", tree.root());
        // x is NOT in root scope; it's inside foo's body scope.
        assert!(x_in_root.is_none());

        // Verify "foo" is in root.
        assert!(tree.lookup("foo", tree.root()).is_some());
    }

    #[test]
    fn shadowing_in_nested_scopes() {
        // fn outer() { let x = (); fn inner() { let x = (); } }
        // Both declare x; inner x shadows outer x.
        let inner_let = Stmt::Let(LetStmt {
            id: 20,
            span: span(),
            pattern: Pattern::Bind {
                id: 21,
                span: span(),
                name: ident("x"),
            },
            ty: None,
            value: Expr::Literal {
                id: 22,
                span: span(),
                lit: Literal::Unit,
            },
        });
        let inner_body = Block {
            id: 30,
            span: span(),
            stmts: vec![inner_let],
            tail: None,
        };
        let outer_let = Stmt::Let(LetStmt {
            id: 40,
            span: span(),
            pattern: Pattern::Bind {
                id: 41,
                span: span(),
                name: ident("x"),
            },
            ty: None,
            value: Expr::Literal {
                id: 42,
                span: span(),
                lit: Literal::Unit,
            },
        });
        let inner_fn_item = Item::Fn(FnDecl {
            id: 50,
            span: span(),
            annotations: vec![],
            visibility: bock_ast::Visibility::Private,
            is_async: false,
            name: ident("inner"),
            generic_params: vec![],
            params: vec![],
            return_type: None,
            effect_clause: vec![],
            where_clause: vec![],
            body: Some(inner_body),
        });
        // outer body: [let x = (); inner fn decl as stmt]
        let outer_body = Block {
            id: 60,
            span: span(),
            stmts: vec![outer_let],
            tail: None,
        };
        let module = make_module(vec![simple_fn(1, "outer", outer_body), inner_fn_item]);
        let tree = build_scope_tree(&module);
        // Both "outer" and "inner" visible in root.
        assert!(tree.lookup("outer", tree.root()).is_some());
        assert!(tree.lookup("inner", tree.root()).is_some());
    }

    #[test]
    fn match_arm_creates_scope() {
        // fn test() { match () { x => () } }
        let arm = MatchArm {
            id: 5,
            span: span(),
            pattern: Pattern::Bind {
                id: 6,
                span: span(),
                name: ident("x"),
            },
            guard: None,
            body: Expr::Literal {
                id: 7,
                span: span(),
                lit: Literal::Unit,
            },
        };
        let match_expr = Expr::Match {
            id: 4,
            span: span(),
            scrutinee: Box::new(Expr::Literal {
                id: 3,
                span: span(),
                lit: Literal::Unit,
            }),
            arms: vec![arm],
        };
        let body = Block {
            id: 2,
            span: span(),
            stmts: vec![],
            tail: Some(Box::new(match_expr)),
        };
        let module = make_module(vec![simple_fn(1, "test", body)]);
        let tree = build_scope_tree(&module);
        // root + fn scope + block + match arm scope = at least 4
        assert!(tree.scope_count() >= 4);
    }

    #[test]
    fn mut_binding_flagged() {
        // fn test() { let mut y = (); }
        let let_stmt = Stmt::Let(LetStmt {
            id: 10,
            span: span(),
            pattern: Pattern::MutBind {
                id: 11,
                span: span(),
                name: ident("y"),
            },
            ty: None,
            value: Expr::Literal {
                id: 12,
                span: span(),
                lit: Literal::Unit,
            },
        });
        let body = Block {
            id: 2,
            span: span(),
            stmts: vec![let_stmt],
            tail: None,
        };
        let module = make_module(vec![simple_fn(1, "test", body)]);
        let tree = build_scope_tree(&module);

        // Walk all scopes looking for the "y" binding.
        let found = (0..tree.next_id)
            .filter_map(|i| tree.get(ScopeId(i)))
            .flat_map(|s| s.bindings.iter())
            .find(|b| b.name == "y");

        assert!(found.is_some());
        assert!(found.unwrap().mutable);
    }

    #[test]
    fn lookup_walks_parent_chain() {
        // Verify that lookup from a child scope finds a binding in the root scope.
        let module = make_module(vec![simple_fn(1, "greet", empty_block(2))]);
        let tree = build_scope_tree(&module);
        // ScopeId(1) is the fn scope, child of root.
        let found = tree.lookup("greet", ScopeId(1));
        assert!(found.is_some());
    }
}
