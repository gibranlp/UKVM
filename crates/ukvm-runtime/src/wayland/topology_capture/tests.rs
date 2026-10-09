use super::*;
use ukvm_network::trust::SessionPermission;
use ukvm_protocol::KeyCode;

#[derive(Default)]
struct Evidence {
    held_key: bool,
    held_button: bool,
    parks: usize,
    entries: usize,
    exits: Vec<Direction>,
}
struct Sink(Arc<Mutex<Evidence>>);
impl InputSink for Sink {
    fn kind(&self) -> SinkKind {
        SinkKind::RemoteInput
    }
    fn begin_session(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn enter_remote(&mut self, entry: EdgePosition) -> Result<(), String> {
        self.enter_topology(entry, vec![entry.edge])
    }
    fn enter_topology(&mut self, _: EdgePosition, exits: Vec<Direction>) -> Result<(), String> {
        let mut evidence = self.0.lock().unwrap();
        evidence.entries += 1;
        evidence.exits = exits;
        Ok(())
    }
    fn update_exits(&mut self, exits: Vec<Direction>) -> Result<(), String> {
        self.0.lock().unwrap().exits = exits;
        Ok(())
    }
    fn handle_event(&mut self, event: &InputEvent) -> Result<SinkAction, String> {
        let mut evidence = self.0.lock().unwrap();
        match event {
            InputEvent::Key { pressed, .. } => evidence.held_key = *pressed,
            InputEvent::PointerButton { pressed, .. } => evidence.held_button = *pressed,
            InputEvent::PointerMotion { dx, dy } => {
                let edge = if *dx < 0.0 {
                    Direction::Left
                } else if *dx > 0.0 {
                    Direction::Right
                } else if *dy < 0.0 {
                    Direction::Top
                } else {
                    Direction::Bottom
                };
                if evidence.exits.contains(&edge) {
                    return Ok(SinkAction::Return(EdgePosition {
                        edge,
                        fraction: 0.5,
                    }));
                }
            }
            _ => {}
        }
        Ok(SinkAction::Continue)
    }
    fn end_session(&mut self) -> Result<(), String> {
        let mut evidence = self.0.lock().unwrap();
        evidence.held_key = false;
        evidence.held_button = false;
        evidence.parks += 1;
        Ok(())
    }
}
struct Remote {
    evidence: Arc<Mutex<Evidence>>,
    task: tokio::task::JoinHandle<()>,
    stop: watch::Sender<bool>,
}
struct Fixture {
    _temp: tempfile::TempDir,
    context: Context,
    remotes: Vec<Remote>,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(&temp.path().join("source")).unwrap();
        let trust = temp.path().join("source-trust");
        let mut store = TrustStore::default();
        let mut config = Config::default();
        let mut remotes = Vec::new();
        for (i, (x, y)) in [(1, 0), (2, 0), (0, -1)].into_iter().enumerate() {
            let peer = Identity::load_or_create(&temp.path().join(format!("peer-{i}"))).unwrap();
            let peer_trust = temp.path().join(format!("trust-{i}"));
            let mut peer_store = TrustStore::default();
            peer_store
                .import(
                    &peer_trust,
                    "source".into(),
                    None,
                    &std::fs::read(&identity.cert_path).unwrap(),
                )
                .unwrap();
            peer_store
                .set_permission(
                    &peer_trust,
                    "source",
                    SessionPermission::ControlThisNode,
                    true,
                )
                .unwrap();
            let listener = transport::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            store
                .import(
                    &trust,
                    format!("peer-{i}"),
                    Some(address.to_string()),
                    &std::fs::read(&peer.cert_path).unwrap(),
                )
                .unwrap();
            config.set_position(x, y, peer.id.clone()).unwrap();
            let evidence = Arc::new(Mutex::new(Evidence::default()));
            let (stop, mut stopped) = watch::channel(false);
            let shared = Arc::new(Mutex::new(Some(Sink(Arc::clone(&evidence)))));
            let gate = FocusGate::default();
            let server_peer = peer.clone();
            let server_store = peer_store.clone();
            let server_trust = peer_trust.clone();
            let task = tokio::spawn(async move {
                let mut connections = FuturesUnordered::new();
                loop {
                    tokio::select! {
                        _ = cancelled(&mut stopped) => break,
                        _ = connections.next(), if !connections.is_empty() => {},
                        accepted = listener.accept() => {
                            let (tcp,_) = accepted.unwrap();
                            let mut sink = ExclusiveSink { inner: SharedSession { shared: Arc::clone(&shared), entered: false }, gate: gate.clone(), reserved: false };
                            let peer = server_peer.clone(); let store = server_store.clone(); let trust = server_trust.clone(); let mut stop = stopped.clone();
                            connections.push(async move {
                                let connection = transport::accept_remote_trusted(tcp,&peer,&store,&trust,"peer",&sink).await.unwrap();
                                let _ = transport::receive_session_until(connection,&mut sink, |_| {}, cancelled(&mut stop)).await;
                            });
                        }
                    }
                }
                while connections.next().await.is_some() {}
            });
            remotes.push(Remote {
                evidence,
                task,
                stop,
            });
        }
        let context = Context {
            clipboard: clipboard::Clipboard::new(&identity.id).unwrap(),
            config,
            identity,
            trust_dir: trust,
            gate: FocusGate::default(),
            stop: AtomicBool::new(false),
            verbose: false,
            hints: Mutex::new(Vec::new()),
            availability: Mutex::new(HashMap::new()),
            pool: Mutex::new(HashMap::new()),
            active: Mutex::new(None),
            rearm: Mutex::new(EmergencyRearmGuard::Ready),
            incoming_peer: Mutex::new(None),
            capture_status: Mutex::new("not requested".into()),
            receiver_status: Arc::new(Mutex::new("consent not requested".into())),
        };
        Self {
            _temp: temp,
            context,
            remotes,
        }
    }
    async fn prepare(&self, coordinate: Coordinate) {
        let node = self
            .context
            .config
            .layout
            .topology()
            .into_iter()
            .find(|node| node.coordinate() == coordinate)
            .unwrap();
        let sender = prepare_for(&self.context, &link_for(&node), true)
            .await
            .unwrap();
        self.context.pool.lock().unwrap().insert(coordinate, sender);
    }
    async fn finish(self, mut path: Path) {
        if let Some(active) = path.active.take() {
            let _ = active.sender.finish().await;
        }
        let senders = std::mem::take(&mut *self.context.pool.lock().unwrap());
        for sender in senders.into_values() {
            let _ = sender.finish().await;
        }
        for remote in self.remotes {
            let _ = remote.stop.send(true);
            tokio::time::timeout(Duration::from_secs(2), remote.task)
                .await
                .unwrap()
                .unwrap();
        }
    }
}
async fn returned(path: &mut Path, context: &Context, dx: f64, dy: f64) {
    let active = path.active.as_mut().unwrap();
    active
        .sender
        .queue
        .send(InputEvent::PointerMotion { dx, dy })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), active.sender.returned.changed())
        .await
        .unwrap()
        .unwrap();
    path.poll_return(context);
}
fn entry() -> EdgePosition {
    EdgePosition {
        edge: Direction::Left,
        fraction: 0.5,
    }
}
const RIGHT: Coordinate = Coordinate { x: 1, y: 0 };
const FAR: Coordinate = Coordinate { x: 2, y: 0 };
const TOP: Coordinate = Coordinate { x: 0, y: -1 };

#[tokio::test]
async fn two_level_chain_returns_through_intermediate_and_preserves_exclusive_focus() {
    let fixture = Fixture::new().await;
    fixture.prepare(RIGHT).await;
    fixture.prepare(FAR).await;
    fixture.prepare(TOP).await;
    assert_eq!(
        allowed_edges(&fixture.context),
        vec![linux_wayland::Edge::Right, linux_wayland::Edge::Top]
    );
    assert!(fixture.context.gate.arm());
    assert!(fixture.context.gate.activate());
    let mut path = Path {
        launch: Some(Direction::Right),
        ..Default::default()
    };
    path.enter(&fixture.context, RIGHT, entry()).unwrap();
    assert!(fixture.context.gate.reserve_inbound().is_err());
    let key = InputEvent::Key {
        code: KeyCode::LinuxEvdev(42),
        pressed: true,
    };
    let button = InputEvent::PointerButton {
        button: 272,
        pressed: true,
    };
    path.track(&key);
    path.track(&button);
    path.active
        .as_ref()
        .unwrap()
        .sender
        .queue
        .send(key)
        .await
        .unwrap();
    path.active
        .as_ref()
        .unwrap()
        .sender
        .queue
        .send(button)
        .await
        .unwrap();
    returned(&mut path, &fixture.context, 100.0, 0.0).await;
    assert_eq!(path.active.as_ref().unwrap().coordinate, FAR);
    assert!(!fixture.remotes[0].evidence.lock().unwrap().held_key);
    assert!(!fixture.remotes[0].evidence.lock().unwrap().held_button);
    fixture.prepare(RIGHT).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !(fixture.remotes[1].evidence.lock().unwrap().held_key
            && fixture.remotes[1].evidence.lock().unwrap().held_button)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    returned(&mut path, &fixture.context, -100.0, 0.0).await;
    assert_eq!(path.active.as_ref().unwrap().coordinate, RIGHT);
    assert_eq!(fixture.remotes[1].evidence.lock().unwrap().entries, 1);
    assert!(!fixture.remotes[1].evidence.lock().unwrap().held_key);
    returned(&mut path, &fixture.context, -100.0, 0.0).await;
    assert!(path.active.is_none());
    assert_eq!(path.returned_local, Some(entry()));
    fixture.context.gate.outbound_closed();
    assert!(fixture.context.rearm.lock().unwrap().allows_arming());
    assert!(fixture.context.gate.arm());
    assert_eq!(fixture.remotes[0].evidence.lock().unwrap().entries, 2);
    fixture.finish(path).await;
}

#[tokio::test]
async fn unavailable_inner_node_never_arms_but_other_direction_and_recovery_work() {
    let fixture = Fixture::new().await;
    fixture.prepare(FAR).await;
    fixture.prepare(TOP).await;
    assert_eq!(
        allowed_edges(&fixture.context),
        vec![linux_wayland::Edge::Top]
    );
    let mut path = Path::default();
    assert!(path.enter(&fixture.context, RIGHT, entry()).is_err());
    assert!(path.active.is_none());
    fixture.prepare(RIGHT).await;
    assert_eq!(
        allowed_edges(&fixture.context),
        vec![linux_wayland::Edge::Right, linux_wayland::Edge::Top]
    );
    path.enter(&fixture.context, RIGHT, entry()).unwrap();
    assert!(path
        .active
        .as_ref()
        .unwrap()
        .permitted_exits
        .contains(&Direction::Right));
    fixture.finish(path).await;
}

#[tokio::test]
async fn emergency_from_second_node_cleans_held_state_and_only_suppresses_launching_edge() {
    let fixture = Fixture::new().await;
    fixture.prepare(RIGHT).await;
    fixture.prepare(FAR).await;
    fixture.prepare(TOP).await;
    let mut path = Path {
        launch: Some(Direction::Right),
        ..Default::default()
    };
    path.enter(&fixture.context, RIGHT, entry()).unwrap();
    returned(&mut path, &fixture.context, 100.0, 0.0).await;
    let button = InputEvent::PointerButton {
        button: 272,
        pressed: true,
    };
    path.track(&button);
    path.active
        .as_ref()
        .unwrap()
        .sender
        .queue
        .send(button)
        .await
        .unwrap();
    fixture.prepare(RIGHT).await;
    path.emergency(
        &fixture.context,
        Some(linux_wayland::OuterSegment {
            zone: 0,
            edge: linux_wayland::Edge::Right,
            start: (1000, 0),
            end: (1000, 999),
        }),
    );
    let active = path.active.take().unwrap();
    active.sender.finish().await.unwrap();
    fixture.context.gate.outbound_closed();
    *fixture.context.active.lock().unwrap() = None;
    assert_eq!(
        allowed_edges(&fixture.context),
        vec![linux_wayland::Edge::Top]
    );
    assert!(!fixture
        .context
        .rearm
        .lock()
        .unwrap()
        .observe_local(ukvm_platform::Point { x: 999.0, y: 500.0 }));
    assert!(fixture
        .context
        .rearm
        .lock()
        .unwrap()
        .observe_local(ukvm_platform::Point { x: 968.0, y: 500.0 }));
    assert_eq!(
        allowed_edges(&fixture.context),
        vec![linux_wayland::Edge::Right, linux_wayland::Edge::Top]
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.remotes[1].evidence.lock().unwrap().parks == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!fixture.remotes[1].evidence.lock().unwrap().held_button);
    fixture.finish(path).await;
}

#[tokio::test]
async fn active_disconnect_restores_source_and_preserves_other_ready_paths() {
    let fixture = Fixture::new().await;
    fixture.prepare(RIGHT).await;
    fixture.prepare(TOP).await;
    assert!(fixture.context.gate.arm());
    assert!(fixture.context.gate.activate());
    let mut path = Path {
        launch: Some(Direction::Right),
        ..Default::default()
    };
    path.enter(&fixture.context, RIGHT, entry()).unwrap();
    fixture.remotes[0].stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while *path.active.as_ref().unwrap().sender.alive.borrow() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!path.health(
        &fixture.context,
        &[linux_wayland::Edge::Right, linux_wayland::Edge::Top]
    ));
    fixture.context.gate.outbound_closed();
    assert_eq!(fixture.context.gate.state(), Focus::Local);
    assert_eq!(
        allowed_edges(&fixture.context),
        vec![linux_wayland::Edge::Top]
    );
    assert!(fixture.context.rearm.lock().unwrap().allows_arming());
    fixture.finish(path).await;
}

#[tokio::test]
async fn independent_workers_keep_other_paths_ready_and_prepare_return_standby() {
    let mut fixture = Fixture::new().await;
    let offline = Identity::load_or_create(&fixture._temp.path().join("offline")).unwrap();
    let mut store = TrustStore::load(&fixture.context.trust_dir).unwrap();
    store
        .import(
            &fixture.context.trust_dir,
            "offline".into(),
            Some("127.0.0.1:1".into()),
            &std::fs::read(&offline.cert_path).unwrap(),
        )
        .unwrap();
    fixture
        .context
        .config
        .set_position(-1, 0, offline.id)
        .unwrap();
    let (stop_tx, stop) = watch::channel(false);
    let context = &fixture.context;
    let run = async {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !ready(context, RIGHT) || !ready(context, FAR) || !ready(context, TOP) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!allowed_edges(context).contains(&linux_wayland::Edge::Left));
        assert!(context.gate.arm());
        assert!(context.gate.activate());
        let mut path = Path {
            launch: Some(Direction::Right),
            ..Default::default()
        };
        path.enter(context, RIGHT, entry()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !ready(context, RIGHT) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // A standby of the SAME pinned peer is idle, not another focus owner.
        assert_eq!(context.gate.state(), Focus::Controlling);
        returned(&mut path, context, 100.0, 0.0).await;
        returned(&mut path, context, -100.0, 0.0).await;
        assert_eq!(path.active.as_ref().unwrap().coordinate, RIGHT);
        returned(&mut path, context, -100.0, 0.0).await;
        assert!(path.returned_local.is_some());
        context.gate.outbound_closed();
        context.stop.store(true, Ordering::Release);
        stop_tx.send(true).unwrap();
        path
    };
    let ((), path) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(peers(context, stop), run)
    })
    .await
    .unwrap();
    assert!(context.pool.lock().unwrap().is_empty());
    assert!(context
        .availability
        .lock()
        .unwrap()
        .contains_key(&Coordinate { x: -1, y: 0 }));
    fixture.finish(path).await;
}

#[tokio::test]
async fn live_availability_updates_add_and_remove_exits_without_restarting_focus() {
    let fixture = Fixture::new().await;
    fixture.prepare(RIGHT).await;
    let mut path = Path {
        launch: Some(Direction::Right),
        ..Default::default()
    };
    path.enter(&fixture.context, RIGHT, entry()).unwrap();
    assert!(!path
        .active
        .as_ref()
        .unwrap()
        .permitted_exits
        .contains(&Direction::Right));
    fixture.prepare(FAR).await;
    path.refresh_exits(&fixture.context);
    assert!(path
        .active
        .as_ref()
        .unwrap()
        .permitted_exits
        .contains(&Direction::Right));
    let far = fixture.context.pool.lock().unwrap().remove(&FAR).unwrap();
    far.finish().await.unwrap();
    path.refresh_exits(&fixture.context);
    assert!(!path
        .active
        .as_ref()
        .unwrap()
        .permitted_exits
        .contains(&Direction::Right));
    assert_eq!(path.active.as_ref().unwrap().coordinate, RIGHT);
    fixture.finish(path).await;
}

#[tokio::test]
async fn out_of_order_refresh_and_duplicate_entry_fail_closed_and_cleanup_focus() {
    for duplicate in [false, true] {
        let fixture = Fixture::new().await;
        fixture.prepare(RIGHT).await;
        let mut path = Path::default();
        if duplicate {
            path.enter(&fixture.context, RIGHT, entry()).unwrap();
        } else {
            path.active = Some(Active {
                coordinate: RIGHT,
                sender: take_ready(&fixture.context, RIGHT).unwrap(),
                permitted_exits: Vec::new(),
                entry_edge: Direction::Left,
            });
        }
        let active = path.active.as_mut().unwrap();
        let message = if duplicate {
            WireMessage::TopologyEntry {
                entry: entry(),
                exits: vec![Direction::Left],
            }
        } else {
            WireMessage::TopologyExits(vec![Direction::Left])
        };
        active.sender.control.send(message).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while *active.sender.alive.borrow() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!fixture.remotes[0].evidence.lock().unwrap().held_key);
        assert!(!fixture.remotes[0].evidence.lock().unwrap().held_button);
        assert_eq!(
            fixture.remotes[0].evidence.lock().unwrap().entries,
            usize::from(duplicate)
        );
        fixture.finish(path).await;
    }
}

#[tokio::test]
async fn remote_cannot_select_an_unadvertised_direction_or_another_identity() {
    let fixture = Fixture::new().await;
    fixture.prepare(RIGHT).await;
    fixture.prepare(TOP).await;
    let mut path = Path {
        launch: Some(Direction::Right),
        ..Default::default()
    };
    path.enter(&fixture.context, RIGHT, entry()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.remotes[0].evidence.lock().unwrap().entries == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Emulate an authenticated but malicious receiver reporting an exit the
    // source never advertised at this coordinate.
    fixture.remotes[0]
        .evidence
        .lock()
        .unwrap()
        .exits
        .push(Direction::Top);
    returned(&mut path, &fixture.context, 0.0, -100.0).await;
    assert!(path.failure.as_ref().unwrap().contains("unadvertised"));
    assert_eq!(path.active.as_ref().unwrap().coordinate, RIGHT);
    assert!(ready(&fixture.context, TOP));
    fixture.finish(path).await;
}

#[tokio::test]
async fn excessive_exit_refresh_rate_ends_session_and_releases_held_input() {
    let fixture = Fixture::new().await;
    fixture.prepare(RIGHT).await;
    let mut path = Path::default();
    path.enter(&fixture.context, RIGHT, entry()).unwrap();
    let active = path.active.as_mut().unwrap();
    active
        .sender
        .queue
        .send(InputEvent::PointerButton {
            button: 272,
            pressed: true,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !fixture.remotes[0].evidence.lock().unwrap().held_button {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for _ in 0..60 {
        if active
            .sender
            .control
            .send(WireMessage::TopologyExits(vec![Direction::Left]))
            .await
            .is_err()
        {
            break;
        }
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while *active.sender.alive.borrow() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!fixture.remotes[0].evidence.lock().unwrap().held_button);
    fixture.finish(path).await;
}
