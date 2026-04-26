//! Cross-file module registry for the Bock compiler.
//!
//! The [`ModuleRegistry`] is built incrementally as modules are compiled in
//! dependency order. Each module's public symbols are collected into a
//! [`ModuleExports`] entry after compilation, and downstream modules query
//! the registry during name resolution and type checking to resolve imports.
//!
//! # Type Representation
//!
//! This module uses [`TypeRef`] (a lightweight string-based handle) rather
//! than the full `Type` algebra from `bock-types`, because `bock-air` sits
//! upstream of `bock-types` in the crate dependency chain. The actual type
//! system integration happens in later compilation passes.

use std::collections::HashMap;

use bock_ast::Visibility;

use crate::stubs::TypeRef;

// ─── Module identifier ───────────────────────────────────────────────────────

/// Unique module identifier: dot-separated path (e.g., `"app.models"`).
pub type ModuleId = String;

// ─── Error types ─────────────────────────────────────────────────────────────

/// Errors returned by registry lookup operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// No module with the given ID has been registered.
    ModuleNotFound { module_id: String },
    /// The module exists but does not export a symbol with this name.
    SymbolNotFound { module_id: String, name: String },
    /// The symbol exists but is not visible from the requesting context.
    NotVisible { module_id: String, name: String },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::ModuleNotFound { module_id } => {
                write!(f, "module not found: `{module_id}`")
            }
            RegistryError::SymbolNotFound { module_id, name } => {
                write!(f, "symbol `{name}` not found in module `{module_id}`")
            }
            RegistryError::NotVisible { module_id, name } => {
                write!(
                    f,
                    "symbol `{name}` in module `{module_id}` is not visible"
                )
            }
        }
    }
}

impl std::error::Error for RegistryError {}

// ─── Registry ────────────────────────────────────────────────────────────────

/// The central cross-file symbol registry.
///
/// Built incrementally as modules are compiled in dependency order.
/// Queried by `resolve.rs` and `checker.rs` when processing imports.
#[derive(Debug, Default)]
pub struct ModuleRegistry {
    /// Per-module export tables, keyed by dot-separated module path.
    modules: HashMap<ModuleId, ModuleExports>,
}

impl ModuleRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a module's exports after it has been fully compiled.
    ///
    /// If a module with the same ID was already registered, it is replaced.
    pub fn register(&mut self, exports: ModuleExports) {
        self.modules.insert(exports.module_id.clone(), exports);
    }

    /// Checks whether a module with the given ID has been registered.
    #[must_use]
    pub fn has_module(&self, module_id: &str) -> bool {
        self.modules.contains_key(module_id)
    }

    /// Looks up a module by its dot-path ID.
    #[must_use]
    pub fn get_module(&self, module_id: &str) -> Option<&ModuleExports> {
        self.modules.get(module_id)
    }

    /// Resolves a specific symbol from a module.
    ///
    /// Returns only symbols that are visible outside the module
    /// (`Public` or `Internal`). Private symbols produce a
    /// [`RegistryError::NotVisible`] error.
    ///
    /// Re-exports are followed transitively.
    pub fn resolve_symbol(
        &self,
        module_id: &str,
        name: &str,
    ) -> Result<&ExportedSymbol, RegistryError> {
        let exports = self
            .modules
            .get(module_id)
            .ok_or_else(|| RegistryError::ModuleNotFound {
                module_id: module_id.to_string(),
            })?;

        // Check direct exports first.
        if let Some(sym) = exports.symbols.get(name) {
            return if sym.visibility == Visibility::Private {
                Err(RegistryError::NotVisible {
                    module_id: module_id.to_string(),
                    name: name.to_string(),
                })
            } else {
                Ok(sym)
            };
        }

        // Check re-exports: follow the chain to the source module.
        if let Some((source_module, original_name)) = exports.reexports.get(name) {
            return self.resolve_symbol(source_module, original_name);
        }

        Err(RegistryError::SymbolNotFound {
            module_id: module_id.to_string(),
            name: name.to_string(),
        })
    }

    /// Returns all publicly visible symbols from a module (for glob imports).
    ///
    /// Includes both direct exports and re-exports that have `Public` or
    /// `Internal` visibility.
    pub fn resolve_glob(
        &self,
        module_id: &str,
    ) -> Result<Vec<(&str, &ExportedSymbol)>, RegistryError> {
        let exports = self
            .modules
            .get(module_id)
            .ok_or_else(|| RegistryError::ModuleNotFound {
                module_id: module_id.to_string(),
            })?;

        let mut result: Vec<(&str, &ExportedSymbol)> = exports
            .symbols
            .iter()
            .filter(|(_, sym)| sym.visibility != Visibility::Private)
            .map(|(name, sym)| (name.as_str(), sym))
            .collect();

        // Resolve re-exports and include them.
        for (local_name, (source_module, original_name)) in &exports.reexports {
            if let Ok(sym) = self.resolve_symbol(source_module, original_name) {
                result.push((local_name.as_str(), sym));
            }
        }

        result.sort_by_key(|(name, _)| *name);
        Ok(result)
    }

    /// Gets the type reference for a specific exported symbol.
    ///
    /// Convenience wrapper around [`resolve_symbol`](Self::resolve_symbol)
    /// that returns just the [`TypeRef`].
    pub fn get_type(
        &self,
        module_id: &str,
        name: &str,
    ) -> Result<&TypeRef, RegistryError> {
        self.resolve_symbol(module_id, name).map(|sym| &sym.ty)
    }

    /// Returns the number of registered modules.
    #[must_use]
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }
}

// ─── Module exports ──────────────────────────────────────────────────────────

/// Everything a downstream module needs to know about an upstream module.
#[derive(Debug, Clone)]
pub struct ModuleExports {
    /// The module's dot-separated path (e.g., `"app.models"`).
    pub module_id: ModuleId,
    /// Source file path (for diagnostics).
    pub source_path: String,
    /// Exported symbols keyed by name.
    pub symbols: HashMap<String, ExportedSymbol>,
    /// Re-exports: names this module re-exports from other modules.
    /// Key = local name, Value = (source_module_id, original_name).
    pub reexports: HashMap<String, (ModuleId, String)>,
}

impl ModuleExports {
    /// Creates a new, empty export table for a module.
    #[must_use]
    pub fn new(module_id: impl Into<String>, source_path: impl Into<String>) -> Self {
        Self {
            module_id: module_id.into(),
            source_path: source_path.into(),
            symbols: HashMap::new(),
            reexports: HashMap::new(),
        }
    }

    /// Adds a symbol to the export table.
    pub fn add_symbol(&mut self, name: impl Into<String>, symbol: ExportedSymbol) {
        self.symbols.insert(name.into(), symbol);
    }

    /// Adds a re-export entry.
    pub fn add_reexport(
        &mut self,
        local_name: impl Into<String>,
        source_module: impl Into<String>,
        original_name: impl Into<String>,
    ) {
        self.reexports
            .insert(local_name.into(), (source_module.into(), original_name.into()));
    }
}

// ─── Exported symbol ─────────────────────────────────────────────────────────

/// A single exported symbol from a module.
#[derive(Debug, Clone)]
pub struct ExportedSymbol {
    /// What kind of entity this is.
    pub kind: ExportKind,
    /// Declared visibility (`Public`, `Internal`, or `Private`).
    pub visibility: Visibility,
    /// Lightweight type reference for this symbol.
    ///
    /// Uses [`TypeRef`] (a string handle) rather than the full `Type` from
    /// `bock-types`, since `bock-air` is upstream in the dependency chain.
    pub ty: TypeRef,
    /// Additional type information needed by importers.
    pub detail: ExportDetail,
}

// ─── Export kind ─────────────────────────────────────────────────────────────

/// Classification of an exported symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    /// A function or method.
    Function,
    /// A record (value-type) declaration.
    Record,
    /// An enum (algebraic data type) declaration.
    Enum,
    /// A trait declaration.
    Trait,
    /// An algebraic effect declaration.
    Effect,
    /// A type alias.
    TypeAlias,
    /// A constant declaration.
    Constant,
}

// ─── Export detail ───────────────────────────────────────────────────────────

/// Type-level details that importers need beyond the primary [`TypeRef`].
#[derive(Debug, Clone)]
pub enum ExportDetail {
    /// No additional detail needed (functions, constants).
    None,

    /// Record: field names, types, generic parameters, and inherent methods.
    Record {
        /// (field_name, field_type_ref) pairs.
        fields: Vec<(String, TypeRef)>,
        /// Names of generic type parameters.
        generic_params: Vec<String>,
        /// Inherent impl methods: method_name → method_type_ref.
        methods: HashMap<String, TypeRef>,
    },

    /// Enum: variant constructors and their types.
    Enum {
        /// Variant definitions.
        variants: Vec<EnumVariantExport>,
        /// Names of generic type parameters.
        generic_params: Vec<String>,
    },

    /// Trait: method signatures.
    Trait {
        /// method_name → method_type_ref.
        methods: HashMap<String, TypeRef>,
    },

    /// Effect: operation signatures and component effects.
    Effect {
        /// (operation_name, operation_type_ref) pairs.
        operations: Vec<(String, TypeRef)>,
        /// Component effect names (for composite effects).
        components: Vec<String>,
    },

    /// Type alias: the underlying type.
    TypeAlias {
        /// The type this alias expands to.
        underlying: TypeRef,
    },
}

// ─── Enum variant export ─────────────────────────────────────────────────────

/// An exported enum variant's constructor information.
#[derive(Debug, Clone)]
pub struct EnumVariantExport {
    /// Variant name (e.g., `"Some"`, `"None"`).
    pub name: String,
    /// For tuple variants: the constructor function type.
    /// `None` for unit variants.
    pub constructor_type: Option<TypeRef>,
    /// For struct variants: (field_name, field_type_ref) pairs.
    /// `None` for unit and tuple variants.
    pub fields: Option<Vec<(String, TypeRef)>>,
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_fn_symbol(name: &str, vis: Visibility) -> (String, ExportedSymbol) {
        (
            name.to_string(),
            ExportedSymbol {
                kind: ExportKind::Function,
                visibility: vis,
                ty: TypeRef(format!("Fn() -> Void")),
                detail: ExportDetail::None,
            },
        )
    }

    fn make_record_symbol(
        name: &str,
        vis: Visibility,
        fields: Vec<(&str, &str)>,
        generics: Vec<&str>,
    ) -> (String, ExportedSymbol) {
        (
            name.to_string(),
            ExportedSymbol {
                kind: ExportKind::Record,
                visibility: vis,
                ty: TypeRef(name.to_string()),
                detail: ExportDetail::Record {
                    fields: fields
                        .into_iter()
                        .map(|(n, t)| (n.to_string(), TypeRef(t.to_string())))
                        .collect(),
                    generic_params: generics.into_iter().map(String::from).collect(),
                    methods: HashMap::new(),
                },
            },
        )
    }

    fn make_enum_symbol(
        name: &str,
        vis: Visibility,
        variants: Vec<EnumVariantExport>,
        generics: Vec<&str>,
    ) -> (String, ExportedSymbol) {
        (
            name.to_string(),
            ExportedSymbol {
                kind: ExportKind::Enum,
                visibility: vis,
                ty: TypeRef(name.to_string()),
                detail: ExportDetail::Enum {
                    variants,
                    generic_params: generics.into_iter().map(String::from).collect(),
                },
            },
        )
    }

    fn sample_module() -> ModuleExports {
        let mut exports = ModuleExports::new("app.models", "src/app/models.bock");

        // Public function
        let (name, sym) = make_fn_symbol("create_user", Visibility::Public);
        exports.add_symbol(name, sym);

        // Public record
        let (name, sym) = make_record_symbol(
            "User",
            Visibility::Public,
            vec![("id", "Int"), ("name", "String")],
            vec![],
        );
        exports.add_symbol(name, sym);

        // Public enum
        let (name, sym) = make_enum_symbol(
            "Status",
            Visibility::Public,
            vec![
                EnumVariantExport {
                    name: "Active".to_string(),
                    constructor_type: None,
                    fields: None,
                },
                EnumVariantExport {
                    name: "Suspended".to_string(),
                    constructor_type: Some(TypeRef("Fn(String) -> Status".to_string())),
                    fields: None,
                },
            ],
            vec![],
        );
        exports.add_symbol(name, sym);

        // Private (internal) helper — should NOT be visible outside
        let (name, sym) = make_fn_symbol("hash_password", Visibility::Private);
        exports.add_symbol(name, sym);

        // Internal function — visible within the package
        let (name, sym) = make_fn_symbol("validate_email", Visibility::Internal);
        exports.add_symbol(name, sym);

        exports
    }

    // ── Registration and basic lookup ────────────────────────────────────

    #[test]
    fn register_and_lookup_module() {
        let mut reg = ModuleRegistry::new();
        assert!(!reg.has_module("app.models"));
        assert_eq!(reg.module_count(), 0);

        reg.register(sample_module());

        assert!(reg.has_module("app.models"));
        assert_eq!(reg.module_count(), 1);

        let m = reg.get_module("app.models").unwrap();
        assert_eq!(m.module_id, "app.models");
        assert_eq!(m.source_path, "src/app/models.bock");
    }

    // ── Resolve a specific name ──────────────────────────────────────────

    #[test]
    fn resolve_public_function() {
        let mut reg = ModuleRegistry::new();
        reg.register(sample_module());

        let sym = reg.resolve_symbol("app.models", "create_user").unwrap();
        assert_eq!(sym.kind, ExportKind::Function);
        assert_eq!(sym.visibility, Visibility::Public);
    }

    #[test]
    fn resolve_public_record() {
        let mut reg = ModuleRegistry::new();
        reg.register(sample_module());

        let sym = reg.resolve_symbol("app.models", "User").unwrap();
        assert_eq!(sym.kind, ExportKind::Record);
        match &sym.detail {
            ExportDetail::Record { fields, generic_params, .. } => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].0, "id");
                assert!(generic_params.is_empty());
            }
            _ => panic!("expected Record detail"),
        }
    }

    #[test]
    fn resolve_public_enum() {
        let mut reg = ModuleRegistry::new();
        reg.register(sample_module());

        let sym = reg.resolve_symbol("app.models", "Status").unwrap();
        assert_eq!(sym.kind, ExportKind::Enum);
        match &sym.detail {
            ExportDetail::Enum { variants, .. } => {
                assert_eq!(variants.len(), 2);
                assert_eq!(variants[0].name, "Active");
                assert!(variants[0].constructor_type.is_none());
                assert_eq!(variants[1].name, "Suspended");
                assert!(variants[1].constructor_type.is_some());
            }
            _ => panic!("expected Enum detail"),
        }
    }

    #[test]
    fn resolve_internal_symbol() {
        let mut reg = ModuleRegistry::new();
        reg.register(sample_module());

        let sym = reg.resolve_symbol("app.models", "validate_email").unwrap();
        assert_eq!(sym.kind, ExportKind::Function);
        assert_eq!(sym.visibility, Visibility::Internal);
    }

    // ── Resolve glob imports ─────────────────────────────────────────────

    #[test]
    fn resolve_glob_excludes_private() {
        let mut reg = ModuleRegistry::new();
        reg.register(sample_module());

        let syms = reg.resolve_glob("app.models").unwrap();
        let names: Vec<&str> = syms.iter().map(|(n, _)| *n).collect();

        // Should include public and internal, but NOT private
        assert!(names.contains(&"create_user"));
        assert!(names.contains(&"User"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"validate_email"));
        assert!(!names.contains(&"hash_password"));
    }

    #[test]
    fn resolve_glob_includes_reexports() {
        let mut reg = ModuleRegistry::new();

        // Register upstream module
        let mut upstream = ModuleExports::new("lib.utils", "src/lib/utils.bock");
        let (name, sym) = make_fn_symbol("format_date", Visibility::Public);
        upstream.add_symbol(name, sym);
        reg.register(upstream);

        // Register downstream module that re-exports from upstream
        let mut downstream = ModuleExports::new("app.helpers", "src/app/helpers.bock");
        let (name, sym) = make_fn_symbol("helper_fn", Visibility::Public);
        downstream.add_symbol(name, sym);
        downstream.add_reexport("format_date", "lib.utils", "format_date");
        reg.register(downstream);

        let syms = reg.resolve_glob("app.helpers").unwrap();
        let names: Vec<&str> = syms.iter().map(|(n, _)| *n).collect();

        assert!(names.contains(&"helper_fn"));
        assert!(names.contains(&"format_date"));
    }

    // ── Missing module → error ───────────────────────────────────────────

    #[test]
    fn missing_module_error() {
        let reg = ModuleRegistry::new();

        let err = reg.resolve_symbol("no.such.module", "foo").unwrap_err();
        assert_eq!(
            err,
            RegistryError::ModuleNotFound {
                module_id: "no.such.module".to_string(),
            }
        );
    }

    #[test]
    fn missing_module_glob_error() {
        let reg = ModuleRegistry::new();

        let err = reg.resolve_glob("no.such.module").unwrap_err();
        assert_eq!(
            err,
            RegistryError::ModuleNotFound {
                module_id: "no.such.module".to_string(),
            }
        );
    }

    #[test]
    fn missing_module_get_type_error() {
        let reg = ModuleRegistry::new();

        let err = reg.get_type("no.such.module", "Foo").unwrap_err();
        assert!(matches!(err, RegistryError::ModuleNotFound { .. }));
    }

    // ── Missing name → error ─────────────────────────────────────────────

    #[test]
    fn missing_name_error() {
        let mut reg = ModuleRegistry::new();
        reg.register(sample_module());

        let err = reg
            .resolve_symbol("app.models", "nonexistent")
            .unwrap_err();
        assert_eq!(
            err,
            RegistryError::SymbolNotFound {
                module_id: "app.models".to_string(),
                name: "nonexistent".to_string(),
            }
        );
    }

    // ── Visibility: private names not visible outside ────────────────────

    #[test]
    fn private_symbol_not_visible() {
        let mut reg = ModuleRegistry::new();
        reg.register(sample_module());

        let err = reg
            .resolve_symbol("app.models", "hash_password")
            .unwrap_err();
        assert_eq!(
            err,
            RegistryError::NotVisible {
                module_id: "app.models".to_string(),
                name: "hash_password".to_string(),
            }
        );
    }

    // ── get_type convenience ─────────────────────────────────────────────

    #[test]
    fn get_type_returns_type_ref() {
        let mut reg = ModuleRegistry::new();
        reg.register(sample_module());

        let ty = reg.get_type("app.models", "User").unwrap();
        assert_eq!(ty.0, "User");
    }

    // ── Re-export resolution ─────────────────────────────────────────────

    #[test]
    fn resolve_reexport_transitively() {
        let mut reg = ModuleRegistry::new();

        // Module A exports "greet"
        let mut mod_a = ModuleExports::new("mod_a", "a.bock");
        let (name, sym) = make_fn_symbol("greet", Visibility::Public);
        mod_a.add_symbol(name, sym);
        reg.register(mod_a);

        // Module B re-exports "greet" from A
        let mut mod_b = ModuleExports::new("mod_b", "b.bock");
        mod_b.add_reexport("greet", "mod_a", "greet");
        reg.register(mod_b);

        // Module C re-exports "greet" from B (transitive chain)
        let mut mod_c = ModuleExports::new("mod_c", "c.bock");
        mod_c.add_reexport("greet", "mod_b", "greet");
        reg.register(mod_c);

        // Should resolve through B → A
        let sym = reg.resolve_symbol("mod_c", "greet").unwrap();
        assert_eq!(sym.kind, ExportKind::Function);
        assert_eq!(sym.visibility, Visibility::Public);
    }

    // ── Multiple modules ─────────────────────────────────────────────────

    #[test]
    fn multiple_modules_independent() {
        let mut reg = ModuleRegistry::new();

        let mut m1 = ModuleExports::new("pkg.alpha", "alpha.bock");
        let (name, sym) = make_fn_symbol("alpha_fn", Visibility::Public);
        m1.add_symbol(name, sym);

        let mut m2 = ModuleExports::new("pkg.beta", "beta.bock");
        let (name, sym) = make_fn_symbol("beta_fn", Visibility::Public);
        m2.add_symbol(name, sym);

        reg.register(m1);
        reg.register(m2);

        assert_eq!(reg.module_count(), 2);
        assert!(reg.resolve_symbol("pkg.alpha", "alpha_fn").is_ok());
        assert!(reg.resolve_symbol("pkg.beta", "beta_fn").is_ok());
        assert!(reg.resolve_symbol("pkg.alpha", "beta_fn").is_err());
        assert!(reg.resolve_symbol("pkg.beta", "alpha_fn").is_err());
    }

    // ── Replace existing module ──────────────────────────────────────────

    #[test]
    fn register_replaces_existing() {
        let mut reg = ModuleRegistry::new();

        let mut m1 = ModuleExports::new("app.core", "core.bock");
        let (name, sym) = make_fn_symbol("old_fn", Visibility::Public);
        m1.add_symbol(name, sym);
        reg.register(m1);

        assert!(reg.resolve_symbol("app.core", "old_fn").is_ok());

        // Re-register with different exports
        let mut m2 = ModuleExports::new("app.core", "core.bock");
        let (name, sym) = make_fn_symbol("new_fn", Visibility::Public);
        m2.add_symbol(name, sym);
        reg.register(m2);

        assert!(reg.resolve_symbol("app.core", "old_fn").is_err());
        assert!(reg.resolve_symbol("app.core", "new_fn").is_ok());
    }
}
