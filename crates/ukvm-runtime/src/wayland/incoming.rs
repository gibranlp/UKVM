//! Bounded concurrent readiness; only the focus owner touches the shared grant.
use super::*;

type Injector = RemoteInputSink<WaylandInputInjector>;
struct Authority {
    input_slots: tokio::sync::Semaphore,
    file_slots: tokio::sync::Semaphore,
    display_slots: tokio::sync::Semaphore,
    sink: Arc<Mutex<Option<Injector>>>,
    initialization: tokio::sync::Mutex<()>,
    status: Arc<Mutex<String>>,
}
struct IncomingSink {
    authority: Arc<Authority>,
    policy: (PathBuf, String),
    verbose: bool,
    session: SharedSession<Injector>,
    gate: FocusGate,
}
impl IncomingSink {
    fn authorize(&self) -> Result<(), String> {
        TrustStore::load(&self.policy.0)?.authorize(
            &self.policy.1,
            ukvm_network::trust::SessionPermission::ControlThisNode,
        )?;
        Ok(())
    }
}
impl InputSink for IncomingSink {
    fn kind(&self) -> SinkKind {
        SinkKind::RemoteInput
    }
    fn begin_session(&mut self) -> Result<(), String> {
        Err("portal readiness requires async initialization".into())
    }
    fn begin_session_async(&mut self) -> SinkFuture<'_> {
        Box::pin(async move {
            self.authorize()?;
            let _initializing = self.authority.initialization.lock().await;
            // A failed grant is replaced only after the entered owner has
            // completed cleanup. Its health poll releases focus independently.
            let invalid = self
                .authority
                .sink
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|sink| !sink.grant_is_healthy());
            if invalid {
                tokio::time::timeout(Duration::from_secs(10), async {
                    while matches!(
                        self.gate.state(),
                        Focus::Controlled | Focus::Reserved | Focus::Yielding
                    ) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .map_err(|_| "invalid grant owner cleanup timed out")?;
            }
            {
                let mut shared = self.authority.sink.lock().unwrap();
                if shared.as_ref().is_some_and(|sink| !sink.grant_is_healthy()) {
                    if let Some(mut sink) = shared.take() {
                        let _ = sink.shutdown();
                    }
                }
            }
            let missing = self.authority.sink.lock().unwrap().is_none();
            if missing {
                *self.authority.status.lock().unwrap() =
                    "requesting normal portal consent/EI grant".into();
                let injector = WaylandInputInjector::open_verbose(self.verbose)
                    .await
                    .map_err(|e| {
                        *self.authority.status.lock().unwrap() = format!("unavailable: {e}");
                        e.to_string()
                    })?;
                let mut sink = RemoteInputSink::new(injector);
                if let Err(error) = sink.begin_session_async().await {
                    *self.authority.status.lock().unwrap() = format!("unavailable: {error}");
                    return Err(error);
                }
                self.authorize()?;
                sink.end_session()?;
                *self.authority.sink.lock().unwrap() = Some(sink);
                *self.authority.status.lock().unwrap() =
                    "permissioned portal/EI grant ready (reused between sessions)".into();
            }
            self.authorize()
        })
    }
    fn enter_window(
        &mut self,
        target: ukvm_protocol::display::WindowTarget,
        x: f64,
        y: f64,
    ) -> Result<(), String> {
        self.authorize()?;
        TrustStore::load(&self.policy.0)?.authorize(
            &self.policy.1,
            ukvm_network::trust::SessionPermission::ViewScreen,
        )?;
        self.session.enter_window(target, x, y)
    }
    fn enter_remote(&mut self, entry: EdgePosition) -> Result<(), String> {
        self.authorize()
            .and_then(|()| self.session.enter_remote(entry))
    }
    fn enter_topology(
        &mut self,
        entry: EdgePosition,
        exits: Vec<ukvm_protocol::Edge>,
    ) -> Result<(), String> {
        self.authorize()
            .and_then(|()| self.session.enter_topology(entry, exits))
    }
    fn update_exits(&mut self, exits: Vec<ukvm_protocol::Edge>) -> Result<(), String> {
        self.authorize()?;
        self.session.update_exits(exits)
    }
    fn handle_event(&mut self, event: &InputEvent) -> Result<SinkAction, String> {
        // Entry and the independent 250 ms policy monitor enforce permission;
        // avoid filesystem work on each high-frequency input event.
        self.session.handle_event(event)
    }
    fn is_healthy(&self) -> bool {
        let (exists, healthy) = {
            let sink = self.authority.sink.lock().unwrap();
            (
                sink.is_some(),
                sink.as_ref().is_some_and(Injector::grant_is_healthy),
            )
        };
        if exists && !healthy {
            *self.authority.status.lock().unwrap() =
                "portal/EI grant invalid; fresh normal consent required".into();
        }
        healthy
    }
    fn end_session(&mut self) -> Result<(), String> {
        let result = self.session.end_session();
        if result.is_err() {
            let mut shared = self.authority.sink.lock().unwrap();
            if let Some(mut sink) = shared.take() {
                let _ = sink.shutdown();
            }
        }
        result
    }
}

async fn incoming(
    context: &Context,
    authority: Arc<Authority>,
    tcp: tokio::net::TcpStream,
    mut stop: watch::Receiver<bool>,
) {
    let store = match TrustStore::load(&context.peers.trust_dir) {
        Ok(store) => store,
        Err(error) => {
            context.peers.log(error);
            return;
        }
    };
    let input_authority = Arc::clone(&authority);
    let mut sink = ExclusiveSink {
        inner: IncomingSink {
            session: SharedSession {
                shared: Arc::clone(&authority.sink),
                entered: false,
            },
            authority,
            policy: (context.peers.trust_dir.clone(), String::new()),
            verbose: context.peers.verbose,
            gate: context.gate.clone(),
        },
        gate: context.gate.clone(),
        reserved: false,
    };
    let connection = tokio::select! {
        _ = cancelled(&mut stop) => return,
        result = transport::accept_runtime(tcp, &context.peers.identity, &store, &context.peers.trust_dir, &context.peers.config.node.name) => match result {
            Ok(connection) => connection,
            Err(error) => { eprintln!("[runtime] incoming authentication rejected: {error}"); return; }
        }
    };
    let connection = match connection {
        transport::RuntimeConnection::Display {
            mut stream,
            peer_id,
        } => {
            if let Ok(_slot) = input_authority.display_slots.try_acquire() {
                if let Err(error) =
                    super::remote::display_session(context, &peer_id, &mut stream, stop).await
                {
                    // A failed first capture has no frame to explain the failure
                    // in the viewer. Surface it without requiring verbose mode.
                    eprintln!("[runtime] remote display: {error}");
                }
            } else {
                eprintln!("[runtime] remote display: another screen-sharing session is active");
            }
            return;
        }
        transport::RuntimeConnection::Input(connection) => connection,
        transport::RuntimeConnection::Files {
            mut stream,
            peer_id,
        } => {
            if let Ok(_slot) = input_authority.file_slots.try_acquire() {
                if let Err(error) =
                    super::files::session(context, &peer_id, &mut stream, stop).await
                {
                    context.peers.log(format!("file transfer: {error}"));
                }
            } else {
                let _ = ukvm_network::files::write(
                    &mut stream,
                    &ukvm_protocol::files::Message::Rejected,
                )
                .await;
            }
            return;
        }
        transport::RuntimeConnection::Clipboard { stream, peer_id } => {
            if let Err(error) = super::clipboard::session(context, &peer_id, stream, stop).await {
                context
                    .peers
                    .log(format!("clipboard peer {peer_id}: {error}"));
            }
            return;
        }
    };
    let Ok(_input_slot) = input_authority.input_slots.try_acquire() else {
        context.peers.log("incoming input capacity reached");
        return;
    };
    let peer_id = connection.peer.id.clone();
    sink.inner.policy.1 = peer_id.clone();
    context
        .peers
        .log(format!("authenticated incoming peer {peer_id}"));
    let shutdown = async {
        loop {
            let allowed = TrustStore::load(&context.peers.trust_dir)
                .ok()
                .and_then(|store| store.find_id(&peer_id).cloned())
                .is_some_and(|peer| peer.permissions.connect && peer.permissions.control_this_node);
            if !allowed {
                return;
            }
            tokio::select! { _ = cancelled(&mut stop) => return, _ = tokio::time::sleep(Duration::from_millis(250)) => {} }
        }
    };
    let result = transport::receive_session_until(
        connection,
        &mut sink,
        |notice| match notice {
            ReceiverNotice::SessionStarted => context
                .peers
                .log(format!("peer {peer_id}: receiver portal/EI ready")),
            ReceiverNotice::RemoteEntered(position) => {
                *context.incoming_peer.lock().unwrap() = Some(peer_id.clone());
                context.peers.log(format!(
                    "peer {peer_id}: incoming {:?} entry; controlled focus",
                    position.edge
                ));
            }
            ReceiverNotice::RemoteReturned(position) => context.peers.log(format!(
                "peer {peer_id}: ReturnControl {:?}; held input released",
                position.edge
            )),
            ReceiverNotice::SessionEnded(_) => context
                .peers
                .log(format!("peer {peer_id}: incoming cleanup complete")),
            ReceiverNotice::Counts(_) => {}
        },
        shutdown,
    )
    .await;
    if context.gate.state() != Focus::Controlled {
        let mut active = context.incoming_peer.lock().unwrap();
        if active.as_deref() == Some(&peer_id) {
            *active = None;
        }
    }
    if let Err(error) = result {
        context
            .peers
            .log(format!("peer {peer_id}: incoming ended: {error}"));
    }
}

// Prepare only an explicitly permitted receiver; never enter remote focus here.
async fn prepare_receiver<S: InputSink + Send>(
    context: &Context,
    sink: &mut S,
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    let store = TrustStore::load(&context.peers.trust_dir)?;
    let Some(peer) = store
        .peers
        .iter()
        .find(|peer| peer.permissions.connect && peer.permissions.control_this_node)
    else {
        return Ok(());
    };
    let peer_id = peer.id.clone();
    let revoked = async {
        loop {
            if TrustStore::load(&context.peers.trust_dir)
                .and_then(|store| {
                    store
                        .authorize(
                            &peer_id,
                            ukvm_network::trust::SessionPermission::ControlThisNode,
                        )
                        .map(|_| ())
                })
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    };
    tokio::select! {
        biased;
        _ = cancelled(&mut stop) => Err("receiver preparation stopped".into()),
        _ = revoked => Err("receiver preparation permission revoked".into()),
        result = sink.begin_session_async() => result,
    }
}

async fn prepare_authority(
    context: &Context,
    authority: Arc<Authority>,
    stop: watch::Receiver<bool>,
) {
    let peer = TrustStore::load(&context.peers.trust_dir)
        .ok()
        .and_then(|store| {
            store
                .peers
                .iter()
                .find(|peer| peer.permissions.connect && peer.permissions.control_this_node)
                .map(|peer| peer.id.clone())
        });
    let Some(peer) = peer else {
        return;
    };
    let mut sink = IncomingSink {
        session: SharedSession {
            shared: Arc::clone(&authority.sink),
            entered: false,
        },
        authority,
        policy: (context.peers.trust_dir.clone(), peer),
        verbose: context.peers.verbose,
        gate: context.gate.clone(),
    };
    if let Err(error) = prepare_receiver(context, &mut sink, stop).await {
        *context.receiver_status.lock().unwrap() = format!("unavailable: {error}");
        context.peers.log(format!("receiver preparation: {error}"));
    }
}

pub(super) async fn receiver(
    context: &Context,
    listener: tokio::net::TcpListener,
    mut stop: watch::Receiver<bool>,
) {
    let authority = Arc::new(Authority {
        input_slots: tokio::sync::Semaphore::new(16),
        file_slots: tokio::sync::Semaphore::new(4),
        display_slots: tokio::sync::Semaphore::new(1),
        sink: Arc::new(Mutex::new(None)),
        initialization: tokio::sync::Mutex::new(()),
        status: Arc::clone(&context.receiver_status),
    });
    let mut connections: FuturesUnordered<futures_util::future::BoxFuture<'_, ()>> =
        FuturesUnordered::new();
    use futures_util::FutureExt;
    // Consent runs concurrently with listening, clipboard and file handling.
    connections.push(prepare_authority(context, Arc::clone(&authority), stop.clone()).boxed());
    // Eight positions per source; a bounded number of readiness relationships
    // may coexist. Excess connections are closed without waiting for consent.
    const MAX_INCOMING: usize = 84;
    loop {
        tokio::select! {
            _ = cancelled(&mut stop) => break,
            _ = connections.next(), if !connections.is_empty() => {},
            accepted = listener.accept() => match accepted {
                Ok((tcp, address)) => {
                    if connections.len() >= MAX_INCOMING { context.peers.log(format!("incoming capacity reached; closing {address}")); }
                    else { connections.push(incoming(context, Arc::clone(&authority), tcp, stop.clone()).boxed()); }
                }
                Err(error) => { context.peers.log(format!("accept failed: {error}")); pause(Duration::from_secs(1), &mut stop).await; }
            }
        }
    }
    // The stop broadcast wakes all owned sessions; never detach a receiver.
    while connections.next().await.is_some() {}
    let closing = authority.sink.lock().unwrap().take();
    if let Some(mut sink) = closing {
        if let Err(error) = sink.shutdown() {
            context.peers.log(format!("injector shutdown: {error}"));
        }
    }
}

#[cfg(test)]
pub(super) async fn assert_denied_before_portal(context: &Context, peer: &str) {
    let authority = Arc::new(Authority {
        input_slots: tokio::sync::Semaphore::new(16),
        file_slots: tokio::sync::Semaphore::new(4),
        display_slots: tokio::sync::Semaphore::new(1),
        sink: Arc::new(Mutex::new(None)),
        initialization: tokio::sync::Mutex::new(()),
        status: Arc::clone(&context.receiver_status),
    });
    let mut sink = IncomingSink {
        session: SharedSession {
            shared: Arc::clone(&authority.sink),
            entered: false,
        },
        authority: Arc::clone(&authority),
        policy: (context.peers.trust_dir.clone(), peer.into()),
        verbose: false,
        gate: context.gate.clone(),
    };
    assert!(sink
        .begin_session_async()
        .await
        .unwrap_err()
        .contains("denied"));
    assert!(authority.sink.lock().unwrap().is_none());
    assert!(sink
        .enter_remote(EdgePosition {
            edge: ukvm_protocol::Edge::Left,
            fraction: 0.5
        })
        .is_err());
}

#[cfg(test)]
mod preparation_tests {
    use super::super::tests::Fixture;
    use super::*;
    use ukvm_network::trust::SessionPermission;

    struct ConsentProbe {
        requests: usize,
        pending: bool,
    }
    impl InputSink for ConsentProbe {
        fn kind(&self) -> SinkKind {
            SinkKind::RemoteInput
        }
        fn begin_session(&mut self) -> Result<(), String> {
            unreachable!()
        }
        fn begin_session_async(&mut self) -> SinkFuture<'_> {
            Box::pin(async move {
                self.requests += 1;
                if self.pending {
                    std::future::pending::<()>().await;
                }
                Err("normal portal approval required".into())
            })
        }
        fn handle_event(&mut self, _: &InputEvent) -> Result<SinkAction, String> {
            unreachable!()
        }
        fn end_session(&mut self) -> Result<(), String> {
            Ok(())
        }
    }
    fn allow(f: &Fixture, permission: SessionPermission, value: bool) {
        TrustStore::load(&f.context.peers.trust_dir)
            .unwrap()
            .set_permission(&f.context.peers.trust_dir, "peer", permission, value)
            .unwrap();
    }

    #[tokio::test]
    async fn applied_control_permission_prepares_receiver_without_network_or_focus() {
        let f = Fixture::new(false);
        let (_tx, stop) = watch::channel(false);
        let mut sink = ConsentProbe {
            requests: 0,
            pending: false,
        };
        prepare_receiver(&f.context, &mut sink, stop.clone())
            .await
            .unwrap();
        assert_eq!(sink.requests, 0);
        allow(&f, SessionPermission::ControlThisNode, true);
        assert!(prepare_receiver(&f.context, &mut sink, stop.clone())
            .await
            .unwrap_err()
            .contains("approval required"));
        assert_eq!(sink.requests, 1);
        assert_eq!(f.context.gate.state(), Focus::Local);
        allow(&f, SessionPermission::ControlThisNode, false);
        prepare_receiver(&f.context, &mut sink, stop.clone())
            .await
            .unwrap();
        assert_eq!(sink.requests, 1);
        allow(&f, SessionPermission::ControlThisNode, true);
        allow(&f, SessionPermission::Connect, false);
        prepare_receiver(&f.context, &mut sink, stop).await.unwrap();
        assert_eq!(sink.requests, 1);
    }

    #[tokio::test]
    async fn revocation_cancels_pending_first_consent() {
        let f = Fixture::new(false);
        allow(&f, SessionPermission::ControlThisNode, true);
        let (_tx, stop) = watch::channel(false);
        let mut sink = ConsentProbe {
            requests: 0,
            pending: true,
        };
        let revoke = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            allow(&f, SessionPermission::ControlThisNode, false);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(prepare_receiver(&f.context, &mut sink, stop), revoke)
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().contains("permission revoked"));
        assert_eq!(sink.requests, 1);
        assert_eq!(f.context.gate.state(), Focus::Local);
    }

    #[tokio::test]
    async fn shutdown_cancels_pending_first_consent() {
        let f = Fixture::new(false);
        allow(&f, SessionPermission::ControlThisNode, true);
        let (tx, stop) = watch::channel(false);
        let mut sink = ConsentProbe {
            requests: 0,
            pending: true,
        };
        let shutdown = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(true).unwrap();
        };
        let (result, ()) = tokio::join!(prepare_receiver(&f.context, &mut sink, stop), shutdown);
        assert!(result.unwrap_err().contains("stopped"));
        assert_eq!(sink.requests, 1);
    }
}
