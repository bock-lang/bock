//! Implementation of the `bock test` command.
//!
//! Discovers `@test`-annotated functions in Bock source files, runs each in an
//! isolated interpreter environment, and reports pass/fail results with timing.

use std::path::{Path, PathBuf};
use std::time::Instant;

use bock_air::{
    lower_module, resolve_names, Binding, NameKind, NodeIdGen, NodeKind, ResolvedName, SymbolTable,
};
use bock_ast::Visibility;
use bock_errors::{Diagnostic, DiagnosticBag, FileId, Severity, Span};
use bock_interp::Interpreter;
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{FnType, PrimitiveType, Strictness, Type, TypeChecker};

/// Result of running a single test.
struct TestResult {
    /// Fully qualified test name: `file::function_name`.
    name: String,
    /// Whether the test passed.
    passed: bool,
    /// Error message if the test failed.
    error: Option<String>,
    /// How long the test took to run.
    duration: std::time::Duration,
}

/// Run the `bock test` command.
///
/// Discovers `.bock` files, finds `@test`-annotated functions, runs each in an
/// isolated interpreter, and prints a summary. Returns exit code 1 if any test fails.
pub async fn run(filter: Option<String>, files: Vec<PathBuf>) -> anyhow::Result<()> {
    let files = if files.is_empty() {
        discover_bock_files(".")?
    } else {
        files
    };

    if files.is_empty() {
        println!("No .bock files found.");
        return Ok(());
    }

    let total_start = Instant::now();
    let mut results: Vec<TestResult> = Vec::new();

    for file_path in &files {
        match run_tests_in_file(file_path, &filter).await {
            Ok(mut file_results) => results.append(&mut file_results),
            Err(e) => {
                eprintln!("error: {}: {e}", file_path.display());
                results.push(TestResult {
                    name: file_path.display().to_string(),
                    passed: false,
                    error: Some(format!("compilation error: {e}")),
                    duration: std::time::Duration::ZERO,
                });
            }
        }
    }

    let total_duration = total_start.elapsed();

    if results.is_empty() {
        println!("No tests found.");
        return Ok(());
    }

    // Print results
    println!();
    let mut passed = 0usize;
    let mut failed = 0usize;

    for result in &results {
        if result.passed {
            passed += 1;
            println!(
                "  \x1b[32mPASS\x1b[0m {} ({:.1}ms)",
                result.name,
                result.duration.as_secs_f64() * 1000.0
            );
        } else {
            failed += 1;
            println!(
                "  \x1b[31mFAIL\x1b[0m {} ({:.1}ms)",
                result.name,
                result.duration.as_secs_f64() * 1000.0
            );
            if let Some(ref err) = result.error {
                println!("       {err}");
            }
        }
    }

    let total = passed + failed;
    println!();
    println!(
        "Tests: {passed} passed, {failed} failed, {total} total ({:.2}s)",
        total_duration.as_secs_f64()
    );

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Compile a single file and run all `@test` functions found in it.
async fn run_tests_in_file(
    path: &Path,
    filter: &Option<String>,
) -> anyhow::Result<Vec<TestResult>> {
    let content =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;

    let mut source_map = SourceMap::new();
    let file_id = source_map.add_file(path.to_path_buf(), content);
    let source_file = source_map.get_file(file_id);
    let filename = path.display().to_string();

    let mut all_diagnostics: Vec<Diagnostic> = Vec::new();

    // Phase 1: Lex
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    collect_diagnostics(&mut all_diagnostics, lexer.diagnostics());

    if has_errors(&all_diagnostics) {
        return Err(format_errors(
            &all_diagnostics,
            &filename,
            &source_file.content,
        ));
    }

    // Phase 2: Parse
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();
    collect_diagnostics(&mut all_diagnostics, parser.diagnostics());

    if has_errors(&all_diagnostics) {
        return Err(format_errors(
            &all_diagnostics,
            &filename,
            &source_file.content,
        ));
    }

    // Phase 3: Name resolution
    let mut symbols = SymbolTable::new();
    register_builtins(&mut symbols);
    let resolve_diags = resolve_names(&module, &mut symbols);
    collect_diagnostics(&mut all_diagnostics, &resolve_diags);

    if has_errors(&all_diagnostics) {
        return Err(format_errors(
            &all_diagnostics,
            &filename,
            &source_file.content,
        ));
    }

    // Phase 4: Lower to S-AIR
    let id_gen = NodeIdGen::new();
    let mut air_module = lower_module(&module, &id_gen, &symbols);

    // Phase 5: Type check (T-AIR)
    let mut checker = TypeChecker::new();
    register_type_builtins(&mut checker);
    checker.check_module(&mut air_module);
    collect_diagnostics(&mut all_diagnostics, &checker.diags);

    if has_errors(&all_diagnostics) {
        return Err(format_errors(
            &all_diagnostics,
            &filename,
            &source_file.content,
        ));
    }

    // Phase 6: Analysis passes (ownership, effects, capabilities)
    // In development mode, downgrade errors to warnings so they don't block test execution.
    let ownership_diags = bock_types::analyze_ownership(&air_module);
    collect_as_warnings(&mut all_diagnostics, &ownership_diags);

    let strictness = Strictness::Development;
    let effect_diags = bock_types::track_effects(&air_module, strictness);
    collect_as_warnings(&mut all_diagnostics, &effect_diags);

    let capability_diags = bock_types::verify_capabilities(&air_module, strictness);
    collect_as_warnings(&mut all_diagnostics, &capability_diags);

    // Print analysis warnings (don't block test execution)
    let warnings: Vec<&Diagnostic> = all_diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .collect();
    if !warnings.is_empty() {
        let to_render: Vec<Diagnostic> = warnings.into_iter().cloned().collect();
        let rendered = bock_errors::render(&to_render, &filename, &source_file.content);
        eprint!("{rendered}");
    }

    // Extract module items
    let items = match &air_module.kind {
        NodeKind::Module { items, .. } => items.clone(),
        _ => return Err(anyhow::anyhow!("internal: expected Module node")),
    };

    // Discover @test functions
    let test_fns = discover_test_functions(&items, filter);

    if test_fns.is_empty() {
        return Ok(Vec::new());
    }

    // Derive the file stem for test naming
    let file_stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| filename.clone());

    // Run each test in an isolated interpreter
    let mut results = Vec::new();

    for (test_name, _test_node_idx) in &test_fns {
        let qualified_name = format!("{file_stem}::{test_name}");

        let start = Instant::now();
        let result = run_single_test(&items, test_name).await;
        let duration = start.elapsed();

        match result {
            Ok(()) => results.push(TestResult {
                name: qualified_name,
                passed: true,
                error: None,
                duration,
            }),
            Err(e) => results.push(TestResult {
                name: qualified_name,
                passed: false,
                error: Some(e),
                duration,
            }),
        }
    }

    Ok(results)
}

/// Run a single test function in a fresh interpreter environment.
async fn run_single_test(items: &[bock_air::AIRNode], test_name: &str) -> Result<(), String> {
    let mut interp = Interpreter::new();
    bock_core::register_core(&mut interp.builtins);

    // Register test assertion builtins (expect, to_equal, etc.)
    interp.builtins.register_test_builtins();

    // Register all top-level functions
    for item in items {
        match &item.kind {
            NodeKind::FnDecl {
                name,
                params,
                body,
                is_async,
                ..
            } => {
                let param_names: Vec<String> =
                    params.iter().filter_map(extract_param_name).collect();
                interp.register_fn_with_async(&name.name, param_names, *body.clone(), *is_async);
            }
            NodeKind::EnumDecl { name, variants, .. } => {
                interp.register_enum(&name.name, variants);
            }
            NodeKind::ConstDecl { name, value, .. } => match interp.eval_expr(value).await {
                Ok(val) => interp.env.define(&name.name, val),
                Err(e) => return Err(format!("setup error: {e}")),
            },
            NodeKind::ImplBlock {
                target, methods, ..
            } => {
                interp.register_impl(target, methods);
            }
            NodeKind::LetBinding { .. } | NodeKind::ModuleHandle { .. } => {
                if let Err(e) = interp.eval_expr(item).await {
                    return Err(format!("setup error: {e}"));
                }
            }
            _ => {}
        }
    }

    // Look up and call the test function
    let test_val = match interp.env.get(test_name) {
        Some(v) => v.clone(),
        None => {
            return Err(format!(
                "test function '{test_name}' not found in environment"
            ))
        }
    };

    match interp.call_fn_value(&test_val, vec![]).await {
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Discover `@test`-annotated function declarations in the AIR items.
///
/// Returns a list of `(function_name, index)` pairs, optionally filtered
/// by a pattern (substring match on the function name).
fn discover_test_functions(
    items: &[bock_air::AIRNode],
    filter: &Option<String>,
) -> Vec<(String, usize)> {
    let mut tests = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        if let NodeKind::FnDecl {
            name, annotations, ..
        } = &item.kind
        {
            let is_test = annotations.iter().any(|a| a.name.name == "test");
            if is_test {
                let fn_name = name.name.clone();
                // Apply filter if present
                if let Some(ref pattern) = filter {
                    if !fn_name.contains(pattern.as_str()) {
                        continue;
                    }
                }
                tests.push((fn_name, idx));
            }
        }
    }

    tests
}

/// Extract the parameter name from a Param AIR node.
fn extract_param_name(param: &bock_air::AIRNode) -> Option<String> {
    if let NodeKind::Param { pattern, .. } = &param.kind {
        if let NodeKind::BindPat { name, .. } = &pattern.kind {
            return Some(name.name.clone());
        }
    }
    None
}

/// Collect diagnostics from a bag into the accumulator.
fn collect_diagnostics(acc: &mut Vec<Diagnostic>, bag: &DiagnosticBag) {
    for diag in bag.iter() {
        acc.push(diag.clone());
    }
}

/// Collect diagnostics, downgrading errors to warnings so they don't block execution.
fn collect_as_warnings(acc: &mut Vec<Diagnostic>, bag: &DiagnosticBag) {
    for diag in bag.iter() {
        let mut d = diag.clone();
        if d.severity == Severity::Error {
            d.severity = Severity::Warning;
        }
        acc.push(d);
    }
}

/// Check if any diagnostic in the list is an error.
fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

/// Format error diagnostics into an anyhow error.
fn format_errors(diagnostics: &[Diagnostic], filename: &str, source: &str) -> anyhow::Error {
    let rendered = bock_errors::render(diagnostics, filename, source);
    anyhow::anyhow!("{rendered}")
}

/// Register builtin function types in the type checker.
fn register_type_builtins(checker: &mut TypeChecker) {
    let io_fn_ty = Type::Function(FnType {
        params: vec![Type::Primitive(PrimitiveType::String)],
        ret: Box::new(Type::Primitive(PrimitiveType::Void)),
        effects: vec![],
    });
    for name in ["print", "println", "debug"] {
        checker.env.define(name, io_fn_ty.clone());
    }

    // Duration, Instant, Channel: named prelude types. Type::Error accepts
    // any method or associated-function access without friction; runtime
    // dispatch via qualified globals handles correctness.
    for name in ["Duration", "Instant", "Channel"] {
        checker.env.define(name, Type::Error);
    }

    // sleep: (Duration) -> Void with Clock (effect elided at this layer)
    let sleep_fn_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Primitive(PrimitiveType::Void)),
        effects: vec![],
    });
    checker.env.define("sleep", sleep_fn_ty);

    // spawn: (Future[T]) -> Future[T]
    let spawn_fn_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Error),
        effects: vec![],
    });
    checker.env.define("spawn", spawn_fn_ty);

    // assert: (Bool, String?) -> Void
    let assert_ty = Type::Function(FnType {
        params: vec![Type::Primitive(PrimitiveType::Bool)],
        ret: Box::new(Type::Primitive(PrimitiveType::Void)),
        effects: vec![],
    });
    checker.env.define("assert", assert_ty);

    // expect: (Any) -> Expectation (test assertion builtin)
    let expect_fn_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Error),
        effects: vec![],
    });
    checker.env.define("expect", expect_fn_ty);

    // todo, unreachable: () -> Never (diverging builtins)
    let never_fn_ty = Type::Function(FnType {
        params: vec![],
        ret: Box::new(Type::Primitive(PrimitiveType::Never)),
        effects: vec![],
    });
    for name in ["todo", "unreachable"] {
        checker.env.define(name, never_fn_ty.clone());
    }
}

/// Register interpreter builtin globals in the symbol table.
fn register_builtins(symbols: &mut SymbolTable) {
    let builtin_span = Span {
        file: FileId(0),
        start: 0,
        end: 0,
    };
    let builtins = [
        ("print", u32::MAX - 1),
        ("println", u32::MAX - 2),
        ("debug", u32::MAX - 3),
        ("expect", u32::MAX - 4),
    ];
    for (name, def_id) in builtins {
        symbols.define(
            name.to_string(),
            Binding {
                name: name.to_string(),
                resolved: ResolvedName {
                    def_id,
                    kind: NameKind::Function,
                },
                visibility: Visibility::Public,
                span: builtin_span,
                used: true,
                is_import: false,
            },
        );
    }
}

/// Recursively discover `.bock` files in the given directory.
fn discover_bock_files(dir: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    discover_bock_files_recursive(Path::new(dir), &mut files)?;
    files.sort();
    Ok(files)
}

/// Recursive helper for file discovery.
fn discover_bock_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("could not read directory '{}': {e}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden directories and common non-source dirs
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.starts_with('.') && name_str != "target" && name_str != "node_modules" {
                discover_bock_files_recursive(&path, files)?;
            }
        } else if path.is_file() {
            if let Some(ext) = path.extension() {
                if ext == "bock" {
                    files.push(path);
                }
            }
        }
    }

    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a temp dir with a Bock test file and run tests on it.
    async fn run_test_on_source(source: &str) -> Vec<TestResult> {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.bock");
        fs::write(&file_path, source).unwrap();

        let mut source_map = SourceMap::new();
        let file_id = source_map.add_file(file_path.clone(), source.to_string());
        let source_file = source_map.get_file(file_id);

        // Lex
        let mut lexer = Lexer::new(source_file);
        let tokens = lexer.tokenize();

        // Parse
        let mut parser = Parser::new(tokens, source_file);
        let module = parser.parse_module();

        // Name resolution
        let mut symbols = SymbolTable::new();
        register_builtins(&mut symbols);
        let _resolve_diags = resolve_names(&module, &mut symbols);

        // Lower
        let id_gen = NodeIdGen::new();
        let mut air_module = lower_module(&module, &id_gen, &symbols);

        // Type check
        let mut checker = TypeChecker::new();
        register_type_builtins(&mut checker);
        checker.check_module(&mut air_module);

        let items = match &air_module.kind {
            NodeKind::Module { items, .. } => items.clone(),
            _ => panic!("expected Module node"),
        };

        let test_fns = discover_test_functions(&items, &None);
        let file_stem = "test";

        let mut results = Vec::new();
        for (test_name, _) in &test_fns {
            let qualified_name = format!("{file_stem}::{test_name}");
            let start = Instant::now();
            let result = run_single_test(&items, test_name).await;
            let duration = start.elapsed();
            match result {
                Ok(()) => results.push(TestResult {
                    name: qualified_name,
                    passed: true,
                    error: None,
                    duration,
                }),
                Err(e) => results.push(TestResult {
                    name: qualified_name,
                    passed: false,
                    error: Some(e),
                    duration,
                }),
            }
        }

        results
    }

    #[tokio::test]
    async fn test_passing_test() {
        let source = r#"
@test
fn test_addition() {
    expect(1 + 1).to_equal(2)
}
"#;
        let results = run_test_on_source(source).await;
        assert_eq!(results.len(), 1);
        assert!(
            results[0].passed,
            "test should pass: {:?}",
            results[0].error
        );
    }

    #[tokio::test]
    async fn test_failing_test() {
        let source = r#"
@test
fn test_bad_math() {
    expect(1 + 1).to_equal(3)
}
"#;
        let results = run_test_on_source(source).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].passed);
        assert!(results[0]
            .error
            .as_ref()
            .unwrap()
            .contains("assertion failed"));
    }

    #[tokio::test]
    async fn test_multiple_tests() {
        let source = r#"
@test
fn test_one() {
    expect(true).to_be_true()
}

@test
fn test_two() {
    expect(false).to_be_false()
}

fn helper() {
    println("not a test")
}
"#;
        let results = run_test_on_source(source).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.passed));
    }

    #[tokio::test]
    async fn test_filter_matches() {
        let source = r#"
@test
fn test_alpha() {
    expect(1).to_equal(1)
}

@test
fn test_beta() {
    expect(2).to_equal(2)
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.bock");
        fs::write(&file_path, source).unwrap();

        let mut source_map = SourceMap::new();
        let file_id = source_map.add_file(file_path, source.to_string());
        let source_file = source_map.get_file(file_id);

        let mut lexer = Lexer::new(source_file);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens, source_file);
        let module = parser.parse_module();
        let mut symbols = SymbolTable::new();
        register_builtins(&mut symbols);
        resolve_names(&module, &mut symbols);
        let id_gen = NodeIdGen::new();
        let mut air_module = lower_module(&module, &id_gen, &symbols);
        let mut checker = TypeChecker::new();
        register_type_builtins(&mut checker);
        checker.check_module(&mut air_module);

        let items = match &air_module.kind {
            NodeKind::Module { items, .. } => items.clone(),
            _ => panic!("expected Module"),
        };

        let filter = Some("alpha".to_string());
        let tests = discover_test_functions(&items, &filter);
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].0, "test_alpha");
    }

    #[tokio::test]
    async fn test_no_test_functions() {
        let source = r#"
fn not_a_test() {
    println("hello")
}
"#;
        let results = run_test_on_source(source).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_discover_bock_files_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.path().join("a.bock"), "").unwrap();
        fs::write(sub.join("b.bock"), "").unwrap();
        fs::write(dir.path().join("c.txt"), "").unwrap();

        let files = discover_bock_files(&dir.path().to_string_lossy()).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.extension().unwrap() == "bock"));
    }

    #[tokio::test]
    async fn test_expect_to_be_some() {
        let source = r#"
@test
fn test_some() {
    let xs = [1, 2, 3]
    expect(xs.get(0)).to_be_some()
}
"#;
        let results = run_test_on_source(source).await;
        assert_eq!(results.len(), 1);
        assert!(
            results[0].passed,
            "test should pass: {:?}",
            results[0].error
        );
    }

    #[tokio::test]
    async fn test_expect_to_be_none() {
        let source = r#"
@test
fn test_none() {
    let xs = [1, 2, 3]
    expect(xs.get(10)).to_be_none()
}
"#;
        let results = run_test_on_source(source).await;
        assert_eq!(results.len(), 1);
        assert!(
            results[0].passed,
            "test should pass: {:?}",
            results[0].error
        );
    }

    #[tokio::test]
    async fn test_isolated_environments() {
        // Each test should run in its own environment
        let source = r#"
@test
fn test_a() {
    expect(1 + 1).to_equal(2)
}

@test
fn test_b() {
    expect(2 + 2).to_equal(4)
}
"#;
        let results = run_test_on_source(source).await;
        assert_eq!(results.len(), 2);
        assert!(
            results.iter().all(|r| r.passed),
            "all tests should pass in isolated envs: {:?}",
            results
                .iter()
                .map(|r| (&r.name, &r.error))
                .collect::<Vec<_>>()
        );
    }
}
