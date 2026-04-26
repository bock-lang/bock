//! Core time module — registers the `sleep` prelude function.
//!
//! `sleep(duration)` spawns a tokio task that suspends for the given duration
//! and returns `Value::Future` immediately, so concurrent sleeps run in
//! parallel when driven by `await`. Uses `tokio::time::sleep` under the hood,
//! which is the asynchronous sleep primitive (does not block an OS thread).

use std::sync::{Arc, Mutex};

use bock_interp::{BuiltinRegistry, RuntimeError, Value};

/// Register the `sleep` global.
pub fn register(registry: &mut BuiltinRegistry) {
    registry.register_global("sleep", builtin_sleep);
}

/// `sleep(duration: Duration) -> Void with Clock` — prelude function.
///
/// Returns `Value::Future` immediately; the backing tokio task yields for the
/// given duration. `await` resolves the future to `Void`.
fn builtin_sleep(args: &[Value]) -> Result<Value, RuntimeError> {
    let nanos = match args.first() {
        Some(Value::Duration(n)) => *n,
        Some(Value::Int(n)) => *n, // permissive: accept Int as nanoseconds
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "sleep expects Duration, got {other}"
            )));
        }
        None => {
            return Err(RuntimeError::ArityMismatch {
                expected: 1,
                got: 0,
            });
        }
    };
    let handle = tokio::spawn(async move {
        if nanos > 0 {
            let dur = std::time::Duration::from_nanos(nanos as u64);
            tokio::time::sleep(dur).await;
        }
        Ok(Value::Void)
    });
    Ok(Value::Future(Arc::new(Mutex::new(Some(handle)))))
}
