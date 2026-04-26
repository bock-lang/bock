//! Export collection pass — extracts public declarations into the [`ModuleRegistry`].
//!
//! After a module is fully compiled (resolve → lower → check → analyze),
//! [`collect_exports`] scans its top-level AIR items and the [`TypeChecker`]'s
//! internal tables to build a [`ModuleExports`] entry for the
//! [`ModuleRegistry`].
//!
//! # Type Representation
//!
//! The registry uses [`TypeRef`] (a lightweight string handle) rather than the
//! full [`Type`] algebra.  The [`type_to_type_ref`] function bridges the two
//! representations.

use std::collections::HashMap;
use std::path::Path;

use bock_air::node::{AIRNode, EnumVariantPayload, NodeKind};
use bock_air::registry::{
    EnumVariantExport, ExportDetail, ExportKind, ExportedSymbol, ModuleExports,
};
use bock_air::TypeRef;
use bock_ast::ImportItems;

use crate::checker::TypeChecker;
use crate::{FnType, GenericType, NamedType, PrimitiveType, Type};

// ─── Type → TypeRef conversion ──────────────────────────────────────────────

/// Converts a [`Type`] into a [`TypeRef`] string handle.
///
/// The string representation is human-readable and round-trippable for
/// display purposes, though the registry treats it as an opaque key.
#[must_use]
pub fn type_to_type_ref(ty: &Type) -> TypeRef {
    TypeRef(format_type(ty))
}

/// Formats a [`Type`] into its string representation.
fn format_type(ty: &Type) -> String {
    match ty {
        Type::Primitive(p) => format_primitive(p),
        Type::Named(NamedType { name }) => name.clone(),
        Type::Generic(GenericType { constructor, args }) => {
            let arg_strs: Vec<String> = args.iter().map(format_type).collect();
            format!("{constructor}[{}]", arg_strs.join(", "))
        }
        Type::Tuple(elems) => {
            let elem_strs: Vec<String> = elems.iter().map(format_type).collect();
            format!("({})", elem_strs.join(", "))
        }
        Type::Function(FnType {
            params,
            ret,
            effects,
        }) => {
            let param_strs: Vec<String> = params.iter().map(format_type).collect();
            let ret_str = format_type(ret);
            let base = format!("Fn({}) -> {}", param_strs.join(", "), ret_str);
            if effects.is_empty() {
                base
            } else {
                let eff_strs: Vec<&str> = effects.iter().map(|e| e.name.as_str()).collect();
                format!("{base} with {}", eff_strs.join(" + "))
            }
        }
        Type::Optional(inner) => {
            let inner_str = format_type(inner);
            format!("{inner_str}?")
        }
        Type::Result(ok, err) => {
            format!("Result[{}, {}]", format_type(ok), format_type(err))
        }
        Type::TypeVar(id) => format!("?{id}"),
        Type::Refined(base, pred) => {
            format!("{} where {}", format_type(base), pred.source)
        }
        Type::Flexible(_) => "Flexible".to_string(),
        Type::Error => "Error".to_string(),
    }
}

/// Formats a primitive type name.
fn format_primitive(p: &PrimitiveType) -> String {
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
    .to_string()
}

// ─── Export collection ──────────────────────────────────────────────────────

/// Collects exports from a fully-compiled module.
///
/// Walks the module's top-level AIR items and queries the [`TypeChecker`]
/// to extract type information, building a [`ModuleExports`] suitable for
/// registration in the [`ModuleRegistry`].
///
/// Both `Public` and `Internal` declarations are exported. `Private`
/// declarations are registered but flagged as non-public (the registry
/// enforces visibility on lookup).
#[must_use]
pub fn collect_exports(
    module_id: &str,
    source_path: &Path,
    checker: &TypeChecker,
    air_module: &AIRNode,
) -> ModuleExports {
    let mut exports = ModuleExports::new(module_id, source_path.display().to_string());

    let items = match &air_module.kind {
        NodeKind::Module { items, .. } => items,
        _ => return exports,
    };

    for item in items {
        match &item.kind {
            NodeKind::FnDecl {
                visibility, name, ..
            } => {
                let ty = checker
                    .env
                    .lookup(&name.name)
                    .cloned()
                    .unwrap_or(Type::Error);

                exports.add_symbol(
                    name.name.clone(),
                    ExportedSymbol {
                        kind: ExportKind::Function,
                        visibility: *visibility,
                        ty: type_to_type_ref(&ty),
                        detail: ExportDetail::None,
                    },
                );
            }

            NodeKind::RecordDecl {
                visibility,
                name,
                generic_params,
                ..
            } => {
                let record_name = &name.name;
                let fields = checker
                    .record_field_types()
                    .get(record_name)
                    .map(|fs| {
                        fs.iter()
                            .map(|(n, t)| (n.clone(), type_to_type_ref(t)))
                            .collect()
                    })
                    .unwrap_or_default();

                let gp_names: Vec<String> = generic_params
                    .iter()
                    .map(|g| g.name.name.clone())
                    .collect();

                let methods = checker
                    .method_types()
                    .get(record_name)
                    .map(|ms| {
                        ms.iter()
                            .map(|(n, t)| (n.clone(), type_to_type_ref(t)))
                            .collect()
                    })
                    .unwrap_or_default();

                exports.add_symbol(
                    record_name.clone(),
                    ExportedSymbol {
                        kind: ExportKind::Record,
                        visibility: *visibility,
                        ty: TypeRef(record_name.clone()),
                        detail: ExportDetail::Record {
                            fields,
                            generic_params: gp_names,
                            methods,
                        },
                    },
                );
            }

            NodeKind::EnumDecl {
                visibility,
                name,
                generic_params,
                variants,
                ..
            } => {
                let enum_name = &name.name;
                let gp_names: Vec<String> = generic_params
                    .iter()
                    .map(|g| g.name.name.clone())
                    .collect();

                let variant_exports: Vec<EnumVariantExport> = variants
                    .iter()
                    .filter_map(|v| {
                        if let NodeKind::EnumVariant { name: vname, payload } = &v.kind {
                            Some(collect_enum_variant(vname.name.as_str(), payload, checker))
                        } else {
                            None
                        }
                    })
                    .collect();

                exports.add_symbol(
                    enum_name.clone(),
                    ExportedSymbol {
                        kind: ExportKind::Enum,
                        visibility: *visibility,
                        ty: TypeRef(enum_name.clone()),
                        detail: ExportDetail::Enum {
                            variants: variant_exports.clone(),
                            generic_params: gp_names,
                        },
                    },
                );

                // Also export each variant as an individual symbol so that
                // `use Module.{VariantName}` works for cross-file imports.
                for ve in &variant_exports {
                    let variant_ty = if let Some(ref ctor) = ve.constructor_type {
                        ctor.clone()
                    } else {
                        TypeRef(enum_name.clone())
                    };
                    exports.add_symbol(
                        ve.name.clone(),
                        ExportedSymbol {
                            kind: ExportKind::Enum,
                            visibility: *visibility,
                            ty: variant_ty,
                            detail: ExportDetail::None,
                        },
                    );
                }
            }

            NodeKind::TraitDecl {
                visibility, name, ..
            } => {
                let trait_name = &name.name;
                let methods: HashMap<String, TypeRef> = checker
                    .trait_method_types()
                    .get(trait_name)
                    .map(|ms| {
                        ms.iter()
                            .map(|(n, t)| (n.clone(), type_to_type_ref(t)))
                            .collect()
                    })
                    .unwrap_or_default();

                exports.add_symbol(
                    trait_name.clone(),
                    ExportedSymbol {
                        kind: ExportKind::Trait,
                        visibility: *visibility,
                        ty: TypeRef(trait_name.clone()),
                        detail: ExportDetail::Trait { methods },
                    },
                );
            }

            NodeKind::EffectDecl {
                visibility,
                name,
                ..
            } => {
                let effect_name = &name.name;
                let operations: Vec<(String, TypeRef)> = checker
                    .effect_op_types()
                    .get(effect_name)
                    .map(|ops| {
                        ops.iter()
                            .map(|(n, t)| (n.clone(), type_to_type_ref(t)))
                            .collect()
                    })
                    .unwrap_or_default();

                let components: Vec<String> = checker
                    .effect_components()
                    .get(effect_name)
                    .cloned()
                    .unwrap_or_default();

                exports.add_symbol(
                    effect_name.clone(),
                    ExportedSymbol {
                        kind: ExportKind::Effect,
                        visibility: *visibility,
                        ty: TypeRef(effect_name.clone()),
                        detail: ExportDetail::Effect {
                            operations,
                            components,
                        },
                    },
                );
            }

            NodeKind::TypeAlias {
                visibility, name, ..
            } => {
                let alias_name = &name.name;
                let underlying = checker
                    .type_aliases()
                    .get(alias_name)
                    .map(type_to_type_ref)
                    .unwrap_or_else(|| TypeRef("Error".to_string()));

                exports.add_symbol(
                    alias_name.clone(),
                    ExportedSymbol {
                        kind: ExportKind::TypeAlias,
                        visibility: *visibility,
                        ty: TypeRef(alias_name.clone()),
                        detail: ExportDetail::TypeAlias { underlying },
                    },
                );
            }

            NodeKind::ConstDecl {
                visibility, name, ..
            } => {
                let const_name = &name.name;
                let ty = checker
                    .env
                    .lookup(const_name)
                    .cloned()
                    .unwrap_or(Type::Error);

                exports.add_symbol(
                    const_name.clone(),
                    ExportedSymbol {
                        kind: ExportKind::Constant,
                        visibility: *visibility,
                        ty: type_to_type_ref(&ty),
                        detail: ExportDetail::None,
                    },
                );
            }

            // Import declarations with non-Private visibility are re-exports.
            NodeKind::ImportDecl { .. } => {
                collect_reexports(item, &mut exports);
            }

            _ => {}
        }
    }

    exports
}

/// Extracts variant export info from an enum variant node.
fn collect_enum_variant(
    name: &str,
    payload: &EnumVariantPayload,
    checker: &TypeChecker,
) -> EnumVariantExport {
    match payload {
        EnumVariantPayload::Unit => EnumVariantExport {
            name: name.to_string(),
            constructor_type: None,
            fields: None,
        },
        EnumVariantPayload::Tuple(_) => {
            // Look up the constructor function type from env.
            let ctor_ty = checker.env.lookup(name).map(type_to_type_ref);
            EnumVariantExport {
                name: name.to_string(),
                constructor_type: ctor_ty,
                fields: None,
            }
        }
        EnumVariantPayload::Struct(fields_decl) => {
            // Use record_field_types which stores struct variant fields
            // under the variant name.
            let fields = checker
                .record_field_types()
                .get(name)
                .map(|fs| {
                    fs.iter()
                        .map(|(n, t)| (n.clone(), type_to_type_ref(t)))
                        .collect()
                })
                .unwrap_or_else(|| {
                    // Fallback: extract field names from the AIR node (types unknown).
                    fields_decl
                        .iter()
                        .map(|f| (f.name.name.clone(), TypeRef("Error".to_string())))
                        .collect()
                });
            EnumVariantExport {
                name: name.to_string(),
                constructor_type: None,
                fields: Some(fields),
            }
        }
    }
}

/// Detects re-exports from import declarations with non-Private visibility.
///
/// `public use app.models.{User, Role}` re-exports User and Role.
fn collect_reexports(item: &AIRNode, _exports: &mut ModuleExports) {
    if let NodeKind::ImportDecl { path, items } = &item.kind {
        let module_path: String = path
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".");

        match items {
            ImportItems::Named(names) => {
                for imported in names {
                    // In Bock, import visibility is determined by the item's
                    // declaration context. For re-exports, the import itself
                    // carries the visibility. Since the AST ImportedName
                    // doesn't carry visibility, we skip re-export detection
                    // from import nodes for now — this will be handled when
                    // the parser/AST gains visibility on imports.
                    let local_name = imported
                        .alias
                        .as_ref()
                        .unwrap_or(&imported.name)
                        .name
                        .clone();
                    let original_name = imported.name.name.clone();
                    // For now, record as potential re-export info but don't
                    // activate until the AST supports `public use`.
                    let _ = (local_name, module_path.clone(), original_name);
                }
            }
            ImportItems::Glob | ImportItems::Module => {
                // Glob/module re-exports are not supported in the first implementation.
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::node::{AIRNode, EnumVariantPayload, NodeIdGen, NodeKind};
    use bock_air::registry::{ExportDetail, ExportKind};
    use bock_ast::{GenericParam, Ident, RecordDeclField, TypeExpr, TypePath, Visibility};
    use bock_errors::Span;

    fn dummy_span() -> Span {
        Span {
            file: bock_errors::FileId(0),
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

    fn make_gen() -> NodeIdGen {
        NodeIdGen::new()
    }

    fn make_type_node(gen: &NodeIdGen, name: &str) -> AIRNode {
        AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::TypeNamed {
                path: TypePath {
                    segments: vec![dummy_ident(name)],
                    span: dummy_span(),
                },
                args: vec![],
            },
        )
    }

    fn make_param_node(gen: &NodeIdGen, param_name: &str, type_name: &str) -> AIRNode {
        let pattern = Box::new(AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::BindPat {
                name: dummy_ident(param_name),
                is_mut: false,
            },
        ));
        let ty = Some(Box::new(make_type_node(gen, type_name)));
        AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Param {
                pattern,
                ty,
                default: None,
            },
        )
    }

    fn make_empty_block(gen: &NodeIdGen) -> Box<AIRNode> {
        Box::new(AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        ))
    }

    fn make_type_expr(gen: &NodeIdGen, name: &str) -> TypeExpr {
        TypeExpr::Named {
            id: gen.next(),
            span: dummy_span(),
            path: TypePath {
                segments: vec![dummy_ident(name)],
                span: dummy_span(),
            },
            args: vec![],
        }
    }

    fn make_record_field(gen: &NodeIdGen, name: &str, type_name: &str) -> RecordDeclField {
        RecordDeclField {
            id: gen.next(),
            span: dummy_span(),
            name: dummy_ident(name),
            ty: make_type_expr(gen, type_name),
            default: None,
        }
    }

    fn make_generic_param(gen: &NodeIdGen, name: &str) -> GenericParam {
        GenericParam {
            id: gen.next(),
            span: dummy_span(),
            name: dummy_ident(name),
            bounds: vec![],
        }
    }

    // ── type_to_type_ref tests ──────────────────────────────────────────

    #[test]
    fn type_ref_primitive() {
        assert_eq!(
            type_to_type_ref(&Type::Primitive(PrimitiveType::Int)),
            TypeRef("Int".to_string())
        );
        assert_eq!(
            type_to_type_ref(&Type::Primitive(PrimitiveType::String)),
            TypeRef("String".to_string())
        );
    }

    #[test]
    fn type_ref_named() {
        let ty = Type::Named(NamedType {
            name: "User".to_string(),
        });
        assert_eq!(type_to_type_ref(&ty), TypeRef("User".to_string()));
    }

    #[test]
    fn type_ref_generic() {
        let ty = Type::Generic(GenericType {
            constructor: "List".to_string(),
            args: vec![Type::Primitive(PrimitiveType::Int)],
        });
        assert_eq!(type_to_type_ref(&ty), TypeRef("List[Int]".to_string()));
    }

    #[test]
    fn type_ref_function() {
        let ty = Type::Function(FnType {
            params: vec![
                Type::Primitive(PrimitiveType::Int),
                Type::Primitive(PrimitiveType::String),
            ],
            ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
            effects: vec![],
        });
        assert_eq!(
            type_to_type_ref(&ty),
            TypeRef("Fn(Int, String) -> Bool".to_string())
        );
    }

    #[test]
    fn type_ref_function_with_effects() {
        use bock_air::stubs::EffectRef;

        let ty = Type::Function(FnType {
            params: vec![Type::Primitive(PrimitiveType::String)],
            ret: Box::new(Type::Primitive(PrimitiveType::Void)),
            effects: vec![EffectRef::new("Logger".to_string())],
        });
        assert_eq!(
            type_to_type_ref(&ty),
            TypeRef("Fn(String) -> Void with Logger".to_string())
        );
    }

    #[test]
    fn type_ref_optional() {
        let ty = Type::Optional(Box::new(Type::Primitive(PrimitiveType::Int)));
        assert_eq!(type_to_type_ref(&ty), TypeRef("Int?".to_string()));
    }

    #[test]
    fn type_ref_result() {
        let ty = Type::Result(
            Box::new(Type::Primitive(PrimitiveType::Int)),
            Box::new(Type::Primitive(PrimitiveType::String)),
        );
        assert_eq!(
            type_to_type_ref(&ty),
            TypeRef("Result[Int, String]".to_string())
        );
    }

    #[test]
    fn type_ref_tuple() {
        let ty = Type::Tuple(vec![
            Type::Primitive(PrimitiveType::Int),
            Type::Primitive(PrimitiveType::Bool),
        ]);
        assert_eq!(type_to_type_ref(&ty), TypeRef("(Int, Bool)".to_string()));
    }

    // ── collect_exports integration tests ───────────────────────────────

    #[test]
    fn collect_public_function() {
        let gen = make_gen();

        let fn_decl = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: dummy_ident("greet"),
                generic_params: vec![],
                params: vec![make_param_node(&gen, "name", "String")],
                return_type: Some(Box::new(make_type_node(&gen, "String"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: make_empty_block(&gen),
            },
        );

        let module = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![fn_decl],
            },
        );

        let mut air_module = module;
        let mut checker = TypeChecker::new();
        checker.check_module(&mut air_module);

        let exports =
            collect_exports("app.greet", Path::new("src/greet.bock"), &checker, &air_module);

        assert_eq!(exports.module_id, "app.greet");
        let sym = exports.symbols.get("greet").expect("greet should be exported");
        assert_eq!(sym.kind, ExportKind::Function);
        assert_eq!(sym.visibility, Visibility::Public);
        assert_eq!(sym.ty.0, "Fn(String) -> String");
    }

    #[test]
    fn collect_public_record() {
        let gen = make_gen();

        let record_decl = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: dummy_ident("User"),
                generic_params: vec![],
                fields: vec![
                    make_record_field(&gen, "id", "Int"),
                    make_record_field(&gen, "name", "String"),
                ],
            },
        );

        let module = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![record_decl],
            },
        );

        let mut air_module = module;
        let mut checker = TypeChecker::new();
        checker.check_module(&mut air_module);

        let exports =
            collect_exports("app.models", Path::new("models.bock"), &checker, &air_module);

        let sym = exports.symbols.get("User").expect("User should be exported");
        assert_eq!(sym.kind, ExportKind::Record);
        assert_eq!(sym.visibility, Visibility::Public);
        match &sym.detail {
            ExportDetail::Record {
                fields,
                generic_params,
                ..
            } => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].0, "id");
                assert_eq!(fields[0].1 .0, "Int");
                assert_eq!(fields[1].0, "name");
                assert_eq!(fields[1].1 .0, "String");
                assert!(generic_params.is_empty());
            }
            _ => panic!("expected Record detail"),
        }
    }

    #[test]
    fn collect_public_enum() {
        let gen = make_gen();

        let enum_decl = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::EnumDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: dummy_ident("Role"),
                generic_params: vec![],
                variants: vec![
                    AIRNode::new(
                        gen.next(),
                        dummy_span(),
                        NodeKind::EnumVariant {
                            name: dummy_ident("Admin"),
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                    AIRNode::new(
                        gen.next(),
                        dummy_span(),
                        NodeKind::EnumVariant {
                            name: dummy_ident("Member"),
                            payload: EnumVariantPayload::Tuple(vec![make_type_node(&gen, "Int")]),
                        },
                    ),
                ],
            },
        );

        let module = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![enum_decl],
            },
        );

        let mut air_module = module;
        let mut checker = TypeChecker::new();
        checker.check_module(&mut air_module);

        let exports =
            collect_exports("app.roles", Path::new("roles.bock"), &checker, &air_module);

        let sym = exports.symbols.get("Role").expect("Role should be exported");
        assert_eq!(sym.kind, ExportKind::Enum);
        match &sym.detail {
            ExportDetail::Enum {
                variants,
                generic_params,
            } => {
                assert_eq!(variants.len(), 2);
                assert_eq!(variants[0].name, "Admin");
                assert!(variants[0].constructor_type.is_none());
                assert_eq!(variants[1].name, "Member");
                assert!(variants[1].constructor_type.is_some());
                assert!(generic_params.is_empty());
            }
            _ => panic!("expected Enum detail"),
        }
    }

    #[test]
    fn collect_public_trait() {
        let gen = make_gen();

        // Build: trait Displayable { fn display(self) -> String }
        let self_param = {
            let pattern = Box::new(AIRNode::new(
                gen.next(),
                dummy_span(),
                NodeKind::BindPat {
                    name: dummy_ident("self"),
                    is_mut: false,
                },
            ));
            AIRNode::new(
                gen.next(),
                dummy_span(),
                NodeKind::Param {
                    pattern,
                    ty: None,
                    default: None,
                },
            )
        };

        let method = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: dummy_ident("display"),
                generic_params: vec![],
                params: vec![self_param],
                return_type: Some(Box::new(make_type_node(&gen, "String"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: make_empty_block(&gen),
            },
        );

        let trait_decl = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: dummy_ident("Displayable"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![method],
            },
        );

        let module = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![trait_decl],
            },
        );

        let mut air_module = module;
        let mut checker = TypeChecker::new();
        checker.check_module(&mut air_module);

        let exports =
            collect_exports("app.traits", Path::new("traits.bock"), &checker, &air_module);

        let sym = exports
            .symbols
            .get("Displayable")
            .expect("trait should be exported");
        assert_eq!(sym.kind, ExportKind::Trait);
        match &sym.detail {
            ExportDetail::Trait { methods } => {
                assert!(methods.contains_key("display"));
            }
            _ => panic!("expected Trait detail"),
        }
    }

    #[test]
    fn collect_public_effect() {
        let gen = make_gen();

        // Build: effect Logger { fn log(msg: String) -> Void }
        let op = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: dummy_ident("log"),
                generic_params: vec![],
                params: vec![make_param_node(&gen, "msg", "String")],
                return_type: Some(Box::new(make_type_node(&gen, "Void"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: make_empty_block(&gen),
            },
        );

        let effect_decl = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: dummy_ident("Logger"),
                generic_params: vec![],
                components: vec![],
                operations: vec![op],
            },
        );

        let module = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![effect_decl],
            },
        );

        let mut air_module = module;
        let mut checker = TypeChecker::new();
        checker.check_module(&mut air_module);

        let exports =
            collect_exports("app.effects", Path::new("effects.bock"), &checker, &air_module);

        let sym = exports
            .symbols
            .get("Logger")
            .expect("effect should be exported");
        assert_eq!(sym.kind, ExportKind::Effect);
        match &sym.detail {
            ExportDetail::Effect {
                operations,
                components,
            } => {
                assert_eq!(operations.len(), 1);
                assert_eq!(operations[0].0, "log");
                assert!(components.is_empty());
            }
            _ => panic!("expected Effect detail"),
        }
    }

    #[test]
    fn private_declarations_included_with_visibility() {
        let gen = make_gen();

        let fn_decl = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: dummy_ident("helper"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: make_empty_block(&gen),
            },
        );

        let module = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![fn_decl],
            },
        );

        let mut air_module = module;
        let mut checker = TypeChecker::new();
        checker.check_module(&mut air_module);

        let exports =
            collect_exports("app.util", Path::new("util.bock"), &checker, &air_module);

        let sym = exports
            .symbols
            .get("helper")
            .expect("helper should be registered");
        assert_eq!(sym.visibility, Visibility::Private);
        assert_eq!(sym.kind, ExportKind::Function);
    }

    #[test]
    fn collect_mixed_visibility_module() {
        let gen = make_gen();

        let public_fn = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: dummy_ident("public_fn"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: make_empty_block(&gen),
            },
        );

        let internal_fn = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Internal,
                is_async: false,
                name: dummy_ident("internal_fn"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: make_empty_block(&gen),
            },
        );

        let private_fn = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: dummy_ident("private_fn"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: make_empty_block(&gen),
            },
        );

        let module = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![public_fn, internal_fn, private_fn],
            },
        );

        let mut air_module = module;
        let mut checker = TypeChecker::new();
        checker.check_module(&mut air_module);

        let exports =
            collect_exports("app.mixed", Path::new("mixed.bock"), &checker, &air_module);

        assert_eq!(exports.symbols.len(), 3);
        assert_eq!(exports.symbols["public_fn"].visibility, Visibility::Public);
        assert_eq!(
            exports.symbols["internal_fn"].visibility,
            Visibility::Internal
        );
        assert_eq!(
            exports.symbols["private_fn"].visibility,
            Visibility::Private
        );
    }

    #[test]
    fn collect_generic_record() {
        let gen = make_gen();

        let record_decl = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: dummy_ident("Pair"),
                generic_params: vec![
                    make_generic_param(&gen, "A"),
                    make_generic_param(&gen, "B"),
                ],
                fields: vec![
                    make_record_field(&gen, "first", "A"),
                    make_record_field(&gen, "second", "B"),
                ],
            },
        );

        let module = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![record_decl],
            },
        );

        let mut air_module = module;
        let mut checker = TypeChecker::new();
        checker.check_module(&mut air_module);

        let exports =
            collect_exports("app.generics", Path::new("generics.bock"), &checker, &air_module);

        let sym = exports.symbols.get("Pair").expect("Pair should be exported");
        assert_eq!(sym.kind, ExportKind::Record);
        match &sym.detail {
            ExportDetail::Record {
                fields,
                generic_params,
                ..
            } => {
                assert_eq!(fields.len(), 2);
                assert_eq!(generic_params, &["A", "B"]);
            }
            _ => panic!("expected Record detail"),
        }
    }

    #[test]
    fn collect_exports_registers_in_registry() {
        use bock_air::registry::ModuleRegistry;

        let gen = make_gen();

        let fn_decl = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: dummy_ident("process"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: make_empty_block(&gen),
            },
        );

        let module = AIRNode::new(
            gen.next(),
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![fn_decl],
            },
        );

        let mut air_module = module;
        let mut checker = TypeChecker::new();
        checker.check_module(&mut air_module);

        let module_exports =
            collect_exports("app.process", Path::new("process.bock"), &checker, &air_module);

        // Register in the ModuleRegistry and verify it's queryable.
        let mut registry = ModuleRegistry::new();
        registry.register(module_exports);

        assert!(registry.has_module("app.process"));
        let sym = registry
            .resolve_symbol("app.process", "process")
            .expect("should resolve");
        assert_eq!(sym.kind, ExportKind::Function);
    }
}
