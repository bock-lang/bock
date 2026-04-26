//! Char primitive type methods and trait implementations.

use bock_interp::{BockString, BuiltinRegistry, RuntimeError, TypeTag, Value};

/// Register all Char methods and trait implementations.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Comparable trait ─────────────────────────────────────────────────
    registry.register(TypeTag::Char, "compare", char_compare);

    // ── Equatable trait ──────────────────────────────────────────────────
    registry.register(TypeTag::Char, "equals", char_equals);

    // ── Hashable trait ───────────────────────────────────────────────────
    registry.register(TypeTag::Char, "hash_code", char_hash_code);

    // ── Displayable trait ────────────────────────────────────────────────
    registry.register(TypeTag::Char, "display", char_display);

    // ── Type-specific methods ────────────────────────────────────────────
    registry.register(TypeTag::Char, "to_upper", char_to_upper);
    registry.register(TypeTag::Char, "to_lower", char_to_lower);
    registry.register(TypeTag::Char, "is_alpha", char_is_alpha);
    registry.register(TypeTag::Char, "is_digit", char_is_digit);
    registry.register(TypeTag::Char, "is_whitespace", char_is_whitespace);
    registry.register(TypeTag::Char, "to_int", char_to_int);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_char(args: &[Value], pos: usize, method: &str) -> Result<char, RuntimeError> {
    match args.get(pos) {
        Some(Value::Char(c)) => Ok(*c),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Char.{method} expects Char, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// ─── Comparable ───────────────────────────────────────────────────────────────

fn char_compare(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_char(args, 0, "compare")?;
    let b = expect_char(args, 1, "compare")?;
    Ok(Value::Int(a.cmp(&b) as i64))
}

// ─── Equatable ────────────────────────────────────────────────────────────────

fn char_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_char(args, 0, "equals")?;
    let b = expect_char(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

// ─── Hashable ─────────────────────────────────────────────────────────────────

fn char_hash_code(args: &[Value]) -> Result<Value, RuntimeError> {
    use std::hash::{Hash, Hasher};
    let a = expect_char(args, 0, "hash_code")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    a.hash(&mut hasher);
    Ok(Value::Int(hasher.finish() as i64))
}

// ─── Displayable ──────────────────────────────────────────────────────────────

fn char_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_char(args, 0, "display")?;
    Ok(Value::String(BockString::new(a.to_string())))
}

// ─── Type-specific methods ────────────────────────────────────────────────────

fn char_to_upper(args: &[Value]) -> Result<Value, RuntimeError> {
    let c = expect_char(args, 0, "to_upper")?;
    // to_uppercase can yield multiple chars; take the first for simple cases.
    let upper: String = c.to_uppercase().collect();
    if upper.chars().count() == 1 {
        Ok(Value::Char(
            upper
                .chars()
                .next()
                .expect("uppercase always yields at least one char"),
        ))
    } else {
        Ok(Value::String(BockString::new(upper)))
    }
}

fn char_to_lower(args: &[Value]) -> Result<Value, RuntimeError> {
    let c = expect_char(args, 0, "to_lower")?;
    let lower: String = c.to_lowercase().collect();
    if lower.chars().count() == 1 {
        Ok(Value::Char(
            lower
                .chars()
                .next()
                .expect("lowercase always yields at least one char"),
        ))
    } else {
        Ok(Value::String(BockString::new(lower)))
    }
}

fn char_is_alpha(args: &[Value]) -> Result<Value, RuntimeError> {
    let c = expect_char(args, 0, "is_alpha")?;
    Ok(Value::Bool(c.is_alphabetic()))
}

fn char_is_digit(args: &[Value]) -> Result<Value, RuntimeError> {
    let c = expect_char(args, 0, "is_digit")?;
    Ok(Value::Bool(c.is_ascii_digit()))
}

fn char_is_whitespace(args: &[Value]) -> Result<Value, RuntimeError> {
    let c = expect_char(args, 0, "is_whitespace")?;
    Ok(Value::Bool(c.is_whitespace()))
}

fn char_to_int(args: &[Value]) -> Result<Value, RuntimeError> {
    let c = expect_char(args, 0, "to_int")?;
    Ok(Value::Int(c as i64))
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
    fn compare_less() {
        let r = reg();
        let result = r.call(
            TypeTag::Char,
            "compare",
            &[Value::Char('a'), Value::Char('z')],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Int(-1));
    }

    #[test]
    fn equals_true() {
        let r = reg();
        let result = r.call(
            TypeTag::Char,
            "equals",
            &[Value::Char('x'), Value::Char('x')],
        );
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn display_char() {
        let r = reg();
        let result = r.call(TypeTag::Char, "display", &[Value::Char('A')]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::String(BockString::new("A"))
        );
    }

    #[test]
    fn to_upper_ok() {
        let r = reg();
        let result = r.call(TypeTag::Char, "to_upper", &[Value::Char('a')]);
        assert_eq!(result.unwrap().unwrap(), Value::Char('A'));
    }

    #[test]
    fn to_lower_ok() {
        let r = reg();
        let result = r.call(TypeTag::Char, "to_lower", &[Value::Char('A')]);
        assert_eq!(result.unwrap().unwrap(), Value::Char('a'));
    }

    #[test]
    fn is_alpha_true() {
        let r = reg();
        let result = r.call(TypeTag::Char, "is_alpha", &[Value::Char('x')]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn is_alpha_false() {
        let r = reg();
        let result = r.call(TypeTag::Char, "is_alpha", &[Value::Char('5')]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn is_digit_true() {
        let r = reg();
        let result = r.call(TypeTag::Char, "is_digit", &[Value::Char('5')]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn is_whitespace_true() {
        let r = reg();
        let result = r.call(TypeTag::Char, "is_whitespace", &[Value::Char(' ')]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn to_int_ok() {
        let r = reg();
        let result = r.call(TypeTag::Char, "to_int", &[Value::Char('A')]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(65));
    }

    #[test]
    fn hash_code_deterministic() {
        let r = reg();
        let h1 = r
            .call(TypeTag::Char, "hash_code", &[Value::Char('x')])
            .unwrap()
            .unwrap();
        let h2 = r
            .call(TypeTag::Char, "hash_code", &[Value::Char('x')])
            .unwrap()
            .unwrap();
        assert_eq!(h1, h2);
    }
}
