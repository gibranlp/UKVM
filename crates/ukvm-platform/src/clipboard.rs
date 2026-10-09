use std::future::Future;
use tokio::sync::watch;

#[derive(Clone)]
pub struct ClipboardWrite {
    pub text: String,
    pub authorize: std::sync::Arc<dyn Fn() -> Option<Box<dyn Send>> + Send + Sync>,
}

#[derive(Clone)]
pub enum ClipboardEvent {
    Text(String),
    Ignored,
    Ready,
    Rejected(&'static str),
}
impl std::fmt::Debug for ClipboardEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClipboardEvent(redacted)")
    }
}

pub trait ClipboardBackend {
    fn run(
        self,
        changes: watch::Sender<Option<ClipboardEvent>>,
        writes: watch::Receiver<Option<ClipboardWrite>>,
        stop: watch::Receiver<bool>,
    ) -> impl Future<Output = Result<(), String>>;
}

#[cfg(target_os = "linux")]
pub mod wayland;
