//! Stub layer-slot types populated by later compiler passes.
//!
//! These types are defined here as placeholders. The actual implementations
//! are filled in by the type-checker (T-AIR), context resolver (C-AIR), and
//! target analyzer (TR-AIR) passes.

use std::collections::{HashMap, HashSet};

// ─── Layer 1: Type / Ownership / Effects / Capabilities ──────────────────────

/// Opaque reference to a resolved type from the type checker's table.
///
/// The full `Type` algebra lives in `bock-types`. This lightweight wrapper
/// is stored on AIR nodes so later passes can identify the resolved type
/// without pulling in the full type-system crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeRef(pub String);

/// Type information attached to an AIR node by the type checker (T-AIR pass).
#[derive(Debug, Clone, PartialEq)]
pub struct TypeInfo {
    /// Resolved type reference, populated by the T-AIR pass.
    pub resolved_type: Option<TypeRef>,
}

/// Ownership state of a value, as tracked on AIR nodes.
///
/// Mirrors the ownership states from the type-level ownership analysis;
/// defined here so AIR nodes can carry this without depending on `bock-types`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipState {
    /// The binding owns its value.
    Owned,
    /// The value is immutably borrowed.
    Borrowed,
    /// The value is mutably borrowed.
    MutBorrowed,
    /// The value has been moved; the binding is no longer valid.
    Moved,
    /// Managed ownership — GC/refcount semantics (`@managed`).
    Managed,
}

/// Ownership/borrow information attached to an AIR node by ownership analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnershipInfo {
    /// Ownership state, populated by ownership analysis.
    pub state: Option<OwnershipState>,
}

/// A reference to an algebraic effect, identified by its fully-qualified name.
///
/// Used in the `effects` set on each AIR node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectRef {
    /// Fully-qualified name of the effect (e.g. `"Std.Io.Log"`).
    pub name: String,
}

impl EffectRef {
    /// Creates a new effect reference from a fully-qualified name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// A platform/language capability required or provided by a node.
///
/// Used in the `capabilities` set on each AIR node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Capability {
    /// Fully-qualified capability name (e.g. `"Std.Io.FileSystem"`).
    pub name: String,
}

impl Capability {
    /// Creates a new capability from a fully-qualified name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

// ─── Layer 2: Context ─────────────────────────────────────────────────────────

/// A behavioral modifier annotation that affects code generation or analysis.
///
/// These are extracted from annotations like `@concurrent`, `@managed`,
/// `@deterministic`, `@inline`, `@cold`, `@hot`, and `@deprecated`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BehavioralModifier {
    /// `@concurrent` — marks a function as safe for concurrent execution.
    Concurrent,
    /// `@managed` — opts into managed ownership (escape hatch for ownership analysis).
    Managed,
    /// `@deterministic` — asserts the function is pure/deterministic.
    Deterministic,
    /// `@inline` — hints that the function should be inlined.
    Inline,
    /// `@cold` — marks a code path as unlikely to be taken.
    Cold,
    /// `@hot` — marks a code path as frequently taken.
    Hot,
    /// `@deprecated` — marks the item as deprecated, with an optional reason.
    Deprecated(Option<String>),
}

/// Context annotations attached/validated by the context resolver (C-AIR pass).
///
/// Produced by [`crate::context::interpret_context`] from parsed AST annotations.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContextBlock {
    /// Free-form text from `@context("""...""")`.
    pub context_text: Option<String>,
    /// Structured markers extracted from `@context` text (e.g. `@intent:`, `@assumption:`).
    pub markers: Vec<ContextMarker>,
    /// Capabilities declared via `@requires(Capability.Network, ...)`.
    pub capabilities: HashSet<Capability>,
    /// Performance budget from `@performance(max_latency: 100.ms, max_memory: 50.mb)`.
    pub performance: Option<PerformanceBudget>,
    /// Invariant expressions from `@invariant(expr)`.
    pub invariants: Vec<String>,
    /// Security classification from `@security(level: "confidential", pii: true)`.
    pub security: Option<SecurityInfo>,
    /// Domain tags from `@domain("e-commerce", "checkout")`.
    pub domains: Vec<String>,
    /// Behavioral modifiers from `@concurrent`, `@managed`, `@inline`, etc.
    pub modifiers: Vec<BehavioralModifier>,
}

impl ContextBlock {
    /// Returns `true` if this context block has no annotations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.context_text.is_none()
            && self.markers.is_empty()
            && self.capabilities.is_empty()
            && self.performance.is_none()
            && self.invariants.is_empty()
            && self.security.is_none()
            && self.domains.is_empty()
            && self.modifiers.is_empty()
    }
}

/// A structured marker extracted from `@context` free-form text.
///
/// Markers follow the pattern `@tag: text` within the context string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMarker {
    /// The marker tag (e.g. `"intent"`, `"assumption"`, `"constraint"`).
    pub tag: String,
    /// The text following the marker tag.
    pub text: String,
}

/// Performance budget constraints from `@performance(...)`.
#[derive(Debug, Clone, PartialEq)]
pub struct PerformanceBudget {
    /// Maximum latency constraint (e.g. `Duration` from `100.ms`).
    pub max_latency: Option<Duration>,
    /// Maximum memory constraint (e.g. `ByteSize` from `50.mb`).
    pub max_memory: Option<ByteSize>,
}

/// A duration value with unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Duration {
    /// The numeric value.
    pub value: f64,
    /// The time unit.
    pub unit: TimeUnit,
}

/// Time unit for duration values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeUnit {
    /// Nanoseconds.
    Ns,
    /// Microseconds.
    Us,
    /// Milliseconds.
    Ms,
    /// Seconds.
    S,
}

/// A byte size value with unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ByteSize {
    /// The numeric value.
    pub value: f64,
    /// The size unit.
    pub unit: SizeUnit,
}

/// Size unit for byte size values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeUnit {
    /// Bytes.
    B,
    /// Kilobytes.
    Kb,
    /// Megabytes.
    Mb,
    /// Gigabytes.
    Gb,
}

/// Security classification from `@security(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityInfo {
    /// Classification level (e.g. `"public"`, `"internal"`, `"confidential"`, `"secret"`).
    pub level: String,
    /// Whether this data contains personally identifiable information.
    pub pii: bool,
}

/// Known security levels in ascending order of sensitivity.
///
/// Index position defines the ordering: higher index = more sensitive.
pub const SECURITY_LEVELS: &[&str] = &["public", "internal", "confidential", "secret"];

/// Returns the sensitivity rank of a security level (0 = least sensitive).
///
/// Returns `None` if the level is not recognized.
#[must_use]
pub fn security_level_rank(level: &str) -> Option<usize> {
    SECURITY_LEVELS.iter().position(|&l| l == level)
}

/// Known capability names in the Bock capability taxonomy.
///
/// These are the 16 spec-defined capabilities from s02-types / s09-context.
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "Network",
    "Storage",
    "Crypto",
    "GPU",
    "Camera",
    "Microphone",
    "Location",
    "Notifications",
    "Bluetooth",
    "Biometrics",
    "Clipboard",
    "SystemProcess",
    "FFI",
    "Environment",
    "Clock",
    "Random",
];

// ─── Layer 3: Target ──────────────────────────────────────────────────────────

/// Target-specific information attached by the target analyzer (TR-AIR pass).
#[derive(Debug, Clone, PartialEq)]
pub struct TargetInfo {
    /// Placeholder field — filled in by the TR-AIR pass.
    pub _placeholder: (),
}

// ─── Metadata value type ──────────────────────────────────────────────────────

/// A dynamically-typed metadata value stored in `AIRNode::metadata`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    List(Vec<Value>),
    Map(HashMap<String, Value>),
}
