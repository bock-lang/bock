//! Bock AST — abstract syntax tree node definitions for the Bock language.
//!
//! Every syntactic construct in the Bock grammar maps to a type in this crate.
//! All nodes carry a [`NodeId`] (for compiler bookkeeping) and a [`Span`]
//! (for diagnostics and error reporting).

pub use bock_errors::{FileId, Span};

pub mod visitor;

// ─── Node identity ────────────────────────────────────────────────────────────

/// Unique identifier for an AST node within a compilation session.
pub type NodeId = u32;

// ─── Primitive building blocks ────────────────────────────────────────────────

/// An identifier token with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// A qualified path of identifiers separated by `.`, e.g. `Std.Io.File`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypePath {
    pub segments: Vec<Ident>,
    pub span: Span,
}

/// A module path declared at the top of a file, e.g. `module Std.Io`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModulePath {
    pub segments: Vec<Ident>,
    pub span: Span,
}

// ─── Visibility ───────────────────────────────────────────────────────────────

/// Declaration visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Visible only within the current module (default).
    #[default]
    Private,
    /// Visible within the current package/crate.
    Internal,
    /// Visible to all consumers.
    Public,
}

// ─── Annotations ─────────────────────────────────────────────────────────────

/// A single argument to an annotation, optionally labelled.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationArg {
    pub label: Option<Ident>,
    pub value: Expr,
}

/// A decorator-style annotation such as `@derive(Equatable)` or
/// `@performance(max_latency: 100)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub id: NodeId,
    pub span: Span,
    pub name: Ident,
    pub args: Vec<AnnotationArg>,
}

// ─── Generic parameters and constraints ──────────────────────────────────────

/// A single generic type parameter, e.g. `T` or `T: Bound`.
#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub id: NodeId,
    pub span: Span,
    pub name: Ident,
    pub bounds: Vec<TypePath>,
}

/// A `where` clause constraint, e.g. `T: Equatable`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeConstraint {
    pub id: NodeId,
    pub span: Span,
    pub param: Ident,
    pub bounds: Vec<TypePath>,
}

// ─── Type expressions ─────────────────────────────────────────────────────────

/// A syntactic type expression as it appears in source code.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    /// A named type, possibly with generic arguments: `List[Int]`.
    Named {
        id: NodeId,
        span: Span,
        path: TypePath,
        args: Vec<TypeExpr>,
    },
    /// A tuple type: `(Int, String)`.
    Tuple {
        id: NodeId,
        span: Span,
        elems: Vec<TypeExpr>,
    },
    /// A function type: `Fn(Int) -> String` or `Fn(String) -> Void with Log`.
    Function {
        id: NodeId,
        span: Span,
        params: Vec<TypeExpr>,
        ret: Box<TypeExpr>,
        /// Effects listed in the `with` clause (empty if none).
        effects: Vec<TypePath>,
    },
    /// An optional type: `Int?`.
    Optional {
        id: NodeId,
        span: Span,
        inner: Box<TypeExpr>,
    },
    /// The `self` type in trait/impl context.
    SelfType { id: NodeId, span: Span },
}

impl TypeExpr {
    /// Returns this node's [`NodeId`].
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        match self {
            TypeExpr::Named { id, .. }
            | TypeExpr::Tuple { id, .. }
            | TypeExpr::Function { id, .. }
            | TypeExpr::Optional { id, .. }
            | TypeExpr::SelfType { id, .. } => *id,
        }
    }

    /// Returns this node's source [`Span`].
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Named { span, .. }
            | TypeExpr::Tuple { span, .. }
            | TypeExpr::Function { span, .. }
            | TypeExpr::Optional { span, .. }
            | TypeExpr::SelfType { span, .. } => *span,
        }
    }
}

// ─── Literals ────────────────────────────────────────────────────────────────

/// A literal value as it appears in source.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    /// Integer literal, stored as the raw token text to preserve radix/suffix.
    Int(String),
    /// Floating-point literal.
    Float(String),
    /// Boolean literal.
    Bool(bool),
    /// Character literal (a single Unicode scalar).
    Char(String),
    /// String literal (already decoded from escape sequences).
    String(String),
    /// The unit value `()`.
    Unit,
}

/// Known numeric type suffixes (without the leading underscore).
const TYPE_SUFFIXES: &[&str] = &[
    "i128", "i64", "i32", "i16", "i8", "u64", "u32", "u16", "u8", "f64", "f32",
];

/// Strip a type suffix (e.g., `_u8`, `_f64`) from a numeric literal string.
///
/// Returns `(numeric_part, Some(suffix_without_underscore))` if a known suffix
/// is present, or `(original, None)` otherwise.
#[must_use]
pub fn strip_type_suffix(s: &str) -> (&str, Option<&str>) {
    for suffix in TYPE_SUFFIXES {
        // Check for `_` + suffix at the end of the string.
        let with_underscore_len = 1 + suffix.len();
        if s.len() > with_underscore_len {
            let split = s.len() - with_underscore_len;
            if s.as_bytes()[split] == b'_' && &s[split + 1..] == *suffix {
                return (&s[..split], Some(suffix));
            }
        }
    }
    (s, None)
}

// ─── Patterns ─────────────────────────────────────────────────────────────────

/// A destructuring pattern used in `let`, `match`, `for`, etc.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// `_` — matches anything, binds nothing.
    Wildcard { id: NodeId, span: Span },
    /// `name` — matches anything and binds it immutably.
    Bind { id: NodeId, span: Span, name: Ident },
    /// `mut name` — matches anything and binds it mutably.
    MutBind { id: NodeId, span: Span, name: Ident },
    /// A literal pattern: `42`, `"hello"`, `true`.
    Literal {
        id: NodeId,
        span: Span,
        lit: Literal,
    },
    /// An enum constructor pattern: `Some(x)`, `Ok(v)`.
    Constructor {
        id: NodeId,
        span: Span,
        path: TypePath,
        fields: Vec<Pattern>,
    },
    /// A record pattern: `User { name, age }` or `User { name: n, .. }`.
    Record {
        id: NodeId,
        span: Span,
        path: TypePath,
        fields: Vec<RecordPatternField>,
        /// `true` when the pattern contains a `..` rest marker (ignore remaining fields).
        rest: bool,
    },
    /// A tuple pattern: `(a, b, c)`.
    Tuple {
        id: NodeId,
        span: Span,
        elems: Vec<Pattern>,
    },
    /// A list pattern: `[head, ..tail]`.
    List {
        id: NodeId,
        span: Span,
        elems: Vec<Pattern>,
        rest: Option<Box<Pattern>>,
    },
    /// An or-pattern: `A | B`.
    Or {
        id: NodeId,
        span: Span,
        alternatives: Vec<Pattern>,
    },
    /// A range pattern: `1..10` or `1..=10`.
    Range {
        id: NodeId,
        span: Span,
        lo: Box<Pattern>,
        hi: Box<Pattern>,
        inclusive: bool,
    },
    /// A rest pattern `..` (used inside list/tuple patterns).
    Rest { id: NodeId, span: Span },
}

impl Pattern {
    /// Returns this node's [`NodeId`].
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        match self {
            Pattern::Wildcard { id, .. }
            | Pattern::Bind { id, .. }
            | Pattern::MutBind { id, .. }
            | Pattern::Literal { id, .. }
            | Pattern::Constructor { id, .. }
            | Pattern::Record { id, .. }
            | Pattern::Tuple { id, .. }
            | Pattern::List { id, .. }
            | Pattern::Or { id, .. }
            | Pattern::Range { id, .. }
            | Pattern::Rest { id, .. } => *id,
        }
    }

    /// Returns this node's source [`Span`].
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard { span, .. }
            | Pattern::Bind { span, .. }
            | Pattern::MutBind { span, .. }
            | Pattern::Literal { span, .. }
            | Pattern::Constructor { span, .. }
            | Pattern::Record { span, .. }
            | Pattern::Tuple { span, .. }
            | Pattern::List { span, .. }
            | Pattern::Or { span, .. }
            | Pattern::Range { span, .. }
            | Pattern::Rest { span, .. } => *span,
        }
    }
}

/// One field binding inside a record pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordPatternField {
    pub span: Span,
    pub name: Ident,
    /// `None` means shorthand: `{ name }` ≡ `{ name: name }`.
    pub pattern: Option<Pattern>,
}

// ─── Expressions ─────────────────────────────────────────────────────────────

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    // Comparison
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    // Logical
    And,
    Or,
    // Bitwise
    BitAnd,
    BitOr,
    BitXor,
    // Function composition
    Compose, // >>
    // Type membership
    Is,
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    /// Bitwise NOT: `~x`.
    BitNot,
}

/// An assignment operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    RemAssign,
}

/// A named argument in a function call: `label: value`.
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub span: Span,
    pub label: Option<Ident>,
    /// Whether the argument is prefixed with `mut` at the call site.
    pub mutable: bool,
    pub value: Expr,
}

/// One field in a record construction expression.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordField {
    pub span: Span,
    pub name: Ident,
    /// `None` means shorthand: `{ name }` ≡ `{ name: name }`.
    pub value: Option<Expr>,
}

/// A spread element in record construction: `..defaults`.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordSpread {
    pub span: Span,
    pub expr: Expr,
}

/// One arm of a `match` expression.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub id: NodeId,
    pub span: Span,
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

/// A binding in a `forall` clause of a `property` test.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyBinding {
    pub span: Span,
    pub name: Ident,
    pub ty: TypeExpr,
}

/// A handler pair inside a `handling` block: `Log with handler`.
#[derive(Debug, Clone, PartialEq)]
pub struct HandlerPair {
    pub span: Span,
    pub effect: TypePath,
    pub handler: Expr,
}

/// An expression node.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A literal value.
    Literal {
        id: NodeId,
        span: Span,
        lit: Literal,
    },

    /// An identifier reference.
    Identifier { id: NodeId, span: Span, name: Ident },

    /// A binary operation: `a + b`.
    Binary {
        id: NodeId,
        span: Span,
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },

    /// A unary operation: `-x`, `!flag`.
    Unary {
        id: NodeId,
        span: Span,
        op: UnaryOp,
        operand: Box<Expr>,
    },

    /// An assignment expression: `x = 5`, `x += 1`.
    Assign {
        id: NodeId,
        span: Span,
        op: AssignOp,
        target: Box<Expr>,
        value: Box<Expr>,
    },

    /// A function call: `f(a, b)`.
    Call {
        id: NodeId,
        span: Span,
        callee: Box<Expr>,
        args: Vec<Arg>,
        type_args: Vec<TypeExpr>,
    },

    /// A method call: `obj.method(a, b)`.
    MethodCall {
        id: NodeId,
        span: Span,
        receiver: Box<Expr>,
        method: Ident,
        type_args: Vec<TypeExpr>,
        args: Vec<Arg>,
    },

    /// Field access: `obj.field`.
    FieldAccess {
        id: NodeId,
        span: Span,
        object: Box<Expr>,
        field: Ident,
    },

    /// Index access: `arr[i]`.
    Index {
        id: NodeId,
        span: Span,
        object: Box<Expr>,
        index: Box<Expr>,
    },

    /// Error propagation: `expr?`.
    Try {
        id: NodeId,
        span: Span,
        expr: Box<Expr>,
    },

    /// A lambda: `(x) => x * 2`.
    Lambda {
        id: NodeId,
        span: Span,
        params: Vec<Param>,
        body: Box<Expr>,
    },

    /// Pipe operator: `data |> parse`.
    Pipe {
        id: NodeId,
        span: Span,
        left: Box<Expr>,
        right: Box<Expr>,
    },

    /// Function composition: `parse >> validate`.
    Compose {
        id: NodeId,
        span: Span,
        left: Box<Expr>,
        right: Box<Expr>,
    },

    /// An `if` / `if-let` expression.
    If {
        id: NodeId,
        span: Span,
        /// For `if-let`, this holds the pattern; `None` for plain `if`.
        let_pattern: Option<Pattern>,
        condition: Box<Expr>,
        then_block: Block,
        else_block: Option<Box<Expr>>,
    },

    /// A `match` expression.
    Match {
        id: NodeId,
        span: Span,
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },

    /// A `loop` expression: `loop { ... break value }`.
    Loop { id: NodeId, span: Span, body: Block },

    /// A block expression: `{ stmts... }`.
    Block {
        id: NodeId,
        span: Span,
        block: Block,
    },

    /// Record construction: `User { id: 1, name, ..defaults }`.
    RecordConstruct {
        id: NodeId,
        span: Span,
        path: TypePath,
        fields: Vec<RecordField>,
        spread: Option<Box<RecordSpread>>,
    },

    /// List literal: `[1, 2, 3]`.
    ListLiteral {
        id: NodeId,
        span: Span,
        elems: Vec<Expr>,
    },

    /// Map literal: `{"key": value}`.
    MapLiteral {
        id: NodeId,
        span: Span,
        entries: Vec<(Expr, Expr)>,
    },

    /// Set literal: `#{"a", "b"}`.
    SetLiteral {
        id: NodeId,
        span: Span,
        elems: Vec<Expr>,
    },

    /// Tuple literal: `("hello", 42)`.
    TupleLiteral {
        id: NodeId,
        span: Span,
        elems: Vec<Expr>,
    },

    /// A range: `1..10` (exclusive) or `1..=10` (inclusive).
    Range {
        id: NodeId,
        span: Span,
        lo: Box<Expr>,
        hi: Box<Expr>,
        inclusive: bool,
    },

    /// An `await` expression: `expr.await` or `await expr`.
    Await {
        id: NodeId,
        span: Span,
        expr: Box<Expr>,
    },

    /// A `return` expression.
    Return {
        id: NodeId,
        span: Span,
        value: Option<Box<Expr>>,
    },

    /// A `break` expression, optionally with a value.
    Break {
        id: NodeId,
        span: Span,
        value: Option<Box<Expr>>,
    },

    /// A `continue` expression.
    Continue { id: NodeId, span: Span },

    /// `unreachable` — a diverging expression.
    Unreachable { id: NodeId, span: Span },

    /// A string interpolation: `"Hello, ${name}!"`.
    Interpolation {
        id: NodeId,
        span: Span,
        parts: Vec<InterpolationPart>,
    },

    /// A placeholder `_` used in pipe expressions.
    Placeholder { id: NodeId, span: Span },

    /// A type-check expression: `expr is Type[Args]`.
    ///
    /// Stores the full [`TypeExpr`] rather than converting to an expression,
    /// so generic arguments (e.g. `List[Int]`) are preserved.
    Is {
        id: NodeId,
        span: Span,
        expr: Box<Expr>,
        type_expr: TypeExpr,
    },
}

impl Expr {
    /// Returns this node's [`NodeId`].
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        match self {
            Expr::Literal { id, .. }
            | Expr::Identifier { id, .. }
            | Expr::Binary { id, .. }
            | Expr::Unary { id, .. }
            | Expr::Assign { id, .. }
            | Expr::Call { id, .. }
            | Expr::MethodCall { id, .. }
            | Expr::FieldAccess { id, .. }
            | Expr::Index { id, .. }
            | Expr::Try { id, .. }
            | Expr::Lambda { id, .. }
            | Expr::Pipe { id, .. }
            | Expr::Compose { id, .. }
            | Expr::If { id, .. }
            | Expr::Match { id, .. }
            | Expr::Loop { id, .. }
            | Expr::Block { id, .. }
            | Expr::RecordConstruct { id, .. }
            | Expr::ListLiteral { id, .. }
            | Expr::MapLiteral { id, .. }
            | Expr::SetLiteral { id, .. }
            | Expr::TupleLiteral { id, .. }
            | Expr::Range { id, .. }
            | Expr::Await { id, .. }
            | Expr::Return { id, .. }
            | Expr::Break { id, .. }
            | Expr::Continue { id, .. }
            | Expr::Unreachable { id, .. }
            | Expr::Interpolation { id, .. }
            | Expr::Placeholder { id, .. }
            | Expr::Is { id, .. } => *id,
        }
    }

    /// Returns this node's source [`Span`].
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Expr::Literal { span, .. }
            | Expr::Identifier { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Assign { span, .. }
            | Expr::Call { span, .. }
            | Expr::MethodCall { span, .. }
            | Expr::FieldAccess { span, .. }
            | Expr::Index { span, .. }
            | Expr::Try { span, .. }
            | Expr::Lambda { span, .. }
            | Expr::Pipe { span, .. }
            | Expr::Compose { span, .. }
            | Expr::If { span, .. }
            | Expr::Match { span, .. }
            | Expr::Loop { span, .. }
            | Expr::Block { span, .. }
            | Expr::RecordConstruct { span, .. }
            | Expr::ListLiteral { span, .. }
            | Expr::MapLiteral { span, .. }
            | Expr::SetLiteral { span, .. }
            | Expr::TupleLiteral { span, .. }
            | Expr::Range { span, .. }
            | Expr::Await { span, .. }
            | Expr::Return { span, .. }
            | Expr::Break { span, .. }
            | Expr::Continue { span, .. }
            | Expr::Unreachable { span, .. }
            | Expr::Interpolation { span, .. }
            | Expr::Placeholder { span, .. }
            | Expr::Is { span, .. } => *span,
        }
    }
}

/// One segment of a string interpolation: either raw text or an embedded expression.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpolationPart {
    /// A raw string segment.
    Literal(String),
    /// An embedded expression: `${expr}`.
    Expr(Expr),
}

// ─── Statements ───────────────────────────────────────────────────────────────

/// A statement node.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// A `let` binding.
    Let(LetStmt),
    /// An expression used as a statement (usually has side effects).
    Expr(Expr),
    /// A `for` loop.
    For(ForLoop),
    /// A `while` loop.
    While(WhileLoop),
    /// An infinite `loop`.
    Loop(LoopStmt),
    /// A `guard` statement.
    Guard(GuardStmt),
    /// A `handling` block.
    Handling(HandlingBlock),
    /// An empty statement (bare semicolon or newline).
    Empty,
}

/// A block of statements with an optional tail expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub id: NodeId,
    pub span: Span,
    pub stmts: Vec<Stmt>,
    /// The final expression whose value becomes the block's value (no trailing newline).
    pub tail: Option<Box<Expr>>,
}

/// `let [mut] pattern [: Type] = value`.
#[derive(Debug, Clone, PartialEq)]
pub struct LetStmt {
    pub id: NodeId,
    pub span: Span,
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
}

/// `for pattern in iterable { body }`.
#[derive(Debug, Clone, PartialEq)]
pub struct ForLoop {
    pub id: NodeId,
    pub span: Span,
    pub pattern: Pattern,
    pub iterable: Expr,
    pub body: Block,
}

/// `while (condition) { body }`.
#[derive(Debug, Clone, PartialEq)]
pub struct WhileLoop {
    pub id: NodeId,
    pub span: Span,
    pub condition: Expr,
    pub body: Block,
}

/// `loop { body }`.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopStmt {
    pub id: NodeId,
    pub span: Span,
    pub body: Block,
}

/// `guard (condition) else { diverging_block }`.
///
/// Supports `guard (let pat = expr) else { ... }` when `let_pattern` is `Some`.
#[derive(Debug, Clone, PartialEq)]
pub struct GuardStmt {
    pub id: NodeId,
    pub span: Span,
    /// For `guard (let pat = expr)`, holds the pattern.
    pub let_pattern: Option<Pattern>,
    pub condition: Expr,
    pub else_block: Block,
}

/// `handling (Effect with handler, ...) { body }`.
#[derive(Debug, Clone, PartialEq)]
pub struct HandlingBlock {
    pub id: NodeId,
    pub span: Span,
    pub handlers: Vec<HandlerPair>,
    pub body: Block,
}

// ─── Function parameters ──────────────────────────────────────────────────────

/// A single function parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub id: NodeId,
    pub span: Span,
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
    /// Optional default value.
    pub default: Option<Expr>,
}

// ─── Declarations ────────────────────────────────────────────────────────────

/// A function declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct FnDecl {
    pub id: NodeId,
    pub span: Span,
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub is_async: bool,
    pub name: Ident,
    pub generic_params: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    /// Effects listed in the `with` clause.
    pub effect_clause: Vec<TypePath>,
    pub where_clause: Vec<TypeConstraint>,
    /// `None` means a required trait method (no default body).
    pub body: Option<Block>,
}

/// A record (value-type) declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordDecl {
    pub id: NodeId,
    pub span: Span,
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: Ident,
    pub generic_params: Vec<GenericParam>,
    pub where_clause: Vec<TypeConstraint>,
    pub fields: Vec<RecordDeclField>,
}

/// A field in a record declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordDeclField {
    pub id: NodeId,
    pub span: Span,
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
}

/// An enum (algebraic data type) declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub id: NodeId,
    pub span: Span,
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: Ident,
    pub generic_params: Vec<GenericParam>,
    pub where_clause: Vec<TypeConstraint>,
    pub variants: Vec<EnumVariant>,
}

/// A single enum variant.
#[derive(Debug, Clone, PartialEq)]
pub enum EnumVariant {
    /// A unit variant: `Variant1`.
    Unit { id: NodeId, span: Span, name: Ident },
    /// A struct-like variant: `Variant2 { field: Type }`.
    Struct {
        id: NodeId,
        span: Span,
        name: Ident,
        fields: Vec<RecordDeclField>,
    },
    /// A tuple-like variant: `Variant3(Type, Type)`.
    Tuple {
        id: NodeId,
        span: Span,
        name: Ident,
        tys: Vec<TypeExpr>,
    },
}

/// A class declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassDecl {
    pub id: NodeId,
    pub span: Span,
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: Ident,
    pub generic_params: Vec<GenericParam>,
    /// Single base class.
    pub base: Option<TypePath>,
    /// Implemented traits.
    pub traits: Vec<TypePath>,
    pub where_clause: Vec<TypeConstraint>,
    pub fields: Vec<RecordDeclField>,
    pub methods: Vec<FnDecl>,
}

/// A trait declaration (or platform-trait when `is_platform` is true).
#[derive(Debug, Clone, PartialEq)]
pub struct TraitDecl {
    pub id: NodeId,
    pub span: Span,
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub is_platform: bool,
    pub name: Ident,
    pub generic_params: Vec<GenericParam>,
    /// Supertraits: `trait Name: Super1, Super2`.
    pub supertraits: Vec<TypePath>,
    pub associated_types: Vec<AssociatedType>,
    pub methods: Vec<FnDecl>,
}

/// An associated type inside a trait declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct AssociatedType {
    pub id: NodeId,
    pub span: Span,
    pub name: Ident,
    pub bounds: Vec<TypePath>,
}

/// A type assignment inside an impl block: `type Output = Int`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeAssignment {
    pub id: NodeId,
    pub span: Span,
    pub name: Ident,
    pub type_expr: TypeExpr,
}

/// An `impl Trait for Type` or `impl Type` block.
#[derive(Debug, Clone, PartialEq)]
pub struct ImplBlock {
    pub id: NodeId,
    pub span: Span,
    pub annotations: Vec<Annotation>,
    pub generic_params: Vec<GenericParam>,
    /// The trait being implemented, if any.
    pub trait_path: Option<TypePath>,
    /// The type being implemented.
    pub target: TypeExpr,
    pub where_clause: Vec<TypeConstraint>,
    /// Associated type assignments: `type Output = Int`.
    pub type_assignments: Vec<TypeAssignment>,
    pub methods: Vec<FnDecl>,
}

/// An algebraic effect declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectDecl {
    pub id: NodeId,
    pub span: Span,
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: Ident,
    pub generic_params: Vec<GenericParam>,
    /// Component effects for composite effects: `effect IO = Log + Clock`.
    pub components: Vec<TypePath>,
    pub operations: Vec<FnDecl>,
}

/// A `type Name = Type where (predicate)` alias.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeAliasDecl {
    pub id: NodeId,
    pub span: Span,
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: Ident,
    pub generic_params: Vec<GenericParam>,
    pub ty: TypeExpr,
    pub where_clause: Vec<TypeConstraint>,
}

/// A `const NAME: Type = value` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstDecl {
    pub id: NodeId,
    pub span: Span,
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    pub name: Ident,
    pub ty: TypeExpr,
    pub value: Expr,
}

/// A module-level `handle Effect with handler` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleHandleDecl {
    pub id: NodeId,
    pub span: Span,
    /// The effect being handled.
    pub effect: TypePath,
    /// The handler expression.
    pub handler: Expr,
}

/// A `property("name") { forall(...) { ... } }` property-based test declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyTestDecl {
    pub id: NodeId,
    pub span: Span,
    /// The test description string.
    pub name: String,
    /// `forall` bindings.
    pub bindings: Vec<PropertyBinding>,
    pub body: Block,
}

// ─── Imports ─────────────────────────────────────────────────────────────────

/// An import declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportDecl {
    pub id: NodeId,
    pub span: Span,
    pub visibility: Visibility,
    pub path: ModulePath,
    pub items: ImportItems,
}

/// The items selected from an import.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportItems {
    /// `import Foo` — import the module itself.
    Module,
    /// `import Foo.{ A, B }` — import specific names.
    Named(Vec<ImportedName>),
    /// `import Foo.*` — glob import.
    Glob,
}

/// One named item in an import list.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedName {
    pub span: Span,
    pub name: Ident,
    /// `import Foo.{ Bar as Baz }` — rename.
    pub alias: Option<Ident>,
}

// ─── Top-level items ──────────────────────────────────────────────────────────

/// A top-level item in a source file.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Fn(FnDecl),
    Record(RecordDecl),
    Enum(EnumDecl),
    Class(ClassDecl),
    Trait(TraitDecl),
    PlatformTrait(TraitDecl),
    Impl(ImplBlock),
    Effect(EffectDecl),
    TypeAlias(TypeAliasDecl),
    Const(ConstDecl),
    /// Module-level `handle Effect with handler`.
    ModuleHandle(ModuleHandleDecl),
    /// `property("name") { forall(...) { ... } }`.
    PropertyTest(PropertyTestDecl),
    /// Error recovery node: wraps tokens that could not be parsed.
    Error {
        id: NodeId,
        span: Span,
    },
}

impl Item {
    /// Returns the primary [`Span`] of this item.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Item::Fn(d) => d.span,
            Item::Record(d) => d.span,
            Item::Enum(d) => d.span,
            Item::Class(d) => d.span,
            Item::Trait(d) | Item::PlatformTrait(d) => d.span,
            Item::Impl(d) => d.span,
            Item::Effect(d) => d.span,
            Item::TypeAlias(d) => d.span,
            Item::Const(d) => d.span,
            Item::ModuleHandle(d) => d.span,
            Item::PropertyTest(d) => d.span,
            Item::Error { span, .. } => *span,
        }
    }
}

// ─── Module (root) ────────────────────────────────────────────────────────────

/// The root AST node for a single Bock source file.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub id: NodeId,
    pub span: Span,
    /// Module-level doc comments.
    pub doc: Vec<String>,
    /// Optional `module Foo.Bar` declaration.
    pub path: Option<ModulePath>,
    pub imports: Vec<ImportDecl>,
    pub items: Vec<Item>,
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

    #[test]
    fn module_is_debug() {
        let m = Module {
            id: 0,
            span: dummy_span(),
            doc: vec![],
            path: None,
            imports: vec![],
            items: vec![],
        };
        let s = format!("{m:?}");
        assert!(s.contains("Module"));
    }

    #[test]
    fn item_fn_span() {
        let fn_decl = FnDecl {
            id: 1,
            span: Span {
                file: FileId(1),
                start: 5,
                end: 20,
            },
            annotations: vec![],
            visibility: Visibility::Public,
            is_async: false,
            name: dummy_ident("foo"),
            generic_params: vec![],
            params: vec![],
            return_type: None,
            effect_clause: vec![],
            where_clause: vec![],
            body: Some(Block {
                id: 2,
                span: Span {
                    file: FileId(1),
                    start: 10,
                    end: 20,
                },
                stmts: vec![],
                tail: None,
            }),
        };
        let item = Item::Fn(fn_decl);
        assert_eq!(item.span().start, 5);
    }

    #[test]
    fn item_module_handle_and_property_test_exist() {
        let mh = Item::ModuleHandle(ModuleHandleDecl {
            id: 10,
            span: dummy_span(),
            effect: TypePath {
                segments: vec![dummy_ident("Log")],
                span: dummy_span(),
            },
            handler: Expr::Identifier {
                id: 11,
                span: dummy_span(),
                name: dummy_ident("console_log"),
            },
        });
        let pt = Item::PropertyTest(PropertyTestDecl {
            id: 20,
            span: dummy_span(),
            name: "addition is commutative".into(),
            bindings: vec![],
            body: Block {
                id: 21,
                span: dummy_span(),
                stmts: vec![],
                tail: None,
            },
        });
        assert!(format!("{mh:?}").contains("ModuleHandle"));
        assert!(format!("{pt:?}").contains("PropertyTest"));
    }

    #[test]
    fn expr_node_id_and_span() {
        let span = Span {
            file: FileId(1),
            start: 3,
            end: 7,
        };
        let e = Expr::Literal {
            id: 42,
            span,
            lit: Literal::Int("42".into()),
        };
        assert_eq!(e.node_id(), 42);
        assert_eq!(e.span(), span);
    }

    #[test]
    fn pattern_wildcard_debug() {
        let p = Pattern::Wildcard {
            id: 0,
            span: dummy_span(),
        };
        assert!(format!("{p:?}").contains("Wildcard"));
    }

    #[test]
    fn type_expr_optional_debug() {
        let inner = TypeExpr::Named {
            id: 0,
            span: dummy_span(),
            path: TypePath {
                segments: vec![dummy_ident("Int")],
                span: dummy_span(),
            },
            args: vec![],
        };
        let opt = TypeExpr::Optional {
            id: 1,
            span: dummy_span(),
            inner: Box::new(inner),
        };
        assert!(format!("{opt:?}").contains("Optional"));
    }

    #[test]
    fn visibility_default_is_private() {
        assert_eq!(Visibility::default(), Visibility::Private);
    }

    #[test]
    fn enum_variant_kinds() {
        let unit = EnumVariant::Unit {
            id: 0,
            span: dummy_span(),
            name: dummy_ident("A"),
        };
        let strukt = EnumVariant::Struct {
            id: 1,
            span: dummy_span(),
            name: dummy_ident("B"),
            fields: vec![],
        };
        let tuple = EnumVariant::Tuple {
            id: 2,
            span: dummy_span(),
            name: dummy_ident("C"),
            tys: vec![],
        };
        assert!(format!("{unit:?}").contains("Unit"));
        assert!(format!("{strukt:?}").contains("Struct"));
        assert!(format!("{tuple:?}").contains("Tuple"));
    }

    #[test]
    fn all_expr_variants_have_span() {
        let span = dummy_span();
        let exprs: Vec<Expr> = vec![
            Expr::Literal {
                id: 0,
                span,
                lit: Literal::Bool(true),
            },
            Expr::Identifier {
                id: 1,
                span,
                name: dummy_ident("x"),
            },
            Expr::Continue { id: 2, span },
            Expr::Unreachable { id: 3, span },
            Expr::Placeholder { id: 4, span },
        ];
        for e in &exprs {
            assert_eq!(e.span(), span);
        }
    }
}
