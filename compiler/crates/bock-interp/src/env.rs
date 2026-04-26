//! Lexical environment (scope stack) for the Bock interpreter.

use std::collections::HashMap;

use crate::value::Value;

/// A single scope frame: maps variable names to their current values.
type Frame = HashMap<String, Value>;

/// Nested lexical scopes for variable bindings.
///
/// The innermost scope is at the back of the `scopes` vec. Variable lookup
/// walks from inner to outer, finding the nearest binding.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    scopes: Vec<Frame>,
}

impl Environment {
    /// Create an environment with a single (global) scope.
    #[must_use]
    pub fn new() -> Self {
        Self {
            scopes: vec![Frame::new()],
        }
    }

    /// Push a new inner scope.
    pub fn push_scope(&mut self) {
        self.scopes.push(Frame::new());
    }

    /// Pop the innermost scope. Does nothing if only the global scope remains.
    pub fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    /// Define (or redefine) a variable in the current (innermost) scope.
    pub fn define(&mut self, name: impl Into<String>, value: Value) {
        if let Some(frame) = self.scopes.last_mut() {
            frame.insert(name.into(), value);
        }
    }

    /// Look up a variable, searching from innermost to outermost scope.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        for frame in self.scopes.iter().rev() {
            if let Some(v) = frame.get(name) {
                return Some(v);
            }
        }
        None
    }

    /// Return all bindings visible in the current scope (inner scopes shadow outer).
    #[must_use]
    pub fn all_bindings(&self) -> Vec<(String, Value)> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for frame in self.scopes.iter().rev() {
            for (name, value) in frame {
                if seen.insert(name.clone()) {
                    result.push((name.clone(), value.clone()));
                }
            }
        }
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    /// Assign to an existing variable in the nearest enclosing scope that
    /// contains it. Returns `false` if no binding was found.
    pub fn assign(&mut self, name: &str, value: Value) -> bool {
        for frame in self.scopes.iter_mut().rev() {
            if frame.contains_key(name) {
                frame.insert(name.to_string(), value);
                return true;
            }
        }
        false
    }
}

/// A key identifying an effect + operation pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectOpKey {
    /// The effect name (e.g. `"Log"`).
    pub effect: String,
    /// The operation name (e.g. `"log"`).
    pub operation: String,
}

/// A single handler frame pushed by a `handling` block.
///
/// Maps effect names to `Value::Function` handler values.
type HandlerFrame = HashMap<String, Value>;

/// Three-layer algebraic effect handler stack.
///
/// Resolution order (innermost wins):
/// 1. **Local** — pushed by `handling` blocks (dynamic stack)
/// 2. **Module** — registered via `handle Effect with handler`
/// 3. **Project** — global defaults from configuration
#[derive(Debug, Clone, Default)]
pub struct EffectStack {
    /// Local handler stack: each entry maps effect names to handler values.
    /// The back of the vec is the innermost (most recent) `handling` block.
    local: Vec<HandlerFrame>,
    /// Module-level handlers: `handle Effect with handler`.
    module: HashMap<String, Value>,
    /// Project-level default handlers.
    project: HashMap<String, Value>,
}

impl EffectStack {
    /// Create an empty effect stack.
    #[must_use]
    pub fn new() -> Self {
        Self {
            local: Vec::new(),
            module: HashMap::new(),
            project: HashMap::new(),
        }
    }

    /// Returns `true` if no handlers are registered at any level.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.local.is_empty() && self.module.is_empty() && self.project.is_empty()
    }

    /// Push a new handler frame for a `handling` block.
    pub fn push_handlers(&mut self, handlers: HashMap<String, Value>) {
        self.local.push(handlers);
    }

    /// Pop the most recent handler frame (when leaving a `handling` block).
    pub fn pop_handlers(&mut self) {
        self.local.pop();
    }

    /// Register a module-level handler for an effect.
    pub fn set_module_handler(&mut self, effect_name: impl Into<String>, handler: Value) {
        self.module.insert(effect_name.into(), handler);
    }

    /// Register a project-level default handler for an effect.
    pub fn set_project_handler(&mut self, effect_name: impl Into<String>, handler: Value) {
        self.project.insert(effect_name.into(), handler);
    }

    /// Resolve a handler for the given effect using three-layer resolution.
    ///
    /// Returns `None` if no handler is registered at any level.
    #[must_use]
    pub fn resolve(&self, effect_name: &str) -> Option<&Value> {
        // Layer 1: local (innermost handling block wins)
        for frame in self.local.iter().rev() {
            if let Some(handler) = frame.get(effect_name) {
                return Some(handler);
            }
        }
        // Layer 2: module
        if let Some(handler) = self.module.get(effect_name) {
            return Some(handler);
        }
        // Layer 3: project
        self.project.get(effect_name)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn define_and_get() {
        let mut env = Environment::new();
        env.define("x", Value::Int(42));
        assert_eq!(env.get("x"), Some(&Value::Int(42)));
    }

    #[test]
    fn inner_scope_shadows_outer() {
        let mut env = Environment::new();
        env.define("x", Value::Int(1));
        env.push_scope();
        env.define("x", Value::Int(2));
        assert_eq!(env.get("x"), Some(&Value::Int(2)));
        env.pop_scope();
        assert_eq!(env.get("x"), Some(&Value::Int(1)));
    }

    #[test]
    fn lookup_outer_from_inner() {
        let mut env = Environment::new();
        env.define("y", Value::Bool(true));
        env.push_scope();
        assert_eq!(env.get("y"), Some(&Value::Bool(true)));
        env.pop_scope();
    }

    #[test]
    fn assign_updates_nearest_binding() {
        let mut env = Environment::new();
        env.define("z", Value::Int(0));
        env.push_scope();
        let updated = env.assign("z", Value::Int(99));
        assert!(updated);
        env.pop_scope();
        assert_eq!(env.get("z"), Some(&Value::Int(99)));
    }

    #[test]
    fn assign_returns_false_for_unknown() {
        let mut env = Environment::new();
        assert!(!env.assign("nope", Value::Void));
    }

    #[test]
    fn pop_global_scope_is_noop() {
        let mut env = Environment::new();
        env.define("k", Value::Int(7));
        env.pop_scope(); // should not remove the global scope
        assert_eq!(env.get("k"), Some(&Value::Int(7)));
    }

    // ── EffectStack tests ────────────────────────────────────────────────

    #[test]
    fn effect_stack_resolve_returns_none_when_empty() {
        let stack = EffectStack::new();
        assert!(stack.resolve("Log").is_none());
    }

    #[test]
    fn effect_stack_project_layer() {
        let mut stack = EffectStack::new();
        stack.set_project_handler("Log", Value::Int(1));
        assert_eq!(stack.resolve("Log"), Some(&Value::Int(1)));
    }

    #[test]
    fn effect_stack_module_overrides_project() {
        let mut stack = EffectStack::new();
        stack.set_project_handler("Log", Value::Int(1));
        stack.set_module_handler("Log", Value::Int(2));
        assert_eq!(stack.resolve("Log"), Some(&Value::Int(2)));
    }

    #[test]
    fn effect_stack_local_overrides_module() {
        let mut stack = EffectStack::new();
        stack.set_module_handler("Log", Value::Int(1));
        let mut frame = HashMap::new();
        frame.insert("Log".to_string(), Value::Int(2));
        stack.push_handlers(frame);
        assert_eq!(stack.resolve("Log"), Some(&Value::Int(2)));
    }

    #[test]
    fn effect_stack_innermost_local_wins() {
        let mut stack = EffectStack::new();
        let mut frame1 = HashMap::new();
        frame1.insert("Log".to_string(), Value::Int(1));
        stack.push_handlers(frame1);

        let mut frame2 = HashMap::new();
        frame2.insert("Log".to_string(), Value::Int(2));
        stack.push_handlers(frame2);

        assert_eq!(stack.resolve("Log"), Some(&Value::Int(2)));
    }

    #[test]
    fn effect_stack_pop_restores_outer() {
        let mut stack = EffectStack::new();
        let mut frame1 = HashMap::new();
        frame1.insert("Log".to_string(), Value::Int(1));
        stack.push_handlers(frame1);

        let mut frame2 = HashMap::new();
        frame2.insert("Log".to_string(), Value::Int(2));
        stack.push_handlers(frame2);

        stack.pop_handlers();
        assert_eq!(stack.resolve("Log"), Some(&Value::Int(1)));
    }

    #[test]
    fn effect_stack_different_effects_in_same_frame() {
        let mut stack = EffectStack::new();
        let mut frame = HashMap::new();
        frame.insert("Log".to_string(), Value::Int(1));
        frame.insert("Clock".to_string(), Value::Int(2));
        stack.push_handlers(frame);
        assert_eq!(stack.resolve("Log"), Some(&Value::Int(1)));
        assert_eq!(stack.resolve("Clock"), Some(&Value::Int(2)));
    }

    #[test]
    fn effect_stack_local_falls_through_to_module() {
        let mut stack = EffectStack::new();
        stack.set_module_handler("Clock", Value::Int(10));
        let mut frame = HashMap::new();
        frame.insert("Log".to_string(), Value::Int(1));
        stack.push_handlers(frame);
        // Log resolves from local
        assert_eq!(stack.resolve("Log"), Some(&Value::Int(1)));
        // Clock falls through to module
        assert_eq!(stack.resolve("Clock"), Some(&Value::Int(10)));
    }
}
