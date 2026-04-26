//! Int primitive type methods and trait implementations.

use bock_interp::{BockString, BuiltinRegistry, OrdF64, RuntimeError, TypeTag, Value};

/// Register all Int methods and trait implementations.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Arithmetic trait methods ─────────────────────────────────────────
    registry.register(TypeTag::Int, "add", int_add);
    registry.register(TypeTag::Int, "sub", int_sub);
    registry.register(TypeTag::Int, "mul", int_mul);
    registry.register(TypeTag::Int, "div", int_div);
    registry.register(TypeTag::Int, "rem", int_rem);
    registry.register(TypeTag::Int, "pow", int_pow);
    registry.register(TypeTag::Int, "negate", int_negate);

    // ── Comparable trait ─────────────────────────────────────────────────
    registry.register(TypeTag::Int, "compare", int_compare);

    // ── Equatable trait ──────────────────────────────────────────────────
    registry.register(TypeTag::Int, "equals", int_equals);

    // ── Hashable trait ───────────────────────────────────────────────────
    registry.register(TypeTag::Int, "hash_code", int_hash_code);

    // ── Displayable trait ────────────────────────────────────────────────
    registry.register(TypeTag::Int, "display", int_display);

    // ── Type-specific methods ────────────────────────────────────────────
    registry.register(TypeTag::Int, "abs", int_abs);
    registry.register(TypeTag::Int, "to_float", int_to_float);
    registry.register(TypeTag::Int, "min", int_min);
    registry.register(TypeTag::Int, "max", int_max);
    registry.register(TypeTag::Int, "clamp", int_clamp);
    registry.register(TypeTag::Int, "shift_left", int_shift_left);
    registry.register(TypeTag::Int, "shift_right", int_shift_right);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_int(args: &[Value], pos: usize, method: &str) -> Result<i64, RuntimeError> {
    match args.get(pos) {
        Some(Value::Int(v)) => Ok(*v),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Int.{method} expects Int, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// ─── Arithmetic ───────────────────────────────────────────────────────────────

fn int_add(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "add")?;
    let b = expect_int(args, 1, "add")?;
    a.checked_add(b)
        .map(Value::Int)
        .ok_or(RuntimeError::IntOverflow)
}

fn int_sub(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "sub")?;
    let b = expect_int(args, 1, "sub")?;
    a.checked_sub(b)
        .map(Value::Int)
        .ok_or(RuntimeError::IntOverflow)
}

fn int_mul(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "mul")?;
    let b = expect_int(args, 1, "mul")?;
    a.checked_mul(b)
        .map(Value::Int)
        .ok_or(RuntimeError::IntOverflow)
}

fn int_div(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "div")?;
    let b = expect_int(args, 1, "div")?;
    if b == 0 {
        return Err(RuntimeError::DivisionByZero);
    }
    Ok(Value::Int(a / b))
}

fn int_rem(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "rem")?;
    let b = expect_int(args, 1, "rem")?;
    if b == 0 {
        return Err(RuntimeError::DivisionByZero);
    }
    Ok(Value::Int(a % b))
}

fn int_pow(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "pow")?;
    let b = expect_int(args, 1, "pow")?;
    if b < 0 {
        return Err(RuntimeError::TypeError(
            "negative integer exponent".to_string(),
        ));
    }
    if b > u32::MAX as i64 {
        return Err(RuntimeError::IntOverflow);
    }
    a.checked_pow(b as u32)
        .map(Value::Int)
        .ok_or(RuntimeError::IntOverflow)
}

fn int_negate(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "negate")?;
    a.checked_neg()
        .map(Value::Int)
        .ok_or(RuntimeError::IntOverflow)
}

// ─── Comparable ───────────────────────────────────────────────────────────────

/// Returns -1, 0, or 1 as an Int representing the ordering.
fn int_compare(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "compare")?;
    let b = expect_int(args, 1, "compare")?;
    Ok(Value::Int(a.cmp(&b) as i64))
}

// ─── Equatable ────────────────────────────────────────────────────────────────

fn int_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "equals")?;
    let b = expect_int(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

// ─── Hashable ─────────────────────────────────────────────────────────────────

fn int_hash_code(args: &[Value]) -> Result<Value, RuntimeError> {
    use std::hash::{Hash, Hasher};
    let a = expect_int(args, 0, "hash_code")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    a.hash(&mut hasher);
    Ok(Value::Int(hasher.finish() as i64))
}

// ─── Displayable ──────────────────────────────────────────────────────────────

fn int_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "display")?;
    Ok(Value::String(BockString::new(a.to_string())))
}

// ─── Type-specific methods ────────────────────────────────────────────────────

fn int_abs(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "abs")?;
    a.checked_abs()
        .map(Value::Int)
        .ok_or(RuntimeError::IntOverflow)
}

fn int_to_float(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "to_float")?;
    Ok(Value::Float(OrdF64(a as f64)))
}

fn int_min(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "min")?;
    let b = expect_int(args, 1, "min")?;
    Ok(Value::Int(a.min(b)))
}

fn int_max(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "max")?;
    let b = expect_int(args, 1, "max")?;
    Ok(Value::Int(a.max(b)))
}

fn int_clamp(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "clamp")?;
    let lo = expect_int(args, 1, "clamp")?;
    let hi = expect_int(args, 2, "clamp")?;
    Ok(Value::Int(a.clamp(lo, hi)))
}

fn int_shift_left(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "shift_left")?;
    let b = expect_int(args, 1, "shift_left")?;
    if !(0..64).contains(&b) {
        return Err(RuntimeError::TypeError(format!(
            "shift amount out of range: {b}"
        )));
    }
    Ok(Value::Int(a << b))
}

fn int_shift_right(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_int(args, 0, "shift_right")?;
    let b = expect_int(args, 1, "shift_right")?;
    if !(0..64).contains(&b) {
        return Err(RuntimeError::TypeError(format!(
            "shift amount out of range: {b}"
        )));
    }
    Ok(Value::Int(a >> b))
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

    #[test]
    fn add_overflow() {
        let r = reg();
        let result = r.call(TypeTag::Int, "add", &[Value::Int(i64::MAX), Value::Int(1)]);
        assert!(matches!(result, Some(Err(RuntimeError::IntOverflow))));
    }

    #[test]
    fn add_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "add", &[Value::Int(3), Value::Int(4)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(7));
    }

    #[test]
    fn sub_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "sub", &[Value::Int(10), Value::Int(3)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(7));
    }

    #[test]
    fn mul_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "mul", &[Value::Int(6), Value::Int(7)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(42));
    }

    #[test]
    fn div_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "div", &[Value::Int(10), Value::Int(3)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(3));
    }

    #[test]
    fn div_by_zero() {
        let r = reg();
        let result = r.call(TypeTag::Int, "div", &[Value::Int(1), Value::Int(0)]);
        assert!(matches!(result, Some(Err(RuntimeError::DivisionByZero))));
    }

    #[test]
    fn rem_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "rem", &[Value::Int(10), Value::Int(3)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(1));
    }

    #[test]
    fn pow_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "pow", &[Value::Int(2), Value::Int(10)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(1024));
    }

    #[test]
    fn negate_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "negate", &[Value::Int(42)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(-42));
    }

    #[test]
    fn compare_less() {
        let r = reg();
        let result = r.call(TypeTag::Int, "compare", &[Value::Int(1), Value::Int(2)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(-1));
    }

    #[test]
    fn compare_equal() {
        let r = reg();
        let result = r.call(TypeTag::Int, "compare", &[Value::Int(5), Value::Int(5)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(0));
    }

    #[test]
    fn compare_greater() {
        let r = reg();
        let result = r.call(TypeTag::Int, "compare", &[Value::Int(5), Value::Int(2)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(1));
    }

    #[test]
    fn equals_true() {
        let r = reg();
        let result = r.call(TypeTag::Int, "equals", &[Value::Int(42), Value::Int(42)]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn equals_false() {
        let r = reg();
        let result = r.call(TypeTag::Int, "equals", &[Value::Int(1), Value::Int(2)]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn display_int() {
        let r = reg();
        let result = r.call(TypeTag::Int, "display", &[Value::Int(42)]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::String(BockString::new("42"))
        );
    }

    #[test]
    fn abs_positive() {
        let r = reg();
        let result = r.call(TypeTag::Int, "abs", &[Value::Int(-42)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(42));
    }

    #[test]
    fn to_float_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "to_float", &[Value::Int(42)]);
        assert_eq!(result.unwrap().unwrap(), Value::Float(OrdF64(42.0)));
    }

    #[test]
    fn min_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "min", &[Value::Int(3), Value::Int(7)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(3));
    }

    #[test]
    fn max_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "max", &[Value::Int(3), Value::Int(7)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(7));
    }

    #[test]
    fn clamp_ok() {
        let r = reg();
        let result = r.call(
            TypeTag::Int,
            "clamp",
            &[Value::Int(15), Value::Int(0), Value::Int(10)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Int(10));
    }

    #[test]
    fn hash_code_deterministic() {
        let r = reg();
        let h1 = r
            .call(TypeTag::Int, "hash_code", &[Value::Int(42)])
            .unwrap()
            .unwrap();
        let h2 = r
            .call(TypeTag::Int, "hash_code", &[Value::Int(42)])
            .unwrap()
            .unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn shift_left_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "shift_left", &[Value::Int(1), Value::Int(4)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(16));
    }

    #[test]
    fn shift_right_ok() {
        let r = reg();
        let result = r.call(TypeTag::Int, "shift_right", &[Value::Int(8), Value::Int(2)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(2));
    }

    #[test]
    fn shift_left_zero() {
        let r = reg();
        let result = r.call(TypeTag::Int, "shift_left", &[Value::Int(42), Value::Int(0)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(42));
    }

    #[test]
    fn shift_right_negative_is_arithmetic() {
        let r = reg();
        // Arithmetic right shift: -8 >> 1 == -4
        let result = r.call(
            TypeTag::Int,
            "shift_right",
            &[Value::Int(-8), Value::Int(1)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Int(-4));
    }

    #[test]
    fn shift_out_of_range() {
        let r = reg();
        let result = r.call(TypeTag::Int, "shift_left", &[Value::Int(1), Value::Int(64)]);
        assert!(matches!(result, Some(Err(RuntimeError::TypeError(_)))));
        let result = r.call(
            TypeTag::Int,
            "shift_right",
            &[Value::Int(1), Value::Int(-1)],
        );
        assert!(matches!(result, Some(Err(RuntimeError::TypeError(_)))));
    }
}
