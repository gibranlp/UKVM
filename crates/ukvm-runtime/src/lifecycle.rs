//! Shared cooperative cancellation; native shutdown registration stays scoped
//! to the OS signal API and never requests desktop input authority.
use std::time::Duration;
use tokio::sync::watch;

pub(crate) async fn cancelled(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() || stop.changed().await.is_err() {
            return;
        }
    }
}

pub(crate) async fn pause(delay: Duration, stop: &mut watch::Receiver<bool>) {
    tokio::select! {
        _ = tokio::time::sleep(delay) => {},
        _ = cancelled(stop) => {},
    }
}

// Registered and retained before the runtime announces readiness. A native
// backend will enable this helper when its user-session runtime is integrated.
#[cfg(target_os = "linux")]
pub(crate) async fn shutdown_signal() -> Result<(), std::io::Error> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[tokio::test]
    async fn cancellation_ignores_false_updates_and_observes_stop() {
        let (sender, mut receiver) = watch::channel(false);
        sender.send(false).unwrap();
        assert!(cancelled(&mut receiver).now_or_never().is_none());
        sender.send(true).unwrap();
        assert!(cancelled(&mut receiver).now_or_never().is_some());
    }

    #[tokio::test]
    async fn closed_stop_channel_cancels_instead_of_spinning() {
        let (sender, mut receiver) = watch::channel(false);
        drop(sender);
        assert!(cancelled(&mut receiver).now_or_never().is_some());
    }

    #[tokio::test]
    async fn retry_pause_ends_on_stop_or_channel_loss() {
        let (sender, mut receiver) = watch::channel(true);
        assert!(pause(Duration::from_secs(60), &mut receiver)
            .now_or_never()
            .is_some());
        sender.send(false).unwrap();
        drop(sender);
        assert!(pause(Duration::from_secs(60), &mut receiver)
            .now_or_never()
            .is_some());
    }
}
