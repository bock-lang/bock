//! Single-file symbol index shared by the navigation features.
//!
//! One AST walk produces everything go-to-definition, find-references and
//! rename need in order to answer position queries:
//!
//! - declaration name spans keyed by `NodeId` (`def_spans`)
//! - top-level declaration names, including enum variants
//!   (`toplevel_by_name`) — used for type references, which the resolver
//!   does not record per-node resolutions for
//! - every identifier use site (`ident_uses`)
//! - every type-path occurrence: annotations, record constructions,
//!   constructor/record patterns, effect clauses, impl headers
//!   (`type_paths`)
//! - uppercase-initial field accesses such as `Color.Red`, which is how
//!   qualified enum-variant references appear in expression position
//!   (`member_accesses`)
//!
//! The index is intentionally single-file: it never looks across module
//! boundaries, matching the rest of the LSP's per-buffer scope.

use std::collections::{HashMap, HashSet};

use bock_ast::{
    visitor::{
        walk_class_decl, walk_const_decl, walk_effect_decl, walk_enum_decl, walk_expr,
        walk_fn_decl, walk_impl_block, walk_record_decl, walk_trait_decl, walk_type_alias_decl,
        walk_type_expr, Visitor,
    },
    ClassDecl, ConstDecl, EffectDecl, EnumDecl, EnumVariant, Expr, FnDecl, ImplBlock, Item, Module,
    NodeId, Pattern, RecordDecl, RecordPatternField, TraitDecl, TypeAliasDecl, TypeExpr, TypePath,
};
use bock_errors::Span;

/// One type-path occurrence: each segment's name and span, in source order.
pub(crate) type PathOccurrence = Vec<(String, Span)>;

/// Index of declarations and reference sites produced by one AST walk.
#[derive(Default)]
pub(crate) struct SymbolIndex {
    /// Declaration `NodeId` → span of the declared *name*.
    pub(crate) def_spans: HashMap<NodeId, Span>,
    /// Top-level declaration name → (decl `NodeId`, name span). Includes
    /// enum variants, which share the module-level constructor namespace.
    pub(crate) toplevel_by_name: HashMap<String, (NodeId, Span)>,
    /// Every `Expr::Identifier` use site, in visit order.
    pub(crate) ident_uses: Vec<(NodeId, Span)>,
    /// Every type-path occurrence, in visit order.
    pub(crate) type_paths: Vec<PathOccurrence>,
    /// Uppercase-initial field accesses (`Color.Red`): the member's name
    /// and span. In expression position these are how qualified enum
    /// variant references parse, so rename/references must consider them.
    pub(crate) member_accesses: Vec<(String, Span)>,
    /// Declarations whose use sites the resolver cannot see: impl/trait/
    /// class methods (invoked via `MethodCall`) and record/class fields
    /// (accessed via `FieldAccess`). Rename refuses these rather than
    /// producing an incomplete edit.
    pub(crate) opaque_member_ids: HashSet<NodeId>,
    /// `NodeId`s of top-level type-like declarations (records, enums,
    /// classes, traits, effects, type aliases). Only these are eligible
    /// for name-based type-path matching.
    pub(crate) type_decl_ids: HashSet<NodeId>,
    /// `NodeId`s of enum variants. Only these are eligible for name-based
    /// `member_accesses` matching.
    pub(crate) variant_ids: HashSet<NodeId>,
}

impl SymbolIndex {
    /// Walk `module` once and build the full index.
    #[must_use]
    pub(crate) fn build(module: &Module) -> Self {
        let mut index = SymbolIndex::default();
        index.collect_toplevel_names(module);
        let mut walker = Indexer {
            index: &mut index,
            member_depth: 0,
        };
        walker.visit_module(module);
        index
    }

    /// The innermost identifier use site containing `offset`, if any.
    pub(crate) fn ident_use_at(&self, offset: usize) -> Option<(NodeId, Span)> {
        let mut best: Option<(NodeId, Span)> = None;
        let mut best_width = usize::MAX;
        for &(id, span) in &self.ident_uses {
            if span_contains(span, offset) {
                let width = span.end.saturating_sub(span.start);
                if width < best_width {
                    best_width = width;
                    best = Some((id, span));
                }
            }
        }
        best
    }

    /// The declaration whose *name span* contains `offset`, if any.
    pub(crate) fn decl_name_at(&self, offset: usize) -> Option<(NodeId, Span)> {
        let mut best: Option<(NodeId, Span)> = None;
        let mut best_width = usize::MAX;
        for (&id, &span) in &self.def_spans {
            if span_contains(span, offset) {
                let width = span.end.saturating_sub(span.start);
                if width < best_width {
                    best_width = width;
                    best = Some((id, span));
                }
            }
        }
        best
    }

    /// The innermost type-path segment containing `offset`, if any.
    pub(crate) fn type_segment_at(&self, offset: usize) -> Option<(&str, Span)> {
        let mut best: Option<(&str, Span)> = None;
        let mut best_width = usize::MAX;
        for path in &self.type_paths {
            for (name, span) in path {
                if span_contains(*span, offset) {
                    let width = span.end.saturating_sub(span.start);
                    if width < best_width {
                        best_width = width;
                        best = Some((name.as_str(), *span));
                    }
                }
            }
        }
        best
    }

    /// Pre-pass: index every top-level named declaration (and enum
    /// variant) by name so type references can resolve by string lookup.
    fn collect_toplevel_names(&mut self, module: &Module) {
        for item in &module.items {
            match item {
                Item::Fn(d) => {
                    self.toplevel_by_name
                        .insert(d.name.name.clone(), (d.id, d.name.span));
                }
                Item::Record(d) => {
                    self.toplevel_by_name
                        .insert(d.name.name.clone(), (d.id, d.name.span));
                }
                Item::Enum(d) => {
                    self.toplevel_by_name
                        .insert(d.name.name.clone(), (d.id, d.name.span));
                    for v in &d.variants {
                        let (id, name, span) = match v {
                            EnumVariant::Unit { id, name, .. }
                            | EnumVariant::Struct { id, name, .. }
                            | EnumVariant::Tuple { id, name, .. } => {
                                (*id, name.name.clone(), name.span)
                            }
                        };
                        self.toplevel_by_name.insert(name, (id, span));
                    }
                }
                Item::Class(d) => {
                    self.toplevel_by_name
                        .insert(d.name.name.clone(), (d.id, d.name.span));
                }
                Item::Trait(d) | Item::PlatformTrait(d) => {
                    self.toplevel_by_name
                        .insert(d.name.name.clone(), (d.id, d.name.span));
                }
                Item::Effect(d) => {
                    self.toplevel_by_name
                        .insert(d.name.name.clone(), (d.id, d.name.span));
                }
                Item::TypeAlias(d) => {
                    self.toplevel_by_name
                        .insert(d.name.name.clone(), (d.id, d.name.span));
                }
                Item::Const(d) => {
                    self.toplevel_by_name
                        .insert(d.name.name.clone(), (d.id, d.name.span));
                }
                Item::Impl(_)
                | Item::ModuleHandle(_)
                | Item::PropertyTest(_)
                | Item::Error { .. } => {}
            }
        }
    }
}

/// `true` if `span` contains the byte `offset` (inclusive of the end, so a
/// cursor sitting just after the last character still matches).
fn span_contains(span: Span, offset: usize) -> bool {
    offset >= span.start && offset <= span.end
}

// ─── AST walker ──────────────────────────────────────────────────────────────

struct Indexer<'a> {
    index: &'a mut SymbolIndex,
    /// Depth of enclosing impl/trait/class containers. Function
    /// declarations encountered at depth > 0 are methods whose call sites
    /// the resolver does not record.
    member_depth: usize,
}

impl Indexer<'_> {
    fn record_decl(&mut self, id: NodeId, span: Span) {
        self.index.def_spans.insert(id, span);
    }

    fn add_path(&mut self, path: &TypePath) {
        if path.segments.is_empty() {
            return;
        }
        self.index.type_paths.push(
            path.segments
                .iter()
                .map(|seg| (seg.name.clone(), seg.span))
                .collect(),
        );
    }

    /// Record pattern bindings and constructor/record paths.
    ///
    /// `record_binds` mirrors the resolver: or-pattern alternatives all
    /// bind the same names, and the resolver only creates bindings from
    /// the first alternative — so only the first alternative's binds are
    /// indexed, while paths are collected from every alternative.
    fn index_pattern(&mut self, pattern: &Pattern, record_binds: bool) {
        match pattern {
            Pattern::Bind { id, span, .. } | Pattern::MutBind { id, span, .. } => {
                if record_binds {
                    self.record_decl(*id, *span);
                }
            }
            Pattern::Tuple { elems, .. } => {
                for e in elems {
                    self.index_pattern(e, record_binds);
                }
            }
            Pattern::Constructor { path, fields, .. } => {
                self.add_path(path);
                for f in fields {
                    self.index_pattern(f, record_binds);
                }
            }
            Pattern::Record { path, fields, .. } => {
                self.add_path(path);
                for RecordPatternField { pattern, .. } in fields {
                    if let Some(p) = pattern {
                        self.index_pattern(p, record_binds);
                    }
                    // Shorthand `{ name }` bindings use a synthetic NodeId
                    // inside the resolver, so they cannot be indexed here.
                }
            }
            Pattern::List { elems, rest, .. } => {
                for e in elems {
                    self.index_pattern(e, record_binds);
                }
                if let Some(r) = rest {
                    self.index_pattern(r, record_binds);
                }
            }
            Pattern::Or { alternatives, .. } => {
                for (i, alt) in alternatives.iter().enumerate() {
                    self.index_pattern(alt, record_binds && i == 0);
                }
            }
            Pattern::Range { lo, hi, .. } => {
                self.index_pattern(lo, record_binds);
                self.index_pattern(hi, record_binds);
            }
            Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::Rest { .. } => {}
        }
    }
}

impl Visitor for Indexer<'_> {
    fn visit_fn_decl(&mut self, node: &FnDecl) {
        self.record_decl(node.id, node.name.span);
        if self.member_depth > 0 {
            self.index.opaque_member_ids.insert(node.id);
        }
        for path in &node.effect_clause {
            self.add_path(path);
        }
        walk_fn_decl(self, node);
    }

    fn visit_record_decl(&mut self, node: &RecordDecl) {
        self.record_decl(node.id, node.name.span);
        self.index.type_decl_ids.insert(node.id);
        for f in &node.fields {
            self.record_decl(f.id, f.name.span);
            self.index.opaque_member_ids.insert(f.id);
        }
        walk_record_decl(self, node);
    }

    fn visit_enum_decl(&mut self, node: &EnumDecl) {
        self.record_decl(node.id, node.name.span);
        self.index.type_decl_ids.insert(node.id);
        for v in &node.variants {
            match v {
                EnumVariant::Unit { id, name, .. }
                | EnumVariant::Struct { id, name, .. }
                | EnumVariant::Tuple { id, name, .. } => {
                    self.record_decl(*id, name.span);
                    self.index.variant_ids.insert(*id);
                }
            }
        }
        walk_enum_decl(self, node);
    }

    fn visit_class_decl(&mut self, node: &ClassDecl) {
        self.record_decl(node.id, node.name.span);
        self.index.type_decl_ids.insert(node.id);
        if let Some(base) = &node.base {
            self.add_path(base);
        }
        for t in &node.traits {
            self.add_path(t);
        }
        for f in &node.fields {
            self.record_decl(f.id, f.name.span);
            self.index.opaque_member_ids.insert(f.id);
        }
        self.member_depth += 1;
        walk_class_decl(self, node);
        self.member_depth -= 1;
    }

    fn visit_trait_decl(&mut self, node: &TraitDecl) {
        self.record_decl(node.id, node.name.span);
        self.index.type_decl_ids.insert(node.id);
        for s in &node.supertraits {
            self.add_path(s);
        }
        self.member_depth += 1;
        walk_trait_decl(self, node);
        self.member_depth -= 1;
    }

    fn visit_impl_block(&mut self, node: &ImplBlock) {
        if let Some(trait_path) = &node.trait_path {
            self.add_path(trait_path);
        }
        self.member_depth += 1;
        walk_impl_block(self, node);
        self.member_depth -= 1;
    }

    fn visit_effect_decl(&mut self, node: &EffectDecl) {
        self.record_decl(node.id, node.name.span);
        self.index.type_decl_ids.insert(node.id);
        for c in &node.components {
            self.add_path(c);
        }
        // Operations are *not* opaque: the resolver injects them as value
        // bindings (keyed by the operation's NodeId) into `with`-annotated
        // function scopes, so their call sites resolve normally.
        walk_effect_decl(self, node);
    }

    fn visit_type_alias_decl(&mut self, node: &TypeAliasDecl) {
        self.record_decl(node.id, node.name.span);
        self.index.type_decl_ids.insert(node.id);
        walk_type_alias_decl(self, node);
    }

    fn visit_const_decl(&mut self, node: &ConstDecl) {
        self.record_decl(node.id, node.name.span);
        walk_const_decl(self, node);
    }

    fn visit_pattern(&mut self, node: &Pattern) {
        // `index_pattern` recurses by itself; no walk needed.
        self.index_pattern(node, true);
    }

    fn visit_expr(&mut self, node: &Expr) {
        match node {
            Expr::Identifier { id, span, .. } => {
                self.index.ident_uses.push((*id, *span));
                // Identifiers have no children; no recursion needed.
            }
            Expr::RecordConstruct { path, .. } => {
                self.add_path(path);
                walk_expr(self, node);
            }
            Expr::FieldAccess { field, .. } => {
                if field.name.starts_with(char::is_uppercase) {
                    self.index
                        .member_accesses
                        .push((field.name.clone(), field.span));
                }
                walk_expr(self, node);
            }
            _ => walk_expr(self, node),
        }
    }

    fn visit_type_expr(&mut self, node: &TypeExpr) {
        match node {
            TypeExpr::Named { path, .. } => self.add_path(path),
            TypeExpr::Function { effects, .. } => {
                for e in effects {
                    self.add_path(e);
                }
            }
            _ => {}
        }
        walk_type_expr(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_lexer::Lexer;
    use bock_parser::Parser;
    use bock_source::SourceMap;
    use std::path::PathBuf;

    fn build_index(src: &str) -> (SourceMap, SymbolIndex) {
        let mut source_map = SourceMap::new();
        let file_id = source_map.add_file(PathBuf::from("test.bock"), src.to_string());
        let source_file = source_map.get_file(file_id);
        let mut lexer = Lexer::new(source_file);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens, source_file);
        let module = parser.parse_module();
        let index = SymbolIndex::build(&module);
        (source_map, index)
    }

    #[test]
    fn indexes_toplevel_declarations() {
        let src = "\
module m

public record Point { x: Int, y: Int }

public enum Color { Red, Green }

fn main() {
    let p = Point { x: 1, y: 2 }
}
";
        let (_map, index) = build_index(src);
        assert!(index.toplevel_by_name.contains_key("Point"));
        assert!(index.toplevel_by_name.contains_key("Color"));
        assert!(index.toplevel_by_name.contains_key("Red"));
        assert!(index.toplevel_by_name.contains_key("Green"));
        assert!(index.toplevel_by_name.contains_key("main"));
    }

    #[test]
    fn record_fields_are_opaque_members() {
        let src = "\
module m

public record Point { x: Int, y: Int }
";
        let (_map, index) = build_index(src);
        // The record itself is a type decl, not opaque.
        let (point_id, _) = index.toplevel_by_name["Point"];
        assert!(index.type_decl_ids.contains(&point_id));
        assert!(!index.opaque_member_ids.contains(&point_id));
        // Both fields are recorded as opaque members.
        assert_eq!(
            index
                .opaque_member_ids
                .iter()
                .filter(|id| index.def_spans.contains_key(id))
                .count(),
            2,
        );
    }

    #[test]
    fn impl_methods_are_opaque_but_toplevel_fns_are_not() {
        let src = "\
module m

record Card { rank: Int }

impl Card {
    public fn beats(self, other: Card) -> Bool {
        self.rank > other.rank
    }
}

fn standalone() -> Int {
    1
}
";
        let (_map, index) = build_index(src);
        let (standalone_id, _) = index.toplevel_by_name["standalone"];
        assert!(!index.opaque_member_ids.contains(&standalone_id));
        // Exactly one fn (the method) plus one field (`rank`) is opaque.
        assert_eq!(index.opaque_member_ids.len(), 2);
    }

    #[test]
    fn type_paths_capture_annotations_and_constructions() {
        let src = "\
module m

public record Point { x: Int, y: Int }

fn origin() -> Point {
    Point { x: 0, y: 0 }
}
";
        let (_map, index) = build_index(src);
        let point_paths: Vec<_> = index
            .type_paths
            .iter()
            .filter(|p| p.iter().any(|(n, _)| n == "Point"))
            .collect();
        // Return-type annotation + record construction.
        assert_eq!(point_paths.len(), 2, "paths: {:?}", index.type_paths);
    }

    #[test]
    fn ident_use_at_picks_innermost() {
        let src = "\
module m

fn main() {
    let answer = 42
    answer
}
";
        let (map, index) = build_index(src);
        let file = map.get_file(bock_errors::FileId(0));
        // Find the offset of the second `answer` (the use site).
        let use_offset = src.rfind("answer").expect("use site present");
        let (_, span) = index.ident_use_at(use_offset).expect("identifier found");
        assert_eq!(file.slice(span), "answer");
    }

    #[test]
    fn decl_name_at_finds_fn_name() {
        let src = "\
module m

fn greet() -> Int {
    1
}
";
        let (map, index) = build_index(src);
        let file = map.get_file(bock_errors::FileId(0));
        let offset = src.find("greet").expect("decl present");
        let (_, span) = index.decl_name_at(offset).expect("decl found");
        assert_eq!(file.slice(span), "greet");
    }
}
