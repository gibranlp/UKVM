use super::*;
use std::sync::atomic::AtomicUsize;
use ukvm_network::trust::SessionPermission;

struct Sink(Arc<AtomicUsize>);
impl InputSink for Sink {
    fn kind(&self) -> SinkKind {
        SinkKind::RemoteInput
    }
    fn begin_session(&mut self) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn enter_remote(&mut self, _: EdgePosition) -> Result<(), String> {
        Ok(())
    }
    fn handle_event(&mut self, _: &InputEvent) -> Result<SinkAction, String> {
        Ok(SinkAction::Continue)
    }
    fn end_session(&mut self) -> Result<(), String> {
        Ok(())
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    context: Context,
    peer: Identity,
    peer_store: TrustStore,
    peer_trust: PathBuf,
    begins: Arc<AtomicUsize>,
}
impl Fixture {
    fn new(allowed: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let local = Identity::load_or_create(&temp.path().join("a")).unwrap();
        let peer = Identity::load_or_create(&temp.path().join("b")).unwrap();
        let trust = temp.path().join("a-trust");
        let peer_trust = temp.path().join("b-trust");
        let mut store = TrustStore::default();
        store
            .import(
                &trust,
                "peer".into(),
                Some("127.0.0.1:1".into()),
                &std::fs::read(&peer.cert_path).unwrap(),
            )
            .unwrap();
        let mut peer_store = TrustStore::default();
        peer_store
            .import(
                &peer_trust,
                "local".into(),
                None,
                &std::fs::read(&local.cert_path).unwrap(),
            )
            .unwrap();
        peer_store
            .set_permission(
                &peer_trust,
                "local",
                SessionPermission::ControlThisNode,
                allowed,
            )
            .unwrap();
        let context = Context {
            config: Config {
                node: ukvm_config::Node {
                    name: "local".into(),
                    role: ukvm_config::ConfigRole::Both,
                },
                network: Default::default(),
                layout: ukvm_config::Layout {
                    nodes: Vec::new(),
                    links: vec![Link {
                        edge: ukvm_config::Edge::Right,
                        peer_id: peer.id.clone(),
                        entry_edge: ukvm_config::Edge::Left,
                    }],
                },
            },
            identity: local,
            trust_dir: trust,
            gate: FocusGate::default(),
            stop: AtomicBool::new(false),
            verbose: false,
            hints: Mutex::new(Vec::new()),
            availability: Mutex::new(HashMap::new()),
            pool: Mutex::new(HashMap::new()),
            active: Mutex::new(None),
            rearm: Mutex::new(EmergencyRearmGuard::Ready),
        };
        Self {
            _temp: temp,
            context,
            peer,
            peer_store,
            peer_trust,
            begins: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn hint(&self, address: std::net::SocketAddr) {
        *self.context.hints.lock().unwrap() = vec![DiscoveryResult {
            service_instance: "test._ukvm._tcp.local.".into(),
            display_name: "spoofable label".into(),
            advertised_node_id: Some(self.peer.id.clone()),
            pairing_version: 2,
            pairing_available: false,
            input_version: Some((1, 2)),
            control_port: Some(address.port()),
            addresses: vec![address],
        }];
    }
    async fn serve(&self, identity: Identity) -> tokio::task::JoinHandle<Result<(), String>> {
        let listener = transport::bind("127.0.0.1:0").await.unwrap();
        self.hint(listener.local_addr().unwrap());
        let store = self.peer_store.clone();
        let trust = self.peer_trust.clone();
        let begins = self.begins.clone();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.map_err(|e| e.to_string())?;
            let mut sink = Sink(begins);
            let connection =
                transport::accept_remote_trusted(tcp, &identity, &store, &trust, "peer", &sink)
                    .await?;
            transport::receive_session(connection, &mut sink, |_| {})
                .await
                .map(|_| ())
        })
    }
    async fn prepare(&self) -> Result<transport::Sender, String> {
        prepare(&self.context, &self.context.config.layout.links[0]).await
    }
}

#[tokio::test]
async fn offline_and_permission_denial_never_arm_capture() {
    let fixture = Fixture::new(false);
    assert!(fixture.prepare().await.is_err());
    assert_eq!(fixture.context.gate.state(), Focus::Local);
    let receiver = fixture.serve(fixture.peer.clone()).await;
    assert!(fixture.prepare().await.is_err());
    assert!(receiver.await.unwrap().unwrap_err().contains("permission"));
    assert_eq!(fixture.begins.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.context.gate.state(), Focus::Local);
}

#[tokio::test]
async fn spoofed_discovery_identity_cannot_replace_pinned_peer() {
    let fixture = Fixture::new(true);
    let impostor = Identity::load_or_create(&fixture._temp.path().join("impostor")).unwrap();
    let receiver = fixture.serve(impostor).await;
    assert!(fixture.prepare().await.is_err());
    assert!(receiver.await.unwrap().is_err());
    assert_eq!(fixture.begins.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.context.gate.state(), Focus::Local);
    assert_eq!(
        TrustStore::load(&fixture.context.trust_dir).unwrap().peers[0].id,
        fixture.peer.id
    );
}

#[tokio::test]
async fn peer_restart_at_new_discovered_address_recovers_without_trust_edit() {
    let fixture = Fixture::new(true);
    for _ in 0..3 {
        let receiver = fixture.serve(fixture.peer.clone()).await;
        let sender = fixture.prepare().await.unwrap();
        assert!(*sender.alive.borrow());
        sender.finish().await.unwrap();
        receiver.await.unwrap().unwrap();
        assert_eq!(fixture.context.gate.state(), Focus::Local);
        // Previous ephemeral endpoint is now closed. A later advertised hint
        // connects to the same certificate on a new listener/port.
        assert!(fixture.prepare().await.is_err());
    }
    assert_eq!(fixture.begins.load(Ordering::SeqCst), 3);
    let store = TrustStore::load(&fixture.context.trust_dir).unwrap();
    assert_eq!(store.peers[0].address.as_deref(), Some("127.0.0.1:1"));
    assert!(!store.peers[0].permissions.control_this_node);
}

#[tokio::test]
async fn dropping_sender_cancels_both_tasks_and_closes_receiver_connection() {
    let fixture = Fixture::new(true);
    let receiver = fixture.serve(fixture.peer.clone()).await;
    let sender = fixture.prepare().await.unwrap();
    drop(sender);
    let result = tokio::time::timeout(Duration::from_secs(2), receiver)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
}

#[tokio::test]
async fn real_tls_repeated_returns_and_active_disconnect_release_focus_and_held_input() {
    use super::super::lifecycle_tests::{entry, sink, Evidence};
    let fixture = Fixture::new(true);
    let listener = transport::bind("127.0.0.1:0").await.unwrap();
    fixture.hint(listener.local_addr().unwrap());
    let identity = fixture.peer.clone();
    let store = fixture.peer_store.clone();
    let trust = fixture.peer_trust.clone();
    let receiver_gate = FocusGate::default();
    let gate = receiver_gate.clone();
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let shared = evidence.clone();
    let receiver = tokio::spawn(async move {
        let mut sink = sink(&gate, &shared);
        for index in 0..11 {
            let (tcp, _) = listener.accept().await.unwrap();
            let connection =
                transport::accept_remote_trusted(tcp, &identity, &store, &trust, "peer", &sink)
                    .await
                    .unwrap();
            let result = transport::receive_session(connection, &mut sink, |_| {}).await;
            if index < 10 {
                result.unwrap();
            } else {
                assert!(result.is_err());
            }
            assert_eq!(gate.state(), Focus::Local);
            assert!(!shared.lock().unwrap().held_key);
            assert!(!shared.lock().unwrap().held_button);
        }
        sink.inner.shutdown().unwrap();
    });
    for index in 0..11 {
        let mut sender = fixture.prepare().await.unwrap();
        sender
            .control
            .send(WireMessage::RemoteEntry(entry()))
            .await
            .unwrap();
        sender
            .queue
            .send(InputEvent::Key {
                code: ukvm_protocol::KeyCode::LinuxEvdev(42),
                pressed: true,
            })
            .await
            .unwrap();
        sender
            .queue
            .send(InputEvent::PointerButton {
                button: 272,
                pressed: true,
            })
            .await
            .unwrap();
        if index < 10 {
            sender
                .queue
                .send(InputEvent::PointerMotion { dx: -1.0, dy: 0.0 })
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), sender.returned.changed())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(*sender.returned.borrow(), Some(entry()));
            sender.finish().await.unwrap();
        } else {
            tokio::time::timeout(Duration::from_secs(2), async {
                while !evidence.lock().unwrap().held_button {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(receiver_gate.state(), Focus::Controlled);
            drop(sender); // no SessionEnd; TLS task ownership closes both halves
        }
    }
    tokio::time::timeout(Duration::from_secs(2), receiver)
        .await
        .unwrap()
        .unwrap();
    let evidence = evidence.lock().unwrap();
    assert_eq!(evidence.grants, 1);
    assert_eq!(evidence.parks, 11);
    assert_eq!(evidence.closes, 1);
}

#[tokio::test]
async fn cancellation_interrupts_reconnect_wait_without_waiting_for_backoff() {
    let (tx, mut stop) = watch::channel(false);
    let cancel = async {
        tokio::task::yield_now().await;
        tx.send(true).unwrap();
    };
    let waiting = pause(Duration::from_secs(16), &mut stop);
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(waiting, cancel);
    })
    .await
    .unwrap();
}

#[test]
fn bounded_discovery_candidates_preserve_configured_fallback_and_ignore_names() {
    let fixture = Fixture::new(true);
    let mut hints = fixture.context.hints.lock().unwrap();
    for index in 2..40 {
        hints.push(DiscoveryResult {
            service_instance: format!("spoof-{index}"),
            display_name: "peer".into(),
            advertised_node_id: Some(fixture.peer.id.clone()),
            pairing_version: 2,
            pairing_available: false,
            input_version: Some((1, 2)),
            control_port: Some(24800),
            addresses: vec![format!("192.0.2.{index}:24800").parse().unwrap()],
        });
    }
    drop(hints);
    let store = TrustStore::load(&fixture.context.trust_dir).unwrap();
    let candidates = addresses(&fixture.context, &store.peers[0]);
    assert_eq!(candidates.len(), 16);
    assert_eq!(candidates.last().unwrap(), "127.0.0.1:1");
    for hint in fixture.context.hints.lock().unwrap().iter_mut() {
        hint.advertised_node_id = Some("f".repeat(64));
    }
    assert_eq!(
        addresses(&fixture.context, &store.peers[0]),
        ["127.0.0.1:1"]
    );
}

#[tokio::test]
async fn denied_runtime_policy_does_not_open_portal_or_injector() {
    let fixture = Fixture::new(false);
    incoming::assert_denied_before_portal(&fixture.context, &fixture.peer.id).await;
}

fn launching_boundary() -> linux_wayland::OuterSegment {
    linux_wayland::OuterSegment {
        zone: 0,
        edge: linux_wayland::Edge::Right,
        start: (0, 0),
        end: (0, 2159),
    }
}

#[test]
fn emergency_restores_local_focus_but_only_inward_movement_allows_next_crossing() {
    let fixture = Fixture::new(true);
    assert!(arm_outgoing(&fixture.context));
    assert!(fixture.context.gate.activate());
    fixture
        .context
        .rearm
        .lock()
        .unwrap()
        .suppress(Some(launching_boundary()));
    fixture.context.gate.outbound_closed(); // completed Release/Close path
    assert_eq!(fixture.context.gate.state(), Focus::Local);
    for point in [
        ukvm_platform::Point { x: -0.5, y: 500.0 },
        ukvm_platform::Point { x: 0.0, y: 800.0 },
        ukvm_platform::Point { x: -31.0, y: 906.0 },
    ] {
        assert!(!fixture.context.rearm.lock().unwrap().observe_local(point));
        assert!(!arm_outgoing(&fixture.context));
        assert!(!fixture.context.gate.activate());
        assert_eq!(fixture.context.gate.state(), Focus::Local);
    }
    assert!(fixture
        .context
        .rearm
        .lock()
        .unwrap()
        .observe_local(ukvm_platform::Point { x: -32.0, y: 906.0 }));
    assert!(arm_outgoing(&fixture.context));
    assert_eq!(fixture.context.gate.state(), Focus::Armed);
    assert!(fixture.context.gate.activate()); // later intentional portal crossing
    assert_eq!(fixture.context.gate.state(), Focus::Controlling);
}

#[test]
fn normal_return_and_disconnect_still_allow_rearming_without_movement_guard() {
    let fixture = Fixture::new(true);
    // Both normal return and disconnect execute the same existing focus Close
    // path; only EmergencyRelease latches the additional movement guard.
    for _ in 0..2 {
        assert!(arm_outgoing(&fixture.context));
        assert!(fixture.context.gate.activate());
        fixture.context.gate.outbound_closed();
        assert_eq!(fixture.context.gate.state(), Focus::Local);
        assert!(fixture.context.rearm.lock().unwrap().allows_arming());
    }
    assert!(arm_outgoing(&fixture.context));
}

#[tokio::test]
async fn emergency_session_end_cleans_held_input_and_discards_queued_events_before_rearming() {
    use super::super::lifecycle_tests::{entry, sink, Evidence};
    let fixture = Fixture::new(true);
    let listener = transport::bind("127.0.0.1:0").await.unwrap();
    fixture.hint(listener.local_addr().unwrap());
    let identity = fixture.peer.clone();
    let store = fixture.peer_store.clone();
    let trust = fixture.peer_trust.clone();
    let remote_gate = FocusGate::default();
    let gate = remote_gate.clone();
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let shared = evidence.clone();
    let receiver = tokio::spawn(async move {
        let mut sink = sink(&gate, &shared);
        let (tcp, _) = listener.accept().await.unwrap();
        let connection =
            transport::accept_remote_trusted(tcp, &identity, &store, &trust, "peer", &sink)
                .await
                .unwrap();
        let counts = transport::receive_session(connection, &mut sink, |_| {})
            .await
            .unwrap();
        // Events queued after emergency SessionEnd never reach the injector.
        assert_eq!(counts.keyboard, 1);
        assert_eq!(counts.buttons, 1);
        assert_eq!(gate.state(), Focus::Local);
        assert!(!shared.lock().unwrap().held_key);
        assert!(!shared.lock().unwrap().held_button);
        sink.inner.shutdown().unwrap();
    });
    let sender = fixture.prepare().await.unwrap();
    assert!(arm_outgoing(&fixture.context));
    assert!(fixture.context.gate.activate());
    sender
        .control
        .send(WireMessage::RemoteEntry(entry()))
        .await
        .unwrap();
    sender
        .queue
        .send(InputEvent::Key {
            code: ukvm_protocol::KeyCode::LinuxEvdev(42),
            pressed: true,
        })
        .await
        .unwrap();
    sender
        .queue
        .send(InputEvent::PointerButton {
            button: 272,
            pressed: true,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !evidence.lock().unwrap().held_button {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture
        .context
        .rearm
        .lock()
        .unwrap()
        .suppress(Some(launching_boundary()));
    sender.control.try_send(WireMessage::SessionEnd).unwrap();
    sender
        .queue
        .try_send(InputEvent::Key {
            code: ukvm_protocol::KeyCode::LinuxEvdev(30),
            pressed: true,
        })
        .unwrap();
    fixture.context.gate.outbound_closed();
    sender.finish().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), receiver)
        .await
        .unwrap()
        .unwrap();
    assert!(!arm_outgoing(&fixture.context));
    assert!(!evidence.lock().unwrap().held_key);
    assert!(!evidence.lock().unwrap().held_button);
}
