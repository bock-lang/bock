//! Duration primitive type — signed nanoseconds (±292 year range).
//!
//! Registers instance methods, arithmetic operator overloads, trait methods,
//! and associated constructor functions (`Duration.zero`, `Duration.seconds`, …).

use bock_interp::{BockString, BuiltinRegistry, RuntimeError, TypeTag, Value};

/// Register all Duration methods, operators, and associated functions.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Arithmetic trait methods (used by BinaryOp trait dispatch) ───────
    registry.register(TypeTag::Duration, "add", duration_add);
    registry.register(TypeTag::Duration, "sub", duration_sub);
    registry.register(TypeTag::Duration, "mul", duration_mul);
    registry.register(TypeTag::Duration, "div", duration_div);
    registry.register(TypeTag::Duration, "negate", duration_negate);

    // ── Comparable / Equatable ───────────────────────────────────────────
    registry.register(TypeTag::Duration, "compare", duration_compare);
    registry.register(TypeTag::Duration, "equals", duration_equals);

    // ── Displayable / Hashable ───────────────────────────────────────────
    registry.register(TypeTag::Duration, "display", duration_display);
    registry.register(TypeTag::Duration, "hash_code", duration_hash_code);

    // ── Instance methods ─────────────────────────────────────────────────
    registry.register(TypeTag::Duration, "as_nanos", duration_as_nanos);
    registry.register(TypeTag::Duration, "as_millis", duration_as_millis);
    registry.register(TypeTag::Duration, "as_seconds", duration_as_seconds);
    registry.register(TypeTag::Duration, "is_zero", duration_is_zero);
    registry.register(TypeTag::Duration, "is_negative", duration_is_negative);
    registry.register(TypeTag::Duration, "abs", duration_abs);

    // ── Associated functions (registered as qualified globals) ───────────
    registry.register_global("Duration.zero", duration_zero);
    registry.register_global("Duration.nanos", duration_ctor_nanos);
    registry.register_global("Duration.micros", duration_ctor_micros);
    registry.register_global("Duration.millis", duration_ctor_millis);
    registry.register_global("Duration.seconds", duration_ctor_seconds);
    registry.register_global("Duration.minutes", duration_ctor_minutes);
    registry.register_global("Duration.hours", duration_ctor_hours);
}

// ─── Helpers ─────────────────────────────────────────────────────────────

fn expect_duration(args: &[Value], pos: usize, method: &str) -> Result<i64, RuntimeError> {
    match args.get(pos) {
        Some(Value::Duration(n)) => Ok(*n),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Duration.{method} expects Duration, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

fn expect_int(args: &[Value], pos: usize, method: &str) -> Result<i64, RuntimeError> {
    match args.get(pos) {
        Some(Value::Int(n)) => Ok(*n),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Duration.{method} expects Int, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

fn scale(n: i64, factor: i64, ctor: &str) -> Result<Value, RuntimeError> {
    match n.checked_mul(factor) {
        Some(nanos) => Ok(Value::Duration(nanos)),
        None => Err(RuntimeError::TypeError(format!(
            "Duration.{ctor}: overflow ({n} exceeds the ±292 year range)"
        ))),
    }
}

// ─── Associated functions ────────────────────────────────────────────────

fn duration_zero(_args: &[Value]) -> Result<Value, RuntimeError> {
    Ok(Value::Duration(0))
}

fn duration_ctor_nanos(args: &[Value]) -> Result<Value, RuntimeError> {
    let n = expect_int(args, 0, "nanos")?;
    Ok(Value::Duration(n))
}

fn duration_ctor_micros(args: &[Value]) -> Result<Value, RuntimeError> {
    let n = expect_int(args, 0, "micros")?;
    scale(n, 1_000, "micros")
}

fn duration_ctor_millis(args: &[Value]) -> Result<Value, RuntimeError> {
    let n = expect_int(args, 0, "millis")?;
    scale(n, 1_000_000, "millis")
}

fn duration_ctor_seconds(args: &[Value]) -> Result<Value, RuntimeError> {
    let n = expect_int(args, 0, "seconds")?;
    scale(n, 1_000_000_000, "seconds")
}

fn duration_ctor_minutes(args: &[Value]) -> Result<Value, RuntimeError> {
    let n = expect_int(args, 0, "minutes")?;
    scale(n, 60_000_000_000, "minutes")
}

fn duration_ctor_hours(args: &[Value]) -> Result<Value, RuntimeError> {
    let n = expect_int(args, 0, "hours")?;
    scale(n, 3_600_000_000_000, "hours")
}

// ─── Arithmetic ──────────────────────────────────────────────────────────

fn duration_add(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_duration(args, 0, "add")?;
    let b = expect_duration(args, 1, "add")?;
    a.checked_add(b)
        .map(Value::Duration)
        .ok_or(RuntimeError::IntOverflow)
}

fn duration_sub(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_duration(args, 0, "sub")?;
    let b = expect_duration(args, 1, "sub")?;
    a.checked_sub(b)
        .map(Value::Duration)
        .ok_or(RuntimeError::IntOverflow)
}

fn duration_mul(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_duration(args, 0, "mul")?;
    let factor = expect_int(args, 1, "mul")?;
    a.checked_mul(factor)
        .map(Value::Duration)
        .ok_or(RuntimeError::IntOverflow)
}

fn duration_div(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_duration(args, 0, "div")?;
    let divisor = expect_int(args, 1, "div")?;
    if divisor == 0 {
        return Err(RuntimeError::DivisionByZero);
    }
    Ok(Value::Duration(a / divisor))
}

fn duration_negate(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_duration(args, 0, "negate")?;
    a.checked_neg()
        .map(Value::Duration)
        .ok_or(RuntimeError::IntOverflow)
}

// ─── Comparable / Equatable ──────────────────────────────────────────────

fn duration_compare(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_duration(args, 0, "compare")?;
    let b = expect_duration(args, 1, "compare")?;
    Ok(Value::Int(a.cmp(&b) as i64))
}

fn duration_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_duration(args, 0, "equals")?;
    let b = expect_duration(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

// ─── Displayable / Hashable ──────────────────────────────────────────────

fn duration_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let d = expect_duration(args, 0, "display")?;
    Ok(Value::String(BockString::new(Value::Duration(d).to_string())))
}

fn duration_hash_code(args: &[Value]) -> Result<Value, RuntimeError> {
    use std::hash::{Hash, Hasher};
    let d = expect_duration(args, 0, "hash_code")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    d.hash(&mut hasher);
    Ok(Value::Int(hasher.finish() as i64))
}

// ─── Instance methods ────────────────────────────────────────────────────

fn duration_as_nanos(args: &[Value]) -> Result<Value, RuntimeError> {
    let d = expect_duration(args, 0, "as_nanos")?;
    Ok(Value::Int(d))
}

fn duration_as_millis(args: &[Value]) -> Result<Value, RuntimeError> {
    let d = expect_duration(args, 0, "as_millis")?;
    Ok(Value::Int(d / 1_000_000))
}

fn duration_as_seconds(args: &[Value]) -> Result<Value, RuntimeError> {
    let d = expect_duration(args, 0, "as_seconds")?;
    Ok(Value::Int(d / 1_000_000_000))
}

fn duration_is_zero(args: &[Value]) -> Result<Value, RuntimeError> {
    let d = expect_duration(args, 0, "is_zero")?;
    Ok(Value::Bool(d == 0))
}

fn duration_is_negative(args: &[Value]) -> Result<Value, RuntimeError> {
    let d = expect_duration(args, 0, "is_negative")?;
    Ok(Value::Bool(d < 0))
}

fn duration_abs(args: &[Value]) -> Result<Value, RuntimeError> {
    let d = expect_duration(args, 0, "abs")?;
    d.checked_abs()
        .map(Value::Duration)
        .ok_or(RuntimeError::IntOverflow)
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
    fn zero_constructor() {
        let r = reg();
        let d = r.call_global("Duration.zero", &[]).unwrap().unwrap();
        assert_eq!(d, Value::Duration(0));
    }

    #[test]
    fn millis_constructor() {
        let r = reg();
        let d = r
            .call_global("Duration.millis", &[Value::Int(500)])
            .unwrap()
            .unwrap();
        assert_eq!(d, Value::Duration(500_000_000));
    }

    #[test]
    fn seconds_constructor() {
        let r = reg();
        let d = r
            .call_global("Duration.seconds", &[Value::Int(2)])
            .unwrap()
            .unwrap();
        assert_eq!(d, Value::Duration(2_000_000_000));
    }

    #[test]
    fn hours_overflow() {
        let r = reg();
        let result = r.call_global("Duration.hours", &[Value::Int(i64::MAX)]);
        assert!(matches!(result, Some(Err(RuntimeError::TypeError(_)))));
    }

    #[test]
    fn add_durations() {
        let r = reg();
        let result = r
            .call(
                TypeTag::Duration,
                "add",
                &[Value::Duration(1_000_000), Value::Duration(2_000_000)],
            )
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Duration(3_000_000));
    }

    #[test]
    fn mul_duration_int() {
        let r = reg();
        let result = r
            .call(
                TypeTag::Duration,
                "mul",
                &[Value::Duration(500_000), Value::Int(3)],
            )
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Duration(1_500_000));
    }

    #[test]
    fn div_duration_int() {
        let r = reg();
        let result = r
            .call(
                TypeTag::Duration,
                "div",
                &[Value::Duration(1_000), Value::Int(4)],
            )
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Duration(250));
    }

    #[test]
    fn div_by_zero() {
        let r = reg();
        let result = r.call(
            TypeTag::Duration,
            "div",
            &[Value::Duration(1_000), Value::Int(0)],
        );
        assert!(matches!(result, Some(Err(RuntimeError::DivisionByZero))));
    }

    #[test]
    fn as_millis() {
        let r = reg();
        let result = r
            .call(TypeTag::Duration, "as_millis", &[Value::Duration(1_500_000_000)])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Int(1500));
    }

    #[test]
    fn is_zero() {
        let r = reg();
        let result = r
            .call(TypeTag::Duration, "is_zero", &[Value::Duration(0)])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn is_negative() {
        let r = reg();
        let result = r
            .call(TypeTag::Duration, "is_negative", &[Value::Duration(-1)])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn abs_negative() {
        let r = reg();
        let result = r
            .call(TypeTag::Duration, "abs", &[Value::Duration(-500)])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Duration(500));
    }

    #[test]
    fn compare_less() {
        let r = reg();
        let result = r
            .call(
                TypeTag::Duration,
                "compare",
                &[Value::Duration(100), Value::Duration(200)],
            )
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Int(-1));
    }

    #[test]
    fn display_formats() {
        assert_eq!(Value::Duration(0).to_string(), "0s");
        assert_eq!(Value::Duration(1_500_000_000).to_string(), "1.5s");
        assert_eq!(Value::Duration(500_000_000).to_string(), "500ms");
        assert_eq!(Value::Duration(250_000_000).to_string(), "250ms");
        assert_eq!(Value::Duration(-500_000_000).to_string(), "-500ms");
    }
}
