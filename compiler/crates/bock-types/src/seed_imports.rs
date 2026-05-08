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
use crate::{EffectRef, FnType, GenericType, NamedType, PrimitiveType, Type};

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
    }
}

// ─── Symbol seeding ─────────────────────────────────────────────────────────

/// Seeds a single exported symbol into the type checker.
fn seed_symbol(checker: &mut TypeChecker, local_name: &str, sym: &ExportedSymbol) {
    // Define the symbol's type in the checker's environment.
    let ty = type_ref_to_type(&sym.ty);
    match sym.kind {
        ExportKind::Function => {
            // For generic functions (those whose type contains TypeVars),
            // seed an FnSig so each call site gets fresh instantiation.
            // (FC-28: without this, the first call binds TypeVars permanently.)
            if let Type::Function(ref fn_ty) = ty {
                checker.seed_imported_generic_fn(local_name, fn_ty);
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
}
