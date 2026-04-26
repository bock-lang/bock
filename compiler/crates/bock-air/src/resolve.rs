//! Name resolution pass — S-AIR layer.
//!
//! Resolves all identifier references in an AST [`Module`] to their
//! definitions, populating the provided [`SymbolTable`].  Diagnostics are
//! emitted for undefined names and unused imports.
//!
//! # Two-pass algorithm
//! 1. **Collect** — all top-level declarations and import bindings are entered
//!    into the module scope.
//! 2. **Resolve** — the AST is walked; every [`Expr::Identifier`] is looked up
//!    in the scope stack and recorded in [`SymbolTable::resolutions`].
//!
//! Inner scopes shadow outer ones (standard lexical scoping).

use std::collections::{HashMap, HashSet};

use bock_ast::{
    Block, EnumVariant, Expr, FnDecl, ForLoop, GuardStmt, HandlingBlock, ImplBlock, ImportItems,
    InterpolationPart, Item, LetStmt, LoopStmt, MatchArm, Module, ModulePath, NodeId, Param,
    Pattern, Stmt, Visibility, WhileLoop,
};
use bock_errors::{DiagnosticBag, DiagnosticCode, Span};

use crate::registry::{ExportDetail, ExportKind, ExportedSymbol, ModuleRegistry, RegistryError};

// ─── Diagnostic codes ─────────────────────────────────────────────────────────

const E_UNDEFINED: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 1001,
};
const E_MODULE_NOT_FOUND: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 1005,
};
const E_SYMBOL_NOT_FOUND: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 1006,
};
const E_NOT_VISIBLE: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 1007,
};
const W_UNUSED_IMPORT: DiagnosticCode = DiagnosticCode {
    prefix: 'W',
    number: 1001,
};

// ─── Public types ─────────────────────────────────────────────────────────────

/// Classification of what a resolved name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    Variable,
    Function,
    Type,
    Trait,
    Effect,
    Module,
    /// A prelude/builtin name (function, type, trait, or constructor)
    /// that is always in scope without an explicit import.
    Builtin,
    /// The actual kind is not yet known (e.g. a named import before
    /// the imported module has been analyzed).
    Unresolved,
}

/// A fully-resolved name reference: the definition site and its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedName {
    /// NodeId of the declaration this name refers to.
    pub def_id: NodeId,
    /// What kind of entity this name refers to.
    pub kind: NameKind,
}

/// A name binding within a [`Scope`].
#[derive(Debug, Clone)]
pub struct Binding {
    /// The name as written in source.
    pub name: String,
    /// The resolved entity.
    pub resolved: ResolvedName,
    /// Declared visibility of the definition.
    pub visibility: Visibility,
    /// Source span of the definition site.
    pub span: Span,
    /// Whether this binding has been referenced at least once.
    pub used: bool,
    /// `true` for bindings introduced by `use` import declarations.
    pub is_import: bool,
}

/// Information about an effect declaration's operations, stored during
/// the collection phase so that `with` clauses can inject operations
/// into function scopes.
#[derive(Debug, Clone, Default)]
pub struct EffectInfo {
    /// Direct operations declared in this effect: `(name, def_id, span)`.
    pub operations: Vec<(String, NodeId, Span)>,
    /// Component effect names (for composite effects like `effect IO = Log + Clock`).
    pub components: Vec<String>,
}

/// A single lexical scope (one entry on the scope stack).
#[derive(Debug, Default)]
pub struct Scope {
    /// Bindings defined in this scope, keyed by name.
    pub bindings: HashMap<String, Binding>,
}

impl Scope {
    fn new() -> Self {
        Self::default()
    }
}

/// A hierarchical symbol table: module scope → nested scopes → bindings.
///
/// The scope stack grows when blocks are entered and shrinks when they exit.
/// Lookups walk from the innermost scope outward; the first match wins
/// (lexical shadowing).
pub struct SymbolTable {
    /// Live scope stack.  Index 0 is the module (outermost) scope.
    scopes: Vec<Scope>,
    /// Resolution map: usage-site NodeId → resolved binding.
    pub resolutions: HashMap<NodeId, ResolvedName>,
    /// Effect declarations: effect name → operations and components.
    /// Populated during the collection phase, read during resolution to
    /// inject effect operations into `with`-annotated function scopes.
    pub effect_info: HashMap<String, EffectInfo>,
    /// Maps enum variant names to their parent enum import name.
    /// Used to propagate "used" status from variants to the enum import.
    variant_parent: HashMap<String, String>,
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolTable {
    /// Creates a new symbol table with one empty module scope.
    #[must_use]
    pub fn new() -> Self {
        Self {
            scopes: vec![Scope::new()],
            resolutions: HashMap::new(),
            effect_info: HashMap::new(),
            variant_parent: HashMap::new(),
        }
    }

    /// Seed the root scope with prelude/builtin names that are always
    /// available without an explicit import.
    ///
    /// Uses a synthetic `NodeId` range starting at `PRELUDE_BASE_ID` to
    /// avoid collisions with parser-assigned IDs.
    fn seed_prelude(&mut self) {
        const PRELUDE_BASE_ID: NodeId = u32::MAX / 4;

        use crate::prelude_vocab::{
            PRELUDE_CONSTRUCTORS, PRELUDE_FUNCTIONS as PRELUDE_FNS, PRELUDE_TRAITS, PRELUDE_TYPES,
        };

        let mut id = PRELUDE_BASE_ID;

        let dummy_span = Span {
            file: bock_errors::FileId(0),
            start: 0,
            end: 0,
        };

        for &name in PRELUDE_FNS {
            self.define(
                name.to_string(),
                Binding {
                    name: name.to_string(),
                    resolved: ResolvedName {
                        def_id: id,
                        kind: NameKind::Builtin,
                    },
                    visibility: Visibility::Public,
                    span: dummy_span,
                    used: true, // never warn about unused builtins
                    is_import: false,
                },
            );
            id += 1;
        }

        for &name in PRELUDE_TYPES {
            self.define(
                name.to_string(),
                Binding {
                    name: name.to_string(),
                    resolved: ResolvedName {
                        def_id: id,
                        kind: NameKind::Builtin,
                    },
                    visibility: Visibility::Public,
                    span: dummy_span,
                    used: true,
                    is_import: false,
                },
            );
            id += 1;
        }

        for &name in PRELUDE_CONSTRUCTORS {
            self.define(
                name.to_string(),
                Binding {
                    name: name.to_string(),
                    resolved: ResolvedName {
                        def_id: id,
                        kind: NameKind::Builtin,
                    },
                    visibility: Visibility::Public,
                    span: dummy_span,
                    used: true,
                    is_import: false,
                },
            );
            id += 1;
        }

        for &name in PRELUDE_TRAITS {
            self.define(
                name.to_string(),
                Binding {
                    name: name.to_string(),
                    resolved: ResolvedName {
                        def_id: id,
                        kind: NameKind::Builtin,
                    },
                    visibility: Visibility::Public,
                    span: dummy_span,
                    used: true,
                    is_import: false,
                },
            );
            id += 1;
        }
    }

    /// Defines `name` in the current (innermost) scope.
    pub fn define(&mut self, name: String, binding: Binding) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.bindings.insert(name, binding);
        }
    }

    /// Pushes a new empty scope onto the stack.
    pub fn push_scope(&mut self) {
        self.scopes.push(Scope::new());
    }

    /// Pops and returns the innermost scope.  Returns `None` if only the
    /// module scope remains (it is never popped).
    pub fn pop_scope(&mut self) -> Option<Scope> {
        if self.scopes.len() > 1 {
            self.scopes.pop()
        } else {
            None
        }
    }

    /// Looks up `name`, walking from innermost to outermost scope.
    ///
    /// Marks the binding as used and returns a clone of the resolved name.
    pub fn lookup(&mut self, name: &str) -> Option<ResolvedName> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(binding) = scope.bindings.get_mut(name) {
                binding.used = true;
                return Some(binding.resolved.clone());
            }
        }
        None
    }

    /// Marks the binding for `name` as used without returning a resolution.
    /// Used for type names that appear in constructor or annotation position.
    pub fn mark_used(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(binding) = scope.bindings.get_mut(name) {
                binding.used = true;
                return;
            }
        }
    }

    /// Immutable lookup — does **not** mark the binding as used.
    #[must_use]
    pub fn lookup_peek(&self, name: &str) -> Option<&Binding> {
        for scope in self.scopes.iter().rev() {
            if let Some(b) = scope.bindings.get(name) {
                return Some(b);
            }
        }
        None
    }

    /// Records that the node at `use_id` resolves to `resolved`.
    pub fn record_resolution(&mut self, use_id: NodeId, resolved: ResolvedName) {
        self.resolutions.insert(use_id, resolved);
    }

    /// Collects every name visible in the current scope stack.
    ///
    /// Used for "did you mean X?" suggestions when a lookup fails. Names
    /// from inner scopes shadow outer scopes, but for suggestion purposes
    /// we include all reachable binders — duplicates are de-duplicated.
    #[must_use]
    pub fn visible_names(&self) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for scope in self.scopes.iter().rev() {
            for name in scope.bindings.keys() {
                if seen.insert(name.clone()) {
                    out.push(name.clone());
                }
            }
        }
        out
    }

    /// Returns `true` if any wildcard (`*`) import exists in the module scope.
    #[must_use]
    pub fn has_wildcard_import(&self) -> bool {
        self.scopes
            .first()
            .map(|s| {
                s.bindings
                    .values()
                    .any(|b| b.is_import && b.name.ends_with(".*"))
            })
            .unwrap_or(false)
    }

    /// Returns all import bindings from the module scope that were never used.
    #[must_use]
    pub fn unused_imports(&self) -> Vec<&Binding> {
        self.scopes
            .first()
            .map(|s| {
                s.bindings
                    .values()
                    .filter(|b| b.is_import && !b.used)
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ─── Resolver (private) ───────────────────────────────────────────────────────

struct Resolver<'a> {
    symbols: &'a mut SymbolTable,
    diag: &'a mut DiagnosticBag,
    /// Counter for synthetic NodeIds (e.g. shorthand record pattern bindings).
    /// Starts high to avoid collision with parser-assigned IDs.
    synthetic_id: NodeId,
    /// Optional cross-file module registry for resolving imports.
    /// When `Some`, named and glob imports are resolved from registered modules.
    registry: Option<&'a ModuleRegistry>,
}

impl<'a> Resolver<'a> {
    fn new(symbols: &'a mut SymbolTable, diag: &'a mut DiagnosticBag) -> Self {
        Self {
            symbols,
            diag,
            synthetic_id: u32::MAX / 2,
            registry: None,
        }
    }

    /// Returns the next unique synthetic [`NodeId`].
    fn next_synthetic_id(&mut self) -> NodeId {
        let id = self.synthetic_id;
        self.synthetic_id += 1;
        id
    }

    // ── Module entry point ────────────────────────────────────────────────────

    fn resolve_module(&mut self, module: &Module) {
        // Pass 0: seed prelude/builtin names into the root scope.
        // These are shadowed by imports and local declarations.
        self.symbols.seed_prelude();
        // Pass 1a: imports first — local declarations can shadow them.
        self.collect_imports(module);
        // Pass 1b: collect all top-level declarations (shadows imports if same name).
        self.collect_items(&module.items);
        // Pass 2: resolve all identifier references.
        for item in &module.items {
            self.resolve_item(item);
        }
        // Pass 3: warn about unused imports.
        self.check_unused_imports();
    }

    // ── Collection passes ─────────────────────────────────────────────────────

    fn collect_imports(&mut self, module: &Module) {
        for import in &module.imports {
            let module_id = module_path_str(&import.path);
            match &import.items {
                ImportItems::Module => {
                    // `use foo.bar` — bind the last segment as a module alias.
                    let name = import
                        .path
                        .segments
                        .last()
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    self.symbols.define(
                        name.clone(),
                        Binding {
                            name,
                            resolved: ResolvedName {
                                def_id: import.id,
                                kind: NameKind::Module,
                            },
                            visibility: Visibility::Private,
                            span: import.span,
                            used: false,
                            is_import: true,
                        },
                    );
                }
                ImportItems::Named(names) => {
                    for imported in names {
                        let local = imported.alias.as_ref().unwrap_or(&imported.name);
                        // Try to resolve from the registry if available.
                        let kind = if let Some(registry) = self.registry {
                            match registry.resolve_symbol(&module_id, &imported.name.name) {
                                Ok(sym) => {
                                    // If this is an effect, seed effect_info so `with`
                                    // clauses can inject its operations into scope.
                                    if sym.kind == ExportKind::Effect {
                                        seed_effect_info_from_registry(
                                            self.symbols,
                                            &local.name,
                                            sym,
                                            import.id,
                                            import.span,
                                        );
                                    }
                                    // If this is an enum, also seed its variant
                                    // constructors so bare names like `Red` work.
                                    if sym.kind == ExportKind::Enum {
                                        seed_enum_variants_from_registry(
                                            self.symbols,
                                            &local.name,
                                            sym,
                                            import.id,
                                            import.span,
                                        );
                                    }
                                    export_kind_to_name_kind(sym.kind)
                                }
                                Err(RegistryError::ModuleNotFound { .. }) => {
                                    self.diag.error(
                                        E_MODULE_NOT_FOUND,
                                        format!("module `{module_id}` not found"),
                                        import.span,
                                    );
                                    NameKind::Unresolved
                                }
                                Err(RegistryError::SymbolNotFound { name, .. }) => {
                                    self.diag.error(
                                        E_SYMBOL_NOT_FOUND,
                                        format!(
                                            "`{name}` is not exported by module `{module_id}`"
                                        ),
                                        imported.span,
                                    );
                                    NameKind::Unresolved
                                }
                                Err(RegistryError::NotVisible { name, .. }) => {
                                    self.diag.error(
                                        E_NOT_VISIBLE,
                                        format!(
                                            "`{name}` in module `{module_id}` is private"
                                        ),
                                        imported.span,
                                    );
                                    NameKind::Unresolved
                                }
                            }
                        } else {
                            // No registry — single-file mode; kind is unknown.
                            NameKind::Unresolved
                        };
                        self.symbols.define(
                            local.name.clone(),
                            Binding {
                                name: local.name.clone(),
                                resolved: ResolvedName {
                                    def_id: import.id,
                                    kind,
                                },
                                visibility: Visibility::Private,
                                span: imported.span,
                                used: false,
                                is_import: true,
                            },
                        );
                    }
                }
                ImportItems::Glob => {
                    // If the registry knows this module, enumerate its exports
                    // and define each one individually.
                    if let Some(registry) = self.registry {
                        match registry.resolve_glob(&module_id) {
                            Ok(exports) => {
                                for (name, sym) in exports {
                                    // Seed effect_info for imported effects.
                                    if sym.kind == ExportKind::Effect {
                                        seed_effect_info_from_registry(
                                            self.symbols,
                                            name,
                                            sym,
                                            import.id,
                                            import.span,
                                        );
                                    }
                                    // Seed enum variant constructors for
                                    // glob-imported enums.
                                    if sym.kind == ExportKind::Enum {
                                        seed_enum_variants_from_registry(
                                            self.symbols,
                                            name,
                                            sym,
                                            import.id,
                                            import.span,
                                        );
                                    }
                                    self.symbols.define(
                                        name.to_string(),
                                        Binding {
                                            name: name.to_string(),
                                            resolved: ResolvedName {
                                                def_id: import.id,
                                                kind: export_kind_to_name_kind(sym.kind),
                                            },
                                            visibility: Visibility::Private,
                                            span: import.span,
                                            used: false,
                                            is_import: true,
                                        },
                                    );
                                }
                            }
                            Err(RegistryError::ModuleNotFound { .. }) => {
                                self.diag.error(
                                    E_MODULE_NOT_FOUND,
                                    format!("module `{module_id}` not found"),
                                    import.span,
                                );
                            }
                            Err(e) => {
                                self.diag.error(
                                    E_SYMBOL_NOT_FOUND,
                                    format!("{e}"),
                                    import.span,
                                );
                            }
                        }
                    }
                    // Always add the sentinel so has_wildcard_import() works
                    // for unresolved glob imports (backward compat).
                    let sentinel = format!("{}.*", module_id);
                    self.symbols.define(
                        sentinel.clone(),
                        Binding {
                            name: sentinel,
                            resolved: ResolvedName {
                                def_id: import.id,
                                kind: NameKind::Module,
                            },
                            visibility: Visibility::Private,
                            span: import.span,
                            used: true, // wildcards are never "unused"
                            is_import: true,
                        },
                    );
                }
            }
        }
    }

    fn collect_items(&mut self, items: &[Item]) {
        for item in items {
            match item {
                Item::Fn(d) => {
                    self.symbols.define(
                        d.name.name.clone(),
                        Binding {
                            name: d.name.name.clone(),
                            resolved: ResolvedName {
                                def_id: d.id,
                                kind: NameKind::Function,
                            },
                            visibility: d.visibility,
                            span: d.span,
                            used: false,
                            is_import: false,
                        },
                    );
                }
                Item::Record(d) => {
                    self.symbols.define(
                        d.name.name.clone(),
                        Binding {
                            name: d.name.name.clone(),
                            resolved: ResolvedName {
                                def_id: d.id,
                                kind: NameKind::Type,
                            },
                            visibility: d.visibility,
                            span: d.span,
                            used: false,
                            is_import: false,
                        },
                    );
                }
                Item::Enum(d) => {
                    self.symbols.define(
                        d.name.name.clone(),
                        Binding {
                            name: d.name.name.clone(),
                            resolved: ResolvedName {
                                def_id: d.id,
                                kind: NameKind::Type,
                            },
                            visibility: d.visibility,
                            span: d.span,
                            used: false,
                            is_import: false,
                        },
                    );
                    // Register each variant as a constructor in scope.
                    for variant in &d.variants {
                        let (vname, vid, vspan) = match variant {
                            EnumVariant::Unit { name, id, span } => (name, id, span),
                            EnumVariant::Struct { name, id, span, .. } => (name, id, span),
                            EnumVariant::Tuple { name, id, span, .. } => (name, id, span),
                        };
                        self.symbols.define(
                            vname.name.clone(),
                            Binding {
                                name: vname.name.clone(),
                                resolved: ResolvedName {
                                    def_id: *vid,
                                    kind: NameKind::Function,
                                },
                                visibility: d.visibility,
                                span: *vspan,
                                used: false,
                                is_import: false,
                            },
                        );
                    }
                }
                Item::Class(d) => {
                    self.symbols.define(
                        d.name.name.clone(),
                        Binding {
                            name: d.name.name.clone(),
                            resolved: ResolvedName {
                                def_id: d.id,
                                kind: NameKind::Type,
                            },
                            visibility: d.visibility,
                            span: d.span,
                            used: false,
                            is_import: false,
                        },
                    );
                }
                Item::Trait(d) | Item::PlatformTrait(d) => {
                    self.symbols.define(
                        d.name.name.clone(),
                        Binding {
                            name: d.name.name.clone(),
                            resolved: ResolvedName {
                                def_id: d.id,
                                kind: NameKind::Trait,
                            },
                            visibility: d.visibility,
                            span: d.span,
                            used: false,
                            is_import: false,
                        },
                    );
                }
                Item::Effect(d) => {
                    self.symbols.define(
                        d.name.name.clone(),
                        Binding {
                            name: d.name.name.clone(),
                            resolved: ResolvedName {
                                def_id: d.id,
                                kind: NameKind::Effect,
                            },
                            visibility: d.visibility,
                            span: d.span,
                            used: false,
                            is_import: false,
                        },
                    );
                    // Store effect operations so `with` clauses can inject
                    // them into function scopes during resolution.
                    let ops: Vec<(String, NodeId, Span)> = d
                        .operations
                        .iter()
                        .map(|op| (op.name.name.clone(), op.id, op.span))
                        .collect();
                    let components: Vec<String> = d
                        .components
                        .iter()
                        .map(|tp| {
                            tp.segments
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join(".")
                        })
                        .collect();
                    self.symbols.effect_info.insert(
                        d.name.name.clone(),
                        EffectInfo {
                            operations: ops,
                            components,
                        },
                    );
                }
                Item::TypeAlias(d) => {
                    self.symbols.define(
                        d.name.name.clone(),
                        Binding {
                            name: d.name.name.clone(),
                            resolved: ResolvedName {
                                def_id: d.id,
                                kind: NameKind::Type,
                            },
                            visibility: d.visibility,
                            span: d.span,
                            used: false,
                            is_import: false,
                        },
                    );
                }
                Item::Const(d) => {
                    self.symbols.define(
                        d.name.name.clone(),
                        Binding {
                            name: d.name.name.clone(),
                            resolved: ResolvedName {
                                def_id: d.id,
                                kind: NameKind::Variable,
                            },
                            visibility: d.visibility,
                            span: d.span,
                            used: false,
                            is_import: false,
                        },
                    );
                }
                // Impl blocks, module handles, and property tests don't
                // introduce names at the module level.
                Item::Impl(_)
                | Item::ModuleHandle(_)
                | Item::PropertyTest(_)
                | Item::Error { .. } => {}
            }
        }
    }

    // ── Item resolution ───────────────────────────────────────────────────────

    fn resolve_item(&mut self, item: &Item) {
        match item {
            Item::Fn(d) => self.resolve_fn(d),
            Item::Impl(d) => self.resolve_impl(d),
            Item::Class(d) => {
                for m in &d.methods {
                    self.resolve_fn(m);
                }
            }
            Item::Trait(d) | Item::PlatformTrait(d) => {
                for m in &d.methods {
                    self.resolve_fn(m);
                }
            }
            Item::Effect(d) => {
                for op in &d.operations {
                    self.resolve_fn(op);
                }
            }
            Item::Const(d) => self.resolve_expr(&d.value),
            Item::ModuleHandle(d) => self.resolve_expr(&d.handler),
            Item::PropertyTest(d) => self.resolve_block(&d.body),
            // Types / aliases: no expressions to resolve yet.
            Item::Record(_) | Item::Enum(_) | Item::TypeAlias(_) | Item::Error { .. } => {}
        }
    }

    fn resolve_fn(&mut self, d: &FnDecl) {
        self.symbols.push_scope();
        for param in &d.params {
            self.resolve_param(param);
        }
        if let Some(ret) = &d.return_type {
            self.resolve_type_expr(ret);
        }
        // Inject effect operations from the `with` clause into scope so
        // that calls like `log("msg")` resolve inside effectful functions.
        self.inject_effect_operations(&d.effect_clause);
        if let Some(ref body) = d.body {
            self.resolve_block_body(body);
        }
        self.symbols.pop_scope();
    }

    /// For each effect in the `with` clause, look up its operations and
    /// define them as function bindings in the current scope.
    fn inject_effect_operations(&mut self, effect_clause: &[bock_ast::TypePath]) {
        let mut visited = HashSet::new();
        for effect_path in effect_clause {
            let effect_name = effect_path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            self.inject_ops_for_effect(&effect_name, &mut visited);
        }
    }

    /// Recursively inject operations for a single effect (handles composites).
    fn inject_ops_for_effect(&mut self, effect_name: &str, visited: &mut HashSet<String>) {
        if !visited.insert(effect_name.to_string()) {
            return; // avoid cycles
        }
        // Clone to avoid borrow conflict with self.symbols.define below.
        let info = self.symbols.effect_info.get(effect_name).cloned();
        if let Some(info) = info {
            for (op_name, op_id, op_span) in &info.operations {
                self.symbols.define(
                    op_name.clone(),
                    Binding {
                        name: op_name.clone(),
                        resolved: ResolvedName {
                            def_id: *op_id,
                            kind: NameKind::Function,
                        },
                        visibility: Visibility::Public,
                        span: *op_span,
                        used: true, // never warn about unused effect ops
                        is_import: false,
                    },
                );
            }
            // Resolve composite effects transitively.
            let components = info.components.clone();
            for component in &components {
                self.inject_ops_for_effect(component, visited);
            }
        }
    }

    fn resolve_param(&mut self, param: &Param) {
        self.collect_pattern_bindings(&param.pattern, NameKind::Variable, Visibility::Private);
        if let Some(ty) = &param.ty {
            self.resolve_type_expr(ty);
        }
        if let Some(default) = &param.default {
            self.resolve_expr(default);
        }
    }

    fn resolve_impl(&mut self, d: &ImplBlock) {
        for m in &d.methods {
            // Check whether the method already declares `self` as a parameter.
            let has_self = m.params.iter().any(|p| {
                matches!(&p.pattern, Pattern::Bind { name, .. } if name.name == "self")
            });

            if has_self || d.trait_path.is_none() {
                // Inherent impl or method with explicit `self` — resolve normally.
                self.resolve_fn(m);
            } else {
                // Effect impl method without explicit `self` parameter.
                // Inject a synthetic `self` binding so the body can access
                // the implementing record's fields.
                self.symbols.push_scope();
                let syn_id = self.next_synthetic_id();
                self.symbols.define(
                    "self".to_string(),
                    Binding {
                        name: "self".to_string(),
                        resolved: ResolvedName {
                            def_id: syn_id,
                            kind: NameKind::Variable,
                        },
                        visibility: Visibility::Private,
                        span: m.span,
                        used: false,
                        is_import: false,
                    },
                );
                for param in &m.params {
                    self.resolve_param(param);
                }
                self.inject_effect_operations(&m.effect_clause);
                if let Some(ref body) = m.body {
                    self.resolve_block_body(body);
                }
                self.symbols.pop_scope();
            }
        }
    }

    // ── Block / statement resolution ──────────────────────────────────────────

    /// Resolve a block, pushing and popping a fresh scope.
    fn resolve_block(&mut self, block: &Block) {
        self.symbols.push_scope();
        self.resolve_block_body(block);
        self.symbols.pop_scope();
    }

    /// Resolve a block's contents in the *current* scope (no scope push/pop).
    ///
    /// Used when the caller has already pushed a scope for the block (e.g. for
    /// `for` loops where the loop variable is in the same scope as the body).
    fn resolve_block_body(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.resolve_stmt(stmt);
        }
        if let Some(tail) = &block.tail {
            self.resolve_expr(tail);
        }
    }

    fn resolve_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let(s) => self.resolve_let(s),
            Stmt::Expr(e) => self.resolve_expr(e),
            Stmt::For(f) => self.resolve_for(f),
            Stmt::While(w) => self.resolve_while(w),
            Stmt::Loop(l) => self.resolve_loop(l),
            Stmt::Guard(g) => self.resolve_guard(g),
            Stmt::Handling(h) => self.resolve_handling(h),
            Stmt::Empty => {}
        }
    }

    fn resolve_let(&mut self, s: &LetStmt) {
        // Resolve the initialiser before binding the name (no self-referential defs).
        self.resolve_expr(&s.value);
        if let Some(ty) = &s.ty {
            self.resolve_type_expr(ty);
        }
        self.collect_pattern_bindings(&s.pattern, NameKind::Variable, Visibility::Private);
    }

    fn resolve_for(&mut self, f: &ForLoop) {
        self.resolve_expr(&f.iterable);
        // The loop variable and the body share one scope.
        self.symbols.push_scope();
        self.collect_pattern_bindings(&f.pattern, NameKind::Variable, Visibility::Private);
        self.resolve_block_body(&f.body);
        self.symbols.pop_scope();
    }

    fn resolve_while(&mut self, w: &WhileLoop) {
        self.resolve_expr(&w.condition);
        self.resolve_block(&w.body);
    }

    fn resolve_loop(&mut self, l: &LoopStmt) {
        self.resolve_block(&l.body);
    }

    fn resolve_guard(&mut self, g: &GuardStmt) {
        self.resolve_expr(&g.condition);
        if let Some(pat) = &g.let_pattern {
            self.collect_pattern_bindings(pat, NameKind::Variable, Visibility::Private);
        }
        self.resolve_block(&g.else_block);
    }

    fn resolve_handling(&mut self, h: &HandlingBlock) {
        for pair in &h.handlers {
            self.resolve_expr(&pair.handler);
        }
        self.resolve_block(&h.body);
    }

    // ── Expression resolution ─────────────────────────────────────────────────

    fn resolve_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Identifier { id, name, .. } => {
                if let Some(resolved) = self.symbols.lookup(&name.name) {
                    self.symbols.record_resolution(*id, resolved);
                } else if !self.symbols.has_wildcard_import() {
                    let visible = self.symbols.visible_names();
                    let diag = self.diag.error(
                        E_UNDEFINED,
                        format!("undefined name `{}`", name.name),
                        name.span,
                    );
                    if let Some(hint) = keyword_hint(&name.name) {
                        diag.note(hint);
                    } else if let Some(suggestion) =
                        bock_errors::suggest_similar(&name.name, visible, 2)
                    {
                        diag.note(format!("did you mean `{suggestion}`?"));
                    }
                }
            }

            // Terminals with no sub-expressions.
            Expr::Literal { .. }
            | Expr::Continue { .. }
            | Expr::Unreachable { .. }
            | Expr::Placeholder { .. } => {}

            Expr::Binary { left, right, .. } => {
                self.resolve_expr(left);
                self.resolve_expr(right);
            }
            Expr::Unary { operand, .. } => self.resolve_expr(operand),
            Expr::Assign { target, value, .. } => {
                self.resolve_expr(target);
                self.resolve_expr(value);
            }
            Expr::Call { callee, args, .. } => {
                self.resolve_expr(callee);
                for arg in args {
                    self.resolve_expr(&arg.value);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.resolve_expr(receiver);
                for arg in args {
                    self.resolve_expr(&arg.value);
                }
            }
            Expr::FieldAccess { object, .. } => self.resolve_expr(object),
            Expr::Index { object, index, .. } => {
                self.resolve_expr(object);
                self.resolve_expr(index);
            }
            Expr::Try { expr, .. } => self.resolve_expr(expr),
            Expr::Lambda { params, body, .. } => {
                self.symbols.push_scope();
                for p in params {
                    self.resolve_param(p);
                }
                self.resolve_expr(body);
                self.symbols.pop_scope();
            }
            Expr::Pipe { left, right, .. } | Expr::Compose { left, right, .. } => {
                self.resolve_expr(left);
                self.resolve_expr(right);
            }
            Expr::If {
                condition,
                let_pattern,
                then_block,
                else_block,
                ..
            } => {
                self.resolve_expr(condition);
                // `if let pat = expr { ... }` — pattern bindings live in the
                // then-branch scope.
                self.symbols.push_scope();
                if let Some(pat) = let_pattern {
                    self.collect_pattern_bindings(pat, NameKind::Variable, Visibility::Private);
                }
                self.resolve_block_body(then_block);
                self.symbols.pop_scope();
                if let Some(eb) = else_block {
                    self.resolve_expr(eb);
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.resolve_expr(scrutinee);
                for arm in arms {
                    self.resolve_match_arm(arm);
                }
            }
            Expr::Loop { body, .. } => self.resolve_block(body),
            Expr::Block { block, .. } => self.resolve_block(block),
            Expr::RecordConstruct {
                path,
                fields,
                spread,
                ..
            } => {
                if let Some(first) = path.segments.first() {
                    self.symbols.mark_used(&first.name);
                }
                for f in fields {
                    if let Some(v) = &f.value {
                        self.resolve_expr(v);
                    }
                }
                if let Some(s) = spread {
                    self.resolve_expr(&s.expr);
                }
            }
            Expr::ListLiteral { elems, .. }
            | Expr::SetLiteral { elems, .. }
            | Expr::TupleLiteral { elems, .. } => {
                for e in elems {
                    self.resolve_expr(e);
                }
            }
            Expr::MapLiteral { entries, .. } => {
                for (k, v) in entries {
                    self.resolve_expr(k);
                    self.resolve_expr(v);
                }
            }
            Expr::Range { lo, hi, .. } => {
                self.resolve_expr(lo);
                self.resolve_expr(hi);
            }
            Expr::Await { expr, .. } => self.resolve_expr(expr),
            Expr::Return { value, .. } | Expr::Break { value, .. } => {
                if let Some(v) = value {
                    self.resolve_expr(v);
                }
            }
            Expr::Interpolation { parts, .. } => {
                for part in parts {
                    if let InterpolationPart::Expr(e) = part {
                        self.resolve_expr(e);
                    }
                }
            }
            Expr::Is { expr, .. } => {
                self.resolve_expr(expr);
            }
        }
    }

    /// Marks type names referenced in a type expression as used imports.
    fn resolve_type_expr(&mut self, ty: &bock_ast::TypeExpr) {
        match ty {
            bock_ast::TypeExpr::Named { path, args, .. } => {
                if let Some(first) = path.segments.first() {
                    self.symbols.mark_used(&first.name);
                }
                for arg in args {
                    self.resolve_type_expr(arg);
                }
            }
            bock_ast::TypeExpr::Tuple { elems, .. } => {
                for e in elems {
                    self.resolve_type_expr(e);
                }
            }
            bock_ast::TypeExpr::Function {
                params, ret, effects, ..
            } => {
                for p in params {
                    self.resolve_type_expr(p);
                }
                self.resolve_type_expr(ret);
                for eff in effects {
                    if let Some(first) = eff.segments.first() {
                        self.symbols.mark_used(&first.name);
                    }
                }
            }
            bock_ast::TypeExpr::Optional { inner, .. } => {
                self.resolve_type_expr(inner);
            }
            bock_ast::TypeExpr::SelfType { .. } => {}
        }
    }

    fn resolve_match_arm(&mut self, arm: &MatchArm) {
        self.symbols.push_scope();
        self.collect_pattern_bindings(&arm.pattern, NameKind::Variable, Visibility::Private);
        if let Some(g) = &arm.guard {
            self.resolve_expr(g);
        }
        self.resolve_expr(&arm.body);
        self.symbols.pop_scope();
    }

    // ── Pattern binding collection ────────────────────────────────────────────

    fn collect_pattern_bindings(
        &mut self,
        pattern: &Pattern,
        kind: NameKind,
        visibility: Visibility,
    ) {
        match pattern {
            // Terminals that bind nothing.
            Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::Rest { .. } => {}

            Pattern::Bind { id, span, name } | Pattern::MutBind { id, span, name } => {
                self.symbols.define(
                    name.name.clone(),
                    Binding {
                        name: name.name.clone(),
                        resolved: ResolvedName { def_id: *id, kind },
                        visibility,
                        span: *span,
                        used: false,
                        is_import: false,
                    },
                );
            }

            Pattern::Constructor { path, fields, .. } => {
                if let Some(first) = path.segments.first() {
                    self.symbols.mark_used(&first.name);
                }
                for f in fields {
                    self.collect_pattern_bindings(f, kind, visibility);
                }
            }
            Pattern::Tuple { elems, .. } => {
                for e in elems {
                    self.collect_pattern_bindings(e, kind, visibility);
                }
            }
            Pattern::Record { path, fields, .. } => {
                if let Some(first) = path.segments.first() {
                    self.symbols.mark_used(&first.name);
                }
                for f in fields {
                    if let Some(p) = &f.pattern {
                        self.collect_pattern_bindings(p, kind, visibility);
                    } else {
                        // Shorthand `{ field }` — bind `field` to itself.
                        let syn_id = self.next_synthetic_id();
                        self.symbols.define(
                            f.name.name.clone(),
                            Binding {
                                name: f.name.name.clone(),
                                resolved: ResolvedName {
                                    def_id: syn_id,
                                    kind,
                                },
                                visibility,
                                span: f.span,
                                used: false,
                                is_import: false,
                            },
                        );
                    }
                }
            }
            Pattern::List { elems, rest, .. } => {
                for e in elems {
                    self.collect_pattern_bindings(e, kind, visibility);
                }
                if let Some(r) = rest {
                    self.collect_pattern_bindings(r, kind, visibility);
                }
            }
            Pattern::Or { alternatives, .. } => {
                // All alternatives must bind the same names (enforced by the
                // type checker).  Collect bindings from the first alternative.
                if let Some(first) = alternatives.first() {
                    self.collect_pattern_bindings(first, kind, visibility);
                }
            }
            Pattern::Range { lo, hi, .. } => {
                self.collect_pattern_bindings(lo, kind, visibility);
                self.collect_pattern_bindings(hi, kind, visibility);
            }
        }
    }

    // ── Unused-import check ───────────────────────────────────────────────────

    fn check_unused_imports(&mut self) {
        // Propagate "used" from enum variants to their parent enum import.
        // If a variant like `Red` was used, mark `Color` as used too.
        if let Some(scope) = self.symbols.scopes.first() {
            let used_parents: Vec<String> = self
                .symbols
                .variant_parent
                .iter()
                .filter(|(variant, _)| {
                    scope
                        .bindings
                        .get(variant.as_str())
                        .is_some_and(|b| b.used)
                })
                .map(|(_, parent)| parent.clone())
                .collect();
            for parent in used_parents {
                self.symbols.mark_used(&parent);
            }
        }

        // Collect into an owned vec to avoid borrowing `self.symbols` and
        // `self.diag` simultaneously.
        let unused: Vec<(String, Span)> = self
            .symbols
            .scopes
            .first()
            .map(|s| {
                s.bindings
                    .values()
                    .filter(|b| b.is_import && !b.used)
                    .map(|b| (b.name.clone(), b.span))
                    .collect()
            })
            .unwrap_or_default();

        for (name, span) in unused {
            self.diag
                .warning(W_UNUSED_IMPORT, format!("unused import `{name}`"), span);
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn module_path_str(path: &ModulePath) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// Converts a registry [`ExportKind`] to a resolver [`NameKind`].
fn export_kind_to_name_kind(kind: ExportKind) -> NameKind {
    match kind {
        ExportKind::Function => NameKind::Function,
        ExportKind::Record | ExportKind::Enum | ExportKind::TypeAlias => NameKind::Type,
        ExportKind::Trait => NameKind::Trait,
        ExportKind::Effect => NameKind::Effect,
        ExportKind::Constant => NameKind::Variable,
    }
}

/// Seeds the symbol table's `effect_info` for an imported effect, so that
/// `with` clauses in the importing module can inject its operations into scope.
fn seed_effect_info_from_registry(
    symbols: &mut SymbolTable,
    local_name: &str,
    sym: &ExportedSymbol,
    import_id: NodeId,
    import_span: Span,
) {
    if let ExportDetail::Effect {
        operations,
        components,
    } = &sym.detail
    {
        let ops: Vec<(String, NodeId, Span)> = operations
            .iter()
            .map(|(name, _type_ref)| (name.clone(), import_id, import_span))
            .collect();
        symbols.effect_info.insert(
            local_name.to_string(),
            EffectInfo {
                operations: ops,
                components: components.clone(),
            },
        );
    }
}

/// Seeds enum variant constructors into the symbol table when an enum type
/// is imported (named or glob). This mirrors what `collect_items` does for
/// locally-defined enums: each variant name becomes a `NameKind::Function`
/// binding so bare constructor names like `Red` or `Circle { ... }` work.
fn seed_enum_variants_from_registry(
    symbols: &mut SymbolTable,
    enum_name: &str,
    sym: &ExportedSymbol,
    import_id: NodeId,
    import_span: Span,
) {
    if let ExportDetail::Enum { variants, .. } = &sym.detail {
        for variant in variants {
            symbols.variant_parent.insert(
                variant.name.clone(),
                enum_name.to_string(),
            );
            symbols.define(
                variant.name.clone(),
                Binding {
                    name: variant.name.clone(),
                    resolved: ResolvedName {
                        def_id: import_id,
                        kind: NameKind::Function,
                    },
                    visibility: Visibility::Private,
                    span: import_span,
                    used: false,
                    // Not marked as an import — auto-seeded variants should
                    // not produce individual "unused import" warnings.
                    is_import: false,
                },
            );
        }
    }
}

// ─── Common-mistake keyword hints ────────────────────────────────────────────

/// Maps foreign-language keywords that users commonly reach for in Bock to a
/// one-line hint suggesting the Bock equivalent.
///
/// Returned as a static hint string when a name lookup fails on one of these
/// keywords. This is preferred over an edit-distance suggestion because the
/// user is almost certainly using vocabulary from another language.
fn keyword_hint(name: &str) -> Option<&'static str> {
    match name {
        "pub" => Some("Bock uses `public` for visibility, not `pub`"),
        "var" => Some("Bock uses `let mut` for mutable bindings, not `var`"),
        "func" | "def" => Some("Bock uses `fn` to declare functions"),
        "interface" => Some("Bock uses `trait` for interfaces"),
        "struct" => Some("Bock uses `record` for value types"),
        "class" => Some("Bock uses `record` for data and `trait` for behavior — there is no `class`"),
        "None_" | "nil" | "null" | "undefined" => {
            Some("Bock uses `None` (from `Optional[T]`) to represent absent values")
        }
        "true_" | "false_" => Some("Bock boolean literals are `true` and `false`"),
        _ => None,
    }
}

// ─── Public entry points ─────────────────────────────────────────────────────

/// Resolve all names in `ast`, populating `symbols` and returning diagnostics.
///
/// This is the single-file entry point. Imports are registered with
/// [`NameKind::Unresolved`] since no cross-file registry is available.
///
/// After this call:
/// - `symbols.resolutions` maps each identifier's usage NodeId to its definition.
/// - Diagnostics include `E1001` errors for undefined names and `W1001`
///   warnings for unused imports.
pub fn resolve_names(ast: &Module, symbols: &mut SymbolTable) -> DiagnosticBag {
    let mut diag = DiagnosticBag::new();
    let mut resolver = Resolver::new(symbols, &mut diag);
    resolver.resolve_module(ast);
    diag
}

/// Resolve all names in `ast` with cross-file import resolution.
///
/// Named imports (`use a.b.{X}`) and glob imports (`use a.b.*`) are resolved
/// against the `registry`. Modules not found in the registry produce `E1005`
/// diagnostics; missing or private symbols produce `E1006`/`E1007`.
///
/// When the registry is empty (no modules registered), this behaves identically
/// to [`resolve_names`] — single-file mode is preserved.
pub fn resolve_names_with_registry(
    ast: &Module,
    symbols: &mut SymbolTable,
    registry: &ModuleRegistry,
) -> DiagnosticBag {
    let mut diag = DiagnosticBag::new();
    let mut resolver = Resolver::new(symbols, &mut diag);
    resolver.registry = Some(registry);
    resolver.resolve_module(ast);
    diag
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_ast::{
        Block, FnDecl, Ident, ImportDecl, ImportItems, ImportedName, Item, Literal, Module,
        ModulePath, Param, Pattern, Stmt, Visibility,
    };
    use bock_errors::{FileId, Span};

    fn sp() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 1,
        }
    }

    fn ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: sp(),
        }
    }

    fn mpath(segments: &[&str]) -> ModulePath {
        ModulePath {
            segments: segments.iter().map(|s| ident(s)).collect(),
            span: sp(),
        }
    }

    fn empty_block(id: NodeId) -> Block {
        Block {
            id,
            span: sp(),
            stmts: vec![],
            tail: None,
        }
    }

    fn simple_module(imports: Vec<ImportDecl>, items: Vec<Item>) -> Module {
        Module {
            id: 0,
            span: sp(),
            doc: vec![],
            path: None,
            imports,
            items,
        }
    }

    fn fn_item(id: NodeId, name: &str, vis: Visibility) -> Item {
        Item::Fn(FnDecl {
            id,
            span: sp(),
            annotations: vec![],
            visibility: vis,
            is_async: false,
            name: ident(name),
            generic_params: vec![],
            params: vec![],
            return_type: None,
            effect_clause: vec![],
            where_clause: vec![],
            body: Some(empty_block(id + 100)),
        })
    }

    // ── SymbolTable basics ────────────────────────────────────────────────────

    #[test]
    fn symbol_table_define_and_lookup() {
        let mut st = SymbolTable::new();
        st.define(
            "foo".into(),
            Binding {
                name: "foo".into(),
                resolved: ResolvedName {
                    def_id: 1,
                    kind: NameKind::Function,
                },
                visibility: Visibility::Public,
                span: sp(),
                used: false,
                is_import: false,
            },
        );
        let r = st.lookup("foo").unwrap();
        assert_eq!(r.def_id, 1);
        assert_eq!(r.kind, NameKind::Function);
    }

    #[test]
    fn symbol_table_lookup_marks_used() {
        let mut st = SymbolTable::new();
        st.define(
            "x".into(),
            Binding {
                name: "x".into(),
                resolved: ResolvedName {
                    def_id: 5,
                    kind: NameKind::Variable,
                },
                visibility: Visibility::Private,
                span: sp(),
                used: false,
                is_import: false,
            },
        );
        st.lookup("x");
        assert!(st.lookup_peek("x").unwrap().used);
    }

    #[test]
    fn symbol_table_inner_scope_shadows_outer() {
        let mut st = SymbolTable::new();
        st.define(
            "x".into(),
            Binding {
                name: "x".into(),
                resolved: ResolvedName {
                    def_id: 1,
                    kind: NameKind::Variable,
                },
                visibility: Visibility::Private,
                span: sp(),
                used: false,
                is_import: false,
            },
        );
        st.push_scope();
        st.define(
            "x".into(),
            Binding {
                name: "x".into(),
                resolved: ResolvedName {
                    def_id: 2,
                    kind: NameKind::Variable,
                },
                visibility: Visibility::Private,
                span: sp(),
                used: false,
                is_import: false,
            },
        );
        // Inner binding (def_id=2) shadows outer (def_id=1).
        assert_eq!(st.lookup("x").unwrap().def_id, 2);
        st.pop_scope();
        // After popping, outer binding is visible again.
        assert_eq!(st.lookup("x").unwrap().def_id, 1);
    }

    #[test]
    fn symbol_table_lookup_unknown_returns_none() {
        let mut st = SymbolTable::new();
        assert!(st.lookup("unknown").is_none());
    }

    #[test]
    fn symbol_table_module_scope_never_popped() {
        let mut st = SymbolTable::new();
        assert!(st.pop_scope().is_none()); // only module scope remains
    }

    // ── resolve_names: simple cases ───────────────────────────────────────────

    #[test]
    fn resolve_defined_identifier() {
        // fn foo() {}
        // fn bar() { foo }
        let module = simple_module(
            vec![],
            vec![
                fn_item(1, "foo", Visibility::Private),
                Item::Fn(FnDecl {
                    id: 2,
                    span: sp(),
                    annotations: vec![],
                    visibility: Visibility::Private,
                    is_async: false,
                    name: ident("bar"),
                    generic_params: vec![],
                    params: vec![],
                    return_type: None,
                    effect_clause: vec![],
                    where_clause: vec![],
                    body: Some(Block {
                        id: 200,
                        span: sp(),
                        stmts: vec![],
                        tail: Some(Box::new(Expr::Identifier {
                            id: 99,
                            span: sp(),
                            name: ident("foo"),
                        })),
                    }),
                }),
            ],
        );
        let mut st = SymbolTable::new();
        let diag = resolve_names(&module, &mut st);
        assert!(
            !diag.has_errors(),
            "unexpected errors: {:?}",
            diag.iter().collect::<Vec<_>>()
        );
        let resolved = st
            .resolutions
            .get(&99)
            .expect("identifier should be resolved");
        assert_eq!(resolved.def_id, 1);
        assert_eq!(resolved.kind, NameKind::Function);
    }

    #[test]
    fn resolve_undefined_identifier_produces_error() {
        let module = simple_module(
            vec![],
            vec![Item::Fn(FnDecl {
                id: 1,
                span: sp(),
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("bar"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Some(Block {
                    id: 100,
                    span: sp(),
                    stmts: vec![],
                    tail: Some(Box::new(Expr::Identifier {
                        id: 42,
                        span: sp(),
                        name: ident("undefined_thing"),
                    })),
                }),
            })],
        );
        let mut st = SymbolTable::new();
        let diag = resolve_names(&module, &mut st);
        assert!(diag.has_errors());
        let msgs: Vec<_> = diag.iter().map(|d| d.message.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("undefined_thing")));
    }

    // ── Import resolution ─────────────────────────────────────────────────────

    #[test]
    fn named_import_creates_binding() {
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["core", "collections"]),
            items: ImportItems::Named(vec![
                ImportedName {
                    span: sp(),
                    name: ident("List"),
                    alias: None,
                },
                ImportedName {
                    span: sp(),
                    name: ident("Map"),
                    alias: None,
                },
            ]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        resolve_names(&module, &mut st);
        // Both List and Map should be in the module scope.
        assert!(st.lookup_peek("List").is_some());
        assert!(st.lookup_peek("Map").is_some());
    }

    #[test]
    fn named_import_with_alias() {
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["core"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("FooBar"),
                alias: Some(ident("FB")),
            }]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        resolve_names(&module, &mut st);
        // Local name is the alias.
        assert!(st.lookup_peek("FB").is_some());
        assert!(st.lookup_peek("FooBar").is_none());
    }

    #[test]
    fn module_import_creates_binding() {
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["app", "models"]),
            items: ImportItems::Module,
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        resolve_names(&module, &mut st);
        // Last path segment is bound.
        let b = st.lookup_peek("models").expect("models should be bound");
        assert_eq!(b.resolved.kind, NameKind::Module);
    }

    #[test]
    fn wildcard_import_suppresses_undefined_errors() {
        // `use some.module.*` — we can't enumerate names, so identifiers that
        // aren't locally defined should NOT produce an error.
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["some", "module"]),
            items: ImportItems::Glob,
        };
        let module = simple_module(
            vec![import],
            vec![Item::Fn(FnDecl {
                id: 1,
                span: sp(),
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Some(Block {
                    id: 100,
                    span: sp(),
                    stmts: vec![],
                    tail: Some(Box::new(Expr::Identifier {
                        id: 99,
                        span: sp(),
                        name: ident("SomethingFromWildcard"),
                    })),
                }),
            })],
        );
        let mut st = SymbolTable::new();
        let diag = resolve_names(&module, &mut st);
        assert!(
            !diag.has_errors(),
            "wildcard import should suppress undefined errors"
        );
    }

    // ── Unused import warnings ────────────────────────────────────────────────

    #[test]
    fn unused_named_import_produces_warning() {
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["core"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("Unused"),
                alias: None,
            }]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names(&module, &mut st);
        assert!(!diag.has_errors());
        let warnings: Vec<_> = diag
            .iter()
            .filter(|d| d.severity == bock_errors::Severity::Warning)
            .collect();
        assert!(!warnings.is_empty(), "expected unused import warning");
        assert!(warnings.iter().any(|w| w.message.contains("Unused")));
    }

    #[test]
    fn used_import_no_warning() {
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["core"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("Used"),
                alias: None,
            }]),
        };
        let module = simple_module(
            vec![import],
            vec![Item::Fn(FnDecl {
                id: 1,
                span: sp(),
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Some(Block {
                    id: 100,
                    span: sp(),
                    stmts: vec![],
                    tail: Some(Box::new(Expr::Identifier {
                        id: 99,
                        span: sp(),
                        name: ident("Used"),
                    })),
                }),
            })],
        );
        let mut st = SymbolTable::new();
        let diag = resolve_names(&module, &mut st);
        assert!(!diag.has_errors());
        let warnings: Vec<_> = diag
            .iter()
            .filter(|d| d.severity == bock_errors::Severity::Warning)
            .collect();
        assert!(warnings.is_empty(), "no warning expected for used import");
    }

    // ── Shadowing ─────────────────────────────────────────────────────────────

    #[test]
    fn let_binding_shadows_outer() {
        // fn test() {
        //   let x = 1
        //   let x = 2  ← shadows
        //   x          ← resolves to inner x (id=20)
        // }
        use bock_ast::LetStmt;
        let outer_let = Stmt::Let(LetStmt {
            id: 10,
            span: sp(),
            pattern: Pattern::Bind {
                id: 10,
                span: sp(),
                name: ident("x"),
            },
            ty: None,
            value: Expr::Literal {
                id: 11,
                span: sp(),
                lit: Literal::Int("1".into()),
            },
        });
        let inner_let = Stmt::Let(LetStmt {
            id: 20,
            span: sp(),
            pattern: Pattern::Bind {
                id: 20,
                span: sp(),
                name: ident("x"),
            },
            ty: None,
            value: Expr::Literal {
                id: 21,
                span: sp(),
                lit: Literal::Int("2".into()),
            },
        });
        let use_x = Expr::Identifier {
            id: 99,
            span: sp(),
            name: ident("x"),
        };

        let module = simple_module(
            vec![],
            vec![Item::Fn(FnDecl {
                id: 1,
                span: sp(),
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Some(Block {
                    id: 100,
                    span: sp(),
                    stmts: vec![outer_let, inner_let],
                    tail: Some(Box::new(use_x)),
                }),
            })],
        );
        let mut st = SymbolTable::new();
        let diag = resolve_names(&module, &mut st);
        assert!(!diag.has_errors());
        // The use of `x` (id=99) should resolve to the inner let (def_id=20).
        let resolved = st.resolutions.get(&99).expect("x should be resolved");
        assert_eq!(resolved.def_id, 20);
    }

    // ── Param binding ─────────────────────────────────────────────────────────

    #[test]
    fn function_param_is_in_scope() {
        let module = simple_module(
            vec![],
            vec![Item::Fn(FnDecl {
                id: 1,
                span: sp(),
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("id"),
                generic_params: vec![],
                params: vec![Param {
                    id: 5,
                    span: sp(),
                    pattern: Pattern::Bind {
                        id: 5,
                        span: sp(),
                        name: ident("n"),
                    },
                    ty: None,
                    default: None,
                }],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Some(Block {
                    id: 100,
                    span: sp(),
                    stmts: vec![],
                    tail: Some(Box::new(Expr::Identifier {
                        id: 99,
                        span: sp(),
                        name: ident("n"),
                    })),
                }),
            })],
        );
        let mut st = SymbolTable::new();
        let diag = resolve_names(&module, &mut st);
        assert!(!diag.has_errors());
        let resolved = st.resolutions.get(&99).expect("param n should resolve");
        assert_eq!(resolved.def_id, 5);
        assert_eq!(resolved.kind, NameKind::Variable);
    }

    // ── Visibility tracked ────────────────────────────────────────────────────

    #[test]
    fn visibility_is_stored_in_binding() {
        let module = simple_module(vec![], vec![fn_item(1, "pub_fn", Visibility::Public)]);
        let mut st = SymbolTable::new();
        resolve_names(&module, &mut st);
        let b = st.lookup_peek("pub_fn").expect("pub_fn should be bound");
        assert_eq!(b.visibility, Visibility::Public);
    }

    // ── Registry-backed import resolution ────────────────────────────────────

    use crate::registry::{
        EnumVariantExport, ExportDetail, ExportKind, ExportedSymbol, ModuleExports,
        ModuleRegistry,
    };
    use crate::stubs::TypeRef;

    /// Build a registry with a sample "app.models" module exporting
    /// User (record), Role (enum), and default_user (function).
    fn sample_registry() -> ModuleRegistry {
        let mut reg = ModuleRegistry::new();
        let mut exports = ModuleExports::new("app.models", "src/app/models.bock");
        exports.add_symbol(
            "User",
            ExportedSymbol {
                kind: ExportKind::Record,
                visibility: Visibility::Public,
                ty: TypeRef("User".to_string()),
                detail: ExportDetail::Record {
                    fields: vec![
                        ("name".to_string(), TypeRef("String".to_string())),
                        ("age".to_string(), TypeRef("Int".to_string())),
                    ],
                    generic_params: vec![],
                    methods: HashMap::new(),
                },
            },
        );
        exports.add_symbol(
            "Role",
            ExportedSymbol {
                kind: ExportKind::Enum,
                visibility: Visibility::Public,
                ty: TypeRef("Role".to_string()),
                detail: ExportDetail::Enum {
                    variants: vec![],
                    generic_params: vec![],
                },
            },
        );
        exports.add_symbol(
            "default_user",
            ExportedSymbol {
                kind: ExportKind::Function,
                visibility: Visibility::Public,
                ty: TypeRef("Fn() -> User".to_string()),
                detail: ExportDetail::None,
            },
        );
        exports.add_symbol(
            "internal_helper",
            ExportedSymbol {
                kind: ExportKind::Function,
                visibility: Visibility::Internal,
                ty: TypeRef("Fn() -> Void".to_string()),
                detail: ExportDetail::None,
            },
        );
        exports.add_symbol(
            "private_secret",
            ExportedSymbol {
                kind: ExportKind::Function,
                visibility: Visibility::Private,
                ty: TypeRef("Fn() -> Void".to_string()),
                detail: ExportDetail::None,
            },
        );
        reg.register(exports);
        reg
    }

    #[test]
    fn registry_named_import_resolves_kind() {
        let registry = sample_registry();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["app", "models"]),
            items: ImportItems::Named(vec![
                ImportedName {
                    span: sp(),
                    name: ident("User"),
                    alias: None,
                },
                ImportedName {
                    span: sp(),
                    name: ident("default_user"),
                    alias: None,
                },
            ]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        assert!(
            !diag.has_errors(),
            "unexpected errors: {:?}",
            diag.iter().collect::<Vec<_>>()
        );
        let user = st.lookup_peek("User").expect("User should be bound");
        assert_eq!(user.resolved.kind, NameKind::Type);
        assert!(user.is_import);
        let dfn = st
            .lookup_peek("default_user")
            .expect("default_user should be bound");
        assert_eq!(dfn.resolved.kind, NameKind::Function);
    }

    #[test]
    fn registry_named_import_with_alias_resolves_kind() {
        let registry = sample_registry();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["app", "models"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("User"),
                alias: Some(ident("AppUser")),
            }]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        assert!(!diag.has_errors());
        assert!(st.lookup_peek("AppUser").is_some());
        assert!(st.lookup_peek("User").is_none());
        assert_eq!(
            st.lookup_peek("AppUser").unwrap().resolved.kind,
            NameKind::Type
        );
    }

    #[test]
    fn registry_named_import_missing_symbol_produces_error() {
        let registry = sample_registry();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["app", "models"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("NonExistent"),
                alias: None,
            }]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        assert!(diag.has_errors());
        let msgs: Vec<_> = diag.iter().map(|d| d.message.clone()).collect();
        assert!(msgs.iter().any(|m| m.contains("NonExistent")));
        assert!(msgs.iter().any(|m| m.contains("not exported")));
    }

    #[test]
    fn registry_named_import_private_symbol_produces_error() {
        let registry = sample_registry();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["app", "models"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("private_secret"),
                alias: None,
            }]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        assert!(diag.has_errors());
        let msgs: Vec<_> = diag.iter().map(|d| d.message.clone()).collect();
        assert!(msgs.iter().any(|m| m.contains("private")));
    }

    #[test]
    fn registry_named_import_module_not_found_produces_error() {
        let registry = sample_registry();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["no", "such", "module"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("Foo"),
                alias: None,
            }]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        assert!(diag.has_errors());
        let msgs: Vec<_> = diag.iter().map(|d| d.message.clone()).collect();
        assert!(msgs.iter().any(|m| m.contains("not found")));
    }

    #[test]
    fn registry_glob_import_defines_all_public_names() {
        let registry = sample_registry();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["app", "models"]),
            items: ImportItems::Glob,
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        assert!(
            !diag.has_errors(),
            "unexpected errors: {:?}",
            diag.iter().collect::<Vec<_>>()
        );
        // Public symbols should be defined.
        assert!(st.lookup_peek("User").is_some());
        assert!(st.lookup_peek("Role").is_some());
        assert!(st.lookup_peek("default_user").is_some());
        // Internal is also visible (within-package access).
        assert!(st.lookup_peek("internal_helper").is_some());
        // Private must NOT be imported.
        assert!(
            st.lookup_peek("private_secret").is_none()
                || st.lookup_peek("private_secret").unwrap().resolved.kind == NameKind::Builtin
        );
        // Sentinel still present for backward compat.
        assert!(st.has_wildcard_import());
    }

    #[test]
    fn registry_glob_import_module_not_found_produces_error() {
        let registry = sample_registry();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["no", "such", "module"]),
            items: ImportItems::Glob,
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        assert!(diag.has_errors());
        let msgs: Vec<_> = diag.iter().map(|d| d.message.clone()).collect();
        assert!(msgs.iter().any(|m| m.contains("not found")));
    }

    #[test]
    fn registry_resolved_import_used_in_body_no_errors() {
        // Simulates: use app.models.{User, default_user}
        // fn main() { default_user() }
        let registry = sample_registry();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["app", "models"]),
            items: ImportItems::Named(vec![
                ImportedName {
                    span: sp(),
                    name: ident("User"),
                    alias: None,
                },
                ImportedName {
                    span: sp(),
                    name: ident("default_user"),
                    alias: None,
                },
            ]),
        };
        let module = simple_module(
            vec![import],
            vec![Item::Fn(FnDecl {
                id: 1,
                span: sp(),
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Some(Block {
                    id: 100,
                    span: sp(),
                    stmts: vec![],
                    tail: Some(Box::new(Expr::Call {
                        id: 50,
                        span: sp(),
                        callee: Box::new(Expr::Identifier {
                            id: 51,
                            span: sp(),
                            name: ident("default_user"),
                        }),
                        type_args: vec![],
                        args: vec![],
                    })),
                }),
            })],
        );
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        assert!(
            !diag.has_errors(),
            "unexpected errors: {:?}",
            diag.iter().collect::<Vec<_>>()
        );
        // default_user call resolves to the import's def_id.
        let resolved = st
            .resolutions
            .get(&51)
            .expect("default_user should resolve");
        assert_eq!(resolved.kind, NameKind::Function);
        // User import is unused — should produce a warning.
        let warnings: Vec<_> = diag
            .iter()
            .filter(|d| d.severity == bock_errors::Severity::Warning)
            .collect();
        assert!(
            warnings.iter().any(|w| w.message.contains("User")),
            "expected unused import warning for User"
        );
    }

    #[test]
    fn empty_registry_behaves_like_single_file() {
        // With an empty registry, named imports stay Unresolved (no error
        // since the module simply isn't registered).
        let registry = ModuleRegistry::new();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["unknown", "module"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("Thing"),
                alias: None,
            }]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        // Empty registry triggers module-not-found for the import.
        assert!(diag.has_errors());
        // But the binding is still defined (as Unresolved) so the compiler
        // can continue downstream.
        let b = st.lookup_peek("Thing").expect("Thing should still be bound");
        assert_eq!(b.resolved.kind, NameKind::Unresolved);
    }

    #[test]
    fn no_registry_leaves_imports_unresolved() {
        // Plain resolve_names (no registry) keeps the old behavior.
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["app", "models"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("User"),
                alias: None,
            }]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names(&module, &mut st);
        // No errors: single-file mode doesn't validate imports.
        assert!(!diag.has_errors());
        let b = st.lookup_peek("User").expect("User should be bound");
        assert_eq!(b.resolved.kind, NameKind::Unresolved);
    }

    #[test]
    fn registry_internal_symbol_is_importable() {
        let registry = sample_registry();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["app", "models"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("internal_helper"),
                alias: None,
            }]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        assert!(
            !diag.has_errors(),
            "internal symbols should be importable: {:?}",
            diag.iter().collect::<Vec<_>>()
        );
        let b = st
            .lookup_peek("internal_helper")
            .expect("internal_helper should be bound");
        assert_eq!(b.resolved.kind, NameKind::Function);
    }

    #[test]
    fn registry_named_enum_import_seeds_variant_constructors() {
        // Build a registry with a module exporting an enum with variants.
        let mut reg = ModuleRegistry::new();
        let mut exports = ModuleExports::new("colors", "colors.bock");
        exports.add_symbol(
            "Color",
            ExportedSymbol {
                kind: ExportKind::Enum,
                visibility: Visibility::Public,
                ty: TypeRef("Color".to_string()),
                detail: ExportDetail::Enum {
                    variants: vec![
                        EnumVariantExport {
                            name: "Red".to_string(),
                            constructor_type: None,
                            fields: None,
                        },
                        EnumVariantExport {
                            name: "Green".to_string(),
                            constructor_type: None,
                            fields: None,
                        },
                        EnumVariantExport {
                            name: "Blue".to_string(),
                            constructor_type: None,
                            fields: None,
                        },
                    ],
                    generic_params: vec![],
                },
            },
        );
        reg.register(exports);

        // use colors.{Color}
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["colors"]),
            items: ImportItems::Named(vec![ImportedName {
                span: sp(),
                name: ident("Color"),
                alias: None,
            }]),
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &reg);
        assert!(
            !diag.has_errors(),
            "unexpected errors: {:?}",
            diag.iter().collect::<Vec<_>>()
        );
        // The enum type itself should be defined.
        assert!(st.lookup_peek("Color").is_some());
        // Each variant constructor should also be in scope.
        let red = st.lookup_peek("Red").expect("Red should be in scope");
        assert_eq!(red.resolved.kind, NameKind::Function);
        // Auto-seeded variants are not marked as imports to avoid
        // spurious "unused import" warnings.
        assert!(!red.is_import);
        assert!(st.lookup_peek("Green").is_some());
        assert!(st.lookup_peek("Blue").is_some());
    }

    #[test]
    fn registry_glob_enum_import_seeds_variant_constructors() {
        // Same registry as above but using glob import.
        let mut reg = ModuleRegistry::new();
        let mut exports = ModuleExports::new("colors", "colors.bock");
        exports.add_symbol(
            "Color",
            ExportedSymbol {
                kind: ExportKind::Enum,
                visibility: Visibility::Public,
                ty: TypeRef("Color".to_string()),
                detail: ExportDetail::Enum {
                    variants: vec![
                        EnumVariantExport {
                            name: "Red".to_string(),
                            constructor_type: None,
                            fields: None,
                        },
                    ],
                    generic_params: vec![],
                },
            },
        );
        reg.register(exports);

        // use colors.*
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["colors"]),
            items: ImportItems::Glob,
        };
        let module = simple_module(vec![import], vec![]);
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &reg);
        assert!(
            !diag.has_errors(),
            "unexpected errors: {:?}",
            diag.iter().collect::<Vec<_>>()
        );
        assert!(st.lookup_peek("Color").is_some());
        let red = st.lookup_peek("Red").expect("Red should be in scope via glob");
        assert_eq!(red.resolved.kind, NameKind::Function);
        assert!(!red.is_import);
    }

    #[test]
    fn registry_glob_with_body_resolves_names() {
        // use app.models.*
        // fn test() { User }
        let registry = sample_registry();
        let import = ImportDecl {
            id: 10,
            span: sp(),
            visibility: Visibility::Private,
            path: mpath(&["app", "models"]),
            items: ImportItems::Glob,
        };
        let module = simple_module(
            vec![import],
            vec![Item::Fn(FnDecl {
                id: 1,
                span: sp(),
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Some(Block {
                    id: 100,
                    span: sp(),
                    stmts: vec![],
                    tail: Some(Box::new(Expr::Identifier {
                        id: 99,
                        span: sp(),
                        name: ident("User"),
                    })),
                }),
            })],
        );
        let mut st = SymbolTable::new();
        let diag = resolve_names_with_registry(&module, &mut st, &registry);
        assert!(
            !diag.has_errors(),
            "unexpected errors: {:?}",
            diag.iter().collect::<Vec<_>>()
        );
        let resolved = st
            .resolutions
            .get(&99)
            .expect("User should resolve from glob import");
        assert_eq!(resolved.kind, NameKind::Type);
    }
}
