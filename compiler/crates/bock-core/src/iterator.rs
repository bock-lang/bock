//! Iterator protocol methods and combinators.
//!
//! Registers methods on `TypeTag::Iterator` for lazy iteration:
//! `next`, `map`, `filter`, `take`, `skip`, `enumerate`, `zip`, `chain`, `collect`.
//!
//! Also registers `iter()` on List, Set, Map, and Range to create iterators.

use bock_interp::{
    BuiltinRegistry, IteratorKind, IteratorNext, IteratorValue, RuntimeError, TypeTag, Value,
};

/// Register all iterator-related methods.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Creating iterators from collections ──────────────────────────────
    registry.register(TypeTag::List, "iter", list_iter);
    registry.register(TypeTag::Set, "iter", set_iter);
    registry.register(TypeTag::Map, "iter", map_iter);
    registry.register(TypeTag::Range, "iter", range_iter);

    // ── Iterator methods ─────────────────────────────────────────────────
    registry.register(TypeTag::Iterator, "next", iter_next);
    registry.register(TypeTag::Iterator, "map", iter_map);
    registry.register(TypeTag::Iterator, "filter", iter_filter);
    registry.register(TypeTag::Iterator, "take", iter_take);
    registry.register(TypeTag::Iterator, "skip", iter_skip);
    registry.register(TypeTag::Iterator, "enumerate", iter_enumerate);
    registry.register(TypeTag::Iterator, "zip", iter_zip);
    registry.register(TypeTag::Iterator, "chain", iter_chain);
    registry.register(TypeTag::Iterator, "collect", iter_collect);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_iterator(args: &[Value], method: &str) -> Result<IteratorValue, RuntimeError> {
    match args.first() {
        Some(Value::Iterator(it)) => Ok(it.clone()),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Iterator.{method} expects Iterator, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: 1,
            got: 0,
        }),
    }
}

fn expect_int(args: &[Value], pos: usize, method: &str) -> Result<i64, RuntimeError> {
    match args.get(pos) {
        Some(Value::Int(v)) => Ok(*v),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Iterator.{method} expects Int, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

fn expect_fn_value(
    args: &[Value],
    pos: usize,
    method: &str,
) -> Result<bock_interp::FnValue, RuntimeError> {
    match args.get(pos) {
        Some(Value::Function(f)) => Ok(f.clone()),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Iterator.{method} expects Function, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// ─── Creating iterators ───────────────────────────────────────────────────────

fn list_iter(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::List(items)) => Ok(Value::Iterator(IteratorValue::new(IteratorKind::List {
            items: items.clone(),
            pos: 0,
        }))),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "List.iter expects List, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: 1,
            got: 0,
        }),
    }
}

fn set_iter(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::Set(set)) => {
            let items: Vec<Value> = set.iter().cloned().collect();
            Ok(Value::Iterator(IteratorValue::new(IteratorKind::Set {
                items,
                pos: 0,
            })))
        }
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Set.iter expects Set, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: 1,
            got: 0,
        }),
    }
}

fn map_iter(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::Map(map)) => {
            let items: Vec<(Value, Value)> =
                map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            Ok(Value::Iterator(IteratorValue::new(
                IteratorKind::MapEntries { items, pos: 0 },
            )))
        }
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Map.iter expects Map, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: 1,
            got: 0,
        }),
    }
}

fn range_iter(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::Range {
            start,
            end,
            inclusive,
            step,
        }) => Ok(Value::Iterator(IteratorValue::new(IteratorKind::Range {
            current: *start,
            end: *end,
            inclusive: *inclusive,
            step: *step,
        }))),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Range.iter expects Range, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: 1,
            got: 0,
        }),
    }
}

// ─── Iterator methods ─────────────────────────────────────────────────────────

fn iter_next(args: &[Value]) -> Result<Value, RuntimeError> {
    let it = expect_iterator(args, "next")?;
    let mut kind = it.kind.lock().unwrap();
    match kind.next() {
        IteratorNext::Some(val) => Ok(Value::Optional(Some(Box::new(val)))),
        IteratorNext::Done => Ok(Value::Optional(None)),
        IteratorNext::NeedsMapCallback { .. } | IteratorNext::NeedsFilterCallback { .. } => {
            Err(RuntimeError::TypeError(
                "Iterator.next on map/filter combinator requires interpreter support \
                 (not available from builtin dispatch)"
                    .to_string(),
            ))
        }
    }
}

fn iter_map(args: &[Value]) -> Result<Value, RuntimeError> {
    let it = expect_iterator(args, "map")?;
    let func = expect_fn_value(args, 1, "map")?;
    Ok(Value::Iterator(IteratorValue::new(IteratorKind::Map {
        source: it.kind.clone(),
        func,
    })))
}

fn iter_filter(args: &[Value]) -> Result<Value, RuntimeError> {
    let it = expect_iterator(args, "filter")?;
    let func = expect_fn_value(args, 1, "filter")?;
    Ok(Value::Iterator(IteratorValue::new(IteratorKind::Filter {
        source: it.kind.clone(),
        pred: func,
    })))
}

fn iter_take(args: &[Value]) -> Result<Value, RuntimeError> {
    let it = expect_iterator(args, "take")?;
    let n = expect_int(args, 1, "take")?;
    let n = n.max(0) as usize;
    Ok(Value::Iterator(IteratorValue::new(IteratorKind::Take {
        source: it.kind.clone(),
        remaining: n,
    })))
}

fn iter_skip(args: &[Value]) -> Result<Value, RuntimeError> {
    let it = expect_iterator(args, "skip")?;
    let n = expect_int(args, 1, "skip")?;
    let n = n.max(0) as usize;
    Ok(Value::Iterator(IteratorValue::new(IteratorKind::Skip {
        source: it.kind.clone(),
        to_skip: n,
        skipped: false,
    })))
}

fn iter_enumerate(args: &[Value]) -> Result<Value, RuntimeError> {
    let it = expect_iterator(args, "enumerate")?;
    Ok(Value::Iterator(IteratorValue::new(
        IteratorKind::Enumerate {
            source: it.kind.clone(),
            index: 0,
        },
    )))
}

fn iter_zip(args: &[Value]) -> Result<Value, RuntimeError> {
    let it = expect_iterator(args, "zip")?;
    let other = match args.get(1) {
        Some(Value::Iterator(other)) => other.clone(),
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "Iterator.zip expects Iterator, got {other}"
            )))
        }
        None => {
            return Err(RuntimeError::ArityMismatch {
                expected: 2,
                got: 1,
            })
        }
    };
    Ok(Value::Iterator(IteratorValue::new(IteratorKind::Zip {
        a: it.kind.clone(),
        b: other.kind.clone(),
    })))
}

fn iter_chain(args: &[Value]) -> Result<Value, RuntimeError> {
    let it = expect_iterator(args, "chain")?;
    let other = match args.get(1) {
        Some(Value::Iterator(other)) => other.clone(),
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "Iterator.chain expects Iterator, got {other}"
            )))
        }
        None => {
            return Err(RuntimeError::ArityMismatch {
                expected: 2,
                got: 1,
            })
        }
    };
    Ok(Value::Iterator(IteratorValue::new(IteratorKind::Chain {
        a: it.kind.clone(),
        b: other.kind.clone(),
        first_done: false,
    })))
}

fn iter_collect(args: &[Value]) -> Result<Value, RuntimeError> {
    let it = expect_iterator(args, "collect")?;
    let mut result = Vec::new();
    loop {
        let mut kind = it.kind.lock().unwrap();
        match kind.next() {
            IteratorNext::Some(val) => result.push(val),
            IteratorNext::Done => break,
            IteratorNext::NeedsMapCallback { .. } | IteratorNext::NeedsFilterCallback { .. } => {
                return Err(RuntimeError::TypeError(
                    "Iterator.collect on map/filter combinator requires interpreter support \
                     (not available from builtin dispatch)"
                        .to_string(),
                ));
            }
        }
    }
    Ok(Value::List(result))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn reg() -> BuiltinRegistry {
        let mut r = BuiltinRegistry::new();
        register(&mut r);
        r
    }

    fn list(vals: &[i64]) -> Value {
        Value::List(vals.iter().map(|&v| Value::Int(v)).collect())
    }

    fn range(start: i64, end: i64, inclusive: bool) -> Value {
        Value::Range {
            start,
            end,
            inclusive,
            step: 1,
        }
    }

    // ── iter() creation ───────────────────────────────────────────────────

    #[test]
    fn list_iter_creates_iterator() {
        let r = reg();
        let result = r.call(TypeTag::List, "iter", &[list(&[1, 2, 3])]);
        assert!(matches!(result.unwrap().unwrap(), Value::Iterator(_)));
    }

    #[test]
    fn range_iter_creates_iterator() {
        let r = reg();
        let result = r.call(TypeTag::Range, "iter", &[range(0, 3, false)]);
        assert!(matches!(result.unwrap().unwrap(), Value::Iterator(_)));
    }

    #[test]
    fn set_iter_creates_iterator() {
        let r = reg();
        let mut set = BTreeSet::new();
        set.insert(Value::Int(1));
        set.insert(Value::Int(2));
        let result = r.call(TypeTag::Set, "iter", &[Value::Set(set)]);
        assert!(matches!(result.unwrap().unwrap(), Value::Iterator(_)));
    }

    #[test]
    fn map_iter_creates_iterator() {
        let r = reg();
        let mut map = BTreeMap::new();
        map.insert(Value::Int(1), Value::Int(10));
        let result = r.call(TypeTag::Map, "iter", &[Value::Map(map)]);
        assert!(matches!(result.unwrap().unwrap(), Value::Iterator(_)));
    }

    // ── next() ────────────────────────────────────────────────────────────

    #[test]
    fn next_returns_elements_then_none() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[10, 20])])
            .unwrap()
            .unwrap();

        let first = r
            .call(TypeTag::Iterator, "next", std::slice::from_ref(&iter_val))
            .unwrap()
            .unwrap();
        assert_eq!(first, Value::Optional(Some(Box::new(Value::Int(10)))));

        let second = r
            .call(TypeTag::Iterator, "next", std::slice::from_ref(&iter_val))
            .unwrap()
            .unwrap();
        assert_eq!(second, Value::Optional(Some(Box::new(Value::Int(20)))));

        let third = r
            .call(TypeTag::Iterator, "next", &[iter_val])
            .unwrap()
            .unwrap();
        assert_eq!(third, Value::Optional(None));
    }

    #[test]
    fn range_iter_produces_values() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::Range, "iter", &[range(1, 4, false)])
            .unwrap()
            .unwrap();

        let mut results = Vec::new();
        loop {
            let v = r
                .call(TypeTag::Iterator, "next", std::slice::from_ref(&iter_val))
                .unwrap()
                .unwrap();
            match v {
                Value::Optional(Some(val)) => results.push(*val),
                Value::Optional(None) => break,
                _ => panic!("unexpected"),
            }
        }
        assert_eq!(results, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    }

    #[test]
    fn range_inclusive_iter() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::Range, "iter", &[range(1, 3, true)])
            .unwrap()
            .unwrap();

        let mut results = Vec::new();
        loop {
            let v = r
                .call(TypeTag::Iterator, "next", std::slice::from_ref(&iter_val))
                .unwrap()
                .unwrap();
            match v {
                Value::Optional(Some(val)) => results.push(*val),
                Value::Optional(None) => break,
                _ => panic!("unexpected"),
            }
        }
        assert_eq!(results, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    }

    // ── collect() ─────────────────────────────────────────────────────────

    #[test]
    fn collect_drains_into_list() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1, 2, 3])])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[iter_val])
            .unwrap()
            .unwrap();
        assert_eq!(collected, list(&[1, 2, 3]));
    }

    #[test]
    fn collect_range_iter() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::Range, "iter", &[range(0, 3, false)])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[iter_val])
            .unwrap()
            .unwrap();
        assert_eq!(collected, list(&[0, 1, 2]));
    }

    // ── take() ────────────────────────────────────────────────────────────

    #[test]
    fn take_limits_elements() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1, 2, 3, 4, 5])])
            .unwrap()
            .unwrap();
        let taken = r
            .call(TypeTag::Iterator, "take", &[iter_val, Value::Int(3)])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[taken])
            .unwrap()
            .unwrap();
        assert_eq!(collected, list(&[1, 2, 3]));
    }

    #[test]
    fn take_zero() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1, 2])])
            .unwrap()
            .unwrap();
        let taken = r
            .call(TypeTag::Iterator, "take", &[iter_val, Value::Int(0)])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[taken])
            .unwrap()
            .unwrap();
        assert_eq!(collected, list(&[]));
    }

    // ── skip() ────────────────────────────────────────────────────────────

    #[test]
    fn skip_elements() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1, 2, 3, 4, 5])])
            .unwrap()
            .unwrap();
        let skipped = r
            .call(TypeTag::Iterator, "skip", &[iter_val, Value::Int(2)])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[skipped])
            .unwrap()
            .unwrap();
        assert_eq!(collected, list(&[3, 4, 5]));
    }

    #[test]
    fn skip_all() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1, 2])])
            .unwrap()
            .unwrap();
        let skipped = r
            .call(TypeTag::Iterator, "skip", &[iter_val, Value::Int(10)])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[skipped])
            .unwrap()
            .unwrap();
        assert_eq!(collected, list(&[]));
    }

    // ── enumerate() ───────────────────────────────────────────────────────

    #[test]
    fn enumerate_adds_index() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[10, 20, 30])])
            .unwrap()
            .unwrap();
        let enumerated = r
            .call(TypeTag::Iterator, "enumerate", &[iter_val])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[enumerated])
            .unwrap()
            .unwrap();
        let expected = Value::List(vec![
            Value::Tuple(vec![Value::Int(0), Value::Int(10)]),
            Value::Tuple(vec![Value::Int(1), Value::Int(20)]),
            Value::Tuple(vec![Value::Int(2), Value::Int(30)]),
        ]);
        assert_eq!(collected, expected);
    }

    // ── zip() ─────────────────────────────────────────────────────────────

    #[test]
    fn zip_two_iterators() {
        let r = reg();
        let iter_a = r
            .call(TypeTag::List, "iter", &[list(&[1, 2, 3])])
            .unwrap()
            .unwrap();
        let iter_b = r
            .call(TypeTag::List, "iter", &[list(&[10, 20])])
            .unwrap()
            .unwrap();
        let zipped = r
            .call(TypeTag::Iterator, "zip", &[iter_a, iter_b])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[zipped])
            .unwrap()
            .unwrap();
        let expected = Value::List(vec![
            Value::Tuple(vec![Value::Int(1), Value::Int(10)]),
            Value::Tuple(vec![Value::Int(2), Value::Int(20)]),
        ]);
        assert_eq!(collected, expected);
    }

    // ── chain() ───────────────────────────────────────────────────────────

    #[test]
    fn chain_two_iterators() {
        let r = reg();
        let iter_a = r
            .call(TypeTag::List, "iter", &[list(&[1, 2])])
            .unwrap()
            .unwrap();
        let iter_b = r
            .call(TypeTag::List, "iter", &[list(&[3, 4])])
            .unwrap()
            .unwrap();
        let chained = r
            .call(TypeTag::Iterator, "chain", &[iter_a, iter_b])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[chained])
            .unwrap()
            .unwrap();
        assert_eq!(collected, list(&[1, 2, 3, 4]));
    }

    // ── Lazy evaluation verification ──────────────────────────────────────

    #[test]
    fn take_is_lazy_does_not_consume_all() {
        // Create iterator, take 2, then collect. The remaining elements
        // in the source should still be accessible.
        let it = IteratorValue::new(IteratorKind::List {
            items: vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)],
            pos: 0,
        });
        let take_kind = IteratorKind::Take {
            source: it.kind.clone(),
            remaining: 2,
        };
        let take_it = IteratorValue::new(take_kind);

        // Collect the take iterator
        let r = reg();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[Value::Iterator(take_it)])
            .unwrap()
            .unwrap();
        assert_eq!(collected, list(&[1, 2]));

        // The source iterator should be at position 2, not fully consumed
        let remaining = r
            .call(TypeTag::Iterator, "next", &[Value::Iterator(it)])
            .unwrap()
            .unwrap();
        assert_eq!(remaining, Value::Optional(Some(Box::new(Value::Int(3)))));
    }

    // ── Combinator chaining ───────────────────────────────────────────────

    #[test]
    fn skip_then_take() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1, 2, 3, 4, 5])])
            .unwrap()
            .unwrap();
        let skipped = r
            .call(TypeTag::Iterator, "skip", &[iter_val, Value::Int(1)])
            .unwrap()
            .unwrap();
        let taken = r
            .call(TypeTag::Iterator, "take", &[skipped, Value::Int(2)])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[taken])
            .unwrap()
            .unwrap();
        assert_eq!(collected, list(&[2, 3]));
    }

    #[test]
    fn enumerate_then_take() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[10, 20, 30])])
            .unwrap()
            .unwrap();
        let enumerated = r
            .call(TypeTag::Iterator, "enumerate", &[iter_val])
            .unwrap()
            .unwrap();
        let taken = r
            .call(TypeTag::Iterator, "take", &[enumerated, Value::Int(2)])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[taken])
            .unwrap()
            .unwrap();
        let expected = Value::List(vec![
            Value::Tuple(vec![Value::Int(0), Value::Int(10)]),
            Value::Tuple(vec![Value::Int(1), Value::Int(20)]),
        ]);
        assert_eq!(collected, expected);
    }

    // ── map/filter create lazy iterators (need interpreter for next) ─────

    #[test]
    fn map_creates_lazy_iterator() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1, 2])])
            .unwrap()
            .unwrap();
        let func = Value::Function(bock_interp::FnValue::new_named("double"));
        let mapped = r
            .call(TypeTag::Iterator, "map", &[iter_val, func])
            .unwrap()
            .unwrap();
        // The map iterator is created lazily
        assert!(matches!(mapped, Value::Iterator(_)));
    }

    #[test]
    fn filter_creates_lazy_iterator() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1, 2])])
            .unwrap()
            .unwrap();
        let pred = Value::Function(bock_interp::FnValue::new_named("is_even"));
        let filtered = r
            .call(TypeTag::Iterator, "filter", &[iter_val, pred])
            .unwrap()
            .unwrap();
        assert!(matches!(filtered, Value::Iterator(_)));
    }

    // ── Map entries iteration ─────────────────────────────────────────────

    #[test]
    fn map_entries_as_tuples() {
        let r = reg();
        let mut map = BTreeMap::new();
        map.insert(Value::Int(1), Value::Int(10));
        map.insert(Value::Int(2), Value::Int(20));
        let iter_val = r
            .call(TypeTag::Map, "iter", &[Value::Map(map)])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[iter_val])
            .unwrap()
            .unwrap();
        let expected = Value::List(vec![
            Value::Tuple(vec![Value::Int(1), Value::Int(10)]),
            Value::Tuple(vec![Value::Int(2), Value::Int(20)]),
        ]);
        assert_eq!(collected, expected);
    }

    // ── Set iteration ─────────────────────────────────────────────────────

    #[test]
    fn set_iter_collects_sorted() {
        let r = reg();
        let mut set = BTreeSet::new();
        set.insert(Value::Int(3));
        set.insert(Value::Int(1));
        set.insert(Value::Int(2));
        let iter_val = r
            .call(TypeTag::Set, "iter", &[Value::Set(set)])
            .unwrap()
            .unwrap();
        let collected = r
            .call(TypeTag::Iterator, "collect", &[iter_val])
            .unwrap()
            .unwrap();
        assert_eq!(collected, list(&[1, 2, 3]));
    }

    // ── Error cases ───────────────────────────────────────────────────────

    #[test]
    fn map_requires_function() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1])])
            .unwrap()
            .unwrap();
        let result = r.call(TypeTag::Iterator, "map", &[iter_val, Value::Int(0)]);
        assert!(matches!(result.unwrap(), Err(RuntimeError::TypeError(_))));
    }

    #[test]
    fn filter_requires_function() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1])])
            .unwrap()
            .unwrap();
        let result = r.call(TypeTag::Iterator, "filter", &[iter_val, Value::Int(0)]);
        assert!(matches!(result.unwrap(), Err(RuntimeError::TypeError(_))));
    }

    #[test]
    fn zip_requires_iterator() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1])])
            .unwrap()
            .unwrap();
        let result = r.call(TypeTag::Iterator, "zip", &[iter_val, Value::Int(0)]);
        assert!(matches!(result.unwrap(), Err(RuntimeError::TypeError(_))));
    }

    #[test]
    fn chain_requires_iterator() {
        let r = reg();
        let iter_val = r
            .call(TypeTag::List, "iter", &[list(&[1])])
            .unwrap()
            .unwrap();
        let result = r.call(TypeTag::Iterator, "chain", &[iter_val, Value::Int(0)]);
        assert!(matches!(result.unwrap(), Err(RuntimeError::TypeError(_))));
    }
}
