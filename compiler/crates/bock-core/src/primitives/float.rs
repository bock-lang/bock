//! Float primitive type methods and trait implementations.

use bock_interp::{BockString, BuiltinRegistry, OrdF64, RuntimeError, TypeTag, Value};

/// Register all Float methods and trait implementations.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Arithmetic trait methods ─────────────────────────────────────────
    registry.register(TypeTag::Float, "add", float_add);
    registry.register(TypeTag::Float, "sub", float_sub);
    registry.register(TypeTag::Float, "mul", float_mul);
    registry.register(TypeTag::Float, "div", float_div);
    registry.register(TypeTag::Float, "rem", float_rem);
    registry.register(TypeTag::Float, "pow", float_pow);
    registry.register(TypeTag::Float, "negate", float_negate);

    // ── Comparable trait ─────────────────────────────────────────────────
    registry.register(TypeTag::Float, "compare", float_compare);

    // ── Equatable trait ──────────────────────────────────────────────────
    registry.register(TypeTag::Float, "equals", float_equals);

    // ── Hashable trait ───────────────────────────────────────────────────
    registry.register(TypeTag::Float, "hash_code", float_hash_code);

    // ── Displayable trait ────────────────────────────────────────────────
    registry.register(TypeTag::Float, "display", float_display);

    // ── Type-specific methods ────────────────────────────────────────────
    registry.register(TypeTag::Float, "abs", float_abs);
    registry.register(TypeTag::Float, "floor", float_floor);
    registry.register(TypeTag::Float, "ceil", float_ceil);
    registry.register(TypeTag::Float, "round", float_round);
    registry.register(TypeTag::Float, "to_int", float_to_int);
    registry.register(TypeTag::Float, "sqrt", float_sqrt);
    registry.register(TypeTag::Float, "is_nan", float_is_nan);
    registry.register(TypeTag::Float, "is_infinite", float_is_infinite);
    registry.register(TypeTag::Float, "min", float_min);
    registry.register(TypeTag::Float, "max", float_max);
    registry.register(TypeTag::Float, "clamp", float_clamp);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_float(args: &[Value], pos: usize, method: &str) -> Result<f64, RuntimeError> {
    match args.get(pos) {
        Some(Value::Float(v)) => Ok(v.0),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Float.{method} expects Float, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// ─── Arithmetic ───────────────────────────────────────────────────────────────

fn float_add(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "add")?;
    let b = expect_float(args, 1, "add")?;
    Ok(Value::Float(OrdF64(a + b)))
}

fn float_sub(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "sub")?;
    let b = expect_float(args, 1, "sub")?;
    Ok(Value::Float(OrdF64(a - b)))
}

fn float_mul(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "mul")?;
    let b = expect_float(args, 1, "mul")?;
    Ok(Value::Float(OrdF64(a * b)))
}

fn float_div(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "div")?;
    let b = expect_float(args, 1, "div")?;
    Ok(Value::Float(OrdF64(a / b)))
}

fn float_rem(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "rem")?;
    let b = expect_float(args, 1, "rem")?;
    Ok(Value::Float(OrdF64(a % b)))
}

fn float_pow(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "pow")?;
    let b = expect_float(args, 1, "pow")?;
    Ok(Value::Float(OrdF64(a.powf(b))))
}

fn float_negate(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "negate")?;
    Ok(Value::Float(OrdF64(-a)))
}

// ─── Comparable ───────────────────────────────────────────────────────────────

fn float_compare(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "compare")?;
    let b = expect_float(args, 1, "compare")?;
    Ok(Value::Int(a.total_cmp(&b) as i64))
}

// ─── Equatable ────────────────────────────────────────────────────────────────

fn float_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "equals")?;
    let b = expect_float(args, 1, "equals")?;
    Ok(Value::Bool(OrdF64(a) == OrdF64(b)))
}

// ─── Hashable ─────────────────────────────────────────────────────────────────

fn float_hash_code(args: &[Value]) -> Result<Value, RuntimeError> {
    use std::hash::{Hash, Hasher};
    let a = expect_float(args, 0, "hash_code")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    OrdF64(a).hash(&mut hasher);
    Ok(Value::Int(hasher.finish() as i64))
}

// ─── Displayable ──────────────────────────────────────────────────────────────

fn float_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "display")?;
    Ok(Value::String(BockString::new(format!("{a}"))))
}

// ─── Type-specific methods ────────────────────────────────────────────────────

fn float_abs(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "abs")?;
    Ok(Value::Float(OrdF64(a.abs())))
}

fn float_floor(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "floor")?;
    Ok(Value::Float(OrdF64(a.floor())))
}

fn float_ceil(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "ceil")?;
    Ok(Value::Float(OrdF64(a.ceil())))
}

fn float_round(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "round")?;
    Ok(Value::Float(OrdF64(a.round())))
}

fn float_to_int(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "to_int")?;
    if a.is_nan() || a.is_infinite() {
        return Err(RuntimeError::TypeError(
            "cannot convert NaN or Infinity to Int".to_string(),
        ));
    }
    Ok(Value::Int(a as i64))
}

fn float_sqrt(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "sqrt")?;
    Ok(Value::Float(OrdF64(a.sqrt())))
}

fn float_is_nan(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "is_nan")?;
    Ok(Value::Bool(a.is_nan()))
}

fn float_is_infinite(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "is_infinite")?;
    Ok(Value::Bool(a.is_infinite()))
}

fn float_min(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "min")?;
    let b = expect_float(args, 1, "min")?;
    Ok(Value::Float(OrdF64(a.min(b))))
}

fn float_max(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "max")?;
    let b = expect_float(args, 1, "max")?;
    Ok(Value::Float(OrdF64(a.max(b))))
}

fn float_clamp(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_float(args, 0, "clamp")?;
    let lo = expect_float(args, 1, "clamp")?;
    let hi = expect_float(args, 2, "clamp")?;
    Ok(Value::Float(OrdF64(a.clamp(lo, hi))))
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

    fn f(v: f64) -> Value {
        Value::Float(OrdF64(v))
    }

    #[test]
    fn add_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "add", &[f(1.5), f(2.5)]);
        assert_eq!(result.unwrap().unwrap(), f(4.0));
    }

    #[test]
    fn sub_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "sub", &[f(5.0), f(2.0)]);
        assert_eq!(result.unwrap().unwrap(), f(3.0));
    }

    #[test]
    fn mul_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "mul", &[f(3.0), f(4.0)]);
        assert_eq!(result.unwrap().unwrap(), f(12.0));
    }

    #[test]
    fn div_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "div", &[f(10.0), f(4.0)]);
        assert_eq!(result.unwrap().unwrap(), f(2.5));
    }

    #[test]
    fn pow_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "pow", &[f(2.0), f(3.0)]);
        assert_eq!(result.unwrap().unwrap(), f(8.0));
    }

    #[test]
    fn negate_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "negate", &[f(3.14)]);
        assert_eq!(result.unwrap().unwrap(), f(-3.14));
    }

    #[test]
    fn compare_less() {
        let r = reg();
        let result = r.call(TypeTag::Float, "compare", &[f(1.0), f(2.0)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(-1));
    }

    #[test]
    fn equals_true() {
        let r = reg();
        let result = r.call(TypeTag::Float, "equals", &[f(1.5), f(1.5)]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn display_float() {
        let r = reg();
        let result = r.call(TypeTag::Float, "display", &[f(3.14)]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::String(BockString::new("3.14"))
        );
    }

    #[test]
    fn abs_negative() {
        let r = reg();
        let result = r.call(TypeTag::Float, "abs", &[f(-42.5)]);
        assert_eq!(result.unwrap().unwrap(), f(42.5));
    }

    #[test]
    fn floor_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "floor", &[f(3.7)]);
        assert_eq!(result.unwrap().unwrap(), f(3.0));
    }

    #[test]
    fn ceil_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "ceil", &[f(3.2)]);
        assert_eq!(result.unwrap().unwrap(), f(4.0));
    }

    #[test]
    fn round_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "round", &[f(3.5)]);
        assert_eq!(result.unwrap().unwrap(), f(4.0));
    }

    #[test]
    fn to_int_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "to_int", &[f(42.9)]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(42));
    }

    #[test]
    fn to_int_nan_error() {
        let r = reg();
        let result = r.call(TypeTag::Float, "to_int", &[f(f64::NAN)]);
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn sqrt_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "sqrt", &[f(9.0)]);
        assert_eq!(result.unwrap().unwrap(), f(3.0));
    }

    #[test]
    fn is_nan_true() {
        let r = reg();
        let result = r.call(TypeTag::Float, "is_nan", &[f(f64::NAN)]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn is_infinite_true() {
        let r = reg();
        let result = r.call(TypeTag::Float, "is_infinite", &[f(f64::INFINITY)]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn min_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "min", &[f(3.0), f(7.0)]);
        assert_eq!(result.unwrap().unwrap(), f(3.0));
    }

    #[test]
    fn max_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "max", &[f(3.0), f(7.0)]);
        assert_eq!(result.unwrap().unwrap(), f(7.0));
    }

    #[test]
    fn clamp_ok() {
        let r = reg();
        let result = r.call(TypeTag::Float, "clamp", &[f(15.0), f(0.0), f(10.0)]);
        assert_eq!(result.unwrap().unwrap(), f(10.0));
    }

    #[test]
    fn hash_code_deterministic() {
        let r = reg();
        let h1 = r
            .call(TypeTag::Float, "hash_code", &[f(3.14)])
            .unwrap()
            .unwrap();
        let h2 = r
            .call(TypeTag::Float, "hash_code", &[f(3.14)])
            .unwrap()
            .unwrap();
        assert_eq!(h1, h2);
    }
}
