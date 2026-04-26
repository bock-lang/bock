//! Run the Bock `check` pipeline on a single in-memory document.
//!
//! The LSP processes one document at a time — cross-file name resolution is
//! not performed here because the client only hands us the current buffer's
//! contents. A full workspace build happens via `bock check`; this path is
//! scoped to fast per-keystroke analysis.

use std::path::PathBuf;

use bock_air::{lower_module, resolve_names_with_registry, ModuleRegistry, NodeIdGen, SymbolTable};
use bock_errors::{Diagnostic, DiagnosticBag};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{seed_imports, FnType, PrimitiveType, Strictness, Type, TypeChecker};

/// Result of running the check pipeline on a single document.
pub struct CheckResult {
    /// Owned source map containing the document (keeps `SourceFile`
    /// borrows valid for the lifetime of the result).
    pub source_map: SourceMap,
    /// Id of the added file inside [`CheckResult::source_map`].
    pub file_id: bock_errors::FileId,
    /// All diagnostics produced by any pipeline stage.
    pub diagnostics: Vec<Diagnostic>,
}

/// Run lex → parse → resolve → lower → type-check → analyze on a document.
///
/// The pipeline short-circuits if lexing or parsing produces error-level
/// diagnostics (those stages must succeed before later passes can run), but
/// non-fatal stages downstream all contribute their diagnostics to the
/// returned vector regardless of earlier warnings.
#[must_use]
pub fn check_document(path: PathBuf, content: String) -> CheckResult {
    let mut source_map = SourceMap::new();
    let file_id = source_map.add_file(path, content);
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    // Borrow the just-added file for the lexer. `SourceMap::add_file` appends
    // to an internal Vec, so the reference stays valid as long as we don't
    // add more files (we don't).
    let source_file = source_map.get_file(file_id);

    // 1. Lex
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    push_all(&mut diagnostics, lexer.diagnostics());

    if has_errors(&diagnostics) {
        return CheckResult {
            source_map,
            file_id,
            diagnostics,
        };
    }

    // 2. Parse
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();
    push_all(&mut diagnostics, parser.diagnostics());

    if has_errors(&diagnostics) {
        return CheckResult {
            source_map,
            file_id,
            diagnostics,
        };
    }

    // 3. Resolve names (empty registry — single-file check)
    let registry = ModuleRegistry::new();
    let mut symbols = SymbolTable::new();
    let resolve_diags = resolve_names_with_registry(&module, &mut symbols, &registry);
    push_all(&mut diagnostics, &resolve_diags);

    if has_errors(&diagnostics) {
        return CheckResult {
            source_map,
            file_id,
            diagnostics,
        };
    }

    // 4. Lower to S-AIR
    let id_gen = NodeIdGen::new();
    let mut air_module = lower_module(&module, &id_gen, &symbols);

    // 5. Type check
    let mut checker = TypeChecker::new();
    register_builtins(&mut checker);
    seed_imports(&mut checker, &module.imports, &registry);
    checker.check_module(&mut air_module);
    push_all(&mut diagnostics, &checker.diags);

    // 6. Analysis passes (always run — they are useful even with type errors)
    let ownership_diags = bock_types::analyze_ownership(&air_module);
    push_all(&mut diagnostics, &ownership_diags);

    let strictness = Strictness::Development;
    let effect_diags = bock_types::track_effects(&air_module, strictness);
    push_all(&mut diagnostics, &effect_diags);

    let capability_diags = bock_types::verify_capabilities(&air_module, strictness);
    push_all(&mut diagnostics, &capability_diags);

    CheckResult {
        source_map,
        file_id,
        diagnostics,
    }
}

fn push_all(acc: &mut Vec<Diagnostic>, bag: &DiagnosticBag) {
    for diag in bag.iter() {
        acc.push(diag.clone());
    }
}

fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|d| d.severity == bock_errors::Severity::Error)
}

/// Define the prelude builtins expected by hand-written Bock programs.
///
/// Kept in sync with `bock-cli`'s `register_type_builtins` — the LSP must
/// treat the same set of identifiers as predefined, otherwise buffers that
/// type-check on disk would show spurious "undefined variable" diagnostics
/// in the editor.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_empty_module_has_no_errors() {
        let src = "module m\n";
        let result = check_document(PathBuf::from("test.bock"), src.to_string());
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == bock_errors::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "unexpected errors: {errors:#?}");
    }

    #[test]
    fn syntax_error_produces_diagnostic() {
        // Missing `=` should produce a parse error.
        let src = "module m\nlet x 1\n";
        let result = check_document(PathBuf::from("test.bock"), src.to_string());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.severity == bock_errors::Severity::Error),
            "expected at least one error diagnostic",
        );
    }
}
