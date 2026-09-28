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
use ukvm_protocol::{InputEvent, KeyCode};

mod geometry;
pub use geometry::{all_outer_segments, outer_segments, Edge, OuterSegment, Zone, ZoneSet};

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

struct CaptureSession {
    portal: InputCapture,
    session: Session<InputCapture>,
    keyboard_granted: bool,
}

impl CaptureSession {
    async fn open() -> Result<Self, String> {
        let portal = InputCapture::new().await.map_err(|e| e.to_string())?;
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
        let disabled = self.portal.disable(&self.session, Default::default()).await;
        let closed = self.session.close().await;
        disabled.map_err(|e| format!("InputCapture Disable failed: {e}"))?;
        closed.map_err(|e| format!("InputCapture session Close failed: {e}"))
    }
}

/// Opens a permissioned session, reads all zones, and closes the session.
pub async fn zones() -> Result<ZoneSet, String> {
    let session = CaptureSession::open().await?;
    let result = session.zones().await;
    let close = session.close().await;
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

/// Selects all segments on one zone edge. Without a zone, there must be one
/// unambiguous segment across the entire layout.
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
    if zone.is_none() && segments.len() != 1 {
        return Err(format!(
            "{} outer {} segments exist; specify a zone index",
            segments.len(),
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
                | (Self::WaitingForBarrier | Self::Captured, Self::Releasing)
                | (Self::Releasing, Self::Closed)
        );
        valid
            .then_some(next)
            .ok_or_else(|| format!("invalid capture state transition: {self:?} -> {next:?}"))
    }
}

#[derive(Clone, Debug)]
pub enum CaptureNotice {
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
            active_duration: Duration::from_secs(5),
            release_mode: ReleaseMode::Escape,
        },
        on_notice,
        on_event,
        is_healthy,
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
    pub active_duration: Duration,
    pub release_mode: ReleaseMode,
}

/// Remote sessions reserve Ctrl+Alt+Shift+Escape for local release. Escape
/// remains an ordinary forwarded key unless all three modifiers are held.
pub async fn capture_with_events_mode<F, G, H>(
    edge: Edge,
    zone_index: Option<usize>,
    timing: CaptureTiming,
    on_notice: &mut F,
    on_event: &mut G,
    is_healthy: &mut H,
) -> Result<CaptureSummary, String>
where
    F: FnMut(CaptureNotice),
    G: FnMut(InputEvent) -> Result<(), String>,
    H: FnMut() -> bool,
{
    let session = CaptureSession::open().await?;
    on_notice(CaptureNotice::SessionCreated {
        version: session.portal.version(),
        keyboard_granted: session.keyboard_granted,
    });
    let mut phase = CapturePhase::CreatingSession.advance(CapturePhase::Configuring)?;
    let mut hooks = CaptureHooks {
        on_event,
        is_healthy,
    };
    let result = capture_inner(
        &session, edge, zone_index, timing, on_notice, &mut hooks, &mut phase,
    )
    .await;
    let close = session.close().await;
    if phase == CapturePhase::Releasing {
        phase = phase.advance(CapturePhase::Closed)?;
        debug_assert_eq!(phase, CapturePhase::Closed);
    }
    on_notice(CaptureNotice::Closed(close.clone()));
    match (result, close) {
        (Ok(summary), Ok(())) => Ok(summary),
        (Err(err), _) | (_, Err(err)) => Err(err),
    }
}

struct CaptureHooks<'a, G, H> {
    on_event: &'a mut G,
    is_healthy: &'a mut H,
}

async fn capture_inner<F, G, H>(
    session: &CaptureSession,
    edge: Edge,
    zone_index: Option<usize>,
    timing: CaptureTiming,
    on_notice: &mut F,
    hooks: &mut CaptureHooks<'_, G, H>,
    phase: &mut CapturePhase,
) -> Result<CaptureSummary, String>
where
    F: FnMut(CaptureNotice),
    G: FnMut(InputEvent) -> Result<(), String>,
    H: FnMut() -> bool,
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
    // ConnectToEIS must precede Enable. Keep the EI receiver alive throughout
    // every enabled/active cycle of this portal session.
    let fd = session
        .portal
        .connect_to_eis(&session.session, Default::default())
        .await
        .map_err(|e| e.to_string())?;
    on_notice(CaptureNotice::EisSocketAcquired);
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
    let context = ei::Context::new(stream).map_err(|e| e.to_string())?;
    on_notice(CaptureNotice::EisContextConnected);
    let (_connection, mut events) = context
        .handshake_tokio("ukvm", ei::handshake::ContextType::Receiver)
        .await
        .map_err(|e| format!("EI handshake failed: {e}"))?;
    on_notice(CaptureNotice::EisHandshakeComplete);
    let zone_set = session.zones().await?;
    on_notice(CaptureNotice::Zones(zone_set.clone()));
    let requested = select_barriers(&zone_set.zones, edge, zone_index)?;
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
    let response = session
        .portal
        .set_pointer_barriers(&session.session, &barriers, zone_set.id, Default::default())
        .await
        .map_err(|e| e.to_string())?
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
    *phase = phase.advance(CapturePhase::WaitingForBarrier)?;
    session
        .portal
        .enable(&session.session, Default::default())
        .await
        .map_err(|e| e.to_string())?;
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
    let mut health_tick = tokio::time::interval(Duration::from_millis(100));
    let mut held_keys = HashSet::new();
    loop {
        tokio::select! {
            _ = health_tick.tick() => {
                if !(hooks.is_healthy)() { abort_reason = Some("peer connection lost; releasing local InputCapture".into()); break; }
            },
            _ = &mut deadline => { summary.timed_out = true; break; },
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
                portal_active = true;
                activation_id = signal.activation_id();
                triggered_barrier = match signal.barrier_id() {
                    Some(ActivatedBarrier::Barrier(id)) => Some(id.get()),
                    _ => None,
                };
                summary.activations += 1;
                if *phase == CapturePhase::Captured { abort_reason = Some("duplicate InputCapture activation".into()); break; }
                *phase = phase.advance(CapturePhase::Captured)?;
                active_until = Some(tokio::time::Instant::now() + timing.active_duration);
                on_notice(CaptureNotice::Activated { activation_id, barrier_id: triggered_barrier });
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
                }
            }
            signal = zones_changed.next() => {
                let Some(signal) = signal else { abort_reason = Some("InputCapture ZonesChanged signal stream closed".into()); break; };
                on_notice(CaptureNotice::ZonesChanged { zone_set: signal.zone_set() });
                abort_reason = Some("display zones changed; capture stopped so barriers can be rebuilt".to_string());
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
                    EiEvent::Disconnected(_) => { abort_reason = Some("EIS disconnected".into()); break; },
                    EiEvent::DeviceStartEmulating(start) => {
                        eis_active = true;
                        summary.eis_starts += 1;
                        on_notice(CaptureNotice::EisStarted { sequence: start.sequence });
                    }
                    EiEvent::DeviceStopEmulating(_) => { eis_active = false; on_notice(CaptureNotice::EisStopped); },
                    other if portal_active || eis_active => {
                        if let Some(input) = normalize_event(&other) {
                            if !(hooks.is_healthy)() { abort_reason = Some("peer connection lost; releasing local InputCapture".into()); break; }
                            if let InputEvent::Key { code: KeyCode::LinuxEvdev(code), pressed } = &input {
                                if *pressed { held_keys.insert(*code); } else { held_keys.remove(code); }
                                if *pressed && *code == 1 && match timing.release_mode {
                                    ReleaseMode::Escape => true,
                                    ReleaseMode::EmergencyChord => emergency_modifiers_held(&held_keys),
                                } {
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
            .map(|item| item.segment.release_position());
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
    use super::{
        emergency_modifiers_held, select_barriers, validate_barriers, CapturePhase, Edge,
        InjectionPortalCapability, InterfaceProbe, Zone,
    };

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
    fn ambiguous_edge_needs_zone_selection() {
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
        assert!(select_barriers(&zones, Edge::Top, None).is_err());
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
