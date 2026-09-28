//! TLS connection setup and the development debug-input session.

use crate::{
    identity::{certificate_id, client_config, load_peer_certificate, server_config, Identity},
    read_frame, write_frame, EventCounts, EVENT_QUEUE,
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{split, AsyncWrite},
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};
use ukvm_protocol::{InputEvent, ProtocolVersion, WireMessage};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(4);

async fn send_with_timeout<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &WireMessage,
) -> Result<usize, String> {
    tokio::time::timeout(HEALTH_TIMEOUT, write_frame(writer, message))
        .await
        .map_err(|_| "peer write timed out".to_string())?
}

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub controller: bool,
    pub debug_sink: bool,
}

pub struct Connection {
    pub stream: TlsStream<TcpStream>,
    pub peer: PeerInfo,
}

fn hello(identity: &Identity, name: &str, controller: bool, debug_sink: bool) -> WireMessage {
    WireMessage::Hello {
        version: ProtocolVersion::CURRENT,
        node_id: identity.id.clone(),
        name: name.to_string(),
        platform: std::env::consts::OS.to_string(),
        controller,
        debug_sink,
    }
}

async fn exchange_hello(
    stream: &mut TlsStream<TcpStream>,
    identity: &Identity,
    name: &str,
    controller: bool,
    debug_sink: bool,
) -> Result<PeerInfo, String> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        write_frame(stream, &hello(identity, name, controller, debug_sink)).await?;
        let (message, _) = read_frame(stream).await?;
        let WireMessage::Hello {
            node_id,
            name,
            platform,
            controller,
            debug_sink,
            ..
        } = message
        else {
            return Err("peer did not send Hello".into());
        };
        let cert = stream
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|chain| chain.first())
            .ok_or("TLS peer certificate missing")?;
        if node_id != certificate_id(cert.as_ref()) {
            return Err("Hello node ID does not match authenticated TLS certificate".into());
        }
        Ok(PeerInfo {
            id: node_id,
            name,
            platform,
            controller,
            debug_sink,
        })
    })
    .await
    .map_err(|_| "protocol handshake timed out".to_string())?
}

pub async fn connect(
    addr: &str,
    identity: &Identity,
    peer_cert: &Path,
    name: &str,
) -> Result<Connection, String> {
    let config = client_config(identity, load_peer_certificate(peer_cert)?)?;
    let tcp = tokio::time::timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| "TCP connection timed out".to_string())?
        .map_err(|e| e.to_string())?;
    tcp.set_nodelay(true).map_err(|e| e.to_string())?;
    let server_name =
        rustls::pki_types::ServerName::try_from("ukvm-node").map_err(|e| e.to_string())?;
    let tls = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        TlsConnector::from(config).connect(server_name, tcp),
    )
    .await
    .map_err(|_| "TLS handshake timed out".to_string())?
    .map_err(|e| format!("TLS peer authentication failed: {e}"))?;
    let mut stream = TlsStream::from(tls);
    let peer = exchange_hello(&mut stream, identity, name, true, false).await?;
    Ok(Connection { stream, peer })
}

pub async fn accept(
    tcp: TcpStream,
    identity: &Identity,
    peer_cert: &Path,
    name: &str,
) -> Result<Connection, String> {
    let config = server_config(identity, load_peer_certificate(peer_cert)?)?;
    tcp.set_nodelay(true).map_err(|e| e.to_string())?;
    let tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, TlsAcceptor::from(config).accept(tcp))
        .await
        .map_err(|_| "TLS handshake timed out".to_string())?
        .map_err(|e| format!("TLS client authentication failed: {e}"))?;
    let mut stream = TlsStream::from(tls);
    let peer = exchange_hello(&mut stream, identity, name, false, true).await?;
    Ok(Connection { stream, peer })
}

#[derive(Clone, Debug)]
pub enum ReceiverNotice {
    SessionStarted,
    Counts(EventCounts),
    SessionEnded(EventCounts),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DebugState {
    Ready,
    Active,
}

impl DebugState {
    fn start(self) -> Result<Self, String> {
        match self {
            Self::Ready => Ok(Self::Active),
            Self::Active => Err("debug session already active".into()),
        }
    }
}

pub async fn receive_debug<F: FnMut(ReceiverNotice)>(
    mut connection: Connection,
    mut notice: F,
) -> Result<EventCounts, String> {
    let mut state = DebugState::Ready;
    let mut counts = EventCounts::default();
    let mut rate_window = Instant::now();
    let mut rate_count = 0u32;
    loop {
        let (message, _) = tokio::time::timeout(HEALTH_TIMEOUT, read_frame(&mut connection.stream))
            .await
            .map_err(|_| "peer heartbeat timed out".to_string())??;
        match message {
            WireMessage::SessionStart if connection.peer.controller => {
                state = state.start()?;
                write_frame(&mut connection.stream, &WireMessage::SessionAccept).await?;
                notice(ReceiverNotice::SessionStarted);
            }
            WireMessage::Input(event) if state == DebugState::Active => {
                if rate_window.elapsed() >= Duration::from_secs(1) {
                    rate_window = Instant::now();
                    rate_count = 0;
                }
                rate_count += 1;
                if rate_count > 20_000 {
                    return Err("input event rate limit exceeded".into());
                }
                counts.add(&event);
                if counts.total() == 1 || counts.total() % 100 == 0 {
                    notice(ReceiverNotice::Counts(counts.clone()));
                }
            }
            WireMessage::Heartbeat(nonce) => {
                write_frame(&mut connection.stream, &WireMessage::HeartbeatAck(nonce)).await?;
            }
            WireMessage::SessionEnd if state == DebugState::Active => {
                notice(ReceiverNotice::SessionEnded(counts.clone()));
                return Ok(counts);
            }
            WireMessage::SessionEnd => return Ok(counts),
            _ => return Err("unexpected message for debug receiver state".into()),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SenderStats {
    pub sent: EventCounts,
    pub bytes: u64,
    pub rtt_micros: u64,
}

pub struct Sender {
    pub queue: mpsc::Sender<InputEvent>,
    pub alive: watch::Receiver<bool>,
    pub rtt_micros: Arc<AtomicU64>,
    writer: JoinHandle<Result<SenderStats, String>>,
    reader: JoinHandle<()>,
}

impl Sender {
    pub async fn finish(self) -> Result<SenderStats, String> {
        drop(self.queue);
        let mut writer = self.writer;
        let result = match tokio::time::timeout(HEALTH_TIMEOUT + HEALTH_TIMEOUT, &mut writer).await
        {
            Ok(joined) => joined.map_err(|e| e.to_string())?,
            Err(_) => {
                writer.abort();
                Err("sender shutdown timed out".into())
            }
        };
        self.reader.abort();
        result
    }
}

pub async fn start_sender(mut connection: Connection) -> Result<Sender, String> {
    require_debug_sink(&connection.peer)?;
    write_frame(&mut connection.stream, &WireMessage::SessionStart).await?;
    let (response, _) = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_frame(&mut connection.stream))
        .await
        .map_err(|_| "debug session acceptance timed out".to_string())??;
    if response != WireMessage::SessionAccept {
        return Err("peer rejected debug session".into());
    }
    let (mut read, mut write) = split(connection.stream);
    let (queue, mut rx) = mpsc::channel::<InputEvent>(EVENT_QUEUE);
    let (alive_tx, alive) = watch::channel(true);
    let rtt_micros = Arc::new(AtomicU64::new(0));
    let pending_ping = Arc::new(std::sync::Mutex::new(None::<(u64, Instant)>));
    let reader = {
        let alive_tx = alive_tx.clone();
        let rtt = Arc::clone(&rtt_micros);
        let pending = Arc::clone(&pending_ping);
        tokio::spawn(async move {
            loop {
                let frame = tokio::time::timeout(HEALTH_TIMEOUT, read_frame(&mut read)).await;
                match frame {
                    Ok(Ok((WireMessage::HeartbeatAck(nonce), _))) => {
                        let mut slot = pending.lock().unwrap();
                        if let Some((_, at)) =
                            slot.take().filter(|(expected, _)| *expected == nonce)
                        {
                            rtt.store(at.elapsed().as_micros() as u64, Ordering::Relaxed);
                        } else {
                            let _ = alive_tx.send(false);
                            break;
                        }
                    }
                    _ => {
                        let _ = alive_tx.send(false);
                        break;
                    }
                }
            }
        })
    };
    let writer = {
        let alive_tx = alive_tx.clone();
        let rtt = Arc::clone(&rtt_micros);
        tokio::spawn(async move {
            let mut stats = SenderStats::default();
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.tick().await;
            let mut nonce = 0u64;
            loop {
                tokio::select! {
                    event = rx.recv() => match event {
                        Some(event) => {
                            match send_with_timeout(&mut write, &WireMessage::Input(event.clone())).await {
                                Ok(bytes) => { stats.sent.add(&event); stats.bytes += bytes as u64; },
                                Err(err) => { let _ = alive_tx.send(false); return Err(err); }
                            }
                        },
                        None => break,
                    },
                    _ = ticker.tick() => {
                        if let Some((_, sent_at)) = *pending_ping.lock().unwrap() {
                            if sent_at.elapsed() >= HEALTH_TIMEOUT {
                                let _ = alive_tx.send(false);
                                return Err("peer heartbeat acknowledgement timed out".into());
                            }
                            continue;
                        }
                        nonce += 1;
                        *pending_ping.lock().unwrap() = Some((nonce, Instant::now()));
                        if let Err(err) = send_with_timeout(&mut write, &WireMessage::Heartbeat(nonce)).await {
                            let _ = alive_tx.send(false); return Err(err);
                        }
                    },
                }
            }
            let _ = send_with_timeout(&mut write, &WireMessage::SessionEnd).await;
            stats.rtt_micros = rtt.load(Ordering::Relaxed);
            Ok(stats)
        })
    };
    Ok(Sender {
        queue,
        alive,
        rtt_micros,
        writer,
        reader,
    })
}

fn require_debug_sink(peer: &PeerInfo) -> Result<(), String> {
    if peer.debug_sink {
        Ok(())
    } else {
        Err("peer did not advertise debug_input_sink".into())
    }
}

pub async fn bind(addr: &str) -> Result<TcpListener, String> {
    TcpListener::bind(addr).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn capability_and_queue_bounds() {
        let peer = PeerInfo {
            id: "a".repeat(64),
            name: "peer".into(),
            platform: "linux".into(),
            controller: true,
            debug_sink: false,
        };
        assert!(require_debug_sink(&peer).is_err());
        assert!(require_debug_sink(&PeerInfo {
            debug_sink: true,
            ..peer
        })
        .is_ok());
        let (tx, _rx) = mpsc::channel(EVENT_QUEUE);
        for _ in 0..EVENT_QUEUE {
            tx.try_send(InputEvent::PointerMotion { dx: 1.0, dy: 0.0 })
                .unwrap();
        }
        assert!(tx
            .try_send(InputEvent::PointerMotion { dx: 1.0, dy: 0.0 })
            .is_err());
    }

    #[tokio::test]
    async fn mutual_tls_and_debug_events_on_localhost() {
        let base = std::env::temp_dir().join(format!("ukvm-network-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let a = Identity::load_or_create(&base.join("a")).unwrap();
        let b = Identity::load_or_create(&base.join("b")).unwrap();
        let listener = bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let a_cert = a.cert_path.clone();
        let b_copy = b.clone();
        let receiver = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let connection = accept(tcp, &b_copy, &a_cert, "node-b").await.unwrap();
            receive_debug(connection, |_| {}).await.unwrap()
        });
        let connection = connect(&addr.to_string(), &a, &b.cert_path, "node-a")
            .await
            .unwrap();
        assert_eq!(connection.peer.id, b.id);
        let sender = start_sender(connection).await.unwrap();
        sender
            .queue
            .send(InputEvent::PointerMotion { dx: 12.0, dy: -4.0 })
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
        let stats = sender.finish().await.unwrap();
        let received = receiver.await.unwrap();
        assert_eq!(stats.sent, received);
        assert_eq!(received.pointer, 1);
        assert_eq!(received.keyboard, 1);
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn untrusted_certificate_is_rejected() {
        let base = std::env::temp_dir().join(format!("ukvm-auth-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let trusted = Identity::load_or_create(&base.join("trusted")).unwrap();
        let server = Identity::load_or_create(&base.join("server")).unwrap();
        let stranger = Identity::load_or_create(&base.join("stranger")).unwrap();
        let listener = bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let expected = trusted.cert_path.clone();
        let server_copy = server.clone();
        let task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            accept(tcp, &server_copy, &expected, "server")
                .await
                .is_err()
        });
        let result = connect(&addr.to_string(), &stranger, &server.cert_path, "stranger").await;
        assert!(result.is_err());
        assert!(task.await.unwrap());
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn sender_notices_peer_disconnect() {
        let base =
            std::env::temp_dir().join(format!("ukvm-disconnect-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let a = Identity::load_or_create(&base.join("a")).unwrap();
        let b = Identity::load_or_create(&base.join("b")).unwrap();
        let listener = bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let a_cert = a.cert_path.clone();
        let b_copy = b.clone();
        let receiver = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut connection = accept(tcp, &b_copy, &a_cert, "node-b").await.unwrap();
            let (message, _) = read_frame(&mut connection.stream).await.unwrap();
            assert_eq!(message, WireMessage::SessionStart);
            write_frame(&mut connection.stream, &WireMessage::SessionAccept)
                .await
                .unwrap();
        });
        let connection = connect(&addr.to_string(), &a, &b.cert_path, "node-a")
            .await
            .unwrap();
        let sender = start_sender(connection).await.unwrap();
        receiver.await.unwrap();
        let mut alive = sender.alive.clone();
        tokio::time::timeout(Duration::from_secs(2), alive.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(!*alive.borrow());
        drop(sender);
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn heartbeat_timeout_marks_sender_unhealthy() {
        let base = std::env::temp_dir().join(format!("ukvm-timeout-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let a = Identity::load_or_create(&base.join("a")).unwrap();
        let b = Identity::load_or_create(&base.join("b")).unwrap();
        let listener = bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let a_cert = a.cert_path.clone();
        let b_copy = b.clone();
        let receiver = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut connection = accept(tcp, &b_copy, &a_cert, "node-b").await.unwrap();
            assert_eq!(
                read_frame(&mut connection.stream).await.unwrap().0,
                WireMessage::SessionStart
            );
            write_frame(&mut connection.stream, &WireMessage::SessionAccept)
                .await
                .unwrap();
            tokio::time::sleep(HEALTH_TIMEOUT + Duration::from_secs(2)).await;
        });
        let connection = connect(&addr.to_string(), &a, &b.cert_path, "node-a")
            .await
            .unwrap();
        let sender = start_sender(connection).await.unwrap();
        let mut alive = sender.alive.clone();
        tokio::time::timeout(HEALTH_TIMEOUT + Duration::from_secs(1), alive.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(!*alive.borrow());
        drop(sender);
        receiver.await.unwrap();
        let _ = fs::remove_dir_all(&base);
    }
}
