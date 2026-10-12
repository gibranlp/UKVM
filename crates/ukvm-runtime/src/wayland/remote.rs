//! Display failures are isolated; native window input leases existing runtime focus.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use ukvm_network::{
    display,
    trust::{permission_guard, SessionPermission},
};
use ukvm_protocol::display::{Message, VERSION};

pub(super) async fn display_session(
    context: &Context,
    peer: &str,
    stream: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin),
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    let check = || {
        TrustStore::load(&context.trust_dir)?
            .authorize(peer, SessionPermission::ViewScreen)
            .map(|_| ())
    };
    check()?;
    if display::read(stream).await? != Message::Hello(VERSION) {
        return Err("expected display v1 Hello".into());
    }
    let bus = zbus_connection().await?;
    let result=async {
        let capture= {
            let open=ukvm_platform::remote::capture::Capture::open(bus.clone());
            tokio::pin!(open);
            let deadline=tokio::time::sleep(Duration::from_secs(120));tokio::pin!(deadline);
            let mut policy=tokio::time::interval(Duration::from_millis(100));
            // Viewer disconnect cancels a pending picker and closes this private bus.
            let disconnected=stream.read_u8();tokio::pin!(disconnected);
            loop {tokio::select! {
                _=cancelled(&mut stop)=>return Err("display stopped".into()),
                _=&mut deadline=>return Err("ScreenCast consent timed out".into()),
                _=&mut disconnected=>return Err("viewer closed during consent".into()),
                _=policy.tick()=>check()?,
                capture=&mut open=>break capture?,
            }}
        };
        let result=async {
        let mut interactive=TrustStore::load(&context.trust_dir)?.authorize(peer,SessionPermission::ControlThisNode).is_ok();
        display::write(stream,&Message::Ready{interactive}).await?;
        let mut closed=capture.session.receive_closed().await.map_err(|e|e.to_string())?;
            let mut last=None;
            let mut previous_send=std::time::Instant::now()-Duration::from_secs(1);
            let mut ticks=tokio::time::interval(Duration::from_millis(33));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut next_send=std::time::Instant::now();
            let mut first_frame=std::time::Instant::now();
            loop {
                tokio::select! {_=cancelled(&mut stop)=>return Ok(()),_=closed.next()=>return Err("ScreenCast portal session closed".into()),_=ticks.tick()=>{}}
                check()?;
                let allowed=TrustStore::load(&context.trust_dir)?.authorize(peer,SessionPermission::ControlThisNode).is_ok();
                if allowed!=interactive {display::write(stream,&Message::Ready{interactive:allowed}).await?;interactive=allowed;}
                if std::time::Instant::now()<next_send {continue;}
                if let Some(error)=capture.error.lock().unwrap().clone() {return Err(error);}
                let new=capture.frames.take();
                if new.is_some() {last=new;first_frame=std::time::Instant::now();}
                else if previous_send.elapsed()<Duration::from_secs(1) {continue;}
                let Some(frame)=last.as_ref() else {
                    if first_frame.elapsed()>Duration::from_secs(10) {return Err("PipeWire supplied no usable frames".into());}
                    continue;
                };
                let sequence=frame.sequence;
                let send=async {
                    let guard=permission_guard(&context.trust_dir,peer,SessionPermission::ViewScreen)?;
                    display::write(stream,&Message::Frame(frame.clone())).await?;
                    drop(guard);
                    if display::read(stream).await?!=Message::Ack(sequence) {return Err("display acknowledgement out of order".into());}
                    Ok::<_,String>(())
                };
                tokio::pin!(send);
                let mut policy=tokio::time::interval(Duration::from_millis(100));
                loop {tokio::select! {
                    _=cancelled(&mut stop)=>return Ok(()),
                    _=closed.next()=>return Err("ScreenCast portal session closed".into()),
                    _=policy.tick()=>check()?,
                    result=&mut send=>{result?;break;},
                }}
                previous_send=std::time::Instant::now();
                next_send=previous_send+Duration::from_secs_f64(frame.jpeg.len() as f64/(8.*1024.*1024.));
            }
        }.await;
        let _=tokio::time::timeout(Duration::from_secs(2),capture.session.close()).await;
        // Joining capture/encoder actors must not stall the input executor.
        let _=tokio::task::spawn_blocking(move ||drop(capture)).await;
        result
    }.await;
    let _ = bus.close().await;
    result
}
async fn zbus_connection() -> Result<ukvm_platform::remote::capture::PortalConnection, String> {
    ukvm_platform::remote::capture::portal_connection().await
}

struct Lease(FocusGate);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.inbound_closed();
    }
}
async fn window_input(
    context: &Context,
    mut local: tokio::net::UnixStream,
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    let uid = local.peer_cred().map_err(|e| e.to_string())?.uid();
    if uid != rustix::process::getuid().as_raw() {
        return Err("window bridge requires the same user".into());
    }
    let request = async {
        let mut id = [0; 64];
        local
            .read_exact(&mut id)
            .await
            .map_err(|_| "window closed")?;
        let id = std::str::from_utf8(&id)
            .map_err(|_| "invalid window peer ID")?
            .to_owned();
        let store = TrustStore::load(&context.trust_dir)?;
        let peer = store.authorize(&id, SessionPermission::Connect)?.clone();
        let (entry, _) = ukvm_network::read_frame(&mut local).await?;
        let WireMessage::WindowEntry { target, x, y } = entry else {
            return Err("expected window entry".into());
        };
        Ok::<_, String>((peer, target, x, y))
    };
    let (peer, target, x, y) = tokio::select! {
        _=cancelled(&mut stop)=>return Ok(()),
        result=tokio::time::timeout(Duration::from_secs(5),request)=>result.map_err(|_|"window request timed out")??,
    };
    context.gate.reserve_inbound()?;
    let _lease = Lease(context.gate.clone());
    let prepare = async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while context.gate.state() == Focus::Yielding {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "local capture cleanup timed out")?;
        {
            let mut focus = context.gate.0.lock().unwrap();
            if *focus != Focus::Reserved {
                return Err("window focus reservation lost".into());
            }
            *focus = Focus::Window;
        }
        for address in addresses(context, &peer) {
            if let Ok(connection) = transport::connect(
                &address,
                &context.identity,
                &cert_path(&context.trust_dir, &peer.id),
                &context.config.node.name,
            )
            .await
            {
                return transport::start_window_sender(connection).await;
            }
        }
        Err("no authenticated input endpoint available".into())
    };
    let mut sender = tokio::select! {
        _=cancelled(&mut stop)=>return Ok(()),
        _=ukvm_network::read_frame(&mut local)=>return Err("window closed before control acceptance".into()),
        sender=prepare=>sender?,
    };
    let result=async {
        sender.control.send(WireMessage::WindowEntry{target,x,y}).await.map_err(|_|"input channel closed")?;
        tokio::select! {
            _=cancelled(&mut stop)=>return Ok(()),
            _=ukvm_network::read_frame(&mut local)=>return Err("window closed during entry".into()),
            result=tokio::time::timeout(Duration::from_secs(12),async {
                while !*sender.entered.borrow() {sender.entered.changed().await.map_err(|_|"receiver entry rejected")?;}
                Ok::<_,String>(())
            })=>result.map_err(|_|"window entry timed out")??,
        }
        // Report control only after receiver focus and EIS window entry succeed.
        ukvm_network::write_frame(&mut local,&WireMessage::RemoteSessionAccept).await?;
        loop {
            let read=ukvm_network::read_frame(&mut local);tokio::pin!(read);
            let mut policy=tokio::time::interval(Duration::from_millis(100));
            let message=loop {tokio::select! {
                _=cancelled(&mut stop)=>return Ok(()),
                _=sender.alive.changed()=>{if !*sender.alive.borrow(){return Err("peer input disconnected".into());}},
                _=policy.tick()=>{TrustStore::load(&context.trust_dir)?.authorize(&peer.id,SessionPermission::Connect)?;},
                result=&mut read=>break result?.0,
            }};
            match message {
                WireMessage::Input(event)=>{
                    if !*sender.alive.borrow(){return Err("peer input disconnected".into());}
                    // Stateful overflow terminates the entire lease and releases held input.
                    sender.queue.try_send(event).map_err(|_|"window input backpressure")?;
                },
                WireMessage::SessionEnd=>return Ok(()),
                _=>return Err("invalid window source message".into()),
            }
        }
    }.await;
    let _ = sender.control.try_send(WireMessage::SessionEnd);
    let _ = sender.finish().await;
    let _ = local.shutdown().await;
    result
}
pub(super) async fn bridge(context: &Context, mut stop: watch::Receiver<bool>) {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    let bind = (|| {
        let path = ownership::directory()?.join("remote-input.sock");
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            if !metadata.file_type().is_socket()
                || metadata.uid() != rustix::process::getuid().as_raw()
            {
                return Err("unsafe remote window socket path".into());
            }
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        let listener = tokio::net::UnixListener::bind(&path).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((listener, path))
    })();
    let (listener, path) = match bind {
        Ok(v) => v,
        Err(e) => {
            context.log(format!("remote window bridge unavailable: {e}"));
            return;
        }
    };
    let mut sessions = FuturesUnordered::new();
    loop {
        tokio::select! {
            _=cancelled(&mut stop)=>break,
            _=sessions.next(),if !sessions.is_empty()=>{},
            accepted=listener.accept()=>if let Ok((socket,_))=accepted {
                if sessions.len()<4 {let stop=stop.clone();sessions.push(async move {if let Err(e)=window_input(context,socket,stop).await {context.log(format!("remote window input: {e}"));}});}
            },
        }
    }
    while sessions.next().await.is_some() {}
    drop(listener);
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Held(Arc<AtomicBool>);
    impl InputSink for Held {
        fn kind(&self) -> SinkKind {
            SinkKind::RemoteInput
        }
        fn begin_session(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn enter_window(
            &mut self,
            _: ukvm_protocol::display::WindowTarget,
            _: f64,
            _: f64,
        ) -> Result<(), String> {
            Ok(())
        }
        fn handle_event(&mut self, _: &InputEvent) -> Result<SinkAction, String> {
            self.0.store(true, Ordering::Release);
            Ok(SinkAction::Continue)
        }
        fn end_session(&mut self) -> Result<(), String> {
            self.0.store(false, Ordering::Release);
            Ok(())
        }
    }
    #[tokio::test]
    async fn native_source_eof_releases_existing_sender_and_focus() {
        let fixture = super::super::tests::Fixture::new(true);
        let temp = tempfile::tempdir().unwrap();
        let trust = temp.path().join("trust");
        let mut store = TrustStore::default();
        store
            .import(
                &trust,
                "local".into(),
                None,
                fixture.context.identity.tls_cert().as_ref(),
            )
            .unwrap();
        for permission in [
            SessionPermission::ControlThisNode,
            SessionPermission::ViewScreen,
        ] {
            store
                .set_permission(&trust, "local", permission, true)
                .unwrap();
        }
        let listener = transport::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let mut local_store = TrustStore::load(&fixture.context.trust_dir).unwrap();
        local_store
            .set_address(&fixture.context.trust_dir, &fixture.peer.id, &address)
            .unwrap();
        let held = Arc::new(AtomicBool::new(false));
        let receiver_held = held.clone();
        let peer = fixture.peer.clone();
        let receiver = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let transport::RuntimeConnection::Input(connection) =
                transport::accept_runtime(tcp, &peer, &store, &trust, "peer")
                    .await
                    .unwrap()
            else {
                panic!("input expected")
            };
            transport::receive_session(connection, &mut Held(receiver_held), |_| {})
                .await
                .unwrap();
        });
        let (source, mut window) = tokio::net::UnixStream::pair().unwrap();
        let (_stop_tx, stop) = watch::channel(false);
        let client = async {
            window.write_all(fixture.peer.id.as_bytes()).await.unwrap();
            let target = ukvm_protocol::display::WindowTarget {
                origin_known: true,
                screen: ukvm_protocol::display::ScreenRect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                mapping_id: None,
            };
            ukvm_network::write_frame(
                &mut window,
                &WireMessage::WindowEntry {
                    target,
                    x: 100.,
                    y: 100.,
                },
            )
            .await
            .unwrap();
            assert_eq!(
                ukvm_network::read_frame(&mut window).await.unwrap().0,
                WireMessage::RemoteSessionAccept
            );
            assert_eq!(fixture.context.gate.state(), Focus::Window);
            ukvm_network::write_frame(
                &mut window,
                &WireMessage::Input(InputEvent::PointerButton {
                    button: 272,
                    pressed: true,
                }),
            )
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !held.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            drop(window);
        };
        let (result, ()) = tokio::join!(window_input(&fixture.context, source, stop), client);
        assert!(result.is_err());
        receiver.await.unwrap();
        assert!(!held.load(Ordering::Acquire));
        assert_eq!(fixture.context.gate.state(), Focus::Local);
        assert!(fixture.context.gate.arm());
    }
}
