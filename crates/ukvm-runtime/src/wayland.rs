use super::*;
use futures_util::{stream::FuturesUnordered, StreamExt};
use std::collections::HashMap;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;
use ukvm_config::{Config, Link, TopologyNode};
use ukvm_core::Coordinate;
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

pub(crate) mod clipboard;

struct Context {
    clipboard: clipboard::Clipboard,
    config: Config,
    identity: Identity,
    trust_dir: PathBuf,
    gate: FocusGate,
    stop: AtomicBool,
    verbose: bool,
    hints: Mutex<Vec<DiscoveryResult>>,
    availability: Mutex<HashMap<Coordinate, PeerAvailability>>,
    pool: Mutex<HashMap<Coordinate, transport::Sender>>,
    active: Mutex<Option<Coordinate>>,
    rearm: Mutex<EmergencyRearmGuard>,
    incoming_peer: Mutex<Option<String>>,
    capture_status: Mutex<String>,
    receiver_status: Arc<Mutex<String>>,
}
impl Context {
    fn availability(&self, node: &TopologyNode, next: PeerAvailability) {
        let mut states = self.availability.lock().unwrap();
        let current = states
            .entry(node.coordinate())
            .or_insert(PeerAvailability::Dormant);
        if *current != next {
            self.log(format!(
                "peer {} coordinate {}: {current:?} -> {next:?}",
                node.peer_id,
                node.coordinate()
            ));
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

pub(super) async fn run(verbose: bool, owner: &ownership::RuntimeOwner) -> Result<(), String> {
    let path = ukvm_config::default_path().map_err(|e| e.to_string())?;
    let config = ukvm_config::load_or_default(&path).map_err(|e| e.to_string())?;
    let trust_dir = default_trust_dir()?;
    let store = TrustStore::load(&trust_dir)?;
    for link in config.layout.topology() {
        if store.find_id(&link.peer_id).is_none() {
            return Err("layout references an unknown trusted peer".into());
        }
    }
    let identity = Identity::load_or_create(&default_state_dir()?)?;
    if config
        .layout
        .topology()
        .iter()
        .any(|node| node.peer_id.eq_ignore_ascii_case(&identity.id))
    {
        return Err("local identity cannot occupy a remote topology coordinate".into());
    }
    let listener = transport::bind(&config.network.listen).await?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let clipboard = clipboard::Clipboard::new(&identity.id)?;
    let context = Context {
        clipboard,
        config,
        identity,
        trust_dir,
        gate: FocusGate::default(),
        stop: AtomicBool::new(false),
        verbose,
        hints: Mutex::new(Vec::new()),
        availability: Mutex::new(HashMap::new()),
        pool: Mutex::new(HashMap::new()),
        active: Mutex::new(None),
        rearm: Mutex::new(EmergencyRearmGuard::Ready),
        incoming_peer: Mutex::new(None),
        capture_status: Mutex::new("not requested".into()),
        receiver_status: Arc::new(Mutex::new("consent not requested".into())),
    };
    let (stop_tx, stop) = watch::channel(false);
    // Register SIGINT before announcing readiness and retain this exact
    // future, so an interrupt cannot race between successive registrations.
    let mut interrupt = Box::pin(shutdown_signal());
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
    let (signal_result, (), (), (), (), (), (), ()) = tokio::join!(
        signal,
        receiver(&context, listener, stop.clone()),
        controller(&context, stop.clone()),
        discovery(&context, port, stop.clone()),
        peers(&context, stop.clone()),
        rearm_observer(&context, stop.clone()),
        clipboard::supervise(&context, stop.clone()),
        publish_status(&context, owner, stop),
    );
    for node in context.config.layout.topology() {
        context.availability(&node, PeerAvailability::Stopped);
    }
    context.log("all sessions closed; local input restored");
    println!("UKVM stopped.");
    signal_result
}

async fn shutdown_signal() -> Result<(), std::io::Error> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! { result = tokio::signal::ctrl_c() => result, _ = terminate.recv() => Ok(()) }
}

async fn publish_status(
    context: &Context,
    owner: &ownership::RuntimeOwner,
    mut stop: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = cancelled(&mut stop) => break,
            _ = tick.tick() => {
                let active = *context.active.lock().unwrap();
                let topology = context.config.layout.topology();
                let states = context.availability.lock().unwrap();
                let snapshot = ownership::RuntimeSnapshot {
                    clipboard: context.clipboard.snapshot(),
                    name: context.config.node.name.clone(),
                    node_id: context.identity.id.clone(),
                    state: "running".into(),
                    focus: format!("{:?}", context.gate.state()),
                    active_peer: active.and_then(|coordinate| topology.iter().find(|node| node.coordinate() == coordinate).map(|node| node.peer_id.clone()))
                        .or_else(|| context.incoming_peer.lock().unwrap().clone()),
                    active_coordinate: active.map(|coordinate| coordinate.to_string()),
                    capture: context.capture_status.lock().unwrap().clone(),
                    receiver: context.receiver_status.lock().unwrap().clone(),
                    peers: topology.iter().map(|node| ownership::PeerStatus {
                        id: node.peer_id.clone(), coordinate: node.coordinate().to_string(),
                        availability: format!("{:?}", states.get(&node.coordinate()).unwrap_or(&PeerAvailability::Dormant)),
                    }).collect(),
                    ..Default::default()
                };
                if let Err(error) = owner.publish(snapshot) { context.log(format!("status snapshot unavailable: {error}")); }
            }
        }
    }
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

mod incoming;
use incoming::receiver;

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
fn node_edge(node: &TopologyNode) -> ukvm_config::Edge {
    match (node.x.signum(), node.y.signum()) {
        (-1, _) => ukvm_config::Edge::Left,
        (1, _) => ukvm_config::Edge::Right,
        (_, -1) => ukvm_config::Edge::Top,
        _ => ukvm_config::Edge::Bottom,
    }
}
fn link_for(node: &TopologyNode) -> Link {
    let edge = node_edge(node);
    Link {
        edge,
        entry_edge: edge.opposite(),
        peer_id: node.peer_id.clone(),
    }
}

async fn peers(context: &Context, stop: watch::Receiver<bool>) {
    let nodes = context.config.layout.topology();
    let mut workers = FuturesUnordered::new();
    for node in &nodes {
        workers.push(peer_worker(context, node, stop.clone()));
    }
    while workers.next().await.is_some() {}
}
async fn peer_worker(context: &Context, node: &TopologyNode, mut stop: watch::Receiver<bool>) {
    let coordinate = node.coordinate();
    let mut backoff = Backoff::default();
    while !context.stop.load(Ordering::Acquire) {
        let state = context
            .pool
            .lock()
            .unwrap()
            .get(&coordinate)
            .map(|sender| *sender.alive.borrow());
        let permitted = TrustStore::load(&context.trust_dir)
            .ok()
            .and_then(|store| store.find_id(&node.peer_id).cloned())
            .is_some_and(|peer| peer.permissions.connect);
        if state == Some(true) && permitted {
            pause(Duration::from_millis(100), &mut stop).await;
            continue;
        }
        let expired = context.pool.lock().unwrap().remove(&coordinate);
        if let Some(sender) = expired {
            let _ = sender.finish().await;
        }
        if *context.active.lock().unwrap() != Some(coordinate) {
            context.availability(node, PeerAvailability::Connecting);
        } else {
            context.log(format!(
                "peer {} coordinate {}: preparing return standby",
                node.peer_id, coordinate
            ));
        }
        let link = link_for(node);
        let result = tokio::select! {
            _ = cancelled(&mut stop) => break,
            result = prepare_for(context, &link, true) => result,
        };
        match result {
            Ok(sender) => {
                if *context.active.lock().unwrap() != Some(coordinate) {
                    context.availability(node, PeerAvailability::Ready);
                } else {
                    context.log(format!(
                        "peer {} coordinate {}: return standby ready",
                        node.peer_id, coordinate
                    ));
                }
                context.pool.lock().unwrap().insert(coordinate, sender);
                backoff.reset();
            }
            Err(error) => {
                if *context.active.lock().unwrap() != Some(coordinate) {
                    context.availability(node, PeerAvailability::Unavailable);
                }
                let delay = backoff.failed();
                context.log(format!(
                    "peer {} coordinate {} unavailable: {error}; retry in {}s",
                    node.peer_id,
                    coordinate,
                    delay.as_secs()
                ));
                pause(delay, &mut stop).await;
            }
        }
    }
    let sender = context.pool.lock().unwrap().remove(&coordinate);
    if let Some(sender) = sender {
        let _ = sender.finish().await;
    }
}

fn allowed_edges(context: &Context) -> Vec<linux_wayland::Edge> {
    let guard = *context.rearm.lock().unwrap();
    let pool = context.pool.lock().unwrap();
    let store = TrustStore::load(&context.trust_dir).ok();
    context.config.layout.topology().iter().filter(|node| {
        store.as_ref().and_then(|store| store.find_id(&node.peer_id)).is_some_and(|peer| peer.permissions.connect)
        && node.x.abs() + node.y.abs() == 1 && pool.get(&node.coordinate()).is_some_and(|sender| *sender.alive.borrow())
        && !matches!(guard, EmergencyRearmGuard::AwaitingInward { boundary: Some(boundary) } if boundary.edge == capture_edge(node_edge(node)))
        && !matches!(guard, EmergencyRearmGuard::AwaitingInward { boundary: None })
    }).map(|node| capture_edge(node_edge(node))).collect()
}

async fn controller(context: &Context, mut stop: watch::Receiver<bool>) {
    let mut backoff = Backoff::default();
    while !context.stop.load(Ordering::Acquire) {
        let edges = allowed_edges(context);
        if context.gate.state() != Focus::Local || edges.is_empty() {
            pause(Duration::from_millis(100), &mut stop).await;
            continue;
        }
        if !context.gate.arm() {
            continue;
        }
        let result = capture_topology(context, &edges).await;
        context.gate.outbound_closed();
        let previous = context.active.lock().unwrap().take();
        if let Some(coordinate) = previous {
            if let Some(node) = context
                .config
                .layout
                .topology()
                .iter()
                .find(|node| node.coordinate() == coordinate)
            {
                let ready = context
                    .pool
                    .lock()
                    .unwrap()
                    .get(&coordinate)
                    .is_some_and(|sender| *sender.alive.borrow());
                context.availability(
                    node,
                    if ready {
                        PeerAvailability::Ready
                    } else {
                        PeerAvailability::Unavailable
                    },
                );
            }
        }
        context.log("outgoing path closed; local input restored");
        let delay = match result {
            Err(error) => {
                context.log(format!("outgoing path ended: {error}"));
                backoff.failed()
            }
            Ok(()) => {
                backoff.reset();
                Duration::from_millis(250)
            }
        };
        pause(delay, &mut stop).await;
    }
}

#[cfg(test)]
fn arm_outgoing(context: &Context) -> bool {
    context.rearm.lock().unwrap().allows_arming() && context.gate.arm()
}

async fn rearm_observer(context: &Context, mut stop: watch::Receiver<bool>) {
    let mut backoff = Backoff::default();
    let mut reported_error = None;
    while !context.stop.load(Ordering::Acquire) {
        if context.rearm.lock().unwrap().allows_arming()
            || !matches!(context.gate.state(), Focus::Local | Focus::Armed)
        {
            pause(Duration::from_millis(100), &mut stop).await;
            continue;
        }
        let result = tokio::select! {
            _ = cancelled(&mut stop) => return,
            position = linux_wayland::local_pointer_position() => position,
        };
        match result {
            Ok(position) => {
                backoff.reset();
                reported_error = None;
                if matches!(context.gate.state(), Focus::Local | Focus::Armed)
                    && context.rearm.lock().unwrap().observe_local(position)
                {
                    context.log("inward movement observed; emergency rearm guard cleared");
                }
                pause(Duration::from_millis(100), &mut stop).await;
            }
            Err(error) => {
                let message = error.to_string();
                if reported_error.as_ref() != Some(&message) {
                    eprintln!(
                        "Cannot observe inward movement: {message}; launching edge stays disarmed."
                    );
                    reported_error = Some(message);
                }
                pause(backoff.failed(), &mut stop).await;
            }
        }
    }
}

#[cfg(test)]
async fn prepare(context: &Context, link: &Link) -> Result<transport::Sender, String> {
    prepare_for(context, link, false).await
}
async fn prepare_for(
    context: &Context,
    link: &Link,
    routed: bool,
) -> Result<transport::Sender, String> {
    let store = TrustStore::load(&context.trust_dir)?;
    let peer = store.find_id(&link.peer_id).ok_or("layout peer removed")?;
    if !peer.permissions.connect {
        return Err("outbound connection denied by local policy".into());
    }
    let mut last_error = "no connection address available".to_string();
    for address in addresses(context, peer) {
        context.log(format!(
            "peer {} {:?} direction: connecting to endpoint hint {address}",
            peer.id, link.edge
        ));
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
                if let Some(node) = context
                    .config
                    .layout
                    .topology()
                    .iter()
                    .find(|node| node.peer_id == link.peer_id)
                {
                    if *context.active.lock().unwrap() != Some(node.coordinate()) {
                        context.availability(node, PeerAvailability::Authenticated);
                    }
                }
                context.log(format!("pinned identity verified: {}", peer.id));
                // Keep discovery addresses ephemeral. Subsequent attempts verify
                // the pin again; no trust-store edit races or auto-trust.
                return if routed {
                    transport::start_topology_sender(connection).await
                } else {
                    transport::start_remote_sender(connection, protocol_edge(link.entry_edge)).await
                };
            }
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

mod topology_capture;
use topology_capture::capture_topology;

#[cfg(test)]
#[path = "wayland_tests.rs"]
mod tests;
