//! Implementation of the `bock test` command.
//!
//! Discovers `@test`-annotated functions in Bock source files, runs each in an
//! isolated interpreter environment, and reports pass/fail results with timing.
//!
//! Like `check`/`run`/`build`, the test runner compiles each test file through
//! the **full multi-file pipeline** with the embedded core stdlib prepended:
//! the parsed `core.*` sources flow through the same dependency-sort →
//! per-module compile → `ModuleRegistry` registration path, so a test file's
//! `use core.<name>.{...}` resolves with no special-casing. Each `@test`
//! function then runs in a fresh interpreter that has every compiled core
//! module registered (in dependency order) alongside the test module's own
//! declarations, plus the interpreter-only assertion builtins (`expect`,
//! `assert`, `to_equal`, …) from `register_test_builtins`. The bare-builtin
//! assertion form and `use core.*` imports therefore coexist.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use bock_air::{
    lower_module, resolve_names_with_registry, Binding, ModuleRegistry, NameKind, NodeIdGen,
    NodeKind, ResolvedName, SymbolTable,
};
use bock_ast::Visibility;
use bock_build::dep_graph::{self, DepGraph};
use bock_errors::{Diagnostic, DiagnosticBag, FileId, Severity, Span};
use bock_interp::Interpreter;
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{
    collect_exports, seed_imports, seed_prelude, FnType, PrimitiveType, Strictness, Type,
    TypeChecker,
};

use crate::check::CheckOutcome;
use crate::output::{print_document, OutputFormat, FORMAT_VERSION};

/// Result of running a single test.
struct TestResult {
    /// Fully qualified test name: `file::function_name`.
    name: String,
    /// The `.bock` file the test lives in (as given on the command line or
    /// discovered).
    file: String,
    /// Whether the test passed.
    passed: bool,
    /// Error message if the test failed.
    error: Option<String>,
    /// How long the test took to run.
    duration: std::time::Duration,
}

/// A compiled test file: the AIR modules to register in each test interpreter
/// (in dependency order, core first) plus the items of the user test module
/// itself (where `@test` functions are discovered).
struct CompiledTestFile {
    /// AIR modules in dependency (topological) order — core modules first,
    /// then the user test module last. Each fresh test interpreter registers
    /// these in this order so dependencies (the core modules a test imports)
    /// are available before the test module's own declarations.
    air_modules_in_order: Vec<bock_air::AIRNode>,
    /// The top-level items of the user test module, used for `@test` discovery.
    test_items: Vec<bock_air::AIRNode>,
}

/// Run the `bock test` command.
///
/// Discovers `.bock` files, finds `@test`-annotated functions, runs each in an
/// isolated interpreter, and reports per-test results plus a summary — human
/// lines by default, or one JSON document on stdout with `--format json` (see
/// [`crate::output`]).
///
/// Returns the pass/fail [`CheckOutcome`] rather than exiting the process, so
/// the exit-code decision stays centralized in `main` (mirroring `bock
/// check`): any failing test maps to a non-zero exit, in both formats.
pub async fn run(
    filter: Option<String>,
    files: Vec<PathBuf>,
    format: OutputFormat,
) -> anyhow::Result<CheckOutcome> {
    let files = if files.is_empty() {
        discover_bock_files(Path::new("."))?
    } else {
        files
    };

    if files.is_empty() {
        match format {
            OutputFormat::Human => println!("No .bock files found."),
            OutputFormat::Json => print_document(&test_document(&[]))?,
        }
        return Ok(CheckOutcome::Clean);
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
                    file: file_path.display().to_string(),
                    passed: false,
                    error: Some(format!("compilation error: {e}")),
                    duration: std::time::Duration::ZERO,
                });
            }
        }
    }

    let total_duration = total_start.elapsed();

    if results.is_empty() {
        match format {
            OutputFormat::Human => println!("No tests found."),
            OutputFormat::Json => print_document(&test_document(&results))?,
        }
        return Ok(CheckOutcome::Clean);
    }

    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.len() - passed;

    match format {
        OutputFormat::Human => print_human_results(&results, passed, failed, total_duration),
        OutputFormat::Json => print_document(&test_document(&results))?,
    }

    Ok(if failed > 0 {
        CheckOutcome::Failed
    } else {
        CheckOutcome::Clean
    })
}

/// Print the per-test PASS/FAIL lines and the closing summary (human mode).
fn print_human_results(
    results: &[TestResult],
    passed: usize,
    failed: usize,
    total_duration: std::time::Duration,
) {
    println!();
    for result in results {
        if result.passed {
            println!(
                "  \x1b[32mPASS\x1b[0m {} ({:.1}ms)",
                result.name,
                result.duration.as_secs_f64() * 1000.0
            );
        } else {
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
}

/// Build the `bock test --format json` document (see [`crate::output`] for
/// the shared envelope contract). `message` is `null` for a passing test and
/// the failure text (assertion message or compile error) otherwise.
fn test_document(results: &[TestResult]) -> serde_json::Value {
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.len() - passed;
    let tests: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "file": r.file,
                "passed": r.passed,
                "message": r.error,
                "duration_ms": r.duration.as_secs_f64() * 1000.0,
            })
        })
        .collect();
    serde_json::json!({
        "format_version": FORMAT_VERSION,
        "command": "test",
        "outcome": if failed == 0 { "clean" } else { "failed" },
        "summary": {
            "tests": results.len(),
            "passed": passed,
            "failed": failed,
        },
        "tests": tests,
    })
}

/// Compile a single test file and run all `@test` functions found in it.
async fn run_tests_in_file(
    path: &Path,
    filter: &Option<String>,
) -> anyhow::Result<Vec<TestResult>> {
    let compiled = compile_test_file(path)?;

    // Discover @test functions in the user test module.
    let test_fns = discover_test_functions(&compiled.test_items, filter);

    if test_fns.is_empty() {
        return Ok(Vec::new());
    }

    // Derive the file stem for test naming
    let filename = path.display().to_string();
    let file_stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| filename.clone());

    // Run each test in an isolated interpreter
    let mut results = Vec::new();

    for (test_name, _test_node_idx) in &test_fns {
        let qualified_name = format!("{file_stem}::{test_name}");

        let start = Instant::now();
        let result = run_single_test(&compiled.air_modules_in_order, test_name).await;
        let duration = start.elapsed();

        match result {
            Ok(()) => results.push(TestResult {
                name: qualified_name,
                file: filename.clone(),
                passed: true,
                error: None,
                duration,
            }),
            Err(e) => results.push(TestResult {
                name: qualified_name,
                file: filename.clone(),
                passed: false,
                error: Some(e),
                duration,
            }),
        }
    }

    Ok(results)
}

/// Compile a test file through the full multi-file pipeline with the embedded
/// core stdlib prepended.
///
/// Mirrors the loading the other CLI commands (`check`/`run`/`build`) perform:
/// the parsed `core.*` sources are prepended to the parsed-files set, a
/// dependency graph (with implicit prelude edges from the user module to every
/// core module) is topologically sorted, and each module is compiled in order
/// with cross-file name resolution + type seeding against the
/// [`ModuleRegistry`]. The result carries the compiled AIR modules in
/// dependency order (for per-test interpreter registration) and the user test
/// module's items (for `@test` discovery).
fn compile_test_file(path: &Path) -> anyhow::Result<CompiledTestFile> {
    let content =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;

    // ── Phase 1: Parse the embedded core sources + the test file ──────────────
    let mut source_map = SourceMap::new();
    let mut parsed_files: Vec<ParsedFile> = Vec::new();

    // Prepend the embedded core-stdlib sources so they compile, register, and
    // become available to the interpreter before the test module — exactly as
    // `check`/`run` do — letting a test file resolve and run `use core.*`.
    for src in crate::stdlib::core_sources() {
        let pf = parse_stdlib_source(&src, &mut source_map)?;
        parsed_files.push(pf);
    }

    // Sibling-module discovery (Q-test-interp-crossfile-use): mirror the
    // project resolution `bock run` performs. Walking up from the test file,
    // a `bock.project` marker pins the project root; every `.bock` file under
    // it is parsed alongside the test file so cross-file imports (`use
    // main.{...}` from `test/<name>_test.bock` against `src/main.bock` — the
    // standard `examples/*/test/` layout) resolve through the same dependency
    // sort → per-module compile → `ModuleRegistry` path the compiled-target
    // project build uses. Outside any project the behavior is unchanged: the
    // test file compiles alone (sweeping the CWD would slurp unrelated `.bock`
    // files, exactly the hazard `bock run` avoids for explicit entries).
    let entry_canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Some(root) = find_project_root(path) {
        for sibling in discover_bock_files(&root)? {
            let sibling_canonical = sibling.canonicalize().unwrap_or_else(|_| sibling.clone());
            if sibling_canonical == entry_canonical {
                continue;
            }
            let sibling_content = std::fs::read_to_string(&sibling)
                .map_err(|e| anyhow::anyhow!("{}: {e}", sibling.display()))?;
            let pf = parse_user_file(&sibling, &sibling_content, &mut source_map)?;
            parsed_files.push(pf);
        }
    }

    // The user test file is the entry; remember which parsed index it lands at.
    let test_pf = parse_user_file(path, &content, &mut source_map)?;
    let test_idx = parsed_files.len();
    parsed_files.push(test_pf);

    // ── Phase 2: Build dependency graph ───────────────────────────────────────
    let mut dep_graph = DepGraph::new();
    let mut id_to_index: HashMap<String, usize> = HashMap::new();

    // Embedded core (`is_stdlib`) module ids — every user module implicitly
    // depends on them so the §18.2 prelude can seed core-defined symbols even
    // without an explicit `use` (see `check::run`/`run::run_project`).
    let core_module_ids: Vec<String> = parsed_files
        .iter()
        .enumerate()
        .filter(|(_, pf)| pf.is_stdlib)
        .map(|(i, pf)| dep_graph::module_id_from_module(&pf.module, i))
        .collect();

    for (i, pf) in parsed_files.iter().enumerate() {
        let module_id = dep_graph::module_id_from_module(&pf.module, i);
        let mut deps = dep_graph::extract_dependencies(&pf.module.imports);
        if !pf.is_stdlib {
            dep_graph::add_prelude_deps(&mut deps, &module_id, &core_module_ids);
        }
        dep_graph.add_module_with_deps(module_id.clone(), deps);
        id_to_index.insert(module_id, i);
    }

    // ── Phase 3: Topological sort + cycle detection ───────────────────────────
    let topo_order = dep_graph
        .topological_order()
        .ok_or_else(|| anyhow::anyhow!("circular module dependency detected"))?;

    // ── Phase 4: Compile in dependency order ──────────────────────────────────
    let mut registry = ModuleRegistry::new();
    let mut air_modules: HashMap<usize, bock_air::AIRNode> = HashMap::new();

    for module_id in &topo_order {
        let Some(&idx) = id_to_index.get(module_id) else {
            continue; // external dependency — not in our source files
        };

        let pf = &parsed_files[idx];
        let source_file = source_map.get_file(pf.file_id);

        let mut all_diagnostics: Vec<Diagnostic> = Vec::new();

        // 4a. Name resolution (with registry for cross-file imports)
        let mut symbols = SymbolTable::new();
        register_builtins(&mut symbols);
        let resolve_diags = resolve_names_with_registry(&pf.module, &mut symbols, &registry);
        collect_diagnostics(&mut all_diagnostics, &resolve_diags);

        if has_errors(&all_diagnostics) {
            // Surface stdlib errors and user errors alike; a stdlib error here
            // is a compiler defect, a user error is the test author's bug.
            return Err(format_errors(
                &all_diagnostics,
                &pf.filename,
                &source_file.content,
            ));
        }

        // 4b. Lower to S-AIR
        let id_gen = NodeIdGen::new();
        let mut air_module = lower_module(&pf.module, &id_gen, &symbols);

        // 4c. Type check (T-AIR) — seed prelude + imports against the registry
        // so cross-module symbols (incl. core) type-check.
        let mut checker = TypeChecker::new();
        register_type_builtins(&mut checker);
        seed_prelude(&mut checker, &registry);
        seed_imports(&mut checker, &pf.module.imports, &registry);
        checker.check_module(&mut air_module);
        collect_diagnostics(&mut all_diagnostics, &checker.diags);

        if has_errors(&all_diagnostics) {
            return Err(format_errors(
                &all_diagnostics,
                &pf.filename,
                &source_file.content,
            ));
        }

        // 4d. Analysis passes (ownership, effects, capabilities). In test/dev
        // mode these are downgraded to warnings so they don't block execution.
        let ownership_diags = bock_types::analyze_ownership(&air_module);
        collect_as_warnings(&mut all_diagnostics, &ownership_diags);

        let strictness = Strictness::Development;
        let effect_diags = bock_types::track_effects(&air_module, strictness);
        collect_as_warnings(&mut all_diagnostics, &effect_diags);

        let capability_diags = bock_types::verify_capabilities(&air_module, strictness);
        collect_as_warnings(&mut all_diagnostics, &capability_diags);

        // Print analysis warnings for user modules only (don't block test
        // execution). Stdlib modules surface only errors (compiler defects);
        // their development-mode warnings describe internal code the user did
        // not author and would otherwise be noise on every `bock test`.
        if !pf.is_stdlib {
            let warnings: Vec<&Diagnostic> = all_diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Warning)
                .collect();
            if !warnings.is_empty() {
                let to_render: Vec<Diagnostic> = warnings.into_iter().cloned().collect();
                let rendered = bock_errors::render(&to_render, &pf.filename, &source_file.content);
                eprint!("{rendered}");
            }
        }

        // 4e. Register exports for downstream modules
        let exports = collect_exports(module_id, &pf.path, &checker, &air_module);
        registry.register(exports);

        air_modules.insert(idx, air_module);
    }

    // Assemble the compiled AIR modules in dependency (topological) order so
    // each test interpreter registers core modules before the test module.
    // Capture the test module's items (for `@test` discovery) as it is moved
    // into the ordered list.
    let mut air_modules_in_order: Vec<bock_air::AIRNode> = Vec::new();
    let mut test_items: Option<Vec<bock_air::AIRNode>> = None;
    for module_id in &topo_order {
        let Some(&idx) = id_to_index.get(module_id) else {
            continue;
        };
        if let Some(m) = air_modules.remove(&idx) {
            if idx == test_idx {
                let items = match &m.kind {
                    NodeKind::Module { items, .. } => items.clone(),
                    _ => return Err(anyhow::anyhow!("internal: expected Module node")),
                };
                test_items = Some(items);
            }
            air_modules_in_order.push(m);
        }
    }

    let test_items = test_items
        .ok_or_else(|| anyhow::anyhow!("internal: test module not found among compiled modules"))?;

    Ok(CompiledTestFile {
        air_modules_in_order,
        test_items,
    })
}

/// Run a single test function in a fresh interpreter environment.
///
/// Each test gets its own interpreter so tests cannot observe each other's
/// mutations. The interpreter is seeded with: the core builtins
/// (`register_core`), the interpreter-only test-assertion builtins
/// (`register_test_builtins` — `expect`/`assert`/`to_equal`/…), and every
/// compiled module (`air_modules_in_order`, core first then the test module),
/// so a test can call both bare assertion builtins and imported `core.*`
/// functions.
async fn run_single_test(
    air_modules_in_order: &[bock_air::AIRNode],
    test_name: &str,
) -> Result<(), String> {
    let mut interp = Interpreter::new();
    bock_core::register_core(&mut interp.builtins);

    // Register test assertion builtins (expect, to_equal, etc.)
    interp.builtins.register_test_builtins();

    // Register all modules in dependency order (core first, then the test
    // module), so a test's imported core functions are defined before the
    // test module's own declarations and the test body that calls them.
    for air_module in air_modules_in_order {
        register_module_in_interpreter(&mut interp, air_module).await?;
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

/// Register all top-level declarations from an AIR module in the interpreter.
///
/// Returns `Err` if a setup expression (const/let/handle) fails to evaluate.
async fn register_module_in_interpreter(
    interp: &mut Interpreter,
    air_module: &bock_air::AIRNode,
) -> Result<(), String> {
    let items = match &air_module.kind {
        NodeKind::Module { items, .. } => items.clone(),
        _ => return Ok(()),
    };

    for item in &items {
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
            NodeKind::EffectDecl {
                name, operations, ..
            } => {
                interp.register_effect(&name.name, operations);
            }
            NodeKind::LetBinding { .. } | NodeKind::ModuleHandle { .. } => {
                if let Err(e) = interp.eval_expr(item).await {
                    return Err(format!("setup error: {e}"));
                }
            }
            _ => {}
        }
    }

    Ok(())
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

/// A successfully parsed source file, ready for compilation.
struct ParsedFile {
    path: PathBuf,
    filename: String,
    file_id: bock_errors::FileId,
    module: bock_ast::Module,
    /// Whether this file is an embedded core-stdlib source (prepended by the
    /// loader) rather than the user test file. Stdlib modules compile and
    /// register exactly like the test module, but their *non-error* diagnostics
    /// (e.g. development-mode context-annotation recommendations) are not
    /// surfaced — they describe internal stdlib code the user did not write.
    is_stdlib: bool,
}

/// Lex and parse the user test file into a [`ParsedFile`].
///
/// Returns an error (rendered) if lexing or parsing produced errors.
fn parse_user_file(
    path: &Path,
    content: &str,
    source_map: &mut SourceMap,
) -> anyhow::Result<ParsedFile> {
    let filename = path.display().to_string();
    let file_id = source_map.add_file(path.to_path_buf(), content.to_string());
    let source_file = source_map.get_file(file_id);

    let mut diags: Vec<Diagnostic> = Vec::new();

    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    collect_diagnostics(&mut diags, lexer.diagnostics());

    if has_errors(&diags) {
        return Err(format_errors(&diags, &filename, &source_file.content));
    }

    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();
    collect_diagnostics(&mut diags, parser.diagnostics());

    if has_errors(&diags) {
        return Err(format_errors(&diags, &filename, &source_file.content));
    }

    Ok(ParsedFile {
        path: path.to_path_buf(),
        filename,
        file_id,
        module,
        is_stdlib: false,
    })
}

/// Lex and parse an embedded core-stdlib source into a [`ParsedFile`].
///
/// Mirrors [`parse_user_file`] but takes the source text directly (the embedded
/// stdlib is compiled into the binary). A parse error here is a
/// compiler-internal defect (the embedded sources are fixed at build time), so
/// it surfaces with the logical path for diagnosis.
fn parse_stdlib_source(
    src: &crate::stdlib::StdlibSource,
    source_map: &mut SourceMap,
) -> anyhow::Result<ParsedFile> {
    let filename = src.logical_path.display().to_string();
    let file_id = source_map.add_file(src.logical_path.clone(), src.source.clone());
    let source_file = source_map.get_file(file_id);

    let mut diags: Vec<Diagnostic> = Vec::new();

    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    collect_diagnostics(&mut diags, lexer.diagnostics());

    if has_errors(&diags) {
        return Err(format_errors(&diags, &filename, &source_file.content));
    }

    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();
    collect_diagnostics(&mut diags, parser.diagnostics());

    if has_errors(&diags) {
        return Err(format_errors(&diags, &filename, &source_file.content));
    }

    Ok(ParsedFile {
        path: src.logical_path.clone(),
        filename,
        file_id,
        module,
        is_stdlib: true,
    })
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

    // Ok, Err, Some: (T) -> Result/Optional — modeled loosely via Error.
    let constructor_fn_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Error),
        effects: vec![],
    });
    for name in ["Ok", "Err", "Some"] {
        checker.env.define(name, constructor_fn_ty.clone());
    }

    // None: Optional[T] (a value, not a function)
    checker.env.define("None", Type::Error);
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

/// Walk up from the test file's parent directory looking for a `bock.project`
/// marker. Returns the directory containing it, or `None` if no project file
/// is found before reaching the filesystem root. Mirrors `run.rs`, so the
/// `bock test` interpreter path resolves sibling modules from the same root
/// the other commands use.
fn find_project_root(entry: &Path) -> Option<PathBuf> {
    let canonical = entry.canonicalize().ok()?;
    let mut current = canonical.parent()?;
    loop {
        if current.join("bock.project").is_file() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

/// Recursively discover `.bock` files in the given directory.
fn discover_bock_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    // The entry directory is scanned as the root: an unreadable root is a
    // genuine error the caller asked for. Sub-directories encountered during
    // recursion are skipped gracefully if unreadable (see below).
    discover_bock_files_recursive(dir, &mut files, true)?;
    files.sort();
    Ok(files)
}

/// Recursive helper for file discovery.
///
/// Sibling-module discovery (Q-test-interp-crossfile-use) walks the whole
/// project subtree rooted at the resolved `bock.project` directory. That
/// subtree can legitimately contain directories this process cannot read
/// (e.g. a root-owned, `0o000` scratch dir that happens to sit under the
/// resolved root — which occurs when a stray `bock.project` marker in an
/// ancestor like `/tmp` pins the root unexpectedly wide). An unreadable
/// *sibling* directory must never abort the test run: it is unrelated to the
/// test file under compilation. We therefore skip it with a warning and
/// continue, rather than `?`-propagating the `read_dir` error (which used to
/// turn a permission error on an irrelevant scanned dir into a spurious
/// compilation failure — and a panic in the unit-test harness, which
/// `.unwrap()`s `run_tests_in_file`). `is_root` is `true` only for the
/// directory the caller explicitly handed us; for that one an unreadable
/// directory remains a hard error.
fn discover_bock_files_recursive(
    dir: &Path,
    files: &mut Vec<PathBuf>,
    is_root: bool,
) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if !is_root => {
            // An unreadable directory encountered while walking the project
            // subtree is skipped, not fatal — it is unrelated to the test
            // file being compiled. Surface a warning so the skip is visible.
            eprintln!(
                "warning: skipping unreadable directory '{}': {e}",
                dir.display()
            );
            return Ok(());
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "could not read directory '{}': {e}",
                dir.display()
            ));
        }
    };

    for entry in entries {
        // A per-entry `read_dir` iteration error (e.g. a racing unlink) on a
        // sibling subtree must likewise not abort discovery of the rest.
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) if !is_root => {
                eprintln!(
                    "warning: skipping unreadable entry under '{}': {e}",
                    dir.display()
                );
                continue;
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "could not read entry under '{}': {e}",
                    dir.display()
                ));
            }
        };
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden directories and common non-source dirs
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.starts_with('.') && name_str != "target" && name_str != "node_modules" {
                discover_bock_files_recursive(&path, files, false)?;
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
    ///
    /// A `bock.project` marker is written into the isolated tempdir so
    /// `find_project_root` stops here rather than walking up into a shared
    /// scratch dir (e.g. `/tmp`) that may hold a stray `bock.project` from
    /// another process — which would otherwise pin the project root absurdly
    /// wide and drag every unrelated `.bock` under that ancestor into the
    /// sibling scan. Bounding the root to the tempdir keeps these unit tests
    /// hermetic.
    async fn run_test_on_source(source: &str) -> Vec<TestResult> {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bock.project"), "[project]\nname = \"t\"\n").unwrap();
        let file_path = dir.path().join("test.bock");
        fs::write(&file_path, source).unwrap();
        run_tests_in_file(&file_path, &None).await.unwrap()
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
        // Bound the project root to this tempdir (see `run_test_on_source`) so
        // the sibling scan stays hermetic against a stray ancestor marker.
        fs::write(dir.path().join("bock.project"), "[project]\nname = \"t\"\n").unwrap();
        let file_path = dir.path().join("test.bock");
        fs::write(&file_path, source).unwrap();

        let compiled = compile_test_file(&file_path).unwrap();
        let filter = Some("alpha".to_string());
        let tests = discover_test_functions(&compiled.test_items, &filter);
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

        let files = discover_bock_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.extension().unwrap() == "bock"));
    }

    /// Regression (Q-bocktest-discovery-readdir-unwrap): an unreadable
    /// sub-directory encountered while scanning the project subtree must be
    /// skipped, not abort discovery. Before the fix, `read_dir` on such a dir
    /// `?`-propagated a `Permission denied` error out of discovery (and a
    /// panic in the test harness). The root dir's own `.bock` files must still
    /// be discovered around the unreadable sibling.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_discover_skips_unreadable_subdir() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.bock"), "").unwrap();

        // A root-owned-style scratch dir we cannot read (0o000).
        let locked = dir.path().join("snap-private");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("hidden.bock"), "").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        // Discovery must not error: the unreadable subtree is skipped, the
        // readable `.bock` at the root is still found.
        let result = discover_bock_files(dir.path());

        // Restore perms before any assertion can unwind, so tempdir cleanup
        // (which must descend into the dir) succeeds either way.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        let files = result.expect("unreadable subdir must not abort discovery");
        assert_eq!(
            files.len(),
            1,
            "only the readable root .bock should be found: {files:?}"
        );
        assert!(files[0].ends_with("a.bock"));
    }

    /// Regression (Q-bocktest-discovery-readdir-unwrap): end-to-end, an
    /// unreadable sibling directory under the resolved project root must not
    /// abort a `bock test` run. This is the exact shape of the original repro:
    /// a `bock.project` marker pins the root, the sibling scan descends into a
    /// `0o000` dir, and the test file's own tests must still run + pass.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_tests_in_file_skips_unreadable_sibling() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        // A `bock.project` marker pins this dir as the project root, so the
        // sibling scan walks the whole subtree below it.
        fs::write(
            root.path().join("bock.project"),
            "[project]\nname = \"t\"\n",
        )
        .unwrap();

        let test_file = root.path().join("sample_test.bock");
        fs::write(
            &test_file,
            "@test\nfn test_ok() {\n    expect(1 + 1).to_equal(2)\n}\n",
        )
        .unwrap();

        // Unreadable sibling directory directly under the project root.
        let locked = root.path().join("snap-private");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        // Must NOT panic / abort: returns Ok and the real test runs + passes.
        let result = run_tests_in_file(&test_file, &None).await;

        // Restore perms so tempdir teardown can descend and clean up.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        let results = result.expect("unreadable sibling dir must not abort the test run");
        assert_eq!(results.len(), 1, "the sample test should have run");
        assert!(
            results[0].passed,
            "sample test should pass: {:?}",
            results[0].error
        );
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

    #[test]
    fn test_document_carries_envelope_and_per_test_entries() {
        let results = vec![
            TestResult {
                name: "t::ok".into(),
                file: "t.bock".into(),
                passed: true,
                error: None,
                duration: std::time::Duration::from_millis(2),
            },
            TestResult {
                name: "t::bad".into(),
                file: "t.bock".into(),
                passed: false,
                error: Some("assertion failed".into()),
                duration: std::time::Duration::ZERO,
            },
        ];
        let doc = test_document(&results);
        assert_eq!(doc["format_version"], FORMAT_VERSION);
        assert_eq!(doc["command"], "test");
        assert_eq!(doc["outcome"], "failed");
        assert_eq!(doc["summary"]["tests"], 2);
        assert_eq!(doc["summary"]["passed"], 1);
        assert_eq!(doc["summary"]["failed"], 1);
        let tests = doc["tests"].as_array().unwrap();
        assert_eq!(tests[0]["name"], "t::ok");
        assert_eq!(tests[0]["file"], "t.bock");
        assert_eq!(tests[0]["passed"], true);
        assert!(tests[0]["message"].is_null(), "passing test → null message");
        assert!(tests[0]["duration_ms"].is_f64() || tests[0]["duration_ms"].is_u64());
        assert_eq!(tests[1]["passed"], false);
        assert_eq!(tests[1]["message"], "assertion failed");

        // No tests at all is a clean outcome (matching the exit contract).
        let empty = test_document(&[]);
        assert_eq!(empty["outcome"], "clean");
        assert_eq!(empty["summary"]["tests"], 0);
    }

    #[tokio::test]
    async fn test_use_core_option_resolves_and_runs() {
        // A test file that `use`s a core.* module must compile (the import
        // resolves through the same multi-file pipeline as check/run) and run
        // the imported function in the per-test interpreter. `count(Some(5))`
        // is `1`; `count(None)` is `0`.
        let source = r#"module mytest

use core.option.{count}

@test
fn test_core_option_count() {
    expect(count(Some(5))).to_equal(1)
    expect(count(None)).to_equal(0)
}
"#;
        let results = run_test_on_source(source).await;
        assert_eq!(results.len(), 1);
        assert!(
            results[0].passed,
            "test using core.option should pass: {:?}",
            results[0].error
        );
    }
}
