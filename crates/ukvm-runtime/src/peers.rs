//! Shared pinned-peer discovery hints, independent readiness and source-owned adjacency.
use crate::{
    lifecycle::{cancelled, pause},
    session::Backoff,
};
use futures_util::{stream::FuturesUnordered, StreamExt};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};
use tokio::sync::watch;
use ukvm_config::{Config, Link, TopologyNode};
use ukvm_core::Coordinate;
use ukvm_discovery::{Advertisement, DiscoveryResult, DiscoveryService};
use ukvm_network::{
    identity::Identity,
    transport,
    trust::{cert_path, resolve_address, TrustStore},
};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PeerAvailability {
    Dormant,
    Connecting,
    Authenticated,
    Ready,
    Active,
    Unavailable,
    Stopped,
}

pub(crate) struct PeerRuntime {
    pub(crate) config: Config,
    pub(crate) identity: Identity,
    pub(crate) trust_dir: PathBuf,
    pub(crate) stop: AtomicBool,
    pub(crate) verbose: bool,
    pub(crate) hints: Mutex<Vec<DiscoveryResult>>,
    pub(crate) availability: Mutex<HashMap<Coordinate, PeerAvailability>>,
    pub(crate) pool: Mutex<HashMap<Coordinate, transport::Sender>>,
    pub(crate) active: Mutex<Option<Coordinate>>,
}
impl PeerRuntime {
    pub(crate) fn new(
        config: Config,
        identity: Identity,
        trust_dir: PathBuf,
        verbose: bool,
    ) -> Self {
        Self {
            config,
            identity,
            trust_dir,
            verbose,
            stop: AtomicBool::new(false),
            hints: Mutex::new(Vec::new()),
            availability: Mutex::new(HashMap::new()),
            pool: Mutex::new(HashMap::new()),
            active: Mutex::new(None),
        }
    }
    fn stopping(&self, stop: &watch::Receiver<bool>) -> bool {
        self.stop.load(Ordering::Acquire) || *stop.borrow() || stop.has_changed().is_err()
    }

    pub(crate) fn availability(&self, node: &TopologyNode, next: PeerAvailability) {
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
    pub(crate) fn log(&self, message: impl std::fmt::Display) {
        if self.verbose {
            eprintln!("[runtime] {message}");
        }
    }
}
pub(crate) async fn discovery(context: &PeerRuntime, port: u16, mut stop: watch::Receiver<bool>) {
    let mut backoff = Backoff::default();
    while !context.stopping(&stop) {
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
            if context.stopping(&stop) {
                break;
            }
        }
        drop(guard);
        context.hints.lock().unwrap().clear();
        pause(backoff.failed(), &mut stop).await;
    }
}

pub(crate) fn protocol_edge(edge: ukvm_config::Edge) -> ukvm_protocol::Edge {
    match edge {
        ukvm_config::Edge::Left => ukvm_protocol::Edge::Left,
        ukvm_config::Edge::Right => ukvm_protocol::Edge::Right,
        ukvm_config::Edge::Top => ukvm_protocol::Edge::Top,
        ukvm_config::Edge::Bottom => ukvm_protocol::Edge::Bottom,
    }
}
pub(crate) fn addresses(context: &PeerRuntime, peer: &ukvm_network::trust::Peer) -> Vec<String> {
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
pub(crate) fn node_edge(node: &TopologyNode) -> ukvm_config::Edge {
    match (node.x.signum(), node.y.signum()) {
        (-1, _) => ukvm_config::Edge::Left,
        (1, _) => ukvm_config::Edge::Right,
        (_, -1) => ukvm_config::Edge::Top,
        _ => ukvm_config::Edge::Bottom,
    }
}
pub(crate) fn link_for(node: &TopologyNode) -> Link {
    let edge = node_edge(node);
    Link {
        edge,
        entry_edge: edge.opposite(),
        peer_id: node.peer_id.clone(),
    }
}

pub(crate) async fn peers(context: &PeerRuntime, stop: watch::Receiver<bool>) {
    let nodes = context.config.layout.topology();
    let mut workers = FuturesUnordered::new();
    for node in &nodes {
        workers.push(peer_worker(context, node, stop.clone()));
    }
    while workers.next().await.is_some() {}
}
async fn peer_worker(context: &PeerRuntime, node: &TopologyNode, mut stop: watch::Receiver<bool>) {
    let coordinate = node.coordinate();
    let mut backoff = Backoff::default();
    while !context.stopping(&stop) {
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

#[cfg(test)]
pub(crate) async fn prepare(
    context: &PeerRuntime,
    link: &Link,
) -> Result<transport::Sender, String> {
    prepare_for(context, link, false).await
}
pub(crate) async fn prepare_for(
    context: &PeerRuntime,
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

const DIRECTIONS: [ukvm_protocol::Edge; 4] = [
    ukvm_protocol::Edge::Left,
    ukvm_protocol::Edge::Right,
    ukvm_protocol::Edge::Top,
    ukvm_protocol::Edge::Bottom,
];
pub(crate) fn ready(context: &PeerRuntime, coordinate: Coordinate) -> bool {
    let allowed = TrustStore::load(&context.trust_dir)
        .ok()
        .is_some_and(|store| {
            context
                .config
                .layout
                .topology()
                .iter()
                .find(|node| node.coordinate() == coordinate)
                .and_then(|node| store.find_id(&node.peer_id))
                .is_some_and(|peer| peer.permissions.connect)
        });
    allowed
        && context
            .pool
            .lock()
            .unwrap()
            .get(&coordinate)
            .is_some_and(|sender| *sender.alive.borrow())
}
pub(crate) fn exits(context: &PeerRuntime, coordinate: Coordinate) -> Vec<ukvm_protocol::Edge> {
    DIRECTIONS
        .into_iter()
        .filter(|edge| {
            let next = coordinate.adjacent(*edge);
            next == Coordinate::LOCAL || ready(context, next)
        })
        .collect()
}
pub(crate) fn take_ready(
    context: &PeerRuntime,
    coordinate: Coordinate,
) -> Option<transport::Sender> {
    if !ready(context, coordinate) {
        return None;
    }
    let sender = context.pool.lock().unwrap().remove(&coordinate)?;
    if *sender.alive.borrow() {
        Some(sender)
    } else {
        drop(sender);
        None
    }
}

#[cfg(test)]
mod tests;
