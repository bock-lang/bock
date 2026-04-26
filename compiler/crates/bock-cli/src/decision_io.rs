//! Shared helpers for CLI commands that read and write decision
//! manifests and pin metadata.
//!
//! The sibling files `inspect.rs`, `pin.rs`, `override.rs`, and
//! `cache_cmd.rs` all operate on the `.bock/decisions/{build,runtime}/`
//! trees. This module centralises the pieces they share:
//!
//! * `find_project_root()` walks up to the enclosing `bock.project`.
//! * `manifest_file_path()` maps a `(scope, module)` pair to its JSON
//!   file, following the extension-append convention the on-disk
//!   writer uses (`src/api.bock` → `src/api.bock.json`).
//! * `resolve_id()` accepts both prefixed ids (`build:abc`, `runtime:def`)
//!   and bare ids, with ambiguous bare ids reported clearly.
//! * `pinned_by()` derives the identity recorded on pin metadata.

use std::fs;
use std::path::{Path, PathBuf};

use bock_ai::{Decision, ManifestScope, ManifestWriter};

/// Walks upward from the current directory until it finds `bock.project`.
pub fn find_project_root() -> anyhow::Result<PathBuf> {
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

/// Physical JSON path for a `(scope, module)` bucket.
pub fn manifest_file_path(project_root: &Path, scope: ManifestScope, module: &Path) -> PathBuf {
    let root = project_root
        .join(".bock")
        .join("decisions")
        .join(scope.dir_name());
    let mut path = root.join(module);
    let new_ext = match path.extension().and_then(|e| e.to_str()) {
        Some(existing) => format!("{existing}.json"),
        None => "json".into(),
    };
    path.set_extension(new_ext);
    path
}

/// Reads a manifest file or returns an empty list if it does not exist.
pub fn read_manifest_file(path: &Path) -> anyhow::Result<Vec<Decision>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path)
        .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("could not parse {}: {e}", path.display()))
}

/// Serialises `entries` to `path`, creating parent directories as needed.
pub fn write_manifest_file(path: &Path, entries: &[Decision]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(entries)
        .map_err(|e| anyhow::anyhow!("could not serialize manifest: {e}"))?;
    fs::write(path, bytes)
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", path.display()))
}

/// Human-readable name for a scope (`"build"` / `"runtime"`).
#[must_use]
pub fn scope_name(scope: ManifestScope) -> &'static str {
    match scope {
        ManifestScope::Build => "build",
        ManifestScope::Runtime => "runtime",
    }
}

/// Parses `build:xxx` / `runtime:xxx` prefixes; returns the bare id and an
/// explicit scope if one was supplied.
pub fn parse_id_prefix(raw: &str) -> (Option<ManifestScope>, &str) {
    if let Some(rest) = raw.strip_prefix("build:") {
        (Some(ManifestScope::Build), rest)
    } else if let Some(rest) = raw.strip_prefix("runtime:") {
        (Some(ManifestScope::Runtime), rest)
    } else {
        (None, raw)
    }
}

/// Resolves an id (prefixed or bare) to a `(Decision, ManifestScope)`
/// pair, using a caller-supplied `force_scope` for disambiguation.
///
/// * Prefixed id + matching `force_scope`: uses the prefix scope.
/// * Prefixed id + conflicting `force_scope`: error.
/// * Bare id + `force_scope`: looks only in that scope.
/// * Bare id, no `force_scope`: searches both; errors if the same bare
///   id appears in both scopes.
pub fn resolve_id(
    writer: &ManifestWriter,
    raw_id: &str,
    force_scope: Option<ManifestScope>,
) -> anyhow::Result<(Decision, ManifestScope)> {
    let (prefix_scope, bare) = parse_id_prefix(raw_id);

    let scope_filter = match (prefix_scope, force_scope) {
        (Some(a), Some(b)) if a != b => {
            anyhow::bail!(
                "id `{raw_id}` is prefixed `{}` but the command requests `{}`",
                scope_name(a),
                scope_name(b)
            );
        }
        (Some(a), _) => Some(a),
        (None, forced) => forced,
    };

    let build = writer
        .read_build()
        .map_err(|e| anyhow::anyhow!("could not read build manifest: {e}"))?;
    let runtime = writer
        .read_runtime()
        .map_err(|e| anyhow::anyhow!("could not read runtime manifest: {e}"))?;

    let build_hit = build.into_iter().find(|d| d.id == bare);
    let runtime_hit = runtime.into_iter().find(|d| d.id == bare);

    match scope_filter {
        Some(ManifestScope::Build) => build_hit
            .map(|d| (d, ManifestScope::Build))
            .ok_or_else(|| anyhow::anyhow!("no build decision with id `{bare}`")),
        Some(ManifestScope::Runtime) => runtime_hit
            .map(|d| (d, ManifestScope::Runtime))
            .ok_or_else(|| anyhow::anyhow!("no runtime decision with id `{bare}`")),
        None => match (build_hit, runtime_hit) {
            (Some(_), Some(_)) => {
                anyhow::bail!(
                    "id `{bare}` is ambiguous: it exists in both build and runtime \
                     manifests. Qualify it with `build:{bare}` or `runtime:{bare}`."
                );
            }
            (Some(d), None) => Ok((d, ManifestScope::Build)),
            (None, Some(d)) => Ok((d, ManifestScope::Runtime)),
            (None, None) => {
                anyhow::bail!("no decision found with id `{bare}`");
            }
        },
    }
}

/// Identity recorded on pin metadata.
pub fn pinned_by() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

/// Human-facing id with its scope prefix, for CLI output.
#[must_use]
pub fn display_id(scope: ManifestScope, id: &str) -> String {
    format!("{}:{id}", scope_name(scope))
}
