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
use bock_errors::{Diagnostic, DiagnosticBag, Severity, Span};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{
    collect_exports, seed_imports, seed_prelude, FnType, PrimitiveType, Strictness, Type,
    TypeChecker,
};

use crate::output::{
    byte_to_line_col, diagnostic_json, io_error_json, print_document, OutputFormat, FORMAT_VERSION,
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
    /// Output format (`--format`): `human` renders diagnostics to stderr as
    /// they surface; `json` collects them and emits one machine-readable
    /// JSON document on stdout (see [`crate::output`]).
    pub format: OutputFormat,
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
            format: OutputFormat::Human,
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

/// Destination for surfaced diagnostics.
///
/// The human format renders immediately to stderr as diagnostics surface,
/// preserving the streaming behavior; json collects every surfaced
/// diagnostic so [`run`] can emit them inside the single end-of-run JSON
/// document. Both consume the same structured [`Diagnostic`] values — the
/// JSON is serialized from the diagnostics themselves, never re-parsed from
/// rendered text.
enum DiagnosticSink {
    /// Render to stderr (rich by default, one-line with `--brief`).
    Human {
        /// Whether `--brief` suppressed the source-context snippets.
        brief: bool,
    },
    /// Collect serialized diagnostics for the end-of-run document. Nothing
    /// else may write to stdout in this mode.
    Json {
        /// The serialized diagnostics, in surfacing order.
        diagnostics: Vec<serde_json::Value>,
    },
}

impl DiagnosticSink {
    fn new(options: &CheckOptions) -> Self {
        match options.format {
            OutputFormat::Human => DiagnosticSink::Human {
                brief: options.brief,
            },
            OutputFormat::Json => DiagnosticSink::Json {
                diagnostics: Vec::new(),
            },
        }
    }

    /// Surface diagnostics located in one file. `source` is the file content
    /// backing the diagnostics' spans.
    fn emit(&mut self, diagnostics: &[Diagnostic], filename: &str, source: &str) {
        match self {
            DiagnosticSink::Human { brief } => {
                print_diagnostics(diagnostics, filename, source, !*brief);
            }
            DiagnosticSink::Json { diagnostics: acc } => {
                acc.extend(
                    diagnostics
                        .iter()
                        .map(|d| diagnostic_json(d, Some(filename), Some(source))),
                );
            }
        }
    }

    /// Surface diagnostics that have no backing source file (e.g. a module
    /// cycle that cannot be pinned to a specific `use` edge).
    fn emit_unlocated(&mut self, diagnostics: &[Diagnostic]) {
        match self {
            DiagnosticSink::Human { .. } => eprintln!("{}", render_codeonly(diagnostics)),
            DiagnosticSink::Json { diagnostics: acc } => {
                acc.extend(diagnostics.iter().map(|d| diagnostic_json(d, None, None)));
            }
        }
    }

    /// Surface an I/O-class failure — an unreadable input file or "no files
    /// found" — that has no compiler [`Diagnostic`] behind it.
    ///
    /// Both formats keep the historical stderr line (so a human watching a
    /// json-mode run still sees the reason on the terminal); json mode
    /// additionally records a `code: null` entry (see
    /// [`crate::output::io_error_json`]) so the stdout document itself
    /// explains why the check failed.
    fn emit_io_error(&mut self, message: &str, file: Option<&str>) {
        match file {
            Some(f) => eprintln!("error: {f}: {message}"),
            None => eprintln!("{message}"),
        }
        if let DiagnosticSink::Json { diagnostics: acc } = self {
            acc.push(io_error_json(message, file));
        }
    }
}

/// Build the `bock check --format json` document (see [`crate::output`] for
/// the shared envelope contract).
fn check_document(
    outcome: CheckOutcome,
    file_count: usize,
    diagnostics: &[serde_json::Value],
) -> serde_json::Value {
    let count = |severity: &str| {
        diagnostics
            .iter()
            .filter(|d| d["severity"] == severity)
            .count()
    };
    serde_json::json!({
        "format_version": FORMAT_VERSION,
        "command": "check",
        "outcome": if outcome.is_clean() { "clean" } else { "failed" },
        "summary": {
            "files": file_count,
            "errors": count("error"),
            "warnings": count("warning"),
        },
        "diagnostics": diagnostics,
    })
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
    let mut sink = DiagnosticSink::new(options);
    let (outcome, file_count) = run_pipeline(files, options, &mut sink)?;
    match sink {
        DiagnosticSink::Human { .. } => {
            if outcome.is_clean() {
                let label = if file_count == 1 { "file" } else { "files" };
                println!("check: {file_count} {label} checked, no errors.");
            }
        }
        DiagnosticSink::Json { diagnostics } => {
            print_document(&check_document(outcome, file_count, &diagnostics))?;
        }
    }
    Ok(outcome)
}

/// The check pipeline proper: everything [`run`] does except the end-of-run
/// output (the clean summary line or the JSON document). Diagnostics surface
/// through `sink` — including I/O-class failures (an unreadable input, "no
/// files found"), which keep their stderr line in both formats but also land
/// in the json document as `code: null` entries (see
/// [`DiagnosticSink::emit_io_error`]). Returns the outcome plus the number
/// of input files, for the summary.
fn run_pipeline(
    files: Vec<PathBuf>,
    options: &CheckOptions,
    sink: &mut DiagnosticSink,
) -> anyhow::Result<(CheckOutcome, usize)> {
    let files = if files.is_empty() {
        discover_bock_files(".")?
    } else {
        files
    };

    if files.is_empty() {
        sink.emit_io_error("No .bock files found.", None);
        return Ok((CheckOutcome::Failed, 0));
    }

    let mut found_errors = false;

    // ── Phase 1: Parse all files ──────────────────────────────────────────────
    let mut source_map = SourceMap::new();
    let mut parsed_files: Vec<ParsedFile> = Vec::new();

    // Prepend the embedded core-stdlib sources so they flow through the SAME
    // pipeline (dependency sort + per-module compile) and land in the registry
    // before any user module resolves `use core.<name>.{...}` against them.
    // Each source's own `module core.<name>` declaration derives its module id,
    // so there is no special-casing in name resolution or the type checker.
    for src in crate::stdlib::core_sources() {
        match parse_stdlib_source(&src, &mut source_map, sink) {
            Ok(pf) => parsed_files.push(pf),
            Err(()) => found_errors = true,
        }
    }

    for file_path in &files {
        match parse_file(file_path, &mut source_map, sink) {
            Ok(pf) => parsed_files.push(pf),
            Err(()) => found_errors = true,
        }
    }

    if found_errors {
        return Ok((CheckOutcome::Failed, files.len()));
    }

    // ── Phase 2: Build dependency graph ───────────────────────────────────────
    let mut dep_graph = DepGraph::new();
    let mut id_to_index: HashMap<String, usize> = HashMap::new();

    // The embedded core (`is_stdlib`) module ids. Every user module implicitly
    // depends on them so the §18.2 prelude can seed core-defined symbols
    // (`Ordering`, `Comparable`, `Into`, …) even when a user module does not
    // `use` them: the implicit edges force the core modules to register before
    // any user module in topo order, so `seed_prelude` always finds them.
    let core_module_ids: Vec<String> = parsed_files
        .iter()
        .enumerate()
        .filter(|(_, pf)| pf.is_stdlib)
        .map(|(i, pf)| dep_graph::module_id_from_module(&pf.module, i))
        .collect();

    for (i, pf) in parsed_files.iter().enumerate() {
        let module_id = dep_graph::module_id_from_module(&pf.module, i);
        let mut deps = dep_graph::extract_dependencies(&pf.module.imports);
        // User modules implicitly depend on every embedded core module (the
        // prelude). Core modules themselves keep only their own edges so they
        // cannot form a prelude self-cycle.
        if !pf.is_stdlib {
            dep_graph::add_prelude_deps(&mut deps, &module_id, &core_module_ids);
        }
        dep_graph.add_module_with_deps(module_id.clone(), deps);
        id_to_index.insert(module_id, i);
    }

    // ── Phase 3: Topological sort + cycle detection ───────────────────────────
    let topo_order = match dep_graph.topological_order() {
        Some(order) => order,
        None => {
            report_module_cycle(&dep_graph, &id_to_index, &parsed_files, &source_map, sink);
            return Ok((CheckOutcome::Failed, files.len()));
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
            let to_print = diagnostics_to_surface(&all_diagnostics, pf.is_stdlib);
            sink.emit(&to_print, &pf.filename, &source_file.content);
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
        seed_prelude(&mut checker, &registry);
        seed_imports(&mut checker, &pf.module.imports, &registry);
        if options.aspects.runs(Aspect::Types) {
            checker.check_module(&mut air_module);
            collect_diagnostics(&mut all_diagnostics, &checker.diags);
        }

        // The chosen strictness (development by default, production under
        // `--strict`) threads through every strictness-gated pass below.
        // Embedded stdlib modules are always checked at development strictness,
        // regardless of the user's `--strict`: the user's strictness governs
        // the user's code, not trusted internal stdlib sources. This keeps
        // stdlib completeness gaps (e.g. missing `@context`) as warnings —
        // which are then suppressed for stdlib (see `diagnostics_to_surface`) —
        // rather than promoting them to errors that would fail the user's
        // `--strict` check on code they did not author.
        let strictness = if pf.is_stdlib {
            Strictness::Development
        } else {
            options.strictness()
        };

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

        // Stdlib modules surface only errors (compiler defects); their
        // development-mode warnings describe internal code the user did not
        // write, so they are not leaked into user output. User modules surface
        // every diagnostic.
        let to_print = diagnostics_to_surface(&all_diagnostics, pf.is_stdlib);
        if !to_print.is_empty() {
            sink.emit(&to_print, &pf.filename, &source_file.content);
        }

        if module_has_errors {
            found_errors = true;
        } else {
            // 4f. Register exports for downstream modules
            let exports = collect_exports(module_id, &pf.path, &checker, &air_module);
            registry.register(exports);
        }
    }

    let outcome = if found_errors {
        CheckOutcome::Failed
    } else {
        CheckOutcome::Clean
    };
    Ok((outcome, files.len()))
}

/// A successfully parsed source file, ready for compilation.
struct ParsedFile {
    path: PathBuf,
    filename: String,
    file_id: bock_errors::FileId,
    module: bock_ast::Module,
    /// Whether this file is an embedded core-stdlib source (prepended by the
    /// loader) rather than a user file. Stdlib modules are compiled and
    /// registered exactly like user modules, but their *non-error* diagnostics
    /// (e.g. development-mode context-annotation recommendations) are not
    /// surfaced to the user — they describe internal stdlib code the user did
    /// not write. Stdlib *errors* still surface (they are compiler defects).
    is_stdlib: bool,
}

/// Lex and parse a single file, adding it to the shared [`SourceMap`].
///
/// Returns `Err(())` if the file could not be read or lexing/parsing
/// produced errors (either way already surfaced through `sink`).
fn parse_file(
    path: &Path,
    source_map: &mut SourceMap,
    sink: &mut DiagnosticSink,
) -> Result<ParsedFile, ()> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            // I/O-class failure: keeps the historical stderr line and, in
            // json mode, records a `code: null` entry in the document.
            sink.emit_io_error(&e.to_string(), Some(&path.display().to_string()));
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
        sink.emit(&diags, &filename, &source_file.content);
        return Err(());
    }

    // Parse
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();
    collect_diagnostics(&mut diags, parser.diagnostics());

    if has_errors(&diags) {
        sink.emit(&diags, &filename, &source_file.content);
        return Err(());
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
/// Mirrors [`parse_file`] but takes the source text directly (the embedded
/// stdlib is compiled into the binary, not read from disk) and registers it in
/// the shared [`SourceMap`] under its logical (repo-relative) path so any
/// diagnostic renders against a stable, recognizable location.
///
/// Returns `Err(())` if lexing or parsing produced errors (already surfaced
/// through `sink`). A parse error here is a compiler-internal defect (the
/// embedded sources are fixed at build time), so it surfaces with the logical
/// path for diagnosis.
fn parse_stdlib_source(
    src: &crate::stdlib::StdlibSource,
    source_map: &mut SourceMap,
    sink: &mut DiagnosticSink,
) -> Result<ParsedFile, ()> {
    let filename = src.logical_path.display().to_string();
    let file_id = source_map.add_file(src.logical_path.clone(), src.source.clone());
    let source_file = source_map.get_file(file_id);

    let mut diags: Vec<Diagnostic> = Vec::new();

    // Lex
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    collect_diagnostics(&mut diags, lexer.diagnostics());

    if has_errors(&diags) {
        sink.emit(&diags, &filename, &source_file.content);
        return Err(());
    }

    // Parse
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();
    collect_diagnostics(&mut diags, parser.diagnostics());

    if has_errors(&diags) {
        sink.emit(&diags, &filename, &source_file.content);
        return Err(());
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

/// Check if any diagnostic in the list is an error.
fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

/// Returns the diagnostics that should be surfaced to the user for a module.
///
/// For user modules this is every diagnostic. For embedded core-stdlib modules
/// (`is_stdlib`), only errors are surfaced: stdlib errors are compiler defects
/// and must be visible, but non-error diagnostics (e.g. development-mode
/// context-annotation recommendations) describe internal stdlib code the user
/// did not author and would otherwise be noise on every `bock check`.
fn diagnostics_to_surface(diagnostics: &[Diagnostic], is_stdlib: bool) -> Vec<Diagnostic> {
    if is_stdlib {
        diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .cloned()
            .collect()
    } else {
        diagnostics.to_vec()
    }
}

/// Find one dependency cycle in `graph` and return its participant module ids
/// in order, with the first id repeated at the end to close the loop
/// (e.g. `["a", "b", "a"]`). Returns `None` if the graph is acyclic.
///
/// Uses only `DepGraph`'s public read API (`modules` / `dependencies`) so the
/// cycle can be reconstructed in `bock-cli` without the graph having to track
/// it. Roots and dependencies are visited in sorted order so the reported cycle
/// is deterministic run-to-run.
fn find_module_cycle(graph: &DepGraph) -> Option<Vec<String>> {
    let mut roots: Vec<String> = graph.modules().into_iter().cloned().collect();
    roots.sort();

    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Iterative DFS that records the active path so a back-edge reveals the
    // exact cycle. `stack` holds (node, sorted dependency iterator position).
    for root in &roots {
        if visited.contains(root) {
            continue;
        }
        let mut path: Vec<String> = Vec::new();
        let mut on_path: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Explicit work stack of (node, children, next-child-index).
        let mut stack: Vec<(String, Vec<String>, usize)> = Vec::new();

        let sorted_deps = |node: &str| -> Vec<String> {
            let mut deps: Vec<String> = graph
                .dependencies(node)
                .map(|d| d.iter().cloned().collect())
                .unwrap_or_default();
            deps.sort();
            deps
        };

        stack.push((root.clone(), sorted_deps(root), 0));
        path.push(root.clone());
        on_path.insert(root.clone());

        while let Some((node, children, idx)) = stack.last_mut() {
            if *idx < children.len() {
                let dep = children[*idx].clone();
                *idx += 1;
                if on_path.contains(&dep) {
                    // Back-edge → cycle. Slice the path from `dep` and close it.
                    let start = path.iter().position(|m| m == &dep).unwrap_or(0);
                    let mut cycle: Vec<String> = path[start..].to_vec();
                    cycle.push(dep);
                    return Some(cycle);
                }
                if !visited.contains(&dep) {
                    let deps = sorted_deps(&dep);
                    path.push(dep.clone());
                    on_path.insert(dep.clone());
                    stack.push((dep, deps, 0));
                }
            } else {
                let node = node.clone();
                visited.insert(node.clone());
                on_path.remove(&node);
                path.pop();
                stack.pop();
            }
        }
    }
    None
}

/// Emit a coded, spanned diagnostic for a circular module dependency.
///
/// Replaces a bare `eprintln!("circular module dependency detected")` that
/// carried no code, no span, and did not name the modules in the cycle —
/// useless to an agent trying to repair it (Q-diag-structure-misc (a)). The
/// diagnostic (`E1008`) names every module in the cycle in order, points its
/// primary span at one offending `use` edge, and notes the fix.
fn report_module_cycle(
    graph: &DepGraph,
    id_to_index: &HashMap<String, usize>,
    parsed_files: &[ParsedFile],
    source_map: &SourceMap,
    sink: &mut DiagnosticSink,
) {
    let code = bock_errors::DiagnosticCode {
        prefix: 'E',
        number: 1008,
    };

    let Some(cycle) = find_module_cycle(graph) else {
        // Defensive: topo sort said there is a cycle but we could not locate
        // it. Still emit a coded diagnostic rather than a bare string.
        let mut bag = DiagnosticBag::new();
        bag.error(
            code,
            "circular module dependency detected",
            bock_errors::Span::dummy(),
        );
        let diags: Vec<Diagnostic> = bag.iter().cloned().collect();
        sink.emit_unlocated(&diags);
        return;
    };

    // `cycle` is e.g. ["a", "b", "a"]; the participant list is everything but
    // the repeated closing node.
    let participants = &cycle[..cycle.len().saturating_sub(1)];
    let chain = cycle.join(" -> ");

    // Choose a primary span: an import in some participant module that targets
    // the next module in the cycle, preferring a module we actually parsed.
    let mut primary: Option<(Span, String)> = None;
    for window in cycle.windows(2) {
        let (from, to) = (&window[0], &window[1]);
        if let Some(&idx) = id_to_index.get(from) {
            let pf = &parsed_files[idx];
            for import in &pf.module.imports {
                if &dep_graph::module_path_to_id(&import.path) == to {
                    primary = Some((import.span, pf.filename.clone()));
                    break;
                }
            }
        }
        if primary.is_some() {
            break;
        }
    }

    let mut bag = DiagnosticBag::new();
    let span = primary.as_ref().map_or_else(Span::dummy, |(s, _)| *s);
    let diag = bag.error(code, format!("circular module dependency: {chain}"), span);
    // One consolidated note: the rich renderer (ariadne 0.4) keeps only the
    // last note per report, so fold the participant list and the fix into a
    // single note rather than losing one.
    diag.note(format!(
        "the cycle involves {} module(s): {}. Break it by removing one of the \
         `use` edges, or extract the shared items into a third module that both \
         can import",
        participants.len(),
        participants.join(", ")
    ));

    let diags: Vec<Diagnostic> = bag.iter().cloned().collect();
    match primary {
        Some((_, ref filename)) => {
            // Render against the offending file's source so the span resolves
            // to a real `line:col` (rich) or correct `line:col` (brief).
            if let Some(&idx) = id_to_index
                .values()
                .find(|&&i| &parsed_files[i].filename == filename)
            {
                let source = &source_map.get_file(parsed_files[idx].file_id).content;
                sink.emit(&diags, filename, source);
            } else {
                sink.emit_unlocated(&diags);
            }
        }
        None => {
            // No parsed participant carried the edge (e.g. a cycle entirely in
            // synthetic ids); still emit the coded, named diagnostic.
            sink.emit_unlocated(&diags);
        }
    }
}

/// Render diagnostics without source context (no file/source available),
/// preserving the `severity[code]: message` shape and any notes.
fn render_codeonly(diagnostics: &[Diagnostic]) -> String {
    let mut out = String::new();
    for diag in diagnostics {
        let severity = match diag.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        };
        out.push_str(&format!("{severity}[{}]: {}\n", diag.code, diag.message));
        for note in &diag.notes {
            out.push_str(&format!("  note: {note}\n"));
        }
    }
    out.trim_end().to_string()
}

/// Print diagnostics, optionally with source context.
fn print_diagnostics(diagnostics: &[Diagnostic], filename: &str, source: &str, context: bool) {
    if context {
        let rendered = bock_errors::render(diagnostics, filename, source);
        eprint!("{rendered}");
    } else {
        // Simple one-line-per-diagnostic format without source context.
        // Locations render as `file:line:col` — the same `line:col` the rich
        // mode and the conformance `error E<code> at <line>:<col>` directive
        // use — not raw byte offsets (Q-diag-brief-span-format, §20.1.1).
        for diag in diagnostics {
            let severity = match diag.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "info",
                Severity::Hint => "hint",
            };
            let (line, col) = byte_to_line_col(source, diag.span.start);
            eprintln!(
                "{severity}[{}]: {} (at {filename}:{line}:{col})",
                diag.code, diag.message
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
    fn brief_diagnostic_renders_line_col_not_byte_offsets() {
        // Q-diag-brief-span-format: brief mode must print `file:line:col`,
        // matching rich mode and the conformance directive — never byte
        // offsets like `129..134`. Capturing eprintln is awkward, so assert
        // on the format building blocks directly.
        let source = "module m\nfn main() -> Void {\n  bad\n}\n";
        // Byte offset of `bad` (line 3): "module m\n"=9, "fn main() -> Void {\n"=20.
        let bad_offset = source.find("bad").unwrap();
        let (line, col) = byte_to_line_col(source, bad_offset);
        assert_eq!((line, col), (3, 3));
        let rendered = format!("error[E4002]: undefined variable `bad` (at m.bock:{line}:{col})");
        assert!(rendered.contains("m.bock:3:3"), "{rendered}");
        assert!(
            !rendered.contains(".."),
            "must not contain a byte range: {rendered}"
        );
    }

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

    // ── Circular module dependency (Q-diag-structure-misc (a)) ─────────────

    #[test]
    fn find_module_cycle_reports_participants_in_order() {
        use std::collections::HashSet;
        let mut g = DepGraph::new();
        g.add_module_with_deps("a".into(), HashSet::from(["b".to_string()]));
        g.add_module_with_deps("b".into(), HashSet::from(["c".to_string()]));
        g.add_module_with_deps("c".into(), HashSet::from(["a".to_string()]));
        let cycle = find_module_cycle(&g).expect("a 3-module cycle must be found");
        // Closed loop: first == last, and every participant appears.
        assert_eq!(cycle.first(), cycle.last());
        for m in ["a", "b", "c"] {
            assert!(
                cycle.contains(&m.to_string()),
                "cycle missing {m}: {cycle:?}"
            );
        }
    }

    #[test]
    fn find_module_cycle_none_for_acyclic_graph() {
        use std::collections::HashSet;
        let mut g = DepGraph::new();
        g.add_module_with_deps("app".into(), HashSet::from(["lib".to_string()]));
        g.add_module("lib".into());
        assert!(find_module_cycle(&g).is_none());
    }

    #[test]
    fn run_circular_module_dependency_returns_failed() {
        // Two mutually-importing modules form a cycle; `bock check` must fail
        // (the coded E1008 diagnostic is verified by manual integration).
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bock");
        let b = dir.path().join("b.bock");
        fs::write(
            &a,
            "module a\nuse b.{ fromB }\npublic fn fromA() -> Int { 1 }\n",
        )
        .unwrap();
        fs::write(
            &b,
            "module b\nuse a.{ fromA }\npublic fn fromB() -> Int { 2 }\n",
        )
        .unwrap();
        let outcome = run(vec![a, b], &CheckOptions::default()).unwrap();
        assert_eq!(outcome, CheckOutcome::Failed);
    }

    #[test]
    fn check_outcome_is_clean_helper() {
        assert!(CheckOutcome::Clean.is_clean());
        assert!(!CheckOutcome::Failed.is_clean());
    }

    // ── --format json (machine output) ─────────────────────────────────────

    #[test]
    fn run_json_format_preserves_outcomes() {
        // The format only changes where output goes; the outcome (and thus
        // the exit code mapped in `main`) is identical to human mode.
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ok.bock");
        fs::write(&ok, "fn add(a: Int, b: Int) -> Int { a + b }\n").unwrap();
        let bad = dir.path().join("bad.bock");
        fs::write(&bad, "fn { broken\n").unwrap();

        let options = CheckOptions {
            format: OutputFormat::Json,
            ..Default::default()
        };
        assert_eq!(run(vec![ok], &options).unwrap(), CheckOutcome::Clean);
        assert_eq!(run(vec![bad], &options).unwrap(), CheckOutcome::Failed);
    }

    #[test]
    fn check_document_counts_by_severity() {
        let diagnostics = vec![
            serde_json::json!({"severity": "error"}),
            serde_json::json!({"severity": "error"}),
            serde_json::json!({"severity": "warning"}),
        ];
        let doc = check_document(CheckOutcome::Failed, 3, &diagnostics);
        assert_eq!(doc["format_version"], FORMAT_VERSION);
        assert_eq!(doc["command"], "check");
        assert_eq!(doc["outcome"], "failed");
        assert_eq!(doc["summary"]["files"], 3);
        assert_eq!(doc["summary"]["errors"], 2);
        assert_eq!(doc["summary"]["warnings"], 1);
        assert_eq!(doc["diagnostics"].as_array().unwrap().len(), 3);

        let clean = check_document(CheckOutcome::Clean, 1, &[]);
        assert_eq!(clean["outcome"], "clean");
        assert!(clean["diagnostics"].as_array().unwrap().is_empty());
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
            format: OutputFormat::Human,
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
            format: OutputFormat::Human,
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
            format: OutputFormat::Human,
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
            format: OutputFormat::Human,
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
            format: OutputFormat::Human,
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
            format: OutputFormat::Human,
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
            format: OutputFormat::Human,
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
            format: OutputFormat::Human,
        };
        assert_eq!(
            run(vec![path], &options).unwrap(),
            CheckOutcome::Clean,
            "--only=types must not run context completeness validation even under --strict"
        );
    }
}
