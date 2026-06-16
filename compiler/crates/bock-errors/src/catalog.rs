//! Catalog of diagnostic codes emitted by the compiler.
//!
//! Source crates emit diagnostics with inline numeric codes (e.g. `E1001`).
//! This catalog is the single queryable registry of those codes together
//! with their human-facing metadata — used by editor extensions, the
//! vocabulary emitter, and online documentation.
//!
//! The catalog is deliberately separate from emission sites: keeping it
//! centralized means tools can render it without depending on the internal
//! structure of every compiler pass. When a new code is introduced at an
//! emission site, add an entry here too.

use crate::Severity;

/// Metadata for a single diagnostic code.
pub struct DiagnosticCodeInfo {
    /// Zero-padded code string (e.g. `"E1001"`).
    pub code: &'static str,
    /// Severity classification.
    pub severity: Severity,
    /// Short single-sentence summary shown by editors.
    pub summary: &'static str,
    /// Longer description (optional; markdown permitted).
    pub description: &'static str,
    /// Spec section references (e.g. `&["§6.1", "§17.4"]`).
    pub spec_refs: &'static [&'static str],
}

/// The full catalog of diagnostic codes.
///
/// Codes are grouped by the *domain* of the violated rule (which usually,
/// but not always, matches the crate of origin):
/// - `1xxx` — lexer (`bock-lexer`) and name resolution (`bock-air`)
/// - `2xxx` — parser (`bock-parser`)
/// - `4xxx` — type checker (`bock-types/checker`)
/// - `5xxx` — ownership (`bock-types/ownership`)
/// - `6xxx` — effects (`bock-types/effects`; E6005 is emitted by the
///   `bock-air` resolver and E6006 by the checker, but both report
///   effect-system rules so they live in this family)
/// - `7xxx` — capabilities (`bock-types/capabilities`)
/// - `8xxx` — context-annotation system (`bock-air`): `@context`/`@capability`
///   interpretation (`context.rs`), context validation (`validate_context.rs`),
///   and capability verification / composition / PII flow
///   (`verify_capabilities.rs`, `compose_context.rs`)
#[must_use]
pub fn diagnostic_catalog() -> Vec<DiagnosticCodeInfo> {
    vec![
        // ── Lexer (1xxx) ────────────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E1001",
            severity: Severity::Error,
            summary: "Unexpected character in source.",
            description: "The lexer encountered a character it does not recognize as part of any valid token. (Lexer-only: the name-resolution pass reports an undefined name as `E1009`, an unfound module as `E1010`, and an unfound symbol as `E1011`.)",
            spec_refs: &["§1"],
        },
        DiagnosticCodeInfo {
            code: "E1002",
            severity: Severity::Error,
            summary: "Unterminated string literal.",
            description: "A string literal was opened but never closed before end of input.",
            spec_refs: &["§1.3"],
        },
        DiagnosticCodeInfo {
            code: "E1003",
            severity: Severity::Error,
            summary: "Invalid escape sequence in string.",
            description: "An escape sequence in a string literal is not one of the recognized forms.",
            spec_refs: &["§1.3"],
        },
        DiagnosticCodeInfo {
            code: "E1004",
            severity: Severity::Error,
            summary: "Invalid character literal.",
            description: "A character literal is empty, contains more than one character, or has an invalid escape.",
            spec_refs: &["§1.3"],
        },
        DiagnosticCodeInfo {
            code: "E1005",
            severity: Severity::Error,
            summary: "Invalid digit for numeric literal.",
            description: "A digit was found that is outside the range of the declared numeric base (e.g. `8` in an octal literal or a non-hex digit after `0x`).",
            spec_refs: &["§1.3"],
        },
        DiagnosticCodeInfo {
            code: "E1006",
            severity: Severity::Error,
            summary: "Unterminated block comment.",
            description: "A `/* ... */` block comment was opened but never closed before end of input.",
            spec_refs: &["§1.2"],
        },
        // ── Name resolution (also 1xxx) ─────────────────────────────────
        // The name-resolution pass (`bock-air/resolve.rs`) owns its own slots
        // in the `1xxx` range; it does NOT share codes with the lexer. The
        // former E1001/E1005/E1006 lexer↔resolver overlaps were split: the
        // resolver's undefined-name → E1009, module-not-found → E1010, and
        // symbol-not-found → E1011 (Q-error-code-renumbering).
        DiagnosticCodeInfo {
            code: "W1001",
            severity: Severity::Warning,
            summary: "Unused import.",
            description: "An import was declared but never referenced. Uses of an imported effect name in `with`, `handling`, and `impl … for` positions count as references (Q-w1001-effect-import-false-positive).",
            spec_refs: &["§10"],
        },
        DiagnosticCodeInfo {
            code: "E1007",
            severity: Severity::Error,
            summary: "Symbol is not visible.",
            description: "The referenced symbol exists but is private; declare it `public` to export it.",
            spec_refs: &["§10"],
        },
        DiagnosticCodeInfo {
            code: "E1008",
            severity: Severity::Error,
            summary: "Circular module dependency.",
            description: "The `use` import graph contains a cycle, so the modules cannot be compiled in any dependency order. The message names every module in the cycle in order and points at one offending `use` edge. Fix by removing one of the `use` edges in the cycle, or by extracting the shared items into a third module that both can import.",
            spec_refs: &["§10"],
        },
        DiagnosticCodeInfo {
            code: "E1009",
            severity: Severity::Error,
            summary: "Undefined name.",
            description: "An identifier reference could not be resolved to any binding in scope. Most often a name is not imported or is misspelled; check the `use` declarations. (A bare effect operation such as `log` called outside a `with` clause or `handling` block is reported separately as `E6005`, not this code.) Emitted by the name-resolution pass.",
            spec_refs: &["§10"],
        },
        DiagnosticCodeInfo {
            code: "E1010",
            severity: Severity::Error,
            summary: "Imported module not found.",
            description: "A `use` path named a module the registry could not locate. Check the import path and that the target file declares `module <path>`. Emitted by the name-resolution pass.",
            spec_refs: &["§10"],
        },
        DiagnosticCodeInfo {
            code: "E1011",
            severity: Severity::Error,
            summary: "Imported symbol not found in module.",
            description: "A `use` path names a module that exists but does not export the requested symbol. Emitted by the name-resolution pass.",
            spec_refs: &["§10"],
        },
        // ── Parser (2xxx) ───────────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E2000",
            severity: Severity::Error,
            summary: "Parse error at top level.",
            description: "The parser encountered unexpected input while reading a top-level item.",
            spec_refs: &["§11"],
        },
        DiagnosticCodeInfo {
            code: "E2001",
            severity: Severity::Error,
            summary: "Unexpected token.",
            description: "The next token did not match any alternative at the current grammar position.",
            spec_refs: &["§11"],
        },
        DiagnosticCodeInfo {
            code: "E2002",
            severity: Severity::Error,
            summary: "Missing expected token.",
            description: "The parser required a specific token here and found something else.",
            spec_refs: &["§11"],
        },
        DiagnosticCodeInfo {
            code: "E2010",
            severity: Severity::Error,
            summary: "Invalid declaration.",
            description: "A top-level declaration has malformed structure.",
            spec_refs: &["§4"],
        },
        DiagnosticCodeInfo {
            code: "E2020",
            severity: Severity::Error,
            summary: "Invalid expression.",
            description: "An expression could not be parsed due to malformed structure.",
            spec_refs: &["§5"],
        },
        DiagnosticCodeInfo {
            code: "E2021",
            severity: Severity::Error,
            summary: "Invalid pattern.",
            description: "A pattern in a `match` or `let` binding could not be parsed.",
            spec_refs: &["§7"],
        },
        DiagnosticCodeInfo {
            code: "E2022",
            severity: Severity::Error,
            summary: "Invalid type expression.",
            description: "A type annotation could not be parsed as a valid type expression.",
            spec_refs: &["§2"],
        },
        DiagnosticCodeInfo {
            code: "E2030",
            severity: Severity::Error,
            summary: "Parentheses required (lambda parameters / `if` condition).",
            description: "A construct that requires parentheses was written without them. Lambda parameters must be parenthesized; single-identifier forms (`x => …`) are not accepted. An `if` condition must be parenthesized (`if (cond) { … }`); the diagnostic names the offending token and a note gives the wrapped form. (A missing function name after `fn` is reported separately as `E2073`.)",
            spec_refs: &["§5"],
        },
        DiagnosticCodeInfo {
            code: "E2031",
            severity: Severity::Error,
            summary: "Invalid lambda / function parameter.",
            description: "A parameter position expected a name and found something else — e.g. a missing name after `mut`, or a non-identifier where a parameter name was required.",
            spec_refs: &["§5"],
        },
        DiagnosticCodeInfo {
            code: "E2040",
            severity: Severity::Error,
            summary: "Invalid generic parameter list.",
            description: "A generic parameter list is malformed.",
            spec_refs: &["§4.5"],
        },
        DiagnosticCodeInfo {
            code: "E2050",
            severity: Severity::Error,
            summary: "Invalid use declaration.",
            description: "A `use` import is malformed.",
            spec_refs: &["§10"],
        },
        DiagnosticCodeInfo {
            code: "E2060",
            severity: Severity::Error,
            summary: "Invalid attribute / annotation.",
            description: "An `@annotation` or `#attribute` could not be parsed.",
            spec_refs: &["§4.7"],
        },
        DiagnosticCodeInfo {
            code: "E2061",
            severity: Severity::Error,
            summary: "Expected constant name.",
            description: "A `const` declaration was missing its name: a constant name was expected and a different token was found.",
            spec_refs: &["§4"],
        },
        DiagnosticCodeInfo {
            code: "E2070",
            severity: Severity::Error,
            summary: "Invalid match arm.",
            description: "A match arm is malformed; each arm is `pattern => expression`.",
            spec_refs: &["§7"],
        },
        DiagnosticCodeInfo {
            code: "E2071",
            severity: Severity::Error,
            summary: "Expected associated type name.",
            description: "An associated-type position inside a `trait`/`impl` expected a type name (`type Name`) and found a different token.",
            spec_refs: &["§4.4"],
        },
        DiagnosticCodeInfo {
            code: "E2072",
            severity: Severity::Error,
            summary: "Expected method name.",
            description: "A method declaration inside a `trait`/`impl`/`class` expected a method name after `fn` and found a different token.",
            spec_refs: &["§4.4"],
        },
        DiagnosticCodeInfo {
            code: "E2073",
            severity: Severity::Error,
            summary: "Expected function name.",
            description: "A top-level (or nested) function declaration expected a function name after `fn` and found a different token.",
            spec_refs: &["§4"],
        },
        DiagnosticCodeInfo {
            code: "E2090",
            severity: Severity::Error,
            summary: "Invalid effect declaration.",
            description: "An `effect` declaration or `with` clause is malformed.",
            spec_refs: &["§8"],
        },
        DiagnosticCodeInfo {
            code: "E2091",
            severity: Severity::Error,
            summary: "Expected effect operation name.",
            description: "An effect operation declaration inside an `effect` block expected an operation name after `fn` and found a different token.",
            spec_refs: &["§8"],
        },
        DiagnosticCodeInfo {
            code: "E2092",
            severity: Severity::Error,
            summary: "Tuple positional indexing is not available in v1.",
            description: "`t.0` / `t.1` positional tuple indexing is not a v1 form. Destructure with `let (a, b) = t` to bind tuple elements instead.",
            spec_refs: &["§5"],
        },
        // ── Type checker (4xxx) ────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E4001",
            severity: Severity::Error,
            summary: "Type mismatch.",
            description: "Expected and actual types do not unify. The message reads `expected `T`, found `U``: `T` is the type the surrounding context requires and `U` is the type the expression actually has, both in surface Bock syntax. When a direct conversion to the expected type exists, a note suggests it (`.to_float()`, `.to_int()`, `.to_string()`, or `Int.try_from`/`Float.try_from` for parsing a String).",
            spec_refs: &["§2"],
        },
        DiagnosticCodeInfo {
            code: "E4002",
            severity: Severity::Error,
            summary: "Undefined variable.",
            description: "A name referenced in an expression has no binding in scope.",
            spec_refs: &["§2"],
        },
        DiagnosticCodeInfo {
            code: "E4003",
            severity: Severity::Error,
            summary: "Arity mismatch in call.",
            description: "The number of arguments does not match the callee's parameter count.",
            spec_refs: &["§5"],
        },
        DiagnosticCodeInfo {
            code: "E4004",
            severity: Severity::Error,
            summary: "Value is not callable.",
            description: "An expression of non-function type was used in a call position.",
            spec_refs: &["§5"],
        },
        DiagnosticCodeInfo {
            code: "E4005",
            severity: Severity::Error,
            summary: "`where` clause predicate failed.",
            description: "A refined-type predicate could not be satisfied.",
            spec_refs: &["§2"],
        },
        DiagnosticCodeInfo {
            code: "E4010",
            severity: Severity::Error,
            summary: "Overlapping trait implementations.",
            description: "Two `impl` blocks apply to the same type and violate coherence.",
            spec_refs: &["§4.4"],
        },
        DiagnosticCodeInfo {
            code: "E4011",
            severity: Severity::Error,
            summary: "Cannot implement a core trait for a primitive type.",
            description: "Core traits (`Equatable`, `Comparable`, `Displayable`, `Hashable`) have sealed, compiler-provided conformances for primitive types. User code may not add its own `impl` of a core trait for a primitive (an orphan-rule violation). Wrap the primitive in a newtype and implement the trait for that instead.",
            spec_refs: &["§18.5"],
        },
        DiagnosticCodeInfo {
            code: "E4012",
            severity: Severity::Error,
            summary: "Conversion could not be resolved.",
            description: "A `.into()` call (or `from`/`try_from`) could not be resolved: no `From`/`Into`/`TryFrom` impl relates the source and target types. For `.into()` the target comes from the expected type, so the call site must have a reachable annotation (a `let y: U =`, an `fn -> U` return position, or a typed argument).",
            spec_refs: &["§18.4"],
        },
        DiagnosticCodeInfo {
            code: "E4013",
            severity: Severity::Error,
            summary: "No such method on a concrete type.",
            description: "A method that does not exist on the receiver's concrete type was called. The receiver resolved to a fully-known type (a primitive, a built-in collection, an `Optional`/`Result`, or a user record/class/enum in scope) and the method is in none of that type's method sets (intrinsic, canonical-trait, inherent/trait impl, or inherited trait default). When a near-miss name exists a \"did you mean `…`?\" suggestion is offered. Not raised for unresolved inference variables or §4.9 sketch-mode receivers.",
            spec_refs: &["§18.3"],
        },
        DiagnosticCodeInfo {
            code: "E4014",
            severity: Severity::Error,
            summary: "Bare module-qualified import.",
            description: "A `use` declaration named a module path with neither a brace-list nor a wildcard (a bare `use core.error`). Per §12.2 this is not a v1 import form; module-qualified access is deferred to v1.x. Import the names you need with the braced form (`use core.error.{ Error }`) or the discouraged wildcard (`use core.error.*`).",
            spec_refs: &["§12.2"],
        },
        DiagnosticCodeInfo {
            code: "E4015",
            severity: Severity::Error,
            summary: "Operand or bound instantiation is not `Equatable`.",
            description: "An `==`/`!=` operand (or a type instantiating an `Equatable` bound) does not conform to `Equatable` (DQ29). Records and enums conform STRUCTURALLY iff every field / variant payload type conforms (recursively); `List[T]`/`Set[T]`/`Optional[T]` iff `T`, `Map[K, V]` iff `K` and `V`, `Result[T, E]` iff `T` and `E`, tuples iff all components; generic user types decide per instantiation. A non-Equatable leaf (e.g. an `Fn` field) poisons the type, and the message names the offending field path and type. Classes are excluded from the structural default and need an explicit `impl Equatable`. An explicit impl always wins over the structural rules. Fix: implement `Equatable` for the type, or remove the comparison.",
            spec_refs: &["§18.5"],
        },
        // ── Ownership (5xxx) ───────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E5001",
            severity: Severity::Error,
            summary: "Use after move.",
            description: "A value was used after it had been moved into another binding or call.",
            spec_refs: &["§3"],
        },
        DiagnosticCodeInfo {
            code: "E5002",
            severity: Severity::Error,
            summary: "Mutable borrow of non-mut binding.",
            description: "The callee takes a `mut` borrow but the binding was not declared `mut`.",
            spec_refs: &["§3"],
        },
        DiagnosticCodeInfo {
            code: "E5003",
            severity: Severity::Error,
            summary: "Value moved inside loop.",
            description: "The loop body moves a value captured from outside, which would move it more than once.",
            spec_refs: &["§3"],
        },
        DiagnosticCodeInfo {
            code: "E5004",
            severity: Severity::Error,
            summary: "In-place `List` mutator requires a `mut` receiver.",
            description: "An in-place `List` mutator (`push`/`append`, DQ18; `pop`/`remove_at`/`insert`/`reverse` and indexed `set`, DQ30) was called on a receiver that is not a mutable lvalue. These methods mutate the list in place, so the receiver must be a `mut` binding. Fix: declare the list with `let mut`, take a `mut` parameter, or use a value-returning combinator (`+` / `concat`).",
            spec_refs: &["§3"],
        },
        // ── Effects (6xxx) ─────────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E6001",
            severity: Severity::Error,
            summary: "Undeclared effect.",
            description: "A function uses an algebraic effect that is not in its declared `with` clause.",
            spec_refs: &["§8"],
        },
        DiagnosticCodeInfo {
            code: "W6002",
            severity: Severity::Warning,
            summary: "Public function has undeclared effect (development mode).",
            description: "A public function uses an effect that is not in its declared `with` clause. Promotes to error in production strictness.",
            spec_refs: &["§8"],
        },
        DiagnosticCodeInfo {
            code: "E6003",
            severity: Severity::Error,
            summary: "Propagated effect not declared.",
            description: "A called function has an effect that the caller does not declare or handle.",
            spec_refs: &["§8"],
        },
        DiagnosticCodeInfo {
            code: "W6004",
            severity: Severity::Warning,
            summary: "Public function propagates undeclared effect (development mode).",
            description: "A public function calls a function whose effects escape its declared clause.",
            spec_refs: &["§8"],
        },
        DiagnosticCodeInfo {
            code: "E6005",
            severity: Severity::Error,
            summary: "Effect operation called without its effect declared or handled.",
            description: "An effect operation (e.g. `log(...)` of `effect Log`) was called, but the operation's effect is neither declared by the enclosing function (`with <Effect>`) nor handled in an enclosing scope (a `handling (<Effect> with <handler>) { ... }` block or a module-level `handle <Effect> with <handler>`). Fix by declaring the effect on the function or installing a handler. Emitted by the name-resolution pass: effect operations are only in scope in those contexts.",
            spec_refs: &["§8", "§10.3", "§10.4"],
        },
        DiagnosticCodeInfo {
            code: "E6006",
            severity: Severity::Error,
            summary: "Lambda-handler form is reserved until v1.x.",
            description: "The lambda-based handler surface `Effect.handler(op: (args) => ...)` is reserved for v1.x and is not a v1 form. v1 supports exactly one handler form: declare a record, `impl <Effect> for <Record>`, and install it with `handle <Effect> with <record>` (module level) or `handling (<Effect> with <record>) { ... }` (block level).",
            spec_refs: &["§10.4"],
        },
        // ── Capabilities (7xxx) ────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E7001",
            severity: Severity::Error,
            summary: "Missing capability.",
            description: "A function requires a capability that is not granted by the caller.",
            spec_refs: &["§9"],
        },
        DiagnosticCodeInfo {
            code: "W7002",
            severity: Severity::Warning,
            summary: "Public function requires uninstalled capability (development mode).",
            description: "A public function's required capability is not present in the caller's capability set.",
            spec_refs: &["§9"],
        },
        DiagnosticCodeInfo {
            code: "E7003",
            severity: Severity::Error,
            summary: "Propagated capability not declared.",
            description: "A callee requires a capability the caller has not declared.",
            spec_refs: &["§9"],
        },
        DiagnosticCodeInfo {
            code: "W7004",
            severity: Severity::Warning,
            summary: "Public function propagates uninstalled capability.",
            description: "A public function calls into code requiring capabilities it has not declared.",
            spec_refs: &["§9"],
        },
        // ── Context-annotation system (8xxx) ───────────────────────────
        // @context / @capability interpretation (bock-air/context.rs).
        DiagnosticCodeInfo {
            code: "E8001",
            severity: Severity::Error,
            summary: "Unknown capability in `@capability`.",
            description: "A `@capability` annotation named a capability the compiler does not recognize.",
            spec_refs: &["§9"],
        },
        DiagnosticCodeInfo {
            code: "E8002",
            severity: Severity::Error,
            summary: "Expected capability name in `@capability`.",
            description: "A `@capability` argument was not a capability name (e.g. `Capability.Network` or `Network`).",
            spec_refs: &["§9"],
        },
        DiagnosticCodeInfo {
            code: "E8003",
            severity: Severity::Error,
            summary: "Expected duration or byte size in `@performance`.",
            description: "A `@performance` budget argument was not a duration (e.g. `100.ms`) or a byte size (e.g. `50.mb`).",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "E8004",
            severity: Severity::Error,
            summary: "`@invariant` requires an expression argument.",
            description: "An `@invariant` annotation was written without the boolean expression it constrains.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "E8005",
            severity: Severity::Error,
            summary: "Invalid `@security` argument.",
            description: "A `@security` annotation expected a string `level` or a boolean `pii` flag, or was given neither.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "E8006",
            severity: Severity::Error,
            summary: "Expected string argument in `@domain`.",
            description: "A `@domain` annotation argument was not a string literal.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "E8010",
            severity: Severity::Error,
            summary: "`@invariant` expression must be boolean-typed.",
            description: "An `@invariant` expression must be a comparison, logical, or call expression that yields a boolean.",
            spec_refs: &["§17"],
        },
        // Context validation (bock-air/validate_context.rs).
        DiagnosticCodeInfo {
            code: "E8011",
            severity: Severity::Error,
            summary: "Child security level less restrictive than parent.",
            description: "An item's `@security` level is less restrictive than the level it inherits from an enclosing context, which would weaken the parent's guarantee.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "W8011",
            severity: Severity::Warning,
            summary: "Child declares `pii=false` but parent declares `pii=true`.",
            description: "PII status is inherited: a child cannot drop a parent's `pii=true` to `pii=false`. The narrower flag is ignored and a warning is issued.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "E8013",
            severity: Severity::Error,
            summary: "Public item is missing context annotations (production mode).",
            description: "In production (strict) mode every public item (`fn`/`class`/`trait`/`record`/`enum`) must carry a `@context` annotation. The standard-mode form of this rule is the warning `W8013`.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "W8013",
            severity: Severity::Warning,
            summary: "Public item is missing context annotations (standard mode).",
            description: "In standard mode a public item (`fn`/`class`/`trait`/`record`/`enum`) without a `@context` annotation is flagged as a recommendation, once per item. Promotes to the error `E8013` in production (strict) mode; suppressed entirely in lax mode and for embedded-stdlib modules.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "W8015",
            severity: Severity::Warning,
            summary: "Unknown security level in `@security`.",
            description: "A `@security(level: …)` string was not one of the known security levels; the annotation is kept but flagged.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "E8016",
            severity: Severity::Error,
            summary: "`@performance` budget must be positive.",
            description: "A `@performance` budget (`max_latency` or `max_memory`) was zero or negative; budgets must be positive values.",
            spec_refs: &["§17"],
        },
        // Capability verification, composition, and PII flow
        // (bock-air/verify_capabilities.rs, compose_context.rs).
        DiagnosticCodeInfo {
            code: "E8020",
            severity: Severity::Error,
            summary: "Effect operation has no handler in scope.",
            description: "A called effect operation has no handler installed in any enclosing scope and the effect is not declared, so it can never be discharged.",
            spec_refs: &["§8", "§10"],
        },
        DiagnosticCodeInfo {
            code: "W8020",
            severity: Severity::Warning,
            summary: "Effect declared-but-unused.",
            description: "An effect declared in a function's `with` clause is never used in its body (capability verification). Drop the unused effect from the `with` clause. (A PII-tainted signature without a security context is reported separately as `W8023`.)",
            spec_refs: &["§8"],
        },
        DiagnosticCodeInfo {
            code: "E8021",
            severity: Severity::Error,
            summary: "Callee requires an undeclared capability.",
            description: "A called function requires a capability that is not declared in the current scope, so the requirement cannot be satisfied.",
            spec_refs: &["§9"],
        },
        DiagnosticCodeInfo {
            code: "W8021",
            severity: Severity::Warning,
            summary: "Importing a PII-returning function into a module without a PII security context.",
            description: "A `use` imports a function that returns PII-tainted types into a module whose `@security` context does not acknowledge PII.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "W8022",
            severity: Severity::Warning,
            summary: "PII-tainted type passed to a logging/output function.",
            description: "A PII-tainted type flows into a logging or output sink, which is a potential data leak. One warning is emitted per call site.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "W8023",
            severity: Severity::Warning,
            summary: "PII-tainted signature without a security context.",
            description: "A function has PII-tainted types in its signature but its module lacks a `@security` context acknowledging PII (e.g. `@security(level: \"confidential\")` or `@security(pii: true)`). Emitted by context composition.",
            spec_refs: &["§17"],
        },
        DiagnosticCodeInfo {
            code: "E8023",
            severity: Severity::Error,
            summary: "Public declaration is missing `@context` (production mode).",
            description: "In production mode a public function, class, trait, or type must carry a `@context` annotation; capability verification rejects the bare declaration.",
            spec_refs: &["§17"],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_non_empty() {
        assert!(!diagnostic_catalog().is_empty());
    }

    #[test]
    fn codes_have_prefix() {
        for info in diagnostic_catalog() {
            let first = info.code.chars().next().unwrap();
            assert!(
                matches!(first, 'E' | 'W'),
                "code {:?} must start with E or W",
                info.code
            );
        }
    }

    // ── Emission ↔ catalog correspondence (Q-error-catalog-completeness) ───────

    /// Parse a catalog `code` string into `(prefix, number)`.
    ///
    /// Every catalog code must be a plain `E`/`W` followed by digits — no
    /// disambiguation suffixes like `"E1005 (module)"`, which neither parse
    /// nor round-trip through [`crate::DiagnosticCode`]'s `Display`.
    fn parse_code(code: &str) -> Option<(char, u16)> {
        let mut chars = code.chars();
        let prefix = chars.next()?;
        if !matches!(prefix, 'E' | 'W') {
            return None;
        }
        let digits: String = chars.collect();
        let number: u16 = digits.parse().ok()?;
        Some((prefix, number))
    }

    #[test]
    fn every_catalog_code_parses_and_round_trips() {
        for info in diagnostic_catalog() {
            let (prefix, number) = parse_code(info.code).unwrap_or_else(|| {
                panic!(
                    "catalog code {:?} is not a parseable `E`/`W` + digits code \
                     (no suffixes like `(module)` — split or fold collisions instead)",
                    info.code
                )
            });
            // Severity prefix must agree with the declared severity.
            let expected_prefix = match info.severity {
                Severity::Error => 'E',
                Severity::Warning => 'W',
                // Info/Hint diagnostics are not catalogued today; guard anyway.
                Severity::Info | Severity::Hint => prefix,
            };
            assert_eq!(
                prefix, expected_prefix,
                "code {} prefix disagrees with its severity {:?}",
                info.code, info.severity
            );
            // Display must reproduce the catalog string exactly.
            let rendered = crate::DiagnosticCode { prefix, number }.to_string();
            assert_eq!(
                rendered, info.code,
                "catalog code {:?} does not round-trip through DiagnosticCode \
                 Display (got {rendered:?})",
                info.code
            );
        }
    }

    #[test]
    fn catalog_has_no_duplicate_codes() {
        let mut seen = std::collections::BTreeSet::new();
        for info in diagnostic_catalog() {
            assert!(
                seen.insert(info.code),
                "duplicate catalog entry for code {:?}",
                info.code
            );
        }
    }

    /// Scan a Rust source string for `DiagnosticCode { prefix: 'X', number: N }`
    /// constructions, returning the set of rendered code strings (e.g. `E8003`).
    ///
    /// Only "real" emitted codes — `number >= 1000` — are collected; the small
    /// numbers (`1`, `42`, `99`, `204`, …) are exclusively diagnostic
    /// *test fixtures* that construct a `DiagnosticCode` directly and are not
    /// part of the emitted vocabulary.
    fn scan_emitted_codes(src: &str) -> std::collections::BTreeSet<String> {
        // Strip whitespace between tokens so the pattern can be matched without
        // caring about line breaks / indentation in the multi-line struct
        // literal. We only need a flattened, whitespace-collapsed view.
        let flat: String = src.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut out = std::collections::BTreeSet::new();
        let needle = "DiagnosticCode { prefix: '";
        let mut rest = flat.as_str();
        while let Some(pos) = rest.find(needle) {
            rest = &rest[pos + needle.len()..];
            // Next char is the prefix; then `' , number: <digits>`.
            let mut it = rest.chars();
            let Some(prefix) = it.next() else { break };
            if !matches!(prefix, 'E' | 'W') {
                continue;
            }
            // Locate `number:` after the prefix.
            let Some(num_pos) = rest.find("number:") else {
                break;
            };
            let after = rest[num_pos + "number:".len()..].trim_start();
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(number) = digits.parse::<u16>() {
                if number >= 1000 {
                    out.insert(format!("{prefix}{number:04}"));
                }
            }
        }
        out
    }

    #[test]
    fn scan_emitted_codes_basic() {
        let src = r#"
            self.diag.error(
                DiagnosticCode { prefix: 'E', number: 8003 },
                "x", span,
            );
            let c = DiagnosticCode {
                prefix: 'W',
                number: 1001,
            };
            // a test fixture, must be ignored:
            DiagnosticCode { prefix: 'E', number: 204 }
        "#;
        let codes = scan_emitted_codes(src);
        assert!(codes.contains("E8003"), "got {codes:?}");
        assert!(codes.contains("W1001"), "got {codes:?}");
        assert!(
            !codes.contains("E0204"),
            "test fixtures (<1000) must be skipped: {codes:?}"
        );
    }

    /// Every diagnostic code actually emitted by a compiler crate must have a
    /// catalog entry. This is the standing guard for
    /// Q-error-catalog-completeness: the catalog is the single registry, and
    /// an unregistered emitted code is a defect (an editor/agent cannot look it
    /// up). The scan walks every `compiler/crates/*/src/**/*.rs` for
    /// `DiagnosticCode { prefix, number >= 1000 }` constructions.
    ///
    /// If this fails, add the missing code to `diagnostic_catalog()` (do NOT
    /// renumber an existing emission — that is a design decision).
    #[test]
    fn every_emitted_code_is_registered() {
        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("bock-errors lives under compiler/crates/");

        let registered: std::collections::BTreeSet<String> = diagnostic_catalog()
            .iter()
            .map(|i| i.code.to_string())
            .collect();

        let mut emitted = std::collections::BTreeSet::new();
        let mut files_scanned = 0usize;
        let mut stack = vec![crates_dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Only descend into `src/` trees and crate roots; skip
                    // `target/` and hidden dirs.
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name == "target" || name.starts_with('.') {
                        continue;
                    }
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    if let Ok(src) = std::fs::read_to_string(&path) {
                        files_scanned += 1;
                        emitted.extend(scan_emitted_codes(&src));
                    }
                }
            }
        }

        assert!(
            files_scanned > 50,
            "expected to scan the whole crates tree, only saw {files_scanned} files \
             (CARGO_MANIFEST_DIR layout changed?)"
        );
        // Sanity floor: the scan must find the well-known codes.
        for expected in ["E1001", "E2000", "E4001", "E6005", "E8003", "W1001"] {
            assert!(
                emitted.contains(expected),
                "scanner failed to find {expected}; scan is broken"
            );
        }

        let unregistered: Vec<&String> = emitted.difference(&registered).collect();
        assert!(
            unregistered.is_empty(),
            "these emitted diagnostic codes are NOT in the catalog \
             (register them in diagnostic_catalog(); do not renumber): {unregistered:?}"
        );
    }
}
