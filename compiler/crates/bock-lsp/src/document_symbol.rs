//! `textDocument/documentSymbol` support — a hierarchical outline of the
//! buffer's declarations.
//!
//! Only lex + parse are needed: the outline is purely syntactic. Items map
//! to LSP `SymbolKind`s as follows:
//!
//! | Bock item            | SymbolKind       | children                  |
//! |----------------------|------------------|---------------------------|
//! | `module` declaration | `MODULE`         | all items                 |
//! | `fn`                 | `FUNCTION`       | —                         |
//! | `record`             | `STRUCT`         | fields (`FIELD`)          |
//! | `enum`               | `ENUM`           | variants (`ENUM_MEMBER`)  |
//! | `class`              | `CLASS`          | fields + methods          |
//! | `trait`              | `INTERFACE`      | methods (`METHOD`)        |
//! | `impl`               | `OBJECT`         | methods (`METHOD`)        |
//! | `effect`             | `EVENT`          | operations (`METHOD`)     |
//! | `type` alias         | `TYPE_PARAMETER` | —                         |
//! | `const`              | `CONSTANT`       | —                         |
//! | `handle` (module)    | `EVENT`          | —                         |
//! | `property` test      | `FUNCTION`       | —                         |
//!
//! Each symbol's `selection_span` is the declared name; its `span` is the
//! whole item. Parse-error recovery items are skipped.

use std::path::PathBuf;

use bock_ast::{
    ClassDecl, EffectDecl, EnumDecl, EnumVariant, FnDecl, ImplBlock, Item, RecordDecl,
    RecordDeclField, TraitDecl, TypeExpr, TypePath,
};
use bock_errors::{FileId, Span};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::{SourceFile, SourceMap};
use tower_lsp::lsp_types::{DocumentSymbol, SymbolKind};

use crate::diagnostics::span_to_range;

/// One node in the buffer's symbol outline, in source-span form.
///
/// Conversion to LSP ranges happens in [`to_lsp_symbols`]; keeping spans
/// here lets tests assert on the tree without a position back-end.
pub struct SymbolNode {
    /// Display name (the declared identifier, or a rendered header for
    /// nameless items like `impl` blocks).
    pub name: String,
    /// Optional extra detail shown next to the name.
    pub detail: Option<String>,
    /// LSP symbol kind.
    pub kind: SymbolKind,
    /// Span of the whole item.
    pub span: Span,
    /// Span of the name. Always contained within [`SymbolNode::span`].
    pub selection_span: Span,
    /// Nested symbols (fields, variants, methods…).
    pub children: Vec<SymbolNode>,
}

/// Result of building the outline for one document.
pub struct DocumentSymbolsResult {
    /// Owned source map containing the document (keeps `SourceFile`
    /// borrows valid for the lifetime of the result).
    pub source_map: SourceMap,
    /// Id of the added file inside [`DocumentSymbolsResult::source_map`].
    pub file_id: FileId,
    /// Root symbols. If the buffer declares `module <path>`, a single
    /// `MODULE` root wraps every item.
    pub symbols: Vec<SymbolNode>,
}

/// Lex + parse `content` and build its symbol outline.
#[must_use]
pub fn document_symbols(path: PathBuf, content: String) -> DocumentSymbolsResult {
    let mut source_map = SourceMap::new();
    let file_id = source_map.add_file(path, content);
    let source_file = source_map.get_file(file_id);

    // Tolerate diagnostics: a partial AST still produces a useful outline
    // for the well-formed items.
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();

    let items: Vec<SymbolNode> = module.items.iter().filter_map(item_symbol).collect();

    let symbols = match &module.path {
        Some(path) => {
            // The root's range must enclose the module path *and* every
            // child item, so clients can rely on the containment invariant.
            let mut full = enclosing_span(module.span, path.span);
            for item in &items {
                full = enclosing_span(full, item.span);
            }
            vec![SymbolNode {
                name: render_segments(&path.segments),
                detail: None,
                kind: SymbolKind::MODULE,
                span: full,
                selection_span: path.span,
                children: items,
            }]
        }
        None => items,
    };

    DocumentSymbolsResult {
        source_map,
        file_id,
        symbols,
    }
}

/// Convert an outline tree into LSP [`DocumentSymbol`]s using `source` for
/// position lookup.
#[must_use]
pub fn to_lsp_symbols(nodes: &[SymbolNode], source: &SourceFile) -> Vec<DocumentSymbol> {
    nodes
        .iter()
        .map(|node| {
            // The LSP requires `selection_range` ⊆ `range`; guard against
            // any item span that fails to cover its name span.
            let full = enclosing_span(node.span, node.selection_span);
            let children = to_lsp_symbols(&node.children, source);
            #[allow(deprecated)] // `deprecated` is a required struct field.
            DocumentSymbol {
                name: node.name.clone(),
                detail: node.detail.clone(),
                kind: node.kind,
                tags: None,
                deprecated: None,
                range: span_to_range(full, source),
                selection_range: span_to_range(node.selection_span, source),
                children: if children.is_empty() {
                    None
                } else {
                    Some(children)
                },
            }
        })
        .collect()
}

/// The smallest span covering both `outer` and `inner`.
fn enclosing_span(outer: Span, inner: Span) -> Span {
    Span {
        file: outer.file,
        start: outer.start.min(inner.start),
        end: outer.end.max(inner.end),
    }
}

fn item_symbol(item: &Item) -> Option<SymbolNode> {
    match item {
        Item::Fn(d) => Some(fn_symbol(d, SymbolKind::FUNCTION)),
        Item::Record(d) => Some(record_symbol(d)),
        Item::Enum(d) => Some(enum_symbol(d)),
        Item::Class(d) => Some(class_symbol(d)),
        Item::Trait(d) | Item::PlatformTrait(d) => Some(trait_symbol(d)),
        Item::Impl(d) => Some(impl_symbol(d)),
        Item::Effect(d) => Some(effect_symbol(d)),
        Item::TypeAlias(d) => Some(SymbolNode {
            name: d.name.name.clone(),
            detail: None,
            kind: SymbolKind::TYPE_PARAMETER,
            span: d.span,
            selection_span: d.name.span,
            children: Vec::new(),
        }),
        Item::Const(d) => Some(SymbolNode {
            name: d.name.name.clone(),
            detail: Some(render_type_expr(&d.ty)),
            kind: SymbolKind::CONSTANT,
            span: d.span,
            selection_span: d.name.span,
            children: Vec::new(),
        }),
        Item::ModuleHandle(d) => Some(SymbolNode {
            name: format!("handle {}", render_type_path(&d.effect)),
            detail: None,
            kind: SymbolKind::EVENT,
            span: d.span,
            selection_span: d.effect.span,
            children: Vec::new(),
        }),
        Item::PropertyTest(d) => Some(SymbolNode {
            name: d.name.clone(),
            detail: Some("property test".to_string()),
            kind: SymbolKind::FUNCTION,
            span: d.span,
            selection_span: d.span,
            children: Vec::new(),
        }),
        Item::Error { .. } => None,
    }
}

fn fn_symbol(d: &FnDecl, kind: SymbolKind) -> SymbolNode {
    SymbolNode {
        name: d.name.name.clone(),
        detail: None,
        kind,
        span: d.span,
        selection_span: d.name.span,
        children: Vec::new(),
    }
}

fn field_symbol(f: &RecordDeclField) -> SymbolNode {
    SymbolNode {
        name: f.name.name.clone(),
        detail: Some(render_type_expr(&f.ty)),
        kind: SymbolKind::FIELD,
        span: f.span,
        selection_span: f.name.span,
        children: Vec::new(),
    }
}

fn record_symbol(d: &RecordDecl) -> SymbolNode {
    SymbolNode {
        name: d.name.name.clone(),
        detail: None,
        kind: SymbolKind::STRUCT,
        span: d.span,
        selection_span: d.name.span,
        children: d.fields.iter().map(field_symbol).collect(),
    }
}

fn enum_symbol(d: &EnumDecl) -> SymbolNode {
    let children = d
        .variants
        .iter()
        .map(|v| {
            let (name, span) = match v {
                EnumVariant::Unit { name, span, .. }
                | EnumVariant::Struct { name, span, .. }
                | EnumVariant::Tuple { name, span, .. } => (name, span),
            };
            SymbolNode {
                name: name.name.clone(),
                detail: None,
                kind: SymbolKind::ENUM_MEMBER,
                span: *span,
                selection_span: name.span,
                children: Vec::new(),
            }
        })
        .collect();
    SymbolNode {
        name: d.name.name.clone(),
        detail: None,
        kind: SymbolKind::ENUM,
        span: d.span,
        selection_span: d.name.span,
        children,
    }
}

fn class_symbol(d: &ClassDecl) -> SymbolNode {
    let mut children: Vec<SymbolNode> = d.fields.iter().map(field_symbol).collect();
    children.extend(d.methods.iter().map(|m| fn_symbol(m, SymbolKind::METHOD)));
    SymbolNode {
        name: d.name.name.clone(),
        detail: None,
        kind: SymbolKind::CLASS,
        span: d.span,
        selection_span: d.name.span,
        children,
    }
}

fn trait_symbol(d: &TraitDecl) -> SymbolNode {
    SymbolNode {
        name: d.name.name.clone(),
        detail: None,
        kind: SymbolKind::INTERFACE,
        span: d.span,
        selection_span: d.name.span,
        children: d
            .methods
            .iter()
            .map(|m| fn_symbol(m, SymbolKind::METHOD))
            .collect(),
    }
}

fn impl_symbol(d: &ImplBlock) -> SymbolNode {
    let target = render_type_expr(&d.target);
    let (name, selection_span) = match &d.trait_path {
        Some(trait_path) => (
            format!("impl {} for {}", render_type_path(trait_path), target),
            trait_path.span,
        ),
        None => (format!("impl {target}"), d.target.span()),
    };
    SymbolNode {
        name,
        detail: None,
        kind: SymbolKind::OBJECT,
        span: d.span,
        selection_span,
        children: d
            .methods
            .iter()
            .map(|m| fn_symbol(m, SymbolKind::METHOD))
            .collect(),
    }
}

fn effect_symbol(d: &EffectDecl) -> SymbolNode {
    SymbolNode {
        name: d.name.name.clone(),
        detail: None,
        kind: SymbolKind::EVENT,
        span: d.span,
        selection_span: d.name.span,
        children: d
            .operations
            .iter()
            .map(|op| fn_symbol(op, SymbolKind::METHOD))
            .collect(),
    }
}

// ─── Type rendering (compact, for names/details) ─────────────────────────────

fn render_segments(segments: &[bock_ast::Ident]) -> String {
    segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

fn render_type_path(path: &TypePath) -> String {
    render_segments(&path.segments)
}

fn render_type_expr(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named { path, args, .. } => {
            let base = render_type_path(path);
            if args.is_empty() {
                base
            } else {
                let rendered: Vec<String> = args.iter().map(render_type_expr).collect();
                format!("{base}[{}]", rendered.join(", "))
            }
        }
        TypeExpr::Tuple { elems, .. } => {
            let rendered: Vec<String> = elems.iter().map(render_type_expr).collect();
            format!("({})", rendered.join(", "))
        }
        TypeExpr::Function { params, ret, .. } => {
            let rendered: Vec<String> = params.iter().map(render_type_expr).collect();
            format!("Fn({}) -> {}", rendered.join(", "), render_type_expr(ret))
        }
        TypeExpr::Optional { inner, .. } => format!("{}?", render_type_expr(inner)),
        TypeExpr::SelfType { .. } => "Self".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> DocumentSymbolsResult {
        document_symbols(PathBuf::from("test.bock"), src.to_string())
    }

    /// Find a child by name within a symbol list, panicking with a useful
    /// message when absent.
    fn find<'a>(nodes: &'a [SymbolNode], name: &str) -> &'a SymbolNode {
        nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("symbol `{name}` not found"))
    }

    const FULL_SRC: &str = "\
module demo.app

public record Point { x: Int, y: Int }

public enum Color { Red, Green, Blue }

trait Shape {
    fn area(self) -> Int
}

impl Shape for Point {
    public fn area(self) -> Int {
        self.x
    }
}

effect Log {
    fn log(message: String) -> Void
}

const MAX: Int = 10

public fn main() -> Int {
    MAX
}
";

    #[test]
    fn module_declaration_becomes_root_symbol() {
        let result = run(FULL_SRC);
        assert_eq!(result.symbols.len(), 1, "one MODULE root expected");
        let root = &result.symbols[0];
        assert_eq!(root.name, "demo.app");
        assert_eq!(root.kind, SymbolKind::MODULE);
        assert_eq!(root.children.len(), 7, "all items nested under module");
    }

    #[test]
    fn record_has_field_children() {
        let result = run(FULL_SRC);
        let point = find(&result.symbols[0].children, "Point");
        assert_eq!(point.kind, SymbolKind::STRUCT);
        let names: Vec<_> = point.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["x", "y"]);
        assert!(point.children.iter().all(|c| c.kind == SymbolKind::FIELD));
        assert_eq!(point.children[0].detail.as_deref(), Some("Int"));
    }

    #[test]
    fn enum_has_variant_children() {
        let result = run(FULL_SRC);
        let color = find(&result.symbols[0].children, "Color");
        assert_eq!(color.kind, SymbolKind::ENUM);
        let names: Vec<_> = color.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Red", "Green", "Blue"]);
        assert!(color
            .children
            .iter()
            .all(|c| c.kind == SymbolKind::ENUM_MEMBER));
    }

    #[test]
    fn trait_and_impl_have_method_children() {
        let result = run(FULL_SRC);
        let shape = find(&result.symbols[0].children, "Shape");
        assert_eq!(shape.kind, SymbolKind::INTERFACE);
        assert_eq!(shape.children.len(), 1);
        assert_eq!(shape.children[0].kind, SymbolKind::METHOD);

        let imp = find(&result.symbols[0].children, "impl Shape for Point");
        assert_eq!(imp.kind, SymbolKind::OBJECT);
        assert_eq!(imp.children.len(), 1);
        assert_eq!(imp.children[0].name, "area");
        assert_eq!(imp.children[0].kind, SymbolKind::METHOD);
    }

    #[test]
    fn effect_const_and_fn_kinds() {
        let result = run(FULL_SRC);
        let children = &result.symbols[0].children;
        assert_eq!(find(children, "Log").kind, SymbolKind::EVENT);
        assert_eq!(find(children, "Log").children[0].name, "log");
        assert_eq!(find(children, "MAX").kind, SymbolKind::CONSTANT);
        assert_eq!(find(children, "MAX").detail.as_deref(), Some("Int"));
        assert_eq!(find(children, "main").kind, SymbolKind::FUNCTION);
    }

    #[test]
    fn selection_span_is_contained_in_span() {
        fn check(node: &SymbolNode) {
            let full = enclosing_span(node.span, node.selection_span);
            assert!(
                full.start <= node.selection_span.start && node.selection_span.end <= full.end,
                "selection span must sit inside the full span for `{}`",
                node.name,
            );
            for child in &node.children {
                check(child);
            }
        }
        let result = run(FULL_SRC);
        for node in &result.symbols {
            check(node);
        }
    }

    #[test]
    fn selection_span_covers_the_name() {
        let result = run(FULL_SRC);
        let file = result.source_map.get_file(result.file_id);
        let point = find(&result.symbols[0].children, "Point");
        assert_eq!(file.slice(point.selection_span), "Point");
        let main_fn = find(&result.symbols[0].children, "main");
        assert_eq!(file.slice(main_fn.selection_span), "main");
    }

    #[test]
    fn to_lsp_symbols_produces_nested_tree() {
        let result = run(FULL_SRC);
        let file = result.source_map.get_file(result.file_id);
        let lsp = to_lsp_symbols(&result.symbols, file);
        assert_eq!(lsp.len(), 1);
        let root = &lsp[0];
        assert_eq!(root.kind, SymbolKind::MODULE);
        let children = root.children.as_ref().expect("module children");
        assert_eq!(children.len(), 7);
        // Ranges must satisfy the LSP containment invariant.
        for child in children {
            assert!(child.range.start <= child.selection_range.start);
            assert!(child.selection_range.end <= child.range.end);
        }
        // Leaf symbols have no children array at all.
        let main_fn = children
            .iter()
            .find(|c| c.name == "main")
            .expect("main present");
        assert!(main_fn.children.is_none());
    }

    #[test]
    fn empty_module_has_no_symbols() {
        let result = run("module m\n");
        assert_eq!(result.symbols.len(), 1, "module root only");
        assert!(result.symbols[0].children.is_empty());
    }

    #[test]
    fn file_without_module_declaration_yields_flat_list() {
        let src = "\
fn lonely() -> Int {
    1
}
";
        let result = run(src);
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "lonely");
        assert_eq!(result.symbols[0].kind, SymbolKind::FUNCTION);
    }
}
