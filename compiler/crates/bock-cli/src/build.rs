//! Implementation of the `bock build` command.
//!
//! Full multi-file pipeline:
//! 1. Discover sources
//! 2. Lex + parse all files
//! 3. Build dependency graph → topological sort
//! 4. Compile in dependency order (with [`bock_air::registry::ModuleRegistry`] for cross-file imports)
//! 5. Code-generate → (optionally) invoke target compiler

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use bock_air::{
    lower_module, resolve_names_with_registry, Binding, ModuleRegistry, NameKind, NodeIdGen,
    ResolvedName, SymbolTable,
};
use bock_ast::Visibility;
use bock_build::dep_graph::{self, DepGraph};
use bock_build::toolchain::ToolchainRegistry;
use bock_codegen::{
    CodeGenerator, GoGenerator, JsGenerator, PyGenerator, RsGenerator, SourceInfo, TsGenerator,
};
use bock_errors::{Diagnostic, DiagnosticBag, FileId, Severity, Span};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{collect_exports, seed_imports, Strictness, TypeChecker};

/// Known target identifiers.
const KNOWN_TARGETS: &[&str] = &["js", "ts", "python", "rust", "go"];

/// Options for the build command.
pub struct BuildOptions {
    /// Target language(s) to build for.
    pub targets: Vec<String>,
    /// Enable release optimizations (accepted, minimal behavior for v1).
    pub release: bool,
    /// Emit generated code without invoking the target compiler.
    pub source_only: bool,
    /// Use only rule-based codegen (skip AI-assisted generation).
    /// Currently all codegen is rule-based, so this is a no-op for v1.
    #[allow(dead_code)]
    pub deterministic: bool,
    /// Force production strictness for this build regardless of the
    /// project's configured default. Implies the pre-build unpinned-
    /// decision gate (§17.6).
    pub strict: bool,
    /// After a successful build, pin every unpinned build-scope
    /// decision in `.bock/decisions/build/`.
    pub pin_all: bool,
    /// Emit source map sidecar files alongside generated code.
    pub source_map: bool,
}

/// Run the `bock build` command.
///
/// Uses the multi-file pipeline: parse all → dependency sort → compile in order
/// with cross-file name resolution via [`bock_air::registry::ModuleRegistry`], then codegen.
pub fn run(options: &BuildOptions) -> anyhow::Result<()> {
    let total_start = Instant::now();

    // Discover source files
    let files = discover_bock_files_recursive(".")?;
    if files.is_empty() {
        eprintln!("error: no .bock files found");
        process::exit(1);
    }

    println!(
        "build: compiling {} source file{}",
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );

    let strictness = if options.strict || options.release {
        Strictness::Production
    } else {
        Strictness::Development
    };

    // §17.6 governance gate: in production strictness, fail early on
    // any unpinned build-scope decision. This runs before we touch the
    // frontend so the user sees the actionable error without waiting
    // for a full compile.
    if matches!(strictness, Strictness::Production) {
        let project_root = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("could not read current directory: {e}"))?;
        run_production_gate(&project_root)?;
    }

    // ── Phase 1: Parse all files ──────────────────────────────────────────────
    let frontend_start = Instant::now();
    let mut source_map = SourceMap::new();
    let mut parsed_files: Vec<ParsedFile> = Vec::new();
    let mut found_errors = false;

    for file_path in &files {
        match parse_file(file_path, &mut source_map) {
            Ok(pf) => parsed_files.push(pf),
            Err(()) => found_errors = true,
        }
    }

    if found_errors {
        eprintln!("build: aborting due to parse errors");
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
    let mut air_modules = Vec::new();
    let mut source_infos = Vec::new();
    let mut module_source_paths: Vec<PathBuf> = Vec::new();

    for module_id in &topo_order {
        let Some(&idx) = id_to_index.get(module_id) else {
            continue; // external dependency — not in our source files
        };

        let pf = &parsed_files[idx];
        let source_file = source_map.get_file(pf.file_id);

        match compile_frontend_with_registry(
            &pf.module,
            &pf.filename,
            source_file,
            &registry,
            strictness,
        ) {
            Ok((air_module, checker)) => {
                // Register exports for downstream modules
                let exports = collect_exports(module_id, &pf.path, &checker, &air_module);
                registry.register(exports);

                air_modules.push(air_module);
                source_infos.push(pf.filename.clone());
                module_source_paths.push(pf.path.clone());
            }
            Err(()) => found_errors = true,
        }
    }

    if found_errors {
        eprintln!("build: aborting due to errors");
        process::exit(1);
    }

    let frontend_ms = frontend_start.elapsed().as_millis();
    println!(
        "  frontend: {frontend_ms}ms ({} modules)",
        air_modules.len()
    );

    // Generate code for each target
    let toolchain_registry = ToolchainRegistry::with_builtins();

    for target in &options.targets {
        let target_start = Instant::now();
        let output_dir = if options.release {
            PathBuf::from(format!("build/release/{target}"))
        } else {
            PathBuf::from(format!("build/{target}"))
        };

        // Create output directory
        std::fs::create_dir_all(&output_dir).map_err(|e| {
            anyhow::anyhow!(
                "failed to create output directory {}: {e}",
                output_dir.display()
            )
        })?;

        // Create the generator for this target
        let generator: Box<dyn CodeGenerator> = create_generator(target)?;

        println!("  target: {target}");

        // Generate output: one file per module, mirroring source structure
        // under `build/<target>/` per spec §20.6.1.
        let mut total_files_written = 0;
        let module_inputs: Vec<(&bock_types::AIRModule, &Path)> = air_modules
            .iter()
            .zip(module_source_paths.iter())
            .map(|(m, p)| (m, p.as_path()))
            .collect();
        match generator.generate_project(&module_inputs) {
            Ok(mut generated) => {
                let emit_url_comment = matches!(target.as_str(), "js" | "ts");

                for output_file in &mut generated.files {
                    if options.source_map {
                        if let Some(sm) = output_file.source_map.as_mut() {
                            populate_source_map(sm, &parsed_files, &source_map);
                        }
                    }

                    let dest = output_dir.join(&output_file.path);
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }

                    if options.source_map {
                        if let Some(sm) = &output_file.source_map {
                            let map_name = format!(
                                "{}.map",
                                output_file
                                    .path
                                    .file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("output")
                            );
                            let map_path = dest.with_file_name(&map_name);
                            std::fs::write(&map_path, sm.to_source_map_v3_json())?;
                            if emit_url_comment {
                                if !output_file.content.is_empty()
                                    && !output_file.content.ends_with('\n')
                                {
                                    output_file.content.push('\n');
                                }
                                output_file
                                    .content
                                    .push_str(&format!("//# sourceMappingURL={map_name}\n"));
                            }
                            total_files_written += 1;
                        }
                    }

                    std::fs::write(&dest, &output_file.content)?;
                    total_files_written += 1;
                }
            }
            Err(e) => {
                eprintln!("error: codegen failed for target {target}: {e}");
                found_errors = true;
            }
        }

        if found_errors {
            eprintln!("build: aborting due to codegen errors");
            process::exit(1);
        }

        // Optionally invoke target compiler
        if !options.source_only {
            // Walk the output directory and invoke the toolchain on each generated file
            let ext = target_file_extension(target);
            let generated_files = find_files_with_extension(&output_dir, &ext)?;
            for gen_file in &generated_files {
                match toolchain_registry.invoke(target, gen_file, false) {
                    Ok(_result) => {}
                    Err(bock_build::toolchain::ToolchainError::NotFound {
                        install_hint, ..
                    }) => {
                        eprintln!(
                            "  warning: {target} toolchain not found, skipping compilation.\n  \
                             hint: {install_hint}"
                        );
                        break;
                    }
                    Err(e) => {
                        eprintln!("error: target compilation failed: {e}");
                        found_errors = true;
                    }
                }
            }
        }

        let target_ms = target_start.elapsed().as_millis();
        println!(
            "    wrote {total_files_written} file{} to {} ({target_ms}ms)",
            if total_files_written == 1 { "" } else { "s" },
            output_dir.display()
        );
    }

    if found_errors {
        eprintln!("build: completed with errors");
        process::exit(1);
    }

    // Post-build: `--pin-all` pins every unpinned build-scope decision
    // so the next production build passes the governance gate. Intended
    // workflow: run in development with `--pin-all`, commit the pins,
    // then ship with `--strict`/`--release`.
    if options.pin_all {
        let project_root = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("could not read current directory: {e}"))?;
        let pinned = pin_all_build_decisions(&project_root)?;
        println!("build: pin-all pinned {pinned} decision(s)");
    }

    let total_ms = total_start.elapsed().as_millis();
    println!("build: done ({total_ms}ms)");
    Ok(())
}

/// Run the §17.6 production validation step.
///
/// Reads the build manifest and exits with the spec-mandated error if
/// any entry is unpinned. Called before the frontend so users see the
/// actionable message without waiting for a full compile.
fn run_production_gate(project_root: &Path) -> anyhow::Result<()> {
    let writer = bock_ai::ManifestWriter::new(project_root);
    let decisions = writer
        .read_build()
        .map_err(|e| anyhow::anyhow!("could not read build manifest: {e}"))?;
    let report = bock_ai::validate_production(&decisions);
    if !report.is_empty() {
        eprint!("{}", report.render_error());
        process::exit(1);
    }
    Ok(())
}

/// Walk every JSON file under `.bock/decisions/build/`, flipping any
/// unpinned decision's `pinned` flag to `true` with pin metadata.
///
/// Returns the number of decisions newly pinned. Returns `Ok(0)` when
/// the tree is missing — a fresh project has nothing to pin.
fn pin_all_build_decisions(project_root: &Path) -> anyhow::Result<usize> {
    let build_root = project_root.join(".bock").join("decisions").join("build");
    if !build_root.exists() {
        return Ok(0);
    }

    let mut files: Vec<PathBuf> = Vec::new();
    collect_json_files(&build_root, &mut files)?;

    let who = pinned_by();
    let now = chrono::Utc::now();
    let mut total_pinned = 0usize;

    for file in files {
        let bytes = std::fs::read(&file)
            .map_err(|e| anyhow::anyhow!("could not read {}: {e}", file.display()))?;
        let mut entries: Vec<bock_ai::Decision> = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("could not parse {}: {e}", file.display()))?;
        let mut changed = false;
        for entry in entries.iter_mut() {
            if entry.pinned {
                continue;
            }
            entry.pinned = true;
            entry.pin_reason = Some(format!("bulk-pinned via `bock build --pin-all` by {who}"));
            entry.pinned_at = Some(now);
            entry.pinned_by = Some(who.clone());
            changed = true;
            total_pinned += 1;
        }
        if changed {
            let pretty = serde_json::to_vec_pretty(&entries)
                .map_err(|e| anyhow::anyhow!("could not serialize {}: {e}", file.display()))?;
            std::fs::write(&file, pretty)
                .map_err(|e| anyhow::anyhow!("could not write {}: {e}", file.display()))?;
        }
    }

    Ok(total_pinned)
}

fn collect_json_files(root: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_json_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

fn pinned_by() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into())
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

    // Lex
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    collect_diagnostics(&mut diags, lexer.diagnostics());

    if has_errors(&diags) {
        print_diagnostics(&diags, &filename, &source_file.content);
        return Err(());
    }

    // Parse
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

/// Run the frontend pipeline on a pre-parsed module with cross-file registry.
///
/// Resolve → lower → type-check → analysis passes.
/// Returns the AIR module and the type checker (needed for export collection).
fn compile_frontend_with_registry(
    module: &bock_ast::Module,
    filename: &str,
    source_file: &bock_source::SourceFile,
    registry: &ModuleRegistry,
    strictness: Strictness,
) -> Result<(bock_types::AIRModule, TypeChecker), ()> {
    let mut all_diagnostics: Vec<Diagnostic> = Vec::new();

    // Name resolution (with registry for cross-file imports)
    let mut symbols = SymbolTable::new();
    register_builtins(&mut symbols);
    let resolve_diags = resolve_names_with_registry(module, &mut symbols, registry);
    collect_diagnostics(&mut all_diagnostics, &resolve_diags);

    if has_errors(&all_diagnostics) {
        print_diagnostics(&all_diagnostics, filename, &source_file.content);
        return Err(());
    }

    // Lower to S-AIR
    let id_gen = NodeIdGen::new();
    let mut air_module = lower_module(module, &id_gen, &symbols);

    // Type check (T-AIR)
    let mut checker = TypeChecker::new();
    register_type_builtins(&mut checker);
    seed_imports(&mut checker, &module.imports, registry);
    checker.check_module(&mut air_module);
    collect_diagnostics(&mut all_diagnostics, &checker.diags);

    if has_errors(&all_diagnostics) {
        print_diagnostics(&all_diagnostics, filename, &source_file.content);
        return Err(());
    }

    // Analysis passes (ownership, effects, capabilities)
    let ownership_diags = bock_types::analyze_ownership(&air_module);
    collect_diagnostics(&mut all_diagnostics, &ownership_diags);

    let effect_diags = bock_types::track_effects(&air_module, strictness);
    collect_diagnostics(&mut all_diagnostics, &effect_diags);

    let capability_diags = bock_types::verify_capabilities(&air_module, strictness);
    collect_diagnostics(&mut all_diagnostics, &capability_diags);

    if has_errors(&all_diagnostics) {
        print_diagnostics(&all_diagnostics, filename, &source_file.content);
        return Err(());
    }

    // Print warnings
    let warnings: Vec<&Diagnostic> = all_diagnostics
        .iter()
        .filter(|d| d.severity != Severity::Error)
        .collect();
    if !warnings.is_empty() {
        let to_render: Vec<Diagnostic> = warnings.into_iter().cloned().collect();
        print_diagnostics(&to_render, filename, &source_file.content);
    }

    Ok((air_module, checker))
}

/// Fill in `sources` + `src_line`/`src_col` on a codegen-produced source map.
/// Uses the compilation's parsed files and their contents from `source_map`.
fn populate_source_map(
    sm: &mut bock_codegen::SourceMap,
    parsed: &[ParsedFile],
    source_map: &SourceMap,
) {
    // Determine the highest file id referenced by any mapping so we only
    // include relevant sources.
    let max_file_id = sm.mappings.iter().map(|m| m.src_file_id).max().unwrap_or(0);

    // Build a map from FileId → (path, content) by iterating known parsed
    // files. Each parsed file's `file_id` gives us a stable index.
    let mut contents: Vec<Option<(String, String)>> =
        vec![None; (max_file_id as usize).saturating_add(1)];
    for pf in parsed {
        let file = source_map.get_file(pf.file_id);
        let idx = pf.file_id.0 as usize;
        if idx < contents.len() {
            contents[idx] = Some((file.path.display().to_string(), file.content.clone()));
        }
    }

    // Attach sources in file-id order; absent slots become placeholder entries.
    sm.sources = contents
        .iter()
        .enumerate()
        .map(|(i, opt)| match opt {
            Some((path, content)) => SourceInfo {
                path: path.clone(),
                content: Some(content.clone()),
            },
            None => SourceInfo {
                path: format!("<unknown-{i}>"),
                content: None,
            },
        })
        .collect();

    // Resolve each mapping's (src_line, src_col) from its byte offset.
    let content_refs: Vec<&str> = contents
        .iter()
        .map(|o| o.as_ref().map(|(_, c)| c.as_str()).unwrap_or(""))
        .collect();
    sm.resolve_positions(&content_refs);
}

/// Create a code generator for the given target ID.
fn create_generator(target: &str) -> anyhow::Result<Box<dyn CodeGenerator>> {
    match target {
        "js" => Ok(Box::new(JsGenerator::new())),
        "ts" => Ok(Box::new(TsGenerator::new())),
        "python" => Ok(Box::new(PyGenerator::new())),
        "rust" => Ok(Box::new(RsGenerator::new())),
        "go" => Ok(Box::new(GoGenerator::new())),
        _ => {
            anyhow::bail!(
                "unknown target '{target}'. Valid targets: {}",
                KNOWN_TARGETS.join(", ")
            );
        }
    }
}

/// Get the file extension for a target's generated files.
///
/// Single source of truth lives on each [`bock_codegen::TargetProfile`] —
/// this helper just delegates so call sites don't duplicate the table.
fn target_file_extension(target: &str) -> String {
    bock_codegen::TargetProfile::from_id(target)
        .map(|p| p.conventions.file_extension)
        .unwrap_or_else(|| "txt".to_string())
}

/// Find all files with a given extension in a directory (recursive).
fn find_files_with_extension(dir: &Path, ext: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    if !dir.is_dir() {
        return Ok(result);
    }
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            result.extend(find_files_with_extension(&path, ext)?);
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            result.push(path);
        }
    }
    result.sort();
    Ok(result)
}

/// Discover `.bock` files recursively from the given directory.
fn discover_bock_files_recursive(dir: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_dir_recursive(Path::new(dir), &mut files)?;
    files.sort();
    Ok(files)
}

/// Recursively walk a directory collecting `.bock` files.
fn walk_dir_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("could not read directory '{}': {e}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Skip build output directories and hidden directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.')
                    || name == "build"
                    || name == "target"
                    || name == "node_modules"
                    || name == "test"
                    || name == "tests"
                {
                    continue;
                }
            }
            walk_dir_recursive(&path, files)?;
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

/// Print diagnostics with source context.
fn print_diagnostics(diagnostics: &[Diagnostic], filename: &str, source: &str) {
    let rendered = bock_errors::render(diagnostics, filename, source);
    eprint!("{rendered}");
}

/// Register builtin function types in the type checker.
fn register_type_builtins(checker: &mut TypeChecker) {
    use bock_types::{FnType, PrimitiveType, Type};
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

/// Register interpreter builtin globals in the symbol table for name resolution.
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
        ("Ok", u32::MAX - 10),
        ("Err", u32::MAX - 11),
        ("Some", u32::MAX - 12),
        ("None", u32::MAX - 13),
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

/// Parse a target list, validating each target ID.
pub fn parse_targets(target: Option<String>, all_targets: bool) -> anyhow::Result<Vec<String>> {
    if all_targets {
        return Ok(KNOWN_TARGETS.iter().map(|s| (*s).to_string()).collect());
    }

    match target {
        Some(t) => {
            if !KNOWN_TARGETS.contains(&t.as_str()) {
                anyhow::bail!(
                    "unknown target '{t}'. Valid targets: {}",
                    KNOWN_TARGETS.join(", ")
                );
            }
            Ok(vec![t])
        }
        None => {
            // Default to js
            Ok(vec!["js".to_string()])
        }
    }
}
