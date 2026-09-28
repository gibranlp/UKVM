//! Permissioned RemoteDesktop portal and EI sender for a Wayland session.
//! A dedicated user thread owns the async portal session so the existing
//! synchronous platform trait never blocks the receiver's Tokio executor.

use super::{
    geometry::Zone,
    return_edge::{MotionResult, RemotePointer},
};
use crate::{InjectionCapabilities, PlatformError, PlatformInputInjector};
use ashpd::desktop::{
    remote_desktop::{
        ConnectToEISOptions, DeviceType, RemoteDesktop, SelectDevicesOptions, StartOptions,
    },
    CreateSessionOptions, Session,
};
use futures_util::StreamExt;
use reis::{
    ei::{self, button::ButtonState, keyboard::KeyState},
    event::{Connection, Device, DeviceCapability, EiEvent},
};
use std::{
    collections::HashSet,
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc as reply, Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use ukvm_protocol::{EdgePosition, InputEvent, KeyCode};

const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(4);
const EI_DEVICE_TIMEOUT: Duration = Duration::from_secs(8);
const COMMAND_QUEUE: usize = 256;

enum Command {
    Begin(reply::SyncSender<Result<(), PlatformError>>),
    BeginAsync(oneshot::Sender<Result<(), PlatformError>>),
    Event(InputEvent, reply::SyncSender<Result<(), PlatformError>>),
    Enter(EdgePosition, reply::SyncSender<Result<(), PlatformError>>),
    RemoteEvent(
        InputEvent,
        reply::SyncSender<Result<Option<EdgePosition>, PlatformError>>,
    ),
    Release(reply::SyncSender<Result<(), PlatformError>>),
    End(reply::SyncSender<Result<(), PlatformError>>),
}

/// A per-connection injector. `open` checks the portal, but only `begin`
/// requests consent. No direct compositor virtual-input global is used by UKVM.
pub struct WaylandInputInjector {
    commands: mpsc::Sender<Command>,
    capabilities: Arc<Mutex<InjectionCapabilities>>,
    healthy: Arc<AtomicBool>,
}

impl WaylandInputInjector {
    pub async fn open() -> Result<Self, PlatformError> {
        Self::open_with_devices(true, false).await
    }

    pub async fn open_verbose(verbose: bool) -> Result<Self, PlatformError> {
        Self::open_with_devices(true, verbose).await
    }

    /// The first physical proof requests pointer consent only.
    pub async fn open_pointer_only() -> Result<Self, PlatformError> {
        Self::open_with_devices(false, false).await
    }

    async fn open_with_devices(
        request_keyboard: bool,
        verbose: bool,
    ) -> Result<Self, PlatformError> {
        let report = super::probe().await;
        if report.remote.version.is_none() {
            return Err(PlatformError::Unsupported(
                "RemoteDesktop portal interface not provided by active backend",
            ));
        }
        if report.remote.version.is_some_and(|version| version < 2) {
            return Err(PlatformError::Unsupported(
                "RemoteDesktop portal does not provide ConnectToEIS",
            ));
        }
        if !report.remote.pointer_advertised {
            return Err(PlatformError::Unsupported(
                "RemoteDesktop portal does not advertise pointer control",
            ));
        }
        let (commands, rx) = mpsc::channel(COMMAND_QUEUE);
        let capabilities = Arc::new(Mutex::new(InjectionCapabilities::default()));
        let healthy = Arc::new(AtomicBool::new(false));
        let actor_caps = Arc::clone(&capabilities);
        let actor_health = Arc::clone(&healthy);
        let (ready_tx, ready_rx) = reply::sync_channel(1);
        std::thread::Builder::new()
            .name("ukvm-wayland-injector".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        let _ = ready_tx.send(Ok(()));
                        runtime.block_on(run_actor(
                            rx,
                            actor_caps,
                            actor_health,
                            request_keyboard,
                            verbose,
                        ));
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(PlatformError::Backend(err.to_string())));
                    }
                }
            })
            .map_err(|err| PlatformError::Backend(err.to_string()))?;
        ready_rx.recv_timeout(COMMAND_TIMEOUT).map_err(|err| {
            PlatformError::Backend(format!("injector actor did not start: {err}"))
        })??;
        Ok(Self {
            commands,
            capabilities,
            healthy,
        })
    }

    fn request(
        &self,
        make_command: impl FnOnce(reply::SyncSender<Result<(), PlatformError>>) -> Command,
        timeout: Duration,
    ) -> Result<(), PlatformError> {
        let (tx, rx) = reply::sync_channel(1);
        self.commands.try_send(make_command(tx)).map_err(|err| {
            PlatformError::Backend(format!("injector command unavailable: {err}"))
        })?;
        rx.recv_timeout(timeout)
            .map_err(|err| PlatformError::Backend(format!("injector command timed out: {err}")))?
    }

    fn request_remote_event(
        &self,
        event: &InputEvent,
    ) -> Result<Option<EdgePosition>, PlatformError> {
        let (tx, rx) = reply::sync_channel(1);
        self.commands
            .try_send(Command::RemoteEvent(event.clone(), tx))
            .map_err(|err| {
                PlatformError::Backend(format!("injector command unavailable: {err}"))
            })?;
        rx.recv_timeout(COMMAND_TIMEOUT)
            .map_err(|err| PlatformError::Backend(format!("injector command timed out: {err}")))?
    }
}

impl PlatformInputInjector for WaylandInputInjector {
    fn capabilities(&self) -> InjectionCapabilities {
        self.capabilities
            .lock()
            .map(|caps| *caps)
            .unwrap_or_default()
    }

    fn begin(&mut self) -> Result<(), PlatformError> {
        self.request(Command::Begin, AUTHORIZE_TIMEOUT)
    }

    fn begin_async(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), PlatformError>> + Send + '_>>
    {
        Box::pin(async {
            let (reply, result) = oneshot::channel();
            self.commands
                .try_send(Command::BeginAsync(reply))
                .map_err(|err| {
                    PlatformError::Backend(format!("injector command unavailable: {err}"))
                })?;
            tokio::time::timeout(AUTHORIZE_TIMEOUT, result)
                .await
                .map_err(|_| {
                    PlatformError::Backend("RemoteDesktop authorization timed out".into())
                })?
                .map_err(|_| PlatformError::Backend("injector authorization stopped".into()))?
        })
    }

    fn inject(&mut self, event: &InputEvent) -> Result<(), PlatformError> {
        self.request(
            |reply| Command::Event(event.clone(), reply),
            COMMAND_TIMEOUT,
        )
    }

    fn enter_remote(&mut self, entry: EdgePosition) -> Result<(), PlatformError> {
        self.request(|reply| Command::Enter(entry, reply), COMMAND_TIMEOUT)
    }

    fn inject_remote(&mut self, event: &InputEvent) -> Result<Option<EdgePosition>, PlatformError> {
        self.request_remote_event(event)
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn release_all(&mut self) -> Result<(), PlatformError> {
        self.request(Command::Release, COMMAND_TIMEOUT)
    }

    fn end(&mut self) -> Result<(), PlatformError> {
        self.request(Command::End, COMMAND_TIMEOUT)
    }
}

struct PortalEisSession {
    _portal: RemoteDesktop,
    portal_session: Session<RemoteDesktop>,
    connection: Connection,
    events: reis::tokio::EiConvertEventStream,
    pointer: Device,
    keyboard: Option<Device>,
    held_keys: HashSet<u32>,
    held_buttons: HashSet<u32>,
    remote_pointer: Option<RemotePointer>,
    clock: Instant,
}

/// A cancelled portal Start still owns a live session. Close it even when the
/// authorization future is dropped by Ctrl+C or a timeout.
struct PendingPortalSession(Option<Session<RemoteDesktop>>);

impl Drop for PendingPortalSession {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = session.close().await;
                });
            }
        }
    }
}

impl PortalEisSession {
    async fn open(request_keyboard: bool, verbose: bool) -> Result<Self, PlatformError> {
        if verbose {
            eprintln!("[wayland] RemoteDesktop: opening portal interface");
        }
        let portal = RemoteDesktop::new()
            .await
            .map_err(|err| PlatformError::Backend(format!("RemoteDesktop unavailable: {err}")))?;
        if portal.version() < 2 {
            return Err(PlatformError::Unsupported(
                "RemoteDesktop portal does not provide ConnectToEIS",
            ));
        }
        if verbose {
            eprintln!("[wayland] RemoteDesktop: CreateSession");
        }
        let session = portal
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(|err| PlatformError::Backend(format!("CreateSession failed: {err}")))?;
        let result = Self::open_session(portal, session, request_keyboard, verbose).await;
        result
    }

    async fn open_session(
        portal: RemoteDesktop,
        session: Session<RemoteDesktop>,
        request_keyboard: bool,
        verbose: bool,
    ) -> Result<Self, PlatformError> {
        let mut pending = PendingPortalSession(Some(session));
        let result = async {
            let requested = if request_keyboard {
                DeviceType::Pointer | DeviceType::Keyboard
            } else {
                DeviceType::Pointer.into()
            };
            if verbose {
                eprintln!("[wayland] RemoteDesktop: SelectDevices");
            }
            portal
                .select_devices(
                    pending.0.as_ref().expect("pending portal session"),
                    SelectDevicesOptions::default().set_devices(requested),
                )
                .await
                .and_then(|request| request.response().map(|_| ()))
                .map_err(|err| PlatformError::Backend(format!("SelectDevices failed: {err}")))?;
            if verbose {
                eprintln!("[wayland] RemoteDesktop: Start pending; consent picker may appear");
            }
            let selected = portal
                .start(
                    pending.0.as_ref().expect("pending portal session"),
                    None,
                    StartOptions::default(),
                )
                .await
                .and_then(|request| request.response())
                .map_err(|err| {
                    PlatformError::Backend(format!("RemoteDesktop Start denied or failed: {err}"))
                })?;
            if !selected.devices().contains(DeviceType::Pointer) {
                return Err(PlatformError::PermissionDenied(
                    "RemoteDesktop did not grant pointer control",
                ));
            }
            if request_keyboard && !selected.devices().contains(DeviceType::Keyboard) {
                return Err(PlatformError::PermissionDenied(
                    "RemoteDesktop did not grant keyboard control",
                ));
            }
            if verbose {
                eprintln!("[wayland] RemoteDesktop: consent granted; ConnectToEIS");
            }
            let fd = portal
                .connect_to_eis(
                    pending.0.as_ref().expect("pending portal session"),
                    ConnectToEISOptions::default(),
                )
                .await
                .map_err(|err| PlatformError::Backend(format!("ConnectToEIS failed: {err}")))?;
            let stream = UnixStream::from(fd);
            stream
                .set_nonblocking(true)
                .map_err(|err| PlatformError::Backend(err.to_string()))?;
            let context = ei::Context::new(stream)
                .map_err(|err| PlatformError::Backend(format!("EI socket failed: {err}")))?;
            if verbose {
                eprintln!("[wayland] EI: socket acquired; handshaking");
            }
            let (connection, mut events) = context
                .handshake_tokio("ukvm", ei::handshake::ContextType::Sender)
                .await
                .map_err(|err| PlatformError::Backend(format!("EI handshake failed: {err}")))?;
            if verbose {
                eprintln!("[wayland] EI: handshake complete; waiting for devices");
            }
            let (pointer, keyboard) = tokio::time::timeout(EI_DEVICE_TIMEOUT, async {
                let mut pointer = None;
                let mut keyboard = None;
                let mut active = Vec::new();
                let mut sequence = 1;
                while pointer
                    .as_ref()
                    .is_none_or(|device| !active.contains(device))
                    || (request_keyboard
                        && keyboard
                            .as_ref()
                            .is_none_or(|device| !active.contains(device)))
                {
                    let event = events
                        .next()
                        .await
                        .ok_or(PlatformError::Backend(
                            "EIS disconnected before devices appeared".into(),
                        ))?
                        .map_err(|err| {
                            PlatformError::Backend(format!("EIS setup failed: {err}"))
                        })?;
                    match event {
                        EiEvent::SeatAdded(seat) => {
                            seat.seat.bind_capabilities(
                                DeviceCapability::Pointer
                                    | DeviceCapability::PointerAbsolute
                                    | DeviceCapability::Button
                                    | DeviceCapability::Scroll
                                    | DeviceCapability::Keyboard,
                            );
                            connection
                                .flush()
                                .map_err(|err| PlatformError::Backend(err.to_string()))?;
                        }
                        EiEvent::DeviceAdded(added) => {
                            let device = added.device;
                            if device.has_capability(DeviceCapability::Pointer)
                                && device.has_capability(DeviceCapability::Button)
                                && device.has_capability(DeviceCapability::Scroll)
                            {
                                pointer = Some(device.clone());
                            }
                            if device.has_capability(DeviceCapability::Keyboard) {
                                keyboard = Some(device);
                            }
                        }
                        EiEvent::DeviceResumed(resumed) => {
                            resumed
                                .device
                                .device()
                                .start_emulating(resumed.serial, sequence);
                            sequence += 1;
                            connection
                                .flush()
                                .map_err(|err| PlatformError::Backend(err.to_string()))?;
                            if !active.contains(&resumed.device) {
                                active.push(resumed.device);
                            }
                        }
                        EiEvent::Disconnected(_) => {
                            return Err(PlatformError::Backend(
                                "EIS disconnected during setup".into(),
                            ));
                        }
                        _ => {}
                    }
                }
                Ok::<_, PlatformError>((pointer.expect("checked"), keyboard))
            })
            .await
            .map_err(|_| PlatformError::Backend("EIS devices did not become available".into()))??;
            if verbose {
                eprintln!(
                    "[wayland] EI: pointer ready (absolute={}); keyboard={}",
                    pointer.has_capability(DeviceCapability::PointerAbsolute),
                    keyboard.is_some()
                );
                for (index, region) in pointer.regions().iter().enumerate() {
                    eprintln!(
                        "[wayland] EI region {index}: {}x{} @ {},{} scale={}",
                        region.width, region.height, region.x, region.y, region.scale
                    );
                }
            }
            Ok((connection, events, pointer, keyboard))
        }
        .await;
        match result {
            Ok((connection, events, pointer, keyboard)) => Ok(Self {
                _portal: portal,
                portal_session: pending.0.take().expect("pending portal session"),
                connection,
                events,
                pointer,
                keyboard,
                held_keys: HashSet::new(),
                held_buttons: HashSet::new(),
                remote_pointer: None,
                clock: Instant::now(),
            }),
            Err(err) => Err(err),
        }
    }

    fn capabilities(&self) -> InjectionCapabilities {
        InjectionCapabilities {
            pointer_relative: true,
            pointer_absolute: self
                .pointer
                .has_capability(DeviceCapability::PointerAbsolute),
            pointer_button: true,
            pointer_scroll: true,
            keyboard: self.keyboard.is_some(),
        }
    }

    fn frame(&self, device: &Device) -> Result<(), PlatformError> {
        device.device().frame(
            self.connection.serial(),
            self.clock.elapsed().as_micros() as u64,
        );
        self.connection
            .flush()
            .map_err(|err| PlatformError::Backend(format!("EI flush failed: {err}")))
    }

    fn enter_remote(&mut self, entry: EdgePosition) -> Result<(), PlatformError> {
        if !self
            .pointer
            .has_capability(DeviceCapability::PointerAbsolute)
        {
            return Err(PlatformError::Unsupported(
                "EI absolute pointer is required for edge return",
            ));
        }
        let zones = self
            .pointer
            .regions()
            .iter()
            .map(|region| {
                Ok(Zone {
                    x: i32::try_from(region.x).map_err(|_| {
                        PlatformError::Backend("EI region x exceeds portal range".into())
                    })?,
                    y: i32::try_from(region.y).map_err(|_| {
                        PlatformError::Backend("EI region y exceeds portal range".into())
                    })?,
                    width: region.width,
                    height: region.height,
                })
            })
            .collect::<Result<Vec<_>, PlatformError>>()?;
        let pointer = RemotePointer::new(zones, entry).map_err(PlatformError::Backend)?;
        let (x, y) = pointer.position();
        self.absolute_motion(x, y)?;
        self.remote_pointer = Some(pointer);
        Ok(())
    }

    fn absolute_motion(&self, x: f32, y: f32) -> Result<(), PlatformError> {
        let pointer =
            self.pointer
                .interface::<ei::PointerAbsolute>()
                .ok_or(PlatformError::Unsupported(
                    "EI absolute pointer interface unavailable",
                ))?;
        pointer.motion_absolute(x, y);
        self.frame(&self.pointer)
    }

    fn inject_remote(&mut self, event: &InputEvent) -> Result<Option<EdgePosition>, PlatformError> {
        if let InputEvent::PointerMotion { dx, dy } = event {
            let pointer = self.remote_pointer.as_mut().ok_or(PlatformError::Backend(
                "remote entry not initialized".into(),
            ))?;
            return match pointer.motion(*dx, *dy) {
                MotionResult::Position(x, y) => {
                    self.absolute_motion(x, y)?;
                    Ok(None)
                }
                MotionResult::Return(position) => Ok(Some(position)),
            };
        }
        self.inject(event)?;
        Ok(None)
    }

    fn inject(&mut self, event: &InputEvent) -> Result<(), PlatformError> {
        if !self.pointer.device().is_alive() {
            return Err(PlatformError::SessionClosed);
        }
        match event {
            InputEvent::PointerMotion { dx, dy } => {
                let pointer =
                    self.pointer
                        .interface::<ei::Pointer>()
                        .ok_or(PlatformError::Unsupported(
                            "EI pointer device lacks relative motion",
                        ))?;
                pointer.motion_relative(*dx as f32, *dy as f32);
                self.frame(&self.pointer)
            }
            InputEvent::PointerButton { button, pressed } => {
                let button_interface =
                    self.pointer
                        .interface::<ei::Button>()
                        .ok_or(PlatformError::Unsupported(
                            "EI pointer device lacks buttons",
                        ))?;
                let code = u32::from(*button);
                button_interface.button(
                    code,
                    if *pressed {
                        ButtonState::Press
                    } else {
                        ButtonState::Released
                    },
                );
                self.frame(&self.pointer)?;
                if *pressed {
                    self.held_buttons.insert(code);
                } else {
                    self.held_buttons.remove(&code);
                }
                Ok(())
            }
            InputEvent::Scroll { dx, dy } => {
                let scroll = self
                    .pointer
                    .interface::<ei::Scroll>()
                    .ok_or(PlatformError::Unsupported("EI pointer device lacks scroll"))?;
                scroll.scroll(*dx as f32, *dy as f32);
                self.frame(&self.pointer)
            }
            InputEvent::Key {
                code: KeyCode::LinuxEvdev(code),
                pressed,
            } => {
                let device = self
                    .keyboard
                    .as_ref()
                    .ok_or(PlatformError::Unsupported("EI keyboard device unavailable"))?;
                let keyboard =
                    device
                        .interface::<ei::Keyboard>()
                        .ok_or(PlatformError::Unsupported(
                            "EI keyboard device lacks key input",
                        ))?;
                keyboard.key(
                    *code,
                    if *pressed {
                        KeyState::Press
                    } else {
                        KeyState::Released
                    },
                );
                self.frame(device)?;
                if *pressed {
                    self.held_keys.insert(*code);
                } else {
                    self.held_keys.remove(code);
                }
                Ok(())
            }
        }
    }

    fn release_all(&mut self) -> Result<(), PlatformError> {
        if let Some(button) = self.pointer.interface::<ei::Button>() {
            for code in self.held_buttons.drain() {
                button.button(code, ButtonState::Released);
            }
            self.frame(&self.pointer)?;
        }
        if let Some(device) = &self.keyboard {
            if let Some(keyboard) = device.interface::<ei::Keyboard>() {
                for code in self.held_keys.drain() {
                    keyboard.key(code, KeyState::Released);
                }
                self.frame(device)?;
            }
        }
        Ok(())
    }

    async fn close(mut self) -> Result<(), PlatformError> {
        let released = self.release_all();
        self.pointer
            .device()
            .stop_emulating(self.connection.serial());
        if let Some(keyboard) = &self.keyboard {
            keyboard.device().stop_emulating(self.connection.serial());
        }
        let flushed = self.connection.flush();
        let closed = self.portal_session.close().await;
        released?;
        flushed.map_err(|err| PlatformError::Backend(format!("EI close flush failed: {err}")))?;
        closed.map_err(|err| PlatformError::Backend(format!("portal session close failed: {err}")))
    }
}

async fn run_actor(
    mut commands: mpsc::Receiver<Command>,
    capabilities: Arc<Mutex<InjectionCapabilities>>,
    healthy: Arc<AtomicBool>,
    request_keyboard: bool,
    verbose: bool,
) {
    let mut session: Option<PortalEisSession> = None;
    loop {
        if let Some(active) = session.as_mut() {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Event(event, reply)) => {
                        let result = active.inject(&event);
                        if result.is_err() { healthy.store(false, Ordering::Release); }
                        let _ = reply.send(result);
                    }
                    Some(Command::Enter(entry, reply)) => {
                        let result = active.enter_remote(entry);
                        if result.is_err() { healthy.store(false, Ordering::Release); }
                        let _ = reply.send(result);
                    }
                    Some(Command::RemoteEvent(event, reply)) => {
                        let result = active.inject_remote(&event);
                        if result.is_err() { healthy.store(false, Ordering::Release); }
                        let _ = reply.send(result);
                    }
                    Some(Command::Release(reply)) => {
                        let _ = reply.send(active.release_all());
                    }
                    Some(Command::End(reply)) => {
                        healthy.store(false, Ordering::Release);
                        let result = session.take().expect("active session").close().await;
                        let _ = reply.send(result);
                        break;
                    }
                    Some(Command::Begin(reply)) => {
                        let _ = reply.send(Err(PlatformError::Backend("injector session already active".into())));
                    }
                    Some(Command::BeginAsync(reply)) => {
                        let _ = reply.send(Err(PlatformError::Backend("injector session already active".into())));
                    }
                    None => break,
                },
                event = active.events.next() => {
                    let failed = matches!(event, Some(Ok(EiEvent::Disconnected(_) | EiEvent::DeviceRemoved(_) | EiEvent::DevicePaused(_))) | Some(Err(_)) | None);
                    if failed {
                        healthy.store(false, Ordering::Release);
                        break;
                    }
                }
            }
        } else {
            match commands.recv().await {
                Some(Command::Begin(reply)) => {
                    match tokio::time::timeout(
                        AUTHORIZE_TIMEOUT,
                        PortalEisSession::open(request_keyboard, verbose),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        Err(PlatformError::Backend(
                            "RemoteDesktop authorization timed out".into(),
                        ))
                    }) {
                        Ok(opened) => {
                            if let Ok(mut value) = capabilities.lock() {
                                *value = opened.capabilities();
                            }
                            healthy.store(true, Ordering::Release);
                            session = Some(opened);
                            let _ = reply.send(Ok(()));
                        }
                        Err(err) => {
                            let _ = reply.send(Err(err));
                        }
                    }
                }
                Some(Command::BeginAsync(mut reply)) => {
                    let opened = tokio::select! {
                        _ = reply.closed() => continue,
                        result = tokio::time::timeout(AUTHORIZE_TIMEOUT, PortalEisSession::open(request_keyboard, verbose)) => result.unwrap_or_else(|_| Err(PlatformError::Backend("RemoteDesktop authorization timed out".into()))),
                    };
                    match opened {
                        Ok(opened) => {
                            if let Ok(mut value) = capabilities.lock() {
                                *value = opened.capabilities();
                            }
                            healthy.store(true, Ordering::Release);
                            if reply.send(Ok(())).is_err() {
                                healthy.store(false, Ordering::Release);
                                let _ = opened.close().await;
                            } else {
                                session = Some(opened);
                            }
                        }
                        Err(err) => {
                            let _ = reply.send(Err(err));
                        }
                    }
                }
                Some(Command::Event(_, reply) | Command::Release(reply) | Command::End(reply)) => {
                    let _ = reply.send(Err(PlatformError::SessionClosed));
                }
                Some(Command::Enter(_, reply)) => {
                    let _ = reply.send(Err(PlatformError::SessionClosed));
                }
                Some(Command::RemoteEvent(_, reply)) => {
                    let _ = reply.send(Err(PlatformError::SessionClosed));
                }
                None => break,
            }
        }
    }
    healthy.store(false, Ordering::Release);
    if let Some(active) = session.take() {
        let _ = active.close().await;
    }
}
