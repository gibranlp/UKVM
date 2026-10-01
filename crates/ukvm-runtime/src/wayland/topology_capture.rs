//! A physical source keeps capture while focus traverses authenticated nodes.
use super::*;
use ukvm_protocol::Edge as Direction;
const DIRECTIONS: [Direction; 4] = [
    Direction::Left,
    Direction::Right,
    Direction::Top,
    Direction::Bottom,
];

struct Active {
    coordinate: Coordinate,
    sender: transport::Sender,
    permitted_exits: Vec<Direction>,
    entry_edge: Direction,
}
#[derive(Default)]
struct Path {
    active: Option<Active>,
    launch: Option<Direction>,
    returned_local: Option<EdgePosition>,
    failure: Option<String>,
    held_keys: std::collections::BTreeSet<u32>,
    held_buttons: std::collections::BTreeSet<u16>,
}
fn direction(edge: linux_wayland::Edge) -> Direction {
    match edge {
        linux_wayland::Edge::Left => Direction::Left,
        linux_wayland::Edge::Right => Direction::Right,
        linux_wayland::Edge::Top => Direction::Top,
        linux_wayland::Edge::Bottom => Direction::Bottom,
    }
}
fn ready(context: &Context, coordinate: Coordinate) -> bool {
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
fn exits(context: &Context, coordinate: Coordinate) -> Vec<Direction> {
    DIRECTIONS
        .into_iter()
        .filter(|edge| {
            let next = coordinate.adjacent(*edge);
            next == Coordinate::LOCAL || ready(context, next)
        })
        .collect()
}
fn take_ready(context: &Context, coordinate: Coordinate) -> Option<transport::Sender> {
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
impl Path {
    fn enter(
        &mut self,
        context: &Context,
        coordinate: Coordinate,
        entry: EdgePosition,
    ) -> Result<(), String> {
        let sender = take_ready(context, coordinate).ok_or("adjacent node is no longer ready")?;
        let mut permitted_exits = exits(context, coordinate);
        if !permitted_exits.contains(&entry.edge) {
            permitted_exits.push(entry.edge);
        }
        // Entry edge always supports the established natural return. A route
        // through it still requires a ready adjacent node at the source.
        sender
            .control
            .try_send(WireMessage::TopologyEntry {
                entry,
                exits: permitted_exits.clone(),
            })
            .map_err(|_| "topology entry queue unavailable")?;
        for code in &self.held_keys {
            sender
                .queue
                .try_send(InputEvent::Key {
                    code: ukvm_protocol::KeyCode::LinuxEvdev(*code),
                    pressed: true,
                })
                .map_err(|_| "held key handoff queue failed")?;
        }
        for button in &self.held_buttons {
            sender
                .queue
                .try_send(InputEvent::PointerButton {
                    button: *button,
                    pressed: true,
                })
                .map_err(|_| "held button handoff queue failed")?;
        }
        *context.active.lock().unwrap() = Some(coordinate);
        if let Some(node) = context
            .config
            .layout
            .topology()
            .iter()
            .find(|node| node.coordinate() == coordinate)
        {
            context.availability(node, PeerAvailability::Active);
        }
        context.log(format!(
            "focus -> {} via {:?} entry",
            coordinate, entry.edge
        ));
        self.active = Some(Active {
            coordinate,
            sender,
            permitted_exits,
            entry_edge: entry.edge,
        });
        Ok(())
    }
    fn refresh_exits(&mut self, context: &Context) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        if active.sender.returned.borrow().is_some() {
            return;
        }
        let mut updated = exits(context, active.coordinate);
        if !updated.contains(&active.entry_edge) {
            updated.push(active.entry_edge);
        }
        if updated != active.permitted_exits {
            if active
                .sender
                .control
                .try_send(WireMessage::TopologyExits(updated.clone()))
                .is_err()
            {
                self.failure = Some("topology exit refresh failed".into());
            } else {
                self.active.as_mut().unwrap().permitted_exits = updated;
            }
        }
    }
    fn poll_return(&mut self, context: &Context) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let returned = *active.sender.returned.borrow();
        let Some(position) = returned else {
            return;
        };
        // A peer can report a boundary, never pick a destination or identity.
        // Validate it against the source's current selected adjacency.
        if !active.permitted_exits.contains(&position.edge) {
            self.failure = Some("peer reported an unadvertised topology exit".into());
            return;
        }
        let destination = active.coordinate.adjacent(position.edge);
        let previous = active.coordinate;
        if let Some(node) = context
            .config
            .layout
            .topology()
            .iter()
            .find(|node| node.coordinate() == previous)
        {
            context.availability(
                node,
                if ready(context, previous) {
                    PeerAvailability::Ready
                } else {
                    PeerAvailability::Dormant
                },
            );
        }
        // ReturnControl is sent only AFTER remote held-state cleanup. Dropping
        // its finished sender owns/cancels both halves before next entry.
        drop(self.active.take());
        *context.active.lock().unwrap() = None;
        context.log(format!(
            "ReturnControl {:?}: {} -> {}",
            position.edge, previous, destination
        ));
        if destination == Coordinate::LOCAL {
            self.returned_local = Some(EdgePosition {
                edge: self.launch.unwrap_or(position.edge).opposite(),
                fraction: position.fraction,
            });
        } else if let Err(error) = self.enter(
            context,
            destination,
            EdgePosition {
                edge: position.edge.opposite(),
                fraction: position.fraction,
            },
        ) {
            self.failure = Some(error);
        }
    }
    fn track(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Key {
                code: ukvm_protocol::KeyCode::LinuxEvdev(code),
                pressed,
            } => {
                if *pressed {
                    self.held_keys.insert(*code);
                } else {
                    self.held_keys.remove(code);
                }
            }
            InputEvent::PointerButton { button, pressed } => {
                if *pressed {
                    self.held_buttons.insert(*button);
                } else {
                    self.held_buttons.remove(button);
                }
            }
            _ => {}
        }
    }
    fn emergency(&mut self, context: &Context, boundary: Option<linux_wayland::OuterSegment>) {
        context.rearm.lock().unwrap().suppress(boundary);
        if let Some(active) = self.active.as_ref() {
            let _ = active.sender.control.try_send(WireMessage::SessionEnd);
        }
        context.log("emergency chord detected; launching edge disarmed until inward movement");
        eprintln!("Emergency release: launching edge stays disarmed until 32 logical pixels inward movement.");
    }
    fn health(&self, context: &Context, initial_edges: &[linux_wayland::Edge]) -> bool {
        if context.stop.load(Ordering::Acquire) || self.failure.is_some() {
            return false;
        }
        if let Some(active) = self.active.as_ref() {
            if context.gate.state() != Focus::Controlling || !*active.sender.alive.borrow() {
                return false;
            }
            let allowed = TrustStore::load(&context.trust_dir)
                .ok()
                .is_some_and(|store| {
                    context
                        .config
                        .layout
                        .topology()
                        .iter()
                        .find(|node| node.coordinate() == active.coordinate)
                        .and_then(|node| store.find_id(&node.peer_id))
                        .is_some_and(|peer| peer.permissions.connect)
                });
            return allowed;
        }
        context.gate.state() == Focus::Armed && allowed_edges(context) == initial_edges
    }
}

pub(super) async fn capture_topology(
    context: &Context,
    edges: &[linux_wayland::Edge],
) -> Result<(), String> {
    let path = Mutex::new(Path::default());
    let mut notice = |notice| {
        match notice {
        CaptureNotice::Activated {
            edge,
            edge_fraction,
            activation_id,
            barrier_id,
        } => {
            context.log(format!("InputCapture Activated: activation_id={activation_id:?}, barrier_id={barrier_id:?}, direction={edge:?}, fraction={edge_fraction:.4}"));
            let mut path = path.lock().unwrap();
            let Some(edge) = edge else {
                path.failure = Some("activation lacks an identifiable launching edge".into());
                return;
            };
            if !edges.contains(&edge) || !context.gate.activate() {
                path.failure = Some("edge activation lost focus/readiness".into());
                return;
            }
            let launch = direction(edge);
            path.launch = Some(launch);
            if let Err(error) = path.enter(
                context,
                Coordinate::LOCAL.adjacent(launch),
                EdgePosition {
                    edge: launch.opposite(),
                    fraction: edge_fraction,
                },
            ) {
                path.failure = Some(error);
            }
        }
        CaptureNotice::EmergencyRelease { boundary } => {
            path.lock().unwrap().emergency(context, boundary)
        }
        CaptureNotice::Zones(zones) => {
            context.log(format!(
                "portal zone set {}: {} logical output rectangles",
                zones.id,
                zones.zones.len()
            ));
            for (index, zone) in zones.zones.iter().enumerate() {
                context.log(format!("zone {index}: origin ({},{}) size {}x{} logical units (portal coordinate space)", zone.x, zone.y, zone.width, zone.height));
            }
        }
        CaptureNotice::BarriersRequested(barriers) => {
            for barrier in barriers {
                context.log(format!(
                    "external {:?} barrier {} zone {}: {:?} -> {:?}",
                    barrier.segment.edge,
                    barrier.id,
                    barrier.segment.zone,
                    barrier.segment.start,
                    barrier.segment.end
                ));
            }
        }
        CaptureNotice::BarrierDecision(decision) => context.log(format!(
            "barriers accepted {:?}; rejected {:?}",
            decision.accepted, decision.rejected
        )),
        CaptureNotice::Lifecycle { phase, detail } => {
            *context.capture_status.lock().unwrap() = format!("{phase:?}: {detail}");
            context.log(format!("InputCapture {phase:?}: {detail}"));
        },
        CaptureNotice::SessionCreated { version, keyboard_granted } => context.log(format!("InputCapture session created: portal v{version}, keyboard_granted={keyboard_granted}")),
        CaptureNotice::EisSocketAcquired => context.log("InputCapture EIS socket acquired"),
        CaptureNotice::EisContextConnected => context.log("InputCapture EI context connected (receiver)"),
        CaptureNotice::EisHandshakeComplete => context.log("InputCapture EI handshake complete"),
        CaptureNotice::SeatAvailable => context.log("InputCapture EI seat announced; capture capabilities bound and flushed"),
        CaptureNotice::DeviceAvailable { pointer, keyboard, button, scroll } => context.log(format!("InputCapture EI device announced: pointer={pointer}, keyboard={keyboard}, button={button}, scroll={scroll}")),
        CaptureNotice::Deactivated { activation_id } => context.log(format!("InputCapture Deactivated: activation_id={activation_id:?}")),
        CaptureNotice::ZonesChanged { zone_set } => context.log(format!("InputCapture ZonesChanged: invalidated zone_set={zone_set:?}; rebuilding capture")),
        CaptureNotice::EisStarted { sequence } => context.log(format!("InputCapture EI start_emulating: sequence={sequence}")),
        CaptureNotice::EisStopped => context.log("InputCapture EI stop_emulating"),
        CaptureNotice::Enabled => context.log(format!("InputCapture edges enabled: {edges:?}")),
        CaptureNotice::ReleaseRequested { activation_id, cursor_position } => context.log(format!("InputCapture Release requested: activation_id={activation_id:?}, cursor_position={cursor_position:?}")),
        CaptureNotice::ReleaseResult(result) => context.log(format!("InputCapture Release result: {result:?}")),
        CaptureNotice::Closed(result) => context.log(format!("InputCapture closed: {result:?}")),
        _ => {}
    }
    };
    let mut event = |event| {
        let mut path = path.lock().unwrap();
        path.poll_return(context);
        if path.failure.is_some() || path.returned_local.is_some() {
            return Ok(());
        }
        path.track(&event);
        let active = path.active.as_ref().ok_or("no active focus recipient")?;
        match active.sender.queue.try_send(event) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(InputEvent::PointerMotion {
                ..
            })) => Ok(()),
            Err(_) => Err("stateful input queue failed; releasing capture".into()),
        }
    };
    let mut healthy = || path.lock().unwrap().health(context, edges);
    let mut return_request = || {
        let mut path = path.lock().unwrap();
        path.poll_return(context);
        path.refresh_exits(context);
        path.returned_local
    };
    let result = linux_wayland::capture_edges_with_events_mode(
        edges,
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
    .await;
    let mut path = path.into_inner().unwrap();
    let finished = if let Some(active) = path.active.take() {
        active.sender.finish().await.map(|_| ())
    } else {
        Ok(())
    };
    result
        .map(|_| ())
        .and(finished)
        .and(path.failure.map_or(Ok(()), Err))
}

#[cfg(test)]
mod tests;
