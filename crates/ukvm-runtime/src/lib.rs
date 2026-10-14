//! Long-lived user-session orchestration. Discovery hints never authorize input.
#[cfg(any(target_os = "linux", test))]
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
#[cfg(any(target_os = "linux", test))]
use ukvm_core::controlled::{InputSink, SinkAction};
#[cfg(target_os = "linux")]
use ukvm_core::controlled::{SinkFuture, SinkKind};
#[cfg(any(target_os = "linux", test))]
use ukvm_protocol::{EdgePosition, InputEvent};

#[cfg(any(target_os = "linux", test))]
mod session;
#[cfg(any(target_os = "linux", test))]
use session::{Backoff, ExclusiveSink, Focus, FocusGate, SharedSession};

#[cfg(any(target_os = "linux", test))]
mod lifecycle;
#[cfg(target_os = "linux")]
use lifecycle::{cancelled, pause, shutdown_signal};

#[cfg(target_os = "linux")]
pub mod ownership;
#[cfg(target_os = "linux")]
mod wayland;

/// Run in the foreground; owns all network, discovery and input resources.
pub async fn run(verbose: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let owner = ownership::RuntimeOwner::acquire()?;
        wayland::run(verbose, &owner).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = verbose;
        Err("persistent runtime currently requires the Linux Wayland backend".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_peer_cannot_activate_and_one_owner_is_enforced() {
        let gate = FocusGate::default();
        assert!(!gate.activate()); // no authenticated acceptance, no arm
        assert!(gate.arm());
        assert!(!gate.arm());
        assert!(gate.activate());
        assert!(gate.reserve_inbound().is_err());
        gate.outbound_closed();
        assert_eq!(gate.state(), Focus::Local);
    }
    #[test]
    fn incoming_entry_waits_for_idle_capture_cleanup() {
        let gate = FocusGate::default();
        assert!(gate.arm());
        gate.reserve_inbound().unwrap();
        assert_eq!(gate.state(), Focus::Yielding);
        assert!(!gate.activate());
        assert!(!gate.arm());
        gate.outbound_closed();
        assert_eq!(gate.state(), Focus::Reserved);
        gate.inbound_closed();
        assert!(gate.arm());
    }
    #[test]
    fn reconnect_is_bounded_and_recovery_resets_delay() {
        let mut backoff = Backoff::default();
        let delays: Vec<_> = (0..9).map(|_| backoff.failed().as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 16, 16, 16, 16]);
        backoff.reset();
        assert_eq!(backoff.failed(), Duration::from_secs(1));
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod lifecycle_tests;
