//! Cross-module ABI integration tests (Q-xmod-bounds / Q-xmod-impl).
//!
//! These exercise the real compile pipeline end to end for two modules:
//! a *provider* module is fully compiled and its exports collected and
//! registered, then a *consumer* module that `use`s it is checked against the
//! registry — mirroring the driver (`bock check`) order
//! `resolve → lower → seed_imports → check_module → collect_exports`.
//!
//! Two cross-module gaps are pinned:
//!
//! 1. **Q-xmod-bounds** — a where-clause trait bound on an *imported* generic
//!    function must be enforced at the call site. A violating call must ERROR;
//!    a satisfying call must check clean.
//! 2. **Q-xmod-impl** — an `impl From[A] for B` (and its blanket `Into`)
//!    declared in the provider must make `a.into()` resolve in the consumer.
//!    (Check-only: the runtime lowering of the bodyless blanket `.into()` is
//!    owned by the sibling Q-blanket-into-codegen lane.)

use bock_air::registry::ModuleRegistry;
use bock_air::{interpret_context, lower_module, resolve_names_with_registry, NodeIdGen};
use bock_ast::Module;
use bock_errors::{FileId, Severity};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceFile;
use bock_types::{collect_exports, seed_imports, TypeChecker};

use std::path::PathBuf;

/// Parse a single source string into an AST `Module`. Panics on lex/parse
/// errors (the fixtures are hand-written and must parse cleanly).
fn parse(src: &str, file_id_no: u32) -> Module {
    let source = SourceFile::new(
        FileId(file_id_no),
        PathBuf::from(format!("test_{file_id_no}.bock")),
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

/// Module id from the `module <name>` declaration.
fn module_id(module: &Module) -> String {
    module
        .path
        .as_ref()
        .map(|p| {
            p.segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".")
        })
        .unwrap_or_default()
}

/// Compile the provider source and register its exports in `registry`.
fn compile_and_register(registry: &mut ModuleRegistry, src: &str, file_id_no: u32) {
    let module = parse(src, file_id_no);
    let id = module_id(&module);

    let mut symbols = bock_air::SymbolTable::new();
    let resolve_diags = resolve_names_with_registry(&module, &mut symbols, registry);
    assert!(
        !resolve_diags.has_errors(),
        "provider resolve errors:\n{src}",
    );

    let id_gen = NodeIdGen::new();
    let mut air = lower_module(&module, &id_gen, &symbols);
    let _ = interpret_context(&mut air);

    let mut checker = TypeChecker::new();
    seed_imports(&mut checker, &module.imports, registry);
    checker.check_module(&mut air);
    assert!(
        !checker.diags.has_errors(),
        "provider check errors:\n{src}\n{:?}",
        checker
            .diags
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>(),
    );

    let exports = collect_exports(&id, &PathBuf::from("provider.bock"), &checker, &air);
    registry.register(exports);
}

/// Compile the consumer source against `registry` and return its checker
/// diagnostics' error count plus the joined messages.
fn check_consumer(registry: &ModuleRegistry, src: &str, file_id_no: u32) -> (usize, String) {
    let module = parse(src, file_id_no);

    let mut symbols = bock_air::SymbolTable::new();
    let resolve_diags = resolve_names_with_registry(&module, &mut symbols, registry);
    // Resolve errors would mask the type-checking signal; surface them.
    assert!(
        !resolve_diags.has_errors(),
        "consumer resolve errors:\n{src}\n{:?}",
        resolve_diags
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>(),
    );

    let id_gen = NodeIdGen::new();
    let mut air = lower_module(&module, &id_gen, &symbols);
    let _ = interpret_context(&mut air);

    let mut checker = TypeChecker::new();
    seed_imports(&mut checker, &module.imports, registry);
    checker.check_module(&mut air);

    let msgs = checker
        .diags
        .iter()
        .map(|d| format!("{} {}", d.code, d.message))
        .collect::<Vec<_>>()
        .join("\n");
    (checker.diags.error_count(), msgs)
}

// ─── Q-xmod-bounds ────────────────────────────────────────────────────────────

/// Provider declares a trait `Show`, an `impl Show for Widget`, and a generic
/// `fn render[T](x: T) -> Int where (T: Show)`.
const BOUNDS_PROVIDER: &str = r#"module provider

public trait Show {
  fn show(self) -> Int
}

public record Widget {
  size: Int
}

impl Show for Widget {
  fn show(self) -> Int { self.size }
}

public record Gadget {
  weight: Int
}

public fn render[T](x: T) -> Int where (T: Show) {
  42
}
"#;

#[test]
fn xmod_where_bound_satisfied_checks_clean() {
    let mut registry = ModuleRegistry::new();
    compile_and_register(&mut registry, BOUNDS_PROVIDER, 1);

    // Widget implements Show (impl is in the provider, visible cross-module),
    // so render(Widget) satisfies the bound.
    let consumer = r#"module consumer
use provider.{ render, Widget }

fn main() -> Int {
  let w = Widget { size: 3 }
  render(w)
}
"#;
    let (errs, msgs) = check_consumer(&registry, consumer, 2);
    assert_eq!(
        errs, 0,
        "expected satisfied cross-module bound to check clean, got:\n{msgs}"
    );
}

#[test]
fn xmod_where_bound_violated_emits_error() {
    let mut registry = ModuleRegistry::new();
    compile_and_register(&mut registry, BOUNDS_PROVIDER, 1);

    // Gadget does NOT implement Show, so render(Gadget) violates `T: Show`.
    // Before the export-ABI threading, the imported fn's where-clause was
    // dropped and this call was wrongly accepted.
    let consumer = r#"module consumer
use provider.{ render, Gadget }

fn main() -> Int {
  let g = Gadget { weight: 9 }
  render(g)
}
"#;
    let (errs, msgs) = check_consumer(&registry, consumer, 2);
    assert!(
        errs >= 1,
        "expected a cross-module bound violation error, got none.\n{msgs}"
    );
    assert!(
        msgs.contains("Show") || msgs.contains("bound"),
        "expected the bound diagnostic to mention the unsatisfied trait, got:\n{msgs}"
    );
}

// ─── Q-xmod-impl ──────────────────────────────────────────────────────────────

/// Provider declares two records and `impl From[Celsius] for Fahrenheit`.
const IMPL_PROVIDER: &str = r#"module conv

public record Celsius {
  deg: Int
}

public record Fahrenheit {
  deg: Int
}

impl From[Celsius] for Fahrenheit {
  fn from(c: Celsius) -> Fahrenheit { Fahrenheit { deg: c.deg } }
}
"#;

#[test]
fn xmod_into_resolves_via_imported_from_impl() {
    let mut registry = ModuleRegistry::new();
    compile_and_register(&mut registry, IMPL_PROVIDER, 1);

    // The blanket `Into[Fahrenheit] for Celsius` derived from the imported
    // `From[Celsius] for Fahrenheit` must let `c.into()` (target Fahrenheit)
    // resolve in the consumer. Check-only — runtime lowering is the sibling
    // codegen lane's responsibility.
    let consumer = r#"module consumer
use conv.{ Celsius, Fahrenheit }

fn main() -> Int {
  let c = Celsius { deg: 100 }
  let f: Fahrenheit = c.into()
  f.deg
}
"#;
    let (errs, msgs) = check_consumer(&registry, consumer, 2);
    assert_eq!(
        errs, 0,
        "expected cross-module `.into()` to resolve via the imported `From` impl, got:\n{msgs}"
    );
}

#[test]
fn xmod_into_unrelated_target_still_errors() {
    let mut registry = ModuleRegistry::new();
    compile_and_register(&mut registry, IMPL_PROVIDER, 1);

    // No `From`/`Into` relates Celsius to Int, so `.into()` to Int must still
    // error — the fold must not over-accept unrelated conversions.
    let consumer = r#"module consumer
use conv.{ Celsius }

fn main() -> Int {
  let c = Celsius { deg: 100 }
  let n: Int = c.into()
  n
}
"#;
    let (errs, _msgs) = check_consumer(&registry, consumer, 2);
    assert!(
        errs >= 1,
        "expected an unrelated `.into()` target to still error"
    );
}
