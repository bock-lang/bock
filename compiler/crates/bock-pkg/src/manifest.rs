//! Parsing and manipulation of `bock.package` TOML manifest files.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::PkgError;

/// A parsed `bock.package` manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// The `[package]` section.
    pub package: PackageSection,

    /// The `[dependencies]` section, including optional target-specific deps.
    #[serde(default)]
    pub dependencies: DependenciesSection,

    /// The `[dev-dependencies]` section.
    #[serde(default, rename = "dev-dependencies")]
    pub dev_dependencies: BTreeMap<String, DependencySpec>,

    /// The `[features]` section.
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
}

/// The `[package]` section of the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSection {
    /// Package name.
    pub name: String,

    /// Package version (semver).
    pub version: String,

    /// Supported compilation targets.
    #[serde(default)]
    pub targets: Option<TargetsSection>,
}

/// The `[package.targets]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetsSection {
    /// List of supported target languages.
    #[serde(default)]
    pub supported: Vec<String>,
}

/// A dependency specification — either a simple version string or an inline table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DependencySpec {
    /// A simple version requirement string, e.g. `"^1.0"`.
    Simple(String),

    /// A detailed dependency specification.
    Detailed(DetailedDep),
}

/// A detailed dependency specification (inline table form).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailedDep {
    /// Version requirement.
    pub version: Option<String>,

    /// Path to a local dependency.
    pub path: Option<String>,

    /// Registry URL.
    pub registry: Option<String>,

    /// Optional features to enable.
    #[serde(default)]
    pub features: Vec<String>,
}

/// The `[dependencies]` section, supporting both common and target-specific deps.
///
/// Common deps are top-level entries like `foo = "^1.0"`. Target-specific deps
/// live under `[dependencies.target.<target>]` (e.g., `[dependencies.target.js]`).
///
/// Implements `Deref`/`DerefMut` to the common deps map for backward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DependenciesSection {
    /// Target-specific dependencies, keyed by target name.
    #[serde(default)]
    pub target: BTreeMap<String, BTreeMap<String, DependencySpec>>,

    /// Common (target-agnostic) dependencies.
    #[serde(flatten)]
    pub common: BTreeMap<String, DependencySpec>,
}

impl Deref for DependenciesSection {
    type Target = BTreeMap<String, DependencySpec>;

    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

impl DerefMut for DependenciesSection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.common
    }
}

impl DependencySpec {
    /// Returns the version requirement string, if any.
    #[must_use]
    pub fn version_req(&self) -> Option<&str> {
        match self {
            DependencySpec::Simple(v) => Some(v.as_str()),
            DependencySpec::Detailed(d) => d.version.as_deref(),
        }
    }
}

impl fmt::Display for DependencySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DependencySpec::Simple(v) => write!(f, "{v}"),
            DependencySpec::Detailed(d) => {
                if let Some(v) = &d.version {
                    write!(f, "{v}")
                } else if let Some(p) = &d.path {
                    write!(f, "path:{p}")
                } else {
                    write!(f, "*")
                }
            }
        }
    }
}

impl Manifest {
    /// Parse a manifest from a TOML string.
    pub fn parse(s: &str) -> Result<Self, PkgError> {
        toml::from_str(s).map_err(|e| PkgError::ManifestParse(e.to_string()))
    }

    /// Read and parse a manifest from a file path.
    pub fn from_file(path: &Path) -> Result<Self, PkgError> {
        let content = std::fs::read_to_string(path).map_err(|e| PkgError::Io(e.to_string()))?;
        Self::parse(&content)
    }

    /// Return all dependencies for a specific build target.
    ///
    /// Merges common (target-agnostic) dependencies with any deps declared
    /// under `[dependencies.target.<target>]`. Target-specific entries
    /// override common entries with the same name.
    #[must_use]
    pub fn dependencies_for_target(&self, target: &str) -> BTreeMap<String, DependencySpec> {
        let mut deps = self.dependencies.common.clone();
        if let Some(target_deps) = self.dependencies.target.get(target) {
            deps.extend(target_deps.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        deps
    }

    /// Serialize the manifest back to a TOML string.
    pub fn to_toml_string(&self) -> Result<String, PkgError> {
        toml::to_string_pretty(self).map_err(|e| PkgError::ManifestParse(e.to_string()))
    }

    /// Add a dependency to the manifest.
    pub fn add_dependency(&mut self, name: String, version: String) {
        self.dependencies
            .insert(name, DependencySpec::Simple(version));
    }

    /// Remove a dependency from the manifest. Returns `true` if removed.
    pub fn remove_dependency(&mut self, name: &str) -> bool {
        self.dependencies.remove(name).is_some()
    }
}

/// A workspace manifest parsed from `bock.project`.
///
/// Workspaces allow multiple packages to share a single repository
/// and optionally share dependency versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceManifest {
    /// The `[workspace]` section.
    pub workspace: WorkspaceSection,
}

/// The `[workspace]` section of a workspace manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSection {
    /// Member package directories (relative paths).
    #[serde(default)]
    pub members: Vec<String>,

    /// Shared dependency versions inherited by workspace members.
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
}

impl WorkspaceManifest {
    /// Parse a workspace manifest from a TOML string.
    pub fn parse(s: &str) -> Result<Self, PkgError> {
        toml::from_str(s).map_err(|e| PkgError::ManifestParse(e.to_string()))
    }

    /// Read and parse a workspace manifest from a file path.
    pub fn from_file(path: &Path) -> Result<Self, PkgError> {
        let content = std::fs::read_to_string(path).map_err(|e| PkgError::Io(e.to_string()))?;
        Self::parse(&content)
    }

    /// Serialize the workspace manifest to a TOML string.
    pub fn to_toml_string(&self) -> Result<String, PkgError> {
        toml::to_string_pretty(self).map_err(|e| PkgError::ManifestParse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_manifest() {
        let toml = r#"
[package]
name = "http-framework"
version = "2.1.0"

[package.targets]
supported = ["js", "rust", "go"]

[dependencies]
core-http = "^1.0"

[dev-dependencies]
test-client = "^1.0"

[features]
default = ["json"]
"#;
        let manifest = Manifest::parse(toml).unwrap();
        assert_eq!(manifest.package.name, "http-framework");
        assert_eq!(manifest.package.version, "2.1.0");
        assert_eq!(
            manifest.package.targets.as_ref().unwrap().supported,
            vec!["js", "rust", "go"]
        );
        assert!(manifest.dependencies.contains_key("core-http"));
        assert!(manifest.dev_dependencies.contains_key("test-client"));
        assert_eq!(manifest.features["default"], vec!["json"]);
    }

    #[test]
    fn add_and_remove_dependency() {
        let toml = r#"
[package]
name = "my-app"
version = "0.1.0"
"#;
        let mut manifest = Manifest::parse(toml).unwrap();
        manifest.add_dependency("foo".into(), "^1.0".into());
        assert!(manifest.dependencies.contains_key("foo"));

        assert!(manifest.remove_dependency("foo"));
        assert!(!manifest.dependencies.contains_key("foo"));
        assert!(!manifest.remove_dependency("nonexistent"));
    }

    #[test]
    fn roundtrip_serialize() {
        let toml = r#"
[package]
name = "test-pkg"
version = "1.0.0"

[dependencies]
dep-a = "^2.0"
"#;
        let manifest = Manifest::parse(toml).unwrap();
        let serialized = manifest.to_toml_string().unwrap();
        let reparsed = Manifest::parse(&serialized).unwrap();
        assert_eq!(reparsed.package.name, "test-pkg");
        assert!(reparsed.dependencies.contains_key("dep-a"));
    }

    #[test]
    fn parse_manifest_with_target_deps() {
        let toml = r#"
[package]
name = "cross-platform"
version = "1.0.0"

[dependencies]
core-http = "^1.0"

[dependencies.target.js]
node-adapter = "^1.0"
dom-shim = "^2.0"

[dependencies.target.rust]
tokio-compat = "^0.3"
"#;
        let manifest = Manifest::parse(toml).unwrap();

        // Common dep present
        assert!(manifest.dependencies.common.contains_key("core-http"));

        // Target deps present
        assert_eq!(manifest.dependencies.target.len(), 2);
        let js_deps = &manifest.dependencies.target["js"];
        assert!(js_deps.contains_key("node-adapter"));
        assert!(js_deps.contains_key("dom-shim"));
        let rust_deps = &manifest.dependencies.target["rust"];
        assert!(rust_deps.contains_key("tokio-compat"));
    }

    #[test]
    fn js_deps_included_when_target_is_js() {
        let toml = r#"
[package]
name = "cross-platform"
version = "1.0.0"

[dependencies]
core-http = "^1.0"

[dependencies.target.js]
node-adapter = "^1.0"

[dependencies.target.rust]
tokio-compat = "^0.3"
"#;
        let manifest = Manifest::parse(toml).unwrap();
        let js = manifest.dependencies_for_target("js");

        // Common dep included
        assert!(js.contains_key("core-http"));
        // JS-specific dep included
        assert!(js.contains_key("node-adapter"));
        // Rust-specific dep excluded
        assert!(!js.contains_key("tokio-compat"));
    }

    #[test]
    fn js_deps_excluded_when_target_is_rust() {
        let toml = r#"
[package]
name = "cross-platform"
version = "1.0.0"

[dependencies]
core-http = "^1.0"

[dependencies.target.js]
node-adapter = "^1.0"

[dependencies.target.rust]
tokio-compat = "^0.3"
"#;
        let manifest = Manifest::parse(toml).unwrap();
        let rust = manifest.dependencies_for_target("rust");

        // Common dep included
        assert!(rust.contains_key("core-http"));
        // JS-specific dep excluded
        assert!(!rust.contains_key("node-adapter"));
        // Rust-specific dep included
        assert!(rust.contains_key("tokio-compat"));
    }

    #[test]
    fn target_dep_overrides_common_dep() {
        let toml = r#"
[package]
name = "override-test"
version = "1.0.0"

[dependencies]
shared-lib = "^1.0"

[dependencies.target.js]
shared-lib = "^2.0"
"#;
        let manifest = Manifest::parse(toml).unwrap();
        let js = manifest.dependencies_for_target("js");
        // Target-specific version overrides the common one
        assert_eq!(js["shared-lib"].version_req(), Some("^2.0"));

        let rust = manifest.dependencies_for_target("rust");
        // Without target override, common version is used
        assert_eq!(rust["shared-lib"].version_req(), Some("^1.0"));
    }

    #[test]
    fn no_target_deps_returns_common_only() {
        let toml = r#"
[package]
name = "simple"
version = "1.0.0"

[dependencies]
foo = "^1.0"
"#;
        let manifest = Manifest::parse(toml).unwrap();
        assert!(manifest.dependencies.target.is_empty());

        let deps = manifest.dependencies_for_target("js");
        assert_eq!(deps.len(), 1);
        assert!(deps.contains_key("foo"));
    }

    #[test]
    fn parse_detailed_dependency() {
        let toml = r#"
[package]
name = "test"
version = "1.0.0"

[dependencies]
local-dep = { path = "../local-dep" }
featured = { version = "^1.0", features = ["extra"] }
"#;
        let manifest = Manifest::parse(toml).unwrap();
        let local = &manifest.dependencies["local-dep"];
        assert!(
            matches!(local, DependencySpec::Detailed(d) if d.path.as_deref() == Some("../local-dep"))
        );
        let featured = &manifest.dependencies["featured"];
        assert_eq!(featured.version_req(), Some("^1.0"));
    }

    #[test]
    fn parse_workspace_manifest() {
        let toml = r#"
[workspace]
members = ["packages/core", "packages/web"]

[workspace.dependencies]
shared-dep = "^1.0"
logging = { version = "^2.0", features = ["color"] }
"#;
        let ws = WorkspaceManifest::parse(toml).unwrap();
        assert_eq!(ws.workspace.members, vec!["packages/core", "packages/web"]);
        assert!(ws.workspace.dependencies.contains_key("shared-dep"));
        assert!(ws.workspace.dependencies.contains_key("logging"));
        assert_eq!(
            ws.workspace.dependencies["shared-dep"].version_req(),
            Some("^1.0")
        );
    }

    #[test]
    fn parse_workspace_empty_members() {
        let toml = r#"
[workspace]
members = []
"#;
        let ws = WorkspaceManifest::parse(toml).unwrap();
        assert!(ws.workspace.members.is_empty());
        assert!(ws.workspace.dependencies.is_empty());
    }

    #[test]
    fn workspace_roundtrip() {
        let toml = r#"
[workspace]
members = ["crates/a", "crates/b"]

[workspace.dependencies]
common = "^1.0"
"#;
        let ws = WorkspaceManifest::parse(toml).unwrap();
        let serialized = ws.to_toml_string().unwrap();
        let reparsed = WorkspaceManifest::parse(&serialized).unwrap();
        assert_eq!(reparsed.workspace.members, vec!["crates/a", "crates/b"]);
        assert!(reparsed.workspace.dependencies.contains_key("common"));
    }
}
