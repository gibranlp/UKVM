//! OS input boundary. Implementations must live under platform-specific modules.

use ukvm_core::controlled::{InputSink, SinkAction, SinkKind};
use ukvm_protocol::{EdgePosition, InputEvent};

pub mod clipboard;
#[cfg(target_os = "linux")]
pub mod linux_wayland;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    LinuxWayland,
    LinuxX11,
    MacOs,
    Windows,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Capabilities {
    pub capture_keyboard: bool,
    pub capture_pointer: bool,
    pub inject_keyboard: bool,
    pub inject_pointer: bool,
    pub pointer_position: bool,
    pub display_enumeration: bool,
    pub edge_barriers: bool,
    pub clipboard: bool,
}

/// Operations an authorized input session can actually perform. Backends may
/// expose only a subset; protocol key codes still need explicit translation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InjectionCapabilities {
    pub pointer_relative: bool,
    pub pointer_absolute: bool,
    pub pointer_button: bool,
    pub pointer_scroll: bool,
    pub keyboard: bool,
}

/// Evidence levels are independent: detection of an API does not mean UKVM
/// implemented, received permission for, or physically proved injection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CapabilityEvidence {
    pub detected: bool,
    pub implemented: bool,
    pub authorized: bool,
    pub physically_proven: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlatformError {
    Unsupported(&'static str),
    PermissionDenied(&'static str),
    SessionClosed,
    Backend(String),
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PlatformError {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub id: String,
    pub origin: Point,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundaryEdge {
    Left,
    Right,
    Top,
    Bottom,
}

/// Identifies one edge of one native display. The backend owns coordinate mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Boundary {
    pub display_id: String,
    pub edge: BoundaryEdge,
}

/// A compositor or OS may provide edge barriers instead of global pointer polling.
pub trait BoundaryDetector: Send {
    fn set_boundaries(&mut self, boundaries: &[Boundary]) -> Result<(), PlatformError>;
    fn next_crossing(&mut self) -> Result<Boundary, PlatformError>;
}

/// Events generated while local control is handed to another trusted node.
pub trait InputCapture: Send {
    fn next_event(&mut self) -> Result<InputEvent, PlatformError>;
    fn stop(&mut self) -> Result<(), PlatformError>;
}

/// Backend permission is requested only when a real capture session starts.
pub trait InputSource: Send {
    fn start_capture(&mut self) -> Result<Box<dyn InputCapture>, PlatformError>;
}

/// A session granted permission to inject input. Creating this session is a
/// platform operation and must fail when permission or the backend is absent.
pub trait PlatformInputInjector: Send {
    fn capabilities(&self) -> InjectionCapabilities;
    /// Must clean up partial native state before returning an error.
    fn begin(&mut self) -> Result<(), PlatformError>;
    fn resume(&mut self) -> Result<(), PlatformError> {
        self.begin()
    }
    fn begin_async(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), PlatformError>> + Send + '_>>
    {
        Box::pin(async { self.begin() })
    }
    fn inject(&mut self, event: &InputEvent) -> Result<(), PlatformError>;
    fn enter_remote(&mut self, _entry: EdgePosition) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "remote edge entry is unavailable",
        ))
    }
    fn enter_topology(
        &mut self,
        _entry: EdgePosition,
        _exits: Vec<ukvm_protocol::Edge>,
    ) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported("topology entry is unavailable"))
    }
    fn update_exits(&mut self, _exits: Vec<ukvm_protocol::Edge>) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported(
            "topology exit updates are unavailable",
        ))
    }
    fn inject_remote(&mut self, event: &InputEvent) -> Result<Option<EdgePosition>, PlatformError> {
        self.inject(event)?;
        Ok(None)
    }
    fn is_healthy(&self) -> bool {
        true
    }
    fn release_all(&mut self) -> Result<(), PlatformError>;
    /// End this control relationship while an explicitly selected listener
    /// may retain its local portal grant for another authorized relationship.
    /// Backends without a reusable grant close the session normally.
    fn park(&mut self) -> Result<(), PlatformError> {
        self.end()
    }
    fn end(&mut self) -> Result<(), PlatformError>;
}

/// Binds a locally authorized platform injector to the network-facing sink.
/// The receiver must still explicitly select RemoteInput and authorize each session.
pub struct RemoteInputSink<I: PlatformInputInjector> {
    injector: I,
    active: bool,
    entered: bool,
}

impl<I: PlatformInputInjector> RemoteInputSink<I> {
    pub fn new(injector: I) -> Self {
        Self {
            injector,
            active: false,
            entered: false,
        }
    }

    pub fn grant_is_healthy(&self) -> bool {
        self.injector.is_healthy()
    }

    pub fn shutdown(&mut self) -> Result<(), String> {
        let released = if self.active {
            self.injector.release_all()
        } else {
            Ok(())
        };
        self.active = false;
        self.entered = false;
        let ended = self.injector.end();
        released.and(ended).map_err(|e| e.to_string())
    }
}

impl<I: PlatformInputInjector> InputSink for RemoteInputSink<I> {
    fn kind(&self) -> SinkKind {
        SinkKind::RemoteInput
    }

    fn begin_session(&mut self) -> Result<(), String> {
        self.injector.begin().map_err(|e| e.to_string())?;
        self.active = true;
        self.entered = false;
        Ok(())
    }

    fn resume_session(&mut self) -> Result<(), String> {
        self.injector.resume().map_err(|e| e.to_string())?;
        self.active = true;
        self.entered = false;
        Ok(())
    }

    fn begin_session_async(&mut self) -> ukvm_core::controlled::SinkFuture<'_> {
        Box::pin(async {
            self.injector
                .begin_async()
                .await
                .map_err(|e| e.to_string())?;
            self.active = true;
            self.entered = false;
            Ok(())
        })
    }

    fn enter_remote(&mut self, entry: EdgePosition) -> Result<(), String> {
        if !self.active || self.entered {
            return Err("remote entry is out of order".into());
        }
        entry.validate()?;
        if !self.injector.capabilities().pointer_absolute {
            return Err("remote edge return requires absolute pointer injection".into());
        }
        self.injector
            .enter_remote(entry)
            .map_err(|e| e.to_string())?;
        self.entered = true;
        Ok(())
    }

    fn enter_topology(
        &mut self,
        entry: EdgePosition,
        exits: Vec<ukvm_protocol::Edge>,
    ) -> Result<(), String> {
        if !self.active || self.entered {
            return Err("topology entry is out of order".into());
        }
        self.injector
            .enter_topology(entry, exits)
            .map_err(|e| e.to_string())?;
        self.entered = true;
        Ok(())
    }

    fn update_exits(&mut self, exits: Vec<ukvm_protocol::Edge>) -> Result<(), String> {
        if !self.active || !self.entered {
            return Err("topology exit update before entry".into());
        }
        self.injector.update_exits(exits).map_err(|e| e.to_string())
    }

    fn handle_event(&mut self, event: &InputEvent) -> Result<SinkAction, String> {
        if !self.active || !self.entered {
            return Err("injector session has not entered remote control".into());
        }
        ukvm_protocol::validate_event(event)?;
        let caps = self.injector.capabilities();
        let supported = match event {
            InputEvent::PointerMotion { .. } => caps.pointer_absolute,
            InputEvent::PointerButton { .. } => caps.pointer_button,
            InputEvent::Scroll { .. } => caps.pointer_scroll,
            InputEvent::Key { .. } => caps.keyboard,
        };
        if !supported {
            return Err("injector capability mismatch".into());
        }
        self.injector
            .inject_remote(event)
            .map(|returned| returned.map_or(SinkAction::Continue, SinkAction::Return))
            .map_err(|e| e.to_string())
    }

    fn is_healthy(&self) -> bool {
        !self.active || self.injector.is_healthy()
    }

    fn end_session(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.entered = false;
        let released = self.injector.release_all();
        let ended = self.injector.park();
        released.and(ended).map_err(|e| e.to_string())
    }
}

pub trait DisplayService: Send {
    fn displays(&self) -> Result<Vec<Display>, PlatformError>;
    fn pointer_position(&self) -> Result<Point, PlatformError>;
}

/// Optional future capability. Text and rich formats need explicit policy.
pub trait ClipboardService: Send {
    fn read_text(&self) -> Result<String, PlatformError>;
    fn write_text(&mut self, text: &str) -> Result<(), PlatformError>;
}

/// Generic trait adapters are not wired yet; use `linux_wayland` for the local probe.
pub fn backend_status(kind: BackendKind) -> Result<Capabilities, PlatformError> {
    let name = match kind {
        BackendKind::LinuxWayland => {
            "generic Wayland trait adapter is not implemented; use linux_wayland::probe"
        }
        BackendKind::LinuxX11 => "Linux X11 backend is not implemented",
        BackendKind::MacOs => "macOS backend is not implemented",
        BackendKind::Windows => "Windows backend is not implemented",
    };
    Err(PlatformError::Unsupported(name))
}

#[cfg(test)]
mod injector_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    struct PointerOnly {
        releases: Arc<AtomicUsize>,
        ends: Arc<AtomicUsize>,
    }
    impl PlatformInputInjector for PointerOnly {
        fn capabilities(&self) -> InjectionCapabilities {
            InjectionCapabilities {
                pointer_relative: true,
                pointer_absolute: true,
                ..InjectionCapabilities::default()
            }
        }
        fn begin(&mut self) -> Result<(), PlatformError> {
            Ok(())
        }
        fn inject(&mut self, _: &InputEvent) -> Result<(), PlatformError> {
            Ok(())
        }
        fn enter_remote(&mut self, _: EdgePosition) -> Result<(), PlatformError> {
            Ok(())
        }
        fn release_all(&mut self) -> Result<(), PlatformError> {
            self.releases.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn end(&mut self) -> Result<(), PlatformError> {
            self.ends.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn remote_sink_rejects_unsupported_event_and_releases() {
        let releases = Arc::new(AtomicUsize::new(0));
        let ends = Arc::new(AtomicUsize::new(0));
        let mut sink = RemoteInputSink::new(PointerOnly {
            releases: releases.clone(),
            ends: ends.clone(),
        });
        assert!(sink
            .handle_event(&InputEvent::PointerMotion { dx: 1.0, dy: 0.0 })
            .is_err());
        sink.begin_session().unwrap();
        sink.enter_remote(EdgePosition {
            edge: ukvm_protocol::Edge::Left,
            fraction: 0.5,
        })
        .unwrap();
        sink.handle_event(&InputEvent::PointerMotion { dx: 1.0, dy: 0.0 })
            .unwrap();
        assert!(sink
            .handle_event(&InputEvent::PointerButton {
                button: 1,
                pressed: true
            })
            .is_err());
        assert!(sink
            .handle_event(&InputEvent::PointerMotion {
                dx: f64::NAN,
                dy: 0.0
            })
            .is_err());
        sink.end_session().unwrap();
        assert_eq!(releases.load(Ordering::SeqCst), 1);
        assert_eq!(ends.load(Ordering::SeqCst), 1);
    }

    struct HeldInjector(Arc<Mutex<(bool, bool)>>);

    impl PlatformInputInjector for HeldInjector {
        fn capabilities(&self) -> InjectionCapabilities {
            InjectionCapabilities {
                pointer_relative: true,
                pointer_absolute: true,
                pointer_button: true,
                keyboard: true,
                ..InjectionCapabilities::default()
            }
        }
        fn begin(&mut self) -> Result<(), PlatformError> {
            Ok(())
        }
        fn enter_remote(&mut self, _: EdgePosition) -> Result<(), PlatformError> {
            Ok(())
        }
        fn inject(&mut self, event: &InputEvent) -> Result<(), PlatformError> {
            let mut held = self.0.lock().unwrap();
            match event {
                InputEvent::Key { pressed, .. } => held.0 = *pressed,
                InputEvent::PointerButton { pressed, .. } => held.1 = *pressed,
                _ => {}
            }
            Ok(())
        }
        fn inject_remote(
            &mut self,
            event: &InputEvent,
        ) -> Result<Option<EdgePosition>, PlatformError> {
            if matches!(event, InputEvent::PointerMotion { dx, .. } if *dx < 0.0) {
                return Ok(Some(EdgePosition {
                    edge: ukvm_protocol::Edge::Left,
                    fraction: 0.5,
                }));
            }
            self.inject(event)?;
            Ok(None)
        }
        fn release_all(&mut self) -> Result<(), PlatformError> {
            *self.0.lock().unwrap() = (false, false);
            Ok(())
        }
        fn end(&mut self) -> Result<(), PlatformError> {
            Ok(())
        }
    }

    #[test]
    fn return_release_clears_held_key_and_button() {
        let held = Arc::new(Mutex::new((false, false)));
        let mut sink = RemoteInputSink::new(HeldInjector(Arc::clone(&held)));
        sink.begin_session().unwrap();
        sink.enter_remote(EdgePosition {
            edge: ukvm_protocol::Edge::Left,
            fraction: 0.5,
        })
        .unwrap();
        sink.handle_event(&InputEvent::Key {
            code: ukvm_protocol::KeyCode::LinuxEvdev(42),
            pressed: true,
        })
        .unwrap();
        sink.handle_event(&InputEvent::PointerButton {
            button: 272,
            pressed: true,
        })
        .unwrap();
        assert_eq!(*held.lock().unwrap(), (true, true));
        assert!(matches!(
            sink.handle_event(&InputEvent::PointerMotion { dx: -1.0, dy: 0.0 })
                .unwrap(),
            SinkAction::Return(_)
        ));
        sink.end_session().unwrap();
        assert_eq!(*held.lock().unwrap(), (false, false));
        assert!(sink
            .handle_event(&InputEvent::PointerMotion { dx: 1.0, dy: 0.0 })
            .is_err());
    }

    #[test]
    fn one_remote_sink_can_serve_repeated_sessions() {
        let held = Arc::new(Mutex::new((false, false)));
        let mut sink = RemoteInputSink::new(HeldInjector(Arc::clone(&held)));
        for _ in 0..3 {
            sink.begin_session().unwrap();
            sink.enter_remote(EdgePosition {
                edge: ukvm_protocol::Edge::Left,
                fraction: 0.5,
            })
            .unwrap();
            sink.handle_event(&InputEvent::Key {
                code: ukvm_protocol::KeyCode::LinuxEvdev(42),
                pressed: true,
            })
            .unwrap();
            sink.end_session().unwrap();
            assert_eq!(*held.lock().unwrap(), (false, false));
        }
    }

    struct ReusableInjector {
        grants: Arc<AtomicUsize>,
        parks: Arc<AtomicUsize>,
        closes: Arc<AtomicUsize>,
        granted: bool,
    }

    impl PlatformInputInjector for ReusableInjector {
        fn capabilities(&self) -> InjectionCapabilities {
            InjectionCapabilities {
                pointer_absolute: true,
                ..InjectionCapabilities::default()
            }
        }
        fn begin(&mut self) -> Result<(), PlatformError> {
            if !self.granted {
                self.grants.fetch_add(1, Ordering::SeqCst);
                self.granted = true;
            }
            Ok(())
        }
        fn inject(&mut self, _: &InputEvent) -> Result<(), PlatformError> {
            Ok(())
        }
        fn enter_remote(&mut self, _: EdgePosition) -> Result<(), PlatformError> {
            Ok(())
        }
        fn release_all(&mut self) -> Result<(), PlatformError> {
            Ok(())
        }
        fn park(&mut self) -> Result<(), PlatformError> {
            self.parks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn end(&mut self) -> Result<(), PlatformError> {
            self.granted = false;
            self.closes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn listener_reuses_one_local_grant_then_closes_it() {
        let grants = Arc::new(AtomicUsize::new(0));
        let parks = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        let mut sink = RemoteInputSink::new(ReusableInjector {
            grants: Arc::clone(&grants),
            parks: Arc::clone(&parks),
            closes: Arc::clone(&closes),
            granted: false,
        });
        for _ in 0..3 {
            sink.begin_session().unwrap();
            sink.enter_remote(EdgePosition {
                edge: ukvm_protocol::Edge::Left,
                fraction: 0.5,
            })
            .unwrap();
            sink.end_session().unwrap();
        }
        assert_eq!(grants.load(Ordering::SeqCst), 1);
        assert_eq!(parks.load(Ordering::SeqCst), 3);
        sink.shutdown().unwrap();
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }
}

pub mod files;
