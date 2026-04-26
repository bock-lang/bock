//! List collection type methods and trait implementations.

use std::collections::BTreeSet;

use futures::future::BoxFuture;
use bock_interp::{BockString, BuiltinRegistry, CallbackInvoker, RuntimeError, TypeTag, Value};

/// Register all List methods and trait implementations.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Core access ──────────────────────────────────────────────────────
    registry.register(TypeTag::List, "len", list_len);
    registry.register(TypeTag::List, "get", list_get);
    registry.register(TypeTag::List, "first", list_first);
    registry.register(TypeTag::List, "last", list_last);
    registry.register(TypeTag::List, "is_empty", list_is_empty);
    registry.register(TypeTag::List, "contains", list_contains);
    registry.register(TypeTag::List, "index_of", list_index_of);

    // ── Mutation (immutable — return new lists) ──────────────────────────
    registry.register(TypeTag::List, "push", list_push);
    registry.register(TypeTag::List, "pop", list_pop);
    registry.register(TypeTag::List, "insert", list_insert);
    registry.register(TypeTag::List, "remove", list_remove);
    registry.register(TypeTag::List, "concat", list_concat);
    registry.register(TypeTag::List, "slice", list_slice);
    registry.register(TypeTag::List, "reverse", list_reverse);
    registry.register(TypeTag::List, "sort", list_sort);
    registry.register(TypeTag::List, "dedup", list_dedup);
    registry.register(TypeTag::List, "flatten", list_flatten);
    registry.register(TypeTag::List, "zip", list_zip);

    // ── Iteration / higher-order ─────────────────────────────────────────
    registry.register_ho(TypeTag::List, "map", list_map);
    registry.register_ho(TypeTag::List, "filter", list_filter);
    registry.register_ho(TypeTag::List, "fold", list_fold);
    registry.register_ho(TypeTag::List, "reduce", list_reduce);
    registry.register_ho(TypeTag::List, "for_each", list_for_each);
    registry.register_ho(TypeTag::List, "any", list_any);
    registry.register_ho(TypeTag::List, "all", list_all);
    registry.register_ho(TypeTag::List, "find", list_find);
    registry.register_ho(TypeTag::List, "flat_map", list_flat_map);
    registry.register(TypeTag::List, "take", list_take);
    registry.register(TypeTag::List, "skip", list_skip);
    registry.register(TypeTag::List, "enumerate", list_enumerate);
    registry.register(TypeTag::List, "count", list_count);

    // ── Conversion ───────────────────────────────────────────────────────
    registry.register(TypeTag::List, "to_set", list_to_set);
    registry.register(TypeTag::List, "join", list_join);

    // ── Trait implementations ────────────────────────────────────────────
    registry.register(TypeTag::List, "equals", list_equals);
    registry.register(TypeTag::List, "display", list_display);
    registry.register(TypeTag::List, "hash_code", list_hash_code);
    registry.register(TypeTag::List, "compare", list_compare);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_list<'a>(
    args: &'a [Value],
    pos: usize,
    method: &str,
) -> Result<&'a [Value], RuntimeError> {
    match args.get(pos) {
        Some(Value::List(items)) => Ok(items.as_slice()),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "List.{method} expects List, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

fn expect_int(args: &[Value], pos: usize, method: &str) -> Result<i64, RuntimeError> {
    match args.get(pos) {
        Some(Value::Int(v)) => Ok(*v),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "List.{method} expects Int, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

fn expect_fn<'a>(args: &'a [Value], pos: usize, method: &str) -> Result<&'a Value, RuntimeError> {
    match args.get(pos) {
        Some(v @ Value::Function(_)) => Ok(v),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "List.{method} expects Function, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// Higher-order methods receive a CallbackInvoker to invoke Bock closures.

// ─── Core access ──────────────────────────────────────────────────────────────

fn list_len(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "len")?;
    Ok(Value::Int(items.len() as i64))
}

fn list_get(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "get")?;
    let idx = expect_int(args, 1, "get")?;
    if idx < 0 || idx as usize >= items.len() {
        Ok(Value::Optional(None))
    } else {
        Ok(Value::Optional(Some(Box::new(items[idx as usize].clone()))))
    }
}

fn list_first(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "first")?;
    Ok(Value::Optional(items.first().cloned().map(Box::new)))
}

fn list_last(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "last")?;
    Ok(Value::Optional(items.last().cloned().map(Box::new)))
}

fn list_is_empty(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "is_empty")?;
    Ok(Value::Bool(items.is_empty()))
}

fn list_contains(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "contains")?;
    let needle = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    Ok(Value::Bool(items.contains(needle)))
}

fn list_index_of(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "index_of")?;
    let needle = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    match items.iter().position(|v| v == needle) {
        Some(pos) => Ok(Value::Optional(Some(Box::new(Value::Int(pos as i64))))),
        None => Ok(Value::Optional(None)),
    }
}

// ─── Mutation (immutable) ─────────────────────────────────────────────────────

fn list_push(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "push")?;
    let val = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    let mut new_list = items.to_vec();
    new_list.push(val.clone());
    Ok(Value::List(new_list))
}

fn list_pop(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "pop")?;
    if items.is_empty() {
        Ok(Value::List(vec![]))
    } else {
        Ok(Value::List(items[..items.len() - 1].to_vec()))
    }
}

fn list_insert(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "insert")?;
    let idx = expect_int(args, 1, "insert")?;
    let val = args.get(2).ok_or(RuntimeError::ArityMismatch {
        expected: 3,
        got: args.len(),
    })?;
    let idx = idx as usize;
    if idx > items.len() {
        return Err(RuntimeError::IndexOutOfBounds {
            index: idx as i64,
            len: items.len(),
        });
    }
    let mut new_list = items.to_vec();
    new_list.insert(idx, val.clone());
    Ok(Value::List(new_list))
}

fn list_remove(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "remove")?;
    let idx = expect_int(args, 1, "remove")?;
    if idx < 0 || idx as usize >= items.len() {
        return Err(RuntimeError::IndexOutOfBounds {
            index: idx,
            len: items.len(),
        });
    }
    let mut new_list = items.to_vec();
    new_list.remove(idx as usize);
    Ok(Value::List(new_list))
}

fn list_concat(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "concat")?;
    let other = expect_list(args, 1, "concat")?;
    let mut new_list = items.to_vec();
    new_list.extend_from_slice(other);
    Ok(Value::List(new_list))
}

fn list_slice(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "slice")?;
    let start = expect_int(args, 1, "slice")?;
    let end = expect_int(args, 2, "slice")?;
    let len = items.len() as i64;
    let start = start.max(0).min(len) as usize;
    let end = end.max(0).min(len) as usize;
    if start >= end {
        Ok(Value::List(vec![]))
    } else {
        Ok(Value::List(items[start..end].to_vec()))
    }
}

fn list_reverse(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "reverse")?;
    let mut new_list = items.to_vec();
    new_list.reverse();
    Ok(Value::List(new_list))
}

fn list_sort(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "sort")?;
    let mut new_list = items.to_vec();
    new_list.sort();
    Ok(Value::List(new_list))
}

fn list_dedup(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "dedup")?;
    let mut new_list = items.to_vec();
    new_list.dedup();
    Ok(Value::List(new_list))
}

fn list_flatten(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "flatten")?;
    let mut result = Vec::new();
    for item in items {
        match item {
            Value::List(inner) => result.extend(inner.iter().cloned()),
            other => result.push(other.clone()),
        }
    }
    Ok(Value::List(result))
}

fn list_zip(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "zip")?;
    let other = expect_list(args, 1, "zip")?;
    let pairs: Vec<Value> = items
        .iter()
        .zip(other.iter())
        .map(|(a, b)| Value::Tuple(vec![a.clone(), b.clone()]))
        .collect();
    Ok(Value::List(pairs))
}

// ─── Higher-order methods ─────────────────────────────────────────────────────
// These validate arguments but require interpreter support for callback invocation.

fn list_map<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let items = expect_list(args, 0, "map")?;
    let f = expect_fn(args, 1, "map")?;
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        result.push(invoker.invoke(f, std::slice::from_ref(item)).await?);
    }
    Ok(Value::List(result))
})
}

fn list_filter<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let items = expect_list(args, 0, "filter")?;
    let f = expect_fn(args, 1, "filter")?;
    let mut result = Vec::new();
    for item in items {
        if let Value::Bool(true) = invoker.invoke(f, std::slice::from_ref(item)).await? {
            result.push(item.clone());
        }
    }
    Ok(Value::List(result))
})
}

fn list_fold<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let items = expect_list(args, 0, "fold")?;
    let init = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 3,
        got: args.len(),
    })?;
    let f = expect_fn(args, 2, "fold")?;
    let mut acc = init.clone();
    for item in items {
        acc = invoker.invoke(f, &[acc, item.clone()]).await?;
    }
    Ok(acc)
})
}

fn list_reduce<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let items = expect_list(args, 0, "reduce")?;
    let f = expect_fn(args, 1, "reduce")?;
    if items.is_empty() {
        return Err(RuntimeError::TypeError(
            "List.reduce called on empty list".to_string(),
        ));
    }
    let mut acc = items[0].clone();
    for item in &items[1..] {
        acc = invoker.invoke(f, &[acc, item.clone()]).await?;
    }
    Ok(acc)
})
}

fn list_for_each<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let items = expect_list(args, 0, "for_each")?;
    let f = expect_fn(args, 1, "for_each")?;
    for item in items {
        invoker.invoke(f, std::slice::from_ref(item)).await?;
    }
    Ok(Value::Void)
})
}

fn list_any<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let items = expect_list(args, 0, "any")?;
    let f = expect_fn(args, 1, "any")?;
    for item in items {
        if let Value::Bool(true) = invoker.invoke(f, std::slice::from_ref(item)).await? {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
})
}

fn list_all<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let items = expect_list(args, 0, "all")?;
    let f = expect_fn(args, 1, "all")?;
    for item in items {
        if let Value::Bool(false) = invoker.invoke(f, std::slice::from_ref(item)).await? {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
})
}

fn list_find<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let items = expect_list(args, 0, "find")?;
    let f = expect_fn(args, 1, "find")?;
    for item in items {
        if let Value::Bool(true) = invoker.invoke(f, std::slice::from_ref(item)).await? {
            return Ok(Value::Optional(Some(Box::new(item.clone()))));
        }
    }
    Ok(Value::Optional(None))
})
}

fn list_flat_map<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let items = expect_list(args, 0, "flat_map")?;
    let f = expect_fn(args, 1, "flat_map")?;
    let mut result = Vec::new();
    for item in items {
        let mapped = invoker.invoke(f, std::slice::from_ref(item)).await?;
        match mapped {
            Value::List(inner) => result.extend(inner),
            other => result.push(other),
        }
    }
    Ok(Value::List(result))
})
}

fn list_take(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "take")?;
    let n = expect_int(args, 1, "take")?;
    let n = (n.max(0) as usize).min(items.len());
    Ok(Value::List(items[..n].to_vec()))
}

fn list_skip(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "skip")?;
    let n = expect_int(args, 1, "skip")?;
    let n = (n.max(0) as usize).min(items.len());
    Ok(Value::List(items[n..].to_vec()))
}

fn list_enumerate(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "enumerate")?;
    let result: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, v)| Value::Tuple(vec![Value::Int(i as i64), v.clone()]))
        .collect();
    Ok(Value::List(result))
}

fn list_count(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "count")?;
    Ok(Value::Int(items.len() as i64))
}

// ─── Conversion ───────────────────────────────────────────────────────────────

fn list_to_set(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "to_set")?;
    let set: BTreeSet<Value> = items.iter().cloned().collect();
    Ok(Value::Set(set))
}

fn list_join(args: &[Value]) -> Result<Value, RuntimeError> {
    let items = expect_list(args, 0, "join")?;
    let sep = match args.get(1) {
        Some(Value::String(s)) => s.as_str().to_owned(),
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "List.join expects String separator, got {other}"
            )))
        }
        None => String::new(),
    };
    let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
    Ok(Value::String(BockString::new(parts.join(&sep))))
}

// ─── Trait implementations ────────────────────────────────────────────────────

fn list_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_list(args, 0, "equals")?;
    let b = expect_list(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

fn list_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let recv = args.first().ok_or(RuntimeError::ArityMismatch {
        expected: 1,
        got: 0,
    })?;
    Ok(Value::String(BockString::new(recv.to_string())))
}

fn list_hash_code(args: &[Value]) -> Result<Value, RuntimeError> {
    use std::hash::{Hash, Hasher};
    let items = expect_list(args, 0, "hash_code")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    items.hash(&mut hasher);
    Ok(Value::Int(hasher.finish() as i64))
}

fn list_compare(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_list(args, 0, "compare")?;
    let b = expect_list(args, 1, "compare")?;
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

    fn list(vals: &[i64]) -> Value {
        Value::List(vals.iter().map(|&v| Value::Int(v)).collect())
    }

    // ── Core access ──────────────────────────────────────────────────────

    #[test]
    fn len() {
        let r = reg();
        let result = r.call(TypeTag::List, "len", &[list(&[1, 2, 3])]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(3));
    }

    #[test]
    fn get_valid() {
        let r = reg();
        let result = r.call(TypeTag::List, "get", &[list(&[10, 20]), Value::Int(1)]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::Optional(Some(Box::new(Value::Int(20))))
        );
    }

    #[test]
    fn get_out_of_bounds() {
        let r = reg();
        let result = r.call(TypeTag::List, "get", &[list(&[10]), Value::Int(5)]);
        assert_eq!(result.unwrap().unwrap(), Value::Optional(None));
    }

    #[test]
    fn first_some() {
        let r = reg();
        let result = r.call(TypeTag::List, "first", &[list(&[1, 2, 3])]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::Optional(Some(Box::new(Value::Int(1))))
        );
    }

    #[test]
    fn first_none() {
        let r = reg();
        let result = r.call(TypeTag::List, "first", &[list(&[])]);
        assert_eq!(result.unwrap().unwrap(), Value::Optional(None));
    }

    #[test]
    fn last_some() {
        let r = reg();
        let result = r.call(TypeTag::List, "last", &[list(&[1, 2, 3])]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::Optional(Some(Box::new(Value::Int(3))))
        );
    }

    #[test]
    fn is_empty_true() {
        let r = reg();
        let result = r.call(TypeTag::List, "is_empty", &[list(&[])]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn is_empty_false() {
        let r = reg();
        let result = r.call(TypeTag::List, "is_empty", &[list(&[1])]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn contains_found() {
        let r = reg();
        let result = r.call(
            TypeTag::List,
            "contains",
            &[list(&[1, 2, 3]), Value::Int(2)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn contains_not_found() {
        let r = reg();
        let result = r.call(
            TypeTag::List,
            "contains",
            &[list(&[1, 2, 3]), Value::Int(5)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn index_of_found() {
        let r = reg();
        let result = r.call(
            TypeTag::List,
            "index_of",
            &[list(&[10, 20, 30]), Value::Int(20)],
        );
        assert_eq!(
            result.unwrap().unwrap(),
            Value::Optional(Some(Box::new(Value::Int(1))))
        );
    }

    #[test]
    fn index_of_not_found() {
        let r = reg();
        let result = r.call(
            TypeTag::List,
            "index_of",
            &[list(&[10, 20]), Value::Int(99)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Optional(None));
    }

    // ── Mutation (immutable) ─────────────────────────────────────────────

    #[test]
    fn push_appends() {
        let r = reg();
        let result = r.call(TypeTag::List, "push", &[list(&[1]), Value::Int(2)]);
        assert_eq!(result.unwrap().unwrap(), list(&[1, 2]));
    }

    #[test]
    fn pop_removes_last() {
        let r = reg();
        let result = r.call(TypeTag::List, "pop", &[list(&[1, 2, 3])]);
        assert_eq!(result.unwrap().unwrap(), list(&[1, 2]));
    }

    #[test]
    fn pop_empty() {
        let r = reg();
        let result = r.call(TypeTag::List, "pop", &[list(&[])]);
        assert_eq!(result.unwrap().unwrap(), list(&[]));
    }

    #[test]
    fn insert_at_index() {
        let r = reg();
        let result = r.call(
            TypeTag::List,
            "insert",
            &[list(&[1, 3]), Value::Int(1), Value::Int(2)],
        );
        assert_eq!(result.unwrap().unwrap(), list(&[1, 2, 3]));
    }

    #[test]
    fn insert_out_of_bounds() {
        let r = reg();
        let result = r.call(
            TypeTag::List,
            "insert",
            &[list(&[1]), Value::Int(5), Value::Int(2)],
        );
        assert!(matches!(
            result.unwrap(),
            Err(RuntimeError::IndexOutOfBounds { .. })
        ));
    }

    #[test]
    fn remove_at_index() {
        let r = reg();
        let result = r.call(TypeTag::List, "remove", &[list(&[1, 2, 3]), Value::Int(1)]);
        assert_eq!(result.unwrap().unwrap(), list(&[1, 3]));
    }

    #[test]
    fn remove_out_of_bounds() {
        let r = reg();
        let result = r.call(TypeTag::List, "remove", &[list(&[1]), Value::Int(5)]);
        assert!(matches!(
            result.unwrap(),
            Err(RuntimeError::IndexOutOfBounds { .. })
        ));
    }

    #[test]
    fn concat_lists() {
        let r = reg();
        let result = r.call(TypeTag::List, "concat", &[list(&[1, 2]), list(&[3, 4])]);
        assert_eq!(result.unwrap().unwrap(), list(&[1, 2, 3, 4]));
    }

    #[test]
    fn slice_middle() {
        let r = reg();
        let result = r.call(
            TypeTag::List,
            "slice",
            &[list(&[1, 2, 3, 4, 5]), Value::Int(1), Value::Int(4)],
        );
        assert_eq!(result.unwrap().unwrap(), list(&[2, 3, 4]));
    }

    #[test]
    fn slice_clamped() {
        let r = reg();
        let result = r.call(
            TypeTag::List,
            "slice",
            &[list(&[1, 2, 3]), Value::Int(-1), Value::Int(100)],
        );
        assert_eq!(result.unwrap().unwrap(), list(&[1, 2, 3]));
    }

    #[test]
    fn reverse_list() {
        let r = reg();
        let result = r.call(TypeTag::List, "reverse", &[list(&[1, 2, 3])]);
        assert_eq!(result.unwrap().unwrap(), list(&[3, 2, 1]));
    }

    #[test]
    fn sort_list() {
        let r = reg();
        let result = r.call(TypeTag::List, "sort", &[list(&[3, 1, 2])]);
        assert_eq!(result.unwrap().unwrap(), list(&[1, 2, 3]));
    }

    #[test]
    fn dedup_list() {
        let r = reg();
        let result = r.call(TypeTag::List, "dedup", &[list(&[1, 1, 2, 2, 3])]);
        assert_eq!(result.unwrap().unwrap(), list(&[1, 2, 3]));
    }

    #[test]
    fn flatten_nested() {
        let r = reg();
        let nested = Value::List(vec![list(&[1, 2]), list(&[3, 4]), Value::Int(5)]);
        let result = r.call(TypeTag::List, "flatten", &[nested]);
        assert_eq!(result.unwrap().unwrap(), list(&[1, 2, 3, 4, 5]));
    }

    #[test]
    fn zip_lists() {
        let r = reg();
        let result = r.call(TypeTag::List, "zip", &[list(&[1, 2, 3]), list(&[10, 20])]);
        let expected = Value::List(vec![
            Value::Tuple(vec![Value::Int(1), Value::Int(10)]),
            Value::Tuple(vec![Value::Int(2), Value::Int(20)]),
        ]);
        assert_eq!(result.unwrap().unwrap(), expected);
    }

    // ── Non-callback iteration methods ───────────────────────────────────

    #[test]
    fn take_elements() {
        let r = reg();
        let result = r.call(TypeTag::List, "take", &[list(&[1, 2, 3, 4]), Value::Int(2)]);
        assert_eq!(result.unwrap().unwrap(), list(&[1, 2]));
    }

    #[test]
    fn take_more_than_len() {
        let r = reg();
        let result = r.call(TypeTag::List, "take", &[list(&[1, 2]), Value::Int(10)]);
        assert_eq!(result.unwrap().unwrap(), list(&[1, 2]));
    }

    #[test]
    fn skip_elements() {
        let r = reg();
        let result = r.call(TypeTag::List, "skip", &[list(&[1, 2, 3, 4]), Value::Int(2)]);
        assert_eq!(result.unwrap().unwrap(), list(&[3, 4]));
    }

    #[test]
    fn enumerate_list() {
        let r = reg();
        let result = r.call(TypeTag::List, "enumerate", &[list(&[10, 20])]);
        let expected = Value::List(vec![
            Value::Tuple(vec![Value::Int(0), Value::Int(10)]),
            Value::Tuple(vec![Value::Int(1), Value::Int(20)]),
        ]);
        assert_eq!(result.unwrap().unwrap(), expected);
    }

    #[test]
    fn count_elements() {
        let r = reg();
        let result = r.call(TypeTag::List, "count", &[list(&[1, 2, 3])]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(3));
    }

    // ── Conversion ───────────────────────────────────────────────────────

    #[test]
    fn to_set_removes_duplicates() {
        let r = reg();
        let result = r.call(TypeTag::List, "to_set", &[list(&[1, 2, 2, 3, 1])]);
        let mut expected = BTreeSet::new();
        expected.insert(Value::Int(1));
        expected.insert(Value::Int(2));
        expected.insert(Value::Int(3));
        assert_eq!(result.unwrap().unwrap(), Value::Set(expected));
    }

    #[test]
    fn join_with_separator() {
        let r = reg();
        let result = r.call(
            TypeTag::List,
            "join",
            &[list(&[1, 2, 3]), Value::String(BockString::new(", "))],
        );
        assert_eq!(
            result.unwrap().unwrap(),
            Value::String(BockString::new("1, 2, 3"))
        );
    }

    // ── Trait implementations ────────────────────────────────────────────

    #[test]
    fn equals_same() {
        let r = reg();
        let result = r.call(TypeTag::List, "equals", &[list(&[1, 2]), list(&[1, 2])]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[tokio::test]
    async fn equals_different() {
        let r = reg();
        let result = r.call(TypeTag::List, "equals", &[list(&[1, 2]), list(&[1, 3])]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[tokio::test]
    async fn display_list() {
        let r = reg();
        let result = r.call(TypeTag::List, "display", &[list(&[1, 2, 3])]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::String(BockString::new("[1, 2, 3]"))
        );
    }

    #[tokio::test]
    async fn compare_lists() {
        let r = reg();
        let result = r.call(TypeTag::List, "compare", &[list(&[1, 2]), list(&[1, 3])]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(-1));
    }

    #[tokio::test]
    async fn hash_code_deterministic() {
        let r = reg();
        let h1 = r
            .call(TypeTag::List, "hash_code", &[list(&[1, 2])])
            .unwrap()
            .unwrap();
        let h2 = r
            .call(TypeTag::List, "hash_code", &[list(&[1, 2])])
            .unwrap()
            .unwrap();
        assert_eq!(h1, h2);
    }

    // ── Higher-order methods validate args ───────────────────────────────

    #[tokio::test]
    async fn map_requires_function() {
        let r = reg();
        let mut invoker = bock_interp::NoOpInvoker;
        let ho_func = r
            .get_ho_method(TypeTag::List, "map")
            .expect("map not registered");
        let result = ho_func(&[list(&[1]), Value::Int(0)], &mut invoker).await;
        assert!(matches!(result, Err(RuntimeError::TypeError(_))));
    }

    #[tokio::test]
    async fn filter_requires_function() {
        let r = reg();
        let mut invoker = bock_interp::NoOpInvoker;
        let ho_func = r
            .get_ho_method(TypeTag::List, "filter")
            .expect("filter not registered");
        let result = ho_func(&[list(&[1]), Value::Int(0)], &mut invoker).await;
        assert!(matches!(result, Err(RuntimeError::TypeError(_))));
    }
}
