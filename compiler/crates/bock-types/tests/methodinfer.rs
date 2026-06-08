//! Method-level type-parameter inference at call sites
//! (Q-checker-method-generic-call-infer).
//!
//! A method that declares its OWN type parameter — e.g.
//! `Box[T].map[U](f: Fn(T) -> U) -> Box[U]` — must have that parameter (`U`)
//! inferred from the *call arguments* at the call site, exactly as a free
//! function's type parameters already are. The receiver pins the type's own
//! params (`T`); the method's own params (`U`) are open and must unify against
//! the argument types.
//!
//! Before the fix, a *call* `b.map(dbl)` left `U` as a dangling abstract
//! `Named("U")` (the method signature was collected with an empty generic-param
//! map, so `U` never became a fresh inference variable). That failed to unify
//! with the closure's return type and produced a spurious type error, which is
//! why the type-zoo exerciser only *declared* `Box.map` and never called it.
//!
//! These are CHECK-ONLY tests: they exercise the real
//! `resolve → lower → check_module` pipeline and assert on the checker's
//! diagnostics. A positive call must check clean; a wrong-typed argument must
//! still ERROR (so the fix does not over-accept).

use bock_air::{interpret_context, lower_module, resolve_names_with_registry, NodeIdGen};
use bock_ast::Module;
use bock_errors::{FileId, Severity};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceFile;
use bock_types::TypeChecker;

use std::path::PathBuf;

/// Parse a single source string into an AST `Module`. Panics on lex/parse
/// errors (the fixtures are hand-written and must parse cleanly).
fn parse(src: &str) -> Module {
    let source = SourceFile::new(
        FileId(1),
        PathBuf::from("methodinfer_test.bock"),
        src.to_string(),
    );
    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize();
    assert!(
        lexer
            .diagnostics()
            .iter()
            .all(|d| d.severity != Severity::Error),
        "lexer errors in fixture:\n{}\nsrc:\n{src}",
        lexer
            .diagnostics()
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mut parser = Parser::new(tokens, &source);
    let module = parser.parse_module();
    assert!(
        parser
            .diagnostics()
            .iter()
            .all(|d| d.severity != Severity::Error),
        "parser errors in fixture:\n{}\nsrc:\n{src}",
        parser
            .diagnostics()
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    module
}

/// Run the real `resolve → lower → check_module` pipeline on a single module
/// and return `(error_count, joined_messages)`.
fn check(src: &str) -> (usize, String) {
    let module = parse(src);

    let mut symbols = bock_air::SymbolTable::new();
    let registry = bock_air::registry::ModuleRegistry::new();
    let resolve_diags = resolve_names_with_registry(&module, &mut symbols, &registry);
    assert!(
        !resolve_diags.has_errors(),
        "resolve errors:\n{src}\n{:?}",
        resolve_diags
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>(),
    );

    let id_gen = NodeIdGen::new();
    let mut air = lower_module(&module, &id_gen, &symbols);
    let _ = interpret_context(&mut air);

    let mut checker = TypeChecker::new();
    checker.check_module(&mut air);

    let msgs = checker
        .diags
        .iter()
        .map(|d| format!("{} {}", d.code, d.message))
        .collect::<Vec<_>>()
        .join("\n");
    (checker.diags.error_count(), msgs)
}

/// Positive: calling a generic method that introduces its own type parameter
/// must infer that parameter from the call argument and check clean.
///
/// `Box[Int].map[U](dbl)` with `dbl: Fn(Int) -> Int` infers `U = Int` and
/// yields `Box[Int]`.
#[test]
fn box_map_own_type_param_inferred_at_call() {
    let src = r#"module m

public record Box[T] {
  value: T
}

impl Box {
  public fn map[U](self, f: Fn(T) -> U) -> Box[U] {
    Box { value: f(self.value) }
  }
}

fn dbl(x: Int) -> Int { x * 2 }

fn needs_int(n: Int) -> Void {}

fn use_it() -> Void {
  let b = Box { value: 21 }
  let mapped = b.map(dbl)
  needs_int(mapped.value)
}
"#;
    let (errors, msgs) = check(src);
    assert_eq!(
        errors, 0,
        "calling a generic method with its own type param should check clean, got:\n{msgs}"
    );
}

/// Positive: the method's own type param is inferred to a *different* type than
/// the receiver's pinned param. `Box[Int].map(to_str)` with
/// `to_str: Fn(Int) -> String` infers `U = String`, yielding `Box[String]`,
/// and a `String`-typed use of the result must check clean.
#[test]
fn box_map_infers_distinct_result_type() {
    let src = r#"module m

public record Box[T] {
  value: T
}

impl Box {
  public fn map[U](self, f: Fn(T) -> U) -> Box[U] {
    Box { value: f(self.value) }
  }
}

fn to_str(x: Int) -> String { "v" }

fn needs_string(s: String) -> Void {}

fn use_it() -> Void {
  let b = Box { value: 21 }
  let mapped = b.map(to_str)
  needs_string(mapped.value)
}
"#;
    let (errors, msgs) = check(src);
    assert_eq!(
        errors, 0,
        "the method's own type param should infer a result type distinct from \
         the receiver's, got:\n{msgs}"
    );
}

/// Negative: a wrong-typed argument to the generic method must still ERROR.
/// `Box[Int].map` expects `f: Fn(Int) -> U`; passing `bad: Fn(String) -> Int`
/// cannot unify the closure's parameter (`String`) with the receiver's pinned
/// `T = Int`, so the call must be rejected — the fix must not over-accept.
#[test]
fn box_map_wrong_arg_type_still_errors() {
    let src = r#"module m

public record Box[T] {
  value: T
}

impl Box {
  public fn map[U](self, f: Fn(T) -> U) -> Box[U] {
    Box { value: f(self.value) }
  }
}

fn bad(x: String) -> Int { 0 }

fn use_it() -> Void {
  let b = Box { value: 21 }
  let mapped = b.map(bad)
  let _ = mapped.value
}
"#;
    let (errors, _msgs) = check(src);
    assert!(
        errors > 0,
        "passing a closure whose param type conflicts with the receiver's \
         pinned type param must be rejected"
    );
}
