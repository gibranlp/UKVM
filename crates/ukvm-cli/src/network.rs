use std::{
    path::PathBuf,
    process::ExitCode,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use ukvm_network::{
    identity::{default_state_dir, Identity},
    transport::{self, ReceiverNotice},
    EventCounts,
};
use ukvm_protocol::{InputEvent, KeyCode, WireMessage};

struct Options {
    state_dir: PathBuf,
    peer_cert: Option<PathBuf>,
    bind: String,
    name: String,
}

fn options(args: &[String]) -> Result<Options, String> {
    let configured_name = ukvm_config::default_path()
        .ok()
        .and_then(|path| ukvm_config::load(&path).ok())
        .map(|config| config.node.name);
    let mut options = Options {
        state_dir: default_state_dir()?,
        peer_cert: None,
        bind: format!("127.0.0.1:{}", ukvm_network::DEFAULT_PORT),
        name: configured_name
            .or_else(|| std::env::var("HOSTNAME").ok())
            .unwrap_or_else(|| "ukvm-node".into()),
    };
    let mut iter = args.iter();
    while let Some(option) = iter.next() {
        let value = iter
            .next()
            .ok_or_else(|| format!("missing value for {option}"))?;
        match option.as_str() {
            "--state-dir" => options.state_dir = PathBuf::from(value),
            "--peer-cert" => options.peer_cert = Some(PathBuf::from(value)),
            "--bind" => options.bind = value.clone(),
            "--name" => options.name = value.clone(),
            _ => return Err(format!("unknown network option {option}")),
        }
    }
    if options.name.is_empty()
        || options.name.len() > 63
        || options.name.chars().any(char::is_control)
    {
        return Err("node name must be 1-63 printable characters".into());
    }
    Ok(options)
}

fn peer_cert(options: &Options) -> Result<&std::path::Path, String> {
    options.peer_cert.as_deref().ok_or("--peer-cert PATH is required; exchange public node.der certificates out of band before connecting".into())
}

fn address(value: &str) -> String {
    if value.contains(':') {
        value.to_string()
    } else {
        format!("{value}:{}", ukvm_network::DEFAULT_PORT)
    }
}

pub fn identity(args: &[String]) -> ExitCode {
    match options(args).and_then(|options| Identity::load_or_create(&options.state_dir)) {
        Ok(identity) => {
            println!(
                "UniversalKVM node identity\n  ID: {}\n  Public certificate: {}",
                identity.id,
                identity.cert_path.display()
            );
            ExitCode::SUCCESS
        }
        Err(err) => super::fail(&err),
    }
}

pub fn listen(args: &[String]) -> ExitCode {
    let result = options(args).and_then(|options| {
        let cert = peer_cert(&options)?.to_path_buf();
        let identity = Identity::load_or_create(&options.state_dir)?;
        Ok((options, cert, identity))
    });
    let (options, peer_cert, identity) = match result {
        Ok(value) => value,
        Err(err) => return super::fail(&err),
    };
    super::with_runtime(async move {
        let listener = match transport::bind(&options.bind).await {
            Ok(listener) => listener,
            Err(err) => return super::fail(&err),
        };
        println!("UniversalKVM Debug Receiver\n  Node: {}\n  ID: {}\n  Listening: {}\n  Transport: mutual TLS 1.3", options.name, identity.id, listener.local_addr().unwrap());
        loop {
            let accepted = tokio::select! { accepted = listener.accept() => accepted, _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS };
            let (tcp, addr) = match accepted {
                Ok(value) => value,
                Err(err) => {
                    eprintln!("Accept error: {err}");
                    continue;
                }
            };
            match transport::accept(tcp, &identity, &peer_cert, &options.name).await {
                Ok(connection) => {
                    println!(
                        "Peer authenticated: {} ({}) from {addr}",
                        connection.peer.name, connection.peer.id
                    );
                    let mut began = None::<Instant>;
                    let result = transport::receive_debug(connection, |notice| match notice {
                        ReceiverNotice::SessionStarted => {
                            println!("Debug session started. No input will be injected.")
                        }
                        ReceiverNotice::Counts(c) => {
                            if began.is_none() {
                                began = Some(Instant::now());
                            }
                            println!(
                                "Received: pointer={} buttons={} scroll={} keyboard={}",
                                c.pointer, c.buttons, c.scroll, c.keyboard
                            );
                        }
                        ReceiverNotice::SessionEnded(c) => {
                            println!(
                                "Session ended: pointer={} buttons={} scroll={} keyboard={}",
                                c.pointer, c.buttons, c.scroll, c.keyboard
                            );
                            if let Some(at) = began {
                                println!(
                                    "  Pointer rate: {:.1} events/sec",
                                    c.pointer as f64 / at.elapsed().as_secs_f64().max(0.001)
                                );
                            }
                        }
                    })
                    .await;
                    if let Err(err) = result {
                        eprintln!("Peer session ended: {err}");
                    }
                }
                Err(err) => eprintln!("Rejected connection from {addr}: {err}"),
            }
        }
    })
}

pub fn connect(target: &str, args: &[String], synthetic: bool) -> ExitCode {
    let result = options(args).and_then(|options| {
        let cert = peer_cert(&options)?.to_path_buf();
        let identity = Identity::load_or_create(&options.state_dir)?;
        Ok((options, cert, identity))
    });
    let (options, peer_cert, identity) = match result {
        Ok(value) => value,
        Err(err) => return super::fail(&err),
    };
    let target = address(target);
    super::with_runtime(async move {
        let mut connection =
            match transport::connect(&target, &identity, &peer_cert, &options.name).await {
                Ok(conn) => conn,
                Err(err) => return super::fail(&err),
            };
        println!(
            "Connected to {} ({})\n  Platform: {}\n  Transport: mutual TLS 1.3\n  Debug sink: {}",
            connection.peer.name,
            connection.peer.id,
            connection.peer.platform,
            connection.peer.debug_sink
        );
        if !synthetic {
            let began = Instant::now();
            if let Err(err) =
                ukvm_network::write_frame(&mut connection.stream, &WireMessage::Heartbeat(1)).await
            {
                return super::fail(&err);
            }
            match tokio::time::timeout(
                Duration::from_secs(4),
                ukvm_network::read_frame(&mut connection.stream),
            )
            .await
            {
                Ok(Ok((WireMessage::HeartbeatAck(1), _))) => {
                    println!("  RTT: {:.2} ms", began.elapsed().as_secs_f64() * 1000.0)
                }
                _ => return super::fail("peer heartbeat response missing"),
            }
            let _ =
                ukvm_network::write_frame(&mut connection.stream, &WireMessage::SessionEnd).await;
            return ExitCode::SUCCESS;
        }
        let sender = match transport::start_sender(connection).await {
            Ok(sender) => sender,
            Err(err) => return super::fail(&err),
        };
        for event in [
            InputEvent::PointerMotion { dx: 10.0, dy: 0.0 },
            InputEvent::PointerMotion { dx: -10.0, dy: 0.0 },
            InputEvent::Key {
                code: KeyCode::LinuxEvdev(42),
                pressed: true,
            },
            InputEvent::Key {
                code: KeyCode::LinuxEvdev(42),
                pressed: false,
            },
        ] {
            if sender.queue.send(event).await.is_err() {
                return super::fail("sender disconnected");
            }
        }
        match sender.finish().await {
            Ok(stats) => {
                print_stats(&stats.sent, stats.bytes, stats.rtt_micros, 0);
                ExitCode::SUCCESS
            }
            Err(err) => super::fail(&err),
        }
    })
}

fn print_stats(counts: &EventCounts, bytes: u64, rtt: u64, dropped: u64) {
    println!("Sent: pointer={} buttons={} scroll={} keyboard={}\n  Input frame bytes: {bytes}\n  Dropped pointer motion: {dropped}\n  RTT: {}", counts.pointer, counts.buttons, counts.scroll, counts.keyboard, if rtt == 0 { "not measured".into() } else { format!("{:.2} ms", rtt as f64 / 1000.0) });
}

#[cfg(target_os = "linux")]
pub fn capture_send(
    edge: ukvm_platform::linux_wayland::Edge,
    zone: Option<usize>,
    target: &str,
    args: &[String],
    verbose: bool,
) -> ExitCode {
    let result = options(args).and_then(|options| {
        let cert = peer_cert(&options)?.to_path_buf();
        let identity = Identity::load_or_create(&options.state_dir)?;
        Ok((options, cert, identity))
    });
    let (options, peer_cert, identity) = match result {
        Ok(value) => value,
        Err(err) => return super::fail(&err),
    };
    let target = address(target);
    super::with_runtime(async move {
        let connection =
            match transport::connect(&target, &identity, &peer_cert, &options.name).await {
                Ok(conn) => conn,
                Err(err) => return super::fail(&err),
            };
        println!(
            "Authenticated peer {} ({}). Requesting debug session before capture.",
            connection.peer.name, connection.peer.id
        );
        let sender = match transport::start_sender(connection).await {
            Ok(sender) => sender,
            Err(err) => return super::fail(&err),
        };
        println!("Remote DebugSink accepted. Cross the selected {edge:?} edge to send event counts. Press Esc to release.");
        let dropped = AtomicU64::new(0);
        let queue = sender.queue.clone();
        let mut health = sender.alive.clone();
        let mut on_event = |event: InputEvent| match queue.try_send(event) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(InputEvent::PointerMotion {
                ..
            })) => {
                dropped.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                Err("bounded queue filled on key/button/scroll; releasing capture".into())
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err("peer sender closed; releasing capture".into())
            }
        };
        let mut healthy = || *health.borrow_and_update();
        let mut notice = |event| super::print_capture_notice(event, verbose);
        let capture = ukvm_platform::linux_wayland::capture_with_events(
            edge,
            zone,
            Duration::from_secs(60),
            &mut notice,
            &mut on_event,
            &mut healthy,
        )
        .await;
        drop(queue);
        let sent = sender.finish().await;
        match (&capture, &sent) {
            (Ok(summary), Ok(stats)) => {
                println!(
                    "Captured: pointer={} keyboard={}\n  Release: {}",
                    summary.pointer_events,
                    summary.key_events,
                    summary.release_succeeded == Some(true)
                );
                print_stats(
                    &stats.sent,
                    stats.bytes,
                    stats.rtt_micros,
                    dropped.load(Ordering::Relaxed),
                );
                ExitCode::SUCCESS
            }
            (Err(err), _) => super::fail(err),
            (_, Err(err)) => super::fail(err),
        }
    })
}
