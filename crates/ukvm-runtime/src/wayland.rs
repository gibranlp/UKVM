use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;
use ukvm_config::{Config, Link};
use ukvm_discovery::{Advertisement, DiscoveryResult, DiscoveryService};
use ukvm_network::{
    identity::{default_state_dir, Identity},
    transport::{self, ReceiverNotice},
    trust::{cert_path, default_trust_dir, resolve_address, TrustStore},
};
use ukvm_platform::{
    linux_wayland::{
        self, CaptureNotice, CaptureTiming, EmergencyRearmGuard, ReleaseMode, WaylandInputInjector,
    },
    RemoteInputSink,
};
use ukvm_protocol::WireMessage;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PeerAvailability {
    Dormant,
    Connecting,
    Authenticated,
    Ready,
    Active,
    Unavailable,
    Stopped,
}

struct Context {
    config: Config,
    identity: Identity,
    trust_dir: PathBuf,
    gate: FocusGate,
    stop: AtomicBool,
    verbose: bool,
    hints: Mutex<Vec<DiscoveryResult>>,
    availability: Mutex<PeerAvailability>,
    rearm: Mutex<EmergencyRearmGuard>,
}
impl Context {
    fn availability(&self, next: PeerAvailability) {
        let mut current = self.availability.lock().unwrap();
        if *current != next {
            self.log(format!("peer availability: {current:?} -> {next:?}"));
            *current = next;
        }
    }
    fn log(&self, message: impl std::fmt::Display) {
        if self.verbose {
            eprintln!("[runtime] {message}");
        }
    }
}
async fn cancelled(stop: &mut watch::Receiver<bool>) {
    if *stop.borrow() {
        return;
    }
    let _ = stop.changed().await;
}
async fn pause(delay: Duration, stop: &mut watch::Receiver<bool>) {
    tokio::select! { _ = tokio::time::sleep(delay) => {}, _ = cancelled(stop) => {} }
}

pub(super) async fn run(verbose: bool) -> Result<(), String> {
    let path = ukvm_config::default_path().map_err(|e| e.to_string())?;
    let config = ukvm_config::load_or_default(&path).map_err(|e| e.to_string())?;
    if config.layout.links.len() > 1 {
        return Err("this runtime supports one outgoing layout edge per node; multiple outgoing edges are not yet supported".into());
    }
    let trust_dir = default_trust_dir()?;
    let store = TrustStore::load(&trust_dir)?;
    for link in &config.layout.links {
        if store.find_id(&link.peer_id).is_none() {
            return Err("layout references an unknown trusted peer".into());
        }
    }
    let identity = Identity::load_or_create(&default_state_dir()?)?;
    let listener = transport::bind(&config.network.listen).await?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let context = Context {
        config,
        identity,
        trust_dir,
        gate: FocusGate::default(),
        stop: AtomicBool::new(false),
        verbose,
        hints: Mutex::new(Vec::new()),
        availability: Mutex::new(PeerAvailability::Dormant),
        rearm: Mutex::new(EmergencyRearmGuard::Ready),
    };
    let (stop_tx, stop) = watch::channel(false);
    // Register SIGINT before announcing readiness and retain this exact
    // future, so an interrupt cannot race between successive registrations.
    let mut interrupt = Box::pin(tokio::signal::ctrl_c());
    let early = std::future::poll_fn(|cx| {
        use std::future::Future;
        std::task::Poll::Ready(match interrupt.as_mut().poll(cx) {
            std::task::Poll::Ready(result) => Some(result),
            std::task::Poll::Pending => None,
        })
    })
    .await;
    if let Some(result) = early {
        return result.map_err(|e| e.to_string());
    }
    println!(
        "UKVM running as {} on {}. Ctrl+C stops; Ctrl+Alt+Shift+Escape releases control.",
        context.config.node.name,
        listener.local_addr().map_err(|e| e.to_string())?
    );
    // Structured concurrency: no detached runtime worker or cancellation of an
    // active capture/receiver future. Each worker executes its own cleanup.
    let signal = async {
        let result = interrupt.await;
        context.stop.store(true, Ordering::Release);
        let _ = stop_tx.send(true);
        result.map_err(|e| e.to_string())
    };
    let (signal_result, (), (), ()) = tokio::join!(
        signal,
        receiver(&context, listener, stop.clone()),
        controller(&context, stop.clone()),
        discovery(&context, port, stop),
    );
    context.availability(PeerAvailability::Stopped);
    context.log("all sessions closed; local input restored");
    println!("UKVM stopped.");
    signal_result
}

async fn discovery(context: &Context, port: u16, mut stop: watch::Receiver<bool>) {
    let mut backoff = Backoff::default();
    while !context.stop.load(Ordering::Acquire) {
        let service = match DiscoveryService::new(None) {
            Ok(service) => service,
            Err(error) => {
                context.log(format!("discovery unavailable: {error}"));
                pause(backoff.failed(), &mut stop).await;
                continue;
            }
        };
        // Runtime advertises its authenticated control endpoint as SRV. Pairing
        // remains a separately invoked ceremony; run never opens pairing.
        let advertisement = Advertisement {
            node_name: context.config.node.name.clone(),
            node_id: context.identity.id.clone(),
            pairing_port: port,
            control_port: port,
            pairing_version: ukvm_network::pairing::PAIRING_VERSION,
            pairing_available: false,
            input_version: (ukvm_protocol::PROTOCOL_MAJOR, ukvm_protocol::PROTOCOL_MINOR),
        };
        let guard = tokio::select! {
            _ = cancelled(&mut stop) => return,
            result = service.advertise(&advertisement) => match result {
                Ok(guard) => guard,
                Err(error) => { context.log(format!("advertisement failed: {error}")); pause(backoff.failed(), &mut stop).await; continue; }
            }
        };
        context.log("discovery advertisement active (control endpoint; no pairing)");
        backoff.reset();
        loop {
            let result = tokio::select! {
                _ = cancelled(&mut stop) => break,
                result = service.browse_for(Duration::from_secs(4)) => result,
            };
            match result {
                Ok(hints) => {
                    context.log(format!("discovery: {} endpoint hints", hints.len()));
                    *context.hints.lock().unwrap() = hints;
                }
                Err(error) => {
                    context.log(format!("discovery stopped: {error}"));
                    break;
                }
            }
            pause(Duration::from_secs(6), &mut stop).await;
            if context.stop.load(Ordering::Acquire) {
                break;
            }
        }
        drop(guard);
        context.hints.lock().unwrap().clear();
        pause(backoff.failed(), &mut stop).await;
    }
}

/// Open only for an authorized session; retain the legitimate grant until
/// invalidation or runtime shutdown. No restore token or synthetic permission.
struct LazySink {
    sink: Option<RemoteInputSink<WaylandInputInjector>>,
    verbose: bool,
    policy: Option<(PathBuf, String)>,
}
impl LazySink {
    fn authorize(&self) -> Result<(), String> {
        let (directory, id) = self
            .policy
            .as_ref()
            .ok_or("no authenticated incoming peer")?;
        TrustStore::load(directory)?
            .authorize(id, ukvm_network::trust::SessionPermission::ControlThisNode)?;
        Ok(())
    }
    fn shutdown(&mut self) {
        if let Some(mut sink) = self.sink.take() {
            if let Err(error) = sink.shutdown() {
                eprintln!("[runtime] injector cleanup: {error}");
            }
        }
    }
}
impl InputSink for LazySink {
    fn kind(&self) -> SinkKind {
        SinkKind::RemoteInput
    }
    fn begin_session(&mut self) -> Result<(), String> {
        Err("runtime authorization requires async begin".into())
    }
    fn begin_session_async(&mut self) -> SinkFuture<'_> {
        Box::pin(async {
            self.authorize()?;
            if self.sink.is_none() {
                self.sink = Some(RemoteInputSink::new(
                    WaylandInputInjector::open_verbose(self.verbose)
                        .await
                        .map_err(|e| e.to_string())?,
                ));
            }
            let result = self
                .sink
                .as_mut()
                .unwrap()
                .begin_session_async()
                .await
                .and_then(|()| self.authorize());
            if result.is_err() {
                self.shutdown();
            }
            result
        })
    }
    fn enter_remote(&mut self, entry: EdgePosition) -> Result<(), String> {
        self.authorize()?;
        self.sink
            .as_mut()
            .ok_or("injector unavailable")?
            .enter_remote(entry)
    }
    fn handle_event(&mut self, event: &InputEvent) -> Result<SinkAction, String> {
        self.sink
            .as_mut()
            .ok_or("injector unavailable")?
            .handle_event(event)
    }
    fn is_healthy(&self) -> bool {
        self.sink.as_ref().is_none_or(InputSink::is_healthy)
    }
    fn end_session(&mut self) -> Result<(), String> {
        let result = self.sink.as_mut().map_or(Ok(()), InputSink::end_session);
        if result.is_err() {
            self.shutdown();
        }
        result
    }
}

async fn receiver(
    context: &Context,
    listener: tokio::net::TcpListener,
    mut stop: watch::Receiver<bool>,
) {
    let mut sink = ExclusiveSink {
        inner: LazySink {
            sink: None,
            verbose: context.verbose,
            policy: None,
        },
        gate: context.gate.clone(),
        reserved: false,
    };
    loop {
        let accepted = tokio::select! { _ = cancelled(&mut stop) => break, result = listener.accept() => result };
        let (tcp, address) = match accepted {
            Ok(value) => value,
            Err(error) => {
                context.log(format!("accept failed: {error}"));
                pause(Duration::from_secs(1), &mut stop).await;
                continue;
            }
        };
        let store = match TrustStore::load(&context.trust_dir) {
            Ok(store) => store,
            Err(error) => {
                context.log(error);
                continue;
            }
        };
        let connection = tokio::select! {
            _ = cancelled(&mut stop) => break,
            result = transport::accept_remote_trusted(tcp, &context.identity, &store, &context.trust_dir, &context.config.node.name, &sink) => match result {
                Ok(connection) => connection,
                Err(error) => { context.log(format!("connection {address} rejected: {error}")); continue; }
            }
        };
        let peer_id = connection.peer.id.clone();
        sink.inner.policy = Some((context.trust_dir.clone(), peer_id.clone()));
        context.log(format!("authenticated incoming peer {peer_id}"));
        let shutdown = async {
            loop {
                let allowed = TrustStore::load(&context.trust_dir)
                    .ok()
                    .and_then(|s| s.find_id(&peer_id).cloned())
                    .is_some_and(|p| p.permissions.connect && p.permissions.control_this_node);
                if !allowed {
                    context.log("incoming permission revoked or denied");
                    return;
                }
                tokio::select! {
                    _ = cancelled(&mut stop) => return,
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {},
                }
            }
        };
        let result = transport::receive_session_until(
            connection,
            &mut sink,
            |notice| match notice {
                ReceiverNotice::SessionStarted => {
                    context.log("receiver portal/EI ready; session authorized")
                }
                ReceiverNotice::RemoteEntered(_) => {
                    context.log("incoming edge transition; controlled focus")
                }
                ReceiverNotice::RemoteReturned(_) => {
                    context.log("ReturnControl; held input released")
                }
                ReceiverNotice::SessionEnded(_) => context.log("incoming session cleanup complete"),
                ReceiverNotice::Counts(_) => {}
            },
            shutdown,
        )
        .await;
        if let Err(error) = result {
            context.log(format!("incoming session ended: {error}"));
        }
        if context.stop.load(Ordering::Acquire) {
            break;
        }
    }
    let _ = sink.end_session();
    sink.inner.shutdown();
}

fn protocol_edge(edge: ukvm_config::Edge) -> ukvm_protocol::Edge {
    match edge {
        ukvm_config::Edge::Left => ukvm_protocol::Edge::Left,
        ukvm_config::Edge::Right => ukvm_protocol::Edge::Right,
        ukvm_config::Edge::Top => ukvm_protocol::Edge::Top,
        ukvm_config::Edge::Bottom => ukvm_protocol::Edge::Bottom,
    }
}
fn capture_edge(edge: ukvm_config::Edge) -> linux_wayland::Edge {
    match edge {
        ukvm_config::Edge::Left => linux_wayland::Edge::Left,
        ukvm_config::Edge::Right => linux_wayland::Edge::Right,
        ukvm_config::Edge::Top => linux_wayland::Edge::Top,
        ukvm_config::Edge::Bottom => linux_wayland::Edge::Bottom,
    }
}

fn addresses(context: &Context, peer: &ukvm_network::trust::Peer) -> Vec<String> {
    let mut addresses = Vec::new();
    for hint in context
        .hints
        .lock()
        .unwrap()
        .iter()
        .filter(|h| h.advertised_node_id.as_deref() == Some(&peer.id))
    {
        if let Some(port) = hint.control_port {
            for address in &hint.addresses {
                let mut address = *address;
                address.set_port(port);
                addresses.push(address.to_string());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    addresses.retain(|address| seen.insert(address.clone()));
    addresses.truncate(15);
    if let Some(address) = peer.address.as_ref().and_then(|a| resolve_address(a).ok()) {
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    addresses
}
async fn controller(context: &Context, mut stop: watch::Receiver<bool>) {
    let Some(link) = context.config.layout.links.first() else {
        cancelled(&mut stop).await;
        return;
    };
    let mut backoff = Backoff::default();
    while !context.stop.load(Ordering::Acquire) {
        if context.gate.state() != Focus::Local {
            pause(Duration::from_millis(250), &mut stop).await;
            continue;
        }
        if !context.rearm.lock().unwrap().allows_arming() {
            wait_for_inward(context, &mut stop).await;
            continue;
        }
        context.availability(PeerAvailability::Connecting);
        let result = tokio::select! {
            _ = cancelled(&mut stop) => break,
            result = prepare(context, link) => result,
        };
        let sender = match result {
            Ok(sender) => sender,
            Err(error) => {
                context.availability(PeerAvailability::Unavailable);
                let delay = backoff.failed();
                context.log(format!(
                    "peer unavailable: {error}; retry in {}s (local input untouched)",
                    delay.as_secs()
                ));
                pause(delay, &mut stop).await;
                continue;
            }
        };
        if context.stop.load(Ordering::Acquire) || !arm_outgoing(context) {
            let _ = sender.finish().await;
            continue;
        }
        context.availability(PeerAvailability::Ready);
        context.log("remote authorized and ready; arming local edge");
        let result = capture(context, link, &sender).await;
        // Keep focus leased until the local portal has completed Release/Close.
        context.gate.outbound_closed();
        let finished = sender.finish().await;
        context.log("outgoing session closed; local input restored");
        if let Err(error) = result.and(finished.map(|_| ())) {
            context.availability(PeerAvailability::Unavailable);
            context.log(format!("outgoing failure: {error}"));
            pause(backoff.failed(), &mut stop).await;
        } else {
            backoff.reset();
            pause(Duration::from_millis(250), &mut stop).await;
        }
    }
}

fn arm_outgoing(context: &Context) -> bool {
    context.rearm.lock().unwrap().allows_arming() && context.gate.arm()
}

async fn wait_for_inward(context: &Context, stop: &mut watch::Receiver<bool>) {
    eprintln!("Emergency release: launching edge remains disarmed. Move at least 32 logical pixels inward to rearm it.");
    let mut backoff = Backoff::default();
    let mut reported_error = None;
    while !context.stop.load(Ordering::Acquire) && !context.rearm.lock().unwrap().allows_arming() {
        if context.gate.state() != Focus::Local {
            pause(Duration::from_millis(100), stop).await;
            continue;
        }
        let result = tokio::select! {
            _ = cancelled(stop) => return,
            position = linux_wayland::local_pointer_position() => position,
        };
        match result {
            Ok(position) => {
                backoff.reset();
                reported_error = None;
                // Ignore observations made while an incoming peer owns focus.
                if context.gate.state() == Focus::Local
                    && context.rearm.lock().unwrap().observe_local(position)
                {
                    context.log("inward movement observed; emergency rearm guard cleared");
                    return;
                }
                pause(Duration::from_millis(100), stop).await;
            }
            Err(error) => {
                let message = error.to_string();
                if reported_error.as_ref() != Some(&message) {
                    eprintln!(
                        "Cannot observe inward movement: {message}; launching edge stays disarmed."
                    );
                    reported_error = Some(message);
                }
                pause(backoff.failed(), stop).await;
            }
        }
    }
}

async fn prepare(context: &Context, link: &Link) -> Result<transport::Sender, String> {
    let store = TrustStore::load(&context.trust_dir)?;
    let peer = store.find_id(&link.peer_id).ok_or("layout peer removed")?;
    if !peer.permissions.connect {
        return Err("outbound connection denied by local policy".into());
    }
    let mut last_error = "no connection address available".to_string();
    for address in addresses(context, peer) {
        context.log(format!("connecting to endpoint hint {address}"));
        match transport::connect(
            &address,
            &context.identity,
            &cert_path(&context.trust_dir, &peer.id),
            &context.config.node.name,
        )
        .await
        {
            Ok(connection) => {
                if connection.peer.id != peer.id {
                    return Err("authenticated identity conflicts with layout".into());
                }
                context.availability(PeerAvailability::Authenticated);
                context.log(format!("pinned identity verified: {}", peer.id));
                // Keep discovery addresses ephemeral. Subsequent attempts verify
                // the pin again; no trust-store edit races or auto-trust.
                return transport::start_remote_sender(connection, protocol_edge(link.entry_edge))
                    .await;
            }
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

async fn capture(context: &Context, link: &Link, sender: &transport::Sender) -> Result<(), String> {
    let entered = AtomicBool::new(false);
    let failed = AtomicBool::new(false);
    let mut alive = sender.alive.clone();
    let mut returned = sender.returned.clone();
    let mut notice = |notice| match notice {
        CaptureNotice::Activated { edge_fraction, .. } => {
            // A competing inbound entry can disarm this barrier. Never forward
            // input from a failed focus acquisition.
            if context.gate.activate()
                && *sender.alive.borrow()
                && !context.stop.load(Ordering::Acquire)
            {
                let entry = EdgePosition {
                    edge: protocol_edge(link.entry_edge),
                    fraction: edge_fraction,
                };
                if sender
                    .control
                    .try_send(WireMessage::RemoteEntry(entry))
                    .is_ok()
                {
                    entered.store(true, Ordering::Release);
                    context.availability(PeerAvailability::Active);
                    context.log("edge transition; controlling peer");
                } else {
                    failed.store(true, Ordering::Release);
                }
            } else {
                failed.store(true, Ordering::Release);
            }
        }
        CaptureNotice::EmergencyRelease { boundary } => {
            context.rearm.lock().unwrap().suppress(boundary);
            context.log("emergency chord detected; launching edge disarmed until inward movement");
            // Terminate forwarding before local Release/Close. SessionEnd
            // releases remote held state; unsent events are discarded.
            if sender.control.try_send(WireMessage::SessionEnd).is_err() {
                context.log(
                    "emergency session end queue unavailable; sender finish will close the session",
                );
            }
        }
        CaptureNotice::Enabled => context.log("InputCapture edge enabled"),
        CaptureNotice::RemoteReturn(_) => context.log("ReturnControl received; releasing capture"),
        CaptureNotice::ReleaseResult(_) => context.log("InputCapture Release completed"),
        CaptureNotice::Closed(_) => context.log("InputCapture closed"),
        _ => {}
    };
    let mut event = |event| {
        if !entered.load(Ordering::Acquire) || failed.load(Ordering::Acquire) {
            return Err("focus unavailable; releasing capture".into());
        }
        match sender.queue.try_send(event) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(InputEvent::PointerMotion {
                ..
            })) => Ok(()),
            Err(_) => Err("input delivery failed; releasing capture".into()),
        }
    };
    let peer_id = link.peer_id.clone();
    let mut last_policy: Option<std::time::Instant> = None;
    let mut permitted = false;
    let mut healthy = || {
        if last_policy.is_none_or(|last| last.elapsed() >= Duration::from_millis(250)) {
            permitted = TrustStore::load(&context.trust_dir)
                .ok()
                .and_then(|s| s.find_id(&peer_id).cloned())
                .is_some_and(|p| p.permissions.connect);
            last_policy = Some(std::time::Instant::now());
        }
        *alive.borrow_and_update()
            && permitted
            && !failed.load(Ordering::Acquire)
            && !context.stop.load(Ordering::Acquire)
            && matches!(context.gate.state(), Focus::Armed | Focus::Controlling)
    };
    let mut return_request = || *returned.borrow_and_update();
    linux_wayland::capture_with_events_mode(
        capture_edge(link.edge),
        None,
        CaptureTiming {
            duration: Duration::from_secs(3600),
            active_duration: None,
            release_mode: ReleaseMode::EmergencyChord,
        },
        &mut notice,
        &mut event,
        &mut healthy,
        &mut return_request,
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
#[path = "wayland_tests.rs"]
mod tests;
