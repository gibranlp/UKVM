//! Bounded concurrent readiness; only the focus owner touches the shared grant.
use super::*;

type Injector = RemoteInputSink<WaylandInputInjector>;
struct Authority {
    sink: Arc<Mutex<Option<Injector>>>,
    initialization: tokio::sync::Mutex<()>,
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
                let injector = WaylandInputInjector::open_verbose(self.verbose)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut sink = RemoteInputSink::new(injector);
                sink.begin_session_async().await?;
                self.authorize()?;
                sink.end_session()?;
                *self.authority.sink.lock().unwrap() = Some(sink);
            }
            self.authorize()
        })
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
        self.authority
            .sink
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(Injector::grant_is_healthy)
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
    let store = match TrustStore::load(&context.trust_dir) {
        Ok(store) => store,
        Err(error) => {
            context.log(error);
            return;
        }
    };
    let mut sink = ExclusiveSink {
        inner: IncomingSink {
            session: SharedSession {
                shared: Arc::clone(&authority.sink),
                entered: false,
            },
            authority,
            policy: (context.trust_dir.clone(), String::new()),
            verbose: context.verbose,
            gate: context.gate.clone(),
        },
        gate: context.gate.clone(),
        reserved: false,
    };
    let connection = tokio::select! {
        _ = cancelled(&mut stop) => return,
        result = transport::accept_remote_trusted(tcp, &context.identity, &store, &context.trust_dir, &context.config.node.name, &sink) => match result {
            Ok(connection) => connection,
            Err(error) => { context.log(format!("incoming authentication rejected: {error}")); return; }
        }
    };
    let peer_id = connection.peer.id.clone();
    sink.inner.policy.1 = peer_id.clone();
    context.log(format!("authenticated incoming peer {peer_id}"));
    let shutdown = async {
        loop {
            let allowed = TrustStore::load(&context.trust_dir)
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
            ReceiverNotice::SessionStarted => {
                context.log(format!("peer {peer_id}: receiver portal/EI ready"))
            }
            ReceiverNotice::RemoteEntered(position) => context.log(format!(
                "peer {peer_id}: incoming {:?} entry; controlled focus",
                position.edge
            )),
            ReceiverNotice::RemoteReturned(position) => context.log(format!(
                "peer {peer_id}: ReturnControl {:?}; held input released",
                position.edge
            )),
            ReceiverNotice::SessionEnded(_) => {
                context.log(format!("peer {peer_id}: incoming cleanup complete"))
            }
            ReceiverNotice::Counts(_) => {}
        },
        shutdown,
    )
    .await;
    if let Err(error) = result {
        context.log(format!("peer {peer_id}: incoming ended: {error}"));
    }
}

pub(super) async fn receiver(
    context: &Context,
    listener: tokio::net::TcpListener,
    mut stop: watch::Receiver<bool>,
) {
    let authority = Arc::new(Authority {
        sink: Arc::new(Mutex::new(None)),
        initialization: tokio::sync::Mutex::new(()),
    });
    let mut connections = FuturesUnordered::new();
    // Eight positions per source; a bounded number of readiness relationships
    // may coexist. Excess connections are closed without waiting for consent.
    const MAX_INCOMING: usize = 16;
    loop {
        tokio::select! {
            _ = cancelled(&mut stop) => break,
            _ = connections.next(), if !connections.is_empty() => {},
            accepted = listener.accept() => match accepted {
                Ok((tcp, address)) => {
                    if connections.len() >= MAX_INCOMING { context.log(format!("incoming capacity reached; closing {address}")); }
                    else { connections.push(incoming(context, Arc::clone(&authority), tcp, stop.clone())); }
                }
                Err(error) => { context.log(format!("accept failed: {error}")); pause(Duration::from_secs(1), &mut stop).await; }
            }
        }
    }
    // The stop broadcast wakes all owned sessions; never detach a receiver.
    while connections.next().await.is_some() {}
    let closing = authority.sink.lock().unwrap().take();
    if let Some(mut sink) = closing {
        if let Err(error) = sink.shutdown() {
            context.log(format!("injector shutdown: {error}"));
        }
    }
}

#[cfg(test)]
pub(super) async fn assert_denied_before_portal(context: &Context, peer: &str) {
    let authority = Arc::new(Authority {
        sink: Arc::new(Mutex::new(None)),
        initialization: tokio::sync::Mutex::new(()),
    });
    let mut sink = IncomingSink {
        session: SharedSession {
            shared: Arc::clone(&authority.sink),
            entered: false,
        },
        authority: Arc::clone(&authority),
        policy: (context.trust_dir.clone(), peer.into()),
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
