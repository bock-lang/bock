//! Dependency resolver using the PubGrub algorithm.
//!
//! Wraps the `pubgrub` crate to resolve Bock package dependencies from
//! a registry of known packages and their version constraints.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use pubgrub::{
    Dependencies, DependencyConstraints, DependencyProvider, PackageResolutionStatistics, Ranges,
};
use semver::Version;

use crate::error::PkgError;
use crate::version::{parse_version, parse_version_req, req_to_pubgrub_range};

/// Type alias for pubgrub version ranges over semver versions.
pub type SemverRanges = Ranges<Version>;

/// A resolved set of packages and their selected versions.
pub type ResolvedDeps = BTreeMap<String, Version>;

/// Unified feature sets: package name → set of enabled features.
pub type UnifiedFeatures = BTreeMap<String, BTreeSet<String>>;

/// Visibility of a resolved dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepVisibility {
    /// Visible to dependents (direct dependency).
    Public,
    /// Not visible to dependents (transitive dependency, private by default).
    Private,
}

/// Metadata for a specific version of a package in the registry.
#[derive(Debug, Clone, Default)]
pub struct PackageVersionMeta {
    /// Dependencies: name → version requirement string.
    pub deps: BTreeMap<String, String>,
    /// Features requested for each dependency: dep_name → feature list.
    pub dep_features: BTreeMap<String, Vec<String>>,
    /// Targets this version supports. `None` means all targets are supported.
    pub supported_targets: Option<Vec<String>>,
    /// Features declared by this package version: feature_name → implied deps/features.
    pub available_features: BTreeMap<String, Vec<String>>,
}

/// A registry of known packages and their versions/dependencies.
///
/// This serves as the `DependencyProvider` for pubgrub resolution.
/// In v1, packages are registered in-memory (from lockfiles, local dirs, etc.).
#[derive(Debug, Clone, Default)]
pub struct PackageRegistry {
    /// Map of package name → available versions (sorted) and their metadata.
    packages: BTreeMap<String, BTreeMap<Version, PackageVersionMeta>>,
}

impl PackageRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a package version with its dependencies.
    ///
    /// Dependencies are given as `name → version_req` pairs (e.g., `"^1.0"`).
    pub fn register(
        &mut self,
        name: &str,
        version: &str,
        deps: BTreeMap<String, String>,
    ) -> Result<(), PkgError> {
        let meta = PackageVersionMeta {
            deps,
            ..Default::default()
        };
        self.register_with_meta(name, version, meta)
    }

    /// Register a package version with full metadata including targets and features.
    pub fn register_with_meta(
        &mut self,
        name: &str,
        version: &str,
        meta: PackageVersionMeta,
    ) -> Result<(), PkgError> {
        let ver = parse_version(version)?;
        self.packages
            .entry(name.to_string())
            .or_default()
            .insert(ver, meta);
        Ok(())
    }

    /// List all available versions for a package.
    #[must_use]
    pub fn available_versions(&self, name: &str) -> Vec<&Version> {
        self.packages
            .get(name)
            .map(|versions| versions.keys().collect())
            .unwrap_or_default()
    }

    /// Check if a package exists in the registry.
    #[must_use]
    pub fn has_package(&self, name: &str) -> bool {
        self.packages.contains_key(name)
    }

    /// Resolve dependencies starting from a root package with given direct dependencies.
    ///
    /// Returns the resolved set of packages and their versions.
    pub fn resolve(
        &self,
        root_name: &str,
        root_version: &str,
        direct_deps: &BTreeMap<String, String>,
    ) -> Result<ResolvedDeps, PkgError> {
        self.resolve_internal(root_name, root_version, direct_deps, None)
    }

    /// Resolve dependencies for a specific build target.
    ///
    /// Packages whose `supported_targets` don't include `target` are skipped
    /// during version selection.
    pub fn resolve_for_target(
        &self,
        root_name: &str,
        root_version: &str,
        direct_deps: &BTreeMap<String, String>,
        target: &str,
    ) -> Result<ResolvedDeps, PkgError> {
        self.resolve_internal(
            root_name,
            root_version,
            direct_deps,
            Some(target.to_string()),
        )
    }

    fn resolve_internal(
        &self,
        root_name: &str,
        root_version: &str,
        direct_deps: &BTreeMap<String, String>,
        active_target: Option<String>,
    ) -> Result<ResolvedDeps, PkgError> {
        let root_ver = parse_version(root_version)?;

        // Create a provider that includes the root package
        let provider = ResolverProvider {
            registry: self,
            root_package: root_name.to_string(),
            root_version: root_ver.clone(),
            root_deps: direct_deps.clone(),
            active_target,
        };

        let result = pubgrub::resolve(&provider, root_name.to_string(), root_ver).map_err(|e| {
            let msg = format!("{e}");
            // Check if this is a "no solution" type error
            if msg.contains("No solution") || msg.contains("conflict") {
                PkgError::UnresolvableConstraints(vec![msg])
            } else {
                PkgError::ResolutionFailed(msg)
            }
        })?;

        // Convert to BTreeMap, excluding the root package
        let mut resolved = BTreeMap::new();
        for (pkg, ver) in result {
            if pkg != root_name {
                resolved.insert(pkg, ver);
            }
        }

        Ok(resolved)
    }

    /// Unify features requested for each dependency across the dependency graph.
    ///
    /// When multiple paths in the dep graph request different features of the
    /// same package, the union of all requested feature sets is returned.
    #[must_use]
    pub fn unify_features(
        &self,
        root_dep_features: &BTreeMap<String, Vec<String>>,
        resolved: &ResolvedDeps,
    ) -> UnifiedFeatures {
        let mut unified: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        // Collect features requested by the root package's deps
        for (name, feats) in root_dep_features {
            if resolved.contains_key(name) && !feats.is_empty() {
                unified
                    .entry(name.clone())
                    .or_default()
                    .extend(feats.iter().cloned());
            }
        }

        // Collect features requested by each resolved package's transitive deps
        for (pkg_name, ver) in resolved {
            if let Some(versions) = self.packages.get(pkg_name) {
                if let Some(meta) = versions.get(ver) {
                    for (dep_name, feats) in &meta.dep_features {
                        if resolved.contains_key(dep_name) && !feats.is_empty() {
                            unified
                                .entry(dep_name.clone())
                                .or_default()
                                .extend(feats.iter().cloned());
                        }
                    }
                }
            }
        }

        unified
    }
}

/// Compute visibility of resolved dependencies.
///
/// Direct dependencies are [`DepVisibility::Public`]. Transitive dependencies
/// (not directly listed in the root's dependency set) are [`DepVisibility::Private`]
/// by default.
#[must_use]
pub fn compute_visibility(
    direct_dep_names: &BTreeMap<String, String>,
    resolved: &ResolvedDeps,
) -> BTreeMap<String, DepVisibility> {
    resolved
        .keys()
        .map(|name| {
            let vis = if direct_dep_names.contains_key(name) {
                DepVisibility::Public
            } else {
                DepVisibility::Private
            };
            (name.clone(), vis)
        })
        .collect()
}

/// Internal provider that implements pubgrub's `DependencyProvider`.
struct ResolverProvider<'a> {
    registry: &'a PackageRegistry,
    root_package: String,
    root_version: Version,
    root_deps: BTreeMap<String, String>,
    active_target: Option<String>,
}

impl<'a> DependencyProvider for ResolverProvider<'a> {
    type P = String;
    type V = Version;
    type VS = Ranges<Version>;
    type M = String;
    type Err = Infallible;
    type Priority = u32;

    fn prioritize(
        &self,
        package: &String,
        _range: &Ranges<Version>,
        _stats: &PackageResolutionStatistics,
    ) -> Self::Priority {
        // Higher priority for packages with fewer versions (more constrained first)
        if package == &self.root_package {
            return u32::MAX;
        }
        let count = self
            .registry
            .packages
            .get(package)
            .map(|v| v.len())
            .unwrap_or(0);
        // Invert: fewer versions = higher priority
        u32::MAX.saturating_sub(count as u32)
    }

    fn choose_version(
        &self,
        package: &String,
        range: &Ranges<Version>,
    ) -> Result<Option<Version>, Infallible> {
        if package == &self.root_package {
            if range.contains(&self.root_version) {
                return Ok(Some(self.root_version.clone()));
            }
            return Ok(None);
        }

        // Choose the highest version that matches the range and target
        let version = self.registry.packages.get(package).and_then(|versions| {
            versions
                .keys()
                .rev()
                .find(|v| {
                    if !range.contains(v) {
                        return false;
                    }
                    // M-090: Skip versions incompatible with the active target
                    if let Some(target) = &self.active_target {
                        if let Some(meta) = versions.get(v) {
                            if let Some(supported) = &meta.supported_targets {
                                return supported.iter().any(|t| t == target);
                            }
                        }
                    }
                    true
                })
                .cloned()
        });

        Ok(version)
    }

    fn get_dependencies(
        &self,
        package: &String,
        version: &Version,
    ) -> Result<Dependencies<String, Ranges<Version>, String>, Infallible> {
        if package == &self.root_package && version == &self.root_version {
            // Return the root's direct dependencies
            let deps: DependencyConstraints<String, Ranges<Version>> = self
                .root_deps
                .iter()
                .filter_map(|(name, req_str)| {
                    parse_version_req(req_str)
                        .ok()
                        .map(|req| (name.clone(), req_to_pubgrub_range(&req)))
                })
                .collect();
            return Ok(Dependencies::Available(deps));
        }

        let Some(versions) = self.registry.packages.get(package) else {
            return Ok(Dependencies::Unavailable(format!(
                "package '{package}' not found in registry"
            )));
        };

        let Some(meta) = versions.get(version) else {
            return Ok(Dependencies::Unavailable(format!(
                "version {version} of '{package}' not found"
            )));
        };

        let deps: DependencyConstraints<String, Ranges<Version>> = meta
            .deps
            .iter()
            .filter_map(|(name, req_str)| {
                parse_version_req(req_str)
                    .ok()
                    .map(|req| (name.clone(), req_to_pubgrub_range(&req)))
            })
            .collect();

        Ok(Dependencies::Available(deps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_deps(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn resolve_simple_deps() {
        let mut registry = PackageRegistry::new();
        registry.register("foo", "1.0.0", BTreeMap::new()).unwrap();
        registry.register("foo", "1.1.0", BTreeMap::new()).unwrap();
        registry
            .register("bar", "2.0.0", make_deps(&[("foo", "^1.0")]))
            .unwrap();

        let direct = make_deps(&[("bar", "^2.0")]);
        let resolved = registry.resolve("my-app", "0.1.0", &direct).unwrap();

        assert!(resolved.contains_key("bar"));
        assert!(resolved.contains_key("foo"));
        assert_eq!(resolved["bar"], Version::new(2, 0, 0));
        // Should pick highest compatible foo version
        assert_eq!(resolved["foo"], Version::new(1, 1, 0));
    }

    #[test]
    fn resolve_transitive_deps() {
        let mut registry = PackageRegistry::new();
        registry.register("c", "1.0.0", BTreeMap::new()).unwrap();
        registry
            .register("b", "1.0.0", make_deps(&[("c", "^1.0")]))
            .unwrap();
        registry
            .register("a", "1.0.0", make_deps(&[("b", "^1.0")]))
            .unwrap();

        let direct = make_deps(&[("a", "^1.0")]);
        let resolved = registry.resolve("root", "0.1.0", &direct).unwrap();

        assert_eq!(resolved.len(), 3);
        assert!(resolved.contains_key("a"));
        assert!(resolved.contains_key("b"));
        assert!(resolved.contains_key("c"));
    }

    #[test]
    fn resolve_conflicting_deps() {
        let mut registry = PackageRegistry::new();
        // Only version 1.0.0 of shared exists
        registry
            .register("shared", "1.0.0", BTreeMap::new())
            .unwrap();
        // a needs shared ^1.0 (ok)
        registry
            .register("a", "1.0.0", make_deps(&[("shared", "^1.0")]))
            .unwrap();
        // b needs shared ^2.0 (conflict — no 2.x available)
        registry
            .register("b", "1.0.0", make_deps(&[("shared", "^2.0")]))
            .unwrap();

        let direct = make_deps(&[("a", "^1.0"), ("b", "^1.0")]);
        let result = registry.resolve("root", "0.1.0", &direct);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_empty_deps() {
        let registry = PackageRegistry::new();
        let direct = BTreeMap::new();
        let resolved = registry.resolve("my-app", "1.0.0", &direct).unwrap();
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_picks_highest_compatible() {
        let mut registry = PackageRegistry::new();
        registry.register("foo", "1.0.0", BTreeMap::new()).unwrap();
        registry.register("foo", "1.2.0", BTreeMap::new()).unwrap();
        registry.register("foo", "1.5.0", BTreeMap::new()).unwrap();
        registry.register("foo", "2.0.0", BTreeMap::new()).unwrap();

        let direct = make_deps(&[("foo", "^1.0")]);
        let resolved = registry.resolve("root", "0.1.0", &direct).unwrap();
        assert_eq!(resolved["foo"], Version::new(1, 5, 0));
    }

    // --- M-090: Target filtering ---

    #[test]
    fn target_filtering_skips_incompatible_packages() {
        let mut registry = PackageRegistry::new();

        // foo 1.0.0 supports only js
        registry
            .register_with_meta(
                "foo",
                "1.0.0",
                PackageVersionMeta {
                    deps: BTreeMap::new(),
                    supported_targets: Some(vec!["js".into()]),
                    ..Default::default()
                },
            )
            .unwrap();

        // foo 1.1.0 supports js and rust
        registry
            .register_with_meta(
                "foo",
                "1.1.0",
                PackageVersionMeta {
                    deps: BTreeMap::new(),
                    supported_targets: Some(vec!["js".into(), "rust".into()]),
                    ..Default::default()
                },
            )
            .unwrap();

        let direct = make_deps(&[("foo", "^1.0")]);

        // Resolving for js should pick 1.1.0 (highest compatible)
        let resolved = registry
            .resolve_for_target("root", "0.1.0", &direct, "js")
            .unwrap();
        assert_eq!(resolved["foo"], Version::new(1, 1, 0));

        // Resolving for rust should pick 1.1.0 (only compatible version)
        let resolved = registry
            .resolve_for_target("root", "0.1.0", &direct, "rust")
            .unwrap();
        assert_eq!(resolved["foo"], Version::new(1, 1, 0));

        // Resolving for python should fail (no compatible version)
        let result = registry.resolve_for_target("root", "0.1.0", &direct, "python");
        assert!(result.is_err());
    }

    #[test]
    fn target_filtering_no_targets_means_all() {
        let mut registry = PackageRegistry::new();

        // foo has no target restriction
        registry.register("foo", "1.0.0", BTreeMap::new()).unwrap();

        let direct = make_deps(&[("foo", "^1.0")]);

        // Should resolve for any target
        let resolved = registry
            .resolve_for_target("root", "0.1.0", &direct, "rust")
            .unwrap();
        assert_eq!(resolved["foo"], Version::new(1, 0, 0));
    }

    #[test]
    fn target_filtering_transitive() {
        let mut registry = PackageRegistry::new();

        // "inner" only supports rust
        registry
            .register_with_meta(
                "inner",
                "1.0.0",
                PackageVersionMeta {
                    deps: BTreeMap::new(),
                    supported_targets: Some(vec!["rust".into()]),
                    ..Default::default()
                },
            )
            .unwrap();

        // "outer" depends on inner, supports all targets
        registry
            .register("outer", "1.0.0", make_deps(&[("inner", "^1.0")]))
            .unwrap();

        let direct = make_deps(&[("outer", "^1.0")]);

        // Resolving for js should fail because inner doesn't support js
        let result = registry.resolve_for_target("root", "0.1.0", &direct, "js");
        assert!(result.is_err());

        // Resolving for rust should succeed
        let resolved = registry
            .resolve_for_target("root", "0.1.0", &direct, "rust")
            .unwrap();
        assert!(resolved.contains_key("outer"));
        assert!(resolved.contains_key("inner"));
    }

    // --- M-091: Feature unification ---

    #[test]
    fn feature_unification_unions_features() {
        let mut registry = PackageRegistry::new();

        // "shared" has features available
        registry
            .register_with_meta(
                "shared",
                "1.0.0",
                PackageVersionMeta {
                    deps: BTreeMap::new(),
                    available_features: BTreeMap::from([
                        ("json".into(), vec![]),
                        ("xml".into(), vec![]),
                        ("yaml".into(), vec![]),
                    ]),
                    ..Default::default()
                },
            )
            .unwrap();

        // "a" depends on shared with feature "json"
        registry
            .register_with_meta(
                "a",
                "1.0.0",
                PackageVersionMeta {
                    deps: make_deps(&[("shared", "^1.0")]),
                    dep_features: BTreeMap::from([("shared".into(), vec!["json".into()])]),
                    ..Default::default()
                },
            )
            .unwrap();

        // "b" depends on shared with features "xml" and "yaml"
        registry
            .register_with_meta(
                "b",
                "1.0.0",
                PackageVersionMeta {
                    deps: make_deps(&[("shared", "^1.0")]),
                    dep_features: BTreeMap::from([(
                        "shared".into(),
                        vec!["xml".into(), "yaml".into()],
                    )]),
                    ..Default::default()
                },
            )
            .unwrap();

        let direct = make_deps(&[("a", "^1.0"), ("b", "^1.0")]);
        let resolved = registry.resolve("root", "0.1.0", &direct).unwrap();

        let unified = registry.unify_features(&BTreeMap::new(), &resolved);

        let shared_features = &unified["shared"];
        assert!(shared_features.contains("json"));
        assert!(shared_features.contains("xml"));
        assert!(shared_features.contains("yaml"));
        assert_eq!(shared_features.len(), 3);
    }

    #[test]
    fn feature_unification_includes_root_features() {
        let mut registry = PackageRegistry::new();
        registry.register("foo", "1.0.0", BTreeMap::new()).unwrap();

        let direct = make_deps(&[("foo", "^1.0")]);
        let resolved = registry.resolve("root", "0.1.0", &direct).unwrap();

        let root_features = BTreeMap::from([("foo".into(), vec!["extra".into(), "debug".into()])]);
        let unified = registry.unify_features(&root_features, &resolved);

        let foo_features = &unified["foo"];
        assert!(foo_features.contains("extra"));
        assert!(foo_features.contains("debug"));
    }

    #[test]
    fn feature_unification_empty_when_no_features() {
        let mut registry = PackageRegistry::new();
        registry.register("foo", "1.0.0", BTreeMap::new()).unwrap();

        let direct = make_deps(&[("foo", "^1.0")]);
        let resolved = registry.resolve("root", "0.1.0", &direct).unwrap();

        let unified = registry.unify_features(&BTreeMap::new(), &resolved);
        assert!(unified.is_empty());
    }

    // --- M-094: Transitive deps private ---

    #[test]
    fn transitive_deps_are_private() {
        let mut registry = PackageRegistry::new();
        registry.register("c", "1.0.0", BTreeMap::new()).unwrap();
        registry
            .register("b", "1.0.0", make_deps(&[("c", "^1.0")]))
            .unwrap();
        registry
            .register("a", "1.0.0", make_deps(&[("b", "^1.0")]))
            .unwrap();

        let direct = make_deps(&[("a", "^1.0")]);
        let resolved = registry.resolve("root", "0.1.0", &direct).unwrap();

        let visibility = compute_visibility(&direct, &resolved);

        // Direct dep is public
        assert_eq!(visibility["a"], DepVisibility::Public);
        // Transitive deps are private
        assert_eq!(visibility["b"], DepVisibility::Private);
        assert_eq!(visibility["c"], DepVisibility::Private);
    }

    #[test]
    fn direct_deps_are_public() {
        let mut registry = PackageRegistry::new();
        registry.register("foo", "1.0.0", BTreeMap::new()).unwrap();
        registry.register("bar", "1.0.0", BTreeMap::new()).unwrap();

        let direct = make_deps(&[("foo", "^1.0"), ("bar", "^1.0")]);
        let resolved = registry.resolve("root", "0.1.0", &direct).unwrap();

        let visibility = compute_visibility(&direct, &resolved);

        assert_eq!(visibility["foo"], DepVisibility::Public);
        assert_eq!(visibility["bar"], DepVisibility::Public);
    }

    #[test]
    fn visibility_mixed_direct_and_transitive() {
        let mut registry = PackageRegistry::new();
        // "shared" is both a direct dep and pulled transitively
        registry
            .register("shared", "1.0.0", BTreeMap::new())
            .unwrap();
        registry
            .register("lib", "1.0.0", make_deps(&[("shared", "^1.0")]))
            .unwrap();

        // Root depends on both "lib" and "shared" directly
        let direct = make_deps(&[("lib", "^1.0"), ("shared", "^1.0")]);
        let resolved = registry.resolve("root", "0.1.0", &direct).unwrap();

        let visibility = compute_visibility(&direct, &resolved);

        // Both are direct deps, so both are public
        assert_eq!(visibility["lib"], DepVisibility::Public);
        assert_eq!(visibility["shared"], DepVisibility::Public);
    }
}
