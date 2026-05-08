//! Result type methods and trait implementations.

use bock_interp::{BockString, BuiltinRegistry, CallbackInvoker, RuntimeError, TypeTag, Value};
use futures::future::BoxFuture;

/// Register all Result methods and trait implementations.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Core methods ──────────────────────────────────────────────────────
    registry.register(TypeTag::Result, "is_ok", result_is_ok);
    registry.register(TypeTag::Result, "is_err", result_is_err);
    registry.register(TypeTag::Result, "unwrap", result_unwrap);
    registry.register(TypeTag::Result, "unwrap_or", result_unwrap_or);

    // ── Higher-order methods (callback-based) ─────────────────────────────
    registry.register_ho(TypeTag::Result, "map", result_map);
    registry.register_ho(TypeTag::Result, "map_err", result_map_err);

    // ── Equatable trait ───────────────────────────────────────────────────
    registry.register(TypeTag::Result, "equals", result_equals);

    // ── Displayable trait ─────────────────────────────────────────────────
    registry.register(TypeTag::Result, "display", result_display);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_result(
    args: &[Value],
    pos: usize,
    method: &str,
) -> Result<std::result::Result<Box<Value>, Box<Value>>, RuntimeError> {
    match args.get(pos) {
        Some(Value::Result(inner)) => Ok(inner.clone()),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Result.{method} expects Result, got {other}"
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
            "Result.{method} expects Function, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// ─── Core methods ─────────────────────────────────────────────────────────────

fn result_is_ok(args: &[Value]) -> Result<Value, RuntimeError> {
    let res = expect_result(args, 0, "is_ok")?;
    Ok(Value::Bool(res.is_ok()))
}

fn result_is_err(args: &[Value]) -> Result<Value, RuntimeError> {
    let res = expect_result(args, 0, "is_err")?;
    Ok(Value::Bool(res.is_err()))
}

fn result_unwrap(args: &[Value]) -> Result<Value, RuntimeError> {
    let res = expect_result(args, 0, "unwrap")?;
    match res {
        Ok(inner) => Ok(*inner),
        Err(e) => Err(RuntimeError::TypeError(format!(
            "called unwrap on Err({e})"
        ))),
    }
}

fn result_unwrap_or(args: &[Value]) -> Result<Value, RuntimeError> {
    let res = expect_result(args, 0, "unwrap_or")?;
    let default = args.get(1).ok_or(RuntimeError::ArityMismatch {
        expected: 2,
        got: args.len(),
    })?;
    match res {
        Ok(inner) => Ok(*inner),
        Err(_) => Ok(default.clone()),
    }
}

// ─── Higher-order methods ─────────────────────────────────────────────────────

fn result_map<'a>(
    args: &'a [Value],
    invoker: &'a mut dyn CallbackInvoker,
) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
        let res = expect_result(args, 0, "map")?;
        let f = expect_fn(args, 1, "map")?;
        match res {
            Ok(inner) => {
                let mapped = invoker.invoke(f, &[*inner]).await?;
                Ok(Value::Result(Ok(Box::new(mapped))))
            }
            Err(e) => Ok(Value::Result(Err(e))),
        }
    })
}

fn result_map_err<'a>(
    args: &'a [Value],
    invoker: &'a mut dyn CallbackInvoker,
) -> BoxFuture<'a, Result<Value, RuntimeError>> {
    Box::pin(async move {
        let res = expect_result(args, 0, "map_err")?;
        let f = expect_fn(args, 1, "map_err")?;
        match res {
            Ok(v) => Ok(Value::Result(Ok(v))),
            Err(inner) => {
                let mapped = invoker.invoke(f, &[*inner]).await?;
                Ok(Value::Result(Err(Box::new(mapped))))
            }
        }
    })
}

// ─── Trait implementations ────────────────────────────────────────────────────

fn result_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_result(args, 0, "equals")?;
    let b = expect_result(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

fn result_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let res = expect_result(args, 0, "display")?;
    let s = match res {
        Ok(inner) => format!("Ok({inner})"),
        Err(e) => format!("Err({e})"),
    };
    Ok(Value::String(BockString::from(s)))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_interp::BuiltinRegistry;

    fn ok_val(v: Value) -> Value {
        Value::Result(Ok(Box::new(v)))
    }

    fn err_val(v: Value) -> Value {
        Value::Result(Err(Box::new(v)))
    }

    fn registry() -> BuiltinRegistry {
        let mut r = BuiltinRegistry::new();
        register(&mut r);
        r
    }

    fn call(r: &BuiltinRegistry, method: &str, args: &[Value]) -> Result<Value, RuntimeError> {
        r.call(TypeTag::Result, method, args)
            .expect("method not found")
    }

    #[test]
    fn is_ok_true() {
        let r = registry();
        assert_eq!(
            call(&r, "is_ok", &[ok_val(Value::Int(1))]),
            Ok(Value::Bool(true))
        );
    }

    #[tokio::test]
    async fn is_ok_false() {
        let r = registry();
        assert_eq!(
            call(&r, "is_ok", &[err_val(Value::Int(1))]),
            Ok(Value::Bool(false))
        );
    }

    #[tokio::test]
    async fn is_err_true() {
        let r = registry();
        assert_eq!(
            call(&r, "is_err", &[err_val(Value::Int(1))]),
            Ok(Value::Bool(true))
        );
    }

    #[tokio::test]
    async fn is_err_false() {
        let r = registry();
        assert_eq!(
            call(&r, "is_err", &[ok_val(Value::Int(1))]),
            Ok(Value::Bool(false))
        );
    }

    #[tokio::test]
    async fn unwrap_ok() {
        let r = registry();
        assert_eq!(
            call(&r, "unwrap", &[ok_val(Value::Int(42))]),
            Ok(Value::Int(42))
        );
    }

    #[tokio::test]
    async fn unwrap_err() {
        let r = registry();
        assert!(call(&r, "unwrap", &[err_val(Value::Int(1))]).is_err());
    }

    #[tokio::test]
    async fn map_validates_args() {
        let r = registry();
        let mut invoker = bock_interp::NoOpInvoker;
        let ho_func = r
            .get_ho_method(TypeTag::Result, "map")
            .expect("map not registered");
        let result = ho_func(&[ok_val(Value::Int(1))], &mut invoker).await;
        assert!(matches!(result, Err(RuntimeError::ArityMismatch { .. })));
    }

    #[tokio::test]
    async fn map_err_validates_args() {
        let r = registry();
        let mut invoker = bock_interp::NoOpInvoker;
        let ho_func = r
            .get_ho_method(TypeTag::Result, "map_err")
            .expect("map_err not registered");
        let result = ho_func(&[err_val(Value::Int(1))], &mut invoker).await;
        assert!(matches!(result, Err(RuntimeError::ArityMismatch { .. })));
    }

    #[test]
    fn equals_ok_ok_same() {
        let r = registry();
        assert_eq!(
            call(
                &r,
                "equals",
                &[ok_val(Value::Int(1)), ok_val(Value::Int(1))]
            ),
            Ok(Value::Bool(true))
        );
    }

    #[test]
    fn equals_ok_ok_diff() {
        let r = registry();
        assert_eq!(
            call(
                &r,
                "equals",
                &[ok_val(Value::Int(1)), ok_val(Value::Int(2))]
            ),
            Ok(Value::Bool(false))
        );
    }

    #[test]
    fn equals_err_err_same() {
        let r = registry();
        assert_eq!(
            call(
                &r,
                "equals",
                &[err_val(Value::Int(1)), err_val(Value::Int(1))]
            ),
            Ok(Value::Bool(true))
        );
    }

    #[test]
    fn equals_ok_err() {
        let r = registry();
        assert_eq!(
            call(
                &r,
                "equals",
                &[ok_val(Value::Int(1)), err_val(Value::Int(1))]
            ),
            Ok(Value::Bool(false))
        );
    }

    #[test]
    fn display_ok() {
        let r = registry();
        assert_eq!(
            call(&r, "display", &[ok_val(Value::Int(42))]),
            Ok(Value::String(BockString::from("Ok(42)".to_string())))
        );
    }

    #[test]
    fn display_err() {
        let r = registry();
        assert_eq!(
            call(
                &r,
                "display",
                &[err_val(Value::String(BockString::from("boom".to_string())))]
            ),
            Ok(Value::String(BockString::from("Err(boom)".to_string())))
        );
    }

    #[test]
    fn unwrap_or_ok_returns_inner() {
        let r = registry();
        assert_eq!(
            call(&r, "unwrap_or", &[ok_val(Value::Int(5)), Value::Int(0)]),
            Ok(Value::Int(5))
        );
    }

    #[test]
    fn unwrap_or_err_returns_default() {
        let r = registry();
        assert_eq!(
            call(
                &r,
                "unwrap_or",
                &[
                    err_val(Value::String(BockString::from("e".to_string()))),
                    Value::Int(0)
                ]
            ),
            Ok(Value::Int(0))
        );
    }
}
