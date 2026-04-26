//! Set collection type methods and trait implementations.

use std::collections::BTreeSet;

use futures::future::BoxFuture;
use bock_interp::{BockString, BuiltinRegistry, CallbackInvoker, RuntimeError, TypeTag, Value};

/// Register all Set methods and trait implementations.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Core access ──────────────────────────────────────────────────────
    registry.register(TypeTag::Set, "len", set_len);
    registry.register(TypeTag::Set, "contains", set_contains);
    registry.register(TypeTag::Set, "is_empty", set_is_empty);

    // ── Mutation (immutable — return new sets) ───────────────────────────
    registry.register(TypeTag::Set, "add", set_add);
    registry.register(TypeTag::Set, "remove", set_remove);

    // ── Set operations ───────────────────────────────────────────────────
    registry.register(TypeTag::Set, "union", set_union);
    registry.register(TypeTag::Set, "intersection", set_intersection);
    registry.register(TypeTag::Set, "difference", set_difference);
    registry.register(
        TypeTag::Set,
        "symmetric_difference",
        set_symmetric_difference,
    );
    registry.register(TypeTag::Set, "is_subset", set_is_subset);
    registry.register(TypeTag::Set, "is_superset", set_is_superset);
    registry.register(TypeTag::Set, "is_disjoint", set_is_disjoint);

    // ── Higher-order methods ────────────────────────────────────────────
    registry.register_ho(TypeTag::Set, "filter", set_filter);
    registry.register_ho(TypeTag::Set, "for_each", set_for_each);
    registry.register_ho(TypeTag::Set, "map", set_map);

    // ── Conversion ───────────────────────────────────────────────────────
    registry.register(TypeTag::Set, "to_list", set_to_list);

    // ── Trait implementations ────────────────────────────────────────────
    registry.register(TypeTag::Set, "equals", set_equals);
    registry.register(TypeTag::Set, "display", set_display);
    registry.register(TypeTag::Set, "hash_code", set_hash_code);
    registry.register(TypeTag::Set, "compare", set_compare);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_set<'a>(
    args: &'a [Value],
    pos: usize,
    method: &str,
) -> Result<&'a BTreeSet<Value>, RuntimeError> {
    match args.get(pos) {
        Some(Value::Set(s)) => Ok(s),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Set.{method} expects Set, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// ─── Core access ──────────────────────────────────────────────────────────────

fn set_len(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_set(args, 0, "len")?;
    Ok(Value::Int(s.len() as i64))
}

fn set_contains(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_set(args, 0, "contains")?;
    let val = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    Ok(Value::Bool(s.contains(val)))
}

fn set_is_empty(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_set(args, 0, "is_empty")?;
    Ok(Value::Bool(s.is_empty()))
}

// ─── Mutation (immutable) ─────────────────────────────────────────────────────

fn set_add(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_set(args, 0, "add")?;
    let val = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    let mut new_set = s.clone();
    new_set.insert(val.clone());
    Ok(Value::Set(new_set))
}

fn set_remove(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_set(args, 0, "remove")?;
    let val = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    let mut new_set = s.clone();
    new_set.remove(val);
    Ok(Value::Set(new_set))
}

// ─── Set operations ───────────────────────────────────────────────────────────

fn set_union(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_set(args, 0, "union")?;
    let b = expect_set(args, 1, "union")?;
    Ok(Value::Set(a.union(b).cloned().collect()))
}

fn set_intersection(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_set(args, 0, "intersection")?;
    let b = expect_set(args, 1, "intersection")?;
    Ok(Value::Set(a.intersection(b).cloned().collect()))
}

fn set_difference(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_set(args, 0, "difference")?;
    let b = expect_set(args, 1, "difference")?;
    Ok(Value::Set(a.difference(b).cloned().collect()))
}

fn set_symmetric_difference(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_set(args, 0, "symmetric_difference")?;
    let b = expect_set(args, 1, "symmetric_difference")?;
    Ok(Value::Set(a.symmetric_difference(b).cloned().collect()))
}

fn set_is_subset(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_set(args, 0, "is_subset")?;
    let b = expect_set(args, 1, "is_subset")?;
    Ok(Value::Bool(a.is_subset(b)))
}

fn set_is_superset(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_set(args, 0, "is_superset")?;
    let b = expect_set(args, 1, "is_superset")?;
    Ok(Value::Bool(a.is_superset(b)))
}

fn set_is_disjoint(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_set(args, 0, "is_disjoint")?;
    let b = expect_set(args, 1, "is_disjoint")?;
    Ok(Value::Bool(a.is_disjoint(b)))
}

// ─── Higher-order methods ─────────────────────────────────────────────────────

fn expect_fn<'a>(args: &'a [Value], pos: usize, method: &str) -> Result<&'a Value, RuntimeError> {
    match args.get(pos) {
        Some(v @ Value::Function(_)) => Ok(v),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Set.{method} expects Function, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

/// `set.filter(fn)` — keep elements where `fn(element)` returns `true`.
fn set_filter<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let s = expect_set(args, 0, "filter")?;
    let f = expect_fn(args, 1, "filter")?;
    let mut result = BTreeSet::new();
    for item in s {
        if let Value::Bool(true) = invoker.invoke(f, std::slice::from_ref(item)).await? {
            result.insert(item.clone());
        }
    }
    Ok(Value::Set(result))
})
}

/// `set.for_each(fn)` — call `fn(element)` for each element, returns Void.
fn set_for_each<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let s = expect_set(args, 0, "for_each")?;
    let f = expect_fn(args, 1, "for_each")?;
    for item in s {
        invoker.invoke(f, std::slice::from_ref(item)).await?;
    }
    Ok(Value::Void)
})
}

/// `set.map(fn)` — apply `fn` to each element, returning a new set.
fn set_map<'a>(args: &'a [Value], invoker: &'a mut dyn CallbackInvoker) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
    let s = expect_set(args, 0, "map")?;
    let f = expect_fn(args, 1, "map")?;
    let mut result = BTreeSet::new();
    for item in s {
        let mapped = invoker.invoke(f, std::slice::from_ref(item)).await?;
        result.insert(mapped);
    }
    Ok(Value::Set(result))
})
}

// ─── Conversion ───────────────────────────────────────────────────────────────

fn set_to_list(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_set(args, 0, "to_list")?;
    Ok(Value::List(s.iter().cloned().collect()))
}

// ─── Trait implementations ────────────────────────────────────────────────────

fn set_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_set(args, 0, "equals")?;
    let b = expect_set(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

fn set_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let recv = args.first().ok_or(RuntimeError::ArityMismatch {
        expected: 1,
        got: 0,
    })?;
    Ok(Value::String(BockString::new(recv.to_string())))
}

fn set_hash_code(args: &[Value]) -> Result<Value, RuntimeError> {
    use std::hash::{Hash, Hasher};
    let s = expect_set(args, 0, "hash_code")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for item in s {
        item.hash(&mut hasher);
    }
    Ok(Value::Int(hasher.finish() as i64))
}

fn set_compare(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_set(args, 0, "compare")?;
    let b = expect_set(args, 1, "compare")?;
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

    fn make_set(vals: &[i64]) -> Value {
        let s: BTreeSet<Value> = vals.iter().map(|&v| Value::Int(v)).collect();
        Value::Set(s)
    }

    // ── Core access ──────────────────────────────────────────────────────

    #[test]
    fn len() {
        let r = reg();
        let result = r.call(TypeTag::Set, "len", &[make_set(&[1, 2, 3])]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(3));
    }

    #[test]
    fn contains_true() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "contains",
            &[make_set(&[1, 2, 3]), Value::Int(2)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn contains_false() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "contains",
            &[make_set(&[1, 2, 3]), Value::Int(5)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn is_empty_true() {
        let r = reg();
        let result = r.call(TypeTag::Set, "is_empty", &[make_set(&[])]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn is_empty_false() {
        let r = reg();
        let result = r.call(TypeTag::Set, "is_empty", &[make_set(&[1])]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    // ── Mutation (immutable) ─────────────────────────────────────────────

    #[test]
    fn add_new_element() {
        let r = reg();
        let result = r.call(TypeTag::Set, "add", &[make_set(&[1, 2]), Value::Int(3)]);
        assert_eq!(result.unwrap().unwrap(), make_set(&[1, 2, 3]));
    }

    #[test]
    fn add_existing_element() {
        let r = reg();
        let result = r.call(TypeTag::Set, "add", &[make_set(&[1, 2]), Value::Int(2)]);
        assert_eq!(result.unwrap().unwrap(), make_set(&[1, 2]));
    }

    #[test]
    fn remove_existing() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "remove",
            &[make_set(&[1, 2, 3]), Value::Int(2)],
        );
        assert_eq!(result.unwrap().unwrap(), make_set(&[1, 3]));
    }

    #[test]
    fn remove_missing() {
        let r = reg();
        let s = make_set(&[1, 2]);
        let result = r.call(TypeTag::Set, "remove", &[s.clone(), Value::Int(99)]);
        assert_eq!(result.unwrap().unwrap(), s);
    }

    // ── Set operations ───────────────────────────────────────────────────

    #[test]
    fn union() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "union",
            &[make_set(&[1, 2]), make_set(&[2, 3])],
        );
        assert_eq!(result.unwrap().unwrap(), make_set(&[1, 2, 3]));
    }

    #[test]
    fn intersection() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "intersection",
            &[make_set(&[1, 2, 3]), make_set(&[2, 3, 4])],
        );
        assert_eq!(result.unwrap().unwrap(), make_set(&[2, 3]));
    }

    #[test]
    fn difference() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "difference",
            &[make_set(&[1, 2, 3]), make_set(&[2, 3, 4])],
        );
        assert_eq!(result.unwrap().unwrap(), make_set(&[1]));
    }

    #[test]
    fn symmetric_difference() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "symmetric_difference",
            &[make_set(&[1, 2, 3]), make_set(&[2, 3, 4])],
        );
        assert_eq!(result.unwrap().unwrap(), make_set(&[1, 4]));
    }

    #[test]
    fn is_subset_true() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "is_subset",
            &[make_set(&[1, 2]), make_set(&[1, 2, 3])],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn is_subset_false() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "is_subset",
            &[make_set(&[1, 4]), make_set(&[1, 2, 3])],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn is_superset_true() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "is_superset",
            &[make_set(&[1, 2, 3]), make_set(&[1, 2])],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn is_disjoint_true() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "is_disjoint",
            &[make_set(&[1, 2]), make_set(&[3, 4])],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn is_disjoint_false() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "is_disjoint",
            &[make_set(&[1, 2]), make_set(&[2, 3])],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    // ── Conversion ───────────────────────────────────────────────────────

    #[test]
    fn to_list_sorted() {
        let r = reg();
        let result = r.call(TypeTag::Set, "to_list", &[make_set(&[3, 1, 2])]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
    }

    // ── Trait implementations ────────────────────────────────────────────

    #[test]
    fn equals_same() {
        let r = reg();
        let s = make_set(&[1, 2, 3]);
        let result = r.call(TypeTag::Set, "equals", &[s.clone(), s]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn equals_different() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "equals",
            &[make_set(&[1, 2]), make_set(&[1, 3])],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn display_set() {
        let r = reg();
        let result = r.call(TypeTag::Set, "display", &[make_set(&[1, 2])]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::String(BockString::new("{1, 2}"))
        );
    }

    #[test]
    fn hash_code_deterministic() {
        let r = reg();
        let s = make_set(&[1, 2]);
        let h1 = r
            .call(TypeTag::Set, "hash_code", &[s.clone()])
            .unwrap()
            .unwrap();
        let h2 = r.call(TypeTag::Set, "hash_code", &[s]).unwrap().unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn compare_sets() {
        let r = reg();
        let result = r.call(
            TypeTag::Set,
            "compare",
            &[make_set(&[1, 2]), make_set(&[1, 3])],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Int(-1));
    }
}
