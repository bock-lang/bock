#![allow(unused_imports, dead_code)]
// ── Bock concurrency runtime ──
use std::sync::Arc;
pub struct __BockChannel<T> {
    tx: tokio::sync::mpsc::UnboundedSender<T>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<T>>,
}
pub fn __bock_channel_new<T: Send + 'static>() -> (Arc<__BockChannel<T>>, Arc<__BockChannel<T>>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let ch = Arc::new(__BockChannel { tx, rx: tokio::sync::Mutex::new(rx) });
    (ch.clone(), ch)
}
impl<T> __BockChannel<T> {
    pub fn send(&self, v: T) { let _ = self.tx.send(v); }
    pub async fn recv(&self) -> T {
        let mut guard = self.rx.lock().await;
        guard.recv().await.expect("channel closed")
    }
    pub fn close(&self) {}
}
pub fn __bock_spawn<T: Send + 'static>(f: impl std::future::Future<Output = T> + Send + 'static) -> tokio::task::JoinHandle<T> {
    tokio::spawn(f)
}
