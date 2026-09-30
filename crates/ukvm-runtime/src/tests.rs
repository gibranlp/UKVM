use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use ukvm_platform::{InjectionCapabilities, PlatformError, PlatformInputInjector, RemoteInputSink};
use ukvm_protocol::{Edge, KeyCode};

#[derive(Default)]
pub(super) struct Evidence {
    pub(super) grants: usize,
    pub(super) parks: usize,
    pub(super) closes: usize,
    pub(super) held_key: bool,
    pub(super) held_button: bool,
}
pub(super) struct Injector {
    evidence: Arc<Mutex<Evidence>>,
    granted: bool,
}
impl PlatformInputInjector for Injector {
    fn capabilities(&self) -> InjectionCapabilities {
        InjectionCapabilities {
            pointer_absolute: true,
            pointer_button: true,
            keyboard: true,
            ..Default::default()
        }
    }
    fn begin(&mut self) -> Result<(), PlatformError> {
        if !self.granted {
            self.evidence.lock().unwrap().grants += 1;
            self.granted = true;
        }
        Ok(())
    }
    fn enter_remote(&mut self, _: EdgePosition) -> Result<(), PlatformError> {
        Ok(())
    }
    fn inject(&mut self, event: &InputEvent) -> Result<(), PlatformError> {
        let mut evidence = self.evidence.lock().unwrap();
        match event {
            InputEvent::Key { pressed, .. } => evidence.held_key = *pressed,
            InputEvent::PointerButton { pressed, .. } => evidence.held_button = *pressed,
            _ => {}
        }
        Ok(())
    }
    fn inject_remote(&mut self, event: &InputEvent) -> Result<Option<EdgePosition>, PlatformError> {
        if matches!(event, InputEvent::PointerMotion { dx, .. } if *dx < 0.0) {
            return Ok(Some(entry()));
        }
        self.inject(event)?;
        Ok(None)
    }
    fn release_all(&mut self) -> Result<(), PlatformError> {
        let mut evidence = self.evidence.lock().unwrap();
        evidence.held_key = false;
        evidence.held_button = false;
        Ok(())
    }
    fn park(&mut self) -> Result<(), PlatformError> {
        self.evidence.lock().unwrap().parks += 1;
        Ok(())
    }
    fn end(&mut self) -> Result<(), PlatformError> {
        self.evidence.lock().unwrap().closes += 1;
        self.granted = false;
        Ok(())
    }
}
pub(super) fn entry() -> EdgePosition {
    EdgePosition {
        edge: Edge::Left,
        fraction: 0.5,
    }
}
pub(super) fn sink(
    gate: &FocusGate,
    evidence: &Arc<Mutex<Evidence>>,
) -> ExclusiveSink<RemoteInputSink<Injector>> {
    ExclusiveSink {
        inner: RemoteInputSink::new(Injector {
            evidence: evidence.clone(),
            granted: false,
        }),
        gate: gate.clone(),
        reserved: false,
    }
}
pub(super) fn hold<S: InputSink>(sink: &mut S) {
    sink.handle_event(&InputEvent::Key {
        code: KeyCode::LinuxEvdev(42),
        pressed: true,
    })
    .unwrap();
    sink.handle_event(&InputEvent::PointerButton {
        button: 272,
        pressed: true,
    })
    .unwrap();
}

#[tokio::test]
async fn ten_round_trips_reuse_grant_and_release_held_input() {
    let gate = FocusGate::default();
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let mut sink = sink(&gate, &evidence);
    for _ in 0..10 {
        sink.begin_session_async().await.unwrap();
        sink.enter_remote_async(entry()).await.unwrap();
        assert!(!gate.arm());
        hold(&mut sink);
        assert_eq!(
            sink.handle_event(&InputEvent::PointerMotion { dx: -1.0, dy: 0.0 })
                .unwrap(),
            SinkAction::Return(entry())
        );
        sink.end_session().unwrap();
        assert_eq!(gate.state(), Focus::Local);
        let evidence = evidence.lock().unwrap();
        assert!(!evidence.held_key && !evidence.held_button);
        assert_eq!(evidence.grants, 1);
        assert_eq!(evidence.closes, 0);
    }
    sink.inner.shutdown().unwrap();
    let evidence = evidence.lock().unwrap();
    assert_eq!(evidence.parks, 10);
    assert_eq!(evidence.closes, 1);
}

#[tokio::test]
async fn incoming_entry_disarms_and_waits_for_capture_close_before_injection() {
    let gate = FocusGate::default();
    assert!(gate.arm());
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let mut sink = sink(&gate, &evidence);
    sink.begin_session_async().await.unwrap();
    let closed = Arc::new(AtomicUsize::new(0));
    let closed_clone = closed.clone();
    let capture_gate = gate.clone();
    let capture = async move {
        while capture_gate.state() != Focus::Yielding {
            tokio::task::yield_now().await;
        }
        assert!(!capture_gate.activate());
        tokio::time::sleep(Duration::from_millis(25)).await;
        closed_clone.store(1, Ordering::SeqCst);
        capture_gate.outbound_closed();
    };
    let (result, ()) = tokio::join!(sink.enter_remote_async(entry()), capture);
    result.unwrap();
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    hold(&mut sink);
    sink.end_session().unwrap();
    assert!(gate.arm());
}

#[tokio::test]
async fn cancelled_entry_does_not_leave_reserved_focus() {
    let gate = FocusGate::default();
    assert!(gate.arm());
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let mut sink = sink(&gate, &evidence);
    sink.begin_session_async().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), sink.enter_remote_async(entry()))
            .await
            .is_err()
    );
    sink.end_session().unwrap();
    assert!(!gate.arm()); // old barrier still needs Close
    gate.outbound_closed();
    assert_eq!(gate.state(), Focus::Local);
    assert!(gate.arm());
}

#[tokio::test]
async fn competing_active_sessions_fail_closed_and_cleanup_preserves_owner() {
    let gate = FocusGate::default();
    assert!(gate.arm());
    assert!(gate.activate());
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let mut incoming = sink(&gate, &evidence);
    incoming.begin_session_async().await.unwrap();
    assert!(incoming.enter_remote_async(entry()).await.is_err());
    incoming.end_session().unwrap();
    assert_eq!(gate.state(), Focus::Controlling);
    gate.outbound_closed();
    incoming.begin_session_async().await.unwrap();
    incoming.enter_remote_async(entry()).await.unwrap();
    hold(&mut incoming);
    incoming.end_session().unwrap();
    assert_eq!(gate.state(), Focus::Local);
    assert!(!evidence.lock().unwrap().held_key);
}
