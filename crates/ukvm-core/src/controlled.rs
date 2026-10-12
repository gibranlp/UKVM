//! Controlled-node session policy, independent of transport and OS input APIs.

use std::{future::Future, pin::Pin};
use ukvm_protocol::{EdgePosition, InputEvent};

pub type SinkFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SinkAction {
    Continue,
    Return(EdgePosition),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SinkKind {
    Debug,
    RemoteInput,
}

/// A receiver is explicit about its purpose. `begin_session` must obtain any
/// local authorization before returning; TLS authentication is insufficient.
pub trait InputSink {
    fn kind(&self) -> SinkKind;
    fn begin_session(&mut self) -> Result<(), String>;
    /// Resume an already prepared grant. A runtime must not silently prompt
    /// for new consent after accepting an edge entry.
    fn resume_session(&mut self) -> Result<(), String> {
        self.begin_session()
    }
    /// Authorization may wait for a desktop consent dialog. Receivers await
    /// this separately so local shutdown remains responsive during that wait.
    fn begin_session_async(&mut self) -> SinkFuture<'_>
    where
        Self: Send,
    {
        Box::pin(async { self.begin_session() })
    }
    fn enter_window(
        &mut self,
        _target: ukvm_protocol::display::WindowTarget,
        _x: f64,
        _y: f64,
    ) -> Result<(), String> {
        Err("window entry unavailable".into())
    }
    fn enter_window_async(
        &mut self,
        target: ukvm_protocol::display::WindowTarget,
        x: f64,
        y: f64,
    ) -> SinkFuture<'_>
    where
        Self: Send,
    {
        Box::pin(async move { self.enter_window(target, x, y) })
    }
    fn enter_remote(&mut self, _entry: EdgePosition) -> Result<(), String> {
        Err("sink does not support remote entry".into())
    }
    /// Runtime entry may first disarm an idle local capture barrier.
    fn enter_remote_async(&mut self, entry: EdgePosition) -> SinkFuture<'_>
    where
        Self: Send,
    {
        Box::pin(async move { self.enter_remote(entry) })
    }
    fn enter_topology(
        &mut self,
        _entry: EdgePosition,
        _exits: Vec<ukvm_protocol::Edge>,
    ) -> Result<(), String> {
        Err("topology entry is unavailable".into())
    }
    fn enter_topology_async(
        &mut self,
        entry: EdgePosition,
        exits: Vec<ukvm_protocol::Edge>,
    ) -> SinkFuture<'_>
    where
        Self: Send,
    {
        Box::pin(async move { self.enter_topology(entry, exits) })
    }
    fn update_exits(&mut self, _exits: Vec<ukvm_protocol::Edge>) -> Result<(), String> {
        Err("topology exit updates are unavailable".into())
    }
    fn handle_event(&mut self, event: &InputEvent) -> Result<SinkAction, String>;
    /// The receiver polls this while idle so backend loss ends the network
    /// session even when no further input arrives.
    fn is_healthy(&self) -> bool {
        true
    }
    /// Must release held inputs even after an event failure or disconnect.
    fn end_session(&mut self) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlledState {
    PeerAuthenticated,
    Authorizing,
    ReceivingInput,
    Releasing,
    Idle,
}

impl ControlledState {
    pub fn request(&mut self) -> Result<(), String> {
        if *self != Self::PeerAuthenticated {
            return Err("control session request out of order".into());
        }
        *self = Self::Authorizing;
        Ok(())
    }

    pub fn accept(&mut self) -> Result<(), String> {
        if *self != Self::Authorizing {
            return Err("control session acceptance out of order".into());
        }
        *self = Self::ReceivingInput;
        Ok(())
    }

    pub fn event(&self) -> Result<(), String> {
        if *self != Self::ReceivingInput {
            return Err("input outside an accepted session".into());
        }
        Ok(())
    }

    pub fn release(&mut self) {
        *self = Self::Releasing;
    }

    pub fn finish(&mut self) {
        *self = Self::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::ControlledState;

    #[test]
    fn input_requires_explicit_request_and_acceptance() {
        let mut state = ControlledState::PeerAuthenticated;
        assert!(state.event().is_err());
        assert!(state.accept().is_err());
        state.request().unwrap();
        assert!(state.event().is_err());
        state.accept().unwrap();
        state.event().unwrap();
        assert!(state.request().is_err());
        state.release();
        assert!(state.event().is_err());
        state.finish();
        assert_eq!(state, ControlledState::Idle);
    }
}
