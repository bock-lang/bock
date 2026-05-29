//! Implementation of the `bock promote` command.
//!
//! Analyzes the current project at the *next* strictness level (sketch →
//! development → production) and reports issues that would prevent a clean
//! check there. With `--apply`, a narrow set of simple fixes is applied in
//! place and the `[strictness]` default in `bock.project` is bumped if the
//! project checks clean afterward.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;

use bock_air::{
    lower_module, resolve_names_with_registry, AIRNode, ModuleRegistry, NodeIdGen, NodeKind,
    SymbolTable,
};
use bock_ast::Visibility;
use bock_build::dep_graph::{self, DepGraph};
use bock_errors::{DiagnosticBag, Severity, Span};
use bock_lexer::Lexer;
use bock_lsp::format_type;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{collect_exports, seed_imports, Strictness, Type, TypeChecker};

use crate::check::{discover_bock_files, register_type_builtins};

/// Options controlling how `bock promote` behaves.
pub struct PromoteOptions {
    /// When `true`, apply safe automatic fixes and bump the project's
    /// strictness level on success. When `false` (the default / `--check`),
    /// report issues only.
    pub apply: bool,
}

/// Entry point for the `bock promote` command.
pub fn run(options: &PromoteOptions) -> anyhow::Result<()> {
    let project_root = find_project_root()?;
    let project_path = project_root.join("bock.project");

    let current = read_strictness(&project_path)?;
    let next = match next_level(current) {
        Some(n) => n,
        None => {
            println!("Project is already at `production` strictness; nothing to promote.");
            return Ok(());
        }
    };

    println!("Promoting: {} → {}", level_name(current), level_name(next));
    println!();

    let files = discover_files(&project_root)?;
    if files.is_empty() {
        eprintln!("No .bock files found under {}.", project_root.display());
        process::exit(1);
    }

    let issues = analyze(&files, next)?;

    if issues.is_empty() {
        println!("No issues found at `{}` strictness.", level_name(next));
        if options.apply {
            update_strictness(&project_path, next)?;
            println!("Updated [strictness] default to `{}`.", level_name(next));
        } else {
            println!("Run `bock promote --apply` to promote.");
        }
        return Ok(());
    }

    report_issues(&issues);

    if options.apply {
        let fixed = apply_fixes(&issues)?;
        println!();
        println!("Applied {fixed} automatic fix(es).");

        // Re-analyze after fixes. If clean, update the project file; otherwise
        // tell the user what still needs manual attention.
        let remaining = analyze(&files, next)?;
        if remaining.is_empty() {
            update_strictness(&project_path, next)?;
            println!("Updated [strictness] default to `{}`.", level_name(next));
        } else {
            println!();
            println!(
                "{} issue(s) still need manual fixing before promotion.",
                remaining.len()
            );
            report_issues(&remaining);
            process::exit(1);
        }
    } else {
        println!();
        println!(
            "{} issue(s) found. Fix these, then run `bock promote --apply`.",
            issues.len()
        );
        // `--check` is advisory; don't fail the process just for reporting.
    }

    Ok(())
}

// ─── Issue model ──────────────────────────────────────────────────────────────

/// A single promotion blocker with optional automatic fix.
#[derive(Debug, Clone)]
struct Issue {
    /// Path to the source file (relative to cwd where possible).
    file: String,
    line: usize,
    column: usize,
    message: String,
    fix: Option<Fix>,
}

/// A byte-precise source rewrite that promotes an issue-free signature.
///
/// Only the narrow set of rewrites that `--apply` understands is represented
/// here — complex changes are reported as un-fixable and left to the user.
#[derive(Debug, Clone)]
struct Fix {
    file: PathBuf,
    /// Byte offset at which to insert `text`.
    position: usize,
    /// Text to insert. Callers pre-format trailing whitespace as needed.
    text: String,
    /// When multiple fixes target the same offset, fixes insert from highest
    /// order-key to lowest. A lower key means the text ends up *closer* to the
    /// original character at `position`. For `fn foo(…) {` we want
    /// `-> Ret with Eff { ` which means the return-type text must precede the
    /// with-clause text, so return-type has a lower order-key.
    order: u8,
}

// ─── Strictness plumbing ──────────────────────────────────────────────────────

fn level_name(s: Strictness) -> &'static str {
    match s {
        Strictness::Sketch => "sketch",
        Strictness::Development => "development",
        Strictness::Production => "production",
    }
}

fn parse_level(s: &str) -> Option<Strictness> {
    match s {
        "sketch" => Some(Strictness::Sketch),
        "development" => Some(Strictness::Development),
        "production" => Some(Strictness::Production),
        _ => None,
    }
}

fn next_level(current: Strictness) -> Option<Strictness> {
    match current {
        Strictness::Sketch => Some(Strictness::Development),
        Strictness::Development => Some(Strictness::Production),
        Strictness::Production => None,
    }
}

/// Parse the `[strictness] default = "<level>"` field from `bock.project`.
/// Defaults to `sketch` if the key is missing.
fn read_strictness(path: &Path) -> anyhow::Result<Strictness> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))?;
    let mut in_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == "[strictness]";
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("default") {
            let rest = rest.trim_start().trim_start_matches('=').trim();
            let value = rest.trim_matches('"');
            if let Some(level) = parse_level(value) {
                return Ok(level);
            }
        }
    }
    Ok(Strictness::Sketch)
}

/// Rewrite (or insert) the `[strictness] default` field to `new_level`.
fn update_strictness(path: &Path, new_level: Strictness) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))?;
    let new_value = level_name(new_level);

    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let mut in_section = false;
    let mut replaced = false;
    let mut section_start: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == "[strictness]";
            if in_section {
                section_start = Some(i);
            }
            continue;
        }
        if in_section && trimmed.starts_with("default") {
            // Overwrite the line while keeping any leading indentation.
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            lines[i] = format!("{indent}default = \"{new_value}\"");
            replaced = true;
            break;
        }
    }

    if !replaced {
        match section_start {
            Some(idx) => {
                lines.insert(idx + 1, format!("default = \"{new_value}\""));
            }
            None => {
                if !lines.last().map(|l| l.is_empty()).unwrap_or(true) {
                    lines.push(String::new());
                }
                lines.push("[strictness]".to_string());
                lines.push(format!("default = \"{new_value}\""));
            }
        }
    }

    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(path, out)
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", path.display()))?;
    Ok(())
}

// ─── Project discovery ────────────────────────────────────────────────────────

fn find_project_root() -> anyhow::Result<PathBuf> {
    let mut cur = std::env::current_dir()?;
    loop {
        if cur.join("bock.project").is_file() {
            return Ok(cur);
        }
        if !cur.pop() {
            anyhow::bail!("no `bock.project` found in current directory or any parent");
        }
    }
}

fn discover_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let path = root.to_string_lossy();
    discover_bock_files(&path)
}

// ─── Analysis ─────────────────────────────────────────────────────────────────

/// Run the compile pipeline for every file, collecting issues that would be
/// diagnosed at `target_strictness`.
fn analyze(files: &[PathBuf], target_strictness: Strictness) -> anyhow::Result<Vec<Issue>> {
    let mut source_map = SourceMap::new();
    let mut parsed_files: Vec<ParsedFile> = Vec::new();

    for file_path in files {
        let pf = parse_file(file_path, &mut source_map)?;
        parsed_files.push(pf);
    }

    let mut dep_graph = DepGraph::new();
    let mut id_to_index: HashMap<String, usize> = HashMap::new();
    for (i, pf) in parsed_files.iter().enumerate() {
        let module_id = dep_graph::module_id_from_module(&pf.module, i);
        let deps = dep_graph::extract_dependencies(&pf.module.imports);
        dep_graph.add_module_with_deps(module_id.clone(), deps);
        id_to_index.insert(module_id, i);
    }

    let topo_order = match dep_graph.topological_order() {
        Some(order) => order,
        None => anyhow::bail!("circular module dependency detected"),
    };

    let mut registry = ModuleRegistry::new();
    let mut issues: Vec<Issue> = Vec::new();

    for module_id in &topo_order {
        let Some(&idx) = id_to_index.get(module_id) else {
            continue;
        };
        let pf = &parsed_files[idx];
        let source_file = source_map.get_file(pf.file_id);

        let mut symbols = SymbolTable::new();
        let _resolve_diags = resolve_names_with_registry(&pf.module, &mut symbols, &registry);

        let id_gen = NodeIdGen::new();
        let mut air_module = lower_module(&pf.module, &id_gen, &symbols);

        let mut checker = TypeChecker::new();
        register_type_builtins(&mut checker);
        seed_imports(&mut checker, &pf.module.imports, &registry);
        checker.check_module(&mut air_module);

        // Effect + capability diagnostics at the *target* strictness.
        let effect_diags = bock_types::track_effects(&air_module, target_strictness);
        let cap_diags = bock_types::verify_capabilities(&air_module, target_strictness);

        // Collect structural issues that don't manifest as diagnostics (no
        // return-type annotation on public fns, unresolved inferred types in
        // signatures, etc.). These are the ones `--apply` knows how to fix.
        collect_signature_issues(
            &air_module,
            &checker,
            &pf.path,
            &source_file.content,
            target_strictness,
            &mut issues,
        );

        // Surface effect/capability diagnostics as issues too (read-only;
        // users must declare these manually).
        append_diag_issues(&effect_diags, &pf.path, &source_file.content, &mut issues);
        append_diag_issues(&cap_diags, &pf.path, &source_file.content, &mut issues);

        // Fresh `with` clauses are offered as fixes: walk the module again to
        // pair each undeclared-effect diagnostic with the inferred effect set.
        augment_effect_fixes(&air_module, &pf.path, target_strictness, &mut issues);

        if !has_errors_in(&effect_diags) && !has_errors_in(&cap_diags) {
            let exports = collect_exports(module_id, &pf.path, &checker, &air_module);
            registry.register(exports);
        }
    }

    issues.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.column.cmp(&b.column))
    });
    Ok(issues)
}

fn has_errors_in(bag: &DiagnosticBag) -> bool {
    bag.iter().any(|d| d.severity == Severity::Error)
}

fn append_diag_issues(bag: &DiagnosticBag, path: &Path, source: &str, issues: &mut Vec<Issue>) {
    for diag in bag.iter() {
        let (line, column) = line_col(source, diag.span.start);
        issues.push(Issue {
            file: display_path(path),
            line,
            column,
            message: diag.message.clone(),
            fix: None,
        });
    }
}

/// Walk the AIR module and flag signatures that will fail at `target`.
fn collect_signature_issues(
    module: &AIRNode,
    checker: &TypeChecker,
    path: &Path,
    source: &str,
    target: Strictness,
    issues: &mut Vec<Issue>,
) {
    let items = match &module.kind {
        NodeKind::Module { items, .. } => items.as_slice(),
        _ => std::slice::from_ref(module),
    };
    for item in items {
        collect_item_issues(item, checker, path, source, target, issues);
    }
}

fn collect_item_issues(
    node: &AIRNode,
    checker: &TypeChecker,
    path: &Path,
    source: &str,
    target: Strictness,
    issues: &mut Vec<Issue>,
) {
    match &node.kind {
        NodeKind::FnDecl { .. } => {
            collect_fn_issues(node, checker, path, source, target, issues);
        }
        NodeKind::ImplBlock { methods, .. } | NodeKind::TraitDecl { methods, .. } => {
            for m in methods {
                collect_item_issues(m, checker, path, source, target, issues);
            }
        }
        NodeKind::ClassDecl { methods, .. } => {
            for m in methods {
                collect_item_issues(m, checker, path, source, target, issues);
            }
        }
        _ => {}
    }
}

fn collect_fn_issues(
    node: &AIRNode,
    checker: &TypeChecker,
    path: &Path,
    source: &str,
    target: Strictness,
    issues: &mut Vec<Issue>,
) {
    let NodeKind::FnDecl {
        name,
        visibility,
        params,
        return_type,
        body,
        ..
    } = &node.kind
    else {
        return;
    };

    let is_public = matches!(visibility, Visibility::Public);
    let should_check = match target {
        Strictness::Development => is_public,
        Strictness::Production => true,
        Strictness::Sketch => false,
    };
    if !should_check {
        return;
    }

    let (line, column) = line_col(source, node.span.start);

    // Missing explicit return type annotation.
    if return_type.is_none() {
        let inferred = infer_return_type(checker, body);
        let (message, fix) = match inferred {
            Some(ref ty) if is_printable_type(ty) => {
                let rendered = format_type(ty);
                (
                    format!(
                        "fn {}(…) — needs explicit return type annotation (inferred `{rendered}`)",
                        name.name
                    ),
                    Some(Fix {
                        file: path.to_path_buf(),
                        position: body.span.start,
                        text: format!("-> {rendered} "),
                        order: 0,
                    }),
                )
            }
            _ => (
                format!(
                    "fn {}(…) — needs explicit return type annotation",
                    name.name
                ),
                None,
            ),
        };
        issues.push(Issue {
            file: display_path(path),
            line,
            column,
            message,
            fix,
        });
    }

    // Parameters without a type annotation.
    for p in params {
        if let NodeKind::Param {
            ty: None, pattern, ..
        } = &p.kind
        {
            let pname = param_bind_name(pattern).unwrap_or_else(|| "_".to_string());
            let (pline, pcol) = line_col(source, p.span.start);
            issues.push(Issue {
                file: display_path(path),
                line: pline,
                column: pcol,
                message: format!(
                    "fn {}: parameter `{pname}` — missing type annotation",
                    name.name
                ),
                fix: None,
            });
        }
    }

    // Inferred parameter or return types that never resolved (Flexible /
    // unresolved TypeVar). These are the moral equivalent of `Flexible`
    // inference results in sketch mode.
    if let Some(ty) = checker.type_of(node.id) {
        flag_unresolved(ty, path, source, node.span, &name.name, issues);
    }
}

fn infer_return_type(checker: &TypeChecker, body: &AIRNode) -> Option<Type> {
    // With no `-> T` annotation, the checker unifies the body against `Void`,
    // so `checker.type_of(body.id)` always reports Void. The tail expression
    // of the block carries the real inferred type.
    if let NodeKind::Block {
        tail: Some(tail_expr),
        ..
    } = &body.kind
    {
        if let Some(ty) = checker.type_of(tail_expr.id) {
            return Some(ty.clone());
        }
    }
    checker.type_of(body.id).cloned()
}

/// Returns `true` if `ty` can be serialized to Bock surface syntax as an
/// annotation (i.e. it's fully resolved and doesn't contain `<error>` /
/// `<flexible>` / inference variables).
fn is_printable_type(ty: &Type) -> bool {
    match ty {
        Type::Primitive(_) => true,
        Type::Named(_) => true,
        Type::Generic(g) => g.args.iter().all(is_printable_type),
        Type::Tuple(elems) => elems.iter().all(is_printable_type),
        Type::Optional(inner) => is_printable_type(inner),
        Type::Result(ok, err) => is_printable_type(ok) && is_printable_type(err),
        Type::Function(f) => f.params.iter().all(is_printable_type) && is_printable_type(&f.ret),
        Type::Refined(base, _) => is_printable_type(base),
        Type::TypeVar(_) | Type::Flexible(_) | Type::Error => false,
    }
}

fn flag_unresolved(
    ty: &Type,
    path: &Path,
    source: &str,
    fn_span: Span,
    fn_name: &str,
    issues: &mut Vec<Issue>,
) {
    if contains_flexible(ty) {
        let (line, column) = line_col(source, fn_span.start);
        issues.push(Issue {
            file: display_path(path),
            line,
            column,
            message: format!(
                "fn {fn_name}: Flexible/unresolved type inferred — add explicit annotations"
            ),
            fix: None,
        });
    }
}

fn contains_flexible(ty: &Type) -> bool {
    match ty {
        Type::Flexible(_) => true,
        Type::Generic(g) => g.args.iter().any(contains_flexible),
        Type::Tuple(elems) => elems.iter().any(contains_flexible),
        Type::Optional(inner) => contains_flexible(inner),
        Type::Result(ok, err) => contains_flexible(ok) || contains_flexible(err),
        Type::Function(f) => f.params.iter().any(contains_flexible) || contains_flexible(&f.ret),
        Type::Refined(base, _) => contains_flexible(base),
        _ => false,
    }
}

fn param_bind_name(pattern: &AIRNode) -> Option<String> {
    match &pattern.kind {
        NodeKind::BindPat { name, .. } => Some(name.name.clone()),
        _ => None,
    }
}

/// Walk the module and, for every function with undeclared effects at
/// `target` strictness, build a `with`-clause insertion fix. Attaches one
/// fix per function to the first matching undeclared-effect issue.
fn augment_effect_fixes(module: &AIRNode, path: &Path, target: Strictness, issues: &mut [Issue]) {
    let mut fixes: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    collect_effect_fixes(module, target, &mut fixes);

    let file_key = display_path(path);
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for issue in issues.iter_mut() {
        if issue.fix.is_some() || issue.file != file_key {
            continue;
        }
        let matched = fixes.iter().find(|(fn_name, _)| {
            if used.contains(*fn_name) {
                return false;
            }
            // Direct use: "function `X` uses effect ..."
            // Propagated: "function `X` calls `Y` which requires effect ..."
            let prefix = format!("function `{fn_name}` ");
            issue.message.starts_with(&prefix)
        });
        let Some((fn_name, (position, effects))) = matched else {
            continue;
        };
        let clause = format!("with {} ", effects.join(", "));
        issue.fix = Some(Fix {
            file: path.to_path_buf(),
            position: *position,
            text: clause,
            order: 1,
        });
        used.insert(fn_name.clone());
    }
}

fn collect_effect_fixes(
    module: &AIRNode,
    target: Strictness,
    out: &mut HashMap<String, (usize, Vec<String>)>,
) {
    // Two-pass walk:
    //   1. Build `declared`: fn_name → declared effect names.
    //   2. For each FnDecl, compute direct + propagated effects and subtract
    //      declared → missing. Record body.span.start for the fix position.
    let mut declared_map: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    collect_declared(module, &mut declared_map);

    compute_missing(module, target, &declared_map, out);
}

fn collect_declared(node: &AIRNode, out: &mut HashMap<String, std::collections::HashSet<String>>) {
    match &node.kind {
        NodeKind::Module { items, .. } => {
            for item in items {
                collect_declared(item, out);
            }
        }
        NodeKind::ImplBlock { methods, .. } | NodeKind::TraitDecl { methods, .. } => {
            for m in methods {
                collect_declared(m, out);
            }
        }
        NodeKind::ClassDecl { methods, .. } => {
            for m in methods {
                collect_declared(m, out);
            }
        }
        NodeKind::FnDecl {
            name,
            effect_clause,
            ..
        } => {
            let set: std::collections::HashSet<String> = effect_clause
                .iter()
                .map(|tp| {
                    tp.segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(".")
                })
                .collect();
            out.insert(name.name.clone(), set);
        }
        _ => {}
    }
}

fn compute_missing(
    node: &AIRNode,
    target: Strictness,
    declared_map: &HashMap<String, std::collections::HashSet<String>>,
    out: &mut HashMap<String, (usize, Vec<String>)>,
) {
    match &node.kind {
        NodeKind::Module { items, .. } => {
            for item in items {
                compute_missing(item, target, declared_map, out);
            }
        }
        NodeKind::ImplBlock { methods, .. } | NodeKind::TraitDecl { methods, .. } => {
            for m in methods {
                compute_missing(m, target, declared_map, out);
            }
        }
        NodeKind::ClassDecl { methods, .. } => {
            for m in methods {
                compute_missing(m, target, declared_map, out);
            }
        }
        NodeKind::FnDecl {
            name,
            visibility,
            body,
            ..
        } => {
            let is_public = matches!(visibility, Visibility::Public);
            let should_check = match target {
                Strictness::Development => is_public,
                Strictness::Production => true,
                Strictness::Sketch => false,
            };
            if !should_check {
                return;
            }

            let declared = declared_map.get(&name.name).cloned().unwrap_or_default();

            // Direct effects used in the body.
            let direct = bock_types::infer_effects(node);

            // Propagated effects: walk the body for called function names and
            // union their declared effects.
            let mut callees: std::collections::HashSet<String> = std::collections::HashSet::new();
            collect_callees(body, &mut callees);
            let mut propagated: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for callee in &callees {
                if let Some(eff) = declared_map.get(callee) {
                    for e in eff {
                        propagated.insert(e.clone());
                    }
                }
            }

            let mut missing: Vec<String> = direct
                .iter()
                .map(|e| e.name.clone())
                .chain(propagated)
                .filter(|e| !declared.contains(e))
                .filter(|e| !matches!(e.as_str(), "Panic" | "Allocate" | "Pure"))
                .collect::<std::collections::HashSet<String>>()
                .into_iter()
                .collect();
            missing.sort();
            if !missing.is_empty() {
                out.insert(name.name.clone(), (body.span.start, missing));
            }
        }
        _ => {}
    }
}

fn collect_callees(node: &AIRNode, out: &mut std::collections::HashSet<String>) {
    use bock_air::Visitor;
    struct V<'a> {
        out: &'a mut std::collections::HashSet<String>,
    }
    impl<'a> Visitor for V<'a> {
        fn visit_node(&mut self, node: &AIRNode) {
            if let NodeKind::Call { callee, .. } = &node.kind {
                if let NodeKind::Identifier { name, .. } = &callee.kind {
                    self.out.insert(name.name.clone());
                }
            }
            bock_air::visitor::walk_node(self, node);
        }
    }
    let mut v = V { out };
    v.visit_node(node);
}

// ─── Reporting ────────────────────────────────────────────────────────────────

fn report_issues(issues: &[Issue]) {
    let mut current_file: Option<&str> = None;
    for issue in issues {
        if current_file != Some(issue.file.as_str()) {
            if current_file.is_some() {
                println!();
            }
            println!("{}:", issue.file);
            current_file = Some(issue.file.as_str());
        }
        println!("  line {}: {}", issue.line, issue.message);
    }
}

// ─── Applying fixes ───────────────────────────────────────────────────────────

/// Apply all fixes in `issues`. Returns the number of fixes applied.
///
/// Multiple fixes in the same file are coalesced and applied from the largest
/// offset to the smallest so earlier edits don't shift later offsets.
fn apply_fixes(issues: &[Issue]) -> anyhow::Result<usize> {
    let mut by_file: HashMap<PathBuf, Vec<&Fix>> = HashMap::new();
    for issue in issues {
        if let Some(fix) = &issue.fix {
            by_file.entry(fix.file.clone()).or_default().push(fix);
        }
    }

    let mut total = 0usize;
    for (file, mut fixes) in by_file {
        // Descending position so earlier edits don't shift later offsets.
        // Within the same position, descending `order` so that `insert_str`
        // (which pushes existing content right) ends with low-order fixes
        // closest to `position` in the final text.
        fixes.sort_by(|a, b| {
            b.position
                .cmp(&a.position)
                .then_with(|| b.order.cmp(&a.order))
        });
        let mut content = std::fs::read_to_string(&file)
            .map_err(|e| anyhow::anyhow!("could not read {}: {e}", file.display()))?;
        for fix in fixes {
            if fix.position > content.len() {
                continue;
            }
            content.insert_str(fix.position, &fix.text);
            total += 1;
        }
        std::fs::write(&file, content)
            .map_err(|e| anyhow::anyhow!("could not write {}: {e}", file.display()))?;
    }
    Ok(total)
}

// ─── Parse helper ─────────────────────────────────────────────────────────────

struct ParsedFile {
    path: PathBuf,
    file_id: bock_errors::FileId,
    module: bock_ast::Module,
}

fn parse_file(path: &Path, source_map: &mut SourceMap) -> anyhow::Result<ParsedFile> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))?;
    let file_id = source_map.add_file(path.to_path_buf(), content);
    let source_file = source_map.get_file(file_id);
    let mut lexer = Lexer::new(source_file);
    let tokens = lexer.tokenize();
    if lexer
        .diagnostics()
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        anyhow::bail!("lex errors in {}", path.display());
    }
    let mut parser = Parser::new(tokens, source_file);
    let module = parser.parse_module();
    if parser
        .diagnostics()
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        anyhow::bail!("parse errors in {}", path.display());
    }
    Ok(ParsedFile {
        path: path.to_path_buf(),
        file_id,
        module,
    })
}

// ─── Span / path helpers ──────────────────────────────────────────────────────

fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, c) in source.char_indices() {
        if i >= offset {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn display_path(path: &Path) -> String {
    let cwd = std::env::current_dir().ok();
    if let Some(cwd) = cwd {
        if let Ok(rel) = path.strip_prefix(&cwd) {
            return rel.display().to_string();
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn read_strictness_defaults_to_sketch() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bock.project");
        fs::write(&p, "[project]\nname = \"t\"\n").unwrap();
        assert_eq!(read_strictness(&p).unwrap(), Strictness::Sketch);
    }

    #[test]
    fn read_strictness_parses_explicit_level() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bock.project");
        fs::write(
            &p,
            "[project]\nname = \"t\"\n\n[strictness]\ndefault = \"development\"\n",
        )
        .unwrap();
        assert_eq!(read_strictness(&p).unwrap(), Strictness::Development);
    }

    #[test]
    fn update_strictness_rewrites_existing_default() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bock.project");
        fs::write(
            &p,
            "[project]\nname = \"t\"\n\n[strictness]\ndefault = \"sketch\"\n",
        )
        .unwrap();
        update_strictness(&p, Strictness::Development).unwrap();
        let content = fs::read_to_string(&p).unwrap();
        assert!(content.contains("default = \"development\""));
        assert!(!content.contains("default = \"sketch\""));
    }

    #[test]
    fn update_strictness_inserts_missing_section() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bock.project");
        fs::write(&p, "[project]\nname = \"t\"\n").unwrap();
        update_strictness(&p, Strictness::Production).unwrap();
        let content = fs::read_to_string(&p).unwrap();
        assert!(content.contains("[strictness]"));
        assert!(content.contains("default = \"production\""));
    }

    #[test]
    fn next_level_steps_through_ladder() {
        assert_eq!(
            next_level(Strictness::Sketch),
            Some(Strictness::Development)
        );
        assert_eq!(
            next_level(Strictness::Development),
            Some(Strictness::Production)
        );
        assert_eq!(next_level(Strictness::Production), None);
    }

    #[test]
    fn line_col_counts_from_one() {
        let src = "abc\ndef\nghij";
        assert_eq!(line_col(src, 0), (1, 1));
        assert_eq!(line_col(src, 4), (2, 1));
        assert_eq!(line_col(src, 10), (3, 3));
    }
}
