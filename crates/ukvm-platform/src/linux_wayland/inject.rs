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
    CreateSessionOptions, PersistMode, Session,
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
const PORTAL_SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_QUEUE: usize = 256;

enum Command {
    Begin(reply::SyncSender<Result<(), PlatformError>>),
    Resume(reply::SyncSender<Result<(), PlatformError>>),
    BeginAsync(oneshot::Sender<Result<(), PlatformError>>),
    Event(InputEvent, reply::SyncSender<Result<(), PlatformError>>),
    Enter(EdgePosition, reply::SyncSender<Result<(), PlatformError>>),
    Topology(
        EdgePosition,
        Vec<ukvm_protocol::Edge>,
        reply::SyncSender<Result<(), PlatformError>>,
    ),
    Exits(
        Vec<ukvm_protocol::Edge>,
        reply::SyncSender<Result<(), PlatformError>>,
    ),
    RemoteEvent(
        InputEvent,
        reply::SyncSender<Result<Option<EdgePosition>, PlatformError>>,
    ),
    Release(reply::SyncSender<Result<(), PlatformError>>),
    Park(reply::SyncSender<Result<(), PlatformError>>),
    End(reply::SyncSender<Result<(), PlatformError>>),
}

/// A per-connection injector. `open` checks the portal, but only `begin`
/// requests consent. No direct compositor virtual-input global is used by UKVM.
pub struct WaylandInputInjector {
    commands: mpsc::Sender<Command>,
    capabilities: Arc<Mutex<InjectionCapabilities>>,
    healthy: Arc<AtomicBool>,
    actor: Option<std::thread::JoinHandle<()>>,
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
        let actor = std::thread::Builder::new()
            .name("ukvm-wayland-injector".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        let portal = runtime.block_on(async {
                            tokio::time::timeout(
                                PORTAL_SETUP_TIMEOUT,
                                PortalEisSession::open_portal(verbose),
                            )
                            .await
                        });
                        match portal.unwrap_or_else(|_| {
                            Err(PlatformError::Backend(
                                "RemoteDesktop portal proxy setup timed out".into(),
                            ))
                        }) {
                            Ok(portal) => {
                                let _ = ready_tx.send(Ok(()));
                                runtime.block_on(run_actor(
                                    rx,
                                    actor_caps,
                                    actor_health,
                                    request_keyboard,
                                    verbose,
                                    portal,
                                ));
                            }
                            Err(err) => {
                                let _ = ready_tx.send(Err(err));
                            }
                        }
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(PlatformError::Backend(err.to_string())));
                    }
                }
            })
            .map_err(|err| PlatformError::Backend(err.to_string()))?;
        let ready = ready_rx
            .recv_timeout(PORTAL_SETUP_TIMEOUT + COMMAND_TIMEOUT)
            .map_err(|err| PlatformError::Backend(format!("injector actor did not start: {err}")))
            .and_then(|ready| ready);
        if let Err(err) = ready {
            drop(commands);
            let _ = actor.join();
            return Err(err);
        }
        Ok(Self {
            commands,
            capabilities,
            healthy,
            actor: Some(actor),
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

impl Drop for WaylandInputInjector {
    fn drop(&mut self) {
        // Close the command channel before joining. The actor releases held
        // input, closes its portal/EI session and destroys its Tokio runtime.
        let (replacement, _) = mpsc::channel(1);
        drop(std::mem::replace(&mut self.commands, replacement));
        if let Some(actor) = self.actor.take() {
            let _ = actor.join();
        }
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

    fn resume(&mut self) -> Result<(), PlatformError> {
        self.request(Command::Resume, COMMAND_TIMEOUT)
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

    fn enter_topology(
        &mut self,
        entry: EdgePosition,
        exits: Vec<ukvm_protocol::Edge>,
    ) -> Result<(), PlatformError> {
        self.request(
            |reply| Command::Topology(entry, exits, reply),
            COMMAND_TIMEOUT,
        )
    }

    fn update_exits(&mut self, exits: Vec<ukvm_protocol::Edge>) -> Result<(), PlatformError> {
        self.request(|reply| Command::Exits(exits, reply), COMMAND_TIMEOUT)
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

    fn park(&mut self) -> Result<(), PlatformError> {
        self.request(Command::Park, COMMAND_TIMEOUT)
    }

    fn end(&mut self) -> Result<(), PlatformError> {
        self.request(Command::End, COMMAND_TIMEOUT)
    }
}

struct PortalEisSession {
    portal_session: Session<RemoteDesktop>,
    _authorization: super::authorization::Authorization,
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
    async fn open_portal(verbose: bool) -> Result<RemoteDesktop, PlatformError> {
        if verbose {
            eprintln!("[wayland] RemoteDesktop: opening dedicated session-bus connection");
        }
        // ashpd::RemoteDesktop::new() uses ashpd's process-global cached D-Bus
        // connection. This actor's Tokio runtime ends after each input session;
        // a later actor must not reuse a connection whose executor belonged to
        // the previous runtime.
        let bus = zbus::Connection::session()
            .await
            .map_err(|err| PlatformError::Backend(format!("session bus unavailable: {err}")))?;
        if verbose {
            eprintln!("[wayland] RemoteDesktop: opening portal interface");
        }
        let portal = RemoteDesktop::with_connection(bus)
            .await
            .map_err(|err| PlatformError::Backend(format!("RemoteDesktop unavailable: {err}")))?;
        if portal.version() < 2 {
            return Err(PlatformError::Unsupported(
                "RemoteDesktop portal does not provide ConnectToEIS",
            ));
        }
        Ok(portal)
    }

    async fn open(
        portal: &RemoteDesktop,
        request_keyboard: bool,
        verbose: bool,
    ) -> Result<Self, PlatformError> {
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
        portal: &RemoteDesktop,
        session: Session<RemoteDesktop>,
        request_keyboard: bool,
        verbose: bool,
    ) -> Result<Self, PlatformError> {
        let mut pending = PendingPortalSession(Some(session));
        let mut authorization = super::authorization::Authorization::begin(
            super::authorization::Kind::RemoteDesktop,
            super::authorization::Context::on_bus(portal.connection(), portal.version())
                .await
                .map_err(PlatformError::Backend)?,
        )
        .map_err(PlatformError::Backend)?;
        let result = async {
            authorize_devices(
                portal,
                pending.0.as_ref().expect("pending portal session"),
                request_keyboard,
                verbose,
                &mut authorization,
            )
            .await?;
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
                portal_session: pending.0.take().expect("pending portal session"),
                _authorization: authorization,
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
        self.enter_topology(entry, Vec::new())
    }

    fn enter_topology(
        &mut self,
        entry: EdgePosition,
        exits: Vec<ukvm_protocol::Edge>,
    ) -> Result<(), PlatformError> {
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
        let pointer =
            RemotePointer::with_exits(zones, entry, exits).map_err(PlatformError::Backend)?;
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
        let closed = tokio::time::timeout(COMMAND_TIMEOUT, self.portal_session.close())
            .await
            .map_err(|_| PlatformError::Backend("portal session close timed out".into()))?;
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
    portal: RemoteDesktop,
) {
    let mut session: Option<PortalEisSession> = None;
    let mut in_use = false;
    loop {
        if let Some(active) = session.as_mut() {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Event(event, reply)) => {
                        let result = if in_use { active.inject(&event) } else { Err(PlatformError::SessionClosed) };
                        if result.is_err() { healthy.store(false, Ordering::Release); }
                        let _ = reply.send(result);
                    }
                    Some(Command::Enter(entry, reply)) => {
                        let result = if in_use { active.enter_remote(entry) } else { Err(PlatformError::SessionClosed) };
                        if result.is_err() { healthy.store(false, Ordering::Release); }
                        let _ = reply.send(result);
                    }
                    Some(Command::Topology(entry, exits, reply)) => {
                        let result = if in_use { active.enter_topology(entry, exits) } else { Err(PlatformError::SessionClosed) };
                        if result.is_err() { healthy.store(false, Ordering::Release); }
                        let _ = reply.send(result);
                    }
                    Some(Command::Exits(exits, reply)) => {
                        let result = if in_use {
                            active.remote_pointer.as_mut().ok_or(PlatformError::SessionClosed).and_then(|pointer| pointer.update_exits(exits).map_err(PlatformError::Backend))
                        } else { Err(PlatformError::SessionClosed) };
                        let _ = reply.send(result);
                    }
                    Some(Command::RemoteEvent(event, reply)) => {
                        let result = if in_use { active.inject_remote(&event) } else { Err(PlatformError::SessionClosed) };
                        if result.is_err() { healthy.store(false, Ordering::Release); }
                        let _ = reply.send(result);
                    }
                    Some(Command::Release(reply)) => {
                        let _ = reply.send(active.release_all());
                    }
                    Some(Command::Park(reply)) => {
                        let result = active.release_all();
                        active.remote_pointer = None;
                        in_use = false;
                        let _ = reply.send(result);
                    }
                    Some(Command::End(reply)) => {
                        healthy.store(false, Ordering::Release);
                        in_use = false;
                        let result = session.take().expect("active session").close().await;
                        let _ = reply.send(result);
                    }
                    Some(Command::Begin(reply) | Command::Resume(reply)) => {
                        let result = if in_use {
                            Err(PlatformError::Backend("injector session already active".into()))
                        } else {
                            in_use = true;
                            Ok(())
                        };
                        let _ = reply.send(result);
                    }
                    Some(Command::BeginAsync(reply)) => {
                        if in_use {
                            let _ = reply.send(Err(PlatformError::Backend("injector session already active".into())));
                        } else {
                            // A cancelled authorization must not strand a
                            // reusable portal grant in an occupied state.
                            in_use = reply.send(Ok(())).is_ok();
                        }
                    }
                    None => break,
                },
                event = active.events.next() => {
                    let failed = matches!(event, Some(Ok(EiEvent::Disconnected(_) | EiEvent::DeviceRemoved(_) | EiEvent::DevicePaused(_))) | Some(Err(_)) | None);
                    if failed {
                        healthy.store(false, Ordering::Release);
                        in_use = false;
                        if let Some(active) = session.take() {
                            if active.close().await.is_err() {
                                // Drop the dedicated portal connection instead
                                // of accumulating uncertain grants after Close
                                // failure. The next runtime attempt opens anew.
                                break;
                            }
                        }
                    }
                }
            }
        } else {
            match commands.recv().await {
                Some(Command::Begin(reply)) => {
                    match tokio::time::timeout(
                        AUTHORIZE_TIMEOUT,
                        PortalEisSession::open(&portal, request_keyboard, verbose),
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
                            in_use = true;
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
                        result = tokio::time::timeout(AUTHORIZE_TIMEOUT, PortalEisSession::open(&portal, request_keyboard, verbose)) => result.unwrap_or_else(|_| Err(PlatformError::Backend("RemoteDesktop authorization timed out".into()))),
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
                                in_use = true;
                                session = Some(opened);
                            }
                        }
                        Err(err) => {
                            let _ = reply.send(Err(err));
                        }
                    }
                }
                Some(Command::Event(_, reply) | Command::Release(reply) | Command::Park(reply)) => {
                    let _ = reply.send(Err(PlatformError::SessionClosed));
                }
                Some(Command::End(reply)) => {
                    let _ = reply.send(Ok(()));
                    break;
                }
                Some(
                    Command::Resume(reply)
                    | Command::Enter(_, reply)
                    | Command::Topology(_, _, reply)
                    | Command::Exits(_, reply),
                ) => {
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

async fn authorize_devices(
    portal: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    request_keyboard: bool,
    verbose: bool,
    authorization: &mut super::authorization::Authorization,
) -> Result<(), PlatformError> {
    let requested = if request_keyboard {
        DeviceType::Pointer | DeviceType::Keyboard
    } else {
        DeviceType::Pointer.into()
    };
    if verbose {
        eprintln!(
            "[wayland] RemoteDesktop: SelectDevices persist_mode=2 restore_token_supplied={}",
            authorization.restore_token().is_some()
        );
    }
    portal
        .select_devices(
            session,
            SelectDevicesOptions::default().set_devices(requested)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(authorization.restore_token()),
        )
        .await
        .and_then(|request| request.response().map(|_| ()))
        .map_err(|_| PlatformError::Backend("RemoteDesktop SelectDevices rejected; retry normal portal approval (any restore token was consumed)".into()))?;
    if verbose {
        eprintln!("[wayland] RemoteDesktop: SelectDevices accepted; Start pending; portal may restore or request consent");
    }
    let selected = portal
        .start(
            session,
            None,
            StartOptions::default(),
        )
        .await
        .and_then(|request| request.response())
        .map_err(|_| PlatformError::Backend("RemoteDesktop Start denied or failed; retry normal portal approval (any restore token was consumed)".into()))?;
    let diagnostic = persistence_result(authorization.restore_token(), selected.restore_token());
    if verbose {
        eprintln!(
            "[wayland] RemoteDesktop: Start succeeded pointer={} keyboard={} {diagnostic}",
            selected.devices().contains(DeviceType::Pointer),
            selected.devices().contains(DeviceType::Keyboard)
        );
    }
    authorization
        .granted(selected.restore_token())
        .map_err(PlatformError::Backend)?;
    if verbose {
        eprintln!("[wayland] RemoteDesktop: returned authorization state persisted; {diagnostic}");
    }
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
    Ok(())
}

fn persistence_result(submitted: Option<&str>, returned: Option<&str>) -> String {
    format!("restore_attempted={} replacement_received={} replacement_same_value={} restoration_confirmation=not_exposed_by_portal",
        submitted.is_some(), returned.is_some(), submitted.is_some() && submitted == returned)
}

#[cfg(test)]
mod persistence_tests {
    use super::super::authorization::{Authorization, Context, Kind};
    use super::*;
    use std::{
        collections::HashMap,
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
        sync::atomic::AtomicU32,
    };
    use zbus::{
        message::Header,
        zvariant::{OwnedObjectPath, OwnedValue, Str},
    };

    struct Bus(Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn value(text: &str) -> OwnedValue {
        OwnedValue::from(Str::from(text))
    }
    async fn response(
        connection: &zbus::Connection,
        header: &Header<'_>,
        options: &HashMap<String, OwnedValue>,
        code: u32,
        results: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        let sender = header.sender().unwrap();
        let token = <&str>::try_from(&options["handle_token"])
            .unwrap()
            .to_owned();
        let path = OwnedObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/request/{}/{}",
            sender.as_str().trim_start_matches(':').replace('.', "_"),
            token
        ))
        .unwrap();
        connection
            .emit_signal(
                Some(sender.as_str()),
                &path,
                "org.freedesktop.portal.Request",
                "Response",
                &(code, results),
            )
            .await
            .unwrap();
        path
    }
    type Selections = Arc<Mutex<Vec<(u32, Option<String>)>>>;
    struct Portal {
        selections: Selections,
        starts: AtomicU32,
        reject: bool,
        devices: u32,
        reissue_same_value: bool,
    }
    #[zbus::interface(name = "org.freedesktop.portal.RemoteDesktop")]
    impl Portal {
        #[zbus(property, name = "version")]
        fn version(&self) -> u32 {
            2
        }
        #[zbus(property)]
        fn available_device_types(&self) -> u32 {
            3
        }
        async fn create_session(
            &self,
            options: HashMap<String, OwnedValue>,
            #[zbus(header)] header: Header<'_>,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> OwnedObjectPath {
            let sender = header
                .sender()
                .unwrap()
                .as_str()
                .trim_start_matches(':')
                .replace('.', "_");
            let token = <&str>::try_from(&options["session_handle_token"])
                .unwrap()
                .to_owned();
            let mut results = HashMap::new();
            results.insert(
                "session_handle".into(),
                value(&format!(
                    "/org/freedesktop/portal/desktop/session/{sender}/{token}"
                )),
            );
            response(connection, &header, &options, 0, results).await
        }
        async fn select_devices(
            &self,
            _session: OwnedObjectPath,
            options: HashMap<String, OwnedValue>,
            #[zbus(header)] header: Header<'_>,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> OwnedObjectPath {
            let mode = u32::try_from(&options["persist_mode"]).unwrap();
            let token = options
                .get("restore_token")
                .map(|value| <&str>::try_from(value).unwrap().to_owned());
            self.selections.lock().unwrap().push((mode, token));
            response(
                connection,
                &header,
                &options,
                if self.reject { 2 } else { 0 },
                HashMap::new(),
            )
            .await
        }
        async fn start(
            &self,
            _session: OwnedObjectPath,
            _parent: String,
            options: HashMap<String, OwnedValue>,
            #[zbus(header)] header: Header<'_>,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> OwnedObjectPath {
            let index = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
            let index = if self.reissue_same_value { 1 } else { index };
            let mut results = HashMap::new();
            results.insert("devices".into(), OwnedValue::from(self.devices));
            results.insert(
                "restore_token".into(),
                value(&format!("private-token-{index}")),
            );
            response(connection, &header, &options, 0, results).await
        }
    }
    async fn fixture(
        reject: bool,
        devices: u32,
        reissue_same_value: bool,
    ) -> (
        tempfile::TempDir,
        Bus,
        zbus::Connection,
        RemoteDesktop,
        Selections,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let bus = Bus(child);
        let selections = Arc::new(Mutex::new(Vec::new()));
        let server = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .serve_at(
                "/org/freedesktop/portal/desktop",
                Portal {
                    selections: selections.clone(),
                    starts: AtomicU32::new(0),
                    reject,
                    devices,
                    reissue_same_value,
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();
        let portal = RemoteDesktop::with_connection(client).await.unwrap();
        (dir, bus, server, portal, selections)
    }
    #[tokio::test]
    async fn official_select_start_options_rotate_tokens_across_service_sessions() {
        let (dir, _bus, _server, portal, selections) = fixture(false, 3, false).await;
        let state = dir.path().join("ukvm/portal-authorization");
        for _ in 0..2 {
            let mut authorization =
                Authorization::begin_in(&state, Kind::RemoteDesktop, Context::current(2)).unwrap();
            let session = portal
                .create_session(CreateSessionOptions::default())
                .await
                .unwrap();
            tokio::time::timeout(
                Duration::from_secs(5),
                authorize_devices(&portal, &session, true, false, &mut authorization),
            )
            .await
            .unwrap()
            .unwrap();
        }
        assert_eq!(
            *selections.lock().unwrap(),
            vec![(2, None), (2, Some("private-token-1".into()))]
        );
        let authorization =
            Authorization::begin_in(&state, Kind::RemoteDesktop, Context::current(2)).unwrap();
        assert_eq!(authorization.restore_token(), Some("private-token-2"));
    }
    #[tokio::test]
    async fn rejected_restore_does_not_leak_or_reuse_token() {
        let (dir, _bus, _server, portal, _selections) = fixture(true, 3, false).await;
        let state = dir.path().join("ukvm/portal-authorization");
        let mut first =
            Authorization::begin_in(&state, Kind::RemoteDesktop, Context::current(2)).unwrap();
        first.granted(Some("expired-private-token")).unwrap();
        drop(first);
        let mut authorization =
            Authorization::begin_in(&state, Kind::RemoteDesktop, Context::current(2)).unwrap();
        let session = portal
            .create_session(CreateSessionOptions::default())
            .await
            .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            authorize_devices(&portal, &session, true, false, &mut authorization),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(!format!("{error:?}").contains("expired-private-token"));
        drop(authorization);
        let next =
            Authorization::begin_in(&state, Kind::RemoteDesktop, Context::current(2)).unwrap();
        assert!(next.restore_token().is_none());
    }
    #[tokio::test]
    async fn remembered_token_does_not_replace_required_device_authorization() {
        let (dir, _bus, _server, portal, _selections) = fixture(false, 1, false).await;
        let mut authorization = Authorization::begin_in(
            &dir.path().join("ukvm/portal-authorization"),
            Kind::RemoteDesktop,
            Context::current(2),
        )
        .unwrap();
        let session = portal
            .create_session(CreateSessionOptions::default())
            .await
            .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            authorize_devices(&portal, &session, true, false, &mut authorization),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(error, PlatformError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn frontend_reissues_same_token_after_success_without_fresh_request() {
        let (dir, _bus, _server, portal, selections) = fixture(false, 3, true).await;
        let state = dir.path().join("ukvm/portal-authorization");
        for attempt in 0..3 {
            let mut authorization =
                Authorization::begin_in(&state, Kind::RemoteDesktop, Context::current(2)).unwrap();
            assert_eq!(
                authorization.restore_token(),
                if attempt == 0 {
                    None
                } else {
                    Some("private-token-1")
                }
            );
            let pending: serde_json::Value =
                serde_json::from_slice(&std::fs::read(state.join("remote-desktop.json")).unwrap())
                    .unwrap();
            assert!(pending["token"].is_null());
            let session = portal
                .create_session(CreateSessionOptions::default())
                .await
                .unwrap();
            authorize_devices(&portal, &session, true, true, &mut authorization)
                .await
                .unwrap();
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(state.join("remote-desktop.json")).unwrap())
                    .unwrap();
            assert_eq!(saved["token"], "private-token-1");
            drop(authorization);
        }
        assert_eq!(
            *selections.lock().unwrap(),
            vec![
                (2, None),
                (2, Some("private-token-1".into())),
                (2, Some("private-token-1".into()))
            ]
        );
    }
    #[test]
    fn restoration_diagnostics_never_include_credentials_or_claim_silent_restore() {
        let secret = "never-expose-the-credential";
        for (submitted, returned) in [
            (None, Some(secret)),
            (Some(secret), Some(secret)),
            (Some(secret), None),
        ] {
            let diagnostic = persistence_result(submitted, returned);
            assert!(!diagnostic.contains(secret));
            assert!(diagnostic.contains("restoration_confirmation=not_exposed_by_portal"));
        }
        assert!(
            persistence_result(Some(secret), Some(secret)).contains("replacement_same_value=true")
        );
    }
}
