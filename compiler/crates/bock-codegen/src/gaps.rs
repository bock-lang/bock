//! Capability gap detection — identifies mismatches between AIR constructs and target support.

use bock_air::node::{AIRNode, NodeKind};
use bock_types::AIRModule;

use crate::profile::{Support, TargetProfile};

/// A mismatch between an AIR construct and the target's support level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGap {
    /// The construct that has a gap (e.g., `"algebraic_types"`).
    pub construct: String,
    /// The target's support level for this construct.
    pub target_support: Support,
    /// The suggested synthesis strategy (e.g., `"Tagged objects + switch"`).
    pub synthesis_strategy: String,
}

/// Detects capability gaps between AIR constructs used in a module and the
/// target's capabilities.
///
/// Walks the AIR module to find which language constructs are actually used,
/// then checks each against the target profile's capability matrix. Only
/// constructs that are *used* and *not natively supported* produce gaps.
#[must_use]
pub fn detect_gaps(module: &AIRModule, target: &TargetProfile) -> Vec<CapabilityGap> {
    let usage = collect_construct_usage(module);
    let mut gaps = Vec::new();
    let caps = &target.capabilities;

    if usage.has_enum_decls && caps.algebraic_types != Support::Native {
        gaps.push(CapabilityGap {
            construct: "algebraic_types".into(),
            target_support: caps.algebraic_types,
            synthesis_strategy: match caps.algebraic_types {
                Support::Emulated | Support::SwitchBased | Support::InterfaceBased => {
                    "Tagged objects + switch".into()
                }
                Support::None => "Cannot represent algebraic types".into(),
                Support::Native => unreachable!(),
            },
        });
    }

    if usage.has_match && caps.pattern_matching != Support::Native {
        gaps.push(CapabilityGap {
            construct: "pattern_matching".into(),
            target_support: caps.pattern_matching,
            synthesis_strategy: match caps.pattern_matching {
                Support::SwitchBased | Support::Emulated => "Switch-based dispatch".into(),
                Support::InterfaceBased | Support::None => "if/else chains".into(),
                Support::Native => unreachable!(),
            },
        });
    }

    if usage.has_traits && !matches!(caps.traits, Support::Native | Support::InterfaceBased) {
        gaps.push(CapabilityGap {
            construct: "traits".into(),
            target_support: caps.traits,
            synthesis_strategy: match caps.traits {
                Support::Emulated | Support::SwitchBased => "Duck typing / protocol classes".into(),
                Support::None => "Cannot represent traits".into(),
                Support::Native | Support::InterfaceBased => unreachable!(),
            },
        });
    }

    if usage.has_ownership && caps.memory_model != crate::profile::MemoryModel::Manual {
        gaps.push(CapabilityGap {
            construct: "ownership".into(),
            target_support: Support::Emulated,
            synthesis_strategy: "Erase ownership annotations".into(),
        });
    }

    if usage.has_effects {
        gaps.push(CapabilityGap {
            construct: "effects".into(),
            target_support: Support::Emulated,
            synthesis_strategy: "Parameter passing".into(),
        });
    }

    if usage.has_interpolation && caps.string_interpolation != Support::Native {
        gaps.push(CapabilityGap {
            construct: "string_interpolation".into(),
            target_support: caps.string_interpolation,
            synthesis_strategy: match caps.string_interpolation {
                Support::Emulated => "String concatenation / format macro".into(),
                Support::SwitchBased | Support::InterfaceBased | Support::None => {
                    "String concatenation".into()
                }
                Support::Native => unreachable!(),
            },
        });
    }

    gaps
}

// ─── Construct usage collector ───────────────────────────────────────────────

/// Tracks which AIR constructs are present in a module.
#[derive(Debug, Default)]
struct ConstructUsage {
    has_enum_decls: bool,
    has_match: bool,
    has_traits: bool,
    has_ownership: bool,
    has_effects: bool,
    has_interpolation: bool,
}

/// Walks the AIR tree and records which construct categories are used.
fn collect_construct_usage(module: &AIRModule) -> ConstructUsage {
    let mut usage = ConstructUsage::default();
    visit_node(module, &mut usage);
    usage
}

fn visit_node(node: &AIRNode, usage: &mut ConstructUsage) {
    match &node.kind {
        // Declarations
        NodeKind::EnumDecl { variants, .. } => {
            usage.has_enum_decls = true;
            for v in variants {
                visit_node(v, usage);
            }
        }
        NodeKind::TraitDecl { methods, .. } => {
            usage.has_traits = true;
            for m in methods {
                visit_node(m, usage);
            }
        }
        NodeKind::ImplBlock { methods, .. } => {
            usage.has_traits = true;
            for m in methods {
                visit_node(m, usage);
            }
        }
        NodeKind::EffectDecl { operations, .. } => {
            usage.has_effects = true;
            for op in operations {
                visit_node(op, usage);
            }
        }

        // Control flow
        NodeKind::Match { scrutinee, arms } => {
            usage.has_match = true;
            visit_node(scrutinee, usage);
            for arm in arms {
                visit_node(arm, usage);
            }
        }

        // Ownership
        NodeKind::Move { expr } | NodeKind::Borrow { expr } | NodeKind::MutableBorrow { expr } => {
            usage.has_ownership = true;
            visit_node(expr, usage);
        }

        // Effects
        NodeKind::EffectOp { .. } => {
            usage.has_effects = true;
        }
        NodeKind::HandlingBlock { body, .. } => {
            usage.has_effects = true;
            visit_node(body, usage);
        }

        // String interpolation
        NodeKind::Interpolation { .. } => {
            usage.has_interpolation = true;
        }

        // Recurse into children for compound nodes
        NodeKind::Module { imports, items, .. } => {
            for i in imports {
                visit_node(i, usage);
            }
            for i in items {
                visit_node(i, usage);
            }
        }
        NodeKind::FnDecl {
            params,
            return_type,
            body,
            ..
        } => {
            for p in params {
                visit_node(p, usage);
            }
            if let Some(rt) = return_type {
                visit_node(rt, usage);
            }
            visit_node(body, usage);
        }
        NodeKind::ClassDecl { methods, .. } => {
            for m in methods {
                visit_node(m, usage);
            }
        }
        NodeKind::Block { stmts, tail } => {
            for s in stmts {
                visit_node(s, usage);
            }
            if let Some(t) = tail {
                visit_node(t, usage);
            }
        }
        NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            visit_node(condition, usage);
            visit_node(then_block, usage);
            if let Some(e) = else_block {
                visit_node(e, usage);
            }
        }
        NodeKind::For {
            pattern,
            iterable,
            body,
        } => {
            visit_node(pattern, usage);
            visit_node(iterable, usage);
            visit_node(body, usage);
        }
        NodeKind::While { condition, body } => {
            visit_node(condition, usage);
            visit_node(body, usage);
        }
        NodeKind::Loop { body } => visit_node(body, usage),
        NodeKind::LetBinding {
            pattern, value, ty, ..
        } => {
            visit_node(pattern, usage);
            visit_node(value, usage);
            if let Some(t) = ty {
                visit_node(t, usage);
            }
        }
        NodeKind::BinaryOp { left, right, .. } => {
            visit_node(left, usage);
            visit_node(right, usage);
        }
        NodeKind::UnaryOp { operand, .. } => visit_node(operand, usage),
        NodeKind::Call { callee, args, .. } => {
            visit_node(callee, usage);
            for a in args {
                visit_node(&a.value, usage);
            }
        }
        NodeKind::MethodCall { receiver, args, .. } => {
            visit_node(receiver, usage);
            for a in args {
                visit_node(&a.value, usage);
            }
        }
        NodeKind::Lambda { params, body } => {
            for p in params {
                visit_node(p, usage);
            }
            visit_node(body, usage);
        }
        NodeKind::Return { value } | NodeKind::Break { value } => {
            if let Some(v) = value {
                visit_node(v, usage);
            }
        }
        NodeKind::MatchArm {
            pattern,
            guard,
            body,
        } => {
            visit_node(pattern, usage);
            if let Some(g) = guard {
                visit_node(g, usage);
            }
            visit_node(body, usage);
        }
        NodeKind::Assign { target, value, .. } => {
            visit_node(target, usage);
            visit_node(value, usage);
        }
        NodeKind::FieldAccess { object, .. } => visit_node(object, usage),
        NodeKind::Index { object, index } => {
            visit_node(object, usage);
            visit_node(index, usage);
        }
        NodeKind::Pipe { left, right } | NodeKind::Compose { left, right } => {
            visit_node(left, usage);
            visit_node(right, usage);
        }
        NodeKind::Await { expr } | NodeKind::Propagate { expr } => visit_node(expr, usage),
        NodeKind::Guard {
            let_pattern,
            condition,
            else_block,
        } => {
            if let Some(pat) = let_pattern {
                visit_node(pat, usage);
            }
            visit_node(condition, usage);
            visit_node(else_block, usage);
        }
        NodeKind::Param {
            pattern,
            ty,
            default,
        } => {
            visit_node(pattern, usage);
            if let Some(t) = ty {
                visit_node(t, usage);
            }
            if let Some(d) = default {
                visit_node(d, usage);
            }
        }

        // Leaf nodes — no children to visit
        NodeKind::Literal { .. }
        | NodeKind::Identifier { .. }
        | NodeKind::Continue
        | NodeKind::Placeholder
        | NodeKind::Unreachable
        | NodeKind::WildcardPat
        | NodeKind::BindPat { .. }
        | NodeKind::LiteralPat { .. }
        | NodeKind::RestPat
        | NodeKind::TypeSelf
        | NodeKind::Error
        | NodeKind::ImportDecl { .. }
        | NodeKind::EffectRef { .. } => {}

        // Collection literals
        NodeKind::ListLiteral { elems }
        | NodeKind::SetLiteral { elems }
        | NodeKind::TupleLiteral { elems } => {
            for e in elems {
                visit_node(e, usage);
            }
        }
        NodeKind::MapLiteral { entries } => {
            for e in entries {
                visit_node(&e.key, usage);
                visit_node(&e.value, usage);
            }
        }

        // Remaining composite nodes
        NodeKind::RecordDecl { .. } => {}
        NodeKind::EnumVariant { .. } => {}
        NodeKind::RecordConstruct { fields, spread, .. } => {
            for f in fields {
                if let Some(v) = &f.value {
                    visit_node(v, usage);
                }
            }
            if let Some(s) = spread {
                visit_node(s, usage);
            }
        }
        NodeKind::Range { lo, hi, .. } => {
            visit_node(lo, usage);
            visit_node(hi, usage);
        }
        NodeKind::ResultConstruct { value: Some(v), .. } => {
            visit_node(v, usage);
        }
        NodeKind::TypeNamed { args, .. } => {
            for a in args {
                visit_node(a, usage);
            }
        }
        NodeKind::TypeTuple { elems } => {
            for e in elems {
                visit_node(e, usage);
            }
        }
        NodeKind::TypeFunction { params, ret, .. } => {
            for p in params {
                visit_node(p, usage);
            }
            visit_node(ret, usage);
        }
        NodeKind::TypeOptional { inner } => visit_node(inner, usage),
        NodeKind::TypeAlias { ty, .. } => visit_node(ty, usage),
        NodeKind::ConstDecl { ty, value, .. } => {
            visit_node(ty, usage);
            visit_node(value, usage);
        }
        NodeKind::ModuleHandle { handler, .. } => visit_node(handler, usage),
        NodeKind::PropertyTest { body, .. } => visit_node(body, usage),
        NodeKind::ConstructorPat { fields, .. } => {
            for f in fields {
                visit_node(f, usage);
            }
        }
        NodeKind::RecordPat { fields, .. } => {
            for f in fields {
                if let Some(p) = &f.pattern {
                    visit_node(p, usage);
                }
            }
        }
        NodeKind::TuplePat { elems } => {
            for e in elems {
                visit_node(e, usage);
            }
        }
        NodeKind::ListPat { elems, rest } => {
            for e in elems {
                visit_node(e, usage);
            }
            if let Some(r) = rest {
                visit_node(r, usage);
            }
        }
        NodeKind::OrPat { alternatives } => {
            for a in alternatives {
                visit_node(a, usage);
            }
        }
        NodeKind::GuardPat { pattern, guard } => {
            visit_node(pattern, usage);
            visit_node(guard, usage);
        }
        NodeKind::RangePat { lo, hi, .. } => {
            visit_node(lo, usage);
            visit_node(hi, usage);
        }

        // Catch-all for future NodeKind variants (non_exhaustive)
        _ => {}
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::node::{AIRNode, AirHandlerPair, NodeKind};
    use bock_ast::{Ident, TypePath, Visibility};
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
            name: name.into(),
            span: span(),
        }
    }

    fn node(id: u32, kind: NodeKind) -> AIRNode {
        AIRNode::new(id, span(), kind)
    }

    fn empty_module() -> AIRModule {
        node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![],
            },
        )
    }

    #[test]
    fn empty_module_has_no_gaps() {
        let module = empty_module();
        let gaps = detect_gaps(&module, &TargetProfile::javascript());
        assert!(gaps.is_empty());
    }

    #[test]
    fn enum_decl_detected_as_gap_for_js() {
        let module = node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![node(
                    1,
                    NodeKind::EnumDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        name: ident("Color"),
                        generic_params: vec![],
                        variants: vec![],
                    },
                )],
            },
        );
        let gaps = detect_gaps(&module, &TargetProfile::javascript());
        assert!(gaps.iter().any(|g| g.construct == "algebraic_types"));
    }

    #[test]
    fn enum_decl_no_gap_for_rust() {
        let module = node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![node(
                    1,
                    NodeKind::EnumDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        name: ident("Color"),
                        generic_params: vec![],
                        variants: vec![],
                    },
                )],
            },
        );
        let gaps = detect_gaps(&module, &TargetProfile::rust());
        assert!(!gaps.iter().any(|g| g.construct == "algebraic_types"));
    }

    #[test]
    fn match_expr_gap_for_go() {
        let module = node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![node(
                    1,
                    NodeKind::Match {
                        scrutinee: Box::new(node(2, NodeKind::Identifier { name: ident("x") })),
                        arms: vec![],
                    },
                )],
            },
        );
        let gaps = detect_gaps(&module, &TargetProfile::go());
        let pm_gap = gaps
            .iter()
            .find(|g| g.construct == "pattern_matching")
            .unwrap();
        assert_eq!(pm_gap.target_support, Support::None);
        assert_eq!(pm_gap.synthesis_strategy, "if/else chains");
    }

    #[test]
    fn match_expr_no_gap_for_rust() {
        let module = node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![node(
                    1,
                    NodeKind::Match {
                        scrutinee: Box::new(node(2, NodeKind::Identifier { name: ident("x") })),
                        arms: vec![],
                    },
                )],
            },
        );
        let gaps = detect_gaps(&module, &TargetProfile::rust());
        assert!(!gaps.iter().any(|g| g.construct == "pattern_matching"));
    }

    #[test]
    fn ownership_gap_for_gc_targets() {
        let module = node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![node(
                    1,
                    NodeKind::Move {
                        expr: Box::new(node(2, NodeKind::Identifier { name: ident("x") })),
                    },
                )],
            },
        );
        let gaps = detect_gaps(&module, &TargetProfile::javascript());
        assert!(gaps.iter().any(|g| g.construct == "ownership"));
    }

    #[test]
    fn ownership_no_gap_for_rust() {
        let module = node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![node(
                    1,
                    NodeKind::Move {
                        expr: Box::new(node(2, NodeKind::Identifier { name: ident("x") })),
                    },
                )],
            },
        );
        let gaps = detect_gaps(&module, &TargetProfile::rust());
        assert!(!gaps.iter().any(|g| g.construct == "ownership"));
    }

    #[test]
    fn effects_always_produce_gap() {
        let tp = TypePath {
            segments: vec![ident("Log")],
            span: span(),
        };
        let module = node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![node(
                    1,
                    NodeKind::HandlingBlock {
                        handlers: vec![AirHandlerPair {
                            effect: tp,
                            handler: Box::new(node(2, NodeKind::Identifier { name: ident("h") })),
                        }],
                        body: Box::new(node(
                            3,
                            NodeKind::Block {
                                stmts: vec![],
                                tail: None,
                            },
                        )),
                    },
                )],
            },
        );
        // Effects produce a gap for all targets (even Rust)
        let gaps = detect_gaps(&module, &TargetProfile::rust());
        assert!(gaps.iter().any(|g| g.construct == "effects"));
    }

    #[test]
    fn interpolation_gap_for_rust() {
        let module = node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![node(
                    1,
                    NodeKind::Interpolation {
                        parts: vec![bock_air::node::AirInterpolationPart::Literal(
                            "hello".into(),
                        )],
                    },
                )],
            },
        );
        let gaps = detect_gaps(&module, &TargetProfile::rust());
        assert!(gaps.iter().any(|g| g.construct == "string_interpolation"));
    }

    #[test]
    fn interpolation_no_gap_for_js() {
        let module = node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![node(
                    1,
                    NodeKind::Interpolation {
                        parts: vec![bock_air::node::AirInterpolationPart::Literal(
                            "hello".into(),
                        )],
                    },
                )],
            },
        );
        let gaps = detect_gaps(&module, &TargetProfile::javascript());
        assert!(!gaps.iter().any(|g| g.construct == "string_interpolation"));
    }

    #[test]
    fn multiple_gaps_detected() {
        let tp = TypePath {
            segments: vec![ident("Log")],
            span: span(),
        };
        let module = node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![
                    node(
                        1,
                        NodeKind::EnumDecl {
                            annotations: vec![],
                            visibility: Visibility::Public,
                            name: ident("Color"),
                            generic_params: vec![],
                            variants: vec![],
                        },
                    ),
                    node(
                        2,
                        NodeKind::Match {
                            scrutinee: Box::new(node(3, NodeKind::Identifier { name: ident("x") })),
                            arms: vec![],
                        },
                    ),
                    node(
                        4,
                        NodeKind::EffectOp {
                            effect: tp,
                            operation: ident("log"),
                            args: vec![],
                        },
                    ),
                ],
            },
        );
        let gaps = detect_gaps(&module, &TargetProfile::go());
        assert!(gaps.iter().any(|g| g.construct == "algebraic_types"));
        assert!(gaps.iter().any(|g| g.construct == "pattern_matching"));
        assert!(gaps.iter().any(|g| g.construct == "effects"));
    }
}
