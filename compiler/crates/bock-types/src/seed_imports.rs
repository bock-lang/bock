//! Seeds the type checker with type information from imported modules.
//!
//! After name resolution resolves cross-file imports, the type checker needs
//! the full type definitions (record fields, enum variants, trait methods, etc.)
//! from the [`bock_air::registry::ModuleRegistry`] so that imported types participate in type
//! checking.
//!
//! The [`seed_imports`] function reads the AST's import declarations, looks up
//! each imported symbol in the registry, converts [`TypeRef`] strings back to
//! [`Type`] values, and populates the checker's internal tables.

use std::collections::HashMap;

use bock_air::registry::{
    EnumVariantExport, ExportDetail, ExportKind, ExportedSymbol, ModuleRegistry,
};
use bock_air::stubs::TypeRef;
use bock_ast::{ImportDecl, ImportItems};

use crate::checker::TypeChecker;
use crate::exports::{FN_BOUNDS_SEP, IMPL_MARKER_PREFIX, IMPL_MARKER_SEP};
use crate::{EffectRef, FnType, GenericType, NamedType, PrimitiveType, Type, TypeVarId};

// ─── Public API ─────────────────────────────────────────────────────────────

/// Seeds the type checker's environment and internal tables with types from
/// imported modules.
///
/// Called after name resolution but before [`TypeChecker::check_module`].
/// Reads the AST's import declarations and populates the checker's internal
/// tables with the relevant types from the registry, so that imported records,
/// enums, functions, traits, and effects participate fully in type checking.
pub fn seed_imports(checker: &mut TypeChecker, imports: &[ImportDecl], registry: &ModuleRegistry) {
    for import in imports {
        let module_id = module_path_to_id(&import.path);

        match &import.items {
            ImportItems::Named(names) => {
                for imported in names {
                    let local = imported.alias.as_ref().unwrap_or(&imported.name);
                    if let Ok(sym) = registry.resolve_symbol(&module_id, &imported.name.name) {
                        seed_symbol(checker, &local.name, sym);
                    }
                }
            }
            ImportItems::Glob => {
                if let Ok(exports) = registry.resolve_glob(&module_id) {
                    for (name, sym) in exports {
                        seed_symbol(checker, name, sym);
                    }
                }
            }
            ImportItems::Module => {
                // Module-level imports (qualified access) are not yet supported
                // in the type checker. Skip for now.
            }
        }

        // Q-xmod-impl: trait impls are module-scoped, not name-gated — importing
        // a module (in any of the import forms above) makes its trait impls
        // visible for coherent resolution. Scan the whole imported module for
        // its synthetic impl markers regardless of which names were imported.
        seed_imported_impls(checker, &module_id, registry);
    }
}

/// Scans an imported module for its synthetic trait-impl marker symbols
/// (Q-xmod-impl) and records each one on the checker, so it is folded into the
/// impl table in [`TypeChecker::check_module`].
///
/// Trait impls are global/coherent: importing a module brings in *all* of its
/// impls regardless of which value/type names the `use` selected, so this scans
/// the module's full symbol set rather than only the imported names.
fn seed_imported_impls(checker: &mut TypeChecker, module_id: &str, registry: &ModuleRegistry) {
    let Some(exports) = registry.get_module(module_id) else {
        return;
    };
    for (name, sym) in &exports.symbols {
        if !name.starts_with(IMPL_MARKER_PREFIX) {
            continue;
        }
        if let Some((trait_name, trait_args, target)) = decode_impl_marker(&sym.ty.0) {
            checker.register_imported_trait_impl(trait_name, trait_args, target);
        }
    }
}

// ─── Prelude injection (§18.2) ────────────────────────────────────────────────

/// The §18.2 prelude symbols that are **defined in the embedded `core.*`
/// modules** and re-exported into the prelude (Design DQ9: "defined in
/// `core.*`, re-exported into the prelude").
///
/// Each entry is `(module_id, symbol_name)`. Seeding a symbol pulls its full
/// definition (enum variants, trait method signatures, etc.) from the registry
/// via the same path an explicit `use core.<module>.{<symbol>}` would take —
/// so bare `Ordering`, `Comparable`, `Into`, … resolve and type-check without
/// an import.
///
/// The set is intentionally the §18.2 subset whose **definitions live in an
/// embedded core module**. The remaining §18.2 names are handled elsewhere:
/// primitives (`Int`, `Float`, …) are intrinsic; `Optional`/`Some`/`None`,
/// `Result`/`Ok`/`Err`, `List`/`Map`/`Set`, `Fn`, `Duration`/`Instant`, and the
/// utility functions (`print`, `println`, `debug`, `assert`, `todo`,
/// `unreachable`, `sleep`) are seeded as builtins by the CLI's
/// `register_type_builtins`. Traits with no embedded definition yet
/// (`Hashable`, `Serializable`, `Cloneable`, `Default`) are name-level prelude
/// builtins in the resolver only; `Iterator`/`Iterable` are now defined in the
/// embedded `core.iter` module and seeded here (alongside its concrete iterator
/// `ListIterator`), so bare `Iterator`/`Iterable`/`ListIterator` resolve and
/// type-check without an explicit import — this is what the `for`-over-Iterable
/// desugar relies on to recognise an `Iterable` user type.
///
/// Seeding `Ordering` also seeds its variants `Less`/`Equal`/`Greater`
/// (via the enum-detail path), so they need no separate entries.
pub const PRELUDE_FROM_CORE: &[(&str, &str)] = &[
    // core.compare
    ("core.compare", "Ordering"),
    ("core.compare", "Comparable"),
    ("core.compare", "Equatable"),
    // core.convert
    ("core.convert", "Into"),
    ("core.convert", "From"),
    ("core.convert", "TryFrom"),
    ("core.convert", "Displayable"),
    // core.error
    ("core.error", "Error"),
    // core.iter
    ("core.iter", "Iterator"),
    ("core.iter", "Iterable"),
    ("core.iter", "ListIterator"),
];

/// Seeds the §18.2 prelude subset that is **defined in the embedded `core.*`
/// modules** into the type checker, as if every user module began with the
/// corresponding `use core.<module>.{<symbol>}`.
///
/// This makes bare `Ordering`/`Less`/`Comparable`/`Into`/`Error`/… resolve and
/// type-check without an explicit import (Design DQ9). It reuses the exact
/// per-symbol seeding path as [`seed_imports`], so trait method signatures and
/// enum variants are registered identically.
///
/// Symbols are only seeded when their source module is present in `registry`
/// (the embedded core sources are always prepended before user files, so they
/// are present for `check`/`build`/`run`). A user's explicit
/// `use core.<module>.{...}` still works: imports are seeded after this call
/// and simply re-define the same symbol with the same type.
///
/// Call this **after** the CLI's builtin seeding and **before**
/// [`seed_imports`], for every user module.
pub fn seed_prelude(checker: &mut TypeChecker, registry: &ModuleRegistry) {
    for (module_id, name) in PRELUDE_FROM_CORE {
        if let Ok(sym) = registry.resolve_symbol(module_id, name) {
            seed_symbol(checker, name, sym);
        }
    }
    // Q-xmod-impl: also pull any user-declared trait impls from the embedded
    // core modules whose symbols we seed, mirroring the per-module impl scan in
    // `seed_imports`. (Canonical primitive conversions are excluded from export
    // and re-registered locally, so this is a no-op for them but keeps the core
    // path consistent with the user-import path.)
    let mut seen_modules: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (module_id, _name) in PRELUDE_FROM_CORE {
        if seen_modules.insert(module_id) {
            seed_imported_impls(checker, module_id, registry);
        }
    }
}

// ─── Symbol seeding ─────────────────────────────────────────────────────────

/// Seeds a single exported symbol into the type checker.
fn seed_symbol(checker: &mut TypeChecker, local_name: &str, sym: &ExportedSymbol) {
    // Q-xmod-impl: synthetic impl-marker symbols are not real values — they are
    // consumed by `seed_imported_impls`'s per-module scan, never seeded as a
    // value/type here. Skip them so they never land in the env.
    if local_name.starts_with(IMPL_MARKER_PREFIX) {
        return;
    }

    // Q-xmod-bounds: a generic function's `ty` may carry an encoded
    // where-clause-bound suffix (`Fn(?5) -> Bool where ?5: Comparable`). Split
    // it off the base type string before parsing, and decode the bounds so the
    // reconstructed `FnSig` enforces them at call sites.
    let (base_ty_str, fn_bounds) = decode_fn_bounds(&sym.ty.0);
    let ty = type_ref_to_type(&TypeRef(base_ty_str));
    match sym.kind {
        ExportKind::Function => {
            // For generic functions (those whose type contains TypeVars),
            // seed an FnSig so each call site gets fresh instantiation.
            // (FC-28: without this, the first call binds TypeVars permanently.)
            if let Type::Function(ref fn_ty) = ty {
                checker.seed_imported_generic_fn_with_bounds(local_name, fn_ty, &fn_bounds);
            } else {
                checker.env.define(local_name, ty.clone());
            }
        }
        ExportKind::Record => {
            // Register as Named type.
            checker.env.define(
                local_name,
                Type::Named(NamedType {
                    name: local_name.to_string(),
                }),
            );
        }
        ExportKind::Enum => {
            // For enum declarations, `ty` resolves to Named(enum_name) which
            // matches local_name. For individual variant exports, `ty` resolves
            // to the parent enum type (unit/struct) or constructor function
            // (tuple), so we must use `ty` rather than Named(local_name) to
            // ensure variants type-check as their parent enum.
            checker.env.define(local_name, ty);
        }
        ExportKind::Trait => {
            checker.env.define(
                local_name,
                Type::Named(NamedType {
                    name: local_name.to_string(),
                }),
            );
        }
        ExportKind::Effect => {
            checker.env.define(
                local_name,
                Type::Named(NamedType {
                    name: local_name.to_string(),
                }),
            );
        }
        ExportKind::TypeAlias | ExportKind::Constant => {
            checker.env.define(local_name, ty);
        }
    }

    // Populate internal tables from the export detail.
    seed_detail(checker, local_name, &sym.detail);
}

/// Populates the checker's internal tables from an exported symbol's detail.
fn seed_detail(checker: &mut TypeChecker, name: &str, detail: &ExportDetail) {
    match detail {
        ExportDetail::Record {
            fields,
            generic_params,
            methods,
        } => {
            let field_types: Vec<(String, Type)> = fields
                .iter()
                .map(|(n, tr)| (n.clone(), type_ref_to_type(tr)))
                .collect();
            checker.insert_record_field_types(name.to_string(), field_types);

            if !generic_params.is_empty() {
                checker.insert_record_generic_params(name.to_string(), generic_params.clone());
            }

            if !methods.is_empty() {
                let method_types: HashMap<String, Type> = methods
                    .iter()
                    .map(|(n, tr)| (n.clone(), type_ref_to_type(tr)))
                    .collect();
                checker.insert_method_types(name.to_string(), method_types);
            }
        }
        ExportDetail::Enum {
            variants,
            generic_params,
        } => {
            // Store generic params for the enum itself.
            if !generic_params.is_empty() {
                checker.insert_record_generic_params(name.to_string(), generic_params.clone());
            }

            let named_ty = Type::Named(NamedType {
                name: name.to_string(),
            });

            for variant in variants {
                seed_enum_variant(checker, name, &named_ty, generic_params, variant);
            }
        }
        ExportDetail::Trait { methods } => {
            let method_types: HashMap<String, Type> = methods
                .iter()
                .map(|(n, tr)| (n.clone(), type_ref_to_type(tr)))
                .collect();
            if !method_types.is_empty() {
                checker.insert_trait_method_types(name.to_string(), method_types);
            }
        }
        ExportDetail::Effect {
            operations,
            components,
        } => {
            let ops: Vec<(String, Type)> = operations
                .iter()
                .map(|(n, tr)| (n.clone(), type_ref_to_type(tr)))
                .collect();
            checker.insert_effect_op_types(name.to_string(), ops);

            if !components.is_empty() {
                checker.insert_effect_components(name.to_string(), components.clone());
            }
        }
        ExportDetail::TypeAlias { underlying } => {
            checker.insert_type_alias(name.to_string(), type_ref_to_type(underlying));
        }
        ExportDetail::None => {}
    }
}

/// Seeds a single enum variant into the checker.
fn seed_enum_variant(
    checker: &mut TypeChecker,
    _enum_name: &str,
    named_ty: &Type,
    _generic_params: &[String],
    variant: &EnumVariantExport,
) {
    if let Some(ref ctor_type_ref) = variant.constructor_type {
        // Tuple variant — register the constructor function type.
        let ctor_ty = type_ref_to_type(ctor_type_ref);
        checker.env.define(variant.name.clone(), ctor_ty);
    } else if let Some(ref fields) = variant.fields {
        // Struct variant — register as Named type + field types.
        checker.env.define(variant.name.clone(), named_ty.clone());
        let field_types: Vec<(String, Type)> = fields
            .iter()
            .map(|(n, tr)| (n.clone(), type_ref_to_type(tr)))
            .collect();
        checker.insert_record_field_types(variant.name.clone(), field_types);
    } else {
        // Unit variant — register as the enum's Named type.
        checker.env.define(variant.name.clone(), named_ty.clone());
    }
}

// ─── TypeRef → Type conversion ──────────────────────────────────────────────

/// Converts a [`TypeRef`] string back to a [`Type`].
///
/// This is the inverse of [`type_to_type_ref`](crate::exports::type_to_type_ref).
/// It handles the string formats produced by `format_type()` in exports.rs.
#[must_use]
pub fn type_ref_to_type(type_ref: &TypeRef) -> Type {
    parse_type(&type_ref.0)
}

/// Parses a type string into a [`Type`].
fn parse_type(s: &str) -> Type {
    let s = s.trim();

    if s.is_empty() || s == "Error" {
        return Type::Error;
    }

    // Function type: "Fn(...) -> ..." — must be checked BEFORE the '?'
    // suffix so that "Fn(Int) -> String?" parses the '?' as part of the
    // return type (Optional(String)), not as wrapping the whole function
    // type in Optional.  (FC-29)
    if s.starts_with("Fn(") {
        return parse_fn_type(s);
    }

    // Optional type: ends with '?' (but not inside brackets)
    if s.ends_with('?') && !s.contains('[') {
        let inner = &s[..s.len() - 1];
        return Type::Optional(Box::new(parse_type(inner)));
    }

    // Tuple type: "(A, B, ...)"
    if s.starts_with('(') && s.ends_with(')') {
        let inner = &s[1..s.len() - 1];
        let elems = split_top_level(inner, ',');
        if elems.len() > 1 || (elems.len() == 1 && !elems[0].trim().is_empty()) {
            let types: Vec<Type> = elems.iter().map(|e| parse_type(e)).collect();
            return Type::Tuple(types);
        }
    }

    // TypeVar: "?N"
    if let Some(rest) = s.strip_prefix('?') {
        if let Ok(id) = rest.parse::<u32>() {
            return Type::TypeVar(id);
        }
    }

    // Generic type: "Name[A, B, ...]"
    if let Some(bracket_start) = s.find('[') {
        if s.ends_with(']') {
            let constructor = s[..bracket_start].to_string();
            let args_str = &s[bracket_start + 1..s.len() - 1];
            let args: Vec<Type> = split_top_level(args_str, ',')
                .iter()
                .map(|a| parse_type(a))
                .collect();

            // Special case: Result[Ok, Err]
            if constructor == "Result" && args.len() == 2 {
                return Type::Result(Box::new(args[0].clone()), Box::new(args[1].clone()));
            }

            return Type::Generic(GenericType { constructor, args });
        }
    }

    // Primitive type
    if let Some(p) = parse_primitive(s) {
        return Type::Primitive(p);
    }

    // Named type (fallback)
    Type::Named(NamedType {
        name: s.to_string(),
    })
}

/// Parses a function type string: "Fn(A, B) -> R" or "Fn(A, B) -> R with E1 + E2".
fn parse_fn_type(s: &str) -> Type {
    // Find the matching ')' for "Fn("
    let after_fn = &s[3..]; // skip "Fn("
    let paren_end = find_matching_paren(after_fn);

    let params_str = &after_fn[..paren_end];
    let params: Vec<Type> = if params_str.trim().is_empty() {
        vec![]
    } else {
        split_top_level(params_str, ',')
            .iter()
            .map(|p| parse_type(p))
            .collect()
    };

    // After the closing paren, look for " -> "
    let rest = &after_fn[paren_end + 1..]; // skip ')'
    let (ret, effects) = if let Some(arrow_pos) = rest.find("->") {
        let ret_and_effects = rest[arrow_pos + 2..].trim();
        // Check for " with " clause
        if let Some(with_pos) = ret_and_effects.find(" with ") {
            let ret_str = &ret_and_effects[..with_pos];
            let effects_str = &ret_and_effects[with_pos + 6..];
            let effect_refs: Vec<EffectRef> = effects_str
                .split('+')
                .map(|e| EffectRef::new(e.trim().to_string()))
                .collect();
            (parse_type(ret_str), effect_refs)
        } else {
            (parse_type(ret_and_effects), vec![])
        }
    } else {
        (Type::Primitive(PrimitiveType::Void), vec![])
    };

    Type::Function(FnType {
        params,
        ret: Box::new(ret),
        effects,
    })
}

/// Finds the index of the matching closing parenthesis in a string starting
/// after an opening parenthesis.
fn find_matching_paren(s: &str) -> usize {
    let mut depth = 1;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    s.len()
}

/// Splits a string at top-level occurrences of `sep`, respecting nested
/// brackets `[]`, parens `()`, and angle brackets.
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = 0;

    for (i, ch) in s.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' if depth > 0 => {
                depth -= 1;
            }
            c if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Parses a primitive type name string.
fn parse_primitive(s: &str) -> Option<PrimitiveType> {
    match s {
        "Int" => Some(PrimitiveType::Int),
        "Float" => Some(PrimitiveType::Float),
        "Int8" => Some(PrimitiveType::Int8),
        "Int16" => Some(PrimitiveType::Int16),
        "Int32" => Some(PrimitiveType::Int32),
        "Int64" => Some(PrimitiveType::Int64),
        "Int128" => Some(PrimitiveType::Int128),
        "UInt8" => Some(PrimitiveType::UInt8),
        "UInt16" => Some(PrimitiveType::UInt16),
        "UInt32" => Some(PrimitiveType::UInt32),
        "UInt64" => Some(PrimitiveType::UInt64),
        "Float32" => Some(PrimitiveType::Float32),
        "Float64" => Some(PrimitiveType::Float64),
        "BigInt" => Some(PrimitiveType::BigInt),
        "BigFloat" => Some(PrimitiveType::BigFloat),
        "Decimal" => Some(PrimitiveType::Decimal),
        "Bool" => Some(PrimitiveType::Bool),
        "Char" => Some(PrimitiveType::Char),
        "String" => Some(PrimitiveType::String),
        "Byte" => Some(PrimitiveType::Byte),
        "Bytes" => Some(PrimitiveType::Bytes),
        "Void" => Some(PrimitiveType::Void),
        "Never" => Some(PrimitiveType::Never),
        _ => None,
    }
}

// ─── Where-bound / impl-marker decoding (Q-xmod-bounds / Q-xmod-impl) ────────

/// Split a generic function's exported type string into its base type string
/// and its decoded where-clause bounds.
///
/// Inverse of [`crate::exports::encode_fn_bounds`]. Returns
/// `(base_type_string, [(type_var_id, [trait_name, …])])`. When no bound suffix
/// is present the bounds vec is empty and the input is returned verbatim.
///
/// The bound suffix is `<base> where ?<id>: T1 + T2; ?<id2>: T3`. The separator
/// (` where `) cannot occur inside a base function type string (which is
/// `Fn(...) -> R [with E]`), so the first occurrence delimits the suffix.
#[must_use]
pub(crate) fn decode_fn_bounds(s: &str) -> (String, Vec<(TypeVarId, Vec<String>)>) {
    let Some(idx) = s.find(FN_BOUNDS_SEP) else {
        return (s.to_string(), vec![]);
    };
    let base = s[..idx].to_string();
    let suffix = &s[idx + FN_BOUNDS_SEP.len()..];

    let mut bounds: Vec<(TypeVarId, Vec<String>)> = Vec::new();
    for entry in suffix.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // Each entry is `?<id>: T1 + T2`.
        let Some((var_part, traits_part)) = entry.split_once(':') else {
            continue;
        };
        let var_part = var_part.trim();
        let Some(id_str) = var_part.strip_prefix('?') else {
            continue;
        };
        let Ok(id) = id_str.trim().parse::<TypeVarId>() else {
            continue;
        };
        let traits: Vec<String> = traits_part
            .split('+')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if !traits.is_empty() {
            bounds.push((id, traits));
        }
    }
    (base, bounds)
}

/// Decode a synthetic impl-marker `TypeRef` into `(trait_name, trait_args,
/// target)`.
///
/// Inverse of [`crate::exports::encode_impl_marker`]. Returns `None` when the
/// string is not a well-formed marker payload.
#[must_use]
pub(crate) fn decode_impl_marker(s: &str) -> Option<(String, Vec<Type>, Type)> {
    let mut parts = s.split(IMPL_MARKER_SEP);
    let trait_name = parts.next()?.to_string();
    let args_str = parts.next()?;
    let target_str = parts.next()?;
    if trait_name.is_empty() {
        return None;
    }
    let trait_args: Vec<Type> = if args_str.trim().is_empty() {
        vec![]
    } else {
        split_top_level(args_str, ',')
            .iter()
            .map(|a| parse_type(a))
            .collect()
    };
    let target = parse_type(target_str);
    Some((trait_name, trait_args, target))
}

/// Converts a `ModulePath` to a dot-separated string.
fn module_path_to_id(path: &bock_ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_primitive_types() {
        assert_eq!(parse_type("Int"), Type::Primitive(PrimitiveType::Int));
        assert_eq!(parse_type("String"), Type::Primitive(PrimitiveType::String));
        assert_eq!(parse_type("Bool"), Type::Primitive(PrimitiveType::Bool));
        assert_eq!(parse_type("Void"), Type::Primitive(PrimitiveType::Void));
        assert_eq!(parse_type("Never"), Type::Primitive(PrimitiveType::Never));
    }

    #[test]
    fn parse_named_type() {
        assert_eq!(
            parse_type("User"),
            Type::Named(NamedType {
                name: "User".to_string()
            })
        );
    }

    #[test]
    fn parse_generic_type() {
        let ty = parse_type("List[Int]");
        assert_eq!(
            ty,
            Type::Generic(GenericType {
                constructor: "List".to_string(),
                args: vec![Type::Primitive(PrimitiveType::Int)],
            })
        );
    }

    #[test]
    fn parse_nested_generic() {
        let ty = parse_type("Map[String, List[Int]]");
        assert_eq!(
            ty,
            Type::Generic(GenericType {
                constructor: "Map".to_string(),
                args: vec![
                    Type::Primitive(PrimitiveType::String),
                    Type::Generic(GenericType {
                        constructor: "List".to_string(),
                        args: vec![Type::Primitive(PrimitiveType::Int)],
                    }),
                ],
            })
        );
    }

    #[test]
    fn parse_function_type() {
        let ty = parse_type("Fn(String, Int) -> Bool");
        assert_eq!(
            ty,
            Type::Function(FnType {
                params: vec![
                    Type::Primitive(PrimitiveType::String),
                    Type::Primitive(PrimitiveType::Int),
                ],
                ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                effects: vec![],
            })
        );
    }

    #[test]
    fn parse_function_no_params() {
        let ty = parse_type("Fn() -> Void");
        assert_eq!(
            ty,
            Type::Function(FnType {
                params: vec![],
                ret: Box::new(Type::Primitive(PrimitiveType::Void)),
                effects: vec![],
            })
        );
    }

    #[test]
    fn parse_function_with_effects() {
        let ty = parse_type("Fn(String) -> Void with Logger + Console");
        if let Type::Function(ft) = &ty {
            assert_eq!(ft.params.len(), 1);
            assert_eq!(ft.effects.len(), 2);
            assert_eq!(ft.effects[0].name, "Logger");
            assert_eq!(ft.effects[1].name, "Console");
        } else {
            panic!("expected Function type");
        }
    }

    #[test]
    fn parse_optional_type() {
        let ty = parse_type("String?");
        assert_eq!(
            ty,
            Type::Optional(Box::new(Type::Primitive(PrimitiveType::String)))
        );
    }

    #[test]
    fn parse_result_type() {
        let ty = parse_type("Result[String, Error]");
        // Result is parsed as a Generic with constructor "Result"
        // since the format_type produces "Result[Ok, Err]"
        assert!(matches!(ty, Type::Result(_, _)));
    }

    #[test]
    fn parse_error_type() {
        assert_eq!(parse_type("Error"), Type::Error);
        assert_eq!(parse_type(""), Type::Error);
    }

    #[test]
    fn parse_type_var() {
        assert_eq!(parse_type("?42"), Type::TypeVar(42));
    }

    #[test]
    fn roundtrip_primitive() {
        use crate::exports::type_to_type_ref;
        let ty = Type::Primitive(PrimitiveType::Int);
        let tr = type_to_type_ref(&ty);
        assert_eq!(type_ref_to_type(&tr), ty);
    }

    #[test]
    fn roundtrip_named() {
        use crate::exports::type_to_type_ref;
        let ty = Type::Named(NamedType {
            name: "User".to_string(),
        });
        let tr = type_to_type_ref(&ty);
        assert_eq!(type_ref_to_type(&tr), ty);
    }

    #[test]
    fn roundtrip_function() {
        use crate::exports::type_to_type_ref;
        let ty = Type::Function(FnType {
            params: vec![
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::Int),
            ],
            ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
            effects: vec![],
        });
        let tr = type_to_type_ref(&ty);
        assert_eq!(type_ref_to_type(&tr), ty);
    }

    #[test]
    fn roundtrip_generic() {
        use crate::exports::type_to_type_ref;
        let ty = Type::Generic(GenericType {
            constructor: "List".to_string(),
            args: vec![Type::Primitive(PrimitiveType::Int)],
        });
        let tr = type_to_type_ref(&ty);
        assert_eq!(type_ref_to_type(&tr), ty);
    }

    /// FC-29: "Fn(Int) -> String?" must parse as Fn(Int) -> Optional(String),
    /// not Optional(Fn(Int) -> String).
    #[test]
    fn parse_fn_with_optional_return() {
        let ty = parse_type("Fn(Int) -> String?");
        assert_eq!(
            ty,
            Type::Function(FnType {
                params: vec![Type::Primitive(PrimitiveType::Int)],
                ret: Box::new(Type::Optional(Box::new(Type::Primitive(
                    PrimitiveType::String
                )))),
                effects: vec![],
            })
        );
    }

    /// FC-29: roundtrip for function returning Optional.
    #[test]
    fn roundtrip_fn_optional_return() {
        use crate::exports::type_to_type_ref;
        let ty = Type::Function(FnType {
            params: vec![Type::Primitive(PrimitiveType::Int)],
            ret: Box::new(Type::Optional(Box::new(Type::Primitive(
                PrimitiveType::String,
            )))),
            effects: vec![],
        });
        let tr = type_to_type_ref(&ty);
        assert_eq!(tr.0, "Fn(Int) -> String?");
        assert_eq!(type_ref_to_type(&tr), ty);
    }

    // ── Q-xmod-bounds: where-bound encode/decode roundtrip ──────────────────

    #[test]
    fn decode_fn_bounds_no_suffix_is_identity() {
        let (base, bounds) = decode_fn_bounds("Fn(?5) -> Bool");
        assert_eq!(base, "Fn(?5) -> Bool");
        assert!(bounds.is_empty());
    }

    #[test]
    fn fn_bounds_roundtrip_single() {
        use crate::exports::encode_fn_bounds;
        let encoded = encode_fn_bounds("Fn(?5) -> Bool", &[(5, vec!["Comparable".into()])]);
        assert_eq!(encoded, "Fn(?5) -> Bool where ?5: Comparable");
        let (base, bounds) = decode_fn_bounds(&encoded);
        assert_eq!(base, "Fn(?5) -> Bool");
        assert_eq!(bounds, vec![(5u32, vec!["Comparable".to_string()])]);
    }

    #[test]
    fn fn_bounds_roundtrip_multi_param_multi_trait() {
        use crate::exports::encode_fn_bounds;
        let encoded = encode_fn_bounds(
            "Fn(?5, ?6) -> Void",
            &[
                (5, vec!["Comparable".into(), "Displayable".into()]),
                (6, vec!["Into".into()]),
            ],
        );
        let (base, bounds) = decode_fn_bounds(&encoded);
        assert_eq!(base, "Fn(?5, ?6) -> Void");
        assert_eq!(
            bounds,
            vec![
                (
                    5u32,
                    vec!["Comparable".to_string(), "Displayable".to_string()]
                ),
                (6u32, vec!["Into".to_string()]),
            ]
        );
    }

    /// A dotted trait path (`a.b.Trait`) must survive the bound encoding so the
    /// reconstructed `TypeConstraint` keeps its segments.
    #[test]
    fn fn_bounds_roundtrip_dotted_trait() {
        use crate::exports::encode_fn_bounds;
        let encoded = encode_fn_bounds(
            "Fn(?0) -> ?0",
            &[(0, vec!["core.compare.Comparable".into()])],
        );
        let (_base, bounds) = decode_fn_bounds(&encoded);
        assert_eq!(
            bounds,
            vec![(0u32, vec!["core.compare.Comparable".to_string()])]
        );
    }

    // ── Q-xmod-impl: impl-marker encode/decode roundtrip ────────────────────

    #[test]
    fn impl_marker_roundtrip_parameterized() {
        use crate::exports::encode_impl_marker;
        let celsius = Type::Named(NamedType {
            name: "Celsius".into(),
        });
        let fahr = Type::Named(NamedType {
            name: "Fahrenheit".into(),
        });
        let encoded = encode_impl_marker("From", std::slice::from_ref(&celsius), &fahr);
        let (trait_name, args, target) = decode_impl_marker(&encoded).expect("well-formed marker");
        assert_eq!(trait_name, "From");
        assert_eq!(args, vec![celsius]);
        assert_eq!(target, fahr);
    }

    #[test]
    fn impl_marker_roundtrip_plain_trait() {
        use crate::exports::encode_impl_marker;
        let widget = Type::Named(NamedType {
            name: "Widget".into(),
        });
        let encoded = encode_impl_marker("Show", &[], &widget);
        let (trait_name, args, target) = decode_impl_marker(&encoded).expect("well-formed marker");
        assert_eq!(trait_name, "Show");
        assert!(args.is_empty());
        assert_eq!(target, widget);
    }

    #[test]
    fn impl_marker_roundtrip_generic_arg() {
        use crate::exports::encode_impl_marker;
        let list_int = Type::Generic(GenericType {
            constructor: "List".into(),
            args: vec![Type::Primitive(PrimitiveType::Int)],
        });
        let target = Type::Named(NamedType { name: "Bag".into() });
        let encoded = encode_impl_marker("From", std::slice::from_ref(&list_int), &target);
        let (trait_name, args, decoded_target) =
            decode_impl_marker(&encoded).expect("well-formed marker");
        assert_eq!(trait_name, "From");
        assert_eq!(args, vec![list_int]);
        assert_eq!(decoded_target, target);
    }

    #[test]
    fn impl_marker_malformed_returns_none() {
        assert!(decode_impl_marker("not-a-marker").is_none());
    }
}
