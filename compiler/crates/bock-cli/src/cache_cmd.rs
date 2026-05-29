//! Implementation of `bock cache` (stats / clear).
//!
//! Covers three on-disk caches under `.bock/`:
//! * `ai-cache/` — content-addressed AI responses (`--decisions` absent).
//! * `decisions/{build,runtime}/` — the decision manifest trees.
//! * `rules/` — the local codegen rule cache.
//!
//! `bock cache clear` wipes the AI cache by default; flags scope the
//! clear to a different subsystem.

use std::fs;
use std::path::Path;

use bock_ai::{AiCache, ManifestScope, ManifestWriter, RuleCache};

use crate::decision_io::find_project_root;

/// Human-readable summary of everything the `bock cache` command can clear.
pub fn run_stats() -> anyhow::Result<()> {
    let project_root = find_project_root()?;

    // AI cache
    let ai_cache = AiCache::new(&project_root);
    let ai_stats = ai_cache
        .stats()
        .map_err(|e| anyhow::anyhow!("could not stat ai cache: {e}"))?;
    println!("AI response cache ({}):", ai_cache.root().display());
    println!("  entries: {}", ai_stats.entries);
    println!("  size:    {}", format_bytes(ai_stats.total_bytes));

    // Decision manifests
    let writer = ManifestWriter::new(&project_root);
    let build = writer.read_build().unwrap_or_default();
    let runtime = writer.read_runtime().unwrap_or_default();
    println!();
    println!("Decision manifests:");
    println!("  build:    {:>4} entries", build.len());
    println!("  runtime:  {:>4} entries", runtime.len());

    // Rule cache
    let rule_cache = RuleCache::new(&project_root);
    let total_rules = count_rule_files(rule_cache.root()).unwrap_or(0);
    println!();
    println!("Rule cache ({}):", rule_cache.root().display());
    println!("  entries: {total_rules}");

    Ok(())
}

/// Options for `bock cache clear`.
#[derive(Debug, Clone, Default)]
pub struct ClearOptions {
    /// Clear the decision manifests instead of the AI cache.
    pub decisions: bool,
    /// Scope the decision clear to runtime only.
    pub runtime: bool,
    /// Scope the decision clear to build only.
    pub build: bool,
    /// Clear the local rule cache instead of the AI cache.
    pub rules: bool,
}

/// Entry point for `bock cache clear`.
pub fn run_clear(options: &ClearOptions) -> anyhow::Result<()> {
    validate_clear_options(options)?;
    let project_root = find_project_root()?;

    if options.decisions {
        return clear_decisions(&project_root, options);
    }
    if options.rules {
        return clear_rules(&project_root);
    }
    clear_ai(&project_root)
}

fn validate_clear_options(options: &ClearOptions) -> anyhow::Result<()> {
    let flag_count = [options.decisions, options.rules]
        .iter()
        .filter(|b| **b)
        .count();
    if flag_count > 1 {
        anyhow::bail!("bock cache clear: pass at most one of --decisions or --rules");
    }
    if (options.runtime || options.build) && !options.decisions {
        anyhow::bail!("bock cache clear: --runtime and --build only apply with --decisions");
    }
    if options.runtime && options.build {
        anyhow::bail!("bock cache clear: --runtime and --build are mutually exclusive");
    }
    Ok(())
}

// ── Internals ────────────────────────────────────────────────────────────────

fn clear_ai(project_root: &Path) -> anyhow::Result<()> {
    let cache = AiCache::new(project_root);
    cache
        .clear()
        .map_err(|e| anyhow::anyhow!("could not clear ai cache: {e}"))?;
    println!("Cleared AI response cache at {}", cache.root().display());
    Ok(())
}

fn clear_decisions(project_root: &Path, options: &ClearOptions) -> anyhow::Result<()> {
    let decisions_root = project_root.join(".bock").join("decisions");
    let mut cleared: Vec<&'static str> = Vec::new();

    let clear_build = !options.runtime;
    let clear_runtime = !options.build;

    if clear_build {
        let dir = decisions_root.join(ManifestScope::Build.dir_name());
        if dir.exists() {
            fs::remove_dir_all(&dir)
                .map_err(|e| anyhow::anyhow!("could not remove {}: {e}", dir.display()))?;
        }
        cleared.push("build");
    }
    if clear_runtime {
        let dir = decisions_root.join(ManifestScope::Runtime.dir_name());
        if dir.exists() {
            fs::remove_dir_all(&dir)
                .map_err(|e| anyhow::anyhow!("could not remove {}: {e}", dir.display()))?;
        }
        cleared.push("runtime");
    }
    println!("Cleared decision manifests: {}", cleared.join(" + "));
    Ok(())
}

fn clear_rules(project_root: &Path) -> anyhow::Result<()> {
    let cache = RuleCache::new(project_root);
    let root = cache.root();
    if root.exists() {
        fs::remove_dir_all(root)
            .map_err(|e| anyhow::anyhow!("could not remove {}: {e}", root.display()))?;
    }
    println!("Cleared rule cache at {}", root.display());
    Ok(())
}

fn count_rule_files(root: &Path) -> std::io::Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let mut n = 0;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            for sub in fs::read_dir(entry.path())? {
                let sub = sub?;
                if sub.path().extension().and_then(|e| e.to_str()) == Some("json") {
                    n += 1;
                }
            }
        }
    }
    Ok(n)
}

fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.2} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_ai::{Decision, DecisionType};
    use chrono::{DateTime, Utc};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn touch_project(root: &Path) {
        fs::write(root.join("bock.project"), "[project]\nname = \"t\"\n").unwrap();
    }

    fn decision(id: &str, dt: DecisionType) -> Decision {
        Decision {
            id: id.into(),
            module: PathBuf::from("src/a.bock"),
            target: Some("js".into()),
            decision_type: dt,
            choice: "x".into(),
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
    fn clear_decisions_only_runtime_preserves_build() {
        let dir = tempdir().unwrap();
        touch_project(dir.path());
        let mut w = ManifestWriter::new(dir.path());
        w.record(decision("b", DecisionType::Codegen));
        w.record(decision("r", DecisionType::AdaptiveRecovery));
        w.flush().unwrap();

        let opts = ClearOptions {
            decisions: true,
            runtime: true,
            build: false,
            rules: false,
        };
        // Exercise the clear helper directly without relying on cwd.
        clear_decisions(dir.path(), &opts).unwrap();

        let w = ManifestWriter::new(dir.path());
        assert!(w.read_runtime().unwrap().is_empty());
        assert_eq!(w.read_build().unwrap().len(), 1);
    }

    #[test]
    fn clear_decisions_both_by_default() {
        let dir = tempdir().unwrap();
        touch_project(dir.path());
        let mut w = ManifestWriter::new(dir.path());
        w.record(decision("b", DecisionType::Codegen));
        w.record(decision("r", DecisionType::AdaptiveRecovery));
        w.flush().unwrap();

        let opts = ClearOptions {
            decisions: true,
            runtime: false,
            build: false,
            rules: false,
        };
        clear_decisions(dir.path(), &opts).unwrap();

        let w = ManifestWriter::new(dir.path());
        assert!(w.read_build().unwrap().is_empty());
        assert!(w.read_runtime().unwrap().is_empty());
    }

    #[test]
    fn clear_ai_wipes_cache_directory() {
        let dir = tempdir().unwrap();
        let cache = AiCache::new(dir.path());
        cache.put(&"req".to_string(), &"resp".to_string()).unwrap();
        assert!(cache.stats().unwrap().entries > 0);
        clear_ai(dir.path()).unwrap();
        assert_eq!(cache.stats().unwrap().entries, 0);
    }

    #[test]
    fn clear_rules_removes_rule_root() {
        let dir = tempdir().unwrap();
        let cache = RuleCache::new(dir.path());
        fs::create_dir_all(cache.target_dir("js")).unwrap();
        fs::write(cache.target_dir("js").join("r.json"), b"{}").unwrap();
        clear_rules(dir.path()).unwrap();
        assert!(!cache.root().exists());
    }

    #[test]
    fn clear_rejects_runtime_without_decisions_flag() {
        let err = validate_clear_options(&ClearOptions {
            runtime: true,
            ..Default::default()
        })
        .unwrap_err();
        assert!(format!("{err}").contains("--runtime"));
    }

    #[test]
    fn clear_rejects_decisions_and_rules_together() {
        let err = validate_clear_options(&ClearOptions {
            decisions: true,
            rules: true,
            ..Default::default()
        })
        .unwrap_err();
        assert!(format!("{err}").contains("at most one"));
    }
}
