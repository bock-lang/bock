//! Implementation of the `bock inspect` command group.
//!
//! Five sub-behaviours. The first four are rooted in the project's `.bock/`
//! tree; the fifth is compiler introspection on a single source file:
//!
//! * `bock inspect [decisions]` — list decisions with scope filters
//!   (`--runtime`, `--all`), pin filter (`--unpinned`), module/type
//!   filters, and `--json` machine output.
//! * `bock inspect decision <id>` — show one decision in detail; accepts
//!   prefixed (`build:abc`, `runtime:def`) or bare ids.
//! * `bock inspect cache` — cache entry counts and (with `--size`) byte
//!   totals for `.bock/ai-cache/`.
//! * `bock inspect rules` — list learned codegen rules, optionally
//!   filtered by target.
//! * `bock inspect air <file>` — dump the lowered AIR tree for one file,
//!   human-readable by default, machine-readable with `--json`. See
//!   [`run_air`] for the JSON contract.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bock_ai::{AiCache, Decision, DecisionType, ManifestScope, ManifestWriter, Rule, RuleCache};
use bock_air::visitor::{walk_node, Visitor};
use bock_air::{
    lower_module, resolve_names_with_registry, AIRNode, ModuleRegistry, NodeIdGen, NodeKind,
    SymbolTable,
};
use bock_build::dep_graph::{self, DepGraph};
use bock_errors::{Diagnostic, FileId, Severity};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::{SourceFile, SourceMap};
use bock_types::{collect_exports, seed_imports, seed_prelude, TypeChecker};

use crate::check::CheckOutcome;
use crate::decision_io::{display_id, find_project_root, scope_name};

/// Which scopes to show when listing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeFilter {
    /// Build manifest only.
    Build,
    /// Runtime manifest only.
    Runtime,
    /// Both manifests, with prefixed ids in output.
    All,
}

/// Options for `bock inspect [decisions]`.
#[derive(Debug, Clone)]
pub struct InspectDecisionsOptions {
    /// Scope to list.
    pub scope: ScopeFilter,
    /// Show only decisions that are not yet pinned.
    pub unpinned_only: bool,
    /// Filter by module path substring.
    pub module_filter: Option<String>,
    /// Filter by decision-type name (e.g. `"codegen"`, `"repair"`).
    pub type_filter: Option<String>,
    /// Emit JSON instead of the human table.
    pub json: bool,
}

/// Entry point for `bock inspect decisions`.
pub fn run_decisions(options: &InspectDecisionsOptions) -> anyhow::Result<()> {
    let project_root = find_project_root()?;
    let writer = ManifestWriter::new(&project_root);

    let mut rows: Vec<(ManifestScope, Decision)> = Vec::new();
    if matches!(options.scope, ScopeFilter::Build | ScopeFilter::All) {
        for d in writer
            .read_build()
            .map_err(|e| anyhow::anyhow!("could not read build manifest: {e}"))?
        {
            rows.push((ManifestScope::Build, d));
        }
    }
    if matches!(options.scope, ScopeFilter::Runtime | ScopeFilter::All) {
        for d in writer
            .read_runtime()
            .map_err(|e| anyhow::anyhow!("could not read runtime manifest: {e}"))?
        {
            rows.push((ManifestScope::Runtime, d));
        }
    }

    if options.unpinned_only {
        rows.retain(|(_, d)| !d.pinned);
    }
    if let Some(filter) = &options.module_filter {
        rows.retain(|(_, d)| d.module.to_string_lossy().contains(filter.as_str()));
    }
    if let Some(filter) = &options.type_filter {
        rows.retain(|(_, d)| decision_type_name(d.decision_type) == filter.as_str());
    }

    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.module.cmp(&b.1.module))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });

    if options.json {
        print_decisions_json(&rows)?;
    } else {
        print_decisions_table(&rows, options.scope);
    }
    Ok(())
}

/// Entry point for `bock inspect decision <id>`.
pub fn run_decision(id: &str, json: bool) -> anyhow::Result<()> {
    let project_root = find_project_root()?;
    let writer = ManifestWriter::new(&project_root);
    let (decision, scope) = crate::decision_io::resolve_id(&writer, id, None)?;

    if json {
        let wrapped = serde_json::json!({
            "scope": scope_name(scope),
            "decision": decision,
        });
        println!("{}", serde_json::to_string_pretty(&wrapped)?);
    } else {
        print_decision_detail(scope, &decision);
    }
    Ok(())
}

/// Entry point for `bock inspect cache`.
pub fn run_cache(show_size: bool) -> anyhow::Result<()> {
    let project_root = find_project_root()?;
    let cache = AiCache::new(&project_root);
    let stats = cache
        .stats()
        .map_err(|e| anyhow::anyhow!("could not stat cache: {e}"))?;

    println!(
        "{}  AI cache at {}",
        color("·", ANSI_DIM),
        cache.root().display()
    );
    println!("  entries: {}", stats.entries);
    if show_size || stats.entries > 0 {
        println!("  size:    {}", format_bytes(stats.total_bytes));
    }
    Ok(())
}

/// Entry point for `bock inspect rules`.
pub fn run_rules(target_filter: Option<&str>) -> anyhow::Result<()> {
    let project_root = find_project_root()?;
    let cache = RuleCache::new(&project_root);
    let root = cache.root();

    let mut rules: Vec<Rule> = Vec::new();
    if root.exists() {
        let targets = list_target_dirs(root, target_filter)?;
        for t in &targets {
            rules.extend(
                cache
                    .load_for_target(t)
                    .map_err(|e| anyhow::anyhow!("could not load rules for {t}: {e}"))?,
            );
        }
    }

    if rules.is_empty() {
        match target_filter {
            Some(t) => println!("no rules found for target `{t}`"),
            None => println!("no rules found in {}", root.display()),
        }
        return Ok(());
    }

    rules.sort_by(|a, b| {
        a.target_id
            .cmp(&b.target_id)
            .then_with(|| a.node_kind.cmp(&b.node_kind))
            .then_with(|| b.priority.cmp(&a.priority))
    });

    println!(
        "{:<10} {:<18} {:<10} {:>4} {:>4} ID",
        "TARGET", "NODE_KIND", "PROV", "PRI", "PIN"
    );
    for r in &rules {
        println!(
            "{:<10} {:<18} {:<10} {:>4} {:>4} {}",
            r.target_id,
            r.node_kind,
            provenance_label(r.provenance),
            r.priority,
            if r.pinned { "yes" } else { "no" },
            short(&r.id),
        );
    }
    Ok(())
}

// ── Human-readable output ────────────────────────────────────────────────────

fn print_decisions_table(rows: &[(ManifestScope, Decision)], scope: ScopeFilter) {
    if rows.is_empty() {
        let name = match scope {
            ScopeFilter::Build => "build",
            ScopeFilter::Runtime => "runtime",
            ScopeFilter::All => "any scope",
        };
        println!("no decisions found ({name})");
        return;
    }

    let show_scope_col = matches!(scope, ScopeFilter::All);

    if show_scope_col {
        println!(
            "{:<8} {:<12} {:<5} {:<5} {:<32} ID",
            "SCOPE", "TYPE", "PIN", "CONF", "MODULE"
        );
    } else {
        println!(
            "{:<12} {:<5} {:<5} {:<32} ID",
            "TYPE", "PIN", "CONF", "MODULE"
        );
    }

    for (s, d) in rows {
        let type_name = decision_type_name(d.decision_type);
        let pin = if d.pinned {
            color("yes", ANSI_GREEN)
        } else {
            color("no", ANSI_YELLOW)
        };
        let conf = format!("{:.2}", d.confidence);
        let module = d.module.display().to_string();
        let id = display_id(*s, &d.id);

        if show_scope_col {
            println!(
                "{:<8} {:<12} {:<5} {:<5} {:<32} {}",
                scope_name(*s),
                type_name,
                pin,
                conf,
                elide(&module, 32),
                id
            );
        } else {
            println!(
                "{:<12} {:<5} {:<5} {:<32} {}",
                type_name,
                pin,
                conf,
                elide(&module, 32),
                id
            );
        }
    }
}

fn print_decision_detail(scope: ManifestScope, d: &Decision) {
    let header = format!(
        "{} {}",
        color(scope_name(scope), ANSI_CYAN),
        display_id(scope, &d.id)
    );
    println!("{header}");
    println!("  {:<14} {}", "type:", decision_type_name(d.decision_type));
    println!("  {:<14} {}", "module:", d.module.display());
    if let Some(t) = &d.target {
        println!("  {:<14} {}", "target:", t);
    }
    println!("  {:<14} {}", "model:", d.model_id);
    println!("  {:<14} {:.3}", "confidence:", d.confidence);
    println!(
        "  {:<14} {}",
        "pinned:",
        if d.pinned {
            color("yes", ANSI_GREEN)
        } else {
            color("no", ANSI_YELLOW)
        }
    );
    if let Some(r) = &d.pin_reason {
        println!("  {:<14} {}", "pin reason:", r);
    }
    if let Some(w) = &d.pinned_by {
        println!("  {:<14} {}", "pinned by:", w);
    }
    if let Some(t) = &d.pinned_at {
        println!("  {:<14} {}", "pinned at:", t.to_rfc3339());
    }
    if let Some(s) = &d.superseded_by {
        println!("  {:<14} {}", "superseded:", s);
    }
    println!("  {:<14} {}", "recorded:", d.timestamp.to_rfc3339());

    println!("  {}", color("choice:", ANSI_BOLD));
    for line in d.choice.lines() {
        println!("    {line}");
    }
    if !d.alternatives.is_empty() {
        println!("  {}", color("alternatives:", ANSI_BOLD));
        for a in &d.alternatives {
            println!("    - {a}");
        }
    }
    if let Some(r) = &d.reasoning {
        println!("  {}", color("reasoning:", ANSI_BOLD));
        for line in r.lines() {
            println!("    {line}");
        }
    }
}

fn print_decisions_json(rows: &[(ManifestScope, Decision)]) -> anyhow::Result<()> {
    let list: Vec<serde_json::Value> = rows
        .iter()
        .map(|(s, d)| {
            serde_json::json!({
                "scope": scope_name(*s),
                "prefixed_id": display_id(*s, &d.id),
                "decision": d,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&list)?);
    Ok(())
}

// ── `bock inspect air` ───────────────────────────────────────────────────────
//
// Machine-readable dump of the lowered AIR (S-AIR) tree for one source file.
// The `--json` shape is a contract: the VS Code extension's AIR tree viewer
// consumes it. Changing field names or meanings is a breaking change to that
// consumer — extend additively only.

/// Entry point for `bock inspect air <file>`.
///
/// Runs the compiler frontend (lex → parse → name resolution → AIR lowering)
/// on a single file and dumps the resulting S-AIR tree. The embedded core
/// stdlib is loaded first, exactly as in `bock check`, so `use core.*`
/// imports resolve the same way they do there. Type checking does NOT run:
/// the v1 contract is structure + spans.
///
/// # JSON contract (`--json`)
///
/// On success, stdout carries a single JSON object — the root `Module` node.
/// Every node has exactly four fields:
///
/// ```json
/// {
///   "kind": "FnDecl",
///   "name": "add",
///   "span": { "start": 14, "end": 53, "line": 3, "col": 1 },
///   "children": []
/// }
/// ```
///
/// * `kind` — the AIR node kind (the `NodeKind` variant name), e.g.
///   `"Module"`, `"FnDecl"`, `"BinaryOp"`.
/// * `name` — the node's source-level name when it has one (declaration
///   names, identifier references, field/method names, dotted module/type
///   paths, literal text), otherwise `null`.
/// * `span` — `start`/`end` are byte offsets into the file (`end` exclusive);
///   `line`/`col` are the 1-based line and column (column counted in
///   characters) of `start`. Compiler-synthesized nodes report `0..0`.
/// * `children` — the node's AIR children in traversal order (may be empty).
///
/// On failure (unreadable file, lex/parse/name-resolution errors), stdout
/// carries a JSON error object instead of a tree, and the command exits
/// non-zero:
///
/// ```json
/// {
///   "error": {
///     "message": "parsing failed",
///     "diagnostics": [
///       {
///         "severity": "error",
///         "code": "E0204",
///         "message": "expected an identifier",
///         "span": { "start": 3, "end": 4, "line": 1, "col": 4 }
///       }
///     ]
///   }
/// }
/// ```
///
/// Consumers distinguish the two by the presence of the top-level `error`
/// key. Without `--json`, the tree renders as an indented human view (one
/// node per line: kind, name, `@line:col`, byte range) and failures render
/// the standard diagnostics to stderr.
pub fn run_air(path: &Path, json: bool) -> anyhow::Result<CheckOutcome> {
    match lower_file_to_air(path) {
        Ok(lowered) => {
            let file = lowered.source_map.get_file(lowered.file_id);
            let tree = build_tree(&lowered.air, file);
            if json {
                println!("{}", serde_json::to_string_pretty(&tree_to_json(&tree))?);
            } else {
                print_tree(&tree, 0);
            }
            Ok(CheckOutcome::Clean)
        }
        Err(failure) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&failure_to_json(&failure))?
                );
            } else {
                print_failure(&failure);
            }
            Ok(CheckOutcome::Failed)
        }
    }
}

/// A successfully lowered file: the AIR root plus the sources backing it
/// (needed for line/column lookups when rendering spans).
struct LoweredAir {
    /// Holds the embedded stdlib sources and the user file.
    source_map: SourceMap,
    /// The user file's id within `source_map`.
    file_id: FileId,
    /// The lowered S-AIR module root (`NodeKind::Module`).
    air: AIRNode,
}

/// A frontend failure: the stage that failed plus its structured diagnostics.
struct AirFailure {
    /// Stage summary, e.g. `"parsing failed"`.
    message: String,
    /// The diagnostics that stopped the pipeline (empty for I/O failures).
    diagnostics: Vec<Diagnostic>,
    /// `(filename, content)` of the file the diagnostics point into;
    /// `None` for failures before any file was read (then `diagnostics` is
    /// empty too).
    file: Option<(String, String)>,
}

impl AirFailure {
    /// A failure with no diagnostics (I/O errors, internal invariants).
    fn from_message(message: impl Into<String>) -> Box<Self> {
        Box::new(Self {
            message: message.into(),
            diagnostics: Vec::new(),
            file: None,
        })
    }

    /// A failure carrying compiler diagnostics for one file.
    fn with_diagnostics(
        message: impl Into<String>,
        diagnostics: Vec<Diagnostic>,
        filename: String,
        content: String,
    ) -> Box<Self> {
        Box::new(Self {
            message: message.into(),
            diagnostics,
            file: Some((filename, content)),
        })
    }
}

/// Run the frontend on `path` up to (and including) AIR lowering.
///
/// Mirrors the phases of `check::run` for a single user file: the embedded
/// core stdlib is parsed into the same [`SourceMap`], the dependency graph
/// gains the implicit prelude edges, and each module is resolved + lowered in
/// topological order with non-user modules registering their exports so the
/// user module's imports resolve. Type checking, ownership/effect analysis,
/// and context validation are intentionally skipped — `bock check` is the
/// diagnostics tool; this is a structure dump.
fn lower_file_to_air(path: &Path) -> Result<LoweredAir, Box<AirFailure>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| AirFailure::from_message(format!("could not read {}: {e}", path.display())))?;

    // ── Phase 1: parse embedded stdlib + the user file ──────────────────────
    let mut source_map = SourceMap::new();
    let mut parsed: Vec<AirParsedFile> = Vec::new();

    for src in crate::stdlib::core_sources() {
        let file_id = source_map.add_file(src.logical_path.clone(), src.source.clone());
        let module = parse_registered_file(&source_map, file_id).map_err(|diags| {
            AirFailure::with_diagnostics(
                format!(
                    "internal error: embedded stdlib module {} failed to parse",
                    src.logical_path.display()
                ),
                diags,
                src.logical_path.display().to_string(),
                src.source.clone(),
            )
        })?;
        parsed.push(AirParsedFile {
            path: src.logical_path.clone(),
            file_id,
            module,
            is_user: false,
        });
    }

    let user_file_id = source_map.add_file(path.to_path_buf(), content);
    let user_module = parse_registered_file(&source_map, user_file_id).map_err(|diags| {
        let f = source_map.get_file(user_file_id);
        AirFailure::with_diagnostics(
            "parsing failed",
            diags,
            f.path.display().to_string(),
            f.content.clone(),
        )
    })?;
    parsed.push(AirParsedFile {
        path: path.to_path_buf(),
        file_id: user_file_id,
        module: user_module,
        is_user: true,
    });

    // ── Phase 2: dependency graph with implicit prelude edges ───────────────
    let mut graph = DepGraph::new();
    let mut id_to_index: HashMap<String, usize> = HashMap::new();
    let core_ids: Vec<String> = parsed
        .iter()
        .enumerate()
        .filter(|(_, pf)| !pf.is_user)
        .map(|(i, pf)| dep_graph::module_id_from_module(&pf.module, i))
        .collect();

    for (i, pf) in parsed.iter().enumerate() {
        let module_id = dep_graph::module_id_from_module(&pf.module, i);
        let mut deps = dep_graph::extract_dependencies(&pf.module.imports);
        if pf.is_user {
            dep_graph::add_prelude_deps(&mut deps, &module_id, &core_ids);
        }
        graph.add_module_with_deps(module_id.clone(), deps);
        id_to_index.insert(module_id, i);
    }

    let topo = graph
        .topological_order()
        .ok_or_else(|| AirFailure::from_message("circular module dependency detected"))?;

    // ── Phase 3: resolve + lower in dependency order ────────────────────────
    let mut registry = ModuleRegistry::new();
    let mut user_air: Option<AIRNode> = None;

    for module_id in &topo {
        let Some(&idx) = id_to_index.get(module_id) else {
            continue; // external dependency — not in our source set
        };
        let pf = &parsed[idx];

        let mut symbols = SymbolTable::new();
        let bag = resolve_names_with_registry(&pf.module, &mut symbols, &registry);
        let diags: Vec<Diagnostic> = bag.iter().cloned().collect();
        if diags.iter().any(|d| d.severity == Severity::Error) {
            let f = source_map.get_file(pf.file_id);
            let message = if pf.is_user {
                "name resolution failed".to_string()
            } else {
                format!(
                    "internal error: embedded stdlib module {} failed name resolution",
                    pf.path.display()
                )
            };
            return Err(AirFailure::with_diagnostics(
                message,
                diags,
                f.path.display().to_string(),
                f.content.clone(),
            ));
        }

        let id_gen = NodeIdGen::new();
        let air = lower_module(&pf.module, &id_gen, &symbols);

        if pf.is_user {
            user_air = Some(air);
        } else {
            // Export collection needs a constructed (but not run) type
            // checker — the same arrangement `check::run` uses when type
            // checking is skipped under an `--only` restriction.
            let mut checker = TypeChecker::new();
            crate::check::register_type_builtins(&mut checker);
            seed_prelude(&mut checker, &registry);
            seed_imports(&mut checker, &pf.module.imports, &registry);
            let exports = collect_exports(module_id, &pf.path, &checker, &air);
            registry.register(exports);
        }
    }

    match user_air {
        Some(air) => Ok(LoweredAir {
            source_map,
            file_id: user_file_id,
            air,
        }),
        None => Err(AirFailure::from_message(
            "internal error: the input module never reached lowering",
        )),
    }
}

/// One parsed file in the inspect-air pipeline (user file or embedded stdlib).
struct AirParsedFile {
    path: PathBuf,
    file_id: FileId,
    module: bock_ast::Module,
    is_user: bool,
}

/// Lex and parse one file already registered in the [`SourceMap`], collecting
/// diagnostics instead of printing them (the `--json` error contract needs
/// them structured; `check::parse_file` prints as it goes).
fn parse_registered_file(
    source_map: &SourceMap,
    file_id: FileId,
) -> Result<bock_ast::Module, Vec<Diagnostic>> {
    let source_file = source_map.get_file(file_id);
    let mut diags: Vec<Diagnostic> = Vec::new();

    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    diags.extend(lexer.diagnostics().iter().cloned());
    if diags.iter().any(|d| d.severity == Severity::Error) {
        return Err(diags);
    }

    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();
    diags.extend(parser.diagnostics().iter().cloned());
    if diags.iter().any(|d| d.severity == Severity::Error) {
        return Err(diags);
    }

    Ok(module)
}

// ── AIR tree projection ──────────────────────────────────────────────────────

/// One node of the serializable AIR tree — the `bock inspect air` output
/// shape. This is a *projection* of [`AIRNode`] (kind + name + span +
/// children), deliberately decoupled from the internal `NodeKind` payloads so
/// the JSON contract stays stable as the AIR evolves.
struct AirTreeNode {
    /// The `NodeKind` variant name, e.g. `"FnDecl"`.
    kind: &'static str,
    /// Source-level name when the node has one (see [`node_name`]).
    name: Option<String>,
    /// Start byte offset (inclusive).
    start: usize,
    /// End byte offset (exclusive).
    end: usize,
    /// 1-based line of `start`.
    line: usize,
    /// 1-based column (in characters) of `start`.
    col: usize,
    /// Children in AIR traversal order.
    children: Vec<AirTreeNode>,
}

/// Builds an [`AirTreeNode`] tree from an AIR root by riding the canonical
/// [`bock_air::visitor`] traversal, so child coverage and order can never
/// drift from the compiler's own definition of the tree.
struct TreeBuilder<'a> {
    file: &'a SourceFile,
    stack: Vec<AirTreeNode>,
    root: Option<AirTreeNode>,
}

impl Visitor for TreeBuilder<'_> {
    fn visit_node(&mut self, node: &AIRNode) {
        self.stack.push(make_entry(self.file, node));
        walk_node(self, node);
        if let Some(done) = self.stack.pop() {
            match self.stack.last_mut() {
                Some(parent) => parent.children.push(done),
                None => self.root = Some(done),
            }
        }
    }
}

/// Project a single [`AIRNode`] (without children) into a tree entry.
fn make_entry(file: &SourceFile, node: &AIRNode) -> AirTreeNode {
    let (line, col) = file.line_col(node.span.start);
    AirTreeNode {
        kind: kind_name(&node.kind),
        name: node_name(&node.kind),
        start: node.span.start,
        end: node.span.end,
        line,
        col,
        children: Vec::new(),
    }
}

/// Project an AIR root into the serializable tree.
fn build_tree(air: &AIRNode, file: &SourceFile) -> AirTreeNode {
    let mut builder = TreeBuilder {
        file,
        stack: Vec::new(),
        root: None,
    };
    builder.visit_node(air);
    // `visit_node` always sets `root` for the outermost node; the childless
    // fallback exists only to avoid an unwrap.
    builder.root.take().unwrap_or_else(|| make_entry(file, air))
}

/// The stable string name of a [`NodeKind`] — the `kind` field of the JSON
/// contract. Always the Rust variant name.
fn kind_name(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Module { .. } => "Module",
        NodeKind::ImportDecl { .. } => "ImportDecl",
        NodeKind::FnDecl { .. } => "FnDecl",
        NodeKind::RecordDecl { .. } => "RecordDecl",
        NodeKind::EnumDecl { .. } => "EnumDecl",
        NodeKind::EnumVariant { .. } => "EnumVariant",
        NodeKind::ClassDecl { .. } => "ClassDecl",
        NodeKind::TraitDecl { .. } => "TraitDecl",
        NodeKind::ImplBlock { .. } => "ImplBlock",
        NodeKind::EffectDecl { .. } => "EffectDecl",
        NodeKind::TypeAlias { .. } => "TypeAlias",
        NodeKind::ConstDecl { .. } => "ConstDecl",
        NodeKind::ModuleHandle { .. } => "ModuleHandle",
        NodeKind::PropertyTest { .. } => "PropertyTest",
        NodeKind::Param { .. } => "Param",
        NodeKind::TypeNamed { .. } => "TypeNamed",
        NodeKind::TypeTuple { .. } => "TypeTuple",
        NodeKind::TypeFunction { .. } => "TypeFunction",
        NodeKind::TypeOptional { .. } => "TypeOptional",
        NodeKind::TypeSelf => "TypeSelf",
        NodeKind::Literal { .. } => "Literal",
        NodeKind::Identifier { .. } => "Identifier",
        NodeKind::BinaryOp { .. } => "BinaryOp",
        NodeKind::UnaryOp { .. } => "UnaryOp",
        NodeKind::Assign { .. } => "Assign",
        NodeKind::Call { .. } => "Call",
        NodeKind::MethodCall { .. } => "MethodCall",
        NodeKind::FieldAccess { .. } => "FieldAccess",
        NodeKind::Index { .. } => "Index",
        NodeKind::Propagate { .. } => "Propagate",
        NodeKind::Lambda { .. } => "Lambda",
        NodeKind::Pipe { .. } => "Pipe",
        NodeKind::Compose { .. } => "Compose",
        NodeKind::Await { .. } => "Await",
        NodeKind::Range { .. } => "Range",
        NodeKind::RecordConstruct { .. } => "RecordConstruct",
        NodeKind::ListLiteral { .. } => "ListLiteral",
        NodeKind::MapLiteral { .. } => "MapLiteral",
        NodeKind::SetLiteral { .. } => "SetLiteral",
        NodeKind::TupleLiteral { .. } => "TupleLiteral",
        NodeKind::Interpolation { .. } => "Interpolation",
        NodeKind::Placeholder => "Placeholder",
        NodeKind::Unreachable => "Unreachable",
        NodeKind::ResultConstruct { .. } => "ResultConstruct",
        NodeKind::If { .. } => "If",
        NodeKind::Guard { .. } => "Guard",
        NodeKind::Match { .. } => "Match",
        NodeKind::MatchArm { .. } => "MatchArm",
        NodeKind::For { .. } => "For",
        NodeKind::While { .. } => "While",
        NodeKind::Loop { .. } => "Loop",
        NodeKind::Block { .. } => "Block",
        NodeKind::Return { .. } => "Return",
        NodeKind::Break { .. } => "Break",
        NodeKind::Continue => "Continue",
        NodeKind::LetBinding { .. } => "LetBinding",
        NodeKind::Move { .. } => "Move",
        NodeKind::Borrow { .. } => "Borrow",
        NodeKind::MutableBorrow { .. } => "MutableBorrow",
        NodeKind::EffectOp { .. } => "EffectOp",
        NodeKind::HandlingBlock { .. } => "HandlingBlock",
        NodeKind::EffectRef { .. } => "EffectRef",
        NodeKind::WildcardPat => "WildcardPat",
        NodeKind::BindPat { .. } => "BindPat",
        NodeKind::LiteralPat { .. } => "LiteralPat",
        NodeKind::ConstructorPat { .. } => "ConstructorPat",
        NodeKind::RecordPat { .. } => "RecordPat",
        NodeKind::TuplePat { .. } => "TuplePat",
        NodeKind::ListPat { .. } => "ListPat",
        NodeKind::OrPat { .. } => "OrPat",
        NodeKind::GuardPat { .. } => "GuardPat",
        NodeKind::RangePat { .. } => "RangePat",
        NodeKind::RestPat => "RestPat",
        NodeKind::Error => "Error",
        // `NodeKind` is #[non_exhaustive]: a variant added upstream surfaces
        // as "Unknown" here until this map learns its name.
        _ => "Unknown",
    }
}

/// The node's source-level name, when it has one: declaration names,
/// identifier references, field/method names, dotted module/type paths, and
/// literal text. `None` for purely structural nodes (blocks, operators, …).
fn node_name(kind: &NodeKind) -> Option<String> {
    match kind {
        NodeKind::Module { path, .. } => path.as_ref().map(module_path_name),
        NodeKind::ImportDecl { path, .. } => Some(module_path_name(path)),
        NodeKind::FnDecl { name, .. }
        | NodeKind::RecordDecl { name, .. }
        | NodeKind::EnumDecl { name, .. }
        | NodeKind::EnumVariant { name, .. }
        | NodeKind::ClassDecl { name, .. }
        | NodeKind::TraitDecl { name, .. }
        | NodeKind::EffectDecl { name, .. }
        | NodeKind::TypeAlias { name, .. }
        | NodeKind::ConstDecl { name, .. }
        | NodeKind::Identifier { name }
        | NodeKind::BindPat { name, .. } => Some(name.name.clone()),
        NodeKind::FieldAccess { field, .. } => Some(field.name.clone()),
        NodeKind::MethodCall { method, .. } => Some(method.name.clone()),
        NodeKind::TypeNamed { path, .. }
        | NodeKind::RecordConstruct { path, .. }
        | NodeKind::ConstructorPat { path, .. }
        | NodeKind::RecordPat { path, .. }
        | NodeKind::EffectRef { path } => Some(type_path_name(path)),
        NodeKind::ModuleHandle { effect, .. } => Some(type_path_name(effect)),
        NodeKind::EffectOp {
            effect, operation, ..
        } => Some(format!("{}.{}", type_path_name(effect), operation.name)),
        NodeKind::PropertyTest { name, .. } => Some(name.clone()),
        NodeKind::Literal { lit } | NodeKind::LiteralPat { lit } => Some(literal_name(lit)),
        _ => None,
    }
}

/// Render a module path (`module Foo.Bar`) as `"Foo.Bar"`.
fn module_path_name(path: &bock_ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// Render a type path (`Std.Io.File`) as `"Std.Io.File"`.
fn type_path_name(path: &bock_ast::TypePath) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// The literal's source-ish text, used as the `name` of literal nodes.
fn literal_name(lit: &bock_ast::Literal) -> String {
    match lit {
        bock_ast::Literal::Int(s) | bock_ast::Literal::Float(s) => s.clone(),
        bock_ast::Literal::Bool(b) => b.to_string(),
        bock_ast::Literal::Char(c) => format!("'{c}'"),
        bock_ast::Literal::String(s) => format!("{s:?}"),
        bock_ast::Literal::Unit => "()".to_string(),
    }
}

// ── AIR output rendering ─────────────────────────────────────────────────────

/// Serialize one tree node to the JSON contract shape (recursive).
fn tree_to_json(node: &AirTreeNode) -> serde_json::Value {
    serde_json::json!({
        "kind": node.kind,
        "name": node.name,
        "span": {
            "start": node.start,
            "end": node.end,
            "line": node.line,
            "col": node.col,
        },
        "children": node.children.iter().map(tree_to_json).collect::<Vec<_>>(),
    })
}

/// Serialize a frontend failure to the JSON error-object contract shape.
fn failure_to_json(failure: &AirFailure) -> serde_json::Value {
    let diagnostics: Vec<serde_json::Value> = failure
        .diagnostics
        .iter()
        .map(|d| {
            let (line, col) = failure
                .file
                .as_ref()
                .map(|(_, content)| line_col_of(content, d.span.start))
                .unwrap_or((1, 1));
            serde_json::json!({
                "severity": severity_name(d.severity),
                "code": d.code.to_string(),
                "message": d.message,
                "span": {
                    "start": d.span.start,
                    "end": d.span.end,
                    "line": line,
                    "col": col,
                },
            })
        })
        .collect();
    serde_json::json!({
        "error": {
            "message": failure.message,
            "diagnostics": diagnostics,
        }
    })
}

/// Print the human-readable indented tree: one node per line with kind, name
/// (when present), `@line:col`, and the byte range.
fn print_tree(node: &AirTreeNode, depth: usize) {
    let indent = "  ".repeat(depth);
    match &node.name {
        Some(name) => println!(
            "{indent}{} {} @{}:{} ({}..{})",
            node.kind, name, node.line, node.col, node.start, node.end
        ),
        None => println!(
            "{indent}{} @{}:{} ({}..{})",
            node.kind, node.line, node.col, node.start, node.end
        ),
    }
    for child in &node.children {
        print_tree(child, depth + 1);
    }
}

/// Render a frontend failure for the human (non-`--json`) mode: the standard
/// diagnostic rendering when we have diagnostics, a plain error line
/// otherwise. Always goes to stderr.
fn print_failure(failure: &AirFailure) {
    match &failure.file {
        Some((filename, content)) if !failure.diagnostics.is_empty() => {
            let rendered = bock_errors::render(&failure.diagnostics, filename, content);
            eprint!("{rendered}");
        }
        _ => eprintln!("error: {}", failure.message),
    }
}

/// Stable lowercase severity names for the JSON error object.
fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
        Severity::Hint => "hint",
    }
}

/// 1-based `(line, col)` of a byte offset, with the column counted in
/// characters — mirrors [`SourceFile::line_col`] for content we hold as a
/// plain string (failure rendering, where no `SourceFile` survives).
fn line_col_of(content: &str, offset: usize) -> (usize, usize) {
    let clamped = offset.min(content.len());
    let prefix = &content[..clamped];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
    let col = prefix[line_start..].chars().count() + 1;
    (line, col)
}

// ── Small helpers ────────────────────────────────────────────────────────────

/// Short string name for a [`DecisionType`].
#[must_use]
pub fn decision_type_name(t: DecisionType) -> &'static str {
    match t {
        DecisionType::Codegen => "codegen",
        DecisionType::Repair => "repair",
        DecisionType::Optimize => "optimize",
        DecisionType::RuleApplied => "rule_applied",
        DecisionType::HandlerChoice => "handler_choice",
        DecisionType::AdaptiveRecovery => "adaptive_recovery",
    }
}

fn provenance_label(p: bock_ai::Provenance) -> &'static str {
    match p {
        bock_ai::Provenance::Builtin => "builtin",
        bock_ai::Provenance::Extracted => "extracted",
        bock_ai::Provenance::Manual => "manual",
    }
}

fn list_target_dirs(root: &Path, target_filter: Option<&str>) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    if let Some(t) = target_filter {
        let p = root.join(t);
        if p.is_dir() {
            out.push(t.to_string());
        }
        return Ok(out);
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    Ok(out)
}

fn short(id: &str) -> String {
    if id.len() <= 12 {
        id.to_string()
    } else {
        format!("{}…", &id[..12])
    }
}

fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(1);
        let mut out: String = s.chars().take(keep).collect();
        out.push('…');
        out
    }
}

fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.2} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

// ── Minimal ANSI colour support (no extra dependency) ────────────────────────

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";

fn color(s: &str, code: &str) -> String {
    if colour_enabled() {
        format!("{code}{s}{ANSI_RESET}")
    } else {
        s.to_string()
    }
}

fn colour_enabled() -> bool {
    use std::io::IsTerminal;
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_ai::Decision;
    use chrono::{DateTime, Utc};
    use std::path::PathBuf;

    fn decision(id: &str) -> Decision {
        Decision {
            id: id.into(),
            module: PathBuf::from("src/x.bock"),
            target: Some("js".into()),
            decision_type: DecisionType::Codegen,
            choice: "code".into(),
            alternatives: vec![],
            reasoning: None,
            model_id: "stub:stub".into(),
            confidence: 1.0,
            pinned: false,
            pin_reason: None,
            pinned_at: None,
            pinned_by: None,
            superseded_by: None,
            timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        }
    }

    #[test]
    fn decision_type_names_are_stable() {
        assert_eq!(decision_type_name(DecisionType::Codegen), "codegen");
        assert_eq!(decision_type_name(DecisionType::Repair), "repair");
        assert_eq!(decision_type_name(DecisionType::Optimize), "optimize");
        assert_eq!(
            decision_type_name(DecisionType::RuleApplied),
            "rule_applied"
        );
        assert_eq!(
            decision_type_name(DecisionType::HandlerChoice),
            "handler_choice"
        );
        assert_eq!(
            decision_type_name(DecisionType::AdaptiveRecovery),
            "adaptive_recovery"
        );
    }

    #[test]
    fn format_bytes_covers_all_scales() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert!(format_bytes(2048).ends_with("KB"));
        assert!(format_bytes(2 * 1024 * 1024).ends_with("MB"));
    }

    #[test]
    fn short_truncates_long_ids() {
        assert_eq!(short("abcdef"), "abcdef");
        let long = "0123456789abcdef0123";
        assert!(short(long).ends_with('…'));
    }

    #[test]
    fn decision_fields_format_without_panic() {
        // Smoke test: exercise the formatter paths on a representative
        // decision. We don't assert on output because ANSI-on-tty varies,
        // but we do ensure no panics for the common shape.
        let d = decision("abc");
        print_decision_detail(ManifestScope::Build, &d);
    }

    // ── inspect air: tree projection ───────────────────────────────────────

    use bock_air::{AIRNode, NodeKind};
    use bock_ast::{BinOp, Ident, Literal};
    use bock_errors::{FileId, Span};

    fn span(start: usize, end: usize) -> Span {
        Span {
            file: FileId(0),
            start,
            end,
        }
    }

    fn ident(name: &str, start: usize, end: usize) -> Ident {
        Ident {
            name: name.to_string(),
            span: span(start, end),
        }
    }

    /// `1 + x` over the source "1 + x\n", as a hand-built AIR fragment.
    fn binary_op_air() -> AIRNode {
        AIRNode::new(
            0,
            span(0, 5),
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(AIRNode::new(
                    1,
                    span(0, 1),
                    NodeKind::Literal {
                        lit: Literal::Int("1".into()),
                    },
                )),
                right: Box::new(AIRNode::new(
                    2,
                    span(4, 5),
                    NodeKind::Identifier {
                        name: ident("x", 4, 5),
                    },
                )),
            },
        )
    }

    fn source_file_for(content: &str) -> (bock_source::SourceMap, bock_errors::FileId) {
        let mut map = bock_source::SourceMap::new();
        let id = map.add_file(PathBuf::from("test.bock"), content.to_string());
        (map, id)
    }

    #[test]
    fn kind_names_are_the_variant_names() {
        assert_eq!(
            kind_name(&NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![],
            }),
            "Module"
        );
        assert_eq!(kind_name(&NodeKind::Continue), "Continue");
        assert_eq!(kind_name(&NodeKind::TypeSelf), "TypeSelf");
        assert_eq!(kind_name(&NodeKind::WildcardPat), "WildcardPat");
        assert_eq!(kind_name(&NodeKind::Error), "Error");
    }

    #[test]
    fn node_names_extract_declaration_and_reference_names() {
        // Identifier reference → its name.
        assert_eq!(
            node_name(&NodeKind::Identifier {
                name: ident("x", 0, 1),
            }),
            Some("x".to_string())
        );
        // Module without a declared path → no name.
        assert_eq!(
            node_name(&NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![],
            }),
            None
        );
        // Literals surface their text.
        assert_eq!(
            node_name(&NodeKind::Literal {
                lit: Literal::Int("42".into()),
            }),
            Some("42".to_string())
        );
        assert_eq!(
            node_name(&NodeKind::Literal {
                lit: Literal::String("hi".into()),
            }),
            Some("\"hi\"".to_string())
        );
        // Structural nodes have no name.
        assert_eq!(node_name(&NodeKind::Continue), None);
    }

    #[test]
    fn build_tree_nests_children_in_traversal_order() {
        let (map, id) = source_file_for("1 + x\n");
        let air = binary_op_air();
        let tree = build_tree(&air, map.get_file(id));

        assert_eq!(tree.kind, "BinaryOp");
        assert_eq!(tree.name, None);
        assert_eq!((tree.start, tree.end, tree.line, tree.col), (0, 5, 1, 1));
        assert_eq!(tree.children.len(), 2);
        assert_eq!(tree.children[0].kind, "Literal");
        assert_eq!(tree.children[0].name.as_deref(), Some("1"));
        assert_eq!(tree.children[1].kind, "Identifier");
        assert_eq!(tree.children[1].name.as_deref(), Some("x"));
        assert_eq!(
            (tree.children[1].line, tree.children[1].col),
            (1, 5),
            "identifier starts at column 5 of line 1"
        );
        assert!(tree.children[0].children.is_empty());
    }

    #[test]
    fn tree_to_json_emits_the_contract_fields() {
        let (map, id) = source_file_for("1 + x\n");
        let air = binary_op_air();
        let json = tree_to_json(&build_tree(&air, map.get_file(id)));

        assert_eq!(json["kind"], "BinaryOp");
        assert!(json["name"].is_null());
        assert_eq!(json["span"]["start"], 0);
        assert_eq!(json["span"]["end"], 5);
        assert_eq!(json["span"]["line"], 1);
        assert_eq!(json["span"]["col"], 1);
        let children = json["children"]
            .as_array()
            .expect("children must be an array");
        assert_eq!(children.len(), 2);
        assert_eq!(children[0]["kind"], "Literal");
        assert_eq!(children[0]["name"], "1");
        assert!(children[0]["children"].as_array().unwrap().is_empty());
    }

    #[test]
    fn failure_to_json_emits_the_error_object() {
        let diag = Diagnostic {
            severity: Severity::Error,
            code: bock_errors::DiagnosticCode {
                prefix: 'E',
                number: 204,
            },
            message: "boom".into(),
            span: span(3, 4),
            labels: vec![],
            notes: vec![],
        };
        let failure = AirFailure::with_diagnostics(
            "parsing failed",
            vec![diag],
            "test.bock".into(),
            "fn {\n".into(),
        );
        let json = failure_to_json(&failure);

        assert_eq!(json["error"]["message"], "parsing failed");
        let diags = json["error"]["diagnostics"].as_array().unwrap();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0]["severity"], "error");
        assert_eq!(diags[0]["code"], "E0204");
        assert_eq!(diags[0]["message"], "boom");
        assert_eq!(diags[0]["span"]["start"], 3);
        assert_eq!(diags[0]["span"]["line"], 1);
        assert_eq!(diags[0]["span"]["col"], 4);
        // No tree fields on a failure object.
        assert!(json.get("kind").is_none());
    }

    #[test]
    fn line_col_of_matches_source_file_semantics() {
        let content = "ab\ncd\n";
        assert_eq!(line_col_of(content, 0), (1, 1));
        assert_eq!(line_col_of(content, 2), (1, 3));
        assert_eq!(line_col_of(content, 3), (2, 1));
        assert_eq!(line_col_of(content, 4), (2, 2));
        // Past the end clamps to the end.
        assert_eq!(line_col_of(content, 999), (3, 1));
        // Column counts characters, not bytes.
        assert_eq!(line_col_of("é_x", 3), (1, 3));
    }
}
