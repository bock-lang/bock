//! Runtime error type for the Bock interpreter.

use thiserror::Error;

use crate::Value;

/// An error that can occur during Bock program evaluation.
///
/// Three variants (`Return`, `Break`, `Continue`) are control-flow signals
/// rather than true errors; they are propagated up through `eval_expr` and
/// caught at the appropriate statement handler (function body, loop body).
#[derive(Debug, Error, PartialEq)]
pub enum RuntimeError {
    // ── Type errors ───────────────────────────────────────────────────────
    #[error("type error: {0}")]
    TypeError(String),

    // ── Variable / name errors ────────────────────────────────────────────
    #[error("undefined variable: {name}")]
    UndefinedVariable { name: String },

    // ── Arithmetic errors ─────────────────────────────────────────────────
    #[error("division by zero")]
    DivisionByZero,

    #[error("integer overflow")]
    IntOverflow,

    // ── Collection errors ─────────────────────────────────────────────────
    #[error("index out of bounds: index {index} for length {len}")]
    IndexOutOfBounds { index: i64, len: usize },

    #[error("field not found: {field} on {type_name}")]
    FieldNotFound { field: String, type_name: String },

    // ── Call errors ───────────────────────────────────────────────────────
    #[error("not callable: {value}")]
    NotCallable { value: String },

    #[error("arity mismatch: expected {expected} args, got {got}")]
    ArityMismatch { expected: usize, got: usize },

    // ── Error propagation (`?`) ───────────────────────────────────────────
    /// Raised when `?` is applied to `None` or `Err(e)`. Caught by the
    /// enclosing function body to propagate the error outward.
    #[error("propagated error: {0}")]
    Propagated(Box<Value>),

    // ── Literal parse errors ──────────────────────────────────────────────
    #[error("integer literal parse failed: {0}")]
    IntParseFailed(String),

    #[error("float literal parse failed: {0}")]
    FloatParseFailed(String),

    // ── Control-flow signals ──────────────────────────────────────────────
    /// Carries the return value from a `return` expression.
    #[error("return")]
    Return(Box<Value>),

    /// Carries an optional `break` value from a `break` expression.
    #[error("break")]
    Break(Option<Box<Value>>),

    /// Signals `continue` inside a loop.
    #[error("continue")]
    Continue,

    // ── Misc ──────────────────────────────────────────────────────────────
    #[error("unreachable code reached")]
    Unreachable,

    #[error("match failed: no arm matched the scrutinee")]
    MatchFailed,

    #[error("no handler for effect `{effect}`: provide a handler via a `handling` block, module-level `handle`, or project config")]
    NoEffectHandler { effect: String },

    #[error("not implemented: {0}")]
    NotImplemented(String),

    // ── Test assertion errors ─────────────────────────────────────────────
    /// Raised when a test assertion (e.g., `expect(x).to_equal(y)`) fails.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
}
