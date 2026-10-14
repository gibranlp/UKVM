use super::*;
use std::sync::{atomic::AtomicUsize, Arc};
use ukvm_core::controlled::{InputSink, SinkAction, SinkKind};
use ukvm_network::trust::SessionPermission;
use ukvm_protocol::{EdgePosition, InputEvent};

#[tokio::test]
async fn independent_workers_drain_ready_connections_when_stop_channel_ends() {
    for explicit_stop in [false, true] {
        let mut fixture = Fixture::new(true);
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
        let receiver = fixture.serve(fixture.peer.clone()).await;
        let (sender, stop) = watch::channel(false);
        let right = Coordinate { x: 1, y: 0 };
        let node = fixture
            .context
            .config
            .layout
            .topology()
            .into_iter()
            .find(|node| node.coordinate() == right)
            .unwrap();
        if explicit_stop {
            *fixture.context.active.lock().unwrap() = Some(right);
            fixture
                .context
                .availability(&node, PeerAvailability::Active);
        }
        let observe = async {
            while !ready(&fixture.context, right) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(!ready(&fixture.context, Coordinate { x: -1, y: 0 }));
            assert_eq!(
                exits(&fixture.context, Coordinate::LOCAL),
                [ukvm_protocol::Edge::Right]
            );
            assert_eq!(exits(&fixture.context, right), [ukvm_protocol::Edge::Left]);
            assert_eq!(
                fixture.context.availability.lock().unwrap().get(&right),
                Some(if explicit_stop {
                    &PeerAvailability::Active
                } else {
                    &PeerAvailability::Ready
                })
            );
            // Do not set the separate atomic stop flag. Either the watch stop
            // request or loss of its sender must end workers, even with a live
            // ready connection, rather than spinning through immediate pauses.
            if explicit_stop {
                sender.send(true).unwrap();
                Some(sender)
            } else {
                drop(sender);
                None
            }
        };
        let ((), keep_sender_alive) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(peers(&fixture.context, stop), observe)
        })
        .await
        .unwrap();
        drop(keep_sender_alive);
        assert!(fixture.context.pool.lock().unwrap().is_empty());
        fixture
            .context
            .availability(&node, PeerAvailability::Stopped);
        assert_eq!(
            fixture.context.availability.lock().unwrap().get(&right),
            Some(&PeerAvailability::Stopped)
        );
        receiver.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn outbound_revocation_and_corrupt_trust_remove_adjacency_before_entry() {
    let fixture = Fixture::new(true);
    let receiver = fixture.serve(fixture.peer.clone()).await;
    let node = fixture.context.config.layout.topology()[0].clone();
    let sender = prepare_for(&fixture.context, &link_for(&node), true)
        .await
        .unwrap();
    fixture
        .context
        .pool
        .lock()
        .unwrap()
        .insert(node.coordinate(), sender);
    assert!(ready(&fixture.context, node.coordinate()));
    let mut store = TrustStore::load(&fixture.context.trust_dir).unwrap();
    store
        .set_permission(
            &fixture.context.trust_dir,
            "peer",
            SessionPermission::Connect,
            false,
        )
        .unwrap();
    assert!(!ready(&fixture.context, node.coordinate()));
    assert!(take_ready(&fixture.context, node.coordinate()).is_none());
    assert!(exits(&fixture.context, Coordinate::LOCAL).is_empty());
    assert_eq!(
        exits(&fixture.context, node.coordinate()),
        [ukvm_protocol::Edge::Left]
    );
    store
        .set_permission(
            &fixture.context.trust_dir,
            "peer",
            SessionPermission::Connect,
            true,
        )
        .unwrap();
    assert!(ready(&fixture.context, node.coordinate()));
    std::fs::write(
        fixture.context.trust_dir.join("peers.toml"),
        "invalid trust record",
    )
    .unwrap();
    assert!(!ready(&fixture.context, node.coordinate()));
    assert!(take_ready(&fixture.context, node.coordinate()).is_none());
    assert!(exits(&fixture.context, Coordinate::LOCAL).is_empty());
    assert!(prepare_for(&fixture.context, &link_for(&node), true)
        .await
        .is_err());
    let sender = fixture
        .context
        .pool
        .lock()
        .unwrap()
        .remove(&node.coordinate())
        .unwrap();
    sender.finish().await.unwrap();
    receiver.await.unwrap().unwrap();
}

#[tokio::test]
async fn stopped_discovery_requests_no_advertisement_or_trust_change() {
    let fixture = Fixture::new(true);
    for lost_sender in [false, true] {
        let (sender, stop) = watch::channel(!lost_sender);
        if lost_sender {
            drop(sender);
        }
        tokio::time::timeout(
            Duration::from_secs(1),
            discovery(&fixture.context, 24800, stop),
        )
        .await
        .unwrap();
        assert!(fixture.context.hints.lock().unwrap().is_empty());
    }
    let store = TrustStore::load(&fixture.context.trust_dir).unwrap();
    assert_eq!(store.peers[0].address.as_deref(), Some("127.0.0.1:1"));
    assert!(!store.peers[0].permissions.control_this_node);
}

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
    fn enter_topology(
        &mut self,
        _: EdgePosition,
        _: Vec<ukvm_protocol::Edge>,
    ) -> Result<(), String> {
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
    context: PeerRuntime,
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
        let mut config = Config::default();
        config.node.name = "local".into();
        config.set_position(1, 0, peer.id.clone()).unwrap();
        Self {
            context: PeerRuntime::new(config, local, trust, false),
            _temp: temp,
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
            input_version: Some((ukvm_protocol::PROTOCOL_MAJOR, ukvm_protocol::PROTOCOL_MINOR)),
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
        prepare(
            &self.context,
            &link_for(&self.context.config.layout.topology()[0]),
        )
        .await
    }
}
#[tokio::test]
async fn offline_and_permission_denial_never_arm_capture() {
    let fixture = Fixture::new(false);
    assert!(fixture.prepare().await.is_err());
    assert!(!ready(&fixture.context, Coordinate { x: 1, y: 0 }));
    let receiver = fixture.serve(fixture.peer.clone()).await;
    assert!(fixture.prepare().await.is_err());
    assert!(receiver.await.unwrap().unwrap_err().contains("permission"));
    assert_eq!(fixture.begins.load(Ordering::SeqCst), 0);
    assert!(!ready(&fixture.context, Coordinate { x: 1, y: 0 }));
}

#[tokio::test]
async fn spoofed_discovery_identity_cannot_replace_pinned_peer() {
    let fixture = Fixture::new(true);
    let impostor = Identity::load_or_create(&fixture._temp.path().join("impostor")).unwrap();
    let receiver = fixture.serve(impostor).await;
    assert!(fixture.prepare().await.is_err());
    assert!(receiver.await.unwrap().is_err());
    assert_eq!(fixture.begins.load(Ordering::SeqCst), 0);
    assert!(!ready(&fixture.context, Coordinate { x: 1, y: 0 }));
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
        assert!(!ready(&fixture.context, Coordinate { x: 1, y: 0 }));
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
