//! Target profile definitions — capability matrices and conventions for each target language.

use std::fmt;

// ─── Support level ───────────────────────────────────────────────────────────

/// How well a target supports a particular language construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Support {
    /// The target has direct, first-class support.
    Native,
    /// The target supports the construct via switch statements (pattern matching).
    SwitchBased,
    /// The target supports the construct via interfaces (traits).
    InterfaceBased,
    /// The target can express the construct through a synthesis strategy.
    Emulated,
    /// The target has no support; the construct cannot be represented.
    None,
}

impl fmt::Display for Support {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native => write!(f, "native"),
            Self::SwitchBased => write!(f, "switch-based"),
            Self::InterfaceBased => write!(f, "interface-based"),
            Self::Emulated => write!(f, "emulated"),
            Self::None => write!(f, "none"),
        }
    }
}

// ─── Memory model ────────────────────────────────────────────────────────────

/// The memory management model of a target language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryModel {
    /// Garbage collected (JS, Python, Go, Java).
    GC,
    /// Automatic reference counting (Swift).
    ARC,
    /// Manual / ownership-based (Rust, C, C++).
    Manual,
}

impl fmt::Display for MemoryModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GC => write!(f, "GC"),
            Self::ARC => write!(f, "ARC"),
            Self::Manual => write!(f, "manual"),
        }
    }
}

// ─── Async model ─────────────────────────────────────────────────────────────

/// How a target handles asynchronous operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AsyncModel {
    /// Single-threaded event loop (JS).
    EventLoop,
    /// Green threads / goroutines (Go).
    GreenThread,
    /// OS threads with async runtime (Rust, Python).
    OSThread,
}

impl fmt::Display for AsyncModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EventLoop => write!(f, "event-loop"),
            Self::GreenThread => write!(f, "green-thread"),
            Self::OSThread => write!(f, "OS-thread"),
        }
    }
}

// ─── Generics model ──────────────────────────────────────────────────────────

/// How a target implements generic/parametric polymorphism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GenericsModel {
    /// Generics are preserved at runtime (Java, C#, TypeScript).
    Reified,
    /// Generics are erased at compile time (Java bytecode, Go <1.18).
    Erased,
    /// Each instantiation generates a separate copy (Rust, C++).
    Monomorphized,
}

impl fmt::Display for GenericsModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reified => write!(f, "reified"),
            Self::Erased => write!(f, "erased"),
            Self::Monomorphized => write!(f, "monomorphized"),
        }
    }
}

// ─── Naming convention ───────────────────────────────────────────────────────

/// The naming convention used by a target language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamingConvention {
    /// `camelCase` — JS, TS, Go (unexported).
    CamelCase,
    /// `snake_case` — Rust, Python.
    SnakeCase,
    /// `PascalCase` — Go (exported), C#.
    PascalCase,
}

impl fmt::Display for NamingConvention {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CamelCase => write!(f, "camelCase"),
            Self::SnakeCase => write!(f, "snake_case"),
            Self::PascalCase => write!(f, "PascalCase"),
        }
    }
}

// ─── Error handling convention ───────────────────────────────────────────────

/// How a target language handles errors idiomatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorHandling {
    /// Exceptions (JS, Python, Java).
    Exceptions,
    /// Result types (Rust).
    ResultType,
    /// Multiple return values (Go).
    MultipleReturn,
}

impl fmt::Display for ErrorHandling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exceptions => write!(f, "exceptions"),
            Self::ResultType => write!(f, "result-type"),
            Self::MultipleReturn => write!(f, "multiple-return"),
        }
    }
}

// ─── Indent style ────────────────────────────────────────────────────────────

/// Indentation style for generated code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndentStyle {
    /// Spaces with a given width.
    Spaces(u8),
    /// Tab characters.
    Tabs,
}

// ─── Target capabilities ─────────────────────────────────────────────────────

/// The capability matrix describing what language constructs a target supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetCapabilities {
    /// Memory management model.
    pub memory_model: MemoryModel,
    /// Whether the target has null-safety built into its type system.
    pub null_safety: bool,
    /// Support level for algebraic data types (sum types / tagged unions).
    pub algebraic_types: Support,
    /// Async programming model.
    pub async_model: AsyncModel,
    /// How generics are implemented.
    pub generics: GenericsModel,
    /// Whether functions are first-class values.
    pub first_class_functions: bool,
    /// Support level for pattern matching.
    pub pattern_matching: Support,
    /// Support level for traits/interfaces.
    pub traits: Support,
    /// Support level for string interpolation.
    pub string_interpolation: Support,
}

// ─── Target conventions ──────────────────────────────────────────────────────

/// Stylistic and idiomatic conventions for a target language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetConventions {
    /// Naming convention for functions and variables.
    pub naming: NamingConvention,
    /// Naming convention for types.
    pub type_naming: NamingConvention,
    /// Idiomatic error handling approach.
    pub error_handling: ErrorHandling,
    /// Indentation style.
    pub indent: IndentStyle,
    /// File extension for generated files (without dot).
    pub file_extension: String,
}

// ─── AI synthesis hints ──────────────────────────────────────────────────────

/// AIR node categories a target profile may flag as needing AI synthesis.
///
/// Populated into [`TargetProfile::ai_hints`] per target; consulted by
/// [`crate::ai_synthesis::needs_ai_synthesis`] via the `CodeGenerator` hook.
/// Trivial constructs (literals, arithmetic, direct calls, etc.) never
/// appear in `ai_hints` — Tier 2 rules are sufficient — per §17.2 (Q3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKindHint {
    /// `match` expression with non-trivial arms (ADT emulation, if/else chain synthesis).
    Match,
    /// `enum` declaration — ADT emulation for targets without native sum types.
    EnumDecl,
    /// Enum variant construction.
    EnumVariant,
    /// `handle ... with` block — effect handler translation.
    HandlingBlock,
    /// Individual effect operation invocation.
    EffectOp,
    /// Move/borrow/mutable-borrow — ownership annotations to erase on GC targets.
    Ownership,
    /// String interpolation — formatting macro/concatenation synthesis.
    Interpolation,
    /// `impl` block — trait-method dispatch emulation.
    ImplBlock,
    /// `trait` declaration — interface emulation on duck-typed targets.
    TraitDecl,
}

// ─── Target profile ──────────────────────────────────────────────────────────

/// A complete target profile combining an identifier, capability matrix, and conventions.
///
/// Each supported transpilation target (JS, TS, Python, Rust, Go, etc.) is
/// represented by a `TargetProfile` that informs the code generator about what
/// constructs are natively available and what synthesis strategies are needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetProfile {
    /// Short identifier: `"js"`, `"ts"`, `"python"`, `"rust"`, `"go"`.
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// What the target supports.
    pub capabilities: TargetCapabilities,
    /// Idiomatic conventions for the target.
    pub conventions: TargetConventions,
    /// AIR node categories that warrant Tier 1 AI synthesis on this target
    /// (§17.2, Q3 amended). Empty → every node goes through Tier 2 rules.
    pub ai_hints: Vec<NodeKindHint>,
}

impl TargetProfile {
    /// Returns the built-in JavaScript target profile.
    #[must_use]
    pub fn javascript() -> Self {
        Self {
            id: "js".into(),
            display_name: "JavaScript".into(),
            capabilities: TargetCapabilities {
                memory_model: MemoryModel::GC,
                null_safety: false,
                algebraic_types: Support::Emulated,
                async_model: AsyncModel::EventLoop,
                generics: GenericsModel::Erased,
                first_class_functions: true,
                pattern_matching: Support::SwitchBased,
                traits: Support::Emulated,
                string_interpolation: Support::Native,
            },
            conventions: TargetConventions {
                naming: NamingConvention::CamelCase,
                type_naming: NamingConvention::PascalCase,
                error_handling: ErrorHandling::Exceptions,
                indent: IndentStyle::Spaces(2),
                file_extension: "js".into(),
            },
            // JS has no native ADTs, no native match, no ownership, no effects.
            ai_hints: vec![
                NodeKindHint::EnumDecl,
                NodeKindHint::EnumVariant,
                NodeKindHint::Match,
                NodeKindHint::HandlingBlock,
                NodeKindHint::EffectOp,
                NodeKindHint::Ownership,
                NodeKindHint::TraitDecl,
                NodeKindHint::ImplBlock,
            ],
        }
    }

    /// Returns the built-in TypeScript target profile.
    #[must_use]
    pub fn typescript() -> Self {
        Self {
            id: "ts".into(),
            display_name: "TypeScript".into(),
            capabilities: TargetCapabilities {
                memory_model: MemoryModel::GC,
                null_safety: true,
                algebraic_types: Support::Emulated,
                async_model: AsyncModel::EventLoop,
                generics: GenericsModel::Erased,
                first_class_functions: true,
                pattern_matching: Support::None,
                traits: Support::InterfaceBased,
                string_interpolation: Support::Native,
            },
            conventions: TargetConventions {
                naming: NamingConvention::CamelCase,
                type_naming: NamingConvention::PascalCase,
                error_handling: ErrorHandling::Exceptions,
                indent: IndentStyle::Spaces(2),
                file_extension: "ts".into(),
            },
            // TS has tagged union types → less ADT synthesis than JS. Match
            // still needs switch-based synthesis. Effects/ownership as on JS.
            ai_hints: vec![
                NodeKindHint::Match,
                NodeKindHint::HandlingBlock,
                NodeKindHint::EffectOp,
                NodeKindHint::Ownership,
            ],
        }
    }

    /// Returns the built-in Python target profile.
    #[must_use]
    pub fn python() -> Self {
        Self {
            id: "python".into(),
            display_name: "Python".into(),
            capabilities: TargetCapabilities {
                memory_model: MemoryModel::GC,
                null_safety: false,
                algebraic_types: Support::Emulated,
                async_model: AsyncModel::OSThread,
                generics: GenericsModel::Erased,
                first_class_functions: true,
                pattern_matching: Support::Native,
                traits: Support::Emulated,
                string_interpolation: Support::Native,
            },
            conventions: TargetConventions {
                naming: NamingConvention::SnakeCase,
                type_naming: NamingConvention::PascalCase,
                error_handling: ErrorHandling::Exceptions,
                indent: IndentStyle::Spaces(4),
                file_extension: "py".into(),
            },
            // Python 3.10+ has structural pattern matching, so Match is rules-only.
            // Effects and ownership still need synthesis; flagged traits for
            // protocol/duck-typing translation.
            ai_hints: vec![
                NodeKindHint::HandlingBlock,
                NodeKindHint::EffectOp,
                NodeKindHint::Ownership,
                NodeKindHint::TraitDecl,
                NodeKindHint::ImplBlock,
            ],
        }
    }

    /// Returns the built-in Rust target profile.
    #[must_use]
    pub fn rust() -> Self {
        Self {
            id: "rust".into(),
            display_name: "Rust".into(),
            capabilities: TargetCapabilities {
                memory_model: MemoryModel::Manual,
                null_safety: true,
                algebraic_types: Support::Native,
                async_model: AsyncModel::OSThread,
                generics: GenericsModel::Monomorphized,
                first_class_functions: true,
                pattern_matching: Support::Native,
                traits: Support::Native,
                string_interpolation: Support::Emulated,
            },
            conventions: TargetConventions {
                naming: NamingConvention::SnakeCase,
                type_naming: NamingConvention::PascalCase,
                error_handling: ErrorHandling::ResultType,
                indent: IndentStyle::Spaces(4),
                file_extension: "rs".into(),
            },
            // Rust is closest to AIR semantics — only effects (still emulated
            // everywhere per §17.6) and string interpolation (no native f-string
            // literal form) warrant synthesis.
            ai_hints: vec![
                NodeKindHint::HandlingBlock,
                NodeKindHint::EffectOp,
                NodeKindHint::Interpolation,
            ],
        }
    }

    /// Returns the built-in Go target profile.
    #[must_use]
    pub fn go() -> Self {
        Self {
            id: "go".into(),
            display_name: "Go".into(),
            capabilities: TargetCapabilities {
                memory_model: MemoryModel::GC,
                null_safety: false,
                algebraic_types: Support::Emulated,
                async_model: AsyncModel::GreenThread,
                generics: GenericsModel::Reified,
                first_class_functions: true,
                pattern_matching: Support::None,
                traits: Support::InterfaceBased,
                string_interpolation: Support::Emulated,
            },
            conventions: TargetConventions {
                naming: NamingConvention::CamelCase,
                type_naming: NamingConvention::PascalCase,
                error_handling: ErrorHandling::MultipleReturn,
                indent: IndentStyle::Tabs,
                file_extension: "go".into(),
            },
            // Go has no match, no sum types, no ownership, emulated interpolation.
            ai_hints: vec![
                NodeKindHint::EnumDecl,
                NodeKindHint::EnumVariant,
                NodeKindHint::Match,
                NodeKindHint::HandlingBlock,
                NodeKindHint::EffectOp,
                NodeKindHint::Ownership,
                NodeKindHint::Interpolation,
                NodeKindHint::TraitDecl,
                NodeKindHint::ImplBlock,
            ],
        }
    }

    /// Returns all built-in target profiles.
    #[must_use]
    pub fn all_builtins() -> Vec<Self> {
        vec![
            Self::javascript(),
            Self::typescript(),
            Self::python(),
            Self::rust(),
            Self::go(),
        ]
    }

    /// Looks up a built-in profile by its short id (e.g., `"js"`, `"rust"`).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "js" | "javascript" => Some(Self::javascript()),
            "ts" | "typescript" => Some(Self::typescript()),
            "python" | "py" => Some(Self::python()),
            "rust" | "rs" => Some(Self::rust()),
            "go" | "golang" => Some(Self::go()),
            _ => None,
        }
    }
}

impl fmt::Display for TargetProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.display_name, self.id)
    }
}

// ─── NodeKind → NodeKindHint mapping ─────────────────────────────────────────

/// Classifies an AIR node into a [`NodeKindHint`] category, or `None` if the
/// node is trivial (literal, arithmetic, direct call, etc.) and must never
/// trigger AI synthesis per §17.2 (Q3).
///
/// Called by [`crate::ai_synthesis::needs_ai_synthesis`] together with
/// [`TargetProfile::ai_hints`] to decide invocation per node.
#[must_use]
pub fn classify_node(node: &bock_air::AIRNode) -> Option<NodeKindHint> {
    use bock_air::NodeKind;
    match &node.kind {
        NodeKind::Match { .. } => Some(NodeKindHint::Match),
        NodeKind::EnumDecl { .. } => Some(NodeKindHint::EnumDecl),
        NodeKind::EnumVariant { .. } => Some(NodeKindHint::EnumVariant),
        NodeKind::HandlingBlock { .. } => Some(NodeKindHint::HandlingBlock),
        NodeKind::EffectOp { .. } => Some(NodeKindHint::EffectOp),
        NodeKind::Move { .. } | NodeKind::Borrow { .. } | NodeKind::MutableBorrow { .. } => {
            Some(NodeKindHint::Ownership)
        }
        NodeKind::Interpolation { .. } => Some(NodeKindHint::Interpolation),
        NodeKind::ImplBlock { .. } => Some(NodeKindHint::ImplBlock),
        NodeKind::TraitDecl { .. } => Some(NodeKindHint::TraitDecl),
        _ => None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtins_has_five_targets() {
        let profiles = TargetProfile::all_builtins();
        assert_eq!(profiles.len(), 5);

        let ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"js"));
        assert!(ids.contains(&"ts"));
        assert!(ids.contains(&"python"));
        assert!(ids.contains(&"rust"));
        assert!(ids.contains(&"go"));
    }

    #[test]
    fn from_id_resolves_aliases() {
        assert_eq!(TargetProfile::from_id("js").unwrap().id, "js");
        assert_eq!(TargetProfile::from_id("javascript").unwrap().id, "js");
        assert_eq!(TargetProfile::from_id("ts").unwrap().id, "ts");
        assert_eq!(TargetProfile::from_id("typescript").unwrap().id, "ts");
        assert_eq!(TargetProfile::from_id("python").unwrap().id, "python");
        assert_eq!(TargetProfile::from_id("py").unwrap().id, "python");
        assert_eq!(TargetProfile::from_id("rust").unwrap().id, "rust");
        assert_eq!(TargetProfile::from_id("rs").unwrap().id, "rust");
        assert_eq!(TargetProfile::from_id("go").unwrap().id, "go");
        assert_eq!(TargetProfile::from_id("golang").unwrap().id, "go");
        assert!(TargetProfile::from_id("unknown").is_none());
    }

    #[test]
    fn js_profile_capabilities() {
        let js = TargetProfile::javascript();
        assert_eq!(js.capabilities.memory_model, MemoryModel::GC);
        assert!(!js.capabilities.null_safety);
        assert_eq!(js.capabilities.algebraic_types, Support::Emulated);
        assert_eq!(js.capabilities.async_model, AsyncModel::EventLoop);
        assert_eq!(js.capabilities.generics, GenericsModel::Erased);
        assert!(js.capabilities.first_class_functions);
        assert_eq!(js.capabilities.pattern_matching, Support::SwitchBased);
        assert_eq!(js.capabilities.string_interpolation, Support::Native);
    }

    #[test]
    fn rust_profile_capabilities() {
        let rust = TargetProfile::rust();
        assert_eq!(rust.capabilities.memory_model, MemoryModel::Manual);
        assert!(rust.capabilities.null_safety);
        assert_eq!(rust.capabilities.algebraic_types, Support::Native);
        assert_eq!(rust.capabilities.generics, GenericsModel::Monomorphized);
        assert_eq!(rust.capabilities.pattern_matching, Support::Native);
        assert_eq!(rust.capabilities.traits, Support::Native);
    }

    #[test]
    fn go_profile_capabilities() {
        let go = TargetProfile::go();
        assert_eq!(go.capabilities.memory_model, MemoryModel::GC);
        assert_eq!(go.capabilities.async_model, AsyncModel::GreenThread);
        assert_eq!(go.capabilities.pattern_matching, Support::None);
        assert_eq!(go.capabilities.traits, Support::InterfaceBased);
        assert_eq!(go.conventions.error_handling, ErrorHandling::MultipleReturn);
    }

    #[test]
    fn python_profile_conventions() {
        let py = TargetProfile::python();
        assert_eq!(py.conventions.naming, NamingConvention::SnakeCase);
        assert_eq!(py.conventions.type_naming, NamingConvention::PascalCase);
        assert_eq!(py.conventions.indent, IndentStyle::Spaces(4));
        assert_eq!(py.conventions.file_extension, "py");
    }

    #[test]
    fn ts_profile_has_null_safety_and_erased_generics() {
        let ts = TargetProfile::typescript();
        assert!(ts.capabilities.null_safety);
        assert_eq!(ts.capabilities.generics, GenericsModel::Erased);
        assert_eq!(ts.capabilities.traits, Support::InterfaceBased);
    }

    #[test]
    fn display_format() {
        let js = TargetProfile::javascript();
        assert_eq!(format!("{js}"), "JavaScript (js)");
    }

    #[test]
    fn support_display() {
        assert_eq!(format!("{}", Support::Native), "native");
        assert_eq!(format!("{}", Support::SwitchBased), "switch-based");
        assert_eq!(format!("{}", Support::InterfaceBased), "interface-based");
        assert_eq!(format!("{}", Support::Emulated), "emulated");
        assert_eq!(format!("{}", Support::None), "none");
    }

    // ── Spec table assertion tests (s13-transpilation) ──────────────────

    #[test]
    fn spec_js_profile() {
        let p = TargetProfile::javascript();
        let c = &p.capabilities;
        assert_eq!(c.memory_model, MemoryModel::GC);
        assert!(!c.null_safety);
        assert_eq!(c.algebraic_types, Support::Emulated);
        assert_eq!(c.async_model, AsyncModel::EventLoop);
        assert_eq!(c.generics, GenericsModel::Erased);
        assert!(c.first_class_functions);
        assert_eq!(c.pattern_matching, Support::SwitchBased);
        assert_eq!(c.traits, Support::Emulated);
        assert_eq!(c.string_interpolation, Support::Native);
        // conventions
        assert_eq!(p.conventions.naming, NamingConvention::CamelCase);
        assert_eq!(p.conventions.type_naming, NamingConvention::PascalCase);
        assert_eq!(p.conventions.error_handling, ErrorHandling::Exceptions);
        assert_eq!(p.conventions.indent, IndentStyle::Spaces(2));
        assert_eq!(p.conventions.file_extension, "js");
    }

    #[test]
    fn spec_ts_profile() {
        let p = TargetProfile::typescript();
        let c = &p.capabilities;
        assert_eq!(c.memory_model, MemoryModel::GC);
        assert!(c.null_safety);
        assert_eq!(c.algebraic_types, Support::Emulated);
        assert_eq!(c.async_model, AsyncModel::EventLoop);
        assert_eq!(c.generics, GenericsModel::Erased);
        assert!(c.first_class_functions);
        assert_eq!(c.pattern_matching, Support::None);
        assert_eq!(c.traits, Support::InterfaceBased);
        assert_eq!(c.string_interpolation, Support::Native);
        // conventions
        assert_eq!(p.conventions.naming, NamingConvention::CamelCase);
        assert_eq!(p.conventions.type_naming, NamingConvention::PascalCase);
        assert_eq!(p.conventions.error_handling, ErrorHandling::Exceptions);
        assert_eq!(p.conventions.indent, IndentStyle::Spaces(2));
        assert_eq!(p.conventions.file_extension, "ts");
    }

    #[test]
    fn spec_python_profile() {
        let p = TargetProfile::python();
        let c = &p.capabilities;
        assert_eq!(c.memory_model, MemoryModel::GC);
        assert!(!c.null_safety);
        assert_eq!(c.algebraic_types, Support::Emulated);
        assert_eq!(c.async_model, AsyncModel::OSThread);
        assert_eq!(c.generics, GenericsModel::Erased);
        assert!(c.first_class_functions);
        assert_eq!(c.pattern_matching, Support::Native);
        assert_eq!(c.traits, Support::Emulated);
        assert_eq!(c.string_interpolation, Support::Native);
        // conventions
        assert_eq!(p.conventions.naming, NamingConvention::SnakeCase);
        assert_eq!(p.conventions.type_naming, NamingConvention::PascalCase);
        assert_eq!(p.conventions.error_handling, ErrorHandling::Exceptions);
        assert_eq!(p.conventions.indent, IndentStyle::Spaces(4));
        assert_eq!(p.conventions.file_extension, "py");
    }

    #[test]
    fn spec_rust_profile() {
        let p = TargetProfile::rust();
        let c = &p.capabilities;
        assert_eq!(c.memory_model, MemoryModel::Manual);
        assert!(c.null_safety);
        assert_eq!(c.algebraic_types, Support::Native);
        assert_eq!(c.async_model, AsyncModel::OSThread);
        assert_eq!(c.generics, GenericsModel::Monomorphized);
        assert!(c.first_class_functions);
        assert_eq!(c.pattern_matching, Support::Native);
        assert_eq!(c.traits, Support::Native);
        assert_eq!(c.string_interpolation, Support::Emulated);
        // conventions
        assert_eq!(p.conventions.naming, NamingConvention::SnakeCase);
        assert_eq!(p.conventions.type_naming, NamingConvention::PascalCase);
        assert_eq!(p.conventions.error_handling, ErrorHandling::ResultType);
        assert_eq!(p.conventions.indent, IndentStyle::Spaces(4));
        assert_eq!(p.conventions.file_extension, "rs");
    }

    #[test]
    fn spec_go_profile() {
        let p = TargetProfile::go();
        let c = &p.capabilities;
        assert_eq!(c.memory_model, MemoryModel::GC);
        assert!(!c.null_safety);
        assert_eq!(c.algebraic_types, Support::Emulated);
        assert_eq!(c.async_model, AsyncModel::GreenThread);
        assert_eq!(c.generics, GenericsModel::Reified);
        assert!(c.first_class_functions);
        assert_eq!(c.pattern_matching, Support::None);
        assert_eq!(c.traits, Support::InterfaceBased);
        assert_eq!(c.string_interpolation, Support::Emulated);
        // conventions
        assert_eq!(p.conventions.naming, NamingConvention::CamelCase);
        assert_eq!(p.conventions.type_naming, NamingConvention::PascalCase);
        assert_eq!(p.conventions.error_handling, ErrorHandling::MultipleReturn);
        assert_eq!(p.conventions.indent, IndentStyle::Tabs);
        assert_eq!(p.conventions.file_extension, "go");
    }
}
