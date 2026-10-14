//! Backend-independent exclusive focus and prepared input-session leases.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use ukvm_core::controlled::{InputSink, SinkAction, SinkFuture, SinkKind};
use ukvm_protocol::{EdgePosition, InputEvent};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Focus {
    Local,
    Armed,
    Controlling,
    Yielding,
    Disarming,
    Reserved,
    Controlled,
    Window,
}

/// One focus owner; idle outbound barriers can yield to incoming control.
#[derive(Clone)]
pub(crate) struct FocusGate(pub(crate) Arc<Mutex<Focus>>);
impl Default for FocusGate {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Focus::Local)))
    }
}
impl FocusGate {
    pub(crate) fn state(&self) -> Focus {
        *self.0.lock().unwrap()
    }
    pub(crate) fn arm(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        if *state != Focus::Local {
            return false;
        }
        *state = Focus::Armed;
        true
    }
    pub(crate) fn activate(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        if *state != Focus::Armed {
            return false;
        }
        *state = Focus::Controlling;
        true
    }
    pub(crate) fn reserve_inbound(&self) -> Result<(), String> {
        let mut state = self.0.lock().unwrap();
        *state = match *state {
            Focus::Local => Focus::Reserved,
            Focus::Armed => Focus::Yielding,
            _ => return Err("node already has an active control session".into()),
        };
        Ok(())
    }
    pub(crate) fn outbound_closed(&self) {
        let mut state = self.0.lock().unwrap();
        *state = match *state {
            Focus::Yielding => Focus::Reserved,
            Focus::Disarming => Focus::Local,
            Focus::Armed | Focus::Controlling => Focus::Local,
            other => other,
        };
    }
    pub(crate) fn inbound_closed(&self) {
        let mut state = self.0.lock().unwrap();
        *state = match *state {
            // Do not allow rearming until the old portal has closed.
            Focus::Yielding => Focus::Disarming,
            Focus::Reserved | Focus::Controlled | Focus::Window => Focus::Local,
            other => other,
        };
    }
}

/// Coordinates entry with local capture cleanup, independent of OS backend.
pub(crate) struct ExclusiveSink<S> {
    pub(crate) inner: S,
    pub(crate) gate: FocusGate,
    pub(crate) reserved: bool,
}
impl<S: InputSink + Send> InputSink for ExclusiveSink<S> {
    fn kind(&self) -> SinkKind {
        self.inner.kind()
    }
    fn begin_session(&mut self) -> Result<(), String> {
        self.inner.begin_session()
    }
    fn begin_session_async(&mut self) -> SinkFuture<'_> {
        self.inner.begin_session_async()
    }
    fn enter_window_async(
        &mut self,
        target: ukvm_protocol::display::WindowTarget,
        x: f64,
        y: f64,
    ) -> SinkFuture<'_> {
        Box::pin(async move {
            self.gate.reserve_inbound()?;
            self.reserved = true;
            tokio::time::timeout(Duration::from_secs(10), async {
                while self.gate.state() == Focus::Yielding {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .map_err(|_| "local capture cleanup timed out")?;
            {
                let mut state = self.gate.0.lock().unwrap();
                if *state != Focus::Reserved {
                    return Err("focus reservation lost".into());
                }
                *state = Focus::Controlled;
            }
            self.inner.enter_window_async(target, x, y).await
        })
    }
    fn enter_remote(&mut self, _: EdgePosition) -> Result<(), String> {
        Err("runtime entry requires asynchronous capture cleanup".into())
    }
    fn enter_remote_async(&mut self, entry: EdgePosition) -> SinkFuture<'_> {
        self.enter(entry, None)
    }
    fn enter_topology_async(
        &mut self,
        entry: EdgePosition,
        exits: Vec<ukvm_protocol::Edge>,
    ) -> SinkFuture<'_> {
        self.enter(entry, Some(exits))
    }
    fn update_exits(&mut self, exits: Vec<ukvm_protocol::Edge>) -> Result<(), String> {
        if self.gate.state() != Focus::Controlled {
            return Err("exit update without exclusive focus".into());
        }
        self.inner.update_exits(exits)
    }
    fn handle_event(&mut self, event: &InputEvent) -> Result<SinkAction, String> {
        if self.gate.state() != Focus::Controlled {
            return Err("input without exclusive focus".into());
        }
        self.inner.handle_event(event)
    }
    fn is_healthy(&self) -> bool {
        self.inner.is_healthy()
    }
    fn end_session(&mut self) -> Result<(), String> {
        let result = self.inner.end_session();
        if self.reserved {
            self.gate.inbound_closed();
            self.reserved = false;
        }
        result
    }
}

impl<S: InputSink + Send> ExclusiveSink<S> {
    fn enter(
        &mut self,
        entry: EdgePosition,
        exits: Option<Vec<ukvm_protocol::Edge>>,
    ) -> SinkFuture<'_> {
        Box::pin(async move {
            self.gate.reserve_inbound()?;
            self.reserved = true;
            tokio::time::timeout(Duration::from_secs(10), async {
                while self.gate.state() == Focus::Yielding {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .map_err(|_| "local capture cleanup timed out")?;
            {
                let mut state = self.gate.0.lock().unwrap();
                if *state != Focus::Reserved {
                    return Err("focus reservation lost".into());
                }
                *state = Focus::Controlled;
            }
            match exits {
                Some(exits) => self.inner.enter_topology_async(entry, exits).await,
                None => self.inner.enter_remote_async(entry).await,
            }
        })
    }
}

/// Per-connection lease of one prepared grant. An idle connection's cleanup
/// must never park or release the entered owner's input.
pub(crate) struct SharedSession<S> {
    pub(crate) shared: Arc<Mutex<Option<S>>>,
    pub(crate) entered: bool,
}
impl<S: InputSink + Send> InputSink for SharedSession<S> {
    fn kind(&self) -> SinkKind {
        SinkKind::RemoteInput
    }
    fn begin_session(&mut self) -> Result<(), String> {
        if self.shared.lock().unwrap().is_some() {
            Ok(())
        } else {
            Err("portal grant unavailable".into())
        }
    }
    fn enter_window(
        &mut self,
        target: ukvm_protocol::display::WindowTarget,
        x: f64,
        y: f64,
    ) -> Result<(), String> {
        self.entered = true;
        let mut shared = self.shared.lock().unwrap();
        let sink = shared.as_mut().ok_or("portal grant unavailable")?;
        sink.resume_session()?;
        sink.enter_window(target, x, y)
    }
    fn enter_remote(&mut self, entry: EdgePosition) -> Result<(), String> {
        self.entered = true;
        let mut shared = self.shared.lock().unwrap();
        let sink = shared.as_mut().ok_or("portal grant unavailable")?;
        sink.resume_session()?;
        sink.enter_remote(entry)
    }
    fn enter_topology(
        &mut self,
        entry: EdgePosition,
        exits: Vec<ukvm_protocol::Edge>,
    ) -> Result<(), String> {
        self.entered = true;
        let mut shared = self.shared.lock().unwrap();
        let sink = shared.as_mut().ok_or("portal grant unavailable")?;
        sink.resume_session()?;
        sink.enter_topology(entry, exits)
    }
    fn update_exits(&mut self, exits: Vec<ukvm_protocol::Edge>) -> Result<(), String> {
        if !self.entered {
            return Err("exit update before entry".into());
        }
        self.shared
            .lock()
            .unwrap()
            .as_mut()
            .ok_or("portal unavailable")?
            .update_exits(exits)
    }
    fn handle_event(&mut self, event: &InputEvent) -> Result<SinkAction, String> {
        if !self.entered {
            return Err("input without entered session".into());
        }
        self.shared
            .lock()
            .unwrap()
            .as_mut()
            .ok_or("portal unavailable")?
            .handle_event(event)
    }
    fn is_healthy(&self) -> bool {
        self.shared
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(InputSink::is_healthy)
    }
    fn end_session(&mut self) -> Result<(), String> {
        if !self.entered {
            return Ok(());
        }
        self.entered = false;
        self.shared
            .lock()
            .unwrap()
            .as_mut()
            .map_or(Ok(()), InputSink::end_session)
    }
}

#[derive(Default)]
pub(crate) struct Backoff {
    failures: u32,
}
impl Backoff {
    pub(crate) fn failed(&mut self) -> Duration {
        let delay = Duration::from_secs(1 << self.failures.min(4));
        self.failures = self.failures.saturating_add(1);
        delay
    }
    pub(crate) fn reset(&mut self) {
        self.failures = 0;
    }
}
