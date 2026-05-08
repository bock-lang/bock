//! On-disk decision manifest reader/writer.
//!
//! Per the 2026-04-22 spec amendment, decisions are stored in two
//! sibling directories under `.bock/decisions/`:
//!
//! ```text
//! .bock/
//!   decisions/
//!     build/      # codegen, repair, optimize, rule_applied — committed
//!     runtime/    # adaptive_recovery — local only
//! ```
//!
//! The writer keeps a per-module bucket of buffered [`Decision`]s and
//! serializes one JSON file per `(scope, module_path)` on
//! [`ManifestWriter::flush`]. Reads merge whatever is on disk with the
//! still-buffered entries, so a decision recorded in the current process
//! is visible to [`find_by_id`](ManifestWriter::find_by_id) before flush.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::decision::{Decision, ManifestScope};

/// Errors produced by manifest reads and writes.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Filesystem I/O failed.
    #[error("manifest I/O error: {0}")]
    Io(#[from] io::Error),
    /// JSON parse failed reading a stored manifest file.
    #[error("manifest parse error in {path}: {source}")]
    Parse {
        /// Offending file path.
        path: PathBuf,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },
    /// JSON serialization failed.
    #[error("manifest serialize error: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Reads and writes decision manifests, auto-routing each [`Decision`]
/// to the build or runtime tree by [`Decision::decision_type`]'s scope.
#[derive(Debug)]
pub struct ManifestWriter {
    build_root: PathBuf,
    runtime_root: PathBuf,
    /// Buffered decisions keyed by (scope, module_path), preserving
    /// insertion order within each bucket.
    pending: BTreeMap<(ManifestScope, PathBuf), Vec<Decision>>,
}

impl ManifestWriter {
    /// Creates a writer rooted at `<project_root>/.bock/decisions/`.
    ///
    /// The directories are not created eagerly — they are materialised
    /// on [`flush`](Self::flush) so reads against an unused project
    /// don't leave stray empty trees behind.
    #[must_use]
    pub fn new(project_root: &Path) -> Self {
        let decisions = project_root.join(".bock").join("decisions");
        Self {
            build_root: decisions.join(ManifestScope::Build.dir_name()),
            runtime_root: decisions.join(ManifestScope::Runtime.dir_name()),
            pending: BTreeMap::new(),
        }
    }

    /// Path to the build-scope root.
    #[must_use]
    pub fn build_root(&self) -> &Path {
        &self.build_root
    }

    /// Path to the runtime-scope root.
    #[must_use]
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    /// Buffers a decision. Routing is determined by
    /// [`DecisionType::scope`](crate::decision::DecisionType::scope).
    pub fn record(&mut self, decision: Decision) {
        let scope = decision.decision_type.scope();
        let key = (scope, decision.module.clone());
        self.pending.entry(key).or_default().push(decision);
    }

    /// Writes all buffered decisions to disk and clears the buffer.
    ///
    /// Each `(scope, module_path)` bucket is written as a JSON array.
    /// If a file already exists for the bucket, its contents are merged
    /// (existing entries first, then newly buffered ones) and rewritten.
    ///
    /// # Errors
    /// Returns [`ManifestError`] on I/O or serialization failure.
    pub fn flush(&mut self) -> Result<(), ManifestError> {
        let pending = std::mem::take(&mut self.pending);
        for ((scope, module), new_entries) in pending {
            let path = self.path_for(scope, &module);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut combined = read_file_if_exists(&path)?;
            combined.extend(new_entries);
            let bytes = serde_json::to_vec_pretty(&combined)?;
            fs::write(&path, bytes)?;
        }
        Ok(())
    }

    /// Reads all build-scope decisions on disk plus any buffered ones.
    ///
    /// # Errors
    /// Returns [`ManifestError`] on I/O or parse failure.
    pub fn read_build(&self) -> Result<Vec<Decision>, ManifestError> {
        self.read_scope(ManifestScope::Build)
    }

    /// Reads all runtime-scope decisions on disk plus any buffered ones.
    ///
    /// # Errors
    /// Returns [`ManifestError`] on I/O or parse failure.
    pub fn read_runtime(&self) -> Result<Vec<Decision>, ManifestError> {
        self.read_scope(ManifestScope::Runtime)
    }

    /// Returns build + runtime decisions merged into one list.
    ///
    /// # Errors
    /// Returns [`ManifestError`] on I/O or parse failure.
    pub fn read_all(&self) -> Result<Vec<Decision>, ManifestError> {
        let mut out = self.read_build()?;
        out.extend(self.read_runtime()?);
        Ok(out)
    }

    /// Searches both scopes for a decision with the given id.
    ///
    /// Returns the matching [`Decision`] together with the
    /// [`ManifestScope`] it was found in, or [`None`] if no match.
    ///
    /// # Errors
    /// Returns [`ManifestError`] on I/O or parse failure.
    pub fn find_by_id(&self, id: &str) -> Result<Option<(Decision, ManifestScope)>, ManifestError> {
        for d in self.read_build()? {
            if d.id == id {
                return Ok(Some((d, ManifestScope::Build)));
            }
        }
        for d in self.read_runtime()? {
            if d.id == id {
                return Ok(Some((d, ManifestScope::Runtime)));
            }
        }
        Ok(None)
    }

    fn root_for(&self, scope: ManifestScope) -> &Path {
        match scope {
            ManifestScope::Build => &self.build_root,
            ManifestScope::Runtime => &self.runtime_root,
        }
    }

    fn path_for(&self, scope: ManifestScope, module: &Path) -> PathBuf {
        let root = self.root_for(scope);
        let mut path = root.join(module);
        let new_ext = match path.extension().and_then(|e| e.to_str()) {
            Some(existing) => format!("{existing}.json"),
            None => "json".into(),
        };
        path.set_extension(new_ext);
        path
    }

    fn read_scope(&self, scope: ManifestScope) -> Result<Vec<Decision>, ManifestError> {
        let mut out = Vec::new();
        let root = self.root_for(scope);
        if root.exists() {
            walk_json_files(root, &mut out)?;
        }
        for ((s, _module), entries) in &self.pending {
            if *s == scope {
                out.extend(entries.iter().cloned());
            }
        }
        Ok(out)
    }
}

fn read_file_if_exists(path: &Path) -> Result<Vec<Decision>, ManifestError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice::<Vec<Decision>>(&bytes).map_err(|source| ManifestError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn walk_json_files(root: &Path, out: &mut Vec<Decision>) -> Result<(), ManifestError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            walk_json_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.extend(read_file_if_exists(&path)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::DecisionType;
    use chrono::{DateTime, Utc};

    fn decision(id: &str, module: &str, dt: DecisionType) -> Decision {
        Decision {
            id: id.into(),
            module: PathBuf::from(module),
            target: Some("rust".into()),
            decision_type: dt,
            choice: "x".into(),
            alternatives: Vec::new(),
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
    fn record_routes_codegen_to_build_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = ManifestWriter::new(dir.path());
        w.record(decision("a", "src/main.bock", DecisionType::Codegen));
        w.flush().unwrap();

        let build_file = w.build_root().join("src/main.bock.json");
        assert!(build_file.exists(), "missing build file: {build_file:?}");
        let runtime_file = w.runtime_root().join("src/main.bock.json");
        assert!(!runtime_file.exists(), "runtime tree should be empty");
    }

    #[test]
    fn record_routes_adaptive_to_runtime_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = ManifestWriter::new(dir.path());
        w.record(decision(
            "a",
            "src/main.bock",
            DecisionType::AdaptiveRecovery,
        ));
        w.flush().unwrap();

        let runtime_file = w.runtime_root().join("src/main.bock.json");
        assert!(runtime_file.exists());
        let build_file = w.build_root().join("src/main.bock.json");
        assert!(!build_file.exists());
    }

    #[test]
    fn read_build_excludes_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = ManifestWriter::new(dir.path());
        w.record(decision("b", "src/main.bock", DecisionType::Codegen));
        w.record(decision(
            "r",
            "src/main.bock",
            DecisionType::AdaptiveRecovery,
        ));
        w.flush().unwrap();

        let build = w.read_build().unwrap();
        assert_eq!(build.len(), 1);
        assert_eq!(build[0].id, "b");

        let runtime = w.read_runtime().unwrap();
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime[0].id, "r");
    }

    #[test]
    fn read_all_returns_merged_view() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = ManifestWriter::new(dir.path());
        w.record(decision("b", "src/main.bock", DecisionType::Codegen));
        w.record(decision(
            "r",
            "src/main.bock",
            DecisionType::AdaptiveRecovery,
        ));
        w.flush().unwrap();

        let all = w.read_all().unwrap();
        let mut ids: Vec<_> = all.into_iter().map(|d| d.id).collect();
        ids.sort();
        assert_eq!(ids, vec!["b", "r"]);
    }

    #[test]
    fn find_by_id_searches_both_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = ManifestWriter::new(dir.path());
        w.record(decision("b1", "src/lib.bock", DecisionType::Codegen));
        w.record(decision(
            "r1",
            "src/lib.bock",
            DecisionType::AdaptiveRecovery,
        ));
        w.flush().unwrap();

        let (d, scope) = w.find_by_id("b1").unwrap().unwrap();
        assert_eq!(d.id, "b1");
        assert_eq!(scope, ManifestScope::Build);

        let (d, scope) = w.find_by_id("r1").unwrap().unwrap();
        assert_eq!(d.id, "r1");
        assert_eq!(scope, ManifestScope::Runtime);

        assert!(w.find_by_id("missing").unwrap().is_none());
    }

    #[test]
    fn flush_appends_to_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = ManifestWriter::new(dir.path());
        w.record(decision("a", "src/main.bock", DecisionType::Codegen));
        w.flush().unwrap();
        w.record(decision("b", "src/main.bock", DecisionType::Codegen));
        w.flush().unwrap();

        let entries = w.read_build().unwrap();
        let ids: Vec<_> = entries.into_iter().map(|d| d.id).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn buffered_decisions_visible_before_flush() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = ManifestWriter::new(dir.path());
        w.record(decision("buf", "src/main.bock", DecisionType::Codegen));
        let (d, scope) = w.find_by_id("buf").unwrap().unwrap();
        assert_eq!(d.id, "buf");
        assert_eq!(scope, ManifestScope::Build);
    }

    #[test]
    fn nested_module_paths_create_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = ManifestWriter::new(dir.path());
        w.record(decision(
            "x",
            "src/net/http_client.bock",
            DecisionType::Codegen,
        ));
        w.flush().unwrap();
        let p = w.build_root().join("src/net/http_client.bock.json");
        assert!(p.exists(), "expected nested file at {p:?}");
    }
}
