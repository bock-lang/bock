//! Optional type methods and trait implementations.

use bock_interp::{BockString, BuiltinRegistry, CallbackInvoker, RuntimeError, TypeTag, Value};
use futures::future::BoxFuture;

/// Register all Optional methods and trait implementations.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Core methods ──────────────────────────────────────────────────────
    registry.register(TypeTag::Optional, "is_some", optional_is_some);
    registry.register(TypeTag::Optional, "is_none", optional_is_none);
    registry.register(TypeTag::Optional, "unwrap", optional_unwrap);
    registry.register(TypeTag::Optional, "unwrap_or", optional_unwrap_or);

    // ── Higher-order methods (callback-based) ─────────────────────────────
    registry.register_ho(TypeTag::Optional, "map", optional_map);
    registry.register_ho(TypeTag::Optional, "flat_map", optional_flat_map);

    // ── Equatable trait ───────────────────────────────────────────────────
    registry.register(TypeTag::Optional, "equals", optional_equals);

    // ── Displayable trait ─────────────────────────────────────────────────
    registry.register(TypeTag::Optional, "display", optional_display);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_optional(
    args: &[Value],
    pos: usize,
    method: &str,
) -> Result<Option<Box<Value>>, RuntimeError> {
    match args.get(pos) {
        Some(Value::Optional(inner)) => Ok(inner.clone()),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Optional.{method} expects Optional, got {other}"
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
            "Optional.{method} expects Function, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// ─── Core methods ─────────────────────────────────────────────────────────────

fn optional_is_some(args: &[Value]) -> Result<Value, RuntimeError> {
    let opt = expect_optional(args, 0, "is_some")?;
    Ok(Value::Bool(opt.is_some()))
}

fn optional_is_none(args: &[Value]) -> Result<Value, RuntimeError> {
    let opt = expect_optional(args, 0, "is_none")?;
    Ok(Value::Bool(opt.is_none()))
}

fn optional_unwrap(args: &[Value]) -> Result<Value, RuntimeError> {
    let opt = expect_optional(args, 0, "unwrap")?;
    match opt {
        Some(inner) => Ok(*inner),
        None => Err(RuntimeError::TypeError("called unwrap on None".to_string())),
    }
}

fn optional_unwrap_or(args: &[Value]) -> Result<Value, RuntimeError> {
    let opt = expect_optional(args, 0, "unwrap_or")?;
    let default = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    match opt {
        Some(inner) => Ok(*inner),
        None => Ok(default.clone()),
    }
}

// ─── Higher-order methods ─────────────────────────────────────────────────────

fn optional_map<'a>(
    args: &'a [Value],
    invoker: &'a mut dyn CallbackInvoker,
) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
        let opt = expect_optional(args, 0, "map")?;
        let f = expect_fn(args, 1, "map")?;
        match opt {
            Some(inner) => {
                let result = invoker.invoke(f, &[*inner]).await?;
                Ok(Value::Optional(Some(Box::new(result))))
            }
            None => Ok(Value::Optional(None)),
        }
    })
}

fn optional_flat_map<'a>(
    args: &'a [Value],
    invoker: &'a mut dyn CallbackInvoker,
) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
        let opt = expect_optional(args, 0, "flat_map")?;
        let f = expect_fn(args, 1, "flat_map")?;
        match opt {
            Some(inner) => {
                let result = invoker.invoke(f, &[*inner]).await?;
                match result {
                    Value::Optional(_) => Ok(result),
                    other => Err(RuntimeError::TypeError(format!(
                        "Optional.flat_map callback must return Optional, got {other}"
                    ))),
                }
            }
            None => Ok(Value::Optional(None)),
        }
    })
}

// ─── Trait implementations ────────────────────────────────────────────────────

fn optional_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_optional(args, 0, "equals")?;
    let b = expect_optional(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

fn optional_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let opt = expect_optional(args, 0, "display")?;
    let s = match opt {
        Some(inner) => format!("Some({inner})"),
        None => "None".to_string(),
    };
    Ok(Value::String(BockString::from(s)))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_interp::BuiltinRegistry;

    fn some_val(v: Value) -> Value {
        Value::Optional(Some(Box::new(v)))
    }

    fn none_val() -> Value {
        Value::Optional(None)
    }

    fn registry() -> BuiltinRegistry {
        let mut r = BuiltinRegistry::new();
        register(&mut r);
        r
    }

    fn call(r: &BuiltinRegistry, method: &str, args: &[Value]) -> Result<Value, RuntimeError> {
        r.call(TypeTag::Optional, method, args)
            .expect("method not found")
    }

    #[test]
    fn is_some_true() {
        let r = registry();
        assert_eq!(
            call(&r, "is_some", &[some_val(Value::Int(1))]),
            Ok(Value::Bool(true))
        );
    }

    #[test]
    fn is_some_false() {
        let r = registry();
        assert_eq!(call(&r, "is_some", &[none_val()]), Ok(Value::Bool(false)));
    }

    #[test]
    fn is_none_true() {
        let r = registry();
        assert_eq!(call(&r, "is_none", &[none_val()]), Ok(Value::Bool(true)));
    }

    #[test]
    fn is_none_false() {
        let r = registry();
        assert_eq!(
            call(&r, "is_none", &[some_val(Value::Int(1))]),
            Ok(Value::Bool(false))
        );
    }

    #[tokio::test]
    async fn unwrap_some() {
        let r = registry();
        assert_eq!(
            call(&r, "unwrap", &[some_val(Value::Int(42))]),
            Ok(Value::Int(42))
        );
    }

    #[tokio::test]
    async fn unwrap_none() {
        let r = registry();
        assert!(call(&r, "unwrap", &[none_val()]).is_err());
    }

    #[tokio::test]
    async fn unwrap_or_some() {
        let r = registry();
        assert_eq!(
            call(&r, "unwrap_or", &[some_val(Value::Int(42)), Value::Int(0)]),
            Ok(Value::Int(42))
        );
    }

    #[tokio::test]
    async fn unwrap_or_none() {
        let r = registry();
        assert_eq!(
            call(&r, "unwrap_or", &[none_val(), Value::Int(99)]),
            Ok(Value::Int(99))
        );
    }

    #[tokio::test]
    async fn map_validates_args() {
        let r = registry();
        let mut invoker = bock_interp::NoOpInvoker;
        let ho_func = r
            .get_ho_method(TypeTag::Optional, "map")
            .expect("map not registered");
        // Missing function arg -> arity error
        let result = ho_func(&[some_val(Value::Int(1))], &mut invoker).await;
        assert!(matches!(result, Err(RuntimeError::ArityMismatch { .. })));
    }

    #[tokio::test]
    async fn flat_map_validates_args() {
        let r = registry();
        let mut invoker = bock_interp::NoOpInvoker;
        let ho_func = r
            .get_ho_method(TypeTag::Optional, "flat_map")
            .expect("flat_map not registered");
        let result = ho_func(&[some_val(Value::Int(1))], &mut invoker).await;
        assert!(matches!(result, Err(RuntimeError::ArityMismatch { .. })));
    }

    #[test]
    fn equals_some_some() {
        let r = registry();
        assert_eq!(
            call(
                &r,
                "equals",
                &[some_val(Value::Int(1)), some_val(Value::Int(1))]
            ),
            Ok(Value::Bool(true))
        );
    }

    #[test]
    fn equals_some_different() {
        let r = registry();
        assert_eq!(
            call(
                &r,
                "equals",
                &[some_val(Value::Int(1)), some_val(Value::Int(2))]
            ),
            Ok(Value::Bool(false))
        );
    }

    #[test]
    fn equals_none_none() {
        let r = registry();
        assert_eq!(
            call(&r, "equals", &[none_val(), none_val()]),
            Ok(Value::Bool(true))
        );
    }

    #[test]
    fn equals_some_none() {
        let r = registry();
        assert_eq!(
            call(&r, "equals", &[some_val(Value::Int(1)), none_val()]),
            Ok(Value::Bool(false))
        );
    }

    #[test]
    fn display_some() {
        let r = registry();
        assert_eq!(
            call(&r, "display", &[some_val(Value::Int(42))]),
            Ok(Value::String(BockString::from("Some(42)".to_string())))
        );
    }

    #[test]
    fn display_none() {
        let r = registry();
        assert_eq!(
            call(&r, "display", &[none_val()]),
            Ok(Value::String(BockString::from("None".to_string())))
        );
    }
}
