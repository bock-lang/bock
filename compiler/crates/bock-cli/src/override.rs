//! Implementation of the `bock override` command.
//!
//! Four behaviours keyed on the flag combination:
//!
//! * `bock override <id>` — pin the named decision in place. `<id>`
//!   is looked up in both scopes; the caller can use `--runtime` to
//!   force runtime lookup or use the `build:`/`runtime:` id prefix.
//! * `bock override <id> <new-choice>` — replace the decision's
//!   `choice` string with the inline argument and auto-pin it.
//! * `bock override <id> --from-file <path>` — replace the decision's
//!   `choice` with the file's contents and auto-pin.
//! * `bock override --promote <id>` — promote a **pinned** runtime
//!   decision to the build manifest per §10.8. The runtime entry is
//!   marked `superseded_by` the build copy and kept for audit.
//!
//! `--reason` may accompany any of the above and is stored on the
//! resulting pin metadata. All operations are file-based: the manifest
//! JSON files are rewritten in place.

use std::fs;
use std::path::{Path, PathBuf};

use bock_ai::{Decision, DecisionType, ManifestScope, ManifestWriter};
use chrono::Utc;

use crate::decision_io::{
    display_id, find_project_root, manifest_file_path, pinned_by, read_manifest_file, resolve_id,
    scope_name, write_manifest_file,
};

/// User-facing options for `bock override`.
#[derive(Debug, Clone, Default)]
pub struct OverrideOptions {
    /// The decision id to pin, replace, or promote.
    pub decision: Option<String>,
    /// When present, replace the decision's `choice` with this inline value.
    pub new_choice: Option<String>,
    /// When present, replace the decision's `choice` with the file's contents.
    pub from_file: Option<PathBuf>,
    /// Treat `decision` as a runtime-scope id when pinning.
    pub runtime: bool,
    /// Promote a pinned runtime decision into the build manifest.
    pub promote: bool,
    /// Optional human-readable reason to record on the pin.
    pub reason: Option<String>,
}

/// Entry point for `bock override`.
///
/// # Errors
/// Returns an `anyhow::Error` on missing decisions, validation failures
/// (e.g. promoting an unpinned decision), or manifest I/O errors.
pub fn run(options: &OverrideOptions) -> anyhow::Result<()> {
    let Some(id) = options.decision.as_deref() else {
        anyhow::bail!("bock override: a decision id is required");
    };

    let project_root = find_project_root()?;
    let writer = ManifestWriter::new(&project_root);

    if options.promote {
        if options.new_choice.is_some() || options.from_file.is_some() {
            anyhow::bail!(
                "bock override: --promote is incompatible with a new choice or --from-file"
            );
        }
        return promote(&project_root, &writer, id, options.reason.as_deref());
    }

    match (options.new_choice.as_deref(), options.from_file.as_deref()) {
        (Some(_), Some(_)) => {
            anyhow::bail!("bock override: cannot combine an inline choice with --from-file")
        }
        (Some(choice), None) => replace_choice(
            &project_root,
            &writer,
            id,
            options.runtime,
            choice.to_string(),
            options.reason.as_deref(),
        ),
        (None, Some(path)) => {
            let body = fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))?;
            replace_choice(
                &project_root,
                &writer,
                id,
                options.runtime,
                body,
                options.reason.as_deref(),
            )
        }
        (None, None) => pin(
            &project_root,
            &writer,
            id,
            options.runtime,
            options.reason.as_deref(),
        ),
    }
}

// ─── Pinning ──────────────────────────────────────────────────────────────────

fn pin(
    project_root: &Path,
    writer: &ManifestWriter,
    id: &str,
    force_runtime: bool,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    let (decision, scope) = resolve_id(writer, id, None)
        .map_err(|e| anyhow::anyhow!("bock override: {e}"))?;
    if force_runtime && scope != ManifestScope::Runtime {
        anyhow::bail!(
            "bock override: `--runtime` was requested but id `{id}` is a {} decision",
            scope_name(scope)
        );
    }

    let file = manifest_file_path(project_root, scope, &decision.module);
    let mut entries = read_manifest_file(&file)?;
    let who = pinned_by();
    let now = Utc::now();
    let mut found = false;
    for entry in entries.iter_mut() {
        if entry.id == decision.id {
            entry.pinned = true;
            entry.pin_reason = Some(
                reason
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("pinned via `bock override` by {who}")),
            );
            entry.pinned_at = Some(now);
            entry.pinned_by = Some(who.clone());
            found = true;
            break;
        }
    }
    if !found {
        anyhow::bail!(
            "bock override: id `{id}` was located in {} but not found in its own file — \
             manifest may be stale",
            scope_name(scope)
        );
    }
    write_manifest_file(&file, &entries)?;

    println!(
        "Pinned {} decision `{}` in {}",
        scope_name(scope),
        display_id(scope, &decision.id),
        file.display()
    );
    Ok(())
}

// ─── Choice replacement (auto-pins) ──────────────────────────────────────────

fn replace_choice(
    project_root: &Path,
    writer: &ManifestWriter,
    id: &str,
    force_runtime: bool,
    new_choice: String,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    let (decision, scope) = resolve_id(writer, id, None)
        .map_err(|e| anyhow::anyhow!("bock override: {e}"))?;
    if force_runtime && scope != ManifestScope::Runtime {
        anyhow::bail!(
            "bock override: `--runtime` was requested but id `{id}` is a {} decision",
            scope_name(scope)
        );
    }

    let file = manifest_file_path(project_root, scope, &decision.module);
    let mut entries = read_manifest_file(&file)?;
    let who = pinned_by();
    let now = Utc::now();
    let mut found = false;
    for entry in entries.iter_mut() {
        if entry.id == decision.id {
            entry.choice = new_choice.clone();
            entry.pinned = true;
            entry.pin_reason = Some(reason.map(str::to_owned).unwrap_or_else(|| {
                format!("override via `bock override <id> <choice>` by {who}")
            }));
            entry.pinned_at = Some(now);
            entry.pinned_by = Some(who.clone());
            found = true;
            break;
        }
    }
    if !found {
        anyhow::bail!(
            "bock override: id `{id}` was located in {} but not found in its own file — \
             manifest may be stale",
            scope_name(scope)
        );
    }
    write_manifest_file(&file, &entries)?;
    println!(
        "Overrode {} decision `{}` with new choice (auto-pinned)",
        scope_name(scope),
        display_id(scope, &decision.id)
    );
    Ok(())
}

// ─── Promotion (runtime → build) ─────────────────────────────────────────────

fn promote(
    project_root: &Path,
    writer: &ManifestWriter,
    id: &str,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    // Look in both scopes first so we can produce a precise error message
    // when the user passes a build id by mistake.
    let (decision, scope) = match resolve_id(writer, id, None) {
        Ok(pair) => pair,
        Err(e) => {
            anyhow::bail!(
                "bock override --promote: no decision found with id `{id}` ({e})"
            );
        }
    };
    if scope != ManifestScope::Runtime {
        anyhow::bail!(
            "bock override --promote: id `{id}` is a {} decision — only runtime \
             decisions can be promoted to the build manifest",
            scope_name(scope)
        );
    }
    if !decision.pinned {
        anyhow::bail!(
            "bock override --promote: runtime decision `{id}` is not pinned. Run \
             `bock override --runtime {id}` to pin it first."
        );
    }

    // Build the promoted entry. Its DecisionType becomes `HandlerChoice`
    // so manifest routing places it in the build tree; the runtime entry
    // is kept for audit.
    let who = pinned_by();
    let now = Utc::now();
    let build_id = format!("{}+promoted", decision.id);
    let promoted = Decision {
        id: build_id.clone(),
        module: decision.module.clone(),
        target: None,
        decision_type: DecisionType::HandlerChoice,
        choice: decision.choice.clone(),
        alternatives: decision.alternatives.clone(),
        reasoning: Some(format!(
            "promoted from runtime decision {}: {}",
            decision.id,
            decision
                .reasoning
                .as_deref()
                .unwrap_or("no reasoning recorded")
        )),
        model_id: decision.model_id.clone(),
        confidence: decision.confidence,
        pinned: true,
        pin_reason: Some(
            reason
                .map(str::to_owned)
                .unwrap_or_else(|| format!("promoted from runtime by {who}")),
        ),
        pinned_at: Some(now),
        pinned_by: Some(who.clone()),
        superseded_by: None,
        timestamp: now,
    };

    // Write the promoted entry into the build manifest file for the module.
    let build_file = manifest_file_path(project_root, ManifestScope::Build, &decision.module);
    let mut build_entries = read_manifest_file(&build_file)?;
    if build_entries.iter().any(|d| d.id == build_id) {
        anyhow::bail!(
            "bock override --promote: build manifest already has an entry with id \
             `{build_id}` — this runtime decision was already promoted"
        );
    }
    build_entries.push(promoted);
    write_manifest_file(&build_file, &build_entries)?;

    // Mark the original runtime entry as superseded (kept for audit).
    let runtime_file = manifest_file_path(project_root, ManifestScope::Runtime, &decision.module);
    let mut runtime_entries = read_manifest_file(&runtime_file)?;
    for entry in runtime_entries.iter_mut() {
        if entry.id == decision.id {
            entry.superseded_by = Some(build_id.clone());
            break;
        }
    }
    write_manifest_file(&runtime_file, &runtime_entries)?;

    println!(
        "Promoted runtime decision `{}` to build manifest as `{}`\n  \
         build:   {}\n  \
         runtime: {} (marked superseded)",
        display_id(ManifestScope::Runtime, &decision.id),
        display_id(ManifestScope::Build, &build_id),
        build_file.display(),
        runtime_file.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn runtime_decision(id: &str, pinned: bool) -> Decision {
        Decision {
            id: id.into(),
            module: PathBuf::from("src/api.bock"),
            target: None,
            decision_type: DecisionType::AdaptiveRecovery,
            choice: "retry(max=3)".into(),
            alternatives: vec!["use_cached".into()],
            reasoning: Some("timeout → retry".into()),
            model_id: "stub:stub".into(),
            confidence: 0.92,
            pinned,
            pin_reason: pinned.then(|| "manual".into()),
            pinned_at: pinned.then(Utc::now),
            pinned_by: pinned.then(|| "alice".into()),
            superseded_by: None,
            timestamp: DateTime::<Utc>::from_timestamp(1_745_000_000, 0).unwrap(),
        }
    }

    fn seed_runtime(root: &Path, decisions: &[Decision]) {
        let path = manifest_file_path(
            root,
            ManifestScope::Runtime,
            &decisions[0].module,
        );
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(decisions).unwrap()).unwrap();
    }

    fn touch_project_file(root: &Path) {
        fs::write(root.join("bock.project"), "[project]\nname = \"t\"\n").unwrap();
    }

    #[test]
    fn promote_copies_pinned_runtime_entry_to_build() {
        let dir = tempdir().unwrap();
        touch_project_file(dir.path());
        seed_runtime(dir.path(), &[runtime_decision("rt1", true)]);

        let writer = ManifestWriter::new(dir.path());
        promote(dir.path(), &writer, "rt1", Some("reviewed by @bob")).unwrap();

        let build_path = manifest_file_path(
            dir.path(),
            ManifestScope::Build,
            &PathBuf::from("src/api.bock"),
        );
        let build: Vec<Decision> =
            serde_json::from_slice(&fs::read(&build_path).unwrap()).unwrap();
        assert_eq!(build.len(), 1);
        assert_eq!(build[0].id, "rt1+promoted");
        assert_eq!(build[0].decision_type, DecisionType::HandlerChoice);
        assert!(build[0].pinned);
        assert_eq!(build[0].pin_reason.as_deref(), Some("reviewed by @bob"));
        assert!(build[0].pinned_at.is_some());
        assert_eq!(build[0].choice, "retry(max=3)");

        let runtime_path = manifest_file_path(
            dir.path(),
            ManifestScope::Runtime,
            &PathBuf::from("src/api.bock"),
        );
        let runtime: Vec<Decision> =
            serde_json::from_slice(&fs::read(&runtime_path).unwrap()).unwrap();
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime[0].superseded_by.as_deref(), Some("rt1+promoted"));
    }

    #[test]
    fn promote_rejects_unpinned_runtime_entry() {
        let dir = tempdir().unwrap();
        touch_project_file(dir.path());
        seed_runtime(dir.path(), &[runtime_decision("rt-unpinned", false)]);

        let writer = ManifestWriter::new(dir.path());
        let err = promote(dir.path(), &writer, "rt-unpinned", None)
            .expect_err("should refuse unpinned promotion");
        let msg = format!("{err}");
        assert!(msg.contains("not pinned"), "unexpected error: {msg}");
    }

    #[test]
    fn promote_rejects_missing_decision() {
        let dir = tempdir().unwrap();
        touch_project_file(dir.path());
        let writer = ManifestWriter::new(dir.path());
        let err = promote(dir.path(), &writer, "missing", None).expect_err("no such id");
        assert!(format!("{err}").contains("no decision found"));
    }

    #[test]
    fn promote_refuses_build_scope_id() {
        let dir = tempdir().unwrap();
        touch_project_file(dir.path());
        // Write a build entry directly.
        let build_decision = Decision {
            decision_type: DecisionType::Codegen,
            ..runtime_decision("build1", true)
        };
        let build_path = manifest_file_path(
            dir.path(),
            ManifestScope::Build,
            &PathBuf::from("src/api.bock"),
        );
        fs::create_dir_all(build_path.parent().unwrap()).unwrap();
        fs::write(
            &build_path,
            serde_json::to_vec_pretty(&[build_decision]).unwrap(),
        )
        .unwrap();

        let writer = ManifestWriter::new(dir.path());
        let err = promote(dir.path(), &writer, "build1", None).expect_err("build scope");
        assert!(format!("{err}").contains("only runtime"));
    }

    #[test]
    fn multiple_promotes_accumulate_in_build_manifest() {
        let dir = tempdir().unwrap();
        touch_project_file(dir.path());
        seed_runtime(
            dir.path(),
            &[
                runtime_decision("rt-a", true),
                runtime_decision("rt-b", true),
            ],
        );

        let writer = ManifestWriter::new(dir.path());
        promote(dir.path(), &writer, "rt-a", None).unwrap();
        promote(dir.path(), &writer, "rt-b", None).unwrap();

        let build_path = manifest_file_path(
            dir.path(),
            ManifestScope::Build,
            &PathBuf::from("src/api.bock"),
        );
        let build: Vec<Decision> =
            serde_json::from_slice(&fs::read(&build_path).unwrap()).unwrap();
        assert_eq!(build.len(), 2);
        let mut ids: Vec<_> = build.into_iter().map(|d| d.id).collect();
        ids.sort();
        assert_eq!(ids, vec!["rt-a+promoted", "rt-b+promoted"]);
    }

    #[test]
    fn promoting_same_id_twice_is_an_error() {
        let dir = tempdir().unwrap();
        touch_project_file(dir.path());
        seed_runtime(dir.path(), &[runtime_decision("rt-x", true)]);

        let writer = ManifestWriter::new(dir.path());
        promote(dir.path(), &writer, "rt-x", None).unwrap();
        let err = promote(dir.path(), &writer, "rt-x", None).expect_err("second promote");
        assert!(format!("{err}").contains("already promoted"));
    }

    #[test]
    fn pin_sets_metadata_fields_on_build_decision() {
        let dir = tempdir().unwrap();
        touch_project_file(dir.path());
        let mut d = runtime_decision("buildable", false);
        d.decision_type = DecisionType::Codegen;
        let path = manifest_file_path(dir.path(), ManifestScope::Build, &d.module);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&[d]).unwrap()).unwrap();

        let writer = ManifestWriter::new(dir.path());
        pin(
            dir.path(),
            &writer,
            "buildable",
            false,
            Some("reviewed by @alice"),
        )
        .unwrap();

        let entries: Vec<Decision> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(entries[0].pinned);
        assert_eq!(
            entries[0].pin_reason.as_deref(),
            Some("reviewed by @alice")
        );
        assert!(entries[0].pinned_at.is_some());
        assert!(entries[0].pinned_by.is_some());
    }

    #[test]
    fn pin_with_runtime_flag_rejects_build_id() {
        let dir = tempdir().unwrap();
        touch_project_file(dir.path());
        let mut d = runtime_decision("build-id", false);
        d.decision_type = DecisionType::Codegen;
        let path = manifest_file_path(dir.path(), ManifestScope::Build, &d.module);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&[d]).unwrap()).unwrap();

        let writer = ManifestWriter::new(dir.path());
        let err = pin(dir.path(), &writer, "build-id", true, None)
            .expect_err("runtime flag but build decision");
        assert!(format!("{err}").contains("--runtime"));
    }
}
