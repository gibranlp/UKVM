//! OS input boundary. Implementations must live under platform-specific modules.

use ukvm_core::controlled::{InputSink, SinkKind};
use ukvm_protocol::InputEvent;

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
    fn inject(&mut self, event: &InputEvent) -> Result<(), PlatformError>;
    fn release_all(&mut self) -> Result<(), PlatformError>;
    fn end(&mut self) -> Result<(), PlatformError>;
}

/// Binds a locally authorized platform injector to the network-facing sink.
/// The receiver must still explicitly select RemoteInput and authorize each session.
pub struct RemoteInputSink<I: PlatformInputInjector> {
    injector: I,
    active: bool,
}

impl<I: PlatformInputInjector> RemoteInputSink<I> {
    pub fn new(injector: I) -> Self {
        Self {
            injector,
            active: false,
        }
    }
}

impl<I: PlatformInputInjector> InputSink for RemoteInputSink<I> {
    fn kind(&self) -> SinkKind {
        SinkKind::RemoteInput
    }

    fn begin_session(&mut self) -> Result<(), String> {
        self.injector.begin().map_err(|e| e.to_string())?;
        self.active = true;
        Ok(())
    }

    fn handle_event(&mut self, event: &InputEvent) -> Result<(), String> {
        if !self.active {
            return Err("injector session is not active".into());
        }
        ukvm_protocol::validate_event(event)?;
        let caps = self.injector.capabilities();
        let supported = match event {
            InputEvent::PointerMotion { .. } => caps.pointer_relative,
            InputEvent::PointerButton { .. } => caps.pointer_button,
            InputEvent::Scroll { .. } => caps.pointer_scroll,
            InputEvent::Key { .. } => caps.keyboard,
        };
        if !supported {
            return Err("injector capability mismatch".into());
        }
        self.injector.inject(event).map_err(|e| e.to_string())
    }

    fn end_session(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let released = self.injector.release_all();
        let ended = self.injector.end();
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
        Arc,
    };

    struct PointerOnly {
        releases: Arc<AtomicUsize>,
        ends: Arc<AtomicUsize>,
    }
    impl PlatformInputInjector for PointerOnly {
        fn capabilities(&self) -> InjectionCapabilities {
            InjectionCapabilities {
                pointer_relative: true,
                ..InjectionCapabilities::default()
            }
        }
        fn begin(&mut self) -> Result<(), PlatformError> {
            Ok(())
        }
        fn inject(&mut self, _: &InputEvent) -> Result<(), PlatformError> {
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
}
