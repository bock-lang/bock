//! Implementation of the `bock check` command.
//!
//! Runs the full multi-file pipeline:
//! 1. Discover all `.bock` files
//! 2. Lex + parse every file
//! 3. Build a dependency graph from import declarations
//! 4. Topological sort (cycle detection → clear error)
//! 5. For each module in dependency order:
//!    a. Resolve names (with [`bock_air::registry::ModuleRegistry`] for cross-file imports)
//!    b. Lower to S-AIR
//!    c. Type-check (T-AIR)
//!    d. Run analysis passes (ownership, effects, capabilities)
//!    e. Collect exports → register in [`bock_air::registry::ModuleRegistry`]
//! 6. Report accumulated diagnostics

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;

use bock_air::{lower_module, resolve_names_with_registry, ModuleRegistry, NodeIdGen, SymbolTable};
use bock_build::dep_graph::{self, DepGraph};
use bock_errors::{Diagnostic, DiagnosticBag, Severity};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{
    collect_exports, seed_imports, FnType, PrimitiveType, Strictness, Type, TypeChecker,
};

/// Options controlling which checks to run.
pub struct CheckOptions {
    /// Run type checking (default: true).
    pub types: bool,
    /// Run lint checks (default: true).
    pub lint: bool,
    /// Show source context in diagnostics (default: true).
    pub context: bool,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            types: true,
            lint: true,
            context: true,
        }
    }
}

/// Run the check command on the given file paths with the specified options.
///
/// Uses the multi-file pipeline: parse all → dependency sort → compile in order
/// with cross-file name resolution via [`bock_air::registry::ModuleRegistry`].
pub fn run(files: Vec<PathBuf>, options: &CheckOptions) -> anyhow::Result<()> {
    let files = if files.is_empty() {
        discover_bock_files(".")?
    } else {
        files
    };

    if files.is_empty() {
        eprintln!("No .bock files found.");
        process::exit(1);
    }

    let mut found_errors = false;

    // ── Phase 1: Parse all files ──────────────────────────────────────────────
    let mut source_map = SourceMap::new();
    let mut parsed_files: Vec<ParsedFile> = Vec::new();

    for file_path in &files {
        match parse_file(file_path, &mut source_map, options.context) {
            Ok(pf) => parsed_files.push(pf),
            Err(()) => found_errors = true,
        }
    }

    if found_errors {
        process::exit(1);
    }

    // ── Phase 2: Build dependency graph ───────────────────────────────────────
    let mut dep_graph = DepGraph::new();
    let mut id_to_index: HashMap<String, usize> = HashMap::new();

    for (i, pf) in parsed_files.iter().enumerate() {
        let module_id = dep_graph::module_id_from_module(&pf.module, i);
        let deps = dep_graph::extract_dependencies(&pf.module.imports);
        dep_graph.add_module_with_deps(module_id.clone(), deps);
        id_to_index.insert(module_id, i);
    }

    // ── Phase 3: Topological sort + cycle detection ───────────────────────────
    let topo_order = match dep_graph.topological_order() {
        Some(order) => order,
        None => {
            eprintln!("error: circular module dependency detected");
            process::exit(1);
        }
    };

    // ── Phase 4: Compile in dependency order ──────────────────────────────────
    let mut registry = ModuleRegistry::new();

    for module_id in &topo_order {
        let Some(&idx) = id_to_index.get(module_id) else {
            continue; // external dependency — not in our source files
        };

        let pf = &parsed_files[idx];
        let source_file = source_map.get_file(pf.file_id);

        let mut all_diagnostics: Vec<Diagnostic> = Vec::new();

        // 4a. Name resolution (with registry for cross-file imports)
        let mut symbols = SymbolTable::new();
        let resolve_diags = resolve_names_with_registry(&pf.module, &mut symbols, &registry);
        collect_diagnostics(&mut all_diagnostics, &resolve_diags);

        if has_errors(&all_diagnostics) {
            print_diagnostics(
                &all_diagnostics,
                &pf.filename,
                &source_file.content,
                options.context,
            );
            found_errors = true;
            continue;
        }

        // 4b. Lower to S-AIR
        let id_gen = NodeIdGen::new();
        let mut air_module = lower_module(&pf.module, &id_gen, &symbols);

        // 4c. Type check (T-AIR) — optional via --types flag
        let mut checker = TypeChecker::new();
        register_type_builtins(&mut checker);
        seed_imports(&mut checker, &pf.module.imports, &registry);
        if options.types {
            checker.check_module(&mut air_module);
            collect_diagnostics(&mut all_diagnostics, &checker.diags);
        }

        // 4d. Analysis passes (ownership, effects, capabilities)
        let ownership_diags = bock_types::analyze_ownership(&air_module);
        collect_diagnostics(&mut all_diagnostics, &ownership_diags);

        let strictness = Strictness::Development;
        let effect_diags = bock_types::track_effects(&air_module, strictness);
        collect_diagnostics(&mut all_diagnostics, &effect_diags);

        let capability_diags = bock_types::verify_capabilities(&air_module, strictness);
        collect_diagnostics(&mut all_diagnostics, &capability_diags);

        // Report diagnostics for this module
        let module_has_errors = has_errors(&all_diagnostics);

        let diagnostics_to_show: Vec<&Diagnostic> = if options.lint {
            all_diagnostics.iter().collect()
        } else {
            all_diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .collect()
        };

        if !diagnostics_to_show.is_empty() {
            let to_render: Vec<Diagnostic> = diagnostics_to_show.into_iter().cloned().collect();
            print_diagnostics(
                &to_render,
                &pf.filename,
                &source_file.content,
                options.context,
            );
        }

        if module_has_errors {
            found_errors = true;
        } else {
            // 4e. Register exports for downstream modules
            let exports = collect_exports(module_id, &pf.path, &checker, &air_module);
            registry.register(exports);
        }
    }

    if found_errors {
        process::exit(1);
    }

    let file_count = files.len();
    let label = if file_count == 1 { "file" } else { "files" };
    println!("check: {file_count} {label} checked, no errors.");
    Ok(())
}

/// A successfully parsed source file, ready for compilation.
struct ParsedFile {
    path: PathBuf,
    filename: String,
    file_id: bock_errors::FileId,
    module: bock_ast::Module,
}

/// Lex and parse a single file, adding it to the shared [`SourceMap`].
///
/// Returns `Err(())` if lexing or parsing produced errors (already printed).
fn parse_file(
    path: &Path,
    source_map: &mut SourceMap,
    show_context: bool,
) -> Result<ParsedFile, ()> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {}: {e}", path.display());
            return Err(());
        }
    };

    let filename = path.display().to_string();
    let file_id = source_map.add_file(path.to_path_buf(), content);
    let source_file = source_map.get_file(file_id);

    let mut diags: Vec<Diagnostic> = Vec::new();

    // Lex
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    collect_diagnostics(&mut diags, lexer.diagnostics());

    if has_errors(&diags) {
        print_diagnostics(&diags, &filename, &source_file.content, show_context);
        return Err(());
    }

    // Parse
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();
    collect_diagnostics(&mut diags, parser.diagnostics());

    if has_errors(&diags) {
        print_diagnostics(&diags, &filename, &source_file.content, show_context);
        return Err(());
    }

    Ok(ParsedFile {
        path: path.to_path_buf(),
        filename,
        file_id,
        module,
    })
}

/// Collect diagnostics from a bag into the accumulator.
fn collect_diagnostics(acc: &mut Vec<Diagnostic>, bag: &DiagnosticBag) {
    for diag in bag.iter() {
        acc.push(diag.clone());
    }
}

/// Check if any diagnostic in the list is an error.
fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

/// Print diagnostics, optionally with source context.
fn print_diagnostics(diagnostics: &[Diagnostic], filename: &str, source: &str, context: bool) {
    if context {
        let rendered = bock_errors::render(diagnostics, filename, source);
        eprint!("{rendered}");
    } else {
        // Simple one-line-per-diagnostic format without source context
        for diag in diagnostics {
            let severity = match diag.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "info",
                Severity::Hint => "hint",
            };
            eprintln!(
                "{severity}[{}]: {} (at {}:{}..{})",
                diag.code, diag.message, filename, diag.span.start, diag.span.end
            );
        }
    }
}

/// Discover `.bock` files recursively from the given directory.
pub(crate) fn discover_bock_files(dir: &str) -> anyhow::Result<Vec<PathBuf>> {
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
            if !name_str.starts_with('.')
                && name_str != "build"
                && name_str != "target"
                && name_str != "node_modules"
            {
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

/// Register prelude builtin functions in the type checker environment so
/// they don't produce "undefined variable" errors.
pub(crate) fn register_type_builtins(checker: &mut TypeChecker) {
    // print, println, debug: (String) -> Void
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

    // assert: (Bool) -> Void
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

    // Ok, Err: (T) -> Result[T, E] — modeled as generic-like via Error
    // Since the checker uses structural typing, we use Error as a
    // "wildcard" that unifies with anything.
    let constructor_fn_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Error),
        effects: vec![],
    });
    for name in ["Ok", "Err"] {
        checker.env.define(name, constructor_fn_ty.clone());
    }

    // Some: (T) -> Optional[T]
    checker.env.define("Some", constructor_fn_ty.clone());

    // None: Optional[T] (a value, not a function)
    checker.env.define("None", Type::Error);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_discover_bock_files_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        let nested = sub.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.path().join("a.bock"), "").unwrap();
        fs::write(sub.join("b.bock"), "").unwrap();
        fs::write(nested.join("c.bock"), "").unwrap();
        fs::write(dir.path().join("d.txt"), "").unwrap();

        let files = discover_bock_files(&dir.path().to_string_lossy()).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.iter().all(|f| f.extension().unwrap() == "bock"));
    }

    #[test]
    fn test_discover_skips_hidden_and_build_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = dir.path().join(".hidden");
        let build_dir = dir.path().join("build");
        let target = dir.path().join("target");
        fs::create_dir_all(&hidden).unwrap();
        fs::create_dir_all(&build_dir).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(hidden.join("x.bock"), "").unwrap();
        fs::write(build_dir.join("y.bock"), "").unwrap();
        fs::write(target.join("z.bock"), "").unwrap();
        fs::write(dir.path().join("main.bock"), "").unwrap();

        let files = discover_bock_files(&dir.path().to_string_lossy()).unwrap();
        assert_eq!(files.len(), 1);
    }
}
