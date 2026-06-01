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

        // Iterate roots in a stable (sorted) order. The result of `has_cycle`
        // does not depend on iteration order, but iterating the `HashMap` keys
        // directly is needless non-determinism; sorting keeps behavior
        // independent of the per-process hash seed.
        for module_id in self.sorted_module_ids() {
            if self.dfs_cycle_check(module_id, &mut visited, &mut in_stack) {
                return true;
            }
        }
        false
    }

    /// Returns a topological ordering of modules (dependencies before dependents).
    ///
    /// Returns `None` if the graph contains a cycle.
    ///
    /// The ordering is **deterministic**: it does not depend on the per-process
    /// `HashMap`/`HashSet` random seed. Root modules are visited in sorted
    /// (module-id) order, and each node's dependencies are visited in sorted
    /// order, so for mutually-independent modules ties are broken by module id.
    /// This makes the order — and every downstream consumer that registers or
    /// emits modules in this order — byte-stable run-to-run.
    #[must_use]
    pub fn topological_order(&self) -> Option<Vec<ModuleId>> {
        let mut visited = HashSet::new();
        let mut in_stack = HashSet::new();
        let mut order = Vec::new();

        for module_id in self.sorted_module_ids() {
            if !self.topo_dfs(module_id, &mut visited, &mut in_stack, &mut order) {
                return None;
            }
        }

        // Post-order DFS with edges pointing dependent→dependency
        // naturally produces dependencies before dependents.
        Some(order)
    }

    /// Returns all module ids as a snapshot sorted by id.
    ///
    /// Used as the iteration order for the DFS roots so that the topological
    /// order (and cycle detection) is independent of the `HashMap` hash seed.
    fn sorted_module_ids(&self) -> Vec<&ModuleId> {
        let mut ids: Vec<&ModuleId> = self.edges.keys().collect();
        ids.sort_unstable();
        ids
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

        for dep in sorted_deps(self.edges.get(node)) {
            if self.dfs_cycle_check(dep, visited, in_stack) {
                return true;
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

        // Visit dependencies in sorted order so that, for a node with multiple
        // independent dependencies, the post-order emission (and thus the
        // overall topological order) is stable run-to-run rather than tied to
        // the `HashSet` hash seed.
        for dep in sorted_deps(self.edges.get(node)) {
            if !self.topo_dfs(dep, visited, in_stack, order) {
                return false;
            }
        }

        in_stack.remove(node);
        order.push(node.to_string());
        true
    }
}

/// Returns the dependency ids of a node as a snapshot sorted by id.
///
/// Iterating a `HashSet` directly yields the per-process hash-seed order, which
/// would make the DFS — and therefore the topological order — non-deterministic
/// whenever a node has multiple independent dependencies. Sorting a snapshot
/// breaks ties by module id for a stable, reproducible traversal.
fn sorted_deps(deps: Option<&HashSet<ModuleId>>) -> Vec<&ModuleId> {
    let mut deps: Vec<&ModuleId> = deps.into_iter().flatten().collect();
    deps.sort_unstable();
    deps
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

/// Augments a user module's dependency set with the implicit prelude
/// dependencies: every embedded core (`is_stdlib`) module.
///
/// The §18.2 prelude makes core-defined symbols (`Ordering`, `Comparable`,
/// `Into`, …) available in every module without an explicit `use`. To seed
/// those symbols from the registry, the core modules that define them must be
/// compiled and registered *before* any user module. These implicit edges
/// encode that ordering in the dependency graph, so the topological sort always
/// places the core modules first.
///
/// `core_module_ids` is the set of embedded core module ids; `self_id` is the
/// id of the module whose deps are being augmented (excluded to avoid a
/// self-edge for a core module). For stdlib modules themselves pass an empty
/// `core_module_ids` (or simply do not call this) so they keep only their own
/// explicit edges and cannot form a prelude self-cycle.
pub fn add_prelude_deps(deps: &mut HashSet<ModuleId>, self_id: &str, core_module_ids: &[ModuleId]) {
    for core_id in core_module_ids {
        if core_id != self_id {
            deps.insert(core_id.clone());
        }
    }
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

    /// Builds a graph from `(id, deps)` pairs in the given input order.
    fn build_graph(modules: &[(&str, &[&str])]) -> DepGraph {
        let mut graph = DepGraph::new();
        for (id, deps) in modules {
            let dep_set: HashSet<ModuleId> = deps.iter().map(|d| (*d).to_string()).collect();
            graph.add_module_with_deps((*id).to_string(), dep_set);
        }
        graph
    }

    #[test]
    fn topological_order_is_stable_across_repeated_calls() {
        // Seven mutually-independent modules — the case that previously yielded a
        // different `HashMap`-seed-dependent order on each call. Repeated calls on
        // the same graph must now return byte-identical orders.
        let graph = build_graph(&[
            ("core.iter", &[]),
            ("core.option", &[]),
            ("core.result", &[]),
            ("core.cmp", &[]),
            ("core.convert", &[]),
            ("core.num", &[]),
            ("core.str", &[]),
        ]);

        let first = graph.topological_order().unwrap();
        for _ in 0..64 {
            assert_eq!(graph.topological_order().unwrap(), first);
        }
        // Independent modules are emitted in sorted id order.
        let mut sorted = first.clone();
        sorted.sort();
        assert_eq!(first, sorted);
    }

    #[test]
    fn topological_order_is_independent_of_input_permutation() {
        // The same logical graph, fed in several different insertion orders, must
        // produce the same topological order. This is the property that makes
        // module-registration / diagnostics / codegen order reproducible
        // regardless of the order files happen to be discovered.
        let permutations: &[&[(&str, &[&str])]] = &[
            &[
                ("a", &[]),
                ("b", &[]),
                ("c", &[]),
                ("d", &["a", "b"]),
                ("e", &["b", "c"]),
            ],
            &[
                ("e", &["c", "b"]),
                ("d", &["b", "a"]),
                ("c", &[]),
                ("b", &[]),
                ("a", &[]),
            ],
            &[
                ("c", &[]),
                ("a", &[]),
                ("e", &["b", "c"]),
                ("b", &[]),
                ("d", &["a", "b"]),
            ],
            &[
                ("b", &[]),
                ("d", &["b", "a"]),
                ("a", &[]),
                ("e", &["c", "b"]),
                ("c", &[]),
            ],
        ];

        let expected = build_graph(permutations[0]).topological_order().unwrap();
        for perm in permutations {
            let order = build_graph(perm).topological_order().unwrap();
            assert_eq!(
                order, expected,
                "topological order changed with input permutation {perm:?}"
            );
            // Topological correctness: every dependency precedes its dependent.
            let pos = |m: &str| order.iter().position(|x| x == m).unwrap();
            assert!(pos("a") < pos("d"));
            assert!(pos("b") < pos("d"));
            assert!(pos("b") < pos("e"));
            assert!(pos("c") < pos("e"));
        }
    }

    #[test]
    fn has_cycle_is_stable_across_permutations() {
        // `has_cycle` must give the same answer regardless of insertion order or
        // hash seed — both for an acyclic graph and one with a cycle.
        let acyclic: &[(&str, &[&str])] = &[("a", &[]), ("b", &["a"]), ("c", &["a", "b"])];
        let cyclic: &[(&str, &[&str])] = &[("a", &["c"]), ("b", &["a"]), ("c", &["b"])];

        for _ in 0..16 {
            assert!(!build_graph(acyclic).has_cycle());
            assert!(build_graph(cyclic).has_cycle());
        }
    }
}
