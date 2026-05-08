//! Map collection type methods and trait implementations.

use std::collections::BTreeMap;

use bock_interp::{BockString, BuiltinRegistry, CallbackInvoker, RuntimeError, TypeTag, Value};
use futures::future::BoxFuture;

/// Register all Map methods and trait implementations.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Core access ──────────────────────────────────────────────────────
    registry.register(TypeTag::Map, "len", map_len);
    registry.register(TypeTag::Map, "get", map_get);
    registry.register(TypeTag::Map, "contains_key", map_contains_key);
    registry.register(TypeTag::Map, "is_empty", map_is_empty);

    // ── Mutation (immutable — return new maps) ───────────────────────────
    registry.register(TypeTag::Map, "set", map_set);
    registry.register(TypeTag::Map, "delete", map_delete);
    registry.register(TypeTag::Map, "merge", map_merge);

    // ── Iteration / views ────────────────────────────────────────────────
    registry.register(TypeTag::Map, "keys", map_keys);
    registry.register(TypeTag::Map, "values", map_values);
    registry.register(TypeTag::Map, "entries", map_entries);

    // ── Higher-order methods ────────────────────────────────────────────
    registry.register_ho(TypeTag::Map, "map_values", map_map_values);
    registry.register_ho(TypeTag::Map, "filter", map_filter);
    registry.register_ho(TypeTag::Map, "for_each", map_for_each);

    // ── Conversion ───────────────────────────────────────────────────────
    registry.register(TypeTag::Map, "to_list", map_to_list);

    // ── Trait implementations ────────────────────────────────────────────
    registry.register(TypeTag::Map, "equals", map_equals);
    registry.register(TypeTag::Map, "display", map_display);
    registry.register(TypeTag::Map, "hash_code", map_hash_code);
    registry.register(TypeTag::Map, "compare", map_compare);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_map<'a>(
    args: &'a [Value],
    pos: usize,
    method: &str,
) -> Result<&'a BTreeMap<Value, Value>, RuntimeError> {
    match args.get(pos) {
        Some(Value::Map(m)) => Ok(m),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Map.{method} expects Map, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// ─── Core access ──────────────────────────────────────────────────────────────

fn map_len(args: &[Value]) -> Result<Value, RuntimeError> {
    let m = expect_map(args, 0, "len")?;
    Ok(Value::Int(m.len() as i64))
}

fn map_get(args: &[Value]) -> Result<Value, RuntimeError> {
    let m = expect_map(args, 0, "get")?;
    let key = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    Ok(Value::Optional(m.get(key).cloned().map(Box::new)))
}

fn map_contains_key(args: &[Value]) -> Result<Value, RuntimeError> {
    let m = expect_map(args, 0, "contains_key")?;
    let key = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    Ok(Value::Bool(m.contains_key(key)))
}

fn map_is_empty(args: &[Value]) -> Result<Value, RuntimeError> {
    let m = expect_map(args, 0, "is_empty")?;
    Ok(Value::Bool(m.is_empty()))
}

// ─── Mutation (immutable) ─────────────────────────────────────────────────────

fn map_set(args: &[Value]) -> Result<Value, RuntimeError> {
    let m = expect_map(args, 0, "set")?;
    if args.len() < 3 {
        return Err(RuntimeError::ArityMismatch {
            expected: 3,
            got: args.len(),
        });
    }
    let key = args[1].clone();
    let val = args[2].clone();
    let mut new_map = m.clone();
    new_map.insert(key, val);
    Ok(Value::Map(new_map))
}

fn map_delete(args: &[Value]) -> Result<Value, RuntimeError> {
    let m = expect_map(args, 0, "delete")?;
    let key = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    let mut new_map = m.clone();
    new_map.remove(key);
    Ok(Value::Map(new_map))
}

fn map_merge(args: &[Value]) -> Result<Value, RuntimeError> {
    let m = expect_map(args, 0, "merge")?;
    let other = expect_map(args, 1, "merge")?;
    let mut new_map = m.clone();
    for (k, v) in other {
        new_map.insert(k.clone(), v.clone());
    }
    Ok(Value::Map(new_map))
}

// ─── Iteration / views ───────────────────────────────────────────────────────

fn map_keys(args: &[Value]) -> Result<Value, RuntimeError> {
    let m = expect_map(args, 0, "keys")?;
    Ok(Value::List(m.keys().cloned().collect()))
}

fn map_values(args: &[Value]) -> Result<Value, RuntimeError> {
    let m = expect_map(args, 0, "values")?;
    Ok(Value::List(m.values().cloned().collect()))
}

fn map_entries(args: &[Value]) -> Result<Value, RuntimeError> {
    let m = expect_map(args, 0, "entries")?;
    let entries: Vec<Value> = m
        .iter()
        .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
        .collect();
    Ok(Value::List(entries))
}

// ─── Higher-order methods ─────────────────────────────────────────────────────

fn expect_fn<'a>(args: &'a [Value], pos: usize, method: &str) -> Result<&'a Value, RuntimeError> {
    match args.get(pos) {
        Some(v @ Value::Function(_)) => Ok(v),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Map.{method} expects Function, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

/// `map.map_values(fn)` — apply `fn` to each value, returning a new map.
fn map_map_values<'a>(
    args: &'a [Value],
    invoker: &'a mut dyn CallbackInvoker,
) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
        let m = expect_map(args, 0, "map_values")?;
        let f = expect_fn(args, 1, "map_values")?;
        let mut result = BTreeMap::new();
        for (k, v) in m {
            let new_v = invoker.invoke(f, std::slice::from_ref(v)).await?;
            result.insert(k.clone(), new_v);
        }
        Ok(Value::Map(result))
    })
}

/// `map.filter(fn)` — keep entries where `fn(key, value)` returns `true`.
fn map_filter<'a>(
    args: &'a [Value],
    invoker: &'a mut dyn CallbackInvoker,
) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
        let m = expect_map(args, 0, "filter")?;
        let f = expect_fn(args, 1, "filter")?;
        let mut result = BTreeMap::new();
        for (k, v) in m {
            if let Value::Bool(true) = invoker.invoke(f, &[k.clone(), v.clone()]).await? {
                result.insert(k.clone(), v.clone());
            }
        }
        Ok(Value::Map(result))
    })
}

/// `map.for_each(fn)` — call `fn(key, value)` for each entry, returns Void.
fn map_for_each<'a>(
    args: &'a [Value],
    invoker: &'a mut dyn CallbackInvoker,
) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
        let m = expect_map(args, 0, "for_each")?;
        let f = expect_fn(args, 1, "for_each")?;
        for (k, v) in m {
            invoker.invoke(f, &[k.clone(), v.clone()]).await?;
        }
        Ok(Value::Void)
    })
}

// ─── Conversion ───────────────────────────────────────────────────────────────

fn map_to_list(args: &[Value]) -> Result<Value, RuntimeError> {
    map_entries(args)
}

// ─── Trait implementations ────────────────────────────────────────────────────

fn map_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_map(args, 0, "equals")?;
    let b = expect_map(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

fn map_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let recv = args.first().ok_or(RuntimeError::ArityMismatch {
        expected: 1,
        got: 0,
    })?;
    Ok(Value::String(BockString::new(recv.to_string())))
}

fn map_hash_code(args: &[Value]) -> Result<Value, RuntimeError> {
    use std::hash::{Hash, Hasher};
    let m = expect_map(args, 0, "hash_code")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (k, v) in m {
        k.hash(&mut hasher);
        v.hash(&mut hasher);
    }
    Ok(Value::Int(hasher.finish() as i64))
}

fn map_compare(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_map(args, 0, "compare")?;
    let b = expect_map(args, 1, "compare")?;
    Ok(Value::Int(a.cmp(b) as i64))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> BuiltinRegistry {
        let mut r = BuiltinRegistry::new();
        register(&mut r);
        r
    }

    fn make_map(pairs: &[(i64, i64)]) -> Value {
        let mut m = BTreeMap::new();
        for &(k, v) in pairs {
            m.insert(Value::Int(k), Value::Int(v));
        }
        Value::Map(m)
    }

    // ── Core access ──────────────────────────────────────────────────────

    #[test]
    fn len() {
        let r = reg();
        let result = r.call(TypeTag::Map, "len", &[make_map(&[(1, 10), (2, 20)])]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(2));
    }

    #[test]
    fn get_existing() {
        let r = reg();
        let result = r.call(TypeTag::Map, "get", &[make_map(&[(1, 10)]), Value::Int(1)]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::Optional(Some(Box::new(Value::Int(10))))
        );
    }

    #[test]
    fn get_missing() {
        let r = reg();
        let result = r.call(TypeTag::Map, "get", &[make_map(&[(1, 10)]), Value::Int(99)]);
        assert_eq!(result.unwrap().unwrap(), Value::Optional(None));
    }

    #[test]
    fn contains_key_true() {
        let r = reg();
        let result = r.call(
            TypeTag::Map,
            "contains_key",
            &[make_map(&[(1, 10)]), Value::Int(1)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn contains_key_false() {
        let r = reg();
        let result = r.call(
            TypeTag::Map,
            "contains_key",
            &[make_map(&[(1, 10)]), Value::Int(99)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn is_empty_true() {
        let r = reg();
        let result = r.call(TypeTag::Map, "is_empty", &[make_map(&[])]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn is_empty_false() {
        let r = reg();
        let result = r.call(TypeTag::Map, "is_empty", &[make_map(&[(1, 10)])]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    // ── Mutation (immutable) ─────────────────────────────────────────────

    #[test]
    fn set_new_key() {
        let r = reg();
        let result = r.call(
            TypeTag::Map,
            "set",
            &[make_map(&[]), Value::Int(1), Value::Int(10)],
        );
        assert_eq!(result.unwrap().unwrap(), make_map(&[(1, 10)]));
    }

    #[test]
    fn set_overwrite() {
        let r = reg();
        let result = r.call(
            TypeTag::Map,
            "set",
            &[make_map(&[(1, 10)]), Value::Int(1), Value::Int(99)],
        );
        assert_eq!(result.unwrap().unwrap(), make_map(&[(1, 99)]));
    }

    #[test]
    fn delete_existing() {
        let r = reg();
        let result = r.call(
            TypeTag::Map,
            "delete",
            &[make_map(&[(1, 10), (2, 20)]), Value::Int(1)],
        );
        assert_eq!(result.unwrap().unwrap(), make_map(&[(2, 20)]));
    }

    #[test]
    fn delete_missing() {
        let r = reg();
        let m = make_map(&[(1, 10)]);
        let result = r.call(TypeTag::Map, "delete", &[m.clone(), Value::Int(99)]);
        assert_eq!(result.unwrap().unwrap(), m);
    }

    #[test]
    fn merge_maps() {
        let r = reg();
        let result = r.call(
            TypeTag::Map,
            "merge",
            &[make_map(&[(1, 10)]), make_map(&[(2, 20), (1, 99)])],
        );
        assert_eq!(result.unwrap().unwrap(), make_map(&[(1, 99), (2, 20)]));
    }

    // ── Iteration / views ────────────────────────────────────────────────

    #[test]
    fn keys() {
        let r = reg();
        let result = r.call(TypeTag::Map, "keys", &[make_map(&[(1, 10), (2, 20)])]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
    }

    #[test]
    fn values() {
        let r = reg();
        let result = r.call(TypeTag::Map, "values", &[make_map(&[(1, 10), (2, 20)])]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::List(vec![Value::Int(10), Value::Int(20)])
        );
    }

    #[test]
    fn entries() {
        let r = reg();
        let result = r.call(TypeTag::Map, "entries", &[make_map(&[(1, 10)])]);
        let expected = Value::List(vec![Value::Tuple(vec![Value::Int(1), Value::Int(10)])]);
        assert_eq!(result.unwrap().unwrap(), expected);
    }

    #[test]
    fn to_list_same_as_entries() {
        let r = reg();
        let m = make_map(&[(1, 10), (2, 20)]);
        let entries = r
            .call(TypeTag::Map, "entries", std::slice::from_ref(&m))
            .unwrap()
            .unwrap();
        let to_list = r.call(TypeTag::Map, "to_list", &[m]).unwrap().unwrap();
        assert_eq!(entries, to_list);
    }

    // ── Trait implementations ────────────────────────────────────────────

    #[test]
    fn equals_same() {
        let r = reg();
        let m = make_map(&[(1, 10), (2, 20)]);
        let result = r.call(TypeTag::Map, "equals", &[m.clone(), m]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn equals_different() {
        let r = reg();
        let result = r.call(
            TypeTag::Map,
            "equals",
            &[make_map(&[(1, 10)]), make_map(&[(1, 99)])],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn display_map() {
        let r = reg();
        let result = r.call(TypeTag::Map, "display", &[make_map(&[(1, 10)])]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::String(BockString::new("{1: 10}"))
        );
    }

    #[test]
    fn hash_code_deterministic() {
        let r = reg();
        let m = make_map(&[(1, 10)]);
        let h1 = r
            .call(TypeTag::Map, "hash_code", std::slice::from_ref(&m))
            .unwrap()
            .unwrap();
        let h2 = r.call(TypeTag::Map, "hash_code", &[m]).unwrap().unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn compare_maps() {
        let r = reg();
        let result = r.call(
            TypeTag::Map,
            "compare",
            &[make_map(&[(1, 10)]), make_map(&[(2, 20)])],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Int(-1));
    }
}
