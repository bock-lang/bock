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
//!    c. Type-check (T-AIR) — runs for the `types` aspect
//!    d. Ownership + effect analysis — full check only
//!    e. Context interpretation (C-AIR) then context-system validation (§11) — runs for the `context` aspect (capability verification + the strictness-gated context-validation pass)
//!    f. Collect exports → register in [`bock_air::registry::ModuleRegistry`]
//! 6. Report accumulated diagnostics
//!
//! Which of the analysis passes run is controlled by [`CheckOptions::aspects`]
//! (spec §20.1.1 `--only`): the default full check runs every pass, while an
//! `--only` restriction runs only the selected aspects' passes (lex, parse,
//! name resolution, and lowering always run).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bock_air::{
    interpret_context, lower_module, resolve_names_with_registry, validate_context, ModuleRegistry,
    NodeIdGen, StrictnessLevel, SymbolTable,
};
use bock_build::dep_graph::{self, DepGraph};
use bock_errors::{Diagnostic, DiagnosticBag, Severity};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{
    collect_exports, seed_imports, FnType, PrimitiveType, Strictness, Type, TypeChecker,
};

/// A v1 aspect of analysis that `bock check --only=<aspect>` can select.
///
/// Per spec §20.1.1, v1 ships exactly two aspects. `lint` is a v1.x aspect
/// (the lint pass exists and runs in the default check, but `--only=lint` is
/// not yet a valid value); unknown values are rejected with the list of valid
/// aspects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Aspect {
    /// Type checking (the T-AIR pass).
    Types,
    /// Context-system validation (§11). In v1 this maps to capability
    /// (`@requires`) verification — the compiler-verified §11 surface that runs
    /// in the default check.
    Context,
}

impl Aspect {
    /// The canonical lowercase name used on the command line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Aspect::Types => "types",
            Aspect::Context => "context",
        }
    }

    /// Parse a single aspect name, returning `None` for unknown values.
    ///
    /// `lint` is intentionally rejected in v1: the lint pass runs in the
    /// default check, but `--only=lint` ships in v1.x alongside `bock fix`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Aspect> {
        match name {
            "types" => Some(Aspect::Types),
            "context" => Some(Aspect::Context),
            _ => None,
        }
    }

    /// All valid v1 aspects, in canonical order.
    pub const ALL: [Aspect; 2] = [Aspect::Types, Aspect::Context];

    /// The comma-separated list of valid v1 aspect names, for error messages.
    ///
    /// Derived from [`Aspect::ALL`] / [`Aspect::as_str`] so the message can
    /// never drift from the set of parseable aspects.
    #[must_use]
    pub fn valid_list() -> String {
        Self::ALL
            .iter()
            .map(|a| a.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Which aspects the check should run.
///
/// `All` is the default full check (every pass, unchanged from pre-`--only`
/// behavior). `Only` restricts the check to the selected aspects' passes; the
/// unconditional infrastructure (lex, parse, name resolution, lowering) always
/// runs regardless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AspectSelection {
    /// No `--only` was given: run the full check (all passes).
    All,
    /// `--only=<aspect>...` was given: run only these aspects' passes.
    Only(std::collections::HashSet<Aspect>),
}

impl AspectSelection {
    /// Build an [`AspectSelection`] from the raw `--only` values.
    ///
    /// Each raw value may itself be a comma-separated list (e.g. `types,context`),
    /// and the flag may be repeated; both forms accumulate into one set. An empty
    /// `raw` (no `--only` given) yields [`AspectSelection::All`].
    ///
    /// # Errors
    ///
    /// Returns the offending unknown aspect name (the first encountered) if any
    /// value does not name a valid v1 aspect.
    pub fn from_raw(raw: &[String]) -> Result<AspectSelection, String> {
        if raw.is_empty() {
            return Ok(AspectSelection::All);
        }
        let mut set = std::collections::HashSet::new();
        for value in raw {
            for part in value.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                match Aspect::parse(part) {
                    Some(aspect) => {
                        set.insert(aspect);
                    }
                    None => return Err(part.to_string()),
                }
            }
        }
        if set.is_empty() {
            // `--only=` or `--only=,` with no real values: treat as the full
            // check rather than a check that runs nothing.
            return Ok(AspectSelection::All);
        }
        Ok(AspectSelection::Only(set))
    }

    /// Whether the given aspect's passes should run under this selection.
    #[must_use]
    pub fn runs(&self, aspect: Aspect) -> bool {
        match self {
            AspectSelection::All => true,
            AspectSelection::Only(set) => set.contains(&aspect),
        }
    }

    /// Whether this is the full (default) check — all passes run.
    #[must_use]
    pub fn is_full(&self) -> bool {
        matches!(self, AspectSelection::All)
    }
}

/// Options controlling which checks to run and how diagnostics are rendered.
pub struct CheckOptions {
    /// Which aspects of analysis to run (§20.1.1 `--only`).
    pub aspects: AspectSelection,
    /// Brief output: omit source-context snippets, one line per diagnostic
    /// (§20.1.1 `--brief`). `false` is the default rich rendering.
    pub brief: bool,
    /// Force production strictness for the check (§20.1 `--strict`). When
    /// `false` (the default) the check runs at development strictness, so
    /// completeness gaps (e.g. a public item missing `@context`) are warnings
    /// and the check still exits clean. When `true`, those gaps become errors
    /// and the check fails. Mirrors `bock build --strict`.
    pub strict: bool,
}

impl CheckOptions {
    /// The [`Strictness`] this check runs at: production under `--strict`,
    /// development otherwise. `check` never selects sketch strictness — the
    /// flag is a binary override matching `bock build --strict`, not the full
    /// sketch/development/production ladder (§1.4).
    #[must_use]
    pub fn strictness(&self) -> Strictness {
        if self.strict {
            Strictness::Production
        } else {
            Strictness::Development
        }
    }
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            aspects: AspectSelection::All,
            brief: false,
            strict: false,
        }
    }
}

/// Map a [`Strictness`] (the CLI's sketch/development/production ladder, §1.4)
/// to the context-validation pass's [`StrictnessLevel`] profile.
///
/// - [`Strictness::Sketch`] → [`StrictnessLevel::Lax`] (error-level checks only;
///   no completeness diagnostics).
/// - [`Strictness::Development`] → [`StrictnessLevel::Standard`] (completeness
///   gaps are warnings).
/// - [`Strictness::Production`] → [`StrictnessLevel::Strict`] (completeness gaps
///   are errors).
///
/// `check` only ever passes development (default) or production (`--strict`);
/// sketch is mapped for completeness so the function is total.
fn context_strictness_level(strictness: Strictness) -> StrictnessLevel {
    match strictness {
        Strictness::Sketch => StrictnessLevel::Lax,
        Strictness::Development => StrictnessLevel::Standard,
        Strictness::Production => StrictnessLevel::Strict,
    }
}

/// The pass/fail result of a check run.
///
/// [`run`] returns this instead of terminating the process directly, so the
/// pass/fail decision is testable at the function level and the single
/// outcome-to-exit-code mapping lives in `main`. The `anyhow::Result` wrapper
/// around this type is reserved for *unexpected* failures (e.g. an unreadable
/// directory during discovery); ordinary check failures (parse/type/analysis
/// errors, a dependency cycle, no files found, an unreadable input file) are
/// reported as [`CheckOutcome::Failed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The check completed with no errors. Maps to a successful (zero) exit.
    Clean,
    /// The check found at least one error (or no files to check). Maps to a
    /// non-zero exit.
    Failed,
}

impl CheckOutcome {
    /// Whether this outcome represents a clean (error-free) check.
    pub fn is_clean(self) -> bool {
        matches!(self, CheckOutcome::Clean)
    }
}

/// Run the check command on the given file paths with the specified options.
///
/// Uses the multi-file pipeline: parse all → dependency sort → compile in order
/// with cross-file name resolution via [`bock_air::registry::ModuleRegistry`].
///
/// Returns the pass/fail [`CheckOutcome`] rather than exiting the process, so the
/// outcome is testable and the exit-code decision is centralized in `main`. The
/// `Err` arm is reserved for unexpected I/O failures surfaced via `?`.
pub fn run(files: Vec<PathBuf>, options: &CheckOptions) -> anyhow::Result<CheckOutcome> {
    let files = if files.is_empty() {
        discover_bock_files(".")?
    } else {
        files
    };

    if files.is_empty() {
        eprintln!("No .bock files found.");
        return Ok(CheckOutcome::Failed);
    }

    let mut found_errors = false;

    // ── Phase 1: Parse all files ──────────────────────────────────────────────
    let mut source_map = SourceMap::new();
    let mut parsed_files: Vec<ParsedFile> = Vec::new();

    for file_path in &files {
        match parse_file(file_path, &mut source_map, !options.brief) {
            Ok(pf) => parsed_files.push(pf),
            Err(()) => found_errors = true,
        }
    }

    if found_errors {
        return Ok(CheckOutcome::Failed);
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
            return Ok(CheckOutcome::Failed);
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
                !options.brief,
            );
            found_errors = true;
            continue;
        }

        // 4b. Lower to S-AIR, then interpret context annotations (C-AIR) so the
        // `context` slot is populated on every node before any context-aware
        // pass (capability verification, context validation) reads it. The
        // interpreter's own diagnostics (e.g. unknown capability names) are
        // collected unconditionally — they are not gated behind an aspect.
        let id_gen = NodeIdGen::new();
        let mut air_module = lower_module(&pf.module, &id_gen, &symbols);
        let context_diags = interpret_context(&mut air_module);
        collect_diagnostics(&mut all_diagnostics, &context_diags);

        // 4c. Type checking (T-AIR) — runs for the `types` aspect (and the
        // full check). The checker is always constructed and seeded so that
        // `collect_exports` below has the type information it needs even when
        // type checking itself is skipped.
        let mut checker = TypeChecker::new();
        register_type_builtins(&mut checker);
        seed_imports(&mut checker, &pf.module.imports, &registry);
        if options.aspects.runs(Aspect::Types) {
            checker.check_module(&mut air_module);
            collect_diagnostics(&mut all_diagnostics, &checker.diags);
        }

        // The chosen strictness (development by default, production under
        // `--strict`) threads through every strictness-gated pass below.
        let strictness = options.strictness();

        // 4d. Ownership and effect analysis — part of the default full check.
        // Neither is a v1 `--only` aspect, so they run only when no `--only`
        // restriction is in effect.
        if options.aspects.is_full() {
            let ownership_diags = bock_types::analyze_ownership(&air_module);
            collect_diagnostics(&mut all_diagnostics, &ownership_diags);

            let effect_diags = bock_types::track_effects(&air_module, strictness);
            collect_diagnostics(&mut all_diagnostics, &effect_diags);
        }

        // 4e. Context-system validation (§11) — runs for the `context` aspect
        // (and the full check). Two compiler-verified §11 surfaces run here:
        //   * capability (`@requires`) verification, and
        //   * the context-validation pass (annotation consistency +
        //     completeness), gated by strictness via `validate_context`.
        // Both are gated by the chosen strictness, so under `--strict`
        // (production) completeness gaps become errors → non-zero exit, while
        // under the default (development) they stay warnings → exit 0. Note
        // PII/security context *composition* (cross-module leak detection via
        // `bock_air::compose_context`) is intentionally NOT wired here: it is
        // reserved for a dedicated v1.x security pass (spec §20.1.1).
        if options.aspects.runs(Aspect::Context) {
            let capability_diags = bock_types::verify_capabilities(&air_module, strictness);
            collect_diagnostics(&mut all_diagnostics, &capability_diags);

            let context_validation_diags =
                validate_context(&air_module, context_strictness_level(strictness));
            collect_diagnostics(&mut all_diagnostics, &context_validation_diags);
        }

        // Report diagnostics for this module. The lint pass runs as part of the
        // default full check; `--only=lint` is a v1.x value (rejected in v1),
        // so under any `--only` restriction we surface every collected
        // diagnostic (the selected aspects' output, errors and warnings alike).
        let module_has_errors = has_errors(&all_diagnostics);

        if !all_diagnostics.is_empty() {
            print_diagnostics(
                &all_diagnostics,
                &pf.filename,
                &source_file.content,
                !options.brief,
            );
        }

        if module_has_errors {
            found_errors = true;
        } else {
            // 4f. Register exports for downstream modules
            let exports = collect_exports(module_id, &pf.path, &checker, &air_module);
            registry.register(exports);
        }
    }

    if found_errors {
        return Ok(CheckOutcome::Failed);
    }

    let file_count = files.len();
    let label = if file_count == 1 { "file" } else { "files" };
    println!("check: {file_count} {label} checked, no errors.");
    Ok(CheckOutcome::Clean)
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

    // ── Exit-code contract (function-level) ───────────────────────────────
    //
    // `run` returns a `CheckOutcome` instead of calling `process::exit`, so
    // the pass/fail decision is unit-testable without spawning a subprocess.

    #[test]
    fn run_clean_file_returns_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.bock");
        fs::write(&path, "fn add(a: Int, b: Int) -> Int { a + b }\n").unwrap();

        let outcome = run(vec![path], &CheckOptions::default()).unwrap();
        assert_eq!(outcome, CheckOutcome::Clean);
        assert!(outcome.is_clean());
    }

    #[test]
    fn run_parse_error_returns_failed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.bock");
        fs::write(&path, "fn { broken\n").unwrap();

        let outcome = run(vec![path], &CheckOptions::default()).unwrap();
        assert_eq!(outcome, CheckOutcome::Failed);
        assert!(!outcome.is_clean());
    }

    #[test]
    fn run_missing_file_returns_failed() {
        // A path that does not exist is an input error, not an unexpected
        // I/O failure: it is reported as `Failed`, not as `Err`.
        let path = PathBuf::from("/nonexistent/definitely-not-here-12345.bock");
        let outcome = run(vec![path], &CheckOptions::default()).unwrap();
        assert_eq!(outcome, CheckOutcome::Failed);
    }

    #[test]
    fn run_analysis_error_returns_failed() {
        // Use-after-move of a record triggers an ownership (analysis) error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ownership.bock");
        fs::write(
            &path,
            "record Thing { id: Int }\nfn process() {\n    let data = Thing { id: 1 }\n    let archive = data\n    let x = data\n}\n",
        )
        .unwrap();

        let outcome = run(vec![path], &CheckOptions::default()).unwrap();
        assert_eq!(outcome, CheckOutcome::Failed);
    }

    #[test]
    fn check_outcome_is_clean_helper() {
        assert!(CheckOutcome::Clean.is_clean());
        assert!(!CheckOutcome::Failed.is_clean());
    }

    // ── Aspect / --only parsing (§20.1.1) ─────────────────────────────────

    #[test]
    fn aspect_parse_recognizes_v1_aspects() {
        assert_eq!(Aspect::parse("types"), Some(Aspect::Types));
        assert_eq!(Aspect::parse("context"), Some(Aspect::Context));
    }

    #[test]
    fn aspect_parse_rejects_lint_and_unknown_in_v1() {
        // `lint` is a v1.x aspect: the pass runs by default, but `--only=lint`
        // is not a valid v1 value.
        assert_eq!(Aspect::parse("lint"), None);
        assert_eq!(Aspect::parse("ownership"), None);
        assert_eq!(Aspect::parse(""), None);
    }

    #[test]
    fn aspect_as_str_round_trips() {
        for aspect in Aspect::ALL {
            assert_eq!(Aspect::parse(aspect.as_str()), Some(aspect));
        }
        let valid = Aspect::valid_list();
        assert!(valid.contains("types"));
        assert!(valid.contains("context"));
    }

    #[test]
    fn aspect_selection_empty_is_full_check() {
        assert_eq!(
            AspectSelection::from_raw(&[]).unwrap(),
            AspectSelection::All
        );
        // `--only=` and `--only=,` collapse to the full check, not an empty run.
        assert_eq!(
            AspectSelection::from_raw(&["".to_string()]).unwrap(),
            AspectSelection::All
        );
        assert_eq!(
            AspectSelection::from_raw(&[",".to_string()]).unwrap(),
            AspectSelection::All
        );
    }

    #[test]
    fn aspect_selection_comma_list_and_repeated_are_equivalent() {
        let comma = AspectSelection::from_raw(&["types,context".to_string()]).unwrap();
        let repeated =
            AspectSelection::from_raw(&["types".to_string(), "context".to_string()]).unwrap();
        assert_eq!(comma, repeated);
        // Both select exactly types + context.
        assert!(comma.runs(Aspect::Types));
        assert!(comma.runs(Aspect::Context));
        assert!(!comma.is_full());
    }

    #[test]
    fn aspect_selection_single_aspect_runs_only_that_aspect() {
        let only_types = AspectSelection::from_raw(&["types".to_string()]).unwrap();
        assert!(only_types.runs(Aspect::Types));
        assert!(!only_types.runs(Aspect::Context));

        let only_context = AspectSelection::from_raw(&["context".to_string()]).unwrap();
        assert!(only_context.runs(Aspect::Context));
        assert!(!only_context.runs(Aspect::Types));
    }

    #[test]
    fn aspect_selection_trims_whitespace() {
        let sel = AspectSelection::from_raw(&[" types , context ".to_string()]).unwrap();
        assert!(sel.runs(Aspect::Types));
        assert!(sel.runs(Aspect::Context));
    }

    #[test]
    fn aspect_selection_unknown_value_errors_with_the_offending_name() {
        let err = AspectSelection::from_raw(&["types,bogus".to_string()]).unwrap_err();
        assert_eq!(err, "bogus");
        // `lint` is rejected in v1 too.
        assert_eq!(
            AspectSelection::from_raw(&["lint".to_string()]).unwrap_err(),
            "lint"
        );
    }

    #[test]
    fn full_check_runs_every_aspect() {
        let full = AspectSelection::All;
        assert!(full.is_full());
        assert!(full.runs(Aspect::Types));
        assert!(full.runs(Aspect::Context));
    }

    #[test]
    fn run_only_types_on_clean_file_returns_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.bock");
        fs::write(&path, "fn add(a: Int, b: Int) -> Int { a + b }\n").unwrap();

        let options = CheckOptions {
            aspects: AspectSelection::Only([Aspect::Types].into_iter().collect()),
            brief: false,
            strict: false,
        };
        let outcome = run(vec![path], &options).unwrap();
        assert_eq!(outcome, CheckOutcome::Clean);
    }

    #[test]
    fn run_only_context_on_clean_file_returns_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.bock");
        fs::write(&path, "fn add(a: Int, b: Int) -> Int { a + b }\n").unwrap();

        let options = CheckOptions {
            aspects: AspectSelection::Only([Aspect::Context].into_iter().collect()),
            brief: false,
            strict: false,
        };
        let outcome = run(vec![path], &options).unwrap();
        assert_eq!(outcome, CheckOutcome::Clean);
    }

    #[test]
    fn run_brief_on_clean_file_returns_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.bock");
        fs::write(&path, "fn add(a: Int, b: Int) -> Int { a + b }\n").unwrap();

        let options = CheckOptions {
            aspects: AspectSelection::All,
            brief: true,
            strict: false,
        };
        let outcome = run(vec![path], &options).unwrap();
        assert_eq!(outcome, CheckOutcome::Clean);
    }

    // ── --strict / strictness mapping (§20.1) ─────────────────────────────

    #[test]
    fn check_options_strictness_maps_strict_flag() {
        // Default check runs at development strictness.
        let dev = CheckOptions::default();
        assert!(!dev.strict);
        assert_eq!(dev.strictness(), Strictness::Development);

        // `--strict` forces production strictness.
        let prod = CheckOptions {
            aspects: AspectSelection::All,
            brief: false,
            strict: true,
        };
        assert_eq!(prod.strictness(), Strictness::Production);
    }

    #[test]
    fn context_strictness_level_maps_profiles() {
        // Sketch→Lax, Development→Standard, Production→Strict (the
        // validate_context profile ladder, mirroring §1.4 strictness).
        assert_eq!(
            context_strictness_level(Strictness::Sketch),
            StrictnessLevel::Lax
        );
        assert_eq!(
            context_strictness_level(Strictness::Development),
            StrictnessLevel::Standard
        );
        assert_eq!(
            context_strictness_level(Strictness::Production),
            StrictnessLevel::Strict
        );
    }

    /// A public item with no `@context` annotation. Under development
    /// strictness this is a completeness *warning*; under production it is an
    /// *error*. (`module Lib` itself also lacks `@context`, exercising the
    /// module-level completeness rule.)
    const PUBLIC_ITEM_NO_CONTEXT: &str = "module Lib

public fn add(a: Int, b: Int) -> Int { a + b }
";

    #[test]
    fn default_check_treats_missing_context_as_warning_clean_exit() {
        // Without --strict, a public item missing @context is a warning: the
        // check still exits clean (warnings never fail the check).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.bock");
        fs::write(&path, PUBLIC_ITEM_NO_CONTEXT).unwrap();

        let outcome = run(vec![path], &CheckOptions::default()).unwrap();
        assert_eq!(
            outcome,
            CheckOutcome::Clean,
            "missing @context must be a warning (exit clean) at development strictness"
        );
    }

    #[test]
    fn strict_check_treats_missing_context_as_error_failed_exit() {
        // With --strict, the same missing @context becomes an error: the check
        // fails. This is the O1+O2 composition — validate_context's
        // completeness gate flips from warning to error under production
        // strictness and that error drives the non-zero exit (H1's contract).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.bock");
        fs::write(&path, PUBLIC_ITEM_NO_CONTEXT).unwrap();

        let options = CheckOptions {
            aspects: AspectSelection::All,
            brief: false,
            strict: true,
        };
        let outcome = run(vec![path], &options).unwrap();
        assert_eq!(
            outcome,
            CheckOutcome::Failed,
            "missing @context must be an error (exit failed) under --strict / production"
        );
    }

    #[test]
    fn only_context_runs_validate_context_completeness_gate() {
        // `--only=context` must run the context-validation pass (not just
        // capability verification): under --strict it flips the same
        // missing-@context completeness gap to an error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.bock");
        fs::write(&path, PUBLIC_ITEM_NO_CONTEXT).unwrap();

        // --only=context, development: warning → clean.
        let dev = CheckOptions {
            aspects: AspectSelection::Only([Aspect::Context].into_iter().collect()),
            brief: false,
            strict: false,
        };
        assert_eq!(
            run(vec![path.clone()], &dev).unwrap(),
            CheckOutcome::Clean,
            "--only=context at development strictness must surface completeness as a warning"
        );

        // --only=context, --strict: error → failed. If validate_context were
        // not wired into the context aspect, this would still be Clean.
        let strict = CheckOptions {
            aspects: AspectSelection::Only([Aspect::Context].into_iter().collect()),
            brief: false,
            strict: true,
        };
        assert_eq!(
            run(vec![path], &strict).unwrap(),
            CheckOutcome::Failed,
            "--only=context --strict must flag missing @context as an error"
        );
    }

    #[test]
    fn only_types_does_not_run_context_validation() {
        // The context-validation pass must be gated by the `context` aspect:
        // `--only=types --strict` does NOT run it, so a public item missing
        // @context stays clean (no completeness error from the types aspect).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.bock");
        fs::write(&path, PUBLIC_ITEM_NO_CONTEXT).unwrap();

        let options = CheckOptions {
            aspects: AspectSelection::Only([Aspect::Types].into_iter().collect()),
            brief: false,
            strict: true,
        };
        assert_eq!(
            run(vec![path], &options).unwrap(),
            CheckOutcome::Clean,
            "--only=types must not run context completeness validation even under --strict"
        );
    }
}
