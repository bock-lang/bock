//! High-level package manager commands (`add`, `remove`, `tree`, etc.).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{PkgError, PkgResult};
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::resolver::PackageRegistry;
use crate::tree;

/// The default manifest file name.
pub const MANIFEST_FILE: &str = "bock.package";

/// The default lockfile name.
pub const LOCKFILE: &str = "bock.lock";

/// Find the manifest file in the given directory or its ancestors.
pub fn find_manifest(start_dir: &Path) -> PkgResult<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(MANIFEST_FILE);
        if candidate.exists() {
            return Ok(candidate);
        }
        if !dir.pop() {
            return Err(PkgError::Io(format!(
                "no {MANIFEST_FILE} found in {start_dir:?} or any parent directory"
            )));
        }
    }
}

/// Initialize a new package manifest in the given directory.
pub fn init(dir: &Path, name: &str) -> PkgResult<PathBuf> {
    let manifest_path = dir.join(MANIFEST_FILE);
    if manifest_path.exists() {
        return Err(PkgError::Io(format!(
            "{MANIFEST_FILE} already exists in {}",
            dir.display()
        )));
    }

    let manifest = Manifest {
        package: crate::manifest::PackageSection {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            targets: None,
        },
        dependencies: Default::default(),
        dev_dependencies: BTreeMap::new(),
        features: BTreeMap::new(),
    };

    let content = manifest.to_toml_string()?;
    std::fs::write(&manifest_path, content).map_err(|e| PkgError::Io(e.to_string()))?;

    Ok(manifest_path)
}

/// Add a dependency to the manifest.
///
/// If no version is specified, defaults to `"*"` (any version).
pub fn add(manifest_path: &Path, name: &str, version: Option<&str>) -> PkgResult<()> {
    let mut manifest = Manifest::from_file(manifest_path)?;
    let version_str = version.unwrap_or("*").to_string();
    manifest.add_dependency(name.to_string(), version_str.clone());

    let content = manifest.to_toml_string()?;
    std::fs::write(manifest_path, content).map_err(|e| PkgError::Io(e.to_string()))?;

    Ok(())
}

/// Remove a dependency from the manifest.
pub fn remove(manifest_path: &Path, name: &str) -> PkgResult<()> {
    let mut manifest = Manifest::from_file(manifest_path)?;

    if !manifest.remove_dependency(name) {
        return Err(PkgError::PackageNotFound(format!(
            "'{name}' is not listed in [dependencies]"
        )));
    }

    let content = manifest.to_toml_string()?;
    std::fs::write(manifest_path, content).map_err(|e| PkgError::Io(e.to_string()))?;

    Ok(())
}

/// Resolve dependencies and generate/update the lockfile.
pub fn resolve_and_lock(manifest_path: &Path, registry: &PackageRegistry) -> PkgResult<Lockfile> {
    let manifest = Manifest::from_file(manifest_path)?;
    let lock_path = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(LOCKFILE);

    // Convert manifest dependencies to name→version_req map
    let direct_deps: BTreeMap<String, String> = manifest
        .dependencies
        .iter()
        .filter_map(|(name, spec)| spec.version_req().map(|v| (name.clone(), v.to_string())))
        .collect();

    let resolved = registry.resolve(
        &manifest.package.name,
        &manifest.package.version,
        &direct_deps,
    )?;

    let lockfile = Lockfile::from_resolved(&resolved);
    lockfile.write_to_file(&lock_path)?;

    Ok(lockfile)
}

/// Display the dependency tree.
pub fn show_tree(manifest_path: &Path, registry: &PackageRegistry) -> PkgResult<String> {
    let manifest = Manifest::from_file(manifest_path)?;
    let lock_path = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(LOCKFILE);

    // Convert manifest dependencies to name→version_req map
    let direct_deps: BTreeMap<String, String> = manifest
        .dependencies
        .iter()
        .filter_map(|(name, spec)| spec.version_req().map(|v| (name.clone(), v.to_string())))
        .collect();

    // Read resolved versions from lockfile if it exists
    let resolved = if lock_path.exists() {
        let lockfile = Lockfile::from_file(&lock_path)?;
        lockfile.to_resolved()?
    } else {
        // Resolve on the fly
        registry.resolve(
            &manifest.package.name,
            &manifest.package.version,
            &direct_deps,
        )?
    };

    Ok(tree::render_tree(
        &manifest.package.name,
        &manifest.package.version,
        &direct_deps,
        &resolved,
        registry,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path(), "test-project").unwrap();
        assert!(path.exists());

        let manifest = Manifest::from_file(&path).unwrap();
        assert_eq!(manifest.package.name, "test-project");
        assert_eq!(manifest.package.version, "0.1.0");
    }

    #[test]
    fn init_fails_if_exists() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path(), "test").unwrap();
        let result = init(dir.path(), "test");
        assert!(result.is_err());
    }

    #[test]
    fn add_dependency_to_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path(), "my-app").unwrap();

        add(&path, "foo", Some("^1.0")).unwrap();

        let manifest = Manifest::from_file(&path).unwrap();
        assert!(manifest.dependencies.contains_key("foo"));
        assert_eq!(manifest.dependencies["foo"].version_req(), Some("^1.0"));
    }

    #[test]
    fn remove_dependency_from_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path(), "my-app").unwrap();

        add(&path, "foo", Some("^1.0")).unwrap();
        remove(&path, "foo").unwrap();

        let manifest = Manifest::from_file(&path).unwrap();
        assert!(!manifest.dependencies.contains_key("foo"));
    }

    #[test]
    fn remove_nonexistent_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path(), "my-app").unwrap();

        let result = remove(&path, "nonexistent");
        assert!(matches!(result, Err(PkgError::PackageNotFound(_))));
    }

    #[test]
    fn resolve_and_lock_creates_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path(), "my-app").unwrap();

        // Add a dep and register it in the registry
        add(&path, "foo", Some("^1.0")).unwrap();

        let mut registry = PackageRegistry::new();
        registry.register("foo", "1.2.0", BTreeMap::new()).unwrap();

        let lockfile = resolve_and_lock(&path, &registry).unwrap();
        assert_eq!(lockfile.get_version("foo"), Some("1.2.0"));

        // Check lockfile was written
        let lock_path = dir.path().join(LOCKFILE);
        assert!(lock_path.exists());
    }

    #[test]
    fn show_tree_renders_deps() {
        let dir = tempfile::tempdir().unwrap();
        let path = init(dir.path(), "my-app").unwrap();
        add(&path, "foo", Some("^1.0")).unwrap();

        let mut registry = PackageRegistry::new();
        registry.register("foo", "1.0.0", BTreeMap::new()).unwrap();

        // Create a lockfile first
        resolve_and_lock(&path, &registry).unwrap();

        let tree_output = show_tree(&path, &registry).unwrap();
        assert!(tree_output.contains("my-app v0.1.0"));
        assert!(tree_output.contains("foo v1.0.0"));
    }
}
