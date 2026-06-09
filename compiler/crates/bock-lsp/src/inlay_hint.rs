//! `textDocument/inlayHint` support — inferred type hints for `let` bindings.
//!
//! For every `let` (or `let mut`) binding *without* an explicit type
//! annotation, the LSP renders an inline `: T` hint immediately after the
//! binding name, where `T` is the type the checker inferred for the binding.
//! `for`-loop binders are collected through the same machinery (the binder
//! pattern survives the checker's iterator desugaring with its `NodeId`
//! intact), so they produce hints whenever the element type resolves.
//!
//! Flow:
//! 1. Run the hover pipeline (lex → parse → resolve → lower → check).
//! 2. *Before* checking, walk the freshly lowered AIR module and record every
//!    bind-pattern site under an unannotated `let` or a `for` binder. Doing
//!    this pre-check matters: the checker synthesizes its own helper
//!    `let __bock_iter_N = …` bindings while desugaring `for` loops, and
//!    collecting first keeps those out of the hint set.
//! 3. After checking, look each site's inferred type up by the pattern's
//!    `NodeId`, drop any that are unresolved or poisoned by type errors, and
//!    render the rest via [`format_type`](crate::type_display::format_type).
//!
//! Hints whose insertion point falls outside the requested range are
//! filtered out, and pathologically long type renders are truncated to
//! [`TYPE_RENDER_BUDGET`] characters.

use std::path::PathBuf;

use bock_air::{
    lower_module, resolve_names_with_registry, visitor::walk_node, visitor::Visitor, AIRNode,
    ModuleRegistry, NodeId, NodeIdGen, NodeKind, SymbolTable,
};
use bock_errors::{FileId, Span};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{seed_imports, seed_prelude, Type, TypeChecker};
use tower_lsp::lsp_types::Range;

use crate::goto_definition::position_to_offset;
use crate::pipeline::register_builtins;
use crate::type_display::format_type;

/// Maximum number of characters of a rendered type shown in a hint label.
/// Longer renders are cut at the budget and terminated with `…`.
pub const TYPE_RENDER_BUDGET: usize = 60;

/// One inferred-type hint, ready for conversion to an LSP `InlayHint`.
pub struct TypeHint {
    /// Zero-width span at the insertion point — immediately after the
    /// binding name. Convert with [`span_to_range`](crate::span_to_range)
    /// and use the range's `start` as the hint position.
    pub span: Span,
    /// Hint label, including the leading `: ` separator.
    pub label: String,
}

/// Result of computing inlay hints for a document.
pub struct InlayHintsResult {
    /// Owned source map containing the document (keeps `SourceFile`
    /// borrows valid for the lifetime of the result).
    pub source_map: SourceMap,
    /// Id of the added file inside [`InlayHintsResult::source_map`].
    pub file_id: FileId,
    /// Hints inside the requested range, sorted by insertion offset.
    pub hints: Vec<TypeHint>,
}

/// Compute inferred-type inlay hints for the unannotated `let` bindings
/// (and `for` binders) whose names end inside `range`.
#[must_use]
pub fn inlay_hints(path: PathBuf, content: String, range: Range) -> InlayHintsResult {
    let mut source_map = SourceMap::new();
    let file_id = source_map.add_file(path, content);
    let source_file = source_map.get_file(file_id);

    // Clamp the requested range to byte offsets. A position past the end of
    // the document clamps to the document end.
    let eof = source_file.content.len();
    let range_start = position_to_offset(
        &source_file.content,
        range.start.line,
        range.start.character,
    )
    .unwrap_or(eof);
    let range_end = position_to_offset(&source_file.content, range.end.line, range.end.character)
        .unwrap_or(eof);

    // Lex + parse. As in hover, we tolerate diagnostics — a partial AST
    // still yields useful hints for the parts that did check.
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();

    // Resolve (single-file — no cross-file registry).
    let registry = ModuleRegistry::new();
    let mut symbols = SymbolTable::new();
    let _ = resolve_names_with_registry(&module, &mut symbols, &registry);

    // Lower to AIR.
    let id_gen = NodeIdGen::new();
    let mut air_module = lower_module(&module, &id_gen, &symbols);

    // Collect hint sites BEFORE type checking: the checker rewrites `for`
    // loops in place and synthesizes helper `let` bindings while doing so;
    // collecting first guarantees only user-written binders produce hints.
    let mut sites = Vec::new();
    SiteCollector { sites: &mut sites }.visit_node(&air_module);

    // Type-check (records inferred pattern types keyed by `NodeId`).
    let mut checker = TypeChecker::new();
    register_builtins(&mut checker);
    seed_prelude(&mut checker, &registry);
    seed_imports(&mut checker, &module.imports, &registry);
    checker.check_module(&mut air_module);

    let mut hints = Vec::new();
    for site in sites {
        let offset = site.name_span.end;
        if offset < range_start || offset > range_end {
            continue;
        }
        let Some(ty) = checker.type_of(site.pattern_id) else {
            continue;
        };
        // Re-apply the substitution so lingering inference variables get
        // their final answer (mirrors the hover path).
        let ty = checker.subst.apply(ty);
        if !is_renderable(&ty) {
            continue;
        }
        let label = format!(": {}", truncate_render(format_type(&ty)));
        hints.push(TypeHint {
            span: Span {
                file: file_id,
                start: offset,
                end: offset,
            },
            label,
        });
    }
    hints.sort_unstable_by_key(|h| h.span.start);

    InlayHintsResult {
        source_map,
        file_id,
        hints,
    }
}

/// `true` if `ty` contains no error poison, unresolved inference variable,
/// or sketch-mode placeholder anywhere — i.e. rendering it produces a
/// clean, fully resolved type string rather than `<error>`/`?N` noise.
fn is_renderable(ty: &Type) -> bool {
    match ty {
        Type::Primitive(_) | Type::Named(_) => true,
        Type::Generic(g) => g.args.iter().all(is_renderable),
        Type::Tuple(elems) => elems.iter().all(is_renderable),
        Type::Function(f) => f.params.iter().all(is_renderable) && is_renderable(&f.ret),
        Type::Optional(inner) => is_renderable(inner),
        Type::Result(ok, err) => is_renderable(ok) && is_renderable(err),
        Type::Refined(base, _) => is_renderable(base),
        Type::TypeVar(_) | Type::Flexible(_) | Type::Error => false,
    }
}

/// Cut a rendered type down to [`TYPE_RENDER_BUDGET`] characters, replacing
/// the overflow with a single `…`.
fn truncate_render(rendered: String) -> String {
    if rendered.chars().count() <= TYPE_RENDER_BUDGET {
        return rendered;
    }
    let mut out: String = rendered.chars().take(TYPE_RENDER_BUDGET - 1).collect();
    out.push('…');
    out
}

// ─── AIR walker: collect unannotated binder sites ────────────────────────────

/// One candidate hint site recorded before type checking.
struct HintSite {
    /// AIR `NodeId` of the `BindPat` whose inferred type becomes the hint.
    pattern_id: NodeId,
    /// Span of the binding *name*; the hint is inserted at `span.end`.
    name_span: Span,
}

/// Visitor that records every bind-pattern under an unannotated `let`
/// binding or a `for`-loop binder. Destructuring patterns (tuples, lists,
/// constructors) contribute one site per bound name.
struct SiteCollector<'a> {
    sites: &'a mut Vec<HintSite>,
}

impl Visitor for SiteCollector<'_> {
    fn visit_node(&mut self, node: &AIRNode) {
        match &node.kind {
            NodeKind::LetBinding {
                ty: None, pattern, ..
            } => collect_bind_pats(pattern, self.sites),
            NodeKind::For { pattern, .. } => collect_bind_pats(pattern, self.sites),
            _ => {}
        }
        walk_node(self, node);
    }
}

/// Recursively collect every `BindPat` under `pattern` (handles
/// destructuring: tuple, list, constructor and record patterns).
fn collect_bind_pats(pattern: &AIRNode, sites: &mut Vec<HintSite>) {
    struct BindPatCollector<'a> {
        sites: &'a mut Vec<HintSite>,
    }

    impl Visitor for BindPatCollector<'_> {
        fn visit_node(&mut self, node: &AIRNode) {
            if let NodeKind::BindPat { name, .. } = &node.kind {
                self.sites.push(HintSite {
                    pattern_id: node.id,
                    name_span: name.span,
                });
            }
            walk_node(self, node);
        }
    }

    BindPatCollector { sites }.visit_node(pattern);
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::Position;

    /// A range that covers any document (end clamps to EOF).
    fn full_range() -> Range {
        Range::new(Position::new(0, 0), Position::new(u32::MAX, 0))
    }

    fn run(src: &str) -> InlayHintsResult {
        inlay_hints(PathBuf::from("test.bock"), src.to_string(), full_range())
    }

    /// Assert that `hint` sits immediately after `name` in `src`.
    fn assert_after_name(src: &str, hint: &TypeHint, name: &str) {
        assert_eq!(
            hint.span.start, hint.span.end,
            "hint span must be zero-width"
        );
        assert!(
            src[..hint.span.start].ends_with(name),
            "hint at offset {} should sit immediately after `{name}`; text before it: {:?}",
            hint.span.start,
            &src[..hint.span.start],
        );
    }

    #[test]
    fn unannotated_let_gets_int_hint_after_name() {
        let src = "\
module m

fn main() {
    let answer = 42
}
";
        let result = run(src);
        assert_eq!(result.hints.len(), 1, "expected exactly one hint");
        assert_eq!(result.hints[0].label, ": Int");
        assert_after_name(src, &result.hints[0], "answer");
    }

    #[test]
    fn annotated_let_gets_no_hint() {
        let src = "\
module m

fn main() {
    let answer: Int = 42
}
";
        let result = run(src);
        assert!(
            result.hints.is_empty(),
            "annotated binding must not produce a hint, got: {:?}",
            result.hints.iter().map(|h| &h.label).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn let_mut_gets_hint_after_name() {
        let src = "\
module m

fn main() {
    let mut count = 1
}
";
        let result = run(src);
        assert_eq!(result.hints.len(), 1);
        assert_eq!(result.hints[0].label, ": Int");
        assert_after_name(src, &result.hints[0], "count");
    }

    #[test]
    fn inferred_generic_list_type() {
        let src = "\
module m

fn main() {
    let xs = [1, 2, 3]
}
";
        let result = run(src);
        assert_eq!(result.hints.len(), 1);
        assert_eq!(result.hints[0].label, ": List[Int]");
        assert_after_name(src, &result.hints[0], "xs");
    }

    #[test]
    fn inferred_optional_from_fn_return() {
        let src = "\
module m

fn find() -> Int? {
    42
}

fn main() {
    let v = find()
}
";
        let result = run(src);
        assert_eq!(result.hints.len(), 1, "expected one hint for `v`");
        assert_eq!(result.hints[0].label, ": Int?");
        assert_after_name(src, &result.hints[0], "v");
    }

    #[test]
    fn error_typed_binding_produces_no_hint() {
        let src = "\
module m

fn main() {
    let x = nonexistent_fn(1)
}
";
        let result = run(src);
        assert!(
            result.hints.is_empty(),
            "error-typed binding must not produce a hint, got: {:?}",
            result.hints.iter().map(|h| &h.label).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn unresolved_lambda_binding_produces_no_hint() {
        // With no call site, the lambda's parameter type stays an
        // inference variable — the hint must be suppressed rather than
        // rendering `Fn(?0) -> ?0`.
        let src = "\
module m

fn main() {
    let f = (x) => x
}
";
        let result = run(src);
        for hint in &result.hints {
            assert!(
                !hint.label.contains('?'),
                "hint must not leak inference variables: {}",
                hint.label,
            );
        }
    }

    #[test]
    fn range_filters_hints_outside_it() {
        let src = "\
module m

fn main() {
    let first = 1
    let second = \"two\"
}
";
        // Full range sees both.
        let all = run(src);
        assert_eq!(all.hints.len(), 2, "full range should see both hints");
        assert_eq!(all.hints[0].label, ": Int");
        assert_eq!(all.hints[1].label, ": String");

        // A range covering only line 4 (`let second = …`) sees one.
        let line4 = Range::new(Position::new(4, 0), Position::new(4, 99));
        let result = inlay_hints(PathBuf::from("test.bock"), src.to_string(), line4);
        assert_eq!(result.hints.len(), 1, "line-4 range should see one hint");
        assert_eq!(result.hints[0].label, ": String");
        assert_after_name(src, &result.hints[0], "second");
    }

    #[test]
    fn tuple_destructuring_hints_each_name() {
        let src = "\
module m

fn main() {
    let (a, b) = (1, \"hi\")
}
";
        let result = run(src);
        assert_eq!(result.hints.len(), 2, "expected a hint per bound name");
        assert_eq!(result.hints[0].label, ": Int");
        assert_after_name(src, &result.hints[0], "a");
        assert_eq!(result.hints[1].label, ": String");
        assert_after_name(src, &result.hints[1], "b");
    }

    #[test]
    fn long_type_render_is_truncated() {
        // 12-element tuple renders to well over the 60-char budget.
        let src = "\
module m

fn main() {
    let t = (1, \"a\", 1, \"a\", 1, \"a\", 1, \"a\", 1, \"a\", 1, \"a\", 1, \"a\")
}
";
        let result = run(src);
        assert_eq!(result.hints.len(), 1);
        let label = &result.hints[0].label;
        assert!(
            label.ends_with('…'),
            "truncated label must end in …: {label}"
        );
        // `: ` prefix plus exactly the budget.
        assert_eq!(
            label.chars().count(),
            2 + TYPE_RENDER_BUDGET,
            "label: {label}"
        );
    }

    #[test]
    fn truncate_render_leaves_short_strings_alone() {
        assert_eq!(truncate_render("Int".to_string()), "Int");
        let exactly = "A".repeat(TYPE_RENDER_BUDGET);
        assert_eq!(truncate_render(exactly.clone()), exactly);
    }

    #[test]
    fn truncate_render_cuts_at_budget() {
        let long = "A".repeat(TYPE_RENDER_BUDGET + 40);
        let out = truncate_render(long);
        assert_eq!(out.chars().count(), TYPE_RENDER_BUDGET);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn for_loop_binder_gets_element_type_hint() {
        let src = "\
module m

fn main() {
    for x in [1, 2, 3] {
        println(\"hi\")
    }
}
";
        let result = run(src);
        assert_eq!(result.hints.len(), 1, "expected one hint for the binder");
        assert_eq!(result.hints[0].label, ": Int");
        assert_after_name(src, &result.hints[0], "x");
    }

    #[test]
    fn range_past_eof_yields_no_hints() {
        let src = "\
module m

fn main() {
    let x = 1
}
";
        let past_eof = Range::new(Position::new(90, 0), Position::new(99, 0));
        let result = inlay_hints(PathBuf::from("test.bock"), src.to_string(), past_eof);
        assert!(result.hints.is_empty());
    }
}
