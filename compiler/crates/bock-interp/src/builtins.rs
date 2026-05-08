//! Built-in method dispatch registry for the Bock interpreter.
//!
//! Provides a `(TypeTag, method_name) → Rust function` dispatch table with a
//! registration API. Phase 6 packages (P6.1–P6.6) can extend the registry
//! without modifying this module.

use std::collections::HashMap;

use futures::future::BoxFuture;

use crate::error::RuntimeError;
use crate::value::{BockString, OrdF64, Value};

// ─── TypeTag ──────────────────────────────────────────────────────────────────

/// Identifies the runtime type of a [`Value`] for method dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeTag {
    Int,
    Float,
    Bool,
    String,
    Char,
    Void,
    List,
    Map,
    Set,
    Tuple,
    Record,
    Enum,
    Function,
    Optional,
    Result,
    Range,
    Iterator,
    StringBuilder,
    Future,
    Duration,
    Instant,
    Channel,
}

impl TypeTag {
    /// Determine the [`TypeTag`] for a given [`Value`].
    #[must_use]
    pub fn of(value: &Value) -> Self {
        match value {
            Value::Int(_) => TypeTag::Int,
            Value::Float(_) => TypeTag::Float,
            Value::Bool(_) => TypeTag::Bool,
            Value::String(_) => TypeTag::String,
            Value::Char(_) => TypeTag::Char,
            Value::Void => TypeTag::Void,
            Value::List(_) => TypeTag::List,
            Value::Map(_) => TypeTag::Map,
            Value::Set(_) => TypeTag::Set,
            Value::Tuple(_) => TypeTag::Tuple,
            Value::Record(_) => TypeTag::Record,
            Value::Enum(_) => TypeTag::Enum,
            Value::Function(_) => TypeTag::Function,
            Value::Optional(_) => TypeTag::Optional,
            Value::Result(_) => TypeTag::Result,
            Value::Range { .. } => TypeTag::Range,
            Value::Iterator(_) => TypeTag::Iterator,
            Value::StringBuilder(_) => TypeTag::StringBuilder,
            Value::Future(_) => TypeTag::Future,
            Value::Duration(_) => TypeTag::Duration,
            Value::Instant(_) => TypeTag::Instant,
            Value::Channel(_) => TypeTag::Channel,
        }
    }

    /// Human-readable name for error messages.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            TypeTag::Int => "Int",
            TypeTag::Float => "Float",
            TypeTag::Bool => "Bool",
            TypeTag::String => "String",
            TypeTag::Char => "Char",
            TypeTag::Void => "Void",
            TypeTag::List => "List",
            TypeTag::Map => "Map",
            TypeTag::Set => "Set",
            TypeTag::Tuple => "Tuple",
            TypeTag::Record => "Record",
            TypeTag::Enum => "Enum",
            TypeTag::Function => "Function",
            TypeTag::Optional => "Optional",
            TypeTag::Result => "Result",
            TypeTag::Range => "Range",
            TypeTag::Iterator => "Iterator",
            TypeTag::StringBuilder => "StringBuilder",
            TypeTag::Future => "Future",
            TypeTag::Duration => "Duration",
            TypeTag::Instant => "Instant",
            TypeTag::Channel => "Channel",
        }
    }
}

impl std::fmt::Display for TypeTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

// ─── CallbackInvoker ──────────────────────────────────────────────────────────

/// Trait for invoking Bock closures from within built-in functions.
///
/// Higher-order builtins (e.g., `List.map`, `Optional.flat_map`) receive a
/// `&mut dyn CallbackInvoker` so they can call user-supplied closures without
/// needing direct access to the interpreter.
///
/// Returns a boxed future so the trait stays object-safe while still letting
/// the interpreter drive recursive async evaluation.
pub trait CallbackInvoker: Send {
    /// Invoke `callable` (a `Value::Function`) with the given arguments.
    fn invoke<'a>(
        &'a mut self,
        callable: &'a Value,
        args: &'a [Value],
    ) -> BoxFuture<'a, Result<Value, RuntimeError>>;
}

/// A no-op invoker that always returns an error.
///
/// Used in tests and contexts where callback invocation is not available.
pub struct NoOpInvoker;

impl CallbackInvoker for NoOpInvoker {
    fn invoke<'a>(
        &'a mut self,
        _callable: &'a Value,
        _args: &'a [Value],
    ) -> BoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async {
            Err(RuntimeError::TypeError(
                "callback invocation not available in this context".to_string(),
            ))
        })
    }
}

// ─── BuiltinFn ────────────────────────────────────────────────────────────────

/// Signature for a built-in method.
///
/// The first element of `args` is the receiver (the value the method is called on).
/// Remaining elements are the method arguments.
pub type BuiltinFn = fn(&[Value]) -> Result<Value, RuntimeError>;

/// Signature for a higher-order built-in method that needs to invoke callbacks.
///
/// Like [`BuiltinFn`], the first element of `args` is the receiver.
/// The `invoker` parameter allows calling Bock closures passed as arguments.
///
/// Returns a boxed future so the builtin can `.await` callback invocations.
pub type HigherOrderBuiltinFn = for<'a> fn(
    &'a [Value],
    &'a mut dyn CallbackInvoker,
) -> BoxFuture<'a, Result<Value, RuntimeError>>;

// ─── BuiltinRegistry ─────────────────────────────────────────────────────────

/// A dispatch table mapping `(TypeTag, method_name)` pairs to built-in
/// implementations.
///
/// Also supports global built-in functions (e.g., `print`, `println`, `debug`).
/// Higher-order methods (those needing callback invocation) are stored separately.
#[derive(Clone)]
pub struct BuiltinRegistry {
    /// Method dispatch: `(TypeTag, name) → fn`.
    methods: HashMap<(TypeTag, String), BuiltinFn>,
    /// Higher-order method dispatch: `(TypeTag, name) → fn` (needs callback invoker).
    ho_methods: HashMap<(TypeTag, String), HigherOrderBuiltinFn>,
    /// Global built-in functions: `name → fn`.
    globals: HashMap<String, BuiltinFn>,
}

impl Default for BuiltinRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BuiltinRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            methods: HashMap::new(),
            ho_methods: HashMap::new(),
            globals: HashMap::new(),
        }
    }

    /// Register a method for a specific type.
    pub fn register(&mut self, type_tag: TypeTag, name: &str, func: BuiltinFn) {
        self.methods.insert((type_tag, name.to_string()), func);
    }

    /// Register a higher-order method that needs callback invocation.
    pub fn register_ho(&mut self, type_tag: TypeTag, name: &str, func: HigherOrderBuiltinFn) {
        self.ho_methods.insert((type_tag, name.to_string()), func);
    }

    /// Register a global built-in function.
    pub fn register_global(&mut self, name: &str, func: BuiltinFn) {
        self.globals.insert(name.to_string(), func);
    }

    /// Look up and call a method on a value.
    ///
    /// `receiver` is the value the method is called on, `args` are the
    /// additional arguments. Returns `None` if no method is registered,
    /// allowing the interpreter to fall back to other dispatch mechanisms.
    ///
    /// Note: this does NOT check higher-order methods. Use `get_ho_method`
    /// for those, since they require a [`CallbackInvoker`].
    pub fn call(
        &self,
        type_tag: TypeTag,
        name: &str,
        args: &[Value],
    ) -> Option<Result<Value, RuntimeError>> {
        self.methods
            .get(&(type_tag, name.to_string()))
            .map(|func| func(args))
    }

    /// Look up and call a global built-in function.
    ///
    /// Returns `None` if no global with that name is registered.
    pub fn call_global(&self, name: &str, args: &[Value]) -> Option<Result<Value, RuntimeError>> {
        self.globals.get(name).map(|func| func(args))
    }

    /// Look up a higher-order method. Returns the function pointer if found.
    ///
    /// The caller is responsible for invoking the returned function with
    /// a `&mut dyn CallbackInvoker`.
    #[must_use]
    pub fn get_ho_method(&self, type_tag: TypeTag, name: &str) -> Option<HigherOrderBuiltinFn> {
        self.ho_methods.get(&(type_tag, name.to_string())).copied()
    }

    /// Check whether a method is registered for the given type (simple or higher-order).
    #[must_use]
    pub fn has_method(&self, type_tag: TypeTag, name: &str) -> bool {
        let key = (type_tag, name.to_string());
        self.methods.contains_key(&key) || self.ho_methods.contains_key(&key)
    }

    /// Check whether a global function is registered.
    #[must_use]
    pub fn has_global(&self, name: &str) -> bool {
        self.globals.contains_key(name)
    }

    /// Iterate all registered `(receiver type, method name)` pairs for both
    /// ordinary and higher-order methods. Intended for introspection tools
    /// (vocab emitter, documentation generators).
    pub fn method_keys(&self) -> impl Iterator<Item = (TypeTag, &str)> {
        self.methods
            .keys()
            .chain(self.ho_methods.keys())
            .map(|(t, n)| (*t, n.as_str()))
    }

    /// Iterate all registered global function names.
    pub fn global_names(&self) -> impl Iterator<Item = &str> {
        self.globals.keys().map(String::as_str)
    }

    /// Register test assertion built-ins (`expect` global and assertion methods).
    ///
    /// Call this when running in test mode so that `expect(x).to_equal(y)` and
    /// related assertion methods are available.
    pub fn register_test_builtins(&mut self) {
        self.register_global("expect", builtin_expect);
        self.register(TypeTag::Record, "to_equal", expect_to_equal);
        self.register(TypeTag::Record, "to_be_ok", expect_to_be_ok);
        self.register(TypeTag::Record, "to_be_err", expect_to_be_err);
        self.register(TypeTag::Record, "to_be_some", expect_to_be_some);
        self.register(TypeTag::Record, "to_be_none", expect_to_be_none);
        self.register(TypeTag::Record, "to_throw", expect_to_throw);
        self.register(TypeTag::Record, "to_be_true", expect_to_be_true);
        self.register(TypeTag::Record, "to_be_false", expect_to_be_false);
    }

    /// Register the minimal bootstrap set of built-in methods and globals.
    ///
    /// This provides just enough to make the interpreter functional:
    /// - Globals: `print`, `println`, `debug`
    /// - String: `len`, `to_string`
    /// - List: `len`, `get`, `push`
    /// - Map: `get`, `set`, `len`
    pub fn register_defaults(&mut self) {
        // ── Global functions ──────────────────────────────────────────────
        self.register_global("print", builtin_print);
        self.register_global("println", builtin_println);
        self.register_global("debug", builtin_debug);
        self.register_global("assert", builtin_assert);
        self.register_global("todo", builtin_todo);
        self.register_global("unreachable", builtin_unreachable);

        // ── String methods ────────────────────────────────────────────────
        self.register(TypeTag::String, "len", string_len);
        self.register(TypeTag::String, "to_string", string_to_string);

        // ── List methods ──────────────────────────────────────────────────
        self.register(TypeTag::List, "len", list_len);
        self.register(TypeTag::List, "get", list_get);
        self.register(TypeTag::List, "push", list_push);

        // ── Map methods ───────────────────────────────────────────────────
        self.register(TypeTag::Map, "len", map_len);
        self.register(TypeTag::Map, "get", map_get);
        self.register(TypeTag::Map, "set", map_set);

        // ── Primitive conversion methods ─────────────────────────────────
        self.register(TypeTag::Int, "to_float", int_to_float);
        self.register(TypeTag::Float, "to_int", float_to_int);
        self.register(TypeTag::Bool, "to_int", bool_to_int);
        self.register(TypeTag::Char, "to_int", char_to_int);

        // ── Universal ─────────────────────────────────────────────────────
        // to_string for all types (registered per type for non-String)
        self.register(TypeTag::Int, "to_string", universal_to_string);
        self.register(TypeTag::Float, "to_string", universal_to_string);
        self.register(TypeTag::Bool, "to_string", universal_to_string);
        self.register(TypeTag::Char, "to_string", universal_to_string);
        self.register(TypeTag::Void, "to_string", universal_to_string);
        self.register(TypeTag::List, "to_string", universal_to_string);
        self.register(TypeTag::Map, "to_string", universal_to_string);
        self.register(TypeTag::Set, "to_string", universal_to_string);
        self.register(TypeTag::Tuple, "to_string", universal_to_string);
        self.register(TypeTag::Record, "to_string", universal_to_string);
        self.register(TypeTag::Enum, "to_string", universal_to_string);
        self.register(TypeTag::Function, "to_string", universal_to_string);
        self.register(TypeTag::Optional, "to_string", universal_to_string);
        self.register(TypeTag::Result, "to_string", universal_to_string);
        self.register(TypeTag::Range, "to_string", universal_to_string);
        self.register(TypeTag::Iterator, "to_string", universal_to_string);
        self.register(TypeTag::StringBuilder, "to_string", universal_to_string);
        self.register(TypeTag::Duration, "to_string", universal_to_string);
        self.register(TypeTag::Instant, "to_string", universal_to_string);
        self.register(TypeTag::Channel, "to_string", universal_to_string);
    }
}

// ─── Global built-in functions ────────────────────────────────────────────────

/// `print(args...)` — print values separated by spaces, no trailing newline.
fn builtin_print(args: &[Value]) -> Result<Value, RuntimeError> {
    let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
    print!("{}", parts.join(" "));
    Ok(Value::Void)
}

/// `println(args...)` — print values separated by spaces, with trailing newline.
fn builtin_println(args: &[Value]) -> Result<Value, RuntimeError> {
    let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
    println!("{}", parts.join(" "));
    Ok(Value::Void)
}

/// `debug(value)` — print a debug representation of a value.
fn builtin_debug(args: &[Value]) -> Result<Value, RuntimeError> {
    for arg in args {
        println!("{arg:?}");
    }
    Ok(Value::Void)
}

/// `assert(condition, message?)` — panic if condition is false.
fn builtin_assert(args: &[Value]) -> Result<Value, RuntimeError> {
    let condition = match args.first() {
        Some(Value::Bool(b)) => *b,
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "assert expects Bool, got {other}"
            )))
        }
        None => {
            return Err(RuntimeError::ArityMismatch {
                expected: 1,
                got: 0,
            })
        }
    };
    if condition {
        Ok(Value::Void)
    } else {
        let msg = match args.get(1) {
            Some(Value::String(s)) => format!("assertion failed: {}", s.as_str()),
            Some(other) => format!("assertion failed: {other}"),
            None => "assertion failed".to_string(),
        };
        Err(RuntimeError::AssertionFailed(msg))
    }
}

/// `todo(message?)` — always panic with "not yet implemented".
fn builtin_todo(args: &[Value]) -> Result<Value, RuntimeError> {
    let msg = match args.first() {
        Some(Value::String(s)) => format!("not yet implemented: {}", s.as_str()),
        Some(other) => format!("not yet implemented: {other}"),
        None => "not yet implemented".to_string(),
    };
    Err(RuntimeError::NotImplemented(msg))
}

/// `unreachable(message?)` — always panic with "entered unreachable code".
fn builtin_unreachable(_args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::Unreachable)
}

// ─── Universal methods ────────────────────────────────────────────────────────

/// `value.to_string()` — convert any value to its string representation.
fn universal_to_string(args: &[Value]) -> Result<Value, RuntimeError> {
    let receiver = args
        .first()
        .ok_or_else(|| RuntimeError::TypeError("to_string requires a receiver".to_string()))?;
    Ok(Value::String(BockString::new(receiver.to_string())))
}

// ─── Primitive conversion methods ────────────────────────────────────────────

/// `int.to_float()` — convert Int to Float.
fn int_to_float(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::Int(n)) => Ok(Value::Float(OrdF64(*n as f64))),
        _ => Err(RuntimeError::TypeError(
            "Int.to_float called on non-Int".to_string(),
        )),
    }
}

/// `float.to_int()` — truncate Float to Int.
fn float_to_int(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::Float(f)) => {
            if f.0.is_nan() || f.0.is_infinite() {
                Err(RuntimeError::TypeError(
                    "cannot convert NaN or Infinity to Int".to_string(),
                ))
            } else {
                Ok(Value::Int(f.0 as i64))
            }
        }
        _ => Err(RuntimeError::TypeError(
            "Float.to_int called on non-Float".to_string(),
        )),
    }
}

/// `bool.to_int()` — convert Bool to Int (true=1, false=0).
fn bool_to_int(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::Bool(b)) => Ok(Value::Int(if *b { 1 } else { 0 })),
        _ => Err(RuntimeError::TypeError(
            "Bool.to_int called on non-Bool".to_string(),
        )),
    }
}

/// `char.to_int()` — convert Char to its Unicode codepoint.
fn char_to_int(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::Char(c)) => Ok(Value::Int(*c as i64)),
        _ => Err(RuntimeError::TypeError(
            "Char.to_int called on non-Char".to_string(),
        )),
    }
}

// ─── String methods ───────────────────────────────────────────────────────────

/// `string.len()` — number of characters (not bytes).
fn string_len(args: &[Value]) -> Result<Value, RuntimeError> {
    let receiver = args
        .first()
        .ok_or_else(|| RuntimeError::TypeError("String.len requires a receiver".to_string()))?;
    match receiver {
        Value::String(s) => Ok(Value::Int(s.as_str().chars().count() as i64)),
        _ => Err(RuntimeError::TypeError(
            "String.len called on non-String".to_string(),
        )),
    }
}

/// `string.to_string()` — identity for strings.
fn string_to_string(args: &[Value]) -> Result<Value, RuntimeError> {
    let receiver = args.first().ok_or_else(|| {
        RuntimeError::TypeError("String.to_string requires a receiver".to_string())
    })?;
    Ok(receiver.clone())
}

// ─── List methods ─────────────────────────────────────────────────────────────

/// `list.len()` — number of elements.
fn list_len(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::List(items)) => Ok(Value::Int(items.len() as i64)),
        _ => Err(RuntimeError::TypeError(
            "List.len called on non-List".to_string(),
        )),
    }
}

/// `list.get(index)` — get element at index, returning `Optional`.
fn list_get(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = match args.first() {
        Some(Value::List(items)) => items,
        _ => {
            return Err(RuntimeError::TypeError(
                "List.get called on non-List".to_string(),
            ))
        }
    };
    let idx = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 1,
        got: 0,
    })?;
    match idx {
        Value::Int(i) => {
            let i = *i;
            if i < 0 || i as usize >= items.len() {
                Ok(Value::Optional(None))
            } else {
                Ok(Value::Optional(Some(Box::new(items[i as usize].clone()))))
            }
        }
        _ => Err(RuntimeError::TypeError(
            "List.get expects an Int index".to_string(),
        )),
    }
}

/// `list.push(value)` — return a new list with the value appended.
fn list_push(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = match args.first() {
        Some(Value::List(items)) => items,
        _ => {
            return Err(RuntimeError::TypeError(
                "List.push called on non-List".to_string(),
            ))
        }
    };
    let val = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 1,
        got: 0,
    })?;
    let mut new_list = items.clone();
    new_list.push(val.clone());
    Ok(Value::List(new_list))
}

// ─── Map methods ──────────────────────────────────────────────────────────────

/// `map.len()` — number of entries.
fn map_len(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::Map(map)) => Ok(Value::Int(map.len() as i64)),
        _ => Err(RuntimeError::TypeError(
            "Map.len called on non-Map".to_string(),
        )),
    }
}

/// `map.get(key)` — look up a key, returning `Optional`.
fn map_get(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = match args.first() {
        Some(Value::Map(map)) => map,
        _ => {
            return Err(RuntimeError::TypeError(
                "Map.get called on non-Map".to_string(),
            ))
        }
    };
    let key = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 1,
        got: 0,
    })?;
    Ok(Value::Optional(map.get(key).cloned().map(Box::new)))
}

/// `map.set(key, value)` — return a new map with the key-value pair added/updated.
fn map_set(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = match args.first() {
        Some(Value::Map(map)) => map,
        _ => {
            return Err(RuntimeError::TypeError(
                "Map.set called on non-Map".to_string(),
            ))
        }
    };
    if args.len() < 3 {
        return Err(RuntimeError::ArityMismatch {
            expected: 2,
            got: args.len() - 1,
        });
    }
    let key = args[1].clone();
    let val = args[2].clone();
    let mut new_map = map.clone();
    new_map.insert(key, val);
    Ok(Value::Map(new_map))
}

// ─── Test assertion built-ins ─────────────────────────────────────────────

use crate::value::RecordValue;
use std::collections::BTreeMap;

/// `expect(value)` — create an `Expectation` record wrapping the given value.
fn builtin_expect(args: &[Value]) -> Result<Value, RuntimeError> {
    let actual = args.first().cloned().unwrap_or(Value::Void);
    let mut fields = BTreeMap::new();
    fields.insert("actual".to_string(), actual);
    Ok(Value::Record(RecordValue {
        type_name: "Expectation".to_string(),
        fields,
    }))
}

/// Extract the `actual` value from an Expectation record.
fn get_expectation_actual(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::Record(r)) if r.type_name == "Expectation" => r
            .fields
            .get("actual")
            .cloned()
            .ok_or_else(|| RuntimeError::TypeError("malformed Expectation".to_string())),
        _ => Err(RuntimeError::TypeError(
            "assertion method called on non-Expectation value".to_string(),
        )),
    }
}

/// `expect(x).to_equal(y)` — assert `x == y`.
fn expect_to_equal(args: &[Value]) -> Result<Value, RuntimeError> {
    let actual = get_expectation_actual(args)?;
    let expected = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 1,
        got: 0,
    })?;
    if actual != *expected {
        return Err(RuntimeError::AssertionFailed(format!(
            "expected {expected}, got {actual}"
        )));
    }
    Ok(Value::Void)
}

/// `expect(x).to_be_ok()` — assert `x` is `Ok(...)`.
fn expect_to_be_ok(args: &[Value]) -> Result<Value, RuntimeError> {
    let actual = get_expectation_actual(args)?;
    match &actual {
        Value::Result(Ok(_)) => Ok(Value::Void),
        _ => Err(RuntimeError::AssertionFailed(format!(
            "expected Ok(...), got {actual}"
        ))),
    }
}

/// `expect(x).to_be_err()` — assert `x` is `Err(...)`.
fn expect_to_be_err(args: &[Value]) -> Result<Value, RuntimeError> {
    let actual = get_expectation_actual(args)?;
    match &actual {
        Value::Result(Err(_)) => Ok(Value::Void),
        _ => Err(RuntimeError::AssertionFailed(format!(
            "expected Err(...), got {actual}"
        ))),
    }
}

/// `expect(x).to_be_some()` — assert `x` is `Some(...)`.
fn expect_to_be_some(args: &[Value]) -> Result<Value, RuntimeError> {
    let actual = get_expectation_actual(args)?;
    match &actual {
        Value::Optional(Some(_)) => Ok(Value::Void),
        _ => Err(RuntimeError::AssertionFailed(format!(
            "expected Some(...), got {actual}"
        ))),
    }
}

/// `expect(x).to_be_none()` — assert `x` is `None`.
fn expect_to_be_none(args: &[Value]) -> Result<Value, RuntimeError> {
    let actual = get_expectation_actual(args)?;
    match &actual {
        Value::Optional(None) => Ok(Value::Void),
        _ => Err(RuntimeError::AssertionFailed(format!(
            "expected None, got {actual}"
        ))),
    }
}

/// `expect(x).to_throw()` — assert `x` is an error value (Err variant).
///
/// Alias-like behavior for `to_be_err` but semantically about "throwing".
fn expect_to_throw(args: &[Value]) -> Result<Value, RuntimeError> {
    expect_to_be_err(args)
}

/// `expect(x).to_be_true()` — assert `x` is `true`.
fn expect_to_be_true(args: &[Value]) -> Result<Value, RuntimeError> {
    let actual = get_expectation_actual(args)?;
    if actual != Value::Bool(true) {
        return Err(RuntimeError::AssertionFailed(format!(
            "expected true, got {actual}"
        )));
    }
    Ok(Value::Void)
}

/// `expect(x).to_be_false()` — assert `x` is `false`.
fn expect_to_be_false(args: &[Value]) -> Result<Value, RuntimeError> {
    let actual = get_expectation_actual(args)?;
    if actual != Value::Bool(false) {
        return Err(RuntimeError::AssertionFailed(format!(
            "expected false, got {actual}"
        )));
    }
    Ok(Value::Void)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn make_registry() -> BuiltinRegistry {
        let mut reg = BuiltinRegistry::new();
        reg.register_defaults();
        reg
    }

    // ── TypeTag ──────────────────────────────────────────────────────────

    #[test]
    fn type_tag_of_all_variants() {
        assert_eq!(TypeTag::of(&Value::Int(0)), TypeTag::Int);
        assert_eq!(TypeTag::of(&Value::Float(0.0.into())), TypeTag::Float);
        assert_eq!(TypeTag::of(&Value::Bool(true)), TypeTag::Bool);
        assert_eq!(
            TypeTag::of(&Value::String(BockString::new("x"))),
            TypeTag::String
        );
        assert_eq!(TypeTag::of(&Value::Char('x')), TypeTag::Char);
        assert_eq!(TypeTag::of(&Value::Void), TypeTag::Void);
        assert_eq!(TypeTag::of(&Value::List(vec![])), TypeTag::List);
        assert_eq!(TypeTag::of(&Value::Map(BTreeMap::new())), TypeTag::Map);
    }

    #[test]
    fn type_tag_display() {
        assert_eq!(TypeTag::Int.to_string(), "Int");
        assert_eq!(TypeTag::String.to_string(), "String");
        assert_eq!(TypeTag::List.to_string(), "List");
    }

    // ── Global functions ─────────────────────────────────────────────────

    #[test]
    fn println_returns_void() {
        let reg = make_registry();
        let result = reg
            .call_global("println", &[Value::String(BockString::new("hello"))])
            .unwrap();
        assert_eq!(result.unwrap(), Value::Void);
    }

    #[test]
    fn print_returns_void() {
        let reg = make_registry();
        let result = reg.call_global("print", &[Value::Int(42)]).unwrap();
        assert_eq!(result.unwrap(), Value::Void);
    }

    #[test]
    fn debug_returns_void() {
        let reg = make_registry();
        let result = reg.call_global("debug", &[Value::Bool(true)]).unwrap();
        assert_eq!(result.unwrap(), Value::Void);
    }

    #[test]
    fn unknown_global_returns_none() {
        let reg = make_registry();
        assert!(reg.call_global("nonexistent", &[]).is_none());
    }

    // ── String methods ───────────────────────────────────────────────────

    #[test]
    fn string_len_counts_chars() {
        let reg = make_registry();
        let recv = Value::String(BockString::new("héllo"));
        let result = reg.call(TypeTag::String, "len", &[recv]).unwrap().unwrap();
        assert_eq!(result, Value::Int(5));
    }

    #[test]
    fn string_to_string_identity() {
        let reg = make_registry();
        let recv = Value::String(BockString::new("test"));
        let result = reg
            .call(TypeTag::String, "to_string", std::slice::from_ref(&recv))
            .unwrap()
            .unwrap();
        assert_eq!(result, recv);
    }

    // ── List methods ─────────────────────────────────────────────────────

    #[test]
    fn list_len_works() {
        let reg = make_registry();
        let recv = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let result = reg.call(TypeTag::List, "len", &[recv]).unwrap().unwrap();
        assert_eq!(result, Value::Int(3));
    }

    #[test]
    fn list_get_valid_index() {
        let reg = make_registry();
        let recv = Value::List(vec![Value::Int(10), Value::Int(20)]);
        let result = reg
            .call(TypeTag::List, "get", &[recv, Value::Int(1)])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Optional(Some(Box::new(Value::Int(20)))));
    }

    #[test]
    fn list_get_out_of_bounds() {
        let reg = make_registry();
        let recv = Value::List(vec![Value::Int(10)]);
        let result = reg
            .call(TypeTag::List, "get", &[recv, Value::Int(5)])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Optional(None));
    }

    #[test]
    fn list_push_appends() {
        let reg = make_registry();
        let recv = Value::List(vec![Value::Int(1)]);
        let result = reg
            .call(TypeTag::List, "push", &[recv, Value::Int(2)])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::List(vec![Value::Int(1), Value::Int(2)]));
    }

    // ── Map methods ──────────────────────────────────────────────────────

    #[test]
    fn map_len_works() {
        let reg = make_registry();
        let mut m = BTreeMap::new();
        m.insert(Value::Int(1), Value::Bool(true));
        let recv = Value::Map(m);
        let result = reg.call(TypeTag::Map, "len", &[recv]).unwrap().unwrap();
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn map_get_existing_key() {
        let reg = make_registry();
        let mut m = BTreeMap::new();
        m.insert(Value::String(BockString::new("a")), Value::Int(42));
        let recv = Value::Map(m);
        let result = reg
            .call(
                TypeTag::Map,
                "get",
                &[recv, Value::String(BockString::new("a"))],
            )
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Optional(Some(Box::new(Value::Int(42)))));
    }

    #[test]
    fn map_get_missing_key() {
        let reg = make_registry();
        let recv = Value::Map(BTreeMap::new());
        let result = reg
            .call(
                TypeTag::Map,
                "get",
                &[recv, Value::String(BockString::new("missing"))],
            )
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Optional(None));
    }

    #[test]
    fn map_set_inserts() {
        let reg = make_registry();
        let recv = Value::Map(BTreeMap::new());
        let result = reg
            .call(
                TypeTag::Map,
                "set",
                &[recv, Value::Int(1), Value::Bool(true)],
            )
            .unwrap()
            .unwrap();
        let mut expected = BTreeMap::new();
        expected.insert(Value::Int(1), Value::Bool(true));
        assert_eq!(result, Value::Map(expected));
    }

    // ── Unknown method produces clear error ──────────────────────────────

    #[test]
    fn unknown_method_returns_none() {
        let reg = make_registry();
        let recv = Value::Int(42);
        assert!(reg.call(TypeTag::Int, "nonexistent", &[recv]).is_none());
    }

    // ── Registration API extensibility ───────────────────────────────────

    #[test]
    fn external_registration_works() {
        let mut reg = make_registry();
        fn custom_method(args: &[Value]) -> Result<Value, RuntimeError> {
            Ok(args.first().cloned().unwrap_or(Value::Void))
        }
        reg.register(TypeTag::Int, "custom", custom_method);
        let result = reg
            .call(TypeTag::Int, "custom", &[Value::Int(99)])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Int(99));
    }

    // ── Universal to_string ──────────────────────────────────────────────

    #[test]
    fn int_to_string() {
        let reg = make_registry();
        let result = reg
            .call(TypeTag::Int, "to_string", &[Value::Int(42)])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::String(BockString::new("42")));
    }

    #[test]
    fn bool_to_string() {
        let reg = make_registry();
        let result = reg
            .call(TypeTag::Bool, "to_string", &[Value::Bool(true)])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::String(BockString::new("true")));
    }

    // ── assert / todo / unreachable ─────────────────────────────────────

    #[test]
    fn assert_true_passes() {
        let reg = make_registry();
        let result = reg.call_global("assert", &[Value::Bool(true)]).unwrap();
        assert_eq!(result.unwrap(), Value::Void);
    }

    #[test]
    fn assert_false_errors() {
        let reg = make_registry();
        let result = reg.call_global("assert", &[Value::Bool(false)]).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn assert_false_with_message() {
        let reg = make_registry();
        let result = reg
            .call_global(
                "assert",
                &[Value::Bool(false), Value::String(BockString::new("bad"))],
            )
            .unwrap();
        match result {
            Err(RuntimeError::AssertionFailed(msg)) => assert!(msg.contains("bad")),
            other => panic!("expected AssertionFailed, got {other:?}"),
        }
    }

    #[test]
    fn todo_produces_runtime_error() {
        let reg = make_registry();
        let result = reg.call_global("todo", &[]).unwrap();
        match result {
            Err(RuntimeError::NotImplemented(msg)) => {
                assert!(msg.contains("not yet implemented"));
            }
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }

    #[test]
    fn unreachable_produces_runtime_error() {
        let reg = make_registry();
        let result = reg.call_global("unreachable", &[]).unwrap();
        assert!(matches!(result, Err(RuntimeError::Unreachable)));
    }
}
