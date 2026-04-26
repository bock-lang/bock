//! AIR node definitions — the unified intermediate representation.
//!
//! Every construct in an Bock program is represented as an [`AIRNode`] with a
//! [`NodeKind`] discriminant that carries typed children. All four AIR layers
//! (S-AIR, T-AIR, C-AIR, TR-AIR) use the same node type; later passes fill
//! in the layer slots that start as `None`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};

use bock_ast::{
    Annotation, AssignOp, BinOp, GenericParam, Ident, ImportItems, Literal, ModulePath,
    PropertyBinding, RecordDeclField, TypeConstraint, TypePath, UnaryOp, Visibility,
};
use bock_errors::Span;

use crate::stubs::{
    Capability, ContextBlock, EffectRef, OwnershipInfo, TargetInfo, TypeInfo, Value,
};

// ─── NodeId ───────────────────────────────────────────────────────────────────

/// Unique identifier for an AIR node within a compilation session.
pub type NodeId = u32;

/// A monotonic counter that generates unique [`NodeId`]s.
///
/// Typically one `NodeIdGen` is created per compilation session and shared
/// (via `&NodeIdGen`) across all lowering passes.
#[derive(Debug, Default)]
pub struct NodeIdGen {
    counter: AtomicU32,
}

impl NodeIdGen {
    /// Creates a new generator starting at zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counter: AtomicU32::new(0),
        }
    }

    /// Returns the next unique [`NodeId`].
    #[must_use]
    pub fn next(&self) -> NodeId {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }
}

// ─── AIR node ─────────────────────────────────────────────────────────────────

/// A single node in the Bock Intermediate Representation.
///
/// Each `AIRNode` carries:
/// - A unique [`NodeId`] and source [`Span`]
/// - A [`NodeKind`] with typed, structured children
/// - Optional slots for each AIR layer (initially `None`, filled by passes)
/// - An extensible metadata map for pass-specific annotations
#[derive(Debug, Clone, PartialEq)]
pub struct AIRNode {
    /// Unique identifier for this node in the session.
    pub id: NodeId,
    /// Source location of this node.
    pub span: Span,
    /// Discriminant and typed children of this node.
    pub kind: NodeKind,
    // ── Layer 1 slots (populated by the type checker) ──────────────────────
    /// Resolved type of this node (set by T-AIR pass).
    pub type_info: Option<TypeInfo>,
    /// Ownership/borrow annotation (set by T-AIR pass).
    pub ownership: Option<OwnershipInfo>,
    /// Algebraic effects this node may perform (set by T-AIR pass).
    pub effects: HashSet<EffectRef>,
    /// Capabilities this node requires (set by T-AIR pass).
    pub capabilities: HashSet<Capability>,
    // ── Layer 2 slot (populated by the context resolver) ──────────────────
    /// Context annotations (set by C-AIR pass).
    pub context: Option<ContextBlock>,
    // ── Layer 3 slot (populated by the target analyzer) ───────────────────
    /// Target-specific information (set by TR-AIR pass).
    pub target: Option<TargetInfo>,
    // ── Extensible metadata ────────────────────────────────────────────────
    /// Arbitrary pass-specific metadata keyed by string.
    pub metadata: HashMap<String, Value>,
}

impl AIRNode {
    /// Creates a new S-AIR node with all layer slots empty.
    #[must_use]
    pub fn new(id: NodeId, span: Span, kind: NodeKind) -> Self {
        Self {
            id,
            span,
            kind,
            type_info: None,
            ownership: None,
            effects: HashSet::new(),
            capabilities: HashSet::new(),
            context: None,
            target: None,
            metadata: HashMap::new(),
        }
    }
}

// ─── Auxiliary types ──────────────────────────────────────────────────────────

/// A named argument in a call expression: `label: value`.
#[derive(Debug, Clone, PartialEq)]
pub struct AirArg {
    /// Optional call-site label (e.g. `with:`, `from:`).
    pub label: Option<Ident>,
    /// The argument expression.
    pub value: AIRNode,
}

/// A field in a record construction expression.
#[derive(Debug, Clone, PartialEq)]
pub struct AirRecordField {
    /// Field name.
    pub name: Ident,
    /// `None` means shorthand: `{ name }` ≡ `{ name: name }`.
    pub value: Option<Box<AIRNode>>,
}

/// A field binding inside a record pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct AirRecordPatternField {
    /// Field name.
    pub name: Ident,
    /// `None` means shorthand: `{ name }` ≡ `{ name: name }`.
    pub pattern: Option<Box<AIRNode>>,
}

/// A key-value entry in a map literal.
#[derive(Debug, Clone, PartialEq)]
pub struct AirMapEntry {
    pub key: AIRNode,
    pub value: AIRNode,
}

/// A handler pair in a `handling` block: `Effect with handler`.
#[derive(Debug, Clone, PartialEq)]
pub struct AirHandlerPair {
    /// The effect being handled.
    pub effect: TypePath,
    /// The handler expression node.
    pub handler: Box<AIRNode>,
}

/// A segment of a string interpolation expression.
#[derive(Debug, Clone, PartialEq)]
pub enum AirInterpolationPart {
    /// A literal string segment.
    Literal(String),
    /// An embedded expression `${expr}`.
    Expr(Box<AIRNode>),
}

/// `Ok` or `Err` variant for result construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultVariant {
    Ok,
    Err,
}

// ─── NodeKind ─────────────────────────────────────────────────────────────────

/// Discriminant and typed children of an [`AIRNode`].
///
/// Children are structured per variant (not a flat `Vec<AIRNode>`), mirroring
/// the AST but lowered into the unified AIR representation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum NodeKind {
    // ── Module ────────────────────────────────────────────────────────────
    /// The root node of a compiled Bock source file.
    Module {
        path: Option<ModulePath>,
        /// Module-level annotations (`@context`, `@requires`, etc.).
        annotations: Vec<Annotation>,
        /// Import declarations (`NodeKind::ImportDecl`).
        imports: Vec<AIRNode>,
        /// Top-level items.
        items: Vec<AIRNode>,
    },

    /// An import declaration: `import Foo.Bar.{ A, B }`.
    ImportDecl {
        path: ModulePath,
        items: ImportItems,
    },

    // ── Declarations ──────────────────────────────────────────────────────
    /// A function declaration.
    FnDecl {
        annotations: Vec<Annotation>,
        visibility: Visibility,
        is_async: bool,
        name: Ident,
        generic_params: Vec<GenericParam>,
        /// Parameter nodes (`NodeKind::Param`).
        params: Vec<AIRNode>,
        /// Optional return-type node (a type-expression variant).
        return_type: Option<Box<AIRNode>>,
        /// Effect names listed in the `with` clause.
        effect_clause: Vec<TypePath>,
        where_clause: Vec<TypeConstraint>,
        /// Body block node (`NodeKind::Block`).
        body: Box<AIRNode>,
    },

    /// A record (value-type) declaration.
    RecordDecl {
        annotations: Vec<Annotation>,
        visibility: Visibility,
        name: Ident,
        generic_params: Vec<GenericParam>,
        fields: Vec<RecordDeclField>,
    },

    /// An enum (algebraic data type) declaration.
    EnumDecl {
        annotations: Vec<Annotation>,
        visibility: Visibility,
        name: Ident,
        generic_params: Vec<GenericParam>,
        /// Variant nodes (unit, struct, or tuple — see AST `EnumVariant`).
        variants: Vec<AIRNode>,
    },

    /// An enum variant — unit, struct-like, or tuple-like.
    EnumVariant {
        name: Ident,
        /// `None` = unit; `Some` = struct fields or positional types.
        payload: EnumVariantPayload,
    },

    /// A class declaration.
    ClassDecl {
        annotations: Vec<Annotation>,
        visibility: Visibility,
        name: Ident,
        generic_params: Vec<GenericParam>,
        base: Option<TypePath>,
        traits: Vec<TypePath>,
        fields: Vec<RecordDeclField>,
        /// Method nodes (`NodeKind::FnDecl`).
        methods: Vec<AIRNode>,
    },

    /// A trait (or platform-trait) declaration.
    TraitDecl {
        annotations: Vec<Annotation>,
        visibility: Visibility,
        is_platform: bool,
        name: Ident,
        generic_params: Vec<GenericParam>,
        associated_types: Vec<bock_ast::AssociatedType>,
        /// Method nodes (`NodeKind::FnDecl`).
        methods: Vec<AIRNode>,
    },

    /// An `impl Trait for Type` or `impl Type` block.
    ImplBlock {
        annotations: Vec<Annotation>,
        generic_params: Vec<GenericParam>,
        trait_path: Option<TypePath>,
        /// The type being implemented (a type-expression node).
        target: Box<AIRNode>,
        where_clause: Vec<TypeConstraint>,
        /// Method nodes (`NodeKind::FnDecl`).
        methods: Vec<AIRNode>,
    },

    /// An algebraic effect declaration.
    EffectDecl {
        annotations: Vec<Annotation>,
        visibility: Visibility,
        name: Ident,
        generic_params: Vec<GenericParam>,
        /// Component effects for composite effects: `effect IO = Log + Clock`.
        components: Vec<TypePath>,
        /// Operation nodes (`NodeKind::FnDecl`).
        operations: Vec<AIRNode>,
    },

    /// A type alias: `type Name[T] = ...`.
    TypeAlias {
        annotations: Vec<Annotation>,
        visibility: Visibility,
        name: Ident,
        generic_params: Vec<GenericParam>,
        /// The aliased type-expression node.
        ty: Box<AIRNode>,
        where_clause: Vec<TypeConstraint>,
    },

    /// A constant declaration: `const NAME: Type = value`.
    ConstDecl {
        annotations: Vec<Annotation>,
        visibility: Visibility,
        name: Ident,
        /// Type annotation node (type-expression variant).
        ty: Box<AIRNode>,
        /// Initialiser expression node.
        value: Box<AIRNode>,
    },

    /// A module-level `handle Effect with handler` declaration.
    ModuleHandle {
        effect: TypePath,
        /// Handler expression node.
        handler: Box<AIRNode>,
    },

    /// A `property("name") { forall(...) { ... } }` property-based test.
    PropertyTest {
        name: String,
        bindings: Vec<PropertyBinding>,
        /// Body block node (`NodeKind::Block`).
        body: Box<AIRNode>,
    },

    // ── Function parameter ────────────────────────────────────────────────
    /// A single function/lambda parameter.
    Param {
        /// Pattern node (a pattern variant).
        pattern: Box<AIRNode>,
        /// Optional type annotation node (type-expression variant).
        ty: Option<Box<AIRNode>>,
        /// Optional default-value expression node.
        default: Option<Box<AIRNode>>,
    },

    // ── Type expressions ──────────────────────────────────────────────────
    /// A named type, possibly with generic arguments: `List[Int]`.
    TypeNamed {
        path: TypePath,
        /// Generic argument nodes (type-expression variants).
        args: Vec<AIRNode>,
    },

    /// A tuple type: `(Int, String)`.
    TypeTuple {
        /// Element type nodes (type-expression variants).
        elems: Vec<AIRNode>,
    },

    /// A function type: `Fn(Int) -> String with Log`.
    TypeFunction {
        /// Parameter type nodes (type-expression variants).
        params: Vec<AIRNode>,
        /// Return type node (type-expression variant).
        ret: Box<AIRNode>,
        /// Effects listed in the `with` clause.
        effects: Vec<TypePath>,
    },

    /// An optional type: `Int?`.
    TypeOptional {
        /// Inner type node (type-expression variant).
        inner: Box<AIRNode>,
    },

    /// The `Self` type in a trait/impl context.
    TypeSelf,

    // ── Expressions ───────────────────────────────────────────────────────
    /// A literal value.
    Literal { lit: Literal },

    /// An identifier reference.
    Identifier { name: Ident },

    /// A binary operation: `a + b`.
    BinaryOp {
        op: BinOp,
        left: Box<AIRNode>,
        right: Box<AIRNode>,
    },

    /// A unary operation: `-x`, `!flag`.
    UnaryOp { op: UnaryOp, operand: Box<AIRNode> },

    /// An assignment expression: `x = 5`, `x += 1`.
    Assign {
        op: AssignOp,
        target: Box<AIRNode>,
        value: Box<AIRNode>,
    },

    /// A function call: `f(a, b)`.
    Call {
        callee: Box<AIRNode>,
        args: Vec<AirArg>,
        type_args: Vec<AIRNode>,
    },

    /// A method call: `obj.method(a, b)`.
    MethodCall {
        receiver: Box<AIRNode>,
        method: Ident,
        type_args: Vec<AIRNode>,
        args: Vec<AirArg>,
    },

    /// Field access: `obj.field`.
    FieldAccess { object: Box<AIRNode>, field: Ident },

    /// Index access: `arr[i]`.
    Index {
        object: Box<AIRNode>,
        index: Box<AIRNode>,
    },

    /// Error propagation: `expr?` — maps to spec's `Propagate`.
    Propagate { expr: Box<AIRNode> },

    /// A lambda: `(x) => x * 2`.
    Lambda {
        /// Parameter nodes (`NodeKind::Param`).
        params: Vec<AIRNode>,
        /// Body expression node.
        body: Box<AIRNode>,
    },

    /// Pipe operator: `data |> parse`.
    Pipe {
        left: Box<AIRNode>,
        right: Box<AIRNode>,
    },

    /// Function composition: `parse >> validate`.
    Compose {
        left: Box<AIRNode>,
        right: Box<AIRNode>,
    },

    /// An `await` expression.
    Await { expr: Box<AIRNode> },

    /// A range: `1..10` (exclusive) or `1..=10` (inclusive).
    Range {
        lo: Box<AIRNode>,
        hi: Box<AIRNode>,
        inclusive: bool,
    },

    /// Record construction: `User { id: 1, name, ..defaults }`.
    RecordConstruct {
        path: TypePath,
        fields: Vec<AirRecordField>,
        spread: Option<Box<AIRNode>>,
    },

    /// List literal: `[1, 2, 3]`.
    ListLiteral { elems: Vec<AIRNode> },

    /// Map literal: `{"key": value}`.
    MapLiteral { entries: Vec<AirMapEntry> },

    /// Set literal: `#{"a", "b"}`.
    SetLiteral { elems: Vec<AIRNode> },

    /// Tuple literal: `("hello", 42)`.
    TupleLiteral { elems: Vec<AIRNode> },

    /// String interpolation: `"Hello, ${name}!"`.
    Interpolation { parts: Vec<AirInterpolationPart> },

    /// A placeholder `_` used in pipe expressions.
    Placeholder,

    /// `unreachable` — a diverging expression.
    Unreachable,

    /// Explicit `Ok(v)` or `Err(e)` result construction.
    ResultConstruct {
        variant: ResultVariant,
        value: Option<Box<AIRNode>>,
    },

    // ── Control flow ──────────────────────────────────────────────────────
    /// An `if` / `if-let` expression.
    If {
        /// For `if let pat = expr`, holds the pattern node.
        let_pattern: Option<Box<AIRNode>>,
        condition: Box<AIRNode>,
        /// Then-branch block node (`NodeKind::Block`).
        then_block: Box<AIRNode>,
        /// Optional else branch (block or nested if node).
        else_block: Option<Box<AIRNode>>,
    },

    /// A `guard condition else { ... }` statement.
    ///
    /// When `let_pattern` is `Some`, this is `guard (let pat = expr) else { ... }`.
    Guard {
        /// For `guard (let pat = expr)`, the pattern node.
        let_pattern: Option<Box<AIRNode>>,
        condition: Box<AIRNode>,
        else_block: Box<AIRNode>,
    },

    /// A `match` expression.
    Match {
        scrutinee: Box<AIRNode>,
        /// Match arm nodes (`NodeKind::MatchArm`).
        arms: Vec<AIRNode>,
    },

    /// One arm of a `match` expression.
    MatchArm {
        /// Pattern node (a pattern variant).
        pattern: Box<AIRNode>,
        /// Optional guard expression.
        guard: Option<Box<AIRNode>>,
        /// Body expression node.
        body: Box<AIRNode>,
    },

    /// A `for` loop.
    For {
        /// Loop variable pattern node (a pattern variant).
        pattern: Box<AIRNode>,
        iterable: Box<AIRNode>,
        body: Box<AIRNode>,
    },

    /// A `while` loop.
    While {
        condition: Box<AIRNode>,
        body: Box<AIRNode>,
    },

    /// An infinite `loop`.
    Loop { body: Box<AIRNode> },

    /// A block of statements with an optional tail expression.
    Block {
        stmts: Vec<AIRNode>,
        tail: Option<Box<AIRNode>>,
    },

    /// A `return` expression.
    Return { value: Option<Box<AIRNode>> },

    /// A `break` expression, optionally with a value.
    Break { value: Option<Box<AIRNode>> },

    /// A `continue` expression.
    Continue,

    // ── Ownership ─────────────────────────────────────────────────────────
    /// A `let [mut] pattern [: Type] = value` binding.
    LetBinding {
        is_mut: bool,
        pattern: Box<AIRNode>,
        ty: Option<Box<AIRNode>>,
        value: Box<AIRNode>,
    },

    /// An explicit move of ownership: `move expr`.
    Move { expr: Box<AIRNode> },

    /// An immutable borrow: `&expr`.
    Borrow { expr: Box<AIRNode> },

    /// A mutable borrow: `&mut expr`.
    MutableBorrow { expr: Box<AIRNode> },

    // ── Effects ───────────────────────────────────────────────────────────
    /// An algebraic-effect operation invocation.
    EffectOp {
        effect: TypePath,
        operation: Ident,
        args: Vec<AirArg>,
    },

    /// A `handling (Effect with handler, ...) { body }` block.
    HandlingBlock {
        handlers: Vec<AirHandlerPair>,
        body: Box<AIRNode>,
    },

    /// A reference to an effect type (used in type positions and signatures).
    EffectRef { path: TypePath },

    // ── Patterns ──────────────────────────────────────────────────────────
    /// `_` — wildcard pattern, matches anything.
    WildcardPat,

    /// `name` or `mut name` — bind pattern.
    BindPat { name: Ident, is_mut: bool },

    /// A literal pattern: `42`, `"hello"`, `true`.
    LiteralPat { lit: Literal },

    /// An enum constructor pattern: `Some(x)`, `Ok(v)`.
    ConstructorPat {
        path: TypePath,
        /// Positional field pattern nodes.
        fields: Vec<AIRNode>,
    },

    /// A record pattern: `User { name, age }`.
    RecordPat {
        path: TypePath,
        fields: Vec<AirRecordPatternField>,
        /// `true` when the pattern contains a `..` rest marker.
        rest: bool,
    },

    /// A tuple pattern: `(a, b, c)`.
    TuplePat { elems: Vec<AIRNode> },

    /// A list pattern: `[head, ..tail]`.
    ListPat {
        elems: Vec<AIRNode>,
        rest: Option<Box<AIRNode>>,
    },

    /// An or-pattern: `A | B`.
    OrPat { alternatives: Vec<AIRNode> },

    /// A guard pattern (in pattern-matching guard position).
    GuardPat {
        pattern: Box<AIRNode>,
        guard: Box<AIRNode>,
    },

    /// A range pattern: `1..10` or `1..=10`.
    RangePat {
        lo: Box<AIRNode>,
        hi: Box<AIRNode>,
        inclusive: bool,
    },

    /// A rest pattern `..` (inside list/tuple patterns).
    RestPat,

    // ── Error recovery ────────────────────────────────────────────────────
    /// An error-recovery node wrapping tokens that could not be lowered.
    Error,
}

/// The payload of an enum variant in the AIR.
#[derive(Debug, Clone, PartialEq)]
pub enum EnumVariantPayload {
    /// Unit variant: `Variant`.
    Unit,
    /// Struct-like variant: `Variant { field: Type, ... }`.
    Struct(Vec<RecordDeclField>),
    /// Tuple-like variant: `Variant(Type, Type)`.
    Tuple(Vec<AIRNode>),
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

    fn make_node(id: NodeId, kind: NodeKind) -> AIRNode {
        AIRNode::new(id, dummy_span(), kind)
    }

    // ── NodeIdGen ──────────────────────────────────────────────────────────

    #[test]
    fn node_id_gen_monotonic() {
        let gen = NodeIdGen::new();
        let a = gen.next();
        let b = gen.next();
        let c = gen.next();
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(c, 2);
    }

    #[test]
    fn node_id_gen_thread_safe() {
        use std::sync::Arc;
        use std::thread;

        let gen = Arc::new(NodeIdGen::new());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let g = Arc::clone(&gen);
                thread::spawn(move || g.next())
            })
            .collect();
        let mut ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        ids.sort();
        // All IDs should be distinct (0..4 in some order)
        assert_eq!(ids, vec![0, 1, 2, 3]);
    }

    // ── AIRNode basics ─────────────────────────────────────────────────────

    #[test]
    fn air_node_new_has_empty_slots() {
        let node = make_node(42, NodeKind::Continue);
        assert_eq!(node.id, 42);
        assert!(node.type_info.is_none());
        assert!(node.ownership.is_none());
        assert!(node.effects.is_empty());
        assert!(node.capabilities.is_empty());
        assert!(node.context.is_none());
        assert!(node.target.is_none());
        assert!(node.metadata.is_empty());
    }

    #[test]
    fn air_node_debug_contains_kind() {
        let node = make_node(0, NodeKind::Unreachable);
        let s = format!("{node:?}");
        assert!(s.contains("Unreachable"));
    }

    // ── NodeKind coverage ─────────────────────────────────────────────────

    #[test]
    fn module_node() {
        let n = make_node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![],
            },
        );
        assert!(matches!(n.kind, NodeKind::Module { .. }));
    }

    #[test]
    fn fn_decl_node() {
        let body = make_node(
            1,
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        let n = make_node(
            0,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: dummy_ident("foo"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        assert!(matches!(n.kind, NodeKind::FnDecl { .. }));
    }

    #[test]
    fn binary_op_node() {
        let left = make_node(
            1,
            NodeKind::Literal {
                lit: Literal::Int("1".into()),
            },
        );
        let right = make_node(
            2,
            NodeKind::Literal {
                lit: Literal::Int("2".into()),
            },
        );
        let n = make_node(
            0,
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(left),
                right: Box::new(right),
            },
        );
        assert!(matches!(n.kind, NodeKind::BinaryOp { op: BinOp::Add, .. }));
    }

    #[test]
    fn pattern_nodes() {
        let wildcard = make_node(0, NodeKind::WildcardPat);
        let bind = make_node(
            1,
            NodeKind::BindPat {
                name: dummy_ident("x"),
                is_mut: false,
            },
        );
        let lit = make_node(
            2,
            NodeKind::LiteralPat {
                lit: Literal::Bool(true),
            },
        );
        assert!(matches!(wildcard.kind, NodeKind::WildcardPat));
        assert!(matches!(bind.kind, NodeKind::BindPat { .. }));
        assert!(matches!(lit.kind, NodeKind::LiteralPat { .. }));
    }

    #[test]
    fn control_flow_nodes() {
        let body = Box::new(make_node(
            1,
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        ));
        let cond = Box::new(make_node(
            2,
            NodeKind::Literal {
                lit: Literal::Bool(true),
            },
        ));

        let while_node = make_node(
            0,
            NodeKind::While {
                condition: cond.clone(),
                body: body.clone(),
            },
        );
        let loop_node = make_node(3, NodeKind::Loop { body: body.clone() });
        let return_node = make_node(4, NodeKind::Return { value: None });
        let break_node = make_node(5, NodeKind::Break { value: None });
        let continue_node = make_node(6, NodeKind::Continue);

        assert!(matches!(while_node.kind, NodeKind::While { .. }));
        assert!(matches!(loop_node.kind, NodeKind::Loop { .. }));
        assert!(matches!(return_node.kind, NodeKind::Return { value: None }));
        assert!(matches!(break_node.kind, NodeKind::Break { value: None }));
        assert!(matches!(continue_node.kind, NodeKind::Continue));
    }

    #[test]
    fn ownership_nodes() {
        let expr = Box::new(make_node(
            1,
            NodeKind::Identifier {
                name: dummy_ident("x"),
            },
        ));
        let mv = make_node(0, NodeKind::Move { expr: expr.clone() });
        let borrow = make_node(2, NodeKind::Borrow { expr: expr.clone() });
        let mut_borrow = make_node(3, NodeKind::MutableBorrow { expr: expr.clone() });
        assert!(matches!(mv.kind, NodeKind::Move { .. }));
        assert!(matches!(borrow.kind, NodeKind::Borrow { .. }));
        assert!(matches!(mut_borrow.kind, NodeKind::MutableBorrow { .. }));
    }

    #[test]
    fn effect_nodes() {
        let handler = Box::new(make_node(
            1,
            NodeKind::Identifier {
                name: dummy_ident("h"),
            },
        ));
        let body = Box::new(make_node(
            2,
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        ));
        let tp = TypePath {
            segments: vec![dummy_ident("Log")],
            span: dummy_span(),
        };
        let handling = make_node(
            0,
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: tp.clone(),
                    handler,
                }],
                body,
            },
        );
        let effect_ref = make_node(3, NodeKind::EffectRef { path: tp });
        assert!(matches!(handling.kind, NodeKind::HandlingBlock { .. }));
        assert!(matches!(effect_ref.kind, NodeKind::EffectRef { .. }));
    }

    #[test]
    fn type_expr_nodes() {
        let named = make_node(
            0,
            NodeKind::TypeNamed {
                path: TypePath {
                    segments: vec![dummy_ident("Int")],
                    span: dummy_span(),
                },
                args: vec![],
            },
        );
        let self_ty = make_node(1, NodeKind::TypeSelf);
        let opt = make_node(
            2,
            NodeKind::TypeOptional {
                inner: Box::new(named.clone()),
            },
        );
        assert!(matches!(named.kind, NodeKind::TypeNamed { .. }));
        assert!(matches!(self_ty.kind, NodeKind::TypeSelf));
        assert!(matches!(opt.kind, NodeKind::TypeOptional { .. }));
    }

    #[test]
    fn metadata_and_effects_mutable() {
        let mut node = make_node(0, NodeKind::Continue);
        node.metadata
            .insert("pass".into(), crate::stubs::Value::String("T-AIR".into()));
        node.effects.insert(EffectRef::new("Std.Io.Log"));
        node.capabilities
            .insert(Capability::new("Std.Io.FileSystem"));
        assert_eq!(node.metadata.len(), 1);
        assert_eq!(node.effects.len(), 1);
        assert_eq!(node.capabilities.len(), 1);
    }
}
