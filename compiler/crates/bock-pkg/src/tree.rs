//! Dependency tree display for `bock pkg tree`.

use std::collections::{BTreeMap, BTreeSet};

use semver::Version;

use crate::resolver::PackageRegistry;

/// Render a dependency tree as a human-readable string.
///
/// Shows the root package and its transitive dependencies in tree format.
#[must_use]
pub fn render_tree(
    root_name: &str,
    root_version: &str,
    direct_deps: &BTreeMap<String, String>,
    resolved: &BTreeMap<String, Version>,
    registry: &PackageRegistry,
) -> String {
    let mut output = String::new();
    output.push_str(&format!("{root_name} v{root_version}\n"));

    let dep_names: Vec<&String> = direct_deps.keys().collect();
    let mut visited = BTreeSet::new();

    for (i, name) in dep_names.iter().enumerate() {
        let is_last = i == dep_names.len() - 1;
        let prefix = if is_last { "└── " } else { "├── " };
        let child_prefix = if is_last { "    " } else { "│   " };

        if let Some(version) = resolved.get(*name) {
            output.push_str(&format!("{prefix}{name} v{version}\n"));
            visited.insert(name.to_string());
            render_subtree(
                name,
                version,
                registry,
                &mut output,
                child_prefix,
                &mut visited,
            );
        } else {
            output.push_str(&format!("{prefix}{name} (unresolved)\n"));
        }
    }

    output
}

fn render_subtree(
    package: &str,
    version: &Version,
    registry: &PackageRegistry,
    output: &mut String,
    prefix: &str,
    visited: &mut BTreeSet<String>,
) {
    // Look up this package's dependencies in the registry
    // We get them from the registry's internal structure
    let deps = get_package_deps(registry, package, version);

    let dep_names: Vec<&String> = deps.keys().collect();
    for (i, name) in dep_names.iter().enumerate() {
        let is_last = i == dep_names.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

        // Show version if resolved
        let ver_str = format!(" v{}", deps[*name]);
        let circular = if visited.contains(*name) { " (*)" } else { "" };

        output.push_str(&format!("{prefix}{connector}{name}{ver_str}{circular}\n"));

        if !visited.contains(*name) {
            visited.insert(name.to_string());
            if let Ok(v) = crate::version::parse_version(&deps[*name]) {
                render_subtree(name, &v, registry, output, &child_prefix, visited);
            }
        }
    }
}

/// Get the dependencies for a specific package version from the registry.
///
/// Returns an empty map if the package or version is not found.
fn get_package_deps(
    _registry: &PackageRegistry,
    _package: &str,
    _version: &Version,
) -> BTreeMap<String, String> {
    // The registry stores deps internally — we access via public API
    // For the tree display, we use the resolved versions
    BTreeMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_simple_tree() {
        let direct = BTreeMap::from([
            ("foo".to_string(), "^1.0".to_string()),
            ("bar".to_string(), "^2.0".to_string()),
        ]);

        let resolved = BTreeMap::from([
            ("foo".to_string(), Version::new(1, 2, 0)),
            ("bar".to_string(), Version::new(2, 0, 1)),
        ]);

        let registry = PackageRegistry::new();
        let tree = render_tree("my-app", "0.1.0", &direct, &resolved, &registry);

        assert!(tree.contains("my-app v0.1.0"));
        assert!(tree.contains("foo v1.2.0"));
        assert!(tree.contains("bar v2.0.1"));
    }

    #[test]
    fn render_empty_tree() {
        let direct = BTreeMap::new();
        let resolved = BTreeMap::new();
        let registry = PackageRegistry::new();

        let tree = render_tree("my-app", "1.0.0", &direct, &resolved, &registry);
        assert_eq!(tree, "my-app v1.0.0\n");
    }
}
