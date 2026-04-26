//! Instant primitive type — monotonic point in time (per-process).
//!
//! Registers instance methods, arithmetic operator overloads (with Duration),
//! comparison/equality, and the `Instant.now()` associated constructor.

use bock_interp::{BuiltinRegistry, RuntimeError, TypeTag, Value};

/// Register all Instant methods, operators, and associated functions.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Arithmetic trait methods (used by BinaryOp trait dispatch) ───────
    // `add` handles Instant + Duration → Instant.
    // `sub` handles both Instant - Instant → Duration and Instant - Duration → Instant.
    registry.register(TypeTag::Instant, "add", instant_add);
    registry.register(TypeTag::Instant, "sub", instant_sub);

    // ── Comparable / Equatable ───────────────────────────────────────────
    registry.register(TypeTag::Instant, "compare", instant_compare);
    registry.register(TypeTag::Instant, "equals", instant_equals);

    // ── Displayable ──────────────────────────────────────────────────────
    registry.register(TypeTag::Instant, "display", instant_display);

    // ── Instance methods ─────────────────────────────────────────────────
    registry.register(TypeTag::Instant, "elapsed", instant_elapsed);
    registry.register(TypeTag::Instant, "duration_since", instant_duration_since);

    // ── Associated functions (registered as qualified globals) ───────────
    registry.register_global("Instant.now", instant_now);
}

// ─── Helpers ─────────────────────────────────────────────────────────────

fn expect_instant(args: &[Value], pos: usize, method: &str) -> Result<std::time::Instant, RuntimeError> {
    match args.get(pos) {
        Some(Value::Instant(i)) => Ok(*i),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Instant.{method} expects Instant, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

/// Apply a signed nanosecond offset to an Instant, saturating at bounds.
fn apply_nanos(instant: std::time::Instant, nanos: i64) -> std::time::Instant {
    if nanos >= 0 {
        instant
            .checked_add(std::time::Duration::from_nanos(nanos as u64))
            .unwrap_or(instant)
    } else {
        let abs = nanos.unsigned_abs();
        instant
            .checked_sub(std::time::Duration::from_nanos(abs))
            .unwrap_or(instant)
    }
}

// ─── Associated functions ────────────────────────────────────────────────

fn instant_now(_args: &[Value]) -> Result<Value, RuntimeError> {
    Ok(Value::Instant(std::time::Instant::now()))
}

// ─── Arithmetic ──────────────────────────────────────────────────────────

/// `Instant + Duration → Instant`.
fn instant_add(args: &[Value]) -> Result<Value, RuntimeError> {
    let i = expect_instant(args, 0, "add")?;
    match args.get(1) {
        Some(Value::Duration(n)) => Ok(Value::Instant(apply_nanos(i, *n))),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Instant + expects Duration, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: 2,
            got: 1,
        }),
    }
}

/// `Instant - Instant → Duration` or `Instant - Duration → Instant`.
fn instant_sub(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_instant(args, 0, "sub")?;
    match args.get(1) {
        Some(Value::Instant(b)) => {
            // Instant - Instant → Duration.
            if a >= *b {
                let d = a.duration_since(*b);
                let nanos = d.as_nanos();
                if nanos > i64::MAX as u128 {
                    Err(RuntimeError::IntOverflow)
                } else {
                    Ok(Value::Duration(nanos as i64))
                }
            } else {
                let d = b.duration_since(a);
                let nanos = d.as_nanos();
                if nanos > i64::MAX as u128 {
                    Err(RuntimeError::IntOverflow)
                } else {
                    Ok(Value::Duration(-(nanos as i64)))
                }
            }
        }
        Some(Value::Duration(n)) => {
            // Instant - Duration → Instant.
            Ok(Value::Instant(apply_nanos(a, -*n)))
        }
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Instant - expects Instant or Duration, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: 2,
            got: 1,
        }),
    }
}

// ─── Comparable / Equatable ──────────────────────────────────────────────

fn instant_compare(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_instant(args, 0, "compare")?;
    let b = expect_instant(args, 1, "compare")?;
    Ok(Value::Int(a.cmp(&b) as i64))
}

fn instant_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_instant(args, 0, "equals")?;
    let b = expect_instant(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

// ─── Displayable ─────────────────────────────────────────────────────────

fn instant_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let _ = expect_instant(args, 0, "display")?;
    Ok(Value::String(bock_interp::BockString::new("<instant>")))
}

// ─── Instance methods ────────────────────────────────────────────────────

fn instant_elapsed(args: &[Value]) -> Result<Value, RuntimeError> {
    let i = expect_instant(args, 0, "elapsed")?;
    let nanos = i.elapsed().as_nanos();
    if nanos > i64::MAX as u128 {
        Err(RuntimeError::IntOverflow)
    } else {
        Ok(Value::Duration(nanos as i64))
    }
}

fn instant_duration_since(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_instant(args, 0, "duration_since")?;
    let b = expect_instant(args, 1, "duration_since")?;
    if a >= b {
        let d = a.duration_since(b);
        let nanos = d.as_nanos();
        if nanos > i64::MAX as u128 {
            Err(RuntimeError::IntOverflow)
        } else {
            Ok(Value::Duration(nanos as i64))
        }
    } else {
        let d = b.duration_since(a);
        let nanos = d.as_nanos();
        if nanos > i64::MAX as u128 {
            Err(RuntimeError::IntOverflow)
        } else {
            Ok(Value::Duration(-(nanos as i64)))
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> BuiltinRegistry {
        let mut r = BuiltinRegistry::new();
        register(&mut r);
        r
    }

    #[test]
    fn now_returns_instant() {
        let r = reg();
        let i = r.call_global("Instant.now", &[]).unwrap().unwrap();
        assert!(matches!(i, Value::Instant(_)));
    }

    #[test]
    fn duration_since_positive() {
        let r = reg();
        let t1 = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let t2 = std::time::Instant::now();
        let result = r
            .call(
                TypeTag::Instant,
                "duration_since",
                &[Value::Instant(t2), Value::Instant(t1)],
            )
            .unwrap()
            .unwrap();
        let nanos = match result {
            Value::Duration(n) => n,
            _ => panic!("expected Duration"),
        };
        assert!(nanos >= 5_000_000, "expected at least 5ms, got {}ns", nanos);
    }

    #[test]
    fn instant_sub_instant() {
        let r = reg();
        let t1 = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let t2 = std::time::Instant::now();
        let result = r
            .call(
                TypeTag::Instant,
                "sub",
                &[Value::Instant(t2), Value::Instant(t1)],
            )
            .unwrap()
            .unwrap();
        assert!(matches!(result, Value::Duration(n) if n >= 5_000_000));
    }

    #[test]
    fn instant_add_duration_is_instant() {
        let r = reg();
        let t = std::time::Instant::now();
        let result = r
            .call(
                TypeTag::Instant,
                "add",
                &[Value::Instant(t), Value::Duration(1_000_000)],
            )
            .unwrap()
            .unwrap();
        assert!(matches!(result, Value::Instant(_)));
    }

    #[test]
    fn instant_sub_duration_is_instant() {
        let r = reg();
        let t = std::time::Instant::now();
        let result = r
            .call(
                TypeTag::Instant,
                "sub",
                &[Value::Instant(t), Value::Duration(1_000_000)],
            )
            .unwrap()
            .unwrap();
        assert!(matches!(result, Value::Instant(_)));
    }
}
