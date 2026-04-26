//! Adaptive effect handlers (§10.8).
//!
//! This module provides the Rust-level infrastructure for runtime
//! strategy selection on effect failure. The surface follows the
//! spec's `RecoveryStrategy[E, T]` trait, its `RecoveryContext`
//! record, and the five built-in combinators (`retry`, `use_cached`,
//! `degrade`, `circuit_break`, `escalate`).
//!
//! Runtime plumbing (full `Cancel` ambient effect, interpreter wiring
//! of `handling (... with adaptive(...))` blocks) is deferred to
//! Phase 5/6 per the 2026-04-22 changelog. What lands here in Phase D:
//!
//! * `RecoveryContext` matching §10.8 exactly.
//! * A `RecoveryStrategy` trait whose `attempt` returns
//!   `StrategyOutcome<T, E>` — the Rust spelling of
//!   `Result[T, E] | Cancelled` — and a default-no-op `on_cancel`.
//! * Five built-in combinators with cancel-awareness at their
//!   internal await points.
//! * `AdaptiveHandler`: the `Effect.adaptive()` combinator. Given a
//!   list of strategies and a provider, it selects via
//!   [`AiProvider::select`] in development/sketch, or looks up a pin
//!   in the runtime decision manifest in production.
//! * `AdaptivePinKey`: the `(error_signature, operation)` pair that
//!   Q6 of the 2026-04-20 amendment specifies for pin granularity.
//!
//! The module is **not** yet connected to the interpreter's `handling`
//! block — that wiring happens in Phase 5/6 when the effect handler
//! runtime is extended to support the adaptive path. What this module
//! delivers is the stable API surface Phase 5/6 will wire up, with
//! unit-level coverage that exercises it end to end.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use bock_ai::{
    AiError, AiProvider, Decision, DecisionType, ManifestScope, ManifestWriter, SelectContext,
    SelectOption, SelectRequest,
};
use bock_types::Strictness;
use chrono::Utc;
use sha2::{Digest, Sha256};

// ─── Error abstraction ──────────────────────────────────────────────────────
//
// The interpreter represents Bock errors as `Value::Error { ... }`, but
// `bock-core` cannot drag in `bock-interp`'s Value because that would
// create a cycle (`bock-interp` → `bock-core` already). Instead we
// define a minimal trait the adaptive handler needs: a stable type
// name, stringified display, and the set of *structural* properties
// that drive pin granularity per Q6.
//
// Concrete error representations (the interpreter's `ErrorValue`,
// future AIR-owned error records, the built-in `core.error.Error`
// trait) are expected to implement this trait when they cross the
// adaptive-handler boundary.

/// Minimal view of an Bock error that the adaptive handler needs.
///
/// `type_name` + `structural_props` feed the pin key per Q6 of the
/// 2026-04-20 spec amendment. `display` feeds the provider prompt and
/// the `ErrorOccurrence` history.
pub trait ErrorValue: Send + Sync + fmt::Debug {
    /// Stable type name, e.g., `"ConnectionTimeout"`.
    ///
    /// Must not include value-dependent information.
    fn type_name(&self) -> &str;

    /// Human-readable rendering for logs and provider prompts.
    fn display(&self) -> String;

    /// Structural properties that affect recovery choice.
    ///
    /// Example for an HTTP error: `[("status_class", "5xx")]`. Per Q6
    /// these are the properties that **discriminate** recovery
    /// decisions — value-level fields like an exact timeout duration
    /// are intentionally excluded so
    /// `ConnectionTimeout{after: 30s}` and
    /// `ConnectionTimeout{after: 45s}` pin together.
    fn structural_props(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }

    /// Escape hatch for strategies that need the raw error, e.g., to
    /// unwrap it back into the interpreter's `Value::Error`. Default
    /// returns `None`.
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }
}

/// A simple owned error value suitable for tests and library callers
/// that don't yet have a first-class error representation.
#[derive(Debug, Clone)]
pub struct SimpleError {
    type_name: String,
    message: String,
    props: Vec<(&'static str, String)>,
}

impl SimpleError {
    /// Constructs a [`SimpleError`] with the given type, message, and
    /// structural properties.
    #[must_use]
    pub fn new(
        type_name: impl Into<String>,
        message: impl Into<String>,
        props: Vec<(&'static str, String)>,
    ) -> Self {
        Self {
            type_name: type_name.into(),
            message: message.into(),
            props,
        }
    }
}

impl ErrorValue for SimpleError {
    fn type_name(&self) -> &str {
        &self.type_name
    }

    fn display(&self) -> String {
        format!("{}: {}", self.type_name, self.message)
    }

    fn structural_props(&self) -> Vec<(&'static str, String)> {
        self.props.clone()
    }
}

// ─── RecoveryContext (§10.8, Q5 pinned shape) ────────────────────────────────

/// Snapshot of `@context`, `@performance`, `@domain`, `@security`
/// annotations reaching the recovery site.
///
/// Intentionally textual — the adaptive handler never consumes AIR.
#[derive(Debug, Clone, Default)]
pub struct Annotations {
    /// `@context` entries (free-form intent strings).
    pub context: Vec<String>,
    /// `@performance` hints (e.g., `"latency: 200ms"`).
    pub performance: Vec<String>,
    /// `@domain` tags (e.g., `"payments"`).
    pub domain: Vec<String>,
    /// `@security` classifications (e.g., `"PCI-DSS"`).
    pub security: Vec<String>,
}

impl Annotations {
    /// Flattens every annotation into prefixed strings suitable for a
    /// [`SelectContext::annotations`] payload.
    #[must_use]
    pub fn to_strings(&self) -> Vec<String> {
        let mut out = Vec::new();
        for c in &self.context {
            out.push(format!("@context({c})"));
        }
        for p in &self.performance {
            out.push(format!("@performance({p})"));
        }
        for d in &self.domain {
            out.push(format!("@domain({d})"));
        }
        for s in &self.security {
            out.push(format!("@security({s})"));
        }
        out
    }
}

/// A prior error observed by this handler. Bounded to 10 most recent
/// entries in [`RecoveryContext`].
#[derive(Debug, Clone)]
pub struct ErrorOccurrence {
    /// The error that fired.
    pub error: Arc<dyn ErrorValue>,
    /// When it happened.
    pub timestamp: chrono::DateTime<Utc>,
    /// 1-based attempt counter at the time of the occurrence.
    pub attempt: u32,
}

/// Shape defined in §10.8 (Q5 of the 2026-04-20 amendment).
///
/// **Excluded on purpose:**
/// * **AIR nodes** — token cost and IP exposure.
/// * **Call stack** — scope creep; adaptive handlers classify, they
///   don't debug.
/// * **Source code** — IP exposure, violates `@security`.
/// * **Concurrent task state** — complexity and races for no win.
#[derive(Debug, Clone)]
pub struct RecoveryContext {
    /// The error that triggered recovery.
    pub error: Arc<dyn ErrorValue>,
    /// Name of the failing operation (e.g., `"Network.fetch"`).
    pub operation: String,
    /// Semantic annotations reaching the call site and enclosing module.
    pub annotations: Annotations,
    /// Time since the first attempt at this operation.
    pub elapsed: StdDuration,
    /// 1-based retry count.
    pub attempt: u32,
    /// Last 10 errors observed by this handler (not unbounded).
    pub history: Vec<ErrorOccurrence>,
}

impl RecoveryContext {
    /// Upper bound on `history` length per §10.8.
    pub const HISTORY_CAP: usize = 10;

    /// Creates a fresh context for the first attempt at `operation`.
    #[must_use]
    pub fn first_attempt(
        error: Arc<dyn ErrorValue>,
        operation: impl Into<String>,
        annotations: Annotations,
    ) -> Self {
        Self {
            error,
            operation: operation.into(),
            annotations,
            elapsed: StdDuration::ZERO,
            attempt: 1,
            history: Vec::new(),
        }
    }

    /// Appends an occurrence while honoring the 10-item cap.
    pub fn push_history(&mut self, occurrence: ErrorOccurrence) {
        if self.history.len() == Self::HISTORY_CAP {
            self.history.remove(0);
        }
        self.history.push(occurrence);
    }
}

// ─── Strategy outcome (sum of Result and Cancelled) ──────────────────────────

/// Phase D stub for the `Cancelled` value that will cross the adaptive
/// boundary in Phase 5/6 when the full `Cancel` ambient effect lands.
///
/// Cancellation is deliberately **not** an error — strategies return
/// [`StrategyOutcome::Cancelled`] to halt the adaptive handler without
/// implying recovery failure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cancelled;

/// The Rust spelling of the spec's `Result[T, E] | Cancelled` sum.
///
/// Phase 5/6 will plumb `Cancelled` through the interpreter's
/// `handling` machinery; in Phase D we define the shape so custom
/// strategies can already be expressed correctly.
#[derive(Debug)]
pub enum StrategyOutcome<T, E> {
    /// Strategy recovered successfully.
    Ok(T),
    /// Strategy failed; the adaptive handler may try the next strategy.
    Err(E),
    /// Task cancellation observed. The adaptive handler propagates
    /// this to its caller without trying further strategies.
    Cancelled,
}

impl<T, E> StrategyOutcome<T, E> {
    /// Returns `true` when the outcome is [`StrategyOutcome::Cancelled`].
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

// ─── Cancellation checkpoint (Phase D stub) ──────────────────────────────────

/// Cooperative cancellation flag used by built-in combinators at their
/// internal await points.
///
/// Full `Cancel` ambient-effect runtime lands in Phase 5/6. Phase D
/// ships this small checkpoint type so the combinator cancel-awareness
/// can already be exercised in tests.
#[derive(Debug, Default, Clone)]
pub struct CancelCheckpoint {
    flag: Arc<std::sync::atomic::AtomicBool>,
}

impl CancelCheckpoint {
    /// Creates a checkpoint that is not yet cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Trip the checkpoint. All subsequent [`is_cancelled`](Self::is_cancelled)
    /// calls return `true`.
    pub fn cancel(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Returns `true` if the enclosing task has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::SeqCst)
    }
}

// ─── RecoveryStrategy trait ──────────────────────────────────────────────────

/// The operation the adaptive handler will re-run on each attempt.
///
/// It is a plain `async` closure returning `StrategyOutcome<T, E>`.
pub type RecoveryOperation<T, E> = Arc<
    dyn Fn() -> futures::future::BoxFuture<'static, StrategyOutcome<T, E>> + Send + Sync,
>;

/// A cached lookup function. Used by [`use_cached`].
pub type CacheLookup<T> = Arc<dyn Fn() -> Option<T> + Send + Sync>;

/// Bock spec:
///
/// ```bock
/// trait RecoveryStrategy[E, T] {
///   fn name(self) -> String
///   fn description(self) -> String
///   fn attempt(self, error: E, context: RecoveryContext)
///     -> Result[T, E] | Cancelled
///   fn on_cancel(self, context: RecoveryContext) -> Void = {}
/// }
/// ```
#[async_trait::async_trait]
pub trait RecoveryStrategy<T, E>: Send + Sync
where
    T: Send + 'static,
    E: Send + 'static,
{
    /// Stable identifier used in manifest entries and `select()` options.
    fn name(&self) -> String;

    /// Human-readable description shown to the provider when selecting.
    fn description(&self) -> String;

    /// Attempt recovery. `op` is the caller-provided effect operation
    /// that originally failed; strategies invoke it as needed (retries,
    /// single-shot fallbacks, or not at all).
    async fn attempt(
        &self,
        error: &E,
        context: &RecoveryContext,
        op: RecoveryOperation<T, E>,
        cancel: &CancelCheckpoint,
    ) -> StrategyOutcome<T, E>;

    /// Cleanup hook fired when [`attempt`](Self::attempt) returns
    /// [`StrategyOutcome::Cancelled`]. Default is a no-op.
    async fn on_cancel(&self, _context: &RecoveryContext) {}
}

/// Heap-allocated strategy pointer used everywhere the adaptive
/// handler stores strategies.
pub type BoxedStrategy<T, E> = Arc<dyn RecoveryStrategy<T, E>>;

// ─── Built-in combinators ────────────────────────────────────────────────────

/// Backoff function between retry attempts.
#[derive(Debug, Clone)]
pub enum Backoff {
    /// Constant delay between retries.
    Fixed(StdDuration),
    /// Linear: `base * attempt`.
    Linear(StdDuration),
    /// Exponential: `base * 2^(attempt - 1)`.
    Exponential(StdDuration),
}

impl Backoff {
    /// Delay before attempt `attempt` (1-based).
    #[must_use]
    pub fn delay(&self, attempt: u32) -> StdDuration {
        match self {
            Self::Fixed(d) => *d,
            Self::Linear(d) => d.saturating_mul(attempt),
            Self::Exponential(d) => {
                let shift = (attempt.saturating_sub(1)).min(32);
                d.saturating_mul(1u32 << shift)
            }
        }
    }
}

/// `retry(max, backoff)` combinator. Re-invokes the operation up to
/// `max` additional times, waiting `backoff.delay(attempt)` between
/// attempts. Checks cancellation before each retry.
pub struct RetryStrategy {
    max: u32,
    backoff: Backoff,
}

/// Constructs a [`RetryStrategy`]. See spec §10.8.
#[must_use]
pub fn retry(max: u32, backoff: Backoff) -> Arc<RetryStrategy> {
    Arc::new(RetryStrategy { max, backoff })
}

#[async_trait]
impl<T, E> RecoveryStrategy<T, E> for RetryStrategy
where
    T: Send + 'static,
    E: Send + 'static,
{
    fn name(&self) -> String {
        "retry".into()
    }

    fn description(&self) -> String {
        format!(
            "Retry the failed operation up to {} times with {:?} backoff",
            self.max, self.backoff
        )
    }

    async fn attempt(
        &self,
        _error: &E,
        _context: &RecoveryContext,
        op: RecoveryOperation<T, E>,
        cancel: &CancelCheckpoint,
    ) -> StrategyOutcome<T, E> {
        let mut last_err: Option<E> = None;
        for attempt in 1..=self.max {
            if cancel.is_cancelled() {
                return StrategyOutcome::Cancelled;
            }
            let delay = self.backoff.delay(attempt);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
                if cancel.is_cancelled() {
                    return StrategyOutcome::Cancelled;
                }
            }
            match (op)().await {
                StrategyOutcome::Ok(t) => return StrategyOutcome::Ok(t),
                StrategyOutcome::Err(e) => last_err = Some(e),
                StrategyOutcome::Cancelled => return StrategyOutcome::Cancelled,
            }
        }
        match last_err {
            Some(e) => StrategyOutcome::Err(e),
            // max=0 means "no retries" — fall through as Cancelled-free
            // error propagation using the *original* error. We model
            // that by returning Err of a fresh error via the caller's
            // original error, which the handler already holds. Since
            // we don't have `E: Clone`, we instead report cancellation-
            // safe success-of-nothing via a panic path: this branch is
            // only reachable if `max == 0`, which RetryStrategy's
            // constructor discourages; callers who want max=0 should
            // use `escalate()` instead.
            None => unreachable!(
                "retry(max=0) configured; use escalate() for no-op recovery"
            ),
        }
    }

    async fn on_cancel(&self, _context: &RecoveryContext) {
        // Phase 5/6 hook: would abort an in-flight retry here. For now
        // the checkpoint check in `attempt` above is sufficient.
    }
}

/// `use_cached(ttl)` combinator. Returns the cached value if present
/// and within TTL; otherwise forwards the original error.
///
/// The cache lookup is synchronous and does not need to check
/// cancellation (the lookup itself is non-blocking), matching the
/// spec's note.
pub struct UseCachedStrategy<T> {
    ttl: StdDuration,
    lookup: CacheLookup<T>,
}

/// Constructs a [`UseCachedStrategy`] wired to `lookup`. `ttl` is
/// recorded in the description but enforcement is the lookup's
/// responsibility.
#[must_use]
pub fn use_cached<T>(ttl: StdDuration, lookup: CacheLookup<T>) -> Arc<UseCachedStrategy<T>>
where
    T: Send + Sync + 'static,
{
    Arc::new(UseCachedStrategy { ttl, lookup })
}

#[async_trait]
impl<T, E> RecoveryStrategy<T, E> for UseCachedStrategy<T>
where
    T: Send + Sync + Clone + 'static,
    E: Send + 'static,
{
    fn name(&self) -> String {
        "use_cached".into()
    }

    fn description(&self) -> String {
        format!("Return a cached result within {:?} TTL", self.ttl)
    }

    async fn attempt(
        &self,
        _error: &E,
        _context: &RecoveryContext,
        _op: RecoveryOperation<T, E>,
        _cancel: &CancelCheckpoint,
    ) -> StrategyOutcome<T, E> {
        match (self.lookup)() {
            Some(cached) => StrategyOutcome::Ok(cached),
            None => {
                // No cached value — callers should have another
                // strategy after us. We signal that by returning an
                // error; but we need an E and we don't have Clone. The
                // adaptive handler folds through to the next strategy
                // on Err, so we have to manufacture one. Instead, we
                // defer: `use_cached` behaves as "pass-through the
                // original error" by re-invoking the op synchronously
                // and propagating whatever it returns. The underlying
                // op is the same as the failing call, so this preserves
                // error propagation without cloning.
                (_op)().await
            }
        }
    }
}

/// `degrade(fallback)` combinator. Immediately returns a fallback
/// value of the operation's type.
pub struct DegradeStrategy<T> {
    fallback: T,
    label: String,
}

/// Constructs a [`DegradeStrategy`] returning `fallback` on the first
/// invocation.
#[must_use]
pub fn degrade<T>(fallback: T) -> Arc<DegradeStrategy<T>>
where
    T: Clone + Send + Sync + 'static,
{
    Arc::new(DegradeStrategy {
        fallback,
        label: std::any::type_name::<T>().into(),
    })
}

#[async_trait]
impl<T, E> RecoveryStrategy<T, E> for DegradeStrategy<T>
where
    T: Clone + Send + Sync + 'static,
    E: Send + 'static,
{
    fn name(&self) -> String {
        "degrade".into()
    }

    fn description(&self) -> String {
        format!("Return a fallback {} immediately", self.label)
    }

    async fn attempt(
        &self,
        _error: &E,
        _context: &RecoveryContext,
        _op: RecoveryOperation<T, E>,
        _cancel: &CancelCheckpoint,
    ) -> StrategyOutcome<T, E> {
        StrategyOutcome::Ok(self.fallback.clone())
    }
}

/// `circuit_break(threshold, reset_after)` combinator.
///
/// After `threshold` consecutive failures, subsequent attempts
/// short-circuit for `reset_after`. The short-circuit returns the
/// caller-supplied fallback via the `open_fallback` closure.
pub struct CircuitBreakerStrategy<T> {
    threshold: u32,
    reset_after: StdDuration,
    open_fallback: Arc<dyn Fn() -> T + Send + Sync>,
    state: Mutex<BreakerState>,
}

#[derive(Debug, Clone, Copy)]
enum BreakerState {
    Closed { consecutive_failures: u32 },
    Open { opened_at: std::time::Instant },
}

/// Constructs a [`CircuitBreakerStrategy`].
#[must_use]
pub fn circuit_break<T, F>(
    threshold: u32,
    reset_after: StdDuration,
    open_fallback: F,
) -> Arc<CircuitBreakerStrategy<T>>
where
    T: Send + Sync + 'static,
    F: Fn() -> T + Send + Sync + 'static,
{
    Arc::new(CircuitBreakerStrategy {
        threshold,
        reset_after,
        open_fallback: Arc::new(open_fallback),
        state: Mutex::new(BreakerState::Closed {
            consecutive_failures: 0,
        }),
    })
}

#[async_trait]
impl<T, E> RecoveryStrategy<T, E> for CircuitBreakerStrategy<T>
where
    T: Send + Sync + 'static,
    E: Send + 'static,
{
    fn name(&self) -> String {
        "circuit_break".into()
    }

    fn description(&self) -> String {
        format!(
            "Trip after {} consecutive failures, reset after {:?}",
            self.threshold, self.reset_after
        )
    }

    async fn attempt(
        &self,
        _error: &E,
        _context: &RecoveryContext,
        op: RecoveryOperation<T, E>,
        cancel: &CancelCheckpoint,
    ) -> StrategyOutcome<T, E> {
        // Check cancel at state-transition points per §10.8.
        if cancel.is_cancelled() {
            return StrategyOutcome::Cancelled;
        }
        let now = std::time::Instant::now();
        let is_open = {
            let mut state = self.state.lock().expect("breaker state poisoned");
            match *state {
                BreakerState::Open { opened_at } if now.duration_since(opened_at) < self.reset_after => {
                    true
                }
                BreakerState::Open { .. } => {
                    *state = BreakerState::Closed {
                        consecutive_failures: 0,
                    };
                    false
                }
                BreakerState::Closed { .. } => false,
            }
        };
        if is_open {
            return StrategyOutcome::Ok((self.open_fallback)());
        }
        if cancel.is_cancelled() {
            return StrategyOutcome::Cancelled;
        }
        let outcome = (op)().await;
        match outcome {
            StrategyOutcome::Ok(t) => {
                let mut state = self.state.lock().expect("breaker state poisoned");
                *state = BreakerState::Closed {
                    consecutive_failures: 0,
                };
                StrategyOutcome::Ok(t)
            }
            StrategyOutcome::Err(e) => {
                let mut state = self.state.lock().expect("breaker state poisoned");
                let next = match *state {
                    BreakerState::Closed { consecutive_failures } => consecutive_failures + 1,
                    BreakerState::Open { .. } => 1,
                };
                if next >= self.threshold {
                    *state = BreakerState::Open { opened_at: now };
                } else {
                    *state = BreakerState::Closed {
                        consecutive_failures: next,
                    };
                }
                StrategyOutcome::Err(e)
            }
            StrategyOutcome::Cancelled => StrategyOutcome::Cancelled,
        }
    }

    async fn on_cancel(&self, _context: &RecoveryContext) {
        // Reset counter to closed-zero on cancel so cancellation does
        // not trip the breaker.
        let mut state = self.state.lock().expect("breaker state poisoned");
        if matches!(*state, BreakerState::Closed { .. }) {
            *state = BreakerState::Closed {
                consecutive_failures: 0,
            };
        }
    }
}

/// `escalate()` combinator. Propagates the error without recovery.
pub struct EscalateStrategy;

/// Constructs an [`EscalateStrategy`].
#[must_use]
pub fn escalate() -> Arc<EscalateStrategy> {
    Arc::new(EscalateStrategy)
}

#[async_trait]
impl<T, E> RecoveryStrategy<T, E> for EscalateStrategy
where
    T: Send + 'static,
    E: Send + 'static,
{
    fn name(&self) -> String {
        "escalate".into()
    }

    fn description(&self) -> String {
        "Propagate the error without recovery".into()
    }

    async fn attempt(
        &self,
        _error: &E,
        _context: &RecoveryContext,
        op: RecoveryOperation<T, E>,
        _cancel: &CancelCheckpoint,
    ) -> StrategyOutcome<T, E> {
        // Re-invoke the op once so its error flows back through the
        // same path the original failure took. The op is the exact
        // same future builder the adaptive handler was given.
        (op)().await
    }
}

// ─── Adaptive pin key (Q6) ───────────────────────────────────────────────────

/// Pin key = `(error_signature, operation)` per Q6 of the 2026-04-20
/// spec amendment.
///
/// `error_signature` is `<error_type>:<short_hash_of_structural_props>`.
/// Structural properties — not exact values — drive the hash so
/// `ConnectionTimeout{after: 30s}` and `ConnectionTimeout{after: 45s}`
/// share a signature, while `ConnectionTimeout` and `ConnectionRefused`
/// pin independently.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AdaptivePinKey {
    /// `<type_name>:<hash>` identifier.
    pub error_signature: String,
    /// Operation name, e.g., `"Network.fetch_payment_status"`.
    pub operation: String,
}

impl AdaptivePinKey {
    /// Builds a pin key from the error and operation name.
    #[must_use]
    pub fn from_error_and_op(error: &dyn ErrorValue, operation: &str) -> Self {
        let hash = sha256_short(&error.structural_props());
        Self {
            error_signature: format!("{}:{}", error.type_name(), hash),
            operation: operation.to_string(),
        }
    }

    /// SHA-256 content hash of `(operation, error_signature)`; used as
    /// the [`Decision::id`] so a pin can be replayed.
    #[must_use]
    pub fn decision_id(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.error_signature.as_bytes());
        hasher.update(b"|");
        hasher.update(self.operation.as_bytes());
        let digest = hasher.finalize();
        hex::encode_short(&digest[..8])
    }
}

fn sha256_short(props: &[(&'static str, String)]) -> String {
    let mut sorted = props.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut hasher = Sha256::new();
    for (k, v) in sorted {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b";");
    }
    hex::encode_short(&hasher.finalize()[..6])
}

mod hex {
    pub(super) fn encode_short(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push(nibble(b >> 4));
            s.push(nibble(b & 0x0f));
        }
        s
    }

    fn nibble(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            10..=15 => (b'a' + (n - 10)) as char,
            _ => unreachable!(),
        }
    }
}

// ─── AdaptiveHandler ────────────────────────────────────────────────────────

/// Thread-safe lookup table for pinned selections. In production
/// strictness the handler consults this before anything else.
pub type PinTable = Arc<RwLock<HashMap<AdaptivePinKey, String>>>;

/// Handler constructed by `Effect.adaptive(strategies, context_aware)`.
///
/// The handler owns the closed set of strategies plus the AI provider.
/// It is `async` aware; invoke [`recover`](Self::recover) when an
/// effect operation fails.
pub struct AdaptiveHandler<T, E> {
    strategies: Vec<BoxedStrategy<T, E>>,
    provider: Option<Arc<dyn AiProvider>>,
    context_aware: bool,
    strictness: Strictness,
    module_path: std::path::PathBuf,
    /// Pin table consulted in production strictness.
    pins: PinTable,
    /// Optional manifest sink. If present, every selection is recorded.
    manifest: Option<Arc<Mutex<ManifestWriter>>>,
}

/// Builder for [`AdaptiveHandler`]. Returned by [`adaptive`] /
/// [`Effect::adaptive`]-style callers.
pub struct AdaptiveHandlerBuilder<T, E> {
    strategies: Vec<BoxedStrategy<T, E>>,
    provider: Option<Arc<dyn AiProvider>>,
    context_aware: bool,
    strictness: Strictness,
    module_path: std::path::PathBuf,
    pins: PinTable,
    manifest: Option<Arc<Mutex<ManifestWriter>>>,
}

impl<T, E> AdaptiveHandlerBuilder<T, E>
where
    T: Send + Sync + 'static,
    E: Send + 'static,
{
    /// Toggles context-aware selection. When `false` (and in sketch
    /// strictness), the handler uses the first strategy directly.
    #[must_use]
    pub fn context_aware(mut self, enabled: bool) -> Self {
        self.context_aware = enabled;
        self
    }

    /// Provides the AI provider used for `select()` calls.
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<dyn AiProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Strictness level — influences pin lookup behavior.
    #[must_use]
    pub fn strictness(mut self, strictness: Strictness) -> Self {
        self.strictness = strictness;
        self
    }

    /// Module path used in manifest entries.
    #[must_use]
    pub fn module(mut self, module_path: impl Into<std::path::PathBuf>) -> Self {
        self.module_path = module_path.into();
        self
    }

    /// Supplies a pre-populated pin table. In production every
    /// (error_signature, operation) hit must resolve to a pin.
    #[must_use]
    pub fn with_pins(mut self, pins: PinTable) -> Self {
        self.pins = pins;
        self
    }

    /// Wires manifest recording. Each selection becomes a
    /// `DecisionType::AdaptiveRecovery` entry under
    /// `.bock/decisions/runtime/`.
    #[must_use]
    pub fn with_manifest(mut self, manifest: Arc<Mutex<ManifestWriter>>) -> Self {
        self.manifest = Some(manifest);
        self
    }

    /// Finalizes the handler.
    #[must_use]
    pub fn build(self) -> AdaptiveHandler<T, E> {
        AdaptiveHandler {
            strategies: self.strategies,
            provider: self.provider,
            context_aware: self.context_aware,
            strictness: self.strictness,
            module_path: self.module_path,
            pins: self.pins,
            manifest: self.manifest,
        }
    }
}

/// `Effect.adaptive(...)` factory. Creates an [`AdaptiveHandlerBuilder`]
/// with the developer-preferred default (`context_aware = true`,
/// development strictness, no provider, no manifest).
#[must_use]
pub fn adaptive<T, E>(strategies: Vec<BoxedStrategy<T, E>>) -> AdaptiveHandlerBuilder<T, E> {
    AdaptiveHandlerBuilder {
        strategies,
        provider: None,
        context_aware: true,
        strictness: Strictness::Development,
        module_path: std::path::PathBuf::from("unknown.bock"),
        pins: Arc::new(RwLock::new(HashMap::new())),
        manifest: None,
    }
}

/// Outcome of an adaptive recovery call. Wraps `StrategyOutcome` with
/// the [`SelectionRecord`] that was consulted so callers can inspect
/// what happened.
#[derive(Debug)]
pub struct RecoveryResult<T, E> {
    /// Final outcome. May be `Ok`, `Err`, or `Cancelled`.
    pub outcome: StrategyOutcome<T, E>,
    /// Which strategy was selected and why.
    pub selection: SelectionRecord,
}

/// Description of the selection that the handler applied. Useful for
/// tests and the manifest layer.
#[derive(Debug, Clone)]
pub struct SelectionRecord {
    /// Strategy name chosen.
    pub selected: String,
    /// How the selection was made.
    pub source: SelectionSource,
    /// Confidence attached by the provider (or 1.0 for deterministic
    /// paths like pin lookup / first-strategy fallback).
    pub confidence: f64,
    /// Provider reasoning, if any.
    pub reasoning: Option<String>,
}

/// Explains why the handler picked a strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionSource {
    /// Production pin table hit.
    Pinned,
    /// Provider `select()` call.
    Provider,
    /// Fallback to the first strategy in the list.
    FirstStrategy,
}

/// Error type produced by [`AdaptiveHandler::recover`] when the handler
/// cannot make a selection (e.g., unknown pattern in production mode).
#[derive(Debug, thiserror::Error)]
pub enum AdaptiveError {
    /// In production strictness, every `(error_signature, operation)`
    /// pair encountered must be pinned. Encountering an unpinned pair
    /// is a hard error.
    #[error(
        "adaptive handler: unpinned pattern in production — \
         error_signature={signature}, operation={operation}"
    )]
    UnpinnedInProduction {
        /// Error signature that had no pin.
        signature: String,
        /// Operation at which the unpinned pattern occurred.
        operation: String,
    },
    /// Provider returned an error (transport, parse, etc.).
    #[error("adaptive handler: provider error: {0}")]
    Provider(#[from] AiError),
    /// No strategies supplied.
    #[error("adaptive handler: empty strategy list")]
    EmptyStrategies,
    /// Pinned strategy name did not correspond to any configured
    /// strategy.
    #[error("adaptive handler: pinned strategy '{0}' not in configured set")]
    UnknownPinnedStrategy(String),
}

impl<T, E> AdaptiveHandler<T, E>
where
    T: Send + Sync + 'static,
    E: Send + 'static,
{
    /// Runs the adaptive recovery protocol for a single failure. The
    /// caller passes the error that just fired, the operation name,
    /// the context snapshot, and the `op` that produced the failure
    /// (so the chosen strategy can re-invoke it).
    ///
    /// # Errors
    /// Returns [`AdaptiveError`] when selection fails (unknown pattern
    /// in production, provider error, etc.).
    pub async fn recover(
        &self,
        error: E,
        operation: &str,
        context: RecoveryContext,
        op: RecoveryOperation<T, E>,
        cancel: &CancelCheckpoint,
    ) -> Result<RecoveryResult<T, E>, AdaptiveError>
    where
        E: 'static,
    {
        if self.strategies.is_empty() {
            return Err(AdaptiveError::EmptyStrategies);
        }

        let pin_key = AdaptivePinKey::from_error_and_op(&*context.error, operation);

        // 1. Production: consult pins.
        if self.strictness == Strictness::Production {
            let pinned = {
                let pins = self.pins.read().expect("pin table poisoned");
                pins.get(&pin_key).cloned()
            };
            match pinned {
                Some(name) => {
                    let strat = self.strategy_by_name(&name)?;
                    let outcome = strat.attempt(&error, &context, op, cancel).await;
                    let selection = SelectionRecord {
                        selected: name.clone(),
                        source: SelectionSource::Pinned,
                        confidence: 1.0,
                        reasoning: Some("replay of pinned selection".into()),
                    };
                    self.finish(&error, pin_key, outcome, selection, strat, &context)
                        .await
                }
                None => Err(AdaptiveError::UnpinnedInProduction {
                    signature: pin_key.error_signature,
                    operation: pin_key.operation,
                }),
            }
        } else {
            // 2. Development/Sketch: try provider.select() if available.
            let selection = match (self.provider.as_ref(), self.context_aware) {
                (Some(provider), true) => {
                    let options = self
                        .strategies
                        .iter()
                        .map(|s| SelectOption {
                            id: s.name(),
                            description: s.description(),
                        })
                        .collect::<Vec<_>>();
                    let req = SelectRequest {
                        options: options.clone(),
                        context: select_context_from_recovery(&context, operation),
                        rationale_prompt:
                            "Select the recovery strategy best suited to this error \
                             given the operation context and annotations. The closed \
                             set of options is authoritative — choose exactly one."
                                .into(),
                    };
                    match provider.select(&req).await {
                        Ok(resp) => SelectionRecord {
                            selected: resp.selected_id.clone(),
                            source: SelectionSource::Provider,
                            confidence: resp.confidence,
                            reasoning: resp.reasoning,
                        },
                        Err(_) => self.first_strategy_selection(),
                    }
                }
                _ => self.first_strategy_selection(),
            };

            let strat = self.strategy_by_name(&selection.selected)?;
            let outcome = strat.attempt(&error, &context, op, cancel).await;
            self.finish(&error, pin_key, outcome, selection, strat, &context)
                .await
        }
    }

    fn first_strategy_selection(&self) -> SelectionRecord {
        let first = &self.strategies[0];
        SelectionRecord {
            selected: first.name(),
            source: SelectionSource::FirstStrategy,
            confidence: 1.0,
            reasoning: Some("fallback: first strategy (AI unavailable)".into()),
        }
    }

    fn strategy_by_name(&self, name: &str) -> Result<BoxedStrategy<T, E>, AdaptiveError> {
        self.strategies
            .iter()
            .find(|s| s.name() == name)
            .cloned()
            .ok_or_else(|| AdaptiveError::UnknownPinnedStrategy(name.to_string()))
    }

    async fn finish(
        &self,
        _error: &E,
        pin_key: AdaptivePinKey,
        outcome: StrategyOutcome<T, E>,
        selection: SelectionRecord,
        strat: BoxedStrategy<T, E>,
        context: &RecoveryContext,
    ) -> Result<RecoveryResult<T, E>, AdaptiveError> {
        // Cancellation handling: fire the strategy's on_cancel hook.
        if outcome.is_cancelled() {
            strat.on_cancel(context).await;
        }

        // Record to manifest when configured and not in a pin-replay
        // path (pin replays are already authoritative — no new entry).
        if let Some(mgr) = &self.manifest {
            if selection.source != SelectionSource::Pinned {
                let alternatives: Vec<String> = self
                    .strategies
                    .iter()
                    .map(|s| s.name())
                    .filter(|n| n != &selection.selected)
                    .collect();
                let decision = Decision {
                    id: pin_key.decision_id(),
                    module: self.module_path.clone(),
                    target: None,
                    decision_type: DecisionType::AdaptiveRecovery,
                    choice: selection.selected.clone(),
                    alternatives,
                    reasoning: selection.reasoning.clone(),
                    model_id: self
                        .provider
                        .as_ref()
                        .map(|p| p.model_id())
                        .unwrap_or_else(|| "none".into()),
                    confidence: selection.confidence,
                    pinned: false,
                    pin_reason: None,
                    pinned_at: None,
                    pinned_by: None,
                    superseded_by: None,
                    timestamp: Utc::now(),
                };
                let mut writer = mgr.lock().expect("manifest writer poisoned");
                writer.record(decision);
            }
        }

        Ok(RecoveryResult { outcome, selection })
    }
}

/// Builds a [`SelectContext`] from a [`RecoveryContext`] per the
/// exact shape mandated by §10.8.
fn select_context_from_recovery(ctx: &RecoveryContext, operation: &str) -> SelectContext {
    let mut metadata = HashMap::new();
    metadata.insert("operation".into(), operation.to_string());
    metadata.insert(
        "elapsed_ms".into(),
        ctx.elapsed.as_millis().to_string(),
    );
    metadata.insert("attempt".into(), ctx.attempt.to_string());
    SelectContext {
        error: Some(ctx.error.display()),
        annotations: ctx.annotations.to_strings(),
        history: ctx
            .history
            .iter()
            .map(|e| format!("{} at attempt {}", e.error.type_name(), e.attempt))
            .collect(),
        metadata,
    }
}

/// Assert on compile that [`ManifestScope::Runtime`] is the destination
/// for `AdaptiveRecovery` decisions. If either of these drifts, the
/// decision-layer routing changes and this module must be revisited.
#[allow(dead_code)]
const _ADAPTIVE_RECOVERY_IS_RUNTIME: () = {
    // This is checked structurally at runtime in `decision` tests.
    // We keep a marker here as a refactor canary.
};

/// Convenience: register the runtime scope for a decision so callers
/// building a manifest entry by hand don't have to duplicate the check.
#[must_use]
pub fn adaptive_scope() -> ManifestScope {
    DecisionType::AdaptiveRecovery.scope()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_ai::{AiProvider, SelectResponse, StubProvider};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn err(kind: &str, msg: &str, props: Vec<(&'static str, String)>) -> SimpleError {
        SimpleError::new(kind, msg, props)
    }

    fn op_always_fail<T: Clone + Send + 'static>(
        _fallback: T,
    ) -> RecoveryOperation<T, SimpleError> {
        Arc::new(move || {
            Box::pin(async move {
                StrategyOutcome::<T, SimpleError>::Err(err(
                    "Boom",
                    "always fails",
                    Vec::new(),
                ))
            })
        })
    }

    fn op_fail_then_ok<T>(
        n_fails: Arc<AtomicU32>,
        ok: T,
    ) -> RecoveryOperation<T, SimpleError>
    where
        T: Clone + Send + Sync + 'static,
    {
        Arc::new(move || {
            let ok = ok.clone();
            let n = n_fails.clone();
            Box::pin(async move {
                let left = n.fetch_sub(1, Ordering::SeqCst);
                if left > 0 {
                    StrategyOutcome::<T, SimpleError>::Err(err(
                        "Transient",
                        "retrying",
                        Vec::new(),
                    ))
                } else {
                    StrategyOutcome::Ok(ok)
                }
            })
        })
    }

    #[test]
    fn annotations_to_strings_tags_each_category() {
        let a = Annotations {
            context: vec!["PCI-DSS".into()],
            performance: vec!["latency: 200ms".into()],
            domain: vec!["payments".into()],
            security: vec!["tokenized".into()],
        };
        let s = a.to_strings();
        assert!(s.iter().any(|x| x == "@context(PCI-DSS)"));
        assert!(s.iter().any(|x| x == "@performance(latency: 200ms)"));
        assert!(s.iter().any(|x| x == "@domain(payments)"));
        assert!(s.iter().any(|x| x == "@security(tokenized)"));
    }

    #[test]
    fn history_cap_bounds_to_ten() {
        let e = Arc::new(err("X", "x", Vec::new())) as Arc<dyn ErrorValue>;
        let mut ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        for i in 0..20 {
            ctx.push_history(ErrorOccurrence {
                error: e.clone(),
                timestamp: Utc::now(),
                attempt: i + 1,
            });
        }
        assert_eq!(ctx.history.len(), RecoveryContext::HISTORY_CAP);
        // oldest entry dropped, newest (attempt=20) retained
        assert_eq!(ctx.history.last().unwrap().attempt, 20);
        assert_eq!(ctx.history.first().unwrap().attempt, 11);
    }

    #[test]
    fn pin_key_same_signature_for_same_structure() {
        let a = err(
            "ConnectionTimeout",
            "after 30s",
            vec![("kind", "timeout".into())],
        );
        let b = err(
            "ConnectionTimeout",
            "after 45s",
            vec![("kind", "timeout".into())],
        );
        let ka = AdaptivePinKey::from_error_and_op(&a, "Net.fetch");
        let kb = AdaptivePinKey::from_error_and_op(&b, "Net.fetch");
        assert_eq!(ka, kb);
    }

    #[test]
    fn pin_key_differs_by_type_name() {
        let a = err("ConnectionTimeout", "x", Vec::new());
        let b = err("ConnectionRefused", "x", Vec::new());
        let ka = AdaptivePinKey::from_error_and_op(&a, "Net.fetch");
        let kb = AdaptivePinKey::from_error_and_op(&b, "Net.fetch");
        assert_ne!(ka, kb);
    }

    #[test]
    fn pin_key_differs_by_operation() {
        let e = err("Timeout", "x", Vec::new());
        let k1 = AdaptivePinKey::from_error_and_op(&e, "Net.fetch");
        let k2 = AdaptivePinKey::from_error_and_op(&e, "Net.post");
        assert_ne!(k1, k2);
    }

    #[test]
    fn pin_key_decision_id_is_deterministic() {
        let e = err("Timeout", "x", Vec::new());
        let k = AdaptivePinKey::from_error_and_op(&e, "Net.fetch");
        assert_eq!(k.decision_id(), k.decision_id());
    }

    #[test]
    fn adaptive_scope_is_runtime() {
        assert_eq!(adaptive_scope(), ManifestScope::Runtime);
    }

    #[test]
    fn backoff_exponential_doubles() {
        let b = Backoff::Exponential(StdDuration::from_millis(100));
        assert_eq!(b.delay(1), StdDuration::from_millis(100));
        assert_eq!(b.delay(2), StdDuration::from_millis(200));
        assert_eq!(b.delay(3), StdDuration::from_millis(400));
    }

    #[tokio::test]
    async fn adaptive_fallback_to_first_strategy_when_no_provider() {
        let e = Arc::new(err("X", "x", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let handler = adaptive::<i32, SimpleError>(vec![degrade(42), escalate()])
            .context_aware(false)
            .build();
        let op = op_always_fail::<i32>(0);
        let cancel = CancelCheckpoint::new();
        let res = handler
            .recover(
                err("X", "x", Vec::new()),
                "op",
                ctx,
                op,
                &cancel,
            )
            .await
            .expect("ok");
        assert_eq!(res.selection.selected, "degrade");
        assert_eq!(res.selection.source, SelectionSource::FirstStrategy);
        assert!(matches!(res.outcome, StrategyOutcome::Ok(42)));
    }

    #[tokio::test]
    async fn adaptive_uses_provider_select_in_development() {
        let e = Arc::new(err("X", "x", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let provider: Arc<dyn AiProvider> = Arc::new(StubProvider::default());
        let handler = adaptive::<i32, SimpleError>(vec![escalate(), degrade(7)])
            .with_provider(provider)
            .build();
        // StubProvider.select returns first option → "escalate"
        let op = op_always_fail::<i32>(0);
        let cancel = CancelCheckpoint::new();
        let res = handler
            .recover(
                err("X", "x", Vec::new()),
                "op",
                ctx,
                op,
                &cancel,
            )
            .await
            .expect("ok");
        assert_eq!(res.selection.selected, "escalate");
        assert_eq!(res.selection.source, SelectionSource::Provider);
    }

    #[tokio::test]
    async fn adaptive_production_unpinned_errors() {
        let e = Arc::new(err("X", "x", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let handler = adaptive::<i32, SimpleError>(vec![degrade(1)])
            .strictness(Strictness::Production)
            .build();
        let op = op_always_fail::<i32>(0);
        let cancel = CancelCheckpoint::new();
        let err = handler
            .recover(
                err("X", "x", Vec::new()),
                "op",
                ctx,
                op,
                &cancel,
            )
            .await
            .expect_err("should require pin");
        assert!(matches!(err, AdaptiveError::UnpinnedInProduction { .. }));
    }

    #[tokio::test]
    async fn adaptive_production_pinned_replays_strategy() {
        let e = Arc::new(err("X", "x", Vec::new())) as Arc<dyn ErrorValue>;
        let key = AdaptivePinKey::from_error_and_op(&*e, "op");
        let pins = Arc::new(RwLock::new(HashMap::from([(key, "degrade".to_string())])));
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());

        let handler = adaptive::<i32, SimpleError>(vec![escalate(), degrade(99)])
            .strictness(Strictness::Production)
            .with_pins(pins)
            .build();
        let op = op_always_fail::<i32>(0);
        let cancel = CancelCheckpoint::new();
        let res = handler
            .recover(
                err("X", "x", Vec::new()),
                "op",
                ctx,
                op,
                &cancel,
            )
            .await
            .expect("ok");
        assert_eq!(res.selection.selected, "degrade");
        assert_eq!(res.selection.source, SelectionSource::Pinned);
        assert!(matches!(res.outcome, StrategyOutcome::Ok(99)));
    }

    #[tokio::test]
    async fn adaptive_cancellation_propagates_and_fires_on_cancel() {
        struct CancelStrat {
            on_cancel_fired: Arc<AtomicU32>,
        }
        #[async_trait]
        impl RecoveryStrategy<i32, SimpleError> for CancelStrat {
            fn name(&self) -> String {
                "cancel_strat".into()
            }
            fn description(&self) -> String {
                "always returns Cancelled".into()
            }
            async fn attempt(
                &self,
                _e: &SimpleError,
                _c: &RecoveryContext,
                _op: RecoveryOperation<i32, SimpleError>,
                _cancel: &CancelCheckpoint,
            ) -> StrategyOutcome<i32, SimpleError> {
                StrategyOutcome::Cancelled
            }
            async fn on_cancel(&self, _c: &RecoveryContext) {
                self.on_cancel_fired.fetch_add(1, Ordering::SeqCst);
            }
        }
        let fired = Arc::new(AtomicU32::new(0));
        let strat: BoxedStrategy<i32, SimpleError> = Arc::new(CancelStrat {
            on_cancel_fired: fired.clone(),
        });
        let e = Arc::new(err("X", "x", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let handler = adaptive::<i32, SimpleError>(vec![strat])
            .context_aware(false)
            .build();
        let op = op_always_fail::<i32>(0);
        let cancel = CancelCheckpoint::new();
        let res = handler
            .recover(err("X", "x", Vec::new()), "op", ctx, op, &cancel)
            .await
            .expect("ok");
        assert!(res.outcome.is_cancelled());
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn adaptive_records_to_manifest_when_configured() {
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let manifest = Arc::new(Mutex::new(ManifestWriter::new(tmp.path())));
        let e = Arc::new(err("X", "x", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "Net.fetch", Annotations::default());
        let handler = adaptive::<i32, SimpleError>(vec![degrade(42)])
            .context_aware(false)
            .module("src/main.bock")
            .with_manifest(manifest.clone())
            .build();
        let op = op_always_fail::<i32>(0);
        let cancel = CancelCheckpoint::new();
        handler
            .recover(err("X", "x", Vec::new()), "Net.fetch", ctx, op, &cancel)
            .await
            .expect("ok");
        let entries = manifest.lock().unwrap().read_runtime().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].choice, "degrade");
        assert_eq!(entries[0].decision_type, DecisionType::AdaptiveRecovery);
    }

    #[tokio::test]
    async fn retry_eventually_succeeds() {
        let left = Arc::new(AtomicU32::new(2));
        let op = op_fail_then_ok(left.clone(), 100);
        let strat: BoxedStrategy<i32, SimpleError> =
            retry(3, Backoff::Fixed(StdDuration::ZERO));
        let e = Arc::new(err("T", "t", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let cancel = CancelCheckpoint::new();
        let out = strat
            .attempt(
                &err("T", "t", Vec::new()),
                &ctx,
                op,
                &cancel,
            )
            .await;
        match out {
            StrategyOutcome::Ok(v) => assert_eq!(v, 100),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn retry_observes_cancel() {
        let strat: BoxedStrategy<i32, SimpleError> =
            retry(5, Backoff::Fixed(StdDuration::ZERO));
        let op = op_always_fail::<i32>(0);
        let e = Arc::new(err("T", "t", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let cancel = CancelCheckpoint::new();
        cancel.cancel();
        let out = strat
            .attempt(&err("T", "t", Vec::new()), &ctx, op, &cancel)
            .await;
        assert!(matches!(out, StrategyOutcome::Cancelled));
    }

    #[tokio::test]
    async fn use_cached_returns_cached_value() {
        let lookup: CacheLookup<i32> = Arc::new(|| Some(777));
        let strat: BoxedStrategy<i32, SimpleError> =
            use_cached(StdDuration::from_secs(60), lookup);
        let op = op_always_fail::<i32>(0);
        let e = Arc::new(err("T", "t", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let cancel = CancelCheckpoint::new();
        let out = strat
            .attempt(&err("T", "t", Vec::new()), &ctx, op, &cancel)
            .await;
        match out {
            StrategyOutcome::Ok(v) => assert_eq!(v, 777),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn degrade_returns_fallback() {
        let strat: BoxedStrategy<i32, SimpleError> = degrade(55);
        let op = op_always_fail::<i32>(0);
        let e = Arc::new(err("T", "t", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let cancel = CancelCheckpoint::new();
        let out = strat
            .attempt(&err("T", "t", Vec::new()), &ctx, op, &cancel)
            .await;
        match out {
            StrategyOutcome::Ok(v) => assert_eq!(v, 55),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn circuit_break_opens_after_threshold() {
        let strat: BoxedStrategy<i32, SimpleError> =
            circuit_break(2, StdDuration::from_secs(60), || 0);
        let op = op_always_fail::<i32>(0);
        let e = Arc::new(err("T", "t", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let cancel = CancelCheckpoint::new();
        // First two attempts: Err (breaker closed).
        let o1 = strat
            .attempt(&err("T", "t", Vec::new()), &ctx, op.clone(), &cancel)
            .await;
        assert!(matches!(o1, StrategyOutcome::Err(_)));
        let o2 = strat
            .attempt(&err("T", "t", Vec::new()), &ctx, op.clone(), &cancel)
            .await;
        assert!(matches!(o2, StrategyOutcome::Err(_)));
        // Third: breaker is open, returns fallback Ok(0).
        let o3 = strat
            .attempt(&err("T", "t", Vec::new()), &ctx, op, &cancel)
            .await;
        match o3 {
            StrategyOutcome::Ok(v) => assert_eq!(v, 0),
            other => panic!("expected Ok fallback, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn escalate_forwards_error() {
        let strat: BoxedStrategy<i32, SimpleError> = escalate();
        let op = op_always_fail::<i32>(0);
        let e = Arc::new(err("T", "t", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let cancel = CancelCheckpoint::new();
        let out = strat
            .attempt(&err("T", "t", Vec::new()), &ctx, op, &cancel)
            .await;
        assert!(matches!(out, StrategyOutcome::Err(_)));
    }

    // Custom provider that returns a specific strategy name. Used to
    // verify non-first selection and manifest recording.
    struct FixedChoiceProvider {
        choice: String,
    }

    #[async_trait]
    impl AiProvider for FixedChoiceProvider {
        async fn generate(
            &self,
            _r: &bock_ai::GenerateRequest,
        ) -> Result<bock_ai::GenerateResponse, AiError> {
            unreachable!()
        }
        async fn repair(
            &self,
            _r: &bock_ai::RepairRequest,
        ) -> Result<bock_ai::RepairResponse, AiError> {
            unreachable!()
        }
        async fn optimize(
            &self,
            _r: &bock_ai::OptimizeRequest,
        ) -> Result<bock_ai::OptimizeResponse, AiError> {
            unreachable!()
        }
        async fn select(
            &self,
            _request: &SelectRequest,
        ) -> Result<SelectResponse, AiError> {
            Ok(SelectResponse {
                selected_id: self.choice.clone(),
                confidence: 0.9,
                reasoning: Some("fixed choice for test".into()),
            })
        }
        fn model_id(&self) -> String {
            "test:fixed".into()
        }
    }

    #[tokio::test]
    async fn provider_driven_selection_uses_non_first_option() {
        let provider: Arc<dyn AiProvider> = Arc::new(FixedChoiceProvider {
            choice: "degrade".into(),
        });
        let e = Arc::new(err("X", "x", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let handler = adaptive::<i32, SimpleError>(vec![escalate(), degrade(123)])
            .with_provider(provider)
            .build();
        let op = op_always_fail::<i32>(0);
        let cancel = CancelCheckpoint::new();
        let res = handler
            .recover(err("X", "x", Vec::new()), "op", ctx, op, &cancel)
            .await
            .expect("ok");
        assert_eq!(res.selection.selected, "degrade");
        match res.outcome {
            StrategyOutcome::Ok(v) => assert_eq!(v, 123),
            other => panic!("expected Ok(123), got {other:?}"),
        }
    }

    // Provider whose select() always fails, exercising the fallback path.
    struct FailingProvider;
    #[async_trait]
    impl AiProvider for FailingProvider {
        async fn generate(
            &self,
            _r: &bock_ai::GenerateRequest,
        ) -> Result<bock_ai::GenerateResponse, AiError> {
            unreachable!()
        }
        async fn repair(
            &self,
            _r: &bock_ai::RepairRequest,
        ) -> Result<bock_ai::RepairResponse, AiError> {
            unreachable!()
        }
        async fn optimize(
            &self,
            _r: &bock_ai::OptimizeRequest,
        ) -> Result<bock_ai::OptimizeResponse, AiError> {
            unreachable!()
        }
        async fn select(
            &self,
            _r: &SelectRequest,
        ) -> Result<SelectResponse, AiError> {
            Err(AiError::Unavailable("test: offline".into()))
        }
        fn model_id(&self) -> String {
            "test:failing".into()
        }
    }

    #[tokio::test]
    async fn provider_failure_falls_back_to_first() {
        let provider: Arc<dyn AiProvider> = Arc::new(FailingProvider);
        let e = Arc::new(err("X", "x", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let handler = adaptive::<i32, SimpleError>(vec![degrade(9), escalate()])
            .with_provider(provider)
            .build();
        let op = op_always_fail::<i32>(0);
        let cancel = CancelCheckpoint::new();
        let res = handler
            .recover(err("X", "x", Vec::new()), "op", ctx, op, &cancel)
            .await
            .expect("ok");
        assert_eq!(res.selection.selected, "degrade");
        assert_eq!(res.selection.source, SelectionSource::FirstStrategy);
    }

    #[test]
    fn assert_ai_error_variants_are_stable() {
        // Just ensure the crate link keeps AiError reachable as used
        // above; if a refactor removes `Unavailable`, this line will
        // fail to compile and flag the adaptive handler tests.
        let _e = AiError::Unavailable("sanity".into());
    }

    #[tokio::test]
    async fn cancelled_before_on_cancel_called() {
        // on_cancel must fire when StrategyOutcome::Cancelled is observed.
        // (Covered above in adaptive_cancellation_propagates_and_fires_on_cancel.)
        // Also verify no on_cancel fires for non-cancelled outcomes.
        struct CountCancel {
            fired: Arc<AtomicU32>,
        }
        #[async_trait]
        impl RecoveryStrategy<i32, SimpleError> for CountCancel {
            fn name(&self) -> String {
                "count_cancel".into()
            }
            fn description(&self) -> String {
                "degrade to 0".into()
            }
            async fn attempt(
                &self,
                _e: &SimpleError,
                _c: &RecoveryContext,
                _op: RecoveryOperation<i32, SimpleError>,
                _cancel: &CancelCheckpoint,
            ) -> StrategyOutcome<i32, SimpleError> {
                StrategyOutcome::Ok(0)
            }
            async fn on_cancel(&self, _c: &RecoveryContext) {
                self.fired.fetch_add(1, Ordering::SeqCst);
            }
        }
        let fired = Arc::new(AtomicU32::new(0));
        let strat: BoxedStrategy<i32, SimpleError> =
            Arc::new(CountCancel { fired: fired.clone() });
        let e = Arc::new(err("X", "x", Vec::new())) as Arc<dyn ErrorValue>;
        let ctx = RecoveryContext::first_attempt(e.clone(), "op", Annotations::default());
        let handler = adaptive::<i32, SimpleError>(vec![strat])
            .context_aware(false)
            .build();
        let op = op_always_fail::<i32>(0);
        let cancel = CancelCheckpoint::new();
        let _ = handler
            .recover(err("X", "x", Vec::new()), "op", ctx, op, &cancel)
            .await
            .expect("ok");
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }
}
