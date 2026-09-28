//! Permissioned RemoteDesktop portal and EI sender for a Wayland session.
//! A dedicated user thread owns the async portal session so the existing
//! synchronous platform trait never blocks the receiver's Tokio executor.

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
use tokio::sync::mpsc;
use ukvm_protocol::{InputEvent, KeyCode};

const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(4);
const EI_DEVICE_TIMEOUT: Duration = Duration::from_secs(8);
const COMMAND_QUEUE: usize = 256;

enum Command {
    Begin(reply::SyncSender<Result<(), PlatformError>>),
    Event(InputEvent, reply::SyncSender<Result<(), PlatformError>>),
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
        Self::open_with_devices(true).await
    }

    /// The first physical proof requests pointer consent only.
    pub async fn open_pointer_only() -> Result<Self, PlatformError> {
        Self::open_with_devices(false).await
    }

    async fn open_with_devices(request_keyboard: bool) -> Result<Self, PlatformError> {
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
                        runtime.block_on(run_actor(rx, actor_caps, actor_health, request_keyboard));
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

    fn inject(&mut self, event: &InputEvent) -> Result<(), PlatformError> {
        self.request(
            |reply| Command::Event(event.clone(), reply),
            COMMAND_TIMEOUT,
        )
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
    clock: Instant,
}

impl PortalEisSession {
    async fn open(request_keyboard: bool) -> Result<Self, PlatformError> {
        let portal = RemoteDesktop::new()
            .await
            .map_err(|err| PlatformError::Backend(format!("RemoteDesktop unavailable: {err}")))?;
        if portal.version() < 2 {
            return Err(PlatformError::Unsupported(
                "RemoteDesktop portal does not provide ConnectToEIS",
            ));
        }
        let session = portal
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(|err| PlatformError::Backend(format!("CreateSession failed: {err}")))?;
        let result = Self::open_session(portal, session, request_keyboard).await;
        result
    }

    async fn open_session(
        portal: RemoteDesktop,
        session: Session<RemoteDesktop>,
        request_keyboard: bool,
    ) -> Result<Self, PlatformError> {
        let result = async {
            let requested = if request_keyboard {
                DeviceType::Pointer | DeviceType::Keyboard
            } else {
                DeviceType::Pointer.into()
            };
            portal
                .select_devices(
                    &session,
                    SelectDevicesOptions::default().set_devices(requested),
                )
                .await
                .and_then(|request| request.response().map(|_| ()))
                .map_err(|err| PlatformError::Backend(format!("SelectDevices failed: {err}")))?;
            let selected = portal
                .start(&session, None, StartOptions::default())
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
            let fd = portal
                .connect_to_eis(&session, ConnectToEISOptions::default())
                .await
                .map_err(|err| PlatformError::Backend(format!("ConnectToEIS failed: {err}")))?;
            let stream = UnixStream::from(fd);
            stream
                .set_nonblocking(true)
                .map_err(|err| PlatformError::Backend(err.to_string()))?;
            let context = ei::Context::new(stream)
                .map_err(|err| PlatformError::Backend(format!("EI socket failed: {err}")))?;
            let (connection, mut events) = context
                .handshake_tokio("ukvm", ei::handshake::ContextType::Sender)
                .await
                .map_err(|err| PlatformError::Backend(format!("EI handshake failed: {err}")))?;
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
            Ok((connection, events, pointer, keyboard))
        }
        .await;
        match result {
            Ok((connection, events, pointer, keyboard)) => Ok(Self {
                _portal: portal,
                portal_session: session,
                connection,
                events,
                pointer,
                keyboard,
                held_keys: HashSet::new(),
                held_buttons: HashSet::new(),
                clock: Instant::now(),
            }),
            Err(err) => {
                let _ = session.close().await;
                Err(err)
            }
        }
    }

    fn capabilities(&self) -> InjectionCapabilities {
        InjectionCapabilities {
            pointer_relative: true,
            pointer_absolute: false,
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
                    match PortalEisSession::open(request_keyboard).await {
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
                Some(Command::Event(_, reply) | Command::Release(reply) | Command::End(reply)) => {
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
