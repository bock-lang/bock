//! Minimal rebuild set computation.
//!
//! Given a set of directly changed modules (from content hash comparison) and
//! the module dependency graph, computes the minimal set of modules that need
//! to be rebuilt. This includes the changed modules plus all transitive
//! dependents (modules that import from changed modules, directly or indirectly).

use std::collections::HashSet;

use crate::dep_graph::DepGraph;

/// Computes the minimal set of modules that must be rebuilt.
///
/// Starting from `changed_modules` (those whose source content changed),
/// computes the transitive closure of reverse dependencies — i.e., all modules
/// that directly or indirectly depend on a changed module.
///
/// The result always includes the `changed_modules` themselves.
#[must_use]
pub fn compute_rebuild_set(graph: &DepGraph, changed_modules: &[String]) -> HashSet<String> {
    let mut rebuild_set = HashSet::new();
    let mut worklist: Vec<String> = changed_modules.to_vec();

    while let Some(module_id) = worklist.pop() {
        if !rebuild_set.insert(module_id.clone()) {
            continue; // already visited
        }

        // Add all modules that depend on this one
        if let Some(dependents) = graph.dependents(&module_id) {
            for dep in dependents {
                if !rebuild_set.contains(dep) {
                    worklist.push(dep.clone());
                }
            }
        }
    }

    rebuild_set
}

/// Computes the rebuild set and returns it in topological order
/// (dependencies before dependents), suitable for ordered recompilation.
///
/// Returns `None` if the dependency graph contains a cycle.
#[must_use]
pub fn ordered_rebuild_set(graph: &DepGraph, changed_modules: &[String]) -> Option<Vec<String>> {
    let rebuild_set = compute_rebuild_set(graph, changed_modules);
    let topo_order = graph.topological_order()?;

    Some(
        topo_order
            .into_iter()
            .filter(|m| rebuild_set.contains(m))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chain_graph() -> DepGraph {
        // Core <- Lib <- App
        let mut graph = DepGraph::new();
        graph.add_module("Core".to_string());
        graph.add_module_with_deps("Lib".to_string(), HashSet::from(["Core".to_string()]));
        graph.add_module_with_deps("App".to_string(), HashSet::from(["Lib".to_string()]));
        graph
    }

    #[test]
    fn rebuild_leaf_only() {
        let graph = make_chain_graph();
        let rebuild = compute_rebuild_set(&graph, &["App".to_string()]);
        // App has no dependents, so only App needs rebuilding
        assert_eq!(rebuild, HashSet::from(["App".to_string()]));
    }

    #[test]
    fn rebuild_root_cascades() {
        let graph = make_chain_graph();
        let rebuild = compute_rebuild_set(&graph, &["Core".to_string()]);
        // Everything depends on Core transitively
        assert_eq!(
            rebuild,
            HashSet::from(["Core".to_string(), "Lib".to_string(), "App".to_string()])
        );
    }

    #[test]
    fn rebuild_middle_cascades_partially() {
        let graph = make_chain_graph();
        let rebuild = compute_rebuild_set(&graph, &["Lib".to_string()]);
        assert_eq!(
            rebuild,
            HashSet::from(["Lib".to_string(), "App".to_string()])
        );
    }

    #[test]
    fn rebuild_nothing_changed() {
        let graph = make_chain_graph();
        let rebuild = compute_rebuild_set(&graph, &[]);
        assert!(rebuild.is_empty());
    }

    #[test]
    fn rebuild_unknown_module() {
        let graph = make_chain_graph();
        let rebuild = compute_rebuild_set(&graph, &["Unknown".to_string()]);
        // Unknown module has no dependents, just itself
        assert_eq!(rebuild, HashSet::from(["Unknown".to_string()]));
    }

    #[test]
    fn ordered_rebuild_respects_topo_order() {
        let graph = make_chain_graph();
        let order = ordered_rebuild_set(&graph, &["Core".to_string()]).unwrap();
        let core_pos = order.iter().position(|m| m == "Core").unwrap();
        let lib_pos = order.iter().position(|m| m == "Lib").unwrap();
        let app_pos = order.iter().position(|m| m == "App").unwrap();
        assert!(core_pos < lib_pos);
        assert!(lib_pos < app_pos);
    }

    #[test]
    fn diamond_rebuild() {
        let mut graph = DepGraph::new();
        graph.add_module("Base".to_string());
        graph.add_module_with_deps("Left".to_string(), HashSet::from(["Base".to_string()]));
        graph.add_module_with_deps("Right".to_string(), HashSet::from(["Base".to_string()]));
        graph.add_module_with_deps(
            "Top".to_string(),
            HashSet::from(["Left".to_string(), "Right".to_string()]),
        );

        let rebuild = compute_rebuild_set(&graph, &["Base".to_string()]);
        assert_eq!(
            rebuild,
            HashSet::from([
                "Base".to_string(),
                "Left".to_string(),
                "Right".to_string(),
                "Top".to_string()
            ])
        );
    }

    #[test]
    fn diamond_partial_rebuild() {
        let mut graph = DepGraph::new();
        graph.add_module("Base".to_string());
        graph.add_module_with_deps("Left".to_string(), HashSet::from(["Base".to_string()]));
        graph.add_module_with_deps("Right".to_string(), HashSet::from(["Base".to_string()]));
        graph.add_module_with_deps(
            "Top".to_string(),
            HashSet::from(["Left".to_string(), "Right".to_string()]),
        );

        let rebuild = compute_rebuild_set(&graph, &["Left".to_string()]);
        assert_eq!(
            rebuild,
            HashSet::from(["Left".to_string(), "Top".to_string()])
        );
    }
}
