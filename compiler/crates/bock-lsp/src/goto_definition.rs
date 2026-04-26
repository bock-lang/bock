//! `textDocument/definition` support — resolve an identifier at a cursor
//! position to the span of its declaration.
//!
//! The flow is:
//! 1. Lex + parse + resolve the current buffer (single-file mode; no
//!    cross-file symbol resolution).
//! 2. Translate the LSP [`Position`] to a byte offset.
//! 3. Walk the AST to find the innermost [`Expr::Identifier`] containing
//!    that offset, while simultaneously collecting a map of `NodeId ->
//!    declaration Span` so we can answer the lookup.
//! 4. Query the populated [`SymbolTable`] for the identifier's resolved
//!    `def_id`, then look up the recorded declaration span.
//!
//! Cross-file go-to-definition is out of scope for F.1.3 — every
//! declaration span returned here belongs to the buffer being checked.

use std::collections::HashMap;
use std::path::PathBuf;

use bock_air::{resolve_names_with_registry, ModuleRegistry, NameKind, SymbolTable};
use bock_ast::{
    visitor::{
        walk_class_decl, walk_effect_decl, walk_enum_decl, walk_expr, walk_fn_decl,
        walk_impl_block, walk_module, walk_record_decl, walk_trait_decl, walk_type_expr, Visitor,
    },
    ClassDecl, ConstDecl, EffectDecl, EnumDecl, EnumVariant, Expr, FnDecl, ImplBlock, Item, Module,
    NodeId, Param, Pattern, RecordDecl, RecordPatternField, TraitDecl, TypeAliasDecl, TypeExpr,
    TypePath,
};
use bock_errors::{FileId, Span};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;

/// Result of a successful go-to-definition lookup.
pub struct DefinitionResult {
    /// Owned source map containing the document (keeps `SourceFile`
    /// borrows valid for the lifetime of the result).
    pub source_map: SourceMap,
    /// The [`FileId`] that the returned span belongs to. For single-file
    /// checks this always equals the id of the document being queried.
    pub file_id: FileId,
    /// Span of the declaration being pointed at.
    pub target: Span,
}

/// Run the minimum pipeline needed to answer a definition query and return
/// the declaration span at the cursor, or `None` if the cursor is not over
/// a resolved identifier.
#[must_use]
pub fn find_definition(
    path: PathBuf,
    content: String,
    line: u32,
    character: u32,
) -> Option<DefinitionResult> {
    let mut source_map = SourceMap::new();
    let file_id = source_map.add_file(path, content);
    let source_file = source_map.get_file(file_id);

    let offset = position_to_offset(&source_file.content, line, character)?;

    // Lex + parse. Even if diagnostics fire, we still try — a partial AST
    // is often good enough to answer a definition query.
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();

    // Resolve (single-file — no cross-file registry).
    let registry = ModuleRegistry::new();
    let mut symbols = SymbolTable::new();
    let _ = resolve_names_with_registry(&module, &mut symbols, &registry);

    // Walk the AST: find the innermost identifier containing `offset` and
    // build a NodeId -> declaration Span index in the same pass.
    let mut finder = DefinitionFinder::new(offset);
    finder.collect_toplevel_names(&module);
    finder.visit_module(&module);

    // Prefer an expression-identifier match (which can be resolved via
    // the symbol table) over a bare type-reference name match.
    let target = finder
        .identifier_id
        .and_then(|id| symbols.resolutions.get(&id))
        .filter(|r| r.kind != NameKind::Builtin)
        .and_then(|r| finder.def_spans.get(&r.def_id).copied())
        .or_else(|| {
            finder
                .type_ref_name
                .as_ref()
                .and_then(|n| finder.toplevel_by_name.get(n).copied())
        })?;

    Some(DefinitionResult {
        source_map,
        file_id,
        target,
    })
}

/// Convert a 0-indexed LSP `(line, character)` position to a byte offset
/// into `content`. Column counts Unicode scalar values (matches the LSP
/// UTF-16 default closely enough for Bock's ASCII-dominant source, and
/// exactly for pure-ASCII files).
///
/// Returns `None` if `line` is past the end of the file.
#[must_use]
pub fn position_to_offset(content: &str, line: u32, character: u32) -> Option<usize> {
    // Locate the start of the target line.
    let mut line_start = 0usize;
    if line > 0 {
        let mut seen = 0u32;
        let mut found = false;
        for (i, ch) in content.char_indices() {
            if ch == '\n' {
                seen += 1;
                if seen == line {
                    line_start = i + 1;
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return None;
        }
    }

    // Walk `character` characters into that line, stopping early at EOL
    // or EOF so we clamp to the end of the line rather than overshooting.
    let rest = &content[line_start..];
    let mut byte_offset = 0usize;
    for (counted, (i, ch)) in rest.char_indices().enumerate() {
        if counted as u32 == character {
            return Some(line_start + i);
        }
        if ch == '\n' {
            return Some(line_start + i);
        }
        byte_offset = i + ch.len_utf8();
    }
    Some(line_start + byte_offset)
}

// ─── AST walker ──────────────────────────────────────────────────────────────

/// Single-pass visitor that:
///   - Records declaration spans keyed by NodeId (`def_spans`).
///   - Finds the innermost `Expr::Identifier` whose span contains a target
///     byte offset (`identifier_id`).
struct DefinitionFinder {
    offset: usize,
    def_spans: HashMap<NodeId, Span>,
    /// Map of top-level declaration name → decl span. Used as a fallback
    /// when the cursor is on a type reference (type refs never create
    /// entries in `SymbolTable::resolutions`; the resolver only calls
    /// `mark_used` for them).
    toplevel_by_name: HashMap<String, Span>,
    identifier_id: Option<NodeId>,
    /// Width of the innermost identifier match found so far. Smaller =
    /// more specific; we prefer tighter matches when nesting occurs.
    best_width: usize,
    /// Name of a [`TypePath`] segment under the cursor, if any.
    type_ref_name: Option<String>,
    /// Width of the innermost type-ref match found so far.
    best_type_width: usize,
}

impl DefinitionFinder {
    fn new(offset: usize) -> Self {
        Self {
            offset,
            def_spans: HashMap::new(),
            toplevel_by_name: HashMap::new(),
            identifier_id: None,
            best_width: usize::MAX,
            type_ref_name: None,
            best_type_width: usize::MAX,
        }
    }

    fn span_contains(&self, span: Span) -> bool {
        self.offset >= span.start && self.offset <= span.end
    }

    fn record_decl(&mut self, id: NodeId, span: Span) {
        self.def_spans.insert(id, span);
    }

    /// Pre-pass: index every top-level named declaration by name so
    /// type references can resolve by string lookup.
    fn collect_toplevel_names(&mut self, module: &Module) {
        for item in &module.items {
            match item {
                Item::Fn(d) => {
                    self.toplevel_by_name.insert(d.name.name.clone(), d.name.span);
                }
                Item::Record(d) => {
                    self.toplevel_by_name.insert(d.name.name.clone(), d.name.span);
                }
                Item::Enum(d) => {
                    self.toplevel_by_name.insert(d.name.name.clone(), d.name.span);
                    for v in &d.variants {
                        let (name, span) = match v {
                            EnumVariant::Unit { name, .. }
                            | EnumVariant::Struct { name, .. }
                            | EnumVariant::Tuple { name, .. } => (name.name.clone(), name.span),
                        };
                        self.toplevel_by_name.insert(name, span);
                    }
                }
                Item::Class(d) => {
                    self.toplevel_by_name.insert(d.name.name.clone(), d.name.span);
                }
                Item::Trait(d) | Item::PlatformTrait(d) => {
                    self.toplevel_by_name.insert(d.name.name.clone(), d.name.span);
                }
                Item::Effect(d) => {
                    self.toplevel_by_name.insert(d.name.name.clone(), d.name.span);
                }
                Item::TypeAlias(d) => {
                    self.toplevel_by_name.insert(d.name.name.clone(), d.name.span);
                }
                Item::Const(d) => {
                    self.toplevel_by_name.insert(d.name.name.clone(), d.name.span);
                }
                Item::Impl(_)
                | Item::ModuleHandle(_)
                | Item::PropertyTest(_)
                | Item::Error { .. } => {}
            }
        }
    }

    /// Check each segment of a [`TypePath`]; if the cursor is on one,
    /// remember its name as a candidate for name-based lookup.
    fn probe_type_path(&mut self, path: &TypePath) {
        for seg in &path.segments {
            if self.span_contains(seg.span) {
                let width = seg.span.end.saturating_sub(seg.span.start);
                if width < self.best_type_width {
                    self.best_type_width = width;
                    self.type_ref_name = Some(seg.name.clone());
                }
            }
        }
    }

    fn record_pattern_bindings(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::Bind { id, span, .. } | Pattern::MutBind { id, span, .. } => {
                self.record_decl(*id, *span);
            }
            Pattern::Tuple { elems, .. } => {
                for e in elems {
                    self.record_pattern_bindings(e);
                }
            }
            Pattern::Constructor { fields, .. } => {
                for f in fields {
                    self.record_pattern_bindings(f);
                }
            }
            Pattern::Record { fields, .. } => {
                for RecordPatternField { pattern, .. } in fields {
                    if let Some(p) = pattern {
                        self.record_pattern_bindings(p);
                    }
                    // Shorthand `{ name }` bindings use a synthetic
                    // NodeId inside the resolver, so we can't resolve
                    // them here — skip.
                }
            }
            Pattern::List { elems, rest, .. } => {
                for e in elems {
                    self.record_pattern_bindings(e);
                }
                if let Some(r) = rest {
                    self.record_pattern_bindings(r);
                }
            }
            Pattern::Or { alternatives, .. } => {
                if let Some(first) = alternatives.first() {
                    self.record_pattern_bindings(first);
                }
            }
            Pattern::Range { lo, hi, .. } => {
                self.record_pattern_bindings(lo);
                self.record_pattern_bindings(hi);
            }
            Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::Rest { .. } => {}
        }
    }
}

impl Visitor for DefinitionFinder {
    fn visit_module(&mut self, node: &Module) {
        walk_module(self, node);
    }

    fn visit_fn_decl(&mut self, node: &FnDecl) {
        self.record_decl(node.id, node.name.span);
        walk_fn_decl(self, node);
    }

    fn visit_record_decl(&mut self, node: &RecordDecl) {
        self.record_decl(node.id, node.name.span);
        for f in &node.fields {
            self.record_decl(f.id, f.name.span);
        }
        walk_record_decl(self, node);
    }

    fn visit_enum_decl(&mut self, node: &EnumDecl) {
        self.record_decl(node.id, node.name.span);
        for v in &node.variants {
            match v {
                EnumVariant::Unit { id, name, .. }
                | EnumVariant::Struct { id, name, .. }
                | EnumVariant::Tuple { id, name, .. } => {
                    self.record_decl(*id, name.span);
                }
            }
        }
        walk_enum_decl(self, node);
    }

    fn visit_class_decl(&mut self, node: &ClassDecl) {
        self.record_decl(node.id, node.name.span);
        for f in &node.fields {
            self.record_decl(f.id, f.name.span);
        }
        walk_class_decl(self, node);
    }

    fn visit_trait_decl(&mut self, node: &TraitDecl) {
        self.record_decl(node.id, node.name.span);
        walk_trait_decl(self, node);
    }

    fn visit_effect_decl(&mut self, node: &EffectDecl) {
        self.record_decl(node.id, node.name.span);
        walk_effect_decl(self, node);
    }

    fn visit_impl_block(&mut self, node: &ImplBlock) {
        walk_impl_block(self, node);
    }

    fn visit_type_alias_decl(&mut self, node: &TypeAliasDecl) {
        self.record_decl(node.id, node.name.span);
    }

    fn visit_const_decl(&mut self, node: &ConstDecl) {
        self.record_decl(node.id, node.name.span);
        // Walk into the initializer so identifiers inside it are still
        // considered for the position-to-node lookup.
        self.visit_expr(&node.value);
    }

    fn visit_param(&mut self, node: &Param) {
        self.record_pattern_bindings(&node.pattern);
        if let Some(default) = &node.default {
            self.visit_expr(default);
        }
    }

    fn visit_pattern(&mut self, node: &Pattern) {
        self.record_pattern_bindings(node);
    }

    fn visit_expr(&mut self, node: &Expr) {
        match node {
            Expr::Identifier { id, span, .. } => {
                if self.span_contains(*span) {
                    let width = span.end.saturating_sub(span.start);
                    if width < self.best_width {
                        self.best_width = width;
                        self.identifier_id = Some(*id);
                    }
                }
                // Identifiers have no children; no recursion needed.
            }
            Expr::RecordConstruct { path, .. } => {
                self.probe_type_path(path);
                walk_expr(self, node);
            }
            _ => walk_expr(self, node),
        }
    }

    fn visit_type_expr(&mut self, node: &TypeExpr) {
        if let TypeExpr::Named { path, .. } = node {
            self.probe_type_path(path);
        }
        walk_type_expr(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offset(content: &str, line: u32, ch: u32) -> usize {
        position_to_offset(content, line, ch).expect("position valid")
    }

    #[test]
    fn position_to_offset_start_of_file() {
        assert_eq!(offset("hello\nworld", 0, 0), 0);
    }

    #[test]
    fn position_to_offset_mid_first_line() {
        assert_eq!(offset("hello\nworld", 0, 3), 3);
    }

    #[test]
    fn position_to_offset_second_line() {
        assert_eq!(offset("hello\nworld", 1, 2), 8); // 'r'
    }

    #[test]
    fn position_to_offset_clamps_past_eol() {
        // `character` past end of line should clamp to the newline,
        // not jump onto the next line.
        assert_eq!(offset("ab\ncd", 0, 99), 2);
    }

    #[test]
    fn position_to_offset_unknown_line_returns_none() {
        assert!(position_to_offset("only one line", 5, 0).is_none());
    }

    #[test]
    fn position_to_offset_multibyte() {
        // "é" is 2 bytes (U+00E9); cursor at column 2 (0-indexed) lands
        // after 'é', i.e. at byte offset 3.
        assert_eq!(offset("aéx", 0, 2), 3);
    }

    #[test]
    fn definition_finds_fn_declaration_from_call_site() {
        let src = "\
module m

public fn greet(name: String) -> String {
    name
}

fn caller() -> String {
    greet(\"world\")
}
";
        // Cursor on the `greet` call inside caller(): line 7, column 4.
        let result = find_definition(PathBuf::from("test.bock"), src.to_string(), 7, 4)
            .expect("definition found");
        // Target should be the fn name in the declaration (line 2, "greet").
        let source = result.source_map.get_file(result.file_id);
        let (line, _) = source.line_col(result.target.start);
        assert_eq!(line, 3, "target should point at the fn declaration line");
        assert_eq!(source.slice(result.target), "greet");
    }

    #[test]
    fn definition_finds_let_binding_from_use_site() {
        let src = "\
module m

fn main() {
    let answer = 42
    answer
}
";
        // Cursor on `answer` use (line 4, col 4).
        let result = find_definition(PathBuf::from("test.bock"), src.to_string(), 4, 4)
            .expect("definition found");
        let source = result.source_map.get_file(result.file_id);
        assert_eq!(source.slice(result.target), "answer");
    }

    #[test]
    fn definition_returns_none_for_unresolved_name() {
        let src = "\
module m

fn main() {
    undefined_name
}
";
        // Cursor on `undefined_name` (line 3, col 4).
        assert!(find_definition(PathBuf::from("test.bock"), src.to_string(), 3, 4).is_none());
    }

    #[test]
    fn definition_returns_none_for_builtin() {
        let src = "\
module m

fn main() {
    print(\"hi\")
}
";
        // Cursor on `print` — a builtin with no in-buffer declaration.
        assert!(find_definition(PathBuf::from("test.bock"), src.to_string(), 3, 4).is_none());
    }

    #[test]
    fn definition_finds_type_declaration() {
        let src = "\
module m

public record Point { x: Int, y: Int }

fn origin() -> Point {
    Point { x: 0, y: 0 }
}
";
        // Cursor on `Point` in the return-type annotation (line 4, col 17).
        let result = find_definition(PathBuf::from("test.bock"), src.to_string(), 4, 17)
            .expect("definition found");
        let source = result.source_map.get_file(result.file_id);
        assert_eq!(source.slice(result.target), "Point");
    }

    #[test]
    fn definition_finds_enum_variant_constructor() {
        let src = "\
module m

public enum Color { Red, Green, Blue }

fn favorite() -> Color {
    Red
}
";
        // Cursor on `Red` at line 5, col 4.
        let result = find_definition(PathBuf::from("test.bock"), src.to_string(), 5, 4)
            .expect("definition found");
        let source = result.source_map.get_file(result.file_id);
        assert_eq!(source.slice(result.target), "Red");
    }

    #[test]
    fn definition_returns_none_when_cursor_is_off_any_identifier() {
        let src = "\
module m

fn main() {
    let x = 1
}
";
        // Cursor in the middle of whitespace on line 3 col 0.
        assert!(find_definition(PathBuf::from("test.bock"), src.to_string(), 3, 0).is_none());
    }
}
