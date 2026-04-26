//! Module dependency graph construction.
//!
//! Builds a directed graph of module dependencies by inspecting import declarations
//! in parsed AST modules. Each node is a module path, and edges represent "depends on"
//! relationships extracted from `use` declarations.

use std::collections::{HashMap, HashSet};

use bock_ast::{ImportDecl, Module, ModulePath};

/// A module identifier derived from its path segments (e.g., `"Std.Io.File"`).
pub type ModuleId = String;

/// Directed dependency graph where edges go from a module to its dependencies.
#[derive(Debug, Clone, Default)]
pub struct DepGraph {
    /// Map from module ID to the set of module IDs it depends on.
    edges: HashMap<ModuleId, HashSet<ModuleId>>,
    /// Reverse edges: module ID → set of modules that depend on it.
    reverse_edges: HashMap<ModuleId, HashSet<ModuleId>>,
}

impl DepGraph {
    /// Creates a new empty dependency graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a dependency graph from a collection of parsed modules.
    ///
    /// Each module's import declarations are inspected to determine dependencies.
    /// Modules without a declared path are assigned a synthetic ID based on their
    /// index in the input slice.
    #[must_use]
    pub fn from_modules(modules: &[Module]) -> Self {
        let mut graph = Self::new();
        for (i, module) in modules.iter().enumerate() {
            let module_id = module_id_from_module(module, i);
            let deps = extract_dependencies(&module.imports);
            graph.add_module_with_deps(module_id, deps);
        }
        graph
    }

    /// Adds a module and its dependencies to the graph.
    pub fn add_module_with_deps(&mut self, module_id: ModuleId, deps: HashSet<ModuleId>) {
        for dep in &deps {
            self.reverse_edges
                .entry(dep.clone())
                .or_default()
                .insert(module_id.clone());
        }
        self.edges.insert(module_id, deps);
    }

    /// Adds a single module with no dependencies.
    pub fn add_module(&mut self, module_id: ModuleId) {
        self.edges.entry(module_id).or_default();
    }

    /// Returns the direct dependencies of a module.
    #[must_use]
    pub fn dependencies(&self, module_id: &str) -> Option<&HashSet<ModuleId>> {
        self.edges.get(module_id)
    }

    /// Returns the modules that directly depend on the given module (reverse deps).
    #[must_use]
    pub fn dependents(&self, module_id: &str) -> Option<&HashSet<ModuleId>> {
        self.reverse_edges.get(module_id)
    }

    /// Returns all module IDs in the graph.
    #[must_use]
    pub fn modules(&self) -> Vec<&ModuleId> {
        self.edges.keys().collect()
    }

    /// Returns the number of modules in the graph.
    #[must_use]
    pub fn module_count(&self) -> usize {
        self.edges.len()
    }

    /// Returns true if the graph contains cycles.
    #[must_use]
    pub fn has_cycle(&self) -> bool {
        let mut visited = HashSet::new();
        let mut in_stack = HashSet::new();

        for module_id in self.edges.keys() {
            if self.dfs_cycle_check(module_id, &mut visited, &mut in_stack) {
                return true;
            }
        }
        false
    }

    /// Returns a topological ordering of modules (dependencies before dependents).
    ///
    /// Returns `None` if the graph contains a cycle.
    #[must_use]
    pub fn topological_order(&self) -> Option<Vec<ModuleId>> {
        let mut visited = HashSet::new();
        let mut in_stack = HashSet::new();
        let mut order = Vec::new();

        for module_id in self.edges.keys() {
            if !self.topo_dfs(module_id, &mut visited, &mut in_stack, &mut order) {
                return None;
            }
        }

        // Post-order DFS with edges pointing dependent→dependency
        // naturally produces dependencies before dependents.
        Some(order)
    }

    fn dfs_cycle_check(
        &self,
        node: &str,
        visited: &mut HashSet<String>,
        in_stack: &mut HashSet<String>,
    ) -> bool {
        if in_stack.contains(node) {
            return true;
        }
        if visited.contains(node) {
            return false;
        }

        visited.insert(node.to_string());
        in_stack.insert(node.to_string());

        if let Some(deps) = self.edges.get(node) {
            for dep in deps {
                if self.dfs_cycle_check(dep, visited, in_stack) {
                    return true;
                }
            }
        }

        in_stack.remove(node);
        false
    }

    fn topo_dfs(
        &self,
        node: &str,
        visited: &mut HashSet<String>,
        in_stack: &mut HashSet<String>,
        order: &mut Vec<String>,
    ) -> bool {
        if in_stack.contains(node) {
            return false; // cycle
        }
        if visited.contains(node) {
            return true;
        }

        visited.insert(node.to_string());
        in_stack.insert(node.to_string());

        if let Some(deps) = self.edges.get(node) {
            for dep in deps {
                if !self.topo_dfs(dep, visited, in_stack, order) {
                    return false;
                }
            }
        }

        in_stack.remove(node);
        order.push(node.to_string());
        true
    }
}

/// Extracts a module ID string from a parsed module.
///
/// Uses the module's declared path if available, otherwise generates a synthetic
/// ID like `"<anonymous_0>"`.
#[must_use]
pub fn module_id_from_module(module: &Module, index: usize) -> ModuleId {
    module
        .path
        .as_ref()
        .map(module_path_to_id)
        .unwrap_or_else(|| format!("<anonymous_{index}>"))
}

/// Converts a `ModulePath` to a dot-separated string ID.
#[must_use]
pub fn module_path_to_id(path: &ModulePath) -> ModuleId {
    path.segments
        .iter()
        .map(|ident| ident.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// Extracts dependency module IDs from a list of import declarations.
#[must_use]
pub fn extract_dependencies(imports: &[ImportDecl]) -> HashSet<ModuleId> {
    imports
        .iter()
        .map(|import| module_path_to_id(&import.path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_graph() {
        let graph = DepGraph::new();
        assert_eq!(graph.module_count(), 0);
        assert!(!graph.has_cycle());
        assert_eq!(graph.topological_order(), Some(vec![]));
    }

    #[test]
    fn single_module_no_deps() {
        let mut graph = DepGraph::new();
        graph.add_module("Main".to_string());
        assert_eq!(graph.module_count(), 1);
        assert!(graph.dependencies("Main").unwrap().is_empty());
        assert!(!graph.has_cycle());
    }

    #[test]
    fn linear_dependency_chain() {
        let mut graph = DepGraph::new();
        graph.add_module_with_deps("App".to_string(), HashSet::from(["Lib".to_string()]));
        graph.add_module_with_deps("Lib".to_string(), HashSet::from(["Core".to_string()]));
        graph.add_module("Core".to_string());

        assert_eq!(graph.module_count(), 3);
        assert!(!graph.has_cycle());

        let order = graph.topological_order().unwrap();
        let core_pos = order.iter().position(|m| m == "Core").unwrap();
        let lib_pos = order.iter().position(|m| m == "Lib").unwrap();
        let app_pos = order.iter().position(|m| m == "App").unwrap();
        assert!(core_pos < lib_pos);
        assert!(lib_pos < app_pos);
    }

    #[test]
    fn cycle_detection() {
        let mut graph = DepGraph::new();
        graph.add_module_with_deps("A".to_string(), HashSet::from(["B".to_string()]));
        graph.add_module_with_deps("B".to_string(), HashSet::from(["A".to_string()]));

        assert!(graph.has_cycle());
        assert!(graph.topological_order().is_none());
    }

    #[test]
    fn reverse_dependencies() {
        let mut graph = DepGraph::new();
        graph.add_module_with_deps("App".to_string(), HashSet::from(["Lib".to_string()]));
        graph.add_module_with_deps("Tests".to_string(), HashSet::from(["Lib".to_string()]));
        graph.add_module("Lib".to_string());

        let dependents = graph.dependents("Lib").unwrap();
        assert!(dependents.contains("App"));
        assert!(dependents.contains("Tests"));
        assert_eq!(dependents.len(), 2);
    }

    #[test]
    fn diamond_dependency() {
        let mut graph = DepGraph::new();
        graph.add_module_with_deps(
            "App".to_string(),
            HashSet::from(["Left".to_string(), "Right".to_string()]),
        );
        graph.add_module_with_deps("Left".to_string(), HashSet::from(["Base".to_string()]));
        graph.add_module_with_deps("Right".to_string(), HashSet::from(["Base".to_string()]));
        graph.add_module("Base".to_string());

        assert!(!graph.has_cycle());
        let order = graph.topological_order().unwrap();
        let base_pos = order.iter().position(|m| m == "Base").unwrap();
        let app_pos = order.iter().position(|m| m == "App").unwrap();
        assert!(base_pos < app_pos);
    }
}
