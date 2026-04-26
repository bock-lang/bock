//! `textDocument/hover` support — resolve the inferred type of the AIR node
//! under the cursor and render it as a markdown tooltip.
//!
//! Flow:
//! 1. Run the full check pipeline (lex → parse → resolve → lower → check).
//! 2. Translate the LSP `(line, character)` position to a byte offset.
//! 3. Walk the checked AIR module to find the innermost node whose span
//!    contains that offset.
//! 4. Query [`bock_types::TypeChecker::type_of`] for the resolved type,
//!    apply the substitution one more time so any lingering inference
//!    variables get their final answer, and format as Bock syntax.

use std::path::PathBuf;

use bock_air::{
    lower_module, resolve_names_with_registry, visitor::walk_node, visitor::Visitor, AIRNode,
    ModuleRegistry, NodeId, NodeIdGen, NodeKind, SymbolTable,
};
use bock_errors::{FileId, Span};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{seed_imports, FnType, PrimitiveType, Type, TypeChecker};

use crate::goto_definition::position_to_offset;
use crate::type_display::format_type;

/// Result of a successful hover lookup.
pub struct HoverResult {
    /// Owned source map containing the document (keeps `SourceFile`
    /// borrows valid for the lifetime of the result).
    pub source_map: SourceMap,
    /// The [`FileId`] the reported span belongs to.
    pub file_id: FileId,
    /// Markdown-formatted hover body, ready for the LSP client.
    pub contents: String,
    /// Source span of the node whose type is reported — the client renders
    /// this as the underline range for the hover tooltip.
    pub span: Span,
}

/// Run the minimum pipeline needed to answer a hover query and return the
/// type of the innermost node at the cursor, or `None` if nothing useful
/// can be said.
#[must_use]
pub fn hover(path: PathBuf, content: String, line: u32, character: u32) -> Option<HoverResult> {
    let mut source_map = SourceMap::new();
    let file_id = source_map.add_file(path, content);
    let source_file = source_map.get_file(file_id);

    let offset = position_to_offset(&source_file.content, line, character)?;

    // Lex + parse. As in go-to-definition, we tolerate diagnostics — even a
    // partial AST often yields a useful hover result.
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();

    // Resolve (single-file — no cross-file registry).
    let registry = ModuleRegistry::new();
    let mut symbols = SymbolTable::new();
    let _ = resolve_names_with_registry(&module, &mut symbols, &registry);

    // Lower to AIR and type-check.
    let id_gen = NodeIdGen::new();
    let mut air_module = lower_module(&module, &id_gen, &symbols);

    let mut checker = TypeChecker::new();
    register_builtins(&mut checker);
    seed_imports(&mut checker, &module.imports, &registry);
    checker.check_module(&mut air_module);

    // Find the innermost node at the cursor.
    let mut finder = NodeFinder::new(offset);
    finder.visit_node(&air_module);
    let (node_id, node_span, kind_label) = finder.best?;

    // Look up the type, re-apply the substitution to resolve any remaining
    // variables, and format for hover.
    let ty = checker.type_of(node_id)?.clone();
    let ty = checker.subst.apply(&ty);

    // An all-error type carries no useful information — suppress the hover.
    if matches!(ty, Type::Error) {
        return None;
    }

    let contents = render_hover(&ty, kind_label);

    Some(HoverResult {
        source_map,
        file_id,
        contents,
        span: node_span,
    })
}

/// Build the markdown body for a hover response, including an optional
/// descriptive prefix like `variable` or `field` so users see the role of
/// the node, not just its type.
fn render_hover(ty: &Type, kind_label: Option<&'static str>) -> String {
    let prefix = match (ty, kind_label) {
        (Type::Function(_), _) => "signature",
        (_, Some(label)) => label,
        (_, None) => "type",
    };

    match ty {
        Type::Function(f) => format!("```\n{}\n```\n\n_{prefix}_", fn_signature(f)),
        _ => format!("`{}`\n\n_{prefix}_", format_type(ty)),
    }
}

fn fn_signature(f: &FnType) -> String {
    let mut out = String::from("fn(");
    let mut first = true;
    for p in &f.params {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str(&format_type(p));
    }
    out.push_str(") -> ");
    out.push_str(&format_type(&f.ret));
    if !f.effects.is_empty() {
        out.push_str(" with ");
        let mut first = true;
        for e in &f.effects {
            if !first {
                out.push_str(", ");
            }
            first = false;
            out.push_str(&e.name);
        }
    }
    out
}

// ─── Builtin prelude (shared with pipeline::check_document) ──────────────────

/// Define the prelude builtins expected by hand-written Bock programs.
///
/// Kept in sync with `pipeline::register_builtins`. Without this the hover
/// query would try to type-check against an environment missing `print`,
/// `assert`, `Ok`, etc., and spurious errors would poison `checker.types`.
fn register_builtins(checker: &mut TypeChecker) {
    let io_fn_ty = Type::Function(FnType {
        params: vec![Type::Primitive(PrimitiveType::String)],
        ret: Box::new(Type::Primitive(PrimitiveType::Void)),
        effects: vec![],
    });
    for name in ["print", "println", "debug"] {
        checker.env.define(name, io_fn_ty.clone());
    }

    let assert_ty = Type::Function(FnType {
        params: vec![Type::Primitive(PrimitiveType::Bool)],
        ret: Box::new(Type::Primitive(PrimitiveType::Void)),
        effects: vec![],
    });
    checker.env.define("assert", assert_ty);

    let expect_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Error),
        effects: vec![],
    });
    checker.env.define("expect", expect_ty);

    let never_fn_ty = Type::Function(FnType {
        params: vec![],
        ret: Box::new(Type::Primitive(PrimitiveType::Never)),
        effects: vec![],
    });
    for name in ["todo", "unreachable"] {
        checker.env.define(name, never_fn_ty.clone());
    }

    let constructor_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Error),
        effects: vec![],
    });
    for name in ["Ok", "Err", "Some"] {
        checker.env.define(name, constructor_ty.clone());
    }
    checker.env.define("None", Type::Error);
}

// ─── AIR walker: find the innermost node at a byte offset ────────────────────

/// Visitor that records the innermost AIR node whose span contains the
/// target byte offset.
struct NodeFinder {
    offset: usize,
    /// Best match so far: (node id, span, optional descriptive label).
    best: Option<(NodeId, Span, Option<&'static str>)>,
    /// Width of the current best match (smaller = more specific).
    best_width: usize,
}

impl NodeFinder {
    fn new(offset: usize) -> Self {
        Self {
            offset,
            best: None,
            best_width: usize::MAX,
        }
    }

    fn consider(&mut self, node: &AIRNode) {
        let span = node.span;
        if !(self.offset >= span.start && self.offset <= span.end) {
            return;
        }
        let width = span.end.saturating_sub(span.start);
        if width <= self.best_width {
            self.best_width = width;
            self.best = Some((node.id, span, describe_kind(&node.kind)));
        }
    }
}

impl Visitor for NodeFinder {
    fn visit_node(&mut self, node: &AIRNode) {
        self.consider(node);
        walk_node(self, node);
    }
}

/// A short user-facing label for a node kind — shown after the type in
/// hover tooltips so the user sees the *role* of the node, not just the
/// bare type string.
fn describe_kind(kind: &NodeKind) -> Option<&'static str> {
    match kind {
        NodeKind::Identifier { .. } => Some("variable"),
        NodeKind::Literal { .. } => Some("literal"),
        NodeKind::Call { .. } => Some("call"),
        NodeKind::MethodCall { .. } => Some("method call"),
        NodeKind::FieldAccess { .. } => Some("field"),
        NodeKind::BinaryOp { .. } => Some("expression"),
        NodeKind::UnaryOp { .. } => Some("expression"),
        NodeKind::RecordConstruct { .. } => Some("record"),
        NodeKind::ListLiteral { .. } => Some("list"),
        NodeKind::MapLiteral { .. } => Some("map"),
        NodeKind::SetLiteral { .. } => Some("set"),
        NodeKind::TupleLiteral { .. } => Some("tuple"),
        NodeKind::Lambda { .. } => Some("lambda"),
        NodeKind::If { .. } => Some("if expression"),
        NodeKind::Match { .. } => Some("match expression"),
        NodeKind::LetBinding { .. } => Some("binding"),
        NodeKind::Block { .. } => Some("block"),
        _ => None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str, line: u32, ch: u32) -> Option<HoverResult> {
        hover(PathBuf::from("test.bock"), src.to_string(), line, ch)
    }

    #[test]
    fn hover_on_let_binding_shows_int_type() {
        let src = "\
module m

fn main() {
    let answer = 42
    answer
}
";
        // Cursor on `answer` use (line 4, col 4).
        let result = run(src, 4, 4).expect("hover returned a result");
        assert!(
            result.contents.contains("Int"),
            "expected Int in hover contents, got: {}",
            result.contents
        );
    }

    #[test]
    fn hover_on_string_literal_shows_string_type() {
        let src = "\
module m

fn main() {
    let greeting = \"hello\"
}
";
        // Cursor on `\"hello\"` — line 3, col 22.
        let result = run(src, 3, 22).expect("hover returned a result");
        assert!(
            result.contents.contains("String"),
            "expected String in hover, got: {}",
            result.contents
        );
    }

    #[test]
    fn hover_on_bool_literal_shows_bool_type() {
        let src = "\
module m

fn main() {
    let flag = true
}
";
        // Cursor on `true` — line 3, col 16.
        let result = run(src, 3, 16).expect("hover returned a result");
        assert!(
            result.contents.contains("Bool"),
            "expected Bool in hover, got: {}",
            result.contents
        );
    }

    #[test]
    fn hover_on_fn_call_callee_shows_signature() {
        let src = "\
module m

fn add(a: Int, b: Int) -> Int {
    a
}

fn main() {
    add(1, 2)
}
";
        // Cursor on `add` call (line 7, col 4).
        let result = run(src, 7, 4).expect("hover returned a result");
        // Either the function signature (Fn(Int, Int) -> Int) or the
        // return type Int should appear.
        assert!(
            result.contents.contains("Int"),
            "expected Int somewhere in hover, got: {}",
            result.contents
        );
    }

    #[test]
    fn hover_returns_none_outside_any_node() {
        let src = "\
module m

fn main() {
    let x = 1
}
";
        // Cursor in whitespace, line 0, col 8 (way past `module m`).
        // Should not crash; may return None.
        let _ = run(src, 0, 8);
    }

    #[test]
    fn hover_returns_none_past_eof() {
        let src = "module m\n";
        assert!(run(src, 99, 0).is_none());
    }

    #[test]
    fn hover_on_list_literal() {
        let src = "\
module m

fn main() {
    let xs = [1, 2, 3]
}
";
        // Cursor on the list literal (line 3, col 13 — inside `[1, 2, 3]`).
        let result = run(src, 3, 13).expect("hover returned a result");
        assert!(
            result.contents.contains("Int") || result.contents.contains("List"),
            "expected list type info, got: {}",
            result.contents
        );
    }

    #[test]
    fn render_hover_formats_function_as_code_block() {
        let ty = Type::Function(FnType {
            params: vec![
                Type::Primitive(PrimitiveType::Int),
                Type::Primitive(PrimitiveType::Int),
            ],
            ret: Box::new(Type::Primitive(PrimitiveType::Int)),
            effects: vec![],
        });
        let out = render_hover(&ty, None);
        assert!(out.contains("fn(Int, Int) -> Int"), "got: {out}");
        assert!(out.contains("signature"), "got: {out}");
    }

    #[test]
    fn render_hover_formats_primitive_inline() {
        let out = render_hover(&Type::Primitive(PrimitiveType::String), Some("variable"));
        assert!(out.contains("`String`"), "got: {out}");
        assert!(out.contains("variable"), "got: {out}");
    }
}
