//! Bock types — type system definitions, inference engine, and trait resolution.
//!
//! This crate defines the internal type representation used by all later
//! compiler passes (type checker, interpreter, code generation). It provides:
//!
//! - [`Type`] — the main type algebra
//! - [`Substitution`] — a type-variable-to-type mapping with path compression
//! - [`unify`] — Hindley-Milner unification with occurs check
//! - [`TypeChecker`] — bidirectional type inference engine (T-AIR pass)

use std::collections::HashMap;

pub use bock_air::stubs::EffectRef;

pub mod checker;
pub use checker::{TypeChecker, TypeEnv};

pub mod traits;
pub use traits::{
    check_supertrait_obligations, resolve_impl, resolve_method, ImplId, ImplTable, ResolvedMethod,
    TraitRef,
};

pub mod ownership;
pub use ownership::{analyze_ownership, AIRModule, OwnershipInfo, OwnershipState};

pub mod effects;
pub use effects::{infer_effects, track_effects, Strictness};

pub mod capabilities;
pub use capabilities::{compute_capabilities, verify_capabilities, CapabilitySet};

pub mod exports;
pub use exports::{collect_exports, type_to_type_ref};

pub mod seed_imports;
pub use seed_imports::seed_imports;

pub mod vocab;

// ─── Primitive types ──────────────────────────────────────────────────────────

/// The set of primitive (built-in scalar) types in Bock.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PrimitiveType {
    // Unsized integer / float
    Int,
    Float,
    // Sized integers
    Int8,
    Int16,
    Int32,
    Int64,
    Int128,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    // Sized floats
    Float32,
    Float64,
    // Arbitrary precision
    BigInt,
    BigFloat,
    Decimal,
    // Other scalars
    Bool,
    Char,
    String,
    Byte,
    Bytes,
    // Unit / bottom
    Void,
    Never,
}

// ─── Named (user-defined) types ───────────────────────────────────────────────

/// A user-defined named type (record, enum, class).
///
/// The `name` is the fully-qualified identifier, e.g. `"Std.Http.Request"`.
/// The `args` field carries any type arguments already applied, e.g. for a
/// generic instantiation `List[Int]` the outer [`Type::Generic`] is used, but
/// for a non-generic named type `args` is empty.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedType {
    /// Fully-qualified name of the type.
    pub name: String,
}

// ─── Generic types ────────────────────────────────────────────────────────────

/// A generic type application: a named type constructor applied to type args.
///
/// Examples: `List[Int]`, `Map[String, Int]`.
#[derive(Debug, Clone, PartialEq)]
pub struct GenericType {
    /// The type constructor name (e.g. `"List"`, `"Map"`).
    pub constructor: String,
    /// Type arguments (in order).
    pub args: Vec<Type>,
}

// ─── Function types ───────────────────────────────────────────────────────────

/// A function type: parameter types, return type, and algebraic-effect set.
#[derive(Debug, Clone, PartialEq)]
pub struct FnType {
    /// Types of the positional parameters.
    pub params: Vec<Type>,
    /// Return type.
    pub ret: Box<Type>,
    /// Algebraic effects this function may perform.
    pub effects: Vec<EffectRef>,
}

// ─── Refined types ────────────────────────────────────────────────────────────

/// A predicate expression in a refined type.
///
/// This is intentionally kept as a simple string representation for now; later
/// passes can elaborate it into a full expression AST.
#[derive(Debug, Clone, PartialEq)]
pub struct Predicate {
    /// Human-readable source of the predicate, e.g. `"1 <= self <= 65535"`.
    pub source: String,
}

// ─── Flexible (sketch-mode) types ─────────────────────────────────────────────

/// Structural constraints for a flexible (sketch-mode) type.
///
/// In sketch mode Bock infers wide types structurally, narrowing by usage.
/// This is a placeholder structure; later passes fill in the constraint set.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StructuralConstraints {
    /// Required field names and their types (may be `Type::TypeVar`).
    pub fields: Vec<(String, Type)>,
}

// ─── Type variable identifier ─────────────────────────────────────────────────

/// Unique identifier for a type-inference variable.
pub type TypeVarId = u32;

// ─── Main type algebra ────────────────────────────────────────────────────────

/// The type of a Bock value.
///
/// This enum covers all type-level constructs in the Bock language spec:
/// primitives, user-defined names, generics, tuples, function types, optional,
/// result, inference variables, refined types, flexible types, and an error
/// sentinel for error recovery.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A built-in primitive scalar type.
    Primitive(PrimitiveType),
    /// A user-defined named type (record, enum, class).
    Named(NamedType),
    /// A generic type application: `List[T]`, `Map[K, V]`.
    Generic(GenericType),
    /// A tuple type: `(A, B, C)`.
    Tuple(Vec<Type>),
    /// A function type: `Fn(Int, Int) -> Int with Log`.
    Function(FnType),
    /// An optional type: `T?` / `Optional[T]`.
    Optional(Box<Type>),
    /// A result type: `Result[T, E]`.
    Result(Box<Type>, Box<Type>),
    /// A type-inference variable.
    TypeVar(TypeVarId),
    /// A refined type: base type + predicate.
    Refined(Box<Type>, Predicate),
    /// A flexible (sketch-mode) type with structural constraints.
    Flexible(StructuralConstraints),
    /// Poison type — used during error recovery. Unifies with anything.
    Error,
}

// ─── TypeError ────────────────────────────────────────────────────────────────

/// An error produced by type unification.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeError {
    /// Two types are not unifiable.
    Mismatch {
        /// The first type.
        left: Type,
        /// The second type.
        right: Type,
    },
    /// An occurs-check failure: binding a type variable would create an
    /// infinite type, e.g. `T = List[T]`.
    OccursCheck {
        /// The type variable that would become infinite.
        var: TypeVarId,
        /// The type that contains `var`.
        ty: Type,
    },
    /// Tuple arity mismatch.
    TupleArity { expected: usize, found: usize },
    /// Function arity mismatch.
    FnArity { expected: usize, found: usize },
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeError::Mismatch { left, right } => {
                write!(f, "type mismatch: {left:?} vs {right:?}")
            }
            TypeError::OccursCheck { var, ty } => {
                write!(f, "occurs check failed: ?{var} in {ty:?}")
            }
            TypeError::TupleArity { expected, found } => {
                write!(
                    f,
                    "tuple arity mismatch: expected {expected}, found {found}"
                )
            }
            TypeError::FnArity { expected, found } => {
                write!(
                    f,
                    "function arity mismatch: expected {expected}, found {found}"
                )
            }
        }
    }
}

impl std::error::Error for TypeError {}

// ─── Substitution ─────────────────────────────────────────────────────────────

/// A partial map from [`TypeVarId`]s to [`Type`]s.
///
/// Supports path compression: when looking up a chain of variable bindings the
/// lookup walks to the final concrete type (or unbound variable).
#[derive(Debug, Clone, Default)]
pub struct Substitution {
    map: HashMap<TypeVarId, Type>,
}

impl Substitution {
    /// Create an empty substitution.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a type variable, following chains of variable bindings until
    /// a concrete type or an unbound variable is reached (path compression is
    /// applied eagerly for direct variable-to-variable chains).
    #[must_use]
    pub fn lookup(&self, mut id: TypeVarId) -> Type {
        loop {
            match self.map.get(&id) {
                None => return Type::TypeVar(id),
                Some(Type::TypeVar(next)) => {
                    id = *next;
                }
                Some(ty) => return ty.clone(),
            }
        }
    }

    /// Bind a type variable to a type.
    ///
    /// Panics in debug builds if `id` is already bound (callers should check
    /// via [`lookup`](Self::lookup) before binding).
    pub fn bind(&mut self, id: TypeVarId, ty: Type) {
        debug_assert!(
            !self.map.contains_key(&id),
            "TypeVar ?{id} is already bound"
        );
        self.map.insert(id, ty);
    }

    /// Apply this substitution to a type, recursively replacing all type
    /// variables that are bound.
    #[must_use]
    pub fn apply(&self, ty: &Type) -> Type {
        match ty {
            Type::TypeVar(id) => {
                let resolved = self.lookup(*id);
                if resolved == *ty {
                    resolved
                } else {
                    self.apply(&resolved)
                }
            }
            Type::Primitive(_) | Type::Error => ty.clone(),
            Type::Named(_) => ty.clone(),
            Type::Generic(g) => Type::Generic(GenericType {
                constructor: g.constructor.clone(),
                args: g.args.iter().map(|a| self.apply(a)).collect(),
            }),
            Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| self.apply(e)).collect()),
            Type::Function(f) => Type::Function(FnType {
                params: f.params.iter().map(|p| self.apply(p)).collect(),
                ret: Box::new(self.apply(&f.ret)),
                effects: f.effects.clone(),
            }),
            Type::Optional(inner) => Type::Optional(Box::new(self.apply(inner))),
            Type::Result(ok, err) => {
                Type::Result(Box::new(self.apply(ok)), Box::new(self.apply(err)))
            }
            Type::Refined(base, pred) => Type::Refined(Box::new(self.apply(base)), pred.clone()),
            Type::Flexible(constraints) => Type::Flexible(StructuralConstraints {
                fields: constraints
                    .fields
                    .iter()
                    .map(|(name, ty)| (name.clone(), self.apply(ty)))
                    .collect(),
            }),
        }
    }

    /// Returns `true` if the type variable `id` is unbound in this substitution.
    #[must_use]
    pub fn is_unbound(&self, id: TypeVarId) -> bool {
        matches!(self.lookup(id), Type::TypeVar(_))
    }
}

// ─── Occurs check ─────────────────────────────────────────────────────────────

/// Returns `true` if type variable `id` appears free in `ty` under the given
/// substitution. Used to prevent binding a variable to a type that contains it.
fn occurs(id: TypeVarId, ty: &Type, subst: &Substitution) -> bool {
    match ty {
        Type::TypeVar(other) => {
            let resolved = subst.lookup(*other);
            match resolved {
                Type::TypeVar(rid) => rid == id,
                _ => occurs(id, &resolved, subst),
            }
        }
        Type::Primitive(_) | Type::Named(_) | Type::Error => false,
        Type::Generic(g) => g.args.iter().any(|a| occurs(id, a, subst)),
        Type::Tuple(elems) => elems.iter().any(|e| occurs(id, e, subst)),
        Type::Function(f) => {
            f.params.iter().any(|p| occurs(id, p, subst)) || occurs(id, &f.ret, subst)
        }
        Type::Optional(inner) => occurs(id, inner, subst),
        Type::Result(ok, err) => occurs(id, ok, subst) || occurs(id, err, subst),
        Type::Refined(base, _) => occurs(id, base, subst),
        Type::Flexible(c) => c.fields.iter().any(|(_, t)| occurs(id, t, subst)),
    }
}

// ─── Unification ──────────────────────────────────────────────────────────────

/// Unify two types under the given substitution, extending the substitution
/// in place when a type variable is bound.
///
/// Follows standard Hindley-Milner rules:
/// - `Type::Error` unifies with anything (poison — prevents cascading errors).
/// - Type variables are bound after an occurs check.
/// - Structural types are unified component-wise.
///
/// # Errors
///
/// Returns a [`TypeError`] if the types are not unifiable.
pub fn unify(a: &Type, b: &Type, subst: &mut Substitution) -> Result<(), TypeError> {
    // Resolve variables before matching.
    let a = subst.apply(a);
    let b = subst.apply(b);

    match (&a, &b) {
        // Error is a poison type that unifies with anything.
        (Type::Error, _) | (_, Type::Error) => Ok(()),

        // Never is the bottom type — it unifies with anything.
        (Type::Primitive(PrimitiveType::Never), _)
        | (_, Type::Primitive(PrimitiveType::Never)) => Ok(()),

        // Two identical types trivially unify.
        _ if a == b => Ok(()),

        // TypeVar vs anything: occurs check, then bind.
        (Type::TypeVar(id), other) | (other, Type::TypeVar(id)) => {
            let id = *id;
            if occurs(id, other, subst) {
                return Err(TypeError::OccursCheck {
                    var: id,
                    ty: other.clone(),
                });
            }
            subst.bind(id, other.clone());
            Ok(())
        }

        // Primitive vs primitive: already handled by the `a == b` case.
        // Named vs named: same.
        // Structural cases:
        (Type::Optional(a_inner), Type::Optional(b_inner)) => unify(a_inner, b_inner, subst),

        (Type::Result(a_ok, a_err), Type::Result(b_ok, b_err)) => {
            unify(a_ok, b_ok, subst)?;
            unify(a_err, b_err, subst)
        }

        (Type::Tuple(a_elems), Type::Tuple(b_elems)) => {
            if a_elems.len() != b_elems.len() {
                return Err(TypeError::TupleArity {
                    expected: a_elems.len(),
                    found: b_elems.len(),
                });
            }
            for (ae, be) in a_elems.iter().zip(b_elems.iter()) {
                unify(ae, be, subst)?;
            }
            Ok(())
        }

        (Type::Function(fa), Type::Function(fb)) => {
            if fa.params.len() != fb.params.len() {
                return Err(TypeError::FnArity {
                    expected: fa.params.len(),
                    found: fb.params.len(),
                });
            }
            for (ap, bp) in fa.params.iter().zip(fb.params.iter()) {
                unify(ap, bp, subst)?;
            }
            unify(&fa.ret, &fb.ret, subst)
        }

        (Type::Generic(ga), Type::Generic(gb)) => {
            if ga.constructor != gb.constructor {
                return Err(TypeError::Mismatch {
                    left: a.clone(),
                    right: b.clone(),
                });
            }
            if ga.args.len() != gb.args.len() {
                return Err(TypeError::Mismatch {
                    left: a.clone(),
                    right: b.clone(),
                });
            }
            for (aa, ba) in ga.args.iter().zip(gb.args.iter()) {
                unify(aa, ba, subst)?;
            }
            Ok(())
        }

        // Refined types: unify the base types (predicates are not unified).
        (Type::Refined(base_a, _), Type::Refined(base_b, _)) => unify(base_a, base_b, subst),

        // Named vs Generic with same constructor: Named("Foo") is the
        // bare (un-parameterized) form of Generic("Foo", [args]).
        // Treat them as compatible so that values typed as Named can
        // flow into contexts expecting the Generic form.
        (Type::Named(nt), Type::Generic(g)) | (Type::Generic(g), Type::Named(nt))
            if nt.name == g.constructor =>
        {
            Ok(())
        }

        // Everything else is a mismatch.
        _ => Err(TypeError::Mismatch {
            left: a.clone(),
            right: b.clone(),
        }),
    }
}

// ─── Type equality helper ─────────────────────────────────────────────────────

/// Check structural equivalence of two types under a substitution.
///
/// Two types are equivalent if [`unify`] succeeds on a fresh scratch
/// substitution that extends the given one (non-destructively).
#[must_use]
pub fn types_equal(a: &Type, b: &Type, subst: &Substitution) -> bool {
    let mut scratch = subst.clone();
    unify(a, b, &mut scratch).is_ok()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn int() -> Type {
        Type::Primitive(PrimitiveType::Int)
    }

    fn bool_ty() -> Type {
        Type::Primitive(PrimitiveType::Bool)
    }

    fn string_ty() -> Type {
        Type::Primitive(PrimitiveType::String)
    }

    fn var(id: TypeVarId) -> Type {
        Type::TypeVar(id)
    }

    // ── Substitution ──────────────────────────────────────────────────────────

    #[test]
    fn subst_lookup_unbound() {
        let s = Substitution::new();
        assert_eq!(s.lookup(0), var(0));
    }

    #[test]
    fn subst_bind_and_lookup() {
        let mut s = Substitution::new();
        s.bind(0, int());
        assert_eq!(s.lookup(0), int());
    }

    #[test]
    fn subst_chain_lookup() {
        let mut s = Substitution::new();
        s.bind(0, var(1));
        s.bind(1, int());
        assert_eq!(s.lookup(0), int());
    }

    #[test]
    fn subst_apply_nested() {
        let mut s = Substitution::new();
        s.bind(0, int());
        let ty = Type::Optional(Box::new(var(0)));
        assert_eq!(s.apply(&ty), Type::Optional(Box::new(int())));
    }

    #[test]
    fn subst_apply_tuple() {
        let mut s = Substitution::new();
        s.bind(0, int());
        s.bind(1, bool_ty());
        let ty = Type::Tuple(vec![var(0), var(1)]);
        assert_eq!(s.apply(&ty), Type::Tuple(vec![int(), bool_ty()]));
    }

    #[test]
    fn subst_apply_function() {
        let mut s = Substitution::new();
        s.bind(0, int());
        s.bind(1, bool_ty());
        let ty = Type::Function(FnType {
            params: vec![var(0)],
            ret: Box::new(var(1)),
            effects: vec![],
        });
        let result = s.apply(&ty);
        assert_eq!(
            result,
            Type::Function(FnType {
                params: vec![int()],
                ret: Box::new(bool_ty()),
                effects: vec![],
            })
        );
    }

    // ── Unification: base cases ───────────────────────────────────────────────

    #[test]
    fn unify_same_primitive() {
        let mut s = Substitution::new();
        assert!(unify(&int(), &int(), &mut s).is_ok());
    }

    #[test]
    fn unify_different_primitives_fails() {
        let mut s = Substitution::new();
        assert!(matches!(
            unify(&int(), &bool_ty(), &mut s),
            Err(TypeError::Mismatch { .. })
        ));
    }

    #[test]
    fn unify_error_with_anything() {
        let mut s = Substitution::new();
        assert!(unify(&Type::Error, &int(), &mut s).is_ok());
        assert!(unify(&bool_ty(), &Type::Error, &mut s).is_ok());
        assert!(unify(&Type::Error, &Type::Error, &mut s).is_ok());
        assert!(unify(&Type::Error, &var(0), &mut s).is_ok());
    }

    #[test]
    fn unify_never_with_anything() {
        let mut s = Substitution::new();
        let never = Type::Primitive(PrimitiveType::Never);
        assert!(unify(&never, &int(), &mut s).is_ok());
        assert!(unify(&bool_ty(), &never, &mut s).is_ok());
        assert!(unify(&never, &never, &mut s).is_ok());
        assert!(unify(&never, &var(10), &mut s).is_ok());
    }

    // ── Unification: type variables ───────────────────────────────────────────

    #[test]
    fn unify_var_with_concrete() {
        let mut s = Substitution::new();
        assert!(unify(&var(0), &int(), &mut s).is_ok());
        assert_eq!(s.lookup(0), int());
    }

    #[test]
    fn unify_concrete_with_var() {
        let mut s = Substitution::new();
        assert!(unify(&int(), &var(0), &mut s).is_ok());
        assert_eq!(s.lookup(0), int());
    }

    #[test]
    fn unify_var_with_var() {
        let mut s = Substitution::new();
        assert!(unify(&var(0), &var(1), &mut s).is_ok());
        // After unification one of the vars points to the other.
        // Applying to ?0 should give either ?1 or ?0 depending on bind direction,
        // but both should be "equal" in the sense that applying subst to a type
        // containing both gives the same result.
        s.bind(1, int());
        assert_eq!(s.lookup(0), int());
    }

    // ── Occurs check ──────────────────────────────────────────────────────────

    #[test]
    fn occurs_check_prevents_infinite_type() {
        let mut s = Substitution::new();
        // T = Optional[T]  →  occurs check failure
        let ty = Type::Optional(Box::new(var(0)));
        assert!(matches!(
            unify(&var(0), &ty, &mut s),
            Err(TypeError::OccursCheck { var: 0, .. })
        ));
    }

    #[test]
    fn occurs_check_list_generic() {
        let mut s = Substitution::new();
        let list_t = Type::Generic(GenericType {
            constructor: "List".into(),
            args: vec![var(0)],
        });
        assert!(matches!(
            unify(&var(0), &list_t, &mut s),
            Err(TypeError::OccursCheck { var: 0, .. })
        ));
    }

    // ── Unification: structural types ─────────────────────────────────────────

    #[test]
    fn unify_optional() {
        let mut s = Substitution::new();
        assert!(unify(
            &Type::Optional(Box::new(var(0))),
            &Type::Optional(Box::new(int())),
            &mut s
        )
        .is_ok());
        assert_eq!(s.lookup(0), int());
    }

    #[test]
    fn unify_result() {
        let mut s = Substitution::new();
        let a = Type::Result(Box::new(var(0)), Box::new(var(1)));
        let b = Type::Result(Box::new(int()), Box::new(string_ty()));
        assert!(unify(&a, &b, &mut s).is_ok());
        assert_eq!(s.lookup(0), int());
        assert_eq!(s.lookup(1), string_ty());
    }

    #[test]
    fn unify_tuple_element_wise() {
        let mut s = Substitution::new();
        let a = Type::Tuple(vec![var(0), var(1)]);
        let b = Type::Tuple(vec![int(), bool_ty()]);
        assert!(unify(&a, &b, &mut s).is_ok());
        assert_eq!(s.lookup(0), int());
        assert_eq!(s.lookup(1), bool_ty());
    }

    #[test]
    fn unify_tuple_arity_mismatch() {
        let mut s = Substitution::new();
        let a = Type::Tuple(vec![int(), bool_ty()]);
        let b = Type::Tuple(vec![int()]);
        assert!(matches!(
            unify(&a, &b, &mut s),
            Err(TypeError::TupleArity {
                expected: 2,
                found: 1
            })
        ));
    }

    #[test]
    fn unify_function_types() {
        let mut s = Substitution::new();
        let a = Type::Function(FnType {
            params: vec![var(0)],
            ret: Box::new(var(1)),
            effects: vec![],
        });
        let b = Type::Function(FnType {
            params: vec![int()],
            ret: Box::new(bool_ty()),
            effects: vec![],
        });
        assert!(unify(&a, &b, &mut s).is_ok());
        assert_eq!(s.lookup(0), int());
        assert_eq!(s.lookup(1), bool_ty());
    }

    #[test]
    fn unify_function_arity_mismatch() {
        let mut s = Substitution::new();
        let a = Type::Function(FnType {
            params: vec![int(), bool_ty()],
            ret: Box::new(int()),
            effects: vec![],
        });
        let b = Type::Function(FnType {
            params: vec![int()],
            ret: Box::new(int()),
            effects: vec![],
        });
        assert!(matches!(
            unify(&a, &b, &mut s),
            Err(TypeError::FnArity {
                expected: 2,
                found: 1
            })
        ));
    }

    #[test]
    fn unify_generic_same_constructor() {
        let mut s = Substitution::new();
        let a = Type::Generic(GenericType {
            constructor: "List".into(),
            args: vec![var(0)],
        });
        let b = Type::Generic(GenericType {
            constructor: "List".into(),
            args: vec![int()],
        });
        assert!(unify(&a, &b, &mut s).is_ok());
        assert_eq!(s.lookup(0), int());
    }

    #[test]
    fn unify_generic_different_constructor_fails() {
        let mut s = Substitution::new();
        let a = Type::Generic(GenericType {
            constructor: "List".into(),
            args: vec![int()],
        });
        let b = Type::Generic(GenericType {
            constructor: "Set".into(),
            args: vec![int()],
        });
        assert!(matches!(
            unify(&a, &b, &mut s),
            Err(TypeError::Mismatch { .. })
        ));
    }

    // ── Refined types ─────────────────────────────────────────────────────────

    #[test]
    fn unify_refined_base_types() {
        let mut s = Substitution::new();
        let a = Type::Refined(
            Box::new(var(0)),
            Predicate {
                source: "self > 0".into(),
            },
        );
        let b = Type::Refined(
            Box::new(int()),
            Predicate {
                source: "self >= 0".into(),
            },
        );
        assert!(unify(&a, &b, &mut s).is_ok());
        assert_eq!(s.lookup(0), int());
    }

    // ── types_equal ───────────────────────────────────────────────────────────

    #[test]
    fn types_equal_same() {
        let s = Substitution::new();
        assert!(types_equal(&int(), &int(), &s));
    }

    #[test]
    fn types_equal_different() {
        let s = Substitution::new();
        assert!(!types_equal(&int(), &bool_ty(), &s));
    }

    #[test]
    fn types_equal_via_subst() {
        let mut s = Substitution::new();
        s.bind(0, int());
        assert!(types_equal(&var(0), &int(), &s));
    }

    // ── All PrimitiveType variants construct ──────────────────────────────────

    #[test]
    fn all_primitive_variants() {
        let prims = [
            PrimitiveType::Int,
            PrimitiveType::Float,
            PrimitiveType::Int8,
            PrimitiveType::Int16,
            PrimitiveType::Int32,
            PrimitiveType::Int64,
            PrimitiveType::Int128,
            PrimitiveType::UInt8,
            PrimitiveType::UInt16,
            PrimitiveType::UInt32,
            PrimitiveType::UInt64,
            PrimitiveType::Float32,
            PrimitiveType::Float64,
            PrimitiveType::BigInt,
            PrimitiveType::BigFloat,
            PrimitiveType::Decimal,
            PrimitiveType::Bool,
            PrimitiveType::Char,
            PrimitiveType::String,
            PrimitiveType::Byte,
            PrimitiveType::Bytes,
            PrimitiveType::Void,
            PrimitiveType::Never,
        ];
        for p in &prims {
            let ty = Type::Primitive(p.clone());
            assert!(matches!(ty, Type::Primitive(_)));
        }
    }
}
