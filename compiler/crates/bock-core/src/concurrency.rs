//! Core concurrency module — registers `Channel[T]` and `spawn`.
//!
//! `Channel[T]` is an unbounded async MPSC channel: `Channel.new()` returns
//! a tuple `(tx, rx)` where both ends share the same underlying queue (the
//! interpreter uses `tokio::sync::mpsc` under the hood). `tx.send(v)` is
//! non-blocking; `rx.recv()` returns a `Value::Future` whose awaited value
//! is the next message (or a runtime error if the sender dropped without
//! sending).
//!
//! `spawn(x)` launches `x` concurrently. When `x` is already a `Future`
//! (as produced by calling an `async fn`), `spawn` is the identity
//! function — our async fn calls already `tokio::spawn` the body. When
//! `x` is a thunk `Fn() -> T`, `spawn` evaluates the thunk as a task and
//! wraps it in a `Future`. The `await` operator resolves the future.
//!
//! Effect handlers propagate naturally: async fn calls clone the
//! interpreter's effect stack into the spawned task, so handlers installed
//! by an enclosing `handling` block remain resolvable inside the task.

use std::sync::{Arc, Mutex};

use bock_interp::{BuiltinRegistry, ChannelHandle, RuntimeError, TypeTag, Value};

/// Register concurrency primitives.
pub fn register(registry: &mut BuiltinRegistry) {
    // Associated function: `Channel.new()` / `Channel[T].new()`.
    registry.register_global("Channel.new", channel_new);

    // Instance methods: send (sync) and recv (returns Future).
    registry.register(TypeTag::Channel, "send", channel_send);
    registry.register(TypeTag::Channel, "recv", channel_recv);
    registry.register(TypeTag::Channel, "close", channel_close);

    // spawn(x) — see module docs.
    registry.register_global("spawn", builtin_spawn);
}

// ─── Channel ────────────────────────────────────────────────────────────────

/// `Channel.new() -> (Channel[T], Channel[T])`.
///
/// Returns a tuple of two clones of the same handle; either end can send or
/// receive. The MPSC queue tolerates multiple senders but only one receiver
/// may `.recv().await` at a time (enforced by an internal async mutex).
fn channel_new(_args: &[Value]) -> Result<Value, RuntimeError> {
    let handle = ChannelHandle::new();
    Ok(Value::Tuple(vec![
        Value::Channel(handle.clone()),
        Value::Channel(handle),
    ]))
}

/// `channel.send(value)` — enqueue a value. Returns `Void`.
///
/// Fails with a TypeError if the receiver has been dropped.
fn channel_send(args: &[Value]) -> Result<Value, RuntimeError> {
    let handle = match args.first() {
        Some(Value::Channel(h)) => h.clone(),
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "Channel.send called on non-Channel: {other}"
            )));
        }
        None => {
            return Err(RuntimeError::ArityMismatch {
                expected: 2,
                got: 0,
            });
        }
    };
    let val = args.get(1).cloned().unwrap_or(Value::Void);
    handle.sender.send(val).map_err(|_| {
        RuntimeError::TypeError("Channel.send: receiver has been dropped".to_string())
    })?;
    Ok(Value::Void)
}

/// `channel.recv() -> Future[T]`.
///
/// Returns a `Value::Future` that resolves to the next value sent on the
/// channel. Awaiting an empty, closed channel yields a runtime error (all
/// senders have dropped and no messages remain).
fn channel_recv(args: &[Value]) -> Result<Value, RuntimeError> {
    let handle = match args.first() {
        Some(Value::Channel(h)) => h.clone(),
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "Channel.recv called on non-Channel: {other}"
            )));
        }
        None => {
            return Err(RuntimeError::ArityMismatch {
                expected: 1,
                got: 0,
            });
        }
    };
    let join = tokio::spawn(async move {
        let mut rx = handle.receiver.lock().await;
        match rx.recv().await {
            Some(v) => Ok(v),
            None => Err(RuntimeError::TypeError(
                "Channel.recv: channel closed with no more messages".to_string(),
            )),
        }
    });
    Ok(Value::Future(Arc::new(Mutex::new(Some(join)))))
}

/// `channel.close()` — close the sender end.
///
/// The sender on the shared handle cannot be dropped individually without
/// consuming it, so this is a no-op that exists for API completeness.
/// Closure happens automatically when all references to the channel are
/// dropped.
fn channel_close(_args: &[Value]) -> Result<Value, RuntimeError> {
    Ok(Value::Void)
}

// ─── spawn ──────────────────────────────────────────────────────────────────

/// `spawn(x) -> Future[T]`.
///
/// If `x` is already a Future (produced by calling an `async fn`), returns
/// it unchanged — our async fn machinery already spawns the body as a
/// tokio task. This means `spawn(async_fn())` and `async_fn()` are
/// equivalent in the interpreter, consistent with the "async fn calls are
/// eager tasks" model we inherit from JS/TS/Python codegen.
///
/// Rejecting non-Future arguments makes the surface predictable: wrap any
/// computation in an `async fn` to make it spawnable.
fn builtin_spawn(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        Some(Value::Future(_)) => Ok(args[0].clone()),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "spawn expects the result of an async fn call (Future), got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: 1,
            got: 0,
        }),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_interp::BockString;

    fn reg() -> BuiltinRegistry {
        let mut r = BuiltinRegistry::new();
        register(&mut r);
        r
    }

    #[tokio::test]
    async fn channel_new_returns_tuple_of_two_channels() {
        let r = reg();
        let result = r.call_global("Channel.new", &[]).unwrap().unwrap();
        match result {
            Value::Tuple(items) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(items[0], Value::Channel(_)));
                assert!(matches!(items[1], Value::Channel(_)));
            }
            other => panic!("expected Tuple, got {other}"),
        }
    }

    #[tokio::test]
    async fn channel_send_recv_roundtrip() {
        let r = reg();
        let tuple = r.call_global("Channel.new", &[]).unwrap().unwrap();
        let (tx, rx) = match tuple {
            Value::Tuple(mut v) => (v.remove(0), v.remove(0)),
            _ => unreachable!(),
        };
        let msg = Value::String(BockString::new("hello"));
        r.call(TypeTag::Channel, "send", &[tx.clone(), msg.clone()])
            .unwrap()
            .unwrap();
        let fut = r
            .call(TypeTag::Channel, "recv", &[rx.clone()])
            .unwrap()
            .unwrap();
        let handle = match fut {
            Value::Future(h) => h,
            _ => panic!("expected Future"),
        };
        let jh = handle.lock().unwrap().take().unwrap();
        let received = jh.await.unwrap().unwrap();
        assert_eq!(received, msg);
    }

    #[tokio::test]
    async fn spawn_returns_future_unchanged() {
        let r = reg();
        let jh = tokio::spawn(async { Ok(Value::Int(42)) });
        let fut = Value::Future(Arc::new(Mutex::new(Some(jh))));
        let result = r.call_global("spawn", &[fut]).unwrap().unwrap();
        assert!(matches!(result, Value::Future(_)));
    }

    #[test]
    fn spawn_rejects_non_future() {
        let r = reg();
        let result = r.call_global("spawn", &[Value::Int(42)]).unwrap();
        assert!(matches!(result, Err(RuntimeError::TypeError(_))));
    }
}
