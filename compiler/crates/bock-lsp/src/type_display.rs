//! Human-readable rendering of [`bock_types::Type`] values for hover content.
//!
//! `bock_types` only derives `Debug` on its type algebra, which is too noisy
//! for editor tooltips. This module formats types in Bock surface syntax so
//! users see `Fn(Int) -> String with Logger` rather than
//! `Function(FnType { ... })`.

use bock_types::{FnType, GenericType, NamedType, Predicate, PrimitiveType, Type};

/// Render a [`Type`] as a single-line Bock-syntax string suitable for hover.
#[must_use]
pub fn format_type(ty: &Type) -> String {
    let mut out = String::new();
    write_type(&mut out, ty);
    out
}

fn write_type(out: &mut String, ty: &Type) {
    match ty {
        Type::Primitive(p) => out.push_str(primitive_name(p)),
        Type::Named(NamedType { name }) => out.push_str(name),
        Type::Generic(GenericType { constructor, args }) => {
            out.push_str(constructor);
            out.push('[');
            write_list(out, args);
            out.push(']');
        }
        Type::Tuple(elems) => {
            out.push('(');
            write_list(out, elems);
            out.push(')');
        }
        Type::Function(f) => write_fn(out, f),
        Type::Optional(inner) => {
            write_type(out, inner);
            out.push('?');
        }
        Type::Result(ok, err) => {
            out.push_str("Result[");
            write_type(out, ok);
            out.push_str(", ");
            write_type(out, err);
            out.push(']');
        }
        Type::TypeVar(id) => {
            // Unresolved inference variable — render as `_` (a wildcard) with
            // the id suffixed so multiple distinct vars can still be told
            // apart in a single hover.
            out.push('?');
            out.push_str(&id.to_string());
        }
        Type::Refined(base, Predicate { source }) => {
            write_type(out, base);
            out.push_str(" where ");
            out.push_str(source);
        }
        Type::Flexible(_) => out.push_str("<flexible>"),
        Type::Error => out.push_str("<error>"),
    }
}

fn write_list(out: &mut String, items: &[Type]) {
    let mut first = true;
    for ty in items {
        if !first {
            out.push_str(", ");
        }
        first = false;
        write_type(out, ty);
    }
}

fn write_fn(out: &mut String, f: &FnType) {
    out.push_str("Fn(");
    write_list(out, &f.params);
    out.push_str(") -> ");
    write_type(out, &f.ret);
    if !f.effects.is_empty() {
        out.push_str(" with ");
        let mut first = true;
        for effect in &f.effects {
            if !first {
                out.push_str(", ");
            }
            first = false;
            out.push_str(&effect.name);
        }
    }
}

fn primitive_name(p: &PrimitiveType) -> &'static str {
    match p {
        PrimitiveType::Int => "Int",
        PrimitiveType::Float => "Float",
        PrimitiveType::Int8 => "Int8",
        PrimitiveType::Int16 => "Int16",
        PrimitiveType::Int32 => "Int32",
        PrimitiveType::Int64 => "Int64",
        PrimitiveType::Int128 => "Int128",
        PrimitiveType::UInt8 => "UInt8",
        PrimitiveType::UInt16 => "UInt16",
        PrimitiveType::UInt32 => "UInt32",
        PrimitiveType::UInt64 => "UInt64",
        PrimitiveType::Float32 => "Float32",
        PrimitiveType::Float64 => "Float64",
        PrimitiveType::BigInt => "BigInt",
        PrimitiveType::BigFloat => "BigFloat",
        PrimitiveType::Decimal => "Decimal",
        PrimitiveType::Bool => "Bool",
        PrimitiveType::Char => "Char",
        PrimitiveType::String => "String",
        PrimitiveType::Byte => "Byte",
        PrimitiveType::Bytes => "Bytes",
        PrimitiveType::Void => "Void",
        PrimitiveType::Never => "Never",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_types::{EffectRef, FnType, GenericType, NamedType};

    fn int() -> Type {
        Type::Primitive(PrimitiveType::Int)
    }

    fn string_ty() -> Type {
        Type::Primitive(PrimitiveType::String)
    }

    #[test]
    fn primitive() {
        assert_eq!(format_type(&int()), "Int");
        assert_eq!(format_type(&Type::Primitive(PrimitiveType::Void)), "Void");
    }

    #[test]
    fn named() {
        let ty = Type::Named(NamedType {
            name: "User".into(),
        });
        assert_eq!(format_type(&ty), "User");
    }

    #[test]
    fn generic_list_of_int() {
        let ty = Type::Generic(GenericType {
            constructor: "List".into(),
            args: vec![int()],
        });
        assert_eq!(format_type(&ty), "List[Int]");
    }

    #[test]
    fn generic_map_of_string_int() {
        let ty = Type::Generic(GenericType {
            constructor: "Map".into(),
            args: vec![string_ty(), int()],
        });
        assert_eq!(format_type(&ty), "Map[String, Int]");
    }

    #[test]
    fn tuple_pair() {
        let ty = Type::Tuple(vec![int(), string_ty()]);
        assert_eq!(format_type(&ty), "(Int, String)");
    }

    #[test]
    fn function_no_effects() {
        let ty = Type::Function(FnType {
            params: vec![int(), int()],
            ret: Box::new(int()),
            effects: vec![],
        });
        assert_eq!(format_type(&ty), "Fn(Int, Int) -> Int");
    }

    #[test]
    fn function_with_effects() {
        let ty = Type::Function(FnType {
            params: vec![],
            ret: Box::new(Type::Primitive(PrimitiveType::Void)),
            effects: vec![EffectRef::new("Logger")],
        });
        assert_eq!(format_type(&ty), "Fn() -> Void with Logger");
    }

    #[test]
    fn function_with_multiple_effects() {
        let ty = Type::Function(FnType {
            params: vec![string_ty()],
            ret: Box::new(Type::Primitive(PrimitiveType::Void)),
            effects: vec![EffectRef::new("Log"), EffectRef::new("Clock")],
        });
        assert_eq!(format_type(&ty), "Fn(String) -> Void with Log, Clock");
    }

    #[test]
    fn optional() {
        let ty = Type::Optional(Box::new(int()));
        assert_eq!(format_type(&ty), "Int?");
    }

    #[test]
    fn result_pair() {
        let ty = Type::Result(Box::new(int()), Box::new(string_ty()));
        assert_eq!(format_type(&ty), "Result[Int, String]");
    }

    #[test]
    fn nested_generic_optional() {
        let list_opt = Type::Generic(GenericType {
            constructor: "List".into(),
            args: vec![Type::Optional(Box::new(int()))],
        });
        assert_eq!(format_type(&list_opt), "List[Int?]");
    }

    #[test]
    fn type_var_renders_with_id() {
        assert_eq!(format_type(&Type::TypeVar(7)), "?7");
    }

    #[test]
    fn refined() {
        let ty = Type::Refined(
            Box::new(int()),
            Predicate {
                source: "self > 0".into(),
            },
        );
        assert_eq!(format_type(&ty), "Int where self > 0");
    }

    #[test]
    fn error_type() {
        assert_eq!(format_type(&Type::Error), "<error>");
    }
}
