//! CLI presentation for a remote window; runtime owns the control lease.
use std::{process::ExitCode, sync::Arc, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, watch},
};
use ukvm_network::{
    display,
    identity::{default_state_dir, Identity},
    trust::{cert_path, default_trust_dir, SessionPermission, TrustStore},
};
use ukvm_platform::remote::window::{self, Command, Shared};
use ukvm_protocol::{
    display::{Message, Options, VERSION},
    WireMessage,
};
pub fn command(args: &[String]) -> ExitCode {
    if matches!(args,[flag] if flag=="--help"||flag=="-h") {
        println!("Usage: ukvm remote PEER [--stats] [--baseline] [--fps 1..60] [--quality 45..100] [--bitrate 10..1000]\n--bitrate is a video ceiling in Mbps; --stats prints privacy-safe pipeline metrics every 5s.\n--baseline compares the previous 1080p/30 FPS software JPEG path.\nView a pinned peer's selected monitor; click to control.\nCtrl+Alt+Shift+Escape releases control and leaves viewing open.\nReceiver requires view-screen; control additionally requires control-this-node.\nInteractive use requires ukvm run (or its user service) on both computers.");
        return ExitCode::SUCCESS;
    }
    let result = (|| {
        let (peer, options, stats) = preferences(args)?;
        let trust = default_trust_dir()?;
        let store = TrustStore::load(&trust)?;
        let peer = store.find(&peer).ok_or("unknown pinned peer")?.clone();
        store.authorize(&peer.id, SessionPermission::Connect)?;
        let identity = Identity::load_or_create(&default_state_dir()?)?;
        let shared = Arc::new(Shared::with_options(options));
        let worker_shared = shared.clone();
        let (commands, rx) = mpsc::channel(256);
        let (stop_tx, stop) = watch::channel(false);
        let name = peer.alias.clone();
        let thread = std::thread::Builder::new()
            .name("ukvm-remote-session".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                runtime.block_on(async {
                    let viewing = async {
                        let result =
                            view(&peer, &identity, &trust, &worker_shared, stop.clone()).await;
                        if let Err(error) = &result {
                            eprintln!("remote display: {error}");
                        }
                        let mut state = worker_shared.state.lock().unwrap();
                        state.connected = false;
                        state.controlling = false;
                        state.requesting = false;
                        state.message = result.err().unwrap_or_else(|| "Disconnected".into());
                        // Display failure cancels input while the window remains open.
                        let _ = stop_tx.send(true);
                    };
                    let input = async {
                        source(&peer.id, worker_shared.clone(), rx, stop.clone()).await;
                        let _ = stop_tx.send(true);
                    };
                    let signal = async {
                        let _ = shutdown_signal().await;
                        worker_shared.state.lock().unwrap().closed = true;
                        let _ = stop_tx.send(true);
                    };
                    tokio::pin!(signal);
                    let sessions = async {
                        let diagnostics=async {
                            if !stats { cancelled(&mut stop.clone()).await; return; }
                            let started=std::time::Instant::now();
                            let mut ticks=tokio::time::interval(Duration::from_secs(5));
                            ticks.tick().await;
                            let mut cancelled_stop=stop.clone();
                            loop { tokio::select! {
                                _=cancelled(&mut cancelled_stop)=>break,
                                _=ticks.tick()=>eprintln!("{}",worker_shared.report(started.elapsed())),
                            }}
                            eprintln!("{}",worker_shared.report(started.elapsed()));
                        };
                        tokio::join!(viewing, input, diagnostics);
                    };
                    tokio::pin!(sessions);
                    tokio::select! {_=&mut sessions=>{},_=&mut signal=>{sessions.await;}}
                    Ok::<_, String>(())
                })
            })
            .map_err(|e| e.to_string())?;
        let result = window::run(name, shared, commands);
        // Dropping the GUI sender cancels pending input/consent and closes viewing.
        let joined = thread.join().map_err(|_| "remote worker failed")?;
        result.and(joined)
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => super::fail(&e),
    }
}
fn preferences(args: &[String]) -> Result<(String, Options, bool), String> {
    let mut options = if args.iter().any(|a| a == "--baseline") {
        Options::baseline()
    } else {
        Options::default()
    };
    let mut peer = None;
    let mut stats = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stats"=>stats=true,"--baseline"=>{},
            "--fps"=>options.fps=args.next().ok_or("missing FPS")?.parse().map_err(|_|"invalid FPS")?,
            "--quality"=>options.quality=args.next().ok_or("missing quality")?.parse().map_err(|_|"invalid quality")?,
            "--bitrate"=>options.mbps=args.next().ok_or("missing bitrate")?.parse().map_err(|_|"invalid bitrate")?,
            _ if !arg.starts_with('-') && peer.is_none()=>peer=Some(arg.clone()),
            _=>return Err("usage: ukvm remote PEER [--stats] [--baseline] [--fps N] [--quality N] [--bitrate Mbps]".into()),
        }
    }
    options.validate()?;
    Ok((peer.ok_or("missing pinned peer")?, options, stats))
}
async fn cancelled(stop: &mut watch::Receiver<bool>) {
    if !*stop.borrow() {
        let _ = stop.changed().await;
    }
}
async fn view(
    peer: &ukvm_network::trust::Peer,
    identity: &Identity,
    trust: &std::path::Path,
    shared: &Shared,
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    let clock = std::time::Instant::now();
    let certificate = cert_path(trust, &peer.id);
    let mut addresses = Vec::new();
    if let Some(address) = &peer.address {
        addresses.push(ukvm_network::trust::resolve_address(address)?);
    }
    let discovery = tokio::select! {_=cancelled(&mut stop)=>return Ok(()),result=super::discovery::browse_all(None)=>result};
    if let Ok(found) = discovery {
        for hint in found
            .into_iter()
            .filter(|h| h.advertised_node_id.as_deref() == Some(&peer.id))
        {
            if let Some(port) = hint.control_port {
                for mut address in hint.addresses {
                    address.set_port(port);
                    let address = address.to_string();
                    if !addresses.contains(&address) {
                        addresses.push(address);
                    }
                }
            }
        }
    }
    let mut stream = None;
    for address in addresses.into_iter().take(8) {
        let attempt = tokio::select! {_=cancelled(&mut stop)=>return Ok(()),attempt=display::connect(&address,identity,&certificate,&peer.id)=>attempt};
        if let Ok(tls) = attempt {
            stream = Some(tls);
            break;
        }
    }
    let mut stream = stream.ok_or(
        "no pinned display endpoint; update both computers and check view-screen permission",
    )?;
    display::write(&mut stream, &Message::Hello(VERSION)).await?;
    display::write(&mut stream, &Message::Configure(shared.options)).await?;
    shared.state.lock().unwrap().message = "Waiting for peer's screen-sharing consent".into();
    let ready = tokio::select! {_=cancelled(&mut stop)=>return Ok(()),ready=display::read_until(&mut stream,Duration::from_secs(125))=>ready?};
    let Message::Ready { interactive } = ready else {
        return Err("invalid display capability response".into());
    };
    shared.state.lock().unwrap().interactive = interactive;
    let mut last_sequence = None;
    let mut frames = 0;
    let mut rate = std::time::Instant::now();
    loop {
        let message = {
            // The capture side allows ten seconds for the first usable buffer.
            // Do not tear it down at the normal five-second steady-state limit.
            let frame = display::read_until(
                &mut stream,
                Duration::from_secs(if last_sequence.is_none() { 12 } else { 5 }),
            );
            tokio::pin!(frame);
            let mut policy = tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    _=cancelled(&mut stop)=>return Ok(()),
                    _=policy.tick()=>{TrustStore::load(trust)?.authorize(&peer.id,SessionPermission::Connect)?;},
                    message=&mut frame=>break message?,
                }
            }
        };
        let received = std::time::Instant::now();
        let received_us = clock.elapsed().as_micros() as u64;
        let frame = match message {
            Message::Frame(frame) => frame,
            Message::Ready { interactive } => {
                let mut state = shared.state.lock().unwrap();
                state.interactive = interactive;
                if !interactive {
                    state.controlling = false;
                    state.requesting = false;
                    shared
                        .release
                        .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
                }
                continue;
            }
            _ => return Err("unexpected display message".into()),
        };
        if last_sequence.is_some_and(|n| frame.sequence < n) {
            return Err("display sequence regressed".into());
        }
        if rate.elapsed() >= Duration::from_secs(1) {
            frames = 0;
            rate = std::time::Instant::now();
        }
        frames += 1;
        if frames > 75 {
            return Err("display frame rate exceeds limit".into());
        }
        last_sequence = Some(frame.sequence);
        let sequence = frame.sequence;
        {
            let mut stats = shared.performance.lock().unwrap();
            stats.receive(&frame);
        }
        shared.frames.replace(window::Received {
            received_us,
            frame,
            received,
        });
        shared.state.lock().unwrap().connected = true;
        display::write(
            &mut stream,
            &Message::Ack {
                sequence,
                received_us,
                sent_us: clock.elapsed().as_micros() as u64,
            },
        )
        .await?;
    }
}
async fn source(
    id: &str,
    shared: Arc<Shared>,
    mut commands: mpsc::Receiver<Command>,
    mut stop: watch::Receiver<bool>,
) {
    let mut local: Option<tokio::net::UnixStream> = None;
    let mut release = shared.release.subscribe();
    loop {
        let command = tokio::select! {
            biased;
            _=release.changed()=>{local=None;continue;},
            _=cancelled(&mut stop)=>break,
            command=commands.recv()=>match command {Some(c)=>c,None=>break},
            _=async {if let Some(socket)=local.as_mut(){let _=ukvm_network::read_frame(socket).await;}else{std::future::pending::<()>().await;}}=>{
                local=None;let mut s=shared.state.lock().unwrap();s.controlling=false;s.requesting=false;s.message="Input disconnected; click to retry".into();continue;
            },
        };
        match command {
            Command::Acquire {
                target,
                x,
                y,
                epoch,
            } => {
                if epoch != *release.borrow() {
                    continue;
                }
                if local.is_some() {
                    continue;
                }
                let open = async {
                    let path = ukvm_runtime::ownership::directory()?.join("remote-input.sock");
                    let mut socket = tokio::net::UnixStream::connect(path)
                        .await
                        .map_err(|_| "start the local UKVM runtime/service to control")?;
                    socket
                        .write_all(id.as_bytes())
                        .await
                        .map_err(|_| "window bridge disconnected")?;
                    ukvm_network::write_frame(
                        &mut socket,
                        &WireMessage::WindowEntry { target, x, y },
                    )
                    .await?;
                    let (accept, _) = ukvm_network::read_frame(&mut socket).await?;
                    if accept != WireMessage::RemoteSessionAccept {
                        return Err("remote control rejected".into());
                    }
                    Ok::<_, String>(socket)
                };
                let result = tokio::select! {
                    biased;
                    _=release.changed()=>{continue;},
                    _=cancelled(&mut stop)=>break,
                    command=commands.recv()=>{if matches!(command,None|Some(Command::Close)){break;}continue;},
                    result=tokio::time::timeout(Duration::from_secs(140),open)=>result.map_err(|_|"control request timed out".to_string()).and_then(|r|r),
                };
                let mut state = shared.state.lock().unwrap();
                state.requesting = false;
                match result {
                    Ok(socket) => {
                        local = Some(socket);
                        state.controlling = true;
                        state.message.clear();
                    }
                    Err(e) => {
                        state.controlling = false;
                        state.message = e;
                    }
                }
            }
            Command::Event { event, epoch } => {
                if epoch == *release.borrow() {
                    if let Some(socket) = local.as_mut() {
                        let message = WireMessage::Input(event);
                        let send = ukvm_network::write_frame(socket, &message);
                        let result = tokio::select! {biased;_=release.changed()=>Err("released".into()),result=tokio::time::timeout(Duration::from_millis(100),send)=>result.unwrap_or_else(|_|Err("input write timed out".into()))};
                        if result.is_err() {
                            local = None;
                            shared.state.lock().unwrap().controlling = false;
                        }
                    }
                }
            }
            Command::Close => break,
        }
    }
    drop(local);
    let mut state = shared.state.lock().unwrap();
    state.controlling = false;
    state.requesting = false;
}

async fn shutdown_signal() {
    if let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    } else {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }
    #[test]
    fn performance_preferences_are_explicit_and_bounded() {
        assert_eq!(
            preferences(&args("io --stats")).unwrap(),
            ("io".into(), Options::default(), true)
        );
        assert_eq!(
            preferences(&args("--baseline io")).unwrap().1,
            Options::baseline()
        );
        let (_, options, _) = preferences(&args("io --quality 99 --fps 45 --bitrate 150")).unwrap();
        assert_eq!((options.quality, options.fps, options.mbps), (99, 45, 150));
        for invalid in [
            "io --fps 61",
            "io --fps",
            "io --quality 101",
            "io --bitrate 0",
            "io --unexpected",
            "io other",
            "",
        ] {
            assert!(preferences(&args(invalid)).is_err());
        }
    }
}
