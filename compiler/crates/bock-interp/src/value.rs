//! Runtime value representation for the Bock interpreter.
//!
//! Every Bock type maps to a [`Value`] variant. Collections that require
//! ordering ([`BTreeMap`], [`BTreeSet`]) are supported via [`OrdF64`], a
//! total-order wrapper for `f64`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use crate::error::RuntimeError;

// ─── ChannelHandle ───────────────────────────────────────────────────────────

/// Shared state for an unbounded MPSC channel of [`Value`]s.
///
/// The `Channel[T]` runtime type holds both a sender and a receiver. Both
/// ends of the channel share a single `Arc<ChannelHandle>`, so cloning or
/// passing a channel around does not duplicate the underlying queue.
///
/// `Channel.new()` returns a pair of clones of the same handle so that
/// `send` and `recv` both operate on the same underlying mpsc.
#[derive(Debug)]
pub struct ChannelHandle {
    /// The sending end; clones share the same underlying producer.
    pub sender: UnboundedSender<Value>,
    /// The receiving end behind an async mutex so a single consumer at a time
    /// can `.recv().await`.
    pub receiver: AsyncMutex<UnboundedReceiver<Value>>,
}

impl ChannelHandle {
    /// Create a new unbounded channel handle.
    #[must_use]
    pub fn new() -> Arc<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(ChannelHandle {
            sender: tx,
            receiver: AsyncMutex::new(rx),
        })
    }
}

// ─── OrdF64 ───────────────────────────────────────────────────────────────────

/// A total-order wrapper for `f64`.
///
/// Uses [`f64::total_cmp`] (stable since Rust 1.62) which defines:
/// `-NaN < -Inf < … < -0.0 < +0.0 < … < +Inf < +NaN`.
///
/// This is necessary because [`BTreeMap`] and [`BTreeSet`] require [`Ord`],
/// but raw `f64` only implements [`PartialOrd`].
#[derive(Debug, Clone, Copy)]
pub struct OrdF64(pub f64);

impl PartialEq for OrdF64 {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0) == std::cmp::Ordering::Equal
    }
}

impl Eq for OrdF64 {}

impl PartialOrd for OrdF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Hash for OrdF64 {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Consistent with total_cmp equality: equal values share the same bit pattern.
        self.0.to_bits().hash(state);
    }
}

impl fmt::Display for OrdF64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<f64> for OrdF64 {
    fn from(v: f64) -> Self {
        OrdF64(v)
    }
}

// ─── BockString ──────────────────────────────────────────────────────────────

/// An Bock string value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BockString(String);

impl BockString {
    /// Create an [`BockString`] from any string-like value.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        BockString(s.into())
    }

    /// View as a `str` slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BockString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for BockString {
    fn from(s: String) -> Self {
        BockString(s)
    }
}

impl From<&str> for BockString {
    fn from(s: &str) -> Self {
        BockString(s.to_owned())
    }
}

// ─── RecordValue ─────────────────────────────────────────────────────────────

/// A record (struct) value: a named type with named fields.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordValue {
    /// The record's type name.
    pub type_name: String,
    /// Fields stored in sorted key order for deterministic comparison.
    pub fields: BTreeMap<String, Value>,
}

// ─── EnumValue ───────────────────────────────────────────────────────────────

/// An enum (sum type) value: a named variant with an optional payload.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnumValue {
    /// The enum type name.
    pub type_name: String,
    /// The variant name.
    pub variant: String,
    /// Optional payload for variants that carry data.
    pub payload: Option<Box<Value>>,
}

// ─── FnValue ─────────────────────────────────────────────────────────────────

/// Global unique-ID counter for function instances.
static NEXT_FN_ID: AtomicU64 = AtomicU64::new(1);

/// A function value.
///
/// Functions are equal only to themselves (compared by [`id`](FnValue::id)).
/// Attempting to **order** function values — e.g. by using one as a map key or
/// set element — is a **runtime error** and will panic with a clear message.
#[derive(Debug, Clone)]
pub struct FnValue {
    /// Unique identifier assigned at creation.
    pub id: u64,
    /// Optional human-readable name, for display.
    pub name: Option<String>,
}

impl FnValue {
    /// Create an anonymous function value with a fresh unique identity.
    #[must_use]
    pub fn new_anonymous() -> Self {
        FnValue {
            id: NEXT_FN_ID.fetch_add(1, AtomicOrdering::Relaxed),
            name: None,
        }
    }

    /// Create a named function value with a fresh unique identity.
    #[must_use]
    pub fn new_named(name: impl Into<String>) -> Self {
        FnValue {
            id: NEXT_FN_ID.fetch_add(1, AtomicOrdering::Relaxed),
            name: Some(name.into()),
        }
    }
}

impl PartialEq for FnValue {
    /// Functions are equal iff they share the same identity.
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for FnValue {}

impl Hash for FnValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl fmt::Display for FnValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(n) => write!(f, "<fn {n}>"),
            None => write!(f, "<fn #{}>", self.id),
        }
    }
}

// ─── IteratorValue ────────────────────────────────────────────────────────────

/// Global unique-ID counter for iterator instances.
static NEXT_ITER_ID: AtomicU64 = AtomicU64::new(1);

/// The internal state of a lazy iterator.
///
/// Each variant wraps a source iterator (or raw data) and computes values
/// on demand via [`IteratorKind::next`]. Combinators like `map` and `filter`
/// require interpreter support to invoke Bock closures — these store the
/// function value but their `next()` returns a special
/// [`IteratorNext::NeedsCallback`] signal.
#[derive(Debug)]
pub enum IteratorKind {
    /// Iterate over a list of values.
    List { items: Vec<Value>, pos: usize },
    /// Iterate over an integer range.
    Range {
        current: i64,
        end: i64,
        inclusive: bool,
        step: i64,
    },
    /// Iterate over set elements.
    Set { items: Vec<Value>, pos: usize },
    /// Iterate over map entries as (key, value) tuples.
    MapEntries {
        items: Vec<(Value, Value)>,
        pos: usize,
    },
    /// Lazy map combinator — requires interpreter callback.
    Map {
        source: Arc<Mutex<IteratorKind>>,
        func: FnValue,
    },
    /// Lazy filter combinator — requires interpreter callback.
    Filter {
        source: Arc<Mutex<IteratorKind>>,
        pred: FnValue,
    },
    /// Take at most N elements.
    Take {
        source: Arc<Mutex<IteratorKind>>,
        remaining: usize,
    },
    /// Skip the first N elements.
    Skip {
        source: Arc<Mutex<IteratorKind>>,
        to_skip: usize,
        skipped: bool,
    },
    /// Attach an index to each element.
    Enumerate {
        source: Arc<Mutex<IteratorKind>>,
        index: usize,
    },
    /// Zip two iterators together.
    Zip {
        a: Arc<Mutex<IteratorKind>>,
        b: Arc<Mutex<IteratorKind>>,
    },
    /// Chain two iterators sequentially.
    Chain {
        a: Arc<Mutex<IteratorKind>>,
        b: Arc<Mutex<IteratorKind>>,
        first_done: bool,
    },
}

/// Result of calling [`IteratorKind::next`].
///
/// Most combinators can compute the next value directly, but `map` and `filter`
/// need the interpreter to invoke an Bock closure. They return `NeedsCallback`
/// with the source value and the function to call.
#[derive(Debug)]
pub enum IteratorNext {
    /// A value was produced.
    Some(Value),
    /// The iterator is exhausted.
    Done,
    /// The combinator needs the interpreter to call `func(value)` and feed the
    /// result back. Used by `Map`.
    NeedsMapCallback { value: Value, func: FnValue },
    /// The combinator needs the interpreter to call `pred(value)` and feed
    /// back whether to keep this element. Used by `Filter`.
    NeedsFilterCallback { value: Value, func: FnValue },
}

impl IteratorKind {
    /// Advance the iterator and return the next value.
    ///
    /// For combinators that need Bock function calls (map, filter), this
    /// returns [`IteratorNext::NeedsMapCallback`] or
    /// [`IteratorNext::NeedsFilterCallback`] instead of a plain value.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> IteratorNext {
        match self {
            IteratorKind::List { items, pos } => {
                if *pos < items.len() {
                    let val = items[*pos].clone();
                    *pos += 1;
                    IteratorNext::Some(val)
                } else {
                    IteratorNext::Done
                }
            }
            IteratorKind::Range {
                current,
                end,
                inclusive,
                step,
            } => {
                let in_bounds = if *step > 0 {
                    if *inclusive {
                        *current <= *end
                    } else {
                        *current < *end
                    }
                } else if *step < 0 {
                    if *inclusive {
                        *current >= *end
                    } else {
                        *current > *end
                    }
                } else {
                    false // zero step = no progress
                };
                if in_bounds {
                    let val = *current;
                    *current += *step;
                    IteratorNext::Some(Value::Int(val))
                } else {
                    IteratorNext::Done
                }
            }
            IteratorKind::Set { items, pos } => {
                if *pos < items.len() {
                    let val = items[*pos].clone();
                    *pos += 1;
                    IteratorNext::Some(val)
                } else {
                    IteratorNext::Done
                }
            }
            IteratorKind::MapEntries { items, pos } => {
                if *pos < items.len() {
                    let (k, v) = items[*pos].clone();
                    *pos += 1;
                    IteratorNext::Some(Value::Tuple(vec![k, v]))
                } else {
                    IteratorNext::Done
                }
            }
            IteratorKind::Map { source, func } => {
                let mut src = source.lock().unwrap();
                match src.next() {
                    IteratorNext::Some(val) => IteratorNext::NeedsMapCallback {
                        value: val,
                        func: func.clone(),
                    },
                    IteratorNext::Done => IteratorNext::Done,
                    // Propagate upstream callback requests
                    other => other,
                }
            }
            IteratorKind::Filter { source, pred } => {
                let mut src = source.lock().unwrap();
                match src.next() {
                    IteratorNext::Some(val) => IteratorNext::NeedsFilterCallback {
                        value: val,
                        func: pred.clone(),
                    },
                    IteratorNext::Done => IteratorNext::Done,
                    other => other,
                }
            }
            IteratorKind::Take { source, remaining } => {
                if *remaining == 0 {
                    return IteratorNext::Done;
                }
                *remaining -= 1;
                source.lock().unwrap().next()
            }
            IteratorKind::Skip {
                source,
                to_skip,
                skipped,
            } => {
                if !*skipped {
                    *skipped = true;
                    let mut src = source.lock().unwrap();
                    for _ in 0..*to_skip {
                        match src.next() {
                            IteratorNext::Done => return IteratorNext::Done,
                            IteratorNext::Some(_) => {}
                            other => return other,
                        }
                    }
                }
                source.lock().unwrap().next()
            }
            IteratorKind::Enumerate { source, index } => {
                let mut src = source.lock().unwrap();
                match src.next() {
                    IteratorNext::Some(val) => {
                        let idx = *index;
                        *index += 1;
                        IteratorNext::Some(Value::Tuple(vec![Value::Int(idx as i64), val]))
                    }
                    other => other,
                }
            }
            IteratorKind::Zip { a, b } => {
                let next_a = a.lock().unwrap().next();
                match next_a {
                    IteratorNext::Some(va) => {
                        let next_b = b.lock().unwrap().next();
                        match next_b {
                            IteratorNext::Some(vb) => {
                                IteratorNext::Some(Value::Tuple(vec![va, vb]))
                            }
                            IteratorNext::Done => IteratorNext::Done,
                            other => other,
                        }
                    }
                    IteratorNext::Done => IteratorNext::Done,
                    other => other,
                }
            }
            IteratorKind::Chain { a, b, first_done } => {
                if !*first_done {
                    let result = a.lock().unwrap().next();
                    match result {
                        IteratorNext::Done => {
                            *first_done = true;
                            b.lock().unwrap().next()
                        }
                        other => other,
                    }
                } else {
                    b.lock().unwrap().next()
                }
            }
        }
    }
}

/// A lazy iterator value.
///
/// Iterators are identity-compared (like functions). Ordering is a runtime error.
/// Interior mutability via [`Mutex`] allows `next()` to advance the state
/// through shared references and across tasks.
#[derive(Debug, Clone)]
pub struct IteratorValue {
    /// Unique identity for equality comparison.
    pub id: u64,
    /// The iterator state.
    pub kind: Arc<Mutex<IteratorKind>>,
}

impl IteratorValue {
    /// Create a new iterator value wrapping the given kind.
    #[must_use]
    pub fn new(kind: IteratorKind) -> Self {
        IteratorValue {
            id: NEXT_ITER_ID.fetch_add(1, AtomicOrdering::Relaxed),
            kind: Arc::new(Mutex::new(kind)),
        }
    }
}

impl PartialEq for IteratorValue {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for IteratorValue {}

impl Hash for IteratorValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl fmt::Display for IteratorValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<iterator #{}>", self.id)
    }
}

// ─── Value ───────────────────────────────────────────────────────────────────

/// A runtime value in the Bock interpreter.
///
/// Every Bock type maps to a variant. Collections requiring total order
/// ([`Map`](Value::Map), [`Set`](Value::Set)) work because [`Value`] implements
/// [`Ord`] — with the single exception that ordering [`Function`](Value::Function)
/// values is a runtime panic.
#[derive(Debug, Clone)]
pub enum Value {
    /// 64-bit signed integer.
    Int(i64),
    /// 64-bit float with total ordering.
    Float(OrdF64),
    /// Boolean.
    Bool(bool),
    /// Unicode string.
    String(BockString),
    /// Unicode scalar character.
    Char(char),
    /// Unit / void.
    Void,
    /// Homogeneous list.
    List(Vec<Value>),
    /// Ordered key-value map (keys must not be functions).
    Map(BTreeMap<Value, Value>),
    /// Ordered set (elements must not be functions).
    Set(BTreeSet<Value>),
    /// Fixed-size heterogeneous tuple.
    Tuple(Vec<Value>),
    /// Record (struct) value.
    Record(RecordValue),
    /// Enum (sum type) value.
    Enum(EnumValue),
    /// Function value — equal only to itself; ordering is a runtime error.
    Function(FnValue),
    /// Optional value (`Some(v)` or `None`).
    Optional(Option<Box<Value>>),
    /// Result value (`Ok(v)` or `Err(e)`).
    Result(std::result::Result<Box<Value>, Box<Value>>),
    /// An integer range with a step (produced by `lo..hi` / `lo..=hi` / `.step()`).
    Range {
        start: i64,
        end: i64,
        inclusive: bool,
        step: i64,
    },
    /// A lazy iterator value.
    Iterator(IteratorValue),
    /// Mutable string builder for efficient concatenation.
    StringBuilder(Arc<Mutex<String>>),
    /// A pending async computation.
    ///
    /// Created when an `async fn` is called; resolved by `await`.
    /// The `JoinHandle` lives behind an `Arc<Mutex<Option<...>>>` so the
    /// Future value can be cloned (handles share the same task) and the
    /// handle can be `take()`-n on first await.
    Future(FutureHandle),
    /// A time duration as signed nanoseconds (±292 year range).
    Duration(i64),
    /// A monotonic point in time (per-process).
    Instant(std::time::Instant),
    /// An unbounded async channel. `Channel.new()` returns a tuple of two
    /// clones of the same handle; `send` and `recv` both operate on the
    /// shared mpsc queue.
    Channel(Arc<ChannelHandle>),
}

/// Shared handle to a spawned async task. See [`Value::Future`].
pub type FutureHandle = Arc<Mutex<Option<JoinHandle<Result<Value, RuntimeError>>>>>;

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Char(a), Value::Char(b)) => a == b,
            (Value::Void, Value::Void) => true,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Set(a), Value::Set(b)) => a == b,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Record(a), Value::Record(b)) => a == b,
            (Value::Enum(a), Value::Enum(b)) => a == b,
            (Value::Function(a), Value::Function(b)) => a == b,
            (Value::Optional(a), Value::Optional(b)) => a == b,
            (Value::Result(a), Value::Result(b)) => match (a, b) {
                (Ok(av), Ok(bv)) => av == bv,
                (Err(ae), Err(be)) => ae == be,
                _ => false,
            },
            (
                Value::Range {
                    start: s1,
                    end: e1,
                    inclusive: i1,
                    step: st1,
                },
                Value::Range {
                    start: s2,
                    end: e2,
                    inclusive: i2,
                    step: st2,
                },
            ) => s1 == s2 && e1 == e2 && i1 == i2 && st1 == st2,
            (Value::Iterator(a), Value::Iterator(b)) => a == b,
            (Value::StringBuilder(a), Value::StringBuilder(b)) => Arc::ptr_eq(a, b),
            (Value::Future(a), Value::Future(b)) => Arc::ptr_eq(a, b),
            (Value::Duration(a), Value::Duration(b)) => a == b,
            (Value::Instant(a), Value::Instant(b)) => a == b,
            (Value::Channel(a), Value::Channel(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for Value {}

/// Numeric discriminant used for cross-variant ordering.
fn variant_ord(v: &Value) -> u8 {
    match v {
        Value::Void => 0,
        Value::Bool(_) => 1,
        Value::Int(_) => 2,
        Value::Float(_) => 3,
        Value::Char(_) => 4,
        Value::String(_) => 5,
        Value::Tuple(_) => 6,
        Value::List(_) => 7,
        Value::Set(_) => 8,
        Value::Map(_) => 9,
        Value::Record(_) => 10,
        Value::Enum(_) => 11,
        Value::Optional(_) => 12,
        Value::Result(_) => 13,
        Value::Function(_) => 14,
        Value::Range { .. } => 15,
        Value::Iterator(_) => 16,
        Value::StringBuilder(_) => 17,
        Value::Future(_) => 18,
        Value::Duration(_) => 19,
        Value::Instant(_) => 20,
        Value::Channel(_) => 21,
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        match (self, other) {
            (Value::Void, Value::Void) => Ordering::Equal,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => a.cmp(b),
            (Value::Char(a), Value::Char(b)) => a.cmp(b),
            (Value::String(a), Value::String(b)) => a.cmp(b),
            (Value::Tuple(a), Value::Tuple(b)) => a.cmp(b),
            (Value::List(a), Value::List(b)) => a.cmp(b),
            (Value::Set(a), Value::Set(b)) => a.cmp(b),
            (Value::Map(a), Value::Map(b)) => a.cmp(b),
            (Value::Record(a), Value::Record(b)) => a.cmp(b),
            (Value::Enum(a), Value::Enum(b)) => a.cmp(b),
            (Value::Optional(a), Value::Optional(b)) => a.cmp(b),
            (Value::Result(a), Value::Result(b)) => match (a, b) {
                (Ok(av), Ok(bv)) => av.cmp(bv),
                (Err(ae), Err(be)) => ae.cmp(be),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
            },
            (
                Value::Range {
                    start: s1,
                    end: e1,
                    inclusive: i1,
                    step: st1,
                },
                Value::Range {
                    start: s2,
                    end: e2,
                    inclusive: i2,
                    step: st2,
                },
            ) => (s1, e1, i1, st1).cmp(&(s2, e2, i2, st2)),
            // SAFETY: Ord requires a total ordering but these types have no meaningful
            // order. Reaching these arms indicates a program logic error (e.g. using
            // functions as map keys). We use unreachable!() to signal the invariant.
            (Value::Function(_), Value::Function(_)) => {
                unreachable!("function values are not orderable and cannot be used as map keys or set elements")
            }
            (Value::Iterator(_), Value::Iterator(_)) => {
                unreachable!("iterator values are not orderable and cannot be used as map keys or set elements")
            }
            (Value::StringBuilder(_), Value::StringBuilder(_)) => {
                unreachable!("StringBuilder values are not orderable")
            }
            (Value::Future(_), Value::Future(_)) => {
                unreachable!("Future values are not orderable")
            }
            (Value::Channel(_), Value::Channel(_)) => {
                unreachable!("Channel values are not orderable")
            }
            (Value::Duration(a), Value::Duration(b)) => a.cmp(b),
            (Value::Instant(a), Value::Instant(b)) => a.cmp(b),
            // Different variants: order by discriminant.
            _ => variant_ord(self).cmp(&variant_ord(other)),
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        variant_ord(self).hash(state);
        match self {
            Value::Int(v) => v.hash(state),
            Value::Float(v) => v.hash(state),
            Value::Bool(v) => v.hash(state),
            Value::String(v) => v.hash(state),
            Value::Char(v) => v.hash(state),
            Value::Void => {}
            Value::List(v) => v.hash(state),
            Value::Tuple(v) => v.hash(state),
            Value::Record(v) => v.hash(state),
            Value::Enum(v) => v.hash(state),
            Value::Function(v) => v.hash(state),
            Value::Optional(v) => v.hash(state),
            // BTreeSet/BTreeMap iterate in sorted order — hashing is deterministic.
            Value::Set(v) => {
                for item in v {
                    item.hash(state);
                }
            }
            Value::Map(v) => {
                for (k, val) in v {
                    k.hash(state);
                    val.hash(state);
                }
            }
            Value::Result(v) => match v {
                Ok(inner) => {
                    0u8.hash(state);
                    inner.hash(state);
                }
                Err(inner) => {
                    1u8.hash(state);
                    inner.hash(state);
                }
            },
            Value::Range {
                start,
                end,
                inclusive,
                step,
            } => {
                start.hash(state);
                end.hash(state);
                inclusive.hash(state);
                step.hash(state);
            }
            Value::Iterator(v) => v.hash(state),
            Value::StringBuilder(v) => v.lock().unwrap().hash(state),
            Value::Future(v) => (Arc::as_ptr(v) as usize).hash(state),
            Value::Duration(v) => v.hash(state),
            Value::Instant(v) => v.hash(state),
            Value::Channel(v) => (Arc::as_ptr(v) as usize).hash(state),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::Bool(true) => write!(f, "true"),
            Value::Bool(false) => write!(f, "false"),
            Value::String(v) => write!(f, "{v}"),
            Value::Char(v) => write!(f, "'{v}'"),
            Value::Void => write!(f, "void"),
            Value::List(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Value::Set(items) => {
                write!(f, "{{")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "}}")
            }
            Value::Map(items) => {
                write!(f, "{{")?;
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Tuple(items) => {
                write!(f, "(")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, ")")
            }
            Value::Record(r) => {
                write!(f, "{} {{", r.type_name)?;
                for (i, (k, v)) in r.fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Enum(e) => {
                write!(f, "{}.{}", e.type_name, e.variant)?;
                if let Some(payload) = &e.payload {
                    write!(f, "({payload})")?;
                }
                Ok(())
            }
            Value::Function(fn_val) => write!(f, "{fn_val}"),
            Value::Optional(Some(v)) => write!(f, "Some({v})"),
            Value::Optional(None) => write!(f, "None"),
            Value::Result(Ok(v)) => write!(f, "Ok({v})"),
            Value::Result(Err(e)) => write!(f, "Err({e})"),
            Value::Range {
                start,
                end,
                inclusive,
                step,
            } => {
                if *step == 1 {
                    if *inclusive {
                        write!(f, "{start}..={end}")
                    } else {
                        write!(f, "{start}..{end}")
                    }
                } else if *inclusive {
                    write!(f, "{start}..={end} step {step}")
                } else {
                    write!(f, "{start}..{end} step {step}")
                }
            }
            Value::Iterator(v) => write!(f, "{v}"),
            Value::StringBuilder(v) => write!(f, "<StringBuilder len={}>", v.lock().unwrap().len()),
            Value::Future(_) => write!(f, "<future>"),
            Value::Duration(nanos) => write!(f, "{}", format_duration(*nanos)),
            Value::Instant(_) => write!(f, "<instant>"),
            Value::Channel(_) => write!(f, "<channel>"),
        }
    }
}

/// Format a duration in nanoseconds using the most natural unit.
fn format_duration(nanos: i64) -> String {
    if nanos == 0 {
        return "0s".to_string();
    }
    let sign = if nanos < 0 { "-" } else { "" };
    let abs_nanos = nanos.unsigned_abs();
    if abs_nanos >= 1_000_000_000 {
        let secs = abs_nanos as f64 / 1_000_000_000.0;
        format!("{sign}{secs}s")
    } else if abs_nanos >= 1_000_000 {
        let ms = abs_nanos as f64 / 1_000_000.0;
        format!("{sign}{ms}ms")
    } else if abs_nanos >= 1_000 {
        let us = abs_nanos as f64 / 1_000.0;
        format!("{sign}{us}µs")
    } else {
        format!("{sign}{abs_nanos}ns")
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── OrdF64 ────────────────────────────────────────────────────────────────

    #[test]
    fn ordf64_total_order_neg_zero_vs_pos_zero() {
        let neg = OrdF64(-0.0_f64);
        let pos = OrdF64(0.0_f64);
        assert!(neg < pos, "-0.0 should be less than +0.0 under total order");
    }

    #[test]
    fn ordf64_nan_is_ordered() {
        let nan = OrdF64(f64::NAN);
        let inf = OrdF64(f64::INFINITY);
        assert!(inf < nan, "+Inf should be less than +NaN under total order");
    }

    #[test]
    fn ordf64_equality_uses_total_cmp() {
        assert_ne!(OrdF64(-0.0), OrdF64(0.0));
        assert_eq!(OrdF64(1.0), OrdF64(1.0));
    }

    // ── BockString ────────────────────────────────────────────────────────────

    #[test]
    fn bock_string_ord() {
        let a = BockString::new("apple");
        let b = BockString::new("banana");
        assert!(a < b);
    }

    #[test]
    fn bock_string_display() {
        let s = BockString::new("hello");
        assert_eq!(s.to_string(), "hello");
    }

    // ── Value equality ────────────────────────────────────────────────────────

    #[test]
    fn int_equality() {
        assert_eq!(Value::Int(42), Value::Int(42));
        assert_ne!(Value::Int(1), Value::Int(2));
    }

    #[test]
    fn float_equality() {
        assert_eq!(Value::Float(OrdF64(1.5)), Value::Float(OrdF64(1.5)));
        assert_ne!(Value::Float(OrdF64(-0.0)), Value::Float(OrdF64(0.0)));
    }

    #[test]
    fn bool_equality() {
        assert_eq!(Value::Bool(true), Value::Bool(true));
        assert_ne!(Value::Bool(true), Value::Bool(false));
    }

    #[test]
    fn string_equality() {
        assert_eq!(
            Value::String(BockString::new("hi")),
            Value::String(BockString::new("hi"))
        );
    }

    #[test]
    fn void_equality() {
        assert_eq!(Value::Void, Value::Void);
    }

    #[test]
    fn different_variants_not_equal() {
        assert_ne!(Value::Int(0), Value::Bool(false));
    }

    // ── Function identity ─────────────────────────────────────────────────────

    #[test]
    fn fn_equality_by_identity() {
        let f1 = FnValue::new_named("foo");
        let f2 = FnValue::new_named("foo");
        assert_eq!(f1, f1.clone());
        assert_ne!(f1, f2);
    }

    #[test]
    fn fn_value_equality_by_identity() {
        let f1 = FnValue::new_anonymous();
        let f2 = FnValue::new_anonymous();
        let v1 = Value::Function(f1.clone());
        let v1_clone = Value::Function(f1);
        let v2 = Value::Function(f2);
        assert_eq!(v1, v1_clone);
        assert_ne!(v1_clone, v2);
    }

    #[test]
    #[should_panic(expected = "function values are not orderable")]
    fn fn_value_ordering_panics() {
        let f1 = Value::Function(FnValue::new_anonymous());
        let f2 = Value::Function(FnValue::new_anonymous());
        let _ = f1.cmp(&f2);
    }

    // ── Value ordering ────────────────────────────────────────────────────────

    #[test]
    fn int_ordering() {
        assert!(Value::Int(1) < Value::Int(2));
        assert!(Value::Int(2) > Value::Int(1));
    }

    #[test]
    fn bool_ordering() {
        assert!(Value::Bool(false) < Value::Bool(true));
    }

    #[test]
    fn optional_none_less_than_some() {
        assert!(Value::Optional(None) < Value::Optional(Some(Box::new(Value::Int(0)))));
    }

    #[test]
    fn result_ok_less_than_err() {
        let ok = Value::Result(Ok(Box::new(Value::Int(0))));
        let err = Value::Result(Err(Box::new(Value::Int(0))));
        assert!(ok < err);
    }

    #[test]
    fn cross_variant_ordering_by_discriminant() {
        // Void (0) < Bool (1) < Int (2)
        assert!(Value::Void < Value::Bool(false));
        assert!(Value::Bool(false) < Value::Int(0));
    }

    // ── Value in BTreeMap / BTreeSet ─────────────────────────────────────────

    #[test]
    fn value_as_btreemap_key() {
        let mut map = BTreeMap::new();
        map.insert(Value::Int(1), Value::String(BockString::new("one")));
        map.insert(Value::Int(2), Value::String(BockString::new("two")));
        assert_eq!(
            map.get(&Value::Int(1)),
            Some(&Value::String(BockString::new("one")))
        );
    }

    #[test]
    fn value_as_btreeset_element() {
        let mut set = BTreeSet::new();
        set.insert(Value::Int(3));
        set.insert(Value::Int(1));
        set.insert(Value::Int(2));
        let sorted: Vec<_> = set.iter().collect();
        assert_eq!(sorted[0], &Value::Int(1));
        assert_eq!(sorted[2], &Value::Int(3));
    }

    #[test]
    fn float_as_btreeset_element() {
        let mut set = BTreeSet::new();
        set.insert(Value::Float(OrdF64(3.0)));
        set.insert(Value::Float(OrdF64(1.0)));
        set.insert(Value::Float(OrdF64(2.0)));
        let mut iter = set.iter();
        assert_eq!(iter.next(), Some(&Value::Float(OrdF64(1.0))));
    }

    // ── Display ───────────────────────────────────────────────────────────────

    #[test]
    fn display_primitives() {
        assert_eq!(Value::Int(42).to_string(), "42");
        assert_eq!(Value::Float(OrdF64(3.14)).to_string(), "3.14");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Bool(false).to_string(), "false");
        assert_eq!(Value::String(BockString::new("hi")).to_string(), "hi");
        assert_eq!(Value::Char('x').to_string(), "'x'");
        assert_eq!(Value::Void.to_string(), "void");
    }

    #[test]
    fn display_list() {
        let v = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        assert_eq!(v.to_string(), "[1, 2, 3]");
    }

    #[test]
    fn display_tuple() {
        let v = Value::Tuple(vec![Value::Int(1), Value::Bool(true)]);
        assert_eq!(v.to_string(), "(1, true)");
    }

    #[test]
    fn display_optional() {
        assert_eq!(
            Value::Optional(Some(Box::new(Value::Int(5)))).to_string(),
            "Some(5)"
        );
        assert_eq!(Value::Optional(None).to_string(), "None");
    }

    #[test]
    fn display_result() {
        assert_eq!(
            Value::Result(Ok(Box::new(Value::Int(0)))).to_string(),
            "Ok(0)"
        );
        assert_eq!(
            Value::Result(Err(Box::new(Value::String(BockString::new("fail"))))).to_string(),
            "Err(fail)"
        );
    }

    #[test]
    fn display_enum_without_payload() {
        let v = Value::Enum(EnumValue {
            type_name: "Color".into(),
            variant: "Red".into(),
            payload: None,
        });
        assert_eq!(v.to_string(), "Color.Red");
    }

    #[test]
    fn display_enum_with_payload() {
        let v = Value::Enum(EnumValue {
            type_name: "Shape".into(),
            variant: "Circle".into(),
            payload: Some(Box::new(Value::Float(OrdF64(1.0)))),
        });
        assert_eq!(v.to_string(), "Shape.Circle(1)");
    }

    #[test]
    fn display_record() {
        let mut fields = BTreeMap::new();
        fields.insert("x".to_string(), Value::Int(1));
        fields.insert("y".to_string(), Value::Int(2));
        let v = Value::Record(RecordValue {
            type_name: "Point".into(),
            fields,
        });
        assert_eq!(v.to_string(), "Point {x: 1, y: 2}");
    }

    #[test]
    fn display_function_named() {
        let v = Value::Function(FnValue::new_named("add"));
        assert_eq!(v.to_string(), "<fn add>");
    }

    // ── Clone ─────────────────────────────────────────────────────────────────

    #[test]
    fn value_clone() {
        let original = Value::List(vec![Value::Int(1), Value::Bool(true)]);
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    // ── Nested values ─────────────────────────────────────────────────────────

    #[test]
    fn nested_map_value() {
        let inner = Value::Map(BTreeMap::from([(Value::Int(1), Value::Bool(true))]));
        let outer = Value::List(vec![inner]);
        assert_eq!(outer.to_string(), "[{1: true}]");
    }
}
