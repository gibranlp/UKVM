//! Portal-based Wayland probing and experimental input capture.
//! D-Bus and EI details stay in this module.

use crate::CapabilityEvidence;
use ashpd::desktop::input_capture::{
    ActivatedBarrier, Barrier, BarrierID, BarrierPosition, Capabilities as CaptureFlags,
    CreateSession2Options, CreateSessionOptions, InputCapture, StartOptions,
};
use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop};
use ashpd::desktop::Session;
use futures_util::StreamExt;
use reis::{
    ei::{self, button::ButtonState, keyboard::KeyState},
    event::{DeviceCapability, EiEvent},
};
use std::{collections::HashSet, os::unix::net::UnixStream, path::PathBuf, time::Duration};
use ukvm_protocol::{EdgePosition, InputEvent, KeyCode};

mod geometry;
mod pointer_position;
pub mod portal_session;
pub use portal_session as portal_trial;
mod rearm;
mod return_edge;
mod service_session;
pub use geometry::{
    all_outer_segments, outer_segments, Edge, EdgeSpan, OuterSegment, Zone, ZoneSet,
};
pub use pointer_position::local_pointer_position;
pub use rearm::{EmergencyRearmGuard, EMERGENCY_REARM_DISTANCE};
pub use service_session::{
    service_environment, service_portal_ready, ServiceEnvironment, SESSION_ENV,
};

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub session_type_hint: Option<String>,
    pub wayland_display: Option<String>,
    pub wayland_socket_reachable: bool,
    pub x11_display_hint: Option<String>,
    pub desktop_hint: Option<String>,
}

impl SessionInfo {
    pub fn detect() -> Self {
        let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
        let wayland_socket_reachable = wayland_display.as_ref().is_some_and(|name| {
            let path = if name.starts_with('/') {
                PathBuf::from(name)
            } else if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
                PathBuf::from(dir).join(name)
            } else {
                return false;
            };
            UnixStream::connect(path).is_ok()
        });
        Self {
            session_type_hint: std::env::var("XDG_SESSION_TYPE").ok(),
            wayland_display,
            wayland_socket_reachable,
            x11_display_hint: std::env::var("DISPLAY").ok(),
            desktop_hint: std::env::var("XDG_CURRENT_DESKTOP").ok(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct InterfaceProbe {
    pub version: Option<u32>,
    pub keyboard_advertised: bool,
    pub pointer_advertised: bool,
    pub error: Option<String>,
}

impl InterfaceProbe {
    fn unavailable(error: impl ToString) -> Self {
        Self {
            version: None,
            keyboard_advertised: false,
            pointer_advertised: false,
            error: Some(error.to_string()),
        }
    }
}

/// What the read-only RemoteDesktop portal probe permits us to infer. A
/// portal property never proves that an EI sender can connect or inject.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InjectionPortalCapability {
    pub portal_available: bool,
    pub eis_api_available: bool,
    pub pointer_advertised: bool,
    pub keyboard_advertised: bool,
}

impl InjectionPortalCapability {
    pub fn from_probe(probe: &InterfaceProbe) -> Self {
        Self {
            portal_available: probe.version.is_some(),
            eis_api_available: probe.version.is_some_and(|version| version >= 2),
            pointer_advertised: probe.pointer_advertised,
            keyboard_advertised: probe.keyboard_advertised,
        }
    }
}

mod inject;
pub use inject::WaylandInputInjector;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WaylandInjectionEvidence {
    pub pointer: CapabilityEvidence,
    pub button: CapabilityEvidence,
    pub scroll: CapabilityEvidence,
    pub keyboard: CapabilityEvidence,
}

pub fn injection_evidence(probe: &InterfaceProbe) -> WaylandInjectionEvidence {
    let eis = probe.version.is_some_and(|version| version >= 2);
    let pointer = CapabilityEvidence {
        detected: eis && probe.pointer_advertised,
        implemented: true,
        ..CapabilityEvidence::default()
    };
    WaylandInjectionEvidence {
        pointer,
        button: pointer,
        scroll: pointer,
        keyboard: CapabilityEvidence {
            detected: eis && probe.keyboard_advertised,
            implemented: true,
            ..CapabilityEvidence::default()
        },
    }
}

#[derive(Clone, Debug)]
pub struct ProbeReport {
    pub session: SessionInfo,
    pub portal_service: bool,
    pub portal_error: Option<String>,
    pub capture: InterfaceProbe,
    pub remote: InterfaceProbe,
}

/// Read-only capability probe. This does not request portal permission or open an EI connection.
pub async fn probe() -> ProbeReport {
    let session = SessionInfo::detect();
    let connection = match zbus::Connection::session().await {
        Ok(value) => value,
        Err(err) => {
            let message = format!("session bus unavailable: {err}");
            return ProbeReport {
                session,
                portal_service: false,
                portal_error: Some(message.clone()),
                capture: InterfaceProbe::unavailable(&message),
                remote: InterfaceProbe::unavailable(message),
            };
        }
    };
    // Ping the well-known portal name so a D-Bus activatable portal counts as available.
    let portal_service = match zbus::Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.DBus.Peer",
    )
    .await
    {
        Ok(proxy) => proxy.call::<_, _, ()>("Ping", &()).await.is_ok(),
        Err(_) => false,
    };
    if !portal_service {
        let message = "org.freedesktop.portal.Desktop did not answer on the session bus";
        return ProbeReport {
            session,
            portal_service,
            portal_error: Some(message.into()),
            capture: InterfaceProbe::unavailable(message),
            remote: InterfaceProbe::unavailable(message),
        };
    }
    let capture = match InputCapture::with_connection(connection.clone()).await {
        Ok(portal) => match portal.supported_capabilities().await {
            Ok(flags) => InterfaceProbe {
                version: Some(portal.version()),
                keyboard_advertised: flags.contains(CaptureFlags::Keyboard),
                pointer_advertised: flags.contains(CaptureFlags::Pointer),
                error: None,
            },
            Err(err) => InterfaceProbe::unavailable(err),
        },
        Err(err) => InterfaceProbe::unavailable(err),
    };
    let remote = match RemoteDesktop::with_connection(connection).await {
        Ok(portal) => match portal.available_device_types().await {
            Ok(flags) => InterfaceProbe {
                version: Some(portal.version()),
                keyboard_advertised: flags.contains(DeviceType::Keyboard),
                pointer_advertised: flags.contains(DeviceType::Pointer),
                error: None,
            },
            Err(err) => InterfaceProbe::unavailable(err),
        },
        Err(err) => InterfaceProbe::unavailable(err),
    };
    ProbeReport {
        session,
        portal_service,
        portal_error: None,
        capture,
        remote,
    }
}

async fn capture_setup<T, E: std::fmt::Display, H: FnMut() -> bool>(
    future: impl std::future::Future<Output = Result<T, E>>,
    healthy: &mut H,
    timeout: Duration,
) -> Result<T, String> {
    if !healthy() {
        return Err("capture setup cancelled before creating authority".into());
    }
    tokio::pin!(future);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            result = &mut future => return result.map_err(|e| e.to_string()),
            _ = &mut deadline => return Err("InputCapture setup timed out".into()),
            _ = tokio::signal::ctrl_c() => return Err("local shutdown during capture setup".into()),
            _ = tick.tick() => if !healthy() { return Err("peer unavailable or local shutdown during capture setup".into()); },
        }
    }
}

struct CaptureSession {
    portal: InputCapture,
    session: Session<InputCapture>,
    keyboard_granted: bool,
    bus: zbus::Connection,
}

impl CaptureSession {
    async fn open() -> Result<Self, String> {
        Self::open_cancellable(&mut || true).await
    }

    async fn open_cancellable<H: FnMut() -> bool>(healthy: &mut H) -> Result<Self, String> {
        let bus = capture_setup(
            zbus::Connection::session(),
            healthy,
            Duration::from_secs(10),
        )
        .await?;
        // Each capture owns its bus connection. If consent/session creation is
        // cancelled, explicitly disconnect that portal client so even a request
        // whose session handle has not arrived cannot retain input authority.
        let result = capture_setup(
            Self::open_on_bus(bus.clone()),
            healthy,
            Duration::from_secs(125),
        )
        .await;
        if result.is_err() {
            let _ = tokio::time::timeout(Duration::from_secs(4), bus.close()).await;
        }
        result
    }

    async fn open_on_bus(bus: zbus::Connection) -> Result<Self, String> {
        let portal = InputCapture::with_connection(bus.clone())
            .await
            .map_err(|e| e.to_string())?;
        let requested = CaptureFlags::Keyboard | CaptureFlags::Pointer;
        let (session, flags) = if portal.version() >= 2 {
            let session = portal
                .create_session2(CreateSession2Options::default())
                .await
                .map_err(|e| e.to_string())?;
            let result = portal
                .start(
                    &session,
                    None,
                    StartOptions::default().set_capabilities(requested),
                )
                .await
                .and_then(|request| request.response().map(|r| r.capabilities()));
            let flags = match result {
                Ok(flags) => flags,
                Err(err) => {
                    let _ = session.close().await;
                    return Err(format!("InputCapture permission/start failed: {err}"));
                }
            };
            (session, flags)
        } else {
            let (session, flags) = portal
                .create_session(
                    None,
                    CreateSessionOptions::default().set_capabilities(requested),
                )
                .await
                .map_err(|e| format!("InputCapture permission/session failed: {e}"))?;
            (session, flags)
        };
        if !flags.contains(CaptureFlags::Pointer) {
            let _ = session.close().await;
            return Err("InputCapture session did not grant pointer capture".into());
        }
        Ok(Self {
            portal,
            session,
            keyboard_granted: flags.contains(CaptureFlags::Keyboard),
            bus,
        })
    }

    async fn zones(&self) -> Result<ZoneSet, String> {
        let response = self
            .portal
            .zones(&self.session, Default::default())
            .await
            .map_err(|e| e.to_string())?
            .response()
            .map_err(|e| e.to_string())?;
        Ok(ZoneSet {
            id: response.zone_set(),
            zones: response
                .regions()
                .iter()
                .map(|r| Zone {
                    x: r.x_offset(),
                    y: r.y_offset(),
                    width: r.width(),
                    height: r.height(),
                })
                .collect(),
        })
    }

    async fn close(self) -> Result<(), String> {
        let disabled = tokio::time::timeout(
            Duration::from_secs(4),
            self.portal.disable(&self.session, Default::default()),
        )
        .await;
        let closed = tokio::time::timeout(Duration::from_secs(4), self.session.close()).await;
        // Disconnect even if Disable/Close failed or the portal disappeared.
        let disconnected = tokio::time::timeout(Duration::from_secs(4), self.bus.close()).await;
        disabled
            .map_err(|_| "InputCapture Disable timed out")?
            .map_err(|e| format!("InputCapture Disable failed: {e}"))?;
        closed
            .map_err(|_| "InputCapture Close timed out")?
            .map_err(|e| format!("InputCapture session Close failed: {e}"))?;
        disconnected
            .map_err(|_| "InputCapture bus close timed out")?
            .map_err(|e| e.to_string())
    }
}

/// Opens a permissioned session, reads all zones, and closes the session.
pub async fn zones() -> Result<ZoneSet, String> {
    let session = CaptureSession::open().await?;
    let result = session.zones().await;
    // A zones-only session was never connected to EIS or enabled. Some
    // backends reject Disable in that state, but Session.Close is valid.
    let close = session.session.close().await.map_err(|e| e.to_string());
    match (result, close) {
        (Ok(zones), Ok(())) => Ok(zones),
        (Err(err), _) | (_, Err(err)) => Err(err),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BarrierRequest {
    pub id: u32,
    pub segment: OuterSegment,
}

/// Selects external segments on one zone edge, or all reachable external
/// segments in this direction across the permissioned portal zones.
pub fn select_barriers(
    zones: &[Zone],
    edge: Edge,
    zone: Option<usize>,
) -> Result<Vec<BarrierRequest>, String> {
    let segments = if let Some(index) = zone {
        outer_segments(zones, index, edge)?
    } else {
        all_outer_segments(zones)?
            .into_iter()
            .filter(|segment| segment.edge == edge)
            .collect()
    };
    if segments.is_empty() {
        return Err(format!(
            "no outer {} edge exists for the selected zone",
            edge.as_str()
        ));
    }
    segments
        .into_iter()
        .enumerate()
        .map(|(index, segment)| {
            let id = u32::try_from(index + 1).map_err(|_| "too many barrier segments")?;
            Ok(BarrierRequest { id, segment })
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BarrierDecision {
    pub accepted: Vec<u32>,
    pub rejected: Vec<u32>,
}

/// `failed_barriers` is a negative list, not a success list.
pub fn validate_barriers(
    requested: &[BarrierRequest],
    failed: &[u32],
) -> Result<BarrierDecision, String> {
    if failed
        .iter()
        .any(|id| !requested.iter().any(|item| item.id == *id))
    {
        return Err("portal returned an unknown failed barrier ID".into());
    }
    let mut rejected = Vec::new();
    let mut accepted = Vec::new();
    for item in requested {
        if failed.contains(&item.id) {
            rejected.push(item.id);
        } else {
            accepted.push(item.id);
        }
    }
    Ok(BarrierDecision { accepted, rejected })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapturePhase {
    CreatingSession,
    Configuring,
    WaitingForBarrier,
    Captured,
    Releasing,
    Closed,
}

impl CapturePhase {
    fn advance(self, next: Self) -> Result<Self, String> {
        let valid = matches!(
            (self, next),
            (Self::CreatingSession, Self::Configuring)
                | (Self::Configuring, Self::WaitingForBarrier)
                | (Self::WaitingForBarrier, Self::Captured)
                | (Self::Captured, Self::WaitingForBarrier)
                | (
                    Self::Configuring | Self::WaitingForBarrier | Self::Captured,
                    Self::Releasing
                )
                | (Self::Releasing, Self::Closed)
        );
        valid
            .then_some(next)
            .ok_or_else(|| format!("invalid capture state transition: {self:?} -> {next:?}"))
    }
}

#[derive(Clone, Debug)]
pub enum CaptureNotice {
    /// Lifecycle metadata only; never individual input events.
    Lifecycle {
        phase: CapturePhase,
        detail: String,
    },
    SessionCreated {
        version: u32,
        keyboard_granted: bool,
    },
    Zones(ZoneSet),
    EisSocketAcquired,
    EisContextConnected,
    EisHandshakeComplete,
    SeatAvailable,
    DeviceAvailable {
        pointer: bool,
        keyboard: bool,
        button: bool,
        scroll: bool,
    },
    BarriersRequested(Vec<BarrierRequest>),
    BarrierDecision(BarrierDecision),
    Enabled,
    Activated {
        activation_id: Option<u32>,
        barrier_id: Option<u32>,
        edge_fraction: f32,
        edge: Option<Edge>,
    },
    RemoteReturn(EdgePosition),
    EmergencyRelease {
        boundary: Option<OuterSegment>,
    },
    Deactivated {
        activation_id: Option<u32>,
    },
    ZonesChanged {
        zone_set: Option<u32>,
    },
    EisStarted {
        sequence: u32,
    },
    EisStopped,
    EventCounts(CaptureSummary),
    ReleaseRequested {
        activation_id: Option<u32>,
        cursor_position: Option<(f64, f64)>,
    },
    ReleaseResult(Result<(), String>),
    Closed(Result<(), String>),
}

#[derive(Clone, Debug, Default)]
pub struct CaptureSummary {
    pub activations: u64,
    pub deactivations: u64,
    pub eis_starts: u64,
    pub pointer_events: u64,
    pub button_events: u64,
    pub scroll_events: u64,
    pub key_events: u64,
    pub release_succeeded: Option<bool>,
    pub timed_out: bool,
}

/// Experimental local-only capture. Event values are normalized in memory and never printed.
pub async fn capture<F: FnMut(CaptureNotice)>(
    edge: Edge,
    zone_index: Option<usize>,
    duration: Duration,
    on_notice: &mut F,
) -> Result<CaptureSummary, String> {
    capture_with_events(
        edge,
        zone_index,
        duration,
        on_notice,
        &mut |_| Ok(()),
        &mut || true,
    )
    .await
}

/// Streams normalized events to a bounded consumer. The health callback is
/// polled even without input, so a lost peer releases local capture promptly.
pub async fn capture_with_events<F, G, H>(
    edge: Edge,
    zone_index: Option<usize>,
    duration: Duration,
    on_notice: &mut F,
    on_event: &mut G,
    is_healthy: &mut H,
) -> Result<CaptureSummary, String>
where
    F: FnMut(CaptureNotice),
    G: FnMut(InputEvent) -> Result<(), String>,
    H: FnMut() -> bool,
{
    capture_with_events_mode(
        edge,
        zone_index,
        CaptureTiming {
            duration,
            active_duration: Some(Duration::from_secs(5)),
            release_mode: ReleaseMode::Escape,
        },
        on_notice,
        on_event,
        is_healthy,
        &mut || None,
    )
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseMode {
    Escape,
    EmergencyChord,
}

#[derive(Clone, Copy, Debug)]
pub struct CaptureTiming {
    pub duration: Duration,
    pub active_duration: Option<Duration>,
    pub release_mode: ReleaseMode,
}

/// Remote sessions reserve Ctrl+Alt+Shift+Escape for local release. Escape
/// remains an ordinary forwarded key unless all three modifiers are held.
pub async fn capture_with_events_mode<F, G, H, R>(
    edge: Edge,
    zone_index: Option<usize>,
    timing: CaptureTiming,
    on_notice: &mut F,
    on_event: &mut G,
    is_healthy: &mut H,
    return_request: &mut R,
) -> Result<CaptureSummary, String>
where
    F: FnMut(CaptureNotice),
    G: FnMut(InputEvent) -> Result<(), String>,
    H: FnMut() -> bool,
    R: FnMut() -> Option<EdgePosition>,
{
    capture_edges_with_events_mode(
        &[edge],
        zone_index,
        timing,
        on_notice,
        on_event,
        is_healthy,
        return_request,
    )
    .await
}

/// One permissioned capture session with barriers for authenticated ready edges.
pub async fn capture_edges_with_events_mode<F, G, H, R>(
    edges: &[Edge],
    zone_index: Option<usize>,
    timing: CaptureTiming,
    on_notice: &mut F,
    on_event: &mut G,
    is_healthy: &mut H,
    return_request: &mut R,
) -> Result<CaptureSummary, String>
where
    F: FnMut(CaptureNotice),
    G: FnMut(InputEvent) -> Result<(), String>,
    H: FnMut() -> bool,
    R: FnMut() -> Option<EdgePosition>,
{
    if edges.is_empty()
        || edges.len() > 4
        || edges
            .iter()
            .enumerate()
            .any(|(i, edge)| edges[..i].contains(edge))
    {
        return Err("capture requires 1–4 distinct ready edges".into());
    }
    let session = CaptureSession::open_cancellable(is_healthy).await?;
    if timing.release_mode == ReleaseMode::EmergencyChord && !session.keyboard_granted {
        let _ = session.close().await;
        return Err(
            "InputCapture did not grant keyboard capture required for emergency release".into(),
        );
    }
    on_notice(CaptureNotice::SessionCreated {
        version: session.portal.version(),
        keyboard_granted: session.keyboard_granted,
    });
    let mut phase = CapturePhase::CreatingSession.advance(CapturePhase::Configuring)?;
    let mut hooks = CaptureHooks {
        on_event,
        is_healthy,
        return_request,
    };
    let result = capture_inner(
        &session, edges, zone_index, timing, on_notice, &mut hooks, &mut phase,
    )
    .await;
    if let Err(error) = &result {
        on_notice(CaptureNotice::Lifecycle {
            phase,
            detail: format!("capture ended: {error}"),
        });
    }
    if phase != CapturePhase::Releasing {
        phase = phase.advance(CapturePhase::Releasing)?;
    }
    on_notice(CaptureNotice::Lifecycle {
        phase,
        detail: "requesting Disable, Session.Close and client bus disconnect".into(),
    });
    let close = session.close().await;
    phase = phase.advance(CapturePhase::Closed)?;
    on_notice(CaptureNotice::Lifecycle {
        phase,
        detail: format!("cleanup result: {close:?}"),
    });
    on_notice(CaptureNotice::Closed(close.clone()));
    match (result, close) {
        (Ok(summary), Ok(())) => Ok(summary),
        (Err(err), _) | (_, Err(err)) => Err(err),
    }
}

struct CaptureHooks<'a, G, H, R> {
    on_event: &'a mut G,
    is_healthy: &'a mut H,
    return_request: &'a mut R,
}

async fn capture_inner<F, G, H, R>(
    session: &CaptureSession,
    edges: &[Edge],
    zone_index: Option<usize>,
    timing: CaptureTiming,
    on_notice: &mut F,
    hooks: &mut CaptureHooks<'_, G, H, R>,
    phase: &mut CapturePhase,
) -> Result<CaptureSummary, String>
where
    F: FnMut(CaptureNotice),
    G: FnMut(InputEvent) -> Result<(), String>,
    H: FnMut() -> bool,
    R: FnMut() -> Option<EdgePosition>,
{
    let mut activated = session
        .portal
        .receive_activated()
        .await
        .map_err(|e| e.to_string())?;
    let mut deactivated = session
        .portal
        .receive_deactivated()
        .await
        .map_err(|e| e.to_string())?;
    let mut zones_changed = session
        .portal
        .receive_zones_changed()
        .await
        .map_err(|e| e.to_string())?;
    let mut closed = session
        .session
        .receive_closed()
        .await
        .map_err(|e| e.to_string())?;
    let mut disabled = session
        .portal
        .receive_disabled()
        .await
        .map_err(|e| e.to_string())?;
    on_notice(CaptureNotice::Lifecycle {
        phase: *phase,
        detail: "subscribed Activated, Deactivated, ZonesChanged, Disabled and Session.Closed; connecting EIS before barriers/Enable".into(),
    });
    // ConnectToEIS must precede Enable. Keep the EI receiver alive throughout
    // every enabled/active cycle of this portal session.
    let fd = capture_setup(
        session
            .portal
            .connect_to_eis(&session.session, Default::default()),
        hooks.is_healthy,
        Duration::from_secs(10),
    )
    .await?;
    on_notice(CaptureNotice::EisSocketAcquired);
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
    let context = ei::Context::new(stream).map_err(|e| e.to_string())?;
    on_notice(CaptureNotice::EisContextConnected);
    let (_connection, mut events) = capture_setup(
        context.handshake_tokio("ukvm", ei::handshake::ContextType::Receiver),
        hooks.is_healthy,
        Duration::from_secs(10),
    )
    .await?;
    on_notice(CaptureNotice::EisHandshakeComplete);
    let zone_set =
        capture_setup(session.zones(), hooks.is_healthy, Duration::from_secs(10)).await?;
    on_notice(CaptureNotice::Zones(zone_set.clone()));
    let mut requested = Vec::new();
    for edge in edges {
        for mut barrier in select_barriers(&zone_set.zones, *edge, zone_index)? {
            barrier.id = requested.len() as u32 + 1;
            requested.push(barrier);
        }
    }
    on_notice(CaptureNotice::BarriersRequested(requested.clone()));
    let barriers = requested
        .iter()
        .map(|item| {
            let id = BarrierID::new(item.id).expect("barrier ids start at one");
            let segment = item.segment;
            Barrier::new(
                id,
                BarrierPosition::new(
                    segment.start.0,
                    segment.start.1,
                    segment.end.0,
                    segment.end.1,
                ),
            )
        })
        .collect::<Vec<_>>();
    let response = capture_setup(
        session.portal.set_pointer_barriers(
            &session.session,
            &barriers,
            zone_set.id,
            Default::default(),
        ),
        hooks.is_healthy,
        Duration::from_secs(10),
    )
    .await?
    .response()
    .map_err(|e| e.to_string())?;
    let failed = response
        .failed_barriers()
        .iter()
        .map(|id| id.get())
        .collect::<Vec<_>>();
    let decision = validate_barriers(&requested, &failed)?;
    on_notice(CaptureNotice::BarrierDecision(decision.clone()));
    if !decision.rejected.is_empty() {
        return Err(format!(
            "portal rejected barrier IDs {:?}",
            decision.rejected
        ));
    }
    on_notice(CaptureNotice::Lifecycle {
        phase: *phase,
        detail: format!("SetPointerBarriers response received: zone_set={}, {} barriers; requesting Enable (portal acceptance is not proof of compositor activation)", zone_set.id, requested.len()),
    });
    capture_setup(
        session.portal.enable(&session.session, Default::default()),
        hooks.is_healthy,
        Duration::from_secs(10),
    )
    .await?;
    *phase = phase.advance(CapturePhase::WaitingForBarrier)?;
    on_notice(CaptureNotice::Lifecycle {
        phase: *phase,
        detail: "Enable completed; waiting for portal Activated and EI start_emulating".into(),
    });
    on_notice(CaptureNotice::Enabled);
    let mut summary = CaptureSummary::default();
    let deadline = tokio::time::sleep(timing.duration);
    tokio::pin!(deadline);
    let mut portal_active = false;
    let mut eis_active = false;
    let mut activation_id = None;
    let mut triggered_barrier = None;
    let mut active_until = None;
    let mut abort_reason = None;
    let mut returned = None;
    let mut health_tick = tokio::time::interval(Duration::from_millis(100));
    let mut held_keys = HashSet::new();
    loop {
        tokio::select! {
            _ = health_tick.tick() => {
                if let Some(position) = (hooks.return_request)() {
                    returned = Some(position);
                    on_notice(CaptureNotice::RemoteReturn(position));
                    break;
                }
                if !(hooks.is_healthy)() { abort_reason = Some("peer connection lost; releasing local InputCapture".into()); break; }
            },
            _ = &mut deadline, if !portal_active => { summary.timed_out = true; break; },
            _ = async {
                if let Some(until) = active_until {
                    tokio::time::sleep_until(until).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => break,
            _ = tokio::signal::ctrl_c() => break,
            signal = activated.next() => {
                let Some(signal) = signal else { abort_reason = Some("InputCapture Activated signal stream closed".into()); break; };
                on_notice(CaptureNotice::Lifecycle {
                    phase: *phase,
                    detail: format!("portal Activated: activation_id={:?}, barrier={:?}, cursor={:?}", signal.activation_id(), signal.barrier_id(), signal.cursor_position()),
                });
                portal_active = true;
                activation_id = signal.activation_id();
                triggered_barrier = match signal.barrier_id() {
                    Some(ActivatedBarrier::Barrier(id)) => Some(id.get()),
                    _ => None,
                };
                summary.activations += 1;
                if *phase == CapturePhase::Captured { abort_reason = Some("duplicate InputCapture activation".into()); break; }
                *phase = phase.advance(CapturePhase::Captured)?;
                active_until = timing.active_duration.map(|duration| tokio::time::Instant::now() + duration);
                let (edge, edge_fraction) = activation_geometry(&requested, triggered_barrier, signal.cursor_position());
                on_notice(CaptureNotice::Activated { activation_id, barrier_id: triggered_barrier, edge_fraction, edge });
            }
            signal = deactivated.next() => {
                let Some(signal) = signal else { abort_reason = Some("InputCapture Deactivated signal stream closed".into()); break; };
                summary.deactivations += 1;
                on_notice(CaptureNotice::Deactivated { activation_id: signal.activation_id() });
                if signal.activation_id().is_none() || signal.activation_id() == activation_id {
                    portal_active = false;
                    active_until = None;
                    if *phase == CapturePhase::Captured {
                        *phase = phase.advance(CapturePhase::WaitingForBarrier)?;
                    }
                    if timing.release_mode == ReleaseMode::EmergencyChord {
                        abort_reason = Some("InputCapture deactivated during remote control; stopping session".into());
                        break;
                    }
                }
            }
            signal = zones_changed.next() => {
                let Some(signal) = signal else { abort_reason = Some("InputCapture ZonesChanged signal stream closed".into()); break; };
                on_notice(CaptureNotice::ZonesChanged { zone_set: signal.zone_set() });
                abort_reason = Some("display zones changed; capture stopped so barriers can be rebuilt".to_string());
                break;
            }
            signal = closed.next() => {
                let Some(details) = signal else { abort_reason = Some("InputCapture Session.Closed signal stream ended".into()); break; };
                abort_reason = Some(portal_closed_reason(&details));
                break;
            }
            signal = disabled.next() => {
                abort_reason = Some(if signal.is_some() {
                    "portal Disabled: capture no longer enabled".into()
                } else {
                    "InputCapture Disabled signal stream ended".into()
                });
                break;
            }
            event = events.next() => {
                let event = match event {
                    Some(Ok(event)) => event,
                    Some(Err(err)) => { abort_reason = Some(format!("EIS error: {err}")); break; },
                    None => { abort_reason = Some("EIS connection closed".into()); break; },
                };
                match event {
                    EiEvent::SeatAdded(seat) => {
                        seat.seat.bind_capabilities(DeviceCapability::Pointer | DeviceCapability::Keyboard | DeviceCapability::Button | DeviceCapability::Scroll);
                        if let Err(err) = context.flush() { abort_reason = Some(format!("EIS flush failed: {err}")); break; }
                        on_notice(CaptureNotice::SeatAvailable);
                    }
                    EiEvent::DeviceAdded(device) => on_notice(CaptureNotice::DeviceAvailable {
                        pointer: device.device.has_capability(DeviceCapability::Pointer),
                        keyboard: device.device.has_capability(DeviceCapability::Keyboard),
                        button: device.device.has_capability(DeviceCapability::Button),
                        scroll: device.device.has_capability(DeviceCapability::Scroll),
                    }),
                    EiEvent::Disconnected(disconnected) => {
                        abort_reason = Some(eis_disconnect_reason(&disconnected, *phase, portal_active, eis_active));
                        break;
                    },
                    EiEvent::DeviceRemoved(_) => on_notice(CaptureNotice::Lifecycle { phase: *phase, detail: "EI device removed".into() }),
                    EiEvent::DevicePaused(_) => on_notice(CaptureNotice::Lifecycle { phase: *phase, detail: "EI device paused".into() }),
                    EiEvent::DeviceResumed(_) => on_notice(CaptureNotice::Lifecycle { phase: *phase, detail: "EI device resumed".into() }),
                    EiEvent::SeatRemoved(_) => on_notice(CaptureNotice::Lifecycle { phase: *phase, detail: "EI seat removed".into() }),
                    EiEvent::DeviceStartEmulating(start) => {
                        eis_active = true;
                        summary.eis_starts += 1;
                        on_notice(CaptureNotice::EisStarted { sequence: start.sequence });
                    }
                    EiEvent::DeviceStopEmulating(_) => {
                        eis_active = false;
                        on_notice(CaptureNotice::EisStopped);
                        if timing.release_mode == ReleaseMode::EmergencyChord && portal_active {
                            abort_reason = Some("EI stopped emulating during remote control".into());
                            break;
                        }
                    },
                    other if portal_active || (eis_active && timing.release_mode == ReleaseMode::Escape) => {
                        if let Some(position) = (hooks.return_request)() {
                            returned = Some(position);
                            on_notice(CaptureNotice::RemoteReturn(position));
                            break;
                        }
                        if let Some(input) = normalize_event(&other) {
                            if !(hooks.is_healthy)() { abort_reason = Some("peer connection lost; releasing local InputCapture".into()); break; }
                            if let InputEvent::Key { code: KeyCode::LinuxEvdev(code), pressed } = &input {
                                if *pressed { held_keys.insert(*code); } else { held_keys.remove(code); }
                                if *pressed && *code == 1 && match timing.release_mode {
                                    ReleaseMode::Escape => true,
                                    ReleaseMode::EmergencyChord => emergency_modifiers_held(&held_keys),
                                } {
                                    if timing.release_mode == ReleaseMode::EmergencyChord {
                                        let boundary = triggered_barrier
                                            .and_then(|id| requested.iter().find(|item| item.id == id))
                                            .or_else(|| (requested.len() == 1).then(|| &requested[0]))
                                            .map(|item| item.segment);
                                        on_notice(CaptureNotice::EmergencyRelease { boundary });
                                    }
                                    break;
                                }
                            }
                            if let Err(err) = (hooks.on_event)(input.clone()) { abort_reason = Some(format!("event forwarding stopped: {err}")); break; }
                            match input {
                                InputEvent::PointerMotion { .. } => summary.pointer_events += 1,
                                InputEvent::PointerButton { .. } => summary.button_events += 1,
                                InputEvent::Scroll { .. } => summary.scroll_events += 1,
                                InputEvent::Key { .. } => summary.key_events += 1,
                            }
                            let total = summary.pointer_events + summary.button_events + summary.scroll_events + summary.key_events;
                            if total == 1 || total % 100 == 0 || matches!(input, InputEvent::Key { .. }) {
                                on_notice(CaptureNotice::EventCounts(summary.clone()));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    *phase = phase.advance(CapturePhase::Releasing)?;
    if portal_active || eis_active {
        let cursor_position = triggered_barrier
            .and_then(|id| requested.iter().find(|item| item.id == id))
            .or_else(|| (requested.len() == 1).then(|| &requested[0]))
            .map(|item| {
                let segments = requested
                    .iter()
                    .map(|item| item.segment)
                    .collect::<Vec<_>>();
                returned.map_or_else(
                    || item.segment.release_position(),
                    |position| {
                        EdgeSpan::new(&segments, item.segment.edge)
                            .map(|span| span.release_position(item.segment, position.fraction))
                            .unwrap_or_else(|_| item.segment.release_position())
                    },
                )
            });
        on_notice(CaptureNotice::ReleaseRequested {
            activation_id,
            cursor_position,
        });
        let options = ashpd::desktop::input_capture::ReleaseOptions::default()
            .set_activation_id(activation_id)
            .set_cursor_position(cursor_position);
        let release = session
            .portal
            .release(&session.session, options)
            .await
            .map_err(|e| e.to_string());
        summary.release_succeeded = Some(release.is_ok());
        on_notice(CaptureNotice::ReleaseResult(release.clone()));
        release?;
    }
    if let Some(reason) = abort_reason {
        Err(reason)
    } else {
        Ok(summary)
    }
}

/// Use the compositor-selected ID, never infer a different edge from cursor position.
fn activation_geometry(
    requested: &[BarrierRequest],
    barrier_id: Option<u32>,
    cursor: Option<(f32, f32)>,
) -> (Option<Edge>, f32) {
    let Some(item) = barrier_id.and_then(|id| requested.iter().find(|item| item.id == id)) else {
        return (None, 0.5);
    };
    let segments = requested
        .iter()
        .map(|item| item.segment)
        .collect::<Vec<_>>();
    let fraction = cursor
        .and_then(|cursor| {
            EdgeSpan::new(&segments, item.segment.edge)
                .ok()
                .map(|span| span.cursor_fraction(item.segment, cursor))
        })
        .unwrap_or(0.5);
    (Some(item.segment.edge), fraction)
}

fn lifecycle_text(text: &str) -> String {
    text.chars()
        .take(256)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn portal_closed_reason(
    details: &std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
) -> String {
    let mut keys = details
        .keys()
        .map(|key| lifecycle_text(key))
        .collect::<Vec<_>>();
    keys.sort();
    let reason = ["reason", "message", "error"]
        .iter()
        .filter_map(|key| {
            details
                .get(*key)
                .and_then(|value| value.downcast_ref::<&str>().ok())
                .map(|value| format!("{key}={}", lifecycle_text(value)))
        })
        .collect::<Vec<_>>();
    format!(
        "portal Session.Closed; detail keys={keys:?}; {}",
        if reason.is_empty() {
            "no reason supplied (Session.Closed defines no standardized reason)".into()
        } else {
            reason.join("; ")
        }
    )
}

fn eis_disconnect_reason(
    disconnected: &reis::event::Disconnected,
    phase: CapturePhase,
    portal_active: bool,
    eis_active: bool,
) -> String {
    // This is backend lifecycle metadata, not EI wire tracing (which can expose input).
    let explanation = lifecycle_text(
        disconnected
            .explanation
            .as_deref()
            .unwrap_or("not supplied"),
    );
    format!("EIS disconnected: reason={:?}, last_serial={}, explanation={explanation}; phase={phase:?}, portal_active={portal_active}, eis_active={eis_active}. Check portal/compositor logs for an asynchronous barrier rejection or backend restart", disconnected.reason, disconnected.last_serial)
}

fn emergency_modifiers_held(keys: &HashSet<u32>) -> bool {
    (keys.contains(&29) || keys.contains(&97))
        && (keys.contains(&56) || keys.contains(&100))
        && (keys.contains(&42) || keys.contains(&54))
}

fn normalize_event(event: &EiEvent) -> Option<InputEvent> {
    match event {
        EiEvent::PointerMotion(e) => Some(InputEvent::PointerMotion {
            dx: f64::from(e.dx),
            dy: f64::from(e.dy),
        }),
        EiEvent::Button(e) => Some(InputEvent::PointerButton {
            button: u16::try_from(e.button).ok()?,
            pressed: e.state == ButtonState::Press,
        }),
        EiEvent::ScrollDelta(e) => Some(InputEvent::Scroll {
            dx: f64::from(e.dx),
            dy: f64::from(e.dy),
        }),
        EiEvent::KeyboardKey(e) => Some(InputEvent::Key {
            code: KeyCode::LinuxEvdev(e.key),
            pressed: e.state == KeyState::Press,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::capture_setup;
    use super::{
        emergency_modifiers_held, select_barriers, validate_barriers, CapturePhase, Edge,
        InjectionPortalCapability, InterfaceProbe, Zone,
    };
    use std::time::Duration;

    #[tokio::test]
    async fn unavailable_peer_does_not_poll_capture_creation() {
        let polls = std::cell::Cell::new(0);
        let pending = std::future::poll_fn(|_| {
            polls.set(polls.get() + 1);
            std::task::Poll::<Result<(), String>>::Pending
        });
        assert!(
            capture_setup(pending, &mut || false, Duration::from_secs(1))
                .await
                .is_err()
        );
        assert_eq!(polls.get(), 0);
    }

    #[tokio::test]
    async fn capture_setup_cancellation_drops_pending_resources() {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };
        struct Resource(Arc<AtomicUsize>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let available = Arc::new(AtomicBool::new(true));
        let pending = async {
            let _resource = Resource(drops.clone());
            std::future::pending::<Result<(), String>>().await
        };
        let peer_loss = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            available.store(false, Ordering::SeqCst);
        };
        let mut healthy = || available.load(Ordering::SeqCst);
        let (result, ()) = tokio::join!(
            capture_setup(pending, &mut healthy, Duration::from_secs(1)),
            peer_loss
        );
        assert!(result.is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn emergency_release_requires_all_three_modifier_families() {
        let mut held = [29, 56, 42].into_iter().collect();
        assert!(emergency_modifiers_held(&held));
        held.remove(&42);
        assert!(!emergency_modifiers_held(&held));
        held.insert(54);
        assert!(emergency_modifiers_held(&held));
        held.remove(&29);
        assert!(!emergency_modifiers_held(&held));
    }

    #[test]
    fn remote_desktop_probe_does_not_imply_proven_injection() {
        let unavailable = InjectionPortalCapability::from_probe(&InterfaceProbe::unavailable(
            "RemoteDesktop frontend absent",
        ));
        assert!(!unavailable.portal_available);
        assert!(!unavailable.eis_api_available);

        let mut advertised = InterfaceProbe {
            version: Some(1),
            keyboard_advertised: true,
            pointer_advertised: true,
            error: None,
        };
        let legacy = InjectionPortalCapability::from_probe(&advertised);
        assert!(legacy.portal_available);
        assert!(!legacy.eis_api_available);
        assert!(legacy.pointer_advertised);
        assert!(legacy.keyboard_advertised);

        advertised.version = Some(2);
        let eis = InjectionPortalCapability::from_probe(&advertised);
        assert!(eis.eis_api_available);
    }

    #[test]
    fn multiple_external_segments_arm_without_manual_zone_selection() {
        let zones = [
            Zone {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            Zone {
                x: 100,
                y: 0,
                width: 100,
                height: 100,
            },
        ];
        let barriers = select_barriers(&zones, Edge::Top, None).unwrap();
        assert_eq!(barriers.len(), 2);
        assert_ne!(barriers[0].id, barriers[1].id);
        assert!(select_barriers(&zones, Edge::Right, Some(0)).is_err());
        assert!(select_barriers(&[], Edge::Left, None).is_err());
        assert_eq!(
            select_barriers(&zones, Edge::Top, Some(0)).unwrap().len(),
            1
        );
    }

    #[test]
    fn barrier_response_is_negative_list() {
        let zones = [Zone {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        }];
        let requested = select_barriers(&zones, Edge::Right, None).unwrap();
        assert_eq!(
            validate_barriers(&requested, &[]).unwrap().accepted,
            vec![1]
        );
        assert_eq!(
            validate_barriers(&requested, &[1]).unwrap().rejected,
            vec![1]
        );
        assert!(validate_barriers(&requested, &[2]).is_err());
    }

    #[test]
    fn capture_phase_rejects_skipped_states() {
        let phase = CapturePhase::CreatingSession
            .advance(CapturePhase::Configuring)
            .unwrap();
        let phase = phase.advance(CapturePhase::WaitingForBarrier).unwrap();
        let phase = phase.advance(CapturePhase::Captured).unwrap();
        assert!(phase.advance(CapturePhase::Closed).is_err());
        let phase = phase.advance(CapturePhase::Releasing).unwrap();
        assert_eq!(
            phase.advance(CapturePhase::Closed).unwrap(),
            CapturePhase::Closed
        );
    }
}

#[cfg(test)]
mod capture_lifecycle_tests {
    use super::*;

    fn spectrum_barriers() -> Vec<BarrierRequest> {
        select_barriers(
            &[
                Zone {
                    x: -3840,
                    y: 0,
                    width: 3840,
                    height: 2160,
                },
                Zone {
                    x: -5760,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
            ],
            Edge::Left,
            None,
        )
        .unwrap()
    }

    #[test]
    fn both_negative_left_barriers_have_inclusive_endpoints_and_distinct_ids() {
        let barriers = spectrum_barriers();
        assert_eq!(barriers.len(), 2);
        assert_eq!(
            (
                barriers[0].id,
                barriers[0].segment.start,
                barriers[0].segment.end
            ),
            (1, (-3840, 1080), (-3840, 2159))
        );
        assert_eq!(
            (
                barriers[1].id,
                barriers[1].segment.start,
                barriers[1].segment.end
            ),
            (2, (-5760, 0), (-5760, 1079))
        );
        assert_eq!(
            validate_barriers(&barriers, &[]).unwrap().accepted,
            vec![1, 2]
        );
        assert_eq!(
            validate_barriers(&barriers, &[1]).unwrap().rejected,
            vec![1]
        );
    }

    #[test]
    fn activation_routes_both_barrier_ids_without_guessing_unknown_ids() {
        let barriers = spectrum_barriers();
        let (edge, fraction) = activation_geometry(&barriers, Some(1), Some((-3841.0, 1500.0)));
        assert_eq!(edge, Some(Edge::Left));
        assert!((fraction - 1500.0 / 2159.0).abs() < 0.001);
        let (edge, fraction) = activation_geometry(&barriers, Some(2), Some((-5761.0, 500.0)));
        assert_eq!(edge, Some(Edge::Left));
        assert!((fraction - 500.0 / 2159.0).abs() < 0.001);
        assert_eq!(
            activation_geometry(&barriers, Some(0), Some((-5761.0, 500.0))).0,
            None
        );
        assert_eq!(activation_geometry(&barriers, Some(99), None).0, None);
        assert_eq!(activation_geometry(&barriers, None, None).0, None);
    }

    #[test]
    fn configuring_failure_has_valid_cleanup_path_without_enable_or_activation() {
        let phase = CapturePhase::Configuring
            .advance(CapturePhase::Releasing)
            .unwrap();
        assert_eq!(
            phase.advance(CapturePhase::Closed).unwrap(),
            CapturePhase::Closed
        );
        assert!(CapturePhase::Configuring
            .advance(CapturePhase::Captured)
            .is_err());
    }

    #[test]
    fn ei_disconnect_preserves_backend_reason_and_waiting_state() {
        let reason = eis_disconnect_reason(
            &reis::event::Disconnected {
                last_serial: 7,
                reason: ei::connection::DisconnectReason::Protocol,
                explanation: Some("invalid barrier\nbackend exited".into()),
            },
            CapturePhase::WaitingForBarrier,
            false,
            false,
        );
        assert!(reason.contains("reason=Protocol"));
        assert!(reason.contains("last_serial=7"));
        assert!(reason.contains("invalid barrier backend exited"));
        assert!(reason.contains("phase=WaitingForBarrier, portal_active=false, eis_active=false"));
        assert!(!reason.contains('\n'));
        assert_eq!(lifecycle_text(&"a".repeat(1000)).len(), 256);
    }

    #[test]
    fn portal_closed_reports_optional_reason_without_dumping_other_values() {
        let mut details = std::collections::HashMap::new();
        assert!(portal_closed_reason(&details).contains("no reason supplied"));
        details.insert(
            "reason".into(),
            zbus::zvariant::OwnedValue::from(zbus::zvariant::Str::from("backend vanished")),
        );
        details.insert(
            "other".into(),
            zbus::zvariant::OwnedValue::from(zbus::zvariant::Str::from("do not log")),
        );
        let message = portal_closed_reason(&details);
        assert!(message.contains("reason=backend vanished"));
        assert!(!message.contains("do not log"));
    }
}
