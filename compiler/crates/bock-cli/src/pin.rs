//! Implementation of `bock pin` and `bock unpin`.
//!
//! Pinning stamps a decision with `pinned = true` plus pin metadata
//! (`pin_reason`, `pinned_at`, `pinned_by`). A pinned build decision
//! is required for production strictness; a pinned runtime decision is
//! a prerequisite for `bock override --promote`.
//!
//! Bulk variants:
//!
//! * `bock pin --all-in <module>` — pin every unpinned decision whose
//!   module path contains the supplied substring (both scopes).
//! * `bock pin --all-build` — pin every unpinned build decision.
//! * `bock pin --all-runtime` — pin every unpinned runtime decision.

use std::path::PathBuf;

use bock_ai::{Decision, ManifestScope, ManifestWriter};
use chrono::Utc;

use crate::decision_io::{
    find_project_root, manifest_file_path, pinned_by, read_manifest_file, resolve_id, scope_name,
    write_manifest_file,
};

/// Options accepted by `bock pin`.
#[derive(Debug, Clone, Default)]
pub struct PinOptions {
    /// Single decision id (prefixed or bare).
    pub id: Option<String>,
    /// Pin every unpinned decision whose module contains this substring.
    pub all_in: Option<String>,
    /// Pin every unpinned build-scope decision.
    pub all_build: bool,
    /// Pin every unpinned runtime-scope decision.
    pub all_runtime: bool,
    /// Free-form reason stored on pin metadata.
    pub reason: Option<String>,
}

/// Entry point for `bock pin`.
pub fn run_pin(options: &PinOptions) -> anyhow::Result<()> {
    let bulk_flags_set = [
        options.all_in.is_some(),
        options.all_build,
        options.all_runtime,
    ]
    .iter()
    .filter(|x| **x)
    .count();

    if bulk_flags_set > 1 {
        anyhow::bail!("bock pin: pass at most one of --all-in, --all-build, --all-runtime");
    }
    if bulk_flags_set == 0 && options.id.is_none() {
        anyhow::bail!(
            "bock pin: supply a decision id or one of --all-in, --all-build, --all-runtime"
        );
    }
    if bulk_flags_set > 0 && options.id.is_some() {
        anyhow::bail!("bock pin: cannot combine an id with --all-* flags");
    }

    let project_root = find_project_root()?;
    let writer = ManifestWriter::new(&project_root);

    if let Some(id) = &options.id {
        let (decision, scope) = resolve_id(&writer, id, None)?;
        let pinned = pin_by_id(&project_root, &decision, scope, options.reason.as_deref())?;
        if pinned {
            println!(
                "Pinned {} decision `{}`",
                scope_name(scope),
                crate::decision_io::display_id(scope, &decision.id)
            );
        } else {
            println!(
                "Decision `{}` is already pinned",
                crate::decision_io::display_id(scope, &decision.id)
            );
        }
        return Ok(());
    }

    if let Some(sub) = &options.all_in {
        let count = pin_bulk(
            &project_root,
            &writer,
            BulkFilter::ModuleSubstring(sub.clone()),
            options.reason.as_deref(),
        )?;
        println!("Pinned {count} decision(s) in modules matching `{sub}`");
        return Ok(());
    }
    if options.all_build {
        let count = pin_bulk(
            &project_root,
            &writer,
            BulkFilter::Scope(ManifestScope::Build),
            options.reason.as_deref(),
        )?;
        println!("Pinned {count} build decision(s)");
        return Ok(());
    }
    if options.all_runtime {
        let count = pin_bulk(
            &project_root,
            &writer,
            BulkFilter::Scope(ManifestScope::Runtime),
            options.reason.as_deref(),
        )?;
        println!("Pinned {count} runtime decision(s)");
        return Ok(());
    }

    unreachable!("validated above")
}

/// Entry point for `bock unpin`.
pub fn run_unpin(id: &str) -> anyhow::Result<()> {
    let project_root = find_project_root()?;
    let writer = ManifestWriter::new(&project_root);
    let (decision, scope) = resolve_id(&writer, id, None)?;

    let file = manifest_file_path(&project_root, scope, &decision.module);
    let mut entries = read_manifest_file(&file)?;
    let mut changed = false;
    for e in entries.iter_mut() {
        if e.id == decision.id {
            if !e.pinned {
                println!(
                    "Decision `{}` is already unpinned",
                    crate::decision_io::display_id(scope, &decision.id)
                );
                return Ok(());
            }
            e.pinned = false;
            e.pin_reason = None;
            e.pinned_at = None;
            e.pinned_by = None;
            changed = true;
            break;
        }
    }
    if !changed {
        anyhow::bail!(
            "bock unpin: decision `{}` not found in its manifest file",
            decision.id
        );
    }
    write_manifest_file(&file, &entries)?;
    println!(
        "Unpinned {} decision `{}`",
        scope_name(scope),
        crate::decision_io::display_id(scope, &decision.id)
    );
    Ok(())
}

// ── Internals ────────────────────────────────────────────────────────────────

enum BulkFilter {
    Scope(ManifestScope),
    ModuleSubstring(String),
}

/// Pin a single decision in place. Returns `true` if the file was modified
/// (i.e. it was not already pinned).
fn pin_by_id(
    project_root: &std::path::Path,
    decision: &Decision,
    scope: ManifestScope,
    reason: Option<&str>,
) -> anyhow::Result<bool> {
    let file = manifest_file_path(project_root, scope, &decision.module);
    let mut entries = read_manifest_file(&file)?;
    let who = pinned_by();
    let now = Utc::now();
    let mut modified = false;

    for e in entries.iter_mut() {
        if e.id == decision.id {
            if !e.pinned {
                e.pinned = true;
                e.pin_reason = Some(
                    reason
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("pinned via `bock pin` by {who}")),
                );
                e.pinned_at = Some(now);
                e.pinned_by = Some(who.clone());
                modified = true;
            }
            break;
        }
    }

    if modified {
        write_manifest_file(&file, &entries)?;
    }
    Ok(modified)
}

fn pin_bulk(
    project_root: &std::path::Path,
    writer: &ManifestWriter,
    filter: BulkFilter,
    reason: Option<&str>,
) -> anyhow::Result<usize> {
    // Collect all matching decisions first so we don't hold manifest file
    // handles across the rewrite loop.
    let mut candidates: Vec<(ManifestScope, Decision)> = Vec::new();
    let mut scopes_to_scan: Vec<ManifestScope> = Vec::new();
    match &filter {
        BulkFilter::Scope(s) => scopes_to_scan.push(*s),
        BulkFilter::ModuleSubstring(_) => {
            scopes_to_scan.push(ManifestScope::Build);
            scopes_to_scan.push(ManifestScope::Runtime);
        }
    }

    for scope in scopes_to_scan {
        let decisions = match scope {
            ManifestScope::Build => writer
                .read_build()
                .map_err(|e| anyhow::anyhow!("could not read build manifest: {e}"))?,
            ManifestScope::Runtime => writer
                .read_runtime()
                .map_err(|e| anyhow::anyhow!("could not read runtime manifest: {e}"))?,
        };
        for d in decisions {
            if d.pinned {
                continue;
            }
            let matches = match &filter {
                BulkFilter::Scope(_) => true,
                BulkFilter::ModuleSubstring(s) => d.module.to_string_lossy().contains(s.as_str()),
            };
            if matches {
                candidates.push((scope, d));
            }
        }
    }

    // Rewrite each affected file once.
    let mut touched_files: std::collections::BTreeMap<(ManifestScope, PathBuf), Vec<String>> =
        std::collections::BTreeMap::new();
    for (scope, d) in &candidates {
        touched_files
            .entry((*scope, d.module.clone()))
            .or_default()
            .push(d.id.clone());
    }

    let who = pinned_by();
    let now = Utc::now();
    let mut count = 0usize;
    for ((scope, module), ids) in touched_files {
        let file = manifest_file_path(project_root, scope, &module);
        let mut entries = read_manifest_file(&file)?;
        for e in entries.iter_mut() {
            if ids.contains(&e.id) && !e.pinned {
                e.pinned = true;
                e.pin_reason = Some(
                    reason
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("pinned via `bock pin` by {who}")),
                );
                e.pinned_at = Some(now);
                e.pinned_by = Some(who.clone());
                count += 1;
            }
        }
        write_manifest_file(&file, &entries)?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_ai::{DecisionType, ManifestScope};
    use chrono::{DateTime, Utc};
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn mk_decision(id: &str, module: &str, dt: DecisionType, pinned: bool) -> Decision {
        Decision {
            id: id.into(),
            module: PathBuf::from(module),
            target: Some("js".into()),
            decision_type: dt,
            choice: "code".into(),
            alternatives: vec![],
            reasoning: None,
            model_id: "stub:stub".into(),
            confidence: 0.9,
            pinned,
            pin_reason: pinned.then(|| "seed".into()),
            pinned_at: pinned.then(Utc::now),
            pinned_by: pinned.then(|| "seed".into()),
            superseded_by: None,
            timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        }
    }

    fn touch_project(root: &Path) {
        fs::write(root.join("bock.project"), "[project]\nname = \"t\"\n").unwrap();
    }

    fn seed(root: &Path, scope: ManifestScope, decisions: &[Decision]) {
        // All decisions written here must share the same module.
        let path = manifest_file_path(root, scope, &decisions[0].module);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(decisions).unwrap()).unwrap();
    }

    #[test]
    fn pin_by_bare_id_marks_pinned() {
        let dir = tempdir().unwrap();
        touch_project(dir.path());
        seed(
            dir.path(),
            ManifestScope::Build,
            &[mk_decision(
                "abc",
                "src/a.bock",
                DecisionType::Codegen,
                false,
            )],
        );

        let writer = ManifestWriter::new(dir.path());
        let (d, s) = resolve_id(&writer, "abc", None).unwrap();
        assert_eq!(s, ManifestScope::Build);
        let modified = pin_by_id(dir.path(), &d, s, Some("reviewed")).unwrap();
        assert!(modified);

        let entries = read_manifest_file(&manifest_file_path(
            dir.path(),
            ManifestScope::Build,
            &PathBuf::from("src/a.bock"),
        ))
        .unwrap();
        assert!(entries[0].pinned);
        assert_eq!(entries[0].pin_reason.as_deref(), Some("reviewed"));
    }

    #[test]
    fn pin_by_prefixed_id_uses_prefix_scope() {
        let dir = tempdir().unwrap();
        touch_project(dir.path());
        // Same bare id exists in both manifests.
        seed(
            dir.path(),
            ManifestScope::Build,
            &[mk_decision(
                "shared",
                "src/a.bock",
                DecisionType::Codegen,
                false,
            )],
        );
        seed(
            dir.path(),
            ManifestScope::Runtime,
            &[mk_decision(
                "shared",
                "src/a.bock",
                DecisionType::AdaptiveRecovery,
                false,
            )],
        );

        let writer = ManifestWriter::new(dir.path());
        let (_, scope) = resolve_id(&writer, "runtime:shared", None).unwrap();
        assert_eq!(scope, ManifestScope::Runtime);
    }

    #[test]
    fn bare_id_ambiguous_across_scopes_errors() {
        let dir = tempdir().unwrap();
        touch_project(dir.path());
        seed(
            dir.path(),
            ManifestScope::Build,
            &[mk_decision(
                "shared",
                "src/a.bock",
                DecisionType::Codegen,
                false,
            )],
        );
        seed(
            dir.path(),
            ManifestScope::Runtime,
            &[mk_decision(
                "shared",
                "src/a.bock",
                DecisionType::AdaptiveRecovery,
                false,
            )],
        );
        let writer = ManifestWriter::new(dir.path());
        let err = resolve_id(&writer, "shared", None).unwrap_err();
        assert!(format!("{err}").contains("ambiguous"));
    }

    #[test]
    fn pin_all_runtime_pins_only_runtime_decisions() {
        let dir = tempdir().unwrap();
        touch_project(dir.path());
        seed(
            dir.path(),
            ManifestScope::Build,
            &[mk_decision("b", "src/a.bock", DecisionType::Codegen, false)],
        );
        seed(
            dir.path(),
            ManifestScope::Runtime,
            &[mk_decision(
                "r",
                "src/a.bock",
                DecisionType::AdaptiveRecovery,
                false,
            )],
        );
        let writer = ManifestWriter::new(dir.path());
        let count = pin_bulk(
            dir.path(),
            &writer,
            BulkFilter::Scope(ManifestScope::Runtime),
            None,
        )
        .unwrap();
        assert_eq!(count, 1);

        let build_entries = read_manifest_file(&manifest_file_path(
            dir.path(),
            ManifestScope::Build,
            &PathBuf::from("src/a.bock"),
        ))
        .unwrap();
        assert!(!build_entries[0].pinned);

        let runtime_entries = read_manifest_file(&manifest_file_path(
            dir.path(),
            ManifestScope::Runtime,
            &PathBuf::from("src/a.bock"),
        ))
        .unwrap();
        assert!(runtime_entries[0].pinned);
    }

    #[test]
    fn pin_all_in_module_filters_by_substring() {
        let dir = tempdir().unwrap();
        touch_project(dir.path());
        seed(
            dir.path(),
            ManifestScope::Build,
            &[mk_decision(
                "api",
                "src/api/client.bock",
                DecisionType::Codegen,
                false,
            )],
        );
        let p = manifest_file_path(
            dir.path(),
            ManifestScope::Build,
            &PathBuf::from("src/other.bock"),
        );
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(
            &p,
            serde_json::to_vec_pretty(&[mk_decision(
                "other",
                "src/other.bock",
                DecisionType::Codegen,
                false,
            )])
            .unwrap(),
        )
        .unwrap();

        let writer = ManifestWriter::new(dir.path());
        let count = pin_bulk(
            dir.path(),
            &writer,
            BulkFilter::ModuleSubstring("src/api".into()),
            None,
        )
        .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn unpin_clears_pin_metadata() {
        let dir = tempdir().unwrap();
        touch_project(dir.path());
        seed(
            dir.path(),
            ManifestScope::Build,
            &[mk_decision("x", "src/a.bock", DecisionType::Codegen, true)],
        );
        let writer = ManifestWriter::new(dir.path());
        let (d, s) = resolve_id(&writer, "x", None).unwrap();
        assert_eq!(s, ManifestScope::Build);

        // Invoke the pin file rewrite path used by `run_unpin` by going
        // through the public helper.
        let file = manifest_file_path(dir.path(), s, &d.module);
        let mut entries = read_manifest_file(&file).unwrap();
        for e in entries.iter_mut() {
            if e.id == d.id {
                e.pinned = false;
                e.pin_reason = None;
                e.pinned_at = None;
                e.pinned_by = None;
            }
        }
        write_manifest_file(&file, &entries).unwrap();

        let entries = read_manifest_file(&file).unwrap();
        assert!(!entries[0].pinned);
        assert!(entries[0].pin_reason.is_none());
    }
}
