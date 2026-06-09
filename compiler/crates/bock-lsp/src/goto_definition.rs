//! `textDocument/definition` support — resolve an identifier at a cursor
//! position to the span of its declaration.
//!
//! The flow is:
//! 1. Lex + parse + resolve the current buffer (single-file mode; no
//!    cross-file symbol resolution).
//! 2. Translate the LSP `Position` to a byte offset.
//! 3. Build a [`SymbolIndex`](crate::symbol_index) over the AST: declaration
//!    spans by `NodeId`, identifier use sites, and type-path occurrences.
//! 4. Query the populated `SymbolTable` for the identifier's resolved
//!    `def_id`, then look up the recorded declaration span. If the cursor is
//!    on a type reference instead (the resolver records no per-node
//!    resolutions for those), fall back to a top-level name lookup.
//!
//! Cross-file go-to-definition is out of scope for F.1.3 — every
//! declaration span returned here belongs to the buffer being checked.

use std::path::PathBuf;

use bock_air::{resolve_names_with_registry, ModuleRegistry, NameKind, SymbolTable};
use bock_errors::{FileId, Span};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;

use crate::symbol_index::SymbolIndex;

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

    let index = SymbolIndex::build(&module);

    // Prefer an expression-identifier match (which can be resolved via
    // the symbol table) over a bare type-reference name match.
    let target = index
        .ident_use_at(offset)
        .and_then(|(id, _)| symbols.resolutions.get(&id))
        .filter(|r| r.kind != NameKind::Builtin)
        .and_then(|r| index.def_spans.get(&r.def_id).copied())
        .or_else(|| {
            index
                .type_segment_at(offset)
                .and_then(|(name, _)| index.toplevel_by_name.get(name).map(|&(_, span)| span))
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
    fn definition_finds_param_type_from_annotation() {
        let src = "\
module m

public record Point { x: Int, y: Int }

fn shift(p: Point) -> Int {
    p.x
}
";
        // Cursor on `Point` in the parameter annotation (line 4, col 12).
        let result = find_definition(PathBuf::from("test.bock"), src.to_string(), 4, 12)
            .expect("definition found");
        let source = result.source_map.get_file(result.file_id);
        assert_eq!(source.slice(result.target), "Point");
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
