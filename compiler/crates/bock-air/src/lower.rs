//! AST to S-AIR lowering pass.
//!
//! Transforms a parsed [`Module`] into an S-AIR [`AIRNode`] tree.
//! Applies syntactic desugaring:
//! - **Pipe** (`|>`) → function calls (three cases: bare, implicit-first, placeholder)
//! - **Compose** (`>>`) → lambda `(x) => g(f(x))`
//! - **Method calls** → `Call` with self prepended
//! - **For loops** → iterator protocol (loop + match on `next()`)
//! - **`if let`** → `match` expression

use bock_ast::{
    Block, ConstDecl, EffectDecl, EnumDecl, EnumVariant, Expr, FnDecl, ForLoop, GuardStmt,
    HandlingBlock, Ident, ImplBlock, ImportDecl, InterpolationPart, Item, LetStmt, LoopStmt,
    MatchArm, Module, ModuleHandleDecl, Param, Pattern, PropertyTestDecl, RecordDecl,
    RecordPatternField, Stmt, TraitDecl, TypeAliasDecl, TypeExpr, WhileLoop,
};
use bock_errors::Span;

use crate::{
    resolve::{NameKind, SymbolTable},
    stubs::Value,
    AIRNode, AirArg, AirHandlerPair, AirInterpolationPart, AirMapEntry, AirRecordField,
    AirRecordPatternField, EnumVariantPayload, NodeIdGen, NodeKind,
};

// ─── Public entry point ────────────────────────────────────────────────────────

/// Lower a parsed [`Module`] into an S-AIR tree.
///
/// Assigns fresh [`crate::NodeId`]s to every AIR node, attaches resolved names
/// (from `symbols`) and a scope counter into each node's metadata, and applies
/// all S-AIR desugaring rules.
///
/// Metadata keys set by this pass:
/// - `"scope_id"` ([`Value::Int`]) — the lexical scope this node lives in.
/// - `"resolved_def_id"` ([`Value::Int`]) — for identifier nodes, the NodeId of
///   the definition site (only when the name was resolved by the name-resolution
///   pass).
/// - `"resolved_kind"` ([`Value::String`]) — for identifier nodes, the
///   [`crate::NameKind`] debug string.
#[must_use]
pub fn lower_module(module: &Module, id_gen: &NodeIdGen, symbols: &SymbolTable) -> AIRNode {
    let mut lowerer = Lowerer::new(id_gen, symbols);
    lowerer.lower_module(module)
}

// ─── Helpers (free functions) ──────────────────────────────────────────────────

/// Create a synthetic [`Ident`] at the given span (used for generated names).
fn synth_ident(name: &str, span: Span) -> Ident {
    Ident {
        name: name.to_string(),
        span,
    }
}

// ─── Lowerer (private) ────────────────────────────────────────────────────────

struct Lowerer<'a> {
    id_gen: &'a NodeIdGen,
    symbols: &'a SymbolTable,
    /// Monotonic scope counter; 0 is the module root.
    scope_counter: u32,
}

impl<'a> Lowerer<'a> {
    fn new(id_gen: &'a NodeIdGen, symbols: &'a SymbolTable) -> Self {
        Self {
            id_gen,
            symbols,
            scope_counter: 1,
        }
    }

    /// Allocates a fresh scope ID (increments the counter).
    fn alloc_scope(&mut self) -> u32 {
        let id = self.scope_counter;
        self.scope_counter += 1;
        id
    }

    /// Builds an AIR node with a fresh `NodeId` and the given scope in metadata.
    fn make_node(&self, span: Span, kind: NodeKind, scope: u32) -> AIRNode {
        let id = self.id_gen.next();
        let mut node = AIRNode::new(id, span, kind);
        node.metadata
            .insert("scope_id".to_string(), Value::Int(scope as i64));
        node
    }

    // ── Module ────────────────────────────────────────────────────────────────

    fn lower_module(&mut self, module: &Module) -> AIRNode {
        let root = 0u32;
        let imports: Vec<AIRNode> = module
            .imports
            .iter()
            .map(|imp| self.lower_import(imp, root))
            .collect();
        let items: Vec<AIRNode> = module
            .items
            .iter()
            .map(|item| self.lower_item(item, root))
            .collect();
        self.make_node(
            module.span,
            NodeKind::Module {
                path: module.path.clone(),
                annotations: vec![],
                imports,
                items,
            },
            root,
        )
    }

    fn lower_import(&mut self, import: &ImportDecl, scope: u32) -> AIRNode {
        self.make_node(
            import.span,
            NodeKind::ImportDecl {
                path: import.path.clone(),
                items: import.items.clone(),
            },
            scope,
        )
    }

    // ── Items ─────────────────────────────────────────────────────────────────

    fn lower_item(&mut self, item: &Item, scope: u32) -> AIRNode {
        match item {
            Item::Fn(d) => self.lower_fn(d, scope),
            Item::Record(d) => self.lower_record(d, scope),
            Item::Enum(d) => self.lower_enum(d, scope),
            Item::Class(d) => self.lower_class(d, scope),
            Item::Trait(d) | Item::PlatformTrait(d) => self.lower_trait(d, scope),
            Item::Impl(d) => self.lower_impl(d, scope),
            Item::Effect(d) => self.lower_effect(d, scope),
            Item::TypeAlias(d) => self.lower_type_alias(d, scope),
            Item::Const(d) => self.lower_const(d, scope),
            Item::ModuleHandle(d) => self.lower_module_handle(d, scope),
            Item::PropertyTest(d) => self.lower_property_test(d, scope),
            Item::Error { span, .. } => self.make_node(*span, NodeKind::Error, scope),
        }
    }

    fn lower_fn(&mut self, decl: &FnDecl, parent_scope: u32) -> AIRNode {
        let fn_scope = self.alloc_scope();
        let params: Vec<AIRNode> = decl
            .params
            .iter()
            .map(|p| self.lower_param(p, fn_scope))
            .collect();
        let return_type = decl
            .return_type
            .as_ref()
            .map(|ty| Box::new(self.lower_type(ty, fn_scope)));
        let body = Box::new(if let Some(ref b) = decl.body {
            self.lower_block(b, fn_scope)
        } else {
            // Bodyless function (e.g. required trait method) — empty block.
            self.make_node(
                decl.span,
                NodeKind::Block {
                    stmts: vec![],
                    tail: None,
                },
                fn_scope,
            )
        });
        self.make_node(
            decl.span,
            NodeKind::FnDecl {
                annotations: decl.annotations.clone(),
                visibility: decl.visibility,
                is_async: decl.is_async,
                name: decl.name.clone(),
                generic_params: decl.generic_params.clone(),
                params,
                return_type,
                effect_clause: decl.effect_clause.clone(),
                where_clause: decl.where_clause.clone(),
                body,
            },
            parent_scope,
        )
    }

    fn lower_param(&mut self, param: &Param, scope: u32) -> AIRNode {
        let pattern = Box::new(self.lower_pattern(&param.pattern, scope));
        let ty = param
            .ty
            .as_ref()
            .map(|t| Box::new(self.lower_type(t, scope)));
        let default = param
            .default
            .as_ref()
            .map(|d| Box::new(self.lower_expr(d, scope)));
        self.make_node(
            param.span,
            NodeKind::Param {
                pattern,
                ty,
                default,
            },
            scope,
        )
    }

    fn lower_record(&mut self, decl: &RecordDecl, scope: u32) -> AIRNode {
        self.make_node(
            decl.span,
            NodeKind::RecordDecl {
                annotations: decl.annotations.clone(),
                visibility: decl.visibility,
                name: decl.name.clone(),
                generic_params: decl.generic_params.clone(),
                fields: decl.fields.clone(),
            },
            scope,
        )
    }

    fn lower_enum(&mut self, decl: &EnumDecl, scope: u32) -> AIRNode {
        let variants = decl
            .variants
            .iter()
            .map(|v| self.lower_enum_variant(v, scope))
            .collect();
        self.make_node(
            decl.span,
            NodeKind::EnumDecl {
                annotations: decl.annotations.clone(),
                visibility: decl.visibility,
                name: decl.name.clone(),
                generic_params: decl.generic_params.clone(),
                variants,
            },
            scope,
        )
    }

    fn lower_enum_variant(&mut self, variant: &EnumVariant, scope: u32) -> AIRNode {
        let (span, name, payload) = match variant {
            EnumVariant::Unit { span, name, .. } => (*span, name.clone(), EnumVariantPayload::Unit),
            EnumVariant::Struct {
                span, name, fields, ..
            } => (
                *span,
                name.clone(),
                EnumVariantPayload::Struct(fields.clone()),
            ),
            EnumVariant::Tuple {
                span, name, tys, ..
            } => {
                let type_nodes = tys.iter().map(|ty| self.lower_type(ty, scope)).collect();
                (*span, name.clone(), EnumVariantPayload::Tuple(type_nodes))
            }
        };
        self.make_node(span, NodeKind::EnumVariant { name, payload }, scope)
    }

    fn lower_class(&mut self, decl: &bock_ast::ClassDecl, scope: u32) -> AIRNode {
        let class_scope = self.alloc_scope();
        let methods = decl
            .methods
            .iter()
            .map(|m| self.lower_fn(m, class_scope))
            .collect();
        self.make_node(
            decl.span,
            NodeKind::ClassDecl {
                annotations: decl.annotations.clone(),
                visibility: decl.visibility,
                name: decl.name.clone(),
                generic_params: decl.generic_params.clone(),
                base: decl.base.clone(),
                traits: decl.traits.clone(),
                fields: decl.fields.clone(),
                methods,
            },
            scope,
        )
    }

    fn lower_trait(&mut self, decl: &TraitDecl, scope: u32) -> AIRNode {
        let trait_scope = self.alloc_scope();
        let methods = decl
            .methods
            .iter()
            .map(|m| self.lower_fn(m, trait_scope))
            .collect();
        self.make_node(
            decl.span,
            NodeKind::TraitDecl {
                annotations: decl.annotations.clone(),
                visibility: decl.visibility,
                is_platform: decl.is_platform,
                name: decl.name.clone(),
                generic_params: decl.generic_params.clone(),
                associated_types: decl.associated_types.clone(),
                methods,
            },
            scope,
        )
    }

    fn lower_impl(&mut self, decl: &ImplBlock, scope: u32) -> AIRNode {
        let impl_scope = self.alloc_scope();
        let target = Box::new(self.lower_type(&decl.target, impl_scope));
        let methods = decl
            .methods
            .iter()
            .map(|m| self.lower_fn(m, impl_scope))
            .collect();
        self.make_node(
            decl.span,
            NodeKind::ImplBlock {
                annotations: decl.annotations.clone(),
                generic_params: decl.generic_params.clone(),
                trait_path: decl.trait_path.clone(),
                target,
                where_clause: decl.where_clause.clone(),
                methods,
            },
            scope,
        )
    }

    fn lower_effect(&mut self, decl: &EffectDecl, scope: u32) -> AIRNode {
        let effect_scope = self.alloc_scope();
        let operations = decl
            .operations
            .iter()
            .map(|op| self.lower_fn(op, effect_scope))
            .collect();
        self.make_node(
            decl.span,
            NodeKind::EffectDecl {
                annotations: decl.annotations.clone(),
                visibility: decl.visibility,
                name: decl.name.clone(),
                generic_params: decl.generic_params.clone(),
                components: decl.components.clone(),
                operations,
            },
            scope,
        )
    }

    fn lower_type_alias(&mut self, decl: &TypeAliasDecl, scope: u32) -> AIRNode {
        let ty = Box::new(self.lower_type(&decl.ty, scope));
        self.make_node(
            decl.span,
            NodeKind::TypeAlias {
                annotations: decl.annotations.clone(),
                visibility: decl.visibility,
                name: decl.name.clone(),
                generic_params: decl.generic_params.clone(),
                ty,
                where_clause: decl.where_clause.clone(),
            },
            scope,
        )
    }

    fn lower_const(&mut self, decl: &ConstDecl, scope: u32) -> AIRNode {
        let ty = Box::new(self.lower_type(&decl.ty, scope));
        let value = Box::new(self.lower_expr(&decl.value, scope));
        self.make_node(
            decl.span,
            NodeKind::ConstDecl {
                annotations: decl.annotations.clone(),
                visibility: decl.visibility,
                name: decl.name.clone(),
                ty,
                value,
            },
            scope,
        )
    }

    fn lower_module_handle(&mut self, decl: &ModuleHandleDecl, scope: u32) -> AIRNode {
        let handler = Box::new(self.lower_expr(&decl.handler, scope));
        self.make_node(
            decl.span,
            NodeKind::ModuleHandle {
                effect: decl.effect.clone(),
                handler,
            },
            scope,
        )
    }

    fn lower_property_test(&mut self, decl: &PropertyTestDecl, scope: u32) -> AIRNode {
        let test_scope = self.alloc_scope();
        let body = Box::new(self.lower_block(&decl.body, test_scope));
        self.make_node(
            decl.span,
            NodeKind::PropertyTest {
                name: decl.name.clone(),
                bindings: decl.bindings.clone(),
                body,
            },
            scope,
        )
    }

    // ── Type expressions ──────────────────────────────────────────────────────

    fn lower_type(&mut self, ty: &TypeExpr, scope: u32) -> AIRNode {
        match ty {
            TypeExpr::Named {
                span, path, args, ..
            } => {
                let args = args.iter().map(|a| self.lower_type(a, scope)).collect();
                self.make_node(
                    *span,
                    NodeKind::TypeNamed {
                        path: path.clone(),
                        args,
                    },
                    scope,
                )
            }
            TypeExpr::Tuple { span, elems, .. } => {
                let elems = elems.iter().map(|e| self.lower_type(e, scope)).collect();
                self.make_node(*span, NodeKind::TypeTuple { elems }, scope)
            }
            TypeExpr::Function {
                span,
                params,
                ret,
                effects,
                ..
            } => {
                let params = params.iter().map(|p| self.lower_type(p, scope)).collect();
                let ret = Box::new(self.lower_type(ret, scope));
                self.make_node(
                    *span,
                    NodeKind::TypeFunction {
                        params,
                        ret,
                        effects: effects.clone(),
                    },
                    scope,
                )
            }
            TypeExpr::Optional { span, inner, .. } => {
                let inner = Box::new(self.lower_type(inner, scope));
                self.make_node(*span, NodeKind::TypeOptional { inner }, scope)
            }
            TypeExpr::SelfType { span, .. } => self.make_node(*span, NodeKind::TypeSelf, scope),
        }
    }

    // ── Patterns ──────────────────────────────────────────────────────────────

    fn lower_pattern(&mut self, pat: &Pattern, scope: u32) -> AIRNode {
        match pat {
            Pattern::Wildcard { span, .. } => self.make_node(*span, NodeKind::WildcardPat, scope),
            Pattern::Bind { span, name, .. } => self.make_node(
                *span,
                NodeKind::BindPat {
                    name: name.clone(),
                    is_mut: false,
                },
                scope,
            ),
            Pattern::MutBind { span, name, .. } => self.make_node(
                *span,
                NodeKind::BindPat {
                    name: name.clone(),
                    is_mut: true,
                },
                scope,
            ),
            Pattern::Literal { span, lit, .. } => {
                self.make_node(*span, NodeKind::LiteralPat { lit: lit.clone() }, scope)
            }
            Pattern::Constructor {
                span, path, fields, ..
            } => {
                let fields = fields
                    .iter()
                    .map(|f| self.lower_pattern(f, scope))
                    .collect();
                self.make_node(
                    *span,
                    NodeKind::ConstructorPat {
                        path: path.clone(),
                        fields,
                    },
                    scope,
                )
            }
            Pattern::Record {
                span,
                path,
                fields,
                rest,
                ..
            } => {
                let fields = fields
                    .iter()
                    .map(|f| self.lower_record_pat_field(f, scope))
                    .collect();
                self.make_node(
                    *span,
                    NodeKind::RecordPat {
                        path: path.clone(),
                        fields,
                        rest: *rest,
                    },
                    scope,
                )
            }
            Pattern::Tuple { span, elems, .. } => {
                let elems = elems.iter().map(|e| self.lower_pattern(e, scope)).collect();
                self.make_node(*span, NodeKind::TuplePat { elems }, scope)
            }
            Pattern::List {
                span, elems, rest, ..
            } => {
                let elems = elems.iter().map(|e| self.lower_pattern(e, scope)).collect();
                let rest = rest
                    .as_ref()
                    .map(|r| Box::new(self.lower_pattern(r, scope)));
                self.make_node(*span, NodeKind::ListPat { elems, rest }, scope)
            }
            Pattern::Or {
                span, alternatives, ..
            } => {
                let alternatives = alternatives
                    .iter()
                    .map(|a| self.lower_pattern(a, scope))
                    .collect();
                self.make_node(*span, NodeKind::OrPat { alternatives }, scope)
            }
            Pattern::Range {
                span,
                lo,
                hi,
                inclusive,
                ..
            } => {
                let lo = Box::new(self.lower_pattern(lo, scope));
                let hi = Box::new(self.lower_pattern(hi, scope));
                self.make_node(
                    *span,
                    NodeKind::RangePat {
                        lo,
                        hi,
                        inclusive: *inclusive,
                    },
                    scope,
                )
            }
            Pattern::Rest { span, .. } => self.make_node(*span, NodeKind::RestPat, scope),
        }
    }

    fn lower_record_pat_field(
        &mut self,
        field: &RecordPatternField,
        scope: u32,
    ) -> AirRecordPatternField {
        AirRecordPatternField {
            name: field.name.clone(),
            pattern: field
                .pattern
                .as_ref()
                .map(|p| Box::new(self.lower_pattern(p, scope))),
        }
    }

    // ── Block & Statements ────────────────────────────────────────────────────

    fn lower_block(&mut self, block: &Block, _parent_scope: u32) -> AIRNode {
        let block_scope = self.alloc_scope();
        let stmts: Vec<AIRNode> = block
            .stmts
            .iter()
            .flat_map(|s| self.lower_stmt(s, block_scope))
            .collect();
        let tail = block
            .tail
            .as_ref()
            .map(|e| Box::new(self.lower_expr(e, block_scope)));
        self.make_node(block.span, NodeKind::Block { stmts, tail }, block_scope)
    }

    fn lower_stmt(&mut self, stmt: &Stmt, scope: u32) -> Vec<AIRNode> {
        match stmt {
            Stmt::Let(s) => vec![self.lower_let(s, scope)],
            Stmt::Expr(e) => vec![self.lower_expr(e, scope)],
            Stmt::For(f) => vec![self.lower_for(f, scope)],
            Stmt::While(w) => vec![self.lower_while(w, scope)],
            Stmt::Loop(l) => vec![self.lower_loop(l, scope)],
            Stmt::Guard(g) => vec![self.lower_guard(g, scope)],
            Stmt::Handling(h) => vec![self.lower_handling(h, scope)],
            Stmt::Empty => vec![],
        }
    }

    fn lower_let(&mut self, let_stmt: &LetStmt, scope: u32) -> AIRNode {
        let is_mut = matches!(&let_stmt.pattern, Pattern::MutBind { .. });
        let pattern = Box::new(self.lower_pattern(&let_stmt.pattern, scope));
        let ty = let_stmt
            .ty
            .as_ref()
            .map(|t| Box::new(self.lower_type(t, scope)));
        let value = Box::new(self.lower_expr(&let_stmt.value, scope));
        self.make_node(
            let_stmt.span,
            NodeKind::LetBinding {
                is_mut,
                pattern,
                ty,
                value,
            },
            scope,
        )
    }

    /// Lower `for pat in iter { body }` to a `NodeKind::For` AIR node.
    ///
    /// The type checker and interpreter handle `For` nodes directly,
    /// extracting the element type from the iterable and binding the
    /// loop variable.
    fn lower_for(&mut self, for_loop: &ForLoop, parent_scope: u32) -> AIRNode {
        let span = for_loop.span;
        let for_scope = self.alloc_scope();

        let pattern = Box::new(self.lower_pattern(&for_loop.pattern, for_scope));
        let iterable = Box::new(self.lower_expr(&for_loop.iterable, for_scope));
        let body = Box::new(self.lower_block(&for_loop.body, for_scope));

        self.make_node(
            span,
            NodeKind::For {
                pattern,
                iterable,
                body,
            },
            parent_scope,
        )
    }

    fn lower_while(&mut self, w: &WhileLoop, scope: u32) -> AIRNode {
        let condition = Box::new(self.lower_expr(&w.condition, scope));
        let body = Box::new(self.lower_block(&w.body, scope));
        self.make_node(w.span, NodeKind::While { condition, body }, scope)
    }

    fn lower_loop(&mut self, l: &LoopStmt, scope: u32) -> AIRNode {
        let body = Box::new(self.lower_block(&l.body, scope));
        self.make_node(l.span, NodeKind::Loop { body }, scope)
    }

    fn lower_guard(&mut self, g: &GuardStmt, scope: u32) -> AIRNode {
        let let_pattern = g
            .let_pattern
            .as_ref()
            .map(|pat| Box::new(self.lower_pattern(pat, scope)));
        let condition = Box::new(self.lower_expr(&g.condition, scope));
        let else_block = Box::new(self.lower_block(&g.else_block, scope));
        self.make_node(
            g.span,
            NodeKind::Guard {
                let_pattern,
                condition,
                else_block,
            },
            scope,
        )
    }

    fn lower_handling(&mut self, h: &HandlingBlock, scope: u32) -> AIRNode {
        let handling_scope = self.alloc_scope();
        let handlers = h
            .handlers
            .iter()
            .map(|pair| AirHandlerPair {
                effect: pair.effect.clone(),
                handler: Box::new(self.lower_expr(&pair.handler, scope)),
            })
            .collect();
        let body = Box::new(self.lower_block(&h.body, handling_scope));
        self.make_node(h.span, NodeKind::HandlingBlock { handlers, body }, scope)
    }

    // ── Expressions ───────────────────────────────────────────────────────────

    fn lower_expr(&mut self, expr: &Expr, scope: u32) -> AIRNode {
        match expr {
            Expr::Literal { span, lit, .. } => {
                self.make_node(*span, NodeKind::Literal { lit: lit.clone() }, scope)
            }

            Expr::Identifier { span, name, id } => {
                let kind = NodeKind::Identifier { name: name.clone() };
                let mut node = self.make_node(*span, kind, scope);
                // Attach resolution from the name-resolution pass.
                if let Some(resolved) = self.symbols.resolutions.get(id) {
                    node.metadata.insert(
                        "resolved_def_id".to_string(),
                        Value::Int(resolved.def_id as i64),
                    );
                    node.metadata.insert(
                        "resolved_kind".to_string(),
                        Value::String(format!("{:?}", resolved.kind)),
                    );
                }
                node
            }

            Expr::Binary {
                span,
                op,
                left,
                right,
                ..
            } => {
                let left = Box::new(self.lower_expr(left, scope));
                let right = Box::new(self.lower_expr(right, scope));
                self.make_node(
                    *span,
                    NodeKind::BinaryOp {
                        op: *op,
                        left,
                        right,
                    },
                    scope,
                )
            }

            Expr::Unary {
                span, op, operand, ..
            } => {
                let operand = Box::new(self.lower_expr(operand, scope));
                self.make_node(*span, NodeKind::UnaryOp { op: *op, operand }, scope)
            }

            Expr::Assign {
                span,
                op,
                target,
                value,
                ..
            } => {
                let target = Box::new(self.lower_expr(target, scope));
                let value = Box::new(self.lower_expr(value, scope));
                self.make_node(
                    *span,
                    NodeKind::Assign {
                        op: *op,
                        target,
                        value,
                    },
                    scope,
                )
            }

            Expr::Call {
                span,
                callee,
                args,
                type_args,
                ..
            } => {
                let callee = Box::new(self.lower_expr(callee, scope));
                let args = self.lower_args(args, scope);
                let type_args = type_args
                    .iter()
                    .map(|t| self.lower_type(t, scope))
                    .collect();
                self.make_node(
                    *span,
                    NodeKind::Call {
                        callee,
                        args,
                        type_args,
                    },
                    scope,
                )
            }

            // Desugar: `obj.method(args)` → `Call { callee: obj.method, args: [self=obj, ...args] }`
            // Special case: `Type.method(args)` (associated function) → no self prepended.
            Expr::MethodCall {
                span,
                receiver,
                method,
                args,
                ..
            } => {
                let is_associated = self.is_type_receiver(receiver);
                let receiver_air = self.lower_expr(receiver, scope);
                let lowered_args = self.lower_args(args, scope);
                if is_associated {
                    self.make_associated_fn_call(
                        receiver_air,
                        method.clone(),
                        lowered_args,
                        *span,
                        scope,
                    )
                } else {
                    self.make_desugared_method_call(
                        receiver_air,
                        method.clone(),
                        lowered_args,
                        *span,
                        scope,
                    )
                }
            }

            Expr::FieldAccess {
                span,
                object,
                field,
                ..
            } => {
                let object = Box::new(self.lower_expr(object, scope));
                self.make_node(
                    *span,
                    NodeKind::FieldAccess {
                        object,
                        field: field.clone(),
                    },
                    scope,
                )
            }

            Expr::Index {
                span,
                object,
                index,
                ..
            } => {
                let object = Box::new(self.lower_expr(object, scope));
                let index = Box::new(self.lower_expr(index, scope));
                self.make_node(*span, NodeKind::Index { object, index }, scope)
            }

            Expr::Try { span, expr, .. } => {
                let expr = Box::new(self.lower_expr(expr, scope));
                self.make_node(*span, NodeKind::Propagate { expr }, scope)
            }

            Expr::Lambda {
                span, params, body, ..
            } => {
                let lambda_scope = self.alloc_scope();
                let params = params
                    .iter()
                    .map(|p| self.lower_param(p, lambda_scope))
                    .collect();
                let body = Box::new(self.lower_expr(body, lambda_scope));
                self.make_node(*span, NodeKind::Lambda { params, body }, scope)
            }

            // Desugar pipe: `left |> right`
            Expr::Pipe {
                span, left, right, ..
            } => self.lower_pipe(left, right, *span, scope),

            // Desugar compose: `f >> g` → `(__compose_x) => g(f(__compose_x))`
            Expr::Compose {
                span, left, right, ..
            } => self.lower_compose(left, right, *span, scope),

            Expr::If {
                span,
                let_pattern,
                condition,
                then_block,
                else_block,
                ..
            } => {
                if let Some(pat) = let_pattern {
                    // Desugar `if let` → match
                    self.lower_if_let(
                        pat,
                        condition,
                        then_block,
                        else_block.as_deref(),
                        *span,
                        scope,
                    )
                } else {
                    self.lower_if(condition, then_block, else_block.as_deref(), *span, scope)
                }
            }

            Expr::Match {
                span,
                scrutinee,
                arms,
                ..
            } => {
                let scrutinee = Box::new(self.lower_expr(scrutinee, scope));
                let arms = arms
                    .iter()
                    .map(|arm| self.lower_match_arm(arm, scope))
                    .collect();
                self.make_node(*span, NodeKind::Match { scrutinee, arms }, scope)
            }

            Expr::Loop { span, body, .. } => {
                let body = Box::new(self.lower_block(body, scope));
                self.make_node(*span, NodeKind::Loop { body }, scope)
            }

            Expr::Block { block, .. } => self.lower_block(block, scope),

            Expr::RecordConstruct {
                span,
                path,
                fields,
                spread,
                ..
            } => {
                let fields = fields
                    .iter()
                    .map(|f| AirRecordField {
                        name: f.name.clone(),
                        value: f
                            .value
                            .as_ref()
                            .map(|v| Box::new(self.lower_expr(v, scope))),
                    })
                    .collect();
                let spread = spread
                    .as_ref()
                    .map(|s| Box::new(self.lower_expr(&s.expr, scope)));
                self.make_node(
                    *span,
                    NodeKind::RecordConstruct {
                        path: path.clone(),
                        fields,
                        spread,
                    },
                    scope,
                )
            }

            Expr::ListLiteral { span, elems, .. } => {
                let elems = elems.iter().map(|e| self.lower_expr(e, scope)).collect();
                self.make_node(*span, NodeKind::ListLiteral { elems }, scope)
            }

            Expr::MapLiteral { span, entries, .. } => {
                let entries = entries
                    .iter()
                    .map(|(k, v)| AirMapEntry {
                        key: self.lower_expr(k, scope),
                        value: self.lower_expr(v, scope),
                    })
                    .collect();
                self.make_node(*span, NodeKind::MapLiteral { entries }, scope)
            }

            Expr::SetLiteral { span, elems, .. } => {
                let elems = elems.iter().map(|e| self.lower_expr(e, scope)).collect();
                self.make_node(*span, NodeKind::SetLiteral { elems }, scope)
            }

            Expr::TupleLiteral { span, elems, .. } => {
                let elems = elems.iter().map(|e| self.lower_expr(e, scope)).collect();
                self.make_node(*span, NodeKind::TupleLiteral { elems }, scope)
            }

            Expr::Range {
                span,
                lo,
                hi,
                inclusive,
                ..
            } => {
                let lo = Box::new(self.lower_expr(lo, scope));
                let hi = Box::new(self.lower_expr(hi, scope));
                self.make_node(
                    *span,
                    NodeKind::Range {
                        lo,
                        hi,
                        inclusive: *inclusive,
                    },
                    scope,
                )
            }

            Expr::Await { span, expr, .. } => {
                let expr = Box::new(self.lower_expr(expr, scope));
                self.make_node(*span, NodeKind::Await { expr }, scope)
            }

            Expr::Return { span, value, .. } => {
                let value = value.as_ref().map(|v| Box::new(self.lower_expr(v, scope)));
                self.make_node(*span, NodeKind::Return { value }, scope)
            }

            Expr::Break { span, value, .. } => {
                let value = value.as_ref().map(|v| Box::new(self.lower_expr(v, scope)));
                self.make_node(*span, NodeKind::Break { value }, scope)
            }

            Expr::Continue { span, .. } => self.make_node(*span, NodeKind::Continue, scope),

            Expr::Unreachable { span, .. } => self.make_node(*span, NodeKind::Unreachable, scope),

            Expr::Interpolation { span, parts, .. } => {
                let parts = parts
                    .iter()
                    .map(|p| match p {
                        InterpolationPart::Literal(s) => AirInterpolationPart::Literal(s.clone()),
                        InterpolationPart::Expr(e) => {
                            AirInterpolationPart::Expr(Box::new(self.lower_expr(e, scope)))
                        }
                    })
                    .collect();
                self.make_node(*span, NodeKind::Interpolation { parts }, scope)
            }

            Expr::Placeholder { span, .. } => self.make_node(*span, NodeKind::Placeholder, scope),

            Expr::Is {
                span,
                expr,
                type_expr,
                ..
            } => {
                // Lower `expr is Type` into BinaryOp::Is with the type name
                // as a string literal on the right, so the interpreter can do
                // a runtime type-tag comparison.
                let left = self.lower_expr(expr, scope);
                let type_name = Self::type_expr_to_name(type_expr);
                let right = self.make_node(
                    *span,
                    NodeKind::Literal {
                        lit: bock_ast::Literal::String(type_name),
                    },
                    scope,
                );
                self.make_node(
                    *span,
                    NodeKind::BinaryOp {
                        op: bock_ast::BinOp::Is,
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    scope,
                )
            }
        }
    }

    /// Extract the base type name from a `TypeExpr` for runtime `is` checks.
    fn type_expr_to_name(te: &bock_ast::TypeExpr) -> String {
        match te {
            bock_ast::TypeExpr::Named { path, .. } => path
                .segments
                .last()
                .map(|s| s.name.as_str())
                .unwrap_or("Unknown")
                .to_string(),
            bock_ast::TypeExpr::Tuple { .. } => "Tuple".to_string(),
            bock_ast::TypeExpr::Function { .. } => "Function".to_string(),
            bock_ast::TypeExpr::Optional { .. } => "Optional".to_string(),
            bock_ast::TypeExpr::SelfType { .. } => "Self".to_string(),
        }
    }

    fn lower_args(&mut self, args: &[bock_ast::Arg], scope: u32) -> Vec<AirArg> {
        args.iter()
            .map(|a| AirArg {
                label: a.label.clone(),
                value: self.lower_expr(&a.value, scope),
            })
            .collect()
    }

    fn lower_match_arm(&mut self, arm: &MatchArm, _scope: u32) -> AIRNode {
        let arm_scope = self.alloc_scope();
        let pattern = Box::new(self.lower_pattern(&arm.pattern, arm_scope));
        let guard = arm
            .guard
            .as_ref()
            .map(|g| Box::new(self.lower_expr(g, arm_scope)));
        let body = Box::new(self.lower_expr(&arm.body, arm_scope));
        self.make_node(
            arm.span,
            NodeKind::MatchArm {
                pattern,
                guard,
                body,
            },
            arm_scope,
        )
    }

    // ── Desugaring helpers ────────────────────────────────────────────────────

    /// Build a desugared method call: `receiver.method(args)` →
    /// `Call { callee: FieldAccess(receiver, method), args: [self=receiver, ...args] }`.
    ///
    /// The receiver AIR node is cloned for use as both the field-access object and
    /// the self argument.
    fn make_desugared_method_call(
        &mut self,
        receiver: AIRNode,
        method: Ident,
        args: Vec<AirArg>,
        span: Span,
        scope: u32,
    ) -> AIRNode {
        let field_access = self.make_node(
            span,
            NodeKind::FieldAccess {
                object: Box::new(receiver.clone()),
                field: method,
            },
            scope,
        );
        let self_arg = AirArg {
            label: None,
            value: receiver,
        };
        let mut all_args = vec![self_arg];
        all_args.extend(args);
        self.make_node(
            span,
            NodeKind::Call {
                callee: Box::new(field_access),
                args: all_args,
                type_args: vec![],
            },
            scope,
        )
    }

    /// Returns `true` if `expr` is an identifier that the resolver classified
    /// as a type name (`NameKind::Type` or `NameKind::Builtin` with a
    /// PascalCase name).  Used to detect `Type.method()` associated-function
    /// call syntax.
    fn is_type_receiver(&self, expr: &Expr) -> bool {
        if let Expr::Identifier { id, .. } = expr {
            if let Some(resolved) = self.symbols.resolutions.get(id) {
                return resolved.kind == NameKind::Type || resolved.kind == NameKind::Builtin;
            }
        }
        false
    }

    /// Build an associated function call: `Type.method(args)` →
    /// `Call { callee: FieldAccess(Type, method), args }`.
    ///
    /// Unlike [`make_desugared_method_call`](Self::make_desugared_method_call),
    /// the receiver is NOT prepended as `self`.
    fn make_associated_fn_call(
        &mut self,
        receiver: AIRNode,
        method: Ident,
        args: Vec<AirArg>,
        span: Span,
        scope: u32,
    ) -> AIRNode {
        let field_access = self.make_node(
            span,
            NodeKind::FieldAccess {
                object: Box::new(receiver),
                field: method,
            },
            scope,
        );
        self.make_node(
            span,
            NodeKind::Call {
                callee: Box::new(field_access),
                args,
                type_args: vec![],
            },
            scope,
        )
    }

    /// Desugar pipe: `left |> right`.
    ///
    /// | Right-hand side            | Result                      |
    /// |----------------------------|-----------------------------|
    /// | Bare (not a call)          | `right(left)`               |
    /// | Call, no `_` placeholder   | `right(left, ...args)`      |
    /// | Call, with `_` placeholder | `right(args with _ → left)` |
    fn lower_pipe(&mut self, left: &Expr, right: &Expr, span: Span, scope: u32) -> AIRNode {
        let left_air = self.lower_expr(left, scope);

        match right {
            Expr::Call {
                span: call_span,
                callee,
                args,
                type_args,
                ..
            } => {
                let callee_air = Box::new(self.lower_expr(callee, scope));
                let type_args_air: Vec<AIRNode> = type_args
                    .iter()
                    .map(|t| self.lower_type(t, scope))
                    .collect();

                let has_placeholder = args
                    .iter()
                    .any(|a| matches!(&a.value, Expr::Placeholder { .. }));

                if has_placeholder {
                    // Case: explicit placeholder — replace `_` with `left`.
                    let mut new_args = Vec::with_capacity(args.len());
                    for a in args {
                        if matches!(&a.value, Expr::Placeholder { .. }) {
                            new_args.push(AirArg {
                                label: a.label.clone(),
                                value: left_air.clone(),
                            });
                        } else {
                            new_args.push(AirArg {
                                label: a.label.clone(),
                                value: self.lower_expr(&a.value, scope),
                            });
                        }
                    }
                    self.make_node(
                        *call_span,
                        NodeKind::Call {
                            callee: callee_air,
                            args: new_args,
                            type_args: type_args_air,
                        },
                        scope,
                    )
                } else {
                    // Case: implicit first — prepend left.
                    let mut new_args = vec![AirArg {
                        label: None,
                        value: left_air,
                    }];
                    new_args.extend(self.lower_args(args, scope));
                    self.make_node(
                        *call_span,
                        NodeKind::Call {
                            callee: callee_air,
                            args: new_args,
                            type_args: type_args_air,
                        },
                        scope,
                    )
                }
            }

            // Case: bare function — `right(left)`.
            _ => {
                let callee_air = Box::new(self.lower_expr(right, scope));
                self.make_node(
                    span,
                    NodeKind::Call {
                        callee: callee_air,
                        args: vec![AirArg {
                            label: None,
                            value: left_air,
                        }],
                        type_args: vec![],
                    },
                    scope,
                )
            }
        }
    }

    /// Desugar `f >> g` → `(__compose_x) => g(f(__compose_x))`.
    fn lower_compose(&mut self, f: &Expr, g: &Expr, span: Span, scope: u32) -> AIRNode {
        let lambda_scope = self.alloc_scope();
        let param_name = synth_ident("__compose_x", span);

        // Parameter pattern and node.
        let param_pat = self.make_node(
            span,
            NodeKind::BindPat {
                name: param_name.clone(),
                is_mut: false,
            },
            lambda_scope,
        );
        let param_node = self.make_node(
            span,
            NodeKind::Param {
                pattern: Box::new(param_pat),
                ty: None,
                default: None,
            },
            lambda_scope,
        );

        // `__compose_x` reference.
        let x_ref = self.make_node(
            span,
            NodeKind::Identifier { name: param_name },
            lambda_scope,
        );

        // `f(__compose_x)`.
        let f_air = Box::new(self.lower_expr(f, lambda_scope));
        let f_call = self.make_node(
            span,
            NodeKind::Call {
                callee: f_air,
                args: vec![AirArg {
                    label: None,
                    value: x_ref,
                }],
                type_args: vec![],
            },
            lambda_scope,
        );

        // `g(f(__compose_x))`.
        let g_air = Box::new(self.lower_expr(g, lambda_scope));
        let g_call = self.make_node(
            span,
            NodeKind::Call {
                callee: g_air,
                args: vec![AirArg {
                    label: None,
                    value: f_call,
                }],
                type_args: vec![],
            },
            lambda_scope,
        );

        self.make_node(
            span,
            NodeKind::Lambda {
                params: vec![param_node],
                body: Box::new(g_call),
            },
            scope,
        )
    }

    /// Desugar `if let pat = cond { then } [else { ... }]`
    /// → `match cond { pat => then, _ => else_or_empty }`.
    fn lower_if_let(
        &mut self,
        pat: &Pattern,
        cond: &Expr,
        then_block: &Block,
        else_block: Option<&Expr>,
        span: Span,
        scope: u32,
    ) -> AIRNode {
        let scrutinee = Box::new(self.lower_expr(cond, scope));

        // `pat => then`
        let pat_scope = self.alloc_scope();
        let lower_pat = self.lower_pattern(pat, pat_scope);
        let body_node = self.lower_block(then_block, pat_scope);
        let then_arm = self.make_node(
            span,
            NodeKind::MatchArm {
                pattern: Box::new(lower_pat),
                guard: None,
                body: Box::new(body_node),
            },
            pat_scope,
        );

        // `_ => else_or_empty`
        let else_scope = self.alloc_scope();
        let else_body = if let Some(else_expr) = else_block {
            self.lower_expr(else_expr, else_scope)
        } else {
            self.make_node(
                span,
                NodeKind::Block {
                    stmts: vec![],
                    tail: None,
                },
                else_scope,
            )
        };
        let wildcard = self.make_node(span, NodeKind::WildcardPat, else_scope);
        let else_arm = self.make_node(
            span,
            NodeKind::MatchArm {
                pattern: Box::new(wildcard),
                guard: None,
                body: Box::new(else_body),
            },
            else_scope,
        );

        self.make_node(
            span,
            NodeKind::Match {
                scrutinee,
                arms: vec![then_arm, else_arm],
            },
            scope,
        )
    }

    /// Lower a plain (non-`if-let`) `if` expression.
    fn lower_if(
        &mut self,
        cond: &Expr,
        then_block: &Block,
        else_block: Option<&Expr>,
        span: Span,
        scope: u32,
    ) -> AIRNode {
        let condition = Box::new(self.lower_expr(cond, scope));
        let then_node = Box::new(self.lower_block(then_block, scope));
        let else_node = else_block.map(|e| Box::new(self.lower_expr(e, scope)));
        self.make_node(
            span,
            NodeKind::If {
                let_pattern: None,
                condition,
                then_block: then_node,
                else_block: else_node,
            },
            scope,
        )
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_ast::{
        Arg, Block, Expr, FnDecl, Ident, Item, LetStmt, Literal, Module, Pattern, Stmt, Visibility,
    };
    use bock_errors::FileId;

    use crate::{
        node::NodeIdGen,
        resolve::{resolve_names, SymbolTable},
    };

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

    fn empty_block(id: u32) -> Block {
        Block {
            id,
            span: span(),
            stmts: vec![],
            tail: None,
        }
    }

    fn simple_fn(id: u32, name: &str, body: Block) -> Item {
        Item::Fn(FnDecl {
            id,
            span: span(),
            annotations: vec![],
            visibility: Visibility::Private,
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

    fn do_lower(module: &Module) -> (AIRNode, SymbolTable) {
        let id_gen = NodeIdGen::new();
        let mut symbols = SymbolTable::new();
        let _diag = resolve_names(module, &mut symbols);
        let air = lower_module(module, &id_gen, &symbols);
        (air, symbols)
    }

    // ── Scope annotation ───────────────────────────────────────────────────────

    #[test]
    fn module_node_has_scope_zero() {
        let module = make_module(vec![]);
        let (air, _) = do_lower(&module);
        assert!(matches!(air.kind, NodeKind::Module { .. }));
        assert_eq!(air.metadata.get("scope_id"), Some(&Value::Int(0)));
    }

    #[test]
    fn all_nodes_have_scope_metadata() {
        let body = Block {
            id: 2,
            span: span(),
            stmts: vec![Stmt::Let(LetStmt {
                id: 3,
                span: span(),
                pattern: Pattern::Bind {
                    id: 4,
                    span: span(),
                    name: ident("x"),
                },
                ty: None,
                value: Expr::Literal {
                    id: 5,
                    span: span(),
                    lit: Literal::Int("1".into()),
                },
            })],
            tail: None,
        };
        let module = make_module(vec![simple_fn(1, "foo", body)]);
        let (air, _) = do_lower(&module);

        // Walk the tree and ensure every node has a scope_id.
        fn check(node: &AIRNode) {
            assert!(
                node.metadata.contains_key("scope_id"),
                "node {:?} is missing scope_id",
                node.kind
            );
            match &node.kind {
                NodeKind::Module { imports, items, .. } => {
                    imports.iter().for_each(check);
                    items.iter().for_each(check);
                }
                NodeKind::FnDecl { params, body, .. } => {
                    params.iter().for_each(check);
                    check(body);
                }
                NodeKind::Block { stmts, tail } => {
                    stmts.iter().for_each(check);
                    tail.as_deref().map(check);
                }
                NodeKind::LetBinding { pattern, value, .. } => {
                    check(pattern);
                    check(value);
                }
                _ => {}
            }
        }
        check(&air);
    }

    // ── Identifier resolution ──────────────────────────────────────────────────

    #[test]
    fn identifier_gets_resolution_metadata() {
        // fn foo() { let x = 1; x }
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
                lit: Literal::Int("1".into()),
            },
        });
        let ref_expr = Expr::Identifier {
            id: 20,
            span: span(),
            name: ident("x"),
        };
        let body = Block {
            id: 2,
            span: span(),
            stmts: vec![let_stmt],
            tail: Some(Box::new(ref_expr)),
        };
        let module = make_module(vec![simple_fn(1, "foo", body)]);
        let (air, _) = do_lower(&module);

        // Find the tail identifier node.
        let fn_node = match &air.kind {
            NodeKind::Module { items, .. } => &items[0],
            _ => panic!("expected module"),
        };
        let block_node = match &fn_node.kind {
            NodeKind::FnDecl { body, .. } => body.as_ref(),
            _ => panic!("expected fn"),
        };
        // The function body block is inside the fn_scope block.
        let inner_block = match &block_node.kind {
            NodeKind::Block { tail, .. } => {
                // tail should be the identifier reference to x
                tail.as_deref().expect("expected tail")
            }
            _ => panic!("expected block"),
        };
        // The identifier `x` should have resolved_def_id in its metadata.
        assert!(
            inner_block.metadata.contains_key("resolved_def_id"),
            "identifier should have resolved_def_id; got {:?}",
            inner_block.metadata
        );
    }

    // ── Pipe desugaring ────────────────────────────────────────────────────────

    #[test]
    fn pipe_bare_function() {
        // `x |> f` → `f(x)` — Call { callee: f, args: [x] }
        let pipe_expr = Expr::Pipe {
            id: 1,
            span: span(),
            left: Box::new(Expr::Identifier {
                id: 2,
                span: span(),
                name: ident("x"),
            }),
            right: Box::new(Expr::Identifier {
                id: 3,
                span: span(),
                name: ident("f"),
            }),
        };
        let body = Block {
            id: 4,
            span: span(),
            stmts: vec![],
            tail: Some(Box::new(pipe_expr)),
        };
        let module = make_module(vec![simple_fn(10, "test", body)]);
        let (air, _) = do_lower(&module);

        let fn_item = match &air.kind {
            NodeKind::Module { items, .. } => &items[0],
            _ => panic!(),
        };
        let inner_block = match &fn_item.kind {
            NodeKind::FnDecl { body, .. } => body.as_ref(),
            _ => panic!(),
        };
        let tail = match &inner_block.kind {
            NodeKind::Block { tail, .. } => tail.as_deref().expect("tail"),
            _ => panic!(),
        };
        // Should be a Call with one arg.
        match &tail.kind {
            NodeKind::Call { callee, args, .. } => {
                assert!(matches!(callee.kind, NodeKind::Identifier { .. }));
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    #[test]
    fn pipe_implicit_first_arg() {
        // `x |> f(y)` → `f(x, y)` — Call { callee: f, args: [x, y] }
        let pipe_expr = Expr::Pipe {
            id: 1,
            span: span(),
            left: Box::new(Expr::Identifier {
                id: 2,
                span: span(),
                name: ident("x"),
            }),
            right: Box::new(Expr::Call {
                id: 3,
                span: span(),
                callee: Box::new(Expr::Identifier {
                    id: 4,
                    span: span(),
                    name: ident("f"),
                }),
                args: vec![Arg {
                    span: span(),
                    label: None,
                    mutable: false,
                    value: Expr::Identifier {
                        id: 5,
                        span: span(),
                        name: ident("y"),
                    },
                }],
                type_args: vec![],
            }),
        };
        let body = Block {
            id: 6,
            span: span(),
            stmts: vec![],
            tail: Some(Box::new(pipe_expr)),
        };
        let module = make_module(vec![simple_fn(10, "test", body)]);
        let (air, _) = do_lower(&module);

        let call_node = find_tail_expr(&air);
        match &call_node.kind {
            NodeKind::Call { args, .. } => {
                // Should have 2 args: x prepended, then y.
                assert_eq!(args.len(), 2, "expected 2 args, got {}", args.len());
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    #[test]
    fn pipe_placeholder_substitution() {
        // `x |> f(a, _, b)` → `f(a, x, b)` — `_` replaced by x.
        let pipe_expr = Expr::Pipe {
            id: 1,
            span: span(),
            left: Box::new(Expr::Identifier {
                id: 2,
                span: span(),
                name: ident("x"),
            }),
            right: Box::new(Expr::Call {
                id: 3,
                span: span(),
                callee: Box::new(Expr::Identifier {
                    id: 4,
                    span: span(),
                    name: ident("f"),
                }),
                args: vec![
                    Arg {
                        span: span(),
                        label: None,
                        mutable: false,
                        value: Expr::Identifier {
                            id: 5,
                            span: span(),
                            name: ident("a"),
                        },
                    },
                    Arg {
                        span: span(),
                        label: None,
                        mutable: false,
                        value: Expr::Placeholder {
                            id: 6,
                            span: span(),
                        },
                    },
                    Arg {
                        span: span(),
                        label: None,
                        mutable: false,
                        value: Expr::Identifier {
                            id: 7,
                            span: span(),
                            name: ident("b"),
                        },
                    },
                ],
                type_args: vec![],
            }),
        };
        let body = Block {
            id: 8,
            span: span(),
            stmts: vec![],
            tail: Some(Box::new(pipe_expr)),
        };
        let module = make_module(vec![simple_fn(10, "test", body)]);
        let (air, _) = do_lower(&module);

        let call_node = find_tail_expr(&air);
        match &call_node.kind {
            NodeKind::Call { args, .. } => {
                // Should have 3 args: a, x (replacing _), b.
                assert_eq!(args.len(), 3, "expected 3 args");
                // Second arg should be an Identifier named "x" (was the placeholder).
                assert!(
                    matches!(&args[1].value.kind, NodeKind::Identifier { name } if name.name == "x"),
                    "expected x at position 1, got {:?}",
                    args[1].value.kind
                );
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    // ── Compose desugaring ─────────────────────────────────────────────────────

    #[test]
    fn compose_produces_lambda() {
        // `f >> g` → `(__compose_x) => g(f(__compose_x))`
        let compose_expr = Expr::Compose {
            id: 1,
            span: span(),
            left: Box::new(Expr::Identifier {
                id: 2,
                span: span(),
                name: ident("f"),
            }),
            right: Box::new(Expr::Identifier {
                id: 3,
                span: span(),
                name: ident("g"),
            }),
        };
        let body = Block {
            id: 4,
            span: span(),
            stmts: vec![],
            tail: Some(Box::new(compose_expr)),
        };
        let module = make_module(vec![simple_fn(10, "test", body)]);
        let (air, _) = do_lower(&module);

        let tail = find_tail_expr(&air);
        match &tail.kind {
            NodeKind::Lambda { params, body } => {
                assert_eq!(params.len(), 1, "lambda should have one param");
                // body should be a Call to g
                assert!(
                    matches!(&body.kind, NodeKind::Call { .. }),
                    "body should be Call"
                );
            }
            other => panic!("expected Lambda, got {:?}", other),
        }
    }

    // ── For-loop desugaring ────────────────────────────────────────────────────

    #[test]
    fn for_loop_lowered_to_for_node() {
        // `for x in items { ... }` → NodeKind::For { pattern, iterable, body }
        let for_stmt = Stmt::For(bock_ast::ForLoop {
            id: 1,
            span: span(),
            pattern: Pattern::Bind {
                id: 2,
                span: span(),
                name: ident("x"),
            },
            iterable: Expr::Identifier {
                id: 3,
                span: span(),
                name: ident("items"),
            },
            body: empty_block(4),
        });
        let fn_body = Block {
            id: 5,
            span: span(),
            stmts: vec![for_stmt],
            tail: None,
        };
        let module = make_module(vec![simple_fn(10, "test", fn_body)]);
        let (air, _) = do_lower(&module);

        let fn_node = match &air.kind {
            NodeKind::Module { items, .. } => &items[0],
            _ => panic!(),
        };
        let fn_body_block = match &fn_node.kind {
            NodeKind::FnDecl { body, .. } => body.as_ref(),
            _ => panic!(),
        };
        let body_block = match &fn_body_block.kind {
            NodeKind::Block { stmts, .. } => stmts,
            _ => panic!("expected Block"),
        };
        // The for-loop should produce a For node directly.
        let for_node = &body_block[0];
        match &for_node.kind {
            NodeKind::For {
                pattern,
                iterable,
                body,
            } => {
                assert!(
                    matches!(&pattern.kind, NodeKind::BindPat { .. }),
                    "expected BindPat, got {:?}",
                    pattern.kind
                );
                assert!(
                    matches!(&iterable.kind, NodeKind::Identifier { .. }),
                    "expected Identifier, got {:?}",
                    iterable.kind
                );
                assert!(
                    matches!(&body.kind, NodeKind::Block { .. }),
                    "expected Block body, got {:?}",
                    body.kind
                );
            }
            other => panic!("expected For node, got {:?}", other),
        }
    }

    // ── If-let desugaring ──────────────────────────────────────────────────────

    #[test]
    fn if_let_desugared_to_match() {
        // `if let Some(x) = e { ... }` → `match e { Some(x) => { ... }, _ => {} }`
        let if_let = Expr::If {
            id: 1,
            span: span(),
            let_pattern: Some(Pattern::Constructor {
                id: 2,
                span: span(),
                path: bock_ast::TypePath {
                    segments: vec![ident("Some")],
                    span: span(),
                },
                fields: vec![Pattern::Bind {
                    id: 3,
                    span: span(),
                    name: ident("x"),
                }],
            }),
            condition: Box::new(Expr::Identifier {
                id: 4,
                span: span(),
                name: ident("e"),
            }),
            then_block: empty_block(5),
            else_block: None,
        };
        let body = Block {
            id: 6,
            span: span(),
            stmts: vec![],
            tail: Some(Box::new(if_let)),
        };
        let module = make_module(vec![simple_fn(10, "test", body)]);
        let (air, _) = do_lower(&module);

        let tail = find_tail_expr(&air);
        match &tail.kind {
            NodeKind::Match { arms, .. } => {
                assert_eq!(arms.len(), 2, "match should have 2 arms");
                // First arm: ConstructorPat
                assert!(
                    matches!(&arms[0].kind, NodeKind::MatchArm { .. }),
                    "expected MatchArm"
                );
                if let NodeKind::MatchArm { pattern, .. } = &arms[0].kind {
                    assert!(
                        matches!(&pattern.kind, NodeKind::ConstructorPat { .. }),
                        "first arm should have ConstructorPat"
                    );
                }
                // Second arm: WildcardPat
                if let NodeKind::MatchArm { pattern, .. } = &arms[1].kind {
                    assert!(
                        matches!(&pattern.kind, NodeKind::WildcardPat),
                        "second arm should have WildcardPat"
                    );
                }
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    // ── Method call desugaring ─────────────────────────────────────────────────

    #[test]
    fn method_call_desugared_to_call_with_self() {
        // `obj.method(arg)` → `Call { callee: FieldAccess(obj, method), args: [obj, arg] }`
        let method_call = Expr::MethodCall {
            id: 1,
            span: span(),
            receiver: Box::new(Expr::Identifier {
                id: 2,
                span: span(),
                name: ident("obj"),
            }),
            method: ident("method"),
            type_args: vec![],
            args: vec![Arg {
                span: span(),
                label: None,
                mutable: false,
                value: Expr::Identifier {
                    id: 3,
                    span: span(),
                    name: ident("arg"),
                },
            }],
        };
        let body = Block {
            id: 4,
            span: span(),
            stmts: vec![],
            tail: Some(Box::new(method_call)),
        };
        let module = make_module(vec![simple_fn(10, "test", body)]);
        let (air, _) = do_lower(&module);

        let tail = find_tail_expr(&air);
        match &tail.kind {
            NodeKind::Call { callee, args, .. } => {
                // Callee should be a FieldAccess.
                assert!(
                    matches!(&callee.kind, NodeKind::FieldAccess { .. }),
                    "callee should be FieldAccess, got {:?}",
                    callee.kind
                );
                // Args: self (obj) + original arg.
                assert_eq!(args.len(), 2, "should have self + 1 arg");
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Walk the AIR tree to find the tail expression of the innermost block.
    fn find_tail_expr(air: &AIRNode) -> &AIRNode {
        match &air.kind {
            NodeKind::Module { items, .. } => find_tail_expr(&items[0]),
            NodeKind::FnDecl { body, .. } => find_tail_expr(body),
            NodeKind::Block {
                stmts: block_stmts,
                tail,
            } => {
                if let Some(t) = tail {
                    t.as_ref()
                } else if !block_stmts.is_empty() {
                    find_tail_expr(block_stmts.last().unwrap())
                } else {
                    air
                }
            }
            _ => air,
        }
    }
}
