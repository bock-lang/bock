//! `textDocument/references` support — find every occurrence of the symbol
//! under the cursor within the current buffer.
//!
//! The flow mirrors go-to-definition:
//! 1. Lex + parse + resolve the buffer (single-file mode).
//! 2. Locate the symbol at the cursor: an identifier use site (resolved
//!    through the symbol table), a declaration name, or a type-path
//!    segment.
//! 3. Reverse-scan the resolver's resolution map for every use site that
//!    points at the same definition, and — for type-like declarations —
//!    name-match type-path segments, which the resolver does not record
//!    per-node resolutions for.
//!
//! The result separates the declaration span from the reference spans so
//! the server can honor `ReferenceContext::include_declaration`, and so
//! rename can edit both.
//!
//! Members whose use sites are invisible to the resolver (impl/trait/class
//! methods called through method syntax, record/class fields accessed
//! through field syntax) yield `None`: returning only a declaration would
//! make rename silently produce broken code.

use std::path::PathBuf;

use bock_air::{resolve_names_with_registry, ModuleRegistry, NameKind, SymbolTable};
use bock_ast::NodeId;
use bock_errors::{FileId, Span};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;

use crate::goto_definition::position_to_offset;
use crate::symbol_index::SymbolIndex;

/// All in-buffer occurrences of one symbol.
pub struct SymbolOccurrences {
    /// Owned source map containing the document (keeps `SourceFile`
    /// borrows valid for the lifetime of the result).
    pub source_map: SourceMap,
    /// The [`FileId`] every span in this result belongs to.
    pub file_id: FileId,
    /// The symbol's name as written at its declaration.
    pub name: String,
    /// Span of the exact token under the cursor that initiated the query.
    pub origin_span: Span,
    /// Span of the declaration's name.
    pub decl_span: Span,
    /// Spans of every reference to the declaration, excluding
    /// [`SymbolOccurrences::decl_span`], sorted by position.
    pub reference_spans: Vec<Span>,
}

/// Find every occurrence of the symbol at the cursor, or `None` if the
/// cursor is not on a symbol whose references can be tracked.
#[must_use]
pub fn find_occurrences(
    path: PathBuf,
    content: String,
    line: u32,
    character: u32,
) -> Option<SymbolOccurrences> {
    let mut source_map = SourceMap::new();
    let file_id = source_map.add_file(path, content);
    let source_file = source_map.get_file(file_id);

    let offset = position_to_offset(&source_file.content, line, character)?;

    // Lex + parse. Diagnostics are tolerated — a partial AST still allows
    // navigation in the well-formed parts of the buffer.
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();

    // Resolve (single-file — no cross-file registry).
    let registry = ModuleRegistry::new();
    let mut symbols = SymbolTable::new();
    let _ = resolve_names_with_registry(&module, &mut symbols, &registry);

    let index = SymbolIndex::build(&module);

    let (def_id, decl_span, origin_span) = locate_target(&index, &symbols, offset)?;

    // Methods and fields are referenced through `MethodCall`/`FieldAccess`
    // nodes the resolver never records — refuse rather than under-report.
    if index.opaque_member_ids.contains(&def_id) {
        return None;
    }

    let name = source_file.slice(decl_span).to_string();

    let mut refs: Vec<Span> = Vec::new();

    // Every identifier use site resolved to this definition.
    for (use_id, span) in &index.ident_uses {
        if symbols
            .resolutions
            .get(use_id)
            .is_some_and(|r| r.def_id == def_id)
        {
            refs.push(*span);
        }
    }

    // Type-like declarations are also referenced through type paths
    // (annotations, constructions, constructor patterns), which carry no
    // per-node resolutions — match those by name.
    if index.type_decl_ids.contains(&def_id) || index.variant_ids.contains(&def_id) {
        collect_path_refs(&index, &name, &mut refs);
    }

    // Qualified enum-variant references in expression position
    // (`Color.Red`) parse as field accesses; match those for variants.
    if index.variant_ids.contains(&def_id) {
        for (member, span) in &index.member_accesses {
            if member == &name {
                refs.push(*span);
            }
        }
    }

    refs.sort_unstable_by_key(|s| (s.start, s.end));
    refs.dedup_by_key(|s| (s.start, s.end));
    refs.retain(|s| !(s.start == decl_span.start && s.end == decl_span.end));

    Some(SymbolOccurrences {
        source_map,
        file_id,
        name,
        origin_span,
        decl_span,
        reference_spans: refs,
    })
}

/// Resolve the cursor to a definition: `(def_id, decl name span, span of the
/// token under the cursor)`.
///
/// Tries, in order: an identifier use site (resolved through the symbol
/// table), a declaration name span, a type-path segment (resolved by
/// top-level name lookup).
fn locate_target(
    index: &SymbolIndex,
    symbols: &SymbolTable,
    offset: usize,
) -> Option<(NodeId, Span, Span)> {
    if let Some((use_id, use_span)) = index.ident_use_at(offset) {
        if let Some(resolved) = symbols
            .resolutions
            .get(&use_id)
            .filter(|r| r.kind != NameKind::Builtin)
        {
            if let Some(&decl_span) = index.def_spans.get(&resolved.def_id) {
                return Some((resolved.def_id, decl_span, use_span));
            }
        }
        // Unresolved or external identifier: fall through to the other
        // probes (they will not match the same offset, so this returns
        // None for e.g. builtins — intentionally).
    }

    if let Some((decl_id, decl_span)) = index.decl_name_at(offset) {
        return Some((decl_id, decl_span, decl_span));
    }

    if let Some((seg_name, seg_span)) = index.type_segment_at(offset) {
        if let Some(&(decl_id, decl_span)) = index.toplevel_by_name.get(seg_name) {
            return Some((decl_id, decl_span, seg_span));
        }
    }

    None
}

/// Push every type-path segment that names `name` onto `refs`.
///
/// A segment matches when it is the first segment of a path (the position
/// the resolver itself treats as the in-scope name), or when the path is
/// rooted at a local top-level declaration (`Color.Red` — `Color` is local,
/// so `Red` is a local member, not a foreign name that happens to collide).
fn collect_path_refs(index: &SymbolIndex, name: &str, refs: &mut Vec<Span>) {
    for path in &index.type_paths {
        let local_root = path
            .first()
            .is_some_and(|(first, _)| index.toplevel_by_name.contains_key(first));
        for (i, (seg, span)) in path.iter().enumerate() {
            if (i == 0 || local_root) && seg == name {
                refs.push(*span);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str, line: u32, ch: u32) -> Option<SymbolOccurrences> {
        find_occurrences(PathBuf::from("test.bock"), src.to_string(), line, ch)
    }

    /// Render each span as its source text for readable assertions.
    fn texts(occ: &SymbolOccurrences) -> Vec<String> {
        let file = occ.source_map.get_file(occ.file_id);
        occ.reference_spans
            .iter()
            .map(|s| file.slice(*s).to_string())
            .collect()
    }

    /// Render each reference span as (line, column), 1-indexed.
    fn positions(occ: &SymbolOccurrences) -> Vec<(usize, usize)> {
        let file = occ.source_map.get_file(occ.file_id);
        occ.reference_spans
            .iter()
            .map(|s| file.line_col(s.start))
            .collect()
    }

    #[test]
    fn references_from_use_site_find_all_uses_and_decl() {
        let src = "\
module m

fn main() -> Int {
    let answer = 42
    let doubled = answer + answer
    doubled
}
";
        // Cursor on the first `answer` use (line 4, col 18).
        let occ = run(src, 4, 18).expect("occurrences found");
        assert_eq!(occ.name, "answer");
        assert_eq!(texts(&occ), vec!["answer", "answer"]);
        // Declaration is the `let answer` binding on line 4 (1-indexed).
        let file = occ.source_map.get_file(occ.file_id);
        assert_eq!(file.slice(occ.decl_span), "answer");
        assert_eq!(file.line_col(occ.decl_span.start).0, 4);
        // Both references are on line 5.
        assert_eq!(positions(&occ), vec![(5, 19), (5, 28)]);
    }

    #[test]
    fn references_from_declaration_site_match_use_site_query() {
        let src = "\
module m

public fn greet(name: String) -> String {
    name
}

fn caller() -> String {
    greet(\"world\")
}
";
        // Cursor on the `greet` declaration (line 2, col 10).
        let from_decl = run(src, 2, 10).expect("occurrences found");
        assert_eq!(from_decl.name, "greet");
        assert_eq!(texts(&from_decl), vec!["greet"]);

        // Cursor on the call site (line 7, col 4) yields the same set.
        let from_use = run(src, 7, 4).expect("occurrences found");
        assert_eq!(from_use.decl_span, from_decl.decl_span);
        assert_eq!(from_use.reference_spans, from_decl.reference_spans);
    }

    #[test]
    fn shadowed_names_in_sibling_scopes_do_not_cross_match() {
        let src = "\
module m

fn first() -> Int {
    let value = 1
    value
}

fn second() -> Int {
    let value = 2
    value + value
}
";
        // Cursor on `value` inside `first` (line 4, col 4).
        let occ = run(src, 4, 4).expect("occurrences found");
        let file = occ.source_map.get_file(occ.file_id);
        // Declaration must be the `let value` in `first` (line 4).
        assert_eq!(file.line_col(occ.decl_span.start).0, 4);
        // Exactly one reference — the use in `first`. The two uses in
        // `second` resolve to a different binding and must not appear.
        assert_eq!(positions(&occ), vec![(5, 5)]);
    }

    #[test]
    fn type_references_found_from_declaration() {
        let src = "\
module m

public record Point { x: Int, y: Int }

fn origin() -> Point {
    Point { x: 0, y: 0 }
}
";
        // Cursor on the `Point` declaration name (line 2, col 14).
        let occ = run(src, 2, 14).expect("occurrences found");
        assert_eq!(occ.name, "Point");
        // Return annotation + record construction.
        assert_eq!(positions(&occ), vec![(5, 16), (6, 5)]);
    }

    #[test]
    fn type_references_found_from_annotation() {
        let src = "\
module m

public record Point { x: Int, y: Int }

fn origin() -> Point {
    Point { x: 0, y: 0 }
}
";
        // Cursor on `Point` in the return annotation (line 4, col 17).
        let occ = run(src, 4, 17).expect("occurrences found");
        let file = occ.source_map.get_file(occ.file_id);
        // Declaration resolves back to line 3 (1-indexed).
        assert_eq!(file.line_col(occ.decl_span.start).0, 3);
        assert_eq!(occ.reference_spans.len(), 2);
    }

    #[test]
    fn enum_variant_references_include_bare_constructor_uses() {
        let src = "\
module m

public enum Color { Red, Green, Blue }

fn favorite() -> Color {
    Red
}

fn other() -> Color {
    Red
}
";
        // Cursor on the `Red` variant declaration (line 2, col 20).
        let occ = run(src, 2, 20).expect("occurrences found");
        assert_eq!(occ.name, "Red");
        assert_eq!(texts(&occ), vec!["Red", "Red"]);
        assert_eq!(positions(&occ), vec![(6, 5), (10, 5)]);
    }

    #[test]
    fn enum_variant_references_include_match_patterns() {
        let src = "\
module m

public enum Color { Red, Green }

fn score(c: Color) -> Int {
    match c {
        Red => 1
        Green => 2
    }
}

fn pick() -> Color {
    Red
}
";
        // Cursor on the `Red` variant declaration (line 2, col 20).
        let occ = run(src, 2, 20).expect("occurrences found");
        assert_eq!(occ.name, "Red");
        // The match-arm constructor pattern and the bare constructor use.
        assert_eq!(texts(&occ), vec!["Red", "Red"]);
        assert_eq!(positions(&occ), vec![(7, 9), (13, 5)]);
    }

    #[test]
    fn builtin_yields_no_occurrences() {
        let src = "\
module m

fn main() {
    print(\"hi\")
}
";
        // Cursor on `print` — a builtin with no in-buffer declaration.
        assert!(run(src, 3, 4).is_none());
    }

    #[test]
    fn whitespace_yields_no_occurrences() {
        let src = "\
module m

fn main() {
    let x = 1
}
";
        assert!(run(src, 3, 0).is_none());
    }

    #[test]
    fn record_field_is_refused() {
        let src = "\
module m

public record Point { x: Int, y: Int }

fn use_point(p: Point) -> Int {
    p.x
}
";
        // Cursor on the field declaration `x` (line 2, col 22). Field
        // accesses are invisible to the resolver, so references/rename
        // must refuse rather than under-report.
        assert!(run(src, 2, 22).is_none());
    }

    #[test]
    fn impl_method_is_refused() {
        let src = "\
module m

record Card { rank: Int }

impl Card {
    public fn beats(self, other: Card) -> Bool {
        self.rank > other.rank
    }
}
";
        // Cursor on the method name `beats` (line 5, col 14). Method calls
        // are invisible to the resolver, so refuse.
        assert!(run(src, 5, 14).is_none());
    }

    #[test]
    fn origin_span_is_token_under_cursor() {
        let src = "\
module m

fn main() -> Int {
    let answer = 42
    answer
}
";
        // Cursor on the use site (line 4, col 4).
        let occ = run(src, 4, 4).expect("occurrences found");
        let file = occ.source_map.get_file(occ.file_id);
        assert_eq!(file.slice(occ.origin_span), "answer");
        assert_eq!(file.line_col(occ.origin_span.start).0, 5);
    }
}
