//! Implementation of the `bock run` command.
//!
//! Multi-file pipeline:
//! 1. Discover the entry file + all project `.bock` files
//! 2. Lex + parse all files
//! 3. Build dependency graph → topological sort
//! 4. Compile in dependency order (with [`ModuleRegistry`] for cross-file imports)
//! 5. Interpret the entry module's `main` function

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;

use bock_air::{
    lower_module, resolve_names_with_registry, Binding, ModuleRegistry, NameKind, NodeIdGen,
    NodeKind, ResolvedName, SymbolTable,
};
use bock_ast::Visibility;
use bock_build::dep_graph::{self, DepGraph};
use bock_errors::{Diagnostic, DiagnosticBag, FileId, Severity, Span};
use bock_interp::{BockString, Interpreter, RuntimeError, Value};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{collect_exports, seed_imports, FnType, PrimitiveType, Strictness, Type, TypeChecker};

/// Run the `bock run` command.
///
/// Uses the multi-file pipeline: discover all project files, compile in
/// dependency order with [`ModuleRegistry`], then interpret the main module.
///
/// If `file` is `None`, looks for `main.bock` or `src/main.bock` in the current directory.
/// `program_args` are arguments passed after `--` for the program to consume.
pub async fn run(file: Option<String>, program_args: Vec<String>) -> anyhow::Result<()> {
    let entry_was_explicit = file.is_some();
    let entry_path = resolve_entry_file(file)?;

    // Pick a scan root for sibling-module discovery. Walking up from the
    // entry, an `bock.project` marker pins the project root; otherwise we
    // only sweep the CWD when the entry was discovered there (default
    // `main.bock` lookup). When the user passed an explicit entry path
    // outside any project, we compile only that file — scanning the CWD or
    // a system temp directory would slurp unrelated `.bock` files (e.g.
    // workspace test fixtures with not-yet-supported syntax).
    let scan_root = find_project_root(&entry_path).or_else(|| {
        if entry_was_explicit {
            None
        } else {
            Some(PathBuf::from("."))
        }
    });

    let mut files = match scan_root {
        Some(root) => discover_bock_files(&root)?,
        None => Vec::new(),
    };

    let entry_canonical = entry_path
        .canonicalize()
        .unwrap_or_else(|_| entry_path.clone());
    let entry_in_list = files.iter().any(|f| {
        f.canonicalize()
            .unwrap_or_else(|_| f.clone())
            == entry_canonical
    });
    if !entry_in_list {
        files.push(entry_path.clone());
    }

    run_project(&files, &entry_path, &program_args).await
}

/// Walk up from the entry file's parent directory looking for an
/// `bock.project` marker. Returns the directory containing it, or `None`
/// if no project file is found before reaching the filesystem root.
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

/// Resolve the entry file path, applying default discovery when no path is given.
fn resolve_entry_file(file: Option<String>) -> anyhow::Result<PathBuf> {
    if let Some(f) = file {
        let p = PathBuf::from(&f);
        if !p.exists() {
            eprintln!("error: file not found: {f}");
            process::exit(1);
        }
        return Ok(p);
    }

    // Default discovery: main.bock, then src/main.bock
    let candidates = ["main.bock", "src/main.bock"];
    for candidate in &candidates {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Ok(p);
        }
    }

    eprintln!(
        "error: no entry file found. Expected main.bock or src/main.bock, or pass a file path."
    );
    process::exit(1);
}

/// A successfully parsed source file.
struct ParsedFile {
    path: PathBuf,
    filename: String,
    file_id: bock_errors::FileId,
    module: bock_ast::Module,
}

/// Lex and parse a single file, adding it to the shared [`SourceMap`].
fn parse_file(path: &Path, source_map: &mut SourceMap) -> Result<ParsedFile, ()> {
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

    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    collect_diagnostics(&mut diags, lexer.diagnostics());

    if has_errors(&diags) {
        print_diagnostics(&diags, &filename, &source_file.content);
        return Err(());
    }

    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();
    collect_diagnostics(&mut diags, parser.diagnostics());

    if has_errors(&diags) {
        print_diagnostics(&diags, &filename, &source_file.content);
        return Err(());
    }

    Ok(ParsedFile {
        path: path.to_path_buf(),
        filename,
        file_id,
        module,
    })
}

/// Compile all project files in dependency order, then interpret the entry module.
async fn run_project(
    files: &[PathBuf],
    entry_path: &Path,
    program_args: &[String],
) -> anyhow::Result<()> {
    // ── Phase 1: Parse all files ──────────────────────────────────────────────
    let mut source_map = SourceMap::new();
    let mut parsed_files: Vec<ParsedFile> = Vec::new();
    let mut found_errors = false;

    for file_path in files {
        match parse_file(file_path, &mut source_map) {
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
    // Collect AIR modules for all files; the entry module is interpreted.
    let mut air_modules: HashMap<usize, bock_air::AIRNode> = HashMap::new();

    for module_id in &topo_order {
        let Some(&idx) = id_to_index.get(module_id) else {
            continue;
        };

        let pf = &parsed_files[idx];
        let source_file = source_map.get_file(pf.file_id);

        let mut all_diagnostics: Vec<Diagnostic> = Vec::new();

        // 4a. Name resolution with registry
        let mut symbols = SymbolTable::new();
        register_builtins(&mut symbols);
        let resolve_diags =
            resolve_names_with_registry(&pf.module, &mut symbols, &registry);
        collect_diagnostics(&mut all_diagnostics, &resolve_diags);

        if has_errors(&all_diagnostics) {
            print_diagnostics(&all_diagnostics, &pf.filename, &source_file.content);
            process::exit(1);
        }

        // 4b. Lower to S-AIR
        let id_gen = NodeIdGen::new();
        let mut air_module = lower_module(&pf.module, &id_gen, &symbols);

        // 4c. Type check (T-AIR)
        let mut checker = TypeChecker::new();
        register_type_builtins(&mut checker);
        seed_imports(&mut checker, &pf.module.imports, &registry);
        checker.check_module(&mut air_module);
        collect_diagnostics(&mut all_diagnostics, &checker.diags);

        if has_errors(&all_diagnostics) {
            print_diagnostics(&all_diagnostics, &pf.filename, &source_file.content);
            process::exit(1);
        }

        // 4d. Analysis passes — downgrade to warnings for run mode
        let ownership_diags = bock_types::analyze_ownership(&air_module);
        collect_as_warnings(&mut all_diagnostics, &ownership_diags);

        let strictness = Strictness::Development;
        let effect_diags = bock_types::track_effects(&air_module, strictness);
        collect_as_warnings(&mut all_diagnostics, &effect_diags);

        let capability_diags = bock_types::verify_capabilities(&air_module, strictness);
        collect_as_warnings(&mut all_diagnostics, &capability_diags);

        // Print warnings
        let warnings: Vec<&Diagnostic> = all_diagnostics
            .iter()
            .filter(|d| d.severity != Severity::Error)
            .collect();
        if !warnings.is_empty() {
            let to_render: Vec<Diagnostic> = warnings.into_iter().cloned().collect();
            print_diagnostics(&to_render, &pf.filename, &source_file.content);
        }

        // 4e. Register exports
        let exports = collect_exports(module_id, &pf.path, &checker, &air_module);
        registry.register(exports);

        air_modules.insert(idx, air_module);
    }

    // ── Phase 5: Interpret the entry module ───────────────────────────────────
    let entry_canonical = entry_path
        .canonicalize()
        .unwrap_or_else(|_| entry_path.to_path_buf());
    let entry_idx = parsed_files
        .iter()
        .position(|pf| {
            pf.path
                .canonicalize()
                .unwrap_or_else(|_| pf.path.clone())
                == entry_canonical
        })
        .unwrap_or_else(|| {
            eprintln!(
                "error: internal: entry file not found in parsed files"
            );
            process::exit(1);
        });

    let entry_filename = &parsed_files[entry_idx].filename;
    let entry_air = air_modules.remove(&entry_idx).unwrap_or_else(|| {
        eprintln!("error: internal: entry module not compiled");
        process::exit(1);
    });

    let mut interp = Interpreter::new();
    bock_core::register_core(&mut interp.builtins);

    // Register all non-entry modules in the interpreter first
    for (idx, air_module) in &air_modules {
        register_module_in_interpreter(&mut interp, air_module, &parsed_files[*idx].filename).await;
    }

    // Register entry module declarations
    register_module_in_interpreter(&mut interp, &entry_air, entry_filename).await;

    // Look for `main` function and call it
    let main_val = match interp.env.get("main") {
        Some(v) => v.clone(),
        None => {
            eprintln!("error: no main function found in {entry_filename}");
            process::exit(1);
        }
    };

    // Build the args list: List[String]
    let args_value = Value::List(
        program_args
            .iter()
            .map(|s| Value::String(BockString::new(s)))
            .collect(),
    );

    // Try calling main with args first; if main doesn't accept a parameter,
    // fall back to calling with no args.
    let result = if program_args.is_empty() {
        interp.call_fn_value(&main_val, vec![]).await
    } else {
        match interp.call_fn_value(&main_val, vec![args_value]).await {
            Err(RuntimeError::ArityMismatch { expected: 0, .. }) => {
                interp.call_fn_value(&main_val, vec![]).await
            }
            other => other,
        }
    };

    // If `main` returned a Value::Future (async fn main), await it now so the
    // program completes before the runtime tears down.
    let result = match result {
        Ok(Value::Future(handle)) => {
            let h = handle.lock().unwrap().take();
            match h {
                Some(jh) => match jh.await {
                    Ok(inner) => inner,
                    Err(e) => Err(RuntimeError::TypeError(format!(
                        "async main panicked: {e}"
                    ))),
                },
                None => Ok(Value::Void),
            }
        }
        other => other,
    };

    match result {
        Ok(_) => {}
        Err(e) => {
            eprintln!("runtime error: {e}");
            process::exit(1);
        }
    }

    Ok(())
}

/// Register all top-level declarations from an AIR module in the interpreter.
async fn register_module_in_interpreter(
    interp: &mut Interpreter,
    air_module: &bock_air::AIRNode,
    _filename: &str,
) {
    let items = match &air_module.kind {
        NodeKind::Module { items, .. } => items.clone(),
        _ => return,
    };

    for item in &items {
        match &item.kind {
            NodeKind::FnDecl {
                name, params, body, is_async, ..
            } => {
                let param_names: Vec<String> =
                    params.iter().filter_map(extract_param_name).collect();
                interp.register_fn_with_async(
                    &name.name,
                    param_names,
                    *body.clone(),
                    *is_async,
                );
            }
            NodeKind::EnumDecl {
                name, variants, ..
            } => {
                interp.register_enum(&name.name, variants);
            }
            NodeKind::ConstDecl { name, value, .. } => {
                match interp.eval_expr(value).await {
                    Ok(val) => interp.env.define(&name.name, val),
                    Err(e) => {
                        eprintln!("runtime error: {e}");
                        process::exit(1);
                    }
                }
            }
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
                    eprintln!("runtime error: {e}");
                    process::exit(1);
                }
            }
            _ => {}
        }
    }
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

/// Print diagnostics with source context.
fn print_diagnostics(diagnostics: &[Diagnostic], filename: &str, source: &str) {
    let rendered = bock_errors::render(diagnostics, filename, source);
    eprint!("{rendered}");
}

/// Register builtin function types in the type checker so it does not
/// report them as undefined variables.
fn register_type_builtins(checker: &mut TypeChecker) {
    // print, println, debug: (Any) -> Void
    // We model them as Fn(String) -> Void for simplicity; the interpreter
    // is flexible about the argument type.
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
    // dispatch via qualified globals (e.g. "Duration.seconds",
    // "Channel.new") handles correctness.
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

    // spawn: (Future[T]) -> Future[T]. Modeled loosely with Error so that
    // any async fn call result can be passed.
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

    // Ok, Err: (T) -> Result[T, E] — modeled as generic-like via Error
    let constructor_fn_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Error),
        effects: vec![],
    });
    for name in ["Ok", "Err"] {
        checker.env.define(name, constructor_fn_ty.clone());
    }

    // Some: (T) -> Optional[T]
    checker.env.define("Some", constructor_fn_ty);

    // None: Optional[T] (a value, not a function)
    checker.env.define("None", Type::Error);
}

/// Discover `.bock` files recursively from the given directory.
fn discover_bock_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    discover_bock_files_recursive(dir, &mut files)?;
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

/// Register interpreter builtin globals in the symbol table so name resolution
/// does not report them as undefined.
fn register_builtins(symbols: &mut SymbolTable) {
    let builtin_span = Span {
        file: FileId(0),
        start: 0,
        end: 0,
    };
    // Use high def_ids that won't collide with real AST node ids.
    let builtins = [
        ("print", u32::MAX - 1),
        ("println", u32::MAX - 2),
        ("debug", u32::MAX - 3),
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
                used: true, // Mark as used to suppress unused warnings
                is_import: false,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn test_run_project_no_args() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("main.bock");
        fs::write(
            &file_path,
            r#"fn main() {
    println("hello")
}
"#,
        )
        .unwrap();

        let result = run_project(&[file_path.clone()], &file_path, &[]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_program_args_to_value() {
        // Verify that program args are correctly converted to a List[String] value
        let args = vec!["arg1".to_string(), "arg2".to_string(), "arg3".to_string()];
        let args_value = Value::List(
            args.iter()
                .map(|s| Value::String(BockString::new(s)))
                .collect(),
        );

        match &args_value {
            Value::List(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Value::String(BockString::new("arg1")));
                assert_eq!(items[1], Value::String(BockString::new("arg2")));
                assert_eq!(items[2], Value::String(BockString::new("arg3")));
            }
            _ => panic!("expected List value"),
        }
    }

    #[tokio::test]
    async fn test_program_args_empty_means_no_call_args() {
        let program_args: Vec<String> = vec![];
        let args_value = Value::List(
            program_args
                .iter()
                .map(|s| Value::String(BockString::new(s)))
                .collect(),
        );
        let call_args = if program_args.is_empty() {
            vec![]
        } else {
            vec![args_value]
        };
        assert!(call_args.is_empty());
    }

    #[tokio::test]
    async fn test_program_args_nonempty_passes_list() {
        let program_args = vec!["hello".to_string()];
        let args_value = Value::List(
            program_args
                .iter()
                .map(|s| Value::String(BockString::new(s)))
                .collect(),
        );
        let call_args = if program_args.is_empty() {
            vec![]
        } else {
            vec![args_value]
        };
        assert_eq!(call_args.len(), 1);
        match &call_args[0] {
            Value::List(items) => assert_eq!(items.len(), 1),
            _ => panic!("expected List"),
        }
    }

    #[tokio::test]
    async fn test_resolve_entry_file_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.bock");
        fs::write(&file_path, "fn main() {}").unwrap();

        let resolved = resolve_entry_file(Some(file_path.to_string_lossy().to_string()));
        assert!(resolved.is_ok());
        assert_eq!(resolved.unwrap(), file_path);
    }

    #[test]
    fn find_project_root_walks_up_to_bock_project() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        fs::create_dir(&proj).unwrap();
        fs::write(proj.join("bock.project"), "[project]\nname = \"p\"\n").unwrap();
        fs::create_dir(proj.join("src")).unwrap();
        let entry = proj.join("src/main.bock");
        fs::write(&entry, "fn main() {}").unwrap();

        let root = find_project_root(&entry).expect("should find project root");
        assert_eq!(
            root.canonicalize().unwrap(),
            proj.canonicalize().unwrap(),
            "project root should be the directory containing bock.project",
        );
    }

    #[test]
    fn find_project_root_returns_none_when_no_marker() {
        // A file in a fresh tempdir with no ancestor bock.project.
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("lonely.bock");
        fs::write(&entry, "fn main() {}").unwrap();

        // Walk from the tempdir up to /: none of those directories should
        // contain an bock.project. We can't assert None unconditionally
        // because the test harness itself might run inside a project, so
        // verify instead that any returned root is an ancestor of the temp
        // file — i.e. the walk either stops at a real marker or fails.
        match find_project_root(&entry) {
            None => {}
            Some(root) => {
                let canonical_entry = entry.canonicalize().unwrap();
                let canonical_root = root.canonicalize().unwrap();
                assert!(
                    canonical_entry.starts_with(&canonical_root),
                    "returned root must be an ancestor of the entry",
                );
                assert!(
                    canonical_root.join("bock.project").is_file(),
                    "returned root must actually contain bock.project",
                );
            }
        }
    }

    #[test]
    fn find_project_root_prefers_nearest_marker() {
        // Two nested bock.project files; the nearer one wins.
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer");
        fs::create_dir(&outer).unwrap();
        fs::write(outer.join("bock.project"), "[project]\nname = \"outer\"\n").unwrap();

        let inner = outer.join("inner");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join("bock.project"), "[project]\nname = \"inner\"\n").unwrap();

        fs::create_dir(inner.join("src")).unwrap();
        let entry = inner.join("src/main.bock");
        fs::write(&entry, "fn main() {}").unwrap();

        let root = find_project_root(&entry).expect("should find project root");
        assert_eq!(
            root.canonicalize().unwrap(),
            inner.canonicalize().unwrap(),
            "nearest bock.project should win",
        );
    }

    // ── Multi-file integration tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_multifile_cross_module_function_call() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();

        fs::write(
            src.join("helpers.bock"),
            "module helpers\n\npublic fn double(x: Int) -> Int {\n    x * 2\n}\n",
        )
        .unwrap();

        let entry = src.join("main.bock");
        fs::write(
            &entry,
            "module main\n\nuse helpers.{double}\n\nfn main() {\n    let result = double(21)\n    println(result.to_string())\n}\n",
        )
        .unwrap();

        let files = vec![src.join("helpers.bock"), entry.clone()];
        let result = run_project(&files, &entry, &[]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_multifile_cross_module_record_construct() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();

        fs::write(
            src.join("models.bock"),
            r#"module models

public record Point {
    x: Int
    y: Int
}

public fn point_to_string(p: Point) -> String {
    "(" + p.x.to_string() + ", " + p.y.to_string() + ")"
}
"#,
        )
        .unwrap();

        let entry = src.join("main.bock");
        fs::write(
            &entry,
            r#"module main

use models.{point_to_string}

fn main() {
    let p = Point { x: 5, y: 10 }
    println(point_to_string(p))
}
"#,
        )
        .unwrap();

        let files = vec![src.join("models.bock"), entry.clone()];
        let result = run_project(&files, &entry, &[]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_multifile_cross_module_effect_handler() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();

        fs::write(
            src.join("effects.bock"),
            r#"module effects

public effect Logger {
    fn log(message: String)
}

public fn console_logger(message: String) {
    println("[LOG] " + message)
}
"#,
        )
        .unwrap();

        let entry = src.join("main.bock");
        fs::write(
            &entry,
            r#"module main

use effects.{Logger, console_logger}

fn greet(name: String) with Logger {
    log("hello " + name)
}

fn main() {
    handling (Logger with console_logger) {
        greet("World")
    }
}
"#,
        )
        .unwrap();

        let files = vec![src.join("effects.bock"), entry.clone()];
        let result = run_project(&files, &entry, &[]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_multifile_three_modules() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();

        fs::write(
            src.join("models.bock"),
            r#"module models

public record Item {
    name: String
    price: Int
}

public fn item_total(item: Item, qty: Int) -> Int {
    item.price * qty
}
"#,
        )
        .unwrap();

        fs::write(
            src.join("format.bock"),
            r#"module format

public fn format_price(cents: Int) -> String {
    cents.to_string() + " cents"
}
"#,
        )
        .unwrap();

        let entry = src.join("main.bock");
        fs::write(
            &entry,
            r#"module main

use models.{item_total}
use format.{format_price}

fn main() {
    let item = Item { name: "Widget", price: 500 }
    let total = item_total(item, 3)
    println(format_price(total))
}
"#,
        )
        .unwrap();

        let files = vec![
            src.join("models.bock"),
            src.join("format.bock"),
            entry.clone(),
        ];
        let result = run_project(&files, &entry, &[]).await;
        assert!(result.is_ok());
    }
}
