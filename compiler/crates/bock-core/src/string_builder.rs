//! StringBuilder type for efficient string concatenation.
//!
//! Provides a mutable buffer that avoids repeated allocation from string `+`
//! chaining. Methods: `new`, `append`, `len`, `build`, `clear`.

use std::sync::{Arc, Mutex};

use bock_interp::{BockString, BuiltinRegistry, RuntimeError, TypeTag, Value};

/// Register all StringBuilder methods and the `StringBuilder.new()` global.
pub fn register(registry: &mut BuiltinRegistry) {
    // Global constructor
    registry.register_global("StringBuilder", sb_new);

    // Instance methods
    registry.register(TypeTag::StringBuilder, "append", sb_append);
    registry.register(TypeTag::StringBuilder, "len", sb_len);
    registry.register(TypeTag::StringBuilder, "build", sb_build);
    registry.register(TypeTag::StringBuilder, "clear", sb_clear);
}

fn expect_sb(args: &[Value], method: &str) -> Result<Arc<Mutex<String>>, RuntimeError> {
    match args.first() {
        Some(Value::StringBuilder(rc)) => Ok(Arc::clone(rc)),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "StringBuilder.{method} called on {other}, expected StringBuilder"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: 1,
            got: 0,
        }),
    }
}

/// `StringBuilder()` → new empty builder.
/// `StringBuilder("initial")` → builder pre-loaded with content.
fn sb_new(args: &[Value]) -> Result<Value, RuntimeError> {
    let initial = match args.first() {
        Some(Value::String(s)) => s.as_str().to_string(),
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "StringBuilder expects optional String argument, got {other}"
            )))
        }
        None => String::new(),
    };
    Ok(Value::StringBuilder(Arc::new(Mutex::new(initial))))
}

/// `sb.append(value)` → appends the string representation, returns the builder for chaining.
fn sb_append(args: &[Value]) -> Result<Value, RuntimeError> {
    let rc = expect_sb(args, "append")?;
    let val = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: 1,
    })?;
    let s = match val {
        Value::String(s) => s.as_str().to_string(),
        other => other.to_string(),
    };
    rc.lock().unwrap().push_str(&s);
    Ok(Value::StringBuilder(rc))
}

/// `sb.len()` → character count of the accumulated content.
fn sb_len(args: &[Value]) -> Result<Value, RuntimeError> {
    let rc = expect_sb(args, "len")?;
    let count = rc.lock().unwrap().chars().count() as i64;
    Ok(Value::Int(count))
}

/// `sb.build()` → finalize into a `String` value.
fn sb_build(args: &[Value]) -> Result<Value, RuntimeError> {
    let rc = expect_sb(args, "build")?;
    let content = rc.lock().unwrap().clone();
    Ok(Value::String(BockString::new(content)))
}

/// `sb.clear()` → empty the buffer, returns the builder.
fn sb_clear(args: &[Value]) -> Result<Value, RuntimeError> {
    let rc = expect_sb(args, "clear")?;
    rc.lock().unwrap().clear();
    Ok(Value::StringBuilder(rc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> BuiltinRegistry {
        let mut r = BuiltinRegistry::new();
        register(&mut r);
        r
    }

    fn s(v: &str) -> Value {
        Value::String(BockString::new(v))
    }

    #[test]
    fn new_empty() {
        let r = reg();
        let sb = r.call_global("StringBuilder", &[]).unwrap().unwrap();
        let result = r
            .call(TypeTag::StringBuilder, "build", &[sb])
            .unwrap()
            .unwrap();
        assert_eq!(result, s(""));
    }

    #[test]
    fn new_with_initial() {
        let r = reg();
        let sb = r
            .call_global("StringBuilder", &[s("hello")])
            .unwrap()
            .unwrap();
        let result = r
            .call(TypeTag::StringBuilder, "build", &[sb])
            .unwrap()
            .unwrap();
        assert_eq!(result, s("hello"));
    }

    #[test]
    fn append_and_build() {
        let r = reg();
        let sb = r.call_global("StringBuilder", &[]).unwrap().unwrap();
        let sb = r
            .call(TypeTag::StringBuilder, "append", &[sb, s("hello")])
            .unwrap()
            .unwrap();
        let sb = r
            .call(TypeTag::StringBuilder, "append", &[sb, s(" world")])
            .unwrap()
            .unwrap();
        let result = r
            .call(TypeTag::StringBuilder, "build", &[sb])
            .unwrap()
            .unwrap();
        assert_eq!(result, s("hello world"));
    }

    #[test]
    fn append_non_string() {
        let r = reg();
        let sb = r.call_global("StringBuilder", &[]).unwrap().unwrap();
        let sb = r
            .call(TypeTag::StringBuilder, "append", &[sb, Value::Int(42)])
            .unwrap()
            .unwrap();
        let result = r
            .call(TypeTag::StringBuilder, "build", &[sb])
            .unwrap()
            .unwrap();
        assert_eq!(result, s("42"));
    }

    #[test]
    fn len_counts_chars() {
        let r = reg();
        let sb = r
            .call_global("StringBuilder", &[s("héllo")])
            .unwrap()
            .unwrap();
        let result = r
            .call(TypeTag::StringBuilder, "len", &[sb])
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Int(5));
    }

    #[test]
    fn clear_empties_buffer() {
        let r = reg();
        let sb = r
            .call_global("StringBuilder", &[s("content")])
            .unwrap()
            .unwrap();
        let sb = r
            .call(TypeTag::StringBuilder, "clear", &[sb])
            .unwrap()
            .unwrap();
        let result = r
            .call(TypeTag::StringBuilder, "build", &[sb])
            .unwrap()
            .unwrap();
        assert_eq!(result, s(""));
    }
}
