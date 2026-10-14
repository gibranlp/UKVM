use super::*;
use crate::peers::{
    addresses, discovery, exits, node_edge, peers, ready, take_ready, PeerAvailability, PeerRuntime,
};
#[cfg(test)]
use crate::peers::{link_for, prepare, prepare_for};
use futures_util::{stream::FuturesUnordered, StreamExt};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::{path::PathBuf, sync::atomic::Ordering};
use tokio::sync::watch;
#[cfg(test)]
use ukvm_config::{Config, Link};
use ukvm_core::Coordinate;
#[cfg(test)]
use ukvm_discovery::DiscoveryResult;
use ukvm_network::{
    identity::{default_state_dir, Identity},
    transport::{self, ReceiverNotice},
    trust::{cert_path, default_trust_dir, TrustStore},
};
use ukvm_platform::{
    linux_wayland::{
        self, CaptureNotice, CaptureTiming, EmergencyRearmGuard, ReleaseMode, WaylandInputInjector,
    },
    RemoteInputSink,
};
use ukvm_protocol::WireMessage;

pub(crate) mod clipboard;
mod files;
mod remote;

struct Context {
    peers: PeerRuntime,
    clipboard: clipboard::Clipboard,
    gate: FocusGate,
    rearm: Mutex<EmergencyRearmGuard>,
    incoming_peer: Mutex<Option<String>>,
    capture_status: Mutex<String>,
    receiver_status: Arc<Mutex<String>>,
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
        peers: PeerRuntime::new(config, identity, trust_dir, verbose),
        gate: FocusGate::default(),
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
        context.peers.config.node.name,
        listener.local_addr().map_err(|e| e.to_string())?
    );
    // Structured concurrency: no detached runtime worker or cancellation of an
    // active capture/receiver future. Each worker executes its own cleanup.
    let signal = async {
        let result = interrupt.await;
        context.peers.stop.store(true, Ordering::Release);
        let _ = stop_tx.send(true);
        result.map_err(|e| e.to_string())
    };
    let (signal_result, (), (), (), (), (), (), (), ()) = tokio::join!(
        signal,
        receiver(&context, listener, stop.clone()),
        controller(&context, stop.clone()),
        discovery(&context.peers, port, stop.clone()),
        peers(&context.peers, stop.clone()),
        rearm_observer(&context, stop.clone()),
        clipboard::supervise(&context, stop.clone()),
        remote::bridge(&context, stop.clone()),
        publish_status(&context, owner, stop),
    );
    for node in context.peers.config.layout.topology() {
        context.peers.availability(&node, PeerAvailability::Stopped);
    }
    context
        .peers
        .log("all sessions closed; local input restored");
    println!("UKVM stopped.");
    signal_result
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
                let active = *context.peers.active.lock().unwrap();
                let topology = context.peers.config.layout.topology();
                let states = context.peers.availability.lock().unwrap();
                let snapshot = ownership::RuntimeSnapshot {
                    file_protocol_version: Some(ukvm_protocol::files::VERSION),
                    display_protocol_version: Some(ukvm_protocol::display::VERSION),
                    clipboard: context.clipboard.snapshot(),
                    name: context.peers.config.node.name.clone(),
                    node_id: context.peers.identity.id.clone(),
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
                if let Err(error) = owner.publish(snapshot) { context.peers.log(format!("status snapshot unavailable: {error}")); }
            }
        }
    }
}

mod incoming;
use incoming::receiver;

fn capture_edge(edge: ukvm_config::Edge) -> linux_wayland::Edge {
    match edge {
        ukvm_config::Edge::Left => linux_wayland::Edge::Left,
        ukvm_config::Edge::Right => linux_wayland::Edge::Right,
        ukvm_config::Edge::Top => linux_wayland::Edge::Top,
        ukvm_config::Edge::Bottom => linux_wayland::Edge::Bottom,
    }
}

fn allowed_edges(context: &Context) -> Vec<linux_wayland::Edge> {
    let guard = *context.rearm.lock().unwrap();
    let pool = context.peers.pool.lock().unwrap();
    let store = TrustStore::load(&context.peers.trust_dir).ok();
    context.peers.config.layout.topology().iter().filter(|node| {
        store.as_ref().and_then(|store| store.find_id(&node.peer_id)).is_some_and(|peer| peer.permissions.connect)
        && node.x.abs() + node.y.abs() == 1 && pool.get(&node.coordinate()).is_some_and(|sender| *sender.alive.borrow())
        && !matches!(guard, EmergencyRearmGuard::AwaitingInward { boundary: Some(boundary) } if boundary.edge == capture_edge(node_edge(node)))
        && !matches!(guard, EmergencyRearmGuard::AwaitingInward { boundary: None })
    }).map(|node| capture_edge(node_edge(node))).collect()
}

async fn controller(context: &Context, mut stop: watch::Receiver<bool>) {
    let mut backoff = Backoff::default();
    while !context.peers.stop.load(Ordering::Acquire) {
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
        let previous = context.peers.active.lock().unwrap().take();
        if let Some(coordinate) = previous {
            if let Some(node) = context
                .peers
                .config
                .layout
                .topology()
                .iter()
                .find(|node| node.coordinate() == coordinate)
            {
                let ready = context
                    .peers
                    .pool
                    .lock()
                    .unwrap()
                    .get(&coordinate)
                    .is_some_and(|sender| *sender.alive.borrow());
                context.peers.availability(
                    node,
                    if ready {
                        PeerAvailability::Ready
                    } else {
                        PeerAvailability::Unavailable
                    },
                );
            }
        }
        context
            .peers
            .log("outgoing path closed; local input restored");
        let delay = match result {
            Err(error) => {
                context.peers.log(format!("outgoing path ended: {error}"));
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
    while !context.peers.stop.load(Ordering::Acquire) {
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
                    context
                        .peers
                        .log("inward movement observed; emergency rearm guard cleared");
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

mod topology_capture;
use topology_capture::capture_topology;

#[cfg(test)]
#[path = "wayland_tests.rs"]
mod tests;
