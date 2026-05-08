//! Implementation of the `bock inspect` command group.
//!
//! Four sub-behaviours, all rooted in the project's `.bock/` tree:
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

use std::path::Path;

use bock_ai::{AiCache, Decision, DecisionType, ManifestScope, ManifestWriter, Rule, RuleCache};

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
}
