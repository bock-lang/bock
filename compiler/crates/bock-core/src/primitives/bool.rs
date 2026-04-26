//! Bool primitive type methods and trait implementations.

use bock_interp::{BockString, BuiltinRegistry, RuntimeError, TypeTag, Value};

/// Register all Bool methods and trait implementations.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Comparable trait ─────────────────────────────────────────────────
    registry.register(TypeTag::Bool, "compare", bool_compare);

    // ── Equatable trait ──────────────────────────────────────────────────
    registry.register(TypeTag::Bool, "equals", bool_equals);

    // ── Hashable trait ───────────────────────────────────────────────────
    registry.register(TypeTag::Bool, "hash_code", bool_hash_code);

    // ── Displayable trait ────────────────────────────────────────────────
    registry.register(TypeTag::Bool, "display", bool_display);

    // ── Type-specific methods ────────────────────────────────────────────
    registry.register(TypeTag::Bool, "negate", bool_negate);
    registry.register(TypeTag::Bool, "to_int", bool_to_int);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_bool(args: &[Value], pos: usize, method: &str) -> Result<bool, RuntimeError> {
    match args.get(pos) {
        Some(Value::Bool(v)) => Ok(*v),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Bool.{method} expects Bool, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// ─── Comparable ───────────────────────────────────────────────────────────────

fn bool_compare(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_bool(args, 0, "compare")?;
    let b = expect_bool(args, 1, "compare")?;
    Ok(Value::Int(a.cmp(&b) as i64))
}

// ─── Equatable ────────────────────────────────────────────────────────────────

fn bool_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_bool(args, 0, "equals")?;
    let b = expect_bool(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

// ─── Hashable ─────────────────────────────────────────────────────────────────

fn bool_hash_code(args: &[Value]) -> Result<Value, RuntimeError> {
    use std::hash::{Hash, Hasher};
    let a = expect_bool(args, 0, "hash_code")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    a.hash(&mut hasher);
    Ok(Value::Int(hasher.finish() as i64))
}

// ─── Displayable ──────────────────────────────────────────────────────────────

fn bool_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_bool(args, 0, "display")?;
    Ok(Value::String(BockString::new(if a {
        "true"
    } else {
        "false"
    })))
}

// ─── Type-specific methods ────────────────────────────────────────────────────

fn bool_negate(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_bool(args, 0, "negate")?;
    Ok(Value::Bool(!a))
}

fn bool_to_int(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_bool(args, 0, "to_int")?;
    Ok(Value::Int(if a { 1 } else { 0 }))
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
    fn compare_false_less_than_true() {
        let r = reg();
        let result = r.call(
            TypeTag::Bool,
            "compare",
            &[Value::Bool(false), Value::Bool(true)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Int(-1));
    }

    #[test]
    fn equals_true() {
        let r = reg();
        let result = r.call(
            TypeTag::Bool,
            "equals",
            &[Value::Bool(true), Value::Bool(true)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn equals_false() {
        let r = reg();
        let result = r.call(
            TypeTag::Bool,
            "equals",
            &[Value::Bool(true), Value::Bool(false)],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn display_true() {
        let r = reg();
        let result = r.call(TypeTag::Bool, "display", &[Value::Bool(true)]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::String(BockString::new("true"))
        );
    }

    #[test]
    fn display_false() {
        let r = reg();
        let result = r.call(TypeTag::Bool, "display", &[Value::Bool(false)]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::String(BockString::new("false"))
        );
    }

    #[test]
    fn negate_ok() {
        let r = reg();
        let result = r.call(TypeTag::Bool, "negate", &[Value::Bool(true)]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn to_int_ok() {
        let r = reg();
        let true_result = r.call(TypeTag::Bool, "to_int", &[Value::Bool(true)]);
        assert_eq!(true_result.unwrap().unwrap(), Value::Int(1));
        let false_result = r.call(TypeTag::Bool, "to_int", &[Value::Bool(false)]);
        assert_eq!(false_result.unwrap().unwrap(), Value::Int(0));
    }

    #[test]
    fn hash_code_deterministic() {
        let r = reg();
        let h1 = r
            .call(TypeTag::Bool, "hash_code", &[Value::Bool(true)])
            .unwrap()
            .unwrap();
        let h2 = r
            .call(TypeTag::Bool, "hash_code", &[Value::Bool(true)])
            .unwrap()
            .unwrap();
        assert_eq!(h1, h2);
    }
}
